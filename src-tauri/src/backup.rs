//! Whole-database backup/restore.
//!
//! `backup_to` uses SQLite's own `VACUUM INTO` (a single-statement, always
//! self-consistent snapshot — no need to pause writers or hold a lock across
//! multiple file copies). `validate_backup` opens the resulting file through
//! a **separate** connection (never the live pool) so a corrupt backup
//! cannot be mistaken for a working one just because sqlite happened to open
//! the file.
//!
//! Restore is staged, not applied live: `stage_restore` copies a validated
//! backup next to the live database as [`RESTORE_STAGE`], and
//! [`apply_pending_restore`] is the only thing that ever swaps it in — see
//! its doc for why that has to run before `tauri-plugin-sql` opens its pool.

use std::path::Path;

use sqlx::sqlite::SqliteConnectOptions;
use sqlx::Connection;

use crate::error::AppError;

/// Filename (inside the app's config dir, alongside `app.db`) that a staged
/// restore is copied to. Present only between "user picked a backup to
/// restore" and the next app start.
pub const RESTORE_STAGE: &str = "app.db.restore";
/// Filename the previous live database is rotated to when a staged restore
/// is applied — a safety net, not a history: only the newest rotation is
/// kept (each `apply_pending_restore` call removes the old one first).
pub const ROTATED: &str = "app.db.bak";

/// Row counts read back from a backup file, so the frontend can show the
/// user what a restore is actually about to install instead of a bare path.
#[derive(Clone, Debug, serde::Serialize)]
pub struct BackupInfo {
    pub cards: i64,
    pub reviews: i64,
    pub decks: i64,
}

/// Snapshot the live database to `dest` via `VACUUM INTO`.
///
/// Refuses when `dest` already exists — SQLite's `VACUUM INTO` would refuse
/// too, but failing here gives a clearer error than whatever sqlite's own
/// message would be, and does it before doing any I/O.
pub async fn backup_to(pool: &sqlx::SqlitePool, dest: &Path) -> Result<u64, AppError> {
    if dest.exists() {
        return Err(AppError::BadInput(format!(
            "backup destination already exists: {}",
            dest.display()
        )));
    }
    let dest_str = dest.to_string_lossy().into_owned();
    // `VACUUM INTO` takes its target as a string literal via a bound
    // parameter; sqlite writes a fully independent, compacted copy in one
    // statement, so there is no window where the destination file is a
    // half-written snapshot.
    sqlx::query("VACUUM INTO ?")
        .bind(&dest_str)
        .execute(pool)
        .await?;
    let meta = std::fs::metadata(dest)
        .map_err(|e| AppError::BadInput(format!("backup written but unreadable: {e}")))?;
    Ok(meta.len())
}

/// Open `src` through its own read-only connection and confirm it is a
/// usable backup: `PRAGMA integrity_check` must return exactly `"ok"`, and
/// `cards`/`review_logs`/`decks` must all exist. Returns their row counts.
///
/// Deliberately never touches the live pool — a corrupt or unrelated
/// sqlite file must not be able to affect the running app just by being
/// probed.
pub async fn validate_backup(src: &Path) -> Result<BackupInfo, AppError> {
    let opts = SqliteConnectOptions::new().filename(src).read_only(true);
    let mut conn = sqlx::sqlite::SqliteConnection::connect_with(&opts)
        .await
        .map_err(AppError::Db)?;

    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&mut conn)
        .await?;
    if integrity != "ok" {
        return Err(AppError::BadInput(format!(
            "backup failed integrity check: {integrity}"
        )));
    }

    for table in ["cards", "review_logs", "decks"] {
        let exists: Option<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(table)
                .fetch_optional(&mut conn)
                .await?;
        if exists.is_none() {
            return Err(AppError::BadInput(format!(
                "backup is missing required table {table:?}"
            )));
        }
    }

    let cards: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cards")
        .fetch_one(&mut conn)
        .await?;
    let reviews: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM review_logs")
        .fetch_one(&mut conn)
        .await?;
    let decks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM decks")
        .fetch_one(&mut conn)
        .await?;
    Ok(BackupInfo {
        cards,
        reviews,
        decks,
    })
}

/// Copy an already-validated backup to `db_dir/app.db.restore`.
///
/// Callers validate first (see [`validate_backup`]) — this function only
/// moves bytes, so the restore command can report the validated
/// [`BackupInfo`] to the user before committing to anything on disk.
pub fn stage_restore(src: &Path, db_dir: &Path) -> Result<(), AppError> {
    let dest = db_dir.join(RESTORE_STAGE);
    std::fs::copy(src, &dest)
        .map_err(|e| AppError::BadInput(format!("failed to stage restore: {e}")))?;
    Ok(())
}

