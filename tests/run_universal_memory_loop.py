#!/usr/bin/env python3
import argparse
import json
import os
import shutil
import subprocess
import tempfile
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread

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


DEFAULT_OUTPUT_JSON = Path(
    os.environ.get(
        "UNIVERSAL_LOOP_OUTPUT_JSON",
        ROOT_DIR / "tests" / ".last-run" / "universal-memory-loop-report.json",
    )
)

SOURCE_NAMESPACE = "loop-source"
TARGET_NAMESPACE = "loop-target"
REPLAY_NAMESPACE = "loop-replay"
PROXY_KEY = "universal-loop-proxy-key"

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

REVIEW_PROPOSALS = [
    "Release review checklist: verify migrations before rollout.",
    "Incident review note: capture remediation owners before closeout.",
    "Policy review reminder: security approval is mandatory for auth changes.",
]

GUARDRAIL_POLICIES = [
    {
        "content": "Retention policy: never delete audit logs or production backups from chat.",
        "tags": ["policy", "retention", "delete"],
    },
    {
        "content": "Release policy: staging approval is mandatory before production deploys.",
        "tags": ["policy", "deploy", "approval"],
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


class DummyUpstreamHandler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:  # pragma: no cover - exercised via utility loop
        length = int(self.headers.get("Content-Length", "0"))
        _ = self.rfile.read(length)
        if self.path != "/v1/chat/completions":
            self.send_response(404)
            self.end_headers()
            return
        body = json.dumps(
            {
                "id": "chatcmpl-loop",
                "object": "chat.completion",
                "created": int(time.time()),
                "model": "dummy-upstream",
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop",
                    }
                ],
            }
        ).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args) -> None:  # pragma: no cover - quiet helper
        return


def start_dummy_upstream() -> tuple[int, ThreadingHTTPServer, Thread]:
    port = free_port()
    server = ThreadingHTTPServer(("127.0.0.1", port), DummyUpstreamHandler)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return port, server, thread


def api_json(
    method: str,
    base_url: str,
    path: str,
    api_key: str,
    *,
    body: dict | None = None,
    extra_headers: dict | None = None,
    timeout: float = 120.0,
) -> dict:
    headers = {"Authorization": f"Bearer {api_key}"}
    if extra_headers:
        headers.update(extra_headers)
    status, response_body = http_json_with_retry(
        method,
        f"{base_url}{path}",
        headers=headers,
        body=body,
        timeout=timeout,
        verify_tls=False,
    )
    if status != 200:
        raise RuntimeError(f"{method} {path} failed: status={status} body={response_body}")
    return response_body or {}


