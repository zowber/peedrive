mod activity;
mod folders;
mod issues;
mod onboarding;
mod overview;
mod preferences;
mod review;
#[cfg(test)]
mod tests;
mod versions;

use super::request;
use adw::prelude::*;
use gtk::{gio, glib};
use peedrive::model::{MappingStatus, Status};
use serde_json::json;
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::mpsc, time::Duration};

const CSS: &str = "
.hero-icon { border-radius: 9999px; min-width: 64px; min-height: 64px; }
.status-icon { border-radius: 9999px; min-width: 30px; min-height: 30px; }
.tint-success { background: color-mix(in srgb, var(--success-bg-color) 22%, transparent); color: var(--success-color); }
.tint-accent { background: color-mix(in srgb, var(--accent-bg-color) 22%, transparent); color: var(--accent-color); }
.tint-warning { background: color-mix(in srgb, var(--warning-bg-color) 22%, transparent); color: var(--warning-color); }
.tint-neutral { background: color-mix(in srgb, currentColor 10%, transparent); }
.hero { padding: 22px 24px; }
.count-badge { background: var(--accent-bg-color); color: var(--accent-fg-color); border-radius: 9999px; padding: 1px 8px; font-weight: bold; font-size: smaller; }
.step { border-radius: 9999px; padding: 5px 12px; background: color-mix(in srgb, currentColor 7%, transparent); }
.step.current { background: color-mix(in srgb, var(--accent-bg-color) 22%, transparent); color: var(--accent-color); font-weight: bold; }
.account { border-radius: 10px; padding: 12px; margin: 8px; background: color-mix(in srgb, currentColor 6%, transparent); }
";

/// Shared handles every page needs.
#[derive(Clone)]
pub struct Ctx {
    pub window: adw::ApplicationWindow,
    pub overlay: adw::ToastOverlay,
    pub status: Rc<RefCell<Status>>,
    sidebar: gtk::ListBox,
}
impl Ctx {
    pub fn toast(&self, text: &str) {
        self.overlay.add_toast(adw::Toast::new(text));
    }
    /// Switch to a sidebar page by name.
    pub fn go(&self, page: &str) {
        let mut row = self.sidebar.first_child();
        while let Some(widget) = row {
            if widget.widget_name() == page
                && let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>()
            {
                self.sidebar.select_row(Some(row));
                return;
            }
            row = widget.next_sibling();
        }
    }
    pub fn mapping(&self) -> Option<MappingStatus> {
        self.status.borrow().mappings.first().cloned()
    }
    /// Absolute local path of a path inside the sync folder.
    pub fn local_path(&self, relative: &str) -> Option<PathBuf> {
        self.mapping()
            .map(|m| PathBuf::from(m.mapping.local).join(relative))
    }
    pub fn open(&self, path: &std::path::Path) {
        gtk::FileLauncher::new(Some(&gio::File::for_path(path))).launch(
            Some(&self.window),
            None::<&gio::Cancellable>,
            |_| {},
        );
    }
    pub fn show_in_folder(&self, path: &std::path::Path) {
        gtk::FileLauncher::new(Some(&gio::File::for_path(path))).open_containing_folder(
            Some(&self.window),
            None::<&gio::Cancellable>,
            |_| {},
        );
    }
    /// Send a request, toast its error, and pass a success value on.
    pub fn send(&self, value: serde_json::Value, done: impl FnOnce(serde_json::Value) + 'static) {
        let ctx = self.clone();
        request(value, move |r| match r {
            Ok(v) => done(v),
            Err(e) => ctx.toast(&e),
        });
    }
}

pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    for class in classes {
        l.add_css_class(class);
    }
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l
}
/// A round tinted icon, as used by the status card and activity rows.
pub fn status_icon(icon: &str, tint: &str, large: bool) -> gtk::Box {
    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    holder.add_css_class(if large { "hero-icon" } else { "status-icon" });
    holder.add_css_class(tint);
    holder.set_valign(gtk::Align::Center);
    holder.set_halign(gtk::Align::Center);
    holder.set_hexpand(false);
    holder.set_vexpand(false);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(if large { 30 } else { 14 });
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    image.set_vexpand(true);
    holder.append(&image);
    holder
}
/// SQLite `YYYY-MM-DD HH:MM:SS` UTC to local time.
pub fn parse_time(utc: &str) -> Option<glib::DateTime> {
    glib::DateTime::from_iso8601(&format!("{}Z", utc.replace(' ', "T")), None)
        .ok()?
        .to_local()
        .ok()
}
pub fn ago(unix: i64) -> String {
    let now = glib::DateTime::now_utc().map_or(unix, |n| n.to_unix());
    match now - unix {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} h ago", s / 3600),
        s if s < 172_800 => "yesterday".into(),
        s => format!("{} days ago", s / 86_400),
    }
}
pub fn short_time(utc: &str) -> String {
    let Some(time) = parse_time(utc) else {
        return utc.into();
    };
    ago(time.to_unix())
}
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
pub fn parent_name(path: &str) -> String {
    path.rsplit_once('/')
        .map_or("My files".into(), |(dir, _)| dir.replace('/', " / "))
}
/// Remove every child through the container's own API, which keeps its bookkeeping intact.
pub fn clear(container: &impl IsA<gtk::Widget>) {
    let container = container.as_ref();
    while let Some(child) = container.first_child() {
        if let Some(list) = container.downcast_ref::<gtk::ListBox>() {
            list.remove(&child);
        } else if let Some(b) = container.downcast_ref::<gtk::Box>() {
            b.remove(&child);
        } else {
            child.unparent();
        }
    }
}
/// A scrolling, width-clamped page body.
pub fn page_body(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(760)
        .tightening_threshold(560)
        .child(child)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build()
}
pub fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}
pub fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}
pub fn margins(widget: &impl IsA<gtk::Widget>, vertical: i32, horizontal: i32) {
    let w = widget.as_ref();
    w.set_margin_top(vertical);
    w.set_margin_bottom(vertical);
    w.set_margin_start(horizontal);
    w.set_margin_end(horizontal);
}
/// Number of things on the Issues page that need a decision.
pub fn issue_count(status: &Status) -> usize {
    status.conflicts.len()
        + status
            .mappings
            .iter()
            .filter(|m| matches!(m.state.as_str(), "Review changes" | "Needs attention"))
            .count()
}

fn nav_row(name: &str, title: &str, icon: &str) -> (gtk::ListBoxRow, gtk::Label) {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(name);
    let content = hbox(12);
    margins(&content, 9, 6);
    content.append(&gtk::Image::from_icon_name(icon));
    let text = label(title, &[]);
    text.set_hexpand(true);
    content.append(&text);
    let badge = label("", &["count-badge"]);
    badge.set_visible(false);
    content.append(&badge);
    row.set_child(Some(&content));
    (row, badge)
}

/// Set by Quit so status polling cannot restart the service through D-Bus activation.
static QUITTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn systemctl(args: &[&str]) -> bool {
    std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .is_ok_and(|s| s.success())
}

