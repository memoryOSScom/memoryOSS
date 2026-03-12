#!/usr/bin/env python3
import json
import hashlib
import math
import os
import shutil
import ssl
import tempfile
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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
DUPLICATE_SAMPLE_LIMIT = int(os.environ.get("BENCHMARK_DUPLICATE_SAMPLE_LIMIT", "100"))

MIN_SUBSTRING_TOKENS = 5
MIN_JACCARD_TOKENS = 6
JACCARD_DUP_THRESHOLD = 0.92


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


def text_tokens(content: str) -> list[str]:
    tokens: list[str] = []
    current: list[str] = []
    for ch in content:
        if ch.isalnum():
            current.append(ch.lower())
        elif current:
            tokens.append("".join(current))
            current = []
    if current:
        tokens.append("".join(current))
    return tokens


def token_jaccard(a_tokens: list[str], b_tokens: list[str]) -> float:
    a_set = set(a_tokens)
    b_set = set(b_tokens)
    if not a_set or not b_set:
        return 0.0
    return len(a_set & b_set) / len(a_set | b_set)


def are_structural_duplicates(a: str, b: str) -> bool:
    a_tokens = text_tokens(a)
    b_tokens = text_tokens(b)
    a_norm = " ".join(a_tokens)
    b_norm = " ".join(b_tokens)
    if not a_norm or not b_norm:
        return False
    if a_norm == b_norm:
        return True

    shorter_norm, shorter_tokens, longer_norm = (
        (a_norm, a_tokens, b_norm)
        if len(a_tokens) <= len(b_tokens)
        else (b_norm, b_tokens, a_norm)
    )
    if len(shorter_tokens) >= MIN_SUBSTRING_TOKENS and shorter_norm in longer_norm:
        return True

    return (
        len(a_tokens) >= MIN_JACCARD_TOKENS
        and len(b_tokens) >= MIN_JACCARD_TOKENS
        and token_jaccard(a_tokens, b_tokens) >= JACCARD_DUP_THRESHOLD
    )


def duplicate_rate_for_sample(contents: list[str]) -> float:
    if not contents:
        return 0.0

    assigned: set[int] = set()
    merged = 0
    for i, content in enumerate(contents):
        if i in assigned:
            continue
        cluster = [i]
        for j in range(i + 1, len(contents)):
            if j in assigned:
                continue
            if are_structural_duplicates(content, contents[j]):
                cluster.append(j)
        if len(cluster) < 2:
            continue
        assigned.update(cluster)
        merged += len(cluster) - 1
    return merged / len(contents)


class DummyOpenAIUpstream(ThreadingHTTPServer):
    def __init__(self, server_address):
        super().__init__(server_address, DummyOpenAIHandler)
        self.captured_requests: list[dict] = []


class DummyOpenAIHandler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:  # noqa: N802 - stdlib handler naming
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length > 0 else b""
        body = None
        if raw:
            try:
                body = json.loads(raw.decode("utf-8"))
            except Exception:
                body = {"raw": raw.decode("utf-8", errors="replace")}

        self.server.captured_requests.append(  # type: ignore[attr-defined]
            {
                "path": self.path,
                "body": body,
            }
        )

        payload = json.dumps(
            {
                "id": "chatcmpl-benchmark-upstream",
                "object": "chat.completion",
                "choices": [
                    {
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "benchmark upstream output",
                        },
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "prompt_tokens": 16,
                    "completion_tokens": 4,
                    "total_tokens": 20,
                },
            }
        ).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, format: str, *args) -> None:  # noqa: A003 - stdlib signature
        return


def start_dummy_upstream():
    port = free_port()
    server = DummyOpenAIUpstream(("127.0.0.1", port))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return port, server, thread


def stop_dummy_upstream(server: DummyOpenAIUpstream, thread: threading.Thread) -> None:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)


def request_has_memory_context(request: dict) -> bool:
    body = request.get("body") or {}
    for message in body.get("messages") or []:
        if message.get("role") != "system":
            continue
        content = message.get("content") or ""
        if isinstance(content, str) and "<memory_context" in content:
            return True
    return False


