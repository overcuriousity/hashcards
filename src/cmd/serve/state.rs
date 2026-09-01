use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum_extra::extract::cookie::Key;
use chrono::Duration;
use parking_lot::Mutex;

use tokio::sync::RwLock;

use crate::cmd::drill::server::AnswerControls;
use crate::cmd::drill::state::MutableState;
use crate::cmd::serve::auth::OidcRuntime;
use crate::cmd::serve::config::ResolvedCollection;
use crate::cmd::serve::config::ResolvedServeConfig;
use crate::cmd::serve::decks::ResolvedCustomDeck;
use crate::types::timestamp::Timestamp;

/// A drill session shared behind a per-slug lock. Handlers clone the `Arc`
/// out of the map (releasing the map lock immediately) and lock the session
/// itself for the duration of the request, so an error can never remove the
/// session from the map.
pub type SharedSession = Arc<Mutex<DrillSession>>;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<ResolvedServeConfig>,
    pub collections: Arc<RwLock<Vec<CollectionInfo>>>,
    pub sessions: Arc<Mutex<HashMap<String, SharedSession>>>,
    pub last_synced: Arc<Mutex<Option<Timestamp>>>,
    pub hedgedoc_sources: Arc<Mutex<Vec<HedgedocSource>>>,
    /// User-assembled cross-collection decks, resolved at startup and
    /// refreshed whenever one is added or deleted. A `parking_lot` mutex
    /// like `hedgedoc_sources`: these are read from the blocking drill
    /// paths, not only from async handlers.
    pub custom_decks: Arc<Mutex<Vec<ResolvedCustomDeck>>>,
    pub hedgedoc_last_synced: Arc<Mutex<Option<Timestamp>>>,
    pub config_path: Arc<Mutex<Option<PathBuf>>>,
    /// When the collection counts were last recomputed (BUG-45).
    pub counts_refreshed_at: Arc<Mutex<Option<Timestamp>>>,
    /// Per-slug count of session rows closed by the startup sweep, waiting to
    /// be reported to the user. The deck browser takes the entry the first
    /// time it renders, so the notice is shown once rather than on every
    /// visit (see `close_dangling_sessions`).
    pub interrupted_closed: Arc<Mutex<HashMap<String, usize>>>,
    /// Signs the OIDC session and login-flow cookies. When `[oidc]` is not
    /// configured this key is generated randomly at startup and never used
    /// — keeping it non-optional avoids threading `Option` through every
    /// cookie read/write, since the auth routes and middleware that use it
    /// are only ever registered when `[oidc]` is configured.
    pub session_key: Key,
    /// Set when `[oidc]` is configured. Gates every route except `/auth/*`
    /// behind login and scopes collections/notes to their `owner`.
    pub oidc: Option<Arc<OidcRuntime>>,
}

/// Lets `axum_extra`'s `SignedCookieJar` extractor pull the signing key
/// straight out of `AppState`.
impl axum::extract::FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Key {
        state.session_key.clone()
    }
}

pub struct CollectionInfo {
    pub name: String,
    pub slug: String,
    pub total_cards: usize,
    pub due_today: usize,
    pub owner: Option<String>,
}

/// A single HedgeDoc note, used as a collection of its own.
///
/// Notes are deliberately *not* grouped by HedgeDoc host. Grouping made the
/// host's first-seen `owner` the owner of every note on it, so on a shared
/// HedgeDoc instance one user's note landed in another user's collection.
/// One note, one collection, one database, one owner.
#[derive(Clone)]
pub struct HedgedocSource {
    /// The scheme/host/port the note lives on. Display only.
    pub source_uri: String,
    pub collection: ResolvedCollection,
    pub note: HedgedocNote,
}

#[derive(Clone)]
pub struct HedgedocNote {
    pub url: String,
    pub deck_name: String,
    pub file_name: String,
    pub last_error: Option<String>,
}

pub struct DrillSession {
    pub directory: PathBuf,
    pub macros: Vec<(String, String)>,
    pub total_cards: usize,
    pub session_started_at: Timestamp,
    /// Last time this session served a page or handled an action.
    /// Sessions idle past the configured timeout are evicted (BUG-08).
    pub last_activity_at: Timestamp,
    pub answer_controls: AnswerControls,
    pub mutable: MutableState,
    /// True once this session has been taken out of the sessions map (ended
    /// via Home, evicted as idle, or replaced by a new drill).
    ///
    /// Request handlers need to know that the session they cloned out of the
    /// map is still the one the map holds. Re-reading the map would mean
    /// taking the map lock while holding the session lock, inverting the
    /// global lock order (map, then session) that eviction and the landing
    /// page follow -- a deadlock with no timeout. Whoever removes a session
    /// from the map instead marks it here, under the session lock it already
    /// has to take, and handlers just read the flag.
    detached: bool,
}

impl DrillSession {
    pub fn new(
        directory: PathBuf,
        macros: Vec<(String, String)>,
        session_started_at: Timestamp,
        answer_controls: AnswerControls,
        mutable: MutableState,
    ) -> Self {
        Self {
            directory,
            macros,
            total_cards: mutable.cards.len(),
            session_started_at,
            last_activity_at: session_started_at,
            answer_controls,
            mutable,
            detached: false,
        }
    }

    /// Mark this session as removed from the sessions map.
    ///
    /// Callers must already hold the map lock, so that the mark and the
    /// removal are seen together.
    pub fn detach(&mut self) {
        self.detached = true;
    }

    /// Has this session been removed from the sessions map?
    pub fn is_detached(&self) -> bool {
        self.detached
    }
}

