//! Download + archive tests: sha256-verified downloads, zip central-directory
//! inspection, zip-slip rejection, symlink refusal, size caps.

use reach_tools::download::{download_file, extract_archive, inspect_zip_entries};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

// ── hand-built STORED zip writer (no external deps) ──

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Build a minimal zip. `unix_mode` sets the external attributes
/// (high 16 bits) with the "made by Unix" marker, used to forge symlinks.
/// `method` 0 stores members; 8 deflates them (raw DEFLATE).
fn build_zip_method(entries: &[(String, Vec<u8>, Option<u32>)], method: u16) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    let mut offsets = Vec::new();
    let mut payload_sizes = Vec::new();
    for (name, data, _unix_mode) in entries {
        let offset = out.len() as u32;
        offsets.push(offset);
        let crc = crc32(data);
        let payload = if method == 8 {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        } else {
            data.clone()
        };
        payload_sizes.push(payload.len() as u32);
        put32(&mut out, 0x0403_4b50); // local file header
        put16(&mut out, 20); // version needed
        put16(&mut out, 0); // flags
        put16(&mut out, method); // method
        put16(&mut out, 0); // time
        put16(&mut out, 0); // date
        put32(&mut out, crc);
        put32(&mut out, payload.len() as u32);
        put32(&mut out, data.len() as u32);
        put16(&mut out, name.len() as u16);
        put16(&mut out, 0); // extra len
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&payload);
    }
    let cd_start = out.len() as u32;
    for ((name, data, unix_mode), (offset, csize)) in
        entries.iter().zip(offsets.iter().zip(&payload_sizes))
    {
        let crc = crc32(data);
        put32(&mut central, 0x0201_4b50);
        put16(&mut central, (3 << 8) | 20); // made by unix
        put16(&mut central, 20); // version needed
        put16(&mut central, 0); // flags
        put16(&mut central, method);
        put16(&mut central, 0); // time
        put16(&mut central, 0); // date
        put32(&mut central, crc);
        put32(&mut central, *csize);
        put32(&mut central, data.len() as u32);
        put16(&mut central, name.len() as u16);
        put16(&mut central, 0); // extra
        put16(&mut central, 0); // comment
        put16(&mut central, 0); // disk
        put16(&mut central, 0); // internal attrs
        put32(&mut central, unix_mode.map(|m| m << 16).unwrap_or(0));
        put32(&mut central, *offset);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    put32(&mut out, 0x0605_4b50); // EOCD
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, entries.len() as u16);
    put16(&mut out, entries.len() as u16);
    put32(&mut out, cd_size);
    put32(&mut out, cd_start);
    put16(&mut out, 0);
    out
}

fn build_zip(entries: &[(String, Vec<u8>, Option<u32>)]) -> Vec<u8> {
    build_zip_method(entries, 0)
}

// ── pure-Rust tar.gz fixtures ──

enum TarEntry<'a> {
    File(&'a str, &'a [u8]),
    Symlink(&'a str, &'a str),
    Hardlink(&'a str, &'a str),
    Fifo(&'a str),
    /// A regular file whose header name bypasses the builder's traversal
    /// check, used to forge an archive that must be refused.
    UnsafeName(&'a str, &'a [u8]),
    /// A header that declares `size` bytes but carries no data.
    DeclaredFile(&'a str, u64),
}

fn build_tar_gz(entries: &[TarEntry<'_>]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    {
        let mut builder = tar::Builder::new(&mut encoder);
        for entry in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            match entry {
                TarEntry::File(name, data) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(data.len() as u64);
                    builder.append_data(&mut header, name, *data).unwrap();
                }
                TarEntry::Symlink(name, target) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_mode(0o777);
                    header.set_size(0);
                    builder.append_link(&mut header, name, target).unwrap();
                }
                TarEntry::Hardlink(name, target) => {
                    header.set_entry_type(tar::EntryType::Link);
                    header.set_size(0);
                    builder.append_link(&mut header, name, target).unwrap();
                }
                TarEntry::Fifo(name) => {
                    header.set_entry_type(tar::EntryType::Fifo);
                    header.set_size(0);
                    header.set_path(name).unwrap();
                    header.set_cksum();
                    builder.append(&header, std::io::empty()).unwrap();
                }
                TarEntry::UnsafeName(name, data) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(data.len() as u64);
                    let bytes = name.as_bytes();
                    header.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
                    header.set_cksum();
                    builder.append(&header, *data).unwrap();
                }
                TarEntry::DeclaredFile(name, size) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(*size);
                    header.set_path(name).unwrap();
                    header.set_cksum();
                    builder.append(&header, std::io::empty()).unwrap();
                }
            }
        }
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn simple_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    build_zip(
        &entries
            .iter()
            .map(|(name, data)| (name.to_string(), data.as_bytes().to_vec(), None))
            .collect::<Vec<_>>(),
    )
}

