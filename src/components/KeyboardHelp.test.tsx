import { fireEvent, render, screen } from "@testing-library/preact";
import { describe, expect, it } from "vitest";

import { expectNoA11yViolations } from "../test/axe";
import { KeyboardHelp } from "./KeyboardHelp";

describe("KeyboardHelp", () => {
  it("takes focus on open and gives it back on close", () => {
    const opener = document.createElement("button");
    opener.textContent = "opener";
    document.body.appendChild(opener);
    opener.focus();

    const { unmount } = render(<KeyboardHelp onClose={() => {}} />);
    expect(screen.getByRole("button", { name: "Close" })).toHaveFocus();

    unmount();
    expect(opener).toHaveFocus();
    opener.remove();
  });

  it("keeps Tab inside the sheet", () => {
    render(<KeyboardHelp onClose={() => {}} />);
    const close = screen.getByRole("button", { name: "Close" });
    expect(close).toHaveFocus();
    const dialog = screen.getByRole("dialog", { name: "Keyboard shortcuts" });

    // Close is the sheet's only control, so Tab either way stays on it.
    expect(fireEvent.keyDown(dialog, { key: "Tab" })).toBe(false);
    expect(close).toHaveFocus();
    expect(fireEvent.keyDown(dialog, { key: "Tab", shiftKey: true })).toBe(false);
    expect(close).toHaveFocus();
  });
});

describe("KeyboardHelp accessibility", () => {
  it("has no violations", async () => {
    const { container } = render(<KeyboardHelp onClose={() => {}} />);
    await expectNoA11yViolations(container);
  });
});
