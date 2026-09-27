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
//! acoustic `hesitation_count` compensates partly: a word that occupies an
//! unusually long span while flagged as unclear is where the model most
//! likely absorbed a filler. It needs word timings, which only exist for
//! read-aloud prompts whose transcript matches the target; open-ended
//! answers get no hesitation count at all.
//!
//! [`detect_held_sounds`] needs no target: it finds filled pauses and
//! stretched words from pitch, level and the CTC path alone. It is not
//! wired into [`analyze`] until it passes its measurement against AMI's
//! hand-transcribed "um"/"uh".

use serde::Serialize;

use crate::asr::CtcFrames;
use crate::pronounce::{tokenize, WordScore};

/// Analysis window and hop for the energy envelope.
pub const WINDOW_MS: f32 = 20.0;
pub const HOP_MS: f32 = 10.0;

/// Silence shorter than this is normal articulation, not a pause.
pub const PAUSE_MIN_MS: i64 = 400;

/// A word span at least this long whose score is below
/// [`crate::pronounce::FLAG_BELOW`] is counted as a hesitation the
/// transcript did not spell out. Tied to the flag cutoff rather than a
/// number of its own: scores are percentiles (`GOP_PERCENTILE`), and every
/// GOP below 0 maps to 34 or less, so any fixed cutoff above that counts
/// ordinary correct words.
pub const HESITATION_MS: i64 = 600;

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
        .filter(|w| {
            (w.end_ms - w.start_ms) >= HESITATION_MS && w.score < crate::pronounce::FLAG_BELOW
        })
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

// ---------------------------------------------------------------------------
// Held sounds: filled pauses and lengthened words, from the audio itself.
//
// After Goto, Itou & Hayamizu (Eurospeech 1999): a speaker who says "uh" or
// stretches "theee" holds the articulators still, so pitch barely moves and
// the level barely changes. On top of those two cues sits the evidence the
// CTC model already produces: voiced sound it did not spell. None of this is
// wired into the app (Phase 7 Stage 5 step 5): on AMI it failed its gate on
// recall (see `corpus`). The classifier thresholds are the ones frozen on
// the AMI dev split; the tracker and segmentation constants were not swept.
// ---------------------------------------------------------------------------

/// A hop must be this far above [`silence_floor_db`] before the pitch tracker
/// looks at it, which keeps mains hum and fans at the floor out of it.
const VOICED_MARGIN_DB: f32 = 10.0;
/// Pitch search range. Covers adult and child voices, not singing.
const F0_MIN_HZ: f32 = 70.0;
const F0_MAX_HZ: f32 = 400.0;
/// Autocorrelation window: at least two periods of the lowest pitch.
const PITCH_WINDOW_MS: f32 = 40.0;
/// Best normalised cross-correlation at or above this is voiced.
const VOICING_NCCF: f32 = 0.6;
/// Octave guard: the first correlation peak within this fraction of the best
/// wins, so a period that also correlates at twice its length is not halved.
const OCTAVE_GUARD: f32 = 0.9;
/// Unvoiced hops a held sound may contain without ending (20 ms).
const HELD_GAP_HOPS: usize = 2;
/// How far pitch may wander (max - min, semitones) inside one candidate.
/// Wider than [`HELD_SPREAD_ST`] so that threshold is a filter, not a cut.
const SEGMENT_SPREAD_ST: f32 = 4.0;
/// Candidates shorter than this are not even logged.
const CANDIDATE_MIN_MS: i64 = 100;
/// Pitch stability: p90 - p10 in semitones. On AMI dev, 2.0 found 21%
/// of filled pauses and 2.5 found 28%, for about a point of precision.
const HELD_SPREAD_ST: f32 = 2.5;
/// Level stability: words have consonant dips; a held "uh" does not. On AMI
/// this barely separates anything (recall moves ~1% from 4 to 10 dB), so it
/// is set loose, at 6 dB, and pitch and CTC do the work.
const HELD_ENERGY_SD_DB: f32 = 6.0;
/// Filled pauses run about 200-600 ms, "um" longer. Past 1.5 s it is more
/// likely a held tone than a hesitation.
const HELD_MIN_MS: i64 = 200;
const HELD_MAX_MS: i64 = 1500;
/// A stretched word has to be held noticeably long to read as hesitation.
const LENGTHENING_MIN_MS: i64 = 300;
/// Letters the model spells a filled pause with when it does not swallow it
/// ("A", "UM", "AH", "ER", "HM").
const FILLER_LETTERS: &[u8] = b"AUMHER";
/// A frame is blank-dominant when P(blank) > 0.5.
const BLANK_DOMINANT_LOGP: f32 = -std::f32::consts::LN_2;
/// Share of blank-dominant frames that makes a candidate "not spelled". The
/// plan also let through anything with at most two filler letters whatever
/// the blank share; on AMI dev that only added false positives.
const BLANK_FRAC_MIN: f32 = 0.7;
/// A blank run this long ends a CTC word even without a `|`.
const WORD_GAP_MS: f32 = 500.0;
/// A CTC word counts as under a candidate when it covers this share of it.
const WORD_OVERLAP_FRAC: f32 = 0.3;

