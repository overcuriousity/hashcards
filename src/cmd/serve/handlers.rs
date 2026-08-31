use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use parking_lot::Mutex;

use axum::Form;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::HeaderName;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::Html;
use axum::response::Redirect;
use maud::html;

use crate::flash::Flash;

use crate::cmd::drill::cache::Cache;
use crate::cmd::drill::get::CompletionAction;
use crate::cmd::drill::get::RenderContext;
use crate::cmd::drill::get::render_completion_page;
use crate::cmd::drill::get::render_session_page;
use crate::cmd::drill::post::Action;
use crate::cmd::drill::post::ActionResult;
use crate::cmd::drill::post::FormData;
use crate::cmd::drill::post::handle_action;
use crate::cmd::drill::server::escape_js_string_literal;
use crate::cmd::drill::state::MutableState;
use crate::cmd::drill::template::page_template;
use crate::cmd::drill::template::page_template_with_script;
use crate::cmd::serve::browse::build_deck_tree;
use crate::cmd::serve::browse::render_browse_page;
use crate::cmd::serve::config::HedgedocEntry;
use crate::cmd::serve::config::ResolvedCollection;
use crate::cmd::serve::git::clone_or_pull;
use crate::cmd::serve::hedgedoc::all_hedgedoc_entries;
use crate::cmd::serve::hedgedoc::normalize_hedgedoc_url;
use crate::cmd::serve::hedgedoc::build_combined_infos;
use crate::cmd::serve::hedgedoc::build_note;
use crate::cmd::serve::hedgedoc::build_source;
use crate::cmd::serve::hedgedoc::create_minimal_config;
use crate::cmd::serve::hedgedoc::persist_hedgedoc_entries;
use crate::cmd::serve::hedgedoc::source_uri_from_url;
use crate::cmd::serve::hedgedoc::sync_source;
use crate::cmd::serve::state::AppState;
use crate::cmd::serve::state::DrillSession;
use crate::cmd::serve::state::SharedSession;
use crate::collection::Collection;
use crate::db::Database;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::error::fail;
use crate::media::load::MediaLoader;
use crate::rng::TinyRng;
use crate::rng::shuffle;
use crate::types::card::Card;
use crate::types::card_hash::CardHash;
use crate::types::date::Date;
use crate::types::timestamp::Timestamp;

