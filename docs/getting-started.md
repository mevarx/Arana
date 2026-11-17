# Getting started

## What arana is

arana is a read-only web agent: one CLI that gives an agent read access to 14
platforms and 40 actions through a single JSON contract. Outbound requests are
all it does — it never posts, follows, messages or moderates anywhere.

It is a Rust reimplementation of [Agent-Reach](https://github.com/Panniantong/Agent-Reach)
1.6.1 (`c70ae6d`), with the same command surface and envelope so existing
prompts, scripts and skill files keep working.

## Requirements

| Need | When |
|---|---|
| Rust stable (edition 2021) | to build from source |
| Chrome or Chromium | browser-backed channels (`browser setup`, logins, opencli sites) |
| Node.js | OpenCLI bridge (installed privately if absent) |
| `yt-dlp`, `ffmpeg` | `youtube read/search/subtitle/transcribe`, `xiaoyuzhou transcribe` |
| a Groq or OpenAI key | audio transcription only |
| Nothing else | `web`, `v2ex`, `github`, `xueqiu`, `bilibili read/hot` work with no account |

External tools (`obscura`, `yt-dlp`, `bili-cli`, `boss-agent-cli`, `opencli`,
`node`, `uv`, `ffmpeg`, Chrome) are resolved in this order: an explicit
`ARANA_*_BIN` override, arana's private copy under `~/.arana/tools/`, then the
system `PATH`. A system copy that satisfies the version policy is used as-is
and never upgraded or shadowed; a private copy is installed only when there is
no usable one, and never into the global environment.

## Build and test

```sh
cargo build --workspace --release --locked     # target/release/arana (+ agent-reach)
cargo test --workspace --locked                # offline suites
python scripts/verify.py --release             # full local gate, keeps real logs
```

On Windows append `.exe`. The release binary is self-contained; Rust is not
needed to run it.

## First run

```sh
arana setup            # inspect tools, install what is missing, link the skill
arana doctor           # per-channel health: tools, versions, login, next action
arana channels         # the frozen catalog: 14 channels, 40 actions
```

`setup` is idempotent and safe to re-run. It never asks for a password, never
touches a personal browser profile and never edits your global `PATH`.

Then try something that needs no account:

```sh
arana github read rust-lang/rust
arana v2ex hot --limit 5
arana web read https://example.com
arana read https://www.youtube.com/watch?v=jNQXAC9IVRw   # auto-routes to youtube
```

Every invocation prints exactly one JSON envelope on stdout:

```json
{"schema_version":1,"ok":true,"channel":"github","action":"read","backend":"github-api",
 "items":[{"title":"rust-lang/rust","url":"https://github.com/rust-lang/rust","author":null,
 "published_at":null,"text":"…","raw":{…}}],
 "meta":{"elapsed_ms":812,"attempts":[{"backend":"github-api","result":"ok"}]},"error":null}
```

Exit code is `0` when `ok` is true and non-zero otherwise; branch on `ok` and
`error.code`, never on error prose. Diagnostics go to stderr with `--verbose`,
so stdout stays parseable.

## Adding platforms

```sh
arana add xiaohongshu       # install that channel's private tools
arana login xiaohongshu     # opens the site's login page; you log in yourself
arana browser setup         # dedicated Chromium profile + OpenCLI bridge
```

Login is always performed by a human in the dedicated browser window. arana
never types credentials, never asks for them and never reads a browser cookie
database.

## Next steps

- [Commands](commands.md) — the full CLI surface and every action.
- [Configuration](configuration.md) — state root, secrets, environment variables.
- [Security](security.md) — what arana owns, what it refuses to touch.
- [Verification](verification.md) — how the gates work and what is proven.
