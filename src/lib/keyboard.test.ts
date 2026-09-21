import { describe, expect, it } from "vitest";

import { reviewKeyAction, type ReviewKeyContext } from "./keyboard";

const revealedWithGradeButtonFocused = (over: Partial<ReviewKeyContext> = {}): ReviewKeyContext => ({
  targetTag: "BUTTON",
  isContentEditable: false,
  isComposing: false,
  hasCard: true,
  revealed: true,
  gradeRowHidden: false,
  revealHidden: true,
  ...over,
});

const hiddenCard = (over: Partial<ReviewKeyContext> = {}): ReviewKeyContext => ({
  targetTag: "BODY",
  isContentEditable: false,
  isComposing: false,
  hasCard: true,
  revealed: false,
  gradeRowHidden: true,
  revealHidden: false,
  ...over,
});

describe("reviewKeyAction", () => {
  it("grades while a grade button holds focus", () => {
    // The regression. Revealing a card moves focus to a grade button, so if
    // BUTTON is treated as off-limits the number keys are dead exactly when
    // the grade row is on screen.
    expect(reviewKeyAction("3", revealedWithGradeButtonFocused())).toEqual({
      kind: "grade",
      rating: 3,
    });
    for (const key of ["1", "2", "3", "4"] as const) {
      expect(reviewKeyAction(key, revealedWithGradeButtonFocused())).not.toBeNull();
    }
  });

  it("refuses to grade a card that has not been revealed", () => {
    // Grading blind would record a rating for an answer nobody saw.
    expect(reviewKeyAction("3", hiddenCard())).toBeNull();
  });

  it("reveals on Space or Enter", () => {
    expect(reviewKeyAction(" ", hiddenCard())).toEqual({ kind: "reveal" });
    expect(reviewKeyAction("Enter", hiddenCard())).toEqual({ kind: "reveal" });
  });

  it("leaves Space and Enter to a focused button", () => {
    // The button activates natively; handling it here too would reveal twice.
    expect(reviewKeyAction(" ", hiddenCard({ targetTag: "BUTTON" }))).toBeNull();
  });

  it("never hijacks typing", () => {
    for (const tag of ["INPUT", "TEXTAREA", "SELECT", "AUDIO"]) {
      expect(reviewKeyAction("3", revealedWithGradeButtonFocused({ targetTag: tag }))).toBeNull();
      expect(reviewKeyAction(" ", hiddenCard({ targetTag: tag }))).toBeNull();
    }
    expect(
      reviewKeyAction("3", revealedWithGradeButtonFocused({ isContentEditable: true })),
    ).toBeNull();
  });

  it("ignores keys during IME composition", () => {
    // Mid-composition a digit is part of a candidate selection, not a grade.
    expect(reviewKeyAction("3", revealedWithGradeButtonFocused({ isComposing: true }))).toBeNull();
  });

  it("does nothing when no card is loaded", () => {
    expect(reviewKeyAction("3", revealedWithGradeButtonFocused({ hasCard: false }))).toBeNull();
    expect(reviewKeyAction(" ", hiddenCard({ hasCard: false }))).toBeNull();
  });

  it("ignores unrelated keys", () => {
    expect(reviewKeyAction("5", revealedWithGradeButtonFocused())).toBeNull();
    expect(reviewKeyAction("a", revealedWithGradeButtonFocused())).toBeNull();
    expect(reviewKeyAction("Tab", hiddenCard())).toBeNull();
  });
});
