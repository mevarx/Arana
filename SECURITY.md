# Security policy

## Scope

Report vulnerabilities in Arana's CLI, MCP server, plugin distribution, release artifacts, or handling of local state and credentials. Arana is pre-release; include the exact build or commit affected rather than assuming the latest source and the latest release are identical.

## Report a vulnerability privately

Use the [maintainer's contact page](https://www.mevarx.xyz/contact) and include **Arana security report** in the subject. Do not open a public GitHub issue for a suspected vulnerability. For ordinary bugs and feature requests, use [GitHub Issues](https://github.com/mevarx/Arana/issues).

Include:

- The output of `arana version` or the exact commit and build source, plus the OS and installation method.
- The affected command, feature, or MCP host and version.
- A description of the impact and the conditions needed to reproduce it.
- Minimal reproduction steps and redacted logs or output, if available.

Do not send passwords, API tokens, cookies, browser profiles, or unredacted state files. If more evidence is needed, arrange a private way to share it with the maintainer. Please coordinate public disclosure before publishing details.

For trust boundaries and implementation safeguards, see [Security design](docs/security.md).
