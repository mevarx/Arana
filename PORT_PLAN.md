# Agent-Reach → Rust: port plan

Source: Panniantong/Agent-Reach v1.6.1, HEAD `c70ae6d` (2026-11-08), MIT.
Target: `D:\CODING\arana`. Based on a full read of the tree, second pass.
Product requirements live in `PRD.md`; this document is the engineering
audit and architecture behind it.

## Verdict

Feasible, and the architecture ports cleanly. What does not port is the
ecosystem underneath: the hard upstreams are external CLIs in other languages
(Node/Python) and the browser layer is a Python package that manages a patched
Chromium. Rust replaces the orchestrator, not those.

This is not "rewrite a Python skill in Rust"; it is "reimplement a
9.4k-LOC process/HTTP orchestrator in Rust and keep talking to the same
external tools". That is a reasonable goal — the Python is almost all
subprocess spawning, HTTP, JSON shaping, filesystem state and fallback policy,
with essentially no algorithmic content.

## What the codebase actually is (verified numbers)

- **9,357 LOC Python, 49 modules**, 14 channels, 40 declared actions,
  321 test functions across 35 files.
- Runtime deps: `requests`, `loguru`, `pyyaml`, `yt-dlp[default]` (pinned set in
  `constraints.txt`: requests 2.32.5, loguru 0.7.3, PyYAML 6.0.3,
  yt-dlp 2026.8.19 + yt-dlp-ejs 0.8.0).
- No MCP server in 1.6.1 (retired; `manage.py` only emits a removal notice).
  "The skill" is `SKILL.md` + this CLI; the Markdown ships alongside the binary.
- Everything lives under `~/.agent-reach/`: `state.json`, `config.yaml`,
  `sessions/<tool>/` (per-tool fake `HOME`), `browser/profile/`, `browser-locks/`,
  `tools/`.
- CLI surface: `<channel> <action>`, `read <url>` (auto-routes via
  `can_handle` over channels exposing `read`), `channels`, `check`, `login`,
  `logout`, `configure`, `doctor`, `add`, `remove`, `setup`, `update`,
  `uninstall`, `skill link|unlink`, `browser setup|status|stop|profiles|use`.
- Release gate: `test.sh` (clean venv + isolated HOME) + `scripts/
  {accept-search,check_release_acceptance,record_release_acceptance}.py`;
  v1.6.1 shipped 34/40 actions live-verified, 6 waived (missing logins/keys).

## Design invariants to preserve exactly

1. **One JSON envelope per invocation** — `{schema_version, ok, channel,
   action, backend, items, meta, error}`; `items[]` normalized to
   `title/url/author/published_at/text/raw`; `meta.attempts` keeps the backend
   trail including skipped/paused backends; messages capped at 500 chars and
   scrubbed of URL credentials. Agents branch on `ok` + `error.code`.
2. **Ordered backend fallback per action** — `backends: [(label, method)]`;
   a `FINAL_CODES` error (`NEED_LOGIN`, `PLATFORM_BLOCKED`, `RATE_LIMITED`,
   `NOT_FOUND`, `BAD_INPUT`, `BUSY`) stops the chain, others fall through.
   `_pick_error` prefers a real error over `NEED_SETUP`.
3. **Error taxonomy as the public API** — `NEED_SETUP`, `NEED_LOGIN`,
   `UPSTREAM_BROKEN`, `TIMEOUT`, `RATE_LIMITED`, `PLATFORM_BLOCKED`,
   `NOT_FOUND`, `BAD_INPUT`, `BUSY`, each with a canned hint `SKILL.md` tells
   the model to follow.
4. **Cross-process health/pacing state in `state.json`** (the part a naive port
   drops — it is deliberate load-shedding, not bookkeeping):
   - `backend_health`: per channel/action/backend pause after failure, keyed
     by a channel fingerprint — TIMEOUT 60 s, UPSTREAM_BROKEN 120 s,
     NEED_SETUP 60 s; paused backends are skipped, shown in `attempts`.
   - `cooldowns`: channel-wide 3600 s pause after a confirmed RATE_LIMITED,
     checked before any backend.
   - `request_starts`: ≥10 s between two `search`/`jobs` on one channel,
     taken under a lock so parallel calls cannot both skip it.
   - Public-web policy: for channels not yet added, `search`/`jobs`/`read` try
     the public web first (budget `min(20 s, remaining/2)`, backend labeled
     `public-web`); for added channels, public web is a **last-resort**
     fallback only after transient (UPSTREAM_BROKEN/TIMEOUT) failures.
