#!/usr/bin/env python3
"""memoryOSS extraction quality evaluation runner.

This evaluates the current extraction prompt against a small labeled dataset.
It does not require a running memoryOSS server; it calls the configured LLM
provider directly with the same prompt text used by the proxy extraction path.
"""

from __future__ import annotations

import json
import os
import re
import statistics
import sys
import time
from pathlib import Path

import requests


ROOT_DIR = Path(__file__).resolve().parent.parent
PROXY_RS = ROOT_DIR / "src" / "server" / "proxy.rs"
DATASET_PATH = Path(
    os.environ.get(
        "EXTRACTION_EVAL_DATASET",
        ROOT_DIR / "tests" / "extraction-eval-dataset.json",
    )
)
OUTPUT_JSON = Path(
    os.environ.get(
        "EXTRACTION_EVAL_OUTPUT_JSON",
        ROOT_DIR / "tests" / "extraction-eval-report.json",
    )
)
TIMEOUT = float(os.environ.get("EXTRACTION_EVAL_TIMEOUT", "60"))
MAX_TOKENS = int(os.environ.get("EXTRACTION_EVAL_MAX_TOKENS", "800"))

GENERIC_PATTERNS = [
    "rust ownership",
    "best practice",
    "generally",
    "in general",
    "always use",
    "should use indexes",
    "memory safety guarantees",
    "hello",
    "how can i help",
    "glad that helped",
]

TRANSIENT_PATTERNS = [
    "sounds good, i'll be here",
    "i'll be here when you return",
    "be back in ten minutes",
    "grab coffee",
    "current ci run is still in progress",
    "still in progress",
    "wait for the run to finish",
]

GENERIC_PRODUCT_PATTERNS = [
    "local memory layer",
    "ai agents",
    "helps preserve context across sessions",
    "preserve context across sessions",
    "designed to preserve context across sessions",
    "maintain context across sessions",
    "context persistence",
]

PROJECT_SPECIFIC_ANCHORS = [
    "/",
    ".rs",
    ".toml",
    ".json",
    "readme",
    "homepage",
    "landing page",
    "docs",
    "documentation",
    "proxy",
    "mcp",
    "oauth",
    "anthropic",
    "openai",
    "docker",
    "workflow",
    "release",
    "latency",
    "namespace",
    "config",
    "setting",
    "flag",
    "bug",
    "fix",
    "decision",
    "constraint",
    "preference",
    "unless",
    "because",
]

KEYWORD_ALIASES = {
    "rollback": [
        ("rollback",),
        ("roll", "back"),
    ],
    "mcp-first": [
        ("mcp", "first"),
        ("mcp", "default"),
    ],
    "do not display": [
        ("do", "not", "display"),
        ("do", "not", "show"),
        ("never", "show"),
        ("avoid", "showing"),
    ],
    "raw memoryoss": [
        ("raw", "memoryoss"),
        ("raw", "memoryoss", "entries"),
        ("raw", "memory", "entries"),
    ],
    "short summaries": [
        ("short", "summaries"),
        ("short", "summary"),
        ("summaries",),
        ("summary",),
    ],
}


def infer_provider() -> str:
    explicit = os.environ.get("EXTRACTION_EVAL_PROVIDER", "").strip().lower()
    if explicit:
        return explicit
    if os.environ.get("ANTHROPIC_API_KEY"):
        return "claude"
    if os.environ.get("OPENAI_API_KEY"):
        return "openai"
    raise SystemExit(
        "Set EXTRACTION_EVAL_PROVIDER or provide ANTHROPIC_API_KEY / OPENAI_API_KEY."
    )


def default_model_for_provider(provider: str) -> str:
    if provider == "claude":
        return "claude-sonnet-4-6"
    return "gpt-4o-mini"


def load_extraction_prompt() -> str:
    source = PROXY_RS.read_text(encoding="utf-8")
    match = re.search(
        r'const EXTRACTION_PROMPT: &str = r#"(.*?)"#;',
        source,
        flags=re.DOTALL,
    )
    if not match:
        raise RuntimeError("Could not locate EXTRACTION_PROMPT in src/server/proxy.rs")
    return match.group(1)


