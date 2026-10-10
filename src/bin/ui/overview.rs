use super::*;

/// Status-first home page: one card that says what is happening and what to do about it.
pub struct Overview {
    pub widget: gtk::ScrolledWindow,
    ctx: Ctx,
    icon_slot: gtk::Box,
    title: gtk::Label,
    detail: gtk::Label,
    primary: gtk::Button,
    open: gtk::Button,
    progress_group: adw::PreferencesGroup,
    /// One row per running transfer; the service runs up to three at once.
    progress_rows: Vec<(adw::ActionRow, gtk::ProgressBar)>,
    recent: adw::PreferencesGroup,
    recent_rows: RefCell<Vec<adw::ActionRow>>,
    recent_key: RefCell<String>,
    hero_key: RefCell<String>,
    action: Rc<RefCell<Hero>>,
}

#[derive(Clone, Default, PartialEq)]
enum Hero {
    #[default]
    None,
    SignIn,
    Choose,
    Review,
    Retry(String),
    Resume(String),
}

impl Overview {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let body = vbox(24);
        margins(&body, 24, 12);

        let card = hbox(20);
        card.add_css_class("card");
        card.add_css_class("hero");
        let icon_slot = hbox(0);
        card.append(&icon_slot);
        let text = vbox(4);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        let title = label("", &["title-2"]);
        let detail = label("", &["dim-label"]);
        text.append(&title);
        text.append(&detail);
        card.append(&text);
        let buttons = hbox(8);
        buttons.set_valign(gtk::Align::Center);
        let primary = gtk::Button::new();
        primary.add_css_class("suggested-action");
        primary.add_css_class("pill");
        let open = gtk::Button::with_label("Open folder");
        open.add_css_class("pill");
        buttons.append(&primary);
        buttons.append(&open);
        card.append(&buttons);
        body.append(&card);

        let progress_group = adw::PreferencesGroup::builder()
            .title("In progress")
            .build();
        let progress_rows: Vec<_> = (0..3)
            .map(|_| {
                let row = adw::ActionRow::new();
                row.set_use_markup(false);
                let bar = gtk::ProgressBar::new();
                bar.set_valign(gtk::Align::Center);
                bar.set_size_request(160, -1);
                row.add_suffix(&bar);
                progress_group.add(&row);
                (row, bar)
            })
            .collect();
        body.append(&progress_group);

        let recent = adw::PreferencesGroup::builder()
            .title("Recently synced")
            .build();
        let all = gtk::Button::with_label("All activity");
        all.add_css_class("flat");
        recent.set_header_suffix(Some(&all));
        body.append(&recent);

        let view = Rc::new(Self {
            widget: page_body(&body),
            ctx: ctx.clone(),
            icon_slot,
            title,
            detail,
            primary,
            open,
            progress_group,
            progress_rows,
            recent,
            recent_rows: RefCell::new(Vec::new()),
            recent_key: RefCell::new(String::new()),
            hero_key: RefCell::new(String::new()),
            action: Rc::new(RefCell::new(Hero::None)),
        });
        {
            let ctx = ctx.clone();
            all.connect_clicked(move |_| ctx.go("activity"));
        }
        {
            let ctx = ctx.clone();
            view.open.connect_clicked(move |_| {
                if let Some(path) = ctx.local_path("") {
                    ctx.open(&path);
                }
            });
        }
        {
            let ctx = ctx.clone();
            let action = view.action.clone();
            view.primary
                .connect_clicked(move |button| match action.borrow().clone() {
                    Hero::SignIn => onboarding::sign_in(&ctx, button),
                    Hero::Choose => ctx.go("folders"),
                    Hero::Review => ctx.go("issues"),
                    Hero::Retry(id) | Hero::Resume(id) => {
                        ctx.send(json!({"action":"resume","id":id}), |_| {});
                    }
                    Hero::None => {}
                });
        }
        view
    }

    pub fn update(&self, status: &Status) {
        let mapping = status.mappings.first();
        let (icon, tint, title, detail, action, button) = hero(status, mapping);
        let key = format!("{icon}{tint}{title}{detail}{button}");
        if *self.hero_key.borrow() != key {
            *self.hero_key.borrow_mut() = key;
            clear(&self.icon_slot);
            self.icon_slot.append(&status_icon(icon, tint, true));
            self.title.set_label(&title);
            self.detail.set_label(&detail);
            self.primary.set_visible(!button.is_empty());
            self.primary.set_label(button);
            *self.action.borrow_mut() = action;
        }
        self.open.set_visible(mapping.is_some());

        let syncing = mapping.is_some_and(|m| m.state == "Syncing");
        let transfers: Vec<_> = status
            .transfers
            .iter()
            .filter(|t| syncing && t.get("path").is_some())
            .collect();
        self.progress_group.set_visible(!transfers.is_empty());
        for (i, (row, bar)) in self.progress_rows.iter().enumerate() {
            row.set_visible(i < transfers.len());
            let Some(t) = transfers.get(i) else { continue };
            let path = t["path"].as_str().unwrap_or_default();
            let size = t["size"].as_u64().unwrap_or(0);
            let bytes = t["bytes"].as_u64().unwrap_or(0).min(size.max(1));
            let direction = if t["direction"] == "upload" {
                "Uploading"
            } else {
                "Downloading"
            };
            row.set_title(file_name(path));
            row.set_subtitle(&format!(
                "{direction} · {} of {}",
                glib::format_size(bytes),
                glib::format_size(size)
            ));
            bar.set_fraction(if size == 0 {
                0.0
            } else {
                bytes as f64 / size as f64
            });
        }

        let recent: Vec<_> = status
            .activity
            .iter()
            .filter(|e| matches!(e.kind.as_str(), "upload" | "download") && e.path.is_some())
            .take(6)
            .collect();
        let key = recent
            .iter()
            .map(|e| format!("{}:{}", e.id, short_time(&e.time)))
            .collect::<String>();
        if *self.recent_key.borrow() == key {
            return;
        }
        *self.recent_key.borrow_mut() = key;
        for row in self.recent_rows.borrow_mut().drain(..) {
            self.recent.remove(&row);
        }
        self.recent.set_visible(!recent.is_empty());
        for event in recent {
            let path = event.path.clone().unwrap_or_default();
            let row = adw::ActionRow::builder()
                .title(file_name(&path))
                .subtitle(parent_name(&path))
                .use_markup(false)
                .activatable(true)
                .build();
            let (icon, verb) = if event.kind == "upload" {
                ("go-up-symbolic", "Uploaded")
            } else {
                ("go-down-symbolic", "Downloaded")
            };
            row.add_prefix(&gtk::Image::from_icon_name("text-x-generic-symbolic"));
            let when = label(
                &format!("{verb} {}", short_time(&event.time)),
                &["dim-label", "caption"],
            );
            when.set_wrap(false);
            row.add_suffix(&gtk::Image::from_icon_name(icon));
            row.add_suffix(&when);
            let show = gtk::Button::from_icon_name("folder-open-symbolic");
            show.add_css_class("flat");
            show.set_valign(gtk::Align::Center);
            show.set_tooltip_text(Some("Show in folder"));
            row.add_suffix(&show);
            if let Some(full) = self.ctx.local_path(&path) {
                let ctx = self.ctx.clone();
                let target = full.clone();
                show.connect_clicked(move |_| ctx.show_in_folder(&target));
                let ctx = self.ctx.clone();
                row.connect_activated(move |_| ctx.open(&full));
            }
            self.recent.add(&row);
            self.recent_rows.borrow_mut().push(row);
        }
    }
}

