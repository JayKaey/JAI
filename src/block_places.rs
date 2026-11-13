//! Remembered places. A block of the board that reopens comes back where it stood; this keeps
//! that across restarts too. Where a block has stood at rest for a moment is written to the
//! kit's store, and the next time it opens (in any session) it is offered that place again,
//! as long as the place is still on the board and nothing else has taken it.

use eframe::egui::{pos2, vec2, Rect};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// A block has to stand this long in one place before the place counts (a glide or a drag
/// passes through many).
const SETTLED: Duration = Duration::from_millis(1500);
/// The places of this many blocks are kept (the oldest leave).
const KEPT: usize = 120;

#[derive(Default)]
struct Places {
    loaded: bool,
    /// [left, top, width, height] in board points, by block id.
    at: HashMap<String, [f32; 4]>,
    /// A place a block stands at now but has not yet stood at long enough.
    pending: HashMap<String, ([f32; 4], Instant)>,
}

static PLACES: LazyLock<Mutex<Places>> = LazyLock::new(Default::default);

fn places() -> std::sync::MutexGuard<'static, Places> {
    let mut places = PLACES.lock().unwrap_or_else(|e| e.into_inner());
    if !places.loaded {
        places.loaded = true;
        for (id, value, _) in crate::kit::list("place") {
            if let Some(rect) = four(&value) {
                places.at.insert(id, rect);
            }
        }
    }
    places
}

fn four(value: &Value) -> Option<[f32; 4]> {
    let list = value.as_array()?;
    let n = |i: usize| list.get(i).and_then(Value::as_f64).map(|v| v as f32).filter(|v| v.is_finite());
    Some([n(0)?, n(1)?, n(2)?, n(3)?])
}

fn rounded(rect: Rect) -> [f32; 4] {
    [rect.min.x.round(), rect.min.y.round(), rect.width().round(), rect.height().round()]
}

/// A place is still good when one screen of the board holds all of it and it has a block's size.
pub fn fits(rect: Rect, screens: &[Rect]) -> bool {
    let [min_w, min_h] = crate::hud_dock::MIN_BLOCK;
    rect.width() >= min_w - 1.0 && rect.height() >= min_h - 1.0 && screens.iter().any(|screen| screen.expand(2.0).contains_rect(rect))
}

/// Where the block stood when Sir last left it, when that place is still on the board and
/// free. (The tests place every block afresh.)
pub fn remembered(id: &str, screens: &[Rect]) -> Option<Rect> {
    if cfg!(test) {
        return None;
    }
    let [x, y, w, h] = *places().at.get(id)?;
    let rect = Rect::from_min_size(pos2(x, y), vec2(w, h));
    (fits(rect, screens) && !crate::hud_dock::collides(id, rect)).then_some(rect)
}

/// The block stands at `rect`, at rest (not moving, not held, not maximised). A place it has
/// kept for a moment is remembered.
pub fn note(id: &str, rect: Rect) {
    if cfg!(test) {
        return;
    }
    let now = rounded(rect);
    let mut places = places();
    if places.at.get(id) == Some(&now) {
        places.pending.remove(id);
        return;
    }
    match places.pending.get(id) {
        Some((was, since)) if *was == now => {
            if since.elapsed() < SETTLED {
                return;
            }
            places.pending.remove(id);
            places.at.insert(id.to_string(), now);
            drop(places);
            // (Written off the HUD's thread: a database write must never hold a frame.)
            let id = id.to_string();
            std::thread::spawn(move || {
                let _ = crate::kit::put("place", &id, &json!(now));
                let mut kept = crate::kit::list("place");
                if kept.len() > KEPT {
                    kept.sort_by(|a, b| a.2.cmp(&b.2));
                    for (old, _, _) in kept.iter().take(kept.len() - KEPT) {
                        crate::kit::remove("place", old);
                    }
                }
            });
        }
        _ => {
            places.pending.insert(id.to_string(), (now, Instant::now()));
        }
    }
}

/// "Forget where the windows were": every block is placed afresh from now on.
pub fn forget_all() -> usize {
    let mut places = places();
    places.at.clear();
    places.pending.clear();
    crate::kit::clear("place")
}

/// The kit's phrase for it.
pub fn command(s: &str) -> Option<Value> {
    static FORGET: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^(?:forget|reset|clear)\s+(?:where\s+(?:the|my|your)\s+(?:windows|blocks)\s+(?:were|are|stood|stand)|(?:the|my|your)\s+(?:window|block)\s+(?:places|positions))$|^vergiss\s+(?:die\s+|deine\s+)?(?:fensterpositionen|fensterplätze|blockpositionen)$").expect("valid regex")
    });
    FORGET.is_match(s.trim()).then(|| json!({ "action": "forget" }))
}

pub fn run(_args: &Value) -> Result<String, String> {
    let n = forget_all();
    Ok(crate::kit::say(format!("Forgot the places of {n} block{}. Each opens in a free place again.", if n == 1 { "" } else { "s" }), "Forgotten, Sir. Each block finds a free place again."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_counts_while_a_screen_of_the_board_holds_all_of_it() {
        let screens = [Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0)), Rect::from_min_size(pos2(1920.0, 0.0), vec2(1080.0, 1920.0))];
        let at = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pos2(x, y), vec2(w, h));
        assert!(fits(at(40.0, 60.0, 460.0, 420.0), &screens));
        assert!(fits(at(2000.0, 1200.0, 460.0, 420.0), &screens), "on the upright screen beside it");
        assert!(!fits(at(1700.0, 60.0, 460.0, 420.0), &screens), "across the bezel is no place");
        assert!(!fits(at(40.0, 900.0, 460.0, 420.0), &screens), "below the screen's edge");
        assert!(!fits(at(40.0, 60.0, 120.0, 90.0), &screens), "smaller than a block can be");
        // A board that moved to a smaller screen: the old place is gone, the block is placed afresh.
        assert!(!fits(at(1400.0, 700.0, 460.0, 320.0), &[Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 720.0))]));
        assert_eq!(four(&json!([40.0, 60.5, 460, 420])), Some([40.0, 60.5, 460.0, 420.0]));
        assert_eq!(four(&json!([40.0, "x", 460, 420])), None);
        assert_eq!(rounded(at(40.4, 59.6, 460.2, 419.9)), [40.0, 60.0, 460.0, 420.0]);
        for said in ["forget where the windows were", "reset the block positions", "vergiss die fensterpositionen"] {
            assert_eq!(command(said), Some(json!({ "action": "forget" })), "{said}");
        }
        assert_eq!(command("forget it"), None);
    }
}
