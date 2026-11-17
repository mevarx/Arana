# Contributing

## Ground rules

- **Read-only forever.** arana never posts, comments, votes, follows, messages
  or moderates. A patch that adds a write action will be rejected.
- **The contract is frozen.** Action names, parameters, defaults, backend
  ordering and the envelope shape come from Agent-Reach 1.6.1 (`c70ae6d`).
  `tests/golden/registry.json` and the byte-identical catalog test are the
  source of truth; runtime additions (like `obscura` as a `web read` backend)
  must not change those bytes.
- **No credentials, ever.** Do not test against accounts you were not asked to
  use, do not read a personal browser profile or cookie database, and do not
  put secrets in fixtures, logs or commit messages.
- **Own only what you create.** Installed state is managed through owners,
  manifests and hashes. See [Security](security.md#ownership-and-removal).

## Development setup

```sh
git clone <repo> && cd arana
cargo build --workspace --release --locked
cargo test --workspace --locked
```

On Windows the `stable-x86_64-pc-windows-gnullvm` toolchain is known good when
the GNU binaries are not; the build needs no C++ toolchain.

## Test discipline

Work test-first: write the failing test, watch it fail for the right reason,
then implement. The suites are organised so each property is proven once:

| Suite | Covers |
|---|---|
| `reach-core/tests/{contracts,dispatch,input,state_clock,process_state,public_deadline,windows_acl}` | envelope, error taxonomy, dispatch rules, state atomicity, ACL identity |
| `reach-channels/tests/{registry_parity,contract_shape,native_backends,external_backends,session_safety,http_safety,media,media_future}` | catalog bytes, recorded envelopes, every backend and its error mapping |
| `reach-tools/tests/{registry_config,download_archive,node_archive,obscura,obscura_batch,management,runner,ssrf_corpus}` | pins, extraction guards, ownership/removal, process bounds, SSRF |
| `reach-browser/tests/{slots_cookies,opencli,browser_acceptance,acceptance,chrome_smoke}` | slot limits, bridge, real Chromium |
| `arana/tests/{cli,configure,management,fsops,watcher,stack_regression,source_signature,acceptance}` | the real binary's surface and safety |

Rules for tests:

- A live test is `#[ignore]` with a reason, never a silent early return. A
  fixture whose setup fails must hard-assert, not `return` — a skipped
  destructive-path test looks green while proving nothing.
- Environment-mutating tests serialise and restore; no test may poison another.
- When a defect is found in review, land a regression for it in the same change.

## Gates before a pull request

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python -m unittest discover -s tests -p "test_*.py"
python -m unittest discover -s scripts/tests -p "test_*.py"
python scripts/verify.py --release     # runs all of the above and keeps the logs
```

Live probes (`scripts/smoke.py`, `scripts/differential.py`,
`verify_live_obscura.py`, the `--ignored` tests) are expected to be run when
the change touches their area, and their failures reported honestly.

## Commits

Conventional prefixes (`feat`, `fix`, `docs`, `test`, `build`, `chore`), one
logical change per commit, and a body that states *why* and which safety
property is affected. Do not stage generated evidence (`artifacts/`, `dist/`,
`target/`) or credentials.

## License

By contributing you agree your work is licensed under the MIT license in
[`LICENSE`](../LICENSE). arana is a derivative of Agent-Reach; keep the
upstream attribution in [`NOTICE`](../NOTICE) intact.
