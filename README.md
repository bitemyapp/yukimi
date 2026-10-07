# Yukimi 雪見

*Snow-viewing for NixOS.*

A yukimi-shōji is a paper screen with a pane of glass set low in it, so you
can sit in a warm room and watch the snow fall in the garden. Yukimi is that
window onto a NixOS system. You can see what is installed and why, find what
is available, and change it without opening a text editor.

Nix can do far more than any GUI can show. Yukimi doesn't try to show all of
it. It covers the everyday 80%: installing and removing software, updating,
going back to an earlier version and freeing disk space. Along the way it
explains what Nix is doing, so the system makes sense to you instead of being
something you edit and hope.

## What it shows

- **Overview.** The system at a glance: its NixOS version and kernel, how
  many packages it has, its generations and disk use, and what changed most
  recently.
- **Installed.** Everything installed, in four groups: applications chosen
  in the installer, packages added for everyone, packages installed just for
  you, and everything else the system needs. You can remove anything Yukimi
  can safely remove.
- **Discover.** Search every top-level package in nixpkgs. The index is built
  once per nixpkgs version, and results are ranked by name first, then by
  description. You can try a package without installing it, install it just
  for you (no password), or install it for everyone. Packages that are broken
  or unfree are labelled before you pick them.
- **Updates.** Where the system comes from: each flake input, the exact
  revision it is pinned to, how old that is, and a link to see it. *Check for
  updates* asks every source for its newest version without changing
  anything, then marks each input as up to date, newer, or *would go back*
  (when what `flake.nix` asks for now points somewhere older than your
  lock). Yukimi updates only the inputs that move forward, now or at the
  next restart.
- **History.** Every generation you can go back to. Open one to see what
  changed from the one before: upgrades, downgrades, additions and removals,
  with sizes. Going back to one also brings back the choices it was built
  from, so your next change starts from there.
- **Storage.** The Nix store split into what the running system needs, older
  generations, user profiles, projects and dev shells, and garbage. It lists
  the heaviest packages and every garbage-collector root, and can clean up
  with one button.

## How it works

Everything is Rust, including the parts that read Nix's own data:

| Crate | What it does |
| --- | --- |
| `yukimi-store` | Reads the Nix store database (`/nix/var/nix/db/db.sqlite`) read-only. Handles store paths and name/version splitting, follows Nix's own `compareVersions`, builds the reference graph (closures, sizes, *why is this here*) and finds GC roots the same way the garbage collector does. |
| `yukimi-config` | Edits Nix files without disturbing them, using a lossless [rnix](https://github.com/nix-community/rnix-parser) syntax tree: string lists, imports, and `flake.lock`. |
| `yukimi-system` | Reads system generations, profiles and the installer's catalog. Builds the package index, diffs closures, and turns Nix's `internal-json` log into progress. |
| `yukimi` | The GTK 4 and libadwaita app. |
| `yukimi-helper` | The only part that runs as root, started through `pkexec`. It changes `/etc/nixos`, builds, switches, rolls back and collects garbage. If anything fails, or you press Stop, it puts the configuration back as it was. Once the new system starts switching it finishes, so the switch is never cut off halfway. |

Yukimi never hand-edits your `configuration.nix`. Packages added for everyone
go in their own file, `/etc/nixos/yukimi.nix`, which `configuration.nix`
imports. Applications chosen in the
[installer](https://github.com/bitemyapp/determinate-nixos-graphical) go
back into the same `calamares.applications` list the installer wrote. A
change is built first and switched to only if the build succeeds, so a
failed build leaves the running system untouched. The helper keeps a copy of
the configuration behind each generation in `/var/lib/yukimi/configurations`,
which is how going back restores your choices too.

## Building

```sh
nix build github:bitemyapp/yukimi
nix develop   # cargo build, cargo test, cargo clippy
```

On NixOS, add the flake as an input and import its module:

```nix
{
  inputs.yukimi.url = "github:bitemyapp/yukimi";
  outputs = { nixpkgs, yukimi, ... }: {
    nixosConfigurations.host = nixpkgs.lib.nixosSystem {
      modules = [
        yukimi.nixosModules.default
        { programs.yukimi.enable = true; }
      ];
    };
  };
}
```

The module installs the app with its polkit policy, so the password prompt
says what Yukimi is asking for. NixOS restarts polkit whenever the installed
packages change, so each change asks for the password again.

## Licence

MIT or Apache-2.0, at your option.