/// Remove sessions idle for at least `timeout_minutes` and close their DB
/// session rows. `timeout_minutes == 0` disables eviction. Returns the
/// evicted slugs.
///
/// DB work happens after the map lock is released, so eviction never holds
/// the sessions lock across SQLite calls.
pub fn evict_idle_sessions(
    sessions: &Mutex<HashMap<String, SharedSession>>,
    timeout_minutes: u64,
    now: Timestamp,
) -> Vec<String> {
    if timeout_minutes == 0 {
        return Vec::new();
    }
    let timeout = Duration::minutes(timeout_minutes as i64);
    let expired: Vec<(String, SharedSession)> = {
        let mut map = sessions.lock();
        let keys: Vec<String> = map
            .iter()
            .filter(|(_, s)| now.into_inner() - s.lock().last_activity_at.into_inner() >= timeout)
            .map(|(k, _)| k.clone())
            .collect();
        keys.into_iter()
            .filter_map(|k| map.remove(&k).map(|s| (k, s)))
            .inspect(|(_, s)| s.lock().detach())
            .collect()
    };
    let mut evicted = Vec::new();
    for (slug, session) in expired {
        let session = session.lock();
        if session.mutable.finished_at.is_none() {
            if let Err(e) = session.mutable.dbs.close_all(now) {
                log::error!("Failed to close evicted session for collection '{slug}': {e}");
            }
        }
        evicted.push(slug);
    }
    evicted
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;
    use crate::cmd::drill::cache::Cache;
    use crate::cmd::drill::state::SessionDbs;
    use crate::error::ErrorReport;
    use crate::error::Fallible;
    use crate::rng::TinyRng;
    use crate::types::performance::Jitter;

    fn session_started_at(at: &str, dir: &Path) -> Fallible<SharedSession> {
        let db_path = dir.join("test.db");
        let db_path_str = db_path
            .to_str()
            .ok_or_else(|| ErrorReport::new("non-UTF-8 temp path"))?;
        let db = crate::db::Database::new(db_path_str)?;
        let started_at = Timestamp::try_from(at.to_string())?;
        let session_id = db.create_session(started_at)?;
        let mutable = MutableState::new(
            SessionDbs::single(db, session_id),
            Cache::new(),
            Vec::new(),
            Jitter::none(),
            TinyRng::from_seed(1),
        );
        Ok(Arc::new(Mutex::new(DrillSession::new(
            dir.to_path_buf(),
            Vec::new(),
            started_at,
            AnswerControls::Full,
            mutable,
        ))))
    }

    /// BUG-08 regression: a session idle past the timeout is removed from
    /// the map and its DB session row is closed.
    #[test]
    fn test_idle_session_is_evicted_and_db_row_closed() -> Fallible<()> {
        let dir = tempdir()?;
        let session = session_started_at("2026-01-01T10:00:00.000", dir.path())?;
        let sessions = Mutex::new(HashMap::from([("demo".to_string(), session)]));

        // 25 hours later, with a 24h (1440 minute) timeout.
        let now = Timestamp::try_from("2026-01-02T11:00:00.000".to_string())?;
        let evicted = evict_idle_sessions(&sessions, 1440, now);
        assert_eq!(evicted, vec!["demo".to_string()]);
        assert!(sessions.lock().is_empty());

        // The DB session row was closed with the eviction time.
        let db_path = dir.path().join("test.db");
        let db_path_str = db_path
            .to_str()
            .ok_or_else(|| ErrorReport::new("non-UTF-8 temp path"))?;
        let db = crate::db::Database::new(db_path_str)?;
        let rows = db.get_all_sessions()?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ended_at, now);
        Ok(())
    }

    /// Eviction must mark the session detached, not merely drop it from the
    /// map: a request handler that already cloned the `Arc` learns the
    /// session is gone by reading this flag. Re-reading the map under the
    /// session lock would invert the global lock order and deadlock.
    #[test]
    fn test_evicted_session_is_marked_detached() -> Fallible<()> {
        let dir = tempdir()?;
        let session = session_started_at("2026-01-01T10:00:00.000", dir.path())?;
        // A handler holding its own clone, as one would across a request.
        let handler_view = Arc::clone(&session);
        assert!(!handler_view.lock().is_detached());

        let sessions = Mutex::new(HashMap::from([("demo".to_string(), session)]));
        let now = Timestamp::try_from("2026-01-02T11:00:00.000".to_string())?;
        assert_eq!(evict_idle_sessions(&sessions, 1440, now), vec!["demo"]);

        assert!(
            handler_view.lock().is_detached(),
            "evicted session must be observable as detached through an \
             already-cloned handle"
        );
        Ok(())
    }

    /// A session inside the timeout window is left alone.
    #[test]
    fn test_active_session_is_not_evicted() -> Fallible<()> {
        let dir = tempdir()?;
        let session = session_started_at("2026-01-01T10:00:00.000", dir.path())?;
        let sessions = Mutex::new(HashMap::from([("demo".to_string(), session)]));

        // Only one hour later.
        let now = Timestamp::try_from("2026-01-01T11:00:00.000".to_string())?;
        let evicted = evict_idle_sessions(&sessions, 1440, now);
        assert!(evicted.is_empty());
        assert_eq!(sessions.lock().len(), 1);
        Ok(())
    }

    /// A timeout of 0 disables eviction entirely.
    #[test]
    fn test_zero_timeout_disables_eviction() -> Fallible<()> {
        let dir = tempdir()?;
        let session = session_started_at("2020-01-01T10:00:00.000", dir.path())?;
        let sessions = Mutex::new(HashMap::from([("demo".to_string(), session)]));
        let now = Timestamp::try_from("2026-01-01T10:00:00.000".to_string())?;
        let evicted = evict_idle_sessions(&sessions, 0, now);
        assert!(evicted.is_empty());
        assert_eq!(sessions.lock().len(), 1);
        Ok(())
    }
}
