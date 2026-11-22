"""Offline installer tests; only the HTTPS transport is replaced by a fixture."""

import contextlib
import hashlib
import importlib.util
import io
import os
import shlex
import stat
import tempfile
import unittest
import zipfile
from email.message import Message
from http.client import IncompleteRead
from pathlib import Path
from unittest.mock import patch
from urllib.request import BaseHandler, build_opener
from urllib.response import addinfourl

SCRIPT = Path(__file__).resolve().parents[1] / "install.py"
VERSION = "v0.2.0"
RELEASE = f"https://github.com/mevarx/Arana/releases/download/{VERSION}"
ASSET = "arana-windows-x64.zip"
BINARY = "arana.exe"
PAYLOAD = b"deterministic Arana executable fixture"
CDN = "https://release-assets.githubusercontent.com/fixture"
ResponseData = tuple[int, dict[str, str], bytes | io.BytesIO]


def zip_bytes(*members):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in members:
            info = name if isinstance(name, zipfile.ZipInfo) else zipfile.ZipInfo(name)
            archive.writestr(info, data, compress_type=zipfile.ZIP_DEFLATED)
    return output.getvalue()


def ok(body: bytes) -> ResponseData:
    return 200, {"Content-Length": str(len(body))}, body


class UnreadableBody(io.BytesIO):
    def read(self, size: int | None = -1, /):
        raise AssertionError("the response body must not be read")

    read1 = read


class TruncatedBody(io.BytesIO):
    def read(self, size: int | None = -1, /):
        raise IncompleteRead(b"", 2)

    read1 = read


def redirect(url: str) -> ResponseData:
    return 302, {"Location": url}, UnreadableBody()


class FixtureResponse(addinfourl):
    msg = "fixture response"


