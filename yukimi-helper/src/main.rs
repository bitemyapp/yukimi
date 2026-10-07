// SPDX-License-Identifier: MIT OR Apache-2.0
//! `yukimi-helper`: the part of Yukimi that changes the whole system. It
//! runs as root through `pkexec`, so it does only a few fixed things, with
//! arguments it checks itself:
//!
//! - `change [--packages a,b] [--applications x,y] [--update i,j] --mode switch|boot`
//!   writes the packages into `/etc/nixos/yukimi.nix` (importing it from
//!   `configuration.nix` once), sets `calamares.applications`, updates flake
//!   inputs, builds the system and switches to it. If anything before the
//!   switch fails, every file it touched is put back as it was.
//! - `rollback <generation>` switches to an earlier system generation.
//! - `clean [--older-than-days n]` deletes old system generations and
//!   collects garbage.
//!
//! Progress goes to standard error in Nix's machine-readable log format,
//! which Yukimi shows; the outcome goes to standard output as JSON.
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use yukimi_config::{edit, packages};
use yukimi_system::catalog;

const CONFIG: &str = "/etc/nixos";
const FLAKE: &str = "path:/etc/nixos";
const PROFILE: &str = "/nix/var/nix/profiles/system";
/// pkexec starts programs with an almost empty environment.
const PATH: &str = "/run/current-system/sw/bin:/run/wrappers/bin";

/// A failure, in words for the person who asked.
struct Failure(String);

impl<E: std::fmt::Display> From<E> for Failure {
    fn from(e: E) -> Failure {
        Failure(e.to_string())
    }
}

type Result<T> = std::result::Result<T, Failure>;

/// Say what is happening, as an activity Yukimi shows.
fn stage(id: u64, text: &str) {
    eprintln!(
        "@nix {}",
        serde_json::json!({"action": "start", "id": id, "level": 0, "type": 0, "text": text, "fields": []})
    );
}

fn stage_done(id: u64) {
    eprintln!("@nix {}", serde_json::json!({"action": "stop", "id": id}));
}

fn report_error(message: &str) {
    eprintln!("@nix {}", serde_json::json!({"action": "msg", "level": 0, "msg": message}));
}

fn command(program: &str) -> Command {
    let mut c = Command::new(program);
    c.env_clear().env("PATH", PATH).env("HOME", "/root").env("LANG", "C.UTF-8").stdin(Stdio::null());
    if let Ok(archive) = std::fs::read_link("/run/current-system/sw/lib/locale/locale-archive") {
        c.env("LOCALE_ARCHIVE", archive);
    }
    c
}

fn nix() -> Command {
    let mut c = command("nix");
    c.args(["--extra-experimental-features", "nix-command flakes"]);
    c
}

/// Run a command whose error output goes straight to Yukimi.
fn run(mut c: Command, what: &str) -> Result<()> {
    let status = c.stdout(Stdio::inherit()).stderr(Stdio::inherit()).status()?;
    if status.success() { Ok(()) } else { Err(Failure(format!("{what} failed"))) }
}

/// Run a command and keep its standard output.
fn output(mut c: Command, what: &str) -> Result<String> {
    let out = c.stdout(Stdio::piped()).stderr(Stdio::inherit()).output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(Failure(format!("{what} failed")))
    }
}

/// Files as they were, to put back if a change fails.
struct Backup(Vec<(PathBuf, Option<Vec<u8>>)>);

impl Backup {
    fn take(files: &[&str]) -> Backup {
        Backup(
            files
                .iter()
                .map(|f| {
                    let path = Path::new(CONFIG).join(f);
                    let contents = std::fs::read(&path).ok();
                    (path, contents)
                })
                .collect(),
        )
    }