def proxy_request(
    base_url: str,
    proxy_key: str,
    query: str,
    *,
    timeout: float = 120.0,
) -> tuple[int, dict | None]:
    payload = json.dumps(
        {
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": query}],
        }
    ).encode("utf-8")
    request = urllib.request.Request(
        f"{base_url}/proxy/v1/chat/completions",
        data=payload,
        headers={
            "Authorization": f"Bearer {proxy_key}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    context = ssl._create_unverified_context()

    try:
        with urllib.request.urlopen(request, timeout=timeout, context=context) as response:
            raw = response.read()
            body = json.loads(raw.decode("utf-8")) if raw else None
            return response.status, body
    except urllib.error.HTTPError as exc:
        raw = exc.read()
        body = json.loads(raw.decode("utf-8")) if raw else None
        return exc.code, body


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
    upstream_port, upstream_server, upstream_thread = start_dummy_upstream()
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
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
default_memory_mode = "full"
min_recall_score = {THRESHOLD}
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "benchmark-proxy-key"
upstream_key = "upstream-benchmark-key"
namespace = "bench"

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

        lifecycle_status, lifecycle = http_json_with_retry(
            "GET",
            (
                f"{base_url}/v1/admin/lifecycle"
                f"?status=active&limit={max(1, min(DUPLICATE_SAMPLE_LIMIT, 100))}"
            ),
            headers={"Authorization": auth_header},
            timeout=120.0,
            verify_tls=False,
        )
        if lifecycle_status != 200:
            raise RuntimeError(f"lifecycle view failed: {lifecycle_status} {lifecycle}")
        lifecycle_summary = lifecycle.get("summary") or {}
        active_sample = lifecycle.get("memories") or []
        observed_duplicate_rate = duplicate_rate_for_sample(
            [
                memory.get("content", "")
                for memory in active_sample
                if isinstance(memory.get("content"), str)
            ]
        )

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

        positive_injection_hits = 0
        negative_false_injections = 0
        positive_probe_count = min(10, SIGNAL_QUERY_COUNT)
        negative_probe_count = min(10, NOISE_QUERY_COUNT)

        for i in range(positive_probe_count):
            before = len(upstream_server.captured_requests)
            proxy_status, proxy_body = proxy_request(
                base_url,
                "benchmark-proxy-key",
                recall_target_query(i),
            )
            if proxy_status != 200:
                raise RuntimeError(
                    f"proxy injection probe failed for signal query {i}: {proxy_status} {proxy_body}"
                )
            after = len(upstream_server.captured_requests)
            if after <= before:
                raise RuntimeError("dummy upstream did not receive signal proxy request")
            if request_has_memory_context(upstream_server.captured_requests[-1]):
                positive_injection_hits += 1

        for i in range(negative_probe_count):
            before = len(upstream_server.captured_requests)
            proxy_status, proxy_body = proxy_request(
                base_url,
                "benchmark-proxy-key",
                noise_query(i),
            )
            if proxy_status != 200:
                raise RuntimeError(
                    f"proxy injection probe failed for noise query {i}: {proxy_status} {proxy_body}"
                )
            after = len(upstream_server.captured_requests)
            if after <= before:
                raise RuntimeError("dummy upstream did not receive noise proxy request")
            if request_has_memory_context(upstream_server.captured_requests[-1]):
                negative_false_injections += 1

        positive_injection_rate = positive_injection_hits / positive_probe_count
        false_positive_injection_rate = negative_false_injections / negative_probe_count

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
            "memory_hygiene": {
                "active_memory_size": int(lifecycle_summary.get("active", 0)),
                "candidate_memory_size": int(lifecycle_summary.get("candidate", 0)),
                "contested_memory_size": int(lifecycle_summary.get("contested", 0)),
                "stale_memory_size": int(lifecycle_summary.get("stale", 0)),
                "archived_memory_size": int(lifecycle_summary.get("archived", 0)),
                "duplicate_rate_before": round(observed_duplicate_rate, 4),
                "duplicate_sample_size": len(active_sample),
            },
            "proxy_quality": {
                "positive_injection_hit_rate": round(positive_injection_rate, 4),
                "false_positive_injection_rate": round(false_positive_injection_rate, 4),
                "positive_probe_count": positive_probe_count,
                "negative_probe_count": negative_probe_count,
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
                    "name": "Active memory size",
                    "status": "pass",
                    "note": f"{int(lifecycle_summary.get('active', 0)):,} active memories in benchmark namespace",
                },
                {
                    "name": "Observed duplicate rate",
                    "status": "pass",
                    "note": (
                        f"{format_pct(observed_duplicate_rate)} across "
                        f"{len(active_sample)} sampled active memories"
                    ),
                },
                {
                    "name": "False-positive injection rate",
                    "status": "pass" if negative_false_injections == 0 else "warn",
                    "note": (
                        f"{format_pct(false_positive_injection_rate)} "
                        f"across {negative_probe_count} negative proxy probes"
                    ),
                },
                {
                    "name": "Positive injection hit rate",
                    "status": "pass" if positive_injection_hits == positive_probe_count else "warn",
                    "note": (
                        f"{format_pct(positive_injection_rate)} "
                        f"across {positive_probe_count} positive proxy probes"
                    ),
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
        stop_dummy_upstream(upstream_server, upstream_thread)


if __name__ == "__main__":
    main()
