// @vitest-environment node
import { describe, expect, it } from "vitest";

import {
  PAIRS,
  TEXT,
  UI,
  checkAll,
  contrastRatio,
  loadTokens,
  luminance,
  parseHex,
  parseTokens,
} from "./contrast.mjs";

describe("contrast maths", () => {
  it("parses long and short hex, and rejects anything else", () => {
    expect(parseHex("#ffffff")).toEqual([255, 255, 255]);
    expect(parseHex("#0f8")).toEqual([0, 255, 136]);
    expect(parseHex("rgb(1 2 3)")).toBeNull();
    expect(parseHex("var(--bg)")).toBeNull();
  });

  it("puts black at 0 and white at 1 luminance", () => {
    expect(luminance([0, 0, 0])).toBe(0);
    expect(luminance([255, 255, 255])).toBeCloseTo(1, 10);
  });

  it("gives the published WCAG figures", () => {
    expect(contrastRatio("#000000", "#ffffff")).toBeCloseTo(21, 5);
    expect(contrastRatio("#ffffff", "#ffffff")).toBeCloseTo(1, 5);
    // #777 on white is the textbook near-miss: 4.48:1, just under 4.5.
    expect(contrastRatio("#777777", "#ffffff")).toBeCloseTo(4.48, 2);
    expect(contrastRatio("#767676", "#ffffff")).toBeGreaterThanOrEqual(TEXT);
  });

  it("does not care which colour is the foreground", () => {
    expect(contrastRatio("#2d5bd7", "#fbfbfd")).toBe(contrastRatio("#fbfbfd", "#2d5bd7"));
  });
});

describe("tokens.css", () => {
  const css = `
    /* a comment with a } and --fake: #000000; in it */
    :root { --bg: #ffffff; --fg: #000000; --keep: #123456; }
    @media (prefers-color-scheme: dark) {
      :root:not([data-theme="light"]) { --bg: #000000; --fg: #ffffff; }
    }
    :root[data-theme="dark"] { --bg: #000000; --fg: #ffffff; }
  `;

  it("layers the dark overrides over the light defaults", () => {
    const t = parseTokens(css);
    expect(t.light).toMatchObject({ bg: "#ffffff", fg: "#000000", keep: "#123456" });
    expect(t.light).not.toHaveProperty("fake");
    expect(t.dark).toMatchObject({ bg: "#000000", fg: "#ffffff", keep: "#123456" });
    expect(t.darkForced).toEqual(t.dark);
  });

  it("refuses a file without all three palettes", () => {
    expect(() => parseTokens(":root { --bg: #fff; }")).toThrow(/palette block is missing/);
  });

  it("keeps the system-dark and forced-dark palettes identical", () => {
    // They are two copies of one palette; drift would make the app look
    // different depending on how dark mode was reached.
    const t = loadTokens();
    expect(t.darkOverrides).toEqual(t.forcedOverrides);
  });

  it("has every token the pairs use, in both themes", () => {
    const t = loadTokens();
    for (const theme of ["light", "dark"]) {
      for (const p of PAIRS) {
        expect(parseHex(t[theme][p.fg] ?? ""), `${theme} --${p.fg}`).not.toBeNull();
        expect(parseHex(t[theme][p.bg] ?? ""), `${theme} --${p.bg}`).not.toBeNull();
      }
    }
  });

  it("meets 4.5:1 for text and 3:1 for UI components in light and dark", () => {
    const failures = checkAll(loadTokens())
      .filter((r) => !r.pass)
      .map((r) => `${r.theme} --${r.fg} on --${r.bg} (${r.use}): ${r.ratio.toFixed(2)} < ${r.min}`);
    expect(failures).toEqual([]);
  });

  it("only asks for the two WCAG thresholds", () => {
    expect(new Set(PAIRS.map((p) => p.min))).toEqual(new Set([TEXT, UI]));
  });
});

describe("checkAll", () => {
  it("reports a pair that falls short", () => {
    const tokens = {
      light: Object.fromEntries(
        PAIRS.flatMap((p) => [
          [p.fg, "#777777"],
          [p.bg, "#ffffff"],
        ]),
      ),
    };
    tokens.dark = tokens.light;
    const rows = checkAll(tokens);
    const grey = rows.find((r) => r.fg === "fg" && r.bg === "bg");
    expect(grey?.pass).toBe(false);
    expect(grey?.ratio).toBeCloseTo(4.48, 2);
  });
});
