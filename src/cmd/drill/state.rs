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
use std::sync::Mutex;

use tokio::sync::oneshot::Sender;

use crate::cmd::drill::cache::Cache;
use crate::cmd::drill::server::AnswerControls;
use crate::db::Database;
use crate::rng::TinyRng;
use crate::fsrs::Grade;
use crate::types::card::Card;
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
