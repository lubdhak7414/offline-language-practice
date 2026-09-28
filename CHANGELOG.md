# Changelog

Notable changes to this project. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Session summary.** From your second attempt in a sitting, Practice shows
  how it is going: attempts, time, average pronunciation and speaking rate
  where they were measured, and the words worth another listen most often.
  Sessions now end when you switch topic or leave the screen.

- **Daily speaking goal.** Practice shows "3 of 5 today" and Progress shows
  a "Speaking today" tile. The goal is five attempts by default and can be
  changed, or set to 0 to hide it, in Settings. It only counts; nothing is
  locked and there are no reminders.

- **Try again, with the change shown.** After a retry of the same read-aloud
  prompt, Practice says whether the pronunciation score went up, down or is
  about the same as your last try. Changes under five points count as the
  same, because the score is a hint. Nothing is compared across a different
  prompt or between different kinds of scoring.

- **Speaking trends on Progress.** Attempts per day and average pronunciation
  over the last 30 days. Days without a read-aloud attempt are left out of the
  pronunciation line rather than drawn as zero.

- **Your own prompts.** Add a sentence to read aloud (scored for
  pronunciation) or a question to answer (free speaking) from "Your own
  prompts" on Practice. They join the random prompts, can be practised
  straight away, and can be deleted. Sentences must be plain letters and
  apostrophes, with numbers written as words, because the recogniser cannot
  produce digits and could never match them. Up to 200 are kept, in the
  database, so a backup carries them.

- **Interview answers get a target length.** An open interview question says
  how long to aim for (30, 60 or 90 seconds by level) and, after you answer,
  whether yours was short, about right or long. It is advice about length
  only; what you said is never scored.

### Changed

- **Practice opens on your goal.** The goal chosen during setup now decides
  whether Practice starts on everyday conversation or job interviews, and can
  be changed in Settings. Before, it was saved and never used.
- **Streak counts speaking practice.** A day counts if you reviewed a card or
  recorded a practice attempt. Before, only card reviews counted, so someone
  who practised speaking every day had no streak.

## [0.1.0] — 2026-09-28

First release. Speaking practice, pronunciation and fluency scoring, grammar
feedback and spaced repetition, running entirely on-device.

### Added

- **Practice loop.** Pick a prompt, record yourself, and get scored: a
  sentence-level pronunciation score from CTC forced alignment, fluency
  (speaking rate, pauses, fillers) and offline grammar feedback, marked in
  place and listed with suggestions under the transcript. About 120
  built-in prompts across everyday conversation and job interviews, at
  three levels you can pick from.
- **Hear yourself.** After an attempt, play your own recording next to the
  feedback and the voice's reading of the prompt. The recording stays in
  memory and is dropped with the prompt; it is never written to disk.
- **Pronunciation feedback calibrated against expert ratings.** The score
  comes from a table measured against speechocean762's expert ratings.
  Individual words are only marked "check" when they are worth another
  listen — about one correctly said word in ten — and never shown as wrong,
  because at best about one flag in four is a real mistake. Fluency timings
  are used only when what was said matches the prompt word for word;
  otherwise pauses are measured from the audio itself.
- **Review.** FSRS-6 spaced repetition with daily caps, tags, suspend, bury
  and undo. Keyboard-first, with the predicted interval shown under each
  grade. Review one deck or all of them, with optional per-deck daily
  limits, and hear any card read aloud.
- **Decks, Progress and Settings.** Deck and card management with search
  and tag filters, reviews-per-day and retention charts, streaks, a
  recent-practice history with each attempt's scores, voice selection, and
  a diagnostics panel.
- **Data portability.** JSON export that round-trips FSRS memory state and
  full review history, CSV/TSV for interchange with Anki, and whole-database
  backup and restore. A restore keeps the database it replaces as
  `app.db.bak`, complete even if the app had not shut down cleanly.
- **In-app model downloader.** First-run onboarding fetches the speech and
  voice models with progress, pause, resume and cancel. Downloads resume
  after an interruption, are verified by sha256 before installation, and are
  also reachable from Settings.
- **Opt-in updates.** Off by default. When turned on, the app checks GitHub
  once per launch; "Check now" is always available. AppImage, macOS and
  Windows copies install signed updates in place; .deb/.rpm copies are told a
  new version exists.
- **Offline by construction.** No account, no telemetry, no cloud calls. The
  app goes online only to fetch model files and, if you opt in, to check
  GitHub for a new version.

### License

- GPL-3.0-or-later. The app statically links espeak-ng, which is
  GPL-3.0-or-later, for the voice's text-to-phoneme step.

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
- Updates are minisign-signed in CI, bound to their version, and verified
  before installation; `latest.json` and the signatures are in `SHA256SUMS`
  and attested. The web view is granted none of the updater's commands.

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

[Unreleased]: https://github.com/lubdhak7414/offline-language-practice/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/lubdhak7414/offline-language-practice/releases/tag/v0.1.0
