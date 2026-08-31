// Copyright 2025 Fernando Borretti
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashSet;
use std::env::current_dir;
use std::fs::read_to_string;
use std::path::PathBuf;
use std::time::Instant;

use crate::db::CLOZE_HASH_SCHEME_CURRENT;
use crate::db::Database;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::error::fail;
use crate::media::validate::validate_media_files;
use crate::parser::DuplicateCard;
use crate::parser::parse_deck;
use crate::types::card::Card;
use crate::types::card_hash::CardHash;

pub struct Collection {
    pub directory: PathBuf,
    pub db: Database,
    pub cards: Vec<Card>,
    pub macros: Vec<(String, String)>,
    /// Byte-identical cards that were deduplicated at load time.
    pub duplicates: Vec<DuplicateCard>,
}

impl Collection {
    pub fn new(directory: Option<String>) -> Fallible<Self> {
        let directory: PathBuf = match directory {
            Some(dir) => PathBuf::from(dir),
            None => current_dir()?,
        };
        let directory: PathBuf = if directory.exists() {
            directory.canonicalize()?
        } else {
            return fail("directory does not exist.");
        };

        let db_path: PathBuf = directory.join("hashcards.db");
        Self::with_db_path(directory, db_path)
    }

    pub fn with_db_path(directory: PathBuf, db_path: PathBuf) -> Fallible<Self> {
        let directory: PathBuf = if directory.exists() {
            directory.canonicalize()?
        } else {
            return fail("directory does not exist.");
        };

        let db_path: &str = db_path
            .to_str()
            .ok_or_else(|| ErrorReport::new("invalid path"))?;
        let mut db: Database = Database::new(db_path)?;

        let macros: Vec<(String, String)> = {
            let macros_path = directory.join("macros.tex");
            if macros_path.exists() {
                let content = read_to_string(&macros_path)?;
                let (parsed, malformed) = parse_macros(&content);
                for line_no in malformed {
                    log::warn!(
                        "{}: line {line_no} is not a valid macro definition (expected a name and a definition separated by a space); line ignored.",
                        macros_path.display()
                    );
                }
                parsed
            } else {
                Vec::new()
            }
        };

        let parsed = {
            log::debug!("Loading deck...");
            let start = Instant::now();
            let parsed = parse_deck(&directory)?;
            let end = Instant::now();
            let duration = end.duration_since(start).as_millis();
            log::debug!("Deck loaded in {duration}ms.");
            parsed
        };
        let cards: Vec<Card> = parsed.cards;
        let duplicates: Vec<DuplicateCard> = parsed.duplicates;

        // Validate media files
        validate_media_files(&cards, &directory)?;

        // One-time re-link of legacy cloze hashes; a cheap no-op normally.
        upgrade_cloze_hashes(&mut db, &cards)?;

        Ok(Self {
            directory,
            db,
            cards,
            macros,
            duplicates,
        })
    }
}

/// One-time upgrade of legacy (offset-based) cloze card hashes to the
/// content-based scheme (see CardContent::hash and CardContent::legacy_hash).
///
/// For every parsed cloze card, its legacy hash — a function of the card's
/// family text plus the deletion's identity (content and occurrence) — is
/// mapped to its new hash, and the database rewrites all referencing rows in
/// one transaction. Database rows matching no parsed card (deleted cards, or
/// hashes written on a machine with different endianness/pointer width) are
/// left untouched; the `orphans` command reports them exactly as before.
///
/// On an already-upgraded database this returns after a single SELECT.
fn upgrade_cloze_hashes(db: &mut Database, cards: &[Card]) -> Fallible<()> {
    if db.cloze_hash_scheme()? == CLOZE_HASH_SCHEME_CURRENT {
        return Ok(());
    }
    let mut seen: HashSet<CardHash> = HashSet::new();
    let mut renames: Vec<(CardHash, CardHash)> = Vec::new();
    for card in cards {
        if let Some(legacy) = card.legacy_hash() {
            // Duplicate hand-written cards share one hash; rename it once.
            if seen.insert(legacy) {
                renames.push((legacy, card.hash()));
            }
        }
    }
    let renamed = db.migrate_cloze_hashes(&renames)?;
    log::info!("Upgraded {renamed} cloze card hash(es) to the content-based scheme.");
    Ok(())
}

