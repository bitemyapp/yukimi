// SPDX-License-Identifier: MIT OR Apache-2.0
//! `flake.lock`: the exact revision of every input a system is built from.
//!
//! [`FlakeLock::inputs`] lists the root's own inputs (nixpkgs, the installer's
//! modules, a desktop) with where each comes from, the revision it is locked
//! to and when that revision was made, which is what "how out of date am I"
//! comes down to. Inputs that follow another one (`calamares.inputs.nixpkgs
//! follows nixpkgs`) are shown as such.
use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{Error, Result};

#[derive(Debug, Deserialize)]
struct Raw {
    nodes: BTreeMap<String, Node>,
    root: String,
}

#[derive(Debug, Deserialize)]
struct Node {
    #[serde(default)]
    inputs: BTreeMap<String, InputRef>,
    locked: Option<Locked>,
    original: Option<Locked>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum InputRef {
    Node(String),
    Follows(Vec<String>),
}

/// A source as `flake.lock` records it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Locked {
    #[serde(rename = "type", default)]
    pub kind: String,
    pub owner: Option<String>,
    pub repo: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub rev: Option<String>,
    pub url: Option<String>,
    pub host: Option<String>,
    pub last_modified: Option<i64>,
    pub nar_hash: Option<String>,
}

impl Locked {
    /// A short description of where the source comes from:
    /// `github:NixOS/nixpkgs/nixos-unstable`, `flakehub:NixOS/nixpkgs/0.1`.
    pub fn describe(&self) -> String {
        match (self.kind.as_str(), &self.owner, &self.repo) {
            ("github" | "gitlab" | "sourcehut", Some(owner), Some(repo)) => {
                let mut text = format!("{}:{owner}/{repo}", self.kind);
                if let Some(reference) = &self.reference {
                    text.push('/');
                    text.push_str(reference);
                }
                text
            }
            _ => match &self.url {
                Some(url) => flakehub(url).unwrap_or_else(|| url.clone()),
                None => self.kind.clone(),
            },
        }
    }

    /// The first characters of the revision, as Git shows them.
    pub fn short_rev(&self) -> Option<&str> {
        self.rev.as_deref().map(|rev| &rev[..rev.len().min(7)])
    }

    /// A web page showing this revision, for GitHub sources.
    pub fn web_url(&self) -> Option<String> {
        match (self.kind.as_str(), &self.owner, &self.repo, &self.rev) {
            ("github", Some(owner), Some(repo), Some(rev)) => {
                Some(format!("https://github.com/{owner}/{repo}/commit/{rev}"))
            }
            _ => None,
        }
    }
}

/// `https://flakehub.com/f/NixOS/nixpkgs/0.1` and the API's tarball URLs,
/// as `flakehub:NixOS/nixpkgs/0.1`.
fn flakehub(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://flakehub.com/f/")
        .or_else(|| url.strip_prefix("https://api.flakehub.com/f/pinned/"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [owner, project, version, ..] => {
            Some(format!("flakehub:{owner}/{project}/{}", version.trim_end_matches(".tar.gz")))
        }
        _ => None,
    }
}

/// One of the root's inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    pub name: String,
    /// What it is locked to; `None` when it follows another input.
    pub locked: Option<Locked>,
    /// What `flake.nix` asks for (a branch, a version range).
    pub original: Option<Locked>,
    /// The input path this one follows, such as `["nixpkgs"]`.
    pub follows: Option<Vec<String>>,
    /// Root inputs that this input's own inputs follow, such as `nixpkgs`
    /// and `tatami` for `calamares`.
    pub follows_inputs: Vec<String>,
}

/// A parsed `flake.lock`.
#[derive(Debug)]
pub struct FlakeLock {
    raw: Raw,
}

impl FlakeLock {
    pub fn parse(text: &str) -> Result<FlakeLock> {
        let raw: Raw = serde_json::from_str(text).map_err(|e| Error::Lock(e.to_string()))?;
        if !raw.nodes.contains_key(&raw.root) {
            return Err(Error::Lock(format!("the root node {:?} is missing", raw.root)));
        }
        Ok(FlakeLock { raw })
    }

