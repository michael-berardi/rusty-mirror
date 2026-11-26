#!/bin/sh
# Rusty Mirror installer for macOS and Linux:
#   curl -fsSL https://github.com/michael-berardi/rusty-mirror/releases/latest/download/install.sh | sh
# Downloads the release archive, checks its SHA-256 against SHA256SUMS, unpacks it into a
# per-tool folder and puts a small shim in ~/.local/bin. An existing shim/binary that this
# installer did not create is never replaced.
# Environment: RUSTY_MIRROR_HOME (default ~/.rusty-mirror), RUSTY_MIRROR_BIN_DIR (default
# ~/.local/bin), RUSTY_MIRROR_VERSION (default: latest, e.g. 0.3.0),
# RUSTY_MIRROR_RELEASE_BASE (override the download base URL, used by tests).
set -eu

REPO="michael-berardi/rusty-mirror"
MARKER='# rusty-mirror installer shim'
HOME_DIR="${RUSTY_MIRROR_HOME:-$HOME/.rusty-mirror}"
BIN_DIR="${RUSTY_MIRROR_BIN_DIR:-$HOME/.local/bin}"
case "$HOME_DIR" in /*) ;; *) HOME_DIR="$(pwd)/$HOME_DIR" ;; esac
case "$BIN_DIR" in /*) ;; *) BIN_DIR="$(pwd)/$BIN_DIR" ;; esac
if [ -n "${RUSTY_MIRROR_RELEASE_BASE:-}" ]; then BASE="$RUSTY_MIRROR_RELEASE_BASE"
elif [ -n "${RUSTY_MIRROR_VERSION:-}" ]; then BASE="https://github.com/$REPO/releases/download/v${RUSTY_MIRROR_VERSION#v}"
else BASE="https://github.com/$REPO/releases/latest/download"; fi
BASE="${BASE%/}"
SHIM="$BIN_DIR/rusty-mirror"

say() { printf '%s\n' "$*"; }
die() { printf 'rusty-mirror install: %s\n' "$*" >&2; exit 1; }

if command -v curl >/dev/null 2>&1; then fetch() { curl -fsSL --retry 3 -o "$2" "$1" || die "download failed: $1"; }
elif command -v wget >/dev/null 2>&1; then fetch() { wget -q -O "$2" "$1" || die "download failed: $1"; }
else die "needs curl or wget"; fi
command -v tar >/dev/null 2>&1 || die "needs tar"

if command -v sha256sum >/dev/null 2>&1; then sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else die "needs sha256sum or shasum"; fi

main() {
# Refuse to take over an install that belongs to something else, before touching any file.
if [ -e "$HOME_DIR" ] && [ ! -f "$HOME_DIR/.installed-by-rusty-mirror" ]; then
  die "$HOME_DIR exists but has no rusty-mirror installer marker; move it aside and retry"
fi
if [ -e "$SHIM" ] || [ -L "$SHIM" ]; then
  if [ ! -f "$SHIM" ] || ! grep -qF "$MARKER" "$SHIM" 2>/dev/null; then
    die "$SHIM exists and was not installed by this installer; move it aside and retry"
  fi
fi
if [ -e "$HOME_DIR/current" ] && [ ! -L "$HOME_DIR/current" ]; then
  die "$HOME_DIR/current is not a symlink; move it aside and retry"
fi

case "$(uname -s)" in
  Darwin) os=darwin ;;
  Linux) os=linux ;;
  *) die "unsupported system $(uname -s); Windows is not supported" ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) die "unsupported CPU $(uname -m)" ;;
esac
if [ "$os" = darwin ]; then asset="rusty-mirror-darwin-universal.tar.gz"
else asset="rusty-mirror-linux-$arch.tar.gz"; fi

TMP=$(mktemp -d "${TMPDIR:-/tmp}/rusty-mirror-install.XXXXXX") || die "cannot create a temporary folder"
trap 'rm -rf "$TMP"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# expect <file> <name> <sums-file>: the file's SHA-256 must equal the line for <name>.
expect() {
  want=$(awk -v n="$2" '{ f=$2; sub(/^\*/, "", f); if (f == n) print $1 }' "$3")
  [ -n "$want" ] || die "no checksum for $2 in $(basename "$3")"
  [ "$(sha256 "$1")" = "$want" ] || die "checksum mismatch for $2; nothing was installed"
}

