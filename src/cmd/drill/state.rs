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

use std::path::PathBuf;

use std::collections::HashMap;
use std::collections::HashSet;

use crate::cmd::drill::cache::Cache;
use crate::db::Database;
use crate::error::Fallible;
use crate::fsrs::Grade;
use crate::rng::TinyRng;
use crate::types::card::Card;
use crate::types::card_hash::CardHash;
use crate::types::performance::Jitter;
use crate::types::performance::Performance;
use crate::types::timestamp::Timestamp;

/// One collection's database, with the session row opened in it.
pub struct SessionDb {
    pub db: Database,
    pub session_id: i64,
    /// Set only for a session drawing on more than one collection, where a
    /// card's media must resolve against the collection it actually came
    /// from. `None` means "use the session's own collection", which is the
    /// CLI drill and the ordinary single-collection serve.
    pub source: Option<SessionSource>,
}

/// Where a routed card's files live, and how to address them over HTTP.
#[derive(Clone)]
pub struct SessionSource {
    pub coll_dir: PathBuf,
    pub file_url_prefix: String,
}

/// The databases a drill session writes to.
///
/// Drilling one collection uses exactly one. A custom deck draws its cards
/// from several collections, and each review must land in the database its
/// card actually belongs to — a card scheduled in two places would come due
/// twice, on schedules that drift apart. Routing is by card hash, which is
/// the card's identity everywhere else too.
pub struct SessionDbs {
    dbs: Vec<SessionDb>,
    /// Card hash to index into `dbs`. Empty for a single-collection session,
    /// where every card belongs to `dbs[0]`.
    routes: HashMap<CardHash, usize>,
}

impl SessionDbs {
    /// A session over a single collection: every card routes to this database.
    pub fn single(db: Database, session_id: i64) -> Self {
        Self {
            dbs: vec![SessionDb {
                db,
                session_id,
                source: None,
            }],
            routes: HashMap::new(),
        }
    }

    /// A session drawing on several collections. `routes` maps each card to
    /// its collection's index in `dbs`.
    pub fn routed(dbs: Vec<SessionDb>, routes: HashMap<CardHash, usize>) -> Self {
        Self { dbs, routes }
    }

    /// The database owning `hash`.
    ///
    /// A card with no route falls back to the first database: that is the
    /// single-collection case, where the map is empty by construction.
    pub fn for_card(&self, hash: CardHash) -> &SessionDb {
        let idx = self.routes.get(&hash).copied().unwrap_or(0);
        // `dbs` is never empty, and every index in `routes` came from it.
        &self.dbs[idx.min(self.dbs.len().saturating_sub(1))]
    }

    /// The database owning `hash`, for the writes that need `&mut Database`.
    pub fn for_card_mut(&mut self, hash: CardHash) -> &mut SessionDb {
        let idx = self.routes.get(&hash).copied().unwrap_or(0);
        let idx = idx.min(self.dbs.len().saturating_sub(1));
        &mut self.dbs[idx]
    }

    /// The first database. Used for session-wide bookkeeping that is not
    /// tied to a particular card.
    pub fn primary(&self) -> &SessionDb {
        &self.dbs[0]
    }

    pub fn all(&self) -> &[SessionDb] {
        &self.dbs
    }

    /// Mark every open session row as still alive.
    pub fn touch_all(&self, now: Timestamp) -> Fallible<()> {
        for entry in &self.dbs {
            entry.db.touch_session(entry.session_id, now)?;
        }
        Ok(())
    }

    /// Close every open session row.
    pub fn close_all(&self, ended_at: Timestamp) -> Fallible<()> {
        for entry in &self.dbs {
            entry.db.close_session(entry.session_id, ended_at)?;
        }
        Ok(())
    }

    /// Every review recorded in this session, across all its databases.
    pub fn all_reviews(&self) -> Fallible<Vec<crate::db::ReviewRow>> {
        let mut rows = Vec::new();
        for entry in &self.dbs {
            rows.extend(entry.db.get_reviews_for_session(entry.session_id)?);
        }
        Ok(rows)
    }
}

