//! The HUD's look and layout, which Jarvis changes himself when Sir asks ("make the interface
//! red", "hide the system panel", "zoom in", "stay on top", "go minimal").
//!
//! The settings live in `jarvis_config.json` (`interface`) and the native window re-applies
//! them live whenever they change. `configure` turns `configure_interface` tool arguments (from
//! the LLM or the instant-command fast path) into new settings plus an optional one-shot window
//! action (show, hide, move, resize).

use crate::tools::{bool_arg, str_arg};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_TITLE: &str = "J.A.R.V.I.S.";
const MIN_OPACITY: u8 = 15;
/// The darkest background "darker" goes to (deep obsidian).
pub const DEEPEST_BACKGROUND: [u8; 3] = [3, 5, 9];
/// The lightest background the HUD ever shows (dark slate glass): colour only lives in accents.
pub const LIGHTEST_BACKGROUND: [u8; 3] = [12, 20, 28];
const MAX_BACKGROUND_LUMINANCE: f32 = 0.085;
/// Pure orb mode: the window is a square of this many pixels.
const MIN_ORB_PX: f32 = 120.0;
const MAX_ORB_PX: f32 = 900.0;
const MIN_ROUNDING: f32 = 0.0;
const MAX_ROUNDING: f32 = 32.0;
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
    /// Every panel (minus `hidden_panels`).
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

/// A card of the HUD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HudPanel {
    Header,
    Audio,
    Devices,
    System,
    Reactor,
    Chat,
    Memory,
    Skills,
    Tools,
    Brain,
    /// Jarvis' dynamic widgets (gauges, charts, timers, notes …).
    Widgets,
    /// The quick-action pills above the conversation ("Daddy's home", "Add calendar event" …).
    Pills,
}

impl HudPanel {
    pub const ALL: [Self; 12] = [
        Self::Header,
        Self::Audio,
        Self::Devices,
        Self::System,
        Self::Reactor,
        Self::Widgets,
        Self::Chat,
        Self::Pills,
        Self::Memory,
        Self::Skills,
        Self::Tools,
        Self::Brain,
    ];
    pub const LEFT: [Self; 3] = [Self::Audio, Self::Devices, Self::System];
    pub const RIGHT: [Self; 4] = [Self::Memory, Self::Skills, Self::Tools, Self::Brain];
    const SIDES: [Self; 7] =
        [Self::Audio, Self::Devices, Self::System, Self::Memory, Self::Skills, Self::Tools, Self::Brain];
    /// The secondary telemetry: system monitor, microphone meters, tool log and brain panel.
    pub const TELEMETRY: [Self; 4] = [Self::System, Self::Audio, Self::Tools, Self::Brain];

    pub fn id(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Audio => "audio",
            Self::Devices => "devices",
            Self::System => "system",
            Self::Reactor => "reactor",
            Self::Chat => "chat",
            Self::Memory => "memory",
            Self::Skills => "skills",
            Self::Tools => "tools",
            Self::Brain => "brain",
            Self::Widgets => "widgets",
            Self::Pills => "pills",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Audio => "microphone panel",
            Self::Devices => "audio devices",
            Self::System => "system monitor",
            Self::Reactor => "reactor",
            Self::Chat => "conversation",
            Self::Memory => "memory panel",
            Self::Skills => "skills panel",
            Self::Tools => "tool log",
            Self::Brain => "brain panel",
            Self::Widgets => "widgets panel",
            Self::Pills => "quick pills bar",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    /// Part of the compact layout.
    fn in_compact(self) -> bool {
        matches!(self, Self::Header | Self::Reactor | Self::Chat | Self::Widgets | Self::Pills)
    }

    /// Panel names, aliases and groups ("left side", "sidebars") in English or German. Words that
    /// also name other things ("microphone", "model", "log") only count with "panel" after them.
    pub fn parse(name: &str) -> Vec<Self> {
        let mut n = words(name);
        let mut explicit = false;
        for suffix in [
            " panels", " panel", " section", " card", " box", " widget", " view", " window", " tabs", " tab", " sidebar",
            " anzeige", " bereich", " fenster", " leiste", " karte", " reiter",
        ] {
            if let Some(rest) = n.strip_suffix(suffix) {
                n = rest.trim().to_string();
                explicit = true;
                break;
            }
        }
        let panels: &[Self] = match n.as_str() {
            "header" | "top bar" | "title bar" | "status bar" | "kopfzeile" | "titelleiste" | "statusleiste" => {
                &[Self::Header]
            }
            "vad" | "voice activity" | "voice activity detection" | "input level" | "sprachaktivität" | "pegel" => {
                &[Self::Audio]
            }
            "microphone" | "mic" | "mikrofon" | "mikro" | "audio" if explicit => &[Self::Audio],
            "devices" | "audio devices" | "audio device" | "geräte" | "geraete" | "audiogeräte" | "audiogeraete" | "audio settings"
            | "sound settings" | "speaker settings" | "audio geräte" => &[Self::Devices],
            "telemetry" | "secondary telemetry" | "telemetrie" | "diagnostics" => &Self::TELEMETRY,
            "pills" | "quick pills" | "quick pills bar" | "pills bar" | "action bar" | "quick actions" | "quick action" | "quick action buttons" | "action buttons" | "suggestions"
            | "suggestion buttons" | "shortcuts" | "shortcut buttons" | "schnellaktionen" => &[Self::Pills],
            "speaker" | "speakers" | "voice" | "lautsprecher" | "stimme" if explicit => &[Self::Devices],
            "system" | "system monitor" | "system stats" | "stats" | "statistics" | "hardware"
            | "performance" | "systemmonitor" | "systemanzeige" | "systeminfo" | "leistung" | "auslastung" | "system telemetry" => {
                &[Self::System]
            }
            "cpu" | "gpu" | "ram" if explicit => &[Self::System],
            "reactor" | "arc reactor" | "orb" | "core" | "reaktor" | "kugel" | "kern" | "arc reactor centerpiece" | "centerpiece"
            | "centrepiece" | "reactor centerpiece" => &[Self::Reactor],
            "chat" | "conversation" | "messages" | "transcript" | "chat log" | "unterhaltung" | "gespräch"
            | "gespraech" | "nachrichten" | "verlauf" | "konversation" => &[Self::Chat],
            "memory" | "memories" | "long term memory" | "gedächtnis" | "gedaechtnis" | "erinnerungen" | "memory manager" => &[Self::Memory],
            "skills" | "skill list" | "abilities" | "fähigkeiten" | "faehigkeiten" => &[Self::Skills],
            "tool log" | "tools" | "activity log" | "werkzeuge" | "tool protokoll" | "protokoll" | "tool logs" | "tool execution log"
            | "tool execution logs" | "execution log" | "execution logs" => &[Self::Tools],
            "log" | "activity" if explicit => &[Self::Tools],
            "brain" | "gehirn" | "language model" | "sprachmodell" => &[Self::Brain],
            "widgets" | "widget grid" | "widget panel" | "dashboard" | "my widgets" | "your widgets" | "dynamic widgets"
            | "dynamic widgets dock" | "widgets dock" | "widget dock" => &[Self::Widgets],
            "model" | "llm" | "modell" if explicit => &[Self::Brain],
            "left" | "left side" | "left sidebar" | "left column" | "linke seite" | "linke spalte" => &Self::LEFT,
            "right" | "right side" | "right sidebar" | "right column" | "rechte seite" | "rechte spalte" => &Self::RIGHT,
            "sidebars" | "sidebar" | "side" | "both sidebars" | "seitenleisten" | "seitenleiste" | "seiten" => &Self::SIDES,
            "all" | "everything" | "alle" | "alles" => &Self::ALL,
            _ => &[],
        };
        panels.to_vec()
    }
}

// ---------------------------------------------------------------------------- settings ----

/// Persisted under `interface` in `jarvis_config.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceSettings {
    /// Colour preset the accent and background came from ("custom" after a manual colour).
    pub theme: String,
    /// Highlight colour (#RRGGBB): reactor, rings, headings, active controls.
    pub accent: String,
    /// Window background (#RRGGBB).
    pub background: String,
    /// Interface zoom, 1.0 = 100 %.
    pub scale: f32,
    /// Background opacity in percent; text and controls stay fully opaque.
    pub opacity: u8,
    pub layout: HudLayout,
    /// Panel ids (see `HudPanel::id`) hidden from the full layout.
    pub hidden_panels: Vec<String>,
    pub always_on_top: bool,
    /// The HUD board fills the monitor (the default since config v16).
    pub fullscreen: bool,
    /// No title bar: the header (or the reactor) moves the window.
    pub frameless: bool,
    pub animations: bool,
    /// Name shown in the header.
    pub title: String,
    /// Minimal zen mode: no sidebars, widgets or pills — only the 3D orb and a clean chat.
    pub zen_mode: bool,
    /// Radius of the 3D sphere orb relative to its viewport (0.4-2.5).
    pub orb_scale: f32,
    /// Text size on top of the zoom (0.75-1.6).
    pub font_scale: f32,
    /// Corner radius of panels (chat bubbles, inputs and buttons follow it), 0-32.
    pub corner_rounding: f32,
    /// The header's live telemetry readouts (latency, CPU, GPU).
    pub header_telemetry: bool,
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
    /// The look: the True-OLED minimal design (default) or the classic sci-fi HUD.
    pub ui_paradigm: crate::oled_minimal_ui::UiDesignParadigm,
    /// The `COMMS // LIVE RESPONSE` block above the bottom strip. Off by default, like the
    /// reference board: the strip shows what Sir said, the voice carries the answer and the text
    /// console keeps every written reply.
    pub comms_block: bool,
    /// The status pill at the top centre (LISTENING / THINKING / SPEAKING), as in the reference
    /// videos.
    pub status_pill: bool,
    /// Sir's voice as a ring of ticks round the emblem while he speaks, as on the reference orb
    /// (the emblem's own rings follow only Jarvis' voice).
    pub voice_rim: bool,
    /// The board's sound cues (a block materialising, the spotlight, a mode change, an alert).
    pub sound_cues: bool,
    /// The HUD's colour theme (`theme_engine`): one of the eight presets ("rose_crimson",
    /// "arc_cyan", …) or "custom:#RRGGBB". The board, the console, the core sphere and the emblem
    /// all read it; a colour request sets it.
    pub hud_palette: String,
    /// Where Sir put the emblem on the board (fractions of the canvas; None = the centre).
    pub orb_board_pos: Option<[f32; 2]>,
    /// "Use all monitors": the borderless HUD spans the whole virtual desktop.
    pub span_all_monitors: bool,
    /// "Overlap the windows" / "stack them": new windows cascade instead of packing side by side.
    pub allow_window_overlap: bool,
    /// Sir told the emblem to get out of the way: it waits in its corner until he calls it back
    /// (or moves it). Never saved: at the next start it is in the middle again.
    #[serde(skip)]
    pub orb_aside: bool,
}

impl Default for InterfaceSettings {
    fn default() -> Self {
        let stark = &THEMES[0];
        Self {
            theme: stark.name.to_string(),
            accent: stark.accent.to_string(),
            background: stark.background.to_string(),
            scale: 1.0,
            opacity: 100,
            layout: HudLayout::Full,
            hidden_panels: Vec::new(),
            always_on_top: false,
            fullscreen: true,
            frameless: true,
            animations: true,
            title: DEFAULT_TITLE.to_string(),
            // The clean two-zone studio layout: the orb and the conversation; every technical
            // panel lives in the Settings drawer.
            zen_mode: true,
            orb_scale: 1.0,
            font_scale: 1.0,
            corner_rounding: 18.0,
            header_telemetry: true,
            orb_mode: false,
            orb_size_px: 260.0,
            orb_opacity: 1.0,
            orb_on_top: true,
            orb_anchor: "bottom-right".to_string(),
            orb_monitor: 0,
            ui_paradigm: crate::oled_minimal_ui::UiDesignParadigm::ModernOledMinimal,
            comms_block: false,
            status_pill: true,
            voice_rim: true,
            sound_cues: true,
            hud_palette: crate::theme_engine::DEFAULT_PRESET.to_string(),
            orb_board_pos: None,
            span_all_monitors: false,
            allow_window_overlap: false,
            orb_aside: false,
        }
    }
}

