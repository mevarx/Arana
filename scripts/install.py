#!/usr/bin/env python3
"""Install only Arana's CLI from a pinned GitHub release (Python 3.11+)."""

from __future__ import annotations

import argparse
import hashlib
import io
import os
import platform
import re
import shlex
import shutil
import stat
import sys
import tempfile
import zipfile
import zlib
from http.client import HTTPException, HTTPResponse
from pathlib import Path
from time import monotonic
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, ProxyHandler, build_opener

RELEASES = "https://github.com/mevarx/Arana/releases/download"
ASSET_CDN = "https://release-assets.githubusercontent.com/"
REDIRECTS = (301, 302, 303, 307, 308)
CHECKSUM_LIMIT = 1024
ARCHIVE_LIMIT = 32 * 1024 * 1024
BINARY_LIMIT = 64 * 1024 * 1024
CHUNK_SIZE = 64 * 1024
NETWORK_TIMEOUT = 30
DOWNLOAD_SECONDS = 180
ARCHITECTURES = {"x86_64": "x64", "amd64": "x64", "arm64": "arm64", "aarch64": "arm64"}
ASSETS = {
    ("Windows", "x64"): ("arana-windows-x64.zip", "arana.exe"),
    ("Linux", "x64"): ("arana-linux-x64.zip", "arana"),
    ("Darwin", "arm64"): ("arana-macos-arm64.zip", "arana"),
}


class InstallError(Exception):
    pass


class NoRedirect(HTTPRedirectHandler):
    # Don't let urllib resolve the Location or drain the redirect body.
    def http_error_302(self, req, fp, code, msg, headers):
        raise HTTPError(req.full_url, code, msg, headers, fp)

    http_error_301 = http_error_303 = http_error_307 = http_error_308 = http_error_302


def pinned_tag(value: str) -> str:
    if not re.fullmatch(r"v(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){2}", value):
        raise argparse.ArgumentTypeError("expected an exact release tag like v0.2.0")
    return value


def release_asset(system: str, machine: str) -> tuple[str, str]:
    asset = ASSETS.get((system, ARCHITECTURES.get(machine.lower(), "")))
    if asset is None:
        raise InstallError(
            f"No release archive for {system} {machine}. "
            "Supported targets: Windows x64, Linux x64, and macOS ARM64."
        )
    return asset


def copy_bounded(read, output, limit: int, label: str) -> int:
    total = 0
    while block := read(min(CHUNK_SIZE, limit - total + 1)):
        total += len(block)
        if total > limit:
            raise InstallError(f"{label} exceeds the {limit}-byte limit.")
        output.write(block)
    return total


def open_asset(url: str, asset: str) -> HTTPResponse:
    opener = build_opener(NoRedirect(), ProxyHandler({}))
    redirected = False
    while True:
        try:
            return opener.open(url, timeout=NETWORK_TIMEOUT)
        except HTTPError as error:
            with error:
                if error.code not in REDIRECTS:
                    raise InstallError(
                        f"GitHub returned HTTP {error.code} for {asset}. "
                        "Check that the selected release publishes this asset."
                    ) from None
                if redirected:
                    raise InstallError(f"Unexpected second release asset redirect for {asset}.") from None
                locations = error.headers.get_all("Location", []) if error.headers else []
                if len(locations) == 1:
                    location = locations[0]
                    if location.startswith(ASSET_CDN) and location.isascii() and not re.search(r"[\x00-\x20\x7f\\#]", location):
                        url = location
                        redirected = True
                        continue
                raise InstallError(f"Unexpected release asset redirect for {asset}.") from None


def download(url: str, output, limit: int, asset: str) -> None:
    deadline = monotonic() + DOWNLOAD_SECONDS
    try:
        with open_asset(url, asset) as response:
            if response.status != 200:
                raise InstallError(f"Expected a complete download of {asset}; got HTTP {response.status}.")
            lengths = response.headers.get_all("Content-Length", [])
            expected = None
            if lengths:
                length = lengths[0].strip(" \t")
                if len(lengths) != 1 or not re.fullmatch(r"[0-9]{1,20}", length):
                    raise InstallError(f"Invalid download length for {asset}.")
                expected = int(length)
                if expected > limit:
                    raise InstallError(f"{asset} exceeds the {limit}-byte limit.")

            def read_chunk(size: int) -> bytes:
                # read1 avoids filling the whole buffer before checking the deadline.
                block = response.read1(size)
                if monotonic() >= deadline:
                    raise InstallError(f"Download of {asset} timed out. Check your connection and retry.")
                return block

            received = copy_bounded(read_chunk, output, limit, asset)
            if expected is not None and received != expected:
                raise InstallError(f"Download length does not match for {asset}; retry the install.")
    except (URLError, HTTPException, TimeoutError, ConnectionError) as error:
        reason = error.reason if isinstance(error, URLError) else error
        raise InstallError(
            f"Could not download {asset} from GitHub ({type(reason).__name__}). "
            "Check your connection and retry."
        ) from None


