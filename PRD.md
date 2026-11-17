# PRD — arana

| | |
|---|---|
| **Product** | arana — read-only internet access for AI agents (Rust) |
| **Version** | 1.0 (draft) |
| **Status** | For review |
| **Date** | 2026-11-17 |
| **Owner** | arana maintainers |
| **Derived from** | Panniantong/Agent-Reach v1.6.1 (`c70ae6d`, MIT), h4ckf0r0day/obscura v0.2.7 (`0d9a2f7`, Apache-2.0) |
| **Homepage** | `arana.mevarx.xyz` (docs, releases, install script) |
| **Companion doc** | `PORT_PLAN.md` (engineering audit, architecture, crate map) |

---

## 1. Executive summary

arana is a Rust reimplementation of Agent-Reach: a single CLI that gives
AI agents (Claude Code, Hermes, OpenClaw, any LLM agent with shell access)
search-and-read access to platforms their built-in tools cannot reach —
login-walled sites, video transcripts, and Chinese platforms — emitting one
line of JSON per invocation.

The product keeps the upstream CLI contract (envelope, error codes, channel
catalog) so the existing skill text and agent prompts work unchanged, and
replaces the ~9.4k-LOC Python orchestrator with a static Rust binary. It
adds **obscura** as a self-hosted, stealth, JS-rendering web backend, ending
the hard dependency on third-party readers (Jina) for most public pages.

Not ported, by decision: the external upstream CLIs (opencli, bili-cli,
boss-agent-cli, yt-dlp) and the Chromium browser bus that hosts the OpenCLI
extension. arana orchestrates them; it does not absorb them.

## 2. Problem statement

CLI agents ship with generic web search/read, but fail at:

1. **Login-walled content** — xiaohongshu notes, X timelines, LinkedIn job
   detail, BOSS直聘 JDs, Reddit threads, Bilibili subtitles.
2. **Audio/video content** — YouTube and Xiaoyuzhou transcripts.
3. **Chinese platforms** — Bilibili, V2EX, Xueqiu; poorly covered by
   generic search.
4. **Structured data** — clean JSON for stock quotes, repo issues, threads.

Agent-Reach solves this but requires a Python runtime, a venv bootstrap, and
three hosted dependencies (Jina/Exa as readers, uv as installer). arana
removes the interpreter for the orchestrator itself, removes the third-party
reader dependency for most public pages (obscura), and keeps everything else
— including the hard-won operational behavior (fallback chains, health
pauses, pacing) — contract-identical.

## 3. Goals and non-goals

### Goals

- G1. **Contract parity**: byte-identical `channels` output and
  schema-identical result envelopes vs Agent-Reach 1.6.1 for all 40 actions.
- G2. **Static binary**: `arana` is one native executable per platform;
  no interpreter required for the CLI itself.
- G3. **Self-hosted public reads**: obscura as the first `web read`
  backend; Jina/Exa as fallbacks, not primaries.
- G4. **Operational parity**: fallback ordering, error taxonomy, backend
  health pauses, channel cooldowns, pacing, browser slots, session reuse —
  all preserved with the same constants.
- G5. **Safe by construction**: read-only, never asks for passwords, never
  reads the user's browser cookie database, private file permissions,
  SSRF-guarded fetches, credential-scrubbed error output.

### Non-goals

- NG1. No write actions — no posting, commenting, liking, DMs. Ever.
- NG2. No reimplementation of upstream site adapters (opencli, bili-cli,
  boss-agent-cli, yt-dlp). They remain external, version-pinned tools.
- NG3. No replacement of the Chromium browser bus. The OpenCLI Chrome
  extension requires a real Chromium; obscura cannot host it.
- NG4. No MCP server (upstream retired theirs; arana stays CLI-first).
- NG5. No guarantee of unlogged public access to login-walled platforms —
  we surface `NEED_LOGIN`, we never fake or bypass logins.

## 4. Users and personas

