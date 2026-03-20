import { spawn } from "node:child_process";
import { access, readFile } from "node:fs/promises";
import * as http from "node:http";
import * as https from "node:https";
import { constants as fsConstants } from "node:fs";
import * as path from "node:path";
import process from "node:process";

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import type { ServerContext } from "@smithery/sdk";
import { z } from "zod";

const SERVER_NAME = "memoryoss";
const SERVER_VERSION = "0.2.0";
const HEALTH_TIMEOUT_MS = 2_500;
const START_TIMEOUT_MS = 20_000;
const HEALTH_POLL_INTERVAL_MS = 500;

const configSchema = z.object({
  configPath: z
    .string()
    .min(1)
    .describe(
      "Absolute path to memoryoss.toml. Generate it with `memoryoss setup` or copy memoryoss.toml.example."
    ),
  binaryPath: z
    .string()
    .min(1)
    .optional()
    .describe(
      "Optional absolute path to the memoryoss binary. If omitted, the wrapper looks for `memoryoss` on PATH."
    ),
  baseUrl: z
    .string()
    .url()
    .optional()
    .describe(
      "Optional override for the memoryOSS HTTP base URL. Defaults to the host/port/tls values from memoryoss.toml."
    ),
  apiKey: z
    .string()
    .min(1)
    .optional()
    .describe(
      "Optional override for the API key used against memoryOSS. Defaults to the first [auth.api_keys] entry in memoryoss.toml."
    ),
  autoStartServer: z
    .boolean()
    .default(true)
    .describe(
      "Start `memoryoss -c <configPath> serve` automatically when the configured HTTP server is not reachable."
    )
});

type RuntimeConfig = z.infer<typeof configSchema>;

type ToolName =
  | "memoryoss_recall"
  | "memoryoss_store"
  | "memoryoss_update"
  | "memoryoss_forget";

type MinimalTomlConfig = {
  server: {
    host: string;
    port: number;
    hybridMode: boolean;
    corePort?: number;
  };
  tls: {
    enabled: boolean;
  };
  auth: {
    apiKeys: Array<{
      key?: string;
    }>;
  };
};

type RuntimeState = {
  apiKey: string;
  baseUrl: string;
  child: ReturnType<typeof spawn> | null;
};

const memoryTypeSchema = z
  .enum(["episodic", "semantic", "procedural", "working"])
  .optional();

const TOOL_DEFINITIONS = [
  {
    name: "memoryoss_recall" as const,
    title: "Recall Relevant Memories",
    description:
      "Search your long-term memory. Call this FIRST at the start of every conversation turn to retrieve relevant context from previous sessions. Pass the user's question or topic as the query. Returns ranked results with content and relevance scores. Always check memory before answering - you may already know things the user told you before.",
    annotations: {
      readOnlyHint: true,
      destructiveHint: false
    },
    inputSchema: {
      query: z
        .string()
        .min(1)
        .describe(
          "Natural language query to search memories"
        ),
      limit: z
        .number()
        .int()
        .positive()
        .max(100)
        .optional()
        .describe("Maximum number of results (default: 10)"),
      agent: z.string().optional().describe("Optional agent filter"),
      session: z.string().optional().describe("Optional session filter"),
      namespace: z
        .string()
        .optional()
        .describe('Optional namespace (default: "default")'),
      tags: z.array(z.string()).default([]).describe("Filter by tags")
    }
  },
  {
    name: "memoryoss_store" as const,
    title: "Store New Memory",
    description:
      "Save important information to long-term memory so you can recall it in future sessions. Store facts, decisions, user preferences, project context, and key findings. Call this whenever you learn something worth remembering - if in doubt, store it. Memories persist across sessions and are searchable by content.",
    annotations: {
      readOnlyHint: false,
      destructiveHint: true
    },
    inputSchema: {
      content: z
        .string()
        .min(1)
        .describe("The text content to store as a memory"),
      tags: z.array(z.string()).default([]).describe("Optional tags for categorization"),
      agent: z.string().optional().describe("Optional agent identifier"),
      session: z.string().optional().describe("Optional session identifier"),
      namespace: z
        .string()
        .optional()
        .describe('Optional namespace (default: "default")'),
      memory_type: memoryTypeSchema.describe(
        "Memory type: episodic, semantic, procedural, or working"
      )
    }
  },
  {
    name: "memoryoss_update" as const,
    title: "Update Existing Memory",
    description:
      "Update an existing memory by ID. Use this when information has changed - e.g. a user preference was corrected or a fact is outdated. Pass the memory ID from a previous recall result.",
    annotations: {
      readOnlyHint: false,
      destructiveHint: true
    },
    inputSchema: {
      id: z.string().min(1).describe("UUID of the memory to update"),
      content: z.string().optional().describe("New content (optional)"),
      tags: z.array(z.string()).optional().describe("New tags (optional)"),
      namespace: z
        .string()
        .optional()
        .describe('Optional namespace (default: "default")')
    }
  },
  {
    name: "memoryoss_forget" as const,
    title: "Delete Stored Memory",
    description:
      "Delete memories by their IDs. Use when the user asks you to forget something or when stored information is wrong and should not be updated but removed entirely.",
    annotations: {
      readOnlyHint: false,
      destructiveHint: true
    },
    inputSchema: {
      ids: z.array(z.string()).default([]).describe("UUIDs of memories to delete"),
      namespace: z
        .string()
        .optional()
        .describe('Optional namespace (default: "default")')
    }
  }
];

