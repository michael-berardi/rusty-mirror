#!/usr/bin/env bash
# Builds the download assets for the current commit into <out> (default dist/release):
#   rusty-mirror-darwin-universal.tar.gz  Developer ID signed, notarized universal CLI
#   rusty-mirror-linux-<arch>.tar.gz      CLI for each Linux target that builds cleanly here
#   install.sh                            the POSIX installer
#   SHA256SUMS                            checksums of all of the above
# Usage: RUSTY_MIRROR_SIGNING_IDENTITY='Developer ID Application: ...' \
#        NOTARY_PROFILE=<notarytool keychain profile> scripts/build-release.sh [out-dir]
# No identity or profile name is stored in the repository. Run from a clean checkout, on a Mac.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-dist/release}"

[ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "build-release: the working tree has changes; build from a clean checkout" >&2; exit 1; }
[ "$(uname -s)" = Darwin ] || { echo "build-release: macOS is required for the signed, notarized binary" >&2; exit 1; }
IDENTITY="${RUSTY_MIRROR_SIGNING_IDENTITY:?Set RUSTY_MIRROR_SIGNING_IDENTITY to a Developer ID Application identity}"
PROFILE="${NOTARY_PROFILE:?Set NOTARY_PROFILE to a notarytool keychain profile}"
for tool in cargo rustup lipo codesign strip zip xcrun tar; do
  command -v "$tool" >/dev/null || { echo "build-release: $tool is unavailable" >&2; exit 1; }
done
if command -v sha256sum >/dev/null 2>&1; then sha_cmd=sha256sum; elif command -v shasum >/dev/null 2>&1; then sha_cmd=shasum; else echo "build-release: needs sha256sum or shasum" >&2; exit 1; fi

VERSION=$(awk '/^version = / { gsub(/"/, "", $3); print $3; exit }' Cargo.toml)
[ -n "$VERSION" ] || { echo "build-release: cannot read the workspace version" >&2; exit 1; }
printf '%s\n' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { echo "build-release: bad workspace version $VERSION" >&2; exit 1; }
COMMIT=$(git rev-parse HEAD)

rm -rf "$OUT"
mkdir -p "$OUT"
BUILD="$PWD/.release-build"
rm -rf "$BUILD"
mkdir -p "$BUILD"
trap 'rm -rf "$BUILD"' EXIT

stage_files() { # stage_files <stage-dir> <built-binary>
  local stage="$1" binary="$2"
  mkdir -p "$stage/rusty-mirror"
  cp "$binary" "$stage/rusty-mirror/rusty-mirror"
  chmod 755 "$stage/rusty-mirror/rusty-mirror"
  cp README.md LICENSE CHANGELOG.md "$stage/rusty-mirror/"
  printf '%s\n' "$VERSION" > "$stage/rusty-mirror/VERSION"
}

pack() { # pack <archive> <stage-dir>
  local dest="$1" stage="$2" list="$2.list"
  ( cd "$stage" && find rusty-mirror -mindepth 1 | LC_ALL=C sort > "$list" )
  if tar --version 2>/dev/null | grep -qi gnu; then
    tar -C "$stage" --owner=0 --group=0 --numeric-owner --mtime=@0 --no-recursion -czf "$dest" -T "$list"
  else
    COPYFILE_DISABLE=1 tar -C "$stage" --uid 0 --gid 0 --uname root --gname root --no-xattrs --no-recursion -czf "$dest" -T "$list"
  fi
  rm -f "$list"
}

echo "build-release: $VERSION at $COMMIT"

# macOS: two architectures, one signed universal binary, notarized from a zip of the staged files.
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  rustup target list --installed | grep -qx "$target" || { echo "build-release: missing Rust target $target" >&2; exit 1; }
  CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" cargo build --release --locked -p rusty-mirror-cli --target "$target"
done
lipo -create \
  "$BUILD/../target/aarch64-apple-darwin/release/rusty-mirror" \
  "$BUILD/../target/x86_64-apple-darwin/release/rusty-mirror" \
  -output "$BUILD/rusty-mirror"
strip -x "$BUILD/rusty-mirror"
codesign --force --sign "$IDENTITY" --options runtime --timestamp "$BUILD/rusty-mirror"
codesign --verify --strict --verbose=2 "$BUILD/rusty-mirror"
MAC_STAGE="$BUILD/macos-stage"
stage_files "$MAC_STAGE" "$BUILD/rusty-mirror"
NOTARY_ZIP="$BUILD/rusty-mirror-notarize.zip"
( cd "$MAC_STAGE" && zip -qry "$NOTARY_ZIP" rusty-mirror )
xcrun notarytool submit "$NOTARY_ZIP" --keychain-profile "$PROFILE" --wait
xcrun stapler staple "$MAC_STAGE/rusty-mirror/rusty-mirror" || echo "WARN: flat binary ticket verifies online"
spctl -a -t install -vv "$MAC_STAGE/rusty-mirror/rusty-mirror"
pack "$OUT/rusty-mirror-darwin-universal.tar.gz" "$MAC_STAGE"

# Linux: only ship targets that are installed here and build cleanly with the existing cross linker.
for spec in "x86_64:x86_64-unknown-linux-gnu:x86_64-linux-gnu-gcc" "aarch64:aarch64-unknown-linux-gnu:aarch64-linux-gnu-gcc"; do
  arch="${spec%%:*}" ; rest="${spec#*:}" ; target="${rest%%:*}" ; linker="${rest#*:}"
  if ! rustup target list --installed | grep -qx "$target"; then echo "build-release: skipping Linux $arch: target $target is not installed"; continue; fi
  if ! command -v "$linker" >/dev/null 2>&1; then echo "build-release: skipping Linux $arch: $linker is not installed"; continue; fi
  if ! env "CARGO_TARGET_$(printf '%s' "$target" | tr 'a-z-' 'A-Z_')_LINKER=$linker" CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
      cargo build --release --locked -p rusty-mirror-cli --target "$target"; then
    echo "build-release: skipping Linux $arch: the build did not succeed with this toolchain" >&2
    continue
  fi
  binary="target/$target/release/rusty-mirror"
  file "$binary" | grep -q 'ELF' || { echo "build-release: skipping Linux $arch: $binary is not an ELF binary" >&2; continue; }
  LINUX_STAGE="$BUILD/linux-$arch-stage"
  stage_files "$LINUX_STAGE" "$binary"
  pack "$OUT/rusty-mirror-linux-$arch.tar.gz" "$LINUX_STAGE"
done

cp scripts/install.sh "$OUT/install.sh"
( cd "$OUT" && if [ "$sha_cmd" = sha256sum ]; then sha256sum *.tar.gz install.sh > SHA256SUMS; else shasum -a 256 *.tar.gz install.sh > SHA256SUMS; fi )
( cd "$OUT" && if [ "$sha_cmd" = sha256sum ]; then sha256sum -c SHA256SUMS; else shasum -a 256 -c SHA256SUMS; fi )
echo "build-release: assets in $OUT"
ls -l "$OUT"
