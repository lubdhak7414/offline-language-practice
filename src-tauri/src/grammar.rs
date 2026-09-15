use std::cell::RefCell;
use std::collections::HashMap;

use harper_core::linting::{LintGroup, LintKind, Linter, Suggestion};
use harper_core::spell::FstDictionary;
use harper_core::{Dialect, Document};

/// Max input size in chars (not bytes). Truncation bounds CPU contention
/// so ML inference keeps VRAM headroom per spec.
const MAX_CHARS: usize = 20_000;

// Per-blocking-thread Harper linters, one per dialect, reused across calls.
//
// `LintGroup` is `!Send`, so instances live in thread-local storage
// on whichever `spawn_blocking` worker first needs them (`Dialect` is
// `Copy + Hash`, so it works as the cache key). The dictionary is
// a cheap `Arc` clone handed to each group at creation time.
thread_local! {
    static LINTERS: RefCell<HashMap<Dialect, LintGroup>> = RefCell::new(HashMap::new());
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct LintDiagnostic {
    pub start: usize,
    pub end: usize,
    pub message: String,
    pub suggestions: Vec<String>,
    /// UI severity derived from the Harper lint kind:
    /// `"error"`, `"warning"`, or `"suggestion"` (see [`severity_of`]).
    pub severity: String,
    /// Harper rule identity: the lint-kind key (e.g. `"Spelling"`).
    /// harper-core 0.62 `Lint` carries no separate rule name, so the
    /// kind string is the finest stable identity available.
    pub rule_id: String,
}

/// Lint result: diagnostics plus whether the input exceeded `MAX_CHARS`
/// and was truncated before linting (callers surface this to the user
/// instead of silently cutting off).
#[derive(Clone, Debug, serde::Serialize)]
pub struct LintOutput {
    pub diags: Vec<LintDiagnostic>,
    pub truncated: bool,
}

/// Map a frontend dialect string to a Harper [`Dialect`].
///
/// Accepts `"american"` (default), `"british"`, `"canadian"`,
/// `"australian"` (all verified against the harper-core 0.62 `Dialect`
/// enum: `American | Canadian | Australian | British`). Matching is
/// case-insensitive and trimmed; `None` and anything unknown fall back
/// to [`Dialect::American`] so default behavior is unchanged.
pub fn parse_dialect(raw: Option<&str>) -> Dialect {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("british") => Dialect::British,
        Some("canadian") => Dialect::Canadian,
        Some("australian") => Dialect::Australian,
        _ => Dialect::American,
    }
}

/// Map a Harper [`LintKind`] to a UI severity string.
///
/// Hard correctness problems (spelling, grammar, typos, agreement, ...)
/// surface as `"error"`; style-level improvements as `"suggestion"`;
/// everything else (formatting, redundancy, regionalisms, ...) as
/// `"warning"`. All 20 harper-core 0.62 `LintKind` variants are matched
/// explicitly (no wildcard) so adding a variant upstream is a compile
/// error here, not a silent misclassification.
fn severity_of(kind: &LintKind) -> &'static str {
    match kind {
        LintKind::Agreement
        | LintKind::BoundaryError
        | LintKind::Capitalization
        | LintKind::Grammar
        | LintKind::Malapropism
        | LintKind::Miscellaneous
        | LintKind::Nonstandard
        | LintKind::Punctuation
        | LintKind::Spelling
        | LintKind::Typo => "error",
        LintKind::Enhancement | LintKind::Readability | LintKind::Style | LintKind::WordChoice => {
            "suggestion"
        }
        LintKind::Eggcorn
        | LintKind::Formatting
        | LintKind::Redundancy
        | LintKind::Regionalism
        | LintKind::Repetition
        | LintKind::Usage => "warning",
    }
}

/// Map a char index to its byte offset in `text`.
///
/// Uses `char_indices` chained once with `text.len()` so the
/// one-past-the-end char index maps to `text.len()`, and any
/// out-of-bounds index saturates to `text.len()`.
fn char_idx_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .map(|(byte_idx, _)| byte_idx)
        .chain(std::iter::once(text.len()))
        .nth(char_idx)
        .unwrap_or(text.len())
}

