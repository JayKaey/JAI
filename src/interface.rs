//! The HUD's look and layout, which JAI changes himself when Sir asks ("make the interface
//! red", "zoom in", "stay on top", "go minimal").
//!
//! The settings live in `jai_config.json` (`interface`) and the native window re-applies
//! them live whenever they change. `configure` turns `configure_interface` tool arguments (from
//! the LLM or the instant-command fast path) into new settings plus an optional one-shot window
//! action (show, hide, move, resize).

use crate::tools::{bool_arg, str_arg};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_TITLE: &str = "J.A.I.";
const MIN_OPACITY: u8 = 15;
/// Pure orb mode: the window is a square of this many pixels.
const MIN_ORB_PX: f32 = 120.0;
const MAX_ORB_PX: f32 = 900.0;
const MIN_SCALE: f32 = 0.5;
const MAX_SCALE: f32 = 2.5;
/// Relative zoom step ("bigger", "zoom out").
const SCALE_STEP: f32 = 1.15;
/// Relative opacity step ("more transparent").
const OPACITY_STEP: i32 = 15;

/// Lower-case, single-spaced, without leading articles/possessives.
fn words(s: &str) -> String {
    const ARTICLES: &[&str] = &[
        "the", "a", "an", "your", "my", "der", "die", "das", "den", "dem", "dein", "deine", "deinen", "mein", "meine",
        "meinen", "ein", "eine", "einen",
    ];
    let lower = s.trim().to_lowercase().replace(['_', '-', ',', '!', '?', '"', '\''], " ");
    let mut parts: Vec<&str> = lower.split_whitespace().collect();
    while parts.len() > 1 && ARTICLES.contains(&parts[0]) {
        parts.remove(0);
    }
    parts.join(" ")
}

// ------------------------------------------------------------------------------ layout ----

/// Which parts of the HUD are on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum HudLayout {
    /// The whole HUD.
    #[default]
    Full,
    /// Header, reactor and conversation.
    Compact,
    /// Just the animated reactor: a small floating status orb.
    Orb,
}

impl HudLayout {
    pub fn parse(name: &str) -> Option<Self> {
        match words(name).as_str() {
            "full" | "normal" | "default" | "standard" | "complete" | "expanded" | "everything" | "all" | "voll"
            | "volle" | "vollständig" | "vollstaendig" | "normale" | "alles" => Some(Self::Full),
            "compact" | "small" | "chat" | "chat only" | "kompakt" | "kompakte" | "klein" | "kleine" | "mini hud" | "compact mini hud"
            | "minihud" | "compact hud" => Some(Self::Compact),
            "orb" | "minimal" | "mini" | "minimalist" | "minimalistic" | "reactor" | "reactor only" | "orb only"
            | "minimalistisch" | "reaktor" => Some(Self::Orb),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Compact => "compact",
            Self::Orb => "orb",
        }
    }
}

/// Lenient: an unknown name in a hand-edited config means the full layout, not a reset config.
impl From<String> for HudLayout {
    fn from(name: String) -> Self {
        Self::parse(&name).unwrap_or_default()
    }
}

impl From<HudLayout> for String {
    fn from(layout: HudLayout) -> Self {
        layout.id().to_string()
    }
}

// ---------------------------------------------------------------------------- settings ----

/// Persisted under `interface` in `jai_config.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceSettings {
    /// Interface zoom, 1.0 = 100 %.
    pub scale: f32,
    /// Background opacity in percent; text and controls stay fully opaque.
    pub opacity: u8,
    pub layout: HudLayout,
    pub always_on_top: bool,
    /// The HUD board fills the monitor (the default since config v16).
    pub fullscreen: bool,
    /// No title bar: the header (or the reactor) moves the window.
    pub frameless: bool,
    pub animations: bool,
    /// The display's finish: fine scan lines and film grain over the whole HUD, so it reads as
    /// light on glass (Sir, 2026-11-11: "more realistic, a bit more rough"). "Turn off the scan
    /// lines" / the tool's `display_finish`.
    pub display_finish: bool,
    /// Calm HUD: the header strip, the telemetry strip and the key hints step back while nothing
    /// happens and return when the mouse comes near or he has something to do ("calm mode",
    /// "show everything").
    pub calm_hud: bool,
    /// Sir's words appear under the emblem while he speaks (paused while a game runs).
    pub live_words: bool,
    /// The power-up sequence at start (short or long by how long he was away and the hour).
    pub startup_sequence: bool,
    /// Unused for some minutes, the HUD dims to a clock, the date, the next appointment and
    /// the weather.
    pub idle_screen: bool,
    /// Night mode by the clock (22:00 to 6:00): dimmer, no sounds, quieter and briefer.
    pub night_mode: bool,
    /// The screen's edges glow for a moment when something happens.
    pub edge_light: bool,
    /// The emblem moves with the music that plays on the PC.
    pub music_visuals: bool,
    /// Mini mode: a slim always-on-top strip at the top of a monitor, over the background.
    pub mini_strip: bool,
    /// As the background he covers every monitor (off: Sir's first one only). ("I want the live
    /// background to be on all monitors if I want it to".)
    pub background_all_monitors: bool,
    /// The voice lock: a spoken order is carried out only in Sir's own voice (after the voice
    /// set-up; push-to-talk and the keyboard are his hands and always work).
    pub voice_lock: bool,
    /// The dashboard on an upright monitor (`desktop_backdrop` draws it there, behind his
    /// windows): "portrait dashboard".
    pub portrait_dashboard: bool,
    /// A confirmation leaves out what Sir just said himself: "Done, Sir." instead of "Done,
    /// Sir. Discord is on Monitor 2." What he did not say (which song was picked) is still said.
    pub brief_confirmations: bool,
    /// Name shown in the header.
    pub title: String,
    /// How he addresses Sir: "Sir", a name, or "" for no title. None until asked on first use.
    pub address: Option<String>,
    /// Radius of the 3D sphere orb relative to its viewport (0.4-2.5).
    pub orb_scale: f32,
    /// Text size on top of the zoom (0.75-1.6).
    pub font_scale: f32,
    /// Pure orb mode: only the 3D orb, borderless, transparent, floating.
    pub orb_mode: bool,
    /// Side length of the orb window in pure orb mode (120-900 px).
    pub orb_size_px: f32,
    /// Opacity of the orb in pure orb mode (0.15-1.0).
    pub orb_opacity: f32,
    /// The orb stays above every other window in pure orb mode.
    pub orb_on_top: bool,
    /// Where the orb sits: "top-right", "top-left", "bottom-right", "bottom-left", "center" or
    /// "free" (wherever Sir dragged it).
    pub orb_anchor: String,
    /// Monitor of the orb (1 = primary, 2 = the second …; 0 = the HUD's current monitor).
    pub orb_monitor: u8,
    /// Which core stands on the HUD: "simple" (the instrument of rings) or "complex" (the
    /// sphere of circuitry).
    pub core_style: String,
    /// The `COMMS // LIVE RESPONSE` block above the bottom strip. Off by default, like the
    /// reference board: the strip shows what Sir said, the voice carries the answer and the text
    /// console keeps every written reply.
    pub comms_block: bool,
    /// The status pill at the top centre (LISTENING / THINKING / SPEAKING), as in the reference
    /// videos.
    pub status_pill: bool,
    /// Sir's voice as a ring of ticks round the emblem while he speaks, as on the reference orb
    /// (the emblem's own rings follow only JAI's voice).
    pub voice_rim: bool,
    /// The board's sound cues (a block materialising, the spotlight, a mode change, an alert).
    pub sound_cues: bool,
    /// The theme (`theme_store::resolve`): one of the eight presets ("stark_gold", "mono", …), a
    /// saved theme ("theme:<name>") or a light of his own colour ("custom:#RRGGBB"). Everything
    /// he draws reads it through `holo::theme()`; a colour request sets it.
    pub hud_palette: String,
    /// Where Sir put the emblem on the board (fractions of the canvas; None = the centre).
    pub orb_board_pos: Option<[f32; 2]>,
    /// "Use all monitors": the borderless HUD spans the whole virtual desktop.
    pub span_all_monitors: bool,
    /// "Overlap the windows" / "stack them": new windows cascade instead of packing side by side.
    pub allow_window_overlap: bool,
    /// As the background: what is being said stands beside the corner orb. (Sir, 2026-11-14: "I
    /// should be able to toggle if the information window next to his orb should show or not".)
    pub orb_words: bool,
    /// The corner orb's size, of its usual one (0.6-1.8): the wheel over it, "smaller orb".
    pub corner_orb_scale: f32,
    /// As the background: the clock under the emblem, with its seconds, in twelve hours, with
    /// the date under it. ("I want the desktop view to have a clock".)
    pub desk_clock: bool,
    pub desk_clock_seconds: bool,
    pub desk_clock_12h: bool,
    pub desk_clock_date: bool,
    /// As the background: the song that plays stands beside the core. (Sir, 2026-11-21: "I also
    /// want to be able to disable to now playing widget that shows next to JAIs orb".)
    pub now_playing: bool,
    /// Media on the board's black (`bg_media`): "behind" (the board over it, dimmed) or "whole"
    /// (the media alone, the board when called); the source per monitor number ("all": every
    /// monitor without its own), a picture, an H.264 MP4 or a folder of pictures; the seconds a
    /// slideshow shows each picture; the video's sound; the black over it behind the board.
    pub bg_mode: String,
    pub bg_media: std::collections::BTreeMap<String, String>,
    pub bg_interval: u32,
    pub bg_sound: bool,
    pub bg_dim: f32,
    /// Where Sir dragged that clock (fractions of his first screen, beyond 0-1 on another
    /// monitor; None: under the emblem, and with it wherever that goes).
    pub desk_clock_pos: Option<[f32; 2]>,
    /// Sir told the emblem to get out of the way: it waits in its corner until he calls it back
    /// (or moves it). Never saved: at the next start it is in the middle again.
    #[serde(skip)]
    pub orb_aside: bool,
}

impl Default for InterfaceSettings {
    fn default() -> Self {
        Self {
            scale: 1.0,
            opacity: 100,
            layout: HudLayout::Full,
            always_on_top: false,
            fullscreen: true,
            frameless: true,
            animations: true,
            display_finish: true,
            calm_hud: true,
            live_words: true,
            startup_sequence: true,
            idle_screen: true,
            night_mode: true,
            edge_light: true,
            music_visuals: true,
            mini_strip: false,
            background_all_monitors: true,
            // (On: he answers to Sir alone once Sir's voice is known, unless told otherwise.)
            voice_lock: true,
            portrait_dashboard: false,
            brief_confirmations: true,
            title: DEFAULT_TITLE.to_string(),
            address: None,
            orb_scale: 1.0,
            font_scale: 1.0,
            orb_mode: false,
            orb_size_px: 260.0,
            orb_opacity: 1.0,
            orb_on_top: true,
            orb_anchor: "bottom-right".to_string(),
            orb_monitor: 0,
            core_style: "simple".to_string(),
            comms_block: false,
            status_pill: true,
            voice_rim: true,
            sound_cues: true,
            hud_palette: crate::holo_theme::DEFAULT_PRESET.to_string(),
            orb_board_pos: None,
            span_all_monitors: false,
            allow_window_overlap: false,
            orb_words: true,
            corner_orb_scale: 1.0,
            desk_clock: true,
            desk_clock_seconds: false,
            desk_clock_12h: false,
            desk_clock_date: true,
            now_playing: true,
            bg_mode: "behind".into(),
            bg_media: Default::default(),
            bg_interval: 30,
            bg_sound: false,
            bg_dim: 0.55,
            desk_clock_pos: None,
            orb_aside: false,
        }
    }
}

impl InterfaceSettings {
    /// One board across every monitor and the fullscreen board on one monitor exclude each
    /// other: whichever of the two was just switched on (compared with `before`) wins, so F11,
    /// the SCREENS menu, a settings checkbox and JAI's own tools can never leave both set.
    pub fn reconcile(&mut self, before: &InterfaceSettings) {
        if self.span_all_monitors && self.fullscreen {
            if before.span_all_monitors && !before.fullscreen {
                self.span_all_monitors = false;
            } else {
                self.fullscreen = false;
            }
        }
    }
}

