use std::collections::HashMap;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use tokio::sync::RwLock;
use tokio::time::interval;

use crate::cmd::serve::config::HedgedocEntry;
use crate::cmd::serve::config::ResolvedCollection;
use crate::cmd::serve::config::slugify;
use crate::cmd::serve::git::compute_collection_counts;
use crate::cmd::serve::state::CollectionInfo;
use crate::cmd::serve::state::HedgedocNote;
use crate::cmd::serve::state::HedgedocSource;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::error::fail;
use crate::types::timestamp::Timestamp;

/// Extract the note ID (last non-empty path segment) from a HedgeDoc URL.
/// Query parameters and fragments are ignored.
pub fn note_id_from_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    parsed
        .path_segments()?
        .rfind(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub fn source_uri_from_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let port = parsed.port();
    Some(match port {
        Some(p) => format!("{}://{}:{}", parsed.scheme(), host, p),
        None => format!("{}://{}", parsed.scheme(), host),
    })
}

pub fn source_display_name(source_uri: &str) -> String {
    // Show just the hostname rather than the full URI for a cleaner display.
    if let Ok(parsed) = reqwest::Url::parse(source_uri) {
        if let Some(host) = parsed.host_str() {
            return host.to_string();
        }
    }
    source_uri.to_string()
}

/// Build the collection slug for a HedgeDoc note.
///
/// Every note is a collection of its own, so the slug is derived from the
/// note URL, never from the host. `owner` is folded into the hash as well:
/// two users may legitimately add the same public note, and they must not
/// end up sharing a slug — and therefore a database.
pub fn slug_for_note(url: &str, owner: Option<&str>) -> String {
    let note_id = note_id_from_url(url).unwrap_or_else(|| url.to_string());
    let stem = slugify(&note_id);
    let stem = if stem.is_empty() {
        "note"
    } else {
        stem.as_str()
    };
    let keyed = match owner {
        Some(owner) => format!("{owner}\n{url}"),
        None => url.to_string(),
    };
    format!("hedgedoc-{}-{}", stem, note_id_disambiguator(&keyed))
}

/// Return the configured collection whose slug collides with `slug`, if any.
///
/// Used when adding a HedgeDoc source: `find_collection` prefers configured
/// collections, so a colliding source would silently drill against the wrong
/// database (BUG-43).
pub fn find_slug_collision<'a>(
    slug: &str,
    collections: &'a [ResolvedCollection],
) -> Option<&'a ResolvedCollection> {
    collections.iter().find(|c| c.slug == slug)
}

/// Reject a startup configuration in which two collections share a URL slug.
///
/// `find_slug_collision` covers the runtime "add a HedgeDoc source" path, but
/// sources built from `[[hedgedoc]]` config entries never went through it,
/// and two distinct source URIs can slugify to the same string. Because
/// `find_collection` prefers configured collections, either case silently
/// drills and edits against the wrong collection's database (BUG-43), so this
/// refuses to start rather than serving the wrong data.
pub fn check_startup_slug_collisions(
    collections: &[ResolvedCollection],
    sources: &[HedgedocSource],
) -> Fallible<()> {
    let mut seen: HashMap<&str, String> = HashMap::new();
    for rc in collections {
        seen.insert(rc.slug.as_str(), format!("collection '{}'", rc.name));
    }
    for source in sources {
        let slug = source.collection.slug.as_str();
        let owner = format!("HedgeDoc note '{}'", source.note.url);
        if let Some(first) = seen.get(slug) {
            return fail(format!(
                "configuration error: {first} and {owner} both map to the URL slug '{slug}'. Rename the collection or use a different HedgeDoc source URL so their slugs differ."
            ));
        }
        seen.insert(slug, owner);
    }
    Ok(())
}

/// Validate that a HedgeDoc URL is safe to fetch (HTTPS only).
fn validate_hedgedoc_url(url: &str) -> Fallible<()> {
    let parsed = reqwest::Url::parse(url).map_err(|e| {
        crate::error::ErrorReport::new(format!("Invalid HedgeDoc URL `{}`: {}", url, e))
    })?;
    if parsed.scheme() != "https" {
        return fail(format!("HedgeDoc URLs must use HTTPS (got: {})", url));
    }
    Ok(())
}

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

/// Build the `/download` URL for a HedgeDoc note, safely appending the path
/// segment without interfering with any query string or fragment.
///
/// HedgeDoc published/shared notes are served at `/s/<noteId>`. The `/s/`
/// prefix is stripped here because the `/download` endpoint lives at
/// `/<noteId>/download`, not `/s/<noteId>/download`.
fn build_download_url(url: &str) -> Fallible<reqwest::Url> {
    let mut parsed = reqwest::Url::parse(url).map_err(|e| {
        crate::error::ErrorReport::new(format!("Invalid HedgeDoc URL `{}`: {}", url, e))
    })?;
    parsed.set_query(None);
    parsed.set_fragment(None);
    // Strip /s/<noteId> → /<noteId> for shared/published note URLs.
    {
        let segments: Vec<String> = parsed
            .path_segments()
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        if segments.first().map(|s| s == "s").unwrap_or(false) && segments.len() >= 2 {
            parsed.set_path(&format!("/{}", segments[1..].join("/")));
        }
    }
    {
        let mut segments = parsed.path_segments_mut().map_err(|_| {
            crate::error::ErrorReport::new(format!("Cannot modify path for HedgeDoc URL `{}`", url))
        })?;
        segments.pop_if_empty();
        segments.push("download");
    }
    Ok(parsed)
}

/// Return the shared HTTP client, initialising it on first use.
/// The client is configured with a 30-second timeout.
fn http_client() -> Fallible<&'static reqwest::Client> {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    // Fast path: already initialised.
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(crate::error::ErrorReport::from)?;
    // get_or_init is stable; if another thread raced us, the already-stored
    // client is returned and our freshly built one is dropped (harmless).
    Ok(CLIENT.get_or_init(|| client))
}

