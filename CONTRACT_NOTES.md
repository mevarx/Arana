# Contract decisions discovered during implementation

The executable contract is Agent-Reach **1.6.1 at
`c70ae6da5d39bc90e09ad2ae0575ccc89f31e284`**, recorded from its real Python
build under an isolated home. `PRD.md` and `PORT_PLAN.md` are preserved as supplied.

## Draft catalog versus the pinned source

The draft PRD table enumerates **41** actions, although its stated target is 40.
The pinned Python registry actually declares **14 channels and 40 actions**.
The implementation follows that registry rather than inventing action names or
silently adding incompatible aliases:

| Channel | Draft spelling | Pinned upstream spelling |
|---|---|---|
| v2ex | `nodes` | `node` |
| youtube | `transcript` | `subtitle` |
| xueqiu | `hot`, `stocks` | `hot_posts`, `hot_stocks` |
| linkedin | `search` | `jobs` |
| reddit | extra `comments` action | No separately declared `comments` action |
| twitter | `read` | `thread` |

Action order, parameters, defaults, backend labels/methods, timeouts, and public
scopes are frozen in `tests/golden/registry.json`. Routing only selects channels
that actually expose `read`, in upstream registry order.

## Obscura extension without catalog drift

`channels` retains the upstream catalog, changing the program name to `arana`.
The implementation adds obscura to the **effective execution order** of `web
read`: `obscura → jina → exa`. Explicit `--backend obscura` is supported even
though the compatibility catalog's upstream `backends` field remains unchanged.
This resolves the tension between byte-identical upstream catalog output and the
new backend requirement without rewriting the frozen recordings. This extension
is documented in `README.md`; it is not hidden as a claimed upstream capability.

## Evidence rules

- Golden files contain real Python invocations and provenance, including errors.
  A recorded failure is not a successful live acceptance result.
- The recorder must supply every required input. A regression test covers all
  declared actions so argument-validation failures do not stand in for platform
  probes merely because a URL was omitted.
- Differential tests remove only `meta.elapsed_ms`. Live counters, rankings, and
  provider content can change between two real requests; those differences stay
  visible in `artifacts/differential/summary.json`, not silently normalized away.
- Bilibili can return HTTP 412 from this network. Matching error classification
  is evidence of error parity, not proof that the video was successfully read.
- Ordinary unit tests do not imply ignored/live tests have run. Explicit browser
  acceptance tests cover the actual 120-second queue wait and real Chromium.

## Platform boundary

Windows uses the WebSocket browser lifecycle, matching the upstream Windows
fallback. POSIX additionally has watcher-owned CDP pipes. A cross-target compile
check is not a Linux/macOS runtime test; the implementation report distinguishes
these evidence levels. Remote CI configuration is not claimed as a completed CI
run.
