import { describe, expect, it } from "vitest";

import type { SessionSummary } from "../ipc/types";
import { describeSession } from "./sessionSummary";

const base: SessionSummary = {
  attempts: 3,
  scored: 2,
  avg_pron: 72,
  avg_wpm: 118.4,
  practice_ms: 6 * 60000,
  words_to_recheck: ["through", "world"],
};

describe("describeSession", () => {
  it("says nothing about a single attempt", () => {
    expect(describeSession({ ...base, attempts: 1 })).toBeNull();
  });

  it("describes a full summary", () => {
    expect(describeSession(base)).toBe(
      "This session: 3 attempts, 6 min, pronunciation averaging 72, 118 words a minute. Worth another listen: through, world.",
    );
  });

  it("leaves out what was not measured instead of showing zero", () => {
    const text = describeSession({
      ...base,
      scored: 0,
      avg_pron: null,
      avg_wpm: null,
      words_to_recheck: [],
      practice_ms: 20_000,
    });
    expect(text).toBe("This session: 3 attempts, under a minute.");
  });
});
