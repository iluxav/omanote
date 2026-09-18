#!/bin/sh
# omanote installer.
#
#   curl -fsSL https://raw.githubusercontent.com/iluxav/omanote/main/install.sh | sh
#
# Downloads the latest release binary for this machine, checks its checksum and
# puts it in ~/.local/bin. Options, as environment variables:
#
#   OMANOTE_VERSION=v0.1.0       a specific release instead of the latest
#   OMANOTE_INSTALL_DIR=/usr/local/bin
#   OMANOTE_REPO=owner/repo      a fork
#
# To remove it again:  … | sh -s -- --uninstall     (your notes in ~/.omanote stay)
set -eu

REPO="${OMANOTE_REPO:-iluxav/omanote}"
VERSION="${OMANOTE_VERSION:-latest}"
DIR="${OMANOTE_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$DIR/omanote"
    say "Removed $DIR/omanote (your notes in ~/.omanote are untouched)"
    exit 0
fi

case "$(uname -s)" in
    Linux) os=unknown-linux-musl ;;
    Darwin) os=apple-darwin ;;
    *) die "unsupported system: $(uname -s) (Linux and macOS only)" ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "unsupported processor: $(uname -m)" ;;
esac
asset="omanote-$arch-$os.tar.gz"

if [ "$VERSION" = latest ]; then
    base="https://github.com/$REPO/releases/latest/download"
else
    base="https://github.com/$REPO/releases/download/$VERSION"
fi
# For testing against a local server.
base="${OMANOTE_BASE_URL:-$base}"

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q "$1" -O "$2"; }
else
    die "need curl or wget"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading $asset ($VERSION) from $REPO…"
fetch "$base/$asset" "$tmp/$asset" || die "could not download $base/$asset — is there a release for this platform?"

if fetch "$base/checksums.txt" "$tmp/checksums.txt" 2>/dev/null; then
    want=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/checksums.txt")
    if command -v sha256sum >/dev/null 2>&1; then
        got=$(sha256sum "$tmp/$asset" | awk '{ print $1 }')
    elif command -v shasum >/dev/null 2>&1; then
        got=$(shasum -a 256 "$tmp/$asset" | awk '{ print $1 }')
    else
        got=""
    fi
    if [ -z "$want" ] || [ -z "$got" ]; then
        say "Warning: could not verify the checksum, continuing."
    elif [ "$want" != "$got" ]; then
        die "checksum mismatch for $asset — not installing"
    else
        say "Checksum OK."
    fi
else
    say "Warning: this release has no checksums.txt, continuing without verification."
fi

tar -xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/omanote" ] || die "the archive did not contain the omanote binary"
mkdir -p "$DIR"
# Move into place rather than overwrite, so a running omanote keeps working.
chmod 755 "$tmp/omanote"
mv -f "$tmp/omanote" "$DIR/omanote"

say "Installed $DIR/omanote"
case ":$PATH:" in
    *":$DIR:"*) say "Run it from anywhere:  omanote --help" ;;
    *)
        say "Note: $DIR is not on your PATH. Add this to your shell profile:"
        say "  export PATH=\"$DIR:\$PATH\""
        ;;
esac
