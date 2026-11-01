import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { SITE_URL, VPC_URL, waitlistUrl } from "@/lib/site";
import * as bench from "./bench";
import * as pricing from "./pricing";
import * as compare from "./compare";
import * as faq from "./faq";

const { ackFor, BENCH_META, BENCH_ROWS } = bench;
const { PLANS, PLAN_FEATURES } = pricing;
const { PRODUCTS, COMPARE_ROWS, PICK_OTHER } = compare;
const { PRICING_FAQ } = faq;

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
    expect(PLANS.find((plan) => plan.id === "pro")?.cta.href).toBe(waitlistUrl("pro"));
    expect(PLANS.find((plan) => plan.id === "team")?.cta.href).toBe(waitlistUrl("team"));
    expect(PLANS.find((plan) => plan.id === "vpc")?.cta.href).toBe(VPC_URL);
    expect(PLANS.find((plan) => plan.id === "local")?.cta).toEqual({ label: "Install AtticDB" });
  });

  it("covers each plan in the full feature table", () => {
    expect(PLAN_FEATURES).toHaveLength(15);
    expect(PLAN_FEATURES[0]).toEqual({
      feature: "Status",
      values: { local: "Available", pro: "Planned", team: "Planned", vpc: "Planned" },
    });
    for (const row of PLAN_FEATURES) {
      expect(Object.keys(row.values).sort()).toEqual(["local", "pro", "team", "vpc"]);
      for (const value of Object.values(row.values)) {
        expect(value).not.toMatch(/Not (specified|announced)/);
      }
    }
  });

  it("does not describe Team social sign-in as SSO", () => {
    expect(PLAN_FEATURES.find((row) => row.feature === "SSO")?.values.team).toBe("Not included");
    expect(PLAN_FEATURES.find((row) => row.feature === "GitHub and Google sign-in")?.values.team).toBe("Included");
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
    expect(COMPARE_ROWS.find((row) => row.label === "MCP")?.cells["claude-md"])
      .toEqual({ value: "Not published" });
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
  it("provides 4 pricing answers", () => {
    expect(PRICING_FAQ).toHaveLength(4);
    expect(PRICING_FAQ.map((item) => item.a)).toEqual([
      "The bytes of your turns, facts and fact history on disk. Forgotten records stop counting after compaction.",
      "That's the plan. Same API and MCP tools, so your agents won't notice. The import tool ships with cloud.",
      "Planned: $1 per extra GB per month. Nothing is deleted and writes never stop for billing.",
      "When enough people ask. Join the waitlist from the buttons above and we'll email you when it opens.",
    ]);
  });

  it("has no em dashes in any content module string", () => {
    for (const content of [bench, pricing, compare, faq]) {
      expect(JSON.stringify(content)).not.toContain("\u2014");
    }
  });
});
