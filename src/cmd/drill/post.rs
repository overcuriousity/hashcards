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

use axum::Form;
use axum::extract::State;
use axum::response::Redirect;
use serde::Deserialize;

use crate::cmd::drill::state::MutableState;
use crate::cmd::drill::state::Review;
use crate::cmd::drill::state::ServerState;
use crate::db::ReviewRecord;
use crate::error::Fallible;
use crate::flash::Flash;
use crate::fsrs::Grade;
use crate::types::card::Card;
use crate::types::card_hash::CardHash;
use crate::types::performance::Performance;
use crate::types::performance::ReviewedPerformance;
use crate::types::performance::update_performance;
use crate::types::timestamp::Timestamp;

#[derive(Debug, Deserialize)]
pub enum Action {
    Reveal,
    Undo,
    End,
    Forgot,
    Hard,
    Good,
    Easy,
    Shutdown,
    Home,
    Bookmark,
    Unbookmark,
}

impl Action {
    pub fn grade(&self) -> Grade {
        match self {
            Action::Forgot => Grade::Forgot,
            Action::Hard => Grade::Hard,
            Action::Good => Grade::Good,
            Action::Easy => Grade::Easy,
            _ => panic!("Action does not correspond to a grade"),
        }
    }
}

#[derive(Deserialize)]
pub struct FormData {
    pub action: Action,
}

/// Result of handling an action on the drill session.
pub enum ActionResult {
    /// Continue drilling (redirect back to the same page).
    Continue,
    /// Continue drilling, showing a one-shot flash message.
    ContinueWithFlash(Flash),
    /// The session finished (all cards done or user pressed End).
    SessionFinished,
    /// The user requested server shutdown (drill mode only).
    Shutdown,
    /// The user requested to go back to the collection list (serve mode).
    Home,
}

pub async fn post_handler(
    State(state): State<ServerState>,
    Form(form): Form<FormData>,
) -> Redirect {
    match action_handler(state, form.action).await {
        Ok(Some(flash)) => flash.redirect("/"),
        Ok(None) => Redirect::to("/"),
        Err(e) => {
            log::error!("error: {e}");
            Flash::error(e.to_string()).redirect("/")
        }
    }
}

async fn action_handler(state: ServerState, action: Action) -> Fallible<Option<Flash>> {
    let mut mutable = state.mutable.lock().unwrap();
    let result = handle_action(&mut mutable, state.session_started_at, action)?;
    match result {
        ActionResult::Shutdown => {
            // Release the lock before sending shutdown signal.
            drop(mutable);
            let mut shutdown_tx = state.shutdown_tx.lock().unwrap();
            if let Some(tx) = shutdown_tx.take() {
                let _ = tx.send(());
            }
            Ok(None)
        }
        ActionResult::ContinueWithFlash(flash) => Ok(Some(flash)),
        _ => Ok(None),
    }
}

