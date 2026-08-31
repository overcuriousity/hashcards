# Panic Hygiene (BUG-49, BUG-50) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove every panic reachable from request handlers (BUG-49) and eliminate all 48 `.lock().unwrap()` calls by switching session/state mutexes to `parking_lot::Mutex` (BUG-50).

**Architecture:** BUG-49 is a set of independent point fixes: each panic site (`panic!`, indexing, `unwrap`, `expect`, `unreachable!`, `assert!`) is converted to a `Fallible` error, an `Option`, or a logged fallback, per the spec's per-site prescription. BUG-50 is one mechanical sweep: all `std::sync::Mutex` instances holding server state become `parking_lot::Mutex` (no poisoning, `lock()` returns the guard directly), so `.lock().unwrap()` becomes `.lock()` everywhere.

**Tech Stack:** Rust (edition 2024), cargo, axum 0.8, maud, rusqlite; new dependency: `parking_lot` (MIT OR Apache-2.0, permitted by `deny.toml`'s license allowlist).

**Spec:** SPEC.md (items BUG-49, BUG-50; PR group 6 "Panic hygiene")

## Global Constraints

Apply to every item below (from `CLAUDE.md`):

- Every bugfix starts with a failing regression test.
- No `unwrap()` in production code; use `Fallible`, `?`, and `fail()`.
- All error messages are user-facing and clear.
- Reviews and performance are written in one transaction; undo voids, never deletes.
- Cloze positions are byte positions: `.bytes()`, never `.chars()`.
- Update `CHANGELOG.xml` per item.

Additional constraints for this plan:

- PLANNING scope is exactly BUG-49 and BUG-50. Do not fix other bugs you notice (they have their own plans).
- `media/load.rs:46` (`assert!(path.is_absolute())`) is **excluded**: BUG-48 in the security plan (PR group 5) replaces that assert with a `Fallible` error as part of config-path validation. Do not touch it here (see "Spec discrepancies" at the end).
- Tests may use `unwrap()`; production code may not.
- Prefer imports over fully qualified names (`use foo::bar;` then `bar()`).
- Run `cargo test` (full suite) and `cargo clippy` before each commit.

---

## File Structure

No new source files. Files modified:

| File | Task(s) | Change |
|---|---|---|
| `src/cmd/drill/post.rs` | 1, 2, 8 | `Action::grade()` → `Option<Grade>`; Undo `pop()` via `if let`; lock sweep (2 sites) |
| `src/cmd/drill/get.rs` | 3, 4, 8 | empty-queue error; `finished_at` error; new tests module; lock sweep (1 site) |
| `src/parser.rs` | 5 | `unreachable!` → `ParserError` |
| `src/cmd/drill/server.rs` | 6, 7, 8 | epoch `unwrap` → `?`; Ctrl+C `expect` → log fallback; `use parking_lot::Mutex`; lock sweep (1 site) |
| `src/cmd/serve/handlers.rs` | 6, 8 | epoch `unwrap` → `?`; lock sweep (32 sites) |
| `src/cmd/serve/server.rs` | 7, 8 | Ctrl+C `expect` → log fallback; `use parking_lot::Mutex` |
| `src/cmd/drill/state.rs` | 8 | `use parking_lot::Mutex` |
| `src/cmd/serve/state.rs` | 8 | `use parking_lot::Mutex` |
| `src/cmd/serve/edit.rs` | 8 | lock sweep (2 sites) |
| `src/cmd/serve/git.rs` | 8 | `use parking_lot::Mutex`; lock sweep (2 sites) |
| `src/cmd/serve/hedgedoc.rs` | 8 | `use parking_lot::Mutex`; lock sweep (5 sites) |
| `src/cmd/serve/landing.rs` | 8 | lock sweep (3 sites) |
| `Cargo.toml` | 8 | add `parking_lot` |
| `CHANGELOG.xml` | 9 | two `<fixed>` entries |

---

### Task 1: BUG-49 — `Action::grade()` returns `Option<Grade>` instead of panicking

**Files:**
- Modify: `src/cmd/drill/post.rs:49-57` (the `grade` method), `:182` (the caller), `:256-262` (existing test)
- Test: `src/cmd/drill/post.rs` (inline `mod tests`)

**Interfaces:**
- Produces: `impl Action { pub fn grade(&self) -> Option<Grade> }` — returns `Some(Grade)` for `Forgot`/`Hard`/`Good`/`Easy`, `None` for every other action. (This is the only caller-visible signature change in this plan besides the lock types in Task 8.)

- [x] **Step 1: Write the failing test**

In `src/cmd/drill/post.rs`, inside the existing `mod tests`, add:

```rust
    #[test]
    fn test_non_grade_action_returns_none() {
        assert!(Action::Reveal.grade().is_none());
        assert!(Action::Undo.grade().is_none());
        assert!(Action::End.grade().is_none());
    }
```

And update the existing `test_action_grade` to the new signature:

```rust
    #[test]
    fn test_action_grade() {
        assert_eq!(Action::Forgot.grade(), Some(Grade::Forgot));
        assert_eq!(Action::Hard.grade(), Some(Grade::Hard));
        assert_eq!(Action::Good.grade(), Some(Grade::Good));
        assert_eq!(Action::Easy.grade(), Some(Grade::Easy));
    }
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test test_non_grade_action_returns_none`
Expected: FAIL — compile error (`is_none` does not exist on `Grade`; `Some(Grade::Forgot)` does not match return type `Grade`). This is the red stage for a type-signature change; with the old code the call `Action::Reveal.grade()` would panic with "Action does not correspond to a grade".

- [x] **Step 3: Implement**

Replace the `grade` method (`src/cmd/drill/post.rs:49-57`):

```rust
impl Action {
    pub fn grade(&self) -> Option<Grade> {
        match self {
            Action::Forgot => Some(Grade::Forgot),
            Action::Hard => Some(Grade::Hard),
            Action::Good => Some(Grade::Good),
            Action::Easy => Some(Grade::Easy),
            _ => None,
        }
    }
}
```

Update the caller in `handle_action` (`src/cmd/drill/post.rs:182`). Before:

```rust
                let grade: Grade = action.grade();
```

After:

```rust
                let grade: Grade = match action.grade() {
                    Some(grade) => grade,
                    None => return fail("Internal error: this action does not correspond to a grade."),
                };
```

Add the import at the top of `src/cmd/drill/post.rs` (next to `use crate::error::Fallible;`):

```rust
use crate::error::fail;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cmd::drill::post`
Expected: PASS (all tests in the module, including `test_action_grade` and `test_non_grade_action_returns_none`).

- [x] **Step 5: Commit**

```bash
git add src/cmd/drill/post.rs
git commit -m "fix: Action::grade returns Option instead of panicking (BUG-49)"
```

---

### Task 2: BUG-49 — Undo pops the review with `if let`, not `unwrap()`

**Files:**
- Modify: `src/cmd/drill/post.rs:121-139` (the `Action::Undo` arm)
- Test: `src/cmd/drill/post.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `make_mutable()` test helper already present in the tests module; `handle_action` (unchanged signature).
- Produces: nothing new — behavior-preserving refactor.

**Note on reachability:** the `unwrap()` at `post.rs:123` is guarded by `!mutable.reviews.is_empty()`, so the panic is not reachable today; this task removes the `unwrap()` per the no-unwrap rule and adds a behavioral pin. The test below passes both before and after — its job is to pin the no-op semantics while the guard is restructured. Rely on compilation plus the existing suite for the rest.

- [x] **Step 1: Write the pinning test**

In `src/cmd/drill/post.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn test_undo_with_no_reviews_is_noop() {
        let mut mutable = make_mutable();
        let now = Timestamp::now();
        let result = handle_action(&mut mutable, now, Action::Undo).unwrap();
        assert!(matches!(result, ActionResult::Continue));
        assert!(mutable.reviews.is_empty());
        assert!(mutable.cards.is_empty());
    }
```

- [x] **Step 2: Run test to verify it passes (pin, not red)**

Run: `cargo test test_undo_with_no_reviews_is_noop`
Expected: PASS (documents the guarded no-op; the panic site is not test-reachable, as noted above).

- [x] **Step 3: Implement the refactor**

Replace the `Action::Undo` arm (`src/cmd/drill/post.rs:121-139`). Before:

```rust
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
                mutable.db.void_review_and_restore_performance(last_review.review_id, hash, last_review.prev_performance)?;
                mutable.cache.update(hash, last_review.prev_performance)?;
                mutable.finished_at = None;
                mutable.reveal = false;
                mutable.card_shown_at = None;
            }
            Ok(ActionResult::Continue)
        }