/// Parse the contents of `macros.tex` into `(name, definition)` pairs.
///
/// Returns the parsed macros and the 1-based line numbers of malformed
/// lines: non-comment, non-blank lines without a space separating the
/// macro name from its definition. Comment lines start with `%`.
fn parse_macros(content: &str) -> (Vec<(String, String)>, Vec<usize>) {
    let mut macros = Vec::new();
    let mut malformed = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('%') {
            continue;
        }
        match line.split_once(' ') {
            Some((name, definition)) => {
                macros.push((name.to_string(), definition.to_string()));
            }
            None => malformed.push(index + 1),
        }
    }
    (macros, malformed)
}

#[cfg(test)]
mod tests {
    use std::fs::write;
    use std::iter::once;

    use rusqlite::Connection;

    use super::*;
    use crate::db::CLOZE_HASH_SCHEME_CURRENT;
    use crate::db::ReviewRecord;
    use crate::error::Fallible;
    use crate::fsrs::Grade;
    use crate::helper::create_tmp_directory;
    use crate::types::card::CardContent;
    use crate::types::card_hash::CardHash;
    use crate::types::performance::Performance;
    use crate::types::performance::ReviewedPerformance;
    use crate::types::timestamp::Timestamp;

    #[test]
    fn test_parse_macros_reports_malformed_lines() {
        let content = "\\foo bar\n% comment\n\nnospace\n\\baz qux quux\n";
        let (macros, malformed) = parse_macros(content);
        assert_eq!(
            macros,
            vec![
                ("\\foo".to_string(), "bar".to_string()),
                ("\\baz".to_string(), "qux quux".to_string()),
            ]
        );
        // Line 4 ("nospace") has no space separator; comment and blank
        // lines are not malformed.
        assert_eq!(malformed, vec![4]);
    }

    /// Regression test for BUG-12: two byte-identical cards must not abort
    /// collection loading (and therefore drill startup); one copy is kept
    /// and the duplicate is reported with both file locations.
    #[test]
    fn test_duplicate_cards_across_files_load_with_warning() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        write(dir.join("a.md"), "Q: same question\nA: same answer\n")?;
        write(dir.join("b.md"), "Q: same question\nA: same answer\n")?;

        let collection = Collection::new(Some(dir.display().to_string()))?;

        // Exactly one copy survives.
        assert_eq!(collection.cards.len(), 1);

