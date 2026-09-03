# Local Collections, Markdown Editor, and Git Sources — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the user a writable markdown tree managed entirely in the browser — folders as collections, a document editor with card-insertion buttons and a live parse preview — and let a git file URL be added as a source exactly like a HedgeDoc note.

**Architecture:** Three source kinds (`hedgedoc`, `git`, `local`) divided by whether hashcards owns the bytes. The two remote kinds reuse the existing HedgeDoc fetch pipeline with one substituted step (`raw_url`). The local kind is a new per-user root at `{data_dir}/local/{user}` whose top-level folders are auto-discovered as collections, each carrying a stable id so renames keep review history. The editor reuses `edit.rs` (atomic write, revert, hash migration) and previews through the production parser.

**Tech Stack:** Rust, axum 0.8, maud 0.27 templates, tokio, reqwest, toml, blake3, rusqlite. Tests are inline `#[cfg(test)] mod tests` blocks — there is no `tests/` directory.

**Spec:** `docs/superpowers/specs/2026-09-03-local-collections-and-editor-design.md`

## Global Constraints

- No `unwrap()` in production code. Tests may use it freely.
- Use `Fallible<T>` and `?` for error handling; build custom errors with `fail()`.
- All error messages are user-facing: name the thing that went wrong and what to do about it.
- Use newtypes for domain concepts.
- Prefer `use foo::bar;` imports over fully qualified `foo::bar()` calls.
- Keep functions small and focused; module files re-export what's needed and hide the rest.
- Dates are naive — no timezones.
- Card frontmatter is **TOML** (`name = "..."`), not YAML.
- Cloze positions are **byte** positions: use `.bytes()`, never `.chars()`.
- When fixing a bug, add the failing regression test first.
- No new crate dependencies. Ids use `blake3`, already in `Cargo.toml`.
- Every task ends green: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`.

## File Structure

| File | Responsibility |
|---|---|
| `src/cmd/serve/source.rs` | **new** — `SourceKind`, URL detection, `raw_url` rewriting |
| `src/cmd/serve/local.rs` | **new** — `LocalRoot` newtype, path validation, `.hashcards.toml` ids, collection discovery |
| `src/cmd/serve/files.rs` | **new** — file-manager and editor handlers |
| `src/cmd/serve/files_ui.rs` | **new** — maud templates for the tree page and the editor |
| `src/cmd/serve/hedgedoc.rs` | modify — `fetch_markdown` delegates to `raw_url`; `[[source]]` persistence |
| `src/cmd/serve/config.rs` | modify — `SourceEntry`, `[[hedgedoc]]` alias, local root wiring |
| `src/cmd/serve/hedgedoc_ui.rs` | modify — kind badge, template block, "Sources" heading |
| `src/cmd/serve/server.rs` | modify — route registration |
| `src/cmd/serve/mod.rs` | modify — declare new modules |
| `README.md`, `CHANGELOG.xml` | modify — document `[[source]]`, `/files`, the editor |

Tasks 1–2 are the git-source slice and stand alone. Tasks 3–4 build local storage. Tasks 5–7 build the UI on top. Task 8 unifies the Sources page. Task 9 documents.

---

### Task 1: Source kinds and raw-URL rewriting

**Files:**
- Create: `src/cmd/serve/source.rs`
- Modify: `src/cmd/serve/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub enum SourceKind { Hedgedoc, Git }` with `pub fn label(&self) -> &'static str`; `pub fn detect_kind(url: &str) -> SourceKind`; `pub fn raw_url(url: &str) -> Fallible<(SourceKind, reqwest::Url)>`.

- [ ] **Step 1: Declare the module**

In `src/cmd/serve/mod.rs`, add alongside the existing `pub mod` lines:

```rust
pub mod source;
```

- [ ] **Step 2: Write the failing tests**

Create `src/cmd/serve/source.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_blob_becomes_raw() {
        let (kind, url) = raw_url("https://github.com/me/cards/blob/main/es/verbs.md").unwrap();
        assert_eq!(kind, SourceKind::Git);
        assert_eq!(
            url.as_str(),
            "https://raw.githubusercontent.com/me/cards/main/es/verbs.md"
        );
    }

    #[test]
    fn gitlab_blob_becomes_raw() {
        let (kind, url) = raw_url("https://gitlab.com/me/cards/-/blob/main/es/verbs.md").unwrap();
        assert_eq!(kind, SourceKind::Git);
        assert_eq!(
            url.as_str(),
            "https://gitlab.com/me/cards/-/raw/main/es/verbs.md"
        );
    }

    #[test]
    fn gitea_src_branch_becomes_raw() {
        let (kind, url) =
            raw_url("https://codeberg.org/me/cards/src/branch/main/es/verbs.md").unwrap();
        assert_eq!(kind, SourceKind::Git);
        assert_eq!(
            url.as_str(),
            "https://codeberg.org/me/cards/raw/branch/main/es/verbs.md"
        );
    }

    #[test]
    fn already_raw_md_url_is_unchanged() {
        let (kind, url) = raw_url("https://example.com/some/where/verbs.md").unwrap();
        assert_eq!(kind, SourceKind::Git);
        assert_eq!(url.as_str(), "https://example.com/some/where/verbs.md");
    }

    #[test]
    fn hedgedoc_note_is_not_git() {
        assert_eq!(
            detect_kind("https://notes.example.com/abc123"),
            SourceKind::Hedgedoc
        );
        assert_eq!(
            detect_kind("https://notes.example.com/s/abc123"),
            SourceKind::Hedgedoc
        );
    }

    #[test]
    fn query_and_fragment_are_dropped() {
        let (_, url) = raw_url("https://github.com/me/cards/blob/main/a.md?plain=1#L3").unwrap();
        assert_eq!(
            url.as_str(),
            "https://raw.githubusercontent.com/me/cards/main/a.md"
        );
    }

    #[test]
    fn non_https_is_rejected() {
        assert!(raw_url("http://github.com/me/cards/blob/main/a.md").is_err());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib cmd::serve::source`
Expected: FAIL to compile — `raw_url`, `detect_kind` and `SourceKind` do not exist.

- [ ] **Step 4: Implement the module**

Put this **above** the test module in `src/cmd/serve/source.rs`:

```rust
use reqwest::Url;

use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::error::fail;

/// Where a source's markdown comes from.
///
/// Local collections are not represented here: they have no URL to fetch,
/// so they never reach this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Hedgedoc,
    Git,
}

impl SourceKind {
    pub fn label(&self) -> &'static str {
        match self {
            SourceKind::Hedgedoc => "hedgedoc",
            SourceKind::Git => "git",
        }
    }
}

/// Which kind of source a pasted URL names.
///
/// Recognises the three common forge "view a file" URL shapes, then falls
/// back on the path extension: a URL ending in `.md` is a raw markdown file,
/// anything else is a HedgeDoc note. HedgeDoc note URLs are note ids and
/// never carry an extension, so the two cannot be confused.
pub fn detect_kind(url: &str) -> SourceKind {
    match Url::parse(url) {
        Ok(parsed) => match git_raw_url(&parsed) {
            Some(_) => SourceKind::Git,
            None => SourceKind::Hedgedoc,
        },
        Err(_) => SourceKind::Hedgedoc,
    }
}

/// The kind of a pasted URL, and the URL its markdown is actually fetched
/// from. HedgeDoc URLs come back unchanged — `hedgedoc::fetch_markdown`
/// appends `/download` itself, because that rewrite also has to strip the
/// `/s/` prefix of a published note.
pub fn raw_url(url: &str) -> Fallible<(SourceKind, Url)> {
    let mut parsed =
        Url::parse(url).map_err(|e| ErrorReport::new(format!("Invalid source URL `{url}`: {e}")))?;
    if parsed.scheme() != "https" {
        return fail(format!("Source URLs must use HTTPS (got: {url})"));
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    match git_raw_url(&parsed) {
        Some(raw) => Ok((SourceKind::Git, raw)),
        None => Ok((SourceKind::Hedgedoc, parsed)),
    }
}

/// The raw-content URL for a forge "view a file" URL, or `None` when this is
/// not one.
fn git_raw_url(parsed: &Url) -> Option<Url> {
    let host = parsed.host_str()?;
    let segments: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();

    // github.com/{owner}/{repo}/blob/{ref}/{path...}
    //   -> raw.githubusercontent.com/{owner}/{repo}/{ref}/{path...}
    if host == "github.com" && segments.len() >= 5 && segments[2] == "blob" {
        let mut raw = parsed.clone();
        raw.set_host(Some("raw.githubusercontent.com")).ok()?;
        let rest = [&segments[0..2], &segments[3..]].concat();
        raw.set_path(&rest.join("/"));
        return Some(raw);
    }

    // {host}/{owner}/{repo}/-/blob/{ref}/{path...}  (GitLab)
    //   -> same host, /-/raw/
    if let Some(dash) = segments.iter().position(|s| *s == "-") {
        if segments.len() > dash + 2 && segments[dash + 1] == "blob" {
            let mut raw = parsed.clone();
            let mut rest = segments.clone();
            rest[dash + 1] = "raw";
            raw.set_path(&rest.join("/"));
            return Some(raw);
        }
    }

    // {host}/{owner}/{repo}/src/branch/{branch}/{path...}  (Gitea, Forgejo)
    //   -> same host, /raw/branch/
    if segments.len() >= 6 && segments[2] == "src" && segments[3] == "branch" {
        let mut raw = parsed.clone();
        let rest = [&segments[0..2], &["raw"][..], &segments[3..]].concat();
        raw.set_path(&rest.join("/"));
        return Some(raw);
    }

    // Any other URL that names a markdown file is already raw.
    if segments.last().is_some_and(|s| s.ends_with(".md")) {
        return Some(parsed.clone());
    }

    None
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib cmd::serve::source`
Expected: PASS, 7 tests.