function createSuccessResult(text: string): CallToolResult {
  return {
    content: [
      {
        type: "text",
        text
      }
    ]
  };
}

function createErrorResult(message: string): CallToolResult {
  return {
    content: [
      {
        type: "text",
        text: message
      }
    ],
    isError: true
  };
}

function buildServer(
  executor: (tool: ToolName, args: Record<string, unknown>) => Promise<CallToolResult>
): McpServer {
  const server = new McpServer({
    name: SERVER_NAME,
    version: SERVER_VERSION
  });

  for (const definition of TOOL_DEFINITIONS) {
    server.registerTool(
      definition.name,
      {
        title: definition.title,
        description: definition.description,
        inputSchema: definition.inputSchema,
        annotations: definition.annotations
      },
      async (args) => executor(definition.name, args as Record<string, unknown>)
    );
  }

  return server;
}

function stripInlineComment(line: string): string {
  let result = "";
  let inSingle = false;
  let inDouble = false;
  let escaped = false;

  for (const char of line) {
    if (escaped) {
      result += char;
      escaped = false;
      continue;
    }

    if (char === "\\" && inDouble) {
      result += char;
      escaped = true;
      continue;
    }

    if (char === "'" && !inDouble) {
      inSingle = !inSingle;
      result += char;
      continue;
    }

    if (char === '"' && !inSingle) {
      inDouble = !inDouble;
      result += char;
      continue;
    }

    if (char === "#" && !inSingle && !inDouble) {
      break;
    }

    result += char;
  }

  return result;
}

function parseTomlValue(value: string): string | number | boolean {
  const trimmed = value.trim();

  if (trimmed.startsWith('"') && trimmed.endsWith('"')) {
    return JSON.parse(trimmed);
  }

  if (trimmed.startsWith("'") && trimmed.endsWith("'")) {
    return trimmed.slice(1, -1);
  }

  if (trimmed === "true") {
    return true;
  }

  if (trimmed === "false") {
    return false;
  }

  if (/^-?\d+$/.test(trimmed)) {
    return Number.parseInt(trimmed, 10);
  }

  return trimmed;
}

function parseMemoryOssToml(text: string): MinimalTomlConfig {
  const config: MinimalTomlConfig = {
    server: {
      host: "127.0.0.1",
      port: 8000,
      hybridMode: false
    },
    tls: {
      enabled: true
    },
    auth: {
      apiKeys: []
    }
  };

  let section = "";
  let currentApiKey: { key?: string } | null = null;

  for (const rawLine of text.split(/\r?\n/u)) {
    const line = stripInlineComment(rawLine).trim();
    if (!line) {
      continue;
    }

    const arraySectionMatch = line.match(/^\[\[(.+)\]\]$/u);
    if (arraySectionMatch) {
      section = arraySectionMatch[1];
      if (section === "auth.api_keys") {
        currentApiKey = {};
        config.auth.apiKeys.push(currentApiKey);
      } else {
        currentApiKey = null;
      }
      continue;
    }

    const sectionMatch = line.match(/^\[(.+)\]$/u);
    if (sectionMatch) {
      section = sectionMatch[1];
      currentApiKey = null;
      continue;
    }

    const kvMatch = line.match(/^([A-Za-z0-9_]+)\s*=\s*(.+)$/u);
    if (!kvMatch) {
      continue;
    }

    const [, key, rawValue] = kvMatch;
    const value = parseTomlValue(rawValue);

    if (section === "server") {
      if (key === "host" && typeof value === "string") {
        config.server.host = value;
      } else if (key === "port" && typeof value === "number") {
        config.server.port = value;
      } else if (key === "hybrid_mode" && typeof value === "boolean") {
        config.server.hybridMode = value;
      } else if (key === "core_port" && typeof value === "number") {
        config.server.corePort = value;
      }
      continue;
    }

    if (section === "tls") {
      if (key === "enabled" && typeof value === "boolean") {
        config.tls.enabled = value;
      }
      continue;
    }

    if (section === "auth.api_keys" && currentApiKey) {
      if (key === "key" && typeof value === "string") {
        currentApiKey.key = value;
      }
    }
  }

  return config;
}

