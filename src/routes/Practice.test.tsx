import { render, screen, waitFor } from "@testing-library/preact";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, makePrompt, type MockIpc } from "../ipc/mock";
import { Practice } from "./Practice";

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