- [ ] **Step 6: Check lints and formatting**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add src/cmd/serve/source.rs src/cmd/serve/mod.rs
git commit -m "feat: detect git file sources and rewrite forge URLs to raw"
```

---

### Task 2: Fetch git sources, and accept `[[source]]` in the config

**Files:**
- Modify: `src/cmd/serve/hedgedoc.rs` (`fetch_markdown`, `persist_hedgedoc_entries`)
- Modify: `src/cmd/serve/config.rs` (`HedgedocEntry` → `SourceEntry`, `ServeConfig`)

**Interfaces:**
- Consumes: `SourceKind`, `raw_url` from Task 1.
- Produces: `pub struct SourceEntry { pub url: String, pub owner: Option<String> }` (the former `HedgedocEntry`); `ServeConfig::source_entries(&self) -> Vec<SourceEntry>`; `persist_source_entries(config_path: &Path, entries: &[SourceEntry]) -> Fallible<()>` writing `[[source]]`.

- [ ] **Step 1: Write the failing config test**

Add to the `mod tests` block in `src/cmd/serve/config.rs`:

```rust
#[test]
fn source_and_hedgedoc_arrays_both_parse() {
    let toml_text = r#"
        [server]
        data_dir = "/tmp/hc"

        [[source]]
        url = "https://github.com/me/cards/blob/main/a.md"

        [[hedgedoc]]
        url = "https://notes.example.com/abc123"
    "#;
    let parsed: ServeConfig = toml::from_str(toml_text).unwrap();
    let entries = parsed.source_entries();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].url, "https://github.com/me/cards/blob/main/a.md");
    assert_eq!(entries[1].url, "https://notes.example.com/abc123");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib cmd::serve::config::tests::source_and_hedgedoc_arrays_both_parse`
Expected: FAIL to compile — no `source_entries` method, no `[[source]]` field.

- [ ] **Step 3: Rename the entry type and add the alias**

In `src/cmd/serve/config.rs`, rename `HedgedocEntry` to `SourceEntry` and keep the old name as an alias so no other module has to change in this step:

```rust
#[derive(Deserialize, Serialize, Clone)]
pub struct SourceEntry {
    pub url: String,
    #[serde(default)]
    pub owner: Option<String>,
}

/// Former name of [`SourceEntry`], kept so existing call sites compile.
pub type HedgedocEntry = SourceEntry;
```

In `ServeConfig`, replace the single `hedgedoc` field with both arrays:

```rust
    #[serde(rename = "source", default)]
    pub sources: Vec<SourceEntry>,
    /// Deprecated spelling of `[[source]]`, still accepted so existing
    /// config files keep working. New entries are written as `[[source]]`.
    #[serde(rename = "hedgedoc", default)]
    pub hedgedoc: Vec<SourceEntry>,
```

Add the merging accessor, `[[source]]` entries first:

```rust
impl ServeConfig {
    /// Every configured remote source, from both the current `[[source]]`
    /// spelling and the deprecated `[[hedgedoc]]` one.
    pub fn source_entries(&self) -> Vec<SourceEntry> {
        let mut entries = self.sources.clone();
        entries.extend(self.hedgedoc.iter().cloned());
        entries
    }
}
```

In `ResolvedServeConfig::from_toml`, replace every read of `config.hedgedoc` with `config.source_entries()`. Leave the `hedgedoc_entries` field name on `ResolvedServeConfig` alone — Task 8 renames it.

- [ ] **Step 4: Run the config test to verify it passes**

Run: `cargo test --lib cmd::serve::config`
Expected: PASS.

- [ ] **Step 5: Write the failing persistence test**

Add to the `mod tests` block in `src/cmd/serve/hedgedoc.rs`:

```rust
#[test]
fn persist_writes_source_arrays_and_drops_the_old_spelling() -> Fallible<()> {
    let dir = crate::helper::create_tmp_directory()?;
    let config_path = dir.join("hashcards.toml");
    std::fs::write(
        &config_path,
        "[server]\ndata_dir = \"/tmp/hc\"\n\n[[hedgedoc]]\nurl = \"https://old.example.com/a\"\n",
    )?;

    let entries = vec![SourceEntry {
        url: "https://github.com/me/cards/blob/main/a.md".to_string(),
        owner: None,
    }];
    persist_source_entries(&config_path, &entries)?;

    let written = std::fs::read_to_string(&config_path)?;
    assert!(written.contains("[[source]]"));
    assert!(!written.contains("[[hedgedoc]]"));
    assert!(written.contains("https://github.com/me/cards/blob/main/a.md"));
    Ok(())
}
```

- [ ] **Step 6: Run it to verify it fails**

Run: `cargo test --lib cmd::serve::hedgedoc::tests::persist_writes_source_arrays`
Expected: FAIL — `persist_source_entries` does not exist.

- [ ] **Step 7: Rename the persistence function**

In `src/cmd/serve/hedgedoc.rs`, rename `persist_hedgedoc_entries` to `persist_source_entries` and change the TOML table it writes from `hedgedoc` to `source`. It must also **remove** any existing `hedgedoc` array from the document, or a config that had one ends up with entries in both places and every source loads twice. Update the call sites in `commit_add` and `commit_delete`.

- [ ] **Step 8: Write the failing fetch-target tests**

Add to the `mod tests` block in `src/cmd/serve/hedgedoc.rs`:

```rust
#[test]
fn fetch_target_for_git_url_is_the_raw_url() -> Fallible<()> {
    let (kind, url) = fetch_target("https://github.com/me/cards/blob/main/a.md")?;
    assert_eq!(kind, SourceKind::Git);
    assert_eq!(
        url.as_str(),
        "https://raw.githubusercontent.com/me/cards/main/a.md"
    );
    Ok(())
}