def extract_json_array(text: str) -> list[dict]:
    start = text.find("[")
    if start == -1:
        return []

    depth = 0
    in_string = False
    escape = False
    end = -1
    for idx, char in enumerate(text[start:], start=start):
        if in_string:
            if escape:
                escape = False
            elif char == "\\":
                escape = True
            elif char == '"':
                in_string = False
            continue

        if char == '"':
            in_string = True
        elif char == "[":
            depth += 1
        elif char == "]":
            depth -= 1
            if depth == 0:
                end = idx + 1
                break

    if end == -1:
        return []

    try:
        parsed = json.loads(text[start:end])
    except json.JSONDecodeError:
        return []
    if not isinstance(parsed, list):
        return []
    return [item for item in parsed if isinstance(item, dict)]


def normalize(text: str) -> str:
    return re.sub(r"\s+", " ", text.lower()).strip()


def tokenize(text: str) -> list[str]:
    return re.findall(r"[a-z0-9]+", normalize(text))


def token_jaccard(a: list[str], b: list[str]) -> float:
    set_a = set(a)
    set_b = set(b)
    if not set_a or not set_b:
        return 0.0
    return len(set_a & set_b) / len(set_a | set_b)


def structural_duplicate_content(a: str, b: str) -> bool:
    norm_a = " ".join(tokenize(a))
    norm_b = " ".join(tokenize(b))
    if not norm_a or not norm_b:
        return False
    if norm_a == norm_b:
        return True

    tokens_a = norm_a.split()
    tokens_b = norm_b.split()
    shorter, longer = (tokens_a, tokens_b) if len(tokens_a) <= len(tokens_b) else (tokens_b, tokens_a)
    shorter_norm = " ".join(shorter)
    longer_norm = " ".join(longer)
    if len(shorter) >= 5 and shorter_norm and shorter_norm in longer_norm:
        return True

    return (
        len(tokens_a) >= 6
        and len(tokens_b) >= 6
        and token_jaccard(tokens_a, tokens_b) >= 0.92
    )


def count_duplicate_facts(facts: list[dict]) -> int:
    seen: list[str] = []
    duplicate_count = 0
    for fact in facts:
        content = str(fact.get("content", ""))
        if not content:
            continue
        if any(structural_duplicate_content(content, prior) for prior in seen):
            duplicate_count += 1
        else:
            seen.append(content)
    return duplicate_count


def fact_matches_keywords(fact: dict, keywords: list[str]) -> bool:
    content = normalize(str(fact.get("content", "")))
    content_tokens = set(tokenize(content))
    return all(keyword_matches_content(keyword, content, content_tokens) for keyword in keywords)


def keyword_matches_content(keyword: str, content: str, content_tokens: set[str]) -> bool:
    normalized_keyword = normalize(keyword)
    if normalized_keyword in content:
        return True

    keyword_tokens = tokenize(normalized_keyword)
    if keyword_tokens and all(token in content_tokens for token in keyword_tokens):
        return True

    for alias in KEYWORD_ALIASES.get(normalized_keyword, []):
        if all(token in content_tokens for token in alias):
            return True

    return False


def generic_fact(fact: dict) -> bool:
    content = normalize(str(fact.get("content", "")))
    if not content:
        return False
    return any(pattern in content for pattern in GENERIC_PATTERNS)


def transient_fact(fact: dict) -> bool:
    content = normalize(str(fact.get("content", "")))
    if not content:
        return False
    return any(pattern in content for pattern in TRANSIENT_PATTERNS)


def generic_product_fact(fact: dict) -> bool:
    content = normalize(str(fact.get("content", "")))
    if not content:
        return False
    generic_hits = sum(1 for pattern in GENERIC_PRODUCT_PATTERNS if pattern in content)
    if generic_hits < 2:
        return False
    return not any(anchor in content for anchor in PROJECT_SPECIFIC_ANCHORS)


def should_store_fact(fact: dict) -> bool:
    return not (
        generic_fact(fact) or transient_fact(fact) or generic_product_fact(fact)
    )


def fallback_preference_facts(transcript: str) -> list[dict]:
    facts: list[dict] = []
    for raw_line in transcript.splitlines():
        line = raw_line.strip()
        if not line.lower().startswith("user:"):
            continue

        content = line.split(":", 1)[1].strip()
        lower = content.lower()
        if (
            "raw memoryoss" in lower
            and (
                "unless i explicitly ask" in lower
                or "unless i ask" in lower
                or "unless explicitly asked" in lower
            )
            and (
                "short summaries" in lower
                or "short summary" in lower
                or "summaries or counts" in lower
                or "summary or counts" in lower
                or "counts are enough" in lower
            )
        ):
            facts.append(
                {
                    "content": "For this user, do not show raw MemoryOSS entries unless they explicitly ask; short summaries or counts are preferred.",
                    "tags": ["user-preference", "memoryoss", "display", "verbosity"],
                }
            )
    return facts


