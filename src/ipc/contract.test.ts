import { describe, expect, it } from "vitest";

import { normalizeLint, setIpc, ipc, tauriIpc } from "./commands";
import { createMockIpc, sanitizePrefs } from "./mock";
import type { DownloadEvent, Ipc } from "./types";

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
        "cancelDownloads",
        "createDeck",
        "deleteCard",
        "deleteDeck",
        "downloadModels",
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
        "listModelCatalog",
        "listTags",
        "listVoices",
        "modelStatus",
        "modelsDir",
        "nextPrompt",
        "optimizeParameters",
        "pauseDownloads",
        "pickOpenPath",
        "pickSavePath",
        "recentReviews",
        "renameDeck",
        "restoreDatabase",
        "resumeDownloads",
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

// The mock is a contract double: where the backend validates, clamps, defaults
// or refuses, the mock must too, or tests against it cannot fail. Each case
// here names the Rust it mirrors.
describe("mock fidelity", () => {
  it("buryCard with no hours uses the saved preference (lib.rs bury_card)", async () => {
    const mock = createMockIpc({ cards: [{ id: "c1", deck_id: "default", front: "F", back: "B" }] });
    const { bury_hours } = await mock.getPreferences();
    const before = Math.floor(Date.now() / 1000);
    const until = await mock.buryCard("c1");
    expect(until).toBeGreaterThanOrEqual(before + bury_hours * 3600);
    expect(until).toBeLessThanOrEqual(Math.floor(Date.now() / 1000) + bury_hours * 3600);
    expect(await mock.buryCard("c1", 0)).toBe(0);
    expect(await mock.buryCard("c1", -5)).toBe(0);
  });

  it("setPreferences returns the sanitized value (prefs.rs sanitize)", async () => {
    const mock = createMockIpc();
    const prefs = await mock.getPreferences();
    const saved = await mock.setPreferences({
      ...prefs,
      theme: "purple",
      dialect: "klingon",
      goal: "fame",
      day_cutoff_hour: 99,
      new_per_day: -5,
      review_per_day: 100_000,
      bury_hours: 999,
    });
    expect(saved).toMatchObject({
      theme: "system",
      dialect: "american",
      goal: "both",
      day_cutoff_hour: 23,
      new_per_day: 0,
      review_per_day: 9999,
      bury_hours: 168,
    });
    expect(await mock.getPreferences()).toEqual(saved);
  });

  it("sanitizePrefs leaves valid values alone", async () => {
    const prefs = await createMockIpc().getPreferences();
    expect(sanitizePrefs(prefs)).toEqual(prefs);
  });

  it("a pause mid-download holds it until resume (download.rs Control)", async () => {
    const mock = createMockIpc();
    const events: DownloadEvent[] = [];
    let paused = false;
    const run = mock.downloadModels([], (e) => {
      events.push(e);
      if (e.kind === "progress" && !paused) {
        paused = true;
        void mock.pauseDownloads();
      }
    });
    for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
    const held = events.length;
    expect(events.some((e) => e.kind === "done")).toBe(false);
    expect(events[held - 1]?.kind).toBe("progress");

    await mock.resumeDownloads();
    await run;
    expect(events.length).toBeGreaterThan(held);
    expect(events[events.length - 1]?.kind).toBe("done");
  });

  it("a pause while idle is discarded, as begin() resets to running", async () => {
    const mock = createMockIpc();
    await mock.pauseDownloads();
    const events: DownloadEvent[] = [];
    await mock.downloadModels([], (e) => events.push(e));
    expect(events[events.length - 1]?.kind).toBe("done");
  });

  it("cancel wakes a paused download so it can stop", async () => {
    const mock = createMockIpc();
    const events: DownloadEvent[] = [];
    const run = mock.downloadModels([], (e) => {
      events.push(e);
      if (e.kind === "started") void mock.pauseDownloads();
    });
    for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
    await mock.cancelDownloads();
    await expect(run).rejects.toThrow("download cancelled");
    expect(events[events.length - 1]?.kind).toBe("cancelled");
    expect((await mock.listModelCatalog()).every((g) => !g.installed)).toBe(true);
  });

  it("refuses a second download while one is running", async () => {
    const mock = createMockIpc();
    let paused = false;
    const first = mock.downloadModels([], (e) => {
      // Once only: pausing on every "started" would re-pause the next model
      // after the resume below, and `first` would never settle.
      if (e.kind === "started" && !paused) {
        paused = true;
        void mock.pauseDownloads();
      }
    });
    for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
    await expect(mock.downloadModels([], () => {})).rejects.toBe("a download is already running");
    await mock.resumeDownloads();
    await first;
  });
});