#[test]
fn fetch_target_for_hedgedoc_url_appends_download() -> Fallible<()> {
    let (kind, url) = fetch_target("https://notes.example.com/abc123")?;
    assert_eq!(kind, SourceKind::Hedgedoc);
    assert_eq!(url.as_str(), "https://notes.example.com/abc123/download");
    Ok(())
}
```

- [ ] **Step 9: Run them to verify they fail**

Run: `cargo test --lib cmd::serve::hedgedoc::tests::fetch_target`
Expected: FAIL — `fetch_target` does not exist.

- [ ] **Step 10: Route the fetch through `raw_url`**

In `src/cmd/serve/hedgedoc.rs`, add the imports:

```rust
use crate::cmd::serve::source::SourceKind;
use crate::cmd::serve::source::raw_url;
```

Add the target helper:

```rust
/// The kind of a source URL and the URL its markdown is fetched from.
///
/// Git file URLs are rewritten to their forge's raw endpoint. HedgeDoc note
/// URLs get `/download` appended, which also strips the `/s/` prefix of a
/// published note.
fn fetch_target(url: &str) -> Fallible<(SourceKind, reqwest::Url)> {
    let (kind, rewritten) = raw_url(url)?;
    match kind {
        SourceKind::Git => Ok((kind, rewritten)),
        SourceKind::Hedgedoc => Ok((kind, build_download_url(url)?)),
    }
}
```

Rewrite `fetch_markdown` to use it, making the HTML guard kind-aware:

```rust
pub async fn fetch_markdown(url: &str) -> Fallible<String> {
    let (kind, download_url) = fetch_target(url)?;
    let client = http_client()?;
    let response = client.get(download_url.clone()).send().await?;
    if !response.status().is_success() {
        return fail(format!(
            "Source fetch returned HTTP {} for {}",
            response.status(),
            download_url
        ));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if content_type.starts_with("text/html") {
        return fail(match kind {
            SourceKind::Git => format!(
                "{download_url} returned a web page instead of markdown — the \
                 repository may be private, or the branch or path may not exist"
            ),
            SourceKind::Hedgedoc => format!(
                "HedgeDoc returned an HTML page for {download_url} — the note's \
                 permission level may require sign-in to download (try setting \
                 the note to \"Freely\" or \"Editable\" in HedgeDoc)"
            ),
        });
    }
    Ok(response.text().await?)
}
```

`raw_url` performs the HTTPS check, so drop the `validate_hedgedoc_url(url)?` line that opened the old body.

- [ ] **Step 11: Run the whole suite**

Run: `cargo test`
Expected: PASS. If a `normalize_hedgedoc_url` test fails on error text, update the assertion to the new "Source URLs must use HTTPS" wording — the behaviour is unchanged.

- [ ] **Step 12: Commit**

```bash
git add src/cmd/serve/hedgedoc.rs src/cmd/serve/config.rs
git commit -m "feat: fetch git file sources and accept [[source]] config entries"
```

---

### Task 3: The local root and safe path resolution

**Files:**
- Create: `src/cmd/serve/local.rs`
- Modify: `src/cmd/serve/mod.rs`

**Interfaces:**
- Consumes: `slugify` from `config.rs`, `ensure_dir` from `utils.rs`.
- Produces: `pub struct LocalRoot`; `LocalRoot::for_user(data_dir: &Path, owner: Option<&str>) -> Fallible<LocalRoot>`; `LocalRoot::path(&self) -> &Path`; `LocalRoot::resolve(&self, rel: &str) -> Fallible<PathBuf>`.

`resolve` deliberately differs from `MediaLoader::validate` in `src/media/load.rs`: the final component need **not** exist, because the file manager calls it to create files and folders. Everything above the leaf must exist and stay inside the root.

- [ ] **Step 1: Declare the module**

In `src/cmd/serve/mod.rs`:

```rust
pub mod local;
```

- [ ] **Step 2: Write the failing tests**

Create `src/cmd/serve/local.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::create_tmp_directory;

    /// `create_tmp_directory` returns a `PathBuf`, not a `TempDir`.
    fn fixture() -> Fallible<(PathBuf, LocalRoot)> {
        let dir = create_tmp_directory()?;
        let root = LocalRoot::for_user(&dir, Some("Me@Example.com"))?;
        Ok((dir, root))
    }

    #[test]
    fn user_dir_is_slugified_and_lowercased() -> Fallible<()> {
        let (dir, root) = fixture()?;
        assert_eq!(root.path(), dir.join("local").join("me-example.com"));
        Ok(())
    }

    #[test]
    fn anonymous_user_gets_the_default_tree() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        let root = LocalRoot::for_user(&dir, None)?;
        assert_eq!(root.path(), dir.join("local").join("default"));
        Ok(())
    }

    #[test]
    fn resolves_a_path_that_does_not_exist_yet() -> Fallible<()> {
        let (_dir, root) = fixture()?;
        let resolved = root.resolve("Spanish/verbs.md")?;
        assert_eq!(resolved, root.path().join("Spanish").join("verbs.md"));
        Ok(())
    }

    #[test]
    fn rejects_parent_components() -> Fallible<()> {
        let (_dir, root) = fixture()?;
        assert!(root.resolve("../escape.md").is_err());
        assert!(root.resolve("Spanish/../../escape.md").is_err());
        Ok(())
    }

    #[test]
    fn rejects_absolute_paths() -> Fallible<()> {
        let (_dir, root) = fixture()?;
        assert!(root.resolve("/etc/passwd").is_err());
        Ok(())
    }

    #[test]
    fn rejects_empty_paths() -> Fallible<()> {
        let (_dir, root) = fixture()?;
        assert!(root.resolve("").is_err());
        assert!(root.resolve("   ").is_err());
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn rejects_escape_through_a_symlinked_directory() -> Fallible<()> {
        let (dir, root) = fixture()?;
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside)?;
        std::os::unix::fs::symlink(&outside, root.path().join("link"))?;
        assert!(root.resolve("link/evil.md").is_err());
        Ok(())
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib cmd::serve::local`
Expected: FAIL to compile — `LocalRoot` does not exist.

- [ ] **Step 4: Implement `LocalRoot`**

Put this above the test module in `src/cmd/serve/local.rs`:

```rust
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use crate::cmd::serve::config::slugify;
use crate::error::Fallible;
use crate::error::fail;
use crate::utils::ensure_dir;

/// One user's writable markdown tree, at `{data_dir}/local/{user}`.
///
/// Deliberately outside `{data_dir}/repo`: `clone_or_pull` may hard-update
/// that directory and source sync overwrites the files it owns, so keeping
/// user writing in a separate root makes "sync cannot clobber your work" a
/// property of the layout rather than a rule to remember.
pub struct LocalRoot {
    root: PathBuf,
}

impl LocalRoot {
    /// The tree belonging to `owner`, creating it if absent. `None` is the
    /// shared `default` tree used when `[oidc]` is not configured.
    pub fn for_user(data_dir: &Path, owner: Option<&str>) -> Fallible<Self> {
        let who = match owner {
            Some(email) => slugify(&email.to_lowercase()),
            None => "default".to_string(),
        };
        if who.is_empty() {
            return fail("Cannot open a local card folder: the owner name is empty.");
        }
        let root = data_dir.join("local").join(who);
        ensure_dir(&root, "local card directory")?;
        Ok(Self { root })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Resolve a client-supplied relative path inside this tree.
    ///
    /// Unlike `MediaLoader::validate`, the final component need not exist —
    /// the file manager calls this to create files and folders. The deepest
    /// ancestor that *does* exist must canonicalize to somewhere inside the
    /// root, so a symlinked directory cannot be used to escape.
    pub fn resolve(&self, rel: &str) -> Fallible<PathBuf> {
        let trimmed = rel.trim();
        if trimmed.is_empty() {
            return fail("No file path was given.");
        }
        let rel_path = PathBuf::from(trimmed);
        if rel_path.components().any(|c| c == Component::ParentDir) {
            return fail(format!("Path must not contain `..` components: `{trimmed}`"));
        }
        if rel_path.is_absolute() || rel_path.has_root() {
            return fail(format!(
                "Path must be relative to your card folder: `{trimmed}`"
            ));
        }
        let joined = self.root.join(&rel_path);

        // Walk up to the deepest ancestor that exists and canonicalize that:
        // the leaf may legitimately be missing.
        let mut existing = joined.as_path();
        while !existing.exists() {
            existing = match existing.parent() {
                Some(p) => p,
                None => return fail(format!("Path is outside your card folder: `{trimmed}`")),
            };
        }
        if existing.is_symlink() {
            return fail(format!("Path goes through a symbolic link: `{trimmed}`"));
        }
        let canonical_root = self.root.canonicalize()?;
        let canonical = existing.canonicalize()?;
        if !canonical.starts_with(&canonical_root) {
            return fail(format!("Path is outside your card folder: `{trimmed}`"));
        }
        Ok(joined)
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib cmd::serve::local`
Expected: PASS, 7 tests.

- [ ] **Step 6: Check lints and formatting**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add src/cmd/serve/local.rs src/cmd/serve/mod.rs
git commit -m "feat: add per-user local card root with traversal-safe path resolution"
```

---

### Task 4: Stable collection ids and local collection discovery

**Files:**
- Modify: `src/cmd/serve/local.rs`
- Modify: `src/cmd/serve/config.rs` (call discovery from `from_toml`)

**Interfaces:**
- Consumes: `LocalRoot` from Task 3; `ResolvedCollection { name, slug, coll_dir, db_path, owner }` and `check_slug_collisions` from `config.rs`.
- Produces: `pub const LOCAL_META_FILE: &str = ".hashcards.toml"`; `pub fn collection_id(folder: &Path) -> Fallible<String>`; `pub fn discover_local_collections(root: &LocalRoot, db_dir: &Path, owner: Option<&str>) -> Fallible<Vec<ResolvedCollection>>`.

The id exists because databases are named `{db_dir}/{id}.db`. If they were named from the slug, renaming `Spanish/` to `Español/` would orphan the database and silently discard every review of that collection.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `src/cmd/serve/local.rs`:

```rust
#[test]
fn collection_id_is_created_once_and_then_reused() -> Fallible<()> {
    let (_dir, root) = fixture()?;
    let folder = root.path().join("Spanish");
    std::fs::create_dir_all(&folder)?;

    let first = collection_id(&folder)?;
    let second = collection_id(&folder)?;
    assert_eq!(first, second);
    assert_eq!(first.len(), 8);
    assert!(folder.join(LOCAL_META_FILE).exists());
    Ok(())
}

#[test]
fn collection_id_survives_a_rename() -> Fallible<()> {
    let (_dir, root) = fixture()?;
    let before = root.path().join("Spanish");
    std::fs::create_dir_all(&before)?;
    let id = collection_id(&before)?;

    let after = root.path().join("Espanol");
    std::fs::rename(&before, &after)?;

    assert_eq!(collection_id(&after)?, id);
    Ok(())
}

#[test]
fn two_folders_get_different_ids() -> Fallible<()> {
    let (_dir, root) = fixture()?;
    let a = root.path().join("A");
    let b = root.path().join("B");
    std::fs::create_dir_all(&a)?;
    std::fs::create_dir_all(&b)?;
    assert_ne!(collection_id(&a)?, collection_id(&b)?);
    Ok(())
}

#[test]
fn discovery_returns_one_collection_per_top_level_folder() -> Fallible<()> {
    let (dir, root) = fixture()?;
    std::fs::create_dir_all(root.path().join("Spanish").join("nested"))?;
    std::fs::create_dir_all(root.path().join("Medicine"))?;
    std::fs::write(root.path().join("loose.md"), "Q: a\nA: b\n")?;

    let db_dir = dir.join("db");
    let mut found = discover_local_collections(&root, &db_dir, Some("me@example.com"))?;
    found.sort_by(|a, b| a.name.cmp(&b.name));

    assert_eq!(found.len(), 2);
    assert_eq!(found[0].name, "Medicine");
    assert_eq!(found[1].name, "Spanish");
    assert_eq!(found[1].owner, Some("me@example.com".to_string()));
    Ok(())
}

#[test]
fn discovered_db_path_is_named_from_the_id_not_the_slug() -> Fallible<()> {
    let (dir, root) = fixture()?;
    let folder = root.path().join("Spanish");
    std::fs::create_dir_all(&folder)?;
    let id = collection_id(&folder)?;

    let db_dir = dir.join("db");
    let found = discover_local_collections(&root, &db_dir, None)?;
    assert_eq!(found[0].db_path, db_dir.join(format!("{id}.db")));
    Ok(())
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib cmd::serve::local`
Expected: FAIL to compile — `collection_id`, `discover_local_collections` and `LOCAL_META_FILE` do not exist.

- [ ] **Step 3: Implement ids and discovery**

Add these imports at the top of `src/cmd/serve/local.rs`:

```rust
use std::fs::read_dir;
use std::fs::read_to_string;
use std::fs::write;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde::Serialize;

use crate::cmd::serve::config::ResolvedCollection;
```

Then add below `impl LocalRoot`:

```rust
/// Per-collection metadata file. Skipped by the parser (it is not `.md`)
/// and hidden from the file manager.
pub const LOCAL_META_FILE: &str = ".hashcards.toml";

#[derive(Deserialize, Serialize)]
struct LocalMeta {
    id: String,
}

/// The stable id of a local collection folder, creating it on first sight.
///
/// Review databases are named from this id rather than from the folder's
/// slug, so renaming a folder keeps its review history. Ids are derived from
/// the clock and the process id rather than from the name, precisely so that
/// a rename cannot change them.
pub fn collection_id(folder: &Path) -> Fallible<String> {
    let meta_path = folder.join(LOCAL_META_FILE);
    if meta_path.exists() {
        let text = read_to_string(&meta_path)?;
        let meta: LocalMeta = toml::from_str(&text)?;
        if !meta.id.is_empty() {
            return Ok(meta.id);
        }
    }
    let id = fresh_id(folder)?;
    let meta = LocalMeta { id: id.clone() };
    write(&meta_path, toml::to_string(&meta)?)?;
    Ok(id)
}

/// Eight hex characters derived from the clock, the process id and the
/// folder path. Not a hash of the name: renaming must not change it.
fn fresh_id(folder: &Path) -> Fallible<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| crate::error::ErrorReport::new(format!("System clock is before 1970: {e}")))?
        .as_nanos();
    let seed = format!("{nanos}:{}:{}", std::process::id(), folder.display());
    Ok(blake3::hash(seed.as_bytes()).to_hex()[..8].to_string())
}

/// Every top-level folder in `root`, as a collection.
///
/// Loose files directly under the root are ignored: a collection is a
/// folder, so that a file always belongs to exactly one.
pub fn discover_local_collections(
    root: &LocalRoot,
    db_dir: &Path,
    owner: Option<&str>,
) -> Fallible<Vec<ResolvedCollection>> {
    let mut collections = Vec::new();
    if !root.path().exists() {
        return Ok(collections);
    }
    for entry in read_dir(root.path())? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || path.is_symlink() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        let id = collection_id(&path)?;
        collections.push(ResolvedCollection {
            slug: slugify(&name),
            name,
            coll_dir: path,
            db_path: db_dir.join(format!("{id}.db")),
            owner: owner.map(|o| o.to_lowercase()),
        });
    }
    Ok(collections)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib cmd::serve::local`
Expected: PASS, 12 tests.

- [ ] **Step 5: Write the failing collision test**

Local slugs must not shadow configured collections. Add to `src/cmd/serve/local.rs` tests:

```rust
#[test]
fn local_slugs_are_checked_against_configured_collections() -> Fallible<()> {
    let (dir, root) = fixture()?;
    std::fs::create_dir_all(root.path().join("Spanish"))?;
    let db_dir = dir.join("db");

    let mut all = vec![ResolvedCollection {
        name: "Spanish".to_string(),
        slug: "Spanish".to_string(),
        coll_dir: dir.join("repo").join("es"),
        db_path: db_dir.join("Spanish.db"),
        owner: None,
    }];
    all.extend(discover_local_collections(&root, &db_dir, None)?);

    assert!(crate::cmd::serve::config::check_slug_collisions(&all).is_err());
    Ok(())
}
```

- [ ] **Step 6: Run it**

Run: `cargo test --lib cmd::serve::local::tests::local_slugs_are_checked`
Expected: FAIL if `check_slug_collisions` is private — make it `pub(crate)` in `src/cmd/serve/config.rs`. Then PASS.

- [ ] **Step 7: Wire discovery into config resolution**

In `src/cmd/serve/config.rs`, add the imports
`use crate::cmd::serve::local::LocalRoot;` and
`use crate::cmd::serve::local::discover_local_collections;`, then add a field to
`ResolvedServeConfig`:

```rust
    /// Collections discovered under `{data_dir}/local/{user}`. Refreshed
    /// whenever the file manager changes the tree.
    pub local_collections: Vec<ResolvedCollection>,
```

In `from_toml`, after the configured `collections` are built and **before** `check_slug_collisions`, discover the local tree for the anonymous case and extend the list, so a local folder cannot shadow a configured collection:

```rust
        let local_root = LocalRoot::for_user(&data_dir, None)?;
        let local_collections = discover_local_collections(&local_root, &db_dir, None)?;
        let mut all = collections.clone();
        all.extend(local_collections.iter().cloned());
        check_slug_collisions(&all)?;
```

Under `[oidc]`, each request's collections are discovered per user in Task 5; this startup pass covers the anonymous tree only. Set `local_collections` in the returned struct.

- [ ] **Step 8: Run the whole suite**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src/cmd/serve/local.rs src/cmd/serve/config.rs
git commit -m "feat: discover local collections with rename-stable ids"
```

---

### Task 5: File manager — tree, create, rename, delete

**Files:**
- Create: `src/cmd/serve/files.rs`
- Create: `src/cmd/serve/files_ui.rs`
- Modify: `src/cmd/serve/mod.rs`, `src/cmd/serve/server.rs`

**Interfaces:**
- Consumes: `LocalRoot`, `discover_local_collections`, `LOCAL_META_FILE` from Tasks 3–4; `AppState`, `CurrentUser`, `Flash`, `page_template`.
- Produces: `pub const CARD_TEMPLATE: &str`; `pub struct TreeEntry { pub rel_path: String, pub name: String, pub is_dir: bool, pub depth: usize }`; `pub fn read_tree(root: &LocalRoot) -> Fallible<Vec<TreeEntry>>`; handlers `files_get_handler`, `files_folder_handler`, `files_file_handler`, `files_rename_handler`, `files_delete_handler`; `pub fn user_root(state: &AppState, user: Option<&CurrentUser>) -> Fallible<LocalRoot>`.

- [ ] **Step 1: Declare the modules**

In `src/cmd/serve/mod.rs`:

```rust
pub mod files;
pub mod files_ui;
```

- [ ] **Step 2: Write the failing tree tests**

Create `src/cmd/serve/files.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::serve::local::LocalRoot;
    use crate::helper::create_tmp_directory;

    #[test]
    fn tree_lists_folders_before_files_depth_first() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        let root = LocalRoot::for_user(&dir, None)?;
        std::fs::create_dir_all(root.path().join("Spanish"))?;
        std::fs::write(root.path().join("Spanish").join("verbs.md"), "Q: a\nA: b\n")?;
        std::fs::write(root.path().join("Spanish").join(LOCAL_META_FILE), "id = \"x\"\n")?;

        let tree = read_tree(&root)?;
        let paths: Vec<&str> = tree.iter().map(|e| e.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["Spanish", "Spanish/verbs.md"]);
        assert!(tree[0].is_dir);
        assert_eq!(tree[1].depth, 1);
        Ok(())
    }

    #[test]
    fn tree_hides_the_metadata_file_and_dotfiles() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        let root = LocalRoot::for_user(&dir, None)?;
        std::fs::write(root.path().join(LOCAL_META_FILE), "id = \"x\"\n")?;
        std::fs::write(root.path().join(".hidden"), "")?;

        assert!(read_tree(&root)?.is_empty());
        Ok(())
    }

    #[test]
    fn card_template_parses_into_two_cards() -> Fallible<()> {
        use crate::parser::Parser;
        use crate::parser::strip_frontmatter_with_offset;

        let (content, offset) = strip_frontmatter_with_offset(CARD_TEMPLATE)?;
        let parser = Parser::new("My deck".to_string(), PathBuf::from("t.md"), offset);
        let parsed = parser.parse_with_duplicates(content)?;
        assert_eq!(parsed.cards.len(), 2);
        Ok(())
    }
}
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test --lib cmd::serve::files`
Expected: FAIL to compile — nothing in the module exists yet.

- [ ] **Step 4: Implement the tree and the template**

Put this above the test module in `src/cmd/serve/files.rs`:

```rust
use std::fs::read_dir;
use std::path::PathBuf;

use crate::cmd::serve::auth::CurrentUser;
use crate::cmd::serve::local::LOCAL_META_FILE;
use crate::cmd::serve::local::LocalRoot;
use crate::cmd::serve::state::AppState;
use crate::error::Fallible;
use crate::error::fail;

/// Seed content for a new card file, also offered for copying on the
/// Sources page. Frontmatter is TOML, matching `parse_deck`.
pub const CARD_TEMPLATE: &str = r#"---
name = "My deck"
---

Q: What is the capital of France?
A: Paris.

C: The mitochondria is the [powerhouse] of the cell.
"#;

/// One row of the file tree, flattened depth-first for rendering.
pub struct TreeEntry {
    pub rel_path: String,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
}

/// The whole tree under `root`, folders before files at each level, each
/// level sorted by name. Dotfiles and the per-collection metadata file are
/// hidden: they are hashcards' bookkeeping, not the user's content.
pub fn read_tree(root: &LocalRoot) -> Fallible<Vec<TreeEntry>> {
    let mut out = Vec::new();
    walk(root.path(), "", 0, &mut out)?;
    Ok(out)
}

fn walk(
    dir: &std::path::Path,
    prefix: &str,
    depth: usize,
    out: &mut Vec<TreeEntry>,
) -> Fallible<()> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in read_dir(dir)? {
        let entry = entry?;
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if name.starts_with('.') || name == LOCAL_META_FILE {
            continue;
        }
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            dirs.push(name);
        } else if name.ends_with(".md") {
            files.push(name);
        }
    }
    dirs.sort();
    files.sort();

    for name in dirs {
        let rel_path = join_rel(prefix, &name);
        out.push(TreeEntry {
            rel_path: rel_path.clone(),
            name: name.clone(),
            is_dir: true,
            depth,
        });
        walk(&dir.join(&name), &rel_path, depth + 1, out)?;
    }
    for name in files {
        out.push(TreeEntry {
            rel_path: join_rel(prefix, &name),
            name,
            is_dir: false,
            depth,
        });
    }
    Ok(())
}

