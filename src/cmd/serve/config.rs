use std::env::current_dir;
use std::fs::read_to_string;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use crate::cmd::drill::server::AnswerControls;
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
    #[serde(rename = "collection", default)]
    pub collections: Vec<CollectionEntry>,
    #[serde(rename = "hedgedoc", default)]
    pub hedgedoc: Vec<HedgedocEntry>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct HedgedocEntry {
    pub url: String,
}

#[derive(Deserialize)]
pub struct ServerSection {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub data_dir: String,
}

/// Default bind address. Deliberately localhost-only: hashcards has no
/// authentication, so binding to all interfaces must be an explicit opt-in
/// (`host = "0.0.0.0"` in the config file).
fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8000
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
}

pub struct TempDirTracker {
    path: PathBuf,
    dismissed: std::sync::atomic::AtomicBool,
}

impl TempDirTracker {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            dismissed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Stop the temp directory from being deleted on drop (e.g. once a config
    /// that references it has been persisted to disk).
    pub fn dismiss(&self) {
        self.dismissed
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Drop for TempDirTracker {
    fn drop(&mut self) {
        if !self.dismissed.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

pub struct ResolvedServeConfig {
    pub host: String,
    pub port: u16,
    pub git: Option<ResolvedGit>,
    pub defaults: DefaultsSection,
    pub collections: Vec<ResolvedCollection>,
    /// Set when loaded from a TOML file; None when using directory arguments.
    pub data_dir: Option<PathBuf>,
    /// Config file path; needed to persist UI changes back to disk.
    pub config_path: Option<PathBuf>,
    /// HedgeDoc source URLs loaded from the config file.
    pub hedgedoc_entries: Vec<HedgedocEntry>,
    /// Kept alive for the process lifetime so the OS temp directory is cleaned up.
    pub _temp_dir: Option<std::sync::Arc<TempDirTracker>>,
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
                if entry_path.is_absolute() {
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
                })
            })
            .collect::<Fallible<Vec<ResolvedCollection>>>()?;

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
            hedgedoc_entries: config.hedgedoc,
            _temp_dir: None,
        })
    }

    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

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
            });
        }

        Ok(Self {
            host,
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections,
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            _temp_dir: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let toml = "[server]\ndata_dir = \"/var/lib/hashcards\"\n\n\
                    [[collection]]\nname = \"Japanese\"\npath = \"japanese\"\n";
        let config: ServeConfig = toml::from_str(toml)?;
        let resolved = ResolvedServeConfig::from_toml(config)?;
        assert_eq!(resolved.collections.len(), 1);
        assert_eq!(
            resolved.collections[0].coll_dir,
            PathBuf::from("/var/lib/hashcards/repo/japanese")
        );
        Ok(())
    }
}
