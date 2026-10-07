// SPDX-License-Identifier: MIT OR Apache-2.0
//! `yukimi.nix`: the packages installed for everyone with Yukimi.
//!
//! A small module of its own next to `configuration.nix`, imported from it
//! once, so Yukimi never has to rewrite anything a person wrote. Packages
//! are kept as their attribute names in Nixpkgs (`htop`,
//! `python3Packages.requests`), checked before they are written.
use crate::edit::{add_import, nix_string, string_list};
use crate::{Error, Result};

/// The file, next to `configuration.nix`.
pub const FILE: &str = "yukimi.nix";
/// How `configuration.nix` imports it.
pub const IMPORT: &str = "./yukimi.nix";

/// Whether a name is an attribute path Yukimi will write: Nix identifiers
/// separated by dots.
pub fn valid_attribute(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name.split('.').all(|part| {
            let mut chars = part.chars();
            chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '\''))
        })
}

/// The file's contents for these packages, sorted and without duplicates.
pub fn render(packages: &[String]) -> Result<String> {
    if let Some(bad) = packages.iter().find(|name| !valid_attribute(name)) {
        return Err(Error::BadAttribute(bad.clone()));
    }
    let mut names = packages.to_vec();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    let list = if names.is_empty() {
        "[ ]".to_owned()
    } else {
        let lines: String = names.iter().map(|name| format!("    {}\n", nix_string(name))).collect();
        format!("[\n{lines}  ]")
    };
    Ok(format!(
        "# Packages for everyone on this computer, added with Yukimi: their\n\
         # attribute names in Nixpkgs, the names `nix search` shows. Yukimi\n\
         # rewrites this file when packages are added or removed there.\n\
         {{ lib, pkgs, ... }}:\n\
         let\n  \
           packages = {list};\n\
         in\n\
         {{\n  \
           environment.systemPackages = map (name: lib.getAttrFromPath (lib.splitString \".\" name) pkgs) packages;\n\
         }}\n"
    ))
}

/// The packages listed in an existing file.
pub fn read(text: &str) -> Result<Vec<String>> {
    Ok(string_list(text, &["packages"])?.unwrap_or_default())
}

/// `configuration.nix` with the file imported.
pub fn import_into(configuration: &str) -> Result<String> {
    add_import(configuration, IMPORT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_names() {
        for good in ["htop", "python3Packages.requests", "gnome-tweaks", "_7zz", "kdePackages.kate"] {
            assert!(valid_attribute(good), "{good}");
        }
        for bad in ["", "a b", "a;b", "a..b", ".a", "${x}", "a\"b", "7zip", "../x"] {
            assert!(!valid_attribute(bad), "{bad}");
        }
    }

    #[test]
    fn renders_and_reads_back() {
        let text = render(&["ripgrep".into(), "htop".into(), "htop".into(), "python3Packages.rich".into()]).unwrap();
        assert!(text.contains("    \"htop\"\n    \"python3Packages.rich\"\n    \"ripgrep\"\n"), "{text}");
        assert_eq!(read(&text).unwrap(), vec!["htop", "python3Packages.rich", "ripgrep"]);
        assert_eq!(read(&render(&[]).unwrap()).unwrap(), Vec::<String>::new());
        assert_eq!(render(&["oops; rm".into()]), Err(Error::BadAttribute("oops; rm".into())));
    }
}
