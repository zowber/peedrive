use super::*;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

/// Selective sync: tick what lives on this computer, then apply.
pub struct Folders {
    pub widget: gtk::Box,
    ctx: Ctx,
    state: RefCell<State>,
    crumbs: gtk::Box,
    search: gtk::SearchEntry,
    stack: gtk::Stack,
    pub(super) list: gtk::ListBox,
    error: adw::StatusPage,
    pub(super) summary: gtk::Label,
    discard: gtk::Button,
    pub(super) apply: gtk::Button,
}
#[derive(Default)]
struct State {
    /// (uid, name) from My files down to the open folder.
    nav: Vec<(String, String)>,
    items: Vec<Value>,
    sizes: HashMap<String, u64>,
    selected: BTreeSet<String>,
    saved: BTreeSet<String>,
    everything: bool,
    saved_key: String,
    dirty: bool,
    busy: bool,
    loaded: bool,
    searching: bool,
}
fn covered(set: &BTreeSet<String>, path: &str) -> bool {
    set.iter()
        .any(|s| path == s || path.starts_with(&format!("{s}/")))
}
fn partial(set: &BTreeSet<String>, path: &str) -> bool {
    set.iter().any(|s| s.starts_with(&format!("{path}/")))
}

impl Folders {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let widget = vbox(0);
        let top = hbox(12);
        margins(&top, 12, 24);
        let crumbs = hbox(2);
        crumbs.set_hexpand(true);
        top.append(&crumbs);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search Drive")
            .width_chars(22)
            .build();
        top.append(&search);
        widget.append(&top);

        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.set_valign(gtk::Align::Start);
        margins(&list, 6, 12);
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&page_body(&list), Some("list"));
        let spinner = adw::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        stack.add_named(&spinner, Some("loading"));
        let error = adw::StatusPage::builder()
            .icon_name("network-offline-symbolic")
            .title("Could not load Proton Drive")
            .build();
        let retry = gtk::Button::with_label("Try again");
        retry.add_css_class("pill");
        retry.set_halign(gtk::Align::Center);
        error.set_child(Some(&retry));
        stack.add_named(&error, Some("error"));
        stack.set_visible_child_name("loading");
        widget.append(&stack);

        let bar = hbox(12);
        margins(&bar, 12, 24);
        let text = vbox(2);
        text.set_hexpand(true);
        let summary = label("", &["heading"]);
        text.append(&summary);
        text.append(&label(
            "Unticked items stay in Proton Drive; their local copies move to Recovered files.",
            &["caption", "dim-label"],
        ));
        bar.append(&text);
        let discard = gtk::Button::with_label("Discard");
        discard.set_valign(gtk::Align::Center);
        let apply = gtk::Button::with_label("Apply");
        apply.add_css_class("suggested-action");
        apply.set_valign(gtk::Align::Center);
        bar.append(&discard);
        bar.append(&apply);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        widget.append(&bar);

        let view = Rc::new(Self {
            widget,
            ctx: ctx.clone(),
            state: RefCell::new(State::default()),
            crumbs,
            search,
            stack,
            list,
            error,
            summary,
            discard,
            apply,
        });
        {
            let v = view.clone();
            retry.connect_clicked(move |_| v.load());
        }
        {
            let v = view.clone();
            view.discard.connect_clicked(move |_| {
                {
                    let mut s = v.state.borrow_mut();
                    s.selected = s.saved.clone();
                    s.dirty = false;
                }
                v.render();
            });
        }
        {
            let v = view.clone();
            view.apply.connect_clicked(move |_| v.apply());
        }
        {
            let v = view.clone();
            view.search
                .connect_search_changed(move |entry| v.search(entry.text().as_str()));
        }
        view
    }

    pub fn update(self: &Rc<Self>, status: &Status) {
        if !status.signed_in {
            let mut s = self.state.borrow_mut();
            s.loaded = false;
            s.nav.clear();
            return;
        }
        let mapping = status.mappings.first();
        let key = mapping.map_or(String::new(), |m| format!("{:?}", m.mapping.selection));
        let (load, render) = {
            let mut s = self.state.borrow_mut();
            let mut render = false;
            if key != s.saved_key {
                s.saved_key = key;
                s.everything = mapping.is_some_and(|m| m.mapping.selection.is_none());
                s.saved = mapping
                    .and_then(|m| m.mapping.selection.clone())
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                if !s.dirty {
                    s.selected = s.saved.clone();
                }
                render = s.loaded;
            }
            (!s.loaded && !s.busy, render)
        };
        if load {
            self.load();
        } else if render {
            self.render();
        }
    }

    fn load(self: &Rc<Self>) {
        let uid = {
            let mut s = self.state.borrow_mut();
            if s.busy {
                return;
            }
            s.busy = true;
            s.nav.last().map(|n| n.0.clone())
        };
        self.stack.set_visible_child_name("loading");
        let v = self.clone();
        request(json!({"action":"browse","uid":uid}), move |r| {
            let mut s = v.state.borrow_mut();
            s.busy = false;
            s.loaded = true;
            match r {
                Err(e) => {
                    drop(s);
                    v.error.set_description(Some(&e));
                    v.stack.set_visible_child_name("error");
                }
                Ok(listing) => {
                    if s.nav.is_empty() {
                        s.nav.push((
                            listing["uid"].as_str().unwrap_or_default().into(),
                            "My files".into(),
                        ));
                    }
                    let prefix = s
                        .nav
                        .iter()
                        .skip(1)
                        .map(|n| format!("{}/", n.1))
                        .collect::<String>();
                    let mut items = listing["children"].as_array().cloned().unwrap_or_default();
                    for item in &mut items {
                        let path = format!("{prefix}{}", item["name"].as_str().unwrap_or_default());
                        if let Some(size) = item["size"].as_u64() {
                            s.sizes.insert(path.clone(), size);
                        }
                        item["path"] = path.into();
                    }
                    items.sort_by_key(|c| {
                        (
                            c["kind"] != "folder",
                            c["name"].as_str().unwrap_or_default().to_lowercase(),
                        )
                    });
                    s.items = items;
                    s.searching = false;
                    drop(s);
                    v.render_crumbs();
                    v.render();
                    v.stack.set_visible_child_name("list");
                }
            }
        });
    }

    fn search(self: &Rc<Self>, text: &str) {
        if text.trim().is_empty() {
            self.state.borrow_mut().loaded = false;
            self.load();
            return;
        }
        let v = self.clone();
        let query = text.to_string();
        request(json!({"action":"search","query":query}), move |r| {
            if v.search.text() != query {
                return;
            }
            match r {
                Ok(results) => {
                    {
                        let mut s = v.state.borrow_mut();
                        s.items = results.as_array().cloned().unwrap_or_default();
                        for item in &s.items.clone() {
                            if let (Some(p), Some(size)) =
                                (item["path"].as_str(), item["size"].as_u64())
                            {
                                s.sizes.insert(p.into(), size);
                            }
                        }
                        s.searching = true;
                    }
                    v.render();
                    v.stack.set_visible_child_name("list");
                }
                Err(e) => v.ctx.toast(&e),
            }
        });
    }

    fn render_crumbs(self: &Rc<Self>) {
        clear(&self.crumbs);
        let nav = self.state.borrow().nav.clone();
        for (i, (_, name)) in nav.iter().enumerate() {
            if i > 0 {
                let sep = gtk::Image::from_icon_name("go-next-symbolic");
                sep.add_css_class("dim-label");
                self.crumbs.append(&sep);
            }
            let crumb = gtk::Button::with_label(name);
            crumb.add_css_class("flat");
            if i + 1 == nav.len() {
                crumb.add_css_class("heading");
            }
            let v = self.clone();
            crumb.connect_clicked(move |_| {
                v.search.set_text("");
                {
                    let mut s = v.state.borrow_mut();
                    s.nav.truncate(i + 1);
                    s.loaded = false;
                }
                v.load();
            });
            self.crumbs.append(&crumb);
        }
    }

    fn render(self: &Rc<Self>) {
        clear(&self.list);
        let items = self.state.borrow().items.clone();
        if items.is_empty() {
            let empty = adw::ActionRow::builder()
                .title(if self.state.borrow().searching {
                    "No matches"
                } else {
                    "This folder is empty"
                })
                .build();
            self.list.append(&empty);
        }
        for item in items {
            self.list.append(&self.row(&item));
        }
        self.summarise();
    }

    fn row(self: &Rc<Self>, item: &Value) -> adw::ActionRow {
        let s = self.state.borrow();
        let name = item["name"].as_str().unwrap_or("Unknown item").to_string();
        let path = item["path"].as_str().unwrap_or(&name).to_string();
        let folder = item["kind"] == "folder";
        let document = item["document"].as_bool().unwrap_or(false);
        let supported = item["supported"].as_bool().unwrap_or(true);
        let inherited = s.everything
            || s.selected
                .iter()
                .any(|p| path.starts_with(&format!("{p}/")));
        let on = s.everything || covered(&s.selected, &path);
        let was = s.everything || covered(&s.saved, &path);
        let row = adw::ActionRow::builder()
            .title(&name)
            .use_markup(false)
            .activatable(folder && !s.searching)
            .build();
        let note = if document {
            "Proton Docs stay online"
        } else if !supported {
            "This item can't be synced"
        } else if on && was {
            "On this computer"
        } else if on {
            "Will download when you apply"
        } else if was {
            "Will leave this computer when you apply"
        } else if partial(&s.selected, &path) {
            "Some items inside sync"
        } else {
            "Only in Proton Drive"
        };
        let subtitle = if s.searching {
            format!("{} · {note}", parent_name(&path))
        } else {
            note.into()
        };
        row.set_subtitle(&subtitle);
        let check = gtk::CheckButton::new();
        check.set_active(on);
        check.set_inconsistent(!on && partial(&s.selected, &path));
        check.set_sensitive(supported && !document && !inherited);
        check.set_valign(gtk::Align::Center);
        check.update_property(&[gtk::accessible::Property::Label(&format!("Sync {name}"))]);
        if inherited && !s.everything {
            check.set_tooltip_text(Some(
                "Included with its parent folder. Untick the parent to choose items inside it.",
            ));
        }
        row.add_prefix(&check);
        let icon = if folder {
            gtk::Image::from_icon_name("folder-symbolic")
        } else {
            let (mime, _) = gio::content_type_guess(Some(std::path::Path::new(&name)), None);
            gtk::Image::from_gicon(&gio::content_type_get_symbolic_icon(&mime))
        };
        row.add_prefix(&icon);
        if let Some(size) = item["size"].as_u64().filter(|_| !document) {
            let size = label(&glib::format_size(size), &["dim-label", "caption"]);
            size.set_wrap(false);
            row.add_suffix(&size);
        }
        if folder && !s.searching {
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let v = self.clone();
            let uid = item["uid"].as_str().unwrap_or_default().to_string();
            let name = name.clone();
            row.connect_activated(move |_| {
                {
                    let mut s = v.state.borrow_mut();
                    s.nav.push((uid.clone(), name.clone()));
                    s.loaded = false;
                }
                v.load();
            });
        }
        drop(s);
        let v = self.clone();
        check.connect_toggled(move |check| {
            {
                let mut s = v.state.borrow_mut();
                s.dirty = true;
                s.selected.retain(|p| !p.starts_with(&format!("{path}/")));
                if check.is_active() {
                    s.selected.insert(path.clone());
                } else {
                    s.selected.remove(&path);
                }
            }
            // Re-render on idle: toggling rebuilds the row that owns this button.
            let v = v.clone();
            glib::idle_add_local_once(move || v.render());
        });
        row
    }

    fn summarise(&self) {
        let s = self.state.borrow();
        let added: Vec<_> = s
            .selected
            .iter()
            .filter(|p| !s.everything && !covered(&s.saved, p))
            .collect();
        let removed: Vec<_> = s
            .saved
            .iter()
            .filter(|p| !covered(&s.selected, p))
            .collect();
        let changes = added.len() + removed.len();
        let download: u64 = added.iter().filter_map(|p| s.sizes.get(*p)).sum();
        let mut text = match changes {
            0 if s.saved.is_empty() && !s.everything => "Nothing is syncing yet".to_string(),
            0 => "No changes".to_string(),
            1 => "1 change".to_string(),
            n => format!("{n} changes"),
        };
        if download > 0 {
            text.push_str(&format!(" · {} will download", glib::format_size(download)));
        }
        if !removed.is_empty() {
            text.push_str(&format!(" · {} will leave this computer", removed.len()));
        }
        self.summary.set_label(&text);
        self.apply.set_sensitive(changes > 0 && !s.busy);
        self.discard.set_sensitive(changes > 0 && !s.busy);
    }

    fn apply(self: &Rc<Self>) {
        let paths: Vec<String> = self.state.borrow().selected.iter().cloned().collect();
        self.state.borrow_mut().busy = true;
        self.apply.set_label("Applying…");
        self.summarise();
        let v = self.clone();
        request(json!({"action":"select-drive","paths":paths}), move |r| {
            v.apply.set_label("Apply");
            {
                let mut s = v.state.borrow_mut();
                s.busy = false;
                if s.selected.iter().eq(paths.iter()) {
                    s.dirty = false;
                }
            }
            v.summarise();
            match r {
                Err(e) => v.ctx.toast(&e),
                Ok(reply) => {
                    let id = reply["mapping"]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let result = &reply["result"];
                    if result.get("operations").is_some() {
                        if let Ok(plan) = serde_json::from_value(result.clone()) {
                            review::show(&v.ctx, &id, &plan);
                        }
                    } else if let Some(error) = result["error"].as_str() {
                        v.ctx.toast(&format!("Saved. {error}"));
                    } else {
                        v.ctx.toast("Your selection is applied");
                    }
                }
            }
        });
    }
}