impl InterfaceSettings {
    /// v24: the stock title follows his new name, and the stock look of then (the cyan theme)
    /// becomes his yellow.
    pub fn renamed_v24(&mut self) {
        // (The stock titles of v1-v23, as fingerprints: `config::fingerprint`.)
        const STOCK_TITLES: [u64; 4] = [0x8b99_a312_d3e9_a308, 0x0fb8_900f_eed4_fcf6, 0xac2f_d99e_782e_a7e4, 0xd68c_c20b_65a0_1b44];
        if STOCK_TITLES.contains(&crate::config::fingerprint(self.title.trim())) {
            self.title = DEFAULT_TITLE.to_string();
        }
        if self.hud_palette == "arc_cyan" {
            self.hud_palette = Self::default().hud_palette;
        }
    }

    pub fn sanitize(&mut self) {
        let defaults = Self::default();
        match crate::holo_theme::preset_name(&self.hud_palette) {
            // (A preset under a name of the old engine, "jai_gold", is kept under its own from
            // now on: whatever compares the setting with a preset's name finds it.)
            Some(preset) => self.hud_palette = preset.to_string(),
            None => {
                let own = self.hud_palette.strip_prefix("custom:#").is_some_and(|h| h.len() == 6 && u32::from_str_radix(h, 16).is_ok())
                    || self.hud_palette.strip_prefix(crate::holo_theme::CUSTOM_PREFIX).is_some_and(|n| crate::theme_store::custom(n).is_some());
                if !own {
                    self.hud_palette = defaults.hud_palette.clone();
                }
            }
        }
        self.scale = if self.scale.is_finite() { (self.scale * 100.0).round() / 100.0 } else { 1.0 };
        self.scale = self.scale.clamp(MIN_SCALE, MAX_SCALE);
        self.opacity = self.opacity.clamp(MIN_OPACITY, 100);
        let round = |x: f32, lo: f32, hi: f32| if x.is_finite() { ((x * 100.0).round() / 100.0).clamp(lo, hi) } else { 1.0 };
        self.orb_scale = round(self.orb_scale, MIN_ORB_SCALE, MAX_ORB_SCALE);
        self.corner_orb_scale = round(self.corner_orb_scale, 0.6, 1.8);
        self.font_scale = round(self.font_scale, MIN_FONT_SCALE, MAX_FONT_SCALE);
        self.orb_size_px = if self.orb_size_px.is_finite() { self.orb_size_px.round().clamp(MIN_ORB_PX, MAX_ORB_PX) } else { 260.0 };
        self.orb_opacity = if self.orb_opacity.is_finite() { ((self.orb_opacity * 100.0).round() / 100.0).clamp(0.15, 1.0) } else { 1.0 };
        self.orb_anchor = anchor_id(&self.orb_anchor).unwrap_or("bottom-right").to_string();
        self.orb_monitor = self.orb_monitor.min(9);
        if self.bg_mode != "whole" {
            self.bg_mode = defaults.bg_mode.clone();
        }
        self.bg_interval = self.bg_interval.clamp(3, 3600);
        self.bg_dim = if self.bg_dim.is_finite() { ((self.bg_dim * 100.0).round() / 100.0).clamp(0.0, 0.9) } else { defaults.bg_dim };
        self.title = self.title.trim().chars().take(32).collect();
        if self.title.is_empty() {
            self.title = defaults.title;
        }
    }

    pub fn describe(&self) -> String {
        let on_off = |b: bool| if b { "on" } else { "off" };
        format!(
            "theme {}, zoom {:.0} %, window opacity {} %, layout {}, always on top {}, fullscreen {}, frameless {}, animations {}, \
             title \"{}\", orb size {:.0} %, font size {:.0} %, pure orb mode {} ({:.0} px, {}, monitor {}, opacity {:.0} %)",
            self.hud_palette,
            self.scale * 100.0,
            self.opacity,
            self.layout.id(),
            on_off(self.always_on_top),
            on_off(self.fullscreen),
            on_off(self.frameless),
            on_off(self.animations),
            self.title,
            self.orb_scale * 100.0,
            self.font_scale * 100.0,
            on_off(self.orb_mode),
            self.orb_size_px,
            self.orb_anchor,
            if self.orb_monitor == 0 { "current".to_string() } else { self.orb_monitor.to_string() },
            self.orb_opacity * 100.0,
        )
    }
}

// ----------------------------------------------------------------------- live layout ----

const MIN_ORB_SCALE: f32 = 0.4;
const MAX_ORB_SCALE: f32 = 2.5;
const MIN_FONT_SCALE: f32 = 0.75;
const MAX_FONT_SCALE: f32 = 1.6;
/// Relative step for "make the orb bigger" / "bigger text".
const LAYOUT_STEP: f32 = 1.2;

/// The HUD window's size in logical points as it last drew itself; (0, 0) while it is closed.
static WINDOW_SIZE: std::sync::Mutex<(f32, f32)> = std::sync::Mutex::new((0.0, 0.0));

/// The HUD reports how large its window is (every frame).
pub fn publish_window_size(size: (f32, f32)) {
    *WINDOW_SIZE.lock().unwrap_or_else(|e| e.into_inner()) = size;
}

fn live_window_size() -> (f32, f32) {
    *WINDOW_SIZE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The result of `control_jai_interface`.
pub struct Controlled {
    pub settings: InterfaceSettings,
    /// A new inner size for the HUD window (logical points).
    pub window_size: Option<(f32, f32)>,
    pub summary: String,
}

fn scale_arg(args: &Value) -> Option<f32> {
    let v = args.get("scale_value").or_else(|| args.get("scale")).or_else(|| args.get("value")).filter(|v| !v.is_null())?;
    let (x, relative) = number_like(v)?;
    // 150 → 150 %, 1.5 → 150 %.
    let factor = if x.abs() > 3.0 { x / 100.0 } else { x };
    Some(if relative { 1.0 + factor } else { factor })
}

/// Applies `control_jai_interface(action, scale_value)` to `current`: the core's size, the text
/// size and the window's size.
pub fn control(current: &InterfaceSettings, args: &Value) -> Result<Controlled, String> {
    let action = str_arg(args, &["action", "command"]).unwrap_or("status").to_lowercase().replace([' ', '-'], "_");
    let mut s = current.clone();
    let mut window_size = None;
    let done = match action.as_str() {
        // (While the orb is out in its corner, it is the one Sir sees and means.)
        "orb_bigger" | "orb_larger" | "bigger_orb" | "orb_smaller" | "smaller_orb" if crate::wallpaper::orb_out() => {
            s.corner_orb_scale = corner_orb_scale(s.corner_orb_scale, if action.contains("smaller") { "smaller" } else { "bigger" });
            "corner orb resized".to_string()
        }
        "orb_bigger" | "orb_larger" | "bigger_orb" => {
            s.orb_scale *= LAYOUT_STEP;
            "orb enlarged".to_string()
        }
        "orb_smaller" | "smaller_orb" => {
            s.orb_scale /= LAYOUT_STEP;
            "orb reduced".to_string()
        }
        "set_orb_scale" | "orb_size" | "orb_scale" => {
            s.orb_scale = scale_arg(args).ok_or("set_orb_scale needs scale_value (0.4-2.5, or a percentage).")?;
            "orb resized".to_string()
        }
        "font_bigger" | "text_bigger" | "bigger_text" => {
            s.font_scale *= 1.1;
            "text enlarged".to_string()
        }
        "font_smaller" | "text_smaller" | "smaller_text" => {
            s.font_scale /= 1.1;
            "text reduced".to_string()
        }
        "set_font_scale" | "font_scale" | "font_size" => {
            s.font_scale = scale_arg(args).ok_or("set_font_scale needs scale_value (0.8-1.6, or a percentage).")?;
            "text resized".to_string()
        }
        "set_window_size" | "window_size" | "resize_window" => {
            let (w, h) = live_window_size();
            let width = args.get("width").and_then(number_like).map(|(x, _)| x);
            let height = args.get("height").and_then(number_like).map(|(x, _)| x);
            let size = match (width, height, scale_arg(args)) {
                (Some(width), Some(height), _) => (width, height),
                (Some(width), None, _) if w > 0.0 => (width, width * h / w),
                (_, _, Some(factor)) if w > 0.0 => (w * factor, h * factor),
                _ => return Err("set_window_size needs width and height (or scale_value relative to the current size).".into()),
            };
            let size = (size.0.clamp(240.0, 7680.0).round(), size.1.clamp(240.0, 4320.0).round());
            window_size = Some(size);
            format!("window resized to {:.0} × {:.0}", size.0, size.1)
        }
        "status" | "describe" | "state" => "no change".to_string(),
        other => return Err(format!("Unknown action '{other}' (orb_bigger, orb_smaller, set_orb_scale, font_bigger, font_smaller, set_font_scale, set_window_size, status).")),
    };
    s.sanitize();
    let (width, height) = window_size.unwrap_or_else(live_window_size);
    let summary = format!("Interface layout: {done}. Now: orb size {:.0} %, font size {:.0} %, window {width:.0} × {height:.0}.", s.orb_scale * 100.0, s.font_scale * 100.0);
    Ok(Controlled { settings: s, window_size, summary })
}

/// In-character confirmation of `control_jai_interface`, from the request and the settings it
/// produced.
pub fn control_ack(args: &Value, now: &InterfaceSettings) -> String {
    let action = str_arg(args, &["action", "command"]).unwrap_or("status").to_lowercase().replace([' ', '-'], "_");
    match action.as_str() {
        "orb_bigger" | "orb_larger" | "bigger_orb" | "orb_smaller" | "smaller_orb" if crate::wallpaper::orb_out() => {
            format!("Done, Sir. The corner orb is at {:.0} percent.", now.corner_orb_scale * 100.0)
        }
        "orb_bigger" | "orb_larger" | "bigger_orb" | "orb_smaller" | "smaller_orb" | "set_orb_scale" | "orb_size" | "orb_scale" => {
            format!("Done, Sir. The orb is at {:.0} percent.", now.orb_scale * 100.0)
        }
        "font_bigger" | "text_bigger" | "bigger_text" | "font_smaller" | "text_smaller" | "smaller_text" | "set_font_scale" | "font_scale" | "font_size" => {
            format!("Is this better, Sir? Text at {:.0} percent.", now.font_scale * 100.0)
        }
        "set_window_size" | "window_size" | "resize_window" => "Done, Sir. The window is resized.".into(),
        _ => "Everything is as you see it, Sir.".into(),
    }
}

// ------------------------------------------------------------ spoken layout commands ----

static ORB_SIZE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\b(orb|sphere|reactor|globe|kugel|3d core)\b.*?\b(bigger|larger|smaller|tinier|grow|shrink|größer|groesser|kleiner|(\d{2,3})\s*(?:%|percent|prozent))|\b(bigger|larger|smaller|enlarge|shrink|grow|vergrößere|verkleinere)\s+(?:the\s+|my\s+|your\s+|die\s+|den\s+)?(?:3d\s+)?(orb|sphere|reactor|globe|kugel)\b").expect("valid layout regex")
});

/// Spoken layout commands on the fast path (`s` = the intent parser's normalised sentence):
/// `control_jai_interface` arguments for the orb's size ("make the orb bigger", "the orb at
/// 150 percent").
pub fn layout_command(s: &str) -> Option<Value> {
    let c = ORB_SIZE.captures(s)?;
    if let Some(pct) = c.get(3).and_then(|m| m.as_str().parse::<f32>().ok()) {
        return Some(json!({ "action": "set_orb_scale", "scale_value": pct / 100.0 }));
    }
    let word = c.get(2).or_else(|| c.get(4)).map(|m| m.as_str()).unwrap_or("");
    let bigger = matches!(word, "bigger" | "larger" | "grow" | "größer" | "groesser" | "enlarge" | "vergrößere");
    Some(json!({ "action": if bigger { "orb_bigger" } else { "orb_smaller" } }))
}

/// The `control_jai_interface` tool schema.
pub fn control_tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "control_jai_interface",
        "description": "Resize parts of YOUR OWN HUD live: the orb (\"make the orb bigger\"), the text or the window.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["orb_bigger", "orb_smaller", "set_orb_scale", "font_bigger", "font_smaller", "set_font_scale", "set_window_size", "status"]},
            "scale_value": {"type": "number", "description": "Orb 0.4-2.5, font 0.8-1.6, window factor; or a percentage"},
            "width": {"type": "number"}, "height": {"type": "number"}
        }, "required": ["action"]}
    }})
}

// ----------------------------------------------------------------------------- colours ----

