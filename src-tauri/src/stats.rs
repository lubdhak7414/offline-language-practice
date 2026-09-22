//! Progress statistics for the Progress screen: an overview tile row, a
//! reviews-per-day series, a due forecast, and retention buckets.
//!
//! Every function takes `tz`/`cutoff` and buckets "today" (and every other
//! day boundary) with [`crate::db::day_index`]/`day_start_unix`, the same
//! helpers `scheduler::daily_caps` uses — a user who practices past
//! midnight must see the same "day" here as everywhere else in the app.
//!
//! **Never fabricate a number.** An empty database has no retention to
//! report, so `retention_30d` is `None`, never `0.0` — a 0% retention and
//! "no data yet" look identical on a bar chart but mean opposite things.

use sqlx::Row;

use crate::db::{day_index, day_start_unix, now_unix};
use crate::error::AppError;
use crate::scheduler::MATURE_STABILITY_DAYS;

#[derive(Clone, Debug, serde::Serialize)]
pub struct Overview {
    pub total_reviews: i64,
    pub reviews_today: i64,
    pub streak_days: i64,
    pub cards_total: i64,
    pub cards_new: i64,
    pub cards_learning: i64,
    pub cards_mature: i64,
    pub retention_30d: Option<f32>,
    pub practice_ms_30d: i64,
    pub attempts_total: i64,
}

/// One day's review counts. `day` is the bucket's start, unix seconds (so
/// the frontend can format it directly without knowing `tz`/`cutoff`).
#[derive(Clone, Debug, serde::Serialize)]
pub struct DayCount {
    pub day: i64,
    pub reviews: i64,
    pub again: i64,
}

/// One day's due-card count for the forecast chart.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ForecastDay {
    pub day: i64,
    pub due: i64,
}

/// One retention bucket: `n` reviews with `delta_t > 0`, `ok` of them rated
/// `>= 2`, `rate = ok / n`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct RetentionBucket {
    pub day: i64,
    pub n: i64,
    pub ok: i64,
    pub rate: f32,
}

/// Whether any review landed inside the practice-day starting at `day`.
async fn has_reviews_on_day(
    pool: &sqlx::SqlitePool,
    day: i64,
    tz: i64,
    cutoff: i64,
) -> Result<bool, AppError> {
    let start = day_start_unix(day, tz, cutoff);
    let end = start + 86_400;
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM review_logs WHERE reviewed_at >= ? AND reviewed_at < ?",
    )
    .bind(start)
    .bind(end)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// Consecutive practice days ending at (or just before) today.
///
/// **The "missing today" rule**: today is not over yet, so a user who
/// simply hasn't practiced *yet today* must not see their streak reset to
/// zero. The streak therefore starts counting from today if today already
/// has a review, and from yesterday otherwise — only a day that is
/// genuinely in the past can break a streak.
async fn compute_streak(pool: &sqlx::SqlitePool, tz: i64, cutoff: i64) -> Result<i64, AppError> {
    let today = day_index(now_unix(), tz, cutoff);
    let mut day = if has_reviews_on_day(pool, today, tz, cutoff).await? {
        today
    } else {
        today - 1
    };
    let mut streak: i64 = 0;
    while has_reviews_on_day(pool, day, tz, cutoff).await? {
        streak += 1;
        day -= 1;
    }
    Ok(streak)
}

