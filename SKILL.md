---
name: arana
description: >
  Read-only access to platform content: search and read posts, comments, subtitles, stock quotes and jobs.
  Use when public search or reading tools fail or lack the body; never post, message, log in for the user,
  or request credentials. Supports Xiaohongshu, Bilibili, X/Twitter, Reddit, YouTube, GitHub, V2EX,
  Xueqiu, LinkedIn, Boss Zhipin, Instagram, Facebook and Xiaoyuzhou.
metadata:
  homepage: https://arana.mevarx.xyz
  version: "0.1.0"
---

# arana

Read-only internet access for agents. Invoke `arana` and consume its single JSON result.

## Route selection

- Prefer your own tools for public content. Use arana when those are unavailable, fail, or miss needed content.
- A platform name or link alone never authorizes setup or login. Do not install tools or open login flows unless the user asks.
- See available actions with `arana channels`; invoke `arana <channel> <action> [args]` or `arana read <url>`.
- Distinguish body text from snippets, page shells, comment counts and subtitle track listings. Never claim content you did not retrieve.

## Results

- `ok: true`: use `items`; each item has `title`, `url`, `author`, `published_at`, `text`, and `raw`.
- `ok: false`: branch on `error.code` and follow `error.hint`. Stop on login-required, rate limits, platform blocks, and busy errors. Never retry through another account or browser.

## Safety

- Read-only. Never post, comment, like, message, or perform other write actions on a platform.
- Never log in for the user, ask for passwords, or read browser cookie databases.
- Do not attempt to bypass captchas, risk controls, access controls, or platform rate limits.
- Ordinary queries never change dependencies or update automatically.
- The human logs in themselves in the dedicated browser when explicitly requested.

## Management

- Add/remove a channel only at the user's request: `arana add <channel>` / `arana remove <channel>`.
- Status: `arana doctor`; update: `arana update`.
- Before uninstalling, show `arana uninstall --dry-run`; uninstall only after explicit confirmation.
- Link into an agent's skills directory with `arana skill link <skills-dir>`; unlink with `arana skill unlink <skills-dir>`.
- Never remove files outside arana's recorded ownership or a matching recorded skill copy.