```

After:

```rust
        Action::Undo => {
            if let Some(last_review) = mutable.reviews.pop() {
                if last_review.should_repeat() {
                    // Remove the card from the back of the queue.
                    mutable.cards.pop();
                }
                let card: Card = last_review.card;
                let hash: CardHash = card.hash();
                mutable.cards.insert(0, card);
                // Void the review and restore prior performance atomically.
                mutable.db.void_review_and_restore_performance(last_review.review_id, hash, last_review.prev_performance)?;
                mutable.cache.update(hash, last_review.prev_performance)?;
                mutable.finished_at = None;
                mutable.reveal = false;
                mutable.card_shown_at = None;
            }
            Ok(ActionResult::Continue)
        }
```

(The `let last_review: Review = ...` binding moves into the `if let` pattern; everything else is unchanged. Note the DB void + performance restore stays a single atomic call, per the global transaction rule.)

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cmd::drill::post`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add src/cmd/drill/post.rs
git commit -m "fix: undo pops last review with if-let instead of unwrap (BUG-49)"
```

---

### Task 3: BUG-49 — empty card queue renders an error page instead of panicking

**Files:**
- Modify: `src/cmd/drill/get.rs:93` (in `render_session_page`)
- Test: `src/cmd/drill/get.rs` (new inline `mod tests` at the end of the file)

**Interfaces:**
- Consumes: `RenderContext`, `MutableState::new(db, session_id, cache, cards)` (`src/cmd/drill/state.rs`), `AnswerControls` (`src/cmd/drill/server.rs`), `CompletionAction` (this file).
- Produces: `render_session_page` keeps its signature `pub fn render_session_page(ctx: &RenderContext, mutable: &MutableState) -> Fallible<Markup>` but now returns `Err` (never panics) on an empty queue. `get_handler` (drill) and `collection_get_inner` (serve, `src/cmd/serve/handlers.rs:140`) already render `Err` as an error page — no caller changes needed.

- [x] **Step 1: Write the failing test**

`src/cmd/drill/get.rs` has no tests module yet. Add one at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::cmd::drill::cache::Cache;
    use crate::db::Database;

    fn make_mutable() -> MutableState {
        let db = Database::new(":memory:").unwrap();
        let session_id = db.create_session(Timestamp::now()).unwrap();
        MutableState::new(db, session_id, Cache::new(), Vec::new())
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
        let mutable = make_mutable();
        let ctx = make_ctx(Path::new("."));
        let result = render_session_page(&ctx, &mutable);
        assert!(result.is_err());
    }
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test test_render_session_page_with_empty_queue_is_error`
Expected: FAIL — the test panics with "index out of bounds: the len is 0" (from `mutable.cards[0]` at `get.rs:93`) instead of returning an `Err`.

