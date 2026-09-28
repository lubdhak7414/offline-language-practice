import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createClipPlayer } from "./player";

let created: string[];
let revoked: string[];

beforeEach(() => {
  created = [];
  revoked = [];
  let n = 0;
  vi.stubGlobal("URL", {
    ...URL,
    createObjectURL: () => {
      const u = `blob:clip-${++n}`;
      created.push(u);
      return u;
    },
    revokeObjectURL: (u: string) => revoked.push(u),
  });
  vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue(undefined);
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("createClipPlayer", () => {
  it("releases the previous clip when a new one starts", async () => {
    const p = createClipPlayer();
    await p.play([new Uint8Array(4)]);
    await p.play([new Uint8Array(4)]);
    expect(created).toEqual(["blob:clip-1", "blob:clip-2"]);
    expect(revoked).toEqual(["blob:clip-1"]);
    p.stop();
    expect(revoked).toEqual(["blob:clip-1", "blob:clip-2"]);
  });

  it("releases a clip when it finishes", async () => {
    const p = createClipPlayer();
    const audios: HTMLMediaElement[] = [];
    vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(function (this: HTMLMediaElement) {
      audios.push(this);
      return Promise.resolve();
    });
    await p.play([new Uint8Array(4)]);
    audios[0]!.dispatchEvent(new Event("ended"));
    expect(revoked).toEqual(["blob:clip-1"]);
  });

  it("releases a clip that fails to play, and reports the failure", async () => {
    vi.spyOn(HTMLMediaElement.prototype, "play").mockRejectedValue(new Error("NotAllowedError"));
    const p = createClipPlayer();
    await expect(p.play([new Uint8Array(4)])).rejects.toThrow("NotAllowedError");
    expect(revoked).toEqual(["blob:clip-1"]);
  });
});
