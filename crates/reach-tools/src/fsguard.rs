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
