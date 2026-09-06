use std::fs::read_dir;
use std::fs::read_to_string;
use std::fs::write;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde::Serialize;

use crate::cmd::serve::config::ResolvedCollection;
use crate::cmd::serve::config::slugify;
use crate::error::ErrorReport;
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
        let root = root_path(data_dir, owner)?;
        ensure_dir(&root, "local card directory")?;
        Ok(Self { root })
    }

    /// The tree belonging to `owner`, without creating it. Read paths use
    /// this: serving a page must not materialize directories.
    pub fn open(data_dir: &Path, owner: Option<&str>) -> Fallible<Self> {
        Ok(Self {
            root: root_path(data_dir, owner)?,
        })
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
        Ok(self.resolve_entry(rel)?.path)
    }

    /// The same, keeping the normalized relative path alongside it.
    ///
    /// Callers that both touch the file and reason about *where* it sits —
    /// which collection owns it, whether it is a collection root — must ask
    /// both questions of the same string. Splitting the raw request path
    /// instead answers them differently: `./Spanish` resolves to the
    /// collection folder while a raw split calls it nested, so the folder
    /// would be deleted without its review database going with it.
    pub fn resolve_entry(&self, rel: &str) -> Fallible<ResolvedEntry> {
        let trimmed = rel.trim();
        if trimmed.is_empty() {
            return fail("No file path was given.");
        }
        let rel_path = PathBuf::from(trimmed);
        if rel_path.is_absolute() || rel_path.has_root() {
            return fail(format!(
                "Path must be relative to your card folder: `{trimmed}`"
            ));
        }
        // `.` components are dropped rather than refused: they name the same
        // place, and normalizing here is what keeps every later answer about
        // this path consistent.
        let mut parts: Vec<String> = Vec::new();
        for component in rel_path.components() {
            match component {
                Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
                Component::CurDir => continue,
                Component::ParentDir => {
                    return fail(format!(
                        "Path must not contain `..` components: `{trimmed}`"
                    ));
                }
                _ => {
                    return fail(format!(
                        "Path must be relative to your card folder: `{trimmed}`"
                    ));
                }
            }
        }
        // Nothing addresses the root by path — every caller wants a file or
        // folder inside it — and one that did would let the file manager
        // delete the whole tree.
        if parts.is_empty() {
            return fail(format!(
                "Path must name something inside your card folder: `{trimmed}`"
            ));
        }
        let normalized = parts.join("/");
        let joined = self.root.join(&normalized);

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
        Ok(ResolvedEntry {
            rel: normalized,
            path: joined,
        })
    }
}

/// A checked client-supplied path: where it is on disk, and its relative
/// path reduced to plain `/`-separated components.
pub struct ResolvedEntry {
    /// Normalized, so `./Spanish/verbs.md` and `Spanish/verbs.md` are the
    /// same string. Never empty, and never starts or ends with `/`.
    pub rel: String,
    pub path: PathBuf,
}

/// Where `owner`'s tree lives, whether or not it exists yet.
fn root_path(data_dir: &Path, owner: Option<&str>) -> Fallible<PathBuf> {
    let who = match owner {
        Some(email) => slugify(&email.to_lowercase()),
        None => "default".to_string(),
    };
    if who.is_empty() {
        return fail("Cannot open a local card folder: the owner name is empty.");
    }
    Ok(data_dir.join("local").join(who))
}

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
    if let Some(id) = existing_collection_id(folder)? {
        return Ok(id);
    }
    let meta_path = folder.join(LOCAL_META_FILE);
    let id = fresh_id(folder)?;
    let meta = LocalMeta { id: id.clone() };
    write(&meta_path, toml::to_string(&meta)?)?;
    Ok(id)
}

/// Eight hex characters derived from the clock, the process id and the
/// folder path. Not a hash of the name alone: renaming must not change it.
fn fresh_id(folder: &Path) -> Fallible<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| ErrorReport::new(format!("System clock is before 1970: {e}")))?
        .as_nanos();
    let seed = format!("{nanos}:{}:{}", std::process::id(), folder.display());
    Ok(blake3::hash(seed.as_bytes()).to_hex()[..8].to_string())
}

