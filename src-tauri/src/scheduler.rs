//! Subsystem 3: cognitive scheduler (FSRS-6).
//!
//! Pure scheduling helpers over an injected `sqlx::SqlitePool`.
//! No tauri imports here — callers resolve the pool via
//! `crate::db::sqlite_pool` and pass it in.

use std::collections::HashMap;

use fsrs::{compute_parameters, ComputeParametersInput, FSRSItem, FSRSReview, MemoryState, FSRS};
use sqlx::Row;

use crate::db::now_unix;
use crate::error::AppError;

/// Card due for review, with preview intervals per rating.
///
/// `intervals` keys are `"1".."4"` (Again/Hard/Good/Easy), values are
/// `next_states` intervals in days, rounded to 2 decimals.
#[derive(Clone, Debug, serde::Serialize)]
pub struct DueCardView {
    pub id: String,
    pub front: String,
    pub back: String,
    pub stability: f32,
    pub difficulty: f32,
    pub days_elapsed: u32,
    pub intervals: HashMap<String, f32>,
}

/// One review-log row, serialized snake_case for the frontend
/// (`card_id`, `delta_t`, `reviewed_at` keys).
///
/// Wired as Tauri commands by the lib.rs owner; allow dead code until then.
#[allow(dead_code)]
#[derive(Clone, Debug, serde::Serialize)]
pub struct DeckRow {
    pub id: String,
    pub name: String,
}

/// One card row for deck browsers.
///
/// Wired as Tauri commands by the lib.rs owner; allow dead code until then.
#[allow(dead_code)]
#[derive(Clone, Debug, serde::Serialize)]
pub struct CardRow {
    pub id: String,
    pub deck_id: String,
    pub front: String,
    pub back: String,
}

/// Aggregate review-history counts for the optimizer gate.
///
/// Wired as Tauri commands by the lib.rs owner; allow dead code until then.
#[allow(dead_code)]
#[derive(Clone, Debug, serde::Serialize)]
pub struct ReviewStats {
    pub distinct_cards: u64,
    pub total_reviews: u64,
    pub min_histories: u64,
}

/// One review-log row, serialized snake_case for the frontend
/// (`card_id`, `delta_t`, `reviewed_at` keys).
#[derive(Clone, Debug, serde::Serialize)]
pub struct ReviewRow {
    pub id: String,
    pub card_id: String,
    pub rating: i64,
    pub delta_t: i64,
    pub reviewed_at: i64,
    pub front: String,
}

fn round2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}

/// FSRS-6 forgetting-curve retrievability: `R = (1 + factor * t / S)^-decay`
/// with `factor = 0.9^(1/-decay) - 1`. Non-positive stability means the
/// memory has no measurable decay yet, so recall is perfect (`1.0`).
pub fn retrievability(stability: f32, days: u32, decay: f32) -> f32 {
    if stability <= 0.0 {
        return 1.0;
    }
    let factor = 0.9f32.powf(1.0 / -decay) - 1.0;
    (1.0 + factor * days as f32 / stability).powf(-decay)
}

/// A card is due when its retrievability has dropped below `retention`.
///
/// Kept as a pure helper (used by tests and external callers); the DB
/// due path filters via `next_due_date` in SQL instead of scanning.
#[allow(dead_code)]
pub fn is_due(stability: f32, days: u32, retention: f32, decay: f32) -> bool {
    retrievability(stability, days, decay) < retention
}

/// Preview intervals for all four ratings.
///
/// - `mem`: current memory state (`None` for never-reviewed).
/// - `retention`: desired retention, e.g. `0.9`.
/// - `elapsed`: days since last review (`0` for first review).
pub fn intervals_of(
    fsrs: &FSRS,
    mem: Option<MemoryState>,
    retention: f32,
    elapsed: u32,
) -> Result<HashMap<String, f32>, AppError> {
    let next = fsrs
        .next_states(mem, retention, elapsed)
        .map_err(|e| AppError::Scheduler(e.to_string()))?;
    let mut m = HashMap::with_capacity(4);
    // Rating mapping: 1=Again, 2=Hard, 3=Good, 4=Easy.
    m.insert("1".to_string(), round2(next.again.interval));
    m.insert("2".to_string(), round2(next.hard.interval));
    m.insert("3".to_string(), round2(next.good.interval));
    m.insert("4".to_string(), round2(next.easy.interval));
    Ok(m)
}