function resolveBaseUrl(
  config: RuntimeConfig,
  parsed: MinimalTomlConfig
): { candidates: string[]; allowSelfSignedTls: boolean } {
  if (config.baseUrl) {
    return {
      candidates: [config.baseUrl],
      allowSelfSignedTls: config.baseUrl.startsWith("https://")
    };
  }

  const expectedScheme = parsed.tls.enabled ? "https" : "http";
  const alternateScheme = parsed.tls.enabled ? "http" : "https";
  const bindTarget = `${parsed.server.host}:${parsed.server.port}`;

  return {
    candidates: [
      `${expectedScheme}://${bindTarget}`,
      `${alternateScheme}://${bindTarget}`
    ],
    allowSelfSignedTls: parsed.tls.enabled
  };
}

async function fileExists(target: string): Promise<boolean> {
  try {
    await access(target, fsConstants.F_OK);
    return true;
  } catch {
    return false;
  }
}

async function executableExists(target: string): Promise<boolean> {
  try {
    await access(target, fsConstants.X_OK);
    return true;
  } catch {
    return false;
  }
}

async function resolveBinaryPath(config: RuntimeConfig): Promise<string> {
  if (config.binaryPath) {
    if (!(await executableExists(config.binaryPath))) {
      throw new Error(`Configured binaryPath does not exist or is not executable: ${config.binaryPath}`);
    }
    return config.binaryPath;
  }

  const pathEntries = (process.env.PATH ?? "")
    .split(path.delimiter)
    .filter(Boolean);
  const executableNames =
    process.platform === "win32" ? ["memoryoss.exe", "memoryoss.cmd"] : ["memoryoss"];

  for (const entry of pathEntries) {
    for (const executableName of executableNames) {
      const candidate = path.join(entry, executableName);
      if (await executableExists(candidate)) {
        return candidate;
      }
    }
  }

  throw new Error(
    "memoryoss binary not found. Install memoryOSS first or pass binaryPath in the Smithery config."
  );
}

function requestJson(
  method: "GET" | "POST" | "PATCH" | "DELETE",
  baseUrl: string,
  requestPath: string,
  apiKey: string,
  body?: Record<string, unknown>,
  allowSelfSignedTls = false
): Promise<unknown> {
  const url = new URL(requestPath, baseUrl);
  const transport = url.protocol === "https:" ? https : http;
  const payload = body === undefined ? undefined : JSON.stringify(body);

  return new Promise((resolve, reject) => {
    const req = transport.request(
      url,
      {
        method,
        headers: {
          accept: "application/json",
          authorization: `Bearer ${apiKey}`,
          ...(payload
            ? {
                "content-type": "application/json",
                "content-length": Buffer.byteLength(payload)
              }
            : {})
        },
        ...(url.protocol === "https:"
          ? {
              rejectUnauthorized: !allowSelfSignedTls
            }
          : {})
      },
      (response) => {
        let responseBody = "";
        response.setEncoding("utf8");
        response.on("data", (chunk) => {
          responseBody += chunk;
        });
        response.on("end", () => {
          const status = response.statusCode ?? 500;
          const trimmed = responseBody.trim();
          const parsed =
            trimmed.length === 0
              ? {}
              : (() => {
                  try {
                    return JSON.parse(trimmed);
                  } catch {
                    return trimmed;
                  }
                })();

          if (status < 200 || status >= 300) {
            const detail =
              typeof parsed === "string" ? parsed : JSON.stringify(parsed);
            reject(new Error(`HTTP ${status} from ${requestPath}: ${detail}`));
            return;
          }

          resolve(parsed);
        });
      }
    );

    req.setTimeout(HEALTH_TIMEOUT_MS, () => {
      req.destroy(new Error(`Request to ${url.toString()} timed out`));
    });

    req.on("error", reject);

    if (payload) {
      req.write(payload);
    }

    req.end();
  });
}

