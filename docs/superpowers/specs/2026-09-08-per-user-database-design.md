# One Database Per User — Design

Status: draft 2026-09-08.

**Scope:** replace the review database per *collection* with one per *user*,
carrying `collection_id` as a column; merge existing per-collection databases
into it at startup, without deleting anything.

This is the first of two specs. It exists because of the second one: an MCP
server that lets a model split, merge and reorganise collections
(`2026-09-08-mcp-server-design.md`, to be written). Every one of those
operations moves cards between collections, and while a collection *is* a
database that means copying rows across two SQLite connections. After this
spec it is `update cards set collection_id = ?`.

## The current shape, and why it is the wrong one

A collection is a top-level folder in a user's markdown tree. It carries a
stable id in its `.hashcards.toml`, minted from the clock and the process id
so that renaming the folder cannot change it, and that id names its review
database:

```
data_dir/
  cards/<user-slug>-<hash>/       CardRoot: one markdown tree per user
    Biology/                      collection = top-level folder
      .hashcards.toml             carries the collection's id
      Cells.md                    deck (deck name = path without .md)
      Genetics/Mitosis.md         deck "Genetics/Mitosis"
      media/                      hashcards' own store
  db/<collection-id>.db           one SQLite per COLLECTION
```

`discover_local_collections` (`src/cmd/serve/cards.rs:406`) builds each
`ResolvedCollection` with `db_path: db_dir.join(format!("{id}.db"))`.

The grain is a fossil. Upstream `hashcards` was a CLI: a collection was the
directory you ran the tool in, and `hashcards.db` sat inside it.
`Collection::new` still has that shape and is `#[cfg(test)]` for exactly this
reason. Nothing about a multi-user server wants it:

- **`db/` is flat and shared across every user.** Which user owns a database
  is discoverable only by walking every tree and reading every
  `.hashcards.toml`. `discover_all_collections` says so itself: "`owner` is
  always `None` … a collection from here must never be routed to."
- **A folder deleted outside the application orphans its database forever.**
  Nothing collects it, and nothing can even attribute it.
- **The landing page opens one connection per collection** per request
  (`refresh_collection_info` → `compute_collection_counts`).
- **The startup sweep spawns a thread per database** to close sessions left
  open by a crash.
- **Splitting a collection is a cross-database row transfer.** This is the
  operation the MCP server exists to offer.

What speaks for the status quo is blast radius — one corrupt file costs one
collection — and that deleting a collection is a file deletion. Both are real
and both are given up here deliberately; the per-user grain keeps the blast
radius at one user rather than the whole server, which is the boundary that
actually matters.

## Decisions

Taken during brainstorming, recorded here so the plan does not relitigate
them:

1. **One database per user**, not one per server. A single server-wide file
   would serialise every user's writes against every other's, and the code
   currently opens SQLite in rollback-journal mode where a writer blocks
   readers file-wide.
2. **WAL**, so that the stats page opening a second connection while a drill
   session writes stops depending on the busy timeout to paper over it.
3. **Identical cards in two collections keep two schedules.** The primary key
   becomes `(collection_id, card_hash)`. This is exactly today's behaviour —
   two databases necessarily meant two schedules — so the merge has no
   conflict to resolve and cannot lose a row.
4. **Migration runs at startup, one transaction per user.** A user whose
   merge fails is refused, loudly; the other users are served.
5. **Source databases are never deleted**, only moved aside after a
   successful merge.

## Schema version 8

```sql
create table cards (
    collection_id text not null,
    card_hash     text not null,
    added_at text not null,
    last_reviewed_at text,
    stability real,
    difficulty real,
    interval_raw real,
    interval_days integer,
    due_date text,
    review_count integer not null,
    primary key (collection_id, card_hash)
) strict;

create table sessions (
    session_id integer primary key,
    collection_id text not null,
    started_at text not null,
    ended_at text not null,
    last_seen_at text,
    closed integer not null default 0
) strict;

create table reviews (
    review_id integer primary key,
    session_id integer not null
        references sessions (session_id)
        on update cascade
        on delete cascade,
    collection_id text not null,
    card_hash text not null,
    reviewed_at text not null,
    grade text not null,
    stability real not null,
    difficulty real not null,
    interval_raw real not null,
    interval_days integer not null,
    due_date text not null,
    duration_ms integer,
    voided integer not null default 0,
    reviewed_date text generated always as (substr(reviewed_at, 1, 10)) virtual,
    foreign key (collection_id, card_hash)
        references cards (collection_id, card_hash)
        on update cascade
        on delete cascade
) strict;

create table bookmarks (
    collection_id text not null,
    card_hash text not null,
    note text,
    created_at text not null,
    primary key (collection_id, card_hash),
    foreign key (collection_id, card_hash)
        references cards (collection_id, card_hash)
        on update cascade
        on delete cascade
) strict;

create index idx_reviews_card on reviews (collection_id, card_hash);
create index idx_reviews_session_id on reviews (session_id);
create index idx_reviews_reviewed_date on reviews (collection_id, reviewed_date);
create index idx_cards_due on cards (collection_id, due_date);
```