- [x] **Step 3: Implement**

In `render_session_page` (`src/cmd/drill/get.rs:93`). Before:

```rust
    let card = mutable.cards[0].clone();
```

After:

```rust
    let card: Card = match mutable.cards.first() {
        Some(card) => card.clone(),
        None => return fail("No cards are left in the queue. The session may already be finished."),
    };
```

Add the import at the top of `src/cmd/drill/get.rs` (next to `use crate::error::Fallible;`):

```rust
use crate::error::fail;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cmd::drill::get`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add src/cmd/drill/get.rs
git commit -m "fix: empty card queue renders error page instead of panicking (BUG-49)"
```

---

### Task 4: BUG-49 — completion page errors when `finished_at` is unset

**Files:**
- Modify: `src/cmd/drill/get.rs:249` (in `render_completion_page`)
- Test: `src/cmd/drill/get.rs` (the `mod tests` created in Task 3)

**Interfaces:**
- Consumes: `make_mutable()` and `make_ctx()` helpers from Task 3's tests module; the `fail` import added in Task 3.
- Produces: `render_completion_page` keeps its signature `pub fn render_completion_page(ctx: &RenderContext, mutable: &MutableState) -> Fallible<Markup>` but returns `Err` instead of panicking when `mutable.finished_at` is `None`.

- [x] **Step 1: Write the failing test**

In the `mod tests` of `src/cmd/drill/get.rs`, add:

```rust
    #[test]
    fn test_render_completion_page_without_finished_at_is_error() {
        let mutable = make_mutable();
        assert!(mutable.finished_at.is_none());
        let ctx = make_ctx(Path::new("."));
        let result = render_completion_page(&ctx, &mutable);
        assert!(result.is_err());
    }
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test test_render_completion_page_without_finished_at_is_error`
Expected: FAIL — the test panics with "called `Option::unwrap()` on a `None` value" (from `mutable.finished_at.unwrap()` at `get.rs:249`).

- [x] **Step 3: Implement**

In `render_completion_page` (`src/cmd/drill/get.rs:249`). Before:

```rust
    let end = mutable.finished_at.unwrap().into_inner();