/// What a held sound most likely was.
#[allow(dead_code)] // Phase 7 Stage 5 step 5 wires detect_held_sounds in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HeldKind {
    /// "uh", "um": voiced, steady, and not spelled as a real word.
    FilledPause,
    /// A real word held unusually long ("theee", "aaand").
    Lengthening,
}

/// One held sound, in milliseconds from the start of the recording.
#[allow(dead_code)] // Phase 7 Stage 5 step 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HeldSound {
    pub start_ms: i64,
    pub end_ms: i64,
    pub kind: HeldKind,
}

/// A word on the greedy CTC path, with the span its letters occupy.
#[allow(dead_code)] // Phase 7 Stage 5 step 5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtcWord {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// What the CTC path says lies under a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnderWord {
    /// No word covers it: the model spelled nothing there.
    None,
    /// Only short words made of filler letters ("A", "AH", "HM"): a filled
    /// pause the model bent into something word-shaped.
    FillerShaped,
    /// A filler the transcript already spelled, so `count_fillers` has it.
    SpelledFiller,
    /// A real word.
    Real,
}

/// A steady voiced stretch, before any threshold but the segmentation one.
///
/// Carries every feature the classifier looks at, so the corpus harness can
/// log candidates once and sweep thresholds outside the build.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HeldCandidate {
    pub start_ms: i64,
    pub end_ms: i64,
    /// p90 - p10 of F0 over the voiced hops, semitones.
    pub f0_spread_st: f32,
    /// Standard deviation of the hop level over the voiced hops, dB.
    pub energy_sd_db: f32,
    /// Share of CTC frames under it where P(blank) > 0.5.
    pub blank_frac: f32,
    /// Letters the greedy path emits under it, collapsed.
    pub letters: String,
    pub under: UnderWord,
    /// The words counted in `under`, space-separated. Only the corpus
    /// harness reads it; the classifier needs `under` alone.
    #[cfg_attr(not(test), allow(dead_code))]
    pub words: String,
}

