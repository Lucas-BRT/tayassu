# Acceptance criteria for isolated agy authentication

Status: proposed. The private-keyring prototype ran on Pop!_OS 24.04 with
Antigravity CLI 1.5.18 and GNOME Keyring 46.1. The released Tayassu launcher
still uses the desktop Secret Service.

## Requested provider behavior

Allow callers to select an OAuth credential identifier and require OS keyring
storage. Apply both settings to initial lookup, sign-in, token refresh and
logout, including any helper processes. Return a distinguishable failure to
the caller when a required credential cannot be stored. Treat a failed save
as an incomplete login even if the current process can use its in-memory token.

## Release checks

Use dedicated accounts A and B and record the exact client and daemon versions.
Observe method names, paths, exit status and account labels. Keep token
payloads, OAuth callback URLs and keyring files out of test artifacts.

| Condition | Required observation |
| --- | --- |
| A and B sign in and reopen in alternating order | Each fresh process recovers its own identity. |
| Both sessions and a background probe overlap | Each operation uses its designated account. |
| The private service is locked before a scheduled probe | The probe reports the lock without prompting or selecting the desktop credential. |
| A stale fallback file exists before onboarding | Tayassu reports its presence before launch and preserves it for the owner. |
| The service disappears during sign-in or refresh | Strict mode reports failure without reading or writing a fallback credential. |
| B signs out | A can reopen and B requires authentication. |
| The machine reboots | Profiles remain distinct after unlock. A probe before unlock reports the locked state. |

## Evidence and outstanding work

Two real accounts retained separate identities after restarting their private
services. Concurrent isolation was tested with synthetic secrets. Real
concurrent sessions, refresh-time failure and reboot remain unverified.
During a real sign-in with an unavailable keyring and a blocked fallback
path, the CLI displayed an authenticated session despite logging a failed
save. A fresh process subsequently requested authentication.

Private buses separate routine lookups. A same-user process with the private
bus address retrieved a synthetic secret from the unlocked service. The
profile boundary therefore provides no guarantee against hostile code running
as that Linux user.
