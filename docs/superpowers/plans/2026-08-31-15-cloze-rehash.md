# Cloze Hash Rehash (BUG-27, full offset-independent scheme) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **DEPENDENCY — READ FIRST:** This plan **depends on plan `2026-08-31-08-database.md` (BUG-31/32/33) being merged first.** It extends that plan's `schema_version` mechanism with **migration number 6** and reuses its `SCHEMA_VERSION` constant, `migrate` match, `OLD_SCHEMA` test fixture, and version-rejection guard. Do not start this plan on a tree where `src/db.rs` still uses the old probe-based migrations (`migrate_add_duration_ms` called directly from `Database::new`).

**Goal:** Make cloze card hashes derive only from platform-independent, offset-independent data (card text + deleted substring + occurrence index), and re-link existing databases' review history, performance, and bookmarks to the new hashes in a one-time, single-transaction, load-time migration.

**Architecture:** `CardContent::hash()` for cloze cards stops hashing `usize::to_le_bytes()` byte offsets and instead hashes the card's text, the deletion's *content* (the deleted substring), and an occurrence index (how many times that substring occurs earlier in the text), all serialized platform-independently. A `CardContent::legacy_hash()` reproduces the old algorithm so that, at collection load, each parsed card's legacy hash can be mapped to its new hash and every referencing DB row rewritten via `on update cascade` in ONE transaction. A `meta` table row `cloze_hash_scheme` (seeded `'1'` for existing DBs by SQL migration 6, `'2'` for fresh DBs by `schema.sql`) makes "already migrated" detection a single SELECT.

**Tech Stack:** Rust (edition 2024), rusqlite (bundled SQLite, `on update cascade` FKs), blake3 via the existing `Hasher` newtype wrapper, `tempfile` for on-disk tests.

