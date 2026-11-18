#[path = "../build_support.rs"]
mod build_support;
use std::{fs, path::Path};
fn fixture(root: &Path) {
    fs::create_dir_all(root.join("crates/example/src")).unwrap();
    fs::write(root.join("crates/example/src/lib.rs"), "pub fn demo() {}\n").unwrap();
    for (name, value) in [
        ("Cargo.toml", "[workspace]\n"),
        ("Cargo.lock", "version = 4\n"),
        ("schema.json", "{}\n"),
    ] {
        fs::write(root.join(name), value).unwrap();
    }
    fs::create_dir_all(root.join("skills/arana")).unwrap();
    fs::write(root.join("skills/arana/SKILL.md"), "skill\n").unwrap();
    fs::create_dir_all(root.join("tests/golden")).unwrap();
    fs::write(root.join("tests/golden/registry.json"), "{}\n").unwrap();
}
#[test]
fn signature_changes_for_runtime_assets_dependencies_and_relative_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fixture(root);
    let original = build_support::source_signature(root).unwrap();
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "skills/arana/SKILL.md",
        "schema.json",
        "tests/golden/registry.json",
    ] {
        let old = fs::read(root.join(path)).unwrap();
        fs::write(root.join(path), b"changed\n").unwrap();
        assert_ne!(
            original,
            build_support::source_signature(root).unwrap(),
            "unhashed runtime input: {path}"
        );
        fs::write(root.join(path), old).unwrap();
    }
    fs::rename(root.join("crates/example"), root.join("crates/renamed")).unwrap();
    assert_ne!(
        original,
        build_support::source_signature(root).unwrap(),
        "crate-relative paths must be hashed"
    );
}
#[test]
fn identical_sources_have_location_independent_signatures() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fixture(a.path());
    fixture(b.path());
    assert_eq!(
        build_support::source_signature(a.path()).unwrap(),
        build_support::source_signature(b.path()).unwrap()
    );
}
