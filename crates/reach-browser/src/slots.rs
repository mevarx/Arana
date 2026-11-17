use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::time::sleep;

#[derive(Clone, Copy, Debug)]
pub struct SlotConfig {
    pub per_site: usize,
    pub global: usize,
    pub wait: Duration,
    pub poll: Duration,
}
impl Default for SlotConfig {
    fn default() -> Self {
        Self {
            per_site: 2,
            global: 4,
            wait: Duration::from_secs(120),
            poll: Duration::from_millis(100),
        }
    }
}

pub struct Slots {
    root: PathBuf,
    config: SlotConfig,
}
impl Slots {
    pub fn new(root: impl AsRef<Path>, config: SlotConfig) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            config,
        }
    }
    pub async fn acquire(&self, site: &str) -> reach_core::Result<SlotGuard> {
        reach_core::state::private_dir(&self.root)?;
        let deadline = Instant::now() + self.config.wait.min(Duration::from_secs(120));
        loop {
            for global in 0..self.config.global {
                let Some(g) = try_slot(&self.root.join(format!("global-{global}.lock")))? else {
                    continue;
                };
                for site_slot in 0..self.config.per_site {
                    let hash = hex_digest(site.as_bytes());
                    let Some(s) =
                        try_slot(&self.root.join(format!("site-{hash}-{site_slot}.lock")))?
                    else {
                        continue;
                    };
                    if self
                        .root
                        .file_name()
                        .is_some_and(|name| name == "browser-locks")
                    {
                        if let Some(state_root) = self.root.parent() {
                            crate::touch(state_root)?;
                        }
                    }
                    return Ok(SlotGuard {
                        _global: g,
                        _site: s,
                    });
                }
            }
            if Instant::now() >= deadline {
                return Err(reach_core::ReachError::new(
                    "BUSY",
                    "Browser concurrency limit reached; queue wait exceeded",
                ));
            }
            sleep(
                self.config
                    .poll
                    .min(deadline.saturating_duration_since(Instant::now())),
            )
            .await;
        }
    }
}
fn try_slot(path: &Path) -> reach_core::Result<Option<File>> {
    reach_core::state::ensure_safe(path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let f = options.open(path).map_err(reach_core::ReachError::from)?;
    match f.try_lock_exclusive() {
        Ok(()) => Ok(Some(f)),
        Err(e) if is_lock_contention(&e) => Ok(None),
        Err(e) => Err(reach_core::ReachError::from(e)),
    }
}
pub(super) fn is_lock_contention(error: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        error.kind() == std::io::ErrorKind::WouldBlock
            || matches!(error.raw_os_error(), Some(32) | Some(33))
    }
    #[cfg(not(windows))]
    {
        error.kind() == std::io::ErrorKind::WouldBlock
    }
}
#[derive(Debug)]
pub struct SlotGuard {
    _global: File,
    _site: File,
}
fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