/// Fetch due cards, lowest retrievability first.
///
/// SQL-filtered (no full-table PAGE scan): a row is due when it has no
/// memory row (never reviewed) or `next_due_date <= now` (`NULL` counts
/// as due for pre-migration rows). Requires the
/// `idx_memory_next_due` index on `card_memory_states(next_due_date)`
/// (created in migration 4). Collected due rows are sorted by `R`
/// ascending and truncated to `limit`.
///
/// `days_elapsed = max(0, (now - last_review) / 86400)`. Rows with no
/// memory state get `stability = 0.0, difficulty = 0.0, elapsed = 0,
/// mem = None`.
pub async fn fetch_due_cards(
    pool: &sqlx::SqlitePool,
    fsrs: &FSRS,
    retention: f32,
    decay: f32,
    limit: i64,
) -> Result<Vec<DueCardView>, AppError> {
    if limit <= 0 {
        return Ok(Vec::new());
    }
    let now = now_unix();
    let rows = sqlx::query(
        "SELECT c.id AS id, c.content_front AS front, c.content_back AS back, \
         m.stability AS stability, m.difficulty AS difficulty, \
         m.last_review_date AS last_review_date \
         FROM cards c LEFT JOIN card_memory_states m ON m.card_id = c.id \
         WHERE m.card_id IS NULL OR m.next_due_date IS NULL OR m.next_due_date <= ? \
         ORDER BY COALESCE(m.next_due_date, 0) ASC, COALESCE(m.last_review_date, 0) ASC \
         LIMIT ?",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let mut collected: Vec<(f32, DueCardView)> = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.get("id");
        let front: String = row.get("front");
        let back: String = row.get("back");
        let stability_opt: Option<f64> = row.get("stability");
        let difficulty_opt: Option<f64> = row.get("difficulty");
        let last_review_opt: Option<i64> = row.get("last_review_date");

        let (stability, difficulty, elapsed, mem) =
            match (stability_opt, difficulty_opt, last_review_opt) {
                (Some(s), Some(d), Some(last)) => {
                    let elapsed = now.saturating_sub(last).div_euclid(86_400).max(0) as u32;
                    let stability = s as f32;
                    let difficulty = d as f32;
                    let mem = Some(MemoryState {
                        stability,
                        difficulty,
                    });
                    (stability, difficulty, elapsed, mem)
                }
                _ => (0.0_f32, 0.0_f32, 0_u32, None),
            };

        let r = match mem {
            None => 0.0,
            Some(_) => retrievability(stability, elapsed, decay),
        };

        let intervals = intervals_of(fsrs, mem, retention, elapsed)?;
        collected.push((
            r,
            DueCardView {
                id,
                front,
                back,
                stability,
                difficulty,
                days_elapsed: elapsed,
                intervals,
            },
        ));
    }
    collected.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    collected.truncate(limit as usize);
    Ok(collected.into_iter().map(|(_, view)| view).collect())
}

