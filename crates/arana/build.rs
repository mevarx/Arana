mod build_support;
use std::path::Path;
fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("Cargo manifest directory");
    let root = Path::new(&manifest)
        .parent()
        .and_then(Path::parent)
        .expect("workspace directory");
    for name in [
        "crates",
        "tests/golden",
        "Cargo.toml",
        "Cargo.lock",
        "skills/arana/SKILL.md",
        "schema.json",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(name).display());
    }
    println!(
        "cargo:rustc-env=ARANA_BUILD_SIGNATURE={}",
        build_support::source_signature(root).expect("hash runtime source inputs")
    );
}
