import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { GITHUB_URL, SITE_URL, VPC_URL, WAITLIST_URL } from "@/lib/site";
import * as bench from "./bench";
import * as pricing from "./pricing";
import * as compare from "./compare";
import * as faq from "./faq";
import * as memory from "./memory";

const { ackFor, BENCH_META, BENCH_ROWS } = bench;
const { PLANS, PLAN_FEATURES, PRICING_NOTES } = pricing;
const { PRODUCTS, COMPARE_ROWS, PICK_OTHER } = compare;
const { HOME_FAQ, PRICING_FAQ } = faq;
const { SAMPLE_MEMORY, searchMemory } = memory;

describe("benchmark content", () => {
  it("returns the published AtticDB closed-loop ack values", () => {
    expect(ackFor("nvme", 1)).toEqual({ p50us: 94, acksPerSec: 9799 });
    expect(ackFor("premium-v2", 1)).toEqual({ p50us: 817, acksPerSec: 1167 });
    expect(ackFor("premium-v2", 16).acksPerSec).toBe(9328);
  });

  it("includes all 50 rows and the run metadata", () => {
    expect(BENCH_ROWS).toHaveLength(50);
    expect(BENCH_META).toEqual({
      date: "2026-10-24",
      machine: "Azure Standard_L8s_v3",
      reps: 7,
    });
  });

  it("copies every value from all 4 H2H tables", () => {
    const source = readFileSync("../bench/H2H.md", "utf8");
    const rows = [];
    let disk = "";
    let load = "closed";
    for (const line of source.split("\n")) {
      if (line === "## Premium SSD v2") {
        disk = "premium-v2";
        load = "closed";
      }
      if (line === "## Local NVMe") {
        disk = "nvme";
        load = "closed";
      }
      if (line === "Agent-shaped:") load = "agent";
      if (!/^\| \d[^|]*\| (atticdb|redis|valkey|dicedb|sqlite) \|/.test(line)) continue;
      const [writers, system, acks, p50, p99, range, p999, max] = line
        .split("|").slice(1, -1).map((cell) => cell.trim().replaceAll(",", ""));
      rows.push({
        disk,
        load,
        writers: Number.parseInt(writers),
        thinkMs: load === "closed" ? 0 : Number(writers.match(/(\d+) ms/)?.[1]),
        system,
        acksPerSec: Number(acks),
        p50us: Number(p50),
        p99us: Number(p99),
        p99RangeUs: range.split(" to ").map(Number),
        p999us: Number(p999),
        maxUs: Number(max),
      });
    }
    expect(rows).toHaveLength(50);
    expect(BENCH_ROWS).toEqual(rows);
  });

  it("keeps the published Premium SSD v2 throughput ratios", () => {
    const rowFor = (writers: number, system: bench.System) => BENCH_ROWS.find(
      (row) => row.disk === "premium-v2" && row.load === "closed"
        && row.writers === writers && row.system === system,
    )!;
    expect(Number((rowFor(4, "atticdb").acksPerSec / rowFor(4, "dicedb").acksPerSec).toFixed(1))).toBe(2.8);
    expect(Number((rowFor(16, "atticdb").acksPerSec / rowFor(16, "redis").acksPerSec).toFixed(1))).toBe(1.8);
  });
});

describe("pricing content", () => {
  it("uses the planned prices, availability, and destination URLs", () => {
    expect(PLANS.map((plan) => plan.price)).toEqual(["$0", "$15", "$29", "Talk to us"]);
    expect(PLANS.map((plan) => plan.status)).toEqual(["Available", "Planned", "Planned", "Planned"]);
    expect(PLANS.find((plan) => plan.id === "pro")?.cta.href).toBe(WAITLIST_URL);
    expect(PLANS.find((plan) => plan.id === "team")?.cta.href).toBe(WAITLIST_URL);
    expect(PLANS.find((plan) => plan.id === "vpc")?.cta.href).toBe(VPC_URL);
    expect(PLANS.find((plan) => plan.id === "local")?.cta.href).toMatch(new RegExp(`^${GITHUB_URL}`));
  });

  it("covers each plan in 6 to 9 feature rows and keeps both pricing notes", () => {
    expect(PLAN_FEATURES.length).toBeGreaterThanOrEqual(6);
    expect(PLAN_FEATURES.length).toBeLessThanOrEqual(9);
    for (const row of PLAN_FEATURES) {
      expect(Object.keys(row.values).sort()).toEqual(["local", "pro", "team", "vpc"]);
      for (const value of Object.values(row.values)) {
        expect(value).not.toBe("Not specified");
      }
    }
    expect(PRICING_NOTES).toHaveLength(2);
  });
});

