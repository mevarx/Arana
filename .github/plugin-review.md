# Arana plugin review notes

Arana ships a portable Agent Plugins package for Hermes and other compatible
hosts, plus a Claude Code manifest and marketplace. Both load the same `arana`
skill and can start the same local `arana mcp` stdio server. No native Hermes
code, hooks, background service, desktop extension, or cloud connector is
installed.

## Prerequisites and scope

- Install the matching Arana CLI separately from the v0.3.3 release archives or build it from source. Verify the archive against its published `.sha256` sidecar before running it. Plugin installation neither downloads nor upgrades the CLI. The plugin does not update itself; use the host's plugin update mechanism.
- The release archives are built by this repository's public `Release` workflow with build-provenance attestation. The plugin package contains no executable; the optional standalone installer fetches only the official v0.3.3 release asset, verifies its published checksum, and writes it to an explicit directory chosen by the operator. It never edits `PATH`.
- For public MCP work, no separate Chrome or OpenCLI setup is needed. The v0.3.3 MCP server uses HTTP backends (`jina` and `exa` for reads, `exa` for search), even if optional CLI renderers are installed. Browser-assisted work is a separate host tool call; Arana does not receive browser sessions or credentials.
- `arana mcp` starts as the local host user, with that user's permissions. The only tools are `list_capabilities` (local), `read_url`, `read_many`, and `search` (public web actions). There are no tool calls for arbitrary commands, account login, browser management, setup, or tool installation. No credentials are required for the public MCP workflow. The broader CLI catalog has separate optional tools and credential-gated actions, outside the MCP interface.
- `read_many` accepts 1–5 public URLs, validates the batch before fetching, and shares one deadline and result budget. A malformed URL rejects the batch. A source failure is reported per source; the batch itself can still be `ok: true`.
- A network request can send the supplied URL or search query to the selected backend. Do not submit secrets or private URLs. Returned web text is untrusted content. Local state and safety boundaries are documented in [Security design](../docs/security.md).
- The plugin package is reviewed for Hermes and Claude Code. This package does not provide a remotely hosted connector for claude.ai or Cowork.

## Reviewer reproduction

From a clean source snapshot, run `hermes plugins validate . --install-deps`,
`hermes plugins doctor . --ci`, and `claude plugin validate --strict .`. Run
`cargo test -p arana --test mcp` for the stdio framing, tool surface, bounds, and
structured-error suite. Keep build outputs and private state outside the scanned
tree. See the [agent integration guide](../docs/agent-plugins.md) for install and
MCP checks; check install, skill discovery, and an MCP tool call separately.

`hermes mcp test arana` is not sufficient for this package: that command reads
native MCP config and does not resolve a server declared by portable `mcp.json`.
For a no-model runtime check, install the plugin in an isolated `HERMES_HOME`,
then run `.github/scripts/verify_hermes_runtime.py` from a Hermes v0.23.3
environment with the MCP extra. It uses that version's plugin and MCP runtime
APIs to assert the qualified skill, all four tools, read-only metadata, and a
real `read_url` dispatch. CI pins the same Hermes commit and runs this script
after building the binary under test (`Plugin validation`, `hermes-runtime` job).

The package version is `0.3.3`. `plugin.json`, the Claude and Antigravity
manifests, the Claude marketplace entry, and `skills/arana/SKILL.md` must agree
with the tag; packaging fails when they drift. `assets/arana-banner.png` is a
1200×600 catalog image pinned to the reviewed commit.

For a tagged build with GitHub provenance, verify the archive against the
reviewed commit (use the peeled commit SHA, not the annotated tag object):

```sh
gh attestation verify ./arana-linux-x64.zip --repo mevarx/Arana --source-digest REVIEWED_COMMIT_SHA
```

A local build or an older release is not covered just because this verification
command is documented. Verify the exact published artifact before submitting a
catalog pin.
