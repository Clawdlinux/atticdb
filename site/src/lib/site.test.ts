import { describe, expect, it } from "vitest";
import { absoluteUrl, DOCS_ORIGIN, DOCS_URL, INSTALL_CMD, WAITLIST_URL, VPC_URL } from "@/lib/site";

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