/// Whether discovery may give a folder that has none a stable id.
///
/// Read paths pass `ExistingOnly`: serving a page must not write into the
/// user's tree. Pages that exist to show the tree (`/files`, the landing
/// page) pass `CreateMissing`, off the async executor.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IdPolicy {
    CreateMissing,
    ExistingOnly,
}

/// The id already recorded for `folder`, if any. Never writes.
pub fn existing_collection_id(folder: &Path) -> Fallible<Option<String>> {
    let meta_path = folder.join(LOCAL_META_FILE);
    if !meta_path.exists() {
        return Ok(None);
    }
    let text = read_to_string(&meta_path)?;
    let meta: LocalMeta = toml::from_str(&text)?;
    if meta.id.is_empty() {
        return Ok(None);
    }
    Ok(Some(meta.id))
}

/// Every top-level folder in `root`, as a collection.
///
/// Loose files directly under the root are ignored: a collection is a
/// folder, so that a file always belongs to exactly one.
///
/// A folder hashcards cannot make sense of is skipped with a warning rather
/// than failing the whole listing: one malformed `.hashcards.toml` must not
/// make every other collection disappear from the landing page. Folders are
/// visited in name order, so which of two names that slugify alike wins does
/// not depend on the order the filesystem happened to list them in.
pub fn discover_local_collections(
    root: &LocalRoot,
    db_dir: &Path,
    owner: Option<&str>,
    policy: IdPolicy,
) -> Fallible<Vec<ResolvedCollection>> {
    let mut collections: Vec<ResolvedCollection> = Vec::new();
    if !root.path().exists() {
        return Ok(collections);
    }
    let mut names = Vec::new();
    for entry in read_dir(root.path())? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || path.is_symlink() {
            continue;
        }
        match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if !n.starts_with('.') => names.push(n.to_string()),
            _ => continue,
        }
    }
    names.sort();

    for name in names {
        let path = root.path().join(&name);
        let id = match folder_id(&path, policy) {
            Ok(Some(id)) => id,
            Ok(None) => continue,
            Err(e) => {
                log::warn!("Skipping local collection `{name}`: {e}");
                continue;
            }
        };
        let slug = slugify(&name);
        if let Some(first) = collections.iter().find(|c| c.slug == slug) {
            log::warn!(
                "Skipping local collection `{name}`: it maps to the same URL slug `{slug}` as `{}`.",
                first.name
            );
            continue;
        }
        collections.push(ResolvedCollection {
            slug,
            name,
            coll_dir: path,
            db_path: db_dir.join(format!("{id}.db")),
            owner: owner.map(|o| o.to_lowercase()),
        });
    }
    Ok(collections)
}

/// Every collection in every user's tree under `{data_dir}/local`.
///
/// For startup-time work that must touch each review database once — the
/// dangling-session sweep — and nothing else. `owner` is always `None`: a
/// tree's directory name is a *slug* of an email, which no request can be
/// matched against, so a collection from here must never be routed to.
/// Every read path goes through `find_collection` instead.
///
/// `ExistingOnly`, so startup never writes an id into a user's tree: a
/// folder with no id has no database either, and nothing to sweep.
pub fn discover_all_collections(data_dir: &Path) -> Vec<ResolvedCollection> {
    let trees_dir = data_dir.join("local");
    let db_dir = data_dir.join("db");
    let entries = match read_dir(&trees_dir) {
        Ok(entries) => entries,
        // No tree yet is the ordinary state of a fresh install.
        Err(_) => return Vec::new(),
    };
    let mut all = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || path.is_symlink() {
            continue;
        }
        let root = LocalRoot { root: path };
        match discover_local_collections(&root, &db_dir, None, IdPolicy::ExistingOnly) {
            Ok(found) => all.extend(found),
            Err(e) => log::warn!("Skipping the card tree at {}: {e}", root.path().display()),
        }
    }
    all
}

