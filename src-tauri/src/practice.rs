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
use crate::fluency::FluencyReport;
use crate::grammar::LintOutput;
use crate::prompts_seed::BUILTIN_PROMPTS;
use crate::pronounce::{align_words, tokenize, PronScore, WordAlignment};

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
    /// How pronunciation was measured: `"gop"` when the acoustic scorer ran,
    /// `"text"` when it could not and word alignment stood in. `None` for
    /// free speaking. This always describes how the number in
    /// `pron_overall` was actually produced.
    pub pron_method: Option<String>,
    /// 0..=100, or `None` when there is no reference to compare against.
    pub pron_overall: Option<u8>,
    /// Word-by-word comparison; `None` for free speaking.
    pub alignment: Option<WordAlignment>,
    /// Per-word acoustic detail, present only when `pron_method` is `"gop"`.
    pub pron: Option<PronScore>,
    /// Delivery metrics; `None` when too little was said to measure.
    pub fluency: Option<FluencyReport>,
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

/// Everything one finished recording carries into the database.
///
/// A struct rather than nine positional parameters: every field but the
/// transcript is optional in some combination, and a call site with four
/// `None`s in a row is where the wrong one gets passed.
pub struct AttemptInput<'a> {
    pub session_id: Option<&'a str>,
    pub prompt_id: Option<&'a str>,
    /// The phrase the learner was asked to say. Blank or absent means free
    /// speaking, which is never scored for pronunciation.
    pub target_text: Option<&'a str>,
    pub transcript: &'a str,
    pub duration_ms: i64,
    pub lint: LintOutput,
    /// Acoustic scoring, when it ran. Absent means text alignment stands in.
    pub pron: Option<PronScore>,
    /// Delivery metrics, when there was enough speech to measure.
    pub fluency: Option<FluencyReport>,
}

impl<'a> AttemptInput<'a> {
    /// The minimum an attempt needs: what was said, for how long, and what
    /// the grammar checker made of it.
    pub fn new(transcript: &'a str, duration_ms: i64, lint: LintOutput) -> Self {
        Self {
            session_id: None,
            prompt_id: None,
            target_text: None,
            transcript,
            duration_ms,
            lint,
            pron: None,
            fluency: None,
        }
    }

    pub fn session(mut self, id: Option<&'a str>) -> Self {
        self.session_id = id;
        self
    }

    pub fn prompt(mut self, id: Option<&'a str>) -> Self {
        self.prompt_id = id;
        self
    }

    pub fn target(mut self, text: Option<&'a str>) -> Self {
        self.target_text = text;
        self
    }

    pub fn scored(mut self, pron: Option<PronScore>, fluency: Option<FluencyReport>) -> Self {
        self.pron = pron;
        self.fluency = fluency;
        self
    }
}