type HeroView = (
    &'static str,
    &'static str,
    String,
    String,
    Hero,
    &'static str,
);

fn hero(status: &Status, mapping: Option<&MappingStatus>) -> HeroView {
    let warn = "dialog-warning-symbolic";
    if !status.signed_in {
        return (
            warn,
            "tint-warning",
            "Sign in to keep syncing".into(),
            format!(
                "Your files are safe on this computer. Sync continues after you sign in.\n{}",
                peedrive::service::THIRD_PARTY
            ),
            Hero::SignIn,
            "Sign in",
        );
    }
    let Some(m) = mapping else {
        return (
            "folder-symbolic",
            "tint-accent",
            "Choose what to sync".into(),
            "Pick the Drive folders and files to keep on this computer.".into(),
            Hero::Choose,
            "Choose folders",
        );
    };
    let id = m.mapping.id.clone();
    match m.state.as_str() {
        "Needs attention" => (
            warn,
            "tint-warning",
            "Sync stopped".into(),
            m.message.clone(),
            Hero::Retry(id),
            "Try again",
        ),
        "Review changes" => (
            warn,
            "tint-warning",
            "Changes are waiting for you".into(),
            review_summary(m),
            Hero::Review,
            "Review",
        ),
        _ if m.mapping.paused => (
            "media-playback-pause-symbolic",
            "tint-neutral",
            "Sync is paused".into(),
            if status.metered {
                "Paused on a metered connection.".into()
            } else {
                "Changes are kept and sync when you resume.".into()
            },
            Hero::Resume(id),
            "Resume",
        ),
        "Paused" if status.metered => (
            "media-playback-pause-symbolic",
            "tint-neutral",
            "Paused on a metered connection".into(),
            "Sync resumes on an unmetered network, or turn this off in Preferences.".into(),
            Hero::None,
            "",
        ),
        "Offline" => (
            "network-offline-symbolic",
            "tint-neutral",
            "Offline".into(),
            "Changes are saved locally and sync when you reconnect.".into(),
            Hero::None,
            "",
        ),
        "Signed out" => (
            warn,
            "tint-warning",
            "Sign in to keep syncing".into(),
            format!("{}\n{}", m.message, peedrive::service::THIRD_PARTY),
            Hero::SignIn,
            "Sign in",
        ),
        "Waiting to retry" => (
            "view-refresh-symbolic",
            "tint-neutral",
            "Waiting to retry".into(),
            m.message.clone(),
            Hero::None,
            "",
        ),
        "Syncing" => {
            let total = m.pending.max(1);
            (
                "view-refresh-symbolic",
                "tint-accent",
                format!("Syncing {} of {} changes", (m.done + 1).min(total), total),
                "Your files stay usable while they sync.".into(),
                Hero::None,
                "",
            )
        }
        "Checking" => (
            "view-refresh-symbolic",
            "tint-accent",
            "Checking for changes…".into(),
            "Comparing this computer with Proton Drive.".into(),
            Hero::None,
            "",
        ),
        _ => {
            let when = m
                .last_synced
                .map_or("Waiting for the first sync".into(), |t| {
                    format!("Last synced {}", ago(t))
                });
            let files = match m.files {
                1 => " · 1 file".to_string(),
                0 => String::new(),
                n => format!(" · {n} files"),
            };
            (
                "object-select-symbolic",
                "tint-success",
                "Everything is up to date".into(),
                format!("{when}{files}"),
                Hero::None,
                "",
            )
        }
    }
}
pub fn review_summary(m: &MappingStatus) -> String {
    let Some(plan) = &m.plan else {
        return "Review the planned changes before they are applied.".into();
    };
    match (plan.deletions, plan.conflicts) {
        (0, c) => format!("{c} files were edited in both places."),
        (d, 0) => format!("{d} removals are waiting for your review."),
        (d, c) => format!("{d} removals and {c} conflicts are waiting for your review."),
    }
}
