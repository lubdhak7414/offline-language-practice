# Offline Language Practice

A desktop language-learning app that runs entirely on your machine — speech recognition, text-to-speech, and spaced-repetition flashcards, with no cloud calls and no account. Record yourself speaking a phrase, get it transcribed and graded for grammar, hear the correct pronunciation, and review vocabulary on a schedule that adapts to what you actually remember.

Everything happens on-device: the speech model, the voice synthesis, the grammar checker, and the scheduling algorithm all run locally, so it works on a plane with no wifi and never sends your voice or your study data anywhere.

![Offline Language Practice — the Practice screen with a read-aloud prompt, ready to record](docs/screenshot.png)

## Features

- **Speaking practice** — pick a prompt, record yourself, and get scored: a sentence-level pronunciation score from CTC forced alignment, with words worth another listen marked, plus fluency (speaking rate, pauses, fillers) and grammar. About 120 built-in prompts across everyday conversation and job interviews.
- **Speech recognition** — transcription runs locally via a Wav2Vec2 ONNX model.
- **Grammar checking** — transcripts, or anything you type, are checked by a local offline linter with in-place suggestions.
- **Text-to-speech** — hear correct pronunciation via a local neural voice (Piper).
- **Spaced repetition** — review scheduled by FSRS-6, the memory model behind modern Anki, fitted to your own history. Daily caps, tags, suspend, bury and undo.
- **Your data stays yours** — JSON export that round-trips full review history and FSRS state, CSV/TSV for Anki, and whole-database backup and restore. CSV export gives a card beginning with `=`, `+`, `-` or `@` a leading apostrophe so spreadsheets show it as text instead of running it as a formula. TSV is written verbatim — use it for Anki, which would otherwise show the apostrophe. JSON export is unchanged.
- **Fully offline** — the only network request the app ever makes is downloading the models. After that it works with the network off.

## Why this was interesting to build

Getting five separate subsystems (speech recognition, speech synthesis, grammar checking, spaced repetition, and local storage) to share one Rust binary meant a few real constraints:

- The ONNX Runtime binding (`ort`) and the bundled TTS engine (`piper-rs`) each pin a specific, incompatible version of the same underlying library, so the inference pipeline is wired by hand in `asr.rs`/`tts.rs` rather than through a higher-level wrapper that assumes one version.
- Audio, grammar checking, and database writes each get their own thread pool so a slow model-loading step or a spaced-repetition parameter refit never freezes the UI.
- CI enforces that `package.json`, `tauri.conf.json`, and `Cargo.toml` all report the same version number before anything else runs, and denies any Rust lint warning outright.

## Quick start

```bash
npm install
npm run tauri dev
```

One terminal is enough — `beforeDevCommand` starts Vite.

The models are not in the repo and not in the installer. On first run the app walks you through downloading them (~456 MB), with progress, pause and resume; you can also get them from Settings later, or fetch them up front:

```bash
./scripts/download-models.sh
```

Either way the files are pinned to immutable upstream commits and verified by sha256 before being installed. Settings → Diagnostics tells you what was found. Without the models the app still runs, it just cannot listen or speak.

**On Linux with an NVIDIA GPU + Wayland**, WebKitGTK's DMA-BUF renderer can crash the window on launch (`Error 71 (Protocol error) dispatching to Wayland display`, then repeated `WebKit encountered an internal error`). If that happens:

```bash
WEBKIT_DISABLE_DMABUF_RENDERER=1 npm run tauri dev
```

**Intel Macs are not supported, and no Intel build is planned.** ONNX Runtime stopped publishing Intel macOS binaries after 1.23.2, the `ort` binding this app uses needs a newer one, and macOS 26 is the last release for Intel Macs. Apple Silicon Macs work.

To build for an Intel Mac yourself, compile a static ONNX Runtime 1.24.2 (the version `ort` 2.0.0-rc.12 expects) on that Mac, then point the build at it:

```bash
ORT_LIB_LOCATION=/path/to/onnxruntime/lib npm run tauri build
```

`ORT_LIB_LOCATION` must be the directory holding `libonnxruntime.a`; `ort`'s build script links from there instead of downloading. See the [`ort` linking guide](https://ort.pyke.io/setup/linking). CoreML acceleration is not needed; the app falls back to CPU.

