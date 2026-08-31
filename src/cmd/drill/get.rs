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

use std::collections::HashMap;
use std::path::Path;

use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use std::collections::HashSet;

use maud::Markup;
use maud::html;

use crate::cmd::drill::server::AnswerControls;
use crate::cmd::drill::state::MutableState;
use crate::cmd::drill::state::ServerState;
use crate::cmd::drill::template::page_template;
use crate::db::ReviewRow;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::error::fail;
use crate::flash::Flash;
use crate::markdown::MarkdownRenderConfig;
use crate::media::resolve::MediaResolverBuilder;
use crate::types::card::Card;
use crate::types::card::CardType;
use crate::types::card_hash::CardHash;
use crate::types::timestamp::Timestamp;

/// What to show on the completion page.
pub enum CompletionAction {
    /// Show a "Shutdown" button (drill mode).
    Shutdown,
    /// Show a "Back to Collections" link (serve mode).
    BackToCollections,
}

/// Everything the rendering functions need, decoupled from ServerState.
pub struct RenderContext<'a> {
    pub directory: &'a Path,
    pub total_cards: usize,
    pub session_started_at: Timestamp,
    pub answer_controls: AnswerControls,
    pub form_action: &'a str,
    pub file_url_prefix: &'a str,
    pub completion_action: CompletionAction,
}

/// A styled error page with a 500 status and a link back to the session.
pub(crate) fn error_response(e: &ErrorReport) -> (StatusCode, Html<String>) {
    let html = page_template(html! {
        div.error {
            h1 { "Error" }
            p { (e) }
            p { a href="/" { "\u{2190} Back to session" } }
        }
    });
    (StatusCode::INTERNAL_SERVER_ERROR, Html(html.into_string()))
}

pub async fn get_handler(
    State(state): State<ServerState>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    match inner(state, flash).await {
        Ok(html) => (StatusCode::OK, Html(html.into_string())),
        Err(e) => error_response(&e),
    }
}

async fn inner(state: ServerState, flash: Option<Flash>) -> Fallible<Markup> {
    let mut mutable = state.mutable.lock();
    // BUG-14: start the per-card timer when the card is served.
    mutable.mark_card_shown();
    let file_url_prefix = format!("http://localhost:{}/file", state.port);
    let ctx = RenderContext {
        directory: &state.directory,
        total_cards: state.total_cards,
        session_started_at: state.session_started_at,
        answer_controls: state.answer_controls,
        form_action: "/",
        file_url_prefix: &file_url_prefix,
        completion_action: CompletionAction::Shutdown,
    };
    let body = if mutable.finished_at.is_some() {
        render_completion_page(&ctx, &mutable)?
    } else {
        render_session_page(&ctx, &mutable)?
    };
    let body = html! {
        @if let Some(f) = &flash { (f.render()) }
        (body)
    };
    let html = page_template(body);
    Ok(html)
}

/// Human-readable progress: "N of M", plus "(+k repeats)" when cards
/// re-queued by Forgot/Hard have come around again.
pub fn progress_text(first_graded: usize, total_cards: usize, repeats: usize) -> String {
    if repeats > 0 {
        let noun = if repeats == 1 { "repeat" } else { "repeats" };
        format!("{first_graded} of {total_cards} (+{repeats} {noun})")
    } else {
        format!("{first_graded} of {total_cards}")
    }
}

