//! Session desktop integration for the daemon: notifications, network and suspend signals.
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use zbus::{
    blocking::{Connection, Proxy},
    zvariant::Value,
};

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";

pub fn open_app() {
    let sibling = std::env::current_exe()
        .map(|e| e.with_file_name("peedrive-app"))
        .ok()
        .filter(|p| p.exists());
    let _ = std::process::Command::new(sibling.unwrap_or_else(|| "peedrive-app".into()))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Desktop notifications. Each topic replaces its previous notification instead of stacking.
pub struct Notifier {
    connection: Option<Connection>,
    topics: HashMap<&'static str, u32>,
    ours: Arc<Mutex<Vec<u32>>>,
}
impl Notifier {
    pub fn new() -> Self {
        let connection = Connection::session().ok();
        let ours = Arc::new(Mutex::new(Vec::new()));
        if let Some(connection) = connection.clone() {
            let ours = ours.clone();
            std::thread::spawn(move || -> zbus::Result<()> {
                let proxy = Proxy::new(
                    &connection,
                    NOTIFICATIONS,
                    NOTIFICATIONS_PATH,
                    NOTIFICATIONS,
                )?;
                for signal in proxy.receive_signal("ActionInvoked")? {
                    let (id, _): (u32, String) = signal.body().deserialize()?;
                    if ours.lock().unwrap().contains(&id) {
                        open_app();
                    }
                }
                Ok(())
            });
        }
        Self {
            connection,
            topics: HashMap::new(),
            ours,
        }
    }
    pub fn send(&mut self, topic: &'static str, summary: &str, body: &str) {
        let Some(connection) = &self.connection else {
            return;
        };
        let hints = HashMap::from([("desktop-entry", Value::from(crate::service::APP_ID))]);
        let replaces = self.topics.get(topic).copied().unwrap_or(0);
        let sent = connection
            .call_method(
                Some(NOTIFICATIONS),
                NOTIFICATIONS_PATH,
                Some(NOTIFICATIONS),
                "Notify",
                &(
                    "Peedrive",
                    replaces,
                    crate::service::APP_ID,
                    summary,
                    body,
                    vec!["default", "Open Peedrive"],
                    hints,
                    -1i32,
                ),
            )
            .and_then(|reply| reply.body().deserialize::<u32>());
        if let Ok(id) = sent {
            self.topics.insert(topic, id);
            let mut ours = self.ours.lock().unwrap();
            ours.retain(|old| *old != replaces);
            ours.push(id);
        }
    }
}
impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

/// NetworkManager and logind signals. `offline` follows NetworkManager when it is present;
/// `wake` is raised when connectivity returns or the machine resumes, to retry immediately.
pub fn watch_system(wake: Arc<AtomicBool>, offline: Arc<AtomicBool>) {
    let Ok(connection) = Connection::system() else {
        return;
    };
    let network = connection.clone();
    let sleep_wake = wake.clone();
    std::thread::spawn(move || -> zbus::Result<()> {
        let nm = Proxy::new(
            &network,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
        )?;
        // Asleep, disconnected or disconnecting. Unknown (0) and partial states still try.
        let down = |state: u32| (10..=30).contains(&state);
        if let Ok(state) = nm.get_property::<u32>("State") {
            offline.store(down(state), Ordering::Relaxed);
        }
        for signal in nm.receive_signal("StateChanged")? {
            let state: u32 = signal.body().deserialize()?;
            let was_down = offline.swap(down(state), Ordering::Relaxed);
            if state >= 70 || (was_down && !down(state)) {
                wake.store(true, Ordering::Relaxed);
            }
        }
        Ok(())
    });
    std::thread::spawn(move || -> zbus::Result<()> {
        let logind = Proxy::new(
            &connection,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )?;
        for signal in logind.receive_signal("PrepareForSleep")? {
            if !signal.body().deserialize::<bool>()? {
                sleep_wake.store(true, Ordering::Relaxed);
            }
        }
        Ok(())
    });
}

/// NetworkManager's view of whether the primary connection is metered.
pub fn metered() -> bool {
    (|| -> zbus::Result<bool> {
        let connection = Connection::system()?;
        let nm = Proxy::new(
            &connection,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
        )?;
        // NM_METERED_YES = 1, NM_METERED_GUESS_YES = 3.
        Ok(matches!(nm.get_property::<u32>("Metered")?, 1 | 3))
    })()
    .unwrap_or(false)
}
