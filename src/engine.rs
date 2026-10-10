use crate::{local, model::*, planner, remote::Remote, store::Store};
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::{
    fs,
    panic::AssertUnwindSafe,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

/// Set by SIGTERM. Sync stops between operations so no transfer is cut off by the service.
pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Execution stopped between operations at the user's or system's request.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    Paused,
    Shutdown,
}
impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Stopped::Paused => "Paused before the next operation",
            Stopped::Shutdown => "Stopped for shutdown before the next operation",
        })
    }
}
impl std::error::Error for Stopped {}

/// Called before each operation that changes files: mapping ID, index, total, operation.
pub type OpHook = Box<dyn FnMut(&str, usize, usize, &Operation) + Send>;

pub struct Engine<R: Remote> {
    pub store: Store,
    pub remote: R,
    pub stop: Arc<AtomicBool>,
    pub on_op: Option<OpHook>,
}
impl<R: Remote> Engine<R> {
    pub fn prepare(&mut self, m: &Mapping) -> Result<Plan> {
        let local = local::scan(m)?;
        let set = local::ignores(&m.ignores)?;
        let mut remote = self
            .remote
            .snapshot_selected(&m.remote, m.selection.as_deref())?;
        for path in remote.keys() {
            local::validate_relative(path)?;
        }
        remote.retain(|p, _| !local::ignored(p, &set) && local::selected(m, p, true));
        let mut history = self.store.history(&m.id)?;
        history.retain(|p, _| !local::ignored(p, &set) && local::selected(m, p, true));
        let mut plan = planner::reconcile(&local, &remote, &history);
        plan.operations.retain(|op| {
            local::selected(m, &op.path, false)
                || matches!(
                    op.action,
                    Action::MkdirLocal | Action::MkdirRemote | Action::Record
                )
        });
        plan.deletions = plan
            .operations
            .iter()
            .filter(|op| matches!(op.action, Action::TrashLocal | Action::TrashRemote))
            .count();
        plan.conflicts = plan
            .operations
            .iter()
            .filter(|op| op.action == Action::Conflict)
            .count();
        Ok(plan)
    }
    pub fn execute(&mut self, m: &Mapping, plan: &Plan) -> Result<()> {
        // Only this run stages files for the mapping, so anything left there is from an interrupted run.
        local::verify_root(m)?;
        if let Ok(stale) = fs::read_dir(Path::new(&m.local).join(".peedrive-tmp")) {
            for file in stale.flatten() {
                let _ = fs::remove_file(file.path());
            }
        }
        let total = plan
            .operations
            .iter()
            .filter(|o| !matches!(o.action, Action::Record | Action::Forget))
            .count();
        let Self {
            store,
            remote,
            stop,
            on_op,
        } = self;
        let run = &Run {
            store,
            remote: &*remote,
            m,
        };
        // Transfers run alongside each other. Every other change waits for them, and they for
        // it, so folders exist before their contents and deletions follow everything else.
        std::thread::scope(|scope| {
            let (done, finished) = mpsc::channel();
            let mut running = 0;
            let mut failed = None;
            let result = (|| -> Result<()> {
                let mut index = 0;
                for op in &plan.operations {
                    let transfer = matches!(
                        op.action,
                        Action::Upload | Action::Download | Action::Conflict
                    );
                    let quick = matches!(op.action, Action::Record | Action::Forget);
                    let room = if transfer || quick { PARALLEL - 1 } else { 0 };
                    reap(&finished, &mut running, room, &mut failed);
                    if failed.is_some() {
                        return Ok(());
                    }
                    if SHUTDOWN.load(Ordering::Relaxed) {
                        bail!(Stopped::Shutdown);
                    }
                    if stop.load(Ordering::Relaxed) {
                        bail!(Stopped::Paused);
                    }
                    local::verify_root(m)?;
                    if op.action == Action::Forget {
                        run.store.record(&m.id, &op.path, None)?;
                        continue;
                    }
                    if op.action == Action::Record {
                        let baseline = Baseline {
                            local: op.local.clone().context("Missing local entry")?,
                            remote: op.remote.clone().context("Missing remote entry")?,
                        };
                        run.store.record(&m.id, &op.path, Some(&baseline))?;
                        continue;
                    }
                    if let Some(hook) = on_op {
                        hook(&m.id, index, total, op);
                    }
                    index += 1;
                    if transfer {
                        running += 1;
                        let done = done.clone();
                        scope.spawn(move || {
                            // Always report back, or the loop would wait forever.
                            let result =
                                std::panic::catch_unwind(AssertUnwindSafe(|| run.perform(op)));
                            done.send(result.unwrap_or_else(|_| {
                                Err(anyhow!("Transfer stopped unexpectedly: {}", op.path))
                            }))
                        });
                    } else {
                        run.perform(op)?;
                    }
                }
                Ok(())
            })();
            reap(&finished, &mut running, 0, &mut failed);
            // A failed transfer happened before whatever stopped the loop.
            failed.map_or(result, Err)
        })
    }
}

