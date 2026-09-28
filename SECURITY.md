# Security

## Reporting a vulnerability

Report privately through GitHub's [Security Advisories](../../security/advisories/new)
rather than a public issue. Expect an acknowledgement within a week.

Please include what you did, what happened, and what you expected. A proof of
concept helps; a working exploit is not required and is not expected.

## What this app does and does not do

It is an offline desktop app. It has no account, no server, no telemetry, and
no analytics. Practice audio, transcripts and review history stay in a local
SQLite database and are never transmitted.

The app makes network requests in exactly two situations. The first is
downloading model files, during first-run onboarding or from Settings. Those
requests go to `huggingface.co` over HTTPS. The second is checking for
updates, which is **off unless you turn it on**, plus pressing Settings →
Check now. That request goes to `github.com` over HTTPS. Everything else —
speech recognition, speech synthesis, grammar checking, scheduling — runs
on-device with the network off.

Settings → Diagnostics can save a text report to a file you choose, for
attaching to a bug report. It is written locally and never sent by the app.
It holds the version, platform, model status, settings and counts of decks,
cards, reviews and practice attempts. It leaves out recordings, transcripts,
card and prompt text, and the microphone's name.

## Model integrity

Model files are large binaries that get loaded into the inference runtime, so
where they come from matters.

Every URL is pinned to an immutable upstream commit, never a branch. This is
not hypothetical: an earlier version of `scripts/download-models.sh` fetched
from `resolve/main/…`, and that URL now returns 404 because the upstream
repository reorganized its directories. A branch reference is a promise
upstream never made.

Every file has a hard-coded sha256 that is verified before the file is
installed. Downloads land in `<name>.part` and are only renamed into place
after the hash matches, so an interrupted or tampered download can never be
mistaken for a complete one. There is no flag to skip verification — a
silently wrong model produces silently wrong output, which is worse than no
model at all.

The same hashes appear in `src-tauri/src/download.rs` and
`scripts/download-models.sh`. Re-pinning a revision means updating both.

## Code signing

Releases are **not code-signed**, on any platform. This is a deliberate,
documented choice rather than an oversight:

- **macOS.** Gatekeeper blocks unsigned and un-notarized apps. Notarization
  requires an Apple Developer account at $99/year, with no fee waiver for
  individuals. Until that exists, macOS users should build from source, or
  knowingly accept the risk: since macOS 15 (Sequoia), Control-click → Open
  no longer bypasses Gatekeeper; open the app once, then choose System
  Settings → Privacy & Security → Open Anyway. The equivalent from a
  terminal is `xattr -d com.apple.quarantine /Applications/<app>.app`.
- **Windows.** Microsoft's own signing service (Artifact Signing, from
  $9.99/month) accepts individual developers only in the United States and
  Canada. A signature no longer buys an instant pass either: EV certificates
  stopped bypassing SmartScreen, and signed files still warn until
  reputation builds. Expect a SmartScreen warning: "More info" then "Run
  anyway". On Windows 11 with Smart App Control turned on, unsigned files
  are blocked outright and there is no "Run anyway". That check covers
  every executable, not only downloads, so building from source does not
  help either: those users cannot run the app until releases are signed. Free signing for open-source
  projects (SignPath Foundation) is the planned route once releases have a
  download history, which it requires.
- **Linux.** Unsigned AppImage and `.deb` artifacts are normal.

What is offered instead of a signature is verifiable provenance. Every release
publishes a `SHA256SUMS` file, and every artifact carries a
[build attestation](https://docs.github.com/actions/security-guides/using-artifact-attestations)
tying it to the exact workflow run and commit that produced it:

```bash
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify <file> --repo <owner>/<repo>
```

That proves the binary came from this repository's CI, which is a stronger
claim than most code signatures make. It does not prove the code is safe.

## Automatic updates

Off by default. When you turn on "Check for a new version each time the app
starts" (Settings, or the model-download step of setup), the app fetches
`https://github.com/lubdhak7414/offline-language-practice/releases/latest/download/latest.json`
once per launch. "Check now" does the same once, when pressed. The request
contains no information about you or your practice; GitHub sees your IP
address and a `tauri-plugin-updater` user agent, as with any connection. The
decision is enforced in the app's Rust code, not in its web view.

Updates are signed with a minisign key held only in this repository's CI
secrets. The app carries the public key, rejects any download whose signature
does not verify, and rejects a signature made for a different version than
the one announced (`requireSignedVersion`). An update is installed only when
you press Install.

- **AppImage, macOS, Windows (NSIS or MSI):** the app downloads, verifies and
  installs the update, then restarts. On Windows the installer closes the app
  and reopens it.
- **.deb and .rpm:** the app tells you a new version exists but does not
  install it. Your package manager owns those files, and installing a package
  needs root, which the app will not ask for.
- **macOS:** run the app from `/Applications` after removing the quarantine
  flag (see Code signing). A copy launched straight from Downloads runs from a
  read-only location and cannot update itself.

`latest.json` and every `.sig` file are listed in `SHA256SUMS` and covered by
the build attestation, like the installers. Only published, non-pre-release
releases are ever offered: GitHub's `releases/latest` does not resolve to a
draft or a pre-release.

## Hardening in the app

- A Content Security Policy is enforced in both dev and release builds
  (`app.security.csp` in `src-tauri/tauri.conf.json`). `object-src` is
  `none`, `frame-ancestors` is `none`, and `connect-src` does not include
  any remote origin — the webview cannot reach the network at all. Model
  downloads and update checks happen in Rust, not in the webview.
- The web view holds no updater capability. `updater:*` permissions are not
  granted; checks and installs go through the app's own Rust commands.
- The frontend holds no SQL capability. `sql:*` permissions are deliberately
  not granted; every database access goes through the app's own Rust
  commands, which use bound parameters throughout.
- Restores never swap a database file underneath an open connection. A
  validated backup is staged and installed before the SQL plugin opens its
  pool, and the previous database is rotated aside rather than overwritten.
