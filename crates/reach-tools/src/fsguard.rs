//! Filesystem guards shared by archive extraction and tool install/remove:
//! never treat a symlink or Windows reparse point (junction) as a directory.

use reach_core::{ReachError, Result};
use std::fs::Metadata;
use std::io;
use std::path::Path;

pub(crate) fn is_link_like(meta: &Metadata) -> bool {
    let symlink = meta.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        symlink || meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        symlink
    }
}

/// A real directory: `symlink_metadata` says directory and it is not a link or
/// reparse point.
pub(crate) fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.is_dir() && !is_link_like(&meta))
        .unwrap_or(false)
}

/// Create each component below `root`, refusing anything that is not a real
/// directory (never follows symlinks or reparse points).
pub(crate) fn create_dir_chain(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() && !is_link_like(&meta) => {}
            Ok(_) => {
                return Err(ReachError::new(
                    "BAD_INPUT",
                    format!(
                        "path conflicts with a non-directory: {}",
                        relative.display()
                    ),
                ))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir(&current).map_err(ReachError::from)?;
            }
            Err(e) => return Err(ReachError::from(e)),
        }
    }
    Ok(())
}

/// Recursively delete a private tool tree without ever following a link or
/// reparse point. The root itself must be a real directory; links found below
/// it are left in place and reported, so a planted junction can never redirect
/// deletion outside the tool tree. Returns `(files_or_links_deleted, skipped)`.
pub(crate) fn remove_tree_guarded(root: &Path) -> Result<(usize, Vec<String>)> {
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((0, Vec::new())),
        Err(e) => return Err(ReachError::from(e)),
        Ok(meta) if !meta.is_dir() || is_link_like(&meta) => {
            return Err(ReachError::new(
                "NEED_SETUP",
                format!(
                    "refusing to delete {}: not a real directory",
                    root.display()
                ),
            ))
        }
        Ok(_) => {}
    }

    let mut deleted = 0usize;
    let mut skipped = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => {
                skipped.push(dir.to_string_lossy().into_owned());
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                skipped.push(path.to_string_lossy().into_owned());
                continue;
            };
            if is_link_like(&meta) {
                // Remove the link itself; the target is never followed.
                let outcome = if meta.is_dir() {
                    std::fs::remove_dir(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                match outcome {
                    Ok(()) => deleted += 1,
                    Err(_) => skipped.push(path.to_string_lossy().into_owned()),
                }
            } else if meta.is_dir() {
                dirs.push(path.clone());
                stack.push(path);
            } else if meta.is_file() {
                match std::fs::remove_file(&path) {
                    Ok(()) => deleted += 1,
                    Err(_) => skipped.push(path.to_string_lossy().into_owned()),
                }
            } else {
                skipped.push(path.to_string_lossy().into_owned());
            }
        }
    }
    // Deepest first; a non-empty directory (skipped link inside) stays.
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for dir in dirs {
        if is_real_dir(&dir) {
            let _ = std::fs::remove_dir(&dir);
        }
    }
    Ok((deleted, skipped))
}
