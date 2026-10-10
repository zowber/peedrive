use crate::model::*;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
};

/// Shared by concurrent transfers; each call holds the connection briefly.
pub struct Store {
    db: Mutex<Connection>,
}
impl Store {
    fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS mappings(id TEXT PRIMARY KEY,json TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS baseline(mapping TEXT NOT NULL,path TEXT NOT NULL,json TEXT NOT NULL,PRIMARY KEY(mapping,path));
          CREATE TABLE IF NOT EXISTS journal(id INTEGER PRIMARY KEY,mapping TEXT NOT NULL,operation TEXT NOT NULL,state TEXT NOT NULL,error TEXT,created TEXT DEFAULT CURRENT_TIMESTAMP);
          CREATE TABLE IF NOT EXISTS activity(id INTEGER PRIMARY KEY,message TEXT NOT NULL,created TEXT DEFAULT CURRENT_TIMESTAMP);")?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 2 {
            db.execute_batch("BEGIN;
              ALTER TABLE activity ADD COLUMN kind TEXT NOT NULL DEFAULT 'info';
              ALTER TABLE activity ADD COLUMN path TEXT;
              CREATE TABLE IF NOT EXISTS recovered(id INTEGER PRIMARY KEY,mapping TEXT NOT NULL,original TEXT NOT NULL,stored TEXT NOT NULL,reason TEXT NOT NULL,size INTEGER NOT NULL DEFAULT 0,created TEXT DEFAULT CURRENT_TIMESTAMP);
              CREATE TABLE IF NOT EXISTS conflicts(id INTEGER PRIMARY KEY,mapping TEXT NOT NULL,path TEXT NOT NULL,copy TEXT NOT NULL,created TEXT DEFAULT CURRENT_TIMESTAMP);
              PRAGMA user_version=2;
              COMMIT;")?;
        }
        Ok(Self { db: Mutex::new(db) })
    }
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db()
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }
    pub fn save_setting(&self, key: &str, value: &str) -> Result<()> {
        self.db().execute("INSERT INTO settings VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value])?;
        Ok(())
    }
    pub fn settings(&self) -> Result<Settings> {
        let mut s = Settings::default();
        if let Some(v) = self.setting("notifications")? {
            s.notifications = v == "true";
        }
        if let Some(v) = self.setting("pause_on_metered")? {
            s.pause_on_metered = v == "true";
        }
        s.recovery_days = self.setting("recovery_days")?.and_then(|v| v.parse().ok());
        s.bandwidth_limit = self
            .setting("bandwidth_limit")?
            .and_then(|v| v.parse().ok());
        Ok(s)
    }
    pub fn mappings(&self) -> Result<Vec<Mapping>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT json FROM mappings ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn save_mapping(&self, m: &Mapping) -> Result<()> {
        self.db().execute(
            "INSERT INTO mappings VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET json=excluded.json",
            params![m.id, serde_json::to_string(m)?],
        )?;
        Ok(())
    }
    pub fn remove_mapping(&self, id: &str) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM mappings WHERE id=?1", [id])?;
        tx.execute("DELETE FROM baseline WHERE mapping=?1", [id])?;
        tx.execute("DELETE FROM conflicts WHERE mapping=?1", [id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn history(&self, id: &str) -> Result<History> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT path,json FROM baseline WHERE mapping=?1")?;
        let rows = stmt.query_map([id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.map(|r| {
            let (p, j) = r?;
            Ok((p, serde_json::from_str(&j)?))
        })
        .collect()
    }
    pub fn file_count(&self, id: &str) -> Result<usize> {
        Ok(self.db().query_row(
            "SELECT count(*) FROM baseline WHERE mapping=?1 AND json_extract(json,'$.local.kind')='file'",
            [id],
            |r| r.get::<_, i64>(0),
        )? as usize)
    }
    pub fn record(&self, id: &str, path: &str, b: Option<&Baseline>) -> Result<()> {
        if let Some(b) = b {
            self.db().execute("INSERT INTO baseline VALUES(?1,?2,?3) ON CONFLICT(mapping,path) DO UPDATE SET json=excluded.json",params![id,path,serde_json::to_string(b)?])?;
        } else {
            self.db().execute(
                "DELETE FROM baseline WHERE mapping=?1 AND path=?2",
                params![id, path],
            )?;
        }
        Ok(())
    }
    pub fn baseline(&self, id: &str, path: &str) -> Result<Option<Baseline>> {
        let json: Option<String> = self
            .db()
            .query_row(
                "SELECT json FROM baseline WHERE mapping=?1 AND path=?2",
                params![id, path],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
    }
    /// Re-key the history of a moved item and everything below it, atomically.
    pub fn move_tree(&self, id: &str, from: &str, to: &str) -> Result<()> {
        let within = "(path=?2 OR substr(path,1,length(?2)+1)=?2||'/')";
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute(
            &format!("DELETE FROM baseline WHERE mapping=?1 AND {within}"),
            params![id, to],
        )?;
        tx.execute(
            &format!("UPDATE baseline SET path=?3||substr(path,length(?2)+1) WHERE mapping=?1 AND {within}"),
            params![id, from, to],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Forget a path and everything below it, so the next plan treats survivors as new.
    pub fn forget_tree(&self, id: &str, path: &str) -> Result<()> {
        self.db().execute(
            "DELETE FROM baseline WHERE mapping=?1 AND (path=?2 OR substr(path,1,length(?2)+1)=?2||'/')",
            params![id, path],
        )?;
        Ok(())
    }
    pub fn begin(&self, id: &str, op: &Operation) -> Result<i64> {
        // One guard for both, so a concurrent insert cannot change the row id.
        let db = self.db();
        db.execute(
            "INSERT INTO journal(mapping,operation,state) VALUES(?1,?2,'running')",
            params![id, serde_json::to_string(op)?],
        )?;
        Ok(db.last_insert_rowid())
    }
    pub fn finish(&self, id: i64, error: Option<&str>) -> Result<()> {
        self.db().execute(
            "UPDATE journal SET state=?2,error=?3 WHERE id=?1",
            params![id, if error.is_some() { "failed" } else { "done" }, error],
        )?;
        Ok(())
    }
    pub fn interrupted(&self, id: &str) -> Result<bool> {
        Ok(self.db().query_row(
            "SELECT EXISTS(SELECT 1 FROM journal WHERE mapping=?1 AND state='running')",
            [id],
            |r| r.get(0),
        )?)
    }
    pub fn acknowledge_recovery(&self, id: &str) -> Result<()> {
        self.db().execute(
            "UPDATE journal SET state='reviewed' WHERE mapping=?1 AND state='running'",
            [id],
        )?;
        Ok(())
    }
    pub fn activity(&self, message: &str) -> Result<()> {
        self.event("info", None, message)
    }
    pub fn event(&self, kind: &str, path: Option<&str>, message: &str) -> Result<()> {
        self.db().execute(
            "INSERT INTO activity(message,kind,path) VALUES(?1,?2,?3)",
            params![message, kind, path],
        )?;
        Ok(())
    }
    pub fn recent(&self) -> Result<Vec<Event>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT id,created,kind,path,message FROM activity ORDER BY id DESC LIMIT 200",
        )?;
        Ok(stmt
            .query_map([], |r| {
                Ok(Event {
                    id: r.get(0)?,
                    time: r.get(1)?,
                    kind: r.get(2)?,
                    path: r.get(3)?,
                    message: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn add_recovered(
        &self,
        mapping: &str,
        original: &str,
        stored: &Path,
        reason: &str,
    ) -> Result<()> {
        let size: u64 = walkdir::WalkDir::new(stored)
            .into_iter()
            .flatten()
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum();
        self.db().execute(
            "INSERT INTO recovered(mapping,original,stored,reason,size) VALUES(?1,?2,?3,?4,?5)",
            params![
                mapping,
                original,
                stored.to_string_lossy(),
                reason,
                size as i64
            ],
        )?;
        Ok(())
    }
    pub fn recovered(&self) -> Result<Vec<Recovered>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT id,mapping,original,stored,reason,created,size FROM recovered ORDER BY id DESC",
        )?;
        let rows: Vec<Recovered> = stmt
            .query_map([], |r| {
                Ok(Recovered {
                    id: r.get(0)?,
                    mapping: r.get(1)?,
                    original: r.get(2)?,
                    stored: r.get(3)?,
                    reason: r.get(4)?,
                    time: r.get(5)?,
                    size: r.get::<_, i64>(6)? as u64,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn remove_recovered(&self, id: i64) -> Result<()> {
        self.db()
            .execute("DELETE FROM recovered WHERE id=?1", [id])?;
        Ok(())
    }
    /// Recovered entries older than `days`.
    pub fn expired_recovered(&self, days: u32) -> Result<Vec<Recovered>> {
        let cutoff: String = self.db().query_row(
            "SELECT datetime('now', ?1)",
            [format!("-{days} days")],
            |r| r.get(0),
        )?;
        Ok(self
            .recovered()?
            .into_iter()
            .filter(|r| r.time < cutoff)
            .collect())
    }
    pub fn add_conflict(&self, mapping: &str, path: &str, copy: &str) -> Result<()> {
        self.db().execute(
            "INSERT INTO conflicts(mapping,path,copy) VALUES(?1,?2,?3)",
            params![mapping, path, copy],
        )?;
        Ok(())
    }
    pub fn conflicts(&self) -> Result<Vec<Conflict>> {
        let db = self.db();
        let mut stmt =
            db.prepare("SELECT id,mapping,path,copy,created FROM conflicts ORDER BY id DESC")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(Conflict {
                    id: r.get(0)?,
                    mapping: r.get(1)?,
                    path: r.get(2)?,
                    copy: r.get(3)?,
                    time: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn remove_conflict(&self, id: i64) -> Result<()> {
        self.db()
            .execute("DELETE FROM conflicts WHERE id=?1", [id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha1_databases_migrate_and_keep_activity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite");
        let old = Connection::open(&path).unwrap();
        old.execute_batch("CREATE TABLE activity(id INTEGER PRIMARY KEY,message TEXT NOT NULL,created TEXT DEFAULT CURRENT_TIMESTAMP);
            INSERT INTO activity(message) VALUES('Connected to Proton Drive'); PRAGMA user_version=1;").unwrap();
        drop(old);
        let store = Store::open(&path).unwrap();
        store
            .event("upload", Some("a.txt"), "Uploaded a.txt")
            .unwrap();
        let events = store.recent().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            (events[0].kind.as_str(), events[0].path.as_deref()),
            ("upload", Some("a.txt"))
        );
        assert_eq!(events[1].kind, "info");
        drop(store);
        Store::open(&path).unwrap();
    }
}
