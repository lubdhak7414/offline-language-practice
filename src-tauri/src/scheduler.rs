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
    pub deck_id: String,
    /// `None` only for a card whose deck row is missing (no FK enforcement).
    pub deck_name: Option<String>,
    pub stability: f32,
    pub difficulty: f32,
    pub days_elapsed: u32,
    pub intervals: HashMap<String, f32>,
}

/// One deck row for deck browsers.
///
/// `card_count`/`due_count`/`new_count` are computed in one query
/// (see [`list_decks`]) rather than N+1 per-deck lookups.
#[derive(Clone, Debug, serde::Serialize)]
pub struct DeckRow {
    pub id: String,
    pub name: String,
    pub card_count: i64,
    pub due_count: i64,
    pub new_count: i64,
}

/// One card row for deck browsers.
///
/// `tags` is filled from one grouped query (see [`list_cards`]), not N+1.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CardRow {
    pub id: String,
    pub deck_id: String,
    pub front: String,
    pub back: String,
    pub tags: Vec<String>,
    pub suspended: bool,
    pub buried_until: i64,
}

/// Aggregate review-history counts for the optimizer gate.
///
/// `train_items` is what the optimizer actually consumes: each card with
/// `n >= 2` reviews contributes `n - 1` prefix items (see
/// [`build_train_set`]), so it is the only count that predicts whether
/// training can run. `trainable_cards` counts cards reaching `n >= 2`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ReviewStats {
    pub distinct_cards: u64,
    pub total_reviews: u64,
    pub trainable_cards: u64,
    pub train_items: u64,
    pub min_train_items: u64,
    pub min_trainable_cards: u64,
}

/// Retrievability assigned to a never-reviewed card.
///
/// New cards already sort after reviews in SQL; this only affects the
/// tie-break within the fetched window. `1.0` means "perfectly remembered",
/// so a new card never displaces a review that is actually fading.
pub const NEW_CARD_R: f32 = 1.0;

/// Desired retention used when nothing is persisted yet.
pub const DEFAULT_RETENTION: f32 = 0.9;
/// Lowest desired retention the scheduler will accept.
pub const RETENTION_MIN: f32 = 0.70;
/// Highest desired retention the scheduler will accept.
pub const RETENTION_MAX: f32 = 0.98;

/// Default daily new-card cap, used when no `app_settings` row (or per-deck
/// `deck_config` override) exists yet.
pub const DEFAULT_NEW_PER_DAY: i64 = 20;
/// Default daily review cap, same fallback rule as [`DEFAULT_NEW_PER_DAY`].
pub const DEFAULT_REVIEW_PER_DAY: i64 = 200;
/// A card is "mature" once its stability reaches this many days — the
/// conventional Anki-style threshold, used by `stats::overview` to split
/// new/learning/mature. Lives here because it is a scheduling concept.
pub const MATURE_STABILITY_DAYS: f32 = 21.0;
/// How many `review_undo` rows to keep. Older rows are pruned after every
/// grade so a long-lived database does not grow this table unboundedly —
/// undo only ever needs the single newest row, but a small buffer survives
/// a client that raced two grades before reading the result of the first.
pub const MAX_UNDO_ROWS: i64 = 50;

/// Remaining daily allowance for new cards and reviews, for one deck (or
/// globally when `deck_id` is `None` — see [`daily_caps`]).
#[derive(Debug, Clone, Copy)]
pub struct DailyCaps {
    pub new_left: i64,
    pub rev_left: i64,
}

impl DailyCaps {
    /// No cap in effect — every existing caller that does not pass `caps`
    /// gets this, so B1 landing does not change `due_cards` behavior for
    /// anyone who hasn't touched daily-limit settings.
    pub fn unlimited() -> Self {
        Self {
            new_left: i64::MAX,
            rev_left: i64::MAX,
        }
    }
}

/// Global new/review daily caps from `app_settings`, falling back to the
/// `DEFAULT_*` constants when the keys are missing or unparseable.
async fn global_caps(pool: &sqlx::SqlitePool) -> Result<(i64, i64), AppError> {
    async fn read(pool: &sqlx::SqlitePool, key: &str, default: i64) -> i64 {
        sqlx::query_scalar::<_, String>("SELECT value FROM app_settings WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(default)
    }
    let new_cap = read(pool, "new_per_day", DEFAULT_NEW_PER_DAY).await;
    let rev_cap = read(pool, "review_per_day", DEFAULT_REVIEW_PER_DAY).await;
    Ok((new_cap, rev_cap))
}

/// How many new cards and reviews are still allowed today.
///
/// "Today" is bucketed with [`crate::db::day_index`]/`day_start_unix` using
/// `tz` and `cutoff` so a user who practices past midnight isn't cut off by
/// a calendar-day boundary that doesn't match their own.
///
/// The cap source is `deck_config` for `deck_id` when a row for that deck
/// exists there, else the global `app_settings` defaults — a deck with no
/// override simply inherits the account-wide limit.
///
/// "Used" splits today's `review_logs` rows by whether each is the *first*
/// review of its card ever (`rowid = MIN(rowid)` for that card — insertion
/// order, not `reviewed_at`, which is only second-precision and so cannot
/// tell apart two reviews of a brand-new card graded inside the same
/// second): a first review is a new-card introduction and counts against
/// `new_left`; every later review of the same card counts against
/// `rev_left`. Both counts are floored at 0 — a cap lowered after cards were
/// already reviewed today must not go negative.
pub async fn daily_caps(
    pool: &sqlx::SqlitePool,
    deck_id: Option<&str>,
    tz: i64,
    cutoff: i64,
) -> Result<DailyCaps, AppError> {
    let now = now_unix();
    let day = crate::db::day_index(now, tz, cutoff);
    let day_start = crate::db::day_start_unix(day, tz, cutoff);
    let day_end = day_start + 86_400;

    let (new_cap, rev_cap) = match deck_id {
        Some(id) => {
            let row = sqlx::query(
                "SELECT new_per_day, review_per_day FROM deck_config WHERE deck_id = ?",
            )
            .bind(id)
            .fetch_optional(pool)
            .await?;
            match row {
                Some(row) => (row.get("new_per_day"), row.get("review_per_day")),
                None => global_caps(pool).await?,
            }
        }
        None => global_caps(pool).await?,
    };

    // "First" is picked by `rowid` (insertion order), not `reviewed_at`:
    // `reviewed_at` is second-precision, so two reviews of the same
    // never-before-seen card graded inside the same second would otherwise
    // both match `MIN(reviewed_at)` and both get counted as "new".
    let row = sqlx::query(
        "SELECT \
           COALESCE(SUM(CASE WHEN rl.rowid = first.first_rowid THEN 1 ELSE 0 END), 0) AS new_used, \
           COALESCE(SUM(CASE WHEN rl.rowid != first.first_rowid THEN 1 ELSE 0 END), 0) AS rev_used \
         FROM review_logs rl \
         JOIN cards c ON c.id = rl.card_id \
         JOIN (SELECT card_id, MIN(rowid) AS first_rowid FROM review_logs GROUP BY card_id) first \
           ON first.card_id = rl.card_id \
         WHERE rl.reviewed_at >= ?1 AND rl.reviewed_at < ?2 \
           AND (?3 IS NULL OR c.deck_id = ?3)",
    )
    .bind(day_start)
    .bind(day_end)
    .bind(deck_id)
    .fetch_one(pool)
    .await?;
    let new_used: i64 = row.get("new_used");
    let rev_used: i64 = row.get("rev_used");

    Ok(DailyCaps {
        new_left: (new_cap - new_used).max(0),
        rev_left: (rev_cap - rev_used).max(0),
    })
}

/// Minimum prefix items required before `optimize_parameters` will run.
pub const MIN_TRAIN_ITEMS: u64 = 32;
/// Minimum cards with >= 2 reviews required before optimizing.
pub const MIN_TRAINABLE_CARDS: u64 = 8;
/// Upper bound (days) on any elapsed interval, ~100 years.
///
/// A coarse sanity bound against corrupt input (a delta stored in the wrong
/// unit, a far-future clock). Note it does NOT catch epoch-relative deltas:
/// the epoch is under 20,000 days ago, so those pass this bound — the
/// `last <= 0` guard in [`elapsed_days`] is what stops them.
pub const MAX_DELTA_T: i64 = 36_500;

/// Whole days between `last` and `now`, clamped to `0..=MAX_DELTA_T`.
///
/// A non-positive `last` means "never reviewed" rather than "reviewed at the
/// epoch". Without that distinction a `last_review_date` of 0 produced an
/// elapsed time of ~20,000 days, which was then persisted as the review's
/// `delta_t` and fed to the optimizer.
fn elapsed_days(now: i64, last: i64) -> u32 {
    if last <= 0 {
        return 0;
    }
    now.saturating_sub(last)
        .div_euclid(86_400)
        .clamp(0, MAX_DELTA_T) as u32
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

/// What to pull into a review session.
#[derive(Debug, Clone, Default)]
pub struct DueOpts<'a> {
    /// Maximum cards to return. Zero or less returns nothing.
    pub limit: i64,
    /// Restrict to one deck. `None` means every deck.
    pub deck_id: Option<&'a str>,
    /// Skip this card. Used straight after grading so the card just
    /// answered is not immediately presented again.
    pub exclude_card_id: Option<&'a str>,
    /// Daily new/review caps still allowed today. `None` means unlimited —
    /// every pre-B1 caller gets today's behavior unchanged.
    pub caps: Option<DailyCaps>,
}

impl DueOpts<'_> {
    /// Every deck, no exclusion — the common case.
    #[cfg(test)]
    pub fn limit(limit: i64) -> Self {
        Self {
            limit,
            ..Default::default()
        }
    }
}

