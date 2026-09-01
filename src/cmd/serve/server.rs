use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use axum::Router;
use axum::routing::get;
use axum::routing::post;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

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
use crate::cmd::drill::template::icon_192_handler;
use crate::cmd::drill::template::icon_512_handler;
use crate::cmd::drill::template::manifest_handler;
use crate::cmd::serve::bookmarks::bookmark_delete_handler;
use crate::cmd::serve::bookmarks::bookmark_list_handler;
use crate::cmd::serve::bookmarks::bookmark_note_handler;
use crate::cmd::serve::config::ResolvedCollection;
use crate::cmd::serve::hedgedoc::check_startup_slug_collisions;
use crate::db::Database;
use crate::cmd::serve::config::ResolvedGit;
use crate::cmd::serve::config::ResolvedServeConfig;
use crate::cmd::serve::edit::edit_get_handler;
use crate::cmd::serve::edit::edit_post_handler;
use crate::cmd::serve::git::clone_or_pull;
use crate::cmd::serve::git::spawn_sync_task;
use crate::cmd::serve::handlers::collection_file_handler;
use crate::cmd::serve::handlers::collection_get_handler;
use crate::cmd::serve::handlers::collection_post_handler;
use crate::cmd::serve::handlers::collection_script_handler;
use crate::cmd::serve::handlers::collection_start_handler;
use crate::cmd::serve::handlers::hedgedoc_add_handler;
use crate::cmd::serve::handlers::hedgedoc_delete_handler;
use crate::cmd::serve::handlers::hedgedoc_manage_handler;
use crate::cmd::serve::handlers::hedgedoc_sync_now_handler;
use crate::cmd::serve::handlers::sync_handler;
use crate::cmd::serve::hedgedoc::build_combined_infos;
use crate::cmd::serve::hedgedoc::build_note;
use crate::cmd::serve::hedgedoc::build_source_lossless;
use crate::cmd::serve::hedgedoc::error_note;
use crate::cmd::serve::hedgedoc::source_uri_from_url;
use crate::cmd::serve::hedgedoc::spawn_hedgedoc_sync_task;
use crate::cmd::serve::landing::landing_handler;
use crate::cmd::serve::state::AppState;
use crate::cmd::serve::state::HedgedocSource;
use crate::cmd::serve::state::SharedSession;
use crate::cmd::serve::state::evict_idle_sessions;
use crate::cmd::serve::stats::collection_stats_handler;
use crate::cmd::signals::terminate_signal;
use crate::error::Fallible;
use crate::types::timestamp::Timestamp;

/// Close session rows left dangling by a crash or restart, across every
/// collection this server serves, and return the per-slug counts so the deck
/// browser can report them once.
///
/// A collection whose database cannot be opened is skipped with a log line
/// rather than failing startup: an unreadable database is the deck browser's
/// problem to report, not a reason to refuse to serve everything else.
fn sweep_dangling_sessions(
    config: &ResolvedServeConfig,
    hedgedoc_sources: &[HedgedocSource],
) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    let collections = config
        .collections
        .iter()
        .chain(hedgedoc_sources.iter().map(|s| &s.collection));
    for rc in collections {
        let Some(db_path) = rc.db_path.to_str() else {
            log::error!("Database path is not valid UTF-8: {}", rc.db_path.display());
            continue;
        };
        let closed = Database::new(db_path).and_then(|db| db.close_dangling_sessions());
        match closed {
            Ok(0) => {}
            Ok(n) => {
                log::info!("Closed {n} interrupted session(s) for collection '{}'", rc.slug);
                counts.insert(rc.slug.clone(), n);
            }
            Err(e) => log::error!(
                "Could not close interrupted sessions for collection '{}': {e}",
                rc.slug
            ),
        }
    }
    counts
}

