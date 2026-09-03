use std::fs::read_dir;
use std::path::Path;
use std::path::PathBuf;

use std::collections::HashMap;

use axum::Form;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use axum::response::Redirect;
use serde::Deserialize;

use crate::cmd::serve::auth::CurrentUser;
use crate::cmd::serve::files_ui::render_tree_page;
use crate::cmd::serve::local::LOCAL_META_FILE;
use crate::cmd::serve::local::LocalRoot;
use crate::cmd::serve::local::collection_id;
use crate::cmd::serve::state::AppState;
use crate::error::Fallible;
use crate::error::fail;
use crate::flash::Flash;

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

fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<TreeEntry>) -> Fallible<()> {
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

/// Names in `dir` that belong to the user, ignoring hashcards' own
/// bookkeeping. A folder holding only bookkeeping counts as empty.
fn non_empty_children(dir: &Path) -> Fallible<Vec<String>> {
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
    Query(query): Query<HashMap<String, String>>,
    current_user: Option<CurrentUser>,
) -> (StatusCode, Html<String>) {
    let flash = Flash::from_query(&query);
    let markup = match user_root(&state, current_user.as_ref()).and_then(|r| read_tree(&r)) {
        Ok(tree) => render_tree_page(&tree, flash),
        Err(e) => render_tree_page(&[], Some(Flash::error(e.to_string()))),
    };
    (StatusCode::OK, Html(markup.into_string()))
}

pub async fn files_folder_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
    Form(form): Form<NewEntryForm>,
) -> Redirect {
    flash_for(create_entry(&state, current_user.as_ref(), &form, true))
}

pub async fn files_file_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
    Form(form): Form<NewEntryForm>,
) -> Redirect {
    flash_for(create_entry(&state, current_user.as_ref(), &form, false))
}

pub async fn files_rename_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
    Form(form): Form<RenameForm>,
) -> Redirect {
    flash_for(rename_entry(&state, current_user.as_ref(), &form))
}

pub async fn files_delete_handler(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
    Form(form): Form<DeleteForm>,
) -> Redirect {
    flash_for(delete_entry(&state, current_user.as_ref(), &form))
}

/// Every file-manager mutation reports back on `/files` the same way.
fn flash_for(outcome: Fallible<String>) -> Redirect {
    match outcome {
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
            collection_id(&target)?;
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

fn rename_entry(
    state: &AppState,
    user: Option<&CurrentUser>,
    form: &RenameForm,
) -> Fallible<String> {
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

fn delete_entry(
    state: &AppState,
    user: Option<&CurrentUser>,
    form: &DeleteForm,
) -> Fallible<String> {
    let root = user_root(state, user)?;
    let target = root.resolve(&form.path)?;
    if !target.exists() {
        return fail(format!("`{}` does not exist.", form.path));
    }
    if target.is_dir() {
        // Refuse a non-empty folder: deleting a whole collection on a
        // misclick would take its review history with it.
        let kept = non_empty_children(&target)?;
        if !kept.is_empty() {
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
        std::fs::write(
            root.path().join("Spanish").join(LOCAL_META_FILE),
            "id = \"x\"\n",
        )?;

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

    #[test]
    fn names_must_be_single_components() {
        assert!(validate_name("verbs.md").is_ok());
        assert!(validate_name("Spanish").is_ok());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name(LOCAL_META_FILE).is_err());
    }

    #[test]
    fn a_non_empty_folder_is_not_deleted() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        let root = LocalRoot::for_user(&dir, None)?;
        let folder = root.path().join("Spanish");
        std::fs::create_dir_all(&folder)?;
        std::fs::write(folder.join("verbs.md"), "Q: a\nA: b\n")?;
        // The metadata file alone must not count as "non-empty".
        std::fs::write(folder.join(LOCAL_META_FILE), "id = \"x\"\n")?;

        assert_eq!(non_empty_children(&folder)?, vec!["verbs.md".to_string()]);

        std::fs::remove_file(folder.join("verbs.md"))?;
        assert!(non_empty_children(&folder)?.is_empty());
        Ok(())
    }

    #[test]
    fn sync_never_writes_into_the_local_root() -> Fallible<()> {
        // The local root must not sit under the directory that git and
        // source sync own, or a pull could overwrite user writing.
        let dir = create_tmp_directory()?;
        let root = LocalRoot::for_user(&dir, None)?;
        assert!(!root.path().starts_with(dir.join("repo")));
        Ok(())
    }
}