| ID | Persona | Description | Primary interface |
|---|---|---|---|
| P1 | **The Agent** | Claude Code, Hermes, OpenClaw, or any LLM agent with shell access. Branches on `ok` and `error.code`, follows `error.hint`. | stdout JSON; `SKILL.md` |
| P2 | **The Operator** | The human who owns the machine. Installs, adds/removes channels, logs in with their own hands, monitors health. | natural language to their agent; CLI mgmt verbs |
| P3 | **The Contributor** | Extends arana with a new channel or backend, runs the acceptance harness. | Rust crates, golden tests |
| P4 | **The Skill Host** | Agent applications whose skills directory receives `SKILL.md` (link or copy). | `skill link` |

## 5. User stories and acceptance criteria

| ID | Story (as P1/P2) | Acceptance criteria |
|---|---|---|
| US-1 | As an **agent**, I search a platform so my answer cites real data. | `arana <channel> search "<q>"` exits 0 with `ok:true` and ≥1 item, or exits non-zero with a single envelope carrying a code from the Appendix A set. |
| US-2 | As an **agent**, I read any URL without knowing which platform it is. | `arana read <url>` routes by `can_handle` precedence; output labels the chosen `backend`. |
| US-3 | As an **agent**, I know what to do next when a platform breaks. | Every failure envelope carries `error.code`, `error.message` (≤500 chars, credential-free) and `error.hint` from the canned set. |
| US-4 | As an **operator**, I add a channel with one sentence and no admin rights. | `arana add xiaohongshu` installs its tools privately under `~/.arana/tools/`, records ownership in `state.json`, and prints next steps. |
| US-5 | As an **operator**, I log in myself, never handing over a password. | `arana login <channel>` opens a window in the dedicated browser or the site's own flow; the CLI never prompts for credentials. |
| US-6 | As an **operator**, removal deletes only what arana installed. | `remove`/`uninstall` delete only paths recorded with `owner: arana`; unrecorded files are listed and left untouched. |
| US-7 | As an **agent**, my parallel sub-agents can query simultaneously. | 2 concurrent calls per site and 4 overall succeed; excess callers queue ≤120 s then receive `BUSY`, never corrupt state. |
| US-8 | As an **operator**, my machine stays clean. | All state under one `~/.arana/`; nothing on `PATH` except the one binary; private dirs 0700, files 0600 (platform-equivalent). |
| US-9 | As an **agent**, I read JS-rendered public pages without third-party readers. | `web read` on a JS-only page returns content via `backend: obscura` with no Jina call in `meta.attempts`. |
| US-10 | As a **skill host operator**, my agent learns arana on its own. | `skill link <dir>` places `SKILL.md`; `update` refreshes links; state records them for clean uninstall. |

## 6. Functional requirements

Priorities: **P0** = release-blocking, **P1** = first GA, **P2** = post-GA.

### 6.1 Command surface (FR-CMD)

| ID | Requirement | Pri |
|---|---|---|
| FR-CMD-1 | `<channel> <action> [params...]` executes a unified action; unknown channel/action → `BAD_INPUT` envelope listing valid names. | P0 |
| FR-CMD-2 | `read <url>` auto-routes to the first channel (in registry order) whose `can_handle` matches and that exposes `read`. | P0 |
| FR-CMD-3 | `channels` lists every channel: name, level (default/optional/experimental), description, actions with params, usage strings, login state. Output must match the Python build byte-for-byte at ship time, modulo the program name inside usage strings (`arana` vs `agent-reach`). | P0 |
| FR-CMD-4 | Management verbs: `add`, `remove`, `setup`, `login`, `logout`, `configure`, `doctor`, `check`, `update`, `uninstall`, `skill link|unlink`, `browser setup|status|stop|profiles|use`, `version`. | P0 (subset P1: see §9 phasing) |
| FR-CMD-5 | `--timeout <sec>` (default 30, per-action overrides for slow actions), `--backend <label>` to pin one backend, `--backend public-web` to force the public route. | P0 |
| FR-CMD-6 | `--verbose` logs to stderr only; stdout is always exactly one JSON object (or the management output). Never interleaved. | P0 |
| FR-CMD-7 | Args after `--` are literal values, including strings that look like flags. | P1 |

### 6.2 Output contract (FR-OUT)

