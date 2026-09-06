use std::collections::HashMap;

use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use maud::Markup;
use maud::html;

use crate::cmd::drill::template::page_template;
use crate::cmd::serve::auth::CurrentUser;
use crate::cmd::serve::counts::refresh_collection_info;
use crate::cmd::serve::decks::ResolvedCustomDeck;
use crate::cmd::serve::files::local_collections_for;
use crate::cmd::serve::handlers::deck_card_counts;
use crate::cmd::serve::handlers::deck_sources;
use crate::cmd::serve::state::AppState;
use crate::cmd::serve::state::CollectionInfo;
use crate::flash::Flash;

pub async fn landing_handler(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    current_user: Option<CurrentUser>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    let owner = current_user.as_ref().map(|u| u.email.clone());

    // Collections are discovered per request: a folder created a moment ago
    // must appear at once, and reading a directory is cheap next to the
    // count refresh that follows it.
    let local_infos = {
        let state = state.clone();
        let user = current_user.clone();
        match tokio::task::spawn_blocking(move || {
            refresh_collection_info(&local_collections_for(&state, user.as_ref()))
        })
        .await
        {
            Ok(infos) => infos,
            Err(e) => {
                log::error!("Failed to count local collections: {e}");
                Vec::new()
            }
        }
    };

    let collections: Vec<&CollectionInfo> = local_infos
        .iter()
        .filter(|c| c.owner.as_deref() == owner.as_deref())
        .collect();
    let config_available = state.config.data_dir.is_some();
    let custom_decks: Vec<ResolvedCustomDeck> = state
        .custom_decks
        .lock()
        .iter()
        .filter(|d| d.owner.as_deref() == owner.as_deref())
        .cloned()
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

    // Collections and decks are both things you sit down and drill, so they
    // share one list. A deck's counts are not cached anywhere — they are a
    // selection over collections — so they are computed here.
    let mut rows: Vec<DrillRow> = collections
        .iter()
        .map(|c| DrillRow {
            name: c.name.clone(),
            slug: c.slug.clone(),
            due_today: c.due_today,
            total_cards: c.total_cards,
            is_deck: false,
        })
        .collect();
    for deck in custom_decks {
        let state = state.clone();
        let user = current_user.clone();
        let counts = tokio::task::spawn_blocking(move || {
            let sources = deck_sources(&state, &deck, user.as_ref().map(|u| u.email.as_str()));
            deck_card_counts(&sources).map(|(due, total)| (deck, due, total))
        })
        .await;
        match counts {
            Ok(Ok((deck, due_today, total_cards))) => rows.push(DrillRow {
                name: deck.name.clone(),
                slug: deck.slug.clone(),
                due_today,
                total_cards,
                is_deck: true,
            }),
            // A deck whose members have gone missing must not take the page
            // down with it: it is listed, uncounted, and says so when opened.
            Ok(Err(e)) => log::error!("Failed to count a deck: {e}"),
            Err(e) => log::error!("Failed to count a deck: {e}"),
        }
    }

    let status = LandingStatus {
        config_available,
        signed_in_as: owner.clone(),
    };
    let html = render_landing_page(&rows, &resume, &status, flash);
    (StatusCode::OK, Html(html.into_string()))
}

/// The server-status details shown under the collection list: whether the
/// config file is in play, how the sources stand, and who is signed in.
struct LandingStatus {
    config_available: bool,
    /// The logged-in user's email, when `[oidc]` is configured. Drives the
    /// only logout control in the UI.
    signed_in_as: Option<String>,
}

/// One row of the list: a collection or a saved deck. Both are things you
/// sit down and drill, so the list does not separate them; only the tag on a
/// deck row says which is which.
struct DrillRow {
    name: String,
    slug: String,
    due_today: usize,
    total_cards: usize,
    is_deck: bool,
}

/// A row with nothing to do is dimmed, so the list reads as "these are the
/// ones waiting for you" at a glance.
fn row_class(row: &DrillRow, resume: &HashMap<String, usize>) -> &'static str {
    if row.due_today == 0 && !resume.contains_key(&row.slug) {
        "drill-row muted"
    } else {
        "drill-row"
    }
}

/// "36 due · 36 cards", or "Nothing due · 11 cards".
fn row_meta(due_today: usize, total_cards: usize) -> Markup {
    html! {
        @if due_today > 0 {
            span.row-due { (format!("{due_today} due")) }
        } @else {
            span.row-due.muted { "Nothing due" }
        }
        span.row-sep { "\u{00b7}" }
        span.row-total {
            (format!("{total_cards} card{}", if total_cards == 1 { "" } else { "s" }))
        }
    }
}

/// The one status line under the title: where the cards came from and when
/// they last arrived. It replaced a stack of full-width bars, each of which
/// announced a background detail as loudly as the list itself.
fn status_line(status: &LandingStatus) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(email) = &status.signed_in_as {
        parts.push(email.clone());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" \u{00b7} "))
    }
}

fn render_landing_page(
    rows: &[DrillRow],
    resume: &HashMap<String, usize>,
    status: &LandingStatus,
    flash: Option<Flash>,
) -> Markup {
    let LandingStatus {
        config_available,
        ref signed_in_as,
        ..
    } = *status;
    let line = status_line(status);
    page_template(html! {
        @if let Some(f) = &flash { (f.render()) }
        div.landing {
            // Title, then everything that is not the list, on one line each.
            // The list is what the page is for; the rest is upkeep.
            header.app-bar {
                h1 { "hashcards-web" }
                nav.app-nav {
                    @if config_available {
                        a.nav-link href="/files" { "My cards" }
                    }
                    @if signed_in_as.is_some() {
                        // POST, so a third-party page cannot log the user out
                        // by embedding the URL.
                        form action="/auth/logout" method="post" {
                            button.nav-link type="submit" { "Log out" }
                        }
                    }
                }
            }
            @if let Some(line) = line {
                p.app-status { (line) }
            }

            @if rows.is_empty() {
                p.empty { "No collections configured." }
            } @else {
                ul.drill-list {
                    @for row in rows {
                        // One `class` attribute: two would be emitted verbatim
                        // and the browser would keep only the first.
                        li class=(row_class(row, resume)) {
                            div.drill-row-main {
                                // The name opens the collection: topics,
                                // stats, bookmarks, export. The button skips
                                // all of it and starts drilling.
                                a.drill-row-name href=(format!("/collection/{}", row.slug)) {
                                    (row.name)
                                    @if row.is_deck { span.row-tag { "deck" } }
                                }
                                div.drill-row-meta { (row_meta(row.due_today, row.total_cards)) }
                            }
                            div.drill-row-action {
                                @if let Some(remaining) = resume.get(&row.slug) {
                                    a.btn.btn-primary href=(format!("/collection/{}", row.slug)) {
                                        (format!("Resume ({remaining} left)"))
                                    }
                                } @else if row.due_today > 0 {
                                    // One tap into card one. Every topic, no
                                    // limit — the choices live behind the name
                                    // for the sessions that want them.
                                    form action=(format!("/collection/{}/start", row.slug)) method="post" {
                                        input type="hidden" name="all_topics" value="1";
                                        input .btn.btn-primary type="submit" value="Drill";
                                    }
                                }
                            }
                        }
                    }
                }
            }
            @if config_available {
                p.list-foot {
                    a href="/decks" { "Decks \u{2192}" }
                    span.row-sep { "\u{00b7}" }
                    "a deck is a saved selection of topics from any of your collections"
                }
            }
        }
    })
}
