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
mod db;
mod error;
mod grammar;
mod inference;
mod paths;
mod scheduler;
mod tts;

use std::collections::HashSet;
use std::sync::RwLock;

use fsrs::{FSRS, FSRS6_DEFAULT_DECAY};
use tauri::ipc::{Channel, Response};
use tauri::{Emitter, Manager, State};
use tauri_plugin_sql::{Builder as SqlBuilder, DbInstances, Migration, MigrationKind};
use tokio::sync::{mpsc, oneshot};

use crate::asr::AsrEngine;
use crate::db::{DB_URL, MIGRATION_1_SQL, MIGRATION_2_SQL, MIGRATION_3_SQL, MIGRATION_4_SQL};
use crate::grammar::LintOutput;
use crate::scheduler::{CardRow, DeckRow, DueCardView, ReviewRow, ReviewStats};
use crate::tts::TtsEngine;

// ─── Neural workers (Domain 3) ───────────────────────────────────────────────

/// Request envelope for the neural worker threads. Each request carries a
/// `oneshot` reply so commands can await exactly their own result.
pub enum NeuralReq {
    Transcribe {
        pcm: Vec<f32>,
        reply: oneshot::Sender<Result<String, String>>,
    },
    Synth {
        text: String,
        reply: oneshot::Sender<Result<(Vec<u8>, u32), String>>,
    },
}

