import { describe, expect, it } from "vitest";

import { friendlyAsrError, friendlyTtsError, isTtsBusy } from "./errors";

describe("friendlyAsrError", () => {
  it("translates each backend prefix", () => {
    expect(friendlyAsrError("ASR_SILENCE: no speech detected")).toMatch(/closer to the mic/);
    expect(friendlyAsrError("ASR_SHAPE: bad shape")).toMatch(/re-record/);
    expect(friendlyAsrError("ASR_NO_VOCAB: missing")).toMatch(/incomplete/);
    expect(friendlyAsrError("ASR_NO_MODEL: missing")).toMatch(/not installed/);
  });

  it("never swallows an error it does not recognise", () => {
    expect(friendlyAsrError(new Error("disk on fire"))).toContain("disk on fire");
  });
});

describe("tts errors", () => {
  it("detects the busy prefix", () => {
    expect(isTtsBusy("TTS_BUSY: queue full")).toBe(true);
    expect(isTtsBusy("something else")).toBe(false);
    expect(friendlyTtsError("TTS_BUSY: queue full")).toMatch(/Still speaking/);
  });
});
