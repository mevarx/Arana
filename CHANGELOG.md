# Changelog

All notable changes to arana are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[semantic versioning](https://semver.org/).

## [0.3.1] - 2026-11-24

### Added

- `arana extract` reads a rendered page through obscura and returns structured
  fields: one item per CSS match, text or attribute, optionally resolved
  against the page URL. Hidden, scripted and zero-width content is removed
  before an agent sees it, and `--adaptive` stores an element fingerprint that
  relocates the match after a site redesign. The command is outside the frozen
  action catalog and is not exposed over MCP.

### Verification

- This regular release does not change the separate live platform acceptance
  status. See [Verification](docs/verification.md) for its scope and limitations.
- The obscura adapter can now request `--dump html` and pass `--selector` as a
  render/wait condition; `web read` keeps using `--dump text`.

## [0.3.0] - 2026-11-22

### Added

- `read_many` reads up to five public URLs through MCP in a single request. It
  preserves input order, identifies which sources failed, and shares a deadline
  and response-size budget across the batch.
- The Arana skill prefers the current agent host's permitted browser when a
  page needs rendering. `arana browser route` accepts the host's observed
  availability and checks for Chrome/OpenCLI only when explicitly authorized.
  This check does not start a browser, install tools, or verify the bridge.
- A version-pinned installer verifies the release archive's SHA-256 before
  installing into an explicit directory. It does not modify `PATH`.
- A reproducible research walkthrough shows how to cite public sources and
  report missing ones in Hermes and Claude Code.

### Compatibility

- The CLI action catalog is unchanged. Public MCP stays read-only and does not
  provision browsers or use a browser profile. Browser-backed standalone
  actions still require the applicable setup or login.
- This is a regular GitHub release. It does not imply full live verification
  across third-party sites; see the [verification record](https://github.com/mevarx/Arana/blob/v0.3.0/docs/verification.md).

## [0.2.0] - 2026-11-18

**Agent integration release.** Adds a local, read-only MCP server and packages
Arana as an agent plugin — one shared skill plus host manifests — for eleven
agent clients, with the CI and packaging fixes that first multi-platform run
exposed. The read-only CLI contract is unchanged.

### Added

- **Local MCP server.** `arana mcp` speaks newline-delimited JSON-RPC over stdio
  and exposes `list_capabilities`, `read_url` and `search`. It maps only to
  public web actions: no channel, backend, profile, login, install or browser
  surface. Requests are bounded (1 MiB frames, 25 items, 16 000 chars per item,
  256 KiB per result, `limit` ≤ 10), a 30 s deadline and cancellation are
  enforced, protocol output stays on stdout and diagnostics on stderr.
- **Plugin packaging.** Root `plugin.json` (Agent Plugins v1), portable
  `mcp.json`, Claude `.claude-plugin/plugin.json` + marketplace, a Codex
  marketplace entry and an Antigravity manifest. The canonical skill moved to
  `skills/arana/SKILL.md` and now tells an agent to check `arana version` and
  install the matching release instead of improvising.
- **`docs/agent-plugins.md`.** Per-host installation, configuration and
  verification for Hermes, Codex, Claude Code, Antigravity, Cursor, Devin,
  OpenCode, Command Code, Oh My Pi, DeepSeek Harness and Pi, each labelled
  exercised, documented, preview or blocked.
- **`DOCS.md` and `SECURITY.md`.** A documentation index and a private
  vulnerability-reporting policy.

### Changed

- The README leads with installation and runnable examples; the released
  archives ship the skill, every manifest, both MCP configs and the new docs.
- `scripts/ci_package.py` rejects package name, version and schema drift before
  writing an archive, and both packagers verify the files they ship.

### Fixed

- The Windows ACL test reads the binary security descriptor instead of SDDL,
  which abbreviates well-known SIDs.
- Packaging decodes child output as UTF-8 rather than the runner's locale codec.
- POSIX test fixtures are real shell scripts; batch-only fixtures failed with
  `EACCES` on Linux and macOS.
- State roots below macOS platform symlinks are resolved at the CLI boundary.
- CI runs every suite with `--no-fail-fast`, supports merge queues and manual
  runs, and authenticates its optional live GitHub probes.

### Removed

- Two registry helpers nothing called, and seven dependency declarations no
  crate referenced.

### Verified

- 313 Rust tests and both Python suites pass; formatting, strict workspace
  Clippy and the locked release build are clean.
- CI is green on Ubuntu, Windows and macOS. Hermes and Claude Code installed the
  plugin in isolated profiles and reported the MCP server connected; `arana mcp`
  completed a live public read from the release binary.
- The optional live probe job passes when run with a token.

### Known limitations

- Nine of the eleven hosts are documented from vendor docs rather than
  exercised end-to-end. Devin cloud sessions need Arana installed in Devin's
  execution environment; DeepSeek Harness is a developer preview.
- Release acceptance is unchanged at **23 of 34** required live passes; passing
  it still needs an operator with real platform logins.
- Browser runtime behaviour is verified on Windows only.

## [0.1.0] - 2026-11-18

**First public pre-release.** arana is a Rust reimplementation of
[Agent-Reach](https://github.com/Panniantong/Agent-Reach) 1.6.1, pinning the
same 14-channel / 40-action catalog and the same one-object JSON envelope. It
is read-only: nothing is ever posted, voted, followed or moderated.

### Added

- **Unified contract.** Every action returns one envelope
  (`schema_version`, `ok`, `channel`, `action`, `backend`, `items`, `meta`,
  `error`), exit status follows `ok`, and `--verbose` diagnostics go to stderr
  so stdout stays machine-parseable.
- **Native backends** for web (Jina + Exa), GitHub, V2EX, Xueqiu and Bilibili,
  plus YouTube read/search/subtitle and Groq/OpenAI transcription with real
  ffmpeg chunking, duration/size budgets and SSRF guards. No Python runtime is
  required for these channels.
- **obscura as the first public-read engine.** A pinned, sha256-verified
  external binary renders JavaScript-only pages that plain fetches cannot see;
  Jina and Exa remain fallbacks, and the frozen catalog stays byte-identical.
- **Browser bus.** A dedicated Chromium profile with ownership proof, 90 s cold
  start, 600 s idle close, a login window that pins the browser, CDP cookie
  import/export and 2-per-site / 4-global slots that report `BUSY` after a real
  120 s wait. POSIX adds watcher-owned CDP pipes (fd 3/4); Windows uses the
  WebSocket transport.
- **OpenCLI bridge.** `browser setup` binds an arana-owned context and installs
  the pinned extension (1.0.27, checksum-verified) into the private profile, so
  the seven browser-session channels run without touching a personal browser.
- **Management surface.** `setup`, `add`/`remove`, `update [--check]`,
  `login`/`logout`, `doctor`, `check`, `uninstall`, `skill link|unlink|update`,
  `configure` and a non-destructive `setup --migrate` import.
- **Ownership-safe tool management.** Ten pinned tools install privately
  (pip venvs, npm prefixes, node/uv/ffmpeg/chrome trees), reuse a compatible
  system copy as-is, and remove only manifest-recorded files — anything else in
  an owned tree is listed and preserved.
- **Release acceptance harness.** A recorder drives the real binary across the
  catalog; a fail-closed validator enforces the frozen 40 actions, ≥34 real
  passes, the exact six upstream waivers, freshness and a matching source
  signature. A self-claimed manifest is re-checked against the binary.

### Verified

- 285 Rust tests and 11 Python tests pass; formatting, strict workspace Clippy
  and the locked release build are clean.
- Release binary is 7.4 MiB with a 19.7 ms p95 `channels` startup.
- Real probes pass for obscura install/render/remove, fixed-video YouTube
  subtitles (identical to the Python build), parallel Chromium reads, login-pin
  survival and a measured 120 s slot timeout.

### Known limitations

- **Release acceptance is not met.** A real recording measured 23 of the
  required 34 live passes (11 failures, 6 upstream waivers, nothing waived):
  logged-in platforms, the OpenCLI bridge and provider keys were unavailable on
  the recording host. Passing the gate needs an operator with real logins.
- Transcription has not been exercised against a live Groq/OpenAI key; the
  pipeline is proven with a real download plus a labelled mock provider.
- The browser bus is exercised on Windows. POSIX pipe ownership compiles
  cross-target and passed review but has not run on a Linux/macOS host.
- Private Unix Node installation is fixture-verified only; macOS
  Chrome-for-Testing fallback can refuse framework symlinks.

### Notes

- Requires Rust stable (edition 2021) to build from source; the release binary
  needs no Rust at runtime.
- Chrome/Chromium, Node.js, `yt-dlp` and `ffmpeg` remain external prerequisites
  for the features that use them, and are installed privately when missing.
