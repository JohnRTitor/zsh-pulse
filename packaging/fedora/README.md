# Fedora packaging for zsh-pulse

This directory contains a community-maintained RPM recipe for building
`zsh-pulse` in a network-isolated environment with vendored Cargo dependencies,
following the style of the `ananicy-rs` packaging recipes.

## Files

- `zsh-pulse.spec`: RPM spec file.
- `make-vendor-archive.sh`: generates `zsh-pulse-<version>-vendor.tar.xz` (Source1)
  from `Cargo.lock`.

## Local build outline

1. Create the source tarball from the release tag (or `git archive` of HEAD for
   testing).
2. Generate the vendor archive:
   ```sh
   ./packaging/fedora/make-vendor-archive.sh
   ```
3. Place `zsh-pulse-<version>.tar.gz` and the vendor archive in your rpmbuild
   `SOURCES` directory, then build:
   ```sh
   rpmbuild -ba packaging/fedora/zsh-pulse.spec
   ```

The package installs the native module to `%{_libdir}/zsh-pulse/zshpulse/core.so`
and the Zsh adapter scripts under `%{_datadir}/zsh-pulse/`. Users can load it by
sourcing `%{_datadir}/zsh-pulse/init.zsh` from their `.zshrc`.
