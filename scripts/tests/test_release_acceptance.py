import json
import sys
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from check_release_acceptance import FROZEN_REQUIRED, UPSTREAM_WAIVERS, validate
from record_release_acceptance import probes, decode

DIGEST = "a" * 64
REQUIRED = sorted(FROZEN_REQUIRED)

def goodrow(key):
    return {"channel": key[0], "action": key[1], "method": "real-cli", "backend": "fixture", "item_count": 1, "at": datetime.now(timezone.utc).isoformat(), "os": "Windows", "source_sha256": DIGEST, "passed": True, "error": None}

def goodmanifest():
    waived = [{"channel": c, "action": a, "reason": reason} for (c, a), reason in UPSTREAM_WAIVERS.items()]
    live = [k for k in REQUIRED if k not in UPSTREAM_WAIVERS]
    return {"source_sha256": DIGEST, "release_ready": True, "pending": [], "waived": waived, "results": [goodrow(k) for k in live]}

class AcceptanceTest(unittest.TestCase):
    def test_34_real_results_and_exact_six_waivers_pass(self):
        self.assertEqual(validate(goodmanifest(), REQUIRED, DIGEST), [])

    def test_failure_is_not_promoted_to_pass_or_waiver(self):
        data = goodmanifest(); data["results"][0]["passed"] = False; data["results"][0]["error"] = "NEED_LOGIN"
        self.assertTrue(any("failed action" in e for e in validate(data, REQUIRED, DIGEST)))

    def test_rejects_duplicate_waiver_and_result(self):
        data = goodmanifest(); data["waived"].append(dict(data["waived"][0])); data["results"].append(dict(data["results"][0]))
        errors = validate(data, REQUIRED, DIGEST)
        self.assertTrue(any("Duplicate waiver" in e for e in errors)); self.assertTrue(any("Duplicate acceptance result" in e for e in errors))

    def test_rejects_stale_nonpositive_wrong_provenance_and_source(self):
        for mutate in (
            lambda r: r.update(at=(datetime.now(timezone.utc)-timedelta(days=8)).isoformat()),
            lambda r: r.update(item_count=True), lambda r: r.update(method="fixture"),
            lambda r: r.update(source_sha256="b"*64), lambda r: r.update(backend=""), lambda r: r.update(os=""),
        ):
            data = goodmanifest(); mutate(data["results"][0]); self.assertTrue(validate(data, REQUIRED, DIGEST))

    def test_unknown_waiver_reason_and_pending_block(self):
        data = goodmanifest(); data["waived"][0]["reason"] = "test"; data["pending"] = [{"action": "x"}]
        errors = validate(data, REQUIRED, DIGEST)
        self.assertTrue(any("Waiver is not" in e for e in errors)); self.assertTrue(any("pending" in e for e in errors))

    def test_recorder_input_extracts_upstream_frozen_argv(self):
        result = probes(ROOT / "tests/golden/manifest.json")
        self.assertEqual(result[("web", "search")][:1], ["yt-dlp github repository"])
        self.assertEqual(len(result), 40)

    def test_malformed_pairs_and_timestamps_are_rejected_without_exceptions(self):
        for section,field,value in (("results","channel",[]),("waived","action",{}),("results","at",1234),("results","at","2026-11-17T12:00:00")):
            with self.subTest(section=section,field=field,value=value):
                data=goodmanifest();data[section][0][field]=value
                self.assertTrue(validate(data,REQUIRED,DIGEST))

    def test_invalid_current_source_digest_is_rejected(self):
        data=goodmanifest();data["source_sha256"]="not-a-sha256"
        for row in data["results"]:row["source_sha256"]="not-a-sha256"
        self.assertTrue(validate(data,REQUIRED,"not-a-sha256"))

    def test_decoder_requires_json_object(self):
        self.assertEqual(decode('{"ok":true}')["ok"], True)
        self.assertEqual(decode("not json"), {})

if __name__ == "__main__": unittest.main()
