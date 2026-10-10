use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Folder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: Kind,
    pub version: String,
    pub uid: String,
    pub size: u64,
    pub hash: Option<String>,
}
pub type Tree = BTreeMap<String, Entry>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub local: Entry,
    pub remote: Entry,
}
pub type History = BTreeMap<String, Baseline>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mapping {
    pub id: String,
    pub local: String,
    pub remote: String,
    pub remote_label: String,
    pub root_device: u64,
    pub root_inode: u64,
    pub paused: bool,
    pub approved: bool,
    pub ignores: Vec<String>,
    /// None syncs everything; Some selects paths and descendants.
    #[serde(default)]
    pub selection: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Upload,
    Download,
    MkdirLocal,
    MkdirRemote,
    TrashLocal,
    TrashRemote,
    Conflict,
    Record,
    Forget,
    /// Move or rename the Drive item at `from` to `path`, following a local move.
    MoveRemote,
    /// Move or rename the local item at `from` to `path`, following a Drive move.
    MoveLocal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub path: String,
    pub action: Action,
    pub local: Option<Entry>,
    pub remote: Option<Entry>,
    pub reason: String,
    /// Previous path of a moved item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    pub operations: Vec<Operation>,
    pub deletions: usize,
    pub conflicts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingStatus {
    pub mapping: Mapping,
    pub state: String,
    pub message: String,
    pub pending: usize,
    pub plan: Option<Plan>,
    /// Operations finished in the current run, out of `pending`.
    #[serde(default)]
    pub done: usize,
    /// Unix time of the last complete pass.
    #[serde(default)]
    pub last_synced: Option<i64>,
    /// Files currently in sync.
    #[serde(default)]
    pub files: usize,
}

/// One activity entry. `kind` is upload, download, folder, conflict, removed_local,
/// removed_remote, problem or info. `time` is UTC `YYYY-MM-DD HH:MM:SS`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub time: String,
    pub kind: String,
    pub path: Option<String>,
    pub message: String,
}

/// A local copy set aside under `.peedrive-recovery` before it was replaced or removed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Recovered {
    pub id: i64,
    pub mapping: String,
    /// Path relative to the sync folder where the file used to be.
    pub original: String,
    /// Absolute path of the kept copy.
    pub stored: String,
    pub reason: String,
    pub time: String,
    pub size: u64,
}

/// A conflict copy saved next to the Drive version of a file edited in both places.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Conflict {
    pub id: i64,
    pub mapping: String,
    pub path: String,
    pub copy: String,
    pub time: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub notifications: bool,
    /// None keeps recovered files forever.
    pub recovery_days: Option<u32>,
    pub pause_on_metered: bool,
    /// Bytes per second for each direction; None is unlimited.
    #[serde(default)]
    pub bandwidth_limit: Option<u64>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            notifications: true,
            recovery_days: None,
            pause_on_metered: false,
            bandwidth_limit: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Quota {
    pub used: u64,
    pub max: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub signed_in: bool,
    #[serde(default)]
    pub account_email: Option<String>,
    #[serde(default)]
    pub sync_folder: Option<String>,
    pub login_url: Option<String>,
    pub account_error: Option<String>,
    pub mappings: Vec<MappingStatus>,
    pub activity: Vec<Event>,
    /// Transfers in progress: id, path, size, bytes and direction.
    #[serde(default)]
    pub transfers: Vec<serde_json::Value>,
    #[serde(default)]
    pub recovered: Vec<Recovered>,
    #[serde(default)]
    pub conflicts: Vec<Conflict>,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub quota: Option<Quota>,
    /// True while sync is held because the connection is metered.
    #[serde(default)]
    pub metered: bool,
    /// False while NetworkManager reports no connection.
    #[serde(default)]
    pub online: bool,
}
