//! Windows resources: app icon (tray, taskbar, Explorer), version info (the name shown in Task
//! Manager) and a manifest declaring per-monitor DPI awareness and themed common controls.
//!
//! The icon is rendered here from `src/app_icon.rs` (the Stark aperture crest, pure Rust) into
//! `assets/jarvis_icon.ico` before the resource script embeds it.
//!
//! The "show me your core" sphere's module graph is generated here too: every `src/*.rs` file, its
//! lines of code and the files it uses (`crate::x`, `use crate::{x, y}`, `super::` in the
//! `#[path]` GUI modules, `mod x;` in main.rs), written to `$OUT_DIR/core_graph.rs`.

#[allow(unexpected_cfgs)]
#[path = "src/app_icon.rs"]
mod app_icon;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=assets/jarvis.rc");
    println!("cargo:rerun-if-changed=assets/jarvis.manifest");
    println!("cargo:rerun-if-changed=src");
    // `app_icon::generate_rose_gyro_app_icon` (egui types) only exists in the application itself.
    println!("cargo::rustc-check-cfg=cfg(jarvis_app)");
    println!("cargo:rustc-cfg=jarvis_app");

    write_icon();
    write_core_graph();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // The main thread runs the start-up and the egui window: 8 MB of stack instead of
        // Windows' default 1 MB (it overflowed once the start-up grew).
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg-bins=/STACK:8388608");
        }
        embed_resource::compile("assets/jarvis.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("compiling assets/jarvis.rc (Windows SDK resource compiler)");
    }
}

/// Re-renders the icon only when `src/app_icon.rs` is newer than it (the build script reruns on
/// every source change for the core graph).
fn write_icon() {
    let path = Path::new("assets/jarvis_icon.ico");
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if let (Some(icon), Some(source)) = (modified(path), modified(Path::new("src/app_icon.rs"))) {
        if icon >= source {
            return;
        }
    }
    let ico = app_icon::icon_ico();
    if std::fs::read(path).ok().as_deref() != Some(ico.as_slice()) {
        std::fs::write(path, &ico).expect("writing assets/jarvis_icon.ico");
    }
}

/// The `crate::name` / `crate::{a, b}` module names in a source file.
fn crate_references(text: &str) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(found) = text[at..].find("crate::") {
        let start = at + found + "crate::".len();
        at = start;
        if text[start..].starts_with('{') {
            // `crate::{a, b::c, d}`: the first segment of every item at the top level.
            let mut depth = 0;
            let mut item = String::new();
            let mut first_segment = true;
            for (i, c) in text[start..].char_indices() {
                match c {
                    '{' => {
                        depth += 1;
                        if depth > 1 {
                            first_segment = false;
                        }
                    }
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            if !item.is_empty() {
                                *out.entry(item.clone()).or_insert(0) += 1;
                            }
                            at = start + i;
                            break;
                        }
                    }
                    ',' if depth == 1 => {
                        if !item.is_empty() {
                            *out.entry(item.clone()).or_insert(0) += 1;
                        }
                        item.clear();
                        first_segment = true;
                    }
                    ':' if depth == 1 => first_segment = false,
                    c if depth == 1 && first_segment && (c.is_alphanumeric() || c == '_') => item.push(c),
                    _ => {}
                }
            }
        } else {
            let name: String = text[start..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let next = bytes.get(start + name.len()).copied();
            if !name.is_empty() && next == Some(b':') {
                *out.entry(name).or_insert(0) += 1;
            }
        }
    }
    out
}

fn write_core_graph() {
    let mut files: BTreeMap<String, (u32, String)> = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir("src") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let loc = text.lines().filter(|l| !l.trim().is_empty()).count() as u32;
            files.insert(name, (loc, text));
        }
    }
    let names: Vec<String> = files.keys().cloned().collect();
    let index = |module: &str| names.iter().position(|n| n == &format!("{module}.rs"));
    let mut edges: BTreeMap<(usize, usize), u32> = BTreeMap::new();
    for (i, name) in names.iter().enumerate() {
        let text = &files[name].1;
        for (module, count) in crate_references(text) {
            if let Some(j) = index(&module).filter(|j| *j != i) {
                *edges.entry((i, j)).or_insert(0) += count;
            }
        }
        // The GUI's `#[path]` modules reach their parent through `super::`.
        if name.starts_with("gui_") && text.contains("super::") {
            if let Some(j) = index("gui") {
                *edges.entry((i, j)).or_insert(0) += 1;
            }
        }
        if name == "main.rs" {
            let declared: BTreeSet<String> = text
                .lines()
                .filter_map(|l| l.trim().strip_prefix("mod ").and_then(|r| r.strip_suffix(';')).map(str::to_string))
                .collect();
            for module in declared {
                if let Some(j) = index(&module).filter(|j| *j != i) {
                    edges.entry((i, j)).or_insert(1);
                }
            }
        }
    }
    let mut out = String::from("/// Generated by build.rs: every `src/*.rs` file and its non-empty lines.\npub static CORE_FILES: &[(&str, u32)] = &[\n");
    for name in &names {
        out.push_str(&format!("    ({name:?}, {}),\n", files[name].0));
    }
    out.push_str("];\n/// Generated by build.rs: (from, to, references) between `CORE_FILES` indices.\npub static CORE_EDGES: &[(u16, u16, u16)] = &[\n");
    for ((a, b), n) in &edges {
        out.push_str(&format!("    ({a}, {b}, {}),\n", (*n).min(u16::MAX as u32)));
    }
    out.push_str("];\n");
    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("core_graph.rs");
    if std::fs::read_to_string(&dest).ok().as_deref() != Some(out.as_str()) {
        std::fs::write(dest, out).expect("writing core_graph.rs");
    }
}
