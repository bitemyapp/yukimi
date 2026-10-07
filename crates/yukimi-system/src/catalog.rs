// SPDX-License-Identifier: MIT OR Apache-2.0
//! The installer's curated applications: a few dozen well-known apps with
//! friendly names, descriptions and categories, installed through the
//! `calamares.applications` setting rather than by package name.
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Application {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub unfree: bool,
    /// The command to run, for terminal applications.
    pub terminal: Option<String>,
    /// Other applications this one selects too.
    #[serde(default)]
    pub requires: Vec<String>,
}

/// The catalog's location in the installer's source.
pub const CATALOG_PATH: &str = "rust/src/applications.json";

pub fn parse(text: &str) -> Option<Vec<Application>> {
    serde_json::from_str(text).ok()
}

/// Whether an id is one Yukimi will write into the configuration.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_installers_catalog_format() {
        let text = r#"[{"id":"zed","name":"Zed","description":"Fast editor.","category":"Editors","packages":["zed-editor"]},
          {"id":"rustup","name":"Rustup","description":"Toolchains.","category":"Development tools","packages":["rustup"],"requires":["build-tools"]},
          {"id":"steam","name":"Steam","description":"Games.","category":"Gaming","packages":["steam"],"unfree":true}]"#;
        let apps = parse(text).unwrap();
        assert_eq!(apps.len(), 3);
        assert_eq!(apps[1].requires, vec!["build-tools"]);
        assert!(apps[2].unfree && !apps[0].unfree);
        assert!(valid_id("google-chrome") && !valid_id("Bad Id") && !valid_id(""));
    }
}
