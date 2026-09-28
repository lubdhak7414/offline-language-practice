import type { AttemptRow } from "../ipc/types";

/** How many attempts the history shows before "Show more". */
export const PAGE_SIZE = 20;

/** The learner's local calendar day of a timestamp, as "2026-9-28". */
export function localDayKey(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${d.getMonth() + 1}-${d.getDate()}`;
}

export type DayOption = { key: string; label: string; count: number };

/** The days that have attempts, newest first, with how many each has. */
export function dayOptions(rows: AttemptRow[]): DayOption[] {
  const byKey = new Map<string, DayOption>();
  for (const r of rows) {
    const key = localDayKey(r.created_at);
    const seen = byKey.get(key);
    if (seen) seen.count += 1;
    else
      byKey.set(key, {
        key,
        count: 1,
        label: new Date(r.created_at).toLocaleDateString(undefined, {
          weekday: "short",
          day: "numeric",
          month: "short",
        }),
      });
  }
  // Rows arrive newest first, and a Map keeps insertion order.
  return [...byKey.values()];
}

/**
 * The rows to list. One chosen day shows all of that day; otherwise the first
 * `shown`. `rows` is newest first.
 */
export function visibleRows(rows: AttemptRow[], dayKey: string | null, shown: number): AttemptRow[] {
  if (dayKey !== null) return rows.filter((r) => localDayKey(r.created_at) === dayKey);
  return rows.slice(0, shown);
}