```

After:

```rust
    let finished_at = match mutable.finished_at {
        Some(finished_at) => finished_at,
        None => return fail("The completion page was requested before the session finished."),
    };
    let end = finished_at.into_inner();
```

(The `use crate::error::fail;` import already exists from Task 3.)

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cmd::drill::get`
Expected: PASS (both new tests).

- [x] **Step 5: Commit**

```bash
git add src/cmd/drill/get.rs
git commit -m "fix: completion page returns error when session is unfinished (BUG-49)"
```

---

### Task 5: BUG-49 — parser `unreachable!` becomes a `ParserError`

**Files:**
- Modify: `src/parser.rs:456` (the `State::End` arm of `parse_line`)
- Test: `src/parser.rs` (existing `mod tests`, which already has a `make_test_parser()` helper)

**Interfaces:**
- Consumes: `ParserError::new(message, file_path: PathBuf, line_num: usize)` (`src/parser.rs:162`), private `State`/`Line` enums (test lives in the same file so it can use them), `make_test_parser()` from the existing tests module.
- Produces: `parse_line` keeps its signature `fn parse_line(&self, state: State, line: Line, line_num: usize, cards: &mut Vec<Card>) -> Result<State, ParserError>` but the `State::End` arm returns `Err` instead of panicking.

**Note on reachability:** `parse()` never feeds a line after `State::End`, so this is a defensive internal error — but `parse_line` is a private method callable from the same-file tests module, so the panic IS test-reachable directly.

- [x] **Step 1: Write the failing test**

In `src/parser.rs`, inside the existing `mod tests`, add:

```rust
    #[test]
    fn test_line_after_end_state_is_error() {
        let parser = make_test_parser();
        let mut cards = Vec::new();
        let result = parser.parse_line(State::End, Line::Eof, 0, &mut cards);
        assert!(result.is_err());
    }
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test test_line_after_end_state_is_error`
Expected: FAIL — panics with "internal error: entered unreachable code: Parsed a line after the end of the file."

- [x] **Step 3: Implement**

In `parse_line` (`src/parser.rs:456`). Before:

```rust
            State::End => unreachable!("Parsed a line after the end of the file."),
```

After:

```rust
            State::End => Err(ParserError::new(
                "Internal parser error: a line was parsed after the end of the file.",
                self.file_path.clone(),
                line_num,
            )),
```

(`ParserError` is defined in the same file; no new imports. The message renders through `ParserError`'s `Display`, which appends "Location: file:line" — user-facing and clear.)

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib parser`
Expected: PASS (all parser tests, including the new one).

- [x] **Step 5: Commit**

```bash
git add src/parser.rs
git commit -m "fix: parser returns error instead of unreachable! after EOF (BUG-49)"
```

---

### Task 6: BUG-49 — `duration_since(UNIX_EPOCH).unwrap()` becomes `?`

**Files:**
- Modify: `src/cmd/drill/server.rs:150-153` (shuffle seed), `src/cmd/serve/handlers.rs:294-297` (shuffle seed)
- Test: none reachable — see note.

**Interfaces:**
- Consumes: `ErrorReport::new(msg: impl Into<String>)` (`src/error.rs:29`).
- Produces: nothing new — both sites are inside functions that already return `Fallible` (`start_server` in drill `server.rs`, the session-building helper in serve `handlers.rs`), so `?` propagates naturally.

**Note on reachability:** this panic fires only when the system clock is set before the Unix epoch (1970). That cannot be simulated from a test without mocking `SystemTime`, which the codebase does not do. Not test-reachable — rely on compilation and the existing suite (which exercises both code paths with a sane clock).

- [x] **Step 1: Implement in `src/cmd/drill/server.rs`**

Before (`src/cmd/drill/server.rs:149-154`):

```rust
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
```

After:

```rust
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| ErrorReport::new(format!("The system clock is set before the Unix epoch: {e}")))?
            .as_nanos() as u64;
