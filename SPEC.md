# Big Update Spec: Bugfixes and Feature Additions

Status: draft. Companion document: `ROADMAP.md` (UX improvements).

Scope: every known bug (including minor ones) plus small, high-value feature
additions. Each item has an ID for cross-referencing in PRs and the changelog.

## Global requirements

Apply to every item below (from `CLAUDE.md`):

- Every bugfix starts with a failing regression test.
- No `unwrap()` in production code; use `Fallible`, `?`, and `fail()`.
- All error messages are user-facing and clear.
- Reviews and performance are written in one transaction; undo voids, never deletes.
- Cloze positions are byte positions: `.bytes()`, never `.chars()`.
- Update `CHANGELOG.xml` per item.

Severity key: **S1** data loss / corruption / security. **S2** wrong behavior
users will hit. **S3** minor / polish / hygiene.

---

## Part 1: Bugs

### A. Data integrity (drill and serve session state)

**BUG-01 (S1) — Serve mode drops the session on any request error.**
`src/cmd/serve/handlers.rs:85` removes the session from the map; it is
re-inserted only at `:145` after fallible rendering (`?` at `:137-141`).
Same shape in `collection_post_inner` (`:386` remove, `:396` fallible
`handle_action`, `:398` re-insert). Any error permanently destroys the
running drill and leaves the DB session row open. A concurrent GET during
the window sees "no session" and renders the deck browser; starting a
drill from there discards the in-flight session (`:234`).
*Fix:* stop take-out/put-back. Hold the session in the map behind a
per-slug lock (e.g. `Mutex<DrillSession>` inside the map entry, or a
guard type that re-inserts on drop). Errors must leave the session intact.
*Test:* force a render/action error mid-session; assert the session
survives and the next GET renders the same card.

**BUG-02 (S1) — Drill mutates in-memory state before the DB write, no rollback.**
Grade path: card removed from queue at `src/cmd/drill/post.rs:180`, DB write
at `:200`. On DB failure the card silently vanishes, is never persisted, and
is not undoable. Undo path: review popped and card re-inserted (`:123-130`)
before `void_review_and_restore_performance` (`:132`); failure leaves cache
and DB diverged and can duplicate the card in the queue.
*Fix:* perform the DB transaction first; mutate queue/cache/undo stack only
after it commits. On error, state must be unchanged.
*Test:* inject a failing DB write; assert the card is still at the head of
the queue and no review row exists.

**BUG-03 (S2) — Errors in POST handlers are swallowed.**
`src/cmd/drill/post.rs:81-87` logs and redirects as if the action succeeded.
`src/cmd/serve/bookmarks.rs:189` and `:214` discard results with `let _ =`,
not even logging. Nine HedgeDoc failure branches and `sync_handler`
(`handlers.rs:495-499`, `:532`, `:551`, `:564`, `:571`, `:591`, `:599`,
`:648`, `:669`) redirect with no user-visible signal.
*Fix:* implement FEAT-01 (flash messages) and surface every one of these
paths through it. Remove all `let _ =` on fallible writes.

**BUG-04 (S2) — Undo after session finish does not reopen the session row.**
`finish_session` closes the row (`post.rs:229`); a subsequent Undo resets
`finished_at = None` (`:134`) but the DB row keeps its old `ended_at` while
new reviews continue to attach to it.
*Fix:* on undo of a finished session, reopen the row (null `ended_at`);
`finish_session` sets it again.

**BUG-05 (S3) — `End` on a not-yet-started session produces nonsense stats.**
`post.rs:140`: completion page reports 0 cards alongside a duration covering
the whole server uptime. *Fix:* when no card was revealed/graded, skip the
stats block (or report "no cards reviewed") and use the session row's real
start time.

**BUG-06 (S2) — Key auto-repeat / double-submit grades multiple cards.**
`script.js:42` does not check `event.repeat`; grade POSTs carry no
idempotency token, so a held key or double-tap grades several cards.
*Fix:* ignore `event.repeat`; include the current card hash in the grade
form and have the server ignore a grade whose hash does not match the queue
head. *Test:* POST the same grade twice with the same card hash; second is
a no-op.