pub async fn start_serve(config: ResolvedServeConfig) -> Fallible<()> {
    // Git mode: clone/pull repo and create data directories
    let sync_git = match &config.git {
        Some(git) => {
            std::fs::create_dir_all(&git.repo_dir)?;
            std::fs::create_dir_all(&git.db_dir)?;

            log::debug!("Initial git sync...");
            clone_or_pull(&git.repo_url, &git.branch, &git.repo_dir).await?;

            Some(ResolvedGit {
                repo_url: git.repo_url.clone(),
                branch: git.branch.clone(),
                poll_interval_minutes: git.poll_interval_minutes,
                commit_author_name: git.commit_author_name.clone(),
                commit_author_email: git.commit_author_email.clone(),
                repo_dir: git.repo_dir.clone(),
                db_dir: git.db_dir.clone(),
            })
        }
        None => None,
    };

    // Ensure DB parent directories exist for all collections (in git mode these
    // are already created above; in non-git TOML mode they may not exist yet).
    for rc in &config.collections {
        if let Some(parent) = rc.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // Build initial HedgeDoc sources (fetch markdown, write to disk).
    let data_dir: Option<PathBuf> = config.data_dir.clone();

    let hedgedoc_sources_init = if let Some(ref dd) = data_dir {
        let mut sources: Vec<HedgedocSource> = Vec::new();
        for entry in &config.hedgedoc_entries {
            let existing_idx = source_uri_from_url(&entry.url)
                .and_then(|source_uri| sources.iter().position(|s| s.source_uri == source_uri));
            match existing_idx {
                Some(idx) => {
                    // Additional note for an already-built source. build_note
                    // reports sync failures via `last_error` rather than
                    // erroring, and even a hard error keeps the entry (BUG-40).
                    let collection = sources[idx].collection.clone();
                    match build_note(&entry.url, &collection).await {
                        Ok(note) => sources[idx].notes.push(note),
                        Err(e) => {
                            log::error!("Failed to initialize HedgeDoc note {}: {e}", entry.url);
                            sources[idx]
                                .notes
                                .push(error_note(&entry.url, e.to_string()));
                        }
                    }
                }
                // First note for this source (or unparseable URL):
                // build_source_lossless never drops the entry.
                None => sources.push(build_source_lossless(&entry.url, dd).await),
            }
        }
        sources
    } else {
        Vec::new()
    };

    // Build combined collection info (static + hedgedoc)
    let collection_infos = build_combined_infos(&config.collections, &hedgedoc_sources_init);
    log::debug!("Loaded {} collections", collection_infos.len());

    let last_synced = if config.git.is_some() {
        Some(Timestamp::now())
    } else {
        None
    };

    let sync_collections: Vec<ResolvedCollection> = config
        .collections
        .iter()
        .map(|c| ResolvedCollection {
            name: c.name.clone(),
            slug: c.slug.clone(),
            coll_dir: c.coll_dir.clone(),
            db_path: c.db_path.clone(),
        })
        .collect();

    // Determine poll interval for HedgeDoc (inherit from git, or default 30 min).
    let hedgedoc_poll_minutes = config
        .git
        .as_ref()
        .map(|g| g.poll_interval_minutes)
        .unwrap_or(30);

    let config_path: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(config.config_path.clone()));
    let bind = format!("{}:{}", config.host, config.port);

    let config = Arc::new(config);
    // Mark initial sync time only if at least one note was fetched without error.
    let hedgedoc_last_synced_init = if hedgedoc_sources_init
        .iter()
        .flat_map(|s| s.notes.iter())
        .any(|n| n.last_error.is_none())
    {
        Some(Timestamp::now())
    } else {
        None
    };
    // BUG-43: refuse to start if two collections share a URL slug. Routing
    // prefers configured collections, so a collision silently addresses the
    // wrong database rather than failing visibly.
    check_startup_slug_collisions(&config.collections, &hedgedoc_sources_init)?;

    // FEAT-03: close session rows left open by a crash or restart, once, at
    // startup. They cannot be resumed (the card queue lives only in memory),
    // so they are closed with all persisted reviews kept. This must not run
    // per request: the predicate cannot distinguish a crashed session from a
    // live one, and a CLI `drill` may be running against the same database.
    let interrupted_closed = sweep_dangling_sessions(&config, &hedgedoc_sources_init);

    let hedgedoc_sources = Arc::new(Mutex::new(hedgedoc_sources_init));
    let hedgedoc_last_synced = Arc::new(Mutex::new(hedgedoc_last_synced_init));

    let state = AppState {
        config: config.clone(),
        collections: Arc::new(RwLock::new(collection_infos)),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        last_synced: Arc::new(Mutex::new(last_synced)),
        hedgedoc_sources: hedgedoc_sources.clone(),
        hedgedoc_last_synced: hedgedoc_last_synced.clone(),
        config_path,
        // Counts were just computed above, so the stamp starts fresh.
        counts_refreshed_at: Arc::new(Mutex::new(Some(Timestamp::now()))),
        interrupted_closed: Arc::new(Mutex::new(interrupted_closed)),
    };

    spawn_session_eviction_task(state.sessions.clone(), config.session_timeout_minutes);

    // Spawn background git sync task (only in git mode)
    if let Some(git) = sync_git {
        spawn_sync_task(
            git,
            sync_collections.clone(),
            state.collections.clone(),
            state.last_synced.clone(),
            state.hedgedoc_sources.clone(),
        );
    }

    // Spawn background HedgeDoc sync task (only when data_dir is available)
    if data_dir.is_some() {
        spawn_hedgedoc_sync_task(
            hedgedoc_sources,
            state.collections.clone(),
            hedgedoc_last_synced,
            sync_collections,
            hedgedoc_poll_minutes,
        );
    }

    let app = Router::new()
        .route("/", get(landing_handler))
        .route("/sync", post(sync_handler))
        .route("/hedgedoc", get(hedgedoc_manage_handler))
        .route("/hedgedoc/add", post(hedgedoc_add_handler))
        .route("/hedgedoc/delete", post(hedgedoc_delete_handler))
        .route("/hedgedoc/sync", post(hedgedoc_sync_now_handler))
        .route("/collection/{slug}", get(collection_get_handler))
        .route("/collection/{slug}", post(collection_post_handler))
        .route("/collection/{slug}/start", post(collection_start_handler))
        .route("/collection/{slug}/stats", get(collection_stats_handler))
        .route("/collection/{slug}/bookmarks", get(bookmark_list_handler))
        .route(
            "/collection/{slug}/bookmarks/{hash}/delete",
            post(bookmark_delete_handler),
        )
        .route(
            "/collection/{slug}/bookmarks/{hash}/note",
            post(bookmark_note_handler),
        )
        .route("/collection/{slug}/edit/{hash}", get(edit_get_handler))
        .route("/collection/{slug}/edit/{hash}", post(edit_post_handler))
        .route(
            "/collection/{slug}/file/{*path}",
            get(collection_file_handler),
        )
        .route(
            "/collection/{slug}/script.js",
            get(collection_script_handler),
        )
        .route("/manifest.json", get(manifest_handler))
        .route("/icons/icon-192.png", get(icon_192_handler))
        .route("/icons/icon-512.png", get(icon_512_handler))
        .route("/script.js", get(script_handler))
        .route("/style.css", get(style_handler))
        .route(KATEX_CSS_URL, get(katex_css_handler))
        .route(KATEX_JS_URL, get(katex_js_handler))
        .route(KATEX_MHCHEM_JS_URL, get(katex_mhchem_js_handler))
        .route("/katex/fonts/{*path}", get(katex_font_handler))
        .route(HLJS_CSS_URL, get(hljs_css_handler))
        .route(HLJS_JS_URL, get(hljs_js_handler))
        .with_state(state);

    log::debug!("Starting server on {bind}");
    let listener = TcpListener::bind(&bind).await?;
    println!("hashcards server running on http://{bind}/");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    log::debug!("Server shut down");
    Ok(())
}

