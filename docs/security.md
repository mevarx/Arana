# Security

arana reads the web on behalf of an agent, so it treats every page, CLI and
manifest as untrusted input, keeps its state private, and deletes only what it
can prove it created.

## Credentials

- arana never asks for, types or transmits a password. Login happens in the
  dedicated browser window, performed by the human.
- It never reads a personal browser's cookie database. Cookies move only
  through CDP `Storage.getCookies` / `setCookies` in the arana-owned profile,
  with expiry-based deletion on logout.
- Secrets written by `configure` are not echoed, not logged and excluded from
  envelopes. Error messages are scrubbed of credential-bearing URLs and
  bounded in length.
- Session credentials live in private fake HOMEs under
  `~/.arana/sessions/<tool>/`. If a credentials file is unreadable or
  malformed, the login state is **unknown**, never "logged out" — the CLI does
  not invent a logout to explain a read failure.

## Private state

| Property | Unix | Windows |
|---|---|---|
| Directories | 0700 | protected DACL owned by the process-token SID |
| Files | 0600 | same |
| Writes | atomic temp + rename | same |
| Mutation | exclusive OS file lock | same |
| Paths | symlink-component refusal | symlink **and** reparse-point (junction) refusal |

The Windows ACL identity is taken from the process token, not from `USERNAME`;
a child process that lies about its username still cannot change the owner of
private state (regression-tested by inspecting the saved descriptor).

## Outbound requests

- URLs are validated as http(s) with a parsed host. Ambiguous authorities
  (userinfo, percent-encoded hosts) are rejected, and the original URL spelling
  is preserved in output.
- SSRF checks cover literal-IP shorthand (`127.1`, decimal/octal), DNS
  resolution and **every redirect hop**, not just the first URL. Private,
  loopback, link-local and reserved targets are refused.
- Response bodies are capped while streaming, not after allocation, so a
  hostile `Content-Length` cannot exhaust memory.
- obscura's own private-network protection is never disabled for user URLs;
  `--allow-private-network` is never passed.

## Process execution

- Children are spawned hidden on Windows (`CREATE_NO_WINDOW`), in their own
  process group on Unix, with stdin from null and bounded stdout/stderr.
- Timeouts and cancellation kill the whole process tree (`taskkill /T /F`,
  negative-pid kill); dropping the future still cleans up.
- No shell is used for arguments, and `.cmd`/`.bat` launchers on Windows are
  invoked as `cmd /C call <launcher>` with metacharacter refusal.
- Installer commands are validated against the private prefix; a command that
  is not part of the registry is not executed.

## Archives and downloads

- Downloads are size-capped, streamed, sha256-verified against a pinned digest,
  and reused from a sha256-addressed cache only while the pin still matches.
- Zip and tar.gz extraction is **two-pass**: names, traversal, absolute paths,
  drive letters, links, devices, entry count, declared size, actual size, total
  size and decompressed bound are validated before a single byte is written.
- The strict extractor refuses all link members. The one exception is the
  private Node installer, which uses a separate regular-members extractor that
  **skips** link entries entirely (it never creates them) and applies the same
  validation first. The strict path stays strict and is regression-tested.
- Extraction targets must be fresh, empty, guarded directories.

## Ownership and removal

- `state.json` records `owner`, `scope`, `kind`, version and hash per path.
  Anything unrecorded is not ours: it is listed and left alone.
- Removal recomputes the canonical path from the registry, so a forged ledger
  row cannot redirect a delete.
- Each install writes `.arana-manifest.json` listing the files it produced.
  Removal deletes only those files, prunes directories that became empty, and
  reports everything else as `left_untouched`. A file a user adds later inside
  an owned tree survives.
- An install snapshot marks files that predate the install as unowned, so a
  repair cannot adopt them; an identical pre-existing file is never claimed
  because it happens to hash-match ours.
- `uninstall` previews by default and deletes only hash-matching owned files;
  a changed config, a forged skill record or a linked ancestor is preserved.
- A whole-directory delete is used only for an arana-created session HOME
  (deleting it is what revokes credentials) and for manifest-less legacy trees,
  and both cases are documented in the code and reported in the result.

## Browser isolation

- The bus uses a dedicated profile (`~/.arana/browser/profile`) and refuses to
  adopt or stop a browser it did not start.
- Selecting a cookie source must be explicit; the pipeline never silently
  copies from the user's browser.
- The OpenCLI extension is installed from one pinned URL with a pinned sha256,
  validated against its own manifest, and ledgered per file so it can be
  removed cleanly.

## Local MCP boundary

- `arana mcp` is a local stdio server, not a hosted service or a sandbox. It
  runs under the account and permissions of the process that launches it.
- It exposes only `list_capabilities`, `read_url`, and `search`. The two network
  tools are restricted to public web actions; there are no tool calls for
  login, setup, browser management, arbitrary commands, or backend overrides.
- The server bounds request frames, tool execution time, and result size.
  Read-only refers to behavior on external platforms, not to a promise that
  the process never writes local operational state or sends data outbound.
- Queries and URLs may be sent to the backend that serves the request. Do not
  pass secrets in them. Treat fetched text as data, not instructions for the
  host agent. Do not put credentials in plugin manifests.

## Reporting

See the [security policy](../SECURITY.md) for private reporting instructions.
