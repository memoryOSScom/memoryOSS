// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors
import { describe, it } from "node:test";
import * as assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { MemoryOSSClient, MemoryOSSError, connect, normalizeConformanceArtifact } from "./index";

describe("MemoryOSSClient", () => {
  it("constructs with defaults", () => {
    const client = new MemoryOSSClient();
    assert.ok(client);
  });

  it("constructs with options", () => {
    const client = new MemoryOSSClient({
      url: "http://localhost:9923",
      apiKey: "test-key",
    });
    assert.ok(client);
  });

  it("throws MemoryOSSError on auth without key", async () => {
    const client = new MemoryOSSClient();
    await assert.rejects(() => client.authenticate(), MemoryOSSError);
  });

  it("connect returns a client without apiKey", async () => {
    const client = await connect({ url: "http://localhost:9923" });
    assert.ok(client instanceof MemoryOSSClient);
  });
});

describe("MemoryOSSError", () => {
  it("formats message with status", () => {
    const err = new MemoryOSSError(404, "not found");
    assert.equal(err.status, 404);
    assert.equal(err.message, "[404] not found");
    assert.equal(err.name, "MemoryOSSError");
  });
});

describe("conformance helpers", () => {
  it("normalize canonical fixtures without changing them", () => {
    const root = join(__dirname, "..", "..", "..");
    const cases: Array<["runtime_contract" | "passport" | "history", string]> = [
      ["runtime_contract", join(root, "conformance", "fixtures", "runtime-contract.json")],
      ["passport", join(root, "conformance", "fixtures", "passport-bundle.json")],
      ["history", join(root, "conformance", "fixtures", "history-bundle.json")],
    ];

    for (const [kind, path] of cases) {
      const fixture = JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>;
      assert.deepEqual(normalizeConformanceArtifact(kind, fixture), fixture);
    }
  });
});
