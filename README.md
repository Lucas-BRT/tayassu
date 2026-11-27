# tayassu

Multi-account session and cooldown manager for subscription-authenticated AI CLI agents (`agy`, `claude`, `codex`, `copilot`, `opencode`, and custom CLIs) on Linux.

## Purpose

When you hold multiple legitimately paid or granted subscription accounts across AI coding CLIs, each account has independent rolling and weekly usage limits (and per-model sub-limits). `tayassu` isolates each account into its own Linux profile directory so you never have to log out and back in, tracks cooldown windows locally, and routes runs to an available account.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Lucas-BRT/tayassu/main/install.sh | bash
```

The script downloads the latest release binary into `${TAYASSU_INSTALL_DIR:-$HOME/.local/bin}` and falls back to `cargo install` when no prebuilt asset matches your platform. The script also configures tab completion for bash, zsh and fish (`tayassu install-completions`, safe to re-run). You can also build from source (then run `tayassu install-completions` yourself):

```bash
cargo install --locked --path apps/tayassu
```

## Quick start

```bash
tayassu import          # discover installed CLIs and copy credentials into isolated profiles
tayassu account login agy
tayassu status
tayassu run claude -- "explain this repository"
```

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
