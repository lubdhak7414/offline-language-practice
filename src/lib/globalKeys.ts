import { ROUTES, type Route } from "../app/router";

/**
 * The app-wide keyboard map, as a pure rule.
 *
 * Same reasoning as `reviewKeyAction`: a shortcut that lives inside a DOM
 * handler can only be tested by simulating a browser, so in practice it is
 * not tested at all — which is how keyboard grading stayed broken through a
 * whole release. The handler in `App` does nothing but ask this function
 * what the key means.
 *
 * Returning `null` means "not ours": the event keeps travelling, so route
 * shortcuts such as the review grades still see it.
 */
export type GlobalKeyContext = {
  /** `event.target.tagName`, or "" when there is no element target. */
  targetTag: string;
  isContentEditable: boolean;
  isComposing: boolean;
  /** Any modifier held; a browser or OS shortcut is never ours to take. */
  hasModifier: boolean;
  helpOpen: boolean;
  /** True when the previous keypress was `g` and is still waiting. */
  pendingGo: boolean;
};

/**
 * Whether the `g` prefix is armed, shared with the routes.
 *
 * Both the app map and the route maps listen on `document`, and
 * `preventDefault` does not stop a sibling listener. Without this, `g` then
 * `p` would navigate to Practice *and* play the prompt. Routes read it at
 * event time, so a plain holder is enough — no reactivity is involved.
 */
export const goPrefix = { armed: false };

export type GlobalKeyAction =
  | { kind: "help"; open: boolean }
  /** Start the `g` prefix and wait for a destination. */
  | { kind: "go" }
  | { kind: "navigate"; route: Route }
  /** The prefix was followed by something meaningless; drop it. */
  | { kind: "cancel" };

const TEXT_ENTRY = new Set(["INPUT", "SELECT", "TEXTAREA", "AUDIO"]);

/**
 * Destinations for the `g` prefix.
 *
 * `p` is Practice and `s` is "stats" for Progress, because both routes start
 * with a P and a shortcut that depends on remembering which one won is not a
 * shortcut.
 */
const GO_TARGETS: Record<string, Route> = {
  p: "practice",
  r: "review",
  d: "decks",
  s: "progress",
  l: "lab",
};

export function globalKeyAction(
  key: string,
  ctx: GlobalKeyContext,
): GlobalKeyAction | null {
  if (TEXT_ENTRY.has(ctx.targetTag) || ctx.isContentEditable || ctx.isComposing) {
    return null;
  }
  if (ctx.hasModifier) return null;

  if (ctx.pendingGo) {
    const route = GO_TARGETS[key.toLowerCase()];
    // The prefix is always consumed, so a mistyped destination cannot leave
    // the next keystroke armed.
    return route && ROUTES.includes(route)
      ? { kind: "navigate", route }
      : { kind: "cancel" };
  }

  if (key === "?") return { kind: "help", open: true };
  if (key === "Escape") return ctx.helpOpen ? { kind: "help", open: false } : null;
  if (key.toLowerCase() === "g") return { kind: "go" };
  return null;
}

/** The map, in the order the help sheet shows it. */
export const KEY_HELP: Array<{ keys: string; what: string }> = [
  { keys: "1 – 4", what: "Grade the card you are reviewing" },
  { keys: "Space", what: "Show the answer" },
  { keys: "R", what: "Start or stop recording" },
  { keys: "P", what: "Play the prompt aloud" },
  { keys: "G then P", what: "Go to Practice" },
  { keys: "G then R", what: "Go to Review" },
  { keys: "G then D", what: "Go to Decks" },
  { keys: "G then S", what: "Go to Progress" },
  { keys: "?", what: "Show this list" },
  { keys: "Esc", what: "Close this list" },
];
