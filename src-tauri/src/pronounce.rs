//! Word-level comparison of what was said against what should have been said.
//!
//! Pure: no IO, no `ort`, no database. That is deliberate — it is the part of
//! scoring that can be tested exhaustively, and it is what the practice loop
//! falls back to whenever acoustic scoring (below) refuses a recording.
//!
//! The comparison is a word-level Levenshtein alignment. Both sides are
//! normalized first, because a wav2vec2 CTC transcript is upper-case and
//! carries no punctuation while a prompt is written like English: comparing
//! `"I'M FINE"` against `"I'm fine, thanks."` must not report three errors
//! that are really typography.
//!
//! On top of that sits the acoustic half: a CTC forward pass and forced
//! alignment over the frame posteriors, giving a Goodness-of-Pronunciation
//! score per word. Still pure — it is handed posteriors, it never runs a
//! model.
//!
//! # Limitation: this is a *grapheme* GOP, not a phoneme GOP
//!
//! The wav2vec2 CTC vocab used here is grapheme-level: its labels are
//! letters, not phones. So a word's score says how confidently the audio
//! spells the word, which is only a proxy for how well it was articulated.
//! English orthography is not phonetic, so the measure conflates spelling
//! with pronunciation — `THROUGH` and `ROUGH` are penalised unevenly — and
//! it can never name *which phoneme* was wrong, only that the word as a
//! whole drifted. A true phoneme GOP needs a separate phone-level acoustic
//! model of roughly 300 MB-1.2 GB, which is not shipped; if one is ever
//! added it should be an optional download surfaced as
//! `model_status.asr_phoneme` rather than a silent change in meaning here.
//!
//! # What it has been measured to do
//!
//! Against expert ratings (speechocean762; see [`GOP_PERCENTILE`]) the
//! per-word signal separates mispronounced words from correct ones at AUC
//! ~0.80, but only about one flag in four is a real error. So a word is
//! flagged for a second listen and never marked wrong, and the sentence mean
//! (Pearson ~0.47 with expert sentence accuracy) is the number to trust more.

use std::ops::Range;

use serde::Serialize;

use crate::asr::{AsrOutput, Vocab};

/// One step of the alignment between the spoken words and the target words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WordOp {
    /// Said correctly.
    Match {
        hyp_index: usize,
        target_index: usize,
        word: String,
    },
    /// Said, but a different word than the target.
    Sub {
        hyp_index: usize,
        target_index: usize,
        spoken: String,
        expected: String,
    },
    /// Said, but not in the target at all.
    Ins { hyp_index: usize, word: String },
    /// In the target, but not said.
    Del { target_index: usize, word: String },
}

/// The alignment plus the counts a UI actually wants to show.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WordAlignment {
    pub ops: Vec<WordOp>,
    pub matched: usize,
    pub substituted: usize,
    pub inserted: usize,
    pub deleted: usize,
    /// Target words said correctly, 0..=100. `None` when there is no target
    /// to compare against — an empty target has no accuracy, and reporting
    /// 0 or 100 would both be lies.
    pub accuracy: Option<u8>,
}

/// Split text into comparable words.
///
/// Upper-cases, drops everything that is not a letter, digit or an internal
/// apostrophe, and discards empties. Apostrophes are kept inside a word so
/// `DON'T` stays one token (the ASR vocab emits it that way), but a leading
/// or trailing quote mark is punctuation and goes.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            word.extend(ch.to_uppercase());
        } else if (ch == '\'' || ch == '\u{2019}') && !word.is_empty() {
            // Only meaningful between letters; a trailing one is trimmed below.
            word.push('\'');
        } else if !word.is_empty() {
            push_word(&mut out, &mut word);
        }
    }
    push_word(&mut out, &mut word);
    out
}

fn push_word(out: &mut Vec<String>, word: &mut String) {
    let trimmed = word.trim_end_matches('\'');
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    word.clear();
}

/// Align spoken words against target words.
///
/// Standard Levenshtein with unit costs, backtracked into explicit ops. Ties
/// resolve toward `Match`/`Sub` first so the alignment stays in step with the
/// target rather than drifting through a run of insertions and deletions
/// that happen to cost the same.
pub fn align_words(hyp: &str, target: &str) -> WordAlignment {
    let h = tokenize(hyp);
    let t = tokenize(target);
    let ops = align_tokens(&h, &t);

    let mut matched = 0;
    let mut substituted = 0;
    let mut inserted = 0;
    let mut deleted = 0;
    for op in &ops {
        match op {
            WordOp::Match { .. } => matched += 1,
            WordOp::Sub { .. } => substituted += 1,
            WordOp::Ins { .. } => inserted += 1,
            WordOp::Del { .. } => deleted += 1,
        }
    }
    let accuracy = if t.is_empty() {
        None
    } else {
        // Extra words are not counted against accuracy here: they are
        // reported separately, because "said more than asked" is a different
        // failure from "got the words wrong".
        Some(
            ((matched as f32 / t.len() as f32) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u8,
        )
    };

    WordAlignment {
        ops,
        matched,
        substituted,
        inserted,
        deleted,
        accuracy,
    }
}

fn align_tokens(h: &[String], t: &[String]) -> Vec<WordOp> {
    let n = h.len();
    let m = t.len();
    // (n+1) x (m+1) cost table, row-major.
    let width = m + 1;
    let mut cost = vec![0u32; (n + 1) * width];
    for i in 0..=n {
        cost[i * width] = i as u32;
    }
    for (j, slot) in cost.iter_mut().take(m + 1).enumerate() {
        *slot = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = cost[(i - 1) * width + (j - 1)] + u32::from(h[i - 1] != t[j - 1]);
            let del = cost[(i - 1) * width + j] + 1; // consume a spoken word
            let ins = cost[i * width + (j - 1)] + 1; // consume a target word
            cost[i * width + j] = sub.min(del).min(ins);
        }
    }

    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let diag = cost[(i - 1) * width + (j - 1)];
            let penalty = u32::from(h[i - 1] != t[j - 1]);
            if cost[i * width + j] == diag + penalty {
                ops.push(if penalty == 0 {
                    WordOp::Match {
                        hyp_index: i - 1,
                        target_index: j - 1,
                        word: t[j - 1].clone(),
                    }
                } else {
                    WordOp::Sub {
                        hyp_index: i - 1,
                        target_index: j - 1,
                        spoken: h[i - 1].clone(),
                        expected: t[j - 1].clone(),
                    }
                });
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && cost[i * width + j] == cost[(i - 1) * width + j] + 1 {
            ops.push(WordOp::Ins {
                hyp_index: i - 1,
                word: h[i - 1].clone(),
            });
            i -= 1;
            continue;
        }
        // Only a deletion is left; `j > 0` is implied by the table invariants
        // but checked so a malformed table cannot underflow.
        if j > 0 {
            ops.push(WordOp::Del {
                target_index: j - 1,
                word: t[j - 1].clone(),
            });
            j -= 1;
        } else {
            break;
        }
    }
    ops.reverse();
    ops
}

// ---------------------------------------------------------------------------
// Acoustic scoring: CTC forward, forced alignment, Goodness of Pronunciation
// ---------------------------------------------------------------------------

/// Where a word's GOP falls among words that expert raters scored as
/// perfectly pronounced: `(gop, percentile)` knots, linearly interpolated.
///
/// So a score of 10 means "this word's audio matched the target less well
/// than 90% of correctly-said words do". It is a percentile, not a grade:
/// about 64% of correctly-said words tie at GOP ~ 0, which is why the last
/// knot jumps from 34 to 100.
///
/// Measured, not chosen. Built by `scripts/calibrate-gop.py` from the
/// `corpus::corpus_dump_gop` run over speechocean762 (OpenSLR-101, CC BY 4.0:
/// 5,000 read sentences by L1-Mandarin learners, half of them children, every
/// word rated 0-10 by five experts). Fitted on the corpus's train split;
/// every figure here is on its test split, as the table is interpolated:
///
/// * Flagging below [`FLAG_BELOW`] hits 10.2% of words the experts rated
///   correct (>= 7) and catches 37% of the ones they rated mispronounced
///   (<= 6). Discrimination is AUC 0.80 and no mapping can raise it; at any
///   cutoff only ~20-28% of flagged words are actually mispronounced, so a
///   flag is a hint to listen again, never a verdict.
/// * The sentence score — the mean of these — tracks expert sentence
///   accuracy at Pearson 0.47.
///
/// Scope: fitted and validated on L1-Mandarin speakers only. Elsewhere only
/// false alarms have been measured: CMU ARCTIC's seven native and
/// near-native studio readers get 1.6-4.6% of words flagged (vs 10.2%
/// here), and whether flags catch real mistakes by speakers of other first
/// languages is unmeasured (`calibrate-gop.py --eval-on`). This replaced `100 * exp(gop / 0.55)`, a
/// hand-picked constant that on the same data called 28% of correctly-said
/// words "poor".
pub const GOP_PERCENTILE: [(f32, u8); 25] = [
    (-14.3822, 0),
    (-7.8743, 1),
    (-6.5342, 1),
    (-5.4974, 2),
    (-4.9056, 3),
    (-4.4019, 4),
    (-4.0104, 5),
    (-3.7124, 6),
    (-3.4266, 7),
    (-3.1950, 8),
    (-2.9261, 9),
    (-2.7203, 10),
    (-2.2821, 12),
    (-1.9404, 14),
    (-1.6192, 16),
    (-1.3679, 18),
    (-1.1228, 20),
    (-0.9198, 22),
    (-0.7237, 24),
    (-0.5557, 26),
    (-0.4010, 28),
    (-0.2566, 30),
    (-0.1262, 32),
    (-0.0158, 34),
    (0.0000, 100),
];

/// Words scoring below this are flagged for a second listen ("unclear").
///
/// The 10th percentile of correctly-said words: a learner who says every
/// word right still sees about one flag in ten words. Raising it catches more
/// real mistakes at the cost of more false alarms — `calibrate-gop.py` prints
/// the whole trade-off. Acoustic scoring never says "poor": with ~25%
/// precision it can suggest a word deserves attention, not that it was wrong.
pub const FLAG_BELOW: u8 = 10;

/// Why a phrase could not be scored acoustically.
///
/// Every variant means "fall back to text alignment", never "show a zero" —
/// a score that could not be computed is not a bad score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScoreError {
    /// The target had no pronounceable words.
    EmptyTarget,
    /// Characters the acoustic model has no label for.
    UnknownChars(String),
    /// The vocab has no word-delimiter token, so words cannot be separated.
    NoWordDelimiter,
    /// The blank id is not a valid index into the vocab.
    BadVocab,
    /// No CTC path spells the target within the available frames, or the
    /// posteriors were malformed.
    NotAlignable,
}

impl std::fmt::Display for ScoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyTarget => write!(f, "target has no pronounceable words"),
            Self::UnknownChars(c) => {
                write!(f, "target uses characters the model cannot score: {c}")
            }
            Self::NoWordDelimiter => write!(f, "vocab has no word-delimiter token"),
            Self::BadVocab => write!(f, "vocab blank id is out of range"),
            Self::NotAlignable => write!(f, "audio is too short to contain the target"),
        }
    }
}

/// A target phrase turned into acoustic labels, with the word structure kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetLabels {
    /// Label ids, word-delimiter tokens included.
    pub ids: Vec<usize>,
    /// Each word and the half-open range of `ids` that spells it.
    pub words: Vec<(String, Range<usize>)>,
}

/// One run of frames the forced alignment assigned to a single label.
///
/// Spans are monotone, non-overlapping, and together cover `[0, frames)` —
/// blank runs included, so the sequence can be read as a timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Span {
    /// Index into the extended label sequence `l'`.
    pub ext_index: usize,
    /// The token id emitted over this run (may be the blank).
    pub label: usize,
    pub start: usize,
    pub end: usize,
}

/// Per-word acoustic result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WordScore {
    pub word: String,
    pub start_ms: i64,
    pub end_ms: i64,
    /// Mean log-posterior ratio over the word's frames; always `<= 0`.
    pub gop: f32,
    pub score: u8,
    /// `"good"` / `"unclear"` / `"poor"`.
    pub verdict: String,
}

/// The acoustic half of a pronunciation report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PronScore {
    pub overall: u8,
    pub words: Vec<WordScore>,
    /// Total log-probability of the target under the model.
    pub target_logprob: f32,
    /// Log-probability of the model's own best frame-wise path — the ceiling
    /// `target_logprob` is measured against.
    pub free_logprob: f32,
    /// `exp((target - free) / frames)`, in `(0, 1]`. Length-independent, so a
    /// long sentence and a short one are comparable.
    pub normalized_conf: f32,
}

