# Changelog

Notable changes to this project. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — unreleased

First release. Speaking practice, pronunciation and fluency scoring, grammar
feedback and spaced repetition, running entirely on-device.

### Added

- **Practice loop.** Pick a prompt, record yourself, and get scored: a
  sentence-level pronunciation score from CTC forced alignment, fluency
  (speaking rate, pauses, fillers) and offline grammar feedback with in-place
  suggestions. About 120 built-in prompts across everyday conversation and
  job interviews.
- **Pronunciation feedback calibrated against expert ratings.** The score
  comes from a table measured against speechocean762's expert ratings.
  Individual words are only marked "check" when they are worth another
  listen — about one correctly said word in ten — and never shown as wrong,
  because at best about one flag in four is a real mistake. Fluency timings
  are used only when what was said matches the prompt word for word;
  otherwise pauses are measured from the audio itself.
- **Review.** FSRS-6 spaced repetition with daily caps, tags, suspend, bury
  and undo. Keyboard-first, with the predicted interval shown under each
  grade.
- **Decks, Progress and Settings.** Deck and card management, reviews-per-day
  and retention charts, streaks, voice selection, and a diagnostics panel.
- **Data portability.** JSON export that round-trips FSRS memory state and
  full review history, CSV/TSV for interchange with Anki, and whole-database
  backup and restore. A restore keeps the database it replaces as
  `app.db.bak`, complete even if the app had not shut down cleanly.
- **In-app model downloader.** First-run onboarding fetches the speech and
  voice models with progress, pause, resume and cancel. Downloads resume
  after an interruption, are verified by sha256 before installation, and are
  also reachable from Settings.
- **Offline by construction.** No account, no telemetry, no cloud calls. The
  only network request the app makes is fetching model files.

### Security

- Content Security Policy enforced in dev and release builds; the webview has
  no network access and no SQL capability.
- Model URLs pinned to immutable upstream commits and verified by hard-coded
  sha256 hashes, with no way to skip verification.
- CSV export never hands spreadsheets a formula to run. A card that begins
  with `=`, `+`, `-` or `@` is written with a leading apostrophe, so Excel,
  LibreOffice and Sheets show it as text; importing the file back removes
  it. TSV export — the format for Anki, which imports text as-is — and JSON
  export are written unchanged.
- Releases publish `SHA256SUMS` and build provenance attestations. Binaries
  are not code-signed — see SECURITY.md for why, and how to verify them.

### Known limitations

- Pronunciation scoring is **grapheme**-level, not phoneme-level: the
  recognition model's vocabulary is letters, so scores conflate spelling with
  articulation and cannot name which phoneme was wrong. A true phoneme model
  would be a separate 300 MB–1.2 GB download and is not shipped.
- Filler-word detection under-counts. The recognition model is trained on
  read speech and rarely emits disfluencies, so an acoustic hesitation signal
  compensates only partly.
- Pronunciation calibration used speakers whose first language is Mandarin
  only; it has not been checked against other first languages.
- Release builds cover Linux (x86_64), macOS (Apple Silicon) and Windows
  (x86_64). Intel Macs are not supported: the ONNX Runtime binding ships no
  prebuilt library for them.
- Automatic updates are disabled. Updating means downloading a new release.

[0.1.0]: https://github.com/lubdhak7414/offline-language-practice/releases/tag/v0.1.0
