#!/usr/bin/env python3
import argparse
import datetime as dt
import json
import re
from pathlib import Path

ROOT_DIR = Path(__file__).resolve().parent.parent

UNIT_DISPLAY = {
    "fusion::tests::test_collapse_explained_entries_merges_duplicates": "Fusion explain collapse merges structural duplicates",
    "fusion::tests::test_collapse_scored_memories_merges_duplicates": "Fusion recall collapse merges structural duplicates",
    "fusion::tests::test_structural_duplicate_by_containment": "Structural duplicate detection handles containment",
    "fusion::tests::test_fuse_contents_unions_unique_sentences": "Content fusion preserves unique sentences",
    "intent_cache::tests::test_canonicalize_all_stopwords": "Intent cache canonicalization handles stopword-only input",
    "intent_cache::tests::test_canonicalize_basic": "Intent cache canonicalization normalizes basic queries",
    "intent_cache::tests::test_canonicalize_preserves_meaningful_terms": "Intent cache keeps meaningful query terms",
    "intent_cache::tests::test_cache_hit_miss": "Intent cache hit/miss behavior",
    "intent_cache::tests::test_cache_session_isolation": "Intent cache session isolation",
    "intent_cache::tests::test_canonicalize_strips_punctuation": "Intent cache strips punctuation",
    "intent_cache::tests::test_canonicalize_sorts_and_deduplicates": "Intent cache sorts and deduplicates tokens",
    "intent_cache::tests::test_cache_invalidation": "Intent cache invalidation on writes",
    "memory::tests::test_confirm_from_signal_promotes_candidate": "Candidate memories promote from repeated signal",
    "memory::tests::test_confirm_from_signal_does_not_revive_superseded_stale_memory": "Superseded stale memories do not revive automatically",
    "prefetch::tests::test_record_and_dedup": "Prefetch query recording deduplicates repeated prompts",
    "prefetch::tests::test_session_seen_tracking": "Prefetch session tracking avoids duplicate warmups",
    "prefetch::tests::test_ring_buffer_eviction": "Prefetch ring buffer evicts oldest entries",
}

INTEGRATION_DISPLAY = {
    "test_store_recall_update_forget": "Store -> Recall -> Update -> Forget roundtrip",
    "test_query_explain_returns_real_score_breakdown": "Admin query explain exposes real score breakdown",
    "test_feedback_updates_memory_lifecycle": "Feedback transitions memory lifecycle states",
    "test_lifecycle_view_filters_and_summarizes": "Lifecycle admin view filters and summarizes status",
    "test_auth_rejected_without_key": "Unauthorized requests are rejected cleanly",
    "test_mcp_http_roundtrip": "MCP stdio roundtrip: initialize, tools/list, store, recall, update, forget",
    "test_mcp_unknown_tool": "MCP unknown tool returns JSON-RPC error",
    "test_concurrent_access": "Concurrent stores and recalls stay stable",
    "test_proxy_error_without_upstream": "Proxy handles upstream failure without panicking",
    "test_proxy_connection_paths_cover_openai_and_anthropic": "Proxy transport paths cover OpenAI and Anthropic connections",
    "test_sharing_connections_cover_owner_and_grantee_paths": "Sharing paths cover owner, grantee, grant, revoke, accessible",
    "test_gdpr_connections_cover_export_access_and_certified_forget": "GDPR export, access, and certified forget roundtrip",
    "test_key_rotation_connections_cover_rotate_list_revoke_and_read": "Key rotation paths cover rotate, list, revoke, readability",
    "test_decay_and_migrate_cli_connections": "Decay and migrate CLI commands work against real data",
}

