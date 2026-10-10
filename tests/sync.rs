use anyhow::{Result, bail, ensure};
use peedrive::{
    engine::{Engine, requires_deletion_review},
    local,
    model::*,
    planner::reconcile,
    remote::Remote,
    store::Store,
};
use sha1::{Digest, Sha1};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use tempfile::TempDir;

/// Tests reach the state directly with `s()`; the engine goes through the lock.
#[derive(Default)]
struct FakeRemote(Mutex<Fake>);
impl FakeRemote {
    fn s(&mut self) -> &mut Fake {
        self.0.get_mut().unwrap()
    }
}
#[derive(Default)]
struct Fake {
    tree: Tree,
    content: BTreeMap<String, Vec<u8>>,
    seq: u64,
    fail_upload: bool,
    fail_download: bool,
    change_during_download: Option<PathBuf>,
    /// Uploads take this long, outside the lock, so they can overlap.
    upload_time: Option<std::time::Duration>,
    uploading: usize,
    most_uploading: usize,
    fail_path: Option<String>,
    /// Modification time of each uploaded source, as the SDK would read it.
    uploaded_modified: BTreeMap<String, std::time::SystemTime>,
}
impl Fake {
    /// Current content of the file at `path`.
    fn bytes(&self, path: &str) -> &[u8] {
        &self.content[&self.tree[path].version]
    }
    fn put(&mut self, path: &str, bytes: &[u8]) -> Entry {
        self.seq += 1;
        let hash = format!("{:x}", Sha1::digest(bytes));
        let entry = Entry {
            kind: Kind::File,
            uid: self
                .tree
                .get(path)
                .map(|e| e.uid.clone())
                .unwrap_or_else(|| format!("node{}", self.seq)),
            version: format!("rev{}", self.seq),
            size: bytes.len() as u64,
            hash: Some(hash),
        };
        self.content.insert(entry.version.clone(), bytes.to_vec());
        self.tree.insert(path.into(), entry.clone());
        entry
    }
    fn folder(&mut self, path: &str) -> Entry {
        self.seq += 1;
        let entry = Entry {
            kind: Kind::Folder,
            uid: format!("folder{}", self.seq),
            version: "directory".into(),
            size: 0,
            hash: None,
        };
        self.tree.insert(path.into(), entry.clone());
        entry
    }
    /// Move a subtree, keeping node identities, as Drive does.
    fn shift(&mut self, from: &str, to: &str) {
        let moved: Vec<String> = self
            .tree
            .keys()
            .filter(|p| *p == from || p.starts_with(&format!("{from}/")))
            .cloned()
            .collect();
        for p in moved {
            let e = self.tree.remove(&p).unwrap();
            self.tree.insert(format!("{to}{}", &p[from.len()..]), e);
        }
    }
}
impl Remote for FakeRemote {
    fn snapshot(&self, _: &str) -> Result<Tree> {
        Ok(self.0.lock().unwrap().tree.clone())
    }
    fn inspect(&self, _: &str, p: &str) -> Result<Option<Entry>> {
        Ok(self.0.lock().unwrap().tree.get(p).cloned())
    }
    fn upload(
        &self,
        _: &str,
        p: &str,
        source: &Path,
        expected: &Option<Entry>,
        hash: &str,
    ) -> Result<Entry> {
        let upload_time = self.0.lock().unwrap().upload_time;
        if let Some(time) = upload_time {
            {
                let mut s = self.0.lock().unwrap();
                s.uploading += 1;
                s.most_uploading = s.most_uploading.max(s.uploading);
            }
            std::thread::sleep(time);
            self.0.lock().unwrap().uploading -= 1;
        }
        let mut s = self.0.lock().unwrap();
        if s.fail_upload || s.fail_path.as_deref() == Some(p) {
            bail!("Simulated network failure");
        }
        ensure!(
            s.tree.get(p) == expected.as_ref(),
            "Concurrent remote write"
        );
        let bytes = fs::read(source)?;
        ensure!(
            format!("{:x}", Sha1::digest(&bytes)) == hash,
            "Wrong upload hash"
        );
        s.uploaded_modified
            .insert(p.into(), fs::metadata(source)?.modified()?);
        Ok(s.put(p, &bytes))
    }
    fn download(&self, _: &str, e: &Entry, destination: &Path) -> Result<()> {
        let mut s = self.0.lock().unwrap();
        if s.fail_download {
            fs::write(destination, b"partial")?;
            bail!("Network disconnected");
        }
        fs::write(destination, &s.content[&e.version])?;
        if let Some(p) = s.change_during_download.take() {
            fs::write(p, b"concurrent local edit")?;
        }
        Ok(())
    }
    fn mkdir(&self, _: &str, p: &str) -> Result<Entry> {
        Ok(self.0.lock().unwrap().folder(p))
    }
    fn trash(&self, _: &str, p: &str, expected: &Entry) -> Result<()> {
        let mut s = self.0.lock().unwrap();
        ensure!(s.tree.get(p) == Some(expected), "Remote changed");
        ensure!(
            !s.tree.keys().any(|q| q.starts_with(&format!("{p}/"))),
            "Folder not empty"
        );
        s.tree.remove(p);
        Ok(())
    }
    fn relocate(&self, _: &str, from: &str, to: &str, expected: &Entry) -> Result<Entry> {
        let mut s = self.0.lock().unwrap();
        ensure!(s.tree.get(from) == Some(expected), "Remote changed");
        ensure!(!s.tree.contains_key(to), "Destination taken");
        s.shift(from, to);
        Ok(expected.clone())
    }
}
struct Fixture {
    dir: TempDir,
    m: Mapping,
    engine: Engine<FakeRemote>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("local");
        fs::create_dir(&root).unwrap();
        let (root_device, root_inode) = local::root_identity(&root).unwrap();
        let m = Mapping {
            id: "test".into(),
            local: root.to_str().unwrap().into(),
            remote: "remote".into(),
            remote_label: "Test".into(),
            root_device,
            root_inode,
            paused: false,
            approved: true,
            ignores: vec![],
            selection: None,
        };
        let store = Store::open(&dir.path().join("state.sqlite")).unwrap();
        store.save_mapping(&m).unwrap();
        Self {
            dir,
            m,
            engine: Engine {
                store,
                remote: FakeRemote::default(),
                stop: Arc::new(AtomicBool::new(false)),
                on_op: None,
            },
        }
    }
    fn file(&self, p: &str, bytes: &[u8]) {
        let path = Path::new(&self.m.local).join(p);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn read(&self, p: &str) -> Vec<u8> {
        fs::read(Path::new(&self.m.local).join(p)).unwrap()
    }
    fn sync(&mut self) {
        let p = self.engine.prepare(&self.m).unwrap();
        self.engine.execute(&self.m, &p).unwrap();
    }
    /// Nothing left to do but record agreement.
    fn settled(&mut self) -> bool {
        let plan = self.engine.prepare(&self.m).unwrap();
        plan.operations.iter().all(|o| o.action == Action::Record)
    }
    fn baseline(&mut self) {
        self.file("note.txt", b"original");
        self.sync();
    }
}

