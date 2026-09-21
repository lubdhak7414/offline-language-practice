import { describe, expect, it } from "vitest";

import { DEFAULT_ROUTE, parseRoute } from "./router";

describe("parseRoute", () => {
  it("reads a known route from the hash", () => {
    expect(parseRoute("#/review")).toBe("review");
    expect(parseRoute("#decks")).toBe("decks");
    expect(parseRoute("#/practice/extra/segments")).toBe("practice");
  });

  it("falls back rather than rendering nothing", () => {
    // A blank or unknown hash is the first-launch case and a typo case; both
    // should land somewhere useful instead of on an empty screen.
    for (const hash of ["", "#", "#/", "#/nope", "#//"]) {
      expect(parseRoute(hash)).toBe(DEFAULT_ROUTE);
    }
  });
});