**Spec:** SPEC.md (item **BUG-27**; the spec's "minimal, non-breaking" option is explicitly superseded — the project owner has decided to do the full offset-independent rehash now, with DB migration).

## Global Constraints

Copied verbatim from SPEC.md "Global requirements":

- Every bugfix starts with a failing regression test.
- No `unwrap()` in production code; use `Fallible`, `?`, and `fail()`.
- All error messages are user-facing and clear.
- Reviews and performance are written in one transaction; undo voids, never deletes.
- Cloze positions are byte positions: `.bytes()`, never `.chars()`.
- Update `CHANGELOG.xml` per item.

Additional repo rules from `CLAUDE.md` that apply here:

- `unwrap()` is fine in tests, never in production code.
- Prefer imports (`use`) over fully qualified names; newtypes for domain concepts; keep functions small.
- End commit messages with the `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` trailer your harness prescribes.

Run all tests with `cargo test` from the repo root.

## Design decisions (context for every task)

**New cloze hash input** (all platform-independent, no byte offsets, no `usize::to_le_bytes()`):

```
blake3( "ClozeV2" ++ text ++ 0xFF ++ deletion ++ 0xFF ++ decimal(occurrence_index) )
```

- `text` is the card's clean text (cloze text without brackets — the existing `CardContent::Cloze::text` field). The deck name is deliberately NOT included, matching the current scheme: including it would orphan history whenever a file moves between decks.
- `deletion` is the deleted substring: the bytes `text[start..=end]`. **Byte** positions stay byte positions everywhere — they are removed from the *hash input* only, not from the card model, the parser, the renderer, or the JSON export.
- `occurrence_index` is the number of occurrences of `deletion` beginning before byte position `start` in `text`, serialized as decimal ASCII (`"0"`, `"1"`, …). Sibling deletions are disjoint, so an earlier identical deletion always counts as an occurrence — two identical deletions in the same card therefore always get distinct indices, hence distinct hashes. Two *different* deletions differ in content, hence distinct hashes. Two hand-written byte-identical cards produce identical `(text, deletion, occurrence)` triples, so they keep colliding exactly as today.
- `0xFF` never occurs in valid UTF-8, so it is an unambiguous field separator (no length prefixes needed, so no integer serialization at all).
- Basic cards are untouched: their hash never contained offsets.
- `family_hash` (`blake3("Cloze" ++ text)`) is untouched.

**Migration is load-time, not SQL-time.** New hashes are computable only from parsed markdown, so `Database::new` cannot do the rehash. Instead: SQL migration 6 (the next number after plan 08's 1–5) creates a `meta` key/value table and seeds `cloze_hash_scheme = '1'` on existing DBs; fresh DBs get `'2'` straight from `schema.sql`. `Collection::with_db_path`, after parsing the deck, reads the scheme: `'2'` → return immediately (normal startups pay one SELECT); `'1'` → compute `(legacy_hash, new_hash)` for every parsed cloze card and rewrite them in ONE transaction that also flips the scheme to `'2'`. A half-migrated DB is impossible (single transaction), and an old or foreign DB state is detected, never silently mixed: a scheme value other than `'1'`/`'2'` is a hard error, and plan 08's version guard makes post-plan-08 binaries older than this feature reject the schema_version-6 DB outright.

**Why recomputing the legacy hash implements "family_hash + deletion identity" matching:** the legacy hash of a card is `blake3("Cloze" ++ text ++ start_le ++ end_le)` — a function of exactly (family text, deletion position), and deletion position within a text is equivalent to (deletion content, occurrence index) — for a fixed text and deletion content, each valid deletion start has a distinct occurrence count (every earlier identical deletion lies fully before it). So mapping each parsed card's legacy hash to its new hash IS "find the new card with the same family and the same deletion identity", with zero ambiguity, and it also correctly handles the degenerate cross-family case (different texts never share a legacy hash short of a blake3 collision). Old DB rows whose legacy hash matches no parsed card (deleted cards, or DBs written on a different endianness/pointer width — unrecoverable by definition, since their stored hashes were computed with that platform's bytes) are left untouched; they are exactly what `hashcards orphans` (`src/cmd/orphans.rs`, `get_orphans` = DB hashes minus collection hashes) already reports today.

**Why the `meta` marker instead of overloading `schema_version`:** `schema_version` (plan 08) describes the *SQL schema shape* and is applied at every connection open inside `Database::new`, where no parsed cards exist. The rehash is a *data* migration that runs at collection-load time. Keeping them separate preserves plan 08's invariants ("a fresh schema.sql DB and a fully migrated DB are structurally identical" — the convergence test compares structure, not rows, so the differing seed value is fine) while still slotting into its numbering: migration 6 creates the marker, so plan 08's "newer schema version is rejected" guard automatically protects migrated DBs from older (post-plan-08) binaries.

**File structure:**

| File | Responsibility |
|---|---|
| `src/types/card.rs` | New `hash()` input, `occurrence_index` helper, `legacy_hash()` |
| `src/schema.sql` | `meta` table + `cloze_hash_scheme = '2'` seed for fresh DBs |
| `src/db.rs` | Migration 6 (`meta` table, seed `'1'`), `cloze_hash_scheme()`, `migrate_cloze_hashes()` |
| `src/collection.rs` | Load-time `upgrade_cloze_hashes` hook + end-to-end migration test |
| `src/cmd/export.rs` | Pin-down test: export carries new-scheme hashes |
| `README.md`, `CHANGELOG.xml` | Document the scheme and the breaking change |

---

### Task 1: Content-based cloze hash and `legacy_hash` in `src/types/card.rs`

**Files:**
- Modify: `src/types/card.rs:155-171` (`CardContent::hash`), plus new free function `occurrence_index` and new methods `CardContent::legacy_hash` / `Card::legacy_hash`
- Test: `src/types/card.rs` (tests module at the bottom of the same file)

**Interfaces:**
- Consumes: existing `Hasher` (`src/types/card_hash.rs:96-116` — `new()`, `update(&[u8])`, `finalize() -> CardHash`), `CardHash::hash_bytes(&[u8]) -> CardHash` (`#[cfg(test)]`), `CardContent::new_cloze(text, start, end)`.
- Produces (Tasks 2–4 depend on these exact signatures):
  - `CardContent::hash(&self) -> CardHash` — same signature, new algorithm for the `Cloze` variant only (`Basic` unchanged, `family_hash` unchanged).
  - `pub fn legacy_hash(&self) -> Option<CardHash>` on `CardContent` — `Some` for cloze (old algorithm), `None` for basic.
  - `pub fn legacy_hash(&self) -> Option<CardHash>` on `Card` — delegates to its content.
  - `fn occurrence_index(bytes: &[u8], start: usize, deletion: &[u8]) -> usize` (private free function in `card.rs`).

- [x] **Step 1: Write the failing regression tests**

Add to the `tests` module at the bottom of `src/types/card.rs` (after `test_family_hash`). Byte-position sanity for the fixtures: in `"The capital of France is Paris"`, `France` occupies bytes 15–20 and `Paris` bytes 25–29; in `"je bois du café"`, `café` occupies bytes 11–15 (`é` is two bytes — positions are BYTE positions per CLAUDE.md).

```rust
    /// BUG-27 regression: the cloze hash is a platform-independent function
    /// of (text, deletion content, occurrence index) — verified against the
    /// reference formula, which contains no offsets, no usize serialization,
    /// no endianness.
    #[test]
    fn test_cloze_hash_is_content_based() {
        let content = CardContent::new_cloze("The capital of France is Paris", 25, 29);
        let mut hasher = Hasher::new();
        hasher.update(b"ClozeV2");
        hasher.update(b"The capital of France is Paris");
        hasher.update(&[0xFF]);
        hasher.update(b"Paris");
        hasher.update(&[0xFF]);
        hasher.update(b"0");
        assert_eq!(content.hash(), hasher.finalize());
    }

    /// Same property for multi-byte (non-ASCII) deletions: the deletion is
    /// the byte slice text[start..=end].
    #[test]
    fn test_cloze_hash_is_content_based_non_ascii() {
        let content = CardContent::new_cloze("je bois du café", 11, 15);
        let mut hasher = Hasher::new();
        hasher.update(b"ClozeV2");
        hasher.update("je bois du café".as_bytes());
        hasher.update(&[0xFF]);
        hasher.update("café".as_bytes());
        hasher.update(&[0xFF]);
        hasher.update(b"0");
        assert_eq!(content.hash(), hasher.finalize());
    }

    /// BUG-27 regression: the old offset-based algorithm is no longer the
    /// hash, but is still reproducible via legacy_hash() for DB migration.
    #[test]
    fn test_cloze_hash_no_longer_uses_offsets() {
        let content = CardContent::new_cloze("The capital of France is Paris", 25, 29);
        let mut legacy: Vec<u8> = Vec::new();
        legacy.extend_from_slice(b"Cloze");
        legacy.extend_from_slice(b"The capital of France is Paris");
        legacy.extend_from_slice(&25usize.to_le_bytes());
        legacy.extend_from_slice(&29usize.to_le_bytes());
        let legacy = CardHash::hash_bytes(&legacy);
        assert_ne!(content.hash(), legacy);
        assert_eq!(content.legacy_hash(), Some(legacy));
    }

    /// Basic cards have no legacy hash: their scheme never changed.
    #[test]
    fn test_basic_card_has_no_legacy_hash() {
        let content = CardContent::new_basic("Q", "A");
        assert_eq!(content.legacy_hash(), None);
        // And the basic hash itself is unchanged from the old scheme.
        let mut hasher = Hasher::new();
        hasher.update(b"Basic");
        hasher.update(b"Q");
        hasher.update(b"A");
        assert_eq!(content.hash(), hasher.finalize());
    }

    /// Two identical deletions in the same card ("C: [a] and [a]") must
    /// still hash differently — the occurrence index disambiguates them.
    #[test]
    fn test_repeated_identical_deletions_hash_differently() {
        // Clean text "a and a": deletions at bytes (0,0) and (6,6).
        let first = CardContent::new_cloze("a and a", 0, 0);
        let second = CardContent::new_cloze("a and a", 6, 6);
        assert_ne!(first.hash(), second.hash());
        assert_eq!(first.family_hash(), second.family_hash());
    }

    /// Two different deletions in the same card hash differently.
    #[test]
    fn test_distinct_deletions_hash_differently() {
        let a = CardContent::new_cloze("The capital of France is Paris", 15, 20);
        let b = CardContent::new_cloze("The capital of France is Paris", 25, 29);
        assert_ne!(a.hash(), b.hash());
    }

    /// Hand-written duplicate cards keep colliding, exactly as today.
    #[test]
    fn test_duplicate_cards_still_collide() {
        let a = CardContent::new_cloze("The capital of France is Paris", 25, 29);
        let b = CardContent::new_cloze("The capital of France is Paris", 25, 29);
        assert_eq!(a.hash(), b.hash());
    }
```

- [x] **Step 2: Run the tests and see them fail**

Run: `cargo test types::card`
Expected: COMPILE ERROR — `legacy_hash` does not exist yet. Comment out the two `legacy_hash` assertions temporarily if you want to see the value failures directly: `test_cloze_hash_is_content_based`, `test_cloze_hash_is_content_based_non_ascii`, and `test_cloze_hash_no_longer_uses_offsets` FAIL against the current offset-based algorithm; `test_repeated_identical_deletions_hash_differently`, `test_distinct_deletions_hash_differently`, and `test_duplicate_cards_still_collide` already pass (pin-down tests). Restore the assertions before Step 3.

- [x] **Step 3: Implement the new hash, `occurrence_index`, and `legacy_hash`**

3a. Replace `CardContent::hash` (currently `src/types/card.rs:155-171`) with:

```rust
    pub fn hash(&self) -> CardHash {
        let mut hasher = Hasher::new();
        match &self {
            CardContent::Basic { question, answer } => {
                hasher.update(b"Basic");
                hasher.update(question.as_bytes());
                hasher.update(answer.as_bytes());
            }
            CardContent::Cloze { text, start, end } => {
                let bytes = text.as_bytes();
                // Positions are byte positions (see CLAUDE.md). A malformed
                // range hashes as an empty deletion instead of panicking.
                let deletion: &[u8] = bytes.get(*start..=*end).unwrap_or(&[]);
                let occurrence = occurrence_index(bytes, *start, deletion);
                // The hash input is fully platform-independent: 0xFF never
                // occurs in UTF-8, so it is an unambiguous field separator,
                // and the occurrence index is serialized as decimal ASCII —
                // no fixed-width integers, no endianness, no pointer width.
                hasher.update(b"ClozeV2");
                hasher.update(bytes);
                hasher.update(&[0xFF]);
                hasher.update(deletion);
                hasher.update(&[0xFF]);
                hasher.update(occurrence.to_string().as_bytes());
            }
        }
        hasher.finalize()
    }

    /// The hash this content had under the legacy (pre-v2) cloze scheme,
    /// which mixed the deletion's byte offsets into the hash as
    /// platform-dependent `usize::to_le_bytes()`. Used only to re-link rows
    /// in databases written by older versions of hashcards — the
    /// `to_le_bytes` here intentionally reproduces what this machine wrote.
    /// `None` for basic cards, whose scheme never changed.
    pub fn legacy_hash(&self) -> Option<CardHash> {
        match &self {
            CardContent::Basic { .. } => None,
            CardContent::Cloze { text, start, end } => {
                let mut hasher = Hasher::new();
                hasher.update(b"Cloze");
                hasher.update(text.as_bytes());
                hasher.update(&start.to_le_bytes());
                hasher.update(&end.to_le_bytes());
                Some(hasher.finalize())
            }
        }
    }
```

3b. Add this free function at the bottom of `src/types/card.rs`, above the `tests` module:

```rust
/// The number of occurrences of `deletion` that begin before byte position
/// `start` in `bytes`. Sibling deletions in a card are disjoint, so an
/// earlier identical deletion always counts as an occurrence — this is what
/// keeps two identical deletions in the same card hashing differently.
/// Byte-wise on purpose: cloze positions are byte positions.
fn occurrence_index(bytes: &[u8], start: usize, deletion: &[u8]) -> usize {
    if deletion.is_empty() {
        return 0;
    }
    (0..start.min(bytes.len()))
        .filter(|&p| bytes[p..].starts_with(deletion))
        .count()
}
```

3c. Add to `impl Card` (after the existing `family_hash` method at `src/types/card.rs:98-100`):

```rust
    /// See [`CardContent::legacy_hash`].
    pub fn legacy_hash(&self) -> Option<CardHash> {
        self.content.legacy_hash()
    }
```

- [x] **Step 4: Run the full test suite and see it pass**

Run: `cargo test`
Expected: all tests PASS. Parser, drill, serve, edit, and export tests never assert concrete cloze hash values — they compare hashes computed through `Card::hash()` on both sides — so the scheme change must not break any of them. If any test fails on a hard-coded cloze hash value, update that value using the reference formula from Step 1 and note it in the commit message.

- [x] **Step 5: Commit**

```bash
git add src/types/card.rs
git commit -m "fix: derive cloze hashes from deletion content, not byte offsets (BUG-27)"
```

---

### Task 2: `meta` table, scheme marker, and `migrate_cloze_hashes` in the DB layer

**Files:**
- Modify: `src/schema.sql` (add `meta` table + seed row, after the `schema_version` table plan 08 added)
- Modify: `src/db.rs` (bump `SCHEMA_VERSION` 5 → 6, add migration arm 6, add `migrate_add_meta`, `cloze_hash_scheme`, `migrate_cloze_hashes`, and the two scheme constants)
- Test: `src/db.rs` (tests module)

**Interfaces:**
- Consumes (from plan 08, which MUST be merged first): `const SCHEMA_VERSION: i64 = 5;`, `fn migrate(tx: &Transaction) -> Fallible<()>` with its `match version` arms `1`–`5`, `set_schema_version`, the tests-module fixture `const OLD_SCHEMA: &str`, and the convergence test `test_migrated_schema_matches_fresh_schema` (which will automatically verify migration 6's table structure against `schema.sql`). Also `CardHash` (`ToSql`/`FromSql`), `params!`, `fail`.
- Produces (Task 3 depends on these exact names):
  - `pub const CLOZE_HASH_SCHEME_LEGACY: i64 = 1;` and `pub const CLOZE_HASH_SCHEME_CURRENT: i64 = 2;` (module-level in `src/db.rs`)
  - `pub fn cloze_hash_scheme(&self) -> Fallible<i64>` on `Database`
  - `pub fn migrate_cloze_hashes(&mut self, renames: &[(CardHash, CardHash)]) -> Fallible<usize>` on `Database` — renames every `(legacy, new)` pair and stamps the scheme `'2'`, all in ONE transaction; returns the number of cards renamed
  - `fn migrate_add_meta(tx: &Transaction) -> Fallible<()>`; `SCHEMA_VERSION` becomes 6

- [x] **Step 1: Write the failing regression tests**

Add to the `tests` module in `src/db.rs`:

```rust
    /// A freshly created database is already on the current cloze hash
    /// scheme: no load-time rehash will ever run on it.
    #[test]
    fn test_fresh_db_has_current_cloze_hash_scheme() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        assert_eq!(db.cloze_hash_scheme()?, CLOZE_HASH_SCHEME_CURRENT);
        Ok(())
    }

    /// A pre-existing database is stamped with the LEGACY scheme by SQL
    /// migration 6, so the load-time rehash knows it has work to do.
    #[test]
    fn test_legacy_db_is_stamped_with_legacy_scheme() -> Fallible<()> {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("legacy.db");
        let path = path.to_str().unwrap();
        {
            let conn = Connection::open(path)?;
            conn.execute_batch(OLD_SCHEMA)?;
        }
        let db = Database::new(path)?;
        assert_eq!(db.cloze_hash_scheme()?, CLOZE_HASH_SCHEME_LEGACY);
        Ok(())
    }

    /// migrate_cloze_hashes rewrites the card row; reviews and bookmarks
    /// follow via ON UPDATE CASCADE; performance columns ride along in the
    /// cards row; rows with no rename entry are untouched; the scheme is
    /// stamped current — all in one transaction.
    #[test]
    fn test_migrate_cloze_hashes_rewrites_all_references() -> Fallible<()> {
        let mut db = Database::new(":memory:")?;
        let legacy = CardHash::hash_bytes(b"legacy");
        let new = CardHash::hash_bytes(b"new");
        let orphan = CardHash::hash_bytes(b"orphan");
        let now = Timestamp::now();
        db.insert_card(legacy, now)?;
        db.insert_card(orphan, now)?;
        let session_id = db.create_session(now)?;
        db.insert_review_and_update_performance(
            session_id,
            &sample_review(legacy, now, 2.0),
            sample_performance(now, 2.0, 1),
        )?;
        db.insert_review_and_update_performance(
            session_id,
            &sample_review(orphan, now, 3.0),
            sample_performance(now, 3.0, 1),
        )?;
        db.insert_bookmark(legacy, Some("note".to_string()), now)?;
        // Simulate a legacy-scheme DB (a fresh one is stamped '2').
        db.conn.execute(
            "update meta set value = '1' where key = 'cloze_hash_scheme';",
            [],
        )?;

        let renamed = db.migrate_cloze_hashes(&[(legacy, new)])?;
        assert_eq!(renamed, 1);
        assert_eq!(db.cloze_hash_scheme()?, CLOZE_HASH_SCHEME_CURRENT);
        assert!(!db.card_exists(legacy)?);
        assert!(db.card_exists(new)?);
        // Performance followed the rename.
        assert!(matches!(
            db.get_card_performance(new)?,
            Performance::Reviewed(_)
        ));
        // Reviews followed via the cascade; the orphan's review is untouched.
        let hashes: Vec<CardHash> = db
            .get_reviews_for_session(session_id)?
            .iter()
            .map(|r| r.data.card_hash)
            .collect();
        assert!(hashes.contains(&new));
        assert!(!hashes.contains(&legacy));
        assert!(hashes.contains(&orphan));
        // The bookmark followed via the cascade.
        assert!(db.get_bookmark(new)?.is_some());
        // The card with no rename entry is untouched.
        assert!(db.card_exists(orphan)?);
        Ok(())
    }

    /// If the target hash already exists (only possible if the DB somehow
    /// already saw the new scheme), the rename is skipped: no data loss,
    /// the legacy row simply stays behind as an orphan.
    #[test]
    fn test_migrate_cloze_hashes_skips_when_target_exists() -> Fallible<()> {
        let mut db = Database::new(":memory:")?;
        let legacy = CardHash::hash_bytes(b"legacy");
        let new = CardHash::hash_bytes(b"new");
        let now = Timestamp::now();
        db.insert_card(legacy, now)?;
        db.insert_card(new, now)?;
        let renamed = db.migrate_cloze_hashes(&[(legacy, new)])?;
        assert_eq!(renamed, 0);
        assert!(db.card_exists(legacy)?);
        assert!(db.card_exists(new)?);
        Ok(())
    }
```

- [x] **Step 2: Run the tests and see them fail**

Run: `cargo test test_fresh_db_has_current_cloze_hash_scheme test_legacy_db_is_stamped_with_legacy_scheme test_migrate_cloze_hashes`
Expected: COMPILE ERROR — `CLOZE_HASH_SCHEME_CURRENT`, `cloze_hash_scheme`, and `migrate_cloze_hashes` do not exist yet. That is the failing state.

- [x] **Step 3: Implement migration 6, the constants, and the two methods**

3a. In `src/schema.sql`, append at the end of the file (after the `schema_version` table plan 08 added). Fresh databases only ever contain new-scheme hashes, so they are seeded `'2'`:

```sql

create table meta (
    key text primary key,
    value text not null
) strict;

insert into meta (key, value) values ('cloze_hash_scheme', '2');
```

3b. In `src/db.rs`, change plan 08's constant:

```rust
const SCHEMA_VERSION: i64 = 6;
```

3c. Add module-level constants directly below `SCHEMA_VERSION`:

```rust
/// Cloze hash schemes, stored in meta('cloze_hash_scheme'). Scheme 1 mixed
/// the deletion's byte offsets into the hash (platform-dependent); scheme 2
/// hashes the deletion's content and occurrence index instead. The upgrade
/// from 1 to 2 happens at collection load, not here: it needs parsed cards.
pub const CLOZE_HASH_SCHEME_LEGACY: i64 = 1;
pub const CLOZE_HASH_SCHEME_CURRENT: i64 = 2;
```

3d. Add the migration arm inside `migrate`'s `match version` (after plan 08's `5 => migrate_add_reviewed_date(tx)?,` arm):

```rust
            6 => migrate_add_meta(tx)?,
```

3e. Add next to the other `migrate_add_*` functions:

```rust
/// Migration 6: a generic key/value meta table. Databases that exist before
/// this migration were written with the legacy (offset-based) cloze hash
/// scheme, so they are stamped scheme 1; the load-time rehash upgrades them
/// to scheme 2. Fresh databases are seeded at scheme 2 by schema.sql.
fn migrate_add_meta(tx: &Transaction) -> Fallible<()> {
    tx.execute_batch(
        "create table if not exists meta (
            key text primary key,
            value text not null
        ) strict;",
    )?;
    tx.execute(
        "insert into meta (key, value) values ('cloze_hash_scheme', '1')
         on conflict (key) do nothing;",
        [],
    )?;
    Ok(())
}
```

3f. Add to `impl Database` (after `rename_card_hash`):

```rust
    /// Which cloze hash scheme this database's card hashes were written
    /// with. See CLOZE_HASH_SCHEME_LEGACY / CLOZE_HASH_SCHEME_CURRENT.
    pub fn cloze_hash_scheme(&self) -> Fallible<i64> {
        let value: String = self.conn.query_row(
            "select value from meta where key = 'cloze_hash_scheme';",
            [],
            |row| row.get(0),
        )?;
        match value.as_str() {
            "1" => Ok(CLOZE_HASH_SCHEME_LEGACY),
            "2" => Ok(CLOZE_HASH_SCHEME_CURRENT),
            other => fail(format!(
                "This database records an unknown cloze hash scheme ('{other}'). It was probably created by a newer version of hashcards. Please upgrade hashcards."
            )),
        }
    }

    /// One-time upgrade of legacy (offset-based) cloze card hashes to the
    /// content-based scheme. For each (legacy, new) pair, rewrites the card
    /// row's hash in place; the reviews and bookmarks referencing it follow
    /// via ON UPDATE CASCADE, and the card's performance columns live in the
    /// renamed row itself. Cards absent from `renames` are left untouched
    /// (they are orphans, exactly as `hashcards orphans` understands them).
    /// The whole upgrade — every rename plus the scheme stamp — is ONE
    /// transaction, so a crash can never leave the database half-migrated.
    /// Returns the number of cards renamed.
    pub fn migrate_cloze_hashes(
        &mut self,
        renames: &[(CardHash, CardHash)],
    ) -> Fallible<usize> {
        let tx = self.conn.transaction()?;
        let mut renamed: usize = 0;
        for (legacy, new) in renames {
            let target_exists: i64 = tx.query_row(
                "select count(*) from cards where card_hash = ?;",
                params![new],
                |row| row.get(0),
            )?;
            if target_exists > 0 {
                // The new hash is already present; leave the legacy row
                // behind as an orphan rather than destroy either row.
                continue;
            }
            let rows = tx.execute(
                "update cards set card_hash = ? where card_hash = ?;",
                params![new, legacy],
            )?;
            if rows == 1 {
                renamed += 1;
            }
        }
        tx.execute(
            "update meta set value = '2' where key = 'cloze_hash_scheme';",
            [],
        )?;
        tx.commit()?;
        Ok(renamed)
    }
```

- [x] **Step 4: Run the tests and see them pass**

Run: `cargo test`
Expected: all tests PASS, including the four new ones and plan 08's `test_migrated_schema_matches_fresh_schema` (which now also proves migration 6's `meta` table structure matches `schema.sql` — the seeded *value* differs by design and is not part of the structural snapshot) and `test_newer_schema_version_is_rejected` (unchanged: 999 > 6).

