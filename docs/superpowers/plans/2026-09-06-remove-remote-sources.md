# Removing Remote Sources Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete `[git]`, `[[source]]`/`[[hedgedoc]]`, `commit_edit` and `[[collection]]`, collapsing three collection kinds into one discovered card tree, and spend the simplification on editing a card from inside a running drill session.

**Architecture:** Two halves, in order. First the subtraction (Tasks 1–5): the count helpers move out of `git.rs`, then `[git]`, then HedgeDoc/`[[source]]`, then `[[collection]]`, then the `local/` → `cards/` rename. Each deletion task must leave `cargo test` green — the suite that survives is the proof. Then the addition (Tasks 6–10): an infallible `MutableState::apply_card_migration` re-keys a live session's four card-identity structures, `sessions_touching` finds the sessions to migrate by asking the sessions what they loaded rather than asking the config, and the card editor and whole-file editor both call it after their database transaction commits.

**Tech Stack:** Rust 2024, axum 0.8, maud, rusqlite (bundled SQLite), tokio, parking_lot, blake3.

**Spec:** `docs/superpowers/specs/2026-09-06-remove-remote-sources-design.md` — read it before starting. Every task below argues from it.

## Global Constraints

Copied from `CLAUDE.md`; these apply to every task and are not repeated per task.

- No `unwrap()` in production code. Tests may use it.
- Error handling is `Fallible<T>` and `?`. Create errors with `fail(...)`, which returns `Err`. All error messages are user-facing: write them for the person reading the page.
- Prefer imports to fully qualified names: add `use foo::bar;` rather than writing `foo::bar()`.
- Newtypes for domain concepts. Keep functions small and focused. Module files re-export what is needed and hide the rest.
- When fixing a bug, write the failing regression test **first**.
- Cloze deletion positions are **byte** positions. Use `.bytes()`, never `.chars()`.
- Dates are naive on purpose. No timezones.
- Update `CHANGELOG.xml` when a change is user-visible. New entries go in the `<unreleased>` block, under `<changed>`, `<added>`, `<fixed>` or `<removed>`, each as `<change author="claude">…</change>`.
- Verification commands, run at the end of every task:
  - `cargo fmt`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test`
- Baseline: **478 tests pass** on `feat/ui-consolidation` at commit `fa43b8f`. Record the number after each task; a drop must be explained entirely by tests deleted with their files.
- Nothing is deployed. There is no compatibility surface: no config shims, no deprecation window, no data migration. Delete rather than deprecate.

## Deviations from the spec

Three things in the spec do not survive contact with the code. Each is resolved here; the resolution is the instruction.

1. **`href.rs` is not deleted.** The spec says it "exists only to render external source URLs safely; both callers are source pages". That is true of `safe_href`, but `href.rs` also holds `encoded_path`, which has three live callers (`files.rs:792`, `files_ui.rs:67`, `files_ui.rs:138`) and gains a fourth in Task 10. **Delete `safe_href` and its two tests; keep `encoded_path` and its test.** `href.rs` shrinks from 84 lines to about 40.

2. **`git.rs` holds two functions that survive.** `refresh_collection_info` and `compute_collection_counts` are used by `landing.rs` and `hedgedoc.rs`, and are about counting cards, not about git. Task 1 moves them to a new `counts.rs` before anything is deleted.

3. **`AppState` goes from twelve fields to seven, not nine.** The spec's count drops `hedgedoc_sources`, `last_synced` and `hedgedoc_last_synced`. But `collections: Arc<RwLock<Vec<CollectionInfo>>>` and `counts_refreshed_at` exist only to cache the counts of *configured and HedgeDoc* collections: every write to them goes through `build_combined_infos`, which is deleted with `hedgedoc.rs`, and discovered collections are already counted per request in `landing_handler`. Keeping a cache nothing writes is dead code, so **`collections` and `counts_refreshed_at` go too.** Surviving fields: `config`, `sessions`, `custom_decks`, `config_path`, `interrupted_closed`, `session_key`, `oidc`.

Two gaps the spec does not address, filled here:

4. **The startup sweep loses its collection list.** `sweep_dangling_sessions` (`server.rs:114`) iterates `config.collections` chained with the HedgeDoc sources — after the cut, both are empty, and FEAT-03 (reporting sessions a crash left open) silently stops working. Task 4 adds `discover_all_collections`, which walks every user's tree under `{data_dir}/cards/`, and re-keys `interrupted_closed` by database path instead of by slug so two users' identically-named collections cannot claim each other's notice.

5. **A topic's edit link cannot be derived from its name.** The spec says the browse page's replacement link "points at `/files/edit/{path}`". A `DeckNode.path` is the deck *name*, which defaults to the file's path relative to the collection but is overridden by a file's frontmatter `name:` (`parser.rs:146`). Task 10 therefore carries the real file path from `build_deck_tree`, where the cards are still in hand, rather than reconstructing it from the name.

---

# Part one: the subtraction

## Task 1: Move the count helpers out of `git.rs`

`git.rs` is deleted in Task 2, but two of its functions have nothing to do with git and are called from elsewhere. Move them first, on their own, so Task 2's diff is pure deletion.

**Files:**
- Create: `src/cmd/serve/counts.rs`
- Modify: `src/cmd/serve/git.rs` (remove the two functions and the one test that covers them)
- Modify: `src/cmd/serve/mod.rs:10` (add `mod counts;` next to `mod git;`)
- Modify: `src/cmd/serve/landing.rs:16` (import path)
- Modify: `src/cmd/serve/hedgedoc.rs:16` (import path)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub fn refresh_collection_info(collections: &[ResolvedCollection]) -> Vec<CollectionInfo>`
  - `pub fn compute_collection_counts(coll_dir: &Path, db_path: &Path) -> Fallible<(usize, usize)>`

  Both in `crate::cmd::serve::counts`, with identical signatures and bodies to the ones in `git.rs:60` and `git.rs:82` today. Every later task uses `refresh_collection_info` from this module.

- [ ] **Step 1: Create `src/cmd/serve/counts.rs`**

```rust
//! Card and due-card counts for a collection.
//!
//! Counting reads the collection off disk and its schedule out of SQLite,
//! which is why it is not done inline in a handler. It lived in `git.rs`
//! because the git sync task was once the only thing that refreshed it.

use std::path::Path;

use crate::cmd::serve::config::ResolvedCollection;
use crate::cmd::serve::state::CollectionInfo;
use crate::collection::Collection;
use crate::error::Fallible;
use crate::types::date::Date;
use crate::types::timestamp::Timestamp;

/// Count every collection, reporting a failure as zero rather than taking
/// the whole listing down: one unreadable collection must not empty the
/// page for the others.
pub fn refresh_collection_info(collections: &[ResolvedCollection]) -> Vec<CollectionInfo> {
    let mut infos = Vec::new();
    for rc in collections {
        let (total_cards, due_today) = match compute_collection_counts(&rc.coll_dir, &rc.db_path) {
            Ok(counts) => counts,
            Err(e) => {
                log::warn!("Failed to load collection '{}': {e}", rc.name);
                (0, 0)
            }
        };

        infos.push(CollectionInfo {
            name: rc.name.clone(),
            slug: rc.slug.clone(),
            total_cards,
            due_today,
            owner: rc.owner.clone(),
        });
    }
    infos
}

/// `(total cards, cards due today)`, inserting any card the database has
/// not seen before so a freshly written card is counted from the moment it
/// exists.
pub fn compute_collection_counts(coll_dir: &Path, db_path: &Path) -> Fallible<(usize, usize)> {
    if !coll_dir.exists() {
        return Ok((0, 0));
    }

    let collection = Collection::with_db_path(coll_dir.to_path_buf(), db_path.to_path_buf())?;
    let total_cards = collection.cards.len();

    let today: Date = Timestamp::now().date();

    let db_hashes = collection.db.card_hashes()?;
    let now = Timestamp::now();
    for card in collection.cards.iter() {
        if !db_hashes.contains(&card.hash()) {
            collection.db.insert_card(card.hash(), now)?;
        }
    }

    let due_hashes = collection.db.due_today(today)?;
    let due_today = collection
        .cards
        .iter()
        .filter(|c| due_hashes.contains(&c.hash()))
        .count();

    Ok((total_cards, due_today))
}
```

- [ ] **Step 2: Move the covering test into `counts.rs`**

Cut `test_refresh_collection_info_carries_owner` out of `git.rs` (it starts at `git.rs:307`) and paste it into a `#[cfg(test)] mod tests` at the bottom of `counts.rs`, adding whatever `use` lines it needs (`tempfile::tempdir`, `std::path::PathBuf`, `crate::error::Fallible`). Do not change its body.

- [ ] **Step 3: Delete the two functions from `git.rs`**

Remove `refresh_collection_info` (lines 60–80) and `compute_collection_counts` (lines 82–112) from `src/cmd/serve/git.rs`, and add at the top of the file:

```rust
use crate::cmd::serve::counts::compute_collection_counts;
```

so `spawn_sync_task` (which calls it at `git.rs:178`) still compiles.

- [ ] **Step 4: Register the module and fix the two import sites**

In `src/cmd/serve/mod.rs`, add `mod counts;` in alphabetical position (after `mod config;`).

In `src/cmd/serve/landing.rs`, change:

```rust
use crate::cmd::serve::git::refresh_collection_info;
```
to:
```rust
use crate::cmd::serve::counts::refresh_collection_info;
```

In `src/cmd/serve/hedgedoc.rs`, change:

```rust
use crate::cmd::serve::git::compute_collection_counts;
```
to:
```rust
use crate::cmd::serve::counts::compute_collection_counts;
```

- [ ] **Step 5: Verify**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS, 478 tests. A pure move changes no count.

- [ ] **Step 6: Commit**

```bash
git add src/cmd/serve/counts.rs src/cmd/serve/git.rs src/cmd/serve/mod.rs src/cmd/serve/landing.rs src/cmd/serve/hedgedoc.rs
git commit -m "refactor: move collection counting out of git.rs

Counting a collection reads markdown and SQLite; it has nothing to do
with git, and lived there only because the sync task was once the only
caller. Moving it first keeps the [git] removal a pure deletion.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RP3W1RufSmp3GWtDuxC763"
```

---

## Task 2: Remove `[git]`

Deletes the repo clone, the poll loop and `commit_edit`. After this the server makes no `git` subprocess call and no outbound request except HedgeDoc's (removed next) and OIDC's.

`[[collection]]` still resolves inside `{data_dir}/repo` after this task — that directory is simply never populated. Task 4 removes the whole notion.

**Files:**
- Delete: `src/cmd/serve/git.rs`
- Modify: `src/cmd/serve/mod.rs` (drop `mod git;`)
- Modify: `src/cmd/serve/config.rs` — `GitSection`, `ResolvedGit`, `ServeConfig.git`, `ResolvedServeConfig.git`, `default_branch`, `default_poll_interval`, `default_commit_author_name`, `default_commit_author_email`
- Modify: `src/cmd/serve/server.rs` — the `sync_git` block, `/sync` route, `spawn_sync_task` call, `last_synced`, `ensure_dir(git.repo_dir)`
- Modify: `src/cmd/serve/handlers.rs` — `sync_handler` and its imports
- Modify: `src/cmd/serve/state.rs` — `last_synced` field, both `test_support` constructors
- Modify: `src/cmd/serve/landing.rs` — `git_enabled`, `last_synced`, the `Sync` nav form, the `git` branch of `status_line`, the poll-interval derivation
- Modify: `src/cmd/serve/edit.rs` — the `commit_edit` call, the author-resolution block, `EditOutcome.committed`, the "Committed to git." suffix
- Modify: `src/cmd/serve/hedgedoc.rs` — `hedgedoc_poll_minutes` no longer inherits from git
- Modify: `src/cmd/serve/auth.rs` — the six `ResolvedServeConfig` literals lose `git: None`
- Modify: `src/cmd/serve/mod.rs` tests, `src/cmd/serve/decks.rs` tests, `src/cmd/serve/handlers.rs` tests — every `ResolvedServeConfig` literal loses `git: None`
- Modify: `Cargo.toml` — drop tokio's `process` feature
- Modify: `README.md` — the `### [git]` section and the `{data_dir}` description
- Modify: `hashcards.example.toml` — the `[git]` block

**Interfaces:**
- Consumes: `crate::cmd::serve::counts::{refresh_collection_info, compute_collection_counts}` from Task 1.
- Produces:
  - `ResolvedServeConfig` no longer has a `git` field. Every literal construction of it in tests must drop `git: None`.
  - `AppState` no longer has `last_synced`.
  - `EditOutcome` is `{ migrated: usize, skipped: usize }`. Task 8 replaces it again.

- [ ] **Step 1: Delete `git.rs` and unregister it**

```bash
git rm src/cmd/serve/git.rs
```

Remove the line `mod git;` from `src/cmd/serve/mod.rs`.

- [ ] **Step 2: Strip git out of `config.rs`**

Delete these items from `src/cmd/serve/config.rs`:
- the `git: Option<GitSection>` field of `ServeConfig` (lines 21–22)
- `pub struct GitSection` and the four `default_*` functions under it (lines 125–156)
- `pub struct ResolvedGit` (lines 254–262)
- the `pub git: Option<ResolvedGit>` field of `ResolvedServeConfig`
- the `let git = match config.git { … }` block in `from_toml` and the `git,` entry in the returned literal
- the `git: None,` entry in `from_directories`'s returned literal

Keep `let repo_dir = data_dir.join("repo");` in `from_toml` — `[[collection]]` still resolves inside it until Task 4.

- [ ] **Step 3: Strip git out of `server.rs`**

In `src/cmd/serve/server.rs`:
- delete the imports `clone_or_pull`, `spawn_sync_task`, `sync_handler`, `ResolvedGit`
- delete the whole `let sync_git = match &config.git { … };` block (lines 210–230)
- delete `let last_synced = if config.git.is_some() { … };` (lines 260–264)
- replace the git-derived poll interval

```rust
    let hedgedoc_poll_minutes = config
        .git
        .as_ref()
        .map(|g| g.poll_interval_minutes)
        .unwrap_or(30);
```
with the constant it always fell back to:
```rust
    // HedgeDoc notes used to inherit the git poll interval. With no git
    // remote left there is nothing to inherit from, so the old fallback is
    // now simply the interval.
    let hedgedoc_poll_minutes = HEDGEDOC_POLL_MINUTES;
```
and add near `SESSION_STALE_MINUTES`:
```rust
/// How often HedgeDoc notes are re-fetched, in minutes.
const HEDGEDOC_POLL_MINUTES: u64 = 30;
```
- delete `last_synced: Arc::new(Mutex::new(last_synced)),` from the `AppState` literal
- delete the `if let Some(git) = sync_git { spawn_sync_task(…); }` block
- delete `.route("/sync", post(sync_handler))` from the router
- delete `ensure_dir(&git.repo_dir, …)` — it lived inside the deleted `sync_git` block

- [ ] **Step 4: Delete `sync_handler` from `handlers.rs`**

Delete `pub async fn sync_handler` (it starts around `handlers.rs:1030`, recognisable by `let git = match &state.config.git`) together with the now-unused `clone_or_pull` import.

