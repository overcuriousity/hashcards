use std::collections::HashMap;
use std::env::current_dir;
use std::fs::read_to_string;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use crate::cmd::drill::render::AnswerControls;
use crate::error::Fallible;
use crate::error::fail;
use crate::types::performance::Jitter;

// --- TOML deserialization structs ---

#[derive(Deserialize)]
pub struct ServeConfig {
    pub server: ServerSection,
    #[serde(default)]
    pub git: Option<GitSection>,
    #[serde(default)]
    pub defaults: DefaultsSection,
    #[serde(default)]
    pub oidc: Option<OidcSection>,
    #[serde(rename = "collection", default)]
    pub collections: Vec<CollectionEntry>,
    #[serde(rename = "hedgedoc", default)]
    pub hedgedoc: Vec<HedgedocEntry>,
    #[serde(rename = "deck", default)]
    pub decks: Vec<CustomDeckEntry>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct HedgedocEntry {
    pub url: String,
    #[serde(default)]
    pub owner: Option<String>,
}

/// A user-assembled deck: a named selection of decks drawn from any of the
/// owner's collections, drilled as one session.
///
/// `members` are `"{collection-slug}/{deck-name}"` pairs. Reviews still go to
/// each card's own collection database, so a card keeps exactly one schedule
/// no matter how many custom decks include it.
#[derive(Deserialize, Serialize, Clone)]
pub struct CustomDeckEntry {
    pub name: String,
    pub members: Vec<String>,
    #[serde(default)]
    pub owner: Option<String>,
}

/// One `members` entry, split into the collection it names and the deck
/// within it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeckMember {
    pub collection_slug: String,
    pub deck_name: String,
}

impl DeckMember {
    /// Parse `"{slug}/{deck}"`. Deck names may contain `/` (they mirror the
    /// file tree), so only the first separator splits.
    pub fn parse(raw: &str) -> Option<Self> {
        let (slug, deck) = raw.split_once('/')?;
        if slug.is_empty() || deck.is_empty() {
            return None;
        }
        Some(Self {
            collection_slug: slug.to_string(),
            deck_name: deck.to_string(),
        })
    }

    pub fn encode(&self) -> String {
        format!("{}/{}", self.collection_slug, self.deck_name)
    }
}

#[derive(Deserialize)]
pub struct ServerSection {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub data_dir: String,
    /// Evict drill sessions idle for this many minutes, closing their DB
    /// session row. 0 disables eviction. Default: 1440 (24 hours).
    #[serde(default = "default_session_timeout_minutes")]
    pub session_timeout_minutes: u64,
}

/// Default bind address. Deliberately localhost-only: without an `[oidc]`
/// section there is no authentication at all, so binding to every interface
/// must be an explicit opt-in (`host = "0.0.0.0"` in the config file).
fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8000
}

fn default_session_timeout_minutes() -> u64 {
    1440
}

#[derive(Deserialize)]
pub struct GitSection {
    pub repo_url: Option<String>,
    #[serde(default = "default_branch")]
    pub branch: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_minutes: u64,
    /// Author name for auto-commits of in-browser edits.
    #[serde(default = "default_commit_author_name")]
    pub commit_author_name: String,
    /// Author email for auto-commits of in-browser edits.
    #[serde(default = "default_commit_author_email")]
    pub commit_author_email: String,
}

fn default_branch() -> String {
    "main".to_string()
}

fn default_poll_interval() -> u64 {
    30
}

fn default_commit_author_name() -> String {
    "hashcards web edit".to_string()
}

fn default_commit_author_email() -> String {
    "hashcards@localhost".to_string()
}

#[derive(Deserialize)]
pub struct OidcSection {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub external_url: String,
    pub session_secret: String,
    #[serde(default = "default_oidc_scopes")]
    pub scopes: Vec<String>,
}

fn default_oidc_scopes() -> Vec<String> {
    vec![
        "openid".to_string(),
        "email".to_string(),
        "profile".to_string(),
    ]
}

/// `Key::derive_from` (the cookie signing key) panics on anything shorter,
/// so this is the minimum a config may declare.
pub const MIN_SESSION_SECRET_BYTES: usize = 32;

#[derive(Clone)]
pub struct ResolvedOidc {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub external_url: String,
    pub session_secret: String,
    pub scopes: Vec<String>,
}