/// How many rows to read before the retrievability sort.
///
/// The sort has to see more rows than it returns or it is a no-op: sorting
/// the first `limit` rows by `R` and then keeping all of them changes
/// nothing. Over-fetching a few multiples gives the tie-break something to
/// work with while staying bounded.
fn fetch_window(limit: i64) -> i64 {
    limit.saturating_mul(4).max(64)
}

/// Fetch due cards, most overdue first.
///
/// A row is due when it has no memory row (never reviewed) or
/// `next_due_date <= now` (`NULL` counts as due for pre-migration rows).
/// Uses `idx_memory_next_due`, and the deck filter and exclusion are pushed
/// into SQL — the previous filtered path fetched *every* due card
/// (`LIMIT i64::MAX`), ran FSRS over all of them, then issued a second query
/// and filtered in Rust through a `HashSet`.
///
/// Ordering is: reviews before new cards, then by due date, in SQL; then a
/// retrievability tie-break in Rust over the over-fetched window. New cards
/// are no longer pinned to `R = 0.0`, which used to make them outrank every
/// genuinely overdue review.
///
/// `days_elapsed = max(0, (now - last_review) / 86400)`. Rows with no
/// memory state get `stability = 0.0, difficulty = 0.0, elapsed = 0,
/// mem = None`.
/// Build a `(retrievability, DueCardView)` pair from a reviews-pass row
/// (always carries a memory row, since that pass inner-joins
/// `card_memory_states`).
fn review_row_to_view(
    row: sqlx::sqlite::SqliteRow,
    now: i64,
    fsrs: &FSRS,
    retention: f32,
    decay: f32,
) -> Result<(f32, DueCardView), AppError> {
    let id: String = row.get("id");
    let front: String = row.get("front");
    let back: String = row.get("back");
    let deck_id: String = row.get("deck_id");
    let deck_name: Option<String> = row.get("deck_name");
    let stability_opt: Option<f64> = row.get("stability");
    let difficulty_opt: Option<f64> = row.get("difficulty");
    let last_review_opt: Option<i64> = row.get("last_review_date");

    let (stability, difficulty, elapsed, mem) =
        match (stability_opt, difficulty_opt, last_review_opt) {
            (Some(s), Some(d), Some(last)) => {
                let elapsed = elapsed_days(now, last);
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
        None => NEW_CARD_R,
        Some(_) => retrievability(stability, elapsed, decay),
    };
    let intervals = intervals_of(fsrs, mem, retention, elapsed)?;
    Ok((
        r,
        DueCardView {
            id,
            front,
            back,
            deck_id,
            deck_name,
            stability,
            difficulty,
            days_elapsed: elapsed,
            intervals,
        },
    ))
}

/// Per-deck (or global, when `deck_id` is `None`) daily new/review caps, as
/// stored — not "remaining today" (see [`daily_caps`] for that).
#[derive(Clone, Debug, serde::Serialize)]
pub struct DailyLimits {
    pub deck_id: Option<String>,
    pub new_per_day: i64,
    pub review_per_day: i64,
}

/// Read the caps in effect for `deck_id` (or the global defaults when
/// `None`), the same source [`daily_caps`] reads from.
pub async fn get_daily_limits(
    pool: &sqlx::SqlitePool,
    deck_id: Option<&str>,
) -> Result<DailyLimits, AppError> {
    match deck_id {
        Some(id) => {
            let row = sqlx::query(
                "SELECT new_per_day, review_per_day FROM deck_config WHERE deck_id = ?",
            )
            .bind(id)
            .fetch_optional(pool)
            .await?;
            let (new_per_day, review_per_day) = match row {
                Some(row) => (row.get("new_per_day"), row.get("review_per_day")),
                None => global_caps(pool).await?,
            };
            Ok(DailyLimits {
                deck_id: Some(id.to_string()),
                new_per_day,
                review_per_day,
            })
        }
        None => {
            let (new_per_day, review_per_day) = global_caps(pool).await?;
            Ok(DailyLimits {
                deck_id: None,
                new_per_day,
                review_per_day,
            })
        }
    }
}

/// Persist new caps for `deck_id` (`deck_config`) or globally
/// (`app_settings`, when `None`), clamped to `0..=9999` the same as
/// `prefs::sanitize`. Returns what was actually stored.
pub async fn set_daily_limits(
    pool: &sqlx::SqlitePool,
    deck_id: Option<&str>,
    new_per_day: i64,
    review_per_day: i64,
) -> Result<DailyLimits, AppError> {
    let new_per_day = new_per_day.clamp(0, 9999);
    let review_per_day = review_per_day.clamp(0, 9999);
    match deck_id {
        Some(id) => {
            sqlx::query(
                "INSERT INTO deck_config(deck_id, new_per_day, review_per_day, updated_at) \
                 VALUES(?, ?, ?, ?) \
                 ON CONFLICT(deck_id) DO UPDATE SET \
                   new_per_day = excluded.new_per_day, \
                   review_per_day = excluded.review_per_day, \
                   updated_at = excluded.updated_at",
            )
            .bind(id)
            .bind(new_per_day)
            .bind(review_per_day)
            .bind(now_unix())
            .execute(pool)
            .await?;
            Ok(DailyLimits {
                deck_id: Some(id.to_string()),
                new_per_day,
                review_per_day,
            })
        }
        None => {
            for (key, value) in [
                ("new_per_day", new_per_day),
                ("review_per_day", review_per_day),
            ] {
                sqlx::query(
                    "INSERT INTO app_settings(key, value) VALUES(?, ?) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                )
                .bind(key)
                .bind(value.to_string())
                .execute(pool)
                .await?;
            }
            Ok(DailyLimits {
                deck_id: None,
                new_per_day,
                review_per_day,
            })
        }
    }
}

/// Fetch due cards, reviews before new cards, most overdue first.
///
/// Two separate queries rather than one `UNION`/`CASE` — daily caps apply
/// different limits to each class (`caps.rev_left` vs `caps.new_left`), and
/// a single `LIMIT` cannot express "at most N of these, at most M of those,
/// reviews always first". `opts.caps = None` means unlimited, so this
/// changes nothing for any caller that hasn't touched daily-limit settings.
///
/// **Reviews pass**: `m.next_due_date IS NULL OR <= now` (`NULL` counts as
/// due for pre-migration rows), inner-joined to `card_memory_states` so only
/// previously-reviewed cards appear; deck/exclude/suspended/buried filters
/// pushed into SQL; ordered by due date in SQL, then over-fetched
/// ([`fetch_window`]) and re-sorted by retrievability in Rust, then
/// truncated to `min(limit, caps.rev_left)`.
///
/// **New pass**: `m.card_id IS NULL`, same non-scheduling filters, ordered
/// by `created_at` (creation order — a new card has no retrievability to
/// sort by), limited to `min(limit - reviews.len(), caps.new_left)`. New
/// cards get `NEW_CARD_R`, `mem = None`, `elapsed = 0`.
///
/// Result is `reviews ++ new`, truncated to `opts.limit` — reviews sorting
/// before new cards is structural (two passes concatenated in order), not a
/// SQL `CASE` that could silently stop doing that.
///
/// `days_elapsed = max(0, (now - last_review) / 86400)`. Rows with no
/// memory state get `stability = 0.0, difficulty = 0.0, elapsed = 0,
/// mem = None`.
pub async fn fetch_due_cards(
    pool: &sqlx::SqlitePool,
    fsrs: &FSRS,
    retention: f32,
    decay: f32,
    opts: DueOpts<'_>,
) -> Result<Vec<DueCardView>, AppError> {
    if opts.limit <= 0 {
        return Ok(Vec::new());
    }
    let now = now_unix();
    let caps = opts.caps.unwrap_or_else(DailyCaps::unlimited);

    let rev_limit = opts.limit.min(caps.rev_left).max(0);
    let mut reviews: Vec<(f32, DueCardView)> = Vec::new();
    if rev_limit > 0 {
        let rows = sqlx::query(
            "SELECT c.id AS id, c.content_front AS front, c.content_back AS back, \
             c.deck_id AS deck_id, d.name AS deck_name, \
             m.stability AS stability, m.difficulty AS difficulty, \
             m.last_review_date AS last_review_date \
             FROM cards c \
             JOIN card_memory_states m ON m.card_id = c.id \
             LEFT JOIN decks d ON d.id = c.deck_id \
             LEFT JOIN card_flags f ON f.card_id = c.id \
             WHERE (m.next_due_date IS NULL OR m.next_due_date <= ?1) \
               AND (?2 IS NULL OR c.deck_id = ?2) \
               AND (?3 IS NULL OR c.id <> ?3) \
               AND COALESCE(f.suspended, 0) = 0 \
               AND COALESCE(f.buried_until, 0) <= ?1 \
             ORDER BY COALESCE(m.next_due_date, 0) ASC, \
                      COALESCE(m.last_review_date, 0) ASC \
             LIMIT ?4",
        )
        .bind(now)
        .bind(opts.deck_id)
        .bind(opts.exclude_card_id)
        .bind(fetch_window(rev_limit))
        .fetch_all(pool)
        .await?;
        for row in rows {
            reviews.push(review_row_to_view(row, now, fsrs, retention, decay)?);
        }
        // The window has to be bigger than what we return or this sort is a
        // no-op — see fetch_window's doc.
        reviews.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        reviews.truncate(rev_limit as usize);
    }

    let new_limit = (opts.limit - reviews.len() as i64)
        .min(caps.new_left)
        .max(0);
    if new_limit > 0 {
        let rows = sqlx::query(
            "SELECT c.id AS id, c.content_front AS front, c.content_back AS back, \
             c.deck_id AS deck_id, d.name AS deck_name \
             FROM cards c \
             LEFT JOIN card_memory_states m ON m.card_id = c.id \
             LEFT JOIN decks d ON d.id = c.deck_id \
             LEFT JOIN card_flags f ON f.card_id = c.id \
             WHERE m.card_id IS NULL \
               AND (?1 IS NULL OR c.deck_id = ?1) \
               AND (?2 IS NULL OR c.id <> ?2) \
               AND COALESCE(f.suspended, 0) = 0 \
               AND COALESCE(f.buried_until, 0) <= ?3 \
             ORDER BY c.created_at ASC, c.id ASC \
             LIMIT ?4",
        )
        .bind(opts.deck_id)
        .bind(opts.exclude_card_id)
        .bind(now)
        .bind(new_limit)
        .fetch_all(pool)
        .await?;
        for row in rows {
            let id: String = row.get("id");
            let front: String = row.get("front");
            let back: String = row.get("back");
            let deck_id: String = row.get("deck_id");
            let deck_name: Option<String> = row.get("deck_name");
            let intervals = intervals_of(fsrs, None, retention, 0)?;
            reviews.push((
                NEW_CARD_R,
                DueCardView {
                    id,
                    front,
                    back,
                    deck_id,
                    deck_name,
                    stability: 0.0,
                    difficulty: 0.0,
                    days_elapsed: 0,
                    intervals,
                },
            ));
        }
    }
    reviews.truncate(opts.limit as usize);
    Ok(reviews.into_iter().map(|(_, view)| view).collect())
}

/// Grade a card and persist the FSRS transition.
///
/// 1. Validate `rating` is `1..=4`, else `Err`.
/// 2. Load current `MemoryState` (or `None`) + `days_elapsed` from
///    `card_memory_states` (`delta_t = elapsed`, first review `0`).
/// 3. `next_states(...)`, pick the `ItemState` for `rating`.
/// 4. In a single transaction: insert a `review_undo` row carrying the new
///    review's id and whatever the memory row said *before* this grade
///    (`had_memory` distinguishes "no prior row" from "had one"), prune
///    `review_undo` to the newest [`MAX_UNDO_ROWS`], then upsert
///    `card_memory_states` (`INSERT ... ON CONFLICT(card_id) DO UPDATE`)
///    with new stability/difficulty/now plus `next_due_date`
///    (second-precision so intraday Again/Hard steps survive a reload:
///    `now + max(60, round(interval_days * 86400))`), then `INSERT` a
///    `review_logs` row (uuid v4, `delta_t = elapsed`,
///    `reviewed_at = now`). Commit before re-fetch so either every row
///    lands or none does (atomicity).
/// 5. Return the next due card (`fetch_due_cards` limit 1, excluding the
///    card just graded, capped by the caller's remaining daily allowance so
///    grading one card cannot pull in one more than the cap permits):
///    `None` only when nothing else is actually due.
#[allow(clippy::too_many_arguments)]
pub async fn grade_card_db(
    pool: &sqlx::SqlitePool,
    fsrs: &FSRS,
    retention: f32,
    decay: f32,
    card_id: &str,
    rating: u32,
    tz_offset_minutes: i64,
    cutoff_hour: i64,
) -> Result<Option<DueCardView>, AppError> {
    if !(1..=4).contains(&rating) {
        return Err(AppError::BadInput(format!(
            "invalid rating {rating}: expected 1..=4"
        )));
    }
    let now = now_unix();

    let rec = sqlx::query(
        "SELECT stability, difficulty, last_review_date, next_due_date \
         FROM card_memory_states WHERE card_id = ?",
    )
    .bind(card_id)
    .fetch_optional(pool)
    .await?;

    let (mem, elapsed, had_memory, prev_stability, prev_difficulty, prev_last, prev_due) = match rec
    {
        Some(row) => {
            let s: f64 = row.get("stability");
            let d: f64 = row.get("difficulty");
            let last: i64 = row.get("last_review_date");
            let due: i64 = row.get("next_due_date");
            let elapsed = elapsed_days(now, last);
            let mem = Some(MemoryState {
                stability: s as f32,
                difficulty: d as f32,
            });
            (mem, elapsed, true, Some(s), Some(d), Some(last), Some(due))
        }
        None => (None, 0_u32, false, None, None, None, None),
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

    let log_id = uuid::Uuid::new_v4().to_string();
    let mut tx = pool.begin().await?;
    // Written before the upsert so it captures the *prior* state — undo has
    // nothing to restore once this transaction commits the new one.
    sqlx::query(
        "INSERT INTO review_undo \
         (review_id, card_id, had_memory, prev_stability, prev_difficulty, \
          prev_last_review_date, prev_next_due_date, reviewed_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&log_id)
    .bind(card_id)
    .bind(had_memory as i64)
    .bind(prev_stability)
    .bind(prev_difficulty)
    .bind(prev_last)
    .bind(prev_due)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    // Keep only the newest MAX_UNDO_ROWS so this table cannot grow without
    // bound over a long-lived database — undo only ever needs the latest.
    sqlx::query(
        "DELETE FROM review_undo WHERE review_id NOT IN \
         (SELECT review_id FROM review_undo ORDER BY reviewed_at DESC, rowid DESC LIMIT ?)",
    )
    .bind(MAX_UNDO_ROWS)
    .execute(&mut *tx)
    .await?;

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

    sqlx::query(
        "INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&log_id)
    .bind(card_id)
    .bind(rating as i64)
    .bind(elapsed as i64)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // Recomputed after commit so the next card offered still respects
    // today's remaining allowance, including the review just written.
    let caps = daily_caps(pool, None, tz_offset_minutes, cutoff_hour).await?;
    // Exclude in SQL rather than fetching one row and discarding it when it
    // happens to be the card just graded — that ended sessions early, and
    // reported "nothing due" with other cards still waiting.
    let mut due = fetch_due_cards(
        pool,
        fsrs,
        retention,
        decay,
        DueOpts {
            limit: 1,
            deck_id: None,
            exclude_card_id: Some(card_id),
            caps: Some(caps),
        },
    )
    .await?;
    Ok(due.pop())
}

/// Result of successfully undoing the most recent review.
#[derive(Clone, Debug, serde::Serialize)]
pub struct UndoResult {
    pub card_id: String,
    pub front: String,
    pub rating: i64,
}

/// Undo the single most recent review, restoring the card's prior
/// scheduling state.
///
/// One transaction: take the newest `review_undo` row
/// (`ORDER BY reviewed_at DESC, rowid DESC LIMIT 1` — `rowid` breaks ties
/// between two reviews graded in the same second), read the matching
/// `review_logs` row for its rating, delete that review log, then either
/// delete the `card_memory_states` row (`had_memory = 0`: the card was new,
/// so undoing its first review must leave it new again) or restore the four
/// prior values (`had_memory = 1`), then delete the undo row itself.
/// Returns `None` when there is nothing to undo — a fresh session, or one
/// that already used its only undo.
pub async fn undo_last_review(pool: &sqlx::SqlitePool) -> Result<Option<UndoResult>, AppError> {
    let mut tx = pool.begin().await?;
    let undo_row = sqlx::query(
        "SELECT review_id, card_id, had_memory, prev_stability, prev_difficulty, \
         prev_last_review_date, prev_next_due_date \
         FROM review_undo ORDER BY reviewed_at DESC, rowid DESC LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(undo_row) = undo_row else {
        tx.commit().await?;
        return Ok(None);
    };
    let review_id: String = undo_row.get("review_id");
    let card_id: String = undo_row.get("card_id");
    let had_memory: i64 = undo_row.get("had_memory");
    let prev_stability: Option<f64> = undo_row.get("prev_stability");
    let prev_difficulty: Option<f64> = undo_row.get("prev_difficulty");
    let prev_last: Option<i64> = undo_row.get("prev_last_review_date");
    let prev_due: Option<i64> = undo_row.get("prev_next_due_date");

    let rating: Option<i64> = sqlx::query_scalar("SELECT rating FROM review_logs WHERE id = ?")
        .bind(&review_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(rating) = rating else {
        // The undo row outlived its review log (should not happen; belt and
        // braces). Drop the stale undo row and report nothing to undo.
        sqlx::query("DELETE FROM review_undo WHERE review_id = ?")
            .bind(&review_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(None);
    };
    let front: String = sqlx::query_scalar("SELECT content_front FROM cards WHERE id = ?")
        .bind(&card_id)
        .fetch_one(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM review_logs WHERE id = ?")
        .bind(&review_id)
        .execute(&mut *tx)
        .await?;

    if had_memory == 0 {
        sqlx::query("DELETE FROM card_memory_states WHERE card_id = ?")
            .bind(&card_id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query(
            "UPDATE card_memory_states SET stability = ?, difficulty = ?, \
             last_review_date = ?, next_due_date = ? WHERE card_id = ?",
        )
        .bind(prev_stability)
        .bind(prev_difficulty)
        .bind(prev_last)
        .bind(prev_due)
        .bind(&card_id)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query("DELETE FROM review_undo WHERE review_id = ?")
        .bind(&review_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(Some(UndoResult {
        card_id,
        front,
        rating,
    }))
}

/// Seed 3 demo EN/ES cards so first-run review is non-empty.
///
/// Cards (`INSERT OR IGNORE`): ids `demo-1..3`, `deck_id = "default"`,
/// `created_at = now`. No `card_memory_states` row is written, so demo cards
/// are genuinely new exactly like `add_card` output. Seeding one previously
/// set `last_review_date = 0`, which made the first grade compute an elapsed
/// time of ~20,000 days and persist it as the review's `delta_t`.
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
    // NOTE: the fallback above names the deck after its id. That is only
    // reachable for a deck id the caller invented; `create_deck` is the
    // supported path and keeps the two separate.
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

/// The deck every card falls back to. Cannot be renamed away or deleted.
pub const DEFAULT_DECK_ID: &str = "default";

/// Create a deck with a generated id.
///
/// The id is a uuid rather than the name, so renaming a deck does not
/// orphan its cards — `add_card` used to register `deck_id` as both id and
/// name, which made the two indistinguishable.
pub async fn create_deck(pool: &sqlx::SqlitePool, name: &str) -> Result<String, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::BadInput(
            "deck name must not be empty".to_string(),
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO decks(id, name, created_at) VALUES(?, ?, ?)")
        .bind(&id)
        .bind(name)
        .bind(now_unix())
        .execute(pool)
        .await?;
    Ok(id)
}

pub async fn rename_deck(
    pool: &sqlx::SqlitePool,
    deck_id: &str,
    name: &str,
) -> Result<(), AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::BadInput(
            "deck name must not be empty".to_string(),
        ));
    }
    let res = sqlx::query("UPDATE decks SET name = ? WHERE id = ?")
        .bind(name)
        .bind(deck_id)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::BadInput(format!("deck not found: {deck_id}")));
    }
    Ok(())
}

/// What to do with the cards in a deck being deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckDeleteMode {
    /// Move them to the default deck.
    Move,
    /// Delete them, and everything that references them.
    Delete,
}

impl DeckDeleteMode {
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "move" => Ok(Self::Move),
            "delete" => Ok(Self::Delete),
            other => Err(AppError::BadInput(format!(
                "unknown delete mode {other:?}: expected \"move\" or \"delete\""
            ))),
        }
    }
}

/// Delete a deck, either rehoming or destroying its cards.
///
/// One transaction, children first — `ON DELETE CASCADE` is not reliable
/// here because the plugin owns the pool and `PRAGMA foreign_keys` is
/// per-connection.
pub async fn delete_deck(
    pool: &sqlx::SqlitePool,
    deck_id: &str,
    mode: DeckDeleteMode,
) -> Result<usize, AppError> {
    if deck_id == DEFAULT_DECK_ID {
        return Err(AppError::BadInput(
            "the default deck cannot be deleted".to_string(),
        ));
    }
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM decks WHERE id = ?")
        .bind(deck_id)
        .fetch_optional(pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::BadInput(format!("deck not found: {deck_id}")));
    }

    let mut tx = pool.begin().await?;
    let affected = match mode {
        DeckDeleteMode::Move => sqlx::query("UPDATE cards SET deck_id = ? WHERE deck_id = ?")
            .bind(DEFAULT_DECK_ID)
            .bind(deck_id)
            .execute(&mut *tx)
            .await?
            .rows_affected(),
        DeckDeleteMode::Delete => {
            let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM cards WHERE deck_id = ?")
                .bind(deck_id)
                .fetch_all(&mut *tx)
                .await?;
            for id in &ids {
                delete_card_children(&mut tx, id).await?;
            }
            sqlx::query("DELETE FROM cards WHERE deck_id = ?")
                .bind(deck_id)
                .execute(&mut *tx)
                .await?;
            ids.len() as u64
        }
    };
    sqlx::query("DELETE FROM deck_config WHERE deck_id = ?")
        .bind(deck_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM decks WHERE id = ?")
        .bind(deck_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(affected as usize)
}

/// Edit a card's text in place, keeping its scheduling history.
pub async fn update_card(
    pool: &sqlx::SqlitePool,
    card_id: &str,
    front: &str,
    back: &str,
) -> Result<(), AppError> {
    let front = front.trim();
    let back = back.trim();
    if front.is_empty() || back.is_empty() {
        return Err(AppError::BadInput(
            "front and back must not be empty".to_string(),
        ));
    }
    let res = sqlx::query("UPDATE cards SET content_front = ?, content_back = ? WHERE id = ?")
        .bind(front)
        .bind(back)
        .bind(card_id)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::BadInput(format!("card not found: {card_id}")));
    }
    Ok(())
}

/// Remove every row that references a card, inside a caller's transaction.
///
/// Kept in one place so a new child table is wired into every delete path at
/// once rather than being forgotten in one of them.
async fn delete_card_children(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
) -> Result<(), AppError> {
    for sql in [
        "DELETE FROM review_undo WHERE card_id = ?",
        "DELETE FROM card_tags WHERE card_id = ?",
        "DELETE FROM attempt_cards WHERE card_id = ?",
        "DELETE FROM card_flags WHERE card_id = ?",
        "DELETE FROM review_logs WHERE card_id = ?",
        "DELETE FROM card_memory_states WHERE card_id = ?",
    ] {
        sqlx::query(sql).bind(card_id).execute(&mut **tx).await?;
    }
    Ok(())
}

/// List all decks ordered by name, with per-deck totals.
///
/// `card_count`/`due_count`/`new_count` are computed in one grouped query
/// rather than one query per deck. `due_count` mirrors `fetch_due_cards`'
/// due predicate (`NULL` or past `next_due_date`, not suspended/buried);
/// `new_count` mirrors its "no memory row, not suspended" predicate. Buried
/// new cards still count as new here (a bury only ever applies to a card
/// that has been reviewed at least once), matching `fetch_due_cards`, which
/// never even queries `card_flags.buried_until` on the new-card side.
pub async fn list_decks(pool: &sqlx::SqlitePool) -> Result<Vec<DeckRow>, AppError> {
    let now = now_unix();
    let rows = sqlx::query(
        "SELECT d.id AS id, d.name AS name, \
         COUNT(c.id) AS card_count, \
         COALESCE(SUM(CASE WHEN m.card_id IS NOT NULL \
                             AND (m.next_due_date IS NULL OR m.next_due_date <= ?1) \
                             AND COALESCE(f.suspended, 0) = 0 \
                             AND COALESCE(f.buried_until, 0) <= ?1 \
                        THEN 1 ELSE 0 END), 0) AS due_count, \
         COALESCE(SUM(CASE WHEN m.card_id IS NULL AND COALESCE(f.suspended, 0) = 0 \
                        THEN 1 ELSE 0 END), 0) AS new_count \
         FROM decks d \
         LEFT JOIN cards c ON c.deck_id = d.id \
         LEFT JOIN card_memory_states m ON m.card_id = c.id \
         LEFT JOIN card_flags f ON f.card_id = c.id \
         GROUP BY d.id, d.name \
         ORDER BY d.name ASC, d.id ASC",
    )
    .bind(now)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| DeckRow {
            id: row.get("id"),
            name: row.get("name"),
            card_count: row.get("card_count"),
            due_count: row.get("due_count"),
            new_count: row.get("new_count"),
        })
        .collect())
}