/// Fetch raw markdown from a HedgeDoc note URL.
/// Appends `/download` to the note URL path to get the raw markdown.
/// Only HTTPS URLs are accepted. Requests time out after 30 seconds.
pub async fn fetch_markdown(url: &str) -> Fallible<String> {
    validate_hedgedoc_url(url)?;
    let download_url = build_download_url(url)?;
    let client = http_client()?;
    let response = client.get(download_url.clone()).send().await?;
    if !response.status().is_success() {
        return fail(format!(
            "HedgeDoc fetch returned HTTP {} for {}",
            response.status(),
            download_url
        ));
    }
    // HedgeDoc's /download endpoint requires edit permission. For notes that
    // are publicly viewable but not editable, HedgeDoc redirects to the HTML
    // view instead of returning 403. Detect this to avoid silently storing
    // HTML as markdown (which would show "OK" status but yield no cards).
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if content_type.starts_with("text/html") {
        return fail(format!(
            "HedgeDoc returned an HTML page for {} — the note's permission \
            level may require sign-in to download (try setting the note to \
            \"Freely\" or \"Editable\" in HedgeDoc)",
            download_url
        ));
    }
    Ok(response.text().await?)
}

/// Extract a human-readable title from markdown content.
/// Checks YAML frontmatter `title:` first, then the first `# heading`.
/// Falls back to `fallback` (typically the note ID).
pub fn extract_title(markdown: &str, fallback: &str) -> String {
    if let Some(title) = frontmatter_title(markdown) {
        return title;
    }
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            let title = rest.trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
    }
    fallback.to_string()
}

fn sanitize_deck_name(name: &str, fallback: &str) -> String {
    let candidate = if name.trim().is_empty() {
        fallback.trim()
    } else {
        name.trim()
    };
    let normalized = candidate.replace('/', " - ");
    if normalized.trim().is_empty() {
        "Deck".to_string()
    } else {
        normalized.trim().to_string()
    }
}

fn split_yaml_frontmatter(markdown: &str) -> Option<(&str, &str)> {
    let content = markdown.trim_start();
    if !content.starts_with("---\n") && !content.starts_with("---\r\n") {
        return None;
    }

    let after_start = if content.starts_with("---\r\n") { 5 } else { 4 };
    let after = &content[after_start..];

    let mut current = 0;
    while let Some(idx) = after[current..].find("\n---") {
        let abs_idx = current + idx;
        let next_char_idx = abs_idx + 4;
        let is_end = next_char_idx == after.len()
            || after[next_char_idx..].starts_with('\n')
            || after[next_char_idx..].starts_with("\r\n");

        if is_end {
            let mut fm_end = abs_idx;
            if fm_end > 0 && after.as_bytes()[fm_end - 1] == b'\r' {
                fm_end -= 1;
            }
            let frontmatter = &after[..fm_end];
            let skip = if after[next_char_idx..].starts_with("\r\n") {
                2
            } else if after[next_char_idx..].starts_with('\n') {
                1
            } else {
                0
            };
            return Some((frontmatter, &after[next_char_idx + skip..]));
        }
        current = abs_idx + 4;
    }
    None
}

fn strip_leading_yaml_frontmatter(markdown: &str) -> &str {
    match split_yaml_frontmatter(markdown) {
        Some((_, body)) => body.trim_start_matches(['\n', '\r']),
        None => markdown,
    }
}

fn wrap_with_deck_frontmatter(markdown: &str, deck_name: &str) -> Fallible<String> {
    let mut table = toml::map::Map::new();
    table.insert(
        "name".to_string(),
        toml::Value::String(deck_name.to_string()),
    );
    let mut frontmatter_toml = toml::to_string(&toml::Value::Table(table))?;
    // Ensure the TOML block ends with a newline so the closing `---` stays on
    // its own line and the frontmatter parser can find it.
    if !frontmatter_toml.ends_with('\n') {
        frontmatter_toml.push('\n');
    }
    let body = strip_leading_yaml_frontmatter(markdown);
    Ok(format!("---\n{frontmatter_toml}---\n\n{body}"))
}

