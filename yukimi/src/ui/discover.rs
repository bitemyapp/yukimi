// SPDX-License-Identifier: MIT OR Apache-2.0
//! Finding packages: the installer's catalog when nothing is typed, and a
//! search of all of Nixpkgs when something is. Every package can be tried
//! without installing it, installed just for you, or for everyone.
use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use yukimi_system::catalog::Application;
use yukimi_system::index::Package;

use super::Ctx;
use super::widgets::{badge, clear, dim, monogram, page, wrapping};
use crate::ops::{self, Mode, Operation};

const RESULTS: usize = 80;

/// Badges for what a package is and whether it is installed.
fn badges(ctx: &Ctx, package: &Package) -> gtk::Box {
    let model = ctx.model();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    if model.yukimi_packages.contains(&package.attr) {
        row.append(&badge("for everyone", "installed"));
    }
    if model.user_attrs().contains(package.attr.as_str()) {
        row.append(&badge("for you", "installed"));
    }
    if model.system_names().contains(package.pname.as_str()) {
        row.append(&badge("on your system", "system"));
    }
    if package.unfree {
        row.append(&badge("unfree", "unfree"));
    }
    if package.broken {
        row.append(&badge("broken", "broken"));
    }
    if package.insecure {
        row.append(&badge("insecure", "broken"));
    }
    if !package.available {
        row.append(&badge("not for this computer", "broken"));
    }
    row
}

fn install_for_everyone(ctx: &Ctx, attr: &str) {
    let model = ctx.model();
    let mut packages = model.yukimi_packages.clone();
    packages.push(attr.to_owned());
    ctx.confirm(
        &format!("Install {attr} for everyone?"),
        "Yukimi adds it to the system configuration (/etc/nixos/yukimi.nix) and builds the new system, which asks \
         for an administrator password and can take a few minutes. The current version stays in History.",
        "Install",
        Operation::ChangeSystem { packages: Some(packages), applications: None, update: vec![], mode: Mode::Switch },
    );
}

fn install_app(ctx: &Ctx, app: &Application) {
    let model = ctx.model();
    let mut applications = model.applications.clone();
    for id in std::iter::once(&app.id).chain(&app.requires) {
        if !applications.contains(id) {
            applications.push(id.clone());
        }
    }
    ctx.confirm(
        &format!("Install {}?", app.name),
        "It is installed for everyone on this computer. Yukimi changes the system configuration and builds the new \
         system, which asks for an administrator password. The current version stays in History.",
        "Install",
        Operation::ChangeSystem {
            packages: None,
            applications: Some(applications),
            update: vec![],
            mode: Mode::Switch,
        },
    );
}

