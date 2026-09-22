//! Offline language practice — Tauri v2.11 shell (Subsystem 1: shell + IPC).
//!
//! Thread-domain map (do not violate):
//! - Domain 1 (main / webview): `run()` builds the Tauri app, registers the
//!   `audiostream` protocol and the invoke handler. Never block here.
//! - Domain 2 (tokio async): all `#[tauri::command]` bodies run as async tasks.
//!   DB access goes through `crate::db::sqlite_pool` (WAL) inside these tasks.
//! - Domain 3 (neural blocking): ASR (`ort` Session) and TTS (`piper`)
//!   inference live on two dedicated `std::thread` workers (`neural-asr` /
//!   `neural-tts`). Commands talk to them over bounded `tokio::mpsc`
//!   channels with per-request `oneshot` replies; engines lazy-load inside
//!   the worker on first use. The `audiostream` protocol handler does its
//!   TTS work on a spawned thread per request so the webview never blocks.
//! - Domain 4 (rayon): FSRS batch work / `optimize_parameters` parallelism
//!   lives inside `crate::scheduler` (rayon), never in this file.

mod asr;
mod audio;
mod backup;
mod csvfmt;
mod db;
mod error;
mod fluency;
mod grammar;
mod inference;
mod paths;
mod portable;
mod practice;
mod prefs;
mod prompts_seed;
mod pronounce;
mod scheduler;
mod stats;
mod tts;

use std::sync::{Arc, RwLock};

use fsrs::{FSRS, FSRS6_DEFAULT_DECAY};
use tauri::ipc::{Channel, Response};
use tauri::{Emitter, Manager, State};
use tauri_plugin_sql::{Builder as SqlBuilder, DbInstances, Migration, MigrationKind};
use tokio::sync::{mpsc, oneshot};

use crate::asr::AsrEngine;
use crate::backup::BackupInfo;
use crate::db::{
    DB_URL, MIGRATION_1_SQL, MIGRATION_2_SQL, MIGRATION_3_SQL, MIGRATION_4_SQL, MIGRATION_5_SQL,
    MIGRATION_6_SQL, MIGRATION_7_SQL,
};
use crate::fluency::FluencyReport;
use crate::grammar::LintOutput;
use crate::portable::{ExportEnvelope, ImportSummary};
use crate::practice::AttemptInput;
use crate::prefs::Preferences;
use crate::pronounce::PronScore;
use crate::scheduler::{CardRow, DeckRow, DueCardView, ReviewRow, ReviewStats, TagRow, UndoResult};
use crate::stats::{DayCount, ForecastDay, Overview, RetentionBucket};
use crate::tts::TtsEngine;

// ─── Neural workers (Domain 3) ───────────────────────────────────────────────

/// Request envelope for the neural worker threads. Each request carries a
/// `oneshot` reply so commands can await exactly their own result.
pub enum NeuralReq {
    Transcribe {
        pcm: Vec<f32>,
        reply: oneshot::Sender<Result<String, String>>,
    },
    /// Transcribe and, when a target is given, score the pronunciation
    /// against it *inside the worker*.
    ///
    /// Scoring is not a separate request on purpose: it needs the frame
    /// posteriors, which are ~1 MB for a two-minute recording and would
    /// otherwise have to cross a channel and be held by an async task. The
    /// PCM is an `Arc` because the command keeps its own handle to run
    /// fluency analysis in parallel, without copying the audio.
    TranscribeScored {
        pcm: Arc<Vec<f32>>,
        target: Option<String>,
        reply: oneshot::Sender<Result<ScoredTranscript, String>>,
    },
    Synth {
        text: String,
        /// Which installed voice to use. `None`/`Some("")` means the
        /// default voice (whatever `find_voice` resolves). A non-empty id
        /// is looked up via `tts::resolve_voice`, which also rejects
        /// anything that is not a plain file-name component — this id
        /// comes straight from the frontend by way of `set_voice`/the
        /// `audiostream://` `?voice=` query param.
        voice_id: Option<String>,
        reply: oneshot::Sender<Result<(Vec<u8>, u32), String>>,
    },
}

/// What the ASR worker returns for a scored request.
pub struct ScoredTranscript {
    pub text: String,
    /// `None` when there was no target, or when scoring refused.
    pub pron: Option<PronScore>,
    /// Why scoring refused, when it did. Surfaced so a fallback to text
    /// alignment is explainable rather than silent.
    pub pron_error: Option<String>,
}

/// Shared state: channel handles to the neural workers plus FSRS state.
/// All fields are `Send + Sync`, so `AppState` is too.
pub struct AppState {
    pub asr_tx: mpsc::Sender<NeuralReq>,
    pub tts_tx: mpsc::Sender<NeuralReq>,
    pub fsrs: RwLock<FSRS>,
    pub params: RwLock<Vec<f32>>,
    pub retention: RwLock<f32>,
    /// The active TTS voice id (`""`/`None` in storage means "default"),
    /// mirrored from `prefs::Preferences::tts_voice` at startup and updated
    /// by `set_voice`. Read by `synthesize_speech` and the `audiostream://`
    /// handler so both use the same voice without re-reading `app_settings`
    /// on every request.
    pub voice: RwLock<Option<String>>,
}

/// Max `NeuralReq`s buffered per neural worker channel (see `run()`).
/// Admission control compares multi-chunk requests against this MAXIMUM,
/// never against currently-free permits: sends drain sequentially (each
/// send is followed by awaiting its reply), so free-permit checks would
/// spuriously report busy under concurrent commands.
const TTS_CHANNEL_SLOTS: usize = 8;
/// `?text=` cap for the `audiostream://` protocol: max chunks (8) × 180
/// chars per chunk. Longer input would always fail admission control.
const AUDIOSTREAM_MAX_CHARS: usize = TTS_CHANNEL_SLOTS * 180;

/// Lazy-load the ASR model into `slot` and hand back a reference.
///
/// The model is loaded once per process, inside the worker thread, on the
/// first request that needs it — startup stays fast and a user without
/// models installed gets an error only when they actually record.
fn ensure_asr(slot: &mut Option<AsrEngine>) -> Result<&AsrEngine, String> {
    if slot.is_none() {
        let path = crate::asr::find_model().ok_or_else(|| {
            "ASR_NO_MODEL: model not installed; install model to enable transcription".to_string()
        })?;
        *slot = Some(
            AsrEngine::load(&path)
                .map_err(|e| format!("ASR_NO_MODEL: failed to load model: {e}"))?,
        );
    }
    slot.as_ref()
        .ok_or_else(|| "ASR_NO_MODEL: model not installed".to_string())
}

/// Transcribe, and score against `target` when there is one.
///
/// A failure to *score* is not a failure to transcribe: the transcript is
/// returned either way with `pron_error` explaining why no acoustic number
/// came with it, and the caller falls back to word alignment. Only a failure
/// to transcribe is an error.
fn transcribe_scored(
    engine: &AsrEngine,
    pcm: &[f32],
    target: Option<&str>,
) -> Result<ScoredTranscript, String> {
    let Some(target) = target else {
        // No reference text: skip the posteriors entirely rather than
        // allocating a megabyte nothing will read.
        return Ok(ScoredTranscript {
            text: engine.transcribe_pcm(pcm).map_err(|e| e.to_string())?,
            pron: None,
            pron_error: None,
        });
    };
    let out = engine
        .transcribe_pcm_detailed(pcm)
        .map_err(|e| e.to_string())?;
    let vocab = crate::asr::load_vocab().map_err(|e| e.to_string())?;
    match crate::pronounce::score_against_target(&out, target, &vocab) {
        Ok(pron) => Ok(ScoredTranscript {
            text: out.text,
            pron: Some(pron),
            pron_error: None,
        }),
        Err(e) => Ok(ScoredTranscript {
            text: out.text,
            pron: None,
            pron_error: Some(e.to_string()),
        }),
    }
}

