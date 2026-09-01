use std::path::Path;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use maud::html;

use crate::cmd::drill::state::ServerState;
use crate::cmd::drill::template::page_template;
use crate::cmd::run_blocking;
use crate::cmd::stats_page::gather_stats;
use crate::cmd::stats_page::render_stats_page;
use crate::collection::Collection;
use crate::error::Fallible;
use crate::types::date::Date;

/// GET /stats — the collection statistics page (FEAT-02).
pub async fn stats_handler(State(state): State<ServerState>) -> (StatusCode, Html<String>) {
    // `stats_inner` re-parses the whole collection, validates its media and
    // opens a second SQLite connection. Running that directly in the async
    // handler blocks a tokio worker for the duration, so it goes to the
    // blocking pool.
    let directory = state.directory.clone();
    let rendered = run_blocking(move || stats_inner(&directory)).await;
    match rendered {
        Ok(html) => (StatusCode::OK, Html(html)),
        Err(e) => {
            let html = page_template(html! {
                div.error {
                    h1 { "Error" }
                    p { (e) }
                    a href="/" { "Back to session" }
                }
            })
            .into_string();
            (StatusCode::INTERNAL_SERVER_ERROR, Html(html))
        }
    }
}

fn stats_inner(directory: &Path) -> Fallible<String> {
    // Re-load the collection with its own read connection; the drill session
    // keeps its own handle inside the mutex, and SQLite allows both.
    let collection = Collection::new(Some(directory.display().to_string()))?;
    let name = directory
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Collection");
    let stats = gather_stats(&collection.db, &collection.cards, Date::today())?;
    let body = render_stats_page(name, &stats, Some("/"));
    Ok(page_template(body).into_string())
}
