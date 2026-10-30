{
  description = "zsh-pulse — incremental syntax highlighting for zsh, in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        zsh-pulse = pkgs.callPackage ./contrib/package.nix { };
      in
      {
        packages = {
          default = zsh-pulse;
          zsh-pulse = zsh-pulse;
        };

        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.zsh
            pkgs.clang
          ];
        };
      }
    );
}