/// Spawn the dedicated ASR thread: lazy-loads the model on first request,
/// then serves `Transcribe` in FIFO order until the channel closes.
///
/// Misrouted `Synth` requests are rejected with `Err` (never dropped)
/// so no `rx.await` hangs forever.
fn spawn_asr_worker(rx: mpsc::Receiver<NeuralReq>) {
    std::thread::Builder::new()
        .name("neural-asr".to_string())
        .spawn(move || {
            let mut engine: Option<AsrEngine> = None;
            let mut rx = rx;
            while let Some(req) = rx.blocking_recv() {
                match req {
                    NeuralReq::Transcribe { pcm, reply } => {
                        let res = ensure_asr(&mut engine)
                            .and_then(|eng| eng.transcribe_pcm(&pcm).map_err(|e| e.to_string()));
                        let _ = reply.send(res);
                    }
                    NeuralReq::TranscribeScored { pcm, target, reply } => {
                        let res = ensure_asr(&mut engine)
                            .and_then(|eng| transcribe_scored(eng, &pcm, target.as_deref()));
                        let _ = reply.send(res);
                    }
                    NeuralReq::Synth { reply, .. } => {
                        let _ = reply.send(Err("ASR worker received Synth request".to_string()));
                    }
                }
            }
        })
        .expect("failed to spawn neural-asr thread");
}

/// Spawn the dedicated TTS thread: lazy-loads a voice on first request that
/// needs it, then serves `Synth` in FIFO order until the channel closes.
///
/// The cache is keyed by the requested voice id (`""` for "no id given",
/// meaning the default voice), not just "loaded or not": a session that
/// switches voices (`set_voice`, or a one-off `?voice=` override) must not
/// keep serving audio from whatever voice happened to load first. Reloading
/// only happens when the key actually changes, so the common case (every
/// request using the same voice) pays the load cost exactly once.
///
/// Misrouted `Transcribe` requests are rejected with `Err` (never dropped)
/// so no `rx.await` hangs forever.
fn spawn_tts_worker(rx: mpsc::Receiver<NeuralReq>) {
    std::thread::Builder::new()
        .name("neural-tts".to_string())
        .spawn(move || {
            let mut cache: Option<(String, TtsEngine)> = None;
            let mut rx = rx;
            while let Some(req) = rx.blocking_recv() {
                match req {
                    NeuralReq::Synth {
                        text,
                        voice_id,
                        reply,
                    } => {
                        let key = voice_id.unwrap_or_default();
                        let res: Result<(Vec<u8>, u32), String> = (|| {
                            // MSRV 1.77 predates `Option::is_none_or` (1.82).
                            let stale = match cache.as_ref() {
                                Some((k, _)) => k != &key,
                                None => true,
                            };
                            if stale {
                                let (model, config) = if key.is_empty() {
                                    crate::tts::find_voice().ok_or_else(|| {
                                        "TTS voice not installed; install voice to enable speech"
                                            .to_string()
                                    })?
                                } else {
                                    crate::tts::resolve_voice(&key).ok_or_else(|| {
                                        format!("TTS voice not installed: {key:?}")
                                    })?
                                };
                                let eng =
                                    TtsEngine::load(&model, &config).map_err(|e| e.to_string())?;
                                cache = Some((key.clone(), eng));
                            }
                            // `stale` guarantees the arm above ran when needed,
                            // so the cache is always populated here.
                            let (_, eng) = cache.as_ref().expect("cache populated above");
                            let wav = eng.synthesize_wav(&text).map_err(|e| e.to_string())?;
                            Ok((wav, eng.sample_rate()))
                        })();
                        let _ = reply.send(res);
                    }
                    NeuralReq::Transcribe { reply, .. } => {
                        let _ =
                            reply.send(Err("TTS worker received Transcribe request".to_string()));
                    }
                    NeuralReq::TranscribeScored { reply, .. } => {
                        let _ =
                            reply.send(Err("TTS worker received Transcribe request".to_string()));
                    }
                }
            }
        })
        .expect("failed to spawn neural-tts thread");
}

fn busy_message<T>(e: tokio::sync::mpsc::error::TrySendError<T>) -> String {
    match e {
        tokio::sync::mpsc::error::TrySendError::Full(_) => "engine busy, try again".to_string(),
        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
            "neural engine unavailable".to_string()
        }
    }
}

fn tts_busy_message<T>(e: tokio::sync::mpsc::error::TrySendError<T>) -> String {
    match e {
        tokio::sync::mpsc::error::TrySendError::Full(_) => {
            "TTS_BUSY: engine busy, try again".to_string()
        }
        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
            "neural engine unavailable".to_string()
        }
    }
}

/// Best-effort retention refresh: reload from `app_settings`, update the cache,
/// and return the fresh value.
async fn refresh_retention(pool: &sqlx::SqlitePool, state: &AppState) -> f32 {
    let retention = crate::scheduler::load_retention(pool).await;
    if let Ok(mut guard) = state.retention.write() {
        *guard = retention;
    }
    retention
}

/// FSRS-6 decay from the optimized params (`params[20]`), else the default.
fn read_decay(state: &AppState) -> f32 {
    state
        .params
        .read()
        .ok()
        .and_then(|p| p.get(20).copied())
        .unwrap_or(FSRS6_DEFAULT_DECAY)
}

/// On-device model presence (`model_status{}`).
///
/// `asr_model`/`asr_vocab` reuse the ASR loader's own candidate paths
/// (`asr::find_model` + `asr::vocab_available`); `tts_voice` reuses the
/// TTS loader via `tts::default_voice_path`.
#[derive(Clone, Debug, serde::Serialize)]
struct ModelStatus {
    asr_model: bool,
    asr_vocab: bool,
    tts_voice: bool,
}

/// One installed TTS voice (`list_voices{}`): `id` is the voice config
/// stem, `label` is human-readable (see `tts::available_voices`).
#[derive(Clone, Debug, serde::Serialize)]
struct VoiceInfo {
    id: String,
    label: String,
}

// ─── Commands ────────────────────────────────────────────────────────────────

/// Stream PCM bytes through on-device ASR.
///
/// Domain 2 → 3: converts bytes to f32 (non-finite → 0.0), validates
/// 16 kHz mono and the 120 s cap, then round-trips through the `neural-asr`
/// worker. A full worker queue reports "engine busy, try again".
/// Missing-model errors propagate (the frontend surfaces them in
/// `asr-status`).
/// Longest utterance the ASR command accepts, in seconds.
const ASR_MAX_SECONDS: usize = 120;

#[tauri::command]
async fn transcribe_pcm_channel(
    channel: Channel<String>,
    pcm_bytes: Vec<u8>,
    sample_rate: u32,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let pcm = decode_pcm(&pcm_bytes, sample_rate)?;

    let (tx, rx) = oneshot::channel();
    state
        .asr_tx
        .try_send(NeuralReq::Transcribe { pcm, reply: tx })
        .map_err(busy_message)?;
    let text = rx
        .await
        .map_err(|_| "ASR engine unavailable".to_string())??;

    let _ = channel.send(text.clone());
    let _ = app.emit("system-status", "transcription-complete");
    Ok(text)
}

