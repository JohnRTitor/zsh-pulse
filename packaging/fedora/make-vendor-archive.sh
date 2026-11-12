#!/bin/sh
# Build the Source1 that packaging/fedora/zsh-pulse.spec expects.
#
# Usage:
#   packaging/fedora/make-vendor-archive.sh [OUTPUT]
#
# OUTPUT defaults to packaging/fedora/zsh-pulse-<version>-vendor.tar.xz.

set -eu

srcdir=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$srcdir"

version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
if [ -z "$version" ]; then
    echo "make-vendor-archive: no [workspace.package] version in Cargo.toml" >&2
    exit 1
fi

output=${1:-"$srcdir/packaging/fedora/zsh-pulse-$version-vendor.tar.xz"}

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT INT HUP TERM

# When there are no crates.io dependencies, `cargo vendor` prints a message
# and does not create the output directory, but still exits successfully.
# Ensure the expected layout exists so packaging always gets a valid archive.
mkdir -p "$workdir/vendor"
if ! cargo vendor --locked --versioned-dirs "$workdir/vendor" >/dev/null; then
    echo "make-vendor-archive: cargo vendor failed" >&2
    exit 1
fi

cat >"$workdir/vendor-config.toml" <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "@VENDOR_DIR@"
EOF

mkdir -p "$(dirname -- "$output")"
tar --create --xz --file="$output" \
    --directory="$workdir" \
    --sort=name --owner=0 --group=0 --numeric-owner \
    --mtime="@${SOURCE_DATE_EPOCH:-0}" \
    vendor vendor-config.toml

echo "wrote $output"
