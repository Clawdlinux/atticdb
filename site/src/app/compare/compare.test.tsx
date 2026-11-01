import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import ComparePage, { metadata } from "@/app/compare/page";
import { COMPARE_ROWS, PICK_ATTIC, PICK_OTHER, PRODUCTS } from "@/content/compare";

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

describe("compare page", () => {
  it("exports page metadata with the spec title and a concise description", () => {
    expect(metadata.title).toBe("AtticDB vs Mem0, Zep, Letta and Supermemory");
    expect(typeof metadata.description).toBe("string");
    expect((metadata.description as string).length).toBeGreaterThan(20);
    expect((metadata.description as string).length).toBeLessThanOrEqual(155);
  });

  it("renders one h1 with the two-line compare headline and supporting sentence", () => {
    const { container } = render(<ComparePage />);
    const h1s = container.querySelectorAll("h1");
    expect(h1s).toHaveLength(1);
    expect(h1s[0]).toHaveTextContent("Where we win.");
    expect(h1s[0]).toHaveTextContent("Where we don't.");
    expect(
      screen.getByText(
        "Facts from each product's own docs, seen 31 October 2026. Where a vendor publishes nothing, the cell says so.",
      ),
    ).toBeInTheDocument();
  });

  it("gives every section after the hero an h2", () => {
    const { container } = render(<ComparePage />);
    const sections = [...container.querySelectorAll("section")];
    expect(sections.length).toBeGreaterThanOrEqual(4);
    for (const section of sections) {
      if (section.querySelector("h1")) continue;
      expect(section.querySelector("h2"), section.className).not.toBeNull();
    }
  });

  it("renders a table with 9 product column headers, 8 rows, Not published cells, and deduplicated source citations", () => {
    const { container } = render(<ComparePage />);
    const region = screen.getByRole("region", { name: /comparison/i });
    expect(region).toHaveAttribute("tabindex", "0");

    const table = within(region).getByRole("table");
    expect(table.querySelector("caption")).not.toBeNull();

    const colHeaders = within(table)
      .getAllByRole("columnheader")
      .map((th) => th.textContent);
    expect(colHeaders).toHaveLength(9);
    expect(colHeaders).toEqual(PRODUCTS.map((p) => p.name));

    const bodyRows = table.querySelectorAll("tbody tr");
    expect(bodyRows).toHaveLength(8);

    const rowHeaders = within(table)
      .getAllByRole("rowheader")
      .map((th) => th.textContent);
    expect(rowHeaders).toEqual(COMPARE_ROWS.map((r) => r.label));

    const notPublished = within(table).getAllByText("Not published");
    expect(notPublished.length).toBeGreaterThan(0);
    for (const cell of notPublished) {
      expect(cell.querySelector("a")).toBeNull();
    }

    const sourcesList = container.querySelector("ol");
    expect(sourcesList).not.toBeNull();

    const uniqueSourceUrls = new Set<string>();
    COMPARE_ROWS.forEach((row, rowIndex) => {
      const tr = bodyRows[rowIndex];
      const cells = tr.querySelectorAll("td");
      PRODUCTS.forEach((product, prodIndex) => {
        const cellData = row.cells[product.id];
        const td = cells[prodIndex];
        expect(td).toHaveTextContent(cellData.value);
        if (product.id !== "atticdb" && cellData.source) {
          uniqueSourceUrls.add(cellData.source);
          const refLink = td.querySelector("sup a");
          expect(refLink).not.toBeNull();
          const href = refLink?.getAttribute("href");
          expect(href).toMatch(/^#src-\d+$/);

          const targetItem = container.querySelector(href!);
          expect(targetItem).not.toBeNull();
          expect(targetItem?.parentElement).toBe(sourcesList);
          expect(targetItem).toHaveTextContent(product.name);
          expect(targetItem).toHaveTextContent(row.label);
          const externalLink = targetItem?.querySelector(`a[href="${cellData.source}"]`);
          expect(externalLink).not.toBeNull();
        } else {
          expect(td.querySelector("sup a")).toBeNull();
        }
      });
    });

    expect(sourcesList?.querySelectorAll("li")).toHaveLength(uniqueSourceUrls.size);
  });

  it("renders Pick AtticDB if and Pick something else if headings with all cases", () => {
    render(<ComparePage />);
    expect(screen.getByRole("heading", { level: 2, name: "Which to pick" })).toBeVisible();
    expect(screen.getAllByRole("heading", { level: 2 })).toHaveLength(3);
    expect(screen.getByRole("heading", { level: 3, name: "Pick AtticDB if" })).toBeInTheDocument();
    for (const item of PICK_ATTIC) {
      expect(screen.getByText(item)).toBeInTheDocument();
    }

    expect(
      screen.getByRole("heading", { level: 3, name: "Pick something else if" }),
    ).toBeInTheDocument();
    const otherSection = screen.getByRole("region", { name: "Which to pick" });
    expect(PICK_OTHER).toHaveLength(3);
    for (const { when, product } of PICK_OTHER) {
      expect(within(otherSection).getByText(when)).toBeInTheDocument();
      expect(within(otherSection).getByText(product)).toBeInTheDocument();
    }
  });
});