- [x] **Step 5: Commit**

```bash
git add src/db.rs src/schema.sql
git commit -m "feat: meta table with cloze hash scheme marker; transactional hash rename (BUG-27)"
```

---

### Task 3: Load-time rehash in `Collection` with end-to-end migration test

**Files:**
- Modify: `src/collection.rs:51-103` (`Collection::with_db_path`: make `db` mutable, call the upgrade after media validation) plus a new private function `upgrade_cloze_hashes` and a new `tests` module (the file has none today)

**Interfaces:**
- Consumes: Task 1's `Card::legacy_hash() -> Option<CardHash>` and `Card::hash() -> CardHash`; Task 2's `Database::cloze_hash_scheme()`, `Database::migrate_cloze_hashes(&[(CardHash, CardHash)]) -> Fallible<usize>`, and `CLOZE_HASH_SCHEME_CURRENT`.
- Produces: `fn upgrade_cloze_hashes(db: &mut Database, cards: &[Card]) -> Fallible<()>` (private to `collection.rs`); every `Collection` constructor (`new`, `with_db_path` — drill, serve, check, orphans, stats, export all go through these) now guarantees the DB is on the current scheme before any other DB access.

- [ ] **Step 1: Write the failing end-to-end regression test**

Append to `src/collection.rs` (the file currently has no tests module). The test builds a DB with old-scheme hashes computed **with the old algorithm inline** (independent of `legacy_hash()`), plus a genuinely-deleted card, then loads the collection and asserts reviews/performance/bookmarks followed their cards while the deleted card's rows stayed orphaned.

