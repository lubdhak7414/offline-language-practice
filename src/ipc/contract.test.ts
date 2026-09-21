import { describe, expect, it } from "vitest";

import { normalizeLint, setIpc, ipc, tauriIpc } from "./commands";
import { createMockIpc } from "./mock";
import type { Ipc } from "./types";

describe("lint normalization", () => {
  const diag = { start: 0, end: 3, message: "m", suggestions: [] };

  it("accepts the current {diags, truncated} shape", () => {
    expect(normalizeLint({ diags: [diag], truncated: true })).toEqual({
      diags: [diag],
      truncated: true,
    });
  });

  it("accepts the legacy bare array", () => {
    // Older backends returned a bare array. Every route downstream sees one
    // shape, so this compatibility wart lives here and nowhere else.
    expect(normalizeLint([diag])).toEqual({ diags: [diag], truncated: false });
  });

  it("tolerates a missing diags field", () => {
    expect(
      normalizeLint({ truncated: false } as unknown as { diags: []; truncated: boolean }),
    ).toEqual({ diags: [], truncated: false });
  });
});

describe("Ipc implementations", () => {
  // Both satisfy `Ipc` structurally, so a command added to the interface
  // without a mock counterpart is a compile error, not a runtime surprise.
  const impls: Array<[string, Ipc]> = [
    ["tauriIpc", tauriIpc],
    ["mock", createMockIpc()],
  ];

  const methods = Object.keys(tauriIpc) as Array<keyof Ipc>;

  it.each(impls)("%s implements every command", (_name, impl) => {
    for (const m of methods) {
      expect(typeof impl[m]).toBe("function");
    }
  });

  it("exposes exactly the commands the backend registers", () => {
    expect(methods.sort()).toEqual(
      [
        "addCard",
        "deleteCard",
        "dueCards",
        "epReport",
        "getRetention",
        "gradeCard",
        "lintText",
        "listCards",
        "listDecks",
        "listVoices",
        "modelStatus",
        "optimizeParameters",
        "recentReviews",
        "reviewStats",
        "seedDemoDeck",
        "setRetention",
        "synthesizeSpeech",
        "transcribePcm",
      ].sort(),
    );
  });
});

describe("setIpc", () => {
  it("swaps the backend and restores it", () => {
    const mock = createMockIpc();
    const restore = setIpc(mock);
    expect(ipc()).toBe(mock);
    restore();
    expect(ipc()).toBe(tauriIpc);
  });
});

describe("mock backend", () => {
  it("records calls and keeps state across them", async () => {
    const mock = createMockIpc();
    const id = await mock.addCard("default", "front", "back");
    const cards = await mock.listCards("default");
    expect(cards).toEqual([{ id, deck_id: "default", front: "front", back: "back" }]);
    expect(mock.calls.map((c) => c.name)).toEqual(["addCard", "listCards"]);
  });

  it("can be told to fail a specific command", async () => {
    const mock = createMockIpc({ fail: { modelStatus: new Error("nope") } });
    await expect(mock.modelStatus()).rejects.toThrow("nope");
  });
});
