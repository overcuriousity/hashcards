# Data Integrity (BUG-01, -02, -04, -06, -15) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make drill/serve session state crash-proof: no session is ever dropped by a request error, no in-memory mutation happens before its DB transaction commits, undo reopens finished sessions, and stale/repeated grade submissions are harmless no-ops.

**Architecture:** All five bugs live in the shared drill action core (`src/cmd/drill/post.rs::handle_action`) and the serve-mode session map (`src/cmd/serve/handlers.rs` + `src/cmd/serve/state.rs`). The fixes reorder each action to be "DB transaction first, memory second", replace the take-out/put-back session pattern with `Arc<Mutex<DrillSession>>` map entries, and add an `ActionResult::Ignored` variant that surfaces no-op grades through the flash-message infrastructure from plan 01.

**Tech Stack:** Rust, axum 0.8, maud, rusqlite, tokio; tests use the built-in test harness plus reqwest/portpicker/tempfile for the HTTP integration test.

**Spec:** SPEC.md

**Depends on:** Plan 01 (flash-messages) must be merged first. It provides `src/flash.rs` with this exact API, which this plan consumes and MUST NOT re-implement:

```rust
// src/flash.rs
pub enum FlashKind { Success, Error }
pub struct Flash { pub kind: FlashKind, pub message: String }
impl Flash {
    pub fn success(message: impl Into<String>) -> Self;
    pub fn error(message: impl Into<String>) -> Self;
    pub fn redirect(self, to: &str) -> axum::response::Redirect;
    pub fn from_query(query: &std::collections::HashMap<String, String>) -> Option<Flash>;
    pub fn render(&self) -> maud::Markup;
}
```

## Global Constraints

Copied verbatim from SPEC.md "Global requirements" (apply to every item below, from `CLAUDE.md`):

- Every bugfix starts with a failing regression test.
- No `unwrap()` in production code; use `Fallible`, `?`, and `fail()`.
- All error messages are user-facing and clear.
- Reviews and performance are written in one transaction; undo voids, never deletes.
- Cloze positions are byte positions: `.bytes()`, never `.chars()`.
- Update `CHANGELOG.xml` per item.

Additional project rules that bind this plan:

- Prefer imports over fully qualified names (add `use` statements at the top of the module).
- Dates/timestamps are naive; never introduce timezones.
- Existing `.lock().unwrap()` calls are an acknowledged project-wide violation swept separately by BUG-50 (plan 07, panic-hygiene). New locking code in this plan follows the existing `.lock().unwrap()` convention so plan 07 can convert everything mechanically in one pass. Do not "fix" lock unwraps here.
- Branch: `fix/data-integrity`. PR target: `overcuriousity/hashcards-web` (pass `--repo` explicitly). PR description references "Fixes BUG-01, BUG-02, BUG-04, BUG-06, BUG-15".
- `CHANGELOG.xml` entries go inside `<changelog><unreleased><fixed>`, as `<change author="mstoeck3">...</change>`, inserted directly after the opening `<fixed>` line. The format is validated by `CHANGELOG.xsd` (change elements are plain strings with an optional `author` attribute).

---

## File Structure

- `src/cmd/drill/post.rs` — action core: grade/undo reordering (BUG-02), session reopen on undo (BUG-04), `ActionResult::Ignored` + stale-hash / no-reveal no-ops (BUG-06, BUG-15). All unit regression tests for those bugs live in its `tests` module.
- `src/cmd/drill/state.rs` — unchanged struct definitions (referenced only).
- `src/cmd/drill/get.rs` — hidden `card` field in the grade form (BUG-06).
- `src/cmd/drill/script.js` — `event.repeat` guard (BUG-06). Served by both drill and serve modes via `include_str!`.
- `src/db.rs` — `void_review_and_restore_performance` gains a `reopen_session` parameter (BUG-04).
- `src/cmd/serve/state.rs` — `sessions` map holds `Arc<Mutex<DrillSession>>` (BUG-01).
- `src/cmd/serve/handlers.rs` — per-slug locking instead of take-out/put-back; `Ignored` → flash redirect (BUG-01, -06, -15).
- `src/cmd/serve/mod.rs` — HTTP integration regression test for BUG-01.
- `CHANGELOG.xml` — one entry per bug.

Execute tasks strictly in order: Task 5 introduces `ActionResult::Ignored` used by Task 6; Task 6 changes the `handle_action` signature that Task 7's handler code uses.

---

### Task 1: Preflight — verify the flash dependency

**Files:**
- Read: `src/flash.rs`, `src/main.rs`

**Interfaces:**
- Consumes: `src/flash.rs` from plan 01 (flash-messages).
- Produces: confidence that `crate::flash::Flash::{error, redirect}` exist for Tasks 5–7.

- [ ] **Step 1: Verify plan 01 landed**

Run:
```bash
test -f src/flash.rs && grep -n "pub fn error" src/flash.rs && grep -n "pub fn redirect" src/flash.rs && grep -n "mod flash" src/main.rs
```
Expected: all three greps print matches.

**If any of these fail, STOP.** This plan depends on plan 01 (`docs/superpowers/plans/` flash-messages plan). Report the missing dependency to the person driving execution; do not write a substitute `flash.rs`.

- [ ] **Step 2: Verify a clean baseline**

Run: `cargo test`
Expected: all existing tests PASS. If the baseline is broken, stop and report.

- [ ] **Step 3: Create the working branch**

```bash
git checkout -b fix/data-integrity
```

---

### Task 2: BUG-02 (grade path) — DB commit before any in-memory mutation

The grade arm of `handle_action` (`src/cmd/drill/post.rs:171-222`) currently does `mutable.cards.remove(0)` at line 180 and clears `card_shown_at` at line 179 *before* the DB write at line 200. A DB failure makes the card silently vanish: never persisted, not undoable.

**Files:**
- Modify: `src/cmd/drill/post.rs:171-222` (grade arm) and its `tests` module (`post.rs:234-299`)
- Modify: `CHANGELOG.xml` (in Task 3, one entry covering both BUG-02 paths)

**Interfaces:**
- Consumes: `Database::insert_review_and_update_performance(&mut self, session_id: i64, review: &ReviewRecord, performance: Performance) -> Fallible<i64>` (already transactional, `src/db.rs:259`).
- Produces: test helpers `make_card(question: &str) -> Card` and `make_state_with_cards(cards: Vec<Card>) -> MutableState` in `post.rs`'s `tests` module, reused by Tasks 3–6. `handle_action` signature is unchanged in this task: `pub fn handle_action(mutable: &mut MutableState, session_started_at: Timestamp, action: Action) -> Fallible<ActionResult>`.