/// If a staged restore exists in `db_dir`, install it in place of the live
/// database and return `true`; otherwise a no-op returning `false`.
///
/// Must run **before** `tauri-plugin-sql` opens its pool: swapping the file
/// out from under an already-open sqlite connection would at best be
/// ignored (the pool keeps its existing file descriptor) and at worst
/// corrupt the WAL. That ordering is handled by registering a tiny plugin
/// ahead of the SQL plugin in `lib.rs::run` — Tauri 2.11 keeps plugins in a
/// `Vec` and calls `initialize_all` in registration order, so an earlier
/// plugin's `setup` hook always finishes before a later one's starts.
///
/// Rotation: any existing `app.db.bak` is removed (only the newest rotation
/// is kept), the live `app.db` is renamed to `app.db.bak` (if it exists —
/// a fresh install may have none yet), the WAL/SHM sidecars are dropped
/// (they belong to the file just rotated away, not the one being
/// installed), then the staged file is renamed onto `app.db`. If that final
/// rename fails, the rotated file is renamed back so the original database
/// is never left missing.
pub fn apply_pending_restore(db_dir: &Path) -> Result<bool, AppError> {
    let stage = db_dir.join(RESTORE_STAGE);
    if !stage.is_file() {
        return Ok(false);
    }
    let live = db_dir.join("app.db");
    let rotated = db_dir.join(ROTATED);

    let _ = std::fs::remove_file(&rotated);
    let had_live = live.is_file();
    if had_live {
        std::fs::rename(&live, &rotated).map_err(|e| {
            AppError::BadInput(format!("restore failed: could not rotate current db: {e}"))
        })?;
    }
    let _ = std::fs::remove_file(db_dir.join("app.db-wal"));
    let _ = std::fs::remove_file(db_dir.join("app.db-shm"));

    if let Err(e) = std::fs::rename(&stage, &live) {
        if had_live {
            // Best-effort rollback: a failed install must not leave the
            // user with no database at all.
            let _ = std::fs::rename(&rotated, &live);
        }
        return Err(AppError::BadInput(format!(
            "restore failed: could not install staged db: {e}"
        )));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real, file-backed migrated pool — **not** `db::testing::test_pool()`.
    ///
    /// `VACUUM INTO` is silently a no-op against a pool sqlx opened with the
    /// `SQLITE_OPEN_MEMORY` flag (which is exactly what `sqlite::memory:`
    /// sets): `sqlite3_exec` returns `SQLITE_OK` and no error, but the
    /// destination file is never written. Confirmed against libsqlite3
    /// directly (not an sqlx bug) — opening the same `:memory:` filename
    /// *without* that flag makes `VACUUM INTO` work normally, so it is
    /// specifically the flag, not in-memory storage in general. Production
    /// never hits this (the real pool is always file-backed), but a test of
    /// `backup_to` itself needs a real file on disk to exercise it.
    async fn file_pool(dir: &std::path::Path) -> sqlx::SqlitePool {
        let path = dir.join("source.db");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .expect("file-backed pool");
        crate::db::testing::apply_range(&pool, 1..=7).await;
        pool
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "olp-backup-test-{}-{}-{name}",
            std::process::id(),
            crate::db::now_unix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn backup_to_writes_a_file_validate_backup_accepts() {
        let dir = temp_dir("backup-ok");
        // File-backed on purpose: see `file_pool`. Against an in-memory pool
        // `VACUUM INTO` reports success and writes nothing, so this test
        // would assert on a file that was never created.
        let pool = file_pool(&dir).await;
        sqlx::query(
            "INSERT INTO cards (id, deck_id, content_front, content_back, created_at) \
             VALUES ('c1','default','F','B',0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let dest = dir.join("out.db");

        let bytes = backup_to(&pool, &dest).await.unwrap();
        assert!(bytes > 0);
        assert!(dest.is_file());

        let info = validate_backup(&dest).await.unwrap();
        assert_eq!(info.cards, 1);
        assert_eq!(info.decks, 1); // migration 4 seeds 'default'

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn backup_to_refuses_an_existing_destination() {
        let pool = crate::db::testing::test_pool().await;
        let dir = temp_dir("backup-exists");
        let dest = dir.join("out.db");
        std::fs::write(&dest, b"already here").unwrap();
        assert!(backup_to(&pool, &dest).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn validate_backup_rejects_random_bytes() {
        let dir = temp_dir("garbage");
        let path = dir.join("garbage.db");
        std::fs::write(&path, b"not a sqlite file at all, just noise").unwrap();
        assert!(validate_backup(&path).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_pending_restore_is_a_no_op_with_no_stage_file() {
        let dir = temp_dir("noop");
        assert!(!apply_pending_restore(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_pending_restore_rotates_the_existing_database() {
        let dir = temp_dir("rotate");
        std::fs::write(dir.join("app.db"), b"old live db").unwrap();
        std::fs::write(dir.join("app.db-wal"), b"wal").unwrap();
        std::fs::write(dir.join(RESTORE_STAGE), b"new staged db").unwrap();

        let applied = apply_pending_restore(&dir).unwrap();
        assert!(applied);
        assert_eq!(std::fs::read(dir.join("app.db")).unwrap(), b"new staged db");
        assert_eq!(std::fs::read(dir.join(ROTATED)).unwrap(), b"old live db");
        assert!(!dir.join("app.db-wal").exists());
        assert!(!dir.join(RESTORE_STAGE).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_pending_restore_installs_cleanly_with_no_prior_database() {
        let dir = temp_dir("fresh");
        std::fs::write(dir.join(RESTORE_STAGE), b"new staged db").unwrap();
        let applied = apply_pending_restore(&dir).unwrap();
        assert!(applied);
        assert_eq!(std::fs::read(dir.join("app.db")).unwrap(), b"new staged db");
        assert!(!dir.join(ROTATED).exists(), "nothing existed to rotate");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
