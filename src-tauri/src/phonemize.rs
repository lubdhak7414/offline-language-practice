//! Target text to espeak-ng IPA phones, for a phone-level acoustic model.
//!
//! The optional phoneme model (`wav2vec2-lv-60-espeak-cv-ft`) was trained on
//! labels produced by espeak-ng's `en-us` voice, so the canonical phones of a
//! target phrase come from the same espeak-ng that piper already links for
//! TTS — no second G2P, no dictionary file.
//!
//! # One lock for all of espeak
//!
//! espeak-ng keeps global, non-reentrant state: the voice set by
//! `espeak_SetVoiceByName` is process-wide, and `espeak_TextToPhonemes`
//! walks an internal cursor. Three TTS engines on three threads segfaulted
//! (see AGENTS.md "Gotchas"). Every call into espeak — [`phonemize`] here and
//! `TtsEngine::synthesize` (inside `piper.create`) — therefore holds
//! [`ESPEAK`]. Initialization is not a second hazard: `espeak_rs` guards
//! `espeak_Initialize` with its own `OnceLock`, and piper-rs and this module
//! share that one `espeak-rs` crate instance, so espeak is initialized once
//! per process whichever caller comes first.
//!
//! # Word groups
//!
//! espeak prints one whitespace-separated group per word, phones joined by
//! the separator, with stress marks inline (`θ_ˈɪ_ŋ_k`). It also merges
//! function words (`in the` -> `ɪ_n_ð_ə`) and expands digits (`101B` -> four
//! groups). So the whole phrase is phonemized first, which keeps
//! connected-speech forms like `the apple` -> `ð ɪ`; if the group count does
//! not match the word count, each word is phonemized on its own and every
//! group it produces belongs to it.
//!
//! # Case matters
//!
//! Targets are compared upper-case (`pronounce::tokenize`), but espeak spells
//! all-caps words as letters: `IT IS US` comes out as `aɪ tiː ɪz juː ɛs`
//! (measured on the vendored 1.52). Words go to espeak lower-cased, except
//! the pronoun `I` and its contractions, which espeak reads differently in
//! lower case (`i am` is stressed, `I am` is reduced).

use std::sync::{Mutex, MutexGuard};

use crate::pronounce::tokenize;

/// Serializes every call into espeak-ng. See the module header.
pub static ESPEAK: Mutex<()> = Mutex::new(());

/// Take [`ESPEAK`]. A panic while it was held cannot leave espeak in a state
/// the next caller can detect or repair, so poisoning is ignored rather than
/// turning one failure into a permanently broken TTS.
pub fn espeak_lock() -> MutexGuard<'static, ()> {
    ESPEAK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The espeak voice the phoneme model's labels were made with
/// (`tokenizer_config.json`: `"phonemizer_lang": "en-us"`).
pub const LANGUAGE: &str = "en-us";

/// Phone separator requested from espeak.
const SEP: char = '_';

/// A target word and its canonical phones, in order.
pub type WordPhones = (String, Vec<String>);

