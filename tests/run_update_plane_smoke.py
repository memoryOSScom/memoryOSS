#!/usr/bin/env python3
"""
Smoke-test the local install -> update -> rollback path for one memoryOSS binary.

The goal is not to emulate a full package manager. It proves the utility-plane
contract that a published binary can be installed into a versioned directory,
keep data through a migrate step, detect a bad update candidate, and recover via
backup/restore without manual data surgery.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from runner_common import (
    free_port,
    http_json,
    http_json_with_retry,
    stop_process,
    wait_for_health,
    wait_for_indexer_sync,
)

AUTH_KEY = "update-plane-key"
JWT_SECRET = "update-plane-secret-that-is-at-least-32-characters-long"
AUDIT_HMAC_SECRET = "update-plane-audit-secret-that-is-at-least-32-bytes-long"
NAMESPACE = "update-plane"


def toml_path(path: Path) -> str:
    return path.resolve().as_posix()


def write_config(
    path: Path,
    *,
    port: int,
    data_dir: Path,
    rate_limit_per_sec: int = 5000,
    extra_sections: str = "",
) -> None:
    path.write_text(
        f"""
[server]
host = "127.0.0.1"
port = {port}

[tls]
enabled = true
auto_generate = true

[storage]
data_dir = "{toml_path(data_dir)}"

[auth]
jwt_secret = "{JWT_SECRET}"
audit_hmac_secret = "{AUDIT_HMAC_SECRET}"

[[auth.api_keys]]
key = "{AUTH_KEY}"
role = "admin"
namespace = "{NAMESPACE}"

[logging]
level = "warn"

[limits]
rate_limit_per_sec = {rate_limit_per_sec}
"""
        .strip()
        + "\n"
        + extra_sections.strip()
        + ("\n" if extra_sections.strip() else ""),
        encoding="utf-8",
    )


def clone_binary(binary: Path, target: Path) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, target)
    mode = target.stat().st_mode
    target.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def start_server(binary: Path, config_path: Path, log_path: Path):
    log_handle = log_path.open("wb")
    process = subprocess.Popen(
        [str(binary), "--config", str(config_path), "serve"],
        stdout=log_handle,
        stderr=log_handle,
    )
    return process, log_handle


def run_checked(command: list[str], *, timeout: int = 120) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if result.returncode != 0:
        raise RuntimeError(
            f"command failed ({' '.join(command)}):\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result


def recall_count(base_url: str) -> int:
    status, body = http_json(
        "POST",
        f"{base_url}/v1/recall",
        headers={"Authorization": f"Bearer {AUTH_KEY}"},
        body={"query": "update smoke anchor", "limit": 10},
        verify_tls=False,
    )
    if status != 200:
        raise RuntimeError(f"recall failed with status={status} body={body}")
    return len((body or {}).get("memories", []))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=str(Path("target/debug/memoryoss")), help="Path to memoryOSS binary")
    parser.add_argument("--channel", default="local", help="Release channel label for the report")
    parser.add_argument(
        "--output-json",
        default=None,
        help="Optional path for the JSON summary (defaults to UPDATE_PLANE_OUTPUT_JSON env var)",
    )
    parser.add_argument("--keep-temp", action="store_true", help="Keep the temp workspace on success")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.exists():
        raise SystemExit(f"binary not found: {binary}")

    output_json = args.output_json or os.environ.get(
        "UPDATE_PLANE_OUTPUT_JSON",
        str(Path(__file__).resolve().with_name("update-plane-report.json")),
    )

    started_at = time.time()
    workspace_root = Path.cwd() / "tests" / ".tmp"
    workspace_root.mkdir(parents=True, exist_ok=True)
    workspace = Path(tempfile.mkdtemp(prefix="memoryoss-update-plane-", dir=workspace_root))
    items: list[dict[str, object]] = []
    stable_process = None
    stable_log = None
    candidate_process = None
    candidate_log = None
    rollback_process = None
    rollback_log = None

    try:
        stable_bin = workspace / "install" / "stable" / binary.name
        candidate_bin = workspace / "install" / "candidate" / binary.name
        clone_binary(binary, stable_bin)
        clone_binary(binary, candidate_bin)
        items.append(
            {
                "name": "Versioned install roots prepared",
                "status": "pass",
                "note": f"stable={stable_bin.parent} candidate={candidate_bin.parent}",
            }
        )

        data_dir = workspace / "data"
        stable_port = free_port()
        stable_config = workspace / "stable.toml"
        stable_log_path = workspace / "stable.log"
        write_config(stable_config, port=stable_port, data_dir=data_dir)
        base_url = f"https://127.0.0.1:{stable_port}"

        stable_process, stable_log = start_server(stable_bin, stable_config, stable_log_path)
        wait_for_health(base_url, timeout=60.0, verify_tls=False)
        items.append({"name": "Fresh install boots cleanly", "status": "pass"})

        for suffix in ("policy", "review", "rollback"):
            status, body = http_json_with_retry(
                "POST",
                f"{base_url}/v1/store",
                headers={"Authorization": f"Bearer {AUTH_KEY}"},
                body={"content": f"update smoke anchor {suffix}", "tags": ["update-plane", suffix]},
                verify_tls=False,
            )
            if status != 200:
                raise RuntimeError(f"store failed with status={status} body={body}")
        seed_count = recall_count(base_url)
        wait_for_indexer_sync(base_url, f"Bearer {AUTH_KEY}", timeout=60.0)
        items.append(
            {
                "name": "Installed binary can seed durable memories",
                "status": "pass",
                "note": f"recall_count={seed_count}",
            }
        )

        stop_process(stable_process)
        stable_process = None
        stable_log.close()
        stable_log = None

        backup_path = workspace / "pre-update-backup.tar.zst"
        run_checked(
            [
                str(stable_bin),
                "--config",
                str(stable_config),
                "backup",
                "--output",
                str(backup_path),
                "--include-key",
            ]
        )
        items.append(
            {
                "name": "Pre-update backup created",
                "status": "pass",
                "note": f"{backup_path.stat().st_size} bytes",
            }
        )

        candidate_port = free_port()
        candidate_config = workspace / "candidate.toml"
        candidate_log_path = workspace / "candidate.log"
        write_config(
            candidate_config,
            port=candidate_port,
            data_dir=data_dir,
            extra_sections="""
