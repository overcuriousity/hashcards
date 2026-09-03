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
use crate::cmd::drill::get::RenderContext;
use crate::cmd::drill::get::render_completion_page;
use crate::cmd::drill::get::render_session_page;
use crate::cmd::drill::post::Action;
use crate::cmd::drill::post::ActionResult;
use crate::cmd::drill::post::FormData;
use crate::cmd::drill::post::handle_action;
use crate::cmd::drill::render::render_macros_declaration;
use crate::cmd::drill::state::MutableState;
use crate::cmd::drill::state::SessionDb;
use crate::cmd::drill::state::SessionDbs;
use crate::cmd::drill::state::SessionSource;
use crate::cmd::drill::template::page_template;
use crate::cmd::drill::template::page_template_with_script;
use crate::cmd::run_blocking;
use crate::cmd::serve::auth::CurrentUser;
use crate::cmd::serve::browse::build_deck_tree;
use crate::cmd::serve::browse::render_browse_page;
use crate::cmd::serve::config::ResolvedCollection;
use crate::cmd::serve::decks::ResolvedCustomDeck;
use crate::cmd::serve::decks::find_custom_deck;
use crate::cmd::serve::git::clone_or_pull;
use crate::cmd::serve::hedgedoc::apply_sync_result;
use crate::cmd::serve::hedgedoc::build_combined_infos;
use crate::cmd::serve::hedgedoc::build_source;
use crate::cmd::serve::hedgedoc::cleanup_after_delete;
use crate::cmd::serve::hedgedoc::commit_add;
use crate::cmd::serve::hedgedoc::commit_delete;
use crate::cmd::serve::hedgedoc::find_slug_collision;
use crate::cmd::serve::hedgedoc::normalize_hedgedoc_url;
use crate::cmd::serve::hedgedoc::slug_for_note;
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
use crate::types::performance::Jitter;
use crate::types::timestamp::Timestamp;

/// Run blocking filesystem/SQLite work on tokio's blocking thread pool.
///
/// Serve handlers parse collections from disk and touch SQLite; doing that
/// on the async executor stalls every other request (BUG-44). Pattern as in
/// `git.rs` (`spawn_blocking` in `spawn_sync_task`).
pub async fn collection_get_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    current_user: Option<CurrentUser>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    let owner = current_user.map(|u| u.email);
    // A slug that isn't the caller's (wrong owner, or doesn't exist at all)
    // 404s before touching the session map, so an active session belonging
    // to a different owner is never reachable by slug alone.
    if find_drill_target(&state, &slug, owner.as_deref()).is_none() {
        return (StatusCode::NOT_FOUND, collection_not_found_page());
    }
    let state2 = state.clone();
    let slug2 = slug.clone();
    let owner2 = owner.clone();
    match run_blocking(move || collection_get_inner(&state2, &slug2, flash, owner2.as_deref()))
        .await
    {
        Ok(html) => (StatusCode::OK, Html(html)),
        Err(e) => {
            let html = page_template(html! {
                div.error {
                    h1 { "Error" }
                    p { (e) }
                    a href="/" { "Back to collections" }
                }
            })
            .into_string();
            (StatusCode::INTERNAL_SERVER_ERROR, Html(html))
        }
    }
}

fn collection_not_found_page() -> Html<String> {
    Html(
        page_template(html! {
            div.error {
                h1 { "Error" }
                p { "Unknown collection" }
                a href="/" { "Back to collections" }
            }
        })
        .into_string(),
    )
}