    fn restore(&self) {
        for (path, contents) in &self.0 {
            match contents {
                Some(bytes) => {
                    let _ = std::fs::write(path, bytes);
                }
                None => {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
}

fn split_list(value: &str) -> Vec<String> {
    value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

/// The `nixosConfigurations` entry to build: the one named after this
/// computer, or the only one.
fn host() -> Result<String> {
    let configuration = std::fs::read_to_string(Path::new(CONFIG).join("configuration.nix")).unwrap_or_default();
    let wanted = edit::string_value(&configuration, &["networking", "hostName"])
        .ok()
        .flatten()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok().map(|h| h.trim().to_owned()))
        .unwrap_or_default();
    let mut names = nix();
    names.args(["eval", "--json", &format!("{FLAKE}#nixosConfigurations"), "--apply", "builtins.attrNames"]);
    let names: Vec<String> = serde_json::from_str(&output(names, "Reading the configurations")?)?;
    if names.contains(&wanted) {
        return Ok(wanted);
    }
    match names.as_slice() {
        [only] => Ok(only.clone()),
        _ => Err(Failure(format!(
            "The flake in {CONFIG} has no configuration named {wanted:?} (it has {})",
            names.join(", ")
        ))),
    }
}

fn valid_host(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// Make a built system the current generation and start it (`switch`) or
/// make it the one the next start uses (`boot`), as nixos-rebuild does:
/// through systemd-run, so restarting services cannot stop it halfway.
fn activate(system: &str, mode: &str) -> Result<()> {
    let mut set = command("nix-env");
    set.args(["-p", PROFILE, "--set", system]);
    run(set, "Setting the system profile")?;
    switch(&format!("{system}/bin/switch-to-configuration"), mode)
}

fn switch(program: &str, mode: &str) -> Result<()> {
    let mut c = command("systemd-run");
    c.args([
        "-E",
        "LOCALE_ARCHIVE",
        "-E",
        "NIXOS_INSTALL_BOOTLOADER",
        "--collect",
        "--no-ask-password",
        "--pipe",
        "--quiet",
        "--service-type=exec",
        "--unit=yukimi-switch-to-configuration",
        "--wait",
        program,
        mode,
    ]);
    run(c, "Switching to the new system")
}

fn change(args: &[String]) -> Result<String> {
    let mut packages_arg = None;
    let mut applications_arg = None;
    let mut update = Vec::new();
    let mut mode = "switch".to_owned();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| Failure(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--packages" => packages_arg = Some(split_list(value)),
            "--applications" => applications_arg = Some(split_list(value)),
            "--update" => update = split_list(value),
            "--mode" => mode = value.clone(),
            other => return Err(Failure(format!("Unknown option {other}"))),
        }
    }
    if !matches!(mode.as_str(), "switch" | "boot") {
        return Err(Failure(format!("Unknown mode {mode}")));
    }
    if let Some(bad) = update.iter().find(|i| !packages::valid_attribute(i)) {
        return Err(Failure(format!("Not an input name: {bad:?}")));
    }
    if let Some(bad) = applications_arg.iter().flatten().find(|a| !catalog::valid_id(a)) {
        return Err(Failure(format!("Not an application id: {bad:?}")));
    }

    let backup = Backup::take(&["configuration.nix", packages::FILE, "flake.lock"]);
    let attempt = || -> Result<String> {
        let config_path = Path::new(CONFIG).join("configuration.nix");
        if packages_arg.is_some() || applications_arg.is_some() {
            stage(1, "Writing your configuration");
            let mut configuration = std::fs::read_to_string(&config_path)?;
            if let Some(list) = &packages_arg {
                std::fs::write(Path::new(CONFIG).join(packages::FILE), packages::render(list)?)?;
                configuration = packages::import_into(&configuration)?;
            }
            if let Some(list) = &applications_arg {
                configuration = edit::set_string_list(&configuration, &["calamares", "applications"], list)?;
            }
            std::fs::write(&config_path, configuration)?;
            stage_done(1);
        }
        if !update.is_empty() {
            stage(2, "Fetching the newest versions");
            let mut c = nix();
            c.args(["flake", "update", "--log-format", "internal-json", "-v", "--flake", FLAKE]);
            c.args(&update);
            run(c, "Updating the flake inputs")?;
            stage_done(2);
        }
        stage(3, "Evaluating your configuration");
        let host = host()?;
        if !valid_host(&host) {
            return Err(Failure(format!("Not a configuration name: {host:?}")));
        }
        let mut build = nix();
        build.args([
            "build",
            "--log-format",
            "internal-json",
            "-v",
            "--no-link",
            "--print-out-paths",
            &format!("{FLAKE}#nixosConfigurations.\"{host}\".config.system.build.toplevel"),
        ]);
        let system = output(build, "Building the new system")?.trim().to_owned();
        stage_done(3);
        Ok(system)
    };
    let system = match attempt() {
        Ok(system) => system,
        Err(e) => {
            backup.restore();
            return Err(e);
        }
    };
    stage(4, if mode == "switch" { "Switching to the new system" } else { "Setting it up for the next start" });
    activate(&system, &mode)?;
    stage_done(4);
    Ok(serde_json::json!({"ok": true, "system": system}).to_string())
}

fn rollback(args: &[String]) -> Result<String> {
    let generation: u32 = args
        .first()
        .and_then(|g| g.parse().ok())
        .ok_or_else(|| Failure("rollback needs a generation number".into()))?;
    stage(1, &format!("Returning to generation {generation}"));
    let mut set = command("nix-env");
    set.args(["-p", PROFILE, "--switch-generation", &generation.to_string()]);
    run(set, "Choosing the generation")?;
    switch(&format!("{PROFILE}/bin/switch-to-configuration"), "switch")?;
    stage_done(1);
    Ok(serde_json::json!({"ok": true, "generation": generation}).to_string())
}

fn clean(args: &[String]) -> Result<String> {
    let mut days = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--older-than-days" => {
                days = Some(
                    it.next()
                        .and_then(|d| d.parse::<u32>().ok())
                        .filter(|d| *d > 0)
                        .ok_or_else(|| Failure("--older-than-days needs a number of days".into()))?,
                )
            }
            other => return Err(Failure(format!("Unknown option {other}"))),
        }
    }
    if let Some(days) = days {
        stage(1, "Deleting old generations");
        let mut delete = command("nix-env");
        delete.args(["-p", PROFILE, "--delete-generations", &format!("{days}d")]);
        run(delete, "Deleting old generations")?;
        stage_done(1);
    }
    stage(2, "Collecting garbage");
    let mut gc = command("nix-store");
    gc.arg("--gc");
    let out = gc.stdout(Stdio::piped()).stderr(Stdio::piped()).output()?;
    let text = String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        eprintln!("{line}");
    }
    if !out.status.success() {
        return Err(Failure("Collecting garbage failed".into()));
    }
    stage_done(2);
    // The boot menu lists only generations that still exist.
    stage(3, "Updating the boot menu");
    switch(&format!("{PROFILE}/bin/switch-to-configuration"), "boot")?;
    stage_done(3);
    let freed = text.lines().rev().find(|l| l.contains("freed")).unwrap_or("").trim().to_owned();
    Ok(serde_json::json!({"ok": true, "freed": freed}).to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest) = match args.split_first() {
        Some((c, rest)) => (c.as_str(), rest),
        None => ("", &[][..]),
    };
    // SAFETY: geteuid has no preconditions.
    if unsafe { geteuid() } != 0 {
        report_error("yukimi-helper must be started through pkexec");
        return ExitCode::from(2);
    }
    let result = match command {
        "change" => change(rest),
        "rollback" => rollback(rest),
        "clean" => clean(rest),
        _ => Err(Failure("Usage: yukimi-helper change|rollback|clean …".into())),
    };
    match result {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(Failure(message)) => {
            report_error(&message);
            println!("{}", serde_json::json!({"ok": false, "error": message}));
            ExitCode::FAILURE
        }
    }
}

unsafe extern "C" {
    fn geteuid() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_names() {
        assert_eq!(split_list("a, b,,c"), vec!["a", "b", "c"]);
        assert!(split_list("").is_empty());
        assert!(valid_host("acer-predator") && !valid_host("a b") && !valid_host("x\"y"));
    }

    #[test]
    fn backups_restore_contents_and_absence() {
        let dir = std::env::temp_dir().join(format!("yukimi-backup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (kept, created) = (dir.join("kept"), dir.join("created"));
        std::fs::write(&kept, "before").unwrap();
        let backup = Backup(vec![(kept.clone(), Some(b"before".to_vec())), (created.clone(), None)]);
        std::fs::write(&kept, "after").unwrap();
        std::fs::write(&created, "new").unwrap();
        backup.restore();
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "before");
        assert!(!created.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
