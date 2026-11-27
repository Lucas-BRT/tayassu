# tayassu Status

Ran `tayassu status` on the tayassu CLI.

```
ACCOUNT            PROVIDER   PRI  STATE              QUOTA                        LABEL
agy-host           agy        50   READY              claude:0% gemini:0%(1h21m)   agy (imported)
claude-host        claude     50   READY              default:ok                   claude (imported)
codex-host         codex      50   READY              default:ok                   codex (imported)
copilot-host       copilot    50   READY              default:ok                   copilot (imported)
opencode-host      opencode   50   ERROR              -                            opencode (imported)
```

**Summary:**
- 5 accounts configured (all imported via `tayassu account add`)
- 4 accounts are READY, 1 (opencode-host) is in ERROR state
- The opencode account has no quota information displayed
- All accounts have priority 50