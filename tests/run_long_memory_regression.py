#!/usr/bin/env python3
import hashlib
import json
import math
import os
import tempfile
import time
from pathlib import Path

from runner_common import (
    free_port,
    http_json_with_retry,
    iso_now,
    percentile,
    start_server,
    stop_process,
    wait_for_health,
    wait_for_indexer_lag_below,
    wait_for_indexer_sync,
    write_test_config,
)


ROOT_DIR = Path(__file__).resolve().parent.parent
OUTPUT_JSON = Path(
    os.environ.get(
        "LONG_MEMORY_OUTPUT_JSON",
        ROOT_DIR / "tests" / "long-memory-regression-report.json",
    )
)
MEMORY_COUNT = int(os.environ.get("LONG_MEMORY_COUNT", "5000"))
BATCH_SIZE = int(os.environ.get("LONG_MEMORY_BATCH_SIZE", "200"))
EMBEDDING_MODE = os.environ.get("LONG_MEMORY_EMBEDDING_MODE", "client").strip().lower()
THRESHOLD = float(os.environ.get("LONG_MEMORY_THRESHOLD", "0.40"))
EMBED_DIM = int(os.environ.get("LONG_MEMORY_EMBED_DIM", "384"))
LAG_PAUSE_THRESHOLD = int(os.environ.get("LONG_MEMORY_LAG_PAUSE_THRESHOLD", "800"))
LAG_RESUME_THRESHOLD = int(os.environ.get("LONG_MEMORY_LAG_RESUME_THRESHOLD", "200"))
LAG_CHECK_EVERY_BATCHES = int(os.environ.get("LONG_MEMORY_LAG_CHECK_EVERY_BATCHES", "10"))

SENTINEL_MARKER = "LONG-MEMORY-SENTINEL-0001"
SENTINEL_QUERY = "LONG-MEMORY-SENTINEL-0001 rollback guardrail"
SENTINEL_CONTENT = (
    "LONG-MEMORY-SENTINEL-0001 deployment rollback guardrail for project-aurora. "
    "If production deploy metrics regress, revert within 15 minutes and notify ops-red."
)


def make_embedding(*tokens: str) -> list[float]:
    values = [0.0] * EMBED_DIM
    for token in tokens:
        digest = hashlib.sha256(token.encode("utf-8")).digest()
        for idx, byte in enumerate(digest):
            pos = (byte + idx * 17) % EMBED_DIM
            values[pos] += (byte / 255.0) - 0.5
    norm = math.sqrt(sum(value * value for value in values)) or 1.0
    return [value / norm for value in values]


def sentinel_memory() -> dict:
    memory = {
        "content": SENTINEL_CONTENT,
        "tags": ["regression", "sentinel", "rollback"],
        "agent": "long-memory-regression",
        "session": "sentinel-seed",
    }
    if EMBEDDING_MODE != "full":
        memory["zero_knowledge"] = True
        memory["embedding"] = make_embedding("sentinel", "rollback", "project:aurora")
    return memory


def background_memory(i: int) -> dict:
    memory = {
        "content": (
            f"background-memory-{i:05d} topic-{i % 97} routine notes about module-{i % 41}, "
            f"workspace-{i % 53}, archive-slot-{i % 31}, cleanup-ticket-{i:05d}, and "
            f"generic developer workflow context entry-{i:05d}."
        ),
        "tags": [f"topic-{i % 97}", "regression", "background"],
        "agent": "long-memory-regression",
        "session": f"background-{i // BATCH_SIZE}",
    }
    if EMBEDDING_MODE != "full":
        memory["zero_knowledge"] = True
        memory["embedding"] = make_embedding(
            f"topic:{i % 97}",
            f"module:{i % 41}",
            "theme:background",
            f"id:{i}",
        )
    return memory


def query_embedding() -> list[float]:
    return make_embedding("sentinel", "rollback", "project:aurora")


def top_final_result(explain_body: dict) -> dict:
    finals = explain_body.get("final_results") or []
    return finals[0] if finals else {}


