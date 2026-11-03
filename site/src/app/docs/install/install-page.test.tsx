import { cleanup, render } from "@testing-library/react";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it } from "vitest";
import DocsInstallPage from "@/app/docs/install/page";
import DocView from "@/app/docs/DocView";
import { INSTALL_CMD } from "@/lib/site";

afterEach(cleanup);

describe("docs install page", () => {
  it("shows the installer open, not folded inside a details", async () => {
    // DocView is an async server component, so resolve it before rendering.
    const page = DocsInstallPage() as ReactElement<Parameters<typeof DocView>[0]>;
    const { container } = render(await DocView(page.props));
    const code = [...container.querySelectorAll("code")].find((c) => c.textContent === INSTALL_CMD);
    expect(code).toBeDefined();
    expect(code!.closest("details")).toBeNull();
    expect(container.querySelector("details")).toBeNull();
  });
});
