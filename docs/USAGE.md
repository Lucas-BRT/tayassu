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

To inspect an import without changing files, run `tayassu import --dry-run`.
The preview lists discovered files and their destinations. Run `tayassu import`
to copy them, then follow any login instructions and check `tayassu ls`.

To add a separate account instead, run `tayassu add <provider> [id]` and
complete the provider's login flow. Use `tayassu where <provider>` to inspect
the account selected before launching it.

```bash
tayassu setup            # install the background probe timer
tayassu import           # discover installed CLIs and copy their credentials
tayassu add agy work     # or register an account yourself and log in
tayassu                  # same as `tayassu ls`: quota and cooldown state
tayassu run agy -- --help
```

`tayassu import` never deletes binaries or updater directories. It copies host
auth files by default. With `--move-auth`, tayassu copies the files and compares
each removable credential with its copy before deleting any originals for that
provider. An error during removal can still leave some originals already moved.

## Command overview

| Command | Alias | Purpose |
| ------- | ----- | ------- |
| `ls [provider]` | (bare `tayassu`) | Quota, cooldowns and sessions per account; `--json`. |
| `add <provider> [id]` | | Register an account and log in (`--no-login` to skip). |
| `rm <id>` | | Remove an account from `config.toml`. |
| `login <id>` | | Re-authenticate inside the isolated profile. |
| `import` | | Discover installed CLIs and import credentials. |
| `use [account]` | | Pin account, `--chain` or `--policy` for this project (`.tayassu.toml`); `--global` for the policy. |
| `run <provider> -- ...` | `r`, `x` | Route to the best account and run its CLI, failing over on rate limits. |
| `where <provider>` | | Show the account, executable and environment that `run` would use. |
| `probe [id]` | | Refresh quota windows. |
| `dash` | | Interactive dashboard. |
| `doctor` | | Validate binaries, profiles and credential permissions. |
| `activate <shell>` | | Print wrapper functions and completion for `eval`. |
| `setup` | | Install the systemd probe timer; `setup shell` opts in to wrappers. |
| `config ls\|get\|set` | | Read or change global settings (`default-policy`, `update-check`). |
| `update` | | Update tayassu itself (verifies SHA-256; `--check`, `--pre`, `--version`). |
| `providers update` | | Run each provider CLI's own updater. |
| `uninstall` | | Remove shell hooks, systemd timer, and binary (`--purge` also deletes profiles). |

`tayassu cooldown ls|set|clear` exists for manual overrides but is hidden from `--help`.

### Cached quota and offline use

Every probe is cached in `state.json`, so `tayassu ls` works offline. Quota
text shows how much to trust it: `43%` (probed in the last 15 minutes), `~43%`
(older reading), `reset✓` (the recorded reset time has passed, so the quota is
presumed restored and the account is routable) and the `CHECKED` column shows the
probe age. A failed probe never discards cached windows. When the network
returns, `tayassu probe` overwrites the cache; it is always the source of truth
and reports when an estimated reset turned out wrong.

### `tayassu add`

```bash
tayassu add agy student --label "Student plan" --priority 10
tayassu add claude                  # id defaults to `claude`, then `claude-2`, ...
tayassu add claude work --binary /opt/claude/bin/claude --no-login
```

Creates an isolated profile with clean authentication (clearing any stale
credentials left behind under that profile id, without copying host auth),
writes the account to `config.toml`, then starts the provider's login flow. On
Linux, new `agy` accounts enable a private keyring by default (pass
`--shared-keyring` to share the desktop Secret Service instead).

### `tayassu run`

```bash
tayassu run claude -- "explain this repository"
tayassu run --account codex-work -- --version
tayassu run --chain claude --model claude -- "fix the tests"
tayassu where claude
```

| Option          | Effect                                                              |
| --------------- | ------------------------------------------------------------------- |
| `--account ID`  | Force a specific account instead of routing automatically.          |
| `--chain NAME`  | Use a named cross-provider fallback chain from `config.toml`.       |
| `--model NAME`  | Model bucket to evaluate cooldowns against (default `default`).     |
| `--policy NAME` | Override the selection policy (`priority`, `most-quota`, `soonest-reset`). Default is `most-quota`. |
| `--dry-run`     | Print the selected account and environment without spawning.        |
| `--no-failover` | Do not retry on the next account after a rate limit (failover is on by default). |

Arguments after `--` are passed verbatim to the underlying CLI.

### `tayassu use`

```bash
tayassu use work                      # prefer account `work` for its provider in this project
tayassu use --chain claude-any        # default fallback chain for this project
tayassu use --policy priority         # project policy
tayassu use --policy most-quota --global
```

### `tayassu dash`

Interactive terminal dashboard. `r` probes every account, `c` clears cooldowns
for the selected account, and `q` quits.

### `tayassu probe`

```bash
tayassu probe
tayassu probe agy-student
tayassu probe --notify
```

Runs each provider's non-interactive usage check and records cooldowns. With
`--notify`, `notify-send` is used when a previously active cooldown clears.

### `tayassu doctor`

`--strict` exits non-zero on warnings as well as errors.

### `tayassu activate` and `tayassu setup`

