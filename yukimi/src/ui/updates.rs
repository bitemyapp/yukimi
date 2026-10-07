// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where the system comes from: each input of its flake, the exact version
//! it is pinned to and how old that is, whether there is a newer one, and
//! updating.
use std::collections::BTreeMap;

use adw::prelude::*;
use yukimi_config::lock::{Movement, compare};
use yukimi_system::{human_age, now};

use super::Ctx;
use super::widgets::{badge, clear, dim, heading, page};
use crate::ops::{Mode, Operation};

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

/// The buttons at the top: check, and update what has a newer version.
fn actions(ctx: &Ctx, movements: Option<&BTreeMap<String, Movement>>) -> gtk::Box {
    let buttons = gtk::Box::new(gtk::Orientation::Vertical, 8);
    buttons.set_valign(gtk::Align::Center);
    if ctx.checking() {
        let busy = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        busy.set_halign(gtk::Align::Center);
        busy.append(&adw::Spinner::new());
        busy.append(&dim("Looking for newer versions…"));
        buttons.append(&busy);
        return buttons;
    }
    let newer: Vec<String> = movements
        .map(|m| m.iter().filter(|(_, m)| matches!(m, Movement::Newer(_))).map(|(name, _)| name.clone()).collect())
        .unwrap_or_default();
    if !newer.is_empty() {
        let what = match newer.as_slice() {
            [one] => one.clone(),
            many => format!("{} sources", many.len()),
        };
        let all = gtk::Button::with_label(&format!("Update {what}"));
        all.add_css_class("pill");
        all.add_css_class("suggested-action");
        let ctx = ctx.clone();
        all.connect_clicked(move |_| update(&ctx, newer.clone(), &what));
        buttons.append(&all);
    }
    let check = gtk::Button::with_label(if movements.is_some() { "Check again" } else { "Check for updates" });
    check.add_css_class("pill");
    if movements.is_none() {
        check.add_css_class("suggested-action");
    }
    let c = ctx.clone();
    check.connect_clicked(move |_| c.check_updates());
    buttons.append(&check);
    if let Some(checked) = ctx.update_check() {
        let when = dim(&format!("Checked {}", human_age(checked.when, now())));
        when.add_css_class("caption");
        buttons.append(&when);
    }
    buttons
}

/// The time, badge and tooltip for one input, given how updating would
/// move it, or why it couldn't be checked.
fn status(row: &adw::ActionRow, source: &str, age: Option<String>, movement: Option<&Movement>, failure: Option<&str>) {
    let age = age.unwrap_or_default();
    if let Some(reason) = failure {
        row.add_suffix(&dim(&age));
        let mark = badge("couldn't check", "unchecked");
        mark.set_tooltip_text(Some(&format!("{source} couldn't be asked for its newest version: {reason}")));
        row.add_suffix(&mark);
        return;
    }
    match movement {
        Some(Movement::Newer(next)) => {
            let text = match next.last_modified {
                Some(t) => format!("{age} → {}", human_age(t, now())),
                None => age,
            };
            row.add_suffix(&dim(&text));
            let mark = badge("newer version", "newer");
            if let Some(rev) = next.short_rev() {
                mark.set_tooltip_text(Some(&format!("{source} is now at {rev}")));
            }
            row.add_suffix(&mark);
        }
        Some(Movement::Older(next)) => {
            row.add_suffix(&dim(&age));
            let mark = badge("would go back", "backwards");
            mark.set_tooltip_text(Some(&format!(
                "{source} now points to {}{}, older than the version you have. Updating would go back to it, so \
                 Yukimi doesn't.",
                next.short_rev().unwrap_or("a version"),
                next.last_modified.map(|t| format!(" from {}", human_age(t, now()))).unwrap_or_default(),
            )));
            row.add_suffix(&mark);
        }
        Some(Movement::Current) => {
            row.add_suffix(&dim(&age));
            row.add_suffix(&badge("up to date", "uptodate"));
        }
        None => row.add_suffix(&dim(&age)),
    }
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
        let check = ctx.update_check();
        let movements = check.as_ref().map(|check| compare(&model.inputs, &check.inputs));
        let failures = check.map(|check| check.failures.clone()).unwrap_or_default();
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        let intro = heading(
            "Where your system comes from",
            "Your system is built from these sources, each pinned to one exact version, so it is the same every \
             time it is built. Checking asks each source for its newest version without changing anything, and \
             updating moves to it. The version you have now stays in History.",
        );
        intro.set_hexpand(true);
        top.append(&intro);
        if !model.inputs.is_empty() {
            top.append(&actions(ctx, movements.as_ref()));
        }
        content.append(&top);

        if let Some(movements) = &movements {
            let older: Vec<&str> = movements
                .iter()
                .filter(|(_, m)| matches!(m, Movement::Older(_)))
                .map(|(name, _)| name.as_str())
                .collect();
            if !older.is_empty() {
                let banner = adw::Banner::new(&format!(
                    "Updating {} would go back to an older version, so Yukimi leaves {} as {} is",
                    older.join(" and "),
                    if older.len() == 1 { "it" } else { "them" },
                    if older.len() == 1 { "it" } else { "they" },
                ));
                banner.set_revealed(true);
                content.append(&banner);
            } else if failures.is_empty() && movements.values().all(|m| *m == Movement::Current) {
                let done = adw::StatusPage::builder()
                    .icon_name("emblem-ok-symbolic")
                    .title("Everything is up to date")
                    .description("Every source is at its newest version.")
                    .build();
                done.add_css_class("compact");
                content.append(&done);
            }
        }

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
                    let mut subtitle = source.clone();
                    if let Some(rev) = locked.short_rev() {
                        subtitle.push_str(&format!("  ·  {rev}"));
                    }
                    if !input.follows_inputs.is_empty() {
                        subtitle.push_str(&format!("  ·  uses this system's {}", input.follows_inputs.join(", ")));
                    }
                    row.set_subtitle(&subtitle);
                    let movement = movements.as_ref().and_then(|m| m.get(&input.name));
                    status(
                        &row,
                        &source,
                        locked.last_modified.map(|t| human_age(t, now())),
                        movement,
                        failures.get(&input.name).map(String::as_str),
                    );
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
                    if matches!(movement, Some(Movement::Newer(_))) {
                        let one = gtk::Button::with_label("Update");
                        one.add_css_class("flat");
                        one.set_valign(gtk::Align::Center);
                        let (ctx, name) = (ctx.clone(), input.name.clone());
                        one.connect_clicked(move |_| update(&ctx, vec![name.clone()], &name));
                        row.add_suffix(&one);
                    }
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
