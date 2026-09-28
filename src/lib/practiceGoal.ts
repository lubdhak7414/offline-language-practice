/**
 * "3 of 5 today", or null when the daily goal is off. Purely informational:
 * reaching the goal changes the wording and nothing else, and missing it
 * withholds nothing.
 */
export function describeDailyGoal(done: number, goal: number): string | null {
  if (goal <= 0) return null;
  if (done >= goal) return `Daily goal reached: ${done} of ${goal} today`;
  return `${done} of ${goal} today`;
}
