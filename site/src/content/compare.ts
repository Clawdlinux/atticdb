import { SITE_URL } from "@/lib/site";

export type Product = { id: string; name: string; url: string };
export type Cell = { value: string; source?: string };

export const PRODUCTS: Product[] = [
  { id: "atticdb", name: "AtticDB", url: SITE_URL },
  { id: "mem0", name: "Mem0", url: "https://mem0.ai" },
  { id: "zep", name: "Zep and Graphiti", url: "https://www.getzep.com" },
  { id: "letta", name: "Letta", url: "https://www.letta.com" },
  { id: "supermemory", name: "Supermemory", url: "https://supermemory.ai" },
  { id: "redis", name: "Redis Agent Memory", url: "https://redis.io/agent-memory/" },
  { id: "cognee", name: "Cognee", url: "https://www.cognee.ai" },
  { id: "engram", name: "Engram", url: "https://github.com/Gentleman-Programming/engram" },
  { id: "claude-md", name: "CLAUDE.md files", url: "https://code.claude.com/docs/en/memory" },
];

const sources = {
  mem0: "https://github.com/mem0ai/mem0",
  mem0Pricing: "https://docs.mem0.ai/platform/pricing",
  mem0Mcp: "https://docs.mem0.ai/platform/mem0-mcp",
  zep: "https://github.com/getzep/graphiti",
  letta: "https://github.com/letta-ai/letta-code",
  supermemory: "https://github.com/supermemoryai/supermemory",
  redis: "https://github.com/redis/agent-memory-server",
  redisMcp: "https://github.com/redis/agent-memory-server/blob/main/V0/README.md",
  cognee: "https://github.com/topoteretes/cognee",
  engram: "https://github.com/Gentleman-Programming/engram",
  claude: "https://code.claude.com/docs/en/memory",
};