pub async fn collection_get_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    // Determine whether this slug is known before calling the inner function,
    // so we can return 404 for unknown collections vs. 500 for real errors.
    let known = find_collection(&state, &slug).is_some()
        || state.sessions.lock().contains_key(&slug);
    match collection_get_inner(&state, &slug, flash) {
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

fn collection_get_inner(state: &AppState, slug: &str, flash: Option<Flash>) -> Fallible<String> {
    // Clone the Arc out of the map so the map lock is not held during
    // rendering; the session itself stays in the map even if rendering fails.
    let session: Option<SharedSession> = state.sessions.lock().get(slug).cloned();

    let Some(session) = session else {
        // No active session: show the deck browser.
        let rc = find_collection(state, slug)
            .ok_or_else(|| crate::error::ErrorReport::new(format!("Unknown collection: {slug}")))?;
        let tree = build_deck_tree(&rc.coll_dir, &rc.db_path)?;
        // Build a deck-name → HedgeDoc URL map so the browse page can show edit
        // links. All URLs were already validated as HTTPS when added. If two notes
        // share the same deck name the edit link is suppressed for that deck to
        // avoid pointing at an arbitrary note.
        let hedge_urls: std::collections::HashMap<String, String> = {
            let sources = state.hedgedoc_sources.lock();
            let mut map = std::collections::HashMap::new();
            let mut dupes: HashSet<String> = HashSet::new();
            for (deck_name, url) in sources
                .iter()
                .filter(|s| s.collection.slug == slug)
                .flat_map(|s| s.notes.iter().map(|n| (n.deck_name.clone(), n.url.clone())))
            {
                if dupes.contains(&deck_name) {
                    continue;
                }
                if map.insert(deck_name.clone(), url).is_some() {
                    dupes.insert(deck_name.clone());
                    map.remove(&deck_name);
                }
            }
            map
        };
        let db_path = rc.db_path.to_str().ok_or_else(|| {
            crate::error::ErrorReport::new(format!(
                "Database path is not valid UTF-8: {}",
                rc.db_path.display()
            ))
        })?;
        let bookmark_count = Database::new(db_path)?.count_bookmarks()?;
        let html = render_browse_page(&rc.name, slug, &tree, &hedge_urls, bookmark_count, flash);
        return Ok(html.into_string());
    };

    let session = session.lock();
    let form_action = format!("/collection/{slug}");
    let file_url_prefix = format!("/collection/{slug}/file");
    let ctx = RenderContext {
        directory: &session.directory,
        total_cards: session.total_cards,
        session_started_at: session.session_started_at,
        answer_controls: session.answer_controls,
        form_action: &form_action,
        file_url_prefix: &file_url_prefix,
        completion_action: CompletionAction::BackToCollections,
    };
    let body = if session.mutable.finished_at.is_some() {
        render_completion_page(&ctx, &session.mutable)?
    } else {
        render_session_page(&ctx, &session.mutable)?
    };
    let body = html! {
        @if let Some(f) = &flash { (f.render()) }
        (body)
    };
    let script_url = format!("/collection/{slug}/script.js");
    let html = page_template_with_script(&script_url, body);
    Ok(html.into_string())
}

pub(super) fn find_collection(state: &AppState, slug: &str) -> Option<ResolvedCollection> {
    if let Some(rc) = state.config.collections.iter().find(|c| c.slug == slug) {
        return Some(rc.clone());
    }
    let sources = state.hedgedoc_sources.lock();
    sources
        .iter()
        .find(|s| s.collection.slug == slug)
        .map(|s| s.collection.clone())
}

/// Form data for the start-drill endpoint.
pub struct StartDrillForm {
    pub decks: Vec<String>,
    /// Optional card limit: `0` or absent means "all due cards".
    pub limit: Option<usize>,
}

/// Custom `Deserialize` for `StartDrillForm`.
///
/// `serde_urlencoded` presents repeated keys (`decks=foo&decks=bar`) as
/// separate map entries rather than grouping them into a sequence first.
/// The derived `Deserialize` macro errors on "duplicate field" in that case,
/// so we implement the visitor manually to accumulate all `decks` values.
impl<'de> serde::Deserialize<'de> for StartDrillForm {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};

        struct FormVisitor;

        impl<'de> Visitor<'de> for FormVisitor {
            type Value = StartDrillForm;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a form with optional decks and limit fields")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut decks = Vec::new();
                let mut limit: Option<usize> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "decks" => decks.push(map.next_value::<String>()?),
                        "limit" => {
                            let raw = map.next_value::<String>()?;
                            if let Ok(n) = raw.parse::<usize>() {
                                if n > 0 {
                                    limit = Some(n);
                                }
                            }
                        }
                        _ => {
                            let _ = map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(StartDrillForm { decks, limit })
            }
        }

        deserializer.deserialize_map(FormVisitor)
    }
}

pub async fn collection_start_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Form(form): Form<StartDrillForm>,
) -> Redirect {
    match collection_start_inner(&state, &slug, form.decks, form.limit) {
        Ok(()) => Redirect::to(&format!("/collection/{slug}")),
        Err(e) => {
            log::error!("error starting drill for collection {slug}: {e}");
            Flash::error(e.to_string()).redirect(&format!("/collection/{slug}"))
        }
    }
}

fn collection_start_inner(
    state: &AppState,
    slug: &str,
    selected_decks: Vec<String>,
    limit: Option<usize>,
) -> Fallible<()> {
    if selected_decks.is_empty() {
        return fail("Select at least one deck.");
    }
    // Remove any existing session before doing DB work.
    state.sessions.lock().remove(slug);

    // Create the session outside the lock (may do filesystem/DB work).
    let session = create_session(state, slug, &selected_decks, limit)?;
    if let Some(s) = session {
        state
            .sessions
            .lock()
            .insert(slug.to_string(), Arc::new(Mutex::new(s)));
    }
    Ok(())
}

