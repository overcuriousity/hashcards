use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use axum::Router;
use axum::routing::get;
use axum::routing::post;
use axum_extra::extract::cookie::Key;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use crate::cmd::drill::fonts::font_handler;
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
use crate::cmd::serve::auth::build_oidc_runtime;
use crate::cmd::serve::auth::callback_handler;
use crate::cmd::serve::auth::login_handler;
use crate::cmd::serve::auth::logout_handler;
use crate::cmd::serve::auth::require_auth;
use crate::cmd::serve::bookmarks::bookmark_delete_handler;
use crate::cmd::serve::bookmarks::bookmark_list_handler;
use crate::cmd::serve::bookmarks::bookmark_note_handler;
use crate::cmd::serve::config::MIN_SESSION_SECRET_BYTES;
use crate::cmd::serve::config::ResolvedCollection;
use crate::cmd::serve::config::ResolvedGit;
use crate::cmd::serve::config::ResolvedOidc;
use crate::cmd::serve::config::ResolvedServeConfig;
use crate::cmd::serve::decks::check_deck_slug_collisions;
use crate::cmd::serve::decks::deck_add_handler;
use crate::cmd::serve::decks::deck_delete_handler;
use crate::cmd::serve::decks::decks_manage_handler;
use crate::cmd::serve::decks::resolve_custom_decks;
use crate::cmd::serve::edit::edit_get_handler;
use crate::cmd::serve::edit::edit_post_handler;
use crate::cmd::serve::export::collection_export_handler;
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
use crate::cmd::serve::hedgedoc::build_source_lossless;
use crate::cmd::serve::hedgedoc::check_startup_slug_collisions;
use crate::cmd::serve::hedgedoc::spawn_hedgedoc_sync_task;
use crate::cmd::serve::landing::landing_handler;
use crate::cmd::serve::state::AppState;
use crate::cmd::serve::state::HedgedocSource;
use crate::cmd::serve::state::SharedSession;
use crate::cmd::serve::state::evict_idle_sessions;
use crate::cmd::serve::stats::collection_stats_handler;
use crate::cmd::signals::terminate_signal;
use crate::db::Database;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::error::fail;
use crate::types::timestamp::Timestamp;
use crate::utils::ensure_dir;

/// How long a session's heartbeat must have been silent before the startup
/// sweep treats it as abandoned. Generous on purpose: closing a session that
/// is merely idle in another process is worse than leaving a crashed one open
/// until the next restart. This does not eliminate the race, only shrinks its
/// window: a CLI `drill` session idle longer than this, in a database this
/// server also serves, is closed if `serve` happens to restart during that
/// window. Accepted tradeoff — any fixed cutoff has some such window.
const SESSION_STALE_MINUTES: i64 = 60;