```

Add the import at the top of `src/cmd/drill/server.rs` (next to `use crate::error::Fallible;`):

```rust
use crate::error::ErrorReport;
```

- [x] **Step 2: Implement in `src/cmd/serve/handlers.rs`**

Before (`src/cmd/serve/handlers.rs:294-297`):

```rust
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
```

After:

```rust
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| ErrorReport::new(format!("The system clock is set before the Unix epoch: {e}")))?
        .as_nanos() as u64;
```

Add the import at the top of `src/cmd/serve/handlers.rs` (next to `use crate::error::Fallible;`) if not already present:

```rust
use crate::error::ErrorReport;
```

- [x] **Step 3: Verify compilation and run the suite**

Run: `cargo test`
Expected: PASS (no behavior change with a sane clock; the change is compile-verified).

- [x] **Step 4: Commit**

```bash
git add src/cmd/drill/server.rs src/cmd/serve/handlers.rs
git commit -m "fix: propagate system clock errors instead of unwrapping epoch time (BUG-49)"
```

---

### Task 7: BUG-49 — Ctrl+C handler `.expect` becomes a logged fallback

**Files:**
- Modify: `src/cmd/drill/server.rs:314-320` (`shutdown_signal`), `src/cmd/serve/server.rs:300-305` (`shutdown_signal`)
- Test: none reachable — see note.

**Interfaces:**
- Consumes: `tokio::signal::ctrl_c` (already imported as `signal` in both files), `std::future::pending`.
- Produces: nothing new — both `shutdown_signal` functions keep their signatures.

**Note on reachability and spec deviation:** installing a Ctrl+C handler only fails in exotic environments (no signal infrastructure); it cannot be triggered from a test. Not test-reachable — rely on compilation and the existing suite. Also: the spec says "`.expect` on Ctrl+C handler → `?`", but both `shutdown_signal` functions return `()` (they are futures handed to `axum::serve(...).with_graceful_shutdown(...)`), so `?` is impossible without redesigning the graceful-shutdown plumbing. Instead: log the error and park the future forever (`pending()`), so a failed handler install disables graceful Ctrl+C shutdown without killing the server. In drill mode the Shutdown-button channel branch of the `select!` still works.

- [x] **Step 1: Implement in `src/cmd/drill/server.rs`**

Before (`src/cmd/drill/server.rs:315-319`):

```rust
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
```

After:

```rust
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            log::error!("Failed to install Ctrl+C handler; graceful shutdown on Ctrl+C is disabled: {e}");
            pending::<()>().await;
        }
    };
