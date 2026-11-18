//! What every HUD block does from its right-click menu, whether it is a tile of a report on the
//! board or an in-app window: `Copy Content`, `Pin Block`, `Maximize`, `Export to Markdown`,
//! `Export to JSON`, `Close`. The board and the block windows draw this one menu and export
//! through these functions, so a note, a chart and a telemetry window all behave alike.

use eframe::egui::{self, FontId, RichText, Ui};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;

/// An entry of the block menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAction {
    Copy,
    Pin,
    Maximize,
    ExportMarkdown,
    ExportJson,
    Close,
}

impl BlockAction {
    pub const ALL: [BlockAction; 6] = [BlockAction::Copy, BlockAction::Pin, BlockAction::Maximize, BlockAction::ExportMarkdown, BlockAction::ExportJson, BlockAction::Close];

    /// The entry's text for a block in this state.
    pub fn label(self, pinned: bool, maximized: bool) -> &'static str {
        match self {
            BlockAction::Copy => "COPY CONTENT",
            BlockAction::Pin if pinned => "UNPIN BLOCK",
            BlockAction::Pin => "PIN BLOCK",
            BlockAction::Maximize if maximized => "RESTORE SIZE",
            BlockAction::Maximize => "MAXIMIZE",
            BlockAction::ExportMarkdown => "EXPORT TO MARKDOWN",
            BlockAction::ExportJson => "EXPORT TO JSON",
            BlockAction::Close => "CLOSE",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            BlockAction::Copy => "The block's text goes to the clipboard.",
            BlockAction::Pin => "A pinned block stays through \"close the windows\" and a new report, and keeps its place when the others are tidied.",
            BlockAction::Maximize => "Fills the board; again to put it back.",
            BlockAction::ExportMarkdown => "Saves the block as a Markdown file in Documents\\JAI Exports.",
            BlockAction::ExportJson => "Saves the block as a JSON file in Documents\\JAI Exports.",
            BlockAction::Close => "Closes the block.",
        }
    }
}

/// The entries inside an open context menu; the one Sir clicked (the menu closes on it).
pub fn menu(ui: &mut Ui, pinned: bool, maximized: bool) -> Option<BlockAction> {
    let mut chosen = None;
    ui.set_min_width(196.0);
    for action in BlockAction::ALL {
        if crate::holo_ui::item(ui, action.label(pinned, maximized), false).on_hover_text(action.hint()).clicked() {
            chosen = Some(action);
            ui.close_menu();
        }
    }
    chosen
}

/// The last text a block menu copied (the verifier reads it back; the clipboard itself belongs
/// to Sir and is never read for a check).
static LAST_COPIED: Mutex<Option<String>> = Mutex::new(None);

/// `Copy Content`: `text` to the clipboard through the window's own clipboard path.
pub fn copy(ctx: &egui::Context, text: &str) {
    ctx.copy_text(text.to_string());
    *LAST_COPIED.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.to_string());
}

pub fn last_copied() -> Option<String> {
    LAST_COPIED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The file format of an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Json,
}

/// The block as a Markdown document.
pub fn markdown(title: &str, text: &str, exported_at: &str) -> String {
    format!("# {}\n\n{}\n\n---\nExported from J.A.I. on {exported_at}\n", title.trim(), text.trim())
}

/// The block as a JSON document (`detail`: the block's own structure, when it has one).
pub fn json_document(title: &str, text: &str, detail: Option<&Value>, exported_at: &str) -> String {
    serde_json::to_string_pretty(&json!({ "title": title.trim(), "exported_at": exported_at, "content": text.trim(), "block": detail })).unwrap_or_default()
}

/// Where exports go instead of Sir's documents (a verification run points this at a scratch
/// folder).
static EXPORT_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_export_dir(dir: Option<PathBuf>) {
    *EXPORT_DIR.lock().unwrap_or_else(|e| e.into_inner()) = dir;
}

pub fn export_dir() -> PathBuf {
    EXPORT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| crate::features::user_folder("Documents").join("JAI Exports"))
}

/// Writes the block to `export_dir()` as `<title>_<time>.md` / `.json`; the file's path.
pub fn export(title: &str, text: &str, detail: Option<&Value>, format: Format) -> Result<PathBuf, String> {
    let dir = export_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let now = chrono::Local::now();
    let (body, extension) = match format {
        Format::Markdown => (markdown(title, text, &now.format("%Y-%m-%d %H:%M").to_string()), "md"),
        Format::Json => (json_document(title, text, detail, &now.format("%Y-%m-%dT%H:%M:%S").to_string()), "json"),
    };
    let name = crate::features::safe_name(title);
    // A second export within the same second gets its own file.
    let mut path = dir.join(format!("{name}_{}.{extension}", now.format("%Y%m%d_%H%M%S")));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{name}_{}_{n}.{extension}", now.format("%Y%m%d_%H%M%S")));
        n += 1;
    }
    std::fs::write(&path, body).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    tracing::info!("[BLOCK] Exported '{title}' to {}", path.display());
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_has_the_six_entries_and_names_the_state_it_would_change() {
        let labels: Vec<&str> = BlockAction::ALL.iter().map(|a| a.label(false, false)).collect();
        assert_eq!(labels, ["COPY CONTENT", "PIN BLOCK", "MAXIMIZE", "EXPORT TO MARKDOWN", "EXPORT TO JSON", "CLOSE"]);
        assert_eq!(BlockAction::Pin.label(true, false), "UNPIN BLOCK");
        assert_eq!(BlockAction::Maximize.label(false, true), "RESTORE SIZE");
        assert!(BlockAction::ALL.iter().all(|a| !a.hint().is_empty() && a.label(true, true).is_ascii()));
    }

    #[test]
    fn a_block_exports_as_markdown_and_as_json_without_overwriting() {
        let dir = std::env::temp_dir().join(format!("jai_export_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_export_dir(Some(dir.clone()));
        let detail = json!({ "type": "note_stack", "pages": 2 });
        let md = export("NOTE // 01 - Ohm's law?", "U = R * I\nVoltage is current times resistance.", None, Format::Markdown).expect("markdown");
        let again = export("NOTE // 01 - Ohm's law?", "U = R * I", None, Format::Markdown).expect("a second file");
        let js = export("NOTE // 01 - Ohm's law?", "U = R * I", Some(&detail), Format::Json).expect("json");
        set_export_dir(None);
        assert_ne!(md, again, "the second export has its own file");
        assert_eq!(md.extension().and_then(|e| e.to_str()), Some("md"));
        let text = std::fs::read_to_string(&md).unwrap();
        assert!(text.starts_with("# NOTE // 01 - Ohm's law?\n\nU = R * I\nVoltage is current times resistance.\n"), "{text}");
        assert!(text.contains("Exported from J.A.I. on "));
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&js).unwrap()).expect("valid JSON");
        assert_eq!(parsed["title"], "NOTE // 01 - Ohm's law?");
        assert_eq!(parsed["content"], "U = R * I");
        assert_eq!(parsed["block"]["pages"], 2);
        // The file name holds nothing a file system refuses.
        let name = md.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.contains(['/', '\\', '?', ':', '*', '"', '<', '>', '|']), "{name}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