5. **Browser concurrency via OS file locks, not a daemon** —
   `browser-locks/<sha256>.lock` with `msvcrt.locking` on Windows /
   `fcntl.flock` on POSIX. 2 slots per site, 4 total, 120 s queue wait, then
   `BUSY`. State writes take a `reach-state-write` execution lock and report
   `BUSY` rather than blocking. There is no stale-lock cleanup — keep that
   property.
6. **One shared browser, bounded lifecycle** — a single Reach-owned Chromium
   profile on CDP 9222 (configurable), headless, idle-close after 600 s,
   ~90 s cold-start wait; one dedicated login window pins it busy.
7. **Injectable clock** — timeouts pause ("off the clock") during interactive
   setup so a login flow doesn't eat the query budget. Port the `Clock`
   abstraction or the acceptance harness will mis-timeout.

## Dependency reality (decides the port shape)

| Upstream | Language | Role | Port to Rust? |
|---|---|---|---|
| opencli (`@jackwener/opencli` 2.0.3, floor 1.8.7) | Node + Chrome extension + local daemon | 7 channels (twitter, reddit, xiaohongshu, instagram, facebook, linkedin, boss) + session-reuse sourcing | **No.** Site adapters live upstream and churn. Keep as a child process + envelope parsing. |
| bili-cli (0.6.2) | Python | Bilibili search/read/hot/subtitles, QR login | Partly. `read`/`hot` already have a native HTTP fallback (`api.bilibili.com/x/web-interface/view\|popular`, unsigned, no login); `search`/`subtitle` need the CLI. yt-dlp was **removed** from bilibili (risk-control 412s) — don't reintroduce it. |
| boss-agent-cli | Python (git zip, pinned commit) | BOSS jobs; fallback when opencli fails | No. Keep. |
| yt-dlp (2026.8.19) | Python | YouTube info/search/audio download | **No.** It is the extractor; breaks weekly, fixed weekly. Vendor the binary, pin the version. |
| ffmpeg | native | audio compress/chunk for transcription | No, keep binary. |
| cloakbrowser 0.6.3 | Python (pip) | downloads + locates a stealth Chromium build | Replaceable (`chromiumoxide` fetcher or system Chrome) at the cost of fingerprint quality on xhs/instagram. |
| obscura v0.2.x (Apache-2.0) | Rust (binary) | from-scratch headless engine: V8 JS, own DOM/layout/paint, CDP **server**, Puppeteer/Playwright drop-in, `--stealth` build | **Partly.** Best-in-slot as a `web read` backend and stealth public-web engine (see next section). **Cannot host opencli** — no Chrome extension support exists in the engine, so the Chromium bus stays. |
| Jina / Exa MCP / V2EX / GitHub / Xueqiu / Bilibili public API / Groq / OpenAI | HTTP | plain REST / hosted MCP | **Yes.** `reqwest`. V2EX's `curl` TLS fallback (an OS-TLS workaround) becomes moot with rustls. |

Consequence: a "zero Python on the machine" binary covers web, v2ex, github,
xueqiu and bilibili read/hot only. YouTube, all opencli sites and BOSS need
their runtimes regardless of orchestrator language. What Rust actually buys:
one static binary, no interpreter/venv bootstrap for the orchestrator, native
async cancellation (the Python must run in-process backends in daemon threads
because HTTP calls cannot be interrupted — `tokio::time::timeout`/`select!`
does this for free), typed error paths. What it does not buy: fewer upstream
runtimes or less site churn. Note the Python entry point is already
lazy-import optimized (`agent_reach/cli.py` defers everything), so the
startup gain is real but modest.

## Obscura: adopt as an engine, not as the browser bus

Audited at HEAD `0d9a2f7` (2026-11-14), v0.2.7, Apache-2.0, 36.1k stars.
Obscura is a **from-scratch Rust browser engine** — ~195k LOC across 10 crates,
real JS via V8 (`deno_core`), its own DOM (html5ever/servo_arc), layout
(taffy), paint (tiny-skia), networking (reqwest + its own cookie jar), and a
CDP **server** that Puppeteer/Playwright connect to as if it were headless
Chrome. `Storage.getCookies/setCookies` and `Runtime.evaluate` are implemented
(verified in `crates/obscura-cdp`); `--stealth` adds fingerprint
randomization, tracker blocking, and a wreq/BoringSSL TLS transport.

