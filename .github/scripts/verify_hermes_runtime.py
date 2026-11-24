#!/usr/bin/env python3
"""Exercise the installed Arana plugin through Hermes' own plugin/MCP runtime.

This is a pinned-version CI probe, not part of the shipped plugin. It checks the
four rungs that `hermes mcp test` cannot see for a portable package: plugin
discovery, skill discovery, MCP tool registration, and a real tool dispatch
through the Hermes tool registry. `shutdown_mcp_servers()` always runs.

    python .github/scripts/verify_hermes_runtime.py --binary-dir target/release
"""
from __future__ import annotations

import argparse
import json
import os
from typing import Any


EXPECTED_TOOLS = {
    "mcp__arana__list_capabilities",
    "mcp__arana__read_many",
    "mcp__arana__read_url",
    "mcp__arana__search",
}
PUBLIC_URL = "https://example.com"


def _payload(result: Any, depth: int = 0) -> dict:
    """Unwrap Hermes tool results into the tool's JSON object.

    Depending on the registry entry point the tool output can arrive as a JSON
    string, as ``{"result": "<json>"}``, or as one further wrapper around that.
    """
    if depth > 4:
        raise AssertionError(f"too many tool result wrappers: {type(result).__name__}")
    if isinstance(result, str):
        return _payload(json.loads(result), depth + 1)
    if isinstance(result, dict):
        if "result" in result:
            return _payload(result["result"], depth + 1)
        return result
    raise AssertionError(f"unexpected tool result shape: {type(result).__name__}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary-dir",
        default=os.environ.get("ARANA_BIN_DIR"),
        help="Directory containing the arana executable (native absolute or relative path)",
    )
    args = parser.parse_args()
    if args.binary_dir:
        # Use a native path here: Git Bash PATH entries are rewritten when a
        # native Windows Python process spawns the stdio MCP child.
        native = os.path.abspath(args.binary_dir)
        os.environ["PATH"] = native + os.pathsep + os.environ.get("PATH", "")

    from hermes_cli.plugins import discover_plugins, get_plugin_manager
    from tools.mcp_tool_discovery import discover_mcp_tools
    from tools.mcp_tool_lifecycle import shutdown_mcp_servers
    from tools.registry import registry

    try:
        discover_plugins(force=True)
        manager = get_plugin_manager()
        plugin = next(
            (row for row in manager.list_plugins() if row.get("name") == "arana"),
            None,
        )
        assert plugin is not None, "arana plugin was not discovered"
        assert plugin.get("enabled"), f"arana plugin is not enabled: {plugin}"
        skill = manager.find_plugin_skill("arana:arana")
        assert skill is not None, "bundled arana skill was not discovered"

        names = discover_mcp_tools(["arana"])
        missing = EXPECTED_TOOLS.difference(names)
        assert not missing, f"missing MCP tools through Hermes: {sorted(missing)}"

        caps_result = registry.dispatch("mcp__arana__list_capabilities", {})
        caps = _payload(caps_result)
        assert caps.get("ok") is True, caps
        assert caps.get("server", {}).get("name") == "arana", caps
        assert caps.get("server", {}).get("access") == "public-read-only", caps
        advertised = {tool.get("name") for tool in caps.get("tools", [])}
        assert advertised == {name.removeprefix("mcp__arana__") for name in EXPECTED_TOOLS}, advertised
        assert all(tool.get("annotations", {}).get("readOnlyHint") is True
                   for tool in caps.get("tools", [])), caps.get("tools")

        read_result = registry.dispatch("mcp__arana__read_url", {"url": PUBLIC_URL})
        read = _payload(read_result)
        assert read.get("ok") is True, read
        items = read.get("items") or []
        assert items and any(item.get("text") for item in items), read

        summary = {
            "plugin": {"name": plugin["name"], "version": plugin.get("version"),
                       "enabled": plugin.get("enabled")},
            "skill": "arana:arana",
            "mcp_tools": sorted(names),
            "server_version": caps["server"].get("version"),
            "access": caps["server"].get("access"),
            "public_read": {"url": PUBLIC_URL, "backend": read.get("backend"),
                            "items": len(items)},
        }
        print(json.dumps(summary, sort_keys=True))
        return 0
    finally:
        shutdown_mcp_servers()


if __name__ == "__main__":
    raise SystemExit(main())