- [ ] **Step 1: Add test helpers and the failing regression test**

In the `tests` module of `src/cmd/drill/post.rs`, extend the imports and add helpers plus the test. The injection technique: the card is in the queue and cache but *not* in the `cards` DB table, so the review insert violates its foreign key (`Database::new` enables FK enforcement) — a deterministic in-transaction DB failure.

```rust
// add to the existing `use` block in `mod tests`:
use std::path::PathBuf;
use crate::types::card::CardContent;
use crate::types::performance::Performance;

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
    assert_eq!(mutable.cards[0].hash(), card.hash(), "card must still be at the head");
    assert!(mutable.reviews.is_empty(), "no review must be recorded in memory");
    assert!(mutable.reveal, "reveal must stay set so the grade can be retried");
    assert!(mutable.card_shown_at.is_some(), "timing info must not be cleared");
    assert!(matches!(mutable.cache.get(card.hash()).unwrap(), Performance::New));
    assert!(
        mutable.db.get_reviews_for_session(mutable.session_id).unwrap().is_empty(),
        "no review row must exist"
    );
}
```

- [ ] **Step 2: Run the test and see it fail**

Run: `cargo test test_grade_db_failure_leaves_state_unchanged`
Expected: FAIL on `assert_eq!(mutable.cards.len(), 1)` — pre-fix code removed the card before the DB write.

- [ ] **Step 3: Reorder the grade arm — DB first, memory second**

Replace the entire `Action::Forgot | Action::Hard | Action::Good | Action::Easy` arm in `handle_action` (`src/cmd/drill/post.rs:171-222`) with:

```rust
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
                let review_id = mutable.db.insert_review_and_update_performance(mutable.session_id, &record, new_performance)?;
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
```

Note: `mutable.cards.remove(0)` after the head clone is safe (the queue was verified non-empty via `first()`), and the pre-existing `card.clone()` for the review is kept because the card may also be pushed to the back of the queue.

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test`
Expected: `test_grade_db_failure_leaves_state_unchanged` PASSES and all pre-existing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add src/cmd/drill/post.rs
git commit -m "fix: commit grade transaction before mutating session state (BUG-02)"
```

---

### Task 3: BUG-02 (undo path) — void the review before touching queue/cache

The `Action::Undo` arm (`src/cmd/drill/post.rs:121-139`) pops the review and re-inserts the card (lines 123-130) *before* `void_review_and_restore_performance` (line 132). A DB failure leaves cache and DB diverged and can duplicate the card in the queue.

**Files:**
- Modify: `src/cmd/drill/post.rs:121-139` (undo arm) and its `tests` module
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: helpers `make_card` / `make_state_with_cards` from Task 2; `Database::void_review_and_restore_performance(&mut self, review_id: i64, card_hash: CardHash, prev_performance: Performance) -> Fallible<()>` (`src/db.rs:294`, three-argument form — Task 4 extends it).
- Produces: undo arm shape that Task 4 builds on.

- [ ] **Step 1: Write the failing regression test**

Add to the `tests` module of `src/cmd/drill/post.rs`:

```rust
#[test]
fn test_undo_db_failure_leaves_state_unchanged() {
    let card_a = make_card("QA");
    let card_b = make_card("QB");
    let mut mutable = make_state_with_cards(vec![card_a.clone(), card_b.clone()]);
    let started = Timestamp::now();
    handle_action(&mut mutable, started, Action::Reveal).unwrap();
    handle_action(&mut mutable, started, Action::Good).unwrap();
    assert_eq!(mutable.cards.len(), 1);
    assert_eq!(mutable.reviews.len(), 1);
    // Inject a DB failure into the undo: point the recorded review at a
    // nonexistent row, so the void update matches zero rows and errors.
    mutable.reviews[0].review_id = 999_999;
    let result = handle_action(&mut mutable, started, Action::Undo);
    assert!(result.is_err(), "the injected DB failure must propagate");
    // On error, in-memory state must be completely unchanged: no duplicate
    // card in the queue, undo stack intact.
    assert_eq!(mutable.cards.len(), 1, "queue must be unchanged");
    assert_eq!(mutable.cards[0].hash(), card_b.hash(), "head card must be unchanged");
    assert_eq!(mutable.reviews.len(), 1, "undo stack must be unchanged");
    assert!(mutable.finished_at.is_none());
}
```

- [ ] **Step 2: Run the test and see it fail**

Run: `cargo test test_undo_db_failure_leaves_state_unchanged`
Expected: FAIL on `assert_eq!(mutable.cards.len(), 1)` — pre-fix code re-inserted the card before the DB call, leaving two cards in the queue.

- [ ] **Step 3: Reorder the undo arm — DB first, memory second**

Replace the entire `Action::Undo` arm in `handle_action` (`src/cmd/drill/post.rs:121-139`) with:

```rust
        Action::Undo => {
            let Some(last_review) = mutable.reviews.last().cloned() else {
                return Ok(ActionResult::Continue);
            };
            let hash: CardHash = last_review.card.hash();
            // Void the review and restore prior performance atomically, and
            // commit BEFORE mutating any in-memory state. If this fails, the
            // queue, cache, and undo stack are untouched.
            mutable.db.void_review_and_restore_performance(last_review.review_id, hash, last_review.prev_performance)?;
            // The transaction committed; it is now safe to mutate memory.
            mutable.reviews.pop();
            if last_review.should_repeat() {
                // Remove the card from the back of the queue.
                mutable.cards.pop();
            }
            mutable.cards.insert(0, last_review.card);
            mutable.cache.update(hash, last_review.prev_performance)?;
            mutable.finished_at = None;
            mutable.reveal = false;
            mutable.card_shown_at = None;
            Ok(ActionResult::Continue)
        }
```

This also removes the `mutable.reviews.pop().unwrap()` at `post.rs:123` (a BUG-49 item, resolved here for free by the `let ... else` on `last().cloned()`).

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test`
Expected: `test_undo_db_failure_leaves_state_unchanged` PASSES; `test_grade_db_failure_leaves_state_unchanged` and all pre-existing tests still pass.

- [ ] **Step 5: Update CHANGELOG.xml (BUG-02)**

Insert directly after the `<fixed>` line inside `<unreleased>` in `CHANGELOG.xml`:

```xml
            <change author="mstoeck3">
                Grading and undoing now commit to the database before changing the in-memory session. Previously a failed database write during a grade silently discarded the card (unrecorded and not undoable), and a failed write during undo could duplicate the card in the queue.
            </change>
