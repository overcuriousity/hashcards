use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use maud::html;

use crate::cmd::drill::template::page_template;
use crate::cmd::run_blocking;
use crate::cmd::serve::handlers::find_collection;
use crate::cmd::serve::state::AppState;
use crate::cmd::stats_page::gather_stats;
use crate::cmd::stats_page::render_stats_page;
use crate::collection::Collection;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::types::date::Date;

/// GET /collection/{slug}/stats — the collection statistics page (FEAT-02).
pub async fn collection_stats_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> (StatusCode, Html<String>) {
    let known = find_collection(&state, &slug).is_some();
    let state2 = state.clone();
    let slug2 = slug.clone();
    // Parsing the collection and reading SQLite is blocking work (BUG-44).
    match run_blocking(move || stats_inner(&state2, &slug2)).await {
        Ok(html) => (StatusCode::OK, Html(html)),
        Err(e) => {
            let status = if known {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::NOT_FOUND
            };
            let html = page_template(html! {
                div.error {
                    h1 { "Error" }
                    p { (e) }
                    a href="/" { "Back to collections" }
                }
            })
            .into_string();
            (status, Html(html))
        }
    }
}

fn stats_inner(state: &AppState, slug: &str) -> Fallible<String> {
    let rc = find_collection(state, slug)
        .ok_or_else(|| ErrorReport::new(format!("Unknown collection: {slug}")))?;
    let collection = Collection::with_db_path(rc.coll_dir.clone(), rc.db_path.clone())?;
    let stats = gather_stats(&collection.db, &collection.cards, Date::today())?;
    let back = format!("/collection/{slug}");
    let body = render_stats_page(&rc.name, &stats, Some(&back));
    Ok(page_template(body).into_string())
}