/// Pitch per [`HOP_MS`] hop, `None` where the hop is unvoiced or skipped.
///
/// Normalised cross-correlation over a [`PITCH_WINDOW_MS`] window at each
/// hop, lags `sr/400 ..= sr/70`. `active` restricts the work to hops that
/// passed an energy gate (`None` = every hop), which bounds the cost by
/// speaking time rather than recording length.
#[allow(dead_code)] // Phase 7 Stage 5 step 5.
pub fn f0_track(pcm: &[f32], sample_rate: u32, active: Option<&[bool]>) -> Vec<Option<f32>> {
    if pcm.is_empty() || sample_rate == 0 {
        return Vec::new();
    }
    let sr = sample_rate as f32;
    let hop = ((HOP_MS / 1000.0) * sr).round().max(1.0) as usize;
    let win = ((PITCH_WINDOW_MS / 1000.0) * sr).round().max(2.0) as usize;
    let min_lag = (sr / F0_MAX_HZ).floor().max(1.0) as usize;
    let max_lag = (sr / F0_MIN_HZ).ceil() as usize;
    let x: Vec<f32> = pcm
        .iter()
        .map(|v| if v.is_finite() { *v } else { 0.0 })
        .collect();
    // Prefix sums of squares make each lag's energy term O(1).
    let mut sq = Vec::with_capacity(x.len() + 1);
    sq.push(0.0f64);
    for v in &x {
        sq.push(sq[sq.len() - 1] + (*v as f64) * (*v as f64));
    }
    let energy = |a: usize, n: usize| sq[a + n] - sq[a];

    let hops = x.len().div_ceil(hop);
    let mut out = vec![None; hops];
    let mut nccf = vec![0.0f32; max_lag + 2];
    for (h, slot) in out.iter_mut().enumerate() {
        if !active.is_none_or(|a| a.get(h).copied().unwrap_or(false)) {
            continue;
        }
        let start = h * hop;
        let avail = x.len() - start;
        // Half a window is the least that still sees two low periods.
        if avail < max_lag + 1 + win / 2 {
            continue;
        }
        let n = win.min(avail - max_lag - 1);
        let e0 = energy(start, n);
        if e0 <= 1e-12 {
            continue;
        }
        let a = &x[start..start + n];
        let mut best = 0.0f32;
        for lag in min_lag..=max_lag + 1 {
            let e1 = energy(start + lag, n);
            let num = dot(a, &x[start + lag..start + lag + n]);
            nccf[lag] = if e1 > 1e-12 {
                (num / (e0 * e1).sqrt()) as f32
            } else {
                0.0
            };
            if lag <= max_lag {
                best = best.max(nccf[lag]);
            }
        }
        if best < VOICING_NCCF {
            continue;
        }
        // First interior peak near the best; an edge maximum means the true
        // period lies outside the search range.
        let Some(k) = (min_lag + 1..=max_lag).find(|&k| {
            nccf[k] >= OCTAVE_GUARD * best && nccf[k] >= nccf[k - 1] && nccf[k] >= nccf[k + 1]
        }) else {
            continue;
        };
        // Parabolic interpolation around the peak: an integer lag alone is
        // ~1% off at 300 Hz.
        let (l, c, r) = (nccf[k - 1], nccf[k], nccf[k + 1]);
        let denom = l - 2.0 * c + r;
        let delta = if denom < 0.0 {
            (0.5 * (l - r) / denom).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        *slot = Some(sr / (k as f32 + delta));
    }
    out
}

/// Dot product with eight partial sums, which lets the compiler vectorise
/// what is otherwise a strictly ordered float reduction.
fn dot(a: &[f32], b: &[f32]) -> f64 {
    let mut acc = [0.0f32; 8];
    let (ca, ra) = a.as_chunks::<8>();
    let (cb, rb) = b.as_chunks::<8>();
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    let tail: f32 = ra.iter().zip(rb).map(|(x, y)| x * y).sum();
    acc.iter().map(|&v| v as f64).sum::<f64>() + tail as f64
}

/// Words on the greedy CTC path.
///
/// A word ends at a `|` or at a blank run of [`WORD_GAP_MS`]; repeated
/// labels on adjacent frames collapse, as in decoding. The span runs from
/// the first letter frame to the end of the last one, so the blanks a held
/// vowel leaves *inside* a word still fall within it.
#[allow(dead_code)] // Phase 7 Stage 5 step 5.
pub fn ctc_word_spans(ctc: &CtcFrames) -> Vec<CtcWord> {
    let stride = ctc.stride_ms;
    if stride <= 0.0 {
        return Vec::new();
    }
    let gap_frames = (WORD_GAP_MS / stride).round().max(1.0) as usize;
    let to_ms = |f: usize| (f as f32 * stride).round() as i64;
    let mut out = Vec::new();
    // (text, first letter frame, last letter frame)
    let mut cur: Option<(String, usize, usize)> = None;
    let mut blanks = 0usize;
    let mut prev = 0u8;
    let mut flush = |cur: &mut Option<(String, usize, usize)>| {
        if let Some((text, first, last)) = cur.take() {
            out.push(CtcWord {
                text,
                start_ms: to_ms(first),
                end_ms: to_ms(last + 1),
            });
        }
    };
    for (t, &label) in ctc.label.iter().enumerate() {
        match label {
            0 => {
                blanks += 1;
                if blanks >= gap_frames {
                    flush(&mut cur);
                }
            }
            b' ' => flush(&mut cur),
            _ => {
                blanks = 0;
                let word = cur.get_or_insert_with(|| (String::new(), t, t));
                if label != prev && label != b'?' {
                    word.0.push(label as char);
                }
                word.2 = t;
            }
        }
        if label != 0 {
            blanks = 0;
        }
        prev = label;
    }
    flush(&mut cur);
    out
}

fn semitones(hz: f32) -> f32 {
    12.0 * hz.log2()
}

/// `q`-quantile of `v` by nearest rank; `v` must be non-empty.
fn quantile(v: &[f32], q: f32) -> f32 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s[((s.len() - 1) as f32 * q).round() as usize]
}