fn join_rel(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

/// The local tree belonging to the caller.
pub fn user_root(state: &AppState, user: Option<&CurrentUser>) -> Fallible<LocalRoot> {
    let data_dir = match &state.config.data_dir {
        Some(d) => d,
        None => {
            return fail(
                "Local card folders need a data directory. Start hashcards-web with a config file.",
            );
        }
    };
    LocalRoot::for_user(data_dir, user.map(|u| u.email.as_str()))
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib cmd::serve::files`
Expected: PASS, 3 tests.

- [ ] **Step 6: Write the failing name-validation test**

A file or folder name is a single component — a user must not smuggle a path into it. Add to the same test module:

```rust
#[test]
fn names_must_be_single_components() {
    assert!(validate_name("verbs.md").is_ok());
    assert!(validate_name("Spanish").is_ok());
    assert!(validate_name("a/b").is_err());
    assert!(validate_name("..").is_err());
    assert!(validate_name("").is_err());
    assert!(validate_name(LOCAL_META_FILE).is_err());
}
```

- [ ] **Step 7: Run it, then implement `validate_name`**

Run: `cargo test --lib cmd::serve::files::tests::names_must_be_single_components`
Expected: FAIL — `validate_name` does not exist. Then add to `files.rs`:

```rust
/// A new file or folder name: exactly one path component, and not one of
/// hashcards' own bookkeeping names.
fn validate_name(name: &str) -> Fallible<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return fail("Enter a name.");
    }
    if trimmed == LOCAL_META_FILE {
        return fail(format!("`{LOCAL_META_FILE}` is reserved by hashcards."));
    }
    if trimmed.starts_with('.') {
        return fail("Names cannot start with a dot.");
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return fail("Names cannot contain `/` or `\\` — create a folder instead.");
    }
    if PathBuf::from(trimmed).components().count() != 1 {
        return fail(format!("`{trimmed}` is not a valid name."));
    }
    Ok(trimmed.to_string())
}
```

Re-run: PASS.

- [ ] **Step 8: Add the handlers**

Append to `src/cmd/serve/files.rs`. They follow the same shape as `hedgedoc_add_handler`: take `State`, an optional `CurrentUser`, a `Form`, and redirect with a `Flash`.

```rust
use axum::Form;
use axum::extract::State;
use axum::response::Html;
use axum::response::Redirect;
use serde::Deserialize;

