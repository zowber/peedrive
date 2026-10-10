use super::*;
use peedrive::model::Event;

const FILTERS: [(&str, &[&str]); 5] = [
    ("All", &[]),
    ("Uploads", &["upload"]),
    ("Downloads", &["download"]),
    ("Removed", &["removed_local", "removed_remote"]),
    ("Problems", &["problem", "conflict"]),
];

/// Day-grouped history of what changed, with filters.
pub struct Activity {
    pub widget: gtk::ScrolledWindow,
    ctx: Ctx,
    days: gtk::Box,
    filter: RefCell<usize>,
    key: RefCell<String>,
    events: RefCell<Vec<Event>>,
}

impl Activity {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let body = vbox(18);
        margins(&body, 18, 12);
        let filters = hbox(0);
        filters.add_css_class("linked");
        filters.set_halign(gtk::Align::Start);
        body.append(&filters);
        let days = vbox(24);
        body.append(&days);
        let view = Rc::new(Self {
            widget: page_body(&body),
            ctx: ctx.clone(),
            days,
            filter: RefCell::new(0),
            key: RefCell::new(String::new()),
            events: RefCell::new(Vec::new()),
        });
        let mut group: Option<gtk::ToggleButton> = None;
        for (i, (name, _)) in FILTERS.iter().enumerate() {
            let button = gtk::ToggleButton::with_label(name);
            button.set_active(i == 0);
            if let Some(first) = &group {
                button.set_group(Some(first));
            } else {
                group = Some(button.clone());
            }
            let v = view.clone();
            button.connect_toggled(move |b| {
                if b.is_active() {
                    *v.filter.borrow_mut() = i;
                    v.key.borrow_mut().clear();
                    v.render();
                }
            });
            filters.append(&button);
        }
        view
    }

    pub fn update(self: &Rc<Self>, status: &Status) {
        *self.events.borrow_mut() = status.activity.clone();
        self.render();
    }

    fn render(self: &Rc<Self>) {
        let kinds = FILTERS[*self.filter.borrow()].1;
        let events: Vec<Event> = self
            .events
            .borrow()
            .iter()
            .filter(|e| kinds.is_empty() || kinds.contains(&e.kind.as_str()))
            .take(150)
            .cloned()
            .collect();
        let today = glib::DateTime::now_local().ok();
        let key = format!(
            "{}:{}:{}",
            *self.filter.borrow(),
            events.first().map_or(0, |e| e.id),
            today.as_ref().map_or(0, |t| t.day_of_year())
        );
        if *self.key.borrow() == key {
            return;
        }
        *self.key.borrow_mut() = key;
        clear(&self.days);
        if events.is_empty() {
            self.days.append(
                &adw::StatusPage::builder()
                    .icon_name("document-open-recent-symbolic")
                    .title("Nothing here yet")
                    .description("Uploads, downloads and other changes will appear here.")
                    .build(),
            );
            return;
        }
        let mut group: Option<(String, adw::PreferencesGroup)> = None;
        for event in events {
            let time = parse_time(&event.time);
            let day = time
                .as_ref()
                .map_or(event.time.clone(), |t| day_label(t, today.as_ref()));
            if group.as_ref().is_none_or(|(d, _)| *d != day) {
                let g = adw::PreferencesGroup::builder().title(&day).build();
                self.days.append(&g);
                group = Some((day, g));
            }
            let (icon, tint) = match event.kind.as_str() {
                "upload" => ("go-up-symbolic", "tint-accent"),
                "download" => ("go-down-symbolic", "tint-success"),
                "folder" => ("folder-new-symbolic", "tint-neutral"),
                "moved" => ("send-to-symbolic", "tint-neutral"),
                "restored" => ("document-revert-symbolic", "tint-accent"),
                "conflict" => ("edit-copy-symbolic", "tint-warning"),
                "removed_local" | "removed_remote" => ("user-trash-symbolic", "tint-neutral"),
                "problem" => ("dialog-warning-symbolic", "tint-warning"),
                _ => ("dialog-information-symbolic", "tint-neutral"),
            };
            // The folder is the subtitle, so the title names just the file.
            let title = event.path.as_deref().map_or(event.message.clone(), |p| {
                event.message.replace(p, file_name(p))
            });
            let row = adw::ActionRow::builder()
                .title(&title)
                .title_lines(2)
                .use_markup(false)
                .build();
            if let Some(path) = &event.path {
                row.set_subtitle(&parent_name(path));
            }
            row.add_prefix(&status_icon(icon, tint, false));
            let when = label(
                &time
                    .as_ref()
                    .and_then(|t| t.format("%H:%M").ok())
                    .map_or(String::new(), |t| t.to_string()),
                &["dim-label", "caption", "numeric"],
            );
            when.set_wrap(false);
            row.add_suffix(&when);
            if let Some(path) = event.path.as_deref().and_then(|p| self.ctx.local_path(p))
                && matches!(
                    event.kind.as_str(),
                    "upload" | "download" | "conflict" | "moved" | "restored"
                )
            {
                let show = gtk::Button::from_icon_name("folder-open-symbolic");
                show.add_css_class("flat");
                show.set_tooltip_text(Some("Show in folder"));
                show.set_valign(gtk::Align::Center);
                let ctx = self.ctx.clone();
                let shown = path.clone();
                show.connect_clicked(move |_| ctx.show_in_folder(&shown));
                row.add_suffix(&show);
                if path.is_file()
                    && let (Some(m), Some(relative)) = (self.ctx.mapping(), event.path.clone())
                {
                    let versions = gtk::Button::from_icon_name("document-open-recent-symbolic");
                    versions.add_css_class("flat");
                    versions.set_tooltip_text(Some("Versions"));
                    versions.set_valign(gtk::Align::Center);
                    let ctx = self.ctx.clone();
                    versions
                        .connect_clicked(move |_| versions::show(&ctx, &m.mapping.id, &relative));
                    row.add_suffix(&versions);
                }
            }
            if let Some((_, g)) = &group {
                g.add(&row);
            }
        }
    }
}

fn day_label(time: &glib::DateTime, today: Option<&glib::DateTime>) -> String {
    let same = |a: &glib::DateTime, b: &glib::DateTime| a.ymd() == b.ymd();
    if let Some(today) = today {
        if same(time, today) {
            return "Today".into();
        }
        if today.add_days(-1).is_ok_and(|y| same(time, &y)) {
            return "Yesterday".into();
        }
    }
    time.format("%A %e %B")
        .map_or(String::new(), |s| s.to_string())
}
