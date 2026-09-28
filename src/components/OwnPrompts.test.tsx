import { render, screen, waitFor } from "@testing-library/preact";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, type MockIpc } from "../ipc/mock";
import { Practice } from "../routes/Practice";

vi.mock("../app/recorder", () => ({
  MAX_RECORDING_MS: 120_000,
  createRecorder: () => ({
    start: async () => {},
    stop: async () => ({ pcm: new Uint8Array(64_000), durationMs: 1000, peak: 0.3 }),
    cancel: () => {},
    isRecording: () => false,
  }),
}));

let restore: (() => void) | undefined;

function mount(mock: MockIpc) {
  restore = setIpc(mock);
  return render(<Practice announce={() => {}} />);
}

afterEach(() => {
  restore?.();
  restore = undefined;
});

async function open(user: ReturnType<typeof userEvent.setup>) {
  await screen.findByText("Reply to a greeting:");
  await user.click(screen.getByText("Your own prompts"));
}

describe("Your own prompts", () => {
  it("adds a read-aloud sentence and practises it straight away", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc();
    mount(mock);
    await open(user);
    await user.type(screen.getByLabelText("Sentence"), "Could I have the bill?");
    await user.click(screen.getByRole("button", { name: "Add and practise" }));

    expect(await screen.findByText("Read this aloud:")).toBeInTheDocument();
    expect(screen.getAllByText("Could I have the bill?").length).toBeGreaterThan(0);
    expect(mock.calls.find((c) => c.name === "addPrompt")?.args[0]).toMatchObject({
      category: "conversation",
      promptText: "",
      targetText: "Could I have the bill?",
      level: 2,
    });
    expect(screen.getByText("Read aloud")).toBeInTheDocument();
  });

  it("adds a question as free speaking, with no sentence to score", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc();
    mount(mock);
    await open(user);
    await user.click(screen.getByRole("button", { name: "Question to answer" }));
    await user.type(screen.getByLabelText("Question"), "Describe your ideal weekend.");
    await user.click(screen.getByRole("button", { name: "Add and practise" }));

    expect(await screen.findByText("Free speaking")).toBeInTheDocument();
    const args = mock.calls.find((c) => c.name === "addPrompt")?.args[0] as { targetText?: string; promptText: string };
    expect(args.promptText).toBe("Describe your ideal weekend.");
    expect(args.targetText).toBeUndefined();
  });

  it("explains a sentence the scorer could never match, and keeps the text", async () => {
    const user = userEvent.setup();
    mount(createMockIpc());
    await open(user);
    await user.type(screen.getByLabelText("Sentence"), "Meet me at 5.");
    await user.click(screen.getByRole("button", { name: "Add and practise" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/plain letters/);
    expect(screen.getByLabelText("Sentence")).toHaveValue("Meet me at 5.");
  });

  it("lists prompts and deletes one", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc();
    mount(mock);
    await open(user);
    await user.type(screen.getByLabelText("Sentence"), "Good morning everyone.");
    await user.click(screen.getByRole("button", { name: "Add and practise" }));
    const list = await screen.findByRole("list", { name: "Your prompts" });
    expect(list).toHaveTextContent("Good morning everyone.");

    await user.click(screen.getByRole("button", { name: "Delete: Good morning everyone." }));
    await waitFor(() => expect(screen.queryByRole("list", { name: "Your prompts" })).not.toBeInTheDocument());
    expect(mock.calls.some((c) => c.name === "deletePrompt")).toBe(true);
  });
});