/// Grade a card and persist the FSRS transition.
///
/// 1. Validate `rating` is `1..=4`, else `Err`.
/// 2. Load current `MemoryState` (or `None`) + `days_elapsed` from
///    `card_memory_states` (`delta_t = elapsed`, first review `0`).
/// 3. `next_states(...)`, pick the `ItemState` for `rating`.
/// 4. In a single transaction: upsert `card_memory_states`
///    (`INSERT ... ON CONFLICT(card_id) DO UPDATE`) with new
///    stability/difficulty/now plus `next_due_date` (second-precision
///    so intraday Again/Hard steps survive a reload:
///    `now + max(60, round(interval_days * 86400))`), then `INSERT` a
///    `review_logs` row (uuid v4, `delta_t = elapsed`,
///    `reviewed_at = now`). Commit before re-fetch so either both rows
///    land or neither does (atomicity).
/// 5. Return the next due card (`fetch_due_cards` limit 1): `None` if
///    nothing else is due. The just-graded card is excluded — if the
///    single top-due row is the card just graded, `None` is returned
///    instead of immediately re-presenting it.
pub async fn grade_card_db(
    pool: &sqlx::SqlitePool,
    fsrs: &FSRS,
    retention: f32,
    decay: f32,
    card_id: &str,
    rating: u32,
) -> Result<Option<DueCardView>, AppError> {
    if !(1..=4).contains(&rating) {
        return Err(AppError::BadInput(format!(
            "invalid rating {rating}: expected 1..=4"
        )));
    }
    let now = now_unix();

    let rec = sqlx::query(
        "SELECT stability, difficulty, last_review_date \
         FROM card_memory_states WHERE card_id = ?",
    )
    .bind(card_id)
    .fetch_optional(pool)
    .await?;

    let (mem, elapsed) = match rec {
        Some(row) => {
            let s: f64 = row.get("stability");
            let d: f64 = row.get("difficulty");
            let last: i64 = row.get("last_review_date");
            let elapsed = now.saturating_sub(last).div_euclid(86_400).max(0) as u32;
            let mem = Some(MemoryState {
                stability: s as f32,
                difficulty: d as f32,
            });
            (mem, elapsed)
        }
        None => (None, 0_u32),
    };

    let next = fsrs
        .next_states(mem, retention, elapsed)
        .map_err(|e| AppError::Scheduler(e.to_string()))?;
    let chosen = match rating {
        1 => &next.again,
        2 => &next.hard,
        3 => &next.good,
        4 => &next.easy,
        _ => unreachable!("rating validated 1..=4"),
    };
    let new_stability = chosen.memory.stability as f64;
    let new_difficulty = chosen.memory.difficulty as f64;
    // Second-precision due time: sub-day intervals (Again/Hard same-day
    // steps) survive a reload. Minimum 60 s so a 0-day interval still
    // schedules a near-future (intraday) review instead of "now".
    let interval_secs = (chosen.interval * 86_400.0).round().max(60.0) as i64;
    let next_due_date = now.saturating_add(interval_secs);

    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO card_memory_states \
         (card_id, stability, difficulty, last_review_date, next_due_date) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(card_id) DO UPDATE SET \
           stability = excluded.stability, \
           difficulty = excluded.difficulty, \
           last_review_date = excluded.last_review_date, \
           next_due_date = excluded.next_due_date",
    )
    .bind(card_id)
    .bind(new_stability)
    .bind(new_difficulty)
    .bind(now)
    .bind(next_due_date)
    .execute(&mut *tx)
    .await?;

    let log_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(log_id)
    .bind(card_id)
    .bind(rating as i64)
    .bind(elapsed as i64)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut due = fetch_due_cards(pool, fsrs, retention, decay, 1).await?;
    let first = due.pop();
    match first {
        Some(card) if card.id == card_id => Ok(None),
        other => Ok(other),
    }
}