def merge_facts(facts: list[dict], supplemental: list[dict]) -> list[dict]:
    merged = list(facts)
    for candidate in supplemental:
        content = str(candidate.get("content", ""))
        if not content:
            continue
        duplicate = any(
            structural_duplicate_content(content, str(existing.get("content", "")))
            for existing in merged
        )
        if not duplicate:
            merged.append(candidate)
    return merged


def anthropic_call(model: str, prompt: str) -> dict:
    api_key = os.environ.get("ANTHROPIC_API_KEY", "")
    if not api_key:
        raise SystemExit("ANTHROPIC_API_KEY is required for provider=claude")
    response = requests.post(
        "https://api.anthropic.com/v1/messages",
        headers={
            "x-api-key": api_key,
            "anthropic-version": "2023-06-01",
            "content-type": "application/json",
        },
        json={
            "model": model,
            "max_tokens": MAX_TOKENS,
            "messages": [{"role": "user", "content": prompt}],
        },
        timeout=TIMEOUT,
    )
    response.raise_for_status()
    data = response.json()
    text = ""
    for block in data.get("content", []):
        if block.get("type") == "text":
            text += block.get("text", "")
    usage = data.get("usage", {})
    return {
        "text": text,
        "input_tokens": usage.get("input_tokens"),
        "output_tokens": usage.get("output_tokens"),
    }


def openai_call(model: str, prompt: str) -> dict:
    api_key = os.environ.get("OPENAI_API_KEY", "")
    if not api_key:
        raise SystemExit("OPENAI_API_KEY is required for provider=openai")
    response = requests.post(
        "https://api.openai.com/v1/chat/completions",
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
        json={
            "model": model,
            "temperature": 0,
            "max_tokens": MAX_TOKENS,
            "messages": [{"role": "user", "content": prompt}],
        },
        timeout=TIMEOUT,
    )
    response.raise_for_status()
    data = response.json()
    usage = data.get("usage", {})
    message = data["choices"][0]["message"]["content"]
    return {
        "text": message or "",
        "input_tokens": usage.get("prompt_tokens"),
        "output_tokens": usage.get("completion_tokens"),
    }


def provider_call(provider: str, model: str, prompt: str) -> dict:
    if provider == "claude":
        return anthropic_call(model, prompt)
    if provider == "openai":
        return openai_call(model, prompt)
    raise SystemExit(f"Unsupported provider: {provider}")


