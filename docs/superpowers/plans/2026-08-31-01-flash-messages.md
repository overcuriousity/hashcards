# Flash Messages + Error Surfacing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a shared one-shot flash-message mechanism (FEAT-01) and route every silent failure path in the drill and serve servers through it (BUG-03, BUG-05, BUG-11, BUG-37, BUG-51).

**Architecture:** A new `src/flash.rs` module carries a `Flash { kind, message }` value across a redirect as percent-encoded query params (`?flash=...&kind=success|error`). GET handlers extract it with axum's `Query<HashMap<String,String>>` (which percent-decodes values) and render it once as a dismissible banner at the top of the page body. POST handlers that today log-and-redirect, or discard errors with `let _ =`, instead redirect with a flash. `handle_action` gains an `ActionResult::ContinueWithFlash(Flash)` variant so shared drill/serve action logic can surface messages (BUG-11). The remaining BUG-51 items are local fixes: stats CLI default/error, macros.tex warnings, a styled 500 drill error page, and a strengthened serve integration test.

**Tech Stack:** Rust (edition 2024), axum 0.8, maud 0.27, rusqlite, `percent-encoding` 2.3 (already a dependency — no new crates), clap 4, tokio, reqwest+portpicker+tempfile for integration tests.

**Spec:** SPEC.md

## Global Constraints

From SPEC.md "Global requirements" (verbatim):

- Every bugfix starts with a failing regression test.
- No `unwrap()` in production code; use `Fallible`, `?`, and `fail()`.
- All error messages are user-facing and clear.
- Reviews and performance are written in one transaction; undo voids, never deletes.
- Cloze positions are byte positions: `.bytes()`, never `.chars()`.
- Update `CHANGELOG.xml` per item.

Additional constraints for this plan:

- This is a binary crate: modules are reached as `crate::flash::...`; register new modules in `src/main.rs`.
- Prefer imports over fully qualified names (add `use` statements at the top of each module).
- `ErrorReport`'s `Display` impl prefixes messages with `"error: "` (`src/error.rs:118`); flash messages built from `e.to_string()` therefore read `error: <message>` — this is acceptable and expected by the tests below.
- The existing `.lock().unwrap()` calls are deliberately left untouched: they are BUG-50, scheduled for a separate PR (PR group 6). Do not "fix" them in passing; match the surrounding style when editing those functions.
- `finished_at.unwrap()` at `src/cmd/drill/get.rs` (inside `render_completion_page`) is BUG-49 (PR group 6) — leave it as is.
- `unwrap()` in tests is fine.
- CHANGELOG entries go under the existing `<unreleased>` element of `CHANGELOG.xml`, as `<change author="claude">...</change>` inside `<fixed>`, `<added>`, or `<changed>` (schema: `CHANGELOG.xsd`; existing entries are the style reference).
- Run `cargo fmt` before every commit; run `cargo test` (full suite) at the end of each task.

---

### Task 1: `src/flash.rs` — the Flash module (FEAT-01 core)

**Files:**
- Create: `src/flash.rs`
- Modify: `src/main.rs:19` (add `mod flash;` after `mod error;`)
- Modify: `src/cmd/drill/style.css` (append `.flash` styles at end of file)

**Interfaces:**
- Consumes: `percent-encoding` crate (already in `Cargo.toml`), `axum::response::Redirect`, `maud`.
- Produces (all later tasks rely on this exact API):
  ```rust
  pub enum FlashKind { Success, Error }               // derives Debug, Clone, Copy, PartialEq, Eq
  pub struct Flash { pub kind: FlashKind, pub message: String }  // derives Debug, Clone, PartialEq
  impl Flash {
      pub fn success(message: impl Into<String>) -> Self;
      pub fn error(message: impl Into<String>) -> Self;
      pub fn redirect(self, to: &str) -> axum::response::Redirect;
      pub fn from_query(query: &std::collections::HashMap<String, String>) -> Option<Flash>;
      pub fn render(&self) -> maud::Markup;
  }
  ```
- Encoding contract: `redirect` percent-encodes the message into `?flash=<encoded>&kind=<success|error>`. `from_query` does **no** decoding because axum's `Query` extractor already percent-decodes values; it reads the map directly.

- [x] **Step 1: Write the failing tests**

Create `src/flash.rs` with only the test module (the `use super::*;` will fail to find the types):

```rust
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn test_redirect_url_percent_encodes_message() {
        let flash = Flash::error("100% bad & wrong?");
        let url = flash.redirect_url("/collection/foo");
        assert_eq!(
            url,
            "/collection/foo?flash=100%25%20bad%20%26%20wrong%3F&kind=error"
        );
    }

    #[test]
    fn test_redirect_url_appends_with_ampersand_when_query_exists() {
        let flash = Flash::success("ok");
        assert_eq!(flash.redirect_url("/x?a=1"), "/x?a=1&flash=ok&kind=success");
    }

    #[test]
    fn test_from_query_roundtrip() {
        // axum's Query extractor percent-decodes values before they reach us,
        // so from_query sees plain text.
        let mut query = HashMap::new();
        query.insert("flash".to_string(), "Bookmark removed.".to_string());
        query.insert("kind".to_string(), "success".to_string());
        let flash = Flash::from_query(&query).unwrap();
        assert_eq!(flash.kind, FlashKind::Success);
        assert_eq!(flash.message, "Bookmark removed.");
    }

    #[test]
    fn test_from_query_missing_flash_is_none() {
        let query = HashMap::new();
        assert!(Flash::from_query(&query).is_none());
    }

    #[test]
    fn test_from_query_unknown_kind_defaults_to_error() {
        let mut query = HashMap::new();
        query.insert("flash".to_string(), "boom".to_string());
        let flash = Flash::from_query(&query).unwrap();
        assert_eq!(flash.kind, FlashKind::Error);
    }

    #[test]
    fn test_render_success_banner() {
        let html = Flash::success("Saved.").render().into_string();
        assert!(html.contains("flash-success"));
        assert!(html.contains("Saved."));
    }

    #[test]
    fn test_render_error_banner() {
        let html = Flash::error("Nope.").render().into_string();
        assert!(html.contains("flash-error"));
        assert!(html.contains("Nope."));
    }

    #[test]
    fn test_render_escapes_html_in_message() {
        let html = Flash::error("<script>alert(1)</script>").render().into_string();
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
```

Register the module in `src/main.rs` — after line 19 (`mod error;`) insert:

```rust
mod flash;
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test flash`
Expected: COMPILE ERROR (`Flash`/`FlashKind` not found).

- [x] **Step 3: Write the implementation**

Prepend to `src/flash.rs` (above the test module):

```rust
//! One-shot flash messages, shared by the drill and serve servers.
//!
//! A `Flash` travels across a redirect as query params
//! (`?flash=<percent-encoded>&kind=<success|error>`), is extracted from the
//! decoded query map on the next GET, and is rendered once as a dismissible
//! banner. It is never persisted: reloading the redirected-to URL re-shows
//! it, navigating away drops it.

use std::collections::HashMap;

use axum::response::Redirect;
use maud::Markup;
use maud::html;
use percent_encoding::NON_ALPHANUMERIC;
use percent_encoding::utf8_percent_encode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashKind {
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flash {
    pub kind: FlashKind,
    pub message: String,
}

impl Flash {
    pub fn success(message: impl Into<String>) -> Self {
        Flash {
            kind: FlashKind::Success,
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Flash {
            kind: FlashKind::Error,
            message: message.into(),
        }
    }

    fn kind_str(&self) -> &'static str {
        match self.kind {
            FlashKind::Success => "success",
            FlashKind::Error => "error",
        }
    }

    /// The redirect target URL with the flash appended as query params.
    /// Separate from `redirect` so it can be unit-tested (axum's `Redirect`
    /// does not expose its target).
    fn redirect_url(&self, to: &str) -> String {
        let sep = if to.contains('?') { '&' } else { '?' };
        let encoded = utf8_percent_encode(&self.message, NON_ALPHANUMERIC);
        format!("{to}{sep}flash={encoded}&kind={}", self.kind_str())
    }

    /// Redirect carrying the flash as query params
    /// `?flash=<percent-encoded>&kind=<success|error>`.
    pub fn redirect(self, to: &str) -> Redirect {
        Redirect::to(&self.redirect_url(to))
    }

    /// Extract from request query params (one-shot: rendered once, not
    /// persisted). The map comes from axum's `Query` extractor, which has
    /// already percent-decoded the values.
    pub fn from_query(query: &HashMap<String, String>) -> Option<Flash> {
        let message = query.get("flash")?.clone();
        let kind = match query.get("kind").map(|s| s.as_str()) {
            Some("success") => FlashKind::Success,
            _ => FlashKind::Error,
        };
        Some(Flash { kind, message })
    }

    /// Dismissible banner: `div.flash.flash-success` / `div.flash.flash-error`.
    pub fn render(&self) -> Markup {
        let class = match self.kind {
            FlashKind::Success => "flash flash-success",
            FlashKind::Error => "flash flash-error",
        };
        html! {
            div class=(class) role="alert" {
                span.flash-message { (self.message) }
                button.flash-dismiss
                    type="button"
                    onclick="this.closest('.flash').remove()"
                    title="Dismiss"
                { "\u{00D7}" }
            }
        }
    }
}
```

