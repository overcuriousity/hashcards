# Rendering Safety (PR group 4: BUG-22, BUG-23, BUG-24, BUG-25) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make card/page rendering safe against sentinel-string collisions (BUG-22), HTML attribute injection (BUG-23), unsafe `href` schemes (BUG-24), and KaTeX load failures leaving cards invisible (BUG-25).

**Architecture:** All four fixes are local to the rendering pipeline: cloze HTML assembly in `src/types/card.rs`, markdown-to-HTML event rewriting in `src/markdown.rs`, two maud templates in `src/cmd/serve/` plus the HedgeDoc URL storage path in `src/cmd/serve/handlers.rs`, and the client-side boot script `src/cmd/drill/script.js`. One tiny new module (`src/cmd/serve/href.rs`) holds the render-time href scheme guard shared by two templates.

**Tech Stack:** Rust (edition 2024), cargo, maud 0.27, pulldown-cmark 0.13, reqwest `Url` for URL parsing, plain JavaScript (no JS test infra exists — no `package.json` anywhere in the repo — so the JS fix uses a documented manual verification).

**Spec:** SPEC.md

## Global Constraints

Copied verbatim from SPEC.md "Global requirements" (apply to every item; from `CLAUDE.md`):

- Every bugfix starts with a failing regression test.
- No `unwrap()` in production code; use `Fallible`, `?`, and `fail()`.
- All error messages are user-facing and clear.
- Reviews and performance are written in one transaction; undo voids, never deletes.
- Cloze positions are byte positions: `.bytes()`, never `.chars()`.
- Update `CHANGELOG.xml` per item.

Additional project rules that bind every task below:

- Prefer imports (`use` statements) over fully qualified paths.
- `unwrap()` is allowed in tests, never in production code.
- Keep functions small and focused.

---

## Design decision for BUG-22 (read this before Task 1)

The current pipeline splices the fixed byte string `CLOZE_DELETION` into the card's markdown at the deletion's byte range, renders the markdown, then does `text.replace(CLOZE_TAG, ...)` on the resulting HTML (`src/types/card.rs:197-201` front, `:222-228` back). Any card whose text literally contains `CLOZE_DELETION` gets that text mangled into a cloze span.

The two candidate fixes were:

1. **Event/byte-level splicing before rendering** — map the deletion's byte range onto pulldown-cmark events and inject the span as `Event::Html`. Rejected: a cloze deletion can span multiple inline elements (e.g. `[a **b** c]`), so the range does not align with event boundaries; this requires byte-offset tracking through the parser (`Parser::into_offset_iter`) and splitting text events mid-token — a large refactor of a working pipeline.
2. **Unforgeable per-render marker** — keep the existing splice pipeline, but generate the marker per render and *check it does not occur in the card text*, retrying with a new suffix until it doesn't. Chosen: collision-free **by construction** (not merely improbable — the containment check makes forgery impossible regardless of card content), deterministic, zero new dependencies, and a minimal diff. The marker (`HASHCARDS-CLOZE-<n>`, ASCII letters/digits/hyphens) passes through pulldown-cmark as plain text exactly as the old sentinel did.

The splice itself continues to operate on **byte** offsets (`marker.bytes()`, never `.chars()`), per the cloze-positions rule.

---

### Task 1: BUG-22 — literal `CLOZE_DELETION` in card text gets mangled

**Files:**
- Modify: `src/types/card.rs` (constants at `:30-31`, `html_front` cloze arm at `:195-205`, `html_back` cloze arm at `:217-232`, tests mod at `:239`)

**Interfaces:**
- Consumes: `markdown_to_html` / `markdown_to_html_inline` from `src/markdown.rs` (unchanged signatures: `fn(&MarkdownRenderConfig, &str) -> Fallible<String>`).
- Produces: private `fn cloze_marker(text: &str) -> String` in `src/types/card.rs`; `CardContent::html_front` / `html_back` signatures unchanged. No other task depends on this one.

- [x] **Step 1: Write the failing regression test**

Add to the existing `mod tests` in `src/types/card.rs` (it starts at line 239). Also add this config helper inside `mod tests` (mirrors `make_test_config` in `src/markdown.rs`):

