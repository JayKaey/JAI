//! His global keys: they work in every program and in games. The microphone on and off (no key
//! until Sir gives it one), his windows to the front, the console, and waking him from standby.
//! Kept in `jai_config.json` (`hotkeys`; the wake keys stay `vram_guard.hotkeys`), changed in the
//! Hotkeys window (`gui_hotkeys.rs`) by pressing the new key. On Windows they are registered on
//! the `jai-desk` thread (`win32_desktop`), on KDE with KGlobalAccel (System Settings › Shortcuts
//! shows them too). A scripted session, and a unit test, takes no key of Sir's keyboard.

use crate::config::{ConfigStore, JaiConfig};
use crate::tray::TrayAction;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};

/// The keys kept in `jai_config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeySettings {
    /// The microphone on and off ("": no key, as it starts).
    pub mute: String,
    /// His windows to the front, with their list.
    pub windows: String,
    /// The console to type to him.
    pub console: String,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self { mute: String::new(), windows: "Ctrl+Alt+J".into(), console: "Ctrl+Alt+K".into() }
    }
}

/// What one of his keys does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Mute,
    Windows,
    Console,
    Wake,
}

impl Act {
    pub const ALL: [Act; 4] = [Act::Mute, Act::Windows, Act::Console, Act::Wake];

    /// What it does, as the window and KDE's shortcut settings say it.
    pub fn name(self) -> &'static str {
        match self {
            Act::Mute => "Microphone on / off",
            Act::Windows => "His windows to the front",
            Act::Console => "The console to type to him",
            Act::Wake => "Wake him from standby",
        }
    }

    /// Its action with KDE (the names its keys were registered by before this table).
    #[cfg(not(windows))]
    fn action(self) -> &'static str {
        match self {
            Act::Mute => "mute",
            Act::Windows => "windows",
            Act::Console => "console",
            Act::Wake => "wake-0",
        }
    }
}

/// Modifiers, numbered as Windows numbers them (here on every system).
pub const ALT: u32 = 0x1;
pub const CTRL: u32 = 0x2;
pub const SHIFT: u32 = 0x4;
pub const WIN: u32 = 0x8;
/// Escape: ends the wait for a new key.
pub const ESCAPE: u32 = 0x1B;

/// The keys by name that are no letter, digit, `Num0`-`Num9` or `F1`-`F24`: (name, virtual key).
const NAMED: [(&str, u32); 9] = [("Space", 0x20), ("Enter", 0x0D), ("Insert", 0x2D), ("Home", 0x24), ("End", 0x23), ("PageUp", 0x21), ("PageDown", 0x22), ("Pause", 0x13), ("ScrollLock", 0x91)];

fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A key's virtual key by its name, in any case: "M", "7", "F9", "Num5", "Pause".
pub fn vk_of(name: &str) -> Option<u32> {
    let up = name.trim().to_ascii_uppercase();
    if let [c] = up.as_bytes() {
        return c.is_ascii_alphanumeric().then_some(u32::from(*c));
    }
    if let Some(n) = up.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=24).contains(n)) {
        return Some(0x6F + n);
    }
    if let Some(n) = up.strip_prefix("NUM").and_then(|n| n.parse::<u32>().ok()).filter(|n| *n <= 9) {
        return Some(0x60 + n);
    }
    let up = if up == "RETURN" { "ENTER".to_string() } else { up };
    NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(&up)).map(|(_, vk)| *vk)
}

/// "Ctrl+Alt+M" → (modifiers, virtual key); None for a name it does not know.
pub fn parse(spec: &str) -> Option<(u32, u32)> {
    let mut mods = 0;
    let mut key = None;
    for part in spec.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "alt" => mods |= ALT,
            "ctrl" | "control" | "strg" => mods |= CTRL,
            "shift" => mods |= SHIFT,
            "win" | "super" | "meta" => mods |= WIN,
            name => key = Some(vk_of(name)?),
        }
    }
    key.map(|k| (mods, k))
}

