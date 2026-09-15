//! Offline TTS via piper-rs 0.2.0.
//!
//! `piper_rs::Piper::create` takes `&mut self`, so the engine wraps it in a
//! `Mutex` to allow sharing behind `&self` (e.g. in Tauri managed state).
//! Errors are plain `String` (no `anyhow` dependency).

use std::path::{Path, PathBuf};

use crate::audio;

/// Candidate (model, config) path pairs, checked in order.
///
/// First entry assumes the process CWD is `src-tauri/` (dev / `cargo run`);
/// second assumes CWD is the workspace root (Tauri bundler / frontend tooling).
pub const VOICE_CANDIDATES: &[(&str, &str)] = &[
    (
        "models/en_US-libritts_r-medium.onnx",
        "models/en_US-libritts_r-medium.onnx.json",
    ),
    (
        "src-tauri/models/en_US-libritts_r-medium.onnx",
        "src-tauri/models/en_US-libritts_r-medium.onnx.json",
    ),
];

/// Fallback sample rate when the voice config cannot be parsed.
/// Matches common piper voices (e.g. libritts-r medium is 22050 Hz).
const FALLBACK_SAMPLE_RATE: u32 = 22_050;

/// Default voice file stems, tried in each search root.
const DEFAULT_VOICE_STEMS: &[&str] = &["en_US-libritts_r-medium"];

/// Return the first candidate pair where **both** files exist.
///
/// Checks the CWD-relative [`VOICE_CANDIDATES`] first, then the default
/// voice stems inside each startup-registered extra root
/// (`$RESOURCE/models/`, …) so bundled voices resolve in installed apps.
pub fn find_voice() -> Option<(PathBuf, PathBuf)> {
    VOICE_CANDIDATES
        .iter()
        .find_map(|(model, config)| {
            let m = PathBuf::from(model);
            let c = PathBuf::from(config);
            if m.is_file() && c.is_file() {
                Some((m, c))
            } else {
                None
            }
        })
        .or_else(|| {
            crate::paths::extra_model_dirs()
                .into_iter()
                .find_map(|dir| {
                    DEFAULT_VOICE_STEMS.iter().find_map(|stem| {
                        let m = dir.join(format!("{stem}.onnx"));
                        let c = dir.join(format!("{stem}.onnx.json"));
                        if m.is_file() && c.is_file() {
                            Some((m, c))
                        } else {
                            None
                        }
                    })
                })
        })
}

/// Default voice model path: the model half of [`find_voice`].
///
/// `None` when no complete (model + config) pair is installed.
/// Used by `model_status` for the `tts_voice` presence check.
pub fn default_voice_path() -> Option<PathBuf> {
    find_voice().map(|(model, _)| model)
}

