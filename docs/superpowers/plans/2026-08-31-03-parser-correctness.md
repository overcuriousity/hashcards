# Parser Correctness (PR group 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the seven parser-correctness bugs BUG-16, BUG-17, BUG-18, BUG-19, BUG-20, BUG-21, BUG-26: empty-cloze underflow, markdown links eaten as deletions, card syntax inside code fences, silent nested/unterminated brackets, frontmatter-offset line numbers, debug-formatted/pathless errors, and "character" docs that mean bytes.

**Architecture:** All parsing lives in `src/parser.rs` (a hand-rolled line state machine plus a byte scanner in `parse_cloze_cards`). Fixes are surgical: guards and a link/fence mode in the existing scanners, a `line_offset` threaded through `Parser`, and message cleanups in `src/error.rs`. BUG-20 changes the meaning of `Card::range()` from content-relative to absolute file lines, which requires updating the splice logic in `src/cmd/serve/edit.rs` in the same task.

**Tech Stack:** Rust (edition per `Cargo.toml`), cargo test. No new dependencies.

**Spec:** SPEC.md

## Global Constraints

Copied verbatim from SPEC.md "Global requirements":

- Every bugfix starts with a failing regression test.
- No `unwrap()` in production code; use `Fallible`, `?`, and `fail()`.
- All error messages are user-facing and clear.
- Reviews and performance are written in one transaction; undo voids, never deletes.
- Cloze positions are byte positions: `.bytes()`, never `.chars()`.
- Update `CHANGELOG.xml` per item.

Additional project rules (CLAUDE.md):

- Prefer imports to fully qualified names.
- `unwrap()` is allowed in tests only.
- Cloze deletion positions are BYTE positions: always `.bytes()`, never `.chars()`.
- CHANGELOG.xml must validate against `CHANGELOG.xsd`: entries are `<change author="...">text</change>` inside `<fixed>`/`<changed>` under `<unreleased>`.

**Task ordering matters.** Tasks 1–3 edit the same two loops in `parse_cloze_cards` and each task's code blocks show the state of the code *after the previous tasks are done*. Execute in order.

Line numbers below refer to `src/parser.rs`, `src/error.rs`, `src/types/card.rs`, `src/cmd/export.rs`, `src/cmd/serve/edit.rs` as of commit `be55f80` and drift as tasks land; anchor on the quoted code, not the numbers.

---

### Task 1: BUG-16 — Empty cloze `[]` is a parse error, not a usize underflow

**Files:**
- Modify: `src/parser.rs` (the `]`-handling branch of the position loop in `parse_cloze_cards`, currently lines 572–583; tests module at the bottom)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: existing `ParserError::new(message, file_path, line_num)` (`src/parser.rs:162`), whose `Display` appends ` Location: {path}:{line+1}`.
- Produces: `parse_cloze_cards` returns `Err(ParserError)` with message `"Cloze deletion is empty."` when a deletion contains zero bytes. No signature changes.

Background: in `parse_cloze_cards`, `index` counts bytes of clean text. At a closing `]` with an open deletion, `end = index` and the card is built with `end - 1` (`src/parser.rs:573-574`). For `C: []`, `start == Some(0)` and `end == 0`, so `end - 1` underflows (panic in debug, `usize::MAX` positions in release).

- [x] **Step 1: Write the failing regression tests**

Add to the `tests` module in `src/parser.rs` (next to `test_cloze_without_deletions`):

```rust
    /// BUG-16: an empty cloze deletion must be a parse error, not a usize
    /// underflow (debug: panic; release: usize::MAX positions).
    #[test]
    fn test_empty_cloze_deletion_is_error() {
        let input = "C: [] foo";
        let parser = make_test_parser();
        let result = parser.parse(input);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(
            err.to_string(),
            "Cloze deletion is empty. Location: test.md:1"
        );
    }

    /// BUG-16 companion: a one-byte deletion still parses.
    #[test]
    fn test_single_byte_cloze_deletion_parses() -> Result<(), ParserError> {
        let input = "C: [a]";
        let parser = make_test_parser();
        let cards = parser.parse(input)?;
        assert_cloze(&cards, "a", &[(0, 0)]);
        Ok(())
    }
```

- [x] **Step 2: Run tests to verify the first one fails**

Run: `cargo test test_empty_cloze_deletion_is_error test_single_byte_cloze_deletion_parses`
Expected: `test_empty_cloze_deletion_is_error` FAILS — in debug builds with `attempt to subtract with overflow` (a panic, not the expected error). `test_single_byte_cloze_deletion_parses` PASSES (it guards against regressions in Step 3).

- [x] **Step 3: Implement the guard**

In `src/parser.rs`, in the position loop of `parse_cloze_cards`, replace the `else if let Some(s) = start` branch (currently lines 572–583):

```rust
                } else if let Some(s) = start {
                    let end = index;
                    if end == s {
                        return Err(ParserError::new(
                            "Cloze deletion is empty.",
                            self.file_path.clone(),
                            start_line,
                        ));
                    }
                    let content = CardContent::new_cloze(clean_text.clone(), s, end - 1);
                    let card = Card::new(
                        self.deck_name.clone(),
                        self.file_path.clone(),
                        (start_line, end_line),
                        content,
                    );
                    cards.push(card);
                    start = None;
                }
```

- [x] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all tests PASS, including both new ones.

- [x] **Step 5: Update CHANGELOG.xml**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, after the last existing `<change>` in that section, add:

```xml
            <change author="claude">
                An empty cloze deletion (`[]`) now produces a clear parse error with the file and line, instead of crashing the parser (debug builds) or silently corrupting cloze positions (release builds). (BUG-16)
            </change>
```

- [x] **Step 6: Commit**

```bash
git add src/parser.rs CHANGELOG.xml
git commit -m "fix: empty cloze deletion is a parse error, not a usize underflow (BUG-16)"
```

---

### Task 2: BUG-19 — Error on nested brackets and unterminated deletions

