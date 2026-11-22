# Commands

`arana` prints exactly one JSON object per invocation (or a `--text` report for the management verbs that offer one), except `arana mcp`, which uses stdout for JSON-RPC frames and stderr for diagnostics. Exit status is non-zero exactly when `ok` is false.

## Unified actions

```
arana <channel> <action> [args] [--timeout SECONDS] [--backend LABEL] [--verbose]
arana read <url>          # auto-route to the first channel that handles the URL
arana channels            # the frozen catalog, byte-identical to the Python build
```

| Channel | Actions | Needs |
|---|---|---|
| `web` | `search`, `read` | none |
| `v2ex` | `hot`, `node`, `read`, `user` | none |
| `github` | `search`, `read` | none (token optional, raises limits) |
| `bilibili` | `search`, `read`, `hot`, `subtitle` | `subtitle` needs login |
| `youtube` | `search`, `read`, `subtitle`, `transcribe` | `transcribe` needs a key |
| `xueqiu` | `quote`, `search`, `hot_posts`, `hot_stocks` | `hot_*` need login |
| `xiaohongshu` | `search`, `read`, `comments` | login + OpenCLI bridge |
| `boss` | `search`, `read` | login or CDP; `read` reuses a recent search |
| `linkedin` | `jobs`, `read` | public jobs page for `read`; site search needs login |
| `reddit` | `search`, `read`, `subreddit` | login + bridge (public-web fallback otherwise) |
| `twitter` | `search`, `profile`, `tweets`, `thread` | login + bridge |
| `facebook` | `search`, `profile` | login + bridge |
| `instagram` | `search`, `profile`, `posts` | login + bridge |
| `xiaoyuzhou` | `transcribe` | Groq/OpenAI key |

These 40 action names are the pinned upstream contract: examples are
`v2ex node` (not `nodes`), `youtube subtitle` (not `transcript`), `xueqiu
hot_posts` / `hot_stocks`, `linkedin jobs`, `twitter thread`. See
`CONTRACT_NOTES.md` for the naming and scope decisions.

`--backend` pins one backend by its label (for example `github-api`, `jina`,
`exa`, `obscura`, `bili-cli`, `opencli`, `tencent`, `public-web`); an unknown
label is `BAD_INPUT` and lists what exists. `--` makes every following argument
literal, including strings that look like flags.

## Management

```
arana version
arana setup [--text] [--migrate [--legacy DIR]]
arana add <channel>            arana remove <channel>
arana update [--check|--sync-only]
arana login <channel>          arana logout <channel>
arana doctor [--text]          arana check [channels...] [--text] [--search] [--timeout S]
arana uninstall [--dry-run|--yes]
arana skill [status|link <dir>|unlink <dir>|update]
arana configure <key> [value|--stdin]
```

| Verb | Meaning |
|---|---|
| `version` | Report the CLI version, upstream revision, and source signature. Use this exact output in bug reports. |
| `setup` | Install missing tools for the default channels, materialise `SKILL.md`, and optionally import a legacy `~/.agent-reach` install (`--migrate`, non-destructive preview unless the import is confirmed). |
| `add` / `remove` | `add` records the channel and installs its optional tools; `remove` moves it back to "not added". Removing a default channel is recorded so it stays removed. |
| `update` | Refresh Arana-owned tools to their tested pins. `--check` only asks whether a newer Arana release exists (and reports `unavailable` when no feed is configured rather than inventing one); `--sync-only` refreshes pins without the check. |
| `login` / `logout` | Open the channel's login page in the dedicated browser, or clear that channel's session cookies and recorded keys. The human types the credentials, never the CLI. |
| `doctor` | Read-only health report: installed tools and versions, whether each channel is added, login state (`logged_in` / `logged_out` / `unknown` / `not_required`), freshness of past checks, and the next action. Never claims readiness from stale evidence. |
| `check` | Runs the real probe actions and records pass/fail per action with the previous result, so regressions are visible. `--search` limits the probe set to search actions. |
| `uninstall` | Previews deletions by default. With confirmation it deletes only files recorded as Arana-owned whose content hash still matches, and lists everything it leaves. |
| `skill` | Manage the embedded `SKILL.md`: `link` a skills directory, `unlink` it (only when the copy is unchanged), `update` refreshed copies, `status` shows links. |
| `configure` | Store secrets (piped to `--stdin`) and settings. Values are never echoed and never appear in logs or envelopes. |

