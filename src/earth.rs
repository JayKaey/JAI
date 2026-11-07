//! THE EARTH ("show me the earth", "switch to satellite view", "earthquakes in this area"): a
//! holographic globe on the board with live earthquakes, storms, a satellite and flights.
//! (Scaffold: the globe, its layers and its data come with the Earth's build.)

use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};

static OPEN: AtomicBool = AtomicBool::new(false);

/// The Earth is up (a block on the board, or its own window).
pub fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// Closes the Earth (its `[ CLOSE ]`, "close the earth").
pub fn close() {
    OPEN.store(false, Ordering::Release);
}

/// `open_jarvis_view {"view": "earth", "action": …, "place": …, "radius_km": …}`: opens the
/// globe or acts on it. The result is the display text and a "Spoken:" line.
pub async fn run(args: &Value) -> Result<String, String> {
    let _ = args;
    OPEN.store(true, Ordering::Release);
    crate::gui::show();
    crate::gui::refresh();
    Ok("The Earth is open.\nSpoken: The Earth is on your display, Sir.".into())
}
