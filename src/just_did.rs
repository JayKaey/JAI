//! "It" is what he just did (Sir, 2026-11-13).
//!
//! "Show me your live desktop background", and a moment later "throw it on all monitors": the
//! language model stretched the board across the monitors, because nothing told it what "it"
//! was. For about a minute after Sir switches something of the interface on or off, a sentence
//! that points at it means that thing (`follow_up`):
//!
//! * after the live background was switched on, "put it on all monitors", "on every monitor",
//!   "auf allen Monitoren" put the background there, and "only on one monitor" brings it back;
//! * "turn it off again", "mach das wieder aus" after a switch is the opposite switch
//!   ("switch it back" and "undo that" were the undo's already and stay it: `undo`).
//!
//! Only what is certain: the switch must be the last thing he had done (`note` forgets it
//! with the next order that changes anything), no block may have been opened or moved since
//! (then "it" is that block, as it always was: `hud_dock::touch`), and a question changes
//! nothing. With nothing recent behind it, an "it" stays what it was.

use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid phrase")
}

/// How long "it" means what he just switched.
pub const MEANT_FOR: Duration = Duration::from_secs(75);

/// The switch of the interface he flipped last: its argument, how it stands, and when.
#[derive(Debug, Clone, PartialEq)]
struct Switched {
    key: String,
    on: bool,
    at: Instant,
}

#[cfg(not(test))]
static LAST: std::sync::Mutex<Option<Switched>> = std::sync::Mutex::new(None);
#[cfg(test)]
thread_local! {
    /// (A test has what "it" is to itself: other tests ask the fast path about "it" meanwhile.)
    static LAST: std::cell::RefCell<Option<Switched>> = const { std::cell::RefCell::new(None) };
}

fn last<R>(f: impl FnOnce(&mut Option<Switched>) -> R) -> R {
    #[cfg(not(test))]
    {
        f(&mut LAST.lock().unwrap_or_else(|e| e.into_inner()))
    }
    #[cfg(test)]
    {
        LAST.with(|last| f(&mut last.borrow_mut()))
    }
}

/// A call went through that changed something (`ToolContext::execute`): a switch of the
/// interface is what "it" means from now on; anything else is something else, and "it" no
/// longer means the switch before it.
pub fn note(tool: &str, args: &Value) {
    let switch = (tool == "configure_interface").then(|| args.as_object()).flatten().filter(|o| o.len() == 1).and_then(|o| o.iter().next()).and_then(|(key, value)| Some((key.as_str(), value.as_bool()?)));
    let now = match switch {
        // (Where the background shows is said of the background: it stays what "it" is.)
        Some(("background_all_monitors", _)) => Some(Switched { key: "desktop_background".to_string(), on: true, at: Instant::now() }),
        Some((key, on)) => Some(Switched { key: key.to_string(), on, at: Instant::now() }),
        None => None,
    };
    last(|last| *last = now);
}

/// The switch "it" means now: fresh, and nothing of his own opened or moved since.
fn meant() -> Option<Switched> {
    let last = last(|last| last.clone())?;
    let age = last.at.elapsed();
    let a_block_since = crate::hud_dock::latest_block().is_some_and(|(_, touched)| touched < age);
    (age < MEANT_FOR && !a_block_since).then_some(last)
}

const SCREENS: &str = r"(?:monitors?|screens?|displays?)";
const ALL: &str = r"(?:all|every|each|both)(?: of)?(?: my| the| your)?(?: (?:two|three|four|\d))?";
const ONE: &str = r"(?:only |just )?(?:one|a single|the main|my main|the first|my first|the primary|my primary)";
const BILDSCHIRME: &str = r"(?:monitore|monitoren|bildschirme|bildschirmen|monitor|bildschirm)";
const ALLE: &str = r"(?:alle|allen|jeden|jedem|beide|beiden)(?: (?:zwei|drei|vier|\d))?";
const BACKGROUND: &str = r"(?:the |your |my |that |this )?(?:live )?(?:desktop )?(?:background|backdrop|wallpaper)";
const HINTERGRUND: &str = r"(?:den |deinen |meinen |das |dein )?(?:live ?)?(?:desktop ?)?(?:hintergrund|hintergrundbild)";
const PUT: &str = r"(?:put|throw|show|move|spread|stretch|have|display|extend|get|bring|set|keep)";
const PACK: &str = r"(?:pack|setz|setze|zeig|zeige|leg|lege|schieb|schiebe|mach|stell|stelle|wirf|tu|lass)";

