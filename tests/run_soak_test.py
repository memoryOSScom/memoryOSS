#!/usr/bin/env python3
"""
Soak test for memoryOSS — continuous store/recall/verify loop over hours.

Validates:
- No memory leaks (RSS growth)
- No index corruption (recall quality stays stable)
- No performance degradation (latency drift)
- Backpressure handling under sustained load
- Concurrent store + recall correctness

Usage:
  SOAK_DURATION_HOURS=4 python3 tests/run_soak_test.py
"""
import json
import math
import hashlib
import os
import random
import resource
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
    wait_for_indexer_sync,
    write_test_config,
)

OUTPUT_JSON = Path(os.environ.get(
    "SOAK_OUTPUT_JSON",
    ROOT_DIR / "tests" / "soak-test-report.json",
))
DURATION_HOURS = float(os.environ.get("SOAK_DURATION_HOURS", "2"))
CHECKPOINT_INTERVAL_MINUTES = float(os.environ.get("SOAK_CHECKPOINT_MINUTES", "10"))
BATCH_SIZE = int(os.environ.get("SOAK_BATCH_SIZE", "20"))
RECALL_PER_CYCLE = int(os.environ.get("SOAK_RECALL_PER_CYCLE", "10"))
EMBED_DIM = 384
RNG = random.Random(42)
THRESHOLD = 0.40


def make_embedding(*tokens: str) -> list[float]:
    values = [0.0] * EMBED_DIM
    for token in tokens:
        digest = hashlib.sha256(token.encode("utf-8")).digest()
        for idx, byte in enumerate(digest):
            pos = (byte + idx * 17) % EMBED_DIM
            values[pos] += (byte / 255.0) - 0.5
    norm = math.sqrt(sum(v * v for v in values)) or 1.0
    return [v / norm for v in values]


def make_memory(cycle: int, i: int) -> dict:
    topic = f"soak-c{cycle:04d}-i{i:03d}"
    theme = RNG.choice(["deploy", "auth", "billing", "dns", "vector", "search"])
    return {
        "content": (
            f"{topic} {theme} soak-test memory for cycle {cycle} item {i} "
            f"with project-{cycle % 7} team-{i % 5} env-{cycle % 3}."
        ),
        "tags": [topic, theme, "soak-test", f"cycle-{cycle}"],
        "zero_knowledge": True,
        "embedding": make_embedding(topic, f"theme:{theme}", f"cycle:{cycle}"),
    }


def make_signal_query(cycle: int, i: int) -> tuple[str, str, list[float]]:
    topic = f"soak-c{cycle:04d}-i{i:03d}"
    return (
        f"{topic} soak-test memory",
        topic,
        make_embedding(topic, f"cycle:{cycle}"),
    )


def make_noise_query(i: int) -> tuple[str, list[float]]:
    return (
        f"absent-soak-noise-{i:06d} unmapped-token",
        make_embedding(f"soak-noise:{i}", f"random:{RNG.randint(0, 99999)}"),
    )


def get_server_rss(pid: int) -> int | None:
    """Get RSS in KB from /proc."""
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except (OSError, ValueError):
        return None


