# Parser Features (FEAT-06, FEAT-07, BUG-12) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `T:`/`D:` term-definition shorthand (FEAT-06), report duplicate-hash cards with both file:line locations in `hashcards check` (FEAT-07), and make duplicate cards produce a user-visible warning at collection load instead of silently vanishing or aborting drill startup (BUG-12).

**Architecture:** All three items live in the parser/collection-load pipeline. We introduce two small domain types, `CardLocation` and `DuplicateCard`, in `src/parser.rs`; `Parser` gains a `parse_with_duplicates` method (the existing `parse` keeps its signature by delegating), and `parse_deck` returns a new `ParsedDeck { cards, duplicates }` struct. `Collection` carries the duplicates through to the drill startup (which prints warnings) and to `hashcards check` (which lists them). The `T:`/`D:` shorthand is a pure parse-time expansion: two new `Line` variants and two new `State` variants that finalize into two ordinary `CardContent::Basic` cards, so hashes are automatically identical to hand-written equivalents.

**Tech Stack:** Rust (edition per `Cargo.toml`), cargo test. No new dependencies.

**Spec:** SPEC.md (items FEAT-06, FEAT-07, BUG-12; PR group 13 "Parser features")

## Global Constraints

From SPEC.md "Global requirements" (verbatim):

- Every bugfix starts with a failing regression test.
- No `unwrap()` in production code; use `Fallible`, `?`, and `fail()`.
- All error messages are user-facing and clear.
- Reviews and performance are written in one transaction; undo voids, never deletes.
- Cloze positions are byte positions: `.bytes()`, never `.chars()`.
- Update `CHANGELOG.xml` per item.

Additional project rules from `CLAUDE.md` that bind every task below:

- Use newtypes for domain concepts.
- Keep functions small and focused.
- Prefer imports to fully qualified names (add `use` statements at the top of the module).
- Tests may use `unwrap()`; production code may not.
- When updating `CLAUDE.md` or docs, be terse.

Run the full suite (`cargo test`) before every commit, not just the new tests.

---

## File Structure

| File | Role in this plan |
|---|---|
| `src/parser.rs` | New types `CardLocation`, `DuplicateCard`, `ParsedFile`, `ParsedDeck`; `Parser::parse_with_duplicates`; `parse_deck` signature change; `T:`/`D:` line/state handling. All new parser tests go in its existing `#[cfg(test)] mod tests`. |
| `src/collection.rs` | `Collection` gains `pub duplicates: Vec<DuplicateCard>`; new tests module. |
| `src/cmd/drill/server.rs` | `start_server` destructures the new field and prints one warning line per duplicate. |
| `src/cmd/check.rs` | `check_collection` lists duplicates before printing `ok`. |
| `src/cmd/export.rs` | Test-only caller of `parse_deck` updated for the new return type. |
| `README.md` | New "Term-Definition Cards" section; note under `check` about duplicate reporting. |
| `CHANGELOG.xml` | One entry per item. |

Verified facts about the current code that this plan builds on (line numbers checked against `master`):

- `src/parser.rs:104-147` — `parse_deck` walks the directory, then `sort_by_key(hash)` (`:141`) and `dedup_by_key(hash)` (`:144`) **silently** drop duplicates.
- `src/parser.rs:262-281` — `Parser::parse` also silently dedups within a single file (`HashSet` at `:273-280`).
- `src/parser.rs:202-231` — the `Line` enum and `Line::read`; prefix predicates at `:233-247`; `trim` at `:249-251` strips the 2-byte tag prefix.
- `src/parser.rs:185-200` — the `State` enum; `parse_line` at `:283-458`.
- `src/types/card.rs` — `Card::new` computes the hash from `CardContent`; `Card::hash()`, `Card::file_path()`, `Card::range()` accessors exist; `range` is a pair of **0-based** line indices (`ParserError::fmt` prints `line_num + 1`, `parser.rs:178`).
- `src/cmd/drill/cache.rs:45-53` — `Cache::insert` fails on a duplicate hash; `src/cmd/drill/server.rs:164` propagates with `?`.
- `src/cmd/drill/server.rs:104-109` — `start_server` destructures `Collection { directory, db, cards, macros }`; adding a field to `Collection` makes this a compile error until updated.
- `src/collection.rs:84-92` — `Collection::with_db_path` calls `parse_deck(&directory)?`.
- `src/cmd/check.rs` — `check_collection` is `let _ = Collection::new(directory)?; println!("ok");`.
- `src/cmd/export.rs:237` — test-only call `let deck = parse_deck(&PathBuf::from(dir.clone()))?;` then iterates `for card in deck`.
- `src/cmd/serve/edit.rs:186` — serve-mode edit calls `Parser::parse` on a single file; its behavior must not change (hash-migration pairing relies on the current dedup semantics), which is why `parse` keeps its exact signature and dedup behavior.
- Test helpers: `crate::helper::create_tmp_directory()` and `create_tmp_copy_of_test_directory()` (`src/helper.rs`); parser tests use `make_test_parser()` (`src/parser.rs:933-935`, builds `Parser::new("test_deck".to_string(), PathBuf::from("test.md"))`).
- `CardHash` is `Copy`, `Eq`, `Hash`, `Ord`, and implements `Display` (used as `{card_hash}` in `cache.rs:47`).
- `DeckName` (`src/types/aliases.rs`) is a `String` alias.

---

### Task 1: Duplicate-card domain types and per-file duplicate reporting