const COLOURS: &[(&str, [u8; 3])] = &[
    ("red", [255, 59, 59]),
    ("rot", [255, 59, 59]),
    ("crimson", [220, 20, 60]),
    ("scarlet", [255, 36, 0]),
    ("ruby", [224, 17, 95]),
    ("orange", [255, 140, 26]),
    ("amber", [255, 176, 32]),
    ("bernstein", [255, 176, 32]),
    ("gold", [255, 194, 51]),
    ("golden", [255, 194, 51]),
    ("yellow", [255, 225, 77]),
    ("gelb", [255, 225, 77]),
    ("lime", [182, 255, 59]),
    ("green", [43, 255, 136]),
    ("grün", [43, 255, 136]),
    ("gruen", [43, 255, 136]),
    ("emerald", [16, 217, 138]),
    ("smaragd", [16, 217, 138]),
    ("mint", [125, 255, 203]),
    ("olive", [160, 170, 60]),
    ("teal", [20, 184, 166]),
    ("petrol", [14, 154, 167]),
    ("cyan", [0, 229, 255]),
    ("aqua", [0, 229, 255]),
    ("turquoise", [46, 230, 214]),
    ("türkis", [46, 230, 214]),
    ("tuerkis", [46, 230, 214]),
    ("blue", [59, 130, 246]),
    ("blau", [59, 130, 246]),
    ("sky blue", [94, 200, 255]),
    ("himmelblau", [94, 200, 255]),
    ("navy", [30, 58, 138]),
    ("indigo", [99, 102, 241]),
    ("cobalt", [0, 71, 171]),
    ("purple", [168, 85, 247]),
    ("lila", [168, 85, 247]),
    ("violet", [167, 139, 250]),
    ("violett", [167, 139, 250]),
    ("lavender", [190, 170, 255]),
    ("lavendel", [190, 170, 255]),
    ("magenta", [255, 61, 242]),
    ("pink", [255, 95, 176]),
    ("rosa", [255, 143, 200]),
    ("white", [242, 247, 255]),
    ("weiß", [242, 247, 255]),
    ("weiss", [242, 247, 255]),
    ("silver", [192, 200, 210]),
    ("silber", [192, 200, 210]),
    ("grey", [138, 153, 168]),
    ("gray", [138, 153, 168]),
    ("grau", [138, 153, 168]),
    ("black", [0, 0, 0]),
    ("schwarz", [0, 0, 0]),
    ("brown", [160, 82, 45]),
    ("braun", [160, 82, 45]),
    ("beige", [232, 216, 176]),
];

const WHITE: [u8; 3] = [255, 255, 255];

pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim();
    let digits = s.strip_prefix('#').unwrap_or(s);
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    match digits.len() {
        6 => Some([byte(0)?, byte(2)?, byte(4)?]),
        3 if s.starts_with('#') => {
            let nibble = |i: usize| u8::from_str_radix(&digits[i..i + 1], 16).ok().map(|v| v * 17);
            Some([nibble(0)?, nibble(1)?, nibble(2)?])
        }
        _ => None,
    }
}

/// "rgb(255, 0, 80)" or "255, 0, 80".
fn parse_rgb_triplet(s: &str) -> Option<[u8; 3]> {
    let inner = s.trim().trim_start_matches("rgb").trim_matches(|c| c == '(' || c == ')' || c == ' ');
    let parts: Vec<u8> = inner
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    <[u8; 3]>::try_from(parts).ok()
}

pub fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Approximate relative luminance, 0 (black) to 1 (white).
pub fn luminance([r, g, b]: [u8; 3]) -> f32 {
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

pub fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

fn scale_rgb(c: [u8; 3], f: f32) -> [u8; 3] {
    c.map(|v| (v as f32 * f).round().clamp(0.0, 255.0) as u8)
}

fn named_colour(name: &str) -> Option<[u8; 3]> {
    let lookup = |n: &str| COLOURS.iter().find(|(k, _)| *k == n).map(|(_, rgb)| *rgb);
    lookup(name)
        .or_else(|| lookup(&name.replace(' ', "")))
        // German adjective endings: "rote", "blaues", "grünen".
        .or_else(|| ["en", "es", "er", "em", "e"].iter().find_map(|s| name.strip_suffix(s).and_then(lookup)))
}

#[derive(Clone, Copy)]
enum Shade {
    Dark,
    Light,
}

/// "dunkelblau" → (dark, "blau"); filler words ("colour", "neon") dropped.
fn split_shade(input: &str) -> (Option<Shade>, String) {
    const DARK: &[&str] = &["dark ", "deep ", "dunkel ", "dunkle ", "dunkles ", "dunkler ", "dunkel"];
    const LIGHT: &[&str] = &["light ", "bright ", "pale ", "pastel ", "hell ", "helle ", "helles ", "heller ", "hell"];
    let n = words(input)
        .split_whitespace()
        .filter(|w| !matches!(*w, "colour" | "color" | "farbe" | "farbig" | "neon" | "shade" | "of" | "ish"))
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(base) = DARK.iter().find_map(|p| n.strip_prefix(p)) {
        return (Some(Shade::Dark), base.trim().to_string());
    }
    if let Some(base) = LIGHT.iter().find_map(|p| n.strip_prefix(p)) {
        return (Some(Shade::Light), base.trim().to_string());
    }
    (None, n)
}

/// Colour names in English or German ("dark blue", "hellgrün", "rote"), `#RRGGBB` or `rgb(…)`.
pub fn parse_color(input: &str) -> Option<[u8; 3]> {
    if let Some(rgb) = parse_hex(input).or_else(|| parse_rgb_triplet(input)) {
        return Some(rgb);
    }
    let (shade, base) = split_shade(input);
    let rgb = named_colour(&base)?;
    Some(match shade {
        Some(Shade::Dark) => scale_rgb(rgb, 0.55),
        Some(Shade::Light) => mix(rgb, WHITE, 0.45),
        None => rgb,
    })
}

/// The colour as JAI says it: always English ("dunkelblau" → "dark blue").
fn colour_word(input: &str) -> String {
    if parse_hex(input).is_some() || parse_rgb_triplet(input).is_some() {
        return "your custom colour".to_string();
    }
    let (shade, base) = split_shade(input);
    // English names come first in COLOURS, so the first entry with the same value is English.
    let english = named_colour(&base)
        .and_then(|rgb| COLOURS.iter().find(|(_, c)| *c == rgb).map(|(name, _)| name.to_string()))
        .unwrap_or(base);
    match shade {
        Some(Shade::Dark) => format!("dark {english}"),
        Some(Shade::Light) => format!("light {english}"),
        None => english,
    }
}

// ------------------------------------------------------------------------------ window ----

/// One-shot actions on the HUD window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCommand {
    Show,
    Hide,
    Minimize,
    Maximize,
    Restore,
    Center,
    Bigger,
    Smaller,
    Place(Corner),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// Left half of the screen.
    Left,
    /// Right half of the screen.
    Right,
}

impl WindowCommand {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match words(name).as_str() {
            "show" | "open" | "focus" | "front" | "bring to front" | "appear" | "zeigen" | "anzeigen" | "öffnen"
            | "oeffnen" => Self::Show,
            "hide" | "tray" | "close" | "background" | "verstecken" | "ausblenden" | "schließen" | "schliessen" => {
                Self::Hide
            }
            "minimize" | "minimise" | "minimieren" => Self::Minimize,
            "maximize" | "maximise" | "maximieren" => Self::Maximize,
            "restore" | "normal" | "wiederherstellen" => Self::Restore,
            "center" | "centre" | "middle" | "mitte" | "zentrieren" => Self::Center,
            "bigger" | "larger" | "grow" | "größer" | "groesser" => Self::Bigger,
            "smaller" | "shrink" | "kleiner" => Self::Smaller,
            other => Self::Place(Corner::parse(other)?),
        })
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Show => "shown",
            Self::Hide => "hidden in the tray",
            Self::Minimize => "minimised",
            Self::Maximize => "maximised",
            Self::Restore => "restored",
            Self::Center => "centred",
            Self::Bigger => "enlarged",
            Self::Smaller => "made smaller",
            Self::Place(_) => "moved",
        }
    }
}

impl Corner {
    pub fn parse(name: &str) -> Option<Self> {
        let n = words(name);
        let n = [" corner", " side", " half", " ecke", " seite", " hälfte"]
            .iter()
            .find_map(|s| n.strip_suffix(s))
            .unwrap_or(&n);
        Some(match n {
            "top left" | "upper left" | "oben links" | "links oben" => Self::TopLeft,
            "top right" | "upper right" | "oben rechts" | "rechts oben" => Self::TopRight,
            "bottom left" | "lower left" | "unten links" | "links unten" => Self::BottomLeft,
            "bottom right" | "lower right" | "unten rechts" | "rechts unten" => Self::BottomRight,
            "left" | "links" => Self::Left,
            "right" | "rechts" => Self::Right,
            _ => return None,
        })
    }
}

// --------------------------------------------------------------------------- configure ----

pub struct Configured {
    pub settings: InterfaceSettings,
    /// One-shot window action (show, hide, move, resize).
    pub window: Option<WindowCommand>,
    /// What changed, for the tool result the LLM phrases its answer from.
    pub summary: String,
}

/// Number, "+15"/"-15", "120%", or a word like "bigger"/"kleiner".
fn number_like(v: &Value) -> Option<(f32, bool)> {
    match v {
        Value::Number(n) => n.as_f64().map(|x| (x as f32, false)),
        Value::String(s) => {
            let t = s.trim().trim_end_matches('%').trim();
            let t = t.trim_end_matches("percent").trim_end_matches("prozent").trim();
            let relative = t.starts_with('+') || t.starts_with('-');
            t.trim_start_matches('+').parse::<f32>().ok().map(|x| (x, relative))
        }
        _ => None,
    }
}

fn scale_value(v: &Value, current: f32) -> Option<f32> {
    if let Value::String(s) = v {
        let rel = match words(s).as_str() {
            "bigger" | "larger" | "big" | "large" | "zoom in" | "increase" | "up" | "more" | "größer" | "groesser"
            | "grösser" => Some(current * SCALE_STEP),
            "smaller" | "small" | "zoom out" | "decrease" | "down" | "less" | "kleiner" => Some(current / SCALE_STEP),
            "much bigger" | "huge" | "riesig" => Some(current * SCALE_STEP * SCALE_STEP),
            "much smaller" | "tiny" | "winzig" => Some(current / SCALE_STEP / SCALE_STEP),
            "normal" | "default" | "reset" | "standard" | "normal size" => Some(1.0),
            _ => None,
        };
        if rel.is_some() {
            return rel;
        }
    }
    let (x, relative) = number_like(v)?;
    // 120 → 120 %, 1.2 → 120 %.
    let as_factor = |x: f32| if x.abs() > 3.0 { x / 100.0 } else { x };
    Some(if relative { current + as_factor(x) } else { as_factor(x) })
}

fn opacity_value(v: &Value, current: u8) -> Option<i32> {
    if let Value::String(s) = v {
        let named = match words(s).as_str() {
            "opaque" | "solid" | "full" | "none" | "undurchsichtig" | "deckend" => Some(100),
            "transparent" | "see through" | "translucent" | "durchsichtig" => Some(75),
            "more transparent" | "less opaque" | "transparenter" | "durchsichtiger" => {
                Some(current as i32 - OPACITY_STEP)
            }
            "less transparent" | "more opaque" | "weniger transparent" | "weniger durchsichtig" => {
                Some(current as i32 + OPACITY_STEP)
            }
            _ => None,
        };
        if named.is_some() {
            return named;
        }
    }
    let (x, relative) = number_like(v)?;
    // 0.8 → 80 %.
    let pct = if x > 0.0 && x <= 1.0 && !relative { x * 100.0 } else { x };
    Some(if relative { current as i32 + pct.round() as i32 } else { pct.round() as i32 })
}

/// Applies `configure_interface` arguments to `current`. Fields that cannot be understood are
/// reported back instead of failing the whole call; nothing understood at all is an error.
/// A saved theme on the whole HUD: palette, accent and background tint.
fn apply_custom_theme(s: &mut InterfaceSettings, t: &crate::theme_store::CustomTheme, done: &mut Vec<String>, verb: &str) {
    s.hud_palette = t.key();
    done.push(format!("theme '{}' {verb}", t.name));
}

/// What a colour request stores. Light is drawn on black glass, so a colour too dark to be a
/// light ("black") is the black and white theme.
fn light_of(word: &str, rgb: [u8; 3]) -> String {
    if luminance(rgb) < 0.08 {
        "mono".to_string()
    } else {
        crate::theme_store::name_for(word, rgb)
    }
}