impl InterfaceSettings {
    /// One board across every monitor and the fullscreen board on one monitor exclude each
    /// other: whichever of the two was just switched on (compared with `before`) wins, so F11,
    /// the SCREENS menu, a settings checkbox and Jarvis' own tools can never leave both set.
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

/// Dark glass only: a background brighter than slate glass is pulled down to it (hue kept), so no
/// request, theme or hand edit can produce a light or neon window.
pub fn dark_glass(rgb: [u8; 3]) -> [u8; 3] {
    let lum = luminance(rgb);
    let mut out = if lum > MAX_BACKGROUND_LUMINANCE { scale_rgb(rgb, MAX_BACKGROUND_LUMINANCE / lum) } else { rgb };
    // A saturated tint ("deep red") keeps every channel low as well.
    let peak = out.iter().copied().max().unwrap_or(0) as f32;
    if peak > 46.0 {
        out = scale_rgb(out, 46.0 / peak);
    }
    out
}

/// How deep the background is: 0 = slate glass rgb(12,20,28), 1 = obsidian rgb(3,5,9).
pub fn background_darkness(rgb: [u8; 3]) -> f32 {
    let (light, deep) = (luminance(LIGHTEST_BACKGROUND), luminance(DEEPEST_BACKGROUND));
    ((light - luminance(rgb)) / (light - deep)).clamp(0.0, 1.0)
}

/// The background at depth `darkness` (0-1), keeping the hue of `current`.
pub fn with_darkness(current: [u8; 3], darkness: f32) -> [u8; 3] {
    let d = darkness.clamp(0.0, 1.0);
    let (light, deep) = (luminance(LIGHTEST_BACKGROUND), luminance(DEEPEST_BACKGROUND));
    let target = light - d * (light - deep);
    let base = if luminance(current) < 0.004 { LIGHTEST_BACKGROUND } else { current };
    // Rounded towards the target, so repeated "darker" really reaches obsidian (nearest
    // rounding could stall a hue one step short of it).
    let factor = target / luminance(base).max(0.001);
    let scaled = base.map(|v| {
        let x = v as f32 * factor;
        (if factor < 1.0 { x.floor() } else { x.ceil() }).clamp(0.0, 255.0) as u8
    });
    // Never below obsidian (a channel already lower stays as it was), never above slate.
    dark_glass(std::array::from_fn(|i| scaled[i].max(DEEPEST_BACKGROUND[i].min(base[i]))))
}

impl InterfaceSettings {
    pub fn sanitize(&mut self) {
        let defaults = Self::default();
        let known = crate::theme_engine::preset(&self.hud_palette).is_some()
            || self.hud_palette.strip_prefix("custom:#").is_some_and(|h| h.len() == 6 && u32::from_str_radix(h, 16).is_ok())
            || self.hud_palette.strip_prefix(crate::theme_engine::CUSTOM_PREFIX).is_some_and(|n| crate::theme_engine::custom(n).is_some());
        if !known {
            self.hud_palette = defaults.hud_palette.clone();
        }
        self.scale = if self.scale.is_finite() { (self.scale * 100.0).round() / 100.0 } else { 1.0 };
        self.scale = self.scale.clamp(MIN_SCALE, MAX_SCALE);
        self.opacity = self.opacity.clamp(MIN_OPACITY, 100);
        let round = |x: f32, lo: f32, hi: f32| if x.is_finite() { ((x * 100.0).round() / 100.0).clamp(lo, hi) } else { 1.0 };
        self.orb_scale = round(self.orb_scale, MIN_ORB_SCALE, MAX_ORB_SCALE);
        self.font_scale = round(self.font_scale, MIN_FONT_SCALE, MAX_FONT_SCALE);
        if parse_hex(&self.accent).is_none() {
            self.accent = defaults.accent;
        }
        match parse_hex(&self.background) {
            Some(rgb) => self.background = hex(dark_glass(rgb)),
            None => self.background = defaults.background.clone(),
        }
        self.corner_rounding = if self.corner_rounding.is_finite() { self.corner_rounding.round().clamp(MIN_ROUNDING, MAX_ROUNDING) } else { 18.0 };
        self.orb_size_px = if self.orb_size_px.is_finite() { self.orb_size_px.round().clamp(MIN_ORB_PX, MAX_ORB_PX) } else { 260.0 };
        self.orb_opacity = if self.orb_opacity.is_finite() { ((self.orb_opacity * 100.0).round() / 100.0).clamp(0.15, 1.0) } else { 1.0 };
        self.orb_anchor = anchor_id(&self.orb_anchor).unwrap_or("bottom-right").to_string();
        self.orb_monitor = self.orb_monitor.min(9);
        self.title = self.title.trim().chars().take(32).collect();
        if self.title.is_empty() {
            self.title = defaults.title;
        }
        let mut panels: Vec<String> = Vec::new();
        for id in self.hidden_panels.drain(..) {
            if HudPanel::from_id(&id).is_some() && !panels.contains(&id) {
                panels.push(id);
            }
        }
        self.hidden_panels = panels;
    }

    pub fn accent_rgb(&self) -> [u8; 3] {
        parse_hex(&self.accent).unwrap_or([0, 229, 255])
    }

    pub fn background_rgb(&self) -> [u8; 3] {
        parse_hex(&self.background).unwrap_or([3, 7, 14])
    }

    fn is_hidden(&self, panel: HudPanel) -> bool {
        self.hidden_panels.iter().any(|id| id == panel.id())
    }

    /// Whether `panel` is on screen with the current layout (the panel model `configure_interface`
    /// edits; the HUD board itself has no panels).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn shows(&self, panel: HudPanel) -> bool {
        match self.layout {
            HudLayout::Orb => panel == HudPanel::Reactor,
            HudLayout::Compact => panel.in_compact() && !self.is_hidden(panel),
            HudLayout::Full => !self.is_hidden(panel),
        }
    }

    fn hide(&mut self, panel: HudPanel) {
        if !self.is_hidden(panel) {
            self.hidden_panels.push(panel.id().to_string());
        }
    }

    /// Un-hides `panel`, widening the layout when it has no room for it.
    fn show(&mut self, panel: HudPanel) {
        self.hidden_panels.retain(|id| id != panel.id());
        let fits = match self.layout {
            HudLayout::Full => true,
            HudLayout::Compact => panel.in_compact(),
            HudLayout::Orb => panel == HudPanel::Reactor,
        };
        if !fits {
            self.layout = if panel.in_compact() { HudLayout::Compact } else { HudLayout::Full };
        }
    }

    pub fn describe(&self) -> String {
        let hidden: Vec<&str> = HudPanel::ALL.iter().filter(|p| self.is_hidden(**p)).map(|p| p.id()).collect();
        let on_off = |b: bool| if b { "on" } else { "off" };
        format!(
            "theme {}, accent {}, background {} (darkness {:.0} %), zoom {:.0} %, window opacity {} %, layout {}, hidden panels {}, \
             always on top {}, fullscreen {}, frameless {}, animations {}, title \"{}\", zen mode {}, orb size {:.0} %, font size {:.0} %, \
             corner rounding {:.0}, pure orb mode {} ({:.0} px, {}, monitor {}, opacity {:.0} %)",
            self.theme,
            self.accent,
            self.background,
            background_darkness(self.background_rgb()) * 100.0,
            self.scale * 100.0,
            self.opacity,
            self.layout.id(),
            if hidden.is_empty() { "none".to_string() } else { hidden.join(", ") },
            on_off(self.always_on_top),
            on_off(self.fullscreen),
            on_off(self.frameless),
            on_off(self.animations),
            self.title,
            on_off(self.zen_mode),
            self.orb_scale * 100.0,
            self.font_scale * 100.0,
            self.corner_rounding,
            on_off(self.orb_mode),
            self.orb_size_px,
            self.orb_anchor,
            if self.orb_monitor == 0 { "current".to_string() } else { self.orb_monitor.to_string() },
            self.orb_opacity * 100.0,
        )
    }

    /// The live layout state of these settings (the window size comes from the open window).
    pub fn layout_state(&self, window_size: (f32, f32)) -> UiLayoutState {
        UiLayoutState {
            show_audio_devices_panel: !self.is_hidden(HudPanel::Devices),
            show_skills_panel: !self.is_hidden(HudPanel::Skills),
            show_quick_pills_bar: !self.is_hidden(HudPanel::Pills),
            show_telemetry_sidebar: HudPanel::TELEMETRY.iter().any(|p| !self.is_hidden(*p)),
            show_orb_panel: !self.is_hidden(HudPanel::Reactor),
            minimal_zen_mode: self.zen_mode,
            orb_radius_scale: self.orb_scale,
            ui_font_scale: self.font_scale,
            window_size,
        }
    }

    /// Writes the switches of a layout state that differ from these settings back into them (an
    /// unchanged switch leaves Sir's individual panel choices alone; the window size is a window
    /// command, not a setting).
    pub fn apply_layout_state(&mut self, state: &UiLayoutState) {
        let current = self.layout_state(state.window_size);
        let switches = [
            (&[HudPanel::Devices][..], current.show_audio_devices_panel, state.show_audio_devices_panel),
            (&[HudPanel::Skills][..], current.show_skills_panel, state.show_skills_panel),
            (&[HudPanel::Pills][..], current.show_quick_pills_bar, state.show_quick_pills_bar),
            (&HudPanel::TELEMETRY[..], current.show_telemetry_sidebar, state.show_telemetry_sidebar),
            (&[HudPanel::Reactor][..], current.show_orb_panel, state.show_orb_panel),
        ];
        for (panels, was, now) in switches {
            if was != now {
                for p in panels {
                    if now {
                        self.show(*p);
                    } else {
                        self.hide(*p);
                    }
                }
            }
        }
        self.zen_mode = state.minimal_zen_mode;
        self.orb_scale = state.orb_radius_scale;
        self.font_scale = state.ui_font_scale;
        self.sanitize();
    }

}

// ----------------------------------------------------------------------- live layout ----

const MIN_ORB_SCALE: f32 = 0.4;
const MAX_ORB_SCALE: f32 = 2.5;
const MIN_FONT_SCALE: f32 = 0.75;
const MAX_FONT_SCALE: f32 = 1.6;
/// Relative step for "make the orb bigger" / "bigger text".
const LAYOUT_STEP: f32 = 1.2;

/// Every panel and element of the HUD that Jarvis (and Sir, through the HUD's own controls) can
/// change while it runs. The panel switches and scales persist in `interface`; the window size
/// is whatever the open window measures. The HUD publishes the live state each frame
/// ([`publish_layout`]), so `control_jarvis_interface` reports what is really on screen.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct UiLayoutState {
    pub show_audio_devices_panel: bool,
    pub show_skills_panel: bool,
    pub show_quick_pills_bar: bool,
    pub show_telemetry_sidebar: bool,
    pub show_orb_panel: bool,
    /// Hides every sidebar, the widgets and the pills: only the 3D sphere orb and a clean chat.
    pub minimal_zen_mode: bool,
    /// 0.4-2.5, by voice, the orb's slider or the mouse wheel over the orb.
    pub orb_radius_scale: f32,
    /// 0.8-1.6.
    pub ui_font_scale: f32,
    /// Logical points (width, height) of the HUD window; (0, 0) while it is closed.
    pub window_size: (f32, f32),
}

impl Default for UiLayoutState {
    fn default() -> Self {
        InterfaceSettings::default().layout_state((0.0, 0.0))
    }
}

static UI_LAYOUT: std::sync::LazyLock<std::sync::Arc<std::sync::Mutex<UiLayoutState>>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(std::sync::Mutex::new(UiLayoutState::default())));

/// The shared live layout state (written by the HUD every frame).
pub fn ui_layout() -> std::sync::Arc<std::sync::Mutex<UiLayoutState>> {
    std::sync::Arc::clone(&UI_LAYOUT)
}

/// The HUD reports what it shows.
pub fn publish_layout(state: UiLayoutState) {
    *UI_LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = state;
}

fn live_window_size() -> (f32, f32) {
    ui_layout().lock().unwrap_or_else(|e| e.into_inner()).window_size
}

/// The result of `control_jarvis_interface`.
pub struct Controlled {
    pub settings: InterfaceSettings,
    /// A new inner size for the HUD window (logical points).
    pub window_size: Option<(f32, f32)>,
    pub summary: String,
}

