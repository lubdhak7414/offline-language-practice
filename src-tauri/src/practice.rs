//! The practice loop: prompts, sessions, attempts and their scores.
//!
//! Everything here takes an injected pool and is free of Tauri types, so the
//! whole loop is exercised against in-memory SQLite in the test suite.
//!
//! Scoring policy, which the rest of the app relies on: a pronunciation
//! number is only ever produced when a reference text exists. Open-ended
//! prompts get grammar feedback and nothing more, and say so. The
//! alternative — scoring free speech against the learner's own transcript —
//! measures how confident the model is, not how well they spoke.

use serde::Serialize;

use crate::db::now_unix;
use crate::error::AppError;
use crate::grammar::LintOutput;
use crate::prompts_seed::BUILTIN_PROMPTS;
use crate::pronounce::{align_words, tokenize, WordAlignment};

/// A prompt as the UI sees it.
#[derive(Debug, Clone, Serialize)]
pub struct PromptView {
    pub id: String,
    pub category: String,
    pub topic: String,
    pub prompt_text: String,
    /// `None` means free speaking: not scored for pronunciation.
    pub target_text: Option<String>,
    pub level: i64,
}

/// One past attempt, for a session history list.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptRow {
    pub id: String,
    pub prompt_id: Option<String>,
    pub target_text: Option<String>,
    pub transcript: String,
    pub duration_ms: i64,
    /// Milliseconds, matching `recent_reviews`.
    pub created_at: i64,
    pub pron_overall: Option<i64>,
    pub pron_method: Option<String>,
    pub overall: i64,
}

/// Everything measured about one recording.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptReport {
    pub attempt_id: String,
    pub transcript: String,
    pub target_text: Option<String>,
    /// How pronunciation was measured: `"text"` (word alignment) or, once
    /// the acoustic scorer ships, `"gop"`. `None` for free speaking.
    pub pron_method: Option<String>,
    /// 0..=100, or `None` when there is no reference to compare against.
    pub pron_overall: Option<u8>,
    /// Word-by-word comparison; `None` for free speaking.
    pub alignment: Option<WordAlignment>,
    pub lint: LintOutput,
    /// 0..=100 from the grammar issues found, relative to how much was said.
    pub grammar_score: u8,
    pub duration_ms: i64,
    pub word_count: usize,
    pub overall: u8,
    /// Which components went into `overall`. The UI shows this verbatim so a
    /// grammar-only score is never mistaken for a full one.
    pub overall_basis: Vec<String>,
}

/// Insert the compiled-in corpus. Idempotent; returns rows newly inserted.
pub async fn seed_prompts(pool: &sqlx::SqlitePool) -> Result<usize, AppError> {
    let now = now_unix();
    let mut tx = pool.begin().await?;
    let mut inserted = 0usize;
    for p in BUILTIN_PROMPTS {
        let res = sqlx::query(
            "INSERT OR IGNORE INTO prompts \
             (id, category, topic, prompt_text, target_text, level, builtin, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, 1, ?)",
        )
        .bind(p.id)
        .bind(p.category)
        .bind(p.topic)
        .bind(p.prompt_text)
        .bind(p.target_text)
        .bind(p.level)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        inserted += res.rows_affected() as usize;
    }
    tx.commit().await?;
    Ok(inserted)
}