| ID | Requirement | Pri |
|---|---|---|
| FR-OUT-1 | Every unified invocation prints one envelope: `{schema_version: 1, ok, channel, action, backend, items, meta, error}`. | P0 |
| FR-OUT-2 | `items[]` fields: `title`, `url`, `author`, `published_at`, `text`, `raw` (missing → null; upstream row kept in `raw`). | P0 |
| FR-OUT-3 | `meta.attempts[]` records every backend tried or skipped, with `result` code and, for paused backends, `retry_after_seconds`. | P0 |
| FR-OUT-4 | Error envelopes: `error = {code, message, hint}`; message ≤500 chars; URLs in messages are credential-scrubbed; hints come from the canned map (Appendix A). | P0 |
| FR-OUT-5 | Exit code non-zero iff `ok: false`. | P0 |
| FR-OUT-6 | `meta` includes `elapsed_ms` and, when applicable, the update notice from an explicit `update --check`. | P1 |

### 6.3 Channel catalog (FR-CH)

The 14 channels and 40 actions of upstream 1.6.1, with levels and auth
requirements:

| Channel | Level | Actions | Needs |
|---|---|---|---|
| web | default | search, read | none |
| v2ex | default | hot, nodes, read, user | none |
| github | default | search, read | none |
| bilibili | default | search, read, hot, subtitle | none; subtitle needs login (QR) |
| youtube | default | search, read, transcript, transcribe | none; transcribe needs Groq/OpenAI key |
| xueqiu | optional | quote, search, hot, stocks | quote/search none; hot/stocks login |
| xiaohongshu | optional | search, read, comments | login |
| boss | optional | search, read | login |
| xiaoyuzhou | optional | transcribe | Groq key |
| linkedin | optional | search, read | public jobs none; site search login |
| reddit | optional | search, read, comments, subreddit | login |
| twitter | experimental | search, profile, tweets, read | login |
| facebook | experimental | search, profile | login |
| instagram | experimental | search, profile, posts | login |

| ID | Requirement | Pri |
|---|---|---|
| FR-CH-1 | All channels and actions above exist with identical names, params, defaults, and backend ordering as upstream 1.6.1. | P0 |
| FR-CH-2 | Per-action backend lists are data, not code: adding a backend never changes the CLI. | P1 |
| FR-CH-3 | Each action declares `public_web` URL scopes used for the public-first/last-resort policy (§6.4). | P0 |

### 6.4 Dispatch and resilience (FR-DIS)

The operational core — all constants must match upstream.

| ID | Requirement | Pri |
|---|---|---|
| FR-DIS-1 | Try `action.backends` in order; on a retryable error fall through to the next; on a `FINAL_CODES` error stop. | P0 |
| FR-DIS-2 | Backend health: after a failure, pause that channel/action/backend — TIMEOUT 60 s, UPSTREAM_BROKEN 120 s, NEED_SETUP 60 s — keyed by channel fingerprint; paused backends are skipped and shown in `attempts`; success clears the entry. | P0 |
| FR-DIS-3 | Channel cooldown: a confirmed RATE_LIMITED pauses the whole channel 3600 s, checked before any backend, reported with `retry_after_seconds`. | P0 |
| FR-DIS-4 | Pacing: ≥10 s between two `search`/`jobs` invocations on the same channel, enforced under a cross-process lock so parallel calls cannot both skip it. | P0 |
| FR-DIS-5 | Public-web policy: not-added channels try the public web first (budget `min(20 s, remaining/2)`, backend labeled `public-web`); added channels fall back to it only after all site backends failed with transient errors (UPSTREAM_BROKEN/TIMEOUT), never after FINAL_CODES. | P0 |
| FR-DIS-6 | Deadline: one global clock per invocation; setup/login interactions pause it ("off the clock"); in-process HTTP is cancellable at the deadline. | P0 |
| FR-DIS-7 | When several backends fail, report the most useful error: a real failure beats `NEED_SETUP`. | P0 |
| FR-DIS-8 | Param validation before any tool install or login prompt: required/missing, type coercion, positive `limit`/`page`, unknown-argument rejection — all as `BAD_INPUT` with usage. | P0 |

### 6.5 Browser bus (FR-BRW)