def main() -> None:
    started = time.time()
    tmp = Path(tempfile.mkdtemp(prefix="memoryoss-long-regression-"))
    port = free_port()
    data_dir = tmp / "data"
    data_dir.mkdir(parents=True, exist_ok=True)
    config_path = tmp / "long-memory.toml"
    log_path = tmp / "server.log"

    auth_entries = """
[[auth.api_keys]]
key = "long-memory-admin-key"
role = "admin"
namespace = "regression"
"""
    extra_sections = f"""
[proxy]
enabled = true
passthrough_auth = false
upstream_url = "https://api.openai.com/v1"
default_memory_mode = "readonly"
min_recall_score = {THRESHOLD}
extraction_enabled = false

[limits]
rate_limit_per_sec = 5000

[trust]
semantic_dedup_threshold = 0.9999
"""
    write_test_config(
        config_path,
        port=port,
        data_dir=data_dir,
        auth_entries=auth_entries,
        extra_sections=extra_sections,
    )

    process = start_server(config_path, log_path=log_path)
    base_url = f"https://127.0.0.1:{port}"
    auth_header = "Bearer long-memory-admin-key"
    lag_waits = 0
    retry_waits = 0
    batch_latencies_ms: list[float] = []

    try:
        wait_for_health(base_url, timeout=45.0, verify_tls=False)
        print(
            f"[long-regression] mode={EMBEDDING_MODE} background_memories={MEMORY_COUNT} batch_size={BATCH_SIZE}",
            flush=True,
        )

        sentinel_status, sentinel_body = http_json_with_retry(
            "POST",
            f"{base_url}/v1/store",
            headers={"Authorization": auth_header},
            body=sentinel_memory(),
            timeout=120.0,
            verify_tls=False,
        )
        if sentinel_status != 200:
            raise RuntimeError(
                f"sentinel store failed: status={sentinel_status} body={sentinel_body}"
            )
        sentinel_id = sentinel_body["id"]
        print(f"[long-regression] stored sentinel id={sentinel_id}", flush=True)

        for batch_index, offset in enumerate(range(0, MEMORY_COUNT, BATCH_SIZE), start=1):
            if batch_index > 1 and batch_index % LAG_CHECK_EVERY_BATCHES == 0:
                health_status, health_body = http_json_with_retry(
                    "GET",
                    f"{base_url}/v1/admin/index-health",
                    headers={"Authorization": auth_header},
                    timeout=20.0,
                    verify_tls=False,
                )
                if health_status == 200 and health_body:
                    lag = int(health_body.get("indexer_lag", 0))
                    print(
                        f"[long-regression] batch={batch_index} stored={min(offset, MEMORY_COUNT)}/{MEMORY_COUNT} lag={lag}",
                        flush=True,
                    )
                    if lag >= LAG_PAUSE_THRESHOLD:
                        wait_for_indexer_lag_below(
                            base_url,
                            auth_header,
                            target_lag=LAG_RESUME_THRESHOLD,
                            timeout=600.0,
                        )
                        lag_waits += 1

            batch = [
                background_memory(i)
                for i in range(offset, min(offset + BATCH_SIZE, MEMORY_COUNT))
            ]
            attempts = 0
            while True:
                attempts += 1
                t0 = time.time()
                status, body = http_json_with_retry(
                    "POST",
                    f"{base_url}/v1/store/batch",
                    headers={"Authorization": auth_header},
                    body={"memories": batch},
                    timeout=180.0,
                    verify_tls=False,
                    max_attempts=2,
                )
                batch_latencies_ms.append((time.time() - t0) * 1000.0)
                if status == 200:
                    break
                if status == 429 and attempts <= 20:
                    wait_for_indexer_lag_below(
                        base_url,
                        auth_header,
                        target_lag=LAG_RESUME_THRESHOLD,
                        timeout=600.0,
                    )
                    retry_waits += 1
                    continue
                raise RuntimeError(
                    f"background batch failed at offset {offset}: status={status} body={body}"
                )

        index_health = wait_for_indexer_sync(base_url, auth_header, timeout=180.0)
        print("[long-regression] indexer synced; starting sentinel recall checks", flush=True)

        recall_body = {
            "query": SENTINEL_QUERY,
            "limit": 10,
            "namespace": "regression",
        }
        explain_body = {
            "query": SENTINEL_QUERY,
            "limit": 10,
            "namespace": "regression",
        }
        if EMBEDDING_MODE != "full":
            recall_body["query_embedding"] = query_embedding()
            explain_body["query_embedding"] = query_embedding()

        t0 = time.time()
        recall_status, recall = http_json_with_retry(
            "POST",
            f"{base_url}/v1/recall",
            headers={"Authorization": auth_header},
            body=recall_body,
            timeout=120.0,
            verify_tls=False,
        )
        recall_latency_ms = (time.time() - t0) * 1000.0
        if recall_status != 200:
            raise RuntimeError(f"sentinel recall failed: {recall_status} {recall}")

        t0 = time.time()
        explain_status, explain = http_json_with_retry(
            "POST",
            f"{base_url}/v1/admin/query-explain",
            headers={"Authorization": auth_header},
            body=explain_body,
            timeout=120.0,
            verify_tls=False,
        )
        explain_latency_ms = (time.time() - t0) * 1000.0
        if explain_status != 200:
            raise RuntimeError(f"sentinel explain failed: {explain_status} {explain}")

        recall_results = recall if isinstance(recall, list) else recall.get("memories", [])
        recalled_contents = []
        for item in recall_results:
            if not isinstance(item, dict):
                continue
            memory = item.get("memory", {})
            if isinstance(memory, dict):
                recalled_contents.append(memory.get("content", ""))
        sentinel_rank = next(
            (idx + 1 for idx, content in enumerate(recalled_contents) if SENTINEL_MARKER in content),
            None,
        )

        top = top_final_result(explain if isinstance(explain, dict) else {})
        top_memory = top.get("memory", {}) if isinstance(top, dict) else {}
        top_content = top_memory.get("content", "")
        top_score = float(top.get("final_score") or 0.0) if isinstance(top, dict) else 0.0

        sentinel_found = sentinel_rank is not None
        sentinel_top_hit = SENTINEL_MARKER in top_content
        score_above_threshold = top_score >= THRESHOLD
        passed = sentinel_found and sentinel_top_hit and score_above_threshold

        report = {
            "runner": "tests/run_long_memory_regression.py",
            "generated_at": iso_now(),
            "duration_seconds": round(time.time() - started, 2),
            "parameters": {
                "background_memory_count": MEMORY_COUNT,
                "batch_size": BATCH_SIZE,
                "embedding_mode": EMBEDDING_MODE,
                "threshold": THRESHOLD,
            },
            "write": {
                "sentinel_id": sentinel_id,
                "total_memories": MEMORY_COUNT + 1,
                "lag_waits": lag_waits,
                "retry_waits": retry_waits,
                "batch_latency_ms": {
                    "min": round(min(batch_latencies_ms), 2) if batch_latencies_ms else 0.0,
                    "p50": round(percentile(batch_latencies_ms, 0.50), 2)
                    if batch_latencies_ms
                    else 0.0,
                    "p95": round(percentile(batch_latencies_ms, 0.95), 2)
                    if batch_latencies_ms
                    else 0.0,
                    "max": round(max(batch_latencies_ms), 2) if batch_latencies_ms else 0.0,
                },
            },
            "recall": {
                "sentinel_found": sentinel_found,
                "sentinel_rank": sentinel_rank,
                "sentinel_top_hit": sentinel_top_hit,
                "top_score": round(top_score, 4),
                "score_above_threshold": score_above_threshold,
                "top_content_prefix": top_content[:160],
                "recall_latency_ms": round(recall_latency_ms, 2),
                "explain_latency_ms": round(explain_latency_ms, 2),
            },
            "index_health": index_health,
            "items": [
                {
                    "name": "Stored sentinel before large background corpus",
                    "status": "pass",
                    "note": f"sentinel_id={sentinel_id}",
                },
                {
                    "name": f"Stored {MEMORY_COUNT:,} background memories",
                    "status": "pass",
                    "note": f"mode={EMBEDDING_MODE}, batch_size={BATCH_SIZE}",
                },
                {
                    "name": "Early sentinel remains retrievable after corpus growth",
                    "status": "pass" if passed else "fail",
                    "note": (
                        f"rank={sentinel_rank}, top_score={top_score:.4f}, "
                        f"top_hit={'yes' if sentinel_top_hit else 'no'}"
                    ),
                },
            ],
        }

        OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(report["recall"], indent=2), flush=True)

        if not passed:
            raise SystemExit(
                "long-memory regression failed: sentinel was not top retrievable hit after corpus growth"
            )
    finally:
        stop_process(process)


if __name__ == "__main__":
    main()