fn create_session(
    state: &AppState,
    slug: &str,
    selected_decks: &[String],
    limit: Option<usize>,
) -> Fallible<Option<DrillSession>> {
    let rc = find_collection(state, slug)
        .ok_or_else(|| crate::error::ErrorReport::new(format!("Unknown collection: {slug}")))?;

    let collection = Collection::with_db_path(rc.coll_dir.clone(), rc.db_path.clone())?;

    let session_started_at = Timestamp::now();
    let today: Date = session_started_at.date();

    // Sync new cards to DB
    let db_hashes: HashSet<CardHash> = collection.db.card_hashes()?;
    for card in collection.cards.iter() {
        if !db_hashes.contains(&card.hash()) {
            collection.db.insert_card(card.hash(), session_started_at)?;
        }
    }

    // Filter by selected decks.
    let deck_filter: HashSet<&str> = selected_decks.iter().map(|s| s.as_str()).collect();
    let cards: Vec<Card> = if deck_filter.is_empty() {
        collection.cards
    } else {
        collection
            .cards
            .into_iter()
            .filter(|card| deck_filter.contains(card.deck_name().as_str()))
            .collect()
    };

    // Find cards due today
    let due_today: HashSet<CardHash> = collection.db.due_today(today)?;
    let mut due_cards: Vec<Card> = cards
        .into_iter()
        .filter(|card| due_today.contains(&card.hash()))
        .collect();

    if state.config.defaults.bury_siblings {
        due_cards = bury_siblings(due_cards);
    }

    if due_cards.is_empty() {
        return Ok(None);
    }

    // Shuffle
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| {
            ErrorReport::new(format!("The system clock is set before the Unix epoch: {e}"))
        })?
        .as_nanos() as u64;
    let mut rng = TinyRng::from_seed(seed);
    due_cards = shuffle(due_cards, &mut rng);

    // Apply session card limit if requested.
    if let Some(n) = limit {
        due_cards.truncate(n);
    }

    // Build cache
    let mut cache = Cache::new();
    for card in due_cards.iter() {
        let performance = collection.db.get_card_performance(card.hash())?;
        cache.insert(card.hash(), performance)?;
    }

    // Create the session row immediately so reviews can be written as they happen.
    let session_id = collection.db.create_session(session_started_at)?;

    let answer_controls = state.config.defaults.answer_controls.into();

    Ok(Some(DrillSession::new(
        collection.directory,
        collection.macros,
        session_started_at,
        answer_controls,
        MutableState::new(collection.db, session_id, cache, due_cards),
    )))
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

pub async fn collection_post_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Form(form): Form<FormData>,
) -> Redirect {
    match collection_post_inner(&state, &slug, form) {
        Ok(redirect) => redirect,
        Err(e) => {
            log::error!("error handling action for collection {slug}: {e}");
            Flash::error(e.to_string()).redirect(&format!("/collection/{slug}"))
        }
    }
}

