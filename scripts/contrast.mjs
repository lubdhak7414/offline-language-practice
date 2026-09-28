#!/usr/bin/env node
// WCAG 2.x contrast check for the colour tokens in src/styles/tokens.css.
//
// jsdom cannot compute colour, so axe's colour-contrast rule is switched off
// in component tests (src/test/axe.ts). This script is what stands in for it:
// it reads the tokens, builds the light and dark palettes the way the
// stylesheet does, and checks every foreground/background pair the app
// actually draws.
//
//   node scripts/contrast.mjs        prints the table, exits 1 on any failure
//
// No dependencies.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

/** WCAG 1.4.3 body text. */
export const TEXT = 4.5;
/** WCAG 1.4.3 large text, and 1.4.11 UI components and graphics. */
export const UI = 3;

/**
 * Every pair the stylesheet draws. `min` is TEXT for anything read as text,
 * UI for a boundary, bar, or focus ring that only has to be seen.
 */
export const PAIRS = [
  // Body and secondary text on each surface.
  ...["bg", "bg-raised", "bg-sunken"].flatMap((bg) => [
    { fg: "fg", bg, min: TEXT, use: "body text" },
    { fg: "fg-muted", bg, min: TEXT, use: "muted text" },
  ]),

  // Links and the current nav item.
  { fg: "accent", bg: "bg", min: TEXT, use: "accent text" },
  { fg: "accent", bg: "bg-raised", min: TEXT, use: "current nav item" },

  // Status text: notices, word marks, flags.
  ...["danger", "warning", "success"].flatMap((fg) =>
    ["bg", "bg-raised", "bg-sunken"].map((bg) => ({ fg, bg, min: TEXT, use: "status text" })),
  ),

  // Solid buttons: primary, pressed segment, recording.
  { fg: "fg-on-accent", bg: "accent", min: TEXT, use: "primary button" },
  { fg: "fg-on-accent", bg: "accent-hover", min: TEXT, use: "primary button, hover" },
  { fg: "fg-on-accent", bg: "danger", min: TEXT, use: "recording button" },

  // Grade buttons draw their label in the grade colour on the button face
  // (bg-raised) and on its hover face (bg-sunken).
  ...["again", "hard", "good", "easy"].flatMap((g) =>
    ["bg-raised", "bg-sunken"].map((bg) => ({
      fg: `grade-${g}`,
      bg,
      min: TEXT,
      use: "grade button label",
    })),
  ),

  // Non-text: chart marks, focus ring, meter and level bars on their track.
  { fg: "accent", bg: "bg", min: UI, use: "chart mark, focus ring" },
  { fg: "accent", bg: "bg-raised", min: UI, use: "chart mark, focus ring" },
  ...["success", "warning", "danger"].map((fg) => ({
    fg,
    bg: "bg-sunken",
    min: UI,
    use: "meter bar on its track",
  })),

  // The outline is the only thing that shows where an input or a select is.
  { fg: "border-strong", bg: "bg", min: UI, use: "control boundary" },
  { fg: "border-strong", bg: "bg-raised", min: UI, use: "control boundary" },
];

/** "#rgb" or "#rrggbb" to [r, g, b] in 0-255, or null if it is not a hex colour. */
export function parseHex(value) {
  const m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(value.trim());
  if (!m) return null;
  let h = m[1];
  if (h.length === 3) h = h.replace(/./g, (c) => c + c);
  return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16));
}

function channel(v) {
  const s = v / 255;
  return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
}

/** WCAG relative luminance. */
export function luminance([r, g, b]) {
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

/** WCAG contrast ratio, 1 to 21, order of the two colours does not matter. */
export function contrastRatio(a, b) {
  const la = luminance(parseHex(a) ?? fail(a));
  const lb = luminance(parseHex(b) ?? fail(b));
  const hi = Math.max(la, lb);
  const lo = Math.min(la, lb);
  return (hi + 0.05) / (lo + 0.05);
}

function fail(value) {
  throw new Error(`not a hex colour: ${value}`);
}

function declarations(body) {
  const out = {};
  for (const m of body.matchAll(/--([\w-]+)\s*:\s*([^;]+);/g)) out[m[1]] = m[2].trim();
  return out;
}

/**
 * Reads the three palettes out of tokens.css: the light defaults, the dark
 * overrides applied by `prefers-color-scheme`, and the dark overrides applied
 * by `[data-theme="dark"]`. Dark is returned merged over light, because that
 * is what the browser resolves: a token dark does not mention stays light.
 */
export function parseTokens(css) {
  const bare = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const light = /^\s*:root\s*\{([^}]*)\}/m.exec(bare);
  const media = /:root:not\(\[data-theme="light"\]\)\s*\{([^}]*)\}/.exec(bare);
  const forced = /:root\[data-theme="dark"\]\s*\{([^}]*)\}/.exec(bare);
  if (!light || !media || !forced) throw new Error("tokens.css: a palette block is missing");
  const base = declarations(light[1]);
  const darkOverrides = declarations(media[1]);
  const forcedOverrides = declarations(forced[1]);
  return {
    light: base,
    dark: { ...base, ...darkOverrides },
    darkForced: { ...base, ...forcedOverrides },
    darkOverrides,
    forcedOverrides,
  };
}

/** Every pair in every theme, with its ratio and whether it passes. */
export function checkAll(tokens) {
  const rows = [];
  for (const theme of ["light", "dark"]) {
    const palette = tokens[theme];
    for (const p of PAIRS) {
      const fg = palette[p.fg];
      const bg = palette[p.bg];
      if (fg === undefined || bg === undefined) {
        throw new Error(`tokens.css: ${theme} has no --${fg === undefined ? p.fg : p.bg}`);
      }
      const ratio = contrastRatio(fg, bg);
      rows.push({ theme, ...p, fgHex: fg, bgHex: bg, ratio, pass: ratio >= p.min });
    }
  }
  return rows;
}

export function formatTable(rows) {
  const lines = ["theme  ratio  need  result  pair (use)"];
  for (const r of rows) {
    lines.push(
      [
        r.theme.padEnd(5),
        r.ratio.toFixed(2).padStart(6),
        String(r.min).padStart(5),
        (r.pass ? "ok" : "FAIL").padStart(6),
        ` --${r.fg} ${r.fgHex} on --${r.bg} ${r.bgHex} (${r.use})`,
      ].join("  "),
    );
  }
  return lines.join("\n");
}

const TOKENS_PATH = new URL("../src/styles/tokens.css", import.meta.url);

export function loadTokens() {
  return parseTokens(readFileSync(TOKENS_PATH, "utf8"));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const rows = checkAll(loadTokens());
  console.log(formatTable(rows));
  const failed = rows.filter((r) => !r.pass);
  console.log(`\n${rows.length - failed.length} of ${rows.length} pairs pass.`);
  process.exit(failed.length === 0 ? 0 : 1);
}