pub fn render_session_page(ctx: &RenderContext, mutable: &MutableState) -> Fallible<Markup> {
    let undo_disabled = mutable.reviews.is_empty();
    let total_cards = ctx.total_cards;
    let (cards_done, repeats) = mutable.progress();
    let percent_done = (cards_done * 100).checked_div(total_cards).unwrap_or(100);
    let progress_bar_style = format!("width: {}%;", percent_done);
    let progress_label = progress_text(cards_done, total_cards, repeats);
    let card: Card = match mutable.cards.first() {
        Some(card) => card.clone(),
        None => {
            return fail("No cards are left in the queue. The session may already be finished.");
        }
    };
    let is_bookmarked = mutable.db.bookmark_exists(card.hash())?;
    let coll_path = ctx.directory.to_path_buf();
    let deck_path = card.relative_file_path(&coll_path)?;
    let config = MarkdownRenderConfig {
        resolver: MediaResolverBuilder::new()
            .with_collection_path(coll_path)?
            .with_deck_path(deck_path)?
            .build()?,
        file_url_prefix: ctx.file_url_prefix.to_string(),
    };
    let card_content = render_card(&card, mutable.reveal, &config)?;
    let form_action = ctx.form_action;
    let card_controls = if mutable.reveal {
        let grades = match ctx.answer_controls {
            AnswerControls::Binary => html! {
                input id="forgot" type="submit" name="action" value="Forgot" title="Mark card as forgotten.";
                input id="good" type="submit" name="action" value="Good" title="Mark card as remembered.";
            },
            AnswerControls::Full => html! {
                input id="forgot" type="submit" name="action" value="Forgot" title="Mark card as forgotten. Shortcut: 1.";
                input id="hard" type="submit" name="action" value="Hard" title="Mark card as difficult. Shortcut: 2.";
                input id="good" type="submit" name="action" value="Good" title="Mark card as remembered well. Shortcut: 3.";
                input id="easy" type="submit" name="action" value="Easy" title="Mark card as very easy. Shortcut: 4.";
            },
        };
        html! {
            form action=(form_action) method="post" {
                input type="hidden" name="card" value=(card.hash().to_hex());
                (undo_button(undo_disabled))
                (bookmark_button(is_bookmarked))
                div.spacer {}
                div.grades {
                    (grades)
                }
                div.spacer {}
                (end_button())
            }
        }
    } else {
        html! {
            form action=(form_action) method="post" {
                (undo_button(undo_disabled))
                (bookmark_button(is_bookmarked))
                div.spacer {}
                input id="reveal" type="submit" name="action" value="Reveal" title="Show the answer. Shortcut: space.";
                div.spacer {}
                (end_button())
            }
        }
    };
    let html = html! {
        div.root {
            div.header {
                div.progress {
                    div.progress-text { (progress_label) }
                    div.progress-bar
                        role="progressbar"
                        aria-label="Study progress"
                        aria-valuenow=(percent_done)
                        aria-valuemin="0"
                        aria-valuemax="100"
                    {
                        div.progress-fill style=(progress_bar_style) {}
                    }
                }
            }
            div.card-container {
                div.card {
                    div.card-header {
                        h1 {
                            (card.deck_name())
                        }
                    }
                    (card_content)
                }
            }
            div.controls {
                (card_controls)
            }
        }
    };
    Ok(html)
}

fn render_card(card: &Card, reveal: bool, config: &MarkdownRenderConfig) -> Fallible<Markup> {
    let html = match card.card_type() {
        CardType::Basic => {
            if reveal {
                html! {
                    div .question .rich-text {
                        (card.html_front(config)?)
                    }
                    div .answer .rich-text {
                        (card.html_back(config)?)
                    }
                }
            } else {
                html! {
                    div .question .rich-text {
                        (card.html_front(config)?)
                    }
                    div .answer .rich-text {}
                }
            }
        }
        CardType::Cloze => {
            if reveal {
                html! {
                    div .prompt .rich-text {
                        (card.html_back(config)?)
                    }
                }
            } else {
                html! {
                    div .prompt .rich-text {
                        (card.html_front(config)?)
                    }
                }
            }
        }
    };
    Ok(html! {
        div.card-content {
            (html)
        }
    })
}

const TS_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

const REDIRECT_SCRIPT: &str = r#"
(function() {
    var secs = 5;
    var el = document.getElementById('countdown');
    var timer = setInterval(function() {
        secs--;
        if (el) el.textContent = secs;
        if (secs <= 0) {
            clearInterval(timer);
            var form = document.getElementById('home-form');
            if (form) form.submit();
        }
    }, 1000);
    var cancel = document.getElementById('cancel-redirect');
    if (cancel) {
        cancel.addEventListener('click', function(e) {
            e.preventDefault();
            clearInterval(timer);
            var notice = document.querySelector('.redirect-notice');
            if (notice) notice.style.display = 'none';
        });
    }
})();
"#;

