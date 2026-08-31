# Drill Polish Implementation Plan (PR 14)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the bookmark-note clobbering bug (BUG-07), remove dead session-start-time threading (BUG-09), make Ctrl+C during a drill a clean exit with a summary (BUG-10), and add a repeat-aware "N of M (+k repeats)" progress counter (FEAT-08).

**Architecture:** All four items live in the drill subsystem (`src/db.rs` bookmark writes, `src/cmd/drill/{post,server,state,get}.rs`, `src/cmd/drill/style.css`). FEAT-08 adds **no new stored session state**: progress is derived from the existing undo-aware `MutableState.reviews` list (distinct card hashes = first grades; surplus = repeats), so Undo rolls the counter back for free. BUG-10 extracts the shutdown path into a plain function so it is testable without sending a real signal.

**Tech Stack:** Rust, cargo, axum, maud, rusqlite. Tests are plain `#[test]`/`#[tokio::test]` with in-memory SQLite (`Database::new(":memory:")`).

**Spec:** SPEC.md (items BUG-07, BUG-09, BUG-10, FEAT-08; PR group 14 "Drill polish"). UX rationale for FEAT-08: ROADMAP.md, Phase 3, "Progress text with repeat-aware bar".

## Global Constraints

From SPEC.md "Global requirements" (copied verbatim):

- Every bugfix starts with a failing regression test.
- No `unwrap()` in production code; use `Fallible`, `?`, and `fail()`.
- All error messages are user-facing and clear.
- Reviews and performance are written in one transaction; undo voids, never deletes.
- Cloze positions are byte positions: `.bytes()`, never `.chars()`.
- Update `CHANGELOG.xml` per item.

Additional project rules that bear on this plan (from CLAUDE.md):

- Prefer imports over qualified paths (`use foo::bar;`, then `bar()`).
- Each grade is written to the database as it happens; the in-memory cache is the session's working state, not the sole writer.
- Keep functions small and focused. Tests may use `unwrap()`.
- CHANGELOG.xml entries are appended as `<change author="claude">…</change>` inside the existing `<fixed>`, `<changed>`, or `<added>` list under `<unreleased>` (see `CHANGELOG.xsd`; the file already has all three lists).

---

### Task 1: BUG-07 — Re-bookmarking must not clobber the note or created_at

`Database::insert_bookmark` (`src/db.rs:403-413`) uses `insert or replace` (`src/db.rs:409`), and the drill Bookmark action always passes `note: None` (`src/cmd/drill/post.rs:159-160`). So pressing `b` on an already-bookmarked card wipes the user's note and resets `created_at`. The doc comment at `src/db.rs:400` ("If the card has no DB row yet, it is created first") is also wrong: `insert_bookmark` does not create the card row — the caller does, via `insert_card_if_new` (`src/cmd/drill/post.rs:159`).

Note: SPEC.md also asks for "a dedicated `update_bookmark_note` for the notes form" — that function **already exists** (`src/db.rs:459-467`) and is already used by the notes form (`src/cmd/serve/bookmarks.rs:233`). Do not add another one; this task only changes `insert_bookmark`'s SQL and doc comment.

**Files:**
- Modify: `src/db.rs:400-413` (doc comment + SQL)
- Test: `src/db.rs` (existing `#[cfg(test)] mod tests` at the bottom of the file, next to `test_bookmark_crud` at `src/db.rs:~927`)

**Interfaces:**
- Consumes: `Database::insert_bookmark(card_hash, note, now)`, `Database::update_bookmark_note(card_hash, note)`, `Database::get_bookmark(card_hash)` — all existing, signatures unchanged.
- Produces: `insert_bookmark` with insert-if-absent semantics (an existing bookmark row is left completely untouched). No signature change; no other task depends on this one.

- [x] **Step 1: Write the failing regression test**

Add to the tests module in `src/db.rs`, next to `test_bookmark_crud`:

```rust
    /// BUG-07: bookmark, add note, re-bookmark — the note and created_at survive.
    #[test]
    fn test_rebookmark_preserves_note_and_created_at() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let hash = CardHash::hash_bytes(b"a");
        let created = Timestamp::new(
            NaiveDate::from_ymd_opt(2026, 8, 30)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
        );
        let later = Timestamp::new(
            NaiveDate::from_ymd_opt(2026, 8, 31)
                .unwrap()
                .and_hms_opt(12, 30, 0)
                .unwrap(),
        );
        db.insert_card(hash, created)?;
        // Bookmark from the drill UI (the drill path always passes note: None).
        db.insert_bookmark(hash, None, created)?;
        // The user adds a note via the bookmark notes form.
        db.update_bookmark_note(hash, Some("needs rephrasing".to_string()))?;
        // The user presses `b` again on the same card in a later session.
        db.insert_bookmark(hash, None, later)?;
        let bm = db.get_bookmark(hash)?.unwrap();
        assert_eq!(bm.note, Some("needs rephrasing".to_string()));
        assert_eq!(bm.created_at, created);
        Ok(())
    }
```

This needs `NaiveDate` in scope inside the tests module. The tests module already imports test helpers with `use` statements at its top; add there:

```rust
    use chrono::NaiveDate;
```

(`chrono` is already a dependency — `src/types/timestamp.rs` uses it. `Timestamp::new` is `#[cfg(test)]`-only, which is exactly this context. `Timestamp` derives `PartialEq`, so `assert_eq!` on `created_at` works.)

- [x] **Step 2: Run the test and see it fail**

Run: `cargo test test_rebookmark_preserves_note_and_created_at`
Expected: FAIL on the `bm.note` assertion — `insert or replace` replaced the row, so `note` is `None` (and `created_at` is `later`).

- [x] **Step 3: Implement the fix**

In `src/db.rs`, replace the doc comment and SQL of `insert_bookmark` (currently at lines 400-413):

```rust
    /// Insert a bookmark for this card if none exists. An existing bookmark —
    /// including its note and creation time — is left completely untouched.
    /// The card row must already exist; drill callers ensure this with
    /// `insert_card_if_new`.
    ///
    /// Unlike reviews, bookmarks write to the DB mid-session so they survive aborted sessions.
    pub fn insert_bookmark(
        &self,
        card_hash: CardHash,
        note: Option<String>,
        now: Timestamp,
    ) -> Fallible<()> {
        let sql = "insert into bookmarks (card_hash, note, created_at) values (?, ?, ?) on conflict (card_hash) do nothing;";
        self.conn.execute(sql, params![card_hash, note, now])?;
        Ok(())
    }
```

(`card_hash` is the primary key of `bookmarks` — see `src/schema.sql` — so `on conflict (card_hash)` is the right conflict target. The only production caller is `src/cmd/drill/post.rs:160`; the notes form goes through `update_bookmark_note`, so nothing loses the ability to change a note.)

- [x] **Step 4: Run the tests and see them pass**

Run: `cargo test --lib db`
Expected: PASS, including `test_rebookmark_preserves_note_and_created_at`, `test_bookmark_crud`, `test_bookmark_cascade_delete`, `test_rename_card_hash_cascades_bookmark`.

- [x] **Step 5: Update CHANGELOG.xml**

In `CHANGELOG.xml`, append inside the existing `<unreleased><fixed>` list:

```xml
            <change author="claude">
                Re-bookmarking a card during drilling (pressing `b` on an already-bookmarked card) no longer erases the bookmark's note or resets its creation time. (BUG-07)
            </change>
```

- [x] **Step 6: Commit**

```bash
git add src/db.rs CHANGELOG.xml
git commit -m "fix: re-bookmarking no longer clobbers the bookmark note (BUG-07)"
```

---

### Task 2: BUG-09 — Remove the dead `_session_started_at` threading

`finish_session` (`src/cmd/drill/post.rs:226`) takes `_session_started_at: Timestamp` and ignores it — a leftover from the batch-flush era. Once it is gone, the `session_started_at` parameter of `handle_action` (`src/cmd/drill/post.rs:108-112`) is itself only ever passed to `finish_session` (`post.rs:141`, `post.rs:217`), so it becomes dead too and must also be removed, along with its call sites: `action_handler` (`post.rs:92`), serve's `collection_post_inner` (`src/cmd/serve/handlers.rs:396`), and the four tests in `post.rs` that pass a `now` argument (`test_home_returns_home`, `test_shutdown_returns_continue_when_unfinished`, `test_reveal_sets_flag`, `test_end_finishes_session`).

