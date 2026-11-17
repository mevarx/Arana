//! Official Node.js Unix tarballs carry legit symlink shims (`bin/npm`,
//! `bin/npx`). The dedicated regular-member extractor must keep every regular
//! file while never creating links; the default strict extractor must keep
//! refusing the same archive. Fixtures are tar.gz archives so the link
//! headers behave identically on Windows (where no link is created).

use reach_tools::download::{extract_archive, extract_archive_regular_members};
use std::path::Path;

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

fn write_file(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn node_fixture_entries() -> Vec<TarEntry<'static>> {
    vec![
        TarEntry::File("node-v24.14.1-linux-x64/bin/node", b"ELF-node"),
        TarEntry::Symlink(
            "node-v24.14.1-linux-x64/bin/npm",
            "../lib/node_modules/npm/bin/npm-cli.js",
        ),
        TarEntry::Symlink(
            "node-v24.14.1-linux-x64/bin/npx",
            "../lib/node_modules/npm/bin/npx-cli.js",
        ),
        TarEntry::Hardlink(
            "node-v24.14.1-linux-x64/bin/corepack",
            "node-v24.14.1-linux-x64/lib/node_modules/corepack/dist/corepack.js",
        ),
        TarEntry::File(
            "node-v24.14.1-linux-x64/lib/node_modules/npm/bin/npm-cli.js",
            b"#!/usr/bin/env node\nnpm-cli",
        ),
        TarEntry::File(
            "node-v24.14.1-linux-x64/lib/node_modules/npm/bin/npx-cli.js",
            b"#!/usr/bin/env node\nnpx-cli",
        ),
        TarEntry::File("node-v24.14.1-linux-x64/README.md", b"readme"),
    ]
}

#[tokio::test]
async fn regular_member_extractor_keeps_files_and_skips_official_node_links() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("node-v24.14.1-linux-x64.tar.gz");
    write_file(&archive, &build_tar_gz(&node_fixture_entries()));
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let files = extract_archive_regular_members(&archive, &dest)
        .await
        .expect("regular members extract despite link entries");
    assert_eq!(files.len(), 4, "only regular files are reported: {files:?}");
    let root = dest.join("node-v24.14.1-linux-x64");
    assert_eq!(std::fs::read(root.join("bin/node")).unwrap(), b"ELF-node");
    assert_eq!(
        std::fs::read(root.join("lib/node_modules/npm/bin/npm-cli.js")).unwrap(),
        b"#!/usr/bin/env node\nnpm-cli"
    );
    assert!(root.join("README.md").is_file());
    for shim in ["bin/npm", "bin/npx", "bin/corepack"] {
        assert!(
            std::fs::symlink_metadata(root.join(shim)).is_err(),
            "link entry {shim} must not exist after extraction"
        );
    }
}

#[tokio::test]
async fn strict_extractor_still_refuses_the_same_node_archive() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("node-v24.14.1-linux-x64.tar.gz");
    write_file(&archive, &build_tar_gz(&node_fixture_entries()));
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let err = extract_archive(&archive, &dest)
        .await
        .expect_err("the default strict extractor keeps refusing links");
    assert_eq!(err.code, "BAD_INPUT");
    assert!(err.message.contains("symlink"), "{}", err.message);
    assert_eq!(
        std::fs::read_dir(&dest).unwrap().count(),
        0,
        "strict extraction writes nothing before validation"
    );
}

#[tokio::test]
async fn regular_member_extractor_still_validates_traversal_types_and_bytes() {
    let cases: Vec<(Vec<TarEntry<'static>>, &str)> = vec![
        (
            vec![TarEntry::UnsafeName("../escaped.txt", b"payload")],
            "escapes the destination",
        ),
        (
            vec![TarEntry::File("ok.txt", b"ok"), TarEntry::Fifo("evil-fifo")],
            "special",
        ),
        (
            vec![TarEntry::DeclaredFile("huge.bin", 300 * 1024 * 1024)],
            "size limit",
        ),
    ];
    for (index, (entries, needle)) in cases.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join(format!("case-{index}.tar.gz"));
        write_file(&archive, &build_tar_gz(entries));
        let dest = dir.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let err = extract_archive_regular_members(&archive, &dest)
            .await
            .expect_err("unsafe entry rejected");
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
async fn regular_member_extractor_counts_link_entries_against_the_cap() {
    let names: Vec<String> = (0..4097).map(|i| format!("link{i}")).collect();
    let entries: Vec<TarEntry<'_>> = names
        .iter()
        .map(|name| TarEntry::Symlink(name.as_str(), "target"))
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("many-links.tar.gz");
    write_file(&archive, &build_tar_gz(&entries));
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let err = extract_archive_regular_members(&archive, &dest)
        .await
        .expect_err("link entries count against the entry cap");
    assert_eq!(err.code, "BAD_INPUT");
    assert!(err.message.contains("entry count"), "{}", err.message);
    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
}
