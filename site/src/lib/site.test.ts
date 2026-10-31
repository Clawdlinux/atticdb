import { afterEach, describe, expect, it, vi } from "vitest";
import { absoluteUrl, siteHref, DOCS_ORIGIN, DOCS_URL, INSTALL_CMD, WAITLIST_URL, VPC_URL } from "@/lib/site";

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

  it("opens the cloud waitlist issue", () => {
    expect(WAITLIST_URL).toBe(
      "https://github.com/Clawdlinux/atticdb/issues/new?template=feature.yml&title=Cloud%20waitlist%3A%20",
    );
  });

  it("points the docs link at the docs host", () => {
    expect(DOCS_URL).toBe("https://docs.atticdb.com");
    expect(DOCS_ORIGIN).toBe("https://docs.atticdb.com");
  });

  it("opens the VPC request issue", () => {
    expect(VPC_URL).toBe(
      "https://github.com/Clawdlinux/atticdb/issues/new?template=feature.yml&title=Cloud%20in%20my%20VPC%3A%20",
    );
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
