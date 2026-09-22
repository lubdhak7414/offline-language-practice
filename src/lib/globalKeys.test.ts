import { describe, expect, it } from "vitest";

import { globalKeyAction, type GlobalKeyContext } from "./globalKeys";

const base: GlobalKeyContext = {
  targetTag: "BODY",
  isContentEditable: false,
  isComposing: false,
  hasModifier: false,
  helpOpen: false,
  pendingGo: false,
};

const ctx = (over: Partial<GlobalKeyContext> = {}): GlobalKeyContext => ({
  ...base,
  ...over,
});

describe("globalKeyAction", () => {
  it("opens the help sheet on ?", () => {
    expect(globalKeyAction("?", ctx())).toEqual({ kind: "help", open: true });
  });

  it("closes the help sheet on Escape only when it is open", () => {
    expect(globalKeyAction("Escape", ctx({ helpOpen: true }))).toEqual({
      kind: "help",
      open: false,
    });
    expect(globalKeyAction("Escape", ctx())).toBeNull();
  });

  it("arms the go prefix and then navigates", () => {
    expect(globalKeyAction("g", ctx())).toEqual({ kind: "go" });
    expect(globalKeyAction("r", ctx({ pendingGo: true }))).toEqual({
      kind: "navigate",
      route: "review",
    });
    expect(globalKeyAction("S", ctx({ pendingGo: true }))).toEqual({
      kind: "navigate",
      route: "progress",
    });
  });

  it("always consumes the prefix, even on a mistyped destination", () => {
    expect(globalKeyAction("q", ctx({ pendingGo: true }))).toEqual({
      kind: "cancel",
    });
  });

  it("keeps out of text entry", () => {
    for (const targetTag of ["INPUT", "TEXTAREA", "SELECT"]) {
      expect(globalKeyAction("?", ctx({ targetTag }))).toBeNull();
      expect(globalKeyAction("g", ctx({ targetTag }))).toBeNull();
    }
    expect(globalKeyAction("g", ctx({ isContentEditable: true }))).toBeNull();
    expect(globalKeyAction("g", ctx({ isComposing: true }))).toBeNull();
  });

  it("never takes a key the browser or OS has claimed", () => {
    expect(globalKeyAction("g", ctx({ hasModifier: true }))).toBeNull();
    expect(globalKeyAction("?", ctx({ hasModifier: true }))).toBeNull();
  });

  it("leaves keys it does not own alone, so routes still see them", () => {
    for (const key of ["3", " ", "Enter", "x"]) {
      expect(globalKeyAction(key, ctx())).toBeNull();
    }
  });
});
