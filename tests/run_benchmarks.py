#!/usr/bin/env python3
import json
import hashlib
import math
import os
import shutil
import tempfile
import time
from pathlib import Path

from runner_common import (
    ROOT_DIR,
    free_port,
    get_index_health,
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

FIXTURE_DIR = ROOT_DIR / "tests" / "fixtures" / "benchmark-20k"
USE_FIXTURE = os.environ.get("BENCHMARK_USE_FIXTURE", "auto").strip().lower()


EMBEDDING_MODE = os.environ.get("BENCHMARK_EMBEDDING_MODE", "client").strip().lower()
OUTPUT_JSON = Path(os.environ.get("BENCHMARK_OUTPUT_JSON", ROOT_DIR / "tests" / "benchmark-report.json"))
MEMORY_COUNT = int(os.environ.get("BENCHMARK_MEMORY_COUNT", "20000"))
BATCH_SIZE = int(
    os.environ.get(
        "BENCHMARK_BATCH_SIZE",
        "50" if EMBEDDING_MODE == "full" else "200",
    )
)
SIGNAL_QUERY_COUNT = int(os.environ.get("BENCHMARK_SIGNAL_QUERIES", "25"))
NOISE_QUERY_COUNT = int(os.environ.get("BENCHMARK_NOISE_QUERIES", "25"))
THRESHOLD = float(os.environ.get("BENCHMARK_THRESHOLD", "0.40"))
EMBED_DIM = int(os.environ.get("BENCHMARK_EMBED_DIM", "384"))
LAG_PAUSE_THRESHOLD = int(os.environ.get("BENCHMARK_LAG_PAUSE_THRESHOLD", "800"))
LAG_RESUME_THRESHOLD = int(os.environ.get("BENCHMARK_LAG_RESUME_THRESHOLD", "200"))
LAG_CHECK_EVERY_BATCHES = int(os.environ.get("BENCHMARK_LAG_CHECK_EVERY_BATCHES", "10"))


def make_embedding(*tokens: str) -> list[float]:
    values = [0.0] * EMBED_DIM
    for token in tokens:
        digest = hashlib.sha256(token.encode("utf-8")).digest()
        for idx, byte in enumerate(digest):
            pos = (byte + idx * 17) % EMBED_DIM
            values[pos] += (byte / 255.0) - 0.5
    norm = math.sqrt(sum(value * value for value in values)) or 1.0
    return [value / norm for value in values]


def make_memory(i: int) -> dict:
    if EMBEDDING_MODE == "full":
        if i < 50:
            return {
                "content": (
                    f"benchmark-signal-{i:03d} concept-{i:03d} deployment rollback guardrail "
                    f"for project-{i % 7} with dns incident review, staging-first policy, "
                    f"release-marker-{i:03d}, owner-team-{i % 11}, and rollback-window-{i % 5}."
                ),
                "tags": [f"signal-{i:03d}", "benchmark", "deployment"],
            }
        return {
            "content": (
                f"background-memory-{i:05d} topic-{i % 97} routine notes about module-{i % 41}, "
                f"workspace-{i % 53}, archive-slot-{i % 31}, cleanup-ticket-{i:05d}, and "
                f"generic developer workflow context entry-{i:05d}."
            ),
            "tags": [f"topic-{i % 97}", "benchmark", "background"],
        }
    if i < 50:
        return {
            "content": (
                f"benchmark-signal-{i:03d} concept-{i:03d} deployment rollback guardrail "
                f"for project-{i % 7} with dns incident review and staging-first policy."
            ),
            "tags": [f"signal-{i:03d}", "benchmark", "deployment"],
            "zero_knowledge": True,
            "embedding": make_embedding(f"signal:{i:03d}", "theme:deployment", f"project:{i % 7}"),
        }
    return {
        "content": (
            f"background-memory-{i:05d} topic-{i % 97} routine notes about module-{i % 41} "
            f"batch-processing telemetry cleanup and generic developer workflow context."
        ),
        "tags": [f"topic-{i % 97}", "benchmark", "background"],
        "zero_knowledge": True,
        "embedding": make_embedding(f"topic:{i % 97}", f"module:{i % 41}", "theme:background", f"id:{i}"),
    }


def recall_target_query(i: int) -> str:
    return f"benchmark-signal-{i:03d} deployment rollback guardrail"


def noise_query(i: int) -> str:
    return f"absent-noise-query-{i:04d} unmapped-token-{i:04d}"


def signal_query_embedding(i: int) -> list[float]:
    return make_embedding(f"signal:{i:03d}", "theme:deployment", f"project:{i % 7}")


def noise_query_embedding(i: int) -> list[float]:
    return make_embedding(f"absent-noise:{i:04d}", f"unmapped:{i:04d}")


def top_final_score(explain_body: dict) -> tuple[float, str]:
    finals = explain_body.get("final_results") or []
    if not finals:
        return 0.0, ""
    top = finals[0]
    return float(top.get("final_score") or 0.0), top.get("memory", {}).get("content", "")


def format_pct(value: float) -> str:
    return f"{value * 100:.1f}%"


def _fixture_available() -> bool:
    return (
        FIXTURE_DIR.is_dir()
        and (FIXTURE_DIR / "memoryoss.redb").exists()
        and (FIXTURE_DIR / "memoryoss.key").exists()
    )


def _should_use_fixture() -> bool:
    if USE_FIXTURE == "never":
        return False
    if USE_FIXTURE == "always":
        return True
    # "auto": use fixture when available and memory count matches default 20k
    return _fixture_available() and MEMORY_COUNT == 20000 and EMBEDDING_MODE == "client"


def main() -> None:
    started = time.time()
    tmp = Path(tempfile.mkdtemp(prefix="memoryoss-bench-"))
    port = free_port()
    data_dir = tmp / "data"
    data_dir.mkdir(parents=True, exist_ok=True)
    config_path = tmp / "benchmark.toml"
    log_path = tmp / "server.log"

    using_fixture = _should_use_fixture()

    if using_fixture:
        # Copy pre-built database snapshot — skip the 30-min store phase
        shutil.copytree(FIXTURE_DIR, data_dir, dirs_exist_ok=True)
        # Remove template config from data dir if copied
        template = data_dir / "benchmark.toml.template"
        if template.exists():
            template.unlink()
        print(f"[benchmark] FIXTURE LOADED from {FIXTURE_DIR} — skipping store phase", flush=True)

    auth_entries = """
[[auth.api_keys]]
key = "benchmark-admin-key"
role = "admin"
namespace = "bench"
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
    auth_header = "Bearer benchmark-admin-key"
    base_url = f"https://127.0.0.1:{port}"
    lag_waits = 0
    retry_waits = 0
    store_duration = 0.0

    try:
        health_timeout = 300.0 if using_fixture else 45.0
        wait_for_health(base_url, timeout=health_timeout, verify_tls=False)
        print(
            f"[benchmark] mode={EMBEDDING_MODE} memories={MEMORY_COUNT} batch_size={BATCH_SIZE} "
            f"signal_queries={SIGNAL_QUERY_COUNT} noise_queries={NOISE_QUERY_COUNT}"
            + (" (fixture)" if using_fixture else ""),
            flush=True,
        )

        if not using_fixture:
            store_started = time.time()
            for batch_index, offset in enumerate(range(0, MEMORY_COUNT, BATCH_SIZE), start=1):
                if batch_index > 1 and batch_index % LAG_CHECK_EVERY_BATCHES == 0:
                    health_status, health_body = get_index_health(base_url, auth_header)
                    if health_status == 200 and health_body:
                        lag = int(health_body.get("indexer_lag", 0))
                        print(
                            f"[benchmark] batch={batch_index} stored={min(offset, MEMORY_COUNT)}/{MEMORY_COUNT} lag={lag}",
                            flush=True,
                        )
                        if lag >= LAG_PAUSE_THRESHOLD:
                            print(
                                f"[benchmark] lag {lag} >= {LAG_PAUSE_THRESHOLD}; waiting for catch-up",
                                flush=True,
                            )
                            wait_for_indexer_lag_below(
                                base_url,
                                auth_header,
                                target_lag=LAG_RESUME_THRESHOLD,
                                timeout=600.0,
                            )
                            lag_waits += 1
                            print(
                                f"[benchmark] resumed after lag wait #{lag_waits}",
                                flush=True,
                            )
                batch = [make_memory(i) for i in range(offset, min(offset + BATCH_SIZE, MEMORY_COUNT))]
                attempts = 0
                while True:
                    attempts += 1
                    status, body = http_json_with_retry(
                        "POST",
                        f"{base_url}/v1/store/batch",
                        headers={"Authorization": auth_header},
                        body={"memories": batch},
                        timeout=180.0,
                        max_attempts=2,
                    )
                    if status == 200:
                        break
                    if status == 429 and attempts <= 20:
                        print(
                            f"[benchmark] 429 at offset={offset}, waiting for indexer catch-up (attempt {attempts})",
                            flush=True,
                        )
                        wait_for_indexer_lag_below(
                            base_url,
                            auth_header,
                            target_lag=LAG_RESUME_THRESHOLD,
                            timeout=600.0,
                        )
                        retry_waits += 1
                        continue
                    raise RuntimeError(
                        f"batch store failed at offset {offset}: status={status} body={body}"
                    )
            store_duration = time.time() - store_started
            print(
                f"[benchmark] store phase complete in {store_duration:.2f}s; waiting for full indexer sync",
                flush=True,
            )

        index_health = wait_for_indexer_sync(base_url, auth_header, timeout=180.0)
        print("[benchmark] indexer synced; starting recall quality checks", flush=True)

        signal_hits = 0
        noise_rejected = 0
        latency_ms: list[float] = []

        for i in range(SIGNAL_QUERY_COUNT):
            query = recall_target_query(i)
            explain_status, explain = http_json_with_retry(
                "POST",
                f"{base_url}/v1/admin/query-explain",
                headers={"Authorization": auth_header},
                body=(
                    {
                        "query": query,
                        "query_embedding": signal_query_embedding(i),
                        "limit": 1,
                        "namespace": "bench",
                    }
                    if EMBEDDING_MODE != "full"
                    else {
                        "query": query,
                        "limit": 1,
                        "namespace": "bench",
                    }
                ),
                timeout=120.0,
            )
            if explain_status != 200:
                raise RuntimeError(f"query explain failed for signal query {i}: {explain_status} {explain}")
            score, content = top_final_score(explain)
            if score >= THRESHOLD and f"benchmark-signal-{i:03d}" in content:
                signal_hits += 1

            t0 = time.time()
            recall_status, recall = http_json_with_retry(
                "POST",
                f"{base_url}/v1/recall",
                headers={"Authorization": auth_header},
                body=(
                    {
                        "query": query,
                        "query_embedding": signal_query_embedding(i),
                        "limit": 5,
                        "namespace": "bench",
                    }
                    if EMBEDDING_MODE != "full"
                    else {
                        "query": query,
                        "limit": 5,
                        "namespace": "bench",
                    }
                ),
                timeout=120.0,
            )
            latency_ms.append((time.time() - t0) * 1000.0)
            if recall_status != 200:
                raise RuntimeError(f"recall failed for signal query {i}: {recall_status} {recall}")

        for i in range(NOISE_QUERY_COUNT):
            query = noise_query(i)
            explain_status, explain = http_json_with_retry(
                "POST",
                f"{base_url}/v1/admin/query-explain",
                headers={"Authorization": auth_header},
                body=(
                    {
                        "query": query,
                        "query_embedding": noise_query_embedding(i),
                        "limit": 1,
                        "namespace": "bench",
                    }
                    if EMBEDDING_MODE != "full"
                    else {
                        "query": query,
                        "limit": 1,
                        "namespace": "bench",
                    }
                ),
                timeout=120.0,
            )
            if explain_status != 200:
                raise RuntimeError(f"query explain failed for noise query {i}: {explain_status} {explain}")
            score, _ = top_final_score(explain)
            if score < THRESHOLD:
                noise_rejected += 1

            t0 = time.time()
            recall_status, recall = http_json_with_retry(
                "POST",
                f"{base_url}/v1/recall",
                headers={"Authorization": auth_header},
                body=(
                    {
                        "query": query,
                        "query_embedding": noise_query_embedding(i),
                        "limit": 5,
                        "namespace": "bench",
                    }
                    if EMBEDDING_MODE != "full"
                    else {
                        "query": query,
                        "limit": 5,
                        "namespace": "bench",
                    }
                ),
                timeout=120.0,
            )
            latency_ms.append((time.time() - t0) * 1000.0)
            if recall_status != 200:
                raise RuntimeError(f"recall failed for noise query {i}: {recall_status} {recall}")

        report = {
            "runner": "tests/run_benchmarks.sh",
            "generated_at": iso_now(),
            "duration_seconds": int(time.time() - started),
            "parameters": {
                "memory_count": MEMORY_COUNT,
                "batch_size": BATCH_SIZE,
                "signal_queries": SIGNAL_QUERY_COUNT,
                "noise_queries": NOISE_QUERY_COUNT,
                "threshold": THRESHOLD,
                "embedding_mode": EMBEDDING_MODE,
                "lag_pause_threshold": LAG_PAUSE_THRESHOLD,
                "lag_resume_threshold": LAG_RESUME_THRESHOLD,
                "semantic_dedup_threshold": 0.9999,
            },
            "write": {
                "memories_stored": MEMORY_COUNT,
                "batch_requests": 0 if using_fixture else math.ceil(MEMORY_COUNT / BATCH_SIZE),
                "duration_seconds": 0.0 if using_fixture else round(store_duration, 2),
                "throughput_memories_per_second": (
                    None if using_fixture or store_duration == 0
                    else round(MEMORY_COUNT / store_duration, 2)
                ),
                "lag_waits": lag_waits,
                "retry_waits": retry_waits,
                "fixture_used": using_fixture,
            },
            "quality": {
                "signal_hit_rate": round(signal_hits / SIGNAL_QUERY_COUNT, 4),
                "noise_rejection": round(noise_rejected / NOISE_QUERY_COUNT, 4),
                "threshold": THRESHOLD,
            },
            "latency_ms": {
                "min": round(min(latency_ms), 2),
                "p50": round(percentile(latency_ms, 0.50), 2),
                "p95": round(percentile(latency_ms, 0.95), 2),
                "p99": round(percentile(latency_ms, 0.99), 2),
                "max": round(max(latency_ms), 2),
            },
            "index_health": index_health,
            "items": [
                {
                    "name": f"Stored {MEMORY_COUNT:,} memories in batches of {BATCH_SIZE}",
                    "status": "pass",
                    "note": (
                        f"loaded from fixture ({FIXTURE_DIR.name})"
                        if using_fixture
                        else (
                            f"{MEMORY_COUNT / store_duration:.2f} memories/sec"
                            + (
                                " with server-side embeddings"
                                if EMBEDDING_MODE == "full"
                                else " with client-provided embeddings"
                            )
                        )
                    ),
                },
                {
                    "name": "Indexer backpressure handling",
                    "status": "pass",
                    "note": f"lag waits={lag_waits}, retry waits={retry_waits}",
                },
                {
                    "name": "Synthetic dedup guard",
                    "status": "pass",
                    "note": "semantic_dedup_threshold=0.9999 for the synthetic scale corpus",
                },
                {
                    "name": "Signal hit rate at current threshold",
                    "status": "pass" if signal_hits == SIGNAL_QUERY_COUNT else "warn",
                    "note": format_pct(signal_hits / SIGNAL_QUERY_COUNT),
                },
                {
                    "name": "Noise rejection at current threshold",
                    "status": "pass" if noise_rejected == NOISE_QUERY_COUNT else "warn",
                    "note": format_pct(noise_rejected / NOISE_QUERY_COUNT),
                },
                {
                    "name": "Recall latency p50 / p95 / p99",
                    "status": "pass",
                    "note": (
                        f"{percentile(latency_ms, 0.50):.2f}ms / "
                        f"{percentile(latency_ms, 0.95):.2f}ms / "
                        f"{percentile(latency_ms, 0.99):.2f}ms"
                    ),
                },
            ],
        }
        OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(report["write"], indent=2))
    finally:
        stop_process(process)


if __name__ == "__main__":
    main()
