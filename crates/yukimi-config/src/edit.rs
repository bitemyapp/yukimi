// SPDX-License-Identifier: MIT OR Apache-2.0
//! Find one setting in a NixOS module and change only it.
//!
//! A setting can be written as a path (`calamares.applications = [ … ];`) or
//! nested (`calamares = { applications = [ … ]; };`); both are found. A
//! module can be a set of settings or a function returning one, possibly
//! through `let … in` or `with …;`. Changes are made by replacing the text of
//! one expression, so everything else in the file is untouched, and the
//! result must parse cleanly or the change is refused.
use rnix::ast::{self, HasEntry};
use rnix::{SyntaxKind, TextRange};
use rowan::ast::AstNode;

use crate::{Error, Result};

fn parse(text: &str) -> Result<ast::Root> {
    let parsed = ast::Root::parse(text);
    if let Some(error) = parsed.errors().first() {
        return Err(Error::Syntax(error.to_string()));
    }
    Ok(parsed.tree())
}

/// The set of settings a module consists of: the body of its function, past
/// any `let … in`, `with …;` and parentheses.
fn module_set(root: &ast::Root) -> Result<ast::AttrSet> {
    let mut expr = root.expr().ok_or(Error::NotAModule)?;
    loop {
        expr = match expr {
            ast::Expr::AttrSet(set) => return Ok(set),
            ast::Expr::Lambda(lambda) => lambda.body().ok_or(Error::NotAModule)?,
            ast::Expr::LetIn(let_in) => let_in.body().ok_or(Error::NotAModule)?,
            ast::Expr::With(with) => with.body().ok_or(Error::NotAModule)?,
            ast::Expr::Paren(paren) => paren.expr().ok_or(Error::NotAModule)?,
            _ => return Err(Error::NotAModule),
        };
    }
}

/// The `let` bindings in front of a module's settings, if any.
fn module_lets(root: &ast::Root) -> Vec<ast::LetIn> {
    let mut lets = Vec::new();
    let mut expr = root.expr();
    while let Some(e) = expr {
        expr = match e {
            ast::Expr::Lambda(lambda) => lambda.body(),
            ast::Expr::LetIn(let_in) => {
                let body = let_in.body();
                lets.push(let_in);
                body
            }
            ast::Expr::With(with) => with.body(),
            ast::Expr::Paren(paren) => paren.expr(),
            _ => None,
        };
    }
    lets
}

/// The plain name of an attribute, if it has one (`a`, `"a"`), as opposed to
/// a computed `${…}` one.
fn attr_name(attr: &ast::Attr) -> Option<String> {
    match attr {
        ast::Attr::Ident(ident) => Some(ident.ident_token()?.text().to_owned()),
        ast::Attr::Str(string) => plain_string(string),
        ast::Attr::Dynamic(_) => None,
    }
}

/// A string literal without interpolation, unescaped.
fn plain_string(string: &ast::Str) -> Option<String> {
    let mut out = String::new();
    for part in string.normalized_parts() {
        match part {
            ast::InterpolPart::Literal(text) => out.push_str(&text),
            ast::InterpolPart::Interpolation(_) => return None,
        }
    }
    Some(out)
}

/// Find a setting below an entry container.
fn find_in(entries: &impl HasEntry, path: &[&str]) -> Option<ast::Expr> {
    for entry in entries.attrpath_values() {
        let Some(attrpath) = entry.attrpath() else { continue };
        let names: Option<Vec<String>> = attrpath.attrs().map(|a| attr_name(&a)).collect();
        let Some(names) = names else { continue };
        if names.len() > path.len() || names.iter().zip(path).any(|(a, b)| a != b) {
            continue;
        }
        let value = entry.value()?;
        if names.len() == path.len() {
            return Some(value);
        }
        if let ast::Expr::AttrSet(inner) = &value
            && let Some(found) = find_in(inner, &path[names.len()..])
        {
            return Some(found);
        }
    }
    None
}

fn find(root: &ast::Root, path: &[&str]) -> Result<Option<ast::Expr>> {
    let set = module_set(root)?;
    if let Some(found) = find_in(&set, path) {
        return Ok(Some(found));
    }
    Ok(module_lets(root).iter().find_map(|let_in| find_in(let_in, path)))
}

fn dotted(path: &[&str]) -> String {
    path.join(".")
}

