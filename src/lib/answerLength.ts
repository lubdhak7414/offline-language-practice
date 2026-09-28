/**
 * Target answer length for an open-ended interview question, by level.
 * Advice for the learner, not a grade: nothing here scores what was said.
 */
const TARGET_SECONDS: Record<number, number> = { 1: 30, 2: 60, 3: 90 };

/** Where "about right" starts and ends, as fractions of the target. */
const SHORT_BELOW = 0.5;
const LONG_ABOVE = 1.5;

export function targetSeconds(level: number): number {
  return TARGET_SECONDS[level] ?? 60;
}

/** One line about the length of an answer, or null when it is not an interview answer. */
export function answerLengthNote(
  durationMs: number,
  level: number,
  isInterviewAnswer: boolean,
): string | null {
  if (!isInterviewAnswer) return null;
  const target = targetSeconds(level);
  const seconds = durationMs / 1000;
  if (seconds < target * SHORT_BELOW) {
    return `Short for this question (${Math.round(seconds)} s). About ${target} seconds is a good answer.`;
  }
  if (seconds > target * LONG_ABOVE) {
    return `Long for this question (${Math.round(seconds)} s). Try to wrap up in about ${target} seconds.`;
  }
  return `Good length for this question (${Math.round(seconds)} s).`;
}