def read_checksum(base_url: str, asset: str) -> str:
    data = io.BytesIO()
    sidecar = f"{asset}.sha256"
    download(f"{base_url}/{sidecar}", data, CHECKSUM_LIMIT, sidecar)
    match = re.fullmatch(rb"([0-9a-fA-F]{64})  " + re.escape(asset.encode("ascii")) + rb"(?:\r?\n)?", data.getvalue())
    if match is None:
        raise InstallError(f"{sidecar} must contain one SHA256 digest for {asset}.")
    return match[1].decode("ascii").lower()


def extract_binary(archive_path: Path, binary: Path) -> None:
    try:
        with zipfile.ZipFile(archive_path) as archive:
            matches = [member for member in archive.infolist() if member.filename == binary.name]
            if len(matches) != 1 or matches[0].orig_filename != binary.name:
                raise InstallError(f"{archive_path.name} must contain exactly one top-level {binary.name}.")
            member = matches[0]
            file_type = stat.S_IFMT(member.external_attr >> 16)
            if file_type not in (0, stat.S_IFREG) or member.external_attr & 0x10:
                raise InstallError(f"{binary.name} is not a regular file in {archive_path.name}.")
            if member.flag_bits & 1 or member.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED):
                raise InstallError(f"{binary.name} uses unsupported ZIP compression or encryption.")
            if member.file_size > BINARY_LIMIT:
                raise InstallError(f"{binary.name} exceeds the {BINARY_LIMIT}-byte limit.")
            if member.file_size == 0:
                raise InstallError(f"{binary.name} is empty in {archive_path.name}.")

            # Ignore every other member; never use member names as output paths.
            with archive.open(member) as source, binary.open("xb") as output:
                size = copy_bounded(source.read, output, BINARY_LIMIT, binary.name)
            if size != member.file_size:
                raise InstallError(f"{binary.name} has the wrong extracted size.")
    except (zipfile.BadZipFile, EOFError, UnicodeError, zlib.error) as error:
        raise InstallError(f"Invalid ZIP {archive_path.name}: {error}.") from None


def occupied_target(target: Path) -> InstallError:
    return InstallError(f"{target} already exists; left it untouched. Choose a different --install-dir.")


def publish(binary: Path, target: Path) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    # Stage on the destination filesystem, then publish atomically without replace.
    with tempfile.TemporaryDirectory(prefix=".arana-install-", dir=target.parent) as stage:
        staged = Path(stage) / binary.name
        shutil.copyfile(binary, staged)
        staged.chmod(0o755)
        try:
            if os.name == "nt":
                # Windows rename fails if the destination already exists.
                os.rename(staged, target)
            else:
                os.link(staged, target)
        except FileExistsError:
            raise occupied_target(target) from None
        except OSError as error:
            raise InstallError(
                f"Could not publish {target}: {error.strerror or error}. "
                "Choose a writable --install-dir on a local filesystem."
            ) from None


def install(version: str, directory: Path, system: str, machine: str) -> Path:
    asset, binary = release_asset(system, machine)
    base_url = f"{RELEASES}/{version}"
    try:
        target = directory.expanduser().resolve() / binary
    except (RuntimeError, ValueError) as error:
        raise InstallError(f"Cannot resolve --install-dir: {error}. Choose an absolute directory.") from None
    try:
        target.lstat()  # Refuse any occupied leaf, even a dangling symlink.
    except FileNotFoundError:
        pass
    else:
        raise occupied_target(target)

    with tempfile.TemporaryDirectory(prefix="arana-install-") as scratch:
        expected = read_checksum(base_url, asset)
        archive_path = Path(scratch) / asset
        with archive_path.open("xb") as output:
            download(f"{base_url}/{asset}", output, ARCHIVE_LIMIT, asset)
        with archive_path.open("rb") as archive:
            if hashlib.file_digest(archive, "sha256").hexdigest() != expected:
                raise InstallError(f"SHA256 mismatch for {asset}; nothing was installed.")
        unpacked = Path(scratch) / binary
        extract_binary(archive_path, unpacked)
        publish(unpacked, target)
    return target


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        epilog="Does not run Arana, install its optional tools, or edit PATH. Existing binaries are left untouched.",
        allow_abbrev=False,
    )
    parser.add_argument("--version", required=True, type=pinned_tag, help="exact release tag, e.g. v0.2.0")
    parser.add_argument("--install-dir", required=True, metavar="DIR", help="directory for the new arana binary")
    args = parser.parse_args(argv)
    if not args.install_dir:
        parser.error("--install-dir cannot be empty")
    system = platform.system()
    try:
        binary = install(args.version, Path(args.install_dir), system, platform.machine())
    except (InstallError, OSError) as error:
        print(f"Install failed: {error}", file=sys.stderr)
        return 1
    if system == "Windows":
        quoted = str(binary).replace("'", "''")
        command = f"& '{quoted}'"
    else:
        command = shlex.quote(str(binary))
    print(f"Installed Arana {args.version}: {binary}")
    shell = " (PowerShell)" if system == "Windows" else ""
    print(f"Run by absolute path{shell}: {command} version")
    print(f"To run by name, add this directory to your user PATH: {binary.parent}")
    print("PATH was not changed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