pub fn build(app: &adw::Application) {
    // The window never owns sync; start the service if nothing answers, and bring the
    // tray back if a Quit removed it.
    std::thread::spawn(|| {
        if systemctl(&["is-enabled", "--quiet", "peedrive-indicator.service"]) {
            systemctl(&["start", "peedrive-indicator.service"]);
        }
        if peedrive::service::get_status().is_err()
            && let Ok(exe) = std::env::current_exe()
        {
            let _ = std::process::Command::new(exe.with_file_name("peedrive"))
                .arg("daemon")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    });
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Peedrive")
        .default_width(1000)
        .default_height(680)
        .width_request(360)
        .height_request(480)
        .build();
    let overlay = adw::ToastOverlay::new();
    let sidebar = gtk::ListBox::new();
    sidebar.add_css_class("navigation-sidebar");
    let ctx = Ctx {
        window: window.clone(),
        overlay: overlay.clone(),
        status: Rc::new(RefCell::new(Status::default())),
        sidebar: sidebar.clone(),
    };

    // Pages
    let overview = overview::Overview::new(&ctx);
    let folders = folders::Folders::new(&ctx);
    let activity = activity::Activity::new(&ctx);
    let issues = issues::Issues::new(&ctx);
    let onboarding = onboarding::Onboarding::new(&ctx);
    let pages = gtk::Stack::new();
    pages.set_transition_type(gtk::StackTransitionType::Crossfade);
    let entries = [
        ("overview", "Overview", "go-home-symbolic"),
        ("folders", "Choose folders", "folder-symbolic"),
        ("activity", "Activity", "document-open-recent-symbolic"),
        ("issues", "Issues", "dialog-warning-symbolic"),
    ];
    let widgets: [gtk::Widget; 4] = [
        overview.widget.clone().upcast(),
        folders.widget.clone().upcast(),
        activity.widget.clone().upcast(),
        issues.widget.clone().upcast(),
    ];
    let mut issue_badge = None;
    for ((name, title, icon), widget) in entries.iter().zip(widgets) {
        pages.add_titled(&widget, Some(name), title);
        let (row, badge) = nav_row(name, title, icon);
        sidebar.append(&row);
        if *name == "issues" {
            issue_badge = Some(badge);
        }
    }
    let issue_badge = issue_badge.unwrap();

    // Sidebar
    let menu = gio::Menu::new();
    let main_section = gio::Menu::new();
    main_section.append(Some("Preferences"), Some("app.preferences"));
    main_section.append(Some("About Peedrive"), Some("app.about"));
    menu.append_section(None, &main_section);
    let quit_section = gio::Menu::new();
    quit_section.append(Some("Quit Peedrive"), Some("app.quit"));
    menu.append_section(None, &quit_section);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .primary(true)
        .tooltip_text("Main menu")
        .build();
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("Peedrive", "")));
    sidebar_header.pack_end(&menu_button);
    let account = vbox(10);
    account.add_css_class("account");
    let account_row = hbox(10);
    let avatar = adw::Avatar::new(32, None, true);
    account_row.append(&avatar);
    let account_text = vbox(0);
    let email = label("", &["heading"]);
    email.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    email.set_wrap(false);
    account_text.append(&email);
    account_text.append(&label("Proton Drive", &["caption", "dim-label"]));
    account_row.append(&account_text);
    account.append(&account_row);
    let storage = gtk::LevelBar::new();
    storage.set_visible(false);
    let storage_text = label("", &["caption", "dim-label"]);
    account.append(&storage);
    account.append(&storage_text);
    let sidebar_box = vbox(0);
    sidebar.set_vexpand(true);
    sidebar_box.append(&sidebar);
    sidebar_box.append(&account);
    let sidebar_view = adw::ToolbarView::new();
    sidebar_view.add_top_bar(&sidebar_header);
    sidebar_view.set_content(Some(&sidebar_box));

    // Content
    let content_title = adw::WindowTitle::new("Overview", "");
    let content_header = adw::HeaderBar::new();
    content_header.set_title_widget(Some(&content_title));
    let pause = gtk::Button::from_icon_name("media-playback-pause-symbolic");
    pause.set_tooltip_text(Some("Pause syncing"));
    pause.set_visible(false);
    content_header.pack_end(&pause);
    let banner = adw::Banner::new("Connecting to the Peedrive service…");
    banner.set_revealed(true);
    let content_view = adw::ToolbarView::new();
    content_view.add_top_bar(&content_header);
    content_view.add_top_bar(&banner);
    content_view.set_content(Some(&pages));

    let split = adw::NavigationSplitView::new();
    split.set_min_sidebar_width(220.0);
    split.set_max_sidebar_width(280.0);
    split.set_sidebar(Some(&adw::NavigationPage::new(&sidebar_view, "Peedrive")));
    let content_page = adw::NavigationPage::new(&content_view, "Overview");
    split.set_content(Some(&content_page));
    let breakpoint =
        adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 640sp").unwrap());
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(breakpoint);
    {
        let pages = pages.clone();
        let split = split.clone();
        let content_title = content_title.clone();
        sidebar.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let name = row.widget_name();
            pages.set_visible_child_name(&name);
            let title = pages
                .page(&pages.child_by_name(&name).unwrap())
                .title()
                .unwrap_or_default();
            content_title.set_title(&title);
            content_page.set_title(&title);
            split.set_show_content(true);
        });
    }
    sidebar.select_row(sidebar.row_at_index(0).as_ref());

    let root = gtk::Stack::new();
    root.set_transition_type(gtk::StackTransitionType::Crossfade);
    root.add_named(&onboarding.widget, Some("onboarding"));
    root.add_named(&split, Some("main"));
    overlay.set_child(Some(&root));
    window.set_content(Some(&overlay));

    // Actions
    let preferences_action = gio::SimpleAction::new("preferences", None);
    {
        let ctx = ctx.clone();
        preferences_action.connect_activate(move |_, _| preferences::show(&ctx));
    }
    app.add_action(&preferences_action);
    let about = gio::SimpleAction::new("about", None);
    {
        let window = window.clone();
        about.connect_activate(move |_, _| {
            adw::AboutDialog::builder()
                .application_name("Peedrive")
                .application_icon(peedrive::service::APP_ID)
                .version("0.1.0 alpha 9")
                .developer_name("Peedrive contributors")
                .comments("An unofficial Proton Drive client for GNOME. Files stay end-to-end encrypted in Drive and appear on this computer as ordinary files.")
                .license_type(gtk::License::MitX11)
                .build()
                .present(Some(&window));
        });
    }
    app.add_action(&about);
    let quit = gio::SimpleAction::new("quit", None);
    {
        let app = app.clone();
        // Quit stops background sync and the tray too; closing the window does not.
        quit.connect_activate(move |_, _| {
            QUITTING.store(true, std::sync::atomic::Ordering::Relaxed);
            systemctl(&["stop", "peedrive-indicator.service"]);
            let app = app.clone();
            request(json!({"action":"quit"}), move |_| app.quit());
        });
    }
    app.add_action(&quit);
    {
        let ctx = ctx.clone();
        pause.connect_clicked(move |button| {
            let Some(mapping) = ctx.mapping() else { return };
            let action = if mapping.mapping.paused {
                "resume"
            } else {
                "pause"
            };
            button.set_sensitive(false);
            let button = button.clone();
            let c = ctx.clone();
            request(json!({"action":action,"id":mapping.mapping.id}), move |r| {
                button.set_sensitive(true);
                match r {
                    Ok(_) => c.toast(if action == "pause" {
                        "Sync paused"
                    } else {
                        "Sync resumed"
                    }),
                    Err(e) => c.toast(&e),
                }
            });
        });
    }

    // Status polling. The service answers Status even while it is busy syncing.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        while !QUITTING.load(std::sync::atomic::Ordering::Relaxed)
            && tx
                .send(peedrive::service::get_status().map_err(|e| e.to_string()))
                .is_ok()
        {
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    let weak = window.downgrade();
    let mut was_setup = true;
    let poll_ctx = ctx.clone();
    glib::timeout_add_local(Duration::from_millis(200), move || {
        if weak.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        let Ok(result) = rx.try_recv() else {
            return glib::ControlFlow::Continue;
        };
        let status = match result {
            Ok(status) => status,
            Err(_) => {
                banner
                    .set_title("The Peedrive service is not running. Reopen Peedrive to start it.");
                banner.set_revealed(true);
                return glib::ControlFlow::Continue;
            }
        };
        banner.set_revealed(status.account_error.is_some() && status.signed_in);
        if let Some(error) = &status.account_error {
            banner.set_title(error);
        }
        *poll_ctx.status.borrow_mut() = status.clone();
        // First run: sign in, then pick the folder. Afterwards the main window always shows.
        let setup =
            status.sync_folder.is_none() || (!status.signed_in && status.mappings.is_empty());
        if setup {
            onboarding.update(&status);
        }
        root.set_visible_child_name(if setup { "onboarding" } else { "main" });
        if was_setup && !setup && status.mappings.is_empty() {
            poll_ctx.go("folders");
        }
        was_setup = setup;
        let email_text = status
            .account_email
            .clone()
            .unwrap_or_else(|| "Not signed in".into());
        email.set_label(&email_text);
        avatar.set_text(Some(&email_text));
        if let Some(q) = status.quota.filter(|q| q.max > 0) {
            storage.set_visible(true);
            storage.set_value(q.used as f64 / q.max as f64);
            storage_text.set_label(&format!(
                "{} of {} used",
                glib::format_size(q.used),
                glib::format_size(q.max)
            ));
        } else {
            storage.set_visible(false);
            storage_text.set_label("");
        }
        let issues_count = issue_count(&status);
        issue_badge.set_visible(issues_count > 0);
        issue_badge.set_label(&issues_count.to_string());
        if let Some(m) = status.mappings.first() {
            pause.set_visible(true);
            let paused = m.mapping.paused;
            pause.set_icon_name(if paused {
                "media-playback-start-symbolic"
            } else {
                "media-playback-pause-symbolic"
            });
            pause.set_tooltip_text(Some(if paused {
                "Resume syncing"
            } else {
                "Pause syncing"
            }));
        } else {
            pause.set_visible(false);
        }
        overview.update(&status);
        folders.update(&status);
        activity.update(&status);
        issues.update(&status);
        glib::ControlFlow::Continue
    });

    if let Ok(page) = std::env::var("PEEDRIVE_SCREENSHOT_PAGE") {
        ctx.go(&page);
    }
    window.present();
    if let Some(path) = std::env::var_os("PEEDRIVE_SCREENSHOT") {
        let window = window.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_secs(4), move || {
            screenshot(&window, std::path::Path::new(&path));
            app.quit();
        });
    }
}

pub fn screenshot(window: &adw::ApplicationWindow, path: &std::path::Path) {
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
    match (snapshot.to_node(), window.renderer()) {
        (Some(node), Some(renderer)) => {
            if let Err(e) = renderer.render_texture(&node, None).save_to_png(path) {
                eprintln!("Screenshot failed: {e}");
            }
        }
        _ => eprintln!("Screenshot failed: the window has not rendered"),
    }
}
