// SPDX-License-Identifier: MIT OR Apache-2.0
//! The window: a sidebar of places and the page for each.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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

/// What checking for updates found: each input as `nix flake update` would
/// lock it, why any couldn't be checked, and when it looked.
pub struct UpdateCheck {
    pub when: i64,
    pub inputs: Vec<Input>,
    pub failures: BTreeMap<String, String>,
}

/// Yukimi's cache directory, `~/.cache/yukimi`.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("yukimi"))
}

/// Where the last check for updates keeps its answers: a lock file for each
/// input it checked (`<input>.lock`) and why others couldn't be checked.
fn check_dir() -> Option<PathBuf> {
    Some(cache_dir()?.join("updates"))
}

const FAILURES: &str = "failures.json";

fn load_check(dir: &Path) -> Option<UpdateCheck> {
    let failures_file = dir.join(FAILURES);
    let failures: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(&failures_file).ok()?).ok()?;
    let modified = std::fs::metadata(&failures_file).ok()?.modified().ok()?;
    let when = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    let mut inputs = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".lock")) else {
            continue;
        };
        let lock = std::fs::read_to_string(&path).ok().and_then(|text| FlakeLock::parse(&text).ok());
        if let Some(input) = lock.and_then(|lock| lock.input(name)) {
            inputs.push(input);
        }
    }
    Some(UpdateCheck { when, inputs, failures })
}

/// The gist of a Nix error: its first line, without the "error:" label
/// (`unable to download 'https://…/commits/stable': HTTP error 422`).
fn first_line(error: &str) -> String {
    error
        .lines()
        .map(|line| line.trim().trim_start_matches("error:").trim())
        .find(|line| !line.is_empty())
        .unwrap_or("unknown error")
        .to_owned()
}

/// Ask Nix for the newest version of each input, keeping the answers.
fn run_check(dir: &Path, names: &[String]) -> Result<UpdateCheck, String> {
    let partial = dir.with_extension("new");
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    let failures: BTreeMap<String, String> =
        yukimi_system::nix::check_updates(yukimi_system::CONFIG_DIR, names, &partial)
            .into_iter()
            .filter_map(|(name, result)| result.err().map(|e| (name, first_line(&e.to_string()))))
            .collect();
    // Nothing answered (offline, say): keep the last check.
    if !names.is_empty() && failures.len() == names.len() {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(failures.into_values().next().unwrap_or_default());
    }
    let json = serde_json::to_string_pretty(&failures).map_err(|e| e.to_string())?;
    std::fs::write(partial.join(FAILURES), json).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::rename(&partial, dir).map_err(|e| e.to_string())?;
    load_check(dir).ok_or_else(|| "The newest versions could not be read".to_owned())
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
        let Some(dir) = check_dir() else {
            return;
        };
        let names: Vec<String> = self
            .model()
            .inputs
            .iter()
            .filter(|input| input.follows.is_none() && input.locked.is_some())
            .map(|input| input.name.clone())
            .collect();
        if self.0.checking.replace(true) {
            return;
        }
        self.refresh_all();
        let ctx = self.clone();
        glib::spawn_future_local(async move {
            let checked = gio::spawn_blocking(move || run_check(&dir, &names))
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
        updates: RefCell::new(check_dir().and_then(|dir| load_check(&dir)).map(Rc::new)),
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
