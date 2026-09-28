import { fireEvent, render, screen, waitFor, within } from "@testing-library/preact";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, type MockIpc } from "../ipc/mock";
import type { CardRow } from "../ipc/types";
import { Decks } from "./Decks";

let restore: (() => void) | undefined;

function mount(mock: MockIpc) {
  restore = setIpc(mock);
  return render(<Decks announce={() => {}} />);
}

afterEach(() => {
  restore?.();
  restore = undefined;
});

describe("Decks", () => {
  it("ignores a slow reply for a deck the user has already left", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc({
      decks: [{ id: "default", name: "Default" }, { id: "travel", name: "Travel" }],
      cards: [
        { id: "c1", deck_id: "default", front: "from-default", back: "B" },
        { id: "c2", deck_id: "travel", front: "from-travel", back: "B" },
      ],
    });
    // Every listCards call waits until the test releases it, so replies can
    // be delivered out of order.
    const pending: Array<{ deckId: string | undefined; release: () => Promise<void> }> = [];
    const slow: MockIpc = {
      ...mock,
      listCards: (deckId?: string) =>
        new Promise<CardRow[]>((resolve) => {
          pending.push({ deckId, release: async () => resolve(await mock.listCards(deckId)) });
        }),
    };
    mount(slow);

    await user.click(await screen.findByRole("button", { name: /^Default/ }));
    await user.click(screen.getByRole("button", { name: /^Travel/ }));
    await waitFor(() => expect(pending.map((p) => p.deckId)).toEqual([undefined, "default", "travel"]));

    // Travel (the current selection) answers first; Default arrives late.
    await pending[2]!.release();
    expect(await screen.findByText("from-travel")).toBeInTheDocument();
    await pending[1]!.release();
    await pending[0]!.release();

    await waitFor(() => expect(screen.getByText("from-travel")).toBeInTheDocument());
    expect(screen.queryByText("from-default")).not.toBeInTheDocument();
  });

  it("lists decks with their counts", async () => {
    mount(
      createMockIpc({
        decks: [{ id: "default", name: "Default" }],
        cards: [{ id: "c1", deck_id: "default", front: "F", back: "B" }],
      }),
    );
    expect(await screen.findByText(/Default/)).toBeInTheDocument();
    expect(screen.getByText(/1 cards/)).toBeInTheDocument();
  });

  it("asks before deleting a deck, and sends the chosen mode", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc({
      decks: [{ id: "default", name: "Default" }, { id: "travel", name: "Travel" }],
      cards: [{ id: "c1", deck_id: "travel", front: "F", back: "B" }],
    });
    mount(mock);
    await screen.findByText(/Travel/);

    // Deleting must not fire before the confirmation appears.
    await user.click(screen.getAllByRole("button", { name: "Delete" })[0]!);
    expect(mock.calls.some((c) => c.name === "deleteDeck")).toBe(false);
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();

    await user.click(screen.getByLabelText("Delete its cards too"));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));

    await waitFor(() =>
      expect(mock.calls.find((c) => c.name === "deleteDeck")?.args).toEqual([
        "default",
        "delete",
      ]),
    );
  });

  it("fires a destructive click exactly once even when double-clicked", async () => {
    const mock = createMockIpc({
      decks: [{ id: "default", name: "Default" }],
      cards: [],
    });
    mount(mock);
    await screen.findByText(/Default/);
    const deckList = document.querySelector(".deck-list") as HTMLElement;
    await userEvent.setup().click(within(deckList).getByRole("button", { name: "Delete" }));
    await screen.findByRole("alertdialog");
    const confirm = screen.getByRole("button", { name: "Confirm delete" });
    fireEvent.click(confirm);
    fireEvent.click(confirm);
    await waitFor(() =>
      expect(mock.calls.filter((c) => c.name === "deleteDeck")).toHaveLength(1),
    );
  });

  it("exports through the save dialog, and does nothing on a cancelled picker", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc({ pickSavePath: null });
    mount(mock);
    await screen.findByText(/Default/);
    await user.click(screen.getByRole("button", { name: "Export" }));
    await waitFor(() => expect(mock.calls.some((c) => c.name === "pickSavePath")).toBe(true));
    expect(mock.calls.some((c) => c.name === "exportData")).toBe(false);
  });

  it("exports to the chosen path when the picker returns one", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc({ pickSavePath: "/tmp/out.json" });
    mount(mock);
    await screen.findByText(/Default/);
    await user.click(screen.getByRole("button", { name: "Export" }));
    await waitFor(() =>
      expect(mock.calls.find((c) => c.name === "exportData")?.args).toEqual([
        { path: "/tmp/out.json", format: "json" },
      ]),
    );
  });

  describe("filtering", () => {
    const cards = [
      { id: "a", deck_id: "default", front: "Good morning", back: "Greeting", tags: ["greetings"] },
      { id: "b", deck_id: "default", front: "Where is the station?", back: "Directions", tags: ["travel"] },
      { id: "c", deck_id: "default", front: "See you later", back: "Goodbye", tags: ["greetings"] },
    ];
    const fronts = () =>
      within(screen.getByRole("table")).getAllByRole("row").slice(1).map((r) => (r as HTMLTableRowElement).cells[1]?.textContent);

    it("narrows the list by text and by tag", async () => {
      const user = userEvent.setup();
      mount(createMockIpc({ cards }));
      await screen.findByText("Good morning");
      await user.type(screen.getByRole("searchbox", { name: "Search cards" }), "GOOD");
      expect(fronts()).toEqual(["Good morning", "See you later"]);
      expect(screen.getByText("2 of 3 cards")).toBeInTheDocument();

      await user.clear(screen.getByRole("searchbox", { name: "Search cards" }));
      await user.selectOptions(screen.getByRole("combobox", { name: "Tag" }), "travel");
      expect(fronts()).toEqual(["Where is the station?"]);

      await user.type(screen.getByRole("searchbox", { name: "Search cards" }), "zzz");
      expect(screen.getByText("No cards match.")).toBeInTheDocument();
    });

    it("never deletes a checked card the filter is hiding", async () => {
      const user = userEvent.setup();
      const mock = createMockIpc({ cards });
      mount(mock);
      await screen.findByText("Good morning");
      await user.click(screen.getByRole("checkbox", { name: "Select Good morning" }));
      await user.click(screen.getByRole("checkbox", { name: "Select Where is the station?" }));
      await user.selectOptions(screen.getByRole("combobox", { name: "Tag" }), "travel");
      await user.click(screen.getByRole("button", { name: "Delete 1 selected" }));
      await waitFor(() =>
        expect(mock.calls.filter((c) => c.name === "deleteCard").map((c) => c.args)).toEqual([["b"]]),
      );
    });
  });

  describe("daily limits", () => {
    it("shows and saves one deck's limits", async () => {
      const user = userEvent.setup();
      const mock = createMockIpc();
      mount(mock);
      await user.click(await screen.findByRole("button", { name: /^Travel/ }));
      const form = await screen.findByRole("form", { name: "Daily limits for this deck" });
      const newCards = within(form).getByRole("spinbutton", { name: /New cards per day/ });
      const reviews = within(form).getByRole("spinbutton", { name: /Reviews per day/ });
      expect(mock.calls.find((c) => c.name === "getDailyLimits")?.args).toEqual(["travel"]);

      await user.clear(newCards);
      await user.type(newCards, "5");
      await user.clear(reviews);
      await user.type(reviews, "80");
      await user.click(within(form).getByRole("button", { name: "Save limits" }));
      await waitFor(() =>
        expect(mock.calls.find((c) => c.name === "setDailyLimits")?.args[0]).toEqual({
          deckId: "travel",
          newPerDay: 5,
          reviewPerDay: 80,
        }),
      );
    });

    it("never saves a cleared or out-of-range field", async () => {
      const user = userEvent.setup();
      const mock = createMockIpc();
      mount(mock);
      await user.click(await screen.findByRole("button", { name: /^Travel/ }));
      const form = await screen.findByRole("form", { name: "Daily limits for this deck" });
      const newCards = within(form).getByRole("spinbutton", { name: /New cards per day/ });
      await user.clear(newCards);
      expect(within(form).getByRole("button", { name: "Save limits" })).toBeDisabled();
      expect(within(form).getByText(/whole numbers from 0 to 9999/)).toBeInTheDocument();
      await user.type(newCards, "10000");
      expect(within(form).getByRole("button", { name: "Save limits" })).toBeDisabled();
      await user.keyboard("{Enter}");
      expect(mock.calls.some((c) => c.name === "setDailyLimits")).toBe(false);
    });

    it("has no per-deck limits for the all-cards view", async () => {
      mount(createMockIpc());
      await screen.findByRole("button", { name: /^Travel/ });
      expect(screen.queryByRole("form", { name: "Daily limits for this deck" })).not.toBeInTheDocument();
    });
  });
});