## Tech stack

| Layer | Tech |
|---|---|
| Shell | Tauri v2 (Rust backend + WebView frontend) |
| Frontend | Preact + `@preact/signals`, TypeScript, Vite |
| Speech recognition | Wav2Vec2 via ONNX Runtime (`ort`) |
| Speech synthesis | Piper neural TTS (`piper-rs`) |
| Spaced repetition | FSRS-6 (`fsrs`) |
| Grammar checking | `harper-core`, fully offline |
| Storage | SQLite via `tauri-plugin-sql`, WAL mode, versioned migrations |

## Building

```bash
npm run build                    # tsc + vite build
cd src-tauri && cargo build --release
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt`, `cargo clippy -D warnings` and `cargo test` on every push, compiles on Linux, macOS and Windows, audits dependencies with `cargo-deny`, and checks that `package.json`, `tauri.conf.json` and `Cargo.toml` agree on the version and that the release icon set exists.

Tagging `v*` builds Linux (x86_64), macOS (Apple Silicon) and Windows (x86_64) and opens a **draft** GitHub release with `SHA256SUMS` and build provenance attached (`.github/workflows/release.yml`). Binaries are not code-signed — SECURITY.md explains why and how to verify them instead.

See CONTRIBUTING.md for the house rules, which exist because breaking them has already cost real debugging time.

## License

GPL-3.0-or-later. See LICENSE.

Copyright © 2026 Safwan Usaid Lubdhak. This program is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. It is distributed in the hope that it will be useful, but without any warranty; without even the implied warranty of merchantability or fitness for a particular purpose.

The app is GPL because its binary statically links [espeak-ng](https://github.com/espeak-ng/espeak-ng) (GPL-3.0-or-later), which the Piper voice uses to turn text into phonemes. espeak-ng's compiled English data ships inside the app, in `src-tauri/resources/espeak-ng-data/`; `scripts/vendor-espeak-data.sh` regenerates it from the espeak-ng sources that `espeak-rs-sys` vendors. Every other dependency is under a GPL-compatible permissive license, checked in CI by `cargo-deny`.

The models are downloaded separately and carry their own licenses: the Wav2Vec2 ONNX export and the Piper LibriTTS-R voice are both MIT-licensed upstream.

The pronunciation score is calibrated against [speechocean762](https://www.openslr.org/101/) (Zhang et al., "speechocean762: An Open-Source Non-native English Speech Corpus For Pronunciation Assessment", Interspeech 2021), used under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Only derived numbers — a 25-point lookup table in `src-tauri/src/pronounce.rs` — are included; the corpus itself is not. Its speakers all have Mandarin as a first language. `scripts/calibrate-gop.py` reproduces the table and prints what it does and does not measure.

Beyond Mandarin speakers, only false alarms have been measured so far (2026-09-28), on [CMU ARCTIC](http://festvox.org/cmu_arctic/) (Kominek and Black, Carnegie Mellon University; free for any use, copyright notice retained): seven speakers each reading about 1,130 prompts in a studio, 70,042 words, none of the recordings included here. The share of words flagged for another listen was 3.9% for the four US English speakers (95% CI 3.0–4.5%, bootstrap over speakers), 3.7% for the Canadian speaker, 4.1% for the Indian English speaker and 1.6% for the Scottish speaker. The equivalent figure for words that experts rated correct in speechocean762 is 10.2%. Each non-US accent is one speaker, so its figure describes that speaker, not the accent. Clean studio reads by fluent speakers are the easiest case for the model, so these numbers set a floor, not a typical rate. Nothing here shows whether the score catches real mistakes by speakers of other first languages. That needs a corpus with mispronunciations labelled by annotators, and none has been measured yet.

The filled-pause detector's thresholds in `src-tauri/src/fluency.rs` (not yet used by the app) were measured on the [AMI Meeting Corpus](https://groups.inf.ed.ac.uk/ami/corpus/) (Carletta et al., "The AMI Meeting Corpus: A Pre-announcement", MLMI 2005), used under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Only a handful of threshold values are included; the corpus itself is not. `scripts/prepare-ami-fillers.py` and `scripts/eval-fillers.py` reproduce the measurement.