fn collection_get_inner(
    state: &AppState,
    slug: &str,
    flash: Option<Flash>,
    owner: Option<&str>,
) -> Fallible<String> {
    // Clone the Arc out of the map so the map lock is not held during
    // rendering; the session itself stays in the map even if rendering fails.
    let session: Option<SharedSession> = state.sessions.lock().get(slug).cloned();

    // A concurrent Home action (or eviction) may have removed this session
    // from the map, and closed its DB row, between the clone above and the
    // lock below; `is_detached` is how removers report that (see the
    // `detached` field's doc comment). Treat it exactly like "no session in
    // the map" rather than rendering a stale session or touching its row.
    let session = session.filter(|s| !s.lock().is_detached());

    let Some(session) = session else {
        // A custom deck has a fixed membership, so it gets a start page
        // rather than the collection's deck tree.
        if let Some(deck) = find_custom_deck(&state.custom_decks.lock(), slug, owner) {
            let sources = deck_sources(state, &deck, owner);
            let due = due_card_count(&sources)?;
            return Ok(render_custom_deck_page(&deck, &sources, due, flash).into_string());
        }
        // No active session: show the deck browser.
        let rc = find_collection(state, slug, owner)
            .ok_or_else(|| crate::error::ErrorReport::new(format!("Unknown collection: {slug}")))?;
        let browse = build_deck_tree(&rc.coll_dir, &rc.db_path)?;
        // Build a deck-name → HedgeDoc URL map so the browse page can show
        // edit links. A HedgeDoc collection is exactly one note, so this is
        // at most one entry. All URLs were validated as HTTPS when added.
        let hedge_urls: std::collections::HashMap<String, String> = {
            let sources = state.hedgedoc_sources.lock();
            sources
                .iter()
                .find(|s| s.collection.slug == slug)
                .map(|s| {
                    std::collections::HashMap::from([(
                        s.note.deck_name.clone(),
                        s.note.url.clone(),
                    )])
                })
                .unwrap_or_default()
        };
        let db_path = rc.db_path.to_str().ok_or_else(|| {
            crate::error::ErrorReport::new(format!(
                "Database path is not valid UTF-8: {}",
                rc.db_path.display()
            ))
        })?;
        let db = Database::new(db_path)?;
        // FEAT-03: report the session rows the startup sweep closed. The
        // sweep itself runs once, at startup: it cannot tell a crashed
        // session from a live one, and a second server may share the same
        // database, so doing it per request would stamp `ended_at` on a
        // session that is still going. Taking the entry also means the
        // notice appears once instead of on every visit.
        let interrupted_closed = state.interrupted_closed.lock().remove(slug).unwrap_or(0);
        let bookmark_count = db.count_bookmarks()?;
        let html = render_browse_page(
            &rc.name,
            slug,
            &browse,
            &hedge_urls,
            bookmark_count,
            interrupted_closed,
            flash,
        );
        return Ok(html.into_string());
    };

    let mut session = session.lock();
    // BUG-08: stamp activity so the eviction task only reaps idle sessions.
    session.last_activity_at = Timestamp::now();
    // BUG-14: start the per-card timer when the card is served.
    session.mutable.mark_card_shown();
    // Heartbeat, so another process's startup sweep can tell this session
    // apart from one abandoned by a crash.
    if let Err(e) = session.mutable.dbs.touch_all(Timestamp::now()) {
        log::debug!("could not stamp session heartbeat: {e}");
    }
    let form_action = format!("/collection/{slug}");
    let file_url_prefix = format!("/collection/{slug}/file");
    let ctx = RenderContext {
        directory: &session.directory,
        total_cards: session.total_cards,
        session_started_at: session.session_started_at,
        answer_controls: session.answer_controls,
        form_action: &form_action,
        file_url_prefix: &file_url_prefix,
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

/// Every slug that can be drilled: a configured collection (or HedgeDoc
/// note, which is one too) or a user-assembled cross-collection deck.
pub(super) enum DrillTarget {
    Collection(ResolvedCollection),
    Deck(ResolvedCustomDeck),
}

/// Resolve a drillable slug, scoped to `owner`. Collections win over decks;
/// startup validation refuses a config where the two could collide.
pub(super) fn find_drill_target(
    state: &AppState,
    slug: &str,
    owner: Option<&str>,
) -> Option<DrillTarget> {
    if let Some(rc) = find_collection(state, slug, owner) {
        return Some(DrillTarget::Collection(rc));
    }
    let decks = state.custom_decks.lock();
    find_custom_deck(&decks, slug, owner).map(DrillTarget::Deck)
}

/// The collections a custom deck draws on, each with the decks it wants.
///
/// Members naming a collection the caller no longer owns (renamed, removed,
/// or never theirs) are skipped rather than failing the whole deck: a stale
/// member should not make the rest of a deck undrillable.
pub(super) fn deck_sources(
    state: &AppState,
    deck: &ResolvedCustomDeck,
    owner: Option<&str>,
) -> Vec<SessionSourceSpec> {
    let mut by_collection: Vec<SessionSourceSpec> = Vec::new();
    for member in &deck.members {
        let Some(rc) = find_collection(state, &member.collection_slug, owner) else {
            continue;
        };
        match by_collection
            .iter_mut()
            .find(|s| s.collection.slug == rc.slug)
        {
            Some(existing) => existing.decks.push(member.deck_name.clone()),
            None => by_collection.push(SessionSourceSpec {
                collection: rc,
                decks: vec![member.deck_name.clone()],
            }),
        }
    }
    by_collection
}

/// Looks up a collection by slug, scoped to `owner` (the caller's email, or
/// `None` when `[oidc]` is off). A collection whose `owner` doesn't match is
/// treated exactly like a nonexistent one, so callers can 404 either case
/// identically rather than leaking which slugs exist for other users.
pub(super) fn find_collection(
    state: &AppState,
    slug: &str,
    owner: Option<&str>,
) -> Option<ResolvedCollection> {
    if let Some(rc) = state
        .config
        .collections
        .iter()
        .find(|c| c.slug == slug && c.owner.as_deref() == owner)
    {
        return Some(rc.clone());
    }
    let sources = state.hedgedoc_sources.lock();
    sources
        .iter()
        .find(|s| s.collection.slug == slug && s.collection.owner.as_deref() == owner)
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
    current_user: Option<CurrentUser>,
    Form(form): Form<StartDrillForm>,
) -> Redirect {
    let state2 = state.clone();
    let slug2 = slug.clone();
    let owner = current_user.map(|u| u.email);
    match run_blocking(move || {
        collection_start_inner(&state2, &slug2, form.decks, form.limit, owner.as_deref())
    })
    .await
    {
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
    owner: Option<&str>,
) -> Fallible<()> {
    // The slug must be the caller's own before anything else happens —
    // including before touching an existing session under that slug, which
    // could otherwise belong to a different owner.
    let target = find_drill_target(state, slug, owner)
        .ok_or_else(|| ErrorReport::new(format!("Unknown collection: {slug}")))?;
    // A custom deck's membership is fixed when it is saved, so it carries no
    // deck checkboxes; a collection needs at least one.
    if matches!(target, DrillTarget::Collection(_)) && selected_decks.is_empty() {
        return fail("Select at least one deck.");
    }
    // FEAT-03: never silently discard an unfinished session. The redirect
    // to /collection/{slug} lands on the running session; the user must
    // End it (or let BUG-08 eviction reap it) before starting a new one.
    {
        let mut sessions = state.sessions.lock();
        if let Some(existing) = sessions.get(slug) {
            if existing.lock().mutable.finished_at.is_none() {
                return Ok(());
            }
        }
        // Finished sessions are replaced.
        if let Some(previous) = sessions.remove(slug) {
            previous.lock().detach();
        }
    }

    // Create the session outside the lock (may do filesystem/DB work).
    let session = match target {
        DrillTarget::Collection(rc) => create_session_from_sources(
            state,
            vec![SessionSourceSpec {
                collection: rc,
                decks: selected_decks,
            }],
            limit,
        )?,
        DrillTarget::Deck(deck) => {
            let sources = deck_sources(state, &deck, owner);
            if sources.is_empty() {
                return fail(format!(
                    "The deck '{}' has no members in collections you own. Edit it from the \
                     Decks page.",
                    deck.name
                ));
            }
            create_session_from_sources(state, sources, limit)?
        }
    };
    if let Some(s) = session {
        state
            .sessions
            .lock()
            .insert(slug.to_string(), Arc::new(Mutex::new(s)));
    }
    Ok(())
}

/// One collection contributing to a drill session, and which of its decks
/// are wanted. An empty `decks` means every deck in the collection.
pub(super) struct SessionSourceSpec {
    pub collection: ResolvedCollection,
    pub decks: Vec<String>,
}

/// Build a drill session over one or more collections.
///
/// Each source keeps its own database and its own open session row, and every
/// card is routed back to the collection it came from, so a card drilled
/// inside a cross-collection deck is scheduled exactly once — in its home
/// collection — rather than acquiring a second, drifting schedule.
pub(super) fn create_session_from_sources(
    state: &AppState,
    sources: Vec<SessionSourceSpec>,
    limit: Option<usize>,
) -> Fallible<Option<DrillSession>> {
    if sources.is_empty() {
        return Ok(None);
    }
    let multi_source = sources.len() > 1;
    let session_started_at = Timestamp::now();
    let today: Date = session_started_at.date();

    let mut session_dbs: Vec<SessionDb> = Vec::new();
    let mut routes: HashMap<CardHash, usize> = HashMap::new();
    let mut due_cards: Vec<Card> = Vec::new();
    let mut cache = Cache::new();
    let mut first_directory: Option<PathBuf> = None;
    let mut macros: Vec<(String, String)> = Vec::new();

    for (index, spec) in sources.into_iter().enumerate() {
        let rc = spec.collection;
        let collection = Collection::with_db_path(rc.coll_dir.clone(), rc.db_path.clone())?;

        // Sync new cards to DB
        let db_hashes: HashSet<CardHash> = collection.db.card_hashes()?;
        for card in collection.cards.iter() {
            if !db_hashes.contains(&card.hash()) {
                collection.db.insert_card(card.hash(), session_started_at)?;
            }
        }

        // Filter by selected decks.
        let deck_filter: HashSet<&str> = spec.decks.iter().map(|s| s.as_str()).collect();
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
        for card in cards.into_iter().filter(|c| due_today.contains(&c.hash())) {
            // Cards are content addressed, so the same fact written into two
            // collections has one hash. Drill it once, routed to the first
            // collection that offered it; the other collection's own copy
            // stays due there and is reviewed when that collection is
            // drilled. Without this the session cache would reject the
            // second copy outright and the whole deck would fail to start.
            if routes.contains_key(&card.hash()) {
                continue;
            }
            let performance = collection.db.get_card_performance(card.hash())?;
            cache.insert(card.hash(), performance)?;
            routes.insert(card.hash(), index);
            due_cards.push(card);
        }

        // Open this collection's session row immediately, so reviews can be
        // written as they happen.
        let session_id = collection.db.create_session(session_started_at)?;
        session_dbs.push(SessionDb {
            db: collection.db,
            session_id,
            // A single-collection session renders media against the
            // session's own directory, exactly as before.
            source: multi_source.then(|| SessionSource {
                coll_dir: rc.coll_dir.clone(),
                file_url_prefix: format!("/collection/{}/file", rc.slug),
            }),
        });
        if first_directory.is_none() {
            first_directory = Some(collection.directory);
            macros = collection.macros;
        }
    }

    if state.config.defaults.bury_siblings {
        due_cards = bury_siblings(due_cards);
    }

    if due_cards.is_empty() {
        // The session rows opened above would otherwise linger as dangling
        // "interrupted" sessions with no reviews.
        for entry in &session_dbs {
            entry
                .db
                .close_session(entry.session_id, session_started_at)?;
        }
        return Ok(None);
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
    due_cards = shuffle(due_cards, &mut rng);

    // Apply session card limit if requested.
    if let Some(n) = limit {
        due_cards.truncate(n);
    }

    let answer_controls = state.config.defaults.answer_controls.into();
    let directory = first_directory
        .ok_or_else(|| ErrorReport::new("a drill session needs at least one collection"))?;
    let dbs = if multi_source {
        SessionDbs::routed(session_dbs, routes)
    } else {
        // One collection: every card belongs to it, so no routing table.
        SessionDbs::routed(session_dbs, HashMap::new())
    };

    Ok(Some(DrillSession::new(
        directory,
        macros,
        session_started_at,
        answer_controls,
        MutableState::new(
            dbs,
            cache,
            due_cards,
            Jitter::new(state.config.defaults.jitter)?,
            rng,
        ),
    )))
}

/// How many of a custom deck's cards are due today, across every collection
/// it draws on.
fn due_card_count(sources: &[SessionSourceSpec]) -> Fallible<usize> {
    let today = Timestamp::now().date();
    let mut total = 0;
    for spec in sources {
        let collection = Collection::with_db_path(
            spec.collection.coll_dir.clone(),
            spec.collection.db_path.clone(),
        )?;
        let due: HashSet<CardHash> = collection.db.due_today(today)?;
        let wanted: HashSet<&str> = spec.decks.iter().map(|d| d.as_str()).collect();
        total += collection
            .cards
            .iter()
            .filter(|c| wanted.contains(c.deck_name().as_str()) && due.contains(&c.hash()))
            .count();
    }
    Ok(total)
}

/// The start page for a custom deck: what it contains, how much is due, and
/// a button to drill it.
fn render_custom_deck_page(
    deck: &ResolvedCustomDeck,
    sources: &[SessionSourceSpec],
    due_today: usize,
    flash: Option<Flash>,
) -> maud::Markup {
    // Saturating: a member can only ever resolve to at most one entry, but
    // the page must not panic if that ever stops holding.
    let missing = deck
        .members
        .len()
        .saturating_sub(sources.iter().map(|s| s.decks.len()).sum::<usize>());
    page_template(html! {
        @if let Some(f) = &flash { (f.render()) }
        div.landing {
            h1 { (deck.name) }
            p { a href="/" { "\u{2190} Back to collections" } }
            p.empty {
                (format!("{due_today} card(s) due today across {} collection(s).", sources.len()))
            }
            @if missing > 0 {
                div.notice {
                    p {
                        (format!(
                            "{missing} member(s) of this deck point at a collection that is no \
                             longer available, and are skipped."
                        ))
                    }
                }
            }
            @if sources.is_empty() {
                p.empty { "This deck has no members in collections you own." }
            } @else {
                table.collection-table {
                    thead { tr { th { "Collection" } th { "Decks" } } }
                    tbody {
                        @for source in sources {
                            tr {
                                td { (source.collection.name) }
                                td { (source.decks.join(", ")) }
                            }
                        }
                    }
                }
                @if due_today > 0 {
                    form action=(format!("/collection/{}/start", deck.slug)) method="post" {
                        label for="limit" { "Card limit (optional): " }
                        input id="limit" type="number" name="limit" min="1" placeholder="all";
                        input .btn.btn-primary type="submit" value="Drill";
                    }
                } @else {
                    p.empty { "Nothing due in this deck today." }
                }
            }
        }
    })
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
    current_user: Option<CurrentUser>,
    Form(form): Form<FormData>,
) -> Redirect {
    let state2 = state.clone();
    let slug2 = slug.clone();
    let owner = current_user.map(|u| u.email);
    match run_blocking(move || collection_post_inner(&state2, &slug2, form, owner.as_deref())).await
    {
        Ok(redirect) => redirect,
        Err(e) => {
            log::error!("error handling action for collection {slug}: {e}");
            Flash::error(e.to_string()).redirect(&format!("/collection/{slug}"))
        }
    }
}

fn collection_post_inner(
    state: &AppState,
    slug: &str,
    form: FormData,
    owner: Option<&str>,
) -> Fallible<Redirect> {
    // A grading/Home action on a slug that isn't the caller's own must not
    // touch that slug's session, even if one happens to be active (sessions
    // are keyed by slug alone, not by owner).
    if find_drill_target(state, slug, owner).is_none() {
        return fail(format!("Unknown collection: {slug}"));
    }
    let action = form.action;
    let submitted_undo: Option<i64> = form.undo_review;
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
            let mut s = s.lock();
            s.detach();
            if s.mutable.finished_at.is_none() {
                if let Err(e) = s.mutable.dbs.close_all(Timestamp::now()) {
                    log::error!("failed to close the session for collection {slug}: {e}");
                }
            }
        }

        // Snapshot inputs, then compute + update in background (don't block response).
        let sources_snapshot = state.hedgedoc_sources.lock().clone();
        let static_collections = state.config.collections.clone();
        let collections_clone = state.collections.clone();
        let counts_refreshed_at = state.counts_refreshed_at.clone();
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || {
                build_combined_infos(&static_collections, &sources_snapshot)
            })
            .await
            {
                Ok(combined) => {
                    *collections_clone.write().await = combined;
                    *counts_refreshed_at.lock() = Some(Timestamp::now());
                }
                Err(e) => log::error!("Collection count refresh failed: {e}"),
            }
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
    // while we were waiting for the session lock, a concurrent start may have
    // replaced it with a new drill, or the idle sweep may have evicted it.
    //
    // This must not re-read the sessions map: taking the map lock here, while
    // the session lock is held, inverts the global order (map, then session)
    // that `evict_idle_sessions` and the landing page follow, and two
    // `parking_lot` mutexes with no timeout deadlock the server outright.
    // Every removal marks the session instead, under the map lock, so the
    // flag is all we need.
    if guard.is_detached() {
        return Ok(
            Flash::error("The session has ended, so that action was ignored.")
                .redirect(&format!("/collection/{slug}")),
        );
    }

    let session = &mut *guard;
    // BUG-08: stamp activity so the eviction task only reaps idle sessions.
    session.last_activity_at = Timestamp::now();

    // `Action::Home` returned early above, and it is the only action for which
    // `handle_action` yields `ActionResult::Home`. Every action reaching here
    // leaves the session running; the only result needing dispatch is
    // `Ignored`, which carries a one-shot message for the user.
    let result = handle_action(&mut session.mutable, action, submitted_card, submitted_undo)?;

    // BUG-45: a finished session changes due counts; refresh them in the
    // background so the landing page is up to date.
    if matches!(result, ActionResult::SessionFinished) {
        let sources_snapshot = state.hedgedoc_sources.lock().clone();
        let static_collections = state.config.collections.clone();
        let collections_clone = state.collections.clone();
        let counts_refreshed_at = state.counts_refreshed_at.clone();
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || {
                build_combined_infos(&static_collections, &sources_snapshot)
            })
            .await
            {
                Ok(combined) => {
                    *collections_clone.write().await = combined;
                    *counts_refreshed_at.lock() = Some(Timestamp::now());
                }
                Err(e) => log::error!("Collection count refresh failed: {e}"),
            }
        });
    }

    match result {
        ActionResult::Ignored(reason) => {
            Ok(Flash::error(reason).redirect(&format!("/collection/{slug}")))
        }
        _ => Ok(Redirect::to(&format!("/collection/{slug}"))),
    }
}

