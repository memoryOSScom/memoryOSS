// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";

export type ConformanceArtifactKind = "runtime_contract" | "passport" | "history";

function compactJson(value: unknown): string {
  return JSON.stringify(value);
}

function sha256Hex(value: unknown): string {
  return createHash("sha256").update(compactJson(value)).digest("hex");
}

function assertHasKeys(value: Record<string, unknown>, keys: string[], kind: string): void {
  for (const key of keys) {
    if (!(key in value)) {
      throw new Error(`${kind} fixture missing ${key}`);
    }
  }
}

function verifyRuntimeContract(value: Record<string, unknown>): void {
  assertHasKeys(
    value,
    [
      "contract_id",
      "version",
      "runtime_name",
      "stable_semantics",
      "experimental_layers",
      "object_model",
      "guarantees",
      "api_mappings",
      "known_gaps",
    ],
    "runtime contract",
  );
}

function verifyPassportBundle(value: Record<string, unknown>): void {
  const payload = {
    bundle_version: value["bundle_version"],
    passport_id: value["passport_id"],
    runtime_contract: value["runtime_contract"],
    scope: value["scope"],
    namespace: value["namespace"],
    exported_at: value["exported_at"],
    provenance: value["provenance"],
    memories: value["memories"],
  };
  const integrity = value["integrity"] as Record<string, unknown>;
  if (String(integrity["algorithm"]).toLowerCase() !== "sha256") {
    throw new Error("passport bundle uses unsupported integrity algorithm");
  }
  if (sha256Hex(payload) !== integrity["payload_sha256"]) {
    throw new Error("passport bundle integrity mismatch");
  }
}

function verifyHistoryBundle(value: Record<string, unknown>): void {
  const payload = {
    bundle_version: value["bundle_version"],
    history_id: value["history_id"],
    root_id: value["root_id"],
    runtime_contract: value["runtime_contract"],
    namespace: value["namespace"],
    exported_at: value["exported_at"],
    memories: value["memories"],
    edges: value["edges"],
    timeline: value["timeline"],
  };
  const integrity = value["integrity"] as Record<string, unknown>;
  if (String(integrity["algorithm"]).toLowerCase() !== "sha256") {
    throw new Error("history bundle uses unsupported integrity algorithm");
  }
  if (sha256Hex(payload) !== integrity["payload_sha256"]) {
    throw new Error("history bundle integrity mismatch");
  }
}

export function normalizeConformanceArtifact(
  kind: ConformanceArtifactKind,
  value: Record<string, unknown>,
): Record<string, unknown> {
  if (kind === "runtime_contract") {
    verifyRuntimeContract(value);
  } else if (kind === "passport") {
    verifyPassportBundle(value);
  } else if (kind === "history") {
    verifyHistoryBundle(value);
  }
  return value;
}

function main(): number {
  const args = process.argv.slice(2);
  let kind: ConformanceArtifactKind | null = null;
  let input: string | null = null;
  let output: string | null = null;

  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--kind") {
      kind = args[index + 1] as ConformanceArtifactKind;
      index += 1;
    } else if (arg === "--input") {
      input = args[index + 1];
      index += 1;
    } else if (arg === "--output") {
      output = args[index + 1];
      index += 1;
    }
  }

  if (!kind || !input || !output) {
    throw new Error("usage: conformance --kind <runtime_contract|passport|history> --input <file> --output <file>");
  }

  const value = JSON.parse(readFileSync(input, "utf8")) as Record<string, unknown>;
  const normalized = normalizeConformanceArtifact(kind, value);
  writeFileSync(output, `${JSON.stringify(normalized, null, 2)}\n`, "utf8");
  return 0;
}

if (require.main === module) {
  process.exitCode = main();
}
