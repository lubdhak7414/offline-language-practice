//! Subsystem 5: embedded SQLite via tauri-plugin-sql.
//!
//! - WAL mode for concurrent reads during review-log writes (enabled once).
//! - Declarative migrations at startup (1..=6; see `MIGRATION_N_SQL` below).
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
        migrated_pool(6).await
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
}
