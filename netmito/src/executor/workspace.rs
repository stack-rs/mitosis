//! Creates the working directories of the executor. A task can change to a different user while
//! it runs, thus the directories that the task writes to are open to all users.

#[cfg(unix)]
use std::fs::Permissions;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The mode of a directory that the task writes to: all users can read, write and enter it. The
/// sticky bit stops a user from removing the files of a different user.
///
/// The worker or the agent creates the directories, but the task can change to a different user
/// (`sudo`, a runtime that drops privileges, a nested container). With the usual umask, that user
/// cannot write its results. Thus the executor sets the mode and does not use the umask.
#[cfg(unix)]
const SHARED_DIR_MODE: u32 = 0o1777;

/// The mode of a directory above the working directory. Only the owner can write to it, but all
/// users can enter it. Thus a task that runs as a different user can get to the directories below.
#[cfg(unix)]
const TRAVERSABLE_DIR_MODE: u32 = 0o755;

/// The mode of a downloaded input. It is also executable, because an input is frequently the
/// program that the task runs. The sticky bit has no effect on a file, thus it is not set.
#[cfg(unix)]
const SHARED_FILE_MODE: u32 = 0o777;

/// Create `path` and its missing parents, then let all users read and write `path`.
///
/// The parents keep the umask. To open a full tree, call this function once for each level, from
/// the top down.
pub async fn create_shared_dir(path: &Path) -> crate::error::Result<()> {
    tokio::fs::create_dir_all(path).await?;
    set_mode(path, SHARED_DIR_MODE).await;
    Ok(())
}

/// Create `path` and its missing parents, then let all users enter `path`. Use it for the
/// directories above a working directory: one `0700` directory in the chain stops a task that
/// changed user.
pub async fn ensure_traversable_dir(path: &Path) -> crate::error::Result<()> {
    tokio::fs::create_dir_all(path).await?;
    set_mode(path, TRAVERSABLE_DIR_MODE).await;
    Ok(())
}

/// Set `mode` on a path that exists. A failure is only logged: the usual cause is a directory that
/// this process does not own, and only a task that changes user is affected.
#[cfg(unix)]
async fn set_mode(path: &Path, mode: u32) {
    if let Err(e) = tokio::fs::set_permissions(path, Permissions::from_mode(mode)).await {
        tracing::warn!(
            "Failed to set mode {mode:o} on {}: {e}. A task that changes user possibly cannot use it",
            path.display()
        );
    }
}

#[cfg(not(unix))]
async fn set_mode(_path: &Path, _mode: u32) {}

/// Log a warning for each parent of `path` that a task that runs as a different user cannot
/// enter. The executor does not change these directories: the operator decides (the usual case is
/// a `$HOME` with mode `0700`).
#[cfg(unix)]
pub async fn warn_unreachable_ancestors(path: &Path) {
    // Collect the paths first. A borrow of `path` across the awaits below makes the future not
    // `Send`.
    let ancestors: Vec<PathBuf> = path.ancestors().skip(1).map(PathBuf::from).collect();
    for ancestor in ancestors {
        let Ok(meta) = tokio::fs::metadata(&ancestor).await else {
            continue;
        };
        let mode = meta.permissions().mode();
        if mode & 0o001 == 0 {
            tracing::warn!(
                "{} has mode {:o}: a task that changes user cannot get to the working directory \
                 below it. Give it at least o+x if such tasks can run",
                ancestor.display(),
                mode & 0o7777
            );
        }
    }
}

#[cfg(not(unix))]
pub async fn warn_unreachable_ancestors(_path: &Path) {}

/// Let all users read, write and execute a downloaded input. `download_file` uses the umask, and
/// the client CLI also uses it, where the downloads of a user must stay private. Thus the executor
/// changes the mode here.
///
/// A `local_path` can name a file in a subdirectory (`bin/tool`). The directories that
/// `download_file` created for it also get the shared mode. If the path is not a plain relative
/// path (`..` or an absolute path), only the file changes, because the change must not go above
/// `resource/`.
#[cfg(unix)]
pub(super) async fn share_fetched_resource(resource_root: &Path, file: &Path) {
    set_mode(file, SHARED_FILE_MODE).await;

    let Ok(nested) = file.strip_prefix(resource_root) else {
        return;
    };
    if !nested
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        tracing::warn!(
            "Do not change the directories of {}: the path goes out of the resource directory",
            file.display()
        );
        return;
    }
    // Go up from the file and stop at `resource/`. `reset_workspace` already shares `resource/`.
    let dirs: Vec<PathBuf> = file
        .ancestors()
        .skip(1)
        .take_while(|dir| *dir != resource_root)
        .map(PathBuf::from)
        .collect();
    for dir in dirs {
        set_mode(&dir, SHARED_DIR_MODE).await;
    }
}

#[cfg(not(unix))]
pub(super) async fn share_fetched_resource(_resource_root: &Path, _file: &Path) {}

/// Remove the working directory and create it again, with the directories that the process uses.
/// All users can write to them, because the process can change to a different user.
pub async fn reset_workspace(cache_path: &Path) -> crate::error::Result<()> {
    let _ = tokio::fs::remove_dir_all(cache_path).await;
    // The process writes `new_task.json` to the working directory itself, thus it is also shared.
    create_shared_dir(cache_path).await?;
    create_shared_dir(&cache_path.join("result")).await?;
    create_shared_dir(&cache_path.join("exec")).await?;
    create_shared_dir(&cache_path.join("resource")).await?;
    Ok(())
}