/// How a key is kept and shown: "Ctrl+Alt+M" (None: a key that cannot be one of his).
pub fn spec(mods: u32, vk: u32) -> Option<String> {
    let name = match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x60..=0x69 => format!("Num{}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => NAMED.iter().find(|(_, k)| *k == vk)?.0.to_string(),
    };
    let held = [(CTRL, "Ctrl"), (ALT, "Alt"), (SHIFT, "Shift"), (WIN, "Win")].into_iter().filter(|(bit, _)| mods & bit != 0).map(|(_, n)| n);
    Some(held.chain([name.as_str()]).collect::<Vec<_>>().join("+"))
}

/// The keys `act` has, as kept.
pub fn keys_of(c: &JaiConfig, act: Act) -> Vec<String> {
    let kept = match act {
        Act::Mute => std::slice::from_ref(&c.hotkeys.mute),
        Act::Windows => std::slice::from_ref(&c.hotkeys.windows),
        Act::Console => std::slice::from_ref(&c.hotkeys.console),
        Act::Wake => c.vram_guard.hotkeys.as_slice(),
    };
    kept.iter().map(|k| k.trim()).filter(|k| !k.is_empty()).map(String::from).collect()
}

/// Whether Sir may give `act` the key (modifiers, virtual key): its name, or why not. A letter, a
/// digit, Space and the like need Ctrl, Alt or Win (alone, or with Shift, they are typing);
/// F1-F24, Pause and Scroll Lock may stand alone. A key another of his has is refused.
pub fn check(c: &JaiConfig, act: Act, mods: u32, vk: u32) -> Result<String, String> {
    let spec = spec(mods, vk).ok_or("That key cannot be one of his: take a letter, a digit or an F key with Ctrl or Alt.")?;
    if mods & (CTRL | ALT | WIN) == 0 && !matches!(vk, 0x70..=0x87 | 0x13 | 0x91) {
        return Err(format!("{spec} is for typing: hold Ctrl or Alt with it."));
    }
    match Act::ALL.into_iter().find(|&other| other != act && keys_of(c, other).iter().any(|k| parse(k) == Some((mods, vk)))) {
        Some(other) => Err(format!("{spec} is already his key for \"{}\".", other.name())),
        None => Ok(spec),
    }
}

/// Where the microphone key's press goes: the tray's own way to the app.
static TRAY: OnceLock<UnboundedSender<TrayAction>> = OnceLock::new();
/// The settings his keys are read from.
static CONFIG: OnceLock<ConfigStore> = OnceLock::new();
/// The Hotkeys window waits for a new key: his keys are let go meanwhile.
static PAUSED: AtomicBool = AtomicBool::new(false);
/// What each registered key does, in the order registered (its id is `BASE` + that).
#[cfg(windows)]
static REGISTERED: Mutex<Vec<Act>> = Mutex::new(Vec::new());
/// The keys another program holds, so they do nothing.
static TAKEN: Mutex<Vec<Act>> = Mutex::new(Vec::new());

/// Takes his keys (none in a scripted session); on Windows on the thread the desktop's mouse
/// hook lives on, which this starts.
pub fn start(config: ConfigStore, tray: UnboundedSender<TrayAction>) {
    let _ = CONFIG.set(config);
    let _ = TRAY.set(tray);
    #[cfg(not(windows))]
    if !crate::platform::hands_off() {
        register_kde(true);
        let pressed_action = |action: &str| {
            if let Some(act) = Act::ALL.into_iter().find(|act| act.action() == action) {
                pressed(act);
            }
        };
        if let Err(e) = crate::kde::listen_global_keys(pressed_action) {
            warn!("[KEYS] KDE tells me of none of my keys: {e}");
        }
    }
    #[cfg(windows)]
    crate::win32_desktop::start(crate::wallpaper::on_mouse, pressed_id, register);
}

