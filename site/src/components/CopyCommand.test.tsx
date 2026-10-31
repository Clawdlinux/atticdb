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
});
