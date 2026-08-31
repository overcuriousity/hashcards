use std::path::Path;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use axum::Form;
use axum::extract::Path as AxumPath;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use axum::response::Redirect;
use maud::Markup;
use maud::html;
use serde::Deserialize;

use crate::cmd::drill::template::page_template;
use crate::cmd::serve::handlers::find_collection;
use crate::cmd::serve::state::AppState;
use crate::db::Database;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::error::fail;
use crate::parser::Parser;
use crate::parser::parse_deck;
use crate::parser::strip_frontmatter_with_offset;
use crate::types::card::Card;
use crate::types::card_hash::CardHash;
use crate::types::timestamp::Timestamp;

// ── GET handler ───────────────────────────────────────────────────────────────

pub async fn edit_get_handler(
    State(state): State<AppState>,
    AxumPath((slug, hash_hex)): AxumPath<(String, String)>,
) -> (StatusCode, Html<String>) {
    match edit_get_inner(&state, &slug, &hash_hex) {
        Ok(html) => (StatusCode::OK, Html(html)),
        Err(e) => error_page(&slug, &hash_hex, &e.to_string()),
    }
}

fn edit_get_inner(state: &AppState, slug: &str, hash_hex: &str) -> Fallible<String> {
    let rc = find_collection(state, slug)
        .ok_or_else(|| ErrorReport::new(format!("Unknown collection: {slug}")))?;

    let hash = CardHash::from_hex(hash_hex)?;
    let coll_dir = rc.coll_dir.canonicalize()?;
    let cards = parse_deck(&coll_dir)?;
    let card = find_card_by_hash(&cards, hash)?;

    let file_path = card.file_path().clone();
    let file_content = std::fs::read_to_string(&file_path)?;
    let mtime_ms = file_mtime_ms(&file_path)?;
    let block = extract_card_block(&file_content, card.range())?;
    let rel_path = file_path
        .strip_prefix(&coll_dir)
        .unwrap_or(&file_path)
        .display()
        .to_string();

    let active_session = state.sessions.lock().unwrap().contains_key(slug);
    let html = render_edit_form(
        &rc.name,
        slug,
        hash_hex,
        &rel_path,
        &block,
        mtime_ms,
        active_session,
    );
    Ok(html.into_string())
}

fn render_edit_form(
    collection_name: &str,
    slug: &str,
    hash_hex: &str,
    rel_path: &str,
    block: &str,
    mtime_ms: u64,
    active_session: bool,
) -> Markup {
    page_template(html! {
        div.edit-page {
            div.browse-header {
                a.back-link href=(format!("/collection/{slug}/bookmarks")) {
                    "\u{2190} " (collection_name) " Bookmarks"
                }
                h1 { "Edit Card" }
            }
            p.edit-path { code { (rel_path) } }
            @if active_session {
                div.edit-warning {
                    "\u{26a0} A drill session is active. End it before saving to avoid stale state."
                }
            }
            form action=(format!("/collection/{slug}/edit/{hash_hex}")) method="post" {
                input type="hidden" name="mtime_ms" value=(mtime_ms);
                textarea
                    name="new_text"
                    class="edit-textarea"
                    rows="20"
                    spellcheck="false"
                    autocorrect="off"
                    autocapitalize="off"
                {
                    (block)
                }
                div.edit-controls {
                    button type="submit" class="btn btn-primary" { "Save" }
                    " "
                    a.btn.btn-secondary
                        href=(format!("/collection/{slug}/bookmarks"))
                    { "Cancel" }
                }
            }
        }
    })
}

// ── POST handler ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct EditForm {
    pub new_text: String,
    pub mtime_ms: String,
}

pub async fn edit_post_handler(
    State(state): State<AppState>,
    AxumPath((slug, hash_hex)): AxumPath<(String, String)>,
    Form(form): Form<EditForm>,
) -> Result<Redirect, (StatusCode, Html<String>)> {
    match edit_post_inner(&state, &slug, &hash_hex, form) {
        Ok(()) => Ok(Redirect::to(&format!("/collection/{slug}/bookmarks"))),
        Err(e) => Err(error_page(&slug, &hash_hex, &e.to_string())),
    }
}