#[test]
fn initial_upload_and_download_converge() {
    let mut f = Fixture::new();
    f.file("a.txt", b"a");
    f.engine.remote.s().put("b.txt", b"b");
    f.sync();
    assert_eq!(f.read("b.txt"), b"b");
    assert_eq!(f.engine.store.history("test").unwrap().len(), 2);
    assert!(
        f.engine
            .prepare(&f.m)
            .unwrap()
            .operations
            .iter()
            .all(|o| o.action == Action::Record)
    );
}
#[test]
fn nested_directories_created_before_files() {
    let mut f = Fixture::new();
    f.file("one/two/a.txt", b"data");
    f.sync();
    assert!(f.engine.remote.s().tree.contains_key("one/two/a.txt"));
}
#[test]
fn remote_nested_download() {
    let mut f = Fixture::new();
    f.engine.remote.s().folder("one");
    f.engine.remote.s().folder("one/two");
    f.engine.remote.s().put("one/two/a", b"data");
    f.sync();
    assert_eq!(f.read("one/two/a"), b"data");
}
#[test]
fn local_edit_uploads_new_revision() {
    let mut f = Fixture::new();
    f.baseline();
    let old = f.engine.remote.s().tree["note.txt"].clone();
    f.file("note.txt", b"new");
    f.sync();
    assert_eq!(f.engine.remote.s().tree["note.txt"].uid, old.uid);
    assert_ne!(f.engine.remote.s().tree["note.txt"].version, old.version);
}
#[test]
fn remote_edit_downloads_and_preserves_previous_copy() {
    let mut f = Fixture::new();
    f.baseline();
    f.engine.remote.s().put("note.txt", b"new remote");
    f.sync();
    assert_eq!(f.read("note.txt"), b"new remote");
    let recover = Path::new(&f.m.local).join(".peedrive-recovery");
    assert_eq!(fs::read_dir(recover).unwrap().count(), 1);
}
#[test]
fn simultaneous_edits_keep_both_and_sync_conflict_copy() {
    let mut f = Fixture::new();
    f.baseline();
    f.file("note.txt", b"local edit");
    f.engine.remote.s().put("note.txt", b"remote edit");
    f.sync();
    assert_eq!(f.read("note.txt"), b"remote edit");
    let local = local::scan(&f.m).unwrap();
    let conflict = local.keys().find(|p| p.contains("conflict")).unwrap();
    assert_eq!(f.read(conflict), b"local edit");
    f.sync();
    assert_eq!(f.engine.remote.s().tree.len(), 2);
}
#[test]
fn deletion_of_local_copy_trashes_remote() {
    let mut f = Fixture::new();
    f.baseline();
    fs::remove_file(Path::new(&f.m.local).join("note.txt")).unwrap();
    f.sync();
    assert!(f.engine.remote.s().tree.is_empty());
}
#[test]
fn remote_deletion_preserves_local_in_recovery() {
    let mut f = Fixture::new();
    f.baseline();
    f.engine.remote.s().tree.clear();
    f.sync();
    assert!(!Path::new(&f.m.local).join("note.txt").exists());
    assert!(Path::new(&f.m.local).join(".peedrive-recovery").exists());
}
#[test]
fn local_delete_remote_edit_preserves_edit() {
    let mut f = Fixture::new();
    f.baseline();
    fs::remove_file(Path::new(&f.m.local).join("note.txt")).unwrap();
    f.engine.remote.s().put("note.txt", b"edit");
    f.sync();
    assert_eq!(f.read("note.txt"), b"edit");
}
#[test]
fn remote_delete_local_edit_preserves_edit() {
    let mut f = Fixture::new();
    f.baseline();
    f.engine.remote.s().tree.clear();
    f.file("note.txt", b"edit");
    f.sync();
    assert_eq!(f.engine.remote.s().bytes("note.txt"), b"edit");
}
#[test]
fn deleted_directory_with_remote_edit_restores_entire_subtree() {
    let mut f = Fixture::new();
    f.file("folder/a", b"a");
    f.file("folder/b", b"b");
    f.sync();
    fs::remove_dir_all(Path::new(&f.m.local).join("folder")).unwrap();
    f.engine.remote.s().put("folder/a", b"remote changed");
    f.sync();
    assert_eq!(f.read("folder/a"), b"remote changed");
    assert_eq!(f.read("folder/b"), b"b");
}
#[test]
fn remote_directory_delete_with_local_edit_preserves_subtree() {
    let mut f = Fixture::new();
    f.file("folder/a", b"a");
    f.file("folder/b", b"b");
    f.sync();
    f.engine.remote.s().tree.clear();
    f.file("folder/a", b"local changed");
    f.sync();
    assert!(f.engine.remote.s().tree.contains_key("folder/b"));
}
#[test]
fn missing_root_does_not_plan_deletions() {
    let mut f = Fixture::new();
    f.baseline();
    fs::rename(&f.m.local, f.dir.path().join("disconnected")).unwrap();
    assert!(f.engine.prepare(&f.m).is_err());
    assert_eq!(f.engine.remote.s().tree.len(), 1);
}
#[test]
fn replaced_root_does_not_plan_deletions() {
    let mut f = Fixture::new();
    f.baseline();
    fs::rename(&f.m.local, f.dir.path().join("original-root")).unwrap();
    fs::create_dir(&f.m.local).unwrap();
    assert!(f.engine.prepare(&f.m).is_err());
}
#[test]
fn interrupted_download_leaves_original_untouched() {
    let mut f = Fixture::new();
    f.baseline();
    f.engine.remote.s().put("note.txt", b"new");
    f.engine.remote.s().fail_download = true;
    let p = f.engine.prepare(&f.m).unwrap();
    assert!(f.engine.execute(&f.m, &p).is_err());
    assert_eq!(f.read("note.txt"), b"original");
}
#[test]
fn local_edit_during_download_is_not_overwritten() {
    let mut f = Fixture::new();
    f.baseline();
    f.engine.remote.s().put("note.txt", b"new");
    f.engine.remote.s().change_during_download = Some(Path::new(&f.m.local).join("note.txt"));
    let p = f.engine.prepare(&f.m).unwrap();
    assert!(f.engine.execute(&f.m, &p).is_err());
    assert_eq!(f.read("note.txt"), b"concurrent local edit");
}
#[test]
fn stale_plan_rejects_local_mutation() {
    let mut f = Fixture::new();
    f.file("a", b"planned");
    let p = f.engine.prepare(&f.m).unwrap();
    f.file("a", b"later");
    assert!(f.engine.execute(&f.m, &p).is_err());
    assert!(f.engine.remote.s().tree.is_empty());
}
#[test]
fn stale_plan_rejects_remote_mutation() {
    let mut f = Fixture::new();
    f.baseline();
    f.file("note.txt", b"local");
    let p = f.engine.prepare(&f.m).unwrap();
    f.engine.remote.s().put("note.txt", b"remote");
    assert!(f.engine.execute(&f.m, &p).is_err());
}
#[test]
fn failed_upload_does_not_advance_baseline() {
    let mut f = Fixture::new();
    f.baseline();
    let old = f.engine.store.history("test").unwrap()["note.txt"]
        .remote
        .clone();
    f.file("note.txt", b"new");
    f.engine.remote.s().fail_upload = true;
    let p = f.engine.prepare(&f.m).unwrap();
    assert!(f.engine.execute(&f.m, &p).is_err());
    assert_eq!(
        f.engine.store.history("test").unwrap()["note.txt"].remote,
        old
    );
}
#[test]
fn crash_journal_survives_database_restart() {
    let mut f = Fixture::new();
    f.file("a", b"new");
    let p = f.engine.prepare(&f.m).unwrap();
    f.engine.store.begin("test", &p.operations[0]).unwrap();
    let reopened = Store::open(&f.dir.path().join("state.sqlite")).unwrap();
    assert!(reopened.interrupted("test").unwrap());
}
#[test]
fn crash_after_upload_reconciles_without_duplicate() {
    let mut f = Fixture::new();
    f.file("a", b"new");
    let p = f.engine.prepare(&f.m).unwrap();
    f.engine.store.begin("test", &p.operations[0]).unwrap();
    f.engine.remote.s().put("a", b"new");
    let plan = f.engine.prepare(&f.m).unwrap();
    assert_eq!(plan.operations[0].action, Action::Record);
    f.engine.execute(&f.m, &plan).unwrap();
    assert_eq!(f.engine.remote.s().tree.len(), 1);
}
#[test]
fn ignores_never_become_remote_deletions() {
    let mut f = Fixture::new();
    f.baseline();
    f.m.ignores = vec!["note.txt".into()];
    assert!(f.engine.prepare(&f.m).unwrap().operations.is_empty());
    assert_eq!(f.engine.remote.s().tree.len(), 1);
}
#[test]
fn symlink_never_uploads_outside_file() {
    let mut f = Fixture::new();
    let secret = f.dir.path().join("outside");
    fs::write(&secret, b"private").unwrap();
    std::os::unix::fs::symlink(secret, Path::new(&f.m.local).join("link")).unwrap();
    assert!(f.engine.prepare(&f.m).is_err());
}
#[test]
fn unsafe_remote_path_is_rejected() {
    let mut f = Fixture::new();
    f.engine.remote.s().put("../outside", b"bad");
    assert!(f.engine.prepare(&f.m).is_err());
}
#[test]
fn symlink_parent_cannot_receive_download() {
    let mut f = Fixture::new();
    f.engine.remote.s().folder("folder");
    f.engine.remote.s().put("folder/a", b"bad");
    let plan = f.engine.prepare(&f.m).unwrap();
    std::os::unix::fs::symlink(f.dir.path(), Path::new(&f.m.local).join("folder")).unwrap();
    assert!(f.engine.execute(&f.m, &plan).is_err());
    assert!(!f.dir.path().join("a").exists());
}
#[test]
fn directory_deletions_are_deepest_first() {
    let mut f = Fixture::new();
    f.file("folder/sub/a", b"a");
    f.sync();
    fs::remove_dir_all(Path::new(&f.m.local).join("folder")).unwrap();
    let plan = f.engine.prepare(&f.m).unwrap();
    assert_eq!(plan.operations[0].path, "folder/sub/a");
    f.engine.execute(&f.m, &plan).unwrap();
    assert!(f.engine.remote.s().tree.is_empty());
}
#[test]
fn deletion_batch_requires_review() {
    let mut f = Fixture::new();
    for i in 0..12 {
        f.file(&format!("f{i}"), b"a");
    }
    f.sync();
    for i in 0..12 {
        fs::remove_file(Path::new(&f.m.local).join(format!("f{i}"))).unwrap();
    }
    assert!(requires_deletion_review(&f.engine.prepare(&f.m).unwrap()));
}
#[test]
fn identical_unverified_remote_file_does_not_create_conflict_copy() {
    let mut f = Fixture::new();
    f.file("a", b"same");
    let mut e = f.engine.remote.s().put("a", b"same");
    e.hash = None;
    f.engine.remote.s().tree.insert("a".into(), e);
    f.sync();
    assert_eq!(local::scan(&f.m).unwrap().len(), 1);
}
#[test]
fn file_folder_conflict_stops_without_mutation() {
    let mut f = Fixture::new();
    f.file("a", b"local");
    f.engine.remote.s().folder("a");
    let plan = f.engine.prepare(&f.m).unwrap();
    assert!(f.engine.execute(&f.m, &plan).is_err());
    assert_eq!(f.read("a"), b"local");
}
#[test]
fn empty_plan_is_stable() {
    assert!(
        reconcile(&Tree::new(), &Tree::new(), &History::new())
            .operations
            .is_empty()
    );
}

