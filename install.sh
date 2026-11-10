#!/bin/sh
# drawer installer
#
#   curl -fsSL https://nakulv.com/drawer/install.sh | sh
#
# Downloads the latest prebuilt binary for your OS/CPU from GitHub Releases,
# verifies its SHA-256 checksum, and installs it to ~/.local/bin (no sudo).
# Falls back to building from source with cargo when no prebuilt binary fits.
#
# Options (environment variables):
#   DRAWER_INSTALL_DIR   where to put the binary   (default: ~/.local/bin)
#   DRAWER_VERSION       a tag like v0.1.0          (default: latest release)

set -eu

REPO="nvrma/drawer"
BIN="drawer"
INSTALL_DIR="${DRAWER_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${DRAWER_VERSION:-latest}"

if [ -t 1 ]; then
    bold=$(printf '\033[1m'); dim=$(printf '\033[2m'); green=$(printf '\033[32m')
    yellow=$(printf '\033[33m'); red=$(printf '\033[31m'); reset=$(printf '\033[0m')
else
    bold=""; dim=""; green=""; yellow=""; red=""; reset=""
fi

say()  { printf '%s\n' "$*"; }
step() { printf '%s›%s %s\n' "$dim" "$reset" "$*"; }
warn() { printf '%s!%s %s\n' "$yellow" "$reset" "$*" >&2; }
die()  { printf '%s✗%s %s\n' "$red" "$reset" "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1; }

download() { # url dest
    if need curl; then
        curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
    elif need wget; then
        wget -q --https-only -O "$2" "$1"
    else
        die "need curl or wget to download drawer"
    fi
}

sha256_of() {
    if need sha256sum; then sha256sum "$1" | cut -d' ' -f1
    elif need shasum; then shasum -a 256 "$1" | cut -d' ' -f1
    else die "need sha256sum or shasum to verify the download"
    fi
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$arch" in
        x86_64 | amd64) arch="x86_64" ;;
        aarch64 | arm64) arch="aarch64" ;;
        *) return 1 ;;
    esac
    case "$os" in
        Linux) echo "${arch}-unknown-linux-musl" ;;
        Darwin) echo "${arch}-apple-darwin" ;;
        *) return 1 ;;
    esac
}

build_from_source() {
    if ! need cargo; then
        die "no prebuilt binary for $(uname -s)/$(uname -m), and cargo isn't installed.
  Install Rust from https://rustup.rs and run this again."
    fi
    step "building from source with cargo (this takes a minute)"
    if [ "$VERSION" = "latest" ]; then
        cargo install --locked --git "https://github.com/$REPO" --root "$tmp/root" "$BIN"
    else
        cargo install --locked --git "https://github.com/$REPO" --tag "$VERSION" --root "$tmp/root" "$BIN"
    fi
    mkdir -p "$INSTALL_DIR"
    install -m 755 "$tmp/root/bin/$BIN" "$INSTALL_DIR/$BIN"
}

main() {
    say ""
    say "  ${bold}drawer${reset} ${dim}— your SSH machines, shell and files in one screen${reset}"
    say ""

    tmp=$(mktemp -d 2>/dev/null || mktemp -d -t drawer)
    trap 'rm -rf "$tmp"' EXIT INT TERM

    if target=$(detect_target); then
        if [ "$VERSION" = "latest" ]; then
            base="https://github.com/$REPO/releases/latest/download"
        else
            base="https://github.com/$REPO/releases/download/$VERSION"
        fi
        archive="$BIN-$target.tar.gz"
        step "downloading $archive"
        if download "$base/$archive" "$tmp/$archive" && download "$base/$archive.sha256" "$tmp/$archive.sha256"; then
            expected=$(cut -d' ' -f1 < "$tmp/$archive.sha256" | tr -d '\r\n')
            actual=$(sha256_of "$tmp/$archive")
            [ -n "$expected" ] && [ "$expected" = "$actual" ] || die "checksum mismatch for $archive; not installing"
            step "checksum ok ${dim}($actual)${reset}"
            tar -xzf "$tmp/$archive" -C "$tmp"
            [ -f "$tmp/$BIN" ] || die "archive did not contain $BIN"
            mkdir -p "$INSTALL_DIR"
            install -m 755 "$tmp/$BIN" "$INSTALL_DIR/$BIN"
        else
            warn "no prebuilt binary found for $target"
            build_from_source
        fi
    else
        warn "no prebuilt binary for $(uname -s)/$(uname -m)"
        build_from_source
    fi

    say ""
    say "  ${green}✓${reset} installed ${bold}$("$INSTALL_DIR/$BIN" --version 2>/dev/null || echo "$BIN")${reset} to $INSTALL_DIR/$BIN"

    if ! need ssh; then
        warn "drawer uses your OpenSSH client, and 'ssh' isn't on your PATH. Install openssh."
    fi
    case ":$PATH:" in
        *":$INSTALL_DIR:"*) say "  run ${bold}drawer${reset} to start" ;;
        *)
            warn "$INSTALL_DIR is not on your PATH. Add this to your shell profile:"
            say "      export PATH=\"$INSTALL_DIR:\$PATH\""
            ;;
    esac
    say ""
}

main "$@"
