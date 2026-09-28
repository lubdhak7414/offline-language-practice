import { describe, expect, it } from "vitest";

import { describeDailyGoal } from "./practiceGoal";

describe("describeDailyGoal", () => {
  it("is silent when the goal is off", () => {
    expect(describeDailyGoal(3, 0)).toBeNull();
  });
  it("counts toward the goal", () => {
    expect(describeDailyGoal(0, 5)).toBe("0 of 5 today");
    expect(describeDailyGoal(3, 5)).toBe("3 of 5 today");
  });
  it("says so when the goal is reached, and keeps counting past it", () => {
    expect(describeDailyGoal(5, 5)).toBe("Daily goal reached: 5 of 5 today");
    expect(describeDailyGoal(7, 5)).toBe("Daily goal reached: 7 of 5 today");
  });
});
