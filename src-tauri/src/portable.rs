//! Portable export/import: a whole deck (or the whole database) as one JSON
//! envelope, plus a lighter CSV/TSV path for plain front/back/tags.
//!
//! # Import rules (read this before changing any of the functions below)
//!
//! - The whole import runs in one transaction: either every row lands, or
//!   (on any error) none does.
//! - Decks are `INSERT OR IGNORE` by id. A card whose `deck_id` names no
//!   deck in the envelope *and* no existing deck in the database falls back
//!   to [`crate::scheduler::DEFAULT_DECK_ID`], with a warning — an import
//!   must never silently invent an orphan deck id.
//! - Card identity is the given `id` when non-empty, else
//!   [`content_card_id`] (a content hash). Resolving to an id that already
//!   exists is an *update* (front/back text, tags, flags); a new id is an
//!   *insert*. This is what makes re-importing the same file idempotent:
//!   the same file always resolves to the same ids, so the second pass only
//!   updates rows it already created.
//! - Reviews are `INSERT OR IGNORE` on `review_logs.id`; a missing id
//!   becomes `format!("{card_id}-{reviewed_at}-{index}")`, which is stable
//!   across re-imports of the same envelope for the same reason ids are.
//!   A rating outside `1..=4` is skipped with a warning (never written —
//!   `review_logs` has no CHECK constraint, so this is the only guard).
//!   `delta_t` is clamped to `0..=scheduler::MAX_DELTA_T`, the same bound
//!   `elapsed_days` enforces everywhere else.
//! - Memory is upserted only when the incoming `last_review_date` is `>=`
//!   the value already stored, so importing a stale export (e.g. an older
//!   backup opened by mistake) can never rewind a card's live scheduling
//!   state to the past.
//! - Tags replace the card's current set (not merged) — the export is
//!   assumed to be a complete snapshot of that card's tags.
//! - An empty (post-trim) `front` is skipped with a warning; a card with no
//!   text is not worth keeping and would violate every place that assumes
//!   `front` is non-blank ([`crate::scheduler::add_card`], etc).
//! - A `schema` other than [`EXPORT_SCHEMA`] is a hard `Err` before any
//!   write happens — a newer/incompatible export format must not be
//!   partially applied.

use sqlx::Row;

use crate::db::now_unix;
use crate::error::AppError;

