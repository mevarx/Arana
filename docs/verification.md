# Verification

arana distinguishes three levels of evidence, and never converts one into
another.

| Level | What it proves | How it runs |
|---|---|---|
| Offline suites | Logic, contracts, guards, fixtures | `cargo test --workspace --locked`, `python -m unittest` |
| Real probes | This host, this network, real binaries | Explicit `--ignored` tests and the `scripts/` harnesses |
| Release acceptance | The frozen 40-action contract at ≥34 real passes | `scripts/record_release_acceptance.py` + the validator |

## Current measured state

- **313 Rust tests pass**, 0 fail, **19 explicitly ignored** live tests.
- **18 Python tests pass** (contract recorder, acceptance validator and package checks).
- Formatting, strict workspace Clippy and the locked release build are clean.
- Release binary: **7.36 MiB**, fresh-process `channels` startup **19.7 ms p95**
  (40 samples, warmed filesystem), both inside the 30 MiB / 50 ms targets.
- Real probes that passed on the reference host: obscura install → JS render
  (10 quotes / 1457 chars) → owned removal; two-worker `scrape`; fixed-video
  YouTube subtitle identical to the Python build; real Chromium parallel reads,
  login-pin survival and CDP smoke; `BUSY` after a real 120 s slot wait;
  credential-free bili-cli search/read; private bili-cli, yt-dlp and opencli
  installs (inspect/update/remove).
- **Release acceptance is recorded, not met.** A fresh recording of the `v0.2.0`
  binary on Windows (2026-11-21) measured **26 passes, 8 failures and 6 upstream
  waivers** (`release_ready: false`), stored in the repository at
  [`.github/release-acceptance.json`](../.github/release-acceptance.json). Every
  anonymous action passes. All eight failures are credential-gated: `NEED_SETUP`
  for the OpenCLI bridge (twitter search/profile/tweets/thread, xiaohongshu
  comments, facebook profile) and the missing transcription key, and
  `NEED_LOGIN` for xueqiu hot posts. None were waived. Passing ≥34 still needs
  an operator with real platform logins.
- Live probes read their targets from `tests/golden/live-inputs.json`, which
  overrides frozen parity ids that no longer resolve upstream. The earlier
  `NOT_FOUND` on `bilibili/read` was such an id, not a defect: the same backend
  reads a current video normally.

## Reproduce

```sh
# full local gate: python suites, fmt, strict clippy, all tests, release build
python scripts/verify.py --release

# performance and size
python scripts/benchmark.py

# live probes (real network, isolated state, failures preserved)
python scripts/smoke.py --binary target/release/arana.exe
python scripts/differential.py --binary <absolute-release-binary>
python scripts/verify_live_obscura.py --binary target/release/arana.exe
cargo run -p reach-tools --example live_batch -- <isolated-state-directory>

# real browser (Windows/macOS/Linux with Chrome installed)
cargo test -p reach-browser --test browser_acceptance --test chrome_smoke -- --ignored --nocapture
cargo test -p reach-browser --test acceptance -- --ignored --nocapture

# release acceptance
python scripts/record_release_acceptance.py --binary target/release/arana.exe --out <manifest>.json
python scripts/check_release_acceptance.py <manifest>.json --binary target/release/arana.exe
```

## What the gates actually enforce

- **Catalog parity**: `arana channels` is byte-identical to the pinned Python
  build after substituting the program name (asserted on exact bytes).
- **Differential**: Python and Rust run the same command on the same network
  and only `meta.elapsed_ms` is removed. A differing live counter, a provider
  failure, or a blocked page stays a failure — the harness is not allowed to
  normalize it away.
- **Recordings vs. success**: `tests/golden` keeps the real recorded errors,
  including missing upstream dependencies. A recording is *not* evidence of a
  live success, and the manifest records how many were live.
- **Acceptance**: exact 40 actions, ≥34 real passes, only the six upstream
  waivers with their exact approved reasons, fresh timezone-aware timestamps, a
  source signature that must match the binary, and no fabricated rows. The
  ignored Rust acceptance test re-derives the catalog and signature from the
  binary itself, so a hand-written manifest cannot satisfy the gate.
- **Signature coverage**: the build signature hashes crate sources and
  manifests, `Cargo.lock`, `schema.json`, `skills/arana/SKILL.md` and the
  golden recordings with repository-relative paths — changing a runtime asset
  invalidates acceptance evidence.

## Evidence artifacts

`scripts/verify.py` writes `artifacts/verification/` (one log per gate plus
`verification.json` with parsed test totals). The probes write
`artifacts/smoke/`, `artifacts/differential/`, `artifacts/obscura-live/`,
`artifacts/browser-*.json`, `artifacts/benchmark.json` and
`artifacts/m6-acceptance/`. Everything under `artifacts/` is local and
git-ignored: the evidence is reproducible, not committed.

## Known limitations

- Real Groq/OpenAI transcription has not been exercised with a live key; the
  Whisper pipeline is proven against a real download plus an explicitly
  labelled mock provider.
- The browser bus is fully exercised on Windows. POSIX pipe ownership passes
  Linux cross-target checks and independent review but has not run on a
  Linux/macOS host here. Remote CI is configured, not observed.
- Bilibili video reads can return HTTP 412 from some networks; hot-list
  counters drift between two live requests, so exact differential parity is
  impossible by construction for that case.
- Private Unix Node installation and the regular-members extractor are
  fixture-verified on Windows only.
