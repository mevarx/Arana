# Configuration

## State root

All state lives under one directory, `~/.arana/` by default.

| Variable | Effect |
|---|---|
| `ARANA_HOME` | Overrides the **state root** (not your home). Intended for tests and isolated installs. |

The root is created privately (0700 on Unix; a protected current-user DACL on Windows) and every read/write is symlink-guarded. Do not place it in a shared, temporary, or world-writable directory.

## Executable location

The release archive extracts wherever you choose; the installer writes only the `arana` executable to `--install-dir`. Neither edits `PATH`, registers a service, or installs optional tools. Terminal hosts can use the directory you added to `PATH`; GUI-launched hosts should be given the absolute executable path in their MCP configuration.

Arana does not self-update. `arana update` refreshes Arana-owned tools to their tested pins; replacing the CLI itself is a manual download or installer run.

## Secrets and settings

`config.yaml` (0600) holds the values below. Write them through the CLI so they are never echoed:

```sh
printf '%s' "$GROQ_KEY" | arana configure groq-key --stdin
printf '%s' "$OPENAI_KEY" | arana configure openai-key --stdin
printf '%s' "$GITHUB_TOKEN" | arana configure github-token --stdin
arana configure browser-port 9333
```

| Key | Purpose |
|---|---|
| `groq_api_key` | `youtube transcribe`, `xiaoyuzhou transcribe` (Groq Whisper) |
| `openai_api_key` | transcription fallback (OpenAI Whisper) |
| `github_token` | optional; raises GitHub REST limits |
| `browser_port` | CDP port for the dedicated browser (default 9222; accepted range 1024–65535) |

`configure` validates the value before creating any state, and re-reads the file under the write lock before merging, so two concurrent `configure` calls cannot lose each other's key. A key can be checked without printing it: `arana configure <key>` reports only whether it is set.

`configure proxy` is accepted for compatibility with the upstream configuration surface, but the current build does not apply that value to requests. Configure a proxy in the host or network layer instead.

## Environment variables

| Variable | Effect |
|---|---|
| `ARANA_HOME` | state root override |
| `ARANA_BROWSER_PORT` | CDP port override (wins over `config.yaml`) |
| `ARANA_CHROME` | path to Chrome/Chromium |
| `ARANA_OBSCURA_BIN` | explicit obscura binary |
| `ARANA_YTDLP_BIN` | explicit yt-dlp |
| `ARANA_BILI_BIN` | explicit bili-cli |
| `ARANA_BOSS_BIN` | explicit boss-agent-cli |
| `ARANA_OPENCLI_BIN` | explicit opencli |
| `ARANA_CLOAKBROWSER_BIN` | explicit cloakbrowser |
| `ARANA_NODE_BIN`, `ARANA_UV_BIN`, `ARANA_FFMPEG_BIN`, `ARANA_CHROME_BIN` | explicit prerequisites |

For tools using the registry resolver, an existing `*_BIN` override is checked before config, private copies, and `PATH`. Standalone Chrome discovery is separate: use `ARANA_CHROME` to override the browser launcher; the `browser route` availability check recognizes launcher and managed Chrome candidates without starting either or checking the bridge.

## MCP server configuration

`arana mcp` has no separate configuration file and reads no MCP-specific settings. The host launches the executable and speaks JSON-RPC over stdio. The server inherits the environment of the process that starts it, including `ARANA_HOME`; the public MCP path does not use the CLI's browser or tool-backend overrides.

- Public MCP needs no API key, account, browser, or profile. Do not add `env`, `headers`, or `userConfig` secrets to a host manifest or MCP entry.
- Use the default private `~/.arana` state root. A GUI host may have a different environment from your shell; set `ARANA_HOME` in the host's server entry only if you deliberately use an isolated root, and keep it private.
- Tool calls may write local operational state (health, cooldowns, pacing, locks) under that root. They do not read a personal browser profile or start a browser.

## state.json

Not meant to be hand-edited; the shape is stable:

```jsonc
{
  "channels": { "xiaohongshu": { "added_at": "…" } },
  "removed_defaults": ["reddit"],
  "tools": {
    "yt-dlp": { "owner": "arana", "scope": "private", "version": "2026.8.19",
                "path": "tools/yt-dlp", "files": […], "installed_at": "…" },
    "browser": { "owner": "system", "scope": "global", "version": "…" }
  },
  "owned_paths": {
    "config.yaml": { "owner": "arana", "scope": "private", "kind": "file", "sha256": "…" },
    "sessions/bili-cli": { "owner": "arana", "scope": "session", "kind": "directory" }
  },
  "checks":   { "github": { "at": 1730000000, "actions": {"read": true}, "previous": {…}, "evidence": {…} } },
  "backend_health": { "<fingerprint>": { "backend": "jina", "until": 1730000060 } },
  "cooldowns": { "<channel>": 1730000000 },
  "request_starts": { "<channel>:search": 1730000000 },
  "skill": { "links": [ { "path": "…/SKILL.md", "target": "…", "kind": "copy", "sha256": "…" } ], "unlinked": [] },
  "opencli_context": "…", "local_browser_source": "…"
}
```

A corrupt file degrades to defaults rather than crashing; a fingerprint change (version, build signature, channel config, tool set) invalidates old health and check evidence instead of letting it masquerade as current.

## Concurrency defaults

| Setting | Value |
|---|---|
| Browser slots | 2 per site, 4 globally |
| Slot wait | 120 s, then `BUSY` |
| Browser cold start | 90 s |
| Idle close | 600 s (suspended while a login window is pinned) |
| Default request timeout | 30 s (per-action overrides up to 90 s) |
| MCP tool deadline | 30 s shared by a `read_many` batch |
| Pacing | 10 s between `search`/`jobs` per channel |
| Health pauses | `TIMEOUT` 60 s, `UPSTREAM_BROKEN` 120 s, `NEED_SETUP` 60 s |
| Rate-limit cooldown | `retry_after_seconds` (default 3600 s) |
