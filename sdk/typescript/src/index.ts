// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors
// memoryOSS TypeScript SDK — Memory for AI Agents

export interface Memory {
  id: string;
  content: string;
  tags: string[];
  agent?: string;
  session?: string;
  namespace?: string;
  memory_type: string;
  version: number;
  created_at?: string;
  updated_at?: string;
}

export interface ScoredMemory {
  memory: Memory;
  score: number;
  provenance: string[];
}

export interface StoreOptions {
  tags?: string[];
  agent?: string;
  session?: string;
  namespace?: string;
  memory_type?: string;
  zero_knowledge?: boolean;
  embedding?: number[];
}

export interface RecallOptions {
  limit?: number;
  agent?: string;
  session?: string;
  namespace?: string;
  memory_type?: string;
  tags?: string[];
  query_embedding?: number[];
}

export interface UpdateOptions {
  content?: string;
  tags?: string[];
  memory_type?: string;
}

export interface ForgetOptions {
  ids?: string[];
  agent?: string;
  session?: string;
  namespace?: string;
  tags?: string[];
  before?: string;
}

export interface ConsolidateOptions {
  threshold?: number;
  dry_run?: boolean;
  namespace?: string;
}

export interface ConsolidationGroup {
  representative_id: string;
  merged_ids: string[];
  similarity: number;
}

export interface ConsolidateResult {
  groups: ConsolidationGroup[];
  merged_count: number;
  dry_run: boolean;
}

export interface StoreResult {
  id: string;
  version: number;
}

export interface UpdateResult {
  id: string;
  version: number;
  updated_at: string;
}

export interface RecallResult {
  memories: ScoredMemory[];
  cursor?: string;
}

export class MemoryOSSError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(`[${status}] ${message}`);
    this.name = "MemoryOSSError";
    this.status = status;
  }
}

export interface ClientOptions {
  url?: string;
  apiKey?: string;
  verifySsl?: boolean;
}

export class MemoryOSSClient {
  private url: string;
  private apiKey?: string;
  private token?: string;

  constructor(options: ClientOptions = {}) {
    this.url = (options.url ?? "https://localhost:8000").replace(/\/+$/, "");
    this.apiKey = options.apiKey;
  }

  private headers(): Record<string, string> {
    const h: Record<string, string> = { "Content-Type": "application/json" };
    if (this.token) {
      h["Authorization"] = `Bearer ${this.token}`;
    }
    return h;
  }

  private async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const resp = await fetch(`${this.url}${path}`, {
      method,
      headers: this.headers(),
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });

    if (!resp.ok) {
      let msg: string;
      try {
        const data = await resp.json() as Record<string, unknown>;
        msg = (data.error as string) ?? resp.statusText;
      } catch {
        msg = resp.statusText;
      }
      throw new MemoryOSSError(resp.status, msg);
    }

    return resp.json() as Promise<T>;
  }

  async authenticate(): Promise<string> {
    if (!this.apiKey) {
      throw new MemoryOSSError(0, "no API key set");
    }
    const data = await this.request<{ token: string }>("POST", "/v1/auth/token", {
      api_key: this.apiKey,
    });
    this.token = data.token;
    return this.token;
  }

  async store(content: string, options: StoreOptions = {}): Promise<StoreResult> {
    return this.request<StoreResult>("POST", "/v1/store", { content, ...options });
  }

  async storeBatch(
    memories: Array<{ content: string } & StoreOptions>
  ): Promise<{ stored: StoreResult[] }> {
    return this.request("POST", "/v1/store/batch", { memories });
  }

  async recall(query: string, options: RecallOptions = {}): Promise<RecallResult> {
    return this.request<RecallResult>("POST", "/v1/recall", { query, ...options });
  }

  async recallBatch(
    queries: Array<{ query: string } & RecallOptions>
  ): Promise<{ results: RecallResult[] }> {
    return this.request("POST", "/v1/recall/batch", { queries });
  }

  async update(id: string, options: UpdateOptions): Promise<UpdateResult> {
    return this.request<UpdateResult>("PATCH", "/v1/update", { id, ...options });
  }

  async forget(options: ForgetOptions): Promise<{ deleted: number }> {
    return this.request("DELETE", "/v1/forget", options);
  }

  async consolidate(options: ConsolidateOptions = {}): Promise<ConsolidateResult> {
    return this.request<ConsolidateResult>("POST", "/v1/consolidate", options);
  }

  async health(): Promise<Record<string, unknown>> {
    return this.request("GET", "/health");
  }
}

export function connect(options: ClientOptions = {}): Promise<MemoryOSSClient> {
  const client = new MemoryOSSClient(options);
  if (options.apiKey) {
    return client.authenticate().then(() => client);
  }
  return Promise.resolve(client);
}