| ID | Requirement | Pri |
|---|---|---|
| FR-BRW-1 | One shared Reach-owned Chromium profile under `~/.arana/browser/profile`, headless by default, CDP on port 9222 unless configured. | P0 |
| FR-BRW-2 | Lifecycle: cold-start wait ≤90 s; idle close after 600 s; a login window pins the browser busy; auto-reopen on next use. | P0 |
| FR-BRW-3 | Concurrency: 2 slots per site, 4 total, via OS file locks (`browser-locks/`); queue wait 120 s then `BUSY`. No stale-lock cleanup; a crashed holder is tolerated like a full browser. | P0 |
| FR-BRW-4 | State writes take an exclusive execution lock; contention yields `BUSY`, never a blocked or corrupted write. | P0 |
| FR-BRW-5 | Cookie export/import and logout via CDP (`Storage.getCookies`/`setCookies` with expiry-based deletion); no Chrome cookie-DB decryption, ever. | P0 |
| FR-BRW-6 | Chromium source: user's installed Chrome, or CloakBrowser build if present; downloading a stealth Chromium is P2. | P1 |
| FR-BRW-7 | `obscura serve` may run alongside as a second CDP engine for public reads, on a port distinct from the bus. | P2 |

### 6.6 Session and login (FR-SES)

| ID | Requirement | Pri |
|---|---|---|
| FR-SES-1 | Upstream CLI logins live in fake `HOME`s under `~/.arana/sessions/<tool>/`; login state detection is tool-aware (e.g. bili-cli requires a non-empty `sessdata`). | P0 |
| FR-SES-2 | Site-scoped reuse: copy one site's cookies from a *connected* local browser (via the OpenCLI bridge) into the dedicated profile, with stale-cookie expiry and rollback on failure; ambiguity across multiple source browsers forces an explicit choice; nothing is copied silently from the user's browser cookie store. | P1 |
| FR-SES-3 | `login <channel>` opens the site's login page in the dedicated browser — the human logs in; the CLI never types credentials and never asks for them. | P0 |
| FR-SES-4 | `logout` clears the channel's cookies in the dedicated browser and its `logout_keys` in config. | P1 |
| FR-SES-5 | Login-need reporting: a failed authenticated call yields `NEED_LOGIN` with the "user must log in themselves" hint; unreadable cookie state is unknown, not logged-out. | P0 |

### 6.7 Tool management (FR-TLS)

| ID | Requirement | Pri |
|---|---|---|
| FR-TLS-1 | Registry of upstream tools with `tested` version (installed at that version), `floor` (oldest accepted), `exclude` (known-bad). | P0 |
| FR-TLS-2 | Private installs only: per-tool venvs / npm prefix under `~/.arana/tools/`; never on the user's PATH; a user's own copy within limits is used as-is, never downgraded. | P0 |
| FR-TLS-3 | Machine prerequisites (node, uv, ffmpeg, chrome): use the system copy when present, else install a private copy without admin rights. | P1 |
| FR-TLS-4 | `add` is resumable and idempotent: a partially failed setup can be re-run without damage. | P1 |
| FR-TLS-5 | Ownership ledger: `state.json` records `owner`/`scope` per installed path; remove/uninstall touch only owned paths. | P0 |
| FR-TLS-6 | Windows: child processes created hidden; timeouts kill the whole process tree (`taskkill /T /F` or equivalent). | P0 |

### 6.8 Obscura integration (FR-OBS)

| ID | Requirement | Pri |
|---|---|---|
| FR-OBS-1 | obscura ships as a pinned external tool (release binary under `~/.arana/tools/obscura/`), not a vendored crate; registry entry kind `binary`, with `tested` + `floor`. | P0 |
| FR-OBS-2 | `web read` backend order: **obscura → jina → exa**. obscura parses `--dump text/html` output into the unified item shape; failures fall through per FR-DIS-1. | P0 |
| FR-OBS-3 | Public-web route uses obscura first with the same scope rules and readable-content checks (reject login walls/empty shells). | P0 |
| FR-OBS-4 | `--stealth` variant used where available (Windows: `-stealth` archive); its absence degrades to non-stealth, never to failure. | P1 |
| FR-OBS-5 | obscura's SSRF defaults preserved: private/internal IPs blocked; `--allow-private-network` never set from user-supplied URLs. | P0 |
| FR-OBS-6 | `web search` remains Exa-based; obscura does not provide search. | P0 |
| FR-OBS-7 | `remove`/`uninstall` delete the obscura tool and its state with the rest of the ownership ledger. | P0 |

