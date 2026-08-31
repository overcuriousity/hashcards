use std::collections::HashMap;
use std::path::Path;

use axum::Form;
use axum::extract::Path as AxumPath;
use axum::extract::Query;
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
use crate::collection::Collection;
use crate::db::Bookmark;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::flash::Flash;
use crate::types::card::Card;
use crate::types::card::CardContent;
use crate::types::card_hash::CardHash;

// ── List ─────────────────────────────────────────────────────────────────────

pub async fn bookmark_list_handler(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    match bookmark_list_inner(&state, &slug, flash) {
        Ok(html) => (StatusCode::OK, Html(html)),
        Err(e) => error_page(&slug, e),
    }
}

fn bookmark_list_inner(state: &AppState, slug: &str, flash: Option<Flash>) -> Fallible<String> {
    let rc = find_collection(state, slug)
        .ok_or_else(|| ErrorReport::new(format!("Unknown collection: {slug}")))?;
    let collection = Collection::with_db_path(rc.coll_dir.clone(), rc.db_path.clone())?;
    let bookmarks = collection.db.list_bookmarks()?;
    let cards_by_hash: HashMap<CardHash, &Card> =
        collection.cards.iter().map(|c| (c.hash(), c)).collect();
    let html = render_bookmark_list(
        &rc.name,
        slug,
        &rc.coll_dir,
        &bookmarks,
        &cards_by_hash,
        flash,
    );
    Ok(html.into_string())
}

fn card_preview(content: &CardContent) -> String {
    let raw = match content {
        CardContent::Basic { question, .. } => question.as_str(),
        CardContent::Cloze { text, .. } => text.as_str(),
    };
    let trimmed = raw.trim();
    if trimmed.len() > 120 {
        // Truncate at char boundary before 120 bytes
        let mut end = 120;
        while !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &trimmed[..end])
    } else {
        trimmed.to_string()
    }
}

fn render_bookmark_list(
    collection_name: &str,
    slug: &str,
    coll_dir: &Path,
    bookmarks: &[Bookmark],
    cards: &HashMap<CardHash, &Card>,
    flash: Option<Flash>,
) -> Markup {
    let active: Vec<(&Bookmark, &Card)> = bookmarks
        .iter()
        .filter_map(|bm| cards.get(&bm.card_hash).map(|c| (bm, *c)))
        .collect();
    let orphaned: Vec<&Bookmark> = bookmarks
        .iter()
        .filter(|bm| !cards.contains_key(&bm.card_hash))
        .collect();

    page_template(html! {
        @if let Some(f) = &flash { (f.render()) }
        div.bookmarks {
            div.browse-header {
                a.back-link href=(format!("/collection/{slug}")) { "\u{2190} " (collection_name) }
                h1 { "Bookmarks" }
            }

            @if active.is_empty() && orphaned.is_empty() {
                p.empty { "No bookmarks yet. Press " b { "b" } " during drilling to bookmark a card." }
            } @else {
                @if !active.is_empty() {
                    div.bookmark-list {
                        @for (bm, card) in &active {
                            (render_bookmark_row(slug, coll_dir, bm, card))
                        }
                    }
                }
                @if !orphaned.is_empty() {
                    h2.orphaned-heading { "Orphaned" }
                    p.orphaned-note { "These cards no longer exist in the collection (likely edited or deleted outside the web UI)." }
                    div.bookmark-list {
                        @for bm in &orphaned {
                            (render_orphaned_row(slug, bm))
                        }
                    }
                }
            }
        }
    })
}

