import { describe, expect, it } from "vitest";

import { formatInterval } from "./interval";

describe("formatInterval", () => {
  it("uses minutes and hours below a day", () => {
    // "Again" typically comes back in minutes; showing "0.0d" would be
    // indistinguishable from "now".
    expect(formatInterval(1 / 24 / 60 / 2)).toBe("1m");
    expect(formatInterval(10 / (24 * 60))).toBe("10m");
    expect(formatInterval(0.25)).toBe("6h");
  });

  it("uses days, months and years as the scale grows", () => {
    expect(formatInterval(1)).toBe("1d");
    expect(formatInterval(6.4)).toBe("6d");
    expect(formatInterval(60)).toBe("2mo");
    expect(formatInterval(547)).toBe("1.5y");
    expect(formatInterval(4015)).toBe("11y");
  });

  it("renders nothing rather than NaN for missing input", () => {
    expect(formatInterval(undefined)).toBe("");
    expect(formatInterval(Number.NaN)).toBe("");
    expect(formatInterval(-1)).toBe("");
  });
});
