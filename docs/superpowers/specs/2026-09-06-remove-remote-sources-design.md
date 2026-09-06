# Removing Remote Sources — Design

Status: draft 2026-09-06.

**Scope:** delete `[git]`, `[[source]]`/`[[hedgedoc]]` and `commit_edit`;
collapse the three collection kinds into one; and spend the simplification
on editing a card from inside a running drill session. One spec, one plan.

Nothing is deployed, so there is no compatibility surface: no config shims,
no deprecation window, no data migration.

## Goal

The 2026-09-03 spec introduced local collections as a third source kind
alongside HedgeDoc notes and git files, divided on one line: whether
hashcards owns the bytes. That line turned out to be the problem rather
than the design.

Everything the application wants to do next needs to write. Editing a card
rewrites its source file and renames its hash. On a local collection that
is a file write. On a HedgeDoc-backed one it is a lie: `sync_source`
overwrites the file on the next tick and the edit disappears with no
warning. On a git-backed one it is a dead end: `commit_edit` commits
locally, nothing pushes, and once the remote moves the branch has diverged
and `git pull --ff-only` fails on every subsequent tick — logged, and
otherwise silent.

So the read-only kinds do not merely fail to benefit from editing; they
make editing conditional on a property of the collection that nothing in
the UI surfaces. Removing them makes "a card can be edited" a flat fact
about the application.

The subtraction also pays for itself structurally. Three collection kinds
means three slug namespaces to keep from colliding, two database-naming
schemes, a three-branch `find_collection`, two background sync tasks, and
a file manager that can only see one kind of collection. All of that
collapses.

## What is being removed

Three separate mechanisms wear the "remote" label:

| Mechanism | What it is |
|---|---|
| `[git]` | one repo cloned to `{data_dir}/repo`, polled with `git pull --ff-only`. `[[collection]].path` resolves inside it. |
| `[[source]]` | one remote markdown *document* per collection, fetched over HTTPS. HedgeDoc note or forge raw-file URL, decided from the URL. |
| `commit_edit` | auto-commits web edits into whatever worktree holds the file. Never pushes. |

Nothing replaces the ability to paste a URL and get a collection. To bring
markdown in, create a file in the file manager and paste the text into the
editor, which previews through the production parser and validates media
before saving. After the cut, nothing in the application makes an outbound
request except OIDC.

## The collection model

**One kind.** A collection is a top-level folder in the caller's card tree,
discovered by reading the directory, identified by the stable id in its
`.hashcards.toml`. This is exactly what `discover_local_collections` does
today; it stops being one of three sources and becomes the only one.

### Data directory

```
{data_dir}/
  cards/{user}/{Collection}/…     ← was local/{user}/…
  db/{id}.db                      ← unchanged; now the only scheme
```

Only `repo/` goes. `db/` stays where it is, under the naming it already
uses, so no database file moves and no review history is at risk.

`local/` was named in opposition to `repo/` — its doc comment says so
outright. With `repo/` gone the name means nothing, and the UI already
calls this "My Cards". Rename the directory to `cards/`, `LocalRoot` to
`CardRoot`, and `local.rs` to `cards.rs`.

### Configuration

```toml
[server]
data_dir = "…"
# host, port, session_timeout_minutes

[defaults]      # optional
[oidc]          # optional
[[deck]]        # optional, still written back by the UI
```

There is no `[[collection]]`. `ResolvedServeConfig` loses `git`,
`collections` and `hedgedoc_entries`, keeping `data_dir`, `config_path`,
`custom_decks`, `oidc`, `defaults`, host, port and the session timeout.

### Ownership becomes structural

Today a collection's owner is a config key that must agree with `[oidc]`,
and three `fail()` branches in `from_toml` exist only to police the
agreement: an `owner` without `[oidc]` names a collection nobody can reach,
and `[oidc]` without an `owner` names one nobody owns.

