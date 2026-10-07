// SPDX-License-Identifier: MIT OR Apache-2.0
//! The window: a sidebar of places and the page for each.
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use yukimi_config::lock::{FlakeLock, Input};
use yukimi_system::index::PackageIndex;

use crate::model::Model;
use crate::ops::{self, Operation};

mod discover;
mod history;
mod installed;
mod job;
mod overview;
mod storage;
mod updates;
pub mod widgets;

/// The places in the sidebar: (name, title, icon, explanation).
const PLACES: [(&str, &str, &str); 6] = [
    ("overview", "Overview", "weather-snow-symbolic"),
    ("installed", "Installed", "view-grid-symbolic"),
    ("discover", "Discover", "system-search-symbolic"),
    ("updates", "Updates", "software-update-available-symbolic"),
    ("history", "History", "document-open-recent-symbolic"),
    ("storage", "Storage", "drive-harddisk-symbolic"),
];

/// What pages share: the window, the loaded model and package index, and
/// a way to run operations and refresh afterwards.
#[derive(Clone)]
pub struct Ctx(Rc<Inner>);

/// Rebuilds one page from the current model.
type Refresh = Box<dyn Fn(&Ctx)>;

pub struct Inner {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    stack: adw::ViewStack,
    sidebar: gtk::ListBox,
    model: RefCell<Rc<Model>>,
    index: RefCell<Option<Rc<PackageIndex>>>,
    indexing: Cell<bool>,
    updates: RefCell<Option<Rc<UpdateCheck>>>,
    checking: Cell<bool>,
    loading: Cell<bool>,
    refreshers: RefCell<Vec<Refresh>>,
}

/// What checking for updates found: the system's inputs as `nix flake
/// update` would lock them, and when it looked.
pub struct UpdateCheck {
    pub when: i64,
    pub inputs: Vec<Input>,
}

/// Yukimi's cache directory, `~/.cache/yukimi`.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("yukimi"))
}

/// Where the last check for updates keeps its lock file.
fn check_file() -> Option<PathBuf> {
    Some(cache_dir()?.join("updates").join("flake.lock"))
}

fn load_check(path: &std::path::Path) -> Option<UpdateCheck> {
    let lock = FlakeLock::parse(&std::fs::read_to_string(path).ok()?).ok()?;
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let when = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(UpdateCheck { when, inputs: lock.inputs() })
}

/// Ask Nix for the newest version of every input, keeping the answer.
fn run_check(path: &std::path::Path) -> Result<UpdateCheck, String> {
    let dir = path.parent().ok_or("No cache directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let partial = dir.join("flake.lock.new");
    let _ = std::fs::remove_file(&partial);
    yukimi_system::nix::check_updates(yukimi_system::CONFIG_DIR, &partial).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, path).map_err(|e| e.to_string())?;
    load_check(path).ok_or_else(|| "The newest versions could not be read".to_owned())
}

impl Ctx {
    pub fn model(&self) -> Rc<Model> {
        self.0.model.borrow().clone()
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.0.window
    }

    pub fn index(&self) -> Option<Rc<PackageIndex>> {
        self.0.index.borrow().clone()
    }

    pub fn indexing(&self) -> bool {
        self.0.indexing.get()
    }

    /// The last check for updates, if there has been one.
    pub fn update_check(&self) -> Option<Rc<UpdateCheck>> {
        self.0.updates.borrow().clone()
    }

    pub fn checking(&self) -> bool {
        self.0.checking.get()
    }

    /// Look for newer versions of the system's inputs, in the background.
    pub fn check_updates(&self) {
        let Some(path) = check_file() else {
            return;
        };
        if self.0.checking.replace(true) {
            return;
        }
        self.refresh_all();
        let ctx = self.clone();
        glib::spawn_future_local(async move {
            let checked = gio::spawn_blocking(move || run_check(&path))
                .await
                .unwrap_or_else(|_| Err("Checking for updates stopped unexpectedly".to_owned()));
            ctx.0.checking.set(false);
            match checked {
                Ok(check) => *ctx.0.updates.borrow_mut() = Some(Rc::new(check)),
                Err(e) => ctx.toast(&format!("Couldn't check for updates: {e}")),
            }
            ctx.refresh_all();
        });
    }