#[test]
fn atomic_install_does_not_replace_a_new_destination() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let destination = dir.path().join("destination");
    fs::write(&source, b"downloaded").unwrap();
    fs::write(&destination, b"new local edit").unwrap();
    assert!(local::rename_new(&source, &destination).is_err());
    assert_eq!(fs::read(destination).unwrap(), b"new local edit");
    assert_eq!(fs::read(source).unwrap(), b"downloaded");
}

#[test]
fn edit_between_download_install_and_baseline_commit_remains_pending() {
    let mut f = Fixture::new();
    let remote = f.engine.remote.s().put("note.txt", b"downloaded revision");
    let staging = f.dir.path().join("staged");
    fs::write(&staging, b"downloaded revision").unwrap();
    let verified = local::entry(&staging).unwrap().unwrap();
    let destination = Path::new(&f.m.local).join("note.txt");
    let installed = local::install_download(&staging, &destination, verified).unwrap();
    // An editor writes after installation but before the state transaction.
    fs::write(&destination, b"new editor contents").unwrap();
    f.engine
        .store
        .record(
            &f.m.id,
            "note.txt",
            Some(&Baseline {
                local: installed,
                remote,
            }),
        )
        .unwrap();
    let plan = f.engine.prepare(&f.m).unwrap();
    assert_eq!(plan.operations[0].action, Action::Upload);
    f.engine.execute(&f.m, &plan).unwrap();
    assert_eq!(
        f.engine.remote.s().bytes("note.txt"),
        b"new editor contents"
    );
}