use crate::cmd::serve::files_ui::render_tree_page;
use crate::flash::Flash;

#[derive(Deserialize)]
pub struct NewEntryForm {
    /// Parent folder, relative to the user's root. Empty means the root.
    pub parent: String,
    pub name: String,
}

#[derive(Deserialize)]
pub struct RenameForm {
    pub path: String,
    pub name: String,
}

#[derive(Deserialize)]
pub struct DeleteForm {
    pub path: String,
}

pub async fn files_get_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
    flash: Option<Flash>,
) -> Html<String> {
    let root = match user_root(&state, current_user.as_ref()) {
        Ok(r) => r,
        Err(e) => return Html(render_tree_page(&[], Some(Flash::error(e.to_string()))).into_string()),
    };
    let tree = match read_tree(&root) {
        Ok(t) => t,
        Err(e) => return Html(render_tree_page(&[], Some(Flash::error(e.to_string()))).into_string()),
    };
    Html(render_tree_page(&tree, flash).into_string())
}

pub async fn files_folder_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
    Form(form): Form<NewEntryForm>,
) -> Redirect {
    match create_entry(&state, current_user.as_ref(), &form, true) {
        Ok(msg) => Flash::success(msg).redirect("/files"),
        Err(e) => Flash::error(e.to_string()).redirect("/files"),
    }
}

pub async fn files_file_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
    Form(form): Form<NewEntryForm>,
) -> Redirect {
    match create_entry(&state, current_user.as_ref(), &form, false) {
        Ok(msg) => Flash::success(msg).redirect("/files"),
        Err(e) => Flash::error(e.to_string()).redirect("/files"),
    }
}

fn create_entry(
    state: &AppState,
    user: Option<&CurrentUser>,
    form: &NewEntryForm,
    is_dir: bool,
) -> Fallible<String> {
    let root = user_root(state, user)?;
    let mut name = validate_name(&form.name)?;
    if !is_dir && !name.ends_with(".md") {
        name.push_str(".md");
    }
    let rel = join_rel(form.parent.trim().trim_matches('/'), &name);
    let target = root.resolve(&rel)?;
    if target.exists() {
        return fail(format!("`{rel}` already exists."));
    }
    if is_dir {
        std::fs::create_dir_all(&target)?;
        // Give a new top-level folder its id immediately, so the collection
        // it becomes keeps its database across a later rename.
        if !rel.contains('/') {
            crate::cmd::serve::local::collection_id(&target)?;
        }
        Ok(format!("Created folder `{rel}`."))
    } else {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, CARD_TEMPLATE)?;
        Ok(format!("Created `{rel}`."))
    }
}

pub async fn files_rename_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
    Form(form): Form<RenameForm>,
) -> Redirect {
    match rename_entry(&state, current_user.as_ref(), &form) {
        Ok(msg) => Flash::success(msg).redirect("/files"),
        Err(e) => Flash::error(e.to_string()).redirect("/files"),
    }
}

fn rename_entry(state: &AppState, user: Option<&CurrentUser>, form: &RenameForm) -> Fallible<String> {
    let root = user_root(state, user)?;
    let from = root.resolve(&form.path)?;
    if !from.exists() {
        return fail(format!("`{}` does not exist.", form.path));
    }
    let mut name = validate_name(&form.name)?;
    if from.is_file() && !name.ends_with(".md") {
        name.push_str(".md");
    }
    let parent_rel = match form.path.trim_matches('/').rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => String::new(),
    };
    let to_rel = join_rel(&parent_rel, &name);
    let to = root.resolve(&to_rel)?;
    if to.exists() {
        return fail(format!("`{to_rel}` already exists."));
    }
    std::fs::rename(&from, &to)?;
    Ok(format!("Renamed to `{to_rel}`."))
}

pub async fn files_delete_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
    Form(form): Form<DeleteForm>,
) -> Redirect {
    match delete_entry(&state, current_user.as_ref(), &form) {
        Ok(msg) => Flash::success(msg).redirect("/files"),
        Err(e) => Flash::error(e.to_string()).redirect("/files"),
    }
}

fn delete_entry(state: &AppState, user: Option<&CurrentUser>, form: &DeleteForm) -> Fallible<String> {
    let root = user_root(state, user)?;
    let target = root.resolve(&form.path)?;
    if !target.exists() {
        return fail(format!("`{}` does not exist.", form.path));
    }
    if target.is_dir() {
        // Refuse a non-empty folder: deleting a whole collection on a
        // misclick would take its review history with it.
        let mut kept = Vec::new();
        for entry in read_dir(&target)? {
            let name = entry?.file_name().into_string().unwrap_or_default();
            if name != LOCAL_META_FILE && !name.starts_with('.') {
                kept.push(name);
            }
        }
        if !kept.is_empty() {
            kept.sort();
            return fail(format!(
                "`{}` is not empty — it still holds {}. Delete those first.",
                form.path,
                kept.join(", ")
            ));
        }
        std::fs::remove_dir_all(&target)?;
    } else {
        std::fs::remove_file(&target)?;
    }
    Ok(format!("Deleted `{}`.", form.path))
}
```

- [ ] **Step 9: Add the tree page template**

Create `src/cmd/serve/files_ui.rs`:

```rust
use maud::Markup;
use maud::html;

use crate::cmd::drill::template::page_template;
use crate::cmd::serve::files::TreeEntry;
use crate::flash::Flash;

/// The file manager: the whole tree, with per-row rename and delete, and a
/// create form for each folder.
pub fn render_tree_page(tree: &[TreeEntry], flash: Option<Flash>) -> Markup {
    page_template(html! {
        div.landing {
            @if let Some(f) = &flash { (f.render()) }
            h1 { "My Cards" }
            p { a.back-link href="/" { "← Back to collections" } }

            p.hint {
                "Each top-level folder is a collection. Files inside it are decks."
            }

            form.inline-form action="/files/folder" method="post" {
                input type="hidden" name="parent" value="";
                input type="text" name="name" placeholder="New collection name" required;
                input type="submit" value="Add collection";
            }

            @if tree.is_empty() {
                p.notice { "No cards yet. Create a collection to start." }
            }

            ul.file-tree {
                @for entry in tree {
                    li style=(format!("margin-left: {}rem", entry.depth)) {
                        @if entry.is_dir {
                            span.folder { (entry.name) }
                            form.inline-form action="/files/file" method="post" {
                                input type="hidden" name="parent" value=(entry.rel_path);
                                input type="text" name="name" placeholder="new-file.md" required;
                                input type="submit" value="Add file";
                            }
                            form.inline-form action="/files/folder" method="post" {
                                input type="hidden" name="parent" value=(entry.rel_path);
                                input type="text" name="name" placeholder="subfolder" required;
                                input type="submit" value="Add folder";
                            }
                        } @else {
                            a href=(format!("/files/edit/{}", entry.rel_path)) { (entry.name) }
                        }
                        form.inline-form action="/files/rename" method="post" {
                            input type="hidden" name="path" value=(entry.rel_path);
                            input type="text" name="name" placeholder="rename to" required;
                            input type="submit" value="Rename";
                        }
                        form.inline-form action="/files/delete" method="post" {
                            input type="hidden" name="path" value=(entry.rel_path);
                            input type="submit" value="Delete";
                        }
                    }
                }
            }
        }
    })
}
```

- [ ] **Step 10: Register the routes**

In `src/cmd/serve/server.rs`, beside the `/hedgedoc` routes:

```rust
        .route("/files", get(files_get_handler))
        .route("/files/folder", post(files_folder_handler))
        .route("/files/file", post(files_file_handler))
        .route("/files/rename", post(files_rename_handler))
        .route("/files/delete", post(files_delete_handler))
