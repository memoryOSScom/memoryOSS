#!/usr/bin/env python3
import json
import os
import subprocess
import tempfile
import time
from pathlib import Path

from runner_common import (
    ROOT_DIR,
    ensure_binary,
    free_port,
    http_json_with_retry,
    iso_now,
    start_server,
    stop_process,
    wait_for_health,
    wait_for_indexer_sync,
    write_test_config,
)


OUTPUT_JSON = Path(
    os.environ.get(
        "UNIVERSAL_LOOP_OUTPUT_JSON",
        ROOT_DIR / "tests" / "universal-memory-loop-report.json",
    )
)

SOURCE_NAMESPACE = "loop-source"
TARGET_NAMESPACE = "loop-target"
REPLAY_NAMESPACE = "loop-replay"

SOURCE_KEY = "universal-loop-source-key"
TARGET_KEY = "universal-loop-target-key"
REPLAY_KEY = "universal-loop-replay-key"

PORTABILITY_MEMORIES = [
    {
        "content": "For review responses, keep findings first and summary brief.",
        "tags": ["review", "style", "preference"],
        "agent": "claude",
    },
    {
        "content": "Startup behavior: browser launcher should auto-open on start.",
        "tags": ["browser", "launcher", "startup"],
    },
    {
        "content": (
            "Deployment rollback guardrail: if production deploy metrics regress, "
            "roll back within 15 minutes."
        ),
        "tags": ["deploy", "rollback", "project"],
    },
    {
        "content": "Deployment checklist: require staging approval before production release.",
        "tags": ["deploy", "checklist", "project"],
    },
    {
        "content": (
            "Auth review checklist: require tests and security review before "
            "merging sensitive changes."
        ),
        "tags": ["review", "security", "checklist"],
    },
]

PORTABILITY_CASES = [
    {
        "label": "review style",
        "query": "How should review responses be structured here?",
        "expected_anchor": "findings first",
    },
    {
        "label": "deploy rollback",
        "query": "What is the rollback rule if production deploy metrics regress?",
        "expected_anchor": "roll back within 15 minutes",
    },
    {
        "label": "deploy checklist",
        "query": "Need the production release checklist before deploy.",
        "expected_anchor": "staging approval",
    },
    {
        "label": "review checklist",
        "query": "Review the auth changes before merge and audit anything risky.",
        "expected_anchor": "security review",
    },
]

TASK_STATE_CASES = [
    {
        "label": "deploy",
        "query": "Need the production rollout steps and rollback guardrail before deploy.",
        "expected_kind": "deploy",
        "anchors": ["staging approval", "roll back within 15 minutes"],
    },
    {
        "label": "review",
        "query": "Review the auth changes before merge and keep the response structure correct.",
        "expected_kind": "review",
        "anchors": ["security review", "findings first"],
    },
]


def auth_entries() -> str:
    return f"""
[[auth.api_keys]]
key = "{SOURCE_KEY}"
role = "admin"
namespace = "{SOURCE_NAMESPACE}"

[[auth.api_keys]]
key = "{TARGET_KEY}"
role = "admin"
namespace = "{TARGET_NAMESPACE}"

[[auth.api_keys]]
key = "{REPLAY_KEY}"
role = "admin"
namespace = "{REPLAY_NAMESPACE}"
"""


def api_json(
    method: str,
    base_url: str,
    path: str,
    api_key: str,
    *,
    body: dict | None = None,
    timeout: float = 120.0,
) -> dict:
    status, response_body = http_json_with_retry(
        method,
        f"{base_url}{path}",
        headers={"Authorization": f"Bearer {api_key}"},
        body=body,
        timeout=timeout,
        verify_tls=False,
    )
    if status != 200:
        raise RuntimeError(f"{method} {path} failed: status={status} body={response_body}")
    return response_body or {}


def run_cli(*args: str) -> str:
    binary = ensure_binary()
    result = subprocess.run(
        [str(binary), *args],
        cwd=ROOT_DIR,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            "command failed: "
            + " ".join(args)
            + f"\nstdout={result.stdout}\nstderr={result.stderr}"
        )
    return result.stdout.strip()


def top_result_content(body: dict) -> str:
    final_results = body.get("final_results") or []
    if not final_results:
        return ""
    top = final_results[0]
    memory = top.get("memory") or {}
    return str(memory.get("content") or "")


def combined_result_text(body: dict) -> str:
    texts = []
    for result in body.get("final_results") or []:
        memory = result.get("memory") or {}
        content = memory.get("content")
        if content:
            texts.append(str(content))
    return "\n".join(texts)


