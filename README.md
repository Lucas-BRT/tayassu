# tayassu

Multi-account session and cooldown manager for subscription-authenticated AI CLI agents (`agy`, `claude`, `codex`, `copilot`, `opencode`, and custom CLIs) on Linux.

## Purpose

When you hold multiple legitimately paid or granted subscription accounts across AI coding CLIs, each account has independent rolling and weekly usage limits (and per-model sub-limits). `tayassu` isolates each account into its own Linux profile directory so you never have to log out and back in, tracks cooldown windows locally, and routes runs to an available account.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Lucas-BRT/tayassu/main/install.sh | bash
```

The script downloads the latest release binary into `${TAYASSU_INSTALL_DIR:-$HOME/.local/bin}` and falls back to `cargo install` when no prebuilt asset matches your platform. You can also build from source:

```bash
cargo install --locked --path apps/tayassu
```

## Quick start

```bash
tayassu import          # discover installed CLIs and copy credentials into isolated profiles
tayassu status
# If import requests a login, use the reported account ID, for example:
# tayassu account login agy-host
tayassu probe
tayassu run claude --dry-run -- --help
tayassu run claude -- --help
```

For more setup options, see how to
[connect two accounts from the same provider](docs/USAGE.md#connect-two-accounts-from-the-same-provider)
or [let Tayassu choose a provider](docs/USAGE.md#let-tayassu-choose-a-provider).

## Documentation

See [docs/USAGE.md](docs/USAGE.md) for the full command reference, configuration format, shell integration, background probing, and troubleshooting.

## Workspace layout

- `apps/tayassu` — CLI entrypoint
- `crates/tayassu-core` — domain model, cooldown tracking, and account selection policy
- `crates/tayassu-providers` — provider adapters and Linux profile isolation (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, or selective fake `HOME`)
- `crates/tayassu-store` — XDG config (`config.toml`) and atomic state storage (`state.json`)

## Development

```bash
git config core.hooksPath .githooks
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo nextest run --locked --workspace
```


## Releases

After promoting `develop` to `main` through a pull request, tag the release commit.
The tag must match the workspace version in `Cargo.toml` and the lockfile must be
up to date. The tag workflow reruns CI, builds and smoke-tests Linux binaries on
x86_64 and ARM64, then publishes both archives and their SHA-256 checksums to
GitHub Releases.