pub fn render_completion_page(ctx: &RenderContext, mutable: &MutableState) -> Fallible<Markup> {
    if mutable.reviews.is_empty() {
        return Ok(render_empty_completion_page(ctx));
    }
    let total_cards = ctx.total_cards;
    let start = ctx.session_started_at.into_inner();
    let finished_at = match mutable.finished_at {
        Some(finished_at) => finished_at,
        None => return fail("The completion page was requested before the session finished."),
    };
    let end = finished_at.into_inner();
    let duration_s = (end - start).num_seconds();

    // BUG-13: all stats come from the session's persisted, non-voided reviews
    // so the numbers agree with the database. Repeats count toward total
    // reviews but not toward distinct cards.
    let rows: Vec<ReviewRow> = mutable.db.get_reviews_for_session(mutable.session_id)?;
    let total_reviews = rows.len();
    let distinct_cards = rows
        .iter()
        .map(|r| r.data.card_hash)
        .collect::<HashSet<CardHash>>()
        .len();

    // Map card hashes to cards so the slowest card can show its question.
    // Every non-voided review of this session has a live entry in
    // mutable.reviews (undo pops the entry it voids), so this covers all rows.
    let cards_by_hash: HashMap<CardHash, &Card> = mutable
        .reviews
        .iter()
        .map(|r| (r.card.hash(), &r.card))
        .collect();

    let mut durations_ms: Vec<i64> = rows.iter().filter_map(|r| r.data.duration_ms).collect();
    durations_ms.sort_unstable();

    let median_pace_s: Option<f64> = if durations_ms.is_empty() {
        None
    } else {
        let n = durations_ms.len();
        let median_ms = if n % 2 == 1 {
            durations_ms[n / 2] as f64
        } else {
            (durations_ms[n / 2 - 1] as f64 + durations_ms[n / 2] as f64) / 2.0
        };
        Some(median_ms / 1000.0)
    };

    let slowest_card: Option<(String, f64)> = rows
        .iter()
        .filter_map(|r| r.data.duration_ms.map(|ms| (r.data.card_hash, ms)))
        .max_by_key(|&(_, ms)| ms)
        .map(|(hash, ms)| {
            let label = match cards_by_hash.get(&hash) {
                Some(card) => card.preview(),
                None => hash.to_string(),
            };
            (label, ms as f64 / 1000.0)
        });

    let pace_rounded = median_pace_s
        .unwrap_or_else(|| {
            if total_reviews == 0 {
                0.0
            } else {
                duration_s as f64 / total_reviews as f64
            }
        })
        .round() as i64;

    let start_ts = start.format(TS_FORMAT).to_string();
    let end_ts = end.format(TS_FORMAT).to_string();
    let duration_min = duration_s / 60;
    let duration_display = if duration_min >= 1 {
        format!("{duration_min} min")
    } else {
        format!("{duration_s} s")
    };
    let summary_line = format!(
        "Done — {distinct_cards} card{} in {duration_display} ({pace_rounded} s/card).",
        if distinct_cards == 1 { "" } else { "s" }
    );

    let (action_button, redirect_notice) = completion_actions(ctx);

    let html = html! {
        div.finished {
            h1 { "Session Completed" }
            div.summary { (summary_line) }
            (redirect_notice)
            details {
                summary { "Session Stats" }
                div.stats {
                    table {
                        tbody {
                            tr {
                                td .key { "Total Cards" }
                                td .val { (total_cards) }
                            }
                            tr {
                                td .key { "Cards Reviewed" }
                                td .val { (distinct_cards) }
                            }
                            tr {
                                td .key { "Total Reviews" }
                                td .val { (total_reviews) }
                            }
                            tr {
                                td .key { "Started" }
                                td .val { (start_ts) }
                            }
                            tr {
                                td .key { "Finished" }
                                td .val { (end_ts) }
                            }
                            tr {
                                td .key { "Duration (seconds)" }
                                td .val { (duration_s) }
                            }
                            @if let Some(median_s) = median_pace_s {
                                tr {
                                    td .key { "Median pace (s/card)" }
                                    td .val { (format!("{:.1}", median_s)) }
                                }
                            }
                            @if let Some((preview, slowest_s)) = slowest_card {
                                tr {
                                    td .key { "Slowest card" }
                                    td .val { (format!("{preview} ({slowest_s:.1} s)")) }
                                }
                            }
                        }
                    }
                }
            }
            (action_button)
        }
    };
    Ok(html)
}