def task_state_has_structure(body: dict) -> bool:
    task_state = body.get("task_state") or {}
    for key in ("constraints", "facts", "decisions"):
        if task_state.get(key):
            return True
    return False


def build_items(
    portability_success_rate: float,
    portability_hits: int,
    merge_preview: dict,
    merge_conflict_rate: float,
    replay_fidelity: float,
    replay_checks: dict,
    task_state_quality: float,
    task_state_hits: int,
) -> list[dict]:
    total_portability = len(PORTABILITY_CASES)
    total_task_state = len(TASK_STATE_CASES)
    total_bundle_entries = merge_preview.get("total_entries", 0)
    return [
        {
            "name": "Public demo path",
            "status": "pass",
            "note": (
                "HTTP API store -> CLI passport export -> HTTP import dry-run/apply -> "
                "HTTP query-explain -> HTTP history replay"
            ),
        },
        {
            "name": "Portability success rate",
            "status": "pass",
            "note": (
                f"{portability_hits}/{total_portability} preserved query anchors after "
                "the API -> CLI -> API loop"
            ),
        },
        {
            "name": "Passport merge/conflict rate",
            "status": "pass",
            "note": (
                f"{total_bundle_entries} bundle entries; create={merge_preview['create_count']}, "
                f"merge={merge_preview['merge_count']}, conflict={merge_preview['conflict_count']} "
                f"({merge_conflict_rate * 100:.1f}% conflicts)"
            ),
        },
        {
            "name": "Replay fidelity",
            "status": "pass",
            "note": (
                f"{sum(1 for ok in replay_checks.values() if ok)}/{len(replay_checks)} checks "
                f"matched after replay into a clean namespace"
            ),
        },
        {
            "name": "Task-state quality",
            "status": "pass",
            "note": (
                f"{task_state_hits}/{total_task_state} task classes produced the expected "
                "kind, structure, and recall anchors"
            ),
        },
    ]


