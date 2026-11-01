import { afterEach, describe, expect, it, vi } from "vitest";
import {
  absoluteUrl,
  siteHref,
  waitlistUrl,
  DOCS_ORIGIN,
  DOCS_URL,
  INSTALL_CMD,
  WAITLIST_PLAN_CHOICES,
  WAITLIST_URL,
  VPC_URL,
} from "@/lib/site";

describe("absoluteUrl", () => {
  it.each([
    ["/", "https://atticdb.com"],
    ["/pricing", "https://atticdb.com/pricing"],
    ["pricing/", "https://atticdb.com/pricing"],
    ["", "https://atticdb.com"],
    ["/pricing/", "https://atticdb.com/pricing"],
  ])("normalizes %j to %s", (path, expected) => {
    expect(absoluteUrl(path)).toBe(expected);
  });
});

describe("site links", () => {
  it("uses the release installer", () => {
    expect(INSTALL_CMD).toBe(
      "curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh",
    );
  });

  it("opens the cloud waitlist form", () => {
    expect(WAITLIST_URL).toBe(
      "https://docs.google.com/forms/d/e/1FAIpQLSfliAcFY85SBQAiUpZnllibyhD3xhFpjWmTk1ynmpUWGRIVcQ/viewform",
    );
  });

  it.each(["pro", "team", "vpc"] as const)("prefills the %s plan with the form's exact choice", (plan) => {
    const url = new URL(waitlistUrl(plan));
    expect(`${url.origin}${url.pathname}`).toBe(WAITLIST_URL);
    expect(url.searchParams.get("usp")).toBe("pp_url");
    expect(url.searchParams.get("entry.690882465")).toBe(WAITLIST_PLAN_CHOICES[plan]);
  });

  it("points the docs link at the docs host", () => {
    expect(DOCS_URL).toBe("https://docs.atticdb.com");
    expect(DOCS_ORIGIN).toBe("https://docs.atticdb.com");
  });

  it("opens the form with Your cloud picked for VPC requests", () => {
    expect(VPC_URL).toBe(waitlistUrl("vpc"));
    expect(new URL(VPC_URL).searchParams.get("entry.690882465")).toBe("Your cloud: your VPC or air-gapped");
  });
});

describe("siteHref", () => {
  afterEach(() => vi.unstubAllEnvs());

  it.each([
    ["/", "", "/"],
    ["/pricing", "", "/pricing"],
    ["/#install", "", "/#install"],
    ["/", "https://atticdb.com", "https://atticdb.com/"],
    ["/pricing", "https://atticdb.com", "https://atticdb.com/pricing"],
    ["/#install", "https://atticdb.com", "https://atticdb.com/#install"],
  ])("joins %s to base %j", (path, base, expected) => {
    expect(siteHref(path, base)).toBe(expected);
  });

  it("defaults to relative links outside Vercel", () => {
    vi.stubEnv("SITE_BASE", undefined);
    expect(siteHref("/pricing")).toBe("/pricing");
  });

  it("uses the configured main origin by default", () => {
    vi.stubEnv("SITE_BASE", "https://atticdb.com");
    expect(siteHref("/pricing")).toBe("https://atticdb.com/pricing");
    expect(siteHref("/pricing", "")).toBe("/pricing");
  });
});
