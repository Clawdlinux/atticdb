import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import CopyCommand from "./CopyCommand";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function shownLabel(container: HTMLElement) {
  return container.querySelector('button [data-present="true"]')?.textContent;
}

describe("CopyCommand", () => {
  it("renders the command as code and the label at rest", () => {
    const { container } = render(<CopyCommand command="atticdb setup" label="Copy setup" />);
    expect(screen.getByText("atticdb setup").tagName).toBe("CODE");
    expect(shownLabel(container)).toBe("Copy setup");
    expect(screen.getByRole("button", { name: "Copy setup" })).toBeInTheDocument();
  });

  it("swaps the label to Copied after a copy", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    const { container } = render(<CopyCommand command="atticdb setup" label="Copy setup" />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button"));
    });
    expect(writeText).toHaveBeenCalledWith("atticdb setup");
    expect(shownLabel(container)).toBe("Copied");
    expect(screen.getByRole("button", { name: "Copied" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Command copied.");
  });

  it("selects the command when the clipboard is refused", async () => {
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: vi.fn().mockRejectedValue(new Error("denied")) },
      configurable: true,
    });
    const { container } = render(<CopyCommand command="atticdb setup" />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button"));
    });
    expect(shownLabel(container)).toBe("Selected");
  });

  it("sets the button in oxide ink on a bone field", () => {
    const { container } = render(<CopyCommand tone="bone" />);
    expect(container.firstElementChild?.className).toMatch(/bone/);
  });

  it("wraps on spaces only when asked, and copies the exact command", async () => {
    const command = "atticdb setup --harness copilot,vscode,claude-code,codex --dry-run";
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    const { container } = render(<CopyCommand command={command} wrap="tokens" />);
    const code = container.querySelector("code")!;
    expect(code.textContent).toBe(command);
    const flag = [...code.querySelectorAll("span")].find((s) => s.textContent === "--dry-run");
    expect(flag).toBeDefined();
    expect(flag!.dataset.token).toBe("word");
    expect(flag!.className).toMatch(/token/);
    const list = [...code.querySelectorAll("span")].find((s) => s.textContent?.startsWith("copilot,"));
    expect(list!.dataset.token).toBe("split");
    await act(async () => {
      fireEvent.click(screen.getByRole("button"));
    });
    expect(writeText).toHaveBeenCalledWith(command);
  });

  it("breaks a long token only after commas and slashes and before =", () => {
    const command = "atticdb setup --harness=copilot,vscode,claude-code,codex https://atticdb.example/a/b";
    const { container } = render(<CopyCommand command={command} wrap="tokens" />);
    const code = container.querySelector("code")!;
    expect(code.textContent).toBe(command);
    const split = [...code.querySelectorAll('[data-token="split"]')];
    expect(split).toHaveLength(2);
    const parts = (el: Element) => [...el.children].filter((c) => c.tagName === "SPAN").map((c) => c.textContent);
    expect(parts(split[0])).toEqual(["--harness", "=copilot,", "vscode,", "claude-code,", "codex"]);
    expect(split[0].querySelectorAll("wbr")).toHaveLength(4);
    expect(parts(split[1])).toEqual(["https:/", "/", "atticdb.example/", "a/", "b"]);
    // No point to break at: the last resort, anywhere.
    const { container: bare } = render(<CopyCommand command="atticdbexampleatticdbexample" wrap="tokens" />);
    expect(bare.querySelector('[data-token="long"]')?.textContent).toBe("atticdbexampleatticdbexample");
  });

  it("keeps the command as one text node by default", () => {
    const { container } = render(<CopyCommand command="atticdb setup --dry-run" />);
    expect(container.querySelector("code span")).toBeNull();
  });
});