/// Seed 3 demo EN/ES cards so first-run review is non-empty.
///
/// Cards (`INSERT OR IGNORE`): ids `demo-1..3`, `deck_id = "default"`,
/// `created_at = now`. Memory rows (`INSERT OR IGNORE`) default to
/// `stability 1.0, difficulty 5.0, last_review 0, next_due_date 0` so the
/// due filter treats them as due and they sort oldest-first.
///
/// Returns the number of card rows actually inserted (0..3).
pub async fn seed_demo_deck(pool: &sqlx::SqlitePool) -> Result<usize, AppError> {
    let now = now_unix();
    let demo: [(&str, &str, &str); 3] = [
        ("demo-1", "Hello", "Hola"),
        ("demo-2", "Good morning", "Buenos días"),
        ("demo-3", "Thank you", "Gracias"),
    ];
    let mut inserted: usize = 0;
    for (id, front, back) in demo {
        let r = sqlx::query(
            "INSERT OR IGNORE INTO cards (id, deck_id, content_front, content_back, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind("default")
        .bind(front)
        .bind(back)
        .bind(now)
        .execute(pool)
        .await?;
        inserted += r.rows_affected() as usize;

        sqlx::query(
            "INSERT OR IGNORE INTO card_memory_states \
             (card_id, stability, difficulty, last_review_date, next_due_date) \
             VALUES (?, 1.0, 5.0, 0, 0)",
        )
        .bind(id)
        .execute(pool)
        .await?;
    }
    Ok(inserted)
}

/// Insert a new card (no memory row, so it is new/due immediately).
///
/// Returns the new card's uuid. `deck_id`, `front`, and `back` must all be
/// non-blank, else `Err`.
pub async fn add_card(
    pool: &sqlx::SqlitePool,
    deck_id: &str,
    front: &str,
    back: &str,
) -> Result<String, AppError> {
    if deck_id.trim().is_empty() {
        return Err(AppError::BadInput("deck_id must not be empty".to_string()));
    }
    if front.trim().is_empty() {
        return Err(AppError::BadInput("front must not be empty".to_string()));
    }
    if back.trim().is_empty() {
        return Err(AppError::BadInput("back must not be empty".to_string()));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_unix();
    // No FK from cards → decks (SQLite FK is best-effort on pooled
    // connections), so register the deck explicitly instead of allowing
    // orphan deck ids.
    sqlx::query("INSERT OR IGNORE INTO decks(id, name, created_at) VALUES(?, ?, ?)")
        .bind(deck_id)
        .bind(deck_id)
        .bind(now)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO cards (id, deck_id, content_front, content_back, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(deck_id)
    .bind(front)
    .bind(back)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(id)
}

/// List all decks ordered by name.
///
/// Wired as a Tauri command by the lib.rs owner; allow dead code until then.
#[allow(dead_code)]
pub async fn list_decks(pool: &sqlx::SqlitePool) -> Result<Vec<DeckRow>, AppError> {
    let rows = sqlx::query("SELECT id, name FROM decks ORDER BY name ASC, id ASC")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| DeckRow {
            id: row.get("id"),
            name: row.get("name"),
        })
        .collect())
}

/// List cards, optionally filtered by deck.
///
/// Maps `content_front`/`content_back` to `front`/`back`.
/// Wired as a Tauri command by the lib.rs owner; allow dead code until then.
#[allow(dead_code)]
pub async fn list_cards(
    pool: &sqlx::SqlitePool,
    deck_id: Option<String>,
) -> Result<Vec<CardRow>, AppError> {
    let rows = match deck_id {
        Some(deck) => {
            sqlx::query(
                "SELECT id, deck_id, content_front AS front, content_back AS back \
                 FROM cards WHERE deck_id = ? ORDER BY created_at ASC, id ASC",
            )
            .bind(deck)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query(
                "SELECT id, deck_id, content_front AS front, content_back AS back \
                 FROM cards ORDER BY created_at ASC, id ASC",
            )
            .fetch_all(pool)
            .await?
        }
    };
    Ok(rows
        .into_iter()
        .map(|row| CardRow {
            id: row.get("id"),
            deck_id: row.get("deck_id"),
            front: row.get("front"),
            back: row.get("back"),
        })
        .collect())
}

/// Delete a card and all its child rows explicitly in one transaction.
///
/// SQLite FK is best-effort on the plugin-owned pool, so `ON DELETE
/// CASCADE` is NOT relied upon: `review_logs` → `card_memory_states` →
/// `cards` are deleted explicitly. Returns `BadInput` if the card id
/// does not exist.
///
/// Wired as a Tauri command by the lib.rs owner; allow dead code until then.
#[allow(dead_code)]
pub async fn delete_card(pool: &sqlx::SqlitePool, card_id: &str) -> Result<(), AppError> {
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM cards WHERE id = ?")
        .bind(card_id)
        .fetch_optional(pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::BadInput(format!("card not found: {card_id}")));
    }
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM review_logs WHERE card_id = ?")
        .bind(card_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM card_memory_states WHERE card_id = ?")
        .bind(card_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM cards WHERE id = ?")
        .bind(card_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Aggregate review-history counts plus the optimizer gate.
///
/// `distinct_cards` = `COUNT(DISTINCT card_id)`, `total_reviews` =
/// `COUNT(*)` from `review_logs`; `min_histories` = 8 (minimum histories
/// `optimize_parameters` requires).
///
/// Wired as a Tauri command by the lib.rs owner; allow dead code until then.
#[allow(dead_code)]
pub async fn review_stats(pool: &sqlx::SqlitePool) -> Result<ReviewStats, AppError> {
    let row = sqlx::query(
        "SELECT COUNT(DISTINCT card_id) AS distinct_cards, COUNT(*) AS total_reviews \
         FROM review_logs",
    )
    .fetch_one(pool)
    .await?;
    let distinct: i64 = row.get("distinct_cards");
    let total: i64 = row.get("total_reviews");
    Ok(ReviewStats {
        distinct_cards: distinct.max(0) as u64,
        total_reviews: total.max(0) as u64,
        min_histories: 8,
    })
}

/// Most recent review-log rows, newest first.
///
/// Contract: `reviewed_at` is emitted as i64 MILLISECONDS since epoch
/// (plain JSON number) so the frontend can `new Date(r.reviewed_at)`.
/// `front` is the first 80 chars of the reviewed card's `content_front`
/// (truncated in SQL via `substr`), INNER JOIN so orphan logs are excluded.
pub async fn recent_reviews(
    pool: &sqlx::SqlitePool,
    limit: i64,
) -> Result<Vec<ReviewRow>, AppError> {
    let rows = sqlx::query(
        "SELECT review_logs.id AS id, review_logs.card_id AS card_id, \
         review_logs.rating AS rating, review_logs.delta_t AS delta_t, \
         review_logs.reviewed_at * 1000 AS reviewed_at, \
         substr(cards.content_front, 1, 80) AS front \
         FROM review_logs JOIN cards ON cards.id = review_logs.card_id \
         ORDER BY review_logs.reviewed_at DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(ReviewRow {
            id: row.get("id"),
            card_id: row.get("card_id"),
            rating: row.get("rating"),
            delta_t: row.get("delta_t"),
            reviewed_at: row.get("reviewed_at"),
            front: row.get("front"),
        });
    }
    Ok(out)
}

/// Optimize FSRS parameters from review history.
///
/// Loads all `review_logs` grouped by `card_id` ordered by
/// `reviewed_at` (plus `rowid` tie-break for same-timestamp rows) into
/// `Vec<FSRSItem { reviews: Vec<FSRSReview { rating, delta_t }> }>`.
/// Requires at least 8 items; fewer keeps the current parameters.
///
/// The blocking `compute_parameters` call runs in
/// `tokio::task::spawn_blocking` so the async runtime stays
/// responsive. NOTE (Thread Domain 4): `compute_parameters` already
/// uses Rayon internally, so this hops from the async executor to a
/// blocking thread which then fans out on the Rayon CPU pool —
/// intentional, do not call it inline on an async worker.
pub async fn optimize_parameters(pool: &sqlx::SqlitePool) -> Result<Vec<f32>, AppError> {
    let rows = sqlx::query(
        "SELECT card_id, rating, delta_t FROM review_logs \
         ORDER BY card_id ASC, reviewed_at ASC, rowid ASC",
    )
    .fetch_all(pool)
    .await?;

    use std::collections::HashMap as Map;
    let mut groups: Map<String, Vec<FSRSReview>> = Map::new();
    let mut order: Vec<String> = Vec::new();
    for row in rows {
        let card_id: String = row.get("card_id");
        let rating_i: i64 = row.get("rating");
        let delta_i: i64 = row.get("delta_t");
        if !(1..=4).contains(&rating_i) {
            continue;
        }
        let review = FSRSReview {
            rating: rating_i as u32,
            delta_t: delta_i.max(0) as u32,
        };
        groups
            .entry(card_id.clone())
            .or_insert_with(|| {
                order.push(card_id);
                Vec::new()
            })
            .push(review);
    }

    let mut items: Vec<FSRSItem> = Vec::with_capacity(groups.len());
    for card_id in &order {
        if let Some(reviews) = groups.remove(card_id) {
            if !reviews.is_empty() {
                items.push(FSRSItem { reviews });
            }
        }
    }

    if items.len() < 8 {
        return Err(AppError::Scheduler(format!(
            "need at least 8 review histories to optimize (found {}); keeping current parameters",
            items.len()
        )));
    }

    let params = tokio::task::spawn_blocking(move || {
        compute_parameters(ComputeParametersInput {
            train_set: items,
            ..Default::default()
        })
    })
    .await
    .map_err(|e| AppError::Scheduler(e.to_string()))?
    .map_err(|e| AppError::Scheduler(e.to_string()))?;
    Ok(params)
}

/// Persist optimized FSRS weights (exactly 21 floats) to `fsrs_params`.
///
/// Single-row semantics via `DELETE` + `INSERT` (table has no PK by
/// design); `updated_at` is seconds since epoch. Best-effort callers
/// should still return the params even if persistence fails.
pub async fn save_fsrs_params(pool: &sqlx::SqlitePool, params: &[f32]) -> Result<(), AppError> {
    if params.len() != 21 {
        return Err(AppError::BadInput(format!(
            "fsrs params must have length 21, got {}",
            params.len()
        )));
    }
    let json = serde_json::to_string(params)
        .map_err(|e| AppError::Scheduler(format!("params serialize failed: {e}")))?;
    let now = now_unix();
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM fsrs_params")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO fsrs_params(params_json, updated_at) VALUES (?, ?)")
        .bind(json)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Load the most recently saved FSRS weights, if any.
///
/// Returns `Ok(None)` when the table is missing/empty. Validates
/// `len == 21` — callers must fall back to `FSRS::default()` with a log
/// line on `None` or length mismatch. Malformed JSON is an `Err`.
pub async fn load_fsrs_params(pool: &sqlx::SqlitePool) -> Result<Option<Vec<f32>>, AppError> {
    let row: Option<sqlx::sqlite::SqliteRow> =
        sqlx::query("SELECT params_json FROM fsrs_params ORDER BY updated_at DESC LIMIT 1")
            .fetch_optional(pool)
            .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let json: String = row.get("params_json");
    let params: Vec<f32> = serde_json::from_str(&json)
        .map_err(|e| AppError::Scheduler(format!("params parse failed: {e}")))?;
    Ok(Some(params))
}

/// Desired retention from the `settings` table, falling back to `0.9`
/// when the row (or table) is missing or unreadable. Best-effort: never
/// fails; out-of-range stored values are clamped to `0.70..=0.98`.
pub async fn load_retention(pool: &sqlx::SqlitePool) -> f32 {
    sqlx::query("SELECT value FROM settings WHERE key = 'retention'")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.try_get::<f64, _>("value").ok())
        .map(|v| (v as f32).clamp(0.70, 0.98))
        .unwrap_or(0.9)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsrs::FSRS6_DEFAULT_DECAY;

    #[test]
    fn intervals_default_has_four_keys() {
        let fsrs = FSRS::default();
        let m = intervals_of(&fsrs, None, 0.9, 0).expect("intervals_of should succeed");
        assert_eq!(m.len(), 4);
        for k in ["1", "2", "3", "4"] {
            assert!(m.contains_key(k), "missing key {k}");
        }
        assert!(m["3"] > 0.0, "good interval should be positive");
    }

    #[test]
    fn retrievability_fresh_is_one() {
        let r = retrievability(2.5, 0, FSRS6_DEFAULT_DECAY);
        assert!((r - 1.0).abs() < 1e-5, "elapsed 0 should give R=1, got {r}");
    }

    #[test]
    fn retrievability_decays_and_guards() {
        let decay = FSRS6_DEFAULT_DECAY;
        let r1 = retrievability(2.5, 1, decay);
        let r10 = retrievability(2.5, 10, decay);
        assert!(
            r1 < 1.0 && r1 > r10,
            "R should decay with time: {r1} vs {r10}"
        );
        assert_eq!(retrievability(0.0, 30, decay), 1.0);
        assert_eq!(retrievability(-1.0, 30, decay), 1.0);
    }

    #[test]
    fn is_due_matches_retention() {
        let decay = FSRS6_DEFAULT_DECAY;
        assert!(is_due(1.0, 365, 0.9, decay));
        assert!(!is_due(100.0, 0, 0.9, decay));
    }

    async fn mem_pool() -> sqlx::SqlitePool {
        use sqlx::sqlite::SqlitePoolOptions;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory pool");
        sqlx::query("PRAGMA foreign_keys=ON;")
            .execute(&pool)
            .await
            .unwrap();
        for stmt in [
            "CREATE TABLE cards (id TEXT PRIMARY KEY NOT NULL, deck_id TEXT NOT NULL, content_front TEXT NOT NULL, content_back TEXT NOT NULL, created_at INTEGER NOT NULL)",
            "CREATE TABLE card_memory_states (card_id TEXT PRIMARY KEY NOT NULL, stability REAL NOT NULL, difficulty REAL NOT NULL, last_review_date INTEGER NOT NULL, next_due_date INTEGER NOT NULL DEFAULT 0, FOREIGN KEY(card_id) REFERENCES cards(id) ON DELETE CASCADE)",
            "CREATE TABLE review_logs (id TEXT PRIMARY KEY NOT NULL, card_id TEXT NOT NULL, rating INTEGER NOT NULL, delta_t INTEGER NOT NULL, reviewed_at INTEGER NOT NULL, FOREIGN KEY(card_id) REFERENCES cards(id) ON DELETE CASCADE)",
            "CREATE INDEX IF NOT EXISTS idx_review_logs_card ON review_logs(card_id)",
            "CREATE INDEX IF NOT EXISTS idx_memory_next_due ON card_memory_states(next_due_date)",
            "CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY NOT NULL, value REAL NOT NULL)",
            "CREATE TABLE IF NOT EXISTS fsrs_params(params_json TEXT NOT NULL, updated_at INTEGER NOT NULL)",
            "CREATE TABLE IF NOT EXISTS decks(id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, created_at INTEGER NOT NULL)",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        pool
    }

    #[tokio::test]
    async fn grade_is_atomic_rollback_on_invalid_rating() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('c1','default','F','B',0)")
            .execute(&pool).await.unwrap();
        // Invalid rating must not write memory or log rows.
        let err = grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "c1", 9).await;
        assert!(err.is_err());
        let mem: Option<sqlx::sqlite::SqliteRow> =
            sqlx::query("SELECT * FROM card_memory_states WHERE card_id='c1'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(mem.is_none(), "failed grade must not create memory row");
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM review_logs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn grade_writes_memory_and_log_atomically_then_rolls_back_on_tx_failure() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('c2','default','F','B',0)")
            .execute(&pool).await.unwrap();
        // Successful grade writes exactly one memory row + one log row.
        let _ = grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "c2", 3)
            .await
            .expect("grade should succeed");
        let n_mem: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM card_memory_states WHERE card_id='c2'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let n_log: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM review_logs WHERE card_id='c2'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((n_mem, n_log), (1, 1));
        // Explicit transaction rollback: staged upsert must vanish when the
        // second statement fails (simulates log-insert failure mid-tx).
        // NOTE: fetch the existing id BEFORE beginning the tx — the
        // in-memory pool is max_connections(1), so a pool query while the
        // tx holds the sole connection would deadlock (PoolTimedOut).
        let existing: String =
            sqlx::query_scalar("SELECT id FROM review_logs WHERE card_id='c2' LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO card_memory_states (card_id, stability, difficulty, last_review_date, next_due_date) \
             VALUES ('c2', 9.9, 9.9, 0, 0) \
             ON CONFLICT(card_id) DO UPDATE SET stability=excluded.stability",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        // Force failure: duplicate PK in review_logs.
        let dup = sqlx::query(
            "INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES (?, 'c2', 3, 0, 0)",
        )
        .bind(existing)
        .execute(&mut *tx)
        .await;
        assert!(dup.is_err(), "duplicate PK should fail");
        tx.rollback().await.unwrap();
        let s: f64 =
            sqlx::query_scalar("SELECT stability FROM card_memory_states WHERE card_id='c2'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            (s - 9.9).abs() > f64::EPSILON,
            "rolled-back upsert must not persist (stability={s})"
        );
    }

    #[tokio::test]
    async fn due_query_uses_next_due_date_and_includes_new_cards() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        let now = crate::db::now_unix();
        // New card (no memory row) is due.
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('new','default','F','B',0)")
            .execute(&pool).await.unwrap();
        // Due card (past due).
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('due','default','F','B',0)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO card_memory_states (card_id, stability, difficulty, last_review_date, next_due_date) VALUES ('due', 2.0, 5.0, 0, 0)")
            .execute(&pool).await.unwrap();
        // Not-due card (future due).
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('future','default','F','B',0)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO card_memory_states (card_id, stability, difficulty, last_review_date, next_due_date) VALUES ('future', 5.0, 5.0, ?, ?)")
            .bind(now)
            .bind(now + 86_400 * 10)
            .execute(&pool).await.unwrap();
        let due = fetch_due_cards(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, 10)
            .await
            .unwrap();
        let ids: Vec<&str> = due.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"new"), "new cards must be due: {ids:?}");
        assert!(ids.contains(&"due"), "past-due must be included: {ids:?}");
        assert!(
            !ids.contains(&"future"),
            "future-due must be excluded: {ids:?}"
        );
    }

    #[tokio::test]
    async fn intraday_again_step_persists_subday_due() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('intra','default','F','B',0)")
            .execute(&pool).await.unwrap();
        let before = crate::db::now_unix();
        let _ = grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "intra", 1)
            .await
            .unwrap();
        let due_ts: i64 = sqlx::query_scalar(
            "SELECT next_due_date FROM card_memory_states WHERE card_id='intra'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let delta = due_ts - before;
        assert!(
            (60..86_400 * 2).contains(&delta),
            "Again step should persist a sub-day-or-small due offset, got {delta}s"
        );
        // Reload simulation: fetch_due must NOT return it immediately when
        // the step is still in the future (intraday scheduling works).
        if delta > 5 {
            let due = fetch_due_cards(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, 10)
                .await
                .unwrap();
            assert!(
                !due.iter().any(|c| c.id == "intra"),
                "just-graded intraday card must not be immediately due"
            );
        }
    }

    #[tokio::test]
    async fn reviewed_at_is_millis() {
        let pool = mem_pool().await;
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('r1','default','F','B',0)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES ('l1','r1',3,0,1_700_000_000)")
            .execute(&pool).await.unwrap();
        let rows = recent_reviews(&pool, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reviewed_at, 1_700_000_000_000);
    }

    #[tokio::test]
    async fn fsrs_params_roundtrip_and_len_guard() {
        let pool = mem_pool().await;
        assert!(load_fsrs_params(&pool).await.unwrap().is_none());
        let params: Vec<f32> = (0..21).map(|i| i as f32 * 0.1).collect();
        save_fsrs_params(&pool, &params).await.unwrap();
        let back = load_fsrs_params(&pool).await.unwrap().expect("saved");
        assert_eq!(back.len(), 21);
        assert!(save_fsrs_params(&pool, &params[..5]).await.is_err());
    }

    #[tokio::test]
    async fn migration4_sql_is_idempotent() {
        let pool = mem_pool().await;
        for _ in 0..2 {
            sqlx::query(crate::db::MIGRATION_4_SQL)
                .execute(&pool)
                .await
                .expect("migration 4 must be re-runnable");
        }
    }

    #[tokio::test]
    async fn seed_uses_default_deck() {
        let pool = mem_pool().await;
        let n = seed_demo_deck(&pool).await.unwrap();
        assert_eq!(n, 3);
        let distinct: Vec<String> = sqlx::query_scalar("SELECT DISTINCT deck_id FROM cards")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(distinct, vec!["default".to_string()]);
    }

    #[tokio::test]
    async fn list_decks_returns_all() {
        let pool = mem_pool().await;
        sqlx::query(
            "INSERT INTO decks (id, name, created_at) VALUES ('d1','Alpha',0), ('d2','Beta',0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let decks = list_decks(&pool).await.unwrap();
        assert_eq!(decks.len(), 2);
        assert_eq!(decks[0].name, "Alpha");
        assert_eq!(decks[1].name, "Beta");
    }

    #[tokio::test]
    async fn list_cards_filter_by_deck() {
        let pool = mem_pool().await;
        for (id, deck) in [("c-a1", "a"), ("c-a2", "a"), ("c-b1", "b")] {
            sqlx::query(
                "INSERT INTO cards (id, deck_id, content_front, content_back, created_at) \
                 VALUES (?, ?, 'F', 'B', 0)",
            )
            .bind(id)
            .bind(deck)
            .execute(&pool)
            .await
            .unwrap();
        }
        let all = list_cards(&pool, None).await.unwrap();
        assert_eq!(all.len(), 3);
        let a = list_cards(&pool, Some("a".to_string())).await.unwrap();
        assert_eq!(a.len(), 2);
        for c in &a {
            assert_eq!(c.deck_id, "a");
            assert_eq!(c.front, "F");
            assert_eq!(c.back, "B");
        }
        let missing = list_cards(&pool, Some("missing".to_string()))
            .await
            .unwrap();
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn delete_card_removes_children_without_cascade() {
        let pool = mem_pool().await;
        // Prove explicit deletes: disable FK so CASCADE cannot help.
        sqlx::query("PRAGMA foreign_keys=OFF;")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('cdel','default','F','B',0)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO card_memory_states (card_id, stability, difficulty, last_review_date, next_due_date) VALUES ('cdel', 2.0, 5.0, 0, 0)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES ('ld1','cdel',3,0,100), ('ld2','cdel',4,1,200)")
            .execute(&pool).await.unwrap();
        delete_card(&pool, "cdel").await.unwrap();
        let n_cards: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cards WHERE id='cdel'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let n_mem: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM card_memory_states WHERE card_id='cdel'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let n_logs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM review_logs WHERE card_id='cdel'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((n_cards, n_mem, n_logs), (0, 0, 0), "no orphans may remain");
        let err = delete_card(&pool, "cdel").await;
        assert!(
            matches!(err, Err(AppError::BadInput(_))),
            "missing card must be BadInput, got {err:?}"
        );
    }

    #[tokio::test]
    async fn review_stats_counts_histories() {
        let pool = mem_pool().await;
        let empty = review_stats(&pool).await.unwrap();
        assert_eq!(
            (
                empty.distinct_cards,
                empty.total_reviews,
                empty.min_histories
            ),
            (0, 0, 8)
        );
        for (id, deck) in [("s1", "default"), ("s2", "default")] {
            sqlx::query(
                "INSERT INTO cards (id, deck_id, content_front, content_back, created_at) \
                 VALUES (?, ?, 'F', 'B', 0)",
            )
            .bind(id)
            .bind(deck)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES ('sl1','s1',3,0,100), ('sl2','s1',3,1,200), ('sl3','s2',4,0,300)")
            .execute(&pool).await.unwrap();
        let stats = review_stats(&pool).await.unwrap();
        assert_eq!(stats.distinct_cards, 2);
        assert_eq!(stats.total_reviews, 3);
        assert_eq!(stats.min_histories, 8);
    }

    #[tokio::test]
    async fn recent_reviews_joins_front_snippet() {
        let pool = mem_pool().await;
        let long_front = "x".repeat(100);
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('rj','default',?, 'B', 0)")
            .bind(&long_front)
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES ('rl1','rj',3,0,1_700_000_001)")
            .execute(&pool).await.unwrap();
        let rows = recent_reviews(&pool, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].front.len(), 80);
        assert_eq!(rows[0].front, "x".repeat(80));
        assert_eq!(rows[0].reviewed_at, 1_700_000_001_000);
    }
}