fn write_file(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

// ── inspection + extraction ──

#[test]
fn inspects_zip_central_directory_entries() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("a.zip");
    std::fs::write(
        &zip,
        simple_zip(&[
            ("obscura.exe", "binary-bytes"),
            ("obscura-worker.exe", "worker-bytes"),
            ("vcruntime140.dll", "dll-bytes"),
            ("docs/readme.txt", "hello"),
        ]),
    )
    .unwrap();
    let entries = inspect_zip_entries(&zip).expect("zip parses");
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].name, "obscura.exe");
    assert_eq!(entries[0].method, 0);
    assert_eq!(entries[0].uncompressed_size, 12);
    assert!(entries.iter().any(|e| e.name == "vcruntime140.dll"));
}

#[tokio::test]
async fn extracts_stored_zip_and_keeps_dlls() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("a.zip");
    std::fs::write(
        &zip,
        simple_zip(&[
            ("obscura.exe", "binary-bytes"),
            ("obscura-worker.exe", "worker-bytes"),
            ("vcruntime140.dll", "dll-bytes"),
            ("docs/readme.txt", "hello"),
        ]),
    )
    .unwrap();
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let files = extract_archive(&zip, &dest)
        .await
        .expect("extraction works");
    // DLLs (and every other regular file) are kept beside the executables.
    assert_eq!(
        std::fs::read(dest.join("vcruntime140.dll")).unwrap(),
        b"dll-bytes"
    );
    assert_eq!(
        std::fs::read(dest.join("obscura.exe")).unwrap(),
        b"binary-bytes"
    );
    assert_eq!(
        std::fs::read(dest.join("docs").join("readme.txt")).unwrap(),
        b"hello"
    );
    assert_eq!(
        files.len(),
        4,
        "every extracted file is reported: {files:?}"
    );
    assert!(files.iter().all(|p| p.starts_with(&dest)));
}

#[tokio::test]
async fn rejects_zip_slip_traversal_and_absolute_paths() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    for evil in [
        "../escaped.txt",
        "/absolute.txt",
        "C:/drive.txt",
        "a/../../up.txt",
        "a\\b.txt",
    ] {
        let zip = dir.path().join("evil.zip");
        write_file(&zip, &simple_zip(&[(evil, "payload")]));
        let dest = dir.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let err = extract_archive(&zip, &dest)
            .await
            .expect_err("unsafe entries must be rejected");
        assert_eq!(err.code, "BAD_INPUT", "{evil}: {}", err.message);
        assert!(!outside.path().join("escaped.txt").exists());
        assert!(!dest.join("escaped.txt").exists(), "{evil}");
    }
}

#[tokio::test]
async fn rejects_symlink_entries() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("link.zip");
    // Unix mode 0o120777 = symlink (S_IFLNK).
    write_file(
        &zip,
        &build_zip(&[(
            "evil-link".into(),
            b"../../etc/passwd".to_vec(),
            Some(0o120777),
        )]),
    );
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let err = extract_archive(&zip, &dest)
        .await
        .expect_err("symlink rejected");
    assert_eq!(err.code, "BAD_INPUT");
    assert!(err.message.contains("symlink"), "{}", err.message);
}

