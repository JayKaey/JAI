//! The HUD's one source of colour: a reactive `ThemePalette` every renderer reads each frame
//! (`current()`), so "change your colour to gold", the `[ THEME ]` menu or a typed command
//! re-themes the whole HUD — board frames and brackets, the text console, the core sphere and the
//! emblem's rings — at once, cross-fading through HSV over 0.35 s.
//!
//! Nine hyper-saturated presets (`PRESETS`, `jai_gold` the default) and any custom colour:
//! a colour word or #RRGGBB becomes a palette with its saturation and value pushed to 1.0
//! (`derive`). The neon itself is premultiplied-additive light (`hdr_neon_color`): four passes
//! from a wide 22 px aura to a 1.5 px white-hot filament (`NEON_PASSES`), so the rings glow
//! instead of turning muddy under alpha blending.

use eframe::egui::Color32;
use std::sync::{LazyLock, RwLock};
use std::time::{Duration, Instant};

/// How long a colour change cross-fades.
pub const FADE: Duration = Duration::from_millis(250);

/// The colour channels of one theme (RGB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    /// The main glowing energy ring and the focal colour.
    pub core_primary: [u8; 3],
    /// The counter-rotating rings and the contrast accent.
    pub core_secondary: [u8; 3],
    /// The white-hot filament at the centre of every neon stroke (90 % white + 10 % primary).
    pub core_hot_center: [u8; 3],
    /// The sharp 1 px block border.
    pub hud_frame_border: [u8; 3],
    /// The outer glow and the active / highlighted border.
    pub hud_frame_glow: [u8; 3],
    /// Corner brackets and tick marks.
    pub hud_bracket_color: [u8; 3],
}

impl ThemePalette {
    const fn new(primary: u32, secondary: u32, border: u32, glow: u32) -> Self {
        let p = rgb(primary);
        Self {
            core_primary: p,
            core_secondary: rgb(secondary),
            core_hot_center: mix_const(p, 0.10),
            hud_frame_border: rgb(border),
            hud_frame_glow: rgb(glow),
            hud_bracket_color: mix_bracket(p),
        }
    }

    pub fn primary(&self) -> Color32 {
        c32(self.core_primary)
    }

    pub fn secondary(&self) -> Color32 {
        c32(self.core_secondary)
    }

    pub fn border(&self) -> Color32 {
        c32(self.hud_frame_border)
    }

    pub fn glow(&self) -> Color32 {
        c32(self.hud_frame_glow)
    }

    pub fn bracket(&self) -> Color32 {
        c32(self.hud_bracket_color)
    }
}