```

- [ ] **Step 6: Commit**

```bash
git add src/cmd/drill/post.rs CHANGELOG.xml
git commit -m "fix: commit undo transaction before mutating session state (BUG-02)"
```

---

### Task 4: BUG-04 — undo of a finished session reopens the DB session row

`finish_session` closes the row (`post.rs:229` calls `close_session`); a later Undo resets `finished_at = None` in memory but the DB row keeps its close time while new reviews continue to attach to it. The session row must be reopened in the same transaction as the void.

Note on "null `ended_at`": the spec says to null the column, but `sessions.ended_at` is `text not null` in a strict table (`src/schema.sql`) and the codebase's documented open-session convention is `ended_at == started_at` (placeholder set by `create_session`, `src/db.rs:245-251`). Reopening therefore resets `ended_at = started_at`, avoiding a schema migration (schema/migration work belongs to BUG-32, plan 08). See "Spec discrepancies".

**Files:**
- Modify: `src/db.rs:294-310` (`void_review_and_restore_performance`) and its two test call sites (`src/db.rs` tests `test_void_review_and_restore_performance` at ~line 815 and `test_void_review_wrong_card_is_rejected` at ~line 857)
- Modify: `src/cmd/drill/post.rs` (undo arm call site) and its `tests` module
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: Task 3's undo arm.
- Produces: `Database::void_review_and_restore_performance(&mut self, review_id: i64, card_hash: CardHash, prev_performance: Performance, reopen_session: Option<i64>) -> Fallible<()>` — the four-argument form used from here on.

- [ ] **Step 1: Write the failing regression test**

Add to the `tests` module of `src/cmd/drill/post.rs`. The session is created with a fixed past `started_at` so the close time provably differs from it:

```rust
// add to the existing `use` block in `mod tests`:
use chrono::NaiveDateTime;

#[test]
fn test_undo_after_finish_reopens_session_row() {
    let card = make_card("Q1");
    let db = Database::new(":memory:").unwrap();
    let started_at = Timestamp::new(
        NaiveDateTime::parse_from_str("2020-01-01T00:00:00.000", "%Y-%m-%dT%H:%M:%S%.3f")
            .unwrap(),
    );
    db.insert_card(card.hash(), started_at).unwrap();
    let session_id = db.create_session(started_at).unwrap();
    let mut cache = Cache::new();
    cache.insert(card.hash(), Performance::New).unwrap();
    let mut mutable = MutableState {
        reveal: false,
        db,
        session_id,
        cache,
        cards: vec![card],
        reviews: Vec::new(),
        finished_at: None,
        card_shown_at: None,
    };
    // Grade the only card: the session finishes and the DB row is closed.
    handle_action(&mut mutable, started_at, Action::Reveal).unwrap();
    let result = handle_action(&mut mutable, started_at, Action::Good).unwrap();
    assert!(matches!(result, ActionResult::SessionFinished));
    assert!(mutable.finished_at.is_some());
    // Undo must reopen the DB session row, not just the in-memory flag.
    handle_action(&mut mutable, started_at, Action::Undo).unwrap();
    assert!(mutable.finished_at.is_none());
    let sessions = mutable.db.get_all_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].ended_at, sessions[0].started_at,
        "undo of a finished session must reopen the session row"
    );
}
```

- [ ] **Step 2: Run the test and see it fail**

Run: `cargo test test_undo_after_finish_reopens_session_row`
Expected: FAIL on the `ended_at == started_at` assertion — pre-fix, `ended_at` keeps the (2026) close time while `started_at` is the fixed 2020 value.

- [ ] **Step 3: Extend the void transaction to optionally reopen the session**

Replace `void_review_and_restore_performance` in `src/db.rs` (lines 290-310, keeping its doc comment style) with:

```rust
    /// Atomically void a review and restore prior card performance (undo).
    ///
    /// When `reopen_session` is `Some(session_id)`, the session row is
    /// reopened in the same transaction (its `ended_at` is reset to
    /// `started_at`, the placeholder used for open sessions), so undoing
    /// past a finished session cannot leave a closed row accumulating
    /// new reviews.
    ///
    /// All operations run inside a single transaction so a crash between
    /// them cannot leave the DB in an inconsistent state.
    pub fn void_review_and_restore_performance(
        &mut self,
        review_id: i64,
        card_hash: CardHash,
        prev_performance: Performance,
        reopen_session: Option<i64>,
    ) -> Fallible<()> {
        let tx = self.conn.transaction()?;
        let rows = tx.execute("update reviews set voided = 1 where review_id = ? and card_hash = ?;", params![review_id, card_hash])?;
        if rows != 1 {
            return fail("review not found or does not belong to this card");
        }
        update_card_performance_tx(&tx, card_hash, prev_performance)?;
        if let Some(session_id) = reopen_session {
            let rows = tx.execute(
                "update sessions set ended_at = started_at where session_id = ?;",
                params![session_id],
            )?;
            if rows != 1 {
                return fail(format!("No session with ID {session_id} to reopen"));
            }
        }
        tx.commit()?;
        Ok(())
    }
```

Update the two existing test call sites in `src/db.rs`'s `tests` module (in `test_void_review_and_restore_performance` and `test_void_review_wrong_card_is_rejected`) to pass `None` as the new fourth argument.

- [ ] **Step 4: Reopen from the undo arm**

In the `Action::Undo` arm of `handle_action` (as written in Task 3), replace the two lines

```rust
            let hash: CardHash = last_review.card.hash();
            mutable.db.void_review_and_restore_performance(last_review.review_id, hash, last_review.prev_performance)?;
```

with

```rust
            let hash: CardHash = last_review.card.hash();
            // If the session had finished, its DB row was closed; reopen it
            // in the same transaction as the void (BUG-04).
            let reopen_session: Option<i64> = mutable.finished_at.map(|_| mutable.session_id);
            mutable.db.void_review_and_restore_performance(last_review.review_id, hash, last_review.prev_performance, reopen_session)?;
```

(`finish_session` already sets `ended_at` again on the next End/last-grade via `close_session`, so re-finishing works unchanged.)

- [ ] **Step 5: Run the tests and see them pass**

Run: `cargo test`
Expected: `test_undo_after_finish_reopens_session_row` PASSES; the updated `src/db.rs` tests and all others still pass.

- [ ] **Step 6: Update CHANGELOG.xml (BUG-04)**

Insert directly after the `<fixed>` line inside `<unreleased>` in `CHANGELOG.xml`:

```xml
            <change author="mstoeck3">
                Undoing after a session has finished now reopens the session's database row, so reviews made after the undo are attached to an open session instead of one that still claims to be closed.
            </change>