**Files:**
- Modify: `src/parser.rs` (imports at `:15-29`; new types after `ParserError` around `:183`; `Parser::parse` at `:262-281`; tests module at `:630+`)

**Interfaces:**
- Consumes: existing `Card` (`hash()`, `file_path()`, `range()`), `CardHash`.
- Produces (later tasks rely on these exact names):
  - `pub struct CardLocation` with `pub fn of(card: &Card) -> CardLocation` and `impl Display` rendering `"{path}:{1-based-line}"`.
  - `pub struct DuplicateCard` with `pub fn new(hash: CardHash, kept: CardLocation, ignored: CardLocation) -> DuplicateCard`, accessors `pub fn kept(&self) -> &CardLocation`, `pub fn ignored(&self) -> &CardLocation`, and `impl Display` rendering `"duplicate card {hash}: kept {kept}, ignored {ignored}"`.
  - `pub struct ParsedFile { pub cards: Vec<Card>, pub duplicates: Vec<DuplicateCard> }`.
  - `pub fn Parser::parse_with_duplicates(&self, text: &str) -> Result<ParsedFile, ParserError>`.
  - `Parser::parse` keeps its exact current signature and observable behavior (returns deduped `Vec<Card>`).

- [x] **Step 1: Write the failing tests**

Add to the `tests` module in `src/parser.rs` (alongside the existing tests; `use super::*;` is already in scope there):

```rust
#[test]
fn test_duplicate_cards_within_file_reported() -> Result<(), ParserError> {
    let input = "Q: a\nA: b\n\n---\n\nQ: a\nA: b";
    let parser = make_test_parser();
    let parsed = parser.parse_with_duplicates(input)?;
    assert_eq!(parsed.cards.len(), 1);
    assert_eq!(parsed.duplicates.len(), 1);
    let dup = &parsed.duplicates[0];
    assert_eq!(dup.kept().to_string(), "test.md:1");
    assert_eq!(dup.ignored().to_string(), "test.md:6");
    Ok(())
}

#[test]
fn test_duplicate_card_display_names_both_locations() -> Result<(), ParserError> {
    let input = "Q: a\nA: b\n\n---\n\nQ: a\nA: b";
    let parser = make_test_parser();
    let parsed = parser.parse_with_duplicates(input)?;
    let message = parsed.duplicates[0].to_string();
    assert!(message.contains("test.md:1"), "message was: {message}");
    assert!(message.contains("test.md:6"), "message was: {message}");
    assert!(message.contains("duplicate card"), "message was: {message}");
    Ok(())
}

#[test]
fn test_parse_still_dedups_silently() -> Result<(), ParserError> {
    // The plain `parse` API (used by serve-mode edit) must keep returning
    // the deduplicated card list with no behavior change.
    let input = "Q: a\nA: b\n\n---\n\nQ: a\nA: b";
    let parser = make_test_parser();
    let cards = parser.parse(input)?;
    assert_eq!(cards.len(), 1);
    Ok(())
}
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test test_duplicate_card`
Expected: compile error — `parse_with_duplicates` does not exist. (A compile failure of the test target counts as the failing state.)

- [x] **Step 3: Implement the types and `parse_with_duplicates`**

In `src/parser.rs`, add to the imports at the top (keeping the existing ones):

```rust
use std::collections::HashMap;

use crate::types::card_hash::CardHash;
```

After the `impl Error for ParserError {}` line (currently `:183`), add:

```rust
/// The location of a card in the collection: file path and 1-based line
/// number of the card's first line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardLocation {
    file_path: PathBuf,
    line: usize,
}

impl CardLocation {
    pub fn of(card: &Card) -> Self {
        Self {
            file_path: card.file_path().clone(),
            line: card.range().0 + 1,
        }
    }
}

impl Display for CardLocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.file_path.display(), self.line)
    }
}

/// Two byte-identical cards found while loading a collection. The `kept`
/// copy stays in the deck; the `ignored` copy is dropped.
#[derive(Debug, Clone)]
pub struct DuplicateCard {
    hash: CardHash,
    kept: CardLocation,
    ignored: CardLocation,
}

impl DuplicateCard {
    pub fn new(hash: CardHash, kept: CardLocation, ignored: CardLocation) -> Self {
        Self { hash, kept, ignored }
    }

    pub fn kept(&self) -> &CardLocation {
        &self.kept
    }

    pub fn ignored(&self) -> &CardLocation {
        &self.ignored
    }
}

impl Display for DuplicateCard {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "duplicate card {}: kept {}, ignored {}",
            self.hash, self.kept, self.ignored
        )
    }
}

/// The result of parsing a single deck file.
pub struct ParsedFile {
    pub cards: Vec<Card>,
    pub duplicates: Vec<DuplicateCard>,
}
```

Replace the body of `Parser::parse` (currently `:262-281`) and add `parse_with_duplicates` next to it:

```rust
    /// Parse all the cards in the given text, silently dropping duplicates.
    ///
    /// This is the historical API; serve-mode edit relies on its exact
    /// dedup behavior. Prefer `parse_with_duplicates` where duplicate
    /// reporting matters.
    pub fn parse(&self, text: &str) -> Result<Vec<Card>, ParserError> {
        Ok(self.parse_with_duplicates(text)?.cards)
    }

    /// Parse all the cards in the given text, reporting duplicates.
    ///
    /// Byte-identical cards share a hash; the first occurrence is kept and
    /// each further occurrence is recorded as a `DuplicateCard`.
    pub fn parse_with_duplicates(&self, text: &str) -> Result<ParsedFile, ParserError> {
        let mut cards = Vec::new();
        let mut state = State::Start;
        let lines: Vec<&str> = text.lines().collect();
        let last_line = if lines.is_empty() { 0 } else { lines.len() - 1 };
        for (line_num, line) in lines.iter().enumerate() {
            let line = Line::read(line);
            state = self.parse_line(state, line, line_num, &mut cards)?;
        }
        self.parse_line(state, Line::Eof, last_line, &mut cards)?;

        let mut index_of: HashMap<CardHash, usize> = HashMap::new();
        let mut unique_cards: Vec<Card> = Vec::new();
        let mut duplicates: Vec<DuplicateCard> = Vec::new();
        for card in cards {
            match index_of.get(&card.hash()) {
                Some(&kept_index) => {
                    // `kept_index` always points into `unique_cards`.
                    duplicates.push(DuplicateCard::new(
                        card.hash(),
                        CardLocation::of(&unique_cards[kept_index]),
                        CardLocation::of(&card),
                    ));
                }
                None => {
                    index_of.insert(card.hash(), unique_cards.len());
                    unique_cards.push(card);
                }
            }
        }
        Ok(ParsedFile {
            cards: unique_cards,
            duplicates,
        })
    }
```

The old `HashSet`-based dedup block inside `parse` is deleted. `HashSet` is no longer used by production code in this file, so move the import: delete `use std::collections::HashSet;` from the top of `src/parser.rs` and add `use std::collections::HashSet;` inside the `#[cfg(test)] mod tests` block (Task 4's hash-equivalence test uses it from there).

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test test_duplicate_card test_parse_still_dedups_silently`
Expected: 3 passed. Then run `cargo test` — the whole suite must pass (no existing caller of `parse` changes behavior).

- [x] **Step 5: Commit**

```bash
git add src/parser.rs
git commit -m "feat: report within-file duplicate cards from the parser (BUG-12 groundwork)"
```

---

### Task 2: BUG-12 — collection load dedups with a warning; drill startup never aborts

**Files:**
- Modify: `src/parser.rs` (`parse_deck` at `:104-147`)
- Modify: `src/collection.rs` (struct at `:28-33`, `parse_deck` call at `:84-92`; add a tests module at the end)
- Modify: `src/cmd/drill/server.rs` (destructure at `:104-109`, cache-fill loop at `:160-165`)
- Modify: `src/cmd/export.rs` (test at `:234-250`)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: `DuplicateCard`, `CardLocation`, `ParsedFile`, `Parser::parse_with_duplicates` from Task 1.
- Produces:
  - `pub struct ParsedDeck { pub cards: Vec<Card>, pub duplicates: Vec<DuplicateCard> }` in `src/parser.rs`.
  - `pub fn parse_deck(directory: &PathBuf) -> Fallible<ParsedDeck>` (signature change).
  - `Collection` gains `pub duplicates: Vec<DuplicateCard>` (Task 3 reads it).

- [ ] **Step 1: Write the failing regression test**

BUG-12's spec text says duplicates abort drill startup via `Cache::insert` (`cache.rs:45-53`) propagated at `server.rs:164`. On current master that abort is actually unreachable, because `parse_deck` already dedups silently (`parser.rs:141-144`) — see "Spec discrepancies" below. The regression test therefore pins the full required behavior: two byte-identical cards load without error (guarding the no-abort property against any future removal of the dedup), drilling material contains exactly one copy, and — the part that fails today — the duplicate is *reported* with both locations instead of vanishing.

Add a tests module at the end of `src/collection.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::fs::write;

    use super::*;
    use crate::error::Fallible;
    use crate::helper::create_tmp_directory;

    /// Regression test for BUG-12: two byte-identical cards must not abort
    /// collection loading (and therefore drill startup); one copy is kept
    /// and the duplicate is reported with both file locations.
    #[test]
    fn test_duplicate_cards_across_files_load_with_warning() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        write(dir.join("a.md"), "Q: same question\nA: same answer\n")?;
        write(dir.join("b.md"), "Q: same question\nA: same answer\n")?;

        let collection = Collection::new(Some(dir.display().to_string()))?;

        // Exactly one copy survives.
        assert_eq!(collection.cards.len(), 1);

        // The duplicate is reported, naming both files.
        assert_eq!(collection.duplicates.len(), 1);
        let dup = &collection.duplicates[0];
        let locations = format!("{} {}", dup.kept(), dup.ignored());
        assert!(locations.contains("a.md:1"), "locations were: {locations}");
        assert!(locations.contains("b.md:1"), "locations were: {locations}");
        Ok(())
    }

    #[test]
    fn test_collection_without_duplicates_reports_none() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        write(dir.join("a.md"), "Q: question one\nA: answer one\n")?;
        write(dir.join("b.md"), "Q: question two\nA: answer two\n")?;
        let collection = Collection::new(Some(dir.display().to_string()))?;
        assert_eq!(collection.cards.len(), 2);
        assert!(collection.duplicates.is_empty());
        Ok(())
    }
}
```

Note: walkdir order is not guaranteed, so the test asserts on the *pair* of locations, not on which copy was kept.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test test_duplicate_cards_across_files`
Expected: compile error — `Collection` has no field `duplicates`.

- [ ] **Step 3: Implement `ParsedDeck` and thread duplicates through**

In `src/parser.rs`, add next to `ParsedFile`:

```rust
/// The result of parsing every deck file in a collection directory.
pub struct ParsedDeck {
    pub cards: Vec<Card>,
    pub duplicates: Vec<DuplicateCard>,
}
```