const fn rgb(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

/// `share` of `colour` on white (const: the hot centre).
const fn mix_const(colour: [u8; 3], share: f32) -> [u8; 3] {
    [on_white(colour[0], share), on_white(colour[1], share), on_white(colour[2], share)]
}

const fn on_white(c: u8, share: f32) -> u8 {
    (255.0 - (255.0 - c as f32) * share) as u8
}

/// Brackets: near-white with a hint of the primary.
const fn mix_bracket(colour: [u8; 3]) -> [u8; 3] {
    mix_const(colour, 0.12)
}

fn c32(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// The presets: his own yellow first (the default since v24), then the eight Stark ones.
pub const PRESETS: [(&str, ThemePalette); 9] = [
    ("jai_gold", ThemePalette::new(0xFFD400, 0xFF9A00, 0xFFC400, 0xFFE873)),
    ("rose_crimson", ThemePalette::new(0xFF0055, 0x00F5FF, 0x00D8FF, 0xFF1E6E)),
    ("arc_cyan", ThemePalette::new(0x00F0FF, 0x38B6FF, 0x00E5FF, 0x80F8FF)),
    ("stark_gold", ThemePalette::new(0xFFB700, 0xFF2A00, 0xFFA500, 0xFFE066)),
    ("emerald_matrix", ThemePalette::new(0x00FF88, 0x00E5FF, 0x00E676, 0x69F0AE)),
    ("ultraviolet", ThemePalette::new(0xB026FF, 0x00F0FF, 0x9D4EDD, 0xE0AAFF)),
    ("crimson_overdrive", ThemePalette::new(0xFF002B, 0xFF6B00, 0xFF1744, 0xFF5252)),
    ("solar_orange", ThemePalette::new(0xFF5500, 0xFFD000, 0xFF6D00, 0xFFAB40)),
    ("ice_monochrome", ThemePalette::new(0xFFFFFF, 0xA5F3FC, 0xE2E8F0, 0xFFFFFF)),
];

pub const DEFAULT_PRESET: &str = "jai_gold";

/// A preset by name ("rose_crimson", "Arc Cyan", "stark-gold").
pub fn preset(name: &str) -> Option<ThemePalette> {
    let key = name.trim().to_lowercase().replace([' ', '-'], "_");
    PRESETS.iter().find(|(n, _)| *n == key).map(|(_, p)| *p)
}

/// `[ THEME ]` cycles: the preset after `name` (the first one after a custom colour).
pub fn next_preset(name: &str) -> &'static str {
    let key = name.trim().to_lowercase().replace([' ', '-'], "_");
    let i = PRESETS.iter().position(|(n, _)| *n == key).map_or(0, |i| i + 1);
    PRESETS[i % PRESETS.len()].0
}

/// A preset's name as the HUD prints it ("STARK GOLD").
pub fn label(name: &str) -> String {
    name.replace('_', " ").to_uppercase()
}

/// The preset a spoken colour word means (English and German), if it has one.
pub fn preset_for_word(word: &str) -> Option<&'static str> {
    let w = word.trim().to_lowercase();
    let w = w.trim_end_matches(['.', '!', '?']);
    if preset(w).is_some() {
        return PRESETS.iter().map(|(n, _)| *n).find(|n| *n == w.replace([' ', '-'], "_"));
    }
    Some(match w {
        "red" | "rot" | "blood red" | "blutrot" | "crimson" => "crimson_overdrive",
        // ("Make the interface rows": the recogniser's "rose".)
        "rose" | "roses" | "rows" | "pink" | "magenta" | "rosa" | "pinke" | "rose crimson" => "rose_crimson",
        "cyan" | "blue" | "blau" | "türkis" | "tuerkis" | "turquoise" | "aqua" | "arc reactor" | "arc" | "hellblau" => "arc_cyan",
        "yellow" | "gelb" | "golden yellow" | "goldgelb" | "jai" | "jai gold" | "jai yellow" => "jai_gold",
        "gold" | "golden" | "stark" => "stark_gold",
        "green" | "grün" | "gruen" | "emerald" | "smaragd" | "matrix" => "emerald_matrix",
        "purple" | "violet" | "lila" | "violett" | "lilac" | "vibranium" => "ultraviolet",
        "orange" | "amber" | "bernstein" | "solar" => "solar_orange",
        "white" | "weiß" | "weiss" | "silver" | "silber" | "ice" | "eis" | "monochrome" => "ice_monochrome",
        _ => return None,
    })
}

// ------------------------------------------------------------------------------------ HSV ----

/// RGB → (hue 0-360, saturation 0-1, value 0-1).
pub fn to_hsv(c: [u8; 3]) -> (f32, f32, f32) {
    let [r, g, b] = c.map(|v| v as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d < 1e-6 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max < 1e-6 { 0.0 } else { d / max }, max)
}