fn collection_post_inner(state: &AppState, slug: &str, form: FormData) -> Fallible<Redirect> {
    let action = form.action;
    let submitted_card: Option<CardHash> = match form.card.as_deref() {
        Some(hex) => match CardHash::from_hex(hex) {
            Ok(hash) => Some(hash),
            Err(_) => {
                log::debug!("ignoring grade with unparseable card hash for collection {slug}");
                return Ok(Flash::error(
                    "That grade carried an invalid card reference and was ignored.",
                )
                .redirect(&format!("/collection/{slug}")));
            }
        },
        None => None,
    };
    // Home action: close the session and drop it without needing to hold the
    // global lock during DB work.
    if matches!(action, Action::Home) {
        let session = state.sessions.lock().remove(slug);
        if let Some(s) = session {
            let s = s.lock();
            if s.mutable.finished_at.is_none() {
                if let Err(e) = s
                    .mutable
                    .db
                    .close_session(s.mutable.session_id, Timestamp::now())
                {
                    log::error!(
                        "failed to close session {} for collection {slug}: {e}",
                        s.mutable.session_id
                    );
                }
            }
        }

        // Snapshot inputs, then compute + update in background (don't block response).
        let sources_snapshot = state.hedgedoc_sources.lock().clone();
        let static_collections = state.config.collections.clone();
        let collections_clone = state.collections.clone();
        tokio::spawn(async move {
            let combined = build_combined_infos(&static_collections, &sources_snapshot);
            *collections_clone.write().await = combined;
        });

        return Ok(Redirect::to("/"));
    }

    // Lock the session in place: the map lock is released immediately, the
    // per-slug lock is held for the DB work, and an error leaves the session
    // in the map untouched.
    let session: SharedSession = match state.sessions.lock().get(slug).cloned() {
        Some(s) => s,
        None => return Ok(Redirect::to(&format!("/collection/{slug}"))),
    };
    let mut guard = session.lock();

    // Re-check that this is still the session the map holds for `slug`: a
    // concurrent Home request may have removed it (and closed its DB row)
    // while we were waiting for the session lock, or a concurrent start may
    // have replaced it with a new drill. Take the map lock in its own scoped
    // block so it is never held while the session lock is held for longer
    // than this check (Home never holds the two locks together either, so
    // this ordering can't deadlock against it).
    let still_current = {
        let map = state.sessions.lock();
        matches!(map.get(slug), Some(current) if Arc::ptr_eq(&session, current))
    };
    if !still_current {
        return Ok(
            Flash::error("The session has ended, so that action was ignored.")
                .redirect(&format!("/collection/{slug}")),
        );
    }

    let session = &mut *guard;

    // `Action::Home` returned early above, and it is the only action for which
    // `handle_action` yields `ActionResult::Home`. Every action reaching here
    // leaves the session running; the only result needing dispatch is
    // `ContinueWithFlash`, which carries a one-shot message for the user.
    let result = handle_action(
        &mut session.mutable,
        session.session_started_at,
        action,
        submitted_card,
    )?;

    match result {
        ActionResult::ContinueWithFlash(flash) => {
            Ok(flash.redirect(&format!("/collection/{slug}")))
        }
        ActionResult::Ignored(reason) => {
            Ok(Flash::error(reason).redirect(&format!("/collection/{slug}")))
        }
        _ => Ok(Redirect::to(&format!("/collection/{slug}"))),
    }
}