#[test]
fn selective_sync_downloads_nested_file_and_leaves_siblings_alone() {
    let mut f = Fixture::new();
    f.m.selection = Some(vec!["docs/chosen.txt".into()]);
    f.engine.remote.s().folder("docs");
    f.engine.remote.s().put("docs/chosen.txt", b"chosen");
    f.engine.remote.s().put("docs/cloud-only.txt", b"cloud");
    f.file("local-only.txt", b"private");
    f.sync();
    assert_eq!(f.read("docs/chosen.txt"), b"chosen");
    assert!(!Path::new(&f.m.local).join("docs/cloud-only.txt").exists());
    assert!(!f.engine.remote.s().tree.contains_key("local-only.txt"));
    f.file("docs/chosen.txt", b"edited");
    f.sync();
    assert_eq!(
        f.engine.remote.s().tree["docs/chosen.txt"].hash,
        local::entry(&Path::new(&f.m.local).join("docs/chosen.txt"))
            .unwrap()
            .unwrap()
            .hash
    );
}

#[test]
fn deselecting_preserves_files_and_history_and_reselecting_reconciles_edits() {
    let mut f = Fixture::new();
    f.baseline();
    f.m.selection = Some(vec![]);
    f.file("note.txt", b"offline edit");
    assert!(f.engine.prepare(&f.m).unwrap().operations.is_empty());
    f.sync();
    assert!(
        f.engine
            .store
            .history(&f.m.id)
            .unwrap()
            .contains_key("note.txt")
    );
    assert_eq!(f.read("note.txt"), b"offline edit");
    f.m.selection = Some(vec!["note.txt".into()]);
    let plan = f.engine.prepare(&f.m).unwrap();
    assert_eq!(plan.operations[0].action, Action::Upload);
}

