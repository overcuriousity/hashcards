use std::path::Path;
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

/// Build the collection slug for a HedgeDoc source URI.
pub fn slug_for_source_uri(source_uri: &str) -> String {
    format!("hedgedoc-{}", slugify(source_uri))
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

/// Build a `ResolvedCollection` for a HedgeDoc source given its note ID and
/// the resolved data directory.
pub fn resolved_collection(source_uri: &str, data_dir: &Path) -> ResolvedCollection {
    let slug = slug_for_source_uri(source_uri);
    let source_key = slugify(source_uri);
    let coll_dir = data_dir.join("hedgedoc").join(source_key);
    let db_path = data_dir.join("db").join(format!("{slug}.db"));
    ResolvedCollection {
        name: source_display_name(source_uri),
        slug,
        coll_dir,
        db_path,
    }
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
    source_uri: &str,
    new_source: Option<HedgedocSource>,
    new_note: Option<HedgedocNote>,
) -> Fallible<Vec<HedgedocSource>> {
    let mut guard = sources.lock();
    if guard
        .iter()
        .flat_map(|s| s.notes.iter())
        .any(|n| n.url == url)
    {
        return fail(format!("HedgeDoc note is already configured: {url}"));
    }
    let mut updated = guard.clone();
    if let Some(source) = new_source {
        match updated
            .iter_mut()
            .find(|s| s.source_uri == source.source_uri)
        {
            // Another request created this source concurrently: merge our
            // note into it rather than pushing a duplicate source.
            Some(existing) => existing.notes.extend(source.notes),
            None => updated.push(source),
        }
    } else if let Some(note) = new_note {
        match updated.iter_mut().find(|s| s.source_uri == source_uri) {
            Some(src) => src.notes.push(note),
            None => {
                return fail(format!(
                    "HedgeDoc source was removed while adding the note: {source_uri}"
                ));
            }
        }
    } else {
        return fail("Nothing to add for HedgeDoc source.");
    }
    if let Some(path) = config_path {
        persist_hedgedoc_entries(path, &all_hedgedoc_entries(&updated))?;
    }
    *guard = updated.clone();
    Ok(updated)
}

/// What a `commit_delete` removed. `removed_note` is the deleted note's file
/// name with its collection; `removed_sources` lists collections whose source
/// was removed entirely (no notes left). BUG-42 uses this to clean up
/// on-disk data and tell the user what remains.
pub struct DeleteOutcome {
    pub snapshot: Vec<HedgedocSource>,
    pub removed_note: Option<(String, ResolvedCollection)>,
    pub removed_sources: Vec<ResolvedCollection>,
}

/// Atomically commit a HedgeDoc source/note deletion (BUG-39): mutate and
/// persist under one lock acquisition, persisting from the post-mutation
/// state. On any error the in-memory state is left unchanged.
pub fn commit_delete(
    sources: &Mutex<Vec<HedgedocSource>>,
    config_path: Option<&Path>,
    url: &str,
) -> Fallible<DeleteOutcome> {
    let mut guard = sources.lock();
    let mut updated = guard.clone();
    let mut removed_note = None;
    for src in updated.iter_mut() {
        if let Some(note) = src.notes.iter().find(|n| n.url == url) {
            removed_note = Some((note.file_name.clone(), src.collection.clone()));
        }
        src.notes.retain(|n| n.url != url);
    }
    if removed_note.is_none() {
        return fail(format!("No HedgeDoc source with this URL: {url}"));
    }
    let removed_sources: Vec<ResolvedCollection> = updated
        .iter()
        .filter(|s| s.notes.is_empty())
        .map(|s| s.collection.clone())
        .collect();
    updated.retain(|s| !s.notes.is_empty());
    if let Some(path) = config_path {
        persist_hedgedoc_entries(path, &all_hedgedoc_entries(&updated))?;
    }
    *guard = updated.clone();
    Ok(DeleteOutcome {
        snapshot: updated,
        removed_note,
        removed_sources,
    })
}

/// Clean up on-disk data after a committed delete (BUG-42) and build the
/// user-facing message describing exactly what was deleted and what remains.
/// The review database is kept deliberately, so review history survives if
/// the source is re-added later. Cleanup failures are reported, not fatal:
/// the source is already removed from config and memory at this point.
pub fn cleanup_after_delete(outcome: &DeleteOutcome) -> String {
    // Whole source removed: delete its note directory.
    if let Some(rc) = outcome.removed_sources.first() {
        if let Err(e) = std::fs::remove_dir_all(&rc.coll_dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return format!(
                    "Source removed, but its note directory {} could not be deleted: {e}. \
                     The review database at {} was kept.",
                    rc.coll_dir.display(),
                    rc.db_path.display(),
                );
            }
        }
        return format!(
            "Source removed. Its note directory {} was deleted. The review database at {} \
             was kept, so your review history survives if you re-add this source.",
            rc.coll_dir.display(),
            rc.db_path.display(),
        );
    }
    // Only one note of a multi-note source removed: delete just its file.
    if let Some((file_name, rc)) = &outcome.removed_note {
        let path = rc.coll_dir.join(file_name);
        if let Err(e) = std::fs::remove_file(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return format!(
                    "Note removed, but its file {} could not be deleted: {e}",
                    path.display()
                );
            }
        }
        return format!(
            "Note removed and its file {} deleted. The review database at {} was kept, \
             so your review history survives if you re-add this note.",
            path.display(),
            rc.db_path.display(),
        );
    }
    "Source removed.".to_string()
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
pub async fn build_source_lossless(url: &str, data_dir: &Path) -> HedgedocSource {
    match build_source(url, data_dir).await {
        Ok(source) => source,
        Err(e) => {
            let msg = e.to_string();
            log::error!("Failed to initialize HedgeDoc source {url}: {msg}");
            let source_uri = source_uri_from_url(url).unwrap_or_else(|| url.to_string());
            let collection = resolved_collection(&source_uri, data_dir);
            HedgedocSource {
                source_uri,
                collection,
                notes: vec![error_note(url, msg)],
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
                    "Failed to count cards for HedgeDoc source '{}': {e}",
                    source.source_uri
                );
                (0, 0)
            }
        };
    CollectionInfo {
        name: source.collection.name.clone(),
        slug: source.collection.slug.clone(),
        total_cards,
        due_today,
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
pub async fn build_source(url: &str, data_dir: &Path) -> Fallible<HedgedocSource> {
    let source_uri = source_uri_from_url(url).ok_or_else(|| {
        crate::error::ErrorReport::new(format!("Cannot derive source URI from URL: {url}"))
    })?;

    let rc = resolved_collection(&source_uri, data_dir);
    tokio::fs::create_dir_all(&rc.coll_dir).await?;
    if let Some(parent) = rc.db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let note = build_note(url, &rc).await?;

    Ok(HedgedocSource {
        source_uri,
        collection: rc,
        notes: vec![note],
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
                    .flat_map(|s| {
                        s.notes
                            .iter()
                            .map(move |n| (n.url.clone(), s.collection.clone()))
                    })
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
                            if let Some(note) = src.notes.iter_mut().find(|n| &n.url == url) {
                                note.deck_name = deck_name;
                                note.file_name = file_name;
                                note.last_error = None;
                            }
                        }
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        log::error!("Periodic HedgeDoc sync failed for {url}: {msg}");
                        let mut sources = hedgedoc_sources.lock();
                        for src in sources.iter_mut() {
                            if let Some(note) = src.notes.iter_mut().find(|n| &n.url == url) {
                                note.last_error = Some(msg.clone());
                                break;
                            }
                        }
                    }
                }
            }

            // Snapshot sources before releasing the lock, then do FS/DB work outside it.
            let hedgedoc_snapshot = hedgedoc_sources.lock().clone();
            let all_infos = build_combined_infos(&static_collections, &hedgedoc_snapshot);
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
        .flat_map(|s| s.notes.iter().map(|n| HedgedocEntry { url: n.url.clone() }))
        .collect()
}

