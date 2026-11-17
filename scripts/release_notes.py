#!/usr/bin/env python3
"""Extract a release's notes from CHANGELOG.md and cross-check the version.

    python scripts/release_notes.py --changelog CHANGELOG.md --tag v0.1.0 --out notes.md
    python scripts/release_notes.py --changelog CHANGELOG.md --tag v0.1.0 --check-version Cargo.toml

The tag must look like `vX.Y.Z`. `--check-version` additionally requires the
workspace manifest to declare the same version, so a mistyped tag fails before
a release is published. An empty or missing changelog section is an error: a
release with no notes is worse than no release.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

HEADING = re.compile(r"^##\s+\[?v?(\d+\.\d+\.\d+)\]?", re.MULTILINE)


def tag_version(tag: str) -> str:
    match = re.fullmatch(r"v(\d+\.\d+\.\d+)", tag.strip())
    if not match:
        raise SystemExit(f"tag {tag!r} is not of the form vX.Y.Z")
    return match.group(1)


def manifest_version(path: Path) -> str:
    text = path.read_text(encoding="utf-8")
    section = text.split("[workspace.package]", 1)
    if len(section) != 2:
        raise SystemExit(f"{path} has no [workspace.package] section")
    match = re.search(r'(?m)^\s*version\s*=\s*"([^"]+)"', section[1])
    if not match:
        raise SystemExit(f"{path} [workspace.package] has no version")
    return match.group(1)


def section(changelog: str, version: str) -> str:
    match = HEADING.search(changelog)
    if not match:
        raise SystemExit("CHANGELOG.md has no version sections")
    heading_starts = [m for m in HEADING.finditer(changelog)]
    for index, candidate in enumerate(heading_starts):
        if candidate.group(1) == version:
            start = candidate.start()
            end = (
                heading_starts[index + 1].start()
                if index + 1 < len(heading_starts)
                else len(changelog)
            )
            body = changelog[start:end].strip()
            if not body:
                raise SystemExit(f"CHANGELOG.md section {version} is empty")
            return body + "\n"
    raise SystemExit(f"CHANGELOG.md has no section for {version}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--changelog", type=Path, default=Path("CHANGELOG.md"))
    parser.add_argument("--tag", required=True)
    parser.add_argument("--out", type=Path, default=None)
    parser.add_argument("--check-version", type=Path, default=None)
    args = parser.parse_args()

    version = tag_version(args.tag)
    if args.check_version:
        declared = manifest_version(args.check_version)
        if declared != version:
            raise SystemExit(
                f"tag {args.tag!r} does not match {args.check_version} version {declared!r}"
            )
    notes = section(args.changelog.read_text(encoding="utf-8"), version)
    if args.out:
        args.out.write_text(notes, encoding="utf-8")
    else:
        sys.stdout.write(notes)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
