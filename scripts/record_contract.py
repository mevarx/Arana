#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Record the Agent-Reach 1.6.1 contract: the channel catalog and one real
invocation per declared action, with provenance.

The recorder never fabricates output. Every file it writes is the verbatim
stdout of a real Python-build invocation (or an explicit "not recorded" stub
when the invocation could not be attempted at all), plus a manifest of how it
was produced.

Usage:
    python scripts/record_contract.py --out tests/golden
    python scripts/record_contract.py --out tests/golden --only web:read,github:search
    python scripts/record_contract.py --out tests/golden --fresh-home

Environment isolation: HOME/XDG_CONFIG_HOME are pointed at a scratch directory
so the recording cannot read or write the operator's real state. No global
Python installs are touched; the interpreter is the isolated .upstream-venv.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_UPSTREAM = Path("D:/CODING/arana-upstream-reference")
DEFAULT_VENV_PYTHON = REPO_ROOT / ".upstream-venv" / "Scripts" / "python.exe"
UPSTREAM_VERSION = "1.6.1"

#: Per-action argument lists used when the upstream Action has no live_test.
#: These are chosen by the recorder (documented in the manifest); they are not
#: upstream data. An input that is unavailable offline still records a real
#: error envelope, which is a valid contract sample.
FALLBACK_INPUTS: dict[str, dict[str, object]] = {
    "reddit:read": {"target": "https://www.reddit.com/r/Python/comments/1x1t161/python_315_released/"},
    "linkedin:read": {"target": "https://www.linkedin.com/jobs/view/software-engineer-at-jobs-via-dice-4325476729"},
    "xiaohongshu:read": {"target": "https://www.xiaohongshu.com/explore/67443a12000000000703508e"},
    "xiaohongshu:comments": {"target": "https://www.xiaohongshu.com/explore/67443a12000000000703508e", "limit": 2},
    "youtube:transcribe": {"target": "https://www.youtube.com/watch?v=jNQXAC9IVRw"},
    "youtube:subtitle": {"target": "https://www.youtube.com/watch?v=jNQXAC9IVRw"},
    "xiaoyuzhou:transcribe": {
        "target": "https://www.xiaoyuzhoufm.com/episode/5e4a2c8a418a84a0463d1f4b"
    },
    "twitter:tweets": {"username": "OpenAI", "limit": 2},
    "twitter:thread": {"target": "https://x.com/OpenAI/status/1726176942383698079"},
    "twitter:profile": {"username": "OpenAI"},
    "reddit:subreddit": {"name": "python", "limit": 2},
    "linkedin:jobs": {"query": "python", "limit": 2},
    "boss:search": {"query": "Python", "limit": 2},
    "boss:read": {"target": "https://www.zhipin.com/job_detail/example.html"},
    "instagram:profile": {"username": "nasa"},
    "facebook:profile": {"username": "zuck"},
    "xueqiu:hot_posts": {"limit": 2},
    "xueqiu:hot_stocks": {"limit": 2},
    "v2ex:read": {"target": "https://www.v2ex.com/t/1000"},
}

#: Budget override per action (seconds). Everything else uses --timeout.
BUDGETS: dict[str, int] = {
    "youtube:transcribe": 60,
    "xiaoyuzhou:transcribe": 60,
    "boss:search": 45,
    "boss:read": 45,
    "linkedin:jobs": 45,
    "linkedin:read": 45,
    # These two spend the whole budget in upstream's public-web/install probe
    # before answering; they need a longer bound to emit any envelope at all.
    "twitter:search": 90,
    "twitter:profile": 90,
}


def _iso_now() -> str:
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _git_head(path: Path) -> str:
    try:
        out = subprocess.run(
            ["git", "-C", str(path), "rev-parse", "HEAD"],
            capture_output=True, text=True, timeout=30, check=False,
        )
        return out.stdout.strip() if out.returncode == 0 else ""
    except (OSError, subprocess.SubprocessError):
        return ""


def _build_env(home: Path, upstream: Path) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("AGENT_REACH")}
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)  # Windows expanduser fallback
    env["XDG_CONFIG_HOME"] = str(home / ".config")
    env["XDG_CACHE_HOME"] = str(home / ".cache")
    env["PYTHONPATH"] = str(upstream)
    env["PYTHONIOENCODING"] = "utf-8"
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    env["NO_COLOR"] = "1"
    return env


_LIVE_TEST_SNIPPET = """
import json
from agent_reach.channels import get_all_channels
out = {}
for ch in get_all_channels():
    out[ch.name] = {
        a.name: {
            "live_test": a.live_test,
            "timeout": a.timeout,
            "params": [
                {"name": p.name, "type": p.type.__name__, "required": p.required,
                 "default": p.default}
                for p in a.params
            ],
            "backends": [[label, method] for label, method in a.backends],
        }
        for a in ch.actions
    }
print(json.dumps(out, ensure_ascii=False))
"""