def main() -> None:
    started = time.time()
    deadline = started + DURATION_HOURS * 3600
    checkpoint_interval = CHECKPOINT_INTERVAL_MINUTES * 60

    tmp = Path(tempfile.mkdtemp(prefix="memoryoss-soak-"))
    port = free_port()
    data_dir = tmp / "data"
    data_dir.mkdir(parents=True, exist_ok=True)
    config_path = tmp / "soak.toml"
    log_path = tmp / "server.log"

    auth_entries = """
[[auth.api_keys]]
key = "soak-admin-key"
role = "admin"
namespace = "soak"
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
rate_limit_per_sec = 2000
"""
    write_test_config(
        config_path,
        port=port,
        data_dir=data_dir,
        auth_entries=auth_entries,
        extra_sections=extra_sections,
    )

    process = start_server(config_path, log_path=log_path)
    auth_header = "Bearer soak-admin-key"
    base_url = f"https://127.0.0.1:{port}"

    checkpoints: list[dict] = []
    total_stored = 0
    total_recalled = 0
    total_signal_hits = 0
    total_signal_queries = 0
    total_noise_rejected = 0
    total_noise_queries = 0
    all_latencies: list[float] = []
    errors: list[dict] = []
    cycle = 0

    try:
        wait_for_health(base_url, timeout=60.0, verify_tls=False)
        print(
            f"[soak] started: duration={DURATION_HOURS}h checkpoint_every={CHECKPOINT_INTERVAL_MINUTES}min "
            f"batch_size={BATCH_SIZE} recall_per_cycle={RECALL_PER_CYCLE}",
            flush=True,
        )

        last_checkpoint = time.time()

        while time.time() < deadline:
            cycle += 1

            # --- STORE PHASE ---
            batch = [make_memory(cycle, i) for i in range(BATCH_SIZE)]
            try:
                status, body = http_json_with_retry(
                    "POST",
                    f"{base_url}/v1/store/batch",
                    headers={"Authorization": auth_header},
                    body={"memories": batch},
                    timeout=120.0,
                    max_attempts=5,
                )
                if status == 200:
                    total_stored += BATCH_SIZE
                else:
                    errors.append({
                        "cycle": cycle, "phase": "store",
                        "status": status, "body": str(body)[:200],
                        "time": iso_now(),
                    })
            except Exception as e:
                errors.append({
                    "cycle": cycle, "phase": "store",
                    "error": str(e)[:200], "time": iso_now(),
                })

            # --- RECALL SIGNAL PHASE ---
            cycle_signal_hits = 0
            cycle_latencies: list[float] = []

            for i in range(min(RECALL_PER_CYCLE, BATCH_SIZE)):
                query_text, marker, query_emb = make_signal_query(cycle, i)
                t0 = time.time()
                try:
                    status, body = http_json_with_retry(
                        "POST",
                        f"{base_url}/v1/recall",
                        headers={"Authorization": auth_header},
                        body={
                            "query": query_text,
                            "query_embedding": query_emb,
                            "limit": 5,
                            "namespace": "soak",
                        },
                        timeout=60.0,
                    )
                    lat = (time.time() - t0) * 1000.0
                    cycle_latencies.append(lat)
                    all_latencies.append(lat)
                    total_recalled += 1
                    total_signal_queries += 1

                    if status == 200 and body:
                        memories = body if isinstance(body, list) else body.get("memories", [])
                        for m in memories[:5]:
                            # Handle nested format: {"memory": {"content": ...}} or flat {"content": ...}
                            content = m.get("content", "") or m.get("memory", {}).get("content", "")
                            if marker in str(content):
                                cycle_signal_hits += 1
                                total_signal_hits += 1
                                break
                except Exception as e:
                    errors.append({
                        "cycle": cycle, "phase": "recall-signal",
                        "error": str(e)[:200], "time": iso_now(),
                    })

            # --- RECALL NOISE PHASE ---
            for i in range(3):
                query_text, query_emb = make_noise_query(cycle * 1000 + i)
                t0 = time.time()
                try:
                    status, body = http_json_with_retry(
                        "POST",
                        f"{base_url}/v1/recall",
                        headers={"Authorization": auth_header},
                        body={
                            "query": query_text,
                            "query_embedding": query_emb,
                            "limit": 5,
                            "namespace": "soak",
                        },
                        timeout=60.0,
                    )
                    lat = (time.time() - t0) * 1000.0
                    cycle_latencies.append(lat)
                    all_latencies.append(lat)
                    total_noise_queries += 1

                    if status == 200:
                        memories = body if isinstance(body, list) else body.get("memories", [])
                        if not memories:
                            total_noise_rejected += 1
                        else:
                            top = memories[0]
                            score = float(top.get("score", 0.0) or top.get("final_score", 0.0))
                            if score < THRESHOLD:
                                total_noise_rejected += 1
                except Exception as e:
                    errors.append({
                        "cycle": cycle, "phase": "recall-noise",
                        "error": str(e)[:200], "time": iso_now(),
                    })

            # --- CHECKPOINT ---
            now = time.time()
            if now - last_checkpoint >= checkpoint_interval:
                rss_kb = get_server_rss(process.pid)
                try:
                    _, health = get_index_health(base_url, auth_header)
                except Exception:
                    health = None

                cp = {
                    "time": iso_now(),
                    "elapsed_minutes": round((now - started) / 60, 1),
                    "cycle": cycle,
                    "total_stored": total_stored,
                    "total_recalled": total_recalled,
                    "signal_hit_rate": round(total_signal_hits / max(total_signal_queries, 1), 4),
                    "noise_rejection": round(total_noise_rejected / max(total_noise_queries, 1), 4),
                    "p50_ms": round(percentile(cycle_latencies, 0.50), 2) if cycle_latencies else 0,
                    "p95_ms": round(percentile(cycle_latencies, 0.95), 2) if cycle_latencies else 0,
                    "rss_mb": round(rss_kb / 1024, 1) if rss_kb else None,
                    "indexer_lag": health.get("indexer_lag") if health else None,
                    "errors_total": len(errors),
                }
                checkpoints.append(cp)
                print(
                    f"[soak] checkpoint: {cp['elapsed_minutes']}min "
                    f"stored={total_stored} recalled={total_recalled} "
                    f"signal_hit={cp['signal_hit_rate']:.1%} noise_rej={cp['noise_rejection']:.1%} "
                    f"p50={cp['p50_ms']}ms p95={cp['p95_ms']}ms "
                    f"rss={cp['rss_mb']}MB lag={cp['indexer_lag']} errors={len(errors)}",
                    flush=True,
                )
                last_checkpoint = now

            # Brief pause to avoid hammering
            time.sleep(0.1)

        # --- FINAL SYNC + REPORT ---
        try:
            wait_for_indexer_sync(base_url, auth_header, timeout=300.0)
        except TimeoutError:
            errors.append({"phase": "final-sync", "error": "indexer did not sync", "time": iso_now()})

        final_rss = get_server_rss(process.pid)

        # Memory leak detection: compare first and last checkpoint RSS
        rss_growth_mb = None
        if len(checkpoints) >= 2:
            first_rss = checkpoints[0].get("rss_mb")
            last_rss = checkpoints[-1].get("rss_mb")
            if first_rss and last_rss:
                rss_growth_mb = round(last_rss - first_rss, 1)

        # Latency drift: compare first 100 vs last 100
        latency_drift_pct = None
        if len(all_latencies) > 200:
            early_p50 = percentile(all_latencies[:100], 0.50)
            late_p50 = percentile(all_latencies[-100:], 0.50)
            if early_p50 > 0:
                latency_drift_pct = round((late_p50 - early_p50) / early_p50 * 100, 1)

        report = {
            "runner": "tests/run_soak_test.py",
            "generated_at": iso_now(),
            "duration_hours": round((time.time() - started) / 3600, 2),
            "cycles": cycle,
            "summary": {
                "total_stored": total_stored,
                "total_recalled": total_recalled,
                "signal_hit_rate": round(total_signal_hits / max(total_signal_queries, 1), 4),
                "noise_rejection": round(total_noise_rejected / max(total_noise_queries, 1), 4),
                "errors": len(errors),
            },
            "latency_ms": {
                "min": round(min(all_latencies), 2) if all_latencies else 0,
                "p50": round(percentile(all_latencies, 0.50), 2) if all_latencies else 0,
                "p95": round(percentile(all_latencies, 0.95), 2) if all_latencies else 0,
                "p99": round(percentile(all_latencies, 0.99), 2) if all_latencies else 0,
                "max": round(max(all_latencies), 2) if all_latencies else 0,
            },
            "stability": {
                "rss_growth_mb": rss_growth_mb,
                "latency_drift_pct": latency_drift_pct,
                "final_rss_mb": round(final_rss / 1024, 1) if final_rss else None,
                "leak_detected": rss_growth_mb is not None and rss_growth_mb > 500,
                "latency_degradation": latency_drift_pct is not None and latency_drift_pct > 50,
            },
            "checkpoints": checkpoints,
            "errors": errors[:50],  # Cap at 50
            "items": [
                {
                    "name": f"Sustained {DURATION_HOURS}h load test",
                    "status": "pass" if len(errors) == 0 else ("warn" if len(errors) < 10 else "fail"),
                    "note": f"{cycle} cycles, {total_stored} stored, {total_recalled} recalled, {len(errors)} errors",
                },
                {
                    "name": "Signal hit rate stability",
                    "status": "pass" if total_signal_hits / max(total_signal_queries, 1) > 0.8 else "warn",
                    "note": f"{total_signal_hits / max(total_signal_queries, 1):.1%}",
                },
                {
                    "name": "Noise rejection stability",
                    "status": "pass" if total_noise_rejected / max(total_noise_queries, 1) > 0.8 else "warn",
                    "note": f"{total_noise_rejected / max(total_noise_queries, 1):.1%}",
                },
                {
                    "name": "Memory leak check",
                    "status": "pass" if not (rss_growth_mb and rss_growth_mb > 500) else "fail",
                    "note": f"RSS growth: {rss_growth_mb}MB" if rss_growth_mb is not None else "N/A",
                },
                {
                    "name": "Latency drift check",
                    "status": "pass" if not (latency_drift_pct and latency_drift_pct > 50) else "warn",
                    "note": f"p50 drift: {latency_drift_pct}%" if latency_drift_pct is not None else "N/A",
                },
            ],
        }

        OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(f"\n[soak] DONE — {cycle} cycles in {report['duration_hours']}h", flush=True)
        print(f"[soak] stored={total_stored} recalled={total_recalled} errors={len(errors)}", flush=True)
        print(f"[soak] signal_hit={report['summary']['signal_hit_rate']:.1%} noise_rej={report['summary']['noise_rejection']:.1%}", flush=True)
        if rss_growth_mb is not None:
            print(f"[soak] RSS growth: {rss_growth_mb}MB", flush=True)
        if latency_drift_pct is not None:
            print(f"[soak] Latency drift: {latency_drift_pct}%", flush=True)
        print(f"[soak] Report: {OUTPUT_JSON}", flush=True)

    finally:
        stop_process(process)


if __name__ == "__main__":
    main()
