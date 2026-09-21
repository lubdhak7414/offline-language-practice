/**
 * Microphone capture runs at whatever the device offers (usually 44.1 or
 * 48 kHz); the ASR graph only accepts 16 kHz. Resampling is done with
 * `OfflineAudioContext` so the browser's own sample-rate converter does the
 * work rather than a hand-rolled one.
 */

/** Sample rate the ASR model requires. Mirrors `asr::ASR_SAMPLE_RATE`. */
export const TARGET_SAMPLE_RATE = 16000;

/** Number of output frames `fromRate` audio of this length becomes at 16 kHz. */
export function resampledFrameCount(inputLength: number, fromRate: number): number {
  if (inputLength === 0) return 0;
  if (!Number.isFinite(fromRate) || fromRate <= 0) return inputLength;
  return Math.max(1, Math.round((inputLength / fromRate) * TARGET_SAMPLE_RATE));
}

/** Flatten captured chunks into one contiguous mono buffer. */
export function concatChunks(chunks: ReadonlyArray<Float32Array>): Float32Array {
  const total = chunks.reduce((n, c) => n + c.length, 0);
  const out = new Float32Array(total);
  let offset = 0;
  for (const c of chunks) {
    out.set(c, offset);
    offset += c.length;
  }
  return out;
}

export async function resampleTo16k(
  mono: Float32Array,
  fromRate: number,
): Promise<Float32Array> {
  if (fromRate === TARGET_SAMPLE_RATE || mono.length === 0) return mono;
  const frames = resampledFrameCount(mono.length, fromRate);
  const offline = new OfflineAudioContext(1, frames, TARGET_SAMPLE_RATE);
  const buf = offline.createBuffer(1, mono.length, fromRate);
  buf.getChannelData(0).set(mono);
  const src = offline.createBufferSource();
  src.buffer = buf;
  src.connect(offline.destination);
  src.start(0);
  const rendered = await offline.startRendering();
  return rendered.getChannelData(0).slice();
}
