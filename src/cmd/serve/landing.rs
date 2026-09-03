use std::collections::HashMap;

use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use maud::Markup;
use maud::html;

use chrono::Duration;

use crate::cmd::drill::template::page_template;
use crate::cmd::serve::auth::CurrentUser;
use crate::cmd::serve::files::local_collections_for;
use crate::cmd::serve::git::refresh_collection_info;
use crate::cmd::serve::hedgedoc::build_combined_infos;
use crate::cmd::serve::state::AppState;
use crate::cmd::serve::state::CollectionInfo;
use crate::flash::Flash;
use crate::types::timestamp::Timestamp;

/// True when the cached collection counts are older than the poll interval.
pub fn counts_are_stale(last: Option<Timestamp>, now: Timestamp, interval_minutes: u64) -> bool {
    match last {
        None => true,
        Some(last) => {
            now.into_inner() - last.into_inner() >= Duration::minutes(interval_minutes as i64)
        }
    }
}

pub async fn landing_handler(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    current_user: Option<CurrentUser>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    let owner = current_user.as_ref().map(|u| u.email.clone());

    // BUG-45: recompute counts when they are older than the poll interval.
    let interval_minutes = state
        .config
        .git
        .as_ref()
        .map(|g| g.poll_interval_minutes)
        .unwrap_or(30);
    let stale = {
        let last = state.counts_refreshed_at.lock();
        counts_are_stale(*last, Timestamp::now(), interval_minutes)
    };
    if stale && interval_minutes > 0 {
        let static_collections = state.config.collections.clone();
        let sources_snapshot = state.hedgedoc_sources.lock().clone();
        match tokio::task::spawn_blocking(move || {
            build_combined_infos(&static_collections, &sources_snapshot)
        })
        .await
        {
            Ok(combined) => {
                *state.collections.write().await = combined;
                *state.counts_refreshed_at.lock() = Some(Timestamp::now());
            }
            Err(e) => log::error!("Failed to refresh collection counts: {e}"),
        }
    }

    // Local collections are discovered per request rather than read from the
    // cached list: a folder created a moment ago must appear at once, and it
    // is the user's own writing, not a remote that syncs on a timer.
    let local_infos = {
        let local = local_collections_for(&state, current_user.as_ref());
        match tokio::task::spawn_blocking(move || refresh_collection_info(&local)).await {
            Ok(infos) => infos,
            Err(e) => {
                log::error!("Failed to count local collections: {e}");
                Vec::new()
            }
        }
    };

    let all_collections = state.collections.read().await;
    let collections: Vec<&CollectionInfo> = all_collections
        .iter()
        .chain(local_infos.iter())
        .filter(|c| c.owner.as_deref() == owner.as_deref())
        .collect();
    let last_synced = *state.last_synced.lock();
    let hedgedoc_last_synced = *state.hedgedoc_last_synced.lock();
    let git_enabled = state.config.git.is_some();
    let hedgedoc_count = state
        .hedgedoc_sources
        .lock()
        .iter()
        .filter(|s| s.collection.owner.as_deref() == owner.as_deref())
        .count();
    let config_available = state.config.data_dir.is_some();
    let custom_decks: Vec<(String, String)> = state
        .custom_decks
        .lock()
        .iter()
        .filter(|d| d.owner.as_deref() == owner.as_deref())
        .map(|d| (d.name.clone(), d.slug.clone()))
        .collect();
    // FEAT-03: surface running sessions so they can be resumed rather than
    // silently restarted.
    let resume: HashMap<String, usize> = {
        let sessions = state.sessions.lock();
        sessions
            .iter()
            .filter_map(|(slug, s)| {
                let session = s.lock();
                if session.mutable.finished_at.is_none() {
                    Some((slug.clone(), session.mutable.cards.len()))
                } else {
                    None
                }
            })
            .collect()
    };
    let status = LandingStatus {
        last_synced,
        git_enabled,
        hedgedoc_count,
        hedgedoc_last_synced,
        config_available,
        signed_in_as: owner.clone(),
    };
    let html = render_landing_page(&collections, &custom_decks, &resume, &status, flash);
    (StatusCode::OK, Html(html.into_string()))
}

/// The server-status details shown under the collection list: whether git
/// and the config file are in play, and when each source last synced.
struct LandingStatus {
    last_synced: Option<Timestamp>,
    git_enabled: bool,
    hedgedoc_count: usize,
    hedgedoc_last_synced: Option<Timestamp>,
    config_available: bool,
    /// The logged-in user's email, when `[oidc]` is configured. Drives the
    /// only logout control in the UI.
    signed_in_as: Option<String>,
}