pub fn configure(current: &InterfaceSettings, args: &Value) -> Result<Configured, String> {
    let reset = bool_arg(args, "reset") == Some(true);
    let mut s = if reset { InterfaceSettings::default() } else { current.clone() };
    let mut done: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    if reset {
        done.push("reset to the default look".into());
    }

    if let Some(raw) = str_arg(args, &["core_style", "core", "core_look"]) {
        match crate::holo_core::CoreStyle::of_word(raw) {
            Some(style) => {
                s.core_style = style.name().to_string();
                done.push(format!("the {} core", style.name()));
            }
            None => failed.push(format!("unknown core '{raw}' (simple, complex)")),
        }
    }
    // A theme by its name or a word for it ("iron man", "black and white"), a layout said as a
    // theme ("minimal"), or a colour of his own.
    if let Some(name) = str_arg(args, &["theme", "preset", "style"]) {
        if let Some(preset) = crate::holo_theme::preset_for_word(&words(name)) {
            s.hud_palette = preset.to_string();
            done.push(format!("theme {}", crate::holo_theme::label(preset)));
        } else if let Some(layout) = HudLayout::parse(name) {
            s.layout = layout;
            done.push(format!("layout {}", layout.id()));
        } else if let Some(rgb) = parse_color(name) {
            s.hud_palette = light_of(name, rgb);
            done.push(format!("accent colour {name}"));
        } else {
            failed.push(format!("unknown theme '{name}' (themes: {})", crate::holo_theme::PRESETS.map(|(name, _, _)| name).join(", ")));
        }
    }
    if let Some(c) = str_arg(args, &["accent_color", "accent_colour", "accent", "color", "colour", "highlight_color"]) {
        match parse_color(c) {
            Some(rgb) => {
                s.hud_palette = light_of(c, rgb);
                done.push(format!("accent colour {c}"));
            }
            None => failed.push(format!("unknown colour '{c}'")),
        }
    }
    if let Some(v) = ["scale", "zoom", "ui_scale"].iter().find_map(|k| args.get(*k).filter(|v| !v.is_null())) {
        match scale_value(v, s.scale) {
            Some(x) => {
                s.scale = x.clamp(MIN_SCALE, MAX_SCALE);
                done.push(format!("zoom {:.0} %", s.scale * 100.0));
            }
            None => failed.push(format!("unclear zoom {v}")),
        }
    }
    for (keys, field, label, lo, hi) in [
        (&["font_scale", "text_size", "ui_font_scale"][..], &mut s.font_scale, "font size", MIN_FONT_SCALE, MAX_FONT_SCALE),
        (&["orb_scale", "orb_size", "orb_radius_scale"][..], &mut s.orb_scale, "orb size", MIN_ORB_SCALE, MAX_ORB_SCALE),
    ] {
        if let Some(v) = keys.iter().find_map(|k| args.get(*k).filter(|v| !v.is_null())) {
            match scale_value(v, *field) {
                Some(x) => {
                    *field = x.clamp(lo, hi);
                    done.push(format!("{label} {:.0} %", *field * 100.0));
                }
                None => failed.push(format!("unclear {label} {v}")),
            }
        }
    }
    let opacity = match (args.get("opacity").filter(|v| !v.is_null()), args.get("transparency").filter(|v| !v.is_null())) {
        (Some(v), _) => Some(opacity_value(v, s.opacity).ok_or(v)),
        // Transparency 30 % = opacity 70 %.
        (None, Some(v)) => Some(number_like(v).map(|(x, _)| 100 - if x <= 1.0 { (x * 100.0) as i32 } else { x as i32 }).ok_or(v)),
        _ => None,
    };
    match opacity {
        Some(Ok(pct)) => {
            s.opacity = pct.clamp(MIN_OPACITY as i32, 100) as u8;
            done.push(format!("background opacity {} %", s.opacity));
        }
        Some(Err(v)) => failed.push(format!("unclear opacity {v}")),
        None => {}
    }
    // "compact": true is the Mini-HUD; false goes back to every panel.
    let compact = bool_arg(args, "compact").map(|on| if on { "compact" } else { "full" });
    if let Some(name) = str_arg(args, &["layout", "mode", "view"]).or(compact) {
        if let Some(layout) = HudLayout::parse(name) {
            s.layout = layout;
            done.push(format!("layout {}", layout.id()));
        } else if let Some(preset) = crate::holo_theme::preset_for_word(&words(name)) {
            s.hud_palette = preset.to_string();
            done.push(format!("theme {}", crate::holo_theme::label(preset)));
        } else {
            failed.push(format!("unknown layout '{name}' (full, compact or orb)"));
        }
    }
    let flags = [
        ("always on top", bool_arg(args, "always_on_top").or_else(|| bool_arg(args, "on_top")), &mut s.always_on_top),
        ("fullscreen", bool_arg(args, "fullscreen"), &mut s.fullscreen),
        (
            "frameless",
            bool_arg(args, "frameless")
                .or_else(|| bool_arg(args, "borderless"))
                .or_else(|| bool_arg(args, "decorations").map(|b| !b))
                .or_else(|| bool_arg(args, "title_bar").map(|b| !b)),
            &mut s.frameless,
        ),
        ("animations", bool_arg(args, "animations"), &mut s.animations),
        ("display finish", bool_arg(args, "display_finish").or_else(|| bool_arg(args, "scan_lines")).or_else(|| bool_arg(args, "grain")), &mut s.display_finish),
        ("sound cues", bool_arg(args, "sound_cues"), &mut s.sound_cues),
        ("conversation block", bool_arg(args, "comms_block"), &mut s.comms_block),
        ("calm HUD", bool_arg(args, "calm_hud"), &mut s.calm_hud),
        ("live words", bool_arg(args, "live_words"), &mut s.live_words),
        ("start-up sequence", bool_arg(args, "startup_sequence"), &mut s.startup_sequence),
        ("idle screen", bool_arg(args, "idle_screen"), &mut s.idle_screen),
        ("night mode by the clock", bool_arg(args, "night_mode"), &mut s.night_mode),
        ("edge light", bool_arg(args, "edge_light"), &mut s.edge_light),
        ("music visuals", bool_arg(args, "music_visuals"), &mut s.music_visuals),
        ("mini mode", bool_arg(args, "mini_strip"), &mut s.mini_strip),
        ("background on every monitor", bool_arg(args, "background_all_monitors"), &mut s.background_all_monitors),
        ("words beside the corner orb", bool_arg(args, "orb_words"), &mut s.orb_words),
        ("desktop clock", bool_arg(args, "desk_clock"), &mut s.desk_clock),
        ("clock seconds", bool_arg(args, "desk_clock_seconds"), &mut s.desk_clock_seconds),
        ("twelve-hour clock", bool_arg(args, "desk_clock_12h"), &mut s.desk_clock_12h),
        ("clock date", bool_arg(args, "desk_clock_date"), &mut s.desk_clock_date),
        ("now playing beside the core", bool_arg(args, "now_playing"), &mut s.now_playing),
        // (Before his voice is known it holds nothing back: the print is gathered from the
        // orders given by name, and the lock holds from then on.)
        ("voice lock", bool_arg(args, "voice_lock"), &mut s.voice_lock),
    ];
    // Guests ("guest mode", "let my friend speak for half an hour"): a request of the moment,
    // carried out by `hud_feel::apply`.
    if let Some(on) = bool_arg(args, "voice_guest") {
        done.push(format!("guest mode {}", if on { "on" } else { "off" }));
    }
    for (label, value, field) in flags {
        if let Some(on) = value {
            *field = on;
            done.push(format!("{label} {}", if on { "on" } else { "off" }));
        }
    }
    crate::bg_media::apply(&mut s, args, &mut done, &mut failed);
    // "Smaller orb", "bigger orb", "orb size normal": the corner orb, a fifth at a time.
    if let Some(step) = str_arg(args, &["corner_orb_size"]) {
        s.corner_orb_scale = corner_orb_scale(s.corner_orb_scale, step);
        done.push(format!("corner orb at {:.0} % of its size", s.corner_orb_scale * 100.0));
    }
    // (Any setting of the clock but its hiding shows it: "show seconds" with no clock is no answer.)
    if ["desk_clock_seconds", "desk_clock_12h", "desk_clock_date"].iter().any(|key| bool_arg(args, key).is_some()) && bool_arg(args, "desk_clock").is_none() {
        s.desk_clock = true;
    }
    if let Some(title) = str_arg(args, &["title", "display_name", "name"]) {
        s.title = title.to_string();
        done.push(format!("title \"{title}\""));
    }
    if bool_arg(args, "open_settings") == Some(true) {
        done.push("settings window open".into());
    }
    // The requests of the moment (subtitles, lyrics, tonight's night mode, the voice set-up):
    // carried out by `hud_feel::apply`.
    for (key, label) in crate::hud_feel::RUNTIME_KEYS {
        if let Some(on) = bool_arg(args, key) {
            done.push(format!("{label} {}", if on { "on" } else { "off" }));
        }
    }
    if let Some(open) = bool_arg(args, "text_console") {
        done.push(format!("text console {}", if open { "open" } else { "closed" }));
    }
    if let Some(open) = bool_arg(args, "core_view") {
        done.push(format!("core view {}", if open { "open" } else { "closed" }));
    }
    if let Some(open) = bool_arg(args, "feature_lab") {
        done.push(format!("Feature Lab {}", if open { "open" } else { "closed" }));
    }
    if let Some(open) = bool_arg(args, "theme_studio") {
        done.push(format!("Theme Studio {}", if open { "open" } else { "closed" }));
    }
    if let Some(open) = bool_arg(args, "mixer_view") {
        done.push(format!("Mixer {}", if open { "open" } else { "closed" }));
    }
    if let Some(cmd) = core_command(args) {
        done.push(format!("core {cmd:?}"));
    }
    if let Some(name) = str_arg(args, &["hud_palette", "palette", "hud_theme"]) {
        match crate::holo_theme::preset_name(name) {
            Some(preset) => {
                s.hud_palette = preset.to_string();
                done.push(format!("HUD theme {preset}"));
            }
            None => match crate::theme_store::custom(name.trim().trim_start_matches(crate::holo_theme::CUSTOM_PREFIX)) {
                Some(t) => apply_custom_theme(&mut s, &t, &mut done, "loaded"),
                None => failed.push(format!("unknown HUD theme '{name}'")),
            },
        }
    }
    // Saved themes: "create a theme called X with …", "design a dark stealth theme", "save this
    // theme as X", "load my X theme" — on disk the moment they exist.
    if let Some(spec) = args.get("create_theme") {
        let text = |k: &str| spec.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
        let description = text("description").or(spec.as_str()).unwrap_or("");
        let colour = |k: &str| text(k).and_then(parse_color);
        let designed = crate::theme_store::design_theme(text("name"), description);
        let base = designed.or_else(|| colour("core_primary").or(colour("window_border")).map(|c| crate::theme_store::CustomTheme::from_theme(&text("name").map_or_else(|| crate::theme_store::free_name("Custom"), str::to_string), &crate::holo_theme::Theme::of_colour(c))));
        match base {
            Some(mut t) => {
                if let Some(n) = text("name") {
                    t.name = n.to_string();
                }
                t.core_primary = colour("core_primary").unwrap_or(t.core_primary);
                t.core_secondary = colour("core_secondary").unwrap_or(t.core_secondary);
                t.window_border = colour("window_border").unwrap_or(t.window_border);
                t.background_tint = colour("background_tint").unwrap_or(t.background_tint);
                match crate::theme_store::save_custom(t.clone()) {
                    Ok(()) => apply_custom_theme(&mut s, &t, &mut done, "created and saved"),
                    Err(e) => failed.push(format!("theme '{}' not saved: {e}", t.name)),
                }
            }
            None => failed.push(format!("no colours or mood in '{description}' to build a theme from")),
        }
    }
    if let Some(name) = str_arg(args, &["save_theme"]) {
        let t = crate::theme_store::CustomTheme::from_theme(name, &crate::theme_store::resolve(&s.hud_palette));
        match crate::theme_store::save_custom(t.clone()) {
            Ok(()) => apply_custom_theme(&mut s, &t, &mut done, "saved"),
            Err(e) => failed.push(format!("theme '{name}' not saved: {e}")),
        }
    }
    if let Some(name) = str_arg(args, &["load_theme"]) {
        match crate::theme_store::custom(name) {
            Some(t) => apply_custom_theme(&mut s, &t, &mut done, "loaded"),
            None => failed.push(format!("no saved theme '{name}' (saved: {})", crate::theme_store::custom_themes().iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", "))),
        }
    }
    if let Some(view) = str_arg(args, &["hud_view"]) {
        match crate::hud_dock::HudViewMode::parse(view) {
            Some(mode) => done.push(format!("view {}", mode.id())),
            None => failed.push(format!("unknown HUD view '{view}'")),
        }
    }
    // "Move your orb to the middle", "use all monitors", "overlap the windows".
    if let Err(e) = apply_board_layout(&mut s, args, &mut done) {
        failed.push(e);
    }
    // (He is always the desktop's background: asked to become it, or to leave it, he says so.)
    if bool_arg(args, "desktop_background").is_some() {
        done.push("the desktop's background, as always".to_string());
    }
    let window = match str_arg(args, &["window", "window_action"]) {
        Some(w) => match WindowCommand::parse(w) {
            Some(cmd) => {
                done.push(format!("window {}", cmd.describe()));
                Some(cmd)
            }
            None => {
                failed.push(format!("unknown window action '{w}'"));
                None
            }
        },
        None => None,
    };

    if done.is_empty() {
        return Err(if failed.is_empty() {
            "Nothing to change: pass hud_palette, accent_color, scale, opacity, layout, always_on_top, fullscreen, frameless, 
             animations, title, window or reset."
                .to_string()
        } else {
            failed.join("; ")
        });
    }
    s.reconcile(current);
    s.sanitize();
    let mut summary = format!("Interface updated: {}.", done.join(", "));
    if !failed.is_empty() {
        summary.push_str(&format!(" Not changed: {}.", failed.join("; ")));
    }
    summary.push_str(&format!(" Current interface: {}.", s.describe()));
    Ok(Configured { settings: s, window, summary })
}