**BUG-07 (S2) — Re-bookmarking clobbers the note.**
`insert_bookmark` uses `insert or replace` (`src/db.rs:409`) and the drill
action always passes `note: None` (`post.rs:159-160`); a resubmit erases the
user's note and resets `created_at`. The doc comment at `db.rs:400-401`
("row is created first") also contradicts the code.
*Fix:* `insert ... on conflict do nothing` for the drill path; a dedicated
`update_bookmark_note` for the notes form. Fix the doc comment.

**BUG-08 (S2) — Sessions keyed by slug only; no multi-client protection, no eviction.**
`src/cmd/serve/state.rs:18`: two browsers share one drill; abandoned
sessions live forever and their DB rows are never closed.
*Fix (minimal):* document single-user semantics; evict and close sessions
idle for a configurable timeout (default e.g. 24h), closing the DB row.

**BUG-09 (S3) — `finish_session` has a dead `_session_started_at` parameter.**
`post.rs:226`, leftover from the batch-flush era. *Fix:* remove it and its
call-site threading (`post.rs:141`, `:217`).

**BUG-10 (S3) — Drill exits non-zero on Ctrl+C with "Session interrupted before completion".**
`src/cmd/drill/server.rs:230`. Since reviews now persist immediately, an
interrupt loses nothing. *Fix:* close the session row, print a summary of
persisted reviews, exit 0.

**BUG-11 (S3) — `Shutdown` before session completion silently no-ops.**
`post.rs:144-150` returns `Continue` with no message. *Fix:* either honor it
(persistence makes this safe now) or show a flash explaining why not.

**BUG-12 (S3) — Duplicate card hashes abort drill startup.**
`cache.rs:45-52` errors on duplicate insert; `server.rs:164` propagates, so
two byte-identical cards prevent drilling entirely.
*Fix:* deduplicate at collection load with a warning naming both locations
(the `check` command should report duplicates too).

**BUG-13 (S3) — Completion stats are internally inconsistent.**
`cards_reviewed` comes from queue arithmetic (`get.rs:247`) while pace and
slowest-card come from `mutable.reviews` (`get.rs:253`, `:272`), which
includes repeats. "Slowest card" actually renders a deck name
(`get.rs:275` vs label at `:366`).
*Fix:* compute all stats from the session's non-voided DB reviews; label
distinct cards vs. total reviews separately; show the card question (or
truncated preview), not the deck, for slowest card.

**BUG-14 (S3) — `card_shown_at` is set on Reveal, not on display.**
`post.rs:117` vs. the field name and comment at `state.rs:49`. Pace stats
exclude recall time. *Fix:* set it when the card is first served
(in the GET path), reset on advance; rename if semantics stay reveal-based.

**BUG-15 (S3) — Grade POST without a prior reveal is silently ignored.**
`post.rs:172` has no else branch and no log. *Fix:* log at debug level and
redirect with a flash (stale-page case, see BUG-06 token).

### B. Parser and rendering

**BUG-16 (S1) — Empty cloze `[]` underflows usize.**
`src/parser.rs:574`: `end - 1` with `end == 0`. Debug: overflow panic.
Release: `usize::MAX` baked into positions, splices, hash, and JSON export.
*Fix:* guard `end == 0` (and empty deletions generally) with a parse error:
"Cloze deletion is empty. Location: file:line".
*Test:* `C: [] foo` yields that error; `C: [a]` still parses.

**BUG-17 (S2) — Markdown links inside cloze cards are parsed as deletions.**
Scanner (`parser.rs:481-534`, `:551-616`) special-cases only `![` and
escapes; `[text](url)` becomes a deletion with `text(url)` as clean text.
*Fix:* treat `](` lookahead as a markdown link (skip it), matching how
images are skipped. Document the escape rules in the README.
*Test:* `C: See [the docs](https://x) for [answer]` produces one deletion.

