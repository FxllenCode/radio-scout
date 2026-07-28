#!/bin/sh
# Radio-Scout installer (#23, spec US 42, ADR-0007).
#
#   curl -fsSL https://raw.githubusercontent.com/FxllenCode/radio-scout/master/install.sh | sh
#
# Works out which prebuilt binary this machine wants, fetches it from the
# GitHub release, checks it against the release's published SHA-256, and puts
# one file on the PATH. There is nothing else to install: the frontend is
# embedded in the binary and first run creates its own database and audio store.
#
# Two things it deliberately does that a one-liner installer usually doesn't:
#
#   * It verifies. Piping a download straight into a shell is bad enough; running
#     an unverified binary afterwards is worse. A checksum that does not match
#     stops the install with nothing written.
#   * It has a dry run. `--dry-run` prints exactly what it would fetch and where
#     it would put it, and touches nothing — so "never pipe a stranger's script
#     into your shell" has an answer other than "trust me".
#
# POSIX sh, no bashisms: a Raspberry Pi OS image, an Alpine container and a Mac
# all have to run this unchanged.

set -eu

REPO="FxllenCode/radio-scout"
BIN="radio-scout"

VERSION=""
TARGET=""
DIR=""
BASE_URL=""
DRY_RUN="no"

usage() {
	cat <<EOF
Install $BIN.

Usage: install.sh [options]

Options:
  --version <tag>   Release to install (default: the latest one).
  --target <triple> Force a target, e.g. aarch64-unknown-linux-musl.
  --dir <path>      Where to put the binary (default: /usr/local/bin, or
                    \$HOME/.local/bin when that is not writable).
  --base-url <url>  Where the release assets are (default: the GitHub release).
  --dry-run         Say what would happen; change nothing.
  -h, --help        This.
EOF
}

say() { printf '%s\n' "$*"; }
die() {
	printf 'error: %s\n' "$*" >&2
	exit 1
}

while [ $# -gt 0 ]; do
	case "$1" in
	--version)
		VERSION="${2:-}"
		shift 2 || die "--version needs a value"
		;;
	--target)
		TARGET="${2:-}"
		shift 2 || die "--target needs a value"
		;;
	--dir)
		DIR="${2:-}"
		shift 2 || die "--dir needs a value"
		;;
	--base-url)
		BASE_URL="${2:-}"
		shift 2 || die "--base-url needs a value"
		;;
	--dry-run)
		DRY_RUN="yes"
		shift
		;;
	-h | --help)
		usage
		exit 0
		;;
	*) die "unknown option: $1 (try --help)" ;;
	esac
done

# --- what this machine is -----------------------------------------------------
#
# Every name here is one a real machine reports: `arm64` is what a Mac and some
# Linux distributions say where others say `aarch64`, and `amd64` turns up on
# Debian derivatives. A machine that is not on this list is refused *by name*,
# because "unsupported" and a 404 halfway through a pipe look very different to
# whoever is reading the terminal.
detect_target() {
	system="$(uname -s)"
	machine="$(uname -m)"
	case "$system" in
	Linux) os="unknown-linux-musl" ;;
	Darwin) os="apple-darwin" ;;
	*) die "no prebuilt binary for $system — build from source with \`cargo build --release\`" ;;
	esac
	case "$machine" in
	x86_64 | amd64) arch="x86_64" ;;
	aarch64 | arm64) arch="aarch64" ;;
	*) die "no prebuilt binary for $machine on $system — build from source with \`cargo build --release\`" ;;
	esac
	printf '%s-%s\n' "$arch" "$os"
}

[ -n "$TARGET" ] || TARGET="$(detect_target)"
case "$TARGET" in
*windows*) die "$TARGET ships as a .zip — download it from https://github.com/$REPO/releases and unzip it" ;;
esac

# --- fetching -----------------------------------------------------------------

# fetch <url> [destination] — to a file, or to stdout when there isn't one.
fetch() {
	out="${2:--}"
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL "$1" -o "$out"
	elif command -v wget >/dev/null 2>&1; then
		wget -qO "$out" "$1"
	else
		die "neither curl nor wget is installed"
	fi
}

if [ -z "$VERSION" ]; then
	VERSION="$(fetch "https://api.github.com/repos/$REPO/releases/latest" |
		sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
	[ -n "$VERSION" ] || die "could not work out the latest release; pass --version"
fi

ASSET="$BIN-$VERSION-$TARGET.tar.gz"
[ -n "$BASE_URL" ] || BASE_URL="https://github.com/$REPO/releases/download/$VERSION"

# --- where it goes ------------------------------------------------------------
#
# `/usr/local/bin` when it can be written to, and the user's own bin directory
# otherwise — so an install without root works rather than failing at the last
# step, which on a shared Pi is the common case.
if [ -z "$DIR" ]; then
	if [ -w /usr/local/bin ]; then
		DIR="/usr/local/bin"
	elif [ -n "${HOME:-}" ]; then
		DIR="$HOME/.local/bin"
	else
		die "/usr/local/bin is not writable and there is no \$HOME to fall back to — pass --dir"
	fi
fi

say "$BIN $VERSION"
say "  target:  $TARGET"
say "  asset:   $ASSET"
say "  from:    $BASE_URL/$ASSET"
say "  into:    $DIR"

if [ "$DRY_RUN" = "yes" ]; then
	say ""
	say "dry run: nothing was downloaded or installed."
	exit 0
fi

# --- fetch, verify, install ---------------------------------------------------

TMP="$(mktemp -d)"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT INT TERM

say ""
say "downloading…"
fetch "$BASE_URL/$ASSET" "$TMP/$ASSET"
fetch "$BASE_URL/SHA256SUMS" "$TMP/SHA256SUMS"

expected="$(awk -v name="$ASSET" '$2 == name || $2 == "*" name { print $1 }' "$TMP/SHA256SUMS" | head -n 1)"
[ -n "$expected" ] || die "the release's SHA256SUMS does not mention $ASSET"

if command -v sha256sum >/dev/null 2>&1; then
	actual="$(sha256sum "$TMP/$ASSET" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
	actual="$(shasum -a 256 "$TMP/$ASSET" | awk '{ print $1 }')"
else
	die "no sha256sum or shasum to verify the download with"
fi

if [ "$actual" != "$expected" ]; then
	die "checksum mismatch for $ASSET
  expected $expected
  got      $actual
Nothing was installed. Try again, and if it keeps happening say so at
https://github.com/$REPO/issues — a download that does not match the release's
own checksum is worth reporting."
fi
say "checksum ok"

tar -xzf "$TMP/$ASSET" -C "$TMP" || die "could not unpack $ASSET"
[ -f "$TMP/$BIN" ] || die "$ASSET does not contain $BIN"

mkdir -p "$DIR" || die "could not create $DIR"
if ! install -m 755 "$TMP/$BIN" "$DIR/$BIN" 2>/dev/null; then
	die "could not write $DIR/$BIN — try again with a writable --dir, or with sudo"
fi

say "installed $DIR/$BIN"

case ":$PATH:" in
*":$DIR:"*) ;;
*)
	say ""
	say "note: $DIR is not on your PATH. Add it with:"
	say "  echo 'export PATH=\"$DIR:\$PATH\"' >> ~/.profile"
	;;
esac

cat <<EOF

Next:
  $BIN                      # run it; first run creates ./radio-scout-data
  $BIN --write-config       # a commented radio-scout.toml with every default
  sudo $BIN service install # run it at boot (systemd / launchd)
EOF
