import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import BenchmarksPage, { metadata } from "@/app/benchmarks/page";
import { BENCH_ROWS } from "@/content/bench";
import { GITHUB_URL } from "@/lib/site";

beforeAll(() => {
  globalThis.IntersectionObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
    takeRecords() {
      return [];
    }
  } as unknown as typeof IntersectionObserver;
});

afterEach(cleanup);

describe("benchmarks page", () => {
  it("exports page metadata with the spec title and a concise description", () => {
    expect(metadata.title).toBe("AtticDB benchmarks: durable acks vs Redis and SQLite");
    expect(typeof metadata.description).toBe("string");
    expect((metadata.description as string).length).toBeGreaterThan(20);
    expect((metadata.description as string).length).toBeLessThanOrEqual(155);
  });

  it("renders one h1 with the two-line benchmarks headline and supporting sentence", () => {
    const { container } = render(<BenchmarksPage />);
    const h1s = container.querySelectorAll("h1");
    expect(h1s).toHaveLength(1);
    expect(h1s[0]).toHaveTextContent("Durable acks,");
    expect(h1s[0]).toHaveTextContent("Measured.");
    expect(
      screen.getByText(
        "How fast can each system ack a write that survives power loss? 5 systems, 2 disks, 7 runs each.",
      ),
    ).toBeInTheDocument();
  });

  it("gives every section after the hero an h2", () => {
    const { container } = render(<BenchmarksPage />);
    const sections = [...container.querySelectorAll("section")];
    expect(sections.length).toBeGreaterThanOrEqual(5);
    for (const section of sections) {
      if (section.querySelector("h1")) continue;
      expect(section.querySelector("h2"), section.className).not.toBeNull();
    }
  });

  it("renders 2 BenchChart SVGs with role=img, aria-labels, and visually hidden table fallbacks", () => {
    const { container } = render(<BenchmarksPage />);
    const charts = screen.getAllByRole("img");
    const svgCharts = charts.filter((el) => el.tagName.toLowerCase() === "svg");
    expect(svgCharts).toHaveLength(2);
    for (const svg of svgCharts) {
      const label = svg.getAttribute("aria-label") ?? "";
      expect(label).toContain("AtticDB");
      expect(svg.querySelectorAll("rect[data-bar]")).toHaveLength(15);
      expect(svg.querySelectorAll('rect[data-bar="atticdb"][fill="var(--accent)"]')).toHaveLength(
        3,
      );
      const region = svg.closest('[role="region"]');
      expect(region).not.toBeNull();
      expect(region).toHaveAttribute("tabindex", "0");
      expect((svg as HTMLElement).style.minWidth).toBe("872px");
    }
    const fallbackTables = container.querySelectorAll("table.visually-hidden");
    expect(fallbackTables).toHaveLength(2);
  });

  it("renders 4 benchmark data tables with formatted numbers, including 9,799 and 94 in the NVMe 1-writer AtticDB row", () => {
    const { container } = render(<BenchmarksPage />);
    const details = [...container.querySelectorAll("details")];
    expect(details).toHaveLength(2);
    expect(details.map((item) => item.querySelector("summary")?.textContent)).toEqual(["All rows, Premium SSD v2", "All rows, Local NVMe"]);
    for (const item of details) {
      expect(item.open).toBe(false);
      item.open = true;
    }
    const dataTables = [...container.querySelectorAll("details table")];
    expect(dataTables.reduce((n, table) => n + table.querySelectorAll("tbody tr").length, 0)).toBe(50);
    expect(dataTables).toHaveLength(4);
    for (const table of dataTables) {
      expect(table.querySelector("caption")).not.toBeNull();
      const region = table.closest('[role="region"]');
      expect(region).not.toBeNull();
      expect(region).toHaveAttribute("tabindex", "0");
    }

    const nvmeClosedRegion = screen.getByRole("region", {
      name: /Local NVMe.*closed/i,
    });
    const nvmeClosedTable = within(nvmeClosedRegion).getByRole("table");
    const firstBodyRow = nvmeClosedTable.querySelector("tbody tr");
    expect(firstBodyRow).not.toBeNull();
    expect(firstBodyRow).toHaveTextContent("1");
    expect(firstBodyRow).toHaveTextContent("atticdb");
    expect(firstBodyRow).toHaveTextContent("9,799");
    expect(firstBodyRow).toHaveTextContent("94");
  });

  it("summarizes each load against the best server per metric and SQLite", () => {
    const { container } = render(<BenchmarksPage />);
    const format = (n: number) => new Intl.NumberFormat("en-US").format(n);
    for (const disk of ["premium-v2", "nvme"] as const) {
      const table = container.querySelector(`[data-ui="bench-summary-${disk}"] table`)!;
      expect(table).not.toBeNull();
      const rows = [...table.querySelectorAll("tbody tr")];
      const attic = BENCH_ROWS.filter(r => r.disk === disk && r.system === "atticdb");
      expect(rows).toHaveLength(5);
      attic.forEach((ours, i) => {
        const peers = BENCH_ROWS.filter(r => r.disk === disk && r.load === ours.load && r.writers === ours.writers);
        const servers = peers.filter(r => ["redis", "valkey", "dicedb"].includes(r.system));
        const fastest = servers.find(row => row.acksPerSec === Math.max(...servers.map(r => r.acksPerSec)))!;
        const lowest = servers.find(row => row.p99us === Math.min(...servers.map(r => r.p99us)))!;
        const sqlite = peers.find(r => r.system === "sqlite")!;
        expect([...rows[i].querySelectorAll("td")].map(td => td.textContent)).toEqual([
          format(ours.acksPerSec), format(ours.p99us),
          `${format(fastest.acksPerSec)} (${fastest.system})`, `${format(lowest.p99us)} (${lowest.system})`,
          format(sqlite.acksPerSec), format(sqlite.p99us),
        ]);
      });
    }
  });

  it("derives win comparisons from benchmark rows, not copied figures", () => {
    render(<BenchmarksPage />);
    const section = screen.getByRole("region", { name: "Where we win, where we lose" });
    expect(within(section).getAllByRole("heading", { level: 2 })).toHaveLength(1);
    const winHeading = within(section).getByRole("heading", { level: 3, name: "We win" });
    const loseHeading = within(section).getByRole("heading", { level: 3, name: "We lose" });
    expect(winHeading.compareDocumentPosition(loseHeading) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    const wins = winHeading.parentElement!;
    const premium = BENCH_ROWS.filter(row => row.disk === "premium-v2");
    const servers = premium.filter(row => ["redis", "valkey", "dicedb"].includes(row.system));
    const ratios = servers.filter(row => row.load === "closed").map(row => {
      const ours = premium.find(ours => ours.system === "atticdb" && ours.load === row.load && ours.writers === row.writers)!;
      return ours.acksPerSec / row.acksPerSec;
    });
    expect(wins).toHaveTextContent(`${Math.min(...ratios).toFixed(1)}x to ${Math.max(...ratios).toFixed(1)}x their closed-loop acks/s`);
    const agentP50s = servers.filter(row => row.load === "agent").map(row => row.p50us / 1000);
    const ourAgentP50 = premium.find(row => row.system === "atticdb" && row.load === "agent")!.p50us / 1000;
    expect(wins).toHaveTextContent(`agent-load p50 is ${ourAgentP50.toFixed(1)} ms versus ${Math.min(...agentP50s).toFixed(1)} to ${Math.max(...agentP50s).toFixed(1)} ms`);

    const nvme = BENCH_ROWS.filter(row => row.disk === "nvme" && row.load === "closed" && row.writers === 16);
    const ours = nvme.find(row => row.system === "atticdb")!;
    const peers = nvme.filter(row => ["redis", "valkey", "dicedb"].includes(row.system));
    const format = (n: number) => new Intl.NumberFormat("en-US").format(n);
    expect(wins).toHaveTextContent(`${format(ours.acksPerSec)} versus ${format(Math.min(...peers.map(row => row.acksPerSec)))} to ${format(Math.max(...peers.map(row => row.acksPerSec)))} acks/s`);
    expect(wins).toHaveTextContent(`p99 ${format(ours.p99us)} versus ${format(Math.min(...peers.map(row => row.p99us)))} to ${format(Math.max(...peers.map(row => row.p99us)))} µs`);
    expect(wins).toHaveTextContent("worst p99 rep beats their best");
    expect(wins).toHaveTextContent("best p50 and p99.9");
    expect(wins).toHaveTextContent("Preallocated, zero-filled segments need 1 device round trip per ack instead of 2.");
    expect(wins).toHaveTextContent("Group commit syncs queued writers with no timer.");
  });

  it("keeps the loss sentences and raw benchmark links beside the wins", () => {
    const { container } = render(<BenchmarksPage />);
    expect(screen.getByRole("heading", { level: 2, name: "Where we win, where we lose" })).toBeInTheDocument();
    expect(container.textContent).toContain("SQLite has a lower p50");
    const losses = screen.getByRole("heading", { level: 3, name: "We lose" }).parentElement!;
    expect([...losses.querySelectorAll("li")].map(item => item.textContent)).toEqual([
      "SQLite has a lower p50 in closed loop at every writer count on both disks. It runs in-process, without sockets.",
      "On Premium SSD v2, SQLite has lower closed-loop p99 and p99.9 at 4 and 16 writers.",
      "On NVMe, SQLite has lower closed-loop p99 at 4 and 16 writers, and lower p99.9 at 16. Its agent-load p50 is lower too.",
      "On NVMe at 16 closed-loop writers, Redis and DiceDB beat AtticDB on p50, p99.9 and max latency.",
      "On NVMe under agent load, Redis has lower p99 at 64 writers. DiceDB and Valkey win at 256.",
    ]);

    const expectedHrefs = [
      `${GITHUB_URL}/blob/main/bench/h2h-nvme.tsv`,
      `${GITHUB_URL}/blob/main/bench/h2h-premium-v2.tsv`,
      `${GITHUB_URL}/blob/main/bench/h2h.sh`,
      `${GITHUB_URL}/blob/main/bench/H2H.md`,
    ];
    for (const href of expectedHrefs) {
      expect(container.querySelector(`a[href="${href}"]`), href).not.toBeNull();
    }
  });
});
