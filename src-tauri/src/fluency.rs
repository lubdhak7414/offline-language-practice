//! How the speech was delivered, as opposed to what was said.
//!
//! Pure: it is handed PCM, a transcript and (when acoustic scoring ran) the
//! word timings, and returns numbers. No IO, no model, no database.
//!
//! Two things make this more than a word count divided by a duration:
//!
//! * **Rate is reported twice.** `wpm` spans the whole recording;
//!   `articulation_wpm` excludes the pauses. A steady slow talker and a fast
//!   talker who stops to think have the same `wpm` and very different
//!   `articulation_wpm`, and only the second one needs advice about pausing.
//! * **The silence floor adapts.** Pauses are found against
//!   `percentile10(dB) + 6 dB`, not a fixed amplitude gate. A fixed gate is
//!   calibrated for digital silence and mistakes ordinary room noise —
//!   a fan, a street outside — for continuous speech, which erases every
//!   pause in the recording.
//! * **Timings are only used when they belong to this recording.** Word spans
//!   come from forced alignment against the target phrase, which succeeds
//!   whether or not the learner said it. They are accepted only when the
//!   transcript and the target line up one word to one word; otherwise the
//!   energy envelope measures the pauses and `method` reports `"energy"`.
//!
//! # Caveat: text fillers under-count
//!
//! The ASR model is trained on read speech (LibriSpeech), which contains
//! almost no disfluencies, so it rarely emits `UM` or `UH` even when they
//! were clearly said — it tends to swallow them or bend them into a real
//! word. The text filler count is therefore a floor, not a measurement. The
//! acoustic `hesitation_count` compensates: a word that occupies an
//! unusually long span while scoring badly is where the model most likely
//! absorbed a filler.

use serde::Serialize;

use crate::pronounce::{tokenize, WordScore};

/// Analysis window and hop for the energy envelope.
pub const WINDOW_MS: f32 = 20.0;
pub const HOP_MS: f32 = 10.0;

/// Silence shorter than this is normal articulation, not a pause.
pub const PAUSE_MIN_MS: i64 = 400;

/// A word span at least this long, scored below [`HESITATION_SCORE`], is
/// counted as a hesitation the transcript did not spell out.
pub const HESITATION_MS: i64 = 600;
pub const HESITATION_SCORE: u8 = 40;

/// Below this many words, rate and pause counts are noise, so nothing is
/// reported at all rather than reporting a number nobody should read.
pub const MIN_WORDS: usize = 3;

/// Comfortable conversational band, words per minute of speaking time.
pub const RATE_IDEAL_LOW: f32 = 110.0;
pub const RATE_IDEAL_HIGH: f32 = 190.0;
/// Outside the band the rate score falls linearly, reaching 0 at these.
pub const RATE_FLOOR: f32 = 40.0;
pub const RATE_CEILING: f32 = 300.0;

/// Pauses per minute that are still normal phrasing.
pub const PAUSES_PER_MIN_OK: f32 = 6.0;
/// A single pause longer than this reads as losing the thread.
pub const PAUSE_LONG_MS: f32 = 1500.0;
/// Fillers per 100 words that pass without comment.
pub const FILLERS_PER_100_OK: f32 = 2.0;

/// A stretch of silence inside the utterance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Pause {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl Pause {
    pub fn duration_ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }
}

/// Filler words, split by how much the count can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct FillerCounts {
    /// Words and phrases that are fillers wherever they appear.
    pub certain: i64,
    /// `LIKE`, which is a filler in "it was, like, fine" and an ordinary verb
    /// in "I like it". Reported separately so the UI can hedge.
    pub like: i64,
}

/// Delivery metrics for one attempt.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FluencyReport {
    /// Words per minute over the whole recording.
    pub wpm: f32,
    /// Words per minute of actual speaking time (pauses removed).
    pub articulation_wpm: f32,
    pub longest_pause_ms: i64,
    pub pause_count: i64,
    pub pauses: Vec<Pause>,
    pub filler_count: i64,
    pub like_count: i64,
    pub hesitation_count: i64,
    pub speaking_ms: i64,
    /// `"aligned"` when word timings drove pause detection, `"energy"` when
    /// the audio envelope did.
    pub method: String,
    pub score: u8,
}

