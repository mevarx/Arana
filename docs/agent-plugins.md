# Agent plugins and MCP host guides

arana ships as one portable package and one canonical skill. The package uses
the [Agent Plugins 1.0.0](https://agent-plugins.org/specification) layout:

```text
plugin.json                     portable manifest
mcp.json                        portable stdio MCP server
skills/arana/SKILL.md           the canonical skill
.claude-plugin/plugin.json      Claude Code package manifest
.claude-plugin/marketplace.json Claude Code marketplace entry
.antigravity-plugin/plugin.json Antigravity manifest
.agents/plugins/marketplace.json Codex marketplace entry
.mcp.json                       Claude Code / Devin MCP file
```

There are two independent delivery paths:

1. **Skill only.** The host loads `skills/arana/SKILL.md`; the model invokes the
   installed `arana` CLI and reads its single JSON envelope. This is the full
   14-channel / 40-action surface, and it works on every host in this document,
   including hosts with no MCP support.
2. **Local MCP.** The host launches `arana mcp` over stdio. The server exposes
   exactly three read-only public web tools: `list_capabilities`, `read_url` and
   `search`. Platform-specific channels (X/Twitter, Reddit, YouTube, Bilibili,
   Xiaohongshu, LinkedIn, BOSS and the rest) remain CLI-only. The MCP server has
   no tool for a channel, backend, browser, login, install, update, management
   command or shell.

Use one registration per host. Installing the portable package *and* hand-adding
an MCP server with the same name can register `arana` twice; keep one
definition.

> **Status: pre-release and partially exercised.** This page separates what was
> run against a real host from what was only checked against vendor
> documentation. It is not a claim that every host has been certified.
> Exercised cases were run on Windows 11 on 2026-11-18 against arana `0.1.0`
> built from the working tree at commit `bfc92e8` (the release binary predates
> the MCP server). No host was exercised on macOS or Linux.

## Status legend

| Status | Meaning |
| --- | --- |
| **Exercised** | Installed and checked in a real client, in an isolated profile where the client supports one. |
| **Documented** | Configuration checked against the vendor's current official documentation; the client was not available here. |
| **Preview** | The vendor labels the surface a developer preview; support is experimental and version-pinned. |
| **Blocked** | The surface cannot be exercised in this environment, or requires a vendor-side remote environment. |

The version in an **Exercised** row is the build that was run, not a claimed
minimum. No minimum version is established for hosts that were only checked
against documentation.

## Host matrix

| Host | Skill / package delivery | MCP delivery | Evidence |
| --- | --- | --- | --- |
| [Hermes Agent](#hermes-agent) | Portable `plugin.json`; install from Git and enable | Bundled `mcp.json`, or `mcp_servers` in `config.yaml` | **Exercised** package validate/install/enable (v0.23.1, isolated home); MCP through Hermes not exercised |
| [OpenAI Codex](#openai-codex) | Root `plugin.json`; `.agents/plugins/marketplace.json`; standalone `~/.agents/skills` | `[mcp_servers.arana]` in `config.toml` | **Documented**; Codex CLI not installed here |
| [Claude Code](#claude-code) | `.claude-plugin/` package and marketplace | Bundled `.mcp.json` | **Exercised** install and MCP connection (v2.1.325, isolated `CLAUDE_CONFIG_DIR`) |
| [Google Antigravity](#google-antigravity) | `.antigravity-plugin/plugin.json` + `skills/` | `mcp_config.json` (`enabledTools` allowlist) | **Documented**; `agy` not installed; package does not yet ship `mcp_config.json` |
| [Cursor](#cursor) | Agent Plugins root `plugin.json` + `skills/` | Bundled `mcp.json`, or `~/.cursor/mcp.json` | **Documented**; Cursor not installed here |
| [Devin](#devin) | Agent Plugins fallback; `devin plugins install` | Plugin `mcp.json` / `.mcp.json`; CLI `mcp_config.json` | **Documented** for CLI; **Blocked** for cloud stdio (binary must exist in Devin's environment) |
| [OpenCode](#opencode) | Shared skill directory | `mcp.servers` in `opencode.json` | **Documented**; OpenCode not installed here |
| [Command Code](#command-code) | `~/.commandcode/skills` | `cmd mcp` / `~/.commandcode/mcp.json` | **Documented**; pinned client not available here |
| [Oh My Pi (OMP)](#oh-my-pi-omp) | `~/.omp/agent/skills` | `~/.omp/agent/mcp.json` | **Documented**; OMP v18.22.0 is installed but no MCP/skill session was exercised |
| [DeepSeek Harness](#deepseek-harness-preview) | `.dsh/skills`, `.agents/skills`, user skills root | Cordis `@deepseek-ai/dsh-mcp-client` plugin | **Preview**; not exercised; pinned preview build required |
| [Pi](#pi) | Agent Skills directory; Pi package pending | Built-in `pi mcp` / `~/.pi/agent/mcp.json` | **Documented**; Pi not installed here |

The canonical skill is the same file everywhere: `skills/arana/SKILL.md`.
Package copies must not edit it per host.

## Before installing on any host

The skill and MCP server do not download, install or update arana. Install the
binary first, from the [release page](https://github.com/mevarx/Arana/releases):

1. Download the archive for your OS/architecture and the matching
   `SHA256SUMS`.
2. Verify the archive checksum before extracting:

   ```sh
   # Linux
   sha256sum -c SHA256SUMS --ignore-missing
   # macOS
   shasum -a 256 -c SHA256SUMS
   ```

   On Windows, compare the output of
   `Get-FileHash .\arana-<version>-<target>.zip -Algorithm SHA256` with the
   entry in `SHA256SUMS`.
3. Put `arana` (or `arana.exe`) on `PATH`, or note its absolute path for the
   host configuration. GUI-launched hosts often do not inherit a shell `PATH`.
4. Verify the installed binary:

   ```sh
   arana version
   arana doctor
   arana read https://example.com
   ```

`arana version` returns the JSON envelope used by every command. `arana doctor`
reports tool, login and channel health; `arana read` must return `"ok": true`
and the fetched text before you wire anything into a host.

The skill performs the same check when it loads: it runs `arana version`, stops
if the binary is missing or older than the skill's `metadata.version`, and asks
the user to install the matching release. It never downloads, installs or
updates anything itself. For a host that expects a skills directory, the CLI can
place a recorded copy with `arana skill link <skills-dir>` and remove only that
recorded copy with `arana skill unlink <skills-dir>`.

No API key, token, cookie or password belongs in a plugin manifest or MCP
configuration. Public reads need no credentials. arana keeps its own state in
`~/.arana` (override only with `ARANA_HOME` for tests); never point a host at a
shared or world-writable state directory.

## Hermes Agent

**Official docs:** [Plugins](https://hermes-agent.nousresearch.com/docs/user-guide/features/plugins)
· [Portable Agent Plugins v1](https://hermes-agent.nousresearch.com/docs/developer-guide/plugins#portable-agent-plugins-v1-packages)
· [MCP](https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp)

Hermes installs portable Agent Plugins packages disabled. An enabled package
loads `skills/*/SKILL.md` read-only, namespaced as `arana:arana`, and stdio
servers from root `mcp.json`. MCP commands are one executable plus a separate
argument list, never a shell.

### Install

```sh
hermes plugins install mevarx/Arana --no-enable
hermes plugins list
hermes plugins enable arana
```

Restart the gateway after enabling (`hermes gateway restart`). To remove it:

```sh
hermes plugins disable arana
hermes plugins remove arana
```

### MCP configuration

The package already contains `mcp.json`; enabling the package is enough. If you
prefer a user-level server without the plugin, add this to
`~/.hermes/config.yaml` instead of installing the package twice:

```yaml
mcp_servers:
  arana:
    command: "arana"
    args: ["mcp"]
    tools:
      include: ["list_capabilities", "read_url", "search"]
```

Use an absolute command path if the Hermes process cannot see your shell
`PATH`. Do not add a manual `arana` entry while the portable package is enabled:
a duplicate server name is a load-time conflict.

### Verify

Ask Hermes to use `list_capabilities` or `read_url` for a public page. The
server name in model-facing tool names is `mcp__arana__<tool>`. A missing
binary or an old arana build must produce instructions, not a silent fallback.

### Evidence and limits

- Hermes Agent v0.23.1 (2026.9.24), Windows 11, isolated `HERMES_HOME`:
  `hermes plugins validate` passed for the package; install from a local Git URL
  (the same package layout the GitHub repo serves) succeeded disabled;
  `hermes plugins enable arana` succeeded; `hermes plugins
  list --user --plain` reported `enabled git 0.1.0 arana`.
- MCP through Hermes was **not** exercised: the isolated Home has no model
  session. The server itself was exercised directly (see
  [Evidence summary](#evidence-summary-and-open-blockers)).
- Hermes project-local plugins under `./.hermes/plugins/` do not load in
  sessions; install at user level as above.

## OpenAI Codex

**Official docs:** [Package plugins](https://developers.openai.com/plugins/build/plugins)
· [Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp#configure-with-configtoml)
· [Codex skills](https://learn.chatgpt.com/docs/build-skills#where-codex-loads-local-skills)

Codex reads the portable root `plugin.json`, `skills/` and `mcp.json`. Plugins
are available in Codex in the ChatGPT desktop app and through the Codex CLI
plugin browser; the IDE extension does not support plugins. Do not assume a
public directory, cloud or ChatGPT web listing from a local install.

### Install

Add the repository marketplace, then install from Codex's `/plugins` browser
and start a new session:

```sh
codex plugin marketplace add mevarx/Arana
```

Skill-only alternative, with no plugin install:

```sh
mkdir -p ~/.agents/skills
cp -R /path/to/Arana/skills/arana ~/.agents/skills/arana
```

Codex scans `.agents/skills` from the current directory up to the repository
root, and `$HOME/.agents/skills` for the user. Restart Codex if a new skill does
not appear.

### MCP configuration

Codex stores MCP servers in `~/.codex/config.toml`, shared by the desktop app,
CLI and IDE extension. Add only this table:

```toml
[mcp_servers.arana]
command = "arana"
args = ["mcp"]
```

Use a quoted absolute path for `command` if Codex is launched from a GUI and
cannot resolve `arana`. Do not use a shell string; `command` is one executable.

### Verify

In Codex, open `/mcp` in the composer and confirm `arana` is connected, then
ask it to call `list_capabilities` or read `https://example.com`. A plugin
install requires a new session before its skill and MCP tools appear.

### Evidence and limits

- **Documented only.** No Codex CLI was installed in this environment.
- The IDE extension does not support plugins.
- Local/repo marketplaces are separate from the public plugin directory; no
  cloud or public-directory compatibility is claimed.

## Claude Code

**Official docs:** [Plugin distribution](https://code.claude.com/docs/en/plugins/publish)
· [Manifest reference](https://code.claude.com/docs/en/plugins/manifest-reference)
· [MCP](https://code.claude.com/docs/en/mcp)

Claude Code reads `.claude-plugin/plugin.json` plus the root `.mcp.json`.
Repository-root marketplaces need `.claude-plugin/marketplace.json`; the plugin
source is `./`.

### Install

From a Claude Code session:

```text
/plugin marketplace add mevarx/Arana
/plugin install arana@arana
```

Claude Code 2.1.275+ also accepts the validator's one-step form:

```text
/plugin install arana --marketplace mevarx/Arana
```

From a shell:

```sh
claude plugin marketplace add mevarx/Arana
claude plugin install arana@arana
claude plugin list
```

Choose user, project or local scope at install time. The plugin starts its
bundled MCP server when the plugin is enabled.

### MCP configuration

No separate configuration is needed; `.mcp.json` contains:

```json
{
  "mcpServers": {
    "arana": {
      "type": "stdio",
      "command": "arana",
      "args": ["mcp"]
    }
  }
}
```

Use an absolute `command` if Claude Code is launched from a GUI. If you do not
install the plugin, `claude mcp add --transport stdio arana -- arana mcp` is the
equivalent manual registration; do not keep both registrations.

### Verify

```sh
claude plugin list
claude mcp list
claude plugin details arana
```

The details inventory must show `Skills (1) arana` and `MCP servers (1) arana`.
`claude mcp list` must report `plugin:arana:arana: arana mcp - √ Connected`.
Then invoke `/arana:arana` or ask Claude to read a public page.

### Evidence and limits

- **Exercised:** Claude Code 2.1.325 on Windows 11, isolated
  `CLAUDE_CONFIG_DIR`, installing the repository package from a local
  marketplace. `claude plugin validate` passed; install reported version
  `0.1.0`, scope `user`, one skill and one MCP server; `claude mcp list` reported
  `√ Connected`. The exact `mevarx/Arana` GitHub source was not fetched; the
  local repository marketplace served the same `.claude-plugin` files.
- The validator reported one directory warning: no plugin icon. Marketplace
  submission also expects an icon and a README install line; neither is needed
  for a local install. No official-directory submission is claimed.
- `claude plugin details` does not support `--json` on 2.1.325.

Uninstall:

```sh
claude plugin uninstall arana@arana
claude plugin marketplace remove arana
```

## Google Antigravity

**Official docs:** [Plugins](https://antigravity.google/docs/plugins)
· [Build plugins](https://antigravity.google/docs/build-plugins)
· [MCP](https://antigravity.google/docs/mcp)

Antigravity accepts a root `plugin.json` or `.antigravity-plugin/plugin.json`;
skills live under `skills/` at the plugin root. MCP servers are declared in
`mcp_config.json`, not the portable `mcp.json`. The CLI validates with
`agy plugin validate --strict` and installs with `agy plugin install`.

### Install (skill / plugin)

```sh
git clone https://github.com/mevarx/Arana
cd Arana
agy plugin validate --strict .
agy plugin install .
agy plugin list
```

The package currently ships the Antigravity manifest and skill. It does **not**
yet ship `mcp_config.json`, so a plugin-only install gives you the skill but not
the MCP server.

### MCP configuration

For MCP, add the server to the plugin's root `mcp_config.json`, or to the
global/workspace file:

- global: `~/.gemini/config/mcp_config.json`
- workspace: `<workspace>/.agents/mcp_config.json`

```json
{
  "mcpServers": {
    "arana": {
      "command": "arana",
      "args": ["mcp"],
      "enabledTools": ["list_capabilities", "read_url", "search"],
      "timeoutSeconds": 60
    }
  }
}
```

`enabledTools` is Antigravity's allowlist: the three arana tools are the only
ones exposed. Keep `timeoutSeconds` above arana's 30-second tool deadline.
`command` is one executable; do not wrap it in a shell.

### Verify

Open `/mcp` in the CLI, or **Customizations → Installed → MCP Servers** in the
desktop app. Confirm the server is active and lists three tools, then ask for a
public read. The CLI and desktop app resolve binaries independently: verify
both before claiming both.

### Evidence and limits

- **Documented only.** `agy` was not installed here, so neither the CLI nor the
  desktop app was exercised.
- The current package has no `mcp_config.json`; the manual/global example is
  the documented shape, not a verified package install.
- Do not claim desktop and CLI support from a CLI-only test.

Uninstall with `agy plugin remove arana` if your build provides it; otherwise
remove the plugin from Antigravity's plugin manager. Remove the `mcp_config.json`
entry separately.

## Cursor

**Official docs:** [Plugins](https://cursor.com/docs/plugins)
· [Plugins reference](https://cursor.com/docs/reference/plugins)
· [MCP](https://cursor.com/docs/mcp)

Cursor loads Agent Plugins packages: root `plugin.json`, `skills/` and
`mcp.json`. Cursor does not expand the portable `${PLUGIN_ROOT}` and
`${PLUGIN_DATA}` variables in `mcp.json`; arana uses a bare executable name, so
no plugin-root variable is needed.

### Install

- Arana is not yet listed in the public Cursor Marketplace. If you distribute
  it from a team or repository marketplace, install it from **Customize** at
  project or user scope. For a local checkout, copy the package to
  `~/.cursor/plugins/local/arana`, restart Cursor or run **Developer: Reload
  Window**, and confirm the skill and MCP server under **Customize**.

Enterprise administrators can disable local plugin imports; that setting also
blocks **Import from Disk**.

### MCP configuration

With the plugin installed, `mcp.json` is used. For a manual install instead,
add to `~/.cursor/mcp.json` (global) or `.cursor/mcp.json` (project):

```json
{
  "mcpServers": {
    "arana": {
      "type": "stdio",
      "command": "arana",
      "args": ["mcp"]
    }
  }
}
```

Cursor resolves `${env:NAME}` variables in `command`, `args` and `env`; arana
needs none. Use an absolute command path if the Cursor app cannot see your shell
`PATH`.

### Verify

Open **Customize → Installed**, confirm the Arana skill and MCP server, then
ask Cursor to read a public page. The MCP server toggle must be on; disabled
servers do not load.

### Evidence and limits

- **Documented only.** Cursor was not installed here.
- Agent Plugins support is documented by Cursor, but the package was not
  exercised in the GUI.
- Do not claim `${PLUGIN_ROOT}` support in Cursor's Agent Plugins `mcp.json`;
  Cursor documents only `${CURSOR_PLUGIN_ROOT}` for Cursor Plugins.

Uninstall from **Customize → Installed**, or delete
`~/.cursor/plugins/local/arana` for a local copy.

## Devin

**Official docs:** [CLI plugins](https://docs.devin.ai/cli/extensibility/plugins/overview)
· [CLI MCP configuration](https://docs.devin.ai/cli/extensibility/mcp/configuration)
· [Devin cloud MCP](https://docs.devin.ai/work-with-devin/mcp)

Devin loads its own `.devin-plugin/plugin.json`, then
`.claude-plugin/plugin.json`, then a root `plugin.json`. The portable Agent
Plugins layout is supported: root `mcp.json`, skills under `skills/`, and
`${PLUGIN_ROOT}` expansion. Root `.mcp.json` also exists in this package and
wins on a server-name collision.

### Install

```sh
devin plugins install mevarx/Arana
devin plugins list
```

For an uncommitted local checkout, use `--local` so the install is linked on
this machine only and is not recorded in your Devin Cloud personal manifest:

```sh
devin plugins install --local ./Arana
```

Installing a plugin normally records it in your personal manifest and syncs it
to your other machines and cloud sessions; use `--local` for testing. Plugin
management requires `devin auth login`; an enterprise can disable CLI plugins.

### MCP configuration

The plugin's `mcp.json` starts with a CLI session. Manual CLI registration:

```sh
devin mcp add -s user arana -- arana mcp
devin mcp list
```

Or edit `~/.config/devin/mcp_config.json` (Windows:
`%APPDATA%\devin\mcp_config.json`):

```json
{
  "mcpServers": {
    "arana": {
      "command": "arana",
      "args": ["mcp"]
    }
  }
}
```

Project scope uses `.devin/mcp_config.json`; the default is
`.devin/mcp_config.local.json`, which is gitignored. Keep personal binaries and
paths in the user/local file, not in a committed project file.

### Verify

`devin plugins list` should show the plugin and its skill; the skill is invoked
as `/arana:arana`. `devin mcp list` should show `arana`; ask Devin to call
`list_capabilities` or read a public page.

### Cloud sessions and remote deployment — blocked

A Devin cloud session runs in Devin's execution environment, not on your
machine. Installing the plugin does not install the arana binary there. A stdio
server therefore fails unless arana is installed and on `PATH` inside that
environment.

- Do not present a local plugin install as cloud support.
- Plugin MCPs used by cloud sessions are connected through the web app's
  **Customize → MCPs** flow, with their own connection status and credentials.
- Hosting arana remotely, or adding a remote HTTP transport, is separate scope
  and is not implemented.

### Evidence and limits

- **Documented only** for the CLI; Devin was not installed here.
- **Blocked** for cloud stdio: the binary is not present in Devin's environment.
- The plugin MCP config must not carry a client secret; Devin rejects one at
  activation. Arana needs no secret at all.

## OpenCode

**Official docs:** [Skills](https://opencode.ai/docs/skills/)
· [MCP servers](https://opencode.ai/docs/mcp-servers/)

OpenCode discovers `SKILL.md` directories under `~/.config/opencode/skills`,
`.opencode/skills`, and the compatibility locations `~/.claude/skills`,
`~/.agents/skills`, `.claude/skills` and `.agents/skills`. MCP configuration
lives in `opencode.json` / `opencode.jsonc` under `mcp.servers` in V2.

### Install the skill

```sh
mkdir -p ~/.config/opencode/skills
cp -R /path/to/Arana/skills/arana ~/.config/opencode/skills/arana
```

Project scope: copy it to `.opencode/skills/arana` instead. OpenCode reads the
skill ID from the directory (`arana`) and loads the body on demand.

### MCP configuration

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "arana": {
        "type": "local",
        "command": ["arana", "mcp"]
      }
    }
  }
}
```

Or use the CLI (global scope):

```sh
opencode mcp add arana --global -- arana mcp
opencode mcp list
```

`command` is an argv array, not a shell string. If OpenCode runs from a GUI and
cannot resolve `arana`, put the absolute executable path in the first array
element.

### Verify

`opencode mcp list` should report the server connected (or show
`needs authentication` only for OAuth servers, which arana is not). Inside a
session, `/mcps` shows the server and its tools; `@arana` mentions the skill.

### Evidence and limits

- **Documented only.** OpenCode was not installed here.
- V2 uses `mcp.servers`; older versions used a different shape. Pin the client
  version and do not paste a V1 config into V2.
- The skill is the supported delivery; no JavaScript wrapper is needed.

Remove the server from the config and delete
`~/.config/opencode/skills/arana` to uninstall.

## Command Code

**Official docs:** [Skills](https://commandcode.ai/docs/skills)
· [MCP](https://commandcode.ai/docs/mcp)
· [Settings and config](https://commandcode.ai/docs/settings)

Command Code implements the Agent Skills standard and reads
`~/.commandcode/skills` (user), `.commandcode/skills` (project),
`~/.agents/skills` and `.agents/skills`. MCP servers live in
`~/.commandcode/mcp.json` (user) or `.mcp.json` (project).

### Install the skill

```sh
mkdir -p ~/.commandcode/skills
cp -R /path/to/Arana/skills/arana ~/.commandcode/skills/arana
```

Project scope: `.commandcode/skills/arana` or `.agents/skills/arana`.
`.commandcode/skills` wins on name conflicts.

### MCP configuration

```sh
cmd mcp add --transport stdio arana -- arana mcp
cmd mcp list
cmd mcp get arana
```

Or write `~/.commandcode/mcp.json`:

```json
{
  "mcpServers": {
    "arana": {
      "command": "arana",
      "args": ["mcp"]
    }
  }
}
```

All `cmd mcp add` options (`--transport`, `--env`, `--scope`, `--header`) come
**before** the server name; `--` separates the name from the stdio command.

### Verify

`cmd mcp list` should show `arana`; `/mcp` inside a session shows connection
status and tools. Ask Command Code to read a public page.

### Evidence and limits

- **Documented only.** No pinned Command Code client was available here.
- Confirm skill discovery and install scope on a clean, pinned client before
  advertising a version; do not generalize from another agent's layout.

Remove with `cmd mcp remove arana` and delete the skill directory.

## Oh My Pi (OMP)

**Official docs:** [Skills](https://omp.sh/docs/skills)
· [MCP](https://omp.sh/docs/mcp)

OMP discovers native skills at `~/.omp/agent/skills/<name>/SKILL.md` and
`.omp/skills/<name>/SKILL.md`, plus the Claude/Codex/Agents/OpenCode
compatibility directories. MCP servers use `~/.omp/agent/mcp.json` (user),
`.omp/mcp.json` (project), or
`~/.omp/profiles/<name>/agent/mcp.json` (named profile). OMP also imports
supported definitions from Claude Code, Codex, Gemini CLI, OpenCode, Cursor,
Windsurf, VS Code, plugins and extension packages.

### Install the skill

```sh
mkdir -p ~/.omp/agent/skills
cp -R /path/to/Arana/skills/arana ~/.omp/agent/skills/arana
```

Project scope: `.omp/skills/arana`. Run `/extensions` to confirm discovery and
`/reload-plugins` after copying.

### MCP configuration

```json
{
  "$schema": "https://raw.githubusercontent.com/can1357/oh-my-pi/main/packages/coding-agent/src/config/mcp-schema.json",
  "mcpServers": {
    "arana": {
      "type": "stdio",
      "command": "arana",
      "args": ["mcp"]
    }
  }
}
```

Save as `~/.omp/agent/mcp.json` for the user scope, or `.omp/mcp.json` for a
project. Use `/mcp reload` after editing.

### Verify

```text
/mcp list
/mcp test arana
/tools
```

`/mcp test` reports the server, version, tool count and up to ten tool names.
MCP tool names appear as `mcp__<server>_<tool>` after normalization. OMP's
importer can discover an `arana` entry from another client as well: if
`/mcp list` shows more than one `arana`, disable the imported duplicate with
`/mcp disable` rather than adding another config.

### Evidence and limits

- **Documented only for this task.** OMP v18.22.0 is installed on this Windows
  machine, but no MCP or skill session was exercised.
- The importer makes duplicate registration a real risk; verify the source
  column in `/mcp list` and keep one definition.

Remove with `/mcp remove arana --scope user` (or project), and delete the skill
directory.

## DeepSeek Harness (preview)

**Official docs:** [Skills subsystem](https://deepseek-harness.github.io/deepseek-harness/en/reference/subsystems/skills)
· [DSH product page](https://www.deepseek.com/harness/en/)
· [MCP (community reference)](https://deepseekdocs.com/en/docs/features/mcp)

DeepSeek Harness is in public preview and its plugin API is still evolving. Pin
a developer-preview build; do not treat this section as support for every DSH
release.

The shipped local skill provider scans, in rank order:

| Rank | Source | Root |
| --- | --- | --- |
| 100 | project DSH | `<projectRoot>/.dsh/skills` |
| 200 | project agents | `<projectRoot>/.agents/skills` |
| 300 | custom | `Config.customSkillDirs` |
| 400 | user DSH | `<dshHome>/skills` |
| 500 | user agents | `<agentsHome>/skills` |

MCP is a Cordis plugin (`@deepseek-ai/dsh-mcp-client`), one plugin instance per
MCP server, configured in the active `cordis.yml` or patch. It does not read
`mcp.json`.

### Install the skill

```sh
mkdir -p .dsh/skills
cp -R /path/to/Arana/skills/arana .dsh/skills/arana
```

For a user install, copy to `<dshHome>/skills/arana` (default `~/.dsh/skills`)
or to `~/.agents/skills/arana`. Use `Config.customSkillDirs` to point at a
shared checkout.

### MCP configuration (preview)

```yaml
- id: mcp-arana
  name: '@deepseek-ai/dsh-mcp-client'
  config:
    serverName: arana
    transport: stdio
    command: arana
    args: ['mcp']
    toolCallTimeoutMs: 45000
```

This is the documented DSH client shape, not a shipped arana adapter. Keep the
`serverName` unique and the tool timeout above arana's 30-second deadline.
Do not put secrets in this file.

### Verify

```sh
dsh web --dump-config | grep -A3 mcp
```

Then run one public read and check the session log for `mcp__arana__` tool
names. The official docs describe this as the MCP client verification path.

### Evidence and limits

- **Preview, not exercised.** No DSH build was installed here; shared-skill
  discovery in Standard mode and the Cordis stdio behavior are unverified.
- Only tools are bridged; resources and prompts have no DSH consumer.
- A thin Cordis adapter is not shipped and is not needed for the documented
  config above.

Remove the YAML entry and the skill directory to uninstall.

## Pi

**Official docs:** [Skills](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/skills.md)
· [Pi Packages](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/packages.md)
· [MCP](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/mcp.md)

Pi implements the Agent Skills specification and reads the portable
`~/.agents/skills` and `.agents/skills` locations. Current Pi builds also ship a
built-in MCP extension: servers are managed with `pi mcp` and stored in
`~/.pi/agent/mcp.json` (user) or `.pi/mcp.json` (project, after project trust).

### Install the skill

```sh
mkdir -p ~/.agents/skills
cp -R /path/to/Arana/skills/arana ~/.agents/skills/arana
```

Project scope: `.agents/skills/arana`. Invoke it as `/skill:arana` or let Pi
match the description.

Pi package distribution (the plan's `pi install git:...` path) is **not
shipped**: this repository does not yet have a `package.json` with a `pi`
manifest. Use the skill copy until that package exists.

### MCP configuration

```sh
pi mcp add arana -- arana mcp
pi mcp list
```

Or `~/.pi/agent/mcp.json`:

```json
{
  "mcpServers": {
    "arana": {
      "command": "arana",
      "args": ["mcp"],
      "timeout": 45
    }
  }
}
```

Pi's stdio `command` is one executable and `args` is its argument list; it is
not a shell string. Project config is read only after project trust is granted.

### Verify

Use `/mcp` inside Pi to inspect the connection and tool exposure, then ask Pi to
read a public page. Pi names tools `mcp__<server>__<tool>`.

### Evidence and limits

- **Documented only.** Pi was not installed here.
- Built-in MCP is documented in current Pi docs; this supersedes older planning
  notes that described MCP as adapter-only. It was not exercised in this task.
- No Pi package or extension is shipped or recommended; a third-party MCP
  adapter is unnecessary for current builds.

Remove the server with `/mcp` or by editing `~/.pi/agent/mcp.json`; delete the
skill directory to uninstall.

## Safe configuration rules

These rules apply to every host above.

### Credentials

- arana public reads need no API key. Do not add `env`, `headers`, `envFile` or
  `userConfig` secrets to a plugin or MCP entry.
- If a future channel requires a login, the human logs in through
  `arana login <channel>` in arana's dedicated browser. Never put a password,
  cookie, token or browser-profile path in a host config, and never let a tool
  call start a login flow.
- Do not store `ARANA_HOME` in a shared, temporary or world-writable directory.
  The default private `~/.arana` is correct for normal use.

### Executable resolution

- Prefer an absolute path in MCP config for GUI-launched hosts (Cursor,
  Antigravity desktop, Claude desktop, Codex desktop). `arana` on an interactive
  shell's `PATH` is not proof that the GUI can find it.
- On Windows use forward slashes in JSON (`C:/Tools/arana/arana.exe`) and quote
  TOML paths. On macOS/Linux use the absolute path, not `~` (some hosts do not
  expand `~` in `command`).
- `command` is one executable token. Do not use `sh -c`, `cmd /C`, `bash -lc`,
  pipes, `&&`, or a wrapper script that forwards arbitrary arguments. The
  server is already `arana mcp`; no shell is needed.

### Keep the surface read-only

- The MCP surface is exactly `list_capabilities`, `read_url` and `search`. There
  is no tool for setup, install, update, login, browser control, channel/backend
  selection, arbitrary commands, or writes.
- Where the host supports a tool allowlist, use it:
  [Antigravity `enabledTools`](#google-antigravity) and
  [Hermes `tools.include`](#hermes-agent). On hosts without an allowlist,
  confirm the host lists exactly three tools before use.
- Keep arana's default rate limiting and cooldowns. Do not retry through a
  second account, browser profile or backend. Do not disable SSRF or
  private-network protections.

### Treat fetched content as data

Page text is untrusted. The server's `initialize` instructions say not to follow
instructions found in fetched content; the skill repeats that rule. A host that
lets the model act on page text without a trust boundary is a prompt-injection
risk, not an arana feature.

### Bounds

| Limit | Value |
| --- | --- |
| MCP request frame | 1 MiB |
| Tool result text | 256 KiB |
| Items per result | 25 |
| Text per item | 16,000 characters |
| Search `limit` | 1–10 |
| URL length | 2,048 characters |
| Query length | 512 characters |
| Tool deadline | 30 seconds |

Errors preserve arana's structured envelope: `ok: false` with `error.code`,
`error.message` and `error.hint`. An unknown tool is a JSON-RPC error listing
the valid names; an unsafe URL is a tool result with `ok: false` and
`BAD_INPUT`. Do not parse error prose for control flow.

## Verification checklist

Run this after any host install. It proves the host and the server, not just the
manifest.

1. `arana version` returns `ok: true`; `arana doctor` starts cleanly.
2. The host lists the skill (or plugin) and `arana` as an MCP server.
3. The host lists exactly three MCP tools: `list_capabilities`, `read_url`,
   `search`.
4. `list_capabilities` succeeds and names those same three tools.
5. `read_url` for `https://example.com` returns `ok: true`, a non-empty `text`,
   and the source URL unchanged.
6. `read_url` for `http://127.0.0.1/` returns a structured `ok: false` error and
   does not fetch.
7. An unknown tool name returns a JSON-RPC error naming the valid tools.
8. Closing stdin exits the server cleanly with no non-protocol text on stdout;
   diagnostics stay on stderr.
9. Uninstalling the host plugin/skill leaves `~/.arana` and unrelated user files
   untouched.

## Evidence summary and open blockers

**Exercised on Windows 11, 2026-11-18** (temporary profiles; no accounts,
credentials or production profile changes)

- `arana mcp` direct stdio handshake against the working-tree debug build:
  initialize (`2025-06-18`), `tools/list` with three tools, `list_capabilities`,
  `read_url https://example.com` (`backend: jina`, `ok: true`), `search`
  (`backend: exa`, `ok: true`), unsafe URL and missing-argument structured
  errors, unknown-tool JSON-RPC error, clean EOF shutdown. The release binary at
  the time of writing predates the MCP server.
- `arana read https://example.com` through the release CLI: `ok: true`.
- Claude Code 2.1.325: local marketplace add, plugin install, inventory
  (1 skill, 1 MCP server), `claude mcp list` connected.
- Hermes Agent 0.23.1: portable package validation, install disabled, enable,
  user list; MCP through Hermes not exercised.

**Documented only**

- Codex, Antigravity, Cursor, Devin CLI, OpenCode, Command Code, OMP, Pi.
  Vendors' current documentation was checked; those clients were not exercised
  in this environment, so their rows must not be presented as verified.

**Preview / blocked**

- **DeepSeek Harness:** developer preview; pinned build and Standard-mode skill
  discovery not exercised; Cordis MCP config documented only.
- **Devin cloud:** stdio requires arana installed in Devin's execution
  environment; local install is not cloud support. Remote deployment is
  separate scope.
- **Antigravity MCP packaging:** no `mcp_config.json` in the package yet; the
  global/workspace example works only as documented configuration.
- **Release packaging:** the MCP server is implemented on the working tree but
  the tested `target/release/arana` was built before it. Rebuild and re-run the
  host checks before publishing.
- **Claude Code directory listing:** validator warns about a missing icon; the
  README lacks the one-line marketplace install command. Local install works;
  official directory submission is not done.
- **Cross-platform:** only Windows 11 was exercised. macOS and Linux remain
  documented/cross-checked, not host-verified.

## References

- Agent Plugins: [specification](https://agent-plugins.org/specification) ·
  [manifest](https://agent-plugins.org/plugin-authors/manifest) ·
  [MCP servers](https://agent-plugins.org/plugin-authors/mcp-servers)
- Hermes: [plugins](https://hermes-agent.nousresearch.com/docs/user-guide/features/plugins) ·
  [portable packages](https://hermes-agent.nousresearch.com/docs/developer-guide/plugins#portable-agent-plugins-v1-packages) ·
  [MCP](https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp)
- Codex: [package plugins](https://developers.openai.com/plugins/build/plugins) ·
  [MCP](https://learn.chatgpt.com/docs/extend/mcp) ·
  [skills](https://learn.chatgpt.com/docs/build-skills)
- Claude Code: [publish](https://code.claude.com/docs/en/plugins/publish) ·
  [manifest](https://code.claude.com/docs/en/plugins/manifest-reference) ·
  [MCP](https://code.claude.com/docs/en/mcp)
- Antigravity: [plugins](https://antigravity.google/docs/plugins) ·
  [build](https://antigravity.google/docs/build-plugins) ·
  [MCP](https://antigravity.google/docs/mcp)
- Cursor: [plugins](https://cursor.com/docs/plugins) ·
  [reference](https://cursor.com/docs/reference/plugins) ·
  [MCP](https://cursor.com/docs/mcp)
- Devin: [CLI plugins](https://docs.devin.ai/cli/extensibility/plugins/overview) ·
  [CLI MCP](https://docs.devin.ai/cli/extensibility/mcp/configuration) ·
  [cloud MCP](https://docs.devin.ai/work-with-devin/mcp)
- OpenCode: [skills](https://opencode.ai/docs/skills/) ·
  [MCP](https://opencode.ai/docs/mcp-servers/)
- Command Code: [skills](https://commandcode.ai/docs/skills) ·
  [MCP](https://commandcode.ai/docs/mcp) ·
  [settings](https://commandcode.ai/docs/settings)
- OMP: [skills](https://omp.sh/docs/skills) · [MCP](https://omp.sh/docs/mcp)
- DeepSeek Harness: [skills](https://deepseek-harness.github.io/deepseek-harness/en/reference/subsystems/skills) ·
  [MCP reference](https://deepseekdocs.com/en/docs/features/mcp)
- Pi: [skills](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/skills.md) ·
  [packages](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/packages.md) ·
  [MCP](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/mcp.md)