**What it gives arana:**
- A self-hosted Jina replacement: `obscura fetch --dump text|html` renders JS,
  no third-party rate limit, no anti-bot interstitial from the reader itself.
  Make it the first backend of `web read` and the public-web route; keep Jina
  and Exa as fallbacks behind it.
- A stealth engine for public pages that today would need the whole Chromium
  bus (or lose to Cloudflare-style walls).
- Optionally a CDP endpoint (`obscura serve`) for cookie-managed public reads,
  drivable by the same `reach-browser` client.

**What it cannot do (verified against the tree, not assumed):**
- **No Chrome extension support anywhere** — no `chrome-extension://`
  service-worker targets, no `chrome.storage`, no `--load-extension`. The
  OpenCLI extension + bridge cannot run on it, so it cannot host the 7 opencli
  channels or the session-reuse sourcing. The Chromium bus is not replaced.
- Its fingerprint is a custom engine's, not Chrome's. Anti-bot efficacy on
  xiaohongshu/instagram/boss **login-walled** pages is unproven; treat
  `--stealth` as "better than plain reqwest", not "CloakBrowser for logged-in
  scraping". Public reads are the safe scope.
- Not on crates.io (the `obscura` crate there is an unrelated 2019
  raytracer). Do not vendor 195k LOC + a from-source V8 build (~5 min, GBs)
  into arana's workspace — consume the pinned release binary.

**How to integrate:** a `reach-tools` registry entry, kind `binary`, exactly
like chrome/cloakbrowser today — install the release archive into
`~/.arana/tools/obscura/`, `tested` pinned, `floor` set. On Windows
pull the `-stealth` archive (the scoop default has no stealth) and keep the
bundled runtime DLLs beside both executables. Two footguns: both reach's
Chromium and `obscura serve` default to port **9222** — allocate distinct
ports; obscura's SSRF gate blocks private IPs by default — keep that and
never forward user-supplied URLs with `--allow-private-network`.

**Shipping both:** arana ships as the CLI; obscura installs on demand via
the existing optional-tool pattern (`add`), keeping release cadences
decoupled. Apache-2.0 requires keeping its NOTICE — carry both attributions
(Agent-Reach MIT, Obscura Apache-2.0).

## Key modules, sized

- `runtime/manage.py` 1034 — add/remove/update/uninstall/doctor, skill
  linking, tool-install orchestration. Largest module; ship last.
- `runtime/browser.py` 947 — Chromium lifecycle, profile, idle watcher, login
  windows, session save/restore, cookie clearing.
- `runtime/tools.py` 639 — tool registry with `tested`/`floor`/`exclude`
  versions, private installs (own venv / `npm -g --prefix`, no admin), node /
  uv / ffmpeg / chrome prerequisites.
- `runtime/cli.py` 582 — argparse-that-raises, envelope emission, routing.
- `transcribe.py` 504 — yt-dlp download, ffmpeg duration/compress/chunk,
  Groq/OpenAI whisper, size budgets, SSRF guards (private-IP and
  literal-IP tricks like `127.1`).
- `runtime/dispatch.py` 322 — the fallback engine (invariants 2 and 4).
- `runtime/cdp.py` 222 — **hand-rolled** CDP: HTTP `/json/version`+`/json/list`
  discovery, a raw WebSocket client (manual handshake, SHA-1 accept, frame
  masking) for one-shot calls, and a `--remote-debugging-pipe` transport
  (NUL-delimited JSON over fds) used by the watcher. Also reads the OpenCLI
  extension's `chrome.storage.local` via `Runtime.evaluate` on its service
  worker. Small, well-bounded — easy Rust port (`tokio-tungstenite`, or
  `chromiumoxide` for the websocket part; the pipe is plain fd I/O).
- `runtime/session_reuse.py` 134 — site-scoped cookie copy. Sources: OpenCLI
  bridge `export_site_cookies(context, domain)` from the user's *connected*
  browsers, plus legacy snapshots. Writes via CDP `Storage.setCookies` with
  stale-cookie expiry and rollback on failure. **No Chrome cookie-DB
  decryption anywhere** — the design never reads Chrome's cookie store, so
  DPAPI / app-bound encryption is not a port problem; the coupling is to
  OpenCLI's Node Page API instead.
