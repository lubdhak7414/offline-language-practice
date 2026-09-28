import { describe, expect, it } from "vitest";

import { answerLengthNote, targetSeconds } from "./answerLength";

describe("answerLengthNote", () => {
  it("says nothing for anything but an interview answer", () => {
    expect(answerLengthNote(5000, 2, false)).toBeNull();
  });

  it("scales the target with the level and defaults unknown levels", () => {
    expect([1, 2, 3].map(targetSeconds)).toEqual([30, 60, 90]);
    expect(targetSeconds(9)).toBe(60);
  });

  it("calls a very short answer short and a very long one long", () => {
    expect(answerLengthNote(10_000, 2, true)).toBe("Short for this question (10 s). About 60 seconds is a good answer.");
    expect(answerLengthNote(100_000, 2, true)).toBe(
      "Long for this question (100 s). Try to wrap up in about 60 seconds.",
    );
  });

  it("calls the range between them good, edges included", () => {
    expect(answerLengthNote(30_000, 2, true)).toMatch(/^Good length/);
    expect(answerLengthNote(90_000, 2, true)).toMatch(/^Good length/);
    expect(answerLengthNote(29_999, 2, true)).toMatch(/^Short/);
    expect(answerLengthNote(90_001, 2, true)).toMatch(/^Long/);
  });
});
