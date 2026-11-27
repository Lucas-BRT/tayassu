#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf -- "$scratch"' EXIT
export TEST_FIXTURES="$scratch"
export TAYASSU_VERSION=v9.8.7
export TAYASSU_INSTALL_DIR="$scratch/install"
export PATH="$scratch/bin:$PATH"
mkdir -p "$scratch/bin" "$scratch/assets"

cat > "$scratch/tayassu" <<'MOCK'
#!/usr/bin/env bash
printf 'tayassu 9.8.7\n'
MOCK
cat > "$scratch/bin/uname" <<'MOCK'
#!/usr/bin/env bash
case "$1" in
  -s) printf 'Linux\n' ;;
  -m) printf 'x86_64\n' ;;
esac
MOCK
cat > "$scratch/bin/curl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == -fsSL && "$2" == *"/${TAYASSU_VERSION}/"* && "$3" == -o ]]
cp -- "$TEST_FIXTURES/assets/${2##*/}" "$4"
MOCK
cat > "$scratch/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == install && "$4" == --tag && "$5" == "$TAYASSU_VERSION" && "$6" == --locked && "$7" == --root ]]
touch "$TEST_FIXTURES/source-used"
install -Dm755 "$TEST_FIXTURES/tayassu" "$8/bin/tayassu"
MOCK
chmod +x "$scratch/bin/"*

asset=tayassu-x86_64-unknown-linux-gnu.tar.gz
bash "$repo_dir/tools/package-release.sh" "$scratch/tayassu" x86_64-unknown-linux-gnu "$scratch/assets"
bash "$repo_dir/install.sh"
[[ "$("$TAYASSU_INSTALL_DIR/tayassu" --version)" == "tayassu 9.8.7" ]]
[[ ! -e "$scratch/source-used" ]]

export TAYASSU_INSTALL_DIR="$scratch/invalid"
printf '%064d  %s\n' 0 "$asset" > "$scratch/assets/$asset.sha256"
if bash "$repo_dir/install.sh"; then
  printf 'installer accepted a bad checksum\n' >&2
  exit 1
fi
[[ ! -e "$TAYASSU_INSTALL_DIR/tayassu" && ! -e "$scratch/source-used" ]]

export TAYASSU_INSTALL_DIR="$scratch/fallback"
mv "$scratch/assets/$asset" "$scratch/unused.tar.gz"
bash "$repo_dir/install.sh"
[[ -e "$scratch/source-used" ]]
[[ "$("$TAYASSU_INSTALL_DIR/tayassu" --version)" == "tayassu 9.8.7" ]]
