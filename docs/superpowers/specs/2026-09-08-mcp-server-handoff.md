# MCP Server — Handoff

Status: **not a spec.** Notes handed off mid-design, 2026-09-08, so the next
session does not re-run the brainstorming. Everything below marked *decided*
was agreed with the user; everything marked *proposed* is my recommendation
awaiting their yes.

## Where things stand

The original request was one thing — an MCP server for hashcards-web with
full per-card CRUD, collection administration and card statistics. It split
into two projects when it turned out that a collection *is* a database, so
moving cards between collections meant a cross-database row transfer.

- **Project A — one review database per user.** Designed, spec written,
  committed as `91d7709`:
  `docs/superpowers/specs/2026-09-08-per-user-database-design.md`. The user
  has read it and said it looks good. **No implementation plan yet** —
  `superpowers:writing-plans` is the next step for A.
- **Project B — the MCP server.** Decisions taken (below), spec not written.

A comes first: after it, moving a card between collections is
`update cards set collection_id = ?`, and B never has to write a row transfer.

## Decided for B

1. **Streamable-HTTP MCP endpoint `/mcp` inside the existing axum process.**
   Not a separate stdio binary. The reason is correctness, not convenience: a
   drill session's card queue lives in the server's memory, so only an
   in-process writer can re-key a running session the way `edit.rs` does
   (`migrate_sessions`). A second process editing the same files would leave
   a live session holding dead hashes.
2. **Auth by token minted in the web UI** by a logged-in user, not written
   into the config by an administrator. Shown once, stored hashed.
3. **Writes cover content and structure; statistics are read-only.** Cards,
   decks, files, collections, scheduling overrides in `.hashcards.toml` and
   saved decks are writable. The schedule itself is not: no forget, no set
   due date, no suspend. Those belong to ROADMAP §2 (card states), and doing
   them here would pre-empt that decision.
4. **Deletion goes all the way up to a full collection, behind a trash.**
   Nothing the MCP deletes is destroyed.
5. **Cards carry their review history when they move between collections.**

## Proposed for B, not yet agreed

**Protocol implementation: hand-rolled, no `rmcp`.** POST `/mcp`, JSON-RPC
2.0, handling `initialize`, `notifications/initialized`, `ping`, `tools/list`
and `tools/call`, answering `application/json` — the Streamable HTTP spec
permits a plain JSON response when nothing streams, so no SSE and no session
id. That is a few hundred lines against a dependency that also pulls
`schemars`, in a project that vets every licence in `deny.toml` and vendors
KaTeX and highlight.js rather than linking them. Flag this one first at
review: it is the decision with the largest blast radius on the plan, and the
cheapest to reverse before work starts.

`/mcp` must sit outside the `require_auth` middleware — that layer redirects
to `/auth/login`, which is meaningless to an MCP client — and do its own
bearer check, exactly as the `/auth/*` routes are merged in
`server.rs:313`.

**Token storage: a server-level `data_dir/auth.db`.** A bearer token has to
resolve to a user *before* the user is known, so it cannot live in the
per-user database that project A introduces without scanning every one.
Table `tokens(token_hash, owner, name, created_at, last_used_at, write,
revoked)`, keyed by a blake3 digest of the secret. `getrandom` is already in
the tree transitively (via `openidconnect`) and would become a direct
dependency for minting; `blake3` already is.

Without `[oidc]` there is no logged-in user, so a token there names the
shared `default` tree — the same property that tree already has.

**Trash: `data_dir/trash/<tree-name>/<timestamp>-<slug>/`** holding a
`manifest.toml` (kind, collection id, original relative path, deleted_at) and
the removed bytes.

The good trick here: **do not delete the database rows.** After project A the
rows live in the user's database keyed by `(collection_id, card_hash)`, and
card hashes are content addresses, so leaving the rows behind as orphans —
which every read path already ignores, see the note in `stats_page.rs` about
orphan rows being excluded from the forecast — makes restoring a trashed
folder restore its whole review history for free, with no row dump to write
or replay. Orphans accumulate; emptying the trash in the web UI is what
collects them.