## Browser

```
arana browser setup|status|stop|profiles|use|export|import|logout|route
```

`setup` starts the dedicated Chromium profile and binds the OpenCLI bridge; `status` reports CDP and bridge state; `stop` closes only a browser Arana started; `profiles` lists connected bridge profiles and `use` selects an explicit cookie source; `export`/`import` move cookies as JSON through CDP; `logout` expires a site's cookies in the dedicated profile.

`route` is a decision-only helper for agents. It takes the host's observed browser status and, only with explicit authorization, considers the standalone Chrome/OpenCLI fallback:

```sh
arana browser route <available|unavailable|unknown|denied> [--allow-chrome <channel> <action>]
```

| Input | `route` | Meaning |
|---|---|---|
| `available` | `host_browser` | Use the host's browser tool directly. |
| `denied` | `blocked` | Do not switch tools to bypass a policy denial. |
| `unknown` | `unknown` | Inspect the current host's tool inventory; do not guess. |
| `unavailable` without `--allow-chrome` | `unavailable` | No host browser and no fallback authorized. |
| `unavailable --allow-chrome <channel> <action>` where the action has no OpenCLI backend | `unsupported` | A generic web URL is not a standalone OpenCLI action. |
| `unavailable --allow-chrome <channel> <action>` with Chrome or OpenCLI missing | `needs_setup` | Reports `chrome_found` and `opencli_found`; it does not install anything. |
| `unavailable --allow-chrome <channel> <action>` with both present | `chrome_opencli` | Execute the existing CLI action with `--backend opencli`. `bridge_verified` is always `false`. |

The command never launches Chrome, creates state or browser files, installs tools, or verifies the bridge or login. Any other status is `BAD_INPUT`.

## MCP server

```
arana mcp
```

`mcp` starts a local, read-only stdio server. It speaks newline-delimited JSON-RPC 2.0, negotiates protocol `2025-06-18` (accepting `2024-11-05` and `2025-03-26`), keeps protocol output on stdout and diagnostics on stderr, and exits cleanly on EOF.

| Tool | Maps to | Notes |
|---|---|---|
| `list_capabilities` | local data | Server description, tool specs, public allowlist, excluded surfaces. |
| `read_url` | `web read` | One public HTTP(S) URL. |
| `read_many` | `web read` | 1–5 public URLs, validated as a batch, read in input order, one shared deadline. |
| `search` | `web search` | Public query; `limit` 1–10. |

Public MCP uses only the HTTP backends `jina` and `exa`; it never uses obscura, a browser, a profile, a login, setup, backend overrides, or arbitrary commands. The full host setup and bounds are in [Agent integrations](agent-plugins.md#bounds).

## Output contract

| Field | Meaning |
|---|---|
| `schema_version` | Always `1`. A breaking change would be `2` plus a migration note. |
| `ok` | Whether the invocation succeeded. |
| `backend` | The backend that produced `items`, or null when every backend failed. |
| `items[]` | `title`, `url`, `author`, `published_at`, `text`, `raw`. Missing values are `null`; the upstream row is kept in `raw`. |
| `meta.attempts[]` | Every backend tried or skipped, with its result code, and `retry_after_seconds` when a backend is paused. |
| `meta.elapsed_ms` | Wall clock for the invocation. |
| `error` | `{code, message, hint}` or null. Messages are bounded at 500 chars and credential-scrubbed. |

## Error codes

| Code | Meaning | Final |
|---|---|---|
| `NEED_SETUP` | A tool or key is missing. | no |
| `NEED_LOGIN` | Login required or expired; the user must log in. | yes |
| `UPSTREAM_BROKEN` | The backend failed. | no |
| `TIMEOUT` | The deadline was exceeded. | no |
| `RATE_LIMITED` | The platform rate-limited; the channel is paused for `retry_after_seconds`. | yes |
| `PLATFORM_BLOCKED` | Captcha or risk control; the user must resolve it on the site. | yes |
| `NOT_FOUND` | The content does not exist. | yes |
| `BAD_INPUT` | Invalid arguments; run `channels` for valid names. | yes |
| `BUSY` | Browser slots or state locks are held; retry later. | yes |

"Final" means fallback stops and the error is reported as-is. Non-final errors
fall through to the next backend in order.