/// The full story of one package, with what can be done with it.
fn details(ctx: &Ctx, package: &Package) {
    let model = ctx.model();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
    content.set_margin_top(6);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    let top = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    top.append(&monogram(&package.pname, 64));
    let names = gtk::Box::new(gtk::Orientation::Vertical, 2);
    names.set_valign(gtk::Align::Center);
    let title = gtk::Label::new(Some(&package.pname));
    title.add_css_class("title-1");
    title.set_xalign(0.0);
    let attr = dim(&format!("{}  ·  {}", package.attr, package.version));
    attr.set_xalign(0.0);
    names.append(&title);
    names.append(&attr);
    top.append(&names);
    content.append(&top);
    content.append(&badges(ctx, package));
    if !package.description.is_empty() {
        content.append(&wrapping(&package.description, &["title-4"]));
    }
    if !package.long_description.is_empty() {
        let text = package.long_description.split_whitespace().collect::<Vec<_>>().join(" ");
        content.append(&wrapping(&text, &["body"]));
    }

    let facts = adw::PreferencesGroup::new();
    let fact = |title: &str, value: &str| {
        let row = adw::ActionRow::new();
        row.set_title(title);
        row.set_subtitle(value);
        row.set_subtitle_selectable(true);
        row.add_css_class("property");
        row
    };
    if !package.main_program.is_empty() {
        facts.add(&fact("Command", &package.main_program));
    }
    if !package.license.is_empty() {
        facts.add(&fact("License", &package.license));
    }
    if !package.homepage.is_empty() {
        let row = fact("Homepage", &package.homepage);
        let open = gtk::Button::from_icon_name("adw-external-link-symbolic");
        open.add_css_class("flat");
        open.set_valign(gtk::Align::Center);
        let url = package.homepage.clone();
        let window = ctx.window().clone();
        open.connect_clicked(move |_| {
            gtk::UriLauncher::new(&url).launch(Some(&window), gtk::gio::Cancellable::NONE, |_| {});
        });
        row.add_suffix(&open);
        facts.add(&row);
    }
    content.append(&facts);

    let installable = package.available && !package.broken;
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    actions.set_halign(gtk::Align::Center);
    actions.set_margin_top(6);
    let try_it = gtk::Button::with_label("Try it");
    try_it.add_css_class("pill");
    try_it.set_tooltip_text(Some("Run it in a terminal without installing it. Nothing is changed."));
    try_it.set_sensitive(installable);
    let for_me = gtk::Button::with_label("Install for me");
    for_me.add_css_class("pill");
    for_me.set_sensitive(installable && !model.user_attrs().contains(package.attr.as_str()));
    let for_all = gtk::Button::with_label("Install for everyone");
    for_all.add_css_class("pill");
    for_all.add_css_class("suggested-action");
    let unfree_blocked = package.unfree && !model.allow_unfree;
    for_all.set_sensitive(installable && !unfree_blocked && !model.yukimi_packages.contains(&package.attr));
    if unfree_blocked {
        for_all.set_tooltip_text(Some("This system was installed without unfree software"));
    }
    actions.append(&try_it);
    actions.append(&for_me);
    actions.append(&for_all);
    content.append(&actions);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(640)
        .child(&content)
        .build();
    view.set_content(Some(&scroll));
    let dialog = adw::Dialog::builder().title(&package.pname).content_width(560).child(&view).build();

    {
        let (ctx, package) = (ctx.clone(), package.clone());
        try_it.connect_clicked(move |_| {
            let command = ops::try_command(&ctx.nixpkgs_ref(), &package.attr, &package.main_program, package.unfree);
            match ops::open_terminal(&command) {
                Ok(()) => ctx.toast(&format!("Trying {} in a terminal", package.pname)),
                Err(e) => ctx.toast(&e),
            }
        });
    }
    {
        let (ctx, package, dialog) = (ctx.clone(), package.clone(), dialog.clone());
        for_me.connect_clicked(move |_| {
            dialog.close();
            ctx.run(Operation::InstallForMe { attr: package.attr.clone(), unfree: package.unfree });
        });
    }
    {
        let (ctx, attr, dialog) = (ctx.clone(), package.attr.clone(), dialog.clone());
        for_all.connect_clicked(move |_| {
            dialog.close();
            install_for_everyone(&ctx, &attr);
        });
    }
    dialog.present(Some(ctx.window()));
}

fn result_row(ctx: &Ctx, package: &Package) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&gtk::glib::markup_escape_text(&package.pname));
    row.set_subtitle(&gtk::glib::markup_escape_text(&package.description));
    row.set_subtitle_lines(2);
    row.add_prefix(&monogram(&package.pname, 40));
    row.add_suffix(&badges(ctx, package));
    row.add_suffix(&dim(&package.version));
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row.set_activatable(true);
    let (ctx, package) = (ctx.clone(), package.clone());
    row.connect_activated(move |_| details(&ctx, &package));
    row
}

fn app_card(ctx: &Ctx, app: &Application) -> gtk::Box {
    let model = ctx.model();
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("card");
    card.add_css_class("app-card");
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    top.append(&monogram(&app.name, 44));
    let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
    names.set_valign(gtk::Align::Center);
    let name = gtk::Label::new(Some(&app.name));
    name.add_css_class("heading");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    names.append(&name);
    let category = dim(&app.category);
    category.set_xalign(0.0);
    names.append(&category);
    top.append(&names);
    card.append(&top);
    let description = wrapping(&app.description, &["dim-label"]);
    description.set_lines(3);
    description.set_ellipsize(gtk::pango::EllipsizeMode::End);
    description.set_vexpand(true);
    description.set_valign(gtk::Align::Start);
    card.append(&description);
    let installed = model.applications.contains(&app.id);
    let blocked = app.unfree && !model.allow_unfree;
    let button = gtk::Button::with_label(if installed { "Installed" } else { "Install" });
    button.add_css_class("pill");
    button.set_halign(gtk::Align::Start);
    if !installed {
        button.add_css_class("suggested-action");
    }
    button.set_sensitive(!installed && !blocked);
    if blocked {
        button.set_tooltip_text(Some("This system was installed without unfree software"));
    }
    let (ctx, app) = (ctx.clone(), app.clone());
    button.connect_clicked(move |_| install_app(&ctx, &app));
    card.append(&button);
    card
}

