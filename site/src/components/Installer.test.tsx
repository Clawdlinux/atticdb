import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { INSTALL_CMD } from "@/lib/site";
import { DEFAULT_OPTIONS, buildInstallSteps } from "@/content/install";
import Installer from "./Installer";

const MAC_UA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
const WINDOWS_UA =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
const SETUP = "atticdb setup --harness copilot,vscode,claude-code,codex";

// Commands render as tokens, so match the whole <code> by its text.
function codeWith(text: string) {
  return (_: string, el: Element | null) => el?.tagName === "CODE" && el.textContent === text;
}

function setUserAgent(ua: string) {
  vi.spyOn(navigator, "userAgent", "get").mockReturnValue(ua);
}

// Lines still leaving carry aria-hidden; only the present ones count.
function presentCommands(container: HTMLElement) {
  return [...container.querySelectorAll('[data-ui="steps"] [data-ui="command"][data-present="true"] code')].map(
    (code) => code.textContent,
  );
}

beforeEach(() => setUserAgent(MAC_UA));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("Installer", () => {
  it("offers 3 systems as a radio group with macOS checked", () => {
    render(<Installer />);
    const system = screen.getByRole("group", { name: "Your system" });
    const radios = within(system).getAllByRole("radio");
    expect(radios.map((r) => r.getAttribute("value"))).toEqual(["macos", "linux", "windows"]);
    expect(within(system).getByRole("radio", { name: "macOS" })).toBeChecked();
  });

  it("checks all 4 apps and shows the setup command", () => {
    render(<Installer />);
    const apps = screen.getByRole("group", { name: "Apps to connect" });
    const boxes = within(apps).getAllByRole("checkbox");
    expect(boxes).toHaveLength(4);
    for (const box of boxes) expect(box).toBeChecked();
    expect(screen.getByText(codeWith(SETUP))).toBeVisible();
  });

  it("renders the steps as an ordered list, with the doctor last", () => {
    const { container } = render(<Installer />);
    const list = container.querySelector('[data-ui="steps"] ol');
    expect(list).not.toBeNull();
    expect(presentCommands(container)).toEqual(
      buildInstallSteps(DEFAULT_OPTIONS).flatMap((s) => s.commands),
    );
  });

  it("wraps its commands on spaces", () => {
    const { container } = render(<Installer />);
    const flag = [...container.querySelectorAll('[data-ui="steps"] code span')].find(
      (s) => s.textContent === "--dry-run",
    );
    expect((flag as HTMLElement | undefined)?.dataset.token).toBe("word");
  });

  it("marks every tile with a tick shape that only a checked input shows", () => {
    const { container } = render(<Installer />);
    for (const input of container.querySelectorAll("input")) {
      const tick = input.closest("label")!.querySelector('svg[data-ui="tick"]');
      expect(tick, input.getAttribute("value") ?? "checkbox").not.toBeNull();
      expect(tick).toHaveAttribute("aria-hidden", "true");
    }
  });

  it("asks for an app when every app is unchecked", () => {
    render(<Installer />);
    const apps = screen.getByRole("group", { name: "Apps to connect" });
    for (const name of ["Copilot CLI", "VS Code", "Claude Code", "Codex"]) {
      fireEvent.click(within(apps).getByRole("checkbox", { name }));
    }
    expect(screen.getByText("Pick at least 1 app to connect.")).toBeInTheDocument();
  });

  it("shows the hook option only while Claude Code or Codex is checked", () => {
    render(<Installer />);
    expect(screen.getByRole("checkbox", { name: "Load memory when a chat starts" })).toBeChecked();
    const apps = screen.getByRole("group", { name: "Apps to connect" });
    fireEvent.click(within(apps).getByRole("checkbox", { name: "Claude Code" }));
    fireEvent.click(within(apps).getByRole("checkbox", { name: "Codex" }));
    expect(screen.queryByRole("checkbox", { name: "Load memory when a chat starts" })).toBeNull();
  });

  it("swaps the install script for the release zip on Windows", () => {
    const { container } = render(<Installer />);
    expect(screen.getByRole("radio", { name: "Install script" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("radio", { name: "Windows" }));
    expect(screen.queryByRole("radio", { name: "Install script" })).toBeNull();
    expect(screen.getByRole("radio", { name: "Release zip" })).toBeChecked();
    expect(screen.getByRole("link", { name: "latest release" })).toHaveAttribute(
      "href",
      "https://github.com/Clawdlinux/atticdb/releases/latest",
    );
    expect(screen.getByText("On Windows, Codex gets only its MCP entry for now.")).toBeInTheDocument();
    expect(
      screen.getByText("Claude Code needs version 2.1.139 or later for the chat-start hook."),
    ).toBeInTheDocument();
    expect(presentCommands(container)[0]).toMatch(/^\(Get-FileHash/);
  });

  it("keeps the Windows notes off other systems", () => {
    render(<Installer />);
    expect(screen.queryByText("On Windows, Codex gets only its MCP entry for now.")).toBeNull();
  });

  it("detects Windows after mount", () => {
    setUserAgent(WINDOWS_UA);
    render(<Installer />);
    expect(screen.getByRole("radio", { name: "Windows" })).toBeChecked();
  });

  it("starts from the options it is given", () => {
    setUserAgent(WINDOWS_UA);
    render(<Installer initial={{ os: "linux", method: "source", preview: false }} />);
    expect(screen.getByRole("radio", { name: "Linux" })).toBeChecked();
    expect(screen.getByRole("radio", { name: "From source" })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: "Preview first" })).not.toBeChecked();
  });

  it("copies every command, one per line", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(<Installer />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Copy all" }));
    });
    expect(writeText).toHaveBeenCalledWith(
      buildInstallSteps(DEFAULT_OPTIONS)
        .flatMap((s) => s.commands)
        .join("\n"),
    );
    expect(screen.getByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  it("renders the macOS steps on the server", () => {
    const text = renderToString(<Installer />).replace(/<[^>]+>/g, "");
    expect(text).toContain(SETUP);
    expect(text).toContain(INSTALL_CMD);
  });
});
