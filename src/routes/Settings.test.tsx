import { fireEvent, render, screen, waitFor } from "@testing-library/preact";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, makeUpdate, type MockIpc } from "../ipc/mock";
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
  it("shows the value the backend saved, not the one typed", async () => {
    // Number inputs do not stop a typed out-of-range value; the backend clamps
    // it. The screen must show what was stored, or it disagrees with the next
    // launch.
    const mock = createMockIpc();
    mount(mock);
    const input = (await screen.findByLabelText("New cards per day")) as HTMLInputElement;
    fireEvent.input(input, { target: { value: "99999" } });
    fireEvent.change(input, { target: { value: "99999" } });
    await waitFor(() =>
      expect(mock.calls.some((c) => c.name === "setPreferences")).toBe(true),
    );
    await waitFor(() => expect(input.value).toBe("9999"));
  });

  it("sends a retention value inside the allowed range", async () => {
    const mock = createMockIpc({ retention: 0.9 });
    mount(mock);
    const slider = await screen.findByLabelText(/Remember about/);
    fireEvent.input(slider, { target: { value: "0.95" } });
    fireEvent.change(slider, { target: { value: "0.95" } });
    await waitFor(() =>
      expect(mock.calls.find((c) => c.name === "setRetention")?.args).toEqual([0.95]),
    );
    const [sent] = mock.calls.find((c) => c.name === "setRetention")!.args as [number];
    expect(sent).toBeGreaterThanOrEqual(0.7);
    expect(sent).toBeLessThanOrEqual(0.98);
  });

  it("saves a number only when the edit is committed", async () => {
    const mock = createMockIpc();
    mount(mock);
    const input = (await screen.findByLabelText("Reviews per day")) as HTMLInputElement;
    const saves = () => mock.calls.filter((c) => c.name === "setPreferences");
    fireEvent.input(input, { target: { value: "1" } });
    fireEvent.input(input, { target: { value: "15" } });
    fireEvent.input(input, { target: { value: "150" } });
    expect(saves()).toHaveLength(0);
    fireEvent.change(input, { target: { value: "150" } });
    await waitFor(() => expect(saves()).toHaveLength(1));
    expect(saves()[0]?.args[0]).toMatchObject({ review_per_day: 150 });
  });

  it("puts back the stored value when a number field is cleared", async () => {
    const mock = createMockIpc();
    mount(mock);
    const input = (await screen.findByLabelText("New cards per day")) as HTMLInputElement;
    const before = input.value;
    fireEvent.input(input, { target: { value: "" } });
    fireEvent.change(input, { target: { value: "" } });
    await waitFor(() => expect(input.value).toBe(before));
    expect(mock.calls.some((c) => c.name === "setPreferences")).toBe(false);
  });

  it("saves retention once, when the slider is let go", async () => {
    const mock = createMockIpc({ retention: 0.9 });
    mount(mock);
    const slider = await screen.findByLabelText(/Remember about/);
    for (const v of ["0.91", "0.92", "0.93"]) fireEvent.input(slider, { target: { value: v } });
    expect(mock.calls.some((c) => c.name === "setRetention")).toBe(false);
    expect(await screen.findByLabelText(/Remember about 93 out of 100/)).toBeInTheDocument();
    fireEvent.change(slider, { target: { value: "0.93" } });
    await waitFor(() =>
      expect(mock.calls.filter((c) => c.name === "setRetention").map((c) => c.args)).toEqual([[0.93]]),
    );
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

  it("explains a busy voice instead of showing the raw error", async () => {
    const mock = createMockIpc({ voices: [{ id: "v1", label: "Voice one" }] });
    vi.spyOn(mock, "synthesizeSpeech").mockRejectedValue("TTS_BUSY: queue full");
    mount(mock);
    const button = await screen.findByRole("button", { name: "Test voice" });
    await waitFor(() => expect(button).toBeEnabled());
    fireEvent.click(button);
    expect(await screen.findByText(/Still speaking/)).toBeInTheDocument();
    expect(screen.queryByText(/TTS_BUSY/)).not.toBeInTheDocument();
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

  it("saves the update opt-in, which starts off", async () => {
    const mock = createMockIpc();
    mount(mock);
    const box = (await screen.findByLabelText(
      "Check for a new version each time the app starts",
    )) as HTMLInputElement;
    expect(box.checked).toBe(false);
    fireEvent.click(box);
    await waitFor(() => {
      const saved = mock.calls.find((c) => c.name === "setPreferences");
      expect((saved?.args[0] as { check_updates: boolean }).check_updates).toBe(true);
    });
    await waitFor(() => expect(box.checked).toBe(true));
  });

  it("checks on request, which needs no opt-in, and says when this is the newest", async () => {
    const mock = createMockIpc();
    mount(mock);
    fireEvent.click(await screen.findByRole("button", { name: "Check now" }));
    expect(await screen.findByText("You have the newest version (0.1.0).")).toBeInTheDocument();
    expect(mock.calls.find((c) => c.name === "checkForUpdate")?.args).toEqual(["manual"]);
  });

  it("installs an update, streaming progress, then offers a restart", async () => {
    const mock = createMockIpc({ update: { available: makeUpdate() } });
    mount(mock);
    fireEvent.click(await screen.findByRole("button", { name: "Check now" }));
    expect(
      await screen.findByText("Version 0.2.0 is available. You have 0.1.0."),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Download and install" }));
    expect(
      await screen.findByText("Installed. Restart to use version 0.2.0."),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Restart now" }));
    await waitFor(() => expect(mock.calls.some((c) => c.name === "restartApp")).toBe(true));
    expect(screen.queryByText(/Could not/)).toBeNull();
  });

  it("only tells a package install where to get the update", async () => {
    mount(createMockIpc({ update: { mode: "notify", bundle: "deb", available: makeUpdate() } }));
    fireEvent.click(await screen.findByRole("button", { name: "Check now" }));
    expect(await screen.findByText(/your package manager owns it/)).toBeInTheDocument();
    expect(
      screen.getByText("https://github.com/lubdhak7414/offline-language-practice/releases"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Download and install" })).toBeNull();
    expect(screen.getByText("Installed as: deb")).toBeInTheDocument();
  });

  it("shows a failed check instead of claiming there is nothing new", async () => {
    mount(createMockIpc({ fail: { checkForUpdate: "update check failed: offline" } }));
    fireEvent.click(await screen.findByRole("button", { name: "Check now" }));
    expect(
      await screen.findByText("Could not check for updates: update check failed: offline"),
    ).toBeInTheDocument();
    expect(screen.queryByText(/newest version/)).toBeNull();
  });

  it("names a development build in Diagnostics", async () => {
    mount(createMockIpc({ update: { mode: "notify", bundle: null } }));
    expect(await screen.findByText("Installed as: development build")).toBeInTheDocument();
  });

  it("surfaces a restart notice after restoring", async () => {
    mount(createMockIpc({ pickOpenPath: "/tmp/backup.sqlite" }));
    await screen.findByText(/Voice/);
    fireEvent.click(screen.getByRole("button", { name: "Restore database" }));
    expect(await screen.findByText(/[Rr]estart required/)).toBeInTheDocument();
  });
});