/// Close session rows left dangling by a crash or restart, across every
/// collection this server serves, and return the per-slug counts so the deck
/// browser can report them once.
///
/// A collection whose database cannot be opened is skipped with a log line
/// rather than failing startup: an unreadable database is the deck browser's
/// problem to report, not a reason to refuse to serve everything else.
///
/// Each collection's database is independent, so the sweeps run on their own
/// threads rather than one after another: a server with many collections
/// would otherwise have its startup delayed in proportion to how many it
/// serves.
fn sweep_dangling_sessions(
    config: &ResolvedServeConfig,
    hedgedoc_sources: &[HedgedocSource],
) -> HashMap<String, usize> {
    // Only sessions whose heartbeat has been silent this long are presumed
    // dead. A CLI `drill` sharing the database stamps its heartbeat as the
    // user works, so a live session is never swept out from under it.
    let stale_before = Timestamp::now().minus_minutes(SESSION_STALE_MINUTES);
    let collections: Vec<&ResolvedCollection> = config
        .collections
        .iter()
        .chain(hedgedoc_sources.iter().map(|s| &s.collection))
        .collect();

    let results: Vec<(String, Fallible<usize>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = collections
            .into_iter()
            .map(|rc| {
                scope.spawn(move || {
                    let closed = (|| {
                        let db_path = rc.db_path.to_str().ok_or_else(|| {
                            ErrorReport::new(format!(
                                "Database path is not valid UTF-8: {}",
                                rc.db_path.display()
                            ))
                        })?;
                        Database::new(db_path)
                            .and_then(|db| db.close_dangling_sessions(stale_before))
                    })();
                    (rc.slug.clone(), closed)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("sweep thread panicked"))
            .collect()
    });

    let mut counts = HashMap::new();
    for (slug, closed) in results {
        match closed {
            Ok(0) => {}
            Ok(n) => {
                log::info!("Closed {n} interrupted session(s) for collection '{slug}'");
                counts.insert(slug, n);
            }
            Err(e) => {
                log::error!("Could not close interrupted sessions for collection '{slug}': {e}")
            }
        }
    }
    counts
}

/// Derive the cookie signing key from `[oidc].session_secret`.
///
/// `Key::derive_from` panics on a secret shorter than
/// `MIN_SESSION_SECRET_BYTES`. `ResolvedServeConfig::from_toml` rejects those
/// already, but a config built any other way (tests, future callers) would
/// otherwise reach the panic, so the length is re-checked here and reported
/// as an ordinary error.
fn session_key(oidc: Option<&ResolvedOidc>) -> Fallible<Key> {
    match oidc {
        // Never used when `[oidc]` is absent: the routes and middleware that
        // read cookies are only registered when it is present.
        None => Ok(Key::generate()),
        Some(o) if o.session_secret.len() >= MIN_SESSION_SECRET_BYTES => {
            Ok(Key::derive_from(o.session_secret.as_bytes()))
        }
        Some(o) => fail(format!(
            "configuration error: [oidc].session_secret must be at least {} bytes long \
             (it is {}); generate one with `openssl rand -hex 32`",
            MIN_SESSION_SECRET_BYTES,
            o.session_secret.len()
        )),
    }
}

pub async fn start_serve(config: ResolvedServeConfig) -> Fallible<()> {
    // Everything the server writes lives under `data_dir`, so it is created
    // and proved writable here rather than at the first write.
    //
    // `data_dir` has no default and the shipped example says
    // `/var/lib/hashcards`, which on a systemd deployment only exists, and is
    // only owned by the service user, when the unit declares
    // `StateDirectory=hashcards`. Without that nothing created it, and the
    // first attempt to write -- adding a HedgeDoc note, minutes or restarts
    // later -- failed with a bare "Permission denied (os error 13)" naming no
    // path at all.
    if let Some(data_dir) = &config.data_dir {
        ensure_dir(data_dir, "data directory")?;
        ensure_dir(&data_dir.join("db"), "review database directory")?;
        ensure_dir(&data_dir.join("hedgedoc"), "HedgeDoc note directory")?;
    }

    // Git mode: clone/pull repo and create data directories
    let sync_git = match &config.git {
        Some(git) => {
            ensure_dir(&git.repo_dir, "git repository directory")?;
            ensure_dir(&git.db_dir, "review database directory")?;

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
        // One source per `[[hedgedoc]]` entry: notes are never grouped by
        // host, so an entry can never inherit another entry's owner.
        // build_source_lossless never drops an entry (BUG-40).
        let mut sources: Vec<HedgedocSource> = Vec::new();
        for entry in &config.hedgedoc_entries {
            sources.push(build_source_lossless(&entry.url, dd, entry.owner.clone()).await);
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
            owner: c.owner.clone(),
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
        .any(|s| s.note.last_error.is_none())
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

    // Discovery happens once at startup, not lazily on the first login
    // attempt, so a broken [oidc] config fails fast with a clear error.
    let oidc = match &config.oidc {
        Some(o) => Some(Arc::new(build_oidc_runtime(o).await?)),
        None => None,
    };

    // User-assembled decks are resolved once here; adds and deletes refresh
    // the list in place.
    let custom_decks = resolve_custom_decks(&config.custom_decks);
    // Decks must not collide with a HedgeDoc note's slug either, since both
    // are addressed through `/collection/{slug}`.
    let all_slugged: Vec<ResolvedCollection> = config
        .collections
        .iter()
        .cloned()
        .chain(hedgedoc_sources.lock().iter().map(|s| s.collection.clone()))
        .collect();
    check_deck_slug_collisions(&custom_decks, &all_slugged)?;

    let state = AppState {
        config: config.clone(),
        collections: Arc::new(RwLock::new(collection_infos)),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        last_synced: Arc::new(Mutex::new(last_synced)),
        hedgedoc_sources: hedgedoc_sources.clone(),
        custom_decks: Arc::new(Mutex::new(custom_decks)),
        hedgedoc_last_synced: hedgedoc_last_synced.clone(),
        config_path,
        // Counts were just computed above, so the stamp starts fresh.
        counts_refreshed_at: Arc::new(Mutex::new(Some(Timestamp::now()))),
        interrupted_closed: Arc::new(Mutex::new(interrupted_closed)),
        session_key: session_key(config.oidc.as_ref())?,
        oidc,
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
        .route("/decks", get(decks_manage_handler))
        .route("/decks/add", post(deck_add_handler))
        .route("/decks/delete", post(deck_delete_handler))
        .route("/collection/{slug}", get(collection_get_handler))
        .route("/collection/{slug}", post(collection_post_handler))
        .route("/collection/{slug}/start", post(collection_start_handler))
        .route("/collection/{slug}/stats", get(collection_stats_handler))
        .route("/collection/{slug}/export", get(collection_export_handler))
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
        );

    // The stylesheet, its fonts and the vendored libraries hold nothing about
    // anyone's cards, and every page that is shown to a logged-out user — the
    // login page, the "session expired" page — links them. Behind the gate
    // those links redirect to `/auth/login`, so exactly the pages a user meets
    // before they have a session were the pages that arrived unstyled.
    let static_routes = Router::new()
        .route("/manifest.json", get(manifest_handler))
        .route("/icons/icon-192.png", get(icon_192_handler))
        .route("/icons/icon-512.png", get(icon_512_handler))
        .route("/script.js", get(script_handler))
        .route("/style.css", get(style_handler))
        .route("/fonts/{name}", get(font_handler))
        .route(KATEX_CSS_URL, get(katex_css_handler))
        .route(KATEX_JS_URL, get(katex_js_handler))
        .route(KATEX_MHCHEM_JS_URL, get(katex_mhchem_js_handler))
        .route("/katex/fonts/{*path}", get(katex_font_handler))
        .route(HLJS_CSS_URL, get(hljs_css_handler))
        .route(HLJS_JS_URL, get(hljs_js_handler));

    // `/auth/*` must NOT go through `require_auth` — gating the login route
    // itself behind login is the classic OIDC redirect loop.
    let app = if state.oidc.is_some() {
        let auth_routes = Router::new()
            .route("/auth/login", get(login_handler))
            .route("/auth/callback", get(callback_handler))
            // POST, not GET: a GET logout is triggerable by any third-party
            // page embedding it as an image.
            .route("/auth/logout", post(logout_handler));
        app.layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth,
        ))
        .merge(auth_routes)
    } else {
        app
    };
    let app = app.merge(static_routes);
    let app = app.with_state(state);

    log::debug!("Starting server on {bind}");
    let listener = TcpListener::bind(&bind).await?;
    println!("hashcards-web running on http://{bind}/");

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