pub fn from_hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r, g, b].map(|v| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Full saturation and value for a coloured hue (greys and whites keep their neutrality).
pub fn saturate(c: [u8; 3]) -> [u8; 3] {
    let (h, s, v) = to_hsv(c);
    if s < 0.15 {
        return mix_const([255, 255, 255], 0.0).map(|w| (w as f32 * v.max(0.9)) as u8);
    }
    from_hsv(h, 1.0, 1.0)
}

/// Between two colours through HSV (hue along the shorter arc), `t` 0-1.
pub fn lerp_color_hsv(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let (ha, sa, va) = to_hsv(a);
    let (hb, sb, vb) = to_hsv(b);
    // A grey has no hue: it takes the other colour's.
    let (ha, hb) = match (sa < 0.05, sb < 0.05) {
        (true, false) => (hb, hb),
        (false, true) => (ha, ha),
        _ => (ha, hb),
    };
    let mut dh = hb - ha;
    if dh > 180.0 {
        dh -= 360.0;
    } else if dh < -180.0 {
        dh += 360.0;
    }
    from_hsv(ha + dh * t, sa + (sb - sa) * t, va + (vb - va) * t)
}

pub fn lerp_palette(a: &ThemePalette, b: &ThemePalette, t: f32) -> ThemePalette {
    ThemePalette {
        core_primary: lerp_color_hsv(a.core_primary, b.core_primary, t),
        core_secondary: lerp_color_hsv(a.core_secondary, b.core_secondary, t),
        core_hot_center: lerp_color_hsv(a.core_hot_center, b.core_hot_center, t),
        hud_frame_border: lerp_color_hsv(a.hud_frame_border, b.hud_frame_border, t),
        hud_frame_glow: lerp_color_hsv(a.hud_frame_glow, b.hud_frame_glow, t),
        hud_bracket_color: lerp_color_hsv(a.hud_bracket_color, b.hud_bracket_color, t),
    }
}

/// A palette for any colour: saturated primary, its complement as the contrast accent, the
/// border in the primary, a lighter glow, white-hot centre and near-white brackets.
pub fn derive(colour: [u8; 3]) -> ThemePalette {
    let primary = saturate(colour);
    let (h, s, _) = to_hsv(primary);
    let secondary = if s < 0.15 { [0xA5, 0xF3, 0xFC] } else { from_hsv(h + 180.0, 1.0, 1.0) };
    let glow = if s < 0.15 { [255, 255, 255] } else { from_hsv(h, 0.62, 1.0) };
    ThemePalette { core_primary: primary, core_secondary: secondary, core_hot_center: mix_const(primary, 0.10), hud_frame_border: primary, hud_frame_glow: glow, hud_bracket_color: mix_bracket(primary) }
}

/// The palette a stored `hud_palette` value names: a preset, a saved theme ("theme:Cyberpunk")
/// or "custom:#RRGGBB".
pub fn resolve(name: &str) -> ThemePalette {
    if let Some(p) = preset(name) {
        return p;
    }
    if let Some(t) = name.trim().strip_prefix(CUSTOM_PREFIX).and_then(custom) {
        return t.palette();
    }
    let hex = name.trim().trim_start_matches("custom:").trim_start_matches('#');
    match u32::from_str_radix(hex, 16) {
        Ok(v) if hex.len() == 6 => derive(rgb(v)),
        _ => preset(DEFAULT_PRESET).unwrap_or(PRESETS[0].1),
    }
}

/// What a colour request stores: the preset its word names, else "custom:#RRGGBB".
pub fn palette_name_for(word: &str, colour: [u8; 3]) -> String {
    match preset_for_word(word) {
        Some(p) => p.to_string(),
        None => format!("custom:#{:02X}{:02X}{:02X}", colour[0], colour[1], colour[2]),
    }
}

// ------------------------------------------------------------------------- saved themes ----

/// `hud_palette` of a saved theme: "theme:" + its name.
pub const CUSTOM_PREFIX: &str = "theme:";
/// Where saved themes live (next to JAI.exe); mirrored into the memory database.
pub const CUSTOM_THEMES_PATH: &str = "config/custom_themes.json";

/// A theme Sir built in `[ + CREATE THEME ]` or JAI designed from a description.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CustomTheme {
    pub name: String,
    pub core_primary: [u8; 3],
    pub core_secondary: [u8; 3],
    pub window_border: [u8; 3],
    pub background_tint: [u8; 3],
}

impl CustomTheme {
    pub fn palette(&self) -> ThemePalette {
        let (h, s, _) = to_hsv(self.window_border);
        let glow = if s < 0.15 { [255, 255, 255] } else { from_hsv(h, 0.55, 1.0) };
        ThemePalette { core_primary: self.core_primary, core_secondary: self.core_secondary, core_hot_center: mix_const(self.core_primary, 0.10), hud_frame_border: self.window_border, hud_frame_glow: glow, hud_bracket_color: mix_bracket(self.core_primary) }
    }

