use crate::model::*;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Proton is unreachable or overloaded.
    Network,
    /// The account session is missing or expired.
    Auth,
    /// The helper restarted; the outcome of its last request is unknown.
    Retry,
    /// Anything that needs a person to look at it.
    Fatal,
}
#[derive(Debug)]
pub struct RemoteFailure {
    pub message: String,
    pub kind: ErrorKind,
}
impl std::fmt::Display for RemoteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for RemoteFailure {}

/// Calls may overlap: the engine runs several transfers at once.
pub trait Remote: Sync {
    fn snapshot(&self, root: &str) -> Result<Tree>;
    fn snapshot_selected(&self, root: &str, _selection: Option<&[String]>) -> Result<Tree> {
        self.snapshot(root)
    }
    fn inspect(&self, root: &str, path: &str) -> Result<Option<Entry>>;
    fn upload(
        &self,
        root: &str,
        path: &str,
        source: &Path,
        expected: &Option<Entry>,
        hash: &str,
    ) -> Result<Entry>;
    /// `path` names the file in progress reports.
    fn download(&self, path: &str, entry: &Entry, destination: &Path) -> Result<()>;
    fn mkdir(&self, root: &str, path: &str) -> Result<Entry>;
    fn trash(&self, root: &str, path: &str, expected: &Entry) -> Result<()>;
    /// Move or rename `expected`, found at `from`, to `to`. Drive keeps its revisions.
    fn relocate(&self, root: &str, from: &str, to: &str, expected: &Entry) -> Result<Entry>;
}

/// Callers waiting for a response, by request ID. None once the helper's output has ended.
type Waiting = Arc<Mutex<Option<HashMap<u64, mpsc::Sender<Value>>>>>;