export const COMPARE_ROWS: { label: string; cells: Record<string, Cell> }[] = [
  {
    label: "what it stores",
    cells: {
      atticdb: { value: "Turn history per session and keyed facts" },
      mem0: { value: "Extracted memories and entity links", source: sources.mem0 },
      zep: { value: "Entities, temporal facts, episodes", source: sources.zep },
      letta: { value: "Memory blocks and Git-backed context", source: sources.letta },
      supermemory: { value: "Extracted facts and profiles", source: sources.supermemory },
      redis: { value: "Session events and extracted facts", source: sources.redis },
      cognee: { value: "Documents, entities, and graphs", source: sources.cognee },
      engram: { value: "Project-aware text memories", source: sources.engram },
      "claude-md": { value: "Instructions and markdown notes", source: sources.claude },
    },
  },
  {
    label: "storage it needs",
    cells: {
      atticdb: { value: "Local disk, single Rust binary" },
      mem0: { value: "Vector database; optional graph store", source: sources.mem0 },
      zep: { value: "Graphiti: graph DB; Zep Cloud: Konig", source: sources.zep },
      letta: { value: "MemFS: Git-backed context repository", source: sources.letta },
      supermemory: { value: "Vector-graph database; cloud or local", source: sources.supermemory },
      redis: { value: "Redis; V0 also needs Docket workers", source: sources.redis },
      cognee: { value: "Relational, vector, and graph stores", source: sources.cognee },
      engram: { value: "Local SQLite + FTS5, single Go binary", source: sources.engram },
      "claude-md": { value: "Local markdown files", source: sources.claude },
    },
  },
  {
    label: "model call on write",
    cells: {
      atticdb: { value: "No" },
      mem0: { value: "Yes, extraction and embeddings", source: sources.mem0Pricing },
      zep: { value: "Yes, extraction and embeddings", source: sources.zep },
      letta: { value: "LLM tool calls rewrite memory blocks", source: sources.letta },
      supermemory: { value: "Yes, extraction and embeddings", source: sources.supermemory },
      redis: { value: "Background extraction and embeddings", source: sources.redis },
      cognee: { value: "Local GLiNER or cloud LLM, plus embeddings", source: sources.cognee },
      engram: { value: "No LLM or embedding call", source: sources.engram },
      "claude-md": { value: "Manual files; LLM-written auto memory", source: sources.claude },
    },
  },
  {
    label: "acked after fsync (published)",
    cells: {
      atticdb: { value: "Yes, after fdatasync" },
      mem0: { value: "Not published" },
      zep: { value: "Not published" },
      letta: { value: "Not published" },
      supermemory: { value: "Not published" },
      redis: { value: "Not published" },
      cognee: { value: "Not published" },
      engram: { value: "Not published" },
      "claude-md": { value: "Not published" },
    },
  },
  {
    label: "runs fully offline",
    cells: {
      atticdb: { value: "Yes" },
      mem0: { value: "Not published" },
      zep: { value: "Not published" },
      letta: { value: "Not published" },
      supermemory: { value: "Not published" },
      redis: { value: "Not published" },
      cognee: { value: "Local extraction supported", source: sources.cognee },
      engram: { value: "Local binary, no model calls on write", source: sources.engram },
      "claude-md": { value: "Not published" },
    },
  },
  {
    label: "licence",
    cells: {
      atticdb: { value: "Apache-2.0 or MIT" },
      mem0: { value: "Apache-2.0", source: sources.mem0 },
      zep: { value: "Graphiti: Apache-2.0; Zep Cloud: proprietary", source: sources.zep },
      letta: { value: "Letta Code: Apache-2.0", source: sources.letta },
      supermemory: { value: "MIT", source: sources.supermemory },
      redis: { value: "V0: Apache-2.0", source: sources.redis },
      cognee: { value: "Apache-2.0", source: sources.cognee },
      engram: { value: "MIT", source: sources.engram },
      "claude-md": { value: "Claude Code: proprietary", source: sources.claude },
    },
  },
  {
    label: "MCP",
    cells: {
      atticdb: { value: "6 built-in tools" },
      mem0: { value: "Hosted MCP and OpenMemory", source: sources.mem0Mcp },
      zep: { value: "Graphiti reference MCP server", source: sources.zep },
      letta: { value: "Letta Code consumes MCP servers", source: sources.letta },
      supermemory: { value: "Hosted and open-source MCP", source: sources.supermemory },
      redis: { value: "V0: stdio/SSE MCP server", source: sources.redisMcp },
      cognee: { value: "cognee-mcp server and agent plugins", source: sources.cognee },
      engram: { value: "Built-in stdio/HTTP MCP server", source: sources.engram },
      "claude-md": { value: "Not published" },
    },
  },
  {
    label: "semantic or graph recall",
    cells: {
      atticdb: { value: "No, search by words" },
      mem0: { value: "Vector search; optional graph memory", source: sources.mem0 },
      zep: { value: "Temporal knowledge graph", source: sources.zep },
      letta: { value: "Not published" },
      supermemory: { value: "Vector-graph recall", source: sources.supermemory },
      redis: { value: "Vector embeddings for long-term memory", source: sources.redis },
      cognee: { value: "Vector and graph recall", source: sources.cognee },
      engram: { value: "Text search with SQLite FTS5", source: sources.engram },
      "claude-md": { value: "Plain-text notes, no embedding database", source: sources.claude },
    },
  },
];

export const PICK_ATTIC: string[] = [
  "You want turns and facts stored as written, with an ack after fdatasync.",
  "Your agent needs word search, without embeddings or model calls.",
  "You want one Rust binary running on your own disk.",
  "You can use early software that is not production ready.",
];

export const PICK_OTHER: { when: string; product: string }[] = [
  { when: "You need semantic or graph recall.", product: "Mem0, Zep" },
  { when: "You want a full agent harness.", product: "Letta" },
  { when: "You want a mature SQLite tool in Go.", product: "Engram" },
];
