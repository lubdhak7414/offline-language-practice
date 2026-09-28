import { describe, expect, it } from "vitest";

import type { PracticeDay } from "../ipc/types";
import { describeWeek } from "./weeklyRecap";

const day = (attempts: number, scored = 0, avg: number | null = null): PracticeDay => ({
  day: 0,
  attempts,
  scored,
  avg_pron: avg,
  avg_wpm: null,
});
const quiet = Array.from({ length: 7 }, () => day(0));

describe("describeWeek", () => {
  it("says nothing when the last week had no practice", () => {
    expect(describeWeek([...quiet, ...quiet])).toBeNull();
    expect(describeWeek([])).toBeNull();
  });

  it("counts attempts and days, with the week before when there was one", () => {
    const before = [day(2), ...quiet.slice(1)];
    const recent = [day(1), day(3), ...quiet.slice(2)];
    expect(describeWeek([...before, ...recent])).toBe(
      "In the last 7 days you did 4 speaking attempts on 2 days (2 the week before).",
    );
    expect(describeWeek([...quiet, day(1), ...quiet.slice(1)])).toBe(
      "In the last 7 days you did 1 speaking attempt on 1 day.",
    );
  });

  it("weights pronunciation by scored attempts, not by day", () => {
    // 1 attempt at 90 and 3 at 50: a plain mean of days would say 70.
    const recent = [day(1, 1, 90), day(3, 3, 50), ...quiet.slice(2)];
    expect(describeWeek([...quiet, ...recent])).toContain("averaged 60 over 4 scored attempts.");
  });

  it("compares with the week before only past the noise threshold", () => {
    const wk = (avg: number) => [day(2, 2, avg), ...quiet.slice(1)];
    expect(describeWeek([...wk(60), ...wk(72)])).toContain("up 12 from the week before (60)");
    expect(describeWeek([...wk(80), ...wk(70)])).toContain("down 10 from the week before (80)");
    expect(describeWeek([...wk(70), ...wk(73)])).toContain("about the same as the week before (70)");
  });

  it("never invents a pronunciation figure for free speaking", () => {
    const recent = [day(3, 0, null), ...quiet.slice(1)];
    expect(describeWeek([...quiet, ...recent])).toBe(
      "In the last 7 days you did 3 speaking attempts on 1 day.",
    );
  });

  it("does not compare with a week that had no scored attempts", () => {
    const before = [day(2, 0, null), ...quiet.slice(1)];
    const recent = [day(2, 2, 70), ...quiet.slice(1)];
    const text = describeWeek([...before, ...recent]) ?? "";
    expect(text).toContain("averaged 70 over 2 scored attempts.");
    expect(text).not.toContain("week before (");
  });
});