/// Panel names (with groups) of a `target_panel` argument: a string, a list or "a and b".
fn target_panels(args: &Value) -> Vec<HudPanel> {
    let mut out: Vec<HudPanel> = Vec::new();
    for name in list_arg(args, "target_panel").into_iter().chain(list_arg(args, "target_panels")).chain(list_arg(args, "panels")) {
        let name = name.replace('_', " ");
        let found = match name.trim() {
            "orb" | "sphere" | "3d orb" | "3d sphere" | "globe" => vec![HudPanel::Reactor],
            "tool log" | "tool logs" | "tools log" => vec![HudPanel::Tools],
            "microphone" | "mic" | "vad" => vec![HudPanel::Audio],
            other => HudPanel::parse(other),
        };
        for p in found {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

fn scale_arg(args: &Value) -> Option<f32> {
    let v = args.get("scale_value").or_else(|| args.get("scale")).or_else(|| args.get("value")).filter(|v| !v.is_null())?;
    let (x, relative) = number_like(v)?;
    // 150 → 150 %, 1.5 → 150 %.
    let factor = if x.abs() > 3.0 { x / 100.0 } else { x };
    Some(if relative { 1.0 + factor } else { factor })
}

fn labels(panels: &[HudPanel]) -> String {
    let telemetry = HudPanel::TELEMETRY.iter().all(|p| panels.contains(p));
    let mut names: Vec<&str> = panels.iter().filter(|p| !(telemetry && HudPanel::TELEMETRY.contains(p))).map(|p| p.label()).collect();
    if telemetry {
        names.push("secondary telemetry panels");
    }
    join_labels(&names)
}

/// Applies `control_jarvis_interface(action, target_panel, scale_value)` to `current`.
pub fn control(current: &InterfaceSettings, args: &Value) -> Result<Controlled, String> {
    let action = str_arg(args, &["action", "command"]).unwrap_or("status").to_lowercase().replace([' ', '-'], "_");
    let mut s = current.clone();
    let mut window_size = None;
    let panels = target_panels(args);
    let need_panels = || if panels.is_empty() { Err(format!("'{action}' needs target_panel (audio_devices, skills, quick_pills, telemetry, orb, memory, tool_log, brain, widgets, chat).")) } else { Ok(()) };
    let done = match action.as_str() {
        "declutter" | "less_full" | "clean_up" | "cleanup" | "tidy" | "simplify" | "optimize" | "optimise" => {
            // The executive decision for "make the interface less full": the chat, the orb, the
            // widgets and the pills stay; everything secondary folds away.
            for p in [HudPanel::Devices, HudPanel::Skills, HudPanel::Memory].into_iter().chain(HudPanel::TELEMETRY) {
                s.hide(p);
            }
            for p in [HudPanel::Chat, HudPanel::Reactor, HudPanel::Header] {
                s.show(p);
            }
            s.zen_mode = false;
            "decluttered: audio devices, skills, memory and the secondary telemetry are collapsed; chat, orb and widgets stay".to_string()
        }
        "hide_panel" | "hide" | "remove" | "disable" | "collapse" => {
            need_panels()?;
            if panels.len() == HudPanel::ALL.len() {
                s.zen_mode = true;
            } else {
                panels.iter().for_each(|p| s.hide(*p));
            }
            format!("hid the {}", labels(&panels))
        }
        "show_panel" | "show" | "restore" | "enable" | "bring_back" => {
            need_panels()?;
            panels.iter().for_each(|p| s.show(*p));
            if panels.iter().any(|p| !matches!(p, HudPanel::Header | HudPanel::Reactor | HudPanel::Chat)) {
                s.zen_mode = false;
            }
            format!("showing the {}", labels(&panels))
        }
        "toggle_panel" | "toggle" => {
            need_panels()?;
            for p in &panels {
                if s.is_hidden(*p) {
                    s.show(*p);
                } else {
                    s.hide(*p);
                }
            }
            format!("toggled the {}", labels(&panels))
        }
        "set_panels" => {
            let hide = target_panels(&json!({ "target_panel": args.get("hide").cloned().unwrap_or(Value::Null) }));
            let show = target_panels(&json!({ "target_panel": args.get("show").cloned().unwrap_or(Value::Null) }));
            if hide.is_empty() && show.is_empty() {
                return Err("set_panels needs 'hide' and/or 'show' panel lists.".into());
            }
            hide.iter().for_each(|p| s.hide(*p));
            show.iter().for_each(|p| s.show(*p));
            if !show.is_empty() {
                s.zen_mode = false;
            }
            [(!hide.is_empty()).then(|| format!("hid the {}", labels(&hide))), (!show.is_empty()).then(|| format!("showing the {}", labels(&show)))]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; ")
        }
        "zen_mode_on" | "zen_on" | "zen_mode" | "zen" | "minimal_zen_mode" => {
            s.zen_mode = true;
            if s.layout == HudLayout::Orb {
                s.layout = HudLayout::Full;
            }
            "zen mode on: only the orb and the chat".to_string()
        }
        "zen_mode_off" | "zen_off" | "exit_zen" => {
            s.zen_mode = false;
            "zen mode off".to_string()
        }
        "toggle_zen" => {
            s.zen_mode = !s.zen_mode;
            format!("zen mode {}", if s.zen_mode { "on" } else { "off" })
        }
        "restore_all" | "show_all" | "reset_layout" | "everything" => {
            s.hidden_panels.clear();
            s.zen_mode = false;
            s.layout = HudLayout::Full;
            "every panel is back".to_string()
        }
        "orb_bigger" | "orb_larger" | "bigger_orb" => {
            s.orb_scale *= LAYOUT_STEP;
            s.show(HudPanel::Reactor);
            "orb enlarged".to_string()
        }
        "orb_smaller" | "smaller_orb" => {
            s.orb_scale /= LAYOUT_STEP;
            "orb reduced".to_string()
        }
        "set_orb_scale" | "orb_size" | "orb_scale" => {
            s.orb_scale = scale_arg(args).ok_or("set_orb_scale needs scale_value (0.4-2.5, or a percentage).")?;
            s.show(HudPanel::Reactor);
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
        other => {
            return Err(format!(
                "Unknown action '{other}' (declutter, hide_panel, show_panel, toggle_panel, set_panels, zen_mode_on, zen_mode_off, restore_all, \
                 orb_bigger, orb_smaller, set_orb_scale, font_bigger, font_smaller, set_font_scale, set_window_size, status)."
            ))
        }
    };
    s.sanitize();
    let state = s.layout_state(window_size.unwrap_or_else(live_window_size));
    let summary = format!(
        "Interface layout: {done}. Now: audio devices {}, skills {}, quick pills {}, telemetry {}, orb {}, zen mode {}, orb size {:.0} %, font size {:.0} %, window {:.0} × {:.0}.",
        on_off(state.show_audio_devices_panel),
        on_off(state.show_skills_panel),
        on_off(state.show_quick_pills_bar),
        on_off(state.show_telemetry_sidebar),
        on_off(state.show_orb_panel),
        on_off(state.minimal_zen_mode),
        state.orb_radius_scale * 100.0,
        state.ui_font_scale * 100.0,
        state.window_size.0,
        state.window_size.1,
    );
    Ok(Controlled { settings: s, window_size, summary })
}

fn is_plural(panels: &[HudPanel]) -> bool {
    let names = labels(panels);
    panels.len() > 1 || (names.ends_with('s') && !names.ends_with("ss"))
}

fn on_off(b: bool) -> &'static str {
    if b { "shown" } else { "hidden" }
}

/// In-character confirmation of `control_jarvis_interface`, from the request and the settings it
/// produced.
pub fn control_ack(args: &Value, now: &InterfaceSettings) -> String {
    let action = str_arg(args, &["action", "command"]).unwrap_or("status").to_lowercase().replace([' ', '-'], "_");
    let panels = target_panels(args);
    let names = labels(&panels);
    // "The audio devices are back", "the skills panel is back".
    let plural = is_plural(&panels);
    match action.as_str() {
        "declutter" | "less_full" | "clean_up" | "cleanup" | "tidy" | "simplify" | "optimize" | "optimise" => {
            "Right away, Sir. I have collapsed the Audio Devices, Skills and secondary telemetry panels to give you a clean workspace.".into()
        }
        "hide_panel" | "hide" | "remove" | "disable" | "collapse" if now.zen_mode && panels.len() == HudPanel::ALL.len() => {
            "Zen mode, Sir. Just the orb and our conversation.".into()
        }
        "hide_panel" | "hide" | "remove" | "disable" | "collapse" => {
            format!("Done, Sir. I've removed the {names}; {} gone from the interface.", if plural { "they're" } else { "it's" })
        }
        "show_panel" | "show" | "restore" | "enable" | "bring_back" => format!("There you are, Sir. The {names} {} back.", if plural { "are" } else { "is" }),
        "toggle_panel" | "toggle" => "Done, Sir.".into(),
        "set_panels" => {
            let hide = target_panels(&json!({ "target_panel": args.get("hide").cloned().unwrap_or(Value::Null) }));
            let show = target_panels(&json!({ "target_panel": args.get("show").cloned().unwrap_or(Value::Null) }));
            match (hide.is_empty(), show.is_empty()) {
                (false, true) => format!("Done, Sir. I've removed the {}.", labels(&hide)),
                (true, false) => format!("There you are, Sir. The {} {} back.", labels(&show), if is_plural(&show) { "are" } else { "is" }),
                _ => format!("Done, Sir. The {} {} gone and the {} back.", labels(&hide), if is_plural(&hide) { "are" } else { "is" }, labels(&show)),
            }
        }
        "zen_mode_on" | "zen_on" | "zen_mode" | "zen" | "minimal_zen_mode" => "Zen mode, Sir. Just the orb and our conversation.".into(),
        "zen_mode_off" | "zen_off" | "exit_zen" => "Very good, Sir. The full workspace is back.".into(),
        "toggle_zen" => if now.zen_mode { "Zen mode, Sir.".into() } else { "The full workspace is back, Sir.".into() },
        "restore_all" | "show_all" | "reset_layout" | "everything" => "There you are, Sir. Every panel is back.".into(),
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

fn rx(pattern: &str) -> regex::Regex {
    regex::Regex::new(pattern).expect("valid layout regex")
}

/// Goals rather than steps: "make the interface less full", "clean up the UI", "declutter",
/// "optimise my layout", "mach das Interface übersichtlicher".
static DECLUTTER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    rx(r"\b(?:less (?:full|cluttered|busy|crowded|messy)|(?:too|so) (?:full|cluttered|busy|crowded|messy)|declutter|de clutter|unclutter|(?:clean|tidy|neaten|sort) (?:up )?(?:the |my |your |this )?(?:overall )?(?:ui|interface|hud|layout|screen|workspace|gui|display|window)|(?:the |my |your |this )?(?:overall )?(?:ui|interface|hud|layout|gui|workspace|display) (?:cleaner|tidier|simpler|less full|less cluttered|less busy|clean|tidy|simple|minimal|minimalistic)|optimi[sz]e (?:the |my |your )?(?:ui|interface|hud|layout|workspace|screen)|übersichtlicher|aufgeräumter|weniger voll|räum (?:das |dein |die )?(?:interface|oberfläche|hud|layout) auf)\b")
});
static ZEN_ON: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    rx(r"\b(?:zen mode|zen modus|zen layout|go zen|(?:only|just) (?:the )?(?:chat|conversation) and (?:the )?(?:orb|sphere|reactor)|(?:only|just) (?:the )?(?:orb|sphere|reactor) and (?:the )?(?:chat|conversation)|nur (?:noch )?(?:den )?(?:chat|orb) und (?:den )?(?:orb|chat))\b")
});
static ZEN_OFF: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    rx(r"\b(?:(?:exit|leave|end|stop|disable|turn off|quit|beende|verlasse?) (?:the )?zen(?: mode| modus)?|zen(?: mode| modus)? (?:off|aus))\b")
});
/// "Hide the panels", "show the panels": all of them at once, which is what zen mode is.
static PANELS_OFF: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    rx(r"^(?:hide|close|remove|clear|get rid of|turn off|switch off|versteck|verstecke|schließ|schließe|blende)\s+(?:all\s+)?(?:(?:of\s+)?(?:the|my|your|alle|die|deine)\s+)?panels(?:\s+aus)?$")
});
static PANELS_ON: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    rx(r"^(?:show|bring back|give me back|turn on|switch on|zeig|zeige|blende)\s+(?:me\s+)?(?:(?:the|my|your|die|deine)\s+)?panels(?:\s+(?:again|back|ein|wieder))*$")
});
static RESTORE_ALL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    rx(r"^(?:(?:show|bring back|restore|give me back|enable) (?:me )?(?:all|every|everything)(?: (?:the|your|my))? ?(?:panels?|tabs?|sidebars?|of the panels)?(?: again| back)?|(?:show|restore) (?:the )?full (?:workspace|layout|interface|hud)|alle (?:panels|reiter|fenster) (?:wieder )?(?:anzeigen|einblenden|zeigen))$")
});
static ORB_SIZE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    rx(r"\b(orb|sphere|reactor|globe|kugel|3d core)\b.*?\b(bigger|larger|smaller|tinier|grow|shrink|größer|groesser|kleiner|(\d{2,3})\s*(?:%|percent|prozent))|\b(bigger|larger|smaller|enlarge|shrink|grow|vergrößere|verkleinere)\s+(?:the\s+|my\s+|your\s+|die\s+|den\s+)?(?:3d\s+)?(orb|sphere|reactor|globe|kugel)\b")
});
/// Verbs that hide a panel, show one, or keep one as it is ("just leave the normal chat window").
static PANEL_VERBS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    rx(r"\b(remove|hide|disable|close|collapse|get rid of|turn off|switch off|take away|take out|drop|lose|kill|deactivate|entferne|entfern|versteck|verstecke|verberge|deaktiviere|deaktivier|schließe|schliesse|blende|weg mit|bring back|show|restore|enable|re enable|reenable|unhide|turn on|switch on|add back|put back|expand|reveal|reopen|open|give me back|zeig|zeige|aktiviere|einblenden|leave|keep|lass|behalte)\b")
});
/// Panel names as Sir says them (longest first, so "audio devices tab" wins over "audio").
const PANEL_PHRASES: &[(&str, &[&str])] = &[
    ("audio devices tab", &["devices"]), ("audio devices panel", &["devices"]), ("audio device panel", &["devices"]), ("audio devices", &["devices"]),
    ("audio device", &["devices"]), ("audio settings", &["devices"]), ("sound settings", &["devices"]), ("speaker settings", &["devices"]),
    ("audio panel", &["devices"]), ("audio tab", &["devices"]), ("devices tab", &["devices"]), ("devices panel", &["devices"]), ("audiogeräte", &["devices"]),
    ("audio geräte", &["devices"]), ("geräte", &["devices"]),
    ("microphone panel", &["audio"]), ("microphone tab", &["audio"]), ("mic panel", &["audio"]), ("vad panel", &["audio"]), ("mikrofon panel", &["audio"]),
    ("skills tab", &["skills"]), ("skills panel", &["skills"]), ("skill tab", &["skills"]), ("skill panel", &["skills"]), ("skills", &["skills"]), ("fähigkeiten", &["skills"]),
    ("telemetry sidebar", &["system", "audio", "tools", "brain"]), ("telemetry panels", &["system", "audio", "tools", "brain"]),
    ("telemetry", &["system", "audio", "tools", "brain"]), ("telemetrie", &["system", "audio", "tools", "brain"]),
    ("system monitor", &["system"]), ("system panel", &["system"]), ("system tab", &["system"]), ("system stats", &["system"]), ("systemmonitor", &["system"]),
    ("tool log", &["tools"]), ("tool logs", &["tools"]), ("tools tab", &["tools"]), ("tools panel", &["tools"]), ("activity log", &["tools"]),
    ("brain panel", &["brain"]), ("brain tab", &["brain"]), ("model panel", &["brain"]),
    ("memory panel", &["memory"]), ("memory tab", &["memory"]), ("memories panel", &["memory"]), ("gedächtnis", &["memory"]),
    ("quick pills bar", &["pills"]), ("quick pills", &["pills"]), ("pills bar", &["pills"]), ("pills", &["pills"]), ("quick actions", &["pills"]),
    ("quick action buttons", &["pills"]), ("suggestion buttons", &["pills"]), ("action buttons", &["pills"]),
    ("3d orb", &["reactor"]), ("3d sphere", &["reactor"]), ("orb", &["reactor"]), ("sphere", &["reactor"]), ("reactor", &["reactor"]), ("kugel", &["reactor"]),
    ("widgets panel", &["widgets"]), ("widget panel", &["widgets"]), ("widgets", &["widgets"]),
    ("chat window", &["chat"]), ("chat panel", &["chat"]), ("chat tab", &["chat"]), ("normal chat", &["chat"]), ("conversation panel", &["chat"]),
    ("left sidebar", &["audio", "devices", "system"]), ("right sidebar", &["memory", "skills", "tools", "brain"]),
    ("sidebars", &["audio", "devices", "system", "memory", "skills", "tools", "brain"]), ("side panels", &["audio", "devices", "system", "memory", "skills", "tools", "brain"]),
];