pub async fn collection_file_handler(
    State(state): State<AppState>,
    Path((slug, path)): Path<(String, String)>,
    current_user: Option<CurrentUser>,
) -> (StatusCode, [(HeaderName, &'static str); 1], Vec<u8>) {
    let owner = current_user.map(|u| u.email);
    let coll_dir = match find_collection(&state, &slug, owner.as_deref()) {
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
    current_user: Option<CurrentUser>,
) -> (StatusCode, [(HeaderName, &'static str); 1], String) {
    let owner = current_user.map(|u| u.email);
    let script = |macros: &[(String, String)]| {
        format!(
            "{}\n{}",
            render_macros_declaration(macros),
            include_str!("../drill/script.js")
        )
    };
    if find_drill_target(&state, &slug, owner.as_deref()).is_none() {
        return (
            StatusCode::OK,
            [(CONTENT_TYPE, "text/javascript")],
            script(&[]),
        );
    }
    let session: Option<SharedSession> = state.sessions.lock().get(&slug).cloned();
    let macros: Vec<(String, String)> = match session {
        Some(session) => session.lock().macros.clone(),
        // No active session; serve the script without macros.
        None => Vec::new(),
    };
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "text/javascript")],
        script(&macros),
    )
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
            let static_collections = state.config.collections.clone();
            match tokio::task::spawn_blocking(move || {
                build_combined_infos(&static_collections, &sources_snapshot)
            })
            .await
            {
                Ok(combined) => {
                    *state.collections.write().await = combined;
                    *state.counts_refreshed_at.lock() = Some(Timestamp::now());
                }
                Err(e) => log::error!("Manual sync failed to compute collection counts: {e}"),
            }
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
    current_user: Option<CurrentUser>,
) -> (StatusCode, Html<String>) {
    use crate::cmd::serve::hedgedoc_ui::render_manage_page;
    let flash = Flash::from_query(&query);
    let owner = current_user.map(|u| u.email);
    let all_sources = state.hedgedoc_sources.lock();
    let sources: Vec<crate::cmd::serve::state::HedgedocSource> = all_sources
        .iter()
        .filter(|s| s.collection.owner == owner)
        .cloned()
        .collect();
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
    current_user: Option<CurrentUser>,
    Form(form): Form<AddHedgedocForm>,
) -> Redirect {
    let owner = current_user.map(|u| u.email);
    // Normalize and validate the URL at storage time (BUG-24): strip
    // query/fragment/trailing slash so equivalent URLs dedupe, and reject
    // anything that is not a well-formed HTTPS URL so no raw string is
    // ever persisted or rendered into an href.
    if form.url.trim().is_empty() {
        return Flash::error("Enter a HedgeDoc URL.").redirect("/hedgedoc");
    }
    let url = match normalize_hedgedoc_url(&form.url) {
        Ok(url) => url,
        Err(e) => return Flash::error(e.to_string()).redirect("/hedgedoc"),
    };

    let data_dir = match &state.config.data_dir {
        Some(d) => d.clone(),
        None => {
            log::error!("Cannot add HedgeDoc source: no data_dir configured");
            return Flash::error(
                "Cannot add HedgeDoc source: no data directory is configured. Start hashcards-web with --config.",
            )
            .redirect("/hedgedoc");
        }
    };

    // Check for duplicate URL, scoped to the caller: two users may each add
    // the same public note, and they get separate collections.
    {
        let sources = state.hedgedoc_sources.lock();
        if sources
            .iter()
            .any(|s| s.note.url == url && s.collection.owner == owner)
        {
            return Flash::error("This note is already added.").redirect("/hedgedoc");
        }
    }

    if source_uri_from_url(&url).is_none() {
        log::error!("Failed to parse HedgeDoc source URI from {url}");
        return Flash::error(format!("Could not parse a HedgeDoc note URL from: {url}"))
            .redirect("/hedgedoc");
    }

    // BUG-43: refuse to create a source whose slug collides with a configured
    // collection; find_collection would route it to the wrong database.
    let new_slug = slug_for_note(&url, owner.as_deref());
    if let Some(existing) = find_slug_collision(&new_slug, &state.config.collections) {
        return Flash::error(format!(
            "Cannot add this HedgeDoc source: its collection slug '{new_slug}' collides with the configured collection '{}'. Rename that collection or use a different source.",
            existing.name
        ))
        .redirect("/hedgedoc");
    }

    let new_source = match build_source(&url, &data_dir, owner.clone()).await {
        Ok(source) => source,
        Err(e) => {
            log::error!("Failed to add HedgeDoc source {url}: {e}");
            return Flash::error(format!("Failed to add HedgeDoc source: {e}"))
                .redirect("/hedgedoc");
        }
    };

    // A config file is mandatory, so the server cannot reach this point
    // without one. Adding a note has nowhere to be persisted otherwise.
    let config_path = match state.config_path.lock().clone() {
        Some(p) => p,
        None => {
            log::error!("Cannot persist a HedgeDoc source: the server has no config file path");
            return Flash::error(
                "Cannot save this HedgeDoc source: the server was started without a \
                 configuration file to write it back to.",
            )
            .redirect("/hedgedoc");
        }
    };

    // BUG-39: duplicate check + mutation + persist under ONE lock, persisting
    // from the post-mutation state. spawn_blocking because commit_add writes
    // the config file while holding the lock.
    let sources_arc = state.hedgedoc_sources.clone();
    let config_path_owned = config_path.clone();
    let url_owned = url.clone();
    let owner_owned = owner.clone();
    let snapshot = match tokio::task::spawn_blocking(move || {
        commit_add(
            &sources_arc,
            Some(config_path_owned.as_path()),
            &url_owned,
            owner_owned.as_deref(),
            new_source,
        )
    })
    .await
    .map_err(|e| ErrorReport::new(format!("HedgeDoc add task panicked: {e}")))
    .and_then(|r| r)
    {
        Ok(snapshot) => snapshot,
        Err(e) => {
            log::error!("Failed to add HedgeDoc source {url}: {e}");
            return Flash::error(e.to_string()).redirect("/hedgedoc");
        }
    };

    // Refresh combined collection infos from the committed snapshot.
    let static_collections = state.config.collections.clone();
    let snapshot_for_counts = snapshot.clone();
    match tokio::task::spawn_blocking(move || {
        build_combined_infos(&static_collections, &snapshot_for_counts)
    })
    .await
    {
        Ok(combined) => *state.collections.write().await = combined,
        Err(e) => log::error!("Failed to compute collection counts: {e}"),
    }

    // Update last synced time if the newly added note fetched without error.
    if snapshot
        .iter()
        .any(|s| s.note.url == url && s.collection.owner == owner && s.note.last_error.is_none())
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
    current_user: Option<CurrentUser>,
    Form(form): Form<DeleteHedgedocForm>,
) -> Redirect {
    let owner = current_user.map(|u| u.email);
    let owns_url = state
        .hedgedoc_sources
        .lock()
        .iter()
        .any(|s| s.note.url == form.url && s.collection.owner == owner);
    if !owns_url {
        return Flash::error("No HedgeDoc source with this URL: ".to_string() + &form.url)
            .redirect("/hedgedoc");
    }
    let maybe_config_path: Option<PathBuf> = state.config_path.lock().clone();
    let sources_arc = state.hedgedoc_sources.clone();
    let url = form.url.clone();
    let owner_owned = owner.clone();
    // BUG-39: mutation + persist under one lock (see commit_delete).
    // BUG-42: on-disk cleanup runs in the same blocking task.
    let (snapshot, message) = match tokio::task::spawn_blocking(move || {
        let outcome = commit_delete(
            &sources_arc,
            maybe_config_path.as_deref(),
            &url,
            owner_owned.as_deref(),
        )?;
        let message = cleanup_after_delete(&outcome);
        Ok::<_, ErrorReport>((outcome.snapshot, message))
    })
    .await
    .map_err(|e| ErrorReport::new(format!("HedgeDoc delete task panicked: {e}")))
    .and_then(|r| r)
    {
        Ok(pair) => pair,
        Err(e) => {
            log::error!("Failed to delete HedgeDoc source {}: {e}", form.url);
            return Flash::error(e.to_string()).redirect("/hedgedoc");
        }
    };

    let static_collections = state.config.collections.clone();
    let snapshot_for_counts = snapshot.clone();
    match tokio::task::spawn_blocking(move || {
        build_combined_infos(&static_collections, &snapshot_for_counts)
    })
    .await
    {
        Ok(combined) => *state.collections.write().await = combined,
        Err(e) => log::error!("Failed to compute collection counts: {e}"),
    }

    Flash::success(message).redirect("/hedgedoc")
}

/// Manually re-sync all HedgeDoc sources.
pub async fn hedgedoc_sync_now_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
) -> Redirect {
    let owner = current_user.map(|u| u.email);
    if state.config.data_dir.is_none() {
        return Flash::error("HedgeDoc sync is not available: no data directory is configured.")
            .redirect("/hedgedoc");
    }

    // Collect URLs to sync (release lock before awaiting). Only the caller's
    // own notes: a manual sync must not do IO on other users' notes, nor
    // name their note URLs in this user's error banner.
    let entries: Vec<(String, ResolvedCollection)> = {
        let sources = state.hedgedoc_sources.lock();
        sources
            .iter()
            .filter(|s| s.collection.owner == owner)
            .map(|s| (s.note.url.clone(), s.collection.clone()))
            .collect()
    };

    let mut any_success = false;

    for (url, rc) in &entries {
        match sync_source(url, rc).await {
            Ok((deck_name, file_name)) => {
                any_success = true;
                let mut sources = state.hedgedoc_sources.lock();
                if let Some(src) = sources.iter_mut().find(|s| s.collection.slug == rc.slug) {
                    apply_sync_result(src, deck_name, file_name);
                }
            }
            Err(e) => {
                let msg = e.to_string();
                log::error!("Manual HedgeDoc sync failed for {url}: {msg}");
                let mut sources = state.hedgedoc_sources.lock();
                if let Some(src) = sources.iter_mut().find(|s| s.collection.slug == rc.slug) {
                    src.note.last_error = Some(msg);
                }
            }
        }
    }

    let sources_snapshot = state.hedgedoc_sources.lock().clone();
    let static_collections = state.config.collections.clone();
    match tokio::task::spawn_blocking(move || {
        build_combined_infos(&static_collections, &sources_snapshot)
    })
    .await
    {
        Ok(combined) => *state.collections.write().await = combined,
        Err(e) => log::error!("Failed to compute collection counts: {e}"),
    }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::Instant;

    use std::collections::HashMap;

    use super::SessionSourceSpec;
    use super::collection_get_inner;
    use super::create_session_from_sources;
    use super::deck_sources;
    use super::run_blocking;
    use crate::cmd::drill::render::AnswerControls;
    use crate::cmd::drill::state::MutableState;
    use crate::cmd::drill::state::SessionDbs;
    use crate::cmd::serve::config::ResolvedCollection;
    use crate::cmd::serve::config::ResolvedServeConfig;
    use crate::cmd::serve::state::AppState;
    use crate::cmd::serve::state::DrillSession;
    use crate::db::Database;
    use crate::error::ErrorReport;
    use crate::error::Fallible;
    use crate::rng::TinyRng;
    use crate::types::performance::Jitter;
    use crate::types::timestamp::Timestamp;

    /// Cross-user isolation: `find_collection` only matches a collection
    /// whose `owner` equals the caller's, and unauthenticated (`None`)
    /// callers only match unowned collections.
    #[test]
    fn test_find_collection_is_scoped_to_owner() -> Fallible<()> {
        use crate::cmd::serve::config::DefaultsSection;
        use crate::cmd::serve::config::ResolvedCollection;
        use crate::cmd::serve::handlers::find_collection;

        let alice_dir = tempfile::tempdir()?;
        let bob_dir = tempfile::tempdir()?;
        let collections = vec![
            ResolvedCollection {
                name: "Alice's Deck".to_string(),
                slug: "alice-deck".to_string(),
                coll_dir: alice_dir.path().to_path_buf(),
                db_path: alice_dir.path().join("hashcards.db"),
                owner: Some("alice@example.com".to_string()),
            },
            ResolvedCollection {
                name: "Bob's Deck".to_string(),
                slug: "bob-deck".to_string(),
                coll_dir: bob_dir.path().to_path_buf(),
                db_path: bob_dir.path().join("hashcards.db"),
                owner: Some("bob@example.com".to_string()),
            },
        ];
        let config = ResolvedServeConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
            git: None,
            defaults: DefaultsSection::default(),
            collections,
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            local_collections: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
        };
        let state = AppState {
            config: std::sync::Arc::new(config),
            collections: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            sessions: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
            last_synced: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            hedgedoc_sources: std::sync::Arc::new(parking_lot::Mutex::new(Vec::new())),
            custom_decks: std::sync::Arc::new(parking_lot::Mutex::new(Vec::new())),
            hedgedoc_last_synced: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            config_path: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            counts_refreshed_at: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            interrupted_closed: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
            session_key: axum_extra::extract::cookie::Key::generate(),
            oidc: None,
        };

        assert!(find_collection(&state, "bob-deck", Some("alice@example.com")).is_none());
        assert!(find_collection(&state, "alice-deck", Some("alice@example.com")).is_some());
        assert!(find_collection(&state, "alice-deck", None).is_none());
        Ok(())
    }

    fn test_state(coll_dir: &std::path::Path) -> Fallible<AppState> {
        let config = ResolvedServeConfig::from_directories(
            vec![coll_dir.display().to_string()],
            "127.0.0.1".to_string(),
            0,
        )?;
        test_state_with_config(config)
    }

    fn test_state_with_config(config: ResolvedServeConfig) -> Fallible<AppState> {
        Ok(AppState {
            config: std::sync::Arc::new(config),
            collections: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            sessions: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
            last_synced: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            hedgedoc_sources: std::sync::Arc::new(parking_lot::Mutex::new(Vec::new())),
            custom_decks: std::sync::Arc::new(parking_lot::Mutex::new(Vec::new())),
            hedgedoc_last_synced: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            config_path: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            counts_refreshed_at: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            interrupted_closed: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
            session_key: axum_extra::extract::cookie::Key::generate(),
            oidc: None,
        })
    }

    /// Regression: a session removed from the map between the GET handler's
    /// clone and its lock (a concurrent Home action, or eviction) must not be
    /// rendered as if it were still live. `collection_post_inner` already
    /// guarded this with an `is_detached` check; `collection_get_inner` did
    /// not, so it would render the stale session and re-stamp its (already
    /// closed) DB row's heartbeat.
    #[test]
    fn test_detached_session_renders_browse_page_not_stale_session() -> Fallible<()> {
        let dir = tempfile::tempdir()?;
        let coll_dir = dir.path().canonicalize()?;
        std::fs::write(coll_dir.join("Deck.md"), "Q: One\nA: 1\n")?;

        let state = test_state(&coll_dir)?;
        let slug = state.config.collections[0].slug.clone();
        let db_path = state.config.collections[0].db_path.clone();
        let db_str = db_path
            .to_str()
            .ok_or_else(|| ErrorReport::new("non-utf8 db path"))?;

        let started_at = Timestamp::now();
        let db = Database::new(db_str)?;
        let session_id = db.create_session(started_at)?;
        let mutable = MutableState::new(
            SessionDbs::single(db, session_id),
            crate::cmd::drill::cache::Cache::new(),
            Vec::new(),
            Jitter::none(),
            TinyRng::from_seed(1),
        );
        let session = std::sync::Arc::new(parking_lot::Mutex::new(DrillSession::new(
            coll_dir.clone(),
            Vec::new(),
            started_at,
            AnswerControls::Full,
            mutable,
        )));
        session.lock().detach();
        state.sessions.lock().insert(slug.clone(), session);

        let html = collection_get_inner(&state, &slug, None, None)?;
        assert!(
            html.contains(r#"class="browse""#),
            "detached session must fall back to the deck browser page, got: {html}"
        );
        Ok(())
    }

    /// Regression test (BUG-44): serve handlers used to run collection
    /// parsing and SQLite work inline on the async executor, so one slow
    /// request stalled every other request. `run_blocking` must move that
    /// work off the executor: on a single-threaded runtime, a concurrent
    /// timer still fires while the blocking closure sleeps.
    #[tokio::test(flavor = "current_thread")]
    async fn run_blocking_does_not_stall_the_executor() -> Fallible<()> {
        let started = Instant::now();
        let slow = tokio::spawn(run_blocking(|| {
            std::thread::sleep(Duration::from_millis(800));
            Ok(())
        }));

        // This timer shares the single executor thread with the task above.
        // It can only fire promptly if the sleep is not on that thread.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the executor was blocked for {:?}",
            started.elapsed()
        );

        slow.await
            .map_err(|e| crate::error::ErrorReport::new(format!("join failed: {e}")))?
    }

    /// A custom deck drills cards from several collections in one session,
    /// and each review lands in the database of the collection the card
    /// actually came from. A card scheduled in two places would come due
    /// twice on schedules that drift apart, which is the whole point of
    /// routing rather than giving the deck a database of its own.
    #[test]
    fn test_custom_deck_routes_reviews_to_each_home_collection() -> Fallible<()> {
        use crate::cmd::drill::post::Action;
        use crate::cmd::serve::decks::ResolvedCustomDeck;
        use crate::cmd::serve::decks::slug_for_deck;

        let alpha_dir = tempfile::tempdir()?;
        std::fs::write(
            alpha_dir.path().join("Alpha.md"),
            "Q: alpha question?\nA: alpha answer\n",
        )?;
        let beta_dir = tempfile::tempdir()?;
        std::fs::write(
            beta_dir.path().join("Beta.md"),
            "Q: beta question?\nA: beta answer\n",
        )?;

        let alpha = ResolvedCollection {
            name: "Alpha".to_string(),
            slug: "alpha".to_string(),
            coll_dir: alpha_dir.path().to_path_buf(),
            db_path: alpha_dir.path().join("alpha.db"),
            owner: None,
        };
        let beta = ResolvedCollection {
            name: "Beta".to_string(),
            slug: "beta".to_string(),
            coll_dir: beta_dir.path().to_path_buf(),
            db_path: beta_dir.path().join("beta.db"),
            owner: None,
        };

        let mut config =
            ResolvedServeConfig::from_directories(Vec::new(), "127.0.0.1".to_string(), 0)?;
        config.collections = vec![alpha.clone(), beta.clone()];
        let state = test_state_with_config(config)?;

        let deck = ResolvedCustomDeck {
            name: "Mixed".to_string(),
            slug: slug_for_deck("Mixed", None),
            owner: None,
            members: vec![
                crate::cmd::serve::config::DeckMember::parse("alpha/Alpha")
                    .ok_or_else(|| ErrorReport::new("member"))?,
                crate::cmd::serve::config::DeckMember::parse("beta/Beta")
                    .ok_or_else(|| ErrorReport::new("member"))?,
            ],
        };
        state.custom_decks.lock().push(deck.clone());

        // The deck resolves to both collections.
        let sources = deck_sources(&state, &deck, None);
        assert_eq!(sources.len(), 2, "both collections must contribute");

        let session = create_session_from_sources(&state, sources, None)?
            .ok_or_else(|| ErrorReport::new("expected due cards from both collections"))?;
        let mut session = session;
        assert_eq!(
            session.mutable.cards.len(),
            2,
            "one new card from each collection"
        );

        // Grade every card in the session.
        while !session.mutable.cards.is_empty() {
            let hash = session.mutable.cards[0].hash();
            session.mutable.reveal = true;
            crate::cmd::drill::post::handle_action(
                &mut session.mutable,
                Action::Good,
                Some(hash),
                None,
            )?;
        }

        // Each collection's own database recorded exactly its own review.
        let alpha_db = crate::db::Database::new(
            alpha
                .db_path
                .to_str()
                .ok_or_else(|| ErrorReport::new("non-utf8 path"))?,
        )?;
        let beta_db = crate::db::Database::new(
            beta.db_path
                .to_str()
                .ok_or_else(|| ErrorReport::new("non-utf8 path"))?,
        )?;
        let alpha_reviews: usize = alpha_db
            .get_all_sessions()?
            .iter()
            .map(|s| {
                alpha_db
                    .get_reviews_for_session(s.session_id)
                    .map(|r| r.len())
            })
            .collect::<Fallible<Vec<usize>>>()?
            .iter()
            .sum();
        let beta_reviews: usize = beta_db
            .get_all_sessions()?
            .iter()
            .map(|s| {
                beta_db
                    .get_reviews_for_session(s.session_id)
                    .map(|r| r.len())
            })
            .collect::<Fallible<Vec<usize>>>()?
            .iter()
            .sum();
        assert_eq!(alpha_reviews, 1, "Alpha's card must be scheduled in Alpha");
        assert_eq!(beta_reviews, 1, "Beta's card must be scheduled in Beta");
        Ok(())
    }

    /// Cards are content addressed, so the same fact in two collections has
    /// one hash. A deck spanning both must still start: the session cache
    /// rejects a duplicate hash outright, so the second copy is skipped
    /// rather than failing the whole deck.
    #[test]
    fn test_custom_deck_tolerates_the_same_card_in_two_collections() -> Fallible<()> {
        let one_dir = tempfile::tempdir()?;
        let two_dir = tempfile::tempdir()?;
        // Byte-identical card text in both collections.
        let card = "Q: shared question?\nA: shared answer\n";
        std::fs::write(one_dir.path().join("Shared.md"), card)?;
        std::fs::write(two_dir.path().join("Shared.md"), card)?;

        let one = ResolvedCollection {
            name: "One".to_string(),
            slug: "one".to_string(),
            coll_dir: one_dir.path().to_path_buf(),
            db_path: one_dir.path().join("one.db"),
            owner: None,
        };
        let two = ResolvedCollection {
            name: "Two".to_string(),
            slug: "two".to_string(),
            coll_dir: two_dir.path().to_path_buf(),
            db_path: two_dir.path().join("two.db"),
            owner: None,
        };
        let mut config =
            ResolvedServeConfig::from_directories(Vec::new(), "127.0.0.1".to_string(), 0)?;
        config.collections = vec![one.clone(), two.clone()];
        let state = test_state_with_config(config)?;

        let sources = vec![
            SessionSourceSpec {
                collection: one,
                decks: vec!["Shared".to_string()],
            },
            SessionSourceSpec {
                collection: two,
                decks: vec!["Shared".to_string()],
            },
        ];
        let session = create_session_from_sources(&state, sources, None)?
            .ok_or_else(|| ErrorReport::new("expected the shared card to be due"))?;
        assert_eq!(
            session.mutable.cards.len(),
            1,
            "the shared card must appear once, not twice"
        );
        Ok(())
    }
}