```

Add the import at the top of `src/cmd/drill/server.rs`:

```rust
use std::future::pending;
```

- [x] **Step 2: Implement in `src/cmd/serve/server.rs`**

Before (`src/cmd/serve/server.rs:300-305`):

```rust
async fn shutdown_signal() {
    signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
    log::debug!("Received shutdown signal");
}
```

After:

```rust
async fn shutdown_signal() {
    if let Err(e) = signal::ctrl_c().await {
        log::error!("Failed to install Ctrl+C handler; graceful shutdown on Ctrl+C is disabled: {e}");
        pending::<()>().await;
    }
    log::debug!("Received shutdown signal");
}
```

Add the import at the top of `src/cmd/serve/server.rs`:

```rust
use std::future::pending;
```

- [x] **Step 3: Verify compilation and run the suite**

Run: `cargo test`
Expected: PASS. Also run: `cargo clippy` — expected: no new warnings.

- [x] **Step 4: Commit**

```bash
git add src/cmd/drill/server.rs src/cmd/serve/server.rs
git commit -m "fix: log Ctrl+C handler failure instead of panicking (BUG-49)"
```

---

### Task 8: BUG-50 — replace all 48 `.lock().unwrap()` with `parking_lot::Mutex`

**Files:**
- Modify: `Cargo.toml` (add dependency)
- Modify (import swap, `use std::sync::Mutex;` → `use parking_lot::Mutex;`): `src/cmd/drill/state.rs:17`, `src/cmd/serve/state.rs:4`, `src/cmd/drill/server.rs:20`, `src/cmd/serve/server.rs:4`, `src/cmd/serve/git.rs:3`, `src/cmd/serve/hedgedoc.rs:4`
- Modify (call-site sweep, `.lock().unwrap()` → `.lock()`): `src/cmd/drill/get.rs` (1 site: `:67`), `src/cmd/drill/server.rs` (1 site: `:219`), `src/cmd/drill/post.rs` (2 sites: `:91`, `:97`), `src/cmd/serve/edit.rs` (2 sites: `:60`, `:148`), `src/cmd/serve/git.rs` (2 sites: `:142`, `:185`), `src/cmd/serve/handlers.rs` (32 sites: `:61`, `:85`, `:97`, `:145`, `:153`, `:234`, `:239`, `:360`, `:373`, `:386`, `:398`, `:460`, `:489`, `:492`, `:509`, `:510`, `:558`, `:577`, `:608`, `:637`, `:652`, `:675`, `:686`, `:696`, `:714`, `:726`, `:742`, `:758`, `:775`, `:788`, `:799`, `:803`), `src/cmd/serve/hedgedoc.rs` (5 sites: `:398`, `:415`, `:430`, `:442`, `:446`), `src/cmd/serve/landing.rs` (3 sites: `:14`, `:15`, `:17`)
- Test: `src/cmd/drill/post.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `Arc<Mutex<MutableState>>` / `Arc<Mutex<Option<Sender<()>>>>` fields on drill `ServerState` (`src/cmd/drill/state.rs:36-37`); `sessions`, `last_synced`, `hedgedoc_sources`, `hedgedoc_last_synced`, `config_path` fields on serve `AppState` (`src/cmd/serve/state.rs:18-22`); `Arc<Mutex<...>>` parameters in `src/cmd/serve/git.rs:115-116` and `src/cmd/serve/hedgedoc.rs:376-378`.
- Produces: every one of those `Mutex` types becomes `parking_lot::Mutex`. `lock()` now returns `MutexGuard` directly (no `Result`, no poisoning). All field names, function signatures (textually), and guard usage patterns are unchanged — only the `Mutex` type behind the identical `Arc<Mutex<T>>` spelling changes via the import.

**Why `parking_lot` rather than the `lock_or_fail` helper:** the spec offers both. A `fn lock_or_fail<T>(m: &Mutex<T>) -> Fallible<MutexGuard<'_, T>>` helper needs `?` at every call site, but many sites live in non-`Fallible` contexts: `landing_handler` returns `(StatusCode, Html<String>)`, `find_collection` returns `Option<ResolvedCollection>`, and the git/hedgedoc periodic-sync loops in spawned tasks return nothing — the helper would force signature changes rippling through handlers and background tasks. `parking_lot::Mutex` has no poisoning by design, so the sweep is purely mechanical and the poisoning-cascade failure mode (spec: "in debug, poisoning cascades"; with `panic = "abort"` in release the process dies under any panic regardless of mutex choice) ceases to exist. Dependency policy check: `Cargo.toml` has no dependency restrictions; `deny.toml` allows MIT and Apache-2.0, and `parking_lot` is dual-licensed "MIT OR Apache-2.0".

- [x] **Step 1: Write the failing regression test**

In `src/cmd/drill/post.rs`, inside `mod tests`, add (this reproduces the poisoning cascade: with `std::sync::Mutex`, a panic while holding the lock poisons it and every later `lock().unwrap()` panics):

```rust
    #[test]
    fn test_lock_usable_after_panicked_holder() {
        use std::sync::Arc;

        use parking_lot::Mutex;

        // Same shape as ServerState.mutable (Arc<Mutex<MutableState>>).
        let mutable = Arc::new(Mutex::new(make_mutable()));
        let m2 = Arc::clone(&mutable);
        let _ = std::thread::spawn(move || {
            let _guard = m2.lock();
            panic!("simulated handler panic while holding the state lock");
        })
        .join();
        // Without poisoning, the lock must still be usable afterwards.
        let guard = mutable.lock();
        assert!(guard.cards.is_empty());
    }
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test test_lock_usable_after_panicked_holder`
Expected: FAIL — compile error `unresolved import parking_lot` (the crate is not yet a dependency). This is the red stage: the same scenario written against `std::sync::Mutex` today would panic on the post-poison `lock().unwrap()`.

- [x] **Step 3: Add the dependency**

In `Cargo.toml`, in the `[dependencies]` section (alphabetical order, after `open`):

```toml
parking_lot = "0.12"
```