/// Offline deterministic grammar lint (harper-core inside `crate::grammar`).
///
/// `dialect` selects the Harper dictionary (`"american"` default, also
/// `"british"`/`"canadian"`/`"australian"`; unknown → American). Returns
/// `{diags, truncated}` where `truncated` signals the 20k-char input cap.
#[tauri::command]
async fn lint_text(text: String, dialect: Option<String>) -> Result<LintOutput, String> {
    // `lint_text_async` owns `!Send` Harper types on a blocking worker thread.
    crate::grammar::lint_text_async(text, dialect).await
}

/// Synthesize speech, returning a single concatenated WAV blob.
///
/// Long input is split via `audio::split_sentences(text, 180)`; each chunk
/// round-trips through the `neural-tts` worker, the resulting WAVs are
/// joined with `concat_wav_mono16`, and the single blob is sent over
/// `channel`. Atomicity: `chunks.len() <= channel capacity` is checked
/// BEFORE sending anything, so overload never leaves a partial stream;
/// overload reports `TTS_BUSY:` (frontend matches this prefix).
/// Returns the WAV sample rate parsed from the combined blob
/// (fallback 22050).
#[tauri::command]
async fn synthesize_speech(
    text: String,
    channel: Channel<Response>,
    state: State<'_, AppState>,
) -> Result<u32, String> {
    if text.trim().is_empty() {
        return Err("TTS: empty text".to_string());
    }
    let chunks = crate::audio::split_sentences(&text, 180);
    let chunks = if chunks.is_empty() {
        vec![text]
    } else {
        chunks
    };

    // Atomicity: never send partial then Err.
    let capacity = TTS_CHANNEL_SLOTS;
    if chunks.len() > capacity {
        return Err(format!(
            "TTS_BUSY: engine busy (need {} slots, have {capacity})",
            chunks.len()
        ));
    }

    let voice_id = state
        .voice
        .read()
        .map_err(|e| format!("voice lock poisoned: {e}"))?
        .clone();

    let mut wavs: Vec<Vec<u8>> = Vec::with_capacity(chunks.len());
    let mut rate: Option<u32> = None;
    for chunk in chunks.iter() {
        let (tx, rx) = oneshot::channel();
        state
            .tts_tx
            .try_send(NeuralReq::Synth {
                text: chunk.clone(),
                voice_id: voice_id.clone(),
                reply: tx,
            })
            .map_err(tts_busy_message)?;
        let (wav, engine_rate) = rx
            .await
            .map_err(|_| "TTS engine unavailable".to_string())??;
        if rate.is_none() {
            rate = crate::audio::parse_wav_sample_rate(&wav).or(Some(engine_rate));
        }
        wavs.push(wav);
    }
    let combined = crate::audio::concat_wav_mono16(wavs)?;
    let final_rate = crate::audio::parse_wav_sample_rate(&combined)
        .or(rate)
        .unwrap_or(22_050);
    let _ = channel.send(Response::new(combined));
    Ok(final_rate)
}

/// Decode PCM bytes the same way [`transcribe_pcm_channel`] does.
///
/// Shared so the validation rules — sample rate, length cap, non-finite
/// samples — cannot drift between the two entry points.
fn decode_pcm(pcm_bytes: &[u8], sample_rate: u32) -> Result<Vec<f32>, String> {
    let rate = crate::asr::ASR_SAMPLE_RATE;
    if sample_rate != rate {
        return Err(format!("expected 16kHz mono, got {sample_rate}Hz"));
    }
    let mut pcm = crate::asr::AsrEngine::bytes_to_f32_mono(pcm_bytes);
    let max_samples = rate as usize * ASR_MAX_SECONDS;
    if pcm.len() > max_samples {
        return Err(format!(
            "audio too long ({} samples); max is {max_samples} samples ({ASR_MAX_SECONDS}s at 16kHz)",
            pcm.len(),
        ));
    }
    for x in pcm.iter_mut() {
        if !x.is_finite() {
            *x = 0.0;
        }
    }
    Ok(pcm)
}

/// Install the built-in prompt corpus. Idempotent; returns rows added.
#[tauri::command]
async fn seed_prompts(db: State<'_, DbInstances>) -> Result<usize, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::practice::seed_prompts(&pool)
        .await
        .map_err(String::from)
}

/// Open a practice session and return its id.
#[tauri::command]
async fn start_session(kind: String, db: State<'_, DbInstances>) -> Result<String, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    // Seeding here rather than at startup keeps `run()` free of DB work and
    // means a database that predates the corpus picks it up on first use.
    crate::practice::seed_prompts(&pool)
        .await
        .map_err(String::from)?;
    crate::practice::start_session(&pool, &kind)
        .await
        .map_err(String::from)
}

#[tauri::command]
async fn end_session(session_id: String, db: State<'_, DbInstances>) -> Result<(), String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::practice::end_session(&pool, &session_id)
        .await
        .map_err(String::from)
}

/// Next prompt for a session, skipping ones it already covered.
#[tauri::command]
async fn next_prompt(
    session_id: Option<String>,
    category: Option<String>,
    level: Option<i64>,
    db: State<'_, DbInstances>,
) -> Result<Option<crate::practice::PromptView>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::practice::seed_prompts(&pool)
        .await
        .map_err(String::from)?;
    crate::practice::next_prompt(&pool, session_id.as_deref(), category.as_deref(), level)
        .await
        .map_err(String::from)
}

#[tauri::command]
async fn list_attempts(
    session_id: Option<String>,
    limit: i64,
    db: State<'_, DbInstances>,
) -> Result<Vec<crate::practice::AttemptRow>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::practice::list_attempts(&pool, session_id.as_deref(), limit)
        .await
        .map_err(String::from)
}

/// Transcribe a recording, score it, and store the attempt.
///
/// The centrepiece of the practice loop. Pronunciation is only scored when
/// `target_text` is present — free speaking gets grammar feedback and an
/// explicit "not scored" rather than an invented number.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn score_attempt(
    pcm_bytes: Vec<u8>,
    sample_rate: u32,
    session_id: Option<String>,
    prompt_id: Option<String>,
    target_text: Option<String>,
    dialect: Option<String>,
    db: State<'_, DbInstances>,
    state: State<'_, AppState>,
) -> Result<crate::practice::AttemptReport, String> {
    let pcm = Arc::new(decode_pcm(&pcm_bytes, sample_rate)?);
    let duration_ms =
        (pcm.len() as f64 * 1000.0 / crate::asr::ASR_SAMPLE_RATE as f64).round() as i64;
    // A whitespace-only target is free speaking, not a reference phrase;
    // normalizing here keeps the worker from allocating posteriors for it.
    let target = target_text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);

    let (tx, rx) = oneshot::channel();
    state
        .asr_tx
        .try_send(NeuralReq::TranscribeScored {
            pcm: pcm.clone(),
            target: target.clone(),
            reply: tx,
        })
        .map_err(busy_message)?;
    let scored = rx
        .await
        .map_err(|_| "ASR engine unavailable".to_string())??;

    // Grammar runs after ASR rather than in parallel: it needs the
    // transcript, and harper's types are !Send so it has to stay inside its
    // own `spawn_blocking`.
    let lint = crate::grammar::lint_text_async(scored.text.clone(), dialect).await?;

    let fluency = analyze_fluency(pcm, &scored, duration_ms).await?;

    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::practice::record_attempt(
        &pool,
        AttemptInput::new(&scored.text, duration_ms, lint)
            .session(session_id.as_deref())
            .prompt(prompt_id.as_deref())
            .target(target.as_deref())
            .scored(scored.pron, fluency),
    )
    .await
    .map_err(String::from)
}

