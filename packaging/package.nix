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
    description = "High-performance Zsh enhancement engine (highlighting, completions, autosuggestions, history) in Rust";
    longDescription = ''
      zsh-pulse is a high-performance shell enhancement engine for Zsh,
      implemented in Rust as a native loadable module (zshpulse/core.so).

      It provides real-time incremental syntax highlighting, bounded
      filesystem completions, Fish-style inline autosuggestions,
      interactive history navigation, and typeahead completion preview
      integration.
    '';
    license = lib.licenses.mit;
    platforms = lib.platforms.unix;
    maintainers = [ lib.maintainers.johnrtitor ];
  };
}
