{
  description = "Yukimi: snow-viewing for NixOS. See what's installed, find what's available, and change it without editing config.";
  inputs.nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1";
  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in
    {
      packages.${system} = {
        yukimi = pkgs.callPackage ./nix/package.nix { };
        default = self.packages.${system}.yukimi;
      };
      devShells.${system}.default = pkgs.mkShell {
        inputsFrom = [ self.packages.${system}.yukimi ];
        packages = [
          pkgs.clippy
          pkgs.rustfmt
          pkgs.rust-analyzer
        ];
      };
      nixosModules.default = import ./nix/module.nix self;
      checks.${system}.yukimi = self.packages.${system}.yukimi;
      formatter.${system} = pkgs.nixfmt;
    };
}