/// Parse the `title:` field from YAML (`---`) frontmatter.
fn frontmatter_title(markdown: &str) -> Option<String> {
    let (frontmatter, _) = split_yaml_frontmatter(markdown)?;
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("title:") {
            let title = rest.trim().trim_matches('"').trim_matches('\'');
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

/// Build a `ResolvedCollection` for a single HedgeDoc note.
///
/// The slug (and with it the note directory and database) is unique per
/// (note URL, owner), so nothing is ever shared between users.
pub fn resolved_collection(
    url: &str,
    data_dir: &Path,
    owner: Option<String>,
) -> ResolvedCollection {
    let slug = slug_for_note(url, owner.as_deref());
    let coll_dir = data_dir.join("hedgedoc").join(&slug);
    let db_path = data_dir.join("db").join(format!("{slug}.db"));
    ResolvedCollection {
        name: note_display_name(url),
        slug,
        coll_dir,
        db_path,
        owner,
    }
}

/// Placeholder collection name for a note that has not synced yet: the note
/// ID, qualified by its host. Replaced by the note's own title on the first
/// successful sync (see `apply_sync_result`).
fn note_display_name(url: &str) -> String {
    let note_id = note_id_from_url(url).unwrap_or_else(|| url.to_string());
    match source_uri_from_url(url) {
        Some(uri) => format!("{} / {note_id}", source_display_name(&uri)),
        None => note_id,
    }
}

/// Record a successful sync on a source: the note's own title becomes the
/// collection name, so the landing page shows the note rather than its host.
pub fn apply_sync_result(source: &mut HedgedocSource, deck_name: String, file_name: String) {
    source.collection.name = deck_name.clone();
    source.note.deck_name = deck_name;
    source.note.file_name = file_name;
    source.note.last_error = None;
}

/// Short, stable disambiguator derived from the exact note ID (blake3, the
/// same hash the rest of the codebase uses for content addressing). Two note
/// IDs that slugify identically (e.g. `abc+1` and `abc-1`) stay distinct.
fn note_id_disambiguator(note_id: &str) -> String {
    blake3::hash(note_id.as_bytes()).to_hex()[..8].to_string()
}

fn note_file_name(url: &str) -> String {
    let note_id = note_id_from_url(url).unwrap_or_else(|| url.to_string());
    let stem = slugify(&note_id);
    let stem = if stem.is_empty() {
        "note"
    } else {
        stem.as_str()
    };
    format!("{}-{}.md", stem, note_id_disambiguator(&note_id))
}

/// Delete the old note file layout (`{slug}.md`, without the hash suffix) if
/// it differs from the current file name, so servers upgraded across the
/// BUG-41 filename change don't parse the same note twice. Best-effort:
/// failure is logged, never fatal.
fn remove_legacy_note_file(coll_dir: &Path, url: &str, current_file_name: &str) {
    let note_id = note_id_from_url(url).unwrap_or_else(|| "note".to_string());
    let stem = slugify(&note_id);
    let stem = if stem.is_empty() {
        "note"
    } else {
        stem.as_str()
    };
    let legacy = format!("{stem}.md");
    if legacy == current_file_name {
        return;
    }
    let legacy_path = coll_dir.join(&legacy);
    if legacy_path.exists() {
        if let Err(e) = std::fs::remove_file(&legacy_path) {
            log::warn!(
                "Failed to remove legacy HedgeDoc note file {}: {e}",
                legacy_path.display()
            );
        }
    }
}

/// Fetch a HedgeDoc document, write it to `{rc.coll_dir}/{note}.md`, and
/// return the extracted deck title with file metadata.
pub async fn sync_source(url: &str, rc: &ResolvedCollection) -> Fallible<(String, String)> {
    let markdown = fetch_markdown(url).await?;
    let note_id = note_id_from_url(url).unwrap_or_default();
    let title = extract_title(&markdown, &note_id);
    let deck_name = sanitize_deck_name(&title, &note_id);
    let sync_markdown = wrap_with_deck_frontmatter(&markdown, &deck_name)?;
    let file_name = note_file_name(url);
    tokio::fs::create_dir_all(&rc.coll_dir).await?;
    // Atomic write: write to a temp file then rename so concurrent readers
    // never see a partially-written note file.
    let final_path = rc.coll_dir.join(&file_name);
    let tmp_path = rc.coll_dir.join(format!(".{}.tmp", file_name));
    tokio::fs::write(&tmp_path, sync_markdown).await?;
    // On Unix, rename over an existing file is atomic.
    // On Windows, rename fails if the destination exists, so remove it first
    // (non-atomic, but best-effort on that platform).
    #[cfg(windows)]
    if tokio::fs::metadata(&final_path).await.is_ok() {
        if let Err(e) = tokio::fs::remove_file(&final_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(e.into());
        }
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, &final_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(e.into());
    }
    remove_legacy_note_file(&rc.coll_dir, url, &file_name);
    Ok((deck_name, file_name))
}

/// Atomically commit an added HedgeDoc source or note (BUG-39): the duplicate
/// check, the mutation, and the config persist all happen under one lock
/// acquisition over the sources state, and the persist is computed from the
/// post-mutation state. On any error the in-memory state is left unchanged.
/// Returns the post-mutation snapshot on success.
pub fn commit_add(
    sources: &Mutex<Vec<HedgedocSource>>,
    config_path: Option<&Path>,
    url: &str,
    owner: Option<&str>,
    new_source: HedgedocSource,
) -> Fallible<Vec<HedgedocSource>> {
    let mut guard = sources.lock();
    // Scoped to the caller: the same public note added by two different
    // users is two separate collections, not a duplicate.
    if guard
        .iter()
        .any(|s| s.note.url == url && s.collection.owner.as_deref() == owner)
    {
        return fail(format!("HedgeDoc note is already configured: {url}"));
    }
    let mut updated = guard.clone();
    updated.push(new_source);
    if let Some(path) = config_path {
        persist_hedgedoc_entries(path, &all_hedgedoc_entries(&updated))?;
    }
    *guard = updated.clone();
    Ok(updated)
}

/// What a `commit_delete` removed: the deleted note's collection. BUG-42
/// uses this to clean up on-disk data and tell the user what remains.
pub struct DeleteOutcome {
    pub snapshot: Vec<HedgedocSource>,
    pub removed: ResolvedCollection,
}

/// Atomically commit a HedgeDoc source/note deletion (BUG-39): mutate and
/// persist under one lock acquisition, persisting from the post-mutation
/// state. On any error the in-memory state is left unchanged.
pub fn commit_delete(
    sources: &Mutex<Vec<HedgedocSource>>,
    config_path: Option<&Path>,
    url: &str,
    owner: Option<&str>,
) -> Fallible<DeleteOutcome> {
    let mut guard = sources.lock();
    let mut updated = guard.clone();
    // Matched on (url, owner): deleting your own note must never remove
    // another user's note with the same URL.
    let is_target =
        |s: &HedgedocSource| s.note.url == url && s.collection.owner.as_deref() == owner;
    let Some(removed) = updated
        .iter()
        .find(|s| is_target(s))
        .map(|s| s.collection.clone())
    else {
        return fail(format!("No HedgeDoc source with this URL: {url}"));
    };
    updated.retain(|s| !is_target(s));
    if let Some(path) = config_path {
        persist_hedgedoc_entries(path, &all_hedgedoc_entries(&updated))?;
    }
    *guard = updated.clone();
    Ok(DeleteOutcome {
        snapshot: updated,
        removed,
    })
}

/// Clean up on-disk data after a committed delete (BUG-42) and build the
/// user-facing message describing exactly what was deleted and what remains.
/// The review database is kept deliberately, so review history survives if
/// the source is re-added later. Cleanup failures are reported, not fatal:
/// the source is already removed from config and memory at this point.
pub fn cleanup_after_delete(outcome: &DeleteOutcome) -> String {
    let rc = &outcome.removed;
    if let Err(e) = std::fs::remove_dir_all(&rc.coll_dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return format!(
                "Note removed, but its directory {} could not be deleted: {e}. \
                 The review database at {} was kept.",
                rc.coll_dir.display(),
                rc.db_path.display(),
            );
        }
    }
    format!(
        "Note removed. Its directory {} was deleted. The review database at {} was kept, \
         so your review history survives if you re-add this note.",
        rc.coll_dir.display(),
        rc.db_path.display(),
    )
}

/// A placeholder note carrying an initialization error. `last_error: Some`
/// is what the manage UI renders as the "Error" status.
pub fn error_note(url: &str, message: String) -> HedgedocNote {
    HedgedocNote {
        url: url.to_string(),
        deck_name: note_id_from_url(url).unwrap_or_else(|| "note".to_string()),
        file_name: note_file_name(url),
        last_error: Some(message),
    }
}

/// Build a source for `url`, never dropping it (BUG-40): if initialization
/// fails (unparseable URL, filesystem error, fetch failure), return a
/// placeholder source whose note carries the error, so the configured entry
/// stays in memory — and in the config file — with an Error status until the
/// user explicitly deletes it. The periodic sync retries it automatically.
pub async fn build_source_lossless(
    url: &str,
    data_dir: &Path,
    owner: Option<String>,
) -> HedgedocSource {
    match build_source(url, data_dir, owner.clone()).await {
        Ok(source) => source,
        Err(e) => {
            let msg = e.to_string();
            log::error!("Failed to initialize HedgeDoc source {url}: {msg}");
            let source_uri = source_uri_from_url(url).unwrap_or_else(|| url.to_string());
            let collection = resolved_collection(url, data_dir, owner);
            HedgedocSource {
                source_uri,
                collection,
                note: error_note(url, msg),
            }
        }
    }
}

/// Compute `CollectionInfo` for a single HedgeDoc source.
pub fn collection_info_for_source(source: &HedgedocSource) -> CollectionInfo {
    let (total_cards, due_today) =
        match compute_collection_counts(&source.collection.coll_dir, &source.collection.db_path) {
            Ok(counts) => counts,
            Err(e) => {
                log::warn!(
                    "Failed to count cards for HedgeDoc note '{}': {e}",
                    source.note.url
                );
                (0, 0)
            }
        };
    CollectionInfo {
        name: source.collection.name.clone(),
        slug: source.collection.slug.clone(),
        total_cards,
        due_today,
        owner: source.collection.owner.clone(),
    }
}

pub async fn build_note(url: &str, collection: &ResolvedCollection) -> Fallible<HedgedocNote> {
    let (deck_name, file_name) = match sync_source(url, collection).await {
        Ok(info) => info,
        Err(e) => {
            let msg = e.to_string();
            log::error!("Initial HedgeDoc sync failed for {url}: {msg}");
            return Ok(HedgedocNote {
                url: url.to_string(),
                deck_name: note_id_from_url(url).unwrap_or_else(|| "note".to_string()),
                file_name: note_file_name(url),
                last_error: Some(msg),
            });
        }
    };

    Ok(HedgedocNote {
        url: url.to_string(),
        deck_name,
        file_name,
        last_error: None,
    })
}

/// Re-derive a `HedgedocSource` from a URL, performing an initial sync for the note.
pub async fn build_source(
    url: &str,
    data_dir: &Path,
    owner: Option<String>,
) -> Fallible<HedgedocSource> {
    let source_uri = source_uri_from_url(url).ok_or_else(|| {
        crate::error::ErrorReport::new(format!("Cannot derive source URI from URL: {url}"))
    })?;

    let mut rc = resolved_collection(url, data_dir, owner);
    tokio::fs::create_dir_all(&rc.coll_dir).await?;
    if let Some(parent) = rc.db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let note = build_note(url, &rc).await?;
    // A note that synced knows its own title; show that rather than the
    // host/ID placeholder.
    if note.last_error.is_none() {
        rc.name = note.deck_name.clone();
    }

    Ok(HedgedocSource {
        source_uri,
        collection: rc,
        note,
    })
}

/// Spawn a background task that periodically re-fetches all HedgeDoc sources.
pub fn spawn_hedgedoc_sync_task(
    hedgedoc_sources: Arc<Mutex<Vec<HedgedocSource>>>,
    collection_infos: Arc<RwLock<Vec<CollectionInfo>>>,
    hedgedoc_last_synced: Arc<Mutex<Option<Timestamp>>>,
    static_collections: Vec<ResolvedCollection>,
    poll_interval_minutes: u64,
) {
    if poll_interval_minutes == 0 {
        log::debug!("Periodic HedgeDoc sync disabled (poll_interval_minutes = 0)");
        return;
    }

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(poll_interval_minutes * 60));
        // Skip the first immediate tick — we already synced on startup.
        ticker.tick().await;

        loop {
            ticker.tick().await;
            log::debug!("Periodic HedgeDoc sync triggered");

            // Collect the URLs we need to sync (without holding the lock during await).
            let entries: Vec<(String, ResolvedCollection)> = {
                let sources = hedgedoc_sources.lock();
                sources
                    .iter()
                    .map(|s| (s.note.url.clone(), s.collection.clone()))
                    .collect()
            };

            let mut any_success = false;

            for (url, rc) in &entries {
                match sync_source(url, rc).await {
                    Ok((deck_name, file_name)) => {
                        any_success = true;
                        let mut sources = hedgedoc_sources.lock();
                        if let Some(src) = sources.iter_mut().find(|s| s.collection.slug == rc.slug)
                        {
                            apply_sync_result(src, deck_name, file_name);
                        }
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        log::error!("Periodic HedgeDoc sync failed for {url}: {msg}");
                        let mut sources = hedgedoc_sources.lock();
                        if let Some(src) = sources.iter_mut().find(|s| s.collection.slug == rc.slug)
                        {
                            src.note.last_error = Some(msg);
                        }
                    }
                }
            }

            // Snapshot sources before releasing the lock, then do FS/DB work outside it.
            let hedgedoc_snapshot = hedgedoc_sources.lock().clone();
            let static_collections_for_counts = static_collections.clone();
            let all_infos = match tokio::task::spawn_blocking(move || {
                build_combined_infos(&static_collections_for_counts, &hedgedoc_snapshot)
            })
            .await
            {
                Ok(infos) => infos,
                Err(e) => {
                    log::error!("Failed to compute collection counts: {e}");
                    continue;
                }
            };
            *collection_infos.write().await = all_infos;
            if any_success {
                *hedgedoc_last_synced.lock() = Some(Timestamp::now());
            }
            log::debug!("Periodic HedgeDoc sync complete");
        }
    });
}