After the cut, ownership *is* which tree the folder sits in —
`cards/{email-slug}/` with `[oidc]` configured, `cards/default/` without.
Those branches go, and that class of config error stops being expressible.
`[[deck]]` keeps its `owner`, because a deck genuinely is a config entry.

### One slug namespace

`check_slug_collisions` (configured against configured),
`find_slug_collision` (source against configured),
`check_startup_slug_collisions`, and the "a local folder shadowing a
configured collection is dropped with a warning" rule in `collections_for`
all disappear.

Two checks survive, both real:

- two folders in one tree that slugify alike — already handled inside
  `discover_local_collections`, which visits folders in name order so the
  winner does not depend on filesystem ordering;
- a custom deck whose slug collides with a collection's, since both address
  `/collection/{slug}`.

`find_collection` becomes a single call into card-tree discovery. It still
needs `spawn_blocking`, because it reads the filesystem, but it stops being
a three-branch search across two locks.

### What this unlocks for free

The file manager (`/files`) and the whole-file editor are already written
against `LocalRoot`. Today they simply cannot see configured or
source-backed collections. After the cut there is nothing else to see:
every collection becomes browsable, creatable, renamable and editable
without a line of new file-manager code.

## Deletion inventory

**Whole files (≈2,430 lines, 55 tests):**

| File | Lines | Why |
|---|---|---|
| `hedgedoc.rs` | 1566 | fetch, note naming, sync task, source config writeback |
| `git.rs` | 414 | `clone_or_pull`, `spawn_sync_task`, `commit_edit` |
| `source.rs` | 199 | forge URL rewriting, `SourceKind` |
| `hedgedoc_ui.rs` | 169 | the `/sources` page |
| `href.rs` | 84 | exists only to render external source URLs safely; both callers are source pages |

**`config.rs`** (1090 lines, roughly halves): `GitSection`, `ResolvedGit`,
`SourceEntry`, `CollectionEntry`, `source_entries()` and the `[[hedgedoc]]`
alias, `check_slug_collisions`, the collection-path validation block
(`is_absolute` / `has_root` / `..`), the three owner-agreement branches, and
the `default_branch` / `default_poll_interval` / `default_commit_author_*`
defaults. `from_directories` goes too: CLAUDE.md already flags it as
test-only scaffolding, and with no `[[collection]]` it has nothing to build.

**`state.rs`**: `HedgedocSource` and `HedgedocNote`. `AppState` drops
`hedgedoc_sources`, `last_synced` and `hedgedoc_last_synced` — twelve
fields to nine. `test_support::state_with_collections` disappears and
`state_with_data_dir` becomes the single constructor, which also collapses
the six hand-rolled config literals in `auth.rs`'s tests.

**`server.rs`**: six routes (`POST /sync`, `/sources`,
`/sources/add|delete|sync`, the `/hedgedoc` redirect), both background
spawns, the startup source-fetch block, and `check_startup_slug_collisions`.

**`handlers.rs`** (1765 lines, roughly −650): `sync_handler` and the four
`hedgedoc_*` handlers with their two form structs; the deck-name to
note-URL map at `handlers.rs:157`; `find_collection`'s third branch.

**Peripheral, one edit each**: `landing.rs` — `ServerStatus` loses
`git_enabled`, `last_synced`, `hedgedoc_count` and `hedgedoc_last_synced`,
leaving the config-file fact and the counts stamp. `browse.rs` — the
`hedge_urls` map threaded through `render_browse_page` and
`render_deck_node`. `decks.rs` — `all_collections_for` drops its
`configured` and `hedgedoc` chains. `files.rs` — `reserved_slugs` shrinks to
deck slugs, and the shadow-drop `retain` in `collections_for` goes.
`edit.rs` — the `commit_edit` call, the git-author resolution block,
`EditOutcome.committed`, and the "Committed to git." flash suffix.

**`Cargo.toml`**: tokio's `process` feature; `git.rs` is its only user.
`reqwest` stays, since OIDC needs it.