#[test]
fn selected_folder_includes_new_children_but_not_similarly_named_siblings() {
    let mut f = Fixture::new();
    f.m.selection = Some(vec!["docs".into()]);
    f.file("docs/new.txt", b"new");
    f.file("docs-other/secret.txt", b"private");
    f.sync();
    assert!(f.engine.remote.s().tree.contains_key("docs/new.txt"));
    assert!(!f.engine.remote.s().tree.contains_key("docs-other"));
}

#[test]
fn partial_selection_never_deletes_ancestor_folder_with_unselected_contents() {
    let mut f = Fixture::new();
    f.m.selection = Some(vec!["docs/a.txt".into()]);
    f.file("docs/a.txt", b"a");
    f.file("docs/keep.txt", b"keep");
    f.sync();
    f.engine.remote.s().tree.remove("docs/a.txt");
    f.engine.remote.s().tree.remove("docs");
    f.sync();
    assert_eq!(f.read("docs/keep.txt"), b"keep");
    assert!(!Path::new(&f.m.local).join("docs/a.txt").exists());
}

#[test]
fn default_sync_folder_resolves_email_without_accepting_traversal() {
    use peedrive::service::expand_sync_folder;
    assert_eq!(
        expand_sync_folder(
            "/tmp/Peedrive/{account email}/Proton Drive",
            Some("me@example.com")
        )
        .unwrap(),
        Path::new("/tmp/Peedrive/me@example.com/Proton Drive")
    );
    assert!(expand_sync_folder("/tmp/{account email}", None).is_err());
    assert!(expand_sync_folder("/tmp/{account email}", Some("../outside")).is_err());
    assert!(expand_sync_folder("/tmp/../outside", None).is_err());
}

