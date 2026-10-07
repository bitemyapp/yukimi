// SPDX-License-Identifier: MIT OR Apache-2.0
//! NixOS configuration, read and changed the way a careful person would.
//!
//! - [`edit`]: find a setting in a Nix file (`calamares.applications`,
//!   `networking.hostName`, `imports`) and change only that setting. The
//!   rest of the file, its comments and formatting, stays byte for byte, and
//!   every change is parsed again before it is returned.
//! - [`packages`]: the file in which Yukimi keeps the packages installed for
//!   everyone, `yukimi.nix`.
//! - [`lock`]: `flake.lock`: which revision of each input a system is built
//!   from, and how old it is.
pub mod edit;
pub mod lock;
pub mod packages;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("the file does not parse as Nix: {0}")]
    Syntax(String),
    #[error("the file is not a NixOS module (a set of settings, or a function returning one)")]
    NotAModule,
    #[error("`{0}` is set to something other than a plain list of strings, so it is left alone")]
    NotAStringList(String),
    #[error("`{0}` is set to something other than a plain list, so it is left alone")]
    NotAList(String),
    #[error("not a valid attribute name in Nixpkgs: {0:?}")]
    BadAttribute(String),
    #[error("flake.lock could not be read: {0}")]
    Lock(String),
}

pub type Result<T> = std::result::Result<T, Error>;