- `runtime/run.py` 173 — `run_cmd` with hard timeout; on Windows kills the
  process tree via `taskkill /PID <pid> /T /F`, on POSIX `killpg`. Rust note:
  `Child::kill` does not kill the tree — spawn `taskkill` or use Job Objects.
- `utils/paths.py` 213 — symlink-checked private dirs, atomic
  write-temp-then-rename, 0600 files, no-follow bounded reads. The security
  core; port faithfully.
- `runtime/onboarding.py` 239 — read-only "what public routes give you"
  guidance; `update_notice.py` — release notes fetched only on explicit
  `update --check`.

## Rust shape

```
arana/
  crates/
    reach-core/      # envelope, error codes, action/param registry, dispatch,
                     #   clock, state.json (with execution locks)
    reach-channels/  # web, v2ex, github, xueqiu, bilibili, youtube,
                     #   opencli_site (generic wrapper for the 7 opencli sites)
    reach-browser/   # profile, CDP (ws + pipe), idle watcher, slots, login,
                     #   session reuse, bridge client
    reach-tools/     # tool registry, version floors/excludes, private installs
    arana/       # bin: clap CLI (channel/action names are data-driven)
  tests/             # golden JSON per action, recorded from the Python build
```

Crate map:

- CLI: `clap` 4 (derive) — but build the subcommand tree from the registry,
  since channel/action/param names are data, not static.
- Async: `tokio`; HTTP `reqwest` (rustls) with per-request timeouts, a body
  cap (Python caps Jina at 1 MiB) and credential scrubbing on error paths.
- Serde: `serde`, `serde_json`, `serde_yaml` (opencli emits a YAML error
  envelope even under `-f json`, preceded by Node warnings; parse from the
  first `^ok: false$` line).
- Errors: `ReachError { code, message, hint }` + `thiserror`; a `classify()`
  equivalent mapping IO/timeout/HTTP/process failures to codes.
- Process: `tokio::process::Command` + `which`;
  `std::os::windows::process::CommandExt::creation_flags(0x0800_0000)`
  replaces `windows_hidden_kwargs`; process-tree kill via `taskkill /T /F`
  or Job Objects.
- Locks: `fd-lock` (or `fs2`) mirroring the msvcrt/fcntl lock files —
  nonblocking `execution_lock`, polling `wait_lock`, slots as N lock files.
- Browser: `chromiumoxide` (async CDP) or a hand-rolled `tokio-tungstenite`
  client mirroring `cdp.py`; the pipe transport is plain fd I/O.
- HTML: `scraper` + `dom_smoothie` (readability port) for the Jina fallback.
- `chrono`, `uuid`, `tracing` (stderr only — stdout is the JSON contract).

## Milestones

**M0 — freeze the contract (2–3 days).** From the Python build, dump
`agent-reach channels` and one real invocation per action into
`tests/golden/*.json`, captured with an isolated `HOME` (like `test.sh`
does). Write `schema.json` for the envelope, including `meta.attempts` shapes.

**M1 — core + no-auth channels (1–2 weeks).** Envelope, error taxonomy,
action registry, dispatch with fallback + backend-health pauses + cooldown +
pacing + public-web-first, state/config with lock files, clock. Channels:
`web` (Jina + Exa hosted MCP over JSON-RPC), `v2ex` (public JSON API),
`github` (REST, optional token), `xueqiu` (public quote/search), bilibili
`read`/`hot` via the public API. Exit: `channels` output byte-identical;
recorded actions return identical envelopes.

**M1.5 — obscura as a web backend (3–4 days).** `reach-tools` entry
(kind `binary`): download the pinned release, verify, resolve the executable.
Add `obscura fetch --dump text|html` as the first backend of `web read` and
the public-web route (Jina/Exa become fallbacks); `obscura scrape` for
multi-URL reads. Exit: `web read` on a JS-rendered page returns content Jina
cannot; `remove` deletes the tool cleanly. This slot deliberately does NOT
touch the browser bus — M3 stays Chromium.

