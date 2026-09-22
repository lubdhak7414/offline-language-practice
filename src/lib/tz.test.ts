import { afterEach, describe, expect, it, vi } from "vitest";

import { tzOffsetMinutes } from "./tz";

describe("tzOffsetMinutes", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("negates getTimezoneOffset's sign", () => {
    // UTC-5 (e.g. US Eastern standard time): getTimezoneOffset returns +300.
    vi.spyOn(Date.prototype, "getTimezoneOffset").mockReturnValue(300);
    expect(tzOffsetMinutes()).toBe(-300);
  });

  it("handles a positive UTC offset", () => {
    // UTC+5:30 (India): getTimezoneOffset returns -330.
    vi.spyOn(Date.prototype, "getTimezoneOffset").mockReturnValue(-330);
    expect(tzOffsetMinutes()).toBe(330);
  });

  it("handles UTC itself", () => {
    vi.spyOn(Date.prototype, "getTimezoneOffset").mockReturnValue(0);
    // Negating zero legitimately yields `-0`, which is still zero by value.
    expect(tzOffsetMinutes()).toBe(-0);
  });
});
