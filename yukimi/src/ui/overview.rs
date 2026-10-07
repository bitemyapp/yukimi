// SPDX-License-Identifier: MIT OR Apache-2.0
//! The first page: this computer, its packages, how fresh it is, and how
//! much room the store takes, at a glance.
use adw::prelude::*;
use yukimi_config::lock::{Movement, compare};
use yukimi_system::diff::{self, ChangeKind};
use yukimi_system::{human_age, human_size, now};

use super::Ctx;
use super::widgets::{clear, page, snowfall, wrapping};

fn greeting() -> &'static str {
    let hour = gtk::glib::DateTime::now_local().map(|t| t.hour()).unwrap_or(12);
    match hour {
        5..12 => "Good morning",
        12..18 => "Good afternoon",
        18..23 => "Good evening",
        _ => "Still up?",
    }
}

fn hero(ctx: &Ctx) -> gtk::Overlay {
    let model = ctx.model();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.add_css_class("hero");
    let hello = gtk::Label::new(Some(greeting()));
    hello.add_css_class("hero-greeting");
    hello.set_xalign(0.0);
    let name =
        gtk::Label::new(Some(if model.info.hostname.is_empty() { "This computer" } else { &model.info.hostname }));
    name.add_css_class("hero-title");
    name.set_xalign(0.0);
    let mut facts = Vec::new();
    if !model.info.nixos_version.is_empty() {
        facts.push(format!("NixOS {}", model.info.release()));
    }
    if let Some(generation) = model.current_generation() {
        facts.push(format!("generation {}, built {}", generation.number, human_age(generation.created, now())));
    }
    if !model.info.kernel.is_empty() {
        facts.push(format!("Linux {}", model.info.kernel));
    }
    let subtitle = gtk::Label::new(Some(&if ctx.loading() && facts.is_empty() {
        "Looking around…".to_owned()
    } else {
        facts.join("  ·  ")
    }));
    subtitle.add_css_class("hero-subtitle");
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    column.append(&hello);
    column.append(&name);
    column.append(&subtitle);
    if model.info.restart_needed {
        let pill = gtk::Label::new(Some("Restart to finish an update"));
        pill.add_css_class("hero-pill");
        pill.set_halign(gtk::Align::Start);
        pill.set_margin_top(8);
        column.append(&pill);
    }
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&column));
    overlay.add_overlay(&snowfall(70));
    overlay.add_css_class("hero-frame");
    overlay
}

