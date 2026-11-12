# Arch Linux packaging for zsh-pulse

Community-maintained PKGBUILD, following the style of the `ananicy-rs` packaging
recipes.

## Build

```sh
cd packaging/archlinux
makepkg -f
```

The recipe builds from the release tag (`v$pkgver`) and runs `cargo fetch`,
`cargo build --release --frozen`, and `cargo test --frozen` before installing
with `make install`.
