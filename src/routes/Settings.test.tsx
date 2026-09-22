import { fireEvent, render, screen, waitFor } from "@testing-library/preact";
import { afterEach, describe, expect, it } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, type MockIpc } from "../ipc/mock";
import { Settings } from "./Settings";

let restore: (() => void) | undefined;

function mount(mock: MockIpc) {
  restore = setIpc(mock);
  return render(<Settings announce={() => {}} />);
}

afterEach(() => {
  restore?.();
  restore = undefined;
  document.documentElement.removeAttribute("data-theme");
});

describe("Settings", () => {
  it("sends a retention value inside the allowed range", async () => {
    const mock = createMockIpc({ retention: 0.9 });
    mount(mock);
    const slider = await screen.findByLabelText(/Remember about/);
    fireEvent.input(slider, { target: { value: "0.95" } });
    await waitFor(() =>
      expect(mock.calls.find((c) => c.name === "setRetention")?.args).toEqual([0.95]),
    );
    const [sent] = mock.calls.find((c) => c.name === "setRetention")!.args as [number];
    expect(sent).toBeGreaterThanOrEqual(0.7);
    expect(sent).toBeLessThanOrEqual(0.98);
  });

  it("sets data-theme when a theme radio is picked", async () => {
    mount(createMockIpc());
    const dark = await screen.findByLabelText("dark");
    fireEvent.click(dark);
    await waitFor(() =>
      expect(document.documentElement.getAttribute("data-theme")).toBe("dark"),
    );
  });

  it("disables the voice test button and explains why with no voice installed", async () => {
    mount(createMockIpc({ voices: [] }));
    const notice = await screen.findByText(/No voice is installed/);
    expect(notice).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Test voice" })).toBeDisabled();
  });

  it("gates the optimizer on the real training-data numbers", async () => {
    mount(
      createMockIpc({
        stats: {
          distinct_cards: 2,
          total_reviews: 4,
          trainable_cards: 1,
          train_items: 4,
          min_train_items: 32,
          min_trainable_cards: 8,
        },
      }),
    );
    expect(await screen.findByText(/4 of 32 reviews needed/)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Optimize scheduling parameters" }),
    ).toBeDisabled();
  });

  it("surfaces a restart notice after restoring", async () => {
    mount(createMockIpc({ pickOpenPath: "/tmp/backup.sqlite" }));
    await screen.findByText(/Voice/);
    fireEvent.click(screen.getByRole("button", { name: "Restore database" }));
    expect(await screen.findByText(/[Rr]estart required/)).toBeInTheDocument();
  });
});