/// The core sphere command in `configure_interface` arguments (`core_scale`, `core_spin`).
pub fn core_command(args: &Value) -> Option<crate::holo_mind::CoreCommand> {
    use crate::holo_mind::CoreCommand;
    if let Some(v) = args.get("core_scale").filter(|v| !v.is_null()) {
        return Some(match (v.as_f64(), v.as_str().map(str::to_lowercase).as_deref()) {
            (Some(p), _) => CoreCommand::Zoom((p as f32 / if p > 5.0 { 100.0 } else { 1.0 }).clamp(0.5, 3.0)),
            (_, Some("smaller")) => CoreCommand::ZoomBy(1.0 / 1.25),
            (_, Some(s)) => match s.trim_end_matches('%').trim().parse::<f32>() {
                Ok(p) => CoreCommand::Zoom((p / 100.0).clamp(0.5, 3.0)),
                Err(_) => CoreCommand::ZoomBy(1.25),
            },
            _ => return None,
        });
    }
    match args.get("core_spin").and_then(Value::as_str) {
        Some("faster") => Some(CoreCommand::Spin(2.0)),
        Some("slower") => Some(CoreCommand::Spin(0.5)),
        Some("stop") => Some(CoreCommand::Spin(0.0)),
        _ => None,
    }
}

/// In-character confirmation for the instant-command fast path, from the request and the
/// settings it produced.
pub fn spoken_ack(args: &Value, now: &InterfaceSettings) -> String {
    let text = |keys: &[&str]| str_arg(args, keys);
    let flag = |key: &str| bool_arg(args, key);
    if let Some(line) = crate::hud_feel::spoken(args) {
        return line;
    }
    if flag("open_settings") == Some(true) {
        return "There you are, Sir. The interface settings are open.".into();
    }
    if let Some(open) = flag("core_view") {
        return if open { "My core, Sir. Every module I run on, wired as it runs. Drag to orbit it." } else { "Core view hidden, Sir." }.into();
    }
    if let Some(open) = flag("feature_lab") {
        return if open { "The Feature Lab, Sir. Everything I can do, each with a button to try it." } else { "Feature Lab closed, Sir." }.into();
    }
    if let Some(open) = flag("theme_studio") {
        return if open { "The Theme Studio, Sir. Pick your colours, name it and save it." } else { "Theme Studio closed, Sir." }.into();
    }
    if let Some(open) = flag("mixer_view") {
        return if open { "The mixer, Sir. A fader for every channel." } else { "Mixer closed, Sir." }.into();
    }
    if let Some(preset) = text(&["hud_palette", "palette", "hud_theme"]).and_then(crate::holo_theme::preset_name) {
        return format!("{} it is, Sir.", spoken_theme(preset));
    }
    if args.get("span_all_monitors").is_some_and(|v| !v.is_null()) {
        return if now.background_all_monitors { "On every monitor, Sir." } else { "On your first monitor only, Sir." }.into();
    }
    if let Some(spot) = text(&["orb_board_position", "emblem_position"]) {
        if now.orb_aside {
            return "Out of your way, Sir. Say the word and I come back.".into();
        }
        if matches!(orb_move(spot), Some(OrbMove::Home)) {
            return "Back in the middle, Sir.".into();
        }
    }
    if let Some(mode) = text(&["hud_view"]).and_then(crate::hud_dock::HudViewMode::parse) {
        use crate::hud_dock::HudViewMode as V;
        return match mode {
            V::Home => "Home, Sir.",
            V::Board => "The board, Sir.",
            V::CoreArchitectureSphere => "My core, Sir.",
            V::SimLab => "The Simulation Lab, Sir.",
            V::Telemetry => "Telemetry's up, Sir.",
            V::Schedule => "Your schedule, Sir.",
            V::TextConsole => "Text console's open, Sir.",
        }
        .into();
    }
    if let Some(cmd) = core_command(args) {
        return match cmd {
            crate::holo_mind::CoreCommand::Spin(f) if f <= 0.0 => "Holding it still, Sir.".into(),
            crate::holo_mind::CoreCommand::Spin(_) => "Spinning it up, Sir.".into(),
            _ => "Scaled, Sir.".into(),
        };
    }
    if let Some(open) = flag("text_console") {
        return if open { "Text console's open, Sir. Type away." } else { "Console closed, Sir." }.into();
    }
    if flag("reset") == Some(true) {
        return "Very good, Sir. The interface is back to its factory look.".into();
    }
    if let Some(style) = text(&["core_style", "core", "core_look"]).and_then(crate::holo_core::CoreStyle::of_word) {
        return format!("The {} core, Sir.", style.name());
    }
    if let Some(cmd) = text(&["window", "window_action"]).and_then(WindowCommand::parse) {
        return match cmd {
            WindowCommand::Show => "There you are, Sir.".into(),
            WindowCommand::Hide => "Very good, Sir. I'll be in the tray if you need me.".into(),
            WindowCommand::Minimize => "Done, Sir. Minimised.".into(),
            WindowCommand::Maximize => "Done, Sir. Maximised.".into(),
            _ => "Done, Sir.".into(),
        };
    }
    if let Some(name) = text(&["theme"]) {
        if let Some(preset) = crate::holo_theme::preset_for_word(&words(name)) {
            return format!("Very good, Sir. Switching to the {} theme.", spoken_theme(preset));
        }
        if let Some(layout) = HudLayout::parse(name) {
            return layout_ack(layout);
        }
    }
    if let Some(c) = text(&["accent_color", "accent", "color"]) {
        if parse_color(c).is_some_and(|rgb| luminance(rgb) < 0.08) {
            return "Black and white, Sir. Only light shows on black glass.".into();
        }
        return format!("Very good, Sir. The interface is now {}.", colour_word(c));
    }
    if let Some(name) = text(&["layout"]) {
        if let Some(layout) = HudLayout::parse(name) {
            return layout_ack(layout);
        }
    }
    if let Some(v) = args.get("scale") {
        return match v.as_str().map(words).as_deref() {
            Some("bigger" | "larger" | "zoom in" | "größer" | "groesser") => "Is this better, Sir?".into(),
            Some("smaller" | "zoom out" | "kleiner") => "Done, Sir. A little smaller.".into(),
            Some("normal" | "default" | "reset") => "Done, Sir. Back to normal size.".into(),
            _ => format!("Done, Sir. Zoom at {:.0} percent.", now.scale * 100.0),
        };
    }
    if args.get("opacity").is_some() || args.get("transparency").is_some() {
        // "Make the interface's opacity 0 %": the floor, said as such.
        let asked = match (args.get("opacity").filter(|v| !v.is_null()), args.get("transparency").filter(|v| !v.is_null())) {
            (Some(v), _) => opacity_value(v, now.opacity),
            (None, Some(v)) => number_like(v).map(|(x, _)| 100 - if x <= 1.0 { (x * 100.0) as i32 } else { x as i32 }),
            _ => None,
        };
        return if now.opacity >= 100 {
            "Done, Sir. Fully opaque again.".into()
        } else if cfg!(windows) {
            // (Windows sees through his black whole: `win32_desktop::key_black`.)
            "Done, Sir. Your wallpaper shows through behind me now.".into()
        } else if now.opacity == MIN_OPACITY && asked.is_some_and(|pct| pct < MIN_OPACITY as i32) {
            format!("Done, Sir. Background opacity at {} percent, the lowest it goes, so you can still find me.", now.opacity)
        } else {
            format!("Done, Sir. Background opacity at {} percent.", now.opacity)
        };
    }
    if let Some(on) = flag("always_on_top") {
        return if on {
            "Very good, Sir. I'll stay on top.".into()
        } else {
            "Of course, Sir. I'll stay in the background.".into()
        };
    }
    if let Some(on) = flag("fullscreen") {
        return if on { "Done, Sir. Full screen.".into() } else { "Done, Sir. Back to a window.".into() };
    }
    if let Some(on) = flag("frameless") {
        return if on { "Done, Sir. Frameless.".into() } else { "Done, Sir. Title bar restored.".into() };
    }
    if let Some(on) = flag("animations") {
        return if on { "Done, Sir. Animations back on.".into() } else { "Done, Sir. Animations off.".into() };
    }
    if let Some(on) = flag("display_finish") {
        return if on { "Done, Sir. Scan lines and grain are back.".into() } else { "Done, Sir. A clean display.".into() };
    }
    if let Some(on) = flag("sound_cues") {
        return if on { "Done, Sir. Sound cues back on.".into() } else { "Done, Sir. The board stays quiet.".into() };
    }
    if let Some(on) = flag("comms_block") {
        return if on { "The conversation, Sir, above the bottom strip.".into() } else { "Done, Sir. The conversation block is away.".into() };
    }
    if text(&["title"]).is_some() {
        return format!("Very good, Sir. On screen I am now {}.", now.title);
    }
    "Done, Sir.".into()
}

// --------------------------------------------------------------------------- orb mode ----

/// Where a spoken phrase puts the emblem on the board.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrbMove {
    /// Back to the centre ("the middle", "home", "Mitte").
    Home,
    /// A fixed spot, in fractions of the canvas ("top left", "right side", "unten mitte").
    Spot([f32; 2]),
    /// A step from where it is, in fractions of the canvas ("a little bit to the left",
    /// "further up", "etwas nach rechts").
    Step([f32; 2]),
}

/// "A little bit", "slightly", "etwas".
pub const ORB_SMALL_STEP: f32 = 0.08;
/// "Up", "more to the left" (no amount given).
pub const ORB_STEP: f32 = 0.14;
/// "A lot", "way", "far", "viel weiter".
pub const ORB_BIG_STEP: f32 = 0.26;

