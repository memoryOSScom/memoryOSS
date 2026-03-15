#!/usr/bin/env python3
import argparse
import json
import tomllib
from pathlib import Path


ANTHROPIC_META_KEY = "io.github.memoryOSScom/anthropic-local-mcp"


def fail(message: str) -> None:
    raise SystemExit(message)


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, payload: object) -> None:
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def load_version(cargo_toml: Path) -> str:
    cargo = tomllib.loads(cargo_toml.read_text(encoding="utf-8"))
    version = cargo.get("package", {}).get("version")
    if not version:
        fail(f"missing package.version in {cargo_toml}")
    return version


def validate_tool_annotations(tool_annotations: list[dict]) -> None:
    if not tool_annotations:
        fail("server.json is missing _meta toolAnnotations")
    for entry in tool_annotations:
        name = entry.get("name")
        title = entry.get("title")
        annotations = entry.get("annotations", {})
        if not name or not title:
            fail("every tool annotation entry needs non-empty name and title")
        if "readOnlyHint" not in annotations or "destructiveHint" not in annotations:
            fail(f"tool {name} is missing safety hints")
        if annotations["readOnlyHint"] == annotations["destructiveHint"]:
            fail(f"tool {name} must set exactly one safety hint")


def build_outputs(server_manifest: dict, version: str) -> dict[str, object]:
    if server_manifest.get("version") != version:
        fail(
            f"server.json version {server_manifest.get('version')} does not match Cargo.toml version {version}"
        )

    meta = server_manifest.get("_meta", {}).get(ANTHROPIC_META_KEY)
    if not isinstance(meta, dict):
        fail(f"server.json missing _meta.{ANTHROPIC_META_KEY}")

    desktop_install = meta.get("desktopInstall")
    manifest_template = meta.get("manifestTemplate")
    tool_annotations = meta.get("toolAnnotations")
    support = meta.get("support", {})
    if not isinstance(desktop_install, dict):
        fail("server.json missing desktopInstall metadata")
    if not isinstance(manifest_template, dict):
        fail("server.json missing manifestTemplate metadata")
    if not isinstance(tool_annotations, list):
        fail("server.json missing toolAnnotations metadata")
    validate_tool_annotations(tool_annotations)

    command = desktop_install.get("command")
    args = desktop_install.get("args")
    if not command or not isinstance(args, list) or not args:
        fail("desktopInstall must include command and args")

    server_copy = json.loads(json.dumps(server_manifest))
    manifest = {
        "manifest_version": manifest_template.get("manifest_version", "0.3"),
        "name": server_manifest["name"],
        "display_name": server_manifest["title"],
        "version": version,
        "description": server_manifest["description"],
        "author": {"name": "memoryOSS Contributors"},
        "homepage": server_manifest["websiteUrl"],
        "documentation": desktop_install.get("docsUrl"),
        "support": support.get("issuesUrl"),
        "privacy_policies": manifest_template.get("privacy_policies", []),
        "repository": server_manifest["repository"]["url"],
        "compatibility": manifest_template.get("compatibility", {}),
        "server": {
            "type": manifest_template.get("server", {}).get("type", "binary"),
            "entry_point": manifest_template.get("server", {}).get("entry_point", "memoryoss"),
            "mcp_config": {
                "command": command,
                "args": args,
            },
        },
        "tools": tool_annotations,
    }
    desktop = {
        "mcpServers": {
            "memoryoss": {
                "command": command,
                "args": args,
            }
        }
    }
    tools = {
        "version": version,
        "wire_annotations_in_tools_list": False,
        "fallback_reason": "rmcp 0.1.5 omits title/readOnlyHint/destructiveHint on live tools/list",
        "tools": tool_annotations,
    }
    index = {
        "version": version,
        "server_manifest": "memoryoss-mcp-server.json",
        "anthropic_manifest": "memoryoss-mcp-manifest.json",
        "claude_desktop_config": "memoryoss-mcp-claude-desktop.json",
        "tool_catalog": "memoryoss-mcp-tools.json",
    }
    return {
        "memoryoss-mcp-server.json": server_copy,
        "memoryoss-mcp-manifest.json": manifest,
        "memoryoss-mcp-claude-desktop.json": desktop,
        "memoryoss-mcp-tools.json": tools,
        "memoryoss-mcp-package.json": index,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Build versioned MCP packaging artifacts from server.json.")
    parser.add_argument("--server-json", type=Path, default=Path("server.json"))
    parser.add_argument("--cargo-toml", type=Path, default=Path("Cargo.toml"))
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()

    server_manifest = read_json(args.server_json)
    version = load_version(args.cargo_toml)
    outputs = build_outputs(server_manifest, version)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for filename, payload in outputs.items():
        write_json(args.output_dir / filename, payload)


if __name__ == "__main__":
    main()