/// RMS energy per hop, in dBFS.
///
/// A 20 ms window every 10 ms: long enough to average out a single pitch
/// period, short enough to see the gap between two words. Silent windows
/// floor at -140 dB rather than diverging to `-inf`.
pub fn frame_energy_db(pcm: &[f32], sample_rate: u32) -> Vec<f32> {
    if pcm.is_empty() || sample_rate == 0 {
        return Vec::new();
    }
    let win = ((WINDOW_MS / 1000.0) * sample_rate as f32).round().max(1.0) as usize;
    let hop = ((HOP_MS / 1000.0) * sample_rate as f32).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(pcm.len() / hop + 1);
    let mut start = 0usize;
    while start < pcm.len() {
        let end = (start + win).min(pcm.len());
        let frame = &pcm[start..end];
        let mean_sq: f64 = frame
            .iter()
            .map(|x| {
                let v = if x.is_finite() { *x as f64 } else { 0.0 };
                v * v
            })
            .sum::<f64>()
            / frame.len() as f64;
        let rms = mean_sq.sqrt().max(1e-7);
        out.push((20.0 * rms.log10()) as f32);
        start += hop;
    }
    out
}

/// The level below which a frame counts as silence.
///
/// The tenth percentile of the recording's own energy, plus 6 dB of headroom.
/// Anchoring to the recording means a noisy room raises the floor with it
/// instead of drowning every pause.
pub fn silence_floor_db(db: &[f32]) -> f32 {
    if db.is_empty() {
        return f32::NEG_INFINITY;
    }
    let mut sorted: Vec<f32> = db.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return f32::NEG_INFINITY;
    }
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() - 1) as f32 * 0.10).round() as usize;
    sorted[idx.min(sorted.len() - 1)] + 6.0
}

/// Pauses from the energy envelope.
///
/// Only silence *between* speech counts: the quiet before someone starts and
/// after they finish is recording latency, not hesitation.
pub fn detect_pauses_energy(db: &[f32], hop_ms: f32, min_pause_ms: i64) -> Vec<Pause> {
    if db.is_empty() || hop_ms <= 0.0 {
        return Vec::new();
    }
    let floor = silence_floor_db(db);
    let voiced: Vec<bool> = db.iter().map(|&v| v > floor).collect();
    let Some(first) = voiced.iter().position(|&v| v) else {
        return Vec::new();
    };
    let Some(last) = voiced.iter().rposition(|&v| v) else {
        return Vec::new();
    };

    let to_ms = |frame: usize| (frame as f32 * hop_ms).round() as i64;
    let mut out = Vec::new();
    let mut run_start: Option<usize> = None;
    for (i, &is_voiced) in voiced.iter().enumerate().take(last + 1).skip(first) {
        if is_voiced {
            if let Some(s) = run_start.take() {
                let pause = Pause {
                    start_ms: to_ms(s),
                    end_ms: to_ms(i),
                };
                if pause.duration_ms() >= min_pause_ms {
                    out.push(pause);
                }
            }
        } else if run_start.is_none() {
            run_start = Some(i);
        }
    }
    out
}

/// Pauses from forced-alignment word timings: the gaps between words.
///
/// More accurate than the envelope when it is available, because it knows a
/// quiet fricative is speech and a held breath is not.
pub fn detect_pauses_aligned(words: &[WordScore], min_pause_ms: i64) -> Vec<Pause> {
    let mut out = Vec::new();
    for pair in words.windows(2) {
        let pause = Pause {
            start_ms: pair[0].end_ms,
            end_ms: pair[1].start_ms,
        };
        if pause.duration_ms() >= min_pause_ms {
            out.push(pause);
        }
    }
    out
}

