#!/bin/sh
# narwhal installer
#
#   curl -fsSL https://github.com/Nonanti/narwhal/releases/latest/download/install.sh | sh
#
# or for the bleeding edge (main branch):
#
#   curl -fsSL https://raw.githubusercontent.com/Nonanti/narwhal/main/scripts/install.sh | sh
#
# Environment:
#   NARWHAL_VERSION   pin to a specific tag (default: latest)
#   NARWHAL_BIN_DIR   install dir       (default: ~/.local/bin, or /usr/local/bin if writable & no HOME bin)
#   NARWHAL_NO_MODIFY_PATH=1   skip PATH advisory
#   NARWHAL_FORCE=1   overwrite existing binary without prompt
#
# Exit codes: 0 ok, 1 generic error, 2 unsupported platform, 3 download/verify fail.

set -eu

REPO="Nonanti/narwhal"
BINARY="narwhal"

# ---- pretty ------------------------------------------------------------------

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  C_BOLD="$(printf '\033[1m')"; C_DIM="$(printf '\033[2m')"
  C_RED="$(printf '\033[31m')"; C_GRN="$(printf '\033[32m')"
  C_YLW="$(printf '\033[33m')"; C_BLU="$(printf '\033[34m')"
  C_RST="$(printf '\033[0m')"
else
  C_BOLD=""; C_DIM=""; C_RED=""; C_GRN=""; C_YLW=""; C_BLU=""; C_RST=""
fi

say()  { printf '%s%s%s %s\n' "$C_BLU" "::" "$C_RST" "$*" >&2; }
ok()   { printf '%s%s%s %s\n' "$C_GRN" "ok" "$C_RST" "$*" >&2; }
warn() { printf '%s%s%s %s\n' "$C_YLW" "!!" "$C_RST" "$*" >&2; }
die()  { printf '%s%s%s %s\n' "$C_RED" "xx" "$C_RST" "$*" >&2; exit "${2:-1}"; }

# ---- platform detection ------------------------------------------------------

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux)  os_part="unknown-linux-gnu" ;;
    Darwin) os_part="apple-darwin" ;;
    *)      die "unsupported OS: $os (see https://github.com/${REPO}#install for cargo install)" 2 ;;
  esac

  case "$arch" in
    x86_64|amd64)  arch_part="x86_64" ;;
    arm64|aarch64) arch_part="aarch64" ;;
    *)             die "unsupported arch: $arch" 2 ;;
  esac

  case "${arch_part}-${os_part}" in
    x86_64-unknown-linux-gnu) ;;
    aarch64-apple-darwin)     ;;
    aarch64-unknown-linux-gnu)
      die "aarch64 Linux prebuilt not shipped yet — install via: cargo install narwhaldb" 2 ;;
    x86_64-apple-darwin)
      die "x86_64 macOS prebuilt not shipped yet — install via: brew install Nonanti/tap/narwhal" 2 ;;
  esac

  printf '%s-%s\n' "$arch_part" "$os_part"
}

# ---- dependencies ------------------------------------------------------------

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing dependency: $1"
}

need uname
need tar
need mkdir
need rm

if command -v curl >/dev/null 2>&1; then
  DL="curl --proto =https --tlsv1.2 -fsSL -o"
  DL_STDOUT="curl --proto =https --tlsv1.2 -fsSL"
elif command -v wget >/dev/null 2>&1; then
  DL="wget --https-only -qO"
  DL_STDOUT="wget --https-only -qO-"
else
  die "need curl or wget"
fi

# ---- version resolution ------------------------------------------------------

resolve_version() {
  if [ -n "${NARWHAL_VERSION:-}" ]; then
    case "$NARWHAL_VERSION" in v*) printf '%s\n' "$NARWHAL_VERSION" ;;
                              *)    printf 'v%s\n' "$NARWHAL_VERSION" ;;
    esac
    return
  fi
  # GitHub redirects /releases/latest → /releases/tag/vX.Y.Z; parse Location.
  url="https://github.com/${REPO}/releases/latest"
  tag="$($DL_STDOUT -I -L "$url" 2>/dev/null \
         | awk 'tolower($1)=="location:"{print $2}' \
         | tail -1 \
         | sed -E 's|.*/tag/||; s|[[:space:]]*$||')"
  [ -n "$tag" ] || die "could not resolve latest release tag" 3
  printf '%s\n' "$tag"
}

