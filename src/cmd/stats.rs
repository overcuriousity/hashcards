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

use std::fmt::Display;
use std::fmt::Formatter;
use std::io::Write;
use std::path::PathBuf;

use clap::ValueEnum;
use serde::Serialize;

use crate::cmd::stats_page::gather_stats;
use crate::cmd::stats_page::render_stats_page;
use crate::collection::Collection;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::types::date::Date;

#[derive(ValueEnum, Clone)]
pub enum StatsFormat {
    /// HTML output.
    Html,
    /// JSON output.
    Json,
}

impl Display for StatsFormat {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            StatsFormat::Html => write!(f, "html"),
            StatsFormat::Json => write!(f, "json"),
        }
    }
}

pub fn print_stats(directory: Option<String>, format: StatsFormat) -> Fallible<()> {
    match format {
        StatsFormat::Html => {
            let html = stats_html(directory)?;
            let path = write_stats_html(&html)?;
            println!("Stats page written to {}", path.display());
            if let Err(e) = open::that(&path) {
                eprintln!("Could not open the browser automatically: {e}");
            }
        }
        StatsFormat::Json => {
            let stats = get_stats(directory)?;
            let stats_json = serde_json::to_string_pretty(&stats)?;
            println!("{}", stats_json);
        }
    }
    Ok(())
}

/// Write the stats page to a fresh file in the temp directory and return its
/// path.
///
/// The name is randomized and the file is created exclusively with owner-only
/// permissions. A fixed name in a world-writable directory would let any
/// other local user pre-create it as a symlink and have an arbitrary file the
/// invoking user can write truncated, and would publish the collection's
/// review statistics at a guessable path.
fn write_stats_html(html: &str) -> Fallible<PathBuf> {
    let file = tempfile::Builder::new()
        .prefix("hashcards-stats-")
        .suffix(".html")
        .rand_bytes(12)
        .tempfile()?;
    let (mut file, path) = file.keep().map_err(|e| {
        ErrorReport::new(format!("Could not write the stats page to a temporary file: {e}"))
    })?;
    file.write_all(html.as_bytes())?;
    file.flush()?;
    Ok(path)
}

/// Render the stats page as a standalone HTML document. The stylesheet is
/// inlined so the file works when opened directly from disk.
fn stats_html(directory: Option<String>) -> Fallible<String> {
    let coll = Collection::new(directory)?;
    let name = coll
        .directory
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Collection")
        .to_string();
    let stats = gather_stats(&coll.db, &coll.cards, Date::today())?;
    let body = render_stats_page(&name, &stats, None);
    Ok(format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{name} \u{2014} Stats</title><style>{css}</style></head><body>{body}</body></html>",
        css = include_str!("drill/style.css"),
        body = body.into_string(),
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    cards_in_deck_count: usize,
    cards_in_db_count: usize,
    tex_macro_count: usize,
    cards_reviewed_today_count: usize,
}

fn get_stats(directory: Option<String>) -> Fallible<Stats> {
    let coll = Collection::new(directory)?;
    let cards_in_db_count = coll.db.card_hashes()?.len();
    let today = Date::today();
    let stats = Stats {
        cards_in_deck_count: coll.cards.len(),
        cards_in_db_count,
        tex_macro_count: coll.macros.len(),
        cards_reviewed_today_count: coll.db.count_reviews_in_date(today)?,
    };
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::create_tmp_copy_of_test_directory;

    /// Regression: the stats page must not land on a fixed, world-readable
    /// path in a shared temp directory. A predictable name lets another
    /// local user pre-create it as a symlink (truncating a file the invoking
    /// user can write) and exposes review statistics to everyone on the box.
    #[test]
    fn test_stats_html_goes_to_a_private_unpredictable_file() -> Fallible<()> {
        let first = write_stats_html("<p>one</p>")?;
        let second = write_stats_html("<p>two</p>")?;

        assert_ne!(first, second, "the file name must not be fixed");
        assert_eq!(std::fs::read_to_string(&first)?, "<p>one</p>");
        assert_eq!(std::fs::read_to_string(&second)?, "<p>two</p>");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&first)?.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "stats page must be owner-only, was {mode:o}");
        }

        std::fs::remove_file(&first)?;
        std::fs::remove_file(&second)?;
        Ok(())
    }

    #[test]
    fn test_display_stats_format() {
        assert_eq!(StatsFormat::Html.to_string(), "html");
        assert_eq!(StatsFormat::Json.to_string(), "json");
    }

    #[test]
    fn test_print_stats_json() -> Fallible<()> {
        let directory = create_tmp_copy_of_test_directory()?;
        print_stats(Some(directory), StatsFormat::Json)?;
        Ok(())
    }

    #[test]
    fn test_get_stats() -> Fallible<()> {
        let directory = create_tmp_copy_of_test_directory()?;
        let stats = get_stats(Some(directory)).unwrap();
        let Stats {
            cards_in_deck_count,
            cards_in_db_count,
            tex_macro_count,
            cards_reviewed_today_count,
        } = stats;
        assert_eq!(cards_in_deck_count, 2);
        assert_eq!(cards_in_db_count, 0);
        assert_eq!(tex_macro_count, 1);
        assert_eq!(cards_reviewed_today_count, 0);
        Ok(())
    }

    /// FEAT-02: the html format renders a standalone stats page.
    #[test]
    fn test_stats_html_is_standalone_page() -> Fallible<()> {
        let directory = create_tmp_copy_of_test_directory()?;
        let html = stats_html(Some(directory))?;
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<style>"), "stylesheet must be inlined");
        assert!(html.contains("Due forecast"));
        assert!(html.contains("Reviews per day"));
        assert!(html.contains("Grade distribution"));
        assert!(html.contains("Retention"));
        Ok(())
    }
}
