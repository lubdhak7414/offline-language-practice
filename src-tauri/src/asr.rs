//! On-device ASR via wav2vec2 ONNX (ort 2.0.0-rc.12).
//!
//! Greedy CTC decode (argmax over vocab, collapse blank + repeats).
//! `vocab.json` next to the model is required; silence, bad shapes and
//! missing vocab surface as `Err` with `ASR_*` prefixes (never debug text).
//!
//! [`AsrEngine::transcribe_pcm_detailed`] additionally returns the
//! log-softmax frame posteriors, which pronunciation scoring needs; plain
//! [`AsrEngine::transcribe_pcm`] never allocates them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ort::session::builder::GraphOptimizationLevel;
use ort::{session::Session, value::Tensor};

use crate::inference::ordered_providers;

/// Sample rate the exported wav2vec2 graph expects. Audio is resampled to
/// this in the frontend and re-checked at the command boundary.
pub const ASR_SAMPLE_RATE: u32 = 16_000;

/// Candidate model locations, checked in order (relative to process CWD),
/// then the startup-registered extra roots (`$RESOURCE/models/`, …).
pub const MODEL_PATH_CANDIDATES: &[&str] =
    &["models/wav2vec2.onnx", "src-tauri/models/wav2vec2.onnx"];

/// Return the first candidate path that exists on disk.
pub fn find_model() -> Option<PathBuf> {
    MODEL_PATH_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .chain(
            crate::paths::extra_model_dirs()
                .into_iter()
                .map(|d| d.join("wav2vec2.onnx")),
        )
        .find(|p| p.exists())
}

