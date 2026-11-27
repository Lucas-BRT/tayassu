#!/usr/bin/env bash
# Install tayassu from a GitHub release, falling back to a source build.
# Supports updating an existing installation.
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
  local tag
  tag="$(curl -fsSL -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null |
    sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' |
    head -n 1)"
  printf '%s' "$tag"
}

resolve_tag() {
  local requested="${TAYASSU_VERSION:-}"
  if [ -n "$requested" ]; then
    printf '%s' "$requested"
    return
  fi
  local latest
  latest="$(latest_tag)"
  if [ -n "$latest" ]; then
    printf '%s' "$latest"
    return
  fi
  printf 'v0.1.3'
}

get_installed_version() {
  if [ -x "${INSTALL_DIR}/${BIN_NAME}" ]; then
    "${INSTALL_DIR}/${BIN_NAME}" --version 2>&1 | head -1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || true
  fi
}

install_from_release() {
  need curl
  need tar
  need sha256sum
  local tag target asset sums tmp
  tag="$(resolve_tag)"
  target="$(detect_target)"
  asset="${BIN_NAME}-${target}.tar.gz"
  sums="SHA256SUMS"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  log "Downloading ${BIN_NAME} ${tag} (${target})"
  if ! curl -fsSL "https://github.com/${REPO}/releases/download/${tag}/${asset}" \
    -o "${tmp}/${asset}" 2>/dev/null; then
    log "Warning: failed to download release asset; falling back to source build."
    return 1
  fi
  if ! curl -fsSL "https://github.com/${REPO}/releases/download/${tag}/${sums}" \
    -o "${tmp}/${sums}" 2>/dev/null; then
    log "Warning: failed to download ${sums}; falling back to source build."
    return 1
  fi
  (cd "$tmp" && grep "  ${asset}\$" "$sums" | sha256sum -c - >/dev/null) || {
    log "Warning: checksum verification failed for ${asset}; falling back to source build."
    return 1
  }
  tar -xzf "${tmp}/${asset}" -C "$tmp"
  install -Dm755 "${tmp}/${BIN_NAME}" "${INSTALL_DIR}/${BIN_NAME}"
}

install_from_source() {
  need cargo
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  log "Building ${BIN_NAME} from source (release asset unavailable)."
  cargo install --git "https://github.com/${REPO}" --tag "$TAG" --locked \
    --root "$tmp" "$BIN_NAME"
  install -Dm755 "${tmp}/bin/${BIN_NAME}" "${INSTALL_DIR}/${BIN_NAME}"
}

# ---------- Main flow ----------

# Resolve the desired tag (respects TAYASSU_VERSION if set).
desired_tag="$(resolve_tag)"

# Check if already installed on the desired tag.
installed_version="$(get_installed_version)"

if [ -n "$installed_version" ] && [ "$installed_version" = "${desired_tag#v}" ]; then
  # Already installed the desired version.
  log "tayassu ${desired_tag} is already installed."
  exit 0
fi

# Decide message based on whether we have an existing install.
if [ -n "$installed_version" ]; then
  log "Updating tayassu from ${installed_version} to ${desired_tag}..."
else
  log "Installing tayassu ${desired_tag}..."
fi

# Attempt release install; fall back to source build.
if install_from_release; then
  log "Installation complete."
else
  log "Release install failed; building from source..."
  install_from_source
fi

# Ensure PATH message (only if we actually installed something).
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *) log "Add ${INSTALL_DIR} to PATH to run ${BIN_NAME} directly." ;;
esac

# Configure shell activation and completion (non-fatal).
"${INSTALL_DIR}/${BIN_NAME}" setup --no-systemd || log "Warning: could not configure shell completions."

# Print final version.
"${INSTALL_DIR}/${BIN_NAME}" --version