fn actions(plan: &Plan) -> Vec<(Action, &str)> {
    plan.operations
        .iter()
        .filter(|o| o.action != Action::Record)
        .map(|o| (o.action.clone(), o.path.as_str()))
        .collect()
}

#[test]
fn local_rename_moves_the_drive_file_instead_of_reuploading() {
    let mut f = Fixture::new();
    f.baseline();
    let node = f.engine.remote.s().tree["note.txt"].clone();
    let root = PathBuf::from(&f.m.local);
    fs::rename(root.join("note.txt"), root.join("renamed.txt")).unwrap();
    let plan = f.engine.prepare(&f.m).unwrap();
    assert_eq!(actions(&plan), [(Action::MoveRemote, "renamed.txt")]);
    assert_eq!(plan.deletions, 0);
    f.engine.execute(&f.m, &plan).unwrap();
    assert_eq!(
        f.engine.remote.s().tree["renamed.txt"],
        node,
        "same node and revision"
    );
    assert!(!f.engine.remote.s().tree.contains_key("note.txt"));
    assert!(f.settled());
    let events = f.engine.store.recent().unwrap();
    assert_eq!(events[0].message, "Renamed note.txt to renamed.txt");
}

#[test]
fn local_folder_move_is_one_operation_and_carries_edits_inside() {
    let mut f = Fixture::new();
    f.file("Projects/a.txt", b"a");
    f.file("Projects/deep/b.txt", b"b");
    f.sync();
    let root = PathBuf::from(&f.m.local);
    fs::create_dir(root.join("Archive")).unwrap();
    fs::rename(root.join("Projects"), root.join("Archive/Old projects")).unwrap();
    f.file("Archive/Old projects/a.txt", b"edited after the move");
    let plan = f.engine.prepare(&f.m).unwrap();
    assert_eq!(
        actions(&plan),
        [
            (Action::MkdirRemote, "Archive"),
            (Action::MoveRemote, "Archive/Old projects"),
            (Action::Upload, "Archive/Old projects/a.txt"),
        ]
    );
    f.engine.execute(&f.m, &plan).unwrap();
    let fake = f.engine.remote.s();
    assert!(fake.tree.keys().all(|p| !p.starts_with("Projects")));
    assert_eq!(
        fake.bytes("Archive/Old projects/a.txt"),
        b"edited after the move"
    );
    assert!(fake.tree.contains_key("Archive/Old projects/deep/b.txt"));
    assert!(f.settled());
}

#[test]
fn drive_rename_renames_the_local_file_without_downloading() {
    let mut f = Fixture::new();
    f.baseline();
    let root = PathBuf::from(&f.m.local);
    let inode = local::entry(&root.join("note.txt")).unwrap().unwrap().uid;
    f.engine.remote.s().fail_download = true;
    f.engine.remote.s().shift("note.txt", "Renamed note.txt");
    let plan = f.engine.prepare(&f.m).unwrap();
    assert_eq!(actions(&plan), [(Action::MoveLocal, "Renamed note.txt")]);
    f.engine.execute(&f.m, &plan).unwrap();
    assert!(!root.join("note.txt").exists());
    assert_eq!(
        local::entry(&root.join("Renamed note.txt"))
            .unwrap()
            .unwrap()
            .uid,
        inode
    );
    assert!(!root.join(".peedrive-recovery").exists());
    assert!(f.settled());
}

#[test]
fn drive_folder_move_with_an_edit_inside_moves_then_downloads() {
    let mut f = Fixture::new();
    f.file("docs/a.txt", b"a");
    f.sync();
    f.engine.remote.s().folder("Archive");
    f.engine.remote.s().shift("docs", "Archive/docs");
    f.engine
        .remote
        .s()
        .put("Archive/docs/a.txt", b"edited in Drive");
    f.sync();
    assert_eq!(f.read("Archive/docs/a.txt"), b"edited in Drive");
    assert!(!Path::new(&f.m.local).join("docs").exists());
}

