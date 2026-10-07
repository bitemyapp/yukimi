// SPDX-License-Identifier: MIT OR Apache-2.0
//! Running Nix: quick queries that return JSON, and long operations whose
//! progress streams through [`crate::log::Progress`].
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use crate::log::Progress;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run {0}: {1}")]
    Spawn(String, std::io::Error),
    #[error("{0}")]
    Failed(String),
    #[error("Nix's answer could not be read: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// `nix` with the features Yukimi uses switched on.
pub fn command() -> Command {
    let mut command = Command::new("nix");
    command.args(["--extra-experimental-features", "nix-command flakes"]);
    command
}

/// Run Nix and parse its output as JSON.
pub fn json(args: &[&str]) -> Result<serde_json::Value> {
    let output = command().args(args).stdin(Stdio::null()).output().map_err(|e| Error::Spawn("nix".into(), e))?;
    if !output.status.success() {
        let stderr = crate::log::strip_ansi(&String::from_utf8_lossy(&output.stderr));
        return Err(Error::Failed(last_error(&stderr)));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// The useful part of Nix's error output: from the last `error:` on.
pub fn last_error(stderr: &str) -> String {
    match stderr.rfind("error:") {
        Some(at) => stderr[at..].trim().to_owned(),
        None => stderr.trim().to_owned(),
    }
}

/// Run a program that speaks Nix's progress log on its error output,
/// calling `update` with the progress after each event. Returns its
/// standard output.
pub fn stream(mut command: Command, mut update: impl FnMut(&Progress)) -> Result<(String, Progress)> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Spawn(program, e))?;
    let stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(stdout), &mut out);
        out
    });
    let mut progress = Progress::default();
    for line in BufReader::new(child.stderr.take().expect("piped")).lines().map_while(|l| l.ok()) {
        if progress.feed(&line) {
            update(&progress);
        }
    }
    let status = child.wait().map_err(|e| Error::Spawn("nix".into(), e))?;
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        let message = progress
            .errors()
            .last()
            .map(str::to_owned)
            .unwrap_or_else(|| progress.log.iter().rev().take(5).cloned().collect::<Vec<_>>().join("\n"));
        return Err(Error::Failed(message));
    }
    Ok((out, progress))
}

/// Store paths of the system flake's inputs Yukimi reads from: Nixpkgs and
/// the installer's source (for its catalog).
pub fn system_inputs(config_dir: &str) -> Result<SystemInputs> {
    let expr = format!(
        "let f = builtins.getFlake \"path:{config_dir}\"; in {{ \
           nixpkgs = f.inputs.nixpkgs.outPath; \
           calamares = if f.inputs ? calamares then f.inputs.calamares.outPath else null; }}"
    );
    let value = json(&["eval", "--json", "--impure", "--expr", &expr])?;
    Ok(SystemInputs {
        nixpkgs: value["nixpkgs"].as_str().map(str::to_owned),
        calamares: value["calamares"].as_str().map(str::to_owned),
    })
}

#[derive(Clone, Debug, Default)]
pub struct SystemInputs {
    pub nixpkgs: Option<String>,
    pub calamares: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_start_at_the_last_error() {
        let stderr = "warning: Git tree is dirty\nerror: builder failed\n\nerror: attribute 'x' missing\n  at line 3";
        assert_eq!(last_error(stderr), "error: attribute 'x' missing\n  at line 3");
        assert_eq!(last_error("plain"), "plain");
    }

    #[test]
    fn streams_progress_and_output() {
        let mut command = Command::new("sh");
        command.args(["-c", r#"echo '@nix {"action":"msg","level":1,"msg":"warning: hello"}' >&2; echo result"#]);
        let mut updates = 0;
        let (out, progress) = stream(command, |_| updates += 1).unwrap();
        assert_eq!(out.trim(), "result");
        assert_eq!(updates, 1);
        assert_eq!(progress.messages[0].text, "warning: hello");
        let mut failing = Command::new("sh");
        failing.args(["-c", r#"echo '@nix {"action":"msg","level":0,"msg":"error: nope"}' >&2; exit 1"#]);
        assert_eq!(stream(failing, |_| {}).unwrap_err().to_string(), "error: nope");
    }
}