`schema_version` and `meta` are unchanged.

The composite foreign key keeps `on update cascade`, which is load-bearing:
`apply_edit_migration` renames a card's hash when its text is edited and
relies on the cascade to carry its reviews across. The same cascade will
carry them when `collection_id` changes, which is how a card moves between
collections in the MCP spec. SQLite requires the referenced columns to be a
primary key or carry a unique index; `(collection_id, card_hash)` is the
primary key of `cards`, so the constraint is satisfiable.

`sessions.collection_id` is new information: a session used to be identified
by which file it was written in.

## Types

`Database` today owns a `Connection` and is implicitly scoped to one
collection because the file is. It keeps both properties, and the scope
becomes explicit.

```rust
/// The stable id of a collection: the string in its `.hashcards.toml`.
pub struct CollectionId(String);

/// One user's review database.
pub struct UserDatabase {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

/// A view of one collection inside a user's database.
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    collection: CollectionId,
}
```

- `CollectionId` is a newtype for a value that currently travels as a bare
  `String` through `collection_id()`, `existing_collection_id()`, a file name
  and — from now on — every query. CLAUDE.md asks for newtypes on domain
  concepts, and this becomes one.
- `UserDatabase::open(path)` opens the file, applies the schema or the
  migration ladder, and sets WAL. `UserDatabase::collection(id) -> Database`
  hands out a scoped view **on the same connection**.
- Every `Database` method gains `and collection_id = ?` in its SQL and keeps
  its signature. The call sites in `drill/`, `stats_page.rs`, `browse.rs`,
  `export.rs` and `bookmarks.rs` do not change.
- `UserDatabase` additionally answers the two questions that are not about a
  single collection: per-collection counts for the landing page, and closing
  dangling sessions for the whole user at once.

**Why the connection is shared.** A saved deck spanning three collections
builds `SessionDbs::routed` with three `Database` handles
(`src/cmd/drill/state.rs:61`). After consolidation those three point at one
file, so three write connections would contend with each other on every
grade. Sharing one `Arc<Mutex<Connection>>` removes the contention and the
lock ordering question with it. A `parking_lot::MutexGuard` derefs to
`&mut Connection`, so the transaction methods can take `&self`;
`insert_review_and_update_performance` and `apply_edit_migration` lose their
`&mut`.

Borrowing (`Database<'a>`) would express the same thing more precisely and is
rejected: `DrillSession` holds its databases and lives in `AppState`, so the
lifetime would have to be threaded through a long-lived self-referential
structure.

## Migration

This does not fit the `migrate` ladder in `src/db.rs`. The ladder alters one
file in place; this merges N files into a new one. It is a separate startup
step in `start_serve`, running before the dangling-session sweep.

For each tree under `data_dir/cards/`:

1. Collect sources: for every collection folder in the tree, read its id
   (`ExistingOnly` — startup must not mint ids into a user's tree) and take
   `db/<id>.db` if it exists. With no sources there is nothing to do: leave
   the tree alone, and let the user's database be created on first use. This
   is the ordinary path for a user who signs up after the upgrade.
2. Open `db/<tree-name>.db`, creating it at schema 8 if absent. Drop any
   source whose id is already recorded there as `meta['merged:<id>']`.
3. Lift each remaining source with the **existing** ladder first: deployed
   databases sit at versions 1 through 7.
4. In one transaction on the target, for each source: copy `cards`,
   `sessions`, `reviews` and `bookmarks` with `collection_id` set to that
   source's id, and record `meta['merged:<id>']`. `sessions.session_id`
   collides between sources, so sessions are inserted with fresh ids and the
   reviews are rewritten through an old→new map held in Rust.
5. Commit, then move the source files — `.db`, `-wal`, `-shm` — into
   `db/legacy/`.

Idempotence rests on the `meta` marker written inside the transaction, not on
the move. A crash between commit and move leaves sources in place; the next
start skips them by marker rather than importing them twice.

A `db/<id>.db` matching no collection in any tree is left untouched and logged
once. It cannot be attributed to a user, so it cannot be merged into one.

### When a user's merge fails

The failure is recorded against that user's tree name in `AppState`. Their
collections then answer with an error page pointing at the startup log.
They must never be served an empty database: silently starting a user from
zero is the one outcome this design exists to prevent. Every other user is
served normally, and the server starts.

## Changes to existing code

| Location | Change |
|---|---|
| `serve/cards.rs:406` | `db_path` becomes the user's file; `ResolvedCollection` gains `collection_id` |
| `serve/files.rs:632` `db_path_for` | returns the user path and a `CollectionId` |
| `serve/files.rs:577` `remove_collection_database` | no longer deletes a file: `delete from cards where collection_id = ?` in a transaction, with `reviews` and `bookmarks` following by cascade and `sessions` deleted explicitly |
| `serve/server.rs:104` `sweep_dangling_sessions` | one call per user database instead of a thread per collection; the interrupted-session counts are keyed by `CollectionId` rather than by path |
| `serve/counts.rs` | one connection instead of N |
| `collection.rs:59` | `Collection::open(dir, db_path, collection_id)` |
| `db.rs` | every query scoped; `Database::new(":memory:")` becomes `Database::memory()` for tests |

The landing page gets one connection instead of N, and no more than that:
`refresh_collection_info` also re-parses every collection from disk on each
request, and that cost is unchanged. The database was never the expensive
half.

## Tests

Failing test first, per CLAUDE.md. The migration can cost data, so it carries
most of them.

**Migration**

- two collections merge, keeping review counts, due dates and bookmarks
- colliding `session_id`s are renumbered and each review stays attached to
  its own session
- a second run imports nothing and duplicates nothing
- a source at schema 1 is lifted to 7 and then merged
- a corrupt source fails only its own user; a second tree migrates normally
- sources are under `db/legacy/` afterwards, and still readable
- an orphan `db/<id>.db` is left untouched

**Schema**

- the same card hash in two collections keeps two independent schedules
- deleting one collection's cards leaves the other collection intact
- an edit that renames a card's hash cascades within its collection and does
  not touch an identical hash in a neighbouring one

**Integration**

- a server started on a `data_dir` seeded with old-shape databases serves the
  prior review history on the collection page

**Existing suite**

Roughly 25 `Database::new(":memory:")` call sites become `Database::memory()`
with a fixed test `CollectionId` — one line each.

## Risks

**The upgrade is one-way, and a downgrade fails quietly.** The ladder rejects
a version-8 file with a clear message, but an older binary never looks at that
file: it looks for `db/<id>.db`, finds nothing, and creates empty
per-collection databases. The session starts from zero with no error. Leaving
the sources in place instead would be worse — the old binary would write into
them, and the next upgrade would skip those reviews on the `merged:` marker
and lose them without a word. So: move them, and state the one-way character
plainly in `CHANGELOG.xml` and the README.

**WAL is not available everywhere.** `pragma journal_mode = wal` fails on some
NFS and SMB mounts. It returns the mode actually set, so: set it, check the
result, log a line on divergence, and continue in rollback-journal mode. An
optimisation must not refuse to start the server. (`remove_collection_database`
already cleans up `-wal` and `-shm`, so WAL was anticipated here.)

**Routing ambiguity survives.** `SessionDbs` routes by card hash, so a saved
deck spanning two collections that hold the same hash already routes
arbitrarily. This design neither causes nor worsens it — but having just
decided that two collections keep two schedules, it is a real bug. It is
noted as its own item, not patched in passing here.

**The `default` tree.** Without `[oidc]`, every request shares `default.db`,
the same property the shared `default` card tree already has.

## Out of scope

- The MCP server itself. Separate spec, built on this one.
- Card states, suspend/bury, lapse counting (ROADMAP §2). This spec adds no
  columns for them; it moves the ones that exist.
- Fixing `SessionDbs` hash routing.
