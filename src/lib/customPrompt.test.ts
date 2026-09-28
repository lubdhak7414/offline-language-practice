import { describe, expect, it } from "vitest";

import { CUSTOM_READ_CUE, MAX_CUSTOM_PROMPT_CHARS, validateCustomPrompt } from "./customPrompt";

const base = { category: "conversation", promptText: "", level: 2 };
const ok = (r: ReturnType<typeof validateCustomPrompt>) => (r.ok ? r.value : null);

describe("validateCustomPrompt", () => {
  it("trims, and gives a read-aloud sentence the default cue", () => {
    const v = ok(validateCustomPrompt({ ...base, targetText: "  I'd like a table for two.  " }));
    expect(v).toEqual({
      category: "conversation",
      prompt_text: CUSTOM_READ_CUE,
      target_text: "I'd like a table for two.",
      level: 2,
    });
  });

  it("treats a blank sentence as free speaking, which needs a question", () => {
    expect(validateCustomPrompt({ ...base, targetText: "  " }).ok).toBe(false);
    const v = ok(validateCustomPrompt({ ...base, promptText: "Describe your week.", targetText: " " }));
    expect(v?.target_text).toBeNull();
  });

  it("rejects what the recogniser could never match", () => {
    for (const bad of ["Meet me at 5.", "Un café, please.", "Save 50% today"]) {
      expect(validateCustomPrompt({ ...base, targetText: bad }).ok, bad).toBe(false);
    }
    expect(validateCustomPrompt({ ...base, targetText: "...!" }).ok).toBe(false);
  });

  it("allows ordinary punctuation and curly apostrophes", () => {
    expect(validateCustomPrompt({ ...base, targetText: 'Well, "hello" - again?' }).ok).toBe(true);
    expect(validateCustomPrompt({ ...base, targetText: "It’s fine." }).ok).toBe(true);
  });

  it("checks category, level and length", () => {
    expect(validateCustomPrompt({ ...base, category: "sport", targetText: "Hello there." }).ok).toBe(false);
    expect(validateCustomPrompt({ ...base, level: 4, targetText: "Hello there." }).ok).toBe(false);
    expect(validateCustomPrompt({ ...base, targetText: "a ".repeat(MAX_CUSTOM_PROMPT_CHARS) }).ok).toBe(false);
  });
});