```

Import the handlers from `crate::cmd::serve::files`. These sit inside the same router that `require_auth` wraps, so under `[oidc]` they are login-gated like every other route.

- [ ] **Step 11: Write the failing safety tests**

These two cover spec requirements that nothing else exercises. Add to the
`mod tests` block in `src/cmd/serve/files.rs`:

```rust
#[test]
fn a_non_empty_folder_is_not_deleted() -> Fallible<()> {
    let dir = create_tmp_directory()?;
    let root = LocalRoot::for_user(&dir, None)?;
    let folder = root.path().join("Spanish");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join("verbs.md"), "Q: a\nA: b\n")?;

    // The metadata file alone must not count as "non-empty".
    std::fs::write(folder.join(LOCAL_META_FILE), "id = \"x\"\n")?;

    let kept = non_empty_children(&folder)?;
    assert_eq!(kept, vec!["verbs.md".to_string()]);

    std::fs::remove_file(folder.join("verbs.md"))?;
    assert!(non_empty_children(&folder)?.is_empty());
    Ok(())
}

#[test]
fn sync_never_writes_into_the_local_root() -> Fallible<()> {
    // The local root must not sit under the directory that git and source
    // sync own, or a pull could overwrite user writing.
    let dir = create_tmp_directory()?;
    let root = LocalRoot::for_user(&dir, None)?;
    let repo_dir = dir.join("repo");
    assert!(!root.path().starts_with(&repo_dir));
    Ok(())
}
```

- [ ] **Step 12: Run them, then extract `non_empty_children`**

Run: `cargo test --lib cmd::serve::files`
Expected: FAIL — `non_empty_children` does not exist.

Pull the emptiness check out of `delete_entry` into its own function so the
test can reach it, and call it from `delete_entry`:

```rust
/// Names in `dir` that belong to the user, ignoring hashcards' own
/// bookkeeping. A folder holding only bookkeeping counts as empty.
fn non_empty_children(dir: &std::path::Path) -> Fallible<Vec<String>> {
    let mut kept = Vec::new();
    for entry in read_dir(dir)? {
        let name = entry?.file_name().into_string().unwrap_or_default();
        if name != LOCAL_META_FILE && !name.starts_with('.') {
            kept.push(name);
        }
    }
    kept.sort();
    Ok(kept)
}
```

Replace the inline loop in `delete_entry` with `let kept = non_empty_children(&target)?;`.

Re-run: PASS.

- [ ] **Step 13: Run everything**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

- [ ] **Step 14: Commit**

```bash
git add src/cmd/serve/files.rs src/cmd/serve/files_ui.rs src/cmd/serve/mod.rs src/cmd/serve/server.rs
git commit -m "feat: add local file manager with create, rename and delete"
```

---

### Task 6: The document editor and saving

**Files:**
- Modify: `src/cmd/serve/files.rs`, `src/cmd/serve/files_ui.rs`, `src/cmd/serve/server.rs`
- Modify: `src/cmd/serve/edit.rs` (widen three helpers to `pub(crate)`)

**Interfaces:**
- Consumes: `LocalRoot::resolve`, `collection_id`, `CARD_TEMPLATE`, `read_tree` from Tasks 3–5.
- Produces: `pub fn db_path_for(root: &LocalRoot, db_dir: &Path, rel: &str) -> Fallible<PathBuf>`; `pub fn parse_buffer(rel: &str, content: &str) -> Fallible<ParsedFile>`; handlers `editor_get_handler`, `editor_post_handler`; `render_editor_page(rel_path: &str, content: &str, mtime: u64, flash: Option<Flash>) -> Markup`.

`edit.rs` already solves the hard parts. Make `write_atomic`, `revert_file`, `file_mtime_ms` and `plan_hash_migration` `pub(crate)` rather than writing new ones — a second atomic-write implementation is how the two drift apart.

- [ ] **Step 1: Write the failing db-path test**

Add to `src/cmd/serve/files.rs` tests:

```rust
#[test]
fn db_path_comes_from_the_top_level_folder_id() -> Fallible<()> {
    let dir = create_tmp_directory()?;
    let root = LocalRoot::for_user(&dir, None)?;
    let folder = root.path().join("Spanish");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join("verbs.md"), "Q: a\nA: b\n")?;
    let id = crate::cmd::serve::local::collection_id(&folder)?;

    let db_dir = dir.join("db");
    let path = db_path_for(&root, &db_dir, "Spanish/verbs.md")?;
    assert_eq!(path, db_dir.join(format!("{id}.db")));
    Ok(())
}

#[test]
fn a_file_outside_any_collection_folder_is_rejected() -> Fallible<()> {
    let dir = create_tmp_directory()?;
    let root = LocalRoot::for_user(&dir, None)?;
    std::fs::write(root.path().join("loose.md"), "Q: a\nA: b\n")?;
    assert!(db_path_for(&root, &dir.join("db"), "loose.md").is_err());
    Ok(())
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib cmd::serve::files`
Expected: FAIL — `db_path_for` does not exist.

- [ ] **Step 3: Implement `db_path_for`**

Add to `src/cmd/serve/files.rs`:

```rust
/// The review database of the collection a local file belongs to.
///
/// A collection is a top-level folder, so a file directly under the root
/// belongs to none and cannot be reviewed — the editor refuses it rather
/// than inventing a database for it.
pub fn db_path_for(root: &LocalRoot, db_dir: &std::path::Path, rel: &str) -> Fallible<PathBuf> {
    let top = match rel.trim_matches('/').split('/').next() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return fail(format!("`{rel}` is not inside a collection folder.")),
    };
    if !rel.trim_matches('/').contains('/') {
        return fail(format!(
            "`{rel}` sits directly in your card folder. Move it into a collection folder so its reviews have somewhere to go."
        ));
    }
    let folder = root.resolve(&top)?;
    if !folder.is_dir() {
        return fail(format!("`{top}` is not a collection folder."));
    }
    let id = crate::cmd::serve::local::collection_id(&folder)?;
    Ok(db_dir.join(format!("{id}.db")))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib cmd::serve::files`
Expected: PASS.

- [ ] **Step 5: Widen the `edit.rs` helpers**

In `src/cmd/serve/edit.rs`, change these four signatures from private to `pub(crate)`, leaving their bodies alone:

```rust
pub(crate) fn write_atomic(file_path: &Path, content: &str) -> Fallible<()>
pub(crate) fn revert_file(file_path: &Path, original: &str) -> Fallible<()>
pub(crate) fn file_mtime_ms(path: &Path) -> Fallible<u64>
pub(crate) fn plan_hash_migration(old_cards: &[&Card], new_cards: &[&Card]) -> MigrationPlan
```

Run `cargo build` and confirm no warnings about unused visibility.

- [ ] **Step 6: Add the editor handlers**

Append to `src/cmd/serve/files.rs`:

```rust
use axum::extract::Path as AxumPath;

use crate::cmd::serve::edit::file_mtime_ms;
use crate::cmd::serve::edit::plan_hash_migration;
use crate::cmd::serve::edit::revert_file;
use crate::cmd::serve::edit::write_atomic;
use crate::cmd::serve::files_ui::render_editor_page;
use crate::db::Database;
use crate::parser::Parser;
use crate::parser::strip_frontmatter_with_offset;
use crate::types::timestamp::Timestamp;

#[derive(Deserialize)]
pub struct SaveForm {
    pub content: String,
    /// The mtime the browser loaded, so a save cannot silently overwrite a
    /// change made elsewhere. Same guard the card editor uses.
    pub mtime: u64,
}

pub async fn editor_get_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
    AxumPath(rel): AxumPath<String>,
    flash: Option<Flash>,
) -> Html<String> {
    let body = match load_for_edit(&state, current_user.as_ref(), &rel) {
        Ok((content, mtime)) => render_editor_page(&rel, &content, mtime, flash),
        Err(e) => render_editor_page(&rel, "", 0, Some(Flash::error(e.to_string()))),
    };
    Html(body.into_string())
}

fn load_for_edit(
    state: &AppState,
    user: Option<&CurrentUser>,
    rel: &str,
) -> Fallible<(String, u64)> {
    let root = user_root(state, user)?;
    let path = root.resolve(rel)?;
    if !path.is_file() {
        return fail(format!("`{rel}` is not a file."));
    }
    let content = std::fs::read_to_string(&path)?;
    let mtime = file_mtime_ms(&path)?;
    Ok((content, mtime))
}

pub async fn editor_post_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
    AxumPath(rel): AxumPath<String>,
    Form(form): Form<SaveForm>,
) -> Redirect {
    let target = format!("/files/edit/{rel}");
    match save_file(&state, current_user.as_ref(), &rel, &form) {
        Ok(msg) => Flash::success(msg).redirect(&target),
        Err(e) => Flash::error(e.to_string()).redirect(&target),
    }
}