/// `ln(exp(a) + exp(b))`, safe at `-inf` and never `NaN`.
///
/// `f64` because it is the inner loop of the forward pass: a long utterance
/// folds thousands of these together and `f32` drifts visibly by the end.
pub fn log_add_exp(a: f64, b: f64) -> f64 {
    if a == f64::NEG_INFINITY {
        return b;
    }
    if b == f64::NEG_INFINITY {
        return a;
    }
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    // `ln_1p` keeps precision when the two terms are far apart, where
    // `ln(1 + x)` with a tiny `x` would round to `ln(1) == 0`.
    hi + (lo - hi).exp().ln_1p()
}

/// Turn a written target into acoustic labels.
///
/// Words are normalized exactly as [`tokenize`] normalizes a transcript, then
/// spelled out character by character with the vocab's word-delimiter token
/// between words. Grapheme-level: see the note in the module header.
pub fn text_to_labels(target: &str, v: &Vocab) -> Result<TargetLabels, ScoreError> {
    let words = tokenize(target);
    if words.is_empty() {
        return Err(ScoreError::EmptyTarget);
    }
    let delim = word_delimiter(v).ok_or(ScoreError::NoWordDelimiter)?;

    let mut ids: Vec<usize> = Vec::new();
    let mut spans: Vec<(String, Range<usize>)> = Vec::new();
    let mut unknown = String::new();
    for (i, word) in words.iter().enumerate() {
        if i > 0 {
            ids.push(delim);
        }
        let start = ids.len();
        for ch in word.chars() {
            match lookup_char(ch, v) {
                Some(id) => ids.push(id),
                None => {
                    if !unknown.contains(ch) {
                        unknown.push(ch);
                    }
                }
            }
        }
        if ids.len() > start {
            spans.push((word.clone(), start..ids.len()));
        }
    }
    if !unknown.is_empty() {
        return Err(ScoreError::UnknownChars(unknown));
    }
    if spans.is_empty() {
        return Err(ScoreError::EmptyTarget);
    }
    Ok(TargetLabels { ids, words: spans })
}

/// Turn per-word phones into acoustic labels for a phone-level model.
///
/// No word delimiter: the phone vocab has none, and CTC does not need one —
/// `words` ranges carry the structure, and [`score_labels`] reads word
/// membership from them whatever the labels are. Phones are looked up
/// verbatim (IPA is case-sensitive). Any phone the vocab lacks refuses the
/// whole target, so the caller falls back to the grapheme path rather than
/// scoring a target with holes in it.
// Consumed only by the phone-GOP corpus dump: the Stage 6A measurement was
// NO-GO, so the app's A7 consumer (phone-level scoring) did not land.
#[allow(dead_code)]
pub fn phones_to_labels(
    words: &[crate::phonemize::WordPhones],
    v: &Vocab,
) -> Result<TargetLabels, ScoreError> {
    let mut ids: Vec<usize> = Vec::new();
    let mut spans: Vec<(String, Range<usize>)> = Vec::new();
    let mut unknown: Vec<&str> = Vec::new();
    for (word, phones) in words {
        let start = ids.len();
        for ph in phones {
            match v
                .token_to_id
                .get(ph.as_str())
                .and_then(|id| usize::try_from(*id).ok())
            {
                Some(id) => ids.push(id),
                None => {
                    if !unknown.contains(&ph.as_str()) {
                        unknown.push(ph);
                    }
                }
            }
        }
        if ids.len() > start {
            spans.push((word.clone(), start..ids.len()));
        }
    }
    if !unknown.is_empty() {
        return Err(ScoreError::UnknownChars(unknown.join(" ")));
    }
    if spans.len() != words.len() || spans.is_empty() {
        return Err(ScoreError::EmptyTarget);
    }
    Ok(TargetLabels { ids, words: spans })
}

/// The vocab's word-delimiter id. wav2vec2 CTC checkpoints spell it `|`.
fn word_delimiter(v: &Vocab) -> Option<usize> {
    v.token_to_id
        .get("|")
        .and_then(|id| usize::try_from(*id).ok())
}

/// Look a single character up, trying the upper-case form the tokenizer
/// produces first and the lower-case form second (some vocabs are lower-case).
fn lookup_char(ch: char, v: &Vocab) -> Option<usize> {
    let mut buf = [0u8; 4];
    let upper: String = ch.to_uppercase().collect();
    let lower: String = ch.to_lowercase().collect();
    for key in [ch.encode_utf8(&mut buf) as &str, &upper, &lower] {
        if let Some(id) = v.token_to_id.get(key) {
            if let Ok(id) = usize::try_from(*id) {
                return Some(id);
            }
        }
    }
    None
}

/// `l' = [b, y1, b, y2, ..., yU, b]`, length `2U + 1`.
fn extended_labels(ids: &[usize], blank: usize) -> Vec<usize> {
    let mut ext = Vec::with_capacity(2 * ids.len() + 1);
    ext.push(blank);
    for &id in ids {
        ext.push(id);
        ext.push(blank);
    }
    ext
}

/// Shared precondition check for the two CTC passes.
///
/// Returns `None` — never a score — when the request is unanswerable: empty
/// input, a label outside the vocab, or fewer frames than the target needs.
/// The frame floor is `U + repeats`, not `2U + 1`: only *repeated* labels
/// require a separating blank, so `"CAT"` fits in three frames.
fn ctc_preconditions(
    logp: &[f32],
    frames: usize,
    vocab: usize,
    ids: &[usize],
    blank: usize,
) -> Option<()> {
    if frames == 0 || vocab == 0 || ids.is_empty() || blank >= vocab {
        return None;
    }
    let needed = frames.checked_mul(vocab)?;
    if logp.len() < needed {
        return None;
    }
    if ids.iter().any(|&id| id >= vocab || id == blank) {
        return None;
    }
    let repeats = ids.windows(2).filter(|w| w[0] == w[1]).count();
    if frames < ids.len() + repeats {
        return None;
    }
    Some(())
}