    /// Its `hud_palette` value.
    pub fn key(&self) -> String {
        format!("{CUSTOM_PREFIX}{}", self.name)
    }

    /// A palette (and the background) kept under `name`.
    pub fn from_palette(name: &str, p: &ThemePalette, background: [u8; 3]) -> Self {
        Self { name: name.trim().to_string(), core_primary: p.core_primary, core_secondary: p.core_secondary, window_border: p.hud_frame_border, background_tint: background }
    }
}

fn store_path() -> std::path::PathBuf {
    if cfg!(test) {
        std::env::temp_dir().join(format!("jai_custom_themes_{}.json", std::process::id()))
    } else {
        std::path::PathBuf::from(CUSTOM_THEMES_PATH)
    }
}

static CUSTOM: LazyLock<RwLock<Vec<CustomTheme>>> = LazyLock::new(|| RwLock::new(load_custom(&store_path())));
static THEME_DB: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The themes in a `custom_themes.json` (none when it is missing or broken).
pub fn load_custom(path: &std::path::Path) -> Vec<CustomTheme> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// The memory database the saved themes are mirrored into; its rows come back when the JSON
/// file was lost.
pub fn attach_database(db: &str) {
    if THEME_DB.set(db.to_string()).is_err() {
        return;
    }
    let Ok(conn) = rusqlite::Connection::open(db) else { return };
    let _ = conn.execute_batch("CREATE TABLE IF NOT EXISTS custom_themes (name TEXT PRIMARY KEY COLLATE NOCASE, json TEXT NOT NULL, saved_at INTEGER NOT NULL)");
    let Ok(mut stmt) = conn.prepare("SELECT json FROM custom_themes") else { return };
    let rows: Vec<CustomTheme> = stmt.query_map([], |r| r.get::<_, String>(0)).map(|it| it.flatten().filter_map(|j| serde_json::from_str(&j).ok()).collect()).unwrap_or_default();
    let mut themes = CUSTOM.write().unwrap_or_else(|e| e.into_inner());
    for t in rows {
        if !themes.iter().any(|k| k.name.eq_ignore_ascii_case(&t.name)) {
            themes.push(t);
        }
    }
}

pub fn custom_themes() -> Vec<CustomTheme> {
    CUSTOM.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A saved theme by name (any case, "my cyberpunk theme" → "Cyberpunk").
pub fn custom(name: &str) -> Option<CustomTheme> {
    let key = name.trim().to_lowercase();
    let key = key.trim_start_matches("my ").trim_end_matches(" theme").trim();
    custom_themes().into_iter().find(|t| t.name.to_lowercase() == key)
}

/// Saves (or replaces) a theme: on disk at once, and in the memory database.
pub fn save_custom(theme: CustomTheme) -> Result<(), String> {
    if theme.name.trim().is_empty() {
        return Err("a theme needs a name".into());
    }
    let themes = {
        let mut all = CUSTOM.write().unwrap_or_else(|e| e.into_inner());
        all.retain(|t| !t.name.eq_ignore_ascii_case(&theme.name));
        all.push(theme.clone());
        all.clone()
    };
    let path = store_path();
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&themes).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(db) = THEME_DB.get() {
        let json = serde_json::to_string(&theme).map_err(|e| e.to_string())?;
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        rusqlite::Connection::open(db)
            .and_then(|c| c.execute("INSERT OR REPLACE INTO custom_themes (name, json, saved_at) VALUES (?1, ?2, ?3)", rusqlite::params![theme.name, json, at]))
            .map_err(|e| format!("theme database: {e}"))?;
    }
    Ok(())
}