/// The action button (Shutdown or Home) and the auto-redirect notice for the
/// completion page, depending on drill vs. serve mode.
fn completion_actions(ctx: &RenderContext) -> (Markup, Markup) {
    match &ctx.completion_action {
        CompletionAction::Shutdown => (
            html! {
                div.shutdown-container {
                    form action=(ctx.form_action) method="post" {
                        input #shutdown .shutdown-button.btn.btn-danger type="submit" name="action" value="Shutdown" title="Shut down the server";
                    }
                }
            },
            html! {},
        ),
        CompletionAction::BackToCollections => (
            html! {
                div.shutdown-container {
                    form #home-form action=(ctx.form_action) method="post" style="display:inline" {
                        input type="hidden" name="action" value="Home";
                        button #home .home-button.btn.btn-primary type="submit" { "Home" }
                    }
                }
            },
            html! {
                p.redirect-notice {
                    "Returning to collections in "
                    span #countdown { "5" }
                    "s. "
                    a #cancel-redirect href="#" { "Cancel" }
                }
                script { (maud::PreEscaped(REDIRECT_SCRIPT)) }
            },
        ),
    }
}

/// Completion page for a session that ended before any card was graded:
/// no stats block, since there is nothing meaningful to report.
fn render_empty_completion_page(ctx: &RenderContext) -> Markup {
    let (action_button, redirect_notice) = completion_actions(ctx);
    html! {
        div.finished {
            h1 { "Session Ended" }
            div.summary { "No cards were reviewed." }
            (redirect_notice)
            (action_button)
        }
    }
}

fn undo_button(disabled: bool) -> Markup {
    if disabled {
        html! {
            input id="undo" type="submit" name="action" value="Undo" disabled;
        }
    } else {
        html! {
            input id="undo" type="submit" name="action" value="Undo" title="Undo last action. Shortcut: u.";
        }
    }
}

fn end_button() -> Markup {
    html! {
        input id="end" type="submit" name="action" value="End" title="End the session (changes are saved)";
    }
}