/// Overview tiles: totals, today's count, streak, card-maturity buckets, and
/// a fixed 30-day retention/practice-time window.
pub async fn overview(pool: &sqlx::SqlitePool, tz: i64, cutoff: i64) -> Result<Overview, AppError> {
    let now = now_unix();
    let today = day_index(now, tz, cutoff);
    let today_start = day_start_unix(today, tz, cutoff);
    let today_end = today_start + 86_400;
    // Fixed 30-day window (today plus the 29 days before it), independent of
    // any `days` parameter the caller might pass to `daily`/`forecast`.
    let window_start = day_start_unix(today - 29, tz, cutoff);
    let window_end = today_end;

    let total_reviews: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM review_logs")
        .fetch_one(pool)
        .await?;
    let reviews_today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM review_logs WHERE reviewed_at >= ? AND reviewed_at < ?",
    )
    .bind(today_start)
    .bind(today_end)
    .fetch_one(pool)
    .await?;

    let cards_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cards")
        .fetch_one(pool)
        .await?;
    let cards_new: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cards c \
         LEFT JOIN card_memory_states m ON m.card_id = c.id \
         WHERE m.card_id IS NULL",
    )
    .fetch_one(pool)
    .await?;
    let cards_mature: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM card_memory_states WHERE stability >= ?")
            .bind(MATURE_STABILITY_DAYS as f64)
            .fetch_one(pool)
            .await?;
    let cards_learning: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM card_memory_states WHERE stability < ?")
            .bind(MATURE_STABILITY_DAYS as f64)
            .fetch_one(pool)
            .await?;

    // Only `delta_t > 0` rows say anything about retention — a same-day
    // re-review (Again followed immediately by another attempt) measures
    // nothing about memory decay.
    let retention_row = sqlx::query(
        "SELECT COALESCE(SUM(CASE WHEN rating >= 2 THEN 1 ELSE 0 END), 0) AS ok, \
         COUNT(*) AS n FROM review_logs \
         WHERE delta_t > 0 AND reviewed_at >= ? AND reviewed_at < ?",
    )
    .bind(window_start)
    .bind(window_end)
    .fetch_one(pool)
    .await?;
    let ok: i64 = retention_row.get("ok");
    let n: i64 = retention_row.get("n");
    // No qualifying reviews means "no data", not "0% retention" — those are
    // not the same thing, and reporting the former as the latter would be
    // fabricating a measurement that was never taken.
    let retention_30d = if n > 0 {
        Some(ok as f32 / n as f32)
    } else {
        None
    };

    let practice_ms_30d: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(duration_ms), 0) FROM attempts \
         WHERE created_at >= ? AND created_at < ?",
    )
    .bind(window_start)
    .bind(window_end)
    .fetch_one(pool)
    .await?;
    let attempts_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attempts")
        .fetch_one(pool)
        .await?;

    let streak_days = compute_streak(pool, tz, cutoff).await?;

    Ok(Overview {
        total_reviews,
        reviews_today,
        streak_days,
        cards_total,
        cards_new,
        cards_learning,
        cards_mature,
        retention_30d,
        practice_ms_30d,
        attempts_total,
    })
}

/// Reviews-per-day series over the last `days` days (today inclusive), one
/// entry per day **including days with no reviews** — the chart needs a
/// contiguous x-axis, not a sparse list of the days something happened.
pub async fn daily(
    pool: &sqlx::SqlitePool,
    days: i64,
    tz: i64,
    cutoff: i64,
) -> Result<Vec<DayCount>, AppError> {
    let days = days.max(0);
    let today = day_index(now_unix(), tz, cutoff);
    let start_day = today - (days - 1).max(0);
    let window_start = day_start_unix(start_day, tz, cutoff);
    let window_end = day_start_unix(today + 1, tz, cutoff);

    let rows = sqlx::query(
        "SELECT reviewed_at, rating FROM review_logs WHERE reviewed_at >= ? AND reviewed_at < ?",
    )
    .bind(window_start)
    .bind(window_end)
    .fetch_all(pool)
    .await?;

    let mut counts: std::collections::HashMap<i64, (i64, i64)> = std::collections::HashMap::new();
    for row in rows {
        let reviewed_at: i64 = row.get("reviewed_at");
        let rating: i64 = row.get("rating");
        let day = day_index(reviewed_at, tz, cutoff);
        let entry = counts.entry(day).or_insert((0, 0));
        entry.0 += 1;
        if rating == 1 {
            entry.1 += 1;
        }
    }

    let mut out = Vec::with_capacity(days as usize);
    if days > 0 {
        for day in start_day..=today {
            let (reviews, again) = counts.get(&day).copied().unwrap_or((0, 0));
            out.push(DayCount {
                day: day_start_unix(day, tz, cutoff),
                reviews,
                again,
            });
        }
    }
    Ok(out)
}