/// Run delivery analysis off the ASR worker.
///
/// Fluency is pure CPU over the raw audio, so it must not occupy the
/// serialized `neural-asr` thread — that thread is the app's bottleneck and
/// every other recording queues behind it. `spawn_blocking` instead, on a
/// pool that exists for exactly this.
async fn analyze_fluency(
    pcm: Arc<Vec<f32>>,
    scored: &ScoredTranscript,
    duration_ms: i64,
) -> Result<Option<FluencyReport>, String> {
    let transcript = scored.text.clone();
    // Word timings come from the forced alignment when acoustic scoring
    // ran; without them the energy envelope finds the pauses instead.
    let words = scored.pron.as_ref().map(|p| p.words.clone());
    tokio::task::spawn_blocking(move || {
        crate::fluency::analyze(
            &pcm,
            crate::asr::ASR_SAMPLE_RATE,
            &transcript,
            words.as_deref(),
            duration_ms,
        )
    })
    .await
    .map_err(|e| format!("fluency analysis failed: {e}"))
}

/// Create a deck. The id is generated; `name` is what the user sees.
#[tauri::command]
async fn create_deck(name: String, db: State<'_, DbInstances>) -> Result<String, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::create_deck(&pool, &name)
        .await
        .map_err(String::from)
}

#[tauri::command]
async fn rename_deck(
    deck_id: String,
    name: String,
    db: State<'_, DbInstances>,
) -> Result<(), String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::rename_deck(&pool, &deck_id, &name)
        .await
        .map_err(String::from)
}

/// Delete a deck. `mode` is `"move"` (cards go to the default deck) or
/// `"delete"`. Returns how many cards were affected.
#[tauri::command]
async fn delete_deck(
    deck_id: String,
    mode: String,
    db: State<'_, DbInstances>,
) -> Result<usize, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let mode = crate::scheduler::DeckDeleteMode::parse(&mode).map_err(String::from)?;
    crate::scheduler::delete_deck(&pool, &deck_id, mode)
        .await
        .map_err(String::from)
}

/// Edit a card's text, keeping its scheduling history.
#[tauri::command]
async fn update_card(
    card_id: String,
    front: String,
    back: String,
    db: State<'_, DbInstances>,
) -> Result<(), String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::update_card(&pool, &card_id, &front, &back)
        .await
        .map_err(String::from)
}

/// Fetch due cards for review.
///
/// `deck_id` (`deckId` in JS) optionally restricts to one deck; `None`
/// means every deck. The filter is applied in SQL — see
/// [`crate::scheduler::DueOpts`]. `tz_offset_minutes` (`tzOffsetMinutes`,
/// default `0`) buckets "today" for the daily caps against the stored
/// `day_cutoff_hour` — see `crate::db::day_index`.
#[tauri::command]
async fn due_cards(
    limit: u32,
    deck_id: Option<String>,
    tz_offset_minutes: Option<i64>,
    db: State<'_, DbInstances>,
    state: State<'_, AppState>,
) -> Result<Vec<DueCardView>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let retention = refresh_retention(&pool, &state).await;
    let decay = read_decay(&state);
    let fsrs = state
        .fsrs
        .read()
        .map_err(|e| format!("fsrs lock poisoned: {e}"))?
        .clone();
    let cutoff = crate::prefs::cutoff_hour(&pool).await;
    let caps = crate::scheduler::daily_caps(
        &pool,
        deck_id.as_deref(),
        tz_offset_minutes.unwrap_or(0),
        cutoff,
    )
    .await
    .map_err(String::from)?;
    crate::scheduler::fetch_due_cards(
        &pool,
        &fsrs,
        retention,
        decay,
        crate::scheduler::DueOpts {
            limit: limit as i64,
            deck_id: deck_id.as_deref(),
            exclude_card_id: None,
            caps: Some(caps),
        },
    )
    .await
    .map_err(String::from)
}

/// Grade a card, returning the next due view (if any).
///
/// `tz_offset_minutes` (`tzOffsetMinutes`, default `0`) is threaded into
/// `grade_card_db` so the post-grade "next card" fetch respects today's
/// daily caps using the caller's local day, not UTC.
#[tauri::command]
async fn grade_card(
    card_id: String,
    rating: u32,
    tz_offset_minutes: Option<i64>,
    db: State<'_, DbInstances>,
    state: State<'_, AppState>,
) -> Result<Option<DueCardView>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let retention = refresh_retention(&pool, &state).await;
    let decay = read_decay(&state);
    let fsrs = state
        .fsrs
        .read()
        .map_err(|e| format!("fsrs lock poisoned: {e}"))?
        .clone();
    let cutoff = crate::prefs::cutoff_hour(&pool).await;
    crate::scheduler::grade_card_db(
        &pool,
        &fsrs,
        retention,
        decay,
        &card_id,
        rating,
        tz_offset_minutes.unwrap_or(0),
        cutoff,
    )
    .await
    .map_err(String::from)
}

/// All tags with their live card counts, ordered by name.
#[tauri::command]
async fn list_tags(db: State<'_, DbInstances>) -> Result<Vec<TagRow>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::list_tags(&pool)
        .await
        .map_err(String::from)
}

/// Replace a card's tags (`cardId` in JS), returning the normalized names
/// actually stored.
#[tauri::command]
async fn set_card_tags(
    card_id: String,
    tags: Vec<String>,
    db: State<'_, DbInstances>,
) -> Result<Vec<String>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::set_card_tags(&pool, &card_id, &tags)
        .await
        .map_err(String::from)
}

/// Suspend or unsuspend a card (`cardId` in JS).
#[tauri::command]
async fn suspend_card(
    card_id: String,
    suspended: bool,
    db: State<'_, DbInstances>,
) -> Result<(), String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::set_card_suspended(&pool, &card_id, suspended)
        .await
        .map_err(String::from)
}

/// Bury a card (`cardId` in JS) for `hours` from now. `Some(0)` clears the
/// bury; omitting `hours` falls back to the saved `bury_hours` preference,
/// so a plain "bury" button press does not need to know that default.
/// Returns the resulting `buried_until` unix timestamp (`0` when cleared).
#[tauri::command]
async fn bury_card(
    card_id: String,
    hours: Option<i64>,
    db: State<'_, DbInstances>,
) -> Result<i64, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let hours = match hours {
        Some(h) => h,
        None => {
            crate::prefs::load(&pool)
                .await
                .map_err(String::from)?
                .bury_hours
        }
    };
    let hours = hours.max(0);
    let until = if hours == 0 {
        0
    } else {
        crate::db::now_unix() + hours.saturating_mul(3600)
    };
    crate::scheduler::bury_card(&pool, &card_id, until)
        .await
        .map_err(String::from)
}

/// Undo the most recent review, if any.
#[tauri::command]
async fn undo_review(db: State<'_, DbInstances>) -> Result<Option<UndoResult>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::undo_last_review(&pool)
        .await
        .map_err(String::from)
}

/// Current user preferences, loaded from `app_settings`.
#[tauri::command]
async fn get_preferences(db: State<'_, DbInstances>) -> Result<Preferences, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::prefs::load(&pool).await.map_err(String::from)
}

