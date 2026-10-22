{
  description = "shell-pulse — incremental syntax highlighting for zsh, in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        shell-pulse = pkgs.callPackage ./nix { };
      in
      {
        packages = {
          default = shell-pulse;
          shell-pulse = shell-pulse;
        };

        devShells.default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.zsh pkgs.clang ];
        };
      });
}
