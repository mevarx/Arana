//! Registry downloads: bounded HTTP with sha256 verification and
//! extraction-safety-checked archives (zip-slip rejection, symlinks refused,
//! bundled runtime DLLs kept).
//!
//! Archive extraction is pure Rust (`zip`, `tar`, `flate2`): every entry's
//! name, type, declared size and the global entry/size caps are validated in a
//! first pass before the second pass writes a single byte. Links and device
//! entries are refused, and the destination must be a fresh, guarded private
//! directory.

use crate::fsguard;
use flate2::read::GzDecoder;
use reach_core::{ReachError, Result};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use zip::CompressionMethod;

/// Caps applied to every archive extraction.
const MAX_ENTRIES: usize = 4096;
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
/// Hard cap on the bytes a tar.gz may decompress to (member data plus
/// headers); stops gzip bombs that declare few or small members.
const MAX_DECOMPRESSED_BYTES: u64 = MAX_TOTAL_BYTES + 64 * 1024 * 1024;
/// Longest registry download; a shorter caller deadline is honored.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

fn bad_input(message: impl Into<String>) -> ReachError {
    ReachError::new("BAD_INPUT", message)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Download `url` into the private cache under `state_dir`, verifying the
/// sha256, enforcing `max_bytes` and the caller's deadline (capped by
/// `DOWNLOAD_TIMEOUT`). Follows redirects (each hop still must be plain
/// http(s)). A previously verified copy in the cache is reused. Returns the
/// archive path.
pub async fn download_file(
    url: &str,
    state_dir: &Path,
    expected_sha256: &str,
    max_bytes: u64,
    timeout: Duration,
) -> Result<PathBuf> {
    let cache = state_dir.join("cache").join("downloads");
    reach_core::state::private_dir(&cache)?;
    let key = hex(&Sha256::digest(url.as_bytes()));
    let dest = cache.join(format!("{key}.bin"));
    // A cached copy is reused when it still matches the pin.
    if dest.exists() {
        if file_sha256(&dest)?.eq_ignore_ascii_case(expected_sha256) {
            return Ok(dest);
        }
        let _ = std::fs::remove_file(&dest);
    }

    let download = async {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| ReachError::new("UPSTREAM_BROKEN", format!("client build failed: {e}")))?;
        let resp = client
            .get(url)
            .header("User-Agent", "arana")
            .send()
            .await
            .map_err(|e| {
                if e.is_builder() || e.is_request() {
                    ReachError::new(
                        "BAD_INPUT",
                        format!("download URL is not http(s): {}", e.without_url()),
                    )
                } else {
                    ReachError::from(e)
                }
            })?;
        let resp = resp.error_for_status().map_err(ReachError::from)?;
        if let Some(len) = resp.content_length() {
            if len > max_bytes {
                return Err(ReachError::new(
                    "UPSTREAM_BROKEN",
                    format!("download exceeds {max_bytes} bytes"),
                ));
            }
        }
        // Stream to a temp file beside the destination, hashing as we go.
        let mut tmp = tempfile::NamedTempFile::new_in(&cache)?;
        let mut hasher = Sha256::new();
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;
        let mut total: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(ReachError::from)?;
            total += chunk.len() as u64;
            if total > max_bytes {
                return Err(ReachError::new(
                    "UPSTREAM_BROKEN",
                    format!("download exceeds {max_bytes} bytes"),
                ));
            }
            hasher.update(&chunk);
            tmp.write_all(&chunk).map_err(ReachError::from)?;
        }
        let digest = hex(&hasher.finalize());
        if !digest.eq_ignore_ascii_case(expected_sha256) {
            return Err(ReachError::new(
                "UPSTREAM_BROKEN",
                format!(
                    "checksum mismatch for {}: expected {expected_sha256}, got {digest}",
                    url.rsplit('/').next().unwrap_or("archive")
                ),
            ));
        }
        tmp.as_file().sync_all().ok();
        tmp.persist(&dest).map_err(|e| ReachError::from(e.error))?;
        Ok(dest)
    };
    let deadline = timeout.min(DOWNLOAD_TIMEOUT);
    match tokio::time::timeout(deadline, download).await {
        Ok(inner) => inner,
        Err(_) => Err(ReachError::new(
            "TIMEOUT",
            "download did not finish within the deadline",
        )),
    }
}

