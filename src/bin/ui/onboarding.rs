use super::*;

/// First run: sign in, then choose where the files go. Choosing what to sync happens in
/// the main window.
pub struct Onboarding {
    pub widget: gtk::Box,
    steps: Vec<gtk::Label>,
    stack: gtk::Stack,
    waiting: gtk::Label,
}

pub fn sign_in(ctx: &Ctx, button: &gtk::Button) {
    button.set_sensitive(false);
    let button = button.clone();
    let c = ctx.clone();
    request(json!({"action":"login"}), move |r| {
        button.set_sensitive(true);
        match r {
            Ok(v) => {
                if let Some(url) = v["login_url"].as_str() {
                    gtk::UriLauncher::new(url).launch(
                        Some(&c.window),
                        None::<&gio::Cancellable>,
                        |_| {},
                    );
                    c.toast("Finish signing in in your browser");
                }
            }
            Err(e) => c.toast(&e),
        }
    });
}

impl Onboarding {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let header = adw::HeaderBar::new();
        header.set_show_title(false);
        let widget = vbox(0);
        widget.append(&header);
        let body = vbox(28);
        body.set_valign(gtk::Align::Center);
        body.set_vexpand(true);
        margins(&body, 12, 32);
        body.set_margin_bottom(48);

        let step_row = hbox(8);
        step_row.set_halign(gtk::Align::Center);
        let steps: Vec<_> = ["1  Sign in", "2  Sync folder", "3  Choose what to sync"]
            .iter()
            .map(|t| {
                let l = label(t, &["step", "caption"]);
                l.set_wrap(false);
                step_row.append(&l);
                l
            })
            .collect();
        body.append(&step_row);
        let icon = gtk::Image::from_icon_name(peedrive::service::APP_ID);
        icon.set_pixel_size(128);
        body.append(&icon);

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::SlideLeft);
        stack.set_vhomogeneous(false);

        // Sign in
        let signin = vbox(14);
        let title = label("Your Proton Drive, on this computer", &["title-1"]);
        title.set_xalign(0.5);
        title.set_justify(gtk::Justification::Center);
        signin.append(&title);
        let text = label(
            "Files stay end-to-end encrypted in Drive and appear here as ordinary files. Choose exactly which folders to keep in sync.",
            &["body"],
        );
        text.set_xalign(0.5);
        text.set_justify(gtk::Justification::Center);
        text.set_max_width_chars(56);
        text.set_halign(gtk::Align::Center);
        signin.append(&text);
        let button = gtk::Button::with_label("Continue in your browser");
        button.add_css_class("suggested-action");
        button.add_css_class("pill");
        button.set_halign(gtk::Align::Center);
        button.set_margin_top(10);
        signin.append(&button);
        let waiting = label(
            "Opens your browser. Peedrive never sees your password.",
            &["caption", "dim-label"],
        );
        waiting.set_xalign(0.5);
        signin.append(&waiting);
        stack.add_named(&signin, Some("signin"));

        // Folder
        let folder = vbox(14);
        let ftitle = label("Where should your files go?", &["title-1"]);
        ftitle.set_xalign(0.5);
        folder.append(&ftitle);
        let ftext = label(
            "Peedrive creates this folder if it doesn’t exist. You can move it later in Preferences.",
            &[],
        );
        ftext.set_xalign(0.5);
        ftext.set_justify(gtk::Justification::Center);
        folder.append(&ftext);
        let entry_row = hbox(8);
        entry_row.set_halign(gtk::Align::Center);
        entry_row.set_margin_top(10);
        let entry = gtk::Entry::new();
        entry.set_text("~/Proton Drive");
        entry.set_width_chars(34);
        entry.update_property(&[gtk::accessible::Property::Label("Sync folder")]);
        let choose = gtk::Button::with_label("Choose…");
        entry_row.append(&entry);
        entry_row.append(&choose);
        folder.append(&entry_row);
        let next = gtk::Button::with_label("Continue");
        next.add_css_class("suggested-action");
        next.add_css_class("pill");
        next.set_halign(gtk::Align::Center);
        next.set_margin_top(10);
        folder.append(&next);
        stack.add_named(&folder, Some("folder"));

        body.append(&stack);
        widget.append(&body);
        let footer = label(
            peedrive::service::THIRD_PARTY,
            &["caption", "dim-label"],
        );
        footer.set_xalign(0.5);
        footer.set_margin_bottom(18);
        widget.append(&footer);

        {
            let ctx = ctx.clone();
            button.connect_clicked(move |b| sign_in(&ctx, b));
        }
        {
            let window = ctx.window.clone();
            let entry = entry.clone();
            choose.connect_clicked(move |_| {
                let entry = entry.clone();
                gtk::FileDialog::builder()
                    .title("Choose your Proton Drive folder")
                    .build()
                    .select_folder(Some(&window), None::<&gio::Cancellable>, move |r| {
                        if let Ok(file) = r
                            && let Some(path) = file.path()
                        {
                            entry.set_text(&path.to_string_lossy());
                        }
                    });
            });
        }
        {
            let ctx = ctx.clone();
            next.connect_clicked(move |b| {
                b.set_sensitive(false);
                let b = b.clone();
                let c = ctx.clone();
                request(
                    json!({"action":"setup","folder":entry.text().as_str()}),
                    move |r| {
                        b.set_sensitive(true);
                        if let Err(e) = r {
                            c.toast(&e);
                        }
                    },
                );
            });
        }
        Rc::new(Self {
            widget,
            steps,
            stack,
            waiting,
        })
    }

    pub fn update(&self, status: &Status) {
        let step = usize::from(status.signed_in);
        for (i, l) in self.steps.iter().enumerate() {
            if i == step {
                l.add_css_class("current");
            } else {
                l.remove_css_class("current");
            }
        }
        self.stack
            .set_visible_child_name(if status.signed_in { "folder" } else { "signin" });
        self.waiting.set_label(if status.login_url.is_some() {
            "Waiting for you to finish in the browser…"
        } else if let Some(error) = &status.account_error {
            error
        } else {
            "Opens your browser. Peedrive never sees your password."
        });
    }
}