say "Downloading Rusty Mirror ($asset)..."
fetch "$BASE/$asset" "$TMP/$asset"
fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS"
expect "$TMP/$asset" "$asset" "$TMP/SHA256SUMS"
mkdir "$TMP/src"
tar -xzf "$TMP/$asset" -C "$TMP/src" || die "cannot unpack the download"
[ -x "$TMP/src/rusty-mirror/rusty-mirror" ] && [ -f "$TMP/src/rusty-mirror/VERSION" ] || die "the download is not a Rusty Mirror release"
VERSION=$(tr -d '[:space:]' < "$TMP/src/rusty-mirror/VERSION")
printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || die "bad VERSION in the release"
if [ -n "${RUSTY_MIRROR_VERSION:-}" ] && [ "v${VERSION}" != "v${RUSTY_MIRROR_VERSION#v}" ]; then
  die "downloaded version $VERSION does not match RUSTY_MIRROR_VERSION"
fi
got=$("$TMP/src/rusty-mirror/rusty-mirror" --version 2>&1 </dev/null) || die "the downloaded CLI does not run here: $got"
case "$got" in *"$VERSION"*) ;; *) die "the downloaded CLI reports '$got', not $VERSION; nothing was installed" ;; esac

mkdir -p "$HOME_DIR/releases" "$BIN_DIR"
chmod 700 "$HOME_DIR" 2>/dev/null || true
printf '%s\n' 'Created by the rusty-mirror installer.' > "$HOME_DIR/.installed-by-rusty-mirror"
REL="$HOME_DIR/releases/$VERSION"
if [ -d "$REL" ]; then
  say "Rusty Mirror $VERSION is already unpacked; relinking."
else
  say "Installing Rusty Mirror $VERSION..."
  rm -rf "$REL.tmp"
  mv "$TMP/src/rusty-mirror" "$REL.tmp"
  mv "$REL.tmp" "$REL"
fi

# Atomic link swap: rename a fresh link over the old one (GNU mv -T, BSD/macOS mv -h).
swap_link() {
  ln -s "$1" "$2.tmp.$$"
  if mv -fT "$2.tmp.$$" "$2" 2>/dev/null || mv -fh "$2.tmp.$$" "$2" 2>/dev/null; then return 0; fi
  rm -f "$2" && mv "$2.tmp.$$" "$2" || { rm -f "$2.tmp.$$"; die "cannot update the link $2"; }
}
swap_link "$REL" "$HOME_DIR/current"

# The shim carries a marker so updates are allowed but foreign binaries are never clobbered.
cat > "$SHIM.tmp.$$" <<WRAP
#!/bin/sh
$MARKER
exec "$HOME_DIR/current/rusty-mirror" "\$@"
WRAP
chmod 755 "$SHIM.tmp.$$"
if ! mv -f "$SHIM.tmp.$$" "$SHIM"; then rm -f "$SHIM.tmp.$$"; die "cannot install the shim $SHIM"; fi
got=$("$SHIM" --version 2>&1 </dev/null) || die "the installed shim does not run: $got"
case "$got" in *"$VERSION"*) ;; *) die "the installed shim reports '$got', not $VERSION" ;; esac

say "Rusty Mirror $VERSION installed ($SHIM)."
case ":$PATH:" in *":$BIN_DIR:"*) ;; *) say "Add $BIN_DIR to your PATH:  export PATH=\"$BIN_DIR:\$PATH\"" ;; esac
}

main "$@"
