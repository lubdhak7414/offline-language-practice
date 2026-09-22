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

The app makes network requests in exactly one situation: downloading model
files, either during first-run onboarding or from Settings. Those requests go
to `huggingface.co` over HTTPS and nowhere else. Everything else — speech
recognition, speech synthesis, grammar checking, scheduling — runs on-device
with the network off.

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

- **macOS.** Gatekeeper blocks unsigned and un-notarized `.dmg` files.
  Notarization requires an Apple Developer account at $99/year. Until that
  exists, macOS users should build from source, or knowingly accept the risk
  with `xattr -d com.apple.quarantine /Applications/<app>.app`.
- **Windows.** An OV or EV certificate runs $200–600/year, and SmartScreen
  reputation still takes time to accumulate afterwards, so a fresh
  certificate buys less than it costs. Expect a SmartScreen warning:
  "More info" then "Run anyway".
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

Disabled. Enabling the Tauri updater needs a minisign keypair held in CI and a
hosted update endpoint; shipping it half-configured would be worse than not
shipping it. Until then, updates are manual.

## Hardening in the app

- A Content Security Policy is enforced in both dev and release builds
  (`app.security.csp` in `src-tauri/tauri.conf.json`). `object-src` is
  `none`, `frame-ancestors` is `none`, and `connect-src` does not include
  any remote origin — the webview cannot reach the network at all. Model
  downloads happen in Rust, not in the webview.
- The frontend holds no SQL capability. `sql:*` permissions are deliberately
  not granted; every database access goes through the app's own Rust
  commands, which use bound parameters throughout.
- Restores never swap a database file underneath an open connection. A
  validated backup is staged and installed before the SQL plugin opens its
  pool, and the previous database is rotated aside rather than overwritten.