Run: `cargo build`
Expected: builds; `parking_lot` appears in `Cargo.lock`.

- [x] **Step 4: Swap the imports (Pattern 1)**

In each of these six files, change the `Mutex` import. Before:

```rust
use std::sync::Mutex;
```

After:

```rust
use parking_lot::Mutex;
```

Files and lines:
- `src/cmd/drill/state.rs:17`
- `src/cmd/serve/state.rs:4`
- `src/cmd/drill/server.rs:20`
- `src/cmd/serve/server.rs:4`
- `src/cmd/serve/git.rs:3`
- `src/cmd/serve/hedgedoc.rs:4`

(`Mutex::new(...)` construction sites in `drill/server.rs:179,189` and `serve/server.rs:156,170,171,176,177` need no textual change — the import swap retypes them.)

- [x] **Step 5: Sweep the call sites (Pattern 2)**

Mechanical replacement of `.lock().unwrap()` with `.lock()` in every file listed in **Files** above:

```bash
grep -rl '\.lock()\.unwrap()' src/ | xargs sed -i 's/\.lock()\.unwrap()/.lock()/g'
```

Fully worked examples, one per distinct usage pattern:

**(a) Guard held in a binding** — `src/cmd/drill/post.rs:91-97` (`action_handler`). Before:

```rust
    let mut mutable = state.mutable.lock().unwrap();
    let result = handle_action(&mut mutable, state.session_started_at, action)?;
    match result {
        ActionResult::Shutdown => {
            // Release the lock before sending shutdown signal.
            drop(mutable);
            let mut shutdown_tx = state.shutdown_tx.lock().unwrap();
```

After:

```rust
    let mut mutable = state.mutable.lock();
    let result = handle_action(&mut mutable, state.session_started_at, action)?;
    match result {
        ActionResult::Shutdown => {
            // Release the lock before sending shutdown signal.
            drop(mutable);
            let mut shutdown_tx = state.shutdown_tx.lock();
```

**(b) Temporary guard, method chained on it** — `src/cmd/serve/handlers.rs:85`. Before:

```rust
    let session = state.sessions.lock().unwrap().remove(slug);
```

After:

```rust
    let session = state.sessions.lock().remove(slug);
```

**(c) Deref-copy out of the guard** — `src/cmd/serve/landing.rs:14`. Before:

```rust
    let last_synced = *state.last_synced.lock().unwrap();
```

After:

```rust
    let last_synced = *state.last_synced.lock();
```

**(d) Assignment through the guard** — `src/cmd/serve/git.rs:185`. Before:

```rust
            *last_synced.lock().unwrap() = Some(Timestamp::now());
```

After:

```rust
            *last_synced.lock() = Some(Timestamp::now());
```

Every one of the 48 sites is an instance of patterns (a)-(d); the `sed` sweep covers them all identically.

- [x] **Step 6: Verify the sweep is complete**

Run: `grep -rn '\.lock()\.unwrap()' src/`
Expected: no output.

Run: `grep -rn 'std::sync::Mutex' src/`
Expected: no output (only `parking_lot::Mutex` remains; `std::sync::Arc`, `RwLock` via tokio, etc. are untouched).

- [x] **Step 7: Run the full suite**

