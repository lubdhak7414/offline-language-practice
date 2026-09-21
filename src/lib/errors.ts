/**
 * Backend errors arrive as strings carrying a machine-readable prefix
 * (`ASR_SILENCE:`, `TTS_BUSY:`, …). This turns them into something a learner
 * can act on, and is the single place that knows those prefixes.
 */

/** Prefix → what the person should actually do about it. */
const ASR_MESSAGES: ReadonlyArray<readonly [string, string]> = [
  ["ASR_SILENCE:", "No speech detected — move closer to the mic and try again."],
  ["ASR_SHAPE:", "Audio could not be read — please re-record."],
  ["ASR_NO_VOCAB:", "The speech model is incomplete — reinstall it from Settings."],
  ["ASR_NO_MODEL:", "The speech model is not installed yet — install it from Settings."],
];

export function friendlyAsrError(e: unknown): string {
  const s = String(e);
  for (const [prefix, message] of ASR_MESSAGES) {
    if (s.includes(prefix)) return message;
  }
  return `Speech recognition is unavailable: ${s}`;
}

/** True when the TTS worker rejected the request because it was already busy. */
export function isTtsBusy(e: unknown): boolean {
  return String(e).includes("TTS_BUSY:");
}

export function friendlyTtsError(e: unknown): string {
  if (isTtsBusy(e)) return "Still speaking — wait a moment and try again.";
  return `Speech playback is unavailable: ${String(e)}`;
}
