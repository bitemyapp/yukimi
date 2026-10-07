// SPDX-License-Identifier: MIT OR Apache-2.0
//! Asking polkit for permission before `pkexec` does.
//!
//! `pkexec` asks polkit about its parent, Yukimi, and Yukimi's policy says
//! `auth_admin_keep`, so one password should cover the next few minutes of
//! changes. But polkit doesn't reuse an authorization granted inside one
//! pkexec's own check for the next pkexec. It does reuse one that Yukimi
//! obtains for itself, so Yukimi asks first (the same question, about
//! itself), and pkexec finds it already answered.
use std::collections::HashMap;

use gtk::gio;
use gtk::glib::prelude::*;
use gtk::glib::{self, Variant};

/// The action in Yukimi's polkit policy.
pub const ACTION: &str = "io.github.bitemyapp.Yukimi.change-system";

pub enum Answer {
    Allowed,
    Refused,
    /// polkit doesn't know Yukimi's action (it was started without its
    /// policy installed) or couldn't be asked: leave it to pkexec.
    Unknown,
}

/// When this process started, in clock ticks since boot, as polkit
/// identifies processes: field 22 of `/proc/self/stat`.
fn start_time() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // The command name, in parentheses, may itself contain spaces.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Ask polkit, showing its password dialog if needed, whether this process
/// may change the system. Blocks until the person answers.
pub fn authorize() -> Answer {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE) else {
        return Answer::Unknown;
    };
    let Some(start) = start_time() else {
        return Answer::Unknown;
    };
    let process: HashMap<String, Variant> = HashMap::from([
        ("pid".to_owned(), std::process::id().to_variant()),
        ("start-time".to_owned(), start.to_variant()),
    ]);
    let details: HashMap<String, String> = HashMap::new();
    // CheckAuthorization(subject, action, details, flags = AllowUserInteraction, cancellation id)
    let arguments = (("unix-process", process), ACTION, details, 1u32, "").to_variant();
    let reply = bus.call_sync(
        Some("org.freedesktop.PolicyKit1"),
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
        "CheckAuthorization",
        Some(&arguments),
        glib::VariantTy::new("((bba{ss}))").ok(),
        gio::DBusCallFlags::NONE,
        // As long as the person takes to type their password.
        i32::MAX,
        gio::Cancellable::NONE,
    );
    match reply.ok().and_then(|reply| reply.child_value(0).child_value(0).get::<bool>()) {
        Some(true) => Answer::Allowed,
        Some(false) => Answer::Refused,
        None => Answer::Unknown,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn this_process_has_a_start_time() {
        assert!(super::start_time().is_some_and(|t| t > 0));
    }
}
