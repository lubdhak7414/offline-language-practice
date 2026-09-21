import { describe, expect, it } from "vitest";

import { parseRating } from "./rating";

describe("parseRating", () => {
  it("accepts the four valid grades as strings or numbers", () => {
    expect(parseRating("1")).toBe(1);
    expect(parseRating(4)).toBe(4);
  });

  it("rejects everything else", () => {
    for (const bad of ["0", "5", "", " ", "x", null, undefined, Number.NaN, 2.5]) {
      expect(parseRating(bad)).toBeNull();
    }
  });
});