**Not removed**: `markdown.rs` keeps passing external image URLs through
(`markdown.rs:125`, `:253`). Its comments name HedgeDoc as the example, but
a card may legitimately reference a remote image. Only the comments change.

## Session-aware editing

### The invariant

A live session keys card identity in four places:

- `cards` — the queue. The **same card may appear twice**: a Forgot or Hard
  grade pushes it to the back while it is also in `reviews`.
- `reviews` — each holding a cloned `Card` plus `prev_performance`, which is
  what undo restores.
- `cache` — hash to performance.
- `dbs.routes` — hash to which collection's database, for custom decks.

An edit renames a hash. It must move all four together or none. That is why
`edit.rs:202` refuses outright today.

### `MutableState::apply_card_migration`

The migration goes where the invariant lives, in `drill/state.rs`, so it is
unit-testable without a server:

```rust
pub struct CardMigration {
    /// Old hash → the re-parsed card that replaces it.
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

impl MutableState {
    pub fn apply_card_migration(&mut self, m: &CardMigration) -> MigrationEffect;
}
```

It is **deliberately infallible**. It runs after the database transaction
has committed and the file is on disk, so there is nothing left to roll back
to. `Cache::insert` and `Cache::update` return errors on a missing or
duplicate key; the migration instead rebuilds `changes`, `routes` and the
two vectors wholesale, which cannot fail. That means adding a `Cache::rekey`
rather than calling the fallible pair in a loop.

`edit_post_inner` already computes `plan.renames: Vec<(CardHash, CardHash)>`
and holds the re-parsed cards, so building a `CardMigration` is a join, not
new analysis.

### Renames and removals

A **rename** is unambiguous: the card follows its new hash into all four
structures. Undo keeps working, because a `Review`'s `prev_performance`
travels with its card.

A **removal** needs a rule, and this is the design's one judgment call:

> A card the edit deleted leaves the queue, leaves the cache, and its
> reviews leave the undo stack.

Grades already written stay written — they happened, and the history stays
attached to the hash that existed at the time. But undoing back to a card
that is in no file would put the user in front of something they cannot
edit, re-drill, or reach again, and a further grade on it would land on a
hash orphaned from every file. `progress()` rewinds accordingly, which
honestly reflects a session that got smaller.

### Finding the sessions to migrate

```rust
fn sessions_touching(state: &AppState, coll_dir: &Path) -> Vec<SharedSession>
```

Answered from **the sessions themselves, not from deck configuration**: a
session touches a collection when any of its `SessionDb.source` carries that
`coll_dir`, or — for a single-collection session, where every `source` is
`None` — when `DrillSession.directory` is that directory.

This reflects what a session actually loaded rather than what the config
says now, and it closes a live bug at its root rather than by re-deriving
membership. Sessions are keyed by slug, and a custom deck has its own slug
(`decks.rs:49`), so today's `sessions.lock().contains_key(slug)` checks the
*collection* slug and misses a running custom-deck session that includes it
— at `edit.rs:202`, `files.rs:697` and `files.rs:829` alike.

### Ordering, and why the race is not one

File write, then database transaction, then session migration — with the
established map-then-session lock discipline: lock the map, clone the
touching `Arc`s, release, then lock each session. No SQLite transaction is
ever held under the sessions lock.

A session starting between the write and the migration looks like a race and
is not. A session parses its collection when it starts, so one beginning
after the write already holds the *new* hashes; the migration looks up old
hashes, finds none, and is a no-op. A session that began before the write is
in the map. Both are correct.

Two consequences need handling explicitly:

- **An edit can empty the queue.** If migration leaves `cards` empty while
  `finished_at` is `None`, finish the session there — otherwise the GET path
  renders a live session with nothing in it.
- **The browser holds a stale hash.** The rendered drill page carries the
  current card's hash in its form, and `handle_action` treats a mismatch as
  a duplicate submit ("That card was already graded"). After an edit that
  renamed the head card, that message would be both wrong and baffling. The
  edit POST must redirect to a fresh render, which `return_to` does anyway.