```rust
#[cfg(test)]
mod tests {
    use std::fs::write;
    use std::iter::once;

    use rusqlite::Connection;

    use super::*;
    use crate::db::CLOZE_HASH_SCHEME_CURRENT;
    use crate::db::ReviewRecord;
    use crate::fsrs::Grade;
    use crate::helper::create_tmp_directory;
    use crate::types::card::CardContent;
    use crate::types::card_hash::CardHash;
    use crate::types::performance::Performance;
    use crate::types::performance::ReviewedPerformance;
    use crate::types::timestamp::Timestamp;

    /// Compute a card's hash under the OLD (pre-v2) scheme, inline:
    /// blake3("Cloze" ++ text ++ start.to_le_bytes() ++ end.to_le_bytes()).
    fn old_scheme_hash(content: &CardContent) -> CardHash {
        match content {
            CardContent::Cloze { text, start, end } => {
                let mut input: Vec<u8> = Vec::new();
                input.extend_from_slice(b"Cloze");
                input.extend_from_slice(text.as_bytes());
                input.extend_from_slice(&start.to_le_bytes());
                input.extend_from_slice(&end.to_le_bytes());
                CardHash::hash_bytes(&input)
            }
            CardContent::Basic { .. } => panic!("expected a cloze card"),
        }
    }

    /// BUG-27 regression: loading a collection whose DB holds legacy
    /// (offset-based) cloze hashes re-links reviews, performance, and
    /// bookmarks to the new content-based hashes in one pass, while a
    /// genuinely-deleted card's rows stay behind as orphans.
    #[test]
    fn test_load_migrates_legacy_cloze_hashes() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        write(dir.join("Deck.md"), "C: The capital of [France] is [Paris].\n")?;
        // Parse the deck to learn the deletions, so the legacy hashes can be
        // computed inline with the old algorithm.
        let cards = parse_deck(&dir)?;
        assert_eq!(cards.len(), 2);
        let legacy_hashes: Vec<CardHash> =
            cards.iter().map(|c| old_scheme_hash(c.content())).collect();
        let orphan = CardHash::hash_bytes(b"a card that was deleted from the deck");

        let now = Timestamp::now();
        let db_path = dir.join("hashcards.db");
        let session_id;
        {
            let mut db = Database::new(db_path.to_str().expect("utf-8 path"))?;
            session_id = db.create_session(now)?;
            for hash in legacy_hashes.iter().chain(once(&orphan)) {
                db.insert_card(*hash, now)?;
                let review = ReviewRecord {
                    card_hash: *hash,
                    reviewed_at: now,
                    grade: Grade::Good,
                    stability: 2.0,
                    difficulty: 2.0,
                    interval_raw: 1.0,
                    interval_days: 1,
                    due_date: now.date(),
                    duration_ms: None,
                };
                let performance = Performance::Reviewed(ReviewedPerformance {
                    last_reviewed_at: now,
                    stability: 2.0,
                    difficulty: 2.0,
                    interval_raw: 1.0,
                    interval_days: 1,
                    due_date: now.date(),
                    review_count: 1,
                });
                db.insert_review_and_update_performance(session_id, &review, performance)?;
            }
            db.insert_bookmark(legacy_hashes[0], Some("check this".to_string()), now)?;
        }
        // A real legacy DB is stamped scheme 1 by SQL migration 6; a fresh
        // Database::new stamps scheme 2, so flip it to simulate legacy.
        {
            let conn = Connection::open(&db_path)?;
            conn.execute(
                "update meta set value = '1' where key = 'cloze_hash_scheme';",
                [],
            )?;
        }

        // Loading the collection performs the one-time rehash.
        let coll = Collection::new(Some(dir.display().to_string()))?;
        let db_hashes = coll.db.card_hashes()?;
        for card in &coll.cards {
            assert!(
                db_hashes.contains(&card.hash()),
                "card {} was not re-linked",
                card.hash()
            );
            // The card's performance followed it.
            assert!(matches!(
                coll.db.get_card_performance(card.hash())?,
                Performance::Reviewed(_)
            ));
        }
        for legacy in &legacy_hashes {
            assert!(!db_hashes.contains(legacy), "legacy hash {legacy} still present");
        }
        // Reviews were rewritten to the new hashes; the orphan's review is untouched.
        let review_hashes: Vec<CardHash> = coll
            .db
            .get_reviews_for_session(session_id)?
            .iter()
            .map(|r| r.data.card_hash)
            .collect();
        for card in &coll.cards {
            assert!(review_hashes.contains(&card.hash()));
        }
        assert!(review_hashes.contains(&orphan));
        // The bookmark followed its card (cards[0] parsed first = France).
        assert!(coll.db.get_bookmark(cards[0].hash())?.is_some());
        // The genuinely-deleted card's row is untouched: still an orphan.
        assert!(coll.db.card_exists(orphan)?);
        assert_eq!(coll.db.cloze_hash_scheme()?, CLOZE_HASH_SCHEME_CURRENT);
        Ok(())
    }

    /// An already-current DB is not touched: reloading is a cheap no-op.
    #[test]
    fn test_load_of_current_scheme_db_is_a_noop() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        write(dir.join("Deck.md"), "C: Water is [H2O].\n")?;
        let first = Collection::new(Some(dir.display().to_string()))?;
        let hashes_before = first.db.card_hashes()?;
        assert_eq!(first.db.cloze_hash_scheme()?, CLOZE_HASH_SCHEME_CURRENT);
        drop(first);
        let second = Collection::new(Some(dir.display().to_string()))?;
        assert_eq!(second.db.card_hashes()?, hashes_before);
        assert_eq!(second.db.cloze_hash_scheme()?, CLOZE_HASH_SCHEME_CURRENT);
        Ok(())
    }
}
```