**No failing regression test is possible for this item**: it is a pure signature change with no observable behavior (the Global Constraints' "failing test first" rule cannot apply to dead-code removal — see Spec discrepancies). The guard is the compiler plus the existing test suite.

Do **not** touch `ServerState.session_started_at` (`src/cmd/drill/state.rs:35`), `RenderContext.session_started_at` (`src/cmd/drill/get.rs:46`), or `DrillSession.session_started_at` (`src/cmd/serve/state.rs:52`) — those are still used by the completion page (`get.rs:248`) and its render contexts (`get.rs:72`, `handlers.rs:131`).

**Files:**
- Modify: `src/cmd/drill/post.rs:90-92, 108-112, 141, 217, 226` (and the tests module in the same file)
- Modify: `src/cmd/serve/handlers.rs:396`

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: `pub fn handle_action(mutable: &mut MutableState, action: Action) -> Fallible<ActionResult>` and `fn finish_session(mutable: &mut MutableState) -> Fallible<()>`. Task 3 and Task 5 read this file but do not call these; serve's `handlers.rs:396` is updated here.

- [x] **Step 1: Remove the parameter from `finish_session` and `handle_action`**

In `src/cmd/drill/post.rs`:

```rust
fn finish_session(mutable: &mut MutableState) -> Fallible<()> {
    log::debug!("Session completed");
    let session_ended_at = Timestamp::now();
    mutable.db.close_session(mutable.session_id, session_ended_at)?;
    mutable.finished_at = Some(session_ended_at);
    Ok(())
}
```

```rust
/// Core action handling logic, reusable by both drill and serve modes.
pub fn handle_action(mutable: &mut MutableState, action: Action) -> Fallible<ActionResult> {
```

Update the two internal calls (`post.rs:141` and `post.rs:217`) to `finish_session(mutable)?;`, and `action_handler` (`post.rs:92`) to:

```rust
    let result = handle_action(&mut mutable, action)?;
```

If `Timestamp` is now unused in any `use` list in `post.rs`, remove the import (it is still used by `Timestamp::now()` in the grade and finish paths, so in practice it stays).

- [x] **Step 2: Update the serve call site**

In `src/cmd/serve/handlers.rs:396`:

```rust
    handle_action(&mut session.mutable, action)?;
```

- [x] **Step 3: Update the tests in `post.rs`**

The four tests currently create `let now = Timestamp::now();` and pass it as the second argument. Change each call to the two-argument form and delete the now-unused `now` binding, e.g.:

```rust
    #[test]
    fn test_end_finishes_session() {
        let mut mutable = make_mutable();
        let result = handle_action(&mut mutable, Action::End).unwrap();
        assert!(matches!(result, ActionResult::SessionFinished));
        assert!(mutable.finished_at.is_some());
    }
```

Apply the same change to `test_home_returns_home`, `test_shutdown_returns_continue_when_unfinished`, and `test_reveal_sets_flag`.

- [x] **Step 4: Build and run the full test suite**

Run: `cargo build && cargo test`
Expected: clean build with no unused-variable/unused-import warnings in `post.rs` or `handlers.rs`; all tests PASS.

- [x] **Step 5: Update CHANGELOG.xml**

Append inside `<unreleased><changed>`:

```xml
            <change author="claude">
                Internal cleanup: removed the unused session start time parameter threaded through the drill action handlers, a leftover from the era when reviews were flushed in a batch at session end. (BUG-09)
            </change>
```

- [x] **Step 6: Commit**

```bash
git add src/cmd/drill/post.rs src/cmd/serve/handlers.rs CHANGELOG.xml
git commit -m "refactor: remove dead session_started_at threading from drill actions (BUG-09)"
```

---

### Task 3: BUG-10 — Ctrl+C during a drill exits cleanly with a summary

`start_server` (`src/cmd/drill/server.rs:218-231`) treats an interrupt as an error: it closes the session row (only logging any failure) and returns `fail("Session interrupted before completion")` (`server.rs:230`), so the process exits non-zero. Since every grade is persisted the moment it happens, an interrupt loses nothing. The fix: close the session row, print a summary of the reviews that were persisted, and return `Ok(())` (exit 0).

**Test strategy (deliberate):** we do not test the Ctrl+C signal itself — sending a real signal to the axum server in a test is flaky and tests tokio/OS plumbing, not our logic. Instead the shutdown bookkeeping is extracted into a plain function, `finalize_interrupted_session`, and that function is tested directly. The graceful-shutdown wiring (`shutdown_signal`, `server.rs:314-333`) is unchanged.

**Files:**
- Modify: `src/cmd/drill/server.rs:218-231` (tail of `start_server`), plus a new function and a new `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Consumes: `MutableState::new(db, session_id, cache, cards)` (`src/cmd/drill/state.rs:55`), `Database::close_session(session_id, ended_at)` (`src/db.rs:342`), `Database::get_reviews_for_session(session_id) -> Fallible<Vec<ReviewRow>>` (`src/db.rs:502`, returns only non-voided reviews — matching the void-not-delete rule), `Database::insert_review_and_update_performance` (`src/db.rs:259`), `update_performance` (`src/types/performance.rs`).
- Produces: `pub fn finalize_interrupted_session(mutable: &MutableState) -> Fallible<String>` in `src/cmd/drill/server.rs`. No other task calls it.

- [ ] **Step 1: Write the failing test**

Add at the bottom of `src/cmd/drill/server.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::cmd::drill::cache::Cache;
    use crate::db::ReviewRecord;
    use crate::fsrs::Grade;
    use crate::types::performance::Performance;
    use crate::types::performance::update_performance;

    use super::*;

    /// BUG-10: interrupting a drill closes the session row and reports how
    /// many reviews were persisted, instead of erroring out.
    #[test]
    fn test_finalize_interrupted_session_closes_row_and_summarizes() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let started_at = Timestamp::now();
        let session_id = db.create_session(started_at)?;
        let hash = CardHash::hash_bytes(b"card");
        db.insert_card(hash, started_at)?;
        // Persist one review, exactly as the grade path does.
        let performance = update_performance(Performance::New, Grade::Good, started_at);
        let record = ReviewRecord {
            card_hash: hash,
            reviewed_at: started_at,
            grade: Grade::Good,
            stability: performance.stability,
            difficulty: performance.difficulty,
            interval_raw: performance.interval_raw,
            interval_days: performance.interval_days,
            due_date: performance.due_date,
            duration_ms: Some(1500),
        };
        db.insert_review_and_update_performance(
            session_id,
            &record,
            Performance::Reviewed(performance),
        )?;
        let mutable = MutableState::new(db, session_id, Cache::new(), Vec::new());

        let summary = finalize_interrupted_session(&mutable)?;

        assert_eq!(summary, "Session interrupted. 1 review saved.");
        // The session row is closed: get_all_sessions decodes ended_at as a
        // non-null Timestamp, so it only succeeds once the row is closed.
        let sessions = mutable.db.get_all_sessions()?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, session_id);
        Ok(())
    }
}
```

(`CardHash::hash_bytes` needs `use crate::types::card_hash::CardHash;` — already imported at the top of `server.rs`. `Fallible`, `Database`, `Timestamp`, `MutableState` are likewise already imported there and arrive via `use super::*;`.)

- [ ] **Step 2: Run the test and see it fail**

Run: `cargo test test_finalize_interrupted_session_closes_row_and_summarizes`
Expected: FAIL to compile with "cannot find function `finalize_interrupted_session`" — the function does not exist yet. (This is the failing-first step; the behavioral regression — `start_server` returning an error on interrupt — is asserted gone in Step 4's code review of the tail, since exercising a real signal is out of scope by design.)

- [ ] **Step 3: Implement `finalize_interrupted_session`**

Add to `src/cmd/drill/server.rs`, just below `start_server`:

```rust
/// Close an interrupted session's DB row and describe what was preserved.
///
/// Every grade is written to the database the moment it happens, so an
/// interrupt loses nothing; this only stamps `ended_at` and counts the
/// persisted (non-voided) reviews for the exit message.
pub fn finalize_interrupted_session(mutable: &MutableState) -> Fallible<String> {
    mutable.db.close_session(mutable.session_id, Timestamp::now())?;
    let count = mutable.db.get_reviews_for_session(mutable.session_id)?.len();
    let noun = if count == 1 { "review" } else { "reviews" };
    Ok(format!("Session interrupted. {count} {noun} saved."))
}
```

- [ ] **Step 4: Rewrite the tail of `start_server`**

Replace `src/cmd/drill/server.rs:218-231` (from the `// Check if session was complete...` comment through the closing brace of the `else` block):