/// Save preferences, returning the sanitized value actually stored.
#[tauri::command]
async fn set_preferences(
    prefs: Preferences,
    db: State<'_, DbInstances>,
) -> Result<Preferences, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::prefs::save(&pool, &prefs)
        .await
        .map_err(String::from)
}

/// Daily new/review caps in effect for `deck_id` (`deckId` in JS), or the
/// global defaults when omitted.
#[tauri::command]
async fn get_daily_limits(
    deck_id: Option<String>,
    db: State<'_, DbInstances>,
) -> Result<crate::scheduler::DailyLimits, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::get_daily_limits(&pool, deck_id.as_deref())
        .await
        .map_err(String::from)
}

/// Persist new daily caps for `deck_id` (`deckId` in JS), or globally when
/// omitted.
#[tauri::command]
async fn set_daily_limits(
    deck_id: Option<String>,
    new_per_day: i64,
    review_per_day: i64,
    db: State<'_, DbInstances>,
) -> Result<crate::scheduler::DailyLimits, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::set_daily_limits(&pool, deck_id.as_deref(), new_per_day, review_per_day)
        .await
        .map_err(String::from)
}

/// Seed the demo deck. Full-path call avoids colliding with this command name.
///
/// Contract: returns the inserted count so the frontend can toast it.
#[tauri::command]
async fn seed_demo_deck(db: State<'_, DbInstances>) -> Result<usize, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::seed_demo_deck(&pool)
        .await
        .map_err(String::from)
}

/// Re-optimize FSRS weights from review logs, swap them into `AppState`, return them.
///
/// On success updates BOTH `fsrs` and the cached `params` (so `read_decay`
/// picks up the optimized decay at `params[20]`), and persists the 21
/// params to `fsrs_params` for reload at startup (best-effort: persistence
/// failure is logged but does not fail the command).
#[tauri::command]
async fn optimize_parameters(
    db: State<'_, DbInstances>,
    state: State<'_, AppState>,
) -> Result<Vec<f32>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let params = crate::scheduler::optimize_parameters(&pool)
        .await
        .map_err(String::from)?;
    let fsrs = FSRS::new(&params).map_err(|e| e.to_string())?;
    *state
        .fsrs
        .write()
        .map_err(|e| format!("fsrs lock poisoned: {e}"))? = fsrs;
    *state
        .params
        .write()
        .map_err(|e| format!("params lock poisoned: {e}"))? = params.clone();
    if let Err(e) = crate::scheduler::save_fsrs_params(&pool, &params)
        .await
        .map_err(String::from)
    {
        eprintln!("optimize: failed to persist fsrs_params: {e}");
    }
    Ok(params)
}

/// Current desired retention (reloaded from `app_settings`, cached in state).
#[tauri::command]
async fn get_retention(
    db: State<'_, DbInstances>,
    state: State<'_, AppState>,
) -> Result<f32, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    Ok(refresh_retention(&pool, &state).await)
}

/// Persist a new desired retention (`0.70..=0.98`), returning it.
#[tauri::command]
async fn set_retention(
    retention: f32,
    db: State<'_, DbInstances>,
    state: State<'_, AppState>,
) -> Result<f32, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::save_retention(&pool, retention)
        .await
        .map_err(String::from)?;
    if let Ok(mut guard) = state.retention.write() {
        *guard = retention;
    }
    Ok(retention)
}

/// Insert a new card into `deck_id`; returns the new card id.
#[tauri::command]
async fn add_card(
    deck_id: String,
    front: String,
    back: String,
    db: State<'_, DbInstances>,
) -> Result<String, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::add_card(&pool, &deck_id, &front, &back)
        .await
        .map_err(String::from)
}

/// Most recent review-log rows, newest first.
#[tauri::command]
async fn recent_reviews(limit: u32, db: State<'_, DbInstances>) -> Result<Vec<ReviewRow>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::recent_reviews(&pool, limit as i64)
        .await
        .map_err(String::from)
}

/// All decks, ordered by name (scheduler helper owns the query).
#[tauri::command]
async fn list_decks(db: State<'_, DbInstances>) -> Result<Vec<DeckRow>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::list_decks(&pool)
        .await
        .map_err(String::from)
}

/// Cards in one deck (`deckId` in JS), or all cards when omitted.
#[tauri::command]
async fn list_cards(
    deck_id: Option<String>,
    db: State<'_, DbInstances>,
) -> Result<Vec<CardRow>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::list_cards(&pool, deck_id)
        .await
        .map_err(String::from)
}

/// Delete a card (`cardId` in JS) with its memory + review-log rows.
#[tauri::command]
async fn delete_card(card_id: String, db: State<'_, DbInstances>) -> Result<(), String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::delete_card(&pool, &card_id)
        .await
        .map_err(String::from)
}

/// Aggregate review stats for optimizer gating (`distinct_cards` /
/// `total_reviews` / `trainable_cards` / `train_items` + the two minimums).
#[tauri::command]
async fn review_stats(db: State<'_, DbInstances>) -> Result<ReviewStats, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    crate::scheduler::review_stats(&pool)
        .await
        .map_err(String::from)
}

/// On-device model presence: ASR model, ASR vocab, TTS voice.
#[tauri::command]
fn model_status() -> ModelStatus {
    ModelStatus {
        asr_model: crate::asr::find_model().is_some(),
        asr_vocab: crate::asr::vocab_available(),
        tts_voice: crate::tts::default_voice_path().is_some_and(|p| p.is_file()),
    }
}

/// Installed TTS voices as `{id, label}` pairs, sorted by id.
#[tauri::command]
fn list_voices() -> Vec<VoiceInfo> {
    crate::tts::available_voices()
        .into_iter()
        .map(|(id, label)| VoiceInfo { id, label })
        .collect()
}

/// Human-readable execution-provider report (CUDA → CoreML → DirectML → …).
#[tauri::command]
fn ep_report() -> String {
    crate::inference::describe_providers().to_string()
}

// ─── Data safety, stats, voice selection (Stream B2) ────────────────────────

/// Result of `export_data`: where it wrote to, how many cards, and the
/// resulting file size — enough for the frontend to show a confirmation
/// without re-reading the file.
#[derive(Clone, Debug, serde::Serialize)]
struct ExportResult {
    path: String,
    cards: i64,
    bytes: u64,
}

/// Result of `backup_database`.
#[derive(Clone, Debug, serde::Serialize)]
struct BackupResult {
    path: String,
    bytes: u64,
}

/// Export a deck (`deckId`, or every deck when omitted) to `path` in
/// `format` (`"json"`, `"csv"`, or `"tsv"`). All file I/O happens here, in
/// Rust — the frontend only ever supplies a path chosen through the dialog
/// plugin's save picker.
#[tauri::command]
async fn export_data(
    deck_id: Option<String>,
    path: String,
    format: String,
    db: State<'_, DbInstances>,
) -> Result<ExportResult, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let env = crate::portable::export_json(&pool, deck_id.as_deref())
        .await
        .map_err(String::from)?;
    let cards = env.cards.len() as i64;
    let contents = match format.as_str() {
        "json" => serde_json::to_string_pretty(&env)
            .map_err(|e| format!("failed to serialize export: {e}"))?,
        "csv" => crate::portable::export_csv(&env, ','),
        "tsv" => crate::portable::export_csv(&env, '\t'),
        other => {
            return Err(format!(
                "unknown export format {other:?}: expected \"json\", \"csv\", or \"tsv\""
            ))
        }
    };
    tokio::fs::write(&path, contents.as_bytes())
        .await
        .map_err(|e| format!("failed to write export file: {e}"))?;
    Ok(ExportResult {
        path,
        cards,
        bytes: contents.len() as u64,
    })
}

