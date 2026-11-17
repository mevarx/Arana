# Configuration

## State root

All state lives under one directory, `~/.arana/` by default.

| Variable | Effect |
|---|---|
| `ARANA_HOME` | Overrides the **state root** (not your home). Used for tests and isolated installs. |

The root is created privately (0700 on Unix; a protected current-user DACL on
Windows) and every read/write is symlink-guarded.

## Secrets and settings

`config.yaml` (0600) holds the values below. Write them through the CLI so they
are never echoed:

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
| `browser_port` | CDP port for the dedicated browser (default 9222) |

`configure` validates the value before creating any state, and re-reads the
file under the write lock before merging, so two concurrent `configure` calls
cannot lose each other's key.

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

Overrides win over private copies and the system `PATH`, which makes tests and
operator-provided builds explicit instead of implicit.

## state.json

Not meant to be hand-edited; the shape is stable:

```jsonc
{
  "channels": { "xiaohongshu": { "owner": "user", "added_at": "…" } },
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

A corrupt file degrades to defaults rather than crashing; a fingerprint change
(version, build signature, channel config, tool set) invalidates old health and
check evidence instead of letting it masquerade as current.

## Concurrency defaults

| Setting | Value |
|---|---|
| Browser slots | 2 per site, 4 globally |
| Slot wait | 120 s, then `BUSY` |
| Browser cold start | 90 s |
| Idle close | 600 s (suspended while a login window is pinned) |
| Default request timeout | 30 s (per-action overrides up to 90 s) |
| Pacing | 10 s between `search`/`jobs` per channel |
| Health pauses | `TIMEOUT` 60 s, `UPSTREAM_BROKEN` 120 s, `NEED_SETUP` 60 s |
| Rate-limit cooldown | `retry_after_seconds` (default 3600 s) |
