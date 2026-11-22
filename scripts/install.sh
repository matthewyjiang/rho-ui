#!/usr/bin/env sh
# Installs rho-ui from a GitHub release, on Linux (x86_64) or macOS (Apple
# silicon). The docs site serves it, so this installs the latest release:
#
#   curl -fsSL https://matthewyjiang.github.io/rho-ui/install.sh | sh
#
# RHO_UI_VERSION=0.1.0 installs that release instead. On Linux it puts
# rho-ui in RHO_UI_INSTALL_DIR (~/.local/bin) and adds it to the app menu. On
# macOS it puts Rho.app in RHO_UI_APP_DIR (/Applications, or ~/Applications
# when that isn't writable) and links rho-ui into RHO_UI_INSTALL_DIR.
# Running it again updates rho-ui in place.
set -eu

REPO="matthewyjiang/rho-ui"
VERSION="${RHO_UI_VERSION:-latest}"
INSTALL_DIR="${RHO_UI_INSTALL_DIR:-$HOME/.local/bin}"
FROM_SOURCE="https://matthewyjiang.github.io/rho-ui/guide/getting-started#from-source"

fail() {
  echo "error: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

# The release's name for this machine, as release.yml builds it.
platform() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
  esac
  # A shell under Rosetta on Apple silicon says x86_64; the arm64 build runs
  # natively there. Intel Macs lack this sysctl.
  if [ "$os-$arch" = Darwin-x86_64 ] &&
    [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = 1 ]; then
    arch=aarch64
  fi
  case "$os-$arch" in
    Linux-x86_64) echo x86_64-linux ;;
    Darwin-aarch64) echo aarch64-macos ;;
    *) fail "no rho-ui build for $os on $arch; build it from source: $FROM_SOURCE" ;;
  esac
}

# Where the release's files are: the latest release, or RHO_UI_VERSION's.
release_url() {
  case "$VERSION" in
    latest) echo "https://github.com/$REPO/releases/latest/download" ;;
    v*) echo "https://github.com/$REPO/releases/download/$VERSION" ;;
    *) echo "https://github.com/$REPO/releases/download/v$VERSION" ;;
  esac
}

# Prints the checksum SHA256SUMS (on stdin) lists for the file $1.
listed_sha256() {
  while read -r sum name; do
    if [ "$name" = "$1" ]; then
      echo "$sum"
      return
    fi
  done
}

if command -v curl >/dev/null 2>&1; then
  download() { curl --fail --location --proto '=https' --tlsv1.2 --silent --show-error "$1" --output "$2"; }
elif command -v wget >/dev/null 2>&1; then
  download() { wget --quiet --https-only "$1" -O "$2"; }
else
  fail "required command not found: curl or wget"
fi
need_cmd uname
need_cmd mktemp

target="$(platform)"
case "$target" in
  *-linux) asset="rho-ui-$target.tar.gz" ;;
  *-macos) asset="rho-ui-$target.zip" ;;
esac
url="$(release_url)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "downloading $asset ($VERSION)..."
download "$url/$asset" "$tmp/$asset" ||
  fail "can't download $url/$asset: check that release $VERSION exists"
download "$url/SHA256SUMS" "$tmp/SHA256SUMS" ||
  fail "can't download $url/SHA256SUMS"
expected="$(listed_sha256 "$asset" < "$tmp/SHA256SUMS")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset")"
else
  need_cmd shasum
  actual="$(shasum -a 256 "$tmp/$asset")"
fi
actual="${actual%% *}"
[ -n "$expected" ] && [ "$actual" = "$expected" ] ||
  fail "$asset doesn't match its checksum in SHA256SUMS"

mkdir -p "$INSTALL_DIR"
dir="$tmp/rho-ui-$target"
case "$target" in
  *-linux)
    tar -xzf "$tmp/$asset" -C "$tmp"
    bin="$INSTALL_DIR/rho-ui"
    # Into a temporary name first, then renamed over the old one, so a
    # running rho-ui keeps its file.
    cp "$dir/rho-ui" "$bin.new"
    chmod 0755 "$bin.new"
    mv -f "$bin.new" "$bin"

    # The app menu's entry. The menu doesn't get the shell's PATH, so it
    # names rho-ui by its full path.
    data="${XDG_DATA_HOME:-$HOME/.local/share}"
    mkdir -p "$data/applications" "$data/icons/hicolor/scalable/apps"
    sed "s|^Exec=rho-ui|Exec=$bin|" "$dir/rho-ui.desktop" > "$data/applications/rho-ui.desktop"
    cp "$dir/icon.svg" "$data/icons/hicolor/scalable/apps/rho-ui.svg"
    ;;
  *-macos)
    if [ -n "${RHO_UI_APP_DIR:-}" ]; then
      apps="$RHO_UI_APP_DIR"
    elif [ -w /Applications ]; then
      apps=/Applications
    else
      apps="$HOME/Applications"
    fi
    # ditto keeps the bundle's signature intact. curl and wget mark no
    # download as quarantined, so Gatekeeper doesn't stop the first open.
    ditto -x -k "$tmp/$asset" "$tmp"
    mkdir -p "$apps"
    rm -rf "$apps/Rho.app"
    mv "$dir/Rho.app" "$apps/Rho.app"
    bin="$INSTALL_DIR/rho-ui"
    ln -sf "$apps/Rho.app/Contents/MacOS/rho-ui" "$bin"
    echo "Rho.app installed to $apps"
    ;;
esac

# `--help` exits before any window opens, so this checks that the binary
# loads here: a Linux older than the build's glibc fails here, not at the
# first launch from the menu.
if ! check="$("$bin" --help 2>&1)" && ! printf '%s' "$check" | grep -q '^usage: rho-ui'; then
  echo "$check" >&2
  fail "rho-ui is installed at $bin but doesn't run here (above)"
fi

echo "rho-ui installed to $bin"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "note: add $INSTALL_DIR to your PATH to run rho-ui from a terminal" ;;
esac
