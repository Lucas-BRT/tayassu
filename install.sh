#!/usr/bin/env bash
# Install tayassu from a GitHub release, falling back to a source build.
set -euo pipefail

REPO="Lucas-BRT/tayassu"
BIN_NAME="tayassu"
INSTALL_DIR="${TAYASSU_INSTALL_DIR:-${HOME}/.local/bin}"

log() { printf '%s\n' "$*" >&2; }
die() { log "error: $*"; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"; }

detect_target() {
  case "$(uname -s)" in
    Linux) ;;
    *) die "unsupported OS: $(uname -s) (Linux only)" ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) printf 'x86_64-unknown-linux-gnu' ;;
    aarch64 | arm64) printf 'aarch64-unknown-linux-gnu' ;;
    *) die "unsupported architecture: $(uname -m)" ;;
  esac
}

latest_tag() {
  curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" |
    sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' |
    head -n 1
}

install_from_release() {
  need curl
  need tar
  local target asset tmp
  target="$(detect_target)"
  asset="${BIN_NAME}-${target}.tar.gz"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  log "Downloading ${BIN_NAME} ${TAG} (${target})"
  curl -fsSL "https://github.com/${REPO}/releases/download/${TAG}/${asset}" \
    -o "${tmp}/${asset}" || return 1
  tar -xzf "${tmp}/${asset}" -C "$tmp"
  install -Dm755 "${tmp}/${BIN_NAME}" "${INSTALL_DIR}/${BIN_NAME}"
}

install_from_source() {
  need cargo
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  log "No release asset available; building ${TAG} from source"
  cargo install --git "https://github.com/${REPO}" --tag "$TAG" --locked \
    --root "$tmp" "$BIN_NAME"
  install -Dm755 "${tmp}/bin/${BIN_NAME}" "${INSTALL_DIR}/${BIN_NAME}"
}

TAG="${TAYASSU_VERSION:-}"
if [ -z "$TAG" ]; then
  TAG="$(latest_tag || true)"
fi
[ -n "$TAG" ] || die "no release found; set TAYASSU_VERSION to a tag such as v0.1.0"

if ! install_from_release; then
  install_from_source
fi

case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *) log "Add ${INSTALL_DIR} to PATH to run ${BIN_NAME} directly." ;;
esac

"${INSTALL_DIR}/${BIN_NAME}" --version