INTEGRATION_GROUPS = {
    "Core API & Lifecycle": {
        "test_store_recall_update_forget",
        "test_query_explain_returns_real_score_breakdown",
        "test_feedback_updates_memory_lifecycle",
        "test_lifecycle_view_filters_and_summarizes",
        "test_auth_rejected_without_key",
        "test_concurrent_access",
    },
    "Connection Paths": {
        "test_proxy_error_without_upstream",
        "test_proxy_connection_paths_cover_openai_and_anthropic",
        "test_sharing_connections_cover_owner_and_grantee_paths",
        "test_gdpr_connections_cover_export_access_and_certified_forget",
        "test_key_rotation_connections_cover_rotate_list_revoke_and_read",
        "test_decay_and_migrate_cli_connections",
    },
    "MCP": {
        "test_mcp_http_roundtrip",
        "test_mcp_unknown_tool",
    },
}

WIZARD_ASSERTIONS = [
    "Setup wizard writes a config file",
    'Setup wizard persists `default_memory_mode = "readonly"`',
    "Setup wizard reaches the ready banner and serves `/health`",
]

COVERAGE_GAPS: list[str] = []  # All former gaps are now covered by run_coverage_gaps.py


def parse_steps(path: Path):
    steps = []
    with path.open("r", encoding="utf-8") as handle:
        for raw in handle:
            raw = raw.rstrip("\n")
            if not raw:
                continue
            slug, label, status, duration, log_path = raw.split("\t", 4)
            steps.append(
                {
                    "slug": slug,
                    "label": label,
                    "status": status,
                    "duration_seconds": int(duration),
                    "log_path": log_path,
                }
            )
    return steps


def display_path(raw_path: str) -> str:
    path = Path(raw_path)
    try:
        return str(path.resolve().relative_to(ROOT_DIR))
    except Exception:
        return path.name


def load_optional_json(path: str | None):
    if not path:
        return None
    file_path = Path(path)
    if not file_path.exists():
        return None
    return json.loads(file_path.read_text(encoding="utf-8"))


def artifact_suffix(report: dict | None, step: dict | None = None) -> str:
    if not report:
        return ""

    fragments = []
    generated_at = report.get("generated_at")
    if generated_at:
        try:
            stamp = dt.datetime.fromisoformat(generated_at.replace("Z", "+00:00"))
            fragments.append(f"artifact {stamp.date().isoformat()}")
        except Exception:
            fragments.append("artifact available")
    if step and step.get("status") != "pass":
        fragments.append(f"current run step {step['status']}")
    return f" ({'; '.join(fragments)})" if fragments else ""


def parse_cargo_tests(log_text: str):
    current = None
    unit = []
    integration = []
    for line in log_text.splitlines():
        if "Running unittests src/main.rs" in line:
            current = "unit"
            continue
        if "Running tests/integration.rs" in line:
            current = "integration"
            continue
        match = re.match(r"^test (.+) \.\.\. ok$", line.strip())
        if not match:
            continue
        name = match.group(1)
        if current == "unit":
            unit.append(name)
        elif current == "integration":
            integration.append(name)
    return unit, integration


def parse_typescript_tests(log_text: str):
    tests = []
    for line in log_text.splitlines():
        match = re.match(r"^\s+ok \d+ - (.+)$", line)
        if match:
            tests.append(match.group(1))
    return tests


def item(name: str, status: str = "pass", note: str | None = None):
    result = {"name": name, "status": status}
    if note:
        result["note"] = note
    return result


def format_rate(value: float | None) -> str:
    if value is None:
        return "n/a"
    return f"{value * 100:.1f}%"


def format_shadow_delta(metric: str, delta: float) -> str:
    if metric.startswith("latency_ms"):
        return f"{delta:+.0f} ms"
    return f"{delta * 100:+.1f} pp"