/// Spoken layout commands on the fast path (`s` = the intent parser's normalised sentence):
/// `control_jarvis_interface` arguments for a goal ("less full" → declutter), zen mode, the orb's
/// size, or the panels a sentence hides and shows — several at once and in any order ("just
/// leave the normal chat window, but remove the audio devices tab and the skills tab").
pub fn layout_command(s: &str) -> Option<Value> {
    if ZEN_OFF.is_match(s) || PANELS_ON.is_match(s) {
        return Some(json!({ "action": "zen_mode_off" }));
    }
    if ZEN_ON.is_match(s) || PANELS_OFF.is_match(s) {
        return Some(json!({ "action": "zen_mode_on" }));
    }
    if RESTORE_ALL.is_match(s) {
        return Some(json!({ "action": "restore_all" }));
    }
    if let Some(c) = ORB_SIZE.captures(s) {
        if let Some(pct) = c.get(3).and_then(|m| m.as_str().parse::<f32>().ok()) {
            return Some(json!({ "action": "set_orb_scale", "scale_value": pct / 100.0 }));
        }
        let word = c.get(2).or_else(|| c.get(4)).map(|m| m.as_str()).unwrap_or("");
        let bigger = matches!(word, "bigger" | "larger" | "grow" | "größer" | "groesser" | "enlarge" | "vergrößere");
        return Some(json!({ "action": if bigger { "orb_bigger" } else { "orb_smaller" } }));
    }
    // Panels named with a verb that governs them.
    let verbs: Vec<(usize, &str)> = PANEL_VERBS.find_iter(s).map(|m| (m.start(), m.as_str())).collect();
    if !verbs.is_empty() {
        let mut hide: Vec<&str> = Vec::new();
        let mut show: Vec<&str> = Vec::new();
        let mut taken = vec![false; s.len()];
        for (phrase, ids) in PANEL_PHRASES {
            for (at, _) in s.match_indices(phrase) {
                let end = at + phrase.len();
                let bounded = (at == 0 || !s[..at].ends_with(char::is_alphanumeric)) && !s[end..].starts_with(char::is_alphanumeric);
                if !bounded || taken[at..end].iter().any(|t| *t) {
                    continue;
                }
                taken[at..end].iter_mut().for_each(|t| *t = true);
                // The nearest verb before the name decides; "blende … aus" / "… ausblenden" too.
                let Some((_, verb)) = verbs.iter().rev().find(|(v, _)| *v < at) else { continue };
                let tail = &s[end..];
                let german_off = *verb == "blende" && !tail.trim_start().starts_with("ein");
                let target = match *verb {
                    "leave" | "keep" | "lass" | "behalte" => continue,
                    "bring back" | "show" | "restore" | "enable" | "re enable" | "reenable" | "unhide" | "turn on" | "switch on" | "add back" | "put back"
                    | "expand" | "reveal" | "reopen" | "open" | "give me back" | "zeig" | "zeige" | "aktiviere" | "einblenden" => &mut show,
                    "blende" if !german_off => &mut show,
                    _ => &mut hide,
                };
                for id in ids.iter() {
                    if !target.contains(id) {
                        target.push(id);
                    }
                }
            }
        }
        let name = |id: &str| match id {
            "devices" => "audio_devices",
            "reactor" => "orb",
            "pills" => "quick_pills",
            "tools" => "tool_log",
            "audio" => "microphone",
            other => other,
        }
        .to_string();
        let telemetry = ["system", "audio", "tools", "brain"];
        let names = |ids: &[&str]| -> Vec<String> {
            if telemetry.iter().all(|t| ids.contains(t)) && ids.len() == 4 {
                vec!["telemetry".to_string()]
            } else {
                ids.iter().map(|id| name(id)).collect()
            }
        };
        match (hide.is_empty(), show.is_empty()) {
            (true, true) => {}
            (false, true) => return Some(json!({ "action": "hide_panel", "target_panel": names(&hide) })),
            (true, false) => return Some(json!({ "action": "show_panel", "target_panel": names(&show) })),
            (false, false) => return Some(json!({ "action": "set_panels", "hide": names(&hide), "show": names(&show) })),
        }
    }
    if DECLUTTER.is_match(s) {
        return Some(json!({ "action": "declutter" }));
    }
    None
}

/// The `control_jarvis_interface` tool schema.
pub fn control_tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "control_jarvis_interface",
        "description": "Change YOUR OWN HUD layout live: hide or show panels (\"remove the audio devices tab and the skills tab\" → hide_panel [audio_devices, skills]; \"bring back the audio settings\" → show_panel audio_devices), declutter (\"make the interface less full\", \"clean up the UI\": decide yourself, no questions), zen mode (only the 3D orb and the chat), the orb's size, the text size or the window size.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["declutter", "hide_panel", "show_panel", "toggle_panel", "zen_mode_on", "zen_mode_off", "restore_all", "orb_bigger", "orb_smaller", "set_orb_scale", "font_bigger", "font_smaller", "set_font_scale", "set_window_size", "status"]},
            "target_panel": {"type": "array", "items": {"type": "string", "enum": ["audio_devices", "skills", "quick_pills", "telemetry", "orb", "memory", "tool_log", "brain", "microphone", "system", "widgets", "chat", "header"]}},
            "scale_value": {"type": "number", "description": "Orb 0.4-2.5, font 0.8-1.6, window factor; or a percentage"},
            "width": {"type": "number"}, "height": {"type": "number"}
        }, "required": ["action"]}
    }})
}

// ------------------------------------------------------------------------------ themes ----

pub struct Theme {
    pub name: &'static str,
    pub display: &'static str,
    pub accent: &'static str,
    pub background: &'static str,
    /// Short description for the tool schema.
    hint: &'static str,
    aliases: &'static [&'static str],
}

