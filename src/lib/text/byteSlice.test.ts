import { describe, expect, it } from "vitest";

import { byteLength, byteSlice } from "./byteSlice";

describe("byteSlice", () => {
  it("matches plain slicing for ASCII", () => {
    expect(byteSlice("hello world", 6, 11)).toBe("world");
  });

  it("uses byte offsets, not char indices", () => {
    // "café" is 5 bytes: c a f Ã© . The lint span for "é" is 3..5, which a
    // naive String.slice would render as "é " off by one.
    const text = "café au lait";
    expect(byteLength(text)).toBe(13);
    expect(byteSlice(text, 3, 5)).toBe("é");
    expect(byteSlice(text, 6, 8)).toBe("au");
  });

  it("handles multi-byte scripts and astral characters", () => {
    expect(byteSlice("日本語", 3, 6)).toBe("本");
    // An emoji is 4 UTF-8 bytes but 2 UTF-16 code units.
    const text = "ok 👍 done";
    expect(byteLength(text)).toBe(12);
    expect(byteSlice(text, 3, 7)).toBe("👍");
  });

  it("clamps out-of-range and inverted spans instead of throwing", () => {
    expect(byteSlice("abc", -5, 99)).toBe("abc");
    expect(byteSlice("abc", 2, 1)).toBe("");
    expect(byteSlice("abc", Number.NaN, 2)).toBe("ab");
  });

  it("does not throw when a span splits a character", () => {
    // Half of "é" is not valid UTF-8; the decoder substitutes rather than
    // throwing, so one mangled glyph beats a crashed panel.
    expect(() => byteSlice("café", 3, 4)).not.toThrow();
  });
});