#[tokio::test]
async fn rejects_archives_over_the_entry_cap() {
    let dir = tempfile::tempdir().unwrap();
    // MAX_ENTRIES is 4096; forge a zip with more (tiny) members.
    let entries: Vec<(String, Vec<u8>, Option<u32>)> = (0..5000)
        .map(|i| (format!("file{i}.txt"), vec![b'x'; 8], None))
        .collect();
    let zip = dir.path().join("many.zip");
    write_file(&zip, &build_zip(&entries));
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let err = extract_archive(&zip, &dest)
        .await
        .expect_err("entry cap enforced");
    assert_eq!(err.code, "BAD_INPUT");
    assert!(err.message.contains("entry count"), "{}", err.message);
}

#[tokio::test]
async fn extracts_compressed_zips_through_the_system_tar() {
    // Regression: GNU tar from git-bash misreads `C:\...` archive paths as
    // remote-host specs; the extractor must use System32 bsdtar on Windows.
    let system_tar = std::env::var("SystemRoot")
        .ok()
        .map(|root| {
            std::path::PathBuf::from(root)
                .join("System32")
                .join("tar.exe")
        })
        .filter(|p| p.exists());
    let Some(tar) = system_tar else {
        eprintln!("skipping: Windows System32 tar.exe not available");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("nested")).unwrap();
    std::fs::write(src.join("obscura.exe"), b"main-exe-bytes").unwrap();
    std::fs::write(src.join("vcruntime140.dll"), b"dll-bytes").unwrap();
    std::fs::write(src.join("nested").join("note.txt"), b"nested-note").unwrap();
    // Build a DEFLATE zip fixture with bsdtar (-a picks zip by extension).
    let zip = dir.path().join("fixture.zip");
    let built = std::process::Command::new(&tar)
        .arg("-a")
        .arg("-cf")
        .arg(&zip)
        .arg("-C")
        .arg(&src)
        .arg(".")
        .output()
        .expect("tar runs");
    assert!(built.status.success(), "fixture build failed: {built:?}");
    // Confirm it is really compressed (deflate), forcing the tar path.
    let entries = inspect_zip_entries(&zip).expect("zip parses");
    assert!(
        entries.iter().any(|e| e.method == 8),
        "fixture must contain deflate members: {entries:?}"
    );
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let files = extract_archive(&zip, &dest)
        .await
        .expect("deflate extraction works");
    assert_eq!(
        std::fs::read(dest.join("obscura.exe")).unwrap(),
        b"main-exe-bytes"
    );
    assert_eq!(
        std::fs::read(dest.join("vcruntime140.dll")).unwrap(),
        b"dll-bytes"
    );
    assert_eq!(
        std::fs::read(dest.join("nested").join("note.txt")).unwrap(),
        b"nested-note"
    );
    assert_eq!(files.len(), 3, "{files:?}");
}

// ── compressed archives: prevalidation before any write ──

#[tokio::test]
async fn rejects_deflate_zip_symlinks_before_writing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("deflate-link.zip");
    let entries = vec![
        ("obscura.exe".to_string(), b"main".to_vec(), None),
        (
            "evil-link".to_string(),
            b"../../outside.txt".to_vec(),
            Some(0o120777),
        ),
    ];
    write_file(&zip, &build_zip_method(&entries, 8));
    let parsed = inspect_zip_entries(&zip).expect("deflate zip parses");
    assert_eq!(parsed[0].method, 8, "fixture really is deflate");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let err = extract_archive(&zip, &dest)
        .await
        .expect_err("deflate symlink rejected");
    assert_eq!(err.code, "BAD_INPUT");
    assert!(err.message.contains("symlink"), "{}", err.message);
    assert_eq!(
        std::fs::read_dir(&dest).unwrap().count(),
        0,
        "nothing may be written before the archive is validated"
    );
}