    /// The root's inputs, by name.
    pub fn inputs(&self) -> Vec<Input> {
        let root = &self.raw.nodes[&self.raw.root];
        root.inputs
            .iter()
            .map(|(name, input)| match input {
                InputRef::Node(node) => {
                    let found = self.raw.nodes.get(node);
                    let follows_inputs = found
                        .map(|n| {
                            n.inputs
                                .values()
                                .filter_map(|r| match r {
                                    InputRef::Follows(path) if path.len() == 1 && path[0] != *name => {
                                        Some(path[0].clone())
                                    }
                                    _ => None,
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Input {
                        name: name.clone(),
                        locked: found.and_then(|n| n.locked.clone()),
                        original: found.and_then(|n| n.original.clone()),
                        follows: None,
                        follows_inputs,
                    }
                }
                InputRef::Follows(path) => Input {
                    name: name.clone(),
                    locked: None,
                    original: None,
                    follows: Some(path.clone()),
                    follows_inputs: Vec::new(),
                },
            })
            .collect()
    }

    /// One input, by name.
    pub fn input(&self, name: &str) -> Option<Input> {
        self.inputs().into_iter().find(|input| input.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"{
      "nodes": {
        "calamares": {
          "inputs": { "nixpkgs": ["nixpkgs"], "tatami": ["tatami"] },
          "locked": { "lastModified": 1791340000, "narHash": "sha256-x", "owner": "bitemyapp", "repo": "calamares",
                      "rev": "0d10e385c48d2c5be463aeb839d73047dc770665", "type": "github" },
          "original": { "owner": "bitemyapp", "ref": "stable", "repo": "calamares", "type": "github" }
        },
        "nixpkgs": {
          "locked": { "lastModified": 1790600678, "narHash": "sha256-y", "rev": "f45c6f04c2f013f004bf94e284e95d72898d9393",
                      "type": "tarball", "url": "https://api.flakehub.com/f/pinned/NixOS/nixpkgs/0.1.912345%2Brev-f45c6f04/0199/source.tar.gz" },
          "original": { "type": "tarball", "url": "https://flakehub.com/f/NixOS/nixpkgs/0.1" }
        },
        "tatami": {
          "inputs": { "nixpkgs": ["nixpkgs"] },
          "locked": { "lastModified": 1791338000, "owner": "bitemyapp", "repo": "tatami", "rev": "b925d7a9c9", "type": "github" },
          "original": { "owner": "bitemyapp", "ref": "stable", "repo": "tatami", "type": "github" }
        },
        "root": { "inputs": { "calamares": "calamares", "nixpkgs": "nixpkgs", "tatami": "tatami", "pkgs": ["nixpkgs"] } }
      },
      "root": "root",
      "version": 7
    }"#;

    #[test]
    fn root_inputs_with_sources_and_ages() {
        let lock = FlakeLock::parse(LOCK).unwrap();
        let inputs = lock.inputs();
        assert_eq!(inputs.len(), 4);
        let calamares = lock.input("calamares").unwrap();
        let locked = calamares.locked.unwrap();
        assert_eq!(locked.short_rev(), Some("0d10e38"));
        assert_eq!(locked.last_modified, Some(1791340000));
        assert_eq!(calamares.original.unwrap().describe(), "github:bitemyapp/calamares/stable");
        assert_eq!(calamares.follows_inputs, vec!["nixpkgs".to_owned(), "tatami".to_owned()]);
        let nixpkgs = lock.input("nixpkgs").unwrap();
        assert_eq!(nixpkgs.original.unwrap().describe(), "flakehub:NixOS/nixpkgs/0.1");
        assert_eq!(lock.input("pkgs").unwrap().follows, Some(vec!["nixpkgs".to_owned()]));
        assert_eq!(
            lock.input("tatami").unwrap().locked.unwrap().web_url().as_deref(),
            Some("https://github.com/bitemyapp/tatami/commit/b925d7a9c9")
        );
        assert!(FlakeLock::parse("{}").is_err());
    }
}
