# Commands

`arana` prints exactly one JSON object per invocation (or a `--text` report for
the management verbs that offer one). Exit status is non-zero exactly when
`ok` is false.

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
| `setup` | Install missing tools for the default channels, materialise `SKILL.md`, and optionally import a legacy `~/.agent-reach` install (`--migrate`, non-destructive preview unless the import is confirmed). |
| `add` / `remove` | Move a channel between "not added" and "added" in the ledger. Removing a default channel is recorded so it stays removed. |
| `update` | Refresh arana-owned tools to their tested pins. `--check` only asks whether a newer arana release exists (and reports `unavailable` when no feed is configured rather than inventing one); `--sync-only` refreshes pins without the check. |
| `login` / `logout` | Open the channel's login page in the dedicated browser, or clear that channel's session cookies and recorded keys. The human types the credentials, never the CLI. |
| `doctor` | Read-only health report: installed tools and versions, whether each channel is added, login state (`logged_in` / `logged_out` / `unknown` / `not_required`), freshness of past checks, and the next action. Never claims readiness from stale evidence. |
| `check` | Runs the real probe actions and records pass/fail per action with the previous result, so regressions are visible. `--search` limits the probe set to search actions. |
| `uninstall` | Previews deletions by default. With confirmation it deletes only files recorded as arana-owned whose content hash still matches, and lists everything it leaves. |
| `skill` | Manage the embedded `SKILL.md`: `link` a skills directory, `unlink` it (only when the copy is unchanged), `update` refreshed copies, `status` shows links. |
| `configure` | Store secrets (piped to `--stdin`) and settings. Values are never echoed and never appear in logs or envelopes. |

## Browser

```
arana browser setup|status|stop|profiles|use|export|import|logout
```

`setup` starts the dedicated Chromium profile and binds the OpenCLI bridge;
`status` reports CDP and bridge state; `stop` closes only a browser arana
started; `profiles` lists connected bridge profiles and `use` selects an
explicit cookie source; `export`/`import` move cookies as JSON through CDP;
`logout` expires a site's cookies in the dedicated profile.

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
