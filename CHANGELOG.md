# Changelog

All notable changes to arana are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[semantic versioning](https://semver.org/).

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