/// Total log-probability of every CTC path that spells `ids`.
///
/// The standard forward recursion in the log domain:
/// `a(t,s) = logp[t][l'[s]] + logaddexp(a(t-1,s), a(t-1,s-1),
///           [a(t-1,s-2) if l'[s] != blank and l'[s] != l'[s-2]])`,
/// finishing with `logaddexp(a(T-1,S-1), a(T-1,S-2))`. Accumulated in `f64`
/// and held in two rolling rows, so cost is `O(T*S)` time and `O(S)` memory.
pub fn ctc_forward_logprob(
    logp: &[f32],
    frames: usize,
    vocab: usize,
    ids: &[usize],
    blank: usize,
) -> Option<f32> {
    ctc_preconditions(logp, frames, vocab, ids, blank)?;
    let ext = extended_labels(ids, blank);
    let s_len = ext.len();

    let mut prev = vec![f64::NEG_INFINITY; s_len];
    prev[0] = logp[ext[0]] as f64;
    if s_len > 1 {
        prev[1] = logp[ext[1]] as f64;
    }
    let mut cur = vec![f64::NEG_INFINITY; s_len];

    for t in 1..frames {
        let row = &logp[t * vocab..t * vocab + vocab];
        for s in 0..s_len {
            let mut acc = prev[s];
            if s >= 1 {
                acc = log_add_exp(acc, prev[s - 1]);
            }
            if s >= 2 && ext[s] != blank && ext[s] != ext[s - 2] {
                acc = log_add_exp(acc, prev[s - 2]);
            }
            cur[s] = if acc == f64::NEG_INFINITY {
                f64::NEG_INFINITY
            } else {
                acc + row[ext[s]] as f64
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }

    let tail = if s_len >= 2 {
        log_add_exp(prev[s_len - 1], prev[s_len - 2])
    } else {
        prev[s_len - 1]
    };
    // A non-finite total means no path exists after all (zero-probability
    // labels); that is "cannot score", not "scored zero".
    if tail.is_finite() {
        Some(tail as f32)
    } else {
        None
    }
}

/// Viterbi forced alignment: the single most likely path spelling `ids`.
///
/// Same lattice as [`ctc_forward_logprob`] with `max` in place of
/// `logaddexp`, plus a `T x S` backpointer plane (one byte per cell, holding
/// the 0/1/2 step taken) so the path can be recovered. That plane is the only
/// large allocation and is why alignment is a separate entry point from
/// scoring.
pub fn ctc_forced_align(
    logp: &[f32],
    frames: usize,
    vocab: usize,
    ids: &[usize],
    blank: usize,
) -> Option<Vec<Span>> {
    ctc_preconditions(logp, frames, vocab, ids, blank)?;
    let ext = extended_labels(ids, blank);
    let s_len = ext.len();

    let mut prev = vec![f64::NEG_INFINITY; s_len];
    prev[0] = logp[ext[0]] as f64;
    if s_len > 1 {
        prev[1] = logp[ext[1]] as f64;
    }
    let mut cur = vec![f64::NEG_INFINITY; s_len];
    let mut back = vec![0u8; frames.checked_mul(s_len)?];

    for t in 1..frames {
        let row = &logp[t * vocab..t * vocab + vocab];
        for s in 0..s_len {
            let mut best = prev[s];
            let mut step = 0u8;
            if s >= 1 && prev[s - 1] > best {
                best = prev[s - 1];
                step = 1;
            }
            if s >= 2 && ext[s] != blank && ext[s] != ext[s - 2] && prev[s - 2] > best {
                best = prev[s - 2];
                step = 2;
            }
            back[t * s_len + s] = step;
            cur[s] = if best == f64::NEG_INFINITY {
                f64::NEG_INFINITY
            } else {
                best + row[ext[s]] as f64
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }

    let mut s = if s_len >= 2 && prev[s_len - 2] > prev[s_len - 1] {
        s_len - 2
    } else {
        s_len - 1
    };
    if !prev[s].is_finite() {
        return None;
    }

    let mut path = vec![0usize; frames];
    for t in (0..frames).rev() {
        path[t] = s;
        if t > 0 {
            // `step` is bounded by `s` at the point it was written, so this
            // cannot underflow.
            s -= back[t * s_len + s] as usize;
        }
    }

    let mut spans: Vec<Span> = Vec::new();
    for (t, &idx) in path.iter().enumerate() {
        match spans.last_mut() {
            Some(last) if last.ext_index == idx => last.end = t + 1,
            _ => spans.push(Span {
                ext_index: idx,
                label: ext[idx],
                start: t,
                end: t + 1,
            }),
        }
    }
    Some(spans)
}

/// Map a mean log-posterior ratio onto 0..=100 via a `(gop, percentile)`
/// table — [`GOP_PERCENTILE`] for the shipped grapheme model.
///
/// Monotone and clamped; `gop == 0` (the model's own best guess) is 100,
/// and anything below the first knot is 0. Positive input cannot occur but
/// is clamped rather than trusted. Non-finite input is 0, as before.
///
/// A table belongs to one acoustic model and one GOP formula: a phone-level
/// model needs its own, measured the same way. `table` must be non-empty
/// with strictly increasing GOP knots; an empty one scores everything 0.
pub fn gop_to_score_with(table: &[(f32, u8)], gop: f32) -> u8 {
    if !gop.is_finite() {
        return 0;
    }
    let (Some(&first), Some(&last)) = (table.first(), table.last()) else {
        return 0;
    };
    if gop <= first.0 {
        return first.1;
    }
    if gop >= last.0 {
        return last.1;
    }
    for pair in table.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        if gop <= x1 {
            let t = (gop - x0) / (x1 - x0);
            let v = y0 as f32 + (y1 as f32 - y0 as f32) * t;
            return v.round().clamp(0.0, 100.0) as u8;
        }
    }
    last.1
}

fn verdict_for(score: u8) -> &'static str {
    if score >= FLAG_BELOW {
        "good"
    } else {
        "unclear"
    }
}

/// Score a spoken utterance against the phrase it was supposed to be.
///
/// Forced-aligns the target onto the frame posteriors, then for each word
/// averages `logp[t][aligned label] - max_v logp[t][v]` over the frames that
/// word owns. That difference is zero when the model would have chosen the
/// target label anyway and grows negative as the audio drifts away from it,
/// which is what makes the measure independent of speaking rate, mic gain and
/// the model's overall confidence.
pub fn score_against_target(
    out: &AsrOutput,
    target: &str,
    v: &Vocab,
) -> Result<PronScore, ScoreError> {
    let labels = text_to_labels(target, v)?;
    let blank = usize::try_from(v.blank).map_err(|_| ScoreError::BadVocab)?;
    score_labels(out, &labels, blank, &GOP_PERCENTILE).map(|(pron, _)| pron)
}

/// One target label's share of a forced alignment.
///
/// For a phone model this is one phone; for the grapheme model, one letter
/// (or a word delimiter).
// Read only by the phone-GOP corpus dump: the Stage 6A measurement was
// NO-GO, so the app's A7 consumer (phone-level scoring) did not land.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelScore {
    /// Token id of the label.
    pub label: usize,
    /// Index into [`TargetLabels::words`] of the word this label spells,
    /// or `None` for a label between words (the grapheme delimiter).
    pub word_index: Option<usize>,
    /// Frames the alignment gave this label; never 0.
    pub frames: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    /// Mean of `logp[t][label] - max_v logp[t][v]` over those frames.
    pub gop: f32,
}

/// Score any label sequence against any percentile table.
///
/// The engine behind [`score_against_target`]: forced alignment, the
/// per-frame GOP, word means over each word's frames, and the table lookup.
/// It does not care what the labels are — letters with `|` between words,
/// or phones with no delimiter at all — because `labels.words` carries the
/// word structure. Returns the per-label detail alongside the word scores.
pub fn score_labels(
    out: &AsrOutput,
    labels: &TargetLabels,
    blank: usize,
    table: &[(f32, u8)],
) -> Result<(PronScore, Vec<LabelScore>), ScoreError> {
    if blank >= out.vocab {
        return Err(ScoreError::BadVocab);
    }
    if labels.words.is_empty() {
        return Err(ScoreError::EmptyTarget);
    }

    let spans = ctc_forced_align(&out.logp, out.frames, out.vocab, &labels.ids, blank)
        .ok_or(ScoreError::NotAlignable)?;
    let target_logprob = ctc_forward_logprob(&out.logp, out.frames, out.vocab, &labels.ids, blank)
        .ok_or(ScoreError::NotAlignable)?;

    // Per-frame best label: the ceiling each aligned frame is measured
    // against, and summed, the model's own unconstrained path.
    let mut frame_max = vec![f32::NEG_INFINITY; out.frames];
    let mut free_logprob = 0.0f64;
    for (t, slot) in frame_max.iter_mut().enumerate() {
        let row = &out.logp[t * out.vocab..t * out.vocab + out.vocab];
        let m = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        *slot = m;
        free_logprob += m as f64;
    }

    let stride = if out.frame_stride_ms.is_finite() && out.frame_stride_ms > 0.0 {
        out.frame_stride_ms
    } else {
        0.0
    };
    let to_ms = |frame: usize| (frame as f32 * stride).round() as i64;

    // Which word owns label position k, if any.
    let mut owner: Vec<Option<usize>> = vec![None; labels.ids.len()];
    for (w, (_, range)) in labels.words.iter().enumerate() {
        for slot in owner.iter_mut().take(range.end).skip(range.start) {
            *slot = Some(w);
        }
    }

    // One entry per non-blank span. Labels sit at the odd extended indices
    // `2k + 1`, and the Viterbi path visits each of them in one contiguous
    // run, so this is one entry per target label, in order.
    let mut label_scores: Vec<LabelScore> = Vec::with_capacity(labels.ids.len());
    // Per word: summed frame GOP and frame count, then first/last frame.
    let mut word_sum = vec![(0.0f64, 0usize, usize::MAX, 0usize); labels.words.len()];
    for span in spans.iter().filter(|s| s.label != blank) {
        let word_index = owner.get(span.ext_index / 2).copied().flatten();
        let frames = span.end - span.start;
        let mut sum = 0.0f64;
        for (t, &best) in frame_max.iter().enumerate().take(span.end).skip(span.start) {
            let d = (out.logp[t * out.vocab + span.label] - best) as f64;
            sum += d;
            // Accumulated frame by frame, in the same order as the word
            // loop this replaced, so the word GOP is bit-for-bit unchanged.
            if let Some(w) = word_index {
                word_sum[w].0 += d;
            }
        }
        if let Some(w) = word_index {
            let acc = &mut word_sum[w];
            acc.1 += frames;
            acc.2 = acc.2.min(span.start);
            acc.3 = acc.3.max(span.end);
        }
        label_scores.push(LabelScore {
            label: span.label,
            word_index,
            frames,
            start_ms: to_ms(span.start),
            end_ms: to_ms(span.end),
            gop: (sum / frames.max(1) as f64) as f32,
        });
    }

    let mut words = Vec::with_capacity(labels.words.len());
    for ((word, _), &(sum, count, start, end)) in labels.words.iter().zip(&word_sum) {
        // Every non-blank position is visited by any valid path, so this is
        // unreachable — but a fabricated zero would be worse than refusing.
        if count == 0 {
            return Err(ScoreError::NotAlignable);
        }
        let gop = (sum / count as f64) as f32;
        let score = gop_to_score_with(table, gop);
        words.push(WordScore {
            word: word.clone(),
            start_ms: to_ms(start),
            end_ms: to_ms(end),
            gop,
            score,
            verdict: verdict_for(score).to_string(),
        });
    }

    let overall = (words.iter().map(|w| w.score as u32).sum::<u32>() as f32 / words.len() as f32)
        .round()
        .clamp(0.0, 100.0) as u8;
    let normalized_conf = if out.frames == 0 {
        0.0
    } else {
        (((target_logprob as f64 - free_logprob) / out.frames as f64).exp() as f32).clamp(0.0, 1.0)
    };

    Ok((
        PronScore {
            overall,
            words,
            target_logprob,
            free_logprob: free_logprob as f32,
            normalized_conf,
        },
        label_scores,
    ))
}

#[cfg(test)]
mod corpus {
    //! Calibration data for [`GOP_PERCENTILE`], from human ratings.
    //!
    //! `#[ignore]`d and needs two things off-repo: the model files, and
    //! speechocean762 (OpenSLR-101, CC BY 4.0 — 5,000 read sentences by
    //! non-native speakers, every word scored 0-10 by five experts):
    //!
    //!   curl -LO https://www.openslr.org/resources/101/speechocean762.tar.gz
    //!   tar xzf speechocean762.tar.gz
    //!   OLP_CORPUS_DIR=$PWD/speechocean762 OLP_MODELS_DIR=$PWD/models \
    //!     cargo test --release --manifest-path src-tauri/Cargo.toml \
    //!     --lib corpus_dump_gop -- --ignored --nocapture
    //!   python3 scripts/calibrate-gop.py $OLP_CORPUS_DIR/gop_dump.tsv
    //!
    //! It writes one row per scored word: the raw GOP this module computes,
    //! the score the current mapping gives it, and the human accuracy. The
    //! fit itself happens in `scripts/calibrate-gop.py`, outside the build.
    //! Only the ASR model is loaded — no TTS, so espeak's global state is
    //! never touched.
    //!
    //! Any other corpus goes through a manifest instead (one row per
    //! utterance, built by `scripts/manifest-*.py`; columns in
    //! `scripts/olp_manifest.py`), which `corpus_dump_gop_manifest` dumps
    //! with the same columns plus `corpus l1 speaker label_kind`:
    //!
    //!   OLP_MANIFEST=~/corpora/cmu_arctic/manifest.tsv OLP_MODELS_DIR=$PWD/models \
    //!     cargo test --release --manifest-path src-tauri/Cargo.toml \
    //!     --lib corpus_dump_gop_manifest -- --ignored --nocapture
    //!   python3 scripts/calibrate-gop.py $OLP_CORPUS_DIR/gop_dump.tsv \
    //!     --eval-on ~/corpora/cmu_arctic/gop_dump.tsv --by l1
    //!
    //! The dump lands next to the manifest as `gop_dump.tsv` unless
    //! `OLP_DUMP_OUT` names another path. `corpus_dump_gop` is kept as it
    //! is so the shipped table stays reproducible from the same command.

    use super::*;
    use std::collections::HashMap;
    use std::io::Write;
    use std::path::Path;

    /// 16-bit PCM mono WAV to samples in [-1, 1], plus its sample rate.
    /// Walks the RIFF chunks rather than assuming a 44-byte header.
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

    /// `key<TAB>value` lines, the Kaldi layout the corpus uses.
    fn kaldi_map(path: &Path) -> HashMap<String, String> {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .lines()
            .filter_map(|l| l.split_once(char::is_whitespace))
            .map(|(k, v)| (k.to_string(), v.trim().to_string()))
            .collect()
    }

    #[test]
    #[ignore = "corpus"]
    fn corpus_dump_gop() {
        let root = std::path::PathBuf::from(
            std::env::var_os("OLP_CORPUS_DIR")
                .expect("set OLP_CORPUS_DIR to an extracted speechocean762; see module doc"),
        );
        if std::env::var_os("OLP_MODELS_DIR").is_none() {
            let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../models");
            std::env::set_var("OLP_MODELS_DIR", models);
        }
        let model = crate::asr::find_model().expect("no ASR model; set OLP_MODELS_DIR");
        let asr = crate::asr::AsrEngine::load(&model).expect("load ASR");
        let vocab = crate::asr::load_vocab().expect("vocab");

        let scores: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("resource/scores.json")).expect("scores.json"),
        )
        .expect("scores.json parses");

        let out_path = root.join("gop_dump.tsv");
        let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path).unwrap());
        writeln!(
            out,
            "split\tutt\tage\tword_index\tword\thuman\tgop\tscore_now\tstart_ms\tend_ms"
        )
        .unwrap();

        let (mut utts, mut words, mut refused, mut mismatched) = (0usize, 0usize, 0usize, 0usize);
        for split in ["train", "test"] {
            let dir = root.join(split);
            let wavs = kaldi_map(&dir.join("wav.scp"));
            let utt2spk = kaldi_map(&dir.join("utt2spk"));
            let spk2age = kaldi_map(&dir.join("spk2age"));
            let mut ids: Vec<&String> = wavs.keys().collect();
            ids.sort();
            for utt in ids {
                let entry = &scores[utt.as_str()];
                let human: Vec<(String, i64)> = entry["words"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{utt}: no words"))
                    .iter()
                    .map(|w| {
                        (
                            w["text"].as_str().unwrap_or_default().to_string(),
                            w["accuracy"].as_i64().unwrap_or(-1),
                        )
                    })
                    .collect();
                // Built from the per-word entries, which carry no punctuation,
                // so word i of the score lines up with human score i.
                let target = human
                    .iter()
                    .map(|(w, _)| w.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                let (pcm, rate) = read_wav(&root.join(&wavs[utt]));
                assert_eq!(rate, crate::asr::ASR_SAMPLE_RATE, "{utt}: resample first");

                let asr_out = asr.transcribe_pcm_detailed(&pcm).expect("inference");
                utts += 1;
                let pron = match score_against_target(&asr_out, &target, &vocab) {
                    Ok(p) => p,
                    Err(_) => {
                        refused += 1;
                        continue;
                    }
                };
                if pron.words.len() != human.len() {
                    mismatched += 1;
                    continue;
                }
                let age = utt2spk
                    .get(utt)
                    .and_then(|spk| spk2age.get(spk))
                    .map(String::as_str)
                    .unwrap_or("");
                for (i, (w, (text, acc))) in pron.words.iter().zip(&human).enumerate() {
                    writeln!(
                        out,
                        "{split}\t{utt}\t{age}\t{i}\t{text}\t{acc}\t{:.6}\t{}\t{}\t{}",
                        w.gop, w.score, w.start_ms, w.end_ms
                    )
                    .unwrap();
                    words += 1;
                }
                if utts % 250 == 0 {
                    eprintln!("{utts} utterances, {words} words");
                }
            }
        }
        out.flush().unwrap();
        eprintln!(
            "done: {utts} utterances, {words} words written, {refused} refused by the \
             scorer, {mismatched} with a word-count mismatch -> {}",
            out_path.display()
        );
        assert!(words > 0);
    }

    #[test]
    #[ignore = "corpus"]
    fn corpus_dump_gop_manifest() {
        let manifest = std::path::PathBuf::from(
            std::env::var_os("OLP_MANIFEST")
                .expect("set OLP_MANIFEST to a scripts/manifest-*.py TSV; see module doc"),
        );
        // `wav` paths are relative to the manifest (an absolute one wins in `join`).
        let base = manifest.parent().unwrap_or(Path::new(".")).to_path_buf();
        if std::env::var_os("OLP_MODELS_DIR").is_none() {
            let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../models");
            std::env::set_var("OLP_MODELS_DIR", models);
        }
        let model = crate::asr::find_model().expect("no ASR model; set OLP_MODELS_DIR");
        let asr = crate::asr::AsrEngine::load(&model).expect("load ASR");
        let vocab = crate::asr::load_vocab().expect("vocab");

        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().expect("empty manifest").split('\t').collect();
        let col = |name: &str| header.iter().position(|h| *h == name);
        let need =
            |name: &str| col(name).unwrap_or_else(|| panic!("manifest has no {name} column"));
        let [corpus, l1, speaker, utt, wav, target, labels, kind] = [
            "corpus",
            "l1",
            "speaker",
            "utt",
            "wav",
            "target",
            "labels",
            "label_kind",
        ]
        .map(need);
        let (split, age) = (col("split"), col("age"));

        let out_path = std::env::var_os("OLP_DUMP_OUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| base.join("gop_dump.tsv"));
        let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path).unwrap());
        writeln!(
            out,
            "split\tutt\tage\tword_index\tword\thuman\tgop\tscore_now\tstart_ms\tend_ms\
             \tcorpus\tl1\tspeaker\tlabel_kind"
        )
        .unwrap();

        let (mut utts, mut words, mut refused, mut mismatched) = (0usize, 0usize, 0usize, 0usize);
        for (n, line) in lines.enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(
                f.len(),
                header.len(),
                "manifest line {}: field count",
                n + 2
            );
            let human: Vec<i64> = f[labels]
                .split_whitespace()
                .map(|x| {
                    x.parse()
                        .unwrap_or_else(|_| panic!("line {}: label {x}", n + 2))
                })
                .collect();
            let path = base.join(f[wav]);
            let (pcm, rate) = read_wav(&path);
            assert_eq!(
                rate,
                crate::asr::ASR_SAMPLE_RATE,
                "{}: resample to 16 kHz first",
                path.display()
            );

            let asr_out = asr.transcribe_pcm_detailed(&pcm).expect("inference");
            utts += 1;
            let pron = match score_against_target(&asr_out, f[target], &vocab) {
                Ok(p) => p,
                Err(_) => {
                    refused += 1;
                    continue;
                }
            };
            // The manifest builders tokenize the way `tokenize` does, so a
            // mismatch means a word the scorer could not spell was dropped.
            if pron.words.len() != human.len() {
                mismatched += 1;
                continue;
            }
            let split = split.map_or("eval", |c| f[c]);
            let age = age.map_or("", |c| f[c]);
            for (i, (w, h)) in pron.words.iter().zip(&human).enumerate() {
                writeln!(
                    out,
                    "{split}\t{}\t{age}\t{i}\t{}\t{h}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    f[utt],
                    w.word,
                    w.gop,
                    w.score,
                    w.start_ms,
                    w.end_ms,
                    f[corpus],
                    f[l1],
                    f[speaker],
                    f[kind]
                )
                .unwrap();
                words += 1;
            }
            if utts % 250 == 0 {
                eprintln!("{utts} utterances, {words} words");
            }
        }
        out.flush().unwrap();
        eprintln!(
            "done: {utts} utterances, {words} words written, {refused} refused by the \
             scorer, {mismatched} with a word-count mismatch -> {}",
            out_path.display()
        );
        assert!(words > 0);
    }
    /// Corpus ARPAbet (with stress digits) to the espeak IPA the phoneme
    /// model was trained on. Evaluation only: the "oracle" G2P, which says
    /// how much of the phone score is lost to espeak disagreeing with the
    /// corpus's canonical phones. Unstressed IY/ER/AH take their reduced
    /// espeak forms.
    fn arpa_to_ipa(arpa: &str) -> Option<&'static str> {
        let reduced = arpa.ends_with('0');
        let base = arpa.trim_end_matches(|c: char| c.is_ascii_digit());
        Some(match base {
            "AA" => "ɑː",
            "AE" => "æ",
            "AH" if reduced => "ə",
            "AH" => "ʌ",
            "AO" => "ɔː",
            "AW" => "aʊ",
            "AY" => "aɪ",
            "EH" => "ɛ",
            "ER" if reduced => "ɚ",
            "ER" => "ɜː",
            "EY" => "eɪ",
            "IH" => "ɪ",
            "IY" if reduced => "i",
            "IY" => "iː",
            "OW" => "oʊ",
            "OY" => "ɔɪ",
            "UH" => "ʊ",
            "UW" => "uː",
            "B" => "b",
            "CH" => "tʃ",
            "D" => "d",
            "DH" => "ð",
            "F" => "f",
            "G" => "ɡ",
            "HH" => "h",
            "JH" => "dʒ",
            "K" => "k",
            "L" => "l",
            "M" => "m",
            "N" => "n",
            "NG" => "ŋ",
            "P" => "p",
            "R" => "ɹ",
            "S" => "s",
            "SH" => "ʃ",
            "T" => "t",
            "TH" => "θ",
            "V" => "v",
            "W" => "w",
            "Y" => "j",
            "Z" => "z",
            "ZH" => "ʒ",
            _ => return None,
        })
    }

    #[test]
    fn arpa_to_ipa_covers_the_corpus_inventory() {
        // Every phone speechocean762's canonical transcriptions use.
        for p in [
            "AA0", "AA1", "AE1", "AH0", "AH1", "AO2", "AW1", "AY0", "B", "CH", "D", "DH", "EH1",
            "ER0", "ER1", "EY2", "F", "G", "HH", "IH", "IH0", "IY0", "IY1", "JH", "K", "L", "M",
            "N", "NG", "OW1", "OY1", "P", "R", "S", "SH", "T", "TH", "UH", "UH0", "UW1", "V", "W",
            "Y", "Z", "ZH",
        ] {
            let ipa = arpa_to_ipa(p).unwrap_or_else(|| panic!("{p} unmapped"));
            assert!(
                crate::phonemize::MODEL_EN_PHONES.contains(&ipa),
                "{p} -> {ipa} is not a model phone"
            );
        }
        assert_eq!(arpa_to_ipa("AH0"), Some("ə"));
        assert_eq!(arpa_to_ipa("AH1"), Some("ʌ"));
        assert_eq!(arpa_to_ipa("XX"), None);
    }

    /// A `/proc/self/status` size field (`VmHWM:` peak, `VmRSS:` current)
    /// in MiB. Linux only; `None` elsewhere.
    pub(super) fn proc_status_mib(field: &str) -> Option<f64> {
        let text = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = text.lines().find(|l| l.starts_with(field))?;
        let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb / 1024.0)
    }

    /// The optional phoneme model and its vocab, from `OLP_PHONEME_MODEL`
    /// (and `OLP_PHONEME_VOCAB`, default `vocab.json` beside the model),
    /// plus a tag for the dump's `model` column (`OLP_PHONEME_TAG`, default
    /// the file stem).
    pub(super) fn phoneme_engine() -> (crate::asr::AsrEngine, Vocab, String) {
        let model = std::path::PathBuf::from(std::env::var_os("OLP_PHONEME_MODEL").expect(
            "set OLP_PHONEME_MODEL to an onnx-community wav2vec2-lv-60-espeak-cv-ft ONNX file",
        ));
        let vocab_path = std::env::var_os("OLP_PHONEME_VOCAB")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| model.with_file_name("vocab.json"));
        let vocab = Vocab::parse(
            &std::fs::read_to_string(&vocab_path)
                .unwrap_or_else(|e| panic!("{}: {e}", vocab_path.display())),
        )
        .expect("phone vocab parses");
        let tag = std::env::var("OLP_PHONEME_TAG").unwrap_or_else(|_| {
            model
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let engine = crate::asr::AsrEngine::load(&model)
            .unwrap_or_else(|e| panic!("{}: load failed: {e}", model.display()));
        (engine, vocab, tag)
    }

    fn joined(v: &serde_json::Value, f: impl Fn(&serde_json::Value) -> Option<String>) -> String {
        v.as_array()
            .map(|a| a.iter().filter_map(&f).collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    }

    /// Phone-level GOP for speechocean762, for `scripts/calibrate-phone-gop.py`.
    ///
    ///   OLP_CORPUS_DIR=~/corpora/speechocean762 \
    ///   OLP_PHONEME_MODEL=~/models-phoneme/model_quantized.onnx \
    ///     cargo test --release --manifest-path src-tauri/Cargo.toml \
    ///     --lib corpus_dump_phone_gop -- --ignored --nocapture
    ///
    /// One row per target phone, twice per utterance: once with espeak's
    /// phones (`g2p = espeak`, the path the app would take) and once with the
    /// corpus's own canonical phones mapped to IPA (`g2p = oracle`). The
    /// expert per-phone scores are written per word (`canon_arpa`,
    /// `canon_acc`); aligning espeak's phones to them is the script's job,
    /// so that choice can change without re-running inference. `word_gop`
    /// is the word's GOP exactly as `score_labels` computes it (mean over
    /// the word's frames). A target that could not be scored gets one row
    /// whose `status` says why (`phonemize`, `word_count`, `unknown_phone`,
    /// `not_alignable`, …). `OLP_SPLITS` (default `train,test`) and
    /// `OLP_DUMP_OUT` (default `phone_gop_dump.tsv` in the corpus dir) let
    /// one split, or one model variant, run on its own; `OLP_MAX_UTTS`
    /// caps utterances per split for a quick check.
    #[test]
    #[ignore = "corpus"]
    fn corpus_dump_phone_gop() {
        let root = std::path::PathBuf::from(
            std::env::var_os("OLP_CORPUS_DIR")
                .expect("set OLP_CORPUS_DIR to an extracted speechocean762; see module doc"),
        );
        crate::phonemize::testing::use_vendored_espeak_data();
        let (engine, vocab, tag) = phoneme_engine();
        let blank = usize::try_from(vocab.blank).expect("blank id");
        let splits = std::env::var("OLP_SPLITS").unwrap_or_else(|_| "train,test".to_string());
        let max_utts: usize = std::env::var("OLP_MAX_UTTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(usize::MAX);

        let scores: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("resource/scores.json")).expect("scores.json"),
        )
        .expect("scores.json parses");

        let out_path = std::env::var_os("OLP_DUMP_OUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join("phone_gop_dump.tsv"));
        let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path).unwrap());
        writeln!(
            out,
            "split\tutt\tage\tspeaker\tword_index\tword\tphone_index\tphone\tg2p\tgop\tframes\
             \tstart_ms\tend_ms\tword_gop\thuman_word\tcanon_arpa\tcanon_acc\tmodel\tstatus"
        )
        .unwrap();

        let (mut utts, mut rows, mut refused) = (0usize, 0usize, 0usize);
        let (mut audio_s, mut infer_s) = (0.0f64, 0.0f64);
        for split in splits.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let dir = root.join(split);
            let wavs = kaldi_map(&dir.join("wav.scp"));
            let utt2spk = kaldi_map(&dir.join("utt2spk"));
            let spk2age = kaldi_map(&dir.join("spk2age"));
            let mut ids: Vec<&String> = wavs.keys().collect();
            ids.sort();
            ids.truncate(max_utts);
            for utt in ids {
                let human = scores[utt.as_str()]["words"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{utt}: no words"));
                let target = human
                    .iter()
                    .map(|w| w["text"].as_str().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(" ");
                let speaker = utt2spk.get(utt).map(String::as_str).unwrap_or("");
                let age = spk2age.get(speaker).map(String::as_str).unwrap_or("");
                let (pcm, rate) = read_wav(&root.join(&wavs[utt]));
                assert_eq!(rate, crate::asr::ASR_SAMPLE_RATE, "{utt}: resample first");

                let t0 = std::time::Instant::now();
                let post = engine.posteriors(&pcm).expect("inference");
                infer_s += t0.elapsed().as_secs_f64();
                audio_s += pcm.len() as f64 / crate::asr::ASR_SAMPLE_RATE as f64;
                utts += 1;

                let oracle: Option<Vec<crate::phonemize::WordPhones>> = human
                    .iter()
                    .map(|w| {
                        let ipa = w["phones"]
                            .as_array()?
                            .iter()
                            .map(|p| arpa_to_ipa(p.as_str()?).map(str::to_string))
                            .collect::<Option<Vec<_>>>()?;
                        Some((w["text"].as_str()?.to_string(), ipa))
                    })
                    .collect();
                let espeak = crate::phonemize::phonemize(&target);

                for g2p in ["espeak", "oracle"] {
                    let words = match g2p {
                        "espeak" => espeak.clone().map_err(|_| "phonemize"),
                        _ => oracle.clone().ok_or("oracle_unmapped"),
                    };
                    let scored = words.and_then(|w| {
                        if w.len() != human.len() {
                            return Err("word_count");
                        }
                        let labels = phones_to_labels(&w, &vocab).map_err(|e| match e {
                            ScoreError::UnknownChars(_) => "unknown_phone",
                            _ => "empty_target",
                        })?;
                        let s = score_labels(&post, &labels, blank, &GOP_PERCENTILE)
                            .map_err(|_| "not_alignable")?;
                        Ok((labels, s))
                    });
                    let (labels, (pron, per)) = match scored {
                        Ok(v) => v,
                        Err(why) => {
                            refused += 1;
                            writeln!(
                                out,
                                "{split}\t{utt}\t{age}\t{speaker}\t-1\t\t-1\t\t{g2p}\t\t\t\t\t\
                                 \t\t\t\t{tag}\t{why}"
                            )
                            .unwrap();
                            continue;
                        }
                    };
                    let mut phone_in_word = vec![0usize; labels.words.len()];
                    for l in &per {
                        let Some(w) = l.word_index else { continue };
                        let k = phone_in_word[w];
                        phone_in_word[w] += 1;
                        let hw = &human[w];
                        let arpa = joined(&hw["phones"], |p| p.as_str().map(str::to_string));
                        let acc = joined(&hw["phones-accuracy"], |p| {
                            p.as_f64().map(|x| format!("{x}"))
                        });
                        let phone = vocab
                            .id_to_token
                            .get(&(l.label as i64))
                            .map(String::as_str)
                            .unwrap_or("?");
                        writeln!(
                            out,
                            "{split}\t{utt}\t{age}\t{speaker}\t{w}\t{}\t{k}\t{phone}\t{g2p}\t{:.6}\
                             \t{}\t{}\t{}\t{:.6}\t{}\t{arpa}\t{acc}\t{tag}\tok",
                            labels.words[w].0,
                            l.gop,
                            l.frames,
                            l.start_ms,
                            l.end_ms,
                            pron.words[w].gop,
                            hw["accuracy"].as_i64().unwrap_or(-1),
                        )
                        .unwrap();
                        rows += 1;
                    }
                }
                if utts % 250 == 0 {
                    eprintln!(
                        "{utts} utterances, {rows} phone rows, {refused} refusals, RTF {:.3}",
                        infer_s / audio_s
                    );
                }
            }
        }
        out.flush().unwrap();
        eprintln!(
            "done: {utts} utterances, {rows} phone rows, {refused} refused (of {} passes), \
             RTF {:.3} over {audio_s:.0} s of audio, peak RSS {:.0} MiB -> {}",
            2 * utts,
            infer_s / audio_s,
            proc_status_mib("VmHWM:").unwrap_or(f64::NAN),
            out_path.display()
        );
        assert!(rows > 0);
    }
}