        // The duplicate is reported, naming both files.
        assert_eq!(collection.duplicates.len(), 1);
        let dup = &collection.duplicates[0];
        let locations = format!("{} {}", dup.kept(), dup.ignored());
        assert!(locations.contains("a.md:1"), "locations were: {locations}");
        assert!(locations.contains("b.md:1"), "locations were: {locations}");
        Ok(())
    }

    #[test]
    fn test_collection_without_duplicates_reports_none() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        write(dir.join("a.md"), "Q: question one\nA: answer one\n")?;
        write(dir.join("b.md"), "Q: question two\nA: answer two\n")?;
        let collection = Collection::new(Some(dir.display().to_string()))?;
        assert_eq!(collection.cards.len(), 2);
        assert!(collection.duplicates.is_empty());
        Ok(())
    }

    /// Compute a card's hash under the OLD (pre-v2) scheme, inline:
    /// blake3("Cloze" ++ text ++ start.to_le_bytes() ++ end.to_le_bytes()).
    fn old_scheme_hash(content: &CardContent) -> CardHash {
        match content {
            CardContent::Cloze { text, start, end } => {
                let mut input: Vec<u8> = Vec::new();
                input.extend_from_slice(b"Cloze");
                input.extend_from_slice(text.as_bytes());
                input.extend_from_slice(&start.to_le_bytes());
                input.extend_from_slice(&end.to_le_bytes());
                CardHash::hash_bytes(&input)
            }
            CardContent::Basic { .. } => panic!("expected a cloze card"),
        }
    }

    /// BUG-27 regression: loading a collection whose DB holds legacy
    /// (offset-based) cloze hashes re-links reviews, performance, and
    /// bookmarks to the new content-based hashes in one pass, while a
    /// genuinely-deleted card's rows stay behind as orphans.
    #[test]
    fn test_load_migrates_legacy_cloze_hashes() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        write(
            dir.join("Deck.md"),
            "C: The capital of [France] is [Paris].\n",
        )?;
        // Parse the deck to learn the deletions, so the legacy hashes can be
        // computed inline with the old algorithm.
        let cards = parse_deck(&dir)?.cards;
        assert_eq!(cards.len(), 2);
        let legacy_hashes: Vec<CardHash> =
            cards.iter().map(|c| old_scheme_hash(c.content())).collect();
        let orphan = CardHash::hash_bytes(b"a card that was deleted from the deck");

        let now = Timestamp::now();
        let db_path = dir.join("hashcards.db");
        let session_id;
        {
            let mut db = Database::new(db_path.to_str().expect("utf-8 path"))?;
            session_id = db.create_session(now)?;
            for hash in legacy_hashes.iter().chain(once(&orphan)) {
                db.insert_card(*hash, now)?;
                let review = ReviewRecord {
                    card_hash: *hash,
                    reviewed_at: now,
                    grade: Grade::Good,
                    stability: 2.0,
                    difficulty: 2.0,
                    interval_raw: 1.0,
                    interval_days: 1,
                    due_date: now.date(),
                    duration_ms: None,
                };
                let performance = Performance::Reviewed(ReviewedPerformance {
                    last_reviewed_at: now,
                    stability: 2.0,
                    difficulty: 2.0,
                    interval_raw: 1.0,
                    interval_days: 1,
                    due_date: now.date(),
                    review_count: 1,
                });
                db.insert_review_and_update_performance(session_id, &review, performance)?;
            }
            db.insert_bookmark(legacy_hashes[0], Some("check this".to_string()), now)?;
        }
        // A real legacy DB is stamped scheme 1 by SQL migration 6; a fresh
        // Database::new stamps scheme 2, so flip it to simulate legacy.
        {
            let conn = Connection::open(&db_path)?;
            conn.execute(
                "update meta set value = '1' where key = 'cloze_hash_scheme';",
                [],
            )?;
        }

        // Loading the collection performs the one-time rehash.
        let coll = Collection::new(Some(dir.display().to_string()))?;
        let db_hashes = coll.db.card_hashes()?;
        for card in &coll.cards {
            assert!(
                db_hashes.contains(&card.hash()),
                "card {} was not re-linked",
                card.hash()
            );
            // The card's performance followed it.
            assert!(matches!(
                coll.db.get_card_performance(card.hash())?,
                Performance::Reviewed(_)
            ));
        }
        for legacy in &legacy_hashes {
            assert!(
                !db_hashes.contains(legacy),
                "legacy hash {legacy} still present"
            );
        }
        // Reviews were rewritten to the new hashes; the orphan's review is untouched.
        let review_hashes: Vec<CardHash> = coll
            .db
            .get_reviews_for_session(session_id)?
            .iter()
            .map(|r| r.data.card_hash)
            .collect();
        for card in &coll.cards {
            assert!(review_hashes.contains(&card.hash()));
        }
        assert!(review_hashes.contains(&orphan));
        // The bookmark followed its card (cards[0] parsed first).
        assert!(coll.db.get_bookmark(cards[0].hash())?.is_some());
        // The genuinely-deleted card's row is untouched: still an orphan.
        assert!(coll.db.card_exists(orphan)?);
        assert_eq!(coll.db.cloze_hash_scheme()?, CLOZE_HASH_SCHEME_CURRENT);
        Ok(())
    }

    /// An already-current DB is not touched: reloading is a cheap no-op.
    #[test]
    fn test_load_of_current_scheme_db_is_a_noop() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        write(dir.join("Deck.md"), "C: Water is [H2O].\n")?;
        let first = Collection::new(Some(dir.display().to_string()))?;
        let hashes_before = first.db.card_hashes()?;
        assert_eq!(first.db.cloze_hash_scheme()?, CLOZE_HASH_SCHEME_CURRENT);
        drop(first);
        let second = Collection::new(Some(dir.display().to_string()))?;
        assert_eq!(second.db.card_hashes()?, hashes_before);
        assert_eq!(second.db.cloze_hash_scheme()?, CLOZE_HASH_SCHEME_CURRENT);
        Ok(())
    }
}
