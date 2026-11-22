# Verification

Arana distinguishes three levels of evidence, and never converts one into another.

| Level | What it proves | How it runs |
|---|---|---|
| Offline suites | Logic, contracts, guards, fixtures | `cargo test --workspace --locked`, `python -m unittest` |
| Real probes | This host, this network, real binaries | Explicit `--ignored` tests and the `scripts/` harnesses |
| Release acceptance | The frozen 40-action contract at ≥34 real passes | `scripts/record_release_acceptance.py` + the validator |

## Current measured state

Measured on Windows 11 on 2026-11-22 against the v0.3.0 tree and local release build.

- **328 Rust tests pass**, 0 fail, **19 explicitly ignored** live tests.
- **55 Python tests ran: 54 passed, 1 skipped** (10 repository tests and 45 installer/acceptance/release-asset tests under `scripts/tests/`).
- Formatting and strict workspace Clippy are clean; the locked release build succeeds.
- For local executable size and cold-start measurements, run the benchmark below. Treat timings as host-specific; a single local measurement is not a cross-platform performance guarantee.
- v0.3.0 targeted live probes passed: a direct stdio handshake listing all four tools; `read_many` over `https://example.com` and `https://example.org` with both sources `ok` in input order; a partial batch where `https://example.invalid/` returned a per-source `UPSTREAM_BROKEN` while the batch stayed `ok: true`; a `file:///…` input rejected as `BAD_INPUT` with no results; and `arana browser route` decisions for `host_browser`, `unavailable`, `unsupported`, and `blocked` with no state created.
- Real probes that passed on the reference host in the v0.2.0 cycle: obscura install → JS render (10 quotes / 1457 chars) → owned removal; two-worker `scrape`; fixed-video YouTube subtitle identical to the Python build; real Chromium parallel reads, login-pin survival and CDP smoke; `BUSY` after a real 120 s slot wait; credential-free bili-cli search/read; private bili-cli, yt-dlp and opencli installs (inspect/update/remove).
- **Release acceptance is recorded, not met.** A fresh recording of the `v0.2.0` binary on Windows (2026-11-21) measured **26 passes, 8 failures and 6 upstream waivers** (`release_ready: false`), stored in the repository at [the v0.2.0 acceptance recording](https://github.com/mevarx/Arana/blob/44908a779c3bcb93b93139c1dfef8b10c0fd152b/.github/release-acceptance.json). The eight failures required additional setup or login: `NEED_SETUP` for the OpenCLI bridge (Twitter search/profile/tweets/thread, Xiaohongshu comments, Facebook profile) and the missing transcription key, and `NEED_LOGIN` for Xueqiu hot posts. None were waived. Passing ≥34 still needs an operator with real platform logins. v0.3.0 has not been re-recorded; a regular GitHub release does not change this result.
- Live probes read their targets from `tests/golden/live-inputs.json`, which overrides frozen parity ids that no longer resolve upstream. The earlier `NOT_FOUND` on `bilibili/read` was such an id, not a defect: the same backend reads a current video normally.

## Reproduce

```sh
# full local gate: python suites, fmt, strict clippy, all tests, release build
python scripts/verify.py --release

# performance and size
python scripts/benchmark.py

# MCP process-level suite (stdio framing, tools, bounds, structured errors)
cargo test -p arana --test mcp

# installer, acceptance-unit, and release-asset suites
python -m unittest discover -s scripts/tests -p "test_*.py"

# published asset set: exactly one zip plus sidecar per platform, expected
# archive contents, and a plugin version matching the tag
python scripts/verify_release_assets.py --dir dist --tag v0.3.0

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
- **Release assets**: `scripts/verify_release_assets.py` requires exactly the three platform archives and one sidecar each, rejects extra archives or sidecars, checks every sidecar's digest and embedded name, opens each archive for CRC and contents (both binaries, `plugin.json`, the canonical skill), and requires the packaged plugin version to match the tag. It fails closed.
- **Signature coverage**: the build signature hashes crate sources and
  manifests, `Cargo.lock`, `schema.json`, `skills/arana/SKILL.md` and the
  golden recordings with repository-relative paths — changing a runtime asset
  invalidates acceptance evidence.

## Evidence artifacts

`scripts/verify.py` writes `artifacts/verification/` (one log per gate plus
`verification.json` with parsed test totals). The probes write
`artifacts/smoke/`, `artifacts/differential/`, `artifacts/obscura-live/`,
`artifacts/browser-*.json` and `artifacts/benchmark.json`. Everything under
`artifacts/` is local and git-ignored: the evidence is reproducible, not
committed. The release-acceptance recording is the exception:
`.github/release-acceptance.json` is committed so reviewers can re-validate it
against the binary.

## Known limitations

- Real Groq/OpenAI transcription has not been exercised with a live key; the Whisper pipeline is proven against a real download plus an explicitly labelled mock provider.
- The browser bus is fully exercised on Windows. POSIX pipe ownership passes Linux cross-target checks and independent review but has not run on a Linux/macOS host here.
- The [preceding `main` CI run](https://github.com/mevarx/Arana/actions/runs/45004011840) passed on Linux, Windows, and macOS. This is not a CI result for the v0.3.0 commit.
- `read_many` was exercised live against two example domains and one failing host. That proves batching, ordering, and per-source errors, not every backend or page shape.
- `arana browser route` was exercised for route decisions only. It does not launch a browser, and no test here verifies the OpenCLI bridge or a logged-in session.
- Host integration evidence is 0.1.0-era for Hermes and Claude Code and was collected on Windows 11. Nine hosts remain documented, preview, or blocked; no host was exercised on macOS or Linux.
- Bilibili video reads can return HTTP 412 from some networks; hot-list counters drift between two live requests, so exact differential parity is impossible by construction for that case.
- Private Unix Node installation and the regular-members extractor are fixture-verified on Windows only.