/// Split the voiced hops into stretches whose pitch stays inside
/// [`SEGMENT_SPREAD_ST`], tolerating [`HELD_GAP_HOPS`] unvoiced hops.
///
/// Greedy, but when a hop breaks the range the new stretch is grown
/// *backwards* from it as far as the range allows and the old one is cut
/// where the new one begins, so a held "uh" straight after a word starts
/// where the pitch settled, not where the word happened to end.
fn stable_runs(f0: &[Option<f32>]) -> Vec<(usize, usize)> {
    let st: Vec<Option<f32>> = f0.iter().map(|f| f.map(semitones)).collect();
    let mut out = Vec::new();
    // (first hop, last voiced hop)
    let mut cur: Option<(usize, usize)> = None;
    for (i, s) in st.iter().enumerate() {
        let Some(s) = *s else {
            if let Some((a, l)) = cur {
                if i - l > HELD_GAP_HOPS {
                    out.push((a, l));
                    cur = None;
                }
            }
            continue;
        };
        let Some((a, _)) = cur else {
            cur = Some((i, i));
            continue;
        };
        let (lo, hi) = st[a..i]
            .iter()
            .flatten()
            .fold((s, s), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        if hi - lo <= SEGMENT_SPREAD_ST {
            cur = Some((a, i));
            continue;
        }
        // Grow the new stretch backwards from i.
        let (mut b, mut lo, mut hi) = (i, s, s);
        let mut j = i;
        while j > a {
            j -= 1;
            if let Some(v) = st[j] {
                if hi.max(v) - lo.min(v) > SEGMENT_SPREAD_ST {
                    break;
                }
                lo = lo.min(v);
                hi = hi.max(v);
                b = j;
            }
        }
        if let Some(l) = (a..b).rev().find(|&k| st[k].is_some()) {
            out.push((a, l));
        }
        cur = Some((b, i));
    }
    if let Some(run) = cur {
        out.push(run);
    }
    out
}

/// Every steady voiced stretch of at least [`CANDIDATE_MIN_MS`], with the
/// features [`classify_held`] needs. Pure.
pub(crate) fn held_candidates(
    pcm: &[f32],
    sample_rate: u32,
    ctc: &CtcFrames,
) -> Vec<HeldCandidate> {
    let db = frame_energy_db(pcm, sample_rate);
    if db.is_empty() {
        return Vec::new();
    }
    let gate = silence_floor_db(&db) + VOICED_MARGIN_DB;
    let active: Vec<bool> = db.iter().map(|&v| v > gate).collect();
    let f0 = f0_track(pcm, sample_rate, Some(&active));
    let words = ctc_word_spans(ctc);
    // A hop's pitch describes the middle of its window.
    let centre = |h: usize| h as f32 * HOP_MS + PITCH_WINDOW_MS / 2.0;

    let mut out = Vec::new();
    for (a, b) in stable_runs(&f0) {
        let start_ms = (centre(a) - HOP_MS / 2.0).round() as i64;
        let end_ms = (centre(b) + HOP_MS / 2.0).round() as i64;
        if end_ms - start_ms < CANDIDATE_MIN_MS {
            continue;
        }
        let voiced: Vec<usize> = (a..=b).filter(|&h| f0[h].is_some()).collect();
        let st: Vec<f32> = voiced
            .iter()
            .filter_map(|&h| f0[h].map(semitones))
            .collect();
        let lv: Vec<f32> = voiced.iter().filter_map(|&h| db.get(h).copied()).collect();
        let mean = lv.iter().sum::<f32>() / lv.len().max(1) as f32;
        let var = lv.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / lv.len().max(1) as f32;

        let (mut blank, mut total, mut letters, mut prev) = (0usize, 0usize, String::new(), 0u8);
        if ctc.stride_ms > 0.0 {
            let f_from = (start_ms as f32 / ctc.stride_ms).floor() as usize;
            let f_to = ((end_ms as f32 / ctc.stride_ms).ceil() as usize).min(ctc.label.len());
            for f in f_from..f_to {
                total += 1;
                if ctc
                    .blank_logp
                    .get(f)
                    .is_some_and(|&p| p > BLANK_DOMINANT_LOGP)
                {
                    blank += 1;
                }
                let l = ctc.label[f];
                if l != 0 && l != b' ' && l != prev {
                    letters.push(l as char);
                }
                prev = l;
            }
        }

        let dur = (end_ms - start_ms) as f32;
        let under_words: Vec<&CtcWord> = words
            .iter()
            .filter(|w| {
                let ov = (w.end_ms.min(end_ms) - w.start_ms.max(start_ms)).max(0) as f32;
                ov >= WORD_OVERLAP_FRAC * dur
            })
            .collect();
        let filler_shaped =
            |w: &CtcWord| w.text.len() <= 2 && w.text.bytes().all(|c| FILLER_LETTERS.contains(&c));
        let under = if under_words.is_empty() {
            UnderWord::None
        } else if under_words
            .iter()
            .any(|w| count_fillers(&w.text).certain > 0)
        {
            UnderWord::SpelledFiller
        } else if under_words.iter().all(|w| filler_shaped(w)) {
            UnderWord::FillerShaped
        } else {
            UnderWord::Real
        };

        out.push(HeldCandidate {
            start_ms,
            end_ms,
            f0_spread_st: quantile(&st, 0.9) - quantile(&st, 0.1),
            energy_sd_db: var.sqrt(),
            blank_frac: if total > 0 {
                blank as f32 / total as f32
            } else {
                1.0
            },
            letters,
            under,
            words: under_words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        });
    }
    out
}

/// The decision rule over one candidate's features.
pub(crate) fn classify_held(c: &HeldCandidate) -> Option<HeldKind> {
    let dur = c.end_ms - c.start_ms;
    if !(HELD_MIN_MS..=HELD_MAX_MS).contains(&dur)
        || c.f0_spread_st > HELD_SPREAD_ST
        || c.energy_sd_db > HELD_ENERGY_SD_DB
    {
        return None;
    }
    match c.under {
        // Already in the text filler count; counting it here would double it.
        UnderWord::SpelledFiller => None,
        UnderWord::Real => (dur >= LENGTHENING_MIN_MS).then_some(HeldKind::Lengthening),
        UnderWord::None | UnderWord::FillerShaped => {
            let filler_letters = c.letters.bytes().all(|l| FILLER_LETTERS.contains(&l));
            (filler_letters && c.blank_frac >= BLANK_FRAC_MIN).then_some(HeldKind::FilledPause)
        }
    }
}

/// Filled pauses and lengthened words in one recording. Pure.
///
/// `ctc` is the greedy path the transcript was decoded from, so its word
/// spans carry the transcript's words with timings: a filler the model did
/// spell ("I UM THINK") is left to [`count_fillers`] rather than counted
/// twice.
#[allow(dead_code)] // Phase 7 Stage 5 step 5.
pub fn detect_held_sounds(pcm: &[f32], sample_rate: u32, ctc: &CtcFrames) -> Vec<HeldSound> {
    held_candidates(pcm, sample_rate, ctc)
        .iter()
        .filter_map(|c| {
            classify_held(c).map(|kind| HeldSound {
                start_ms: c.start_ms,
                end_ms: c.end_ms,
                kind,
            })
        })
        .collect()
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
    fn a_long_correct_word_is_not_a_hesitation() {
        // Scores are percentiles among words experts rated perfect
        // (`GOP_PERCENTILE`), so 28 is an ordinary correct word. Any GOP
        // below 0 maps to 34 or less; a cutoff above that counts every long
        // word that was not a perfect GOP-0 match.
        let words = [word("UNFORTUNATELY", 0, 800, 28)];
        assert_eq!(count_hesitations(&words), 0);
    }

    #[test]
    fn hesitations_are_long_words_the_model_scored_badly() {
        let words = [
            word("I", 0, 200, 95),
            word("SUPPOSE", 300, 1100, 5),
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
                let score = if i % 2 == 0 { 5 } else { 90 };
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

    // --- held sounds -------------------------------------------------------

    /// `(duration_ms, f0_from, f0_to, amplitude)` segments of harmonics `ks`
    /// at amplitude 1/k, continuous phase, linear pitch glide. A zero
    /// amplitude is digital silence.
    fn harmonics(
        sr: u32,
        segments: &[(f32, f32, f32, f32)],
        ks: std::ops::RangeInclusive<u32>,
    ) -> Vec<f32> {
        let norm: f32 = ks.clone().map(|k| 1.0 / k as f32).sum();
        let tau = std::f64::consts::TAU;
        let mut out = Vec::new();
        let mut phase = 0.0f64;
        for &(ms, from, to, amp) in segments {
            let n = ((ms / 1000.0) * sr as f32).round() as usize;
            for i in 0..n {
                let f = from + (to - from) * i as f32 / n as f32;
                phase = (phase + tau * f as f64 / sr as f64) % tau;
                let s: f32 = ks
                    .clone()
                    .map(|k| (k as f64 * phase).sin() as f32 / k as f32)
                    .sum();
                out.push(amp * s / norm);
            }
        }
        out
    }

    /// A voice-like tone: harmonics 1-5.
    fn voice(sr: u32, segments: &[(f32, f32, f32, f32)]) -> Vec<f32> {
        harmonics(sr, segments, 1..=5)
    }

    /// 600 ms of "words": three 140 ms syllables whose pitch glides several
    /// semitones, each followed by a 60 ms stop. Nothing in it is held.
    const WORDS: [(f32, f32, f32, f32); 6] = [
        (140.0, 130.0, 175.0, 0.3),
        (60.0, 0.0, 0.0, 0.0),
        (140.0, 190.0, 140.0, 0.25),
        (60.0, 0.0, 0.0, 0.0),
        (140.0, 120.0, 160.0, 0.3),
        (60.0, 0.0, 0.0, 0.0),
    ];

    /// Words, 100 ms of silence, `held`, 100 ms of silence, words.
    fn around(held: (f32, f32, f32, f32)) -> Vec<f32> {
        let mut segs = WORDS.to_vec();
        segs.push((100.0, 0.0, 0.0, 0.0));
        segs.push(held);
        segs.push((100.0, 0.0, 0.0, 0.0));
        segs.extend(WORDS);
        voice(16_000, &segs)
    }

    /// One 20 ms CTC frame per character: `.` blank, `|` delimiter,
    /// anything else that letter.
    fn ctc_pattern(s: &str, stride_ms: f32) -> CtcFrames {
        let mut f = CtcFrames {
            stride_ms,
            ..CtcFrames::default()
        };
        for c in s.bytes() {
            let (label, p_blank) = match c {
                b'.' => (0, 0.9f32),
                b'|' => (b' ', 0.05),
                l => (l, 0.05),
            };
            f.label.push(label);
            f.blank_logp.push(p_blank.ln());
        }
        f
    }

    /// `ms` worth of 20 ms frames with `chars` spread evenly, blanks between.
    fn spell(ms: f32, chars: &str) -> String {
        let n = (ms / 20.0).round() as usize;
        let mut out = vec!['.'; n];
        let c: Vec<char> = chars.chars().collect();
        for (i, ch) in c.iter().enumerate() {
            out[i * n / c.len()] = *ch;
        }
        out.into_iter().collect()
    }

    /// CTC for `around`: `a` over the first words, `held` over the held
    /// sound, `b` over the last words, delimiters in the silences.
    fn ctc_around(a: &str, held_ms: f32, held: &str, b: &str) -> CtcFrames {
        let s = [
            spell(600.0, a),
            spell(100.0, "|"),
            spell(held_ms, held),
            spell(100.0, "|"),
            spell(600.0, b),
        ]
        .concat();
        ctc_pattern(&s, 20.0)
    }

    fn voiced_f0(pcm: &[f32]) -> Vec<f32> {
        f0_track(pcm, 16_000, None).into_iter().flatten().collect()
    }

    #[test]
    fn f0_of_a_pure_tone_is_its_frequency() {
        for hz in [100.0f32, 150.0, 220.0, 300.0] {
            let pcm = harmonics(16_000, &[(500.0, hz, hz, 0.5)], 1..=1);
            let track = f0_track(&pcm, 16_000, None);
            let f0 = voiced_f0(&pcm);
            assert!(
                f0.len() * 10 >= track.len() * 8,
                "{hz} Hz: only {} of {} hops voiced",
                f0.len(),
                track.len()
            );
            for f in f0 {
                assert!((f - hz).abs() <= 0.02 * hz, "{hz} Hz tracked as {f}");
            }
        }
    }

    #[test]
    fn f0_is_not_fooled_by_a_missing_fundamental() {
        // Harmonics 2-5 of 120 Hz: no energy at 120 Hz itself, but the
        // waveform still repeats every 1/120 s, which is what a listener hears.
        let pcm = harmonics(16_000, &[(500.0, 120.0, 120.0, 0.5)], 2..=5);
        let f0 = voiced_f0(&pcm);
        assert!(!f0.is_empty());
        let median = quantile(&f0, 0.5);
        assert!(
            (median - 120.0).abs() <= 2.4,
            "tracked {median} Hz, not the 120 Hz period"
        );
    }

    #[test]
    fn white_noise_is_not_voiced() {
        let mut state = 0x2545_f491u32;
        let pcm: Vec<f32> = (0..16_000)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * 0.3
            })
            .collect();
        assert!(voiced_f0(&pcm).is_empty(), "noise has no period");
    }

    #[test]
    fn a_held_flat_vowel_between_words_is_a_filled_pause() {
        let pcm = around((500.0, 110.0, 110.0, 0.3));
        let ctc = ctc_around("SO|I", 500.0, "", "THINK");
        let held = detect_held_sounds(&pcm, 16_000, &ctc);
        let fp: Vec<&HeldSound> = held
            .iter()
            .filter(|h| h.kind == HeldKind::FilledPause)
            .collect();
        assert_eq!(fp.len(), 1, "expected one filled pause, got {held:?}");
        assert!((fp[0].start_ms - 700).abs() <= 30, "{:?}", fp[0]);
        assert!((fp[0].end_ms - 1200).abs() <= 30, "{:?}", fp[0]);
    }

    #[test]
    fn a_pitch_glide_is_not_held() {
        // 110 -> 180 Hz is 8.5 semitones in half a second: intonation, not a
        // held sound, although the model spelled nothing there either.
        let pcm = around((500.0, 110.0, 180.0, 0.3));
        let ctc = ctc_around("SO|I", 500.0, "", "THINK");
        let held = detect_held_sounds(&pcm, 16_000, &ctc);
        assert!(held.is_empty(), "{held:?}");
    }

    #[test]
    fn too_short_and_too_long_are_rejected() {
        for ms in [120.0f32, 2500.0] {
            let pcm = around((ms, 110.0, 110.0, 0.3));
            let ctc = ctc_around("SO|I", ms, "", "THINK");
            let held = detect_held_sounds(&pcm, 16_000, &ctc);
            assert!(held.is_empty(), "{ms} ms: {held:?}");
        }
    }

    #[test]
    fn a_spelled_real_word_is_a_lengthening_not_a_filler() {
        let pcm = around((400.0, 140.0, 140.0, 0.3));
        let ctc = ctc_around("HELLO", 400.0, "WORLD", "AGAIN");
        let held = detect_held_sounds(&pcm, 16_000, &ctc);
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!(held[0].kind, HeldKind::Lengthening);
    }

    #[test]
    fn a_filler_the_transcript_already_spelled_is_not_counted_twice() {
        // "I UM THINK": the model spelled the UM, so count_fillers has it.
        let pcm = around((500.0, 110.0, 110.0, 0.3));
        let ctc = ctc_around("I", 500.0, "UM", "THINK");
        let words: Vec<String> = ctc_word_spans(&ctc).into_iter().map(|w| w.text).collect();
        assert_eq!(words, ["I", "UM", "THINK"]);
        let held = detect_held_sounds(&pcm, 16_000, &ctc);
        assert!(held.is_empty(), "{held:?}");
    }

    #[test]
    fn hum_at_the_room_floor_is_not_speech() {
        // A steady -40 dBFS mains hum under the whole recording, alone for
        // 600 ms twice: perfectly periodic and perfectly flat, so everything
        // but the energy gate would call those stretches held vowels.
        let mut segs = WORDS.to_vec();
        segs.push((600.0, 0.0, 0.0, 0.0));
        segs.extend(WORDS);
        segs.push((600.0, 0.0, 0.0, 0.0));
        segs.extend(WORDS);
        let mut pcm = voice(16_000, &segs);
        let hum = harmonics(16_000, &[(3000.0, 120.0, 120.0, 0.01)], 1..=1);
        for (x, h) in pcm.iter_mut().zip(hum) {
            *x += h;
        }
        let ctc = ctc_pattern(
            &[
                spell(600.0, "SO"),
                spell(600.0, "|"),
                spell(600.0, "I"),
                spell(600.0, "|"),
                spell(600.0, "THINK"),
            ]
            .concat(),
            20.0,
        );
        let held = detect_held_sounds(&pcm, 16_000, &ctc);
        assert!(held.is_empty(), "{held:?}");
    }

    #[test]
    fn ctc_word_spans_split_on_delimiter_and_blank_runs() {
        // Blanks between letters stay inside a word and doubled frames
        // collapse; `|` ends a word, and so does a long enough blank run.
        let s = format!(".HH.E.L.L.O|.W.O.R.L.D{}A.B", ".".repeat(30));
        let words = ctc_word_spans(&ctc_pattern(&s, 20.0));
        let got: Vec<(&str, i64, i64)> = words
            .iter()
            .map(|w| (w.text.as_str(), w.start_ms, w.end_ms))
            .collect();
        assert_eq!(
            got,
            [("HELLO", 20, 220), ("WORLD", 260, 440), ("AB", 1040, 1100)]
        );
    }
}

#[cfg(test)]
mod corpus {
    //! Measurement data for [`detect_held_sounds`], against hand-transcribed
    //! filled pauses.
    //!
    //! `#[ignore]`d and needs two things off-repo: the model files, and a
    //! directory holding `segments.tsv` plus the WAVs it names, as written by
    //! `scripts/prepare-ami-fillers.py` from the AMI Meeting Corpus (CC BY
    //! 4.0; individual headset microphones, mostly non-native speakers):
    //!
    //!   python3 scripts/prepare-ami-fillers.py ~/corpora/ami ~/corpora/ami-fillers
    //!   OLP_FILLER_DIR=~/corpora/ami-fillers OLP_MODELS_DIR=$PWD/models \
    //!     cargo test --release --manifest-path src-tauri/Cargo.toml \
    //!     --lib corpus_dump_fillers -- --ignored --nocapture
    //!   python3 scripts/eval-fillers.py ~/corpora/ami-fillers
    //!
    //! It writes every held-sound *candidate* with its features, not just the
    //! detections, so the thresholds can be swept outside the build; plus the
    //! fillers the transcript spelled, which is the text-only baseline the
    //! detector has to beat. Only the ASR model is loaded.
    //!
    //! Result (2026-09-28; thresholds swept on the 12 dev meetings, frozen,
    //! then the 12 test meetings scored once; 3,542 reference um/uh): on test
    //! precision 0.82 (Wilson 0.79-0.85), recall 0.32, against 0.09 recall for
    //! the transcript alone. The gate needs recall 0.40, so it is NO-GO and
    //! nothing here is wired in. No threshold setting reaches 0.40 on dev even
    //! at precision 0.5: roughly 30% of the fillers sit under a real word
    //! the model bent them into ("AND", "I"), where this detector cannot tell
    //! a filled pause from a lengthening.

    use super::*;
    use std::io::Write;
    use std::path::Path;

    /// 16-bit PCM mono WAV to samples in [-1, 1], plus its sample rate.
    ///
    /// A copy of the reader in `pronounce::corpus`, kept separate so the two
    /// harnesses do not reach into each other's private test modules.
    fn read_wav(path: &Path) -> (Vec<f32>, u32) {
        let b = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE");
        let (mut i, mut rate, mut bits, mut channels) = (12usize, 0u32, 0u16, 0u16);
        while i + 8 <= b.len() {
            let id = &b[i..i + 4];
            let len = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
            let body = &b[i + 8..(i + 8 + len).min(b.len())];
            if id == b"fmt " && body.len() >= 16 {
                channels = u16::from_le_bytes([body[2], body[3]]);
                rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                bits = u16::from_le_bytes([body[14], body[15]]);
            } else if id == b"data" {
                assert_eq!(
                    (channels, bits),
                    (1, 16),
                    "{}: not 16-bit mono",
                    path.display()
                );
                let pcm = body
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| i16::from_le_bytes(*c) as f32 / 32768.0)
                    .collect();
                return (pcm, rate);
            }
            i += 8 + len + (len & 1);
        }
        panic!("{}: no data chunk", path.display());
    }

    fn under_name(u: UnderWord) -> &'static str {
        match u {
            UnderWord::None => "none",
            UnderWord::FillerShaped => "filler_shaped",
            UnderWord::SpelledFiller => "spelled_filler",
            UnderWord::Real => "real",
        }
    }

    #[test]
    #[ignore = "corpus"]
    fn corpus_dump_fillers() {
        let root = std::path::PathBuf::from(
            std::env::var_os("OLP_FILLER_DIR")
                .expect("set OLP_FILLER_DIR to a prepare-ami-fillers.py output; see module doc"),
        );
        if std::env::var_os("OLP_MODELS_DIR").is_none() {
            let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../models");
            std::env::set_var("OLP_MODELS_DIR", models);
        }
        let model = crate::asr::find_model().expect("no ASR model; set OLP_MODELS_DIR");
        let asr = crate::asr::AsrEngine::load(&model).expect("load ASR");

        let listing = std::fs::read_to_string(root.join("segments.tsv")).expect("segments.tsv");
        let mut lines = listing.lines();
        let header: Vec<&str> = lines.next().expect("header").split('\t').collect();
        let col = |name: &str| {
            header
                .iter()
                .position(|h| *h == name)
                .unwrap_or_else(|| panic!("segments.tsv has no {name} column"))
        };
        let (c_id, c_wav, c_split) = (col("id"), col("wav"), col("split"));

        let out_path = root.join("filler_dump.tsv");
        let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path).unwrap());
        // row = seg | text | cand. A `seg` row carries the transcript in
        // `words` and its count_fillers total in `kind`; a `text` row is one
        // filler the transcript spelled; a `cand` row is one candidate, `kind`
        // being what the current thresholds make of it.
        writeln!(
            out,
            "row\tid\tsplit\tstart_ms\tend_ms\tspread_st\tenergy_sd_db\tblank_frac\t\
             letters\tunder\twords\tkind"
        )
        .unwrap();

        let (mut segs, mut cands, mut refused) = (0usize, 0usize, 0usize);
        let started = std::time::Instant::now();
        for line in lines {
            let f: Vec<&str> = line.split('\t').collect();
            let (id, split) = (f[c_id], f[c_split]);
            let (pcm, rate) = read_wav(&root.join(f[c_wav]));
            assert_eq!(rate, crate::asr::ASR_SAMPLE_RATE, "{id}: resample first");
            let (text, ctc) = match asr.transcribe_pcm_frames(&pcm) {
                Ok(v) => v,
                Err(_) => {
                    // Digital silence and the like: the segment still counts,
                    // with nothing detected and nothing spelled.
                    refused += 1;
                    (String::new(), CtcFrames::default())
                }
            };
            let dur_ms = (pcm.len() as f32 * 1000.0 / rate as f32).round() as i64;
            writeln!(
                out,
                "seg\t{id}\t{split}\t0\t{dur_ms}\t\t\t\t\t\t{}\t{}",
                text.trim(),
                count_fillers(&text).certain
            )
            .unwrap();
            for w in ctc_word_spans(&ctc) {
                if count_fillers(&w.text).certain > 0 {
                    writeln!(
                        out,
                        "text\t{id}\t{split}\t{}\t{}\t\t\t\t{}\t\t{}\t",
                        w.start_ms, w.end_ms, w.text, w.text
                    )
                    .unwrap();
                }
            }
            for c in held_candidates(&pcm, rate, &ctc) {
                let kind = match classify_held(&c) {
                    Some(HeldKind::FilledPause) => "filled_pause",
                    Some(HeldKind::Lengthening) => "lengthening",
                    None => "",
                };
                writeln!(
                    out,
                    "cand\t{id}\t{split}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{}\t{}\t{}\t{kind}",
                    c.start_ms,
                    c.end_ms,
                    c.f0_spread_st,
                    c.energy_sd_db,
                    c.blank_frac,
                    c.letters,
                    under_name(c.under),
                    c.words
                )
                .unwrap();
                cands += 1;
            }
            segs += 1;
            if segs % 250 == 0 {
                eprintln!(
                    "{segs} segments, {cands} candidates, {:.0} s",
                    started.elapsed().as_secs_f32()
                );
            }
        }
        out.flush().unwrap();
        eprintln!(
            "done: {segs} segments ({refused} refused by the ASR), {cands} candidates in \
             {:.0} s -> {}",
            started.elapsed().as_secs_f32(),
            out_path.display()
        );
        assert!(segs > 0);
    }
}
