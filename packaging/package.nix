# The `zsh-pulse` Nix package.
#
# It builds the native zsh module and installs the zsh adapter beside it:
#
#   $out/lib/zshpulse/core.so        the module (`zmodload zshpulse/core`)
#   $out/share/zsh-pulse/zsh/…        the adapter (`init.zsh`, `widgets.zsh`)
#
# The layout is not a choice: zsh's `modname_ok()`
# (`zsh-src/Src/module.c:2177-2186`) rejects an absolute path and a `.so`
# suffix in a `zmodload` name, so the module has to be
# `zshpulse/core.so` inside a directory on `$module_path`.
{
  lib,
  rustPlatform,
  zsh,
  version ? "unstable",
}:
rustPlatform.buildRustPackage {
  pname = "zsh-pulse";
  inherit version;

  nativeBuildInputs = [ zsh ];

  # Only the sources: the checkout also holds `target/`, the research
  # material and the benchmarks, none of which belong in the build.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../zsh
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  # The module is a `cdylib` that resolves zsh's symbols at load time with
  # `dlsym`, so it needs no zsh headers and no zsh to link against.

  # `cargo test` is not run here: the integration tests drive a real zsh and
  # need the module installed under a `module_path` first, which is what
  # `bench/install.sh` does for a checkout.
  doCheck = false;

  postInstall = ''
    # zsh wants `zshpulse/core.so`; cargo produced
    # `libzshpulse_core.so`.
    mkdir -p "$out/lib/zshpulse"
    mv "$out/lib/libzshpulse_core.so" "$out/lib/zshpulse/core.so"

    # The adapter. `init.zsh` loads the module and sources `widgets.zsh`,
    # which installs the `zle-line-pre-redraw` hook.
    install -Dm644 zsh/init.zsh "$out/share/zsh-pulse/zsh/init.zsh"
    install -Dm644 zsh/widgets.zsh "$out/share/zsh-pulse/zsh/widgets.zsh"
    install -Dm644 zsh/completion.zsh "$out/share/zsh-pulse/zsh/completion.zsh"

    # A one-liner for a `.zshrc`: it points the adapter at the module that
    # was just built, because the adapter's own default path is relative to
    # the checkout it came from.
    sed "s|@out@|$out|g" zsh/nix-init.zsh > "$out/share/zsh-pulse/init.zsh"

    # Pre-compile all installed zsh scripts into wordcode (.zwc) using zcompile.
    # This strips comment overhead, eliminates parsing latency on shell startup,
    # and allows zsh to memory-map (mmap) the compiled bytecode directly.
    zsh -c "zcompile '$out/share/zsh-pulse/init.zsh'"
    zsh -c "zcompile '$out/share/zsh-pulse/zsh/init.zsh'"
    zsh -c "zcompile '$out/share/zsh-pulse/zsh/widgets.zsh'"
    zsh -c "zcompile '$out/share/zsh-pulse/zsh/completion.zsh'"
  '';

  meta = {
    description = "Incremental syntax highlighting for zsh, in Rust";
    longDescription = ''
      zsh-pulse loads a small native module into zsh and highlights the
      command line as it is typed, through zsh's `region_highlight`
      parameter. The lexer is pure Rust and re-lexes only the part of the
      line that changed.
    '';
    license = lib.licenses.mit;
    platforms = lib.platforms.unix;
    mainProgram = "zsh-pulse";
  };
}
