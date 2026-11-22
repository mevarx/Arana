# Getting started

## What Arana is

Arana is a read-only web agent: one CLI and one local MCP server that give an agent read access to 14 platforms and 40 actions through a single JSON contract. Outbound read requests are all it does — it never posts, follows, messages, or moderates anywhere.

It is a Rust reimplementation of [Agent-Reach](https://github.com/Panniantong/Agent-Reach) 1.6.1 (`c70ae6d`), with the same command surface and envelope, so existing prompts, scripts, and skill files keep working. Release archives include the `agent-reach` alias alongside `arana`.

No separate Chrome or OpenCLI setup is required for the public MCP workflow. For interactive or rendered pages, see [when to use the host browser](agent-plugins.md#when-to-use-the-host-browser). The optional standalone channel tools below are not required for the public research demo.

## Requirements

| Need | When |
|---|---|
| Rust stable (edition 2021) | to build from source |
| Chrome or Chromium | browser-backed channels (`browser setup`, logins, OpenCLI sites) |
| Node.js | OpenCLI bridge (installed privately if absent) |
| `yt-dlp`, `ffmpeg` | `youtube read/search/subtitle/transcribe`, `xiaoyuzhou transcribe` |
| a Groq or OpenAI key | audio transcription only |
| No account | `web`, `v2ex`, `github`, `xueqiu` quote/search, and public `bilibili` reads; some actions install a private tool on first use |

External tools (`obscura`, `yt-dlp`, `bili-cli`, `boss-agent-cli`, `opencli`, `node`, `uv`, `ffmpeg`, Chrome) are resolved in this order: an explicit `ARANA_*_BIN` override, Arana's private copy under `~/.arana/tools/`, then the system `PATH`. A system copy that satisfies the version policy is used as-is and never upgraded or shadowed; a private copy is installed only when there is no usable one, and never into the global environment.

## Install a release

Download the archive for your platform from the [v0.3.0 release](https://github.com/mevarx/Arana/releases/tag/v0.3.0). If that tag does not have published assets yet, [build from source](#build-and-test) instead:

| Platform | Archive |
|---|---|
| Windows x64 | `arana-windows-x64.zip` |
| Linux x64 | `arana-linux-x64.zip` |
| macOS ARM64 | `arana-macos-arm64.zip` |

Each archive is published with a `.sha256` sidecar. Verify it before extracting:

```sh
# Linux
sha256sum -c arana-linux-x64.zip.sha256

# macOS
shasum -a 256 -c arana-macos-arm64.zip.sha256
```

```powershell
# Windows PowerShell
$expected = (Get-Content .\arana-windows-x64.zip.sha256).Split()[0]
$actual = (Get-FileHash .\arana-windows-x64.zip -Algorithm SHA256).Hash.ToLower()
if ($actual -ne $expected) { throw "SHA-256 mismatch" }
```

The archive contains:

```text
arana / arana.exe               the CLI
agent-reach / agent-reach.exe   the compatibility alias
skills/arana/SKILL.md           the shared agent skill
plugin.json, mcp.json, .mcp.json
.claude-plugin/, .agents/, .antigravity-plugin/
README.md, DOCS.md, SECURITY.md, CONTRACT_NOTES.md, docs/, CHANGELOG.md,
LICENSE, NOTICE
THIRD_PARTY/obscura/
```

Extract it and put `arana` (`arana.exe` on Windows) on your `PATH`. The skill and manifests are optional; installing the archive does not install the agent package into any host. Use the [agent integration guide](agent-plugins.md) for that.

Alternatively, from a repository checkout with Python 3.11 or newer:

```sh
python scripts/install.py --version v0.3.0 --install-dir ./arana-bin
```

The installer supports the same three targets as the archives (Windows x64, Linux x64, macOS ARM64). It downloads the pinned archive and its SHA-256 sidecar, verifies the digest, extracts only the CLI binary, and prints its absolute path. It refuses to overwrite an existing executable, does not install optional tools, and does not edit `PATH`. Add the chosen directory to user `PATH` for terminal hosts, or set the absolute executable path in a GUI host's MCP configuration. To remove it, delete the executable you installed and undo any `PATH` change you made.

## Build and test

```sh
cargo build --workspace --release --locked     # target/release/arana (+ agent-reach)
cargo test --workspace --locked                # offline suites
python scripts/verify.py --release             # full local gate, keeps real logs
```

On Windows append `.exe`. The release binary is self-contained; Rust is not needed to run it.

## First run

```sh
arana version          # version, upstream revision, source signature
arana channels         # the frozen catalog: 14 channels, 40 actions
arana doctor           # per-channel health: tools, versions, login, next action
arana setup            # inspect tools, install what is missing, materialise the skill
```

`setup` is idempotent and safe to re-run. It never asks for a password, never touches a personal browser profile, and never edits your global `PATH`.

Then try something that needs no account:

```sh
arana github read rust-lang/rust
arana v2ex hot --limit 5
arana web read https://example.com
arana read https://www.youtube.com/watch?v=jNQXAC9IVRw   # auto-routes to youtube
```

Every non-MCP invocation prints exactly one JSON envelope on stdout. `arana mcp` reserves stdout for protocol frames and sends diagnostics to stderr.

```json
{"schema_version":1,"ok":true,"channel":"github","action":"read","backend":"github-api",
 "items":[{"title":"rust-lang/rust","url":"https://github.com/rust-lang/rust","author":null,
 "published_at":null,"text":"…","raw":{…}}],
 "meta":{"elapsed_ms":812,"attempts":[{"backend":"github-api","result":"ok"}]},"error":null}
```

Exit code is `0` when `ok` is true and non-zero otherwise; branch on `ok` and `error.code`, never on error prose. Diagnostics go to stderr with `--verbose`, so stdout stays parseable.

## Connect an agent

The public MCP server needs no account or browser setup:

```sh
arana mcp     # newline-delimited JSON-RPC 2.0 over stdio
```

It exposes `list_capabilities`, `read_url`, `read_many`, and `search`. Use the [agent integration guide](agent-plugins.md) to install the shared skill or register the server in Hermes, Claude Code, Codex, Cursor, Antigravity, and the other supported hosts.

## Adding platforms

```sh
arana add xiaohongshu       # add the channel and install its private tools
arana login xiaohongshu     # opens the site's login page; you log in yourself
arana browser setup         # dedicated Chromium profile + OpenCLI bridge
```

Login is always performed by a human in the dedicated browser window. Arana never types credentials, never asks for them, and never reads a browser cookie database.

## Next steps

- [Agent integrations](agent-plugins.md) — host setup, MCP bounds, and evidence status.
- [Commands](commands.md) — the full CLI surface, browser routing, and every action.
- [Configuration](configuration.md) — state root, secrets, environment variables.
- [Security](security.md) — what Arana owns and what it refuses to touch.
- [Verification](verification.md) — how the gates work and what is proven.
