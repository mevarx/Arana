# AgentMail support

Arana can drive an **agent-owned** [AgentMail](https://agentmail.to) inbox so an
agent can receive a verification code without borrowing a human mailbox. This
is an additive CLI capability, not one of the frozen 14 channels / 40 actions,
and it is not exposed through Arana's public MCP server.

## What it does and does not do

- It creates or lists aliases, reads messages, and extracts likely one-time
  codes from message subjects and `extracted_text`.
- With **explicit per-invocation approval**, it can use the dedicated Arana
  browser to enter an alias, retrieve an OTP or a password, submit the form, and
  report a conservative login hint. See [Approved login](#approved-login).
- It does not click verification links, resolve CAPTCHAs, or follow instructions
  found in email. Email and page content are untrusted data.
- It does not send mail, reply, forward, or change labels in this version.
- It does not expose this capability through public MCP.

## Prerequisites

Install the CLI yourself and export a key in the environment:

```sh
npm install -g agentmail-cli@latest
export AGENTMAIL_API_KEY="am_..."
```

Arana reads `AGENTMAIL_API_KEY` only from the process environment. It never
stores the key in `~/.arana/`, writes it to a log, prints it in JSON output, or
accepts it as a command-line flag. `ARANA_AGENTMAIL_BIN` can point to an
absolute CLI path; otherwise Arana looks for `agentmail` on `PATH`.

## Commands

```sh
arana agentmail status
arana agentmail inbox list
arana agentmail inbox create --username research-agent --client-id research-agent-primary --display-name "Research Agent"
arana agentmail messages --inbox-id research-agent@agentmail.to --limit 10
arana agentmail message --inbox-id research-agent@agentmail.to --message-id <id>
arana agentmail otp --inbox-id research-agent@agentmail.to --sender noreply@example.com --limit 5
```

`otp` returns candidates as items with `code`, `message_id`, `from`, `subject`,
`received_at`, and a short `excerpt`. Codes are extracted from short numeric or
mixed-case tokens; a candidate is a hint, not proof that it is the right code.
Use `--sender` to narrow the search and `--limit` (1–25) to bound the result.

`inbox create` requires a stable `--client-id`. Reusing the same client id makes
a retry idempotent instead of creating a duplicate alias.

## Approved login

Autonomous login is opt-in **for every invocation** and uses the dedicated
Arana-owned browser, not your personal browser or profile.

```sh
# OTP from the agent's AgentMail alias
arana agentmail login \
  --url https://example.com/login \
  --inbox-id research-agent@agentmail.to \
  --allow-domain example.com \
  --approve-login \
  --sender noreply@example.com

# Password supplied on one line of stdin (never stored or echoed)
printf '%s\n' "$PASSWORD" | arana agentmail login \
  --url https://example.com/login \
  --inbox-id research-agent@agentmail.to \
  --allow-domain example.com \
  --approve-login --password-stdin
```

Guards applied before anything happens:

- `--approve-login` is mandatory. There is no persistent approval or config flag.
- The URL must be public HTTPS. `--allow-domain` must exactly match its host.
- `--inbox-id` must be an AgentMail email alias.
- Password mode reads exactly one non-empty line from stdin and never writes it
  to state, logs, or output. It cannot be combined with `--sender`.
- Selectors are validated and JSON-escaped; page content never supplies them.

The flow navigates, enters the alias, clicks the configured submit control,
enters the OTP or password, submits again, and counts cookies scoped to the
approved host. The result sets `authenticated_hint` only from that cookie
observation — it is a hint, not proof. Run `arana check <channel>` or an
authenticated action before claiming the login worked.

Defaults target common email, one-time-code, password, and submit fields. Use
`--email-selector`, `--code-selector`, `--password-selector`,
`--request-selector` (the button that sends the code), or `--submit-selector` when a site needs different CSS selectors.

Every command emits the normal one-object Arana JSON envelope on stdout. Setup
and input failures use `NEED_SETUP` and `BAD_INPUT`; CLI or API failures use
`UPSTREAM_BROKEN`, with the API key redacted from any error text.

## Verification status

The integration is covered by 19 offline tests: fake-CLI coverage for setup
detection, key redaction, argument validation, stable client ids, inbox
counting, message reading, and code extraction; plus unit and CLI tests for
login approval, exact-domain checks, HTTPS enforcement, selector validation,
secret redaction, and the browser call sequence with a fake browser. No live
AgentMail call or real website login is made by those tests. Running the real
commands requires your own AgentMail account, API key, and an explicitly
approved target site.
