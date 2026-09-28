import { render, screen } from "@testing-library/preact";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc } from "../ipc/mock";
import { Practice } from "./Practice";

const peak = vi.hoisted(() => ({ value: 0.3 }));
const started = vi.hoisted(() => [] as Array<string | undefined>);

vi.mock("../app/recorder", () => {
  let recording = false;
  return {
    MAX_RECORDING_MS: 120_000,
    createRecorder: () => ({
      start: async (_onLevel?: unknown, deviceId?: string) => {
        started.push(deviceId);
        recording = true;
      },
      stop: async () => {
        recording = false;
        return { pcm: new Uint8Array(64_000), durationMs: 1000, peak: peak.value };
      },
      cancel: () => {
        recording = false;
      },
      isRecording: () => recording,
    }),
  };
});

let restore: (() => void) | undefined;

afterEach(() => {
  restore?.();
  restore = undefined;
  peak.value = 0.3;
  started.length = 0;
});

async function recordOnce(mock: ReturnType<typeof createMockIpc>) {
  const user = userEvent.setup();
  restore = setIpc(mock);
  render(<Practice announce={() => {}} />);
  await screen.findByText("Reply to a greeting:");
  await user.click(screen.getByRole("button", { name: /^Record/ }));
  await user.click(await screen.findByRole("button", { name: /^Stop/ }));
  await screen.findByText(/^Overall/);
}

describe("Practice microphone", () => {
  it("warns when the recording was quiet", async () => {
    peak.value = 0.03;
    await recordOnce(createMockIpc());
    expect(screen.getByText(/That was quiet/)).toBeInTheDocument();
  });

  it("warns when the recording was cut off", async () => {
    peak.value = 1;
    await recordOnce(createMockIpc());
    expect(screen.getByText(/too loud/)).toBeInTheDocument();
  });

  it("says nothing about a healthy level", async () => {
    await recordOnce(createMockIpc());
    expect(screen.queryByText(/That was quiet|too loud|No sound/)).not.toBeInTheDocument();
  });

  it("records from the microphone chosen in Settings", async () => {
    await recordOnce(createMockIpc({ preferences: { mic_device_id: "usb-mic" } }));
    expect(started).toEqual(["usb-mic"]);
  });
});