/// Truncate to `MAX_CHARS` chars on a char boundary (suffix cut).
fn truncate_to_char_limit(text: &str) -> &str {
    match text.char_indices().nth(MAX_CHARS) {
        Some((byte_idx, _)) => &text[..byte_idx],
        None => text,
    }
}

/// Synchronous Harper lint using the thread-local [`LINTERS`] cache.
///
/// Both `Document` and `LintGroup` are `!Send`, so callers must invoke
/// this inside `tokio::task::spawn_blocking` (see [`lint_text_async`]).
/// Never store the linter in Tauri `State`.
pub fn lint_text_sync(text: &str, dialect: Dialect) -> Vec<LintDiagnostic> {
    let text = truncate_to_char_limit(text);

    LINTERS.with(|cell| {
        let mut slot = cell.borrow_mut();
        let linter = slot.entry(dialect).or_insert_with(|| {
            // `FstDictionary::curated()` returns an `Arc`; cheap to clone.
            let dict = FstDictionary::curated();
            LintGroup::new_curated(dict, dialect)
        });
        let doc = Document::new_plain_english_curated(text);
        let lints = linter.lint(&doc);

        let mut diagnostics: Vec<LintDiagnostic> = lints
            .iter()
            .map(|lint| {
                let start = char_idx_to_byte(text, lint.span.start);
                let end = char_idx_to_byte(text, lint.span.end);
                let suggestions: Vec<String> = lint
                    .suggestions
                    .iter()
                    .filter_map(|sug| match sug {
                        Suggestion::ReplaceWith(chars) => Some(chars.iter().collect::<String>()),
                        Suggestion::InsertAfter(chars) => {
                            Some(format!("+{}", chars.iter().collect::<String>()))
                        }
                        // `Remove` carries no replacement text — skip it so we
                        // don't emit an empty-string suggestion.
                        Suggestion::Remove => None,
                    })
                    .take(3)
                    .collect();

                LintDiagnostic {
                    start,
                    end,
                    message: lint.message.clone(),
                    suggestions,
                    severity: severity_of(&lint.lint_kind).to_string(),
                    rule_id: lint.lint_kind.to_string_key(),
                }
            })
            .collect();

        diagnostics.sort_by_key(|d| d.start);
        diagnostics
    })
}