/// Parent directories of the voice candidates, in priority order.
///
/// Derived from [`VOICE_CANDIDATES`] so the scan stays in sync with the
/// loader, plus the startup-registered extra roots (`$RESOURCE/models/`).
/// Relative entries resolve against the process CWD (same assumption as
/// the loader).
fn models_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for (_, config) in VOICE_CANDIDATES {
        let parent = Path::new(config)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        if !dirs.contains(&parent) {
            dirs.push(parent);
        }
    }
    for dir in crate::paths::extra_model_dirs() {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// Voice id from a `*.onnx.json` config file name
/// (e.g. `en_US-libritts_r-medium.onnx.json` → `en_US-libritts_r-medium`),
/// or `None` when the name doesn't match.
fn voice_id_from_file_name(name: &str) -> Option<String> {
    name.strip_suffix(".onnx.json")
        .filter(|stem| !stem.is_empty())
        .map(str::to_string)
}

/// Human-friendly label for a voice id.
fn voice_label_for(id: &str) -> String {
    id.replace(['_', '-'], " ")
}

/// Scan `dirs` for installed voices: every `*.onnx.json` config file is one
/// voice, with the config stem as its id. The `.onnx` sidecar is probed per
/// voice — entries whose audio file is missing are still listed (so the
/// picker can show them as unavailable) but labeled `"<label> (missing
/// audio)"` instead of being silently dropped. Output is sorted by id with
/// duplicate ids (same voice visible from two roots) collapsed.
fn available_voices_in(dirs: &[PathBuf]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(id) = voice_id_from_file_name(name) else {
                continue;
            };
            if out.iter().any(|(existing, _)| existing == &id) {
                continue;
            }
            let complete = dir.join(format!("{id}.onnx")).is_file();
            let label = if complete {
                voice_label_for(&id)
            } else {
                format!("{} (missing audio)", voice_label_for(&id))
            };
            out.push((id, label));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// All installed TTS voices as `(id, label)` pairs, sorted by id.
///
/// Scans the same candidate dirs the voice loader uses; no behavior change
/// to synthesis. Empty when no voice configs are installed.
pub fn available_voices() -> Vec<(String, String)> {
    available_voices_in(&models_dirs())
}

fn sample_rate_from_config(config_path: &Path) -> u32 {
    let Ok(text) = std::fs::read_to_string(config_path) else {
        return FALLBACK_SAMPLE_RATE;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return FALLBACK_SAMPLE_RATE;
    };
    v.get("audio")
        .and_then(|a| a.get("sample_rate"))
        .and_then(|s| s.as_u64())
        .and_then(|s| u32::try_from(s).ok())
        .filter(|&s| s > 0)
        .unwrap_or(FALLBACK_SAMPLE_RATE)
}

pub struct TtsEngine {
    inner: std::sync::Mutex<piper_rs::Piper>,
    sample_rate: u32,
}

impl TtsEngine {
    /// Load a voice from `(model, config)` onnx + json pair.
    pub fn load(model: &Path, config: &Path) -> Result<Self, String> {
        let sample_rate = sample_rate_from_config(config);
        let piper =
            piper_rs::Piper::new(model, config).map_err(|e| format!("TTS load failed: {e}"))?;
        Ok(Self {
            inner: std::sync::Mutex::new(piper),
            sample_rate,
        })
    }

    /// Current voice sample rate (from config at load time).
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Synthesize `text` to `(f32 mono samples, sample_rate)`.
    ///
    /// piper-rs 0.2.0 signature:
    /// `create(&mut self, &str, bool, Option<i64>, Option<f32> x3)`
    /// `-> PiperResult<(Vec<f32>, u32)>`.
    pub fn synthesize(&self, text: &str) -> Result<(Vec<f32>, u32), String> {
        if text.trim().is_empty() {
            return Err("TTS: empty text".to_string());
        }
        let mut piper = self
            .inner
            .lock()
            .map_err(|e| format!("TTS mutex poisoned: {e}"))?;
        piper
            .create(text, false, None, None, None, None)
            .map_err(|e| format!("TTS synthesize failed: {e}"))
    }

    /// Synthesize `text` directly to a RIFF/WAVE (PCM 16-bit mono) blob.
    pub fn synthesize_wav(&self, text: &str) -> Result<Vec<u8>, String> {
        let (samples, rate) = self.synthesize(text)?;
        Ok(audio::encode_wav_mono_16bit(&samples, rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_id_from_config_name() {
        assert_eq!(
            voice_id_from_file_name("en_US-libritts_r-medium.onnx.json"),
            Some("en_US-libritts_r-medium".to_string())
        );
        assert_eq!(voice_id_from_file_name("vocab.json"), None);
        assert_eq!(voice_id_from_file_name("model.onnx"), None);
        assert_eq!(voice_id_from_file_name("voice.onnx.json.bak"), None);
        assert_eq!(voice_id_from_file_name(".onnx.json"), None);
        assert_eq!(
            voice_label_for("en_US-libritts_r-medium"),
            "en US libritts r medium"
        );
    }

    #[test]
    fn default_voice_path_agrees_with_find_voice() {
        assert_eq!(default_voice_path(), find_voice().map(|(model, _)| model));
    }

    #[test]
    fn available_voices_in_scans_configs_and_flags_missing_sidecar() {
        let dir = std::env::temp_dir().join(format!(
            "olp-voices-test-{}-{}",
            std::process::id(),
            "sidecar"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Complete pair, config-only voice, and an unrelated file.
        std::fs::write(dir.join("b_voice.onnx.json"), "{}").unwrap();
        std::fs::write(dir.join("b_voice.onnx"), "fake").unwrap();
        std::fs::write(dir.join("a_voice.onnx.json"), "{}").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignore me").unwrap();
        // A directory named like a config must not be listed.
        std::fs::create_dir_all(dir.join("c_voice.onnx.json")).unwrap();

        let voices = available_voices_in(std::slice::from_ref(&dir));
        assert_eq!(
            voices,
            vec![
                ("a_voice".to_string(), "a voice (missing audio)".to_string()),
                ("b_voice".to_string(), "b voice".to_string()),
            ]
        );
        // Duplicate ids across roots collapse; output stays sorted.
        let dupes = available_voices_in(&[dir.clone(), dir.clone()]);
        assert_eq!(dupes, voices);
        // Missing dirs are skipped silently.
        assert!(available_voices_in(&[dir.join("nope")]).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn available_voices_is_sorted_and_deduped() {
        let voices = available_voices();
        let mut ids: Vec<&str> = voices.iter().map(|(id, _)| id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "voices must be sorted by id");
        ids.dedup();
        assert_eq!(ids.len(), voices.len(), "voice ids must be unique");
        for (id, label) in &voices {
            assert!(!id.is_empty() && !label.is_empty());
        }
    }
}