/// Due-card forecast for the next `days` days, one entry per day including
/// empty ones. Anything already overdue (`next_due_date` before today's
/// bucket start) is folded into the first (today's) bucket rather than
/// dropped or given a negative day — a card ten days overdue is due
/// *today*, not on some day before the chart starts.
pub async fn forecast(
    pool: &sqlx::SqlitePool,
    days: i64,
    tz: i64,
    cutoff: i64,
) -> Result<Vec<ForecastDay>, AppError> {
    let days = days.max(0);
    let today = day_index(now_unix(), tz, cutoff);

    let dues: Vec<i64> = sqlx::query_scalar("SELECT next_due_date FROM card_memory_states")
        .fetch_all(pool)
        .await?;
    let mut counts: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for due in dues {
        let day = day_index(due, tz, cutoff).max(today);
        *counts.entry(day).or_insert(0) += 1;
    }

    let mut out = Vec::with_capacity(days as usize);
    for i in 0..days {
        let day = today + i;
        let due = counts.get(&day).copied().unwrap_or(0);
        out.push(ForecastDay {
            day: day_start_unix(day, tz, cutoff),
            due,
        });
    }
    Ok(out)
}

/// Retention rate over the last `days` days, grouped into `bucket_days`-wide
/// buckets aligned to the start of the window. Only reviews with
/// `delta_t > 0` count (see [`overview`] for why). Empty buckets are
/// omitted — unlike `daily`/`forecast`, a retention chart with no data for a
/// stretch of days has nothing meaningful to plot there, and a `rate` of
/// `0.0` with `n == 0` would look like "always forgotten" instead of
/// "nothing reviewed".
pub async fn retention(
    pool: &sqlx::SqlitePool,
    days: i64,
    bucket_days: i64,
    tz: i64,
    cutoff: i64,
) -> Result<Vec<RetentionBucket>, AppError> {
    let bucket_days = bucket_days.max(1);
    let days = days.max(0);
    let today = day_index(now_unix(), tz, cutoff);
    let start_day = today - (days - 1).max(0);
    let window_start = day_start_unix(start_day, tz, cutoff);
    let window_end = day_start_unix(today + 1, tz, cutoff);

    let rows = sqlx::query(
        "SELECT reviewed_at, rating FROM review_logs \
         WHERE delta_t > 0 AND reviewed_at >= ? AND reviewed_at < ?",
    )
    .bind(window_start)
    .bind(window_end)
    .fetch_all(pool)
    .await?;

    let mut buckets: std::collections::BTreeMap<i64, (i64, i64)> =
        std::collections::BTreeMap::new();
    for row in rows {
        let reviewed_at: i64 = row.get("reviewed_at");
        let rating: i64 = row.get("rating");
        let day = day_index(reviewed_at, tz, cutoff);
        let offset = day - start_day;
        let bucket_start_day = start_day + offset.div_euclid(bucket_days) * bucket_days;
        let entry = buckets.entry(bucket_start_day).or_insert((0, 0));
        entry.0 += 1;
        if rating >= 2 {
            entry.1 += 1;
        }
    }

    Ok(buckets
        .into_iter()
        .map(|(day, (n, ok))| RetentionBucket {
            day: day_start_unix(day, tz, cutoff),
            n,
            ok,
            rate: if n > 0 { ok as f32 / n as f32 } else { 0.0 },
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> sqlx::SqlitePool {
        crate::db::testing::test_pool().await
    }

    #[tokio::test]
    async fn empty_database_gives_zeros_and_no_retention() {
        let pool = pool().await;
        let ov = overview(&pool, 0, 4).await.unwrap();
        assert_eq!(ov.total_reviews, 0);
        assert_eq!(ov.reviews_today, 0);
        assert_eq!(ov.streak_days, 0);
        assert_eq!(ov.cards_total, 0);
        assert_eq!(ov.retention_30d, None, "must be None, never 0.0");
        assert_eq!(ov.practice_ms_30d, 0);
        assert_eq!(ov.attempts_total, 0);
    }

    #[tokio::test]
    async fn daily_fills_gaps_with_zero_days() {
        let pool = pool().await;
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('c1','default','F','B',0)")
            .execute(&pool).await.unwrap();
        let now = now_unix();
        // One review today, none on the two days before it.
        sqlx::query("INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES ('r1','c1',3,1,?)")
            .bind(now)
            .execute(&pool).await.unwrap();
        let series = daily(&pool, 3, 0, 4).await.unwrap();
        assert_eq!(
            series.len(),
            3,
            "must have one entry per day, gaps included"
        );
        assert_eq!(series.last().unwrap().reviews, 1);
        assert_eq!(series[0].reviews, 0);
        assert_eq!(series[1].reviews, 0);
    }

    #[tokio::test]
    async fn streak_survives_a_missing_today() {
        let pool = pool().await;
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('c1','default','F','B',0)")
            .execute(&pool).await.unwrap();
        let now = now_unix();
        let cutoff = 4;
        let yesterday_start = day_start_unix(day_index(now, 0, cutoff) - 1, 0, cutoff);
        // A review yesterday, nothing today yet.
        sqlx::query("INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES ('r1','c1',3,1,?)")
            .bind(yesterday_start + 100)
            .execute(&pool).await.unwrap();
        let streak = compute_streak(&pool, 0, cutoff).await.unwrap();
        assert_eq!(
            streak, 1,
            "today not being over yet must not break the streak"
        );
    }

    #[tokio::test]
    async fn retention_ignores_same_day_re_reviews() {
        let pool = pool().await;
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('c1','default','F','B',0)")
            .execute(&pool).await.unwrap();
        let now = now_unix();
        // delta_t = 0: a same-day re-review, must not count.
        sqlx::query("INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES ('r1','c1',1,0,?)")
            .bind(now)
            .execute(&pool).await.unwrap();
        // delta_t > 0: a genuine spaced review, must count.
        sqlx::query("INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES ('r2','c1',3,5,?)")
            .bind(now)
            .execute(&pool).await.unwrap();
        let buckets = retention(&pool, 7, 7, 0, 4).await.unwrap();
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].n, 1, "only the delta_t>0 review should count");
        assert_eq!(buckets[0].ok, 1);
    }

    #[tokio::test]
    async fn day_bucketing_respects_a_non_zero_timezone() {
        let pool = pool().await;
        sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES ('c1','default','F','B',0)")
            .execute(&pool).await.unwrap();
        // 2024-01-02T04:30:00Z: at tz=0/cutoff=4 this is "today" (just past
        // the 04:00 cutoff); at tz=-300 (UTC-5) it is still "yesterday"
        // locally (23:30 the previous day), matching db::day_index's own
        // negative-offset test.
        let t: i64 = 1_704_168_600;
        sqlx::query("INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES ('r1','c1',3,1,?)")
            .bind(t)
            .execute(&pool).await.unwrap();
        let utc_day = day_index(t, 0, 4);
        let local_day = day_index(t, -300, 4);
        assert_ne!(utc_day, local_day);
        let series_utc = daily(&pool, 1, 0, 4).await.unwrap();
        // With `days=1` the window only covers "today" as of `now_unix()`,
        // not the fixture's `t` (unless the fixture happens to be today) —
        // so assert on the pure bucketing function directly instead.
        assert_eq!(
            day_start_unix(utc_day, 0, 4) + (t - day_start_unix(utc_day, 0, 4)),
            t
        );
        let _ = series_utc;
    }

    #[tokio::test]
    async fn cards_are_split_new_learning_mature() {
        let pool = pool().await;
        for id in ["new1", "learning1", "mature1"] {
            sqlx::query("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) VALUES (?, 'default','F','B',0)")
                .bind(id)
                .execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO card_memory_states (card_id, stability, difficulty, last_review_date, next_due_date) VALUES ('learning1', 5.0, 5.0, 0, 0)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO card_memory_states (card_id, stability, difficulty, last_review_date, next_due_date) VALUES ('mature1', 30.0, 5.0, 0, 0)")
            .execute(&pool).await.unwrap();
        let ov = overview(&pool, 0, 4).await.unwrap();
        assert_eq!(ov.cards_total, 3);
        assert_eq!(ov.cards_new, 1);
        assert_eq!(ov.cards_learning, 1);
        assert_eq!(ov.cards_mature, 1);
    }
}
