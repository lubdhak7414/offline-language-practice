//! On-device ASR via wav2vec2 ONNX (ort 2.0.0-rc.12).
//!
//! Greedy CTC decode (argmax over vocab, collapse blank id 0 + repeats).
//! `vocab.json` next to the model is required; silence, bad shapes and
//! missing vocab surface as `Err` with `ASR_*` prefixes (never debug text).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ort::session::builder::GraphOptimizationLevel;
use ort::{session::Session, value::Tensor};

use crate::inference::ordered_providers;

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

/// Greedy CTC collapse over row-major `[frames, vocab]` logits.
///
/// Argmax per frame, skip blank id 0, collapse repeats. Pure (no IO).
/// Short slices terminate early instead of panicking.
pub fn ctc_collapse_argmax(logits: &[f32], frames: usize, vocab: usize) -> Vec<i64> {
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
        if best_id != 0 && Some(best_id) != prev {
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

        // Greedy argmax per frame, then CTC collapse (blank id 0 + repeats).
        let collapsed = ctc_collapse_argmax(data, frames, vocab);

        Self::try_decode_ids(&collapsed)
    }

    /// Decode via `vocab.json` next to the resolved model.
    ///
    /// Returns `Err` with `ASR_NO_VOCAB:` when no vocab file resolves or
    /// parsing fails (never raw token-id dumps).
    fn try_decode_ids(collapsed: &[i64]) -> ort::Result<String> {
        if !vocab_available() {
            return Err(ort::Error::new(
                "ASR_NO_VOCAB: vocab.json not found (no candidate resolves)".to_string(),
            ));
        }
        let vocab_path = vocab_candidates()
            .into_iter()
            .find(|p| p.is_file())
            .ok_or_else(|| {
                ort::Error::new(
                    "ASR_NO_VOCAB: vocab.json not found (no candidate resolves)".to_string(),
                )
            })?;
        let text = std::fs::read_to_string(&vocab_path).map_err(|e| {
            ort::Error::new(format!(
                "ASR_NO_VOCAB: cannot read {}: {e}",
                vocab_path.display()
            ))
        })?;
        let token_to_id: HashMap<String, i64> = serde_json::from_str(&text)
            .map_err(|e| ort::Error::new(format!("ASR_NO_VOCAB: invalid vocab.json: {e}")))?;
        let mut id_to_token: HashMap<i64, String> = HashMap::new();
        for (tok, id) in token_to_id.iter() {
            id_to_token.entry(*id).or_insert_with(|| tok.clone());
        }
        Ok(decode_collapsed_ids(collapsed, &id_to_token))
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
        assert_eq!(ctc_collapse_argmax(&logits, 4, 3), vec![1, 2]);
    }

    #[test]
    fn ctc_collapse_empty_and_truncated() {
        assert!(ctc_collapse_argmax(&[], 0, 3).is_empty());
        assert!(ctc_collapse_argmax(&[], 4, 0).is_empty());
        // Truncated slice terminates instead of panicking.
        let logits = vec![0.0f32, 1.0];
        let out = ctc_collapse_argmax(&logits, 4, 3);
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
        let err = AsrEngine::try_decode_ids(&[1, 2, 3])
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("ASR_NO_VOCAB:"),
            "expected ASR_NO_VOCAB: prefix, got {err:?}"
        );
    }

    #[test]
    fn vocab_available_reports_file_presence() {
        // Must not panic; agrees with the same candidate paths the engine uses.
        let available = vocab_available();
        let any_file = super::vocab_candidates().iter().any(|p| p.is_file());
        assert_eq!(available, any_file);
    }
}
