# SPDX-License-Identifier: MIT OR Apache-2.0
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.yukimi;
in
{
  options.programs.yukimi = {
    enable = lib.mkEnableOption "Yukimi, the window onto what is installed and available";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.yukimi;
      defaultText = lib.literalExpression "yukimi.packages.\${system}.yukimi";
      description = "The Yukimi package. Its polkit policy names its own helper.";
    };
  };
  config = lib.mkIf cfg.enable {
    # The package carries its desktop entry, icon and polkit action, which
    # the system profile links into place.
    environment.systemPackages = [ cfg.package ];
    security.polkit.enable = true;
  };
}