/// Write the buffer, reparse it, and migrate card hashes so a reworded card
/// keeps its schedule. A buffer that does not parse never reaches disk.
fn save_file(
    state: &AppState,
    user: Option<&CurrentUser>,
    rel: &str,
    form: &SaveForm,
) -> Fallible<String> {
    let root = user_root(state, user)?;
    let path = root.resolve(rel)?;
    if !path.is_file() {
        return fail(format!("`{rel}` is not a file."));
    }

    let on_disk = file_mtime_ms(&path)?;
    if on_disk != form.mtime {
        return fail(
            "This file changed since you opened it. Reload the page and reapply your edit.",
        );
    }

    let original = std::fs::read_to_string(&path)?;
    let old_cards = parse_buffer(rel, &original)?.cards;

    write_atomic(&path, &form.content)?;
    let new_cards = match parse_buffer(rel, &form.content) {
        Ok(parsed) => parsed.cards,
        Err(e) => {
            revert_file(&path, &original)?;
            return fail(format!("Not saved — {e}"));
        }
    };

    let db_dir = match &state.config.data_dir {
        Some(d) => d.join("db"),
        None => return fail("No data directory is configured."),
    };
    let db_path = db_path_for(&root, &db_dir, rel)?;
    let old_refs: Vec<&crate::types::card::Card> = old_cards.iter().collect();
    let new_refs: Vec<&crate::types::card::Card> = new_cards.iter().collect();
    let plan = plan_hash_migration(&old_refs, &new_refs);

    let mut db = Database::new(&db_path)?;
    let counts = db.apply_edit_migration(&plan.renames, &plan.fresh, Timestamp::now())?;

    let skipped = plan.skipped + counts.collided;
    if skipped > 0 {
        Ok(format!(
            "Saved {} cards. {skipped} could not be matched to their old review history and start fresh.",
            new_cards.len()
        ))
    } else {
        Ok(format!("Saved {} cards.", new_cards.len()))
    }
}

/// Parse an unsaved buffer as the file at `rel` would be parsed on disk.
///
/// The deck name comes from the filename, the line offset from the TOML
/// frontmatter, so reported error lines match what is in the textarea.
pub fn parse_buffer(rel: &str, content: &str) -> Fallible<crate::parser::ParsedFile> {
    let (body, offset) = strip_frontmatter_with_offset(content)?;
    let deck_name = rel
        .rsplit('/')
        .next()
        .unwrap_or(rel)
        .trim_end_matches(".md")
        .to_string();
    let parser = Parser::new(deck_name, PathBuf::from(rel), offset);
    Ok(parser.parse_with_duplicates(body)?)
}
```

If `apply_edit_migration` does not take a `Timestamp`, match the call exactly as `edit_post_inner` makes it in `src/cmd/serve/edit.rs`.

- [ ] **Step 7: Add the editor template**

Append to `src/cmd/serve/files_ui.rs`:

```rust
/// The document editor: raw markdown on the left with insert buttons, a
/// live parse preview on the right (filled in by Task 7).
pub fn render_editor_page(
    rel_path: &str,
    content: &str,
    mtime: u64,
    flash: Option<Flash>,
) -> Markup {
    page_template(html! {
        div.editor {
            @if let Some(f) = &flash { (f.render()) }
            h1 { (rel_path) }
            p { a.back-link href="/files" { "← Back to my cards" } }

            div.editor-toolbar {
                button type="button" data-snippet="Q: \nA: " { "Q/A" }
                button type="button" data-snippet="C: The [answer] goes here." { "Cloze" }
                button type="button" data-snippet="T: \nD: " { "Term" }
                button type="button" data-snippet="\n---\n" { "Separator" }
                button type="button" data-snippet="$$\n\n$$" { "LaTeX" }
                button type="button" data-snippet="![](image.png)" { "Image" }
            }

            div.editor-panes {
                form #editor-form action=(format!("/files/edit/{rel_path}")) method="post" {
                    input type="hidden" name="mtime" value=(mtime);
                    textarea #editor-text name="content" spellcheck="false" { (content) }
                    input type="submit" value="Save";
                }
                div #preview .preview-pane {
                    p.hint { "Start typing to see your cards." }
                }
            }
        }
    })
}
```

- [ ] **Step 8: Add the toolbar behaviour**

Append to `src/cmd/drill/script.js` — insert at the caret and keep focus, so the button does not steal the cursor position:

```javascript
// Markdown editor: insert a card skeleton at the caret.
document.querySelectorAll('.editor-toolbar button[data-snippet]').forEach(function (button) {
  button.addEventListener('click', function () {
    var textarea = document.getElementById('editor-text');
    if (!textarea) return;
    var snippet = button.getAttribute('data-snippet');
    var start = textarea.selectionStart;
    var end = textarea.selectionEnd;
    textarea.value = textarea.value.slice(0, start) + snippet + textarea.value.slice(end);
    textarea.selectionStart = textarea.selectionEnd = start + snippet.length;
    textarea.focus();
    textarea.dispatchEvent(new Event('input'));
  });
});
```

- [ ] **Step 9: Register the routes**

In `src/cmd/serve/server.rs`:

```rust
        .route("/files/edit/{*path}", get(editor_get_handler))
        .route("/files/edit/{*path}", post(editor_post_handler))
```

`{*path}` is a wildcard capture — card paths contain `/`, so a single segment will not match.

- [ ] **Step 10: Write the failing round-trip test**

Add to `src/cmd/serve/files.rs` tests:

```rust
#[test]
fn parse_buffer_names_the_deck_after_the_file() -> Fallible<()> {
    let parsed = parse_buffer("Spanish/verbs.md", "Q: the cat\nA: el gato\n")?;
    assert_eq!(parsed.cards.len(), 1);
    assert_eq!(parsed.cards[0].deck_name(), "verbs");
    Ok(())
}

#[test]
fn parse_buffer_honours_toml_frontmatter_offset() -> Fallible<()> {
    let text = "---\nname = \"Custom\"\n---\n\nQ: a\nA: b\n";
    let parsed = parse_buffer("Spanish/verbs.md", text)?;
    assert_eq!(parsed.cards.len(), 1);
    Ok(())
}

#[test]
fn parse_buffer_reports_an_error_for_a_dangling_question() {
    assert!(parse_buffer("Spanish/verbs.md", "Q: no answer follows\n").is_err());
}
```

- [ ] **Step 11: Run everything**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS. If the dangling-question test fails because the parser accepts it, change the assertion to whatever the parser genuinely rejects — check `src/parser.rs` `State` transitions and pick a real error case rather than weakening the test to `is_ok()`.

- [ ] **Step 12: Commit**

```bash
git add src/cmd/serve/files.rs src/cmd/serve/files_ui.rs src/cmd/serve/edit.rs src/cmd/serve/server.rs src/cmd/drill/script.js
git commit -m "feat: add a document editor for local cards with hash-preserving saves"
```

---

### Task 7: Live parse preview

**Files:**
- Modify: `src/cmd/serve/files.rs`, `src/cmd/serve/files_ui.rs`, `src/cmd/serve/server.rs`, `src/cmd/drill/script.js`

**Interfaces:**
- Consumes: `parse_buffer` from Task 6; `Card::html_front`, `Card::html_back`, `MediaResolverBuilder`.
- Produces: `preview_handler` at `POST /files/preview`; `render_preview(rel_path: &str, content: &str) -> Markup`.

The preview answers one question: *will hashcards read this?* It therefore runs the production parser. A JavaScript approximation could render a card hashcards cannot actually read, which would be worse than no preview at all.

- [ ] **Step 1: Write the failing preview-render tests**

Add to `src/cmd/serve/files.rs` tests:

```rust
#[test]
fn preview_of_valid_markdown_lists_every_card() -> Fallible<()> {
    let markup = render_preview("Spanish/verbs.md", "Q: the cat\nA: el gato\n\nC: A [dog] barks.");
    let html = markup.into_string();
    assert!(html.contains("2 cards"));
    assert!(html.contains("el gato"));
    Ok(())
}