### 6.9 Skill and management (FR-MGT)

| ID | Requirement | Pri |
|---|---|---|
| FR-MGT-1 | `SKILL.md` ships with the binary and is linked/copied into agent skills directories on setup; links recorded in state; update refreshes; uninstall removes only our entries. | P1 |
| FR-MGT-2 | `doctor` reports per-channel health: tools installed, versions within floor, login state, last check, what is broken and the next action. | P1 |
| FR-MGT-3 | `check [channels...]` runs live probe actions and records pass/fail per action with previous values; `--text` renders human-readable. | P1 |
| FR-MGT-4 | `configure <key> [value|--stdin]` manages secrets (groq-key, openai-key, browser-port); values never echoed to chat or logs. | P0 |
| FR-MGT-5 | `update` refreshes the binary's tools to their tested versions; release notes fetched only on explicit `--check`. | P1 |
| FR-MGT-6 | `uninstall` lists everything it will delete, requires confirmation, deletes only owned paths. | P0 |
| FR-MGT-7 | Migration: `setup` detects an existing Python Agent-Reach install (`~/.agent-reach/`) and offers a one-time, non-destructive import — ownership ledger, config keys, session dirs, browser profile — into `~/.arana/`; the original is left untouched so both builds can coexist during differential testing. | P1 |

### 6.10 Transcription (FR-TRX)

| ID | Requirement | Pri |
|---|---|---|
| FR-TRX-1 | YouTube/xiaoyuzhou transcribe: yt-dlp download → ffmpeg compress/chunk (25 MB-equivalent budgets) → Groq (`whisper-large-v3`) or OpenAI (`whisper-1`). | P0 |
| FR-TRX-2 | SSRF guards on user URLs: block private IPs incl. non-literal forms (`127.1`, decimal/octal), DNS-checked; file size and duration budgets enforced. | P0 |
| FR-TRX-3 | Subtitle retrieval for Bilibili requires login and reports `NEED_LOGIN` with the QR-login hint; "subtitles unavailable" without login is never conflated with "video has none". | P0 |

## 7. Non-functional requirements

| ID | Requirement | Target |
|---|---|---|
| NFR-1 Performance | `arana channels` cold start (no network) | ≤ 50 ms p95 |
| NFR-2 Performance | `web read` via obscura, cached engine, static page | ≤ 1.5 s p50 |
| NFR-3 Footprint | Binary size | ≤ 30 MB (release, stripped) |
| NFR-4 Portability | Platforms | Windows x64, macOS arm64/x64, Linux x64/arm64 (glibc 2.35+) |
| NFR-5 Security | State privacy | `~/.arana/` tree 0700/0600-equivalent; state writes atomic; symlink-guarded reads |
| NFR-6 Security | Secrets | API keys stored in `config.yaml` (0600), never in logs, envelopes, or chat |
| NFR-7 Reliability | State corruption | A corrupt `state.json` degrades to defaults; never crashes the CLI |
| NFR-8 Reliability | Concurrency | Parallel invocations never corrupt state or double-pace (verified by stress test) |
| NFR-9 Observability | `--verbose` stderr diagnostics per backend attempt; `doctor --text` summarizes | — |
| NFR-10 Compatibility | Envelope schema | `schema_version` stays 1 through GA; breaking change requires 2 and a migration note |
| NFR-11 Build | CI builds all four targets; no C++ toolchain needed for `arana` itself (obscura consumed as binary) | — |
| NFR-12 i18n | UTF-8 stdout on Windows without console codepage setup | — |

## 8. Architecture (summary)

Five crates in one workspace — `reach-core` (envelope, errors, registry,
dispatch, clock, state), `reach-channels` (per-channel backends + generic
opencli wrapper), `reach-browser` (CDP client, slots, login, session reuse),
`reach-tools` (tool registry, private installs), `arana` (bin/CLI).
External binaries: Chromium+opencli, bili-cli, boss-agent-cli, yt-dlp,
ffmpeg, obscura. Full audit, crate map, and per-module sizing: see
`PORT_PLAN.md`.

