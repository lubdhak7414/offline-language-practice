//! Subsystem 5: embedded SQLite via tauri-plugin-sql.
//!
//! - WAL mode for concurrent reads during review-log writes (enabled once).
//! - Declarative migrations at startup (1..=7; see `MIGRATION_N_SQL` below).
//! - All access inside async DB worker tasks (never the webview thread).

use tauri::State;
use tauri_plugin_sql::{DbInstances, DbPool};

use crate::error::AppError;

pub const DB_URL: &str = "sqlite:app.db";

/// Migration 1: core cards + memory states.
pub const MIGRATION_1_SQL: &str = "
CREATE TABLE IF NOT EXISTS cards (
  id TEXT PRIMARY KEY NOT NULL,
  deck_id TEXT NOT NULL,
  content_front TEXT NOT NULL,
  content_back TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS card_memory_states (
  card_id TEXT PRIMARY KEY NOT NULL,
  stability REAL NOT NULL,
  difficulty REAL NOT NULL,
  last_review_date INTEGER NOT NULL,
  FOREIGN KEY(card_id) REFERENCES cards(id) ON DELETE CASCADE
);";

/// Migration 2: FSRS review history.
pub const MIGRATION_2_SQL: &str = "
CREATE TABLE IF NOT EXISTS review_logs (
  id TEXT PRIMARY KEY NOT NULL,
  card_id TEXT NOT NULL,
  rating INTEGER NOT NULL,
  delta_t INTEGER NOT NULL,
  reviewed_at INTEGER NOT NULL,
  FOREIGN KEY(card_id) REFERENCES cards(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_review_logs_card ON review_logs(card_id);";

/// Migration 3: retention setting + next-due column.
///
/// `ALTER TABLE ... ADD COLUMN` with a non-null default is fine on SQLite,
/// and tauri-plugin-sql tracks applied versions so this runs once.
pub const MIGRATION_3_SQL: &str = "
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY NOT NULL,value REAL NOT NULL);
INSERT OR IGNORE INTO settings(key,value) VALUES('retention',0.90);
ALTER TABLE card_memory_states ADD COLUMN next_due_date INTEGER NOT NULL DEFAULT 0;";

/// Migration 4: optimizer params, decks, due index, deck normalization.
///
/// All statements are idempotent-safe (`IF NOT EXISTS` / `OR IGNORE` /
/// idempotent `UPDATE`s) so re-running is a no-op. No bare `ADD COLUMN`
/// here (migration 3's bare `ADD COLUMN` is left untouched for history).
pub const MIGRATION_4_SQL: &str = "
CREATE TABLE IF NOT EXISTS fsrs_params(params_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS decks(id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, created_at INTEGER NOT NULL);
INSERT OR IGNORE INTO decks(id, name, created_at) VALUES('default', 'Default', strftime('%s','now'));
CREATE INDEX IF NOT EXISTS idx_memory_next_due ON card_memory_states(next_due_date);
UPDATE cards SET deck_id = 'default' WHERE deck_id = 'demo';
UPDATE card_memory_states SET next_due_date = 0 WHERE next_due_date IS NULL;";

/// Migration 5: string-valued settings, per-deck config, card flags,
/// missing indexes, and a one-off repair of demo-seed damage.
///
/// Three things are being fixed here.
///
/// 1. `settings.value` is `REAL` (migration 3), so no string preference —
///    voice id, dialect, theme — can be stored at all. `app_settings` is the
///    TEXT-valued replacement; existing rows are copied across and `settings`
///    stays readable (but no longer written) for one release.
/// 2. The old `seed_demo_deck` inserted memory rows with `last_review_date =
///    0`, so the first grade measured "elapsed" from the Unix epoch and wrote
///    that as `delta_t`. Both the memory rows and the poisoned review logs are
///    repaired below.
/// 3. `cards(deck_id)` and `review_logs(reviewed_at)` were both unindexed
///    despite being the filter/sort keys of the two hottest queries.
///
/// Every statement is `IF NOT EXISTS` / `OR IGNORE` / an idempotent `UPDATE`
/// or `DELETE`, so a partially-applied migration re-runs cleanly. No bare
/// `ALTER TABLE` (SQLite has no `ADD COLUMN IF NOT EXISTS`); new columns
/// arrive as sidecar tables joined at read time.
pub const MIGRATION_5_SQL: &str = "
CREATE TABLE IF NOT EXISTS app_settings(key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);
INSERT OR IGNORE INTO app_settings(key, value) SELECT key, CAST(value AS TEXT) FROM settings;
INSERT OR IGNORE INTO app_settings(key, value) VALUES
  ('retention','0.90'),
  ('tts_voice',''),
  ('dialect','american'),
  ('theme','system'),
  ('day_cutoff_hour','4');
CREATE TABLE IF NOT EXISTS deck_config(
  deck_id TEXT PRIMARY KEY NOT NULL,
  new_per_day INTEGER NOT NULL DEFAULT 20,
  review_per_day INTEGER NOT NULL DEFAULT 200,
  retention REAL,
  updated_at INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS card_flags(
  card_id TEXT PRIMARY KEY NOT NULL,
  suspended INTEGER NOT NULL DEFAULT 0,
  buried_until INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_cards_deck ON cards(deck_id);
CREATE INDEX IF NOT EXISTS idx_review_logs_at ON review_logs(reviewed_at);
UPDATE card_memory_states SET last_review_date = 0, next_due_date = 0
  WHERE last_review_date > 0 AND last_review_date < 86400;
DELETE FROM review_logs
  WHERE delta_t > 3650 AND delta_t >= (reviewed_at / 86400) - 1;";

/// Migration 6: the practice loop — prompts, sessions, attempts, scores.
///
/// A *session* is one sitting. An *attempt* is one recording inside it, and
/// carries the transcript plus, in `attempt_scores` / `attempt_word_scores`,
/// whatever could be measured about it. Scores live in sidecar tables rather
/// than columns on `attempts` because the acoustic scorer is not shipped yet:
/// rows that only ever had text-level scoring stay valid, and the
/// `pron_method` column records which one produced a given number so a chart
/// never silently mixes the two.
///
/// `attempt_cards` links an attempt to any card it created, so "save this
/// phrase to review" leads somewhere traceable.
pub const MIGRATION_6_SQL: &str = "
CREATE TABLE IF NOT EXISTS prompts(
  id TEXT PRIMARY KEY NOT NULL,
  category TEXT NOT NULL,
  topic TEXT NOT NULL,
  prompt_text TEXT NOT NULL,
  target_text TEXT,
  level INTEGER NOT NULL DEFAULT 1,
  builtin INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_prompts_cat ON prompts(category, level);
CREATE TABLE IF NOT EXISTS practice_sessions(
  id TEXT PRIMARY KEY NOT NULL,
  kind TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  ended_at INTEGER
);
CREATE TABLE IF NOT EXISTS attempts(
  id TEXT PRIMARY KEY NOT NULL,
  session_id TEXT,
  prompt_id TEXT,
  target_text TEXT,
  transcript TEXT NOT NULL,
  duration_ms INTEGER NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_attempts_session ON attempts(session_id, created_at);
CREATE TABLE IF NOT EXISTS attempt_scores(
  attempt_id TEXT PRIMARY KEY NOT NULL,
  pron_overall INTEGER,
  pron_method TEXT,
  target_logprob REAL,
  free_logprob REAL,
  normalized_conf REAL,
  wpm REAL,
  articulation_wpm REAL,
  longest_pause_ms INTEGER,
  pause_count INTEGER,
  filler_count INTEGER,
  lint_error_count INTEGER,
  lint_suggestion_count INTEGER,
  overall INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS attempt_word_scores(
  attempt_id TEXT NOT NULL,
  word_index INTEGER NOT NULL,
  word TEXT NOT NULL,
  start_ms INTEGER,
  end_ms INTEGER,
  gop REAL,
  score INTEGER,
  verdict TEXT,
  PRIMARY KEY(attempt_id, word_index)
);
CREATE TABLE IF NOT EXISTS attempt_cards(
  attempt_id TEXT NOT NULL,
  card_id TEXT NOT NULL,
  reason TEXT NOT NULL,
  PRIMARY KEY(attempt_id, card_id)
);";

/// Migration 7: tags, per-review undo, daily-cap defaults, missing index.
///
/// Three independent additions land together because they are all small and
/// all needed by the same B1 work:
///
/// 1. Tags are a many-to-many sidecar (`tags` + `card_tags`) rather than a
///    column, for the same reason `card_flags` is a sidecar table — no bare
///    `ALTER TABLE` is allowed from migration 5 on, and a free-text tag list
///    cannot be indexed or deduplicated as a column anyway.
/// 2. `review_undo` remembers, per review, exactly enough of the prior
///    `card_memory_states` row to put it back: `had_memory` distinguishes "no
///    prior row" (undo deletes the memory row entirely) from "had a row"
///    (undo restores its four fields). It is pruned to the newest
///    `scheduler::MAX_UNDO_ROWS` after every grade, so it cannot grow
///    unbounded over a long-lived database.
/// 3. `idx_review_logs_card_at` supports `daily_caps`' "is this review this
///    card's first" check without a table scan; the existing
///    `idx_review_logs_at` (migration 5) is a poor substitute because it
///    doesn't have `card_id` as a leading column.
///
/// The three `app_settings` defaults are what `prefs::Preferences` reads for
/// `new_per_day` / `review_per_day` / `bury_hours` when nothing has been
/// saved yet.
///
/// Every statement is `IF NOT EXISTS` / `OR IGNORE`, so re-running (a
/// migration that fails partway is re-run from the top) is a no-op.
pub const MIGRATION_7_SQL: &str = "
CREATE TABLE IF NOT EXISTS tags(
  id TEXT PRIMARY KEY NOT NULL,
  name TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS card_tags(
  card_id TEXT NOT NULL, tag_id TEXT NOT NULL,
  PRIMARY KEY(card_id, tag_id)
);
CREATE INDEX IF NOT EXISTS idx_card_tags_tag ON card_tags(tag_id);
CREATE TABLE IF NOT EXISTS review_undo(
  review_id TEXT PRIMARY KEY NOT NULL,
  card_id TEXT NOT NULL,
  had_memory INTEGER NOT NULL,
  prev_stability REAL, prev_difficulty REAL,
  prev_last_review_date INTEGER, prev_next_due_date INTEGER,
  reviewed_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_review_undo_at ON review_undo(reviewed_at);
CREATE INDEX IF NOT EXISTS idx_review_logs_card_at ON review_logs(card_id, reviewed_at);
INSERT OR IGNORE INTO app_settings(key,value) VALUES
  ('new_per_day','20'),('review_per_day','200'),('bury_hours','20');";

/// Minutes to ADD to UTC to get local time (UTC-5 -> `-300`). The frontend
/// always sends `-new Date().getTimezoneOffset()`, which is exactly this
/// sign convention (JS's own offset is the other way round).
///
/// Bucket a unix timestamp into a "practice day" that starts at
/// `cutoff_hour` local time rather than local midnight — a session that runs
/// past midnight (or a user who does their practice at 1am) should not have
/// it counted as two different days. `div_euclid` (not `/`) so a timestamp
/// before the epoch, or a negative `tz_offset_minutes` large enough to push
/// the shifted time negative, still floors toward the correct earlier day
/// instead of truncating toward zero.
pub fn day_index(t: i64, tz_offset_minutes: i64, cutoff_hour: i64) -> i64 {
    (t + tz_offset_minutes * 60 - cutoff_hour * 3600).div_euclid(86_400)
}

/// Inverse of [`day_index`]: the unix timestamp at which `day_index` starts.
pub fn day_start_unix(day_index: i64, tz_offset_minutes: i64, cutoff_hour: i64) -> i64 {
    day_index * 86_400 + cutoff_hour * 3600 - tz_offset_minutes * 60
}

/// WAL is idempotent but only needs to run once per process.
static WAL: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// Clone the sqlite pool for `DB_URL` and ensure WAL mode.
/// Returns `AppError` so commands can use `?` with `map_err(String::from)`.
pub async fn sqlite_pool(db: &State<'_, DbInstances>) -> Result<sqlx::SqlitePool, AppError> {
    let instances = db.0.read().await;
    let pool = instances.get(DB_URL).ok_or_else(|| {
        AppError::Db(sqlx::Error::Configuration(
            format!("database not loaded ({DB_URL}); ensure preload ran").into(),
        ))
    })?;
    let sqlite = match pool {
        DbPool::Sqlite(p) => p.clone(),
        #[allow(unreachable_patterns)]
        _ => {
            return Err(AppError::Db(sqlx::Error::Configuration(
                "unexpected non-sqlite pool".into(),
            )));
        }
    };
    drop(instances);
    // WAL: concurrent reads during background writes (runs once).
    WAL.get_or_try_init(|| async {
        sqlx::query("PRAGMA journal_mode=WAL;")
            .execute(&sqlite)
            .await
            .map(|_| ())
    })
    .await?;
    // FK enforcement is per-connection in SQLite and this pool is owned
    // by tauri-plugin-sql (no connect hook available), so these pragmas
    // are best-effort: connections created later may run with FK off until
    // first use. Consequence: `ON DELETE CASCADE` is NOT guaranteed —
    // delete paths must remove child rows explicitly (see wave-2
    // `delete_card`). busy_timeout avoids SQLITE_BUSY on concurrent
    // review-log writes. Run on every call (not just once) so each
    // checked-out connection gets the pragmas.
    sqlx::query("PRAGMA foreign_keys=ON;")
        .execute(&sqlite)
        .await?;
    sqlx::query("PRAGMA busy_timeout=5000;")
        .execute(&sqlite)
        .await?;
    Ok(sqlite)
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
pub mod testing {
    /// Apply the production migration SQL, in order, to a fresh in-memory
    /// database.
    ///
    /// Tests build their schema from the shipped constants rather than a
    /// hand-written `CREATE TABLE` block, so the test schema cannot drift
    /// from the real one and a migration that fails to parse fails the suite.
    /// `through` is the highest migration version to apply, which lets a test
    /// set up a pre-migration fixture and then step forward.
    pub async fn migrated_pool(through: u32) -> sqlx::SqlitePool {
        use sqlx::sqlite::SqlitePoolOptions;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory pool");
        sqlx::query("PRAGMA foreign_keys=ON;")
            .execute(&pool)
            .await
            .expect("enable foreign keys");
        apply_range(&pool, 1..=through).await;
        pool
    }

    /// Apply a contiguous span of migrations to an existing pool.
    ///
    /// The span is explicit because migration 3 contains a bare
    /// `ALTER TABLE ... ADD COLUMN`: stepping a fixture forward has to resume
    /// where it left off, not replay from version 1.
    pub async fn apply_range(pool: &sqlx::SqlitePool, versions: std::ops::RangeInclusive<u32>) {
        for (version, sql) in [
            (1, super::MIGRATION_1_SQL),
            (2, super::MIGRATION_2_SQL),
            (3, super::MIGRATION_3_SQL),
            (4, super::MIGRATION_4_SQL),
            (5, super::MIGRATION_5_SQL),
            (6, super::MIGRATION_6_SQL),
            (7, super::MIGRATION_7_SQL),
        ] {
            if !versions.contains(&version) {
                continue;
            }
            sqlx::raw_sql(sql)
                .execute(pool)
                .await
                .unwrap_or_else(|e| panic!("migration {version} failed: {e}"));
        }
    }

    /// The full, current schema — what every non-migration test wants.
    pub async fn test_pool() -> sqlx::SqlitePool {
        migrated_pool(7).await
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{apply_range, migrated_pool};

    async fn setting(pool: &sqlx::SqlitePool, key: &str) -> Option<String> {
        sqlx::query_scalar::<_, String>("SELECT value FROM app_settings WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await
            .expect("read app_settings")
    }

    #[tokio::test]
    async fn migration_5_is_idempotent() {
        let pool = migrated_pool(5).await;
        let before = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM app_settings")
            .fetch_one(&pool)
            .await
            .unwrap();
        // A migration that fails partway is re-run from the top, so applying
        // it twice must be a no-op rather than an error.
        sqlx::raw_sql(super::MIGRATION_5_SQL)
            .execute(&pool)
            .await
            .expect("migration 5 must be re-runnable");
        let after = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM app_settings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(before, after, "re-running must not duplicate settings");
    }

    #[tokio::test]
    async fn migration_5_seeds_string_valued_defaults() {
        let pool = migrated_pool(5).await;
        // The whole point of `app_settings`: `settings.value` is REAL, so
        // none of these could be stored at all before migration 5.
        assert_eq!(setting(&pool, "dialect").await.as_deref(), Some("american"));
        assert_eq!(setting(&pool, "theme").await.as_deref(), Some("system"));
        assert_eq!(setting(&pool, "tts_voice").await.as_deref(), Some(""));
        assert_eq!(
            setting(&pool, "day_cutoff_hour").await.as_deref(),
            Some("4")
        );
    }

    #[tokio::test]
    async fn migration_5_carries_over_an_existing_retention_choice() {
        let pool = migrated_pool(4).await;
        sqlx::query("UPDATE settings SET value = 0.85 WHERE key = 'retention'")
            .execute(&pool)
            .await
            .unwrap();
        apply_range(&pool, 5..=5).await;
        // The copy runs before the defaults, so the user's value wins.
        assert_eq!(setting(&pool, "retention").await.as_deref(), Some("0.85"));
    }

    #[tokio::test]
    async fn migration_5_repairs_epoch_poisoned_rows() {
        let pool = migrated_pool(4).await;
        sqlx::query(
            "INSERT INTO cards (id, deck_id, content_front, content_back, created_at) \
             VALUES ('c1','default','F','B',0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        // What the old demo seed produced: a memory row dated to the epoch.
        sqlx::query(
            "INSERT INTO card_memory_states \
             (card_id, stability, difficulty, last_review_date, next_due_date) \
             VALUES ('c1', 1.0, 5.0, 1, 500)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES \
             ('poisoned','c1',3,20400,1762560000), \
             ('legit','c1',3,3,1700000000), \
             ('long-but-real','c1',3,5000,1700000000)",
        )
        .execute(&pool)
        .await
        .unwrap();

        apply_range(&pool, 5..=5).await;

        let (last, due) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT last_review_date, next_due_date FROM card_memory_states WHERE card_id='c1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((last, due), (0, 0), "epoch-dated memory row must reset");

        let kept = sqlx::query_scalar::<_, String>("SELECT id FROM review_logs ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        // `long-but-real` is the reason the predicate is surgical rather than
        // an absolute `delta_t > 36500`: poisoned deltas are ~20,400 days,
        // which is *below* that bound, while a merely implausible 5,000-day
        // interval is not epoch-shaped and must survive.
        assert_eq!(kept, vec!["legit".to_string(), "long-but-real".to_string()]);
    }

    #[tokio::test]
    async fn migration_7_is_idempotent() {
        let pool = migrated_pool(7).await;
        let before = setting(&pool, "new_per_day").await;
        // A migration that fails partway is re-run from the top, so applying
        // it twice must be a no-op rather than an error.
        sqlx::raw_sql(super::MIGRATION_7_SQL)
            .execute(&pool)
            .await
            .expect("migration 7 must be re-runnable");
        assert_eq!(setting(&pool, "new_per_day").await, before);
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM app_settings WHERE key='new_per_day'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(n, 1, "re-running must not duplicate settings");
    }

    #[tokio::test]
    async fn migration_7_seeds_daily_cap_defaults() {
        let pool = migrated_pool(7).await;
        assert_eq!(setting(&pool, "new_per_day").await.as_deref(), Some("20"));
        assert_eq!(
            setting(&pool, "review_per_day").await.as_deref(),
            Some("200")
        );
        assert_eq!(setting(&pool, "bury_hours").await.as_deref(), Some("20"));
    }

    #[test]
    fn day_index_round_trips_through_day_start_unix() {
        for (tz, cutoff) in [(0_i64, 4_i64), (-300, 4), (330, 0), (-720, 23)] {
            for day in [-10_i64, 0, 1, 365, 20_000] {
                let start = super::day_start_unix(day, tz, cutoff);
                assert_eq!(
                    super::day_index(start, tz, cutoff),
                    day,
                    "round trip failed for tz={tz} cutoff={cutoff} day={day}"
                );
            }
        }
    }

    #[test]
    fn day_index_respects_the_cutoff_hour_boundary() {
        // 2024-01-02 03:59:00 UTC and 04:00:00 UTC, straddling a cutoff=4
        // boundary with tz=0: 03:59 belongs to the previous day, 04:00 to
        // the day that just started.
        let before_cutoff: i64 = 1_704_167_940; // 2024-01-02T03:59:00Z
        let at_cutoff: i64 = 1_704_168_000; // 2024-01-02T04:00:00Z
        let d_before = super::day_index(before_cutoff, 0, 4);
        let d_at = super::day_index(at_cutoff, 0, 4);
        assert_eq!(d_at, d_before + 1, "04:00 must start a new practice day");
    }

    #[test]
    fn day_index_handles_a_negative_tz_offset() {
        // UTC-5 (tz_offset_minutes = -300): 04:30 UTC is 23:30 the previous
        // local day, still before a local cutoff of 4 — so it is still
        // "yesterday" locally, one bucket behind the same instant at tz=0.
        let t: i64 = 1_704_168_600; // 2024-01-02T04:30:00Z
        let utc_day = super::day_index(t, 0, 4);
        let local_day = super::day_index(t, -300, 4);
        assert_eq!(
            local_day,
            utc_day - 1,
            "a negative offset must shift the bucket backward"
        );
    }
}