#[cfg(test)]
mod real_models {
    //! End-to-end calibration against real weights.
    //!
    //! `#[ignore]`d: these are the only tests that need the ~456 MB of model
    //! files on disk. Run them after fetching models:
    //!
    //!   ./scripts/download-models.sh
    //!   OLP_MODELS_DIR=$PWD/models cargo test --manifest-path src-tauri/Cargo.toml \
    //!       --lib real_models -- --ignored --nocapture
    //!
    //! They exist because every other pronunciation test is synthetic. The
    //! CTC maths is proven by brute-force equivalence on a 3-frame toy
    //! problem, which says the implementation matches the definition but
    //! nothing about what the numbers mean on real audio.
    //!
    //! **What these tests do and do not settle.** They prove the pipeline
    //! works end to end on real weights, that word alignments are sane, and
    //! that the score discriminates — the right target scores 100 and a
    //! wrong one scores 0 on the same audio. They do **not** calibrate the
    //! curve for human speech: the on-device voice is unnaturally clean, so
    //! every word comes back at exactly 100 with a GOP of ~0, and a
    //! saturated metric cannot tell you where the interesting middle of the
    //! range sits. `the_score_degrades_gradually_as_audio_gets_worse`
    //! probes that middle by degrading the audio, which is the closest this
    //! repo can get without a microphone and real speakers. For real
    //! learners, see the `corpus` module and `scripts/calibrate-gop.py`:
    //! against speechocean762's expert ratings, per-word GOP separates
    //! mispronounced from correct words with AUC ~0.80, but at every cutoff
    //! only ~20-28% of flagged words are actually mispronounced.