/// Download `url` into the private cache when the upstream publishes no
/// checksum (Chrome for Testing, Node's SHASUMS index). Returns the cached
/// path and the sha256 of the received bytes; callers record that digest so a
/// later install can be verified. Caps and the deadline are the same as
/// [`download_file`].
pub async fn download_cached(
    url: &str,
    state_dir: &Path,
    max_bytes: u64,
    timeout: Duration,
) -> Result<(PathBuf, String)> {
    let cache = state_dir.join("cache").join("downloads");
    reach_core::state::private_dir(&cache)?;
    let key = hex(&Sha256::digest(url.as_bytes()));
    let dest = cache.join(format!("{key}.bin"));
    if dest.is_file() {
        return Ok((dest.clone(), file_sha256(&dest)?));
    }

    let download = async {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| ReachError::new("UPSTREAM_BROKEN", format!("client build failed: {e}")))?;
        let resp = client
            .get(url)
            .header("User-Agent", "arana")
            .send()
            .await
            .map_err(ReachError::from)?;
        let resp = resp.error_for_status().map_err(ReachError::from)?;
        if let Some(len) = resp.content_length() {
            if len > max_bytes {
                return Err(ReachError::new(
                    "UPSTREAM_BROKEN",
                    format!("download exceeds {max_bytes} bytes"),
                ));
            }
        }
        let mut tmp = tempfile::NamedTempFile::new_in(&cache)?;
        let mut hasher = Sha256::new();
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;
        let mut total: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(ReachError::from)?;
            total += chunk.len() as u64;
            if total > max_bytes {
                return Err(ReachError::new(
                    "UPSTREAM_BROKEN",
                    format!("download exceeds {max_bytes} bytes"),
                ));
            }
            hasher.update(&chunk);
            tmp.write_all(&chunk).map_err(ReachError::from)?;
        }
        tmp.as_file().sync_all().ok();
        let digest = hex(&hasher.finalize());
        tmp.persist(&dest).map_err(|e| ReachError::from(e.error))?;
        Ok((dest, digest))
    };
    let deadline = timeout.min(DOWNLOAD_TIMEOUT);
    match tokio::time::timeout(deadline, download).await {
        Ok(inner) => inner,
        Err(_) => Err(ReachError::new(
            "TIMEOUT",
            "download did not finish within the deadline",
        )),
    }
}

fn file_sha256(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(ReachError::from)?;
    Ok(hex(&Sha256::digest(bytes)))
}

// ── zip central directory ──

/// One zip central-directory record.
#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub name: String,
    /// 0 = stored, 8 = deflate.
    pub method: u16,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    /// High 16 bits of the external attributes (unix mode), if present.
    pub unix_mode: Option<u32>,
}

#[allow(deprecated)]
fn method_code(method: CompressionMethod) -> u16 {
    match method {
        CompressionMethod::Stored => 0,
        CompressionMethod::Deflated => 8,
        CompressionMethod::Unsupported(other) => other,
        _ => u16::MAX,
    }
}

/// Inspect a zip's central directory without extracting: entry names,
/// methods, sizes, unix modes. Used to reject unsafe archives before any
/// bytes are written.
pub fn inspect_zip_entries(archive: &Path) -> Result<Vec<ZipEntry>> {
    let file = File::open(archive).map_err(ReachError::from)?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| bad_input(format!("not a zip archive: {e}")))?;
    inspect_open_zip(&mut zip)
}

fn inspect_open_zip<R: Read + io::Seek>(zip: &mut zip::ZipArchive<R>) -> Result<Vec<ZipEntry>> {
    let total_entries = zip.len();
    if total_entries > MAX_ENTRIES {
        return Err(bad_input(format!(
            "zip entry count {total_entries} exceeds the safety limit {MAX_ENTRIES}"
        )));
    }
    let mut entries = Vec::with_capacity(total_entries);
    for index in 0..total_entries {
        let entry = zip
            .by_index(index)
            .map_err(|e| bad_input(format!("corrupt zip central directory: {e}")))?;
        entries.push(ZipEntry {
            name: entry.name().to_string(),
            method: method_code(entry.compression()),
            compressed_size: entry.compressed_size(),
            uncompressed_size: entry.size(),
            unix_mode: entry.unix_mode(),
        });
    }
    Ok(entries)
}

