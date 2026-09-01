#!/bin/sh
#
# install.sh — install the `deploy` CLI from prebuilt GitHub Release binaries.
#
# Usage:
#     curl -fsSL https://raw.githubusercontent.com/facetlayer/deploy-tool/main/install.sh | sh
#
# Options (pass after `| sh -s --` when piping):
#     --version <tag>    Install a specific release (e.g. v0.1.0). Default: latest.
#     --bin-dir <dir>    Where to install. Default: $HOME/.local/bin.
#     --uninstall        Remove the installed binary.
#     --help             Show this message.
#
# Environment overrides: DEPLOY_CLI_VERSION, DEPLOY_CLI_BIN_DIR.
#
# This installs the client only. The `deploy-server` daemon is not released as
# an artifact; it is built and shipped with install/build-release.sh.

set -eu

REPO="facetlayer/deploy-tool"
BIN_DIR="${DEPLOY_CLI_BIN_DIR:-$HOME/.local/bin}"
VERSION="${DEPLOY_CLI_VERSION:-latest}"
ACTION="install"
BINARY="deploy"

say() { printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  # Print the leading comment block (everything after the shebang, up to the
  # first line that isn't a comment), with the '#' markers stripped.
  awk 'NR==1 {next} /^#/ {sub(/^# ?/, ""); print; next} {exit}' "$0"
  exit 0
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="${2:-}"; [ -n "$VERSION" ] || err "--version needs a value"; shift 2 ;;
    --bin-dir) BIN_DIR="${2:-}"; [ -n "$BIN_DIR" ] || err "--bin-dir needs a value"; shift 2 ;;
    --uninstall) ACTION="uninstall"; shift ;;
    --help|-h) usage ;;
    *) err "unknown option: $1 (try --help)" ;;
  esac
done

need() { command -v "$1" >/dev/null 2>&1 || err "'$1' is required but was not found"; }

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Darwin) os_part="apple-darwin" ;;
    Linux)  os_part="unknown-linux-gnu" ;;
    *) err "unsupported operating system: $os. Releases cover macOS and Linux; otherwise build from source with 'cargo install --path crates/deploy-cli'." ;;
  esac
  case "$arch" in
    x86_64|amd64) arch_part="x86_64" ;;
    arm64|aarch64) arch_part="aarch64" ;;
    *) err "unsupported architecture: $arch" ;;
  esac
  printf '%s-%s' "$arch_part" "$os_part"
}

resolve_version() {
  if [ "$VERSION" != "latest" ]; then
    printf '%s' "$VERSION"
    return
  fi
  # Follow the /releases/latest redirect and read the tag off the final URL.
  # With no published releases GitHub redirects to /releases instead of
  # /releases/tag/<tag>, so require the /tag/ segment before trusting the result.
  url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" 2>/dev/null || true)"
  case "$url" in
    */tag/*) tag="${url##*/tag/}" ;;
    *) tag="" ;;
  esac
  case "$tag" in
    ''|*/*) err "could not determine the latest release of $REPO. Check https://github.com/$REPO/releases, or pass --version <tag>." ;;
  esac
  printf '%s' "$tag"
}

do_uninstall() {
  removed=0
  for dir in "$BIN_DIR" "$HOME/.cargo/bin" "/usr/local/bin"; do
    if [ -f "$dir/$BINARY" ]; then
      rm -f "$dir/$BINARY"
      say "removed $dir/$BINARY"
      removed=1
    fi
  done
  [ "$removed" -eq 1 ] || say "no '$BINARY' binary found in $BIN_DIR, ~/.cargo/bin, or /usr/local/bin"
  say ""
  say "Your API key file was not touched. To delete it:"
  say "    rm -f \"\$HOME/secrets/deploy.env\""
  exit 0
}

[ "$ACTION" = "uninstall" ] && do_uninstall

need curl
need tar
need uname

TARGET="$(detect_target)"
TAG="$(resolve_version)"
ARCHIVE="deploy-$TAG-$TARGET.tar.gz"
BASE_URL="https://github.com/$REPO/releases/download/$TAG"

say "==> Installing deploy $TAG ($TARGET)"

TMP_DIR="$(mktemp -d)"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT INT TERM

say "==> Downloading $ARCHIVE"
curl -fsSL "$BASE_URL/$ARCHIVE" -o "$TMP_DIR/$ARCHIVE" \
  || err "download failed. Is there a release named '$TAG' with an asset for $TARGET?"

# Verify the checksum when a SHA256SUMS asset and a local sha256 tool are both available.
if curl -fsSL "$BASE_URL/SHA256SUMS" -o "$TMP_DIR/SHA256SUMS" 2>/dev/null; then
  if command -v shasum >/dev/null 2>&1; then
    sha_cmd="shasum -a 256"
  elif command -v sha256sum >/dev/null 2>&1; then
    sha_cmd="sha256sum"
  else
    sha_cmd=""
  fi
  if [ -n "$sha_cmd" ]; then
    expected="$(grep " $ARCHIVE\$" "$TMP_DIR/SHA256SUMS" | awk '{print $1}')"
    actual="$($sha_cmd "$TMP_DIR/$ARCHIVE" | awk '{print $1}')"
    if [ -n "$expected" ] && [ "$expected" != "$actual" ]; then
      err "checksum mismatch for $ARCHIVE (expected $expected, got $actual)"
    fi
    say "==> Checksum verified"
  fi
fi

tar -xzf "$TMP_DIR/$ARCHIVE" -C "$TMP_DIR"

if ! mkdir -p "$BIN_DIR" 2>/dev/null; then
  err "cannot create '$BIN_DIR' (permission denied). Re-run with sudo, or choose a
       writable directory with --bin-dir (the default, ~/.local/bin, needs no sudo)."
fi
if [ ! -w "$BIN_DIR" ]; then
  err "'$BIN_DIR' is not writable. Re-run with sudo, or choose a writable directory
       with --bin-dir (the default, ~/.local/bin, needs no sudo)."
fi

src="$(find "$TMP_DIR" -type f -name "$BINARY" | head -n 1)"
[ -n "$src" ] || err "'$BINARY' was not found inside $ARCHIVE"
install -m 755 "$src" "$BIN_DIR/$BINARY" 2>/dev/null || {
  cp "$src" "$BIN_DIR/$BINARY" && chmod 755 "$BIN_DIR/$BINARY"
}
say "    $BINARY -> $BIN_DIR/$BINARY"

# Inside a GitHub Action, put the install dir on the PATH of every later step.
# $GITHUB_PATH is the documented way to do that; a plain `export` would not
# survive the end of the step running this script.
if [ -n "${GITHUB_PATH:-}" ]; then
  printf '%s\n' "$BIN_DIR" >> "$GITHUB_PATH"
  say "    added $BIN_DIR to \$GITHUB_PATH"
fi

say ""
say "==> Done. Installed deploy $TAG to $BIN_DIR"

case ":$PATH:" in
  *":$BIN_DIR:"*) say "    Run 'deploy --help' to get started." ;;
  *)
    if [ -z "${GITHUB_PATH:-}" ]; then
      say ""
      say "note: '$BIN_DIR' is not on your PATH. Add it to your shell profile:"
      say "      export PATH=\"$BIN_DIR:\$PATH\""
    fi
    ;;
esac