/// Core action handling logic, reusable by both drill and serve modes.
pub fn handle_action(
    mutable: &mut MutableState,
    session_started_at: Timestamp,
    action: Action,
) -> Fallible<ActionResult> {
    match action {
        Action::Reveal => {
            if !mutable.reveal {
                mutable.reveal = true;
                mutable.card_shown_at = Some(Timestamp::now());
            }
            Ok(ActionResult::Continue)
        }
        Action::Undo => {
            if !mutable.reviews.is_empty() {
                let last_review: Review = mutable.reviews.pop().unwrap();
                if last_review.should_repeat() {
                    // Remove the card from the back of the queue.
                    mutable.cards.pop();
                }
                let card: Card = last_review.card;
                let hash: CardHash = card.hash();
                mutable.cards.insert(0, card);
                // Void the review and restore prior performance atomically.
                mutable.db.void_review_and_restore_performance(
                    last_review.review_id,
                    hash,
                    last_review.prev_performance,
                )?;
                mutable.cache.update(hash, last_review.prev_performance)?;
                mutable.finished_at = None;
                mutable.reveal = false;
                mutable.card_shown_at = None;
            }
            Ok(ActionResult::Continue)
        }
        Action::End => {
            finish_session(mutable, session_started_at)?;
            Ok(ActionResult::SessionFinished)
        }
        Action::Shutdown => {
            if mutable.finished_at.is_some() {
                Ok(ActionResult::Shutdown)
            } else {
                Ok(ActionResult::ContinueWithFlash(Flash::error(
                    "The session is still in progress. Press End to finish it before shutting down.",
                )))
            }
        }
        Action::Home => Ok(ActionResult::Home),
        Action::Bookmark => {
            // Write immediately to DB so bookmarks survive aborted sessions.
            if !mutable.cards.is_empty() {
                let hash = mutable.cards[0].hash();
                let now = Timestamp::now();
                mutable.db.insert_card_if_new(hash, now)?;
                mutable.db.insert_bookmark(hash, None, now)?;
            }
            Ok(ActionResult::Continue)
        }
        Action::Unbookmark => {
            if !mutable.cards.is_empty() {
                let hash = mutable.cards[0].hash();
                mutable.db.delete_bookmark(hash)?;
            }
            Ok(ActionResult::Continue)
        }
        Action::Forgot | Action::Hard | Action::Good | Action::Easy => {
            if mutable.reveal {
                let head: Card = match mutable.cards.first() {
                    Some(card) => card.clone(),
                    None => return Ok(ActionResult::Continue),
                };
                let reviewed_at: Timestamp = Timestamp::now();
                let duration_ms: Option<i64> = mutable.card_shown_at.map(|shown_at| {
                    (reviewed_at.into_inner() - shown_at.into_inner())
                        .num_milliseconds()
                        .max(0)
                });
                let hash: CardHash = head.hash();
                let grade: Grade = action.grade();
                let prev_performance: Performance = mutable.cache.get(hash)?;
                let performance: ReviewedPerformance =
                    update_performance(prev_performance, grade, reviewed_at);
                let record = ReviewRecord {
                    card_hash: hash,
                    reviewed_at,
                    grade,
                    stability: performance.stability,
                    difficulty: performance.difficulty,
                    interval_raw: performance.interval_raw,
                    interval_days: performance.interval_days,
                    due_date: performance.due_date,
                    duration_ms,
                };
                let new_performance = Performance::Reviewed(performance);
                // Write review and card performance atomically, and commit
                // BEFORE mutating any in-memory state. If this fails, the
                // queue, cache, undo stack, and reveal state are untouched
                // and the grade can simply be retried.
                let review_id = mutable.db.insert_review_and_update_performance(
                    mutable.session_id,
                    &record,
                    new_performance,
                )?;
                // The transaction committed; it is now safe to mutate memory.
                mutable.cache.update(hash, new_performance)?;
                mutable.card_shown_at = None;
                let card: Card = mutable.cards.remove(0);
                let review = Review {
                    card: card.clone(),
                    review_id,
                    grade: record.grade,
                    duration_ms: record.duration_ms,
                    prev_performance,
                };
                if review.should_repeat() {
                    mutable.cards.push(card);
                }
                mutable.reviews.push(review);
                mutable.reveal = false;

                // Was this the last card?
                if mutable.cards.is_empty() {
                    finish_session(mutable, session_started_at)?;
                    return Ok(ActionResult::SessionFinished);
                }
            }
            Ok(ActionResult::Continue)
        }
    }
}