- [ ] **Step 2: Run the tests and see the regression test fail**

Run: `cargo test test_load_migrates_legacy_cloze_hashes test_load_of_current_scheme_db_is_a_noop`
Expected: `test_load_migrates_legacy_cloze_hashes` FAILS at `card {} was not re-linked` — nothing performs the rehash yet, so the DB still holds the legacy hashes. `test_load_of_current_scheme_db_is_a_noop` PASSES (fresh DBs are already scheme 2 after Task 2 — pin-down test).

- [ ] **Step 3: Implement the load-time upgrade hook**

3a. In `src/collection.rs`, add imports at the top (with the other `use` statements):

```rust
use std::collections::HashSet;

use crate::db::CLOZE_HASH_SCHEME_CURRENT;
use crate::types::card_hash::CardHash;
```

3b. In `Collection::with_db_path`, make the database binding mutable — change (currently `src/collection.rs:61`):

```rust
        let db: Database = Database::new(db_path)?;
```

to:

```rust
        let mut db: Database = Database::new(db_path)?;
```

3c. Still in `with_db_path`, after the `validate_media_files(&cards, &directory)?;` line and before the final `Ok(Self { ... })`, insert:

```rust
        // One-time re-link of legacy cloze hashes; a cheap no-op normally.
        upgrade_cloze_hashes(&mut db, &cards)?;
```

