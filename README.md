# Arana

**Read and search the web from AI agents, scripts, and the terminal.**

Arana is a Rust reimplementation of [Agent-Reach](https://github.com/Panniantong/Agent-Reach). It preserves the upstream 14-channel, 40-action catalog and the `agent-reach` command alias. The CLI returns structured JSON with source URLs and backend results. Agent clients can use a shared skill or connect to a local MCP server.

[Releases](https://github.com/mevarx/Arana/releases) · [Documentation](DOCS.md) · [Agent integrations](docs/agent-plugins.md) · [Security](SECURITY.md)

## Quick start

Download the [v0.1.0 pre-release](https://github.com/mevarx/Arana/releases/tag/v0.1.0) for Windows x64, Linux x64, or macOS ARM64. Verify the archive against its `.sha256` file, extract it, and put `arana` (`arana.exe` on Windows) on your `PATH`. See [Getting started](docs/getting-started.md) for installation and optional setup.

```sh
arana version
arana channels
arana read https://example.com
arana github read rust-lang/rust
```

These examples do not require an account, but upstream services can impose rate limits or other restrictions. Run `arana doctor` to see which optional backends are available.

To build from a source checkout with Rust and Cargo installed:

```sh
cargo build --workspace --release --locked
```

The binaries are in `target/release/`.

## What Arana can read

| Content | Channels |
| --- | --- |
| General web pages and search | `web` |
| Repositories and communities | `github`, `v2ex`, `reddit` |
| Video, subtitles, and transcription | `youtube`, `bilibili`, `xiaoyuzhou` |
| Social posts and profiles | `twitter`, `xiaohongshu`, `instagram`, `facebook` |
| Finance and jobs | `xueqiu`, `linkedin`, `boss` |

Actions vary by channel. Run `arana channels` for the catalog, or see the [command reference](docs/commands.md) for arguments, backends, and prerequisites. Arana does not post, vote, follow, or send messages.

Use the JSON `ok` field to distinguish success from failure. Content actions return `items` on success and `error.code` and `error.hint` on failure. `meta.attempts` records which backends were tried or skipped. See [Contract notes](CONTRACT_NOTES.md) for compatibility details.

## Agent integrations

| Integration | What it exposes |
| --- | --- |
| Shared skill | Instructions for using the installed CLI and interpreting its results |
| Local stdio MCP (`arana mcp`) | `list_capabilities`, `read_url`, and `search` for public web content; no login, setup, browser management, or arbitrary commands |

The [agent integration guide](docs/agent-plugins.md) covers Hermes, Codex, Claude Code, Google Antigravity, Cursor, Devin, OpenCode, Command Code, Oh My Pi, DeepSeek Harness, and Pi. It distinguishes documented setup from integrations tested in a client. **The published v0.1.0 binaries predate MCP.** Build from a checkout that includes `arana mcp` to try it.

## Privacy and security

Arana stores its state under `~/.arana/` by default and uses a dedicated browser profile for supported login flows. Users sign in directly. Arana does not ask for passwords or read personal browser cookie databases. Read requests may go to the selected platform or an external backend. Treat returned web content as untrusted data, not instructions.

See [Configuration](docs/configuration.md) for state and credentials, [Security design](docs/security.md) for trust boundaries, and [Security policy](SECURITY.md) to report a vulnerability privately.

## Development

See [Contributing](docs/contributing.md) for local checks and [Verification](docs/verification.md) for what offline and live tests cover. GitHub Actions runs offline checks on Linux, Windows, and macOS. Live HTTP probes are opt-in; a passing offline run does not establish live coverage for every action.

## License

[MIT](LICENSE). Derived from [Agent-Reach](https://github.com/Panniantong/Agent-Reach) by Agent Eyes. See [NOTICE](NOTICE) and [third-party notices](THIRD_PARTY/obscura/README.md).
