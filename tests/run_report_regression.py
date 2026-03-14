#!/usr/bin/env python3
import importlib.util
from pathlib import Path

ROOT_DIR = Path(__file__).resolve().parent.parent


def load_generate_report_module():
    module_path = ROOT_DIR / "tests" / "generate_report.py"
    spec = importlib.util.spec_from_file_location("generate_report", module_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"failed to load module from {module_path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    generate_report = load_generate_report_module()
    stale_universal_loop_artifact = {
        "generated_at": "2026-03-14T00:00:00Z",
        "summary": {
            "portability_success_rate": 1.0,
            "replay_fidelity": 1.0,
            "task_state_quality": 1.0,
            "repeated_context_elimination_rate": 1.0,
            "review_throughput_per_minute": 7.5,
            "blocked_bad_actions_rate": 1.0,
        },
    }
    steps = [
        {
            "slug": "universal_memory_loop",
            "label": "everyday utility loop",
            "status": "fail",
            "duration_seconds": 12,
            "log_path": str(ROOT_DIR / "tests" / ".last-run" / "universal_memory_loop.log"),
        }
    ]

    report = generate_report.build_report(
        steps,
        12,
        universal_loop_report=stale_universal_loop_artifact,
    )

    assert report["summary"]["status"] == "fail"
    assert report["summary"]["universal_loop_portability_rate"] == 0
    assert report["summary"]["universal_loop_replay_fidelity"] == 0
    assert report["summary"]["universal_loop_task_state_quality"] == 0
    assert report["utility_loop"] is None
    section = next(
        section
        for section in report["sections"]
        if section["title"] == "Universal Memory Loop Proof"
    )
    assert section["items"][0]["status"] == "fail"
    assert "current run step fail" in section["items"][0]["note"]

    print("report artifact regression passed")


if __name__ == "__main__":
    main()