#[test]
fn copy_and_delete_with_unique_content_is_a_move_but_duplicates_are_not() {
    let mut f = Fixture::new();
    f.file("one.txt", b"unique");
    f.file("a.txt", b"same");
    f.file("b.txt", b"same");
    f.sync();
    let root = PathBuf::from(&f.m.local);
    for (from, to) in [
        ("one.txt", "One.txt"),
        ("a.txt", "c.txt"),
        ("b.txt", "d.txt"),
    ] {
        fs::copy(root.join(from), root.join(to)).unwrap();
        fs::remove_file(root.join(from)).unwrap();
    }
    let plan = f.engine.prepare(&f.m).unwrap();
    assert_eq!(
        actions(&plan),
        [
            (Action::MoveRemote, "One.txt"),
            (Action::Upload, "c.txt"),
            (Action::Upload, "d.txt"),
            (Action::TrashRemote, "a.txt"),
            (Action::TrashRemote, "b.txt"),
        ]
    );
}

#[test]
fn moved_and_edited_file_with_a_new_identity_is_not_a_move() {
    let mut f = Fixture::new();
    f.baseline();
    let root = PathBuf::from(&f.m.local);
    fs::remove_file(root.join("note.txt")).unwrap();
    f.file("elsewhere.txt", b"different content");
    let plan = f.engine.prepare(&f.m).unwrap();
    assert_eq!(
        actions(&plan),
        [
            (Action::Upload, "elsewhere.txt"),
            (Action::TrashRemote, "note.txt")
        ]
    );
}

#[test]
fn moves_on_both_sides_never_lose_either_copy() {
    let mut f = Fixture::new();
    f.baseline();
    let root = PathBuf::from(&f.m.local);
    fs::rename(root.join("note.txt"), root.join("local name.txt")).unwrap();
    f.engine.remote.s().shift("note.txt", "drive name.txt");
    f.sync();
    assert_eq!(f.read("drive name.txt"), b"original");
    assert!(f.engine.remote.s().tree.contains_key("local name.txt"));
}

#[test]
fn crash_after_drive_move_converges_without_transfers() {
    let mut f = Fixture::new();
    f.baseline();
    let root = PathBuf::from(&f.m.local);
    fs::rename(root.join("note.txt"), root.join("moved.txt")).unwrap();
    // The move reached Drive but the baseline was never re-keyed.
    f.engine.remote.s().shift("note.txt", "moved.txt");
    let plan = f.engine.prepare(&f.m).unwrap();
    assert!(
        plan.operations
            .iter()
            .all(|o| matches!(o.action, Action::Record | Action::Forget)),
        "{:?}",
        actions(&plan)
    );
}

#[test]
fn uploads_run_three_at_a_time_and_each_records_its_own_baseline() {
    let mut f = Fixture::new();
    for i in 0..7 {
        f.file(&format!("docs/{i}.txt"), format!("file {i}").as_bytes());
    }
    f.engine.remote.s().upload_time = Some(std::time::Duration::from_millis(50));
    f.sync();
    assert_eq!(f.engine.remote.s().most_uploading, 3);
    assert_eq!(f.engine.store.history(&f.m.id).unwrap().len(), 8);
    assert!(f.settled());
}

#[test]
fn one_failed_transfer_keeps_the_others_and_retries_only_itself() {
    let mut f = Fixture::new();
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        f.file(name, name.as_bytes());
    }
    f.engine.remote.s().upload_time = Some(std::time::Duration::from_millis(20));
    f.engine.remote.s().fail_path = Some("b.txt".into());
    let plan = f.engine.prepare(&f.m).unwrap();
    assert!(f.engine.execute(&f.m, &plan).is_err());
    let history = f.engine.store.history(&f.m.id).unwrap();
    assert!(!history.contains_key("b.txt"));
    assert!(history.contains_key("a.txt") && history.contains_key("c.txt"));
    f.engine.remote.s().fail_path = None;
    let plan = f.engine.prepare(&f.m).unwrap();
    let uploads: Vec<_> = actions(&plan).into_iter().map(|(_, p)| p).collect();
    assert!(
        uploads.contains(&"b.txt") && !uploads.contains(&"a.txt"),
        "{uploads:?}"
    );
}

