# AGENTS.md

Operational conventions for `tayassu`.

## Language

- Code, comments, documentation, commits, and error messages are written in English.

## Workspace layout

- `apps/tayassu`: CLI binary (`anyhow`, `clap`, `tracing`, `tracing-subscriber`).
- `crates/tayassu-core`: domain types, quota/cooldown state machine, account selection policy (`thiserror`).
- `crates/tayassu-providers`: `ProviderAdapter` trait, Linux profile isolation (native env var or symlinked fake `HOME`), and provider implementations (`agy`, `claude`, `codex`, `custom`).
- `crates/tayassu-store`: XDG config (`config.toml`) and atomic state persistence (`state.json`).
- `tools/`: development utilities (never `xtask/`).

## Rust rules

- Toolchain is pinned in `rust-toolchain.toml` (`1.99.1`, `minimal` profile with `rustfmt` and `clippy`).
- Workspace lints live in root `Cargo.toml` (`clippy::pedantic`, `unsafe_code = "forbid"`); every crate inherits `[lints] workspace = true`.
- Behaviour lives on the domain type that owns it (methods, `FromStr`, `Display`, `From`/`TryFrom`), not loose free functions over primitives.
- Never leave bare magic literals: declare documented `const` values in the file that uses them and build limit messages from those constants.
- Use `thiserror` in library crates, `anyhow` with `.context(...)` in binaries, and `tracing` for logging. No `Box<dyn Error>` plumbing and no `println!` logging.
- Always pass `--locked` to `cargo` commands in hooks and CI.

## Git flow and hooks

- Branch `feat/<slug>` or `fix/<slug>` from `develop`. Open a PR to `develop`, then promote `develop` to `main` via PR. Never push directly to `main`.
- Enable versioned hooks once per clone:
  ```bash
  git config core.hooksPath .githooks
  ```
- Make atomic commits: one cohesive logical change per commit, never mixing unrelated edits.
- `commit-msg` enforces Conventional Commits (`feat|fix|chore|docs|refactor|test|ci|build|perf|style|revert`).
- `pre-push` runs `cargo fmt --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings`.
