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
current platform, it falls back to `cargo install --git` using the same release
tag. Downloaded archives are verified against their published SHA-256 checksum.

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

# 2. If import requests a fresh login, use the account ID it prints.
tayassu account login agy-host

# 3. Check configured accounts and inspect their status.
tayassu probe
tayassu status

# 4. Run an agent through the account router.
tayassu run agy -- --help
```

`tayassu import` never deletes binaries or updater directories. It copies host
auth files by default; pass `--move-auth` only when you want the host files
removed after a verified copy.

## Connect two accounts from the same provider

Each account ID gets its own profile directory. Credential isolation depends on
where the provider stores its login.

**Antigravity CLI 1.5.18 on Pop!_OS 24.04:** Two Tayassu profiles under the
same desktop login can use the same Google account. Both query the desktop
Secret Service for the same OAuth entry. Signing in through the second profile
can therefore change the identity used by the first, even with
`tayassu run --account <id>`. Use one managed agy identity per desktop login
until private keyring support is available.
For example, to keep a personal and a work Codex account signed in:

```bash
tayassu account add codex-personal codex --label "Personal" --priority 10
tayassu account login codex-personal

tayassu account add codex-work codex --label "Work" --priority 20
tayassu account login codex-work

tayassu probe --id codex-personal
tayassu probe --id codex-work
tayassu status --provider codex
```

Sign in with the corresponding provider account in each login flow. If the
browser reuses an existing sign-in, choose the intended account before
authorizing the CLI. If you already ran `tayassu import --provider codex`,
you can use the imported `codex-host` profile as one of the two accounts and
register only the second one.

```bash
tayassu run codex             # choose an eligible Codex account
tayassu run --account codex-work  # use the work profile explicitly
```

The default `priority` policy prefers lower numbers among eligible accounts.
With the example priorities, new Codex runs normally use `codex-personal`
first and can use `codex-work` when the personal account is cooling down.
Use `tayassu run codex --failover` to retry once on another eligible account
if a new run fails with a recognized rate limit.

## Let Tayassu choose a provider

First register and authenticate accounts for the providers you want to use.
With Codex and Claude accounts configured, add this entry to the
`[fallback_chains]` table in `~/.config/tayassu/config.toml` (or
`$XDG_CONFIG_HOME/tayassu/config.toml`). If the table already exists, add
only the `auto` line under it.

```toml
[fallback_chains]
auto = ["codex:default", "claude:default"]
```

```bash
tayassu probe
tayassu run --chain auto --dry-run
tayassu run --chain auto --failover
```

Tayassu checks the chain in order. It chooses an eligible Codex account first
and checks Claude when no Codex account qualifies. The account selection
policy ranks accounts within each target. Eligibility uses the recorded
login and quota information and active cooldowns. Some provider probes only
check login, so a provider with unknown quota can still be selected.
`--failover` can retry once on another eligible account after a recognized
rate limit, including an account later in the chain.

To refresh quota before launch and select only accounts with measured quota
remaining for the target, use:

```bash
tayassu run --chain auto --require-quota --failover
```

With `--dry-run`, this flag uses the last saved probe results. Run
`tayassu probe` first if you want an up-to-date preview.

To type just `tayassu run` inside a project, add this top-level setting to
`.tayassu.toml` in the project directory:

```toml
fallback_chain = "auto"
```

Now `tayassu run` uses the chain in that directory and its subdirectories.
For a one-off run elsewhere, use `tayassu run --chain auto`. An explicit
provider such as `tayassu run codex` selects within that provider. Arguments
after `--` go directly to the selected CLI, so use arguments supported by
every provider in the chain or open the selected CLI interactively.

## Commands

### `tayassu import` (alias `discover`)

Scans the host for installed AI CLIs and registers one account per provider.

```bash
tayassu import --dry-run
tayassu import --provider claude --provider codex
tayassu import --exclude copilot --priority 10
```

Options: `--provider` (repeatable include filter), `--exclude` (repeatable),
`--dry-run`, `--refresh`, `--move-auth`, `--priority`.

Repeat imports skip registered accounts by default. Use `--refresh` to copy
credentials again while retaining the existing label, priority and enabled state.

### `tayassu account`

```bash
tayassu account add agy-student agy --label "Student plan" --priority 10
tayassu account add work-claude claude --binary /opt/claude/bin/claude
tayassu account env agy-student
tayassu account login agy-student
tayassu account edit agy-student --label "Personal" --priority 20
tayassu account disable agy-student
tayassu account enable agy-student
tayassu account remove agy-student
```

- `add` creates the isolated profile directory and writes the account to `config.toml`.
- `env` prints the profile directory and `export` lines for manual use.
- `login` launches the provider CLI inside the isolated profile.
- `edit` updates the label or priority.
- `disable` pauses automatic selection for new sessions. `enable` restores it.
- `remove` drops the account from `config.toml` but leaves profile data on disk.

### `tayassu run`

Selects an available account and invokes its CLI inside the isolated profile.

```bash
tayassu run claude -- "explain this repository"
tayassu run --account codex-work -- --version
tayassu run --chain auto --failover -- --help
tayassu run agy --dry-run -- --help
```

Options:

| Option          | Effect                                                              |
| --------------- | ------------------------------------------------------------------- |
| `--account ID`  | Force a specific account instead of routing automatically.          |
| `--chain NAME`  | Use a named cross-provider fallback chain from `config.toml`.       |
| `--model NAME`  | Model bucket for a provider or explicit account (default `default`). Chains use the bucket on each target. |
| `--policy NAME` | Override the selection policy (`priority`, `most-quota`, `soonest-reset`). |
| `--dry-run`     | Explain the selection and print the environment without modifying profiles or state.        |
| `--require-quota` | Refresh usage and require measured quota before launch. Dry runs use the last saved probe. |
| `--failover`    | Retry once on a different account after a rate limit. Requires automatic routing.        |

Arguments after `--` are passed verbatim to the underlying CLI. Resuming a
conversation on its previous account keeps the session on that account.

### `tayassu status`

```bash
tayassu status
tayassu status --provider claude
tayassu status --model claude
```

Shows login status, measured quota, cooldowns, last probe time and active sessions per account.
Providers that only report authentication show `unknown` quota. Probe failures include the cause and a recovery command.

### `tayassu dash`

Interactive terminal dashboard. `r` probes every account, `c` clears cooldowns
for the selected account, and `q` quits. Quota is shown per model bucket, one
line per bucket, with active cooldown countdowns. The details show login status
and the last probe attempt. If the probe fails, earlier quota readings appear
as stale. Windows marked `disabled` by AGY are omitted from the quota display.

### `tayassu probe`

```bash
tayassu probe
tayassu probe --id agy-student
tayassu probe --notify
```

Runs each provider's non-interactive usage check and records cooldowns. With
`--notify`, `notify-send` is used when a previously active cooldown clears.

Quota is displayed as percentage remaining. AGY reports remaining quota for
model groups. Codex reports used percentages for account windows and any
additional limit IDs returned by its App Server. Copilot reports remaining
premium-interaction entitlement for the account, or chat entitlement when the
plan has no premium-interaction allowance. A shared account-level
window is shown under `default` and applies across model buckets. A bucket
identifies a limit for routing. Use the provider's own model option after
`--` to choose an actual model when needed.

#### Claude Code

The probe checks authentication first, then opens Claude Code's built-in
`/usage` panel in a temporary terminal. It captures current session and
weekly plan limits, including family-specific windows when Claude reports
them. It converts usage consumed into quota remaining. If the panel is
unavailable or says the data is cached, quota remains unknown.

```bash
tayassu probe --id claude-host
tayassu status --provider claude --model sonnet
tayassu run claude --model sonnet --require-quota
```

Strict routing passes `opus`, `sonnet`, or `haiku` to Claude as the launch
model when selected. With a family-specific limit in the snapshot, specify
the family so the router can check both the family and shared windows. To let
a chain try families in order, use targets such as `claude:sonnet` and
`claude:opus`.

#### OpenCode

Run the probe from the project where you use OpenCode. The model catalog is
project-specific. Tayassu evaluates each discovered model using the limits
of its integration.

```bash
tayassu probe --id opencode-host
tayassu status --provider opencode --list-models
tayassu status --provider opencode --model github-copilot/gpt-example
tayassu run opencode --require-quota --dry-run
tayassu run opencode --require-quota
```

Replace `github-copilot/gpt-example` with a model ID from `--list-models`.
With `--require-quota`, Tayassu forwards the selected model to OpenCode. A
chain can use `opencode:default` to choose from measured models or a target
such as `opencode:github-copilot/gpt-example` to pin the model.

- **OpenRouter** uses the active stored API key to read the key budget and
  shared daily free-model requests. The USD budget is shown separately.
  Paid models remain of unknown request quota. The daily meter does not
  predict per-minute throttling or provider capacity.
- **OpenCode Go** uses the active stored API key to read shared rolling,
  weekly, and monthly usage windows.
- **GitHub Copilot** requires an explicit link. In the OpenCode account's
  existing `[[accounts]]` entry in `config.toml`, set
  `opencode_copilot_account = "copilot-host"` only if that Tayassu Copilot
  profile is signed into the same GitHub account as the sole Copilot
  connection in OpenCode. Rerun the probe after changing either profile.
- **Other integrations** appear in the model list with unknown quota unless
  a supported meter applies.

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

Enables autocompletion for Tayassu commands, flags, providers, account IDs and
fallback chains. In Zsh, initialize completion with `compinit` before evaluating
the snippet.

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
an exclusive file lock, and account management and imports use a separate
lock for configuration changes.

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

### Automatic routing and probing

`tayassu auto run` uses the project fallback chain, or the global `auto`
chain when no project chain is configured. Without either, it tries enabled
providers in account priority order. It refreshes quota and selects only
a target with confirmed quota. Use `--chain NAME` to override the chain or
`--dry-run` to inspect the last recorded measurements.

`tayassu auto probe` refreshes accounts immediately, then installs (if absent)
and enables the systemd user timer. Existing unit files are preserved. The
user systemd manager must be available. In the dashboard, `r` starts a probe
in the background so navigation remains responsive.

In `tayassu dash`, select an account with `j`/`k` to see its individual
quota windows. Press `m` to switch between windows and models, `g` to cycle
model quota scopes, and `Page Up`/`Page Down` to move between pages. Model
percentages reflect shared limits when the provider reports a shared scope.
