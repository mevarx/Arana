# Architecture

Five crates, one binary, no runtime service.

| Crate | Responsibility |
|---|---|
| `reach-core` | Envelope and error contract, atomic private state with OS locks, the shared deadline clock, ordered dispatch with health/cooldown/pacing, strict argument parsing, release-acceptance validation. |
| `reach-channels` | The frozen 14-channel / 40-action registry and every backend: native HTTP (web, github, v2ex, xueqiu, bilibili), media (yt-dlp/ffmpeg/Whisper), and process adapters (opencli, bili-cli, boss-agent-cli) with private session homes. |
| `reach-tools` | Tool registry and private installs (pip venvs, npm prefixes, node/uv/ffmpeg/chrome trees, pinned obscura), bounded process runner, SSRF guard, archive extraction guards, ownership manifests. |
| `reach-browser` | Dedicated Chromium lifecycle, CDP client, concurrency slots, cookie import/export, login pin, idle watcher, POSIX pipe transport, OpenCLI bridge and extension. |
| `arana` | The CLI: argument surface, management verbs, JSON rendering, the `agent-reach` compatibility alias, and the build signature. |

## Request pipeline

```
argv ──▶ parse ──▶ validate params ──▶ select channel/action ──▶ dispatch
                                                                   │
        ┌──────────────────────────────────────────────────────────┘
        ▼
  for each backend in the action's order:
      skip if paused by health/cooldown
      run under the shared deadline  ──▶ ok ──▶ envelope
                                     └─ retryable ──▶ next backend
                                     └─ final error ──▶ stop
        │
        └─ public-web fallback per policy (never after a final error)
        ▼
  envelope {schema_version, ok, channel, action, backend, items, meta, error}
```

Rules that the code enforces rather than documents:

- **One clock.** A single deadline covers the invocation; setup/login steps
  pause it ("off the clock") so a human is never charged for the agent's budget.
- **Health pauses.** After `TIMEOUT` (60 s), `UPSTREAM_BROKEN` (120 s) or
  `NEED_SETUP` (60 s) a channel/action/backend is skipped and shown in
  `meta.attempts`; success clears it. A `RATE_LIMITED` pauses the whole channel
  for the reported `retry_after_seconds`, and a public-provider limit never
  pauses the destination channel.
- **Pacing.** Two `search`/`jobs` calls on the same channel are at least 10 s
  apart, enforced under a cross-process lock so parallel processes cannot both
  skip the wait.
- **Public-web policy.** A channel that is not added tries the public web first
  (budget `min(20 s, remaining/2)`); an added channel falls back to it only
  after transient failures, never after a final error.
- **Backend order is data.** `catalog` decides ordering; runtime may prepend
  `obscura` to `web read` without changing the frozen catalog bytes.

## State layout

```
~/.arana/
  state.json          # channels, tools (owner/scope/version), checks, backend_health,
                      # cooldowns, request_starts, skill links, owned_paths
  config.yaml         # secrets and settings (0600)
  SKILL.md            # embedded skill, materialised by setup
  sessions/<tool>/    # private fake HOME per external CLI
  browser/profile/    # dedicated Chromium profile
  browser/extension/  # pinned OpenCLI extension (unpacked, owned)
  browser-locks/      # slot, execution, pacing and watcher locks
  tools/              # private installs: pip venvs, npm prefixes, node, uv, ffmpeg, chrome, obscura
  cache/downloads/    # verified downloads (sha256-addressed)
  watcher.lock, login-pin.json
```

Writes are atomic (`tempfile` + rename), serialised by OS file locks, and
guarded against symlink/reparse components. Keys are `owner`, `scope`
(`private` / `session` / `system`), `kind` (`file` / `directory`), `version`
and a content hash.

## Browser bus

One Chromium instance per machine, started by arana with an arana-owned
profile:

- cold start is bounded at 90 s and the browser is verified to be *ours* (the
  CDP `user-data-dir` must match) before it is used or stopped;
- an unrelated process on the CDP port is never adopted or killed (`BUSY`);
- concurrency is 2 slots per site and 4 globally through OS locks; waiting is
  bounded at 120 s and then reports `BUSY`;
- idle close happens after 600 s, and an open login window pins the browser so
  a human can finish signing in;
- cookies cross the boundary only as CDP `Storage.getCookies`/`setCookies`
  JSON — never from a browser database.

On POSIX the browser is launched by a hidden long-lived watcher that owns the
child process and the CDP pipe (fd 3/4); the foreground CLI only waits for
readiness. On Windows the upstream WebSocket launch path is used, matching
upstream's explicit lack of pipe support there.

## Tools and ownership

Every installed path is recorded with an owner. Removal recomputes the path
from the registry (a forged ledger row cannot redirect a delete), refuses link
or junction roots, and deletes only files listed in the install's
`.arana-manifest.json`. Files someone else placed in an owned directory are
reported in `left_untouched` instead of being deleted; an owned install whose
recorded version is outside `floor`/`exclude` is repaired rather than trusted.
See [Security](security.md) for the full rule set.

## Acceptance harness

`scripts/record_release_acceptance.py` drives the real binary for every
non-waived action with fixed probe inputs and writes a manifest;
`reach_core::acceptance::validate` (mirrored in Python) enforces the frozen
40-action catalog, ≥34 real passes, the exact six upstream waivers, fresh
timestamps and a matching source signature. A manifest alone is never proof —
the ignored Rust acceptance test re-derives catalog and signature from the
binary and re-validates the recorded result.