**M2 — YouTube (3–5 days).** `yt-dlp`/`ffmpeg` as child processes, Groq /
OpenAI transcription with chunking, duration/size budgets, SSRF guards.
Exit: transcript for a fixed video matches the Python build.

**M3 — browser bus (1–2 weeks).** Chromium launch (cloak or system), profile,
cold start, idle-close watcher (pipe transport), single-instance + slot locks,
login-window handoff, cookie import/export via CDP, `browser
setup|status|stop|profiles|use`. Obscura may sit **alongside** this bus as an
optional CDP engine for public reads (`obscura serve` on its own port, never
9222), but the bus itself stays Chromium because opencli's extension needs
it. Exit: parallel reads on two sites both succeed; a login window survives
the idle timer; slot exhaustion yields `BUSY` after 120 s.

**M4 — opencli channels (1 week).** Generic `OpenCLIChannel` with per-site
`can_handle`/arg mapping, environment binding (`OPENCLI_PROFILE`,
`OPENCLI_WINDOW=background`), YAML error parsing, item normalization
(OpenCLI 2.0 wraps rows as `{ok, data: []}` — an empty list means zero rows,
not one row). Exit: the 7 opencli actions reproduce Python output live.

**M5 — management (1–2 weeks).** `add`, `remove`, `login`, `logout`,
`configure`, `doctor`, `check`, `update`, `uninstall`, `skill link/unlink`;
`state.json` `owner`/`scope`/`skill.links` bookkeeping is what keeps uninstall
safe; the `constraints.txt` equivalent is Cargo.lock plus the pinned
tool-version registry. Consider a compatibility shim until this lands.

**M6 — acceptance.** Port `test.sh` + `check_release_acceptance.py` as
`tests/acceptance.rs`. Upstream shipped 34/40 verified / 6 waived — the
harness must distinguish `waived` from `failed` and record reasons.

Realistic total: **5–8 weeks** solo for parity with M5; **~2 weeks** for a
genuinely useful M0–M2 build (web/v2ex/github/xueqiu/youtube + bilibili
read, no browser).

## Risks, ranked

1. **opencli is the product.** 7 of 14 channels plus session-reuse sourcing go
   through one Node CLI, its Chrome extension and its bridge daemon.
   Output-shape changes hit your parser. Mitigation: pin the version (floor
   1.8.7 / tested 2.0.3), parse defensively, keep `raw` passthrough.
2. **Stealth.** Plain Chromium trips xiaohongshu/instagram/boss risk control
   more than CloakBrowser's build. Test those three before dropping it; if
   blocked, keep CloakBrowser as the binary provider and replace only the
   Python launcher.
3. **Login glue is upstream-shaped.** Bilibili login reuse shells into
   bili-cli's own Python (`bili_cli.auth.save_credential`) to write its
   credential file; the Rust port must write `credential.json` directly
   (small, known JSON) or keep that shim. QR-login channels inherently keep
   their upstream CLIs.
4. **yt-dlp drift.** Never reimplement. Pin, and make `update` a
   tool-version operation like the original.
5. **Windows details** — hidden children (CREATE_NO_WINDOW), process-tree
   kill (`taskkill /T /F`), UTF-8 stdout (Python wraps the console manually;
   Rust gets this free), long paths, and the 0600/atomic-write/no-follow
   semantics of `utils/paths.py` (Windows ACLs differ — decide what "private"
   means there and test it).
6. **Live verification cost.** 321 unit tests are mostly logic-level; real
   parity needs accounts and cookies for 6+ platforms. Budget for manual
   acceptance; the 6 upstream waivers exist for a reason.
7. **Obscura is young.** v0.2.x on a 7-month-old engine with a fast release
   cadence; rendering is self-described as an evolving independent engine
   (long-tail CSS, media playback, some Web APIs differ from Chromium). Pin a
   tested version, wrap it in the backend-health pause machinery, and let
   failures fall through to Jina/Exa — it must degrade, never block the chain.
   Its port (9222 default) collides with the browser bus; its stealth
   efficacy against real anti-bot login walls is unproven and must be tested
   before depending on it beyond public reads.

## License

Agent-Reach is MIT — a port is fine; keep the copyright notice and attribution
to Panniantong/Agent-Reach in a `NOTICE` file. Obscura is Apache-2.0 — if you
redistribute its binaries (the `add` flow does), ship its NOTICE too and
state the pinned version.
