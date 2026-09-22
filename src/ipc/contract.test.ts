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
        "backupDatabase",
        "buryCard",
        "createDeck",
        "deleteCard",
        "deleteDeck",
        "dueCards",
        "endSession",
        "epReport",
        "exportData",
        "getDailyLimits",
        "getPreferences",
        "getRetention",
        "getVoice",
        "gradeCard",
        "importData",
        "lintText",
        "listAttempts",
        "listCards",
        "listDecks",
        "listTags",
        "listVoices",
        "modelStatus",
        "nextPrompt",
        "optimizeParameters",
        "pickOpenPath",
        "pickSavePath",
        "recentReviews",
        "renameDeck",
        "restoreDatabase",
        "reviewStats",
        "scoreAttempt",
        "seedDemoDeck",
        "seedPrompts",
        "setCardTags",
        "setDailyLimits",
        "setPreferences",
        "setRetention",
        "setVoice",
        "startSession",
        "statsDaily",
        "statsForecast",
        "statsOverview",
        "statsRetention",
        "suspendCard",
        "synthesizeSpeech",
        "transcribePcm",
        "undoReview",
        "updateCard",
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
    // An empty seed, not the default fixture deck: this test is about call
    // recording and persistence, not the realistic sample data.
    const mock = createMockIpc({ decks: [{ id: "default", name: "Default" }], cards: [] });
    const id = await mock.addCard("default", "front", "back");
    const cards = await mock.listCards("default");
    expect(cards).toEqual([
      { id, deck_id: "default", front: "front", back: "back", tags: [], suspended: false, buried_until: 0 },
    ]);
    expect(mock.calls.map((c) => c.name)).toEqual(["addCard", "listCards"]);
  });

  it("can be told to fail a specific command", async () => {
    const mock = createMockIpc({ fail: { modelStatus: new Error("nope") } });
    await expect(mock.modelStatus()).rejects.toThrow("nope");
  });
});
