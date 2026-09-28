import { fireEvent, render, screen, waitFor } from "@testing-library/preact";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc } from "../ipc/mock";
import { Settings } from "./Settings";

const state = vi.hoisted(() => ({
  peak: 0.3,
  started: [] as Array<string | undefined>,
  fail: null as unknown,
  mics: [
    { id: "built-in", label: "Built-in Microphone" },
    { id: "usb", label: "USB headset" },
  ],
}));

vi.mock("../app/devices", () => ({ listMics: async () => state.mics }));
vi.mock("../app/recorder", () => ({
  MAX_RECORDING_MS: 120_000,
  createRecorder: () => ({
    start: async (_l?: unknown, id?: string) => {
      if (state.fail) throw state.fail;
      state.started.push(id);
    },
    stop: async () => ({ pcm: new Uint8Array(8), durationMs: 2000, peak: state.peak }),
    cancel: () => {},
    isRecording: () => false,
  }),
}));

let restore: (() => void) | undefined;

afterEach(() => {
  restore?.();
  restore = undefined;
  state.peak = 0.3;
  state.started.length = 0;
  state.fail = null;
  vi.useRealTimers();
});

function mount(mock = createMockIpc()) {
  restore = setIpc(mock);
  render(<Settings announce={() => {}} />);
  return mock;
}

describe("Settings microphone", () => {
  it("lists the microphones and saves the choice", async () => {
    const mock = mount();
    const select = (await screen.findByLabelText("Microphone")) as HTMLSelectElement;
    await screen.findByRole("option", { name: "USB headset" });
    fireEvent.change(select, { target: { value: "usb" } });
    await waitFor(() => expect(select.value).toBe("usb"));
    const saves = mock.calls.filter((c) => c.name === "setPreferences");
    expect((saves[saves.length - 1]?.args[0] as { mic_device_id: string }).mic_device_id).toBe("usb");
  });

  it("shows a saved microphone that is no longer connected instead of silently resetting", async () => {
    mount(createMockIpc({ preferences: { mic_device_id: "gone" } }));
    expect(await screen.findByRole("option", { name: "Saved microphone (not connected)" })).toBeInTheDocument();
  });

  it("tests the chosen microphone and reports a quiet one", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    state.peak = 0.03;
    mount(createMockIpc({ preferences: { mic_device_id: "usb" } }));
    fireEvent.click(await screen.findByRole("button", { name: "Test microphone" }));
    await vi.advanceTimersByTimeAsync(2100);
    expect(await screen.findByText(/That was quiet/)).toBeInTheDocument();
    expect(state.started).toEqual(["usb"]);
  });

  it("says so when the level is fine", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Test microphone" }));
    await vi.advanceTimersByTimeAsync(2100);
    expect(await screen.findByText("That sounds good.")).toBeInTheDocument();
  });

  it("explains a blocked microphone", async () => {
    state.fail = Object.assign(new Error("x"), { name: "NotAllowedError" });
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Test microphone" }));
    expect(await screen.findByText(/Microphone access was blocked/)).toBeInTheDocument();
  });
});
