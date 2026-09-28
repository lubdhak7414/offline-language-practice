/**
 * A one-shot request from another screen for Practice to open a particular
 * prompt (history's "Practise again"). A module variable rather than a route
 * parameter: the router only knows screen names, and the request must be
 * used once, not survive a reload or a later visit to Practice.
 */
let wanted: string | null = null;

export function requestPractice(promptId: string): void {
  wanted = promptId;
}

/** Returns the requested prompt id and forgets it. */
export function takePracticeRequest(): string | null {
  const id = wanted;
  wanted = null;
  return id;
}