Run: `cargo test`
Expected: PASS, including `test_lock_usable_after_panicked_holder` (the spawned thread's panic prints a backtrace in test output — that is expected; the test itself passes).

Run: `cargo clippy`
Expected: no new warnings.

- [x] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/cmd/drill/state.rs src/cmd/serve/state.rs src/cmd/drill/server.rs src/cmd/serve/server.rs src/cmd/drill/get.rs src/cmd/drill/post.rs src/cmd/serve/edit.rs src/cmd/serve/git.rs src/cmd/serve/handlers.rs src/cmd/serve/hedgedoc.rs src/cmd/serve/landing.rs
git commit -m "fix: switch state mutexes to parking_lot, removing 48 lock().unwrap() calls (BUG-50)"
```

---

### Task 9: CHANGELOG entries and final verification

**Files:**
- Modify: `CHANGELOG.xml` (the `<fixed>` block inside `<unreleased>`)

**Interfaces:**
- Consumes: the existing `<unreleased><fixed>` element at the top of `CHANGELOG.xml` (format defined by `CHANGELOG.xsd`: `<change>` elements with an optional `author` attribute, free-text body).
- Produces: nothing consumed by later tasks (this is the last task).

- [x] **Step 1: Add the changelog entries**

In `CHANGELOG.xml`, inside the existing `<unreleased>` → `<fixed>` element (after the last existing `<change>` in that block), add:

```xml
            <change author="claude">
                Removed panics reachable from web request handlers: an empty card queue and a premature completion-page request now render an error page instead of crashing the server; internal parser and grade-action inconsistencies, system-clock errors, and Ctrl+C handler failures are reported as errors instead of panicking.
            </change>
            <change author="claude">
                Session and sync state locks no longer use `.lock().unwrap()`: a panic in one request can no longer poison the locks and cascade into crashes of every subsequent request.
            </change>
```

(Indentation: 12 spaces for `<change>`, matching the existing entries. Do not add a new `<fixed>` block — reuse the existing one.)

- [x] **Step 2: Validate the changelog against the schema**

Run: `xmllint --noout --schema CHANGELOG.xsd CHANGELOG.xml`
Expected: `CHANGELOG.xml validates`. (If `xmllint` is not installed, run `python3 -c "import xml.etree.ElementTree as ET; ET.parse('CHANGELOG.xml'); print('well-formed')"` as a well-formedness fallback.)

- [x] **Step 3: Final full verification**

Run: `cargo test`
Expected: PASS (entire suite).

Run: `cargo clippy`
Expected: no warnings introduced by this plan.

Run: `grep -rn 'panic!\|unreachable!\|\.expect(\|\.unwrap()' src/cmd/drill/post.rs src/cmd/drill/get.rs src/parser.rs src/cmd/drill/server.rs src/cmd/serve/handlers.rs src/cmd/serve/server.rs src/cmd/serve/landing.rs src/cmd/serve/edit.rs src/cmd/serve/git.rs src/cmd/serve/hedgedoc.rs`
Expected: every remaining hit lies inside a `#[cfg(test)] mod tests` block (check each hit's line number against the file). No production-code hits remain among the BUG-49 sites; `edit.rs:52` (`strip_prefix(...).unwrap_or(...)`) is `unwrap_or`, not `unwrap()`, and is fine.

- [x] **Step 4: Commit**

```bash
git add CHANGELOG.xml
git commit -m "docs: changelog entries for panic hygiene fixes (BUG-49, BUG-50)"
```

---

## Spec discrepancies

Verified every cited line against the working tree (commit `be55f80`):

1. **BUG-50 count:** the spec says "~45 `.lock().unwrap()` calls"; the actual count is **48**, across 8 files (`handlers.rs` 32, `hedgedoc.rs` 5, `landing.rs` 3, `post.rs` 2, `edit.rs` 2, `git.rs` 2, `get.rs` 1, drill `server.rs` 1). All line numbers are enumerated in Task 8.
2. **BUG-49 `server.rs:318` / serve `server.rs:301-303` "→ `?`":** `?` is not applicable — both `shutdown_signal` functions return `()` and are consumed by `with_graceful_shutdown`, which takes a plain future. Task 7 substitutes a logged fallback (`log::error!` + `pending()`), which honors the intent (no panic) without redesigning the shutdown plumbing.
3. **BUG-49 `media/load.rs:46` `assert!`:** confirmed present. Excluded here per BUG-48: the security plan (PR group 5) replaces the assert with a `Fallible` error as part of config-path validation. The change itself would be small (only two production callers, `drill/server.rs:275` and `serve/handlers.rs:417`, both in `Fallible` functions), but planning it in both PRs would produce conflicting edits to the same lines, so it lands in the security plan only.
4. **Naming:** the spec's suggested helper signature mentions `MutexGuard`; this plan chooses the spec's alternative (`parking_lot::Mutex`) instead — rationale in Task 8 (many call sites are in non-`Fallible` contexts).
5. **All other cited lines verified exact:** `post.rs:55` (`panic!`), `post.rs:123` (`pop().unwrap()`), `get.rs:93` (`cards[0]`), `get.rs:249` (`finished_at.unwrap()`), `parser.rs:456` (`unreachable!`, in a method named `parse_line`, not "process_line"), drill `server.rs:152` (epoch `unwrap`), serve `handlers.rs:294-297` (epoch `unwrap` at `:296`), drill `server.rs:318` (`expect`), serve `server.rs:303` (`expect`), and the serve-side caller of `render_session_page` at `handlers.rs:140`.
