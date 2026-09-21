import { describe, expect, it } from "vitest";

import { concatChunks, resampledFrameCount, TARGET_SAMPLE_RATE } from "./resample";

describe("concatChunks", () => {
  it("joins chunks in order without gaps", () => {
    const out = concatChunks([
      new Float32Array([1, 2]),
      new Float32Array([]),
      new Float32Array([3, 4, 5]),
    ]);
    expect(Array.from(out)).toEqual([1, 2, 3, 4, 5]);
  });

  it("returns an empty buffer for no chunks", () => {
    expect(concatChunks([]).length).toBe(0);
  });
});

describe("resampledFrameCount", () => {
  it("scales by the rate ratio", () => {
    expect(resampledFrameCount(48000, 48000)).toBe(TARGET_SAMPLE_RATE);
    expect(resampledFrameCount(44100, 44100)).toBe(TARGET_SAMPLE_RATE);
  });

  it("never asks OfflineAudioContext for zero frames", () => {
    // A zero-frame OfflineAudioContext throws, so a very short clip must
    // still round up to one frame.
    expect(resampledFrameCount(1, 48000)).toBe(1);
    expect(resampledFrameCount(0, 48000)).toBe(0);
  });

  it("degrades to passthrough on a nonsense rate", () => {
    expect(resampledFrameCount(100, 0)).toBe(100);
    expect(resampledFrameCount(100, Number.NaN)).toBe(100);
  });
});