/// Combine `CollectionInfo` for static (git/directory) and HedgeDoc sources.
pub fn build_combined_infos(
    static_collections: &[ResolvedCollection],
    hedgedoc_sources: &[HedgedocSource],
) -> Vec<CollectionInfo> {
    use crate::cmd::serve::git::refresh_collection_info;

    let mut infos = refresh_collection_info(static_collections);
    for src in hedgedoc_sources {
        infos.push(collection_info_for_source(src));
    }
    infos
}

pub fn all_hedgedoc_entries(hedgedoc_sources: &[HedgedocSource]) -> Vec<HedgedocEntry> {
    hedgedoc_sources
        .iter()
        .map(|s| HedgedocEntry {
            url: s.note.url.clone(),
            owner: s.collection.owner.clone(),
        })
        .collect()
}

/// Write the current set of HedgeDoc URLs back to the TOML config file.
/// Other config keys are preserved by value, but comments and key ordering
/// in the file are not guaranteed to survive the round-trip.
pub fn persist_hedgedoc_entries(config_path: &Path, entries: &[HedgedocEntry]) -> Fallible<()> {
    let content = std::fs::read_to_string(config_path)?;
    let mut doc: toml::Value = toml::from_str(&content)?;

    let table = doc
        .as_table_mut()
        .ok_or_else(|| crate::error::ErrorReport::new("Config is not a TOML table"))?;

    if entries.is_empty() {
        table.remove("hedgedoc");
    } else {
        let array: Vec<toml::Value> = entries
            .iter()
            .map(|e| {
                let mut t = toml::map::Map::new();
                t.insert("url".to_string(), toml::Value::String(e.url.clone()));
                // `owner` must survive the round-trip: dropping it rewrites
                // the config into one that `[oidc]` validation rejects at the
                // next startup, and would re-slug the note to a fresh, empty
                // database.
                if let Some(owner) = &e.owner {
                    t.insert("owner".to_string(), toml::Value::String(owner.clone()));
                }
                toml::Value::Table(t)
            })
            .collect();
        table.insert("hedgedoc".to_string(), toml::Value::Array(array));
    }

    let serialized = toml::to_string_pretty(&doc)?;
    // Atomic write: write to a temp file in the same directory then rename,
    // so a crash mid-write cannot corrupt the config.
    static WRITE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = config_path.parent().unwrap_or(Path::new("."));
    let tmp_path = dir.join(format!(
        ".hashcards-config-{}-{}.tmp",
        std::process::id(),
        n
    ));
    std::fs::write(&tmp_path, serialized)?;
    // On Unix, rename over an existing file is atomic.
    // On Windows, rename fails if the destination exists, so remove it first
    // (non-atomic, but best-effort on that platform).
    #[cfg(windows)]
    if config_path.exists() {
        if let Err(e) = std::fs::remove_file(config_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e.into());
        }
    }
    if let Err(e) = std::fs::rename(&tmp_path, config_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn note_id_strips_query_and_fragment() {
        assert_eq!(
            note_id_from_url("https://notes.example.com/abc123?foo=1"),
            Some("abc123".to_string())
        );
        assert_eq!(
            note_id_from_url("https://notes.example.com/abc123#section"),
            Some("abc123".to_string())
        );
        assert_eq!(
            note_id_from_url("https://notes.example.com/abc123"),
            Some("abc123".to_string())
        );
        assert_eq!(
            note_id_from_url("https://notes.example.com/abc123/"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn build_download_url_basic() {
        let url = build_download_url("https://notes.example.com/abc123").unwrap();
        assert_eq!(url.as_str(), "https://notes.example.com/abc123/download");
    }

    #[test]
    fn build_download_url_strips_query_and_fragment() {
        let url = build_download_url("https://notes.example.com/abc123?foo=1#bar").unwrap();
        assert_eq!(url.as_str(), "https://notes.example.com/abc123/download");
    }

    #[test]
    fn build_download_url_trailing_slash() {
        let url = build_download_url("https://notes.example.com/abc123/").unwrap();
        assert_eq!(url.as_str(), "https://notes.example.com/abc123/download");
    }

    #[test]
    fn build_download_url_shared_note_strips_s_prefix() {
        // Published/shared notes at /s/<noteId> use the same note ID as the
        // primary note. Strip /s/ so we fetch /<noteId>/download directly.
        let url = build_download_url("https://notes.example.com/s/NNOBZSN2Yi").unwrap();
        assert_eq!(
            url.as_str(),
            "https://notes.example.com/NNOBZSN2Yi/download"
        );
    }

    #[test]
    fn build_download_url_shared_note_trailing_slash() {
        let url = build_download_url("https://notes.example.com/s/NNOBZSN2Yi/").unwrap();
        assert_eq!(
            url.as_str(),
            "https://notes.example.com/NNOBZSN2Yi/download"
        );
    }

    #[test]
    fn validate_hedgedoc_url_rejects_http() {
        assert!(validate_hedgedoc_url("http://notes.example.com/abc123").is_err());
    }

    #[test]
    fn validate_hedgedoc_url_accepts_https() {
        assert!(validate_hedgedoc_url("https://notes.example.com/abc123").is_ok());
    }

    fn write_toml(path: &Path, content: &str) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn persist_adds_hedgedoc_array() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hashcards.toml");
        write_toml(&config_path, "[server]\ndata_dir = \"/tmp\"\n");

        let entries = vec![
            HedgedocEntry {
                url: "https://notes.example.com/doc1".to_string(),
                owner: None,
            },
            HedgedocEntry {
                url: "https://notes.example.com/doc2".to_string(),
                owner: None,
            },
        ];
        persist_hedgedoc_entries(&config_path, &entries).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let value: toml::Value = toml::from_str(&content).unwrap();
        let table = value.as_table().unwrap();
        assert!(table.contains_key("server"));
        let arr = table["hedgedoc"].as_array().unwrap();
        let urls: Vec<&str> = arr
            .iter()
            .map(|v| v.as_table().unwrap()["url"].as_str().unwrap())
            .collect();
        assert_eq!(
            urls,
            vec![
                "https://notes.example.com/doc1",
                "https://notes.example.com/doc2"
            ]
        );
    }

    #[test]
    fn persist_replaces_existing_array() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hashcards.toml");
        write_toml(
            &config_path,
            "[[hedgedoc]]\nurl = \"https://old.example.com/old\"\n[server]\ndata_dir = \"/tmp\"\n",
        );

        let entries = vec![HedgedocEntry {
            url: "https://new.example.com/new".to_string(),
            owner: None,
        }];
        persist_hedgedoc_entries(&config_path, &entries).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let value: toml::Value = toml::from_str(&content).unwrap();
        let arr = value.as_table().unwrap()["hedgedoc"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0].as_table().unwrap()["url"].as_str().unwrap(),
            "https://new.example.com/new"
        );
    }

    #[test]
    fn persist_removes_array_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hashcards.toml");
        write_toml(
            &config_path,
            "[[hedgedoc]]\nurl = \"https://example.com/doc\"\n[server]\ndata_dir = \"/tmp\"\n",
        );

        persist_hedgedoc_entries(&config_path, &[]).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let value: toml::Value = toml::from_str(&content).unwrap();
        let table = value.as_table().unwrap();
        assert!(!table.contains_key("hedgedoc"));
        assert!(table.contains_key("server"));
    }

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

    /// Regression test (BUG-41): `abc+1` and `abc-1` both slugify to `abc-1`,
    /// so without a disambiguating hash their note files collide.
    #[test]
    fn note_file_name_distinguishes_colliding_note_ids() {
        let a = note_file_name("https://notes.example.com/abc+1");
        let b = note_file_name("https://notes.example.com/abc-1");
        assert_ne!(a, b);
        assert!(a.ends_with(".md"));
        assert!(b.ends_with(".md"));
    }

    #[test]
    fn note_file_name_is_stable_for_the_same_url() {
        assert_eq!(
            note_file_name("https://notes.example.com/abc123"),
            note_file_name("https://notes.example.com/abc123"),
        );
    }

    /// After the filename change, the pre-existing un-hashed file must be
    /// removed on sync or the collection parses the same note twice.
    #[test]
    fn remove_legacy_note_file_deletes_old_unhashed_file() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://notes.example.com/abc123";
        let current = note_file_name(url);
        std::fs::write(dir.path().join("abc123.md"), "old").unwrap();
        std::fs::write(dir.path().join(&current), "new").unwrap();
        remove_legacy_note_file(dir.path(), url, &current);
        assert!(!dir.path().join("abc123.md").exists());
        assert!(dir.path().join(&current).exists());
    }

    fn test_note_for(url: &str) -> HedgedocNote {
        HedgedocNote {
            url: url.to_string(),
            deck_name: "Deck".to_string(),
            file_name: "deck.md".to_string(),
            last_error: None,
        }
    }

    fn test_source_for(uri: &str, url: &str) -> HedgedocSource {
        test_source_owned_by(uri, url, None)
    }

    fn test_source_owned_by(uri: &str, url: &str, owner: Option<&str>) -> HedgedocSource {
        let owner = owner.map(|o| o.to_string());
        HedgedocSource {
            source_uri: uri.to_string(),
            collection: ResolvedCollection {
                name: uri.to_string(),
                slug: slug_for_note(url, owner.as_deref()),
                coll_dir: PathBuf::from("/nonexistent/hedgedoc/test"),
                db_path: PathBuf::from("/nonexistent/db/test.db"),
                owner,
            },
            note: test_note_for(url),
        }
    }

    #[test]
    fn commit_add_rejects_duplicate_and_leaves_state_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hashcards.toml");
        write_toml(&config_path, "[server]\ndata_dir = \"/tmp\"\n");
        let sources = Mutex::new(vec![test_source_for(
            "https://n.example.com",
            "https://n.example.com/doc1",
        )]);

        let result = commit_add(
            &sources,
            Some(&config_path),
            "https://n.example.com/doc1",
            None,
            test_source_for("https://n.example.com", "https://n.example.com/doc1"),
        );
        assert!(result.is_err());
        assert_eq!(sources.lock().len(), 1);
    }

    #[test]
    fn commit_add_persist_failure_leaves_memory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        // Config path in a directory that does not exist: persist must fail.
        let missing_config = dir.path().join("no-such-dir").join("hashcards.toml");
        let sources = Mutex::new(vec![test_source_for(
            "https://n.example.com",
            "https://n.example.com/doc1",
        )]);

        let result = commit_add(
            &sources,
            Some(&missing_config),
            "https://n.example.com/doc2",
            None,
            test_source_for("https://n.example.com", "https://n.example.com/doc2"),
        );
        assert!(result.is_err());
        assert_eq!(sources.lock().len(), 1);
    }

    /// Regression test (BUG-39): concurrent adds must not lose entries. The
    /// old handler snapshotted, persisted, and applied under separate lock
    /// acquisitions, so interleaved adds could persist a config missing each
    /// other's notes. `commit_add` holds one lock across check+mutate+persist.
    #[test]
    fn concurrent_commit_adds_lose_no_entries() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hashcards.toml");
        write_toml(&config_path, "[server]\ndata_dir = \"/tmp\"\n");
        let sources = Arc::new(Mutex::new(vec![test_source_for(
            "https://n.example.com",
            "https://n.example.com/doc0",
        )]));

        let mut handles = Vec::new();
        for i in 1..=8 {
            let sources = sources.clone();
            let config_path = config_path.clone();
            handles.push(std::thread::spawn(move || {
                let url = format!("https://n.example.com/doc{i}");
                commit_add(
                    &sources,
                    Some(&config_path),
                    &url,
                    None,
                    test_source_for("https://n.example.com", &url),
                )
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(sources.lock().len(), 9);
        let content = std::fs::read_to_string(&config_path).unwrap();
        for i in 0..=8 {
            assert!(content.contains(&format!("doc{i}")), "config lost doc{i}");
        }
    }

    #[test]
    fn commit_delete_removes_note_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hashcards.toml");
        write_toml(
            &config_path,
            "[[hedgedoc]]\nurl = \"https://n.example.com/doc1\"\n[server]\ndata_dir = \"/tmp\"\n",
        );
        let sources = Mutex::new(vec![test_source_for(
            "https://n.example.com",
            "https://n.example.com/doc1",
        )]);

        let outcome = commit_delete(
            &sources,
            Some(&config_path),
            "https://n.example.com/doc1",
            None,
        )
        .unwrap();
        assert!(outcome.snapshot.is_empty());
        assert_eq!(outcome.removed.owner, None);
        assert!(sources.lock().is_empty());
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(!content.contains("doc1"));
    }

    #[test]
    fn commit_delete_unknown_url_is_an_error_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hashcards.toml");
        write_toml(&config_path, "[server]\ndata_dir = \"/tmp\"\n");
        let sources = Mutex::new(vec![test_source_for(
            "https://n.example.com",
            "https://n.example.com/doc1",
        )]);

        let result = commit_delete(
            &sources,
            Some(&config_path),
            "https://n.example.com/other",
            None,
        );
        assert!(result.is_err());
        assert_eq!(sources.lock().len(), 1);
    }

    /// Regression test (BUG-39): a failed persist must not desync memory from
    /// the config file — the in-memory sources stay untouched.
    #[test]
    fn commit_delete_persist_failure_leaves_memory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let missing_config = dir.path().join("no-such-dir").join("hashcards.toml");
        let sources = Mutex::new(vec![test_source_for(
            "https://n.example.com",
            "https://n.example.com/doc1",
        )]);

        let result = commit_delete(
            &sources,
            Some(&missing_config),
            "https://n.example.com/doc1",
            None,
        );
        assert!(result.is_err());
        assert_eq!(sources.lock().len(), 1);
    }

    /// Regression test (BUG-42): deleting a source's last note must delete the
    /// note directory, keep the review DB, and say so.
    #[test]
    fn cleanup_after_delete_removes_dir_and_keeps_db() {
        let dir = tempfile::tempdir().unwrap();
        let coll_dir = dir.path().join("hedgedoc").join("src");
        let db_path = dir.path().join("db").join("hedgedoc-src.db");
        std::fs::create_dir_all(&coll_dir).unwrap();
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(coll_dir.join("note.md"), "Q: q\nA: a\n").unwrap();
        std::fs::write(&db_path, "not a real db").unwrap();

        let rc = ResolvedCollection {
            name: "src".to_string(),
            slug: "hedgedoc-src".to_string(),
            coll_dir: coll_dir.clone(),
            db_path: db_path.clone(),
            owner: None,
        };
        let outcome = DeleteOutcome {
            snapshot: Vec::new(),
            removed: rc,
        };

        let message = cleanup_after_delete(&outcome);
        assert!(!coll_dir.exists(), "note directory must be deleted");
        assert!(db_path.exists(), "review DB must be kept");
        assert!(
            message.contains("kept"),
            "message must say the DB was kept: {message}"
        );
        assert!(
            message.contains(&db_path.display().to_string()),
            "message must name the kept DB: {message}"
        );
    }

    /// Regression test (BUG-40): a source that fails to initialize at startup
    /// must stay in memory with an Error status — and therefore survive the
    /// config round-trip — instead of being silently dropped and then written
    /// out of the config by the next add/delete.
    #[tokio::test]
    async fn build_source_lossless_keeps_failing_entry_with_error_status() {
        let dir = tempfile::tempdir().unwrap();
        let source = build_source_lossless("not a valid url", dir.path(), None).await;

        assert_eq!(source.note.url, "not a valid url");
        assert!(
            source.note.last_error.is_some(),
            "failing source must carry an Error status"
        );

        // The entry survives the config persistence round-trip.
        let entries = all_hedgedoc_entries(&[source]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "not a valid url");
    }

    /// BUG-43 regression: a `[[hedgedoc]]` source configured at startup that
    /// slugifies onto a configured collection must stop the server, not
    /// silently route to the wrong database.
    #[test]
    fn test_startup_hedgedoc_slug_collision_is_rejected() {
        let uri = "https://pad.example.com/notes";
        let slug = slug_for_note(uri, None);
        let collections = vec![ResolvedCollection {
            name: "Notes".to_string(),
            slug: slug.clone(),
            coll_dir: PathBuf::from("/tmp/notes"),
            db_path: PathBuf::from("/tmp/notes.db"),
            owner: None,
        }];
        let sources = vec![HedgedocSource {
            source_uri: uri.to_string(),
            collection: ResolvedCollection {
                name: uri.to_string(),
                slug,
                coll_dir: PathBuf::from("/tmp/hedge"),
                db_path: PathBuf::from("/tmp/hedge.db"),
                owner: None,
            },
            note: test_note_for(uri),
        }];
        assert!(check_startup_slug_collisions(&collections, &sources).is_err());
        assert!(check_startup_slug_collisions(&collections, &[]).is_ok());
    }

    /// Two notes whose IDs slugify identically stay distinct: the slug ends
    /// in a hash of the full URL, so `abc+1` and `abc-1` do not collide.
    #[test]
    fn test_notes_slugifying_alike_stay_distinct() {
        let a = slug_for_note("https://pad.example.com/abc+1", None);
        let b = slug_for_note("https://pad.example.com/abc-1", None);
        assert_ne!(a, b, "the URL hash must keep look-alike note IDs apart");
    }

    /// The HIGH finding this restructure fixes: two users adding notes from
    /// the same HedgeDoc host must get separate collections, databases and
    /// owners. Grouping by host used to give the second user's note to
    /// whoever was configured first.
    #[tokio::test]
    async fn notes_from_one_host_are_never_shared_between_users() {
        let dir = tempfile::tempdir().unwrap();
        let alice = build_source_lossless(
            "https://pad.example.com/alice-note",
            dir.path(),
            Some("alice@example.com".to_string()),
        )
        .await;
        let bob = build_source_lossless(
            "https://pad.example.com/bob-note",
            dir.path(),
            Some("bob@example.com".to_string()),
        )
        .await;

        assert_ne!(alice.collection.slug, bob.collection.slug);
        assert_ne!(alice.collection.db_path, bob.collection.db_path);
        assert_ne!(alice.collection.coll_dir, bob.collection.coll_dir);
        assert_eq!(alice.collection.owner.as_deref(), Some("alice@example.com"));
        assert_eq!(bob.collection.owner.as_deref(), Some("bob@example.com"));

        // The config round-trip keeps each entry with its own owner rather
        // than re-deriving both from one shared source.
        let entries = all_hedgedoc_entries(&[alice, bob]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].owner.as_deref(), Some("alice@example.com"));
        assert_eq!(entries[1].owner.as_deref(), Some("bob@example.com"));
    }

    /// Two users may add the very same public note. They must not share a
    /// slug, and therefore not a review database.
    #[test]
    fn same_note_added_by_two_users_gets_two_slugs() {
        let url = "https://pad.example.com/shared";
        let alice = slug_for_note(url, Some("alice@example.com"));
        let bob = slug_for_note(url, Some("bob@example.com"));
        assert_ne!(alice, bob);
    }

    /// Deleting your own note must not remove another user's note with the
    /// same URL.
    #[test]
    fn commit_delete_only_removes_the_callers_note() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hashcards.toml");
        write_toml(&config_path, "[server]\ndata_dir = \"/tmp\"\n");
        let url = "https://n.example.com/shared";
        let sources = Mutex::new(vec![
            test_source_owned_by("https://n.example.com", url, Some("alice@example.com")),
            test_source_owned_by("https://n.example.com", url, Some("bob@example.com")),
        ]);

        let outcome =
            commit_delete(&sources, Some(&config_path), url, Some("alice@example.com")).unwrap();
        assert_eq!(outcome.removed.owner.as_deref(), Some("alice@example.com"));
        assert_eq!(sources.lock().len(), 1);
        assert_eq!(
            sources.lock()[0].collection.owner.as_deref(),
            Some("bob@example.com")
        );
    }

    /// The same note added by two users is not a duplicate for the second
    /// user.
    #[test]
    fn commit_add_allows_the_same_note_for_a_different_owner() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hashcards.toml");
        write_toml(&config_path, "[server]\ndata_dir = \"/tmp\"\n");
        let url = "https://n.example.com/shared";
        let sources = Mutex::new(vec![test_source_owned_by(
            "https://n.example.com",
            url,
            Some("alice@example.com"),
        )]);

        let added = commit_add(
            &sources,
            Some(&config_path),
            url,
            Some("bob@example.com"),
            test_source_owned_by("https://n.example.com", url, Some("bob@example.com")),
        );
        assert!(added.is_ok(), "{:?}", added.err().map(|e| e.to_string()));
        assert_eq!(sources.lock().len(), 2);
    }

    /// BUG-43 regression: a configured collection whose slug equals a
    /// HedgeDoc source slug must be reported as a collision.
    #[test]
    fn test_hedgedoc_slug_collision_is_detected() {
        let source_uri = "demo@hedgedoc.example.org";
        let hedgedoc_slug = slug_for_note(source_uri, None);
        let colliding = ResolvedCollection {
            name: "Sneaky".to_string(),
            slug: hedgedoc_slug.clone(),
            coll_dir: PathBuf::from("/tmp/sneaky"),
            db_path: PathBuf::from("/tmp/sneaky.db"),
            owner: None,
        };
        let harmless = ResolvedCollection {
            name: "Fine".to_string(),
            slug: "fine".to_string(),
            coll_dir: PathBuf::from("/tmp/fine"),
            db_path: PathBuf::from("/tmp/fine.db"),
            owner: None,
        };

        let collections = vec![harmless, colliding];
        let hit = find_slug_collision(&hedgedoc_slug, &collections);
        assert_eq!(hit.map(|c| c.name.as_str()), Some("Sneaky"));
        assert!(find_slug_collision("something-else", &collections).is_none());
    }

    /// Regression: `owner` must survive a config rewrite. Dropping it left a
    /// config that `[oidc]` validation rejects at the next startup, and would
    /// have re-slugged every note onto a fresh, empty review database.
    #[test]
    fn persist_keeps_the_owner_of_each_entry() -> Fallible<()> {
        use crate::cmd::serve::config::ResolvedServeConfig;
        use crate::cmd::serve::config::ServeConfig;

        let dir = tempfile::tempdir()?;
        let config_path = dir.path().join("hashcards.toml");
        let data_dir = dir.path().join("data");
        write_toml(
            &config_path,
            &format!(
                "[server]\ndata_dir = {:?}\n\n\
                 [oidc]\n\
                 issuer_url = \"https://idp.example.com\"\n\
                 client_id = \"abc\"\n\
                 client_secret = \"secret\"\n\
                 external_url = \"https://hashcards.example.com\"\n\
                 session_secret = \"a-very-long-random-session-secret-value\"\n",
                data_dir
            ),
        );

        let entries = vec![
            HedgedocEntry {
                url: "https://pad.example.com/alice".to_string(),
                owner: Some("alice@example.com".to_string()),
            },
            HedgedocEntry {
                url: "https://pad.example.com/bob".to_string(),
                owner: Some("bob@example.com".to_string()),
            },
        ];
        persist_hedgedoc_entries(&config_path, &entries)?;

        // The rewritten config must still load: with `[oidc]` set, an entry
        // without an owner is a hard error.
        let content = std::fs::read_to_string(&config_path)?;
        let config: ServeConfig = toml::from_str(&content)?;
        let resolved = ResolvedServeConfig::from_toml(config)?;
        assert_eq!(resolved.hedgedoc_entries.len(), 2);
        assert_eq!(
            resolved.hedgedoc_entries[0].owner.as_deref(),
            Some("alice@example.com")
        );
        assert_eq!(
            resolved.hedgedoc_entries[1].owner.as_deref(),
            Some("bob@example.com")
        );
        Ok(())
    }

    /// An entry with no owner (no `[oidc]`) stays ownerless rather than
    /// gaining an empty `owner` key.
    #[test]
    fn persist_omits_owner_when_there_is_none() -> Fallible<()> {
        let dir = tempfile::tempdir()?;
        let config_path = dir.path().join("hashcards.toml");
        write_toml(&config_path, "[server]\ndata_dir = \"/tmp\"\n");
        persist_hedgedoc_entries(
            &config_path,
            &[HedgedocEntry {
                url: "https://pad.example.com/abc".to_string(),
                owner: None,
            }],
        )?;
        let content = std::fs::read_to_string(&config_path)?;
        assert!(!content.contains("owner"), "config: {content}");
        Ok(())
    }
}