/// Wire schema tag for the JSON export format. Bumped only on a breaking
/// change to the envelope shape; `import_json` refuses anything else.
pub const EXPORT_SCHEMA: &str = "olp.export.v1";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExportEnvelope {
    pub schema: String,
    pub exported_at: i64,
    pub app_version: String,
    pub decks: Vec<ExportDeck>,
    pub cards: Vec<ExportCard>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExportDeck {
    pub id: String,
    pub name: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExportCard {
    pub id: String,
    pub deck_id: String,
    pub front: String,
    pub back: String,
    pub created_at: i64,
    pub tags: Vec<String>,
    pub suspended: bool,
    pub buried_until: i64,
    pub memory: Option<ExportMemory>,
    pub reviews: Vec<ExportReview>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExportMemory {
    pub stability: f64,
    pub difficulty: f64,
    pub last_review_date: i64,
    pub next_due_date: i64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExportReview {
    pub id: String,
    pub rating: i64,
    pub delta_t: i64,
    pub reviewed_at: i64,
}

/// Summary of what an import actually did, returned to the frontend so a
/// user can see whether anything was skipped rather than a bare "done".
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ImportSummary {
    pub decks_created: usize,
    pub cards_created: usize,
    pub cards_updated: usize,
    pub reviews_imported: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
}

/// Deterministic id for a card that arrives with no id of its own: a plain
/// FNV-1a 64-bit hash of `deck_id` and `front` (null-byte separated so no
/// pair of strings can collide by concatenation), rendered as 16 hex
/// digits. Stable across runs/platforms (no `HashMap` seed involved, unlike
/// `std::hash`) and across decks (the deck id is part of the hashed bytes),
/// which is exactly what re-import idempotency and CSV import both need.
pub fn content_card_id(deck_id: &str, front: &str) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in deck_id
        .as_bytes()
        .iter()
        .copied()
        .chain(std::iter::once(0u8))
        .chain(front.as_bytes().iter().copied())
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("imported-{hash:016x}")
}

/// Build the full export envelope for one deck (`Some(deck_id)`) or every
/// deck (`None`).
pub async fn export_json(
    pool: &sqlx::SqlitePool,
    deck_id: Option<&str>,
) -> Result<ExportEnvelope, AppError> {
    let deck_rows = sqlx::query(
        "SELECT id, name, created_at FROM decks WHERE (?1 IS NULL OR id = ?1) ORDER BY id",
    )
    .bind(deck_id)
    .fetch_all(pool)
    .await?;
    let decks: Vec<ExportDeck> = deck_rows
        .into_iter()
        .map(|row| ExportDeck {
            id: row.get("id"),
            name: row.get("name"),
            created_at: row.get("created_at"),
        })
        .collect();

    let card_rows = sqlx::query(
        "SELECT c.id AS id, c.deck_id AS deck_id, c.content_front AS front, \
         c.content_back AS back, c.created_at AS created_at, \
         COALESCE(f.suspended, 0) AS suspended, COALESCE(f.buried_until, 0) AS buried_until \
         FROM cards c LEFT JOIN card_flags f ON f.card_id = c.id \
         WHERE (?1 IS NULL OR c.deck_id = ?1) \
         ORDER BY c.created_at ASC, c.id ASC",
    )
    .bind(deck_id)
    .fetch_all(pool)
    .await?;

    let mut cards = Vec::with_capacity(card_rows.len());
    for row in card_rows {
        let id: String = row.get("id");
        let tags: Vec<String> = sqlx::query_scalar(
            "SELECT t.name FROM card_tags ct JOIN tags t ON t.id = ct.tag_id \
             WHERE ct.card_id = ? ORDER BY t.name ASC",
        )
        .bind(&id)
        .fetch_all(pool)
        .await?;

        let mem_row = sqlx::query(
            "SELECT stability, difficulty, last_review_date, next_due_date \
             FROM card_memory_states WHERE card_id = ?",
        )
        .bind(&id)
        .fetch_optional(pool)
        .await?;
        let memory = mem_row.map(|r| ExportMemory {
            stability: r.get("stability"),
            difficulty: r.get("difficulty"),
            last_review_date: r.get("last_review_date"),
            next_due_date: r.get("next_due_date"),
        });

        let review_rows = sqlx::query(
            "SELECT id, rating, delta_t, reviewed_at FROM review_logs \
             WHERE card_id = ? ORDER BY reviewed_at ASC, rowid ASC",
        )
        .bind(&id)
        .fetch_all(pool)
        .await?;
        let reviews = review_rows
            .into_iter()
            .map(|r| ExportReview {
                id: r.get("id"),
                rating: r.get("rating"),
                delta_t: r.get("delta_t"),
                reviewed_at: r.get("reviewed_at"),
            })
            .collect();

        let suspended: i64 = row.get("suspended");
        cards.push(ExportCard {
            id,
            deck_id: row.get("deck_id"),
            front: row.get("front"),
            back: row.get("back"),
            created_at: row.get("created_at"),
            tags,
            suspended: suspended != 0,
            buried_until: row.get("buried_until"),
            memory,
            reviews,
        });
    }

    Ok(ExportEnvelope {
        schema: EXPORT_SCHEMA.to_string(),
        exported_at: now_unix(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        decks,
        cards,
    })
}

/// Replace a card's tag set inside an already-open transaction.
///
/// Mirrors [`crate::scheduler::set_card_tags`] but does not open its own
/// transaction (and does not prune orphan tags itself — the caller does one
/// sweep at the end of the whole import instead of one per card).
async fn replace_card_tags_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
    tags: &[String],
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM card_tags WHERE card_id = ?")
        .bind(card_id)
        .execute(&mut **tx)
        .await?;
    let mut seen: Vec<String> = Vec::new();
    for raw in tags {
        let Some(name) = crate::scheduler::normalize_tag(raw) else {
            continue;
        };
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        let existing: Option<String> = sqlx::query_scalar("SELECT id FROM tags WHERE name = ?")
            .bind(&name)
            .fetch_optional(&mut **tx)
            .await?;
        let tag_id = match existing {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                sqlx::query("INSERT OR IGNORE INTO tags(id, name, created_at) VALUES(?, ?, ?)")
                    .bind(&id)
                    .bind(&name)
                    .bind(now_unix())
                    .execute(&mut **tx)
                    .await?;
                sqlx::query_scalar("SELECT id FROM tags WHERE name = ?")
                    .bind(&name)
                    .fetch_one(&mut **tx)
                    .await?
            }
        };
        sqlx::query("INSERT OR IGNORE INTO card_tags(card_id, tag_id) VALUES(?, ?)")
            .bind(card_id)
            .bind(&tag_id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// Import a full envelope. See the module header for every rule this
/// enforces; this function is deliberately just the sequencing of those
/// rules, one card at a time, inside a single transaction.
pub async fn import_json(
    pool: &sqlx::SqlitePool,
    env: &ExportEnvelope,
) -> Result<ImportSummary, AppError> {
    if env.schema != EXPORT_SCHEMA {
        return Err(AppError::BadInput(format!(
            "unsupported export schema {:?}: expected {EXPORT_SCHEMA:?}",
            env.schema
        )));
    }
    let mut summary = ImportSummary::default();
    let mut tx = pool.begin().await?;

    let known_deck_ids: std::collections::HashSet<&str> =
        env.decks.iter().map(|d| d.id.as_str()).collect();
    for deck in &env.decks {
        let r = sqlx::query("INSERT OR IGNORE INTO decks(id, name, created_at) VALUES(?, ?, ?)")
            .bind(&deck.id)
            .bind(&deck.name)
            .bind(deck.created_at)
            .execute(&mut *tx)
            .await?;
        if r.rows_affected() > 0 {
            summary.decks_created += 1;
        }
    }

    for card in &env.cards {
        let front = card.front.trim();
        if front.is_empty() {
            summary.skipped += 1;
            summary
                .warnings
                .push(format!("skipped card {:?}: empty front", card.id));
            continue;
        }

        let deck_id: String = if known_deck_ids.contains(card.deck_id.as_str()) {
            card.deck_id.clone()
        } else {
            let exists: Option<String> = sqlx::query_scalar("SELECT id FROM decks WHERE id = ?")
                .bind(&card.deck_id)
                .fetch_optional(&mut *tx)
                .await?;
            match exists {
                Some(id) => id,
                None => {
                    summary.warnings.push(format!(
                        "card {:?}: deck {:?} not found, using default",
                        card.id, card.deck_id
                    ));
                    crate::scheduler::DEFAULT_DECK_ID.to_string()
                }
            }
        };

        let card_id = if card.id.trim().is_empty() {
            content_card_id(&deck_id, front)
        } else {
            card.id.clone()
        };

        let existing: Option<String> = sqlx::query_scalar("SELECT id FROM cards WHERE id = ?")
            .bind(&card_id)
            .fetch_optional(&mut *tx)
            .await?;
        if existing.is_some() {
            sqlx::query(
                "UPDATE cards SET content_front = ?, content_back = ?, deck_id = ? WHERE id = ?",
            )
            .bind(front)
            .bind(&card.back)
            .bind(&deck_id)
            .bind(&card_id)
            .execute(&mut *tx)
            .await?;
            summary.cards_updated += 1;
        } else {
            sqlx::query(
                "INSERT INTO cards(id, deck_id, content_front, content_back, created_at) \
                 VALUES(?, ?, ?, ?, ?)",
            )
            .bind(&card_id)
            .bind(&deck_id)
            .bind(front)
            .bind(&card.back)
            .bind(card.created_at)
            .execute(&mut *tx)
            .await?;
            summary.cards_created += 1;
        }

        sqlx::query(
            "INSERT INTO card_flags(card_id, suspended, buried_until, updated_at) \
             VALUES(?, ?, ?, ?) \
             ON CONFLICT(card_id) DO UPDATE SET suspended = excluded.suspended, \
               buried_until = excluded.buried_until, updated_at = excluded.updated_at",
        )
        .bind(&card_id)
        .bind(card.suspended as i64)
        .bind(card.buried_until)
        .bind(now_unix())
        .execute(&mut *tx)
        .await?;

        replace_card_tags_tx(&mut tx, &card_id, &card.tags).await?;

        if let Some(mem) = &card.memory {
            let stored_last: Option<i64> = sqlx::query_scalar(
                "SELECT last_review_date FROM card_memory_states WHERE card_id = ?",
            )
            .bind(&card_id)
            .fetch_optional(&mut *tx)
            .await?;
            let should_write = match stored_last {
                Some(existing_last) => mem.last_review_date >= existing_last,
                None => true,
            };
            if should_write {
                sqlx::query(
                    "INSERT INTO card_memory_states \
                     (card_id, stability, difficulty, last_review_date, next_due_date) \
                     VALUES(?, ?, ?, ?, ?) \
                     ON CONFLICT(card_id) DO UPDATE SET \
                       stability = excluded.stability, difficulty = excluded.difficulty, \
                       last_review_date = excluded.last_review_date, \
                       next_due_date = excluded.next_due_date",
                )
                .bind(&card_id)
                .bind(mem.stability)
                .bind(mem.difficulty)
                .bind(mem.last_review_date)
                .bind(mem.next_due_date)
                .execute(&mut *tx)
                .await?;
            } else {
                summary
                    .warnings
                    .push(format!("card {card_id}: stale memory state ignored"));
            }
        }

        for (i, rev) in card.reviews.iter().enumerate() {
            if !(1..=4).contains(&rev.rating) {
                summary.skipped += 1;
                summary.warnings.push(format!(
                    "card {card_id}: review with rating {} skipped",
                    rev.rating
                ));
                continue;
            }
            let delta_t = rev.delta_t.clamp(0, crate::scheduler::MAX_DELTA_T);
            let review_id = if rev.id.trim().is_empty() {
                format!("{card_id}-{}-{i}", rev.reviewed_at)
            } else {
                rev.id.clone()
            };
            let r = sqlx::query(
                "INSERT OR IGNORE INTO review_logs(id, card_id, rating, delta_t, reviewed_at) \
                 VALUES(?, ?, ?, ?, ?)",
            )
            .bind(&review_id)
            .bind(&card_id)
            .bind(rev.rating)
            .bind(delta_t)
            .bind(rev.reviewed_at)
            .execute(&mut *tx)
            .await?;
            if r.rows_affected() > 0 {
                summary.reviews_imported += 1;
            }
        }
    }

    // One orphan sweep for the whole import rather than one per card.
    sqlx::query("DELETE FROM tags WHERE id NOT IN (SELECT DISTINCT tag_id FROM card_tags)")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(summary)
}

/// Render an envelope's cards as CSV/TSV: header `front,back,tags`, tags
/// space-separated inside the field (Anki's convention, so exports round
/// trip through other tools that expect the same shape).
pub fn export_csv(env: &ExportEnvelope, delimiter: char) -> String {
    let mut rows = vec![vec![
        "front".to_string(),
        "back".to_string(),
        "tags".to_string(),
    ]];
    for card in &env.cards {
        rows.push(vec![
            card.front.clone(),
            card.back.clone(),
            card.tags.join(" "),
        ]);
    }
    crate::csvfmt::write_csv(&rows, delimiter)
}

/// Parse `front,back,tags` CSV/TSV text (delimiter sniffed from the first
/// line) into `(front, back, tags)` triples, ready for
/// [`import_csv_cards`]. Skips a case-insensitive `front`/`back` header row
/// and blank lines.
pub fn parse_csv_cards(text: &str) -> Result<Vec<(String, String, Vec<String>)>, String> {
    let first_line = text.lines().next().unwrap_or("");
    let delimiter = crate::csvfmt::sniff_delimiter(first_line);
    let rows = crate::csvfmt::parse_csv(text, delimiter)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if row.iter().all(|f| f.trim().is_empty()) {
            continue;
        }
        let front = row.first().cloned().unwrap_or_default();
        let back = row.get(1).cloned().unwrap_or_default();
        if front.trim().eq_ignore_ascii_case("front") && back.trim().eq_ignore_ascii_case("back") {
            continue;
        }
        let tags = row
            .get(2)
            .map(|s| s.split_whitespace().map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default();
        out.push((front, back, tags));
    }
    Ok(out)
}

/// Import plain `(front, back, tags)` rows into `deck_id`, using the same
/// content-hash identity and tag-replace rules as [`import_json`] (so a CSV
/// re-import is idempotent too), but with no review/memory data to carry.
pub async fn import_csv_cards(
    pool: &sqlx::SqlitePool,
    deck_id: &str,
    rows: &[(String, String, Vec<String>)],
) -> Result<ImportSummary, AppError> {
    let mut summary = ImportSummary::default();
    let mut tx = pool.begin().await?;
    sqlx::query("INSERT OR IGNORE INTO decks(id, name, created_at) VALUES(?, ?, ?)")
        .bind(deck_id)
        .bind(deck_id)
        .bind(now_unix())
        .execute(&mut *tx)
        .await?;

    for (front, back, tags) in rows {
        let front = front.trim();
        if front.is_empty() {
            summary.skipped += 1;
            summary
                .warnings
                .push("skipped row: empty front".to_string());
            continue;
        }
        let card_id = content_card_id(deck_id, front);
        let existing: Option<String> = sqlx::query_scalar("SELECT id FROM cards WHERE id = ?")
            .bind(&card_id)
            .fetch_optional(&mut *tx)
            .await?;
        if existing.is_some() {
            sqlx::query("UPDATE cards SET content_front = ?, content_back = ? WHERE id = ?")
                .bind(front)
                .bind(back)
                .bind(&card_id)
                .execute(&mut *tx)
                .await?;
            summary.cards_updated += 1;
        } else {
            sqlx::query(
                "INSERT INTO cards(id, deck_id, content_front, content_back, created_at) \
                 VALUES(?, ?, ?, ?, ?)",
            )
            .bind(&card_id)
            .bind(deck_id)
            .bind(front)
            .bind(back)
            .bind(now_unix())
            .execute(&mut *tx)
            .await?;
            summary.cards_created += 1;
        }
        replace_card_tags_tx(&mut tx, &card_id, tags).await?;
    }

    sqlx::query("DELETE FROM tags WHERE id NOT IN (SELECT DISTINCT tag_id FROM card_tags)")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> sqlx::SqlitePool {
        crate::db::testing::test_pool().await
    }

    #[test]
    fn content_card_id_is_stable_and_differs_across_decks() {
        let a1 = content_card_id("deck-a", "Hello");
        let a2 = content_card_id("deck-a", "Hello");
        let b = content_card_id("deck-b", "Hello");
        assert_eq!(a1, a2, "same input must hash the same every time");
        assert_ne!(a1, b, "the deck id must be part of the hash");
        assert!(a1.starts_with("imported-"));
    }

    /// The plan's headline check: export a deck with review history and
    /// FSRS memory state, import into an empty pool, and get back exactly
    /// the same memory state and every review log.
    #[tokio::test]
    async fn export_then_import_round_trips_memory_and_reviews() {
        let src = pool().await;
        sqlx::query(
            "INSERT INTO cards (id, deck_id, content_front, content_back, created_at) \
             VALUES ('c1','default','Hello','Hola',1000)",
        )
        .execute(&src)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO card_memory_states \
             (card_id, stability, difficulty, last_review_date, next_due_date) \
             VALUES ('c1', 12.5, 4.25, 2000, 5000)",
        )
        .execute(&src)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) VALUES \
             ('r1','c1',3,0,1000), ('r2','c1',4,3,2000)",
        )
        .execute(&src)
        .await
        .unwrap();
        sqlx::query("INSERT OR IGNORE INTO tags(id,name,created_at) VALUES('t1','greeting',0)")
            .execute(&src)
            .await
            .unwrap();
        sqlx::query("INSERT INTO card_tags(card_id, tag_id) VALUES('c1','t1')")
            .execute(&src)
            .await
            .unwrap();

        let env = export_json(&src, None).await.unwrap();
        assert_eq!(env.schema, EXPORT_SCHEMA);
        let card = env.cards.iter().find(|c| c.id == "c1").unwrap();
        assert_eq!(card.tags, vec!["greeting".to_string()]);
        assert_eq!(card.reviews.len(), 2);
        let mem = card.memory.as_ref().unwrap();
        assert_eq!((mem.stability, mem.difficulty), (12.5, 4.25));

        let dst = pool().await;
        let summary = import_json(&dst, &env).await.unwrap();
        assert_eq!(summary.cards_created, 1);
        assert_eq!(summary.reviews_imported, 2);

        let mem_row = sqlx::query(
            "SELECT stability, difficulty, last_review_date, next_due_date \
             FROM card_memory_states WHERE card_id='c1'",
        )
        .fetch_one(&dst)
        .await
        .unwrap();
        assert_eq!(mem_row.get::<f64, _>("stability"), 12.5);
        assert_eq!(mem_row.get::<f64, _>("difficulty"), 4.25);
        assert_eq!(mem_row.get::<i64, _>("last_review_date"), 2000);
        assert_eq!(mem_row.get::<i64, _>("next_due_date"), 5000);

        let review_ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM review_logs WHERE card_id='c1' ORDER BY id")
                .fetch_all(&dst)
                .await
                .unwrap();
        assert_eq!(review_ids, vec!["r1".to_string(), "r2".to_string()]);
    }

    #[tokio::test]
    async fn importing_twice_is_a_no_op() {
        let src = pool().await;
        sqlx::query(
            "INSERT INTO cards (id, deck_id, content_front, content_back, created_at) \
             VALUES ('c1','default','Hello','Hola',1000)",
        )
        .execute(&src)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO review_logs (id, card_id, rating, delta_t, reviewed_at) \
             VALUES ('r1','c1',3,0,1000)",
        )
        .execute(&src)
        .await
        .unwrap();
        let env = export_json(&src, None).await.unwrap();

        let dst = pool().await;
        let first = import_json(&dst, &env).await.unwrap();
        assert_eq!((first.cards_created, first.reviews_imported), (1, 1));
        let second = import_json(&dst, &env).await.unwrap();
        assert_eq!(second.cards_created, 0, "no new cards on re-import");
        assert_eq!(second.cards_updated, 1, "the existing card is updated");
        assert_eq!(
            second.reviews_imported, 0,
            "review_logs.id is INSERT OR IGNORE, so re-import inserts nothing new"
        );
        let n_cards: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cards")
            .fetch_one(&dst)
            .await
            .unwrap();
        let n_reviews: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM review_logs")
            .fetch_one(&dst)
            .await
            .unwrap();
        assert_eq!((n_cards, n_reviews), (1, 1));
    }

    #[tokio::test]
    async fn a_stale_memory_row_does_not_rewind_live_state() {
        let dst = pool().await;
        sqlx::query(
            "INSERT INTO cards (id, deck_id, content_front, content_back, created_at) \
             VALUES ('c1','default','Hello','Hola',0)",
        )
        .execute(&dst)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO card_memory_states \
             (card_id, stability, difficulty, last_review_date, next_due_date) \
             VALUES ('c1', 50.0, 2.0, 9000, 20000)",
        )
        .execute(&dst)
        .await
        .unwrap();

        let env = ExportEnvelope {
            schema: EXPORT_SCHEMA.to_string(),
            exported_at: 0,
            app_version: "0".to_string(),
            decks: vec![],
            cards: vec![ExportCard {
                id: "c1".to_string(),
                deck_id: "default".to_string(),
                front: "Hello".to_string(),
                back: "Hola".to_string(),
                created_at: 0,
                tags: vec![],
                suspended: false,
                buried_until: 0,
                memory: Some(ExportMemory {
                    stability: 1.0,
                    difficulty: 8.0,
                    last_review_date: 100, // older than the stored 9000
                    next_due_date: 200,
                }),
                reviews: vec![],
            }],
        };
        let summary = import_json(&dst, &env).await.unwrap();
        assert!(summary.warnings.iter().any(|w| w.contains("stale")));
        let stability: f64 =
            sqlx::query_scalar("SELECT stability FROM card_memory_states WHERE card_id='c1'")
                .fetch_one(&dst)
                .await
                .unwrap();
        assert_eq!(stability, 50.0, "stale import must not rewind memory");
    }

    #[tokio::test]
    async fn a_rating_of_seven_is_skipped_with_a_warning() {
        let dst = pool().await;
        let env = ExportEnvelope {
            schema: EXPORT_SCHEMA.to_string(),
            exported_at: 0,
            app_version: "0".to_string(),
            decks: vec![],
            cards: vec![ExportCard {
                id: "c1".to_string(),
                deck_id: "default".to_string(),
                front: "Hello".to_string(),
                back: "Hola".to_string(),
                created_at: 0,
                tags: vec![],
                suspended: false,
                buried_until: 0,
                memory: None,
                reviews: vec![ExportReview {
                    id: "r1".to_string(),
                    rating: 7,
                    delta_t: 0,
                    reviewed_at: 100,
                }],
            }],
        };
        let summary = import_json(&dst, &env).await.unwrap();
        assert_eq!(summary.reviews_imported, 0);
        assert_eq!(summary.skipped, 1);
        assert!(summary.warnings.iter().any(|w| w.contains("rating 7")));
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM review_logs")
            .fetch_one(&dst)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn a_schema_mismatch_is_a_hard_error() {
        let dst = pool().await;
        let env = ExportEnvelope {
            schema: "something.else".to_string(),
            exported_at: 0,
            app_version: "0".to_string(),
            decks: vec![],
            cards: vec![],
        };
        assert!(import_json(&dst, &env).await.is_err());
    }

    #[test]
    fn csv_round_trip_preserves_tags() {
        let env = ExportEnvelope {
            schema: EXPORT_SCHEMA.to_string(),
            exported_at: 0,
            app_version: "0".to_string(),
            decks: vec![],
            cards: vec![ExportCard {
                id: "c1".to_string(),
                deck_id: "default".to_string(),
                front: "Hello, world".to_string(),
                back: "Hola".to_string(),
                created_at: 0,
                tags: vec!["greeting".to_string(), "basics".to_string()],
                suspended: false,
                buried_until: 0,
                memory: None,
                reviews: vec![],
            }],
        };
        let text = export_csv(&env, ',');
        let parsed = parse_csv_cards(&text).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "Hello, world");
        assert_eq!(parsed[0].1, "Hola");
        assert_eq!(
            parsed[0].2,
            vec!["greeting".to_string(), "basics".to_string()]
        );
    }

    #[tokio::test]
    async fn a_card_whose_deck_is_unknown_falls_back_to_default() {
        let dst = pool().await;
        let env = ExportEnvelope {
            schema: EXPORT_SCHEMA.to_string(),
            exported_at: 0,
            app_version: "0".to_string(),
            decks: vec![],
            cards: vec![ExportCard {
                id: "c1".to_string(),
                deck_id: "no-such-deck".to_string(),
                front: "Hello".to_string(),
                back: "Hola".to_string(),
                created_at: 0,
                tags: vec![],
                suspended: false,
                buried_until: 0,
                memory: None,
                reviews: vec![],
            }],
        };
        let summary = import_json(&dst, &env).await.unwrap();
        assert!(summary.warnings.iter().any(|w| w.contains("no-such-deck")));
        let deck_id: String = sqlx::query_scalar("SELECT deck_id FROM cards WHERE id='c1'")
            .fetch_one(&dst)
            .await
            .unwrap();
        assert_eq!(deck_id, crate::scheduler::DEFAULT_DECK_ID);
    }
}