3d. Add this function at the bottom of `src/collection.rs`, above the `tests` module:

```rust
/// One-time upgrade of legacy (offset-based) cloze card hashes to the
/// content-based scheme (see CardContent::hash and CardContent::legacy_hash).
///
/// For every parsed cloze card, its legacy hash — a function of the card's
/// family text plus the deletion's identity (content and occurrence) — is
/// mapped to its new hash, and the database rewrites all referencing rows in
/// one transaction. Database rows matching no parsed card (deleted cards, or
/// hashes written on a machine with different endianness/pointer width) are
/// left untouched; the `orphans` command reports them exactly as before.
///
/// On an already-upgraded database this returns after a single SELECT.
fn upgrade_cloze_hashes(db: &mut Database, cards: &[Card]) -> Fallible<()> {
    if db.cloze_hash_scheme()? == CLOZE_HASH_SCHEME_CURRENT {
        return Ok(());
    }
    let mut seen: HashSet<CardHash> = HashSet::new();
    let mut renames: Vec<(CardHash, CardHash)> = Vec::new();
    for card in cards {
        if let Some(legacy) = card.legacy_hash() {
            // Duplicate hand-written cards share one hash; rename it once.
            if seen.insert(legacy) {
                renames.push((legacy, card.hash()));
            }
        }
    }
    let renamed = db.migrate_cloze_hashes(&renames)?;
    log::info!("Upgraded {renamed} cloze card hash(es) to the content-based scheme.");
    Ok(())
}
```