[proxy]
enabled = false
extraction_enabled = false
default_memory_mode = "full"
""",
        )
        run_checked([str(candidate_bin), "--config", str(candidate_config), "migrate"])
        items.append({"name": "Candidate update runs schema migration cleanly", "status": "pass"})

        candidate_process, candidate_log = start_server(candidate_bin, candidate_config, candidate_log_path)
        candidate_base = f"https://127.0.0.1:{candidate_port}"
        wait_for_health(candidate_base, timeout=60.0, verify_tls=False)
        candidate_count = recall_count(candidate_base)
        if candidate_count < seed_count:
            raise RuntimeError(f"candidate lost memories: seed={seed_count} candidate={candidate_count}")
        items.append(
            {
                "name": "Update keeps data readable after migrate",
                "status": "pass",
                "note": f"recall_count={candidate_count}",
            }
        )

        stop_process(candidate_process)
        candidate_process = None
        candidate_log.close()
        candidate_log = None

        bad_config = workspace / "bad-candidate.toml"
        bad_data_dir = workspace / "bad-data"
        write_config(
            bad_config,
            port=free_port(),
            data_dir=bad_data_dir,
            rate_limit_per_sec=0,
        )
        bad_result = subprocess.run(
            [str(candidate_bin), "--config", str(bad_config), "doctor"],
            capture_output=True,
            text=True,
            timeout=60,
        )
        if bad_result.returncode == 0:
            raise RuntimeError("bad candidate unexpectedly passed doctor")
        items.append(
            {
                "name": "Broken update candidate is rejected before rollout",
                "status": "pass",
                "note": bad_result.stderr.strip() or bad_result.stdout.strip() or "doctor returned non-zero",
            }
        )

        rollback_data_dir = workspace / "rollback-data"
        rollback_port = free_port()
        rollback_config = workspace / "rollback.toml"
        rollback_log_path = workspace / "rollback.log"
        write_config(rollback_config, port=rollback_port, data_dir=rollback_data_dir)
        run_checked([str(stable_bin), "--config", str(rollback_config), "restore", str(backup_path)])
        rollback_process, rollback_log = start_server(stable_bin, rollback_config, rollback_log_path)
        rollback_base = f"https://127.0.0.1:{rollback_port}"
        wait_for_health(rollback_base, timeout=60.0, verify_tls=False)
        rollback_count = recall_count(rollback_base)
        if rollback_count < seed_count:
            raise RuntimeError(f"rollback restore lost memories: seed={seed_count} rollback={rollback_count}")
        items.append(
            {
                "name": "Rollback restore recovers the pre-update state",
                "status": "pass",
                "note": f"recall_count={rollback_count}",
            }
        )

        stop_process(rollback_process)
        rollback_process = None
        rollback_log.close()
        rollback_log = None

        summary = {
            "runner": "tests/run_update_plane_smoke.py",
            "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "duration_seconds": round(time.time() - started_at, 2),
            "channel": args.channel,
            "status": "pass",
            "seed_count": seed_count,
            "candidate_count": candidate_count,
            "rollback_count": rollback_count,
            "items": items,
            "workspace": str(workspace),
        }
    except Exception as exc:
        summary = {
            "runner": "tests/run_update_plane_smoke.py",
            "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "duration_seconds": round(time.time() - started_at, 2),
            "channel": args.channel,
            "status": "fail",
            "items": items
            + [
                {
                    "name": "Update plane smoke",
                    "status": "fail",
                    "note": str(exc),
                }
            ],
            "workspace": str(workspace),
        }
    finally:
        if stable_process is not None:
            stop_process(stable_process)
        if candidate_process is not None:
            stop_process(candidate_process)
        if rollback_process is not None:
            stop_process(rollback_process)
        if stable_log is not None:
            stable_log.close()
        if candidate_log is not None:
            candidate_log.close()
        if rollback_log is not None:
            rollback_log.close()

    output_path = Path(output_json) if output_json else None
    if output_path is not None:
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")

    print(json.dumps(summary, indent=2))
    if summary["status"] == "pass" and not args.keep_temp:
        shutil.rmtree(workspace, ignore_errors=True)
    return 0 if summary["status"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
