use super::*;

/// Rebuilds the exclusion rows; set once the rows exist, so it can refer to itself.
type Render = Rc<RefCell<Option<Box<dyn Fn()>>>>;

fn systemd_enabled(unit: &str) -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "is-enabled", "--quiet", unit])
        .status()
        .is_ok_and(|s| s.success())
}
fn systemd_set(ctx: &Ctx, unit: &str, on: bool) {
    let mut args = vec!["--user", if on { "enable" } else { "disable" }];
    // The tray can start and stop now; the sync service only changes what happens at login.
    if unit.contains("indicator") {
        args.push("--now");
    }
    args.push(unit);
    let ok = std::process::Command::new("systemctl")
        .args(&args)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        ctx.toast(&format!("Could not change {unit}"));
    }
}
fn setting(ctx: &Ctx, key: &str, value: serde_json::Value) {
    ctx.send(json!({"action":"settings","settings":{key:value}}), |_| {});
}

pub fn show(ctx: &Ctx) {
    let status = ctx.status.borrow().clone();
    let dialog = adw::PreferencesDialog::new();
    dialog.set_search_enabled(false);

    // General
    let general = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("emblem-system-symbolic")
        .build();
    let location = adw::PreferencesGroup::builder().title("Location").build();
    let folder_path = ctx.mapping().map(|m| m.mapping.local).or_else(|| {
        status.sync_folder.as_deref().and_then(|f| {
            peedrive::service::expand_sync_folder(f, status.account_email.as_deref())
                .ok()
                .map(|p| p.display().to_string())
        })
    });
    let folder_row = adw::ActionRow::builder()
        .title("Sync folder")
        .subtitle(
            folder_path
                .clone()
                .unwrap_or_else(|| "Not chosen yet".into()),
        )
        .use_markup(false)
        .build();
    let open = gtk::Button::with_label("Open");
    open.set_valign(gtk::Align::Center);
    let move_button = gtk::Button::with_label("Move…");
    move_button.set_valign(gtk::Align::Center);
    folder_row.add_suffix(&open);
    folder_row.add_suffix(&move_button);
    location.add(&folder_row);
    general.add(&location);
    if let Some(path) = folder_path.clone() {
        let ctx = ctx.clone();
        open.connect_clicked(move |_| ctx.open(std::path::Path::new(&path)));
    }
    {
        let ctx = ctx.clone();
        let folder_row = folder_row.clone();
        let current = folder_path.clone();
        move_button.connect_clicked(move |_| {
            let ctx = ctx.clone();
            let folder_row = folder_row.clone();
            let name = current
                .as_deref()
                .and_then(|p| std::path::Path::new(p).file_name())
                .map_or("Proton Drive".into(), |n| n.to_string_lossy().to_string());
            let window = ctx.window.clone();
            gtk::FileDialog::builder()
                .title("Choose where the sync folder goes")
                .accept_label("Move here")
                .build()
                .select_folder(Some(&window), None::<&gio::Cancellable>, move |r| {
                    let Some(parent) = r.ok().and_then(|f| f.path()) else {
                        return;
                    };
                    let target = parent.join(&name).display().to_string();
                    let c = ctx.clone();
                    ctx.send(json!({"action":"move-folder","folder":target}), move |_| {
                        folder_row.set_subtitle(&target);
                        c.toast("Sync folder moved");
                    });
                });
        });
    }

    let behaviour = adw::PreferencesGroup::builder().title("Behaviour").build();
    let login = adw::SwitchRow::builder()
        .title("Start at login")
        .subtitle("Sync in the background whenever you sign in")
        .active(systemd_enabled("peedrive.service"))
        .build();
    let tray = adw::SwitchRow::builder()
        .title("Show in the top bar")
        .subtitle("Status and quick pause from the system tray")
        .active(systemd_enabled("peedrive-indicator.service"))
        .build();
    let notifications = adw::SwitchRow::builder()
        .title("Notifications")
        .subtitle("Conflicts, held changes and sign-in problems only")
        .active(status.settings.notifications)
        .build();
    let metered = adw::SwitchRow::builder()
        .title("Pause on metered connections")
        .subtitle("Mobile hotspots and capped networks")
        .active(status.settings.pause_on_metered)
        .build();
    {
        let ctx = ctx.clone();
        login.connect_active_notify(move |r| systemd_set(&ctx, "peedrive.service", r.is_active()));
    }
    {
        let ctx = ctx.clone();
        tray.connect_active_notify(move |r| {
            systemd_set(&ctx, "peedrive-indicator.service", r.is_active())
        });
    }
    {
        let ctx = ctx.clone();
        notifications
            .connect_active_notify(move |r| setting(&ctx, "notifications", r.is_active().into()));
    }
    {
        let ctx = ctx.clone();
        metered.connect_active_notify(move |r| {
            setting(&ctx, "pause_on_metered", r.is_active().into())
        });
    }
    for row in [&login, &tray, &notifications, &metered] {
        behaviour.add(row);
    }
    general.add(&behaviour);

    let recovery = adw::PreferencesGroup::builder()
        .title("Recovered files")
        .build();
    let keep = adw::ComboRow::builder()
        .title("Keep recovered files")
        .subtitle("Local copies set aside before they were replaced or removed")
        .model(&gtk::StringList::new(&["Forever", "90 days", "30 days"]))
        .selected(match status.settings.recovery_days {
            Some(30) => 2,
            Some(_) => 1,
            None => 0,
        })
        .build();
    {
        let ctx = ctx.clone();
        keep.connect_selected_notify(move |r| {
            let days = match r.selected() {
                1 => json!(90),
                2 => json!(30),
                _ => serde_json::Value::Null,
            };
            setting(&ctx, "recovery_days", days);
        });
    }
    recovery.add(&keep);
    general.add(&recovery);

    let transfers = adw::PreferencesGroup::builder()
        .title("Transfers")
        .description("Up to three files upload or download at once.")
        .build();
    // Bytes per second, in the decimal units the rest of the app shows sizes in.
    const LIMITS: [Option<u64>; 5] = [
        None,
        Some(10_000_000),
        Some(5_000_000),
        Some(1_000_000),
        Some(500_000),
    ];
    let speed = adw::ComboRow::builder()
        .title("Speed limit")
        .subtitle("Applies to uploads and downloads separately")
        .model(&gtk::StringList::new(&[
            "Unlimited",
            "10 MB/s",
            "5 MB/s",
            "1 MB/s",
            "500 kB/s",
        ]))
        .selected(
            LIMITS
                .iter()
                .position(|l| *l == status.settings.bandwidth_limit)
                .unwrap_or(0) as u32,
        )
        .build();
    {
        let ctx = ctx.clone();
        speed.connect_selected_notify(move |r| {
            let limit = LIMITS.get(r.selected() as usize).copied().flatten();
            setting(&ctx, "bandwidth_limit", json!(limit));
        });
    }
    transfers.add(&speed);
    general.add(&transfers);
    dialog.add(&general);

    // Exclusions
    let exclusions = adw::PreferencesPage::builder()
        .title("Exclusions")
        .icon_name("action-unavailable-symbolic")
        .build();
    let patterns_group = adw::PreferencesGroup::builder()
        .title("Never sync")
        .description("Files and folders matching these patterns stay where they are. For example *.tmp, **/node_modules or .cache")
        .build();
    exclusions.add(&patterns_group);
    if let Some(mapping) = ctx.mapping() {
        let patterns = Rc::new(RefCell::new(mapping.mapping.ignores.clone()));
        let rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::default();
        let render: Render = Rc::default();
        let add = adw::EntryRow::builder()
            .title("Add a pattern")
            .show_apply_button(true)
            .build();
        {
            let (ctx, patterns, rows, group, render2, add) = (
                ctx.clone(),
                patterns.clone(),
                rows.clone(),
                patterns_group.clone(),
                render.clone(),
                add.clone(),
            );
            let id = mapping.mapping.id.clone();
            *render.borrow_mut() = Some(Box::new(move || {
                for row in rows.borrow_mut().drain(..) {
                    group.remove(&row);
                }
                group.remove(&add);
                for (i, pattern) in patterns.borrow().iter().enumerate() {
                    let row = adw::ActionRow::builder()
                        .title(pattern)
                        .use_markup(false)
                        .build();
                    let remove = gtk::Button::from_icon_name("list-remove-symbolic");
                    remove.add_css_class("flat");
                    remove.set_valign(gtk::Align::Center);
                    remove.set_tooltip_text(Some("Remove pattern"));
                    let (ctx, patterns, render, id) =
                        (ctx.clone(), patterns.clone(), render2.clone(), id.clone());
                    remove.connect_clicked(move |_| {
                        patterns.borrow_mut().remove(i);
                        let list = patterns.borrow().clone();
                        ctx.send(json!({"action":"ignore","id":id,"patterns":list}), |_| {});
                        if let Some(f) = render.borrow().as_ref() {
                            f();
                        }
                    });
                    row.add_suffix(&remove);
                    group.add(&row);
                    rows.borrow_mut().push(row);
                }
                group.add(&add);
            }));
        }
        {
            let (ctx, patterns, render) = (ctx.clone(), patterns.clone(), render.clone());
            let id = mapping.mapping.id.clone();
            add.connect_apply(move |entry| {
                let text = entry.text().trim().to_string();
                if text.is_empty() {
                    return;
                }
                patterns.borrow_mut().push(text);
                entry.set_text("");
                let list = patterns.borrow().clone();
                ctx.send(json!({"action":"ignore","id":id,"patterns":list}), |_| {});
                if let Some(f) = render.borrow().as_ref() {
                    f();
                }
            });
        }
        if let Some(f) = render.borrow().as_ref() {
            f();
        }
    } else {
        patterns_group.add(
            &adw::ActionRow::builder()
                .title("Choose folders to sync first")
                .build(),
        );
    }
    dialog.add(&exclusions);

    // Account
    let account = adw::PreferencesPage::builder()
        .title("Account")
        .icon_name("avatar-default-symbolic")
        .build();
    let who = adw::PreferencesGroup::builder()
        .title("Proton account")
        .build();
    who.add(
        &adw::ActionRow::builder()
            .title(
                status
                    .account_email
                    .clone()
                    .unwrap_or_else(|| "Not signed in".into()),
            )
            .subtitle("Signed in through your browser. Peedrive never sees your password.")
            .use_markup(false)
            .build(),
    );
    if let Some(q) = status.quota.filter(|q| q.max > 0) {
        let row = adw::ActionRow::builder()
            .title("Storage")
            .subtitle(format!(
                "{} of {} used",
                glib::format_size(q.used),
                glib::format_size(q.max)
            ))
            .build();
        let bar = gtk::LevelBar::new();
        bar.set_value(q.used as f64 / q.max as f64);
        bar.set_valign(gtk::Align::Center);
        bar.set_size_request(140, -1);
        row.add_suffix(&bar);
        who.add(&row);
    }
    account.add(&who);
    if status.signed_in {
        let out = adw::PreferencesGroup::new();
        let sign_out = gtk::Button::with_label("Sign out");
        sign_out.add_css_class("destructive-action");
        sign_out.add_css_class("pill");
        sign_out.set_halign(gtk::Align::Center);
        out.add(&sign_out);
        account.add(&out);
        let ctx = ctx.clone();
        let dialog_ref = dialog.clone();
        sign_out.connect_clicked(move |_| {
            let alert = adw::AlertDialog::new(
                Some("Sign out of Proton Drive?"),
                Some("Sync stops and your files stay on this computer. Sign in again to continue."),
            );
            alert.add_response("cancel", "Cancel");
            alert.add_response("out", "Sign out");
            alert.set_response_appearance("out", adw::ResponseAppearance::Destructive);
            alert.set_close_response("cancel");
            let ctx = ctx.clone();
            let prefs = dialog_ref.clone();
            alert.connect_response(None, move |_, r| {
                if r == "out" {
                    let c = ctx.clone();
                    ctx.send(json!({"action":"logout"}), move |_| {
                        c.toast("Signed out. Your files are kept.")
                    });
                    prefs.close();
                }
            });
            alert.present(Some(&dialog_ref));
        });
    }
    dialog.add(&account);
    dialog.present(Some(&ctx.window));
}
