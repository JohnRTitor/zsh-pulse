# The `zsh-pulse` Nix package.
#
# It builds the native zsh module and installs the zsh adapter beside it:
#
#   $out/lib/zshpulse/core.so         the module (`zmodload zshpulse/core`)
#   $out/share/zsh-pulse/zsh/…        the adapter (`init.zsh`, `widgets.zsh`, `completion.zsh`)
#   $out/share/zsh-pulse/init.zsh     Nix entrypoint (sets module path)
#
# The layout is required by zsh's `modname_ok()` (`zsh-src/Src/module.c`),
# which rejects an absolute path and a `.so` suffix in a `zmodload` name.
{
  lib,
  stdenv,
  rustPlatform,
  zsh,
  version ? "unstable",
}:
rustPlatform.buildRustPackage {
  pname = "zsh-pulse";
  inherit version;

  nativeBuildInputs = [ zsh ];

  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../../Cargo.toml
      ../../Cargo.lock
      ../../crates
      ../../zsh
      ../../Makefile
    ];
  };

  cargoLock.lockFile = ../../Cargo.lock;

  # We install the module at $out/lib/zshpulse/core.so (and adapter files)
  # ourselves; skip cargoInstallHook's default copy of cdylibs into $out/lib.
  dontCargoInstall = true;

  # The Makefile's default `install` target writes to /usr; install into $out
  # from postInstall instead.
  installPhase = ''
    runHook preInstall

    make install-files \
      DESTDIR="$out" \
      PREFIX=/ \
      LIBDIR=/lib \
      DATADIR=/share \
      MODULE_LIB_SUBDIR=zshpulse \
      CARGO_TARGET_DIR="target/${stdenv.hostPlatform.rust.cargoShortTarget}" \
      TARGET=release

    sed "s|@out@|$out|g" zsh/nix-init.zsh > "$out/share/zsh-pulse/init.zsh"

    # Pre-compile installed zsh scripts into wordcode (.zwc) so zsh can mmap
    # them directly on startup.
    make install-zcompile \
      DESTDIR="$out" \
      PREFIX=/ \
      DATADIR=/share \
      ZSH=zsh

    runHook postInstall
  '';

  # Integration tests drive a real zsh and need the module installed under a
  # module_path first (see bench/install.sh), so they are not run here.
  doCheck = false;

  meta = {
    description = "Fast, tiny, high-performance Zsh enhancement engine (highlighting, completions, autosuggestions, history) in Rust";
    longDescription = ''
      zsh-pulse is a fast, tiny, high-performance shell enhancement engine for Zsh,
      implemented in Rust as a native loadable module (zshpulse/core.so).

      It provides real-time incremental syntax highlighting, bounded
      filesystem completions, Fish-style inline autosuggestions,
      interactive history navigation, and typeahead completion preview
      integration.
    '';
    license = lib.licenses.mit;
    platforms = lib.platforms.unix;
    mainProgram = "zsh-pulse";
  };
}
