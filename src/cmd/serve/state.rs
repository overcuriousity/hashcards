use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Duration;
use parking_lot::Mutex;

use tokio::sync::RwLock;

use crate::cmd::drill::server::AnswerControls;
use crate::cmd::drill::state::MutableState;
use crate::cmd::serve::config::ResolvedCollection;
use crate::cmd::serve::config::ResolvedServeConfig;
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
    pub hedgedoc_last_synced: Arc<Mutex<Option<Timestamp>>>,
    pub config_path: Arc<Mutex<Option<PathBuf>>>,
    /// When the collection counts were last recomputed (BUG-45).
    pub counts_refreshed_at: Arc<Mutex<Option<Timestamp>>>,
}

pub struct CollectionInfo {
    pub name: String,
    pub slug: String,
    pub total_cards: usize,
    pub due_today: usize,
}

/// A HedgeDoc markdown endpoint used as a collection source.
#[derive(Clone)]
pub struct HedgedocSource {
    pub source_uri: String,
    pub collection: ResolvedCollection,
    pub notes: Vec<HedgedocNote>,
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
        }
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
            .collect()
    };
    let mut evicted = Vec::new();
    for (slug, session) in expired {
        let session = session.lock();
        if session.mutable.finished_at.is_none() {
            if let Err(e) = session
                .mutable
                .db
                .close_session(session.mutable.session_id, now)
            {
                log::error!(
                    "Failed to close evicted session {} for collection '{slug}': {e}",
                    session.mutable.session_id
                );
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
            db,
            session_id,
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
