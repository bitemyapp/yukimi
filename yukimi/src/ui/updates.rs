// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where the system comes from: each input of its flake, the exact version
//! it is pinned to and how old that is, and updating them.
use adw::prelude::*;
use yukimi_system::{human_age, now};

use super::Ctx;
use super::widgets::{badge, clear, heading, page};
use crate::ops::{Mode, Operation};

/// How an input's age feels: fresh, getting old, or stale.
fn freshness(seconds: i64) -> (&'static str, &'static str) {
    match seconds / 86400 {
        0..=14 => ("fresh", "fresh"),
        15..=60 => ("a while", "aging"),
        _ => ("stale", "stale"),
    }
}

/// Ask, then update: now, or set up for the next restart (kind to a kernel
/// update, and to work in progress).
fn update(ctx: &Ctx, inputs: Vec<String>, what: &str) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Update {what}?")),
        Some(
            "Yukimi fetches the newest versions and builds the new system, which asks for an administrator \
             password and can take a while. It can take over now, or when the computer next starts. The current \
             version stays in History, so you can always return to it.",
        ),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("boot", "At next restart"), ("switch", "Now")]);
    dialog.set_response_appearance("switch", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("switch"));
    dialog.set_close_response("cancel");
    let ctx2 = ctx.clone();
    dialog.connect_response(None, move |_, response| {
        let mode = match response {
            "switch" => Mode::Switch,
            "boot" => Mode::Boot,
            _ => return,
        };
        ctx2.run(Operation::ChangeSystem { packages: None, applications: None, update: inputs.clone(), mode });
    });
    dialog.present(Some(ctx.window()));
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
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let intro = heading(
            "Where your system comes from",
            "Your system is built from these sources, each pinned to one exact version, so it is the same every \
             time it is built. Updating moves them to their newest versions. Nothing changes until it is applied, \
             and the version you have now stays in History.",
        );
        intro.set_hexpand(true);
        top.append(&intro);
        let all = gtk::Button::with_label("Update everything");
        all.add_css_class("pill");
        all.add_css_class("suggested-action");
        all.set_valign(gtk::Align::Center);
        let updatable: Vec<String> =
            model.inputs.iter().filter(|i| i.follows.is_none()).map(|i| i.name.clone()).collect();
        all.set_sensitive(!updatable.is_empty());
        {
            let ctx = ctx.clone();
            all.connect_clicked(move |_| update(&ctx, updatable.clone(), "everything"));
        }
        top.append(&all);
        content.append(&top);

        let group = adw::PreferencesGroup::new();
        if model.inputs.is_empty() {
            let row = adw::ActionRow::new();
            row.set_title("No flake.lock in /etc/nixos");
            row.set_subtitle("This system is not configured by a flake, so there are no pinned sources to show.");
            group.add(&row);
        }
        for input in &model.inputs {
            let row = adw::ActionRow::new();
            row.set_title(&input.name);
            match (&input.locked, &input.follows) {
                (_, Some(path)) => {
                    row.set_subtitle(&format!("Follows {}", path.join("/")));
                    row.add_suffix(&badge("follows", "category"));
                }
                (Some(locked), None) => {
                    let source = input.original.as_ref().unwrap_or(locked).describe();
                    let mut subtitle = source;
                    if let Some(rev) = locked.short_rev() {
                        subtitle.push_str(&format!("  ·  {rev}"));
                    }
                    if !input.follows_inputs.is_empty() {
                        subtitle.push_str(&format!("  ·  uses this system's {}", input.follows_inputs.join(", ")));
                    }
                    row.set_subtitle(&subtitle);
                    if let Some(time) = locked.last_modified {
                        let age = now() - time;
                        let (word, class) = freshness(age);
                        let when = gtk::Label::new(Some(&human_age(time, now())));
                        when.add_css_class("dim-label");
                        row.add_suffix(&when);
                        row.add_suffix(&badge(word, class));
                    }
                    if let Some(url) = locked.web_url() {
                        let open = gtk::Button::from_icon_name("adw-external-link-symbolic");
                        open.add_css_class("flat");
                        open.set_valign(gtk::Align::Center);
                        open.set_tooltip_text(Some("See this version"));
                        let window = ctx.window().clone();
                        open.connect_clicked(move |_| {
                            gtk::UriLauncher::new(&url).launch(Some(&window), gtk::gio::Cancellable::NONE, |_| {});
                        });
                        row.add_suffix(&open);
                    }
                    let one = gtk::Button::with_label("Update");
                    one.add_css_class("flat");
                    one.set_valign(gtk::Align::Center);
                    let (ctx, name) = (ctx.clone(), input.name.clone());
                    one.connect_clicked(move |_| update(&ctx, vec![name.clone()], &name));
                    row.add_suffix(&one);
                }
                (None, None) => {}
            }
            row.add_prefix(&super::widgets::monogram(&input.name, 32));
            group.add(&row);
        }
        content.append(&group);
    });
    scroll
}

#[cfg(test)]
mod tests {
    #[test]
    fn freshness_by_age() {
        assert_eq!(super::freshness(3 * 86400).1, "fresh");
        assert_eq!(super::freshness(30 * 86400).1, "aging");
        assert_eq!(super::freshness(90 * 86400).1, "stale");
    }
}
