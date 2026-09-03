# Local Collections and the Markdown Editor — Design

Status: approved 2026-09-03.

**Scope:** a writable, hashcards-owned place to keep markdown, edited in the
browser; plus git file URLs as a third source kind. One spec, one plan.

## Goal

Today every card file arrives from somewhere else: a git clone that
`clone_or_pull` may hard-update, or a HedgeDoc note that sync overwrites.
Nothing in hashcards is writable by the person using it — the pre-fork
ability to just keep markdown on disk went away with the CLI, and under
OIDC a user has no filesystem access to put it back.

This restores plain-disk storage as a first-class source, managed entirely
through the web interface: a folder tree the user arranges, a markdown
editor with buttons that insert card skeletons, and a live preview whose
purpose is to prove the file *parses* before it is saved.

It also generalizes source handling so a link to a markdown file in a git
forge can be added exactly like a HedgeDoc note.

## Source kinds

There are three, and they divide on one line: whether hashcards owns the
bytes.

| Kind | Added by | Writable | Refreshed by |
|---|---|---|---|
| `hedgedoc` | pasting a note URL | no | the source sync task |
| `git` | pasting a file URL | no | the source sync task (same loop) |
| `local` | creating a folder | **yes** | never — the user is the source |

`hedgedoc` and `git` are read-only mirrors of a remote. `local` is the only
writable kind, and no sync task ever walks it.

### Git file sources

A git source is one markdown file fetched over HTTPS. The existing HedgeDoc
pipeline already does everything needed — `normalize_hedgedoc_url` →
`build_download_url` → `fetch_markdown` → write into the collection dir →
`slug_for_note` → persist. Only the second step is HedgeDoc-specific.

Replace it with `raw_url(kind, url)`:

| Pasted URL | Fetched from |
|---|---|
| `github.com/{o}/{r}/blob/{ref}/{path}` | `raw.githubusercontent.com/{o}/{r}/{ref}/{path}` |
| `gitlab.com/{o}/{r}/-/blob/{ref}/{path}` | same host, `/-/raw/{ref}/{path}` |
| Gitea/Forgejo `{host}/{o}/{r}/src/branch/{b}/{path}` | same host, `/raw/branch/{b}/{path}` |
| any URL whose path ends in `.md` | unchanged (already raw) |
| anything else | HedgeDoc: `/download`, current behaviour |

Kind is detected from the URL, never asked for. The `.md` suffix is the
discriminator for the generic case: HedgeDoc note URLs never end in `.md`.

The existing "server returned HTML, not markdown" guard applies to both
kinds with a kind-appropriate message — for git it means a private repo or
a bad ref, not a note permission level.

Everything downstream (`build_source`, `sync_source`, `commit_add`,
`commit_delete`, `collection_info_for_source`) sees only a URL and an owner
and is unchanged.

### Config

`HedgedocEntry` is already `{ url, owner }`, which is exactly what a git
source needs, so the persisted shape does not change. The array is renamed
to `[[source]]`; `[[hedgedoc]]` keeps parsing as a synonym so existing
config files work untouched, and new additions are written as `[[source]]`.
Kind is re-derived from the URL at load, never stored.

## Storage layout

A new root beside the existing ones:

```
{data_dir}/
  repo/            git clone      — clone_or_pull may hard-update this
  db/              review databases
  local/           NEW: user-owned markdown
    {user}/        one tree per user
      Spanish/
        verbs.md
      Medicine/
        Ch1.md
```

`{user}` is the slugified owner email when `[oidc]` is configured, and
`default` when it is not.

Keeping this outside `repo/` is the point: sync clobbering user writing
becomes structurally impossible rather than a rule to remember.

## Collections from the local tree

Each **top-level folder** under the user's root is a collection. Nested
folders and files inside it are decks, exactly as they are for a git
collection today. There is no `[[collection]]` entry — creating a folder is
creating a collection.

Discovery runs at startup and after any file-manager mutation, producing
`ResolvedCollection` values that join the configured and HedgeDoc ones.
Local slugs participate in the existing `check_slug_collisions` and
`find_slug_collision` checks, so a folder cannot shadow a configured
collection or a note.

### Stable ids

Review databases are named `db_dir/{slug}.db`, and a slug is derived from a
name. Renaming `Spanish/` to `Español/` would change the slug, orphan the
database, and silently discard every review.

So each top-level folder carries a `.hashcards.toml`:

```toml
id = "k3f9x2m8"
```

generated on creation — eight random alphanumeric characters, not
derived from the name, so it survives every rename. The database is `db_dir/{id}.db`; the slug stays
URL-only. Renaming a folder then costs nothing. A folder without the file
(hand-created on disk) gets one written on discovery.

`.hashcards.toml` is skipped by the parser and hidden in the file manager.

## File manager

`GET /files` renders the user's tree. Mutations, each `POST`:

| Route | Effect |
|---|---|
| `/files/folder` | create a folder |
| `/files/file` | create a `.md` file, seeded from the template |
| `/files/rename` | rename a file or folder |
| `/files/delete` | delete a file, or an empty folder |

Deleting a non-empty folder is refused with a message naming what it holds;
that keeps a whole collection and its review history from vanishing on a
misclick. Moving files between folders is **out of scope** — rename plus
create covers the need, and drag-and-drop is a plan of its own.

Every path is validated against the user's root before use, reusing the
traversal defenses in `src/media/load.rs`: relative only, no `..`
components, no root or drive prefix. Under OIDC a user reaches only their
own root; the check is by construction, not by comparing owners after the
fact.

## Editor

`GET /collection/{slug}/edit/{hash}` stays as it is — it edits one card
block. The new editor is document-level:

```
GET  /files/edit/{*path}     the editor
POST /files/edit/{*path}     save
POST /files/preview          parse + render an unsaved buffer
```

`{*path}` is a wildcard capture: card paths contain `/`, so a single
segment will not do.

A monospace textarea holds the raw markdown. Toolbar buttons insert a
skeleton at the cursor:

| Button | Inserts |
|---|---|
| Q/A | `Q: ` / `A: ` on two lines |
| Cloze | `C: ` with `[]` and the caret inside |
| Term | `T: ` / `D: ` |
| Separator | `---` |
| LaTeX | `$$` with the caret inside |
| Image | `![](path)` |

New files start from the template, which is also offered on the Sources
page for pasting into HedgeDoc or a git repo:

```markdown
---
name = "My deck"
---

Q: What is the capital of France?
A: Paris.

C: The mitochondria is the [powerhouse] of the cell.
```

Note the frontmatter is TOML (`name = "..."`), matching `parse_deck`.

### Live parse preview

The right-hand pane answers one question: *will hashcards read this?*

`POST /files/preview` takes the unsaved buffer plus the path being edited,
builds a `Parser` from that path (deck name from the filename or the TOML
frontmatter, and the line offset `strip_frontmatter_with_offset` reports so
error lines match the textarea), runs `Parser::parse_with_duplicates`, and
returns either the cards rendered by the drill renderer, or the
`ParserError` with its `line_num`. Duplicates
are reported too, since `ParsedFile` already carries them. Requests are
debounced on input.

This runs the production parser deliberately. A JavaScript approximation
could render a card that hashcards cannot actually read, which would make
the preview worse than none.

## Saving

Saving reuses `edit.rs` rather than reimplementing it:

- `write_atomic` for the write, `revert_file` if the reparse fails;
- `file_mtime_ms` for the same stale-buffer check the card editor makes;
- `plan_hash_migration` between the old and new parses, so rewording a card
  keeps its schedule instead of resetting it to new.

A save that does not parse is rejected with the error and line number, and
the file on disk is left alone.

## Testing

Written first, per CLAUDE.md.

- `raw_url` for each forge form, the `.md` fallthrough, and HedgeDoc URLs
  still detected as HedgeDoc.
- `[[hedgedoc]]` still parses after the rename to `[[source]]`.
- Every file-manager route rejects `..`, absolute paths, and paths outside
  the user's root.
- Preview returns a line-numbered error for malformed input and cards for
  valid input.
- A folder rename keeps its `id`, its database, and its review history.
- Local slugs collide-check against configured and HedgeDoc collections.
- A git and a HedgeDoc sync both leave `local/` untouched.
- A document-level save migrates card hashes for a reworded card.
- Deleting a non-empty folder is refused.

`CHANGELOG.xml` gets an entry.

## Out of scope

- Moving files between folders; drag-and-drop.
- Git-backing the local tree (commit/push user writing to a remote).
- Sharing a local collection between users.
- Rich-text or WYSIWYG editing — the file stays plain markdown.
- Conflict resolution beyond the existing mtime check.
- Media upload into the local tree; `![](...)` still references files that
  arrive by other means.