def extraction_lane_items(label: str, summary: dict) -> list[dict]:
    prefix = f"{label} " if label else ""
    items = []
    if summary.get("project_specific_fact_rate") is not None:
        items.append(
            item(
                f"{prefix}project-specific fact rate",
                "pass",
                format_rate(summary["project_specific_fact_rate"]),
            )
        )
    if summary.get("duplicate_fact_rate") is not None:
        duplicate_note = format_rate(summary["duplicate_fact_rate"])
        kept_duplicate_rate = summary.get("kept_duplicate_fact_rate")
        if kept_duplicate_rate is not None:
            duplicate_note += f" raw, {format_rate(kept_duplicate_rate)} kept"
        items.append(item(f"{prefix}duplicate fact rate", "pass", duplicate_note))
    if summary.get("extraction_yield") is not None:
        items.append(
            item(
                f"{prefix}extraction yield",
                "pass",
                f"{summary['extraction_yield']:.2f} kept facts per positive case",
            )
        )
    if summary.get("false_positive_case_rate") is not None:
        items.append(
            item(
                f"{prefix}false-positive case rate",
                "pass",
                format_rate(summary["false_positive_case_rate"]),
            )
        )
    if summary.get("false_positive_fact_rate") is not None:
        items.append(
            item(
                f"{prefix}false-positive fact rate",
                "pass",
                format_rate(summary["false_positive_fact_rate"]),
            )
        )
    recall_key = (
        "case_recall_after_filter"
        if summary.get("case_recall_after_filter") is not None
        else "case_recall"
    )
    specificity_key = (
        "case_specificity_after_filter"
        if summary.get("case_specificity_after_filter") is not None
        else "case_specificity"
    )
    if summary.get(recall_key) is not None:
        items.append(
            item(
                f"{prefix}positive-case recall",
                "pass",
                format_rate(summary[recall_key]),
            )
        )
    if summary.get(specificity_key) is not None:
        items.append(
            item(
                f"{prefix}negative-case specificity",
                "pass",
                format_rate(summary[specificity_key]),
            )
        )
    if summary.get("generic_fact_rate") is not None:
        items.append(
            item(
                f"{prefix}generic fact rate",
                "pass",
                format_rate(summary["generic_fact_rate"]),
            )
        )
    if summary.get("avg_facts_per_case") is not None:
        items.append(
            item(
                f"{prefix}average facts per case",
                "pass",
                f"{summary['avg_facts_per_case']:.2f}",
            )
        )
    items.append(
        item(
            f"{prefix}{summary['provider']} / {summary['model']}",
            "pass",
            (
                f"mean {summary['latency_ms_mean']:.0f} ms, "
                f"p95 {summary['latency_ms_p95']:.0f} ms"
            ),
        )
    )
    return items


def retrieval_injection_lane_items(label: str, summary: dict) -> list[dict]:
    prefix = f"{label} " if label else ""
    return [
        item(
            f"{prefix}positive injection hit rate",
            "pass",
            format_rate(summary.get("positive_injection_hit_rate")),
        ),
        item(
            f"{prefix}identifier-case hit rate",
            "pass",
            format_rate(summary.get("identifier_case_hit_rate")),
        ),
        item(
            f"{prefix}wrong injection rate",
            "pass",
            format_rate(summary.get("wrong_injection_rate")),
        ),
        item(
            f"{prefix}abstain precision",
            "pass",
            format_rate(summary.get("abstain_precision")),
        ),
        item(
            f"{prefix}abstain recall",
            "pass",
            format_rate(summary.get("abstain_recall")),
        ),
        item(
            f"{prefix}need more evidence recall",
            "pass",
            format_rate(summary.get("need_more_evidence_recall")),
        ),
        item(
            f"{prefix}missed evidence rate",
            "pass",
            format_rate(summary.get("missed_evidence_rate")),
        ),
        item(
            f"{prefix}summary context shrink vs flat",
            "pass",
            format_rate(summary.get("summary_context_shrink_rate")),
        ),
        item(
            f"{prefix}task-state usage rate",
            "pass",
            format_rate(summary.get("task_state_usage_rate")),
        ),
        item(
            f"{prefix}task-state hit rate",
            "pass",
            format_rate(summary.get("task_state_hit_rate")),
        ),
        item(
            f"{prefix}task-state shrink vs flat",
            "pass",
            format_rate(summary.get("task_state_context_shrink_rate")),
        ),
        item(
            f"{prefix}proxy latency p95",
            "pass",
            f"{summary.get('proxy_latency_ms_p95', 0):.0f} ms",
        ),
    ]


