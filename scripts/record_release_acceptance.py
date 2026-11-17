"""Record 40 acceptance decisions by executing the real arana binary.

Run after building: python scripts/record_release_acceptance.py --binary target/release/arana.exe
Only the six pinned upstream waiver entries are excluded. Failures remain failed.
"""
from __future__ import annotations
import argparse
import json
import os
import platform
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from check_release_acceptance import UPSTREAM_WAIVERS, binary_signature, catalog, validate
DEFAULT_OUT = ROOT / ".github/release-acceptance.json"

def probes(path: Path) -> dict[tuple[str, str], list[str]]:
    fixture = json.loads(path.read_text(encoding="utf-8"))
    result = {}
    for row in fixture.get("recorder", {}).get("records", fixture.get("records", [])):
        argv = row.get("argv")
        if isinstance(argv, list) and len(argv) >= 3:
            result[(str(argv[1]), str(argv[2]))] = [str(x) for x in argv[3:]]
    return result

def decode(stdout: str) -> dict:
    # The binary contract is one JSON envelope on stdout; do not infer from stderr.
    try:
        parsed = json.loads(stdout)
        return parsed if isinstance(parsed, dict) else {}
    except ValueError:
        return {}

def record(binary: Path, required: list[tuple[str, str]], inputs: dict[tuple[str, str], list[str]], digest: str) -> tuple[dict, list[str]]:
    rows = []
    pending = []
    for channel, action in required:
        key = (channel, action)
        if key in UPSTREAM_WAIVERS:
            continue
        argv = inputs.get(key)
        if argv is None:
            pending.append({"channel": channel, "action": action, "reason": "no recorded fixed probe input; setup required"})
            continue
        try:
            proc = subprocess.run([str(binary), channel, action, *argv], capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=900, cwd=ROOT)
            envelope = decode(proc.stdout)
            items = envelope.get("items")
            if not isinstance(items, list):
                items = []
            error = envelope.get("error")
            ok = proc.returncode == 0 and envelope.get("ok") is True and bool(items) and isinstance(envelope.get("backend"), str) and bool(envelope["backend"].strip())
            raw_code = error.get("code") if isinstance(error, dict) else None
            known_codes = {"NEED_SETUP", "NEED_LOGIN", "UPSTREAM_BROKEN", "TIMEOUT", "RATE_LIMITED", "PLATFORM_BLOCKED", "NOT_FOUND", "BAD_INPUT", "BUSY"}
            code = None if ok else (raw_code if raw_code in known_codes else ("EMPTY" if envelope else "UNREADABLE"))
            rows.append({"channel": channel, "action": action, "method": "real-cli", "backend": envelope.get("backend"), "item_count": len(items), "at": datetime.now(timezone.utc).isoformat(), "os": platform.system(), "source_sha256": digest, "passed": ok, "error": code})
            print(f"{channel}/{action}: {'PASS' if ok else 'FAIL ' + str(code)} ({len(items)} items)")
        except subprocess.TimeoutExpired:
            rows.append({"channel": channel, "action": action, "method": "real-cli", "backend": None, "item_count": 0, "at": datetime.now(timezone.utc).isoformat(), "os": platform.system(), "source_sha256": digest, "passed": False, "error": "TIMEOUT"})
            print(f"{channel}/{action}: FAIL TIMEOUT")
    waivers = [{"channel": c, "action": a, "reason": r} for (c, a), r in sorted(UPSTREAM_WAIVERS.items())]
    data = {"source_sha256": digest, "release_ready": True, "pending": pending, "waived": waivers, "results": sorted(rows, key=lambda r: (r["channel"], r["action"]))}
    errors = validate(data, required, digest)
    data["release_ready"] = not errors
    return data, errors

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--binary", type=Path, default=ROOT / "target/release/arana.exe")
    ap.add_argument("--inputs", type=Path, default=ROOT / "tests/golden/manifest.json")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ns = ap.parse_args()
    try:
        digest = binary_signature(ns.binary)
        required = catalog(ns.binary)
        data, errors = record(ns.binary, required, probes(ns.inputs), digest)
        ns.out.parent.mkdir(parents=True, exist_ok=True)
        ns.out.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(f"recorded {len(data['results'])} real-cli rows; {sum(r['passed'] is True for r in data['results'])} passes; {len(data['pending'])} pending; release_ready={data['release_ready']}")
        for error in errors: print(error)
        return 0 if data["release_ready"] else 1
    except (OSError, ValueError, subprocess.SubprocessError, json.JSONDecodeError) as exc:
        print(f"Acceptance recording unavailable: {exc}", file=sys.stderr)
        return 1

if __name__ == "__main__": raise SystemExit(main())
