import { describe, expect, it } from "vitest";

import { classifyPeak, describeMicLevel } from "./micLevel";

describe("mic level", () => {
  it("classifies by peak, edges included", () => {
    expect(classifyPeak(0)).toBe("nothing");
    expect(classifyPeak(0.0099)).toBe("nothing");
    expect(classifyPeak(0.01)).toBe("quiet");
    expect(classifyPeak(0.0799)).toBe("quiet");
    expect(classifyPeak(0.08)).toBe("ok");
    expect(classifyPeak(0.44)).toBe("ok");
    expect(classifyPeak(0.9899)).toBe("ok");
    expect(classifyPeak(0.99)).toBe("clipping");
    expect(classifyPeak(1)).toBe("clipping");
  });

  it("only speaks up when something is wrong", () => {
    expect(describeMicLevel(0.4)).toBeNull();
    expect(describeMicLevel(0.03)).toMatch(/quiet/);
    expect(describeMicLevel(0)).toMatch(/No sound/);
    expect(describeMicLevel(1)).toMatch(/too loud/);
  });
});
