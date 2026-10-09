# SPDX-License-Identifier: MIT OR Apache-2.0
# Yukimi's NixOS module for systems without a flake:
#
#   imports = [
#     "${builtins.fetchTarball "https://github.com/bitemyapp/yukimi/archive/stable.tar.gz"}/nix/nixos.nix"
#   ];
#   programs.yukimi.enable = true;
import ./module.nix null