/// Import cards from `path` into `deckId` (CSV/TSV only; ignored for a JSON
/// envelope, which carries its own deck ids). Format is sniffed from the
/// file's own content (a JSON envelope starts with `{`), not the extension,
/// so a renamed file still imports correctly.
#[tauri::command]
async fn import_data(
    path: String,
    deck_id: Option<String>,
    db: State<'_, DbInstances>,
) -> Result<ImportSummary, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let contents = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("failed to read import file: {e}"))?;
    if contents.trim_start().starts_with('{') {
        let env: ExportEnvelope = serde_json::from_str(&contents)
            .map_err(|e| format!("failed to parse import file as JSON: {e}"))?;
        crate::portable::import_json(&pool, &env)
            .await
            .map_err(String::from)
    } else {
        let rows = crate::portable::parse_csv_cards(&contents)?;
        let deck_id = deck_id.unwrap_or_else(|| crate::scheduler::DEFAULT_DECK_ID.to_string());
        crate::portable::import_csv_cards(&pool, &deck_id, &rows)
            .await
            .map_err(String::from)
    }
}

/// Snapshot the live database to `path` via `VACUUM INTO`.
#[tauri::command]
async fn backup_database(path: String, db: State<'_, DbInstances>) -> Result<BackupResult, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let dest = std::path::PathBuf::from(&path);
    let bytes = crate::backup::backup_to(&pool, &dest)
        .await
        .map_err(String::from)?;
    Ok(BackupResult { path, bytes })
}

/// Validate a backup file and stage it for restore on next launch.
///
/// Never touches the live database file — only [`apply_pending_restore`]
/// (run from the `db-restore` plugin's `setup` hook, before the SQL plugin
/// opens its pool) does that. The frontend is expected to tell the user a
/// restart is required.
#[tauri::command]
async fn restore_database(path: String, app: tauri::AppHandle) -> Result<BackupInfo, String> {
    let src = std::path::PathBuf::from(&path);
    let info = crate::backup::validate_backup(&src)
        .await
        .map_err(String::from)?;
    let db_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("could not resolve app config dir: {e}"))?;
    crate::backup::stage_restore(&src, &db_dir).map_err(String::from)?;
    Ok(info)
}

/// Overview tiles for the Progress screen.
#[tauri::command]
async fn stats_overview(
    tz_offset_minutes: Option<i64>,
    db: State<'_, DbInstances>,
) -> Result<Overview, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let cutoff = crate::prefs::cutoff_hour(&pool).await;
    crate::stats::overview(&pool, tz_offset_minutes.unwrap_or(0), cutoff)
        .await
        .map_err(String::from)
}

/// Reviews-per-day series over the last `days` days.
#[tauri::command]
async fn stats_daily(
    days: i64,
    tz_offset_minutes: Option<i64>,
    db: State<'_, DbInstances>,
) -> Result<Vec<DayCount>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let cutoff = crate::prefs::cutoff_hour(&pool).await;
    crate::stats::daily(&pool, days, tz_offset_minutes.unwrap_or(0), cutoff)
        .await
        .map_err(String::from)
}

/// Due-card forecast for the next `days` days.
#[tauri::command]
async fn stats_forecast(
    days: i64,
    tz_offset_minutes: Option<i64>,
    db: State<'_, DbInstances>,
) -> Result<Vec<ForecastDay>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let cutoff = crate::prefs::cutoff_hour(&pool).await;
    crate::stats::forecast(&pool, days, tz_offset_minutes.unwrap_or(0), cutoff)
        .await
        .map_err(String::from)
}

/// Retention rate over the last `days` days, bucketed every `bucketDays`.
#[tauri::command]
async fn stats_retention(
    days: i64,
    bucket_days: i64,
    tz_offset_minutes: Option<i64>,
    db: State<'_, DbInstances>,
) -> Result<Vec<RetentionBucket>, String> {
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let cutoff = crate::prefs::cutoff_hour(&pool).await;
    crate::stats::retention(
        &pool,
        days,
        bucket_days,
        tz_offset_minutes.unwrap_or(0),
        cutoff,
    )
    .await
    .map_err(String::from)
}

/// The active TTS voice id (`""` means the default voice).
#[tauri::command]
async fn get_voice(state: State<'_, AppState>) -> Result<String, String> {
    let voice = state
        .voice
        .read()
        .map_err(|e| format!("voice lock poisoned: {e}"))?
        .clone()
        .unwrap_or_default();
    Ok(voice)
}

/// Set the active TTS voice (`""` clears back to the default), validated
/// with [`crate::tts::resolve_voice`] — the same rejection rule the neural
/// worker and the `audiostream://` handler apply, since this id ends up
/// concatenated into a filesystem path in all three places. Persists
/// through `prefs` and updates the live cache so the very next synthesis
/// uses it. Returns the id actually stored.
#[tauri::command]
async fn set_voice(
    voice_id: String,
    db: State<'_, DbInstances>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let trimmed = voice_id.trim();
    if !trimmed.is_empty() && crate::tts::resolve_voice(trimmed).is_none() {
        return Err(format!("unknown voice id {voice_id:?}"));
    }
    let stored = trimmed.to_string();

    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    let mut prefs = crate::prefs::load(&pool).await.map_err(String::from)?;
    prefs.tts_voice = stored.clone();
    crate::prefs::save(&pool, &prefs)
        .await
        .map_err(String::from)?;

    if let Ok(mut guard) = state.voice.write() {
        *guard = Some(stored.clone());
    }
    Ok(stored)
}

// ─── audiostream Range support ───────────────────────────────────────────────

/// Parse a single-range `Range` header (`bytes=S-E`, `bytes=S-`, `bytes=-N`).
///
/// Returns `Ok(None)` when the header is absent or malformed (serve 200),
/// `Ok(Some((start, end)))` inclusive bounds (serve 206), or `Err(())`
/// when unsatisfiable (serve 416).
fn parse_single_range(header: Option<&str>, total: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(h) = header else {
        return Ok(None);
    };
    let Some(spec) = h.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') {
        // Multipart ranges are not supported; serve the full body.
        return Ok(None);
    }
    let Some((s, e)) = spec.split_once('-') else {
        return Ok(None);
    };
    let s = s.trim();
    let e = e.trim();
    if total == 0 {
        return Err(());
    }
    if s.is_empty() {
        // Suffix range: last N bytes.
        let Ok(n) = e.parse::<u64>() else {
            return Ok(None);
        };
        if n == 0 {
            return Ok(None);
        }
        return Ok(Some((total.saturating_sub(n), total - 1)));
    }
    let Ok(start) = s.parse::<u64>() else {
        return Ok(None);
    };
    if start >= total {
        return Err(());
    }
    if e.is_empty() {
        return Ok(Some((start, total - 1)));
    }
    let Ok(end) = e.parse::<u64>() else {
        return Ok(None);
    };
    if end < start {
        return Ok(None);
    }
    Ok(Some((start, end.min(total - 1))))
}

