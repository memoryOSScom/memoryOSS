#!/usr/bin/env python3
import argparse
import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

from runner_common import ROOT_DIR, ensure_binary, iso_now


OUTPUT_JSON = Path(
    os.environ.get(
        "SETUP_PROFILE_SMOKE_OUTPUT_JSON",
        ROOT_DIR / "tests" / ".last-run" / "setup-profile-smoke.json",
    )
)


def assert_result(name: str, passed: bool, note: str | None = None) -> dict:
    result = {"name": name, "status": "pass" if passed else "fail"}
    if note:
        result["note"] = note
    return result


def run_setup(binary: Path, home: Path, config_path: Path, profile: str, *, second_run: bool = False) -> subprocess.CompletedProcess:
    env = os.environ.copy()
    env["HOME"] = str(home)
    env["SHELL"] = os.environ.get("SHELL", "/bin/bash")
    env["MEMORYOSS_PORT"] = "18000"
    env["MEMORYOSS_DISABLE_SYSTEMD"] = "1"
    env["MEMORYOSS_SKIP_START"] = "1"
    env.pop("CODEX_HOME", None)
    return subprocess.run(
        [str(binary), "--config", str(config_path), "setup", "--profile", profile],
        cwd=ROOT_DIR,
        env=env,
        input=("1\n" if second_run else "\n"),
        text=True,
        capture_output=True,
        check=False,
    )


def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8") if path.exists() else ""


def expected_surfaces(profile: str) -> tuple[bool, bool, bool]:
    if profile == "team-node":
        return (
            shutil.which("claude") is not None,
            shutil.which("codex") is not None,
            shutil.which("cursor") is not None or shutil.which("cursor-agent") is not None,
        )
    return {
        "claude": (True, False, False),
        "codex": (False, True, False),
        "cursor": (False, False, True),
    }[profile]


def run_profile(binary: Path, profile: str) -> dict:
    tmp = Path(tempfile.mkdtemp(prefix=f"memoryoss-setup-profile-{profile}-"))
    home = tmp / "home"
    home.mkdir(parents=True, exist_ok=True)
    config_path = tmp / "memoryoss.toml"

    first = run_setup(binary, home, config_path, profile)
    assertions = [
        assert_result("first setup exits cleanly", first.returncode == 0, first.stderr[-400:]),
        assert_result("config written", config_path.exists()),
        assert_result("profile persisted in config", f'profile = "{profile.replace("-", "_")}"' in read_text(config_path)),
        assert_result("profile banner printed", f"Selected profile: {profile.replace('-', '_')}" in first.stdout),
        assert_result("setup done banner printed", "Setup done." in first.stdout),
        assert_result("server start skipped for smoke", "MEMORYOSS_SKIP_START" in first.stdout),
    ]

    claude_written = (home / ".claude.json").exists() and (home / ".claude" / "settings.json").exists()
    codex_written = (home / ".codex" / "config.toml").exists() and (home / "AGENTS.md").exists()
    cursor_written = (home / ".cursor" / "mcp.json").exists()
    cursor_rule_written = (home / ".cursor" / "rules" / "memoryoss.mdc").exists()

    expected = expected_surfaces(profile)

    assertions.extend(
        [
            assert_result("claude files match profile", claude_written == expected[0]),
            assert_result("codex files match profile", codex_written == expected[1]),
            assert_result("cursor files match profile", cursor_written == expected[2]),
            assert_result("cursor rule file matches profile", cursor_rule_written == expected[2]),
        ]
    )

    second = run_setup(binary, home, config_path, profile, second_run=True)
    assertions.extend(
        [
            assert_result("second setup exits cleanly", second.returncode == 0, second.stderr[-400:]),
            assert_result("second run keeps existing config by default", "Keeping existing config." in second.stdout),
        ]
    )

    passed = all(item["status"] == "pass" for item in assertions)
    return {
        "name": profile,
        "status": "pass" if passed else "fail",
        "assertions": assertions,
        "assertion_count": len(assertions),
    }


def main() -> None:
    OUTPUT_JSON.parent.mkdir(parents=True, exist_ok=True)
    parser = argparse.ArgumentParser(description="Run setup-profile smoke scenarios.")
    parser.add_argument("--binary", type=Path, help="Path to the memoryoss binary to test")
    args = parser.parse_args()
    binary = args.binary or Path(
        os.environ.get("SETUP_PROFILE_SMOKE_BINARY", str(ensure_binary()))
    )
    started = time.time()
    profiles = ["claude", "codex", "cursor", "team-node"]
    results = [run_profile(binary, profile) for profile in profiles]
    passed_assertions = sum(
        1
        for scenario in results
        for assertion in scenario["assertions"]
        if assertion["status"] == "pass"
    )
    report = {
        "runner": "tests/run_setup_profile_smoke.py",
        "generated_at": iso_now(),
        "duration_seconds": int(time.time() - started),
        "summary": {
            "profiles": len(results),
            "profiles_passed": sum(1 for scenario in results if scenario["status"] == "pass"),
            "assertions_passed": passed_assertions,
        },
        "profiles": results,
    }
    OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report["summary"], indent=2))
    if report["summary"]["profiles_passed"] != len(results):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