/// Gives `act` the key `key` ("": none) and takes his keys anew.
pub fn set(config: &ConfigStore, act: Act, key: &str) {
    config.update(|c| match act {
        Act::Mute => c.hotkeys.mute = key.to_string(),
        Act::Windows => c.hotkeys.windows = key.to_string(),
        Act::Console => c.hotkeys.console = key.to_string(),
        Act::Wake => c.vram_guard.hotkeys = if key.is_empty() { Vec::new() } else { vec![key.to_string()] },
    });
    info!("[KEYS] {}: {}", act.name(), if key.is_empty() { "no key" } else { key });
    #[cfg(windows)]
    crate::win32_desktop::keys_again();
    #[cfg(not(windows))]
    register_kde(false);
}

/// His keys are let go while the Hotkeys window waits for a new one (so one of them can be
/// pressed there), and taken again after.
pub fn pause(on: bool) {
    if PAUSED.swap(on, Ordering::AcqRel) == on {
        return;
    }
    #[cfg(not(windows))]
    if CONFIG.get().is_some() && !crate::platform::hands_off() {
        if let Err(e) = crate::kde::block_global_keys(on) {
            warn!("[KEYS] KDE would not {} the global keys: {e}", if on { "hold back" } else { "give back" });
        }
    }
    #[cfg(windows)]
    crate::win32_desktop::keys_again();
}

/// The keys another program holds.
pub fn taken() -> Vec<Act> {
    lock(&TAKEN).clone()
}

/// The first key `act` has now (None: none, or his keys are not read yet).
pub fn first(act: Act) -> Option<String> {
    CONFIG.get()?.read(|c| keys_of(c, act).into_iter().next())
}

/// Every key of his as kept: what it does and its name.
fn table() -> Vec<(Act, String)> {
    let Some(config) = CONFIG.get() else { return Vec::new() };
    config.read(|c| Act::ALL.into_iter().flat_map(|act| keys_of(c, act).into_iter().map(move |key| (act, key))).collect())
}

/// One of his keys was pressed.
fn pressed(act: Act) {
    info!("[KEYS] {}", act.name());
    match act {
        Act::Mute => {
            if let Some(tray) = TRAY.get() {
                let _ = tray.send(TrayAction::ToggleMic);
            }
        }
        Act::Windows => {
            crate::gui::windows_on_call();
        }
        Act::Console => crate::gui::set_text_console(true),
        // (Off this thread: waking loads the models, and on Windows the mouse hook lives here.)
        Act::Wake => {
            let _ = std::thread::Builder::new().name("jai-wake".into()).spawn(|| crate::vram_guard::wake("hotkey"));
        }
    }
}

/// The first id of his keys on the `jai-desk` thread.
#[cfg(windows)]
const BASE: i32 = 0x4B00;

/// One of his keys was pressed (its id as registered).
#[cfg(windows)]
fn pressed_id(id: usize) {
    let act = (id as i32).checked_sub(BASE).and_then(|i| usize::try_from(i).ok()).and_then(|i| lock(&REGISTERED).get(i).copied());
    if let Some(act) = act {
        pressed(act);
    }
}

/// Registers his keys on the `jai-desk` thread, where WM_HOTKEY then arrives, in place of the
/// ones before: none while the Hotkeys window waits for a new key, in a scripted session or a test.
#[cfg(windows)]
fn register() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
    let mut registered = lock(&REGISTERED);
    for i in 0..registered.len() as i32 {
        // SAFETY: a key this thread registered (or tried to) under that id.
        let _ = unsafe { UnregisterHotKey(HWND(0), BASE + i) };
    }
    registered.clear();
    let table = if PAUSED.load(Ordering::Acquire) || crate::platform::hands_off() { Vec::new() } else { table() };
    let mut taken = Vec::new();
    for (act, key) in table {
        let Some((mods, vk)) = parse(&key) else {
            warn!("[KEYS] '{key}' is no key I know ({})", act.name());
            continue;
        };
        // SAFETY: a key of this thread's (no window); its id is unique on it.
        if let Err(e) = unsafe { RegisterHotKey(HWND(0), BASE + registered.len() as i32, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk) } {
            warn!("[KEYS] {key} ({}) is taken by another program ({})", act.name(), e.message());
            taken.push(act);
        } else {
            info!("[KEYS] {key}: {}", act.name());
        }
        registered.push(act);
    }
    *lock(&TAKEN) = taken;
}