/// Count filler words and phrases in a transcript.
///
/// Matching is on whole tokens, so `LIKELY` is not a `LIKE` and `UMBRELLA`
/// is not an `UM`. Multi-word phrases are matched first and consume their
/// tokens, so `YOU KNOW` is one filler rather than two misses.
pub fn count_fillers(text: &str) -> FillerCounts {
    const SINGLE: &[&str] = &["UM", "UH", "ER", "AH", "MM", "HMM", "ERM", "UHM"];
    const PHRASES: &[&[&str]] = &[
        &["YOU", "KNOW"],
        &["I", "MEAN"],
        &["SORT", "OF"],
        &["KIND", "OF"],
    ];

    let tokens = tokenize(text);
    let mut counts = FillerCounts::default();
    let mut i = 0usize;
    'outer: while i < tokens.len() {
        for phrase in PHRASES {
            if tokens.len() - i >= phrase.len()
                && tokens[i..i + phrase.len()]
                    .iter()
                    .zip(phrase.iter())
                    .all(|(a, b)| a == b)
            {
                counts.certain += 1;
                i += phrase.len();
                continue 'outer;
            }
        }
        if SINGLE.contains(&tokens[i].as_str()) {
            counts.certain += 1;
        } else if tokens[i] == "LIKE" {
            counts.like += 1;
        }
        i += 1;
    }
    counts
}

/// Words the model most likely absorbed a filler into: long span, poor score.
pub fn count_hesitations(words: &[WordScore]) -> i64 {
    words
        .iter()
        .filter(|w| (w.end_ms - w.start_ms) >= HESITATION_MS && w.score < HESITATION_SCORE)
        .count() as i64
}

/// Whether forced-alignment timings for `target` can be trusted to describe
/// `transcript`.
///
/// A substitution is harmless: the learner said *something* in that slot, so
/// the span still bounds real audio. An insertion or a deletion is not — a
/// deleted target word has no audio behind its span, and an inserted spoken
/// word has no span at all. Either breaks the one-to-one correspondence that
/// makes a word timing mean anything, even when the word counts still agree.
pub fn timings_describe(transcript: &str, target: &str) -> bool {
    let a = crate::pronounce::align_words(transcript, target);
    a.inserted == 0 && a.deleted == 0
}

/// 0..=100 for speaking rate: flat inside the comfortable band, falling
/// linearly outside it.
pub fn rate_score(articulation_wpm: f32) -> u8 {
    if !articulation_wpm.is_finite() || articulation_wpm <= 0.0 {
        return 0;
    }
    let frac = if articulation_wpm < RATE_IDEAL_LOW {
        (articulation_wpm - RATE_FLOOR) / (RATE_IDEAL_LOW - RATE_FLOOR)
    } else if articulation_wpm > RATE_IDEAL_HIGH {
        (RATE_CEILING - articulation_wpm) / (RATE_CEILING - RATE_IDEAL_HIGH)
    } else {
        1.0
    };
    (frac.clamp(0.0, 1.0) * 100.0).round() as u8
}

/// 0..=100 for pausing: how far past normal phrasing the pauses go, and
/// whether any single one ran long.
pub fn pause_score(pauses: &[Pause], total_ms: i64) -> u8 {
    if total_ms <= 0 {
        return 0;
    }
    let minutes = (total_ms as f32 / 60_000.0).max(1.0 / 60.0);
    let per_min = pauses.len() as f32 / minutes;
    let frequency_penalty = ((per_min - PAUSES_PER_MIN_OK).max(0.0) / PAUSES_PER_MIN_OK) * 50.0;
    let longest = pauses.iter().map(|p| p.duration_ms()).max().unwrap_or(0) as f32;
    let length_penalty = ((longest - PAUSE_LONG_MS).max(0.0) / PAUSE_LONG_MS) * 50.0;
    (100.0 - frequency_penalty - length_penalty).clamp(0.0, 100.0) as u8
}

/// 0..=100 for fillers, relative to how much was said.
///
/// `LIKE` counts at half weight: it is a filler often enough to matter and an
/// ordinary word often enough that full weight would punish correct English.
pub fn filler_score(fillers: FillerCounts, hesitations: i64, word_count: usize) -> u8 {
    if word_count == 0 {
        return 0;
    }
    let weighted = fillers.certain as f32 + fillers.like as f32 * 0.5 + hesitations as f32;
    let per_100 = weighted * 100.0 / word_count as f32;
    let penalty = (per_100 - FILLERS_PER_100_OK).max(0.0) * 10.0;
    (100.0 - penalty).clamp(0.0, 100.0) as u8
}

