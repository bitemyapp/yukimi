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
//! - `rollback <generation>` switches to an earlier system generation, and
//!   puts back the configuration it was built from.
//! - `clean [--older-than-days n]` deletes old system generations and
//!   collects garbage.
//!
//! Each generation's configuration is kept: a copy of `/etc/nixos` in
//! [`SAVED`]`/generation-<n>`. So going back to a generation brings back
//! the choices that made it, and the next change builds on those.
//!
//! Progress goes to standard error in Nix's machine-readable log format,
//! which Yukimi shows; the outcome goes to standard output as JSON. When
//! standard input is a pipe and Yukimi closes it, the helper stops what it
//! is doing and puts the configuration back, unless the new system is
//! already being started, which is never interrupted.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Mutex;

use yukimi_config::{edit, packages};
use yukimi_system::catalog;

const CONFIG: &str = "/etc/nixos";
const FLAKE: &str = "path:/etc/nixos";
const PROFILE: &str = "/nix/var/nix/profiles/system";
/// pkexec starts programs with an almost empty environment.
const PATH: &str = "/run/current-system/sw/bin:/run/wrappers/bin";
/// Where each generation's configuration is kept, as `generation-<n>`.
const SAVED: &str = yukimi_system::SAVED_CONFIGURATIONS;

/// Where the work is, which decides whether it can still be stopped.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Working,
    /// Yukimi asked to stop.
    Stopping,
    /// The new system is being started: too late to stop.
    Committed,
}

/// The phase, and the program running now (to stop when asked).
static STATE: Mutex<(Phase, Option<u32>)> = Mutex::new((Phase::Working, None));

fn state() -> std::sync::MutexGuard<'static, (Phase, Option<u32>)> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Stop when standard input closes, if it is a pipe: that is how Yukimi
/// asks. Standard input that is a terminal or /dev/null means nothing.
fn watch_for_stop() {
    // SAFETY: fstat writes only into the struct it is given.
    let is_pipe = unsafe {
        let mut stat: libc::stat = std::mem::zeroed();
        libc::fstat(0, &mut stat) == 0 && (stat.st_mode & libc::S_IFMT) == libc::S_IFIFO
    };
    if !is_pipe {
        return;
    }
    std::thread::spawn(|| {
        let mut buffer = [0u8; 64];
        while matches!(std::io::stdin().read(&mut buffer), Ok(n) if n > 0) {}
        let mut state = state();
        if state.0 == Phase::Committed {
            return;
        }
        state.0 = Phase::Stopping;
        if let Some(pid) = state.1 {
            // SAFETY: signalling a process has no memory-safety preconditions.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
    });
}

fn check_stopped() -> Result<()> {
    if state().0 == Phase::Stopping { Err(Failure("Stopped".into())) } else { Ok(()) }
}

/// From here on the work finishes even if Yukimi goes away, so that the
/// system is never left half switched.
fn commit() -> Result<()> {
    let mut state = state();
    if state.0 == Phase::Stopping {
        return Err(Failure("Stopped".into()));
    }
    state.0 = Phase::Committed;
    Ok(())
}

/// Start a command, remembering it so it can be stopped, and wait for it.
fn wait(c: &mut Command) -> Result<std::process::Output> {
    let child = {
        let mut state = state();
        if state.0 == Phase::Stopping {
            return Err(Failure("Stopped".into()));
        }
        let child = c.spawn()?;
        state.1 = Some(child.id());
        child
    };
    let out = child.wait_with_output();
    state().1 = None;
    check_stopped()?;
    Ok(out?)
}

/// A failure, in words for the person who asked.
#[derive(Debug)]
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
    let out = wait(c.stdout(Stdio::inherit()).stderr(Stdio::inherit()))?;
    if out.status.success() { Ok(()) } else { Err(Failure(format!("{what} failed"))) }
}

/// Run a command and keep its standard output.
fn output(mut c: Command, what: &str) -> Result<String> {
    let out = wait(c.stdout(Stdio::piped()).stderr(Stdio::inherit()))?;
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

/// The number of the system generation the profile points at.
fn current_generation() -> Option<u32> {
    let link = std::fs::read_link(PROFILE).ok()?;
    generation_number(link.file_name()?.to_str()?)
}

/// `system-12-link` is generation 12.
fn generation_number(link: &str) -> Option<u32> {
    link.strip_prefix("system-")?.strip_suffix("-link")?.parse().ok()
}

fn saved(generation: u32) -> PathBuf {
    yukimi_system::saved_configuration(generation)
}

/// Copy a configuration directory: its files and folders, not hidden ones
/// (such as a Git directory), readable by everyone as /etc/nixos is.
fn copy_configuration(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let (from, to) = (entry.path(), to.join(&name));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_configuration(&from, &to)?;
        } else if kind.is_file() {
            std::fs::write(&to, std::fs::read(&from)?)?;
            std::fs::set_permissions(&to, std::os::unix::fs::PermissionsExt::from_mode(0o644))?;
        }
    }
    Ok(())
}