# ---- install dir -------------------------------------------------------------

choose_bin_dir() {
  if [ -n "${NARWHAL_BIN_DIR:-}" ]; then
    printf '%s\n' "$NARWHAL_BIN_DIR"; return
  fi
  if [ -n "${HOME:-}" ]; then
    printf '%s/.local/bin\n' "$HOME"; return
  fi
  printf '/usr/local/bin\n'
}

# ---- main --------------------------------------------------------------------

main() {
  target="$(detect_target)"
  tag="$(resolve_version)"
  version="${tag#v}"
  bin_dir="$(choose_bin_dir)"

  archive="narwhal-${version}-${target}.tar.gz"
  base="https://github.com/${REPO}/releases/download/${tag}"
  url="${base}/${archive}"
  sha_url="${url}.sha256"

  say "version  ${C_BOLD}${tag}${C_RST}"
  say "target   ${C_BOLD}${target}${C_RST}"
  say "bin dir  ${C_BOLD}${bin_dir}${C_RST}"

  tmp="$(mktemp -d 2>/dev/null || mktemp -d -t narwhal)"
  trap 'rm -rf "$tmp"' EXIT INT HUP TERM

  say "downloading ${C_DIM}${url}${C_RST}"
  $DL "${tmp}/${archive}"      "$url"     || die "download failed" 3
  $DL "${tmp}/${archive}.sha256" "$sha_url" || warn "checksum file missing, skipping verify"

  if [ -f "${tmp}/${archive}.sha256" ]; then
    say "verifying checksum"
    expected="$(awk '{print $1}' "${tmp}/${archive}.sha256")"
    if command -v sha256sum >/dev/null 2>&1; then
      actual="$(sha256sum "${tmp}/${archive}" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
      actual="$(shasum -a 256 "${tmp}/${archive}" | awk '{print $1}')"
    else
      warn "no sha256sum/shasum on PATH — skipping"
      actual="$expected"
    fi
    [ "$expected" = "$actual" ] || die "checksum mismatch (expected $expected, got $actual)" 3
    ok "checksum verified"
  fi

  say "extracting"
  ( cd "$tmp" && tar -xzf "$archive" )
  extracted="${tmp}/narwhal-${version}-${target}/${BINARY}"
  [ -x "$extracted" ] || die "binary not found in archive" 3

  mkdir -p "$bin_dir" || die "cannot create $bin_dir"
  target_path="${bin_dir}/${BINARY}"

  if [ -e "$target_path" ] && [ "${NARWHAL_FORCE:-0}" != "1" ]; then
    existing="$("$target_path" --version 2>/dev/null || echo unknown)"
    warn "overwriting existing ${target_path} (${existing})"
  fi

  install -m 0755 "$extracted" "$target_path" 2>/dev/null \
    || { cp "$extracted" "$target_path" && chmod 0755 "$target_path"; } \
    || die "failed to install to $target_path"

  ok "installed ${C_BOLD}${BINARY}${C_RST} -> ${target_path}"

  installed_version="$("$target_path" --version 2>/dev/null || true)"
  [ -n "$installed_version" ] && say "${installed_version}"

  # PATH advisory
  if [ "${NARWHAL_NO_MODIFY_PATH:-0}" != "1" ]; then
    case ":$PATH:" in
      *":${bin_dir}:"*) : ;;
      *)
        printf '\n'
        warn "${bin_dir} is not on your PATH"
        printf '   Add this to your shell rc (bash/zsh):\n'
        printf '     %sexport PATH="%s:$PATH"%s\n\n' "$C_BOLD" "$bin_dir" "$C_RST"
        ;;
    esac
  fi

  printf '\n%s%s%s  run %snarwhal%s to start, or %snarwhal --help%s for a tour.\n' \
    "$C_GRN" "::" "$C_RST" "$C_BOLD" "$C_RST" "$C_BOLD" "$C_RST"
}

main "$@"
