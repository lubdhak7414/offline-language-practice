import type { PracticeDay } from "../ipc/types";
import { MIN_REPORTED_CHANGE } from "./attemptDelta";

type Week = { attempts: number; daysPracticed: number; scored: number; avgPron: number | null };

function summarise(days: PracticeDay[]): Week {
  let scored = 0;
  let total = 0;
  for (const d of days) {
    if (d.avg_pron !== null && d.scored > 0) {
      scored += d.scored;
      total += d.avg_pron * d.scored;
    }
  }
  return {
    attempts: days.reduce((a, d) => a + d.attempts, 0),
    daysPracticed: days.filter((d) => d.attempts > 0).length,
    scored,
    avgPron: scored > 0 ? Math.round(total / scored) : null,
  };
}

/**
 * The last seven days of speaking against the seven before, in a sentence or
 * two, or null when there is nothing to say. `days` is oldest first and ends
 * today, as `statsPractice` returns it. Pronunciation is averaged over the
 * attempts that were scored acoustically, weighted by how many there were, and
 * a comparison is made only when both weeks have one and the difference is
 * more than noise.
 */
export function describeWeek(days: PracticeDay[]): string | null {
  const recent = summarise(days.slice(-7));
  const before = summarise(days.slice(-14, -7));
  if (recent.attempts === 0) return null;

  const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;
  const parts = [
    `In the last 7 days you did ${plural(recent.attempts, "speaking attempt", "speaking attempts")} on ${plural(recent.daysPracticed, "day", "days")}`,
  ];
  if (before.attempts > 0) parts[0] += ` (${before.attempts} the week before)`;
  parts[0] += ".";

  if (recent.avgPron !== null) {
    let line = `Reading aloud averaged ${recent.avgPron} over ${plural(recent.scored, "scored attempt", "scored attempts")}`;
    if (before.avgPron !== null) {
      const delta = recent.avgPron - before.avgPron;
      if (Math.abs(delta) < MIN_REPORTED_CHANGE) line += `, about the same as the week before (${before.avgPron})`;
      else line += `, ${delta > 0 ? "up" : "down"} ${Math.abs(delta)} from the week before (${before.avgPron})`;
    }
    parts.push(line + ".");
  }
  return parts.join(" ");
}
