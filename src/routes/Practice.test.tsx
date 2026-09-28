import { render, screen, waitFor } from "@testing-library/preact";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, makePrompt, type MockIpc } from "../ipc/mock";
import { expectNoA11yViolations } from "../test/axe";
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

  it("opens on interview prompts when the saved goal is interview", async () => {
    const mock = createMockIpc({
      preferences: { goal: "interview" },
      prompts: [
        makePrompt({ id: "i1", category: "interview", prompt_text: "Why this role?", target_text: null }),
      ],
    });
    mount(mock);
    await screen.findByText("Why this role?");
    const start = mock.calls.filter((c) => c.name === "startSession");
    expect(start.map((c) => c.args[0])).toEqual(["interview"]);
    expect(screen.getByRole("button", { name: "Job interview" })).toHaveAttribute("aria-pressed", "true");
  });

  it("opens on conversation for the default goal, with one session", async () => {
    const mock = createMockIpc();
    mount(mock);
    await screen.findByText("Reply to a greeting:");
    const start = mock.calls.filter((c) => c.name === "startSession");
    expect(start.map((c) => c.args[0])).toEqual(["conversation"]);
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

  it("counts today's attempts toward the daily goal, and hides it when off", async () => {
    const user = userEvent.setup();
    mount(createMockIpc({ preferences: { practice_goal_attempts: 2 } }));
    expect(await screen.findByText("0 of 2 today")).toBeInTheDocument();
    // Record stays disabled until the prompt has loaded; the goal can win
    // that race, and a click on a disabled button does nothing.
    await screen.findByText("Reply to a greeting:");
    await user.click(screen.getByRole("button", { name: /^Record/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));
    expect(await screen.findByText("1 of 2 today")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /^Try again/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));
    expect(await screen.findByText("Daily goal reached: 2 of 2 today")).toBeInTheDocument();
  });

  it("shows no daily goal when it is set to 0", async () => {
    mount(createMockIpc({ preferences: { practice_goal_attempts: 0 } }));
    await screen.findByText("Reply to a greeting:");
    expect(screen.queryByText(/of \d+ today/)).not.toBeInTheDocument();
  });

  it("compares a retry with the last try on the same prompt, not the first try", async () => {
    const user = userEvent.setup();
    mount(createMockIpc());
    await screen.findByText("Reply to a greeting:");
    await user.click(screen.getByRole("button", { name: /^Record/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));
    await screen.findByText(/^Overall/);
    expect(screen.queryByText(/your last try/)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /^Try again/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));
    expect(await screen.findByText("About the same as your last try (100).")).toBeInTheDocument();
  });

  it("gives an open interview answer a target length and a length note", async () => {
    const user = userEvent.setup();
    mount(
      createMockIpc({
        preferences: { goal: "interview" },
        prompts: [
          makePrompt({
            id: "i1",
            category: "interview",
            level: 2,
            prompt_text: "Why this role?",
            target_text: null,
          }),
        ],
      }),
    );
    await screen.findByText("Why this role?");
    expect(screen.getByText("Aim for about 60 seconds.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /^Record/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));
    // The mock's attempt lasts two seconds: well short of a minute.
    expect(await screen.findByText(/^Short for this question \(2 s\)/)).toBeInTheDocument();
  });

  it("gives no length advice on a read-aloud prompt", async () => {
    const user = userEvent.setup();
    mount(createMockIpc());
    await screen.findByText("Reply to a greeting:");
    expect(screen.queryByText(/^Aim for about/)).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /^Record/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));
    await screen.findByText(/^Overall/);
    expect(screen.queryByText(/for this question/)).not.toBeInTheDocument();
  });

  it("shows a session summary from the second attempt, not the first", async () => {
    const user = userEvent.setup();
    mount(createMockIpc());
    await screen.findByText("Reply to a greeting:");
    await user.click(screen.getByRole("button", { name: /^Record/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));
    await screen.findByText(/^Overall/);
    expect(screen.queryByText(/^This session/)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /^Try again/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));
    expect(await screen.findByText(/^This session: 2 attempts/)).toBeInTheDocument();
  });

  it("ends the session it opened when the category changes and on unmount", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc();
    const view = mount(mock);
    await screen.findByText("Reply to a greeting:");
    const ended = () => mock.calls.filter((c) => c.name === "endSession").length;
    expect(ended()).toBe(0);
    await user.click(screen.getByRole("button", { name: "Job interview" }));
    await waitFor(() => expect(ended()).toBe(1));
    view.unmount();
    await waitFor(() => expect(ended()).toBe(2));
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

  it("asks for prompts at the picked level, in the same session", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc({
      prompts: [
        makePrompt({ id: "l1", level: 1, prompt_text: "Easy one" }),
        makePrompt({ id: "l3", level: 3, prompt_text: "Hard one" }),
      ],
    });
    mount(mock);
    await screen.findByText("Easy one");
    await user.click(screen.getByRole("button", { name: "Level 3" }));
    expect(await screen.findByText("Hard one")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Level 3" })).toHaveAttribute("aria-pressed", "true");
    const nexts = mock.calls.filter((c) => c.name === "nextPrompt");
    expect(nexts[nexts.length - 1]?.args[0]).toMatchObject({ level: 3 });
    expect(mock.calls.filter((c) => c.name === "startSession")).toHaveLength(1);

    // Skip keeps the level.
    await user.click(screen.getByRole("button", { name: "Skip" }));
    const after = mock.calls.filter((c) => c.name === "nextPrompt");
    expect(after[after.length - 1]?.args[0]).toMatchObject({ level: 3 });

    await user.click(screen.getByRole("button", { name: "Any level" }));
    const any = mock.calls.filter((c) => c.name === "nextPrompt");
    expect(any[any.length - 1]?.args[0]).not.toHaveProperty("level");
  });

  it("says so when a level has no prompts instead of loading forever", async () => {
    const user = userEvent.setup();
    mount(createMockIpc({ prompts: [makePrompt({ level: 1 })] }));
    await screen.findByText("Reply to a greeting:");
    await user.click(screen.getByRole("button", { name: "Level 2" }));
    expect(await screen.findByText(/No prompts here at level 2/)).toBeInTheDocument();
    expect(screen.queryByText("Loading a prompt…")).not.toBeInTheDocument();
  });

  describe("audio and saving", () => {
    let played: number;
    beforeEach(() => {
      played = 0;
      vi.stubGlobal("URL", {
        ...URL,
        createObjectURL: () => "blob:test",
        revokeObjectURL: () => {},
      });
      vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(() => {
        played++;
        return Promise.resolve();
      });
      vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
    });
    afterEach(() => vi.unstubAllGlobals());

    async function recordOnce(user: ReturnType<typeof userEvent.setup>) {
      await user.click(screen.getByRole("button", { name: /^(Record|Try again)/ }));
      await user.click(await screen.findByRole("button", { name: /^Stop/ }));
      await screen.findByRole("button", { name: "Next prompt" });
    }

    it("saves a phrase once, however often the button is pressed", async () => {
      const user = userEvent.setup();
      const mock = createMockIpc();
      mount(mock);
      await screen.findByText("Reply to a greeting:");
      await recordOnce(user);

      const save = screen.getByRole("button", { name: "Save phrase to review" });
      await user.dblClick(save);
      expect(await screen.findByRole("button", { name: "Saved to review" })).toBeDisabled();
      expect(mock.calls.filter((c) => c.name === "addCard")).toHaveLength(1);

      // Trying the same prompt again does not offer to save it twice.
      await recordOnce(user);
      expect(screen.getByRole("button", { name: "Saved to review" })).toBeDisabled();
    });

    it("plays back the learner's own recording", async () => {
      const user = userEvent.setup();
      mount(createMockIpc());
      await screen.findByText("Reply to a greeting:");
      expect(screen.queryByRole("button", { name: "Play my recording" })).not.toBeInTheDocument();
      await recordOnce(user);
      await user.click(screen.getByRole("button", { name: "Play my recording" }));
      expect(played).toBe(1);

      // The clip goes with the prompt.
      await user.click(screen.getByRole("button", { name: "Next prompt" }));
      await waitFor(() =>
        expect(screen.queryByRole("button", { name: "Play my recording" })).not.toBeInTheDocument(),
      );
    });

    it("stops playback when a new recording starts", async () => {
      const user = userEvent.setup();
      const pause = vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
      mount(createMockIpc());
      await screen.findByText("Reply to a greeting:");
      await recordOnce(user);
      await user.click(screen.getByRole("button", { name: "Play my recording" }));
      pause.mockClear();
      await user.click(screen.getByRole("button", { name: /^Try again/ }));
      expect(pause).toHaveBeenCalled();
    });

    it("explains a busy voice instead of showing the raw error", async () => {
      const user = userEvent.setup();
      const mock = createMockIpc();
      vi.spyOn(mock, "synthesizeSpeech").mockRejectedValue("TTS_BUSY: queue full");
      mount(mock);
      await screen.findByText("Reply to a greeting:");
      await user.click(screen.getByRole("button", { name: /^Listen \S*P/ }));
      expect(await screen.findByRole("alert")).toHaveTextContent(/Still speaking/);
      expect(screen.queryByText(/TTS_BUSY/)).not.toBeInTheDocument();
    });

    it("Listen slowly asks for slow speech, and plain Listen does not", async () => {
      const user = userEvent.setup();
      const mock = createMockIpc();
      const spy = vi.spyOn(mock, "synthesizeSpeech");
      mount(mock);
      await screen.findByText("Reply to a greeting:");
      await user.click(screen.getByRole("button", { name: /^Listen \S*P/ }));
      await waitFor(() => expect(played).toBe(1));
      expect(spy.mock.calls[0]?.[2]).toBeUndefined();
      await user.click(screen.getByRole("button", { name: /^Listen slowly/ }));
      await waitFor(() => expect(played).toBe(2));
      expect(spy.mock.calls[1]?.[2]).toEqual({ slow: true });
    });

    it("pressing a marked word plays that word alone, in lower case", async () => {
      const user = userEvent.setup();
      const mock = createMockIpc({
        report: {
          pron_method: "gop",
          pron_overall: 60,
          pron: {
            overall: 60,
            words: [
              { word: "GOOD", start_ms: 0, end_ms: 300, gop: 0, score: 100, verdict: "good" },
              { word: "THROUGH", start_ms: 300, end_ms: 700, gop: -2, score: 3, verdict: "unclear" },
            ],
            target_logprob: -3,
            free_logprob: -2,
            normalized_conf: 0.5,
          },
        },
      });
      const spy = vi.spyOn(mock, "synthesizeSpeech");
      const { container } = mount(mock);
      await screen.findByText("Reply to a greeting:");
      await recordOnce(user);
      await screen.findByRole("button", { name: "Hear the word through" });
      await expectNoA11yViolations(container);
      await user.click(screen.getByRole("button", { name: "Hear the word through" }));
      await waitFor(() => expect(spy).toHaveBeenCalledTimes(1));
      expect(spy.mock.calls[0]?.[0]).toBe("through");
    });

    it("the S key plays the prompt slowly", async () => {
      const user = userEvent.setup();
      const mock = createMockIpc();
      const spy = vi.spyOn(mock, "synthesizeSpeech");
      mount(mock);
      await screen.findByText("Reply to a greeting:");
      await user.keyboard("s");
      await waitFor(() => expect(spy).toHaveBeenCalledTimes(1));
      expect(spy.mock.calls[0]?.[2]).toEqual({ slow: true });
    });

    it("ignores a second Listen while the first is still being synthesized", async () => {
      const user = userEvent.setup();
      const mock = createMockIpc();
      let release: () => void = () => {};
      const spy = vi.spyOn(mock, "synthesizeSpeech").mockImplementation(
        () => new Promise<number>((r) => (release = () => r(22050))),
      );
      mount(mock);
      await screen.findByText("Reply to a greeting:");
      const listen = screen.getByRole("button", { name: /^Listen \S*P/ });
      await user.click(listen);
      await user.click(listen);
      expect(spy).toHaveBeenCalledTimes(1);
      release();
      await waitFor(() => expect(played).toBe(1));
    });
  });
});

describe("Practice accessibility", () => {
  it("has no violations on the prompt view", async () => {
    const { container } = mount(createMockIpc());
    await screen.findByText("Reply to a greeting:");
    await expectNoA11yViolations(container);
  });

  it("has no violations on the feedback view", async () => {
    const user = userEvent.setup();
    const { container } = mount(createMockIpc());
    await screen.findByText("Reply to a greeting:");
    await user.click(screen.getByRole("button", { name: /^Record/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));
    await screen.findByRole("button", { name: "Next prompt" });
    await expectNoA11yViolations(container);
  });

  it("has no violations on a free-speaking prompt", async () => {
    const { container } = mount(
      createMockIpc({
        prompts: [makePrompt({ target_text: null, prompt_text: "Tell me about yourself." })],
      }),
    );
    await screen.findByText("Free speaking");
    await expectNoA11yViolations(container);
  });

  it("has no violations while recording", async () => {
    const user = userEvent.setup();
    const { container } = mount(createMockIpc());
    await screen.findByText("Reply to a greeting:");
    await user.click(screen.getByRole("button", { name: /^Record/ }));
    await screen.findByRole("button", { name: /^Stop/ });
    await expectNoA11yViolations(container);
  });

  it("has no violations on feedback with grammar and pronunciation marks", async () => {
    const user = userEvent.setup();
    const { container } = mount(
      createMockIpc({
        report: {
          transcript: "hi good too see you",
          lint: {
            diags: [{ start: 8, end: 11, message: "Did you mean to?", suggestions: ["to"] }],
            truncated: false,
          },
          grammar_score: 80,
          pron: {
            overall: 60,
            words: [
              { word: "HI", start_ms: 0, end_ms: 200, gop: -0.1, score: 95, verdict: "good" },
              { word: "GOOD", start_ms: 200, end_ms: 500, gop: -1.5, score: 55, verdict: "unclear" },
              { word: "SEE", start_ms: 500, end_ms: 800, gop: -3, score: 20, verdict: "poor" },
            ],
            target_logprob: -2.5,
            free_logprob: -1.5,
            normalized_conf: 0.9,
          },
        },
      }),
    );
    await screen.findByText("Reply to a greeting:");
    await user.click(screen.getByRole("button", { name: /^Record/ }));
    await user.click(await screen.findByRole("button", { name: /^Stop/ }));
    await screen.findByRole("button", { name: "Next prompt" });
    await expectNoA11yViolations(container);
  });

  it("has no violations with the speech model missing", async () => {
    const { container } = mount(
      createMockIpc({ modelStatus: { asr_model: false, asr_vocab: false, tts_voice: true } }),
    );
    await screen.findByText(/speech model is not installed yet/i);
    await expectNoA11yViolations(container);
  });

  it("has no violations with an error notice showing", async () => {
    const { container } = mount(createMockIpc({ fail: { nextPrompt: new Error("db is gone") } }));
    await screen.findByRole("alert");
    await expectNoA11yViolations(container);
  });

  it("has no violations with the writing check open and answered", async () => {
    const user = userEvent.setup();
    const { container } = mount(
      createMockIpc({
        lint: {
          diags: [{ start: 2, end: 6, message: "Use a singular noun.", suggestions: ["cat"] }],
          truncated: false,
        },
      }),
    );
    await screen.findByText("Reply to a greeting:");
    await user.click(screen.getByText("Check writing"));
    await user.type(screen.getByPlaceholderText(/Paste or type/), "a cats sat");
    await user.click(screen.getByRole("button", { name: "Check" }));
    await screen.findByRole("list", { name: "Grammar suggestions" });
    await expectNoA11yViolations(container);
  });
});
