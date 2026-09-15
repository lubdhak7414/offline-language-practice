//! Subsystem 5: embedded SQLite via tauri-plugin-sql.
//!
//! - WAL mode for concurrent reads during review-log writes (enabled once).
//! - Declarative migrations at startup (spec Migrations 1+2+3).
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
