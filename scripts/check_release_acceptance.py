"""Validate operator-trusted arana acceptance metadata offline.

This checks schema, policy, freshness and source identity; it does not execute
network probes or cryptographically attest a manifest's claimed execution.
Use record_release_acceptance.py to create records from actual CLI runs. Treat
externally supplied manifests as untrusted until the operator reviews their
provenance; a fabricated local manifest is not proof of live acceptance.
"""
from __future__ import annotations
import argparse
import json
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
UPSTREAM_WAIVERS = {
    ("reddit", "subreddit"): "Reddit's site tool returns HTML instead of data and subreddit has no public-web route yet; approved to ship untested in 1.6.1",
    ("instagram", "search"): "No Instagram login on the test machine; approved to ship untested in 1.6.1",
    ("instagram", "profile"): "No Instagram login on the test machine; approved to ship untested in 1.6.1",
    ("instagram", "posts"): "No Instagram login on the test machine; approved to ship untested in 1.6.1",
    ("bilibili", "subtitle"): "Upstream QR login saves an empty SESSDATA and the test Chrome has no Bilibili login; approved to ship untested in 1.6.1",
    ("xiaoyuzhou", "transcribe"): "No Groq key on the test machine; approved to ship untested in 1.6.1",
}
FROZEN_REQUIRED = {("web", "search"), ("web", "read"), ("v2ex", "hot"), ("v2ex", "node"), ("v2ex", "read"), ("v2ex", "user"), ("github", "search"), ("github", "read"), ("bilibili", "search"), ("bilibili", "read"), ("bilibili", "hot"), ("bilibili", "subtitle"), ("youtube", "search"), ("youtube", "read"), ("youtube", "subtitle"), ("youtube", "transcribe"), ("xueqiu", "quote"), ("xueqiu", "search"), ("xueqiu", "hot_posts"), ("xueqiu", "hot_stocks"), ("xiaohongshu", "search"), ("xiaohongshu", "read"), ("xiaohongshu", "comments"), ("boss", "search"), ("boss", "read"), ("xiaoyuzhou", "transcribe"), ("linkedin", "jobs"), ("linkedin", "read"), ("reddit", "search"), ("reddit", "read"), ("reddit", "subreddit"), ("twitter", "search"), ("twitter", "profile"), ("twitter", "tweets"), ("twitter", "thread"), ("facebook", "search"), ("facebook", "profile"), ("instagram", "search"), ("instagram", "profile"), ("instagram", "posts")}

def binary_signature(binary: Path) -> str:
    proc = subprocess.run([str(binary), "version"], capture_output=True, text=True, timeout=30)
    try:
        data = json.loads(proc.stdout)
        value = data["result"]["source_sha256"]
    except (ValueError, KeyError, TypeError) as exc:
        raise ValueError("binary version output lacks result.source_sha256; rebuild with source signature") from exc
    if proc.returncode or not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
        raise ValueError("binary returned invalid source signature")
    return value

def catalog(binary: Path) -> list[tuple[str, str]]:
    p = subprocess.run([str(binary), "channels"], capture_output=True, text=True, timeout=30)
    data = json.loads(p.stdout)
    if p.returncode or data.get("ok") is not True:
        raise ValueError("binary channels command failed")
    return [(c["name"], a["name"]) for c in data["channels"] for a in c["actions"]]