describe("comparison content", () => {
  it("lists the 9 products in spec order and all 8 rows", () => {
    expect(PRODUCTS.map((product) => product.id)).toEqual([
      "atticdb", "mem0", "zep", "letta", "supermemory", "redis", "cognee", "engram", "claude-md",
    ]);
    expect(COMPARE_ROWS.map((row) => row.label)).toEqual([
      "what it stores", "storage it needs", "model call on write", "acked after fsync (published)",
      "runs fully offline", "licence", "MCP", "semantic or graph recall",
    ]);
    expect(PICK_OTHER).toHaveLength(3);
  });

  it("uses homepages and leaves unverified guarantees unpublished", () => {
    expect(PRODUCTS.find((product) => product.id === "atticdb")?.url).toBe(SITE_URL);
    expect(PRODUCTS.find((product) => product.id === "claude-md")?.url).toBe("https://code.claude.com/docs/en/memory");
    const durability = COMPARE_ROWS.find((row) => row.label === "acked after fsync (published)")!;
    for (const product of PRODUCTS.filter((product) => product.id !== "atticdb")) {
      expect(durability.cells[product.id].value).toBe("Not published");
    }
    const offline = COMPARE_ROWS.find((row) => row.label === "runs fully offline")!;
    for (const id of ["mem0", "zep", "letta", "supermemory", "redis", "claude-md"]) {
      expect(offline.cells[id].value).toBe("Not published");
    }
    for (const row of COMPARE_ROWS) {
      for (const cell of Object.values(row.cells)) {
        if (cell.value.toLowerCase().includes("not published")) expect(cell.value).toBe("Not published");
      }
    }
  });

  it("fills every cell with a short, sourced claim or Not published", () => {
    const sources = readFileSync("design/competitors-2026-10-31.md", "utf8");
    for (const row of COMPARE_ROWS) {
      expect(Object.keys(row.cells).sort()).toEqual(PRODUCTS.map((product) => product.id).sort());
      for (const product of PRODUCTS) {
        const cell = row.cells[product.id];
        expect(cell.value.length).toBeLessThan(60);
        if (product.id === "atticdb") {
          expect(cell.source).toBeUndefined();
        } else if (cell.value !== "Not published") {
          expect(cell.source).toMatch(/^https:\/\//);
          expect(sources).toContain(cell.source);
        }
      }
    }
  });
});

describe("FAQ content", () => {
  it("provides all 6 home questions in spec order and 4 pricing answers", () => {
    expect(HOME_FAQ).toHaveLength(6);
    expect(HOME_FAQ.map((item) => item.q)).toEqual([
      "Is it production ready?", "Does it call a model?", "Where is my data?",
      "How is it different from Mem0 or Zep?", "What happens if the disk fills?", "What will the cloud add?",
    ]);
    expect(PRICING_FAQ).toHaveLength(4);
    expect(PRICING_FAQ.map((item) => item.a)).toEqual([
      "The bytes of your turns, facts and fact history on disk. Forgotten records stop counting after compaction.",
      "That's the plan. Same API and MCP tools, so your agents won't notice. The import tool ships with cloud.",
      "Planned: $1 per extra GB per month. Nothing is deleted and writes never stop for billing.",
      "When enough people ask. Add a thumbs-up to the waitlist issue.",
    ]);
  });

  it("has no em dashes in any content module string", () => {
    for (const content of [bench, pricing, compare, faq, memory]) {
      expect(JSON.stringify(content)).not.toContain("\u2014");
    }
  });
});

describe("sample memory search", () => {
  it("includes 12 to 20 dated records with turns and keyed facts", () => {
    expect(SAMPLE_MEMORY.length).toBeGreaterThanOrEqual(12);
    expect(SAMPLE_MEMORY.length).toBeLessThanOrEqual(20);
    expect(new Set(SAMPLE_MEMORY.map((record) => record.kind))).toEqual(new Set(["turn", "fact"]));
    for (const record of SAMPLE_MEMORY) {
      expect(new Date(record.at).toISOString()).toBe(record.at);
      expect(record.at >= "2026-10-20T00:00:00.000Z").toBe(true);
      expect(record.at < "2026-10-31T00:00:00.000Z").toBe(true);
      expect(record.kind === "turn" ? record.session : record.key).toBeTruthy();
    }
    expect(SAMPLE_MEMORY.filter((record) => record.text.includes("ext4")).length).toBeGreaterThanOrEqual(2);
    expect(searchMemory("flaky test").length).toBeGreaterThan(0);
    expect(searchMemory("user prefs").length).toBeGreaterThan(0);
  });

  it("finds ext4 records and rejects missing words", () => {
    const results = searchMemory("ext4");
    expect(results.length).toBeGreaterThan(0);
    for (const record of results) expect(record.text.toLowerCase()).toContain("ext4");
    expect(searchMemory("zzzz")).toEqual([]);
    expect(searchMemory("ext4 zzzz")).toEqual([]);
  });

  it("matches every word case-insensitively across text, key, and session", () => {
    expect(searchMemory("DEPLOY region").length).toBeGreaterThan(0);
    expect(searchMemory("DEPLOY region")).toEqual(searchMemory("  deploy\tREGION\n"));
    expect(searchMemory("repo/atticdb/fs").length).toBeGreaterThan(0);
    expect(searchMemory("atticdb@main").length).toBeGreaterThan(0);
    expect(searchMemory("why did we pick ext4").length).toBeGreaterThan(0);
  });

  it("returns newest first with the default or requested limit, without mutating the sample", () => {
    const original = structuredClone(SAMPLE_MEMORY);
    const all = searchMemory("", 20);
    expect(all.map((record) => record.at)).toEqual(SAMPLE_MEMORY.map((record) => record.at).sort().reverse());
    expect(searchMemory("")).toEqual(all.slice(0, 3));
    expect(searchMemory("  \n\t", 2)).toEqual(all.slice(0, 2));
    expect(searchMemory("", 0)).toEqual([]);
    expect(searchMemory("ext4").map((record) => record.at)).toEqual(
      searchMemory("ext4").map((record) => record.at).sort().reverse(),
    );
    expect(SAMPLE_MEMORY).toEqual(original);
  });
});