## 9. Release plan

| Milestone | Scope (from `PORT_PLAN.md`) | Exit criteria | Maps to |
|---|---|---|---|
| M0 | Contract freeze | Golden JSON for `channels` + one invocation per action recorded from the Python build; schema.json written | FR-OUT-*, FR-CH-1 |
| M1 | Core + no-auth channels | `channels` byte-identical modulo program name; web/v2ex/github/xueqiu/bilibili(read,hot) envelopes match goldens | FR-CMD-1..6, FR-OUT-*, FR-DIS-*, FR-CH |
| M1.5 | obscura web backend | JS-rendered page reads via `backend: obscura`; Jina/Exa still pass as fallbacks; tool installs+removes cleanly | FR-OBS-1..3,5 |
| M2 | YouTube + transcription | Fixed-video transcript matches Python output; SSRF tests green | FR-TRX-*, FR-TLS-6 |
| M3 | Browser bus | Parallel-read test, login-window survival, `BUSY` after 120 s slot wait | FR-BRW-*, FR-DIS-4 |
| M4 | opencli channels | 7 site channels reproduce Python output live | FR-CH-1, FR-SES-1 |
| M5 | Management | add/remove/login/logout/configure/doctor/check/update/uninstall/skill; ledger-gated deletion verified | FR-MGT-*, FR-TLS-*, FR-SES-* |
| M6 | Acceptance | Harness distinguishes waived vs failed; ≥34/40 live actions pass with the upstream 6 waived | §10 |

Phasing for GA: **M0–M3** = public beta (default channels + browser reads);
**M4–M6** = GA (full catalog). Session-reuse sourcing (FR-SES-2) and
browser-profile stealth download (FR-BRW-6) may trail GA as P1/P2.

## 10. Acceptance and QA strategy

1. **Golden-envelope tests** (M0 recordings): every action's output shape is
   diffed against the Python build with frozen inputs; field-level diffs fail
   CI.
2. **Unit tests** ported 1:1 from upstream's 321 (validation, dispatch,
   pacing, locks, URL security, credential scrubbing) — same behaviors, Rust
   tests.
3. **Differential harness**: run Python and Rust builds side by side against
   the same live inputs during M1–M4; `meta.attempts` trails must match.
4. **Live acceptance** (M6): the upstream gate model — 40 probe actions,
   pass/waived/fail with recorded reasons; waived requires a written
   justification (missing login, missing key, platform block) as upstream
   1.6.1 did (34/40, 6 waived).
5. **Concurrency stress**: N parallel invocations across 2 sites assert slot
   limits, no state corruption, pacing honored exactly once.
6. **Security tests**: SSRF corpus (private IPs, literal-IP tricks),
   symlinked state paths, oversized responses, credential-bearing URLs in
   errors.

## 11. Success metrics

| Metric | GA target |
|---|---|
| Contract stability | 100% golden envelope tests pass; zero `schema_version` bumps |
| Live parity | ≥34/40 actions passing; identical waiver set as upstream 1.6.1 |
| Self-hosted reads | ≥80% of successful `web read` calls served by `backend: obscura` |
| Install friction | `channels` works < 60 s after first run on a clean machine (excl. optional tools) |
| Runtime footprint | No Python interpreter required for web/v2ex/github/xueqiu/bilibili read+hot |
| Crash rate | Zero panics across the live acceptance suite (panics abort the run) |

## 12. Risks and mitigations