fn edit_post_inner(
    state: &AppState,
    slug: &str,
    hash_hex: &str,
    form: EditForm,
) -> Fallible<()> {
    let rc = find_collection(state, slug)
        .ok_or_else(|| ErrorReport::new(format!("Unknown collection: {slug}")))?;

    if state.sessions.lock().unwrap().contains_key(slug) {
        return fail("A drill session is active. End it before editing.");
    }

    let hash = CardHash::from_hex(hash_hex)?;
    let coll_dir = rc.coll_dir.canonicalize()?;
    let cards = parse_deck(&coll_dir)?;
    let card = find_card_by_hash(&cards, hash)?;

    let file_path = card.file_path().clone();
    let current_mtime = file_mtime_ms(&file_path)?;
    let submitted_mtime: u64 = form
        .mtime_ms
        .parse()
        .map_err(|_| ErrorReport::new("Invalid mtime in form"))?;
    if current_mtime != submitted_mtime {
        return fail(
            "The source file changed on disk since you opened this form. Reload and try again.",
        );
    }

    // Collect sibling cards at the same (file_path, range) (cloze siblings share a range).
    let range = card.range();
    let old_hashes: Vec<CardHash> = cards
        .iter()
        .filter(|c| c.file_path() == &file_path && c.range() == range)
        .map(|c| c.hash())
        .collect();

    let new_text = form.new_text.trim().to_string();
    let file_content = std::fs::read_to_string(&file_path)?;

    splice_card_block(&file_path, &file_content, range, &new_text)?;

    // Re-parse the modified file; revert on any parse error.
    let new_file_content = std::fs::read_to_string(&file_path)?;
    let (after_fm, line_offset) = strip_frontmatter_with_offset(&new_file_content)?;
    let new_cards_result =
        Parser::new(card.deck_name().clone(), file_path.clone(), line_offset).parse(after_fm);

    let new_cards = match new_cards_result {
        Ok(c) => c,
        Err(e) => {
            // Revert the file.
            let _ = std::fs::write(&file_path, &file_content);
            return fail(format!("Parse error — edit reverted: {e}"));
        }
    };

    // Find new cards at the same start_line (ranges are absolute file lines, same as old).
    let new_hashes: Vec<CardHash> = new_cards
        .iter()
        .filter(|c| c.file_path() == &file_path && c.range().0 == range.0)
        .map(|c| c.hash())
        .collect();

    let db_path = rc
        .db_path
        .to_str()
        .ok_or_else(|| ErrorReport::new("invalid db path"))?;
    let db = Database::new(db_path)?;
    let now = Timestamp::now();

    if new_hashes.len() == old_hashes.len() {
        for (old, new) in old_hashes.iter().zip(new_hashes.iter()) {
            if old == new {
                continue;
            }
            if db.card_exists(*old)? {
                db.rename_card_hash(*old, *new)?;
            } else {
                db.insert_card_if_new(*new, now)?;
            }
        }
    } else {
        // Card count changed — insert new hashes fresh (old history stays as orphans).
        for new in &new_hashes {
            db.insert_card_if_new(*new, now)?;
        }
    }

    Ok(())
}

// ── Core splice logic ─────────────────────────────────────────────────────────

fn is_card_terminator(line: &str) -> bool {
    line.starts_with("Q:") || line.starts_with("C:") || line.trim() == "---"
}

/// Compute the exclusive upper bound for splicing a card block.
///
/// `Card.range().1` is the terminator line index in non-EOF cases (exclusive)
/// and the last content line index in the EOF case (inclusive). This function
/// normalises to an exclusive bound suitable for `lines[start..end]`.
fn block_end(lines: &[&str], range: (usize, usize)) -> usize {
    let end = range.1;
    if end < lines.len() && is_card_terminator(lines[end]) {
        end
    } else {
        (end + 1).min(lines.len())
    }
}

/// Extract the raw markdown source block for a card (the lines the user edits).
/// `range` uses absolute 0-based file line numbers.
pub fn extract_card_block(file_content: &str, range: (usize, usize)) -> Fallible<String> {
    let lines: Vec<&str> = file_content.lines().collect();
    let start = range.0;
    let end = block_end(&lines, range);
    if start > lines.len() {
        return fail("Card range is out of bounds in source file");
    }
    Ok(lines[start..end].join("\n"))
}

/// Atomically replace a card's block in the source file with `new_text`.
/// `range` uses absolute 0-based file line numbers.
fn splice_card_block(
    file_path: &Path,
    file_content: &str,
    range: (usize, usize),
    new_text: &str,
) -> Fallible<()> {
    let ends_with_newline = file_content.ends_with('\n');
    let mut lines: Vec<&str> = file_content.lines().collect();
    let start = range.0;
    let end = block_end(&lines, range);
    let new_lines: Vec<&str> = new_text.lines().collect();
    lines.splice(start..end, new_lines.iter().copied());

    let mut result = lines.join("\n");
    if ends_with_newline {
        result.push('\n');
    }

    let dir = file_path.parent().unwrap_or(Path::new("."));
    let file_name = file_path
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("card"))
        .to_string_lossy();
    let tmp: PathBuf = dir.join(format!(".{file_name}.hashcards-edit.tmp"));
    std::fs::write(&tmp, &result)?;
    std::fs::rename(&tmp, file_path)?;
    Ok(())
}

