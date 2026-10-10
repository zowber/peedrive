mod ui;
use adw::prelude::*;
use gtk::glib;
use peedrive::service;
use serde_json::Value;
#[cfg(test)]
use std::cell::RefCell;
use std::{sync::mpsc, time::Duration};

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(service::APP_ID)
        .build();
    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        ui::build(app);
    });
    app.set_accels_for_action("app.preferences", &["<Control>comma"]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);
    app.run()
}

#[cfg(test)]
type PendingRequest = (Value, Box<dyn FnOnce(Result<Value, String>)>);
#[cfg(test)]
thread_local! {
    static TEST_REQUESTS: RefCell<Option<Vec<PendingRequest>>> = const { RefCell::new(None) };
}
/// Run a service request off the main thread and deliver the reply on it.
fn request<F: FnOnce(Result<Value, String>) + 'static>(value: Value, done: F) {
    #[cfg(test)]
    if TEST_REQUESTS.with(|pending| pending.borrow().is_some()) {
        TEST_REQUESTS.with(|pending| {
            pending
                .borrow_mut()
                .as_mut()
                .unwrap()
                .push((value, Box::new(done)))
        });
        return;
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(service::call(value).map_err(|e| format!("{e:#}")));
    });
    let mut done = Some(done);
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(result) => {
            done.take().unwrap()(result);
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(_) => {
            done.take().unwrap()(Err("Background operation stopped".into()));
            glib::ControlFlow::Break
        }
    });
}