/// The emblem move in a place phrase, word by word, so the order and the small words do not
/// matter: "a little bit to the left", "to the top right corner of the screen", "back to the
/// middle", "further up", "ganz nach unten links". None when the phrase names anything that is
/// not a place on the board ("monitor 2", "red").
pub fn orb_move(phrase: &str) -> Option<OrbMove> {
    let lower = phrase.to_lowercase();
    // -1 = left / up, +1 = right / down.
    let (mut h, mut v_spot, mut v_step) = (0i8, 0i8, 0i8);
    let (mut centre, mut relative, mut ambiguous_lower) = (false, false, false);
    let mut amount: Option<f32> = None;
    let mut any = false;
    for w in lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        any = true;
        match w {
            // Small words that carry no place.
            "the" | "a" | "an" | "to" | "of" | "on" | "at" | "in" | "into" | "onto" | "over" | "toward" | "towards" | "my" | "your" | "screen" | "side"
            | "corner" | "hand" | "edge" | "half" | "back" | "again" | "please" | "it" | "just" | "there" | "by" | "die" | "den" | "der" | "dem" | "das"
            | "nach" | "auf" | "zur" | "zum" | "wieder" | "seite" | "ecke" | "bildschirm" | "rand" | "bitte" | "mal" | "ein" | "eine" | "einen"
            | "ganz" | "hin" | "rüber" | "rueber" | "area" | "position" | "spot" | "bereich" => {}
            "little" | "bit" | "slightly" | "tad" | "touch" | "smidge" | "smidgen" | "tiny" | "small" | "etwas" | "bisschen" | "wenig" | "leicht"
            | "stück" | "stueck" | "stückchen" | "kleines" => amount = Some(amount.map_or(ORB_SMALL_STEP, |a: f32| a.min(ORB_SMALL_STEP))),
            "lot" | "lots" | "far" | "way" | "much" | "viel" | "weit" | "deutlich" => amount = Some(amount.map_or(ORB_BIG_STEP, |a: f32| a.max(ORB_BIG_STEP))),
            "more" | "further" | "farther" | "mehr" | "weiter" | "noch" => relative = true,
            "left" | "links" | "leftward" | "leftwards" => h = -1,
            "right" | "rechts" | "rightward" | "rightwards" => h = 1,
            "up" | "upward" | "upwards" | "higher" | "hoch" | "höher" | "hoeher" | "rauf" | "hinauf" => v_step = -1,
            "down" | "downward" | "downwards" | "runter" | "tiefer" | "hinunter" | "herunter" => v_step = 1,
            "top" | "upper" | "oben" | "above" => v_spot = -1,
            "bottom" | "unten" | "below" => v_spot = 1,
            // "Lower left" is a corner, "a bit lower" a step.
            "lower" => ambiguous_lower = true,
            "center" | "centre" | "middle" | "mitte" | "home" | "default" | "zentrum" | "centered" | "centred" | "central" | "zentriert" | "mittig" => centre = true,
            _ => return None,
        }
    }
    if !any {
        return None;
    }
    if ambiguous_lower {
        if h != 0 && amount.is_none() && !relative {
            v_spot = 1;
        } else {
            v_step = 1;
        }
    }
    // An amount or "more" makes it a step; "up" / "down" alone are steps too.
    if amount.is_some() || relative || (v_step != 0 && v_spot == 0 && h == 0 && !centre) {
        let size = amount.unwrap_or(ORB_STEP);
        let v = if v_step != 0 { v_step } else { v_spot };
        if h == 0 && v == 0 {
            return None;
        }
        return Some(OrbMove::Step([h as f32 * size, v as f32 * size]));
    }
    let v = if v_spot != 0 { v_spot } else { v_step };
    if h == 0 && v == 0 {
        return centre.then_some(OrbMove::Home);
    }
    let x = match h {
        -1 => 0.14,
        1 => 0.86,
        _ => 0.5,
    };
    let y = match v {
        -1 => 0.2,
        1 => 0.8,
        _ => 0.5,
    };
    Some(OrbMove::Spot([x, y]))
}

/// Where a move leaves the emblem (None = the centre), starting from `from`.
pub fn apply_orb_move(m: OrbMove, from: Option<[f32; 2]>) -> Option<[f32; 2]> {
    match m {
        OrbMove::Home => None,
        OrbMove::Spot(at) => Some(at),
        OrbMove::Step([dx, dy]) => {
            let [x, y] = from.unwrap_or([0.5, 0.5]);
            // (On the monitor it stands on: Sir's first is 0-1, the next to the right 1-2 …)
            let cell = |v: f32| if (0.0..=1.0).contains(&v) { 0.0 } else { v.floor() };
            let (cx, cy) = (cell(x), cell(y));
            Some([(x + dx).clamp(cx + 0.08, cx + 0.92), (y + dy).clamp(cy + 0.12, cy + 0.88)])
        }
    }
}

/// "Aside", "out of the way", "zur Seite": the emblem is to leave the middle for its corner.
pub fn orb_aside_phrase(spot: &str) -> bool {
    matches!(words(spot).as_str(), "aside" | "out of the way" | "out of my way" | "away" | "to the side" | "its corner" | "docked" | "dock" | "zur seite" | "aus dem weg" | "beiseite")
}

/// "slightly left", "a little bit to the left", "etwas nach links": a step from where the emblem
/// is (`from` None = the centre). None when the phrase is no step.
pub fn nudge(name: &str, from: Option<[f32; 2]>) -> Option<Option<[f32; 2]>> {
    match orb_move(name)? {
        m @ OrbMove::Step(_) => Some(apply_orb_move(m, from)),
        _ => None,
    }
}

/// A spot on the board for the emblem ("top left", "bottom center", "right side"): fractions of
/// the canvas; `Some(None)` = back to the centre.
pub fn board_spot(name: &str) -> Option<Option<[f32; 2]>> {
    match orb_move(name)? {
        OrbMove::Step(_) => None,
        m => Some(apply_orb_move(m, None)),
    }
}

/// "top right", "Top-Right", "oben rechts", "centre" → the floating orb's anchor id.
pub fn anchor_id(name: &str) -> Option<&'static str> {
    Some(match words(name).replace(' ', "-").as_str() {
        "top-right" | "upper-right" | "oben-rechts" | "rechts-oben" | "topright" => "top-right",
        "top-left" | "upper-left" | "oben-links" | "links-oben" | "topleft" => "top-left",
        "bottom-right" | "lower-right" | "unten-rechts" | "rechts-unten" | "bottomright" => "bottom-right",
        "bottom-left" | "lower-left" | "unten-links" | "links-unten" | "bottomleft" => "bottom-left",
        "center" | "centre" | "middle" | "mitte" | "zentriert" => "center",
        "free" | "custom" | "manual" | "here" | "dragged" => "free",
        _ => return None,
    })
}

/// "2", "second", "monitor 2", "left", "primary" → the monitor number (1-based; 0 = current).
pub fn monitor_number(v: &Value) -> Option<u8> {
    if let Some(n) = v.as_u64() {
        return Some(n.min(9) as u8);
    }
    let w = words(v.as_str()?);
    let w = w.trim_start_matches("monitor ").trim_start_matches("screen ").trim_start_matches("display ").trim_end_matches(" monitor").trim_end_matches(" screen").trim();
    Some(match w {
        "1" | "one" | "first" | "primary" | "main" | "eins" | "erste" | "erster" | "haupt" => 1,
        "2" | "two" | "second" | "zwei" | "zweite" | "zweiter" | "other" | "secondary" => 2,
        "3" | "three" | "third" | "drei" | "dritte" | "dritter" => 3,
        "current" | "this" | "same" => 0,
        n => n.parse::<u8>().ok()?.min(9),
    })
}

/// The corner orb's size after "smaller", "bigger" or "normal" (a fifth at a time, between six
/// tenths and not quite twice its usual size); anything else leaves it.
pub fn corner_orb_scale(now: f32, step: &str) -> f32 {
    let next = match words(step).as_str() {
        "smaller" | "small" | "kleiner" | "klein" => now / 1.2,
        "bigger" | "larger" | "big" | "large" | "größer" | "groesser" | "gross" | "groß" => now * 1.2,
        "normal" | "default" | "reset" | "usual" => 1.0,
        _ => now,
    };
    next.clamp(0.6, 1.8)
}

/// The board's own layout, accepted by `configure_interface` and `control_orb_mode` alike: where
/// the emblem sits (`orb_board_position`), one board across every monitor (`span_all_monitors`)
/// and whether new windows may overlap (`allow_window_overlap`).
fn apply_board_layout(s: &mut InterfaceSettings, args: &Value, done: &mut Vec<String>) -> Result<(), String> {
    if let Some(spot) = str_arg(args, &["orb_board_position", "emblem_position"]) {
        if orb_aside_phrase(spot) {
            // "Move out of the way": the emblem waits in its corner until it is called back.
            s.orb_aside = true;
            done.push("emblem out of the way, docked in its corner".to_string());
        } else {
            let m = orb_move(spot).ok_or_else(|| format!("unknown emblem spot '{spot}' (top left, top center, right side, bottom center, center, a bit left, aside …)"))?;
            s.orb_board_pos = apply_orb_move(m, s.orb_board_pos);
            s.orb_aside = false;
            // In pure orb mode the floating orb goes to that corner too.
            if let Some(a) = anchor_id(spot).filter(|_| s.orb_mode) {
                s.orb_anchor = a.to_string();
            }
            done.push(match m {
                OrbMove::Step(_) => "emblem nudged".to_string(),
                OrbMove::Home => "emblem moved to the centre".to_string(),
                OrbMove::Spot(_) => format!("emblem moved to the {spot}"),
            });
        }
    }
    // "Use all monitors" is said of the background he always is (Sir, 2026-11-13, of the
    // background: "throw it on all monitors"); "toggle span mode": the other of the two.
    let span = match args.get("span_all_monitors").and_then(Value::as_str).map(words).as_deref() {
        Some("toggle" | "switch" | "flip" | "other") => Some(!s.background_all_monitors),
        _ => bool_arg(args, "span_all_monitors"),
    };
    if let Some(on) = span {
        s.background_all_monitors = on;
        done.push(if on { "the background on every monitor".into() } else { "the background on the first monitor".to_string() });
    }
    if let Some(on) = bool_arg(args, "allow_window_overlap") {
        s.allow_window_overlap = on;
        done.push(if on { "windows may overlap".into() } else { "windows keep apart".to_string() });
    }
    Ok(())
}

/// `control_orb_mode(enabled, orb_size_px, screen_position, monitor, opacity, always_on_top)`.
pub fn control_orb_mode(current: &InterfaceSettings, args: &Value) -> Result<Configured, String> {
    let mut s = current.clone();
    // "Are you in orb mode?": how it stands, nothing moved.
    if args["action"] == "status" {
        let summary = if s.orb_mode {
            format!("Orb mode is on, Sir: {:.0} pixels, {}.", s.orb_size_px, s.orb_anchor.replace('-', " "))
        } else {
            "Orb mode is off, Sir. The full display is up.".to_string()
        };
        return Ok(Configured { settings: s, window: None, summary });
    }
    let mut done: Vec<String> = Vec::new();
    let enabled = bool_arg(args, "enabled").or_else(|| bool_arg(args, "orb_mode")).or_else(|| bool_arg(args, "on"));
    if let Some(on) = enabled {
        s.orb_mode = on;
        done.push(if on { "pure orb mode on".into() } else { "pure orb mode off, the full HUD is back".into() });
    }
    if let Some(v) = args.get("orb_size_px").or_else(|| args.get("size")).or_else(|| args.get("orb_size")).filter(|v| !v.is_null()) {
        let px = match v.as_str().map(words).as_deref() {
            Some("bigger" | "larger" | "größer" | "groesser") => Some(s.orb_size_px * 1.2),
            Some("smaller" | "kleiner") => Some(s.orb_size_px / 1.2),
            _ => number_like(v).map(|(x, rel)| if rel { s.orb_size_px + x } else { x }),
        };
        match px {
            Some(px) => {
                s.orb_size_px = px.clamp(MIN_ORB_PX, MAX_ORB_PX);
                done.push(format!("orb {:.0} px", s.orb_size_px));
            }
            None => return Err(format!("unclear orb size {v} (120-900 px)")),
        }
    }
    apply_board_layout(&mut s, args, &mut done)?;
    if let Some(pos) = str_arg(args, &["screen_position", "position", "anchor", "corner"]) {
        // "top-right of monitor 2" carries the monitor too.
        let lower = pos.to_lowercase();
        let (place, monitor) = match lower.split_once(" of ").or_else(|| lower.split_once(" on ")) {
            Some((a, m)) => (a.trim().to_string(), monitor_number(&json!(m.trim()))),
            None => (pos.to_string(), None),
        };
        match anchor_id(&place) {
            Some(a) => {
                s.orb_anchor = a.to_string();
                done.push(format!("orb at {a}"));
            }
            None => return Err(format!("unknown position '{pos}' (top-right, top-left, bottom-right, bottom-left, center)")),
        }
        if let Some(m) = monitor {
            s.orb_monitor = m;
        }
    }
    if let Some(m) = args.get("monitor").or_else(|| args.get("target_monitor")).filter(|v| !v.is_null()) {
        match monitor_number(m) {
            Some(n) => {
                s.orb_monitor = n;
                done.push(if n == 0 { "orb on the current monitor".into() } else { format!("orb on monitor {n}") });
            }
            None => return Err(format!("unknown monitor {m}")),
        }
    }
    if let Some(v) = args.get("opacity").filter(|v| !v.is_null()) {
        let current_pct = (s.orb_opacity * 100.0).round() as u8;
        match opacity_value(v, current_pct) {
            Some(pct) => {
                s.orb_opacity = (pct as f32 / 100.0).clamp(0.15, 1.0);
                done.push(format!("orb opacity {:.0} %", s.orb_opacity * 100.0));
            }
            None => return Err(format!("unclear opacity {v} (0.15-1.0)")),
        }
    }
    if let Some(on) = bool_arg(args, "always_on_top").or_else(|| bool_arg(args, "on_top")) {
        s.orb_on_top = on;
        done.push(format!("orb always on top {}", if on { "on" } else { "off" }));
    }
    if done.is_empty() {
        return Err("Nothing to change: pass enabled, orb_size_px (120-900), screen_position, monitor, opacity (0.15-1.0) or always_on_top.".into());
    }
    // Placing the orb means orb mode; a size alone in the full HUD resizes its orb there.
    let placed = args.get("screen_position").is_some() || args.get("monitor").is_some();
    if enabled.is_none() && !s.orb_mode && placed {
        s.orb_mode = true;
        done.insert(0, "pure orb mode on".into());
    } else if enabled.is_none() && !s.orb_mode && args.get("orb_size_px").is_some() {
        s.orb_scale = (s.orb_size_px / 300.0).clamp(MIN_ORB_SCALE, MAX_ORB_SCALE);
        done.push(format!("HUD orb at {:.0} %", s.orb_scale * 100.0));
    }
    s.reconcile(current);
    s.sanitize();
    let summary = format!(
        "Orb mode: {}. Now: orb mode {}, {:.0} px, {}, monitor {}, opacity {:.0} %, on top {}.",
        done.join(", "),
        if s.orb_mode { "on" } else { "off" },
        s.orb_size_px,
        s.orb_anchor,
        if s.orb_monitor == 0 { "current".to_string() } else { s.orb_monitor.to_string() },
        s.orb_opacity * 100.0,
        if s.orb_on_top { "yes" } else { "no" },
    );
    Ok(Configured { settings: s, window: None, summary })
}