/// Async wrapper: runs [`lint_text_sync`] (which owns `!Send` Harper types)
/// on a native worker thread via `spawn_blocking`.
///
/// Returns [`LintOutput`]: the diagnostics plus a `truncated` flag that is
/// true when the input exceeded `MAX_CHARS` and was cut before linting
/// (previously a silent cut). The dialect string is parsed with
/// [`parse_dialect`] (`None`/unknown → American).
///
/// A `JoinError` (panic/cancellation on the worker) is propagated as `Err`
/// instead of being reported as a clean (empty) lint.
pub async fn lint_text_async(text: String, dialect: Option<String>) -> Result<LintOutput, String> {
    let truncated = text.chars().count() > MAX_CHARS;
    let dialect = parse_dialect(dialect.as_deref());
    let diags = tokio::task::spawn_blocking(move || lint_text_sync(&text, dialect))
        .await
        .map_err(|e| format!("grammar worker failed: {e}"))?;
    Ok(LintOutput { diags, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_idx_to_byte_maps_multibyte_chars() {
        // "héllo": h = 1 byte, é = 2 bytes, l/l/o = 1 byte each; len = 6 bytes.
        let text = "héllo";
        assert_eq!(text.len(), 6);
        assert_eq!(char_idx_to_byte(text, 0), 0);
        assert_eq!(char_idx_to_byte(text, 1), 1);
        // char idx 2 is the first 'l', which starts at byte 3 (after 2-byte é).
        assert_eq!(char_idx_to_byte(text, 2), 3);
        assert_eq!(char_idx_to_byte(text, 3), 4);
        assert_eq!(char_idx_to_byte(text, 4), 5);
        // One-past-the-end maps to text.len().
        assert_eq!(char_idx_to_byte(text, 5), 6);
        // Out-of-bounds saturates to text.len().
        assert_eq!(char_idx_to_byte(text, 99), 6);
    }

    #[test]
    fn lint_detects_article_error() {
        let text = "This is an test.";
        let diagnostics = lint_text_sync(text, Dialect::American);
        assert!(
            !diagnostics.is_empty(),
            "expected at least one diagnostic for {text:?}"
        );
        for diag in &diagnostics {
            assert!(
                diag.start <= diag.end && diag.end <= text.len(),
                "span out of bounds: {}..{} for text len {}",
                diag.start,
                diag.end,
                text.len()
            );
            assert!(
                ["error", "warning", "suggestion"].contains(&diag.severity.as_str()),
                "unexpected severity {:?} (rule {:?})",
                diag.severity,
                diag.rule_id
            );
            assert!(
                !diag.rule_id.is_empty(),
                "rule_id must not be empty for {text:?}"
            );
        }
    }

    #[test]
    fn parse_dialect_maps_known_and_defaults_unknown() {
        assert_eq!(parse_dialect(None), Dialect::American);
        assert_eq!(parse_dialect(Some("american")), Dialect::American);
        assert_eq!(parse_dialect(Some("british")), Dialect::British);
        assert_eq!(parse_dialect(Some("canadian")), Dialect::Canadian);
        assert_eq!(parse_dialect(Some("australian")), Dialect::Australian);
        // Case-insensitive + trimmed; unknown falls back to American.
        assert_eq!(parse_dialect(Some(" British ")), Dialect::British);
        assert_eq!(parse_dialect(Some("ENGLISH")), Dialect::American);
        assert_eq!(parse_dialect(Some("")), Dialect::American);
    }

    #[test]
    fn severity_covers_all_lint_kinds() {
        // Every harper-core 0.62 LintKind variant must map to one of the
        // three UI severities (exhaustive match in `severity_of`).
        let kinds = [
            LintKind::Agreement,
            LintKind::BoundaryError,
            LintKind::Capitalization,
            LintKind::Eggcorn,
            LintKind::Enhancement,
            LintKind::Formatting,
            LintKind::Grammar,
            LintKind::Malapropism,
            LintKind::Miscellaneous,
            LintKind::Nonstandard,
            LintKind::Punctuation,
            LintKind::Readability,
            LintKind::Redundancy,
            LintKind::Regionalism,
            LintKind::Repetition,
            LintKind::Spelling,
            LintKind::Style,
            LintKind::Typo,
            LintKind::Usage,
            LintKind::WordChoice,
        ];
        assert_eq!(kinds.len(), 20, "expected all 20 harper-core 0.62 kinds");
        for kind in kinds {
            let sev = severity_of(&kind);
            assert!(
                ["error", "warning", "suggestion"].contains(&sev),
                "kind {kind:?} maps to unexpected severity {sev:?}"
            );
            assert!(
                !kind.to_string_key().is_empty(),
                "kind {kind:?} must have a non-empty rule key"
            );
        }
        // Spot-check the mapping contract.
        assert_eq!(severity_of(&LintKind::Spelling), "error");
        assert_eq!(severity_of(&LintKind::Grammar), "error");
        assert_eq!(severity_of(&LintKind::Style), "suggestion");
        assert_eq!(severity_of(&LintKind::Enhancement), "suggestion");
        assert_eq!(severity_of(&LintKind::Formatting), "warning");
    }

    #[tokio::test]
    async fn lint_output_signals_truncation() {
        let short = lint_text_async("Hello world.".to_string(), None)
            .await
            .unwrap();
        assert!(!short.truncated, "short input must not flag truncated");
        // Just over the 20k char limit: flag set, diagnostics still computed
        // over the truncated prefix.
        let long_text: String = "word ".repeat(MAX_CHARS);
        assert!(long_text.chars().count() > MAX_CHARS);
        let long = lint_text_async(long_text, Some("british".to_string()))
            .await
            .unwrap();
        assert!(long.truncated, "over-limit input must flag truncated");
        // Unknown dialect falls back to American without failing.
        let fallback = lint_text_async("Hello world.".to_string(), Some("klingon".to_string()))
            .await
            .unwrap();
        assert!(!fallback.truncated);
    }
}