/// The strings in a setting that is a list of strings, or `None` when the
/// setting is absent.
pub fn string_list(text: &str, path: &[&str]) -> Result<Option<Vec<String>>> {
    let root = parse(text)?;
    let Some(value) = find(&root, path)? else {
        return Ok(None);
    };
    let ast::Expr::List(list) = value else {
        return Err(Error::NotAStringList(dotted(path)));
    };
    list.items()
        .map(|item| match item {
            ast::Expr::Str(string) => plain_string(&string).ok_or_else(|| Error::NotAStringList(dotted(path))),
            _ => Err(Error::NotAStringList(dotted(path))),
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

/// A setting that is a plain string, such as `networking.hostName`.
pub fn string_value(text: &str, path: &[&str]) -> Result<Option<String>> {
    let root = parse(text)?;
    Ok(match find(&root, path)? {
        Some(ast::Expr::Str(string)) => plain_string(&string),
        _ => None,
    })
}

/// A setting that is `true` or `false`, such as `nixpkgs.config.allowUnfree`.
pub fn bool_value(text: &str, path: &[&str]) -> Result<Option<bool>> {
    let root = parse(text)?;
    Ok(match find(&root, path)? {
        Some(ast::Expr::Ident(ident)) => match ident.ident_token().map(|t| t.text().to_owned()).as_deref() {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        },
        _ => None,
    })
}

/// Write a string as a Nix string literal.
pub fn nix_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn list_text(values: &[String]) -> String {
    if values.is_empty() {
        return "[ ]".to_owned();
    }
    let items: Vec<String> = values.iter().map(|v| nix_string(v)).collect();
    format!("[ {} ]", items.join(" "))
}

fn replace(text: &str, range: TextRange, with: &str) -> String {
    let (start, end) = (usize::from(range.start()), usize::from(range.end()));
    format!("{}{}{}", &text[..start], with, &text[end..])
}

/// The indentation of the line a position is on.
fn indent_at(text: &str, position: usize) -> &str {
    let line_start = text[..position].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[line_start..];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// Insert a new `name = value;` setting at the end of the module's set,
/// indented like the settings around it.
fn insert_setting(text: &str, root: &ast::Root, binding: &str) -> Result<String> {
    let set = module_set(root)?;
    let closing = set.r_curly_token().ok_or(Error::NotAModule)?;
    let close_at = usize::from(closing.text_range().start());
    let indent = match set.attrpath_values().last() {
        Some(last) => indent_at(text, usize::from(last.syntax().text_range().start())).to_owned(),
        None => format!("{}  ", indent_at(text, close_at)),
    };
    // Insert on a line of its own just before the closing brace.
    let before = text[..close_at].trim_end_matches([' ', '\t']);
    let line_start = before.len();
    let insertion = if before.ends_with('\n') {
        format!("{indent}{binding}\n{}", indent_at(text, close_at))
    } else {
        format!("\n{indent}{binding}\n{}", indent_at(text, close_at))
    };
    let new = format!("{}{}{}", &text[..line_start], insertion, &text[close_at..]);
    Ok(new)
}

/// Set a list-of-strings setting, adding it if it is absent.
pub fn set_string_list(text: &str, path: &[&str], values: &[String]) -> Result<String> {
    let root = parse(text)?;
    let new = match find(&root, path)? {
        Some(ast::Expr::List(list)) => {
            // Refuse to flatten anything but plain strings.
            string_list(text, path)?;
            replace(text, list.syntax().text_range(), &list_text(values))
        }
        Some(_) => return Err(Error::NotAStringList(dotted(path))),
        None => insert_setting(text, &root, &format!("{} = {};", dotted(path), list_text(values)))?,
    };
    parse(&new)?;
    Ok(new)
}

/// Add a path (such as `./yukimi.nix`) to a module's `imports`, adding the
/// setting if it is absent. Unchanged if it is already there.
pub fn add_import(text: &str, import: &str) -> Result<String> {
    let root = parse(text)?;
    let new = match find(&root, &["imports"])? {
        Some(ast::Expr::List(list)) => {
            if list.items().any(|item| item.syntax().text() == import) {
                return Ok(text.to_owned());
            }
            let closing = list
                .syntax()
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| t.kind() == SyntaxKind::TOKEN_R_BRACK)
                .ok_or_else(|| Error::NotAList("imports".into()))?;
            let at = usize::from(closing.text_range().start());
            let before = text[..at].trim_end_matches([' ', '\t']);
            if before.ends_with('\n') {
                // A list written one item per line.
                let item_indent = list
                    .items()
                    .last()
                    .map(|i| indent_at(text, usize::from(i.syntax().text_range().start())).to_owned())
                    .unwrap_or_else(|| format!("{}  ", indent_at(text, at)));
                format!("{before}{item_indent}{import}\n{}{}", indent_at(text, at), &text[at..])
            } else {
                format!("{before} {import} {}", &text[at..])
            }
        }
        Some(_) => return Err(Error::NotAList("imports".into())),
        None => insert_setting(text, &root, &format!("imports = [ {import} ];"))?,
    };
    parse(&new)?;
    Ok(new)
}

/// Whether a file's syntax tree has a node of this kind; for tests.
#[cfg(test)]
fn has(node: &rnix::SyntaxNode, kind: SyntaxKind) -> bool {
    node.descendants().any(|n| n.kind() == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTALLED: &str = r#"# Generated by the NixOS-focused Rust Calamares installer.
{ config, lib, pkgs, ... }: {
  # The calamares.* options come from the flake's `calamares` input.
  imports = [ ./hardware-configuration.nix ];
  calamares.installUser = "ref";
  calamares.applications = [ "firefox" "steam" ];
  networking.hostName = "acer";
  nixpkgs.config.allowUnfree = true;
  system.stateVersion = "26.11";
}
"#;

    #[test]
    fn reads_settings() {
        assert_eq!(
            string_list(INSTALLED, &["calamares", "applications"]).unwrap(),
            Some(vec!["firefox".to_owned(), "steam".to_owned()])
        );
        assert_eq!(string_value(INSTALLED, &["networking", "hostName"]).unwrap(), Some("acer".into()));
        assert_eq!(bool_value(INSTALLED, &["nixpkgs", "config", "allowUnfree"]).unwrap(), Some(true));
        assert_eq!(string_list(INSTALLED, &["environment", "systemPackages"]).unwrap(), None);
    }

    #[test]
    fn nested_settings_are_found() {
        let text = "{ ... }: { calamares = { applications = [ \"zed\" ]; }; networking = { hostName = \"x\"; }; }";
        assert_eq!(string_list(text, &["calamares", "applications"]).unwrap(), Some(vec!["zed".into()]));
        assert_eq!(string_value(text, &["networking", "hostName"]).unwrap(), Some("x".into()));
    }

    #[test]
    fn changes_only_the_list() {
        let new = set_string_list(
            INSTALLED,
            &["calamares", "applications"],
            &["firefox".into(), "steam".into(), "zed".into()],
        )
        .unwrap();
        assert_eq!(new, INSTALLED.replace(r#"[ "firefox" "steam" ]"#, r#"[ "firefox" "steam" "zed" ]"#));
    }

    #[test]
    fn adds_an_absent_setting_in_the_modules_style() {
        let new = set_string_list(INSTALLED, &["yukimi", "packages"], &["htop".into()]).unwrap();
        assert!(new.contains("  system.stateVersion = \"26.11\";\n  yukimi.packages = [ \"htop\" ];\n}\n"), "{new}");
        assert_eq!(string_list(&new, &["yukimi", "packages"]).unwrap(), Some(vec!["htop".into()]));
    }

    #[test]
    fn imports_grow_once() {
        let new = add_import(INSTALLED, "./yukimi.nix").unwrap();
        assert!(new.contains("imports = [ ./hardware-configuration.nix ./yukimi.nix ];"), "{new}");
        assert_eq!(add_import(&new, "./yukimi.nix").unwrap(), new);
        let tall = "{ ... }:\n{\n  imports = [\n    ./a.nix\n  ];\n}\n";
        assert_eq!(
            add_import(tall, "./yukimi.nix").unwrap(),
            "{ ... }:\n{\n  imports = [\n    ./a.nix\n    ./yukimi.nix\n  ];\n}\n"
        );
        let none = "{ ... }:\n{\n  boot.loader.grub.enable = true;\n}\n";
        assert_eq!(
            add_import(none, "./yukimi.nix").unwrap(),
            "{ ... }:\n{\n  boot.loader.grub.enable = true;\n  imports = [ ./yukimi.nix ];\n}\n"
        );
    }

    #[test]
    fn refuses_what_it_should_not_touch() {
        let computed = "{ pkgs, ... }: { calamares.applications = [ \"a\" ] ++ extra; }";
        assert_eq!(
            set_string_list(computed, &["calamares", "applications"], &[]),
            Err(Error::NotAStringList("calamares.applications".into()))
        );
        let interpolated = "{ ... }: { calamares.applications = [ \"${x}\" ]; }";
        assert!(string_list(interpolated, &["calamares", "applications"]).is_err());
        assert!(matches!(set_string_list("{ broken", &["a"], &[]), Err(Error::Syntax(_))));
        assert_eq!(string_list("42", &["a"]), Err(Error::NotAModule));
    }

    #[test]
    fn strings_are_escaped() {
        assert_eq!(nix_string(r#"a"b\c${d}$e"#), r#""a\"b\\c\${d}$e""#);
        let root = parse("{ x = \"a\\\"b\"; }").unwrap();
        assert!(has(root.syntax(), SyntaxKind::NODE_STRING));
    }
}
