# Arana plugin review notes

Arana uses the portable Agent Plugins format for Hermes and a Claude Code
manifest and marketplace for Claude Code. Both load the same `arana` skill and
can start the same local `arana mcp` stdio server. No native Hermes code, hooks,
background service, desktop extension, or cloud connector is installed.

## Prerequisites and scope

- Install the matching Arana CLI separately from the project's release archives
  or build it from source. Verify the published SHA-256 checksum before running
  an archive. Plugin installation neither downloads nor upgrades the CLI. The
  plugin does not update itself; use the host's plugin update mechanism.
- `arana mcp` starts as the local host user, with that user's permissions. The
  only tools are `list_capabilities` (local), `read_url`, and `search` (public web
  actions). There are no tool calls for arbitrary commands, account login,
  browser management, setup, or tool installation. No credentials are required
  for the public MCP workflow. The broader CLI catalog has separate optional
  tools and credential-gated actions, outside the MCP interface.
- A network request can send the supplied URL or search query to the selected
  backend. Do not submit secrets or private URLs. Returned web text is untrusted
  content. Local state and safety boundaries are documented in
  [Security design](../docs/security.md).
- The local MCP workflow is for Hermes and Claude Code. This package does not
  provide a remotely hosted connector for claude.ai or Cowork.

## Reviewer reproduction

From a clean source checkout, run `hermes plugins validate . --install-deps`,
`hermes plugins doctor . --ci`, and `claude plugin validate --strict .`. Keep
build outputs and private state outside the scanned tree. See the
[agent integration guide](../docs/agent-plugins.md) for install and MCP checks;
check install, skill discovery, and an MCP tool call separately.

For a tagged build with GitHub provenance, verify the archive against the
reviewed commit (use the peeled commit SHA, not the annotated tag object):

```sh
gh attestation verify ./arana-linux-x64.zip --repo mevarx/Arana --source-digest REVIEWED_COMMIT_SHA
```

A local build or an older release is not covered just because this verification
command is documented. Verify the exact published artifact before submitting a
catalog pin.
