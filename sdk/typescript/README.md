# memoryOSS TypeScript SDK

TypeScript client for the [memoryOSS](https://github.com/memoryOSScom/memoryoss) Agent Memory Database.

## Install

```bash
npm install memoryoss
```

## Quickstart

```typescript
import { connect } from "memoryoss";

const db = await connect({
  url: "http://localhost:8000",
  apiKey: "your-api-key",
});

// Store a memory
const { id } = await db.store("User prefers dark mode", {
  tags: ["preference", "ui"],
  agent: "ui-assistant",
});

// Recall memories
const { memories } = await db.recall("UI preferences", { limit: 5 });
for (const m of memories) {
  console.log(`[${m.score.toFixed(2)}] ${m.memory.content}`);
}

// Update
await db.update(id, { content: "User switched to light mode" });

// Forget
const { deleted } = await db.forget({ agent: "ui-assistant" });
```

## API

### `connect(options?)`
Creates and authenticates a client. Returns `Promise<MemoryOSSClient>`.

### `client.store(content, options?)`
Store a memory. Returns `{ id, version }`.

### `client.storeBatch(memories)`
Batch store. Returns `{ stored: [{ id, version }] }`.

### `client.recall(query, options?)`
Semantic + keyword search. Returns `{ memories: ScoredMemory[], cursor? }`.

### `client.recallBatch(queries)`
Parallel recall. Returns `{ results: RecallResult[] }`.

### `client.update(id, options)`
Update a memory. Returns `{ id, version, updated_at }`.

### `client.forget(options)`
Delete memories by filter. Returns `{ deleted: number }`.

### `client.consolidate(options?)`
Merge similar memories. Returns groups and merge count.

### `client.health()`
Server health check.

## License

AGPL-3.0-only