```

- [ ] **Step 7: Commit**

```bash
git add src/db.rs src/cmd/drill/post.rs CHANGELOG.xml
git commit -m "fix: reopen session row when undoing past a finished session (BUG-04)"
```

---

### Task 5: BUG-15 — grade without reveal logs at debug and redirects with a flash

`post.rs:171-172`: the grade arm's `if mutable.reveal` has no else branch and no log; a grade POST from a stale page is silently ignored. Introduce `ActionResult::Ignored(String)` and surface it as a flash-message redirect in both the drill and serve POST handlers.

**Files:**
- Modify: `src/cmd/drill/post.rs` (`ActionResult`, grade arm, `post_handler`, `action_handler`, tests)
- Modify: `src/cmd/serve/handlers.rs:342-400` (`collection_post_handler` / `collection_post_inner`)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: `crate::flash::Flash` (plan 01); Task 2's grade arm.
- Produces: `ActionResult::Ignored(String)` — "the action was a harmless no-op; show `String` to the user as an error flash". Task 6 returns it for stale-hash grades. Drill-mode `action_handler(state: ServerState, form: FormData) -> Fallible<Option<Flash>>`.

- [ ] **Step 1: Write the failing regression test**

Add to the `tests` module of `src/cmd/drill/post.rs`:

```rust
#[test]
fn test_grade_without_reveal_is_ignored_with_flash() {
    let card = make_card("Q1");
    let mut mutable = make_state_with_cards(vec![card.clone()]);
    // No Reveal has happened; a grade POST (e.g. from a stale page) arrives.
    let result = handle_action(&mut mutable, Timestamp::now(), Action::Good).unwrap();
    assert!(
        matches!(result, ActionResult::Ignored(_)),
        "a grade without a prior reveal must be reported as ignored"
    );
    assert_eq!(mutable.cards.len(), 1);
    assert_eq!(mutable.cards[0].hash(), card.hash());
    assert!(mutable.db.get_reviews_for_session(mutable.session_id).unwrap().is_empty());
}
```

- [ ] **Step 2: Run the test and see it fail**

Run: `cargo test test_grade_without_reveal_is_ignored_with_flash`
Expected: FAIL — pre-fix `handle_action` returns `ActionResult::Continue` (and `ActionResult::Ignored` does not exist yet, so this step may fail at compile time instead; either counts as the failing state).

- [ ] **Step 3: Add the `Ignored` variant and the else branch**

In `src/cmd/drill/post.rs`, extend `ActionResult`:

```rust
/// Result of handling an action on the drill session.
pub enum ActionResult {
    /// Continue drilling (redirect back to the same page).
    Continue,
    /// The session finished (all cards done or user pressed End).
    SessionFinished,
    /// The user requested server shutdown (drill mode only).
    Shutdown,
    /// The user requested to go back to the collection list (serve mode).
    Home,
    /// The action was a harmless no-op (stale page, double submit); the
    /// message is shown to the user as a flash.
    Ignored(String),
}
```

In the grade arm (Task 2's version), convert the `if mutable.reveal { ... } Ok(ActionResult::Continue)` shape into an early-return guard at the top of the arm, before the `head` lookup:

```rust
        Action::Forgot | Action::Hard | Action::Good | Action::Easy => {
            if !mutable.reveal {
                // A grade arrived without a prior reveal: stale page or
                // duplicate submission. Ignore it (BUG-15).
                log::debug!("ignoring grade action: no card is revealed");
                return Ok(ActionResult::Ignored(
                    "That grade was ignored because no answer was revealed. The current card is shown below.".to_string(),
                ));
            }
            let head: Card = match mutable.cards.first() {
                Some(card) => card.clone(),
                None => return Ok(ActionResult::Continue),
            };
            // ... the rest of the arm is the code ALREADY IN THE FILE at
            // this point (from the previous commit): everything from
            // `let reviewed_at: Timestamp = Timestamp::now();` down to the
            // `if mutable.cards.is_empty()` finish check, now un-nested
            // (one indentation level removed) because the `if mutable.reveal`
            // wrapper is gone ...
            Ok(ActionResult::Continue)
        }
```

(Only the guard shape changes; do not alter any statement in the DB-first body committed in Task 2.)

- [ ] **Step 4: Surface `Ignored` as a flash in the drill server**

In `src/cmd/drill/post.rs`, add `use crate::flash::Flash;` to the imports and replace `post_handler` and `action_handler` (lines 77-105) with:

```rust
pub async fn post_handler(
    State(state): State<ServerState>,
    Form(form): Form<FormData>,
) -> Redirect {
    match action_handler(state, form).await {
        Ok(Some(flash)) => flash.redirect("/"),
        Ok(None) => Redirect::to("/"),
        Err(e) => {
            log::error!("error handling drill action: {e}");
            Redirect::to("/")
        }
    }
}

async fn action_handler(state: ServerState, form: FormData) -> Fallible<Option<Flash>> {
    let mut mutable = state.mutable.lock().unwrap();
    let result = handle_action(&mut mutable, state.session_started_at, form.action)?;
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
        ActionResult::Ignored(reason) => Ok(Some(Flash::error(reason))),
        ActionResult::Continue | ActionResult::SessionFinished | ActionResult::Home => Ok(None),
    }
}
```

(The `Err` branch still only logs and redirects — surfacing handler *errors* is BUG-03, owned by plan 01's group.)

- [ ] **Step 5: Surface `Ignored` as a flash in the serve server**

In `src/cmd/serve/handlers.rs`, add `use crate::flash::Flash;` to the imports and change the tail of `collection_post_inner` (lines 386-399) to capture the result and re-insert the session before the `?`:

```rust
    // Take ownership of the session so we can release the global lock before
    // handle_action does any DB work.
    let mut session = match state.sessions.lock().unwrap().remove(slug) {
        Some(s) => s,
        None => return Ok(Redirect::to(&format!("/collection/{slug}"))),
    };

    // `Action::Home` returned early above, and it is the only action for which
    // `handle_action` yields `ActionResult::Home`. Every action reaching here
    // therefore leaves the session running.
    let result = handle_action(&mut session.mutable, session.session_started_at, action);
    state.sessions.lock().unwrap().insert(slug.to_owned(), session);
    match result? {
        ActionResult::Ignored(reason) => Ok(Flash::error(reason).redirect(&format!("/collection/{slug}"))),
        _ => Ok(Redirect::to(&format!("/collection/{slug}"))),
    }