/// Up to this many uploads and downloads run at once.
const PARALLEL: usize = 3;

/// Wait until at most `room` transfers are running, keeping the first failure.
fn reap(
    finished: &mpsc::Receiver<Result<()>>,
    running: &mut usize,
    room: usize,
    failed: &mut Option<anyhow::Error>,
) {
    while *running > room {
        if let Ok(Err(e)) = finished.recv() {
            failed.get_or_insert(e);
        }
        *running -= 1;
    }
}

/// What one operation needs. Shared by the transfers running at the same time.
struct Run<'a, R> {
    store: &'a Store,
    remote: &'a R,
    m: &'a Mapping,
}
impl<R: Remote> Run<'_, R> {
    /// Journal, apply and log one operation.
    fn perform(&self, op: &Operation) -> Result<()> {
        let journal = self.store.begin(&self.m.id, op)?;
        match self.apply(op) {
            Ok(()) => {
                self.store.finish(journal, None)?;
                let (kind, done) = match op.action {
                    Action::Upload => ("upload", "Uploaded"),
                    Action::Download => ("download", "Downloaded"),
                    Action::MkdirLocal | Action::MkdirRemote => ("folder", "Created folder"),
                    Action::TrashLocal => ("removed_local", "Removed from this computer"),
                    Action::TrashRemote => ("removed_remote", "Moved to Drive Trash"),
                    Action::Conflict => ("conflict", "Kept both versions of"),
                    Action::MoveRemote | Action::MoveLocal => ("moved", "Moved"),
                    Action::Record | Action::Forget => ("info", "Recorded"),
                };
                let message = match &op.from {
                    Some(from) => moved_message(from, &op.path),
                    None => format!("{done} {}", op.path),
                };
                self.store.event(kind, Some(&op.path), &message)
            }
            Err(e) => {
                self.store.finish(journal, Some(&e.to_string()))?;
                Err(e)
            }
        }
    }
    /// Move an item on one side to match a move on the other, then carry its history along.
    fn relocate(&self, op: &Operation, from: &str) -> Result<()> {
        let m = self.m;
        let root = Path::new(&m.local);
        let node = op.remote.as_ref().context("Missing remote item to move")?;
        if op.action == Action::MoveRemote {
            local::assert_unchanged(root, &op.path, &op.local)?;
            let moved = self.remote.relocate(&m.remote, from, &op.path, node)?;
            self.store.move_tree(&m.id, from, &op.path)?;
            // Drive keeps the node and its revision; record whatever the move reported.
            if let Some(mut b) = self.store.baseline(&m.id, &op.path)? {
                b.remote = moved;
                self.store.record(&m.id, &op.path, Some(&b))?;
            }
        } else {
            local::assert_unchanged(root, from, &op.local)?;
            ensure!(
                remote_equal(&self.remote.inspect(&m.remote, &op.path)?, &op.remote),
                "Remote item changed after planning: {}",
                op.path
            );
            let source = local::rooted(root, from)?;
            let destination = local::rooted(root, &op.path)?;
            local::rename_new(&source, &destination)?;
            local::sync_parent(&source)?;
            local::sync_parent(&destination)?;
            self.store.move_tree(&m.id, from, &op.path)?;
        }
        Ok(())
    }
    fn apply(&self, op: &Operation) -> Result<()> {
        let m = self.m;
        if let Some(from) = &op.from {
            return self.relocate(op, from);
        }
        let root = Path::new(&m.local);
        local::assert_unchanged(root, &op.path, &op.local)?;
        let path = local::rooted(root, &op.path)?;
        let current = self.remote.inspect(&m.remote, &op.path)?;
        ensure!(
            remote_equal(&current, &op.remote),
            "Remote item changed after planning: {}",
            op.path
        );
        let mut remote = op.remote.clone();
        match op.action {
            Action::Upload => {
                let l = op.local.as_ref().context("Missing local upload source")?;
                ensure!(
                    l.kind == Kind::File,
                    "Folder type changed; resolve the conflict first"
                );
                // Upload immutable staged bytes. Edits to the source remain pending for the next pass.
                let temp_dir = root.join(".peedrive-tmp");
                local::private_dir(&temp_dir)?;
                let staged = temp_dir.join(uuid::Uuid::new_v4().to_string());
                // Read the time first: if the file changes after this, the hash check below fails.
                let modified = fs::symlink_metadata(&path)?.modified()?;
                fs::copy(&path, &staged)?;
                let result = (|| -> Result<Entry> {
                    // Drive records the staged copy's time, so give it the original's.
                    fs::File::options()
                        .write(true)
                        .open(&staged)?
                        .set_modified(modified)?;
                    let staged_entry = local::entry(&staged)?.context("Staged upload missing")?;
                    ensure!(
                        staged_entry.hash == l.hash,
                        "Local file changed while staging upload"
                    );
                    local::assert_unchanged(root, &op.path, &op.local)?;
                    self.remote.upload(
                        &m.remote,
                        &op.path,
                        &staged,
                        &op.remote,
                        l.hash.as_deref().context("Missing hash")?,
                    )
                })();
                let _ = fs::remove_file(staged);
                remote = Some(result?);
                self.store.record(
                    &m.id,
                    &op.path,
                    Some(&Baseline {
                        local: l.clone(),
                        remote: remote.unwrap(),
                    }),
                )?;
                return Ok(());
            }
            Action::Download | Action::Conflict => {
                let r = op
                    .remote
                    .as_ref()
                    .context("Missing remote download source")?;
                ensure!(
                    r.kind == Kind::File && op.local.as_ref().is_none_or(|e| e.kind == Kind::File),
                    "A file and folder share this name: {}. Rename one side, then retry.",
                    op.path
                );
                let temp_dir = root.join(".peedrive-tmp");
                local::private_dir(&temp_dir)?;
                let staged = temp_dir.join(uuid::Uuid::new_v4().to_string());
                let result = (|| -> Result<Entry> {
                    self.remote.download(&op.path, r, &staged)?;
                    let downloaded = local::entry(&staged)?.context("Download missing")?;
                    if let Some(hash) = &r.hash {
                        ensure!(
                            downloaded.hash.as_ref() == Some(hash),
                            "Downloaded content hash mismatch"
                        );
                    }
                    fs::File::open(&staged)?.sync_all()?;
                    local::verify_root(m)?;
                    local::assert_unchanged(root, &op.path, &op.local)?;
                    ensure!(
                        remote_equal(&self.remote.inspect(&m.remote, &op.path)?, &op.remote),
                        "Remote item changed during download"
                    );
                    if op.action == Action::Conflict
                        && op.local.as_ref().is_some_and(|l| l.hash == downloaded.hash)
                    {
                        // Preserve the observed local identity. A subsequent edit must remain dirty.
                        return op.local.clone().context("Missing identical local file");
                    }
                    if path.exists() {
                        if op.action == Action::Conflict {
                            let copy = conflict_name(&op.path)?;
                            let backup = local::rooted(root, &copy)?;
                            ensure!(!backup.exists(), "Conflict filename collision");
                            local::rename_new(&path, &backup)?;
                            local::sync_parent(&backup)?;
                            self.store.add_conflict(&m.id, &op.path, &copy)?;
                        } else {
                            let backup = local::recovery_path(root, &op.path)?;
                            local::rename_new(&path, &backup)?;
                            local::sync_parent(&backup)?;
                            self.store.add_recovered(
                                &m.id,
                                &op.path,
                                &backup,
                                "Replaced by a newer Drive version",
                            )?;
                        }
                    }
                    // Recheck parents, then rename on the same filesystem.
                    let destination = local::rooted(root, &op.path)?;
                    ensure!(
                        !destination.exists(),
                        "A local file appeared during download"
                    );
                    local::install_download(&staged, &destination, downloaded)
                })();
                let _ = fs::remove_file(&staged);
                let installed = result?;
                // Do not re-read the live destination into the baseline: an editor can
                // change it immediately after the rename. Record only the bytes we installed.
                self.store.record(
                    &m.id,
                    &op.path,
                    Some(&Baseline {
                        local: installed,
                        remote: r.clone(),
                    }),
                )?;
                return Ok(());
            }
            Action::MkdirLocal => {
                fs::create_dir(&path)?;
                local::sync_parent(&path)?;
            }
            Action::MkdirRemote => {
                remote = Some(self.remote.mkdir(&m.remote, &op.path)?);
            }
            Action::TrashLocal => {
                if op.local.as_ref().is_some_and(|e| e.kind == Kind::Folder) {
                    ensure!(
                        fs::read_dir(&path)?.next().is_none(),
                        "Folder gained contents or has ignored files; local deletion held: {}",
                        op.path
                    );
                }
                let backup = local::recovery_path(root, &op.path)?;
                local::rename_new(&path, &backup)?;
                local::sync_parent(&path)?;
                local::sync_parent(&backup)?;
                self.store
                    .add_recovered(&m.id, &op.path, &backup, "Removed from Proton Drive")?;
                self.store.record(&m.id, &op.path, None)?;
                return Ok(());
            }
            Action::TrashRemote => {
                self.remote.trash(
                    &m.remote,
                    &op.path,
                    op.remote
                        .as_ref()
                        .context("Missing remote deletion target")?,
                )?;
                self.store.record(&m.id, &op.path, None)?;
                return Ok(());
            }
            _ => unreachable!(),
        }
        let l = local::entry(&path)?.context("Local item disappeared during sync")?;
        self.store.record(
            &m.id,
            &op.path,
            Some(&Baseline {
                local: l,
                remote: remote.context("Missing remote result")?,
            }),
        )?;
        Ok(())
    }
}
/// "Renamed a.txt to b.txt" within a folder, otherwise "Moved a.txt to Archive/a.txt".
fn moved_message(from: &str, to: &str) -> String {
    let (old, new) = (Path::new(from), Path::new(to));
    match (old.file_name(), new.file_name()) {
        (Some(a), Some(b)) if old.parent() == new.parent() => {
            format!("Renamed {} to {}", a.to_string_lossy(), b.to_string_lossy())
        }
        _ => format!("Moved {from} to {to}"),
    }
}
pub fn remote_equal(a: &Option<Entry>, b: &Option<Entry>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.kind == b.kind && a.uid == b.uid && a.version == b.version,
        _ => false,
    }
}
/// `Report (conflicted copy 2026-10-10 1a2b).pdf`: the extension stays last so the copy
/// still opens in the right application.
pub fn conflict_name(relative: &str) -> Result<String> {
    let original = Path::new(relative);
    let name = original
        .file_name()
        .and_then(|n| n.to_str())
        .context("Invalid name")?;
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64 / 86_400);
    let (y, mo, d) = civil(days);
    let suffix = &uuid::Uuid::new_v4().to_string()[..4];
    let copy = format!("{stem} (conflicted copy {y:04}-{mo:02}-{d:02} {suffix}){ext}");
    Ok(match original.parent().and_then(|p| p.to_str()) {
        Some(parent) if !parent.is_empty() => format!("{parent}/{copy}"),
        _ => copy,
    })
}
/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn requires_deletion_review(plan: &Plan) -> bool {
    let relevant = plan
        .operations
        .iter()
        .filter(|o| o.action != Action::Forget)
        .count();
    plan.deletions >= 10 || (plan.deletions >= 3 && plan.deletions * 4 >= relevant)
}

#[cfg(test)]
mod tests {
    #[test]
    fn conflict_copies_keep_their_extension() {
        let name = super::conflict_name("Projects/Floor plan.v3.pdf").unwrap();
        assert!(
            name.starts_with("Projects/Floor plan.v3 (conflicted copy 2"),
            "{name}"
        );
        assert!(name.ends_with(").pdf"), "{name}");
        assert!(
            super::conflict_name(".bashrc")
                .unwrap()
                .starts_with(".bashrc (conflicted copy")
        );
        assert_eq!(super::civil(0), (1970, 1, 1));
        assert_eq!(super::civil(20_736), (2026, 10, 10));
    }
}