/// Candidate `vocab.json` locations: next to the resolved model plus the
/// conventional `models/vocab.json` paths (same roots as the model).
fn vocab_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(model) = find_model() {
        if let Some(parent) = model.parent() {
            out.push(parent.join("vocab.json"));
        }
    }
    for cand in ["models/vocab.json", "src-tauri/models/vocab.json"] {
        let p = PathBuf::from(cand);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    for dir in crate::paths::extra_model_dirs() {
        let p = dir.join("vocab.json");
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// True when a `vocab.json` resolves (same candidate paths the engine uses).
pub fn vocab_available() -> bool {
    vocab_candidates().iter().any(|p| p.is_file())
}

/// A parsed `vocab.json`, both directions plus the resolved blank id.
#[derive(Debug, Clone)]
pub struct Vocab {
    /// Token → id. Decoding only needs the reverse map; this direction is
    /// what pronunciation scoring uses to turn a target phrase into labels.
    pub token_to_id: HashMap<String, i64>,
    pub id_to_token: HashMap<i64, String>,
    /// CTC blank. wav2vec2 CTC checkpoints use `<pad>` for this; it is
    /// conventionally id 0 but that is a convention, not a guarantee, so it
    /// is read from the file and only falls back to 0 when absent.
    pub blank: i64,
}

impl Vocab {
    /// Parse a `vocab.json` body (a flat `{token: id}` map).
    ///
    /// Pure, so the id/blank handling is testable without a model on disk.
    pub fn parse(text: &str) -> Result<Self, String> {
        let token_to_id: HashMap<String, i64> =
            serde_json::from_str(text).map_err(|e| format!("invalid vocab.json: {e}"))?;
        if token_to_id.is_empty() {
            return Err("vocab.json is empty".to_string());
        }
        let mut id_to_token: HashMap<i64, String> = HashMap::new();
        for (tok, id) in token_to_id.iter() {
            id_to_token.entry(*id).or_insert_with(|| tok.clone());
        }
        let blank = token_to_id.get("<pad>").copied().unwrap_or(0);
        Ok(Self {
            token_to_id,
            id_to_token,
            blank,
        })
    }
}

/// Parsed vocab, cached for the process.
///
/// Only successes are cached: a user can install models while the app is
/// running, and caching the "not found" answer would make that require a
/// restart.
static VOCAB: OnceLock<Arc<Vocab>> = OnceLock::new();

/// Resolve and parse `vocab.json`, reusing the cached copy when present.
///
/// Before this cache the file was re-read and re-parsed from disk on every
/// single transcription.
pub fn load_vocab() -> ort::Result<Arc<Vocab>> {
    if let Some(v) = VOCAB.get() {
        return Ok(v.clone());
    }
    let path = vocab_candidates()
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| {
            ort::Error::new(
                "ASR_NO_VOCAB: vocab.json not found (no candidate resolves)".to_string(),
            )
        })?;
    let text = std::fs::read_to_string(&path).map_err(|e| {
        ort::Error::new(format!("ASR_NO_VOCAB: cannot read {}: {e}", path.display()))
    })?;
    let parsed =
        Arc::new(Vocab::parse(&text).map_err(|e| ort::Error::new(format!("ASR_NO_VOCAB: {e}")))?);
    // Two threads may parse concurrently on first use; whichever `set` lands
    // first wins and both callers go on to use that same instance.
    let _ = VOCAB.set(parsed.clone());
    Ok(VOCAB.get().cloned().unwrap_or(parsed))
}

/// Rewrite row-major `[frames, vocab]` logits into log-softmax in place.
///
/// Row-wise `x - max - ln(sum(exp(x - max)))`. The max subtraction is what
/// keeps `exp` from overflowing; the sum is accumulated in `f64` because a
/// long utterance sums thousands of terms. Rows that run past the end of the
/// slice are left untouched rather than panicking.
pub fn log_softmax_rows(logits: &mut [f32], frames: usize, vocab: usize) {
    if frames == 0 || vocab == 0 {
        return;
    }
    for t in 0..frames {
        let base = t.saturating_mul(vocab);
        let Some(row) = logits.get_mut(base..base + vocab) else {
            break;
        };
        let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if !max.is_finite() {
            continue;
        }
        let sum: f64 = row.iter().map(|&x| ((x - max) as f64).exp()).sum();
        let log_sum = (max as f64) + sum.ln();
        for x in row.iter_mut() {
            *x = (*x as f64 - log_sum) as f32;
        }
    }
}

/// Greedy CTC collapse over row-major `[frames, vocab]` logits.
///
/// Argmax per frame, skip `blank`, collapse repeats. Pure (no IO).
/// Short slices terminate early instead of panicking.
pub fn ctc_collapse_argmax(logits: &[f32], frames: usize, vocab: usize, blank: i64) -> Vec<i64> {
    if frames == 0 || vocab == 0 {
        return Vec::new();
    }
    let mut collapsed: Vec<i64> = Vec::new();
    let mut prev: Option<i64> = None;
    for t in 0..frames {
        let base = t.saturating_mul(vocab);
        if base >= logits.len() {
            break;
        }
        let mut best_id: i64 = 0;
        let mut best_val = logits[base];
        for v in 1..vocab {
            match logits.get(base + v) {
                Some(&val) => {
                    if val > best_val {
                        best_val = val;
                        best_id = v as i64;
                    }
                }
                None => break,
            }
        }
        if best_id != blank && Some(best_id) != prev {
            collapsed.push(best_id);
        }
        prev = Some(best_id);
    }
    collapsed
}

/// Render collapsed token ids via an id→token map. Pure (no IO).
///
/// Joins pieces, maps `|` to word boundaries, drops wav2vec2 specials.
pub fn decode_collapsed_ids(collapsed: &[i64], id_to_token: &HashMap<i64, String>) -> String {
    let joined: String = collapsed
        .iter()
        .filter_map(|id| id_to_token.get(id))
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("");
    joined
        .replace('|', " ")
        .replace("<pad>", "")
        .replace("<s>", "")
        .replace("</s>", "")
        .replace("<unk>", "")
}

/// Zero-mean / unit-variance normalization over one utterance (HF formula).
///
/// `x = (x - mean) / sqrt(var + eps)`, `eps = 1e-7`. Operates in place;
/// no-op on empty input.
pub fn normalize_utterance(samples: &mut [f32]) {
    if samples.is_empty() {
        return;
    }
    let n = samples.len() as f32;
    let mean = samples.iter().sum::<f32>() / n;
    let var = samples.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n;
    let std = (var + 1e-7).sqrt();
    for x in samples.iter_mut() {
        *x = (*x - mean) / std;
    }
}

/// True when the utterance is (near-)digital silence.
///
/// Threshold on peak absolute amplitude; empty input counts as silence.
pub fn is_silence(pcm: &[f32]) -> bool {
    pcm.iter().fold(0.0f32, |a, &b| a.max(b.abs())) < 1e-4
}

/// A transcription plus the frame posteriors it was decoded from.
///
/// `logp` is row-major `[frames, vocab]` log-softmax, i.e. every value is
/// `<= 0` and each row sums (in probability space) to 1.
#[derive(Debug, Clone)]
pub struct AsrOutput {
    pub text: String,
    pub logp: Vec<f32>,
    pub frames: usize,
    pub vocab: usize,
    /// Milliseconds of audio per output frame, derived from this run.
    /// Multiplied by `frames` it recovers the utterance length, so the
    /// duration is not stored a second time.
    pub frame_stride_ms: f32,
}

/// Wav2vec2 session wrapper. `Session::run` takes `&mut self`, hence the Mutex.
pub struct AsrEngine {
    session: Mutex<Session>,
    input_name: String,
}

impl AsrEngine {
    /// Build the session, probing EPs in `ordered_providers` order with
    /// automatic CPU fallback (no `error_on_failure`, so CPU fallback is allowed).
    pub fn load(model_path: &Path) -> ort::Result<Self> {
        // Half the cores for intra-op, clamped to 1..=4; inter-op stays
        // serial and the memory arena is off (large dynamic-length audio).
        let intra = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).clamp(1, 4))
            .unwrap_or(2);
        let session = Session::builder()?
            .with_execution_providers(ordered_providers())?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(intra)?
            .with_inter_threads(1)?
            .with_parallel_execution(false)?
            .with_memory_pattern(false)?
            .commit_from_file(model_path)?;
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .unwrap_or_else(|| "input_values".to_string());
        Ok(Self {
            session: Mutex::new(session),
            input_name,
        })
    }

    /// Run greedy CTC decode over `[1, n]` mono f32 PCM.
    ///
    /// Non-finite samples are mapped to `0.0`, then the utterance is
    /// normalized ([`normalize_utterance`]). Digital silence, unexpected
    /// logits shapes and missing vocab surface as `Err` with `ASR_*`
    /// prefixes (never debug text as transcript).
    pub fn transcribe_pcm(&self, pcm_f32: &[f32]) -> ort::Result<String> {
        // Vocab is resolved before inference: without it the transcript
        // cannot be rendered, so running the graph first would just burn
        // seconds on audio whose result is unusable.
        let vocab = load_vocab()?;
        self.with_logits(pcm_f32, |data, frames, n_vocab| {
            let collapsed = ctc_collapse_argmax(data, frames, n_vocab, vocab.blank);
            Ok(decode_collapsed_ids(&collapsed, &vocab.id_to_token))
        })
    }

    /// Transcribe and also return the frame posteriors.
    ///
    /// Same decode as [`Self::transcribe_pcm`], but the logits are kept and
    /// converted to log-softmax so pronunciation scoring can run a CTC
    /// forward pass over them. At the 120 s command cap this is roughly
    /// 6000 frames x 32 vocab x 4 B, under 1 MB, and it stays inside the ASR
    /// worker — it is never sent across a channel.
    pub fn transcribe_pcm_detailed(&self, pcm_f32: &[f32]) -> ort::Result<AsrOutput> {
        let vocab = load_vocab()?;
        let audio_samples = pcm_f32.len();
        self.with_logits(pcm_f32, |data, frames, n_vocab| {
            let len = frames.saturating_mul(n_vocab).min(data.len());
            let mut logp = data[..len].to_vec();
            log_softmax_rows(&mut logp, frames, n_vocab);
            let collapsed = ctc_collapse_argmax(&logp, frames, n_vocab, vocab.blank);
            let audio_ms = 1000.0 * audio_samples as f32 / ASR_SAMPLE_RATE as f32;
            Ok(AsrOutput {
                text: decode_collapsed_ids(&collapsed, &vocab.id_to_token),
                logp,
                frames,
                vocab: n_vocab,
                // Derived from the actual output length rather than assuming
                // wav2vec2's nominal 20 ms hop, so a re-exported or strided
                // model does not silently skew every word timing.
                frame_stride_ms: if frames == 0 {
                    0.0
                } else {
                    audio_ms / frames as f32
                },
            })
        })
    }

    /// Validate, normalize and run the graph, handing the raw `[frames,
    /// vocab]` logits to `f` while the session output is still alive.
    ///
    /// The borrow is why this is a closure rather than a returned slice: it
    /// lets [`Self::transcribe_pcm`] decode without copying the logits at
    /// all, while [`Self::transcribe_pcm_detailed`] copies them deliberately.
    fn with_logits<T>(
        &self,
        pcm_f32: &[f32],
        f: impl FnOnce(&[f32], usize, usize) -> ort::Result<T>,
    ) -> ort::Result<T> {
        if pcm_f32.is_empty() {
            return Err(ort::Error::new(
                "ASR_SILENCE: empty audio (0 samples)".to_string(),
            ));
        }
        let mut norm: Vec<f32> = pcm_f32
            .iter()
            .map(|x| if x.is_finite() { *x } else { 0.0 })
            .collect();
        if is_silence(&norm) {
            return Err(ort::Error::new(
                "ASR_SILENCE: no speech detected (digital silence)".to_string(),
            ));
        }
        normalize_utterance(&mut norm);
        let n = norm.len();
        let input = Tensor::<f32>::from_array(([1, n], norm.into_boxed_slice()))?;
        let input_name = self.input_name.clone();
        let mut guard = self
            .session
            .lock()
            .map_err(|e| ort::Error::new(format!("ASR mutex poisoned: {e}")))?;
        let outputs = guard.run(ort::inputs! { input_name => input })?;

        let (_, dyn_value) = outputs
            .iter()
            .next()
            .ok_or_else(|| ort::Error::new("ASR_SHAPE: session produced no outputs".to_string()))?;
        let (shape, data) = dyn_value.try_extract_tensor::<f32>()?;

        // Logits are [1, frames, vocab] (or [frames, vocab] for some exports).
        let (frames, vocab) = match shape.len() {
            3 => (shape[1] as usize, shape[2] as usize),
            2 => (shape[0] as usize, shape[1] as usize),
            _ => (0usize, 0usize),
        };
        if frames == 0 || vocab == 0 {
            return Err(ort::Error::new(format!(
                "ASR_SHAPE: unexpected logits shape (frames={frames}, vocab={vocab}): shape={shape:?}"
            )));
        }

        f(data, frames, vocab)
    }

    /// Interpret LE f32 bytes as mono samples; ignore trailing partial chunk.
    pub fn bytes_to_f32_mono(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_roundtrip() {
        let vals = [1.0f32, -2.5f32];
        let mut bytes = Vec::new();
        for v in vals {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let out = AsrEngine::bytes_to_f32_mono(&bytes);
        assert_eq!(out, vals);
    }

    #[test]
    fn normalize_zero_mean_unit_var() {
        let mut samples = vec![0.5f32, 1.5, -0.5, 2.5, 1.0];
        normalize_utterance(&mut samples);
        let n = samples.len() as f32;
        let mean = samples.iter().sum::<f32>() / n;
        let var = samples.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n;
        assert!(mean.abs() < 1e-5, "mean should be ~0, got {mean}");
        assert!((var.sqrt() - 1.0).abs() < 1e-4, "std should be ~1");
    }

    #[test]
    fn normalize_empty_is_noop() {
        let mut samples: Vec<f32> = Vec::new();
        normalize_utterance(&mut samples);
        assert!(samples.is_empty());
    }

    #[test]
    fn silence_detection() {
        assert!(is_silence(&[]));
        assert!(is_silence(&vec![0.0f32; 160]));
        assert!(is_silence(&[1e-5, -1e-5, 5e-5]));
        assert!(!is_silence(&[0.0, 0.0, 1e-3, 0.0]));
    }

    #[test]
    fn ctc_collapse_skips_blank_and_repeats() {
        // frames=4, vocab=3. Argmax per frame: [1, 1, 0, 2] -> collapsed [1, 2].
        let logits: Vec<f32> = vec![
            0.0, 5.0, 1.0, // frame 0 -> id 1
            0.0, 4.0, 1.0, // frame 1 -> id 1 (repeat, collapsed)
            9.0, 1.0, 1.0, // frame 2 -> blank 0
            0.0, 1.0, 7.0, // frame 3 -> id 2
        ];
        assert_eq!(ctc_collapse_argmax(&logits, 4, 3, 0), vec![1, 2]);
    }

    #[test]
    fn ctc_collapse_empty_and_truncated() {
        assert!(ctc_collapse_argmax(&[], 0, 3, 0).is_empty());
        assert!(ctc_collapse_argmax(&[], 4, 0, 0).is_empty());
        // Truncated slice terminates instead of panicking.
        let logits = vec![0.0f32, 1.0];
        let out = ctc_collapse_argmax(&logits, 4, 3, 0);
        assert!(out.len() <= 1);
    }

    #[test]
    fn decode_collapsed_ids_maps_pipe_and_specials() {
        let mut map: HashMap<i64, String> = HashMap::new();
        map.insert(1, "h".to_string());
        map.insert(2, "i".to_string());
        map.insert(3, "|".to_string());
        map.insert(4, "<pad>".to_string());
        assert_eq!(decode_collapsed_ids(&[1, 2], &map), "hi");
        assert_eq!(decode_collapsed_ids(&[1, 3, 2], &map), "h i");
        assert_eq!(decode_collapsed_ids(&[1, 4, 2], &map), "hi");
        assert_eq!(decode_collapsed_ids(&[99], &map), "");
    }

    #[test]
    fn mutex_poison_maps_to_err() {
        let m = std::sync::Mutex::new(());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = m.lock().unwrap();
            panic!("intentional poison for test");
        }));
        let res: ort::Result<()> = m
            .lock()
            .map(|_| ())
            .map_err(|e| ort::Error::new(format!("ASR mutex poisoned: {e}")));
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("ASR mutex poisoned"),
            "unexpected poison message: {err}"
        );
    }

    #[test]
    fn missing_vocab_returns_no_vocab_prefix() {
        // Without models on disk there is no vocab to resolve; the error
        // must carry the verbatim frontend prefix (never raw token ids).
        if vocab_available() {
            return;
        }
        let err = load_vocab().unwrap_err().to_string();
        assert!(
            err.starts_with("ASR_NO_VOCAB:"),
            "expected ASR_NO_VOCAB: prefix, got {err:?}"
        );
    }

    #[test]
    fn vocab_reads_blank_from_the_file() {
        // wav2vec2 conventionally puts <pad> at 0, but the id is read rather
        // than assumed: an export that numbers it differently must still
        // collapse against the right symbol.
        let v = Vocab::parse(r#"{"<pad>": 3, "a": 0, "b": 1, "|": 2}"#).expect("parse");
        assert_eq!(v.blank, 3);
        assert_eq!(v.token_to_id["a"], 0);
        assert_eq!(v.id_to_token[&2], "|");
    }

    #[test]
    fn vocab_defaults_blank_to_zero_without_pad() {
        let v = Vocab::parse(r#"{"a": 0, "b": 1}"#).expect("parse");
        assert_eq!(v.blank, 0);
    }

    #[test]
    fn vocab_rejects_junk_and_empty() {
        assert!(Vocab::parse("not json").is_err());
        assert!(Vocab::parse("{}").is_err());
    }

    #[test]
    fn collapse_honours_a_non_zero_blank() {
        // Same logits as `ctc_collapse_skips_blank_and_repeats`; argmax per
        // frame is [1, 1, 0, 2]. With blank=1 the 0 now survives and the
        // leading 1s drop out.
        let logits: Vec<f32> = vec![
            0.0, 5.0, 1.0, //
            0.0, 4.0, 1.0, //
            9.0, 1.0, 1.0, //
            0.0, 1.0, 7.0, //
        ];
        assert_eq!(ctc_collapse_argmax(&logits, 4, 3, 1), vec![0, 2]);
    }

    #[test]
    fn log_softmax_rows_normalizes_each_row() {
        let mut logits = vec![1.0f32, 2.0, 3.0, 0.0, 0.0, 0.0];
        log_softmax_rows(&mut logits, 2, 3);
        for row in logits.chunks(3) {
            let sum: f64 = row.iter().map(|&x| (x as f64).exp()).sum();
            assert!((sum - 1.0).abs() < 1e-5, "row must sum to 1, got {sum}");
            assert!(row.iter().all(|&x| x <= 0.0), "log-probs must be <= 0");
        }
        // Uniform row: every value is ln(1/3).
        let expected = (1.0f32 / 3.0).ln();
        assert!((logits[3] - expected).abs() < 1e-5);
    }

    #[test]
    fn log_softmax_rows_survives_extremes() {
        // Large magnitudes would overflow exp() without the max subtraction.
        let mut logits = vec![1000.0f32, 999.0, -1000.0];
        log_softmax_rows(&mut logits, 1, 3);
        assert!(logits.iter().all(|x| x.is_finite()), "got {logits:?}");
        assert!(logits[0] > logits[1] && logits[1] > logits[2]);

        // Degenerate shapes and short rows must not panic.
        let mut empty: Vec<f32> = Vec::new();
        log_softmax_rows(&mut empty, 0, 0);
        let mut short = vec![1.0f32, 2.0];
        log_softmax_rows(&mut short, 4, 3);
        assert_eq!(short.len(), 2);
    }

    #[test]
    fn vocab_available_reports_file_presence() {
        // Must not panic; agrees with the same candidate paths the engine uses.
        let available = vocab_available();
        let any_file = super::vocab_candidates().iter().any(|p| p.is_file());
        assert_eq!(available, any_file);
    }
}