async fn script_handler() -> (
    axum::http::StatusCode,
    [(axum::http::HeaderName, &'static str); 1],
    &'static str,
) {
    (
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/javascript")],
        // Landing/browse pages use this route and expect MACROS to be defined.
        concat!("let MACROS = {};\n\n", include_str!("../drill/script.js")),
    )
}

async fn style_handler() -> (
    axum::http::StatusCode,
    [(axum::http::HeaderName, &'static str); 2],
    &'static [u8],
) {
    (
        axum::http::StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "text/css"),
            (
                axum::http::header::CACHE_CONTROL,
                crate::utils::CACHE_CONTROL_IMMUTABLE,
            ),
        ],
        include_bytes!("../drill/style.css"),
    )
}

async fn shutdown_signal() {
    terminate_signal().await;
    log::debug!("Received shutdown signal");
}

/// Periodically evict drill sessions idle past the configured timeout,
/// closing their DB session rows (BUG-08).
fn spawn_session_eviction_task(
    sessions: Arc<Mutex<HashMap<String, SharedSession>>>,
    timeout_minutes: u64,
) {
    if timeout_minutes == 0 {
        log::debug!("Idle session eviction disabled (session_timeout_minutes = 0)");
        return;
    }
    tokio::spawn(async move {
        // Check at a quarter of the timeout, capped at every 10 minutes.
        let tick_secs = (timeout_minutes * 60 / 4).clamp(1, 600);
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(tick_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let sessions = sessions.clone();
            match tokio::task::spawn_blocking(move || {
                evict_idle_sessions(&sessions, timeout_minutes, Timestamp::now())
            })
            .await
            {
                Ok(evicted) if !evicted.is_empty() => {
                    log::info!(
                        "Evicted {} idle drill session(s): {}",
                        evicted.len(),
                        evicted.join(", ")
                    );
                }
                Ok(_) => {}
                Err(e) => log::error!("Session eviction task failed: {e}"),
            }
        }
    });
}