- [ ] **Step 4: Run the full test suite and see it pass**

Run: `cargo test`
Expected: all tests PASS, including both new collection tests. Every command (`drill`, `serve`, `check`, `orphans`, `stats`, `export`) constructs its DB access through `Collection::new`/`with_db_path`, so they all get the upgrade for free; the serve-mode edit path (`src/cmd/serve/edit.rs`) opens `Database::new` directly, but only against a DB that a `Collection` load has already upgraded in the same process.

- [ ] **Step 5: Commit**

```bash
git add src/collection.rs
git commit -m "feat: re-link legacy cloze hashes at collection load (BUG-27)"
```

---

### Task 4: Export coverage, README, CHANGELOG, and IDEAS.md check

**Files:**
- Test: `src/cmd/export.rs` (tests module)
- Modify: `README.md` (Cloze Cards section, after the multi-line example ending at line 264)
- Modify: `CHANGELOG.xml` (new `<breaking>` block under `<unreleased>`)
- Check (no change expected): `IDEAS.md`

**Interfaces:**
- Consumes: Task 1's hash scheme (reference formula), `Hasher` from `src/types/card_hash.rs`, existing export test helpers (`create_tmp_directory` is already imported in `export.rs`'s tests via `crate::helper`).
- Produces: nothing new in code — this task pins down and documents behavior.

**JSON export implications, for the record:** `CardExport.hash`, `CardExport.family_hash`, and `ReviewExport.hash` (`src/cmd/export.rs:56-57`, `:111`) are read through `Collection`, which upgrades the DB before export — so an export taken after upgrading is internally consistent, but its cloze hash values differ from any export taken with an older hashcards. `CardContentExport::Cloze::start`/`end` (`src/cmd/export.rs:79-83`) remain BYTE positions in the export, unchanged (see BUG-26 for their doc wording). External consumers that key on cloze hash values must re-key once — the changelog entry says so.

- [ ] **Step 1: Write the export pin-down test**

Add to the `tests` module in `src/cmd/export.rs` (imports shown go at the top of the module, next to the existing ones):

```rust
    use std::fs::write as fs_write;

    use crate::helper::create_tmp_directory;
    use crate::types::card_hash::Hasher;

    /// The JSON export carries the content-based (v2) cloze hashes.
    #[test]
    fn test_export_uses_content_based_cloze_hashes() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        fs_write(dir.join("Deck.md"), "C: Water is [wet].\n")?;
        let coll = Collection::new(Some(dir.display().to_string()))?;
        let export = get_export(coll)?;
        let json = serde_json::to_string(&export)?;
        // Reference formula: clean text "Water is wet." with deletion "wet"
        // (bytes 9..=11), first occurrence.
        let mut hasher = Hasher::new();
        hasher.update(b"ClozeV2");
        hasher.update(b"Water is wet.");
        hasher.update(&[0xFF]);
        hasher.update(b"wet");
        hasher.update(&[0xFF]);
        hasher.update(b"0");
        let expected = hasher.finalize();
        assert!(
            json.contains(&expected.to_hex()),
            "export does not contain the v2 cloze hash"
        );
        Ok(())
    }
```