def validate(data: object, required: list[tuple[str, str]], digest: str, now: int | None = None) -> list[str]:
    errors: list[str] = []
    if not isinstance(data, dict):
        return ["Acceptance manifest must be an object"]
    if not isinstance(digest,str) or not re.fullmatch(r"[0-9a-fA-F]{64}",digest):
        errors.append("current source digest must be 64 hexadecimal characters")
    required_set = set(required)
    if len(required) != 40 or len(required_set) != 40 or required_set != FROZEN_REQUIRED:
        errors.append("required action catalog must exactly match the frozen 40-action upstream contract")
    if data.get("source_sha256") != digest:
        errors.append("manifest source digest does not match current binary")
    if data.get("release_ready") is not True:
        errors.append("manifest release_ready must be true")
    if data.get("pending") or data.get("additional_required_validation"):
        errors.append("Acceptance still has pending actions or required validation")
    rows = data.get("results")
    if not isinstance(rows, list):
        return errors + ["Acceptance results must be a list"]
    waivers = data.get("waived")
    if not isinstance(waivers, list):
        return errors + ["Acceptance waived must be a list"]
    allowed = UPSTREAM_WAIVERS
    waived: set[tuple[str, str]] = set()
    for w in waivers:
        if not isinstance(w, dict):
            errors.append("Waiver must be an object"); continue
        if not all(isinstance(w.get(field),str) for field in ("channel","action")):
            errors.append("Waiver channel and action must be strings"); continue
        key = (w.get("channel"), w.get("action"))
        if key not in required_set or allowed.get(key) != w.get("reason"):
            errors.append("Waiver is not one of the six exact upstream policy entries")
        if key in waived:
            errors.append(f"Duplicate waiver: {key[0]}/{key[1]}")
        waived.add(key)
    if waived != set(allowed):
        errors.append("Waiver set must equal the six approved upstream waivers")
    seen: set[tuple[str, str]] = set(); passed: set[tuple[str, str]] = set(); failed = 0
    now = int(time.time()) if now is None else now
    for row in rows:
        if not isinstance(row, dict):
            errors.append("Acceptance result must be an object"); continue
        if not all(isinstance(row.get(field),str) for field in ("channel","action")):
            errors.append("Acceptance channel and action must be strings"); continue
        key = (row.get("channel"), row.get("action"))
        if key not in required_set:
            errors.append("Unknown acceptance action in result row"); continue
        if key in seen:
            errors.append(f"Duplicate acceptance result: {key[0]}/{key[1]}"); continue
        seen.add(key)
        if key in waived:
            errors.append(f"Waived action also has a result: {key[0]}/{key[1]}")
        if row.get("passed") is not True:
            failed += 1
            if not isinstance(row.get("error"), str) or not row["error"].strip():
                errors.append(f"Failed result needs an explicit reason: {key[0]}/{key[1]}")
            continue
        if row.get("method") != "real-cli": errors.append(f"Not a real-cli result: {key[0]}/{key[1]}")
        if not isinstance(row.get("backend"), str) or not row["backend"].strip(): errors.append(f"Missing backend: {key[0]}/{key[1]}")
        if not isinstance(row.get("os"), str) or not row["os"].strip(): errors.append(f"Missing OS: {key[0]}/{key[1]}")
        if row.get("source_sha256") != digest: errors.append(f"Result source digest mismatch: {key[0]}/{key[1]}")
        if type(row.get("item_count")) is not int or row["item_count"] <= 0: errors.append(f"Non-positive item_count: {key[0]}/{key[1]}")
        try:
            raw_stamp=row.get("at")
            if not isinstance(raw_stamp,str) or not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})",raw_stamp):
                raise ValueError("timestamp must be timezone-aware RFC3339")
            stamp = datetime.fromisoformat(raw_stamp.replace("Z", "+00:00"))
            age = now - int(stamp.timestamp())
            if age < 0 or age > 7 * 86400: raise ValueError
        except (KeyError, TypeError, ValueError, OverflowError):
            errors.append(f"Missing, invalid, future, or stale timestamp: {key[0]}/{key[1]}")
        if row.get("error") not in (None, ""): errors.append(f"Passing result contains failure reason: {key[0]}/{key[1]}")
        passed.add(key)
    for key in sorted(required_set - seen - waived): errors.append(f"Missing acceptance result: {key[0]}/{key[1]}")
    if len(passed) < 34: errors.append(f"Release gate requires at least 34 real passes; found {len(passed)}")
    if len(passed) + len(waived) != 40: errors.append(f"Release gate requires 40 accounted actions; found {len(passed)} passes and {len(waived)} waivers")
    if failed: errors.append(f"{failed} failed action(s) block release; failures are never automatically waived")
    return errors

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("manifest", nargs="?", type=Path, default=ROOT / ".github/release-acceptance.json")
    ap.add_argument("--binary", type=Path, default=ROOT / "target/release/arana.exe")
    ns = ap.parse_args()
    try:
        digest = binary_signature(ns.binary)
        required = catalog(ns.binary)
        data = json.loads(ns.manifest.read_text(encoding="utf-8"))
        errors = validate(data, required, digest)
    except (OSError, ValueError, subprocess.SubprocessError, json.JSONDecodeError) as exc:
        errors = [f"Acceptance validation unavailable: {exc}"]
    for error in errors: print(error)
    if errors: print("Release blocked. Failures are not converted to waivers.")
    else: print("Acceptance metadata policy is valid; execution provenance remains operator-trusted, not independently attested.")
    return 1 if errors else 0

if __name__ == "__main__": raise SystemExit(main())
