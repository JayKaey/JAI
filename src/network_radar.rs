//! NETWORK RADAR ("check the network", "who is on my network?"): the home network swept and
//! drawn as a radar on the board, every device placed by how fast it answers.
//! (Scaffold: the sweep, the device list and the latency chart come with the radar's build.)

use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};

static OPEN: AtomicBool = AtomicBool::new(false);

/// The radar is up (a block on the board, or its own window).
pub fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// Closes the radar (its `[ CLOSE ]`, "close the network radar").
pub fn close() {
    OPEN.store(false, Ordering::Release);
}

/// `open_jarvis_view {"view": "network_radar", "action": …}`: opens the radar and starts a
/// sweep. The result is the display text and a "Spoken:" line.
pub async fn run(args: &Value) -> Result<String, String> {
    let _ = args;
    OPEN.store(true, Ordering::Release);
    crate::gui::show();
    crate::gui::refresh();
    Ok("Network radar open.\nSpoken: The network radar is up, Sir.".into())
}
