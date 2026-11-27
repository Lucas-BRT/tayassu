# tayassu usage guide

Complete reference for installing, configuring, and running `tayassu`.
For the project overview, see the [README](../README.md).

## Requirements

- Linux (profile isolation relies on symlinks and POSIX environment variables).
- A subscription-authenticated AI CLI already installed and logged in on the host:
  `agy`, `claude`, `codex`, `copilot`, or `opencode`. Custom binaries are supported.
- Rust `1.99.1` (pinned in `rust-toolchain.toml`) only when building from source.

## Installation

### Install script

```bash
curl -fsSL https://raw.githubusercontent.com/Lucas-BRT/tayassu/main/install.sh | bash
```

The script downloads the latest release binary for your architecture into
`${TAYASSU_INSTALL_DIR:-$HOME/.local/bin}`. When no prebuilt asset matches the
current platform, it falls back to `cargo install --git`.

Environment overrides:

| Variable              | Meaning                                              |
| --------------------- | ---------------------------------------------------- |
| `TAYASSU_VERSION`     | Release tag to install, for example `v0.1.0`.        |
| `TAYASSU_INSTALL_DIR` | Destination directory. Defaults to `$HOME/.local/bin`. |

Make sure the destination directory is on your `PATH`.

### Build from source

```bash
git clone https://github.com/Lucas-BRT/tayassu.git
cd tayassu
cargo install --locked --path apps/tayassu
```

## Quick start

```bash
# 1. Discover installed CLIs and copy their credentials into isolated profiles.
tayassu import

# 2. Authenticate any account that needs a fresh login.
tayassu account login agy

# 3. Inspect quota and cooldown state.
tayassu status

# 4. Run an agent through the account router.
tayassu run agy -- --help
```

`tayassu import` never deletes binaries or updater directories. It copies host
auth files by default; pass `--move-auth` only when you want the host files
removed after a verified copy.

## Commands

### `tayassu import` (alias `discover`)

Scans the host for installed AI CLIs and registers one account per provider.

```bash
tayassu import --dry-run
tayassu import --provider claude --provider codex
tayassu import --exclude copilot --priority 10
```

Options: `--provider` (repeatable include filter), `--exclude` (repeatable),
`--dry-run`, `--move-auth`, `--priority`.

### `tayassu account`

```bash
tayassu account add agy-student agy --label "Student plan" --priority 10
tayassu account add work-claude claude --binary /opt/claude/bin/claude
tayassu account env agy-student
tayassu account login agy-student
tayassu account remove agy-student
```

- `add` creates the isolated profile directory and writes the account to `config.toml`.
- `env` prints the profile directory and `export` lines for manual use.
- `login` launches the provider CLI inside the isolated profile.
- `remove` drops the account from `config.toml` but leaves profile data on disk.

### `tayassu run`

Selects an available account and invokes its CLI inside the isolated profile.

```bash
tayassu run claude -- "explain this repository"
tayassu run --account codex-work -- --version
tayassu run --chain claude --model claude --failover -- "fix the tests"
tayassu run agy --dry-run -- --help
```

Options:

| Option          | Effect                                                              |
| --------------- | ------------------------------------------------------------------- |
| `--account ID`  | Force a specific account instead of routing automatically.          |
| `--chain NAME`  | Use a named cross-provider fallback chain from `config.toml`.       |
| `--model NAME`  | Model bucket to evaluate cooldowns against (default `default`).     |
| `--policy NAME` | Override the selection policy (`priority`, `most-quota`, `soonest-reset`). |
| `--dry-run`     | Print the selected account and environment without spawning.        |
| `--failover`    | Retry once on the next available account after a rate limit.        |

Arguments after `--` are passed verbatim to the underlying CLI.

### `tayassu status`

```bash
tayassu status
tayassu status --provider claude
tayassu status --model claude
```

Prints remaining quota, cooldown windows, and active sessions per account.

### `tayassu dash`

Interactive terminal dashboard. `p` probes every account, `c` clears cooldowns
for the selected account, and `q` quits.

### `tayassu probe`

```bash
tayassu probe
tayassu probe --id agy-student
tayassu probe --notify
```

Runs each provider's non-interactive usage check and records cooldowns. With
`--notify`, `notify-send` is used when a previously active cooldown clears.

