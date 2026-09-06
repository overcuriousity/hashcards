use std::path::Path;
use std::process::Command as SyncCommand;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use tokio::process::Command;
use tokio::sync::RwLock;
use tokio::time::interval;

use crate::cmd::serve::config::ResolvedCollection;
use crate::cmd::serve::config::ResolvedGit;
use crate::cmd::serve::counts::compute_collection_counts;
use crate::cmd::serve::counts::refresh_collection_info;
use crate::cmd::serve::state::CollectionInfo;
use crate::cmd::serve::state::HedgedocSource;
use crate::error::Fallible;
use crate::error::fail;
use crate::types::timestamp::Timestamp;

pub async fn clone_or_pull(repo_url: &str, branch: &str, target_dir: &Path) -> Fallible<()> {
    if target_dir.join(".git").exists() {
        log::debug!("Checking out branch {} in {}", branch, target_dir.display());
        let checkout = Command::new("git")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["checkout", branch])
            .current_dir(target_dir)
            .output()
            .await?;
        if !checkout.status.success() {
            let stderr = String::from_utf8_lossy(&checkout.stderr);
            return fail(format!("git checkout {branch} failed: {stderr}"));
        }
        log::debug!("Pulling latest changes in {}", target_dir.display());
        let pull = Command::new("git")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["pull", "--ff-only", "origin", branch])
            .current_dir(target_dir)
            .output()
            .await?;
        if !pull.status.success() {
            let stderr = String::from_utf8_lossy(&pull.stderr);
            return fail(format!("git pull origin {branch} failed: {stderr}"));
        }
    } else {
        log::debug!("Cloning {} into {}", repo_url, target_dir.display());
        let output = Command::new("git")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["clone", "--branch", branch, "--single-branch", repo_url])
            .arg(target_dir)
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return fail(format!("git clone failed: {stderr}"));
        }
    }
    Ok(())
}

pub fn spawn_sync_task(
    git: ResolvedGit,
    collections: Vec<ResolvedCollection>,
    collection_infos: Arc<RwLock<Vec<CollectionInfo>>>,
    last_synced: Arc<Mutex<Option<Timestamp>>>,
    hedgedoc_sources: Arc<Mutex<Vec<HedgedocSource>>>,
) {
    if git.poll_interval_minutes == 0 {
        log::debug!("Periodic git sync disabled (poll_interval_minutes = 0)");
        return;
    }

    let poll_minutes = git.poll_interval_minutes;
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(poll_minutes * 60));
        // Skip the first immediate tick (we already synced on startup)
        ticker.tick().await;

        loop {
            ticker.tick().await;
            log::debug!("Periodic git sync triggered");
            if let Err(e) = clone_or_pull(&git.repo_url, &git.branch, &git.repo_dir).await {
                log::error!("Periodic git sync failed: {e}");
                continue;
            }
            let collections_for_counts = collections.clone();
            let static_infos = match tokio::task::spawn_blocking(move || {
                refresh_collection_info(&collections_for_counts)
            })
            .await
            {
                Ok(infos) => infos,
                Err(e) => {
                    log::error!("Failed to join collection counts task: {e}");
                    continue;
                }
            };
            // Snapshot only the paths needed, then release the lock before
            // doing filesystem/DB work to avoid blocking other handlers.
            let source_paths: Vec<(
                String,
                String,
                std::path::PathBuf,
                std::path::PathBuf,
                Option<String>,
            )> = {
                let sources = hedgedoc_sources.lock();
                sources
                    .iter()
                    .map(|s| {
                        (
                            s.collection.name.clone(),
                            s.collection.slug.clone(),
                            s.collection.coll_dir.clone(),
                            s.collection.db_path.clone(),
                            s.collection.owner.clone(),
                        )
                    })
                    .collect()
            };
            let hedgedoc_infos: Vec<CollectionInfo> = match tokio::task::spawn_blocking(move || {
                source_paths
                    .into_iter()
                    .map(|(name, slug, coll_dir, db_path, owner)| {
                        let (total_cards, due_today) = match compute_collection_counts(&coll_dir, &db_path) {
                            Ok(counts) => counts,
                            Err(e) => {
                                log::warn!(
                                    "Failed to compute HedgeDoc collection counts for '{}' (slug: '{}', dir: '{}', db: '{}'): {e}",
                                    name,
                                    slug,
                                    coll_dir.display(),
                                    db_path.display(),
                                );
                                (0, 0)
                            }
                        };
                        CollectionInfo { name, slug, total_cards, due_today, owner }
                    })
                    .collect::<Vec<CollectionInfo>>()
            })
            .await
            {
                Ok(infos) => infos,
                Err(e) => {
                    log::error!("Failed to join HedgeDoc collection counts task: {e}");
                    Vec::new()
                }
            };
            let mut combined = static_infos;
            combined.extend(hedgedoc_infos);
            *collection_infos.write().await = combined;
            *last_synced.lock() = Some(Timestamp::now());
            log::debug!("Periodic git sync complete");
        }
    });
}