/// List cards, optionally filtered by deck.
///
/// Maps `content_front`/`content_back` to `front`/`back`. `tags` comes from
/// a correlated `GROUP_CONCAT` subquery (one query overall, not one round
/// trip per card), joined with `char(31)` (unit separator) so a tag name
/// containing a comma cannot be mis-split.
pub async fn list_cards(
    pool: &sqlx::SqlitePool,
    deck_id: Option<String>,
) -> Result<Vec<CardRow>, AppError> {
    let sql = "SELECT c.id AS id, c.deck_id AS deck_id, \
         c.content_front AS front, c.content_back AS back, \
         COALESCE(f.suspended, 0) AS suspended, \
         COALESCE(f.buried_until, 0) AS buried_until, \
         (SELECT GROUP_CONCAT(t.name, char(31)) \
          FROM card_tags ct JOIN tags t ON t.id = ct.tag_id \
          WHERE ct.card_id = c.id) AS tags \
         FROM cards c \
         LEFT JOIN card_flags f ON f.card_id = c.id \
         WHERE (?1 IS NULL OR c.deck_id = ?1) \
         ORDER BY c.created_at ASC, c.id ASC";
    let rows = sqlx::query(sql).bind(deck_id).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let tags_raw: Option<String> = row.get("tags");
            let tags = tags_raw
                .map(|s| s.split('\u{1f}').map(str::to_string).collect())
                .unwrap_or_default();
            let suspended: i64 = row.get("suspended");
            CardRow {
                id: row.get("id"),
                deck_id: row.get("deck_id"),
                front: row.get("front"),
                back: row.get("back"),
                tags,
                suspended: suspended != 0,
                buried_until: row.get("buried_until"),
            }
        })
        .collect())
}

