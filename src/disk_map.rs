//! DISK MAP ("what is taking up space?", "map my D drive"): a drive scanned live into a treemap
//! on the board, with what fills it by file type and the largest folders.
//! (Scaffold: the scan, the treemap and the summary come with the disk map's build.)

use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};

static OPEN: AtomicBool = AtomicBool::new(false);

/// The disk map is up (a block on the board, or its own window).
pub fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// Closes the disk map (its `[ CLOSE ]`, "close the disk map").
pub fn close() {
    OPEN.store(false, Ordering::Release);
}

/// `open_jarvis_view {"view": "disk_map", "path": …, "action": …}`: opens the map and starts a
/// scan. The result is the display text and a "Spoken:" line.
pub async fn run(args: &Value) -> Result<String, String> {
    let _ = args;
    OPEN.store(true, Ordering::Release);
    crate::gui::show();
    crate::gui::refresh();
    Ok("Disk map open.\nSpoken: The disk map is up, Sir.".into())
}