    pub fn loading(&self) -> bool {
        self.0.loading.get()
    }

    /// Run `refresh` now and whenever the model or index changes.
    pub fn on_refresh(&self, refresh: impl Fn(&Ctx) + 'static) {
        refresh(self);
        self.0.refreshers.borrow_mut().push(Box::new(refresh));
    }

    fn refresh_all(&self) {
        let refreshers = self.0.refreshers.borrow();
        for refresh in refreshers.iter() {
            refresh(self);
        }
    }

    pub fn toast(&self, text: &str) {
        self.0.toasts.add_toast(adw::Toast::new(text));
    }

    /// Show a place.
    pub fn show(&self, place: &str) {
        self.0.stack.set_visible_child_name(place);
        if let Some(i) = PLACES.iter().position(|(name, _, _)| *name == place)
            && let Some(row) = self.0.sidebar.row_at_index(i as i32)
        {
            self.0.sidebar.select_row(Some(&row));
        }
    }

    /// Read everything again, away from the interface.
    pub fn reload(&self) {
        if self.0.loading.replace(true) {
            return;
        }
        self.refresh_all();
        let ctx = self.clone();
        glib::spawn_future_local(async move {
            let model = gio::spawn_blocking(Model::load).await.unwrap_or_default();
            *ctx.0.model.borrow_mut() = Rc::new(model);
            ctx.0.loading.set(false);
            ctx.refresh_all();
            ctx.ensure_index(false);
        });
    }

    /// The flake reference packages are installed from.
    pub fn nixpkgs_ref(&self) -> String {
        ops::nixpkgs_ref(&self.model().inputs)
    }

    /// Run an operation in a progress dialog, then read everything again.
    pub fn run(&self, operation: Operation) {
        job::run(self, operation);
    }

    /// Ask before an operation that changes the whole system.
    pub fn confirm(&self, heading: &str, body: &str, action: &str, operation: Operation) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_responses(&[("cancel", "Cancel"), ("go", action)]);
        dialog.set_response_appearance("go", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("go"));
        dialog.set_close_response("cancel");
        let ctx = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "go" {
                ctx.run(operation.clone());
            }
        });
        dialog.present(Some(self.window()));
    }

    /// Where the package index for the system's Nixpkgs is cached.
    fn index_cache(&self) -> Option<PathBuf> {
        let nixpkgs = self.model().nixpkgs.clone()?;
        let name = std::path::Path::new(&nixpkgs).file_name()?.to_string_lossy().into_owned();
        Some(cache_dir()?.join(format!("packages-{name}.json")))
    }

    /// Load the package index from the cache, or build it (`build` forces a
    /// rebuild; otherwise it is built only when missing).
    pub fn ensure_index(&self, build: bool) {
        if self.0.indexing.get() || (self.index().is_some() && !build) {
            return;
        }
        let (Some(cache), Some(nixpkgs)) = (self.index_cache(), self.model().nixpkgs.clone()) else {
            return;
        };
        self.0.indexing.set(true);
        self.refresh_all();
        let ctx = self.clone();
        glib::spawn_future_local(async move {
            let loaded = gio::spawn_blocking(move || load_or_build_index(&cache, &nixpkgs, build))
                .await
                .unwrap_or_else(|_| Err("The package index could not be built".to_owned()));
            ctx.0.indexing.set(false);
            match loaded {
                Ok(index) => *ctx.0.index.borrow_mut() = Some(Rc::new(index)),
                Err(e) => ctx.toast(&e),
            }
            ctx.refresh_all();
        });
    }
}

fn load_or_build_index(cache: &std::path::Path, nixpkgs: &str, build: bool) -> Result<PackageIndex, String> {
    if !build
        && let Ok(text) = std::fs::read_to_string(cache)
        && let Ok(index) = PackageIndex::parse(&text)
    {
        return Ok(index);
    }
    let expression = yukimi_system::index::expression(nixpkgs);
    let output = yukimi_system::nix::command()
        .args(["eval", "--json", "--impure", "--expr", &expression])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("Nix could not be started: {e}"))?;
    if !output.status.success() {
        let stderr = yukimi_system::log::strip_ansi(&String::from_utf8_lossy(&output.stderr));
        return Err(yukimi_system::nix::last_error(&stderr));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let index = PackageIndex::parse(&text).map_err(|e| format!("The package list could not be read: {e}"))?;
    if let Some(dir) = cache.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(cache, text.as_bytes());
    Ok(index)
}

