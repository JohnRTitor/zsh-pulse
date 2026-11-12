# Fedora packaging recipe for zsh-pulse.
#
# Community-maintained recipe, not a Fedora package. See packaging/fedora/README.md.
# Keep Version in step with Cargo.toml's [workspace.package] table.

Name:           zsh-pulse
Version:        0.1.0
Release:        1%{?dist}
Summary:        High-performance Zsh enhancement module written in Rust

License:        MIT
URL:            https://github.com/JohnRTitor/zsh-pulse
Source0:        %{url}/releases/download/v%{version}/%{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo
BuildRequires:  rust >= 1.85
BuildRequires:  make
BuildRequires:  zsh

# The module is a cdylib loaded by Zsh at runtime. No explicit Requires are
# added beyond what rpm derives; Zsh is needed to use it.
Requires:       zsh

%description
zsh-pulse is a high-performance shell enhancement engine for Zsh, implemented
in Rust as a native loadable module (zshpulse/core.so).

It provides real-time incremental syntax highlighting, bounded filesystem
completions, Fish-style inline autosuggestions, interactive history
navigation, and typeahead completion preview integration.

%prep
%autosetup -n %{name}-%{version}

if ! grep -qx 'version = "%{version}"' Cargo.toml; then
    : 'Version: does not match [workspace.package] version in Cargo.toml'
    grep -n '^version = ' Cargo.toml || :
    exit 1
fi

%{__xz} -dc %{SOURCE1} | %{__tar} -x -C %{_builddir}

install -d %{_builddir}/cargo-home
sed -e 's|@VENDOR_DIR@|%{_builddir}/vendor|' \
    %{_builddir}/vendor-config.toml > %{_builddir}/cargo-home/config.toml

%build
CARGO_HOME=%{_builddir}/cargo-home \
    cargo build --release --locked --offline --verbose --workspace

%install
make install DESTDIR=%{buildroot} PREFIX=%{_prefix} LIBDIR=%{_libdir}

%check
CARGO_HOME=%{_builddir}/cargo-home \
    cargo test --locked --offline --workspace

%files
%license LICENSE
%{_libdir}/zsh-pulse/zshpulse/core.so
%{_datadir}/zsh-pulse/init.zsh
%{_datadir}/zsh-pulse/zsh/init.zsh
%{_datadir}/zsh-pulse/zsh/widgets.zsh
%{_datadir}/zsh-pulse/zsh/completion.zsh

%changelog
* Thu Nov 12 2026 zsh-pulse packagers <zsh-pulse-packaging@example.invalid> - 0.1.0-1
- Initial community packaging recipe, built from the v0.1.0 release tarball with
  Cargo dependencies vendored for a network-isolated build.