/// Spoken confirmation of `control_orb_mode`.
pub fn orb_mode_ack(args: &Value, now: &InterfaceSettings) -> String {
    match bool_arg(args, "enabled").or_else(|| bool_arg(args, "orb_mode")) {
        Some(false) => "There you are, Sir. The full display is back.".into(),
        Some(true) if args.as_object().is_some_and(|o| o.len() == 1) => "Orb mode, Sir.".into(),
        // A size while the full HUD is shown resized its orb there.
        _ if !now.orb_mode => format!("Done, Sir. The orb is at {:.0} percent.", now.orb_scale * 100.0),
        _ => format!("Done, Sir. The orb is {:.0} pixels, {}.", now.orb_size_px, now.orb_anchor.replace('-', " ")),
    }
}

/// The `control_orb_mode` tool schema.
pub fn orb_mode_tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "control_orb_mode",
        "description": "Pure Orb Mode: only YOUR 3D orb floats on screen (borderless, transparent, always on top; Sir drags it, the mouse wheel resizes it, a double-click brings the full HUD back). \"Switch to orb mode\", \"only show the orb\", \"go minimal orb\" → enabled true; \"normal mode\", \"show chat\" → enabled false. Also its size, corner, monitor and opacity.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["status"], "description": "\"status\" only reads how it stands; leave it out to change something."},
            "enabled": {"type": "boolean"},
            "orb_size_px": {"type": "number", "description": "120-900"},
            "screen_position": {"type": "string", "enum": ["top-right", "top-left", "bottom-right", "bottom-left", "center"]},
            "monitor": {"type": "integer", "description": "1 = primary, 2 = second monitor …"},
            "opacity": {"type": "number", "description": "0.15-1.0"},
            "always_on_top": {"type": "boolean"}
        }, "required": []}
    }})
}

fn layout_ack(layout: HudLayout) -> String {
    match layout {
        HudLayout::Orb => "Very good, Sir. Just the reactor.".into(),
        HudLayout::Compact => "Very good, Sir. Compact view.".into(),
        HudLayout::Full => "There you are, Sir. Full display.".into(),
    }
}

