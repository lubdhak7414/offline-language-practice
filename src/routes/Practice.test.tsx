import { render, screen, waitFor } from "@testing-library/preact";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, makePrompt, type MockIpc } from "../ipc/mock";
import { Practice } from "./Practice";

// jsdom has no microphone. A recorder that hands back one second of silence
// is enough to drive Practice through scoring into its feedback view.
vi.mock("../app/recorder", () => {
  let recording = false;
  return {
    MAX_RECORDING_MS: 120_000,
    createRecorder: () => ({
      start: async () => {
        recording = true;
      },
      stop: async () => {
        recording = false;
        return { pcm: new Uint8Array(64_000), durationMs: 1000, peak: 0.3 };
      },
      cancel: () => {
        recording = false;
      },
      isRecording: () => recording,
    }),
  };
});

let restore: (() => void) | undefined;

function mount(mock: MockIpc) {
  restore = setIpc(mock);
  return render(<Practice announce={() => {}} />);
}

beforeEach(() => {
  // The recorder needs a microphone and an AudioContext; neither exists in
  // jsdom. Tests that press Record stub them explicitly.
  vi.restoreAllMocks();
});

afterEach(() => {
  restore?.();
  restore = undefined;
});

describe("Practice", () => {
  it("opens a session and shows the first prompt", async () => {
    const mock = createMockIpc();
    mount(mock);
    expect(await screen.findByText("Reply to a greeting:")).toBeInTheDocument();
    expect(screen.getByText("Hi, good to see you again.")).toBeInTheDocument();
    expect(mock.calls.some((c) => c.name === "startSession")).toBe(true);
  });

  it("labels a read-aloud prompt as scoreable", async () => {
    mount(createMockIpc());
    expect(await screen.findByText("Read aloud")).toBeInTheDocument();
  });

  it("labels an open-ended prompt as free speaking", async () => {
    mount(
      createMockIpc({
        prompts: [makePrompt({ target_text: null, prompt_text: "Tell me about yourself." })],
      }),
    );
    expect(await screen.findByText("Free speaking")).toBeInTheDocument();
  });

  it("asks for a new prompt when Skip is pressed", async () => {
    const mock = createMockIpc({
      prompts: [makePrompt({ id: "p1" }), makePrompt({ id: "p2", prompt_text: "Second" })],
    });
    mount(mock);
    await screen.findByText("Reply to a greeting:");
    const before = mock.calls.filter((c) => c.name === "nextPrompt").length;
    await userEvent.click(screen.getByRole("button", { name: "Skip" }));
    await waitFor(() =>
      expect(mock.calls.filter((c) => c.name === "nextPrompt").length).toBe(before + 1),
    );
  });

  it("keeps the newest prompt when two Skips resolve out of order", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc({
      prompts: [
        makePrompt({ id: "p1" }),
        makePrompt({ id: "p2", prompt_text: "Stale prompt", target_text: null }),
        makePrompt({ id: "p3", prompt_text: "Fresh prompt", target_text: null }),
      ],
    });
    // The first prompt loads normally. Each later reply is decided when it is
    // *requested* — so the first Skip's reply is "Stale", the second's "Fresh" —
    // but is only delivered when the test releases it.
    const held: Array<() => void> = [];
    let served = 0;
    const slow: MockIpc = {
      ...mock,
      nextPrompt: (args) => {
        served += 1;
        const reply = mock.nextPrompt(args);
        if (served === 1) return reply;
        return new Promise((resolve) => {
          held.push(() => resolve(reply));
        });
      },
    };
    mount(slow);
    await screen.findByText("Reply to a greeting:");

    await user.click(screen.getByRole("button", { name: "Skip" }));
    await user.click(screen.getByRole("button", { name: "Skip" }));
    await waitFor(() => expect(held).toHaveLength(2));

    // The newer request answers first; the older one's reply arrives late.
    held[1]!();
    expect(await screen.findByText("Fresh prompt")).toBeInTheDocument();
    held[0]!();

    // Give the late reply every chance to land before checking it did not.
    await new Promise((r) => setTimeout(r, 0));
    await waitFor(() => expect(screen.getByText("Fresh prompt")).toBeInTheDocument());
    expect(screen.queryByText("Stale prompt")).not.toBeInTheDocument();
  });

  it("says so when pronunciation fell back to word matching", async () => {
    const user = userEvent.setup();
    // What the backend returns when acoustic scoring refuses: no per-word
    // acoustic detail, the word-alignment accuracy standing in, and fluency
    // measured from the audio envelope.
    const mock = createMockIpc({
      report: {
        pron_method: "text",
        pron: null,
        pron_overall: 100,
        fluency: {
          wpm: 120,
          articulation_wpm: 140,
          longest_pause_ms: 0,
          pause_count: 0,
          pauses: [],
          filler_count: 0,
          like_count: 0,
          hesitation_count: 0,
          speaking_ms: 1800,
          method: "energy",
          score: 88,
        },
      },
    });
    mount(mock);
    await screen.findByText("Reply to a greeting:");

    await user.click(screen.getByRole("button", { name: /^Record/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));

    expect(await screen.findByText(/word-by-word matching/)).toBeInTheDocument();
    expect(mock.calls.find((c) => c.name === "scoreAttempt")?.args[0]).toMatchObject({
      targetText: "Hi, good to see you again.",
    });
  });

  it("does not claim a fallback when acoustic scoring ran", async () => {
    const user = userEvent.setup();
    mount(createMockIpc());
    await screen.findByText("Reply to a greeting:");
    await user.click(screen.getByRole("button", { name: /^Record/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));
    expect(await screen.findByText(/^Overall/)).toBeInTheDocument();
    expect(screen.queryByText(/word-by-word matching/)).not.toBeInTheDocument();
  });

  it("switches the session when the category changes", async () => {
    const mock = createMockIpc();
    mount(mock);
    await screen.findByText("Reply to a greeting:");
    await userEvent.click(screen.getByRole("button", { name: "Job interview" }));
    await waitFor(() => {
      const kinds = mock.calls.filter((c) => c.name === "startSession").map((c) => c.args[0]);
      expect(kinds).toContain("interview");
    });
  });

  it("blocks recording when the speech model is missing", async () => {
    // Letting someone record for a minute and then telling them the model is
    // missing was a reachable path; this is the guard against it.
    mount(
      createMockIpc({
        modelStatus: { asr_model: false, asr_vocab: false, tts_voice: true },
      }),
    );
    expect(
      await screen.findByText(/speech model is not installed yet/i),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /^Record/ })).toBeDisabled(),
    );
  });

  it("keeps recording available when the model probe fails", async () => {
    // Unknown status must not lock a working install out.
    mount(createMockIpc({ fail: { modelStatus: new Error("probe blew up") } }));
    await screen.findByText("Reply to a greeting:");
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /^Record/ })).toBeEnabled(),
    );
  });

  it("surfaces a failure to load a prompt instead of spinning", async () => {
    mount(createMockIpc({ fail: { nextPrompt: new Error("db is gone") } }));
    expect(await screen.findByText(/db is gone/)).toBeInTheDocument();
  });
});