/// Validate an archive member name and turn it into a relative path. Returns
/// `None` for the archive-root directory entry (`.`/`./`). Names are shared by
/// zip and tar: no absolute paths, drive letters/streams, backslashes or `..`
/// traversal.
fn safe_relative_path(name: &str) -> Result<Option<PathBuf>> {
    if name.is_empty() {
        return Err(bad_input("archive entry has an empty name"));
    }
    if name.contains('\\') {
        return Err(bad_input(format!(
            "archive entry uses backslash separators: {name}"
        )));
    }
    if name.starts_with('/') || name.contains(':') {
        return Err(bad_input(format!(
            "archive entry has an absolute path: {name}"
        )));
    }
    let mut relative = PathBuf::new();
    for component in name.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            return Err(bad_input(format!(
                "archive entry escapes the destination: {name}"
            )));
        }
        relative.push(component);
    }
    if relative.as_os_str().is_empty() {
        if name.ends_with('/') {
            // The archive-root directory entry (e.g. "./"): the destination
            // already exists, so there is nothing to create.
            return Ok(None);
        }
        return Err(bad_input(format!(
            "archive entry has an empty path: {name}"
        )));
    }
    Ok(Some(relative))
}

fn reject_unix_mode(mode: u32, name: &str) -> Result<()> {
    let kind = mode & 0o170000;
    match kind {
        0 | 0o100000 | 0o040000 => Ok(()),
        0o120000 => Err(bad_input(format!("archive entry is a symlink: {name}"))),
        _ => Err(bad_input(format!(
            "archive entry is a special file: {name}"
        ))),
    }
}

/// Validate entry names, types and sizes for safe extraction. Entry count,
/// per-entry size and total-size caps are enforced before extraction.
fn validate_entries(entries: &[ZipEntry]) -> Result<()> {
    if entries.len() > MAX_ENTRIES {
        return Err(bad_input(format!(
            "archive has {} entries; safety limit is {MAX_ENTRIES}",
            entries.len()
        )));
    }
    let mut total: u64 = 0;
    for entry in entries {
        let Some(_relative) = safe_relative_path(&entry.name)? else {
            continue;
        };
        let is_dir = entry.name.ends_with('/');
        if entry.method != 0 && entry.method != 8 && !is_dir {
            return Err(bad_input(format!(
                "archive entry uses an unsupported compression method: {}",
                entry.name
            )));
        }
        if let Some(mode) = entry.unix_mode {
            reject_unix_mode(mode, &entry.name)?;
        }
        if entry.uncompressed_size > MAX_ENTRY_BYTES {
            return Err(bad_input(format!(
                "archive entry exceeds the size limit: {}",
                entry.name
            )));
        }
        total = total.saturating_add(entry.uncompressed_size);
        if total > MAX_TOTAL_BYTES {
            return Err(bad_input("archive total size exceeds the safety limit"));
        }
    }
    Ok(())
}

/// Extract `archive` into `dest` (a fresh private dir), refusing archives with
/// absolute paths, `..` traversal, drive letters, links or device entries, and
/// capping entry count, per-entry size, total size and decompressed bytes.
/// Every entry is validated before the first write. Returns the extracted file
/// paths in archive order. DLLs and every other regular file are kept.
pub async fn extract_archive(archive: &Path, dest: &Path) -> Result<Vec<PathBuf>> {
    let mut magic = [0u8; 4];
    let mut file = File::open(archive).map_err(ReachError::from)?;
    let read = file.read(&mut magic).map_err(ReachError::from)?;
    drop(file);
    prepare_dest(dest)?;
    let is_zip = read >= 2 && &magic[..2] == b"PK";
    let is_gzip = read >= 2 && magic[0] == 0x1F && magic[1] == 0x8B;
    if is_zip {
        extract_zip(archive, dest)
    } else if is_gzip {
        extract_tar_gz(archive, dest)
    } else {
        Err(bad_input("unsupported archive format"))
    }
}