def _action_metadata(python: Path, env: dict[str, str], home: Path) -> dict:
    """Live-test inputs and backend method names, read from the Python objects.

    The `channels` payload deliberately omits live_test; the recorder needs the
    real values so each probe uses upstream's own inputs instead of invented ones.
    """
    proc = subprocess.run(
        [str(python), "-c", _LIVE_TEST_SNIPPET],
        capture_output=True, text=True, encoding="utf-8", errors="replace",
        timeout=120, env=env, cwd=str(home), check=False,
    )
    if proc.returncode != 0:
        raise SystemExit(f"could not read action metadata: {proc.stderr[-1500:]}")
    return json.loads(proc.stdout)


def _run_cli(python: Path, env: dict[str, str], args: list[str], timeout: float) -> dict:
    """Run one CLI invocation; return stdout/stderr/exit/duration."""
    argv = [str(python), "-m", "agent_reach.cli", *args]
    started = time.monotonic()
    try:
        proc = subprocess.run(
            argv, capture_output=True, text=True, encoding="utf-8", errors="replace",
            timeout=timeout, env=env, cwd=str(env.get("HOME", str(Path.home()))), check=False,
        )
        stdout, stderr, code, timed_out = proc.stdout, proc.stderr, proc.returncode, False
    except subprocess.TimeoutExpired as exc:
        stdout = (exc.stdout or "") if isinstance(exc.stdout, str) else ""
        stderr = (exc.stderr or "") if isinstance(exc.stderr, str) else ""
        code, timed_out = None, True
    duration_ms = int((time.monotonic() - started) * 1000)
    return {
        "argv": ["agent-reach", *args],
        "stdout": stdout,
        "stderr": stderr,
        "exit_code": code,
        "timed_out": timed_out,
        "duration_ms": duration_ms,
    }


def _parse_single_json(stdout: str) -> dict | None:
    """Return the one JSON object the CLI prints, or None."""
    text = stdout.strip()
    if not text:
        return None
    try:
        value = json.loads(text)
    except ValueError:
        # Tolerate a leading log line, but never invent fields.
        start = text.find("{")
        if start < 0:
            return None
        try:
            value = json.loads(text[start:])
        except ValueError:
            return None
    return value if isinstance(value, dict) else None


def _catalog_entries(python: Path, env: dict[str, str], timeout: float) -> tuple[dict | None, dict]:
    run = _run_cli(python, env, ["channels"], timeout)
    payload = _parse_single_json(run["stdout"])
    run.pop("stdout")
    run["stderr"] = run["stderr"][-2000:]
    return payload, run


