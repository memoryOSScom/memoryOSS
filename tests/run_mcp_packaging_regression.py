#!/usr/bin/env python3
import json
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

from runner_common import ROOT_DIR, iso_now


OUTPUT_JSON = ROOT_DIR / "tests" / ".last-run" / "mcp-packaging-report.json"
SCRIPT = ROOT_DIR / "scripts" / "build_mcp_package.py"


def assert_result(name: str, passed: bool, note: str | None = None) -> dict:
    result = {"name": name, "status": "pass" if passed else "fail"}
    if note:
        result["note"] = note
    return result


def run_builder(*, server_json: Path, cargo_toml: Path, output_dir: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        [
            "python3",
            str(SCRIPT),
            "--server-json",
            str(server_json),
            "--cargo-toml",
            str(cargo_toml),
            "--output-dir",
            str(output_dir),
        ],
        cwd=ROOT_DIR,
        text=True,
        capture_output=True,
        check=False,
    )


def main() -> None:
    OUTPUT_JSON.parent.mkdir(parents=True, exist_ok=True)
    started = time.time()
    tmp = Path(tempfile.mkdtemp(prefix="memoryoss-mcp-packaging-"))
    assertions: list[dict] = []
    try:
        output_dir = tmp / "out"
        ok = run_builder(
            server_json=ROOT_DIR / "server.json",
            cargo_toml=ROOT_DIR / "Cargo.toml",
            output_dir=output_dir,
        )
        assertions.append(assert_result("package builder exits cleanly", ok.returncode == 0, ok.stderr[-400:]))
        expected_files = [
            "memoryoss-mcp-server.json",
            "memoryoss-mcp-manifest.json",
            "memoryoss-mcp-claude-desktop.json",
            "memoryoss-mcp-tools.json",
            "memoryoss-mcp-package.json",
        ]
        for filename in expected_files:
            assertions.append(assert_result(f"{filename} emitted", (output_dir / filename).exists()))

        manifest = json.loads((output_dir / "memoryoss-mcp-manifest.json").read_text(encoding="utf-8"))
        desktop = json.loads((output_dir / "memoryoss-mcp-claude-desktop.json").read_text(encoding="utf-8"))
        tools = json.loads((output_dir / "memoryoss-mcp-tools.json").read_text(encoding="utf-8"))
        cargo_toml = (ROOT_DIR / "Cargo.toml").read_text(encoding="utf-8")
        version = cargo_toml.split('version = "', 1)[1].split('"', 1)[0]
        assertions.extend(
            [
                assert_result("manifest version matches Cargo.toml", manifest.get("version") == version),
                assert_result(
                    "desktop config points to memoryoss command",
                    desktop.get("mcpServers", {}).get("memoryoss", {}).get("command") == "memoryoss",
                ),
                assert_result(
                    "tool catalog ships titles and safety hints",
                    all(
                        entry.get("title")
                        and "readOnlyHint" in entry.get("annotations", {})
                        and "destructiveHint" in entry.get("annotations", {})
                        for entry in tools.get("tools", [])
                    ),
                ),
            ]
        )

        missing_tools = tmp / "missing-tools.json"
        broken_server = json.loads((ROOT_DIR / "server.json").read_text(encoding="utf-8"))
        broken_server["_meta"]["io.github.memoryOSScom/anthropic-local-mcp"].pop("toolAnnotations", None)
        missing_tools.write_text(json.dumps(broken_server, indent=2) + "\n", encoding="utf-8")
        missing_tools_run = run_builder(
            server_json=missing_tools,
            cargo_toml=ROOT_DIR / "Cargo.toml",
            output_dir=tmp / "missing-tools-out",
        )
        assertions.append(
            assert_result(
                "missing tool annotations fails closed",
                missing_tools_run.returncode != 0 and "toolAnnotations" in missing_tools_run.stderr,
                missing_tools_run.stderr[-400:],
            )
        )

        skew_server = tmp / "version-skew.json"
        skew_payload = json.loads((ROOT_DIR / "server.json").read_text(encoding="utf-8"))
        skew_payload["version"] = "9.9.9"
        skew_server.write_text(json.dumps(skew_payload, indent=2) + "\n", encoding="utf-8")
        skew_run = run_builder(
            server_json=skew_server,
            cargo_toml=ROOT_DIR / "Cargo.toml",
            output_dir=tmp / "version-skew-out",
        )
        assertions.append(
            assert_result(
                "version skew fails closed",
                skew_run.returncode != 0 and "does not match Cargo.toml version" in skew_run.stderr,
                skew_run.stderr[-400:],
            )
        )

        broken_manifest = tmp / "missing-manifest.json"
        broken_payload = json.loads((ROOT_DIR / "server.json").read_text(encoding="utf-8"))
        broken_payload["_meta"]["io.github.memoryOSScom/anthropic-local-mcp"].pop("manifestTemplate", None)
        broken_manifest.write_text(json.dumps(broken_payload, indent=2) + "\n", encoding="utf-8")
        broken_manifest_run = run_builder(
            server_json=broken_manifest,
            cargo_toml=ROOT_DIR / "Cargo.toml",
            output_dir=tmp / "missing-manifest-out",
        )
        assertions.append(
            assert_result(
                "missing manifest metadata fails closed",
                broken_manifest_run.returncode != 0 and "manifestTemplate" in broken_manifest_run.stderr,
                broken_manifest_run.stderr[-400:],
            )
        )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    passed = all(item["status"] == "pass" for item in assertions)
    report = {
        "runner": "tests/run_mcp_packaging_regression.py",
        "generated_at": iso_now(),
        "duration_seconds": int(time.time() - started),
        "summary": {
            "status": "pass" if passed else "fail",
            "assertions": len(assertions),
            "assertions_passed": sum(1 for item in assertions if item["status"] == "pass"),
        },
        "assertions": assertions,
    }
    OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report["summary"], indent=2))
    if not passed:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