/// Normalize a raw tag name: trim, collapse internal whitespace to `-`,
/// lowercase. Empty (post-trim) names are dropped so a blank tag box in the
/// UI never becomes a stored tag.
pub fn normalize_tag(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let collapsed = trimmed
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase();
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

/// One tag with how many cards currently carry it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TagRow {
    pub id: String,
    pub name: String,
    pub card_count: i64,
}

/// All tags, ordered by name, with live card counts.
pub async fn list_tags(pool: &sqlx::SqlitePool) -> Result<Vec<TagRow>, AppError> {
    let rows = sqlx::query(
        "SELECT t.id AS id, t.name AS name, COUNT(ct.card_id) AS card_count \
         FROM tags t LEFT JOIN card_tags ct ON ct.tag_id = t.id \
         GROUP BY t.id, t.name ORDER BY t.name ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| TagRow {
            id: row.get("id"),
            name: row.get("name"),
            card_count: row.get("card_count"),
        })
        .collect())
}

/// Replace a card's tag set with `tags` (normalized, deduped), returning the
/// normalized names actually stored.
///
/// One transaction: normalize + dedupe input, `INSERT OR IGNORE` each tag
/// (uuid id, so a re-used name reuses its existing row via the `UNIQUE`
/// constraint racing harmlessly with `OR IGNORE`), delete the card's current
/// `card_tags` rows, insert the new set, then delete any tag left with zero
/// references — tags are not manually managed, so an orphan would just be
/// clutter in `list_tags` forever.
pub async fn set_card_tags(
    pool: &sqlx::SqlitePool,
    card_id: &str,
    tags: &[String],
) -> Result<Vec<String>, AppError> {
    let mut normalized: Vec<String> = Vec::new();
    for raw in tags {
        if let Some(n) = normalize_tag(raw) {
            if !normalized.contains(&n) {
                normalized.push(n);
            }
        }
    }

    let mut tx = pool.begin().await?;
    let mut tag_ids: Vec<String> = Vec::with_capacity(normalized.len());
    for name in &normalized {
        let existing: Option<String> = sqlx::query_scalar("SELECT id FROM tags WHERE name = ?")
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?;
        let id = match existing {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                sqlx::query("INSERT OR IGNORE INTO tags(id, name, created_at) VALUES(?, ?, ?)")
                    .bind(&id)
                    .bind(name)
                    .bind(now_unix())
                    .execute(&mut *tx)
                    .await?;
                // Another writer may have raced this insert (UNIQUE(name));
                // re-read rather than trust the id we just tried to insert.
                sqlx::query_scalar("SELECT id FROM tags WHERE name = ?")
                    .bind(name)
                    .fetch_one(&mut *tx)
                    .await?
            }
        };
        tag_ids.push(id);
    }

    sqlx::query("DELETE FROM card_tags WHERE card_id = ?")
        .bind(card_id)
        .execute(&mut *tx)
        .await?;
    for tag_id in &tag_ids {
        sqlx::query("INSERT OR IGNORE INTO card_tags(card_id, tag_id) VALUES(?, ?)")
            .bind(card_id)
            .bind(tag_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM tags WHERE id NOT IN (SELECT DISTINCT tag_id FROM card_tags)")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(normalized)
}

/// Suspend or unsuspend a card. Upserts `card_flags`; `Err(BadInput)` when
/// the card does not exist (a suspend for a card that was just deleted out
/// from under the caller should be visible, not silently a no-op).
pub async fn set_card_suspended(
    pool: &sqlx::SqlitePool,
    card_id: &str,
    suspended: bool,
) -> Result<(), AppError> {
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM cards WHERE id = ?")
        .bind(card_id)
        .fetch_optional(pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::BadInput(format!("card not found: {card_id}")));
    }
    sqlx::query(
        "INSERT INTO card_flags(card_id, suspended, buried_until, updated_at) \
         VALUES(?, ?, 0, ?) \
         ON CONFLICT(card_id) DO UPDATE SET suspended = excluded.suspended, \
           updated_at = excluded.updated_at",
    )
    .bind(card_id)
    .bind(suspended as i64)
    .bind(now_unix())
    .execute(pool)
    .await?;
    Ok(())
}