```rust
    use crate::helper::create_tmp_directory;
    use crate::media::resolve::MediaResolverBuilder;

    fn make_render_config() -> Fallible<MarkdownRenderConfig> {
        let coll_path: PathBuf = create_tmp_directory()?;
        let deck_path: PathBuf = coll_path.join("deck.md");
        std::fs::write(&deck_path, "")?;
        Ok(MarkdownRenderConfig {
            resolver: MediaResolverBuilder::new()
                .with_collection_path(coll_path)?
                .with_deck_path(PathBuf::from("deck.md"))?
                .build()?,
            file_url_prefix: "http://localhost:1234/file".to_string(),
        })
    }

    #[test]
    fn test_literal_cloze_sentinel_in_card_text_survives_rendering() -> Fallible<()> {
        // Regression test for BUG-22: a card containing the literal string
        // `CLOZE_DELETION` must render that text verbatim, with exactly one
        // cloze span (for the actual deletion).
        let text = "The string CLOZE_DELETION marks Paris in the code";
        let start = text.find("Paris").unwrap();
        let end = start + "Paris".len() - 1;
        let content = CardContent::new_cloze(text, start, end);
        let config = make_render_config()?;

        let front = content.html_front(&config)?.into_string();
        assert!(
            front.contains("CLOZE_DELETION"),
            "literal sentinel text must survive front rendering: {front}"
        );
        assert_eq!(
            front.matches("<span class='cloze'>").count(),
            1,
            "exactly one cloze blank expected: {front}"
        );

        let back = content.html_back(&config)?.into_string();
        assert!(
            back.contains("CLOZE_DELETION"),
            "literal sentinel text must survive back rendering: {back}"
        );
        assert!(
            back.contains("<span class='cloze-reveal'>Paris</span>"),
            "deletion must be revealed on the back: {back}"
        );
        assert_eq!(
            back.matches("<span class='cloze-reveal'>").count(),
            1,
            "exactly one reveal span expected: {back}"
        );
        Ok(())
    }
```

Note: `Fallible` and `MarkdownRenderConfig` are already imported at the top of `card.rs`; `PathBuf` likewise. `use super::*;` in the tests mod pulls them in.

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test test_literal_cloze_sentinel_in_card_text_survives_rendering`
Expected: FAIL — the front asserts fail because `CLOZE_DELETION` in the card text is replaced by a cloze span (two spans, literal text gone).

- [x] **Step 3: Implement the checked per-render marker**

In `src/types/card.rs`, delete the two constants at lines 30-31:

```rust
const CLOZE_TAG_BYTES: &[u8] = b"CLOZE_DELETION";
const CLOZE_TAG: &str = "CLOZE_DELETION";
```

Add this free function (below the `use` block, above `pub struct Card`):

```rust
/// Return a marker string that is guaranteed not to occur in `text`.
///
/// Used to stand in for a cloze deletion while the card's markdown is
/// rendered to HTML; the containment check makes it impossible for card
/// text to forge the marker (BUG-22). The marker is plain ASCII letters,
/// digits, and hyphens, so pulldown-cmark passes it through unchanged.
fn cloze_marker(text: &str) -> String {
    let mut n: u64 = 0;
    loop {
        let marker = format!("HASHCARDS-CLOZE-{n}");
        if !text.contains(&marker) {
            return marker;
        }
        n += 1;
    }
}
```

Replace the `html_front` cloze arm (lines 195-205) with:

```rust
            CardContent::Cloze { text, start, end } => {
                let marker = cloze_marker(text);
                let mut text_bytes: Vec<u8> = text.as_bytes().to_owned();
                text_bytes.splice(*start..*end + 1, marker.bytes());
                let text: String = String::from_utf8(text_bytes)?;
                let text: String = markdown_to_html(config, &text)?;
                let text: String =
                    text.replace(&marker, "<span class='cloze'>.............</span>");
                html! {
                    (PreEscaped(text))
                }
            }
```

Replace the `html_back` cloze arm (lines 217-232) with:

```rust
            CardContent::Cloze { text, start, end } => {
                let marker = cloze_marker(text);
                let mut text_bytes: Vec<u8> = text.as_bytes().to_owned();
                let deleted_text: Vec<u8> = text_bytes[*start..*end + 1].to_owned();
                let deleted_text: String = String::from_utf8(deleted_text)?;
                let deleted_text: String = markdown_to_html_inline(config, &deleted_text)?;
                text_bytes.splice(*start..*end + 1, marker.bytes());
                let text: String = String::from_utf8(text_bytes)?;
                let text = markdown_to_html(config, &text)?;
                let text = text.replace(
                    &marker,
                    &format!("<span class='cloze-reveal'>{}</span>", deleted_text),
                );
                html! {
                    (PreEscaped(text))
                }
            }