/// Archive container detected from the leading bytes.
enum ArchiveKind {
    Zip,
    TarGz,
}

fn archive_kind(archive: &Path) -> Result<ArchiveKind> {
    let mut magic = [0u8; 4];
    let mut file = File::open(archive).map_err(ReachError::from)?;
    let read = file.read(&mut magic).map_err(ReachError::from)?;
    drop(file);
    if read >= 2 && &magic[..2] == b"PK" {
        Ok(ArchiveKind::Zip)
    } else if read >= 2 && magic[0] == 0x1F && magic[1] == 0x8B {
        Ok(ArchiveKind::TarGz)
    } else {
        Err(bad_input("unsupported archive format"))
    }
}

/// Extract an archive whose payload is regular files, tolerating link members
/// by skipping them entirely.
///
/// Official Node.js Unix tarballs legitimately carry `bin/npm`/`bin/npx`
/// symlink shims pointing into `lib/node_modules/npm`. The default extractor
/// refuses every link and stays that way; this opt-in extractor validates the
/// whole archive first exactly like the strict tar path (entry count, names,
/// types, declared sizes, total and decompressed limits), then writes only
/// regular files and directories. Links are never created, opened or
/// followed. Zip archives take the unchanged strict zip path.
pub async fn extract_archive_regular_members(archive: &Path, dest: &Path) -> Result<Vec<PathBuf>> {
    let kind = archive_kind(archive)?;
    prepare_dest(dest)?;
    match kind {
        ArchiveKind::Zip => extract_zip(archive, dest),
        ArchiveKind::TarGz => {
            validate_tar_gz_regular(archive)?;
            extract_tar_gz_regular(archive, dest)
        }
    }
}

