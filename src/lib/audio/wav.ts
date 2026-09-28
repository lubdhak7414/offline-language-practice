/**
 * 16 kHz f32 PCM (the recorder's output) to a playable WAV file.
 *
 * Only for "Play my recording": the clip lives in memory for as long as the
 * feedback is on screen and is never written anywhere.
 */

const HEADER_BYTES = 44;

/**
 * `pcm` is little-endian f32 samples, as `Recording.pcm` holds them. Returns
 * a mono 16-bit PCM WAV. Samples outside [-1, 1] are clipped, not wrapped.
 */
export function f32ToWav(pcm: Uint8Array, sampleRate: number): Uint8Array<ArrayBuffer> {
  const samples = Math.floor(pcm.byteLength / 4);
  const dataBytes = samples * 2;
  const out = new Uint8Array(HEADER_BYTES + dataBytes);
  const v = new DataView(out.buffer);
  const ascii = (at: number, s: string) => {
    for (let i = 0; i < s.length; i++) v.setUint8(at + i, s.charCodeAt(i));
  };
  ascii(0, "RIFF");
  v.setUint32(4, 36 + dataBytes, true);
  ascii(8, "WAVE");
  ascii(12, "fmt ");
  v.setUint32(16, 16, true); // fmt chunk size
  v.setUint16(20, 1, true); // PCM
  v.setUint16(22, 1, true); // mono
  v.setUint32(24, sampleRate, true);
  v.setUint32(28, sampleRate * 2, true); // byte rate
  v.setUint16(32, 2, true); // block align
  v.setUint16(34, 16, true); // bits per sample
  ascii(36, "data");
  v.setUint32(40, dataBytes, true);

  // A DataView, not a Float32Array over the buffer: the recording's bytes
  // need not start on a 4-byte boundary.
  const src = new DataView(pcm.buffer, pcm.byteOffset, samples * 4);
  for (let i = 0; i < samples; i++) {
    const x = Math.max(-1, Math.min(1, src.getFloat32(i * 4, true)));
    v.setInt16(HEADER_BYTES + i * 2, x < 0 ? Math.round(x * 0x8000) : Math.round(x * 0x7fff), true);
  }
  return out;
}