def format_shadow_metric_note(metric: dict) -> str:
    if metric["metric"].startswith("latency_ms"):
        stable = f"{metric['stable']:.0f} ms"
        experimental = f"{metric['experimental']:.0f} ms"
    else:
        stable = format_rate(metric["stable"])
        experimental = format_rate(metric["experimental"])
    return (
        f"stable {stable}, experimental {experimental}, "
        f"delta {format_shadow_delta(metric['metric'], metric['delta'])}"
    )


def build_sections(
    steps,
    unit_tests,
    integration_tests,
    ts_tests,
    wizard_matrix,
    benchmark_report,
    calibration_report,
    extraction_eval_report=None,
    coverage_gaps_report=None,
    long_memory_report=None,
    token_savings_report=None,
):
    step_by_slug = {step["slug"]: step for step in steps}

    build_items = []
    for slug in ("cargo_fmt", "cargo_clippy", "cargo_test", "cargo_build"):
        step = step_by_slug.get(slug)
        if step:
            build_items.append(item(step["label"], step["status"], f'{step["duration_seconds"]}s'))

    wizard_step = step_by_slug.get("wizard_smoke")
    if wizard_step:
        wizard_items = [
            item(assertion, wizard_step["status"], f'{wizard_step["duration_seconds"]}s')
            for assertion in WIZARD_ASSERTIONS
        ]
    else:
        wizard_items = []

    ts_step = step_by_slug.get("typescript_sdk")
    ts_status = ts_step["status"] if ts_step else "skip"
    ts_items = [item(name, ts_status) for name in ts_tests]

    audit_step = step_by_slug.get("cargo_audit")
    audit_items = []
    if audit_step:
        audit_note = f'{audit_step["duration_seconds"]}s'
        audit_items.append(item(audit_step["label"], audit_step["status"], audit_note))

    grouped_integration = []
    used = set()
    for title, names in INTEGRATION_GROUPS.items():
        items = []
        for name in integration_tests:
            if name in names:
                used.add(name)
                items.append(item(INTEGRATION_DISPLAY.get(name, name)))
        if items:
            grouped_integration.append(
                {
                    "title": title,
                    "count": len(items),
                    "items": items,
                }
            )
    remaining = [name for name in integration_tests if name not in used]
    if remaining:
        grouped_integration.append(
            {
                "title": "Other Integration Paths",
                "count": len(remaining),
                "items": [item(INTEGRATION_DISPLAY.get(name, name)) for name in remaining],
            }
        )

    sections = [
        {
            "title": "Build Gates",
            "count": len(build_items),
            "items": build_items,
        },
        {
            "title": "Rust Unit Tests",
            "count": len(unit_tests),
            "items": [item(UNIT_DISPLAY.get(name, name)) for name in unit_tests],
        },
    ]
    sections.extend(grouped_integration)
    sections.extend(
        [
            {
                "title": "Wizard Smoke Test",
                "count": len(wizard_items),
                "items": wizard_items,
            },
            {
                "title": "TypeScript SDK Tests",
                "count": len(ts_items),
                "items": ts_items,
            },
            {
                "title": "Dependency Audit",
                "count": len(audit_items),
                "items": audit_items,
            },
        ]
    )

    wizard_matrix_step = step_by_slug.get("wizard_matrix")
    if wizard_matrix:
        sections.append(
            {
                "title": "Wizard Scenario Matrix",
                "count": len(wizard_matrix["scenarios"]),
                "items": [
                    item(
                        scenario["name"],
                        scenario["status"],
                        (
                            f"claude={scenario['signals']['claude']}, "
                            f"codex={scenario['signals']['codex']}, "
                            f"openai_key={scenario['signals']['openai_key']}, "
                            f"anthropic_key={scenario['signals']['anthropic_key']}, "
                            f"assertions={scenario['assertion_count']}"
                        ),
                    )
                    for scenario in wizard_matrix["scenarios"]
                ],
            }
        )
    elif wizard_matrix_step:
        sections.append(
            {
                "title": "Wizard Scenario Matrix",
                "count": 1,
                "items": [
                    item(
                        wizard_matrix_step["label"],
                        wizard_matrix_step["status"],
                        f"{wizard_matrix_step['duration_seconds']}s — report artifact not available",
                    )
                ],
            }
        )

    benchmark_step = step_by_slug.get("benchmark")
    if benchmark_report:
        sections.append(
            {
                "title": "20k Scaling Benchmark",
                "count": len(benchmark_report["items"]),
                "items": benchmark_report["items"],
            }
        )
        retrieval_eval = benchmark_report.get("retrieval_injection_eval")
        if retrieval_eval:
            stable_lane = retrieval_eval.get("lanes", {}).get("stable")
            experimental_lane = retrieval_eval.get("lanes", {}).get("experimental")
            comparison = retrieval_eval.get("comparison")
            dataset_size = (
                stable_lane.get("summary", {}).get("dataset_size", 0) if stable_lane else 0
            )
            expected_inject = (
                stable_lane.get("summary", {}).get("expected_inject_cases", 0)
                if stable_lane
                else 0
            )
            expected_abstain = (
                stable_lane.get("summary", {}).get("expected_abstain_cases", 0)
                if stable_lane
                else 0
            )
            expected_need_more_evidence = (
                stable_lane.get("summary", {}).get("expected_need_more_evidence_cases", 0)
                if stable_lane
                else 0
            )
            items = [
                item(
                    "Probe dataset coverage",
                    "pass",
                    (
                        f"{dataset_size} cases "
                        f"({expected_inject} inject, {expected_abstain} abstain, "
                        f"{expected_need_more_evidence} need_more_evidence)"
                        f"{artifact_suffix(benchmark_report, benchmark_step)}"
                    ),
                )
            ]
            if stable_lane:
                items.extend(retrieval_injection_lane_items("Stable", stable_lane["summary"]))
            if experimental_lane:
                items.extend(
                    retrieval_injection_lane_items("Experimental", experimental_lane["summary"])
                )
            if comparison:
                for metric in comparison.get("metrics", []):
                    items.append(
                        item(
                            f"Shadow delta: {metric['metric']}",
                            "warn" if metric.get("regression") else "pass",
                            format_shadow_metric_note(metric),
                        )
                    )
            sections.append(
                {
                    "title": "Retrieval & Injection Evaluation",
                    "count": len(items),
                    "items": items,
                }
            )
    elif benchmark_step:
        sections.append(
            {
                "title": "20k Scaling Benchmark",
                "count": 1,
                "items": [
                    item(
                        benchmark_step["label"],
                        benchmark_step["status"],
                        f"{benchmark_step['duration_seconds']}s — report artifact not available",
                    )
                ],
            }
        )

    if long_memory_report:
        recall = long_memory_report.get("recall", {})
        write = long_memory_report.get("write", {})
        sections.append(
            {
                "title": "Long-Memory Regression",
                "count": len(long_memory_report.get("items", [])) + 2,
                "items": [
                    item(
                        "Corpus growth after sentinel insert",
                        "pass",
                        (
                            f"{write.get('total_memories', 0):,} total memories; "
                            f"batch p50 {write.get('batch_latency_ms', {}).get('p50', 0):.0f} ms"
                        ),
                    ),
                    item(
                        "Sentinel retrieval after growth",
                        "pass" if recall.get("sentinel_top_hit") else "warn",
                        (
                            f"rank {recall.get('sentinel_rank', '-')}, "
                            f"score {recall.get('top_score', 0):.4f}, "
                            f"recall {recall.get('recall_latency_ms', 0):.2f} ms"
                            f"{artifact_suffix(long_memory_report)}"
                        ),
                    ),
                    *long_memory_report.get("items", []),
                ],
            }
        )

    calibration_step = step_by_slug.get("calibration")
    if calibration_report:
        distribution = calibration_report["score_distribution"]
        current = calibration_report["current_threshold_row"]
        optimal = calibration_report["optimal_threshold_row"]
        sections.append(
            {
                "title": "Scoring Calibration",
                "count": 6,
                "items": [
                    item(
                        "Calibration corpus",
                        "pass",
                        (
                            f"{calibration_report['summary']['queries']:,} queries "
                            f"({calibration_report['summary']['exact_queries']} exact, "
                            f"{calibration_report['summary']['related_queries']} related, "
                            f"{calibration_report['summary']['noise_queries']:,} noise)"
                        ),
                    ),
                    item(
                        "Exact score distribution",
                        "pass",
                        (
                            f"min {distribution['exact']['min']:.3f}, "
                            f"p5 {distribution['exact']['p5']:.3f}, "
                            f"p50 {distribution['exact']['p50']:.3f}, "
                            f"p95 {distribution['exact']['p95']:.3f}, "
                            f"max {distribution['exact']['max']:.3f}"
                        ),
                    ),
                    item(
                        "Related score distribution",
                        "pass",
                        (
                            f"min {distribution['related']['min']:.3f}, "
                            f"p5 {distribution['related']['p5']:.3f}, "
                            f"p50 {distribution['related']['p50']:.3f}, "
                            f"p95 {distribution['related']['p95']:.3f}, "
                            f"max {distribution['related']['max']:.3f}"
                        ),
                    ),
                    item(
                        "Noise score distribution",
                        "pass",
                        (
                            f"min {distribution['noise']['min']:.3f}, "
                            f"p5 {distribution['noise']['p5']:.3f}, "
                            f"p50 {distribution['noise']['p50']:.3f}, "
                            f"p95 {distribution['noise']['p95']:.3f}, "
                            f"max {distribution['noise']['max']:.3f}"
                        ),
                    ),
                    item(
                        f"Threshold {current['threshold']:.3f}",
                        "pass",
                        (
                            f"noise rejected {current['noise_rejected_pct'] * 100:.1f}%, "
                            f"exact kept {current['exact_kept_pct'] * 100:.1f}%, "
                            f"related kept {current['related_kept_pct'] * 100:.1f}%, "
                            f"F1 {current['f1']:.3f}"
                        ),
                    ),
                    item(
                        f"Best scanned threshold {optimal['threshold']:.3f}",
                        "pass",
                        f"F1 {optimal['f1']:.3f}",
                    ),
                ],
            }
        )
    elif calibration_step:
        sections.append(
            {
                "title": "Scoring Calibration",
                "count": 1,
                "items": [
                    item(
                        calibration_step["label"],
                        calibration_step["status"],
                        f"{calibration_step['duration_seconds']}s — report artifact not available",
                    )
                ],
            }
        )

    extraction_eval_step = step_by_slug.get("extraction_eval")
    if extraction_eval_report:
        stable_lane = extraction_eval_report.get("lanes", {}).get("stable")
        experimental_lane = extraction_eval_report.get("lanes", {}).get("experimental")
        comparison = extraction_eval_report.get("comparison")
        summary = stable_lane["summary"] if stable_lane else extraction_eval_report["summary"]
        dataset_meta = extraction_eval_report.get("dataset_meta", {})
        items = [
            item(
                "Dataset coverage",
                "pass",
                (
                    f"{summary['dataset_size']} cases "
                    f"({summary['positive_cases']} positive, "
                    f"{summary['negative_cases']} negative; "
                    f"{dataset_meta.get('base_cases', 0)} base + {dataset_meta.get('template_cases', 0)} template)"
                    f"{artifact_suffix(extraction_eval_report, extraction_eval_step)}"
                ),
            )
        ]
        if stable_lane:
            items.extend(extraction_lane_items("Stable", stable_lane["summary"]))
        else:
            items.extend(extraction_lane_items("", summary))
        if experimental_lane:
            items.extend(extraction_lane_items("Experimental", experimental_lane["summary"]))
        if comparison:
            for metric in comparison.get("metrics", []):
                items.append(
                    item(
                        f"Shadow delta: {metric['metric']}",
                        "warn" if metric.get("regression") else "pass",
                        format_shadow_metric_note(metric),
                    )
                )
        sections.append(
            {
                "title": "Extraction Quality Evaluation",
                "count": len(items),
                "items": items,
            }
        )
    elif extraction_eval_step:
        sections.append(
            {
                "title": "Extraction Quality Evaluation",
                "count": 1,
                "items": [
                    item(
                        extraction_eval_step["label"],
                        extraction_eval_step["status"],
                        f"{extraction_eval_step['duration_seconds']}s — report artifact not available",
                    )
                ],
            }
        )

    if token_savings_report:
        summary = token_savings_report.get("summary", {})
        sections.append(
            {
                "title": "Token Efficiency Benchmark",
                "count": 4,
                "items": [
                    item(
                        "Benchmark scope",
                        "pass",
                        (
                            f"{token_savings_report.get('total_tasks', 0)} repeated-task prompts, "
                            f"{token_savings_report.get('runs_per_task', 0)} runs each; "
                            f"constrained context-compression benchmark"
                            f"{artifact_suffix(token_savings_report)}"
                        ),
                    ),
                    item(
                        "Average input tokens",
                        "pass",
                        (
                            f"{summary.get('avg_input_tokens_without_memory', 0)} without memory vs "
                            f"{summary.get('avg_input_tokens_with_memory', 0)} with memory"
                        ),
                    ),
                    item(
                        "Average token savings",
                        "pass",
                        f"{summary.get('avg_savings_percent', 0):.1f}%",
                    ),
                    item(
                        "Estimated monthly savings at 10k queries",
                        "pass",
                        f"${summary.get('estimated_monthly_savings_10k_queries_usd', 0):.1f}",
                    ),
                ],
            }
        )

    coverage_gaps_step = step_by_slug.get("coverage_gaps")
    if coverage_gaps_report:
        for group in coverage_gaps_report.get("groups", []):
            sections.append(
                {
                    "title": group["title"],
                    "count": group["count"],
                    "items": group["items"],
                }
            )
    elif coverage_gaps_step:
        sections.append(
            {
                "title": "Coverage Gap Tests",
                "count": 1,
                "items": [
                    item(
                        coverage_gaps_step["label"],
                        coverage_gaps_step["status"],
                        f"{coverage_gaps_step['duration_seconds']}s — report artifact not available",
                    )
                ],
            }
        )

    return sections