pub async fn collection_file_handler(
    State(state): State<AppState>,
    Path((slug, path)): Path<(String, String)>,
) -> (StatusCode, [(HeaderName, &'static str); 1], Vec<u8>) {
    let coll_dir = match find_collection(&state, &slug) {
        Some(rc) => rc.coll_dir.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                [(CONTENT_TYPE, "text/plain")],
                b"Collection not found".to_vec(),
            );
        }
    };

    let loader = match MediaLoader::new(coll_dir) {
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

pub async fn collection_script_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> (StatusCode, [(HeaderName, &'static str); 1], String) {
    let session: Option<SharedSession> = state.sessions.lock().get(&slug).cloned();
    let macros: Vec<(String, String)> = match session {
        Some(session) => session.lock().macros.clone(),
        None => {
            // No active session; serve script without macros
            let content = format!(
                "let MACROS = {{}};\n\n{}",
                include_str!("../drill/script.js")
            );
            return (StatusCode::OK, [(CONTENT_TYPE, "text/javascript")], content);
        }
    };
    let mut content = String::new();
    content.push_str("let MACROS = {};\n");
    for (name, definition) in &macros {
        let name = escape_js_string_literal(name);
        let definition = escape_js_string_literal(definition);
        content.push_str(&format!("MACROS['{name}'] = '{definition}';\n"));
    }
    content.push('\n');
    content.push_str(include_str!("../drill/script.js"));
    (StatusCode::OK, [(CONTENT_TYPE, "text/javascript")], content)
}

pub async fn sync_handler(State(state): State<AppState>) -> Redirect {
    let git = match &state.config.git {
        Some(git) => git,
        None => {
            return Flash::error("Sync is not available: no git repository is configured.")
                .redirect("/");
        }
    };

    match clone_or_pull(&git.repo_url, &git.branch, &git.repo_dir).await {
        Ok(()) => {
            let sources_snapshot = state.hedgedoc_sources.lock().clone();
            let combined = build_combined_infos(&state.config.collections, &sources_snapshot);
            *state.collections.write().await = combined;
            *state.last_synced.lock() = Some(Timestamp::now());
            log::debug!("Manual sync completed successfully");
            Flash::success("Sync complete.").redirect("/")
        }
        Err(e) => {
            log::error!("Manual sync failed: {e}");
            Flash::error(format!("Sync failed: {e}")).redirect("/")
        }
    }
}

// ---- HedgeDoc management handlers ----

/// Render the HedgeDoc source management page.
pub async fn hedgedoc_manage_handler(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    use crate::cmd::serve::hedgedoc_ui::render_manage_page;
    let flash = Flash::from_query(&query);
    let sources = state.hedgedoc_sources.lock();
    let last_synced = *state.hedgedoc_last_synced.lock();
    let config_available = state.config.data_dir.is_some();
    let html = render_manage_page(&sources, last_synced, config_available, flash);
    (StatusCode::OK, Html(html.into_string()))
}

#[derive(serde::Deserialize)]
pub struct AddHedgedocForm {
    pub url: String,
}

/// Add a new HedgeDoc source URL.
pub async fn hedgedoc_add_handler(
    State(state): State<AppState>,
    Form(form): Form<AddHedgedocForm>,
) -> Redirect {
    // Normalize and validate the URL at storage time (BUG-24): strip
    // query/fragment/trailing slash so equivalent URLs dedupe, and reject
    // anything that is not a well-formed HTTPS URL so no raw string is
    // ever persisted or rendered into an href.
    let url = match normalize_hedgedoc_url(&form.url) {
        Ok(url) => url,
        Err(e) => {
            return Flash::error(format!("Invalid HedgeDoc URL: {e}")).redirect("/hedgedoc");
        }
    };

    let data_dir = match &state.config.data_dir {
        Some(d) => d.clone(),
        None => {
            log::error!("Cannot add HedgeDoc source: no data_dir configured");
            return Flash::error(
                "Cannot add HedgeDoc source: no data directory is configured. Start hashcards with --config.",
            )
            .redirect("/hedgedoc");
        }
    };

    // Check for duplicate URL.
    {
        let sources = state.hedgedoc_sources.lock();
        if sources
            .iter()
            .flat_map(|s| s.notes.iter())
            .any(|n| n.url == url)
        {
            return Flash::error("This note is already added.").redirect("/hedgedoc");
        }
    }

    let source_uri = match source_uri_from_url(&url) {
        Some(uri) => uri,
        None => {
            log::error!("Failed to parse HedgeDoc source URI from {url}");
            return Flash::error(format!("Could not parse a HedgeDoc note URL from: {url}"))
                .redirect("/hedgedoc");
        }
    };

    let existing_collection = {
        let sources = state.hedgedoc_sources.lock();
        sources
            .iter()
            .find(|s| s.source_uri == source_uri)
            .map(|s| s.collection.clone())
    };

    let mut new_source: Option<crate::cmd::serve::state::HedgedocSource> = None;
    let mut new_note: Option<crate::cmd::serve::state::HedgedocNote> = None;

    if let Some(collection) = existing_collection {
        match build_note(&url, &collection).await {
            Ok(note) => new_note = Some(note),
            Err(e) => {
                log::error!("Failed to add HedgeDoc note {url}: {e}");
                return Flash::error(format!("Failed to add HedgeDoc note: {e}"))
                    .redirect("/hedgedoc");
            }
        }
    } else {
        match build_source(&url, &data_dir).await {
            Ok(source) => new_source = Some(source),
            Err(e) => {
                log::error!("Failed to add HedgeDoc source {url}: {e}");
                return Flash::error(format!("Failed to add HedgeDoc source: {e}"))
                    .redirect("/hedgedoc");
            }
        }
    }

    // Compute what the new entries list would be without mutating shared state yet.
    // This lets us persist to disk first; we only update in-memory state after success.
    let new_entries: Vec<HedgedocEntry> = {
        let sources = state.hedgedoc_sources.lock();
        if new_source.is_some() {
            if sources
                .iter()
                .flat_map(|s| s.notes.iter())
                .any(|n| n.url == url)
            {
                return Flash::error("This note is already added.").redirect("/hedgedoc");
            }
        } else if new_note.is_some() {
            if let Some(src) = sources.iter().find(|s| s.source_uri == source_uri) {
                if src.notes.iter().any(|n| n.url == url) {
                    return Flash::error("This note is already added.").redirect("/hedgedoc");
                }
            }
        }
        let mut updated = sources.clone();
        if let Some(ref source) = new_source {
            updated.push(source.clone());
        } else if let Some(ref note) = new_note {
            if let Some(src) = updated.iter_mut().find(|s| s.source_uri == source_uri) {
                src.notes.push(note.clone());
            }
        }
        all_hedgedoc_entries(&updated)
    };

    // Get or create config path outside the lock, using spawn_blocking for the
    // filesystem work so we don't block the async runtime.
    let maybe_config_path = state.config_path.lock().clone();
    let config_path = match maybe_config_path {
        Some(p) => p,
        None => {
            let data_dir_owned = data_dir.clone();
            let p =
                match tokio::task::spawn_blocking(move || create_minimal_config(&data_dir_owned))
                    .await
                    .map_err(|e| {
                        crate::error::ErrorReport::new(format!(
                            "Config creation task panicked: {e}"
                        ))
                    }) {
                    Ok(Ok(p)) => p,
                    Ok(Err(e)) | Err(e) => {
                        log::error!("Failed to create minimal config file: {e}");
                        return Flash::error(format!("Failed to create config file: {e}"))
                            .redirect("/hedgedoc");
                    }
                };
            *state.config_path.lock() = Some(p.clone());
            // The config now references the temp data dir; stop cleanup on exit.
            if let Some(tracker) = state.config._temp_dir.as_ref() {
                tracker.dismiss();
            }
            p
        }
    };

    // Persist to TOML via spawn_blocking (atomic write).
    let config_path_owned = config_path.clone();
    let entries_owned = new_entries.clone();
    if let Err(e) = tokio::task::spawn_blocking(move || {
        persist_hedgedoc_entries(&config_path_owned, &entries_owned)
    })
    .await
    .map_err(|e| crate::error::ErrorReport::new(format!("Persist task panicked: {e}")))
    .and_then(|r| r)
    {
        log::error!("Failed to persist HedgeDoc entries to config: {e}");
        return Flash::error(format!("Failed to save HedgeDoc sources to config: {e}"))
            .redirect("/hedgedoc");
    }

    // Persist succeeded: now update in-memory state.
    {
        let mut sources = state.hedgedoc_sources.lock();
        if let Some(source) = new_source {
            sources.push(source);
        } else if let Some(note) = new_note {
            if let Some(src) = sources.iter_mut().find(|s| s.source_uri == source_uri) {
                src.notes.push(note);
            }
        }
    }

    // Refresh combined collection infos.
    let sources_snapshot = state.hedgedoc_sources.lock().clone();
    let combined = build_combined_infos(&state.config.collections, &sources_snapshot);
    *state.collections.write().await = combined;

    // Update last synced time if the newly added note fetched without error.
    if sources_snapshot
        .iter()
        .flat_map(|s| s.notes.iter())
        .any(|n| n.url == url && n.last_error.is_none())
    {
        *state.hedgedoc_last_synced.lock() = Some(Timestamp::now());
    }

    Flash::success("HedgeDoc source added.").redirect("/hedgedoc")
}

#[derive(serde::Deserialize)]
pub struct DeleteHedgedocForm {
    pub url: String,
}

/// Remove a HedgeDoc source by URL.
pub async fn hedgedoc_delete_handler(
    State(state): State<AppState>,
    Form(form): Form<DeleteHedgedocForm>,
) -> Redirect {
    // Phase 1: compute the post-deletion entries without mutating shared state.
    let (remaining, new_sources) = {
        let sources = state.hedgedoc_sources.lock();
        let mut working = sources.clone();
        for src in working.iter_mut() {
            src.notes.retain(|n| n.url != form.url);
        }
        working.retain(|s| !s.notes.is_empty());
        let entries = all_hedgedoc_entries(&working);
        (entries, working)
    };

    // Phase 2: persist to TOML if config file is available, via spawn_blocking.
    // Extract config path before any await so the MutexGuard is dropped first.
    let maybe_config_path: Option<std::path::PathBuf> = state.config_path.lock().clone();
    let persist_result: Result<(), String> = if let Some(config_path) = maybe_config_path {
        let remaining_for_persist = remaining.clone();
        match tokio::task::spawn_blocking(move || {
            persist_hedgedoc_entries(&config_path, &remaining_for_persist)
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                log::error!("Failed to persist HedgeDoc entries after deletion: {e}");
                Err(e.to_string())
            }
            Err(e) => {
                log::error!("Persist task panicked after deletion: {e}");
                Err(e.to_string())
            }
        }
    } else {
        Ok(()) // no config file to persist to, treat as success
    };

    // Only update in-memory state if persist succeeded (or was not needed).
    match persist_result {
        Ok(()) => {
            *state.hedgedoc_sources.lock() = new_sources.clone();
            let combined = build_combined_infos(&state.config.collections, &new_sources);
            *state.collections.write().await = combined;
            Flash::success("HedgeDoc source removed.").redirect("/hedgedoc")
        }
        Err(msg) => {
            Flash::error(format!("Failed to remove HedgeDoc source: {msg}")).redirect("/hedgedoc")
        }
    }
}

/// Manually re-sync all HedgeDoc sources.
pub async fn hedgedoc_sync_now_handler(State(state): State<AppState>) -> Redirect {
    if state.config.data_dir.is_none() {
        return Flash::error("HedgeDoc sync is not available: no data directory is configured.")
            .redirect("/hedgedoc");
    }

    // Collect URLs to sync (release lock before awaiting).
    let entries: Vec<(String, ResolvedCollection)> = {
        let sources = state.hedgedoc_sources.lock();
        sources
            .iter()
            .flat_map(|s| {
                s.notes
                    .iter()
                    .map(move |n| (n.url.clone(), s.collection.clone()))
            })
            .collect()
    };

    let mut any_success = false;

    for (url, rc) in &entries {
        match sync_source(url, rc).await {
            Ok((deck_name, file_name)) => {
                any_success = true;
                let mut sources = state.hedgedoc_sources.lock();
                for src in sources.iter_mut() {
                    if let Some(note) = src.notes.iter_mut().find(|n| &n.url == url) {
                        note.deck_name = deck_name.clone();
                        note.file_name = file_name.clone();
                        note.last_error = None;
                        break;
                    }
                }
            }
            Err(e) => {
                let msg = e.to_string();
                log::error!("Manual HedgeDoc sync failed for {url}: {msg}");
                let mut sources = state.hedgedoc_sources.lock();
                for src in sources.iter_mut() {
                    if let Some(note) = src.notes.iter_mut().find(|n| &n.url == url) {
                        note.last_error = Some(msg.clone());
                        break;
                    }
                }
            }
        }
    }

    let sources_snapshot = state.hedgedoc_sources.lock().clone();
    let combined = build_combined_infos(&state.config.collections, &sources_snapshot);
    *state.collections.write().await = combined;
    if any_success {
        *state.hedgedoc_last_synced.lock() = Some(Timestamp::now());
    }

    if any_success || entries.is_empty() {
        Flash::success("HedgeDoc sync finished.").redirect("/hedgedoc")
    } else {
        Flash::error("HedgeDoc sync failed for all notes; see the statuses below.")
            .redirect("/hedgedoc")
    }
}