Append to `src/cmd/drill/style.css`:

```css
.flash {
    display: flex;
    align-items: center;
    gap: 12px;
    max-width: 800px;
    margin: 16px auto;
    padding: 12px 16px;
    border: 1px solid;
    border-radius: 8px;
    font-size: 14px;
}

.flash-message {
    flex: 1;
}

.flash-dismiss {
    background: none;
    border: none;
    font-size: 18px;
    line-height: 1;
    padding: 0 4px;
    cursor: pointer;
    color: inherit;
}

.flash-error {
    background: #fff4f1;
    border-color: #f3c5bd;
    color: #7a271a;
}

.flash-success {
    background: #f0fdf4;
    border-color: #bbe5c8;
    color: #027a48;
}
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test flash`
Expected: all 8 tests PASS.

- [x] **Step 5: Commit**

```bash
cargo fmt
git add src/flash.rs src/main.rs src/cmd/drill/style.css
git commit -m "feat: add shared flash message module (FEAT-01)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_012SzQd3T2Dbb4nr1FGnFjTW"
```

---

### Task 2: Wire flash into the drill server (BUG-03, drill half)

`src/cmd/drill/post.rs:81-87` logs errors and redirects as if the action succeeded. Fix: the POST handler redirects with `Flash::error`, and the GET handler extracts and renders the flash. Also add the `ActionResult::ContinueWithFlash(Flash)` variant that Task 4 (BUG-11) and Task 7 (serve) build on.

**Files:**
- Modify: `src/cmd/drill/post.rs:64-105` (`ActionResult`, `post_handler`, `action_handler`)
- Modify: `src/cmd/drill/get.rs:54-86` (`get_handler`, `inner`)
- Modify: `src/cmd/drill/mod.rs` (tests module — integration test)

**Interfaces:**
- Consumes: `crate::flash::Flash` from Task 1.
- Produces: `ActionResult::ContinueWithFlash(Flash)` — new variant of `pub enum ActionResult` in `src/cmd/drill/post.rs`; `handle_action` may return it, and every caller must map it to `flash.redirect(...)`. Also: drill `get_handler` now takes `Query<HashMap<String, String>>` and renders the flash above the page body; `inner` becomes `async fn inner(state: ServerState, flash: Option<Flash>) -> Fallible<Markup>`.

- [x] **Step 1: Write the failing integration test**

In `src/cmd/drill/mod.rs`, inside the existing `mod tests`, add (imports `pick_unused_port`, `spawn`, `wait_for_server`, `create_tmp_copy_of_test_directory`, `Timestamp`, `ServerConfig`, `AnswerControls`, `start_server`, `TEST_HOST` already exist in that module):

```rust
#[tokio::test]
async fn test_flash_query_param_renders_banner() -> Fallible<()> {
    let port = pick_unused_port().unwrap();
    let directory = create_tmp_copy_of_test_directory()?;
    let session_started_at = Timestamp::now();
    let config = ServerConfig {
        directory: Some(directory),
        host: TEST_HOST.to_string(),
        port,
        session_started_at,
        card_limit: None,
        new_card_limit: None,
        deck_filter: None,
        shuffle: false,
        answer_controls: AnswerControls::Full,
        bury_siblings: false,
    };
    spawn(async move { start_server(config).await });
    wait_for_server(TEST_HOST, port).await?;

    let response =
        reqwest::get(format!("http://{TEST_HOST}:{port}/?flash=Hello%20there&kind=success"))
            .await?;
    assert!(response.status().is_success());
    let body = response.text().await?;
    assert!(body.contains("flash-success"), "body: {body}");
    assert!(body.contains("Hello there"));
    Ok(())
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test test_flash_query_param_renders_banner`
Expected: FAIL — the assertion `body.contains("flash-success")` fails (query param is ignored today).

- [x] **Step 3: Implement**

In `src/cmd/drill/post.rs`, add imports:

```rust
use crate::flash::Flash;
```

Extend `ActionResult` (replace the enum at `post.rs:64-74`):

```rust
/// Result of handling an action on the drill session.
pub enum ActionResult {
    /// Continue drilling (redirect back to the same page).
    Continue,
    /// Continue drilling, showing a one-shot flash message.
    ContinueWithFlash(Flash),
    /// The session finished (all cards done or user pressed End).
    SessionFinished,
    /// The user requested server shutdown (drill mode only).
    Shutdown,
    /// The user requested to go back to the collection list (serve mode).
    Home,
}
```

Replace `post_handler` and `action_handler` (`post.rs:77-105`):

```rust
pub async fn post_handler(
    State(state): State<ServerState>,
    Form(form): Form<FormData>,
) -> Redirect {
    match action_handler(state, form.action).await {
        Ok(Some(flash)) => flash.redirect("/"),
        Ok(None) => Redirect::to("/"),
        Err(e) => {
            log::error!("error: {e}");
            Flash::error(e.to_string()).redirect("/")
        }
    }
}

async fn action_handler(state: ServerState, action: Action) -> Fallible<Option<Flash>> {
    let mut mutable = state.mutable.lock().unwrap();
    let result = handle_action(&mut mutable, state.session_started_at, action)?;
    match result {
        ActionResult::Shutdown => {
            // Release the lock before sending shutdown signal.
            drop(mutable);
            let mut shutdown_tx = state.shutdown_tx.lock().unwrap();
            if let Some(tx) = shutdown_tx.take() {
                let _ = tx.send(());
            }
            Ok(None)
        }
        ActionResult::ContinueWithFlash(flash) => Ok(Some(flash)),
        _ => Ok(None),
    }
}
```

(Note: the `let _ = tx.send(())` on the oneshot channel stays — the receiver can only be gone during shutdown races; it is not a fallible DB write. The `.lock().unwrap()` calls stay per BUG-50.)

In `src/cmd/drill/get.rs`, add imports:

```rust
use std::collections::HashMap;

use axum::extract::Query;

use crate::flash::Flash;
```

Replace `get_handler` and `inner` (`get.rs:54-86`):

```rust
pub async fn get_handler(
    State(state): State<ServerState>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    let html = match inner(state, flash).await {
        Ok(html) => html,
        Err(e) => page_template(html! {
            div.error {
                h1 { "Error" }
                p { (e) }
            }
        }),
    };
    (StatusCode::OK, Html(html.into_string()))
}

async fn inner(state: ServerState, flash: Option<Flash>) -> Fallible<Markup> {
    let mutable = state.mutable.lock().unwrap();
    let file_url_prefix = format!("http://localhost:{}/file", state.port);
    let ctx = RenderContext {
        directory: &state.directory,
        total_cards: state.total_cards,
        session_started_at: state.session_started_at,
        answer_controls: state.answer_controls,
        form_action: "/",
        file_url_prefix: &file_url_prefix,
        completion_action: CompletionAction::Shutdown,
    };
    let body = if mutable.finished_at.is_some() {
        render_completion_page(&ctx, &mutable)?
    } else {
        render_session_page(&ctx, &mutable)?
    };
    let body = html! {
        @if let Some(f) = &flash { (f.render()) }
        (body)
    };
    let html = page_template(body);
    Ok(html)
}
```

