#!/usr/bin/env python3
"""Verify a release bundle holds exactly one zip/sha256 pair per platform.

A published release is only trustworthy if the asset set is complete, so this
gate fails closed. Given a directory of downloaded assets it requires:

* exactly ``arana-linux-x64.zip``, ``arana-macos-arm64.zip`` and
  ``arana-windows-x64.zip`` plus one ``.sha256`` sidecar each
* no other archive or sidecar, so a fourth file cannot ride along unnoticed
* every sidecar is ``<sha256>  <its own archive name>`` in the same two-space
  form ``scripts/install.py`` verifies
* every archive opens, passes its CRC check, and carries the platform's two
  binaries plus ``plugin.json`` and the canonical skill
* ``plugin.json`` inside the archive declares the version of the tag the
  release is published under

Run it against the downloaded build artifacts before a draft is created, and
again before the draft is published. A non-zero exit means "do not publish".
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import zipfile
from pathlib import Path
from typing import Sequence

PLATFORMS = ("linux-x64", "macos-arm64", "windows-x64")
DOCUMENTS = ("plugin.json", "skills/arana/SKILL.md")
# Same shape scripts/install.py accepts: lowercase or uppercase hex, two
# spaces, the archive name, and an optional CRLF the Windows packer may leave.
SIDECAR = re.compile(r"([0-9a-fA-F]{64})  (\S+)(?:\r?\n)?\Z")
TAG = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z")


def archive_name(platform: str) -> str:
    return f"arana-{platform}.zip"


def sidecar_name(platform: str) -> str:
    return f"{archive_name(platform)}.sha256"


def binaries(platform: str) -> tuple[str, str]:
    """Return the CLI and its compatibility alias as packaged for a platform."""
    suffix = ".exe" if platform.startswith("windows-") else ""
    return f"arana{suffix}", f"agent-reach{suffix}"


def expected_names(platforms: Sequence[str]) -> list[str]:
    return [
        name
        for platform in platforms
        for name in (archive_name(platform), sidecar_name(platform))
    ]


def tag_version(tag: str) -> str:
    match = TAG.fullmatch(tag)
    if match is None:
        raise SystemExit(f"tag {tag!r} is not of the form vX.Y.Z")
    return tag[1:]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def check_sidecar(path: Path, archive: str, digest: str) -> list[str]:
    """Return why a sidecar is not a valid record of ``archive``."""
    try:
        text = path.read_text(encoding="ascii")
    except UnicodeDecodeError:
        return [f"{path.name} is not ASCII text"]
    match = SIDECAR.match(text)
    if match is None:
        return [f"{path.name} is not '<sha256>  <archive>' with a single line"]
    if match.group(2) != archive:
        return [f"{path.name} records {match.group(2)!r}, expected {archive!r}"]
    if match.group(1).lower() != digest:
        return [
            f"{path.name} records {match.group(1)}, "
            f"but {archive} hashes to {digest}"
        ]
    return []


def check_archive(path: Path, platform: str, version: str | None) -> list[str]:
    """Return why an archive is not the packaged build for ``platform``."""
    try:
        with zipfile.ZipFile(path) as bundle:
            corrupt = bundle.testzip()
            if corrupt is not None:
                return [f"{path.name} has a corrupt member {corrupt!r}"]
            packed = set(bundle.namelist())
            problems = [
                f"{path.name} is missing {member}"
                for member in (*binaries(platform), *DOCUMENTS)
                if member not in packed
            ]
            if version is not None and "plugin.json" in packed:
                try:
                    manifest = json.loads(bundle.read("plugin.json"))
                except ValueError as error:
                    problems.append(f"{path.name} plugin.json is not JSON: {error}")
                else:
                    declared = manifest.get("version")
                    if declared != version:
                        problems.append(
                            f"{path.name} plugin.json version {declared!r} "
                            f"does not match tag version {version!r}"
                        )
    except (zipfile.BadZipFile, OSError) as error:
        return [f"{path.name} is not a readable zip: {error}"]
    return problems


def verify(
    directory: Path,
    platforms: Sequence[str] = PLATFORMS,
    tag: str | None = None,
) -> dict:
    """Check the bundle and return a JSON-ready summary, or exit non-zero."""
    if not directory.is_dir():
        raise SystemExit(f"{directory} is not a directory")
    version = tag_version(tag) if tag is not None else None
    platforms = list(dict.fromkeys(platforms))
    wanted = expected_names(platforms)

    problems: list[str] = []
    present = sorted(path.name for path in directory.iterdir() if path.is_file())
    problems.extend(f"missing {name}" for name in wanted if name not in present)
    problems.extend(
        f"unexpected release file {name}"
        for name in present
        if (name.endswith(".zip") or name.endswith(".sha256")) and name not in wanted
    )

    files = []
    for platform in platforms:
        archive = directory / archive_name(platform)
        if not archive.is_file():
            continue
        digest = sha256(archive)
        files.append(
            {
                "platform": platform,
                "archive": archive.name,
                "bytes": archive.stat().st_size,
                "sha256": digest,
            }
        )
        sidecar = directory / sidecar_name(platform)
        if not sidecar.is_file():
            continue
        problems.extend(check_sidecar(sidecar, archive.name, digest))
        problems.extend(check_archive(archive, platform, version))

    if problems:
        raise SystemExit(
            "release assets are not publishable:\n- " + "\n- ".join(problems)
        )
    return {
        "directory": str(directory),
        "tag": tag,
        "version": version,
        "platforms": platforms,
        "files": files,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--dir",
        type=Path,
        required=True,
        help="directory holding the downloaded release assets",
    )
    parser.add_argument(
        "--tag",
        default=None,
        help="tag the assets will be published under; enables the version check",
    )
    parser.add_argument(
        "--platform",
        action="append",
        choices=PLATFORMS,
        default=None,
        help="platform to require (repeatable; defaults to the release contract)",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="print the expected file names and exit without checking anything",
    )
    args = parser.parse_args()

    platforms = tuple(args.platform) if args.platform else PLATFORMS
    if args.list:
        sys.stdout.write("\n".join(expected_names(dict.fromkeys(platforms))) + "\n")
        return 0

    summary = verify(args.dir, platforms, args.tag)
    print(json.dumps(summary))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