/// The live background by its name on all monitors, or on one: said in so many words, it
/// needs nothing before it.
static BACKGROUND_ALL: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:{PUT} )?{BACKGROUND} (?:on|onto|to|across|over|in) {ALL} {SCREENS}$|^(?:{PACK} )?{HINTERGRUND} auf {ALLE} {BILDSCHIRME}$|^{HINTERGRUND} auf {ALLE} {BILDSCHIRME} (?:zeigen|anzeigen|legen|setzen|packen)$"))
});
static BACKGROUND_ONE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:{PUT} )?{BACKGROUND} (?:back )?(?:only |just )?(?:on|onto|to) {ONE} {SCREENS}(?: only| again)?$|^(?:{PACK} )?{HINTERGRUND} (?:wieder )?(?:nur )?auf (?:nur )?(?:einem|einen|dem ersten|den ersten) (?:monitor|bildschirm)$"))
});
/// The same of "it", and without naming anything: "throw it on all monitors", "on every
/// monitor", "auf allen Monitoren".
static IT_ALL: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^{PUT} (?:it|that|this|him|yourself) (?:on|onto|to|across|over|in) {ALL} {SCREENS}$|^(?:and |also )?(?:on|onto|across|over) {ALL} {SCREENS}(?: too| as well)?$|^(?:all|every|both) {SCREENS}$|^{PACK} (?:es|ihn|das|den|dich) auf {ALLE} {BILDSCHIRME}$|^(?:und )?auf {ALLE} {BILDSCHIRME}(?: auch)?$"
    ))
});
static IT_ONE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^{PUT} (?:it|that|this|him|yourself) (?:back )?(?:only |just )?(?:on|onto|to) {ONE} {SCREENS}(?: only| again)?$|^(?:only |just )(?:on )?(?:one|a single|the main|my main) {SCREENS}$|^(?:back )?(?:on|to) (?:only |just )?(?:one|a single) {SCREENS}(?: only| again)?$|^nur (?:auf )?(?:einem|einen|dem ersten)(?: monitor| bildschirm)?$|^{PACK} (?:es|ihn|das|den|dich) (?:wieder )?(?:nur )?auf (?:nur )?(?:einen|einem) (?:monitor|bildschirm)$"
    ))
});
/// "Turn it off again", "mach das wieder aus": said of what was just switched on.
static IT_OFF: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:turn|switch|shut|take) (?:it|that|this) (?:back )?off(?: again)?$|^(?:turn|switch|shut) off (?:it|that|this)(?: again)?$|^off again$|^(?:mach|mache|schalt|schalte|stell|stelle) (?:das|es|ihn|sie|die|den) (?:wieder |bitte wieder )?(?:aus|ab|weg)$|^(?:das |es )?wieder (?:aus|ausmachen|ausschalten|abschalten)$")
});
/// "Turn it back on", "mach das wieder an": said of what was just switched off.
static IT_ON: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:turn|switch|put) (?:it|that|this) (?:back )?on(?: again)?$|^(?:turn|switch) on (?:it|that|this) again$|^(?:back )?on again$|^(?:mach|mache|schalt|schalte|stell|stelle) (?:das|es|ihn|sie|die|den) (?:wieder |bitte wieder )?(?:an|ein)$|^(?:das |es )?wieder (?:an|anmachen|einschalten|anschalten)$")
});