/// Require a fresh private destination: guarded against linked ancestors and
/// empty, so no pre-existing link can redirect a write.
fn prepare_dest(dest: &Path) -> Result<()> {
    reach_core::state::private_dir(dest)?;
    reach_core::state::ensure_safe(dest)?;
    let mut entries = std::fs::read_dir(dest).map_err(ReachError::from)?;
    if entries.next().is_some() {
        return Err(bad_input(
            "archive extraction destination must be a fresh, empty directory",
        ));
    }
    Ok(())
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<Vec<PathBuf>> {
    let file = File::open(archive).map_err(ReachError::from)?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| bad_input(format!("not a zip archive: {e}")))?;
    let entries = inspect_open_zip(&mut zip)?;
    validate_entries(&entries)?;

    let mut total = 0u64;
    let mut files = Vec::new();
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|e| bad_input(format!("corrupt zip archive: {e}")))?;
        let name = entry.name().to_string();
        let Some(relative) = safe_relative_path(&name)? else {
            continue;
        };
        if entry.is_dir() || name.ends_with('/') {
            fsguard::create_dir_chain(dest, &relative)?;
            continue;
        }
        if let Some(parent) = relative.parent() {
            fsguard::create_dir_chain(dest, parent)?;
        }
        let target = dest.join(&relative);
        let declared = entry.size();
        let mut out = open_new_file(&target)?;
        copy_bounded(&mut entry, &mut out, &mut total, &name, declared)?;
        drop(out);
        apply_mode(&target, entry.unix_mode())?;
        files.push(target);
    }
    Ok(files)
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<Vec<PathBuf>> {
    validate_tar_gz(archive)?;
    let file = File::open(archive).map_err(ReachError::from)?;
    let mut tar = tar::Archive::new(BoundedReader::new(
        GzDecoder::new(file),
        MAX_DECOMPRESSED_BYTES,
    ));

    let mut total = 0u64;
    let mut files = Vec::new();
    let entries = tar
        .entries()
        .map_err(|e| bad_input(format!("corrupt tar.gz archive: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| bad_input(format!("corrupt tar.gz archive: {e}")))?;
        let kind = entry.header().entry_type();
        if is_tar_metadata(kind) {
            continue;
        }
        let name = tar_entry_name(&entry)?;
        let Some(relative) = safe_relative_path(&name)? else {
            continue;
        };
        if kind.is_dir() {
            fsguard::create_dir_chain(dest, &relative)?;
            continue;
        }
        if !kind.is_file() {
            return Err(reject_tar_kind(kind, &name));
        }
        if let Some(parent) = relative.parent() {
            fsguard::create_dir_chain(dest, parent)?;
        }
        let target = dest.join(&relative);
        let mode = entry.header().mode().ok();
        let declared = entry.size();
        let mut out = open_new_file(&target)?;
        copy_bounded(&mut entry, &mut out, &mut total, &name, declared)?;
        drop(out);
        apply_mode(&target, mode)?;
        files.push(target);
    }
    Ok(files)
}

/// Second pass of [`extract_archive_regular_members`]: write regular files and
/// directories only; link members are skipped without being created or read.
fn extract_tar_gz_regular(archive: &Path, dest: &Path) -> Result<Vec<PathBuf>> {
    let file = File::open(archive).map_err(ReachError::from)?;
    let mut tar = tar::Archive::new(BoundedReader::new(
        GzDecoder::new(file),
        MAX_DECOMPRESSED_BYTES,
    ));

    let mut total = 0u64;
    let mut files = Vec::new();
    let entries = tar
        .entries()
        .map_err(|e| bad_input(format!("corrupt tar.gz archive: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| bad_input(format!("corrupt tar.gz archive: {e}")))?;
        let kind = entry.header().entry_type();
        if is_tar_metadata(kind) {
            continue;
        }
        let name = tar_entry_name(&entry)?;
        let Some(relative) = safe_relative_path(&name)? else {
            continue;
        };
        if kind.is_dir() {
            fsguard::create_dir_chain(dest, &relative)?;
            continue;
        }
        if kind.is_symlink() || kind.is_hard_link() {
            continue;
        }
        if !kind.is_file() {
            return Err(reject_tar_kind(kind, &name));
        }
        if let Some(parent) = relative.parent() {
            fsguard::create_dir_chain(dest, parent)?;
        }
        let target = dest.join(&relative);
        let mode = entry.header().mode().ok();
        let declared = entry.size();
        let mut out = open_new_file(&target)?;
        copy_bounded(&mut entry, &mut out, &mut total, &name, declared)?;
        drop(out);
        apply_mode(&target, mode)?;
        files.push(target);
    }
    Ok(files)
}

/// First tar.gz pass for [`extract_archive_regular_members`]: every member —
/// including link entries that will be skipped — is counted and its name,
/// type and declared bytes validated before extraction writes anything.
fn validate_tar_gz_regular(archive: &Path) -> Result<()> {
    let file = File::open(archive).map_err(ReachError::from)?;
    let mut tar = tar::Archive::new(BoundedReader::new(
        GzDecoder::new(file),
        MAX_DECOMPRESSED_BYTES,
    ));
    let mut count = 0usize;
    let mut total = 0u64;
    let entries = tar
        .entries()
        .map_err(|e| bad_input(format!("corrupt tar.gz archive: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| bad_input(format!("corrupt tar.gz archive: {e}")))?;
        let kind = entry.header().entry_type();
        if is_tar_metadata(kind) {
            continue;
        }
        count += 1;
        if count > MAX_ENTRIES {
            return Err(bad_input(format!(
                "tar entry count exceeds the safety limit {MAX_ENTRIES}"
            )));
        }
        let name = tar_entry_name(&entry)?;
        let Some(_relative) = safe_relative_path(&name)? else {
            continue;
        };
        if !kind.is_file() && !kind.is_dir() && !kind.is_symlink() && !kind.is_hard_link() {
            return Err(reject_tar_kind(kind, &name));
        }
        // Link entries carry no extracted payload and are skipped; only
        // regular members count against the byte limits.
        let size = if kind.is_file() { entry.size() } else { 0 };
        if size > MAX_ENTRY_BYTES {
            return Err(bad_input(format!(
                "archive entry exceeds the size limit: {name}"
            )));
        }
        total = total.saturating_add(size);
        if total > MAX_TOTAL_BYTES {
            return Err(bad_input("archive total size exceeds the safety limit"));
        }
    }
    Ok(())
}

/// First tar.gz pass: every member is validated before extraction writes.
fn validate_tar_gz(archive: &Path) -> Result<()> {
    let file = File::open(archive).map_err(ReachError::from)?;
    let mut tar = tar::Archive::new(BoundedReader::new(
        GzDecoder::new(file),
        MAX_DECOMPRESSED_BYTES,
    ));
    let mut count = 0usize;
    let mut total = 0u64;
    let entries = tar
        .entries()
        .map_err(|e| bad_input(format!("corrupt tar.gz archive: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| bad_input(format!("corrupt tar.gz archive: {e}")))?;
        let kind = entry.header().entry_type();
        if is_tar_metadata(kind) {
            continue;
        }
        count += 1;
        if count > MAX_ENTRIES {
            return Err(bad_input(format!(
                "tar entry count exceeds the safety limit {MAX_ENTRIES}"
            )));
        }
        let name = tar_entry_name(&entry)?;
        let Some(_relative) = safe_relative_path(&name)? else {
            continue;
        };
        if !kind.is_file() && !kind.is_dir() {
            return Err(reject_tar_kind(kind, &name));
        }
        let size = entry.size();
        if size > MAX_ENTRY_BYTES {
            return Err(bad_input(format!(
                "archive entry exceeds the size limit: {name}"
            )));
        }
        total = total.saturating_add(size);
        if total > MAX_TOTAL_BYTES {
            return Err(bad_input("archive total size exceeds the safety limit"));
        }
    }
    Ok(())
}

fn tar_entry_name<R: Read>(entry: &tar::Entry<'_, R>) -> Result<String> {
    let path = entry
        .path()
        .map_err(|e| bad_input(format!("corrupt tar.gz entry name: {e}")))?;
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| bad_input("archive entry name is not valid UTF-8"))
}

fn is_tar_metadata(kind: tar::EntryType) -> bool {
    matches!(
        kind,
        tar::EntryType::XHeader
            | tar::EntryType::XGlobalHeader
            | tar::EntryType::GNULongName
            | tar::EntryType::GNULongLink
    )
}

fn reject_tar_kind(kind: tar::EntryType, name: &str) -> ReachError {
    if kind.is_symlink() {
        bad_input(format!("archive entry is a symlink: {name}"))
    } else if kind.is_hard_link() {
        bad_input(format!("archive entry is a hard link: {name}"))
    } else {
        bad_input(format!("archive entry is a special file: {name}"))
    }
}

/// Create a new regular file, refusing an existing path (including dangling
/// links) and any linked ancestor.
fn open_new_file(target: &Path) -> Result<File> {
    reach_core::state::ensure_safe(target)?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(ReachError::from)
}

fn copy_bounded<R: Read>(
    reader: &mut R,
    out: &mut File,
    total: &mut u64,
    name: &str,
    declared: u64,
) -> Result<u64> {
    let remaining = MAX_TOTAL_BYTES.saturating_sub(*total);
    let cap = remaining.min(MAX_ENTRY_BYTES).min(declared);
    let mut limited = reader.take(cap.saturating_add(1));
    let copied = io::copy(&mut limited, out)
        .map_err(|e| bad_input(format!("corrupt archive entry {name}: {e}")))?;
    if copied > cap {
        return Err(bad_input(format!(
            "archive entry exceeds its declared size or the size limit: {name}"
        )));
    }
    *total += copied;
    Ok(copied)
}

fn apply_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = mode {
            let permissions = mode & 0o777;
            if permissions != 0 {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(permissions))
                    .map_err(ReachError::from)?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// A reader that fails when the stream expands past `cap` bytes, stopping
/// gzip bombs before they consume the disk or memory.
struct BoundedReader<R> {
    inner: R,
    read: u64,
    cap: u64,
}

impl<R> BoundedReader<R> {
    fn new(inner: R, cap: u64) -> Self {
        Self {
            inner,
            read: 0,
            cap,
        }
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.read >= self.cap {
            // Probe one byte so a stream that ends exactly at the cap is fine.
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive expands beyond the decompression limit",
                )),
            };
        }
        let allowed = (self.cap - self.read).min(buf.len() as u64) as usize;
        let read = self.inner.read(&mut buf[..allowed])?;
        self.read += read as u64;
        Ok(read)
    }
}
