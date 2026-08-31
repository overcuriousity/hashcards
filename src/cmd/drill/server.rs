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

use std::collections::HashSet;
use std::fmt::Display;
use std::fmt::Formatter;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use parking_lot::Mutex;

use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::http::HeaderName;
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::http::header::CONTENT_TYPE;
use axum::response::Html;
use axum::routing::get;
use axum::routing::post;
use clap::ValueEnum;
use tokio::net::TcpListener;
use tokio::select;
use tokio::sync::oneshot::Receiver;
use tokio::sync::oneshot::channel;

use crate::cmd::drill::cache::Cache;
use crate::cmd::drill::get::get_handler;
use crate::cmd::drill::hljs::HLJS_CSS_URL;
use crate::cmd::drill::hljs::HLJS_JS_URL;
use crate::cmd::drill::hljs::hljs_css_handler;
use crate::cmd::drill::hljs::hljs_js_handler;
use crate::cmd::drill::katex::KATEX_CSS_URL;
use crate::cmd::drill::katex::KATEX_JS_URL;
use crate::cmd::drill::katex::KATEX_MHCHEM_JS_URL;
use crate::cmd::drill::katex::katex_css_handler;
use crate::cmd::drill::katex::katex_font_handler;
use crate::cmd::drill::katex::katex_js_handler;
use crate::cmd::drill::katex::katex_mhchem_js_handler;
use crate::cmd::drill::post::post_handler;
use crate::cmd::drill::state::MutableState;
use crate::cmd::drill::state::ServerState;
use crate::cmd::drill::stats::stats_handler;
use crate::cmd::drill::template::icon_192_handler;
use crate::cmd::drill::template::icon_512_handler;
use crate::cmd::drill::template::manifest_handler;
use crate::cmd::signals::terminate_signal;
use crate::collection::Collection;
use crate::db::Database;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::media::load::MediaLoader;
use crate::rng::TinyRng;
use crate::rng::shuffle;
use crate::types::card::Card;
use crate::types::card_hash::CardHash;
use crate::types::date::Date;
use crate::types::performance::Jitter;
use crate::types::timestamp::Timestamp;
use crate::utils::CACHE_CONTROL_IMMUTABLE;

#[derive(ValueEnum, Clone, Copy, PartialEq)]
pub enum AnswerControls {
    /// Show all four rating buttons (Forgot/Hard/Good/Easy).
    Full,
    /// Show only two rating buttons (Forgot/Good).
    Binary,
}

impl Display for AnswerControls {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AnswerControls::Full => write!(f, "full"),
            AnswerControls::Binary => write!(f, "binary"),
        }
    }
}

pub struct ServerConfig {
    pub directory: Option<String>,
    pub host: String,
    pub port: u16,
    pub session_started_at: Timestamp,
    pub card_limit: Option<usize>,
    pub new_card_limit: Option<usize>,
    pub deck_filter: Option<String>,
    pub shuffle: bool,
    /// Fractional random jitter applied to computed intervals.
    pub jitter: Jitter,
    pub answer_controls: AnswerControls,
    pub bury_siblings: bool,
}

