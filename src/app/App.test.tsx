import { act, fireEvent, render, screen, waitFor } from "@testing-library/preact";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, makeUpdate } from "../ipc/mock";
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
  it("opens on onboarding when the database says it has never been run", async () => {
    // The whole point of the flag: a fresh install must not land on
    // Practice, where every button needs models that are not there yet.
    restore?.();
    restore = setIpc(createMockIpc({ preferences: { onboarded: false } }));
    render(<App />);
    expect(await screen.findByRole("heading", { name: "Welcome" })).toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "Main" })).toBeNull();
  });

  it("opens on the app proper once onboarding has been completed", async () => {
    render(<App />);
    expect(
      await screen.findByRole("navigation", { name: "Main" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Welcome" })).toBeNull();
  });

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

  describe("startup update check", () => {
    // Only timers are faked: promises must still settle for the mock to answer.
    beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] }));
    afterEach(() => vi.useRealTimers());

    it("never runs while the preference is off", async () => {
      // Guard test: this passes before the feature too, and must keep passing.
      restore?.();
      const mock = createMockIpc({ update: { available: makeUpdate() } });
      restore = setIpc(mock);
      render(<App />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(10_000);
      });
      expect(mock.calls.some((c) => c.name === "checkForUpdate")).toBe(false);
    });

    it("runs once, a few seconds after launch, and announces what it found", async () => {
      restore?.();
      const mock = createMockIpc({
        preferences: { check_updates: true },
        update: { available: makeUpdate() },
      });
      restore = setIpc(mock);
      render(<App />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(1_000);
      });
      expect(mock.calls.some((c) => c.name === "checkForUpdate")).toBe(false);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(4_500);
      });
      expect(screen.getByRole("status")).toHaveTextContent(
        "Version 0.2.0 is available. Open Settings to install it.",
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(30_000);
      });
      const checks = mock.calls.filter((c) => c.name === "checkForUpdate");
      expect(checks.map((c) => c.args)).toEqual([["startup"]]);
    });
  });

  it("keeps exactly one toast region for the whole app", async () => {
    render(<App />);
    await screen.findByRole("heading", { level: 1, name: "Practice" });
    expect(screen.getAllByRole("status")).toHaveLength(1);
  });
});