/// One running helper process.
struct Helper {
    child: Child,
    input: Option<ChildStdin>,
    waiting: Waiting,
}
impl Drop for Helper {
    /// Closing stdin lets the helper finish its current requests, such as persisting a
    /// rotated session token, before it exits.
    fn drop(&mut self) {
        self.input.take();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The SDK helper. Requests from several threads share it; a reader thread routes each
/// response and progress event to its request by ID.
pub struct Adapter {
    executable: PathBuf,
    data: PathBuf,
    helper: Mutex<Helper>,
    next: AtomicU64,
    pub status: Arc<Mutex<Status>>,
}
fn stopped() -> anyhow::Error {
    RemoteFailure {
        message: "The Proton Drive helper stopped unexpectedly. Retrying.".into(),
        kind: ErrorKind::Retry,
    }
    .into()
}
impl Adapter {
    pub fn spawn(executable: &Path, data: &Path, status: Arc<Mutex<Status>>) -> Result<Self> {
        Ok(Self {
            helper: Mutex::new(Self::start(executable, data, &status)?),
            executable: executable.into(),
            data: data.into(),
            next: AtomicU64::new(0),
            status,
        })
    }
    fn start(executable: &Path, data: &Path, status: &Arc<Mutex<Status>>) -> Result<Helper> {
        let mut command = Command::new(executable);
        command
            .env("PEEDRIVE_DATA_DIR", data)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().with_context(|| {
            format!(
                "Could not start SDK adapter at {}. Run scripts/build.sh first.",
                executable.display()
            )
        })?;
        let input = child.stdin.take();
        let output = BufReader::new(child.stdout.take().unwrap());
        let waiting: Waiting = Arc::new(Mutex::new(Some(HashMap::new())));
        let (routes, status) = (waiting.clone(), status.clone());
        std::thread::spawn(move || route(output, &routes, &status));
        Ok(Helper {
            child,
            input,
            waiting,
        })
    }
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.request(method, params, None)
    }
    /// `transfer` (direction, path, size) is shown in the status while the request runs.
    fn request(&self, method: &str, params: Value, transfer: Option<Value>) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = mpsc::channel();
        {
            let mut helper = self.helper.lock().unwrap();
            let routed = helper.waiting.lock().unwrap().is_some();
            if !routed || helper.child.try_wait()?.is_some() {
                *helper = Self::start(&self.executable, &self.data, &self.status)?;
            }
            match helper.waiting.lock().unwrap().as_mut() {
                Some(waiting) => waiting.insert(id, tx),
                None => return Err(stopped()),
            };
            if let Some(mut t) = transfer {
                t["id"] = id.into();
                t["bytes"] = 0.into();
                self.status.lock().unwrap().transfers.push(t);
            }
            let input = helper
                .input
                .as_mut()
                .context("SDK adapter is shutting down")?;
            let sent = writeln!(
                input,
                "{}",
                json!({"id":id,"method":method,"params":params})
            )
            .and_then(|_| input.flush());
            if sent.is_err() {
                // The reader sees the helper exit and drops our sender.
                let _ = helper.child.kill();
            }
        }
        let value = rx.recv();
        self.status
            .lock()
            .unwrap()
            .transfers
            .retain(|t| t["id"] != id);
        let value = value.map_err(|_| stopped())?;
        if let Some(error) = value.get("error") {
            return Err(RemoteFailure {
                message: error["message"]
                    .as_str()
                    .unwrap_or("SDK operation failed")
                    .into(),
                kind: match error["kind"].as_str() {
                    Some("network") => ErrorKind::Network,
                    Some("auth") => ErrorKind::Auth,
                    _ => ErrorKind::Fatal,
                },
            }
            .into());
        }
        Ok(value["result"].clone())
    }
    /// The bandwidth limit for new transfers, in bytes per second.
    fn limit(&self) -> Option<u64> {
        self.status.lock().unwrap().settings.bandwidth_limit
    }
}
/// Deliver responses and progress until the helper's output ends, then release every
/// waiting caller so it reports a restart instead of blocking.
fn route(mut output: BufReader<ChildStdout>, waiting: &Waiting, status: &Mutex<Status>) {
    let mut line = String::new();
    loop {
        line.clear();
        match output.read_line(&mut line) {
            Ok(n) if n > 0 && line.len() < 64 * 1024 * 1024 => {}
            _ => break,
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            break;
        };
        if value.get("event").is_some_and(|e| e == "progress") {
            let mut status = status.lock().unwrap();
            if let Some(t) = status.transfers.iter_mut().find(|t| t["id"] == value["id"]) {
                t["bytes"] = value["bytes"].clone();
            }
            continue;
        }
        let sender = value["id"]
            .as_u64()
            .and_then(|id| waiting.lock().unwrap().as_mut().and_then(|w| w.remove(&id)));
        if let Some(sender) = sender {
            let _ = sender.send(value);
        }
    }
    waiting.lock().unwrap().take();
}
impl Remote for Adapter {
    fn snapshot_selected(&self, root: &str, selection: Option<&[String]>) -> Result<Tree> {
        Ok(serde_json::from_value(self.call(
            "snapshot",
            json!({"root":root,"selection":selection}),
        )?)?)
    }
    fn snapshot(&self, root: &str) -> Result<Tree> {
        Ok(serde_json::from_value(
            self.call("snapshot", json!({"root":root}))?,
        )?)
    }
    fn inspect(&self, root: &str, path: &str) -> Result<Option<Entry>> {
        Ok(serde_json::from_value(
            self.call("inspect", json!({"root":root,"path":path}))?,
        )?)
    }
    fn upload(
        &self,
        root: &str,
        path: &str,
        source: &Path,
        expected: &Option<Entry>,
        hash: &str,
    ) -> Result<Entry> {
        let size = std::fs::metadata(source)?.len();
        Ok(serde_json::from_value(self.request(
            "upload",
            json!({"root":root,"path":path,"source":source,"expected":expected,"hash":hash,"limit":self.limit()}),
            Some(json!({"direction":"upload","path":path,"size":size})),
        )?)?)
    }
    fn download(&self, path: &str, entry: &Entry, destination: &Path) -> Result<()> {
        self.request(
            "download",
            json!({"entry":entry,"destination":destination,"limit":self.limit()}),
            Some(json!({"direction":"download","path":path,"size":entry.size})),
        )?;
        Ok(())
    }
    fn mkdir(&self, root: &str, path: &str) -> Result<Entry> {
        Ok(serde_json::from_value(
            self.call("mkdir", json!({"root":root,"path":path}))?,
        )?)
    }
    fn trash(&self, root: &str, path: &str, expected: &Entry) -> Result<()> {
        self.call(
            "trash",
            json!({"root":root,"path":path,"expected":expected}),
        )?;
        Ok(())
    }
    fn relocate(&self, root: &str, from: &str, to: &str, expected: &Entry) -> Result<Entry> {
        Ok(serde_json::from_value(self.call(
            "move",
            json!({"root":root,"from":from,"to":to,"expected":expected}),
        )?)?)
    }
}
pub fn adapter_path() -> PathBuf {
    if let Some(p) = std::env::var_os("PEEDRIVE_ADAPTER") {
        return p.into();
    }
    let installed = PathBuf::from("/usr/lib/peedrive/peedrive-adapter");
    if installed.exists() {
        installed
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dist/peedrive-adapter")
    }
}