/// Bury a card until `until` (unix seconds), returning the value stored.
/// `until <= now` clears the bury (stores `0`) rather than storing a
/// past timestamp that would trivially satisfy every due check anyway.
pub async fn bury_card(
    pool: &sqlx::SqlitePool,
    card_id: &str,
    until: i64,
) -> Result<i64, AppError> {
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM cards WHERE id = ?")
        .bind(card_id)
        .fetch_optional(pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::BadInput(format!("card not found: {card_id}")));
    }
    let now = now_unix();
    let stored = if until <= now { 0 } else { until };
    sqlx::query(
        "INSERT INTO card_flags(card_id, suspended, buried_until, updated_at) \
         VALUES(?, 0, ?, ?) \
         ON CONFLICT(card_id) DO UPDATE SET buried_until = excluded.buried_until, \
           updated_at = excluded.updated_at",
    )
    .bind(card_id)
    .bind(stored)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(stored)
}

/// Delete a card and all its child rows explicitly in one transaction.
///
/// SQLite FK is best-effort on the plugin-owned pool, so `ON DELETE
/// CASCADE` is NOT relied upon: `review_logs` → `card_memory_states` →
/// `cards` are deleted explicitly. Returns `BadInput` if the card id
/// does not exist.
pub async fn delete_card(pool: &sqlx::SqlitePool, card_id: &str) -> Result<(), AppError> {
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM cards WHERE id = ?")
        .bind(card_id)
        .fetch_optional(pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::BadInput(format!("card not found: {card_id}")));
    }
    let mut tx = pool.begin().await?;
    delete_card_children(&mut tx, card_id).await?;
    sqlx::query("DELETE FROM cards WHERE id = ?")
        .bind(card_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Aggregate review-history counts plus the optimizer gate.
///
/// `train_items` mirrors what [`build_train_set`] produces — each card with
/// `n >= 2` reviews yields `n - 1` prefix items — so the frontend can gate the
/// optimizer on the same number the optimizer itself checks.
pub async fn review_stats(pool: &sqlx::SqlitePool) -> Result<ReviewStats, AppError> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS distinct_cards, \
         COALESCE(SUM(cnt), 0) AS total_reviews, \
         COALESCE(SUM(CASE WHEN cnt >= 2 THEN 1 ELSE 0 END), 0) AS trainable_cards, \
         COALESCE(SUM(CASE WHEN cnt >= 2 THEN cnt - 1 ELSE 0 END), 0) AS train_items \
         FROM (SELECT card_id, COUNT(*) AS cnt FROM review_logs \
               WHERE rating BETWEEN 1 AND 4 GROUP BY card_id)",
    )
    .fetch_one(pool)
    .await?;
    let distinct: i64 = row.get("distinct_cards");
    let total: i64 = row.get("total_reviews");
    let trainable: i64 = row.get("trainable_cards");
    let items: i64 = row.get("train_items");
    Ok(ReviewStats {
        distinct_cards: distinct.max(0) as u64,
        total_reviews: total.max(0) as u64,
        trainable_cards: trainable.max(0) as u64,
        train_items: items.max(0) as u64,
        min_train_items: MIN_TRAIN_ITEMS,
        min_trainable_cards: MIN_TRAINABLE_CARDS,
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

/// Build the FSRS training set from ordered `(card_id, rating, delta_t)` rows.
///
/// `fsrs` expects one [`FSRSItem`] per review, each carrying that card's
/// preceding reviews as a prefix — "Each `FSRSItem` corresponds to a single
/// review, but contains the previous reviews of the card as well"
/// (`fsrs::dataset`). So a card with `n` valid reviews contributes `n - 1`
/// items of lengths `2..=n`, and a card with fewer than two contributes none.
///
/// The returned `card_ids` are index-aligned with the returned items and
/// densely numbered from 0, as `ComputeParametersInput::card_ids` requires.
///
/// `rows` must already be ordered by `(card_id, reviewed_at, rowid)`.
fn build_train_set(rows: &[(String, i64, i64)]) -> (Vec<FSRSItem>, Vec<i64>) {
    let mut groups: HashMap<&str, Vec<FSRSReview>> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for (card_id, rating, delta_t) in rows {
        if !(1..=4).contains(rating) {
            continue;
        }
        groups
            .entry(card_id.as_str())
            .or_insert_with(|| {
                order.push(card_id.as_str());
                Vec::new()
            })
            .push(FSRSReview {
                rating: *rating as u32,
                delta_t: (*delta_t).clamp(0, MAX_DELTA_T) as u32,
            });
    }

    let mut items: Vec<FSRSItem> = Vec::new();
    let mut card_ids: Vec<i64> = Vec::new();
    let mut next_dense: i64 = 0;
    for card_id in &order {
        let Some(reviews) = groups.get_mut(card_id) else {
            continue;
        };
        if reviews.len() < 2 {
            continue;
        }
        // The first review introduces the card, so it has no elapsed time.
        reviews[0].delta_t = 0;
        for k in 2..=reviews.len() {
            items.push(FSRSItem {
                reviews: reviews[..k].to_vec(),
            });
            card_ids.push(next_dense);
        }
        next_dense += 1;
    }
    (items, card_ids)
}

/// Optimize FSRS parameters from review history.
///
/// Loads all `review_logs` ordered by `card_id`, then `reviewed_at` (with a
/// `rowid` tie-break for same-timestamp rows) and expands them into prefix
/// items via [`build_train_set`]. Requires [`MIN_TRAIN_ITEMS`] items across
/// [`MIN_TRAINABLE_CARDS`] cards; less keeps the current parameters.
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
    let rows: Vec<(String, i64, i64)> = rows
        .into_iter()
        .map(|row| (row.get("card_id"), row.get("rating"), row.get("delta_t")))
        .collect();

    let (items, card_ids) = build_train_set(&rows);
    // Dense ids are contiguous from 0, so the last one sizes the set.
    let trainable = card_ids.last().map_or(0, |&last| last as u64 + 1);
    if (items.len() as u64) < MIN_TRAIN_ITEMS || trainable < MIN_TRAINABLE_CARDS {
        return Err(AppError::Scheduler(format!(
            "need at least {MIN_TRAIN_ITEMS} review histories across {MIN_TRAINABLE_CARDS} cards \
             to optimize (found {} across {trainable}); keeping current parameters",
            items.len()
        )));
    }

    let params = tokio::task::spawn_blocking(move || {
        compute_parameters(ComputeParametersInput {
            train_set: items,
            card_ids: Some(card_ids),
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
    // `app_settings` (migration 5) is authoritative; the REAL-valued
    // `settings` table stays readable for one release so a database that
    // stopped between migrations 4 and 5 still reports the user's real
    // choice instead of silently snapping back to the default.
    for sql in [
        "SELECT CAST(value AS REAL) FROM app_settings WHERE key = 'retention'",
        "SELECT value FROM settings WHERE key = 'retention'",
    ] {
        let value = sqlx::query_scalar::<_, f64>(sql)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            // SQLite CASTs unparseable TEXT to 0.0 rather than NULL, so a
            // junk value must fall through to the next source — clamping it
            // would silently pin retention to the bottom of the range.
            .filter(|v| v.is_finite() && *v > 0.0);
        if let Some(v) = value {
            return (v as f32).clamp(RETENTION_MIN, RETENTION_MAX);
        }
    }
    DEFAULT_RETENTION
}

/// Persist desired retention to `app_settings`.
///
/// The legacy `settings` table is deliberately not written: from migration 5
/// on it is read-only history, and keeping both in sync would just create two
/// sources of truth that can disagree.
pub async fn save_retention(pool: &sqlx::SqlitePool, retention: f32) -> Result<(), AppError> {
    if !(RETENTION_MIN..=RETENTION_MAX).contains(&retention) {
        return Err(AppError::BadInput(format!(
            "retention {retention} out of range: expected {RETENTION_MIN:.2}..={RETENTION_MAX:.2}"
        )));
    }
    sqlx::query(
        "INSERT INTO app_settings(key, value) VALUES('retention', ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(retention.to_string())
    .execute(pool)
    .await?;
    Ok(())
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

    /// Every DB test runs against the real migration chain (see
    /// `db::testing`) rather than a hand-written schema, so the tests cannot
    /// silently drift from what ships.
    async fn mem_pool() -> sqlx::SqlitePool {
        crate::db::testing::test_pool().await
    }

    #[tokio::test]
    async fn grade_is_atomic_rollback_on_invalid_rating() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('c1','default','F','B',0)")
            .execute(&pool).await.unwrap();
        // Invalid rating must not write memory or log rows.
        let err = grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "c1", 9, 0, 4).await;
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
        let _ = grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "c2", 3, 0, 4)
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
        let due = fetch_due_cards(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, DueOpts::limit(10))
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
        let _ = grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "intra", 1, 0, 4)
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
            let due = fetch_due_cards(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, DueOpts::limit(10))
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
        // Migration 4 seeds 'default'/'Default', which every real database
        // has, so the expected list is the two inserted decks plus it.
        let names: Vec<&str> = decks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Beta", "Default"]);
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
                empty.trainable_cards,
                empty.train_items
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(empty.min_train_items, MIN_TRAIN_ITEMS);
        assert_eq!(empty.min_trainable_cards, MIN_TRAINABLE_CARDS);
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
        // s1 has 2 reviews (1 prefix item), s2 has 1 (none): only s1 trains.
        assert_eq!(stats.trainable_cards, 1);
        assert_eq!(stats.train_items, 1);
    }

    /// `(card_id, rating, delta_t)` rows in the order the optimizer query returns.
    fn rows(spec: &[(&str, i64, i64)]) -> Vec<(String, i64, i64)> {
        spec.iter()
            .map(|(c, r, d)| ((*c).to_string(), *r, *d))
            .collect()
    }

    #[test]
    fn train_set_expands_one_item_per_review_with_prefix() {
        // The crate wants one FSRSItem per review carrying the prior reviews,
        // so 5 reviews must yield 4 items of lengths 2..=5 — not 1 item of 5.
        let input = rows(&[
            ("c1", 3, 0),
            ("c1", 3, 1),
            ("c1", 4, 3),
            ("c1", 2, 7),
            ("c1", 3, 15),
        ]);
        let (items, card_ids) = build_train_set(&input);
        let lengths: Vec<usize> = items.iter().map(|i| i.reviews.len()).collect();
        assert_eq!(lengths, vec![2, 3, 4, 5]);
        assert_eq!(card_ids, vec![0, 0, 0, 0]);
        // Every item is a prefix of the full history, in order.
        for item in &items {
            assert_eq!(item.reviews[1].rating, 3);
            assert_eq!(item.reviews[1].delta_t, 1);
        }
    }

    #[test]
    fn train_set_zeroes_first_delta_t_in_every_item() {
        let input = rows(&[("c1", 3, 99), ("c1", 3, 5), ("c1", 3, 9)]);
        let (items, _) = build_train_set(&input);
        assert_eq!(items.len(), 2);
        for item in &items {
            assert_eq!(
                item.reviews[0].delta_t, 0,
                "the introducing review has no elapsed time"
            );
        }
    }

    #[test]
    fn train_set_skips_cards_with_fewer_than_two_reviews() {
        let (items, card_ids) = build_train_set(&rows(&[("only", 3, 0)]));
        assert!(items.is_empty());
        assert!(card_ids.is_empty());
        assert!(build_train_set(&[]).0.is_empty());
    }

    #[test]
    fn train_set_separates_interleaved_cards_and_numbers_them_densely() {
        // "a" contributes 2 items, "b" 1, "solo" none — and the dense ids must
        // stay contiguous so `card_ids.last() + 1` counts trainable cards.
        let input = rows(&[
            ("a", 3, 0),
            ("a", 3, 2),
            ("a", 4, 5),
            ("solo", 3, 0),
            ("b", 1, 0),
            ("b", 3, 4),
        ]);
        let (items, card_ids) = build_train_set(&input);
        assert_eq!(items.len(), 3);
        assert_eq!(card_ids, vec![0, 0, 1]);
        assert_eq!(card_ids.len(), items.len());
        assert_eq!(card_ids.last().map_or(0, |&l| l as u64 + 1), 2);
        // The "b" item must carry b's reviews, not a's.
        assert_eq!(items[2].reviews[0].rating, 1);
        assert_eq!(items[2].reviews[1].delta_t, 4);
    }

    #[test]
    fn train_set_drops_invalid_ratings() {
        // A card whose only surviving review is one row contributes nothing.
        let input = rows(&[("c1", 0, 0), ("c1", 3, 1), ("c2", 3, 0), ("c2", 9, 2)]);
        let (items, card_ids) = build_train_set(&input);
        assert!(
            items.is_empty(),
            "both cards drop to a single valid review, got {items:?}"
        );
        assert!(card_ids.is_empty());
    }

    #[test]
    fn train_set_clamps_out_of_range_delta_t() {
        let input = rows(&[("c1", 3, 0), ("c1", 3, 99_999), ("c1", 3, -5)]);
        let (items, _) = build_train_set(&input);
        let last = items.last().expect("two items expected");
        assert_eq!(last.reviews[1].delta_t, MAX_DELTA_T as u32);
        assert_eq!(last.reviews[2].delta_t, 0, "negative deltas clamp to 0");
    }

    #[test]
    fn train_set_survives_epoch_poisoned_delta_t() {
        // The pre-fix demo seed wrote last_review_date = 0, so the first grade
        // persisted delta_t ~= days since the epoch. That value is under
        // MAX_DELTA_T, so it passes the clamp — it must at least not panic and
        // must still expand into well-formed prefix items.
        let input = rows(&[("c1", 3, 20_400), ("c1", 3, 1), ("c1", 3, 3)]);
        let (items, _) = build_train_set(&input);
        assert_eq!(items.len(), 2);
        // Zeroing the introducing review is what actually neutralizes it here.
        assert!(items.iter().all(|i| i.reviews[0].delta_t == 0));
    }

    #[test]
    fn elapsed_days_treats_unreviewed_as_zero() {
        let now = 1_700_000_000;
        assert_eq!(elapsed_days(now, 0), 0, "epoch means never reviewed");
        assert_eq!(elapsed_days(now, -1), 0);
        assert_eq!(elapsed_days(now, now), 0);
        assert_eq!(elapsed_days(now, now - 86_400 * 3), 3);
        // Clock rollback (last in the future) floors at 0.
        assert_eq!(elapsed_days(now, now + 86_400 * 5), 0);
        // Absurd clock skew saturates rather than overflowing.
        assert_eq!(elapsed_days(i64::MAX, 1), MAX_DELTA_T as u32);
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

    #[tokio::test]
    async fn retention_round_trips_through_app_settings() {
        let pool = mem_pool().await;
        assert_eq!(load_retention(&pool).await, 0.90);
        save_retention(&pool, 0.85).await.expect("save");
        assert_eq!(load_retention(&pool).await, 0.85);
        // The legacy REAL table is read-only from migration 5 on.
        let legacy =
            sqlx::query_scalar::<_, f64>("SELECT value FROM settings WHERE key='retention'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(legacy, 0.90, "settings must not be written any more");
    }

    #[tokio::test]
    async fn save_retention_rejects_out_of_range() {
        let pool = mem_pool().await;
        assert!(save_retention(&pool, 0.5).await.is_err());
        assert!(save_retention(&pool, 1.5).await.is_err());
        assert_eq!(load_retention(&pool).await, 0.90, "rejects must not write");
    }

    #[tokio::test]
    async fn load_retention_ignores_unparseable_text() {
        let pool = mem_pool().await;
        // CAST('lots' AS REAL) is 0.0, not NULL. Clamping that would pin
        // retention to 0.70; falling through to `settings` is correct.
        sqlx::query("UPDATE app_settings SET value='lots' WHERE key='retention'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(load_retention(&pool).await, 0.90);
    }

    #[tokio::test]
    async fn load_retention_falls_back_to_legacy_settings() {
        let pool = crate::db::testing::migrated_pool(4).await;
        sqlx::query("UPDATE settings SET value = 0.8 WHERE key = 'retention'")
            .execute(&pool)
            .await
            .unwrap();
        // A database that stopped between migrations 4 and 5 still reports
        // the user's choice rather than snapping back to the default.
        assert_eq!(load_retention(&pool).await, 0.8);
    }

    async fn card(pool: &sqlx::SqlitePool, id: &str, deck: &str) {
        sqlx::query("INSERT OR IGNORE INTO decks(id, name, created_at) VALUES(?, ?, 0)")
            .bind(deck)
            .bind(deck)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO cards (id, deck_id, content_front, content_back, created_at) \
             VALUES (?, ?, 'F', 'B', 0)",
        )
        .bind(id)
        .bind(deck)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn reviewed(pool: &sqlx::SqlitePool, id: &str, stability: f64, days_ago: i64) {
        let now = crate::db::now_unix();
        sqlx::query(
            "INSERT INTO card_memory_states \
             (card_id, stability, difficulty, last_review_date, next_due_date) \
             VALUES (?, ?, 5.0, ?, ?)",
        )
        .bind(id)
        .bind(stability)
        .bind(now - days_ago * 86_400)
        .bind(now - 60)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn due_cards_filters_by_deck_in_sql() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "a1", "alpha").await;
        card(&pool, "a2", "alpha").await;
        card(&pool, "b1", "beta").await;
        let due = fetch_due_cards(
            &pool,
            &fsrs,
            0.9,
            FSRS6_DEFAULT_DECAY,
            DueOpts {
                limit: 10,
                deck_id: Some("alpha"),
                exclude_card_id: None,
                caps: None,
            },
        )
        .await
        .unwrap();
        let mut ids: Vec<&str> = due.iter().map(|c| c.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a1", "a2"]);
        assert!(due.iter().all(|c| c.deck_id == "alpha"));
        assert_eq!(due[0].deck_name.as_deref(), Some("alpha"));
    }

    #[tokio::test]
    async fn due_cards_can_exclude_one_card() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "keep", "default").await;
        card(&pool, "skip", "default").await;
        let due = fetch_due_cards(
            &pool,
            &fsrs,
            0.9,
            FSRS6_DEFAULT_DECAY,
            DueOpts {
                limit: 10,
                deck_id: None,
                exclude_card_id: Some("skip"),
                caps: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            due.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["keep"]
        );
    }

    #[tokio::test]
    async fn grading_does_not_end_a_session_that_still_has_cards() {
        // The old code fetched the single top-due row and returned None when
        // it was the card just graded, which ended sessions early.
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "first", "default").await;
        card(&pool, "second", "default").await;
        let next = grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "first", 3, 0, 4)
            .await
            .unwrap();
        let next = next.expect("another card is still due");
        assert_eq!(next.id, "second");
    }

    #[tokio::test]
    async fn grading_the_last_card_reports_done() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "only", "default").await;
        let next = grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "only", 3, 0, 4)
            .await
            .unwrap();
        assert!(next.is_none());
    }

    #[tokio::test]
    async fn overdue_reviews_come_before_new_cards() {
        // New cards used to be pinned to R = 0.0, which made them outrank
        // every review no matter how overdue it was.
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "brand-new", "default").await;
        card(&pool, "badly-overdue", "default").await;
        reviewed(&pool, "badly-overdue", 2.0, 400).await;
        let due = fetch_due_cards(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, DueOpts::limit(10))
            .await
            .unwrap();
        assert_eq!(
            due[0].id,
            "badly-overdue",
            "got {:?}",
            due.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn suspended_and_buried_cards_are_not_due() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "normal", "default").await;
        card(&pool, "suspended", "default").await;
        card(&pool, "buried", "default").await;
        let later = crate::db::now_unix() + 86_400;
        sqlx::query("INSERT INTO card_flags(card_id, suspended, buried_until, updated_at) VALUES ('suspended',1,0,0), ('buried',0,?,0)")
            .bind(later)
            .execute(&pool)
            .await
            .unwrap();
        let due = fetch_due_cards(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, DueOpts::limit(10))
            .await
            .unwrap();
        assert_eq!(
            due.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["normal"]
        );
    }

    #[tokio::test]
    async fn create_and_rename_keep_id_and_name_separate() {
        let pool = mem_pool().await;
        let id = create_deck(&pool, "Spanish Verbs").await.unwrap();
        assert_ne!(id, "Spanish Verbs", "the id must not be the name");
        rename_deck(&pool, &id, "Verbs").await.unwrap();
        let decks = list_decks(&pool).await.unwrap();
        let found = decks.iter().find(|d| d.id == id).expect("deck");
        assert_eq!(found.name, "Verbs");
        assert!(create_deck(&pool, "   ").await.is_err());
        assert!(rename_deck(&pool, "nope", "X").await.is_err());
    }

    #[tokio::test]
    async fn delete_deck_moves_cards_to_default() {
        let pool = mem_pool().await;
        let id = create_deck(&pool, "Temp").await.unwrap();
        card(&pool, "c1", &id).await;
        let moved = delete_deck(&pool, &id, DeckDeleteMode::Move).await.unwrap();
        assert_eq!(moved, 1);
        let deck: String = sqlx::query_scalar("SELECT deck_id FROM cards WHERE id='c1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(deck, DEFAULT_DECK_ID);
    }

    #[tokio::test]
    async fn delete_deck_with_cards_clears_every_child_table() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        let id = create_deck(&pool, "Doomed").await.unwrap();
        card(&pool, "c1", &id).await;
        grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "c1", 3, 0, 4)
            .await
            .unwrap();
        sqlx::query("INSERT INTO card_flags(card_id, suspended, buried_until, updated_at) VALUES ('c1',1,0,0)")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            delete_deck(&pool, &id, DeckDeleteMode::Delete)
                .await
                .unwrap(),
            1
        );
        // Cascade is unreliable on this pool, so every child must be cleared
        // explicitly — a leftover row would resurrect as an orphan.
        for table in [
            "cards",
            "review_logs",
            "card_memory_states",
            "card_flags",
            "attempt_cards",
        ] {
            let n: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM {table} WHERE {} = 'c1'",
                if table == "cards" { "id" } else { "card_id" }
            ))
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(n, 0, "{table} still references the deleted card");
        }
    }

    #[tokio::test]
    async fn the_default_deck_cannot_be_deleted() {
        let pool = mem_pool().await;
        // Deleting it would leave "move" with nowhere to move cards to.
        assert!(delete_deck(&pool, DEFAULT_DECK_ID, DeckDeleteMode::Move)
            .await
            .is_err());
        assert!(delete_deck(&pool, "missing", DeckDeleteMode::Move)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn update_card_keeps_scheduling_history() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "c1", "default").await;
        grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "c1", 3, 0, 4)
            .await
            .unwrap();
        update_card(&pool, "c1", "new front", "new back")
            .await
            .unwrap();
        let (front, back) = sqlx::query_as::<_, (String, String)>(
            "SELECT content_front, content_back FROM cards WHERE id='c1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((front.as_str(), back.as_str()), ("new front", "new back"));
        let logs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM review_logs WHERE card_id='c1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(logs, 1, "editing text must not reset scheduling");
        assert!(update_card(&pool, "c1", "", "x").await.is_err());
        assert!(update_card(&pool, "missing", "a", "b").await.is_err());
    }

    #[test]
    fn deck_delete_mode_rejects_anything_else() {
        assert_eq!(DeckDeleteMode::parse("move").unwrap(), DeckDeleteMode::Move);
        assert_eq!(
            DeckDeleteMode::parse("delete").unwrap(),
            DeckDeleteMode::Delete
        );
        assert!(DeckDeleteMode::parse("destroy").is_err());
    }

    #[test]
    fn fetch_window_always_exceeds_the_limit() {
        // If the window equalled the limit the R sort would be a no-op:
        // sorting exactly the rows you return changes nothing.
        for limit in [1_i64, 5, 20, 100, 10_000] {
            assert!(fetch_window(limit) > limit, "window too small for {limit}");
        }
        assert_eq!(fetch_window(i64::MAX), i64::MAX, "must not overflow");
    }

    // ─── B1: tags ──────────────────────────────────────────────────────────

    #[test]
    fn normalize_tag_collapses_and_lowercases() {
        assert_eq!(
            normalize_tag("  Verb  Phrase "),
            Some("verb-phrase".to_string())
        );
        assert_eq!(normalize_tag("   "), None);
        assert_eq!(normalize_tag(""), None);
        assert_eq!(normalize_tag("Solo"), Some("solo".to_string()));
    }

    #[tokio::test]
    async fn set_card_tags_replaces_rather_than_appends() {
        let pool = mem_pool().await;
        card(&pool, "t1", "default").await;
        let stored = set_card_tags(&pool, "t1", &["Verb".to_string(), "Hard".to_string()])
            .await
            .unwrap();
        assert_eq!(stored, vec!["verb".to_string(), "hard".to_string()]);

        // Replacing with a different set must drop the old tags, not add to
        // them.
        let stored2 = set_card_tags(&pool, "t1", &["Noun".to_string()])
            .await
            .unwrap();
        assert_eq!(stored2, vec!["noun".to_string()]);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM card_tags WHERE card_id='t1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn set_card_tags_removes_orphan_tags() {
        let pool = mem_pool().await;
        card(&pool, "t1", "default").await;
        set_card_tags(&pool, "t1", &["only-here".to_string()])
            .await
            .unwrap();
        // Reassigning to a different tag must leave the old one orphaned
        // and therefore removed.
        set_card_tags(&pool, "t1", &["elsewhere".to_string()])
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE name='only-here'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "orphan tag must be deleted");
    }

    #[tokio::test]
    async fn set_card_tags_dedupes_and_ignores_blank_entries() {
        let pool = mem_pool().await;
        card(&pool, "t1", "default").await;
        let stored = set_card_tags(
            &pool,
            "t1",
            &["Verb".to_string(), "  verb ".to_string(), "   ".to_string()],
        )
        .await
        .unwrap();
        assert_eq!(stored, vec!["verb".to_string()]);
    }

    #[tokio::test]
    async fn list_tags_reports_live_counts() {
        let pool = mem_pool().await;
        card(&pool, "t1", "default").await;
        card(&pool, "t2", "default").await;
        set_card_tags(&pool, "t1", &["shared".to_string()])
            .await
            .unwrap();
        set_card_tags(&pool, "t2", &["shared".to_string()])
            .await
            .unwrap();
        let tags = list_tags(&pool).await.unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "shared");
        assert_eq!(tags[0].card_count, 2);
    }

    // ─── B1: suspend / bury ─────────────────────────────────────────────────

    #[tokio::test]
    async fn suspend_card_toggles_and_rejects_missing_card() {
        let pool = mem_pool().await;
        card(&pool, "s1", "default").await;
        set_card_suspended(&pool, "s1", true).await.unwrap();
        let suspended: i64 =
            sqlx::query_scalar("SELECT suspended FROM card_flags WHERE card_id='s1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(suspended, 1);
        set_card_suspended(&pool, "s1", false).await.unwrap();
        let suspended: i64 =
            sqlx::query_scalar("SELECT suspended FROM card_flags WHERE card_id='s1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(suspended, 0);
        assert!(set_card_suspended(&pool, "missing", true).await.is_err());
    }

    #[tokio::test]
    async fn bury_card_clears_with_a_non_positive_until() {
        let pool = mem_pool().await;
        card(&pool, "b1", "default").await;
        let now = crate::db::now_unix();
        let until = bury_card(&pool, "b1", now + 3600).await.unwrap();
        assert!(until > now);
        let cleared = bury_card(&pool, "b1", now).await.unwrap();
        assert_eq!(cleared, 0);
        assert!(bury_card(&pool, "missing", now + 100).await.is_err());
    }

    // ─── B1: daily caps ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn daily_caps_defaults_to_the_global_app_settings() {
        let pool = mem_pool().await;
        let caps = daily_caps(&pool, None, 0, 4).await.unwrap();
        assert_eq!(caps.new_left, DEFAULT_NEW_PER_DAY);
        assert_eq!(caps.rev_left, DEFAULT_REVIEW_PER_DAY);
    }

    #[tokio::test]
    async fn daily_caps_counts_first_review_as_new_and_later_as_review() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "cap1", "default").await;
        // First grade: introduces the card, counts against `new`.
        grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "cap1", 3, 0, 4)
            .await
            .unwrap();
        let caps = daily_caps(&pool, None, 0, 4).await.unwrap();
        assert_eq!(caps.new_left, DEFAULT_NEW_PER_DAY - 1);
        assert_eq!(caps.rev_left, DEFAULT_REVIEW_PER_DAY);
        // Second grade of the same card: counts against `review`.
        grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "cap1", 3, 0, 4)
            .await
            .unwrap();
        let caps = daily_caps(&pool, None, 0, 4).await.unwrap();
        assert_eq!(caps.new_left, DEFAULT_NEW_PER_DAY - 1);
        assert_eq!(caps.rev_left, DEFAULT_REVIEW_PER_DAY - 1);
    }

    #[tokio::test]
    async fn daily_caps_floor_at_zero_with_a_low_cap() {
        let pool = mem_pool().await;
        set_daily_limits(&pool, None, 2, 2).await.unwrap();
        let fsrs = FSRS::default();
        card(&pool, "cap-a", "default").await;
        card(&pool, "cap-b", "default").await;
        // Two "first" reviews consume the whole new cap.
        grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "cap-a", 3, 0, 4)
            .await
            .unwrap();
        grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "cap-b", 3, 0, 4)
            .await
            .unwrap();
        let caps = daily_caps(&pool, None, 0, 4).await.unwrap();
        assert_eq!(caps.new_left, 0);
        assert_eq!(
            caps.rev_left, 2,
            "reviews cap untouched by new introductions"
        );
    }

    #[tokio::test]
    async fn get_and_set_daily_limits_round_trip_globally_and_per_deck() {
        let pool = mem_pool().await;
        let global = get_daily_limits(&pool, None).await.unwrap();
        assert_eq!(global.new_per_day, DEFAULT_NEW_PER_DAY);
        set_daily_limits(&pool, None, 5, 55).await.unwrap();
        let global = get_daily_limits(&pool, None).await.unwrap();
        assert_eq!((global.new_per_day, global.review_per_day), (5, 55));

        card(&pool, "d1", "alpha").await;
        // A deck with no override still reads the (now-changed) global.
        let per_deck = get_daily_limits(&pool, Some("alpha")).await.unwrap();
        assert_eq!((per_deck.new_per_day, per_deck.review_per_day), (5, 55));
        set_daily_limits(&pool, Some("alpha"), 3, 30).await.unwrap();
        let per_deck = get_daily_limits(&pool, Some("alpha")).await.unwrap();
        assert_eq!((per_deck.new_per_day, per_deck.review_per_day), (3, 30));
        // The global default must be unaffected by the per-deck override.
        let global = get_daily_limits(&pool, None).await.unwrap();
        assert_eq!((global.new_per_day, global.review_per_day), (5, 55));
    }

    #[tokio::test]
    async fn fetch_due_cards_honours_review_cap() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        for i in 0..5 {
            let id = format!("rev{i}");
            card(&pool, &id, "default").await;
            reviewed(&pool, &id, 2.0, 400).await;
        }
        let due = fetch_due_cards(
            &pool,
            &fsrs,
            0.9,
            FSRS6_DEFAULT_DECAY,
            DueOpts {
                limit: 10,
                deck_id: None,
                exclude_card_id: None,
                caps: Some(DailyCaps {
                    new_left: 100,
                    rev_left: 2,
                }),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            due.len(),
            2,
            "rev_left=2 must cap reviews even though 5 are due"
        );
    }

    #[tokio::test]
    async fn fetch_due_cards_honours_new_cap_even_with_room_in_limit() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "brand-new-1", "default").await;
        card(&pool, "brand-new-2", "default").await;
        let due = fetch_due_cards(
            &pool,
            &fsrs,
            0.9,
            FSRS6_DEFAULT_DECAY,
            DueOpts {
                limit: 10,
                deck_id: None,
                exclude_card_id: None,
                caps: Some(DailyCaps {
                    new_left: 0,
                    rev_left: 100,
                }),
            },
        )
        .await
        .unwrap();
        assert!(
            due.is_empty(),
            "new_left=0 must exclude new cards even though limit has room"
        );
    }

    #[tokio::test]
    async fn fetch_due_cards_still_puts_reviews_before_new_cards_under_caps() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "capped-new", "default").await;
        card(&pool, "capped-review", "default").await;
        reviewed(&pool, "capped-review", 2.0, 400).await;
        let due = fetch_due_cards(
            &pool,
            &fsrs,
            0.9,
            FSRS6_DEFAULT_DECAY,
            DueOpts {
                limit: 10,
                deck_id: None,
                exclude_card_id: None,
                caps: Some(DailyCaps {
                    new_left: 5,
                    rev_left: 5,
                }),
            },
        )
        .await
        .unwrap();
        assert_eq!(due[0].id, "capped-review");
        assert_eq!(due[1].id, "capped-new");
    }

    // ─── B1: undo ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn undo_after_a_new_cards_first_review_makes_it_new_again() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "u1", "default").await;
        grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "u1", 3, 0, 4)
            .await
            .unwrap();
        let undone = undo_last_review(&pool)
            .await
            .unwrap()
            .expect("something to undo");
        assert_eq!(undone.card_id, "u1");
        assert_eq!(undone.rating, 3);

        let mem: Option<sqlx::sqlite::SqliteRow> =
            sqlx::query("SELECT * FROM card_memory_states WHERE card_id='u1'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(
            mem.is_none(),
            "undoing the first review must remove the memory row"
        );
        let logs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM review_logs WHERE card_id='u1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(logs, 0);

        let due = fetch_due_cards(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, DueOpts::limit(10))
            .await
            .unwrap();
        assert!(
            due.iter().any(|c| c.id == "u1"),
            "the card must be due again after undo"
        );
    }

    #[tokio::test]
    async fn undo_restores_the_prior_review_exactly() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        card(&pool, "u2", "default").await;
        grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "u2", 3, 0, 4)
            .await
            .unwrap();
        let (first_stability, first_due): (f64, i64) = sqlx::query_as(
            "SELECT stability, next_due_date FROM card_memory_states WHERE card_id='u2'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "u2", 4, 0, 4)
            .await
            .unwrap();

        let undone = undo_last_review(&pool)
            .await
            .unwrap()
            .expect("something to undo");
        assert_eq!(undone.rating, 4);

        let (restored_stability, restored_due): (f64, i64) = sqlx::query_as(
            "SELECT stability, next_due_date FROM card_memory_states WHERE card_id='u2'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(restored_stability, first_stability);
        assert_eq!(restored_due, first_due);
        let logs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM review_logs WHERE card_id='u2'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(logs, 1, "only the second review's log must remain");
    }

    #[tokio::test]
    async fn undo_with_nothing_to_undo_is_none() {
        let pool = mem_pool().await;
        assert!(undo_last_review(&pool).await.unwrap().is_none());
    }

    // ─── B1: grown row types ─────────────────────────────────────────────────

    #[tokio::test]
    async fn list_decks_reports_card_due_and_new_counts() {
        let pool = mem_pool().await;
        let fsrs = FSRS::default();
        let id = create_deck(&pool, "Counted").await.unwrap();
        card(&pool, "lc1", &id).await;
        card(&pool, "lc2", &id).await;
        reviewed(&pool, "lc1", 2.0, 400).await; // due (overdue)
        grade_card_db(&pool, &fsrs, 0.9, FSRS6_DEFAULT_DECAY, "lc1", 3, 0, 4)
            .await
            .unwrap();
        let decks = list_decks(&pool).await.unwrap();
        let found = decks.iter().find(|d| d.id == id).expect("deck");
        assert_eq!(found.card_count, 2);
        assert_eq!(found.new_count, 1, "lc2 has no memory row");
    }

    #[tokio::test]
    async fn list_cards_includes_tags_and_flags() {
        let pool = mem_pool().await;
        card(&pool, "lc-tag", "default").await;
        set_card_tags(&pool, "lc-tag", &["Alpha".to_string(), "Beta".to_string()])
            .await
            .unwrap();
        set_card_suspended(&pool, "lc-tag", true).await.unwrap();
        let cards = list_cards(&pool, None).await.unwrap();
        let found = cards.iter().find(|c| c.id == "lc-tag").expect("card");
        let mut tags = found.tags.clone();
        tags.sort();
        assert_eq!(tags, vec!["alpha".to_string(), "beta".to_string()]);
        assert!(found.suspended);
        assert_eq!(found.buried_until, 0);
    }
}