fn render_landing_page(
    collections: &[&CollectionInfo],
    custom_decks: &[(String, String)],
    resume: &HashMap<String, usize>,
    status: &LandingStatus,
    flash: Option<Flash>,
) -> Markup {
    let LandingStatus {
        last_synced,
        git_enabled,
        hedgedoc_count,
        hedgedoc_last_synced,
        config_available,
        ref signed_in_as,
    } = *status;
    page_template(html! {
        @if let Some(f) = &flash { (f.render()) }
        div.landing {
            h1 { "hashcards-web" }
            @if let Some(email) = signed_in_as {
                div.sync-bar {
                    span.sync-status { (format!("Signed in as {email}")) }
                    // POST, so a third-party page cannot log the user out by
                    // embedding the URL.
                    form action="/auth/logout" method="post" style="display:inline" {
                        input .sync-button.btn.btn-secondary type="submit" value="Log out";
                    }
                }
            }
            @if git_enabled {
                div.sync-bar {
                    span.sync-status {
                        @if let Some(ts) = last_synced {
                            (format!("Last synced: {}", ts.into_inner().format("%Y-%m-%d %H:%M:%S")))
                        } @else {
                            "Not yet synced"
                        }
                    }
                    form action="/sync" method="post" style="display:inline" {
                        input .sync-button.btn.btn-secondary type="submit" value="Sync Now";
                    }
                }
            }
            @if config_available {
                div.sync-bar {
                    span.sync-status {
                        @if custom_decks.is_empty() {
                            "No decks"
                        } @else {
                            (format!("{} deck(s)", custom_decks.len()))
                        }
                    }
                    form action="/decks" method="get" style="display:inline" {
                        input .sync-button.btn.btn-secondary type="submit" value="Manage Decks";
                    }
                }
            }
            @if !custom_decks.is_empty() {
                h2 { "Decks" }
                table.collection-table {
                    tbody {
                        @for (name, slug) in custom_decks {
                            tr {
                                td { (name) }
                                td {
                                    a.drill-link.btn.btn-primary href=(format!("/collection/{slug}")) {
                                        "Open"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            @if config_available {
                div.sync-bar {
                    span.sync-status {
                        @if hedgedoc_count > 0 {
                            @if let Some(ts) = hedgedoc_last_synced {
                                (format!("HedgeDoc synced: {}", ts.into_inner().format("%Y-%m-%d %H:%M:%S")))
                            } @else {
                                (format!("{hedgedoc_count} HedgeDoc source(s) — not yet synced"))
                            }
                        } @else {
                            "No sources"
                        }
                    }
                    form action="/sources" method="get" style="display:inline" {
                        input .sync-button.btn.btn-secondary type="submit" value="Manage sources";
                    }
                    form action="/files" method="get" style="display:inline" {
                        input .sync-button.btn.btn-secondary type="submit" value="My Cards";
                    }
                }
            }
            @if collections.is_empty() {
                p.empty { "No collections configured." }
            } @else {
                table.collection-table {
                    thead {
                        tr {
                            th { "Collection" }
                            th { "Due Today" }
                            th { "Total Cards" }
                            th { "" }
                        }
                    }
                    tbody {
                        @for coll in collections {
                            tr class=@if coll.due_today == 0 && !resume.contains_key(&coll.slug) { "muted" } {
                                td { (coll.name.clone()) }
                                td.num { (coll.due_today) }
                                td.num { (coll.total_cards) }
                                td {
                                    @if let Some(remaining) = resume.get(&coll.slug) {
                                        a.drill-link.btn.btn-primary href=(format!("/collection/{}", coll.slug)) {
                                            (format!("Resume session ({remaining} cards remaining)"))
                                        }
                                    } @else if coll.due_today > 0 {
                                        a.drill-link.btn.btn-primary href=(format!("/collection/{}", coll.slug)) {
                                            "Drill"
                                        }
                                    } @else {
                                        span.no-cards { "Nothing due" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Fallible;

    /// BUG-45: counts older than the poll interval are stale; missing
    /// counts are always stale.
    #[test]
    fn test_counts_are_stale() -> Fallible<()> {
        let t0 = Timestamp::try_from("2026-01-01T10:00:00.000".to_string())?;
        let t29 = Timestamp::try_from("2026-01-01T10:29:00.000".to_string())?;
        let t30 = Timestamp::try_from("2026-01-01T10:30:00.000".to_string())?;
        assert!(counts_are_stale(None, t0, 30));
        assert!(!counts_are_stale(Some(t0), t29, 30));
        assert!(counts_are_stale(Some(t0), t30, 30));
        Ok(())
    }
}