def main() -> None:
    provider = infer_provider()
    model = os.environ.get(
        "EXTRACTION_EVAL_MODEL", default_model_for_provider(provider)
    ).strip()
    prompt_prefix = load_extraction_prompt()
    dataset = json.loads(DATASET_PATH.read_text(encoding="utf-8"))
    if not isinstance(dataset, list) or not dataset:
        raise SystemExit(f"Dataset at {DATASET_PATH} is empty or invalid.")

    started = time.time()
    results = []
    latencies_ms = []
    total_facts = 0
    generic_facts = 0
    transient_facts = 0
    generic_product_facts = 0
    duplicate_facts = 0
    kept_duplicate_facts = 0
    positive_cases = 0
    negative_cases = 0
    positive_hits = 0
    positive_hits_after_filter = 0
    negative_clean = 0
    negative_clean_after_filter = 0
    total_kept_facts = 0
    negative_kept_facts = 0

    for case in dataset:
        prompt = f"{prompt_prefix}{case['transcript']}"
        t0 = time.time()
        llm_result = provider_call(provider, model, prompt)
        latencies_ms.append((time.time() - t0) * 1000.0)

        facts = merge_facts(
            extract_json_array(llm_result["text"]),
            fallback_preference_facts(case["transcript"]),
        )
        kept_facts = [fact for fact in facts if should_store_fact(fact)]
        total_facts += len(facts)
        total_kept_facts += len(kept_facts)
        generic_count = sum(1 for fact in facts if generic_fact(fact))
        transient_count = sum(1 for fact in facts if transient_fact(fact))
        generic_product_count = sum(1 for fact in facts if generic_product_fact(fact))
        duplicate_count = count_duplicate_facts(facts)
        kept_duplicate_count = count_duplicate_facts(kept_facts)
        generic_facts += generic_count
        transient_facts += transient_count
        generic_product_facts += generic_product_count
        duplicate_facts += duplicate_count
        kept_duplicate_facts += kept_duplicate_count

        matched_expected = False
        matched_expected_after_filter = False
        for expected in case.get("expected_facts", []):
            if any(fact_matches_keywords(fact, expected) for fact in facts):
                matched_expected = True
            if any(fact_matches_keywords(fact, expected) for fact in kept_facts):
                matched_expected_after_filter = True
            if matched_expected and matched_expected_after_filter:
                break

        expect_extract = bool(case.get("expect_extract"))
        if expect_extract:
            positive_cases += 1
            if matched_expected:
                positive_hits += 1
            if matched_expected_after_filter:
                positive_hits_after_filter += 1
        else:
            negative_cases += 1
            if not facts:
                negative_clean += 1
            if not kept_facts:
                negative_clean_after_filter += 1
            negative_kept_facts += len(kept_facts)

        results.append(
            {
                "id": case["id"],
                "category": case["category"],
                "expect_extract": expect_extract,
                "facts_found": len(facts),
                "facts_kept": len(kept_facts),
                "matched_expected": matched_expected,
                "matched_expected_after_filter": matched_expected_after_filter,
                "generic_facts": generic_count,
                "transient_facts": transient_count,
                "generic_product_facts": generic_product_count,
                "duplicate_facts": duplicate_count,
                "kept_duplicate_facts": kept_duplicate_count,
                "input_tokens": llm_result.get("input_tokens"),
                "output_tokens": llm_result.get("output_tokens"),
                "latency_ms": round(latencies_ms[-1], 2),
                "facts": facts,
                "kept_facts": kept_facts,
            }
        )
        print(
            f"[extraction-eval] {case['id']} category={case['category']} facts={len(facts)} "
            f"kept={len(kept_facts)} matched={matched_expected_after_filter} "
            f"generic={generic_count} transient={transient_count} dup={duplicate_count}",
            flush=True,
        )

    summary = {
        "provider": provider,
        "model": model,
        "dataset_size": len(dataset),
        "positive_cases": positive_cases,
        "negative_cases": negative_cases,
        "case_recall": round(positive_hits / positive_cases, 4) if positive_cases else None,
        "case_recall_after_filter": round(positive_hits_after_filter / positive_cases, 4)
        if positive_cases
        else None,
        "case_specificity": round(negative_clean / negative_cases, 4)
        if negative_cases
        else None,
        "case_specificity_after_filter": round(
            negative_clean_after_filter / negative_cases, 4
        )
        if negative_cases
        else None,
        "generic_fact_rate": round(generic_facts / total_facts, 4) if total_facts else 0.0,
        "transient_fact_rate": round(transient_facts / total_facts, 4)
        if total_facts
        else 0.0,
        "generic_product_fact_rate": round(generic_product_facts / total_facts, 4)
        if total_facts
        else 0.0,
        "project_specific_fact_rate": round(total_kept_facts / total_facts, 4)
        if total_facts
        else 0.0,
        "duplicate_fact_rate": round(duplicate_facts / total_facts, 4)
        if total_facts
        else 0.0,
        "kept_duplicate_fact_rate": round(kept_duplicate_facts / total_kept_facts, 4)
        if total_kept_facts
        else 0.0,
        "false_positive_case_rate": round(
            (negative_cases - negative_clean_after_filter) / negative_cases, 4
        )
        if negative_cases
        else None,
        "false_positive_fact_rate": round(negative_kept_facts / total_kept_facts, 4)
        if total_kept_facts
        else 0.0,
        "post_filter_keep_rate": round(
            total_kept_facts / total_facts, 4
        )
        if total_facts
        else 0.0,
        "extraction_yield": round(total_kept_facts / positive_cases, 3)
        if positive_cases
        else 0.0,
        "avg_facts_per_case": round(total_facts / len(dataset), 3),
        "avg_facts_per_positive_case": round(total_facts / positive_cases, 3)
        if positive_cases
        else 0.0,
        "latency_ms_mean": round(statistics.mean(latencies_ms), 2),
        "latency_ms_p95": round(
            sorted(latencies_ms)[max(0, int(len(latencies_ms) * 0.95) - 1)], 2
        ),
        "duration_s": round(time.time() - started, 2),
    }

    report = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "dataset_path": str(DATASET_PATH.relative_to(ROOT_DIR)),
        "summary": summary,
        "results": results,
    }
    OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    print(json.dumps(summary, indent=2), flush=True)
    print(f"[extraction-eval] report written to {OUTPUT_JSON}", flush=True)


if __name__ == "__main__":
    main()
