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
  /** `deviceId` is a browser microphone id; empty or absent means the system default. */
  start(onLevel?: (peak: number) => void, deviceId?: string): Promise<void>;
  stop(): Promise<Recording>;
  cancel(): void;
  isRecording(): boolean;
};

/** Longest single recording, matching the backend's cap. */
export const MAX_RECORDING_MS = 120_000;

/**
 * Capture runs in an AudioWorklet: ScriptProcessorNode is deprecated and does
 * its work on the main thread, where a busy render can drop audio. The
 * processor is loaded from a blob URL (the CSP allows `blob:` for workers) and
 * posts each 128-frame block to the page. Where a worklet cannot be had the
 * recorder falls back to the ScriptProcessor, so an older WebView still records.
 */
const WORKLET_SOURCE = `
class OlpCapture extends AudioWorkletProcessor {
  process(inputs) {
    const channel = inputs[0] && inputs[0][0];
    if (channel) this.port.postMessage(channel.slice(0));
    return true;
  }
}
registerProcessor("olp-capture", OlpCapture);
`;

/** How long a worklet may stay silent before the ScriptProcessor takes over. */
export const WORKLET_WATCHDOG_MS = 1500;

type Capture = { node: AudioNode; detach(): void };

async function captureWithWorklet(
  ctx: AudioContext,
  push: (block: Float32Array) => void,
): Promise<Capture> {
  if (!ctx.audioWorklet) throw new Error("AudioWorklet is not available");
  const url = URL.createObjectURL(new Blob([WORKLET_SOURCE], { type: "text/javascript" }));
  try {
    await ctx.audioWorklet.addModule(url);
  } finally {
    URL.revokeObjectURL(url);
  }
  const node = new AudioWorkletNode(ctx, "olp-capture", { numberOfInputs: 1, numberOfOutputs: 1 });
  node.port.onmessage = (e: MessageEvent<Float32Array>) => push(e.data);
  return {
    node,
    detach() {
      node.port.onmessage = null;
    },
  };
}

function captureWithScriptProcessor(
  ctx: AudioContext,
  push: (block: Float32Array) => void,
): Capture {
  const node = ctx.createScriptProcessor(4096, 1, 1);
  node.onaudioprocess = (e) => push(new Float32Array(e.inputBuffer.getChannelData(0)));
  return {
    node,
    detach() {
      node.onaudioprocess = null;
    },
  };
}

export function createRecorder(): Recorder {
  let stream: MediaStream | null = null;
  let ctx: AudioContext | null = null;
  let node: AudioNode | null = null;
  let detach: (() => void) | null = null;
  let watchdog: ReturnType<typeof setTimeout> | null = null;
  let sink: GainNode | null = null;
  let chunks: Float32Array[] = [];
  let startedAt = 0;
  let capturedRate = TARGET_SAMPLE_RATE;

  function contextClosed(): boolean {
    return ctx === null;
  }

  function teardown() {
    if (watchdog) clearTimeout(watchdog);
    watchdog = null;
    detach?.();
    detach = null;
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

    async start(onLevel, deviceId) {
      if (ctx) return;
      chunks = [];
      stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          // `ideal`, not `exact`: a device that cannot do 16 kHz should
          // still record, and the resampler handles the difference. The same
          // goes for the chosen microphone: if it has been unplugged, the
          // default is used rather than failing the recording.
          sampleRate: { ideal: TARGET_SAMPLE_RATE },
          channelCount: { ideal: 1 },
          echoCancellation: true,
          ...(deviceId ? { deviceId: { ideal: deviceId } } : {}),
        },
      });
      ctx = new AudioContext();
      capturedRate = ctx.sampleRate;
      const source = ctx.createMediaStreamSource(stream);
      let gotBlock = false;
      const push = (data: Float32Array) => {
        gotBlock = true;
        chunks.push(data);
        if (onLevel) {
          let peak = 0;
          for (const v of data) peak = Math.max(peak, Math.abs(v));
          onLevel(peak);
        }
      };
      let capture: Capture;
      let viaWorklet = false;
      try {
        capture = await captureWithWorklet(ctx, push);
        viaWorklet = true;
      } catch (e) {
        console.warn("AudioWorklet capture unavailable, using ScriptProcessor:", e);
        capture = captureWithScriptProcessor(ctx, push);
      }
      // A cancel while the worklet was loading has already closed the context.
      if (contextClosed()) {
        capture.detach();
        return;
      }
      node = capture.node;
      detach = capture.detach;
      // A ScriptProcessor only runs while connected to the destination, but
      // routing the mic to the speakers would feed back, so it goes through
      // a silent gain node.
      sink = ctx.createGain();
      sink.gain.value = 0;
      source.connect(node);
      node.connect(sink);
      sink.connect(ctx.destination);
      startedAt = Date.now();

      // A worklet that loaded but never delivers audio would record silence
      // and look like a broken microphone. Real ones deliver within a few
      // milliseconds; if none has by the deadline, swap in the ScriptProcessor.
      if (viaWorklet) {
        const liveCtx = ctx;
        watchdog = setTimeout(() => {
          watchdog = null;
          if (gotBlock || contextClosed() || liveCtx !== ctx) return;
          console.warn("AudioWorklet delivered no audio, switching to ScriptProcessor");
          detach?.();
          node?.disconnect();
          const fallback = captureWithScriptProcessor(liveCtx, push);
          node = fallback.node;
          detach = fallback.detach;
          source.connect(node);
          node.connect(sink as GainNode);
        }, WORKLET_WATCHDOG_MS);
      }
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