### `tayassu cooldown`

```bash
tayassu cooldown set agy-student --minutes 300 --reason "weekly limit"
tayassu cooldown set claude-work --model opus --minutes 60
tayassu cooldown clear agy-student
```

Default manual cooldown is 300 minutes (5 hours).

### `tayassu doctor`

```bash
tayassu doctor
tayassu doctor --strict
```

Validates binaries, fake-`HOME` symlinks, and credential permissions. `--strict`
exits non-zero on warnings as well as errors.

### `tayassu update`

```bash
tayassu update --dry-run
tayassu update --provider claude
```

Runs each CLI's native self-updater with the real host `$HOME`, so isolated
profiles never break upgrades.

### `tayassu shell-init`

```bash
eval "$(tayassu shell-init bash)"
eval "$(tayassu shell-init zsh --failover)"
tayassu shell-init fish | source
```

Generates wrapper functions that route `agy`, `claude`, `codex`, `copilot`, and
`opencode` through `tayassu run`. Bypass a wrapper with `command <cli> ...`.

Add the `eval` line to `~/.bashrc`, `~/.zshrc`, or `~/.config/fish/config.fish`
to make routing permanent.

### `tayassu systemd-unit`

```bash
tayassu systemd-unit
tayassu systemd-unit --interval-minutes 30 --install
systemctl --user daemon-reload
systemctl --user enable --now tayassu-probe.timer
```

Generates or installs `tayassu-probe.service` and `tayassu-probe.timer` under
`$XDG_CONFIG_HOME/systemd/user/`. The timer runs `tayassu probe --notify`.

## Configuration

`tayassu` reads `$XDG_CONFIG_HOME/tayassu/config.toml` (default
`~/.config/tayassu/config.toml`). Accounts are normally created by
`tayassu import` or `tayassu account add`, but the file can be edited directly.

```toml
default_policy = "priority"

[fallback_chains]
claude = ["agy:claude", "claude:default"]
codex = ["claude:default", "codex:default"]

[[accounts]]
id = "agy-student"
provider = "agy"
label = "Student plan"
priority = 10
enabled = true

[[accounts]]
id = "claude-work"
provider = "claude"
label = "Work account"
priority = 20
enabled = true
binary_override = "/opt/claude/bin/claude"
shared_home_links = [".gitconfig", ".config/gh"]
```

Fields:

- `default_policy`: `priority` (lower priority value wins), `most-quota`, or
  `soonest-reset`.
- `fallback_chains`: named lists of `provider:bucket` targets used by `--chain`.
- `accounts`: account id, provider, label, priority, enabled flag, optional
  binary override, and extra host paths to symlink into a fake-`HOME` profile.

### Per-project overrides

Place `.tayassu.toml` in a repository; the nearest file walking up from the
working directory wins.

```toml
default_policy = "most-quota"
fallback_chain = "claude"

[preferred_accounts]
claude = "claude-work"
agy = "agy-student"
```

## Files and directories

| Path                                         | Contents                                  |
| -------------------------------------------- | ----------------------------------------- |
| `$XDG_CONFIG_HOME/tayassu/config.toml`       | Accounts, policies, and fallback chains.  |
| `$XDG_STATE_HOME/tayassu/state.json`         | Quotas, cooldowns, and active sessions.   |
| `$XDG_DATA_HOME/tayassu/profiles/<account>/` | Isolated per-account profiles.            |

`config.toml` and `state.json` are written atomically; `state.json` updates take
an exclusive file lock, so concurrent runs and background probes are safe.

## Troubleshooting

- **No accounts after `import`**: run `tayassu import --dry-run` to see what was
  discovered, and confirm the CLI is on `PATH`.
- **Login required**: run `tayassu account login <id>`; some providers require a
  fresh OAuth flow after credentials are copied.
- **Broken profile links**: run `tayassu doctor --strict`.
- **A provider stopped working after an upgrade**: run `tayassu update`, then
  `tayassu doctor`.
- **Wrapper loops or wrong account**: call the CLI directly with
  `command <cli> ...`, or check the account selected by
  `tayassu run <provider> --dry-run`.
- **Rate limited during a run**: add `--failover`, or use a chain that includes
  another provider.