    use super::*;

    /// Absolute path to the repo's `models/` dir, whatever the test CWD is.
    fn models_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri has a parent")
            .join("models")
    }

    /// The loaded engines, created once and used under a lock.
    ///
    /// Not an optimisation. espeak-ng (under `piper-rs`) keeps global,
    /// non-reentrant state: three tests each building a `TtsEngine` on its
    /// own thread prints `maximum number 349 of (N_VOICES_LIST = 350 - 1)
    /// reached` and then segfaults. Production never hits this because TTS
    /// is confined to the single `neural-tts` worker thread — this lock is
    /// the test-side equivalent of that thread, and loading the 380 MB ASR
    /// model once instead of per test is just a bonus. Each synthesize also
    /// takes `phonemize::ESPEAK`, the lock the phonemizer tests share.
    struct Engines {
        tts: crate::tts::TtsEngine,
        asr: crate::asr::AsrEngine,
        vocab: std::sync::Arc<crate::asr::Vocab>,
    }

    fn engines() -> std::sync::MutexGuard<'static, Engines> {
        static ENGINES: std::sync::OnceLock<std::sync::Mutex<Engines>> = std::sync::OnceLock::new();
        ENGINES
            .get_or_init(|| {
                std::env::set_var("OLP_MODELS_DIR", models_root());
                // The shipped espeak data, not the build tree's copy, so
                // these tests fail if the vendored subset stops working.
                // Shared with the phonemizer tests, under the espeak lock.
                crate::phonemize::testing::use_vendored_espeak_data();
                let (voice, config) = crate::tts::find_voice()
                    .expect("no voice installed; run ./scripts/download-models.sh");
                let model = crate::asr::find_model()
                    .expect("no ASR model installed; run ./scripts/download-models.sh");
                std::sync::Mutex::new(Engines {
                    tts: crate::tts::TtsEngine::load(&voice, &config).expect("load voice"),
                    asr: crate::asr::AsrEngine::load(&model).expect("load ASR"),
                    vocab: crate::asr::load_vocab().expect("vocab"),
                })
            })
            // A panic in one test must not hide the others behind a
            // poisoned lock with no output.
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Linear resample to 16 kHz. Good enough for a calibration harness —
    /// in the app the frontend resamples before audio reaches ASR.
    fn to_16k(input: &[f32], rate: u32) -> Vec<f32> {
        if rate == crate::asr::ASR_SAMPLE_RATE || input.is_empty() {
            return input.to_vec();
        }
        let ratio = crate::asr::ASR_SAMPLE_RATE as f64 / rate as f64;
        let out_len = ((input.len() as f64) * ratio).round() as usize;
        (0..out_len)
            .map(|i| {
                let src = i as f64 / ratio;
                let lo = src.floor() as usize;
                let hi = (lo + 1).min(input.len() - 1);
                let frac = (src - lo as f64) as f32;
                let a = input.get(lo).copied().unwrap_or(0.0);
                let b = input.get(hi).copied().unwrap_or(0.0);
                a + (b - a) * frac
            })
            .collect()
    }

    /// Speak `text` with the installed voice, at 16 kHz mono.
    fn speak(e: &Engines, text: &str) -> Vec<f32> {
        let (pcm, rate) = e.tts.synthesize(text, false).expect("synthesize");
        to_16k(&pcm, rate)
    }

    #[test]
    #[ignore = "models"]
    fn slow_speech_is_slower_and_still_understood() {
        let e = engines();
        let text = "Could you tell me how to get to the train station please";
        let (natural, rate) = e.tts.synthesize(text, false).expect("natural");
        let (slow, slow_rate) = e.tts.synthesize(text, true).expect("slow");
        assert_eq!(rate, slow_rate);
        let ratio = slow.len() as f64 / natural.len() as f64;
        // Silences and pauses do not stretch, so the whole clip grows a little
        // less than the per-phoneme factor; anything from 1.25x to 1.6x is right.
        assert!(
            (1.25..1.6).contains(&ratio),
            "slow/natural duration ratio {ratio:.2}"
        );
        let heard = e
            .asr
            .transcribe_pcm_detailed(&to_16k(&slow, slow_rate))
            .expect("transcribe")
            .text;
        // Word-for-word equality is too strict (this recogniser hears "TRAINED"
        // for "TRAIN" at natural speed too); the sentence must be recognisable.
        assert!(
            heard.contains("TELL ME HOW TO GET TO THE") && heard.contains("STATION"),
            "slow speech should still be intelligible, heard {heard:?}"
        );
    }

    #[test]
    #[ignore = "models"]
    fn a_clearly_spoken_phrase_transcribes_and_scores_well() {
        let e = engines();
        let target = "THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG";

        let pcm = speak(&e, target);
        assert!(
            pcm.len() > crate::asr::ASR_SAMPLE_RATE as usize,
            "expected over a second of audio, got {} samples",
            pcm.len()
        );

        let out = e.asr.transcribe_pcm_detailed(&pcm).expect("transcribe");

        println!("transcript: {:?}", out.text);
        println!(
            "frames={} vocab={} stride={:.2}ms audio={:.0}ms",
            out.frames,
            out.vocab,
            out.frame_stride_ms,
            pcm.len() as f32 * 1000.0 / crate::asr::ASR_SAMPLE_RATE as f32
        );

        let score = score_against_target(&out, target, &e.vocab).expect("score");
        println!(
            "overall={} normalized_conf={:.3} target_logprob={:.1} free_logprob={:.1}",
            score.overall, score.normalized_conf, score.target_logprob, score.free_logprob
        );
        for w in &score.words {
            println!(
                "  {:>6} {:>3}  gop={:+.3}  {:>7}  [{:>5}..{:>5}]ms",
                w.word, w.score, w.gop, w.verdict, w.start_ms, w.end_ms
            );
        }

        // The recognizer should hear roughly what was said. Exact equality
        // is too brittle a bar for any acoustic model, so require that most
        // words survive the round trip.
        let said: Vec<&str> = out.text.split_whitespace().collect();
        let want: Vec<&str> = target.split_whitespace().collect();
        let matched = want.iter().filter(|w| said.contains(w)).count();
        assert!(
            matched * 2 >= want.len(),
            "transcript {:?} shares only {matched}/{} words with the target",
            out.text,
            want.len()
        );

        // Clear speech must land high, not among the flagged. If the
        // percentile table ever drifts so that a clean reading scores like a
        // struggling learner, this is what catches it.
        assert!(
            score.overall >= 70,
            "clear speech scored {} — GOP_PERCENTILE is mis-built",
            score.overall
        );
        assert!(
            score.words.iter().filter(|w| w.verdict == "good").count() * 2 >= score.words.len(),
            "most words in a clean reading should be 'good'"
        );
    }

    /// Deterministic white-ish noise, so a run is reproducible.
    fn noisy(pcm: &[f32], amount: f32) -> Vec<f32> {
        let mut state: u32 = 0x1234_5678;
        pcm.iter()
            .map(|v| {
                // xorshift32: no dependency, and the same sequence every run.
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let n = (state as f32 / u32::MAX as f32) * 2.0 - 1.0;
                (v + n * amount).clamp(-1.0, 1.0)
            })
            .collect()
    }

    #[test]
    #[ignore = "models"]
    fn the_score_degrades_gradually_as_audio_gets_worse() {
        // The saturation in the test above is the reason this one exists.
        // A metric that reads 100 on clean audio and 0 on the wrong words
        // could still be a step function, which would be useless for
        // telling a learner they are *nearly* there. Walk the audio from
        // clean to badly degraded and watch the curve.
        //
        // OBSERVED, and worth knowing before trusting a displayed number:
        // the curve is a cliff, not a ramp. A representative run gives
        //
        //     noise 0.00 -> 100      noise 0.10 -> 25
        //     noise 0.05 ->  34      noise 0.20 ->  0
        //
        // so the whole interesting middle of the range is crossed between
        // 0 and 0.05. Part of that is this test's own crudeness — additive
        // white noise wrecks recognition itself, not just articulation, and
        // by 0.05 the transcript is already garbage — but it does mean
        // the old `TAU = 0.55` curve was a cliff. It has since been replaced by
        // a percentile table measured against expert ratings (see
        // `GOP_PERCENTILE`); this test still only demands monotonicity,
        // because synthetic noise says nothing about real learners.
        let e = engines();
        let target = "THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG";
        let clean = speak(&e, target);

        let mut scores = Vec::new();
        for amount in [0.0f32, 0.05, 0.1, 0.2, 0.4] {
            let pcm = if amount == 0.0 {
                clean.clone()
            } else {
                noisy(&clean, amount)
            };
            let out = e.asr.transcribe_pcm_detailed(&pcm).expect("transcribe");
            let s = score_against_target(&out, target, &e.vocab).expect("score");
            println!(
                "noise={amount:<5} overall={:<4} conf={:.3}  transcript={:?}",
                s.overall,
                s.normalized_conf,
                out.text.trim()
            );
            scores.push(s.overall);
        }

        let first = scores.first().copied().unwrap_or(0);
        let last = scores.last().copied().unwrap_or(0);
        assert!(
            first > last,
            "adding noise did not lower the score: {scores:?}"
        );
        // Monotone within tolerance: an acoustic model is not obliged to be
        // perfectly ordered, but a metric that bounces around is not
        // measuring what it claims to.
        for pair in scores.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(b <= a + 5, "score rose with more noise: {scores:?}");
        }
    }

    #[test]
    #[ignore = "models"]
    fn a_wrong_target_scores_below_the_right_one() {
        // The score has to discriminate, not just be high. Same audio,
        // scored against what was said and against something else: if the
        // wrong target does not score clearly worse, the number is
        // measuring audio quality rather than pronunciation.
        let e = engines();
        let spoken = "THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG";
        let wrong = "PLEASE SUBMIT THE QUARTERLY BUDGET BEFORE FRIDAY MORNING";

        let pcm = speak(&e, spoken);
        let out = e.asr.transcribe_pcm_detailed(&pcm).expect("transcribe");

        let right = score_against_target(&out, spoken, &e.vocab).expect("score right");
        let other = score_against_target(&out, wrong, &e.vocab).expect("score wrong");
        println!(
            "right={} wrong={} (conf {:.3} vs {:.3})",
            right.overall, other.overall, right.normalized_conf, other.normalized_conf
        );
        assert!(
            other.overall < right.overall,
            "a wrong target scored {} against a right-target {}",
            other.overall,
            right.overall
        );
    }
    /// Greedy phone decode of posteriors, blank and repeats collapsed.
    fn greedy_phones(out: &crate::asr::AsrOutput, v: &Vocab) -> Vec<String> {
        crate::asr::ctc_collapse_argmax(&out.logp, out.frames, out.vocab, v.blank)
            .into_iter()
            .filter_map(|id| v.id_to_token.get(&id).cloned())
            .collect()
    }

    fn edit_distance(a: &[String], b: &[String]) -> usize {
        let mut prev: Vec<usize> = (0..=b.len()).collect();
        for (i, x) in a.iter().enumerate() {
            let mut cur = vec![i + 1; b.len() + 1];
            for (j, y) in b.iter().enumerate() {
                cur[j + 1] = (prev[j] + usize::from(x != y))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1);
            }
            prev = cur;
        }
        prev[b.len()]
    }

    /// E0 of the phoneme-model experiment: does this ONNX variant load and
    /// run under ort rc.12 on CPU, and what does it cost?
    ///
    ///   OLP_PHONEME_MODEL=~/models-phoneme/model_quantized.onnx \
    ///     cargo test --release --manifest-path src-tauri/Cargo.toml \
    ///     --lib phoneme_variant_loads_and_runs -- --ignored --nocapture
    ///
    /// One variant per process, because peak RSS (`VmHWM`) is per process.
    /// Prints one `E0` line: load time, peak RSS added over the loaded
    /// grapheme+TTS baseline, p50/p95 latency and RTF on 10 s of TTS audio,
    /// and the greedy phone error rate against espeak's phones for the text.
    #[test]
    #[ignore = "models"]
    fn phoneme_variant_loads_and_runs() {
        let e = engines();
        let text = "THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG. PLEASE CALL STELLA AND ASK \
                    HER TO BRING THESE THINGS WITH HER FROM THE STORE.";
        let mut pcm = speak(&e, text);
        let ten_s = 10 * crate::asr::ASR_SAMPLE_RATE as usize;
        let spoken = pcm.len();
        while pcm.len() < ten_s {
            pcm.extend_from_within(..spoken.min(ten_s - pcm.len()));
        }
        pcm.truncate(ten_s);
        // Baseline: both app models loaded and the grapheme model run once.
        e.asr.transcribe_pcm_detailed(&pcm).expect("grapheme run");
        let base_hwm = super::corpus::proc_status_mib("VmHWM:").unwrap_or(f64::NAN);

        let t0 = std::time::Instant::now();
        let (engine, vocab, tag) = super::corpus::phoneme_engine();
        let load_ms = t0.elapsed().as_millis();

        let mut times = Vec::new();
        let mut last = None;
        for _ in 0..20 {
            let t = std::time::Instant::now();
            let out = engine.posteriors(&pcm).expect("phoneme model runs");
            times.push(t.elapsed().as_secs_f64());
            last = Some(out);
        }
        let out = last.expect("ran");
        times.sort_by(f64::total_cmp);
        let p50 = times[times.len() / 2];
        let p95 = times[(times.len() * 95).div_ceil(100) - 1];
        let hwm = super::corpus::proc_status_mib("VmHWM:").unwrap_or(f64::NAN);
        assert!(out.frames > 0 && out.vocab == vocab.id_to_token.len());

        // Phone error rate on one clean reading (not repeated audio).
        let clean = speak(&e, text);
        let clean_out = engine.posteriors(&clean).expect("phoneme model runs");
        let heard = greedy_phones(&clean_out, &vocab);
        let want: Vec<String> = crate::phonemize::phonemize(text)
            .expect("phonemize")
            .into_iter()
            .flat_map(|(_, p)| p)
            .collect();
        let per = edit_distance(&heard, &want) as f64 / want.len() as f64;

        // Discrimination on the phone path: audio of "sink", target "think".
        let sink = speak(&e, "SINK");
        let sink_out = engine.posteriors(&sink).expect("runs");
        let blank = usize::try_from(vocab.blank).expect("blank");
        let lowest = |target: &str| {
            let words = crate::phonemize::phonemize(target).expect("phonemize");
            let labels = phones_to_labels(&words, &vocab).expect("labels");
            let (pron, per) =
                score_labels(&sink_out, &labels, blank, &GOP_PERCENTILE).expect("score");
            let worst = per
                .iter()
                .min_by(|a, b| a.gop.total_cmp(&b.gop))
                .map(|l| vocab.id_to_token[&(l.label as i64)].clone());
            (pron.words[0].gop, worst)
        };
        let (gop_sink, _) = lowest("SINK");
        let (gop_think, worst_think) = lowest("THINK");

        println!(
            "E0 {tag}: load {load_ms} ms, peak RSS +{:.0} MiB (baseline {base_hwm:.0}), \
             10 s audio p50 {p50:.3} s p95 {p95:.3} s, RTF {:.3}, PER {:.3} \
             ({} heard vs {} espeak), \"sink\" audio: GOP sink {gop_sink:.3} think \
             {gop_think:.3} (lowest phone {worst_think:?})",
            hwm - base_hwm,
            p50 / 10.0,
            per,
            heard.len(),
            want.len()
        );
        println!("  heard: {}", heard.join(" "));
        println!("  want:  {}", want.join(" "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped mapping.
    fn gop_to_score(gop: f32) -> u8 {
        gop_to_score_with(&GOP_PERCENTILE, gop)
    }

    fn kinds(a: &WordAlignment) -> Vec<&'static str> {
        a.ops
            .iter()
            .map(|op| match op {
                WordOp::Match { .. } => "match",
                WordOp::Sub { .. } => "sub",
                WordOp::Ins { .. } => "ins",
                WordOp::Del { .. } => "del",
            })
            .collect()
    }

    #[test]
    fn tokenize_upper_cases_and_strips_punctuation() {
        assert_eq!(tokenize("Hello, world!"), vec!["HELLO", "WORLD"]);
        assert_eq!(tokenize("  spaced   out  "), vec!["SPACED", "OUT"]);
        assert_eq!(tokenize(""), Vec::<String>::new());
        assert_eq!(tokenize("!!! ???"), Vec::<String>::new());
    }

    #[test]
    fn tokenize_keeps_internal_apostrophes_only() {
        // The ASR vocab emits DON'T as one token, so the target must too —
        // otherwise every contraction reads as an error.
        assert_eq!(tokenize("Don't stop."), vec!["DON'T", "STOP"]);
        assert_eq!(tokenize("'quoted'"), vec!["QUOTED"]);
        // Typographic apostrophes are the default in most written prompts.
        assert_eq!(tokenize("I\u{2019}m fine"), vec!["I'M", "FINE"]);
    }

    #[test]
    fn tokenize_keeps_digits() {
        assert_eq!(tokenize("Room 101B."), vec!["ROOM", "101B"]);
    }

    #[test]
    fn perfect_read_is_all_matches() {
        let a = align_words("THE CAT SAT", "The cat sat.");
        assert_eq!(kinds(&a), vec!["match", "match", "match"]);
        assert_eq!(a.matched, 3);
        assert_eq!(a.accuracy, Some(100));
    }

    #[test]
    fn one_wrong_word_is_one_substitution() {
        let a = align_words("THE HAT SAT", "THE CAT SAT");
        assert_eq!(kinds(&a), vec!["match", "sub", "match"]);
        assert_eq!(
            a.ops[1],
            WordOp::Sub {
                hyp_index: 1,
                target_index: 1,
                spoken: "HAT".to_string(),
                expected: "CAT".to_string(),
            }
        );
        assert_eq!(a.accuracy, Some(67));
    }

    #[test]
    fn a_skipped_word_is_a_deletion() {
        let a = align_words("THE SAT", "THE CAT SAT");
        assert_eq!(kinds(&a), vec!["match", "del", "match"]);
        assert_eq!(a.deleted, 1);
        assert_eq!(a.accuracy, Some(67));
    }

    #[test]
    fn an_extra_word_is_an_insertion() {
        let a = align_words("THE BIG CAT SAT", "THE CAT SAT");
        assert_eq!(kinds(&a), vec!["match", "ins", "match", "match"]);
        assert_eq!(a.inserted, 1);
        // All three target words were said, so accuracy is full; the extra
        // word is reported on its own rather than hidden in the percentage.
        assert_eq!(a.accuracy, Some(100));
    }

    #[test]
    fn indices_point_back_at_both_sides() {
        let a = align_words("A X C", "A B C");
        match &a.ops[1] {
            WordOp::Sub {
                hyp_index,
                target_index,
                ..
            } => {
                assert_eq!((*hyp_index, *target_index), (1, 1));
            }
            other => panic!("expected a substitution, got {other:?}"),
        }
        match &a.ops[2] {
            WordOp::Match {
                hyp_index,
                target_index,
                ..
            } => assert_eq!((*hyp_index, *target_index), (2, 2)),
            other => panic!("expected a match, got {other:?}"),
        }
    }

    #[test]
    fn saying_nothing_deletes_every_target_word() {
        let a = align_words("", "THE CAT SAT");
        assert_eq!(kinds(&a), vec!["del", "del", "del"]);
        assert_eq!(a.accuracy, Some(0));
    }

    #[test]
    fn no_target_has_no_accuracy() {
        // Free speaking is not scored against a reference. Reporting 0 or
        // 100 would both be inventing a number.
        let a = align_words("ANYTHING AT ALL", "");
        assert_eq!(a.accuracy, None);
        assert_eq!(a.inserted, 3);
    }

    #[test]
    fn both_empty_is_empty() {
        let a = align_words("", "");
        assert!(a.ops.is_empty());
        assert_eq!(a.accuracy, None);
    }

    #[test]
    fn completely_different_utterance_substitutes_throughout() {
        let a = align_words("ONE TWO THREE", "FOUR FIVE SIX");
        assert_eq!(kinds(&a), vec!["sub", "sub", "sub"]);
        assert_eq!(a.accuracy, Some(0));
    }

    #[test]
    fn repeated_words_do_not_collapse() {
        // CTC collapses repeated *characters*, never repeated words; if a
        // learner says "very" twice, the alignment must show it.
        let a = align_words("VERY VERY GOOD", "VERY GOOD");
        assert_eq!(a.matched, 2);
        assert_eq!(a.inserted, 1);
    }

    #[test]
    fn op_count_covers_every_word_on_both_sides() {
        for (hyp, target) in [
            ("", ""),
            ("A", ""),
            ("", "A"),
            ("A B C D", "A C"),
            ("X", "A B C"),
            ("THE QUICK BROWN FOX", "THE LAZY BROWN DOG JUMPED"),
        ] {
            let a = align_words(hyp, target);
            let h = tokenize(hyp).len();
            let t = tokenize(target).len();
            assert_eq!(
                a.matched + a.substituted + a.inserted,
                h,
                "every spoken word must be accounted for ({hyp:?} vs {target:?})"
            );
            assert_eq!(
                a.matched + a.substituted + a.deleted,
                t,
                "every target word must be accounted for ({hyp:?} vs {target:?})"
            );
        }
    }

    #[test]
    fn ops_are_ordered_and_indices_are_monotonic() {
        let a = align_words("THE QUICK BROWN FOX", "THE LAZY BROWN DOG JUMPED");
        let mut last_hyp: Option<usize> = None;
        let mut last_target: Option<usize> = None;
        for op in &a.ops {
            let (h, t) = match op {
                WordOp::Match {
                    hyp_index,
                    target_index,
                    ..
                }
                | WordOp::Sub {
                    hyp_index,
                    target_index,
                    ..
                } => (Some(*hyp_index), Some(*target_index)),
                WordOp::Ins { hyp_index, .. } => (Some(*hyp_index), None),
                WordOp::Del { target_index, .. } => (None, Some(*target_index)),
            };
            if let (Some(prev), Some(cur)) = (last_hyp, h) {
                assert!(cur > prev, "hyp indices must advance: {:?}", a.ops);
            }
            if let (Some(prev), Some(cur)) = (last_target, t) {
                assert!(cur > prev, "target indices must advance: {:?}", a.ops);
            }
            last_hyp = h.or(last_hyp);
            last_target = t.or(last_target);
        }
    }

    // -----------------------------------------------------------------
    // CTC scoring
    // -----------------------------------------------------------------

    /// Row-major log-posteriors from explicit probability rows.
    fn logp_from_probs(rows: &[&[f64]]) -> Vec<f32> {
        rows.iter()
            .flat_map(|r| r.iter().map(|p| p.ln() as f32))
            .collect()
    }

    /// One frame per label, `conf` on the intended label and the rest spread
    /// evenly over the others.
    fn peaky(labels: &[usize], vocab: usize, conf: f64) -> Vec<f32> {
        let other = (1.0 - conf) / (vocab - 1) as f64;
        let mut out = Vec::with_capacity(labels.len() * vocab);
        for &l in labels {
            for v in 0..vocab {
                out.push((if v == l { conf } else { other }).ln() as f32);
            }
        }
        out
    }

    /// The definition of CTC, computed the slow way: enumerate every
    /// frame-label path, collapse it, and sum the probability of the ones
    /// that spell the target.
    ///
    /// This is the oracle the whole acoustic score rests on. The forward
    /// recursion is an optimisation of exactly this sum, so if they ever
    /// disagree the recursion is wrong.
    fn brute_force_prob(
        logp: &[f32],
        frames: usize,
        vocab: usize,
        target: &[usize],
        blank: usize,
    ) -> f64 {
        let mut sum = 0.0f64;
        for code in 0..vocab.pow(frames as u32) {
            let mut path = Vec::with_capacity(frames);
            let mut rest = code;
            for _ in 0..frames {
                path.push(rest % vocab);
                rest /= vocab;
            }
            // Collapse: merge runs of the same label, then drop blanks.
            let mut collapsed: Vec<usize> = Vec::new();
            let mut prev: Option<usize> = None;
            for &l in &path {
                if Some(l) != prev && l != blank {
                    collapsed.push(l);
                }
                prev = Some(l);
            }
            if collapsed == target {
                let lp: f64 = path
                    .iter()
                    .enumerate()
                    .map(|(t, &l)| logp[t * vocab + l] as f64)
                    .sum();
                sum += lp.exp();
            }
        }
        sum
    }

    fn assert_matches_brute_force(
        rows: &[&[f64]],
        vocab: usize,
        target: &[usize],
        blank: usize,
    ) -> f32 {
        let logp = logp_from_probs(rows);
        let frames = rows.len();
        let expected = brute_force_prob(&logp, frames, vocab, target, blank);
        let got = ctc_forward_logprob(&logp, frames, vocab, target, blank)
            .expect("target is reachable in these frames");
        assert!(
            (got.exp() as f64 - expected).abs() < 1e-5,
            "forward {} (p={}) != brute force {expected}",
            got,
            got.exp()
        );
        got
    }

    #[test]
    fn ctc_forward_matches_brute_force_single_label() {
        // 3 frames x 3 labels = 27 paths, enumerated exhaustively.
        assert_matches_brute_force(
            &[&[0.2, 0.5, 0.3], &[0.6, 0.1, 0.3], &[0.25, 0.35, 0.40]],
            3,
            &[1],
            0,
        );
    }

    #[test]
    fn ctc_forward_matches_brute_force_two_labels() {
        assert_matches_brute_force(
            &[
                &[0.2, 0.5, 0.3],
                &[0.6, 0.1, 0.3],
                &[0.25, 0.35, 0.40],
                &[0.1, 0.2, 0.7],
            ],
            3,
            &[1, 2],
            0,
        );
    }

    #[test]
    fn ctc_forward_matches_brute_force_repeated_label() {
        // The repeat is the interesting case: the `s-2` skip must be blocked
        // so the two labels cannot merge back into one.
        assert_matches_brute_force(
            &[
                &[0.2, 0.5, 0.3],
                &[0.6, 0.1, 0.3],
                &[0.25, 0.35, 0.40],
                &[0.3, 0.45, 0.25],
            ],
            3,
            &[1, 1],
            0,
        );
    }

    #[test]
    fn ctc_forward_matches_brute_force_with_nonzero_blank() {
        // The blank is read from the vocab, so it is not always id 0.
        assert_matches_brute_force(
            &[&[0.5, 0.2, 0.3], &[0.1, 0.6, 0.3], &[0.3, 0.3, 0.4]],
            3,
            &[0, 1],
            2,
        );
    }

    #[test]
    fn ctc_logprob_is_never_positive() {
        let logp = logp_from_probs(&[&[0.2, 0.5, 0.3], &[0.6, 0.1, 0.3], &[0.1, 0.8, 0.1]]);
        for target in [vec![1], vec![2], vec![1, 2]] {
            let lp = ctc_forward_logprob(&logp, 3, 3, &target, 0).expect("reachable");
            assert!(lp <= 0.0, "log-probability {lp} exceeds 0 for {target:?}");
        }
    }

    #[test]
    fn peaky_on_target_approaches_zero() {
        let logp = peaky(&[1, 2], 3, 0.999);
        let lp = ctc_forward_logprob(&logp, 2, 3, &[1, 2], 0).expect("reachable");
        assert!(lp > -0.05, "confident target should score near 0, got {lp}");
    }

    #[test]
    fn peaky_on_wrong_labels_is_strongly_negative() {
        let logp = peaky(&[2, 2], 3, 0.999);
        let lp = ctc_forward_logprob(&logp, 2, 3, &[1], 0).expect("reachable");
        assert!(
            lp < -5.0,
            "wrong target should be heavily penalised, got {lp}"
        );
    }

    #[test]
    fn repeated_label_needs_a_separating_blank() {
        // Two frames cannot spell "the same label twice": the only path is a
        // repeat, which CTC collapses back to one.
        let two = peaky(&[1, 1], 3, 0.9);
        assert_eq!(ctc_forward_logprob(&two, 2, 3, &[1, 1], 0), None);
        let three = peaky(&[1, 0, 1], 3, 0.9);
        let lp = ctc_forward_logprob(&three, 3, 3, &[1, 1], 0).expect("reachable in 3 frames");
        assert!(lp.is_finite());
    }

    #[test]
    fn short_audio_is_unscorable_not_zero() {
        let logp = peaky(&[1, 2], 3, 0.9);
        // Three labels do not fit in two frames.
        assert_eq!(ctc_forward_logprob(&logp, 2, 3, &[1, 2, 1], 0), None);
        assert_eq!(ctc_forced_align(&logp, 2, 3, &[1, 2, 1], 0), None);
    }

    #[test]
    fn degenerate_inputs_return_none_without_panicking() {
        let logp = peaky(&[1, 2], 3, 0.9);
        /// `(posteriors, frames, vocab, target, blank)`.
        type Case<'a> = (&'a [f32], usize, usize, Vec<usize>, usize);
        let cases: Vec<Case<'_>> = vec![
            (&logp, 0, 3, vec![1], 0), // no frames
            (&logp, 2, 0, vec![1], 0), // no vocab
            (&logp, 2, 3, vec![], 0),  // no target
            (&logp, 2, 3, vec![7], 0), // label outside the vocab
            (&logp, 2, 3, vec![0], 0), // label *is* the blank
            (&logp, 2, 3, vec![1], 9), // blank outside the vocab
            (&[], 2, 3, vec![1], 0),   // posteriors shorter than claimed
        ];
        for (data, frames, vocab, ids, blank) in cases {
            assert_eq!(
                ctc_forward_logprob(data, frames, vocab, &ids, blank),
                None,
                "frames={frames} vocab={vocab} ids={ids:?} blank={blank}"
            );
            assert_eq!(ctc_forced_align(data, frames, vocab, &ids, blank), None);
        }
    }

    #[test]
    fn log_add_exp_is_infinity_safe() {
        assert_eq!(
            log_add_exp(f64::NEG_INFINITY, f64::NEG_INFINITY),
            f64::NEG_INFINITY
        );
        assert_eq!(log_add_exp(f64::NEG_INFINITY, -2.0), -2.0);
        assert_eq!(log_add_exp(-2.0, f64::NEG_INFINITY), -2.0);
        // Far-apart terms must not round the smaller one away entirely, and
        // must not lose the larger one either.
        let far = log_add_exp(-1.0, -60.0);
        assert!((far - -1.0).abs() < 1e-6, "got {far}");
    }

    #[test]
    fn log_add_exp_matches_the_naive_formula() {
        for (a, b) in [(-1.0f64, -2.0f64), (-0.1, -0.1), (-8.0, -3.0)] {
            let naive = (a.exp() + b.exp()).ln();
            assert!((log_add_exp(a, b) - naive).abs() < 1e-5, "{a} {b}");
        }
    }

    #[test]
    fn alignment_spans_tile_the_whole_timeline() {
        let logp = peaky(&[1, 1, 0, 2, 2], 3, 0.8);
        let spans = ctc_forced_align(&logp, 5, 3, &[1, 2], 0).expect("alignable");
        assert_eq!(spans.first().map(|s| s.start), Some(0));
        assert_eq!(spans.last().map(|s| s.end), Some(5));
        for pair in spans.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert_eq!(a.end, b.start, "spans must be contiguous: {a:?} {b:?}");
            assert!(a.ext_index < b.ext_index, "spans must advance");
        }
        for s in &spans {
            assert!(s.start < s.end, "empty span {s:?}");
        }
    }

    #[test]
    fn alignment_lands_labels_on_the_frames_that_carry_them() {
        let logp = peaky(&[1, 1, 0, 2, 2], 3, 0.9);
        let spans = ctc_forced_align(&logp, 5, 3, &[1, 2], 0).expect("alignable");
        let label_1 = spans
            .iter()
            .find(|s| s.label == 1)
            .expect("label 1 aligned");
        let label_2 = spans
            .iter()
            .find(|s| s.label == 2)
            .expect("label 2 aligned");
        assert_eq!((label_1.start, label_1.end), (0, 2));
        assert_eq!((label_2.start, label_2.end), (3, 5));
    }

    #[test]
    fn the_percentile_table_is_well_formed() {
        for pair in GOP_PERCENTILE.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "GOP knots must strictly increase: {pair:?}"
            );
            assert!(pair[0].1 <= pair[1].1, "scores must not decrease: {pair:?}");
        }
        assert_eq!(
            GOP_PERCENTILE[GOP_PERCENTILE.len() - 1],
            (0.0, 100),
            "GOP 0 is perfect"
        );
        assert!(GOP_PERCENTILE.iter().all(|&(g, v)| g <= 0.0 && v <= 100));
        // The flag threshold is a knot, so the cutoff is exactly where the
        // measurement put it rather than an interpolation artefact.
        assert!(GOP_PERCENTILE.iter().any(|&(_, v)| v == FLAG_BELOW));
    }

    #[test]
    fn acoustic_scoring_flags_but_never_condemns() {
        // ~25% of flags are right (see GOP_PERCENTILE): a hint, not "poor".
        assert_eq!(verdict_for(0), "unclear");
        assert_eq!(verdict_for(FLAG_BELOW - 1), "unclear");
        assert_eq!(verdict_for(FLAG_BELOW), "good");
        assert_eq!(verdict_for(100), "good");
        let cut = GOP_PERCENTILE
            .iter()
            .find(|&&(_, v)| v == FLAG_BELOW)
            .unwrap()
            .0;
        assert_eq!(verdict_for(gop_to_score(cut)), "good");
        assert_eq!(verdict_for(gop_to_score(cut - 0.3)), "unclear");
    }

    #[test]
    fn gop_to_score_is_monotone_and_clamped() {
        assert_eq!(gop_to_score(0.0), 100);
        assert_eq!(
            gop_to_score(1.0),
            100,
            "positive input is clamped, not trusted"
        );
        assert_eq!(gop_to_score(f32::NEG_INFINITY), 0);
        assert_eq!(gop_to_score(f32::NAN), 0);
        let mut prev = 101u8;
        for step in 0..40 {
            let score = gop_to_score(-0.1 * step as f32);
            assert!(
                score <= prev,
                "score rose from {prev} to {score} at step {step}"
            );
            prev = score;
        }
        assert_eq!(gop_to_score(-20.0), 0);
    }

    fn test_vocab() -> Vocab {
        Vocab::parse(r#"{"<pad>":0,"|":1,"C":2,"A":3,"T":4,"S":5}"#).expect("vocab parses")
    }

    fn out_from(logp: Vec<f32>, frames: usize, vocab: usize) -> AsrOutput {
        AsrOutput {
            text: String::new(),
            logp,
            frames,
            vocab,
            frame_stride_ms: 20.0,
        }
    }

    #[test]
    fn text_to_labels_spells_words_and_separates_them() {
        let v = test_vocab();
        let labels = text_to_labels("cat sat", &v).expect("spellable");
        assert_eq!(labels.ids, vec![2, 3, 4, 1, 5, 3, 4]);
        assert_eq!(labels.words[0].0, "CAT");
        assert_eq!(labels.words[0].1, 0..3);
        assert_eq!(labels.words[1].0, "SAT");
        assert_eq!(labels.words[1].1, 4..7);
    }

    #[test]
    fn text_to_labels_refuses_what_it_cannot_spell() {
        let v = test_vocab();
        assert_eq!(text_to_labels("", &v), Err(ScoreError::EmptyTarget));
        assert_eq!(text_to_labels("   ,.!", &v), Err(ScoreError::EmptyTarget));
        match text_to_labels("cats bark", &v) {
            Err(ScoreError::UnknownChars(chars)) => {
                assert!(chars.contains('B'), "expected B reported, got {chars}");
            }
            other => panic!("expected UnknownChars, got {other:?}"),
        }
    }

    #[test]
    fn text_to_labels_needs_a_word_delimiter() {
        let v = Vocab::parse(r#"{"<pad>":0,"C":1,"A":2,"T":3}"#).expect("vocab parses");
        assert_eq!(text_to_labels("cat", &v), Err(ScoreError::NoWordDelimiter));
    }

    #[test]
    fn a_confidently_spoken_phrase_scores_at_the_top() {
        let v = test_vocab();
        // One frame per label, each confident: C A T | S A T.
        let logp = peaky(&[2, 3, 4, 1, 5, 3, 4], 6, 0.9);
        let out = out_from(logp, 7, 6);
        let score = score_against_target(&out, "cat sat", &v).expect("scorable");
        assert_eq!(score.overall, 100);
        assert_eq!(score.words.len(), 2);
        assert!(score.words.iter().all(|w| w.verdict == "good"));
        assert!(score.target_logprob <= 0.0);
        assert!(score.normalized_conf > 0.9 && score.normalized_conf <= 1.0);
    }

    #[test]
    fn mispronouncing_one_word_drops_only_that_word() {
        let v = test_vocab();
        // Same phrase, but the frame that should carry the S of "SAT" looks
        // like a C instead. "CAT" must hold while "SAT" falls.
        let mut labels = vec![2, 3, 4, 1, 5, 3, 4];
        labels[4] = 2;
        let logp = peaky(&labels, 6, 0.9);
        let out = out_from(logp, 7, 6);
        let score = score_against_target(&out, "cat sat", &v).expect("scorable");

        let cat = &score.words[0];
        let sat = &score.words[1];
        assert_eq!(cat.word, "CAT");
        assert_eq!(sat.word, "SAT");
        assert_eq!(cat.score, 100, "the untouched word must hold");
        assert!(
            sat.score < 40,
            "the mispronounced word must fall, got {}",
            sat.score
        );
        // Whether this synthetic word crosses the flag line is a fact about
        // the calibration table, not about alignment; that is pinned in
        // `acoustic_scoring_flags_but_never_condemns`. What matters here is
        // that acoustic scoring never says "poor" — it can only hint.
        assert_ne!(sat.verdict, "poor");
        assert!(sat.gop < cat.gop);
    }

    #[test]
    fn word_timings_follow_the_frame_stride() {
        let v = test_vocab();
        let logp = peaky(&[2, 3, 4, 1, 5, 3, 4], 6, 0.9);
        let out = out_from(logp, 7, 6);
        let score = score_against_target(&out, "cat sat", &v).expect("scorable");
        // Frames 0..3 at 20 ms each, then the delimiter, then frames 4..7.
        assert_eq!((score.words[0].start_ms, score.words[0].end_ms), (0, 60));
        assert_eq!((score.words[1].start_ms, score.words[1].end_ms), (80, 140));
    }

    /// `score_against_target` exactly as it was before `score_labels`
    /// existed, kept as the oracle for the refactor.
    fn legacy_score(out: &AsrOutput, target: &str, v: &Vocab) -> Result<PronScore, ScoreError> {
        let labels = text_to_labels(target, v)?;
        let blank = usize::try_from(v.blank).map_err(|_| ScoreError::BadVocab)?;
        if blank >= out.vocab {
            return Err(ScoreError::BadVocab);
        }
        let spans = ctc_forced_align(&out.logp, out.frames, out.vocab, &labels.ids, blank)
            .ok_or(ScoreError::NotAlignable)?;
        let target_logprob =
            ctc_forward_logprob(&out.logp, out.frames, out.vocab, &labels.ids, blank)
                .ok_or(ScoreError::NotAlignable)?;
        let mut frame_max = vec![f32::NEG_INFINITY; out.frames];
        let mut free_logprob = 0.0f64;
        for (t, slot) in frame_max.iter_mut().enumerate() {
            let row = &out.logp[t * out.vocab..t * out.vocab + out.vocab];
            let m = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            *slot = m;
            free_logprob += m as f64;
        }
        let stride = out.frame_stride_ms;
        let to_ms = |frame: usize| (frame as f32 * stride).round() as i64;
        let mut words = Vec::new();
        for (word, range) in &labels.words {
            let first_ext = 2 * range.start + 1;
            let last_ext = 2 * (range.end - 1) + 1;
            let (mut sum, mut count, mut start, mut end) = (0.0f64, 0usize, usize::MAX, 0usize);
            for span in &spans {
                if span.ext_index < first_ext || span.ext_index > last_ext || span.label == blank {
                    continue;
                }
                start = start.min(span.start);
                end = end.max(span.end);
                for (t, &best) in frame_max.iter().enumerate().take(span.end).skip(span.start) {
                    sum += (out.logp[t * out.vocab + span.label] - best) as f64;
                    count += 1;
                }
            }
            if count == 0 {
                return Err(ScoreError::NotAlignable);
            }
            let gop = (sum / count as f64) as f32;
            let score = gop_to_score(gop);
            words.push(WordScore {
                word: word.clone(),
                start_ms: to_ms(start),
                end_ms: to_ms(end),
                gop,
                score,
                verdict: verdict_for(score).to_string(),
            });
        }
        let overall = (words.iter().map(|w| w.score as u32).sum::<u32>() as f32
            / words.len() as f32)
            .round()
            .clamp(0.0, 100.0) as u8;
        let normalized_conf = (((target_logprob as f64 - free_logprob) / out.frames as f64).exp()
            as f32)
            .clamp(0.0, 1.0);
        Ok(PronScore {
            overall,
            words,
            target_logprob,
            free_logprob: free_logprob as f32,
            normalized_conf,
        })
    }

    /// Deterministic random posteriors: `frames` rows of log-softmax.
    fn random_logp(frames: usize, vocab: usize, seed: u32) -> Vec<f32> {
        let mut state = seed.max(1);
        let mut out: Vec<f32> = (0..frames * vocab)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32) * 8.0
            })
            .collect();
        crate::asr::log_softmax_rows(&mut out, frames, vocab);
        out
    }

    #[test]
    fn score_labels_matches_score_against_target() {
        // The refactor pin: every fixture, plus random posteriors of several
        // lengths, gives a PronScore identical (bit for bit, `==` on f32) to
        // the implementation `score_labels` replaced.
        let v = test_vocab();
        let mut sat_wrong = vec![2, 3, 4, 1, 5, 3, 4];
        sat_wrong[4] = 2;
        let mut cases = vec![
            out_from(peaky(&[2, 3, 4, 1, 5, 3, 4], 6, 0.9), 7, 6),
            out_from(peaky(&sat_wrong, 6, 0.9), 7, 6),
            out_from(peaky(&[2, 3], 6, 0.9), 2, 6),
        ];
        for (seed, frames) in [(1u32, 7usize), (7, 12), (42, 30), (762, 64)] {
            cases.push(out_from(random_logp(frames, 6, seed), frames, 6));
        }
        let mut scored = 0;
        for (i, out) in cases.iter().enumerate() {
            for target in ["cat sat", "sat", "cat cat sat", "tact"] {
                let got = score_against_target(out, target, &v);
                assert_eq!(
                    got,
                    legacy_score(out, target, &v),
                    "case {i}, target {target:?}"
                );
                scored += usize::from(got.is_ok());
            }
        }
        // Mostly real scores, not two matching refusals.
        assert!(scored >= 20, "only {scored} of 28 cases scored");
    }

    #[test]
    fn score_labels_reports_one_entry_per_label() {
        let v = test_vocab();
        let labels = text_to_labels("cat sat", &v).expect("spellable");
        let out = out_from(peaky(&[2, 3, 4, 1, 5, 3, 4], 6, 0.9), 7, 6);
        let (pron, per) = score_labels(&out, &labels, 0, &GOP_PERCENTILE).expect("scorable");
        assert_eq!(pron.words.len(), 2);
        assert_eq!(
            per.iter().map(|l| l.label).collect::<Vec<_>>(),
            labels.ids,
            "one entry per target label, in order"
        );
        // The `|` between the words belongs to neither.
        assert_eq!(
            per.iter().map(|l| l.word_index).collect::<Vec<_>>(),
            vec![Some(0), Some(0), Some(0), None, Some(1), Some(1), Some(1)]
        );
        assert!(per.iter().all(|l| l.frames == 1 && l.gop <= 0.0));
        assert_eq!((per[4].start_ms, per[4].end_ms), (80, 100));
    }

    #[test]
    fn gop_to_score_with_uses_the_given_table() {
        let table = [(-2.0f32, 0u8), (0.0, 50)];
        assert_eq!(gop_to_score_with(&table, -3.0), 0);
        assert_eq!(gop_to_score_with(&table, -1.0), 25);
        assert_eq!(gop_to_score_with(&table, 0.0), 50);
        assert_eq!(gop_to_score_with(&table, 1.0), 50);
        assert_eq!(gop_to_score_with(&table, f32::NAN), 0);
        assert_eq!(gop_to_score_with(&[], -1.0), 0);
    }

    /// A tiny phone vocab: blank 0, no word delimiter.
    fn phone_vocab() -> Vocab {
        Vocab::parse(r#"{"<pad>":0,"θ":1,"ɪ":2,"ŋ":3,"k":4,"t":5,"s":6,"æ":7}"#)
            .expect("vocab parses")
    }

    fn wp(word: &str, phones: &[&str]) -> crate::phonemize::WordPhones {
        (
            word.to_string(),
            phones.iter().map(|p| p.to_string()).collect(),
        )
    }

    #[test]
    fn phones_to_labels_keeps_word_ranges_without_a_delimiter() {
        let v = phone_vocab();
        let labels = phones_to_labels(
            &[wp("THINK", &["θ", "ɪ", "ŋ", "k"]), wp("IT", &["ɪ", "t"])],
            &v,
        )
        .expect("known phones");
        assert_eq!(labels.ids, vec![1, 2, 3, 4, 2, 5]);
        assert_eq!(labels.words[0], ("THINK".to_string(), 0..4));
        assert_eq!(labels.words[1], ("IT".to_string(), 4..6));
    }

    #[test]
    fn phones_to_labels_refuses_unknown_phones() {
        let v = phone_vocab();
        assert_eq!(
            phones_to_labels(&[wp("THINK", &["θ", "ʘ", "k"])], &v),
            Err(ScoreError::UnknownChars("ʘ".to_string()))
        );
        assert_eq!(phones_to_labels(&[], &v), Err(ScoreError::EmptyTarget));
        // A word with no phones would silently shift every later word.
        assert_eq!(
            phones_to_labels(&[wp("IT", &["ɪ", "t"]), wp("", &[])], &v),
            Err(ScoreError::EmptyTarget)
        );
    }

    #[test]
    fn a_corrupted_phone_is_the_lowest_label_score() {
        // "sat it": s æ t | ɪ t, one confident frame each, except that the
        // frame meant to carry æ looks like θ instead.
        let v = phone_vocab();
        let labels = phones_to_labels(&[wp("SAT", &["s", "æ", "t"]), wp("IT", &["ɪ", "t"])], &v)
            .expect("known phones");
        let mut frames = labels.ids.clone();
        frames[1] = 1;
        let out = out_from(peaky(&frames, 8, 0.9), frames.len(), 8);
        let (pron, per) = score_labels(&out, &labels, 0, &GOP_PERCENTILE).expect("scorable");
        let worst = per
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.gop.total_cmp(&b.1.gop))
            .map(|(i, _)| i);
        assert_eq!(worst, Some(1), "the æ label must score lowest: {per:?}");
        assert_eq!(per[1].word_index, Some(0));
        assert!(
            per.iter().all(|l| l.word_index.is_some()),
            "no delimiter labels"
        );
        assert_eq!(pron.words[1].score, 100, "the untouched word must hold");
        assert!(pron.words[0].score < pron.words[1].score);
    }

    #[test]
    fn audio_too_short_for_the_target_is_a_refusal() {
        let v = test_vocab();
        let logp = peaky(&[2, 3], 6, 0.9);
        let out = out_from(logp, 2, 6);
        assert_eq!(
            score_against_target(&out, "cat sat", &v),
            Err(ScoreError::NotAlignable)
        );
    }
}