pub struct MutableState {
    pub reveal: bool,
    pub dbs: SessionDbs,
    pub cache: Cache,
    pub cards: Vec<Card>,
    pub reviews: Vec<Review>,
    pub finished_at: Option<Timestamp>,
    /// Timestamp when the current card was first shown (for per-card timing).
    pub card_shown_at: Option<Timestamp>,
    /// Fractional random jitter applied to computed intervals (FEAT-05).
    pub jitter: Jitter,
    /// RNG for interval jitter, seeded once per session.
    pub rng: TinyRng,
}

impl MutableState {
    /// State for a freshly started session: nothing revealed, nothing graded.
    pub fn new(
        dbs: SessionDbs,
        cache: Cache,
        cards: Vec<Card>,
        jitter: Jitter,
        rng: TinyRng,
    ) -> Self {
        Self {
            reveal: false,
            dbs,
            cache,
            cards,
            reviews: Vec::new(),
            finished_at: None,
            card_shown_at: None,
            jitter,
            rng,
        }
    }

    /// Record that the current card has just been served to the client.
    ///
    /// Called from the GET path so per-card durations include recall time
    /// (measured from display, not from reveal). Idempotent: a page refresh
    /// does not restart the timer. No-op when the session is finished or the
    /// queue is empty.
    pub fn mark_card_shown(&mut self) {
        if self.card_shown_at.is_none() && self.finished_at.is_none() && !self.cards.is_empty() {
            self.card_shown_at = Some(Timestamp::now());
        }
    }

    /// Session progress as `(first_graded, repeats)`.
    ///
    /// `first_graded` counts distinct cards graded at least once — the
    /// progress bar advances on the first grade of each card. `repeats`
    /// counts the additional grades of already-counted cards, i.e. the
    /// completed re-queues from Forgot/Hard. Both are derived from the
    /// undo-aware `reviews` list, so Undo rolls progress back and no extra
    /// session state is stored.
    pub fn progress(&self) -> (usize, usize) {
        let mut seen: HashSet<CardHash> = HashSet::new();
        for review in &self.reviews {
            seen.insert(review.card.hash());
        }
        let first_graded = seen.len();
        let repeats = self.reviews.len() - first_graded;
        (first_graded, repeats)
    }
}

#[derive(Clone)]
pub struct Review {
    pub card: Card,
    pub review_id: i64,
    pub grade: Grade,
    /// Performance before this review, used to restore state on undo.
    pub prev_performance: Performance,
}

impl Review {
    pub fn should_repeat(&self) -> bool {
        self.grade == Grade::Forgot || self.grade == Grade::Hard
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cmd::drill::cache::Cache;
    use crate::db::Database;
    use crate::types::card::CardContent;
    use crate::types::performance::Jitter;

    fn make_card(question: &str) -> Card {
        Card::new(
            "test-deck".to_string(),
            PathBuf::from("/tmp/deck.md"),
            (1, 2),
            CardContent::new_basic(question, "answer"),
        )
    }

    fn make_review(card: &Card, grade: Grade) -> Review {
        Review {
            card: card.clone(),
            review_id: 1,
            grade,
            prev_performance: Performance::New,
        }
    }

    /// FEAT-08: the bar advances on the first grade of each card; further
    /// grades of the same card count as repeats. Derived from `reviews`,
    /// so Undo (which pops `reviews`) rolls progress back automatically.
    #[test]
    fn test_progress_counts_first_grades_and_repeats() {
        let db = Database::new(":memory:").unwrap();
        let session_id = db.create_session(Timestamp::now()).unwrap();
        let a = make_card("question a");
        let b = make_card("question b");
        let mut mutable = MutableState::new(
            SessionDbs::single(db, session_id),
            Cache::new(),
            vec![a.clone(), b.clone()],
            Jitter::none(),
            TinyRng::from_seed(1),
        );
        assert_eq!(mutable.progress(), (0, 0));
        // First grade of card a; Forgot re-queues it but it still counts once.
        mutable.reviews.push(make_review(&a, Grade::Forgot));
        assert_eq!(mutable.progress(), (1, 0));
        // First grade of card b.
        mutable.reviews.push(make_review(&b, Grade::Good));
        assert_eq!(mutable.progress(), (2, 0));
        // Card a comes around again: a repeat, not new progress.
        mutable.reviews.push(make_review(&a, Grade::Good));
        assert_eq!(mutable.progress(), (2, 1));
        // Undo pops the repeat review; progress recovers on its own.
        mutable.reviews.pop();
        assert_eq!(mutable.progress(), (2, 0));
    }
}
