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

  it("renders the Where we lose section with SQLite lower p50 and links to the raw TSVs, runbook, and write-up", () => {
    const { container } = render(<BenchmarksPage />);
    expect(screen.getByRole("heading", { level: 2, name: "Where we lose" })).toBeInTheDocument();
    expect(container.textContent).toContain("SQLite has a lower p50");

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
