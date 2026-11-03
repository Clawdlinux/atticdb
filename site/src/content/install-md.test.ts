// @vitest-environment node
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { APPS } from "@/content/install";

// public/install.md is what an agent runs when a user pastes the one-line prompt.
// It must stay in step with the installer's app names and the CLI it drives.
const INSTALL_MD = readFileSync(resolve(process.cwd(), "public/install.md"), "utf8");
const MAIN_RS = "/Users/sunny/clawdlinux/atticdb/crates/atticdb/src/main.rs";
const SUBCOMMANDS = ["--version", "setup", "doctor", "fact set", "disconnect"];

describe("public/install.md", () => {
  it("names exactly the apps the installer uses", () => {
    expect([...APPS].sort()).toEqual(["claude-code", "codex", "copilot", "vscode"]);
    const table = INSTALL_MD.split("## 3.")[1].split("\n\n").find((block) => block.startsWith("| "))!;
    const names = [...table.matchAll(/\| `([a-z-]+)` \|/g)].map((m) => m[1]);
    expect([...names].sort()).toEqual([...APPS].sort());
  });

  it("shows each atticdb subcommand it relies on", () => {
    for (const sub of SUBCOMMANDS) expect(INSTALL_MD).toContain(`atticdb ${sub}`);
    // Every command it runs: a code line or an inline code span that starts with atticdb.
    const used = new Set([...INSTALL_MD.matchAll(/(?:^|`)atticdb (--version|[a-z]+(?: set)?)\b/gm)].map((m) => m[1]));
    expect([...used].sort()).toEqual([...SUBCOMMANDS].sort());
  });

  it.skipIf(!existsSync(MAIN_RS))("uses only subcommands the CLI's USAGE lists", () => {
    const usage = readFileSync(MAIN_RS, "utf8").match(/const USAGE: &str = "([\s\S]*?)";/)![1];
    for (const sub of SUBCOMMANDS) expect(usage).toContain(`atticdb ${sub}`);
    for (const flag of ["--dry-run", "--yes", "--harness"]) expect(usage).toMatch(new RegExp(`atticdb setup [^\\n]*${flag}`));
  });

  it("has no em dash", () => {
    expect(INSTALL_MD).not.toContain("—");
  });
});