async function firstHealthyBaseUrl(
  candidates: string[],
  apiKey: string,
  allowSelfSignedTls: boolean
): Promise<string | null> {
  for (const candidate of candidates) {
    try {
      await requestJson("GET", candidate, "/health", apiKey, undefined, allowSelfSignedTls);
      return candidate;
    } catch {
      continue;
    }
  }

  return null;
}

function streamChildLogs(stream: NodeJS.ReadableStream | null, prefix: string): void {
  if (!stream) {
    return;
  }

  stream.on("data", (chunk) => {
    const text = String(chunk).trimEnd();
    if (!text) {
      return;
    }
    for (const line of text.split(/\r?\n/u)) {
      console.error(`[memoryoss ${prefix}] ${line}`);
    }
  });
}

async function terminateChild(child: ReturnType<typeof spawn> | null): Promise<void> {
  if (!child || child.killed || child.exitCode !== null) {
    return;
  }

  await new Promise<void>((resolve) => {
    const timeout = setTimeout(() => {
      if (child.exitCode === null) {
        child.kill("SIGKILL");
      }
      resolve();
    }, 2_000);

    child.once("exit", () => {
      clearTimeout(timeout);
      resolve();
    });

    child.kill("SIGTERM");
  });
}

class MemoryOssRuntime {
  private readonly config: RuntimeConfig;
  private initialization: Promise<RuntimeState> | null = null;

  constructor(config: RuntimeConfig) {
    this.config = config;
  }

  async close(): Promise<void> {
    if (!this.initialization) {
      return;
    }

    try {
      const state = await this.initialization;
      await terminateChild(state.child);
    } catch {
      // Ignore failed initialization on close.
    }
  }

  async execute(tool: ToolName, args: Record<string, unknown>): Promise<CallToolResult> {
    try {
      const runtime = await this.ensureReady();
      switch (tool) {
        case "memoryoss_store":
          return await this.handleStore(runtime, args);
        case "memoryoss_recall":
          return await this.handleRecall(runtime, args);
        case "memoryoss_update":
          return await this.handleUpdate(runtime, args);
        case "memoryoss_forget":
          return await this.handleForget(runtime, args);
      }
    } catch (error) {
      return createErrorResult(error instanceof Error ? error.message : String(error));
    }
  }

  private async ensureReady(): Promise<RuntimeState> {
    if (!this.initialization) {
      this.initialization = this.initialize();
    }
    return this.initialization;
  }

  private async initialize(): Promise<RuntimeState> {
    const configPath = path.resolve(this.config.configPath);
    if (!(await fileExists(configPath))) {
      throw new Error(`memoryOSS config not found: ${configPath}`);
    }

    const parsed = parseMemoryOssToml(await readFile(configPath, "utf8"));
    const apiKey = this.config.apiKey ?? parsed.auth.apiKeys[0]?.key;
    if (!apiKey) {
      throw new Error(
        "No API key available for memoryOSS. Add an [auth.api_keys] entry or pass apiKey in the Smithery config."
      );
    }

    const { candidates, allowSelfSignedTls } = resolveBaseUrl(this.config, parsed);
    const healthyUrl = await firstHealthyBaseUrl(candidates, apiKey, allowSelfSignedTls);
    if (healthyUrl) {
      return {
        apiKey,
        baseUrl: healthyUrl,
        child: null
      };
    }

    if (!this.config.autoStartServer) {
      throw new Error(
        `memoryOSS is not reachable at ${candidates.join(" or ")} and autoStartServer is disabled.`
      );
    }

    const binaryPath = await resolveBinaryPath(this.config);
    const child = spawn(binaryPath, ["-c", configPath, "serve"], {
      stdio: ["ignore", "pipe", "pipe"]
    });

    streamChildLogs(child.stdout, "stdout");
    streamChildLogs(child.stderr, "stderr");

    const startupDeadline = Date.now() + START_TIMEOUT_MS;
    while (Date.now() < startupDeadline) {
      if (child.exitCode !== null) {
        throw new Error(`memoryOSS exited during startup with code ${child.exitCode}`);
      }

      const startedUrl = await firstHealthyBaseUrl(candidates, apiKey, allowSelfSignedTls);
      if (startedUrl) {
        return {
          apiKey,
          baseUrl: startedUrl,
          child
        };
      }

      await new Promise((resolve) => setTimeout(resolve, HEALTH_POLL_INTERVAL_MS));
    }

    await terminateChild(child);
    throw new Error(
      `memoryOSS did not become healthy within ${START_TIMEOUT_MS / 1000}s at ${candidates.join(" or ")}.`
    );
  }

