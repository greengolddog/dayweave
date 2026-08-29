#!/usr/bin/env python3
"""Validate DayWeave's public plugin package without external dependencies."""

from __future__ import annotations

import json
import re
from pathlib import Path
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / ".codex-plugin" / "plugin.json"
MCP_MANIFEST = ROOT / ".mcp.json"
SKILL = ROOT / "skills" / "dayweave-scheduling" / "SKILL.md"
SKILL_METADATA = ROOT / "skills" / "dayweave-scheduling" / "agents" / "openai.yaml"
CONNECTION_REFERENCE = (
    ROOT / "skills" / "dayweave-scheduling" / "references" / "connection.md"
)

SECRET_PATTERN = re.compile(
    br"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"
    br"|github_pat_[A-Za-z0-9_]{20,}"
    br"|gh[pousr]_[A-Za-z0-9]{20,}"
    br"|AIza[0-9A-Za-z_-]{20,}"
    br"|AKIA[0-9A-Z]{16}"
    br"|ya29\.[0-9A-Za-z_-]+"
    br"|sk-[0-9A-Za-z_-]{20,}"
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


def load_json(path: Path) -> dict[str, object]:
    value = json.loads(path.read_text(encoding="utf-8"))
    require(isinstance(value, dict), f"{path.relative_to(ROOT)} must contain an object")
    return value


def validate_manifest() -> None:
    manifest = load_json(MANIFEST)
    require(manifest.get("name") == "dayweave", "plugin name must be dayweave")
    version = manifest.get("version")
    require(
        isinstance(version, str)
        and re.fullmatch(
            r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)", version
        )
        is not None,
        "plugin version must be stable semantic versioning",
    )
    require(manifest.get("skills") == "./skills/", "skills path must remain local")
    require(manifest.get("mcpServers") == "./.mcp.json", "MCP path must remain local")

    interface = manifest.get("interface")
    require(isinstance(interface, dict), "plugin interface metadata is required")
    prompts = interface.get("defaultPrompt")
    require(isinstance(prompts, list) and 1 <= len(prompts) <= 3, "provide 1-3 prompts")
    require(
        all(isinstance(prompt, str) and 1 <= len(prompt) <= 128 for prompt in prompts),
        "starter prompts must be non-empty and at most 128 characters",
    )


def validate_mcp() -> None:
    payload = load_json(MCP_MANIFEST)
    servers = payload.get("mcpServers")
    require(isinstance(servers, dict), "mcpServers must be an object")
    server = servers.get("dayweave")
    require(isinstance(server, dict), "dayweave MCP server is required")
    require(server.get("type") == "http", "DayWeave MCP must use streamable HTTP")
    require(
        server.get("bearer_token_env_var") == "DAYWEAVE_MCP_TOKEN",
        "MCP bearer credential must come from DAYWEAVE_MCP_TOKEN",
    )
    require("bearer_token" not in server, "never embed a bearer token in the plugin")

    url = server.get("url")
    require(isinstance(url, str), "MCP URL must be a string")
    parsed = urlsplit(url)
    require(parsed.username is None and parsed.password is None, "MCP URL must not contain credentials")
    require(not parsed.query and not parsed.fragment, "MCP URL must not contain query or fragment")
    require(parsed.path == "/mcp", "MCP URL path must be /mcp")
    require(
        parsed.scheme == "https"
        or (parsed.scheme == "http" and parsed.hostname in {"127.0.0.1", "::1"}),
        "MCP URL must use HTTPS unless it is loopback development",
    )

    headers = server.get("headers", {})
    require(isinstance(headers, dict), "MCP headers must be an object")
    require(
        not any(str(name).lower() == "authorization" for name in headers),
        "Authorization must not be embedded in MCP headers",
    )


def validate_skill() -> None:
    skill = SKILL.read_text(encoding="utf-8")
    require(skill.startswith("---\nname: dayweave-scheduling\n"), "skill frontmatter name is invalid")
    for tool in (
        "get_schedule",
        "search_items",
        "explain_placement",
        "get_conflicts",
        "simulate_plan",
        "submit_proposal",
    ):
        require(f"`{tool}`" in skill, f"skill must describe {tool}")
    require("references/connection.md" in skill, "skill must route connection failures")
    require(CONNECTION_REFERENCE.is_file(), "connection recovery reference is missing")

    metadata = SKILL_METADATA.read_text(encoding="utf-8")
    for expected in (
        "$dayweave-scheduling",
        'type: "mcp"',
        'value: "dayweave"',
        'transport: "streamable_http"',
        'url: "http://127.0.0.1:8787/mcp"',
    ):
        require(expected in metadata, f"skill metadata is missing {expected}")


def validate_public_files() -> None:
    for path in sorted(ROOT.rglob("*")):
        if not path.is_file() or ".DS_Store" in path.parts:
            continue
        contents = path.read_bytes()
        require(
            SECRET_PATTERN.search(contents) is None,
            f"credential-like material found in {path}",
        )


def main() -> None:
    validate_manifest()
    validate_mcp()
    validate_skill()
    validate_public_files()
    print("DayWeave plugin contract: PASS")


if __name__ == "__main__":
    main()