/// A colour phrase: "neon yellow", "electric purple", "dark red", "#FF8800".
fn colour_phrase(phrase: &str) -> Option<[u8; 3]> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    let (mut sat, mut val) = (None::<f32>, None::<f32>);
    let mut rest = Vec::new();
    for w in &words {
        match *w {
            "neon" | "electric" | "bright" | "hot" | "vivid" | "glowing" | "toxic" => (sat, val) = (Some(1.0), Some(1.0)),
            "dark" | "deep" | "stealth" | "dim" | "midnight" => val = Some(0.55),
            "pale" | "light" | "soft" | "pastel" => sat = Some(0.45),
            w => rest.push(w),
        }
    }
    let base = (0..rest.len()).find_map(|i| crate::interface::parse_color(&rest[i..].join(" ")))?;
    let (h, s, v) = to_hsv(base);
    Some(from_hsv(h, sat.unwrap_or(s), val.unwrap_or(v)))
}

/// Moods JAI designs from ("design a dark stealth theme"): (word, primary, secondary, border).
const MOODS: &[(&str, u32, u32, u32)] = &[
    ("stealth", 0x7FA3B8, 0x3E5566, 0x4A6272),
    ("cyberpunk", 0xFFE600, 0xB026FF, 0xFFE600),
    ("ocean", 0x00B4FF, 0x00FFD0, 0x0091D5),
    ("fire", 0xFF4D00, 0xFFC300, 0xFF3300),
    ("lava", 0xFF2200, 0xFF8800, 0xE01B00),
    ("forest", 0x2EFF6A, 0xB8FF3C, 0x1FC85A),
    ("royal", 0x9B30FF, 0xFFC400, 0x8A2BE2),
    ("arctic", 0xE6FAFF, 0x6FE3FF, 0xBFEFFF),
    ("sunset", 0xFF5E3A, 0xFF2A8A, 0xFF7A3D),
    ("night", 0x5F7DFF, 0x8C5CFF, 0x4A5FCC),
];

/// A theme from Sir's words: explicit roles ("neon yellow borders and electric purple core",
/// "black background") win, then a mood ("dark stealth"), then any colour named.
pub fn design_theme(name: Option<&str>, description: &str) -> Option<CustomTheme> {
    let d = description.to_lowercase();
    let mood = MOODS.iter().find(|(w, ..)| d.contains(w));
    let mut primary = mood.map(|m| rgb(m.1));
    let mut secondary = mood.map(|m| rgb(m.2));
    let mut border = mood.map(|m| rgb(m.3));
    let mut background = None;
    for part in d.split([',', ';']).flat_map(|p| p.split(" and ")).flat_map(|p| p.split(" with ")) {
        let words: Vec<&str> = part.split_whitespace().collect();
        for (i, w) in words.iter().enumerate() {
            let role = match w.trim_end_matches('s') {
                "border" | "frame" | "window" | "outline" => &mut border,
                "core" | "primary" | "orb" | "emblem" => &mut primary,
                "secondary" | "ring" | "accent" | "highlight" => &mut secondary,
                "background" | "backdrop" | "tint" => &mut background,
                _ => continue,
            };
            // "neon yellow borders" or "borders in neon yellow".
            let before = words[..i].iter().rev().take(3).rev().copied().collect::<Vec<_>>().join(" ");
            let after = words[i + 1..].iter().skip_while(|w| matches!(**w, "in" | "of" | "is" | "are" | "should" | "be")).take(3).copied().collect::<Vec<_>>().join(" ");
            if let Some(c) = (0..3).filter_map(|k| colour_phrase(&before.split_whitespace().skip(k).collect::<Vec<_>>().join(" "))).next().or_else(|| colour_phrase(&after)) {
                *role = Some(c);
            }
        }
    }
    if primary.is_none() && border.is_none() {
        let any = d.split_whitespace().collect::<Vec<_>>();
        let c = (0..any.len()).find_map(|i| colour_phrase(&any[i..(i + 2).min(any.len())].join(" ")))?;
        let p = derive(c);
        (primary, secondary, border) = (Some(p.core_primary), Some(p.core_secondary), Some(p.hud_frame_border));
    }
    let primary = primary.or(border)?;
    let border = border.unwrap_or(primary);
    let secondary = secondary.unwrap_or_else(|| derive(primary).core_secondary);
    let dark = d.contains("dark") || d.contains("stealth") || d.contains("black") || d.contains("night");
    let (h, _, _) = to_hsv(primary);
    let background = background.unwrap_or(if dark { [2, 3, 5] } else { from_hsv(h, 0.6, 0.06) });
    let name = name.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| {
        let skip = ["a", "an", "the", "me", "for", "theme", "design", "create", "make", "build", "new", "custom", "please", "jai", "with", "and", "my"];
        let words: Vec<String> = d.split_whitespace().filter(|w| !skip.contains(w)).take(2).map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }).collect();
        if words.is_empty() { "Custom".into() } else { words.join(" ") }
    });
    Some(CustomTheme { name, core_primary: primary, core_secondary: secondary, window_border: border, background_tint: background })
}