def build_report(
    steps,
    duration_seconds,
    wizard_matrix=None,
    benchmark_report=None,
    calibration_report=None,
    extraction_eval_report=None,
    coverage_gaps_report=None,
    long_memory_report=None,
    token_savings_report=None,
):
    cargo_step = next((step for step in steps if step["slug"] == "cargo_test"), None)
    ts_step = next((step for step in steps if step["slug"] == "typescript_sdk"), None)

    cargo_log = Path(cargo_step["log_path"]).read_text(encoding="utf-8") if cargo_step else ""
    ts_log = Path(ts_step["log_path"]).read_text(encoding="utf-8") if ts_step else ""

    unit_tests, integration_tests = parse_cargo_tests(cargo_log)
    ts_tests = parse_typescript_tests(ts_log)

    sections = build_sections(
        steps,
        unit_tests,
        integration_tests,
        ts_tests,
        wizard_matrix,
        benchmark_report,
        calibration_report,
        extraction_eval_report,
        coverage_gaps_report,
        long_memory_report,
        token_savings_report,
    )

    total_checks = sum(
        1 for section in sections for entry in section["items"] if entry["status"] == "pass"
    )
    wizard_assertions = (
        wizard_matrix["summary"]["assertions_passed"] if wizard_matrix else len(WIZARD_ASSERTIONS)
    )

    return {
        "runner": "tests/run_all.sh",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "duration_seconds": duration_seconds,
        "summary": {
            "status": "pass",
            "total_checks_passed": total_checks,
            "sections": len(sections),
            "rust_unit_tests": len(unit_tests),
            "rust_integration_tests": len(integration_tests),
            "typescript_tests": len(ts_tests),
            "wizard_assertions": wizard_assertions,
            "wizard_scenarios": wizard_matrix["summary"]["scenarios"] if wizard_matrix else 0,
            "benchmark_memories": (
                benchmark_report["write"]["memories_stored"] if benchmark_report else 0
            ),
            "calibration_queries": (
                calibration_report["summary"]["queries"] if calibration_report else 0
            ),
            "extraction_eval_cases": (
                extraction_eval_report["summary"]["dataset_size"]
                if extraction_eval_report
                else 0
            ),
            "long_memory_total_memories": (
                long_memory_report["write"]["total_memories"] if long_memory_report else 0
            ),
            "token_savings_percent": (
                token_savings_report["summary"]["avg_savings_percent"]
                if token_savings_report
                else 0
            ),
        },
        "steps": [
            {
                **step,
                "log_path": display_path(step["log_path"]),
            }
            for step in steps
        ],
        "sections": sections,
        "coverage_gaps": COVERAGE_GAPS,
    }