/// A preset's name as he says it ("Stark Gold").
fn spoken_theme(preset: &str) -> String {
    preset.split('_').map(capitalise).collect::<Vec<_>>().join(" ")
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The `configure_interface` tool schema.
pub fn tool_definition() -> Value {
    let mut definition = json!({"type": "function", "function": {
        "name": "configure_interface",
        "description": "Restyle or rearrange YOUR OWN HUD (the J.A.I. window) when Sir asks: theme, colour, core, zoom, transparency, layout, always on top, fullscreen, title bar, name, or show/hide/move/resize the window. Pass only what changes.",
        "parameters": {"type": "object", "properties": {
            "core_style": {"type": "string", "enum": ["simple", "complex"], "description": "Your core: simple (rings) or complex (the sphere of circuitry)."},
            "accent_color": {"type": "string", "description": "A light of his own colour: a colour name or #RRGGBB."},
            "scale": {"type": "number", "description": "Zoom: 1.0 normal, 1.25 bigger, 0.8 smaller."},
            "opacity": {"type": "integer", "description": "Background opacity 20-100 %."},
            "layout": {"type": "string", "enum": ["full", "compact", "orb"], "description": "compact = reactor and chat, orb = reactor only."},
            "always_on_top": {"type": "boolean"},
            "fullscreen": {"type": "boolean"},
            "core_view": {"type": "boolean", "description": "The 3D sphere of your source modules ('show me your core')."},
            "hud_palette": {"type": "string", "enum": ["stark_gold", "mono", "arc_reactor", "hot_rod", "gamma", "vibranium", "ember", "graphite"], "description": "The whole HUD's colour theme; a plain colour is accent_color."},
            "create_theme": {"type": "object", "description": "Design, save and apply a named theme ('a theme called Cyberpunk with neon yellow borders').", "properties": {"name": {"type": "string"}, "description": {"type": "string", "description": "Sir's colours per role and/or a mood."}, "core_primary": {"type": "string"}, "core_secondary": {"type": "string"}, "window_border": {"type": "string"}, "background_tint": {"type": "string"}}},
            "save_theme": {"type": "string", "description": "Save the current look under this name."},
            "load_theme": {"type": "string", "description": "Apply a saved theme by name."},
            "orb_board_position": {"type": "string", "description": "The emblem's spot on the board: top left … bottom right, center."},
            "span_all_monitors": {"type": "boolean", "description": "His background on every monitor (false: the first)."},
            "allow_window_overlap": {"type": "boolean", "description": "New windows may stack (default: side by side)."},
            "hud_view": {"type": "string", "enum": ["home", "board", "core", "sim_lab", "telemetry", "schedule", "console"]},
            "feature_lab": {"type": "boolean", "description": "The Feature Lab: everything you can do, each with a TRY button."},
            "theme_studio": {"type": "boolean", "description": "The Theme Studio, where Sir designs a theme ('create a new theme')."},
            "mixer_view": {"type": "boolean", "description": "The Mixer block ('open the mixer')."},
            "core_scale": {"type": ["number", "string"], "description": "Core sphere size: 50-300 % or bigger / smaller."},
            "core_spin": {"type": "string", "enum": ["faster", "slower", "stop"]},
            "text_console": {"type": "boolean", "description": "The slide-out text console."},
            "frameless": {"type": "boolean", "description": "true hides the title bar."},
            "animations": {"type": "boolean"},
            "display_finish": {"type": "boolean", "description": "scan lines and film grain over the HUD"},
            "sound_cues": {"type": "boolean", "description": "The board's sound cues."},
            "title": {"type": "string", "description": "Name in the HUD header."},
            "window": {"type": "string", "enum": ["show", "hide", "minimize", "maximize", "restore", "center", "bigger", "smaller", "top_left", "top_right", "bottom_left", "bottom_right", "left", "right"]},
            "reset": {"type": "boolean", "description": "Restore the default look (other fields apply on top)."}
        }, "required": []}
    }});
    for (key, schema) in crate::hud_feel::schema() {
        definition["function"]["parameters"]["properties"][key] = schema;
    }
    definition
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(args: Value) -> InterfaceSettings {
        configure(&InterfaceSettings::default(), &args).expect("configures").settings
    }

    #[test]
    fn colours_in_english_and_german() {
        assert_eq!(parse_color("red"), Some([255, 59, 59]));
        assert_eq!(parse_color("Rot"), Some([255, 59, 59]));
        assert_eq!(parse_color("rote"), Some([255, 59, 59]));
        assert_eq!(parse_color("the colour blue"), Some([59, 130, 246]));
        assert_eq!(parse_color("#ff8800"), Some([255, 136, 0]));
        assert_eq!(parse_color("#f80"), Some([255, 136, 0]));
        assert_eq!(parse_color("rgb(1, 2, 3)"), Some([1, 2, 3]));
        assert_eq!(parse_color("dunkelblau"), Some(scale_rgb([59, 130, 246], 0.55)));
        assert_eq!(parse_color("light green"), Some(mix([43, 255, 136], WHITE, 0.45)));
        assert_eq!(parse_color("hellgrün"), parse_color("light green"));
        assert_eq!(parse_color("spotify"), None);
        assert_eq!(parse_color("volume to 30"), None);
    }

    #[test]
    fn themes_and_layouts_by_alias() {
        // The words that named the old engine's themes name the presets of today.
        for (word, preset) in [
            ("Iron Man", "hot_rod"),
            ("mark-42", "hot_rod"),
            ("crimson_mk42", "hot_rod"),
            ("dark", "graphite"),
            ("stealth_obsidian", "graphite"),
            ("light", "mono"),
            ("black and white", "mono"),
            ("stark_gold", "stark_gold"),
            ("Stark", "stark_gold"),
            ("default", "stark_gold"),
            ("arc_cyan", "arc_reactor"),
            ("emerald_matrix", "gamma"),
        ] {
            assert_eq!(crate::holo_theme::preset_for_word(&words(word)), Some(preset), "{word}");
        }
        assert_eq!(HudLayout::parse("Minimal"), Some(HudLayout::Orb));
        assert_eq!(HudLayout::parse("kompakt"), Some(HudLayout::Compact));
    }

    #[test]
    fn configure_applies_colours_and_reports_changes() {
        // A colour that names a preset is that preset; any other is a light of its own.
        assert_eq!(apply(json!({"accent_color": "red"})).hud_palette, "hot_rod");
        assert_eq!(apply(json!({"accent_color": "#12AB34"})).hud_palette, "custom:#12AB34");
        assert_eq!(apply(json!({"theme": "mark 42"})).hud_palette, "hot_rod");
        // "Make the interface black": only light shows on black glass.
        assert_eq!(apply(json!({"accent_color": "black"})).hud_palette, "mono");
        assert_eq!(apply(json!({"accent_color": "#050505"})).hud_palette, "mono");
        let done = configure(&InterfaceSettings::default(), &json!({"accent_color": "red", "opacity": "nonsense"})).unwrap();
        assert!(done.summary.contains("accent colour red") && done.summary.contains("Not changed"));
        assert!(configure(&InterfaceSettings::default(), &json!({})).is_err());
        assert!(configure(&InterfaceSettings::default(), &json!({"theme": "banana"})).is_err());
        // The canvas is always black: a background is no setting any more.
        assert!(configure(&InterfaceSettings::default(), &json!({"background_color": "red"})).is_err());
    }

    #[test]
    fn configure_relative_zoom_and_opacity() {
        let s = apply(json!({"scale": "bigger"}));
        assert_eq!(s.scale, 1.15);
        assert_eq!(apply(json!({"scale": 120})).scale, 1.2);
        assert_eq!(apply(json!({"scale": 9.0})).scale, MIN_SCALE, "9 means 9 %, clamped");
        assert_eq!(apply(json!({"opacity": "-15"})).opacity, 85);
        assert_eq!(apply(json!({"opacity": 0.6})).opacity, 60);
        assert_eq!(apply(json!({"opacity": 5})).opacity, MIN_OPACITY);
        assert_eq!(apply(json!({"transparency": 30})).opacity, 70);
    }

    #[test]
    fn configure_layout_and_window() {
        let done = configure(&InterfaceSettings::default(), &json!({"window": "top_right"})).unwrap();
        assert_eq!(done.window, Some(WindowCommand::Place(Corner::TopRight)));
        assert_eq!(WindowCommand::parse("oben links"), Some(WindowCommand::Place(Corner::TopLeft)));
        let s = apply(json!({"always_on_top": true, "title": "  F.R.I.D.A.Y.  "}));
        assert!(s.always_on_top);
        assert_eq!(s.title, "F.R.I.D.A.Y.");
        let s = configure(&s, &json!({"reset": true})).unwrap().settings;
        assert_eq!(s, InterfaceSettings::default());
    }

    #[test]
    fn hand_edited_settings_load_leniently() {
        let s: InterfaceSettings =
            serde_json::from_str(r#"{"layout": "weird", "hidden_panels": ["chat"], "opacity": 3}"#).unwrap();
        let mut s = s;
        s.sanitize();
        assert_eq!(s.layout, HudLayout::Full);
        assert_eq!(s.opacity, MIN_OPACITY);
        assert_eq!(serde_json::to_value(HudLayout::Orb).unwrap(), json!("orb"));
    }

    #[test]
    fn emblem_place_phrases_are_read_word_by_word() {
        use OrbMove::*;
        // The screenshot sentence: the leading "a" used to be stripped before the amount words
        // were compared, so every "a little / a bit" phrase fell through to window snapping.
        assert_eq!(orb_move("a little bit to the left"), Some(Step([-ORB_SMALL_STEP, 0.0])));
        assert_eq!(orb_move("a bit up"), Some(Step([0.0, -ORB_SMALL_STEP])));
        assert_eq!(orb_move("slightly left"), Some(Step([-ORB_SMALL_STEP, 0.0])));
        assert_eq!(orb_move("etwas nach rechts"), Some(Step([ORB_SMALL_STEP, 0.0])));
        assert_eq!(orb_move("ein bisschen nach unten"), Some(Step([0.0, ORB_SMALL_STEP])));
        assert_eq!(orb_move("further to the right"), Some(Step([ORB_STEP, 0.0])));
        assert_eq!(orb_move("way more to the left"), Some(Step([-ORB_BIG_STEP, 0.0])));
        assert_eq!(orb_move("up"), Some(Step([0.0, -ORB_STEP])));
        assert_eq!(orb_move("a bit lower"), Some(Step([0.0, ORB_SMALL_STEP])));
        // Fixed spots, however they are worded.
        for (phrase, at) in [
            ("top left", [0.14, 0.2]),
            ("to the top right corner of the screen", [0.86, 0.2]),
            ("upper left", [0.14, 0.2]),
            ("lower left", [0.14, 0.8]),
            ("bottom center", [0.5, 0.8]),
            ("top", [0.5, 0.2]),
            ("left", [0.14, 0.5]),
            ("the right side", [0.86, 0.5]),
            ("middle left", [0.14, 0.5]),
            ("oben rechts", [0.86, 0.2]),
            ("unten mitte", [0.5, 0.8]),
            ("ganz nach unten links", [0.14, 0.8]),
        ] {
            assert_eq!(orb_move(phrase), Some(Spot(at)), "{phrase}");
        }
        for home in ["center", "the centre", "back to the middle", "in die Mitte", "home", "default"] {
            assert_eq!(orb_move(home), Some(Home), "{home}");
        }
        // Not a place on the board: left to the floating-orb and window rules.
        for other in ["monitor 2", "top right of monitor 2", "red", "bigger", "a little bit", "back", ""] {
            assert_eq!(orb_move(other), None, "{other}");
        }
        // A step starts where the emblem is and stays on the canvas.
        let near = |got: Option<[f32; 2]>, want: [f32; 2]| got.is_some_and(|g| (g[0] - want[0]).abs() < 1e-5 && (g[1] - want[1]).abs() < 1e-5);
        assert!(near(apply_orb_move(Step([-0.08, 0.0]), None), [0.42, 0.5]));
        assert_eq!(apply_orb_move(Step([-0.26, 0.0]), Some([0.14, 0.5])), Some([0.08, 0.5]));
        assert_eq!(apply_orb_move(Home, Some([0.2, 0.2])), None);
        // The older entry points agree.
        assert!(near(nudge("a little bit to the left", None).flatten(), [0.42, 0.5]));
        assert_eq!(nudge("top left", None), None);
        assert_eq!(board_spot("middle"), Some(None));
        assert_eq!(board_spot("slightly left"), None);
    }

    /// 2026-11-14: whether what is said stands beside the corner orb, that orb's size and the
    /// desktop clock are settings of his; a step from a spot on another monitor stays there.
    #[test]
    fn the_corner_orb_and_the_desktop_clock_are_settings() {
        let now = InterfaceSettings::default();
        assert!(now.orb_words && now.desk_clock && now.desk_clock_date && !now.desk_clock_seconds && !now.desk_clock_12h && now.desk_clock_pos.is_none());
        assert!(!configure(&now, &json!({"orb_words": false})).unwrap().settings.orb_words);
        // Any setting of the clock but its hiding shows it.
        let hidden = configure(&now, &json!({"desk_clock": false})).unwrap().settings;
        assert!(!hidden.desk_clock);
        let seconds = configure(&hidden, &json!({"desk_clock_seconds": true})).unwrap().settings;
        assert!(seconds.desk_clock && seconds.desk_clock_seconds);
        // The corner orb: a fifth at a time, within its range.
        assert!((corner_orb_scale(1.0, "bigger") - 1.2).abs() < 1e-5 && (corner_orb_scale(1.0, "smaller") - 1.0 / 1.2).abs() < 1e-5);
        assert_eq!(corner_orb_scale(1.7, "bigger"), 1.8);
        assert_eq!(corner_orb_scale(0.61, "smaller"), 0.6);
        assert_eq!(corner_orb_scale(1.44, "normal"), 1.0);
        assert_eq!(corner_orb_scale(1.44, "purple"), 1.44);
        let smaller = configure(&now, &json!({"corner_orb_size": "smaller"})).unwrap().settings;
        assert!((smaller.corner_orb_scale - 0.83).abs() < 0.01 && smaller.orb_scale == now.orb_scale, "{}", smaller.corner_orb_scale);
        // A step from a spot on another monitor (beyond 0-1) stays on that monitor; on the
        // first one it keeps to the screen.
        let [x, y] = apply_orb_move(OrbMove::Step([-0.1, 0.0]), Some([1.5, 0.5])).unwrap();
        assert!((x - 1.4).abs() < 1e-5 && y == 0.5, "{x} {y}");
        assert_eq!(apply_orb_move(OrbMove::Step([0.5, 0.0]), Some([0.9, 0.5])), Some([0.92, 0.5]));
        // Step after step on another monitor never runs off it (past it, the emblem fell back
        // to the first, and the steps back seemed to do nothing).
        for (step, from, want) in [(0.5, 1.85, 1.92), (-0.5, 1.2, 1.08), (-0.5, -0.85, -0.92)] {
            let [x, _] = apply_orb_move(OrbMove::Step([step, 0.0]), Some([from, 0.5])).unwrap();
            assert!((x - want).abs() < 1e-5, "{step} from {from}: {x}");
        }
    }

    #[test]
    fn board_layout_commands_reach_configure_interface() {
        // The fast path sends these three to `configure_interface`; only `control_orb_mode` knew
        // them, so each ended in "Nothing to change".
        let now = InterfaceSettings::default();
        let s = configure(&now, &json!({"orb_board_position": "top left"})).unwrap().settings;
        assert_eq!(s.orb_board_pos, Some([0.14, 0.2]));
        let nudged = configure(&s, &json!({"orb_board_position": "a little bit to the right"})).unwrap();
        let at = nudged.settings.orb_board_pos.expect("still off-centre");
        assert!((at[0] - 0.22).abs() < 1e-5 && (at[1] - 0.2).abs() < 1e-5, "{at:?}");
        assert!(nudged.summary.contains("emblem nudged"), "{}", nudged.summary);
        let home = configure(&nudged.settings, &json!({"orb_board_position": "back to the middle"})).unwrap().settings;
        assert_eq!(home.orb_board_pos, None);
        assert!(configure(&now, &json!({"orb_board_position": "monitor 2"})).is_err());
        // "Use all monitors" is said of the background he always is: it covers every monitor or
        // the first, both ways, toggled too, and he says which (as the background he once said
        // "Back on your first monitor" while it went over all of them; "stop spanning" did nothing).
        let one = configure(&now, &json!({"span_all_monitors": false})).unwrap().settings;
        assert!(!one.background_all_monitors && one.span_all_monitors == now.span_all_monitors && one.fullscreen == now.fullscreen);
        let all = configure(&one, &json!({"span_all_monitors": "toggle"})).unwrap().settings;
        assert!(all.background_all_monitors);
        assert!(!configure(&all, &json!({"span_all_monitors": "toggle"})).unwrap().settings.background_all_monitors);
        assert_eq!(spoken_ack(&json!({"span_all_monitors": true}), &all), "On every monitor, Sir.");
        assert_eq!(spoken_ack(&json!({"span_all_monitors": false}), &one), "On your first monitor only, Sir.");
        // (The board as a window, in a session without a desktop:) spanning and the fullscreen
        // board exclude each other, whichever way one is asked for.
        let spanned = InterfaceSettings { span_all_monitors: true, fullscreen: false, ..now.clone() };
        let full = configure(&spanned, &json!({"fullscreen": true})).unwrap().settings;
        assert!(full.fullscreen && !full.span_all_monitors);
        let mut f11 = spanned.clone();
        f11.fullscreen = true;
        f11.reconcile(&spanned);
        assert!(f11.fullscreen && !f11.span_all_monitors);
        let mut menu = now.clone();
        menu.span_all_monitors = true;
        menu.reconcile(&now);
        assert!(menu.span_all_monitors && !menu.fullscreen, "the SCREENS switch on the fullscreen board");
        // "Move out of the way": the emblem docks until it is called back or put somewhere.
        let aside = configure(&now, &json!({"orb_board_position": "aside"})).unwrap().settings;
        assert!(aside.orb_aside && aside.orb_board_pos.is_none());
        assert_eq!(spoken_ack(&json!({"orb_board_position": "aside"}), &aside), "Out of your way, Sir. Say the word and I come back.");
        let returned = configure(&aside, &json!({"orb_board_position": "center"})).unwrap().settings;
        assert!(!returned.orb_aside);
        assert_eq!(spoken_ack(&json!({"orb_board_position": "center"}), &returned), "Back in the middle, Sir.");
        assert!(!configure(&aside, &json!({"orb_board_position": "top left"})).unwrap().settings.orb_aside);
        // It is never saved: at the next start the emblem is in the middle again.
        assert!(!serde_json::to_string(&aside).unwrap().contains("orb_aside"));
        assert!(configure(&now, &json!({"allow_window_overlap": true})).unwrap().settings.allow_window_overlap);
        // `control_orb_mode` still takes them (saved spatial scenes use it).
        assert_eq!(control_orb_mode(&now, &json!({"orb_board_position": "bottom right"})).unwrap().settings.orb_board_pos, Some([0.86, 0.8]));
    }

    #[test]
    fn orb_mode_is_set_and_left() {
        let now = InterfaceSettings::default();
        let s = control_orb_mode(&now, &json!({"enabled": true, "orb_size_px": 5000, "screen_position": "top right of monitor 2", "opacity": 0.05})).unwrap().settings;
        assert!(s.orb_mode);
        assert_eq!((s.orb_size_px, s.orb_anchor.as_str(), s.orb_monitor), (MAX_ORB_PX, "top-right", 2));
        assert_eq!(s.orb_opacity, 0.15);
        let s = control_orb_mode(&s, &json!({"enabled": false})).unwrap().settings;
        assert!(!s.orb_mode);
    }

    #[test]
    fn spoken_confirmations() {
        let now = InterfaceSettings::default();
        assert_eq!(spoken_ack(&json!({"accent_color": "rot"}), &now), "Very good, Sir. The interface is now red.");
        assert_eq!(spoken_ack(&json!({"accent_color": "dunkelblau"}), &now), "Very good, Sir. The interface is now dark blue.");
        assert_eq!(spoken_ack(&json!({"theme": "iron man"}), &now), "Very good, Sir. Switching to the Hot Rod theme.");
        assert_eq!(spoken_ack(&json!({"hud_palette": "arc-reactor"}), &now), "Arc Reactor it is, Sir.");
        // The conversation block ("close the chat"): off by default, switched and said so.
        assert!(!now.comms_block && apply(json!({"comms_block": true})).comms_block);
        assert_eq!(spoken_ack(&json!({"comms_block": false}), &now), "Done, Sir. The conversation block is away.");
        assert_eq!(spoken_ack(&json!({"scale": "bigger"}), &now), "Is this better, Sir?");
        assert_eq!(spoken_ack(&json!({"window": "hide"}), &now), "Very good, Sir. I'll be in the tray if you need me.");
    }
}