/// Create a minimal hashcards.toml config file in the current working directory
/// if it doesn't already exist. `data_dir` should be the actual data directory
/// already in use by the running server so the generated config matches it.
pub fn create_minimal_config(data_dir: &Path) -> Fallible<PathBuf> {
    let config_path = std::env::current_dir()?.join("hashcards.toml");

    if config_path.exists() {
        return Ok(config_path);
    }

    let minimal_config = format!(
        "# hashcards server configuration\n# Auto-generated on first HedgeDoc source add\n\n[server]\ndata_dir = {:?}\n",
        data_dir.to_string_lossy()
    );

    std::fs::write(&config_path, minimal_config)?;
    Ok(config_path)
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
            },
            HedgedocEntry {
                url: "https://notes.example.com/doc2".to_string(),
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
        HedgedocSource {
            source_uri: uri.to_string(),
            collection: ResolvedCollection {
                name: uri.to_string(),
                slug: slug_for_source_uri(uri),
                coll_dir: PathBuf::from("/nonexistent/hedgedoc/test"),
                db_path: PathBuf::from("/nonexistent/db/test.db"),
            },
            notes: vec![test_note_for(url)],
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
            "https://n.example.com",
            None,
            Some(test_note_for("https://n.example.com/doc1")),
        );
        assert!(result.is_err());
        assert_eq!(sources.lock()[0].notes.len(), 1);
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
            "https://n.example.com",
            None,
            Some(test_note_for("https://n.example.com/doc2")),
        );
        assert!(result.is_err());
        assert_eq!(sources.lock()[0].notes.len(), 1);
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
                    "https://n.example.com",
                    None,
                    Some(test_note_for(&url)),
                )
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(sources.lock()[0].notes.len(), 9);
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

        let outcome =
            commit_delete(&sources, Some(&config_path), "https://n.example.com/doc1").unwrap();
        // The note's source had no other notes, so the whole source is gone.
        assert!(outcome.snapshot.is_empty());
        assert_eq!(outcome.removed_sources.len(), 1);
        assert!(outcome.removed_note.is_some());
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

        let result = commit_delete(&sources, Some(&config_path), "https://n.example.com/other");
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
        );
        assert!(result.is_err());
        assert_eq!(sources.lock().len(), 1);
        assert_eq!(sources.lock()[0].notes.len(), 1);
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
        };
        let outcome = DeleteOutcome {
            snapshot: Vec::new(),
            removed_note: Some(("note.md".to_string(), rc.clone())),
            removed_sources: vec![rc],
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

    /// When the source still has other notes, only the deleted note's file
    /// goes; the directory (and the other notes' files) stay.
    #[test]
    fn cleanup_after_delete_note_only_removes_just_that_file() {
        let dir = tempfile::tempdir().unwrap();
        let coll_dir = dir.path().join("hedgedoc").join("src");
        std::fs::create_dir_all(&coll_dir).unwrap();
        std::fs::write(coll_dir.join("gone.md"), "x").unwrap();
        std::fs::write(coll_dir.join("stays.md"), "y").unwrap();

        let rc = ResolvedCollection {
            name: "src".to_string(),
            slug: "hedgedoc-src".to_string(),
            coll_dir: coll_dir.clone(),
            db_path: dir.path().join("db").join("hedgedoc-src.db"),
        };
        let outcome = DeleteOutcome {
            snapshot: Vec::new(), // irrelevant for cleanup
            removed_note: Some(("gone.md".to_string(), rc)),
            removed_sources: Vec::new(),
        };

        let message = cleanup_after_delete(&outcome);
        assert!(!coll_dir.join("gone.md").exists());
        assert!(coll_dir.join("stays.md").exists());
        assert!(
            message.contains("kept"),
            "message must say what remains: {message}"
        );
    }

    /// Regression test (BUG-40): a source that fails to initialize at startup
    /// must stay in memory with an Error status — and therefore survive the
    /// config round-trip — instead of being silently dropped and then written
    /// out of the config by the next add/delete.
    #[tokio::test]
    async fn build_source_lossless_keeps_failing_entry_with_error_status() {
        let dir = tempfile::tempdir().unwrap();
        let source = build_source_lossless("not a valid url", dir.path()).await;

        assert_eq!(source.notes.len(), 1);
        assert_eq!(source.notes[0].url, "not a valid url");
        assert!(
            source.notes[0].last_error.is_some(),
            "failing source must carry an Error status"
        );

        // The entry survives the config persistence round-trip.
        let entries = all_hedgedoc_entries(&[source]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "not a valid url");
    }

    /// BUG-43 regression: a configured collection whose slug equals a
    /// HedgeDoc source slug must be reported as a collision.
    #[test]
    fn test_hedgedoc_slug_collision_is_detected() {
        let source_uri = "demo@hedgedoc.example.org";
        let hedgedoc_slug = slug_for_source_uri(source_uri);
        let colliding = ResolvedCollection {
            name: "Sneaky".to_string(),
            slug: hedgedoc_slug.clone(),
            coll_dir: PathBuf::from("/tmp/sneaky"),
            db_path: PathBuf::from("/tmp/sneaky.db"),
        };
        let harmless = ResolvedCollection {
            name: "Fine".to_string(),
            slug: "fine".to_string(),
            coll_dir: PathBuf::from("/tmp/fine"),
            db_path: PathBuf::from("/tmp/fine.db"),
        };

        let collections = vec![harmless, colliding];
        let hit = find_slug_collision(&hedgedoc_slug, &collections);
        assert_eq!(hit.map(|c| c.name.as_str()), Some("Sneaky"));
        assert!(find_slug_collision("something-else", &collections).is_none());
    }
}
