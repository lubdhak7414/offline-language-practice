import { describe, expect, it } from "vitest";

import type { AttemptRow } from "../ipc/types";
import { dayOptions, localDayKey, visibleRows } from "./attemptDays";

const at = (y: number, m: number, d: number, h: number, id: string): AttemptRow => ({
  id,
  prompt_id: null,
  target_text: null,
  transcript: "",
  duration_ms: 1000,
  created_at: new Date(y, m - 1, d, h).getTime(),
  pron_overall: null,
  pron_method: null,
  overall: 80,
});

// Newest first, as the backend returns them.
const rows = [at(2026, 9, 28, 20, "a"), at(2026, 9, 28, 8, "b"), at(2026, 9, 27, 23, "c"), at(2026, 9, 1, 9, "d")];

describe("attempt days", () => {
  it("keys a timestamp by its local day, not UTC", () => {
    expect(localDayKey(new Date(2026, 8, 28, 23, 59).getTime())).toBe("2026-9-28");
    expect(localDayKey(new Date(2026, 8, 28, 0, 1).getTime())).toBe("2026-9-28");
  });

  it("lists each day once, newest first, with a count", () => {
    const opts = dayOptions(rows);
    expect(opts.map((o) => [o.key, o.count])).toEqual([
      ["2026-9-28", 2],
      ["2026-9-27", 1],
      ["2026-9-1", 1],
    ]);
    expect(dayOptions([])).toEqual([]);
  });

  it("shows the first few, or every attempt of a chosen day", () => {
    expect(visibleRows(rows, null, 2).map((r) => r.id)).toEqual(["a", "b"]);
    expect(visibleRows(rows, null, 99)).toHaveLength(4);
    expect(visibleRows(rows, "2026-9-28", 1).map((r) => r.id)).toEqual(["a", "b"]);
    expect(visibleRows(rows, "2026-1-1", 5)).toEqual([]);
  });
});
