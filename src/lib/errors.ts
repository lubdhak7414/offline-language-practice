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

/**
 * `getUserMedia` failures by DOMException name. The names are what browsers
 * and WebKitGTK/WebView2 actually throw; the message text is not stable.
 */
const MIC_MESSAGES: Readonly<Record<string, string>> = {
  NotAllowedError:
    "Microphone access was blocked — allow it for this app in your system settings, then try again.",
  SecurityError:
    "Microphone access was blocked — allow it for this app in your system settings, then try again.",
  NotFoundError: "No microphone found — plug one in and try again.",
  OverconstrainedError: "No microphone found — plug one in and try again.",
  NotReadableError: "The microphone is in use by another app — close it and try again.",
  AbortError: "The microphone is in use by another app — close it and try again.",
};

export function friendlyMicError(e: unknown): string {
  const name = typeof e === "object" && e !== null && "name" in e ? String(e.name) : "";
  return MIC_MESSAGES[name] ?? `Microphone unavailable: ${String(e)}`;
}
