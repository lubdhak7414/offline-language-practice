import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createRecorder, WORKLET_WATCHDOG_MS } from "./recorder";

type Port = { onmessage: ((e: { data: Float32Array }) => void) | null };

/** Just enough Web Audio for the recorder to run its two capture paths. */
function installFakes(opts: { worklet: boolean }) {
  const ports: Port[] = [];
  const scripts: Array<{ onaudioprocess: ((e: unknown) => void) | null }> = [];
  const constraints: unknown[] = [];

  class FakeNode {
    connect() {}
    disconnect() {}
  }
  class FakeWorkletNode extends FakeNode {
    port: Port = { onmessage: null };
    constructor() {
      super();
      ports.push(this.port);
    }
  }
  class FakeContext {
    sampleRate = 16000;
    audioWorklet = opts.worklet ? { addModule: async () => {} } : undefined;
    destination = new FakeNode();
    createMediaStreamSource() {
      return new FakeNode();
    }
    createGain() {
      return Object.assign(new FakeNode(), { gain: { value: 1 } });
    }
    createScriptProcessor() {
      const node = Object.assign(new FakeNode(), { onaudioprocess: null as ((e: unknown) => void) | null });
      scripts.push(node);
      return node;
    }
    async close() {}
  }
  vi.stubGlobal("AudioContext", FakeContext);
  vi.stubGlobal("AudioWorkletNode", FakeWorkletNode);
  vi.stubGlobal("navigator", {
    mediaDevices: {
      getUserMedia: async (c: unknown) => {
        constraints.push(c);
        return { getTracks: () => [{ stop() {} }] };
      },
    },
  });
  URL.createObjectURL = () => "blob:fake";
  URL.revokeObjectURL = () => {};
  return { ports, scripts, constraints };
}

describe("recorder capture", () => {
  beforeEach(() => vi.spyOn(console, "warn").mockImplementation(() => {}));
  afterEach(() => vi.unstubAllGlobals());

  it("records through an AudioWorklet and reports levels", async () => {
    const fake = installFakes({ worklet: true });
    const rec = createRecorder();
    const levels: number[] = [];
    await rec.start((p) => levels.push(p));
    expect(rec.isRecording()).toBe(true);
    expect(fake.scripts).toHaveLength(0);
    fake.ports[0]?.onmessage?.({ data: new Float32Array([0.1, -0.5, 0.2]) });
    fake.ports[0]?.onmessage?.({ data: new Float32Array([0.05]) });
    expect(levels).toHaveLength(2);
    expect(levels[0]).toBeCloseTo(0.5, 5);
    expect(levels[1]).toBeCloseTo(0.05, 5);
    const out = await rec.stop();
    expect(rec.isRecording()).toBe(false);
    expect(out.pcm.byteLength).toBe(4 * 4);
    expect(out.peak).toBeCloseTo(0.5, 5);
  });

  it("falls back to the ScriptProcessor when there is no worklet", async () => {
    const fake = installFakes({ worklet: false });
    const rec = createRecorder();
    await rec.start();
    expect(fake.ports).toHaveLength(0);
    fake.scripts[0]?.onaudioprocess?.({ inputBuffer: { getChannelData: () => new Float32Array([0.3, 0.4]) } });
    const out = await rec.stop();
    expect(out.pcm.byteLength).toBe(2 * 4);
  });

  it("switches to the ScriptProcessor when the worklet loads but delivers nothing", async () => {
    vi.useFakeTimers();
    try {
      const fake = installFakes({ worklet: true });
      const rec = createRecorder();
      await rec.start();
      expect(fake.scripts).toHaveLength(0);
      await vi.advanceTimersByTimeAsync(WORKLET_WATCHDOG_MS + 10);
      expect(fake.scripts).toHaveLength(1);
      fake.scripts[0]?.onaudioprocess?.({ inputBuffer: { getChannelData: () => new Float32Array([0.2]) } });
      const out = await rec.stop();
      expect(out.pcm.byteLength).toBe(4);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not switch when the worklet is delivering", async () => {
    vi.useFakeTimers();
    try {
      const fake = installFakes({ worklet: true });
      const rec = createRecorder();
      await rec.start();
      fake.ports[0]?.onmessage?.({ data: new Float32Array([0.2]) });
      await vi.advanceTimersByTimeAsync(WORKLET_WATCHDOG_MS + 10);
      expect(fake.scripts).toHaveLength(0);
      rec.cancel();
    } finally {
      vi.useRealTimers();
    }
  });

  it("asks for the chosen microphone as ideal, and for the default when none is chosen", async () => {
    const fake = installFakes({ worklet: true });
    const rec = createRecorder();
    await rec.start(undefined, "mic-42");
    rec.cancel();
    await rec.start(undefined, "");
    rec.cancel();
    const audio = fake.constraints.map((c) => (c as { audio: Record<string, unknown> }).audio);
    expect(audio[0]?.deviceId).toEqual({ ideal: "mic-42" });
    expect(audio[1]).not.toHaveProperty("deviceId");
  });
});
