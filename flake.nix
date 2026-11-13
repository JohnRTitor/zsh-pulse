{
  description = "zsh-pulse — high-performance Zsh enhancement engine (highlighting, completions, autosuggestions, history) in Rust";

  inputs = {
    nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.zst";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    flake-compat = {
      url = "github:NixOS/flake-compat";
      flake = false;
    };
  };

  outputs =
    inputs@{ self, ... }:
    let
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);

      rev =
        self.shortRev or self.dirtyShortRev or (inputs.nixpkgs.lib.substring 0 8 self.lastModifiedDate);
      version = "${cargoToml.workspace.package.version}+${rev}";
    in
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      perSystem =
        {
          config,
          pkgs,
          ...
        }:
        {
          formatter = pkgs.nixfmt;

          packages = {
            default = pkgs.callPackage ./packaging/package.nix { inherit version; };
            zsh-pulse = config.packages.default;
          };

          checks = {
            zsh-pulse = config.packages.default;
          };

          devShells.default = pkgs.mkShell {
            inputsFrom = [ config.packages.default ];
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              zsh
              clang
            ];
          };
        };
    };
}
