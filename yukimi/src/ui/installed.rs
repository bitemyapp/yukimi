// SPDX-License-Identifier: MIT OR Apache-2.0
//! What is installed, grouped by why: chosen from the catalog, added for
//! everyone, installed by a user for themselves, or part of the system
//! because a desktop or setting brings it.
use adw::prelude::*;
use yukimi_system::human_size;

use super::Ctx;
use super::widgets::{badge, clear, dim, monogram, page};
use crate::ops::{Mode, Operation};

fn remove_button(tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name("user-trash-symbolic");
    button.add_css_class("flat");
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some(tooltip));
    button
}

fn empty_row(text: &str) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(text);
    row.add_css_class("dim-row");
    row
}

pub fn build(ctx: &Ctx) -> gtk::ScrolledWindow {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
    content.set_margin_top(24);
    content.set_margin_bottom(36);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let scroll = page(&content);
    ctx.on_refresh(move |ctx| {
        clear(&content);
        let model = ctx.model();
        let index = ctx.index();

        // Chosen from the installer's catalog.
        let apps = adw::PreferencesGroup::new();
        apps.set_title("Apps you chose");
        apps.set_description(Some("From the installer's catalog, for everyone on this computer."));
        let browse = gtk::Button::with_label("Browse apps");
        browse.add_css_class("flat");
        {
            let ctx = ctx.clone();
            browse.connect_clicked(move |_| ctx.show("discover"));
        }
        apps.set_header_suffix(Some(&browse));
        if model.applications.is_empty() {
            apps.add(&empty_row("None yet"));
        }
        for id in &model.applications {
            let app = model.application(id);
            let row = adw::ActionRow::new();
            row.set_title(app.map_or(id.as_str(), |a| a.name.as_str()));
            if let Some(app) = app {
                row.set_subtitle(&app.description);
                row.add_suffix(&badge(&app.category, "category"));
            }
            row.add_prefix(&monogram(app.map_or(id.as_str(), |a| a.name.as_str()), 36));
            let remove = remove_button("Remove for everyone");
            let (ctx2, id2, name) = (ctx.clone(), id.clone(), app.map_or(id.clone(), |a| a.name.clone()));
            remove.connect_clicked(move |_| {
                let model = ctx2.model();
                let applications: Vec<String> = model.applications.iter().filter(|a| **a != id2).cloned().collect();
                ctx2.confirm(
                    &format!("Remove {name}?"),
                    "It is removed for everyone on this computer. Yukimi changes the system configuration and \
                     rebuilds, which asks for an administrator password. The current version stays in History.",
                    "Remove",
                    Operation::ChangeSystem {
                        packages: None,
                        applications: Some(applications),
                        update: vec![],
                        mode: Mode::Switch,
                    },
                );
            });
            row.add_suffix(&remove);
            apps.add(&row);
        }
        content.append(&apps);

        // Added for everyone with Yukimi.
        let everyone = adw::PreferencesGroup::new();
        everyone.set_title("Added for everyone");
        everyone.set_description(Some("Packages from Nixpkgs added with Yukimi, kept in /etc/nixos/yukimi.nix."));
        if model.yukimi_packages.is_empty() {
            everyone.add(&empty_row("None yet: find packages in Discover"));
        }
        for attr in &model.yukimi_packages {
            let row = adw::ActionRow::new();
            row.set_title(attr);
            if let Some(p) = index.as_ref().and_then(|i| i.get(attr)) {
                row.set_subtitle(&p.description);
                row.add_suffix(&dim(&p.version));
            }
            row.add_prefix(&monogram(attr, 36));
            let remove = remove_button("Remove for everyone");
            let (ctx2, attr2) = (ctx.clone(), attr.clone());
            remove.connect_clicked(move |_| {
                let model = ctx2.model();
                let packages: Vec<String> = model.yukimi_packages.iter().filter(|p| **p != attr2).cloned().collect();
                ctx2.confirm(
                    &format!("Remove {attr2}?"),
                    "It is removed for everyone on this computer, after an administrator password. The current \
                     version stays in History.",
                    "Remove",
                    Operation::ChangeSystem {
                        packages: Some(packages),
                        applications: None,
                        update: vec![],
                        mode: Mode::Switch,
                    },
                );
            });
            row.add_suffix(&remove);
            everyone.add(&row);
        }
        content.append(&everyone);

        // The user's own profile.
        let mine = adw::PreferencesGroup::new();
        mine.set_title("Just for you");
        mine.set_description(Some("Installed into your own profile with nix profile. No password needed."));
        if model.user_packages.is_empty() {
            mine.add(&empty_row("None yet"));
        }
        for element in &model.user_packages {
            let row = adw::ActionRow::new();
            let title = element.package_attr().unwrap_or(&element.name).to_owned();
            row.set_title(&title);
            if let Some(path) = element.store_paths.first() {
                row.add_suffix(&dim(path.version()));
            }
            if let Some(p) = index.as_ref().and_then(|i| i.get(&title)) {
                row.set_subtitle(&p.description);
            } else if let Some(url) = &element.original_url {
                row.set_subtitle(url);
            }
            row.add_prefix(&monogram(&title, 36));
            let remove = remove_button("Remove from your profile");
            let (ctx2, name) = (ctx.clone(), element.name.clone());
            remove.connect_clicked(move |_| ctx2.run(Operation::RemoveForMe { name: name.clone() }));
            row.add_suffix(&remove);
            mine.add(&row);
        }
        content.append(&mine);

        // Everything else the system has.
        let system = adw::PreferencesGroup::new();
        system.set_title("Part of the system");
        system.set_description(Some(&format!(
            "{} packages your desktops and settings bring. They come and go with those settings.",
            model.system_packages.len()
        )));
        let filter = gtk::SearchEntry::new();
        filter.set_placeholder_text(Some("Filter"));
        filter.set_valign(gtk::Align::Center);
        system.set_header_suffix(Some(&filter));
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        for package in &model.system_packages {
            let row = adw::ActionRow::new();
            row.set_title(&package.name);
            if !package.version.is_empty() {
                row.set_subtitle(&package.version);
            }
            row.add_suffix(&dim(&human_size(package.size)));
            list.append(&row);
        }
        {
            let list = list.clone();
            filter.connect_search_changed(move |entry| {
                let text = entry.text().to_lowercase();
                let mut child = list.first_child();
                while let Some(widget) = child {
                    if let Some(row) = widget.downcast_ref::<adw::ActionRow>() {
                        row.set_visible(text.is_empty() || row.title().to_lowercase().contains(&text));
                    }
                    child = widget.next_sibling();
                }
            });
        }
        system.add(&list);
        content.append(&system);
    });
    scroll
}