/// Gives each of his actions its keys with KDE (`first`: at start, where a key Sir chose in KDE's
/// own settings stays his); none in a scripted session or a test.
#[cfg(not(windows))]
fn register_kde(first: bool) {
    if CONFIG.get().is_none() || crate::platform::hands_off() {
        return;
    }
    let table = table();
    // (IsDefault, then SetPresent; to change one, SetPresent | NoAutoloading.)
    let flags: &[u32] = if first { &[8, 2] } else { &[6] };
    let mut taken = Vec::new();
    for act in Act::ALL {
        let codes: Vec<i32> = table.iter().filter(|(a, _)| *a == act).filter_map(|(_, key)| crate::kde::qt_key(key)).collect();
        match crate::kde::set_global_key(act.action(), act.name(), &codes, flags) {
            Ok(true) => {}
            Ok(false) => taken.push(act),
            Err(e) => {
                warn!("[KEYS] KDE took no key for {}: {e}", act.name());
                taken.push(act);
            }
        }
    }
    *lock(&TAKEN) = taken;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hotkeys() {
        assert_eq!(parse("Alt+J"), Some((ALT, 'J' as u32)));
        assert_eq!(parse("ctrl + space"), Some((CTRL, 0x20)));
        assert_eq!(parse("Ctrl+Shift+F9"), Some((CTRL | SHIFT, 0x78)));
        assert_eq!(parse("Strg+Alt+return"), Some((CTRL | ALT, 0x0D)));
        assert_eq!(parse("Pause"), Some((0, 0x13)));
        assert_eq!(parse("Ctrl+Num5"), Some((CTRL, 0x65)));
        assert_eq!(parse("win+pageup"), Some((WIN, 0x21)));
        for wrong in ["Alt+Banana", "Ctrl+", "", "F25", "Num10", "Alt+F0"] {
            assert_eq!(parse(wrong), None, "{wrong}");
        }
    }

    /// A key Sir presses is kept under a name that reads back as that key.
    #[test]
    fn a_pressed_key_is_kept_by_its_name() {
        for (mods, vk, name) in [
            (CTRL | ALT, 0x4D, "Ctrl+Alt+M"),
            (0, 0x78, "F9"),
            (SHIFT | CTRL, 0x31, "Ctrl+Shift+1"),
            (ALT, 0x63, "Alt+Num3"),
            (0, 0x91, "ScrollLock"),
            (WIN | ALT, 0x22, "Alt+Win+PageDown"),
            (0, 0x87, "F24"),
        ] {
            assert_eq!(spec(mods, vk).as_deref(), Some(name));
            assert_eq!(parse(name), Some((mods, vk)), "{name}");
        }
        // Escape, Tab, an arrow, a mouse button: no key of his.
        for vk in [ESCAPE, 0x09, 0x25, 0x01, 0] {
            assert_eq!(spec(CTRL, vk), None, "{vk:#x}");
        }
    }

    #[test]
    fn his_keys_as_they_start_and_as_sir_changes_them() {
        let mut c = JaiConfig::default();
        let all = |c: &JaiConfig| Act::ALL.map(|act| keys_of(c, act));
        assert_eq!(all(&c), [vec![], vec!["Ctrl+Alt+J".to_string()], vec!["Ctrl+Alt+K".to_string()], vec!["Alt+J".to_string()]]);
        // Typing is refused, a key alone only where it types nothing.
        assert!(check(&c, Act::Mute, 0, 0x4D).unwrap_err().contains("hold Ctrl or Alt"));
        assert!(check(&c, Act::Mute, SHIFT, 0x4D).is_err());
        assert!(check(&c, Act::Mute, 0, 0x20).is_err());
        assert_eq!(check(&c, Act::Mute, 0, 0x78).as_deref(), Ok("F9"));
        assert_eq!(check(&c, Act::Mute, 0, 0x13).as_deref(), Ok("Pause"));
        assert_eq!(check(&c, Act::Mute, CTRL | ALT, 0x4D).as_deref(), Ok("Ctrl+Alt+M"));
        assert_eq!(check(&c, Act::Mute, CTRL | SHIFT, 0x4D).as_deref(), Ok("Ctrl+Shift+M"));
        // A key another of his has, however it was written.
        c.hotkeys.windows = "ctrl + alt + j".into();
        let taken = check(&c, Act::Mute, CTRL | ALT, 0x4A).unwrap_err();
        assert!(taken.contains("His windows to the front"), "{taken}");
        // Its own key again is no clash.
        assert_eq!(check(&c, Act::Windows, CTRL | ALT, 0x4A).as_deref(), Ok("Ctrl+Alt+J"));
        assert!(check(&c, Act::Mute, CTRL, ESCAPE).is_err());
        // Several wake keys of an older config are all his; blanks are none.
        c.vram_guard.hotkeys = vec!["Alt+J".into(), " ".into(), "F10".into()];
        assert_eq!(keys_of(&c, Act::Wake), ["Alt+J", "F10"]);
        assert!(check(&c, Act::Mute, 0, 0x79).is_err(), "F10 wakes him");
    }

    /// The orders for the window, all the way through the router; the shortcut sheets of other
    /// programs and the microphone itself stay what they were.
    #[test]
    fn the_window_is_asked_for_in_his_ways() {
        let window = Some(("configure_interface".to_string(), serde_json::json!({"hotkeys_window": true})));
        for said in ["open the hotkeys window", "Change my mute key.", "Zeig mir deine Tastenkürzel", "öffne das Hotkey-Fenster", "JAI, show me your hot keys", "open the hotkey settings"] {
            assert_eq!(crate::phrasings::route(said), window, "{said}");
        }
        for said in ["what is the shortcut for a screenshot", "mute my microphone", "close the hotkeys window"] {
            assert_ne!(crate::phrasings::route(said), window, "{said}");
        }
        // The program in front's own shortcuts stay its sheet, in either language.
        for said in ["hotkeys", "show me the hotkeys", "show me the shortcuts", "zeig mir die Tastenkürzel"] {
            assert_eq!(crate::phrasings::route(said).map(|(tool, _)| tool).as_deref(), Some("show_app_shortcuts"), "{said}");
        }
        // Closed as his window, never as a program of that name (it was close_application, forced).
        for said in ["close the hotkeys window", "close the hotkeys", "schließ das Hotkey-Fenster"] {
            assert!(crate::phrasings::route(said).is_some_and(|(tool, args)| tool == "hud_board" && args["action"] == "close" && args["target"] == "hotkeys"), "{said}: {:?}", crate::phrasings::route(said));
        }
    }

    /// SET and CLEAR in the window: what is kept in the settings.
    #[test]
    fn set_and_clear_keep_the_key_in_the_settings() {
        let store = ConfigStore::new(JaiConfig::default());
        set(&store, Act::Mute, "Ctrl+Alt+M");
        set(&store, Act::Wake, "F10");
        set(&store, Act::Console, "");
        let c = store.snapshot();
        assert_eq!((c.hotkeys.mute.as_str(), c.vram_guard.hotkeys.as_slice(), c.hotkeys.console.as_str()), ("Ctrl+Alt+M", ["F10".to_string()].as_slice(), ""));
        set(&store, Act::Wake, "");
        assert!(keys_of(&store.snapshot(), Act::Wake).is_empty());
        // An older settings file without the section starts with the defaults.
        let old: JaiConfig = serde_json::from_str("{}").expect("an empty settings file");
        assert_eq!(old.hotkeys, HotkeySettings::default());
    }
}