**Files:**
- Modify: `src/parser.rs` (the `[`-handling branch, the final `else` byte branch, and the end of the position loop in `parse_cloze_cards`; tests module)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: Task 1's empty-deletion guard (the `]` branch shown in Task 1 Step 3 stays as-is).
- Produces: `parse_cloze_cards` returns `Err(ParserError)` with message `"Nested cloze brackets."` on `[` while a deletion is open, and `"Unterminated cloze deletion."` on a newline or end-of-text with a deletion open. Task 3 builds its link handling on top of the `[` branch defined here.

Background: today `[[a]]` silently overwrites `start` (`src/parser.rs:561`), dropping a bracket; `C: foo [bar` with no `]` falls through to the misleading `"Cloze card must contain at least one cloze deletion."` (`src/parser.rs:618-623`). Note: this task makes deletions that span a newline (`[foo\nbar]`) an error; the parser previously allowed them silently, and no existing test uses one.

- [x] **Step 1: Write the failing regression tests**

Add to the `tests` module in `src/parser.rs`:

```rust
    /// BUG-19: a `[` while a deletion is already open must be an error, not a
    /// silent restart of the deletion.
    #[test]
    fn test_nested_cloze_brackets_is_error() {
        let input = "C: [[a]]";
        let parser = make_test_parser();
        let result = parser.parse(input);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(
            err.to_string(),
            "Nested cloze brackets. Location: test.md:1"
        );
    }

    /// BUG-19: an unmatched `[` at end of text must say so, not complain that
    /// the card has no deletions.
    #[test]
    fn test_unterminated_cloze_at_eof_is_error() {
        let input = "C: foo [bar";
        let parser = make_test_parser();
        let result = parser.parse(input);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(
            err.to_string(),
            "Unterminated cloze deletion. Location: test.md:1"
        );
    }

    /// BUG-19: a deletion left open at the end of a line is an error.
    #[test]
    fn test_unterminated_cloze_at_eol_is_error() {
        let input = "C: foo [bar\nbaz] quux";
        let parser = make_test_parser();
        let result = parser.parse(input);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(
            err.to_string(),
            "Unterminated cloze deletion. Location: test.md:1"
        );
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test test_nested_cloze_brackets_is_error test_unterminated_cloze_at_eof_is_error test_unterminated_cloze_at_eol_is_error`
Expected: all three FAIL. The nested test parses successfully (no error); the two unterminated tests fail on the message: they currently get `"Cloze card must contain at least one cloze deletion. Location: test.md:1"`.

- [x] **Step 3: Implement the errors**

In `src/parser.rs`, in the position loop of `parse_cloze_cards`:

(a) Replace the `if c == b'['` branch (currently lines 552–562):

```rust
            if c == b'[' {
                if image_mode {
                    // We are in image mode, so this bracket is part of a markdown image.
                    index += 1;
                } else if escape_mode {
                    // We are in escape mode, so this bracket is part of the markdown text.
                    index += 1;
                    escape_mode = false;
                } else if start.is_some() {
                    return Err(ParserError::new(
                        "Nested cloze brackets.",
                        self.file_path.clone(),
                        start_line,
                    ));
                } else {
                    start = Some(index);
                }
            } else if c == b']' {
```

(b) Insert a newline branch immediately before the final `} else {` / `index += 1;` catch-all of the loop (currently lines 613–615):

```rust
            } else if c == b'\n' {
                if start.is_some() {
                    return Err(ParserError::new(
                        "Unterminated cloze deletion.",
                        self.file_path.clone(),
                        start_line,
                    ));
                }
                index += 1;
            } else {
                index += 1;
            }
```

(c) Immediately after the closing brace of the position loop (before the `if cards.is_empty()` check at the end of `parse_cloze_cards`), add:

```rust
        if start.is_some() {
            return Err(ParserError::new(
                "Unterminated cloze deletion.",
                self.file_path.clone(),
                start_line,
            ));
        }
```

- [x] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all PASS, including `test_multi_line_cloze` and `test_cloze_with_initial_blank_line` (their deletions never cross a newline).

- [x] **Step 5: Update CHANGELOG.xml**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add after the Task 1 entry:

```xml
            <change author="claude">
                Nested cloze brackets (`[[a]]`) and cloze deletions left open at the end of a line or file now produce clear parse errors with the file and line, instead of silently dropping brackets or reporting a misleading "must contain at least one cloze deletion" message. (BUG-19)
            </change>
```

- [x] **Step 6: Commit**

```bash
git add src/parser.rs CHANGELOG.xml
git commit -m "fix: error on nested and unterminated cloze brackets (BUG-19)"
```

---

### Task 3: BUG-17 — Markdown links `[text](url)` are skipped, not treated as deletions

**Files:**
- Modify: `src/parser.rs` (both loops in `parse_cloze_cards`: the clean-text loop and the position loop; a new helper function; tests module)
- Modify: `README.md` (the "Cloze Cards" section, after the multi-line example ending at line 264)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: Task 1's empty-deletion guard and Task 2's nested/unterminated errors — the `[` branch modified below is the one Task 2 Step 3(a) produced.
- Produces: new private helper `fn is_markdown_link_open(text: &str, open_pos: usize) -> bool` in `src/parser.rs`, and a `link_mode: bool` flag in both scanner loops. Link brackets and their `(url)` stay verbatim in the clean text, exactly like image syntax.

Background: the scanner special-cases only `![` (image) and `\[`/`\]` (escape), so `C: See [the docs](https://x)` turns the link text into a deletion and leaves `(https://x)` behind. The fix: when a `[` opens a bracket group that closes with `](`, treat the whole bracket pair as markdown, in both loops.

- [x] **Step 1: Write the failing regression test**

Add to the `tests` module in `src/parser.rs`:

```rust
    /// BUG-17: `[text](url)` is a markdown link, not a cloze deletion.
    /// Byte positions: "See [the docs](https://x) for " is 30 bytes of clean
    /// text; the deletion covers "answer", bytes 30..=35.
    #[test]
    fn test_markdown_link_is_not_a_deletion() -> Result<(), ParserError> {
        let input = "C: See [the docs](https://x) for [answer]";
        let parser = make_test_parser();
        let cards = parser.parse(input)?;

        assert_cloze(&cards, "See [the docs](https://x) for answer", &[(30, 35)]);
        Ok(())
    }
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test test_markdown_link_is_not_a_deletion`
Expected: FAIL. With Task 2 in place, `parse` currently returns Ok with two deletions and mangled clean text (`the docs(https://x)` became a deletion), so `assert_cloze`'s length assertion fails.

- [x] **Step 3: Implement link skipping**

(a) Add a private helper above `impl Parser` in `src/parser.rs`:

```rust
/// Returns true if the unescaped `[` at byte position `open_pos` in `text`
/// opens a markdown link: its bracket group closes with `](`. Nested `[`
/// before the close means this is not a simple link.
fn is_markdown_link_open(text: &str, open_pos: usize) -> bool {
    let bytes = text.as_bytes();
    let mut i = open_pos + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'[' => return false,
            b']' => return bytes.get(i + 1) == Some(&b'('),
            _ => i += 1,
        }
    }
    false
}
```

(b) In the **clean-text loop** of `parse_cloze_cards` (the block building `clean_text`, currently starting at line 470): add `let mut link_mode = false; // [text](url)` next to the existing `let mut image_mode = false;` declaration, then replace the `[` and `]` branches (currently lines 482–501):

```rust
                if c == b'[' {
                    if image_mode || link_mode {
                        clean_text.push(c);
                    } else if escape_mode {
                        escape_mode = false;
                        clean_text.push(c);
                    } else if is_markdown_link_open(&text, bytepos) {
                        // This bracket opens a markdown link; keep it verbatim.
                        link_mode = true;
                        clean_text.push(c);
                    }
                } else if c == b']' {
                    if image_mode {
                        // We are in image mode, so this closing bracket is
                        // part of a Markdown image.
                        image_mode = false;
                        clean_text.push(c);
                    } else if link_mode {
                        // Closing bracket of a markdown link.
                        link_mode = false;
                        clean_text.push(c);
                    } else if escape_mode {
                        // We are in escape mode, so this closing bracket is
                        // part of the markdown text.
                        escape_mode = false;
                        clean_text.push(c);
                    }
                } else if c == b'!' {
```

(c) In the **position loop**: add `let mut link_mode = false;` next to its `let mut image_mode = false;` (currently line 549), then replace the `[` and `]` branches so they read (this incorporates Task 1's and Task 2's code — do not lose those guards):

```rust
            if c == b'[' {
                if image_mode || link_mode {
                    index += 1;
                } else if escape_mode {
                    index += 1;
                    escape_mode = false;
                } else if is_markdown_link_open(&text, bytepos) {
                    // This bracket opens a markdown link; it stays in the text.
                    link_mode = true;
                    index += 1;
                } else if start.is_some() {
                    return Err(ParserError::new(
                        "Nested cloze brackets.",
                        self.file_path.clone(),
                        start_line,
                    ));
                } else {
                    start = Some(index);
                }
            } else if c == b']' {
                if image_mode {
                    image_mode = false;
                    index += 1;
                } else if link_mode {
                    link_mode = false;
                    index += 1;
                } else if escape_mode {
                    escape_mode = false;
                    index += 1;
                } else if let Some(s) = start {
                    let end = index;
                    if end == s {
                        return Err(ParserError::new(
                            "Cloze deletion is empty.",
                            self.file_path.clone(),
                            start_line,
                        ));
                    }
                    let content = CardContent::new_cloze(clean_text.clone(), s, end - 1);
                    let card = Card::new(
                        self.deck_name.clone(),
                        self.file_path.clone(),
                        (start_line, end_line),
                        content,
                    );
                    cards.push(card);
                    start = None;
                }
            } else if c == b'!' {
```

Note: `text` here is the `&str` produced by `let text = text.trim();` at the top of `parse_cloze_cards`, so `is_markdown_link_open(&text, bytepos)` uses the same byte positions as the loop. (`&text` where `text: &str` coerces to `&str`; writing `text` alone also works.)

- [x] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all PASS, in particular `test_cloze_with_image`, `test_cloze_with_escaped_square_bracket`, `test_cloze_with_multiple_escaped_square_brackets`, and the Task 1/2 tests.

- [x] **Step 5: Document the escape rules in README.md**

In `README.md`, in the "Cloze Cards" section, insert after the multi-line cloze example's closing code fence (currently line 264, just before `### Separators`):

```markdown
Square brackets are reserved for cloze deletions inside `C:` cards. The
exact rules:

- `[text]` marks a cloze deletion. It must be non-empty and must close on
  the same line it opens.
- `\[` and `\]` produce literal square brackets.
- Image syntax (`![alt](path)`) is passed through to Markdown untouched.
- Link syntax (`[text](url)`) is passed through to Markdown untouched: a
  bracket group immediately followed by `(` is treated as a link, not a
  deletion.
- Nested brackets (`[[a]]`) and deletions left open at the end of a line
  are parse errors.
```

- [x] **Step 6: Update CHANGELOG.xml**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add after the Task 2 entry:

```xml
            <change author="claude">
                Markdown links (`[text](url)`) inside cloze cards are no longer misparsed as cloze deletions; they are passed through to Markdown like image syntax. The bracket escape rules are now documented in the README. (BUG-17)
            </change>
```

- [x] **Step 7: Commit**

```bash
git add src/parser.rs README.md CHANGELOG.xml
git commit -m "fix: skip markdown links inside cloze cards (BUG-17)"
```

---

### Task 4: BUG-18 — Card syntax inside fenced code blocks is plain text

**Files:**
- Modify: `src/parser.rs` (replace the stateless `Line::read` with a stateful `LineReader`; update `Parser::parse`; tests module)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: existing `Line` enum (`src/parser.rs:202-215`) and helper predicates `is_question`/`is_answer`/`is_cloze`/`is_separator` — all unchanged.
- Produces: `struct LineReader { fence: Option<FenceKind> }` with `fn new() -> Self` and `fn read(&mut self, line: &str) -> Line`; private `enum FenceKind { Backtick, Tilde }`. `impl Line { fn read }` is deleted. `Parser::parse` holds one `LineReader` for the whole file.

Background: `Line::read` (`src/parser.rs:218-230`) is a stateless prefix match, so `Q:`, `A:`, `C:`, and `---` inside ``` or ~~~ fences create cards or separators.

- [x] **Step 1: Write the failing regression tests**

Add to the `tests` module in `src/parser.rs`:

```rust
    /// BUG-18: `Q:`/`C:`/`---` lines inside a fenced code block are literal
    /// text, so an answer containing a fence round-trips as one card.
    #[test]
    fn test_card_syntax_inside_backtick_fence_is_text() -> Result<(), ParserError> {
        let input =
            "Q: What does the file look like?\nA: Like this:\n```\nQ: not a card\n---\nC: not [a] cloze\n```\nDone.";
        let parser = make_test_parser();
        let cards = parser.parse(input)?;

        assert_eq!(cards.len(), 1);
        assert!(matches!(
            &cards[0].content(),
            CardContent::Basic {
                question,
                answer,
            } if question == "What does the file look like?"
                && answer == "Like this:\n```\nQ: not a card\n---\nC: not [a] cloze\n```\nDone."
        ));
        Ok(())
    }

    /// BUG-18: tilde fences count too.
    #[test]
    fn test_card_syntax_inside_tilde_fence_is_text() -> Result<(), ParserError> {
        let input = "Q: q\nA: a\n~~~\nQ: not a card\n~~~";
        let parser = make_test_parser();
        let cards = parser.parse(input)?;

        assert_eq!(cards.len(), 1);
        assert!(matches!(
            &cards[0].content(),
            CardContent::Basic {
                question,
                answer,
            } if question == "q" && answer == "a\n~~~\nQ: not a card\n~~~"
        ));
        Ok(())
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test test_card_syntax_inside_backtick_fence_is_text test_card_syntax_inside_tilde_fence_is_text`
Expected: both FAIL — the `Q:` line inside the fence is read as a new question while an answer is open, producing extra cards (the first assertion on `cards.len()` fails, or `parse` errors on the in-fence `---`).

- [x] **Step 3: Implement the fence-tracking line reader**

In `src/parser.rs`, replace the whole `impl Line { fn read ... }` block (currently lines 217–231) with:

```rust
/// The kind of fenced code block currently open.
#[derive(PartialEq)]
enum FenceKind {
    /// ``` fences.
    Backtick,
    /// ~~~ fences.
    Tilde,
}

/// Classifies lines, tracking fenced-code-block state: while inside a
/// ``` or ~~~ fence, every line (including the closing fence) is `Text`,
/// so card syntax inside code blocks is never parsed.
struct LineReader {
    fence: Option<FenceKind>,
}

impl LineReader {
    fn new() -> Self {
        LineReader { fence: None }
    }

    fn read(&mut self, line: &str) -> Line {
        let trimmed = line.trim_start();
        match self.fence {
            Some(FenceKind::Backtick) => {
                if trimmed.starts_with("```") {
                    self.fence = None;
                }
                return Line::Text(line.to_string());
            }
            Some(FenceKind::Tilde) => {
                if trimmed.starts_with("~~~") {
                    self.fence = None;
                }
                return Line::Text(line.to_string());
            }
            None => {}
        }
        if trimmed.starts_with("```") {
            self.fence = Some(FenceKind::Backtick);
            return Line::Text(line.to_string());
        }
        if trimmed.starts_with("~~~") {
            self.fence = Some(FenceKind::Tilde);
            return Line::Text(line.to_string());
        }
        if is_question(line) {
            Line::StartQuestion(trim(line))
        } else if is_answer(line) {
            Line::StartAnswer(trim(line))
        } else if is_cloze(line) {
            Line::StartCloze(trim(line))
        } else if is_separator(line) {
            Line::Separator
        } else {
            Line::Text(line.to_string())
        }
    }
}
```

Then in `Parser::parse` (currently lines 262–271), replace the loop body:

```rust
        let mut cards = Vec::new();
        let mut state = State::Start;
        let mut reader = LineReader::new();
        let lines: Vec<&str> = text.lines().collect();
        let last_line = if lines.is_empty() { 0 } else { lines.len() - 1 };
        for (line_num, line) in lines.iter().enumerate() {
            let line = reader.read(line);
            state = self.parse_line(state, line, line_num, &mut cards)?;
        }
        self.parse_line(state, Line::Eof, last_line, &mut cards)?;
```

- [x] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all PASS.

- [x] **Step 5: Update CHANGELOG.xml**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add after the Task 3 entry:

```xml
            <change author="claude">
                `Q:`, `A:`, `C:`, and `---` lines inside fenced code blocks (``` or ~~~) are now treated as literal text instead of card syntax, so cards containing code samples parse correctly. (BUG-18)
            </change>
```

- [x] **Step 6: Commit**

```bash
git add src/parser.rs CHANGELOG.xml
git commit -m "fix: ignore card syntax inside fenced code blocks (BUG-18)"
```

---

### Task 5: BUG-20 — Line numbers account for stripped frontmatter

**Files:**
- Modify: `src/parser.rs` (`extract_frontmatter`, `strip_frontmatter`, `Parser` struct/`new`/`parse`, `parse_deck`; frontmatter tests and `make_test_parser`)
- Modify: `src/cmd/serve/edit.rs` (`edit_post_inner` re-parse, `extract_card_block`, `splice_card_block`)
- Modify: `src/media/validate.rs` (four test call sites of `CardParser::new` at lines 166, 203, 225, 246)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: `extract_frontmatter` (`src/parser.rs:42`), `Parser::new` (`src/parser.rs:254`).
- Produces:
  - `fn extract_frontmatter(text: &str) -> Fallible<(DeckMetadata, &str, usize)>` — third element is the 0-based file line where content starts (0 when there is no frontmatter).
  - `pub fn strip_frontmatter_with_offset(text: &str) -> Fallible<(&str, usize)>`; `pub fn strip_frontmatter(text: &str) -> Fallible<&str>` keeps its signature.
  - `pub fn Parser::new(deck_name: DeckName, file_path: PathBuf, line_offset: usize) -> Self`.
  - **Semantic change:** `Card::range()`, all `ParserError` line numbers, export `line_start`/`line_end`, and media-error `card_lines` become absolute 0-based file lines (displayed 1-based). Tasks 6–7 and later PRs rely on this.

Background: `parse_deck` strips frontmatter (`src/parser.rs:113`) and then parses the remainder (`:133-134`), so every reported line is off by the frontmatter length. `src/cmd/serve/edit.rs` currently compensates by also stripping frontmatter in `extract_card_block`/`splice_card_block` (`edit.rs:255`, `:272-274`); once ranges are absolute, those functions must index the full file instead, and the re-parse in `edit_post_inner` (`edit.rs:184-186`) must pass the same offset so its range comparison against collection cards still matches.

- [x] **Step 1: Write the failing regression tests**

Add to the `tests` module in `src/parser.rs` (uses the existing `use std::env::temp_dir;` and `use std::fs::create_dir_all;` imports):

```rust
    /// BUG-20: a parse error after `---` frontmatter reports the real file
    /// line, not the line within the stripped content.
    #[test]
    fn test_parse_error_after_frontmatter_reports_real_line() -> Fallible<()> {
        let directory = temp_dir().join("frontmatter_error_line_test");
        create_dir_all(&directory).expect("Failed to create test directory");
        let file = directory.join("deck.md");
        // "A: orphan" is on file line 5 (1-based).
        std::fs::write(
            &file,
            "---\nname = \"X\"\n---\n\nA: orphan answer\n",
        )
        .expect("Failed to write test file");

        let result = parse_deck(&directory);
        std::fs::remove_dir_all(&directory).ok();

        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(
            err.to_string().contains("deck.md:5"),
            "expected real file line 5 in: {err}"
        );
        Ok(())
    }

    /// BUG-20: card ranges are absolute file lines when frontmatter is present.
    #[test]
    fn test_card_range_accounts_for_frontmatter() -> Fallible<()> {
        let directory = temp_dir().join("frontmatter_range_test");
        create_dir_all(&directory).expect("Failed to create test directory");
        let file = directory.join("deck.md");
        // Q: is on 0-based file line 4, A: on line 5.
        std::fs::write(
            &file,
            "---\nname = \"X\"\n---\n\nQ: question\nA: answer\n",
        )
        .expect("Failed to write test file");

        let deck = parse_deck(&directory);
        std::fs::remove_dir_all(&directory).ok();

        let cards = deck?;
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].range(), (4, 5));
        Ok(())
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test test_parse_error_after_frontmatter_reports_real_line test_card_range_accounts_for_frontmatter`
Expected: both FAIL — the error reports `deck.md:2` (content-relative) and the range is `(1, 2)`.

- [x] **Step 3: Implement the offset**

(a) `extract_frontmatter` returns the content-start line. Change the signature and the two return points (`src/parser.rs:42`, `:48`, `:94`):

```rust
fn extract_frontmatter(text: &str) -> Fallible<(DeckMetadata, &str, usize)> {
```

The no-frontmatter early return becomes:

```rust
        _ => return Ok((DeckMetadata { name: None }, text, 0)),
```

The final return becomes (`content_start_line` already exists at line 74):

```rust
    Ok((metadata, content, content_start_line))
```

(b) Replace `strip_frontmatter` (`src/parser.rs:97-101`) with two functions:

```rust
/// Strip TOML frontmatter and return only the card content portion of a file.
pub fn strip_frontmatter(text: &str) -> Fallible<&str> {
    let (content, _) = strip_frontmatter_with_offset(text)?;
    Ok(content)
}

/// Like `strip_frontmatter`, but also return the 0-based file line at which
/// the content starts (0 when there is no frontmatter). Pass this as the
/// `line_offset` of `Parser::new` so parse errors and card ranges report
/// real file lines.
pub fn strip_frontmatter_with_offset(text: &str) -> Fallible<(&str, usize)> {
    let (_, content, offset) = extract_frontmatter(text)?;
    Ok((content, offset))
}
```

(c) `Parser` gains the offset. Struct (`src/parser.rs:149-152`) and constructor (`:254-259`):

```rust
pub struct Parser {
    deck_name: DeckName,
    file_path: PathBuf,
    /// The 0-based file line at which the parsed text begins. Non-zero when
    /// TOML frontmatter was stripped before parsing, so that all error
    /// locations and card ranges refer to real file lines.
    line_offset: usize,
}
```

```rust
    pub fn new(deck_name: DeckName, file_path: PathBuf, line_offset: usize) -> Self {
        Parser {
            deck_name,
            file_path,
            line_offset,
        }
    }
```

(d) In `Parser::parse` (as left by Task 4 Step 3), add the offset where line numbers enter `parse_line`:

```rust
        for (line_num, line) in lines.iter().enumerate() {
            let line = reader.read(line);
            state = self.parse_line(state, line, line_num + self.line_offset, &mut cards)?;
        }
        self.parse_line(state, Line::Eof, last_line + self.line_offset, &mut cards)?;
```

(e) In `parse_deck` (`src/parser.rs:113`, `:133`):

```rust
            let (metadata, content, line_offset) = extract_frontmatter(&text)?;
```

```rust
            let parser = Parser::new(deck_name, path.to_path_buf(), line_offset);
```

(f) Update call sites that construct a `Parser` directly:

- `src/parser.rs` tests, `make_test_parser`:

```rust
    fn make_test_parser() -> Parser {
        Parser::new("test_deck".to_string(), PathBuf::from("test.md"), 0)
    }
```

- `src/media/validate.rs` test call sites at lines 166, 203, 225, 246 — append `, 0`:

```rust
        let parser = CardParser::new("test_deck".to_string(), card_file.clone(), 0);
```

(g) Update the frontmatter tests in `src/parser.rs` that destructure the old 2-tuple. In `test_extract_frontmatter_with_name`, `test_extract_frontmatter_without_name`, `test_extract_frontmatter_empty`, `test_no_frontmatter`, and `test_parse_with_frontmatter`, change the destructuring and add an offset assertion:

```rust
        let (metadata, content, offset) = result.unwrap();
```

with, respectively, `assert_eq!(offset, 3);` (with_name), `assert_eq!(offset, 3);` (without_name), `assert_eq!(offset, 2);` (empty), `assert_eq!(offset, 0);` (no_frontmatter). In `test_parse_with_frontmatter` change line 1108 to:

```rust
        let (metadata, content, _offset) = extract_frontmatter(input).unwrap();
```

(h) Update `src/cmd/serve/edit.rs` for absolute ranges:

- In `edit_post_inner`, replace lines 184–186:

```rust
    let (after_fm, line_offset) = strip_frontmatter_with_offset(&new_file_content)?;
    let new_cards_result =
        Parser::new(card.deck_name().clone(), file_path.clone(), line_offset).parse(after_fm);
```

and change the import at line 24 from `use crate::parser::strip_frontmatter;` to `use crate::parser::strip_frontmatter_with_offset;`.

- Replace `extract_card_block` (lines 253–262) — ranges are now absolute, so index the full file:

```rust
/// Extract the raw markdown source block for a card (the lines the user edits).
/// `range` uses absolute 0-based file line numbers.
pub fn extract_card_block(file_content: &str, range: (usize, usize)) -> Fallible<String> {
    let lines: Vec<&str> = file_content.lines().collect();
    let start = range.0;
    let end = block_end(&lines, range);
    if start > lines.len() {
        return fail("Card range is out of bounds in source file");
    }
    Ok(lines[start..end].join("\n"))
}
```

- Replace `splice_card_block` (lines 265–297) — drop the frontmatter pointer arithmetic and splice the full file's lines (the atomic tmp-write/rename tail is unchanged):

```rust
/// Atomically replace a card's block in the source file with `new_text`.
/// `range` uses absolute 0-based file line numbers.
fn splice_card_block(
    file_path: &Path,
    file_content: &str,
    range: (usize, usize),
    new_text: &str,
) -> Fallible<()> {
    let ends_with_newline = file_content.ends_with('\n');
    let mut lines: Vec<&str> = file_content.lines().collect();
    let start = range.0;
    let end = block_end(&lines, range);
    let new_lines: Vec<&str> = new_text.lines().collect();
    lines.splice(start..end, new_lines.iter().copied());

    let mut result = lines.join("\n");
    if ends_with_newline {
        result.push('\n');
    }

    let dir = file_path.parent().unwrap_or(Path::new("."));
    let file_name = file_path
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("card"))
        .to_string_lossy();
    let tmp: PathBuf = dir.join(format!(".{file_name}.hashcards-edit.tmp"));
    std::fs::write(&tmp, &result)?;
    std::fs::rename(&tmp, file_path)?;
    Ok(())
}
```

- [x] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all PASS. The edit.rs tests (`test_extract_card_block_no_fm`, `test_splice_basic`, `test_splice_eof`, ...) use frontmatter-free content, so full-file indexing keeps them green. Also run `cargo clippy --all-targets` and fix any leftover-import warnings (e.g. an unused `strip_frontmatter` import).

- [x] **Step 5: Add an edit.rs regression test for frontmatter files**

Add to the `tests` module in `src/cmd/serve/edit.rs` (absolute ranges must splice the right lines in a file *with* frontmatter):

```rust
    #[test]
    fn test_splice_with_frontmatter_absolute_range() {
        let dir = std::env::temp_dir().join("hashcards_edit_test_fm");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.md");
        // Q: is on absolute 0-based line 3, A: on line 4.
        let original = "---\nname = \"X\"\n---\nQ: foo\nA: bar\n";
        std::fs::write(&path, original).unwrap();

        splice_card_block(&path, original, (3, 4), "Q: foo edited\nA: bar edited").unwrap();
        let result = std::fs::read_to_string(&path).unwrap();
        assert_eq!(result, "---\nname = \"X\"\n---\nQ: foo edited\nA: bar edited\n");

        std::fs::remove_dir_all(&dir).ok();
    }
```

Run: `cargo test test_splice_with_frontmatter_absolute_range`
Expected: PASS.

- [x] **Step 6: Update CHANGELOG.xml**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add after the Task 4 entry:

```xml
            <change author="claude">
                Line numbers in parse errors, media errors, and the JSON export are no longer offset by the length of a file's TOML frontmatter; they now refer to real file lines. (BUG-20)
            </change>
```

- [x] **Step 7: Commit**

```bash
git add src/parser.rs src/cmd/serve/edit.rs src/media/validate.rs CHANGELOG.xml
git commit -m "fix: report real file lines when frontmatter is present (BUG-20)"
```

---

### Task 6: BUG-21 — Errors carry file paths and human-readable messages

**Files:**
- Modify: `src/error.rs` (six `{value:#?}` `From` impls at lines 39, 47, 55, 63, 87, 95; `From<ParserError>` at lines 100–106; new `message()` accessor)
- Modify: `src/parser.rs` (`parse_deck`: wrap the `read_to_string` and `extract_frontmatter` errors with the file path)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: Task 5's `parse_deck` shape (`extract_frontmatter` returning a 3-tuple).
- Produces: `pub fn ErrorReport::message(&self) -> &str`. `ErrorReport::from(ParserError)` no longer prepends `"Parse error: "` (the `ParserError` `Display` already carries message + location, and `ErrorReport`'s `Display` adds the single `"error: "` prefix). All `From` impls use `Display`, not `{:#?}` debug formatting.

- [x] **Step 1: Write the failing regression tests**

`src/error.rs` has no tests module; add one at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::path::PathBuf;

    use super::*;

    /// BUG-21: I/O errors must render as human-readable messages, not
    /// multi-line `{:#?}` debug spew.
    #[test]
    fn test_io_error_is_human_readable() {
        let io_err = std::io::Error::new(ErrorKind::NotFound, "file missing");
        let report = ErrorReport::from(io_err);
        assert_eq!(report.to_string(), "error: I/O error: file missing");
    }

    /// BUG-21: converting a ParserError must not stack a redundant
    /// "Parse error:" prefix onto the "error:" prefix.
    #[test]
    fn test_parser_error_has_no_double_prefix() {
        let parser_err = ParserError {
            message: "Cloze deletion is empty.".to_string(),
            file_path: PathBuf::from("deck.md"),
            line_num: 4,
        };
        let report = ErrorReport::from(parser_err);
        assert_eq!(
            report.to_string(),
            "error: Cloze deletion is empty. Location: deck.md:5"
        );
    }
}
```

And add to the `tests` module in `src/parser.rs`:

```rust
    /// BUG-21: frontmatter errors name the file they came from.
    #[test]
    fn test_frontmatter_error_carries_file_path() -> Fallible<()> {
        let directory = temp_dir().join("frontmatter_path_test");
        create_dir_all(&directory).expect("Failed to create test directory");
        let file = directory.join("broken.md");
        std::fs::write(&file, "---\nname = \"X\"\n").expect("Failed to write test file");

        let result = parse_deck(&directory);
        std::fs::remove_dir_all(&directory).ok();

        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(
            err.to_string().contains("broken.md"),
            "expected file path in: {err}"
        );
        assert!(err.to_string().contains("no closing '---'"));
        Ok(())
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test test_io_error_is_human_readable test_parser_error_has_no_double_prefix test_frontmatter_error_carries_file_path`
Expected: all three FAIL — the I/O message contains debug formatting (`Custom {` / `kind:`), the parser report reads `error: Parse error: ...`, and the frontmatter error has no path.

- [x] **Step 3: Implement**

(a) In `src/error.rs`, add an accessor to `impl ErrorReport` (after `new`):

```rust
    /// The message without the "error: " display prefix.
    pub fn message(&self) -> &str {
        &self.message
    }
```

(b) Replace the six debug-formatted messages, keeping each `From` impl's shape:

- `From<std::io::Error>` (line 39): `message: format!("I/O error: {value}"),`
- `From<StripPrefixError>` (line 47): `message: format!("Path prefix error: {value}"),`
- `From<walkdir::Error>` (line 55): `message: format!("Directory traversal error: {value}"),`
- `From<rusqlite::Error>` (line 63): `message: format!("Database error: {value}"),`
- `From<FromUtf8Error>` (line 87): `message: format!("UTF-8 conversion error: {value}"),`
- `From<serde_json::Error>` (line 95): `message: format!("JSON error: {value}"),`

(c) Replace `From<ParserError>` (lines 100–106):

```rust
impl From<ParserError> for ErrorReport {
    fn from(value: ParserError) -> Self {
        ErrorReport {
            message: value.to_string(),
        }
    }
}
```

(d) In `src/parser.rs` `parse_deck`, attach the path to I/O and frontmatter errors (replacing the current lines 110 and the Task 5 version of line 113):

```rust
            let text = read_to_string(path).map_err(|e| {
                ErrorReport::new(format!("Failed to read {}: {e}", path.display()))
            })?;

            // Extract frontmatter and get custom deck name if specified
            let (metadata, content, line_offset) = extract_frontmatter(&text).map_err(|e| {
                ErrorReport::new(format!("{} File: {}", e.message(), path.display()))
            })?;
```

- [x] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all PASS (the existing `test_frontmatter_unclosed` asserts only on `contains("no closing '---'")`, which still holds).

- [x] **Step 5: Update CHANGELOG.xml**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add after the Task 5 entry:

```xml
            <change author="claude">
                Frontmatter and file-read errors now name the file they came from; I/O, database, path, UTF-8, and JSON errors are rendered as human-readable messages instead of debug dumps; and parse errors no longer carry a redundant "Parse error:" prefix. (BUG-21)
            </change>
```

- [x] **Step 6: Commit**

```bash
git add src/error.rs src/parser.rs CHANGELOG.xml
git commit -m "fix: human-readable errors with file paths (BUG-21)"
```

---

### Task 7: BUG-26 — Cloze positions documented as bytes, non-ASCII round-trip test

**Files:**
- Modify: `src/types/card.rs` (doc comments on `CardContent::Cloze` at lines 56–59; tests module)
- Modify: `src/cmd/export.rs` (doc comments on `CardContentExport::Cloze` fields at lines 79–83)
- Modify: `src/parser.rs` (tests module)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: `CardContent::new_cloze(text, start, end)` (`src/types/card.rs:147`), `Card::html_front`/`html_back` (`:130-136`), `MarkdownRenderConfig` (`src/markdown.rs:50`), `MediaResolverBuilder` (`src/media/resolve.rs`), `crate::helper::create_tmp_directory`.
- Produces: documentation only, plus tests. No code behavior changes.

Byte-position arithmetic for the test (`.bytes()`, never `.chars()`): in `"Größe: 10 µm"`, the prefix `Größe: ` is 9 bytes (`G`=1, `r`=1, `ö`=2, `ß`=2, `e`=1, `:`=1, space=1) and the deletion `10 µm` is 6 bytes (`1`,`0`,space each 1, `µ`=2, `m`=1), so the deletion spans bytes 9..=14.

- [x] **Step 1: Write the failing-by-absence regression tests**

(a) Add to the `tests` module in `src/parser.rs`:

```rust
    /// BUG-26: cloze positions are byte offsets. Multi-byte characters before
    /// and inside the deletion must yield byte positions, not char positions.
    /// "Größe: " is 9 bytes; "10 µm" is 6 bytes (µ is 2 bytes).
    #[test]
    fn test_non_ascii_cloze_positions_are_bytes() -> Result<(), ParserError> {
        let input = "C: Größe: [10 µm]";
        let parser = make_test_parser();
        let cards = parser.parse(input)?;

        assert_cloze(&cards, "Größe: 10 µm", &[(9, 14)]);
        Ok(())
    }
```

(b) Add to the `tests` module in `src/types/card.rs` (a full round trip through the byte-splicing render paths):

```rust
    use std::fs::write;
    use std::path::PathBuf;

    use crate::helper::create_tmp_directory;
    use crate::media::resolve::MediaResolverBuilder;

    fn make_test_render_config() -> Fallible<MarkdownRenderConfig> {
        let coll_path: PathBuf = create_tmp_directory()?;
        let abs_deck_path: PathBuf = coll_path.join("deck.md");
        write(&abs_deck_path, "")?;
        let config = MarkdownRenderConfig {
            resolver: MediaResolverBuilder::new()
                .with_collection_path(coll_path)?
                .with_deck_path(PathBuf::from("deck.md"))?
                .build()?,
            file_url_prefix: "http://localhost:1234/file".to_string(),
        };
        Ok(config)
    }

    /// BUG-26: rendering a cloze card with non-ASCII text before and inside
    /// the deletion round-trips through the byte-position splice without
    /// mangling multi-byte characters.
    #[test]
    fn test_non_ascii_cloze_render_round_trip() -> Fallible<()> {
        let config = make_test_render_config()?;
        // "Größe: " = 9 bytes; deletion "10 µm" = bytes 9..=14.
        let content = CardContent::new_cloze("Größe: 10 µm", 9, 14);

        let front = content.html_front(&config)?.into_string();
        assert!(front.contains("Größe:"), "front was: {front}");
        assert!(front.contains("<span class='cloze'>"), "front was: {front}");
        assert!(!front.contains("10 µm"), "deletion leaked into front: {front}");

        let back = content.html_back(&config)?.into_string();
        assert!(
            back.contains("<span class='cloze-reveal'>10 µm</span>"),
            "back was: {back}"
        );
        assert!(back.contains("Größe:"), "back was: {back}");
        Ok(())
    }
```

- [x] **Step 2: Run tests to verify they pass (docs bug, not code bug)**

Run: `cargo test test_non_ascii_cloze_positions_are_bytes test_non_ascii_cloze_render_round_trip`
Expected: both PASS — the code already works in bytes; the bug is the documentation. These tests pin the behavior so a future "fix" toward char positions fails loudly. If either FAILS, stop: that is a real positions bug — investigate before touching docs.

- [x] **Step 3: Correct the docs**

(a) In `src/types/card.rs`, replace the field docs on `CardContent::Cloze` (lines 54–59):

```rust
    Cloze {
        /// The text of the card without brackets.
        text: String,
        /// The byte position (not character position) of the first byte of
        /// the deletion within `text`.
        start: usize,
        /// The byte position (not character position) of the last byte of
        /// the deletion within `text`, inclusive.
        end: usize,
    },
```

(b) In `src/cmd/export.rs`, document the exported cloze fields (lines 79–83) so JSON consumers know these are byte offsets:

```rust
    Cloze {
        /// The text of the card without cloze brackets.
        text: String,
        /// Byte offset (not character offset) of the first byte of the
        /// deletion within `text`.
        start: usize,
        /// Byte offset (not character offset) of the last byte of the
        /// deletion within `text`, inclusive.
        end: usize,
    },
```

- [x] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all PASS.

- [x] **Step 5: Update CHANGELOG.xml**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add after the Task 6 entry:

```xml
            <change author="claude">
                Cloze deletion positions are documented as byte offsets (they were previously described as character positions, including in the JSON export), and a non-ASCII cloze round-trip test now pins this behavior. (BUG-26)
            </change>
```

- [x] **Step 6: Commit**

```bash
git add src/types/card.rs src/cmd/export.rs src/parser.rs CHANGELOG.xml
git commit -m "docs: cloze positions are byte offsets; add non-ASCII round-trip test (BUG-26)"
```

---

## Final verification (after all tasks)

- [x] Run `cargo test` — everything green. (171 passed)
- [x] Run `cargo clippy --all-targets` — no new warnings.
- [x] Run `xmllint --schema CHANGELOG.xsd CHANGELOG.xml --noout` — validates.

## Spec discrepancies

Found while verifying the spec's citations against the code at `be55f80`:

1. **BUG-21: all `src/error.rs` line numbers are stale.** The spec cites `error.rs:119-125`, `:122`, `:130`, `:138`, `:146`, `:170`, `:178`, `:183-189`, `:201`; the file is 134 lines long. The actual `{value:#?}` uses are at lines 39 (`io::Error`), 47 (`StripPrefixError`), 55 (`walkdir::Error`), 63 (`rusqlite::Error`), 87 (`FromUtf8Error`), 95 (`serde_json::Error`) — six impls, not the spec's implied set (the `reqwest`, `toml::ser`, and `toml::de` impls already use `Display`). `From<ParserError>` is at 100–106 and the `"error: "` prefix at 118.
2. **BUG-21: the "double 'error: Parse error:' prefix" is really one of each.** `ErrorReport::Display` (error.rs:118) prepends `"error: "` once and `From<ParserError>` (error.rs:103) prepends `"Parse error: "` once, yielding `error: Parse error: <msg> Location: ...`. The plan drops the `"Parse error: "` component, as the `ParserError` display already carries message + location.
3. **BUG-26: `export.rs:80-83` has no "character" docs to correct.** `CardContentExport::Cloze`'s `start`/`end` fields are entirely undocumented; the "character" wording exists only in `src/types/card.rs:56-58`. The plan *adds* byte-offset docs to the export struct rather than correcting existing ones.
4. **BUG-19 makes newline-spanning deletions an error.** The spec's "open deletion at EOL" rule outlaws `[foo\nbar]`, which the current parser accepts silently (producing a deletion spanning the newline). No existing test or documented behavior relies on cross-line deletions, and the README (updated in Task 3) documents the new rule — but this is a small behavior change beyond pure error reporting.
5. **BUG-20 ripples into `src/cmd/serve/edit.rs`, which the spec does not mention.** `extract_card_block` (edit.rs:253-262) and `splice_card_block` (edit.rs:265-297) interpret `Card::range()` as post-frontmatter line indices (they call `strip_frontmatter` themselves), and `edit_post_inner` (edit.rs:184-186) compares ranges from its own offset-less re-parse against collection cards. Making ranges absolute without updating edit.rs would splice web edits at the wrong lines in files with frontmatter. Task 5 updates all three sites.
6. **Minor citation drift:** the misleading no-deletion error the spec places at `parser.rs:619-623` is at 618–623; the frontmatter locations `parser.rs:65`, `:70`, `:110`, `:113`, `:133-134` and the scanner ranges `:481-534`, `:551-616`, `:574`, `:561`, `:218-230`, `:245-247` all check out exactly.
7. **BUG-17 link-vs-nested interaction is under-specified.** The spec wants both "skip `[text](url)`" (BUG-17) and "error on `[` while a deletion is open" (BUG-19). The plan resolves the overlap in favor of links: a bracket group closing with `](` is skipped even inside an open deletion (mirroring how images are skipped), so `C: [see [docs](x) here]` parses; a nested plain bracket still errors.
