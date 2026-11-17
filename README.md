# arana

Read-only internet access for AI agents, implemented in Rust. Derived from
Agent-Reach 1.6.1 (`c70ae6d`); public web reads add obscura 0.2.7 as a private,
self-hosted backend. Requirements and milestone exit criteria are in
[`PRD.md`](PRD.md) and [`PORT_PLAN.md`](PORT_PLAN.md). Read
[`IMPLEMENTATION_REPORT.md`](IMPLEMENTATION_REPORT.md) for verified scope and
remaining acceptance gaps, and [`CONTRACT_NOTES.md`](CONTRACT_NOTES.md) for
source-versus-draft catalog differences.

## Build

```sh
cargo build --workspace --release --locked
cargo test --workspace --locked
```

The workspace produces `arana` and its compatibility alias `agent-reach`.
On Windows, append `.exe`. Rust is needed to build, not to run the CLI.
External extraction tools still have their own runtime requirements.

## Use

```sh
arana channels
arana github read rust-lang/rust --backend github-api
arana github search rust --limit 2 --backend github-api
arana v2ex hot --limit 2
arana bilibili hot --limit 2 --backend bilibili-api
arana xueqiu quote AAPL --backend tencent
arana read https://example.com
arana web search "Rust async cancellation" --limit 5
```

Unified commands emit exactly one UTF-8 JSON envelope. A failure exits nonzero;
branch on `ok` and `error.code`, not error prose. `--verbose` sends diagnostics to
stderr only. Arguments after `--` are literal, including flag-looking strings.

```sh
arana web search -- --timeout-is-part-of-my-query
```

`channels` preserves the upstream catalog and usage contract. The runtime adds
`obscura` ahead of `jina` and `exa` for `web read`; it can be selected explicitly
with `--backend obscura`. Search remains Exa-based. Public-web fallbacks are
site-scoped and identify their provenance and unverified completeness.

### State and secrets

The default state root is `~/.arana/`. Set `ARANA_HOME` to an alternate **state
root** for testing; it is not a replacement for your normal home directory.
State writes are atomic, OS-locked, bounded, and symlink/reparse-checked. Private
permissions use 0700/0600 on Unix and a protected process-token-owner DACL on
Windows; `USERNAME` is not trusted for ACL identity.

```sh
arana configure groq-key --stdin
arana configure openai-key --stdin
arana configure github-token --stdin
arana configure browser-port 9222
```

Pipe secrets to `--stdin`; values are not returned in JSON or logs. Transcription
uses explicitly configured arana credentials. Browser login belongs to the human;
arana never requests passwords or reads a browser cookie database.

### Public web engine and browser bus

```sh
arana add obscura
arana web read https://example.com --backend obscura
arana remove obscura
arana browser setup
arana browser status
arana browser stop
```

Obscura is an external pinned release, not a vendored Rust dependency. Its
private-network protection is never disabled for user URLs. The browser bus is a
separate Chromium instance with an arana-owned profile; obscura cannot host the
OpenCLI extension. An unrelated browser occupying the configured CDP port must
not be taken over or stopped.

For batch readers, `reach_tools::obscura_batch::read_many` uses real `obscura
scrape` workers, with 1–32 validated public URLs, 1–4 workers, one overall
deadline, bounded output, and ordered per-URL results. It is a Rust API, not an
extra catalog action. A real opt-in probe is available as
`cargo run -p reach-tools --example live_batch -- <isolated-state-directory>`.

Full OpenCLI channel adapters and general management are M4–M5, outside this
implementation's requested M0–M3 scope. See the implementation report for actual
verified milestone status and any remaining acceptance blockers.

## Verification

```sh
python scripts/verify.py --release
python scripts/smoke.py --binary target/release/arana.exe
```

The first command runs formatting, strict Clippy, tests, and a release build,
preserving real logs under `artifacts/verification/`. The second runs live native
HTTP probes under isolated state and records successes **and** failures under
`artifacts/smoke/`. On Unix omit `.exe` in the binary path.

Ignored/live tests are not implied to pass by the ordinary test suite. Golden
recordings retain their Python provenance; a recorded failure is not evidence of
successful live parity. CI is configured for Linux, Windows, and macOS; adding a
workflow does not mean those remote jobs have run.

See `NOTICE` and `LICENSE` for upstream attribution.
