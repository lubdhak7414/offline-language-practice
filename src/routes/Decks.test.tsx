import { fireEvent, render, screen, waitFor, within } from "@testing-library/preact";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, type MockIpc } from "../ipc/mock";
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
});