def write_markdown(report, path: Path):
    lines = [
        "# Test Report",
        "",
        f"- Runner: `{report['runner']}`",
        f"- Generated at: `{report['generated_at']}`",
        f"- Duration: `{report['duration_seconds']}s`",
        f"- Total passed checks: `{report['summary']['total_checks_passed']}`",
        "",
    ]
    for section in report["sections"]:
        lines.append(f"## {section['title']} ({section['count']})")
        lines.append("")
        for entry in section["items"]:
            note = f" — {entry['note']}" if entry.get("note") else ""
            lines.append(f"- [{entry['status'].upper()}] {entry['name']}{note}")
        lines.append("")
    lines.append("## Coverage Gaps")
    lines.append("")
    for gap in report["coverage_gaps"]:
        lines.append(f"- {gap}")
    path.write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--steps", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--website-json", required=True)
    parser.add_argument("--wizard-json")
    parser.add_argument("--benchmark-json")
    parser.add_argument("--calibration-json")
    parser.add_argument("--extraction-eval-json")
    parser.add_argument("--coverage-gaps-json")
    parser.add_argument("--long-memory-json")
    parser.add_argument("--token-savings-json")
    parser.add_argument("--duration", required=True, type=int)
    args = parser.parse_args()

    steps = parse_steps(Path(args.steps))
    wizard_matrix = load_optional_json(args.wizard_json)
    benchmark_report = load_optional_json(args.benchmark_json)
    calibration_report = load_optional_json(args.calibration_json)
    extraction_eval_report = load_optional_json(args.extraction_eval_json)
    coverage_gaps_report = load_optional_json(args.coverage_gaps_json)
    long_memory_report = load_optional_json(args.long_memory_json)
    token_savings_report = load_optional_json(args.token_savings_json)
    report = build_report(
        steps,
        args.duration,
        wizard_matrix=wizard_matrix,
        benchmark_report=benchmark_report,
        calibration_report=calibration_report,
        extraction_eval_report=extraction_eval_report,
        coverage_gaps_report=coverage_gaps_report,
        long_memory_report=long_memory_report,
        token_savings_report=token_savings_report,
    )

    output_json = Path(args.output_json)
    output_json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    write_markdown(report, Path(args.output_md))

    website_json = Path(args.website_json)
    website_json.parent.mkdir(parents=True, exist_ok=True)
    website_json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
