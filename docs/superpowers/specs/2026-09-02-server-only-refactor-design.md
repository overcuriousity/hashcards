# Server-Only Refactor Design

Status: approved 2026-09-02.

## Goal

`hashcards` was forked from [eudoxia0/hashcards](https://github.com/eudoxia0/hashcards),
a local CLI spaced-repetition tool. The fork developed into a persistent,
multi-user web server: git-backed collections, HedgeDoc note sources,
cross-collection decks, in-browser editing, and OIDC login.

The CLI surface is now dead weight. This refactor removes it, renames the
project to `hashcards-web`, and preserves the two CLI capabilities that had
real value by moving them into the server.

Upstream authorship is preserved: the Apache-2.0 licence and every
`Copyright 2025 Fernando Borretti` header stay untouched, and the README
credits the original project.

## Decisions

Settled during brainstorming, 2026-09-02:

1. **Keep git-backed collections.** `[git]` cloning and `[[collection]]`
   markdown directories stay. Only the CLI and the two config-free entry
   paths are removed.
2. **Rename to `hashcards-web`,** package and docs only. On-disk
   `hashcards.toml` and `hashcards.db` keep their names so no deployment
   breaks.
3. **Keep `export` and duplicate reporting** as server features; drop
   `orphans` entirely.
4. **Delete in place.** Surviving modules stay at their current paths; the
   now-inaccurate `cmd/serve` and `cmd/drill` directory names are cosmetic
   and can be renamed in a separate mechanical commit.

## 1. CLI surface

Five subcommands collapse to a flat binary:

```
hashcards-web [--config <path>]     # defaults to ./hashcards.toml
```

Removed from `src/cli.rs`: the `Drill`, `Check`, `Stats`, `Orphans` and
`Export` variants, `OrphanCommand`, `--host`/`--port` (config-only now),
browser auto-open, and both config-free branches of `resolve_serve_config`
(positional directories, and the temp-dir HedgeDoc-only mode).

A missing config file is a hard error naming the expected path. Both removed
paths hardcoded `oidc: None`, so each was an unauthenticated bypass of
everything the fork added.

## 2. Deletions

| Path | Action |
|---|---|
| `src/cmd/check.rs` | delete; duplicate logic moves to the collection page (§4) |
| `src/cmd/orphans.rs` | delete outright |
| `src/cmd/stats.rs` | delete (CLI wrapper only; `cmd/stats_page.rs` is shared with serve and stays) |
| `src/cmd/export.rs` | CLI shell deleted; `get_export` and its serde structs move to `cmd/serve/export.rs` (§3) |
| `src/cmd/drill/server.rs` | **reduced, not deleted** — keeps `AnswerControls` and `escape_js_string_literal`, both used by serve; drops `start_server`, `ServerConfig`, `finalize_interrupted_session` |
| `src/cmd/drill/stats.rs` | delete; `cmd/serve/stats.rs` is the server equivalent |
| `src/cmd/drill/mod.rs` | drop the eight drill-CLI integration tests |
| `config.rs::from_directories` | delete |
| `utils::wait_for_server` | becomes `#[cfg(test)]`; only tests use it afterwards |
| `SPEC.md`, `ROADMAP.md`, `PLAN.md`, `docs/superpowers/plans/*` | delete — historical ledgers of landed work, citing paths this refactor removes |

`cmd/drill/{get,post,state,cache,template,katex,hljs}.rs`, `script.js` and
`style.css` stay: they are the shared drill engine and its assets, which
serve depends on directly. `IDEAS.md` stays (forward-looking, no stale paths).

**Consequent collapse.** A mandatory config file means `state.config_path`
is always `Some`, which makes `create_minimal_config`, `TempDirTracker` and
the `_temp_dir` field dead (`handlers.rs:1085-1105`). A whole branch of the
HedgeDoc add path disappears with them.

**Dropped dependency:** `open`. This moots PR #206 (bumps `open`) and PR
#183 (adds `drill --min-cards`).

## 3. Export as a route

`get_export` and its serde structs move to `src/cmd/serve/export.rs`, behind
a new owner-gated route:

```
GET /collection/{slug}/export
  -> application/json
     Content-Disposition: attachment; filename="{slug}-export.json"
```

Gated through the existing `find_collection(&state, &slug, owner)`, so a
non-owner gets the same 404 as on every other collection route.

Rationale: markdown lives in git, but the review databases live in
`data_dir` and are in nobody's git. Under OIDC a user has no filesystem
access, so this is their only route to their own review history.

## 4. Duplicates on the collection page

`Collection.duplicates` is already computed at load
(`src/collection.rs:35`) and discarded by serve; the drill CLI was the only
thing that reported it. Render it as a warning banner in
`collection_get_handler` (`handlers.rs:81`), one line per `DuplicateCard`
naming both `file:line` locations.

## 5. Rename

`Cargo.toml`: `name = "hashcards-web"`, reworded description,
`homepage`/`repository` pointed at `overcuriousity/hashcards-web`,
`categories` changed from `command-line-utilities` to `web-programming`.

The binary becomes `hashcards-web`, so these follow: `Makefile` (targets,
and `make example` currently runs `cargo run -- drill example`),
`install.sh` (`:50`, `:73`), `.github/workflows/release.yaml`
(`ARCHIVE_NAME`, `BINARY_NAME`, `:126-133`), `auto-release.yaml` (its `sed`
at `:46` matches `name = "hashcards"` literally), and the `main.rs:41` error
prefix.

Accepted breakage: anyone installed under the old binary name must
reinstall. `hashcards.toml` and `hashcards.db` are unaffected.

## 6. Documentation

README fully rewritten around the server: configuration, deployment, OIDC,
collections, HedgeDoc, decks. All CLI usage removed. Credits the upstream
project. `CLAUDE.md` updated to match the new layout. `CHANGELOG.xml` gains
the entry. `hashcards.example.toml` already documents everything and needs
no change.

## 7. Testing

The existing serve integration tests (`cmd/serve/mod.rs`, `cmd/serve/auth.rs`)
are the regression net and must stay green throughout.

New, written failing-first per `CLAUDE.md`:

- export route returns the owner's JSON; a non-owner gets 404
- the duplicate banner renders on a collection containing a duplicate

Removed: all drill-CLI integration tests, plus the `check`, `orphans`,
`export` and `stats` unit tests.
