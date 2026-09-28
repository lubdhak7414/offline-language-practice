/**
 * One clip at a time, and no leaked object URLs.
 *
 * Every play replaces what was playing, and a clip's object URL is revoked
 * as soon as it ends, is replaced, or fails — a route that plays a prompt a
 * hundred times otherwise holds a hundred WAV blobs until the window closes.
 */
import { ipc } from "../../ipc/commands";

export type ClipPlayer = {
  /** Stop whatever is playing, then play `parts` as one WAV. */
  play(parts: BlobPart[]): Promise<void>;
  /** Stop and release the current clip, if any. */
  stop(): void;
};

export function createClipPlayer(): ClipPlayer {
  let audio: HTMLAudioElement | null = null;
  let url: string | null = null;

  function stop() {
    audio?.pause();
    audio = null;
    if (url !== null) URL.revokeObjectURL(url);
    url = null;
  }

  return {
    async play(parts) {
      stop();
      const mine = URL.createObjectURL(new Blob(parts, { type: "audio/wav" }));
      url = mine;
      const el = new Audio(mine);
      audio = el;
      el.addEventListener("ended", () => {
        if (url === mine) stop();
      });
      try {
        await el.play();
      } catch (e) {
        if (url === mine) stop();
        throw e;
      }
    },
    stop,
  };
}

/** Synthesize `text` with the selected voice and play it through `player`. */
export async function speak(
  text: string,
  player: ClipPlayer,
  opts?: { slow?: boolean },
): Promise<void> {
  const chunks: BlobPart[] = [];
  await ipc().synthesizeSpeech(text, (buf) => chunks.push(new Uint8Array(buf)), opts);
  await player.play(chunks);
}
