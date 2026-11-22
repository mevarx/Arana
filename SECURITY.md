# Security policy

## Supported versions

Security fixes are prepared for the latest regular release line. The intended supported line for this documentation is **0.3.x** once it is published. Older releases are supported on a best-effort basis; a fix may land only in the current line.

Run `arana version` and include the reported version, source signature, OS, and installation method in any report. The latest source and the latest published archive are not automatically the same build.

## Report a vulnerability privately

Use GitHub private vulnerability reporting:

[Report a vulnerability on GitHub](https://github.com/mevarx/Arana/security/advisories/new)

Do not open a public GitHub issue, discussion, or pull request for a suspected vulnerability. If GitHub private reporting is unavailable to you, use the [maintainer's contact page](https://www.mevarx.xyz/contact) with **Arana security report** in the subject and say that GitHub reporting was unavailable. Ordinary bugs and feature requests belong in [GitHub Issues](https://github.com/mevarx/Arana/issues).

Include:

- The output of `arana version` (or the exact commit and build source), plus the OS and installation method.
- The affected command, MCP tool, plugin manifest, installer path, or release artifact.
- The impact and the conditions needed to reproduce it.
- Minimal reproduction steps and redacted logs or output, if available.

Do not send passwords, API tokens, cookies, browser profiles, or unredacted state files. If more evidence is needed, the maintainer will propose a private way to share it. Please allow time to investigate and coordinate disclosure before publishing details.

## In scope

- The `arana` CLI, the `agent-reach` alias, and the local `arana mcp` stdio server.
- The shared skill, host manifests, and MCP configuration files in this repository and in release archives.
- Release packaging, SHA-256 sidecars, the installer script, and update or uninstall behaviour.
- Handling of local state under `~/.arana/`, the dedicated browser profile, sessions, and credentials.
- Request guards: URL validation, SSRF and redirect checks, response bounds, and process execution.

## Out of scope

- Vulnerabilities in the third-party sites, APIs, or tools that Arana calls. Report those to the operator of that service.
- A compromised host account, malicious local administrator, or a browser extension installed outside Arana's dedicated profile.
- Denial of service against third-party services, or automated scanner output with no demonstrated impact on Arana.
- Social engineering, and reports that require access to another person's account or data.

Test only against accounts and systems you are authorised to use. Do not degrade a third-party service, access other users' data, or bypass a platform's access controls while researching.

## Disclosure

The maintainer will confirm receipt through the private advisory, investigate, and coordinate a fix and disclosure with you. Timelines depend on severity and on upstream services; no bounty or fixed response time is promised.

For trust boundaries, ownership rules, and implementation safeguards, see [Security design](docs/security.md). For what the automated gates do and do not prove, see [Verification](docs/verification.md).
