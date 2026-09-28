import type { AttemptReport } from "../ipc/types";

/**
 * Smaller changes than this are not reported as a change. Pronunciation here
 * is a hint measured with a model that is right about a flagged word only
 * about one time in four, so a few points either way is noise. A judgement,
 * not a measured retest spread: no corpus has one speaker reading one
 * sentence twice.
 */
export const MIN_REPORTED_CHANGE = 5;

/**
 * How this try compares with the last one on the same prompt, or null when
 * the two cannot be compared. Only two acoustic ("gop") scores are: a
 * word-matching fallback is a different measurement, and free speaking has no
 * pronunciation number at all.
 */
export function describeChange(prev: AttemptReport | null, now: AttemptReport): string | null {
  if (!prev || prev.target_text === null || prev.target_text !== now.target_text) return null;
  if (prev.pron_method !== "gop" || now.pron_method !== "gop") return null;
  if (prev.pron_overall === null || now.pron_overall === null) return null;
  const delta = now.pron_overall - prev.pron_overall;
  if (Math.abs(delta) < MIN_REPORTED_CHANGE) {
    return `About the same as your last try (${prev.pron_overall}).`;
  }
  const points = Math.abs(delta);
  return delta > 0
    ? `Up ${points} points from your last try (${prev.pron_overall}).`
    : `Down ${points} points from your last try (${prev.pron_overall}).`;
}
