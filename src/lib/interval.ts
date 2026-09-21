/**
 * Render an FSRS interval the way a learner reads it.
 *
 * "0.23" days is meaningless on a button that has to be understood in the
 * half-second before grading; "6m" is not.
 */
export function formatInterval(days: number | undefined): string {
  if (days === undefined || !Number.isFinite(days) || days < 0) return "";
  const minutes = days * 24 * 60;
  if (minutes < 60) return `${Math.max(1, Math.round(minutes))}m`;
  if (days < 1) return `${Math.round(days * 24)}h`;
  if (days < 30) return `${Math.round(days)}d`;
  if (days < 365) return `${Math.round(days / 30)}mo`;
  const years = days / 365;
  // One decimal below 10 years, because 1.5y and 2y are a real difference.
  return years < 10 ? `${years.toFixed(1)}y` : `${Math.round(years)}y`;
}