#[tokio::test]
async fn extracts_tar_gz_files_and_nested_directories() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("payload.tar.gz");
    write_file(
        &archive,
        &build_tar_gz(&[
            TarEntry::File("obscura", b"main-binary"),
            TarEntry::File("nested/note.txt", b"nested-note"),
        ]),
    );
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let files = extract_archive(&archive, &dest)
        .await
        .expect("tar.gz extraction works");
    assert_eq!(std::fs::read(dest.join("obscura")).unwrap(), b"main-binary");
    assert_eq!(
        std::fs::read(dest.join("nested").join("note.txt")).unwrap(),
        b"nested-note"
    );
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(files.iter().all(|p| p.starts_with(&dest)));
}

#[tokio::test]
async fn rejects_tar_gz_links_devices_and_traversal_before_writing() {
    let cases: Vec<(Vec<TarEntry<'_>>, &str)> = vec![
        (
            vec![
                TarEntry::File("obscura", b"main"),
                TarEntry::Symlink("evil-link", "../../outside.txt"),
            ],
            "symlink",
        ),
        (
            vec![
                TarEntry::File("obscura", b"main"),
                TarEntry::Hardlink("evil-hard", "obscura"),
            ],
            "hard link",
        ),
        (
            vec![
                TarEntry::File("obscura", b"main"),
                TarEntry::Fifo("evil-fifo"),
            ],
            "special",
        ),
        (
            vec![TarEntry::UnsafeName("../escaped.txt", b"payload")],
            "escapes the destination",
        ),
    ];
    for (index, (entries, needle)) in cases.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join(format!("case-{index}.tar.gz"));
        write_file(&archive, &build_tar_gz(entries));
        let dest = dir.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let err = extract_archive(&archive, &dest)
            .await
            .expect_err("unsafe tar.gz entry rejected");
        assert_eq!(err.code, "BAD_INPUT", "{needle}: {}", err.message);
        assert!(err.message.contains(needle), "{needle}: {}", err.message);
        assert_eq!(
            std::fs::read_dir(&dest).unwrap().count(),
            0,
            "{needle}: nothing may be written before validation"
        );
    }
}

#[tokio::test]
async fn rejects_tar_gz_over_the_entry_cap_and_declared_size_cap() {
    // 4097 members: one past MAX_ENTRIES.
    let names: Vec<String> = (0..4097).map(|i| format!("f{i}.txt")).collect();
    let entries: Vec<TarEntry<'_>> = names
        .iter()
        .map(|name| TarEntry::File(name.as_str(), &[]))
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("many.tar.gz");
    write_file(&archive, &build_tar_gz(&entries));
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let err = extract_archive(&archive, &dest)
        .await
        .expect_err("entry cap enforced for tar.gz");
    assert_eq!(err.code, "BAD_INPUT");
    assert!(err.message.contains("entry count"), "{}", err.message);
    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);

    // A header that claims 300 MiB is refused before its data is read.
    let archive = dir.path().join("huge.tar.gz");
    write_file(
        &archive,
        &build_tar_gz(&[TarEntry::DeclaredFile("huge.bin", 300 * 1024 * 1024)]),
    );
    let dest = dir.path().join("out-huge");
    std::fs::create_dir_all(&dest).unwrap();
    let err = extract_archive(&archive, &dest)
        .await
        .expect_err("declared oversize entry rejected");
    assert_eq!(err.code, "BAD_INPUT");
    assert!(err.message.contains("size limit"), "{}", err.message);
    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
}

// ── downloads ──

/// One-shot HTTP fixture server bound to 127.0.0.1 with per-path handlers.
struct Server {
    port: u16,
    handle: Option<std::thread::JoinHandle<()>>,
}

type Route = Box<dyn Fn() -> (u16, Vec<u8>, Vec<(String, String)>) + Send + Sync>;