#[derive(Deserialize)]
pub struct DefaultsSection {
    #[serde(default = "default_answer_controls")]
    pub answer_controls: AnswerControlsConfig,
    #[serde(default = "default_true")]
    pub bury_siblings: bool,
    #[serde(default = "default_jitter")]
    pub jitter: f64,
}

fn default_jitter() -> f64 {
    Jitter::DEFAULT_FRACTION
}

impl Default for DefaultsSection {
    fn default() -> Self {
        Self {
            answer_controls: default_answer_controls(),
            bury_siblings: true,
            jitter: default_jitter(),
        }
    }
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum AnswerControlsConfig {
    Full,
    Binary,
}

fn default_answer_controls() -> AnswerControlsConfig {
    AnswerControlsConfig::Full
}

fn default_true() -> bool {
    true
}

impl From<AnswerControlsConfig> for AnswerControls {
    fn from(config: AnswerControlsConfig) -> Self {
        match config {
            AnswerControlsConfig::Full => AnswerControls::Full,
            AnswerControlsConfig::Binary => AnswerControls::Binary,
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
pub struct CollectionEntry {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub owner: Option<String>,
}

impl CollectionEntry {
    pub fn slug(&self) -> String {
        slugify(&self.path)
    }
}

pub fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub fn load_config(path: &Path) -> Fallible<ServeConfig> {
    let content = read_to_string(path)?;
    let config: ServeConfig = toml::from_str(&content)?;
    Jitter::new(config.defaults.jitter)?;
    Ok(config)
}

// --- Resolved runtime config ---

pub struct ResolvedGit {
    pub repo_url: String,
    pub branch: String,
    pub poll_interval_minutes: u64,
    pub commit_author_name: String,
    pub commit_author_email: String,
    pub repo_dir: PathBuf,
    pub db_dir: PathBuf,
}

#[derive(Clone)]
pub struct ResolvedCollection {
    pub name: String,
    pub slug: String,
    pub coll_dir: PathBuf,
    pub db_path: PathBuf,
    /// Owning user's email (lowercased), when `[oidc]` is configured.
    pub owner: Option<String>,
}

pub struct ResolvedServeConfig {
    pub host: String,
    pub port: u16,
    pub git: Option<ResolvedGit>,
    pub defaults: DefaultsSection,
    pub collections: Vec<ResolvedCollection>,
    /// The directory holding the repo clone and the review databases.
    pub data_dir: Option<PathBuf>,
    /// Config file path; needed to persist UI changes back to disk.
    pub config_path: Option<PathBuf>,
    /// HedgeDoc source URLs loaded from the config file.
    pub hedgedoc_entries: Vec<HedgedocEntry>,
    /// User-assembled cross-collection decks loaded from the config file.
    pub custom_decks: Vec<CustomDeckEntry>,
    /// Idle drill sessions are evicted after this many minutes (0 = never).
    pub session_timeout_minutes: u64,
    /// Set when `[oidc]` is configured. Gates every route except `/auth/*`
    /// behind login and scopes collections/notes to their `owner`.
    pub oidc: Option<ResolvedOidc>,
}

/// Reject collections whose names map to the same URL slug.
///
/// `slugify` collapses every non-alphanumeric character to `-`, so distinct
/// paths like `a/b` and `a-b` collide and would silently share one database.
fn check_slug_collisions(collections: &[ResolvedCollection]) -> Fallible<()> {
    let mut seen: HashMap<&str, &ResolvedCollection> = HashMap::new();
    for rc in collections {
        if let Some(first) = seen.get(rc.slug.as_str()) {
            return fail(format!(
                "configuration error: collections '{}' and '{}' both map to the URL slug '{}'. Rename one of them so their slugs differ.",
                first.name, rc.name, rc.slug
            ));
        }
        seen.insert(rc.slug.as_str(), rc);
    }
    Ok(())
}

impl ResolvedServeConfig {
    pub fn from_toml(config: ServeConfig) -> Fallible<Self> {
        let data_dir = {
            let p = PathBuf::from(&config.server.data_dir);
            if p.is_absolute() {
                p
            } else {
                current_dir()?.join(p)
            }
        };
        let repo_dir = data_dir.join("repo");
        let db_dir = data_dir.join("db");

        let collections = config
            .collections
            .iter()
            .map(|entry| {
                let entry_path = PathBuf::from(&entry.path);
                // `is_absolute()` alone misses a path that `has_root()` but,
                // on Windows, no drive prefix (e.g. `/etc`, or a UNC-less
                // `\etc`): `is_absolute()` is false for it there, yet
                // `repo_dir.join(entry_path)` below still discards
                // `repo_dir` and keeps only its own root, exactly like an
                // absolute path would. `has_root()` catches that case on
                // every platform (on Unix it is equivalent to
                // `is_absolute()`).
                if entry_path.is_absolute() || entry_path.has_root() {
                    return fail(format!(
                        "configuration error: collection path must be relative \
                         (it is resolved inside `{}`), but `{}` is absolute",
                        repo_dir.display(),
                        entry.path
                    ));
                }
                if entry_path.components().any(|c| c == Component::ParentDir) {
                    return fail(format!(
                        "configuration error: collection path must not contain \
                         `..` components: `{}`",
                        entry.path
                    ));
                }
                let slug = entry.slug();
                Ok(ResolvedCollection {
                    name: entry.name.clone(),
                    coll_dir: repo_dir.join(&entry.path),
                    db_path: db_dir.join(format!("{slug}.db")),
                    slug,
                    owner: entry.owner.as_ref().map(|o| o.to_lowercase()),
                })
            })
            .collect::<Fallible<Vec<ResolvedCollection>>>()?;

        check_slug_collisions(&collections)?;

        let oidc = match config.oidc {
            None => None,
            Some(o) => {
                if !o.scopes.iter().any(|s| s == "openid") || !o.scopes.iter().any(|s| s == "email")
                {
                    return fail(
                        "configuration error: [oidc].scopes must include `openid` and `email` \
                         (the email claim is required to match collections to their owner)",
                    );
                }
                // `Key::derive_from` requires at least 32 bytes and panics
                // below that, so a short secret has to be rejected here with
                // a message the operator can act on.
                if o.session_secret.len() < MIN_SESSION_SECRET_BYTES {
                    return fail(format!(
                        "configuration error: [oidc].session_secret must be at least {} bytes \
                         long (it is {}); generate one with `openssl rand -hex 32`",
                        MIN_SESSION_SECRET_BYTES,
                        o.session_secret.len()
                    ));
                }
                Some(ResolvedOidc {
                    issuer_url: o.issuer_url,
                    client_id: o.client_id,
                    client_secret: o.client_secret,
                    external_url: o.external_url,
                    session_secret: o.session_secret,
                    scopes: o.scopes,
                })
            }
        };

        // `[[hedgedoc]]` owners are matched against the lowercased email claim
        // just like collection owners, so they are normalized the same way.
        let hedgedoc_entries: Vec<HedgedocEntry> = config
            .hedgedoc
            .iter()
            .map(|h| HedgedocEntry {
                url: h.url.clone(),
                owner: h.owner.as_ref().map(|o| o.to_lowercase()),
            })
            .collect();

        let custom_decks: Vec<CustomDeckEntry> = config
            .decks
            .iter()
            .map(|d| CustomDeckEntry {
                name: d.name.clone(),
                members: d.members.clone(),
                owner: d.owner.as_ref().map(|o| o.to_lowercase()),
            })
            .collect();
        for deck in &custom_decks {
            if deck.name.trim().is_empty() {
                return fail("configuration error: a [[deck]] entry has an empty `name`");
            }
            for raw in &deck.members {
                if DeckMember::parse(raw).is_none() {
                    return fail(format!(
                        "configuration error: [[deck]] '{}' has the member `{raw}`, which is not \
                         in `collection-slug/deck-name` form",
                        deck.name
                    ));
                }
            }
        }

        if oidc.is_some() {
            for c in &collections {
                if c.owner.is_none() {
                    return fail(format!(
                        "configuration error: [oidc] is enabled, so every collection must \
                         declare an `owner`, but collection '{}' has none",
                        c.name
                    ));
                }
            }
            for h in &hedgedoc_entries {
                if h.owner.is_none() {
                    return fail(format!(
                        "configuration error: [oidc] is enabled, so every [[hedgedoc]] entry \
                         must declare an `owner`, but the entry for '{}' has none",
                        h.url
                    ));
                }
            }
            for d in &custom_decks {
                if d.owner.is_none() {
                    return fail(format!(
                        "configuration error: [oidc] is enabled, so every [[deck]] entry must \
                         declare an `owner`, but the deck '{}' has none",
                        d.name
                    ));
                }
            }
        } else {
            // Without `[oidc]` every request is unauthenticated, so `owner`
            // can never match and the entry would simply be unreachable.
            // Silently ignoring the field would hide that; refuse instead.
            if let Some(c) = collections.iter().find(|c| c.owner.is_some()) {
                return fail(format!(
                    "configuration error: collection '{}' declares an `owner`, but [oidc] is \
                     not configured, so nobody is ever logged in and the collection would be \
                     unreachable. Add an [oidc] section or remove the `owner`",
                    c.name
                ));
            }
            if let Some(d) = custom_decks.iter().find(|d| d.owner.is_some()) {
                return fail(format!(
                    "configuration error: the [[deck]] entry '{}' declares an `owner`, but \
                     [oidc] is not configured, so nobody is ever logged in and the deck would \
                     be unreachable. Add an [oidc] section or remove the `owner`",
                    d.name
                ));
            }
            if let Some(h) = hedgedoc_entries.iter().find(|h| h.owner.is_some()) {
                return fail(format!(
                    "configuration error: the [[hedgedoc]] entry for '{}' declares an `owner`, \
                     but [oidc] is not configured, so nobody is ever logged in and the note \
                     would be unreachable. Add an [oidc] section or remove the `owner`",
                    h.url
                ));
            }
        }

        let git = match config.git {
            None => None,
            Some(g) => match g.repo_url {
                Some(repo_url) => Some(ResolvedGit {
                    repo_url,
                    branch: g.branch,
                    poll_interval_minutes: g.poll_interval_minutes,
                    commit_author_name: g.commit_author_name,
                    commit_author_email: g.commit_author_email,
                    repo_dir: repo_dir.clone(),
                    db_dir: db_dir.clone(),
                }),
                None => {
                    return fail(
                        "configuration error: [git] section is present but `repo_url` is missing",
                    );
                }
            },
        };

        Ok(Self {
            host: config.server.host,
            port: config.server.port,
            git,
            defaults: config.defaults,
            collections,
            data_dir: Some(data_dir),
            config_path: None,
            hedgedoc_entries,
            custom_decks,
            session_timeout_minutes: config.server.session_timeout_minutes,
            oidc,
        })
    }

    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    /// Build a config that serves `directories` directly, with no git
    /// remote, no HedgeDoc sources and no owners.
    ///
    /// Test-only: the server requires a config file, so there is no
    /// production path that reaches this. It survives because most serve
    /// tests want a state holding one throwaway collection.
    #[cfg(test)]
    pub fn from_directories(directories: Vec<String>, host: String, port: u16) -> Fallible<Self> {
        let base = current_dir()?;
        let mut collections = Vec::new();

        for dir_str in &directories {
            let dir = base.join(dir_str);
            if !dir.exists() {
                return fail(format!("directory does not exist: {dir_str}"));
            }
            let dir = dir.canonicalize()?;

            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir_str.clone());

            let slug = slugify(&name);
            let db_path = dir.join("hashcards.db");

            collections.push(ResolvedCollection {
                name,
                slug,
                coll_dir: dir,
                db_path,
                owner: None,
            });
        }

        check_slug_collisions(&collections)?;

        Ok(Self {
            host,
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections,
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: default_session_timeout_minutes(),
            oidc: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorReport;
    use crate::error::Fallible;

    /// Regression test for BUG-47: with no `host` key in the config, the
    /// server must bind to localhost, not to all interfaces.
    #[test]
    fn test_default_host_is_localhost() -> Fallible<()> {
        let toml = "[server]\ndata_dir = \"/var/lib/hashcards\"\n";
        let config: ServeConfig = toml::from_str(toml)?;
        assert_eq!(config.server.host, "127.0.0.1");
        Ok(())
    }

    /// Regression test for BUG-48: an absolute collection path (e.g.
    /// `path = "/etc"`) must be rejected at config load time.
    #[test]
    fn test_absolute_collection_path_rejected() -> Fallible<()> {
        let toml = "[server]\ndata_dir = \"/var/lib/hashcards\"\n\n\
                    [[collection]]\nname = \"Evil\"\npath = \"/etc\"\n";
        let config: ServeConfig = toml::from_str(toml)?;
        let error = match ResolvedServeConfig::from_toml(config) {
            Ok(_) => panic!("expected an error for an absolute collection path"),
            Err(e) => e,
        };
        assert!(error.to_string().contains("must be relative"));
        Ok(())
    }

    /// Regression test for BUG-48: a collection path with `..` components
    /// must be rejected at config load time.
    #[test]
    fn test_parent_collection_path_rejected() -> Fallible<()> {
        let toml = "[server]\ndata_dir = \"/var/lib/hashcards\"\n\n\
                    [[collection]]\nname = \"Evil\"\npath = \"../../etc\"\n";
        let config: ServeConfig = toml::from_str(toml)?;
        let error = match ResolvedServeConfig::from_toml(config) {
            Ok(_) => panic!("expected an error for a `..` collection path"),
            Err(e) => e,
        };
        assert!(error.to_string().contains(".."));
        Ok(())
    }

    /// A well-formed relative collection path still resolves under
    /// {data_dir}/repo.
    #[test]
    fn test_relative_collection_path_accepted() -> Fallible<()> {
        // `/var/lib/hashcards` is not a platform-absolute path on Windows
        // (no drive prefix), so a literal expected string would not match
        // there; build both the fixture and the expectation from the same
        // guaranteed-absolute path instead.
        let data_dir = current_dir()?.join("var-lib-hashcards");
        let toml = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [[collection]]\nname = \"Japanese\"\npath = \"japanese\"\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&toml)?;
        let resolved = ResolvedServeConfig::from_toml(config)?;
        assert_eq!(resolved.collections.len(), 1);
        assert_eq!(
            resolved.collections[0].coll_dir,
            data_dir.join("repo").join("japanese")
        );
        Ok(())
    }

    /// BUG-43 regression: `a/b` and `a-b` slugify identically and must be
    /// rejected at config load with a message naming both collections.
    #[test]
    fn test_slug_collision_in_config_is_rejected() -> Fallible<()> {
        let toml_str = r#"
[server]
data_dir = "/tmp/hc-test-data"

[[collection]]
name = "Alpha Slash"
path = "a/b"

[[collection]]
name = "Alpha Dash"
path = "a-b"
"#;
        let config: ServeConfig =
            toml::from_str(toml_str).map_err(|e| ErrorReport::new(e.to_string()))?;
        let err = match ResolvedServeConfig::from_toml(config) {
            Ok(_) => return fail("expected a slug collision error"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Alpha Slash"),
            "must name the first collection: {msg}"
        );
        assert!(
            msg.contains("Alpha Dash"),
            "must name the second collection: {msg}"
        );
        assert!(msg.contains("a-b"), "must name the colliding slug: {msg}");
        Ok(())
    }

    /// Distinct slugs still load fine.
    #[test]
    fn test_distinct_slugs_are_accepted() -> Fallible<()> {
        let toml_str = r#"
[server]
data_dir = "/tmp/hc-test-data"

[[collection]]
name = "Alpha"
path = "alpha"

[[collection]]
name = "Beta"
path = "beta"
"#;
        let config: ServeConfig =
            toml::from_str(toml_str).map_err(|e| ErrorReport::new(e.to_string()))?;
        let resolved = ResolvedServeConfig::from_toml(config)?;
        assert_eq!(resolved.collections.len(), 2);
        Ok(())
    }

    /// When `[oidc]` is present, every collection must declare an `owner`.
    #[test]
    fn test_oidc_requires_owner_on_every_collection() -> Fallible<()> {
        let data_dir = current_dir()?.join("var-lib-hashcards-oidc-owner-test");
        let toml_str = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [oidc]\n\
             issuer_url = \"https://idp.example.com\"\n\
             client_id = \"abc\"\n\
             client_secret = \"secret\"\n\
             external_url = \"https://hashcards.example.com\"\n\
             session_secret = \"a-very-long-random-session-secret-value\"\n\n\
             [[collection]]\n\
             name = \"Japanese\"\n\
             path = \"japanese\"\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&toml_str)?;
        match ResolvedServeConfig::from_toml(config) {
            Ok(_) => panic!("expected an error for a collection with no owner while [oidc] is set"),
            Err(e) => assert!(
                e.to_string().contains("Japanese") && e.to_string().contains("owner"),
                "error should name the offending collection and mention `owner`: {e}"
            ),
        }
        Ok(())
    }

    /// When `[oidc]` is present and every collection has an `owner`, config
    /// loads cleanly and the owner is lowercased for case-insensitive
    /// matching later.
    #[test]
    fn test_oidc_with_owner_on_every_collection_loads() -> Fallible<()> {
        let data_dir = current_dir()?.join("var-lib-hashcards-oidc-owner-ok-test");
        let toml_str = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [oidc]\n\
             issuer_url = \"https://idp.example.com\"\n\
             client_id = \"abc\"\n\
             client_secret = \"secret\"\n\
             external_url = \"https://hashcards.example.com\"\n\
             session_secret = \"a-very-long-random-session-secret-value\"\n\n\
             [[collection]]\n\
             name = \"Japanese\"\n\
             path = \"japanese\"\n\
             owner = \"Me@Example.com\"\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&toml_str)?;
        let resolved = ResolvedServeConfig::from_toml(config)?;
        assert_eq!(
            resolved.collections[0].owner.as_deref(),
            Some("me@example.com")
        );
        assert!(resolved.oidc.is_some());
        Ok(())
    }

    /// Without `[oidc]`, an `owner`-less collection loads exactly as before
    /// — the new field is inert.
    #[test]
    fn test_no_oidc_owner_field_is_inert() -> Fallible<()> {
        let data_dir = current_dir()?.join("var-lib-hashcards-no-oidc-test");
        let toml_str = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [[collection]]\n\
             name = \"Japanese\"\n\
             path = \"japanese\"\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&toml_str)?;
        let resolved = ResolvedServeConfig::from_toml(config)?;
        assert!(resolved.oidc.is_none());
        assert_eq!(resolved.collections[0].owner, None);
        Ok(())
    }

    /// `Key::derive_from` panics below 32 bytes, so a short `session_secret`
    /// must be rejected at config load with a message the operator can act
    /// on rather than crashing the server at startup.
    #[test]
    fn test_short_session_secret_is_rejected() -> Fallible<()> {
        let data_dir = current_dir()?.join("var-lib-hashcards-short-secret-test");
        let toml_str = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [oidc]\n\
             issuer_url = \"https://idp.example.com\"\n\
             client_id = \"abc\"\n\
             client_secret = \"secret\"\n\
             external_url = \"https://hashcards.example.com\"\n\
             session_secret = \"hunter2\"\n\n\
             [[collection]]\n\
             name = \"Japanese\"\n\
             path = \"japanese\"\n\
             owner = \"me@example.com\"\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&toml_str)?;
        match ResolvedServeConfig::from_toml(config) {
            Ok(_) => panic!("expected an error for a session_secret shorter than 32 bytes"),
            Err(e) => assert!(
                e.to_string().contains("session_secret"),
                "error should name the offending setting: {e}"
            ),
        }
        Ok(())
    }

    /// An `owner` with no `[oidc]` section means nobody is ever logged in, so
    /// the collection would silently 404 for everyone. Refuse to start.
    #[test]
    fn test_owner_without_oidc_is_rejected() -> Fallible<()> {
        let data_dir = current_dir()?.join("var-lib-hashcards-owner-no-oidc-test");
        let toml_str = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [[collection]]\n\
             name = \"Japanese\"\n\
             path = \"japanese\"\n\
             owner = \"me@example.com\"\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&toml_str)?;
        match ResolvedServeConfig::from_toml(config) {
            Ok(_) => panic!("expected an error for an `owner` without [oidc]"),
            Err(e) => assert!(
                e.to_string().contains("Japanese") && e.to_string().contains("oidc"),
                "error should name the collection and mention [oidc]: {e}"
            ),
        }
        Ok(())
    }

    /// `[[hedgedoc]]` owners are lowercased just like collection owners, so a
    /// config written with mixed case still matches the OIDC email claim.
    #[test]
    fn test_hedgedoc_owner_is_lowercased() -> Fallible<()> {
        let data_dir = current_dir()?.join("var-lib-hashcards-hedgedoc-owner-case-test");
        let toml_str = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [oidc]\n\
             issuer_url = \"https://idp.example.com\"\n\
             client_id = \"abc\"\n\
             client_secret = \"secret\"\n\
             external_url = \"https://hashcards.example.com\"\n\
             session_secret = \"a-very-long-random-session-secret-value\"\n\n\
             [[hedgedoc]]\n\
             url = \"https://pad.example.com/abc\"\n\
             owner = \"Me@Example.com\"\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&toml_str)?;
        let resolved = ResolvedServeConfig::from_toml(config)?;
        assert_eq!(
            resolved.hedgedoc_entries[0].owner.as_deref(),
            Some("me@example.com")
        );
        Ok(())
    }

    /// Windows CI regression: `data_dir` is interpolated into TOML with
    /// `{:?}`, not `{}`, so a path containing backslashes stays a valid TOML
    /// basic string. Formatting it raw produced `data_dir = "D:\a\..."`,
    /// which the parser rejects as a bad escape. This test runs everywhere so
    /// the fix cannot regress on a Linux-only run.
    #[test]
    fn test_windows_style_data_dir_survives_toml_formatting() -> Fallible<()> {
        let data_dir = PathBuf::from(r"D:\a\hashcards-web\var-lib-hashcards");
        let toml_str = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [[collection]]\n\
             name = \"Japanese\"\n\
             path = \"japanese\"\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&toml_str)?;
        assert_eq!(PathBuf::from(&config.server.data_dir), data_dir);
        Ok(())
    }

    /// A `[[deck]]` member must be `collection-slug/deck-name`.
    #[test]
    fn test_malformed_deck_member_is_rejected() -> Fallible<()> {
        let data_dir = current_dir()?.join("var-lib-hashcards-deck-member-test");
        let toml_str = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [[deck]]\n\
             name = \"Mixed\"\n\
             members = [\"no-separator\"]\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&toml_str)?;
        match ResolvedServeConfig::from_toml(config) {
            Ok(_) => panic!("expected an error for a malformed [[deck]] member"),
            Err(e) => assert!(
                e.to_string().contains("Mixed") && e.to_string().contains("no-separator"),
                "error should name the deck and the bad member: {e}"
            ),
        }
        Ok(())
    }

    /// With `[oidc]` on, every deck needs an owner, exactly like collections
    /// and HedgeDoc notes.
    #[test]
    fn test_oidc_requires_owner_on_every_deck() -> Fallible<()> {
        let data_dir = current_dir()?.join("var-lib-hashcards-deck-owner-test");
        let toml_str = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [oidc]\n\
             issuer_url = \"https://idp.example.com\"\n\
             client_id = \"abc\"\n\
             client_secret = \"secret\"\n\
             external_url = \"https://hashcards.example.com\"\n\
             session_secret = \"a-very-long-random-session-secret-value\"\n\n\
             [[deck]]\n\
             name = \"Mixed\"\n\
             members = [\"japanese/Verbs\"]\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&toml_str)?;
        match ResolvedServeConfig::from_toml(config) {
            Ok(_) => panic!("expected an error for a deck with no owner while [oidc] is set"),
            Err(e) => assert!(
                e.to_string().contains("Mixed") && e.to_string().contains("owner"),
                "error should name the deck and mention `owner`: {e}"
            ),
        }
        Ok(())
    }

    /// A deck owner is lowercased for case-insensitive matching, and a deck
    /// with no `[oidc]` may not declare one at all.
    #[test]
    fn test_deck_owner_is_lowercased_and_rejected_without_oidc() -> Fallible<()> {
        let data_dir = current_dir()?.join("var-lib-hashcards-deck-owner-case-test");
        let with_oidc = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [oidc]\n\
             issuer_url = \"https://idp.example.com\"\n\
             client_id = \"abc\"\n\
             client_secret = \"secret\"\n\
             external_url = \"https://hashcards.example.com\"\n\
             session_secret = \"a-very-long-random-session-secret-value\"\n\n\
             [[deck]]\n\
             name = \"Mixed\"\n\
             members = [\"japanese/Verbs\"]\n\
             owner = \"Me@Example.com\"\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&with_oidc)?;
        let resolved = ResolvedServeConfig::from_toml(config)?;
        assert_eq!(
            resolved.custom_decks[0].owner.as_deref(),
            Some("me@example.com")
        );

        let without_oidc = format!(
            "[server]\ndata_dir = {:?}\n\n\
             [[deck]]\n\
             name = \"Mixed\"\n\
             members = [\"japanese/Verbs\"]\n\
             owner = \"me@example.com\"\n",
            data_dir
        );
        let config: ServeConfig = toml::from_str(&without_oidc)?;
        assert!(
            ResolvedServeConfig::from_toml(config).is_err(),
            "an `owner` without [oidc] must be rejected for decks too"
        );
        Ok(())
    }
}
