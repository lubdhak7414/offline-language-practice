//! User-facing preferences, backed by `app_settings` (migration 5's
//! TEXT-valued table; see `db.rs`'s module doc for why it exists).
//!
//! Every field here already lives in `app_settings` as an individual key —
//! `dialect`, `theme`, `tts_voice`, `day_cutoff_hour` since migration 5,
//! `new_per_day`/`review_per_day`/`bury_hours` since migration 7. This
//! module is the one place that reads and validates all of them together, so
//! `get_preferences`/`set_preferences` in `lib.rs` have a single source of
//! truth instead of re-deriving the defaults and clamps at each call site.

use sqlx::Row;

use crate::error::AppError;

/// Every user-facing preference, mirrored verbatim (snake_case) to the
/// frontend's `Preferences` TS type.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Preferences {
    pub dialect: String,
    pub theme: String,
    pub day_cutoff_hour: i64,
    pub tts_voice: String,
    pub new_per_day: i64,
    pub review_per_day: i64,
    pub bury_hours: i64,
    /// Whether first-run onboarding has been completed. Its only job is to
    /// decide whether the app opens on onboarding or on Practice, so a
    /// fresh install and a user who dismissed onboarding are not the same
    /// state.
    pub onboarded: bool,
    /// What the user said they are practising for: `everyday`, `interview`
    /// or `both`. Chosen during onboarding, changeable in Settings, and
    /// used to bias which prompt categories come up first.
    pub goal: String,
    /// Opt-in: check github.com for a new version once per launch. Off by
    /// default — see SECURITY.md "Automatic updates". `serde(default)` so a
    /// webview that predates the field saves "off" instead of failing.
    #[serde(default)]
    pub check_updates: bool,
    /// Practice attempts per day to aim for; 0 turns the daily goal off. Only
    /// ever shown as "3 of 5 today" — no reminders, nothing is withheld.
    /// `serde(default)` for the same reason as `check_updates`.
    #[serde(default = "default_practice_goal_attempts")]
    pub practice_goal_attempts: i64,
}

/// Five recordings is a few minutes: enough to matter, short enough to keep.
pub const DEFAULT_PRACTICE_GOAL_ATTEMPTS: i64 = 5;

fn default_practice_goal_attempts() -> i64 {
    DEFAULT_PRACTICE_GOAL_ATTEMPTS
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            dialect: "american".to_string(),
            theme: "system".to_string(),
            day_cutoff_hour: 4,
            tts_voice: String::new(),
            new_per_day: crate::scheduler::DEFAULT_NEW_PER_DAY,
            review_per_day: crate::scheduler::DEFAULT_REVIEW_PER_DAY,
            bury_hours: 20,
            onboarded: false,
            goal: "both".to_string(),
            check_updates: false,
            practice_goal_attempts: DEFAULT_PRACTICE_GOAL_ATTEMPTS,
        }
    }
}

/// Clamp/replace every field to a value the rest of the app can safely act
/// on, rather than rejecting a bad save outright — a stray value from an
/// older release, or a frontend bug, should self-heal on the next load
/// instead of locking the user out of Settings.
pub fn sanitize(p: Preferences) -> Preferences {
    let theme = match p.theme.as_str() {
        "system" | "light" | "dark" => p.theme,
        _ => "system".to_string(),
    };
    let dialect = match p.dialect.as_str() {
        "american" | "british" | "canadian" | "australian" => p.dialect,
        _ => "american".to_string(),
    };
    let goal = match p.goal.as_str() {
        "everyday" | "interview" | "both" => p.goal,
        _ => "both".to_string(),
    };
    Preferences {
        dialect,
        theme,
        day_cutoff_hour: p.day_cutoff_hour.clamp(0, 23),
        tts_voice: p.tts_voice,
        new_per_day: p.new_per_day.clamp(0, 9999),
        review_per_day: p.review_per_day.clamp(0, 9999),
        bury_hours: p.bury_hours.clamp(0, 168),
        onboarded: p.onboarded,
        goal,
        check_updates: p.check_updates,
        practice_goal_attempts: p.practice_goal_attempts.clamp(0, 100),
    }
}

/// The `app_settings` keys this module owns, paired with the
/// `Preferences` field each fills. Kept in one place so `load`/`save` cannot
/// drift apart on which keys exist.
const KEYS: [&str; 11] = [
    "dialect",
    "theme",
    "day_cutoff_hour",
    "tts_voice",
    "new_per_day",
    "review_per_day",
    "bury_hours",
    "onboarded",
    "goal",
    "check_updates",
    "practice_goal_attempts",
];

