use super::*;
use peedrive::model::{Conflict, Recovered};

/// Everything that needs a decision, plus recovered copies to restore or delete.
pub struct Issues {
    pub widget: gtk::ScrolledWindow,
    ctx: Ctx,
    waiting: gtk::Box,
    recovered: adw::PreferencesGroup,
    empty_recovered: gtk::Button,
    recovered_rows: RefCell<Vec<adw::ActionRow>>,
    key: RefCell<String>,
}

impl Issues {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let body = vbox(24);
        margins(&body, 24, 12);
        let waiting = vbox(0);
        body.append(&waiting);
        let recovered = adw::PreferencesGroup::builder()
            .title("Recovered files")
            .description("Local copies set aside before they were replaced or removed.")
            .build();
        let empty_recovered = gtk::Button::with_label("Empty…");
        empty_recovered.add_css_class("flat");
        empty_recovered.add_css_class("destructive-action");
        recovered.set_header_suffix(Some(&empty_recovered));
        body.append(&recovered);
        let view = Rc::new(Self {
            widget: page_body(&body),
            ctx: ctx.clone(),
            waiting,
            recovered,
            empty_recovered,
            recovered_rows: RefCell::new(Vec::new()),
            key: RefCell::new(String::new()),
        });
        {
            let v = view.clone();
            view.empty_recovered.connect_clicked(move |_| {
                let ids: Vec<i64> = v
                    .ctx
                    .status
                    .borrow()
                    .recovered
                    .iter()
                    .map(|r| r.id)
                    .collect();
                confirm_delete(&v.ctx, "Delete all recovered files?", ids);
            });
        }
        view
    }

    pub fn update(&self, status: &Status) {
        let key = serde_json::to_string(&(
            status
                .mappings
                .iter()
                .map(|m| {
                    (
                        &m.state,
                        &m.message,
                        m.plan.as_ref().map(|p| p.operations.len()),
                    )
                })
                .collect::<Vec<_>>(),
            &status.conflicts,
            status.recovered.iter().map(|r| r.id).collect::<Vec<_>>(),
        ))
        .unwrap_or_default();
        if *self.key.borrow() == key {
            return;
        }
        *self.key.borrow_mut() = key;

        clear(&self.waiting);
        let group = adw::PreferencesGroup::builder()
            .title("Waiting for you")
            .build();
        let mut any = false;
        for m in &status.mappings {
            let id = m.mapping.id.clone();
            match m.state.as_str() {
                "Review changes" => {
                    any = true;
                    let row = adw::ActionRow::builder()
                        .title(overview::review_summary(m))
                        .subtitle("Nothing has changed yet.")
                        .use_markup(false)
                        .build();
                    row.add_prefix(&status_icon(
                        "dialog-warning-symbolic",
                        "tint-warning",
                        false,
                    ));
                    let review = gtk::Button::with_label("Review");
                    review.add_css_class("suggested-action");
                    review.set_valign(gtk::Align::Center);
                    let ctx = self.ctx.clone();
                    let plan = m.plan.clone();
                    review.connect_clicked(move |_| {
                        if let Some(plan) = &plan {
                            review::show(&ctx, &id, plan);
                        }
                    });
                    row.add_suffix(&review);
                    group.add(&row);
                }
                "Needs attention" => {
                    any = true;
                    let row = adw::ActionRow::builder()
                        .title("Sync stopped")
                        .subtitle(&m.message)
                        .subtitle_lines(4)
                        .use_markup(false)
                        .build();
                    row.add_prefix(&status_icon(
                        "dialog-warning-symbolic",
                        "tint-warning",
                        false,
                    ));
                    let retry = gtk::Button::with_label("Try again");
                    retry.set_valign(gtk::Align::Center);
                    let ctx = self.ctx.clone();
                    retry.connect_clicked(move |_| {
                        ctx.send(json!({"action":"resume","id":id}), |_| {});
                    });
                    row.add_suffix(&retry);
                    group.add(&row);
                }
                _ => {}
            }
        }
        for conflict in &status.conflicts {
            any = true;
            group.add(&self.conflict_row(conflict));
        }
        if any {
            self.waiting.append(&group);
        } else {
            let done = adw::StatusPage::builder()
                .icon_name("object-select-symbolic")
                .title("Nothing needs you")
                .description("Conflicts and changes that need a decision will appear here.")
                .build();
            done.add_css_class("compact");
            self.waiting.append(&done);
        }

        for row in self.recovered_rows.borrow_mut().drain(..) {
            self.recovered.remove(&row);
        }
        let total: u64 = status.recovered.iter().map(|r| r.size).sum();
        self.recovered
            .set_description(Some(&if status.recovered.is_empty() {
                "Local copies set aside before they were replaced or removed. None right now."
                    .into()
            } else {
                format!(
                    "Local copies set aside before they were replaced or removed · {}",
                    glib::format_size(total)
                )
            }));
        self.empty_recovered
            .set_visible(!status.recovered.is_empty());
        for item in status.recovered.iter().take(200) {
            let row = self.recovered_row(item);
            self.recovered.add(&row);
            self.recovered_rows.borrow_mut().push(row);
        }
    }

    fn conflict_row(&self, conflict: &Conflict) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        let content = vbox(10);
        margins(&content, 12, 12);
        let top = hbox(12);
        top.append(&status_icon("edit-copy-symbolic", "tint-warning", false));
        let text = vbox(2);
        text.append(&label(
            &format!("{} was edited in both places", file_name(&conflict.path)),
            &["heading"],
        ));
        text.append(&label(
            &format!(
                "{} · your version is saved as “{}”",
                parent_name(&conflict.path),
                file_name(&conflict.copy)
            ),
            &["dim-label", "caption"],
        ));
        top.append(&text);
        content.append(&top);
        let buttons = hbox(8);
        buttons.set_margin_start(42);
        let compare = gtk::Button::with_label("Compare");
        let mine = gtk::Button::with_label("Keep this computer’s");
        let drive = gtk::Button::with_label("Keep Drive’s");
        let both = gtk::Button::with_label("Keep both");
        both.add_css_class("flat");
        for b in [&compare, &mine, &drive, &both] {
            buttons.append(b);
        }
        content.append(&buttons);
        row.set_child(Some(&content));
        {
            let ctx = self.ctx.clone();
            let (a, b) = (conflict.path.clone(), conflict.copy.clone());
            compare.connect_clicked(move |_| {
                for p in [&a, &b] {
                    if let Some(path) = ctx.local_path(p) {
                        ctx.open(&path);
                    }
                }
            });
        }
        for (button, keep, done) in [
            (&mine, "local", "Kept this computer’s version"),
            (
                &drive,
                "drive",
                "Kept Drive’s version. Yours is in Recovered files.",
            ),
            (&both, "both", "Kept both versions"),
        ] {
            let ctx = self.ctx.clone();
            let id = conflict.id;
            button.connect_clicked(move |_| {
                let c = ctx.clone();
                ctx.send(
                    json!({"action":"resolve-conflict","conflict":id,"keep":keep}),
                    move |_| c.toast(done),
                );
            });
        }
        row
    }

    fn recovered_row(&self, item: &Recovered) -> adw::ActionRow {
        let when = short_time(&item.time);
        let row = adw::ActionRow::builder()
            .title(file_name(&item.original))
            .subtitle(format!(
                "{} · {} · {when} · {}",
                parent_name(&item.original),
                item.reason,
                glib::format_size(item.size)
            ))
            .use_markup(false)
            .build();
        let restore = gtk::Button::with_label("Restore");
        restore.set_valign(gtk::Align::Center);
        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.add_css_class("flat");
        delete.add_css_class("error");
        delete.set_valign(gtk::Align::Center);
        delete.set_tooltip_text(Some("Delete permanently"));
        row.add_suffix(&restore);
        row.add_suffix(&delete);
        {
            let ctx = self.ctx.clone();
            let id = item.id;
            let name = file_name(&item.original).to_string();
            restore.connect_clicked(move |_| {
                let c = ctx.clone();
                let name = name.clone();
                ctx.send(
                    json!({"action":"restore-recovered","ids":[id]}),
                    move |_| c.toast(&format!("Restored {name}")),
                );
            });
        }
        {
            let ctx = self.ctx.clone();
            let id = item.id;
            let name = file_name(&item.original).to_string();
            delete.connect_clicked(move |_| {
                confirm_delete(&ctx, &format!("Delete “{name}” permanently?"), vec![id])
            });
        }
        row
    }
}

fn confirm_delete(ctx: &Ctx, heading: &str, ids: Vec<i64>) {
    let dialog = adw::AlertDialog::new(
        Some(heading),
        Some("This cannot be undone. Proton Drive is not affected."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let ctx2 = ctx.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "delete" {
            let c = ctx2.clone();
            ctx2.send(json!({"action":"delete-recovered","ids":ids}), move |_| {
                c.toast("Deleted")
            });
        }
    });
    dialog.present(Some(&ctx.window));
}
