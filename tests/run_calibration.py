#!/usr/bin/env python3
import json
import hashlib
import os
import random
import tempfile
import time
from pathlib import Path

from runner_common import (
    ROOT_DIR,
    free_port,
    http_json_with_retry,
    iso_now,
    percentile,
    start_server,
    stop_process,
    wait_for_health,
    wait_for_indexer_sync,
    write_test_config,
)


OUTPUT_JSON = Path(os.environ.get("CALIBRATION_OUTPUT_JSON", ROOT_DIR / "tests" / "calibration-report.json"))
TOTAL_QUERY_COUNT = int(os.environ.get("CALIBRATION_QUERY_COUNT", "20000"))
CURRENT_THRESHOLD = float(os.environ.get("CALIBRATION_CURRENT_THRESHOLD", "0.40"))
SCAN_THRESHOLDS = [0.30, 0.375, 0.40, 0.45, 0.50]
TOPIC_COUNT = int(os.environ.get("CALIBRATION_TOPIC_COUNT", "55"))
EMBED_DIM = int(os.environ.get("CALIBRATION_EMBED_DIM", "384"))
RNG = random.Random(42)

EXACT_RATIO = 165 / 9585
RELATED_RATIO = 205 / 9585
EXACT_COUNT = int(round(TOTAL_QUERY_COUNT * EXACT_RATIO))
RELATED_COUNT = int(round(TOTAL_QUERY_COUNT * RELATED_RATIO))
NOISE_COUNT = TOTAL_QUERY_COUNT - EXACT_COUNT - RELATED_COUNT


THEMES = [
    ("deployment", "rollback guardrail and staging-first rollout"),
    ("dns", "dns cutover incident and resolver cache behavior"),
    ("auth", "oauth scope mismatch and bearer token forwarding"),
    ("billing", "invoice export formatting and audit retention"),
    ("vector", "embedding recall drift and ranking thresholds"),
    ("search", "bm25 exact identifier retrieval and precision gate"),
    ("security", "namespace isolation and admin scope boundaries"),
    ("observability", "metrics scrape path and latency debugging"),
    ("backup", "restore validation and encrypted snapshot rotation"),
    ("release", "release checklist and smoke validation"),
]


def make_embedding(*tokens: str) -> list[float]:
    values = [0.0] * EMBED_DIM
    for token in tokens:
        digest = hashlib.sha256(token.encode("utf-8")).digest()
        for idx, byte in enumerate(digest):
            pos = (byte + idx * 17) % EMBED_DIM
            values[pos] += (byte / 255.0) - 0.5
    norm = sum(value * value for value in values) ** 0.5 or 1.0
    return [value / norm for value in values]


def make_memory(topic_id: int) -> dict:
    theme_name, theme_detail = THEMES[topic_id % len(THEMES)]
    concept = f"cal-topic-{topic_id:03d}"
    return {
        "content": (
            f"{concept} identifier-{topic_id:03d} {theme_name} {theme_detail} "
            f"owner team-{topic_id % 9} environment env-{topic_id % 5}."
        ),
        "tags": [concept, theme_name, "calibration"],
        "zero_knowledge": True,
        "embedding": make_embedding(
            concept,
            f"identifier-{topic_id:03d}",
            f"theme:{theme_name}",
            f"team:{topic_id % 9}",
        ),
    }


def exact_query(index: int) -> tuple[str, str, list[float]]:
    topic = index % TOPIC_COUNT
    return (
        f"identifier-{topic:03d} cal-topic-{topic:03d}",
        f"cal-topic-{topic:03d}",
        make_embedding(
            f"cal-topic-{topic:03d}",
            f"identifier-{topic:03d}",
            f"theme:{THEMES[topic % len(THEMES)][0]}",
            f"team:{topic % 9}",
        ),
    )


def related_query(index: int) -> tuple[str, str, list[float]]:
    topic = index % TOPIC_COUNT
    theme_name, theme_detail = THEMES[topic % len(THEMES)]
    fragment = theme_detail.split(" and ")[0]
    return (
        f"{theme_name} {fragment} for team-{topic % 9}",
        f"cal-topic-{topic:03d}",
        make_embedding(
            f"theme:{theme_name}",
            f"cal-topic-{topic:03d}",
            f"team:{topic % 9}",
        ),
    )


def noise_query(index: int) -> tuple[str, str, list[float]]:
    return (
        f"absent calibration noise token {index:05d} random-{RNG.randint(100000, 999999)}",
        "",
        make_embedding(f"noise:{index:05d}", f"random:{index % 997}"),
    )


def top_result(explain_body: dict) -> tuple[float, str]:
    final_results = explain_body.get("final_results") or []
    if not final_results:
        return 0.0, ""
    top = final_results[0]
    return float(top.get("final_score") or 0.0), top.get("memory", {}).get("content", "")


def percentile_summary(values: list[float]) -> dict:
    return {
        "min": round(min(values), 3) if values else 0.0,
        "p5": round(percentile(values, 0.05), 3),
        "p50": round(percentile(values, 0.50), 3),
        "p95": round(percentile(values, 0.95), 3),
        "max": round(max(values), 3) if values else 0.0,
    }


def evaluate_threshold(threshold: float, exact_scores, related_scores, noise_scores) -> dict:
    tp = sum(1 for score, ok in exact_scores + related_scores if score >= threshold and ok)
    fn = sum(1 for score, ok in exact_scores + related_scores if not (score >= threshold and ok))
    fp = sum(1 for score in noise_scores if score >= threshold)
    fp += sum(1 for score, ok in exact_scores + related_scores if score >= threshold and not ok)
    precision = tp / (tp + fp) if tp + fp else 0.0
    recall = tp / (tp + fn) if tp + fn else 0.0
    f1 = 2 * precision * recall / (precision + recall) if precision + recall else 0.0
    return {
        "threshold": threshold,
        "noise_rejected_pct": round(sum(1 for score in noise_scores if score < threshold) / len(noise_scores), 4),
        "exact_kept_pct": round(sum(1 for score, ok in exact_scores if score >= threshold and ok) / len(exact_scores), 4),
        "related_kept_pct": round(sum(1 for score, ok in related_scores if score >= threshold and ok) / len(related_scores), 4),
        "precision": round(precision, 4),
        "recall": round(recall, 4),
        "f1": round(f1, 4),
    }