def main() -> None:
    started = time.time()
    tmp = Path(tempfile.mkdtemp(prefix="memoryoss-universal-loop-"))
    source_data_dir = tmp / "source-data"
    target_data_dir = tmp / "target-data"
    source_data_dir.mkdir(parents=True, exist_ok=True)
    target_data_dir.mkdir(parents=True, exist_ok=True)

    source_port = free_port()
    target_port = free_port()
    source_config = tmp / "source.toml"
    target_config = tmp / "target.toml"
    source_log = tmp / "source.log"
    target_log = tmp / "target.log"
    bundle_path = tmp / "portable-loop-passport.json"

    write_test_config(
        source_config,
        port=source_port,
        data_dir=source_data_dir,
        auth_entries=auth_entries(),
        extra_sections="""
[limits]
rate_limit_per_sec = 5000
""",
    )
    write_test_config(
        target_config,
        port=target_port,
        data_dir=target_data_dir,
        auth_entries=auth_entries(),
        extra_sections="""
[limits]
rate_limit_per_sec = 5000
""",
    )

    source_process = None
    target_process = None
    try:
        source_process = start_server(source_config, log_path=source_log)
        target_process = start_server(target_config, log_path=target_log)

        source_base = f"https://127.0.0.1:{source_port}"
        target_base = f"https://127.0.0.1:{target_port}"

        wait_for_health(source_base, verify_tls=False)
        wait_for_health(target_base, verify_tls=False)

        print("[loop] seeding portability source memories", flush=True)
        api_json(
            "POST",
            source_base,
            "/v1/store/batch",
            SOURCE_KEY,
            body={"memories": PORTABILITY_MEMORIES},
            timeout=180.0,
        )
        wait_for_indexer_sync(source_base, f"Bearer {SOURCE_KEY}", timeout=180.0)

        stop_process(source_process)
        source_process = None

        print("[loop] exporting portable passport via CLI", flush=True)
        passport_export_stdout = run_cli(
            "--config",
            str(source_config),
            "passport",
            "export",
            "--namespace",
            SOURCE_NAMESPACE,
            "--scope",
            "all",
            "--output",
            str(bundle_path),
        )
        bundle = json.loads(bundle_path.read_text(encoding="utf-8"))
        bundle_entries = len(bundle.get("memories") or [])

        print("[loop] seeding duplicate/conflict target state", flush=True)
        api_json(
            "POST",
            target_base,
            "/v1/store/batch",
            TARGET_KEY,
            body={
                "memories": [
                    {
                        "content": "For review responses, keep findings first and summary brief.",
                        "tags": ["review", "style", "preference"],
                    },
                    {
                        "content": (
                            "Startup behavior: browser launcher should not auto-open on start."
                        ),
                        "tags": ["browser", "launcher", "startup"],
                    },
                ]
            },
            timeout=180.0,
        )
        wait_for_indexer_sync(target_base, f"Bearer {TARGET_KEY}", timeout=180.0)

        print("[loop] dry-running passport import into second runtime", flush=True)
        dry_run = api_json(
            "POST",
            target_base,
            "/v1/passport/import",
            TARGET_KEY,
            body={"dry_run": True, "namespace": TARGET_NAMESPACE, "bundle": bundle},
            timeout=180.0,
        )
        merge_preview = dict(dry_run.get("preview") or {})
        merge_preview["total_entries"] = bundle_entries

        if not merge_preview.get("integrity_valid"):
            raise RuntimeError("passport dry-run reported invalid integrity")
        expected_preview = {"create_count": 3, "merge_count": 1, "conflict_count": 1}
        for field, expected in expected_preview.items():
            actual = int(merge_preview.get(field, -1))
            if actual != expected:
                raise RuntimeError(
                    f"passport preview {field} mismatch: expected {expected}, got {actual}"
                )

        print("[loop] applying passport import into second runtime", flush=True)
        imported = api_json(
            "POST",
            target_base,
            "/v1/passport/import",
            TARGET_KEY,
            body={"namespace": TARGET_NAMESPACE, "bundle": bundle},
            timeout=180.0,
        )
        if int(imported.get("imported", 0)) != 3:
            raise RuntimeError(f"passport import imported={imported.get('imported')} expected 3")
        wait_for_indexer_sync(target_base, f"Bearer {TARGET_KEY}", timeout=180.0)

        print("[loop] verifying portability recall anchors", flush=True)
        portability_hits = 0
        portability_details = []
        for case in PORTABILITY_CASES:
            explain = api_json(
                "POST",
                target_base,
                "/v1/admin/query-explain",
                TARGET_KEY,
                body={"query": case["query"], "limit": 5},
                timeout=180.0,
            )
            top_content = top_result_content(explain)
            success = case["expected_anchor"] in top_content
            portability_hits += 1 if success else 0
            portability_details.append(
                {
                    "label": case["label"],
                    "success": success,
                    "expected_anchor": case["expected_anchor"],
                    "top_result": top_content,
                }
            )
        portability_success_rate = round(portability_hits / len(PORTABILITY_CASES), 4)
        if portability_success_rate < 1.0:
            raise RuntimeError(
                f"portability success rate too low: {portability_success_rate:.4f}"
            )

        print("[loop] verifying task-state quality after portability import", flush=True)
        task_state_hits = 0
        task_state_details = []
        for case in TASK_STATE_CASES:
            explain = api_json(
                "POST",
                target_base,
                "/v1/admin/query-explain",
                TARGET_KEY,
                body={"query": case["query"], "limit": 5},
                timeout=180.0,
            )
            task_context = explain.get("task_context") or {}
            task_state = explain.get("task_state") or {}
            combined = combined_result_text(explain)
            success = (
                task_context.get("kind") == case["expected_kind"]
                and task_state.get("kind") == case["expected_kind"]
                and task_state_has_structure(explain)
                and all(anchor in combined for anchor in case["anchors"])
            )
            task_state_hits += 1 if success else 0
            task_state_details.append(
                {
                    "label": case["label"],
                    "success": success,
                    "task_context_kind": task_context.get("kind"),
                    "task_state_kind": task_state.get("kind"),
                    "anchors": case["anchors"],
                }
            )
        task_state_quality = round(task_state_hits / len(TASK_STATE_CASES), 4)
        if task_state_quality < 1.0:
            raise RuntimeError(f"task-state quality too low: {task_state_quality:.4f}")

        print("[loop] creating replay lineage on source runtime", flush=True)
        source_process = start_server(source_config, log_path=source_log)
        wait_for_health(source_base, verify_tls=False)

        old_id = api_json(
            "POST",
            source_base,
            "/v1/store",
            SOURCE_KEY,
            body={
                "content": "Project policy: use feature branches for deploys.",
                "tags": ["policy", "project"],
            },
        )["id"]
        conflict_id = api_json(
            "POST",
            source_base,
            "/v1/store",
            SOURCE_KEY,
            body={
                "content": "Project policy: do not use feature branches for deploys.",
                "tags": ["policy", "project"],
            },
        )["id"]
        replacement_id = api_json(
            "POST",
            source_base,
            "/v1/store",
            SOURCE_KEY,
            body={
                "content": "Project policy: use protected release branches for deploys.",
                "tags": ["policy", "project"],
            },
        )["id"]
        api_json(
            "POST",
            source_base,
            "/v1/feedback",
            SOURCE_KEY,
            body={"id": old_id, "action": "supersede", "superseded_by": replacement_id},
        )
        wait_for_indexer_sync(source_base, f"Bearer {SOURCE_KEY}", timeout=180.0)

        source_history = api_json(
            "GET",
            source_base,
            f"/v1/history/{old_id}",
            SOURCE_KEY,
        )
        history_bundle = api_json(
            "GET",
            source_base,
            f"/v1/history/{old_id}/bundle",
            SOURCE_KEY,
        )

        print("[loop] replaying history into a clean namespace", flush=True)
        history_preview = api_json(
            "POST",
            target_base,
            "/v1/history/replay",
            REPLAY_KEY,
            body={"dry_run": True, "namespace": REPLAY_NAMESPACE, "bundle": history_bundle},
            timeout=180.0,
        )
        if not history_preview.get("preview", {}).get("can_replay"):
            raise RuntimeError(f"history replay preview blocked: {history_preview}")

        replay_result = api_json(
            "POST",
            target_base,
            "/v1/history/replay",
            REPLAY_KEY,
            body={"namespace": REPLAY_NAMESPACE, "bundle": history_bundle},
            timeout=180.0,
        )
        if int(replay_result.get("imported", 0)) != len(history_bundle.get("memories") or []):
            raise RuntimeError(
                "history replay imported unexpected count: "
                f"{replay_result.get('imported')}"
            )
        wait_for_indexer_sync(target_base, f"Bearer {REPLAY_KEY}", timeout=180.0)

        target_history = api_json(
            "GET",
            target_base,
            f"/v1/history/{old_id}",
            REPLAY_KEY,
        )
        replay_checks = {
            "root_id": source_history.get("root_id") == target_history.get("root_id"),
            "branch_safe": source_history.get("branch_safe") == target_history.get("branch_safe"),
            "node_count": len(source_history.get("nodes") or [])
            == len(target_history.get("nodes") or []),
            "timeline_count": len(source_history.get("timeline") or [])
            == len(target_history.get("timeline") or []),
            "visible_memory_ids": source_history.get("visible_memory_ids")
            == target_history.get("visible_memory_ids"),
            "preview_create_count": int(history_preview.get("preview", {}).get("create_count", 0))
            == len(history_bundle.get("memories") or []),
            "imported_count": int(replay_result.get("imported", 0))
            == len(history_bundle.get("memories") or []),
            "conflict_node_present": any(
                node.get("id") == conflict_id for node in (target_history.get("nodes") or [])
            ),
        }
        replay_fidelity = round(
            sum(1 for ok in replay_checks.values() if ok) / len(replay_checks),
            4,
        )
        if replay_fidelity < 1.0:
            raise RuntimeError(f"replay fidelity too low: {replay_fidelity:.4f}")

        merge_conflict_rate = round(
            int(merge_preview["conflict_count"]) / bundle_entries,
            4,
        )

        report = {
            "runner": "tests/run_universal_memory_loop.py",
            "generated_at": iso_now(),
            "duration_seconds": int(time.time() - started),
            "summary": {
                "demo_clients": [
                    "http_api_store",
                    "cli_passport_export",
                    "http_api_import",
                    "http_query_explain",
                    "http_history_replay",
                ],
                "bundle_entries": bundle_entries,
                "portability_queries": len(PORTABILITY_CASES),
                "portability_success_rate": portability_success_rate,
                "merge_count": int(merge_preview["merge_count"]),
                "conflict_count": int(merge_preview["conflict_count"]),
                "merge_conflict_rate": merge_conflict_rate,
                "replay_fidelity": replay_fidelity,
                "task_state_cases": len(TASK_STATE_CASES),
                "task_state_quality": task_state_quality,
            },
            "items": build_items(
                portability_success_rate,
                portability_hits,
                merge_preview,
                merge_conflict_rate,
                replay_fidelity,
                replay_checks,
                task_state_quality,
                task_state_hits,
            ),
            "details": {
                "passport_export_stdout": passport_export_stdout,
                "portability_cases": portability_details,
                "task_state_cases": task_state_details,
                "replay_checks": replay_checks,
                "source_history": {
                    "root_id": source_history.get("root_id"),
                    "node_count": len(source_history.get("nodes") or []),
                    "timeline_count": len(source_history.get("timeline") or []),
                },
                "target_history": {
                    "root_id": target_history.get("root_id"),
                    "node_count": len(target_history.get("nodes") or []),
                    "timeline_count": len(target_history.get("timeline") or []),
                },
            },
        }
        OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(report["summary"], indent=2), flush=True)
    finally:
        if source_process is not None:
            stop_process(source_process)
        if target_process is not None:
            stop_process(target_process)


if __name__ == "__main__":
    main()