fn render_bookmark_row(slug: &str, coll_dir: &Path, bm: &Bookmark, card: &Card) -> Markup {
    let hash_hex = bm.card_hash.to_hex();
    let preview = card_preview(card.content());
    let rel_path = card
        .file_path()
        .strip_prefix(coll_dir)
        .unwrap_or(card.file_path())
        .display()
        .to_string();

    html! {
        div.bookmark-row {
            div.bookmark-meta {
                span.bookmark-deck { (card.deck_name()) }
                span.bookmark-path { (rel_path) }
                span.bookmark-date { (bm.created_at) }
            }
            p.bookmark-preview { (preview) }
            @if let Some(ref note) = bm.note {
                p.bookmark-note-display { (note) }
            }
            div.bookmark-actions {
                a.edit-link.btn.btn-secondary
                    href=(format!("/collection/{slug}/edit/{hash_hex}"))
                { "Edit" }
                form.inline-form
                    action=(format!("/collection/{slug}/bookmarks/{hash_hex}/delete"))
                    method="post"
                {
                    button type="submit" class="btn btn-secondary" { "Remove" }
                }
                form.note-form
                    action=(format!("/collection/{slug}/bookmarks/{hash_hex}/note"))
                    method="post"
                {
                    input
                        type="text"
                        name="note"
                        class="note-input"
                        placeholder="Add a note…"
                        value=(bm.note.as_deref().unwrap_or(""));
                    button type="submit" class="btn btn-secondary" { "Save note" }
                }
            }
        }
    }
}

fn render_orphaned_row(slug: &str, bm: &Bookmark) -> Markup {
    let hash_hex = bm.card_hash.to_hex();
    html! {
        div.bookmark-row.bookmark-orphaned {
            p.bookmark-preview { code { (hash_hex) } }
            div.bookmark-actions {
                form.inline-form
                    action=(format!("/collection/{slug}/bookmarks/{hash_hex}/delete"))
                    method="post"
                {
                    button type="submit" class="btn btn-secondary" { "Remove" }
                }
            }
        }
    }
}

// ── Delete ────────────────────────────────────────────────────────────────────

pub async fn bookmark_delete_handler(
    State(state): State<AppState>,
    AxumPath((slug, hash_hex)): AxumPath<(String, String)>,
) -> Redirect {
    let _ = bookmark_delete_inner(&state, &slug, &hash_hex);
    Redirect::to(&format!("/collection/{slug}/bookmarks"))
}

fn bookmark_delete_inner(state: &AppState, slug: &str, hash_hex: &str) -> Fallible<()> {
    let rc = find_collection(state, slug)
        .ok_or_else(|| ErrorReport::new(format!("Unknown collection: {slug}")))?;
    let collection = Collection::with_db_path(rc.coll_dir, rc.db_path)?;
    let hash = CardHash::from_hex(hash_hex)?;
    collection.db.delete_bookmark(hash)?;
    Ok(())
}

// ── Update note ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct NoteForm {
    pub note: String,
}

pub async fn bookmark_note_handler(
    State(state): State<AppState>,
    AxumPath((slug, hash_hex)): AxumPath<(String, String)>,
    Form(form): Form<NoteForm>,
) -> Redirect {
    let _ = bookmark_note_inner(&state, &slug, &hash_hex, form.note);
    Redirect::to(&format!("/collection/{slug}/bookmarks"))
}

fn bookmark_note_inner(state: &AppState, slug: &str, hash_hex: &str, note: String) -> Fallible<()> {
    let rc = find_collection(state, slug)
        .ok_or_else(|| ErrorReport::new(format!("Unknown collection: {slug}")))?;
    let collection = Collection::with_db_path(rc.coll_dir, rc.db_path)?;
    let hash = CardHash::from_hex(hash_hex)?;
    let note = if note.trim().is_empty() {
        None
    } else {
        Some(note.trim().to_string())
    };
    collection.db.update_bookmark_note(hash, note)?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn error_page(slug: &str, e: impl std::fmt::Display) -> (StatusCode, Html<String>) {
    let html = page_template(html! {
        div.error {
            h1 { "Error" }
            p { (e) }
            a href=(format!("/collection/{slug}")) { "\u{2190} Back" }
        }
    })
    .into_string();
    (StatusCode::INTERNAL_SERVER_ERROR, Html(html))
}