// ------------------------------------------------------------------------------- the fade ----

/// Bumped on every palette change (fades): the HUD rebuilds its colours when it moves.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

fn start_fade(to: ThemePalette, name: &str) {
    let now = current();
    let mut f = ACTIVE.write().unwrap_or_else(|e| e.into_inner());
    *f = Fade { from: now, to, start: Instant::now(), name: name.to_string() };
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

struct Fade {
    from: ThemePalette,
    to: ThemePalette,
    start: Instant,
    name: String,
}

static ACTIVE: LazyLock<RwLock<Fade>> = LazyLock::new(|| {
    let p = preset(DEFAULT_PRESET).unwrap_or(PRESETS[0].1);
    RwLock::new(Fade { from: p, to: p, start: Instant::now() - FADE, name: DEFAULT_PRESET.to_string() })
});

fn eased(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The palette right now (mid-fade while a change runs). Every renderer calls this per frame.
pub fn current() -> ThemePalette {
    let f = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    let t = f.start.elapsed().as_secs_f32() / FADE.as_secs_f32();
    if t >= 1.0 {
        f.to
    } else {
        lerp_palette(&f.from, &f.to, eased(t))
    }
}

/// Fades to the palette `name` (a preset or "custom:#RRGGBB"); returns whether it changed.
pub fn follow(name: &str) -> bool {
    {
        let f = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
        if f.name == name {
            return false;
        }
    }
    start_fade(resolve(name), name);
    true
}

/// A colour change is still fading (the HUD keeps 60 FPS meanwhile).
pub fn fading() -> bool {
    ACTIVE.read().unwrap_or_else(|e| e.into_inner()).start.elapsed() < FADE
}

/// Ends a running fade at once: the palette is the one being faded to. (The visual verifier
/// steps through every theme and checks each at its final colours.)
pub fn finish_fade() {
    let mut f = ACTIVE.write().unwrap_or_else(|e| e.into_inner());
    if f.start.elapsed() < FADE {
        f.start = Instant::now().checked_sub(FADE).unwrap_or(f.start);
        f.from = f.to;
        GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// The name of the palette on screen (or being faded to): a preset, "custom:#RRGGBB" or
/// "theme:<name>".
pub fn active_name() -> String {
    ACTIVE.read().unwrap_or_else(|e| e.into_inner()).name.clone()
}

// ------------------------------------------------------------------------------- HDR neon ----

/// Which colour a neon pass takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeonLayer {
    Primary,
    HotCenter,
}

/// The four additive passes of every neon stroke: (width px at the reference size, alpha, layer):
/// the wide ambient aura, the outer halo, the vibrant body and the white-hot laser filament.
pub const NEON_PASSES: [(f32, u8, NeonLayer); 4] = [(22.0, 28, NeonLayer::Primary), (10.0, 75, NeonLayer::Primary), (4.2, 220, NeonLayer::Primary), (1.5, 255, NeonLayer::HotCenter)];

/// Premultiplied-additive light of `colour` at `alpha` (0-255), its saturation pushed to 1.0:
/// zero alpha, so it adds to what is under it instead of greying it.
pub fn hdr_neon_color(colour: [u8; 3], alpha: u8) -> Color32 {
    let vivid = saturate(colour);
    let a = alpha as f32 / 255.0;
    Color32::from_rgba_premultiplied((vivid[0] as f32 * a) as u8, (vivid[1] as f32 * a) as u8, (vivid[2] as f32 * a) as u8, 0)
}

/// The active palette is global: tests that change it (here and in the visual verifier) run
/// one at a time.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_theme_button_cycles_through_all_the_presets() {
        let mut name = PRESETS[0].0;
        let mut seen = vec![name];
        for _ in 0..PRESETS.len() - 1 {
            name = next_preset(name);
            seen.push(name);
        }
        assert_eq!(seen, PRESETS.map(|(n, _)| n), "JAI Gold → Rose Crimson → Arc Cyan → … → Ice Monochrome");
        assert_eq!(next_preset("ice_monochrome"), "jai_gold", "and round again");
        assert_eq!(next_preset("custom:#123456"), "jai_gold");
        assert_eq!(label("stark_gold"), "STARK GOLD");
    }

    #[test]
    fn the_presets_are_fully_saturated_with_white_hot_centres() {
        assert_eq!(PRESETS.len(), 9);
        // His own yellow is the default: yellow, not the gold preset's orange.
        let own = preset(DEFAULT_PRESET).unwrap();
        assert_eq!((DEFAULT_PRESET, own.core_primary), ("jai_gold", [0xFF, 0xD4, 0x00]));
        assert!(own.core_primary[1] > preset("stark_gold").unwrap().core_primary[1], "more yellow than Stark Gold");
        assert_eq!(preset("rose_crimson").unwrap().core_primary, [0xFF, 0x00, 0x55]);
        assert_eq!(preset("Arc Cyan").unwrap().hud_frame_border, [0x00, 0xE5, 0xFF]);
        assert_eq!(preset("stark-gold").unwrap().core_secondary, [0xFF, 0x2A, 0x00]);
        for (name, p) in PRESETS.iter().filter(|(n, _)| *n != "ice_monochrome") {
            let (_, s, v) = to_hsv(p.core_primary);
            assert!(s > 0.8 && v > 0.95, "{name}: s {s} v {v}");
            // On screen every pass is pushed to full saturation.
            let lit = hdr_neon_color(p.core_primary, 255);
            assert!(to_hsv([lit.r(), lit.g(), lit.b()]).1 > 0.99, "{name} glows at full saturation");
            // 90 % white + 10 % primary: bright, faintly tinted.
            let (_, hs, hv) = to_hsv(p.core_hot_center);
            assert!(hv > 0.99 && hs < 0.12 && hs > 0.0, "{name} hot centre {:?}", p.core_hot_center);
        }
        assert_eq!(preset("ice_monochrome").unwrap().core_primary, [255, 255, 255]);
    }

    #[test]
    fn colour_words_pick_presets_in_english_and_german() {
        for (word, want) in [("red", "crimson_overdrive"), ("Rot", "crimson_overdrive"), ("blue", "arc_cyan"), ("cyan", "arc_cyan"), ("gold", "stark_gold"), ("gelb", "jai_gold"), ("yellow", "jai_gold"), ("green", "emerald_matrix"), ("grün", "emerald_matrix"), ("purple", "ultraviolet"), ("lila", "ultraviolet"), ("orange", "solar_orange"), ("pink", "rose_crimson"), ("white", "ice_monochrome"), ("weiß", "ice_monochrome"), ("ultraviolet", "ultraviolet")] {
            assert_eq!(preset_for_word(word), Some(want), "{word}");
        }
        assert_eq!(preset_for_word("teal-ish brown"), None);
        assert_eq!(palette_name_for("brown", [0x8B, 0x45, 0x13]), "custom:#8B4513");
        assert_eq!(palette_name_for("gold", [0xFF, 0xD7, 0x00]), "stark_gold");
    }

    #[test]
    fn a_custom_colour_becomes_a_vivid_palette() {
        let p = resolve("custom:#8B4513");
        let (h, s, v) = to_hsv(p.core_primary);
        assert!((h - 25.0).abs() < 2.0 && s > 0.99 && v > 0.99, "{h} {s} {v}");
        assert_eq!(p.hud_frame_border, p.core_primary);
        let (hs, _, _) = to_hsv(p.core_secondary);
        assert!(((hs - h).rem_euclid(360.0) - 180.0).abs() < 2.0, "the complement");
        assert_eq!(resolve("nonsense"), preset(DEFAULT_PRESET).unwrap());
        assert_eq!(resolve("emerald_matrix"), preset("emerald_matrix").unwrap());
    }

    #[test]
    fn colours_cross_fade_through_hsv_over_350_ms() {
        let red = [255, 0, 0];
        let blue = [0, 0, 255];
        // Red → blue goes through magenta (the shorter hue arc), never grey.
        let mid = lerp_color_hsv(red, blue, 0.5);
        let (h, s, v) = to_hsv(mid);
        assert!((h - 300.0).abs() < 2.0 && s > 0.99 && v > 0.99, "{mid:?}");
        assert_eq!(lerp_color_hsv(red, blue, 0.0), red);
        assert_eq!(lerp_color_hsv(red, blue, 1.0), blue);
        assert_eq!(FADE, Duration::from_millis(250));
        // The live fade.
        let _g = test_lock();
        follow("arc_cyan");
        std::thread::sleep(FADE + Duration::from_millis(20));
        assert_eq!(current(), preset("arc_cyan").unwrap());
        assert!(follow("stark_gold"));
        assert!(fading());
        let during = current();
        assert_ne!(during, preset("stark_gold").unwrap(), "mid-fade");
        assert!(!follow("stark_gold"), "same target: no restart");
        std::thread::sleep(FADE + Duration::from_millis(20));
        assert_eq!(current(), preset("stark_gold").unwrap());
        follow(DEFAULT_PRESET);
    }

    #[test]
    fn hdr_neon_is_additive_full_saturation_light() {
        assert_eq!(NEON_PASSES.map(|p| (p.0, p.1)), [(22.0, 28), (10.0, 75), (4.2, 220), (1.5, 255)]);
        assert_eq!(NEON_PASSES[3].2, NeonLayer::HotCenter);
        // A dull rose (s 0.6, v 0.7) comes out as pure neon at full alpha.
        let c = hdr_neon_color([180, 72, 108], 255);
        assert_eq!(c.a(), 0, "premultiplied additive");
        let (_, s, v) = to_hsv([c.r(), c.g(), c.b()]);
        assert!(s > 0.99 && v > 0.99, "{c:?}");
        // The aura pass is faint light, scaled by its alpha.
        let aura = hdr_neon_color([255, 0, 85], 28);
        assert!(aura.r() <= 28 && aura.r() > 20 && aura.a() == 0);
    }

    #[test]
    fn jai_designs_saves_and_reloads_custom_themes() {
        let t = design_theme(Some("Cyberpunk"), "neon yellow borders and electric purple core").unwrap();
        assert_eq!(t.name, "Cyberpunk");
        let (bh, bs, bv) = to_hsv(t.window_border);
        assert!((45.0..=65.0).contains(&bh) && bs > 0.99 && bv > 0.99, "neon yellow border {:?}", t.window_border);
        let (ph, ..) = to_hsv(t.core_primary);
        assert!((ph - 300.0).abs() < 40.0 || (ph - 270.0).abs() < 40.0, "purple core {:?}", t.core_primary);
        let stealth = design_theme(None, "design a dark stealth theme for me").unwrap();
        assert_eq!(stealth.name, "Dark Stealth");
        assert!(to_hsv(stealth.window_border).2 < 0.6 && stealth.background_tint == [2, 3, 5]);
        assert!(design_theme(None, "something nice").is_none(), "no colour, no mood: nothing invented");
        // Saved at once, found again by any spelling, resolvable as a palette, and on disk.
        save_custom(t.clone()).unwrap();
        assert_eq!(custom("my cyberpunk theme").as_ref(), Some(&t));
        assert_eq!(resolve(&t.key()).hud_frame_border, t.window_border);
        assert!(load_custom(&store_path()).contains(&t), "persisted to custom_themes.json");
        let mut renamed = t.clone();
        renamed.core_secondary = [1, 2, 3];
        save_custom(renamed.clone()).unwrap();
        assert_eq!(custom_themes().iter().filter(|x| x.name == "Cyberpunk").count(), 1, "re-saving replaces");
        let _ = std::fs::remove_file(store_path());
    }
}
