# arana

**Read-only internet access for AI agents — one CLI, one JSON contract, 14
platforms.**

arana gives an agent read access to the open web, YouTube, GitHub, V2EX,
Bilibili, Xueqiu, Reddit, LinkedIn, X/Twitter, Xiaohongshu, Instagram,
Facebook, BOSS and Xiaoyuzhou. It never posts, follows, messages or moderates
anywhere. It is a Rust reimplementation of
[Agent-Reach](https://github.com/Panniantong/Agent-Reach) 1.6.1, pinning the
same 14-channel / 40-action catalog so existing prompts and skill files keep
working — with native backends, private tool installs and a browser bus that
the original shells out to Python for.

```console
$ arana github read rust-lang/rust
{"schema_version":1,"ok":true,"channel":"github","action":"read","backend":"github-api",
 "items":[{"title":"rust-lang/rust","url":"https://github.com/rust-lang/rust", …}],
 "meta":{"elapsed_ms":812,"attempts":[{"backend":"github-api","result":"ok"}]},"error":null}
```

> **Status: pre-release.** The offline gate is green (285 tests). Release
> acceptance is not yet met: a real recording measured 23 of the 34 required
> live passes, because logged-in platforms, the OpenCLI bridge and provider
> keys were unavailable on the recording host. Failures were recorded and none
> were waived. See
> [Verification](docs/verification.md#current-measured-state) for the exact
> numbers and what remains open.

## Why arana

- **One contract.** Every action returns the same envelope: `items` with
  `title`/`url`/`author`/`published_at`/`text`/`raw`, plus `meta.attempts`
  explaining which backend answered and why others were skipped or paused.
  Exit code is non-zero exactly when `ok` is false.
- **Native where it matters.** Web, GitHub, V2EX, Xueqiu, Bilibili, YouTube and
  transcription are implemented in Rust, with no Python runtime. Only the
  browser-session sites (opencli) and two site CLIs still ride external
  processes — pinned, versioned and resolved privately.
- **Resilient by design.** Ordered backends with health pauses, channel
  cooldowns, cross-process pacing, a single shared deadline and a public-web
  fallback policy — all matching upstream constants.
- **Private by construction.** 0700/0600 state, atomic writes, OS file locks,
  symlink/junction guards, and on Windows a DACL derived from the process
  token rather than `USERNAME`.
- **Deletes only what it created.** Per-install manifests and an ownership
  ledger mean a file you drop inside an owned tree survives removal.
- **Honest evidence.** Live probes and release acceptance record failures as
  failures; a golden recording is never presented as a live pass.

## Quick start

```sh
cargo build --workspace --release --locked
./target/release/arana setup
./target/release/arana doctor
```

Then run something that needs no account:

```sh
arana channels                     # the frozen 40-action catalog
arana read https://example.com     # auto-routes to the right channel
arana v2ex hot --limit 5
arana xueqiu quote AAPL
```

Prerequisites, first-run details and platform notes:
**[Getting started →](docs/getting-started.md)**

Prebuilt archives for Windows, Linux and macOS are attached to
[Releases](https://github.com/mevarx/Arana/releases); the change history lives
in [CHANGELOG.md](CHANGELOG.md).

## Commands

```sh
arana <channel> <action> [args] [--timeout S] [--backend LABEL] [--verbose]
arana read <url>
arana channels

arana setup | add <channel> | remove <channel> | update [--check|--sync-only]
arana login <channel> | logout <channel> | configure <key> [--stdin]
arana doctor [--text] | check [channels...] [--text] [--search]
arana uninstall [--dry-run|--yes] | skill [status|link <dir>|unlink <dir>|update]
arana browser setup|status|stop|profiles|use|export|import|logout
```

| Channel | Actions |
|---|---|
| `web` | search, read |
| `v2ex` | hot, node, read, user |
| `github` | search, read |
| `bilibili` | search, read, hot, subtitle |
| `youtube` | search, read, subtitle, transcribe |
| `xueqiu` | quote, search, hot_posts, hot_stocks |
| `xiaohongshu` | search, read, comments |
| `boss` | search, read |
| `linkedin` | jobs, read |
| `reddit` | search, read, subreddit |
| `twitter` | search, profile, tweets, thread |
| `facebook` | search, profile |
| `instagram` | search, profile, posts |
| `xiaoyuzhou` | transcribe |

Full reference — parameters, backends, error codes, management semantics:
**[Commands →](docs/commands.md)** · Naming and contract decisions:
**[CONTRACT_NOTES.md](CONTRACT_NOTES.md)**

## How it works

Five crates, one binary:

| Crate | Role |
|---|---|
| `reach-core` | envelope, errors, atomic state, deadline clock, dispatch, acceptance gate |
| `reach-channels` | channel registry and every backend (HTTP, media, process adapters) |
| `reach-tools` | tool registry, private installs, process runner, SSRF + archive guards, ownership |
| `reach-browser` | Chromium lifecycle, CDP, slots, cookies, login pin, POSIX pipe, OpenCLI bridge |
| `arana` | CLI surface, management verbs, JSON rendering, `agent-reach` alias |

A request runs through one clock and ordered backends, with health pauses,
cooldowns and pacing enforced before any call. The runtime may prepend
`obscura` to `web read` (pinned release, sha256-verified, private) without
changing the frozen catalog bytes; Jina and Exa remain fallbacks.

Pipeline, state layout and browser bus internals:
**[Architecture →](docs/architecture.md)**

## State, secrets and privacy

Everything lives under `~/.arana/` (override the *state root* with
`ARANA_HOME`). Secrets are written only through `--stdin`, stored 0600, never
echoed and never included in envelopes or logs:

```sh
printf '%s' "$GROQ_KEY" | arana configure groq-key --stdin
arana configure browser-port 9333
```

Login is always performed by a human in the dedicated browser window: arana
never asks for a password, never types one, and never reads a personal browser
cookie database. Configuration keys, environment variables and the full state
layout: **[Configuration →](docs/configuration.md)**

## Security model

- Private state: 0700/0600, atomic writes, OS locks, symlink and Windows
  reparse-point refusal, token-derived DACL identity.
- Outbound requests: SSRF checks on literal-IP tricks, DNS and every redirect
  hop; streaming response caps; obscura's private-network protection never
  disabled for user URLs.
- Archives and downloads: two-pass validation before any write, sha256 pins,
  link/device/traversal/expansion refusal.
- Removal: registry-derived paths, ownership ledger, per-install manifests,
  hash-matched deletion, `left_untouched` for anything else.
- Browser: dedicated profile, ownership proof before use or stop, never adopts
  a stranger on the CDP port.

Threat model and the exact rules: **[Security →](docs/security.md)**

## Verification and release gate

```sh
python scripts/verify.py --release      # python suites, fmt, clippy, tests, release build
python scripts/benchmark.py             # size + fresh-process startup
python scripts/smoke.py --binary target/release/arana.exe
```

- **285 Rust tests**, 11 Python tests, strict Clippy, locked release build — all
  green; **19 live tests** are opt-in and run explicitly.
- Release binary **7.36 MiB**, `channels` startup **19.7 ms p95** (targets: 30
  MiB, 50 ms).
- Real Chromium: parallel reads, login-pin survival, CDP smoke, `BUSY` after a
  measured 120 s slot wait.
- Release acceptance: **23 / 34 required live passes, 11 failures, 6 upstream
  waivers — gate open by design.** Failures are recorded, never waived.

What each gate proves, how to reproduce it, and the known limits:
**[Verification →](docs/verification.md)**

## Platform support

| Platform | State |
|---|---|
| Windows x64 | Primary development and all live testing (Chromium, CDP, WebSocket transport) |
| Linux x64 | Compiles cross-target with strict lints; POSIX pipe transport implemented; runtime not yet exercised here |
| macOS | Cross-checked; Chrome-for-Testing private fallback can refuse framework symlinks |

CI is configured for Linux, Windows and macOS. **No remote CI run is claimed** —
the workflow exists, the results above come from local execution.

## Non-goals

- No writes: posting, commenting, voting, following or moderating anywhere.
- No credential handling: no password prompts, no browser-profile reading, no
  cookie-database decryption.
- No multi-user or hosted service; no MCP server surface; no proxy management.

## Contributing

Bug reports, channel fixes and test improvements are welcome. The contract is
frozen, tests are written before code, and live probes must report honestly.

Development setup, per-suite conventions and the pre-PR gate list:
**[Contributing →](docs/contributing.md)**

## License and attribution

MIT — see [LICENSE](LICENSE). arana is a derivative work of
[Agent-Reach](https://github.com/Panniantong/Agent-Reach) by Agent Eyes; the
upstream notice is preserved in [NOTICE](NOTICE).

Third-party components:

- **obscura** 0.2.7 (Apache-2.0) — pinned external binary, not a vendored
  dependency; full license text in
  [`THIRD_PARTY/obscura/LICENSE`](THIRD_PARTY/obscura/LICENSE).
- **OpenCLI** 2.0.3 and its browser extension 1.0.27 — installed privately
  from pinned, checksum-verified assets.