/// Stage and commit a single edited file in its containing git repository.
///
/// Synchronous by design: the edit path (`edit_post_inner`) is synchronous.
/// Returns `Ok(false)` when the file is not inside a git work tree or the
/// edit produced no diff; `Ok(true)` when a commit was created.
pub fn commit_edit(file_path: &Path, author_name: &str, author_email: &str) -> Fallible<bool> {
    let dir = match file_path.parent() {
        Some(d) => d,
        None => {
            return fail(format!(
                "file has no parent directory: {}",
                file_path.display()
            ));
        }
    };

    let inside = SyncCommand::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(dir)
        .output()?;
    if !inside.status.success() {
        // Not a git-backed collection: nothing to do.
        return Ok(false);
    }

    let add = SyncCommand::new("git")
        .arg("add")
        .arg("--")
        .arg(file_path)
        .current_dir(dir)
        .output()?;
    if !add.status.success() {
        let stderr = String::from_utf8_lossy(&add.stderr);
        return fail(format!(
            "git add failed for {}: {stderr}",
            file_path.display()
        ));
    }

    let status = SyncCommand::new("git")
        .args(["status", "--porcelain"])
        .arg("--")
        .arg(file_path)
        .current_dir(dir)
        .output()?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return fail(format!(
            "git status failed for {}: {stderr}",
            file_path.display()
        ));
    }
    if status.stdout.is_empty() {
        // The edit produced no diff: nothing to commit.
        return Ok(false);
    }

    let file_name = file_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.display().to_string());
    let commit = SyncCommand::new("git")
        .arg("-c")
        .arg(format!("user.name={author_name}"))
        .arg("-c")
        .arg(format!("user.email={author_email}"))
        .arg("commit")
        .arg("-m")
        .arg(format!("hashcards: web edit of {file_name}"))
        .arg("--")
        .arg(file_path)
        .current_dir(dir)
        .output()?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        return fail(format!(
            "git commit failed for {}: {stderr}",
            file_path.display()
        ));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command as SyncCommand;

    use super::commit_edit;

    fn git(dir: &Path, args: &[&str]) {
        let out = SyncCommand::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn test_commit_edit_commits_and_leaves_clean_tree() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-b", "main"]);
        let file = repo.join("Deck.md");
        std::fs::write(&file, "Q: foo\nA: bar\n").unwrap();
        git(repo, &["add", "."]);
        git(
            repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                "init",
            ],
        );

        std::fs::write(&file, "Q: foo\nA: baz\n").unwrap();
        let committed = commit_edit(&file, "hashcards web edit", "hashcards@localhost").unwrap();
        assert!(committed);

        let status = SyncCommand::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            status.stdout.is_empty(),
            "working tree not clean: {}",
            String::from_utf8_lossy(&status.stdout)
        );

        let log = SyncCommand::new("git")
            .args(["log", "-1", "--pretty=%an %s"])
            .current_dir(repo)
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&log.stdout);
        assert!(log.contains("hashcards web edit"), "unexpected log: {log}");
        assert!(log.contains("web edit of Deck.md"), "unexpected log: {log}");
    }

    #[test]
    fn test_commit_edit_outside_git_repo_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Deck.md");
        std::fs::write(&file, "Q: foo\nA: bar\n").unwrap();
        let committed = commit_edit(&file, "n", "e@e").unwrap();
        assert!(!committed);
    }

    #[test]
    fn test_commit_edit_without_changes_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-b", "main"]);
        let file = repo.join("Deck.md");
        std::fs::write(&file, "Q: foo\nA: bar\n").unwrap();
        git(repo, &["add", "."]);
        git(
            repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                "init",
            ],
        );

        let committed = commit_edit(&file, "n", "e@e").unwrap();
        assert!(!committed);
    }
}