| # | Risk | Sev | Mitigation |
|---|---|---|---|
| R1 | opencli output-shape changes break 7 channels | High | Pin tested version + floor; defensive YAML/JSON parsing; `raw` passthrough; health-pause machinery absorbs transient breakage |
| R2 | obscura stealth insufficient for login-walled scraping | Med | Scope obscura to public reads (FR-OBS-2..3); Chromium+CloakBrowser path remains for logged-in channels |
| R3 | Obscura 0.2.x API/binary churn | Med | Pinned release binary; adapter isolates CLI flags; fall-through to Jina/Exa |
| R4 | Port-collision (9222) between bus and obscura | Med | Distinct configurable ports; probe-before-bind |
| R5 | Login glue is upstream-shaped (bili-cli credential writer) | Med | Write known-format `credential.json` directly, verified against bili-cli; shim retained as fallback |
| R6 | yt-dlp weekly drift | High | Never reimplement; pin + `update` as tool-version op |
| R7 | Windows process/permission semantics diverge | Med | CREATE_NO_WINDOW, tree-kill via `taskkill /T /F`, UTF-8 stdout, ACL-equivalent privacy tests in CI |
| R8 | Live verification cost (accounts, cookies) | Med | Waiver model copied from upstream; acceptance harness separates waived from failed |
| R9 | Behavioral drift vs Python (pacing TTLs, budget splits) | High | Constants centralized in one module; differential harness asserts `meta.attempts` equality |

## 13. Out of scope / future work

- Writing/posting/moderating anywhere (permanent non-goal, NG1).
- obscura as the browser-bus host or as an opencli replacement (blocked by
  extension support; revisit only if obscura ships Chrome-extension
  compatibility).
- Native Bilibili search/subtitle backends (WBI signing) — the public-API
  fallback covers `read`/`hot` only.
- MCP server surface; multi-user/remote deployments; residential-proxy
  management.

## 14. Open questions

| # | Question | Recommendation |
|---|---|---|
| Q1 | Binary name: keep `agent-reach` (drop-in) or `arana` (own identity)? | **Decided 2026-11-17:** ship `arana` (crates.io free; "openreach" was dropped — BT trademark); provide an `agent-reach` alias for drop-in installs; state dir `~/.arana/` with one-time import from `~/.agent-reach/` (FR-MGT-7); homepage `arana.mevarx.xyz`. |
| Q2 | Ship obscura in the installer bundle or fetch on first use? | Fetch on first `add`/first `web read` (keeps the arana download lean, matches tool-registry pattern). |
| Q3 | Keep the CloakBrowser stealth-Chromium download at all, or Chromium-only until proven insufficient? | Chromium-only for GA (P2 for cloak), per FR-BRW-6. |
| Q4 | Upstream sync policy: track Agent-Reach releases and port their channel fixes? | Yes, manually per release, gated by the differential harness. |
| Q5 | Do we publish `SKILL.md` changes upstream-portable (bilingual) or arana-specific? | Keep upstream-compatible text; only the command name changes with Q1. |

## 15. Appendices

### A. Error codes (contract)

| Code | Meaning | Final? | Canned hint (abridged) |
|---|---|---|---|
| NEED_SETUP | dependency/key missing | no | run `doctor` / `add <channel>` |
| NEED_LOGIN | login required or expired | yes | user logs in themselves; never ask for passwords |
| UPSTREAM_BROKEN | backend failed | no | name the platform, not the tool; no retry loops |
| TIMEOUT | deadline exceeded | no | inspect status; don't re-open login windows |
| RATE_LIMITED | platform rate limit | yes | respect `retry_after_seconds`; channel paused |
| PLATFORM_BLOCKED | captcha / risk control | yes | stop; user resolves on the official page |
| NOT_FOUND | content absent | yes | check link/id |
| BAD_INPUT | invalid arguments | yes | run `channels` |
| BUSY | browser full or state locked | yes | retry later; don't switch profiles |

### B. State layout

```
~/.arana/
  state.json        # channels, tools (owner/scope/version), checks,
                    # backend_health, cooldowns, request_starts,
                    # session_reuse, skill.links
  config.yaml       # groq/openai keys, browser_port (0600)
  sessions/<tool>/  # per-tool fake HOMEs
  browser/profile/  # shared dedicated Chromium profile
  browser-locks/    # msvcrt/fcntl lock files (slots, execution, pace)
  tools/            # private venvs, npm prefix, node, uv, ffmpeg, obscura
```

### C. Glossary

- **Backend** — one executable/HTTP route behind an action (e.g. jina, exa,
  obscura, bili-cli, opencli, public-web).
- **Channel** — one platform's facade: actions, scopes, login policy.
- **Bus** — the shared Chromium instance hosting opencli and logins.
- **Envelope** — the single JSON object every invocation prints.
- **Golden test** — output recorded from the Python build, diffed in CI.