/// What a sentence that points at what he just did asks for: the tool and its arguments.
/// None for everything else, and for an "it" with nothing recent behind it.
pub fn follow_up(text: &str) -> Option<(&'static str, Value)> {
    // (Asked of every sentence, so the two things it can be about are looked for first: the
    // background by its name, or something he switched a moment ago.)
    let lower = text.to_lowercase();
    let named = ["background", "backdrop", "wallpaper", "hintergrund"].iter().any(|name| lower.contains(name));
    let it = meant();
    // (A plain question never changes anything: "is it on all monitors?")
    if (!named && it.is_none()) || crate::intents::only_asks(text) {
        return None;
    }
    let s = crate::intents::normalize_phrase(text).replace('-', " ");
    let interface = |args: Value| Some(("configure_interface", args));
    // (Named, it is switched on with it: "the background on all monitors" said while he is the
    // board means the background, there.)
    if named && BACKGROUND_ALL.is_match(&s) {
        return interface(json!({"desktop_background": true, "background_all_monitors": true}));
    }
    if named && BACKGROUND_ONE.is_match(&s) {
        return interface(json!({"background_all_monitors": false}));
    }
    let it = it?;
    if it.key == "desktop_background" && it.on {
        if IT_ALL.is_match(&s) {
            return interface(json!({"background_all_monitors": true}));
        }
        if IT_ONE.is_match(&s) {
            return interface(json!({"background_all_monitors": false}));
        }
    }
    if (it.on && IT_OFF.is_match(&s)) || (!it.on && IT_ON.is_match(&s)) {
        return interface(json!({ it.key.as_str(): !it.on }));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(on: bool) -> Option<(&'static str, Value)> {
        Some(("configure_interface", json!({"background_all_monitors": on})))
    }

    #[test]
    fn after_the_live_background_it_is_the_background() {
        let _dock = crate::hud_dock::test_lock();
        crate::hud_dock::begin_frame();
        crate::hud_dock::begin_frame();
        // Nothing recent behind it: "it" stays what it was (not this module's).
        for said in ["Throw it in all monitors.", "put it on all monitors", "on every monitor", "auf allen Monitoren", "only on one monitor", "turn it off again"] {
            assert_eq!(follow_up(said), None, "{said}");
        }
        // "Show me your live desktop background" switched it on a moment ago.
        note("configure_interface", &json!({"desktop_background": true}));
        for said in ["Throw it in all monitors.", "put it on all monitors", "Throw it on all three monitors", "show it on both screens", "on every monitor", "On all monitors, please.", "all monitors", "JAI, put it on every screen", "Auf allen Monitoren.", "pack es auf alle Bildschirme", "zeig dich auf allen Monitoren"] {
            assert_eq!(follow_up(said), all(true), "{said}");
        }
        for said in ["only on one monitor", "just one monitor", "put it back on one monitor", "nur auf einem", "Nur auf einem Monitor.", "pack es wieder auf einen Bildschirm"] {
            assert_eq!(follow_up(said), all(false), "{said}");
        }
        // Where it shows is said of the background: it stays what "it" is, and goes off as one.
        note("configure_interface", &json!({"background_all_monitors": true}));
        assert_eq!(follow_up("only on one monitor"), all(false));
        assert_eq!(follow_up("turn it off again"), Some(("configure_interface", json!({"desktop_background": false}))));
        // A question changes nothing, and other sentences are not its business.
        for said in ["is it on all monitors?", "why is it on every monitor?", "use all monitors", "span all monitors", "move it to monitor 2", "put it on the left", "close it", "make it bigger"] {
            assert_eq!(follow_up(said), None, "{said}");
        }
        // By its name it needs nothing before it.
        last(|last| *last = None);
        for said in ["put the live background on all monitors", "live background on every screen", "show your wallpaper on both monitors", "Hintergrund auf allen Monitoren", "pack den Live-Hintergrund auf alle Bildschirme"] {
            assert_eq!(follow_up(said), Some(("configure_interface", json!({"desktop_background": true, "background_all_monitors": true}))), "{said}");
        }
        for said in ["put the background back on one monitor", "background only on the main screen", "Hintergrund nur auf einem Monitor"] {
            assert_eq!(follow_up(said), all(false), "{said}");
        }
        for said in ["change the background to blue", "make the background darker", "what is on all monitors"] {
            assert_eq!(follow_up(said), None, "{said}");
        }
    }

    #[test]
    fn turn_it_off_again_is_the_opposite_of_the_switch_he_just_flipped() {
        let _dock = crate::hud_dock::test_lock();
        crate::hud_dock::begin_frame();
        crate::hud_dock::begin_frame();
        for (key, said) in [("night_now", "Turn it off again."), ("subtitles", "switch it off again"), ("desktop_background", "Mach das wieder aus."), ("mini_strip", "turn that off"), ("calm_hud", "schalte es wieder aus"), ("lyrics", "off again")] {
            note("configure_interface", &json!({ key: true }));
            assert_eq!(follow_up(said), Some(("configure_interface", json!({ key: false }))), "{key}: {said}");
            // (It was on: "turn it on again" says nothing certain.)
            assert_eq!(follow_up("turn it back on"), None, "{key}");
        }
        for (key, said) in [("subtitles", "turn it back on"), ("night_now", "Switch it on again."), ("lyrics", "mach das wieder an")] {
            note("configure_interface", &json!({ key: false }));
            assert_eq!(follow_up(said), Some(("configure_interface", json!({ key: true }))), "{key}: {said}");
            assert_eq!(follow_up("turn it off again"), None, "{key}");
        }
        // Something else done since: "it" is no longer the switch.
        note("configure_interface", &json!({"subtitles": true}));
        note("media_and_volume_control", &json!({"action": "volume_up"}));
        assert_eq!(follow_up("turn it off again"), None);
        // A change that is no switch (a theme, a layout) has no opposite to name.
        note("configure_interface", &json!({"theme": "arc cyan"}));
        assert_eq!(follow_up("turn it off again"), None);
        note("configure_interface", &json!({"subtitles": true, "subtitles_translate": true}));
        assert_eq!(follow_up("turn it off again"), None, "two things at once: which one is \"it\"?");
        // After about a minute "it" is no longer that.
        note("configure_interface", &json!({"subtitles": true}));
        last(|last| last.as_mut().unwrap().at = Instant::now() - MEANT_FOR - Duration::from_secs(1));
        assert_eq!(follow_up("turn it off again"), None);
        // A block of his opened or moved since: "it" is that block, as it always was.
        note("configure_interface", &json!({"desktop_background": true}));
        last(|last| last.as_mut().unwrap().at = Instant::now() - Duration::from_secs(20));
        let at = eframe::egui::Rect::from_min_size(eframe::egui::Pos2::new(40.0, 60.0), eframe::egui::vec2(460.0, 320.0));
        crate::hud_dock::publish_block("kit_notebook", "Notebook // your notes", at);
        crate::hud_dock::touch("kit_notebook");
        assert_eq!(follow_up("put it on all monitors"), None);
        assert_eq!(follow_up("turn it off again"), None);
        crate::hud_dock::forget("kit_notebook");
        crate::hud_dock::begin_frame();
        crate::hud_dock::begin_frame();
    }
}