Replace `parse_deck` (currently `:104-147`) so the walk collects per-file duplicates and the cross-file dedup records locations instead of `dedup_by_key`:

```rust
/// Parses all Markdown files in the given directory.
///
/// Byte-identical cards (same hash) are deduplicated: the first copy
/// encountered is kept, and every dropped copy is reported in
/// `ParsedDeck::duplicates` with both locations.
pub fn parse_deck(directory: &PathBuf) -> Fallible<ParsedDeck> {
    let mut all_cards = Vec::new();
    let mut duplicates = Vec::new();
    for entry in WalkDir::new(directory) {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            let text = read_to_string(path)?;

            // Extract frontmatter and get custom deck name if specified
            let (metadata, content) = extract_frontmatter(&text)?;

            let deck_name: DeckName = metadata.name.unwrap_or_else(|| {
                path.strip_prefix(directory)
                    .ok()
                    .map(|rel| {
                        rel.with_extension("")
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join("/")
                    })
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        path.file_stem()
                            .map(|os_str| os_str.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "None".to_string())
                    })
            });

            let parser = Parser::new(deck_name, path.to_path_buf());
            let parsed = parser.parse_with_duplicates(content)?;
            all_cards.extend(parsed.cards);
            duplicates.extend(parsed.duplicates);
        }
    }

    // Cards are sorted by their hash to make subsequent code more
    // deterministic. The sort is stable, so among byte-identical cards the
    // first one encountered on disk stays first and is the copy we keep.
    all_cards.sort_by_key(|c| c.hash());

    // Remove cross-file duplicates, recording both locations.
    let mut cards: Vec<Card> = Vec::new();
    for card in all_cards {
        match cards.last() {
            Some(kept) if kept.hash() == card.hash() => {
                duplicates.push(DuplicateCard::new(
                    card.hash(),
                    CardLocation::of(kept),
                    CardLocation::of(&card),
                ));
            }
            _ => cards.push(card),
        }
    }

    Ok(ParsedDeck { cards, duplicates })
}
```

(The `unwrap_or_else` calls above are the pre-existing deck-name fallback logic, unchanged — they are closures on `Option`, not `unwrap()`.)

In `src/collection.rs`:

```rust
use crate::parser::DuplicateCard;
```

(add to the existing imports), extend the struct:

```rust
pub struct Collection {
    pub directory: PathBuf,
    pub db: Database,
    pub cards: Vec<Card>,
    pub macros: Vec<(String, String)>,
    /// Byte-identical cards that were deduplicated at load time.
    pub duplicates: Vec<DuplicateCard>,
}
```

and replace the card-loading block in `with_db_path` (currently `:84-92`) plus the final `Ok`:

```rust
        let parsed = {
            log::debug!("Loading deck...");
            let start = Instant::now();
            let parsed = parse_deck(&directory)?;
            let end = Instant::now();
            let duration = end.duration_since(start).as_millis();
            log::debug!("Deck loaded in {duration}ms.");
            parsed
        };
        let cards: Vec<Card> = parsed.cards;
        let duplicates: Vec<DuplicateCard> = parsed.duplicates;

        // Validate media files
        validate_media_files(&cards, &directory)?;

        Ok(Self {
            directory,
            db,
            cards,
            macros,
            duplicates,
        })
```

In `src/cmd/drill/server.rs`, update the destructure at `:104-109` and warn immediately after it:

```rust
    let Collection {
        directory,
        db,
        cards,
        macros,
        duplicates,
    } = Collection::new(config.directory)?;

    // BUG-12: byte-identical cards are deduplicated at load time. Warn so
    // the user can clean them up; drilling proceeds with one copy.
    for duplicate in &duplicates {
        eprintln!("Warning: {duplicate}. Drilling with one copy.");
    }
```

In `src/cmd/export.rs` test `test_full_export`, change the `parse_deck` call (`:237`) and its loop:

```rust
        let deck = parse_deck(&PathBuf::from(dir.clone()))?;
        let now = Timestamp::now();
        let mut reviews = Vec::new();
        for card in deck.cards {
```