```

Note `marker.bytes()` — byte iterator, consistent with the byte-position rule. The splice ranges are untouched.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib` (the new test plus all existing card/markdown/parser tests must pass — the marker change must not break normal cloze rendering).
Expected: PASS, zero failures.

- [x] **Step 5: Update CHANGELOG.xml**

`CHANGELOG.xml` has an `<unreleased>` element containing `<fixed>` and `<added>` sections (see `CHANGELOG.xsd`: `<change>` elements with an `author` attribute, plain-text content, XML-escaped). Append inside the existing `<unreleased><fixed>` section:

```xml
            <change author="claude">
                Cloze cards whose text literally contains the string `CLOZE_DELETION` no longer render that text as a spurious cloze blank. The internal placeholder is now generated per render and checked against the card text.
            </change>
```

- [x] **Step 6: Commit**

```bash
git add src/types/card.rs CHANGELOG.xml
git commit -m "fix: stop mangling literal CLOZE_DELETION text in cloze cards (BUG-22)"
```

---

### Task 2: BUG-23 — unescaped `title` in audio element (HTML injection)

**Files:**
- Modify: `src/markdown.rs` (audio element at `:73-81`, `make_test_config` helper at `:159-175`, tests mod)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: private `fn escape_attribute(s: &str) -> String` in `src/markdown.rs`. No other task depends on it.

- [x] **Step 1: Write the failing regression test**

In `src/markdown.rs` tests mod, first make the shared helper create an audio file. In `make_test_config` (currently at lines 159-175), after the `image_path` lines add:

```rust
        let audio_path: PathBuf = coll_path.join("audio.mp3");
        std::fs::write(&audio_path, "")?;
```

Then add the test:

```rust
    #[test]
    fn test_audio_title_is_attribute_escaped() -> Fallible<()> {
        // Regression test for BUG-23: the markdown image title is interpolated
        // into an HTML attribute; `"` and `<` must be escaped.
        let markdown = r#"![alt](@/audio.mp3 "a \" <b> title")"#;
        let config = make_test_config()?;
        let html = markdown_to_html(&config, markdown)?;
        assert!(
            html.contains(r#"title="a &quot; &lt;b&gt; title""#),
            "title must be attribute-escaped: {html}"
        );
        assert!(
            !html.contains(r#"<b> title"#),
            "raw markup from the title must not appear in output: {html}"
        );
        Ok(())
    }
```

(pulldown-cmark parses the backslash-escaped quote, handing the rewriter the raw title `a " <b> title`.)

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test test_audio_title_is_attribute_escaped`
Expected: FAIL — output contains `title="a " <b> title"` (raw quote and angle brackets, broken attribute).

- [x] **Step 3: Implement attribute escaping**

In `src/markdown.rs`, add below `is_audio_file`:

```rust
/// Escape a string for interpolation into a double-quoted HTML attribute.
fn escape_attribute(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
```

(`&` must be replaced first so already-escaped entities are not double-mangled the wrong way round.)

Change the audio element construction (lines 74-81) to escape the title:

```rust
                    Event::Html(CowStr::Boxed(
                        format!(
                            r#"<audio controls src="{}" title="{}"></audio>"#,
                            url,
                            escape_attribute(&title)
                        )
                        .into_boxed_str(),
                    ))
```

(`url` needs no escaping here: `modify_url` produces either a percent-encoded path or a re-serialized http(s) `Url`, neither of which can contain `"` or `<`.)

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS, including all pre-existing markdown tests.

- [x] **Step 5: Update CHANGELOG.xml**

Add a `<security>` section inside `<unreleased>` (the XSD's `changesType` is an unbounded choice of `added`/`fixed`/`changed`/`removed`/`deprecated`/`security`/`breaking`, so a new `<security>` sibling next to `<fixed>` is valid). Append inside `<unreleased>`:

```xml
        <security>
            <change author="claude">
                Titles of audio media references are now HTML-escaped before being embedded in the audio element, preventing HTML injection through a crafted markdown title.
            </change>
        </security>
```

(If Task 4 has already created the `<security>` section, append the `<change>` into it instead of adding a second section.)

- [x] **Step 6: Commit**

```bash
git add src/markdown.rs CHANGELOG.xml
git commit -m "fix: escape audio element title attribute (BUG-23)"
```

---

### Task 3: BUG-24 (storage half) — validate HedgeDoc URL scheme at storage time

**Files:**
- Modify: `src/cmd/serve/hedgedoc.rs` (near `validate_hedgedoc_url` at `:57-64`; tests mod at `:547`)
- Modify: `src/cmd/serve/handlers.rs` (`hedgedoc_add_handler` URL normalization block at `:527-545`; hedgedoc imports at `:33-40`)

**Interfaces:**
- Consumes: existing private `fn validate_hedgedoc_url(url: &str) -> Fallible<()>` (`hedgedoc.rs:57`), `fail` and `Fallible` from `crate::error`.
- Produces: `pub fn normalize_hedgedoc_url(raw: &str) -> Fallible<String>` in `src/cmd/serve/hedgedoc.rs` — consumed by `hedgedoc_add_handler` in this task. Task 4 does not depend on it (render guard is independent, by design).

Background: `hedgedoc_add_handler` (`handlers.rs:527-545`) currently falls back to `Err(_) => trimmed.to_string()` when `Url::parse` fails, persisting a raw arbitrary string that later renders into `href` attributes. Scheme validation exists only at fetch time (`fetch_markdown`, `hedgedoc.rs:121`) and is HTTPS-only.

- [x] **Step 1: Write the failing regression tests**

Append to the existing `mod tests` in `src/cmd/serve/hedgedoc.rs` (starts at line 547):

```rust
    #[test]
    fn normalize_hedgedoc_url_rejects_javascript_scheme() {
        // Regression test for BUG-24: unsafe schemes must be rejected at
        // storage time, never persisted.
        assert!(normalize_hedgedoc_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn normalize_hedgedoc_url_rejects_unparseable_input() {
        // Previously an unparseable string was stored raw (handlers.rs
        // fell back to `trimmed.to_string()` on parse failure).
        assert!(normalize_hedgedoc_url("not a url at all").is_err());
        assert!(normalize_hedgedoc_url("").is_err());
    }

    #[test]
    fn normalize_hedgedoc_url_rejects_plain_http() {
        // Matches the existing fetch-time policy (validate_hedgedoc_url).
        assert!(normalize_hedgedoc_url("http://notes.example.com/abc").is_err());
    }

    #[test]
    fn normalize_hedgedoc_url_normalizes_https_urls() -> Fallible<()> {
        // Query, fragment, and trailing slash are stripped so equivalent
        // URLs dedupe to the same entry (preserves current handler behavior).
        assert_eq!(
            normalize_hedgedoc_url("  https://notes.example.com/abc/?x=1#frag ")?,
            "https://notes.example.com/abc"
        );
        Ok(())
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test normalize_hedgedoc_url`
Expected: FAIL to compile — `normalize_hedgedoc_url` does not exist yet. (A compile error in the test-only code is the failing state for a not-yet-written function.)

- [x] **Step 3: Implement `normalize_hedgedoc_url`**

In `src/cmd/serve/hedgedoc.rs`, directly below `validate_hedgedoc_url` (line 64), add:

```rust
/// Normalize and validate a user-supplied HedgeDoc note URL at storage time.
///
/// Strips query, fragment, and trailing slash so equivalent URLs map to the
/// same entry; rejects anything that is not a well-formed HTTPS URL, so an
/// unvalidated string can never be persisted or rendered into a link
/// (BUG-24).
pub fn normalize_hedgedoc_url(raw: &str) -> Fallible<String> {
    let trimmed = raw.trim();
    let mut parsed = reqwest::Url::parse(trimmed)
        .map_err(|e| ErrorReport::new(format!("Invalid HedgeDoc URL `{trimmed}`: {e}")))?;
    validate_hedgedoc_url(trimmed)?;
    parsed.set_query(None);
    parsed.set_fragment(None);
    if let Ok(mut segments) = parsed.path_segments_mut() {
        segments.pop_if_empty();
    }
    Ok(parsed.to_string())
}
```

If `ErrorReport` is not yet imported in `hedgedoc.rs`, add `use crate::error::ErrorReport;` to the top of the file (the file already imports `Fallible` and `fail`; prefer imports over `crate::error::ErrorReport::new(...)` qualified calls).

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test normalize_hedgedoc_url`
Expected: PASS (all four tests).

- [x] **Step 5: Wire the handler to use it**

In `src/cmd/serve/handlers.rs`, add to the hedgedoc import block (lines 33-40):

```rust
use crate::cmd::serve::hedgedoc::normalize_hedgedoc_url;
```

Replace the entire `let url = { ... };` normalization block in `hedgedoc_add_handler` (lines 527-545, from `// Normalize URL:` through the closing `};`) with:

```rust
    // Normalize and validate the URL at storage time (BUG-24): strip
    // query/fragment/trailing slash so equivalent URLs dedupe, and reject
    // anything that is not a well-formed HTTPS URL so no raw string is
    // ever persisted or rendered into an href.
    let url = match normalize_hedgedoc_url(&form.url) {
        Ok(url) => url,
        Err(e) => {
            log::error!("Rejected HedgeDoc URL: {e}");
            return Redirect::to("/hedgedoc");
        }
    };
```

(The old `if trimmed.is_empty()` early return is subsumed: an empty string fails `Url::parse`. The log-and-redirect error style matches every other failure branch in this handler; routing these through flash messages is FEAT-01/BUG-03, PR group 2.)

Also update the now-accurate comment at `handlers.rs:96-99` if needed — after this task, "All URLs were already validated as HTTPS when added" becomes true for newly added URLs (it was previously aspirational; fetch-time validation happened, storage-time did not).

- [x] **Step 6: Run the full test suite**

Run: `cargo test`
Expected: PASS — in particular the serve integration tests in `src/cmd/serve/mod.rs` must still pass.

- [x] **Step 7: Commit**

```bash
git add src/cmd/serve/hedgedoc.rs src/cmd/serve/handlers.rs
git commit -m "fix: validate HedgeDoc URL scheme at storage time (BUG-24)"
```

(CHANGELOG entry for BUG-24 is written once, in Task 4 Step 7, covering both halves.)

---

### Task 4: BUG-24 (render half) — assert href scheme at render time

**Files:**
- Create: `src/cmd/serve/href.rs`
- Modify: `src/cmd/serve/mod.rs` (module list at `:1-11`)
- Modify: `src/cmd/serve/hedgedoc_ui.rs` (URL cell at `:70-72`)
- Modify: `src/cmd/serve/browse.rs` (`edit_url` at `:207`, link at `:240`; add a tests mod — the file has none)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: `render_manage_page(sources: &[HedgedocSource], last_synced: Option<Timestamp>, config_available: bool) -> Markup` (`hedgedoc_ui.rs:8`), `render_browse_page(collection_name: &str, slug: &str, tree: &DeckNode, hedge_urls: &HashMap<String, String>, bookmark_count: usize) -> Markup` (`browse.rs:144`), structs `HedgedocSource`/`HedgedocNote` (`state.rs:33-46`), `ResolvedCollection` (`config.rs:149`, all fields pub, `Clone`), `DeckNode` (`browse.rs:16`, all fields pub).
- Produces: `pub fn safe_href(url: &str) -> Option<&str>` in `src/cmd/serve/href.rs`. Consumed by `hedgedoc_ui.rs` and `browse.rs` in this task only.

- [x] **Step 1: Write the failing regression tests**

Add a tests mod at the end of `src/cmd/serve/hedgedoc_ui.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cmd::serve::config::ResolvedCollection;
    use crate::cmd::serve::state::HedgedocNote;

    fn source_with_url(url: &str) -> HedgedocSource {
        HedgedocSource {
            source_uri: "https://notes.example.com".to_string(),
            collection: ResolvedCollection {
                name: "Notes".to_string(),
                slug: "hedgedoc-notes".to_string(),
                coll_dir: PathBuf::from("/tmp/notes"),
                db_path: PathBuf::from("/tmp/notes.db"),
            },
            notes: vec![HedgedocNote {
                url: url.to_string(),
                deck_name: "deck".to_string(),
                file_name: "deck.md".to_string(),
                last_error: None,
            }],
        }
    }

    #[test]
    fn manage_page_never_links_unsafe_url_schemes() {
        // Regression test for BUG-24: even if a bad URL reaches the state
        // (hand-edited config, pre-fix persisted data), it must not become
        // a clickable link.
        let sources = vec![source_with_url("javascript:alert(1)")];
        let html = render_manage_page(&sources, None, true).into_string();
        assert!(
            !html.contains(r#"href="javascript:"#),
            "unsafe scheme must not be rendered as a link: {html}"
        );
        assert!(
            html.contains("javascript:alert(1)"),
            "the URL should still be visible as plain text: {html}"
        );
    }

    #[test]
    fn manage_page_links_https_urls() {
        let sources = vec![source_with_url("https://notes.example.com/abc")];
        let html = render_manage_page(&sources, None, true).into_string();
        assert!(
            html.contains(r#"href="https://notes.example.com/abc""#),
            "https URLs must still be linked: {html}"
        );
    }
}
```

Add a tests mod at the end of `src/cmd/serve/browse.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_page_never_links_unsafe_edit_urls() {
        // Regression test for BUG-24 (render-time guard on edit links).
        let tree = DeckNode {
            name: String::new(),
            path: String::new(),
            total_cards: 0,
            due_today: 0,
            children: vec![DeckNode {
                name: "deck".to_string(),
                path: "deck".to_string(),
                total_cards: 1,
                due_today: 1,
                children: vec![],
            }],
        };
        let mut hedge_urls: HashMap<String, String> = HashMap::new();
        hedge_urls.insert("deck".to_string(), "javascript:alert(1)".to_string());
        let html = render_browse_page("Coll", "coll", &tree, &hedge_urls, 0).into_string();
        assert!(
            !html.contains(r#"href="javascript:"#),
            "unsafe scheme must not become an edit link: {html}"
        );
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test manage_page_never_links_unsafe_url_schemes browse_page_never_links_unsafe_edit_urls` — cargo takes one filter; run them separately:
`cargo test manage_page_never` and `cargo test browse_page_never`
Expected: both FAIL — the current templates render `href="javascript:alert(1)"` verbatim (maud escapes the attribute value but the scheme is live). `manage_page_links_https_urls` (run via `cargo test manage_page_links`) should already PASS.

- [x] **Step 3: Implement `safe_href`**

Create `src/cmd/serve/href.rs`:

```rust
/// Return the URL if it is safe to render into an `href` attribute
/// (http or https scheme only), otherwise `None`.
///
/// Defense in depth for BUG-24: URLs are validated when stored, but a
/// hand-edited config file, pre-existing persisted data, or a future
/// refactor must not be able to smuggle a `javascript:` (or other unsafe
/// scheme) URL into a link.
pub fn safe_href(url: &str) -> Option<&str> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        Some(trimmed)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_http_and_https() {
        assert_eq!(safe_href("https://x.com/a"), Some("https://x.com/a"));
        assert_eq!(safe_href("http://x.com/a"), Some("http://x.com/a"));
        assert_eq!(safe_href("  https://x.com  "), Some("https://x.com"));
    }

    #[test]
    fn rejects_everything_else() {
        assert_eq!(safe_href("javascript:alert(1)"), None);
        assert_eq!(safe_href("JavaScript:alert(1)"), None);
        assert_eq!(safe_href(" javascript:alert(1)"), None);
        assert_eq!(safe_href("data:text/html,x"), None);
        assert_eq!(safe_href("/relative/path"), None);
        assert_eq!(safe_href(""), None);
    }
}
```

In `src/cmd/serve/mod.rs`, add to the module list (alphabetical, after `mod hedgedoc_ui;`):

```rust
mod href;
```

- [x] **Step 4: Guard the two render sites**

In `src/cmd/serve/hedgedoc_ui.rs`, add the import:

```rust
use crate::cmd::serve::href::safe_href;
```

Replace the URL cell (lines 70-72):

```rust
                                        td.source-url-cell {
                                            a href=(note.url) target="_blank" rel="noopener noreferrer" { (note.url) }
                                        }
```

with:

```rust
                                        td.source-url-cell {
                                            @if let Some(href) = safe_href(&note.url) {
                                                a href=(href) target="_blank" rel="noopener noreferrer" { (note.url) }
                                            } @else {
                                                span { (note.url) }
                                            }
                                        }
```

In `src/cmd/serve/browse.rs`, add the import:

```rust
use crate::cmd::serve::href::safe_href;
```

Change the `edit_url` computation in `render_deck_node` (line 207) from:

```rust
    let edit_url = if !has_children { hedge_urls.get(&node.path) } else { None };
```

to:

```rust
    let edit_url = if !has_children {
        hedge_urls.get(&node.path).and_then(|url| safe_href(url))
    } else {
        None
    };
```

(`edit_url` becomes `Option<&str>` instead of `Option<&String>`; the `@if let Some(url) = edit_url { a.edit-link href=(url) ... }` at line 240 compiles unchanged.)

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test`
Expected: PASS — the two new template tests, the `href` unit tests, and the whole existing suite.

- [x] **Step 6: Update CHANGELOG.xml**

Append inside `<unreleased><security>` (created in Task 2; create the section as shown there if executing this task first):

```xml
            <change author="claude">
                HedgeDoc source URLs are now validated at storage time (well-formed HTTPS only) and re-checked at render time, so a link with an unsafe scheme such as `javascript:` can never be stored or rendered as a clickable link.
            </change>
```

- [x] **Step 7: Commit**

```bash
git add src/cmd/serve/href.rs src/cmd/serve/mod.rs src/cmd/serve/hedgedoc_ui.rs src/cmd/serve/browse.rs CHANGELOG.xml
git commit -m "fix: guard href scheme at render time for user-supplied URLs (BUG-24)"
```

---

### Task 5: BUG-25 — KaTeX failure leaves `.card-content` permanently invisible

**Files:**
- Modify: `src/cmd/drill/script.js` (DOMContentLoaded handler at `:15-40`)
- Modify: `CHANGELOG.xml`

**Interfaces:**
- Consumes: `MACROS` global (prepended to the served script by `src/cmd/drill/server.rs:238-242` — always defined); `katex` and `hljs` globals from vendored scripts loaded via `<script defer>` in `src/cmd/drill/template.rs:76-78`; the `.card-content { opacity: 0; }` bootstrap style at `template.rs:79`.
- Produces: nothing consumed by other tasks. `template.rs` itself is NOT modified — the `opacity: 0` bootstrap and the `noscript` fallback at `:80` stay as they are.

There is no JavaScript test infrastructure in this repository (no `package.json`, no node toolchain), so per the global TDD rule the "failing test" is a documented manual verification executed before and after the change.

- [x] **Step 1: Manual verification — reproduce the bug (the "failing test")**

Important precondition: the bug only triggers when the displayed card contains math — `katex` is referenced inside the `.math-inline`/`.math-display` `forEach` callbacks (`script.js:17-31`), so with no math elements the handler completes and restores opacity even without KaTeX. Use a scratch deck with a math card.

Exact steps:

1. Create a scratch collection with one math card:

```bash
mkdir -p /tmp/claude-1000/-home-user01-Projekte-hashcards-web/34e4ac38-9a26-4114-a0a1-c2247151e1df/scratchpad/katex-repro
printf 'Q: What is $x^2$ when $x = 2$?\nA: $4$\n' > /tmp/claude-1000/-home-user01-Projekte-hashcards-web/34e4ac38-9a26-4114-a0a1-c2247151e1df/scratchpad/katex-repro/Deck.md
```

2. `cargo run -- drill /tmp/claude-1000/-home-user01-Projekte-hashcards-web/34e4ac38-9a26-4114-a0a1-c2247151e1df/scratchpad/katex-repro --port 8123` — the drill serves at `http://127.0.0.1:8123` (and opens the browser by default).
3. In the browser, open DevTools → Network tab → enable request blocking for the pattern `*katex*` (Chrome: Network request blocking panel; Firefox: right-click a katex request → Block URL).
4. Reload the page.
5. Observe the bug: the card area is blank — `.card-content` stays at `opacity: 0` because `script.js:18` calls `katex.render` unguarded, the `ReferenceError` aborts the `DOMContentLoaded` handler, and the opacity restore at `script.js:36-39` never runs. Confirm the `ReferenceError: katex is not defined` in the DevTools console.
6. Quit the drill (Ctrl+C).

Record: bug reproduced (card invisible, ReferenceError in console).

- [x] **Step 2: Implement the guard + finally**

Replace the whole `DOMContentLoaded` handler in `src/cmd/drill/script.js` (lines 15-40) with:

```javascript
document.addEventListener("DOMContentLoaded", function () {
  try {
    if (typeof katex !== "undefined") {
      // Render inline math
      document.querySelectorAll(".math-inline").forEach(function (element) {
        katex.render(element.textContent, element, {
          displayMode: false,
          throwOnError: false,
          macros: MACROS,
        });
      });
      // Render display math
      document.querySelectorAll(".math-display").forEach(function (element) {
        katex.render(element.textContent, element, {
          displayMode: true,
          throwOnError: false,
          macros: MACROS,
        });
      });
    }
    // Initialize syntax highlighting
    if (typeof hljs !== "undefined") {
      hljs.highlightAll();
    }
  } finally {
    // The card content must become visible no matter what failed above
    // (BUG-25): the page bootstraps with `.card-content { opacity: 0 }`
    // to avoid a flash of unrendered math.
    const cardContent = document.querySelector(".card-content");
    if (cardContent) {
      cardContent.style.opacity = "1";
    }
  }
});
```

(Both the `typeof` guard and the `finally` are deliberate: the guard handles a missing script cleanly with no console noise for the known case; the `finally` guarantees visibility against any other failure, e.g. a broken `MACROS` entry or a katex internal error.)

- [x] **Step 3: Manual verification — confirm the fix (the "passing test")**

Repeat Step 1's exact steps 1-4 (same scratch deck, `cargo run -- drill .../katex-repro --port 8123`, block `*katex*`, reload).
Expected: the card content is visible (math appears as raw TeX source, which is acceptable degraded behavior); no uncaught `ReferenceError` breaks the handler; syntax highlighting still runs. Then unblock `*katex*`, reload, and confirm math renders normally and the card is visible — no regression in the happy path.

Also run: `cargo test`
Expected: PASS (script.js is embedded/served by the drill server; the Rust suite must still be green).

- [x] **Step 4: Update CHANGELOG.xml**

Append inside the existing `<unreleased><fixed>` section:

```xml
            <change author="claude">
                Cards no longer stay permanently invisible when the KaTeX script fails to load or math rendering throws: card content visibility is now restored even when rendering fails.
            </change>
```

- [x] **Step 5: Commit**

```bash
git add src/cmd/drill/script.js CHANGELOG.xml
git commit -m "fix: always restore card visibility when KaTeX fails (BUG-25)"
```

---

### Task 6: Final verification pass

**Files:**
- None created or modified (fix-ups only if checks fail).

**Interfaces:**
- Consumes: everything from Tasks 1-5.
- Produces: a green branch ready for the "Rendering safety" PR.

- [x] **Step 1: Run the full suite**

Run: `cargo test`
Expected: PASS, zero failures.

- [x] **Step 2: Lint and format**

Run: `cargo clippy --all-targets` and `cargo fmt -- --check`
Expected: no warnings from the changed files, no formatting diffs. Fix anything reported and amend the relevant commit (or add a `chore:` commit).

- [x] **Step 3: Grep for leftovers**

Run: `grep -rn "CLOZE_TAG" src/`
Expected: no matches (the old sentinel constants are gone).
Run: `grep -rn "unwrap()" src/cmd/serve/href.rs src/markdown.rs src/types/card.rs | grep -v "mod tests" | grep -v "#\[test\]"`
Expected: no new production `unwrap()` (test-mod hits are fine; verify any hit is inside `#[cfg(test)]`).

- [x] **Step 4: Validate CHANGELOG.xml against the XSD**

Run: `xmllint --schema CHANGELOG.xsd CHANGELOG.xml --noout` (if `xmllint` is unavailable, visually check: each new `<change>` sits inside `<fixed>` or `<security>` inside `<unreleased>`, with an `author` attribute and XML-escaped text).
Expected: `CHANGELOG.xml validates`.

---

## Spec discrepancies

Verified every cited line against the working tree (commit `be55f80`):

1. **All line citations for this PR group are accurate.** `card.rs:201` and `:225-228` (the two `replace(CLOZE_TAG, ...)` calls), `markdown.rs:74-79` (audio `Event::Html`, format string at `:76`), `hedgedoc_ui.rs:71` (`a href=(note.url)`), `browse.rs:240` (`a.edit-link href=(url)`), `handlers.rs:534-545` (URL parse with `Err(_) => trimmed.to_string()` at `:544`), `template.rs:79` (`.card-content { opacity: 0; }`), `script.js:18` (unguarded `katex.render`), `:33` (`hljs` guard), `:36-39` (opacity restore) — all match.
2. **BUG-24 says "validate scheme is http/https at storage time", but the codebase's existing policy is HTTPS-only.** `validate_hedgedoc_url` (`hedgedoc.rs:57-64`) rejects plain `http://` at fetch time, with tests pinning that behavior (`hedgedoc.rs:604-611`). This plan makes storage-time validation HTTPS-only (reusing `validate_hedgedoc_url`, consistent with fetch-time and with SPEC BUG-38), while the render-time guard (`safe_href`) accepts http *and* https per the spec — it is generic defense-in-depth that may later guard non-HedgeDoc URLs.
3. **The comment at `handlers.rs:96-99` ("All URLs were already validated as HTTPS when added") is currently false** — validation happens at fetch time, not add time. Task 3 makes it true for newly added URLs; pre-existing persisted bad URLs are covered by the render-time guard (Task 4).
4. **BUG-25's fix lands entirely in `script.js`; `template.rs:79` is cited as context only.** The `opacity: 0` bootstrap and the `noscript { opacity: 1 }` fallback at `template.rs:79-80` are correct as-is and are not modified.
5. **No JS test infrastructure exists** (no `package.json` in the repo), so BUG-25's regression "test" is the documented manual verification in Task 5 Steps 1 and 3, per the planning directive.
6. **SPEC BUG-24 references BUG-38 (validation in `hedgedoc_add_handler` before persisting).** Task 3 implements exactly that wiring; when PR group 9 tackles BUG-38's flash-message UX, only the `log::error` + redirect branch needs replacing — the validation itself will already exist.