/// Load every preference from `app_settings`, falling back field-by-field to
/// [`Default`] when a key is missing or fails to parse (an old database
/// before migration 7 has the first four keys but not the last three).
pub async fn load(pool: &sqlx::SqlitePool) -> Result<Preferences, AppError> {
    let placeholders = KEYS.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("SELECT key, value FROM app_settings WHERE key IN ({placeholders})");
    let mut query = sqlx::query(&sql);
    for k in KEYS {
        query = query.bind(k);
    }
    let rows = query.fetch_all(pool).await?;

    let mut raw: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for row in rows {
        raw.insert(row.get("key"), row.get("value"));
    }

    let default = Preferences::default();
    let parsed = Preferences {
        dialect: raw.get("dialect").cloned().unwrap_or(default.dialect),
        theme: raw.get("theme").cloned().unwrap_or(default.theme),
        day_cutoff_hour: raw
            .get("day_cutoff_hour")
            .and_then(|v| v.parse().ok())
            .unwrap_or(default.day_cutoff_hour),
        tts_voice: raw.get("tts_voice").cloned().unwrap_or(default.tts_voice),
        new_per_day: raw
            .get("new_per_day")
            .and_then(|v| v.parse().ok())
            .unwrap_or(default.new_per_day),
        review_per_day: raw
            .get("review_per_day")
            .and_then(|v| v.parse().ok())
            .unwrap_or(default.review_per_day),
        bury_hours: raw
            .get("bury_hours")
            .and_then(|v| v.parse().ok())
            .unwrap_or(default.bury_hours),
        // Stored as "1"/"0". An unreadable value means "not yet
        // onboarded", which shows onboarding again — annoying but
        // recoverable, unlike skipping it on a machine with no models.
        onboarded: raw.get("onboarded").map(|v| v == "1").unwrap_or(false),
        goal: raw.get("goal").cloned().unwrap_or(default.goal),
        // "1"/"0" like `onboarded`. Anything unreadable is off: the safe
        // failure for a setting that makes a network request.
        check_updates: raw.get("check_updates").map(|v| v == "1").unwrap_or(false),
        practice_goal_attempts: raw
            .get("practice_goal_attempts")
            .and_then(|v| v.parse().ok())
            .unwrap_or(default.practice_goal_attempts),
    };
    Ok(sanitize(parsed))
}