```rust
    // Check if session was complete when server shut down.
    let mutable = state.mutable.lock().unwrap();
    if mutable.finished_at.is_some() {
        Ok(())
    } else {
        // Interrupted (e.g. Ctrl+C). Reviews persist as they happen, so
        // nothing is lost: close the session row and exit cleanly.
        let summary = finalize_interrupted_session(&mutable)?;
        println!("{summary}");
        Ok(())
    }
```

Also remove `use crate::error::fail;` from the imports at the top of `server.rs` if `fail` is now unused in this file (it is — `server.rs` had exactly one `fail(...)` call, at line 230).

Note: the pre-existing `state.mutable.lock().unwrap()` at `server.rs:219` stays as-is — the `.lock().unwrap()` sweep is BUG-50 (PR 6), out of scope here.

- [ ] **Step 5: Run the tests and see them pass**

Run: `cargo test --lib cmd::drill`
Expected: PASS, including the new test and the existing drill server tests in `src/cmd/drill/mod.rs`.

- [ ] **Step 6: Update CHANGELOG.xml**

Append inside `<unreleased><fixed>`:

```xml
            <change author="claude">
                Interrupting a drill with Ctrl+C now exits cleanly: the session is closed, a summary of the saved reviews is printed, and the exit code is 0. Reviews are written to the database as they happen, so an interrupted session loses nothing. Previously the command exited with the error "Session interrupted before completion". (BUG-10)
            </change>
```

