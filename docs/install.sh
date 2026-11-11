#!/bin/sh
# velo's installer: fetch the latest release binary for this machine, check it
# against the checksum CI published beside it, and put `velo` on disk.
#
#   curl -fsSL https://alvaldes.github.io/velo/install.sh | sh
#
# Environment:
#   VELO_VERSION      a tag to install instead of the latest (v0.42.2)
#   VELO_INSTALL_DIR  where the binary goes (default: ~/.local/bin)
#
# It asks GitHub for two files and nothing else, never uses sudo, and leaves
# nothing behind when it fails. The whole script is one function called on the
# last line, so a download cut short runs nothing.

set -eu

REPO="alvaldes/velo"

fail() {
  echo "velo install: $1" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "'$1' is needed and was not found"
}

target() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$arch" in
    arm64 | aarch64) cpu=aarch64 ;;
    x86_64 | amd64) cpu=x86_64 ;;
    *) fail "no release binary for the '$arch' architecture; build from source: https://github.com/$REPO#install" ;;
  esac
  case "$os" in
    Darwin) echo "$cpu-apple-darwin" ;;
    Linux) echo "$cpu-unknown-linux-gnu" ;;
    *) fail "no release binary for '$os'; build from source: https://github.com/$REPO#install" ;;
  esac
}

# The latest tag is where /releases/latest redirects to: no API, no rate limit.
latest_tag() {
  url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") ||
    fail "could not reach github.com to find the latest release"
  tag=${url##*/}
  case "$tag" in
    v[0-9]*) echo "$tag" ;;
    *) fail "could not read the latest release tag from '$url'" ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  else
    fail "neither 'sha256sum' nor 'shasum' was found, so the download cannot be verified"
  fi
}

main() {
  need curl
  need tar
  need uname

  triple=$(target)
  tag=${VELO_VERSION:-$(latest_tag)}
  case "$tag" in
    v[0-9]*[!0-9A-Za-z.+-]* | v[0-9]*/*) fail "'$tag' is not a release tag (expected something like v0.42.2)" ;;
    v[0-9]*) ;;
    *) fail "'$tag' is not a release tag (expected something like v0.42.2)" ;;
  esac
  dir=${VELO_INSTALL_DIR:-$HOME/.local/bin}
  name="velo-$tag-$triple"
  base="https://github.com/$REPO/releases/download/$tag"

  tmp=$(mktemp -d) || fail "could not create a temporary directory"
  trap 'rm -rf "$tmp"' EXIT
  # dash does not run the EXIT trap when a signal kills it; turning the signal
  # into an exit does, so an interrupted download leaves nothing behind.
  trap 'exit 130' INT HUP TERM

  echo "velo install: downloading $name.tar.gz"
  curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" ||
    fail "could not download $base/$name.tar.gz"
  curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$base/$name.tar.gz.sha256" ||
    fail "could not download the checksum for $name.tar.gz"

  want=$(cut -d ' ' -f 1 "$tmp/$name.tar.gz.sha256")
  got=$(sha256_of "$tmp/$name.tar.gz")
  [ -n "$want" ] && [ "$want" = "$got" ] ||
    fail "checksum mismatch for $name.tar.gz (expected '$want', got '$got'); nothing was installed"

  tar -xzf "$tmp/$name.tar.gz" -C "$tmp" || fail "could not unpack $name.tar.gz"
  [ -f "$tmp/$name/velo" ] || fail "the archive did not contain $name/velo"

  mkdir -p "$dir" || fail "could not create $dir; set VELO_INSTALL_DIR to a directory you can write to"
  [ ! -d "$dir/velo" ] || fail "$dir/velo is a directory; nothing was installed"
  # Staged beside the destination, then renamed: a velo that is running keeps
  # its old file, and a failed copy never leaves half a binary in place.
  if ! { cp "$tmp/$name/velo" "$dir/.velo.new" && chmod 755 "$dir/.velo.new" && mv -f "$dir/.velo.new" "$dir/velo"; }; then
    rm -f "$dir/.velo.new"
    fail "could not write $dir/velo; set VELO_INSTALL_DIR to a directory you can write to"
  fi

  echo "velo install: $tag is at $dir/velo"
  case ":$PATH:" in
    *":$dir:"*) echo "velo install: run 'velo --library /path/to/your/music'" ;;
    *) echo "velo install: $dir is not on your PATH; add it, e.g. export PATH=\"$dir:\$PATH\"" ;;
  esac
}

main "$@"
