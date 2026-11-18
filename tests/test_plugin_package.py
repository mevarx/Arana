"""Validate the shared skill layout and every shipped plugin manifest."""

import json
import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import ci_package  # noqa: E402
import package_release  # noqa: E402

AGENT_SCHEMA = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json"
ANTIGRAVITY_SCHEMA = "https://antigravity.google/schemas/v1/plugin.json"
PLUGIN_FILES = {
    "plugin.json",
    ".claude-plugin/plugin.json",
    ".claude-plugin/marketplace.json",
    ".agents/plugins/marketplace.json",
    ".antigravity-plugin/plugin.json",
}
PACKAGE_FILES = PLUGIN_FILES | {"skills/arana/SKILL.md"}
NAME_RE = re.compile(r"^[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$")


def read_json(relative):
    return json.loads((ROOT / relative).read_text(encoding="utf-8"))


def frontmatter(text):
    match = re.match(r"^---\n(.*?)\n---\n", text, re.DOTALL)
    return match.group(1) if match else ""


class SkillLayoutTest(unittest.TestCase):
    def test_canonical_skill_is_relocated(self):
        self.assertTrue((ROOT / "skills/arana/SKILL.md").is_file())
        self.assertFalse(
            (ROOT / "SKILL.md").exists(),
            "the canonical skill must live only at skills/arana/SKILL.md",
        )

    def test_skill_frontmatter_and_actionable_setup_guidance(self):
        text = (ROOT / "skills/arana/SKILL.md").read_text(encoding="utf-8")
        header = frontmatter(text)
        self.assertIn("name: arana", header)
        self.assertRegex(header, r"(?m)^description:\s*\S")
        self.assertRegex(header, r'(?m)^\s+version:\s*"?' + re.escape(
            ci_package.manifest_version()
        ) + r'"?$')
        # The skill must tell the agent how to establish a compatible binary,
        # where to get the matching release, and how to verify it.
        self.assertIn("arana version", text)
        self.assertIn("https://github.com/mevarx/Arana/releases", text)
        self.assertIn(".sha256", text)
        self.assertIn("SHA256SUMS", text)
        # Setup is human-driven: the agent never downloads or installs.
        self.assertRegex(text, r"(?i)do not download, install, or update")
        self.assertNotRegex(text, r"(?i)curl[^\n]*\|\s*sh")


class AgentPluginsManifestTest(unittest.TestCase):
    def test_portable_manifest_uses_the_closed_schema(self):
        manifest = read_json("plugin.json")
        self.assertEqual(manifest["$schema"], AGENT_SCHEMA)
        self.assertEqual(
            set(manifest),
            {
                "$schema",
                "name",
                "version",
                "description",
                "author",
                "homepage",
                "repository",
                "license",
                "keywords",
            },
        )
        self.assertEqual(manifest["name"], "arana")
        self.assertRegex(manifest["name"], NAME_RE)
        self.assertEqual(manifest["version"], ci_package.manifest_version())
        self.assertEqual(set(manifest["author"]), {"name", "url"})
        self.assertTrue(manifest["homepage"].startswith("https://"))
        self.assertTrue(manifest["repository"].startswith("https://github.com/"))
        self.assertEqual(manifest["license"], "MIT")

    def test_package_scripts_include_the_complete_plugin(self):
        self.assertEqual(set(ci_package.PACKAGE_FILES), PACKAGE_FILES)
        self.assertEqual(set(package_release.PACKAGE_FILES), PACKAGE_FILES)
        self.assertEqual(
            set(ci_package.OPTIONAL_FILES), set(package_release.OPTIONAL_FILES)
        )
        for relative in PACKAGE_FILES:
            self.assertTrue((ROOT / relative).is_file(), relative)
        # MCP configs are added by the MCP integration; once present they must
        # travel with the same package in both packaging paths.
        for relative in ci_package.OPTIONAL_FILES:
            if (ROOT / relative).is_file():
                self.assertIn(relative, ci_package.package_files())
                self.assertIn(relative, package_release.package_files())
        ci_package.verify_package(ci_package.manifest_version())


class ClaudePluginTest(unittest.TestCase):
    def test_manifest_and_marketplace_agree(self):
        manifest = read_json(".claude-plugin/plugin.json")
        self.assertEqual(manifest["name"], "arana")
        self.assertEqual(manifest["version"], ci_package.manifest_version())
        self.assertEqual(manifest["license"], "MIT")

        market = read_json(".claude-plugin/marketplace.json")
        self.assertRegex(market["name"], NAME_RE)
        self.assertTrue(market["owner"]["name"])
        self.assertEqual(len(market["plugins"]), 1)
        entry = market["plugins"][0]
        self.assertEqual(entry["name"], manifest["name"])
        self.assertEqual(entry["source"], "./")
        self.assertEqual(entry.get("version", manifest["version"]), manifest["version"])


class CodexMarketplaceTest(unittest.TestCase):
    def test_repo_marketplace_points_at_the_package(self):
        market = read_json(".agents/plugins/marketplace.json")
        self.assertEqual(market["name"], "arana")
        self.assertTrue(market["interface"]["displayName"])
        self.assertEqual(len(market["plugins"]), 1)
        entry = market["plugins"][0]
        self.assertEqual(entry["name"], "arana")
        self.assertEqual(entry["source"], {"source": "local", "path": "./"})
        self.assertEqual(
            entry["policy"],
            {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
        )
        self.assertTrue(entry["category"])
        resolved = (ROOT / entry["source"]["path"]).resolve()
        self.assertEqual(resolved, ROOT.resolve())


class AntigravityPluginTest(unittest.TestCase):
    def test_manifest_uses_the_antigravity_schema(self):
        manifest = read_json(".antigravity-plugin/plugin.json")
        allowed = {
            "$schema",
            "name",
            "displayName",
            "description",
            "version",
            "disabled",
            "logo",
            "suggestedPrompts",
            "skills",
            "author",
            "homepage",
            "repository",
            "license",
            "keywords",
        }
        self.assertEqual(set(manifest) - allowed, set())
        self.assertEqual(manifest["$schema"], ANTIGRAVITY_SCHEMA)
        self.assertEqual(manifest["name"], "arana")
        self.assertRegex(manifest["name"], NAME_RE)
        self.assertTrue(manifest["displayName"])
        self.assertEqual(manifest["version"], ci_package.manifest_version())
        self.assertTrue(manifest["author"]["url"].startswith("https://"))
        self.assertTrue(manifest["homepage"].startswith("https://"))
        self.assertTrue(manifest["repository"].startswith("https://github.com/"))


if __name__ == "__main__":
    unittest.main()
