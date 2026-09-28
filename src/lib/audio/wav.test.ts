import { describe, expect, it } from "vitest";

import { f32ToWav } from "./wav";

function f32Bytes(xs: number[]): Uint8Array {
  return new Uint8Array(new Float32Array(xs).buffer);
}

describe("f32ToWav", () => {
  it("writes a mono 16-bit header for the given rate", () => {
    const wav = f32ToWav(f32Bytes([0, 0, 0]), 16000);
    const v = new DataView(wav.buffer);
    const text = (at: number) => String.fromCharCode(...wav.slice(at, at + 4));
    expect(text(0)).toBe("RIFF");
    expect(text(8)).toBe("WAVE");
    expect(text(12)).toBe("fmt ");
    expect(text(36)).toBe("data");
    expect(v.getUint32(4, true)).toBe(36 + 6);
    expect(v.getUint16(20, true)).toBe(1);
    expect(v.getUint16(22, true)).toBe(1);
    expect(v.getUint32(24, true)).toBe(16000);
    expect(v.getUint32(28, true)).toBe(32000);
    expect(v.getUint16(34, true)).toBe(16);
    expect(v.getUint32(40, true)).toBe(6);
    expect(wav.byteLength).toBe(44 + 6);
  });

  it("scales and clips samples", () => {
    const wav = f32ToWav(f32Bytes([0, 1, -1, 0.5, 2, -3]), 16000);
    const v = new DataView(wav.buffer);
    const s = (i: number) => v.getInt16(44 + i * 2, true);
    expect([s(0), s(1), s(2), s(3), s(4), s(5)]).toEqual([0, 32767, -32768, 16384, 32767, -32768]);
  });

  it("reads samples that do not start on a 4-byte boundary", () => {
    const backing = new Uint8Array(1 + 8);
    backing.set(f32Bytes([0.5, -0.5]), 1);
    const wav = f32ToWav(backing.subarray(1), 16000);
    const v = new DataView(wav.buffer);
    expect(v.getInt16(44, true)).toBe(16384);
    expect(v.getInt16(46, true)).toBe(-16384);
  });

  it("ignores a trailing partial sample", () => {
    expect(f32ToWav(new Uint8Array(7), 16000).byteLength).toBe(44 + 2);
  });
});
