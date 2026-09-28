/**
 * Plain-language reading of a recording's peak level (0..1).
 *
 * Thresholds come from real learner recordings: in a 600-file sample of
 * speechocean762 (5,245 phone recordings) the quietest 1% peaked at 0.086,
 * the median at 0.44, and none reached 0.99. So a peak under 0.08 is quieter
 * than almost any recording the scorer was calibrated on, and 0.99 is the
 * flat top of the range, where the waveform is cut off.
 */
export const QUIET_BELOW = 0.08;
export const NOTHING_BELOW = 0.01;
export const CLIPPING_AT = 0.99;

export type MicLevel = "nothing" | "quiet" | "ok" | "clipping";

export function classifyPeak(peak: number): MicLevel {
  if (peak < NOTHING_BELOW) return "nothing";
  if (peak < QUIET_BELOW) return "quiet";
  if (peak >= CLIPPING_AT) return "clipping";
  return "ok";
}

/** A sentence about the level, or null when there is nothing to say. */
export function describeMicLevel(peak: number): string | null {
  switch (classifyPeak(peak)) {
    case "nothing":
      return "No sound came through. Check that the right microphone is selected and not muted.";
    case "quiet":
      return "That was quiet, so the score may be less reliable. Move closer to the microphone or raise its input volume.";
    case "clipping":
      return "That was too loud and the sound was cut off, so the score may be less reliable. Move back or lower the input volume.";
    case "ok":
      return null;
  }
}
