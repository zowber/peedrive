use super::*;
use peedrive::model::{Action, Plan};

/// Shown only when a plan would remove files or has conflicts.
pub fn show(ctx: &Ctx, id: &str, plan: &Plan) {
    let count = |a: Action| plan.operations.iter().filter(|o| o.action == a).count();
    let (here, there, conflicts) = (
        count(Action::TrashLocal),
        count(Action::TrashRemote),
        plan.conflicts,
    );
    let files = |n: usize| {
        if n == 1 {
            "1 file".to_string()
        } else {
            format!("{n} files")
        }
    };
    let (title, body, keep, remove) = match (here, there) {
        (0, 0) => (
            format!("{} edited in both places", if conflicts == 1 { "1 file was".into() } else { format!("{conflicts} files were") }),
            "Both versions will be kept: Drive’s at the original name, and yours as a conflicted copy beside it.".to_string(),
            None,
            "Keep both versions",
        ),
        (n, 0) => (
            format!("{} removed from Proton Drive", if n == 1 { "1 file was".into() } else { format!("{n} files were") }),
            if n == 1 {
                "Peedrive is waiting before removing it here too. If you meant to delete it, remove it here. If not, put it back in Drive."
            } else {
                "Peedrive is waiting before removing them here too. If you meant to delete them, remove them here. If not, put them back in Drive."
            }
            .to_string(),
            Some("Put back in Drive"),
            "Remove here too",
        ),
        (0, n) => (
            format!("{} deleted on this computer", if n == 1 { "1 file was".into() } else { format!("{n} files were") }),
            if n == 1 {
                "Peedrive is waiting before moving it to Drive Trash. If you didn’t mean to delete it, restore it here."
            } else {
                "Peedrive is waiting before moving them to Drive Trash. If you didn’t mean to delete them, restore them here."
            }
            .to_string(),
            Some("Restore here"),
            "Move to Drive Trash",
        ),
        (a, b) => (
            format!("{} would be removed", files(a + b)),
            format!("{} left Proton Drive and {} were deleted on this computer.", files(a), files(b)),
            Some("Keep all files"),
            "Remove them",
        ),
    };
    let dialog = adw::Dialog::builder()
        .title("Review changes")
        .content_width(600)
        .content_height(560)
        .build();
    let body_box = vbox(16);
    margins(&body_box, 6, 24);
    body_box.set_margin_bottom(24);
    body_box.append(&label(&title, &["title-2"]));
    body_box.append(&label(&body, &[]));
    if conflicts > 0 && here + there > 0 {
        body_box.append(&label(
            &format!(
                "{} edited in both places will keep both versions.",
                files(conflicts)
            ),
            &["dim-label"],
        ));
    }
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_valign(gtk::Align::Start);
    let relevant: Vec<_> = plan
        .operations
        .iter()
        .filter(|o| {
            matches!(
                o.action,
                Action::TrashLocal | Action::TrashRemote | Action::Conflict
            )
        })
        .collect();
    for op in relevant.iter().take(300) {
        let what = match op.action {
            Action::TrashLocal => "Removed from Drive",
            Action::TrashRemote => "Deleted on this computer",
            _ => "Edited in both places",
        };
        let row = adw::ActionRow::builder()
            .title(file_name(&op.path))
            .subtitle(format!("{} · {what}", parent_name(&op.path)))
            .use_markup(false)
            .build();
        if let Some(size) = op
            .local
            .as_ref()
            .or(op.remote.as_ref())
            .map(|e| e.size)
            .filter(|s| *s > 0)
        {
            let l = label(&glib::format_size(size), &["dim-label", "caption"]);
            l.set_wrap(false);
            row.add_suffix(&l);
        }
        list.append(&row);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    body_box.append(&scroll);
    if relevant.len() > 300 {
        body_box.append(&label(
            &format!("And {} more.", relevant.len() - 300),
            &["dim-label"],
        ));
    }
    if here > 0 {
        body_box.append(&label(
            "Removed local copies go to Recovered files, where you can restore them at any time.",
            &["caption", "dim-label"],
        ));
    }
    let error = label("", &["error"]);
    error.set_visible(false);
    body_box.append(&error);
    let buttons = hbox(10);
    buttons.set_halign(gtk::Align::End);
    let spinner = adw::Spinner::new();
    spinner.set_visible(false);
    buttons.append(&spinner);
    let keep_button = keep.map(|text| {
        let b = gtk::Button::with_label(text);
        buttons.append(&b);
        b
    });
    let remove_button = gtk::Button::with_label(remove);
    remove_button.add_css_class(if here + there > 0 {
        "destructive-action"
    } else {
        "suggested-action"
    });
    buttons.append(&remove_button);
    body_box.append(&buttons);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&body_box));
    dialog.set_child(Some(&toolbar));

    let run = Rc::new({
        let ctx = ctx.clone();
        let dialog = dialog.clone();
        let id = id.to_string();
        let buttons = buttons.clone();
        move |action: &'static str, done: &'static str| {
            buttons.set_sensitive(false);
            spinner.set_visible(true);
            error.set_visible(false);
            let ctx = ctx.clone();
            let dialog = dialog.clone();
            let buttons = buttons.clone();
            let spinner = spinner.clone();
            let error = error.clone();
            request(json!({"action":action,"id":id}), move |r| {
                spinner.set_visible(false);
                buttons.set_sensitive(true);
                match r {
                    Ok(_) => {
                        dialog.close();
                        ctx.toast(done);
                    }
                    Err(e) => {
                        error.set_label(&e);
                        error.set_visible(true);
                    }
                }
            });
        }
    });
    if let Some(button) = keep_button {
        let run = run.clone();
        button.connect_clicked(move |_| run("keep", "Files kept. They will sync back."));
    }
    remove_button.connect_clicked(move |_| run("approve", "Changes applied"));
    dialog.present(Some(&ctx.window));
}
