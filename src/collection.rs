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

use std::env::current_dir;
use std::fs::read_to_string;
use std::path::PathBuf;
use std::time::Instant;

use crate::db::Database;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::error::fail;
use crate::media::validate::validate_media_files;
use crate::parser::DuplicateCard;
use crate::parser::parse_deck;
use crate::types::card::Card;

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
        let db: Database = Database::new(db_path)?;

        let macros: Vec<(String, String)> = {
            let mut macros = Vec::new();
            let macros_path = directory.join("macros.tex");
            if macros_path.exists() {
                let content = read_to_string(macros_path)?;
                for line in content.lines() {
                    // Skip lines starting with '%'.
                    if !line.trim_start().starts_with('%') {
                        let split = line.split_once(' ');
                        match split {
                            Some((name, definition)) => {
                                macros.push((name.to_string(), definition.to_string()));
                            }
                            None => {}
                        }
                    }
                }
            }
            macros
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

        Ok(Self {
            directory,
            db,
            cards,
            macros,
            duplicates,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs::write;

    use super::*;
    use crate::error::Fallible;
    use crate::helper::create_tmp_directory;

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
}
