import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { INSTALL_CMD } from "@/lib/site";
import {
  APP_LABELS,
  DEFAULT_OPTIONS,
  PATH_CMD,
  PATH_NOTE,
  buildInstallSteps,
  detectOs,
  hookApplies,
  methodsFor,
  normalize,
  type InstallOptions,
} from "./install";

const ALL = "copilot,vscode,claude-code,codex";

function step(o: InstallOptions, id: string) {
  const found = buildInstallSteps(o).find((s) => s.id === id);
  if (!found) throw new Error(`no step ${id}`);
  return found;
}

describe("buildInstallSteps", () => {
  it("defaults to plain setup on macOS, which finds the apps", () => {
    const steps = buildInstallSteps(DEFAULT_OPTIONS);
    expect(DEFAULT_OPTIONS.apps).toEqual([]);
    expect(steps.map((s) => s.id)).toEqual(["install", "path", "preview", "setup", "doctor"]);
    expect(steps[0].commands).toEqual([INSTALL_CMD]);
    expect(steps[1].commands).toEqual(['export PATH="$HOME/.local/bin:$PATH"']);
    expect(step(DEFAULT_OPTIONS, "preview").commands).toEqual(["atticdb setup --dry-run"]);
    expect(step(DEFAULT_OPTIONS, "setup").commands).toEqual(["atticdb setup"]);
    expect(step(DEFAULT_OPTIONS, "doctor").commands).toEqual(["atticdb doctor"]);
  });

  it("says what to do on command not found, right after the curl line", () => {
    const [, path] = buildInstallSteps(DEFAULT_OPTIONS);
    expect(path.id).toBe("path");
    expect(path.text).toBe(PATH_NOTE);
    expect(path.text).toContain('"command not found"');
    expect(path.text).toContain("~/.local/bin/atticdb setup");
    expect(path.text).toContain("~/.zshrc (the macOS default) or ~/.bashrc");
    expect(path.commands).toEqual([PATH_CMD]);
  });

  it("names every picked app with --harness", () => {
    const o = { ...DEFAULT_OPTIONS, apps: ["copilot", "vscode", "claude-code", "codex"] } as InstallOptions;
    expect(step(o, "setup").commands).toEqual([`atticdb setup --harness ${ALL}`]);
    expect(step(o, "doctor").commands).toEqual([`atticdb doctor --harness ${ALL}`]);
    expect(step(o, "preview").commands).toEqual([`atticdb setup --harness ${ALL} --dry-run`]);
  });

  it("drops the preview step when preview is off", () => {
    const ids = buildInstallSteps({ ...DEFAULT_OPTIONS, preview: false }).map((s) => s.id);
    expect(ids).toEqual(["install", "path", "setup", "doctor"]);
  });

  it("lists the apps in the CLI's order, whatever the input order", () => {
    const o = { ...DEFAULT_OPTIONS, apps: ["codex", "copilot"] } as InstallOptions;
    expect(step(o, "setup").commands).toEqual(["atticdb setup --harness copilot,codex"]);
    expect(step(o, "doctor").commands).toEqual(["atticdb doctor --harness copilot,codex"]);
  });

  it("adds --no-hook only when the hook is off and a hook app is picked", () => {
    const vscode = { ...DEFAULT_OPTIONS, hook: false, apps: ["vscode"] } as InstallOptions;
    for (const s of buildInstallSteps(vscode)) {
      for (const c of s.commands) expect(c).not.toContain("--no-hook");
    }
    const codex = { ...DEFAULT_OPTIONS, hook: false, apps: ["vscode", "codex"] } as InstallOptions;
    expect(step(codex, "setup").commands[0]).toMatch(/ --no-hook$/);
    expect(step(codex, "preview").commands[0]).toBe(
      "atticdb setup --harness vscode,codex --dry-run --no-hook",
    );
    expect(step(codex, "doctor").commands[0]).toBe("atticdb doctor --harness vscode,codex");
  });

  it("adds --no-hook to plain setup when the hook is off, as setup may find Claude Code or Codex", () => {
    const o = { ...DEFAULT_OPTIONS, hook: false };
    expect(hookApplies(o)).toBe(true);
    expect(step(o, "setup").commands).toEqual(["atticdb setup --no-hook"]);
    expect(step(o, "preview").commands).toEqual(["atticdb setup --dry-run --no-hook"]);
    expect(step(o, "doctor").commands).toEqual(["atticdb doctor"]);
    for (const s of buildInstallSteps(o)) for (const c of s.commands) expect(c).not.toContain("--harness");
  });

  it("uses the release zip on Windows", () => {
    const o = normalize({ ...DEFAULT_OPTIONS, os: "windows" });
    expect(o.method).toBe("zip");
    const steps = buildInstallSteps(o);
    expect(steps.map((s) => s.id).slice(0, 2)).toEqual(["download", "install"]);
    expect(steps[0].commands).toEqual([]);
    expect(steps[1].commands).toHaveLength(3);
    expect(steps[1].commands[0].startsWith("(Get-FileHash")).toBe(true);
    expect(steps[1].commands).toEqual([
      "(Get-FileHash .\\atticdb-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()",
      'Expand-Archive .\\atticdb-x86_64-pc-windows-msvc.zip "$env:LOCALAPPDATA\\Programs\\atticdb"',
      '$env:PATH += ";$env:LOCALAPPDATA\\Programs\\atticdb"',
    ]);
  });

  it("builds from source with cargo", () => {
    const o = normalize({ ...DEFAULT_OPTIONS, os: "linux", method: "source" });
    const steps = buildInstallSteps(o);
    expect(steps[0].id).toBe("install");
    expect(steps[0].commands[0].startsWith("cargo install --git")).toBe(true);
    expect(steps.map((s) => s.id)).not.toContain("path");
  });
});