/// Split one espeak phoneme string into word groups of phones. Pure.
///
/// Stress marks go, empty segments go (espeak leaves a trailing separator
/// before a space sometimes), and clause punctuation counts as a group
/// break: `espeak_rs` concatenates the clauses of a sentence with nothing
/// between them. Length marks and multi-character units (`ɑːɹ`, `aɪ`, `əl`)
/// are kept whole, because the model's vocab holds them as single tokens.
pub fn split_groups(phonemes: &str) -> Vec<Vec<String>> {
    phonemes
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | '?' | '!' | ';' | ':'))
        .map(|group| {
            group
                .split(SEP)
                .map(|seg| seg.replace(['ˈ', 'ˌ'], ""))
                .filter(|seg| !seg.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|g| !g.is_empty())
        .collect()
}

/// Pair words with groups when espeak produced exactly one group per word.
/// `None` means it merged or split words and the caller must go word by word.
pub fn assign_groups(words: &[String], groups: Vec<Vec<String>>) -> Option<Vec<WordPhones>> {
    (groups.len() == words.len()).then(|| words.iter().cloned().zip(groups).collect())
}

/// How a tokenized (upper-case) word is written for espeak. Pure.
fn espeak_spelling(word: &str) -> String {
    let lower = word.to_lowercase();
    if lower == "i" || lower.starts_with("i'") {
        let mut s = String::from("I");
        s.push_str(&lower[1..]);
        s
    } else {
        lower
    }
}

/// espeak groups for `text`. Caller holds [`ESPEAK`].
fn groups_locked(text: &str) -> Result<Vec<Vec<String>>, String> {
    let sentences = espeak_rs::text_to_phonemes(text, LANGUAGE, Some(SEP))
        .map_err(|e| format!("phonemize: {e}"))?;
    Ok(split_groups(&sentences.join(" ")))
}

/// Canonical espeak phones for each word of `target`.
///
/// Words are [`tokenize`]d exactly as the scorer tokenizes them, so word `i`
/// here is word `i` of a `PronScore`. Takes [`ESPEAK`] for the whole call,
/// including the word-by-word fallback. Errors (espeak failed to start, a
/// word with no phones, an empty target) mean "no phone scoring", never a
/// score.
// Consumed by the phone-GOP corpus dump today and by the app in Phase 7
// Stage 6A A7 (phone-level scoring), gated on the E2 measurement.
#[allow(dead_code)]
pub fn phonemize(target: &str) -> Result<Vec<WordPhones>, String> {
    let words = tokenize(target);
    if words.is_empty() {
        return Err("phonemize: target has no words".to_string());
    }
    let spelled: Vec<String> = words.iter().map(|w| espeak_spelling(w)).collect();

    let _guard = espeak_lock();
    if let Some(pairs) = assign_groups(&words, groups_locked(&spelled.join(" "))?) {
        return Ok(pairs);
    }
    let mut out = Vec::with_capacity(words.len());
    for (word, text) in words.iter().zip(&spelled) {
        let phones: Vec<String> = groups_locked(text)?.into_iter().flatten().collect();
        if phones.is_empty() {
            return Err(format!("phonemize: espeak gave no phones for {word:?}"));
        }
        out.push((word.clone(), phones));
    }
    Ok(out)
}

/// Every phone espeak-ng 1.52 (`en-us`) produces for the read-aloud built-in
/// prompts and the 2,604 word types of speechocean762, each checked present
/// as a single token in the phoneme model's `vocab.json` (onnx-community
/// `wav2vec2-lv-60-espeak-cv-ft-ONNX` @ c69750f5, sha256 d732ab24…).
///
/// Pins espeak <-> model compatibility without the model on disk: if an
/// espeak or data upgrade starts emitting a phone outside this list, the
/// `builtin_prompts_phonemize_into_the_model_inventory` test fails.
#[cfg(test)]
pub const MODEL_EN_PHONES: &[&str] = &[
    "aɪ", "aɪə", "aɪɚ", "aʊ", "b", "d", "dʒ", "eɪ", "f", "h", "i", "iə", "iː", "j", "k", "l", "m",
    "n", "n\u{329}", "oʊ", "p", "s", "t", "tʃ", "uː", "v", "w", "z", "æ", "ð", "ŋ", "ɐ", "ɑː",
    "ɑːɹ", "ɔ", "ɔɪ", "ɔː", "ɔːɹ", "ə", "əl", "ɚ", "ɛ", "ɛɹ", "ɜː", "ɡ", "ɪ", "ɪɹ", "ɹ", "ɾ", "ʃ",
    "ʊ", "ʊɹ", "ʌ", "ʒ", "ʔ", "θ", "ᵻ",
];

#[cfg(test)]
pub mod testing {
    /// Point espeak at the vendored data (`src-tauri/resources`) before its
    /// first initialization, as `run()` does for an installed app, so tests
    /// fail if the shipped subset stops working. Idempotent, and done under
    /// [`super::ESPEAK`] so no espeak call can race the env write.
    pub fn use_vendored_espeak_data() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _guard = super::espeak_lock();
            std::env::set_var(
                crate::paths::ESPEAK_DATA_ENV,
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources"),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn split_strips_stress_and_empty_segments() {
        // Verbatim espeak-ng 1.52 output for "the apple".
        assert_eq!(
            split_groups("ð_ɪ_ ˈæ_p_əl"),
            vec![v(&["ð", "ɪ"]), v(&["æ", "p", "əl"])]
        );
        assert_eq!(split_groups("θ_ˈɪ_ŋ_k"), vec![v(&["θ", "ɪ", "ŋ", "k"])]);
        assert_eq!(
            split_groups("k_ˈɑːɹ f_ɔːɹ"),
            vec![v(&["k", "ɑːɹ"]), v(&["f", "ɔːɹ"])]
        );
        assert!(split_groups("").is_empty());
    }

    #[test]
    fn split_drops_clause_punctuation() {
        // espeak_rs appends clause punctuation and joins sub-clauses with
        // nothing between them.
        assert_eq!(split_groups("t_ˈɛ_s_t."), vec![v(&["t", "ɛ", "s", "t"])]);
        assert_eq!(
            split_groups("f_ˈaɪ_n,θ_ˈæ_ŋ_k_s."),
            vec![v(&["f", "aɪ", "n"]), v(&["θ", "æ", "ŋ", "k", "s"])]
        );
        assert_eq!(split_groups("h_ˈaʊ? "), vec![v(&["h", "aʊ"])]);
    }

    #[test]
    fn groups_fall_back_when_espeak_merges_words() {
        let words = v(&["IN", "THE", "GARDEN"]);
        let merged = split_groups("ɪ_n_ð_ə ɡ_ˈɑːɹ_d_ə_n");
        assert_eq!(assign_groups(&words, merged), None);
        let apple = split_groups("ð_ɪ_ ˈæ_p_əl");
        assert_eq!(
            assign_groups(&v(&["THE", "APPLE"]), apple),
            Some(vec![
                ("THE".to_string(), v(&["ð", "ɪ"])),
                ("APPLE".to_string(), v(&["æ", "p", "əl"])),
            ])
        );
    }

    #[test]
    fn words_reach_espeak_in_sentence_case() {
        assert_eq!(espeak_spelling("IT"), "it");
        assert_eq!(espeak_spelling("US"), "us");
        assert_eq!(espeak_spelling("DON'T"), "don't");
        assert_eq!(espeak_spelling("I"), "I");
        assert_eq!(espeak_spelling("I'M"), "I'm");
        assert_eq!(espeak_spelling("ICE"), "ice");
    }

    // espeak-backed from here on: real espeak-ng, vendored data, under the
    // same lock as TTS. No model files needed.

    #[test]
    fn phonemize_think() {
        testing::use_vendored_espeak_data();
        assert_eq!(
            phonemize("think").expect("phonemize"),
            vec![("THINK".to_string(), v(&["θ", "ɪ", "ŋ", "k"]))]
        );
    }

    #[test]
    fn all_caps_words_are_read_not_spelled() {
        // Upper case is how targets are tokenized; espeak would spell
        // "IT" as "I T" if it saw it that way.
        testing::use_vendored_espeak_data();
        let got = phonemize("IT IS US").expect("phonemize");
        let words: Vec<&str> = got.iter().map(|(w, _)| w.as_str()).collect();
        assert_eq!(words, ["IT", "IS", "US"]);
        assert_eq!(got[0].1.first().map(String::as_str), Some("ɪ"));
        assert!(got[2].1.len() <= 2, "US spelled out: {:?}", got[2].1);
    }

    #[test]
    fn merged_and_expanded_words_keep_the_word_count() {
        testing::use_vendored_espeak_data();
        for target in [
            "in the garden",
            "Room 101B.",
            "I'm fine, thanks. How are you?",
        ] {
            let got = phonemize(target).expect("phonemize");
            assert_eq!(
                got.iter().map(|(w, _)| w.clone()).collect::<Vec<_>>(),
                tokenize(target),
                "{target:?}"
            );
            assert!(
                got.iter().all(|(_, p)| !p.is_empty()),
                "{target:?}: {got:?}"
            );
        }
    }

    #[test]
    fn builtin_prompts_phonemize_into_the_model_inventory() {
        testing::use_vendored_espeak_data();
        let mut outside = std::collections::BTreeSet::new();
        let mut n = 0;
        for p in crate::prompts_seed::BUILTIN_PROMPTS {
            let Some(target) = p.target_text else {
                continue;
            };
            n += 1;
            for (_, phones) in phonemize(target).expect("phonemize") {
                for ph in phones {
                    if !MODEL_EN_PHONES.contains(&ph.as_str()) {
                        outside.insert(ph);
                    }
                }
            }
        }
        assert!(n >= 60, "only {n} read-aloud prompts");
        assert!(outside.is_empty(), "phones outside the model: {outside:?}");
    }
}
