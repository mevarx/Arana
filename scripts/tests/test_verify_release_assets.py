"""The release bundle gate: exactly three zip/sha256 pairs, nothing else."""

import json
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import verify_release_assets as gate  # noqa: E402


def write_archive(path, platform, version="0.3.0", members=None):
    binary, alias = gate.binaries(platform)
    names = list(members) if members is not None else [binary, alias, *gate.DOCUMENTS]
    with zipfile.ZipFile(path, "w") as bundle:
        for name in names:
            payload = b"binary"
            if name == "plugin.json":
                payload = json.dumps({"name": "arana", "version": version}).encode()
            bundle.writestr(name, payload)


def write_sidecar(path, archive, digest=None):
    path.write_text(f"{digest or gate.sha256(archive)}  {archive.name}\n", encoding="ascii")


def build_bundle(directory, platforms=gate.PLATFORMS, version="0.3.0"):
    for platform in platforms:
        archive = directory / gate.archive_name(platform)
        write_archive(archive, platform, version)
        write_sidecar(directory / gate.sidecar_name(platform), archive)


class ReleaseAssetsTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self._tmp.name)
        build_bundle(self.dir)

    def tearDown(self):
        self._tmp.cleanup()

    def assert_rejected(self, expected):
        with self.assertRaises(SystemExit) as caught:
            gate.verify(self.dir, tag="v0.3.0")
        self.assertIn(expected, str(caught.exception))

    def test_three_pairs_pass_and_report_every_digest(self):
        summary = gate.verify(self.dir, tag="v0.3.0")
        self.assertEqual(summary["version"], "0.3.0")
        self.assertEqual(
            [entry["archive"] for entry in summary["files"]],
            [gate.archive_name(platform) for platform in gate.PLATFORMS],
        )
        for entry in summary["files"]:
            self.assertEqual(len(entry["sha256"]), 64)
            self.assertGreater(entry["bytes"], 0)

    def test_the_expected_set_is_the_release_contract(self):
        self.assertEqual(
            sorted(gate.expected_names(gate.PLATFORMS)),
            sorted(
                [
                    "arana-linux-x64.zip",
                    "arana-linux-x64.zip.sha256",
                    "arana-macos-arm64.zip",
                    "arana-macos-arm64.zip.sha256",
                    "arana-windows-x64.zip",
                    "arana-windows-x64.zip.sha256",
                ]
            ),
        )

    def test_missing_sidecar_is_rejected(self):
        (self.dir / gate.sidecar_name("macos-arm64")).unlink()
        self.assert_rejected("missing arana-macos-arm64.zip.sha256")

    def test_extra_pair_is_rejected(self):
        stray = self.dir / "arana-linux-arm64.zip"
        write_archive(stray, "linux-arm64")
        write_sidecar(self.dir / "arana-linux-arm64.zip.sha256", stray)
        self.assert_rejected("unexpected release file arana-linux-arm64.zip")

    def test_archive_changed_after_the_sidecar_is_rejected(self):
        archive = self.dir / gate.archive_name("linux-x64")
        write_archive(archive, "linux-x64", members=["arana", "agent-reach"])
        self.assert_rejected("hashes to")

    def test_sidecar_naming_another_archive_is_rejected(self):
        sidecar = self.dir / gate.sidecar_name("windows-x64")
        sidecar.write_text(
            f"{gate.sha256(self.dir / gate.archive_name('windows-x64'))}  arana-linux-x64.zip\n",
            encoding="ascii",
        )
        self.assert_rejected("expected 'arana-windows-x64.zip'")

    def test_malformed_sidecars_are_rejected(self):
        sidecar = self.dir / gate.sidecar_name("linux-x64")
        for text in ("not-a-digest  arana-linux-x64.zip\n", "a" * 63 + "  arana-linux-x64.zip\n"):
            with self.subTest(text=text):
                sidecar.write_text(text, encoding="ascii")
                with self.assertRaises(SystemExit):
                    gate.verify(self.dir, tag="v0.3.0")

    def test_sidecar_with_a_second_line_is_rejected(self):
        sidecar = self.dir / gate.sidecar_name("linux-x64")
        digest = gate.sha256(self.dir / gate.archive_name("linux-x64"))
        sidecar.write_text(f"{digest}  arana-linux-x64.zip\n{digest}  arana-macos-arm64.zip\n", encoding="ascii")
        self.assert_rejected("single line")

    def test_archive_must_carry_the_platform_binaries(self):
        archive = self.dir / gate.archive_name("windows-x64")
        write_archive(archive, "linux-x64")
        write_sidecar(self.dir / gate.sidecar_name("windows-x64"), archive)
        self.assert_rejected("is missing arana.exe")

    def test_plugin_version_must_match_the_tag(self):
        archive = self.dir / gate.archive_name("macos-arm64")
        write_archive(archive, "macos-arm64", version="0.2.0")
        write_sidecar(self.dir / gate.sidecar_name("macos-arm64"), archive)
        self.assert_rejected("does not match tag version '0.3.0'")

    def test_unreadable_archive_is_rejected_without_a_traceback(self):
        archive = self.dir / gate.archive_name("linux-x64")
        archive.write_bytes(b"not a zip")
        write_sidecar(self.dir / gate.sidecar_name("linux-x64"), archive)
        self.assert_rejected("is not a readable zip")

    def test_empty_directory_reports_the_whole_set_missing(self):
        empty = Path(self._tmp.name) / "empty"
        empty.mkdir()
        with self.assertRaises(SystemExit) as caught:
            gate.verify(empty, tag="v0.3.0")
        message = str(caught.exception)
        for name in gate.expected_names(gate.PLATFORMS):
            self.assertIn(f"missing {name}", message)

    def test_tags_must_be_vX_Y_Z(self):
        for tag in ("0.3.0", "v0.3", "v0.3.0-rc1", "v00.3.0", "v0.3.0 "):
            with self.subTest(tag=tag):
                with self.assertRaises(SystemExit):
                    gate.verify(self.dir, tag=tag)

    def test_documented_sidecar_format_matches_the_installer_contract(self):
        # scripts/install.py accepts "<64 hex>  <asset>" optionally CRLF
        # terminated; the release gate must accept the same bytes.
        sidecar = self.dir / gate.sidecar_name("linux-x64")
        digest = gate.sha256(self.dir / gate.archive_name("linux-x64"))
        sidecar.write_bytes(f"{digest.upper()}  arana-linux-x64.zip\r\n".encode("ascii"))
        gate.verify(self.dir, tag="v0.3.0")


if __name__ == "__main__":
    unittest.main()