/// The id of one folder under `policy`. `None` means "no id yet, and this
/// caller may not create one".
fn folder_id(path: &Path, policy: IdPolicy) -> Fallible<Option<String>> {
    match policy {
        IdPolicy::CreateMissing => Ok(Some(collection_id(path)?)),
        IdPolicy::ExistingOnly => existing_collection_id(path),
    }
}

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

    /// The normalized path is what every later question about the file is
    /// asked of, so it has to be free of the `.` components a client can
    /// put in: `./Spanish` and `Spanish` name one collection, and answers
    /// derived from splitting the raw string disagreed about that.
    #[test]
    fn resolving_normalizes_away_dot_components() -> Fallible<()> {
        let (_dir, root) = fixture()?;
        let plain = root.resolve_entry("Spanish/verbs.md")?;
        for path in [
            "./Spanish/verbs.md",
            "Spanish/./verbs.md",
            "Spanish//verbs.md",
        ] {
            let entry = root.resolve_entry(path)?;
            assert_eq!(entry.rel, plain.rel, "for `{path}`");
            assert_eq!(entry.path, plain.path, "for `{path}`");
        }
        // The root itself still names nothing inside the tree.
        assert!(root.resolve_entry(".").is_err());
        assert!(root.resolve_entry("./").is_err());
        assert!(root.resolve_entry("./..").is_err());
        Ok(())
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

    /// The root is not addressable. `delete_entry` resolves the path it is
    /// given and removes it: `.` would have named the whole card folder,
    /// and a `media` collection at the top level does not even count
    /// towards "is it empty", so the tree would be judged empty and wiped.
    #[test]
    fn rejects_the_root_itself() -> Fallible<()> {
        let (_dir, root) = fixture()?;
        assert!(root.resolve(".").is_err());
        assert!(root.resolve("./").is_err());
        assert!(root.resolve("/").is_err());
        assert!(root.resolve("  ").is_err());
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
        let mut found = discover_local_collections(
            &root,
            &db_dir,
            Some("me@example.com"),
            IdPolicy::CreateMissing,
        )?;
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
        let found = discover_local_collections(&root, &db_dir, None, IdPolicy::CreateMissing)?;
        assert_eq!(found[0].db_path, db_dir.join(format!("{id}.db")));
        Ok(())
    }

    #[test]
    fn discovery_skips_a_folder_whose_metadata_is_broken() -> Fallible<()> {
        // One unreadable `.hashcards.toml` must not take every other local
        // collection down with it.
        let (dir, root) = fixture()?;
        let broken = root.path().join("Spanish");
        std::fs::create_dir_all(&broken)?;
        std::fs::write(broken.join(LOCAL_META_FILE), "this is not toml {{{")?;
        std::fs::create_dir_all(root.path().join("Medicine"))?;

        let found =
            discover_local_collections(&root, &dir.join("db"), None, IdPolicy::CreateMissing)?;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Medicine");
        Ok(())
    }

    #[test]
    fn read_only_discovery_writes_no_metadata_file() -> Fallible<()> {
        let (dir, root) = fixture()?;
        let folder = root.path().join("Spanish");
        std::fs::create_dir_all(&folder)?;

        let found =
            discover_local_collections(&root, &dir.join("db"), None, IdPolicy::ExistingOnly)?;
        assert!(found.is_empty(), "a folder with no id yet must be skipped");
        assert!(!folder.join(LOCAL_META_FILE).exists(), "read path wrote");
        Ok(())
    }

    #[test]
    fn discovery_keeps_one_of_two_folders_that_slugify_alike() -> Fallible<()> {
        // `slugify` maps both names to `Verbs-1`; serving both would make
        // `/collection/Verbs-1` mean whichever `read_dir` yielded first.
        let (dir, root) = fixture()?;
        std::fs::create_dir_all(root.path().join("Verbs 1"))?;
        std::fs::create_dir_all(root.path().join("Verbs-1"))?;

        let found =
            discover_local_collections(&root, &dir.join("db"), None, IdPolicy::CreateMissing)?;
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].name, "Verbs 1",
            "the first name by sort order wins"
        );
        Ok(())
    }

    #[test]
    fn opening_a_tree_does_not_create_it() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        let root = LocalRoot::open(&dir, Some("me@example.com"))?;
        assert!(!root.path().exists());
        Ok(())
    }
}
