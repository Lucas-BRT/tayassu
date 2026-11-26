# tayassu

Multi-account session and cooldown manager for subscription-authenticated AI CLI agents (`agy`, `claude`, `codex`, and custom CLIs) on Linux.

## Purpose

When you hold multiple legitimately paid or granted subscription accounts across AI coding CLIs, each account has independent rolling and weekly usage limits (and per-model sub-limits). `tayassu` isolates each account into its own Linux profile directory so you never have to log out and back in, tracks cooldown windows locally, and routes runs to an available account.

## Workspace layout

- `apps/tayassu` — CLI entrypoint (`run`, `status`, `probe`, `account`, `cooldown`)
- `crates/tayassu-core` — domain model, cooldown tracking, and account selection policy
- `crates/tayassu-providers` — provider adapters and Linux profile isolation (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, or selective fake `HOME`)
- `crates/tayassu-store` — XDG config (`config.toml`) and atomic state storage (`state.json`)

## Development

```bash
git config core.hooksPath .githooks
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo nextest run --locked --workspace
```
