use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
fn collect(root: &Path, extension: &[&str], files: &mut Vec<PathBuf>) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source inputs must not be symlinks",
            ));
        }
        if kind.is_dir() {
            collect(&path, extension, files)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| extension.contains(&e))
        {
            files.push(path);
        }
    }
    Ok(())
}
/// Hash actual runtime/build assets and dependency pins with stable relative
/// paths. This invalidates acceptance when embedded registries/skills change,
/// not just when a Rust source file changes.
pub fn source_signature(root: &Path) -> io::Result<String> {
    let mut files = Vec::new();
    collect(&root.join("crates"), &["rs", "toml"], &mut files)?;
    collect(&root.join("tests/golden"), &["json"], &mut files)?;
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "skills/arana/SKILL.md",
        "schema.json",
    ] {
        files.push(root.join(name));
    }
    files.sort();
    let mut hash = Sha256::new();
    hash.update(b"arana-source-v2\0");
    for file in files {
        let relative = file
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = fs::read(&file)?;
        hash.update((relative.len() as u64).to_be_bytes());
        hash.update(relative.as_bytes());
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    Ok(format!("{:x}", hash.finalize()))
}
