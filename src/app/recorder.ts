/**
 * Microphone capture for the practice loop.
 *
 * Kept out of the components so a route can be tested without a MediaStream,
 * and so there is exactly one place that knows how to tear the graph down —
 * a leaked AudioContext keeps the microphone indicator lit after a session
 * ends, which looks like the app is still listening.
 */
import { concatChunks, resampleTo16k, TARGET_SAMPLE_RATE } from "../lib/audio/resample";

export type Recording = {
  /** 16 kHz mono PCM as little-endian f32 bytes, ready for the backend. */
  pcm: Uint8Array;
  durationMs: number;
  /** Peak absolute amplitude, for telling silence from a quiet room. */
  peak: number;
};

export type Recorder = {
  start(onLevel?: (peak: number) => void): Promise<void>;
  stop(): Promise<Recording>;
  cancel(): void;
  isRecording(): boolean;
};

/** Longest single recording, matching the backend's cap. */
export const MAX_RECORDING_MS = 120_000;

export function createRecorder(): Recorder {
  let stream: MediaStream | null = null;
  let ctx: AudioContext | null = null;
  let node: ScriptProcessorNode | null = null;
  let sink: GainNode | null = null;
  let chunks: Float32Array[] = [];
  let startedAt = 0;
  let capturedRate = TARGET_SAMPLE_RATE;

  function teardown() {
    node?.disconnect();
    sink?.disconnect();
    node = null;
    sink = null;
    stream?.getTracks().forEach((t) => t.stop());
    stream = null;
    void ctx?.close().catch(() => {});
    ctx = null;
  }

  return {
    isRecording: () => ctx !== null,

    async start(onLevel) {
      if (ctx) return;
      chunks = [];
      stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          // `ideal`, not `exact`: a device that cannot do 16 kHz should
          // still record, and the resampler handles the difference.
          sampleRate: { ideal: TARGET_SAMPLE_RATE },
          channelCount: { ideal: 1 },
          echoCancellation: true,
        },
      });
      ctx = new AudioContext();
      capturedRate = ctx.sampleRate;
      const source = ctx.createMediaStreamSource(stream);
      node = ctx.createScriptProcessor(4096, 1, 1);
      node.onaudioprocess = (e) => {
        const data = new Float32Array(e.inputBuffer.getChannelData(0));
        chunks.push(data);
        if (onLevel) {
          let peak = 0;
          for (const v of data) peak = Math.max(peak, Math.abs(v));
          onLevel(peak);
        }
      };
      // A ScriptProcessor only runs while connected to the destination, but
      // routing the mic to the speakers would feed back, so it goes through
      // a silent gain node.
      sink = ctx.createGain();
      sink.gain.value = 0;
      source.connect(node);
      node.connect(sink);
      sink.connect(ctx.destination);
      startedAt = Date.now();
    },

    async stop() {
      const mono = concatChunks(chunks);
      const rate = capturedRate;
      const elapsed = Date.now() - startedAt;
      teardown();
      chunks = [];
      const pcm16 = await resampleTo16k(mono, rate);
      let peak = 0;
      for (const v of pcm16) peak = Math.max(peak, Math.abs(v));
      return {
        pcm: new Uint8Array(pcm16.buffer, pcm16.byteOffset, pcm16.byteLength),
        durationMs: elapsed,
        peak,
      };
    },

    cancel() {
      teardown();
      chunks = [];
    },
  };
}
