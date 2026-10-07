// SPDX-License-Identifier: MIT OR Apache-2.0
//! What Yukimi can do to a system, as the commands that do it.
//!
//! Changes for the current user run Nix directly (`nix profile`). Changes
//! for everyone go through `yukimi-helper`, started with `pkexec` so polkit
//! asks for an administrator password; it edits `/etc/nixos`, builds the new
//! system and switches to it, and puts the configuration back if the build
//! fails. Both report Nix's progress on their error output.
use std::path::PathBuf;
use std::process::Command;

use yukimi_config::lock::Input;
use yukimi_system::nix;

/// When a new system takes over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Now.
    Switch,
    /// At the next restart.
    Boot,
}

impl Mode {
    fn arg(self) -> &'static str {
        match self {
            Mode::Switch => "switch",
            Mode::Boot => "boot",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Operation {
    /// Install a Nixpkgs package into the current user's profile.
    InstallForMe { attr: String, unfree: bool },
    /// Remove a package from the current user's profile.
    RemoveForMe { name: String },
    /// Change what the system installs, update its sources, or both, then
    /// build and apply it.
    ChangeSystem { packages: Option<Vec<String>>, applications: Option<Vec<String>>, update: Vec<String>, mode: Mode },
    /// Return the system to an earlier generation.
    Rollback { generation: u32 },
    /// Delete old system generations and collect garbage.
    Clean { older_than_days: Option<u32> },
}

impl Operation {
    /// What is happening, for the progress dialog's title.
    pub fn title(&self) -> String {
        match self {
            Operation::InstallForMe { attr, .. } => format!("Installing {attr} for you"),
            Operation::RemoveForMe { name } => format!("Removing {name}"),
            Operation::ChangeSystem { update, packages, applications, .. } => {
                if !update.is_empty() && packages.is_none() && applications.is_none() {
                    "Updating your system".to_owned()
                } else {
                    "Changing what your system installs".to_owned()
                }
            }
            Operation::Rollback { generation } => format!("Returning to generation {generation}"),
            Operation::Clean { .. } => "Cleaning up the store".to_owned(),
        }
    }

    /// What success looks like, for a toast.
    pub fn done(&self) -> String {
        match self {
            Operation::InstallForMe { attr, .. } => format!("{attr} is installed for you"),
            Operation::RemoveForMe { name } => format!("{name} is removed"),
            Operation::ChangeSystem { mode: Mode::Boot, .. } => "Ready: it takes over at the next restart".to_owned(),
            Operation::ChangeSystem { .. } => "Your system is up to date with your choices".to_owned(),
            Operation::Rollback { generation } => format!("Generation {generation} is running"),
            Operation::Clean { .. } => "The store is tidy".to_owned(),
        }
    }

    /// What to say when it worked, given what the helper reported.
    pub fn outcome(&self, out: &str) -> String {
        let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_default();
        match self {
            Operation::Rollback { generation } if report["configuration"] == false => format!(
                "Generation {generation} is running. Its configuration wasn't kept, so your next change builds on \
                 the newer one"
            ),
            Operation::Clean { .. } => match report["freed"].as_str() {
                Some(freed) if !freed.is_empty() => format!("The store is tidy: {freed}"),
                _ => self.done(),
            },
            _ => self.done(),
        }
    }

    /// Whether this changes the whole system (and asks for a password).
    pub fn is_system(&self) -> bool {
        !matches!(self, Operation::InstallForMe { .. } | Operation::RemoveForMe { .. })
    }

    /// The command that does it.
    pub fn command(&self, nixpkgs: &str) -> Command {
        match self {
            Operation::InstallForMe { attr, unfree } => {
                let mut c = nix::command();
                c.args(["profile", "install", "--log-format", "internal-json", "-v"]);
                if *unfree {
                    c.env("NIXPKGS_ALLOW_UNFREE", "1").arg("--impure");
                }
                c.arg(format!("{nixpkgs}#{attr}"));
                c
            }
            Operation::RemoveForMe { name } => {
                let mut c = nix::command();
                c.args(["profile", "remove", "--log-format", "internal-json", "-v", name]);
                c
            }
            Operation::ChangeSystem { packages, applications, update, mode } => {
                let mut c = helper();
                c.arg("change");
                if let Some(packages) = packages {
                    c.arg("--packages").arg(packages.join(","));
                }
                if let Some(applications) = applications {
                    c.arg("--applications").arg(applications.join(","));
                }
                if !update.is_empty() {
                    c.arg("--update").arg(update.join(","));
                }
                c.arg("--mode").arg(mode.arg());
                c
            }
            Operation::Rollback { generation } => {
                let mut c = helper();
                c.args(["rollback", &generation.to_string()]);
                c
            }
            Operation::Clean { older_than_days } => {
                let mut c = helper();
                c.arg("clean");
                if let Some(days) = older_than_days {
                    c.arg("--older-than-days").arg(days.to_string());
                }
                c
            }
        }
    }
}

/// `pkexec yukimi-helper`, the helper installed next to Yukimi.
fn helper() -> Command {
    let mut c = Command::new("pkexec");
    c.arg(helper_path());
    c
}

pub fn helper_path() -> PathBuf {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("/run/current-system/sw/bin"));
    dir.join("yukimi-helper")
}

/// The flake reference of the system's Nixpkgs, as `flake.lock` pins it, so
/// packages installed for one user match the system's versions.
pub fn nixpkgs_ref(inputs: &[Input]) -> String {
    let locked = inputs.iter().find(|i| i.name == "nixpkgs").and_then(|i| i.locked.as_ref());
    match locked {
        Some(l) if l.kind == "github" => match (&l.owner, &l.repo, &l.rev) {
            (Some(owner), Some(repo), Some(rev)) => format!("github:{owner}/{repo}/{rev}"),
            _ => "nixpkgs".to_owned(),
        },
        Some(l) => l.url.clone().unwrap_or_else(|| "nixpkgs".to_owned()),
        None => "nixpkgs".to_owned(),
    }
}

/// Open a terminal running `command`, with whichever terminal is installed.
pub fn open_terminal(command: &[String]) -> Result<(), String> {
    // (program, arguments before the command)
    let terminals: [(&str, &[&str]); 9] = [
        ("xdg-terminal-exec", &[]),
        ("kgx", &["--"]),
        ("gnome-terminal", &["--"]),
        ("konsole", &["-e"]),
        ("xfce4-terminal", &["-x"]),
        ("foot", &[]),
        ("ghostty", &["-e"]),
        ("kitty", &[]),
        ("alacritty", &["-e"]),
    ];
    for (program, prefix) in terminals {
        if find_program(program) {
            return Command::new(program).args(prefix).args(command).spawn().map(|_| ()).map_err(|e| e.to_string());
        }
    }
    Err("No terminal was found".to_owned())
}

fn find_program(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// The command that tries a package without installing it.
pub fn try_command(nixpkgs: &str, attr: &str, program: &str, unfree: bool) -> Vec<String> {
    let mut command = vec!["env".to_owned()];
    if unfree {
        command.push("NIXPKGS_ALLOW_UNFREE=1".to_owned());
    }
    command.extend(["nix", "shell"].map(str::to_owned));
    if unfree {
        command.push("--impure".to_owned());
    }
    command.push(format!("{nixpkgs}#{attr}"));
    command.push("--command".to_owned());
    if program.is_empty() {
        command.push(std::env::var("SHELL").unwrap_or_else(|_| "bash".to_owned()));
    } else {
        command.push(program.to_owned());
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use yukimi_config::lock::Locked;

    #[test]
    fn commands_for_each_operation() {
        let install =
            Operation::InstallForMe { attr: "htop".into(), unfree: false }.command("github:NixOS/nixpkgs/abc");
        let args: Vec<_> = install.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.ends_with(&["github:NixOS/nixpkgs/abc#htop".to_owned()]));
        let change = Operation::ChangeSystem {
            packages: Some(vec!["htop".into(), "ripgrep".into()]),
            applications: None,
            update: vec![],
            mode: Mode::Switch,
        }
        .command("x");
        let args: Vec<_> = change.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(&args[1..], ["change", "--packages", "htop,ripgrep", "--mode", "switch"]);
        assert!(Operation::Rollback { generation: 3 }.is_system());
    }

    #[test]
    fn nixpkgs_from_the_lock() {
        let input = |locked: Locked| Input {
            name: "nixpkgs".into(),
            locked: Some(locked),
            original: None,
            follows: None,
            follows_inputs: vec![],
        };
        let github = Locked {
            kind: "github".into(),
            owner: Some("NixOS".into()),
            repo: Some("nixpkgs".into()),
            rev: Some("abc".into()),
            ..Default::default()
        };
        assert_eq!(nixpkgs_ref(&[input(github)]), "github:NixOS/nixpkgs/abc");
        let tarball = Locked {
            kind: "tarball".into(),
            url: Some("https://api.flakehub.com/f/pinned/NixOS/nixpkgs/0.1.1/x/source.tar.gz".into()),
            ..Default::default()
        };
        assert!(nixpkgs_ref(&[input(tarball)]).starts_with("https://api.flakehub.com/"));
        assert_eq!(nixpkgs_ref(&[]), "nixpkgs");
        let try_it = try_command("nixpkgs", "cowsay", "cowsay", false);
        assert_eq!(try_it, ["env", "nix", "shell", "nixpkgs#cowsay", "--command", "cowsay"]);
    }
}
