// @vitest-environment node
import { describe, expect, it } from "vitest";

import { buildManifest } from "./updater-manifest.mjs";

const REPO = "lubdhak7414/offline-language-practice";
const P = "offline-language-practice_0.2.0";

// Every asset a complete draft holds, as tauri-action names them (the CLI's
// file names, except the macOS tarball, which it renames with version and
// arch), plus SHA256SUMS from a previous attempt.
const INSTALLERS = [
  `${P}_amd64.deb`,
  `${P}_amd64.AppImage`,
  "offline-language-practice-0.2.0-1.x86_64.rpm",
  `${P}_aarch64.dmg`,
  `${P}_aarch64.app.tar.gz`,
  `${P}_x64-setup.exe`,
  `${P}_x64_en-US.msi`,
];
const SIGNED = INSTALLERS.filter((n) => !n.endsWith(".dmg"));
const ALL = [...INSTALLERS, ...SIGNED.map((n) => `${n}.sig`), "SHA256SUMS"];

function build(names = ALL) {
  return buildManifest({
    names,
    repo: REPO,
    tag: "v0.2.0",
    version: "0.2.0",
    pubDate: "2026-10-01T12:00:00.000Z",
    readSig: (n) => `sig-of:${n}\n`,
  });
}

describe("updater manifest", () => {
  it("offers every self-installing bundle under the plugin's keys", () => {
    const { manifest, missing } = build();
    expect(missing).toEqual([]);
    expect(manifest.version).toBe("0.2.0");
    expect(manifest.pub_date).toBe("2026-10-01T12:00:00.000Z");
    expect(Object.keys(manifest.platforms).sort()).toEqual(
      [
        "darwin-aarch64",
        "darwin-aarch64-app",
        "linux-x86_64",
        "linux-x86_64-appimage",
        "windows-x86_64",
        "windows-x86_64-msi",
        "windows-x86_64-nsis",
      ].sort(),
    );
  });

  it("uses tag-based download URLs and the trimmed signature", () => {
    const { manifest } = build();
    expect(manifest.platforms["linux-x86_64"]).toEqual({
      signature: `sig-of:${P}_amd64.AppImage.sig`,
      url: `https://github.com/${REPO}/releases/download/v0.2.0/${P}_amd64.AppImage`,
    });
    expect(manifest.notes).toBe(`https://github.com/${REPO}/releases/tag/v0.2.0`);
  });

  it("never offers a .deb or .rpm, which the app does not install", () => {
    const { manifest } = build();
    const keys = Object.keys(manifest.platforms);
    expect(keys.some((k) => k.endsWith("-deb") || k.endsWith("-rpm"))).toBe(false);
    const urls = Object.values(manifest.platforms).map((e) => e.url);
    expect(urls.some((u) => u.endsWith(".deb") || u.endsWith(".rpm"))).toBe(false);
  });

  it("makes NSIS the bare Windows fallback, not the MSI", () => {
    const { manifest } = build();
    expect(manifest.platforms["windows-x86_64"]?.url).toMatch(/_x64-setup\.exe$/);
    expect(manifest.platforms["windows-x86_64-msi"]?.url).toMatch(/_x64_en-US\.msi$/);
  });

  it("refuses a bundle without its signature", () => {
    const names = ALL.filter((n) => n !== `${P}_x64-setup.exe.sig`);
    expect(() => build(names)).toThrow(`${P}_x64-setup.exe has no ${P}_x64-setup.exe.sig`);
  });

  it("refuses two bundles for one platform", () => {
    const names = [...ALL, "other_0.2.0_amd64.AppImage", "other_0.2.0_amd64.AppImage.sig"];
    expect(() => build(names)).toThrow(/more than one \*_amd64\.AppImage/);
  });

  it("reports the platforms a partial draft is missing", () => {
    const names = ALL.filter((n) => !n.includes("_aarch64."));
    const { manifest, missing } = build(names);
    expect(missing).toEqual(["_aarch64.app.tar.gz"]);
    expect(manifest.platforms["darwin-aarch64"]).toBeUndefined();
    expect(manifest.platforms["linux-x86_64"]).toBeDefined();
  });

  it("does not mistake a signature for a bundle", () => {
    // `_amd64.AppImage.sig` must not match the `_amd64.AppImage` suffix.
    const { manifest } = build();
    expect(manifest.platforms["linux-x86_64"]?.url.endsWith(".sig")).toBe(false);
  });
});