(The error arm still returns `StatusCode::OK` here — Task 10 fixes that; do not fix it in this task.)

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test test_flash_query_param_renders_banner` then `cargo test`
Expected: PASS; full suite green.

- [x] **Step 5: Commit**

```bash
cargo fmt
git add src/cmd/drill/post.rs src/cmd/drill/get.rs src/cmd/drill/mod.rs
git commit -m "feat: surface drill POST errors as flash messages (BUG-03, drill)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_012SzQd3T2Dbb4nr1FGnFjTW"
```

---

### Task 3: Render flash on all serve-mode GET pages (FEAT-01 wiring)

Every redirect target in serve mode must render an incoming flash: `/` (landing), `/collection/{slug}` (browse + session pages), `/collection/{slug}/bookmarks`, `/hedgedoc`.

**Files:**
- Modify: `src/cmd/serve/handlers.rs:55-148` (`collection_get_handler`, `collection_get_inner`), `:503-514` (`hedgedoc_manage_handler`)
- Modify: `src/cmd/serve/landing.rs:12-40`
- Modify: `src/cmd/serve/bookmarks.rs:27-46, 66-82`
- Modify: `src/cmd/serve/browse.rs:144-153`
- Modify: `src/cmd/serve/hedgedoc_ui.rs:8-15`
- Modify: `src/cmd/serve/mod.rs` (tests module — integration test)

**Interfaces:**
- Consumes: `Flash` (Task 1).
- Produces (Task 6 and Task 7 tests rely on these pages rendering flashes):
  - `render_browse_page(collection_name: &str, slug: &str, tree: &DeckNode, hedge_urls: &HashMap<String, String>, bookmark_count: usize, flash: Option<Flash>) -> Markup`
  - `render_landing_page(collections: &[CollectionInfo], last_synced: Option<Timestamp>, git_enabled: bool, hedgedoc_count: usize, hedgedoc_last_synced: Option<Timestamp>, config_available: bool, flash: Option<Flash>) -> Markup`
  - `render_manage_page(sources: &[HedgedocSource], last_synced: Option<Timestamp>, config_available: bool, flash: Option<Flash>) -> Markup`
  - `render_bookmark_list(collection_name: &str, slug: &str, coll_dir: &Path, bookmarks: &[Bookmark], cards: &HashMap<CardHash, &Card>, flash: Option<Flash>) -> Markup`
  - `collection_get_inner(state: &AppState, slug: &str, flash: Option<Flash>) -> Fallible<String>`
  - `bookmark_list_inner(state: &AppState, slug: &str, flash: Option<Flash>) -> Fallible<String>`
  - Also: `spawn_test_server` helper in serve `mod.rs` tests (used by Tasks 6, 7, 11).

- [x] **Step 1: Write the failing integration test and the test-server helper**

In `src/cmd/serve/mod.rs`, inside `mod tests`, add the helper (above the existing test) and refactor is NOT needed yet — the existing test keeps its inline config. Add:

```rust
use std::path::PathBuf;

/// Start a serve-mode server for one collection rooted at `coll_dir`,
/// registered under `slug`. Returns the port.
async fn spawn_test_server(coll_dir: PathBuf, slug: &str) -> Fallible<u16> {
    let port = pick_unused_port().unwrap();
    let config = ResolvedServeConfig {
        host: TEST_HOST.to_string(),
        port,
        git: None,
        defaults: DefaultsSection::default(),
        collections: vec![ResolvedCollection {
            name: "Test Collection".to_string(),
            slug: slug.to_string(),
            coll_dir: coll_dir.clone(),
            db_path: coll_dir.join("hashcards.db"),
        }],
        data_dir: None,
        config_path: None,
        hedgedoc_entries: Vec::new(),
        _temp_dir: None,
    };
    spawn(async move { start_serve(config).await });
    wait_for_server(TEST_HOST, port).await?;
    Ok(port)
}