pub const THEMES: &[Theme] = &[
    Theme {
        name: "stark",
        display: "Arc Cyan",
        accent: "#00e5ff",
        // Deep obsidian glass.
        background: "#070b12",
        hint: "cyan, the default",
        aliases: &["jarvis", "default", "standard", "classic", "original", "arc reactor", "stark industries", "normal", "stark cyan", "cyan", "arc cyan", "arc_cyan"],
    },
    Theme {
        name: "mark42",
        display: "Mark 42",
        accent: "#ffc233",
        background: "#170507",
        hint: "red and gold",
        aliases: &[
            "mark 42", "mark xlii", "iron man", "ironman", "red and gold", "red gold", "rot gold", "hot rod", "gold and red", "gold red",
            "iron man gold and red", "iron man gold & red", "iron man red and gold", "iron man gold red",
        ],
    },
    Theme {
        name: "ultron",
        display: "Ultron",
        accent: "#ff3b3b",
        background: "#0d0204",
        hint: "menacing red",
        aliases: &["evil", "danger", "red alert", "alarm", "ultron crimson"],
    },
    Theme {
        name: "stealth",
        display: "Stealth",
        accent: "#a9b8c8",
        background: "#050608",
        hint: "dark grey",
        aliases: &[
            "dark", "darker", "dunkel", "dunkler", "dunkle", "night", "nacht", "black", "schwarz", "monochrome", "grey", "gray", "grau",
            "minimal monochrome", "monochrome minimal", "monochrom", "stealth obsidian", "obsidian", "stealth_obsidian",
        ],
    },
    Theme {
        name: "emerald",
        display: "Emerald",
        accent: "#22f59a",
        background: "#020b06",
        hint: "green",
        aliases: &["matrix", "hacker", "hulk", "smaragd", "stealth matrix", "matrix green", "emerald matrix", "emerald_matrix"],
    },
    Theme {
        name: "amethyst",
        display: "Amethyst",
        accent: "#b48cff",
        background: "#08040f",
        hint: "purple",
        aliases: &["vision", "purple haze", "amethyst purple"],
    },
    Theme {
        name: "arctic",
        display: "Arctic",
        accent: "#d2f1ff",
        background: "#05090f",
        hint: "ice white",
        aliases: &["ice", "frost", "snow", "eis", "polar", "light", "white mode", "bright"],
    },
    Theme {
        name: "midnight",
        display: "Midnight",
        accent: "#4f8cff",
        background: "#030616",
        hint: "deep blue",
        aliases: &["ocean", "deep blue", "mitternacht"],
    },
    Theme {
        name: "sunset",
        display: "Sunset",
        accent: "#ff8a3d",
        background: "#130804",
        hint: "orange",
        aliases: &["hulkbuster", "sonnenuntergang", "ember"],
    },
    Theme {
        name: "stark_gold",
        display: "Stark Gold",
        accent: "#ffb800",
        background: "#0b0905",
        hint: "Stark gold on obsidian",
        aliases: &["stark gold", "gold", "golden", "gold theme", "stark industries gold"],
    },
    Theme {
        name: "crimson_mk42",
        display: "Crimson MK42",
        accent: "#ff2a45",
        background: "#0f0507",
        hint: "crimson on dark glass",
        aliases: &["crimson", "crimson mk42", "crimson mark 42", "mk42 crimson", "crimson red"],
    },
];

pub fn theme(name: &str) -> Option<&'static Theme> {
    let n = words(name);
    let compact = n.replace(' ', "");
    THEMES
        .iter()
        .find(|t| t.name == compact || t.display.to_lowercase() == n || t.aliases.contains(&n.as_str()))
}