/// The sidebar's brand: the name, its meaning, and a snowflake.
fn brand() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("brand");
    let mark = gtk::Image::from_icon_name("weather-snow-symbolic");
    mark.set_pixel_size(28);
    mark.add_css_class("brand-mark");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let title = gtk::Label::new(Some("Yukimi"));
    title.add_css_class("brand-title");
    title.set_xalign(0.0);
    let subtitle = gtk::Label::new(Some("雪見 · snow-viewing for NixOS"));
    subtitle.add_css_class("brand-subtitle");
    subtitle.set_xalign(0.0);
    text.append(&title);
    text.append(&subtitle);
    row.append(&mark);
    row.append(&text);
    row
}

pub fn build_window(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Yukimi")
        .default_width(1180)
        .default_height(800)
        .build();
    let stack = adw::ViewStack::new();
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    let sidebar = gtk::ListBox::new();
    sidebar.add_css_class("navigation-sidebar");
    let ctx = Ctx(Rc::new(Inner {
        window: window.clone(),
        toasts: toasts.clone(),
        stack: stack.clone(),
        sidebar: sidebar.clone(),
        model: RefCell::new(Rc::new(Model::default())),
        index: RefCell::new(None),
        indexing: Cell::new(false),
        updates: RefCell::new(check_file().and_then(|path| load_check(&path)).map(Rc::new)),
        checking: Cell::new(false),
        loading: Cell::new(false),
        refreshers: RefCell::new(Vec::new()),
    }));

    let pages: [(&str, gtk::Widget); 6] = [
        ("overview", overview::build(&ctx).upcast()),
        ("installed", installed::build(&ctx).upcast()),
        ("discover", discover::build(&ctx).upcast()),
        ("updates", updates::build(&ctx).upcast()),
        ("history", history::build(&ctx).upcast()),
        ("storage", storage::build(&ctx).upcast()),
    ];
    for ((name, title, icon), (_, page)) in PLACES.iter().zip(pages) {
        stack.add_titled_with_icon(&page, Some(name), title, icon);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        row.set_margin_start(6);
        row.append(&gtk::Image::from_icon_name(icon));
        let label = gtk::Label::new(Some(title));
        label.set_xalign(0.0);
        row.append(&label);
        sidebar.append(&row);
    }

    let content_title = adw::WindowTitle::new("Overview", "");
    let content_page = adw::NavigationPage::builder().title("Overview").build();
    {
        let stack = stack.clone();
        let content_title = content_title.clone();
        let content_page = content_page.clone();
        sidebar.connect_row_selected(move |_, row| {
            if let Some(row) = row
                && let Some((name, title, _)) = PLACES.get(row.index() as usize)
            {
                stack.set_visible_child_name(name);
                content_title.set_title(title);
                content_page.set_title(title);
            }
        });
    }
    sidebar.select_row(sidebar.row_at_index(0).as_ref());

    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.set_title_widget(Some(&brand()));
    let sidebar_view = adw::ToolbarView::new();
    sidebar_view.add_top_bar(&sidebar_header);
    let sidebar_scroll =
        gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&sidebar).build();
    sidebar_view.set_content(Some(&sidebar_scroll));

    let content_header = adw::HeaderBar::new();
    content_header.set_title_widget(Some(&content_title));
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some("Read everything again"));
    {
        let ctx = ctx.clone();
        refresh.connect_clicked(move |_| ctx.reload());
    }
    content_header.pack_end(&refresh);
    let content_view = adw::ToolbarView::new();
    content_view.add_top_bar(&content_header);
    content_view.set_content(Some(&toasts));
    content_page.set_child(Some(&content_view));

    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&adw::NavigationPage::new(&sidebar_view, "Yukimi")));
    split.set_content(Some(&content_page));
    split.set_min_sidebar_width(230.0);
    window.set_content(Some(&split));
    window.present();
    ctx.reload();
}
