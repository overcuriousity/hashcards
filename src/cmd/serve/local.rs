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
            return fail(format!(
                "Path must not contain `..` components: `{trimmed}`"
            ));
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
/// folder path. Not a hash of the name alone: renaming must not change it.
fn fresh_id(folder: &Path) -> Fallible<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| ErrorReport::new(format!("System clock is before 1970: {e}")))?
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
}