- [ ] **Step 7: Commit**

```bash
git add src/cmd/drill/server.rs CHANGELOG.xml
git commit -m "fix: exit cleanly with a summary on Ctrl+C during drill (BUG-10)"
```

---

### Task 4: FEAT-08 (part 1) — Repeat-aware progress accounting on `MutableState`

Today the bar is computed as `cards_done = ctx.total_cards - mutable.cards.len()` (`src/cmd/drill/get.rs:90`). When a Forgot/Hard grade re-queues the card (`post.rs:209-211` pushes it back), the queue length does not shrink, so the bar silently stalls (ROADMAP.md Phase 3).

**State decision (required by the spec):** no new field is added to `MutableState`. Both numbers are *derived* from the existing `reviews: Vec<Review>` (`src/cmd/drill/state.rs:47`), which is already undo-aware (Undo pops it, `post.rs:123`):

- `first_graded` = number of **distinct** card hashes in `reviews` — the bar advances exactly on the first grade of each card.
- `repeats` = `reviews.len() - first_graded` — every additional grade of an already-counted card is a completed re-queue from Forgot/Hard.

Deriving instead of storing keeps Undo, session finish, and the grade path all consistent without touching `post.rs` at all. Card hashes are unique within a session queue (duplicate hashes abort startup — `cache.rs` insert errors; that's BUG-12, PR 13), so "distinct hash" equals "distinct card". Note the deliberate edge case: after the last *first* grade, the text can read "20 of 20 (+2 repeats)" with the bar full while re-queued cards are still being finished — that is the intended reading ("all cards seen, repeats remaining").

**Files:**
- Modify: `src/cmd/drill/state.rs` (new method + imports + new tests module)

**Interfaces:**
- Consumes: `MutableState.reviews`, `Review.card`, `Card::hash()` — all existing.
- Produces: `pub fn progress(&self) -> (usize, usize)` on `MutableState` returning `(first_graded, repeats)`. **Task 5 calls exactly this.**

- [ ] **Step 1: Write the failing test**

Add at the bottom of `src/cmd/drill/state.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::cmd::drill::cache::Cache;
    use crate::db::Database;
    use crate::types::card::CardContent;

    use super::*;

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
        let mut mutable =
            MutableState::new(db, session_id, Cache::new(), vec![a.clone(), b.clone()]);
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
```

(`Card`, `Grade`, `Performance`, `Timestamp` are already imported at the top of `state.rs` and arrive via `use super::*;`. `Card` and `Review` derive `Clone`. The deck name is a plain `String` — `DeckName` is `pub type DeckName = String` in `src/types/aliases.rs`.)

- [ ] **Step 2: Run the test and see it fail**

Run: `cargo test test_progress_counts_first_grades_and_repeats`
Expected: FAIL to compile with "no method named `progress` found for struct `MutableState`".

- [ ] **Step 3: Implement `MutableState::progress`**

In `src/cmd/drill/state.rs`, add to the imports at the top:

```rust
use std::collections::HashSet;

use crate::types::card_hash::CardHash;
```

and add the method inside the existing `impl MutableState` block (below `new`):

```rust
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
```

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test --lib cmd::drill::state`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cmd/drill/state.rs
git commit -m "feat: derive repeat-aware progress accounting from the review list (FEAT-08)"
```

(The CHANGELOG entry for FEAT-08 lands with the user-visible half in Task 5.)

---

### Task 5: FEAT-08 (part 2) — "N of M (+k repeats)" text and bar wiring

Render the counter next to the bar and drive the bar from `progress()` instead of queue arithmetic. `render_session_page` is shared by drill and serve modes (serve calls it at `src/cmd/serve/handlers.rs:141`), so both UIs get the feature from this one change. The completion-page stats (`get.rs:245-296`) are **not** touched — their inconsistencies are BUG-13 (PR 12).

**Files:**
- Modify: `src/cmd/drill/get.rs:87-93` (progress computation), `get.rs:143-155` (header markup), plus a new `progress_text` function and tests in the same file
- Modify: `src/cmd/drill/style.css` (inside the `.root { .header { … } }` nesting, around lines 36-56)

**Interfaces:**
- Consumes: `MutableState::progress() -> (usize, usize)` from Task 4.
- Produces: `pub fn progress_text(first_graded: usize, total_cards: usize, repeats: usize) -> String` in `src/cmd/drill/get.rs` (pure; unit-tested). Markup gains `div.progress` wrapping a new `div.progress-text` and the existing `div.progress-bar`.

- [ ] **Step 1: Write the failing tests for the text format**

`get.rs` has no tests module yet; add one at the bottom of `src/cmd/drill/get.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// FEAT-08: "N of M" text, with "(+k repeats)" only when repeats exist.
    #[test]
    fn test_progress_text_formats() {
        assert_eq!(progress_text(0, 20, 0), "0 of 20");
        assert_eq!(progress_text(7, 20, 0), "7 of 20");
        assert_eq!(progress_text(7, 20, 1), "7 of 20 (+1 repeat)");
        assert_eq!(progress_text(7, 20, 3), "7 of 20 (+3 repeats)");
        assert_eq!(progress_text(20, 20, 2), "20 of 20 (+2 repeats)");
    }
}
```

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test test_progress_text_formats`
Expected: FAIL to compile with "cannot find function `progress_text`".

- [ ] **Step 3: Implement `progress_text`**

Add to `src/cmd/drill/get.rs` (above `render_session_page`):

```rust
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
```

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test test_progress_text_formats`
Expected: PASS.

- [ ] **Step 5: Wire it into `render_session_page`**

In `src/cmd/drill/get.rs`, replace lines 88-92 (from `let undo_disabled…` through `let progress_bar_style…`):

```rust
    let undo_disabled = mutable.reviews.is_empty();
    let total_cards = ctx.total_cards;
    let (cards_done, repeats) = mutable.progress();
    let percent_done = (cards_done * 100).checked_div(total_cards).unwrap_or(100);
    let progress_bar_style = format!("width: {}%;", percent_done);
    let progress_label = progress_text(cards_done, total_cards, repeats);
```

(`checked_div(...).unwrap_or(...)` is not an `unwrap()` call and stays. The bar now advances on first grades only — repeats no longer make it stall *or* jump.)

Then replace the header block in the markup (lines 145-155):

```rust
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
```

- [ ] **Step 6: Style the text**

In `src/cmd/drill/style.css`, inside the `.root { .header { … } }` block, add alongside the existing `.progress-bar` rule (which keeps working — CSS nesting produces descendant selectors, and the bar is still a descendant of `.header`):

```css
        .progress {
            display: flex;
            flex-direction: column;
            align-items: center;
            gap: 6px;
        }

        .progress-text {
            font-size: 14px;
            color: #555;
        }
```

- [ ] **Step 7: Run the full test suite**

Run: `cargo test`
Expected: PASS (the drill e2e tests in `src/cmd/drill/mod.rs` fetch pages and must still render).

- [ ] **Step 8: Update CHANGELOG.xml**

Append inside `<unreleased><added>`:

```xml
            <change author="claude">
                The drill progress bar now has a numeric counter: "7 of 20 (+3 repeats)". The bar advances when a card is graded for the first time; cards re-queued by Forgot/Hard are tallied in the repeats counter instead of silently stalling the bar. Undo rolls the counter back. (FEAT-08)
            </change>
```

- [ ] **Step 9: Commit**

```bash
git add src/cmd/drill/get.rs src/cmd/drill/style.css CHANGELOG.xml
git commit -m "feat: repeat-aware progress counter next to the drill bar (FEAT-08)"
```

---

## Spec discrepancies

Found while verifying every cited line against the working tree (as of commit `be55f80`):

1. **BUG-07 — `update_bookmark_note` already exists.** The spec asks for "a dedicated `update_bookmark_note` for the notes form", but `Database::update_bookmark_note` is already implemented (`src/db.rs:459-467`) and already used by the notes form (`src/cmd/serve/bookmarks.rs:233`). The remaining work is only the `insert or replace` → `on conflict do nothing` change and the doc comment; Task 1 reflects that and adds no duplicate function.
2. **BUG-07 doc comment location.** The contradictory comment is at `src/db.rs:400` (first doc line of `insert_bookmark`; the "created first" claim is on that line, not spread over 400-401). The `insert or replace` SQL is at `src/db.rs:409` — matches the spec exactly.
3. **BUG-09 — the dead threading is wider than the spec's citations.** The spec cites `post.rs:226` (definition) and call sites `post.rs:141`, `:217` — all verified. But removing the parameter from `finish_session` makes `handle_action`'s own `session_started_at` parameter (`post.rs:110`) dead as well, cascading to `action_handler` (`post.rs:92`), serve's `collection_post_inner` (`src/cmd/serve/handlers.rs:396`), and four tests in `post.rs`. Task 2 removes the whole dead chain. The still-live `session_started_at` fields on `ServerState`, `RenderContext`, and `DrillSession` (used by the completion page) are explicitly left alone.
4. **BUG-09 vs. the "failing regression test first" global rule.** A dead-parameter removal has no observable behavior to write a failing test against; Task 2 says so explicitly and relies on the compiler and the existing suite. This is a genuine (if minor) conflict between the spec's global requirement and this item.
5. **BUG-10 citation verified exactly**: `fail("Session interrupted before completion")` is at `src/cmd/drill/server.rs:230`, inside the `start_server` tail at `:218-231`. Note the current code *already* closes the session row on interrupt (`server.rs:224`, added with immediate persistence) — the remaining bug is only the non-zero exit and missing summary, which is what Task 3 fixes (and it upgrades the close from log-and-continue to a propagated `Fallible` error).
6. **FEAT-08 / ROADMAP citation verified**: the stalling arithmetic is `let cards_done = ctx.total_cards - mutable.cards.len();` at `src/cmd/drill/get.rs:90` (ROADMAP says `get.rs:90` — exact). No new session state is needed; the plan derives both numbers from `MutableState.reviews`, which satisfies the spec's demand to "define exactly what state is added" with the answer: none — see Task 4's state decision.