### What still refuses

Migration handles *edits*. It cannot handle a collection or file that
stopped existing: a session cannot be re-keyed onto cards that are gone, and
deleting a collection unlinks the database the session still holds open. So
the file manager's delete and rename guards stay, and get the
`sessions_touching` widening. The whole-file editor's save path, which
already reuses the card editor's hash migration, gets session migration for
free.

## Where editing is reachable from

**The drill screen: a pencil beside the star.** The card chrome already has
the pattern — `.icon-button` for actions, bookmark star in the top-right
(`get.rs:401`). The pencil submits `return_to=collection`, so a save lands
back on the card being drilled, re-rendered.

**Shown only after reveal.** Opening the editor before the answer is
revealed puts the answer in a textarea in front of the user, quietly
corrupting the grade they are about to give. Before reveal, the star is the
right affordance and already exists.

That also dates a caption: the star's tooltip reads "Bookmark this card for
later editing." With editing one tap away, bookmarking means "save this for
later" again, and the tooltip should say so.

**The browse page: an edit link per deck node.** A direct replacement for
something the removal takes away — `hedge_urls` currently links each deck
out to its HedgeDoc note (`browse.rs:296`). The replacement points at
`/files/edit/{path}`: the in-app editor, on every collection rather than
only remote-backed ones, in the same tab. This is also why `safe_href` is no
longer needed — the target is a path we construct.

**The bookmarks page**: the existing Edit link stays, with
`return_to=bookmarks`.

**`return_to` is a closed enum, not a URL.** Two values, `collection` and
`bookmarks`, mapping to `/collection/{slug}` and
`/collection/{slug}/bookmarks`. A caller-supplied path would be an open
redirect for no benefit; `/collection/{slug}` already renders the drill when
a session is live and the deck browser when it is not, which covers every
entry point.

**The warning banner goes.** `render_edit_form` currently shows "A drill
session is active. End it before saving to avoid stale state." That advice
becomes false. The post-save flash reports what actually happened instead:
cards migrated, cards that started fresh, and whether a running session was
updated. `EditOutcome` drops `committed` and gains the session effect.

## Verification

**Regression tests first**, per CLAUDE.md. Two bugs are fixed here and each
gets a failing test before its fix:

- a custom-deck session not caught by the collection-slug guard, at
  `edit.rs:202`, `files.rs:697` and `files.rs:829`;
- once migration exists, a grade landing on a renamed hash.

**Unit tests on `apply_card_migration`**, with no server:

- a rename reaching all four structures at once;
- a card present twice in the queue (graded Forgot, requeued) renamed in
  both places;
- undo across a rename restoring the right performance;
- a removal emptying the queue and finishing the session;
- a migration against a session that already holds the new hashes being a
  no-op.

**End-to-end in `mod.rs`**, which already spins up real servers: start a
drill, edit the current card, grade it, and assert the review landed on the
new hash with the old card's history intact.

**Deletion is verified by subtraction.** The 55 tests in the removed files
go with them. The number to watch is the rest: 477 tests today, and every
one not about HedgeDoc or git should still pass. `auth.rs`'s six hand-rolled
config literals and every `state_with_collections` caller get mechanically
rewritten onto the card-tree fixture — if that rewrite turns out to be
interesting, something is wrong.

**Documentation is part of done**: README's `[git]` and `[[source]]`
sections, `hashcards.example.toml`, and a `CHANGELOG.xml` entry marking this
as breaking.

## Net effect

Roughly −3,000 lines against perhaps +400 for the session work. `AppState`
goes from twelve fields to nine, `find_collection` from three branches to
one, collection kinds from three to one, background sync tasks from two to
zero, slug namespaces from three to one, and database-naming schemes from
two to one. Editing a card stops being conditional on where the card came
from.
