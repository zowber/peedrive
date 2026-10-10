use crate::{
    desktop::{self, Notifier},
    engine::{Engine, SHUTDOWN, Stopped, requires_deletion_review},
    files, local,
    model::*,
    remote::{Adapter, ErrorKind, Remote, RemoteFailure, adapter_path},
    store::Store,
};
use anyhow::{Context, Result, bail, ensure};
use notify::{RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};

pub const APP_ID: &str = "io.github.zowber.Peedrive";
pub const BUS: &str = "io.github.zowber.Peedrive.Sync";
pub const OBJECT: &str = "/io/github/zowber/Peedrive";
/// Shown wherever Peedrive asks someone to sign in, as Proton's SDK guidelines require.
pub const THIRD_PARTY: &str = "This is a third-party application not officially supported by Proton.";
type Jobs = Arc<Mutex<BTreeMap<String, Value>>>;
struct Request {
    id: String,
    value: Value,
}
struct Control {
    sender: mpsc::Sender<Request>,
    status: Arc<Mutex<Status>>,
    jobs: Jobs,
    stop: Arc<AtomicBool>,
    /// A read-only view of the state database, for per-file status while the worker is busy.
    baseline: Mutex<Option<rusqlite::Connection>>,
    database: PathBuf,
}
#[zbus::interface(name = "io.github.zowber.Peedrive.Sync")]
impl Control {
    fn status(&self) -> String {
        serde_json::to_string(&*self.status.lock().unwrap()).unwrap()
    }
    fn submit(&self, request: &str) -> zbus::fdo::Result<String> {
        if request.len() > 1024 * 1024 {
            return Err(zbus::fdo::Error::InvalidArgs("Request too large".into()));
        }
        let value: Value = serde_json::from_str(request)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        if value["action"] == "quit" {
            // Stop between operations now rather than after the current sync pass.
            SHUTDOWN.store(true, Ordering::Relaxed);
        }
        if value["action"] == "pause"
            && let Some(id) = value["id"].as_str()
            && self
                .status
                .lock()
                .unwrap()
                .mappings
                .iter()
                .any(|m| m.mapping.id.starts_with(id) && m.state == "Syncing")
        {
            self.stop.store(true, Ordering::Relaxed);
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.sender
            .send(Request {
                id: id.clone(),
                value,
            })
            .map_err(|_| zbus::fdo::Error::Failed("Sync worker stopped".into()))?;
        Ok(id)
    }
    /// One state per absolute path, for file manager emblems: see `files::file_state`.
    fn file_states(&self, paths: Vec<String>) -> String {
        let mut db = self.baseline.lock().unwrap();
        if db.is_none() {
            *db = rusqlite::Connection::open_with_flags(
                &self.database,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .ok();
        }
        let synced = |mapping: &str, path: &str| {
            db.as_ref().is_some_and(|db| {
                db.query_row(
                    "SELECT 1 FROM baseline WHERE mapping=?1 AND path=?2",
                    [mapping, path],
                    |_| Ok(()),
                )
                .is_ok()
            })
        };
        let status = self.status.lock().unwrap();
        let states: Vec<_> = paths
            .iter()
            .map(|p| files::file_state(&status, Path::new(p), &synced))
            .collect();
        serde_json::to_string(&states).unwrap()
    }
    fn job(&self, id: &str) -> String {
        serde_json::to_string(
            &self
                .jobs
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .unwrap_or(Value::Null),
        )
        .unwrap()
    }
}
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("PEEDRIVE_DATA_DIR") {
        return p.into();
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/share"));
    base.join("peedrive")
}
extern "C" fn on_terminate(_: libc::c_int) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}
/// Errors that clear up without user action, with the state and message to show meanwhile.
fn waiting(e: &anyhow::Error) -> Option<(&'static str, String)> {
    if e.downcast_ref::<Stopped>() == Some(&Stopped::Shutdown) {
        return Some(("Waiting to retry", "Stopped for shutdown".into()));
    }
    if let Some(failure) = e.downcast_ref::<RemoteFailure>() {
        return match failure.kind {
            ErrorKind::Network => Some((
                "Offline",
                format!(
                    "Proton Drive is unreachable ({failure}). Sync continues when the connection returns."
                ),
            )),
            ErrorKind::Auth => Some((
                "Signed out",
                "Sign in to Proton Drive to continue syncing. Your files are safe.".into(),
            )),
            ErrorKind::Retry => Some(("Waiting to retry", failure.to_string())),
            ErrorKind::Fatal => None,
        };
    }
    let text = e.to_string();
    [
        "while reading",
        "during scan",
        "Sync folder is unavailable",
        "disk is unavailable",
    ]
    .iter()
    .any(|t| text.contains(t))
    .then(|| {
        (
            "Waiting to retry",
            format!("{text}. Sync will retry automatically."),
        )
    })
}

pub fn daemon() -> Result<()> {
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_terminate as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_terminate as *const () as libc::sighandler_t,
        );
    }
    let data = data_dir();
    local::private_dir(&data)?;
    // One daemon per state directory, whichever bus it was started on: two would race
    // on rotating session tokens and on the same folders.
    let lock = std::fs::File::create(data.join("daemon.lock"))?;
    ensure!(
        unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&lock),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        } == 0,
        "Another Peedrive service is already using {}",
        data.display()
    );
    let store = Store::open(&data.join("state.sqlite"))?;
    let status = Arc::new(Mutex::new(Status {
        online: true,
        ..Status::default()
    }));
    let jobs: Jobs = Arc::new(Mutex::new(BTreeMap::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let control = Control {
        sender,
        status: status.clone(),
        jobs: jobs.clone(),
        stop: stop.clone(),
        baseline: Mutex::new(None),
        database: data.join("state.sqlite"),
    };
    let _connection = zbus::blocking::connection::Builder::session()?
        .name(BUS)?
        .serve_at(OBJECT, control)?
        .build()?;
    let adapter = Adapter::spawn(&adapter_path(), &data, status.clone())?;
    let progress = status.clone();
    let mut d = Daemon {
        engine: Engine {
            store,
            remote: adapter,
            stop,
            // Transfers report themselves through the adapter; this counts operations.
            on_op: Some(Box::new(move |id, index, total, _| {
                let mut s = progress.lock().unwrap();
                if let Some(m) = s.mappings.iter_mut().find(|m| m.mapping.id == id) {
                    m.done = index;
                    m.pending = total;
                }
            })),
        },
        status: status.clone(),
        strict: BTreeSet::new(),
        applying: false,
        signing_out: false,
        notifier: Notifier::new(),
    };
    d.engine.remote.call("hello", json!({}))?;
    for mut mapping in d.engine.store.mappings()? {
        if mapping.remote_label == "root" {
            mapping.remote_label = "My files".into();
            d.engine.store.save_mapping(&mapping)?;
        }
        if d.engine.store.interrupted(&mapping.id)? {
            // Replanning is safe after a crash, but a half-replaced file can look like a
            // deletion, so deletions and conflicts wait for review until a clean pass.
            d.engine.store.acknowledge_recovery(&mapping.id)?;
            d.strict.insert(mapping.id.clone());
            d.engine.store.activity(
                "Checking files after an interrupted sync. Deletions and conflicts will wait for review.",
            )?;
        }
    }
    refresh_status(&d.engine.store, &status)?;
    let dirty = Arc::new(AtomicBool::new(false));
    let watch_dirty = dirty.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.as_ref().map_or(true, |e| {
            !e.paths.iter().all(|p| {
                p.components()
                    .any(|c| c.as_os_str().to_string_lossy().starts_with(".peedrive"))
            })
        }) {
            watch_dirty.store(true, Ordering::Relaxed);
        }
    })?;
    let wake = Arc::new(AtomicBool::new(false));
    let offline = Arc::new(AtomicBool::new(false));
    desktop::watch_system(wake.clone(), offline.clone());
    let mut watched = Vec::new();
    let mut next_sync = Instant::now();
    let mut next_account = Instant::now();
    let mut backoff: BTreeMap<String, (Instant, u64)> = BTreeMap::new();
    let mut clock = (Instant::now(), SystemTime::now());
    let mut next_prune = Instant::now();
    while !SHUTDOWN.load(Ordering::Relaxed) {
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(request) => {
                let output = match d.handle(&request.value) {
                    Ok(v) => json!({"result":v}),
                    Err(e) => json!({"error":format!("{e:#}")}),
                };
                let mut j = jobs.lock().unwrap();
                if j.len() > 100 {
                    j.clear();
                }
                j.insert(request.id, output);
                drop(j);
                let _ = refresh_status(&d.engine.store, &status);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
        // Suspend pauses the monotonic clock but not the wall clock.
        let now = (Instant::now(), SystemTime::now());
        let wall = now.1.duration_since(clock.1).unwrap_or_default();
        if wall > now.0.duration_since(clock.0) + Duration::from_secs(30) {
            wake.store(true, Ordering::Relaxed);
        }
        clock = now;
        if wake.swap(false, Ordering::Relaxed) {
            backoff.clear();
            next_account = Instant::now();
            next_sync = Instant::now() + Duration::from_secs(5);
        }
        for mapping in d.engine.store.mappings()? {
            if !watched.contains(&mapping.local)
                && watcher
                    .watch(Path::new(&mapping.local), RecursiveMode::Recursive)
                    .is_ok()
            {
                watched.push(mapping.local);
            }
        }
        if Instant::now() >= next_account {
            let was_signed_in = status.lock().unwrap().signed_in;
            if let Err(e) = d.account_status() {
                let message = e.to_string();
                if status.lock().unwrap().account_error.as_deref() != Some(&message) {
                    d.engine
                        .store
                        .activity(&format!("Could not connect to Proton Drive: {message}"))?;
                }
                status.lock().unwrap().account_error = Some(message);
            }
            if !was_signed_in && status.lock().unwrap().signed_in {
                backoff.clear();
                next_sync = Instant::now();
            }
            next_account = Instant::now() + Duration::from_secs(3);
            let _ = refresh_status(&d.engine.store, &status);
        }
        if Instant::now() >= next_prune {
            if let Err(e) = d.prune_recovered() {
                d.engine.store.event(
                    "problem",
                    None,
                    &format!("Could not clean up recovered files: {e}"),
                )?;
            }
            next_prune = Instant::now() + Duration::from_secs(3600);
        }
        // NetworkManager can miss connections it does not manage, so it only informs the UI
        // and triggers early retries; failed requests decide whether a mapping is offline.
        status.lock().unwrap().online = !offline.load(Ordering::Relaxed);
        if Instant::now() >= next_sync {
            let signed_in = status.lock().unwrap().signed_in;
            let metered = d.engine.store.settings()?.pause_on_metered && desktop::metered();
            status.lock().unwrap().metered = metered;
            for mapping in d.engine.store.mappings()? {
                if mapping.paused {
                    continue;
                }
                if metered {
                    d.set(
                        &mapping.id,
                        "Paused",
                        "Paused on a metered connection".into(),
                        None,
                    );
                    continue;
                }
                if !signed_in {
                    d.set(
                        &mapping.id,
                        "Signed out",
                        "Sign in to Proton Drive to continue syncing. Your files are safe.".into(),
                        None,
                    );
                    continue;
                }
                if backoff
                    .get(&mapping.id)
                    .is_none_or(|(when, _)| Instant::now() >= *when)
                {
                    if d.sync_mapping(&mapping).is_err() {
                        let delay = backoff
                            .get(&mapping.id)
                            .map_or(15, |(_, seconds)| (seconds * 2).min(300));
                        backoff.insert(
                            mapping.id.clone(),
                            (Instant::now() + Duration::from_secs(delay), delay),
                        );
                    } else {
                        backoff.remove(&mapping.id);
                    }
                }
            }
            dirty.store(false, Ordering::Relaxed);
            next_sync = Instant::now() + Duration::from_secs(30);
            let _ = refresh_status(&d.engine.store, &status);
        } else if dirty.swap(false, Ordering::Relaxed) {
            // Debounce writes; event API calls remain bounded by the adapter's refresh interval.
            next_sync = next_sync.min(Instant::now() + Duration::from_secs(3));
        }
    }
    // Dropping the daemon closes the helper's input and lets it finish its last request.
    Ok(())
}
fn refresh_status(store: &Store, status: &Arc<Mutex<Status>>) -> Result<()> {
    let mappings = store.mappings()?;
    let mut s = status.lock().unwrap();
    s.mappings
        .retain(|old| mappings.iter().any(|m| m.id == old.mapping.id));
    for mapping in mappings {
        if let Some(old) = s
            .mappings
            .iter_mut()
            .find(|old| old.mapping.id == mapping.id)
        {
            old.mapping = mapping;
        } else {
            s.mappings.push(MappingStatus {
                state: if mapping.paused { "Paused" } else { "Waiting" }.into(),
                message: String::new(),
                last_synced: store
                    .setting(&format!("last_synced:{}", mapping.id))?
                    .and_then(|v| v.parse().ok()),
                files: store.file_count(&mapping.id)?,
                mapping,
                pending: 0,
                plan: None,
                done: 0,
            });
        }
    }
    s.activity = store.recent()?;
    s.sync_folder = store.setting("sync_folder")?;
    s.recovered = store.recovered()?;
    s.conflicts = store.conflicts()?;
    s.settings = store.settings()?;
    Ok(())
}