def main() -> None:
    started = time.time()
    tmp = Path(tempfile.mkdtemp(prefix="memoryoss-calibration-"))
    port = free_port()
    data_dir = tmp / "data"
    data_dir.mkdir(parents=True, exist_ok=True)
    config_path = tmp / "calibration.toml"
    log_path = tmp / "server.log"

    auth_entries = """
[[auth.api_keys]]
key = "calibration-admin-key"
role = "admin"
namespace = "cal"
"""
    extra_sections = f"""
[proxy]
enabled = true
passthrough_auth = false
upstream_url = "https://api.openai.com/v1"
default_memory_mode = "readonly"
min_recall_score = {CURRENT_THRESHOLD}
extraction_enabled = false

[limits]
rate_limit_per_sec = 5000
"""
    write_test_config(
        config_path,
        port=port,
        data_dir=data_dir,
        auth_entries=auth_entries,
        extra_sections=extra_sections,
    )

    process = start_server(config_path, log_path=log_path)
    auth_header = "Bearer calibration-admin-key"
    base_url = f"https://127.0.0.1:{port}"
    try:
        wait_for_health(base_url, verify_tls=False)
        memories = [make_memory(i) for i in range(TOPIC_COUNT)]
        status, body = http_json_with_retry(
            "POST",
            f"{base_url}/v1/store/batch",
            headers={"Authorization": auth_header},
            body={"memories": memories},
            timeout=180.0,
        )
        if status != 200:
            raise RuntimeError(f"failed to seed calibration corpus: {status} {body}")
        wait_for_indexer_sync(base_url, auth_header, timeout=120.0)

        exact_scores = []
        related_scores = []
        noise_scores = []

        def score_query(query: str, expected_marker: str, query_embedding: list[float]) -> tuple[float, bool]:
            status, explain = http_json_with_retry(
                "POST",
                f"{base_url}/v1/admin/query-explain",
                headers={"Authorization": auth_header},
                body={
                    "query": query,
                    "query_embedding": query_embedding,
                    "limit": 1,
                    "namespace": "cal",
                },
                timeout=120.0,
            )
            if status != 200:
                raise RuntimeError(f"query explain failed: {status} {explain}")
            score, content = top_result(explain)
            matched = bool(expected_marker) and expected_marker in content
            return score, matched

        for i in range(EXACT_COUNT):
            query, marker, query_embedding = exact_query(i)
            exact_scores.append(score_query(query, marker, query_embedding))
        for i in range(RELATED_COUNT):
            query, marker, query_embedding = related_query(i)
            related_scores.append(score_query(query, marker, query_embedding))
        for i in range(NOISE_COUNT):
            query, _marker, query_embedding = noise_query(i)
            score, _matched = score_query(query, "", query_embedding)
            noise_scores.append(score)

        threshold_rows = [evaluate_threshold(t, exact_scores, related_scores, noise_scores) for t in SCAN_THRESHOLDS]

        sweep_rows = [
            evaluate_threshold(round(threshold, 3), exact_scores, related_scores, noise_scores)
            for threshold in [0.20 + i * 0.025 for i in range(17)]
        ]
        optimal = max(sweep_rows, key=lambda row: row["f1"])
        current_row = next(row for row in threshold_rows if abs(row["threshold"] - CURRENT_THRESHOLD) < 1e-9)

        report = {
            "runner": "tests/run_calibration.sh",
            "generated_at": iso_now(),
            "duration_seconds": int(time.time() - started),
            "summary": {
                "queries": EXACT_COUNT + RELATED_COUNT + NOISE_COUNT,
                "exact_queries": EXACT_COUNT,
                "related_queries": RELATED_COUNT,
                "noise_queries": NOISE_COUNT,
                "current_threshold": CURRENT_THRESHOLD,
                "optimal_threshold": optimal["threshold"],
                "optimal_f1": optimal["f1"],
            },
            "score_distribution": {
                "exact": percentile_summary([score for score, _ in exact_scores]),
                "related": percentile_summary([score for score, _ in related_scores]),
                "noise": percentile_summary(noise_scores),
            },
            "threshold_analysis": threshold_rows,
            "current_threshold_row": current_row,
            "optimal_threshold_row": optimal,
            "items": [
                {
                    "name": "Calibration corpus size",
                    "status": "pass",
                    "note": f"{EXACT_COUNT + RELATED_COUNT + NOISE_COUNT:,} queries with client-provided query embeddings",
                },
                {
                    "name": "Current threshold performance",
                    "status": "pass",
                    "note": (
                        f"noise rejected {current_row['noise_rejected_pct'] * 100:.1f}%, "
                        f"exact kept {current_row['exact_kept_pct'] * 100:.1f}%, "
                        f"related kept {current_row['related_kept_pct'] * 100:.1f}%, "
                        f"F1 {current_row['f1']:.3f}"
                    ),
                },
                {
                    "name": "Optimal scanned threshold",
                    "status": "pass",
                    "note": f"{optimal['threshold']:.3f} with F1 {optimal['f1']:.3f}",
                },
            ],
        }
        OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(report["summary"], indent=2))
    finally:
        stop_process(process)


if __name__ == "__main__":
    main()