```

Also add `use crate::cmd::drill::post::ActionResult;` to the imports. (Re-inserting before `?` already narrows BUG-01's POST-side session loss; Task 7 removes take-out/put-back entirely.)

- [ ] **Step 6: Run the tests and see them pass**

Run: `cargo test`
Expected: `test_grade_without_reveal_is_ignored_with_flash` PASSES; all other tests pass.

- [ ] **Step 7: Update CHANGELOG.xml (BUG-15)**

Insert directly after the `<fixed>` line inside `<unreleased>` in `CHANGELOG.xml`:

```xml
            <change author="mstoeck3">
                A grade submitted without a revealed answer (for example from a stale browser page) is no longer silently ignored: the page now shows a notice explaining that the grade was not counted.
            </change>
```

- [ ] **Step 8: Commit**

```bash
git add src/cmd/drill/post.rs src/cmd/serve/handlers.rs CHANGELOG.xml
git commit -m "fix: report ignored grade-without-reveal via flash message (BUG-15)"
```

---

### Task 6: BUG-06 — key auto-repeat / double-submit must not grade multiple cards

`src/cmd/drill/script.js:42`'s keydown handler does not check `event.repeat`, and grade POSTs carry no idempotency token, so a held key or double-tap grades several cards in a row. Fix all three layers: client ignores `event.repeat`; the grade form carries the current card's hash; the server no-ops a grade whose hash does not match the queue head.

**Files:**
- Modify: `src/cmd/drill/post.rs` (`FormData`, `handle_action` signature, `action_handler`, all test call sites)
- Modify: `src/cmd/serve/handlers.rs` (`collection_post_handler` / `collection_post_inner` pass the submitted hash)
- Modify: `src/cmd/drill/get.rs:119-130` (hidden `card` input in the revealed-grades form)
- Modify: `src/cmd/drill/script.js:42-49` (`event.repeat` guard)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: `ActionResult::Ignored(String)` from Task 5; `CardHash::{to_hex, from_hex}` (`src/types/card_hash.rs`).
- Produces: `pub fn handle_action(mutable: &mut MutableState, session_started_at: Timestamp, action: Action, submitted_card: Option<CardHash>) -> Fallible<ActionResult>` — the four-argument form every later task and caller uses. `FormData { pub action: Action, pub card: Option<String> }`.

- [ ] **Step 1: Write the failing regression test**

Add to the `tests` module of `src/cmd/drill/post.rs` (written against the new four-argument signature; it will not compile until Step 3, which is this test's failing state):

```rust
#[test]
fn test_double_post_same_card_hash_is_a_no_op() {
    let card_a = make_card("QA");
    let card_b = make_card("QB");
    let mut mutable = make_state_with_cards(vec![card_a.clone(), card_b.clone()]);
    let started = Timestamp::now();
    // First grade of card A, carrying its hash: accepted.
    handle_action(&mut mutable, started, Action::Reveal, None).unwrap();
    let result = handle_action(&mut mutable, started, Action::Good, Some(card_a.hash())).unwrap();
    assert!(matches!(result, ActionResult::Continue));
    assert_eq!(mutable.cards.len(), 1);
    assert_eq!(mutable.cards[0].hash(), card_b.hash());
    // Card B is revealed, then the SAME grade POST for card A arrives again
    // (key auto-repeat / double submit): it must be a no-op.
    handle_action(&mut mutable, started, Action::Reveal, None).unwrap();
    let result = handle_action(&mut mutable, started, Action::Good, Some(card_a.hash())).unwrap();
    assert!(
        matches!(result, ActionResult::Ignored(_)),
        "a grade whose card hash does not match the queue head must be ignored"
    );
    assert_eq!(mutable.cards.len(), 1, "card B must not have been graded");
    assert_eq!(mutable.cards[0].hash(), card_b.hash());
    assert_eq!(
        mutable.db.get_reviews_for_session(mutable.session_id).unwrap().len(),
        1,
        "exactly one review row must exist"
    );
}
```

- [ ] **Step 2: Run the test and see it fail**

Run: `cargo test test_double_post_same_card_hash_is_a_no_op`
Expected: FAIL to compile — `handle_action` does not yet take a fourth argument. A compile error in the test is the failing state here.

- [ ] **Step 3: Add the `submitted_card` parameter and the stale-hash guard**

In `src/cmd/drill/post.rs`:

1. Extend `FormData`:

```rust
#[derive(Deserialize)]
pub struct FormData {
    pub action: Action,
    /// Hex hash of the card the client believes it is grading. Grades whose
    /// hash does not match the head of the queue are ignored (BUG-06).
    pub card: Option<String>,
}
```

2. Change the `handle_action` signature:

```rust
/// Core action handling logic, reusable by both drill and serve modes.
///
/// `submitted_card` is the card hash the client's grade form carried; a
/// grade for a card that is no longer at the head of the queue (stale page,
/// double submit, key auto-repeat) is ignored. `None` (e.g. non-grade
/// actions) skips the check.
pub fn handle_action(
    mutable: &mut MutableState,
    session_started_at: Timestamp,
    action: Action,
    submitted_card: Option<CardHash>,
) -> Fallible<ActionResult> {
```

3. In the grade arm, insert the stale-hash guard between the head lookup and the reveal guard — the full top of the arm becomes:

```rust
        Action::Forgot | Action::Hard | Action::Good | Action::Easy => {
            let head: Card = match mutable.cards.first() {
                Some(card) => card.clone(),
                None => return Ok(ActionResult::Continue),
            };
            if let Some(submitted) = submitted_card {
                if submitted != head.hash() {
                    // Stale grade: the card it refers to is no longer at the
                    // head of the queue (double submit or key auto-repeat).
                    log::debug!(
                        "ignoring stale grade for card {submitted}: current card is {}",
                        head.hash()
                    );
                    return Ok(ActionResult::Ignored(
                        "That card was already graded, so the repeated grade was ignored.".to_string(),
                    ));
                }
            }
            if !mutable.reveal {
                log::debug!("ignoring grade action: no card is revealed");
                return Ok(ActionResult::Ignored(
                    "That grade was ignored because no answer was revealed. The current card is shown below.".to_string(),
                ));
            }
            // ... the rest of the arm is the code ALREADY IN THE FILE from
            // the previous commit, starting at
            // `let reviewed_at: Timestamp = Timestamp::now();` — unchanged
            // except: there must be exactly ONE `let head` lookup in the
            // arm (the one above), so delete the now-duplicate lookup that
            // previously sat below the reveal guard, and keep using `head`
            // where the deleted binding was used ...
```

4. Parse the submitted hash in `action_handler` (an unparseable hash is treated the same as a stale one — ignored — rather than an error, since it can only come from a tampered or corrupted form):

```rust
async fn action_handler(state: ServerState, form: FormData) -> Fallible<Option<Flash>> {
    let submitted_card: Option<CardHash> = match form.card.as_deref() {
        Some(hex) => match CardHash::from_hex(hex) {
            Ok(hash) => Some(hash),
            Err(_) => {
                log::debug!("ignoring grade with unparseable card hash");
                return Ok(Some(Flash::error(
                    "That grade carried an invalid card reference and was ignored.",
                )));
            }
        },
        None => None,
    };
    let mut mutable = state.mutable.lock().unwrap();
    let result = handle_action(&mut mutable, state.session_started_at, form.action, submitted_card)?;
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
        ActionResult::Ignored(reason) => Ok(Some(Flash::error(reason))),
        ActionResult::Continue | ActionResult::SessionFinished | ActionResult::Home => Ok(None),
    }
}
```

5. Update every existing test call of `handle_action` in `post.rs`'s `tests` module to pass `None` as the fourth argument: `test_home_returns_home`, `test_shutdown_returns_continue_when_unfinished`, `test_reveal_sets_flag`, `test_end_finishes_session`, `test_grade_db_failure_leaves_state_unchanged`, `test_undo_db_failure_leaves_state_unchanged`, `test_undo_after_finish_reopens_session_row`, `test_grade_without_reveal_is_ignored_with_flash`.

- [ ] **Step 4: Pass the submitted hash through the serve handler**

In `src/cmd/serve/handlers.rs`, add `use crate::types::card_hash::CardHash;` (already imported — verify) and change `collection_post_handler` / `collection_post_inner` to carry the form's card field:

```rust
pub async fn collection_post_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Form(form): Form<FormData>,
) -> Redirect {
    match collection_post_inner(&state, &slug, form) {
        Ok(redirect) => redirect,
        Err(e) => {
            log::error!("error handling action for collection {slug}: {e}");
            Redirect::to(&format!("/collection/{slug}"))
        }
    }
}