def _action_args(channel: dict, action: dict, meta: dict) -> tuple[list[str], dict, str]:
    """CLI argv for one action, plus the params used and their source."""
    params: dict[str, object] = {}
    source = "upstream live_test"
    for p in action["params"]:
        if p["required"]:
            params[p["name"]] = ""
    for p in action["params"]:
        if not p["required"] and p["default"] is not None:
            params[p["name"]] = p["default"]
    live = meta.get("live_test")
    if isinstance(live, dict) and live:
        params.update(live)
    else:
        frozen = FALLBACK_INPUTS.get(f"{channel['name']}:{action['name']}")
        if frozen:
            params.update(frozen)
            source = "recorder fallback input"
        else:
            source = "recorder fallback input (required params unset)"
    argv = [channel["name"], action["name"]]
    for p in action["params"]:
        value = params.get(p["name"])
        if p["required"]:
            if value is None or value == "":
                continue
            argv.append(str(value))
        elif value is not None:
            argv.extend([f"--{p['name']}", str(value)])
    return argv, params, source


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Record the Agent-Reach 1.6.1 contract")
    ap.add_argument("--out", default=str(REPO_ROOT / "tests" / "golden"))
    ap.add_argument("--upstream", default=str(DEFAULT_UPSTREAM))
    ap.add_argument("--python", default=str(DEFAULT_VENV_PYTHON))
    ap.add_argument("--home", default=str(REPO_ROOT / ".upstream-home" / "record"))
    ap.add_argument("--fresh-home", action="store_true", help="delete the recording HOME first")
    ap.add_argument("--timeout", type=float, default=30.0, help="default per-action budget")
    ap.add_argument("--only", default="", help="comma separated channel:action filters")
    ap.add_argument("--catalog-only", action="store_true")
    ap.add_argument(
        "--merge",
        action="store_true",
        help="keep existing records for actions not re-recorded in this run",
    )
    args = ap.parse_args(argv)

    upstream = Path(args.upstream).resolve()
    python = Path(args.python).resolve()
    out_dir = Path(args.out).resolve()
    home = Path(args.home).resolve()
    if args.fresh_home and home.exists():
        import shutil

        shutil.rmtree(home)
    (home / ".config").mkdir(parents=True, exist_ok=True)
    out_dir.mkdir(parents=True, exist_ok=True)
    if not python.is_file():
        print(f"missing interpreter: {python}", file=sys.stderr)
        return 2

    env = _build_env(home, upstream)
    filters = {f.strip() for f in args.only.split(",") if f.strip()}

    started_at = _iso_now()
    meta_all = _action_metadata(python, env, home)
    catalog, catalog_run = _catalog_entries(python, env, max(args.timeout, 20))
    if not catalog or not catalog.get("channels"):
        print(f"catalog recording failed: {catalog_run}", file=sys.stderr)
        return 1

    (out_dir / "channels.json").write_text(
        json.dumps(catalog, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )

    # Full registry data: backend (label, method) pairs, live_test, timeouts and
    # Python type names — everything the Rust registry must reproduce.
    registry = {"schema_version": 1, "channels": []}
    for channel in catalog["channels"]:
        entry = {k: channel[k] for k in ("name", "description", "level", "login")}
        entry["actions"] = []
        for action in channel["actions"]:
            meta = meta_all.get(channel["name"], {}).get(action["name"], {})
            entry["actions"].append(
                {
                    **{k: action[k] for k in ("name", "help", "usage", "params", "public_web")},
                    "backends": meta.get("backends", []),
                    "live_test": meta.get("live_test"),
                    "timeout": meta.get("timeout"),
                }
            )
        registry["channels"].append(entry)
    (out_dir / "registry.json").write_text(
        json.dumps(registry, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )

    records: list[dict] = []
    if args.merge:
        existing = out_dir / "manifest.json"
        if existing.is_file():
            try:
                previous = json.loads(existing.read_text(encoding="utf-8"))
                records = [
                    record
                    for record in previous.get("records", [])
                    if not filters or f"{record['channel']}:{record['action']}" not in filters
                ]
            except (OSError, ValueError):
                records = []
    if not args.catalog_only:
        for channel in catalog["channels"]:
            for action in channel["actions"]:
                key = f"{channel['name']}:{action['name']}"
                if filters and key not in filters:
                    continue
                meta = meta_all.get(channel["name"], {}).get(action["name"], {})
                cli_args, params, source = _action_args(channel, action, meta)
                budget = float(BUDGETS.get(key) or args.timeout)
                run = _run_cli(python, env, [*cli_args, "--timeout", str(int(budget))], budget + 15)
                envelope = _parse_single_json(run["stdout"])
                name = f"{channel['name']}__{action['name']}.json"
                record = {
                    "schema_version": 1,
                    "channel": channel["name"],
                    "action": action["name"],
                    "params": params,
                    "params_source": source,
                    "argv": run["argv"],
                    "exit_code": run["exit_code"],
                    "duration_ms": run["duration_ms"],
                    "timed_out": run["timed_out"],
                    "live_success": bool(envelope and envelope.get("ok") is True),
                    "recorded_at": _iso_now(),
                    "envelope": envelope,
                    "stderr_tail": run["stderr"][-1500:],
                }
                (out_dir / name).write_text(
                    json.dumps(record, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
                )
                records.append({k: record[k] for k in (
                    "channel", "action", "params", "params_source", "argv", "exit_code",
                    "duration_ms", "timed_out", "live_success",
                )} | {"file": name})
                status = "ok" if record["live_success"] else (
                    (envelope or {}).get("error", {}).get("code") if envelope else "no-envelope"
                )
                print(f"{key:34s} {status}")

    manifest = {
        "schema_version": 1,
        "kind": "agent-reach-contract-recording",
        "upstream": {
            "project": "Panniantong/Agent-Reach",
            "version": UPSTREAM_VERSION,
            "git_head": _git_head(upstream),
            "path": str(upstream),
        },
        "recorder": {
            "tool": "scripts/record_contract.py",
            "started_at": started_at,
            "finished_at": _iso_now(),
            "python": sys.version.split()[0],
            "interpreter": str(python),
            "isolated_home": str(home),
            "catalog_argv": catalog_run["argv"],
            "catalog_exit_code": catalog_run["exit_code"],
            "default_timeout_s": args.timeout,
            "budgets_s": BUDGETS,
            "fallback_inputs": FALLBACK_INPUTS,
        },
        "catalog_file": "channels.json",
        "action_count": sum(len(c["actions"]) for c in catalog["channels"]),
        "channel_count": len(catalog["channels"]),
        "recorded_count": len(records),
        "live_success_count": sum(1 for r in records if r["live_success"]),
        "records": records,
    }
    (out_dir / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(
        f"recorded {len(records)}/{manifest['action_count']} actions "
        f"({manifest['live_success_count']} live successes) -> {out_dir}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