(only the `parse_deck` line's binding usage changes: iterate `deck.cards` instead of `deck`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test test_duplicate_cards_across_files test_collection_without_duplicates`
Expected: 2 passed. Then `cargo test` — full suite green (compiler will point out any missed `parse_deck` caller; fix by using `.cards`).

- [ ] **Step 5: Update CHANGELOG.xml for BUG-12**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add as the first `<change>` (format matches the existing entries; validate the shape against `CHANGELOG.xsd`: `<change>` has text content and an optional `author` attribute):

```xml
            <change author="claude">
                Byte-identical duplicate cards no longer disappear silently: collection loading keeps one copy and prints a warning naming both file locations, and drilling proceeds normally. Duplicates can no longer abort drill startup.
            </change>
```

- [ ] **Step 6: Commit**

```bash
git add src/parser.rs src/collection.rs src/cmd/drill/server.rs src/cmd/export.rs CHANGELOG.xml
git commit -m "fix: warn (with both locations) instead of silently dropping duplicate cards (BUG-12)"
```

---

### Task 3: FEAT-07 — `hashcards check` lists duplicate cards

**Files:**
- Modify: `src/cmd/check.rs` (whole file is 41 lines)
- Modify: `README.md` (`### check` section, lines 155-162)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: `Collection::duplicates` (Task 2), `DuplicateCard: Display` (Task 1).
- Produces: `check_collection` keeps its signature `pub fn check_collection(directory: Option<String>) -> Fallible<()>`; new helper `fn duplicate_report(duplicates: &[DuplicateCard]) -> Vec<String>` (module-private, unit-testable).

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/cmd/check.rs`:

```rust
    use std::fs::write;

    use crate::collection::Collection;
    use crate::helper::create_tmp_directory;

    use super::duplicate_report;

    #[test]
    fn test_check_reports_duplicates_and_succeeds() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        write(dir.join("a.md"), "Q: same question\nA: same answer\n")?;
        write(dir.join("b.md"), "Q: same question\nA: same answer\n")?;
        let dir_string = dir.display().to_string();

        // `check` succeeds: duplicates are a warning, not an error.
        assert!(check_collection(Some(dir_string.clone())).is_ok());

        // The report lists the duplicate with both file:line locations.
        let collection = Collection::new(Some(dir_string))?;
        let report = duplicate_report(&collection.duplicates);
        assert_eq!(report.len(), 1);
        assert!(report[0].contains("a.md:1"), "report was: {}", report[0]);
        assert!(report[0].contains("b.md:1"), "report was: {}", report[0]);
        Ok(())
    }

    #[test]
    fn test_duplicate_report_empty_without_duplicates() {
        assert!(duplicate_report(&[]).is_empty());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test test_check_reports_duplicates test_duplicate_report_empty`
Expected: compile error — `duplicate_report` does not exist.

- [ ] **Step 3: Implement the duplicate report in `check`**

Replace the production part of `src/cmd/check.rs` with:

```rust
use crate::collection::Collection;
use crate::error::Fallible;
use crate::parser::DuplicateCard;

pub fn check_collection(directory: Option<String>) -> Fallible<()> {
    let collection = Collection::new(directory)?;
    for line in duplicate_report(&collection.duplicates) {
        println!("{line}");
    }
    println!("ok");
    Ok(())
}

/// One warning line per duplicate card, naming both file:line locations.
fn duplicate_report(duplicates: &[DuplicateCard]) -> Vec<String> {
    duplicates
        .iter()
        .map(|duplicate| format!("warning: {duplicate}"))
        .collect()
}
```

(The existing `test_non_existent_directory` and `test_directory` tests stay unchanged.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib check`
Expected: all `check` tests pass, including the two pre-existing ones. Then `cargo test` — full suite green.

- [ ] **Step 5: Update CHANGELOG.xml for FEAT-07**

In `CHANGELOG.xml`, inside `<unreleased><added>`, add as the first `<change>`:

```xml
            <change author="claude">
                `hashcards check` now lists byte-identical duplicate cards, naming both file:line locations for each duplicate.
            </change>
```

Also document it in `README.md` under the `### check` section (currently lines 155-162), after the code block:

```markdown
If the collection contains byte-identical duplicate cards, `check` prints a
warning for each one, naming both file:line locations.
```

- [ ] **Step 6: Commit**

```bash
git add src/cmd/check.rs CHANGELOG.xml README.md
git commit -m "feat: list duplicate cards with both locations in hashcards check (FEAT-07)"
```

---

### Task 4: FEAT-06 — `T:`/`D:` shorthand expands into two reciprocal cards

**Files:**
- Modify: `src/parser.rs` (`Line` enum `:202-215`, `Line::read` `:217-231`, predicates `:233-247`, `State` enum `:185-200`, `parse_line` `:283-458`; tests module)

**Interfaces:**
- Consumes: `Card::new`, `CardContent::new_basic` (`src/types/card.rs`), existing `State`/`Line` machinery.
- Produces (Task 5 relies on these exact names):
  - `Line::StartTerm(String)` and `Line::StartDefinition(String)` variants; predicates `fn is_term(line: &str) -> bool` (`starts_with("T:")`) and `fn is_definition(line: &str) -> bool` (`starts_with("D:")`).
  - `State::ReadingTerm { term: String, start_line: usize }` and `State::ReadingDefinition { term: String, definition: String, start_line: usize }` variants.
  - `fn Parser::push_term_cards(&self, term: String, definition: String, start_line: usize, end_line: usize, cards: &mut Vec<Card>)` — pushes the "Define:" card then the "Term for:" card.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/parser.rs`:

```rust
#[test]
fn test_term_definition_expands_to_two_cards() -> Result<(), ParserError> {
    let input = "T: Monoid\nD: A semigroup with an identity element.";
    let parser = make_test_parser();
    let cards = parser.parse(input)?;
    assert_eq!(cards.len(), 2);
    assert!(matches!(
        &cards[0].content(),
        CardContent::Basic { question, answer }
            if question == "Define: Monoid"
            && answer == "A semigroup with an identity element."
    ));
    assert!(matches!(
        &cards[1].content(),
        CardContent::Basic { question, answer }
            if question == "Term for: A semigroup with an identity element."
            && answer == "Monoid"
    ));
    Ok(())
}

#[test]
fn test_term_definition_hashes_match_handwritten_cards() -> Result<(), ParserError> {
    let shorthand = "T: Monoid\nD: A semigroup with an identity element.";
    let handwritten = "Q: Define: Monoid\n\
                       A: A semigroup with an identity element.\n\
                       \n\
                       ---\n\
                       \n\
                       Q: Term for: A semigroup with an identity element.\n\
                       A: Monoid";
    let parser = make_test_parser();
    let from_shorthand: HashSet<_> = parser.parse(shorthand)?.iter().map(|c| c.hash()).collect();
    let from_handwritten: HashSet<_> = parser.parse(handwritten)?.iter().map(|c| c.hash()).collect();
    assert_eq!(from_shorthand.len(), 2);
    assert_eq!(from_shorthand, from_handwritten);
    Ok(())
}

#[test]
fn test_two_term_pairs_separated() -> Result<(), ParserError> {
    let input = "T: Monoid\nD: A semigroup with an identity element.\n\n---\n\nT: Magma\nD: A set with a binary operation.";
    let parser = make_test_parser();
    let cards = parser.parse(input)?;
    assert_eq!(cards.len(), 4);
    Ok(())
}

#[test]
fn test_term_pair_followed_directly_by_term_pair() -> Result<(), ParserError> {
    // A new T: finalizes the previous pair, like Q: after an answer.
    let input = "T: Monoid\nD: A semigroup with an identity element.\nT: Magma\nD: A set with a binary operation.";
    let parser = make_test_parser();
    let cards = parser.parse(input)?;
    assert_eq!(cards.len(), 4);
    Ok(())
}
```

(`HashSet` is available in the tests module via the `use std::collections::HashSet;` import that Task 1 moved there.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test test_term_definition test_two_term_pairs test_term_pair_followed`
Expected: FAIL — `T:`/`D:` lines are treated as plain `Text`, so `parse` returns 0 cards (`assert_eq!(cards.len(), 2)` fails) or errors.

- [ ] **Step 3: Implement the shorthand**

In `src/parser.rs`:

1. Extend the `Line` enum (`:202-215`):

```rust
    /// A line like `T: <text>` (term-definition shorthand).
    StartTerm(String),
    /// A line like `D: <text>` (term-definition shorthand).
    StartDefinition(String),
```

2. Extend `Line::read` (`:217-231`) — insert before the `is_separator` branch:

```rust
        } else if is_term(line) {
            Line::StartTerm(trim(line))
        } else if is_definition(line) {
            Line::StartDefinition(trim(line))
```

3. Add the predicates next to `is_cloze` (`:241-243`):

```rust
fn is_term(line: &str) -> bool {
    line.starts_with("T:")
}

fn is_definition(line: &str) -> bool {
    line.starts_with("D:")
}
```

(`trim` at `:249-251` strips a 2-byte prefix, which `T:`/`D:` both have.)

4. Extend the `State` enum (`:185-200`):

```rust
    /// Reading a term (T:), waiting for its definition.
    ReadingTerm { term: String, start_line: usize },
    /// Reading a definition (D:) for a term.
    ReadingDefinition {
        term: String,
        definition: String,
        start_line: usize,
    },
```

5. Add the expansion helper to `impl Parser` (next to `parse_cloze_cards`):

```rust
    /// Expand a term-definition pair into its two reciprocal basic cards.
    ///
    /// The generated cards are ordinary basic cards, so their hashes are
    /// identical to hand-written `Q: Define: ...` / `Q: Term for: ...`
    /// equivalents.
    fn push_term_cards(
        &self,
        term: String,
        definition: String,
        start_line: usize,
        end_line: usize,
        cards: &mut Vec<Card>,
    ) {
        let term = term.trim();
        let definition = definition.trim();
        cards.push(Card::new(
            self.deck_name.clone(),
            self.file_path.clone(),
            (start_line, end_line),
            CardContent::new_basic(format!("Define: {term}"), definition),
        ));
        cards.push(Card::new(
            self.deck_name.clone(),
            self.file_path.clone(),
            (start_line, end_line),
            CardContent::new_basic(format!("Term for: {definition}"), term),
        ));
    }
```

6. In `parse_line`, handle the new line variants in the existing states and add the two new state arms. The complete additions (the existing arms stay as they are):

In `State::Start` (`:291-308`), add:

```rust
                Line::StartTerm(text) => Ok(State::ReadingTerm {
                    term: text,
                    start_line: line_num,
                }),
                Line::StartDefinition(_) => Err(ParserError::new(
                    "Found definition tag without a term.",
                    self.file_path.clone(),
                    line_num,
                )),
```

In `State::ReadingQuestion` (`:309-342`), add:

```rust
                Line::StartTerm(_) => Err(ParserError::new(
                    "Found term tag while reading a question.",
                    self.file_path.clone(),
                    line_num,
                )),
                Line::StartDefinition(_) => Err(ParserError::new(
                    "Found definition tag while reading a question.",
                    self.file_path.clone(),
                    line_num,
                )),
```

In `State::ReadingAnswer` (`:343-413`), add (mirrors the existing `StartQuestion`/`StartCloze` finalize-and-transition arms):

```rust
                    Line::StartTerm(text) => {
                        // Finalize the previous card.
                        let card = Card::new(
                            self.deck_name.clone(),
                            self.file_path.clone(),
                            (start_line, line_num),
                            CardContent::new_basic(question, answer),
                        );
                        cards.push(card);
                        // Start reading a term.
                        Ok(State::ReadingTerm {
                            term: text,
                            start_line: line_num,
                        })
                    }
                    Line::StartDefinition(_) => Err(ParserError::new(
                        "Found definition tag without a term.",
                        self.file_path.clone(),
                        line_num,
                    )),
```

In `State::ReadingCloze` (`:414-455`), add:

```rust
                    Line::StartTerm(new_text) => {
                        // Finalize the previous cloze card.
                        cards.extend(self.parse_cloze_cards(text, start_line, line_num)?);
                        // Start reading a term.
                        Ok(State::ReadingTerm {
                            term: new_text,
                            start_line: line_num,
                        })
                    }
                    Line::StartDefinition(_) => Err(ParserError::new(
                        "Found definition tag without a term.",
                        self.file_path.clone(),
                        line_num,
                    )),
```

Add the two new state arms before `State::End`:

```rust
            State::ReadingTerm { term, start_line } => match line {
                Line::StartQuestion(_) => Err(ParserError::new(
                    "Found question tag while reading a term without a definition.",
                    self.file_path.clone(),
                    line_num,
                )),
                Line::StartAnswer(_) => Err(ParserError::new(
                    "Found answer tag while reading a term. Terms take a definition (D:), not an answer.",
                    self.file_path.clone(),
                    line_num,
                )),
                Line::StartCloze(_) => Err(ParserError::new(
                    "Found cloze tag while reading a term without a definition.",
                    self.file_path.clone(),
                    line_num,
                )),
                Line::StartTerm(_) => Err(ParserError::new(
                    "New term without a definition.",
                    self.file_path.clone(),
                    line_num,
                )),
                Line::StartDefinition(text) => Ok(State::ReadingDefinition {
                    term,
                    definition: text,
                    start_line,
                }),
                Line::Separator => Err(ParserError::new(
                    "Found flashcard separator while reading a term without a definition.",
                    self.file_path.clone(),
                    line_num,
                )),
                Line::Text(text) => Ok(State::ReadingTerm {
                    term: format!("{term}\n{text}"),
                    start_line,
                }),
                Line::Eof => Err(ParserError::new(
                    "File ended while reading a term without a definition.",
                    self.file_path.clone(),
                    line_num,
                )),
            },
            State::ReadingDefinition {
                term,
                definition,
                start_line,
            } => match line {
                Line::StartQuestion(text) => {
                    self.push_term_cards(term, definition, start_line, line_num, cards);
                    Ok(State::ReadingQuestion {
                        question: text,
                        start_line: line_num,
                    })
                }
                Line::StartAnswer(_) => Err(ParserError::new(
                    "Found answer tag while reading a definition.",
                    self.file_path.clone(),
                    line_num,
                )),
                Line::StartCloze(text) => {
                    self.push_term_cards(term, definition, start_line, line_num, cards);
                    Ok(State::ReadingCloze {
                        text,
                        start_line: line_num,
                    })
                }
                Line::StartTerm(text) => {
                    self.push_term_cards(term, definition, start_line, line_num, cards);
                    Ok(State::ReadingTerm {
                        term: text,
                        start_line: line_num,
                    })
                }
                Line::StartDefinition(_) => Err(ParserError::new(
                    "Found definition tag while already reading a definition.",
                    self.file_path.clone(),
                    line_num,
                )),
                Line::Separator => {
                    self.push_term_cards(term, definition, start_line, line_num, cards);
                    Ok(State::Start)
                }
                Line::Text(text) => Ok(State::ReadingDefinition {
                    term,
                    definition: format!("{definition}\n{text}"),
                    start_line,
                }),
                Line::Eof => {
                    self.push_term_cards(term, definition, start_line, line_num, cards);
                    Ok(State::End)
                }
            },
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test test_term_definition test_two_term_pairs test_term_pair_followed`
Expected: 4 passed. Then `cargo test` — full suite green (the compiler enforces that every `match` on `Line` covers the new variants).

- [ ] **Step 5: Commit**

```bash
git add src/parser.rs
git commit -m "feat: T:/D: term-definition shorthand expands into two reciprocal cards (FEAT-06)"
```

---

### Task 5: FEAT-06 error cases, multi-line support, docs, changelog

**Files:**
- Modify: `src/parser.rs` (tests module only — the error arms exist after Task 4)
- Modify: `README.md` (insert a section between "Cloze Cards", ending ~line 264, and "### Separators" at line 266)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: everything from Task 4 (`Line::StartTerm`, `State::ReadingTerm`, `push_term_cards`, and the error messages exactly as written there).
- Produces: nothing new — this task pins error behavior with tests and ships the user-facing docs.

- [ ] **Step 1: Write the failing (or newly pinning) tests**

Add to the `tests` module in `src/parser.rs`. These test the error arms written in Task 4; if any fail, the Task 4 arms are wrong — fix the arms, not the tests.

```rust
#[test]
fn test_definition_without_term_errors() {
    let input = "D: A semigroup with an identity element.";
    let parser = make_test_parser();
    let result = parser.parse(input);
    assert!(result.is_err());
    let message = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(message.contains("definition tag without a term"), "message was: {message}");
}

#[test]
fn test_term_without_definition_at_eof_errors() {
    let input = "T: Monoid";
    let parser = make_test_parser();
    let result = parser.parse(input);
    assert!(result.is_err());
    let message = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(message.contains("without a definition"), "message was: {message}");
}

#[test]
fn test_term_then_question_errors() {
    let input = "T: Monoid\nQ: What is a monoid?";
    let parser = make_test_parser();
    assert!(parser.parse(input).is_err());
}

#[test]
fn test_term_then_separator_errors() {
    let input = "T: Monoid\n---";
    let parser = make_test_parser();
    assert!(parser.parse(input).is_err());
}

#[test]
fn test_multiline_term_and_definition() -> Result<(), ParserError> {
    let input = "T: Monoid\nhomomorphism\nD: A map between monoids\nthat preserves the operation and identity.";
    let parser = make_test_parser();
    let cards = parser.parse(input)?;
    assert_eq!(cards.len(), 2);
    assert!(matches!(
        &cards[0].content(),
        CardContent::Basic { question, .. }
            if question == "Define: Monoid\nhomomorphism"
    ));
    Ok(())
}

#[test]
fn test_term_pair_after_basic_card() -> Result<(), ParserError> {
    let input = "Q: What is Rust?\nA: A systems programming language.\nT: Monoid\nD: A semigroup with an identity element.";
    let parser = make_test_parser();
    let cards = parser.parse(input)?;
    assert_eq!(cards.len(), 3);
    Ok(())
}

#[test]
fn test_term_pair_after_cloze_card() -> Result<(), ParserError> {
    let input = "C: An [agonist] activates a receptor.\nT: Monoid\nD: A semigroup with an identity element.";
    let parser = make_test_parser();
    let cards = parser.parse(input)?;
    assert_eq!(cards.len(), 3);
    Ok(())
}

#[test]
fn test_term_pair_followed_by_question() -> Result<(), ParserError> {
    let input = "T: Monoid\nD: A semigroup with an identity element.\nQ: What is Rust?\nA: A language.";
    let parser = make_test_parser();
    let cards = parser.parse(input)?;
    assert_eq!(cards.len(), 3);
    Ok(())
}

#[test]
fn test_definition_inside_definition_errors() {
    let input = "T: Monoid\nD: first\nD: second";
    let parser = make_test_parser();
    assert!(parser.parse(input).is_err());
}
```

(Tests may use `unwrap_or_default`/`unwrap` freely; the no-`unwrap()` rule binds production code only.)

- [ ] **Step 2: Run the tests**

Run: `cargo test test_definition test_term_`
Expected: all pass if Task 4's arms are correct; any failure means a Task 4 arm diverges from this pinned behavior — fix the arm in `parse_line` and re-run until green.

- [ ] **Step 3: Document the syntax in README.md**

Insert between the end of the "Cloze Cards" section (after the multi-line cloze example, currently ending around line 264) and `### Separators` (line 266). (The block below is fenced with four backticks because the inserted README text itself contains three-backtick fences.)

````markdown
### Term-Definition Cards

Term-definition pairs start with the `T:` and `D:` tags:

```
T: Monoid
D: A semigroup with an identity element.
```

This is shorthand: at parse time, the pair expands into two ordinary
front-back cards, one in each direction:

```
Q: Define: Monoid
A: A semigroup with an identity element.

---

Q: Term for: A semigroup with an identity element.
A: Monoid
```

The generated cards are indistinguishable from hand-written ones — same
content, same hashes — so converting between the shorthand and the explicit
form preserves review history. Like questions and answers, terms and
definitions can span multiple lines.

Note that lines starting with `T:` or `D:` are now card tags everywhere,
just like `Q:` and `A:`; to use such text literally inside a card, don't
start a line with it.
````

- [ ] **Step 4: Update CHANGELOG.xml for FEAT-06**

In `CHANGELOG.xml`, inside `<unreleased><added>`, add:

```xml
            <change author="claude">
                Term-definition shorthand: a `T:`/`D:` line pair expands at parse time into two reciprocal front-back cards ("Define: X" and "Term for: Y"). The generated cards hash identically to hand-written equivalents, so review history is preserved when converting. Note: lines beginning with `T:` or `D:` are now card tags, like `Q:` and `A:`.
            </change>
```

- [ ] **Step 5: Run the full suite**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/parser.rs README.md CHANGELOG.xml
git commit -m "feat: pin T:/D: error handling; document term-definition shorthand (FEAT-06)"
```

---

## Spec discrepancies

Checked against `master` while writing this plan:

1. **BUG-12's described failure is currently unreachable.** The spec says duplicates abort drill startup via `cache.rs:45-52` / `server.rs:164`. Those citations are accurate (`Cache::insert` fails on a duplicate at `src/cmd/drill/cache.rs:45-53`, propagated with `?` at `src/cmd/drill/server.rs:164`), but `parse_deck` already deduplicates silently — `all_cards.sort_by_key` + `dedup_by_key` at `src/parser.rs:141-144` (added in commit `006542f`, "Fix #4") — and `Parser::parse` additionally dedups within each file at `src/parser.rs:273-280`. So byte-identical cards cannot reach `Cache::insert` today and drill startup does not abort. The remaining real defect is that duplicates vanish *silently*, with no warning and no locations; that is what Task 2 fixes, and its regression test also pins the no-abort property in case the silent dedup is ever removed.
2. **BUG-12 spec line citation drift is minor:** `cache.rs:45-52` — the `insert` function actually spans lines 45-53 (error branch at 47). Substance correct.
3. **Duplicate reporting granularity:** because `Parser::parse` (not just `parse_deck`) dedups, within-file duplicates never reach the collection level on master. The spec text for FEAT-07 does not distinguish within-file from cross-file duplicates; this plan reports both (Task 1 handles within-file, Task 2 cross-file) while deliberately leaving the `Parser::parse` signature and behavior untouched, because serve-mode edit (`src/cmd/serve/edit.rs:186`) depends on its dedup semantics for hash migration (see BUG-35 — out of scope here).
4. **FEAT-06 backward-compatibility note not in the spec:** introducing `T:`/`D:` as line tags changes the meaning of existing card text lines that happen to start with `T:` or `D:` (previously plain `Text`). The spec is silent on this. The plan follows the spec as written and calls the change out in the README and the changelog entry.
5. **Serve mode is untouched by BUG-12's warning.** The spec says "warning naming both locations" at collection load; drill prints to stderr and `check` reports duplicates. Serve mode gets the `duplicates` field on `Collection` for free but prints nothing — surfacing warnings in serve mode belongs to FEAT-01 (flash messages, PR group 2), not this PR.
