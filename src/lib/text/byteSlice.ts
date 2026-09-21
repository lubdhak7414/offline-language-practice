/**
 * The backend reports lint spans as UTF-8 **byte** offsets; JS string indices
 * are UTF-16 code units. Slicing with the raw numbers silently highlights the
 * wrong characters the moment a line contains an emoji, an accent, or CJK —
 * and looks perfectly fine in every ASCII test.
 */

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: false });

/**
 * Slice `text` by UTF-8 byte offsets, clamped into range.
 *
 * Offsets that land mid-character decode to U+FFFD rather than throwing:
 * a mangled character is a better failure than a crashed panel.
 */
export function byteSlice(text: string, start: number, end: number): string {
  const bytes = encoder.encode(text);
  const s = clamp(start, 0, bytes.length);
  const e = clamp(end, s, bytes.length);
  return decoder.decode(bytes.slice(s, e));
}

/** Length of `text` in UTF-8 bytes — the unit the backend counts in. */
export function byteLength(text: string): number {
  return encoder.encode(text).length;
}

function clamp(v: number, lo: number, hi: number): number {
  if (!Number.isFinite(v)) return lo;
  return Math.max(lo, Math.min(Math.trunc(v), hi));
}
