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
use crate::parser::parse_deck;
use crate::types::card::Card;

pub struct Collection {
    pub directory: PathBuf,
    pub db: Database,
    pub cards: Vec<Card>,
    pub macros: Vec<(String, String)>,
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

        let cards: Vec<Card> = {
            log::debug!("Loading deck...");
            let start = Instant::now();
            let cards = parse_deck(&directory)?;
            let end = Instant::now();
            let duration = end.duration_since(start).as_millis();
            log::debug!("Deck loaded in {duration}ms.");
            cards
        };

        // Validate media files
        validate_media_files(&cards, &directory)?;

        Ok(Self {
            directory,
            db,
            cards,
            macros,
        })
    }
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
    use super::*;

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
}
