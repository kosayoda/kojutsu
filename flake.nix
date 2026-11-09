{
  description = "Terminal UI for Jujutsu";

  # jj-lib needs a recent rustc, which a stable nixpkgs release may not have.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        kojutsu = pkgs.callPackage ./nix/package.nix { };
        default = kojutsu;
      });
    };
}
