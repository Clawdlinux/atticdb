import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AGENT_PROMPT, INSTALL_MD_URL } from "@/lib/site";
import AgentPrompt from "./AgentPrompt";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("AgentPrompt", () => {
  it("is the exact one line the owner wrote", () => {
    expect(AGENT_PROMPT).toBe(
      "Set up AtticDB memory for me. Follow https://atticdb.com/install.md step by step.",
    );
    expect(INSTALL_MD_URL).toBe("https://atticdb.com/install.md");
  });

  it("shows the prompt as prose, not code", () => {
    render(<AgentPrompt />);
    const text = screen.getByText(AGENT_PROMPT);
    expect(text.tagName).toBe("P");
    expect(text.closest("pre, code")).toBeNull();
  });

  it("copies the prompt exactly and says Copied", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(<AgentPrompt />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    });
    expect(writeText).toHaveBeenCalledWith(AGENT_PROMPT);
    expect(screen.getByRole("button", { name: "Copied" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Prompt copied.");
  });

  it("names the apps and links to the steps", () => {
    const { container } = render(<AgentPrompt />);
    expect(container.textContent).toContain(
      "Works in Claude Code, Codex, GitHub Copilot CLI and VS Code. Read the steps first: install.md",
    );
    expect(screen.getByRole("link", { name: "install.md" })).toHaveAttribute("href", INSTALL_MD_URL);
  });

  it("puts the prompt text in the server HTML, so it shows without JS", () => {
    const html = renderToString(<AgentPrompt />);
    expect(html).toContain(AGENT_PROMPT);
    expect(html).toContain(`href="${INSTALL_MD_URL}"`);
  });
});
