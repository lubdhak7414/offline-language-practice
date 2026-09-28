import type { SessionSummary } from "../ipc/types";

/** A summary of one attempt is just that attempt, so it is only shown from the second. */
export const MIN_ATTEMPTS_FOR_SUMMARY = 2;

/**
 * One plain sentence about the sitting so far, or null before there is enough
 * to say. Parts that were not measured are left out, never shown as zero.
 */
export function describeSession(s: SessionSummary): string | null {
  if (s.attempts < MIN_ATTEMPTS_FOR_SUMMARY) return null;
  const minutes = s.practice_ms / 60000;
  const time = minutes < 1 ? "under a minute" : `${Math.round(minutes)} min`;
  const parts = [`${s.attempts} attempts`, time];
  if (s.avg_pron !== null) parts.push(`pronunciation averaging ${s.avg_pron}`);
  if (s.avg_wpm !== null) parts.push(`${Math.round(s.avg_wpm)} words a minute`);
  let text = `This session: ${parts.join(", ")}.`;
  if (s.words_to_recheck.length > 0) {
    text += ` Worth another listen: ${s.words_to_recheck.join(", ")}.`;
  }
  return text;
}