impl Server {
    fn spawn(routes: Vec<(&'static str, Route)>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let Ok(req) = read_request(&mut stream) else {
                    continue;
                };
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut response = None;
                for (route, handler) in &routes {
                    if *route == path {
                        response = Some(handler());
                        break;
                    }
                }
                let (status, body, headers) =
                    response.unwrap_or_else(|| (404, b"not found".to_vec(), Vec::new()));
                let mut head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\n", body.len());
                for (k, v) in headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("Connection: close\r\n\r\n");
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self {
            port,
            handle: Some(handle),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        let _ = self.handle.take();
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> std::io::Result<String> {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn downloads_verify_sha256_and_cache() {
    let body = b"archive-bytes-here".to_vec();
    let good_sha = hex(&Sha256::digest(&body));
    let server = Server::spawn(vec![(
        "/obscura.zip",
        Box::new(move || (200, body.clone(), Vec::new())),
    )]);
    let url = format!("http://127.0.0.1:{}/obscura.zip", server.port);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();

    let path = download_file(&url, state, &good_sha, 1024 * 1024, Duration::from_secs(10))
        .await
        .expect("download succeeds");
    assert_eq!(std::fs::read(&path).unwrap(), b"archive-bytes-here");
    let first = path.clone();

    // A second download of the same URL reuses the verified cache entry.
    let again = download_file(&url, state, &good_sha, 1024 * 1024, Duration::from_secs(10))
        .await
        .expect("cached download succeeds");
    assert_eq!(again, first);
}

#[tokio::test]
async fn download_rejects_checksum_mismatch_and_oversize() {
    let body = b"some-archive".to_vec();
    let good_sha = hex(&Sha256::digest(&body));
    let server = Server::spawn(vec![
        (
            "/obscura.zip",
            Box::new(move || (200, body.clone(), Vec::new())),
        ),
        (
            "/missing.zip",
            Box::new(|| (404, b"gone".to_vec(), Vec::new())),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();

    // Wrong sha → mismatch, and nothing is left behind.
    let err = download_file(
        &format!("http://127.0.0.1:{}/obscura.zip", server.port),
        state,
        &"0".repeat(64),
        1024 * 1024,
        Duration::from_secs(10),
    )
    .await
    .expect_err("checksum mismatch fails");
    assert_eq!(err.code, "UPSTREAM_BROKEN");
    assert!(err.message.contains("checksum"), "{}", err.message);

    // Oversized body → rejected before writing unbounded bytes.
    let err = download_file(
        &format!("http://127.0.0.1:{}/obscura.zip", server.port),
        state,
        &good_sha,
        4,
        Duration::from_secs(10),
    )
    .await
    .expect_err("oversized download fails");
    assert_eq!(err.code, "UPSTREAM_BROKEN");

    // 404 maps to NOT_FOUND.
    let err = download_file(
        &format!("http://127.0.0.1:{}/missing.zip", server.port),
        state,
        &good_sha,
        1024 * 1024,
        Duration::from_secs(10),
    )
    .await
    .expect_err("404 fails");
    assert_eq!(err.code, "NOT_FOUND");
}

#[tokio::test]
async fn download_honors_a_shorter_caller_deadline() {
    // The server answers after 2s; a 150ms caller deadline must win instead of
    // being stretched to the 600s registry default.
    let server = Server::spawn(vec![(
        "/slow.zip",
        Box::new(|| {
            std::thread::sleep(Duration::from_secs(2));
            (200, b"late archive".to_vec(), Vec::new())
        }),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let err = download_file(
        &format!("http://127.0.0.1:{}/slow.zip", server.port),
        dir.path(),
        &"0".repeat(64),
        1024 * 1024,
        Duration::from_millis(150),
    )
    .await
    .expect_err("short deadline must time out");
    assert_eq!(err.code, "TIMEOUT");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "caller deadline was ignored: elapsed {:?}",
        started.elapsed()
    );
}
