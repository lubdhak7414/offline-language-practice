import type { Rating } from "../ipc/types";
import { parseRating } from "./rating";

/**
 * What the review screen looks like when a key arrives.
 *
 * Pulling this out of the DOM makes the rule testable. It is worth testing:
 * the handler used to bail out whenever focus was on a `BUTTON`, and
 * revealing a card focuses a grade button — so 1-4 did nothing in exactly
 * the state they exist for, and nothing caught it.
 */
export type ReviewKeyContext = {
  /** `event.target.tagName`, or "" when there is no element target. */
  targetTag: string;
  isContentEditable: boolean;
  isComposing: boolean;
  hasCard: boolean;
  revealed: boolean;
  gradeRowHidden: boolean;
  revealHidden: boolean;
  /** True while the global `g` prefix is armed; the route stands down. */
  goPending: boolean;
};

export type ReviewKeyAction = { kind: "grade"; rating: Rating } | { kind: "reveal" };

/** Elements whose own keyboard behaviour must never be hijacked. */
const TEXT_ENTRY = new Set(["INPUT", "SELECT", "TEXTAREA", "AUDIO"]);

export function reviewKeyAction(
  key: string,
  ctx: ReviewKeyContext,
): ReviewKeyAction | null {
  if (TEXT_ENTRY.has(ctx.targetTag) || ctx.isContentEditable || ctx.isComposing) {
    return null;
  }
  if (ctx.goPending || !ctx.hasCard) return null;

  const rating = parseRating(key);
  if (rating) {
    return ctx.revealed && !ctx.gradeRowHidden ? { kind: "grade", rating } : null;
  }

  if (key === " " || key === "Enter") {
    // A focused button already activates on Space/Enter; reveal here would
    // fire twice. Grades are digits, so they have no such conflict — which
    // is why BUTTON is excluded here and only here.
    if (ctx.targetTag === "BUTTON") return null;
    return !ctx.revealed && !ctx.revealHidden ? { kind: "reveal" } : null;
  }

  return null;
}

/** What the practice screen looks like when a key arrives. */
export type PracticeKeyContext = {
  targetTag: string;
  isContentEditable: boolean;
  isComposing: boolean;
  goPending: boolean;
  hasPrompt: boolean;
  recording: boolean;
  /** False while the model is missing or a score is still being computed. */
  canRecord: boolean;
};

export type PracticeKeyAction =
  | { kind: "record" }
  | { kind: "stop" }
  | { kind: "listen" };

export function practiceKeyAction(
  key: string,
  ctx: PracticeKeyContext,
): PracticeKeyAction | null {
  if (TEXT_ENTRY.has(ctx.targetTag) || ctx.isContentEditable || ctx.isComposing) {
    return null;
  }
  if (ctx.goPending || !ctx.hasPrompt) return null;

  switch (key.toLowerCase()) {
    case "r":
      // One key for both edges: a learner mid-sentence should not have to
      // remember a different key to stop than the one they used to start.
      if (ctx.recording) return { kind: "stop" };
      return ctx.canRecord ? { kind: "record" } : null;
    case "p":
      return ctx.recording ? null : { kind: "listen" };
    default:
      return null;
  }
}