Note: `create_tmp_directory` may already be imported in this tests module (it is used by `test_full_export`); if so, skip the duplicate `use`.

- [ ] **Step 2: Run the test and see it pass**

Run: `cargo test test_export_uses_content_based_cloze_hashes`
Expected: PASS — this is a pin-down test guarding the export against future hash-input drift (any change to the scheme now fails here and in Task 1's reference tests together).

- [ ] **Step 3: Document the scheme in README.md**

In `README.md`, in the "Cloze Cards" section, insert after the multi-line cloze example's closing fence (line 264, just before the `### Separators` heading) the following paragraph:

```markdown
Each bracketed deletion becomes its own card. A cloze card's identity (its
hash) is derived from the card's text, the deleted substring, and — when the
same substring is deleted more than once — an occurrence index. It does not
depend on byte offsets or on the machine's CPU architecture, so a
`hashcards.db` written on one computer works on any other.
```

- [ ] **Step 4: Update CHANGELOG.xml (breaking-change entry)**

`CHANGELOG.xsd` allows a `<breaking>` change list inside `<unreleased>` (the `changesType` choice includes `added`/`fixed`/`changed`/`removed`/`deprecated`/`security`/`breaking` in any order). In `CHANGELOG.xml`, inside `<unreleased>`, after the closing `</changed>` tag, add (match the file's 8/12-space indentation; if a `<breaking>` block already exists by then, just append the `<change>` to it):

```xml
        <breaking>
            <change author="mstoeck3">
                Cloze card hashes are now derived from the card's text, the deleted substring, and an occurrence index, instead of the deletion's raw byte offsets. Card identity no longer depends on CPU endianness or pointer width, so a hashcards.db can move between machines. On first load with this version, an existing database is upgraded automatically in a single transaction: every cloze card's reviews, performance data, and bookmarks are re-linked to its new hash. Cards no longer present in the deck keep their old hashes and remain visible to the orphans command. An upgraded database is rejected by the immediately-preceding hashcards versions (which check the schema version); versions older than that would treat all cloze history as orphaned — do not open an upgraded database with an old hashcards. (BUG-27)
            </change>
        </breaking>
```

Validate: `xmllint --noout --schema CHANGELOG.xsd CHANGELOG.xml` (if `xmllint` is unavailable, eyeball against the XSD: `breaking` contains `change` elements with an optional `author` attribute).

- [ ] **Step 5: Confirm IDEAS.md needs no change**

Read `IDEAS.md`. As of this plan's writing it contains: Card Stages, Term-Definition Cards, Preview Command, Jitter, Logo — **no** entry about the cloze rehash. The spec's BUG-27 instruction to "document it in IDEAS.md with a migration sketch" applied only to the deferred option, which the project owner has rejected in favor of doing the rehash now; since the work is done, nothing is added to IDEAS.md. If someone has added a cloze-rehash/BUG-27 entry to IDEAS.md in the meantime, delete that entry (the work is no longer future work) and include the deletion in this task's commit.

- [ ] **Step 6: Run the full suite one last time**

Run: `cargo test`
Expected: all tests PASS.

- [ ] **Step 7: Commit**

```bash
git add src/cmd/export.rs README.md CHANGELOG.xml
git commit -m "docs: document content-based cloze hash scheme; export pin-down test (BUG-27)"
```

---

## Spec discrepancies

Checked every line number the spec and this plan cite against the current code (branch state as of 2026-08-31):

- **BUG-27 cites `card.rs:166-167`** for the `usize::to_le_bytes()` hashing — accurate: `src/types/card.rs:166` (`start.to_le_bytes()`) and `:167` (`end.to_le_bytes()`), inside `CardContent::hash` at `:155-171`.
- **BUG-27 cites `card.rs:176-186`** for `family_hash` — accurate.
- **`src/cmd/export.rs:80-83`** (cloze `start`/`end` export fields, referenced via BUG-26) — the `Cloze` variant of `CardContentExport` actually spans `:79-83`; off by one line, immaterial.
- **Owner decision supersedes the spec's fix text.** SPEC.md's BUG-27 prescribes the "minimal, non-breaking" fixed-width big-endian tweak and defers the full rehash to an IDEAS.md sketch. Per the project owner's explicit decision, this plan implements the full offset-independent rehash now, with automatic DB migration; consequently no fixed-width interim scheme is ever shipped and no IDEAS.md sketch is written (IDEAS.md contains no BUG-27 entry today — verified).
- **"Use `family_hash` to re-link performance" (spec's migration sketch):** implemented equivalently and more precisely by recomputing each parsed card's *legacy hash* (a function of the family text plus the deletion's identity) — see the design-decisions section. This avoids the ambiguity a pure family-level match would have between sibling deletions.
- **Dependency on plan 08 is real and structural:** migration numbers 1–5, `SCHEMA_VERSION`, the `migrate` match, `OLD_SCHEMA`, and the future-version rejection guard all come from `2026-08-31-08-database.md`. This plan claims **migration 6** and bumps `SCHEMA_VERSION` to 6. If another plan lands a migration 6 first, renumber this plan's migration to the next free number (mechanical: the arm number, the constant, and nothing else).
- **Cross-platform legacy DBs cannot be re-linked** (a DB written on a big-endian or 32-bit machine holds hashes this machine's `legacy_hash()` cannot reproduce). This is inherent — those hashes are exactly the platform-dependence being removed — and such rows are left as orphans, matching the "unmatched old rows are left untouched" requirement. The changelog entry warns accordingly.