```bash
eval "$(tayassu activate bash)"        # zsh: same; fish: tayassu activate fish | source
tayassu setup                          # install the background probe timer
tayassu setup shell                    # opt in to wrappers for detected shells
tayassu setup completion               # install or repair completion only
tayassu setup --no-systemd
```

`activate` prints wrapper functions that route `agy`, `claude`, `codex`,
`copilot` and `opencode` through `tayassu run` (bypass with `command <cli>`),
plus dynamic tab completion for providers, accounts, chains and policies.
When no account exists for a provider, its wrapper runs the host CLI directly.
Wrappers generated by this version also run the host CLI if tayassu has been
uninstalled. After upgrading or uninstalling an older version, restart the shell
to remove its previously loaded wrappers.
`setup shell` appends the `eval` line to `~/.bashrc`/`~/.zshrc` and writes
`~/.config/fish/conf.d/tayassu.fish`. `setup` installs `tayassu-probe.service` and
`tayassu-probe.timer` under `$XDG_CONFIG_HOME/systemd/user/` (enable with
`systemctl --user enable --now tayassu-probe.timer`). It is idempotent.
Installing these units does not enable the timer. Check its status with
`systemctl --user status tayassu-probe.timer`; `--no-systemd` skips installation.
Use `tayassu auto probe` to refresh quotas and enable the timer. If the systemd
user manager is unavailable, use `tayassu probe` to refresh quotas manually.

### `tayassu update`, `tayassu uninstall`, and `tayassu providers update`

```bash
tayassu update                  # download latest stable release, verify SHA-256, replace binary
tayassu update --check          # check only
tayassu update --pre            # allow pre-releases
tayassu update --version v0.2.0 # install a specific tag
tayassu uninstall --dry-run     # preview removal
tayassu uninstall --purge       # also remove config, state and isolated profiles
tayassu providers update        # run each provider CLI's native updater with real $HOME
```

If GitHub's unauthenticated API quota is exhausted, retry after the time shown
or set `TAYASSU_GITHUB_TOKEN`. With GitHub CLI already signed in, run:

```bash
TAYASSU_GITHUB_TOKEN="$(gh auth token --hostname github.com)" tayassu update
```

Tayassu uses the token only for GitHub release metadata requests.

When running interactively in a terminal, `tayassu` checks for new releases in
the background at most once every 24 hours and prints a one-line notice on
stderr when an update is available (never during `run` or `activate`). Disable
with `tayassu config set update-check off` or `TAYASSU_NO_UPDATE_CHECK=1`.

## Configuration

`tayassu` reads `$XDG_CONFIG_HOME/tayassu/config.toml` (default
`~/.config/tayassu/config.toml`). Accounts are normally created by
`tayassu import` or `tayassu add`, but the file can be edited directly.

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
- **Login required**: run `tayassu login <id>`; some providers require a
  fresh OAuth flow after credentials are copied.
- **Broken profile links**: run `tayassu doctor --strict`.
- **Codex app-server does not become ready**: on Linux, tayassu can restore a
  missing control link when the selected profile's daemon is still listening.
  If the daemon is unavailable, try `tayassu run codex -- --no-daemon`.
  Refer to the Codex error for resume and fork arguments.
- **A provider stopped working after an upgrade**: run `tayassu providers update`, then
  `tayassu doctor`.
- **Wrapper loops or wrong account**: call the CLI directly with
  `command <cli> ...`, or check the account selected by
  `tayassu run <provider> --dry-run`.
- **Rate limited during a run**: failover is on by default; use a chain that includes
  another provider.

### Separate Antigravity accounts on Linux

New `agy` accounts created with `tayassu add agy <id>` enable
`agy_private_keyring = true` automatically on Linux (pass `--shared-keyring` to
share the desktop Secret Service instead). For existing `[[accounts]]` entries
in `config.toml` that should use an isolated Google sign-in, add
`agy_private_keyring = true`. Leave the option unset for profiles that should
keep using the desktop Secret Service.

The mode requires an active `XDG_RUNTIME_DIR`, plus `dbus-daemon`, `gdbus`,
`gnome-keyring-daemon` and `systemd-ask-password` on `PATH`. Each enabled profile
gets a private session bus under `XDG_RUNTIME_DIR` and an encrypted login keyring
under its isolated `home/.local/share/keyrings`.

1. Run `tayassu add agy <id>` (or `tayassu login <id>` for an existing profile)
   in a terminal. Choose a password for the private keyring on first use, then
   complete the Antigravity sign-in. Exit the Antigravity session so Tayassu can
   verify authentication through a fresh `/usage` process.
2. Repeat for the second account with its own Google sign-in.
3. Run `tayassu run agy --account <id>` for each account and check the identity
   in the Antigravity banner. After a reboot, interactive `tayassu run` can
   unlock an already initialized keyring directly; background `tayassu probe`
   reports the locked state without prompting.

If the profile contains `home/.gemini/antigravity-cli/antigravity-oauth-token`,
Tayassu reports the file and preserves it. Move it out of the profile before
onboarding. Tayassu also checks for this file after the provider exits. The
Antigravity CLI controls its file fallback during execution, so a failed
Secret Service write may still leave a credential in the profile. Private
services separate routine credential lookups between profiles under the
same Linux user.
