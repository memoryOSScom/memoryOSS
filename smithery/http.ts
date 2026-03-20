import process from "node:process";
import * as http from "node:http";
import * as https from "node:https";

import { createMcpExpressApp } from "@modelcontextprotocol/sdk/server/express.js";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod/v4";

const SERVER_VERSION = "0.2.0";
const UPSTREAM_BASE_URL =
  process.env.MEMORYOSS_UPSTREAM_URL ?? "http://127.0.0.1:8000";
const LISTEN_HOST = process.env.MEMORYOSS_MCP_HTTP_HOST ?? "127.0.0.1";
const LISTEN_PORT = Number.parseInt(
  process.env.MEMORYOSS_MCP_HTTP_PORT ?? "8012",
  10
);
const ALLOWED_HOSTS = (
  process.env.MEMORYOSS_MCP_ALLOWED_HOSTS ??
  "memoryoss.com,www.memoryoss.com,127.0.0.1,localhost"
)
  .split(",")
  .map((value) => value.trim())
  .filter(Boolean);
const REQUEST_TIMEOUT_MS = 30_000;

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
      query: z.string().min(1).describe("Natural language query to search memories"),
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
      content: z.string().min(1).describe("The text content to store as a memory"),
      tags: z.array(z.string()).default([]).describe("Optional tags for categorization"),
      agent: z.string().optional().describe("Optional agent identifier"),
      session: z.string().optional().describe("Optional session identifier"),
      namespace: z
        .string()
        .optional()
        .describe('Optional namespace (default: "default")'),
      memory_type: z
        .enum(["episodic", "semantic", "procedural", "working"])
        .optional()
        .describe("Memory type: episodic, semantic, procedural, or working")
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

type ToolName = (typeof TOOL_DEFINITIONS)[number]["name"];

function extractApiKey(headers: http.IncomingHttpHeaders): string | null {
  const direct = headers["memoryoss-api-key"];
  if (typeof direct === "string" && direct.trim()) {
    return direct.trim();
  }

  const auth = headers.authorization;
  if (typeof auth === "string") {
    const match = auth.match(/^Bearer\s+(.+)$/i);
    if (match?.[1]) {
      return match[1].trim();
    }
  }

  return null;
}

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

function requestJson(
  method: "GET" | "POST" | "PATCH" | "DELETE",
  requestPath: string,
  apiKey: string,
  body?: Record<string, unknown>
): Promise<unknown> {
  const url = new URL(requestPath, UPSTREAM_BASE_URL);
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
        }
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

    req.setTimeout(REQUEST_TIMEOUT_MS, () => {
      req.destroy(new Error(`Request to ${url.toString()} timed out`));
    });
    req.on("error", reject);

    if (payload) {
      req.write(payload);
    }

    req.end();
  });
}

async function executeTool(
  tool: ToolName,
  apiKey: string | null,
  args: Record<string, unknown>
): Promise<CallToolResult> {
  if (!apiKey) {
    return createErrorResult(
      "Missing memoryOSS API key. Provide the `memoryoss-api-key` header when connecting through Smithery."
    );
  }

  try {
    switch (tool) {
      case "memoryoss_store": {
        const payload: Record<string, unknown> = {
          content: args.content,
          tags: args.tags ?? []
        };
        for (const key of ["agent", "session", "namespace", "memory_type"]) {
          if (args[key] !== undefined) {
            payload[key] = args[key];
          }
        }
        const response = (await requestJson("POST", "/v1/store", apiKey, payload)) as Record<
          string,
          unknown
        >;
        return createSuccessResult(
          JSON.stringify({
            id: response.id ?? "",
            version: response.version ?? 0,
            stored: true
          })
        );
      }
      case "memoryoss_recall": {
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
        const response = (await requestJson("POST", "/v1/recall", apiKey, payload)) as {
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
      case "memoryoss_update": {
        const memoryId = typeof args.id === "string" ? args.id : "";
        if (!memoryId) {
          return createErrorResult("Missing memory id.");
        }

        const payload: Record<string, unknown> = {
          id: memoryId
        };
        for (const key of ["content", "tags", "namespace"]) {
          if (args[key] !== undefined) {
            payload[key] = args[key];
          }
        }
        const response = (await requestJson("PATCH", "/v1/update", apiKey, payload)) as Record<
          string,
          unknown
        >;
        return createSuccessResult(
          JSON.stringify({
            id: response.id ?? memoryId,
            version: response.version ?? 0,
            updated: true
          })
        );
      }
      case "memoryoss_forget": {
        const payload: Record<string, unknown> = {
          ids: Array.isArray(args.ids) ? args.ids : []
        };
        if (args.namespace !== undefined) {
          payload.namespace = args.namespace;
        }
        const response = (await requestJson("DELETE", "/v1/forget", apiKey, payload)) as Record<
          string,
          unknown
        >;
        return createSuccessResult(
          JSON.stringify({
            deleted: response.deleted ?? 0
          })
        );
      }
    }
  } catch (error) {
    return createErrorResult(error instanceof Error ? error.message : String(error));
  }
}

function createBridgeServer(apiKey: string | null): McpServer {
  const server = new McpServer(
    {
      name: "memoryoss",
      version: SERVER_VERSION,
      title: "memoryOSS",
      description:
        "Persistent memory for AI agents with local MCP tools and hybrid recall.",
      websiteUrl: "https://memoryoss.com"
    },
    {
      capabilities: {
        logging: {}
      }
    }
  );

  for (const definition of TOOL_DEFINITIONS) {
    server.registerTool(
      definition.name,
      {
        title: definition.title,
        description: definition.description,
        inputSchema: definition.inputSchema,
        annotations: definition.annotations
      },
      async (args) => executeTool(definition.name, apiKey, args as Record<string, unknown>)
    );
  }

  return server;
}

const app = createMcpExpressApp({
  host: LISTEN_HOST,
  allowedHosts: ALLOWED_HOSTS
});
app.disable("x-powered-by");

app.get("/health", async (_req, res) => {
  try {
    const upstream = await requestJson("GET", "/health", "bridge-health-check");
    res.json({
      status: "ok",
      upstream: upstream
    });
  } catch (error) {
    res.status(503).json({
      status: "error",
      message: error instanceof Error ? error.message : String(error)
    });
  }
});

app.post("/mcp", async (req, res) => {
  const apiKey = extractApiKey(req.headers);
  const server = createBridgeServer(apiKey);

  try {
    const transport = new StreamableHTTPServerTransport({
      sessionIdGenerator: undefined
    });
    await server.connect(transport);
    await transport.handleRequest(req, res, req.body);
    res.on("close", () => {
      void transport.close();
      void server.close();
    });
  } catch (error) {
    console.error("Error handling MCP request:", error);
    if (!res.headersSent) {
      res.status(500).json({
        jsonrpc: "2.0",
        error: {
          code: -32603,
          message: "Internal server error"
        },
        id: null
      });
    }
  }
});

for (const method of ["get", "delete"] as const) {
  app[method]("/mcp", async (_req, res) => {
    res.writeHead(405).end(
      JSON.stringify({
        jsonrpc: "2.0",
        error: {
          code: -32000,
          message: "Method not allowed."
        },
        id: null
      })
    );
  });
}

app.listen(LISTEN_PORT, LISTEN_HOST, (error?: Error) => {
  if (error) {
    console.error("Failed to start streamable HTTP MCP bridge:", error);
    process.exit(1);
  }

  console.error(
    `memoryOSS streamable HTTP MCP bridge listening on http://${LISTEN_HOST}:${LISTEN_PORT}/mcp -> ${UPSTREAM_BASE_URL}`
  );
});