/// Persist every field to `app_settings`, sanitizing first so a malformed
/// save never reaches storage. Returns the sanitized value actually saved.
pub async fn save(pool: &sqlx::SqlitePool, p: &Preferences) -> Result<Preferences, AppError> {
    let p = sanitize(p.clone());
    let pairs: [(&str, String); 11] = [
        ("dialect", p.dialect.clone()),
        ("theme", p.theme.clone()),
        ("day_cutoff_hour", p.day_cutoff_hour.to_string()),
        ("tts_voice", p.tts_voice.clone()),
        ("new_per_day", p.new_per_day.to_string()),
        ("review_per_day", p.review_per_day.to_string()),
        ("bury_hours", p.bury_hours.to_string()),
        ("onboarded", if p.onboarded { "1" } else { "0" }.to_string()),
        ("goal", p.goal.clone()),
        (
            "check_updates",
            if p.check_updates { "1" } else { "0" }.to_string(),
        ),
        (
            "practice_goal_attempts",
            p.practice_goal_attempts.to_string(),
        ),
    ];
    let mut tx = pool.begin().await?;
    for (key, value) in pairs {
        sqlx::query(
            "INSERT INTO app_settings(key, value) VALUES(?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(p)
}

/// Convenience accessor for just the cutoff hour, used by every command that
/// needs day bucketing but not the whole preferences bundle. Defaults to `4`
/// on any read failure, same as [`Preferences::default`].
pub async fn cutoff_hour(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query_scalar::<_, String>("SELECT value FROM app_settings WHERE key = 'day_cutoff_hour'")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .map(|v| v.clamp(0, 23))
        .unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn onboarding_flags_round_trip() {
        let pool = pool().await;
        // A database that has never seen these keys reports "not onboarded",
        // which is what makes onboarding show on a fresh install.
        let fresh = load(&pool).await.unwrap();
        assert!(!fresh.onboarded);
        assert_eq!(fresh.goal, "both");

        let saved = save(
            &pool,
            &Preferences {
                onboarded: true,
                goal: "interview".to_string(),
                ..Preferences::default()
            },
        )
        .await
        .unwrap();
        assert!(saved.onboarded);

        let back = load(&pool).await.unwrap();
        assert!(back.onboarded, "onboarded must survive a round trip");
        assert_eq!(back.goal, "interview");
    }

    #[tokio::test]
    async fn the_daily_practice_goal_defaults_clamps_and_round_trips() {
        let pool = pool().await;
        assert_eq!(load(&pool).await.unwrap().practice_goal_attempts, 5);
        let saved = save(
            &pool,
            &Preferences {
                practice_goal_attempts: 9999,
                ..Preferences::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(saved.practice_goal_attempts, 100);
        let off = save(
            &pool,
            &Preferences {
                practice_goal_attempts: -3,
                ..Preferences::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(off.practice_goal_attempts, 0, "0 means the goal is off");
        assert_eq!(load(&pool).await.unwrap().practice_goal_attempts, 0);
    }

    #[test]
    fn a_webview_that_predates_the_goal_field_still_saves() {
        let json = r#"{"dialect":"american","theme":"system","day_cutoff_hour":4,"tts_voice":"","new_per_day":20,"review_per_day":200,"bury_hours":20,"onboarded":true,"goal":"both"}"#;
        let p: Preferences = serde_json::from_str(json).unwrap();
        assert_eq!(p.practice_goal_attempts, 5);
    }

    #[tokio::test]
    async fn an_unknown_goal_falls_back_instead_of_failing_the_save() {
        let pool = pool().await;
        let saved = save(
            &pool,
            &Preferences {
                goal: "astronaut".to_string(),
                ..Preferences::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(saved.goal, "both");
        assert_eq!(load(&pool).await.unwrap().goal, "both");
    }

    #[tokio::test]
    async fn check_updates_defaults_off_and_round_trips() {
        let pool = pool().await;
        // Off on a fresh database: the app must not contact github.com until
        // the user opts in (SECURITY.md "Automatic updates").
        assert!(!load(&pool).await.unwrap().check_updates);

        let saved = save(
            &pool,
            &Preferences {
                check_updates: true,
                ..Preferences::default()
            },
        )
        .await
        .unwrap();
        assert!(saved.check_updates);
        assert!(load(&pool).await.unwrap().check_updates);
        let stored: String =
            sqlx::query_scalar("SELECT value FROM app_settings WHERE key = 'check_updates'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, "1");
    }

    #[test]
    fn a_frontend_without_check_updates_deserializes_to_off() {
        // An older webview build that does not know the field must not be
        // able to fail `set_preferences`, and must never turn checks on.
        let p: Preferences = serde_json::from_value(serde_json::json!({
            "dialect": "american", "theme": "system", "day_cutoff_hour": 4,
            "tts_voice": "", "new_per_day": 20, "review_per_day": 200,
            "bury_hours": 20, "onboarded": true, "goal": "both"
        }))
        .unwrap();
        assert!(!p.check_updates);
    }

    async fn pool() -> sqlx::SqlitePool {
        crate::db::testing::test_pool().await
    }

    #[test]
    fn sanitize_rejects_bad_theme_and_out_of_range_cutoff() {
        let p = Preferences {
            theme: "purple".to_string(),
            dialect: "klingon".to_string(),
            day_cutoff_hour: 99,
            new_per_day: -5,
            review_per_day: 100_000,
            bury_hours: 999,
            ..Preferences::default()
        };
        let s = sanitize(p);
        assert_eq!(s.theme, "system");
        assert_eq!(s.dialect, "american");
        assert_eq!(s.day_cutoff_hour, 23);
        assert_eq!(s.new_per_day, 0);
        assert_eq!(s.review_per_day, 9999);
        assert_eq!(s.bury_hours, 168);
    }

    #[test]
    fn sanitize_keeps_valid_values() {
        let p = Preferences {
            theme: "dark".to_string(),
            dialect: "british".to_string(),
            day_cutoff_hour: 4,
            tts_voice: "en_US-amy".to_string(),
            new_per_day: 15,
            review_per_day: 150,
            bury_hours: 24,
            onboarded: true,
            goal: "everyday".to_string(),
            check_updates: true,
            practice_goal_attempts: 8,
        };
        assert_eq!(sanitize(p.clone()), p);
    }

    #[tokio::test]
    async fn load_defaults_when_nothing_saved_yet() {
        let pool = pool().await;
        // Migration 5/7 already seed these keys in a real database, so wipe
        // them to exercise the missing-key fallback path.
        sqlx::query("DELETE FROM app_settings")
            .execute(&pool)
            .await
            .unwrap();
        let p = load(&pool).await.unwrap();
        assert_eq!(p, Preferences::default());
    }

    #[tokio::test]
    async fn save_then_load_round_trips() {
        let pool = pool().await;
        let saved = save(
            &pool,
            &Preferences {
                dialect: "canadian".to_string(),
                theme: "dark".to_string(),
                day_cutoff_hour: 3,
                tts_voice: "en_GB-alan".to_string(),
                new_per_day: 10,
                review_per_day: 50,
                bury_hours: 12,
                onboarded: true,
                goal: "interview".to_string(),
                check_updates: false,
                practice_goal_attempts: 12,
            },
        )
        .await
        .unwrap();
        let loaded = load(&pool).await.unwrap();
        assert_eq!(loaded, saved);
        assert_eq!(loaded.dialect, "canadian");
        assert_eq!(loaded.new_per_day, 10);
    }

    #[tokio::test]
    async fn save_sanitizes_before_persisting() {
        let pool = pool().await;
        let saved = save(
            &pool,
            &Preferences {
                theme: "nonsense".to_string(),
                ..Preferences::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(saved.theme, "system");
        let loaded = load(&pool).await.unwrap();
        assert_eq!(loaded.theme, "system");
    }

    #[tokio::test]
    async fn cutoff_hour_defaults_to_four() {
        let pool = pool().await;
        sqlx::query("DELETE FROM app_settings WHERE key='day_cutoff_hour'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(cutoff_hour(&pool).await, 4);
    }
}
