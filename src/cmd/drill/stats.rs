use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use maud::html;

use crate::cmd::drill::state::ServerState;
use crate::cmd::drill::template::page_template;
use crate::cmd::stats_page::gather_stats;
use crate::cmd::stats_page::render_stats_page;
use crate::collection::Collection;
use crate::error::Fallible;
use crate::types::date::Date;

/// GET /stats — the collection statistics page (FEAT-02).
pub async fn stats_handler(State(state): State<ServerState>) -> (StatusCode, Html<String>) {
    match stats_inner(&state) {
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

fn stats_inner(state: &ServerState) -> Fallible<String> {
    // Re-load the collection with its own read connection; the drill session
    // keeps its own handle inside the mutex, and SQLite allows both.
    let collection = Collection::new(Some(state.directory.display().to_string()))?;
    let name = state
        .directory
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Collection");
    let stats = gather_stats(&collection.db, &collection.cards, Date::today())?;
    let body = render_stats_page(name, &stats, Some("/"));
    Ok(page_template(body).into_string())
}