fn bookmark_button(is_bookmarked: bool) -> Markup {
    if is_bookmarked {
        html! {
            button #bookmark .bookmark-active type="submit" name="action" value="Unbookmark"
                title="Remove bookmark. Shortcut: b." {
                "\u{2605} Bookmarked"
            }
        }
    } else {
        html! {
            button #bookmark type="submit" name="action" value="Bookmark"
                title="Bookmark this card for later editing. Shortcut: b." {
                "\u{2606} Bookmark"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::path::PathBuf;

    use super::*;
    use crate::cmd::drill::cache::Cache;
    use crate::cmd::drill::state::Review;
    use crate::db::Database;
    use crate::db::ReviewRecord;
    use crate::error::ErrorReport;
    use crate::fsrs::Grade;
    use crate::rng::TinyRng;
    use crate::types::card::CardContent;
    use crate::types::performance::Jitter;
    use crate::types::performance::Performance;

    #[test]
    fn test_completion_page_without_reviews_skips_stats() -> Fallible<()> {
        let db = Database::new(":memory:").unwrap();
        let session_id = db.create_session(Timestamp::now()).unwrap();
        let mutable = MutableState {
            reveal: false,
            session_id,
            db,
            cache: Cache::new(),
            cards: Vec::new(),
            reviews: Vec::new(),
            finished_at: Some(Timestamp::now()),
            card_shown_at: None,
            jitter: Jitter::none(),
            rng: TinyRng::from_seed(1),
        };
        let ctx = RenderContext {
            directory: Path::new("."),
            total_cards: 0,
            session_started_at: Timestamp::now(),
            answer_controls: AnswerControls::Full,
            form_action: "/",
            file_url_prefix: "/file",
            completion_action: CompletionAction::Shutdown,
        };
        let html = render_completion_page(&ctx, &mutable)?.into_string();
        assert!(html.contains("No cards were reviewed."), "html: {html}");
        assert!(!html.contains("Session Stats"));
        assert!(!html.contains("s/card"));
        Ok(())
    }

    #[test]
    fn test_error_response_is_styled_500_with_home_link() {
        let (status, Html(body)) = error_response(&ErrorReport::new("kaboom"));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("class=\"error\""));
        assert!(body.contains("kaboom"));
        assert!(body.contains("href=\"/\""), "body: {body}");
    }

    fn make_empty_mutable() -> MutableState {
        let db = Database::new(":memory:").unwrap();
        let session_id = db.create_session(Timestamp::now()).unwrap();
        MutableState::new(
            db,
            session_id,
            Cache::new(),
            Vec::new(),
            Jitter::none(),
            TinyRng::from_seed(1),
        )
    }

    fn make_ctx(directory: &Path) -> RenderContext<'_> {
        RenderContext {
            directory,
            total_cards: 0,
            session_started_at: Timestamp::now(),
            answer_controls: AnswerControls::Full,
            form_action: "/",
            file_url_prefix: "http://localhost:0/file",
            completion_action: CompletionAction::Shutdown,
        }
    }

    #[test]
    fn test_render_session_page_with_empty_queue_is_error() {
        let mutable = make_empty_mutable();
        let ctx = make_ctx(Path::new("."));
        let result = render_session_page(&ctx, &mutable);
        assert!(result.is_err());
    }

    /// A completion page requested with reviews recorded but no finish
    /// timestamp must return an error, not panic. (With no reviews at all the
    /// page short-circuits to the empty-session variant before reaching here.)
    #[test]
    fn test_render_completion_page_without_finished_at_is_error() {
        let mut mutable = make_empty_mutable();
        let card = Card::new(
            "TestDeck".to_string(),
            std::path::PathBuf::from("/tmp/test-deck.md"),
            (1, 2),
            crate::types::card::CardContent::new_basic("q", "a"),
        );
        mutable.reviews.push(Review {
            card,
            review_id: 1,
            grade: crate::fsrs::Grade::Good,
            prev_performance: Performance::New,
        });
        assert!(mutable.finished_at.is_none());
        let ctx = make_ctx(Path::new("."));
        let result = render_completion_page(&ctx, &mutable);
        assert!(result.is_err());
    }

    /// FEAT-08: "N of M" text, with "(+k repeats)" only when repeats exist.
    #[test]
    fn test_progress_text_formats() {
        assert_eq!(progress_text(0, 20, 0), "0 of 20");
        assert_eq!(progress_text(7, 20, 0), "7 of 20");
        assert_eq!(progress_text(7, 20, 1), "7 of 20 (+1 repeat)");
        assert_eq!(progress_text(7, 20, 3), "7 of 20 (+3 repeats)");
        assert_eq!(progress_text(20, 20, 2), "20 of 20 (+2 repeats)");
    }

    fn make_card(deck: &str, question: &str) -> Card {
        Card::new(
            deck.to_string(),
            PathBuf::from("/tmp/deck.md"),
            (1, 2),
            CardContent::new_basic(question, "answer"),
        )
    }

    fn record(card: &Card, now: Timestamp, duration_ms: i64) -> ReviewRecord {
        ReviewRecord {
            card_hash: card.hash(),
            reviewed_at: now,
            grade: Grade::Good,
            stability: 2.0,
            difficulty: 2.0,
            interval_raw: 1.0,
            interval_days: 1,
            due_date: now.date(),
            duration_ms: Some(duration_ms),
        }
    }

    fn reviewed(now: Timestamp) -> Performance {
        Performance::Reviewed(crate::types::performance::ReviewedPerformance {
            last_reviewed_at: now,
            stability: 2.0,
            difficulty: 2.0,
            interval_raw: 1.0,
            interval_days: 1,
            due_date: now.date(),
            review_count: 1,
        })
    }

    /// BUG-13: all completion stats come from the session's non-voided DB
    /// reviews; distinct cards and total reviews are labeled separately; the
    /// slowest card shows the question preview, not the deck name.
    #[test]
    fn test_completion_stats_from_db_reviews() -> Fallible<()> {
        let mut db = Database::new(":memory:")?;
        let now = Timestamp::now();
        let slow = make_card("SlowDeck", "What is the slowest question?");
        let fast = make_card("FastDeck", "Fast question?");
        db.insert_card(slow.hash(), now)?;
        db.insert_card(fast.hash(), now)?;
        let session_id = db.create_session(now)?;

        // Three live reviews on two distinct cards (slow card repeats).
        let id1 = db.insert_review_and_update_performance(
            session_id,
            &record(&slow, now, 1000),
            reviewed(now),
        )?;
        let id2 = db.insert_review_and_update_performance(
            session_id,
            &record(&fast, now, 2000),
            reviewed(now),
        )?;
        let id3 = db.insert_review_and_update_performance(
            session_id,
            &record(&slow, now, 9000),
            reviewed(now),
        )?;
        // A voided (undone) review must not count anywhere.
        let id4 = db.insert_review_and_update_performance(
            session_id,
            &record(&slow, now, 50000),
            reviewed(now),
        )?;
        db.void_review_and_restore_performance(id4, slow.hash(), reviewed(now), None)?;
        db.close_session(session_id, now)?;

        let reviews = vec![
            Review {
                card: slow.clone(),
                review_id: id1,
                grade: Grade::Good,
                prev_performance: Performance::New,
            },
            Review {
                card: fast.clone(),
                review_id: id2,
                grade: Grade::Good,
                prev_performance: Performance::New,
            },
            Review {
                card: slow.clone(),
                review_id: id3,
                grade: Grade::Good,
                prev_performance: Performance::New,
            },
        ];
        let mut mutable = MutableState::new(
            db,
            session_id,
            Cache::new(),
            Vec::new(),
            Jitter::none(),
            TinyRng::from_seed(1),
        );
        mutable.reviews = reviews;
        mutable.finished_at = Some(now);
        let ctx = RenderContext {
            directory: std::path::Path::new("/tmp"),
            total_cards: 2,
            session_started_at: now,
            answer_controls: AnswerControls::Full,
            form_action: "/",
            file_url_prefix: "/file",
            completion_action: CompletionAction::Shutdown,
        };
        let html = render_completion_page(&ctx, &mutable)?.into_string();

        // Distinct cards vs. total reviews, separately labeled.
        assert!(
            html.contains("Cards Reviewed"),
            "missing distinct-cards row: {html}"
        );
        assert!(
            html.contains("Total Reviews"),
            "missing total-reviews row: {html}"
        );
        assert!(
            html.contains("Done — 2 cards"),
            "summary must count distinct cards: {html}"
        );
        // Slowest card: question preview, not deck name; voided 50s review excluded.
        assert!(
            html.contains("What is the slowest question? (9.0 s)"),
            "slowest must show preview: {html}"
        );
        assert!(
            !html.contains("SlowDeck (9.0 s)"),
            "slowest must not show deck name: {html}"
        );
        // Median of [1000, 2000, 9000] = 2.0 s.
        assert!(html.contains("2.0"), "median pace must be 2.0 s: {html}");
        Ok(())
    }
}
