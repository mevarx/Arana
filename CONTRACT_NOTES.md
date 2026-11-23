# Contract notes

The executable contract is **Agent-Reach 1.6.1 at
`c70ae6da5d39bc90e09ad2ae0575ccc89f31e284`**, recorded from its real Python
build under an isolated home. That registry — not prose — is the source of
truth for channel names, action names, parameters, defaults, backend order and
timeouts. This page records the decisions made *inside* that contract, and the
evidence rules that keep the claims honest.

## Naming and scope decisions

These names follow the pinned registry exactly. Several older integrations used
different spellings; arana adds no aliases for them, because an unknown name is
a `BAD_INPUT` that lists the valid names — it fails loudly instead of silently
doing something else.

| Channel | Name | Note |
|---|---|---|
| `v2ex` | `node` | not `nodes` |
| `youtube` | `subtitle` | not `transcript`; `transcribe` is the ASR action |
| `xueqiu` | `hot_posts`, `hot_stocks` | the community actions, distinct from `quote`/`search` |
| `linkedin` | `jobs` | the public job search |
| `twitter` | `thread` | one conversation, while `tweets` is a timeline |
| `reddit` | no separate `comments` action | comments are merged into `read` |

Action order, parameter names, defaults, backend labels/methods, timeouts and
public-web scopes are frozen in `tests/golden/registry.json`, and the `channels`
command is tested byte-for-byte against that file. Routing selects only
channels that actually expose `read`, in registry order.

## obscura without catalog drift

`channels` reproduces the upstream catalog unchanged apart from the program
name. The runtime prepends `obscura` to the **effective execution order** of
`web read` (`obscura → jina → exa`), and it can be selected explicitly with
`--backend obscura`, while the catalog's `backends` field keeps its upstream
value. Search remains Exa-based. This keeps the compatibility promise and the
self-hosted backend from fighting over the same bytes; the extension is
documented in the README rather than hidden inside a catalog field.

## Post-parity extraction command

`arana extract` sits outside the frozen action contract. It is a separate
top-level verb, so `channels` bytes and the 40-action catalog are untouched; it
renders through obscura and parses the HTML locally. Public MCP stays on the
Jina/Exa HTTP allowlist and does not expose it. Its adaptive fingerprints live
under `~/.arana/extract/`, keyed by the fragment-free URL and selector, and are
private state written through the same atomic, symlink-guarded path as the rest
of `~/.arana`.

## Evidence rules

- `tests/golden` contains real Python invocations with provenance, **including
  their errors**. A recorded failure is not a successful live result.
- The recorder must supply every required input, and a regression test covers
  all declared actions so a missing URL cannot masquerade as a platform probe.
- The differential harness removes `meta.elapsed_ms` and nothing else. Live
  counters, rankings and provider content legitimately change between two real
  requests; those differences stay visible in `artifacts/differential/`.
- Some networks get HTTP 412 from Bilibili. Matching error classification is
  evidence of error parity, not proof the video was read.
- Ordinary unit tests do not imply ignored/live tests ran. Browser acceptance is
  an explicit opt-in suite, and the release-acceptance recorder reports the
  real pass count.
- A manifest that claims its own success is metadata, not proof: the acceptance
  test re-derives catalog and source signature from the binary itself.

## Platform boundary

Windows uses the WebSocket browser lifecycle, matching upstream's Windows
behaviour. POSIX additionally has watcher-owned CDP pipes. A cross-target
compile is not a Linux/macOS runtime test, and configured CI is not an observed
CI run; the verification docs state exactly which of those has happened.