- [ ] **Step 5: Drop `last_synced` from `AppState`**

In `src/cmd/serve/state.rs`, delete the field:

```rust
    pub last_synced: Arc<Mutex<Option<Timestamp>>>,
```

and the matching `last_synced: Arc::new(Mutex::new(None)),` line in `test_support::state_with_config`.

- [ ] **Step 6: Drop git from the landing page**

In `src/cmd/serve/landing.rs`:
- replace the git-derived staleness interval

```rust
    let interval_minutes = state
        .config
        .git
        .as_ref()
        .map(|g| g.poll_interval_minutes)
        .unwrap_or(30);
```
with:
```rust
    // Counts were refreshed on the git poll interval; with no remote left,
    // the same period simply keeps a long-lived tab from going stale.
    let interval_minutes = COUNT_REFRESH_MINUTES;
```
and add above `counts_are_stale`:
```rust
/// How often the cached collection counts are recomputed, in minutes.
const COUNT_REFRESH_MINUTES: u64 = 30;
```
- delete `let last_synced = *state.last_synced.lock();` and `let git_enabled = state.config.git.is_some();`
- delete the `last_synced` and `git_enabled` fields from `LandingStatus`, from the literal that builds it, and from the destructuring in `render_landing_page`
- delete the `if status.git_enabled { … }` branch of `status_line`
- delete the `@if git_enabled { form action="/sync" … }` block from the nav

- [ ] **Step 7: Drop the commit from the card editor**

In `src/cmd/serve/edit.rs`, delete `use crate::cmd::serve::git::commit_edit;` and delete the whole author-resolution and commit block (lines 284–303), i.e. everything from the `// FEAT-04:` comment to the closing `})?;`. `EditOutcome` becomes:

```rust
/// What a successful edit did, for user-facing reporting.
pub struct EditOutcome {
    /// Cards whose review history was migrated to a new hash.
    pub migrated: usize,
    /// New cards that could not be matched to prior history and start fresh.
    pub skipped: usize,
}
```

and the tail of `edit_post_inner` becomes:

```rust
    Ok(EditOutcome {
        migrated: counts.renamed,
        // A rename the database declined as a true collision (its target
        // hash already has history) leaves the old row unmatched, which the
        // user should hear about. A rename whose old hash had no history of
        // its own is not: there was nothing to lose.
        skipped: plan.skipped + counts.collided,
    })
```

In `edit_post_handler`, drop `committed` from the log line and delete the `if outcome.committed { msg.push_str(" Committed to git."); }` branch:

```rust
        Ok(outcome) => {
            log::debug!(
                "Edit saved: {} card(s) migrated, {} skipped",
                outcome.migrated,
                outcome.skipped
            );
            let target = format!("/collection/{slug}/bookmarks");
            let msg = String::from("Card saved.");
```

`current_user` is now used only for `owner`, so change the parameter of `edit_post_inner` from `current_user: Option<CurrentUser>` to `owner: Option<&str>` and pass `current_user.map(|u| u.email)` from the handler, mirroring `edit_get_handler`.

- [ ] **Step 8: Fix every `ResolvedServeConfig` and `AppState` literal in tests**

Remove `git: None,` from every literal. The sites are:
`src/cmd/serve/auth.rs` lines 979, 1050, 1113, 1176, 1255, 1441; `src/cmd/serve/mod.rs` lines 48 and 218; `src/cmd/serve/state.rs` (both `test_support` constructors); plus any in `handlers.rs` and `decks.rs` that the compiler names. Let `cargo build --tests` list them:

```bash
cargo build --tests 2>&1 | grep -n "missing field\|struct .* has no field" | head -30
```

- [ ] **Step 9: Drop tokio's `process` feature**

In `Cargo.toml`, change:

```toml
tokio = { version = "1.52.1", features = ["rt-multi-thread", "fs", "signal", "process", "time"] }
```
to:
```toml
tokio = { version = "1.52.1", features = ["rt-multi-thread", "fs", "signal", "time"] }
```

- [ ] **Step 10: Update the documentation**

In `README.md`: delete the whole `### [git]` section (starting at line 123, ending where `### [[collection]]` begins) and any surrounding sentence that promises a git remote. In the `[server]` section, change the `data_dir` description at lines 92–93 so it no longer mentions "the repo clone".

In `hashcards.example.toml`: delete the whole `[git]` block (lines 26–31) and edit the `data_dir` comment so it reads "Directory for the review databases and any HedgeDoc notes."

- [ ] **Step 11: Verify**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS. 478 − 4 (the three `commit_edit` tests plus `test_clone_or_pull_*`, whichever `git.rs` held after Task 1) ≈ **475 tests**. Confirm the delta is exactly the tests that lived in `git.rs`:

```bash
git show HEAD~1:src/cmd/serve/git.rs | grep -c "#\[test\]"
```

- [ ] **Step 12: Commit**

```bash
git add -A
git commit -m "feat!: remove the [git] remote

The repo clone was polled with 'git pull --ff-only' while commit_edit
committed web edits into the same worktree and never pushed. Once the
remote moved, the branch had diverged and every later pull failed --
logged, and otherwise silent. Nothing replaces it: cards are written in
hashcards itself.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RP3W1RufSmp3GWtDuxC763"
```

---

## Task 3: Remove `[[source]]` and HedgeDoc

The largest deletion: 1,566 + 199 + 169 lines of source plus the routes, the sync task and the `AppState` fields that carried them.

**Files:**
- Delete: `src/cmd/serve/hedgedoc.rs`, `src/cmd/serve/source.rs`, `src/cmd/serve/hedgedoc_ui.rs`
- Modify: `src/cmd/serve/href.rs` — delete `safe_href` and its two tests; keep `encoded_path`
- Modify: `src/cmd/serve/mod.rs` — drop `mod hedgedoc;`, `mod source;`, `mod hedgedoc_ui;`
- Modify: `src/cmd/serve/config.rs` — `SourceEntry`, `ServeConfig.sources`, `ServeConfig.hedgedoc`, `source_entries()`, `ResolvedServeConfig.hedgedoc_entries`, the `hedgedoc_entries` owner branches
- Modify: `src/cmd/serve/state.rs` — `HedgedocSource`, `HedgedocNote`, `hedgedoc_sources`, `hedgedoc_last_synced`, `collections`, `counts_refreshed_at`
- Modify: `src/cmd/serve/server.rs` — the four `/sources*` routes, the `/hedgedoc` redirect, the startup fetch, `spawn_hedgedoc_sync_task`, `check_startup_slug_collisions`, `build_combined_infos`, the `hedgedoc` data directory
- Modify: `src/cmd/serve/handlers.rs` — the four `hedgedoc_*` handlers, `AddHedgedocForm`, `DeleteHedgedocForm`, the `hedge_urls` map, `find_collection`'s third branch, the three count-refresh `tokio::spawn` blocks
- Modify: `src/cmd/serve/browse.rs` — the `hedge_urls` parameter and the outbound edit link
- Modify: `src/cmd/serve/landing.rs` — the count-refresh block, `hedgedoc_count`, `hedgedoc_last_synced`, the `Sources` nav link, the HedgeDoc branch of `status_line`
- Modify: `src/cmd/serve/decks.rs` — the `hedgedoc` chain in `owned_collections`
- Modify: `src/cmd/serve/files.rs` — the HedgeDoc half of `reserved_slugs`, the shadow-drop `retain` in `collections_for`
- Modify: `README.md`, `hashcards.example.toml`

**Interfaces:**
- Consumes: `refresh_collection_info` from Task 1.
- Produces:
  - `AppState` is `{ config, collections, sessions, custom_decks, config_path, counts_refreshed_at, interrupted_closed, session_key, oidc }` after this task; Task 4 removes `collections` and `counts_refreshed_at`.
  - `pub fn render_browse_page(collection_name: &str, slug: &str, browse: &BrowseData, bookmark_count: usize, interrupted_closed: usize, flash: Option<Flash>) -> Markup` — the `hedge_urls` parameter is gone. Task 10 adds a different one.
  - `fn reserved_slugs(state: &AppState, owner: Option<&str>) -> Vec<(String, String)>` now returns only custom-deck slugs.
  - `href::encoded_path` survives unchanged.

- [ ] **Step 1: Delete the three files and unregister them**

```bash
git rm src/cmd/serve/hedgedoc.rs src/cmd/serve/source.rs src/cmd/serve/hedgedoc_ui.rs
```

Remove `mod hedgedoc;`, `mod hedgedoc_ui;` and `mod source;` from `src/cmd/serve/mod.rs`.

- [ ] **Step 2: Shrink `href.rs` to `encoded_path`**

Delete `pub fn safe_href` (lines 5–20) and the two tests `accepts_http_and_https` and `rejects_everything_else`. Keep `encoded_path`, `PATH_RESERVED` and the test `encodes_what_would_end_a_url_path`. Update the module's remaining imports: `use percent_encoding::AsciiSet; use percent_encoding::CONTROLS; use percent_encoding::utf8_percent_encode;` all stay.

- [ ] **Step 3: Strip sources out of `config.rs`**

Delete from `src/cmd/serve/config.rs`:
- the `sources` and `hedgedoc` fields of `ServeConfig` (lines 29–34)
- `pub struct SourceEntry` (lines 42–47)
- the entire `impl ServeConfig` block holding `source_entries()` (lines 49–56)
- `let configured_sources = config.source_entries();` and the `let hedgedoc_entries: Vec<SourceEntry> = …` block in `from_toml`
- the `pub hedgedoc_entries: Vec<SourceEntry>` field of `ResolvedServeConfig` and both literals that set it
- the `for h in &hedgedoc_entries { … }` owner-required branch inside `if oidc.is_some()`
- the `if let Some(h) = hedgedoc_entries.iter().find(…)` branch in the `else`
- the test `source_and_hedgedoc_arrays_both_parse`

- [ ] **Step 4: Strip HedgeDoc out of `state.rs`**

Delete `pub struct HedgedocSource`, `pub struct HedgedocNote`, the `hedgedoc_sources` and `hedgedoc_last_synced` fields of `AppState`, and the matching lines in `test_support::state_with_config`.

- [ ] **Step 5: Strip HedgeDoc out of `server.rs`**

- delete the imports `build_combined_infos`, `build_source_lossless`, `check_startup_slug_collisions`, `spawn_hedgedoc_sync_task`, `HedgedocSource`, and the four `hedgedoc_*` handler imports
- delete `ensure_dir(&data_dir.join("hedgedoc"), "HedgeDoc note directory")?;`
- delete the `hedgedoc_sources_init` block, `hedgedoc_last_synced_init`, `hedgedoc_poll_minutes`, `HEDGEDOC_POLL_MINUTES` (added in Task 2), the `hedgedoc_sources` and `hedgedoc_last_synced` `Arc`s, and the `spawn_hedgedoc_sync_task` call
- delete the `check_startup_slug_collisions(…)` call
- replace the collection-info build

```rust
    let collection_infos = build_combined_infos(&config.collections, &hedgedoc_sources_init);
```
with:
```rust
    let collection_infos = refresh_collection_info(&config.collections);
```
adding `use crate::cmd::serve::counts::refresh_collection_info;`
- change `sweep_dangling_sessions` to take only the config:
```rust
fn sweep_dangling_sessions(config: &ResolvedServeConfig) -> HashMap<String, usize> {
    let stale_before = Timestamp::now().minus_minutes(SESSION_STALE_MINUTES);
    let collections: Vec<&ResolvedCollection> = config.collections.iter().collect();
```
leaving the rest of the body unchanged, and update its call site to `sweep_dangling_sessions(&config)`
- replace the deck-collision inputs
```rust
    let all_slugged: Vec<ResolvedCollection> = config
        .collections
        .iter()
        .cloned()
        .chain(hedgedoc_sources.lock().iter().map(|s| s.collection.clone()))
        .collect();
    check_deck_slug_collisions(&custom_decks, &all_slugged)?;
```
with:
```rust
    check_deck_slug_collisions(&custom_decks, &config.collections)?;
```
- delete `sync_collections` (it exists only to feed the two sync tasks)
- delete the four `/sources*` routes and the `/hedgedoc` redirect route, and the now-unused `use axum::response::Redirect;`

- [ ] **Step 6: Strip HedgeDoc out of `handlers.rs`**

- delete every `use crate::cmd::serve::hedgedoc::…` import
- delete the whole `// ---- HedgeDoc management handlers ----` section: `hedgedoc_manage_handler`, `AddHedgedocForm`, `hedgedoc_add_handler`, `DeleteHedgedocForm`, `hedgedoc_delete_handler`, `hedgedoc_sync_now_handler`
- in `collection_get_inner`, delete the `hedge_urls` map (lines 157–171) and drop the argument from the `render_browse_page` call
- in `find_collection`, delete the third branch so the body ends after the discovery lookup:

```rust
pub(super) fn find_collection(
    state: &AppState,
    slug: &str,
    owner: Option<&str>,
) -> Option<ResolvedCollection> {
    if let Some(rc) = state
        .config
        .collections
        .iter()
        .find(|c| c.slug == slug && c.owner.as_deref() == owner)
    {
        return Some(rc.clone());
    }
    existing_local_collections_for(state, current_user_for(owner).as_ref())
        .into_iter()
        .find(|c| c.slug == slug && c.owner.as_deref() == owner)
}
```
- in the three background count-refresh blocks (the `Home` action, `SessionFinished`, and the one in the deleted sync handler), replace `build_combined_infos(&static_collections, &sources_snapshot)` with `refresh_collection_info(&static_collections)` and delete the `sources_snapshot` line above each

- [ ] **Step 7: Drop `hedge_urls` from `browse.rs`**

- delete `use crate::cmd::serve::href::safe_href;` and the `use std::collections::HashMap;` if it becomes unused
- change the signature of `render_browse_page` to drop `hedge_urls: &HashMap<String, String>`
- change `fn render_deck_node(node: &DeckNode, depth: usize) -> Markup`, delete the `let edit_url = …` binding and the `@if let Some(url) = edit_url { a.edit-link … }` block, and drop the argument from both recursive calls
- delete the test that asserts a `javascript:` URL is not rendered (it starts around `browse.rs:437` and builds a `hedge_urls` map) — it tested `safe_href`, which no longer exists

- [ ] **Step 8: Drop HedgeDoc from the landing page**

In `src/cmd/serve/landing.rs`:
- delete the `use …::build_combined_infos;` import and replace the whole stale-count refresh block's closure body with `refresh_collection_info(&static_collections)`, deleting the `sources_snapshot` line
- delete `let hedgedoc_last_synced = …;` and the `let hedgedoc_count = …;` block
- delete `hedgedoc_count` and `hedgedoc_last_synced` from `LandingStatus`, from its literal, and delete the `if status.hedgedoc_count > 0 { … }` branch of `status_line`
- delete `a.nav-link href="/sources" { "Sources" }` from the nav

- [ ] **Step 9: Drop HedgeDoc from `decks.rs` and `files.rs`**

In `decks.rs`, `owned_collections` becomes:

```rust
fn owned_collections(state: &AppState, owner: Option<&str>) -> Vec<ResolvedCollection> {
    let configured = state
        .config
        .collections
        .iter()
        .filter(|c| c.owner.as_deref() == owner)
        .cloned();
    let local = local_collections_for(state, current_user_for(owner).as_ref());
    configured.chain(local).collect()
}
```

In `files.rs`, `reserved_slugs` loses its HedgeDoc half and gains the deck slugs it was always meant to guard:

```rust
/// Slugs a card folder must not take: the caller's own saved decks. Both a
/// deck and a collection are addressed through `/collection/{slug}`, and
/// routing prefers the collection, so a folder that matches a deck slug
/// makes the deck unreachable.
fn reserved_slugs(state: &AppState, owner: Option<&str>) -> Vec<(String, String)> {
    state
        .custom_decks
        .lock()
        .iter()
        .filter(|d| d.owner.as_deref() == owner)
        .map(|d| (d.slug.clone(), d.name.clone()))
        .collect()
}
```

and the shadow-drop `retain` at the end of `collections_for` is deleted, so the function ends:

```rust
    match discover_local_collections(&root, &data_dir.join("db"), owner, policy) {
        Ok(found) => found,
        Err(e) => {
            log::error!("Cannot list local collections: {e}");
            Vec::new()
        }
    }
}
```

(with the `let mut found =` binding changed back to a direct `match` return, and the now-unused `owner_key(user)` call removed from this function).

- [ ] **Step 10: Fix the comments in `markdown.rs`**

`markdown.rs` is **not** changed behaviourally: it keeps passing external image URLs through, because a card may legitimately reference a remote image. Only the comments naming HedgeDoc as the example change.

At `markdown.rs:124`:

```rust
        // An external URL is passed through after parsing and re-serializing.
        // This rejects invalid URLs (including those containing whitespace or
        // control characters) and non-http(s) schemes, and produces a
        // canonicalized string that is safe to embed in HTML.
```

At `markdown.rs:252`, inside `test_external_url_image_renders_unchanged`:

```rust
        // An external image URL must be passed through as-is so the browser
        // fetches it directly.
```

- [ ] **Step 11: Update the documentation**

In `README.md`: delete the `### [[source]]` section (lines 157–197), the `HedgeDoc note` clause in the intro at line 27, the `{data_dir}/hedgedoc` mention at lines 92–93, the `/hedgedoc` row of the routes table at line 329, the `[[source]]` mention in the `[oidc]` paragraph at line 291, and the `[HedgeDoc]: https://hedgedoc.org/` link definition at line 605.

In `hashcards.example.toml`: delete any `[[source]]` example and the "and any HedgeDoc notes" clause from the `data_dir` comment, and drop `[[source]]` from the `[oidc]` paragraph.

- [ ] **Step 12: Verify**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS. Roughly 475 − 48 ≈ **427 tests** (37 in `hedgedoc.rs`, 9 in `source.rs`, 2 in `hedgedoc_ui.rs`, 2 `safe_href` tests in `href.rs`, plus the one browse test that exercised `safe_href`, and the two `mod.rs` tests `test_hedgedoc_add_empty_url_is_surfaced_as_flash` and `test_hedgedoc_add_rejects_http_url_before_persisting`, which must be deleted along with their routes). Confirm no surviving failure mentions HedgeDoc.

- [ ] **Step 13: Commit**

```bash
git add -A
git commit -m "feat!: remove [[source]] and HedgeDoc notes

A remote markdown document could not be edited: sync_source overwrote
the file on the next tick and the edit vanished with no warning. That
made 'a card can be edited' conditional on where the card came from,
which nothing in the UI surfaced. To bring markdown in, create a file
and paste the text into the editor.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RP3W1RufSmp3GWtDuxC763"
```

---

## Task 4: Remove `[[collection]]` — one collection kind

After this task a collection is exactly one thing: a top-level folder in the caller's card tree with a stable id in its `.hashcards.toml`.

**Files:**
- Modify: `src/cmd/serve/config.rs` — `CollectionEntry`, `ServeConfig.collections`, `ResolvedServeConfig.collections`, the path-validation block, `check_slug_collisions`, the collection owner branches, `from_directories`
- Modify: `src/cmd/serve/local.rs` — add `discover_all_collections`
- Modify: `src/cmd/serve/state.rs` — drop `collections`, `counts_refreshed_at`; re-key `interrupted_closed`; collapse `test_support`
- Modify: `src/cmd/serve/server.rs` — the startup sweep, the collection-info build, the db-parent loop
- Modify: `src/cmd/serve/handlers.rs` — `find_collection`, the count-refresh blocks, the `interrupted_closed` read, the test fixtures
- Modify: `src/cmd/serve/landing.rs` — the stale-count block
- Modify: `src/cmd/serve/decks.rs` — `owned_collections`
- Modify: `src/cmd/serve/files.rs` — the test fixtures that build configured collections
- Modify: `src/cmd/serve/export.rs` — the one `state_with_collections` caller
- Modify: `src/cmd/serve/auth.rs` — the six config literals
- Modify: `src/cmd/serve/mod.rs` — `spawn_test_server`
- Modify: `README.md`, `hashcards.example.toml`

**Interfaces:**
- Consumes: `discover_local_collections`, `collection_id`, `LocalRoot`, `IdPolicy` from `local.rs` (renamed in Task 5).
- Produces:
  - `pub fn discover_all_collections(data_dir: &Path) -> Vec<ResolvedCollection>` in `local.rs` — every user's collections, for startup-time work only. `owner` is always `None`: the tree's directory name is a slug of an email, not the email, so it must never be used for routing.
  - `AppState` is `{ config, sessions, custom_decks, config_path, interrupted_closed, session_key, oidc }`, with `interrupted_closed: Arc<Mutex<HashMap<PathBuf, usize>>>`.
  - `ResolvedServeConfig` is `{ host, port, defaults, data_dir, config_path, custom_decks, session_timeout_minutes, oidc }`.
  - `pub fn state_with_data_dir(data_dir: PathBuf) -> AppState` — the single test constructor; `state_with_collections` and `state_with_config` are gone.

- [ ] **Step 1: Write the failing test for cross-user interrupted-session notices**