pub fn build(ctx: &Ctx) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search packages"));
    search.add_css_class("discover-search");
    search.set_hexpand(true);
    let status = dim("");
    let search_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    search_box.set_margin_top(24);
    search_box.set_margin_start(18);
    search_box.set_margin_end(18);
    search_box.append(&search);
    search_box.append(&status);
    let search_clamp = adw::Clamp::builder().maximum_size(720).child(&search_box).build();
    outer.append(&search_clamp);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.set_margin_top(18);
    content.set_margin_bottom(36);
    content.set_margin_start(18);
    content.set_margin_end(18);
    outer.append(&page(&content));

    let query = Rc::new(RefCell::new(String::new()));
    let render: Rc<dyn Fn(&Ctx)> = {
        let (content, status, query) = (content.clone(), status.clone(), query.clone());
        Rc::new(move |ctx: &Ctx| {
            clear(&content);
            let model = ctx.model();
            let index = ctx.index();
            status.set_text(&match (&index, ctx.indexing()) {
                (Some(index), _) => format!("{} packages in Nixpkgs", group_digits(index.len())),
                (None, true) => "Getting to know Nixpkgs… about a minute, and only the first time".to_owned(),
                (None, false) => String::new(),
            });
            let text = query.borrow().clone();
            if text.trim().is_empty() {
                if index.is_none() && !ctx.indexing() && !ctx.loading() {
                    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                    let label = wrapping("To search all of Nixpkgs, Yukimi first makes a list of its packages.", &[]);
                    label.set_hexpand(true);
                    let build = gtk::Button::with_label("Make the list");
                    build.add_css_class("pill");
                    let ctx2 = ctx.clone();
                    build.connect_clicked(move |_| ctx2.ensure_index(true));
                    row.append(&label);
                    row.append(&build);
                    content.append(&row);
                }
                if ctx.indexing() {
                    let spinner = adw::Spinner::new();
                    spinner.set_size_request(32, 32);
                    content.append(&spinner);
                }
                // The catalog, by category.
                let mut categories: Vec<&str> = Vec::new();
                for app in &model.catalog {
                    if !categories.contains(&app.category.as_str()) {
                        categories.push(&app.category);
                    }
                }
                for category in categories {
                    let heading = gtk::Label::new(Some(category));
                    heading.add_css_class("title-4");
                    heading.set_xalign(0.0);
                    content.append(&heading);
                    let flow = gtk::FlowBox::new();
                    flow.set_selection_mode(gtk::SelectionMode::None);
                    flow.set_homogeneous(true);
                    flow.set_min_children_per_line(2);
                    flow.set_max_children_per_line(4);
                    flow.set_column_spacing(12);
                    flow.set_row_spacing(12);
                    for app in model.catalog.iter().filter(|a| a.category == category) {
                        flow.append(&app_card(ctx, app));
                    }
                    content.append(&flow);
                }
                return;
            }
            let Some(index) = index else {
                content.append(&wrapping("The package list is still being made.", &["dim-label"]));
                return;
            };
            let results = index.search(&text, RESULTS);
            if results.is_empty() {
                let empty = adw::StatusPage::builder()
                    .icon_name("system-search-symbolic")
                    .title("Nothing by that name")
                    .description("Try a shorter word, or what the program does: \"image editor\", \"pdf\".")
                    .build();
                content.append(&empty);
                return;
            }
            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);
            for package in results {
                list.append(&result_row(ctx, package));
            }
            content.append(&list);
        })
    };
    {
        let render = render.clone();
        ctx.on_refresh(move |ctx| render(ctx));
    }
    {
        let ctx = ctx.clone();
        search.connect_search_changed(move |entry| {
            *query.borrow_mut() = entry.text().to_string();
            render(&ctx);
        });
    }
    outer
}

fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn digits_are_grouped() {
        assert_eq!(super::group_digits(123), "123");
        assert_eq!(super::group_digits(1234), "1,234");
        assert_eq!(super::group_digits(1234567), "1,234,567");
    }
}
