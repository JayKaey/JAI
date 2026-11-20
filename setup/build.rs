//! Packs the files of the folder JAI_SETUP_PAYLOAD names (J.A.I.'s program and the libraries it
//! starts with) into the setup. Without it the setup holds nothing and says so when started.

#[allow(dead_code)] // (this packs; the setup unpacks)
#[path = "src/pack.rs"]
mod pack;

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=JAI_SETUP_PAYLOAD");
    let mut files = Vec::new();
    if let Some(dir) = std::env::var_os("JAI_SETUP_PAYLOAD").filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        println!("cargo:rerun-if-changed={}", dir.display());
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("JAI_SETUP_PAYLOAD {}: {e}", dir.display())) {
            let path = entry.expect("a payload file").path();
            if path.is_file() {
                println!("cargo:rerun-if-changed={}", path.display());
                let name = path.file_name().and_then(|n| n.to_str()).expect("a payload file's name").to_string();
                files.push((name, std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))));
            }
        }
        assert!(!files.is_empty(), "JAI_SETUP_PAYLOAD {} holds no files", dir.display());
        files.sort();
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("payload.bin");
    std::fs::write(&out, pack::pack(&files)).expect("payload.bin");
}