#[tokio::test]
async fn test_flash_query_param_renders_on_collection_page() -> Fallible<()> {
    let dir = tempdir()?;
    let coll_dir = dir.path().to_path_buf();
    write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
    let slug = "test-collection";
    let port = spawn_test_server(coll_dir, slug).await?;

    let response = reqwest::get(format!(
        "http://{TEST_HOST}:{port}/collection/{slug}?flash=Hello%20world&kind=success"
    ))
    .await?;
    let body = response.text().await?;
    assert!(body.contains("flash-success"), "body: {body}");
    assert!(body.contains("Hello world"));
    Ok(())
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test test_flash_query_param_renders_on_collection_page`
Expected: FAIL on `body.contains("flash-success")`.

- [x] **Step 3: Implement**

`src/cmd/serve/handlers.rs` — add imports:

```rust
use std::collections::HashMap;

use axum::extract::Query;

use crate::flash::Flash;
```

Replace the `collection_get_handler` signature and first lines (`handlers.rs:55-63`):

```rust
pub async fn collection_get_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    // Determine whether this slug is known before calling the inner function,
    // so we can return 404 for unknown collections vs. 500 for real errors.
    let known = find_collection(&state, &slug).is_some()
        || state.sessions.lock().unwrap().contains_key(&slug);
    match collection_get_inner(&state, &slug, flash) {
```

(The rest of the handler's match arms stay unchanged.)

Change `collection_get_inner` (`handlers.rs:84`) signature:

```rust
fn collection_get_inner(state: &AppState, slug: &str, flash: Option<Flash>) -> Fallible<String> {
```

In its browse branch, change the `render_browse_page` call (`handlers.rs:126`):

```rust
        let html = render_browse_page(&rc.name, slug, &tree, &hedge_urls, bookmark_count, flash);
```

In its session branch, wrap the body before `page_template_with_script` (`handlers.rs:137-143`):

```rust
    let body = if session.mutable.finished_at.is_some() {
        render_completion_page(&ctx, &session.mutable)?
    } else {
        render_session_page(&ctx, &session.mutable)?
    };
    let body = html! {
        @if let Some(f) = &flash { (f.render()) }
        (body)
    };
    let script_url = format!("/collection/{slug}/script.js");
    let html = page_template_with_script(&script_url, body);
```

Replace `hedgedoc_manage_handler` (`handlers.rs:503-514`):

```rust
pub async fn hedgedoc_manage_handler(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    use crate::cmd::serve::hedgedoc_ui::render_manage_page;
    let flash = Flash::from_query(&query);
    let sources = state.hedgedoc_sources.lock().unwrap();
    let last_synced = *state.hedgedoc_last_synced.lock().unwrap();
    let config_available = state.config.data_dir.is_some();
    let html = render_manage_page(&sources, last_synced, config_available, flash);
    (StatusCode::OK, Html(html.into_string()))
}
```

`src/cmd/serve/browse.rs` — add import `use crate::flash::Flash;`, then change `render_browse_page` (`browse.rs:144-153`):

```rust
pub fn render_browse_page(
    collection_name: &str,
    slug: &str,
    tree: &DeckNode,
    hedge_urls: &HashMap<String, String>,
    bookmark_count: usize,
    flash: Option<Flash>,
) -> Markup {
    let total_due = tree.due_today_recursive();
    page_template(html! {
        @if let Some(f) = &flash { (f.render()) }
        div.browse {
```

`src/cmd/serve/landing.rs` — add imports:

```rust
use std::collections::HashMap;

use axum::extract::Query;

use crate::flash::Flash;
```

Change `landing_handler` (`landing.rs:12`) to extract and pass the flash:

```rust
pub async fn landing_handler(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    let collections = state.collections.read().await;
    let last_synced = *state.last_synced.lock().unwrap();
    let hedgedoc_last_synced = *state.hedgedoc_last_synced.lock().unwrap();
    let git_enabled = state.config.git.is_some();
    let hedgedoc_count = state.hedgedoc_sources.lock().unwrap().len();
    let config_available = state.config.data_dir.is_some();
    let html = render_landing_page(
        &collections,
        last_synced,
        git_enabled,
        hedgedoc_count,
        hedgedoc_last_synced,
        config_available,
        flash,
    );
    (StatusCode::OK, Html(html.into_string()))
}
```

And `render_landing_page` gains the trailing parameter and renders it first:

```rust
fn render_landing_page(
    collections: &[CollectionInfo],
    last_synced: Option<Timestamp>,
    git_enabled: bool,
    hedgedoc_count: usize,
    hedgedoc_last_synced: Option<Timestamp>,
    config_available: bool,
    flash: Option<Flash>,
) -> Markup {
    page_template(html! {
        @if let Some(f) = &flash { (f.render()) }
        div.landing {
```

`src/cmd/serve/hedgedoc_ui.rs` — add import `use crate::flash::Flash;`, change `render_manage_page` (`hedgedoc_ui.rs:8-15`):

```rust
pub fn render_manage_page(
    sources: &[HedgedocSource],
    last_synced: Option<Timestamp>,
    config_available: bool,
    flash: Option<Flash>,
) -> Markup {
    page_template(html! {
        @if let Some(f) = &flash { (f.render()) }
        div.landing {
```

`src/cmd/serve/bookmarks.rs` — add imports:

```rust
use axum::extract::Query;

use crate::flash::Flash;
```

Change `bookmark_list_handler` and `bookmark_list_inner` (`bookmarks.rs:27-46`):

```rust
pub async fn bookmark_list_handler(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    match bookmark_list_inner(&state, &slug, flash) {
        Ok(html) => (StatusCode::OK, Html(html)),
        Err(e) => error_page(&slug, e),
    }
}

fn bookmark_list_inner(state: &AppState, slug: &str, flash: Option<Flash>) -> Fallible<String> {
    let rc = find_collection(state, slug)
        .ok_or_else(|| ErrorReport::new(format!("Unknown collection: {slug}")))?;
    let collection = Collection::with_db_path(rc.coll_dir.clone(), rc.db_path.clone())?;
    let bookmarks = collection.db.list_bookmarks()?;
    let cards_by_hash: HashMap<CardHash, &Card> =
        collection.cards.iter().map(|c| (c.hash(), c)).collect();
    let html = render_bookmark_list(&rc.name, slug, &rc.coll_dir, &bookmarks, &cards_by_hash, flash);
    Ok(html.into_string())
}
```

And `render_bookmark_list` (`bookmarks.rs:66`):

```rust
fn render_bookmark_list(
    collection_name: &str,
    slug: &str,
    coll_dir: &Path,
    bookmarks: &[Bookmark],
    cards: &HashMap<CardHash, &Card>,
    flash: Option<Flash>,
) -> Markup {
```

and inside its `page_template(html! { ... })`, render the flash first:

```rust
    page_template(html! {
        @if let Some(f) = &flash { (f.render()) }
        div.bookmarks {
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test test_flash_query_param_renders_on_collection_page` then `cargo test`
Expected: PASS; full suite green.

- [x] **Step 5: Update CHANGELOG.xml (FEAT-01)**

In `CHANGELOG.xml`, inside `<unreleased><added>`, add:

```xml
            <change author="claude">
                Flash messages: actions in the drill and serve web interfaces now show one-shot success/error banners (dismissible) instead of failing or succeeding silently. The message travels as query parameters across the post-redirect.
            </change>
```

- [x] **Step 6: Commit**

```bash
cargo fmt
git add src/cmd/serve src/cmd/drill CHANGELOG.xml
git commit -m "feat: render flash messages on all serve-mode pages (FEAT-01)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_012SzQd3T2Dbb4nr1FGnFjTW"
```

---

### Task 4: BUG-11 — `Shutdown` before session completion explains itself

`src/cmd/drill/post.rs:144-150`: `Shutdown` on an unfinished session returns `Continue` with no message. Per spec, show a flash explaining why (honoring the shutdown is the alternative; the flash keeps this PR's scope to messaging).

**Files:**
- Modify: `src/cmd/drill/post.rs:144-150` (Shutdown arm) and its tests module

**Interfaces:**
- Consumes: `ActionResult::ContinueWithFlash` (Task 2), `Flash`, `FlashKind` (Task 1).
- Produces: nothing new.

- [x] **Step 1: Write the failing regression test**

In `src/cmd/drill/post.rs` `mod tests`, add `use crate::flash::FlashKind;` to the test imports and **replace** the existing `test_shutdown_returns_continue_when_unfinished` test with:

```rust
#[test]
fn test_shutdown_before_finish_flashes_explanation() {
    let mut mutable = make_mutable();
    assert!(mutable.finished_at.is_none());
    let now = Timestamp::now();
    let result = handle_action(&mut mutable, now, Action::Shutdown).unwrap();
    match result {
        ActionResult::ContinueWithFlash(flash) => {
            assert_eq!(flash.kind, FlashKind::Error);
            assert!(flash.message.contains("still in progress"));
        }
        _ => panic!("expected ContinueWithFlash"),
    }
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test test_shutdown_before_finish_flashes_explanation`
Expected: FAIL with `panicked at 'expected ContinueWithFlash'`.

- [x] **Step 3: Implement**

Replace the `Action::Shutdown` arm in `handle_action` (`post.rs:144-150`):

```rust
        Action::Shutdown => {
            if mutable.finished_at.is_some() {
                Ok(ActionResult::Shutdown)
            } else {
                Ok(ActionResult::ContinueWithFlash(Flash::error(
                    "The session is still in progress. Press End to finish it before shutting down.",
                )))
            }
        }
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test test_shutdown_before_finish_flashes_explanation` then `cargo test`
Expected: PASS; full suite green.

- [x] **Step 5: Update CHANGELOG.xml (BUG-11)**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add:

```xml
            <change author="claude">
                Pressing Shutdown before the drill session is finished now shows a message explaining that the session is still in progress, instead of silently doing nothing.
            </change>
```

- [x] **Step 6: Commit**

```bash
cargo fmt
git add src/cmd/drill/post.rs CHANGELOG.xml
git commit -m "fix: explain why Shutdown is refused mid-session (BUG-11)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_012SzQd3T2Dbb4nr1FGnFjTW"
```

---

### Task 5: BUG-05 — `End` on a not-yet-started session skips nonsense stats

`End` before any card is graded renders "0 cards in N min (0 s/card)" where N is the whole server uptime (`src/cmd/drill/post.rs:140-143` finishes the session; the stats render in `render_completion_page`, `src/cmd/drill/get.rs:241` onward). Fix per spec option: when no card was graded, report "No cards were reviewed." and skip the stats block entirely (which also removes the uptime-derived duration).

**Files:**
- Modify: `src/cmd/drill/get.rs` (`render_completion_page`, new `completion_actions` and `render_empty_completion_page` helpers, new tests module)

**Interfaces:**
- Consumes: `MutableState` (pub fields: `reveal, db, session_id, cache, cards, reviews, finished_at, card_shown_at`), `RenderContext`, `CompletionAction`.
- Produces: `render_completion_page` behavior change only; signature unchanged (`pub fn render_completion_page(ctx: &RenderContext, mutable: &MutableState) -> Fallible<Markup>`). Serve mode (`handlers.rs:138`) picks the fix up automatically.

- [x] **Step 1: Write the failing regression test**

`src/cmd/drill/get.rs` has no tests module; add one at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::cmd::drill::cache::Cache;
    use crate::db::Database;

    #[test]
    fn test_completion_page_without_reviews_skips_stats() -> Fallible<()> {
        let db = Database::new(":memory:").unwrap();
        let session_id = db.create_session(Timestamp::now()).unwrap();
        let mutable = MutableState {
            reveal: false,
            session_id,
            db,
            cache: Cache::new(),
            cards: Vec::new(),
            reviews: Vec::new(),
            finished_at: Some(Timestamp::now()),
            card_shown_at: None,
        };
        let ctx = RenderContext {
            directory: Path::new("."),
            total_cards: 0,
            session_started_at: Timestamp::now(),
            answer_controls: AnswerControls::Full,
            form_action: "/",
            file_url_prefix: "/file",
            completion_action: CompletionAction::Shutdown,
        };
        let html = render_completion_page(&ctx, &mutable)?.into_string();
        assert!(html.contains("No cards were reviewed."), "html: {html}");
        assert!(!html.contains("Session Stats"));
        assert!(!html.contains("s/card"));
        Ok(())
    }
}
```

(`AnswerControls` is already imported at the top of `get.rs` via `use crate::cmd::drill::server::AnswerControls;`.)

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test test_completion_page_without_reviews_skips_stats`
Expected: FAIL — today the page says "Done — 0 cards in 0 s (0 s/card)." and contains "Session Stats".

- [x] **Step 3: Implement**

In `src/cmd/drill/get.rs`, extract the `(action_button, redirect_notice)` match that currently sits inside `render_completion_page` into a helper, add an early return, and add the empty-page renderer. At the top of `render_completion_page` insert:

```rust
pub fn render_completion_page(ctx: &RenderContext, mutable: &MutableState) -> Fallible<Markup> {
    if mutable.reviews.is_empty() {
        return Ok(render_empty_completion_page(ctx));
    }
    let total_cards = ctx.total_cards;
```

Then replace the existing `let (action_button, redirect_notice) = match &ctx.completion_action { ... };` block inside `render_completion_page` with:

```rust
    let (action_button, redirect_notice) = completion_actions(ctx);
```

and add the two helpers after `render_completion_page` (the match body is moved verbatim from the current function):

```rust
/// The action button (Shutdown or Home) and the auto-redirect notice for the
/// completion page, depending on drill vs. serve mode.
fn completion_actions(ctx: &RenderContext) -> (Markup, Markup) {
    match &ctx.completion_action {
        CompletionAction::Shutdown => (
            html! {
                div.shutdown-container {
                    form action=(ctx.form_action) method="post" {
                        input #shutdown .shutdown-button.btn.btn-danger type="submit" name="action" value="Shutdown" title="Shut down the server";
                    }
                }
            },
            html! {},
        ),
        CompletionAction::BackToCollections => (
            html! {
                div.shutdown-container {
                    form #home-form action=(ctx.form_action) method="post" style="display:inline" {
                        input type="hidden" name="action" value="Home";
                        button #home .home-button.btn.btn-primary type="submit" { "Home" }
                    }
                }
            },
            html! {
                p.redirect-notice {
                    "Returning to collections in "
                    span #countdown { "5" }
                    "s. "
                    a #cancel-redirect href="#" { "Cancel" }
                }
                script { (maud::PreEscaped(REDIRECT_SCRIPT)) }
            },
        ),
    }
}

/// Completion page for a session that ended before any card was graded:
/// no stats block, since there is nothing meaningful to report.
fn render_empty_completion_page(ctx: &RenderContext) -> Markup {
    let (action_button, redirect_notice) = completion_actions(ctx);
    html! {
        div.finished {
            h1 { "Session Ended" }
            div.summary { "No cards were reviewed." }
            (redirect_notice)
            (action_button)
        }
    }
}
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test test_completion_page_without_reviews_skips_stats` then `cargo test`
Expected: PASS; full suite green.

- [x] **Step 5: Update CHANGELOG.xml (BUG-05)**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add:

```xml
            <change author="claude">
                Ending a session before revealing or grading any card no longer shows nonsense statistics (0 cards reviewed over the whole server uptime); the completion page now simply reports that no cards were reviewed.
            </change>
```

- [x] **Step 6: Commit**

```bash
cargo fmt
git add src/cmd/drill/get.rs CHANGELOG.xml
git commit -m "fix: skip stats block when ending an unstarted session (BUG-05)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_012SzQd3T2Dbb4nr1FGnFjTW"
```

---

### Task 6: BUG-37 — empty deck selection is rejected with a flash

`src/cmd/serve/handlers.rs:267-268`: an empty `decks` selection falls back to *all* cards, so a no-JS or hand-made POST with no `decks` field drills the whole collection. Fix: empty selection is an error, surfaced as a flash on the browse page.

**Files:**
- Modify: `src/cmd/serve/handlers.rs:213-242` (`collection_start_handler`, `collection_start_inner`)
- Modify: `src/cmd/serve/mod.rs` (tests module)

**Interfaces:**
- Consumes: `Flash` (Task 1), flash rendering on `/collection/{slug}` (Task 3), `spawn_test_server` (Task 3), `fail` from `crate::error`.
- Produces: `collection_start_handler` error arm redirects with `Flash::error(e.to_string())` — Task 7 leaves this handler alone.

- [x] **Step 1: Write the failing regression test**

In `src/cmd/serve/mod.rs` `mod tests`, add:

```rust
#[tokio::test]
async fn test_start_with_no_decks_is_rejected_with_flash() -> Fallible<()> {
    let dir = tempdir()?;
    let coll_dir = dir.path().to_path_buf();
    write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
    let slug = "test-collection";
    let port = spawn_test_server(coll_dir, slug).await?;

    // POST with no `decks` field at all (no-JS or hand-made form).
    let response = reqwest::Client::new()
        .post(format!("http://{TEST_HOST}:{port}/collection/{slug}/start"))
        .body("")
        .header("content-type", "application/x-www-form-urlencoded")
        .send()
        .await?;
    let body = response.text().await?;
    // The post-redirect page shows the flash and stays on the deck browser:
    assert!(body.contains("Select at least one deck"), "body: {body}");
    assert!(body.contains("flash-error"));
    // No session was started (a session page would show the Reveal button).
    assert!(!body.contains("value=\"Reveal\""));
    Ok(())
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test test_start_with_no_decks_is_rejected_with_flash`
Expected: FAIL — today the empty selection starts a whole-collection drill and the body contains `value="Reveal"` and no flash.

- [x] **Step 3: Implement**

In `src/cmd/serve/handlers.rs`, add `use crate::error::fail;` to the imports. Replace `collection_start_handler` (`handlers.rs:213-225`):

```rust
pub async fn collection_start_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Form(form): Form<StartDrillForm>,
) -> Redirect {
    match collection_start_inner(&state, &slug, form.decks, form.limit) {
        Ok(()) => Redirect::to(&format!("/collection/{slug}")),
        Err(e) => {
            log::error!("error starting drill for collection {slug}: {e}");
            Flash::error(e.to_string()).redirect(&format!("/collection/{slug}"))
        }
    }
}
```

And add the guard at the top of `collection_start_inner` (`handlers.rs:227-242`):

```rust
fn collection_start_inner(
    state: &AppState,
    slug: &str,
    selected_decks: Vec<String>,
    limit: Option<usize>,
) -> Fallible<()> {
    if selected_decks.is_empty() {
        return fail("Select at least one deck.");
    }
    // Remove any existing session before doing DB work.
    state.sessions.lock().unwrap().remove(slug);
```

(The `deck_filter.is_empty()` fallback inside `create_session` at `handlers.rs:268` is now unreachable from this path; leave it as defensive code.)

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test test_start_with_no_decks_is_rejected_with_flash` then `cargo test`
Expected: PASS (the existing `test_start_with_multiple_decks` must still pass); full suite green.

- [x] **Step 5: Update CHANGELOG.xml (BUG-37)**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add:

```xml
            <change author="claude">
                Starting a drill with no decks selected no longer silently drills the entire collection; it now shows "Select at least one deck."
            </change>
```

- [x] **Step 6: Commit**

```bash
cargo fmt
git add src/cmd/serve/handlers.rs src/cmd/serve/mod.rs CHANGELOG.xml
git commit -m "fix: reject empty deck selection with a flash (BUG-37)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_012SzQd3T2Dbb4nr1FGnFjTW"
```

---

### Task 7: BUG-03 — surface every swallowed serve-mode error through Flash

Route the remaining silent paths through flash: `collection_post_handler` (`handlers.rs:344-354` + the `handle_action` result at `:407-411`), `sync_handler` (`handlers.rs:481-500`), the two `let _ =` handlers in `bookmarks.rs:185-215`, and every early-return branch in `hedgedoc_add_handler` / `hedgedoc_delete_handler` / `hedgedoc_sync_now_handler` (`handlers.rs:521-806`, cited branches at `:532, :551, :564, :571, :591, :599, :648, :669`, plus the uncited but identically shaped ones at `:615, :620, :733-734, :753`).

**Files:**
- Modify: `src/cmd/serve/handlers.rs` (`collection_post_handler`, `collection_post_inner`, `sync_handler`, `hedgedoc_add_handler`, `hedgedoc_delete_handler`, `hedgedoc_sync_now_handler`)
- Modify: `src/cmd/serve/bookmarks.rs:185-215` (`bookmark_delete_handler`, `bookmark_note_handler`)
- Modify: `src/cmd/serve/mod.rs` (tests module)

**Interfaces:**
- Consumes: `Flash` (Task 1), `ActionResult::ContinueWithFlash` (Task 2), flash rendering on `/`, `/collection/{slug}`, `/collection/{slug}/bookmarks`, `/hedgedoc` (Task 3), `spawn_test_server` (Task 3).
- Produces: nothing new.

- [x] **Step 1: Write the failing regression tests**

In `src/cmd/serve/mod.rs` `mod tests`, add two tests exercising previously silent failures:

```rust
#[tokio::test]
async fn test_bookmark_delete_error_is_surfaced_as_flash() -> Fallible<()> {
    let dir = tempdir()?;
    let coll_dir = dir.path().to_path_buf();
    write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
    let slug = "test-collection";
    let port = spawn_test_server(coll_dir, slug).await?;

    // "nothex" is not a valid card hash: the delete must fail, and the
    // failure must be visible on the post-redirect bookmarks page.
    let response = reqwest::Client::new()
        .post(format!(
            "http://{TEST_HOST}:{port}/collection/{slug}/bookmarks/nothex/delete"
        ))
        .send()
        .await?;
    let body = response.text().await?;
    assert!(body.contains("flash-error"), "body: {body}");
    Ok(())
}

#[tokio::test]
async fn test_hedgedoc_add_empty_url_is_surfaced_as_flash() -> Fallible<()> {
    let dir = tempdir()?;
    let coll_dir = dir.path().to_path_buf();
    write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
    let port = spawn_test_server(coll_dir, "test-collection").await?;

    let response = reqwest::Client::new()
        .post(format!("http://{TEST_HOST}:{port}/hedgedoc/add"))
        .body("url=")
        .header("content-type", "application/x-www-form-urlencoded")
        .send()
        .await?;
    let body = response.text().await?;
    assert!(body.contains("Enter a HedgeDoc URL"), "body: {body}");
    assert!(body.contains("flash-error"));
    Ok(())
}
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test test_bookmark_delete_error_is_surfaced_as_flash test_hedgedoc_add_empty_url_is_surfaced_as_flash` (run each; `cargo test surfaced_as_flash` matches both)
Expected: both FAIL — today those requests redirect with no flash markup in the body.

- [x] **Step 3: Implement — bookmarks (`let _ =` removal)**

In `src/cmd/serve/bookmarks.rs` (imports `use crate::flash::Flash;` already added in Task 3), replace `bookmark_delete_handler` (`bookmarks.rs:185-190`) and `bookmark_note_handler` (`bookmarks.rs:209-215`):

```rust
pub async fn bookmark_delete_handler(
    State(state): State<AppState>,
    AxumPath((slug, hash_hex)): AxumPath<(String, String)>,
) -> Redirect {
    let to = format!("/collection/{slug}/bookmarks");
    match bookmark_delete_inner(&state, &slug, &hash_hex) {
        Ok(()) => Flash::success("Bookmark removed.").redirect(&to),
        Err(e) => Flash::error(format!("Failed to remove bookmark: {e}")).redirect(&to),
    }
}
```

```rust
pub async fn bookmark_note_handler(
    State(state): State<AppState>,
    AxumPath((slug, hash_hex)): AxumPath<(String, String)>,
    Form(form): Form<NoteForm>,
) -> Redirect {
    let to = format!("/collection/{slug}/bookmarks");
    match bookmark_note_inner(&state, &slug, &hash_hex, form.note) {
        Ok(()) => Flash::success("Note saved.").redirect(&to),
        Err(e) => Flash::error(format!("Failed to save note: {e}")).redirect(&to),
    }
}
```

- [x] **Step 4: Implement — collection POST and sync**

In `src/cmd/serve/handlers.rs`, add `use crate::cmd::drill::post::ActionResult;` to the imports. Replace `collection_post_handler` (`handlers.rs:344-356`):

```rust
pub async fn collection_post_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Form(form): Form<FormData>,
) -> Redirect {
    match collection_post_inner(&state, &slug, form.action) {
        Ok(redirect) => redirect,
        Err(e) => {
            log::error!("error handling action for collection {slug}: {e}");
            Flash::error(e.to_string()).redirect(&format!("/collection/{slug}"))
        }
    }
}
```

Replace the tail of `collection_post_inner` (`handlers.rs:400-411` — the comment block, `handle_action` call, re-insert, and return):

```rust
    // `Action::Home` returned early above, and it is the only action for which
    // `handle_action` yields `ActionResult::Home`. Every action reaching here
    // leaves the session running; the only result needing dispatch is
    // `ContinueWithFlash`, which carries a one-shot message for the user.
    let result = handle_action(&mut session.mutable, session.session_started_at, action)?;

    state.sessions.lock().unwrap().insert(slug.to_owned(), session);
    match result {
        ActionResult::ContinueWithFlash(flash) => {
            Ok(flash.redirect(&format!("/collection/{slug}")))
        }
        _ => Ok(Redirect::to(&format!("/collection/{slug}"))),
    }
}
```

Replace `sync_handler` (`handlers.rs:481-500`):

```rust
pub async fn sync_handler(State(state): State<AppState>) -> Redirect {
    let git = match &state.config.git {
        Some(git) => git,
        None => {
            return Flash::error("Sync is not available: no git repository is configured.")
                .redirect("/");
        }
    };

    match clone_or_pull(&git.repo_url, &git.branch, &git.repo_dir).await {
        Ok(()) => {
            let sources_snapshot = state.hedgedoc_sources.lock().unwrap().clone();
            let combined = build_combined_infos(&state.config.collections, &sources_snapshot);
            *state.collections.write().await = combined;
            *state.last_synced.lock().unwrap() = Some(Timestamp::now());
            log::debug!("Manual sync completed successfully");
            Flash::success("Sync complete.").redirect("/")
        }
        Err(e) => {
            log::error!("Manual sync failed: {e}");
            Flash::error(format!("Sync failed: {e}")).redirect("/")
        }
    }
}
```

- [x] **Step 5: Implement — HedgeDoc add/delete/sync-now**

In `hedgedoc_add_handler` (`handlers.rs:521-699`), replace each silent early return, keeping the existing `log::error!` lines where present:

1. Empty URL (`:532`): replace `return Redirect::to("/hedgedoc");` with

```rust
            return Flash::error("Enter a HedgeDoc URL.").redirect("/hedgedoc");
```

2. No data dir (`:551-552`): keep the `log::error!` line, replace the return with

```rust
            return Flash::error(
                "Cannot add HedgeDoc source: no data directory is configured. Start hashcards with --config.",
            )
            .redirect("/hedgedoc");
```

3. Duplicate URL pre-check (`:564`):

```rust
            return Flash::error("This note is already added.").redirect("/hedgedoc");
```

4. Unparseable source URI (`:571-572`): keep the `log::error!`, replace the return with

```rust
            return Flash::error(format!("Could not parse a HedgeDoc note URL from: {url}"))
                .redirect("/hedgedoc");
```

5. `build_note` failure (`:591-592`): keep the `log::error!`, replace the return with

```rust
                return Flash::error(format!("Failed to add HedgeDoc note: {e}"))
                    .redirect("/hedgedoc");
```

6. `build_source` failure (`:599-600`): keep the `log::error!`, replace the return with

```rust
                return Flash::error(format!("Failed to add HedgeDoc source: {e}"))
                    .redirect("/hedgedoc");
```

7. Duplicate re-checks under the lock (`:615` and `:620`): replace both `return Redirect::to("/hedgedoc");` with

```rust
                return Flash::error("This note is already added.").redirect("/hedgedoc");
```

8. Minimal-config creation failure (`:648-649`): keep the `log::error!`, replace the return with

```rust
                    return Flash::error(format!("Failed to create config file: {e}"))
                        .redirect("/hedgedoc");
```

9. Persist failure (`:669-670`): keep the `log::error!`, replace the return with

```rust
        return Flash::error(format!("Failed to save HedgeDoc sources to config: {e}"))
            .redirect("/hedgedoc");
```

10. Final success return (`:699`): replace `Redirect::to("/hedgedoc")` with

```rust
    Flash::success("HedgeDoc source added.").redirect("/hedgedoc")
```

In `hedgedoc_delete_handler` (`handlers.rs:707-748`), replace the `persist_ok: bool` plumbing so the failure reaches the user. Replace everything from `let persist_ok = if let Some(config_path) = maybe_config_path {` through the final `Redirect::to("/hedgedoc")` with:

```rust
    let persist_result: Result<(), String> = if let Some(config_path) = maybe_config_path {
        let remaining_for_persist = remaining.clone();
        match tokio::task::spawn_blocking(move || {
            persist_hedgedoc_entries(&config_path, &remaining_for_persist)
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                log::error!("Failed to persist HedgeDoc entries after deletion: {e}");
                Err(e.to_string())
            }
            Err(e) => {
                log::error!("Persist task panicked after deletion: {e}");
                Err(e.to_string())
            }
        }
    } else {
        Ok(()) // no config file to persist to, treat as success
    };

    // Only update in-memory state if persist succeeded (or was not needed).
    match persist_result {
        Ok(()) => {
            *state.hedgedoc_sources.lock().unwrap() = new_sources.clone();
            let combined = build_combined_infos(&state.config.collections, &new_sources);
            *state.collections.write().await = combined;
            Flash::success("HedgeDoc source removed.").redirect("/hedgedoc")
        }
        Err(msg) => {
            Flash::error(format!("Failed to remove HedgeDoc source: {msg}")).redirect("/hedgedoc")
        }
    }
}
```

In `hedgedoc_sync_now_handler` (`handlers.rs:751-806`):

- No data dir (`:753`): replace `return Redirect::to("/hedgedoc");` with

```rust
        return Flash::error("HedgeDoc sync is not available: no data directory is configured.")
            .redirect("/hedgedoc");
```

- Final return (`:806`): replace `Redirect::to("/hedgedoc")` with

```rust
    if any_success || entries.is_empty() {
        Flash::success("HedgeDoc sync finished.").redirect("/hedgedoc")
    } else {
        Flash::error("HedgeDoc sync failed for all notes; see the statuses below.")
            .redirect("/hedgedoc")
    }
```

After these edits, verify no swallowed writes remain in the serve/drill handlers:

Run: `grep -n 'let _ =' src/cmd/serve/*.rs src/cmd/drill/*.rs`
Expected remaining matches only: `config.rs:176` (best-effort temp-dir cleanup on exit), `edit.rs:192` (BUG-36, separate PR), `hedgedoc.rs` temp-file cleanup lines (best-effort `remove_file` after a failure already being reported), `handlers.rs` `map.next_value::<serde::de::IgnoredAny>()` (deliberate form-field skip), `post.rs:99` (oneshot shutdown signal). None of these is a user-actionable fallible write. `bookmarks.rs:189` and `:214` must be gone.

- [x] **Step 6: Run tests to verify they pass**

Run: `cargo test surfaced_as_flash` then `cargo test`
Expected: both new tests PASS; full suite green.

- [x] **Step 7: Update CHANGELOG.xml (BUG-03)**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add:

```xml
            <change author="claude">
                Errors in web actions are no longer silently swallowed: failed grades and undos, bookmark removal and note saving, manual git sync, and all HedgeDoc add/delete/sync failures now show an error banner instead of redirecting as if they had succeeded.
            </change>
```

- [x] **Step 8: Commit**

```bash
cargo fmt
git add src/cmd/serve CHANGELOG.xml
git commit -m "fix: surface all swallowed serve-mode errors via flash (BUG-03)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_012SzQd3T2Dbb4nr1FGnFjTW"
```

---

### Task 8: BUG-51a — `stats` defaults to JSON; `--format html` is an error

`src/cmd/stats.rs:45-47` prints "HTML output is not implemented yet." to stderr and exits 0; `src/cli.rs:80` makes this broken format the default. Fix per spec: make `json` the default, and make `html` a real error (until FEAT-02 lands).

**Files:**
- Modify: `src/cli.rs:80` and new tests module in `src/cli.rs`
- Modify: `src/cmd/stats.rs:42-54` and its tests module

**Interfaces:**
- Consumes: `fail` from `crate::error`, `create_tmp_copy_of_test_directory` from `crate::helper`.
- Produces: nothing other tasks use.

- [x] **Step 1: Write the failing regression tests**

In `src/cmd/stats.rs` `mod tests`, add:

```rust
#[test]
fn test_print_stats_html_is_an_error() -> Fallible<()> {
    let directory = create_tmp_copy_of_test_directory()?;
    let result = print_stats(Some(directory), StatsFormat::Html);
    assert!(result.is_err());
    Ok(())
}
```

At the end of `src/cli.rs`, add a tests module (`Command` is private to `cli.rs`, so the test lives there; `clap::Parser` is already imported at the top of the file):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_default_format_is_json() {
        let cmd = Command::try_parse_from(["hashcards", "stats"]).unwrap();
        match cmd {
            Command::Stats { format, .. } => {
                assert!(matches!(format, StatsFormat::Json));
            }
            _ => panic!("expected the stats command"),
        }
    }
}
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test test_print_stats_html_is_an_error test_stats_default_format_is_json` (i.e. `cargo test stats_`)
Expected: `test_print_stats_html_is_an_error` FAILS (`print_stats` returns `Ok`); `test_stats_default_format_is_json` FAILS (default is `Html`).

- [x] **Step 3: Implement**

`src/cli.rs:80` — change the default:

```rust
        #[arg(long, default_value_t = StatsFormat::Json)]
        format: StatsFormat,
```

`src/cmd/stats.rs` — add `use crate::error::fail;` to the imports and replace the `Html` arm in `print_stats` (`stats.rs:45-47`):

```rust
        StatsFormat::Html => {
            return fail("HTML output is not implemented yet. Use --format json.");
        }
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test stats_` then `cargo test`
Expected: PASS; full suite green.

- [x] **Step 5: Update CHANGELOG.xml (BUG-51, stats item)**

In `CHANGELOG.xml`, inside `<unreleased><changed>`, add:

```xml
            <change author="claude">
                `hashcards stats` now defaults to JSON output. Requesting `--format html` is an error (it was never implemented and previously printed a note to stderr while exiting successfully).
            </change>
```

- [x] **Step 6: Commit**

```bash
cargo fmt
git add src/cli.rs src/cmd/stats.rs CHANGELOG.xml
git commit -m "fix: make stats default to json and error on html (BUG-51)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_012SzQd3T2Dbb4nr1FGnFjTW"
```

---

### Task 9: BUG-51b — warn about malformed `macros.tex` lines

`src/collection.rs:69-79`: a non-comment line without a space separator is silently dropped (`None => {}`). Fix: extract macro parsing into a testable function that reports malformed line numbers; the caller logs a warning per malformed line, naming the file and 1-based line number.

**Files:**
- Modify: `src/collection.rs:63-82` (macros block; new `parse_macros` function; new tests module)

**Interfaces:**
- Consumes: `log::warn!`.
- Produces: `fn parse_macros(content: &str) -> (Vec<(String, String)>, Vec<usize>)` (private to `collection.rs`).

- [x] **Step 1: Write the failing regression test**

`src/collection.rs` has no tests module; add one at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_macros_reports_malformed_lines() {
        let content = "\\foo bar\n% comment\n\nnospace\n\\baz qux quux\n";
        let (macros, malformed) = parse_macros(content);
        assert_eq!(
            macros,
            vec![
                ("\\foo".to_string(), "bar".to_string()),
                ("\\baz".to_string(), "qux quux".to_string()),
            ]
        );
        // Line 4 ("nospace") has no space separator; comment and blank
        // lines are not malformed.
        assert_eq!(malformed, vec![4]);
    }
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test test_parse_macros_reports_malformed_lines`
Expected: COMPILE ERROR (`parse_macros` not found).

- [x] **Step 3: Implement**

In `src/collection.rs`, add the function (below the `impl Collection` block):

```rust
/// Parse the contents of `macros.tex` into `(name, definition)` pairs.
///
/// Returns the parsed macros and the 1-based line numbers of malformed
/// lines: non-comment, non-blank lines without a space separating the
/// macro name from its definition. Comment lines start with `%`.
fn parse_macros(content: &str) -> (Vec<(String, String)>, Vec<usize>) {
    let mut macros = Vec::new();
    let mut malformed = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('%') {
            continue;
        }
        match line.split_once(' ') {
            Some((name, definition)) => {
                macros.push((name.to_string(), definition.to_string()));
            }
            None => malformed.push(index + 1),
        }
    }
    (macros, malformed)
}
```

Replace the macros block inside `with_db_path` (`collection.rs:63-82`):

```rust
        let macros: Vec<(String, String)> = {
            let macros_path = directory.join("macros.tex");
            if macros_path.exists() {
                let content = read_to_string(&macros_path)?;
                let (parsed, malformed) = parse_macros(&content);
                for line_no in malformed {
                    log::warn!(
                        "{}: line {line_no} is not a valid macro definition (expected a name and a definition separated by a space); line ignored.",
                        macros_path.display()
                    );
                }
                parsed
            } else {
                Vec::new()
            }
        };
```

(Behavior note: whitespace-only lines previously produced a garbage `("", "")` macro via `split_once(' ')` on `"  "`; the new blank-line skip fixes that in passing.)

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test test_parse_macros_reports_malformed_lines` then `cargo test`
Expected: PASS (the existing `test_get_stats` asserts `tex_macro_count == 1` and must still pass); full suite green.

- [x] **Step 5: Update CHANGELOG.xml (BUG-51, macros item)**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add:

```xml
            <change author="claude">
                Malformed lines in `macros.tex` (lines without a space between the macro name and its definition) are now reported as warnings with their line number instead of being silently ignored.
            </change>
```

- [x] **Step 6: Commit**

```bash
cargo fmt
git add src/collection.rs CHANGELOG.xml
git commit -m "fix: warn about malformed macros.tex lines (BUG-51)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_012SzQd3T2Dbb4nr1FGnFjTW"
```

---

### Task 10: BUG-51c — styled drill error page with proper status code and a way back

The drill error page (`src/cmd/drill/get.rs`, `get_handler` error arm) returns `StatusCode::OK`, `div.error` has no CSS, and the page has no link back. Fix: 500 status, `.error` styling, and a "Back to session" link (drill mode's only page is `/`).

**Files:**
- Modify: `src/cmd/drill/get.rs` (`get_handler` error arm; new `error_response` function; tests module from Task 5)
- Modify: `src/cmd/drill/style.css` (append `.error` styles)

**Interfaces:**
- Consumes: `page_template`, `ErrorReport` (`crate::error::ErrorReport`), get_handler shape from Task 2.
- Produces: `pub(crate) fn error_response(e: &ErrorReport) -> (StatusCode, Html<String>)` in `src/cmd/drill/get.rs`.

- [x] **Step 1: Write the failing regression test**

In the `src/cmd/drill/get.rs` tests module (created in Task 5), add `use crate::error::ErrorReport;` to the test imports and:

```rust
#[test]
fn test_error_response_is_styled_500_with_home_link() {
    let (status, Html(body)) = error_response(&ErrorReport::new("kaboom"));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains("class=\"error\""));
    assert!(body.contains("kaboom"));
    assert!(body.contains("href=\"/\""), "body: {body}");
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test test_error_response_is_styled_500_with_home_link`
Expected: COMPILE ERROR (`error_response` not found).

- [x] **Step 3: Implement**

In `src/cmd/drill/get.rs`, add `use crate::error::ErrorReport;` to the imports, add the function, and rewrite `get_handler` (as left by Task 2) to use it:

```rust
/// A styled error page with a 500 status and a link back to the session.
pub(crate) fn error_response(e: &ErrorReport) -> (StatusCode, Html<String>) {
    let html = page_template(html! {
        div.error {
            h1 { "Error" }
            p { (e) }
            p { a href="/" { "\u{2190} Back to session" } }
        }
    });
    (StatusCode::INTERNAL_SERVER_ERROR, Html(html.into_string()))
}

pub async fn get_handler(
    State(state): State<ServerState>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    match inner(state, flash).await {
        Ok(html) => (StatusCode::OK, Html(html.into_string())),
        Err(e) => error_response(&e),
    }
}
```

Append to `src/cmd/drill/style.css` (after the `.flash` rules from Task 1; nested CSS is used elsewhere in this file):

```css
.error {
    max-width: 800px;
    margin: 48px auto;
    padding: 24px;
    border: 1px solid #f3c5bd;
    border-radius: 8px;
    background: #fff4f1;
    color: #7a271a;

    h1 {
        margin-bottom: 12px;
    }

    p {
        margin-bottom: 8px;
    }

    a {
        color: inherit;
        font-weight: 600;
    }
}
```

(This also styles the pre-existing `div.error` pages in serve mode: `handlers.rs` `collection_get_handler` and `bookmarks.rs` `error_page`, which already set correct 404/500 status codes and have back links.)

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test test_error_response_is_styled_500_with_home_link` then `cargo test`
Expected: PASS; full suite green.

- [x] **Step 5: Update CHANGELOG.xml (BUG-51, error-page item)**

In `CHANGELOG.xml`, inside `<unreleased><fixed>`, add:

```xml
            <change author="claude">
                The drill-mode error page now returns HTTP 500 instead of 200, is styled, and has a link back to the session.
            </change>
```

- [x] **Step 6: Commit**

```bash
cargo fmt
git add src/cmd/drill/get.rs src/cmd/drill/style.css CHANGELOG.xml
git commit -m "fix: styled 500 drill error page with a way back (BUG-51)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_012SzQd3T2Dbb4nr1FGnFjTW"
```

---

### Task 11: BUG-51d — serve `mod.rs` test asserts post-redirect content

`src/cmd/serve/mod.rs` `test_start_with_multiple_decks` asserts only a 2xx after the redirect — which fires on success *and* failure (both redirect to `/collection/{slug}`). Strengthen it to assert the post-redirect page actually shows a running drill session. Here the strengthened assertion itself is the deliverable, so the "failing test" step verifies the new assertion is exercised rather than reproducing a bug.

**Files:**
- Modify: `src/cmd/serve/mod.rs` (tests module, `test_start_with_multiple_decks`)

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing.

- [x] **Step 1: Strengthen the test**

In `test_start_with_multiple_decks`, after the existing status assertion (keep it), add:

```rust
        // The redirect target must show the running drill session, not the
        // deck browser (the redirect alone fires on success and failure).
        let body = response.text().await?;
        assert!(
            body.contains("value=\"Reveal\""),
            "expected the post-redirect page to show the drill session, got: {body}"
        );
```

- [x] **Step 2: Verify the assertion is exercised and can fail**

Temporarily invert it to `assert!(!body.contains("value=\"Reveal\""), ...)`, run `cargo test test_start_with_multiple_decks`, confirm it FAILS, then restore the correct assertion. (This proves the session page really contains the marker and the assertion is not vacuous.)

- [x] **Step 3: Run the test to verify it passes**

Run: `cargo test test_start_with_multiple_decks` then `cargo test`
Expected: PASS; full suite green.

- [x] **Step 4: Commit**

Test-only change; no CHANGELOG entry (CHANGELOG records user-visible changes).

```bash
cargo fmt
git add src/cmd/serve/mod.rs
git commit -m "test: assert post-redirect content after starting a drill (BUG-51)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_012SzQd3T2Dbb4nr1FGnFjTW"
```

---

## Spec discrepancies

Verified against the working tree at commit `be55f80`. The plan follows the code, not the stale citations.

1. **BUG-51 cites `stats.rs:408-410`** — the file is 113 lines. The actual location is `src/cmd/stats.rs:45-47` (`eprintln!("HTML output is not implemented yet.")` inside `print_stats`), with the broken `html` default at `src/cli.rs:80`.
2. **BUG-51 cites `collection.rs:288-294`** — the file is 104 lines. The actual silent drop of malformed `macros.tex` lines is `src/collection.rs:69-79` (`None => {}` on `split_once(' ')`).
3. **BUG-51 cites the drill error page at `get.rs:56-63`** — the handler actually spans `src/cmd/drill/get.rs:54-64`; substance matches (`StatusCode::OK`, unstyled `div.error`, no link back).
4. **BUG-03's "nine HedgeDoc failure branches"** — the cited lines (`handlers.rs:495-499, :532, :551, :564, :571, :591, :599, :648, :669`) all check out (the cited line is the `log::error!`/branch head; the silent `return Redirect` is on the line after in several cases, and `:495-499` is `sync_handler`, not HedgeDoc). However, the count misses identically shaped silent branches the plan also fixes: `handlers.rs:615` and `:620` (duplicate re-checks under the lock in `hedgedoc_add_handler`), `:733-734` (persist failures in `hedgedoc_delete_handler`, only logged), `:753` (`hedgedoc_sync_now_handler` with no data dir), plus the log-and-redirect error arms of `collection_start_handler` (`:221`, fixed in Task 6) and `collection_post_handler` (`:350`, fixed in Task 7).
5. **BUG-05 cites `post.rs:140`** — the `End` arm is `src/cmd/drill/post.rs:140-143`, but the nonsense stats themselves are rendered in `src/cmd/drill/get.rs` (`render_completion_page`, line 241 onward). The spec's alternative suggestion to "use the session row's real start time" is moot: in drill mode the session row is created with the same `session_started_at` the stats already use (`src/cmd/drill/server.rs:168`), so the plan takes the spec's other option (skip the stats block when nothing was reviewed).
6. **BUG-11 (`post.rs:144-150`) and BUG-37 (`handlers.rs:267-276`)** — citations match the code exactly (`Action::Shutdown` arm; `deck_filter.is_empty()` fallback at `handlers.rs:267-277`).
7. **Serve `mod.rs:32-87` test citation** — matches (`test_start_with_multiple_decks` spans lines 32-87).
8. **Percent-encoding** — `percent-encoding` 2.3.2 is already a dependency (`Cargo.toml`) and already used in `src/markdown.rs` and `src/media/`; no new dependency and no hand-rolled encode/decode helper is needed. One design consequence baked into the mandated API: axum's `Query` extractor percent-decodes values before `Flash::from_query` sees them, so encoding lives in `redirect` and decoding is the extractor's job.
9. **FEAT-01's "or short-lived cookie"** — the spec allows either mechanism; the mandated interface for this plan pins the query-param variant. Consequence: reloading the redirected-to URL re-shows the banner (it is one-shot in the sense of not being persisted server-side). Accepted trade-off; noted in `src/flash.rs` docs.
10. **`ErrorReport::Display` prefixes `"error: "`** (`src/error.rs:118`), so flashes built from `e.to_string()` read "error: Select at least one deck." — the spec's message text ("select at least one deck") still appears verbatim within it; BUG-21 (separate PR) is the place where that prefix gets cleaned up.
