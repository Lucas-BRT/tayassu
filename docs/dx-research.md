# Developer experience study

Use isolated Linux environments and test credentials. Record the tayassu version,
shell, installed providers, systemd availability and starting configuration.
Do not collect credentials, tokens, private prompts or unredacted profile files.

## Sessions

Ask participants unfamiliar with the implementation to think aloud. Give each
task without prescribing commands. Note completion, elapsed time, incorrect
steps, documentation consulted and points of uncertainty. After each task ask:
"What did you expect to happen?" and "How confident are you in the result?"

1. Install tayassu and use an existing provider account.
2. Add a second account and determine which account will run.
3. Diagnose an account that is disabled or requires login.
4. Determine whether automatic quota probing is active.
5. Uninstall tayassu and run the host provider again in the same shell.

Repeat with bash, zsh and fish, and with and without a user systemd instance.
Include an environment with no provider and one with an existing installation.
Use mock provider executables for repeatability, then validate representative
flows against real providers in disposable accounts where available.

## Analysis

Group observations by task and severity. Record failed tasks and confusing
messages alongside successful attempts; do not infer satisfaction from speed.
Repeat the same tasks and environment after changes, comparing outcomes and
participant feedback. Record untested combinations rather than treating them
as passing. The Codex daemon failure requires a separately controlled
reproduction and must not be simulated as proof of a real recovery.