class FixtureHTTPS(BaseHandler):
    handler_order = 0  # Always handle requests before the real HTTPS transport.

    def __init__(self, responses):
        super().__init__()
        self.responses = responses
        self.requests = []

    def https_open(self, request):
        self.requests.append(request.full_url)
        status, fields, body = self.responses[request.full_url]
        headers = Message()
        for name, value in fields.items():
            headers[name] = value
        stream = io.BytesIO(body) if isinstance(body, bytes) else body
        return FixtureResponse(stream, headers, request.full_url, status)

    http_open = https_open


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue(SCRIPT.is_file(), "the explicit release installer is missing")
        spec = importlib.util.spec_from_file_location("arana_release_installer", SCRIPT)
        assert spec is not None and spec.loader is not None
        self.installer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.installer)
        self.scratch = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        self.destination = self.scratch / "bin with spaces"
        self.target = self.destination / BINARY

    def release_fixture(self, archive=None, checksum=None, asset=ASSET):
        if archive is None:
            archive = zip_bytes((BINARY, PAYLOAD))
        if checksum is None:
            checksum = f"{hashlib.sha256(archive).hexdigest()}  {asset}\n".encode("ascii")
        return {f"{RELEASE}/{asset}.sha256": ok(checksum), f"{RELEASE}/{asset}": ok(archive)}

    def run_installer(self, responses, args=None, *, system="Windows", machine="AMD64"):
        transport = FixtureHTTPS(responses)
        stdout, stderr = io.StringIO(), io.StringIO()
        if args is None:
            args = ["--version", VERSION, "--install-dir", str(self.destination)]
        with (
            patch.object(self.installer.platform, "system", return_value=system),
            patch.object(self.installer.platform, "machine", return_value=machine),
            patch.object(self.installer, "build_opener", side_effect=lambda *handlers: build_opener(transport, *handlers)),
            contextlib.redirect_stdout(stdout),
            contextlib.redirect_stderr(stderr),
        ):
            result = self.installer.main(args)
        return result, stdout.getvalue(), stderr.getvalue(), transport.requests

    def test_installs_only_the_primary_binary_from_the_pinned_release(self):
        archive = zip_bytes(
            (BINARY, PAYLOAD), ("agent-reach.exe", b"upstream alias"),
            ("README.md", b"docs"), ("../outside.txt", b"must not extract"),
        )
        original_path = os.environ.get("PATH")
        result, output, error, requests = self.run_installer(self.release_fixture(archive))
        self.assertEqual(result, 0, error)
        self.assertEqual(self.target.read_bytes(), PAYLOAD)
        self.assertCountEqual(self.destination.iterdir(), [self.target])
        self.assertFalse((self.scratch / "outside.txt").exists())
        self.assertIn(str(self.target), output)
        self.assertIn("PATH", output)
        self.assertEqual(os.environ.get("PATH"), original_path)
        self.assertEqual(requests, [f"{RELEASE}/{ASSET}.sha256", f"{RELEASE}/{ASSET}"])

    def test_selects_the_published_asset_and_run_command_for_each_supported_platform(self):
        for system, machine, asset, binary in (
            ("Windows", "AMD64", "arana-windows-x64.zip", "arana.exe"),
            ("Linux", "x86_64", "arana-linux-x64.zip", "arana"),
            ("Darwin", "arm64", "arana-macos-arm64.zip", "arana"),
            ("Darwin", "aarch64", "arana-macos-arm64.zip", "arana"),
        ):
            with self.subTest(system=system, machine=machine):
                dest = self.scratch / f"{machine}'s bin"
                archive = zip_bytes((binary, PAYLOAD))
                result, output, error, requests = self.run_installer(
                    self.release_fixture(archive, asset=asset),
                    ["--version", VERSION, "--install-dir", str(dest)],
                    system=system, machine=machine,
                )
                self.assertEqual(result, 0, error)
                path = dest / binary
                self.assertEqual(path.read_bytes(), PAYLOAD)
                self.assertEqual(requests, [f"{RELEASE}/{asset}.sha256", f"{RELEASE}/{asset}"])
                if system == "Windows":
                    quoted = str(path).replace("'", "''")
                    self.assertIn(f"& '{quoted}' version", output)
                else:
                    self.assertIn(f"{shlex.quote(str(path))} version", output)
                    if os.name == "posix":
                        self.assertTrue(os.access(path, os.X_OK))

    def test_rejects_unsupported_platforms_before_downloading_or_writing(self):
        for system, machine in (("Windows", "ARM64"), ("Linux", "aarch64"), ("Darwin", "x86_64"), ("FreeBSD", "x86_64")):
            with self.subTest(system=system, machine=machine):
                result, _, error, requests = self.run_installer({}, system=system, machine=machine)
                self.assertNotEqual(result, 0)
                self.assertIn(f"{system} {machine}", error)
                self.assertIn("Windows x64, Linux x64, and macOS ARM64", error)
                self.assertEqual(requests, [])
                self.assertFalse(self.destination.exists())

    def test_cli_requires_an_explicit_destination_and_literal_version_pin(self):
        for args in (
            [], ["--version", VERSION], ["--install-dir", str(self.destination)],
            ["--version", VERSION, "--install-dir", ""],
            *(["--version", tag, "--install-dir", str(self.destination)] for tag in (
                "latest", "0.2.0", "v0.2.0/../latest", "v0.2.0?asset=other", "v0.2.0\n", "v0.02.0",
            )),
        ):
            with self.subTest(args=args):
                with self.assertRaises(SystemExit) as stopped:
                    self.run_installer({}, args)
                self.assertEqual(stopped.exception.code, 2)
        self.assertFalse(self.destination.exists())

    def test_refuses_existing_binary_before_any_download(self):
        self.destination.mkdir()
        self.target.write_bytes(b"user-owned executable")
        result, _, error, requests = self.run_installer(self.release_fixture())
        self.assertNotEqual(result, 0)
        self.assertIn("already exists", error)
        self.assertIn("choose a different --install-dir", error.lower())
        self.assertEqual(requests, [])
        self.assertEqual(self.target.read_bytes(), b"user-owned executable")
        self.assertCountEqual(self.destination.iterdir(), [self.target])

    def test_refuses_dangling_symlink_as_an_existing_target(self):
        self.destination.mkdir()
        missing = self.scratch / "missing-target"
        try:
            self.target.symlink_to(missing)
        except OSError as error:
            if getattr(error, "winerror", None) == 1314:
                self.skipTest("this Windows user cannot create symlinks")
            raise
        self.assertTrue(self.target.is_symlink())
        result, _, error, requests = self.run_installer(self.release_fixture())
        self.assertNotEqual(result, 0)
        self.assertIn("already exists", error)
        self.assertEqual(requests, [])
        self.assertTrue(self.target.is_symlink())
        self.assertFalse(missing.exists())

    def test_publish_refuses_to_overwrite_a_file_created_during_the_download(self):
        self.destination.mkdir()
        target = self.target
        unrelated = b"other install completed during download"

        class NewTargetBody(io.BytesIO):
            def read1(self, size: int | None = -1, /):
                if not target.exists():
                    target.write_bytes(unrelated)
                return super().read1(size)

        responses = self.release_fixture()
        url = f"{RELEASE}/{ASSET}"
        code, headers, body = responses[url]
        assert isinstance(body, bytes)
        responses[url] = code, headers, NewTargetBody(body)
        result, _, error, requests = self.run_installer(responses)
        self.assertNotEqual(result, 0)
        self.assertIn("choose a different --install-dir", error.lower())
        self.assertEqual(requests, [f"{url}.sha256", url])
        self.assertEqual(self.target.read_bytes(), unrelated)
        self.assertCountEqual(self.destination.iterdir(), [self.target])

    def test_mismatched_checksum_leaves_no_install(self):
        checksum = f"{'0' * 64}  {ASSET}\n".encode("ascii")
        result, _, error, _ = self.run_installer(self.release_fixture(b"must not parse as a ZIP", checksum))
        self.assertNotEqual(result, 0)
        self.assertIn("SHA256", error)
        self.assertFalse(self.destination.exists())

    def test_rejects_sidecars_that_do_not_name_exactly_the_selected_archive(self):
        sidecar = f"{RELEASE}/{ASSET}.sha256"
        for checksum in (
            f"{'a' * 64}  arana-linux-x64.zip\n".encode("ascii"),
            f"{'z' * 64}  {ASSET}\n".encode("ascii"),
            f"{'a' * 64}  {ASSET}\n{'b' * 64}  {ASSET}\n".encode("ascii"),
            b"\xff",
        ):
            with self.subTest(checksum=checksum):
                result, _, error, requests = self.run_installer(self.release_fixture(checksum=checksum))
                self.assertNotEqual(result, 0)
                self.assertIn("SHA256", error)
                self.assertEqual(requests, [sidecar])
                self.assertFalse(self.destination.exists())

    def test_rejects_oversized_archive_from_content_length_before_reading_it(self):
        responses = self.release_fixture()
        responses[f"{RELEASE}/{ASSET}"] = (200, {"Content-Length": "25"}, UnreadableBody())
        with patch.object(self.installer, "ARCHIVE_LIMIT", 24):
            result, _, error, _ = self.run_installer(responses)
        self.assertNotEqual(result, 0)
        self.assertIn("limit", error)
        self.assertFalse(self.destination.exists())

    def test_caps_actual_archive_and_sidecar_bytes_even_without_content_length(self):
        for asset, limit in ((ASSET, "ARCHIVE_LIMIT"), (f"{ASSET}.sha256", "CHECKSUM_LIMIT")):
            with self.subTest(asset=asset):
                responses = self.release_fixture()
                responses[f"{RELEASE}/{asset}"] = (200, {}, b"a" * 25)
                with patch.object(self.installer, limit, 24):
                    result, _, error, _ = self.run_installer(responses)
                self.assertNotEqual(result, 0)
                self.assertIn(f"{asset} exceeds the 24-byte limit", error)
                self.assertFalse(self.destination.exists())

    def test_rejects_incomplete_downloads_or_partial_http_responses(self):
        for code, headers, payload, diagnostic in (
            (200, {"Content-Length": "3"}, b"xy", "length"),
            (200, {"Content-Length": "invalid"}, b"xy", "length"),
            (200, {}, TruncatedBody(), "download"),
            (206, {}, b"xy", "HTTP 206"),
        ):
            with self.subTest(code=code, headers=headers, payload=payload):
                responses = self.release_fixture()
                responses[f"{RELEASE}/{ASSET}"] = code, headers, payload
                result, _, error, _ = self.run_installer(responses)
                self.assertNotEqual(result, 0)
                self.assertIn(diagnostic, error)
                self.assertFalse(self.destination.exists())

    def test_aborts_a_response_that_exceeds_the_download_deadline(self):
        with patch.object(self.installer, "monotonic", side_effect=[0, 10000], create=True):
            result, _, error, requests = self.run_installer(self.release_fixture())
        self.assertNotEqual(result, 0)
        self.assertIn("timed out", error)
        self.assertEqual(requests, [f"{RELEASE}/{ASSET}.sha256"])
        self.assertFalse(self.destination.exists())

    def test_requires_exactly_one_literal_root_binary(self):
        with self.assertWarnsRegex(UserWarning, "Duplicate name"):
            duplicate = zip_bytes((BINARY, PAYLOAD), (BINARY, PAYLOAD))
        wrapped = zip_bytes((f"bin/{BINARY}", PAYLOAD))
        nul_suffixed = zip_bytes((f"{BINARY}-extra", PAYLOAD)).replace(
            f"{BINARY}-extra".encode("ascii"), f"{BINARY}\0extra".encode("ascii"),
        )
        for name, archive in (("duplicate", duplicate), ("wrapped", wrapped), ("NUL suffix", nul_suffixed)):
            with self.subTest(name=name):
                dest = self.scratch / name
                args = ["--version", VERSION, "--install-dir", str(dest)]
                result, _, error, _ = self.run_installer(self.release_fixture(archive), args)
                self.assertNotEqual(result, 0)
                self.assertIn(f"exactly one top-level {BINARY}", error)
                self.assertFalse(dest.exists())

    def test_refuses_symlink_or_directory_metadata_on_the_binary(self):
        for mode in (stat.S_IFLNK | 0o777, stat.S_IFDIR | 0o755):
            with self.subTest(mode=mode):
                info = zipfile.ZipInfo(BINARY)
                info.create_system = 3
                info.external_attr = mode << 16
                archive = zip_bytes((info, PAYLOAD))
                dest = self.scratch / str(mode)
                args = ["--version", VERSION, "--install-dir", str(dest)]
                result, _, error, _ = self.run_installer(self.release_fixture(archive), args)
                self.assertNotEqual(result, 0)
                self.assertIn("regular file", error)
                self.assertFalse(dest.exists())

    def test_checks_decompressed_binary_size_before_opening_the_member(self):
        responses = self.release_fixture(zip_bytes((BINARY, b"x" * 17)))
        with (
            patch.object(self.installer, "BINARY_LIMIT", 16),
            patch.object(zipfile.ZipFile, "open", side_effect=AssertionError("oversized member was opened")),
        ):
            result, _, error, _ = self.run_installer(responses)
        self.assertNotEqual(result, 0)
        self.assertIn(f"{BINARY} exceeds the 16-byte limit", error)
        self.assertFalse(self.destination.exists())

    def test_rejects_corrupt_binary_even_when_archive_matches_its_sha256(self):
        output = io.BytesIO()
        with zipfile.ZipFile(output, "w") as archive:
            archive.writestr(zipfile.ZipInfo(BINARY), PAYLOAD, compress_type=zipfile.ZIP_STORED)
        corrupt = output.getvalue().replace(PAYLOAD, b"x" * len(PAYLOAD), 1)
        self.assertNotEqual(corrupt, output.getvalue())
        result, _, error, _ = self.run_installer(self.release_fixture(corrupt))
        self.assertNotEqual(result, 0)
        self.assertIn("CRC", error)
        self.assertFalse(self.destination.exists())

    def test_follows_one_official_asset_redirect_per_download_without_reading_redirect_bodies(self):
        responses = self.release_fixture()
        requests_in_order = []
        for official in tuple(responses):
            cdn = f"{CDN}/{official.rsplit('/', 1)[1]}?fixture=query"
            responses[cdn] = responses[official]
            responses[official] = redirect(cdn)
            requests_in_order.extend([official, cdn])
        result, _, error, requests = self.run_installer(responses)
        self.assertEqual(result, 0, error)
        self.assertEqual(requests, requests_in_order)
        self.assertEqual(self.target.read_bytes(), PAYLOAD)

    def test_rejects_untrusted_redirects_before_making_a_request(self):
        sidecar = f"{RELEASE}/{ASSET}.sha256"
        for url in (
            "https://example.invalid/asset", "http://release-assets.githubusercontent.com/asset",
            "https://release-assets.githubusercontent.com.example.invalid/asset",
            "https://fixture@release-assets.githubusercontent.com/asset",
            "https://release-assets.githubusercontent.com:8443/asset",
            "https://github.com/another-owner/another-repo/releases/download/v0.2.0/asset",
            "/relative-asset", "https://[malformed",
        ):
            with self.subTest(url=url):
                result, _, error, requests = self.run_installer({sidecar: redirect(url)})
                self.assertNotEqual(result, 0)
                self.assertIn("unexpected release asset redirect", error.lower())
                self.assertEqual(requests, [sidecar])
                self.assertFalse(self.destination.exists())

    def test_does_not_follow_a_second_redirect_from_the_asset_cdn(self):
        sidecar = f"{RELEASE}/{ASSET}.sha256"
        result, _, error, requests = self.run_installer({sidecar: redirect(CDN), CDN: redirect(f"{CDN}/other")})
        self.assertNotEqual(result, 0)
        self.assertIn("second release asset redirect", error.lower())
        self.assertEqual(requests, [sidecar, CDN])
        self.assertFalse(self.destination.exists())

    def test_reports_missing_asset_without_exposing_cdn_query(self):
        sidecar = f"{RELEASE}/{ASSET}.sha256"
        signed = f"{CDN}?fixture=example"
        result, _, error, requests = self.run_installer({sidecar: redirect(signed), signed: (404, {}, b"")})
        self.assertNotEqual(result, 0)
        self.assertIn("HTTP 404", error)
        self.assertIn(ASSET, error)
        self.assertIn("release", error.lower())
        self.assertNotIn(signed, error)
        self.assertEqual(requests, [sidecar, signed])


if __name__ == "__main__":
    unittest.main()
