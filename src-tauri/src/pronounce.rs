//! Word-level comparison of what was said against what should have been said.
//!
//! Pure: no IO, no `ort`, no database. That is deliberate — it is the part of
//! scoring that can be tested exhaustively, and it is the whole product in
//! Phase 2 (acoustic scoring arrives later and degrades back to this on any
//! error).
//!
//! The comparison is a word-level Levenshtein alignment. Both sides are
//! normalized first, because a wav2vec2 CTC transcript is upper-case and
//! carries no punctuation while a prompt is written like English: comparing
//! `"I'M FINE"` against `"I'm fine, thanks."` must not report three errors
//! that are really typography.

use serde::Serialize;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