// ─── App entry ───────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Domain 3 workers start before the webview exists; commands and the
    // protocol handler only hold the `Sender` halves.
    let (asr_tx, asr_rx) = mpsc::channel::<NeuralReq>(8);
    let (tts_tx, tts_rx) = mpsc::channel::<NeuralReq>(8);
    spawn_asr_worker(asr_rx);
    spawn_tts_worker(tts_rx);

    let migrations = vec![
        Migration {
            version: 1,
            description: "cards-memory",
            sql: MIGRATION_1_SQL,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 2,
            description: "review-logs",
            sql: MIGRATION_2_SQL,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 3,
            description: "retention-next-due",
            sql: MIGRATION_3_SQL,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 4,
            description: "fsrs-params-decks-due-index",
            sql: MIGRATION_4_SQL,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 5,
            description: "app-settings-deck-config-card-flags-repair",
            sql: MIGRATION_5_SQL,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 6,
            description: "prompts-sessions-attempts-scores",
            sql: MIGRATION_6_SQL,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 7,
            description: "tags-review-undo-daily-cap-defaults",
            sql: MIGRATION_7_SQL,
            kind: MigrationKind::Up,
        },
    ];

    tauri::Builder::default()
        .manage(AppState {
            asr_tx,
            tts_tx,
            fsrs: RwLock::new(FSRS::default()),
            params: RwLock::new(Vec::new()),
            retention: RwLock::new(crate::scheduler::DEFAULT_RETENTION),
            voice: RwLock::new(None),
        })
        .plugin(tauri_plugin_dialog::init())
        // Registered BEFORE the SQL plugin so its `setup` hook runs first:
        // Tauri 2.11 stores plugins in a `Vec` and `initialize_all` iterates
        // it in registration order, and `tauri-plugin-sql` 2.4.1 opens its
        // pool from inside its own `setup` hook (see `path_mapper` in that
        // crate: `sqlite:app.db` resolves to `app_config_dir()/app.db`,
        // the same directory `apply_pending_restore` operates on). Applying
        // a staged restore has to finish before that pool is opened, or the
        // running app would keep using the file descriptor of the database
        // that just got renamed out from under it.
        .plugin(
            tauri::plugin::Builder::new("db-restore")
                .setup(|app, _api: tauri::plugin::PluginApi<_, ()>| {
                    if let Ok(dir) = app.path().app_config_dir() {
                        match crate::backup::apply_pending_restore(&dir) {
                            Ok(true) => eprintln!("restored database from staged backup"),
                            Ok(false) => {}
                            Err(e) => eprintln!("restore failed, keeping current database: {e}"),
                        }
                    }
                    Ok(())
                })
                .build(),
        )
        .plugin(
            SqlBuilder::default()
                .add_migrations(DB_URL, migrations)
                .build(),
        )
        .setup(|app| {
            // Snapshot model search roots for the handle-less neural
            // workers: bundled `$RESOURCE/models/` first, then the
            // app-data dir (user-installed voices). CWD relatives stay
            // as fallback inside the asr/tts loaders.
            let mut roots = Vec::new();
            if let Ok(res) = app.path().resource_dir() {
                roots.push(res.join("models"));
            }
            if let Ok(data) = app.path().app_data_dir() {
                roots.push(data.join("models"));
            }
            crate::paths::init(roots);
            // Best-effort restore of persisted optimizer output: validate
            // len==21 before `FSRS::new`, else fall back to default.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Retry: the SQL preload may still be initializing on a
                // slow first launch; fall back to defaults only after.
                let mut attempts = 0;
                let pool = loop {
                    match crate::db::sqlite_pool(&handle.state::<DbInstances>()).await {
                        Ok(p) => break Some(p),
                        Err(e) => {
                            attempts += 1;
                            if attempts >= 4 {
                                eprintln!("fsrs params load: db unavailable ({e}); using default");
                                break None;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(300 * attempts))
                                .await;
                        }
                    }
                };
                let Some(pool) = pool else {
                    return;
                };
                // Best-effort: an unreadable preferences row just leaves the
                // voice at its `RwLock::new(None)` default (the default
                // voice), same failure mode as everything else in this task.
                if let Ok(prefs) = crate::prefs::load(&pool).await {
                    let st = handle.state::<AppState>();
                    if let Ok(mut g) = st.voice.write() {
                        *g = Some(prefs.tts_voice);
                    };
                }
                match crate::scheduler::load_fsrs_params(&pool).await {
                    Ok(Some(params)) if params.len() == 21 => match FSRS::new(&params) {
                        Ok(fsrs) => {
                            let st = handle.state::<AppState>();
                            if let Ok(mut g) = st.fsrs.write() {
                                *g = fsrs;
                            };
                            if let Ok(mut p) = st.params.write() {
                                *p = params;
                            };
                        }
                        Err(e) => {
                            eprintln!("fsrs params load: invalid params ({e}); using default");
                        }
                    },
                    Ok(Some(params)) => {
                        eprintln!(
                            "fsrs params load: len=={} != 21; using default",
                            params.len()
                        );
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("fsrs params load failed ({e}); using default");
                    }
                }
            });
            Ok(())
        })
        // Domain 1 → 3: async protocol; TTS goes through the `neural-tts`
        // worker via blocking channel ops on a spawned thread, with HTTP
        // single-range support. Long text is split via `split_sentences`
        // and re-joined with `concat_wav_mono16` for consistency with the
        // `synthesize_speech` command. `?text=` is capped at
        // AUDIOSTREAM_MAX_CHARS (414 over limit); empty input is 400.
        // Overload never sends partial: admission is checked against the
        // channel maximum and reported as `TTS_BUSY:`.
        .register_asynchronous_uri_scheme_protocol("audiostream", |ctx, req, responder| {
            let uri_string = req.uri().to_string();
            let text = crate::audio::decode_query_param(&uri_string, "text").unwrap_or_default();
            // `?voice=` overrides the stored voice for this one request;
            // validated the same way `set_voice` validates it, below.
            let voice_param = crate::audio::decode_query_param(&uri_string, "voice");
            let range = req
                .headers()
                .get(http::header::RANGE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let app = ctx.app_handle().clone();
            std::thread::spawn(move || {
                // 414: URI/query too long. Cap == max admittable chunks so
                // any accepted input can pass admission control below.
                if text.chars().count() > AUDIOSTREAM_MAX_CHARS {
                    responder.respond(
                        http::Response::builder()
                            .status(414)
                            .header(http::header::CONTENT_TYPE, "text/plain")
                            .body(
                                format!("TTS text too long (max {AUDIOSTREAM_MAX_CHARS} chars)")
                                    .into_bytes(),
                            )
                            .unwrap(),
                    );
                    return;
                }
                let wav_result: Result<Vec<u8>, String> = (|| {
                    if text.trim().is_empty() {
                        return Err("TTS: empty text".to_string());
                    }
                    let st = app.state::<AppState>();
                    // `?voice=` wins when present and valid; an invalid id
                    // is a hard error rather than a silent fallback to the
                    // default, same as `set_voice`.
                    let voice_id: Option<String> = match voice_param.as_deref() {
                        Some(v) if !v.is_empty() => {
                            if crate::tts::resolve_voice(v).is_none() {
                                return Err(format!("unknown voice id {v:?}"));
                            }
                            Some(v.to_string())
                        }
                        _ => st.voice.read().ok().and_then(|g| g.clone()),
                    };
                    let chunks = crate::audio::split_sentences(&text, 180);
                    let chunks = if chunks.is_empty() {
                        vec![text.clone()]
                    } else {
                        chunks
                    };
                    if chunks.len() > TTS_CHANNEL_SLOTS {
                        return Err(format!(
                            "TTS_BUSY: engine busy (need {} slots, max {TTS_CHANNEL_SLOTS})",
                            chunks.len()
                        ));
                    }
                    let mut wavs: Vec<Vec<u8>> = Vec::with_capacity(chunks.len());
                    for chunk in chunks {
                        let (tx, rx) = oneshot::channel();
                        // Non-blocking send: no TOCTOU race with concurrent
                        // streams, and overload surfaces as `TTS_BUSY:` 503
                        // instead of serializing behind another request.
                        // Atomicity still holds: nothing reaches the UI
                        // until all chunks are concatenated below.
                        st.tts_tx
                            .try_send(NeuralReq::Synth {
                                text: chunk,
                                voice_id: voice_id.clone(),
                                reply: tx,
                            })
                            .map_err(|_| "TTS_BUSY: engine busy, retry shortly".to_string())?;
                        let (wav, _) = rx
                            .blocking_recv()
                            .map_err(|_| "TTS engine unavailable".to_string())??;
                        wavs.push(wav);
                    }
                    crate::audio::concat_wav_mono16(wavs)
                })();

                let wav: Vec<u8> = match wav_result {
                    Ok(v) => v,
                    Err(e) => {
                        // Empty input is a client error (400); overload
                        // (`TTS_BUSY:`) and synthesis failures surface as
                        // 503 with the error string as body so the frontend
                        // can match the `TTS_BUSY:` prefix.
                        let status = if e.starts_with("TTS: empty text") {
                            400
                        } else {
                            503
                        };
                        responder.respond(
                            http::Response::builder()
                                .status(status)
                                .header(http::header::CONTENT_TYPE, "text/plain")
                                .body(e.into_bytes())
                                .unwrap(),
                        );
                        return;
                    }
                };

                let b = wav;
                let resp: http::Response<Vec<u8>> = if b.is_empty() {
                    http::Response::builder()
                        .status(200)
                        .header(http::header::CONTENT_TYPE, "audio/wav")
                        .header(http::header::ACCEPT_RANGES, "bytes")
                        .header(http::header::CONTENT_LENGTH, "0")
                        .body(b)
                        .unwrap()
                } else {
                    let total = b.len() as u64;
                    match parse_single_range(range.as_deref(), total) {
                        Err(()) => http::Response::builder()
                            .status(416)
                            .header(http::header::CONTENT_TYPE, "text/plain")
                            .header(http::header::CONTENT_RANGE, format!("bytes */{total}"))
                            .body(b"requested range not satisfiable".to_vec())
                            .unwrap(),
                        Ok(None) => http::Response::builder()
                            .status(200)
                            .header(http::header::CONTENT_TYPE, "audio/wav")
                            .header(http::header::ACCEPT_RANGES, "bytes")
                            .header(http::header::CONTENT_LENGTH, b.len().to_string())
                            .body(b)
                            .unwrap(),
                        Ok(Some((start, end))) => {
                            let body = b[start as usize..=end as usize].to_vec();
                            http::Response::builder()
                                .status(206)
                                .header(http::header::CONTENT_TYPE, "audio/wav")
                                .header(http::header::ACCEPT_RANGES, "bytes")
                                .header(
                                    http::header::CONTENT_RANGE,
                                    format!("bytes {start}-{end}/{total}"),
                                )
                                .header(http::header::CONTENT_LENGTH, body.len().to_string())
                                .body(body)
                                .unwrap()
                        }
                    }
                };
                responder.respond(resp);
            });
        })
        .invoke_handler(tauri::generate_handler![
            transcribe_pcm_channel,
            score_attempt,
            seed_prompts,
            start_session,
            end_session,
            next_prompt,
            list_attempts,
            create_deck,
            rename_deck,
            delete_deck,
            update_card,
            lint_text,
            synthesize_speech,
            due_cards,
            grade_card,
            seed_demo_deck,
            optimize_parameters,
            ep_report,
            get_retention,
            set_retention,
            add_card,
            recent_reviews,
            list_decks,
            list_cards,
            delete_card,
            review_stats,
            model_status,
            list_voices,
            list_tags,
            set_card_tags,
            suspend_card,
            bury_card,
            undo_review,
            get_preferences,
            set_preferences,
            get_daily_limits,
            set_daily_limits,
            export_data,
            import_data,
            backup_database,
            restore_database,
            stats_overview,
            stats_daily,
            stats_forecast,
            stats_retention,
            get_voice,
            set_voice
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn asr_worker_replies_err_on_synth_misroute() {
        let (tx, rx) = mpsc::channel::<NeuralReq>(8);
        spawn_asr_worker(rx);
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.try_send(NeuralReq::Synth {
            text: "hello".to_string(),
            voice_id: None,
            reply: reply_tx,
        })
        .expect("send");
        let res = tokio::time::timeout(Duration::from_secs(2), reply_rx)
            .await
            .expect("asr worker must reply (no hang)")
            .expect("oneshot open");
        assert!(res.is_err(), "misrouted Synth to ASR must Err, got Ok");
    }

    #[tokio::test]
    async fn tts_worker_replies_err_on_transcribe_misroute() {
        let (tx, rx) = mpsc::channel::<NeuralReq>(8);
        spawn_tts_worker(rx);
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.try_send(NeuralReq::Transcribe {
            pcm: vec![0.0; 16],
            reply: reply_tx,
        })
        .expect("send");
        let res = tokio::time::timeout(Duration::from_secs(2), reply_rx)
            .await
            .expect("tts worker must reply (no hang)")
            .expect("oneshot open");
        assert!(res.is_err(), "misrouted Transcribe to TTS must Err, got Ok");
    }

    #[test]
    fn concat_single_passthrough_and_multi_joins() {
        // Integration with the canonical `audio::concat_wav_mono16`
        // (owned by audio.rs): single round-trips, multi joins PCM.
        let a = crate::audio::encode_wav_mono_16bit(&[0.0, 0.5], 22_050);
        let b = crate::audio::encode_wav_mono_16bit(&[0.25, -0.25], 22_050);
        let single = crate::audio::concat_wav_mono16(vec![a.clone()]).expect("single");
        assert_eq!(single, a);
        let joined = crate::audio::concat_wav_mono16(vec![a.clone(), b.clone()]).expect("join");
        assert_eq!(crate::audio::parse_wav_sample_rate(&joined), Some(22_050));
        // PCM payload is the concatenation of both data payloads
        // (44-byte headers stripped by the canonical helper).
        let pa = a[44..].to_vec();
        let pb = b[44..].to_vec();
        let pj = joined[44..].to_vec();
        assert_eq!(pj.len(), pa.len() + pb.len());
        assert_eq!(&pj[..pa.len()], &pa[..]);
        assert_eq!(&pj[pa.len()..], &pb[..]);
        assert!(crate::audio::concat_wav_mono16(vec![]).is_err());
    }

    #[test]
    fn tts_busy_prefix_on_full() {
        let (tx, _rx) = mpsc::channel::<NeuralReq>(1);
        // Fill the single slot.
        let (dtx, _drx) = oneshot::channel();
        tx.try_send(NeuralReq::Synth {
            text: "x".to_string(),
            voice_id: None,
            reply: dtx,
        })
        .unwrap();
        let (dtx2, _drx2) = oneshot::channel();
        let err = tx
            .try_send(NeuralReq::Synth {
                text: "y".to_string(),
                voice_id: None,
                reply: dtx2,
            })
            .expect_err("second send must be full");
        assert!(tts_busy_message(err).starts_with("TTS_BUSY:"));
    }
}