/// Shared state: channel handles to the neural workers plus FSRS state.
/// All fields are `Send + Sync`, so `AppState` is too.
pub struct AppState {
    pub asr_tx: mpsc::Sender<NeuralReq>,
    pub tts_tx: mpsc::Sender<NeuralReq>,
    pub fsrs: RwLock<FSRS>,
    pub params: RwLock<Vec<f32>>,
    pub retention: RwLock<f32>,
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
                        let res: Result<String, String> = (|| {
                            if engine.is_none() {
                                let path = crate::asr::find_model().ok_or_else(|| {
                                    "ASR_NO_MODEL: model not installed; install model to enable transcription"
                                        .to_string()
                                })?;
                                let eng = AsrEngine::load(&path)
                                    .map_err(|e| format!("ASR_NO_MODEL: failed to load model: {e}"))?;
                                engine = Some(eng);
                            }
                            let eng = engine
                                .as_ref()
                                .ok_or_else(|| "ASR_NO_MODEL: model not installed".to_string())?;
                            eng.transcribe_pcm(&pcm).map_err(|e| e.to_string())
                        })();
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

/// Spawn the dedicated TTS thread: lazy-loads the voice on first request,
/// then serves `Synth` in FIFO order until the channel closes.
///
/// Misrouted `Transcribe` requests are rejected with `Err` (never dropped)
/// so no `rx.await` hangs forever.
fn spawn_tts_worker(rx: mpsc::Receiver<NeuralReq>) {
    std::thread::Builder::new()
        .name("neural-tts".to_string())
        .spawn(move || {
            let mut engine: Option<TtsEngine> = None;
            let mut rx = rx;
            while let Some(req) = rx.blocking_recv() {
                match req {
                    NeuralReq::Synth { text, reply } => {
                        let res: Result<(Vec<u8>, u32), String> = (|| {
                            if engine.is_none() {
                                let (model, config) =
                                    crate::tts::find_voice().ok_or_else(|| {
                                        "TTS voice not installed; install voice to enable speech"
                                            .to_string()
                                    })?;
                                let eng =
                                    TtsEngine::load(&model, &config).map_err(|e| e.to_string())?;
                                engine = Some(eng);
                            }
                            let eng = engine
                                .as_ref()
                                .ok_or_else(|| "TTS voice not installed".to_string())?;
                            let wav = eng.synthesize_wav(&text).map_err(|e| e.to_string())?;
                            Ok((wav, eng.sample_rate()))
                        })();
                        let _ = reply.send(res);
                    }
                    NeuralReq::Transcribe { reply, .. } => {
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

/// Best-effort retention refresh: reload from `settings`, update the cache,
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
#[tauri::command]
async fn transcribe_pcm_channel(
    channel: Channel<String>,
    pcm_bytes: Vec<u8>,
    sample_rate: u32,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    if sample_rate != 16_000 {
        return Err(format!("expected 16kHz mono, got {sample_rate}Hz"));
    }
    let mut pcm = crate::asr::AsrEngine::bytes_to_f32_mono(&pcm_bytes);
    if pcm.len() > 16_000 * 120 {
        return Err(format!(
            "audio too long ({} samples); max is {} samples (120s at 16kHz)",
            pcm.len(),
            16_000 * 120
        ));
    }
    for x in pcm.iter_mut() {
        if !x.is_finite() {
            *x = 0.0;
        }
    }

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

    let mut wavs: Vec<Vec<u8>> = Vec::with_capacity(chunks.len());
    let mut rate: Option<u32> = None;
    for chunk in chunks.iter() {
        let (tx, rx) = oneshot::channel();
        state
            .tts_tx
            .try_send(NeuralReq::Synth {
                text: chunk.clone(),
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

/// Fetch due cards for review.
///
/// `deck_id` (`deckId` in JS) optionally restricts to one deck; `None`
/// preserves the all-decks behavior. Filtering happens here: over-fetch,
/// keep cards in the deck, then truncate back to `limit` so a filtered
/// result still fills the page.
#[tauri::command]
async fn due_cards(
    limit: u32,
    deck_id: Option<String>,
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
    let fetch_limit: i64 = match &deck_id {
        None => limit as i64,
        // Filtered path: fetch everything due, filter, then truncate so
        // the deck still fills `limit` (unfiltered path is unchanged).
        Some(_) => i64::MAX,
    };
    let mut due = crate::scheduler::fetch_due_cards(&pool, &fsrs, retention, decay, fetch_limit)
        .await
        .map_err(String::from)?;
    if let Some(deck) = deck_id.as_deref() {
        let rows: Vec<String> =
            sqlx::query_scalar::<_, String>("SELECT id FROM cards WHERE deck_id = ?")
                .bind(deck)
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?;
        let ids: HashSet<String> = rows.into_iter().collect();
        due.retain(|card| ids.contains(card.id.as_str()));
        due.truncate(limit as usize);
    }
    Ok(due)
}

/// Grade a card, returning the next due view (if any).
#[tauri::command]
async fn grade_card(
    card_id: String,
    rating: u32,
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
    crate::scheduler::grade_card_db(&pool, &fsrs, retention, decay, &card_id, rating)
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

/// Current desired retention (reloaded from `settings`, cached in state).
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
    if !(0.70..=0.98).contains(&retention) {
        return Err(format!(
            "retention {retention} out of range: expected 0.70..=0.98"
        ));
    }
    let pool = crate::db::sqlite_pool(&db).await.map_err(String::from)?;
    sqlx::query(
        "INSERT INTO settings(key, value) VALUES('retention', ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(retention as f64)
    .execute(&pool)
    .await
    .map_err(|e| e.to_string())?;
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

/// Aggregate review stats for optimizer gating
/// (`distinct_cards` / `total_reviews` / `min_histories`).
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
    ];

    tauri::Builder::default()
        .manage(AppState {
            asr_tx,
            tts_tx,
            fsrs: RwLock::new(FSRS::default()),
            params: RwLock::new(Vec::new()),
            retention: RwLock::new(0.9),
        })
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
            list_voices
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
            reply: dtx,
        })
        .unwrap();
        let (dtx2, _drx2) = oneshot::channel();
        let err = tx
            .try_send(NeuralReq::Synth {
                text: "y".to_string(),
                reply: dtx2,
            })
            .expect_err("second send must be full");
        assert!(tts_busy_message(err).starts_with("TTS_BUSY:"));
    }
}