  private async handleStore(
    runtime: RuntimeState,
    args: Record<string, unknown>
  ): Promise<CallToolResult> {
    const payload: Record<string, unknown> = {
      content: args.content,
      tags: args.tags ?? []
    };
    for (const key of ["agent", "session", "namespace", "memory_type"]) {
      if (args[key] !== undefined) {
        payload[key] = args[key];
      }
    }

    const response = (await requestJson(
      "POST",
      runtime.baseUrl,
      "/v1/store",
      runtime.apiKey,
      payload,
      runtime.baseUrl.startsWith("https://")
    )) as Record<string, unknown>;

    return createSuccessResult(
      JSON.stringify({
        id: response.id ?? "",
        version: response.version ?? 0,
        stored: true
      })
    );
  }

  private async handleRecall(
    runtime: RuntimeState,
    args: Record<string, unknown>
  ): Promise<CallToolResult> {
    const payload: Record<string, unknown> = {
      query: args.query,
      limit: args.limit ?? 10
    };

    for (const key of ["agent", "session", "namespace"]) {
      if (args[key] !== undefined) {
        payload[key] = args[key];
      }
    }

    if (Array.isArray(args.tags) && args.tags.length > 0) {
      payload.tags = args.tags;
    }

    const response = (await requestJson(
      "POST",
      runtime.baseUrl,
      "/v1/recall",
      runtime.apiKey,
      payload,
      runtime.baseUrl.startsWith("https://")
    )) as {
      memories?: Array<{
        score?: number;
        memory?: {
          id?: string;
          content?: string;
          tags?: unknown;
          agent?: unknown;
          created_at?: string;
          memory_type?: string;
        };
      }>;
    };

    const simplified = (response.memories ?? [])
      .map((entry) => {
        if (!entry.memory) {
          return null;
        }
        return {
          id: entry.memory.id ?? "",
          content: entry.memory.content ?? "",
          tags: entry.memory.tags ?? [],
          agent: entry.memory.agent ?? null,
          score:
            typeof entry.score === "number" ? entry.score.toFixed(3) : "",
          created_at: entry.memory.created_at ?? "",
          memory_type: entry.memory.memory_type ?? "episodic"
        };
      })
      .filter((entry): entry is NonNullable<typeof entry> => entry !== null);

    return createSuccessResult(JSON.stringify(simplified, null, 2));
  }

  private async handleUpdate(
    runtime: RuntimeState,
    args: Record<string, unknown>
  ): Promise<CallToolResult> {
    const payload: Record<string, unknown> = {
      id: args.id
    };
    for (const key of ["content", "tags", "namespace"]) {
      if (args[key] !== undefined) {
        payload[key] = args[key];
      }
    }

    const response = (await requestJson(
      "PATCH",
      runtime.baseUrl,
      "/v1/update",
      runtime.apiKey,
      payload,
      runtime.baseUrl.startsWith("https://")
    )) as Record<string, unknown>;

    return createSuccessResult(
      JSON.stringify({
        id: response.id ?? args.id ?? "",
        version: response.version ?? 0,
        updated: true
      })
    );
  }

  private async handleForget(
    runtime: RuntimeState,
    args: Record<string, unknown>
  ): Promise<CallToolResult> {
    const payload: Record<string, unknown> = {
      ids: Array.isArray(args.ids) ? args.ids : []
    };

    if (args.namespace !== undefined) {
      payload.namespace = args.namespace;
    }

    const response = (await requestJson(
      "DELETE",
      runtime.baseUrl,
      "/v1/forget",
      runtime.apiKey,
      payload,
      runtime.baseUrl.startsWith("https://")
    )) as Record<string, unknown>;

    return createSuccessResult(
      JSON.stringify({
        deleted: response.deleted ?? 0
      })
    );
  }
}

export { configSchema };

export async function createSandboxServer(): Promise<McpServer> {
  return buildServer(async (tool) =>
    createSuccessResult(
      JSON.stringify({
        tool,
        ready: true,
        note:
          "Smithery sandbox scan succeeded. Install memoryOSS locally and provide configPath to execute real calls."
      })
    )
  );
}

export default async function createServer(
  context: ServerContext<RuntimeConfig>
): Promise<McpServer> {
  const runtime = new MemoryOssRuntime(context.config);
  const server = buildServer((tool, args) => runtime.execute(tool, args));
  const originalClose = server.close.bind(server);

  server.close = async () => {
    await runtime.close();
    await originalClose();
  };

  return server;
}