fn collection_post_inner(state: &AppState, slug: &str, form: FormData) -> Fallible<Redirect> {
    let action = form.action;
    let submitted_card: Option<CardHash> = match form.card.as_deref() {
        Some(hex) => match CardHash::from_hex(hex) {
            Ok(hash) => Some(hash),
            Err(_) => {
                log::debug!("ignoring grade with unparseable card hash for collection {slug}");
                return Ok(Flash::error("That grade carried an invalid card reference and was ignored.")
                    .redirect(&format!("/collection/{slug}")));
            }
        },
        None => None,
    };
    // The `Action::Home` early-return block ALREADY IN THE FILE
    // (handlers.rs, "Home action: close the session and drop it...") stays
    // byte-for-byte unchanged here.
    // Below it, the session take-out / re-insert from Task 5 stays in place
    // (Task 7 replaces it with per-slug locking); only the handle_action
    // call gains the fourth argument:
    let mut session = match state.sessions.lock().unwrap().remove(slug) {
        Some(s) => s,
        None => return Ok(Redirect::to(&format!("/collection/{slug}"))),
    };
    let result = handle_action(&mut session.mutable, session.session_started_at, action, submitted_card);
    state.sessions.lock().unwrap().insert(slug.to_owned(), session);
    match result? {
        ActionResult::Ignored(reason) => Ok(Flash::error(reason).redirect(&format!("/collection/{slug}"))),
        _ => Ok(Redirect::to(&format!("/collection/{slug}"))),
    }
}
```

- [ ] **Step 5: Carry the card hash in the grade form**

In `src/cmd/drill/get.rs`, in `render_session_page`'s revealed branch (the form at lines 119-130), add a hidden input as the first child of the form:

```rust
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
```

(`card` is already in scope from line 93. The unrevealed form does not need the field — Reveal/Undo/End/Bookmark ignore `submitted_card`.)

- [ ] **Step 6: Ignore key auto-repeat on the client**

In `src/cmd/drill/script.js`, at the very top of the keydown listener body (line 43, before the input/textarea check), add:

```js
document.addEventListener("keydown", function (event) {
  // A held-down key fires repeated keydown events; only the first physical
  // press should act (BUG-06).
  if (event.repeat) {
    return;
  }
  // Skip during text input or textarea.
```

- [ ] **Step 7: Run the tests and see them pass**

Run: `cargo test`
Expected: `test_double_post_same_card_hash_is_a_no_op` PASSES; all updated tests compile and pass.

- [ ] **Step 8: Update CHANGELOG.xml (BUG-06)**

Insert directly after the `<fixed>` line inside `<unreleased>` in `CHANGELOG.xml`:

```xml
            <change author="mstoeck3">
                Holding down a grading key or double-submitting a grade no longer grades several cards in a row: key auto-repeat is ignored in the browser, and the server ignores a grade for a card that is no longer current, showing a notice instead.
            </change>
```

- [ ] **Step 9: Commit**

```bash
git add src/cmd/drill/post.rs src/cmd/drill/get.rs src/cmd/drill/script.js src/cmd/serve/handlers.rs CHANGELOG.xml
git commit -m "fix: make grade submissions idempotent against auto-repeat and double-submit (BUG-06)"
```

---

### Task 7: BUG-01 — serve mode must not drop the session on a request error

`collection_get_inner` removes the session from the map (`handlers.rs:85`) and only re-inserts it after fallible rendering (`:145`, with `?` at `:137-141`); pre-Task-5 `collection_post_inner` had the same shape. Any error permanently destroyed the running drill and left the DB session row open, and a concurrent GET during the window saw "no session". Replace take-out/put-back with per-slug locking: the map holds `Arc<Mutex<DrillSession>>`, handlers clone the `Arc`, release the map lock, and lock the session entry for the duration of the work. Errors leave the session in the map untouched.

**Files:**
- Modify: `src/cmd/serve/state.rs:18` (`AppState::sessions` type; add `SharedSession` alias)
- Modify: `src/cmd/serve/handlers.rs` (`collection_get_inner`, `collection_post_inner`, `collection_start_inner`, `collection_script_handler`)
- Test: `src/cmd/serve/mod.rs` (`tests` module — HTTP integration regression test)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: Task 6's `handle_action(&mut MutableState, Timestamp, Action, Option<CardHash>)` and Task 5/6's `collection_post_inner(state, slug, form: FormData)` shape.
- Produces: `pub type SharedSession = std::sync::Arc<std::sync::Mutex<DrillSession>>;` in `src/cmd/serve/state.rs`; `AppState.sessions: Arc<Mutex<HashMap<String, SharedSession>>>`. `src/cmd/serve/edit.rs` uses only `contains_key` on the map (lines 60, 148) and needs no change; `src/cmd/serve/server.rs:176` initializes with `HashMap::new()`, which infers the new type unchanged.

- [ ] **Step 1: Write the failing HTTP regression test**

Add to the existing `tests` module in `src/cmd/serve/mod.rs` (reusing its config-building pattern; the render error is forced by deleting the card's source file, which makes `Card::relative_file_path`'s canonicalize fail during `render_session_page`):

```rust
    /// Regression test (BUG-01): a request error mid-session must not drop
    /// the drill session. Forces a render error by deleting the card's
    /// source file, then asserts the session survives and the next GET
    /// renders the same card rather than the deck browser.
    #[tokio::test]
    async fn test_session_survives_render_error() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        let card_file = coll_dir.join("Alpha.md");
        write(&card_file, "Q: What is 1+1?\nA: 2\n")?;

        let slug = "test-collection".to_string();
        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: vec![ResolvedCollection {
                name: "Test Collection".to_string(),
                slug: slug.clone(),
                coll_dir: coll_dir.clone(),
                db_path: coll_dir.join("hashcards.db"),
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            _temp_dir: None,
        };

        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;
        let client = reqwest::Client::new();

        // Start a drill session; the redirect is followed to the session page.
        let response = client
            .post(format!("http://{TEST_HOST}:{port}/collection/{slug}/start"))
            .body("decks=Alpha")
            .header("content-type", "application/x-www-form-urlencoded")
            .send()
            .await?;
        assert!(response.status().is_success());
        let body = response.text().await?;
        assert!(body.contains("progress-bar"), "expected a running session, got: {body}");

        // Force a render error mid-session: the card's source file vanishes.
        std::fs::remove_file(&card_file)?;
        let response = client
            .get(format!("http://{TEST_HOST}:{port}/collection/{slug}"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);

        // Restore the file (identical content, identical hash). The session
        // must have survived the error: the next GET renders the same card,
        // not the deck browser.
        write(&card_file, "Q: What is 1+1?\nA: 2\n")?;
        let response = client
            .get(format!("http://{TEST_HOST}:{port}/collection/{slug}"))
            .send()
            .await?;
        assert!(response.status().is_success());
        let body = response.text().await?;
        assert!(
            body.contains("progress-bar"),
            "session was dropped by the render error: {body}"
        );
        assert!(
            !body.contains("deck-tree"),
            "deck browser rendered instead of the surviving session"
        );
        Ok(())
    }
```

- [ ] **Step 2: Run the test and see it fail**

Run: `cargo test test_session_survives_render_error`
Expected: FAIL on the final `progress-bar` assertion — pre-fix, the erroring GET removed the session, so the last GET renders the deck browser (`deck-tree`).

- [ ] **Step 3: Change the session map to hold `Arc<Mutex<DrillSession>>`**

In `src/cmd/serve/state.rs`:

```rust
/// A drill session shared behind a per-slug lock. Handlers clone the `Arc`
/// out of the map (releasing the map lock immediately) and lock the session
/// itself for the duration of the request, so an error can never remove the
/// session from the map.
pub type SharedSession = Arc<Mutex<DrillSession>>;
```

and change the field in `AppState`:

```rust
    pub sessions: Arc<Mutex<HashMap<String, SharedSession>>>,
```

- [ ] **Step 4: Rework the handlers to per-slug locking**

In `src/cmd/serve/handlers.rs` (add `use crate::cmd::serve::state::SharedSession;` and `use std::sync::Arc; use std::sync::Mutex;` to the imports):

1. `collection_get_inner` (lines 83-147): replace the take-out (line 85) and put-back (line 145) with a lookup, and lock the entry for rendering:

```rust
fn collection_get_inner(state: &AppState, slug: &str) -> Fallible<String> {
    // Clone the Arc out of the map so the map lock is not held during
    // rendering; the session itself stays in the map even if rendering fails.
    let session: Option<SharedSession> = state.sessions.lock().unwrap().get(slug).cloned();

    let Some(session) = session else {
        // ... the deck-browser branch ALREADY IN THE FILE (handlers.rs:88-123,
        // from "No active session: show the deck browser." through
        // `return Ok(html.into_string());`) stays byte-for-byte unchanged ...
    };

    let session = session.lock().unwrap();
    let form_action = format!("/collection/{slug}");
    let file_url_prefix = format!("/collection/{slug}/file");
    let ctx = RenderContext {
        directory: &session.directory,
        total_cards: session.total_cards,
        session_started_at: session.session_started_at,
        answer_controls: session.answer_controls,
        form_action: &form_action,
        file_url_prefix: &file_url_prefix,
        completion_action: CompletionAction::BackToCollections,
    };
    let body = if session.mutable.finished_at.is_some() {
        render_completion_page(&ctx, &session.mutable)?
    } else {
        render_session_page(&ctx, &session.mutable)?
    };
    let script_url = format!("/collection/{slug}/script.js");
    let html = page_template_with_script(&script_url, body);
    Ok(html.into_string())
}
```

(The `// Put the session back now that rendering is done.` line and comment are deleted.)

2. `collection_post_inner`: the Home early-return still removes the session from the map (the session is ending by explicit user request), but locks the entry to close the DB row:

```rust
    if matches!(action, Action::Home) {
        let session = state.sessions.lock().unwrap().remove(slug);
        if let Some(s) = session {
            let s = s.lock().unwrap();
            if s.mutable.finished_at.is_none() {
                if let Err(e) = s.mutable.db.close_session(s.mutable.session_id, Timestamp::now()) {
                    log::error!(
                        "failed to close session {} for collection {slug}: {e}",
                        s.mutable.session_id
                    );
                }
            }
        }
        // ... the background collection-info refresh ALREADY IN THE FILE
        // (handlers.rs:372-379, snapshot + tokio::spawn) stays unchanged ...
        return Ok(Redirect::to("/"));
    }
```

and the action path stops removing/re-inserting entirely:

```rust
    // Lock the session in place: the map lock is released immediately, the
    // per-slug lock is held for the DB work, and an error leaves the session
    // in the map untouched.
    let session: SharedSession = match state.sessions.lock().unwrap().get(slug).cloned() {
        Some(s) => s,
        None => return Ok(Redirect::to(&format!("/collection/{slug}"))),
    };
    let mut guard = session.lock().unwrap();
    let session = &mut *guard;
    let result = handle_action(&mut session.mutable, session.session_started_at, action, submitted_card)?;
    match result {
        ActionResult::Ignored(reason) => Ok(Flash::error(reason).redirect(&format!("/collection/{slug}"))),
        _ => Ok(Redirect::to(&format!("/collection/{slug}"))),
    }
```

(The `let session = &mut *guard;` reborrow lets `session.mutable` and `session.session_started_at` be borrowed disjointly in one call.)

3. `collection_start_inner` (lines 227-242): wrap the new session when inserting:

```rust
    if let Some(s) = session {
        state.sessions.lock().unwrap().insert(slug.to_string(), Arc::new(Mutex::new(s)));
    }
```

(The `remove(slug)` at the top of `collection_start_inner` stays: starting a new drill deliberately replaces any existing session — an explicit user action, not an error path.)

4. `collection_script_handler` (lines 456-479): clone the macros out through the per-slug lock:

```rust
pub async fn collection_script_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> (StatusCode, [(HeaderName, &'static str); 1], String) {
    let session: Option<SharedSession> = state.sessions.lock().unwrap().get(&slug).cloned();
    let macros: Vec<(String, String)> = match session {
        Some(session) => session.lock().unwrap().macros.clone(),
        None => {
            // No active session; serve script without macros
            let content = format!("let MACROS = {{}};\n\n{}", include_str!("../drill/script.js"));
            return (StatusCode::OK, [(CONTENT_TYPE, "text/javascript")], content);
        }
    };
    let mut content = String::new();
    content.push_str("let MACROS = {};\n");
    for (name, definition) in &macros {
        let name = escape_js_string_literal(name);
        let definition = escape_js_string_literal(definition);
        content.push_str(&format!("MACROS['{name}'] = '{definition}';\n"));
    }
    content.push('\n');
    content.push_str(include_str!("../drill/script.js"));
    (StatusCode::OK, [(CONTENT_TYPE, "text/javascript")], content)
}
```

The `known` check in `collection_get_handler` (line 61) and the `contains_key` calls in `src/cmd/serve/edit.rs:60` and `:148` compile unchanged against the new value type.

- [ ] **Step 5: Run the tests and see them pass**

Run: `cargo test`
Expected: `test_session_survives_render_error` PASSES; `test_start_with_multiple_decks` and all other tests still pass.

- [ ] **Step 6: Update CHANGELOG.xml (BUG-01)**

Insert directly after the `<fixed>` line inside `<unreleased>` in `CHANGELOG.xml`:

```xml
            <change author="mstoeck3">
                In serve mode, an error while rendering a card or handling an action no longer destroys the running drill session. The session now stays in place behind a per-collection lock, so after a transient error the same card is shown again and progress is kept.
            </change>
```

- [ ] **Step 7: Commit**

```bash
git add src/cmd/serve/state.rs src/cmd/serve/handlers.rs src/cmd/serve/mod.rs CHANGELOG.xml
git commit -m "fix: keep serve-mode sessions alive across request errors via per-slug locking (BUG-01)"
```

---

### Task 8: Final verification and PR

**Files:** none new.

- [ ] **Step 1: Full test suite and lint**

Run:
```bash
cargo test && cargo clippy -- -D warnings && cargo fmt --check
```
Expected: all pass. Fix any clippy/fmt fallout within the files this plan touched before proceeding.

- [ ] **Step 2: Verify no production `unwrap()` was introduced**

Run:
```bash
git diff master -- src | grep -n "unwrap()" | grep -v "mod tests" || true
```
Manually confirm every surviving `unwrap()` in the diff is either inside a `#[cfg(test)]` module or an existing-convention `.lock().unwrap()` (BUG-50's scope).

- [ ] **Step 3: Open the PR**

```bash
git push -u origin fix/data-integrity
gh pr create --repo overcuriousity/hashcards-web \
  --title "Data integrity: session survival, DB-first mutations, idempotent grades" \
  --body "Fixes BUG-01, BUG-02, BUG-04, BUG-06, BUG-15 from SPEC.md (PR group 1). Each fix carries a regression test; CHANGELOG.xml updated per item."
```

---

## Spec discrepancies

Findings from verifying every cited file/line against the working tree (all other cited line numbers — `handlers.rs:85/:137-141/:145/:234/:386/:396/:398`, `post.rs:123-134/:140/:172/:180/:200/:226-229`, `state.rs:18`, `script.js:42` — match the code exactly):

1. **BUG-04 "null `ended_at`" is impossible without a migration.** `sessions.ended_at` is declared `text not null` in a *strict* table (`src/schema.sql`), and `SessionRow.ended_at` is a non-optional `Timestamp` (`src/db.rs:52`). The codebase's documented open-session convention is instead `ended_at == started_at` — `create_session` sets that placeholder explicitly (`src/db.rs:245-251`). This plan reopens sessions by resetting `ended_at = started_at`, matching the existing convention; making the column nullable is a table-rebuild migration that belongs with BUG-32 (plan 08, schema versioning), not a data-integrity PR.
2. **`script.js` location.** The spec cites `script.js:42` without a path; the file is `src/cmd/drill/script.js` (there is no `static/` directory). It is embedded via `include_str!` and served by both the drill server and serve mode's `/collection/{slug}/script.js`, so the single `event.repeat` fix covers both modes. Line 42 is indeed the keydown listener.
3. **Flash dependency crosses the spec's PR grouping.** SPEC.md places FEAT-01 (flash messages) in PR group 2, but BUG-06 and BUG-15 in group 1 need it. The master plan (`docs/superpowers/plans/2026-08-31-00-master.md`) resolves this by ordering plan 01 (flash-messages) before this plan; Task 1 verifies that dependency and stops if it is missing.
4. **BUG-01's `:234` note is a design choice, not part of the fix.** `collection_start_inner:234` discarding an in-flight session when a new drill is started is an explicit user action; the spec's fix directive ("errors must leave the session intact") does not ask to change it, and this plan keeps it. What the fix removes is the error window during which a concurrent GET could observe "no session" at all.
5. **BUG-02 undo also fixes a BUG-49 item early.** Reordering the undo arm naturally removes the `mutable.reviews.pop().unwrap()` cited at `post.rs:123` in BUG-49. Plan 07 (panic-hygiene) should find that item already resolved.
6. **Pre-existing `assert_eq` note.** The spec text for BUG-02 says "the card silently vanishes"; strictly, the handler error *is* logged (`handlers.rs:350`, `post.rs:84`) — the vanishing is silent only from the user's perspective. The fix direction is unaffected.