#[test]
fn pause_lets_running_transfers_finish_and_starts_no_more() {
    let mut f = Fixture::new();
    for i in 0..9 {
        f.file(&format!("{i}.txt"), format!("file {i}").as_bytes());
    }
    f.engine.remote.s().upload_time = Some(std::time::Duration::from_millis(30));
    let stop = f.engine.stop.clone();
    f.engine.on_op = Some(Box::new(move |_, index, _, _| {
        if index == 4 {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }));
    let plan = f.engine.prepare(&f.m).unwrap();
    let error = f.engine.execute(&f.m, &plan).unwrap_err();
    assert!(error.to_string().contains("Paused"), "{error}");
    assert_eq!(
        f.engine.remote.s().tree.len(),
        5,
        "started transfers finish"
    );
    assert_eq!(f.engine.store.history(&f.m.id).unwrap().len(), 5);
}

#[test]
fn stop_syncing_keeps_the_rest_of_every_selected_ancestor() {
    use peedrive::files::{deselect, select};
    let tree = ["a", "a/b", "a/b/c.txt", "a/b/d.txt", "a/e.txt", "f.txt"];
    let mut children = |dir: &str| -> Vec<String> {
        tree.iter()
            .filter(|p| Path::new(p).parent() == Some(Path::new(dir)))
            .map(|p| p.to_string())
            .collect()
    };
    assert_eq!(
        deselect(None, "a/b/c.txt", &mut children),
        ["a/b/d.txt", "a/e.txt", "f.txt"]
    );
    assert_eq!(
        deselect(Some(vec!["a".into(), "x".into()]), "a/b", &mut children),
        ["a/e.txt", "x"]
    );
    assert_eq!(
        deselect(
            Some(vec!["a/b/c.txt".into(), "f.txt".into()]),
            "a",
            &mut children
        ),
        ["f.txt"]
    );
    assert_eq!(select(None, "a"), None, "everything already syncs");
    assert_eq!(
        select(Some(vec!["a/b/c.txt".into(), "f.txt".into()]), "a"),
        Some(vec!["f.txt".into(), "a".into()])
    );
    assert_eq!(
        select(Some(vec!["a".into()]), "a/e.txt"),
        Some(vec!["a".into()])
    );
}

#[test]
fn file_states_follow_baseline_plan_and_selection() {
    use peedrive::files::file_state;
    let mut f = Fixture::new();
    f.m.selection = Some(vec!["docs".into()]);
    f.m.ignores = vec!["*.tmp".into()];
    f.file("docs/done.txt", b"done");
    f.sync();
    f.file("docs/new.txt", b"new");
    f.file("docs/edit.tmp", b"scratch");
    f.file("elsewhere.txt", b"not selected");
    let root = PathBuf::from(&f.m.local);
    let plan = f.engine.prepare(&f.m).unwrap();
    let mut status = Status {
        mappings: vec![MappingStatus {
            mapping: f.m.clone(),
            state: "Syncing".into(),
            message: String::new(),
            pending: 1,
            plan: Some(plan),
            done: 0,
            last_synced: None,
            files: 1,
        }],
        ..Status::default()
    };
    let history = f.engine.store.history(&f.m.id).unwrap();
    let synced = |_: &str, p: &str| history.contains_key(p);
    let state = |status: &Status, p: &str| file_state(status, &root.join(p), &synced);
    assert_eq!(state(&status, "docs/done.txt"), "synced");
    assert_eq!(state(&status, "docs/new.txt"), "syncing");
    assert_eq!(
        state(&status, "docs"),
        "syncing",
        "a folder shows pending contents"
    );
    assert_eq!(state(&status, "docs/edit.tmp"), "ignored");
    assert_eq!(state(&status, "elsewhere.txt"), "none");
    assert_eq!(
        file_state(&status, Path::new("/elsewhere"), &synced),
        "none"
    );
    status.mappings[0].mapping.paused = true;
    assert_eq!(state(&status, "docs/new.txt"), "paused");
    status.conflicts.push(Conflict {
        mapping: f.m.id.clone(),
        path: "docs/done.txt".into(),
        copy: "docs/done (conflicted copy).txt".into(),
        ..Conflict::default()
    });
    assert_eq!(
        state(&status, "docs/done (conflicted copy).txt"),
        "attention"
    );
}

#[test]
fn uploads_carry_the_files_own_modification_time() {
    let mut f = Fixture::new();
    f.file("old.txt", b"written long ago");
    let then = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
    fs::File::options()
        .write(true)
        .open(Path::new(&f.m.local).join("old.txt"))
        .unwrap()
        .set_modified(then)
        .unwrap();
    f.sync();
    assert_eq!(f.engine.remote.s().uploaded_modified["old.txt"], then);
}
