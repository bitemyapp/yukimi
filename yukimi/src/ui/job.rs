// SPDX-License-Identifier: MIT OR Apache-2.0
//! Running an operation: a dialog with what Nix is doing right now, how far
//! along it is, and its log, which closes itself when everything worked and
//! stays with the error when it did not.
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;
use yukimi_system::log::Progress;
use yukimi_system::nix;

use super::Ctx;
use crate::ops::Operation;

/// What the dialog shows, taken from the progress now and then.
struct Snapshot {
    current: Option<String>,
    summary: String,
    fraction: Option<f64>,
    log: Vec<String>,
}

impl Snapshot {
    fn of(progress: &Progress) -> Snapshot {
        Snapshot {
            current: progress.current(),
            summary: progress.summary(),
            fraction: progress.fraction(),
            log: progress.log.iter().rev().take(200).rev().cloned().collect(),
        }
    }
}

enum Event {
    Progress(Snapshot),
    Done(Result<String, String>),
}

/// How long Nix may say nothing before the dialog mentions it.
const QUIET: Duration = Duration::from_secs(180);

/// Words for failures a person can act on.
fn explain(error: &str, system: bool, stopped: bool) -> String {
    if error.contains("dismissed") || error.contains("Not authorized") || error.contains("126") {
        return "Cancelled. Nothing was changed.".to_owned();
    }
    if stopped {
        return if system {
            "Stopped. Your configuration was put back as it was, and the running system was not changed.".to_owned()
        } else {
            "Stopped. Nothing was changed.".to_owned()
        };
    }
    if system {
        format!("{error}\n\nYour configuration was put back as it was, and the running system was not changed.")
    } else {
        error.to_owned()
    }
}

pub fn run(ctx: &Ctx, operation: Operation) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    let status = gtk::Label::new(Some(if operation.is_system() { "Waiting for permission…" } else { "Starting…" }));
    status.add_css_class("title-4");
    status.set_xalign(0.0);
    status.set_wrap(true);
    let bar = gtk::ProgressBar::new();
    let summary = super::widgets::dim("");
    summary.set_xalign(0.0);
    let error = super::widgets::wrapping("", &["error"]);
    error.set_selectable(true);
    error.set_visible(false);

    let log = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    log.add_css_class("job-log");
    let log_scroll = gtk::ScrolledWindow::builder().min_content_height(220).max_content_height(220).child(&log).build();
    let details = gtk::Expander::new(Some("Details"));
    details.set_child(Some(&log_scroll));

    let quiet = super::widgets::wrapping(
        "Nix hasn't reported anything for a few minutes. It may be waiting on a slow download. You can wait, or \
         stop and try again later.",
        &["dim-label"],
    );
    quiet.set_visible(false);

    let close = gtk::Button::with_label("Close");
    close.add_css_class("pill");
    close.set_halign(gtk::Align::Center);
    close.set_visible(false);
    let stop_button = gtk::Button::with_label("Stop");
    stop_button.add_css_class("pill");
    stop_button.set_halign(gtk::Align::Center);

    content.append(&status);
    content.append(&bar);
    content.append(&summary);
    content.append(&quiet);
    content.append(&error);
    content.append(&details);
    content.append(&stop_button);
    content.append(&close);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(false);
    view.add_top_bar(&header);
    view.set_content(Some(&content));
    let dialog = adw::Dialog::builder().title(operation.title()).content_width(600).child(&view).build();
    dialog.set_can_close(false);
    {
        let dialog = dialog.clone();
        close.connect_clicked(move |_| {
            dialog.force_close();
        });
    }
    dialog.present(Some(ctx.window()));

    // Pulse until Nix says how much there is to do.
    let pulsing = std::rc::Rc::new(std::cell::Cell::new(true));
    {
        let bar = bar.clone();
        let pulsing = pulsing.clone();
        glib::timeout_add_local(Duration::from_millis(120), move || {
            if pulsing.get() {
                bar.pulse();
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
    }

    // Stopping: the helper (as root) is asked through its input; this
    // user's own Nix is also signalled.
    let stop = nix::Stop::new(!operation.is_system());
    {
        let (stop, status) = (stop.clone(), status.clone());
        stop_button.connect_clicked(move |button| {
            stop.request();
            button.set_sensitive(false);
            status.set_text("Stopping…");
        });
    }
    // Mention it when Nix goes quiet for long.
    let heard = std::rc::Rc::new(std::cell::Cell::new(Instant::now()));
    {
        let (heard, quiet, running) = (heard.clone(), quiet.clone(), stop_button.clone());
        glib::timeout_add_seconds_local(5, move || {
            if !running.is_visible() {
                return glib::ControlFlow::Break;
            }
            quiet.set_visible(heard.get().elapsed() >= QUIET);
            glib::ControlFlow::Continue
        });
    }

    let (sender, receiver) = async_channel::unbounded::<Event>();
    let command = operation.command(&ctx.nixpkgs_ref());
    let streaming = stop.clone();
    std::thread::spawn(move || {
        let mut last = Instant::now() - Duration::from_secs(1);
        let result = nix::stream(command, &streaming, |progress| {
            // Plenty for the eye, without flooding the interface.
            if last.elapsed() >= Duration::from_millis(100) {
                last = Instant::now();
                let _ = sender.send_blocking(Event::Progress(Snapshot::of(progress)));
            }
        });
        let done = match result {
            Ok((out, progress)) => {
                let _ = sender.send_blocking(Event::Progress(Snapshot::of(&progress)));
                Ok(out)
            }
            Err(e) => Err(e.to_string()),
        };
        let _ = sender.send_blocking(Event::Done(done));
    });

    let ctx = ctx.clone();
    glib::spawn_future_local(async move {
        while let Ok(event) = receiver.recv().await {
            match event {
                Event::Progress(snapshot) => {
                    heard.set(Instant::now());
                    quiet.set_visible(false);
                    if !stop.requested() {
                        status.set_text(&snapshot.current.unwrap_or_else(|| "Working…".to_owned()));
                    }
                    summary.set_text(&snapshot.summary);
                    if let Some(fraction) = snapshot.fraction {
                        pulsing.set(false);
                        bar.set_fraction(fraction);
                    }
                    let buffer = log.buffer();
                    buffer.set_text(&snapshot.log.join("\n"));
                    let mut end = buffer.end_iter();
                    log.scroll_to_iter(&mut end, 0.0, false, 0.0, 0.0);
                }
                Event::Done(result) => {
                    pulsing.set(false);
                    dialog.set_can_close(true);
                    stop_button.set_visible(false);
                    quiet.set_visible(false);
                    match result {
                        Ok(out) => {
                            bar.set_fraction(1.0);
                            status.set_text(&operation.done());
                            ctx.toast(&operation.outcome(&out));
                            ctx.reload();
                            let dialog = dialog.clone();
                            glib::timeout_add_local_once(Duration::from_millis(900), move || {
                                dialog.force_close();
                            });
                        }
                        Err(message) => {
                            status.set_text(if stop.requested() { "Stopped" } else { "That did not work" });
                            error.set_text(&explain(&message, operation.is_system(), stop.requested()));
                            error.set_visible(true);
                            details.set_expanded(true);
                            close.set_visible(true);
                            ctx.reload();
                        }
                    }
                    break;
                }
            }
        }
    });
}