**BUG-18 (S2) — Card syntax inside fenced code blocks is parsed.**
`Line::read` (`parser.rs:218-230`) is a prefix match with no fence state;
`Q:`/`A:`/`C:`/`---` inside ``` blocks create cards or separators
(`:245-247`). *Fix:* track fence state (```` ``` ````/`~~~`) in the
line reader; lines inside fences are always `Text`.
*Test:* an answer containing a fenced block with `Q:` and `---` lines
round-trips as one card.

**BUG-19 (S2) — Nested/unbalanced cloze brackets fail silently.**
`[[a]]` drops a bracket without diagnostics (`parser.rs:561`); an unmatched
`[` yields the misleading "must contain at least one cloze deletion"
(`:619-623`). *Fix:* error on `[` while a deletion is open ("nested cloze
brackets") and on EOL/EOF with an open deletion ("unterminated cloze
deletion"), both with location.

**BUG-20 (S2) — Frontmatter offsets every line number.**
`parse_deck` strips frontmatter (`parser.rs:113`) before parsing
(`:133-134`); all error locations, `Card::range`, export line numbers, and
media-error locations are off by the frontmatter length.
*Fix:* give `Parser` a starting line offset computed from the stripped
prefix. *Test:* a parse error after `---` frontmatter reports the real
file line.

**BUG-21 (S3) — Frontmatter and I/O errors carry no file path.**
`parser.rs:65`, `:70` (no location); `parser.rs:110` I/O errors format as
debug spew via `error.rs:119-125`; several `From` impls use `{:#?}`
(`error.rs:122, 130, 138, 146, 170, 178`); `ErrorReport::from(ParserError)`
double-prefixes "error: Parse error:" (`error.rs:183-189`, `:201`).
*Fix:* attach the path to frontmatter/I/O errors; replace `{:#?}` with
human-readable messages; drop the redundant prefix.

**BUG-22 (S2) — `CLOZE_DELETION` sentinel replaced in card text.**
`src/types/card.rs:201`, `:225-228`: `text.replace(CLOZE_TAG, ...)` on
rendered HTML mangles any card containing the literal string.
*Fix:* use an unforgeable sentinel (e.g. random per-render marker) or
splice at the event/byte level before rendering.

**BUG-23 (S2) — Unescaped `title` in audio element (HTML injection).**
`src/markdown.rs:74-79` interpolates the raw markdown title into an HTML
attribute emitted as `Event::Html`. *Fix:* HTML-attribute-escape the title.
*Test:* a title containing `"` and `<` renders escaped.

**BUG-24 (S3) — User-supplied URLs rendered into `href` without scheme guard.**
`hedgedoc_ui.rs:71`, `browse.rs:240`; `handlers.rs:534-545` can store a raw
string when `Url::parse` fails. Currently mitigated, one refactor from a
`javascript:` href. *Fix:* validate scheme is http/https at storage time
(see BUG-38) and assert it at render time.

**BUG-25 (S3) — KaTeX failure leaves cards permanently invisible.**
`template.rs:79` sets `.card-content { opacity: 0 }`; `script.js:18` uses
`katex` unguarded (unlike `hljs` at `:33`), so a load failure aborts before
the opacity restore at `:36-39`. *Fix:* wrap in try/catch (or guard like
hljs) and always restore visibility; move the restore to a `finally`.

**BUG-26 (S3) — Cloze position docs say "character", code means bytes.**
`src/types/card.rs:56`, `:58`, leaking into JSON export consumers
(`export.rs:80-83`). *Fix:* correct the docs and export field docs; add a
non-ASCII cloze round-trip test (none exists today).

**BUG-27 (S3) — Cloze hash is offset-, endianness-, and pointer-width-dependent.**
`card.rs:166-167` hashes `usize::to_le_bytes()` of byte offsets: editing
text before a deletion re-hashes all sibling clozes (orphaning history);
a DB is not portable across architectures.
*Fix (minimal, non-breaking for now):* hash offsets as fixed-width `u64`
big-endian behind a schema/version note. Full offset-independence is a
breaking rehash — defer, but document it in IDEAS.md with a migration
sketch (use `family_hash`, `card.rs:176-186`, to re-link performance).

### C. Scheduling and dates

**BUG-28 (S2) — Clock rollback / timezone travel yields negative elapsed time → NaN retrievability.**
`performance.rs:715-726` can produce negative `t`; `fsrs.rs:324` then gives
`r > 1` or NaN. *Fix:* clamp elapsed days to `>= 0.0` at
`performance.rs:726`. *Test:* `last_reviewed_at` one day in the future
still yields a finite, clamped update.

**BUG-29 (S3) — `fail("invalid grade string: {value}")` missing `format!`.**
`src/fsrs.rs:300` prints the literal `{value}`. *Fix:* one-line format fix.

**BUG-30 (S3) — `interval_raw` documented as hours; it is days.**
`performance.rs:699-701` vs `:733-737`; exported verbatim
(`export.rs:92`). *Fix:* correct the docs.

### D. Database

**BUG-31 (S2) — `delete_card` hard-deletes reviews and is non-transactional.**
`src/db.rs:354-363`: bypasses the void model and can crash mid-way leaving
partial state. *Fix:* single transaction; rely on the FK cascade (one
`delete from cards` statement) — audit trail note in the changelog.

**BUG-32 (S3) — No schema version table; migrations are probe-based.**
`db.rs:70-80`, `:567-601`: three hand-rolled `pragma_table_info` probes; no
test that `schema.sql` and the migrated schema converge.
*Fix:* add `schema_version` table + integer version; convert existing
probes into numbered migrations; add a test that migrates an old-schema DB
and diffs the result against a fresh `schema.sql` DB.

**BUG-33 (S3) — No indexes on `reviews`; date filter defeats indexing.**
`count_reviews_in_date` (`db.rs:477-481`) uses `substr(reviewed_at,1,10)`.
*Fix:* index `reviews(card_hash)`, `reviews(session_id)`; store or generate
a date column and index it; rewrite the query as a range scan.

### E. Serve mode: sync, edit, HedgeDoc, config

**BUG-34 (S1) — In-browser edits silently break git sync.**
`edit.rs:265-297` writes into the git working tree; nothing commits;
`pull --ff-only` then fails forever, only logged (`git.rs:132-137`).
*Fix:* after a successful edit, `git add <file> && git commit` with a
standard message (author from config or "hashcards web edit"). Surface pull
failures via flash (FEAT-01). Optional push stays out of scope.
*Test:* edit a card in a git-backed collection; working tree is clean and
a subsequent pull succeeds.

**BUG-35 (S2) — Edit hash migration pairs cloze siblings positionally.**
`edit.rs:211-221` zips old/new hashes in document order: reorder or delete
one sibling and history transplants onto the wrong card; count mismatch
silently orphans rows (`:222-227`). *Fix:* match by cloze content/deletion
range where possible; migrate only unambiguous pairs; report skipped ones
via flash.

**BUG-36 (S2) — Edit is TOCTOU and the revert path ignores its own failure.**
mtime read at `edit.rs:158`, content re-read at `:178`, splice at `:180`;
`let _ = std::fs::write` at `:192` can leave a corrupted file while
reporting only a parse error. *Fix:* re-check mtime immediately before the
atomic rename; propagate revert failures with an explicit "file may be
inconsistent" message.

**BUG-37 (S2) — Empty deck selection drills the whole collection.**
`handlers.rs:267-276`: `deck_filter.is_empty()` falls back to all cards, so
a no-JS or hand-made POST with no `decks` starts everything. *Fix:* empty
selection is an error → redirect with flash "select at least one deck".

**BUG-38 (S2) — HedgeDoc URL validation deferred until fetch.**
An `http://` or garbage URL is accepted and persisted, then permanently
shows "Error" (`hedgedoc.rs:57-64`). *Fix:* run `validate_hedgedoc_url` in
`hedgedoc_add_handler` before persisting; reject with a flash.

**BUG-39 (S2) — HedgeDoc add/delete config races can lose entries.**
Snapshot at `handlers.rs:607-633`, persisted at `:664`, applied to memory
at `:674-683`; duplicate checks are separate lock acquisitions
(`:557-566`, `:607-623`). *Fix:* do check + mutate + persist under one
lock over the sources state; persist from the post-mutation state.

**BUG-40 (S2) — HedgeDoc startup errors permanently delete config entries.**
`server.rs:111-121` skips failing entries; config is later rebuilt from
memory (`hedgedoc.rs:467-472`), so the next add/delete writes them out of
existence. *Fix:* keep failing entries in memory with an Error status;
never drop a configured source without an explicit delete.

**BUG-41 (S3) — HedgeDoc note filenames can collide.**
`note_file_name` (`hedgedoc.rs:273-277`) maps all non-alphanumerics to `-`.
*Fix:* append a short hash of the exact note ID to the filename.

**BUG-42 (S3) — Deleting a HedgeDoc source orphans its data dir and DB.**
`handlers.rs:713-722`. *Fix:* delete the collection directory (the review
DB may be kept deliberately — decide; at minimum tell the user what
remains on disk).

**BUG-43 (S2) — Slug collisions route one collection to another's DB.**
`slugify` (`config.rs:120-130`) maps `a/b` and `a-b` identically;
`find_collection` (`handlers.rs:149-158`) returns the first match; hedgedoc
`hedgedoc-*` slugs can collide with configured collections.
*Fix:* detect collisions at config load / source add and fail with a clear
message naming both colliding names.

**BUG-44 (S3) — Collection parsing and SQLite run on the async runtime.**
`handlers.rs` GET/edit/sync/hedgedoc handlers and `git.rs:138` block the
executor. *Fix:* wrap collection loads and DB work in `spawn_blocking`
(pattern already present at `git.rs:153`).

**BUG-45 (S3) — Landing counts go stale.**
Recomputed only on sync / hedgedoc changes / `Home` action
(`handlers.rs:373-379`). *Fix:* refresh the affected collection's counts
when a session finishes or is ended, and on landing-page GET if older than
the poll interval.

### F. Security

**BUG-46 (S1) — Symlinked directories escape the media root.**
`MediaLoader::validate` checks only the final component
(`src/media/load.rs:66`); `linkdir -> /etc` + request `linkdir/passwd`
serves arbitrary files. The `@/` branch of `MediaResolver`
(`resolve.rs:123-140`) skips canonicalization entirely.
*Fix:* canonicalize the joined path and require
`starts_with(canonicalized_root)` in both places.
*Test:* symlinked dir inside a collection is rejected with a clear error.

**BUG-47 (S2) — Unauthenticated network exposure by default.**
Default bind `0.0.0.0` (`config.rs:42-44`) with no auth; the edit endpoint
is an arbitrary-file-rewrite for anyone on the LAN.
*Fix:* default bind `127.0.0.1`; require an explicit config opt-in
(`bind = "0.0.0.0"`) whose docs state there is no authentication.

**BUG-48 (S3) — Config `path` entries escape the data directory.**
`config.rs:217` joins without validation (`path = "/etc"` works);
`MediaLoader::new` asserts absolute paths (`load.rs:45-46`), panicking on
relative collection dirs. *Fix:* validate config paths resolve inside the
repo/data dir; replace the assert with a `Fallible` error.

### G. `unwrap()` / panic hygiene (project-rule violations)

**BUG-49 (S2) — Panics reachable from request handlers.**
- `post.rs:55` `panic!` in `Action::grade()` → return `Option<Grade>`.
- `get.rs:93` `mutable.cards[0]` unchecked (pub fn, also called from serve
  `handlers.rs:140`) → return an error page on empty queue.
- `get.rs:249` `finished_at.unwrap()` → `?` with error.
- `post.rs:123` `reviews.pop().unwrap()` → `if let Some(..)`.
- `media/load.rs:46` `assert!` → error (covered by BUG-48).
- `parser.rs:456` `unreachable!` → error return.
- `server.rs:152` / `handlers.rs:294-297` `duration_since(UNIX_EPOCH).unwrap()` → `?`.
- `server.rs:318` / serve `server.rs:301-303` `.expect` on Ctrl+C handler → `?`.

**BUG-50 (S2) — ~45 `.lock().unwrap()` calls across serve/drill.**
With `panic = "abort"` in release, any panic under a lock kills the whole
server; in debug, poisoning cascades. *Fix:* a small helper
(`fn lock_or_fail<T>(m: &Mutex<T>) -> Fallible<MutexGuard<T>>`) or switch
these to `parking_lot::Mutex` (no poisoning). Mechanical sweep; one PR.

**BUG-51 (S3) — Misc silent paths.**
- `stats.rs:408-410`: default `--format html` prints a stderr note and
  exits 0 → make `json` the default until FEAT-02 lands, or return an error.
- `collection.rs:288-294`: malformed `macros.tex` lines silently dropped →
  warn with line number.
- Drill error page returns `StatusCode::OK`, is unstyled, and has no way
  back (`get.rs:56-63`) → proper status code, `div.error` styling, a Home/
  session link.
- Serve `mod.rs:32-87` test asserts only a 302 that fires on success *and*
  failure → assert on post-redirect content.

---

## Part 2: Feature additions

**FEAT-01 — Flash messages (infrastructure; prerequisite for BUG-03/-11/-15/-34/-35/-37/-38).**
One-shot notice mechanism: set via redirect query param or short-lived
cookie; rendered as a dismissible banner in `page_template`. Success and
error variants. All silent failure paths in Part 1 route through it.

**FEAT-02 — Web stats page.**
`GET /collection/{slug}/stats` (serve) and `/stats` (drill): due forecast
(next 30 days), reviews per day (last 90 days), grade distribution,
retention estimate, per-deck due/total. Data already in the DB; needs
BUG-33's indexes. Replaces the broken `stats --format html` (BUG-51):
`hashcards stats` opens this page in the browser.

**FEAT-03 — Session resume.**
Since reviews persist immediately: on visiting a collection with an
unfinished session (from the sessions map or a dangling open DB session
row), offer "Resume session (N cards remaining)" instead of silently
starting over. Depends on BUG-08 eviction semantics.

**FEAT-04 — Auto-commit in-browser edits.**
The fix for BUG-34, promoted to a feature: every web edit becomes a git
commit, giving versioned card history for free. Changelog entry framed as
such.

**FEAT-05 — Interval jitter (from IDEAS.md).**
Add ±5% (configurable) random noise to computed intervals in
`update_performance`, using the existing `rng.rs`, to diffuse review peaks.
Deterministic under a seeded RNG for tests.

**FEAT-06 — Term-definition shorthand (from IDEAS.md).**
`T:`/`D:` line pair expands at parse time into the two reciprocal `Q:`/`A:`
cards ("Define: X" / "Term for: Y"). Hashes derive from the generated
cards so behavior matches hand-written equivalents.

**FEAT-07 — Duplicate card report in `check`.**
Companion to BUG-12: `hashcards check` lists duplicate-hash cards with both
file:line locations.

**FEAT-08 — Progress counter with repeat-aware semantics.**
Replace the bar-only indicator with "N of M (+k repeats)" text alongside
the bar; bar advances on first grade of each card, repeats tracked in the
`+k` counter. (UX rationale in `ROADMAP.md`; listed here because it
changes drill state accounting.)

---

## Suggested PR grouping

1. **Data integrity:** BUG-01, -02, -04, -06, -15 (+ tests).
2. **Flash messages + error surfacing:** FEAT-01, BUG-03, -05, -11, -37, -51.
3. **Parser correctness:** BUG-16 … BUG-21, BUG-26.
4. **Rendering safety:** BUG-22, -23, -24, -25.
5. **Security:** BUG-46, -47, -48.
6. **Panic hygiene:** BUG-49, -50.
7. **DB:** BUG-31, -32, -33.
8. **Edit + git:** BUG-34/FEAT-04, -35, -36.
9. **HedgeDoc hardening:** BUG-38 … BUG-42.
10. **Serve misc:** BUG-08, -43, -44, -45, FEAT-03.
11. **Scheduling:** BUG-28, -29, -30, FEAT-05.
12. **Stats:** FEAT-02, BUG-13, -14.
13. **Parser features:** FEAT-06, FEAT-07, BUG-12.
14. **Drill polish:** BUG-07, -09, -10, FEAT-08.
