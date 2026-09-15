# Offline Language Practice

A desktop language-learning app that runs entirely on your machine — speech recognition, text-to-speech, and spaced-repetition flashcards, with no cloud calls and no account. Record yourself speaking a phrase, get it transcribed and graded for grammar, hear the correct pronunciation, and review vocabulary on a schedule that adapts to what you actually remember.

Everything happens on-device: the speech model, the voice synthesis, the grammar checker, and the scheduling algorithm all run locally, so it works on a plane with no wifi and never sends your voice or your study data anywhere.

![Offline Language Practice — record, transcribe, and grammar-check a phrase](docs/screenshot.png)

## Features

- **Speech recognition** — record a phrase and get it transcribed locally via a Wav2Vec2 ONNX model.
- **Grammar checking** — transcripts (or anything you type) get checked for grammar and style issues by a local, offline linter, no network round-trip.
- **Text-to-speech** — hear correct pronunciation via a local neural voice (Piper).
- **Spaced repetition** — flashcard review scheduled by FSRS-6, the same memory-modeling algorithm behind modern Anki, tuned to your own review history rather than a fixed interval.
- **Fully offline** — once the models are downloaded once, the app makes zero network calls.

## Why this was interesting to build

Getting five separate subsystems (speech recognition, speech synthesis, grammar checking, spaced repetition, and local storage) to share one Rust binary meant a few real constraints:

- The ONNX Runtime binding (`ort`) and the bundled TTS engine (`piper-rs`) each pin a specific, incompatible version of the same underlying library, so the inference pipeline is wired by hand in `asr.rs`/`tts.rs` rather than through a higher-level wrapper that assumes one version.
- Audio, grammar checking, and database writes each get their own thread pool so a slow model-loading step or a spaced-repetition parameter refit never freezes the UI.
- CI enforces that `package.json`, `tauri.conf.json`, and `Cargo.toml` all report the same version number before anything else runs, and denies any Rust lint warning outright.

## Quick start

```bash
npm install
./scripts/download-models.sh          # fetches the ASR + TTS models from Hugging Face (~350 MB)

# two terminals, since this project doesn't wire beforeDevCommand:
npm run dev                           # terminal 1: Vite dev server
npm run tauri dev                     # terminal 2: Tauri window
```

`model_status` in the UI (and the Diagnostics-style output in dev tools) tells you whether the ASR/TTS models were found; without them the app still runs, it just can't transcribe or speak.

**On Linux with an NVIDIA GPU + Wayland**, WebKitGTK's DMA-BUF renderer can crash the window on launch (`Error 71 (Protocol error) dispatching to Wayland display`, then repeated `WebKit encountered an internal error`). If that happens:

```bash
WEBKIT_DISABLE_DMABUF_RENDERER=1 npm run tauri dev
```

## Tech stack

| Layer | Tech |
|---|---|
| Shell | Tauri v2 (Rust backend + WebView frontend) |
| Frontend | TypeScript, Vite, no framework |
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

CI (`.github/workflows/ci.yml`) runs `cargo fmt`, `cargo clippy -D warnings`, and `cargo test` on every push, plus a version-consistency check across the three manifest files and a check that release icons exist.

## License

MIT