/// Delivery metrics for one attempt, or `None` when there is not enough
/// speech to measure honestly.
///
/// `words` are the forced-alignment word scores when acoustic scoring ran;
/// without them — or when they do not describe this transcript — pauses come
/// from the energy envelope instead. Callers should additionally gate on
/// [`timings_describe`]: a length match alone cannot see a dropped word paid
/// for by an added one.
pub fn analyze(
    pcm: &[f32],
    sample_rate: u32,
    transcript: &str,
    words: Option<&[WordScore]>,
    duration_ms: i64,
) -> Option<FluencyReport> {
    let word_count = tokenize(transcript).len();
    if word_count < MIN_WORDS || duration_ms <= 0 {
        return None;
    }

    // The word timings describe the *target* phrase, not the recording: forced
    // alignment spells whatever it is asked to spell, whether or not the audio
    // contains it. Timing a transcript against another utterance's spans
    // reports pauses between words nobody said, and divides a real word count
    // by a duration that was never spoken. One span per spoken word is the
    // cheapest proof the two describe the same thing; when they disagree the
    // energy envelope is the only self-consistent source left.
    let words = words.filter(|w| w.len() == word_count);

    let (pauses, method, span_ms) = match words {
        Some(w) if w.len() >= 2 => {
            let first = w.first().map(|x| x.start_ms).unwrap_or(0);
            let last = w.last().map(|x| x.end_ms).unwrap_or(duration_ms);
            (
                detect_pauses_aligned(w, PAUSE_MIN_MS),
                "aligned",
                (last - first).max(0),
            )
        }
        _ => {
            let db = frame_energy_db(pcm, sample_rate);
            let pauses = detect_pauses_energy(&db, HOP_MS, PAUSE_MIN_MS);
            (pauses, "energy", duration_ms)
        }
    };

    let paused_ms: i64 = pauses.iter().map(|p| p.duration_ms()).sum();
    let speaking_ms = (span_ms - paused_ms).max(0);
    let wpm = word_count as f32 * 60_000.0 / duration_ms as f32;
    let articulation_wpm = if speaking_ms > 0 {
        word_count as f32 * 60_000.0 / speaking_ms as f32
    } else {
        wpm
    };

    let fillers = count_fillers(transcript);
    let hesitations = words.map(count_hesitations).unwrap_or(0);

    // Rate carries the most weight because it is the most reliably measured;
    // fillers the least, because the model under-reports them (see header).
    let score = (rate_score(articulation_wpm) as f32 * 0.5
        + pause_score(&pauses, duration_ms) as f32 * 0.3
        + filler_score(fillers, hesitations, word_count) as f32 * 0.2)
        .round()
        .clamp(0.0, 100.0) as u8;

    Some(FluencyReport {
        wpm,
        articulation_wpm,
        longest_pause_ms: pauses.iter().map(|p| p.duration_ms()).max().unwrap_or(0),
        pause_count: pauses.len() as i64,
        pauses,
        filler_count: fillers.certain,
        like_count: fillers.like,
        hesitation_count: hesitations,
        speaking_ms,
        method: method.to_string(),
        score,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build PCM from `(duration_ms, amplitude)` segments of a 440 Hz tone.
    /// A zero amplitude is digital silence.
    fn tone(sample_rate: u32, segments: &[(f32, f32)]) -> Vec<f32> {
        let mut out = Vec::new();
        let mut phase = 0.0f32;
        let step = 2.0 * std::f32::consts::PI * 440.0 / sample_rate as f32;
        for &(ms, amp) in segments {
            let n = ((ms / 1000.0) * sample_rate as f32).round() as usize;
            for _ in 0..n {
                out.push(phase.sin() * amp);
                phase += step;
            }
        }
        out
    }

    fn word(word: &str, start_ms: i64, end_ms: i64, score: u8) -> WordScore {
        WordScore {
            word: word.to_string(),
            start_ms,
            end_ms,
            gop: 0.0,
            score,
            verdict: "good".to_string(),
        }
    }

    #[test]
    fn energy_pauses_land_on_the_silent_gap() {
        let pcm = tone(16_000, &[(1000.0, 0.5), (600.0, 0.0), (1000.0, 0.5)]);
        let db = frame_energy_db(&pcm, 16_000);
        let pauses = detect_pauses_energy(&db, HOP_MS, PAUSE_MIN_MS);
        assert_eq!(pauses.len(), 1, "expected one gap, got {pauses:?}");
        let p = pauses[0];
        assert!((p.start_ms - 1000).abs() <= 30, "start {p:?}");
        assert!((p.end_ms - 1600).abs() <= 30, "end {p:?}");
    }

    #[test]
    fn the_silence_floor_adapts_to_room_noise() {
        // The "silence" here is a steady -40 dBFS hum, well above the fixed
        // 1e-4 gate the old silence check used. An absolute threshold would
        // call this continuous speech and find no pause at all.
        let pcm = tone(16_000, &[(1000.0, 0.5), (600.0, 0.01), (1000.0, 0.5)]);
        assert!(
            pcm.iter().fold(0.0f32, |a, &b| a.max(b.abs())) > 1e-4,
            "the gap must be audibly noisy for this test to mean anything"
        );
        let db = frame_energy_db(&pcm, 16_000);
        let pauses = detect_pauses_energy(&db, HOP_MS, PAUSE_MIN_MS);
        assert_eq!(pauses.len(), 1, "expected one gap, got {pauses:?}");
        assert!((pauses[0].start_ms - 1000).abs() <= 40);
    }

    #[test]
    fn continuous_speech_has_no_pauses() {
        let pcm = tone(16_000, &[(2000.0, 0.4)]);
        let db = frame_energy_db(&pcm, 16_000);
        assert!(detect_pauses_energy(&db, HOP_MS, PAUSE_MIN_MS).is_empty());
    }

    #[test]
    fn silence_at_the_edges_is_not_a_pause() {
        let pcm = tone(16_000, &[(800.0, 0.0), (1000.0, 0.5), (800.0, 0.0)]);
        let db = frame_energy_db(&pcm, 16_000);
        assert!(
            detect_pauses_energy(&db, HOP_MS, PAUSE_MIN_MS).is_empty(),
            "lead-in and tail silence are recording latency, not hesitation"
        );
    }

    #[test]
    fn a_short_gap_is_articulation_not_a_pause() {
        let pcm = tone(16_000, &[(600.0, 0.5), (150.0, 0.0), (600.0, 0.5)]);
        let db = frame_energy_db(&pcm, 16_000);
        assert!(detect_pauses_energy(&db, HOP_MS, PAUSE_MIN_MS).is_empty());
    }

    #[test]
    fn energy_of_empty_or_invalid_audio_is_empty() {
        assert!(frame_energy_db(&[], 16_000).is_empty());
        assert!(frame_energy_db(&[0.1, 0.2], 0).is_empty());
        assert!(detect_pauses_energy(&[], HOP_MS, PAUSE_MIN_MS).is_empty());
    }

    #[test]
    fn aligned_pauses_are_the_gaps_between_words() {
        let words = [
            word("I", 0, 200, 90),
            word("WAS", 200, 400, 90),
            word("THINKING", 1200, 1600, 90),
        ];
        let pauses = detect_pauses_aligned(&words, PAUSE_MIN_MS);
        assert_eq!(pauses.len(), 1);
        assert_eq!(
            pauses[0],
            Pause {
                start_ms: 400,
                end_ms: 1200
            }
        );
    }

    #[test]
    fn fillers_match_whole_words_only() {
        assert_eq!(count_fillers("UM I THINK SO").certain, 1);
        assert_eq!(
            count_fillers("THAT IS LIKELY AN UMBRELLA").certain,
            0,
            "LIKELY is not LIKE and UMBRELLA is not UM"
        );
        assert_eq!(count_fillers("THAT IS LIKELY AN UMBRELLA").like, 0);
    }

    #[test]
    fn like_is_counted_apart_from_the_certain_fillers() {
        let c = count_fillers("IT WAS LIKE REALLY GOOD AND I LIKE IT");
        assert_eq!(c.certain, 0);
        assert_eq!(c.like, 2, "both uses are counted; the UI hedges, not us");
    }

    #[test]
    fn filler_phrases_count_once_not_twice() {
        assert_eq!(count_fillers("YOU KNOW IT IS FINE").certain, 1);
        assert_eq!(count_fillers("I MEAN SORT OF KIND OF").certain, 3);
        assert_eq!(
            count_fillers("DO YOU KNOW THE WAY").certain,
            1,
            "the phrase matcher does not care about grammar, only tokens"
        );
    }

    #[test]
    fn hesitations_are_long_words_the_model_scored_badly() {
        let words = [
            word("I", 0, 200, 95),
            word("SUPPOSE", 300, 1100, 20),
            word("SO", 1100, 1300, 95),
        ];
        assert_eq!(count_hesitations(&words), 1);
        let clean = [word("SUPPOSE", 300, 1100, 95)];
        assert_eq!(
            count_hesitations(&clean),
            0,
            "long but clear is not hesitant"
        );
    }

    #[test]
    fn rate_score_is_flat_inside_the_band_and_falls_outside() {
        assert_eq!(rate_score(150.0), 100);
        assert_eq!(rate_score(RATE_IDEAL_LOW), 100);
        assert_eq!(rate_score(RATE_IDEAL_HIGH), 100);
        assert_eq!(rate_score(RATE_FLOOR), 0);
        assert_eq!(rate_score(RATE_CEILING), 0);
        assert_eq!(rate_score(0.0), 0);
        assert!(rate_score(80.0) < 100 && rate_score(80.0) > 0);
        assert!(rate_score(240.0) < 100 && rate_score(240.0) > 0);
    }

    #[test]
    fn pause_score_penalises_frequency_and_length() {
        assert_eq!(pause_score(&[], 60_000), 100);
        let one_long = [Pause {
            start_ms: 0,
            end_ms: 4500,
        }];
        assert!(pause_score(&one_long, 60_000) < 100);
        let many: Vec<Pause> = (0..30)
            .map(|i| Pause {
                start_ms: i * 1000,
                end_ms: i * 1000 + 500,
            })
            .collect();
        assert_eq!(pause_score(&many, 60_000), 0, "clamped, never negative");
        assert_eq!(pause_score(&[], 0), 0);
    }

    #[test]
    fn filler_score_scales_with_how_much_was_said() {
        assert_eq!(filler_score(FillerCounts::default(), 0, 50), 100);
        let two = FillerCounts {
            certain: 2,
            like: 0,
        };
        assert_eq!(filler_score(two, 0, 100), 100, "two per hundred passes");
        assert!(filler_score(two, 0, 20) < 100, "two in twenty does not");
        assert_eq!(filler_score(two, 0, 0), 0);
    }

    #[test]
    fn too_little_speech_is_not_scored_at_all() {
        let pcm = tone(16_000, &[(500.0, 0.5)]);
        assert!(analyze(&pcm, 16_000, "HELLO", None, 500).is_none());
        assert!(analyze(&pcm, 16_000, "", None, 500).is_none());
        assert!(
            analyze(&pcm, 16_000, "ONE TWO THREE", None, 0).is_none(),
            "a zero duration would divide by zero, not produce a rate"
        );
    }

    #[test]
    fn articulation_rate_exceeds_overall_rate_when_there_are_pauses() {
        let pcm = tone(16_000, &[(1000.0, 0.5), (800.0, 0.0), (1000.0, 0.5)]);
        let report = analyze(&pcm, 16_000, "ONE TWO THREE FOUR", None, 2800)
            .expect("four words over 2.8 s is measurable");
        assert_eq!(report.method, "energy");
        assert_eq!(report.pause_count, 1);
        assert!(report.longest_pause_ms >= 700);
        assert!(
            report.articulation_wpm > report.wpm,
            "{} should exceed {}",
            report.articulation_wpm,
            report.wpm
        );
        assert!(report.speaking_ms < 2800);
    }

    #[test]
    fn word_timings_take_over_from_the_envelope_when_present() {
        let pcm = tone(16_000, &[(2000.0, 0.5)]);
        let words = [
            word("ONE", 0, 300, 90),
            word("TWO", 300, 600, 90),
            word("THREE", 1500, 1900, 90),
        ];
        let report = analyze(&pcm, 16_000, "ONE TWO THREE", Some(&words), 2000)
            .expect("three words is measurable");
        assert_eq!(report.method, "aligned");
        assert_eq!(
            report.pause_count, 1,
            "the envelope sees no gap here; the timings do"
        );
        assert_eq!(report.longest_pause_ms, 900);
    }

    #[test]
    fn timings_for_a_different_utterance_are_ignored() {
        // Nine target words aligned across the clip; the learner said four.
        let pcm = tone(16_000, &[(1200.0, 0.5), (600.0, 0.0), (1000.0, 0.5)]);
        let words: Vec<WordScore> = (0..9)
            .map(|i| word("W", i * 300, i * 300 + 200, 20))
            .collect();
        let report = analyze(&pcm, 16_000, "ONE TWO THREE FOUR", Some(&words), 2800)
            .expect("four words is measurable");
        assert_eq!(
            report.method, "energy",
            "nine spans cannot time four spoken words"
        );
        assert_eq!(report.hesitation_count, 0, "no trustworthy spans to count");
    }

    #[test]
    fn a_substitution_keeps_the_timings_but_a_deletion_does_not() {
        assert!(
            timings_describe("THE HAT SAT", "THE CAT SAT"),
            "sub keeps the slot"
        );
        assert!(
            !timings_describe("THE SAT", "THE CAT SAT"),
            "deleted word has no audio"
        );
        assert!(
            !timings_describe("THE BIG CAT SAT", "THE CAT SAT"),
            "inserted word has no span"
        );
        assert!(
            timings_describe("THE CAT SAT", "the cat sat."),
            "case and punctuation only"
        );
    }

    #[test]
    fn a_one_word_shift_is_rejected_although_the_counts_match() {
        // Dropped the first word, added one at the end: five tokens against
        // five, so `analyze`'s length check alone would accept it while every
        // span is off by one word. This is the case that needs the caller gate.
        let said = "TELL ME ABOUT YOURSELF NOW";
        let target = "PLEASE TELL ME ABOUT YOURSELF";
        assert_eq!(tokenize(said).len(), tokenize(target).len());
        assert!(!timings_describe(said, target));
        // Pure substitutions keep every slot, so they are not rejected.
        assert!(timings_describe("THE CAT ON MAT", "THE BIG CAT SAT"));
    }

    #[test]
    fn a_steady_well_paced_answer_scores_high() {
        let words: Vec<WordScore> = (0..20)
            .map(|i| word("WORD", i * 350, i * 350 + 300, 90))
            .collect();
        let text = ["WORD"; 20].join(" ");
        let report = analyze(&[], 16_000, &text, Some(&words), 7000).expect("measurable");
        assert_eq!(report.pause_count, 0);
        assert!(
            report.score >= 80,
            "expected a high score, got {}",
            report.score
        );
    }

    #[test]
    fn a_hesitant_answer_scores_lower_than_a_fluent_one() {
        let fluent: Vec<WordScore> = (0..12)
            .map(|i| word("WORD", i * 350, i * 350 + 300, 90))
            .collect();
        let text = ["WORD"; 12].join(" ");
        let fluent_report = analyze(&[], 16_000, &text, Some(&fluent), 4200).expect("measurable");

        // Same words, but each one trails a long gap and half of them scored
        // badly over a long span, which is what an absorbed filler looks like.
        let hesitant: Vec<WordScore> = (0..12)
            .map(|i| {
                let score = if i % 2 == 0 { 20 } else { 90 };
                word("WORD", i * 1200, i * 1200 + 700, score)
            })
            .collect();
        let hesitant_report =
            analyze(&[], 16_000, &text, Some(&hesitant), 14_400).expect("measurable");

        assert!(hesitant_report.pause_count > 0);
        assert!(hesitant_report.hesitation_count > 0);
        assert!(
            hesitant_report.score < fluent_report.score,
            "hesitant {} should score below fluent {}",
            hesitant_report.score,
            fluent_report.score
        );
    }
}
