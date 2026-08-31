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
use std::sync::Arc;

use parking_lot::Mutex;

use tokio::sync::oneshot::Sender;

use std::collections::HashSet;

use crate::cmd::drill::cache::Cache;
use crate::cmd::drill::server::AnswerControls;
use crate::db::Database;
use crate::fsrs::Grade;
use crate::rng::TinyRng;
use crate::types::card::Card;
use crate::types::card_hash::CardHash;
use crate::types::performance::Jitter;
use crate::types::performance::Performance;
use crate::types::timestamp::Timestamp;

#[derive(Clone)]
pub struct ServerState {
    pub port: u16,
    pub directory: PathBuf,
    pub macros: Vec<(String, String)>,
    pub total_cards: usize,
    pub session_started_at: Timestamp,
    pub mutable: Arc<Mutex<MutableState>>,
    pub shutdown_tx: Arc<Mutex<Option<Sender<()>>>>,
    pub answer_controls: AnswerControls,
}

pub struct MutableState {
    pub reveal: bool,
    pub db: Database,
    pub session_id: i64,
    pub cache: Cache,
    pub cards: Vec<Card>,
    pub reviews: Vec<Review>,
    pub finished_at: Option<Timestamp>,
    /// Timestamp when the current card was revealed (for per-card timing).
    pub card_shown_at: Option<Timestamp>,
    /// Fractional random jitter applied to computed intervals (FEAT-05).
    pub jitter: Jitter,
    /// RNG for interval jitter, seeded once per session.
    pub rng: TinyRng,
}

impl MutableState {
    /// State for a freshly started session: nothing revealed, nothing graded.
    pub fn new(
        db: Database,
        session_id: i64,
        cache: Cache,
        cards: Vec<Card>,
        jitter: Jitter,
        rng: TinyRng,
    ) -> Self {
        Self {
            reveal: false,
            db,
            session_id,
            cache,
            cards,
            reviews: Vec::new(),
            finished_at: None,
            card_shown_at: None,
            jitter,
            rng,
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
    pub duration_ms: Option<i64>,
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
            duration_ms: None,
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
            db,
            session_id,
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
