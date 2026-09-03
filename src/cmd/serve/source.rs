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
    /// Short label shown as a badge on the Sources page.
    pub fn label(&self) -> &'static str {
        match self {
            SourceKind::Hedgedoc => "hedgedoc",
            SourceKind::Git => "git",
        }
    }
}

/// Which kind of source a pasted URL names, for display.
///
/// Shares `git_raw_url` with `raw_url`, so the badge cannot disagree with
/// where the markdown is actually fetched from.
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
    let mut parsed = Url::parse(url)
        .map_err(|e| ErrorReport::new(format!("Invalid source URL `{url}`: {e}")))?;
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
        let (kind, _) = raw_url("https://notes.example.com/abc123").unwrap();
        assert_eq!(kind, SourceKind::Hedgedoc);
        let (kind, _) = raw_url("https://notes.example.com/s/abc123").unwrap();
        assert_eq!(kind, SourceKind::Hedgedoc);
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

    #[test]
    fn the_badge_never_disagrees_with_the_fetch_target() {
        for url in [
            "https://github.com/me/cards/blob/main/a.md",
            "https://gitlab.com/me/cards/-/blob/main/a.md",
            "https://codeberg.org/me/cards/src/branch/main/a.md",
            "https://example.com/raw/a.md",
            "https://notes.example.com/abc123",
            "https://notes.example.com/s/abc123",
        ] {
            let (kind, _) = raw_url(url).unwrap();
            assert_eq!(detect_kind(url), kind, "disagreed for {url}");
        }
    }
}