pub async fn start_server(config: ServerConfig) -> Fallible<()> {
    let Collection {
        directory,
        db,
        cards,
        macros,
        duplicates,
    } = Collection::new(config.directory)?;

    // BUG-12: byte-identical cards are deduplicated at load time. Warn so
    // the user can clean them up; drilling proceeds with one copy.
    for duplicate in &duplicates {
        eprintln!("Warning: {duplicate}. Drilling with one copy.");
    }

    let today: Date = config.session_started_at.date();

    let db_hashes: HashSet<CardHash> = db.card_hashes()?;
    // If a card is in the directory, but not in the DB, it is new. Add it to
    // the database.
    for card in cards.iter() {
        if !db_hashes.contains(&card.hash()) {
            db.insert_card(card.hash(), config.session_started_at)?;
        }
    }

    // Find cards due today.
    let due_today: HashSet<CardHash> = db.due_today(today)?;
    let due_today: Vec<Card> = cards
        .into_iter()
        .filter(|card| due_today.contains(&card.hash()))
        .collect::<Vec<_>>();

    let due_today: Vec<Card> = filter_deck(
        &db,
        due_today,
        config.card_limit,
        config.new_card_limit,
        config.deck_filter,
    )?;

    let due_today: Vec<Card> = if config.bury_siblings {
        bury_siblings(due_today)
    } else {
        due_today
    };

    if due_today.is_empty() {
        println!("No cards due today.");
        return Ok(());
    }

    // Seed a session RNG, used for shuffling and interval jitter.
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| {
            ErrorReport::new(format!(
                "The system clock is set before the Unix epoch: {e}"
            ))
        })?
        .as_nanos() as u64;
    let mut rng = TinyRng::from_seed(seed);
    let due_today: Vec<Card> = if config.shuffle {
        shuffle(due_today, &mut rng)
    } else {
        due_today
    };

    // For all cards due today, fetch their performance from the database and store it in the cache.
    let mut cache = Cache::new();
    for card in due_today.iter() {
        let performance = db.get_card_performance(card.hash())?;
        cache.insert(card.hash(), performance)?;
    }

    // Create the session row immediately so reviews can be written as they happen.
    let session_id = db.create_session(config.session_started_at)?;

    // Create shutdown channel
    let (shutdown_tx, shutdown_rx) = channel();

    let state = ServerState {
        port: config.port,
        directory,
        macros,
        total_cards: due_today.len(),
        session_started_at: config.session_started_at,
        mutable: Arc::new(Mutex::new(MutableState {
            reveal: false,
            db,
            session_id,
            cache,
            cards: due_today,
            reviews: Vec::new(),
            finished_at: None,
            card_shown_at: None,
            jitter: config.jitter,
            rng,
        })),
        shutdown_tx: Arc::new(Mutex::new(Some(shutdown_tx))),
        answer_controls: config.answer_controls,
    };
    let app = Router::new();
    let app = app.route("/", get(get_handler));
    let app = app.route("/", post(post_handler));
    let app = app.route("/stats", get(stats_handler));
    let app = app.route("/manifest.json", get(manifest_handler));
    let app = app.route("/icons/icon-192.png", get(icon_192_handler));
    let app = app.route("/icons/icon-512.png", get(icon_512_handler));
    let app = app.route("/script.js", get(script_handler));
    let app = app.route("/style.css", get(style_handler));
    let app = app.route(KATEX_CSS_URL, get(katex_css_handler));
    let app = app.route(KATEX_JS_URL, get(katex_js_handler));
    let app = app.route(KATEX_MHCHEM_JS_URL, get(katex_mhchem_js_handler));
    let app = app.route("/katex/fonts/{*path}", get(katex_font_handler));
    let app = app.route(HLJS_CSS_URL, get(hljs_css_handler));
    let app = app.route(HLJS_JS_URL, get(hljs_js_handler));
    let app = app.route("/file/{*path}", get(file_handler));
    let app = app.fallback(not_found_handler);
    let app = app.with_state(state.clone());
    let bind = format!("{}:{}", config.host, config.port);

    // Start the server with graceful shutdown on Ctrl+C or shutdown button.
    log::debug!("Starting server on {bind}");
    let listener = TcpListener::bind(bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_rx))
        .await?;

    // Check if session was complete when server shut down
    let mutable = state.mutable.lock();
    if mutable.finished_at.is_some() {
        Ok(())
    } else {
        // Interrupted (e.g. Ctrl+C). Reviews persist as they happen, so
        // nothing is lost: close the session row and exit cleanly.
        let summary = finalize_interrupted_session(&mutable)?;
        println!("{summary}");
        Ok(())
    }
}

/// Close an interrupted session's DB row and describe what was preserved.
///
/// Every grade is written to the database the moment it happens, so an
/// interrupt loses nothing; this only stamps `ended_at` and counts the
/// persisted (non-voided) reviews for the exit message.
pub fn finalize_interrupted_session(mutable: &MutableState) -> Fallible<String> {
    mutable
        .db
        .close_session(mutable.session_id, Timestamp::now())?;
    let count = mutable
        .db
        .get_reviews_for_session(mutable.session_id)?
        .len();
    let noun = if count == 1 { "review" } else { "reviews" };
    Ok(format!("Session interrupted. {count} {noun} saved."))
}

async fn script_handler(
    State(state): State<ServerState>,
) -> (StatusCode, [(HeaderName, &'static str); 1], String) {
    let mut content = String::new();
    content.push_str("let MACROS = {};\n");
    for (name, definition) in &state.macros {
        let name = escape_js_string_literal(name);
        let definition = escape_js_string_literal(definition);
        content.push_str(&format!("MACROS['{name}'] = '{definition}';\n"));
    }
    content.push('\n');
    content.push_str(include_str!("script.js"));
    (StatusCode::OK, [(CONTENT_TYPE, "text/javascript")], content)
}

pub fn escape_js_string_literal(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace('$', "\\$")
}

async fn style_handler() -> (StatusCode, [(HeaderName, &'static str); 2], &'static [u8]) {
    let bytes = include_bytes!("style.css");
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, "text/css"),
            (CACHE_CONTROL, CACHE_CONTROL_IMMUTABLE),
        ],
        bytes,
    )
}

async fn not_found_handler() -> (StatusCode, Html<String>) {
    (StatusCode::NOT_FOUND, Html("Not Found".to_string()))
}

async fn file_handler(
    State(state): State<ServerState>,
    Path(path): Path<String>,
) -> (StatusCode, [(HeaderName, &'static str); 1], Vec<u8>) {
    let loader = match MediaLoader::new(state.directory.clone()) {
        Ok(loader) => loader,
        Err(error) => {
            log::error!("Failed to create media loader: {error}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(CONTENT_TYPE, "text/plain")],
                b"Internal Server Error".to_vec(),
            );
        }
    };
    let validated_path: PathBuf = match loader.validate(&path) {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                [(CONTENT_TYPE, "text/plain")],
                b"Not Found".to_vec(),
            );
        }
    };
    let extension = validated_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();
    let content_type: &str = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    };
    let content = tokio::fs::read(validated_path).await;
    match content {
        Ok(bytes) => (StatusCode::OK, [(CONTENT_TYPE, content_type)], bytes),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(CONTENT_TYPE, "text/plain")],
            b"Internal Server Error".to_vec(),
        ),
    }
}