fn theme_list() -> String {
    THEMES.iter().map(|t| format!("{} ({})", t.name, t.hint)).collect::<Vec<_>>().join(", ")
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

/// Background colours, always as dark glass: a colour name becomes a deep tint ("red background"
/// = obsidian with a red cast), a light name or value the lightest slate glass, never a light or
/// neon window.
pub fn parse_background(input: &str) -> Option<[u8; 3]> {
    let w = words(input);
    match w.as_str() {
        "darker" | "dark" | "deeper" | "dunkler" | "dunkel" | "black" | "schwarz" | "obsidian" => return Some(DEEPEST_BACKGROUND),
        "lighter" | "light" | "brighter" | "heller" | "hell" | "white" | "weiß" | "weiss" | "slate" => return Some(LIGHTEST_BACKGROUND),
        _ => {}
    }
    if let Some(rgb) = parse_hex(input).or_else(|| parse_rgb_triplet(input)) {
        return Some(dark_glass(rgb));
    }
    let rgb = parse_color(input)?;
    Some(if luminance(rgb) > 0.62 { LIGHTEST_BACKGROUND } else { dark_glass(scale_rgb(rgb, 0.14)) })
}

/// The colour as Jarvis says it: always English ("dunkelblau" → "dark blue").
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

/// Strings, or one string with commas / "and" ("memory and skills").
fn list_arg(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(s)) => s
            .replace(" and ", ",")
            .replace(" und ", ",")
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Applies `configure_interface` arguments to `current`. Fields that cannot be understood are
/// reported back instead of failing the whole call; nothing understood at all is an error.
/// A saved theme on the whole HUD: palette, accent and background tint.
fn apply_custom_theme(s: &mut InterfaceSettings, t: &crate::theme_engine::CustomTheme, done: &mut Vec<String>, verb: &str) {
    s.hud_palette = t.key();
    s.accent = hex(t.core_primary);
    s.background = hex(t.background_tint);
    s.theme = "custom".into();
    done.push(format!("theme '{}' {verb}", t.name));
}

pub fn configure(current: &InterfaceSettings, args: &Value) -> Result<Configured, String> {
    let reset = bool_arg(args, "reset") == Some(true);
    let mut s = if reset { InterfaceSettings::default() } else { current.clone() };
    let mut done: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    if reset {
        done.push("reset to the default look".into());
    }

    if let Some(raw) = str_arg(args, &["ui_paradigm", "design", "ui_design", "look"]) {
        match crate::oled_minimal_ui::UiDesignParadigm::parse(raw) {
            Some(p) => {
                s.ui_paradigm = p;
                done.push(format!("design {}", p.id()));
            }
            None => failed.push(format!("unknown design '{raw}' (modern_oled_minimal, classic_scifi_hud)")),
        }
    }
    if let Some(raw) = str_arg(args, &["accent_preset"]) {
        match crate::oled_minimal_ui::accent_preset(raw) {
            Some((name, hex_code)) => {
                s.accent = hex_code.to_string();
                s.theme = "custom".into();
                done.push(format!("accent {name}"));
            }
            None => failed.push(format!("unknown accent '{raw}' (Ice White, Electric Blue, Stark Gold, Emerald, Violet, Crimson, Sunset Orange)")),
        }
    }
    if let Some(name) = str_arg(args, &["theme", "preset", "style"]) {
        if let Some(t) = theme(name) {
            s.theme = t.name.to_string();
            s.accent = t.accent.to_string();
            s.background = t.background.to_string();
            // The HUD's palette follows: the preset the word names, else the theme's accent.
            if let Some(rgb) = parse_color(t.accent) {
                s.hud_palette = crate::theme_engine::palette_name_for(name, rgb);
            }
            done.push(format!("theme {}", t.display));
        } else if let Some(layout) = HudLayout::parse(name) {
            s.layout = layout;
            done.push(format!("layout {}", layout.id()));
        } else if let Some(rgb) = parse_color(name) {
            s.accent = hex(rgb);
            s.theme = "custom".into();
            s.hud_palette = crate::theme_engine::palette_name_for(name, rgb);
            done.push(format!("accent colour {name}"));
        } else {
            failed.push(format!("unknown theme '{name}' (themes: {})", theme_list()));
        }
    }
    if let Some(c) = str_arg(args, &["accent_color", "accent_colour", "accent", "color", "colour", "highlight_color"]) {
        match parse_color(c) {
            // A near-black highlight would be invisible: Sir wants a black interface.
            Some(rgb) if luminance(rgb) < 0.08 => {
                s.background = hex(rgb);
                s.theme = "custom".into();
                done.push(format!("background {c}"));
            }
            Some(rgb) => {
                s.accent = hex(rgb);
                s.theme = "custom".into();
                s.hud_palette = crate::theme_engine::palette_name_for(c, rgb);
                done.push(format!("accent colour {c}"));
            }
            None => failed.push(format!("unknown colour '{c}'")),
        }
    }
    if let Some(c) = str_arg(args, &["background_color", "background_colour", "background", "backdrop"]) {
        // "Darker" / "lighter" are relative: a deeper or a shallower dark glass.
        let relative = match words(c).as_str() {
            "darker" | "deeper" | "dunkler" | "blacker" | "more dark" => Some(0.25),
            "lighter" | "brighter" | "heller" | "less dark" => Some(-0.25),
            _ => None,
        };
        match relative.map(|step| with_darkness(s.background_rgb(), background_darkness(s.background_rgb()) + step)).or_else(|| parse_background(c)) {
            Some(rgb) => {
                s.background = hex(rgb);
                done.push(format!("background {} (darkness {:.0} %)", hex(rgb), background_darkness(rgb) * 100.0));
            }
            None => failed.push(format!("unknown background colour '{c}'")),
        }
    }
    // "Make it darker": +0.15 deeper, -0.15 shallower (always within dark glass).
    if let Some((step, _)) = args.get("background_shade").filter(|v| !v.is_null()).and_then(number_like) {
        let before = background_darkness(s.background_rgb());
        let rgb = with_darkness(s.background_rgb(), before + step);
        s.background = hex(rgb);
        let after = background_darkness(rgb);
        if (after - before).abs() < 0.01 {
            done.push(format!("background already at its {} (darkness {:.0} %)", if step > 0.0 { "deepest" } else { "lightest dark glass" }, after * 100.0));
        } else {
            done.push(format!("background {} (darkness {:.0} %)", if step > 0.0 { "deeper" } else { "lighter" }, after * 100.0));
        }
    }
    if let Some((d, _)) = args.get("background_darkness").filter(|v| !v.is_null()).and_then(number_like) {
        let d = if d > 1.0 { d / 100.0 } else { d };
        let rgb = with_darkness(s.background_rgb(), d);
        s.background = hex(rgb);
        done.push(format!("background darkness {:.0} %", background_darkness(rgb) * 100.0));
    }
    if let Some((r, relative)) = args.get("corner_rounding").or_else(|| args.get("rounding")).filter(|v| !v.is_null()).and_then(number_like) {
        s.corner_rounding = (if relative { s.corner_rounding + r } else { r }).clamp(MIN_ROUNDING, MAX_ROUNDING);
        done.push(format!("corner rounding {:.0}", s.corner_rounding));
    }
    if let Some(on) = bool_arg(args, "header_telemetry").or_else(|| bool_arg(args, "show_header_telemetry")) {
        s.header_telemetry = on;
        done.push(format!("header telemetry {}", if on { "on" } else { "off" }));
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
    if let Some(on) = bool_arg(args, "zen_mode").or_else(|| bool_arg(args, "minimal_zen_mode")) {
        s.zen_mode = on;
        done.push(format!("zen mode {}", if on { "on" } else { "off" }));
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
        } else if let Some(t) = theme(name) {
            s.theme = t.name.to_string();
            s.accent = t.accent.to_string();
            s.background = t.background.to_string();
            done.push(format!("theme {}", t.display));
        } else {
            failed.push(format!("unknown layout '{name}' (full, compact or orb)"));
        }
    }
    for (key, show) in [("hide_panels", false), ("show_panels", true)] {
        for name in list_arg(args, key) {
            let panels = HudPanel::parse(&name);
            if panels.is_empty() {
                failed.push(format!(
                    "unknown panel '{name}' (panels: {})",
                    HudPanel::ALL.iter().map(|p| p.id()).collect::<Vec<_>>().join(", ")
                ));
                continue;
            }
            if show && panels.len() == HudPanel::ALL.len() {
                s.hidden_panels.clear();
                s.layout = HudLayout::Full;
            } else if !show && panels.len() == HudPanel::ALL.len() {
                // "Hide everything": only the reactor stays.
                s.layout = HudLayout::Orb;
            } else {
                for p in panels {
                    if show {
                        s.show(p);
                    } else {
                        s.hide(p);
                    }
                }
            }
            done.push(format!("{} {name}", if show { "showing" } else { "hiding" }));
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
        ("sound cues", bool_arg(args, "sound_cues"), &mut s.sound_cues),
    ];
    for (label, value, field) in flags {
        if let Some(on) = value {
            *field = on;
            done.push(format!("{label} {}", if on { "on" } else { "off" }));
        }
    }
    if let Some(title) = str_arg(args, &["title", "display_name", "name"]) {
        s.title = title.to_string();
        done.push(format!("title \"{title}\""));
    }
    if bool_arg(args, "open_settings") == Some(true) {
        done.push("settings window open".into());
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
    if bool_arg(args, "dock_windows") == Some(true) {
        done.push("every window docked inside the HUD".into());
    }
    if let Some(name) = str_arg(args, &["hud_palette", "palette", "hud_theme"]) {
        match crate::theme_engine::preset(name) {
            Some(p) => {
                s.hud_palette = name.trim().to_lowercase().replace([' ', '-'], "_");
                s.accent = hex(p.core_primary);
                done.push(format!("HUD theme {}", s.hud_palette));
            }
            None => match crate::theme_engine::custom(name.trim().trim_start_matches(crate::theme_engine::CUSTOM_PREFIX)) {
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
        let designed = crate::theme_engine::design_theme(text("name"), description);
        let base = designed.or_else(|| colour("core_primary").or(colour("window_border")).map(|c| crate::theme_engine::CustomTheme::from_palette(text("name").unwrap_or("Custom"), &crate::theme_engine::derive(c), s.background_rgb())));
        match base {
            Some(mut t) => {
                if let Some(n) = text("name") {
                    t.name = n.to_string();
                }
                t.core_primary = colour("core_primary").unwrap_or(t.core_primary);
                t.core_secondary = colour("core_secondary").unwrap_or(t.core_secondary);
                t.window_border = colour("window_border").unwrap_or(t.window_border);
                t.background_tint = text("background_tint").and_then(parse_background).unwrap_or(t.background_tint);
                match crate::theme_engine::save_custom(t.clone()) {
                    Ok(()) => apply_custom_theme(&mut s, &t, &mut done, "created and saved"),
                    Err(e) => failed.push(format!("theme '{}' not saved: {e}", t.name)),
                }
            }
            None => failed.push(format!("no colours or mood in '{description}' to build a theme from")),
        }
    }
    if let Some(name) = str_arg(args, &["save_theme"]) {
        let t = crate::theme_engine::CustomTheme::from_palette(name, &crate::theme_engine::resolve(&s.hud_palette), s.background_rgb());
        match crate::theme_engine::save_custom(t.clone()) {
            Ok(()) => apply_custom_theme(&mut s, &t, &mut done, "saved"),
            Err(e) => failed.push(format!("theme '{name}' not saved: {e}")),
        }
    }
    if let Some(name) = str_arg(args, &["load_theme"]) {
        match crate::theme_engine::custom(name) {
            Some(t) => apply_custom_theme(&mut s, &t, &mut done, "loaded"),
            None => failed.push(format!("no saved theme '{name}' (saved: {})", crate::theme_engine::custom_themes().iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", "))),
        }
    }
    if let Some(name) = str_arg(args, &["detach_window"]) {
        done.push(format!("'{name}' detached into its own window"));
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
            "Nothing to change: pass theme, accent_color, background_color, scale, opacity, layout, show_panels, \
             hide_panels, always_on_top, fullscreen, frameless, animations, title, window or reset."
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
pub fn core_command(args: &Value) -> Option<crate::core_sphere_3d::CoreCommand> {
    use crate::core_sphere_3d::CoreCommand;
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
    let panels = |key: &str| {
        list_arg(args, key)
            .iter()
            .flat_map(|n| HudPanel::parse(n))
            .map(|p| p.label())
            .collect::<Vec<_>>()
    };
    if flag("open_settings") == Some(true) {
        return "There you are, Sir. The interface settings are open.".into();
    }
    if let Some(open) = flag("core_view") {
        return if open { "My core, Sir. Every module I run on, wired as it runs. Drag to orbit it." } else { "Core view hidden, Sir." }.into();
    }
    if flag("dock_windows") == Some(true) {
        return "Every window is back inside the HUD, Sir.".into();
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
    if let Some(name) = text(&["hud_palette", "palette", "hud_theme"]).filter(|n| crate::theme_engine::preset(n).is_some()) {
        return format!("{} it is, Sir.", name.replace(['_', '-'], " ").split_whitespace().map(|w| { let mut c = w.chars(); c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default() }).collect::<Vec<_>>().join(" "));
    }
    if text(&["detach_window"]).is_some() {
        return "Detached into its own window, Sir.".into();
    }
    if args.get("span_all_monitors").is_some_and(|v| !v.is_null()) {
        return if now.span_all_monitors { "One board across every monitor, Sir. I stay on your first screen." } else { "Back on your first monitor, Sir." }.into();
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
            crate::core_sphere_3d::CoreCommand::Spin(f) if f <= 0.0 => "Holding it still, Sir.".into(),
            crate::core_sphere_3d::CoreCommand::Spin(_) => "Spinning it up, Sir.".into(),
            _ => "Scaled, Sir.".into(),
        };
    }
    if let Some(open) = flag("text_console") {
        return if open { "Text console's open, Sir. Type away." } else { "Console closed, Sir." }.into();
    }
    if flag("reset") == Some(true) {
        return "Very good, Sir. The interface is back to its factory look.".into();
    }
    if let Some(p) = text(&["ui_paradigm", "design", "ui_design", "look"]).and_then(crate::oled_minimal_ui::UiDesignParadigm::parse) {
        return format!("Very good, Sir. Switched to {}.", p.spoken());
    }
    if let Some((name, _)) = text(&["accent_preset"]).and_then(crate::oled_minimal_ui::accent_preset) {
        return format!("Done, Sir. The accent is now {name}.");
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
        if let Some(t) = theme(name) {
            return format!("Very good, Sir. Switching to the {} theme.", t.display);
        }
        if let Some(layout) = HudLayout::parse(name) {
            return layout_ack(layout);
        }
    }
    if let Some(c) = text(&["accent_color", "accent", "color"]) {
        if parse_color(c).is_some_and(|rgb| luminance(rgb) < 0.08) {
            return "Very good, Sir. Going dark.".into();
        }
        return format!("Very good, Sir. The interface is now {}.", colour_word(c));
    }
    if let Some((step, _)) = args.get("background_shade").and_then(number_like) {
        let d = background_darkness(now.background_rgb());
        return if step > 0.0 && d >= 0.99 {
            "It is as deep as obsidian goes, Sir.".into()
        } else if step > 0.0 {
            "Done, Sir. A deeper shade of obsidian.".into()
        } else if d <= 0.01 {
            "That is the lightest the glass goes, Sir. Any lighter would hurt your eyes.".into()
        } else {
            "Done, Sir. A touch lighter, still dark glass.".into()
        };
    }
    if text(&["background_color", "background"]).is_some() || args.get("background_darkness").is_some() {
        return "Done, Sir. A new background.".into();
    }
    if let Some(name) = text(&["layout"]) {
        if let Some(layout) = HudLayout::parse(name) {
            return layout_ack(layout);
        }
    }
    let hidden = panels("hide_panels");
    if !hidden.is_empty() {
        if now.layout == HudLayout::Orb && hidden.len() == HudPanel::ALL.len() {
            return layout_ack(HudLayout::Orb);
        }
        return format!("Done, Sir. {} hidden.", capitalise(&join_labels(&hidden)));
    }
    if !panels("show_panels").is_empty() {
        return "There you are, Sir.".into();
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
    if let Some(on) = flag("sound_cues") {
        return if on { "Done, Sir. Sound cues back on.".into() } else { "Done, Sir. The board stays quiet.".into() };
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
            Some([(x + dx).clamp(0.08, 0.92), (y + dy).clamp(0.12, 0.88)])
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
    // "Toggle span mode": the other of the two.
    let span = match args.get("span_all_monitors").and_then(Value::as_str).map(words).as_deref() {
        Some("toggle" | "switch" | "flip" | "other") => Some(!s.span_all_monitors),
        _ => bool_arg(args, "span_all_monitors"),
    };
    if let Some(on) = span {
        s.span_all_monitors = on;
        // One board over every monitor, or back to fullscreen on one.
        s.fullscreen = !on;
        done.push(if on { "the HUD spans every monitor".into() } else { "the HUD back on one monitor".to_string() });
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

// ------------------------------------------------------------------- GUI master config ----

/// Every aspect of the GUI under one switchboard (`control_gui_aspect`): the live view of
/// `InterfaceSettings` with the names Sir and the model use.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GuiMasterConfig {
    /// "full", "minimal" (zen: orb and chat) or "orb_only" (pure orb mode).
    pub mode: &'static str,
    /// arc_cyan, stark_gold, crimson_mk42, emerald_matrix, stealth_obsidian (or another theme).
    pub accent_theme: String,
    /// 0 = slate glass, 1 = deepest obsidian.
    pub background_darkness: f32,
    /// 0.15-1.0.
    pub window_opacity: f32,
    /// 0-32 (panels; bubbles and buttons follow).
    pub corner_rounding: f32,
    /// 0.75-1.6.
    pub font_scale: f32,
    /// 100-800 px (the orb in orb mode, the reactor's share of its panel otherwise).
    pub orb_size: f32,
    pub always_on_top: bool,
    pub show_audio_panel: bool,
    pub show_skills_panel: bool,
    pub show_pills_bar: bool,
    pub show_header_telemetry: bool,
    pub show_chat_panel: bool,
    /// Logical size of the open window.
    pub window_size: (f32, f32),
}

impl GuiMasterConfig {
    pub fn of(s: &InterfaceSettings) -> Self {
        let accent_theme = match s.theme.as_str() {
            "stark" => "arc_cyan".to_string(),
            "stealth" => "stealth_obsidian".to_string(),
            "emerald" => "emerald_matrix".to_string(),
            other => other.to_string(),
        };
        Self {
            mode: if s.orb_mode {
                "orb_only"
            } else if s.zen_mode || s.layout == HudLayout::Orb {
                "minimal"
            } else {
                "full"
            },
            accent_theme,
            background_darkness: (background_darkness(s.background_rgb()) * 100.0).round() / 100.0,
            window_opacity: s.opacity as f32 / 100.0,
            corner_rounding: s.corner_rounding,
            font_scale: s.font_scale,
            orb_size: if s.orb_mode { s.orb_size_px } else { (s.orb_scale * 300.0).round() },
            always_on_top: s.always_on_top,
            show_audio_panel: !s.is_hidden(HudPanel::Devices) || !s.is_hidden(HudPanel::Audio),
            show_skills_panel: !s.is_hidden(HudPanel::Skills),
            show_pills_bar: !s.is_hidden(HudPanel::Pills),
            show_header_telemetry: s.header_telemetry,
            show_chat_panel: !s.is_hidden(HudPanel::Chat),
            window_size: live_window_size(),
        }
    }
}

/// The result of `control_gui_aspect`.
pub struct GuiChange {
    pub settings: InterfaceSettings,
    pub window: Option<WindowCommand>,
    /// A new inner size for the HUD window (logical points).
    pub window_size: Option<(f32, f32)>,
    pub summary: String,
}

/// A boolean from true/false, "on"/"off", "show"/"hide", 1/0.
fn switch_value(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|x| x != 0.0),
        Value::String(t) => match words(t).as_str() {
            "true" | "on" | "yes" | "show" | "visible" | "enable" | "enabled" | "an" | "ein" | "ja" => Some(true),
            "false" | "off" | "no" | "hide" | "hidden" | "disable" | "disabled" | "aus" | "nein" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// `control_gui_aspect(property, value)` (or several `property: value` pairs at once) on the
/// GUI master config.
pub fn control_gui_aspect(current: &InterfaceSettings, args: &Value) -> Result<GuiChange, String> {
    let mut pairs: Vec<(String, Value)> = Vec::new();
    if let Some(prop) = str_arg(args, &["property", "aspect"]) {
        pairs.push((prop.to_lowercase().replace([' ', '-'], "_"), args.get("value").cloned().unwrap_or(Value::Null)));
    }
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            if !matches!(k.as_str(), "property" | "aspect" | "value") {
                pairs.push((k.to_lowercase(), v.clone()));
            }
        }
    }
    if pairs.is_empty() {
        return Err("control_gui_aspect needs property and value (mode, accent_theme, background_darkness, window_opacity, corner_rounding, font_scale, orb_size, always_on_top, show_audio_panel, show_skills_panel, show_pills_bar, show_header_telemetry, show_chat_panel, window_position, window_size).".into());
    }
    let mut s = current.clone();
    let mut window = None;
    let mut window_size = None;
    let mut done: Vec<String> = Vec::new();
    for (prop, v) in pairs {
        let bad = || format!("unclear value {v} for {prop}");
        match prop.as_str() {
            "mode" | "gui_mode" | "layout" => {
                let m = v.as_str().map(words).ok_or_else(bad)?;
                match m.as_str() {
                    "full" | "normal" | "standard" | "everything" => {
                        s.orb_mode = false;
                        s.zen_mode = false;
                        s.layout = HudLayout::Full;
                    }
                    "minimal" | "zen" | "minimal zen" | "clean" => {
                        s.orb_mode = false;
                        s.zen_mode = true;
                        if s.layout == HudLayout::Orb {
                            s.layout = HudLayout::Full;
                        }
                    }
                    "orb only" | "orb" | "orb_only" | "pure orb" => s.orb_mode = true,
                    _ => return Err(format!("unknown mode '{m}' (full, minimal, orb_only)")),
                }
                done.push(format!("mode {m}"));
            }
            "accent_theme" | "theme" | "accent" => {
                let name = v.as_str().ok_or_else(bad)?;
                if let Some(t) = theme(name) {
                    s.theme = t.name.to_string();
                    s.accent = t.accent.to_string();
                    s.background = t.background.to_string();
                    done.push(format!("theme {}", t.display));
                } else if let Some(rgb) = parse_color(name) {
                    s.accent = hex(rgb);
                    s.theme = "custom".into();
                    done.push(format!("accent {}", hex(rgb)));
                } else {
                    return Err(format!("unknown theme '{name}' (arc_cyan, stark_gold, crimson_mk42, emerald_matrix, stealth_obsidian)"));
                }
            }
            "background_darkness" | "darkness" => {
                let (x, rel) = match v.as_str().map(words).as_deref() {
                    Some("darker" | "deeper" | "dunkler") => (0.2, true),
                    Some("lighter" | "brighter" | "heller") => (-0.2, true),
                    _ => number_like(&v).ok_or_else(bad)?,
                };
                let x = if x.abs() > 1.0 { x / 100.0 } else { x };
                let d = if rel { background_darkness(s.background_rgb()) + x } else { x };
                let rgb = with_darkness(s.background_rgb(), d);
                s.background = hex(rgb);
                done.push(format!("background darkness {:.0} %", background_darkness(rgb) * 100.0));
            }
            "window_opacity" | "opacity" => {
                let (x, rel) = number_like(&v).ok_or_else(bad)?;
                let pct = if x.abs() <= 1.0 { x * 100.0 } else { x };
                let pct = if rel { s.opacity as f32 + pct } else { pct };
                s.opacity = pct.round().clamp(MIN_OPACITY as f32, 100.0) as u8;
                done.push(format!("window opacity {} %", s.opacity));
            }
            "corner_rounding" | "rounding" | "corner_radius" => {
                let (x, rel) = number_like(&v).ok_or_else(bad)?;
                s.corner_rounding = (if rel { s.corner_rounding + x } else { x }).clamp(MIN_ROUNDING, MAX_ROUNDING);
                done.push(format!("corner rounding {:.0}", s.corner_rounding));
            }
            "font_scale" | "text_size" | "font_size" => {
                let x = scale_value(&v, s.font_scale).ok_or_else(bad)?;
                s.font_scale = x.clamp(MIN_FONT_SCALE, MAX_FONT_SCALE);
                done.push(format!("font scale {:.0} %", s.font_scale * 100.0));
            }
            "orb_size" | "orb_size_px" => {
                let (x, rel) = number_like(&v).ok_or_else(bad)?;
                if s.orb_mode {
                    s.orb_size_px = (if rel { s.orb_size_px + x } else { x }).clamp(100.0, 800.0).max(MIN_ORB_PX);
                    done.push(format!("orb {:.0} px", s.orb_size_px));
                } else {
                    let px = if rel { s.orb_scale * 300.0 + x } else { x };
                    s.orb_scale = (px.clamp(100.0, 800.0) / 300.0).clamp(MIN_ORB_SCALE, MAX_ORB_SCALE);
                    s.orb_size_px = px.clamp(MIN_ORB_PX, MAX_ORB_PX);
                    done.push(format!("orb {:.0} %", s.orb_scale * 100.0));
                }
            }
            "always_on_top" | "on_top" => {
                s.always_on_top = switch_value(&v).ok_or_else(bad)?;
                done.push(format!("always on top {}", if s.always_on_top { "on" } else { "off" }));
            }
            "show_audio_panel" | "show_skills_panel" | "show_pills_bar" | "show_chat_panel" | "show_memory_panel" | "show_tool_log" | "show_system_panel" => {
                let on = switch_value(&v).ok_or_else(bad)?;
                let panels: &[HudPanel] = match prop.as_str() {
                    "show_audio_panel" => &[HudPanel::Audio, HudPanel::Devices],
                    "show_skills_panel" => &[HudPanel::Skills],
                    "show_pills_bar" => &[HudPanel::Pills],
                    "show_memory_panel" => &[HudPanel::Memory],
                    "show_tool_log" => &[HudPanel::Tools],
                    "show_system_panel" => &[HudPanel::System],
                    _ => &[HudPanel::Chat],
                };
                for p in panels {
                    if on {
                        s.show(*p);
                    } else {
                        s.hide(*p);
                    }
                }
                if on {
                    s.zen_mode = false;
                }
                done.push(format!("{} {}", labels(panels), if on { "shown" } else { "hidden" }));
            }
            "show_header_telemetry" | "header_telemetry" => {
                s.header_telemetry = switch_value(&v).ok_or_else(bad)?;
                done.push(format!("header telemetry {}", if s.header_telemetry { "shown" } else { "hidden" }));
            }
            "window_position" | "position" => {
                let name = v.as_str().ok_or_else(bad)?;
                let cmd = WindowCommand::parse(name).filter(|c| matches!(c, WindowCommand::Place(_) | WindowCommand::Center)).ok_or_else(bad)?;
                window = Some(cmd);
                done.push(format!("window {}", cmd.describe()));
            }
            "window_size" | "size" => {
                let size = match &v {
                    Value::Array(a) if a.len() == 2 => a[0].as_f64().zip(a[1].as_f64()).map(|(w, h)| (w as f32, h as f32)),
                    Value::Object(o) => o.get("width").and_then(Value::as_f64).zip(o.get("height").and_then(Value::as_f64)).map(|(w, h)| (w as f32, h as f32)),
                    Value::String(t) => t.to_lowercase().replace(' ', "").split_once(['x', '×', '*']).and_then(|(w, h)| w.parse::<f32>().ok().zip(h.parse::<f32>().ok())),
                    _ => None,
                }
                .ok_or_else(bad)?;
                let size = (size.0.clamp(240.0, 7680.0).round(), size.1.clamp(200.0, 4320.0).round());
                window_size = Some(size);
                done.push(format!("window {:.0} × {:.0}", size.0, size.1));
            }
            other => return Err(format!("unknown property '{other}'")),
        }
    }
    s.sanitize();
    let cfg = GuiMasterConfig::of(&s);
    let summary = format!("GUI updated: {}. Now: {}.", done.join(", "), serde_json::to_string(&cfg).unwrap_or_default());
    Ok(GuiChange { settings: s, window, window_size, summary })
}

/// Spoken confirmation of `control_gui_aspect`.
pub fn gui_aspect_ack(args: &Value, now: &InterfaceSettings) -> String {
    let prop = str_arg(args, &["property", "aspect"]).map(|p| p.to_lowercase()).or_else(|| args.as_object().and_then(|o| o.keys().next().cloned())).unwrap_or_default();
    match prop.as_str() {
        "mode" if now.orb_mode => "Orb mode, Sir.".into(),
        "mode" if now.zen_mode => "Minimal view, Sir. Just the orb and our conversation.".into(),
        "mode" => "There you are, Sir. The full display.".into(),
        "accent_theme" | "theme" => format!("Very good, Sir. {} it is.", theme(&now.theme).map(|t| t.display).unwrap_or("Your colour")),
        "background_darkness" | "darkness" => format!("Done, Sir. The glass is at {:.0} percent depth.", background_darkness(now.background_rgb()) * 100.0),
        "corner_rounding" | "rounding" => format!("Done, Sir. Corners at {:.0}.", now.corner_rounding),
        "font_scale" | "text_size" => format!("Is this better, Sir? Text at {:.0} percent.", now.font_scale * 100.0),
        "window_opacity" | "opacity" => format!("Done, Sir. Window opacity at {} percent.", now.opacity),
        _ => "Done, Sir.".into(),
    }
}

/// The `control_gui_aspect` tool schema.
pub fn gui_aspect_tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "control_gui_aspect",
        "description": "Change any single aspect of YOUR OWN GUI (GuiMasterConfig). The background is ALWAYS dark obsidian glass: \"darker\" = background_darkness up, never a light colour. Examples: \"make it darker\" → background_darkness \"darker\"; \"rounder corners\" → corner_rounding 26; \"gold accents\" → accent_theme stark_gold; \"hide the pills\" → show_pills_bar false.",
        "parameters": {"type": "object", "properties": {
            "property": {"type": "string", "enum": ["mode", "accent_theme", "background_darkness", "window_opacity", "corner_rounding", "font_scale", "orb_size", "always_on_top", "show_audio_panel", "show_skills_panel", "show_pills_bar", "show_header_telemetry", "show_chat_panel", "window_position", "window_size"]},
            "value": {"description": "mode: full|minimal|orb_only; accent_theme: arc_cyan|stark_gold|crimson_mk42|emerald_matrix|stealth_obsidian; background_darkness 0-1 or \"darker\"/\"lighter\"; window_opacity 0.15-1; corner_rounding 0-32; font_scale 0.75-1.6; orb_size 100-800; booleans; window_position top_left|top_right|bottom_left|bottom_right|center|left|right; window_size [w, h]"}
        }, "required": ["property", "value"]}
    }})
}

fn layout_ack(layout: HudLayout) -> String {
    match layout {
        HudLayout::Orb => "Very good, Sir. Just the reactor.".into(),
        HudLayout::Compact => "Very good, Sir. Compact view.".into(),
        HudLayout::Full => "There you are, Sir. Full display.".into(),
    }
}

fn join_labels(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The `configure_interface` tool schema.
pub fn tool_definition() -> Value {
    let panels: Vec<&str> = HudPanel::ALL.iter().map(|p| p.id()).collect();
    json!({"type": "function", "function": {
        "name": "configure_interface",
        "description": "Restyle or rearrange YOUR OWN on-screen HUD (the J.A.R.V.I.S. window) when Sir asks: colours or theme, zoom, transparency, layout, which panels are visible, always on top, fullscreen, title bar, the displayed name, or show/hide/move/resize the window. Pass only what should change.",
        "parameters": {"type": "object", "properties": {
            "ui_paradigm": {"type": "string", "enum": ["modern_oled_minimal", "classic_scifi_hud"], "description": "The overall design: true-black OLED minimal (default) or the classic sci-fi HUD."},
            "accent_preset": {"type": "string", "enum": ["Ice White", "Electric Blue", "Stark Gold", "Emerald", "Violet", "Crimson", "Sunset Orange"]},
            "theme": {"type": "string", "description": format!("Colour preset: {}.", theme_list())},
            "accent_color": {"type": "string", "description": "Highlight colour: a colour name (red, gold, cyan, …) or #RRGGBB."},
            "background_color": {"type": "string", "description": "Background colour: a colour name or #RRGGBB."},
            "scale": {"type": "number", "description": "Zoom factor: 1.0 normal, 1.25 bigger, 0.8 smaller (0.5-2.5)."},
            "opacity": {"type": "integer", "description": "Background opacity in percent, 20-100 (lower = more see-through)."},
            "layout": {"type": "string", "enum": ["full", "compact", "orb"], "description": "full = every panel, compact = reactor and chat, orb = only the animated reactor."},
            "show_panels": {"type": "array", "items": {"type": "string", "enum": panels}},
            "hide_panels": {"type": "array", "items": {"type": "string", "enum": panels}},
            "always_on_top": {"type": "boolean"},
            "fullscreen": {"type": "boolean"},
            "core_view": {"type": "boolean", "description": "Open (true) or close (false) the 3D sphere of your own source modules ('show me your core')."},
            "dock_windows": {"type": "boolean", "description": "true: every detached view comes back inside the HUD ('dock all windows back inside Jarvis')."},
            "hud_palette": {"type": "string", "enum": ["rose_crimson", "arc_cyan", "stark_gold", "emerald_matrix", "ultraviolet", "crimson_overdrive", "solar_orange", "ice_monochrome"], "description": "The whole HUD's colour theme (board, console, core, emblem). For a plain colour use accent_color."},
            "detach_window": {"type": "string", "description": "Pop a view out of the HUD into its own OS window ('detach the telemetry window')."},
            "create_theme": {"type": "object", "description": "Design and save a named theme ('create a theme called Cyberpunk with neon yellow borders and electric purple core', 'design a dark stealth theme'). It is saved to disk at once and applied.", "properties": {"name": {"type": "string"}, "description": {"type": "string", "description": "Sir's words: colours per role and/or a mood (stealth, cyberpunk, ocean, fire, lava, forest, royal, arctic, sunset, night)."}, "core_primary": {"type": "string"}, "core_secondary": {"type": "string"}, "window_border": {"type": "string"}, "background_tint": {"type": "string"}}},
            "save_theme": {"type": "string", "description": "Save the current look under this name ('save this theme as Night Ops')."},
            "load_theme": {"type": "string", "description": "Apply a saved theme by name ('load my Cyberpunk theme')."},
            "orb_board_position": {"type": "string", "description": "Move the emblem on the board: top left, top center, top right, left side, center, right side, bottom left, bottom center, bottom right."},
            "span_all_monitors": {"type": "boolean", "description": "true: the HUD stretches across every monitor ('use all monitors'); false: back to one."},
            "allow_window_overlap": {"type": "boolean", "description": "true: new windows may stack on each other ('overlap the windows'); false: they open side by side (default)."},
            "hud_view": {"type": "string", "enum": ["home", "board", "core", "sim_lab", "telemetry", "schedule", "console"], "description": "Switch the HUD to one of its navigation views."},
            "feature_lab": {"type": "boolean", "description": "Open (true) or close the Feature Lab: the list of everything you can do, each with a TRY button (\"open the feature lab\")."},
            "theme_studio": {"type": "boolean", "description": "Open (true) or close the Theme Studio, where Sir designs a colour theme of his own (\"open the theme studio\", \"create a new theme\")."},
            "mixer_view": {"type": "boolean", "description": "Open (true) or close the Mixer block: a live fader and mute key for the master volume and every app (\"open the mixer\")."},
            "core_scale": {"type": ["number", "string"], "description": "The core sphere's size: a percentage (50-300) or 'bigger' / 'smaller'."},
            "core_spin": {"type": "string", "enum": ["faster", "slower", "stop"]},
            "text_console": {"type": "boolean", "description": "Open (true) or close (false) the slide-out text console where Sir types and reads the transcript."},
            "frameless": {"type": "boolean", "description": "true hides the window's title bar."},
            "animations": {"type": "boolean"},
            "sound_cues": {"type": "boolean", "description": "The board's sound cues (a block appearing, the spotlight, a mode change, an alert): 'turn off the sound cues'."},
            "title": {"type": "string", "description": "Name shown in the HUD header."},
            "window": {"type": "string", "enum": ["show", "hide", "minimize", "maximize", "restore", "center", "bigger", "smaller", "top_left", "top_right", "bottom_left", "bottom_right", "left", "right"]},
            "reset": {"type": "boolean", "description": "Restore the default look (other fields apply on top)."}
        }, "required": []}
    }})
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
        assert_eq!(theme("Iron Man").map(|t| t.name), Some("mark42"));
        assert_eq!(theme("mark-42").map(|t| t.name), Some("mark42"));
        assert_eq!(theme("dark").map(|t| t.name), Some("stealth"));
        assert_eq!(theme("light").map(|t| t.name), Some("arctic"), "light names stay dark glass");
        assert_eq!(theme("stark_gold").map(|t| t.name), Some("stark_gold"));
        assert_eq!(theme("crimson_mk42").map(|t| t.name), Some("crimson_mk42"));
        assert_eq!(theme("arc_cyan").map(|t| t.name), Some("stark"));
        assert_eq!(theme("stealth_obsidian").map(|t| t.name), Some("stealth"));
        assert_eq!(theme("emerald_matrix").map(|t| t.name), Some("emerald"));
        assert!(theme("red").is_none(), "plain colours are accents, not themes");
        assert_eq!(HudLayout::parse("Minimal"), Some(HudLayout::Orb));
        assert_eq!(HudLayout::parse("kompakt"), Some(HudLayout::Compact));
    }

    #[test]
    fn panels_parse_with_aliases_and_groups() {
        assert_eq!(HudPanel::parse("the system panel"), vec![HudPanel::System]);
        assert_eq!(HudPanel::parse("tool log"), vec![HudPanel::Tools]);
        assert_eq!(HudPanel::parse("Gedächtnis"), vec![HudPanel::Memory]);
        assert_eq!(HudPanel::parse("left side"), HudPanel::LEFT.to_vec());
        // Ambiguous words need "panel": "turn off the microphone" means muting, not hiding.
        assert!(HudPanel::parse("microphone").is_empty());
        assert_eq!(HudPanel::parse("microphone panel"), vec![HudPanel::Audio]);
        assert!(HudPanel::parse("spotify").is_empty());
    }

    #[test]
    fn configure_applies_colours_and_reports_changes() {
        let s = apply(json!({"accent_color": "red"}));
        assert_eq!(s.accent, "#ff3b3b");
        assert_eq!(s.theme, "custom");
        let s = apply(json!({"theme": "mark 42"}));
        assert_eq!((s.accent.as_str(), s.background.as_str()), ("#ffc233", "#170507"));
        // "Make the interface black" darkens the background instead of hiding the accent.
        let s = apply(json!({"accent_color": "black"}));
        assert_eq!((s.accent.as_str(), s.background.as_str()), ("#00e5ff", "#000000"));
        let s = apply(json!({"background_color": "red"}));
        assert_eq!(s.background, hex(scale_rgb([255, 59, 59], 0.14)));
        let done = configure(&InterfaceSettings::default(), &json!({"accent_color": "red", "opacity": "nonsense"})).unwrap();
        assert!(done.summary.contains("accent colour red") && done.summary.contains("Not changed"));
        assert!(configure(&InterfaceSettings::default(), &json!({})).is_err());
        assert!(configure(&InterfaceSettings::default(), &json!({"theme": "banana"})).is_err());
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
    fn configure_panels_layout_and_window() {
        let s = apply(json!({"hide_panels": ["system", "tools"]}));
        assert!(!s.shows(HudPanel::System) && !s.shows(HudPanel::Tools) && s.shows(HudPanel::Chat));
        let compact = InterfaceSettings { layout: HudLayout::Compact, ..Default::default() };
        let shown = configure(&compact, &json!({"show_panels": "memory"})).unwrap().settings;
        assert_eq!(shown.layout, HudLayout::Full, "showing a side panel widens the layout");
        assert_eq!(apply(json!({"hide_panels": ["everything"]})).layout, HudLayout::Orb);
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
            serde_json::from_str(r#"{"layout": "weird", "hidden_panels": ["chat", "bogus", "chat"], "opacity": 3}"#).unwrap();
        let mut s = s;
        s.sanitize();
        assert_eq!(s.layout, HudLayout::Full);
        assert_eq!(s.hidden_panels, vec!["chat".to_string()]);
        assert_eq!(s.opacity, MIN_OPACITY);
        assert_eq!(serde_json::to_value(HudLayout::Orb).unwrap(), json!("orb"));
    }

    #[test]
    fn backgrounds_are_always_dark_glass() {
        // The reported bug: a light mint background.
        let s = apply(json!({"background_color": "#E0FFE8"}));
        assert!(luminance(s.background_rgb()) <= MAX_BACKGROUND_LUMINANCE + 0.005, "{}", s.background);
        let s = apply(json!({"background_color": "white"}));
        assert_eq!(s.background_rgb(), LIGHTEST_BACKGROUND);
        let s = apply(json!({"background_color": "mint"}));
        assert!(s.background_rgb().iter().all(|c| *c <= 46), "{}", s.background);
        // "Make it darker" goes deeper, step by step, down to obsidian and no further.
        let mut s = InterfaceSettings::default();
        let start = background_darkness(s.background_rgb());
        s = configure(&s, &json!({"background_shade": 0.15})).unwrap().settings;
        assert!(background_darkness(s.background_rgb()) > start);
        for _ in 0..12 {
            s = configure(&s, &json!({"background_shade": 0.15})).unwrap().settings;
        }
        assert!(background_darkness(s.background_rgb()) >= 0.99);
        assert!(s.background_rgb().iter().zip(DEEPEST_BACKGROUND).all(|(c, d)| *c >= d.min(*c)));
        // Lighter stays dark glass.
        for _ in 0..20 {
            s = configure(&s, &json!({"background_shade": -0.15})).unwrap().settings;
        }
        assert!(luminance(s.background_rgb()) <= MAX_BACKGROUND_LUMINANCE + 0.005);
        let hand_edited: InterfaceSettings = serde_json::from_str(r##"{"background": "#eef3f7"}"##).unwrap();
        let mut hand_edited = hand_edited;
        hand_edited.sanitize();
        assert!(luminance(hand_edited.background_rgb()) <= MAX_BACKGROUND_LUMINANCE + 0.005);
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
        let spanned = configure(&now, &json!({"span_all_monitors": true})).unwrap().settings;
        assert!(spanned.span_all_monitors && !spanned.fullscreen);
        // The toggle goes both ways, and back means the fullscreen board on one monitor.
        let back = configure(&spanned, &json!({"span_all_monitors": "toggle"})).unwrap().settings;
        assert!(!back.span_all_monitors && back.fullscreen);
        assert!(configure(&back, &json!({"span_all_monitors": "toggle"})).unwrap().settings.span_all_monitors);
        assert_eq!(spoken_ack(&json!({"span_all_monitors": true}), &spanned), "One board across every monitor, Sir. I stay on your first screen.");
        assert_eq!(spoken_ack(&json!({"span_all_monitors": "toggle"}), &back), "Back on your first monitor, Sir.");
        // Spanning and the fullscreen board exclude each other, whichever way one is asked for:
        // "full screen" (or F11) on a spanned board ends the span, it never leaves both set.
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
    fn orb_mode_and_gui_aspects() {
        let now = InterfaceSettings::default();
        let s = control_orb_mode(&now, &json!({"enabled": true, "orb_size_px": 5000, "screen_position": "top right of monitor 2", "opacity": 0.05})).unwrap().settings;
        assert!(s.orb_mode);
        assert_eq!((s.orb_size_px, s.orb_anchor.as_str(), s.orb_monitor), (MAX_ORB_PX, "top-right", 2));
        assert_eq!(s.orb_opacity, 0.15);
        let s = control_orb_mode(&s, &json!({"enabled": false})).unwrap().settings;
        assert!(!s.orb_mode);
        let g = control_gui_aspect(&now, &json!({"property": "accent_theme", "value": "stark_gold"})).unwrap().settings;
        assert_eq!(g.accent, "#ffb800");
        let g = control_gui_aspect(&now, &json!({"property": "background_darkness", "value": 1.0})).unwrap().settings;
        assert!(background_darkness(g.background_rgb()) >= 0.99);
        let g = control_gui_aspect(&now, &json!({"property": "corner_rounding", "value": 99})).unwrap().settings;
        assert_eq!(g.corner_rounding, MAX_ROUNDING);
        let g = control_gui_aspect(&now, &json!({"property": "mode", "value": "orb_only"})).unwrap().settings;
        assert!(g.orb_mode);
        let g = control_gui_aspect(&now, &json!({"show_pills_bar": false, "window_opacity": 0.5})).unwrap().settings;
        assert!(!g.shows(HudPanel::Pills));
        assert_eq!(g.opacity, 50);
        let c = control_gui_aspect(&now, &json!({"property": "window_size", "value": "1280x720"})).unwrap();
        assert_eq!(c.window_size, Some((1280.0, 720.0)));
        assert!(control_gui_aspect(&now, &json!({"property": "accent_theme", "value": "banana"})).is_err());
        // The default is the clean two-zone studio layout (orb + conversation).
        assert_eq!(GuiMasterConfig::of(&now).mode, "minimal");
        let full = InterfaceSettings { zen_mode: false, ..now.clone() };
        assert_eq!(GuiMasterConfig::of(&full).mode, "full");
    }

    #[test]
    fn spoken_confirmations() {
        let now = InterfaceSettings::default();
        assert_eq!(spoken_ack(&json!({"accent_color": "rot"}), &now), "Very good, Sir. The interface is now red.");
        assert_eq!(spoken_ack(&json!({"accent_color": "dunkelblau"}), &now), "Very good, Sir. The interface is now dark blue.");
        assert_eq!(spoken_ack(&json!({"theme": "iron man"}), &now), "Very good, Sir. Switching to the Mark 42 theme.");
        assert_eq!(
            spoken_ack(&json!({"hide_panels": ["system", "tools"]}), &now),
            "Done, Sir. System monitor and tool log hidden."
        );
        assert_eq!(spoken_ack(&json!({"scale": "bigger"}), &now), "Is this better, Sir?");
        assert_eq!(spoken_ack(&json!({"window": "hide"}), &now), "Very good, Sir. I'll be in the tray if you need me.");
    }
}
