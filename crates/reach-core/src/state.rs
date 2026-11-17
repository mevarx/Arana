//! Private, bounded, symlink-guarded atomic state with OS execution locks.
use crate::{ReachError, Result};
use fs2::FileExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub fn ensure_safe(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(m) => {
                #[cfg(windows)]
                let reparse = {
                    use std::os::windows::fs::MetadataExt;
                    m.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let reparse = false;
                if m.file_type().is_symlink() || reparse {
                    return Err(ReachError::new(
                        "BAD_INPUT",
                        "Refusing a symlink or reparse point in private state",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub fn private_dir(path: &Path) -> Result<()> {
    ensure_safe(path)?;
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    protect(path, true)?;
    Ok(())
}
fn protect(path: &Path, is_dir: bool) -> Result<()> {
    #[cfg(windows)]
    {
        crate::windows_acl::protect(path, is_dir)?;
    }
    #[cfg(not(windows))]
    let _ = (path, is_dir);
    Ok(())
}
fn open_read(path: &Path) -> Result<File> {
    ensure_safe(path)?;
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(0x00200000);
    }
    let f = o.open(path)?;
    if !f.metadata()?.is_file() {
        return Err(ReachError::new(
            "BAD_INPUT",
            "Private state is not a regular file",
        ));
    }
    Ok(f)
}
pub fn read_private(path: &Path, cap: u64) -> Result<String> {
    let mut buf = String::new();
    open_read(path)?.take(cap + 1).read_to_string(&mut buf)?;
    if buf.len() as u64 > cap {
        return Err(ReachError::new(
            "BAD_INPUT",
            "Private state exceeds read budget",
        ));
    }
    Ok(buf)
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    ensure_safe(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| ReachError::new("BAD_INPUT", "Missing parent directory"))?;
    private_dir(parent)?;
    let mut f = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    protect(f.path(), false)?;
    f.write_all(bytes)?;
    f.as_file().sync_all()?;
    f.persist(path).map_err(|e| ReachError::from(e.error))?;
    Ok(())
}
pub struct FileLock {
    file: File,
}
impl FileLock {
    pub fn try_acquire(root: &Path, name: &str) -> Result<Self> {
        let dir = root.join("browser-locks");
        private_dir(&dir)?;
        let path = dir.join(format!("{:x}.lock", Sha256::digest(name.as_bytes())));
        ensure_safe(&path)?;
        let new = !path.exists();
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        if new {
            protect(&path, false)?;
        }
        file.try_lock_exclusive().map_err(|_| {
            ReachError::new("BUSY", "Private state is being updated by another command")
        })?;
        Ok(Self { file })
    }
    pub async fn wait(root: &Path, name: &str, budget: std::time::Duration) -> Result<Self> {
        let start = std::time::Instant::now();
        loop {
            match Self::try_acquire(root, name) {
                Ok(v) => return Ok(v),
                Err(e) if e.code == "BUSY" && start.elapsed() < budget => {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await
                }
                Err(e) => return Err(e),
            }
        }
    }
}
impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}
#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
}
impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn load(&self) -> Result<Value> {
        let path = self.root.join("state.json");
        ensure_safe(&path)?;
        let mut v = read_private(&path, 1024 * 1024)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        v["version"] = v.get("version").cloned().unwrap_or(json!(1));
        for k in [
            "channels",
            "tools",
            "checks",
            "skill",
            "backend_health",
            "cooldowns",
            "request_starts",
            "owned_paths",
        ] {
            if !v[k].is_object() {
                v[k] = json!({});
            }
        }
        for k in ["links", "unlinked"] {
            if !v["skill"][k].is_array() {
                v["skill"][k] = json!([]);
            }
        }
        if !v["removed_defaults"].is_array() {
            v["removed_defaults"] = json!([]);
        }
        Ok(v)
    }
    pub fn mutate(&self, change: impl FnOnce(&mut Value)) -> Result<()> {
        private_dir(&self.root)?;
        let _lock = FileLock::try_acquire(&self.root, "reach-state-write")?;
        let mut v = self.load()?;
        change(&mut v);
        v["owned_paths"]["state.json"] = json!("state ledger");
        atomic_write(
            &self.root.join("state.json"),
            &serde_json::to_vec_pretty(&v)
                .map_err(|e| ReachError::new("UPSTREAM_BROKEN", e.to_string()))?,
        )
    }
}
