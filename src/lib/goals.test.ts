import { describe, expect, it } from "vitest";

import { categoryForGoal, GOALS } from "./goals";

describe("categoryForGoal", () => {
  it("opens interviews for the interview goal", () => {
    expect(categoryForGoal("interview")).toBe("interview");
  });
  it("opens conversation for everyday, both, and anything unknown", () => {
    expect(categoryForGoal("everyday")).toBe("conversation");
    expect(categoryForGoal("both")).toBe("conversation");
    expect(categoryForGoal("")).toBe("conversation");
  });
  it("lists every goal the backend accepts", () => {
    expect(GOALS.map((g) => g.id)).toEqual(["everyday", "interview", "both"]);
  });
});