/// Keep the configuration in /etc/nixos as generation `generation`'s,
/// unless one is kept already (or `replace`).
fn save_configuration(generation: u32, replace: bool) -> Result<()> {
    let dir = saved(generation);
    if dir.is_dir() && !replace {
        return Ok(());
    }
    let partial = dir.with_extension("new");
    let _ = std::fs::remove_dir_all(&partial);
    copy_configuration(Path::new(CONFIG), &partial)?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(&partial, &dir)?;
    Ok(())
}

/// Put a kept configuration back into /etc/nixos, with no `yukimi.nix` if
/// it had none.
fn restore_configuration(source: &Path) -> Result<()> {
    copy_configuration(source, Path::new(CONFIG))?;
    if !source.join(packages::FILE).exists() {
        let _ = std::fs::remove_file(Path::new(CONFIG).join(packages::FILE));
    }
    Ok(())
}

/// Forget the configurations of generations that no longer exist.
fn prune_saved() {
    let Ok(entries) = std::fs::read_dir(SAVED) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let generation = name.strip_prefix("generation-").and_then(|n| n.parse::<u32>().ok());
        let exists =
            generation.map(|g| Path::new(&format!("{PROFILE}-{g}-link")).symlink_metadata().is_ok()).unwrap_or(false);
        if !exists {
            let _ = std::fs::remove_dir_all(entry.path());
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
    // The configuration that built it, for going back to it later.
    if let Some(generation) = current_generation() {
        let _ = save_configuration(generation, true);
    }
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

    // The configuration as it is belongs to the running generation: keep it,
    // so going back to that generation brings it back.
    if let Some(generation) = current_generation() {
        let _ = save_configuration(generation, false);
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
    let system = match attempt().and_then(|system| commit().map(|_| system)) {
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
    if Path::new(&format!("{PROFILE}-{generation}-link")).symlink_metadata().is_err() {
        return Err(Failure(format!("There is no generation {generation}")));
    }
    if let Some(current) = current_generation() {
        let _ = save_configuration(current, false);
    }
    stage(1, &format!("Returning to generation {generation}"));
    commit()?;
    let mut set = command("nix-env");
    set.args(["-p", PROFILE, "--switch-generation", &generation.to_string()]);
    run(set, "Choosing the generation")?;
    // The configuration follows the system, so the next change builds on
    // what this generation was made from.
    let kept = saved(generation).is_dir();
    if kept {
        restore_configuration(&saved(generation))?;
    }
    switch(&format!("{PROFILE}/bin/switch-to-configuration"), "switch")?;
    stage_done(1);
    Ok(serde_json::json!({"ok": true, "generation": generation, "configuration": kept}).to_string())
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
        prune_saved();
        stage_done(1);
    }
    stage(2, "Collecting garbage");
    let mut gc = command("nix-store");
    gc.arg("--gc");
    let out = wait(gc.stdout(Stdio::piped()).stderr(Stdio::piped()))?;
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
    commit()?;
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
    if unsafe { libc::geteuid() } != 0 {
        report_error("yukimi-helper must be started through pkexec");
        return ExitCode::from(2);
    }
    watch_for_stop();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_names() {
        assert_eq!(split_list("a, b,,c"), vec!["a", "b", "c"]);
        assert!(split_list("").is_empty());
        assert!(valid_host("acer-predator") && !valid_host("a b") && !valid_host("x\"y"));
        assert_eq!(generation_number("system-12-link"), Some(12));
        assert_eq!(generation_number("system"), None);
    }

    #[test]
    fn configurations_copy_without_hidden_files() {
        let dir = std::env::temp_dir().join(format!("yukimi-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (from, to) = (dir.join("nixos"), dir.join("saved"));
        std::fs::create_dir_all(from.join("modules")).unwrap();
        std::fs::create_dir_all(from.join(".git")).unwrap();
        std::fs::write(from.join("configuration.nix"), "{ }").unwrap();
        std::fs::write(from.join("modules/desk.nix"), "{ }").unwrap();
        std::fs::write(from.join(".git/HEAD"), "ref").unwrap();
        copy_configuration(&from, &to).unwrap();
        assert_eq!(std::fs::read_to_string(to.join("configuration.nix")).unwrap(), "{ }");
        assert!(to.join("modules/desk.nix").is_file());
        assert!(!to.join(".git").exists());
        let _ = std::fs::remove_dir_all(&dir);
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