/// A card with a big number, a line under it, and a button.
fn card(icon: &str, title: &str, big: &str, line: &str, button: (&str, &str), ctx: &Ctx) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card.add_css_class("card");
    card.add_css_class("stat-card");
    card.set_hexpand(true);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class("stat-icon");
    let label = gtk::Label::new(Some(title));
    label.add_css_class("heading");
    top.append(&image);
    top.append(&label);
    let number = gtk::Label::new(Some(big));
    number.add_css_class("stat-number");
    number.set_xalign(0.0);
    let text = wrapping(line, &["dim-label"]);
    text.set_vexpand(true);
    text.set_valign(gtk::Align::Start);
    let go = gtk::Button::with_label(button.0);
    go.add_css_class("pill");
    go.set_halign(gtk::Align::Start);
    let place = button.1.to_owned();
    let ctx = ctx.clone();
    go.connect_clicked(move |_| ctx.show(&place));
    card.append(&top);
    card.append(&number);
    card.append(&text);
    card.append(&go);
    card
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
        content.append(&hero(ctx));

        for problem in &model.problems {
            let banner = adw::Banner::new(problem);
            banner.set_revealed(true);
            content.append(&banner);
        }

        let cards = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        cards.set_homogeneous(true);
        // The cards share a height, but don't stretch to fill the window.
        cards.set_vexpand(false);
        let chosen = model.applications.len() + model.yukimi_packages.len();
        cards.append(&card(
            "view-grid-symbolic",
            "Packages",
            &(chosen + model.user_packages.len()).to_string(),
            &format!(
                "{} chosen for everyone, {} just for you, and {} more that your desktops and settings bring.",
                chosen,
                model.user_packages.len(),
                model.system_packages.len()
            ),
            ("See what's installed", "installed"),
            ctx,
        ));
        let newest = model.inputs.iter().filter(|i| i.name == "nixpkgs").find_map(|i| i.locked.as_ref()?.last_modified);
        cards.append(&card(
            "software-update-available-symbolic",
            "Freshness",
            &newest.map(|t| human_age(t, now())).unwrap_or_else(|| "—".to_owned()),
            &{
                let mut line = "since the version of Nixpkgs your system is built from was published.".to_owned();
                if let Some(check) = ctx.update_check() {
                    let newer = compare(&model.inputs, &check.inputs)
                        .values()
                        .filter(|m| matches!(m, Movement::Newer(_)))
                        .count();
                    line.push_str(&match newer {
                        0 => " Everything was up to date when last checked.".to_owned(),
                        1 => " One source has a newer version.".to_owned(),
                        n => format!(" {n} sources have newer versions."),
                    });
                }
                line
            },
            ("See updates", "updates"),
            ctx,
        ));
        let c = &model.composition;
        cards.append(&card(
            "drive-harddisk-symbolic",
            "Storage",
            &human_size(c.total),
            &if c.garbage + c.old_generations > 0 {
                format!(
                    "in the store. {} is garbage, and older generations keep {}.",
                    human_size(c.garbage),
                    human_size(c.old_generations)
                )
            } else {
                "in the store.".to_owned()
            },
            ("See storage", "storage"),
            ctx,
        ));
        content.append(&cards);

        // What the latest change brought.
        let n = model.generations.len();
        if n >= 2 {
            let (before, after) = (&model.generations[n - 2], &model.generations[n - 1]);
            let d = diff::diff(&model.graph, &model.closure_of(&before.target), &model.closure_of(&after.target));
            let group = adw::PreferencesGroup::new();
            group.set_title("Most recently");
            group.set_description(Some(&format!(
                "Generation {} replaced generation {} {}.",
                after.number,
                before.number,
                human_age(after.created, now())
            )));
            let row = adw::ActionRow::new();
            let mut parts = Vec::new();
            for (kind, word) in [
                (ChangeKind::Upgraded, "upgraded"),
                (ChangeKind::Added, "added"),
                (ChangeKind::Removed, "removed"),
                (ChangeKind::Downgraded, "downgraded"),
            ] {
                let count = d.count(kind);
                if count > 0 {
                    parts.push(format!("{count} {word}"));
                }
            }
            row.set_title(&if parts.is_empty() { "Only settings changed".to_owned() } else { parts.join(", ") });
            let names: Vec<String> = d.changes.iter().take(6).map(|c| c.name.clone()).collect();
            if !names.is_empty() {
                row.set_subtitle(&format!("{}{}", names.join(", "), if d.changes.len() > 6 { "…" } else { "" }));
            }
            row.add_prefix(&gtk::Image::from_icon_name("document-open-recent-symbolic"));
            let go = gtk::Button::from_icon_name("go-next-symbolic");
            go.add_css_class("flat");
            go.set_valign(gtk::Align::Center);
            let ctx = ctx.clone();
            go.connect_clicked(move |_| ctx.show("history"));
            row.add_suffix(&go);
            row.set_activatable_widget(Some(&go));
            group.add(&row);
            content.append(&group);
        } else if let Some(only) = model.generations.last() {
            let group = adw::PreferencesGroup::new();
            group.set_title("Most recently");
            let row = adw::ActionRow::new();
            row.set_title("The system as it was installed");
            row.set_subtitle(&format!(
                "Generation {} was built {}. Each change you make becomes a new generation, and what it changed \
                 shows up here.",
                only.number,
                human_age(only.created, now())
            ));
            row.add_prefix(&gtk::Image::from_icon_name("document-open-recent-symbolic"));
            group.add(&row);
            content.append(&group);
        }
    });
    scroll
}
