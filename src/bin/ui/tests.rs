use super::*;
use crate::TEST_REQUESTS;
use peedrive::model::Plan;
use serde_json::Value;

fn pump() {
    let context = glib::MainContext::default();
    for _ in 0..20 {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn descendants<T: IsA<gtk::Widget>>(widget: &gtk::Widget, out: &mut Vec<T>) {
    if let Some(found) = widget.downcast_ref::<T>() {
        out.push(found.clone());
    }
    let mut child = widget.first_child();
    while let Some(w) = child {
        descendants(&w, out);
        child = w.next_sibling();
    }
}
fn button(widget: &impl IsA<gtk::Widget>, text: &str) -> gtk::Button {
    let mut all = Vec::<gtk::Button>::new();
    descendants(widget.upcast_ref(), &mut all);
    all.into_iter()
        .find(|b| b.label().as_deref() == Some(text))
        .unwrap_or_else(|| panic!("No button labelled {text}"))
}
fn check_for(list: &gtk::ListBox, title: &str) -> gtk::CheckButton {
    let mut rows = Vec::<adw::ActionRow>::new();
    descendants(list.upcast_ref(), &mut rows);
    let row = rows.into_iter().find(|r| r.title() == title).expect("row");
    let mut checks = Vec::<gtk::CheckButton>::new();
    descendants(row.upcast_ref(), &mut checks);
    checks.remove(0)
}
fn reply(action: &str, result: Result<Value, String>) -> Value {
    pump();
    let (request, done) = TEST_REQUESTS.with(|p| p.borrow_mut().as_mut().unwrap().remove(0));
    assert_eq!(request["action"], action, "{request}");
    done(result);
    pump();
    request
}
fn capture(window: &adw::ApplicationWindow, name: &str) {
    if let Some(dir) = std::env::var_os("PEEDRIVE_TEST_SCREENSHOTS") {
        for _ in 0..10 {
            pump();
        }
        screenshot(window, &PathBuf::from(dir).join(name));
    }
}

#[test]
#[ignore = "requires a display; run with scripts/check-ui.sh"]
// GTK lives on one thread, so all dialogs are checked in this one test.
fn folders_apply_directly_review_only_removals_and_versions_restore() {
    adw::init().unwrap();
    gtk::Settings::default()
        .unwrap()
        .set_gtk_enable_animations(false);
    TEST_REQUESTS.with(|p| *p.borrow_mut() = Some(Vec::new()));
    let window = adw::ApplicationWindow::builder()
        .default_width(1000)
        .default_height(680)
        .build();
    let overlay = adw::ToastOverlay::new();
    let ctx = Ctx {
        window: window.clone(),
        overlay: overlay.clone(),
        status: Rc::default(),
        sidebar: gtk::ListBox::new(),
    };
    let folders = folders::Folders::new(&ctx);
    overlay.set_child(Some(&folders.widget));
    window.set_content(Some(&overlay));
    window.present();
    let status: Status = serde_json::from_value(json!({
        "signed_in": true, "sync_folder": "~/Proton Drive", "login_url": null, "account_error": null,
        "transfers": [], "activity": [],
        "mappings": [{"mapping": {"id":"m","local":"/tmp/peedrive-ui-test","remote":"root","remote_label":"My files",
            "root_device":1,"root_inode":1,"paused":false,"approved":true,"ignores":[],"selection":["Finance"]},
            "state":"Up to date","message":"","pending":0,"plan":null}]
    }))
    .unwrap();
    *ctx.status.borrow_mut() = status.clone();
    folders.update(&status);
    reply(
        "browse",
        Ok(json!({"uid":"root","children":[
        {"uid":"f","name":"Finance","kind":"folder","supported":true,"size":2000},
        {"uid":"s","name":"Scans","kind":"folder","supported":true,"size":1_100_000}]})),
    );
    assert!(!folders.apply.is_sensitive(), "Nothing changed yet");

    check_for(&folders.list, "Scans").set_active(true);
    pump();
    assert!(
        folders.summary.label().contains("will download"),
        "{}",
        folders.summary.label()
    );
    assert!(folders.apply.is_sensitive());
    folders.apply.emit_clicked();
    let sent = reply(
        "select-drive",
        Ok(json!({"mapping":{"id":"m"},"result":{"synced":true,"conflicts":0}})),
    );
    assert_eq!(sent["paths"], json!(["Finance", "Scans"]));
    assert!(
        window.visible_dialog().is_none(),
        "Adding folders must not open a review"
    );

    check_for(&folders.list, "Finance").set_active(false);
    pump();
    assert!(
        folders.summary.label().contains("leave this computer"),
        "{}",
        folders.summary.label()
    );
    folders.apply.emit_clicked();
    let plan = Plan {
        operations: vec![
            serde_json::from_value(
                json!({"path":"Old/a.pdf","action":"trash_local","reason":"Deleted remotely",
            "local":{"kind":"file","version":"v","uid":"u","size":5,"hash":null},"remote":null}),
            )
            .unwrap(),
        ],
        deletions: 1,
        conflicts: 0,
    };
    reply(
        "select-drive",
        Ok(json!({"mapping":{"id":"m"},"result":plan})),
    );
    let dialog = window.visible_dialog().expect("Removals must be reviewed");
    button(&dialog, "Put back in Drive");
    capture(&window, "peedrive-ui-review.png");
    button(&dialog, "Remove here too").emit_clicked();
    let sent = reply("approve", Ok(json!({"synced":true})));
    assert_eq!(sent["id"], "m");
    assert!(
        window.visible_dialog().is_none(),
        "The dialog closes after applying"
    );

    versions::show(&ctx, "m", "Letters/Lease.pdf");
    let sent = reply(
        "versions",
        Ok(json!([
            {"uid":"r2","time":1_790_000_000,"size":900_000,"active":true},
            {"uid":"r1","time":1_780_000_000,"size":850_000,"active":false}
        ])),
    );
    assert_eq!(sent["path"], "Letters/Lease.pdf");
    let dialog = window.visible_dialog().expect("Versions dialog");
    capture(&window, "peedrive-ui-versions.png");
    button(&dialog, "Restore").emit_clicked();
    let sent = reply("restore-version", Ok(json!({"synced":true})));
    assert_eq!(
        (sent["id"].as_str(), sent["revision"].as_str()),
        (Some("m"), Some("r1"))
    );
    assert!(
        window.visible_dialog().is_none(),
        "Restoring closes the dialog"
    );
    TEST_REQUESTS.with(|p| assert!(p.borrow().as_ref().unwrap().is_empty()));
    TEST_REQUESTS.with(|p| *p.borrow_mut() = None);
}