/// Score a finished recording and persist it.
///
/// `transcript` is whatever ASR produced; this function never runs inference
/// itself, which keeps it testable and keeps the logits inside the ASR
/// worker where they belong. Acoustic scoring, when it happened, arrives
/// already computed in [`AttemptInput::pron`].
///
/// The pronunciation number prefers the acoustic score and falls back to
/// word-level accuracy, recording which one it used in `pron_method`. Word
/// alignment is computed either way: it is what tells the learner *which*
/// word went missing, which a per-word confidence cannot.
pub async fn record_attempt(
    pool: &sqlx::SqlitePool,
    input: AttemptInput<'_>,
) -> Result<AttemptReport, AppError> {
    let AttemptInput {
        session_id,
        prompt_id,
        target_text,
        transcript,
        duration_ms,
        lint,
        pron,
        fluency,
    } = input;

    let target = target_text.map(str::trim).filter(|t| !t.is_empty());
    let word_count = tokenize(transcript).len();

    let alignment = target.map(|t| align_words(transcript, t));
    // Acoustic first; word accuracy only where there was no acoustic score.
    // `accuracy` is None only for an empty reference, which `target` has
    // already excluded — but map rather than unwrap so a future change to
    // that rule cannot turn into a panic here.
    let (pron_overall, pron_method) = match (&pron, alignment.as_ref().and_then(|a| a.accuracy)) {
        (Some(p), _) => (Some(p.overall), Some("gop".to_string())),
        (None, Some(acc)) => (Some(acc), Some("text".to_string())),
        (None, None) => (None, None),
    };

    let grammar_score = grammar_score(&lint, word_count);

    let mut components: Vec<(&str, u8)> = vec![("grammar", grammar_score)];
    if let Some(p) = pron_overall {
        components.insert(0, ("pronunciation", p));
    }
    if let Some(f) = fluency.as_ref() {
        components.push(("fluency", f.score));
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
         (attempt_id, pron_overall, pron_method, target_logprob, free_logprob, \
          normalized_conf, wpm, articulation_wpm, longest_pause_ms, pause_count, \
          filler_count, lint_error_count, lint_suggestion_count, overall) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&attempt_id)
    .bind(pron_overall.map(i64::from))
    .bind(pron_method.as_deref())
    .bind(pron.as_ref().map(|p| p.target_logprob as f64))
    .bind(pron.as_ref().map(|p| p.free_logprob as f64))
    .bind(pron.as_ref().map(|p| p.normalized_conf as f64))
    .bind(fluency.as_ref().map(|f| f.wpm as f64))
    .bind(fluency.as_ref().map(|f| f.articulation_wpm as f64))
    .bind(fluency.as_ref().map(|f| f.longest_pause_ms))
    .bind(fluency.as_ref().map(|f| f.pause_count))
    .bind(fluency.as_ref().map(|f| f.filler_count + f.like_count))
    .bind(lint_errors)
    .bind(lint_suggestions)
    .bind(overall as i64)
    .execute(&mut *tx)
    .await?;

    for (index, w) in word_rows(pron.as_ref(), alignment.as_ref())
        .into_iter()
        .enumerate()
    {
        sqlx::query(
            "INSERT INTO attempt_word_scores \
             (attempt_id, word_index, word, start_ms, end_ms, gop, score, verdict) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&attempt_id)
        .bind(index as i64)
        .bind(&w.word)
        .bind(w.start_ms)
        .bind(w.end_ms)
        .bind(w.gop)
        .bind(w.score)
        .bind(w.verdict)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    Ok(AttemptReport {
        attempt_id,
        transcript: transcript.to_string(),
        target_text: target.map(str::to_string),
        pron_method,
        pron_overall,
        alignment,
        pron,
        fluency,
        lint,
        grammar_score,
        duration_ms,
        word_count,
        overall,
        overall_basis,
    })
}

/// One row of `attempt_word_scores`, from whichever scorer ran.
struct WordRow {
    word: String,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    gop: Option<f64>,
    score: i64,
    verdict: &'static str,
}

/// Per-word rows, preferring acoustic detail over the text verdict.
///
/// Timings and `gop` are `NULL` for text-level rows rather than zero: a
/// missing measurement is not a measurement of zero, and `pron_method`
/// records which scorer wrote them.
fn word_rows(pron: Option<&PronScore>, alignment: Option<&WordAlignment>) -> Vec<WordRow> {
    if let Some(p) = pron {
        return p
            .words
            .iter()
            .map(|w| WordRow {
                word: w.word.clone(),
                start_ms: Some(w.start_ms),
                end_ms: Some(w.end_ms),
                gop: Some(w.gop as f64),
                score: w.score as i64,
                verdict: match w.verdict.as_str() {
                    "good" => "good",
                    "unclear" => "unclear",
                    _ => "poor",
                },
            })
            .collect();
    }
    alignment
        .map(|a| {
            target_words(a)
                .into_iter()
                .map(|(word, score, verdict)| WordRow {
                    word,
                    start_ms: None,
                    end_ms: None,
                    gop: None,
                    score,
                    verdict,
                })
                .collect()
        })
        .unwrap_or_default()
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
                AttemptInput::new("SOMETHING", 1000, no_lint())
                    .session(Some(&session))
                    .prompt(Some(&p.id))
                    .target(p.target_text.as_deref()),
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
            AttemptInput::new("SAY THIS", 900, no_lint())
                .session(Some(&session))
                .prompt(Some("p1"))
                .target(Some("SAY THIS")),
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
            AttemptInput::new("THE CAT SAT ON THE HAT", 3000, no_lint())
                .target(Some("The cat sat on the mat.")),
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
            AttemptInput::new("I WENT TO THE SHOPS AND BOUGHT SOME BREAD", 5000, no_lint()),
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
        let report = record_attempt(
            &pool,
            AttemptInput::new("ANYTHING", 100, no_lint()).target(Some("   ")),
        )
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
            AttemptInput::new("THE HAT SAT", 2000, no_lint())
                .session(Some(&session))
                .target(Some("THE CAT SAT")),
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

    fn fake_pron(overall: u8) -> PronScore {
        use crate::pronounce::WordScore;
        PronScore {
            overall,
            words: vec![
                WordScore {
                    word: "THE".to_string(),
                    start_ms: 0,
                    end_ms: 200,
                    gop: -0.05,
                    score: 91,
                    verdict: "good".to_string(),
                },
                WordScore {
                    word: "CAT".to_string(),
                    start_ms: 200,
                    end_ms: 600,
                    gop: -1.4,
                    score: 8,
                    verdict: "poor".to_string(),
                },
            ],
            target_logprob: -3.2,
            free_logprob: -1.1,
            normalized_conf: 0.8,
        }
    }

    fn fake_fluency(score: u8) -> FluencyReport {
        FluencyReport {
            wpm: 120.0,
            articulation_wpm: 140.0,
            longest_pause_ms: 500,
            pause_count: 1,
            pauses: Vec::new(),
            filler_count: 2,
            like_count: 1,
            hesitation_count: 0,
            speaking_ms: 1800,
            method: "aligned".to_string(),
            score,
        }
    }

    #[tokio::test]
    async fn acoustic_scoring_outranks_word_alignment() {
        let pool = test_pool().await;
        // The words are all correct, so text alignment would say 100. The
        // acoustic scorer heard them badly and says 50 — and it is the one
        // that actually listened.
        let report = record_attempt(
            &pool,
            AttemptInput::new("THE CAT", 1000, no_lint())
                .target(Some("THE CAT"))
                .scored(Some(fake_pron(50)), None),
        )
        .await
        .unwrap();
        assert_eq!(report.pron_method.as_deref(), Some("gop"));
        assert_eq!(report.pron_overall, Some(50));
        assert_eq!(
            report.alignment.as_ref().and_then(|a| a.accuracy),
            Some(100),
            "the word-by-word view is still computed; it answers a different question"
        );
    }

    #[tokio::test]
    async fn acoustic_word_rows_carry_timings_and_text_rows_do_not() {
        let pool = test_pool().await;
        let scored = record_attempt(
            &pool,
            AttemptInput::new("THE CAT", 1000, no_lint())
                .target(Some("THE CAT"))
                .scored(Some(fake_pron(50)), None),
        )
        .await
        .unwrap();
        let row =
            sqlx::query_as::<_, (String, Option<i64>, Option<i64>, Option<f64>, i64, String)>(
                "SELECT word, start_ms, end_ms, gop, score, verdict FROM attempt_word_scores \
             WHERE attempt_id = ? ORDER BY word_index",
            )
            .bind(&scored.attempt_id)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(row.len(), 2);
        assert_eq!(row[1].0, "CAT");
        assert_eq!((row[1].1, row[1].2), (Some(200), Some(600)));
        assert_eq!(row[1].4, 8);
        assert_eq!(row[1].5, "poor");

        let text_only = record_attempt(
            &pool,
            AttemptInput::new("THE CAT", 1000, no_lint()).target(Some("THE CAT")),
        )
        .await
        .unwrap();
        let text_rows = sqlx::query_as::<_, (Option<i64>, Option<f64>)>(
            "SELECT start_ms, gop FROM attempt_word_scores WHERE attempt_id = ?",
        )
        .bind(&text_only.attempt_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(
            text_rows.iter().all(|r| r.0.is_none() && r.1.is_none()),
            "a measurement that was never taken is NULL, not zero"
        );
    }

    #[tokio::test]
    async fn fluency_joins_the_overall_score_only_when_measured() {
        let pool = test_pool().await;
        let with = record_attempt(
            &pool,
            AttemptInput::new("THE CAT", 1000, no_lint())
                .target(Some("THE CAT"))
                .scored(Some(fake_pron(60)), Some(fake_fluency(90))),
        )
        .await
        .unwrap();
        assert_eq!(with.overall_basis, ["pronunciation", "grammar", "fluency"]);
        assert_eq!(with.overall, (60 + 100 + 90) / 3);
        assert!(with.fluency.is_some());

        let without = record_attempt(
            &pool,
            AttemptInput::new("THE CAT", 1000, no_lint())
                .target(Some("THE CAT"))
                .scored(Some(fake_pron(60)), None),
        )
        .await
        .unwrap();
        assert_eq!(without.overall_basis, ["pronunciation", "grammar"]);
        assert!(without.fluency.is_none());
    }

    #[tokio::test]
    async fn fluency_and_acoustic_columns_persist() {
        let pool = test_pool().await;
        let report = record_attempt(
            &pool,
            AttemptInput::new("THE CAT", 1000, no_lint())
                .target(Some("THE CAT"))
                .scored(Some(fake_pron(60)), Some(fake_fluency(90))),
        )
        .await
        .unwrap();
        let row = sqlx::query_as::<
            _,
            (
                Option<f64>,
                Option<f64>,
                Option<f64>,
                Option<f64>,
                Option<i64>,
            ),
        >(
            "SELECT target_logprob, normalized_conf, wpm, articulation_wpm, filler_count \
             FROM attempt_scores WHERE attempt_id = ?",
        )
        .bind(&report.attempt_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!((row.0.unwrap() - -3.2).abs() < 1e-5);
        assert!((row.1.unwrap() - 0.8).abs() < 1e-5);
        assert!((row.2.unwrap() - 120.0).abs() < 1e-3);
        assert!((row.3.unwrap() - 140.0).abs() < 1e-3);
        assert_eq!(row.4, Some(3), "certain fillers plus the hedged LIKE count");
    }

    #[tokio::test]
    async fn list_attempts_is_scoped_to_its_session() {
        let pool = test_pool().await;
        let a = start_session(&pool, "conversation").await.unwrap();
        let b = start_session(&pool, "interview").await.unwrap();
        record_attempt(
            &pool,
            AttemptInput::new("ONE", 1, no_lint()).session(Some(&a)),
        )
        .await
        .unwrap();
        record_attempt(
            &pool,
            AttemptInput::new("TWO", 1, no_lint()).session(Some(&b)),
        )
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