Add to `src/cmd/serve/server.rs`'s test module (create one if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::serve::local::collection_id;
    use crate::helper::create_tmp_directory;

    /// Two users may each have a collection called "Spanish". They slugify
    /// alike, so a notice keyed by slug would be shown to whichever of them
    /// opened the page first, reporting the other's interrupted sessions.
    /// Keyed by database path, each notice reaches its own owner.
    #[test]
    fn interrupted_notices_are_keyed_per_database_not_per_slug() -> Fallible<()> {
        let data_dir = create_tmp_directory()?;
        let db_dir = data_dir.join("db");
        ensure_dir(&db_dir, "review database directory")?;

        // Still `local/` at this point; Task 5 renames it to `cards/` and
        // updates this fixture with it.
        let mut db_paths = Vec::new();
        for user in ["alice-example.com", "bob-example.com"] {
            let folder = data_dir.join("local").join(user).join("Spanish");
            std::fs::create_dir_all(&folder)?;
            let id = collection_id(&folder)?;
            let db_path = db_dir.join(format!("{id}.db"));
            let db_str = match db_path.to_str() {
                Some(p) => p,
                None => return fail("temp path is not UTF-8"),
            };
            let db = Database::new(db_str)?;
            // A session row opened long ago and never closed: exactly what a
            // crash leaves behind.
            let started = Timestamp::now().minus_minutes(SESSION_STALE_MINUTES * 2);
            db.create_session(started)?;
            db_paths.push(db_path);
        }

        let counts = sweep_dangling_sessions(&data_dir);
        assert_eq!(counts.len(), 2, "each user's collection swept separately");
        for db_path in &db_paths {
            assert_eq!(counts.get(db_path), Some(&1), "{}", db_path.display());
        }
        Ok(())
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test interrupted_notices_are_keyed_per_database_not_per_slug`
Expected: FAIL to compile — `sweep_dangling_sessions` takes a `&ResolvedServeConfig`, not a `&Path`, and returns a `HashMap<String, usize>`.

- [ ] **Step 3: Add `discover_all_collections`**

Append to `src/cmd/serve/local.rs`:

```rust
/// Every collection in every user's tree under `{data_dir}/local`.
///
/// For startup-time work that must touch each review database once — the
/// dangling-session sweep — and nothing else. `owner` is always `None`: a
/// tree's directory name is a *slug* of an email, which no request can be
/// matched against, so a collection from here must never be routed to.
/// Every read path goes through `find_collection` instead.
///
/// `ExistingOnly`, so startup never writes an id into a user's tree: a
/// folder with no id has no database either, and nothing to sweep.
pub fn discover_all_collections(data_dir: &Path) -> Vec<ResolvedCollection> {
    let trees_dir = data_dir.join("local");
    let db_dir = data_dir.join("db");
    let entries = match read_dir(&trees_dir) {
        Ok(entries) => entries,
        // No tree yet is the ordinary state of a fresh install.
        Err(_) => return Vec::new(),
    };
    let mut all = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || path.is_symlink() {
            continue;
        }
        let root = LocalRoot { root: path };
        match discover_local_collections(&root, &db_dir, None, IdPolicy::ExistingOnly) {
            Ok(found) => all.extend(found),
            Err(e) => log::warn!("Skipping the card tree at {}: {e}", root.path().display()),
        }
    }
    all
}
```

`LocalRoot`'s private `root` field is reachable here because both live in the same module. Task 5 renames the type to `CardRoot` and `data_dir.join("local")` to `data_dir.join("cards")`.

- [ ] **Step 4: Rewrite the startup sweep**

In `src/cmd/serve/server.rs`, replace `sweep_dangling_sessions` with:

```rust
/// Close session rows left dangling by a crash or restart, across every
/// collection this server can serve, and return the per-database counts so
/// the topic browser can report them once.
///
/// Keyed by database path rather than by URL slug: two users may each own a
/// collection called "Spanish", and a slug-keyed notice would be shown to
/// whichever of them opened the page first.
///
/// A collection whose database cannot be opened is skipped with a log line
/// rather than failing startup. Each database is independent, so the sweeps
/// run on their own threads.
fn sweep_dangling_sessions(data_dir: &Path) -> HashMap<PathBuf, usize> {
    let stale_before = Timestamp::now().minus_minutes(SESSION_STALE_MINUTES);
    let collections = discover_all_collections(data_dir);

    let results: Vec<(PathBuf, Fallible<usize>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = collections
            .iter()
            .map(|rc| {
                scope.spawn(move || {
                    let closed = (|| {
                        let db_path = rc.db_path.to_str().ok_or_else(|| {
                            ErrorReport::new(format!(
                                "Database path is not valid UTF-8: {}",
                                rc.db_path.display()
                            ))
                        })?;
                        Database::new(db_path)
                            .and_then(|db| db.close_dangling_sessions(stale_before))
                    })();
                    (rc.db_path.clone(), closed)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("sweep thread panicked"))
            .collect()
    });

    let mut counts = HashMap::new();
    for (db_path, closed) in results {
        match closed {
            Ok(0) => {}
            Ok(n) => {
                log::info!("Closed {n} interrupted session(s) in {}", db_path.display());
                counts.insert(db_path, n);
            }
            Err(e) => log::error!(
                "Could not close interrupted sessions in {}: {e}",
                db_path.display()
            ),
        }
    }
    counts
}
```

Add `use std::path::Path;` and `use crate::cmd::serve::local::discover_all_collections;`. Change the call site to:

```rust
    let interrupted_closed = match &config.data_dir {
        Some(data_dir) => sweep_dangling_sessions(data_dir),
        None => HashMap::new(),
    };
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test interrupted_notices_are_keyed_per_database_not_per_slug`
Expected: PASS.

- [ ] **Step 6: Re-key `interrupted_closed` in `AppState` and its reader**

In `src/cmd/serve/state.rs`:

```rust
    /// Per-database count of session rows closed by the startup sweep,
    /// waiting to be reported to the user. The topic browser takes the
    /// entry the first time it renders, so the notice is shown once rather
    /// than on every visit (see `sweep_dangling_sessions`).
    pub interrupted_closed: Arc<Mutex<HashMap<PathBuf, usize>>>,
```

In `handlers.rs`, `collection_get_inner`, change:

```rust
        let interrupted_closed = state.interrupted_closed.lock().remove(slug).unwrap_or(0);
```
to:
```rust
        let interrupted_closed = state
            .interrupted_closed
            .lock()
            .remove(&rc.db_path)
            .unwrap_or(0);
```

- [ ] **Step 7: Strip `[[collection]]` out of `config.rs`**

Delete:
- the `collections: Vec<CollectionEntry>` field of `ServeConfig`
- `pub struct CollectionEntry` and its `impl` (lines 231–241)
- `pub struct ResolvedServeConfig`'s `collections` field
- the whole `let collections = config.collections.iter().map(…)` block in `from_toml`, including the `is_absolute`/`has_root`/`ParentDir` validation, and `let repo_dir = data_dir.join("repo");`
- `pub(crate) fn check_slug_collisions` and its call
- inside `if oidc.is_some()`, the `for c in &collections { … }` branch; inside the `else`, the `if let Some(c) = collections.iter().find(…)` branch
- `#[cfg(test)] pub fn from_directories` entirely
- the tests `test_absolute_collection_path_rejected`, `test_parent_collection_path_rejected`, `test_relative_collection_path_accepted`, and any other test whose fixture TOML contains `[[collection]]`

`ResolvedCollection` itself stays: it is what discovery produces.

- [ ] **Step 8: Collapse `find_collection` to one branch**

In `handlers.rs`:

```rust
/// Looks up a collection by slug, scoped to `owner` (the caller's email, or
/// `None` when `[oidc]` is off). A collection whose `owner` doesn't match is
/// treated exactly like a nonexistent one, so callers can 404 either case
/// identically rather than leaking which slugs exist for other users.
pub(super) fn find_collection(
    state: &AppState,
    slug: &str,
    owner: Option<&str>,
) -> Option<ResolvedCollection> {
    existing_local_collections_for(state, current_user_for(owner).as_ref())
        .into_iter()
        .find(|c| c.slug == slug && c.owner.as_deref() == owner)
}
```

`find_collection_blocking` keeps its `spawn_blocking` wrapper: the lookup still reads the filesystem.

- [ ] **Step 9: Delete the count cache**

In `state.rs`, delete the `collections` and `counts_refreshed_at` fields of `AppState` and the matching lines in `test_support`. Then delete every read and write of them:
- `landing.rs`: the whole `if stale && interval_minutes > 0 { … }` block, `counts_are_stale`, `COUNT_REFRESH_MINUTES`, the `let all_collections = state.collections.read().await;` binding and `drop(all_collections)`. The collection rows now come from `local_infos` alone:
```rust
    let collections: Vec<&CollectionInfo> = local_infos
        .iter()
        .filter(|c| c.owner.as_deref() == owner.as_deref())
        .collect();
```
- `handlers.rs`: both background `tokio::spawn` count-refresh blocks (in the `Home` action and after `SessionFinished`) — delete them entirely, along with the `BUG-45` comments that introduce them. Discovery counts per request, so there is nothing to pre-warm.
- `server.rs`: `let collection_infos = refresh_collection_info(&config.collections);`, the `collections:` and `counts_refreshed_at:` entries of the `AppState` literal, the `for rc in &config.collections { create_dir_all(parent) }` loop, and the `use tokio::sync::RwLock;` import if unused.
- `state.rs`: delete `use tokio::sync::RwLock;` if unused.

Keep `CollectionInfo` — `refresh_collection_info` still produces it for the landing page.

- [ ] **Step 10: Collapse `test_support` to one constructor**

In `state.rs`:

```rust
/// Test-only constructors for `AppState`.
#[cfg(test)]
pub mod test_support {
    use super::*;
    use crate::cmd::serve::config::DefaultsSection;
    use crate::cmd::serve::config::ResolvedServeConfig;

    /// An `AppState` whose card trees live under `data_dir`, with no OIDC
    /// runtime and no saved decks.
    pub fn state_with_data_dir(data_dir: PathBuf) -> AppState {
        AppState {
            config: Arc::new(ResolvedServeConfig {
                host: "127.0.0.1".to_string(),
                port: 0,
                defaults: DefaultsSection::default(),
                data_dir: Some(data_dir),
                config_path: None,
                custom_decks: Vec::new(),
                session_timeout_minutes: 1440,
                oidc: None,
            }),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            custom_decks: Arc::new(Mutex::new(Vec::new())),
            config_path: Arc::new(Mutex::new(None)),
            interrupted_closed: Arc::new(Mutex::new(HashMap::new())),
            session_key: Key::generate(),
            oidc: None,
        }
    }
}
```

`auth.rs`'s six hand-rolled literals set `oidc: Some(…)`, so they cannot use this. Leave them as literals but delete their `git`, `collections` and `hedgedoc_entries` entries — they shrink to eight fields each.

- [ ] **Step 11: Rewrite the test fixtures onto the card tree**

Every fixture that built a configured collection now builds a folder. The sites:

- `src/cmd/serve/export.rs:404` — replace `state_with_collections(vec![ResolvedCollection { … }])` with a card-tree fixture:
```rust
        let data_dir = create_tmp_directory()?;
        let folder = data_dir.join("cards").join("default").join("Test");
        std::fs::create_dir_all(&folder)?;
        std::fs::write(folder.join("Deck.md"), "Q: a\nA: b\n")?;
        collection_id(&folder)?;
        let state = state_with_data_dir(data_dir.clone());
```
and look the collection up with `find_collection(&state, "Test", None)` where the test previously used its literal.
- `src/cmd/serve/files.rs:1246` — `state_for(data_dir, collections)` loses its second parameter and becomes `fn state_for(data_dir: &Path) -> AppState { state_with_data_dir(data_dir.to_path_buf()) }`. Delete the `fn configured(name, slug)` helper and every test that only asserted a folder is dropped for shadowing a configured collection.
- `src/cmd/serve/handlers.rs:1476` `test_state_with_config` and its callers — replace with `state_with_data_dir`, creating the collections as folders.
- `src/cmd/serve/decks.rs:574` — `state_with_data_dir(dir.clone(), Vec::new())` becomes `state_with_data_dir(dir.clone())`.
- `src/cmd/serve/mod.rs:47` `spawn_test_server` — rewrite it to serve a discovered collection:
```rust
    /// Start a server whose card tree holds one collection named `name`,
    /// its files copied from `files` (relative path, content). Returns the
    /// port and the data directory, which the caller keeps alive.
    async fn spawn_test_server(name: &str, files: &[(&str, &str)]) -> Fallible<(u16, TempDir)> {
        let port = pick_unused_port().unwrap();
        let dir = tempdir()?;
        let folder = dir.path().join("cards").join("default").join(name);
        std::fs::create_dir_all(&folder)?;
        for (rel, content) in files {
            let path = folder.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            write(path, content)?;
        }
        // Read paths skip a folder with no id, so stamp one the way the file
        // manager would.
        collection_id(&folder)?;

        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            defaults: DefaultsSection::default(),
            data_dir: Some(dir.path().to_path_buf()),
            config_path: None,
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
        };
        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;
        Ok((port, dir))
    }
```
Every caller changes from `spawn_test_server(coll_dir, slug)` to `spawn_test_server("Test Collection", &[("Alpha.md", "Q: What is 1+1?\nA: 2\n")])`, and the slug they assert against becomes `slugify` of the name — for `"Test Collection"` that is `Test-Collection`. Keep each test's assertions unchanged otherwise; if a rewrite turns out to be *interesting* rather than mechanical, stop and say so — per the spec, that means something is wrong.

- [ ] **Step 12: Update the documentation**

In `README.md`: delete the `### [[collection]]` section (lines 142–156) and every reference to configuring a collection. Rewrite the `[server]` `data_dir` paragraph to describe `{data_dir}/cards/{user}/` and `{data_dir}/db/`. In the `[oidc]` paragraph at line 291, drop `[[collection]]` so only `[[deck]]` remains, and add a sentence: ownership is now which tree a folder sits in — `cards/{email-slug}/` with `[oidc]`, `cards/default/` without.

In `hashcards.example.toml`: delete the `[[collection]]` example and the same `[oidc]` clause.

- [ ] **Step 13: Verify**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS. The count drops by the config tests deleted in Step 7 and the shadow-drop tests deleted in Step 11; everything else must survive.

- [ ] **Step 14: Commit**

```bash
git add -A
git commit -m "feat!: one collection kind

A collection is a top-level folder in the caller's card tree, discovered
by reading the directory and identified by the stable id in its
.hashcards.toml. Ownership is which tree the folder sits in, so a
collection owned by nobody reachable stops being expressible. Three slug
namespaces become one, two database-naming schemes become one, and
find_collection becomes a single lookup.

The startup sweep for interrupted sessions now walks every user's tree
and reports per database rather than per slug, so two users' Spanish
collections cannot claim each other's notice.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RP3W1RufSmp3GWtDuxC763"
```

---

## Task 5: Rename `local/` to `cards/`

Purely mechanical, and worth its own commit so it can be read as one. `local/` was named in opposition to `repo/`; with `repo/` gone the name means nothing, and the UI already says "My Cards".

**Files:**
- Rename: `src/cmd/serve/local.rs` → `src/cmd/serve/cards.rs`
- Modify: `src/cmd/serve/mod.rs`
- Modify: every module importing from `local` — `files.rs`, `handlers.rs`, `decks.rs`, `landing.rs`, `server.rs`, `export.rs`
- Modify: `CHANGELOG.xml`
- Modify: `README.md`

**Interfaces:**
- Consumes: everything from Task 4.
- Produces:
  - `crate::cmd::serve::cards::{CardRoot, ResolvedEntry, IdPolicy, collection_id, existing_collection_id, discover_local_collections, discover_all_collections, COLLECTION_META_FILE}`
  - `CardRoot::for_user` and `CardRoot::open` resolve to `{data_dir}/cards/{who}`.

- [ ] **Step 1: Move the file and rename the type**

```bash
git mv src/cmd/serve/local.rs src/cmd/serve/cards.rs
```

In `src/cmd/serve/mod.rs`, replace `mod local;` with `mod cards;` (in alphabetical position, before `mod config;`).

In `cards.rs`, apply these renames throughout:
- `LocalRoot` → `CardRoot`
- `LOCAL_META_FILE` → `COLLECTION_META_FILE`
- `LocalMeta` → `CollectionMeta`
- in `root_path`, `data_dir.join("local")` → `data_dir.join("cards")`
- in `discover_all_collections`, `data_dir.join("local")` → `data_dir.join("cards")`, its doc comment's `{data_dir}/local` with it
- in `server.rs`'s `interrupted_notices_are_keyed_per_database_not_per_slug`, `data_dir.join("local")` → `data_dir.join("cards")`, and delete the "Still `local/` at this point" comment

and replace the type's doc comment, which names the directory it was defined against:

```rust
/// One user's markdown tree, at `{data_dir}/cards/{user}`.
///
/// Every collection lives in one of these: a collection is a top-level
/// folder here, discovered by reading the directory rather than declared in
/// the config file, and owned by whoever the tree belongs to.
pub struct CardRoot {
    root: PathBuf,
}
```

Also update the two tests that assert the path:

```rust
        assert_eq!(root.path(), dir.join("cards").join("me-example.com"));
```
and
```rust
        assert_eq!(root.path(), dir.join("cards").join("default"));
```

- [ ] **Step 2: Fix every importer**

```bash
grep -rln "serve::local\|LocalRoot\|LOCAL_META_FILE" src/
```

Rewrite each hit: `crate::cmd::serve::local::` → `crate::cmd::serve::cards::`, `LocalRoot` → `CardRoot`, `LOCAL_META_FILE` → `COLLECTION_META_FILE`. Also rename the two wrappers in `files.rs` so they stop saying "local":
- `local_collections_for` → `collections_for_user`
- `existing_local_collections_for` → `existing_collections_for_user`

and their call sites in `landing.rs`, `decks.rs` and `handlers.rs`.

- [ ] **Step 3: Verify**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS, with the same count as after Task 4. A rename changes no count.

- [ ] **Step 4: Record the removal in `CHANGELOG.xml`**

Add a `<removed>` block inside `<unreleased>` (after `<changed>`), with one entry covering Tasks 2–5:

```xml
        <removed>
            <change author="claude">
            **Remote sources are gone.** `[git]`, `[[source]]`/`[[hedgedoc]]` and the auto-commit of web edits are removed, and with them the `/sources` page, both background sync tasks and every outbound request except OIDC. They could not be edited: a save to a HedgeDoc-backed collection was overwritten on the next sync tick with no warning, and a commit into the git worktree was never pushed, so once the remote moved the branch had diverged and every later `git pull --ff-only` failed silently. A collection is now one thing — a top-level folder in your own card tree, discovered by reading the directory and identified by the stable id in its `.hashcards.toml` — so "a card can be edited" is a flat fact about the application rather than a property of where the card came from. Three slug namespaces collapse to one, two database-naming schemes to one, and `{data_dir}/local` is renamed `{data_dir}/cards` now that there is no `{data_dir}/repo` for it to be named against. Review databases stay in `{data_dir}/db` under the names they already have, so no history moves. To bring markdown in, create a file in **My Cards** and paste the text into the editor, which previews through the production parser and validates images before saving. Nothing was deployed, so there is no migration: `[git]`, `[[source]]`, `[[hedgedoc]]` and `[[collection]]` are no longer accepted in `hashcards.toml`.
            </change>
        </removed>
```

- [ ] **Step 5: Fix the changelog's own path references**

`CHANGELOG.xml`'s 0.4.8 entry describes the file manager as living at `{data_dir}/local/{user}` and explains why it sits outside `{data_dir}/repo`. Released entries are history and stay as written — do **not** edit them. The new `<removed>` entry above already records the rename.

In `README.md`, update every `{data_dir}/local` to `{data_dir}/cards` and delete the sentence in the **My Cards** section explaining that the tree sits outside the repo clone.

- [ ] **Step 6: Verify and commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`

```bash
git add -A
git commit -m "refactor: rename the card tree from local/ to cards/

local/ was named in opposition to repo/, which is gone. LocalRoot
becomes CardRoot and local.rs becomes cards.rs. Review databases keep
their names and their location, so no history moves.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RP3W1RufSmp3GWtDuxC763"
```

---

# Part two: session-aware editing

## Task 6: `apply_card_migration`

The migration primitive, unit-testable without a server. It is deliberately infallible: it runs after the database transaction has committed and the file is on disk, so there is nothing left to roll back to.

**Files:**
- Modify: `src/cmd/drill/cache.rs` — add `rekey` and `remove`
- Modify: `src/cmd/drill/state.rs` — add `SessionDbs::rekey_routes`, `CardMigration`, `MigrationEffect`, `MutableState::apply_card_migration`
- Test: `src/cmd/drill/state.rs` (`mod tests`), `src/cmd/drill/cache.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Card`, `CardHash`, `Performance`, `Timestamp` — all unchanged.
- Produces:
  - `Cache::rekey(&mut self, old: CardHash, new: CardHash)` — infallible
  - `Cache::remove(&mut self, card_hash: CardHash)` — infallible
  - `SessionDbs::rekey_routes(&mut self, renamed: &HashMap<CardHash, CardHash>, removed: &HashSet<CardHash>)`
  - `pub struct CardMigration { pub renamed: Vec<(CardHash, Card)>, pub removed: Vec<CardHash> }`
  - `pub struct MigrationEffect { pub renamed: usize, pub dropped: usize, pub session_finished: bool }`
  - `MutableState::apply_card_migration(&mut self, m: &CardMigration) -> MigrationEffect`

- [ ] **Step 1: Write the failing cache tests**

Add to `src/cmd/drill/cache.rs`'s `mod tests`:

```rust
    /// A rename moves the performance to the new hash and forgets the old
    /// one: a session that kept both would answer `get` for a card that no
    /// file contains any more.
    #[test]
    fn test_cache_rekey_moves_the_performance() -> Fallible<()> {
        let mut cache = Cache::new();
        let old = CardHash::hash_bytes(b"old");
        let new = CardHash::hash_bytes(b"new");
        cache.insert(old, Performance::New)?;
        cache.rekey(old, new);
        assert!(cache.get(new)?.is_new());
        assert!(cache.get(old).is_err());
        Ok(())
    }

    /// Rekeying a hash the session never held is a no-op, not an error: an
    /// edit renames cards across a whole file, and a session may hold only
    /// some of them.
    #[test]
    fn test_cache_rekey_of_an_absent_hash_is_a_noop() {
        let mut cache = Cache::new();
        let old = CardHash::hash_bytes(b"old");
        let new = CardHash::hash_bytes(b"new");
        cache.rekey(old, new);
        assert!(cache.get(new).is_err());
    }

    #[test]
    fn test_cache_remove_forgets_the_card() -> Fallible<()> {
        let mut cache = Cache::new();
        let hash = CardHash::hash_bytes(b"a");
        cache.insert(hash, Performance::New)?;
        cache.remove(hash);
        assert!(cache.get(hash).is_err());
        Ok(())
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib cmd::drill::cache`
Expected: FAIL to compile — no method named `rekey` / `remove` on `Cache`.

- [ ] **Step 3: Add `rekey` and `remove` to `Cache`**

In `src/cmd/drill/cache.rs`, after `update`:

```rust
    /// Move a card's performance to a new hash after an edit renamed it.
    ///
    /// Infallible on purpose. `insert` and `update` refuse a missing or
    /// duplicate key, which is right while a session is grading; this runs
    /// after an edit has already been written to disk and committed, where
    /// there is nothing to roll back to and a hash this session never held
    /// is simply not its business. If the target hash is already present —
    /// the database declined that rename as a collision — the new value
    /// wins: the cache mirrors the file, and the database is the record.
    pub fn rekey(&mut self, old: CardHash, new: CardHash) {
        if let Some(performance) = self.changes.remove(&old) {
            self.changes.insert(new, performance);
        }
    }

    /// Forget a card the edit deleted from the corpus.
    pub fn remove(&mut self, card_hash: CardHash) {
        self.changes.remove(&card_hash);
    }
```

- [ ] **Step 4: Run the cache tests to verify they pass**

Run: `cargo test --lib cmd::drill::cache`
Expected: PASS.

- [ ] **Step 5: Write the failing migration tests**

Add to `src/cmd/drill/state.rs`'s `mod tests`. Extend the existing helpers first:

```rust
    /// A card whose text — and therefore hash — differs from `make_card`'s.
    fn make_card_with(question: &str, answer: &str) -> Card {
        Card::new(
            "test-deck".to_string(),
            PathBuf::from("/tmp/deck.md"),
            (1, 2),
            CardContent::new_basic(question, answer),
        )
    }

    /// A session holding `cards`, with every card in the cache as `New` and
    /// a real in-memory database behind it.
    fn session_over(cards: Vec<Card>) -> Fallible<MutableState> {
        let db = Database::new(":memory:")?;
        let session_id = db.create_session(Timestamp::now())?;
        let mut cache = Cache::new();
        for card in &cards {
            // A card queued twice is cached once.
            let _ = cache.insert(card.hash(), Performance::New);
        }
        Ok(MutableState::new(
            SessionDbs::single(db, session_id),
            cache,
            cards,
            Jitter::none(),
            TinyRng::from_seed(1),
        ))
    }
```

then the tests:

```rust
    /// A rename must reach the queue, the undo stack and the cache in one
    /// move. Leaving any one of them on the old hash strands the grades the
    /// session is about to write.
    #[test]
    fn migration_renames_a_card_in_queue_cache_and_reviews() -> Fallible<()> {
        let old = make_card("question a");
        let new = make_card_with("question a", "a better answer");
        let mut mutable = session_over(vec![old.clone()])?;
        mutable.reviews.push(make_review(&old, Grade::Good));

        let effect = mutable.apply_card_migration(&CardMigration {
            renamed: vec![(old.hash(), new.clone())],
            removed: Vec::new(),
        });

        assert_eq!(effect.renamed, 1);
        assert_eq!(effect.dropped, 0);
        assert!(!effect.session_finished);
        assert_eq!(mutable.cards[0].hash(), new.hash());
        assert_eq!(mutable.reviews[0].card.hash(), new.hash());
        assert!(mutable.cache.get(new.hash())?.is_new());
        assert!(mutable.cache.get(old.hash()).is_err());
        Ok(())
    }

    /// A Forgot or Hard grade pushes a card to the back of the queue while
    /// it is also in `reviews`, so the same card is in the queue twice at
    /// the moment an edit lands. Both copies must follow the rename.
    #[test]
    fn migration_renames_every_copy_of_a_requeued_card() -> Fallible<()> {
        let old = make_card("question a");
        let other = make_card("question b");
        let new = make_card_with("question a", "a better answer");
        let mut mutable = session_over(vec![old.clone(), other, old.clone()])?;

        mutable.apply_card_migration(&CardMigration {
            renamed: vec![(old.hash(), new.clone())],
            removed: Vec::new(),
        });

        let renamed = mutable
            .cards
            .iter()
            .filter(|c| c.hash() == new.hash())
            .count();
        assert_eq!(renamed, 2, "both copies of the requeued card follow");
        assert!(!mutable.cards.iter().any(|c| c.hash() == old.hash()));
        Ok(())
    }

    /// Undo restores the performance a `Review` carries, so a review that
    /// travelled with its card must still restore the right one.
    #[test]
    fn migration_keeps_prev_performance_on_a_renamed_review() -> Fallible<()> {
        let old = make_card("question a");
        let new = make_card_with("question a", "a better answer");
        let mut mutable = session_over(vec![old.clone()])?;
        let mut review = make_review(&old, Grade::Good);
        review.prev_performance = Performance::New;
        review.review_id = 42;
        mutable.reviews.push(review);

        mutable.apply_card_migration(&CardMigration {
            renamed: vec![(old.hash(), new.clone())],
            removed: Vec::new(),
        });

        assert_eq!(mutable.reviews.len(), 1);
        assert_eq!(mutable.reviews[0].review_id, 42);
        assert!(mutable.reviews[0].prev_performance.is_new());
        assert_eq!(mutable.reviews[0].card.hash(), new.hash());
        Ok(())
    }

    /// A card the edit deleted leaves the queue, leaves the cache, and its
    /// reviews leave the undo stack: undoing back to a card that is in no
    /// file would put the user in front of something they cannot edit,
    /// re-drill or reach again. `progress()` rewinds with it.
    #[test]
    fn migration_drops_a_removed_card_and_its_reviews() -> Fallible<()> {
        let gone = make_card("question a");
        let kept = make_card("question b");
        let mut mutable = session_over(vec![gone.clone(), kept.clone()])?;
        mutable.reviews.push(make_review(&gone, Grade::Good));
        assert_eq!(mutable.progress(), (1, 0));

        let effect = mutable.apply_card_migration(&CardMigration {
            renamed: Vec::new(),
            removed: vec![gone.hash()],
        });

        assert_eq!(effect.dropped, 1);
        assert_eq!(mutable.cards.len(), 1);
        assert_eq!(mutable.cards[0].hash(), kept.hash());
        assert!(mutable.reviews.is_empty());
        assert!(mutable.cache.get(gone.hash()).is_err());
        assert_eq!(mutable.progress(), (0, 0));
        Ok(())
    }

    /// An edit can empty the queue. Left alone, the GET path would render a
    /// live session with nothing in it, so the session finishes here.
    #[test]
    fn migration_finishes_a_session_it_empties() -> Fallible<()> {
        let gone = make_card("question a");
        let mut mutable = session_over(vec![gone.clone()])?;

        let effect = mutable.apply_card_migration(&CardMigration {
            renamed: Vec::new(),
            removed: vec![gone.hash()],
        });

        assert!(effect.session_finished);
        assert!(mutable.cards.is_empty());
        assert!(mutable.finished_at.is_some());
        Ok(())
    }

    /// A session that started after the file was written already holds the
    /// new hashes. The migration looks up old hashes, finds none, and
    /// changes nothing — which is why the write-then-migrate ordering has
    /// no race in it.
    #[test]
    fn migration_against_a_session_holding_the_new_hashes_is_a_noop() -> Fallible<()> {
        let old = make_card("question a");
        let new = make_card_with("question a", "a better answer");
        let mut mutable = session_over(vec![new.clone()])?;

        let effect = mutable.apply_card_migration(&CardMigration {
            renamed: vec![(old.hash(), new.clone())],
            removed: Vec::new(),
        });

        assert_eq!(effect.renamed, 0);
        assert_eq!(effect.dropped, 0);
        assert!(!effect.session_finished);
        assert_eq!(mutable.cards.len(), 1);
        assert_eq!(mutable.cards[0].hash(), new.hash());
        Ok(())
    }

    /// A card drilled inside a custom deck routes its grades to its own
    /// collection's database. The route must follow the rename, or the
    /// grade after an edit lands in the wrong database.
    #[test]
    fn migration_rekeys_the_database_route() -> Fallible<()> {
        let old = make_card("question a");
        let new = make_card_with("question a", "a better answer");
        let db_a = Database::new(":memory:")?;
        let session_a = db_a.create_session(Timestamp::now())?;
        let db_b = Database::new(":memory:")?;
        let session_b = db_b.create_session(Timestamp::now())?;
        let source = SessionSource {
            coll_dir: PathBuf::from("/tmp/coll"),
            file_url_prefix: "/collection/x/file".to_string(),
        };
        let dbs = SessionDbs::routed(
            vec![
                SessionDb {
                    db: db_a,
                    session_id: session_a,
                    source: Some(source.clone()),
                },
                SessionDb {
                    db: db_b,
                    session_id: session_b,
                    source: Some(source),
                },
            ],
            HashMap::from([(old.hash(), 1)]),
        );
        let mut cache = Cache::new();
        cache.insert(old.hash(), Performance::New)?;
        let mut mutable = MutableState::new(
            dbs,
            cache,
            vec![old.clone()],
            Jitter::none(),
            TinyRng::from_seed(1),
        );

        mutable.apply_card_migration(&CardMigration {
            renamed: vec![(old.hash(), new.clone())],
            removed: Vec::new(),
        });

        // Routing by the new hash must still reach the second database, not
        // fall back to the first.
        assert_eq!(mutable.dbs.for_card(new.hash()).session_id, session_b);
        Ok(())
    }
```

Add the imports these need at the top of `mod tests`: `use std::collections::HashMap;`, `use crate::cmd::drill::state::SessionDb;`, `use crate::cmd::drill::state::SessionSource;`, `use crate::types::performance::Performance;`.

- [ ] **Step 6: Run them to verify they fail**

Run: `cargo test --lib cmd::drill::state`
Expected: FAIL to compile — `CardMigration`, `MigrationEffect` and `apply_card_migration` do not exist.

- [ ] **Step 7: Add `rekey_routes` to `SessionDbs`**

In `src/cmd/drill/state.rs`, inside `impl SessionDbs`:

```rust
    /// Re-key the card-to-database routes after an edit renamed or removed
    /// cards.
    ///
    /// Rebuilt wholesale rather than patched in place: a rename whose target
    /// is also some other rename's source would otherwise give a different
    /// answer depending on iteration order. A single-collection session has
    /// an empty map by construction and needs no work — `for_card` falls
    /// back to the first database.
    pub fn rekey_routes(
        &mut self,
        renamed: &HashMap<CardHash, CardHash>,
        removed: &HashSet<CardHash>,
    ) {
        if self.routes.is_empty() {
            return;
        }
        let mut next: HashMap<CardHash, usize> = HashMap::with_capacity(self.routes.len());
        for (hash, idx) in self.routes.drain() {
            if removed.contains(&hash) {
                continue;
            }
            next.insert(renamed.get(&hash).copied().unwrap_or(hash), idx);
        }
        self.routes = next;
    }
```

- [ ] **Step 8: Add the migration types and `apply_card_migration`**

In `src/cmd/drill/state.rs`, above `impl MutableState`:

```rust
/// What an edit did to a collection's cards, in the terms a live session
/// needs: which hashes were renamed to which cards, and which cards the
/// edit removed from the corpus outright.
pub struct CardMigration {
    /// Old hash to the re-parsed card that replaces it.
    pub renamed: Vec<(CardHash, Card)>,
    /// Cards the edit removed from the corpus.
    pub removed: Vec<CardHash>,
}

/// What the migration did to one session, for the post-save flash.
pub struct MigrationEffect {
    pub renamed: usize,
    pub dropped: usize,
    pub session_finished: bool,
}
```

and inside `impl MutableState`:

```rust
    /// Re-key this session onto the cards an edit produced.
    ///
    /// A live session holds card identity in four places: the queue (where
    /// the same card may appear twice, a Forgot or Hard grade having pushed
    /// it to the back while it is also in `reviews`), the undo stack, the
    /// performance cache, and the per-card database routes. An edit renames
    /// a hash, and it must move all four together or none.
    ///
    /// Deliberately infallible. It runs after the database transaction has
    /// committed and the file is on disk, so there is nothing left to roll
    /// back to: a failure here could only be reported, never repaired.
    ///
    /// A removed card leaves the queue, leaves the cache, and its reviews
    /// leave the undo stack. Grades already written stay written — they
    /// happened, and the history stays attached to the hash that existed at
    /// the time — but undoing back to a card that is in no file would put
    /// the user in front of something they cannot edit, re-drill or reach
    /// again, and a further grade on it would land on a hash orphaned from
    /// every file.
    pub fn apply_card_migration(&mut self, m: &CardMigration) -> MigrationEffect {
        let renames: HashMap<CardHash, Card> = m
            .renamed
            .iter()
            .map(|(old, card)| (*old, card.clone()))
            .collect();
        let rename_hashes: HashMap<CardHash, CardHash> = m
            .renamed
            .iter()
            .map(|(old, card)| (*old, card.hash()))
            .collect();
        let removed: HashSet<CardHash> = m.removed.iter().copied().collect();

        // Counted before anything moves, and over what the *session* holds:
        // an edit renames cards across a whole file, and this session may
        // hold only some of them.
        let held: HashSet<CardHash> = self
            .cards
            .iter()
            .map(|c| c.hash())
            .chain(self.reviews.iter().map(|r| r.card.hash()))
            .collect();
        let renamed = m
            .renamed
            .iter()
            .filter(|(old, _)| held.contains(old))
            .count();
        let dropped = m.removed.iter().filter(|h| held.contains(h)).count();

        let head_before: Option<CardHash> = self.cards.first().map(|c| c.hash());

        // The queue, every copy of every card.
        let mut cards: Vec<Card> = Vec::with_capacity(self.cards.len());
        for card in self.cards.drain(..) {
            let hash = card.hash();
            match renames.get(&hash) {
                Some(replacement) => cards.push(replacement.clone()),
                None if removed.contains(&hash) => {}
                None => cards.push(card),
            }
        }
        self.cards = cards;

        // The undo stack. `prev_performance` travels with its review, so
        // undo across a rename still restores the right performance.
        self.reviews.retain(|r| !removed.contains(&r.card.hash()));
        for review in &mut self.reviews {
            if let Some(replacement) = renames.get(&review.card.hash()) {
                review.card = replacement.clone();
            }
        }

        // The cache and the database routes.
        for (old, card) in &m.renamed {
            self.cache.rekey(*old, card.hash());
        }
        for hash in &m.removed {
            self.cache.remove(*hash);
        }
        self.dbs.rekey_routes(&rename_hashes, &removed);

        // The rendered page carries the head card's answer. If the head is
        // the same card under a new hash, the reveal still describes what is
        // on screen; if a different card is now in front, it does not.
        let head_after: Option<CardHash> = self.cards.first().map(|c| c.hash());
        let head_followed = match head_before {
            Some(before) => head_after == Some(rename_hashes.get(&before).copied().unwrap_or(before)),
            None => head_after.is_none(),
        };
        if !head_followed {
            self.reveal = false;
            self.card_shown_at = None;
        }

        // An edit can empty the queue. Left running, the GET path would
        // render a live session with no card in it.
        let mut session_finished = false;
        if self.cards.is_empty() && self.finished_at.is_none() {
            let ended_at = Timestamp::now();
            if let Err(e) = self.dbs.close_all(ended_at) {
                log::error!("Could not close the session an edit emptied: {e}");
            }
            self.finished_at = Some(ended_at);
            session_finished = true;
        }

        MigrationEffect {
            renamed,
            dropped,
            session_finished,
        }
    }
```

- [ ] **Step 9: Run the migration tests to verify they pass**

Run: `cargo test --lib cmd::drill::state`
Expected: PASS, all seven new tests plus `test_progress_counts_first_grades_and_repeats`.

- [ ] **Step 10: Verify the whole suite and commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`

```bash
git add src/cmd/drill/cache.rs src/cmd/drill/state.rs
git commit -m "feat: re-key a live drill session onto edited cards

A session keys card identity in four places -- the queue (where a card
requeued by Forgot or Hard appears twice), the undo stack, the
performance cache, and the per-card database routes. An edit renames a
hash and must move all four together or none, which is why editing was
refused mid-session.

apply_card_migration is infallible on purpose: it runs after the
transaction has committed and the file is on disk, so there is nothing
left to roll back to. Hence Cache::rekey rather than a loop over the
fallible insert/update pair.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RP3W1RufSmp3GWtDuxC763"
```

---

## Task 7: `sessions_touching` and the three guards

Fixes a live bug: sessions are keyed by slug, and a custom deck has its own slug, so `sessions.lock().contains_key(slug)` checks the *collection's* slug and misses a running custom-deck session that includes it — at `edit.rs`, and at both file-manager guards.

**Files:**
- Modify: `src/cmd/serve/state.rs` — add `sessions_touching`
- Modify: `src/cmd/serve/edit.rs` — the guard at the top of `edit_post_inner`, and `active_session` in `edit_get_inner`
- Modify: `src/cmd/serve/files.rs` — `refuse_if_drilling`, `save_file`'s guard, `owning_collection_slug` → `owning_collection_folder`
- Test: `src/cmd/serve/files.rs` (`mod tests`)

**Interfaces:**
- Consumes: `SharedSession`, `DrillSession`, `AppState`, `SessionDb.source` — all existing.
- Produces:
  - `pub fn sessions_touching(state: &AppState, coll_dir: &Path) -> Vec<SharedSession>` in `crate::cmd::serve::state`
  - `fn owning_collection_folder(root: &CardRoot, rel: &str) -> Option<PathBuf>` in `files.rs`
  - `refuse_if_drilling(state: &AppState, root: &CardRoot, rel: &str) -> Fallible<()>`

- [ ] **Step 1: Write the failing regression test**

Add to `src/cmd/serve/files.rs`'s `mod tests`. It needs a variant of the existing `start_session` helper that builds a *routed* session under a deck slug:

```rust
    /// Start a session keyed by `deck_slug` that draws its cards from
    /// `folder` — the shape a custom-deck drill has. `SessionDb.source`
    /// carries the collection, which is the only place that fact is
    /// recorded: the sessions map is keyed by the deck's own slug.
    fn start_deck_session(
        state: &AppState,
        data_dir: &Path,
        folder: &Path,
        deck_slug: &str,
    ) -> Fallible<()> {
        use crate::cmd::drill::state::MutableState;
        use crate::cmd::drill::state::SessionDb;
        use crate::cmd::drill::state::SessionDbs;
        use crate::cmd::drill::state::SessionSource;
        use crate::cmd::serve::state::DrillSession;
        use crate::rng::TinyRng;
        use crate::types::performance::Jitter;

        let db_dir = data_dir.join("db");
        ensure_dir(&db_dir, "review database directory")?;
        let db_path = db_path_for(folder, &db_dir)?;
        let db_str = match db_path.to_str() {
            Some(p) => p,
            None => return fail("temp path is not UTF-8"),
        };
        let started_at = Timestamp::now();
        let db = Database::new(db_str)?;
        let session_id = db.create_session(started_at)?;
        let dbs = SessionDbs::routed(
            vec![SessionDb {
                db,
                session_id,
                source: Some(SessionSource {
                    coll_dir: folder.to_path_buf(),
                    file_url_prefix: format!("/collection/{deck_slug}/file"),
                }),
            }],
            std::collections::HashMap::new(),
        );
        let mutable = MutableState::new(
            dbs,
            crate::cmd::drill::cache::Cache::new(),
            Vec::new(),
            Jitter::none(),
            TinyRng::from_seed(1),
        );
        let session = std::sync::Arc::new(parking_lot::Mutex::new(DrillSession::new(
            // A deck session's own directory is not any one collection's.
            data_dir.to_path_buf(),
            Vec::new(),
            started_at,
            AnswerControls::Full,
            mutable,
        )));
        state.sessions.lock().insert(deck_slug.to_string(), session);
        Ok(())
    }

    /// A custom-deck session is keyed by the deck's slug, not the
    /// collection's, so the guard that looked the collection slug up in the
    /// sessions map never saw it: deleting the collection unlinked the
    /// database that session still held open.
    #[test]
    fn a_delete_is_refused_while_a_custom_deck_session_uses_the_collection() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        let state = state_for(&dir);
        let root = user_root(&state, None)?;
        let folder = root.path().join("Spanish");
        std::fs::create_dir_all(&folder)?;
        std::fs::write(folder.join("verbs.md"), "Q: the cat\nA: el gato\n")?;

        start_deck_session(&state, &dir, &folder, "deck-exam-0badc0de")?;

        assert!(
            delete_entry(
                &state,
                None,
                &DeleteForm {
                    path: "Spanish".to_string(),
                },
            )
            .is_err(),
            "a deck session drawing on this collection must block the delete"
        );
        assert!(folder.exists());
        Ok(())
    }

    /// Same bug, same shape, on the rename path.
    #[test]
    fn a_rename_is_refused_while_a_custom_deck_session_uses_the_collection() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        let state = state_for(&dir);
        let root = user_root(&state, None)?;
        let folder = root.path().join("Spanish");
        std::fs::create_dir_all(&folder)?;
        std::fs::write(folder.join("verbs.md"), "Q: the cat\nA: el gato\n")?;

        start_deck_session(&state, &dir, &folder, "deck-exam-0badc0de")?;

        assert!(
            rename_entry(
                &state,
                None,
                &RenameForm {
                    path: "Spanish".to_string(),
                    name: "Espanol".to_string(),
                },
            )
            .is_err(),
            "a deck session drawing on this collection must block the rename"
        );
        assert!(folder.exists());
        Ok(())
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test custom_deck_session_uses_the_collection`
Expected: FAIL — both assertions fail because `delete_entry` and `rename_entry` return `Ok`, having looked up the slug `Spanish` in a map keyed `deck-exam-0badc0de`.

- [ ] **Step 3: Add `sessions_touching`**

In `src/cmd/serve/state.rs`, after `evict_idle_sessions`:

```rust
/// Every live session drawing cards from `coll_dir`.
///
/// Answered from the sessions themselves rather than from deck
/// configuration: a session holds what it actually loaded when it started,
/// which is the question being asked, and the configuration may have moved
/// since. It also closes a bug at its root — sessions are keyed by slug and
/// a custom deck has its own slug, so looking the *collection's* slug up in
/// the map misses a running deck session that includes it.
///
/// A session drawing on several collections records each one in its
/// `SessionDb.source`. A single-collection session leaves every `source`
/// as `None` and is identified by its own directory instead.
///
/// Follows the map-then-session lock order: the map lock is released before
/// any session lock is taken.
pub fn sessions_touching(state: &AppState, coll_dir: &Path) -> Vec<SharedSession> {
    let candidates: Vec<SharedSession> = state.sessions.lock().values().cloned().collect();
    candidates
        .into_iter()
        .filter(|shared| {
            let session = shared.lock();
            if session.is_detached() {
                return false;
            }
            let mut routed = false;
            for entry in session.mutable.dbs.all() {
                if let Some(source) = &entry.source {
                    routed = true;
                    if same_dir(&source.coll_dir, coll_dir) {
                        return true;
                    }
                }
            }
            !routed && same_dir(&session.directory, coll_dir)
        })
        .collect()
}

/// Whether two paths name the same directory.
///
/// Canonicalized first: one caller has the path the collection was
/// discovered at and another has it after `canonicalize`, and a symlinked
/// or `..`-bearing spelling of the same directory must not read as a
/// different collection. A path that cannot be canonicalized — it was just
/// deleted, say — falls back to a literal comparison.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
```

Add `use std::path::Path;` to the imports.

- [ ] **Step 4: Widen the file-manager guards**

In `src/cmd/serve/files.rs`, replace `owning_collection_slug` and `refuse_if_drilling`:

```rust
/// The folder of the collection a normalized path belongs to, if it belongs
/// to one.
///
/// Unlike `collection_folder`, the path may *be* the collection folder:
/// deleting or renaming a collection touches its cards just as much as
/// editing one of them does. A loose file in the root belongs to no
/// collection, so nothing can be drilling it.
fn owning_collection_folder(root: &CardRoot, rel: &str) -> Option<PathBuf> {
    let top = rel.split('/').next().filter(|t| !t.is_empty())?;
    let folder = root.resolve(top).ok()?;
    folder.is_dir().then_some(folder)
}

/// Refuse a change to a collection somebody is drilling right now.
///
/// Migration handles *edits*; it cannot handle a collection or file that
/// stopped existing. A session cannot be re-keyed onto cards that are gone,
/// and deleting a collection unlinks the database the session still holds
/// open, so every grade it writes after that goes to a file nothing can
/// read back.
///
/// `rel` must already be normalized, so that where the file is and which
/// collection owns it are answered from the same string.
fn refuse_if_drilling(state: &AppState, root: &CardRoot, rel: &str) -> Fallible<()> {
    let Some(folder) = owning_collection_folder(root, rel) else {
        return Ok(());
    };
    if sessions_touching(state, &folder).is_empty() {
        return Ok(());
    }
    fail("A drill session is using this collection's cards. End it before changing its files.")
}
```

Update both call sites, which already hold `root`:
- `rename_entry`: `refuse_if_drilling(state, &root, &from_rel)?;`
- `delete_entry`: `refuse_if_drilling(state, &root, &rel)?;`

Add `use crate::cmd::serve::state::sessions_touching;` to `files.rs`.

- [ ] **Step 5: Run the regression tests to verify they pass**

Run: `cargo test custom_deck_session_uses_the_collection`
Expected: PASS.

- [ ] **Step 6: Update the two remaining slug-keyed reads**

These are replaced properly in Tasks 8 and 9; for now they must at least see deck sessions.

In `edit.rs`, `edit_get_inner`:

```rust
    let active_session = !sessions_touching(state, &coll_dir).is_empty();
```

In `edit.rs`, `edit_post_inner`:

```rust
    if !sessions_touching(state, &coll_dir).is_empty() {
        return fail("A drill session is active. End it before editing.");
    }
```

(both after `let coll_dir = rc.coll_dir.canonicalize()?;`), and in `files.rs`, `save_file`, replace the `collection_slug_for` guard:

```rust
    // A live session drills the cards it cached when it started. Rewriting
    // their hashes underneath it strands the grades it is about to write.
    let coll_dir = collection_folder(&root, rel)?;
    if !sessions_touching(state, &coll_dir).is_empty() {
        return fail("A drill session is active on this collection. End it before editing.");
    }
```

moving the existing `let coll_dir = collection_folder(&root, rel)?;` binding up to here and deleting the later duplicate. `collection_slug_for` now has no callers — delete it and the `LOOSE_FILE` reference it carried, if `LOOSE_FILE` is otherwise unused.

- [ ] **Step 7: Verify and commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS, two more tests than after Task 6.

```bash
git add src/cmd/serve/state.rs src/cmd/serve/edit.rs src/cmd/serve/files.rs
git commit -m "fix: find drill sessions by what they loaded, not by slug

Sessions are keyed by slug and a custom deck has its own slug, so
looking the collection's slug up in the sessions map missed a running
deck session drawing on that collection. Deleting the collection then
unlinked a database the session still held open, and every grade it
wrote afterwards went to a file nothing could read back.

sessions_touching asks each session which collections it actually
loaded -- SessionDb.source for a routed session, DrillSession.directory
for a single-collection one.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RP3W1RufSmp3GWtDuxC763"
```

---

## Task 8: Migrate sessions from the card editor

Replaces the refusal at the top of `edit_post_inner` with a migration after the transaction commits.

**Files:**
- Modify: `src/cmd/serve/state.rs` — add `migrate_sessions`
- Modify: `src/cmd/serve/edit.rs` — `build_card_migration`, `EditOutcome`, `edit_post_inner`, `edit_post_handler`
- Test: `src/cmd/serve/mod.rs` (`mod tests`) — end to end

**Interfaces:**
- Consumes: `sessions_touching` (Task 7), `CardMigration`, `MigrationEffect`, `apply_card_migration` (Task 6), `MigrationPlan` (existing, `edit.rs`).
- Produces:
  - `pub fn migrate_sessions(state: &AppState, coll_dir: &Path, migration: &CardMigration) -> MigrationEffect` in `crate::cmd::serve::state`
  - `pub struct EditOutcome { pub migrated: usize, pub skipped: usize, pub session: MigrationEffect }`
  - `fn build_card_migration(plan: &MigrationPlan, old_cards: &[&Card], new_cards: &[Card]) -> CardMigration` in `edit.rs`, `pub(crate)` so Task 9 can reuse it

- [ ] **Step 1: Write the failing end-to-end test**

Add to `src/cmd/serve/mod.rs`'s `mod tests`:

```rust
    /// Editing the card you are drilling used to be refused outright. Now
    /// the session follows the card to its new hash, so the grade that
    /// comes next lands on the card that is actually on screen — and the
    /// old card's review history came with it.
    #[tokio::test]
    async fn test_a_card_can_be_edited_mid_session_and_then_graded() -> Fallible<()> {
        let (port, dir) = spawn_test_server(
            "Spanish",
            &[("verbs.md", "Q: the cat\nA: el gato\n")],
        )
        .await?;
        let base = format!("http://{TEST_HOST}:{port}");
        let client = reqwest::Client::builder().cookie_store(true).build()?;

        // Start a drill on the whole collection.
        client
            .post(format!("{base}/collection/Spanish/start"))
            .form(&[("decks", "verbs")])
            .send()
            .await?;
        let page = client
            .get(format!("{base}/collection/Spanish"))
            .send()
            .await?
            .text()
            .await?;
        let old_hash = extract_card_hash(&page)?;

        // Reveal, then edit the card that is on screen.
        client
            .post(format!("{base}/collection/Spanish"))
            .form(&[("action", "Reveal"), ("card", old_hash.as_str())])
            .send()
            .await?;

        let form = client
            .get(format!("{base}/collection/Spanish/edit/{old_hash}"))
            .send()
            .await?
            .text()
            .await?;
        let mtime = extract_input_value(&form, "mtime_ms")?;
        let save = client
            .post(format!("{base}/collection/Spanish/edit/{old_hash}"))
            .form(&[
                ("new_text", "Q: the cat\nA: el gato (masc.)"),
                ("mtime_ms", mtime.as_str()),
            ])
            .send()
            .await?;
        assert!(save.status().is_success(), "save returned {}", save.status());

        // The session is still live, and now shows the edited card.
        let page = client
            .get(format!("{base}/collection/Spanish"))
            .send()
            .await?
            .text()
            .await?;
        let new_hash = extract_card_hash(&page)?;
        assert_ne!(new_hash, old_hash, "the edit must have renamed the card");

        // Grading it must be accepted, not answered with "already graded".
        client
            .post(format!("{base}/collection/Spanish"))
            .form(&[("action", "Reveal"), ("card", new_hash.as_str())])
            .send()
            .await?;
        client
            .post(format!("{base}/collection/Spanish"))
            .form(&[("action", "Good"), ("card", new_hash.as_str())])
            .send()
            .await?;

        // The review landed on the new hash, in the collection's own
        // database, with the old card's row carried over.
        let folder = dir.path().join("cards").join("default").join("Spanish");
        let id = crate::cmd::serve::cards::existing_collection_id(&folder)?
            .ok_or_else(|| ErrorReport::new("the collection has no id"))?;
        let db_path = dir.path().join("db").join(format!("{id}.db"));
        let db_str = match db_path.to_str() {
            Some(p) => p,
            None => return fail("temp path is not UTF-8"),
        };
        let db = Database::new(db_str)?;
        let hash = CardHash::from_hex(&new_hash)?;
        assert!(db.card_exists(hash)?, "the edited card has no row");
        match db.get_card_performance_opt(hash)? {
            Some(Performance::Reviewed(rp)) => assert_eq!(
                rp.review_count, 1,
                "the grade did not land on the edited card"
            ),
            other => {
                return fail(format!(
                    "the edited card was never reviewed: {}",
                    if other.is_some() { "still New" } else { "no row" }
                ));
            }
        }
        Ok(())
    }
```

with two small parsers next to `spawn_test_server`:

```rust
    /// The hash of the card the drill page is showing, out of its hidden
    /// `card` input.
    fn extract_card_hash(html: &str) -> Fallible<String> {
        extract_input_value(html, "card")
    }

    /// The `value` of the first `<input name="{name}" … value="…">` on the
    /// page. Deliberately crude: it exists so a test can act as a browser
    /// would, not to parse HTML in general.
    fn extract_input_value(html: &str, name: &str) -> Fallible<String> {
        let needle = format!("name=\"{name}\"");
        let after = match html.split_once(&needle) {
            Some((_, rest)) => rest,
            None => return fail(format!("no input named `{name}` on the page")),
        };
        let after = match after.split_once("value=\"") {
            Some((_, rest)) => rest,
            None => return fail(format!("input `{name}` has no value")),
        };
        match after.split_once('"') {
            Some((value, _)) => Ok(value.to_string()),
            None => fail(format!("input `{name}` has an unterminated value")),
        }
    }
```

Add the imports these need: `crate::types::card_hash::CardHash`, `crate::types::performance::Performance`, `crate::error::ErrorReport`, `crate::error::fail`, `tempfile::TempDir`. `Database::card_exists` is at `db.rs:613` and `get_card_performance_opt` at `db.rs:188`; `ReviewedPerformance.review_count` is a public field. Do not add a database method for this test.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test test_a_card_can_be_edited_mid_session_and_then_graded`
Expected: FAIL — the save returns an error page reading "A drill session is active. End it before editing."

- [ ] **Step 3: Add `migrate_sessions`**

In `src/cmd/serve/state.rs`, after `sessions_touching`:

```rust
/// Apply `migration` to every live session drawing on `coll_dir`, and
/// report the total effect.
///
/// Called only after the file has been written and the database
/// transaction has committed, which is what lets it be infallible.
///
/// A session that started between the write and this call looks like a
/// race and is not: a session parses its collection when it starts, so one
/// beginning after the write already holds the new hashes, and the
/// migration — which looks up old ones — finds nothing and does nothing. A
/// session that began before the write is in the map.
pub fn migrate_sessions(
    state: &AppState,
    coll_dir: &Path,
    migration: &CardMigration,
) -> MigrationEffect {
    let mut total = MigrationEffect {
        renamed: 0,
        dropped: 0,
        session_finished: false,
    };
    for shared in sessions_touching(state, coll_dir) {
        let mut session = shared.lock();
        let effect = session.mutable.apply_card_migration(migration);
        // The progress bar's denominator counts the cards the session set
        // out with; an edit that removed some must not leave it stuck.
        session.total_cards = session.total_cards.saturating_sub(effect.dropped);
        total.renamed += effect.renamed;
        total.dropped += effect.dropped;
        total.session_finished |= effect.session_finished;
    }
    total
}
```

Add `use crate::cmd::drill::state::CardMigration;` and `use crate::cmd::drill::state::MigrationEffect;`.

- [ ] **Step 4: Build the migration in `edit.rs`**

Add to `src/cmd/serve/edit.rs`, next to `plan_hash_migration`:

```rust
/// Join a `MigrationPlan` with the re-parsed cards, in the terms a live
/// session needs.
///
/// `plan.renames` already pairs old and new *hashes*; this looks each new
/// hash up among the re-parsed cards so the session gets the whole card,
/// and works out which old cards the edit removed: the ones that were
/// neither renamed nor still present under their own hash.
pub(crate) fn build_card_migration(
    plan: &MigrationPlan,
    old_cards: &[&Card],
    new_cards: &[Card],
) -> CardMigration {
    let by_hash: HashMap<CardHash, &Card> = new_cards.iter().map(|c| (c.hash(), c)).collect();
    let renamed: Vec<(CardHash, Card)> = plan
        .renames
        .iter()
        .filter_map(|(old, new)| by_hash.get(new).map(|card| (*old, (*card).clone())))
        .collect();
    let renamed_from: HashSet<CardHash> = plan.renames.iter().map(|(old, _)| *old).collect();
    let removed: Vec<CardHash> = old_cards
        .iter()
        .map(|c| c.hash())
        .filter(|h| !by_hash.contains_key(h) && !renamed_from.contains(h))
        .collect();
    CardMigration { renamed, removed }
}
```

Add `use std::collections::HashMap;`, `use std::collections::HashSet;`, `use crate::cmd::drill::state::CardMigration;`.

- [ ] **Step 5: Wire it into `edit_post_inner`**

Delete the guard added in Task 7 Step 6:

```rust
    if !sessions_touching(state, &coll_dir).is_empty() {
        return fail("A drill session is active. End it before editing.");
    }
```

and replace the tail of the function (from the `let counts = match db.apply_edit_migration(…)` block onwards) with:

```rust
    let counts = match db.apply_edit_migration(&plan.renames, &plan.fresh, now) {
        Ok(counts) => counts,
        Err(e) => {
            revert_file(&file_path, &file_content)?;
            return fail(format!(
                "Edit reverted: the review history could not be updated: {e}"
            ));
        }
    };

    // The file is written and the transaction has committed, so nothing
    // below can fail: a live session is re-keyed onto the cards that now
    // exist, or it is not, and there is no state left to roll back to.
    let migration = build_card_migration(&plan, &old_cards, &new_cards);
    let session = migrate_sessions(state, &coll_dir, &migration);

    Ok(EditOutcome {
        migrated: counts.renamed,
        // A rename the database declined as a true collision (its target
        // hash already has history) leaves the old row unmatched, which the
        // user should hear about. A rename whose old hash had no history of
        // its own is not: there was nothing to lose.
        skipped: plan.skipped + counts.collided,
        session,
    })
```

and change `EditOutcome`:

```rust
/// What a successful edit did, for user-facing reporting.
pub struct EditOutcome {
    /// Cards whose review history was migrated to a new hash.
    pub migrated: usize,
    /// New cards that could not be matched to prior history and start fresh.
    pub skipped: usize,
    /// What the edit did to any live drill session on this collection.
    pub session: MigrationEffect,
}
```

Add `use crate::cmd::drill::state::MigrationEffect;` and `use crate::cmd::serve::state::migrate_sessions;`.

- [ ] **Step 6: Report it in the flash**

In `edit_post_handler`:

```rust
        Ok(outcome) => {
            log::debug!(
                "Edit saved: {} card(s) migrated, {} skipped, {} session card(s) renamed, {} dropped",
                outcome.migrated,
                outcome.skipped,
                outcome.session.renamed,
                outcome.session.dropped
            );
            let target = format!("/collection/{slug}/bookmarks");
            let mut msg = String::from("Card saved.");
            if outcome.session.renamed > 0 || outcome.session.dropped > 0 {
                msg.push_str(" Your running session was updated.");
            }
            if outcome.session.session_finished {
                msg.push_str(" It has no cards left, so it is finished.");
            }
            let flash = if outcome.skipped > 0 {
                Flash::error(format!(
                    "{msg} {} card(s) could not be matched to their previous review history and will start fresh.",
                    outcome.skipped
                ))
            } else {
                Flash::success(msg)
            };
            Ok(flash.redirect(&target))
        }
```

(`target` becomes `return_to`-aware in Task 10.)

- [ ] **Step 7: Drop the warning banner**

In `edit_get_inner`, delete `let active_session = …;` and drop the argument from `render_edit_form`. In `render_edit_form`, delete the `active_session: bool` parameter and the `@if active_session { div.edit-warning { … } }` block: the advice it gave — "End it before saving to avoid stale state" — is now false.

Delete the `.edit-warning` rule from `src/cmd/drill/style.css` if nothing else uses it:

```bash
grep -rn "edit-warning" src/
```

- [ ] **Step 8: Run the end-to-end test to verify it passes**

Run: `cargo test test_a_card_can_be_edited_mid_session_and_then_graded`
Expected: PASS.

- [ ] **Step 9: Verify and commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`

```bash
git add src/cmd/serve/state.rs src/cmd/serve/edit.rs src/cmd/serve/mod.rs src/cmd/drill/style.css
git commit -m "feat: edit a card without ending the drill session

The card editor refused to save while a session was running, because an
edit renames a hash and the session keys card identity in four places.
It now writes the file, commits the review-history transaction, and then
re-keys every session drawing on that collection -- after which nothing
can fail, so the migration is infallible by construction.

The banner advising the user to end the session first is gone; the
post-save flash says what actually happened instead.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RP3W1RufSmp3GWtDuxC763"
```

---

## Task 9: Migrate sessions from the whole-file editor

`save_file` already reuses the card editor's plan and transaction, so it gets the migration for the same price.

**Files:**
- Modify: `src/cmd/serve/files.rs` — `save_file`
- Test: `src/cmd/serve/files.rs` (`mod tests`)

**Interfaces:**
- Consumes: `build_card_migration` and `migrate_sessions` from Task 8.
- Produces: no new public API. `save_file` returns the same `Fallible<String>` message, now with a sentence about the session when one was touched.

- [ ] **Step 1: Write the failing test**

Add to `src/cmd/serve/files.rs`'s `mod tests`:

```rust
    /// The whole-file editor refused to save while a session was running,
    /// for the same reason the card editor did. It now re-keys the session
    /// onto the rewritten file instead.
    #[test]
    fn a_save_migrates_a_running_session_instead_of_refusing() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        let state = state_for(&dir);
        let root = user_root(&state, None)?;
        let folder = root.path().join("Spanish");
        std::fs::create_dir_all(&folder)?;
        let path = folder.join("verbs.md");
        std::fs::write(&path, "Q: the cat\nA: el gato\n")?;

        start_session(&state, &dir, &folder, "Spanish")?;

        let saved = save_file(
            &state,
            None,
            "Spanish/verbs.md",
            &SaveForm {
                content: "Q: the cat\nA: el gato (masc.)\n".to_string(),
                mtime: file_mtime_ms(&path)?,
            },
        )?;
        assert!(saved.starts_with("Saved"), "got: {saved}");
        assert!(
            std::fs::read_to_string(&path)?.contains("masc."),
            "the edit is not on disk"
        );
        Ok(())
    }
```

Delete the test it replaces, `a_save_is_refused_while_a_session_drills_the_collection`: the behaviour it pins is exactly what this task removes.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test a_save_migrates_a_running_session_instead_of_refusing`
Expected: FAIL — `save_file` returns "A drill session is active on this collection. End it before editing."

- [ ] **Step 3: Replace the guard with a migration**

In `save_file`, delete:

```rust
    let coll_dir = collection_folder(&root, rel)?;
    if !sessions_touching(state, &coll_dir).is_empty() {
        return fail("A drill session is active on this collection. End it before editing.");
    }
```
keeping the `let coll_dir = collection_folder(&root, rel)?;` binding, which the rest of the function needs.

Then, after the existing transaction (`db.apply_edit_migration(&plan.renames, &plan.fresh, now)`) has succeeded and before the function returns its message, add:

```rust
    // Written and committed: from here the session is re-keyed onto the
    // cards that now exist, and nothing left can fail.
    let old_refs: Vec<&Card> = old_cards.iter().collect();
    let migration = build_card_migration(&plan, &old_refs, &new_cards);
    let session = migrate_sessions(state, &coll_dir, &migration);
```

and fold it into the two-branch message the function already ends with, keeping both branches intact and appending to whichever one ran:

```rust
    // Only mention unmatched cards when there was history to lose: on a file
    // nobody has drilled, every card is "unmatched" and saying so is noise.
    let skipped = plan.skipped + counts.collided;
    let worth_reporting = skipped > 0 && any_card_has_history(db_path_str, &old_cards)?;
    let mut message = if worth_reporting {
        format!(
            "Saved {} cards. {skipped} could not be matched to their old review history and start fresh.",
            new_cards.len()
        )
    } else {
        format!("Saved {} cards.", new_cards.len())
    };
    if session.renamed > 0 || session.dropped > 0 {
        message.push_str(" Your running session was updated.");
    }
    if session.session_finished {
        message.push_str(" It has no cards left, so it is finished.");
    }
    Ok(message)
```

Add `use crate::cmd::serve::edit::build_card_migration;` and `use crate::cmd::serve::state::migrate_sessions;`.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test a_save_migrates_a_running_session_instead_of_refusing`
Expected: PASS.

- [ ] **Step 5: Verify and commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`

```bash
git add src/cmd/serve/files.rs
git commit -m "feat: save a file without ending the drill session

The whole-file editor already reused the card editor's plan and its
transaction; it now reuses the session migration too. Delete and rename
still refuse, because a session cannot be re-keyed onto cards that are
gone and deleting a collection unlinks a database it still holds open.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RP3W1RufSmp3GWtDuxC763"
```

---

## Task 10: Where editing is reachable from

The pencil beside the star, the topic-tree edit link, `return_to`, and the tooltip that stopped being true.

**Files:**
- Modify: `src/cmd/serve/edit.rs` — `ReturnTo`, both handlers, `render_edit_form`
- Modify: `src/cmd/serve/bookmarks.rs` — the existing Edit link gains `?return_to=bookmarks`
- Modify: `src/cmd/drill/get.rs` — `RenderContext.slug`, `edit_button`, the header, the bookmark tooltip
- Modify: `src/cmd/serve/handlers.rs` — the `RenderContext` literal
- Modify: `src/cmd/serve/browse.rs` — `BrowseData.edit_paths`, the per-topic edit link
- Modify: `src/cmd/drill/style.css` — the header's third column
- Modify: `CHANGELOG.xml`
- Test: `src/cmd/serve/edit.rs`, `src/cmd/serve/browse.rs`

**Interfaces:**
- Consumes: everything from Tasks 8 and 9; `href::encoded_path`.
- Produces:
  - `pub enum ReturnTo { Bookmarks, Collection }` with `ReturnTo::parse(raw: Option<&str>) -> Self`, `ReturnTo::target(self, slug: &str) -> String`, `ReturnTo::as_str(self) -> &'static str`
  - `RenderContext` gains `pub slug: &'a str`
  - `BrowseData` gains `pub edit_paths: HashMap<String, PathBuf>` — topic name to the file its cards came from, relative to the collection folder
  - `pub fn render_browse_page(collection_name: &str, slug: &str, browse: &BrowseData, bookmark_count: usize, interrupted_closed: usize, flash: Option<Flash>) -> Markup` — unchanged shape; the edit paths ride in `browse`

- [ ] **Step 1: Write the failing `ReturnTo` test**

Add to `src/cmd/serve/edit.rs` a `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A closed set, not a caller-supplied path: a redirect target taken
    /// from a form field is an open redirect for no benefit.
    #[test]
    fn return_to_is_a_closed_set() {
        assert_eq!(
            ReturnTo::parse(Some("collection")).target("Spanish"),
            "/collection/Spanish"
        );
        assert_eq!(
            ReturnTo::parse(Some("bookmarks")).target("Spanish"),
            "/collection/Spanish/bookmarks"
        );
        // Anything else is the bookmarks page, not a 400 and not the value.
        for raw in [None, Some(""), Some("https://evil.example.com"), Some("//x")] {
            assert_eq!(
                ReturnTo::parse(raw).target("Spanish"),
                "/collection/Spanish/bookmarks",
                "for {raw:?}"
            );
        }
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test return_to_is_a_closed_set`
Expected: FAIL to compile — `ReturnTo` does not exist.

- [ ] **Step 3: Add `ReturnTo`**

In `src/cmd/serve/edit.rs`, above the GET handler:

```rust
/// Where a card edit returns to when it is saved.
///
/// Two values, not a caller-supplied path: a redirect target taken from a
/// query string or a form field is an open redirect, and these cover every
/// entry point the editor has. `/collection/{slug}` renders the drill when
/// a session is live and the topic browser when it is not, so the pencil in
/// a session and the edit link on the topic tree can share one value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReturnTo {
    Bookmarks,
    Collection,
}

impl ReturnTo {
    /// Anything unrecognized is the bookmarks page. A bad value in a URL is
    /// not worth an error page: the user still gets somewhere sensible.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("collection") => ReturnTo::Collection,
            _ => ReturnTo::Bookmarks,
        }
    }

    pub fn target(self, slug: &str) -> String {
        match self {
            ReturnTo::Collection => format!("/collection/{slug}"),
            ReturnTo::Bookmarks => format!("/collection/{slug}/bookmarks"),
        }
    }

    /// The spelling that round-trips through the form's hidden field.
    pub fn as_str(self) -> &'static str {
        match self {
            ReturnTo::Collection => "collection",
            ReturnTo::Bookmarks => "bookmarks",
        }
    }
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test return_to_is_a_closed_set`
Expected: PASS.

- [ ] **Step 5: Thread `return_to` through both handlers**

`edit_get_handler` gains a query extractor:

```rust
pub async fn edit_get_handler(
    State(state): State<AppState>,
    AxumPath((slug, hash_hex)): AxumPath<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    current_user: Option<CurrentUser>,
) -> (StatusCode, Html<String>) {
    let owner = current_user.map(|u| u.email);
    let return_to = ReturnTo::parse(query.get("return_to").map(String::as_str));
    let state2 = state.clone();
    let slug2 = slug.clone();
    let hash2 = hash_hex.clone();
    match run_blocking(move || edit_get_inner(&state2, &slug2, &hash2, owner.as_deref(), return_to))
        .await
    {
        Ok(html) => (StatusCode::OK, Html(html)),
        Err(e) => error_page(&slug, &hash_hex, &e.to_string()),
    }
}
```

`edit_get_inner` passes it to `render_edit_form`, which carries it in a hidden field and in the Cancel link:

```rust
                form action=(format!("/collection/{slug}/edit/{hash_hex}")) method="post" {
                    input type="hidden" name="mtime_ms" value=(mtime_ms);
                    input type="hidden" name="return_to" value=(return_to.as_str());
```
and
```rust
                        a.btn.btn-secondary href=(return_to.target(slug)) { "Cancel" }
```
and the back-link at the top of the page:
```rust
                a.back-link href=(return_to.target(slug)) {
                    @match return_to {
                        ReturnTo::Collection => { "\u{2190} " (collection_name) }
                        ReturnTo::Bookmarks => { "\u{2190} " (collection_name) " Bookmarks" }
                    }
                }
```

`EditForm` gains the field:

```rust
#[derive(Deserialize)]
pub struct EditForm {
    pub new_text: String,
    pub mtime_ms: String,
    #[serde(default)]
    pub return_to: Option<String>,
}
```

and `edit_post_handler` uses it for the redirect:

```rust
    let return_to = ReturnTo::parse(form.return_to.as_deref());
    …
            let target = return_to.target(&slug);
```

reading `form.return_to` before `form` is moved into `run_blocking`.

Add `use axum::extract::Query;` and `use std::collections::HashMap;` to `edit.rs`.

- [ ] **Step 6: Point the bookmarks page at `return_to=bookmarks`**

In `src/cmd/serve/bookmarks.rs:142`:

```rust
                    href=(format!("/collection/{slug}/edit/{hash_hex}?return_to=bookmarks"))
```

- [ ] **Step 7: Add the pencil to the drill screen**

In `src/cmd/drill/get.rs`, add the slug to the render context:

```rust
pub struct RenderContext<'a> {
    pub directory: &'a Path,
    pub total_cards: usize,
    pub session_started_at: Timestamp,
    pub answer_controls: AnswerControls,
    pub form_action: &'a str,
    pub file_url_prefix: &'a str,
    /// The slug this session is addressed by, for links out of the drill.
    pub slug: &'a str,
}
```

and set it in `handlers.rs`'s `collection_get_inner`:

```rust
    let ctx = RenderContext {
        directory: &session.directory,
        total_cards: session.total_cards,
        session_started_at: session.session_started_at,
        answer_controls: session.answer_controls,
        form_action: &form_action,
        file_url_prefix: &file_url_prefix,
        slug,
    };
```

Add the button next to `bookmark_button`:

```rust
/// The card editor, as a pencil beside the star.
///
/// A link, not a submit: the editor is its own page, and the drill screen
/// is one big form whose every button grades or navigates the session.
///
/// Shown only after the answer is revealed. Opening the editor before then
/// would put the answer in a textarea in front of a user who is still
/// trying to recall it, quietly corrupting the grade they are about to
/// give. Before reveal the star is the right affordance, and it is already
/// there.
fn edit_button(slug: &str, hash_hex: &str) -> Markup {
    html! {
        a.icon-button href=(format!("/collection/{slug}/edit/{hash_hex}?return_to=collection"))
            aria-label="Edit this card"
            title="Edit this card." {
            span aria-hidden="true" { "\u{270e}" }
        }
    }
}
```

and wrap the two controls in the header:

```rust
                div.header-actions {
                    @if mutable.reveal {
                        (edit_button(ctx.slug, &card.hash().to_hex()))
                    }
                    (bookmark_button(is_bookmarked))
                }
```

replacing the bare `(bookmark_button(is_bookmarked))` line.

- [ ] **Step 8: Correct the bookmark tooltip**

With editing one tap away, bookmarking means "save this for later" again:

```rust
            button #bookmark .icon-button type="submit" name="action" value="Bookmark"
                aria-label="Bookmark this card" aria-pressed="false"
                title="Save this card for later. Shortcut: b." {
```

- [ ] **Step 9: Make room in the header**

In `src/cmd/drill/style.css`, replace:

```css
.root .header {
  flex: none;
  display: grid;
  grid-template-columns: 2.5rem 1fr 2.5rem;
```
with `grid-template-columns: 2.5rem 1fr 5rem;`, and replace:
```css
.root .header #bookmark { justify-self: end; }
```
with:
```css
/* The star, and the pencil that joins it once the answer is revealed. The
   column is sized for both so the star does not shift when it appears. */
.root .header .header-actions { grid-column: 3; justify-self: end; display: flex; align-items: center; }
```

Add `text-decoration: none;` to the `.icon-button` block, since it is now used on an `<a>` as well as a `<button>`.

- [ ] **Step 10: Carry the real file path into the topic tree**

In `src/cmd/serve/browse.rs`, add to `BrowseData`:

```rust
    /// Topic name to the file its cards came from, relative to the
    /// collection folder — the target of the topic's edit link.
    ///
    /// Not derived from the topic *name*: that defaults to the file's path
    /// but a file's frontmatter `name:` overrides it, so the name is not a
    /// path. A topic whose cards come from more than one file gets no
    /// entry, since there is no one file to open.
    pub edit_paths: HashMap<String, PathBuf>,
```

and build it in `build_deck_tree` alongside `counts`:

```rust
    let mut counts: HashMap<String, DeckCounts> = HashMap::new();
    // `None` marks a topic seen in more than one file.
    let mut sources: HashMap<String, Option<PathBuf>> = HashMap::new();
    for card in &collection.cards {
        let entry = counts
            .entry(card.deck_name().clone())
            .or_insert(DeckCounts { total: 0, due: 0 });
        entry.total += 1;
        if due_hashes.contains(&card.hash()) {
            entry.due += 1;
        }
        let rel = card.relative_file_path(coll_dir).ok();
        match sources.entry(card.deck_name().clone()) {
            Entry::Occupied(mut slot) => {
                if slot.get().as_ref() != rel.as_ref() {
                    slot.insert(None);
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(rel);
            }
        }
    }
    let edit_paths: HashMap<String, PathBuf> = sources
        .into_iter()
        .filter_map(|(name, path)| path.map(|p| (name, p)))
        .collect();
```

with `use std::collections::hash_map::Entry;`, and add `edit_paths` to the returned `BrowseData`.

- [ ] **Step 11: Render the topic edit link**

`render_browse_page` builds the link prefix once and passes it down. Where it currently calls `render_deck_node(child, 0, hedge_urls)`, call `render_deck_node(child, 0, collection_name, &browse.edit_paths)`, and:

```rust
/// A leaf topic links to the file it lives in, opened in the in-app editor.
/// This replaces the outbound link to a HedgeDoc note: the target is a path
/// we construct, on every collection rather than only remote-backed ones,
/// and in the same tab.
fn render_deck_node(
    node: &DeckNode,
    depth: usize,
    collection_name: &str,
    edit_paths: &HashMap<String, PathBuf>,
) -> Markup {
    let total = node.total_cards_recursive();
    let due = node.due_today_recursive();
    let has_children = !node.children.is_empty();
    // A parent aggregates several files; there is no one file to open.
    let edit_url = if has_children {
        None
    } else {
        edit_paths.get(&node.path).map(|rel| {
            let full = format!("{collection_name}/{}", rel.display());
            format!("/files/edit/{}", encoded_path(&full))
        })
    };
```

and the link itself, replacing the old outbound one:

```rust
                @if let Some(url) = edit_url {
                    a.edit-link href=(url) { "Edit" }
                }
```

with the recursive call updated to `render_deck_node(child, depth + 1, collection_name, edit_paths)`.

Add `use crate::cmd::serve::href::encoded_path;`.

`rel.display()` on Windows would emit `\`; the file manager's paths are `/`-separated everywhere else in this codebase, so normalize:

```rust
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let full = format!("{collection_name}/{rel}");
```

- [ ] **Step 12: Test the topic edit link**

Add to `src/cmd/serve/browse.rs`'s `mod tests`:

```rust
    /// The link points at the file the topic's cards actually live in, not
    /// at its name: a file's frontmatter `name:` renames the topic without
    /// moving the file, and a link built from the name would 404.
    #[test]
    fn a_topic_links_to_the_file_its_cards_live_in() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        std::fs::create_dir_all(dir.join("grammar"))?;
        std::fs::write(
            dir.join("grammar").join("particles.md"),
            "---\nname: Little words\n---\n\nQ: wa\nA: topic marker\n",
        )?;
        let browse = build_deck_tree(&dir, &dir.join("test.db"))?;
        let html = render_browse_page("My Cards", "My-Cards", &browse, 0, 0, None).into_string();
        assert!(
            html.contains("/files/edit/My%20Cards/grammar/particles.md"),
            "got: {html}"
        );
        Ok(())
    }
```

- [ ] **Step 13: Record the feature in `CHANGELOG.xml`**

Add to the `<unreleased>` `<added>` block (create it above `<changed>` if absent):

```xml
        <added>
            <change author="claude">
            **A card can be edited while you are drilling it.** A pencil sits beside the bookmark star once the answer is revealed, and saving lands back on the card, re-rendered. It was refused before, and had to be: a live session keys card identity in four places — the queue, where a card requeued by Forgot or Hard appears twice; the undo stack, which holds the performance to restore; the performance cache; and the per-card database routes that send a custom deck's grades to the collection the card actually belongs to — and an edit renames the hash all four are keyed by. The session now follows the card, so the grade after an edit lands on the card in front of you and the old card's schedule comes with it. The pencil appears only after the answer is shown: opening the editor earlier would put the answer in a textarea in front of a user still trying to recall it. A card the edit deleted leaves the queue and the undo stack — undoing back to a card that is in no file would offer something you could not edit, re-drill or reach again — and an edit that empties the queue finishes the session rather than leaving a live one with nothing in it. Every topic in a collection now carries an Edit link to its file in the in-app editor, and the whole-file editor migrates a running session the same way. Deleting or renaming a collection is still refused while it is being drilled: a session cannot be re-keyed onto cards that are gone, and deleting a collection unlinks the database the session still holds open. That guard, and the two the editors used to apply, now find a session by asking what it loaded rather than by looking a slug up in the sessions map — which missed a running custom-deck session every time, since a deck is keyed by its own slug. The bookmark star's tooltip no longer says "for later editing": with editing one tap away, a bookmark means "save this for later" again.
            </change>
        </added>
```

- [ ] **Step 14: Verify and commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`

```bash
git add -A
git commit -m "feat: reach the card editor from the drill and the topic tree

A pencil beside the star, shown only after reveal, and an Edit link on
every leaf topic pointing at its file in the in-app editor -- a direct
replacement for the outbound HedgeDoc link the removal took away, on
every collection rather than only remote-backed ones.

return_to is a closed enum of two values rather than a caller-supplied
path, which would be an open redirect for no benefit.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RP3W1RufSmp3GWtDuxC763"
```

---

## Final verification

- [ ] **Whole suite green**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`

- [ ] **Nothing reaches the network but OIDC**

```bash
grep -rn "reqwest::" src/ | grep -v "^src/cmd/serve/auth.rs" | grep -v "#\[cfg(test)\]"
```
Expected: no hits outside `auth.rs` and test code. `reqwest` stays in `Cargo.toml` because OIDC needs it.

- [ ] **No dead references to the removed mechanisms**

```bash
grep -rniE "hedgedoc|\[\[source\]\]|commit_edit|clone_or_pull|LocalRoot|data_dir./repo" src/ README.md hashcards.example.toml
```
Expected: no hits in `src/`, `README.md` or `hashcards.example.toml`. `CHANGELOG.xml`'s released entries keep theirs — they are history.

- [ ] **The example config still loads**

```bash
sed 's|^data_dir = .*|data_dir = "/tmp/hashcards-check"|' hashcards.example.toml > /tmp/hashcards-check.toml
cargo run -- --config /tmp/hashcards-check.toml &
sleep 2 && curl -sf http://127.0.0.1:8000/ > /dev/null && echo OK
kill %1
```

- [ ] **Net effect matches the spec**

Report the real numbers rather than the spec's estimates: lines removed and added (`git diff --stat master...HEAD`), the test count, `AppState`'s field count, and the number of background tasks. State any figure that came out materially different from the spec's ("roughly −3,000 against perhaps +400", twelve fields to nine) and why.