**The MCP gets no purge tool.** `list_trash` and `restore_from_trash` only.
Emptying the trash is a human action in the web UI, which makes "the model
cannot destroy anything irrecoverably" a property of the design rather than a
hope.

**Tool surface, ~23 tools.** Explicit tools with good descriptions, rather
than a few overloaded ones.

- Read: `list_collections`, `get_collection`, `read_deck`, `list_cards`
  (filters: deck, due-only, has-history, text query; paginated with a cursor),
  `get_card` (source block, plain-text front/back, stats, review history),
  `get_collection_stats` (the existing `gather_stats`), `get_user_stats`
  (cheap after A).
- Cards: `create_card`, `update_card`, `delete_card`.
- Decks and files: `create_deck`, `write_deck` (whole-file replace — the code
  path `editor_post_handler` already runs), `move_decks`, `delete_deck`.
- Collections: `create_collection`, `rename_collection`, `delete_collection`,
  `set_collection_scheduling`.
- Saved decks: `list_saved_decks`, `set_saved_deck`, `delete_saved_deck`
  (these rewrite `hashcards.toml` through `persist_custom_decks`).
- Trash: `list_trash`, `restore_from_trash`.

Deliberately absent: `split_collection` and `merge_collections`. Splitting is
`create_collection` plus `move_decks`, merging is `move_decks` plus
`delete_collection`; two conveniences that can only add ways to be wrong.
Also absent: `search_cards`, folded into `list_cards` as a `query` parameter.

**Concurrency comes free from content addressing.** `update_card` takes a
hash; if the hash no longer resolves, the card has changed or moved and the
call fails with a clear message. No mtime token has to be handed to the
model — the handler reads the mtime itself and passes it into
`splice_card_block`, whose re-check just before the rename closes the
remaining window. All tool handlers go through `run_blocking`, like every
other path that touches SQLite or the tree.

**Teaching the model the domain.** The `initialize` result's `instructions`
field carries the taxonomy (user → collection → deck → card), the fact that
a card hash is a content address so editing a card changes its hash, and the
card syntax: `Q:`/`A:`, `C:` with `[cloze]` deletions, `---` separators, TOML
frontmatter `name` overriding the deck name. Tool descriptions repeat the
syntax where a tool takes card text.

## Open questions nobody has answered yet

- Whether MCP *resources* (`hashcards://collection/{slug}/deck/{name}`) are
  worth it, or whether tools cover everything. My inclination: out of scope
  for v1.
- Whether `[mcp] enabled` defaults to on or off. Leaning on: a token is
  required anyway, the minting page is behind auth, and defaulting off means
  a freshly minted token silently does not work.
- Read-only tokens: proposed as a `write` flag per token, not discussed.
- Request body limit for `write_deck` (axum's default is 2 MB; `/files/media`
  already sets its own).

## Risks worth writing into the spec

- **Card text is untrusted input read by a model.** A collection can contain
  anything, including text shaped like instructions. Worth stating plainly in
  the spec rather than discovering later.
- **A write token can restructure a lot very quickly.** The trash, and the
  absence of a purge tool, are the whole mitigation; there is no rate limit.
- **A hand-rolled protocol implementation drifts.** Pin the protocol revision
  string and name it in the spec.

## Also noticed, unrelated but real

- `CLAUDE.md` still lists "git, HedgeDoc" as parts of `src/cmd/serve/`.
  Neither exists any more — `grep` finds nothing. Stale since the
  remove-remote-sources work.
- `SessionDbs` routes cards to databases by card hash
  (`src/cmd/drill/state.rs:61`). A saved deck spanning two collections that
  hold the same card hash routes arbitrarily. Project A neither causes nor
  worsens this, but it contradicts A's decision that two collections keep two
  schedules. Noted in A's spec as its own item.