#[test]
fn preview_of_broken_markdown_shows_the_line_number() {
    let html = render_preview("Spanish/verbs.md", "Q: dangling question\n").into_string();
    assert!(html.contains("Line"));
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib cmd::serve::files`
Expected: FAIL — `render_preview` does not exist.

- [ ] **Step 3: Implement the preview renderer**

Add to `src/cmd/serve/files_ui.rs`:

```rust
use crate::media::resolve::MediaResolverBuilder;
use crate::markdown::MarkdownRenderConfig;

/// Render an unsaved buffer's parse result.
///
/// Returns a fragment, not a page: the editor swaps it into the preview
/// pane. Errors are shown with their line number so the user can find them
/// in the textarea.
pub fn render_preview(rel_path: &str, content: &str) -> Markup {
    let parsed = match crate::cmd::serve::files::parse_buffer(rel_path, content) {
        Ok(p) => p,
        Err(e) => {
            return html! {
                div.preview-error {
                    p { "This file does not parse yet." }
                    p.error-detail { (e.to_string()) }
                }
            };
        }
    };

    let config = match preview_render_config(rel_path) {
        Ok(c) => c,
        Err(e) => {
            return html! { div.preview-error { p { (e.to_string()) } } };
        }
    };

    html! {
        p.preview-count { (format!("{} cards", parsed.cards.len())) }
        @if !parsed.duplicates.is_empty() {
            p.preview-warning {
                (format!("{} duplicate cards will be skipped.", parsed.duplicates.len()))
            }
        }
        @for card in &parsed.cards {
            div.preview-card {
                div.preview-front {
                    @match card.html_front(&config) {
                        Ok(m) => (m),
                        Err(e) => p.error-detail { (e.to_string()) },
                    }
                }
                div.preview-back {
                    @match card.html_back(&config) {
                        Ok(m) => (m),
                        Err(e) => p.error-detail { (e.to_string()) },
                    }
                }
            }
        }
    }
}

/// Media in a preview is not resolvable — the buffer is unsaved and its
/// images may not exist yet — so the resolver points at the file's own
/// folder and broken references simply render as broken links.
fn preview_render_config(rel_path: &str) -> crate::error::Fallible<MarkdownRenderConfig> {
    Ok(MarkdownRenderConfig {
        resolver: MediaResolverBuilder::new().build()?,
        file_url_prefix: format!("/files/media/{rel_path}"),
    })
}
```

If `MediaResolverBuilder::build()` requires a collection path, pass the user's root — check `src/media/resolve.rs` and use whichever constructor accepts a directory that exists.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib cmd::serve::files`
Expected: PASS. Adjust the asserted strings if the rendered markup differs — assert on content the renderer genuinely emits, never by loosening to `assert!(true)`.

- [ ] **Step 5: Add the preview handler**

Append to `src/cmd/serve/files.rs`:

```rust
#[derive(Deserialize)]
pub struct PreviewForm {
    pub path: String,
    pub content: String,
}

/// Parse and render an unsaved buffer. Returns an HTML fragment for the
/// editor's preview pane; never touches disk.
pub async fn preview_handler(Form(form): Form<PreviewForm>) -> Html<String> {
    Html(crate::cmd::serve::files_ui::render_preview(&form.path, &form.content).into_string())
}
```

Register it in `src/cmd/serve/server.rs`:

```rust
        .route("/files/preview", post(preview_handler))
```

- [ ] **Step 6: Wire the pane up in the browser**

Append to `src/cmd/drill/script.js`:

```javascript
// Markdown editor: debounced live parse preview.
(function () {
  var textarea = document.getElementById('editor-text');
  var pane = document.getElementById('preview');
  var form = document.getElementById('editor-form');
  if (!textarea || !pane || !form) return;

  var timer = null;
  function refresh() {
    var body = new URLSearchParams();
    body.set('path', form.getAttribute('action').replace('/files/edit/', ''));
    body.set('content', textarea.value);
    fetch('/files/preview', {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: body.toString(),
    })
      .then(function (r) { return r.text(); })
      .then(function (html) { pane.innerHTML = html; })
      .catch(function () { /* leave the last good preview in place */ });
  }

  textarea.addEventListener('input', function () {
    if (timer) clearTimeout(timer);
    timer = setTimeout(refresh, 300);
  });
  refresh();
})();
```

- [ ] **Step 7: Style the two panes**

Append to `src/cmd/drill/style.css`:

```css
.editor-panes { display: flex; gap: 1rem; align-items: flex-start; }
.editor-panes form { flex: 1 1 50%; display: flex; flex-direction: column; }
#editor-text { width: 100%; min-height: 60vh; font-family: monospace; }
.preview-pane { flex: 1 1 50%; min-height: 60vh; overflow-y: auto; }
.preview-card { border: 1px solid currentColor; border-radius: 4px; margin-bottom: .75rem; padding: .5rem; }
.preview-back { opacity: .8; }
.preview-error { border-left: 3px solid #c00; padding-left: .75rem; }
.editor-toolbar button { margin-right: .25rem; }
```

- [ ] **Step 8: Run everything**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src/cmd/serve/files.rs src/cmd/serve/files_ui.rs src/cmd/serve/server.rs src/cmd/drill/script.js src/cmd/drill/style.css
git commit -m "feat: live parse preview in the markdown editor"
```

---

### Task 8: One Sources page for all three kinds

**Files:**
- Modify: `src/cmd/serve/hedgedoc_ui.rs`, `src/cmd/serve/server.rs`, `src/cmd/serve/handlers.rs`

**Interfaces:**
- Consumes: `SourceKind`, `detect_kind` from Task 1; `CARD_TEMPLATE` from Task 5.
- Produces: `/sources` routes; `/hedgedoc` redirects to `/sources`.

- [ ] **Step 1: Move the routes**

In `src/cmd/serve/server.rs`, rename the four HedgeDoc routes and add a redirect so existing bookmarks keep working:

```rust
        .route("/sources", get(hedgedoc_manage_handler))
        .route("/sources/add", post(hedgedoc_add_handler))
        .route("/sources/delete", post(hedgedoc_delete_handler))
        .route("/sources/sync", post(hedgedoc_sync_now_handler))
        .route("/hedgedoc", get(|| async { Redirect::permanent("/sources") }))
```

- [ ] **Step 2: Update every redirect target**

In `src/cmd/serve/handlers.rs`, change every `.redirect("/hedgedoc")` to `.redirect("/sources")`. Search for the string to be sure none are missed:

```bash
grep -rn '"/hedgedoc' src/
```

Only the permanent-redirect route registered in Step 1 should remain.

- [ ] **Step 3: Retitle the page and show the kind**

In `src/cmd/serve/hedgedoc_ui.rs`, change the heading from `"HedgeDoc Sources"` to `"Sources"`, point the forms at `/sources/*`, and add a kind badge per row. Inside the loop that renders each source:

```rust
                            span.source-kind { (detect_kind(&source.note.url).label()) }
```

with `use crate::cmd::serve::source::detect_kind;` at the top. Update the add box's placeholder to `"Paste a HedgeDoc note or git file URL"`.

- [ ] **Step 4: Add the copyable template block**

In the same file, directly under the add form:

```rust
                details.template-block {
                    summary { "Need a starting point?" }
                    p { "Paste this into a HedgeDoc note or a markdown file in a git repository:" }
                    pre #card-template { (CARD_TEMPLATE) }
                    button type="button" #copy-template { "Copy template" }
                }
```

with `use crate::cmd::serve::files::CARD_TEMPLATE;`. Add the copy behaviour to `src/cmd/drill/script.js`:

```javascript
// Sources page: copy the starter template.
(function () {
  var button = document.getElementById('copy-template');
  var block = document.getElementById('card-template');
  if (!button || !block) return;
  button.addEventListener('click', function () {
    navigator.clipboard.writeText(block.textContent).then(function () {
      button.textContent = 'Copied';
      setTimeout(function () { button.textContent = 'Copy template'; }, 2000);
    });
  });
})();
```

- [ ] **Step 5: Link the file manager from the landing page**

In `src/cmd/serve/landing.rs`, add a link to `/files` beside the existing sources link, labelled `"My Cards"`.

- [ ] **Step 6: Rename the leftover field**

Rename `ResolvedServeConfig::hedgedoc_entries` to `source_entries_resolved` (or keep `hedgedoc_entries` if the rename ripples too far — note the decision in the commit message). Then remove the `pub type HedgedocEntry = SourceEntry;` alias from Task 2 and fix the resulting compile errors, so only one name survives.

- [ ] **Step 7: Run everything**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/cmd/serve/
git commit -m "feat: unify HedgeDoc, git and local sources on one Sources page"
```

---

### Task 9: Documentation and changelog

**Files:**
- Modify: `README.md`, `CHANGELOG.xml`

- [ ] **Step 1: Document `[[source]]` in the README**

Replace the `### `[[hedgedoc]]`` section with a `### `[[source]]`` section covering both kinds:

````markdown
### `[[source]]`

A markdown document fetched over HTTPS and drilled as its own collection.
Either a HedgeDoc note or a file in a git repository — hashcards works out
which from the URL.

```toml
[[source]]
url = "https://notes.example.com/abc123"
# owner = "me@example.com"

[[source]]
url = "https://github.com/me/cards/blob/main/spanish.md"
# owner = "me@example.com"
```

Recognised git URL shapes: GitHub `/blob/`, GitLab `/-/blob/`, Gitea and
Forgejo `/src/branch/`, and any URL that already points straight at a `.md`
file. Private repositories are not supported — the file must be reachable
without authentication.

`[[hedgedoc]]` is still accepted as a deprecated spelling of `[[source]]`.
Sources added through the web interface are written as `[[source]]`.
````

- [ ] **Step 2: Document the local card folder**

Add a new README section after `[[source]]`:

````markdown
## My Cards

Cards do not have to come from anywhere else. **My Cards** (`/files`) is a
folder tree that hashcards stores on disk at `{data_dir}/local/{user}` and
that only you write to — no git remote, no sync that can overwrite it.

Each top-level folder is a collection; files inside it are decks. Create a
folder, add a `.md` file, and write cards in the editor: the buttons insert
Q/A, cloze and term skeletons, and the pane on the right shows the cards as
hashcards parses them, so a file that does not parse never gets saved.

Renaming a folder is safe. Each one keeps a `.hashcards.toml` holding a
stable id, and review databases are named from that id rather than from the
folder name, so your history follows the rename.
````

- [ ] **Step 3: Add the changelog entries**

Add to `CHANGELOG.xml`, matching the surrounding element shape exactly (open the file and copy the structure of the most recent release block rather than guessing):

- `Added`: local card folders with a browser file manager and markdown editor, at `/files`.
- `Added`: live parse preview in the editor.
- `Added`: git file URLs as a source kind (GitHub, GitLab, Gitea/Forgejo, and raw `.md` URLs).
- `Changed`: `[[hedgedoc]]` config entries are now `[[source]]`; the old spelling still works.
- `Changed`: `/hedgedoc` is now `/sources` and redirects.

- [ ] **Step 4: Verify the documented paths are real**

Run:

```bash
grep -rn '"/files\|"/sources' src/cmd/serve/server.rs
```

Expected: every route named in the README appears. Fix the README if a name drifted during implementation.

- [ ] **Step 5: Run everything one last time**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add README.md CHANGELOG.xml
git commit -m "docs: document [[source]], local card folders and the editor"
```

---

## Done when

- A logged-out instance with no `[git]` and no sources can still create a collection, write a card, and drill it.
- Pasting a GitHub `/blob/` URL on `/sources` adds a working collection.
- Renaming a collection folder keeps its due counts.
- A file that does not parse cannot be saved, and the preview says why and on which line.
- `cargo clippy --all-targets -- -D warnings` and `cargo test` are clean.