// ── Utilities ─────────────────────────────────────────────────────────────────

fn find_card_by_hash(cards: &[Card], hash: CardHash) -> Fallible<&Card> {
    cards
        .iter()
        .find(|c| c.hash() == hash)
        .ok_or_else(|| ErrorReport::new("Card not found in collection (may have been edited or deleted)"))
}

fn file_mtime_ms(path: &Path) -> Fallible<u64> {
    let meta = std::fs::metadata(path)?;
    let mtime = meta.modified()?;
    let ms = mtime
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ErrorReport::new("file mtime predates UNIX epoch"))?
        .as_millis() as u64;
    Ok(ms)
}

fn error_page(slug: &str, hash_hex: &str, msg: &str) -> (StatusCode, Html<String>) {
    let html = page_template(html! {
        div.error {
            h1 { "Error" }
            p { (msg) }
            div.edit-controls {
                a.btn.btn-secondary
                    href=(format!("/collection/{slug}/edit/{hash_hex}"))
                { "\u{2190} Back" }
                " "
                a.btn.btn-secondary
                    href=(format!("/collection/{slug}/bookmarks"))
                { "Bookmarks" }
            }
        }
    })
    .into_string();
    (StatusCode::INTERNAL_SERVER_ERROR, Html(html))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_end_terminator() {
        let lines = vec!["Q: foo", "A: bar", "---", "Q: baz"];
        // Card at (0, 2): range.1 = 2, lines[2] = "---" → terminator
        assert_eq!(block_end(&lines, (0, 2)), 2);
    }

    #[test]
    fn test_block_end_eof() {
        let lines = vec!["Q: foo", "A: bar"];
        // Card at (0, 1): range.1 = 1, lines[1] = "A: bar" → not a terminator
        assert_eq!(block_end(&lines, (0, 1)), 2);
    }

    #[test]
    fn test_block_end_next_card() {
        let lines = vec!["Q: foo", "A: bar", "Q: baz", "A: quux"];
        // First card at (0, 2): range.1 = 2, lines[2] = "Q: baz" → terminator
        assert_eq!(block_end(&lines, (0, 2)), 2);
    }

    #[test]
    fn test_extract_card_block_no_fm() {
        let content = "Q: foo\nA: bar\n---\nQ: baz\nA: quux";
        let block = extract_card_block(content, (0, 2)).unwrap();
        assert_eq!(block, "Q: foo\nA: bar");
    }

    #[test]
    fn test_extract_card_block_eof() {
        let content = "Q: foo\nA: bar";
        let block = extract_card_block(content, (0, 1)).unwrap();
        assert_eq!(block, "Q: foo\nA: bar");
    }

    #[test]
    fn test_splice_basic() {
        let dir = std::env::temp_dir().join("hashcards_edit_test_splice");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.md");
        let original = "Q: foo\nA: bar\n---\nQ: baz\nA: quux\n";
        std::fs::write(&path, original).unwrap();

        splice_card_block(&path, original, (0, 2), "Q: foo edited\nA: bar edited").unwrap();
        let result = std::fs::read_to_string(&path).unwrap();
        assert_eq!(result, "Q: foo edited\nA: bar edited\n---\nQ: baz\nA: quux\n");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_splice_eof() {
        let dir = std::env::temp_dir().join("hashcards_edit_test_eof");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.md");
        let original = "Q: foo\nA: bar\n";
        std::fs::write(&path, original).unwrap();

        splice_card_block(&path, original, (0, 1), "Q: foo edited\nA: bar edited").unwrap();
        let result = std::fs::read_to_string(&path).unwrap();
        assert_eq!(result, "Q: foo edited\nA: bar edited\n");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_splice_with_frontmatter_absolute_range() {
        let dir = std::env::temp_dir().join("hashcards_edit_test_fm");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.md");
        // Q: is on absolute 0-based line 3, A: on line 4.
        let original = "---\nname = \"X\"\n---\nQ: foo\nA: bar\n";
        std::fs::write(&path, original).unwrap();

        splice_card_block(&path, original, (3, 4), "Q: foo edited\nA: bar edited").unwrap();
        let result = std::fs::read_to_string(&path).unwrap();
        assert_eq!(result, "---\nname = \"X\"\n---\nQ: foo edited\nA: bar edited\n");

        std::fs::remove_dir_all(&dir).ok();
    }
}