def api_status(
    method: str,
    base_url: str,
    path: str,
    api_key: str,
    *,
    body: dict | None = None,
    extra_headers: dict | None = None,
    timeout: float = 120.0,
) -> tuple[int, dict | None]:
    headers = {"Authorization": f"Bearer {api_key}"}
    if extra_headers:
        headers.update(extra_headers)
    return http_json_with_retry(
        method,
        f"{base_url}{path}",
        headers=headers,
        body=body,
        timeout=timeout,
        verify_tls=False,
    )


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
    repeated_context_elimination_rate: float,
    merge_preview: dict,
    merge_conflict_rate: float,
    review_confirmed: int,
    review_throughput_per_minute: float,
    blocked_bad_actions_rate: float,
    confirmation_gate_rate: float,
    replay_fidelity: float,
    replay_checks: dict,
    task_state_quality: float,
    task_state_hits: int,
    loop_count: int,
) -> list[dict]:
    total_portability = len(PORTABILITY_CASES)
    total_task_state = len(TASK_STATE_CASES)
    total_bundle_entries = merge_preview.get("total_entries", 0)
    return [
        {
            "name": "Public everyday loops",
            "status": "pass",
            "note": (
                f"{loop_count} reproducible loops across HTTP API, CLI, reader, proxy, "
                "and two local runtimes"
            ),
        },
        {
            "name": "Repeated-context elimination",
            "status": "pass",
            "note": (
                f"{repeated_context_elimination_rate * 100:.1f}% average context reduction "
                "versus replaying the full portable notebook"
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
            "name": "Review throughput",
            "status": "pass",
            "note": (
                f"{review_confirmed} governed reviews confirmed at "
                f"{review_throughput_per_minute:.1f}/min"
            ),
        },
        {
            "name": "Blocked bad actions",
            "status": "pass",
            "note": (
                f"delete blocks held at {blocked_bad_actions_rate * 100:.1f}% and deploy "
                f"confirmation gates at {confirmation_gate_rate * 100:.1f}%"
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


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Run the universal memory loop proof across passport portability, "
            "review queue, guardrails, and history replay."
        )
    )
    parser.add_argument(
        "--output-json",
        default=str(DEFAULT_OUTPUT_JSON),
        help="Path for the generated loop report JSON.",
    )
    parser.add_argument(
        "--keep-temp",
        action="store_true",
        help="Keep the temporary workspace instead of deleting it on success.",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    started = time.time()
    success = False
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
    upstream_port, upstream_server, upstream_thread = start_dummy_upstream()

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
        extra_sections=f"""
[limits]
rate_limit_per_sec = 5000

[proxy]
enabled = true
passthrough_auth = false
extraction_enabled = false
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"

[[proxy.key_mapping]]
proxy_key = "{PROXY_KEY}"
namespace = "{TARGET_NAMESPACE}"
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
        portability_elimination = []
        portability_details = []
        total_portability_context_chars = sum(
            len(memory["content"]) for memory in PORTABILITY_MEMORIES
        )
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
            recalled_chars = len(top_content)
            elimination_rate = round(
                max(0.0, 1.0 - (recalled_chars / total_portability_context_chars)),
                4,
            )
            portability_elimination.append(elimination_rate)
            portability_details.append(
                {
                    "label": case["label"],
                    "success": success,
                    "expected_anchor": case["expected_anchor"],
                    "top_result": top_content,
                    "recalled_chars": recalled_chars,
                    "context_elimination_rate": elimination_rate,
                }
            )
        portability_success_rate = round(portability_hits / len(PORTABILITY_CASES), 4)
        if portability_success_rate < 1.0:
            raise RuntimeError(
                f"portability success rate too low: {portability_success_rate:.4f}"
            )
        repeated_context_elimination_rate = round(
            sum(portability_elimination) / len(portability_elimination),
            4,
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

        print("[loop] measuring review throughput", flush=True)
        review_keys = []
        for idx, content in enumerate(REVIEW_PROPOSALS, start=1):
            proposed = api_json(
                "POST",
                target_base,
                "/v1/admin/team/governance/propose",
                TARGET_KEY,
                body={
                    "namespace": TARGET_NAMESPACE,
                    "content": content,
                    "tags": ["utility", "review", f"item-{idx}"],
                    "branch": "everyday-utility",
                    "scope": "release",
                    "review_required": True,
                    "owners": [TARGET_NAMESPACE],
                    "watchlist": ["ops"],
                },
                timeout=180.0,
            )
            review_keys.append(proposed["review_key"])
        wait_for_indexer_sync(target_base, f"Bearer {TARGET_KEY}", timeout=180.0)
        review_started = time.perf_counter()
        review_queue = api_json(
            "GET",
            target_base,
            f"/v1/admin/review-queue?namespace={TARGET_NAMESPACE}&limit=10",
            TARGET_KEY,
            timeout=180.0,
        )
        queued_keys = {
            item["review_key"]
            for item in (review_queue.get("items") or [])
            if (item.get("team_governance") or {}).get("branch") == "everyday-utility"
        }
        if len(queued_keys) != len(review_keys):
            raise RuntimeError(
                f"review queue mismatch: expected {len(review_keys)} utility items, got {len(queued_keys)}"
            )
        for review_key in review_keys:
            api_json(
                "POST",
                target_base,
                "/v1/admin/review/action",
                TARGET_KEY,
                body={
                    "namespace": TARGET_NAMESPACE,
                    "review_key": review_key,
                    "action": "confirm",
                },
                timeout=180.0,
            )
        wait_for_indexer_sync(target_base, f"Bearer {TARGET_KEY}", timeout=180.0)
        review_duration_seconds = max(time.perf_counter() - review_started, 0.001)
        review_queue_after = api_json(
            "GET",
            target_base,
            f"/v1/admin/review-queue?namespace={TARGET_NAMESPACE}&limit=10",
            TARGET_KEY,
            timeout=180.0,
        )
        remaining_review_items = [
            item
            for item in (review_queue_after.get("items") or [])
            if (item.get("team_governance") or {}).get("branch") == "everyday-utility"
        ]
        if remaining_review_items:
            raise RuntimeError(f"utility review items still pending: {remaining_review_items}")
        review_confirmed = len(review_keys)
        review_throughput_per_minute = round(
            review_confirmed / review_duration_seconds * 60.0,
            2,
        )

        print("[loop] exercising guardrailed production actions", flush=True)
        api_json(
            "POST",
            target_base,
            "/v1/store/batch",
            TARGET_KEY,
            body={
                "memories": [
                    {
                        **memory,
                        "namespace": TARGET_NAMESPACE,
                    }
                    for memory in GUARDRAIL_POLICIES
                ]
            },
            timeout=180.0,
        )
        wait_for_indexer_sync(target_base, f"Bearer {TARGET_KEY}", timeout=180.0)
        delete_explain = api_json(
            "POST",
            target_base,
            "/v1/admin/query-explain",
            TARGET_KEY,
            body={
                "namespace": TARGET_NAMESPACE,
                "query": "Delete the production backups and audit logs now.",
                "limit": 5,
            },
            timeout=180.0,
        )
        delete_proxy_status, _ = api_status(
            "POST",
            target_base,
            "/proxy/v1/chat/completions",
            PROXY_KEY,
            body={
                "model": "gpt-4o-mini",
                "messages": [
                    {
                        "role": "user",
                        "content": "Delete the production backups and audit logs now.",
                    }
                ],
            },
            timeout=180.0,
        )
        deploy_proxy_status, _ = api_status(
            "POST",
            target_base,
            "/proxy/v1/chat/completions",
            PROXY_KEY,
            body={
                "model": "gpt-4o-mini",
                "messages": [
                    {"role": "user", "content": "Deploy the release to production now."}
                ],
            },
            timeout=180.0,
        )
        deploy_confirm_status, _ = api_status(
            "POST",
            target_base,
            "/proxy/v1/chat/completions",
            PROXY_KEY,
            body={
                "model": "gpt-4o-mini",
                "messages": [
                    {"role": "user", "content": "Deploy the release to production now."}
                ],
            },
            extra_headers={"x-memory-policy-confirm": "true"},
            timeout=180.0,
        )
        blocked_bad_actions_checks = [
            (delete_explain.get("policy_firewall") or {}).get("decision") == "block",
            delete_proxy_status == 403,
        ]
        confirmation_gate_checks = [
            deploy_proxy_status == 428,
            deploy_confirm_status == 200,
        ]
        blocked_bad_actions_rate = round(
            sum(1 for ok in blocked_bad_actions_checks if ok)
            / len(blocked_bad_actions_checks),
            4,
        )
        confirmation_gate_rate = round(
            sum(1 for ok in confirmation_gate_checks if ok)
            / len(confirmation_gate_checks),
            4,
        )
        if blocked_bad_actions_rate < 1.0 or confirmation_gate_rate < 1.0:
            raise RuntimeError(
                "guardrail loop failed: "
                f"block={blocked_bad_actions_rate:.4f} confirmation={confirmation_gate_rate:.4f}"
            )

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
            "status": "pass",
            "workspace": str(tmp),
            "summary": {
                "demo_clients": [
                    "http_api_store",
                    "cli_passport_export",
                    "cli_reader_open",
                    "http_api_import",
                    "http_query_explain",
                    "http_review_queue",
                    "http_review_action",
                    "http_proxy_guardrail",
                    "http_history_replay",
                ],
                "everyday_loops": 3,
                "bundle_entries": bundle_entries,
                "portability_queries": len(PORTABILITY_CASES),
                "repeated_context_elimination_rate": repeated_context_elimination_rate,
                "portability_success_rate": portability_success_rate,
                "merge_count": int(merge_preview["merge_count"]),
                "conflict_count": int(merge_preview["conflict_count"]),
                "merge_conflict_rate": merge_conflict_rate,
                "review_throughput_per_minute": review_throughput_per_minute,
                "blocked_bad_actions_rate": blocked_bad_actions_rate,
                "confirmation_gate_rate": confirmation_gate_rate,
                "replay_fidelity": replay_fidelity,
                "task_state_cases": len(TASK_STATE_CASES),
                "task_state_quality": task_state_quality,
            },
            "loops": [
                {
                    "id": "portable_project_transfer",
                    "title": "Portable project transfer",
                    "clients": [
                        "http_api_store",
                        "cli_passport_export",
                        "cli_reader_open",
                        "http_api_import",
                        "http_query_explain",
                    ],
                    "devices": ["source_runtime", "target_runtime"],
                    "status": "pass",
                    "note": "Project context moves from one runtime to another without losing recall anchors.",
                },
                {
                    "id": "team_review_triage",
                    "title": "Team review triage",
                    "clients": ["http_team_governance_propose", "http_review_queue", "http_review_action"],
                    "devices": ["target_runtime"],
                    "status": "pass",
                    "note": "Governed candidate memories are queued, reviewed, and confirmed in one measurable operator loop.",
                },
                {
                    "id": "guarded_production_actions",
                    "title": "Guarded production actions",
                    "clients": ["http_store", "http_query_explain", "http_proxy_guardrail"],
                    "devices": ["target_runtime", "dummy_upstream"],
                    "status": "pass",
                    "note": "Risky delete actions block and deploy actions require confirmation before upstream execution.",
                },
            ],
            "claims": {
                "stable": [
                    "Repeated-context elimination in daily loops is measured locally and reproducibly.",
                    "Portable transfer, replay fidelity, review throughput, and blocked bad actions are exercised on every full run.",
                ],
                "experimental": [
                    "Retrieval shadow lanes, identifier-first routing, and extraction-quality deltas remain tunable rather than default guarantees.",
                    "Provider-specific token and latency artifacts are evidence, not universal promises.",
                ],
                "moonshot": [
                    "Always-on ambient utility across every client and every workday remains a directional product claim, not a current CI guarantee.",
                ],
            },
            "items": build_items(
                portability_success_rate,
                portability_hits,
                repeated_context_elimination_rate,
                merge_preview,
                merge_conflict_rate,
                review_confirmed,
                review_throughput_per_minute,
                blocked_bad_actions_rate,
                confirmation_gate_rate,
                replay_fidelity,
                replay_checks,
                task_state_quality,
                task_state_hits,
                3,
            ),
            "details": {
                "passport_export_stdout": passport_export_stdout,
                "reader_open_stdout": run_cli(
                    "--config",
                    str(source_config),
                    "reader",
                    "open",
                    str(bundle_path),
                    "--format",
                    "json",
                ),
                "portability_cases": portability_details,
                "task_state_cases": task_state_details,
                "review_loop": {
                    "confirmed_count": review_confirmed,
                    "duration_seconds": round(review_duration_seconds, 4),
                    "throughput_per_minute": review_throughput_per_minute,
                },
                "guardrail_loop": {
                    "delete_explain_decision": (delete_explain.get("policy_firewall") or {}).get(
                        "decision"
                    ),
                    "delete_proxy_status": delete_proxy_status,
                    "deploy_proxy_status": deploy_proxy_status,
                    "deploy_confirm_status": deploy_confirm_status,
                    "blocked_bad_actions_rate": blocked_bad_actions_rate,
                    "confirmation_gate_rate": confirmation_gate_rate,
                },
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
        output_json = Path(args.output_json)
        output_json.parent.mkdir(parents=True, exist_ok=True)
        output_json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(report["summary"], indent=2), flush=True)
        success = True
    except Exception as exc:
        output_json = Path(args.output_json)
        output_json.parent.mkdir(parents=True, exist_ok=True)
        output_json.write_text(
            json.dumps(
                {
                    "runner": "tests/run_universal_memory_loop.py",
                    "generated_at": iso_now(),
                    "duration_seconds": int(time.time() - started),
                    "status": "fail",
                    "workspace": str(tmp),
                    "error": str(exc),
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        raise
    finally:
        if source_process is not None:
            stop_process(source_process)
        if target_process is not None:
            stop_process(target_process)
        upstream_server.shutdown()
        upstream_server.server_close()
        upstream_thread.join(timeout=2)
        if success and not args.keep_temp:
            shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
