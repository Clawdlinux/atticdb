import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import Hero from "./Hero";

// Its own file, so the canvas module is not yet loaded when the button is pressed.
describe("Hero before the tape loads", () => {
  afterEach(cleanup);

  it("holds a pull made before the canvas loads, then cuts", async () => {
    render(<Hero />);
    fireEvent.click(screen.getByRole("button", { name: "Pull the plug" }));
    expect(screen.getByText("Cutting the power mid-group.")).toBeInTheDocument();
    expect(await screen.findByText("Power off. Frames not yet synced drop away.")).toBeInTheDocument();
  });
});
