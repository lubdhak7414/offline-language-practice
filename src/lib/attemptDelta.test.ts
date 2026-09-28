import { describe, expect, it } from "vitest";

import type { AttemptReport } from "../ipc/types";
import { describeChange } from "./attemptDelta";

function report(over: Partial<AttemptReport>): AttemptReport {
  return {
    attempt_id: "a",
    transcript: "t",
    target_text: "Hi there.",
    pron_method: "gop",
    pron_overall: 70,
    alignment: null,
    pron: null,
    fluency: null,
    lint: { diags: [], truncated: false },
    grammar_score: 100,
    duration_ms: 1000,
    word_count: 2,
    overall: 80,
    overall_basis: ["pronunciation"],
    ...over,
  };
}

describe("describeChange", () => {
  it("is null with nothing to compare to", () => {
    expect(describeChange(null, report({}))).toBeNull();
  });

  it("reports a clear rise and fall", () => {
    expect(describeChange(report({ pron_overall: 60 }), report({ pron_overall: 72 }))).toBe(
      "Up 12 points from your last try (60).",
    );
    expect(describeChange(report({ pron_overall: 80 }), report({ pron_overall: 70 }))).toBe(
      "Down 10 points from your last try (80).",
    );
  });

  it("calls a small change the same", () => {
    expect(describeChange(report({ pron_overall: 70 }), report({ pron_overall: 74 }))).toBe(
      "About the same as your last try (70).",
    );
    expect(describeChange(report({ pron_overall: 70 }), report({ pron_overall: 65 }))).toMatch(/^Down 5/);
  });

  it("does not compare different measurements", () => {
    expect(describeChange(report({ pron_method: "text" }), report({}))).toBeNull();
    expect(describeChange(report({}), report({ pron_method: "text" }))).toBeNull();
  });

  it("does not compare free speaking or different prompts", () => {
    const free = { target_text: null, pron_method: null, pron_overall: null };
    expect(describeChange(report(free), report(free))).toBeNull();
    expect(describeChange(report({ target_text: "Other." }), report({}))).toBeNull();
  });
});