/// Open a practice session. `kind` is a category name.
pub async fn start_session(pool: &sqlx::SqlitePool, kind: &str) -> Result<String, AppError> {
    if kind.trim().is_empty() {
        return Err(AppError::BadInput("session kind is required".to_string()));
    }
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO practice_sessions(id, kind, started_at, ended_at) VALUES (?, ?, ?, NULL)",
    )
    .bind(&id)
    .bind(kind)
    .bind(now_unix())
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn end_session(pool: &sqlx::SqlitePool, session_id: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE practice_sessions SET ended_at = ? WHERE id = ? AND ended_at IS NULL")
        .bind(now_unix())
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Pick the next prompt for a session.
///
/// Prompts already attempted in this session are excluded, so a sitting does
/// not hand back the same sentence twice; when the pool runs dry the filter
/// is dropped rather than returning nothing. Selection is random within the
/// filter — a fixed order would mean everyone practises the same eight
/// prompts and never reaches the rest.
pub async fn next_prompt(
    pool: &sqlx::SqlitePool,
    session_id: Option<&str>,
    category: Option<&str>,
    level: Option<i64>,
) -> Result<Option<PromptView>, AppError> {
    let sql = "SELECT id, category, topic, prompt_text, target_text, level FROM prompts \
               WHERE (?1 IS NULL OR category = ?1) \
                 AND (?2 IS NULL OR level = ?2) \
                 AND (?3 IS NULL OR id NOT IN \
                      (SELECT prompt_id FROM attempts \
                       WHERE session_id = ?3 AND prompt_id IS NOT NULL)) \
               ORDER BY RANDOM() LIMIT 1";
    let row = sqlx::query_as::<_, (String, String, String, String, Option<String>, i64)>(sql)
        .bind(category)
        .bind(level)
        .bind(session_id)
        .fetch_optional(pool)
        .await?;

    let row = match row {
        Some(r) => Some(r),
        // Exhausted: repeat rather than end the session on an empty screen.
        None if session_id.is_some() => {
            sqlx::query_as::<_, (String, String, String, String, Option<String>, i64)>(sql)
                .bind(category)
                .bind(level)
                .bind(Option::<&str>::None)
                .fetch_optional(pool)
                .await?
        }
        None => None,
    };

    Ok(row.map(
        |(id, category, topic, prompt_text, target_text, level)| PromptView {
            id,
            category,
            topic,
            prompt_text,
            target_text,
            level,
        },
    ))
}

/// Score a finished recording and persist it.
///
/// `transcript` is whatever ASR produced; this function never runs inference
/// itself, which keeps it testable and keeps the logits inside the ASR
/// worker where they belong.
#[allow(clippy::too_many_arguments)]
pub async fn record_attempt(
    pool: &sqlx::SqlitePool,
    session_id: Option<&str>,
    prompt_id: Option<&str>,
    target_text: Option<&str>,
    transcript: &str,
    duration_ms: i64,
    lint: LintOutput,
) -> Result<AttemptReport, AppError> {
    let target = target_text.map(str::trim).filter(|t| !t.is_empty());
    let word_count = tokenize(transcript).len();

    let alignment = target.map(|t| align_words(transcript, t));
    // `accuracy` is None only for an empty reference, which `target` has
    // already excluded — but map rather than unwrap so a future change to
    // that rule cannot turn into a panic here.
    let pron_overall = alignment.as_ref().and_then(|a| a.accuracy);
    let pron_method = pron_overall.map(|_| "text".to_string());

    let grammar_score = grammar_score(&lint, word_count);

    let mut components: Vec<(&str, u8)> = vec![("grammar", grammar_score)];
    if let Some(p) = pron_overall {
        components.insert(0, ("pronunciation", p));
    }
    let overall =
        (components.iter().map(|(_, v)| *v as u32).sum::<u32>() / components.len() as u32) as u8;
    let overall_basis: Vec<String> = components.iter().map(|(k, _)| (*k).to_string()).collect();

    let attempt_id = uuid::Uuid::new_v4().to_string();
    let now = now_unix();
    let lint_errors = lint.diags.iter().filter(|d| d.severity == "error").count() as i64;
    let lint_suggestions = lint.diags.len() as i64 - lint_errors;

    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO attempts \
         (id, session_id, prompt_id, target_text, transcript, duration_ms, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&attempt_id)
    .bind(session_id)
    .bind(prompt_id)
    .bind(target)
    .bind(transcript)
    .bind(duration_ms)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO attempt_scores \
         (attempt_id, pron_overall, pron_method, lint_error_count, lint_suggestion_count, overall) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&attempt_id)
    .bind(pron_overall.map(i64::from))
    .bind(pron_method.as_deref())
    .bind(lint_errors)
    .bind(lint_suggestions)
    .bind(overall as i64)
    .execute(&mut *tx)
    .await?;

    if let Some(a) = alignment.as_ref() {
        for (index, word) in target_words(a).into_iter().enumerate() {
            sqlx::query(
                "INSERT INTO attempt_word_scores \
                 (attempt_id, word_index, word, score, verdict) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&attempt_id)
            .bind(index as i64)
            .bind(&word.0)
            .bind(word.1)
            .bind(word.2)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;

    Ok(AttemptReport {
        attempt_id,
        transcript: transcript.to_string(),
        target_text: target.map(str::to_string),
        pron_method,
        pron_overall,
        alignment,
        lint,
        grammar_score,
        duration_ms,
        word_count,
        overall,
        overall_basis,
    })
}

/// Per-target-word outcome: `(word, score, verdict)`.
///
/// Text-level alignment only knows right from wrong, so the score is 100 or
/// 0. It is stored anyway because the acoustic scorer will fill the same
/// column with a graded value, and `pron_method` records which produced it.
fn target_words(a: &WordAlignment) -> Vec<(String, i64, &'static str)> {
    use crate::pronounce::WordOp;
    let mut out = Vec::new();
    for op in &a.ops {
        match op {
            WordOp::Match { word, .. } => out.push((word.clone(), 100, "correct")),
            WordOp::Sub { expected, .. } => out.push((expected.clone(), 0, "wrong")),
            WordOp::Del { word, .. } => out.push((word.clone(), 0, "missed")),
            WordOp::Ins { .. } => {}
        }
    }
    out
}

/// Turn grammar issues into 0..=100.
///
/// Scaled by how much was said: two mistakes in six words is a different
/// performance from two in sixty, and a flat per-issue penalty would punish
/// anyone who tries to say more. Errors count double against suggestions.
fn grammar_score(lint: &LintOutput, word_count: usize) -> u8 {
    if word_count == 0 {
        return 0;
    }
    let errors = lint.diags.iter().filter(|d| d.severity == "error").count();
    let others = lint.diags.len() - errors;
    let weighted = errors as f32 * 2.0 + others as f32;
    // One weighted issue per five words lands at zero.
    let ratio = (weighted * 5.0 / word_count as f32).clamp(0.0, 1.0);
    ((1.0 - ratio) * 100.0).round() as u8
}

pub async fn list_attempts(
    pool: &sqlx::SqlitePool,
    session_id: Option<&str>,
    limit: i64,
) -> Result<Vec<AttemptRow>, AppError> {
    let limit = limit.clamp(1, 200);
    let rows = sqlx::query_as::<
        _,
        (
            String,
            Option<String>,
            Option<String>,
            String,
            i64,
            i64,
            Option<i64>,
            Option<String>,
            Option<i64>,
        ),
    >(
        "SELECT a.id, a.prompt_id, a.target_text, a.transcript, a.duration_ms, \
                a.created_at * 1000 AS created_at, s.pron_overall, s.pron_method, s.overall \
         FROM attempts a LEFT JOIN attempt_scores s ON s.attempt_id = a.id \
         WHERE (?1 IS NULL OR a.session_id = ?1) \
         ORDER BY a.created_at DESC, a.rowid DESC LIMIT ?2",
    )
    .bind(session_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                prompt_id,
                target_text,
                transcript,
                duration_ms,
                created_at,
                pron_overall,
                pron_method,
                overall,
            )| {
                AttemptRow {
                    id,
                    prompt_id,
                    target_text,
                    transcript,
                    duration_ms,
                    created_at,
                    pron_overall,
                    pron_method,
                    overall: overall.unwrap_or(0),
                }
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testing::test_pool;
    use crate::grammar::LintDiagnostic;

    fn no_lint() -> LintOutput {
        LintOutput {
            diags: Vec::new(),
            truncated: false,
        }
    }

    fn lint_with(severities: &[&str]) -> LintOutput {
        LintOutput {
            diags: severities
                .iter()
                .map(|s| LintDiagnostic {
                    start: 0,
                    end: 1,
                    message: "m".to_string(),
                    suggestions: Vec::new(),
                    severity: (*s).to_string(),
                    rule_id: "R".to_string(),
                })
                .collect(),
            truncated: false,
        }
    }

    #[tokio::test]
    async fn seeding_is_idempotent() {
        let pool = test_pool().await;
        let first = seed_prompts(&pool).await.unwrap();
        assert_eq!(first, BUILTIN_PROMPTS.len());
        // Re-seeding on upgrade must top up, not duplicate.
        assert_eq!(seed_prompts(&pool).await.unwrap(), 0);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prompts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n as usize, BUILTIN_PROMPTS.len());
    }

    #[tokio::test]
    async fn next_prompt_respects_category_and_level() {
        let pool = test_pool().await;
        seed_prompts(&pool).await.unwrap();
        let p = next_prompt(&pool, None, Some("interview"), None)
            .await
            .unwrap()
            .expect("a prompt");
        assert_eq!(p.category, "interview");
        let p = next_prompt(&pool, None, Some("conversation"), Some(1))
            .await
            .unwrap()
            .expect("a prompt");
        assert_eq!((p.category.as_str(), p.level), ("conversation", 1));
    }

    #[tokio::test]
    async fn next_prompt_skips_what_the_session_already_saw() {
        let pool = test_pool().await;
        seed_prompts(&pool).await.unwrap();
        let session = start_session(&pool, "conversation").await.unwrap();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10 {
            let p = next_prompt(&pool, Some(&session), Some("conversation"), None)
                .await
                .unwrap()
                .expect("a prompt");
            assert!(seen.insert(p.id.clone()), "handed back {} twice", p.id);
            record_attempt(
                &pool,
                Some(&session),
                Some(&p.id),
                p.target_text.as_deref(),
                "SOMETHING",
                1000,
                no_lint(),
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn next_prompt_repeats_rather_than_running_out() {
        let pool = test_pool().await;
        // One prompt, already attempted: the session must still get work.
        sqlx::query(
            "INSERT INTO prompts (id, category, topic, prompt_text, target_text, level, builtin, created_at) \
             VALUES ('p1','conversation','t','say this','SAY THIS',1,1,0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let session = start_session(&pool, "conversation").await.unwrap();
        record_attempt(
            &pool,
            Some(&session),
            Some("p1"),
            Some("SAY THIS"),
            "SAY THIS",
            900,
            no_lint(),
        )
        .await
        .unwrap();
        let again = next_prompt(&pool, Some(&session), Some("conversation"), None)
            .await
            .unwrap();
        assert!(again.is_some(), "an exhausted pool must repeat, not end");
    }

    #[tokio::test]
    async fn read_aloud_attempt_scores_pronunciation() {
        let pool = test_pool().await;
        let report = record_attempt(
            &pool,
            None,
            None,
            Some("The cat sat on the mat."),
            "THE CAT SAT ON THE HAT",
            3000,
            no_lint(),
        )
        .await
        .unwrap();
        assert_eq!(report.pron_method.as_deref(), Some("text"));
        // Five of six target words correct.
        assert_eq!(report.pron_overall, Some(83));
        assert_eq!(report.alignment.as_ref().unwrap().substituted, 1);
        assert!(report.overall_basis.contains(&"pronunciation".to_string()));
    }

    #[tokio::test]
    async fn open_ended_attempt_is_never_given_a_pronunciation_number() {
        let pool = test_pool().await;
        let report = record_attempt(
            &pool,
            None,
            None,
            None,
            "I WENT TO THE SHOPS AND BOUGHT SOME BREAD",
            5000,
            no_lint(),
        )
        .await
        .unwrap();
        assert_eq!(report.pron_overall, None);
        assert_eq!(report.pron_method, None);
        assert!(report.alignment.is_none());
        assert_eq!(report.overall_basis, vec!["grammar".to_string()]);
        // Grammar alone is still a real measurement, so `overall` is it.
        assert_eq!(report.overall, report.grammar_score);
    }

    #[tokio::test]
    async fn a_blank_target_is_treated_as_free_speaking() {
        let pool = test_pool().await;
        let report = record_attempt(&pool, None, None, Some("   "), "ANYTHING", 100, no_lint())
            .await
            .unwrap();
        assert_eq!(report.pron_overall, None, "whitespace is not a reference");
    }

    #[tokio::test]
    async fn attempt_rows_and_word_scores_persist() {
        let pool = test_pool().await;
        let session = start_session(&pool, "conversation").await.unwrap();
        let report = record_attempt(
            &pool,
            Some(&session),
            None,
            Some("THE CAT SAT"),
            "THE HAT SAT",
            2000,
            no_lint(),
        )
        .await
        .unwrap();

        let words = sqlx::query_as::<_, (String, i64, String)>(
            "SELECT word, score, verdict FROM attempt_word_scores \
             WHERE attempt_id = ? ORDER BY word_index",
        )
        .bind(&report.attempt_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            words,
            vec![
                ("THE".to_string(), 100, "correct".to_string()),
                ("CAT".to_string(), 0, "wrong".to_string()),
                ("SAT".to_string(), 100, "correct".to_string()),
            ]
        );

        let rows = list_attempts(&pool, Some(&session), 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pron_method.as_deref(), Some("text"));
        assert_eq!(rows[0].transcript, "THE HAT SAT");
    }

    #[tokio::test]
    async fn list_attempts_is_scoped_to_its_session() {
        let pool = test_pool().await;
        let a = start_session(&pool, "conversation").await.unwrap();
        let b = start_session(&pool, "interview").await.unwrap();
        record_attempt(&pool, Some(&a), None, None, "ONE", 1, no_lint())
            .await
            .unwrap();
        record_attempt(&pool, Some(&b), None, None, "TWO", 1, no_lint())
            .await
            .unwrap();
        assert_eq!(list_attempts(&pool, Some(&a), 10).await.unwrap().len(), 1);
        assert_eq!(list_attempts(&pool, None, 10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn end_session_stamps_once() {
        let pool = test_pool().await;
        let id = start_session(&pool, "conversation").await.unwrap();
        end_session(&pool, &id).await.unwrap();
        let first: Option<i64> =
            sqlx::query_scalar("SELECT ended_at FROM practice_sessions WHERE id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(first.is_some());
        end_session(&pool, &id).await.unwrap();
        let second: Option<i64> =
            sqlx::query_scalar("SELECT ended_at FROM practice_sessions WHERE id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(first, second, "a second close must not move the timestamp");
    }

    #[test]
    fn grammar_score_scales_with_how_much_was_said() {
        // The same two issues are a worse performance in six words than in
        // sixty; a flat penalty would punish anyone who says more.
        let two = lint_with(&["suggestion", "suggestion"]);
        assert!(grammar_score(&two, 60) > grammar_score(&two, 10));
        assert_eq!(grammar_score(&no_lint(), 20), 100);
        // Errors weigh double.
        assert!(
            grammar_score(&lint_with(&["error"]), 20)
                < grammar_score(&lint_with(&["suggestion"]), 20)
        );
        // Saying nothing is not a perfect score.
        assert_eq!(grammar_score(&no_lint(), 0), 0);
        // Never underflows.
        assert_eq!(grammar_score(&lint_with(&["error"; 50]), 3), 0);
    }
}