struct Daemon {
    engine: Engine<Adapter>,
    status: Arc<Mutex<Status>>,
    /// Mappings whose last run was cut short: deletions and conflicts need review.
    strict: BTreeSet<String>,
    applying: bool,
    signing_out: bool,
    notifier: Notifier,
}
impl Daemon {
    fn set(&self, id: &str, state: &str, message: String, plan: Option<Plan>) {
        if let Some(s) = self
            .status
            .lock()
            .unwrap()
            .mappings
            .iter_mut()
            .find(|s| s.mapping.id == id)
        {
            if state != "Syncing" {
                s.done = 0;
            }
            s.state = state.into();
            s.message = message;
            s.pending = plan.as_ref().map_or(0, |p| {
                p.operations
                    .iter()
                    .filter(|o| !matches!(o.action, Action::Record | Action::Forget))
                    .count()
            });
            s.plan = plan;
        }
    }
    fn notify(&mut self, topic: &'static str, summary: &str, body: &str) {
        if self.engine.store.settings().is_ok_and(|s| s.notifications) {
            self.notifier.send(topic, summary, body);
        }
    }
    fn prune_recovered(&mut self) -> Result<()> {
        if let Some(days) = self.engine.store.settings()?.recovery_days {
            for item in self.engine.store.expired_recovered(days)? {
                self.delete_recovered(&item)?;
            }
        }
        Ok(())
    }
    /// The kept copy lives alone in `.peedrive-recovery/<uuid>/`; remove that folder.
    fn delete_recovered(&mut self, item: &Recovered) -> Result<()> {
        let stored = Path::new(&item.stored);
        let holder = stored.parent().context("Invalid recovered path")?;
        ensure!(
            holder
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n == ".peedrive-recovery"),
            "Refusing to delete outside the recovery folder"
        );
        match std::fs::remove_dir_all(holder) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        self.engine.store.remove_recovered(item.id)
    }
    /// Move deselected local items into recovery. Only paths the old selection covered are
    /// touched, so local files that were never synced stay where they are.
    fn remove_deselected(&mut self, old: &Mapping, new: &Mapping) -> Result<()> {
        // Never move files out of a folder that is not the one Peedrive synced.
        local::verify_root(old)?;
        let root = Path::new(&old.local);
        let mut queue = vec![String::new()];
        while let Some(dir) = queue.pop() {
            let Ok(entries) = std::fs::read_dir(root.join(&dir)) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if name.starts_with(".peedrive") {
                    continue;
                }
                let path = if dir.is_empty() {
                    name
                } else {
                    format!("{dir}/{name}")
                };
                if !local::selected(old, &path, true) {
                    continue;
                }
                if local::selected(new, &path, true) {
                    if !local::selected(new, &path, false) && entry.path().is_dir() {
                        queue.push(path);
                    }
                    continue;
                }
                if !local::selected(old, &path, false) {
                    // An ancestor kept only as scaffolding for the old selection.
                    queue.push(path);
                    continue;
                }
                let backup = local::recovery_path(root, &path)?;
                local::rename_new(&local::rooted(root, &path)?, &backup)?;
                self.engine.store.add_recovered(
                    &old.id,
                    &path,
                    &backup,
                    "Unticked in Choose folders",
                )?;
                self.engine.store.forget_tree(&old.id, &path)?;
                self.engine.store.event(
                    "removed_local",
                    Some(&path),
                    &format!("Stopped syncing {path}; the local copy is in Recovered files"),
                )?;
            }
        }
        Ok(())
    }
    fn account_status(&mut self) -> Result<Value> {
        let result = self.engine.remote.call("auth.status", json!({}))?;
        let signed_in = result["signed_in"].as_bool().unwrap_or(false);
        let was_signed_in = self.status.lock().unwrap().signed_in;
        if signed_in && !was_signed_in {
            self.engine.store.activity("Connected to Proton Drive")?;
        } else if !signed_in && was_signed_in {
            self.engine
                .store
                .activity("Disconnected from Proton Drive. Local files are kept.")?;
            if !self.signing_out {
                self.notify(
                    "account",
                    "Sign in to keep syncing",
                    &format!(
                        "Proton Drive ended this session. Your files are safe; sync resumes after you sign in. {THIRD_PARTY}"
                    ),
                );
            }
        }
        let error = result["error"].as_str();
        if let Some(error) = error
            && self.status.lock().unwrap().account_error.as_deref() != Some(error)
        {
            self.engine
                .store
                .activity(&format!("Proton Drive connection: {error}"))?;
        }
        let mut s = self.status.lock().unwrap();
        s.signed_in = signed_in;
        s.account_email = result["email"].as_str().map(str::to_string);
        s.login_url = result["login_url"].as_str().map(str::to_string);
        s.account_error = error.map(str::to_string);
        s.quota = serde_json::from_value(result["quota"].clone()).ok();
        Ok(result)
    }
    /// The mapping plus either a plan to review, `synced`, or an error that will retry.
    /// The mapping's selection without `path`, listing folders from Drive and this computer.
    fn deselect(&mut self, m: &Mapping, path: &str) -> Result<Vec<String>> {
        let remote = self.engine.remote.snapshot(&m.remote)?;
        let root = Path::new(&m.local);
        Ok(files::deselect(m.selection.clone(), path, &mut |dir| {
            let parent = |p: &str| Path::new(p).parent().map(Path::to_path_buf);
            let mut items: Vec<String> = remote
                .keys()
                .filter(|p| parent(p).as_deref() == Some(Path::new(dir)))
                .cloned()
                .collect();
            for entry in std::fs::read_dir(root.join(dir))
                .into_iter()
                .flatten()
                .flatten()
            {
                if let Some(name) = entry.file_name().to_str()
                    && !name.starts_with(".peedrive")
                {
                    items.push(if dir.is_empty() {
                        name.into()
                    } else {
                        format!("{dir}/{name}")
                    });
                }
            }
            items
        }))
    }
    fn selection_result(&mut self, mapping: &Mapping) -> Value {
        let result = match self.sync_mapping(mapping) {
            Ok(v) => v,
            Err(e) => json!({"error": format!("{e:#}")}),
        };
        json!({"mapping": mapping, "result": result})
    }
    fn sync_mapping(&mut self, mapping: &Mapping) -> Result<Value> {
        self.applying = false;
        let result = self.plan_and_apply(mapping);
        self.settle(mapping, result)
    }
    fn plan_and_apply(&mut self, mapping: &Mapping) -> Result<Value> {
        self.set(
            &mapping.id,
            "Checking",
            "Comparing local and remote changes".into(),
            None,
        );
        let plan = self.engine.prepare(mapping)?;
        let strict = self.strict.contains(&mapping.id);
        let held = requires_deletion_review(&plan)
            || (strict && (plan.deletions > 0 || plan.conflicts > 0));
        if !mapping.approved || held {
            if mapping.approved {
                self.notify(
                    "review",
                    "Review changes before syncing",
                    &format!(
                        "{} deletions and {} conflicts are waiting for your review.",
                        plan.deletions, plan.conflicts
                    ),
                );
            }
            self.set(
                &mapping.id,
                "Review changes",
                "Review the planned changes before syncing".into(),
                Some(plan.clone()),
            );
            return Ok(json!(plan));
        }
        self.apply(mapping, &plan)
    }
    fn apply(&mut self, mapping: &Mapping, plan: &Plan) -> Result<Value> {
        self.set(
            &mapping.id,
            "Syncing",
            "Applying changes".into(),
            Some(plan.clone()),
        );
        self.applying = true;
        self.engine.execute(mapping, plan)?;
        Ok(json!({"synced":true,"conflicts":plan.conflicts}))
    }
    /// One place decides whether a failure waits, pauses, or needs review.
    fn settle(&mut self, mapping: &Mapping, result: Result<Value>) -> Result<Value> {
        let applying = std::mem::take(&mut self.applying);
        let e = match result {
            Ok(value) => {
                if value["synced"] == true {
                    self.strict.remove(&mapping.id);
                    self.set(&mapping.id, "Up to date", "All changes synced".into(), None);
                    let now = SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs() as i64);
                    self.engine
                        .store
                        .save_setting(&format!("last_synced:{}", mapping.id), &now.to_string())?;
                    let files = self.engine.store.file_count(&mapping.id)?;
                    if let Some(m) = self
                        .status
                        .lock()
                        .unwrap()
                        .mappings
                        .iter_mut()
                        .find(|m| m.mapping.id == mapping.id)
                    {
                        m.last_synced = Some(now);
                        m.files = files;
                    }
                    let conflicts = value["conflicts"].as_u64().unwrap_or(0);
                    if conflicts > 0 {
                        self.notify(
                            "conflict",
                            "Kept both versions",
                            &format!(
                                "{conflicts} files were edited in both places. Your copy was saved next to the Drive version."
                            ),
                        );
                    }
                }
                return Ok(value);
            }
            Err(e) => e,
        };
        if e.downcast_ref::<Stopped>() == Some(&Stopped::Paused) {
            let mut paused = mapping.clone();
            paused.paused = true;
            self.engine.store.save_mapping(&paused)?;
            self.set(
                &mapping.id,
                "Paused",
                "Paused after the current transfer".into(),
                None,
            );
            return Ok(json!({"paused":true}));
        }
        if let Some((state, message)) = waiting(&e) {
            if applying {
                // The interrupted operation may or may not have reached Proton.
                self.strict.insert(mapping.id.clone());
            }
            self.set(&mapping.id, state, message, None);
            return Err(e);
        }
        let mut paused = mapping.clone();
        paused.paused = true;
        paused.approved = false;
        self.engine.store.save_mapping(&paused)?;
        self.set(&mapping.id, "Needs attention", e.to_string(), None);
        self.engine.store.activity(&format!("Sync stopped: {e}"))?;
        self.notify("attention", "Sync needs attention", &e.to_string());
        Err(e)
    }
    fn handle(&mut self, value: &Value) -> Result<Value> {
        let action = value["action"].as_str().context("Missing action")?;
        match action {
            "live-test" => return crate::live_test::run(&mut self.engine, &data_dir()),
            "quit" => {
                SHUTDOWN.store(true, Ordering::Relaxed);
                return Ok(json!({}));
            }
            "setup" => {
                let folder = value["folder"]
                    .as_str()
                    .context("Choose a local sync folder")?;
                let expanded = expand_sync_folder(folder, Some("account@example.com"))?;
                ensure!(
                    !data_dir().starts_with(&expanded),
                    "Choose a folder outside Peedrive's state directory"
                );
                ensure!(
                    self.engine.store.setting("sync_folder")?.is_none()
                        || self.engine.store.mappings()?.is_empty(),
                    "Disconnect existing folder pairs before changing the default location; files will be kept"
                );
                self.engine.store.save_setting("sync_folder", folder)?;
                self.engine
                    .store
                    .activity(&format!("Local sync folder chosen: {folder}"))?;
                return Ok(json!({}));
            }
            "select-drive" => {
                let info = self.engine.remote.call("browse", json!({}))?;
                let root = info["uid"].as_str().context("Missing Drive root")?;
                let existing = self
                    .engine
                    .store
                    .mappings()?
                    .into_iter()
                    .find(|m| m.remote == root);
                // GNOME Files adds or removes one item; the app sends the whole selection.
                let selection = match (value["add"].as_str(), value["remove"].as_str()) {
                    (Some(path), _) => {
                        local::validate_relative(path)?;
                        match &existing {
                            Some(m) => files::select(m.selection.clone(), path),
                            None => Some(vec![path.to_string()]),
                        }
                    }
                    (_, Some(path)) => {
                        local::validate_relative(path)?;
                        let m = existing.as_ref().context("Nothing is syncing yet")?;
                        Some(self.deselect(m, path)?)
                    }
                    _ => Some(serde_json::from_value(value["paths"].clone())?),
                };
                for path in selection.iter().flatten() {
                    local::validate_relative(path)?;
                }
                if let Some(old) = existing {
                    let mut mapping = old.clone();
                    mapping.selection = selection;
                    mapping.paused = false;
                    mapping.approved = true;
                    self.remove_deselected(&old, &mapping)?;
                    self.engine.store.save_mapping(&mapping)?;
                    self.engine
                        .store
                        .activity("Updated the folders that sync")?;
                    // Additions apply at once; deletions or conflicts wait for review.
                    self.strict.insert(mapping.id.clone());
                    refresh_status(&self.engine.store, &self.status)?;
                    return Ok(self.selection_result(&mapping));
                }
                let paths = selection.unwrap_or_default();
                ensure!(!paths.is_empty(), "Select at least one file or folder");
                let folder = self
                    .engine
                    .store
                    .setting("sync_folder")?
                    .context("Choose your local sync folder first")?;
                let email = self.status.lock().unwrap().account_email.clone();
                let local = expand_sync_folder(&folder, email.as_deref())?;
                // Check all existing components before creating the selected destination.
                let relative = local
                    .strip_prefix("/")?
                    .to_str()
                    .context("Invalid folder")?;
                local::rooted(Path::new("/"), relative)?;
                std::fs::create_dir_all(&local)?;
                return self.handle(
                    &json!({"action":"add","local":local,"remote":root,"selection":paths}),
                );
            }
            "login" => {
                self.signing_out = false;
                self.engine
                    .store
                    .activity("Opening Proton Drive sign-in in your browser")?;
                let v = match self.engine.remote.call("auth.login", json!({})) {
                    Ok(v) => v,
                    Err(e) => {
                        self.engine
                            .store
                            .activity(&format!("Could not start Proton Drive sign-in: {e}"))?;
                        return Err(e);
                    }
                };
                self.account_status()?;
                return Ok(v);
            }
            "logout" => {
                self.signing_out = true;
                for mut m in self.engine.store.mappings()? {
                    m.paused = true;
                    self.engine.store.save_mapping(&m)?;
                }
                self.engine.remote.call("auth.logout", json!({}))?;
                self.account_status()?;
                return Ok(json!({}));
            }
            "browse" => {
                return self
                    .engine
                    .remote
                    .call("browse", json!({"uid":value.get("uid")}));
            }
            "search" => {
                return self
                    .engine
                    .remote
                    .call("search", json!({"query":value["query"]}));
            }
            "settings" => {
                let s = &value["settings"];
                if let Some(v) = s["notifications"].as_bool() {
                    self.engine
                        .store
                        .save_setting("notifications", &v.to_string())?;
                }
                if let Some(v) = s["pause_on_metered"].as_bool() {
                    self.engine
                        .store
                        .save_setting("pause_on_metered", &v.to_string())?;
                }
                if let Some(v) = s.get("bandwidth_limit") {
                    let limit = v.as_u64().map_or(String::new(), |b| b.to_string());
                    self.engine.store.save_setting("bandwidth_limit", &limit)?;
                }
                if let Some(v) = s.get("recovery_days") {
                    let days = v.as_u64().map_or(String::new(), |d| d.to_string());
                    self.engine.store.save_setting("recovery_days", &days)?;
                    self.prune_recovered()?;
                }
                return Ok(json!(self.engine.store.settings()?));
            }
            "restore-recovered" | "delete-recovered" => {
                let ids: Vec<i64> = serde_json::from_value(value["ids"].clone())?;
                let mappings = self.engine.store.mappings()?;
                for item in self.engine.store.recovered()? {
                    if !ids.contains(&item.id) {
                        continue;
                    }
                    if action == "delete-recovered" {
                        self.delete_recovered(&item)?;
                        continue;
                    }
                    let mapping = mappings
                        .iter()
                        .find(|m| m.id == item.mapping)
                        .context("Its sync folder is no longer connected")?;
                    let root = Path::new(&mapping.local);
                    local::verify_root(mapping)?;
                    let mut target = item.original.clone();
                    if local::rooted(root, &target)?.exists() {
                        target = restored_name(&item.original);
                    }
                    let destination = local::rooted(root, &target)?;
                    if let Some(parent) = destination.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    local::rename_new(Path::new(&item.stored), &destination)?;
                    let _ = std::fs::remove_dir(Path::new(&item.stored).parent().unwrap_or(root));
                    self.engine.store.remove_recovered(item.id)?;
                    self.engine.store.event(
                        "info",
                        Some(&target),
                        &format!("Restored {target}"),
                    )?;
                }
                return Ok(json!({}));
            }
            "resolve-conflict" => {
                let id = value["conflict"].as_i64().context("Missing conflict")?;
                let keep = value["keep"]
                    .as_str()
                    .context("Choose which version to keep")?;
                let conflict = self
                    .engine
                    .store
                    .conflicts()?
                    .into_iter()
                    .find(|c| c.id == id)
                    .context("Conflict not found")?;
                let mapping = self
                    .engine
                    .store
                    .mappings()?
                    .into_iter()
                    .find(|m| m.id == conflict.mapping)
                    .context("Its sync folder is no longer connected")?;
                let root = Path::new(&mapping.local);
                local::verify_root(&mapping)?;
                let copy = local::rooted(root, &conflict.copy)?;
                let original = local::rooted(root, &conflict.path)?;
                match keep {
                    "both" => {}
                    "drive" if copy.exists() => {
                        let backup = local::recovery_path(root, &conflict.copy)?;
                        local::rename_new(&copy, &backup)?;
                        self.engine.store.add_recovered(
                            &mapping.id,
                            &conflict.copy,
                            &backup,
                            "Conflict resolved in favour of Drive",
                        )?;
                    }
                    "local" if copy.exists() => {
                        if original.exists() {
                            let backup = local::recovery_path(root, &conflict.path)?;
                            local::rename_new(&original, &backup)?;
                            self.engine.store.add_recovered(
                                &mapping.id,
                                &conflict.path,
                                &backup,
                                "Conflict resolved in favour of this computer",
                            )?;
                        }
                        local::rename_new(&copy, &original)?;
                    }
                    "drive" | "local" => {}
                    _ => bail!("Unknown conflict choice"),
                }
                self.engine.store.remove_conflict(id)?;
                return Ok(json!({}));
            }
            "move-folder" => {
                let folder = value["folder"].as_str().context("Choose a folder")?;
                let target = expand_sync_folder(folder, None)?;
                ensure!(!target.exists(), "Choose a folder that does not exist yet");
                ensure!(
                    !data_dir().starts_with(&target),
                    "Choose a folder outside Peedrive's state directory"
                );
                let mut mappings = self.engine.store.mappings()?;
                ensure!(mappings.len() <= 1, "Move works with a single sync folder");
                if let Some(mapping) = mappings.first_mut() {
                    local::verify_root(mapping)?;
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::rename(&mapping.local, &target).map_err(|e| {
                        if e.raw_os_error() == Some(libc::EXDEV) {
                            anyhow::anyhow!("Choose a folder on the same disk as the current one")
                        } else {
                            e.into()
                        }
                    })?;
                    mapping.local = target.to_str().context("Invalid UTF-8 folder")?.into();
                    (mapping.root_device, mapping.root_inode) = local::root_identity(&target)?;
                    self.engine.store.save_mapping(mapping)?;
                }
                self.engine
                    .store
                    .save_setting("sync_folder", target.to_str().context("Invalid folder")?)?;
                self.engine
                    .store
                    .activity(&format!("Moved the sync folder to {}", target.display()))?;
                return Ok(json!({}));
            }
            "add" => {
                let supplied = Path::new(value["local"].as_str().context("Missing local folder")?);
                ensure!(
                    !std::fs::symlink_metadata(supplied)?
                        .file_type()
                        .is_symlink(),
                    "Choose a real folder, not a symbolic link"
                );
                let local = supplied.canonicalize()?;
                let remote = value["remote"].as_str().context("Missing remote folder")?;
                let info = self.engine.remote.call("browse", json!({"uid":remote}))?;
                for existing in self.engine.store.mappings()? {
                    let old = Path::new(&existing.local);
                    ensure!(
                        !local.starts_with(old) && !old.starts_with(&local),
                        "Local folder overlaps an existing mapping"
                    );
                    ensure!(existing.remote != remote, "Remote folder is already mapped");
                    let old_info = self
                        .engine
                        .remote
                        .call("browse", json!({"uid":existing.remote}))?;
                    let ancestors = |v: &Value, id: &str| {
                        v["ancestors"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(id)))
                    };
                    ensure!(
                        !ancestors(&info, &existing.remote) && !ancestors(&old_info, remote),
                        "Remote folder overlaps an existing mapping"
                    );
                }
                ensure!(
                    !data_dir().starts_with(&local),
                    "The sync root cannot contain Peedrive's state directory"
                );
                let (root_device, root_inode) = local::root_identity(&local)?;
                let m = Mapping {
                    id: uuid::Uuid::new_v4().to_string(),
                    local: local.to_str().context("Invalid UTF-8 folder")?.into(),
                    remote: remote.into(),
                    remote_label: info["name"].as_str().unwrap_or("Drive folder").into(),
                    root_device,
                    root_inode,
                    paused: false,
                    approved: true,
                    ignores: vec![],
                    selection: value
                        .get("selection")
                        .map(|v| serde_json::from_value(v.clone()))
                        .transpose()?,
                };
                if let Some(paths) = &m.selection {
                    for path in paths {
                        local::validate_relative(path)?;
                    }
                }
                self.engine.store.save_mapping(&m)?;
                self.engine.store.activity(&format!(
                    "Added sync folder: {} → {}",
                    m.remote_label, m.local
                ))?;
                self.strict.insert(m.id.clone());
                refresh_status(&self.engine.store, &self.status)?;
                return Ok(self.selection_result(&m));
            }
            _ => {}
        }
        let id = value["id"].as_str().context("Missing mapping ID")?;
        let mut mapping = self
            .engine
            .store
            .mappings()?
            .into_iter()
            .find(|m| m.id == id || m.id.starts_with(id))
            .context("Mapping not found")?;
        match action {
            "versions" | "restore-version" => {
                let path = value["path"].as_str().context("Missing path")?;
                local::validate_relative(path)?;
                ensure!(
                    local::selected(&mapping, path, false),
                    "This file is not syncing"
                );
                let Some(revision) = value["revision"].as_str() else {
                    return self
                        .engine
                        .remote
                        .call("revisions", json!({"root":mapping.remote,"path":path}));
                };
                self.engine.remote.call(
                    "restoreRevision",
                    json!({"root":mapping.remote,"path":path,"revision":revision}),
                )?;
                self.engine.store.event(
                    "restored",
                    Some(path),
                    &format!("Restored an earlier version of {path}"),
                )?;
                // The restored version downloads like any other Drive edit; the replaced
                // local copy goes to Recovered files, or is kept beside it if edited.
                if mapping.paused {
                    return Ok(json!({"restored":true}));
                }
                return self.sync_mapping(&mapping);
            }
            "pause" => {
                mapping.paused = true;
                self.engine.store.save_mapping(&mapping)?;
                self.engine.stop.store(false, Ordering::Relaxed);
                self.set(
                    &mapping.id,
                    "Paused",
                    "Sync will resume when you choose Resume".into(),
                    None,
                );
            }
            "resume" => {
                mapping.paused = false;
                if self.engine.store.interrupted(&mapping.id)? {
                    mapping.approved = false;
                }
                self.engine.stop.store(false, Ordering::Relaxed);
                self.engine.store.acknowledge_recovery(&mapping.id)?;
                self.engine.store.save_mapping(&mapping)?;
                return self.sync_mapping(&mapping);
            }
            "remove" => {
                self.engine.store.remove_mapping(&mapping.id)?;
                self.strict.remove(&mapping.id);
                return Ok(json!({"local_and_remote_files_preserved":true}));
            }
            "ignore" => {
                let patterns: Vec<String> = serde_json::from_value(value["patterns"].clone())?;
                local::ignores(&patterns)?;
                mapping.ignores = patterns;
                self.engine.store.save_mapping(&mapping)?;
                // Newly excluded items are kept on both sides; anything that would delete waits.
                self.strict.insert(mapping.id.clone());
                return self.sync_mapping(&mapping);
            }
            "preview" => {
                let plan = self.engine.prepare(&mapping)?;
                self.set(
                    &mapping.id,
                    "Review changes",
                    "Review this plan before applying it".into(),
                    Some(plan.clone()),
                );
                return Ok(json!(plan));
            }
            "keep" => {
                // Undo the held deletions: forgetting their history turns each survivor
                // into a new item that is copied back to the side that lost it.
                let plan = self
                    .status
                    .lock()
                    .unwrap()
                    .mappings
                    .iter()
                    .find(|m| m.mapping.id == mapping.id)
                    .and_then(|m| m.plan.clone())
                    .context("Preview changes first")?;
                for op in &plan.operations {
                    if matches!(op.action, Action::TrashLocal | Action::TrashRemote) {
                        self.engine.store.forget_tree(&mapping.id, &op.path)?;
                    }
                }
                mapping.approved = true;
                mapping.paused = false;
                self.engine.store.save_mapping(&mapping)?;
                return self.sync_mapping(&mapping);
            }
            "approve" => {
                let shown = self
                    .status
                    .lock()
                    .unwrap()
                    .mappings
                    .iter()
                    .find(|m| m.mapping.id == mapping.id)
                    .and_then(|m| m.plan.clone())
                    .context("Preview changes first")?;
                let current = self.engine.prepare(&mapping)?;
                ensure!(
                    serde_json::to_value(&shown)? == serde_json::to_value(&current)?,
                    "Files changed since preview. Preview the new plan before applying it."
                );
                mapping.approved = true;
                mapping.paused = false;
                self.engine.stop.store(false, Ordering::Relaxed);
                self.engine.store.save_mapping(&mapping)?;
                // A reviewed plan counts as reviewed, even after an interruption.
                self.strict.remove(&mapping.id);
                self.applying = false;
                let result = self.apply(&mapping, &current);
                return self.settle(&mapping, result);
            }
            "sync" => {
                ensure!(!mapping.paused, "Mapping is paused. Resume it first.");
                return self.sync_mapping(&mapping);
            }
            _ => bail!("Unknown action"),
        }
        Ok(json!({}))
    }
}
pub fn get_status() -> Result<Status> {
    let connection = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&connection, BUS, OBJECT, BUS)?;
    let response: String = proxy.call("Status", &())?;
    Ok(serde_json::from_str(&response)?)
}
pub fn call(request: Value) -> Result<Value> {
    let connection = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&connection, BUS, OBJECT, BUS)?;
    let id: String = proxy.call("Submit", &(serde_json::to_string(&request)?,))?;
    let deadline = Instant::now() + Duration::from_secs(900);
    loop {
        let response: String = proxy.call("Job", &(&id,))?;
        let value: Value = serde_json::from_str(&response)?;
        if let Some(error) = value.get("error") {
            bail!("{}", error.as_str().unwrap_or("Operation failed"));
        }
        if let Some(result) = value.get("result") {
            return Ok(result.clone());
        }
        ensure!(
            Instant::now() < deadline,
            "Operation is still running. Check status."
        );
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Resolve the suggested account path only after the SDK supplies the account email.
pub fn expand_sync_folder(folder: &str, email: Option<&str>) -> Result<PathBuf> {
    let folder = if let Some(suffix) = folder.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").context("Home folder unavailable")?).join(suffix)
    } else {
        PathBuf::from(folder)
    };
    ensure!(
        folder.is_absolute(),
        "Choose an absolute folder path or a path starting with ~/"
    );
    let mut resolved = PathBuf::new();
    for component in folder.components() {
        match component {
            std::path::Component::RootDir => resolved.push("/"),
            std::path::Component::Normal(part) if part == "{account email}" => {
                let email = email.context(
                    "Waiting for your Proton account email before creating the default folder",
                )?;
                ensure!(
                    !email.is_empty() && !email.contains('/') && email != "." && email != "..",
                    "Invalid account email for folder name"
                );
                resolved.push(email);
            }
            std::path::Component::Normal(part) => resolved.push(part),
            _ => anyhow::bail!("Choose a folder without parent-directory components"),
        }
    }
    ensure!(
        resolved != Path::new("/"),
        "Choose a folder below your home directory"
    );
    Ok(resolved)
}

/// `notes.txt` → `notes (restored).txt`, next to the original location.
fn restored_name(path: &str) -> String {
    let (dir, name) = path.rsplit_once('/').map_or(("", path), |(d, n)| (d, n));
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let renamed = format!(
        "{stem} (restored {}){ext}",
        &uuid::Uuid::new_v4().to_string()[..4]
    );
    if dir.is_empty() {
        renamed
    } else {
        format!("{dir}/{renamed}")
    }
}
