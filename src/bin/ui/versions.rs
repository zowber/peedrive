use super::*;

/// A file's versions in Proton Drive, newest first, with Restore for older ones.
pub fn show(ctx: &Ctx, id: &str, path: &str) {
    let dialog = adw::Dialog::builder()
        .title("Versions")
        .content_width(480)
        .content_height(460)
        .build();
    let body = vbox(12);
    margins(&body, 6, 24);
    body.set_margin_bottom(24);
    body.append(&label(file_name(path), &["title-3"]));
    body.append(&label(
        "Restoring makes an older version current in Proton Drive. The copy on this computer moves to Recovered files.",
        &["dim-label"],
    ));
    let spinner = adw::Spinner::new();
    spinner.set_size_request(32, 32);
    body.append(&spinner);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_valign(gtk::Align::Start);
    list.set_visible(false);
    body.append(&list);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page_body(&body)));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&ctx.window));

    let (ctx, id, path) = (ctx.clone(), id.to_string(), path.to_string());
    let request_value = json!({"action":"versions","id":id,"path":path});
    request(request_value, move |reply| {
        spinner.set_visible(false);
        let versions = match reply {
            Ok(serde_json::Value::Array(v)) => v,
            Ok(_) => Vec::new(),
            Err(e) => {
                body.append(&label(&e, &["error"]));
                return;
            }
        };
        list.set_visible(true);
        for version in versions {
            let time = version["time"].as_i64().unwrap_or(0);
            let when = glib::DateTime::from_unix_local(time)
                .ok()
                .and_then(|t| t.format("%e %B %Y, %H:%M").ok())
                .map_or(String::new(), |t| t.trim().to_string());
            let row = adw::ActionRow::builder()
                .title(&when)
                .subtitle(format!(
                    "{} · {}",
                    glib::format_size(version["size"].as_u64().unwrap_or(0)),
                    ago(time)
                ))
                .use_markup(false)
                .build();
            if version["active"] == true {
                let current = label("Current", &["dim-label"]);
                current.set_wrap(false);
                row.add_suffix(&current);
            } else {
                let restore = gtk::Button::with_label("Restore");
                restore.set_valign(gtk::Align::Center);
                let (ctx, dialog) = (ctx.clone(), dialog.clone());
                let value = json!({"action":"restore-version","id":id,"path":path,
                    "revision":version["uid"]});
                restore.connect_clicked(move |b| {
                    b.set_sensitive(false);
                    let (ctx2, dialog) = (ctx.clone(), dialog.clone());
                    ctx.send(value.clone(), move |_| {
                        dialog.close();
                        ctx2.toast("Earlier version restored");
                    });
                });
                row.add_suffix(&restore);
            }
            list.append(&row);
        }
    });
}
