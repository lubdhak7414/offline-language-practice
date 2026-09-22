import { fireEvent, render, screen, waitFor } from "@testing-library/preact";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc } from "../ipc/mock";
import { goPrefix } from "../lib/globalKeys";
import { App } from "./App";

let restore: (() => void) | undefined;

beforeEach(() => {
  window.location.hash = "#/practice";
  goPrefix.armed = false;
  restore = setIpc(createMockIpc());
});

afterEach(() => {
  restore?.();
  restore = undefined;
  goPrefix.armed = false;
});

describe("App", () => {
  it("shows the shortcut sheet on ? and closes it on Escape", async () => {
    render(<App />);
    fireEvent.keyDown(document, { key: "?" });
    const sheet = await screen.findByRole("dialog", { name: "Keyboard shortcuts" });
    expect(sheet).toBeInTheDocument();
    expect(screen.getByText("Grade the card you are reviewing")).toBeInTheDocument();

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
  });

  it("navigates with the g prefix", async () => {
    render(<App />);
    await screen.findByRole("heading", { level: 1, name: "Practice" });
    fireEvent.keyDown(document, { key: "g" });
    fireEvent.keyDown(document, { key: "r" });
    expect(
      await screen.findByRole("heading", { level: 1, name: "Review" }),
    ).toBeInTheDocument();
    expect(goPrefix.armed).toBe(false);
  });

  it("drops the prefix when the destination is nonsense", async () => {
    render(<App />);
    await screen.findByRole("heading", { level: 1, name: "Practice" });
    fireEvent.keyDown(document, { key: "g" });
    fireEvent.keyDown(document, { key: "q" });
    expect(goPrefix.armed).toBe(false);
    expect(
      screen.getByRole("heading", { level: 1, name: "Practice" }),
    ).toBeInTheDocument();
  });

  it("keeps exactly one toast region for the whole app", async () => {
    render(<App />);
    await screen.findByRole("heading", { level: 1, name: "Practice" });
    expect(screen.getAllByRole("status")).toHaveLength(1);
  });
});
