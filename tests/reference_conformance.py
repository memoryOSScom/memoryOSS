#!/usr/bin/env python3
import argparse
import hashlib
import json
from pathlib import Path


def compact_json_bytes(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def write_pretty(path: Path, value) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def verify_runtime_contract(value):
    required = [
        "contract_id",
        "version",
        "runtime_name",
        "stable_semantics",
        "experimental_layers",
        "object_model",
        "guarantees",
        "api_mappings",
        "known_gaps",
    ]
    for key in required:
        if key not in value:
            raise ValueError(f"runtime contract fixture missing {key}")


def verify_passport_bundle(value):
    payload = {
        "bundle_version": value["bundle_version"],
        "passport_id": value["passport_id"],
        "runtime_contract": value["runtime_contract"],
        "scope": value["scope"],
        "namespace": value["namespace"],
        "exported_at": value["exported_at"],
        "provenance": value["provenance"],
        "memories": value["memories"],
    }
    digest = hashlib.sha256(compact_json_bytes(payload)).hexdigest()
    if value["integrity"]["algorithm"].lower() != "sha256":
        raise ValueError("passport bundle uses unsupported integrity algorithm")
    if digest != value["integrity"]["payload_sha256"]:
        raise ValueError("passport bundle integrity mismatch")


def verify_history_bundle(value):
    payload = {
        "bundle_version": value["bundle_version"],
        "history_id": value["history_id"],
        "root_id": value["root_id"],
        "runtime_contract": value["runtime_contract"],
        "namespace": value["namespace"],
        "exported_at": value["exported_at"],
        "memories": value["memories"],
        "edges": value["edges"],
        "timeline": value["timeline"],
    }
    digest = hashlib.sha256(compact_json_bytes(payload)).hexdigest()
    if value["integrity"]["algorithm"].lower() != "sha256":
        raise ValueError("history bundle uses unsupported integrity algorithm")
    if digest != value["integrity"]["payload_sha256"]:
        raise ValueError("history bundle integrity mismatch")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--kind", required=True, choices=["runtime_contract", "passport", "history"])
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()

    value = json.loads(Path(args.input).read_text(encoding="utf-8"))
    if args.kind == "runtime_contract":
        verify_runtime_contract(value)
    elif args.kind == "passport":
        verify_passport_bundle(value)
    elif args.kind == "history":
        verify_history_bundle(value)

    write_pretty(Path(args.output), value)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