async fn shutdown_signal(shutdown_rx: Receiver<()>) {
    let shutdown = async {
        shutdown_rx.await.ok();
    };

    select! {
        _ = terminate_signal() => {
            log::debug!("Received termination signal, shutting down gracefully");
        },
        _ = shutdown => {
            log::debug!("Received shutdown signal, shutting down gracefully");
        },
    }
}

fn filter_deck(
    db: &Database,
    deck: Vec<Card>,
    card_limit: Option<usize>,
    new_card_limit: Option<usize>,
    deck_filter: Option<String>,
) -> Fallible<Vec<Card>> {
    // Apply the deck filter.
    let deck = match deck_filter {
        Some(filter) => deck
            .into_iter()
            .filter(|card| card.deck_name() == &filter)
            .collect(),
        None => deck,
    };

    // Apply the card limit.
    let deck = match card_limit {
        Some(limit) => deck.into_iter().take(limit).collect(),
        None => deck,
    };

    // Apply the new card limit.
    let deck = match new_card_limit {
        Some(limit) => {
            let mut new_count = 0;
            let mut result = Vec::new();
            for card in deck.into_iter() {
                if db.get_card_performance(card.hash())?.is_new() {
                    if new_count < limit {
                        result.push(card);
                        new_count += 1;
                    }
                } else {
                    result.push(card);
                }
            }
            result
        }
        None => deck,
    };

    Ok(deck)
}

fn bury_siblings(deck: Vec<Card>) -> Vec<Card> {
    let mut seen_families = HashSet::new();
    let mut result = Vec::new();
    for card in deck.into_iter() {
        if let Some(family) = card.family_hash() {
            if seen_families.contains(&family) {
                continue;
            }
            seen_families.insert(family);
        }
        result.push(card);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::drill::cache::Cache;
    use crate::db::ReviewRecord;
    use crate::fsrs::Grade;
    use crate::types::performance::Jitter;
    use crate::types::performance::Performance;
    use crate::types::performance::update_performance;

    /// BUG-10: interrupting a drill closes the session row and reports how
    /// many reviews were persisted, instead of erroring out.
    #[test]
    fn test_finalize_interrupted_session_closes_row_and_summarizes() -> Fallible<()> {
        let mut db = Database::new(":memory:")?;
        let started_at = Timestamp::now();
        let session_id = db.create_session(started_at)?;
        let hash = CardHash::hash_bytes(b"card");
        db.insert_card(hash, started_at)?;
        // Persist one review, exactly as the grade path does.
        let mut rng = TinyRng::from_seed(1);
        let performance = update_performance(
            Performance::New,
            Grade::Good,
            started_at,
            Jitter::none(),
            &mut rng,
        );
        let record = ReviewRecord {
            card_hash: hash,
            reviewed_at: started_at,
            grade: Grade::Good,
            stability: performance.stability,
            difficulty: performance.difficulty,
            interval_raw: performance.interval_raw,
            interval_days: performance.interval_days,
            due_date: performance.due_date,
            duration_ms: Some(1500),
        };
        db.insert_review_and_update_performance(
            session_id,
            &record,
            Performance::Reviewed(performance),
        )?;
        let mutable = MutableState::new(
            db,
            session_id,
            Cache::new(),
            Vec::new(),
            Jitter::none(),
            TinyRng::from_seed(1),
        );

        let summary = finalize_interrupted_session(&mutable)?;

        assert_eq!(summary, "Session interrupted. 1 review saved.");
        // The session row is closed: get_all_sessions decodes ended_at as a
        // non-null Timestamp, so it only succeeds once the row is closed.
        let sessions = mutable.db.get_all_sessions()?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, session_id);
        Ok(())
    }
}
