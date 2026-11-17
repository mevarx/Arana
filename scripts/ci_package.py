#!/usr/bin/env python3
"""Build a portable, self-verifying release archive from the built binaries.

Works the same on Linux, macOS and Windows so CI and a local release produce
identical layouts:

    arana / agent-reach (+ .exe)  the CLI and its compatibility alias
    README.md, CONTRACT_NOTES.md, CHANGELOG.md, docs/**
    LICENSE, NOTICE, THIRD_PARTY/obscura/LICENSE

The archive is smoke-tested before it is written: both binaries must answer
`version` and `channels`, agree on the catalog, and report the expected
version. A `.sha256` sidecar is written next to the archive.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DOCS = ["README.md", "CONTRACT_NOTES.md", "CHANGELOG.md", "LICENSE", "NOTICE"]
THIRD_PARTY = ["THIRD_PARTY/obscura/LICENSE", "THIRD_PARTY/obscura/README.md"]


def manifest_version() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    section = text.split("[workspace.package]", 1)
    if len(section) != 2:
        raise SystemExit("Cargo.toml has no [workspace.package] section")
    match = re.search(r'(?m)^\s*version\s*=\s*"([^"]+)"', section[1])
    if not match:
        raise SystemExit("Cargo.toml [workspace.package] has no version")
    return match.group(1)


def platform_tag() -> str:
    system = {"nt": "windows", "posix": "unix"}[os.name]
    if os.name == "nt":
        return "windows-x64"
    machine = platform.machine().lower()
    arch = {"x86_64": "x64", "amd64": "x64", "arm64": "arm64", "aarch64": "arm64"}.get(
        machine, machine
    )
    name = {"darwin": "macos", "linux": "linux"}.get(sys.platform, sys.platform)
    return f"{name}-{arch}"


def run_json(binary: Path, *args: str) -> dict:
    result = subprocess.run(
        [str(binary), *args],
        capture_output=True,
        text=True,
        # The catalog is UTF-8 (CJK channel descriptions); a Windows runner
        # decodes pipes as cp1252 by default and would crash here.
        encoding="utf-8",
        errors="replace",
        timeout=60,
    )
    if result.returncode != 0:
        raise SystemExit(
            f"{binary.name} {' '.join(args)} exited {result.returncode}: "
            f"{result.stderr.strip()[:400]}"
        )
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise SystemExit(f"{binary.name} did not print one JSON object: {error}") from error


def verify(binary: Path, expected: str) -> None:
    version = run_json(binary, "version")
    if version.get("ok") is not True:
        raise SystemExit(f"{binary.name} version is not ok: {version}")
    actual = version.get("result", {}).get("version")
    if expected and actual != expected:
        raise SystemExit(
            f"{binary.name} reports version {actual!r}, expected {expected!r}"
        )
    catalog = run_json(binary, "channels")
    channels = catalog.get("channels")
    if not isinstance(channels, list) or len(channels) != 14:
        raise SystemExit(f"{binary.name} catalog is not the 14-channel contract")
    if sum(len(channel.get("actions", [])) for channel in channels) != 40:
        raise SystemExit(f"{binary.name} catalog is not the 40-action contract")


def deterministic_info(name: str, mode: int) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = (mode << 16) | (0x20 if mode & 0o111 else 0)
    return info


def add_text(archive: zipfile.ZipFile, relative: str, mode: int = 0o644) -> None:
    path = ROOT / relative
    if not path.is_file():
        raise SystemExit(f"missing release file: {relative}")
    archive.writestr(deterministic_info(relative, mode), path.read_bytes())


def add_docs(archive: zipfile.ZipFile) -> None:
    for relative in DOCS + THIRD_PARTY:
        add_text(archive, relative)
    for path in sorted((ROOT / "docs").glob("*.md")):
        add_text(archive, str(path.relative_to(ROOT)))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=ROOT / "dist")
    parser.add_argument(
        "--binary-dir",
        type=Path,
        default=ROOT / "target" / "release",
        help="directory holding the built binaries",
    )
    parser.add_argument(
        "--version",
        default=None,
        help="expected version (defaults to the workspace manifest)",
    )
    parser.add_argument("--tag", default=None, help="tag whose version must match")
    args = parser.parse_args()

    expected = args.version or manifest_version()
    if args.tag:
        tagged = args.tag.lstrip("v")
        if tagged != expected:
            raise SystemExit(
                f"tag {args.tag!r} does not match workspace version {expected!r}"
            )

    suffix = ".exe" if os.name == "nt" else ""
    binaries = [args.binary_dir / f"{name}{suffix}" for name in ("arana", "agent-reach")]
    for binary in binaries:
        if not binary.is_file():
            raise SystemExit(f"missing built binary: {binary}")

    with tempfile.TemporaryDirectory():
        for binary in binaries:
            verify(binary, expected)
        arana_catalog = run_json(binaries[0], "channels")
        alias_catalog = run_json(binaries[1], "channels")
        if arana_catalog != alias_catalog:
            raise SystemExit("agent-reach alias disagrees with the arana catalog")

    args.out.mkdir(parents=True, exist_ok=True)
    archive_path = args.out / f"arana-{platform_tag()}.zip"
    with zipfile.ZipFile(archive_path, "w") as archive:
        for binary in binaries:
            archive.writestr(
                deterministic_info(binary.name, 0o755), binary.read_bytes()
            )
        add_docs(archive)

    with zipfile.ZipFile(archive_path) as archive:
        if archive.testzip() is not None:
            raise SystemExit("archive CRC check failed")
        for binary in binaries:
            packed = archive.read(binary.name)
            if hashlib.sha256(packed).digest() != hashlib.sha256(binary.read_bytes()).digest():
                raise SystemExit(f"packed {binary.name} differs from the built binary")

    digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
    (args.out / f"{archive_path.name}.sha256").write_text(
        f"{digest}  {archive_path.name}\n", encoding="ascii"
    )
    print(
        json.dumps(
            {
                "archive": str(archive_path),
                "bytes": archive_path.stat().st_size,
                "sha256": digest,
                "version": expected,
                "verified": ["version", "channels", "alias-catalog", "crc", "binary-hash"],
            }
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
