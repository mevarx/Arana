use sha2::{Digest, Sha256};
use std::{fs, path::Path};
fn hash_tree(path: &Path, hash: &mut Sha256) {
    let mut entries: Vec<_> = fs::read_dir(path)
        .expect("read source tree")
        .map(|e| e.expect("source entry").path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            hash_tree(&p, hash);
        } else if p.extension().is_some_and(|s| s == "rs" || s == "toml") {
            println!("cargo:rerun-if-changed={}", p.display());
            hash.update(
                p.strip_prefix(path)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .as_bytes(),
            );
            hash.update(fs::read(&p).expect("read source"));
        }
    }
}
fn main() {
    let root = Path::new("../..");
    let mut hash = Sha256::new();
    hash_tree(&root.join("crates"), &mut hash);
    println!(
        "cargo:rustc-env=ARANA_BUILD_SIGNATURE={:x}",
        hash.finalize()
    );
}
