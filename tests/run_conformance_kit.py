#!/usr/bin/env python3
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
OUTPUT_JSON = Path(
    os.environ.get("CONFORMANCE_OUTPUT_JSON", ROOT / "tests/.last-run/conformance-report.json")
)
FIXTURES = [
    ("runtime_contract", ROOT / "conformance/fixtures/runtime-contract.json"),
    ("passport", ROOT / "conformance/fixtures/passport-bundle.json"),
    ("history", ROOT / "conformance/fixtures/history-bundle.json"),
]
SCHEMAS = [
    ROOT / "conformance/schemas/runtime-contract.schema.json",
    ROOT / "conformance/schemas/passport-bundle.schema.json",
    ROOT / "conformance/schemas/history-bundle.schema.json",
]


def run(command):
    return subprocess.run(command, capture_output=True, text=True, check=False)


def main() -> int:
    OUTPUT_JSON.parent.mkdir(parents=True, exist_ok=True)
    missing = [str(path) for path in SCHEMAS + [fixture for _, fixture in FIXTURES] if not path.exists()]
    if missing:
        raise SystemExit(f"missing conformance kit files: {', '.join(missing)}")

    node = shutil.which("node")
    ts_script = ROOT / "sdk/typescript/dist/conformance.js"
    if not node or not ts_script.exists():
        print("SKIP: TypeScript conformance path is not built yet")
        OUTPUT_JSON.write_text(
            json.dumps(
                {
                    "status": "skip",
                    "cases": [],
                    "reason": "typescript reference path unavailable",
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        return 0

    cases = []
    with tempfile.TemporaryDirectory() as tmp:
        tmpdir = Path(tmp)
        for kind, fixture in FIXTURES:
            rust_out = tmpdir / f"{kind}.rust.json"
            py_out = tmpdir / f"{kind}.python.json"
            ts_out = tmpdir / f"{kind}.ts.json"

            rust = run(
                [
                    str(ROOT / "target/debug/memoryoss"),
                    "conformance",
                    "normalize",
                    "--kind",
                    kind,
                    "--input",
                    str(fixture),
                    "--output",
                    str(rust_out),
                ]
            )
            python = run(
                [
                    "python3",
                    str(ROOT / "tests/reference_conformance.py"),
                    "--kind",
                    kind,
                    "--input",
                    str(fixture),
                    "--output",
                    str(py_out),
                ]
            )
            typescript = run(
                [
                    node,
                    str(ts_script),
                    "--kind",
                    kind,
                    "--input",
                    str(fixture),
                    "--output",
                    str(ts_out),
                ]
            )

            if rust.returncode != 0:
                raise SystemExit(f"rust conformance failed for {kind}: {rust.stderr or rust.stdout}")
            if python.returncode != 0:
                raise SystemExit(
                    f"python conformance failed for {kind}: {python.stderr or python.stdout}"
                )
            if typescript.returncode != 0:
                raise SystemExit(
                    f"typescript conformance failed for {kind}: {typescript.stderr or typescript.stdout}"
                )

            fixture_value = json.loads(fixture.read_text(encoding="utf-8"))
            rust_value = json.loads(rust_out.read_text(encoding="utf-8"))
            python_value = json.loads(py_out.read_text(encoding="utf-8"))
            ts_value = json.loads(ts_out.read_text(encoding="utf-8"))
            if rust_value != fixture_value or python_value != fixture_value or ts_value != fixture_value:
                raise SystemExit(f"normalized artifact mismatch for {kind}")

            cases.append(
                {
                    "kind": kind,
                    "fixture": str(fixture.relative_to(ROOT)),
                    "rust": "pass",
                    "python": "pass",
                    "typescript": "pass",
                }
            )

    report = {
        "status": "pass",
        "schema_count": len(SCHEMAS),
        "fixture_count": len(FIXTURES),
        "cases": cases,
        "artifact_versions": {
            "runtime_contract": "memoryoss.runtime.v1alpha1",
            "passport_bundle": "memoryoss.passport.v1alpha1",
            "history_bundle": "memoryoss.history.v1alpha1",
        },
    }
    OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Conformance kit passed for {len(cases)} artifact kinds")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
