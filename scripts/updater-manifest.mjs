#!/usr/bin/env node
// Writes <dir>/latest.json for tauri-plugin-updater from a release draft's
// assets. Run by release.yml's `attest` job, once, after every build leg,
// over exactly what the draft holds; see SECURITY.md "Automatic updates".
//
//   node scripts/updater-manifest.mjs <dir> <owner/repo> <tag> <version>
//
// Node builtins only: the attest job has no `npm ci`.
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

/** Asset suffix -> updater platform keys. Order within `keys` does not matter;
 * the plugin tries `{os}-{arch}-{installer}` first, then `{os}-{arch}`.
 * .deb/.rpm are absent on purpose: the app never installs them itself. */
export const TARGETS = [
  { suffix: "_amd64.AppImage", keys: ["linux-x86_64-appimage", "linux-x86_64"] },
  { suffix: "_aarch64.app.tar.gz", keys: ["darwin-aarch64-app", "darwin-aarch64"] },
  // NSIS is the per-user install, so it is the bare Windows fallback: an MSI
  // offered to an NSIS install would add a second, per-machine copy.
  { suffix: "_x64-setup.exe", keys: ["windows-x86_64-nsis", "windows-x86_64"] },
  { suffix: "_x64_en-US.msi", keys: ["windows-x86_64-msi"] },
];

/**
 * @param {{ names: string[], readSig: (name: string) => string, repo: string,
 *           tag: string, version: string, pubDate: string }} args
 * @returns {{ manifest: { version: string, notes: string, pub_date: string,
 *             platforms: Record<string, { signature: string, url: string }> },
 *             missing: string[] }}
 */
export function buildManifest({ names, readSig, repo, tag, version, pubDate }) {
  /** @type {Record<string, { signature: string, url: string }>} */
  const platforms = {};
  const missing = [];
  for (const t of TARGETS) {
    const hits = names.filter((n) => n.endsWith(t.suffix));
    if (hits.length > 1) throw new Error(`more than one *${t.suffix}: ${hits.join(", ")}`);
    const name = hits[0];
    if (name === undefined) {
      missing.push(t.suffix);
      continue;
    }
    if (!names.includes(`${name}.sig`)) throw new Error(`${name} has no ${name}.sig`);
    const entry = {
      signature: readSig(`${name}.sig`).trim(),
      // Tag-based, so nothing resolves until the draft is published.
      url: `https://github.com/${repo}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(name)}`,
    };
    for (const k of t.keys) platforms[k] = entry;
  }
  return {
    manifest: {
      version,
      notes: `https://github.com/${repo}/releases/tag/${tag}`,
      pub_date: pubDate,
      platforms,
    },
    missing,
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [dir, repo, tag, version] = process.argv.slice(2);
  if (!dir || !repo || !tag || !version) {
    console.error("usage: updater-manifest.mjs <dir> <owner/repo> <tag> <version>");
    process.exit(2);
  }
  const names = readdirSync(dir);
  let built;
  try {
    built = buildManifest({
      names,
      repo,
      tag,
      version,
      // RFC 3339, which the plugin parses; whole seconds keep it plain.
      pubDate: new Date().toISOString().replace(/\.\d+Z$/, "Z"),
      readSig: (n) => readFileSync(join(dir, n), "utf8"),
    });
  } catch (e) {
    console.log(`::error::${e instanceof Error ? e.message : String(e)}`);
    process.exit(1);
  }
  const { manifest, missing } = built;
  for (const m of missing) console.log(`::warning::no *${m} in the draft; latest.json will not offer it`);
  if (Object.keys(manifest.platforms).length === 0) {
    console.log("::error::no updater bundles in the draft; latest.json not written");
    process.exit(1);
  }
  writeFileSync(join(dir, "latest.json"), JSON.stringify(manifest, null, 2) + "\n");
  console.log(JSON.stringify(manifest.platforms, null, 2));
}