describe("methodsFor and normalize", () => {
  it("offers the script on macOS and Linux and the zip on Windows", () => {
    expect(methodsFor("macos")).toEqual(["script", "source"]);
    expect(methodsFor("linux")).toEqual(["script", "source"]);
    expect(methodsFor("windows")).toEqual(["zip", "source"]);
  });

  it("keeps an allowed method and dedupes the apps in order", () => {
    const o = normalize({
      ...DEFAULT_OPTIONS,
      os: "windows",
      method: "source",
      apps: ["codex", "vscode", "codex"],
    });
    expect(o.method).toBe("source");
    expect(o.apps).toEqual(["vscode", "codex"]);
    expect(normalize({ ...DEFAULT_OPTIONS, method: "zip" }).method).toBe("script");
  });

  it("labels the apps", () => {
    expect(APP_LABELS).toEqual({
      copilot: "Copilot CLI",
      vscode: "VS Code",
      "claude-code": "Claude Code",
      codex: "Codex",
    });
  });
});

describe("detectOs", () => {
  it("reads the user agent", () => {
    expect(
      detectOs(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36",
      ),
    ).toBe("macos");
    expect(
      detectOs(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36",
      ),
    ).toBe("windows");
    expect(detectOs("Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0")).toBe(
      "linux",
    );
    expect(
      detectOs(
        "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Mobile Safari/537.36",
      ),
    ).toBe("linux");
    expect(detectOs("SomethingElse/1.0")).toBe("macos");
    expect(detectOs("")).toBe("macos");
  });

  it("falls back to the platform", () => {
    expect(detectOs("SomethingElse/1.0", "Win32")).toBe("windows");
    expect(detectOs("SomethingElse/1.0", "Linux x86_64")).toBe("linux");
  });
});

describe("install copy", () => {
  it("has no em dash", () => {
    const source = readFileSync("src/content/install.ts", "utf8");
    expect(source).not.toContain("\u2014");
  });
});
