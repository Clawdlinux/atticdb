import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import PricingPage, { metadata } from "@/app/pricing/page";
import { PRICING_FAQ } from "@/content/faq";
import { PLAN_FEATURES, PLANS, PRICING_NOTES } from "@/content/pricing";
import { INSTALL_CMD, VPC_URL, WAITLIST_URL } from "@/lib/site";

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

describe("pricing page", () => {
  it("exports page metadata with the spec title and a concise description", () => {
    expect(metadata.title).toBe("AtticDB pricing: free locally, cloud planned");
    expect(typeof metadata.description).toBe("string");
    expect((metadata.description as string).length).toBeGreaterThan(20);
    expect((metadata.description as string).length).toBeLessThanOrEqual(155);
  });

  it("renders one h1 with the two-line pricing headline and supporting sentence", () => {
    const { container } = render(<PricingPage />);
    const h1s = container.querySelectorAll("h1");
    expect(h1s).toHaveLength(1);
    expect(h1s[0]).toHaveTextContent("Free on your disk.");
    expect(h1s[0]).toHaveTextContent("Cloud when you want it.");
    expect(
      screen.getByText(
        "Run AtticDB free on your own machine. The cloud is planned, and you can join the waitlist now.",
      ),
    ).toBeInTheDocument();
  });

  it("gives every section after the hero an h2", () => {
    const { container } = render(<PricingPage />);
    const sections = [...container.querySelectorAll("section")];
    expect(sections.length).toBeGreaterThanOrEqual(5);
    for (const section of sections) {
      if (section.querySelector("h1")) continue;
      expect(section.querySelector("h2"), section.className).not.toBeNull();
    }
  });

  it("renders the 4 plans in order with status, includes and actions", () => {
    render(<PricingPage />);
    const plansRegion = screen.getByRole("region", { name: "Plans" });
    const planHeadings = within(plansRegion)
      .getAllByRole("heading", { level: 2 })
      .map((h) => h.textContent);
    expect(planHeadings).toEqual(["Local", "Cloud Pro", "Cloud Team", "Your cloud"]);

    const planned = within(plansRegion).getAllByText("Planned.");
    expect(planned).toHaveLength(3);
    expect(within(plansRegion).getByText("Available.")).toBeInTheDocument();
    // Status reads inline, in one sentence with the terms.
    expect(planned[0].parentElement).toHaveTextContent("$15 per month. Planned.");
    expect(within(plansRegion).getByText("Available.").parentElement).toHaveTextContent(
      "$0 forever. Available.",
    );

    expect(within(plansRegion).getByText(INSTALL_CMD).tagName).toBe("CODE");
    expect(within(plansRegion).getByRole("button", { name: /Install AtticDB/i })).toBeInTheDocument();

    const waitlistLinks = within(plansRegion).getAllByRole("link", {
      name: "Join the cloud waitlist",
    });
    expect(waitlistLinks).toHaveLength(2);
    for (const link of waitlistLinks) {
      expect(link).toHaveAttribute("href", WAITLIST_URL);
    }

    const vpcLink = within(plansRegion).getByRole("link", { name: "Talk to us" });
    expect(vpcLink).toHaveAttribute("href", VPC_URL);
  });

  it("heads the notes with a fact, not a repeat of the note", () => {
    render(<PricingPage />);
    expect(screen.getByRole("heading", { level: 2, name: "No per-call fees" })).toBeInTheDocument();
  });

  it("renders both pricing notes as statement paragraphs", () => {
    render(<PricingPage />);
    for (const note of PRICING_NOTES) {
      const el = screen.getByText(note);
      expect(el.tagName).toBe("P");
    }
  });

  it("renders the feature comparison table inside a keyboard-scrollable region", () => {
    render(<PricingPage />);
    const region = screen.getByRole("region", { name: "Plan comparison" });
    expect(region).toHaveAttribute("tabindex", "0");

    const table = within(region).getByRole("table");
    expect(table.querySelector("caption")).not.toBeNull();

    const colHeaders = within(table)
      .getAllByRole("columnheader")
      .map((th) => th.textContent);
    expect(colHeaders.slice(-4)).toEqual(PLANS.map((p) => p.name));

    const rowHeaders = within(table)
      .getAllByRole("rowheader")
      .map((th) => th.textContent);
    expect(rowHeaders).toEqual(PLAN_FEATURES.map((f) => f.feature));

    const bodyRows = table.querySelectorAll("tbody tr");
    expect(bodyRows).toHaveLength(PLAN_FEATURES.length);
    for (const row of PLAN_FEATURES) {
      for (const value of Object.values(row.values)) {
        expect(value).not.toBe("Not specified");
      }
    }
    expect(within(table).queryByText("Not specified")).toBeNull();
  });

  it("renders all 4 pricing FAQ questions and answers visibly", () => {
    render(<PricingPage />);
    for (const { q, a } of PRICING_FAQ) {
      expect(screen.getByRole("heading", { level: 3, name: q })).toBeVisible();
      expect(screen.getByText(a)).toBeVisible();
    }
  });
});