fn finish_session(mutable: &mut MutableState, _session_started_at: Timestamp) -> Fallible<()> {
    log::debug!("Session completed");
    let session_ended_at = Timestamp::now();
    mutable
        .db
        .close_session(mutable.session_id, session_ended_at)?;
    mutable.finished_at = Some(session_ended_at);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::drill::cache::Cache;
    use crate::cmd::drill::state::MutableState;
    use crate::db::Database;
    use crate::flash::FlashKind;
    use crate::types::card::CardContent;
    use crate::types::performance::Performance;
    use std::path::PathBuf;

    fn make_mutable() -> MutableState {
        let db = Database::new(":memory:").unwrap();
        let session_id = db.create_session(Timestamp::now()).unwrap();
        MutableState {
            reveal: false,
            session_id,
            db,
            cache: Cache::new(),
            cards: Vec::new(),
            reviews: Vec::new(),
            finished_at: None,
            card_shown_at: None,
        }
    }

    fn make_card(question: &str) -> Card {
        Card::new(
            "TestDeck".to_string(),
            PathBuf::from("/tmp/test-deck.md"),
            (1, 2),
            CardContent::new_basic(question, "answer"),
        )
    }

    /// A session whose cards exist in the queue, the cache, AND the cards table.
    fn make_state_with_cards(cards: Vec<Card>) -> MutableState {
        let db = Database::new(":memory:").unwrap();
        let now = Timestamp::now();
        let mut cache = Cache::new();
        for card in &cards {
            db.insert_card(card.hash(), now).unwrap();
            cache.insert(card.hash(), Performance::New).unwrap();
        }
        let session_id = db.create_session(now).unwrap();
        MutableState {
            reveal: false,
            db,
            session_id,
            cache,
            cards,
            reviews: Vec::new(),
            finished_at: None,
            card_shown_at: None,
        }
    }

    #[test]
    fn test_action_grade() {
        assert_eq!(Action::Forgot.grade(), Grade::Forgot);
        assert_eq!(Action::Hard.grade(), Grade::Hard);
        assert_eq!(Action::Good.grade(), Grade::Good);
        assert_eq!(Action::Easy.grade(), Grade::Easy);
    }

    #[test]
    fn test_home_returns_home() {
        let mut mutable = make_mutable();
        let now = Timestamp::now();
        let result = handle_action(&mut mutable, now, Action::Home).unwrap();
        assert!(matches!(result, ActionResult::Home));
    }

    #[test]
    fn test_shutdown_before_finish_flashes_explanation() {
        let mut mutable = make_mutable();
        assert!(mutable.finished_at.is_none());
        let now = Timestamp::now();
        let result = handle_action(&mut mutable, now, Action::Shutdown).unwrap();
        match result {
            ActionResult::ContinueWithFlash(flash) => {
                assert_eq!(flash.kind, FlashKind::Error);
                assert!(flash.message.contains("still in progress"));
            }
            _ => panic!("expected ContinueWithFlash"),
        }
    }

    #[test]
    fn test_reveal_sets_flag() {
        let mut mutable = make_mutable();
        let now = Timestamp::now();
        assert!(!mutable.reveal);
        let result = handle_action(&mut mutable, now, Action::Reveal).unwrap();
        assert!(matches!(result, ActionResult::Continue));
        assert!(mutable.reveal);
    }

    #[test]
    fn test_end_finishes_session() {
        let mut mutable = make_mutable();
        let now = Timestamp::now();
        let result = handle_action(&mut mutable, now, Action::End).unwrap();
        assert!(matches!(result, ActionResult::SessionFinished));
        assert!(mutable.finished_at.is_some());
    }

    #[test]
    fn test_grade_db_failure_leaves_state_unchanged() {
        // The card is in the queue and cache but NOT in the cards table, so
        // inserting its review fails the foreign-key check: an injected DB
        // write failure inside the grade transaction.
        let card = make_card("Q1");
        let db = Database::new(":memory:").unwrap();
        let session_id = db.create_session(Timestamp::now()).unwrap();
        let mut cache = Cache::new();
        cache.insert(card.hash(), Performance::New).unwrap();
        let mut mutable = MutableState {
            reveal: true,
            db,
            session_id,
            cache,
            cards: vec![card.clone()],
            reviews: Vec::new(),
            finished_at: None,
            card_shown_at: Some(Timestamp::now()),
        };
        let result = handle_action(&mut mutable, Timestamp::now(), Action::Good);
        assert!(result.is_err(), "the injected DB failure must propagate");
        // On error, in-memory state must be completely unchanged.
        assert_eq!(mutable.cards.len(), 1, "card must still be in the queue");
        assert_eq!(
            mutable.cards[0].hash(),
            card.hash(),
            "card must still be at the head"
        );
        assert!(
            mutable.reviews.is_empty(),
            "no review must be recorded in memory"
        );
        assert!(
            mutable.reveal,
            "reveal must stay set so the grade can be retried"
        );
        assert!(
            mutable.card_shown_at.is_some(),
            "timing info must not be cleared"
        );
        assert!(matches!(
            mutable.cache.get(card.hash()).unwrap(),
            Performance::New
        ));
        assert!(
            mutable
                .db
                .get_reviews_for_session(mutable.session_id)
                .unwrap()
                .is_empty(),
            "no review row must exist"
        );
    }
}
