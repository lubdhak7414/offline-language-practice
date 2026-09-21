import type { Rating } from "../ipc/types";

/**
 * Narrow arbitrary input to a valid grade.
 *
 * Grades reach us as `data-grade` attributes and keystrokes — both strings —
 * and the backend rejects anything outside 1..=4. Parsing in one place means
 * a typo'd attribute is `null` here rather than a rejected command later.
 */
export function parseRating(value: unknown): Rating | null {
  const n = Number(value);
  return n === 1 || n === 2 || n === 3 || n === 4 ? n : null;
}
