# M0–M3 implementation and verification

## Delivery scope

A five-crate Rust workspace now builds the `arana` executable and the
`agent-reach` compatibility alias. Implementation covers M0, M1, M1.5, M2 and
M3. **Implementation complete does not mean every external acceptance condition
has been verified.** In particular, live paid-provider ASR and POSIX browser
runtime acceptance remain open below. M4 OpenCLI adapters and M5 general
management are intentionally outside this delivery.

`PRD.md` and `PORT_PLAN.md` are preserved. The pinned Agent-Reach source is
1.6.1, commit `c70ae6da5d39bc90e09ad2ae0575ccc89f31e284`.
`CONTRACT_NOTES.md` explains the six catalog differences between that source and
the draft PRD; no incompatible action aliases were silently invented.

## Milestone evidence

| Milestone | Implemented | Actual verification / limits |
|---|---|---|
| M0 | Frozen registry, envelope schema, real per-action Python recordings and recorder provenance | 14 channels, 40 actions, 40 real invocations recorded. The manifest records 21 live successes; other recordings honestly retain errors, including missing upstream dependencies. Catalog output is tested byte-for-byte after program-name substitution. |
| M1 | Core dispatch, deadlines, pacing, cooldown/health, atomic private state; native web, GitHub, V2EX, Xueqiu and Bilibili backends | Full offline suites and five real release-CLI smoke probes pass. Latest 12-case Python/Rust differential has ten exact successful matches, one exact error match, and one live counter-drift case. This is not claimed as twelve successful identical live envelopes. |
| M1.5 | Pinned SHA-256-verified obscura install, fetch backend, first public-web fallback, owned-only removal, full Apache license/attribution; bounded multi-URL scrape Rust API | Actual release CLI installs 0.2.7, renders ten JS-only quote nodes / 1457 characters (a real Jina comparison returns only 256 characters and none of the first quote), then removes ledger-owned files and preserves a user file. A real two-worker `scrape` probe also returns two ordered nonempty public-page items and removes its installation. |
| M2 | yt-dlp read/search/subtitle, VTT normalization; real ffmpeg splitting, Groq/OpenAI multipart ASR, budgets, SSRF and retry rules | Media tests pass under the normal parallel runner. Fixed-video subtitle items exactly match pinned Python output (217 characters, English). Both builds downloaded real audio and ran ffmpeg, then produced identical normalized transcript items through an explicitly labeled local mock provider. **Actual Groq/OpenAI recognition has not been validated.** |
| M3 | Dedicated Chromium ownership/profile, launch and idle watcher, 2/site + 4/global OS-locked slots, login pin, CDP cookie import/export/logout; POSIX watcher-owned fd3/fd4 pipe and readiness hook | Real Windows Chromium parallel reads on two controlled origins pass; a visible credential-free login window survives a zero-idle threshold and closes after unpinning. Real default queue exhaustion returns BUSY after 120.032 seconds. POSIX code passes Linux cross-target checks and strict Clippy, and independent Luna review, but has not run on Linux/macOS. |

## Important corrections and hardening

- Windows private state permissions use the actual process-token SID and a
  protected DACL. A spoofed `USERNAME` cannot select another ACL owner; regression
  tests inspect the saved descriptor for exactly one full-access allow ACE.
- Nested process-backend futures use heap-backed scratch space, fixing a real
  Windows main-thread stack overflow. Both future-layout and actual CLI failure
  regressions pass; no enlarged-stack production workaround is used.
- HTTP response caps are enforced incrementally. Ambiguous URL authorities are
  rejected while output URL spelling preserves the pinned contract.
- Downloads and archives have byte/entry/expansion limits and path/link guards.
  Install/remove share an execution lock; forged ownership entries, linked
  ancestors and unrecorded files cannot redirect destructive cleanup.
- Media test environment mutations are serialized and restored; fixture locking
  cannot poison later tests. Work directories are atomically unique; 429 retry
  leaves an upload window rather than consuming the entire remaining budget.
- Pipe commands have a bounded deadline, bounded NUL frames and reply queues;
  readers are drained and aborted on close/drop. Hidden watcher arguments are
  validated before browser launch. Profile path comparison respects POSIX case.
- Secrets are not echoed by configure or included in packaged files. Human login
  is not automated; personal browser cookie databases are never read.

## Reproduce the gates

Latest full local gates: **194 Rust tests passed, 0 failed, 13 explicitly
ignored**, plus **2 Python tests passed**; formatting, strict workspace Clippy and
the locked release build all passed. Three real browser acceptance tests were
also run explicitly rather than counted as ordinary unit-test successes.

```sh
python scripts/verify.py --release
python scripts/benchmark.py
python scripts/smoke.py --binary target/release/arana.exe
python scripts/differential.py --binary <absolute-release-binary>
python scripts/verify_live_obscura.py --binary target/release/arana.exe
cargo run -p reach-tools --example live_batch -- <isolated-state-directory>
cargo test -p reach-browser --test browser_acceptance --test chrome_smoke -- --ignored --nocapture
cargo test -p reach-browser --test acceptance -- --ignored --nocapture
```

On Unix omit `.exe`. The differential harness defaults to the isolated pinned
Python environment and intentionally exits nonzero when a provider fails or live
values differ; it removes elapsed time only, not mutable counters. It does not
turn a matching blocked response into a successful acceptance result.

Authoritative local evidence:

- `artifacts/verification/verification.json` and its separate Python/fmt/Clippy/
  Rust-test/release logs; Rust test totals are parsed from actual test output.
- `artifacts/benchmark.json`: measured release size, SHA-256 and forty fresh-process
  startup samples. This is warmed-filesystem startup, not a claim about uncached
  disk startup. Both the 30 MiB size and 50 ms p95 targets passed locally.
- `artifacts/smoke/summary.json`, `artifacts/differential/summary.json`.
- `artifacts/obscura-live/`, `artifacts/batch-live.json` and independent Luna
  approval in `artifacts/batch-review.json`.
- `artifacts/media-parity.json` and the Python/Rust subtitle and explicitly
  mock-provider transcript JSON captures.
- `artifacts/tools-hardening.json`: archive parsing and cleanup evidence, with
  residual metadata-allocation bounds explicitly recorded.
- `artifacts/browser-acceptance.json`, `artifacts/browser-queue.json`,
  `artifacts/browser-review.json`, `artifacts/linux-cross-check.json`.

The release bundle contains binaries, documentation, license/attribution and a
small set of verification summaries—not credentials, cookies, downloaded browser
engines, private state or the upstream virtual environment. yt-dlp/ffmpeg and
Chromium remain external runtime prerequisites for their respective features.

## Remaining acceptance gaps and later milestones

1. Run actual Groq/OpenAI ASR using explicitly configured arana credentials.
   The mock-provider parity result is deliberately not a substitute.
2. Run native Linux/macOS browser lifecycle tests. Linux cross-compilation and
   independent review do not establish POSIX runtime correctness. Remote CI is
   configured, but no remote CI run is claimed.
3. Bilibili video read currently returns HTTP 412 on this network in both builds;
   hot-list favorite counters can change between requests. These live conditions
   prevent an all-green exact differential, even though fixture normalization
   tests and the public hot-list smoke pass.
4. Implement M4 OpenCLI channel adapters, then M5 full setup/check/update/uninstall,
   profile/session management and migration. Some registry actions already appear
   in the frozen catalog but correctly report setup/unsupported-backend errors
   until those later adapters exist; catalog presence is not implementation proof.

No credentials were requested or taken from unrelated application configuration.
No commits or remote publication are implied by this report.
