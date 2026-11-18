//! Themes of his own: saved by name, designed from a description ("a dark stealth theme with
//! neon yellow borders"), and the names a setting may carry: a preset, "theme:<name>" for a
//! saved one, "custom:#RRGGBB" for a bare colour. The colours themselves are `holo_theme`.

use crate::holo_theme::{self, from_hsv, mix, to_hsv, Theme, CUSTOM_PREFIX, DEFAULT_PRESET, PRESETS};
use std::sync::{LazyLock, RwLock};

/// Where saved themes live (next to JAI.exe); mirrored into the memory database.
pub const CUSTOM_THEMES_PATH: &str = "config/custom_themes.json";

/// A theme Sir built in the theme studio or JAI designed from a description. (The field names
/// are the file's: themes saved before the hologram look still load.)
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CustomTheme {
    pub name: String,
    /// The light itself.
    pub core_primary: [u8; 3],
    /// The same light further back.
    pub core_secondary: [u8; 3],
    /// What frames and text are tinted with (the same as the light: plain white ink).
    pub window_border: [u8; 3],
    /// The canvas.
    pub background_tint: [u8; 3],
}

const INK: [u8; 3] = [0xEE, 0xF3, 0xF7];

impl CustomTheme {
    pub fn theme(&self) -> Theme {
        let derived = Theme::of_colour(self.core_primary);
        // A frame colour of its own tints the structure; text must still read, so only a third.
        let ink = if self.window_border == self.core_primary { derived.ink } else { mix(INK, self.window_border, 0.30) };
        // The canvas stays dark whatever was asked: light is drawn on it.
        let (h, s, v) = to_hsv(self.background_tint);
        Theme { deep: self.core_secondary, ink, void: from_hsv(h, s, v.min(0.07)), ..derived }
    }

    /// The value a setting carries for it.
    pub fn key(&self) -> String {
        format!("{CUSTOM_PREFIX}{}", self.name)
    }

    /// A theme kept under `name`.
    pub fn from_theme(name: &str, t: &Theme) -> Self {
        Self { name: name.trim().to_string(), core_primary: t.accent, core_secondary: t.deep, window_border: t.accent, background_tint: t.void }
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

/// The theme a stored value names: a preset (a former name too), a saved theme
/// ("theme:Cyberpunk") or "custom:#RRGGBB"; his yellow for anything else.
pub fn resolve(name: &str) -> Theme {
    if let Some(t) = holo_theme::preset(name) {
        return t;
    }
    if let Some(t) = name.trim().strip_prefix(CUSTOM_PREFIX).and_then(custom) {
        return t.theme();
    }
    let hex = name.trim().trim_start_matches("custom:").trim_start_matches('#');
    match u32::from_str_radix(hex, 16) {
        Ok(v) if hex.len() == 6 => Theme::of_colour([(v >> 16) as u8, (v >> 8) as u8, v as u8]),
        _ => holo_theme::preset(DEFAULT_PRESET).unwrap_or(PRESETS[0].2),
    }
}

/// What a colour request stores: the preset its word names, else "custom:#RRGGBB".
pub fn name_for(word: &str, colour: [u8; 3]) -> String {
    match holo_theme::preset_for_word(word) {
        Some(p) => p.to_string(),
        None => format!("custom:#{:02X}{:02X}{:02X}", colour[0], colour[1], colour[2]),
    }
}

/// Fades to the theme `name` stands for; says whether anything changed.
pub fn follow(name: &str) -> bool {
    if holo_theme::active_name() == name {
        return false;
    }
    holo_theme::show(name, resolve(name))
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

/// Moods JAI designs from ("design a dark stealth theme"): (word, light, deep tone, frames).
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

const fn rgb(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

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
        let t = Theme::of_colour(c);
        (primary, secondary, border) = (Some(t.accent), Some(t.deep), Some(t.accent));
    }
    let primary = primary.or(border)?;
    let border = border.unwrap_or(primary);
    let secondary = secondary.unwrap_or_else(|| Theme::of_colour(primary).deep);
    let dark = d.contains("dark") || d.contains("stealth") || d.contains("black") || d.contains("night");
    let (h, _, _) = to_hsv(primary);
    let background = background.unwrap_or(if dark { [2, 3, 5] } else { from_hsv(h, 0.6, 0.06) });
    let name = name.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| {
        let skip = ["a", "an", "the", "me", "for", "theme", "design", "create", "make", "build", "new", "custom", "please", "jai", "with", "and", "my"];
        let words: Vec<String> = d
            .split_whitespace()
            .filter(|w| !skip.contains(w))
            .take(2)
            .map(|w| {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            })
            .collect();
        if words.is_empty() { "Custom".into() } else { words.join(" ") }
    });
    Some(CustomTheme { name, core_primary: primary, core_secondary: secondary, window_border: border, background_tint: background })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_names_a_preset_a_saved_theme_or_a_bare_colour() {
        assert_eq!(resolve("stark_gold").accent, [0xFF, 0xD4, 0x00]);
        assert_eq!(resolve("jai_gold").accent, [0xFF, 0xD4, 0x00], "the name his settings file carries");
        assert_eq!(resolve("custom:#00E58A").accent, [0x00, 0xE5, 0x8A]);
        assert_eq!(resolve("nonsense"), holo_theme::preset(DEFAULT_PRESET).unwrap());
        assert_eq!(name_for("blue", [0, 0, 255]), "arc_reactor");
        assert_eq!(name_for("teal", [0, 128, 128]), "custom:#008080");
    }

    #[test]
    fn a_theme_is_designed_from_words() {
        let t = design_theme(None, "a dark stealth theme").unwrap();
        assert_eq!((t.name.as_str(), t.core_primary, t.background_tint), ("Dark Stealth", [0x7F, 0xA3, 0xB8], [2, 3, 5]));
        let t = design_theme(Some("Cyber"), "neon yellow borders and an electric purple core").unwrap();
        assert_eq!(t.name, "Cyber");
        let (hue, ..) = to_hsv(t.core_primary);
        assert!((250.0..310.0).contains(&hue), "the core is purple: {hue}");
        assert_ne!(t.window_border, t.core_primary);
        assert!(design_theme(None, "something nice").is_none());
    }

    /// A saved theme becomes a theme that reads: the canvas stays dark, the ink stays light,
    /// and a theme saved from the live one comes back as it was.
    #[test]
    fn a_saved_theme_keeps_the_canvas_dark_and_round_trips() {
        let loud = CustomTheme { name: "Loud".into(), core_primary: [255, 0, 80], core_secondary: [0, 200, 255], window_border: [255, 230, 0], background_tint: [200, 220, 255] };
        let t = loud.theme();
        assert!(t.void.iter().all(|c| *c < 24), "{:?}", t.void);
        assert!(t.ink.iter().all(|c| *c > 150), "{:?}", t.ink);
        assert_eq!((t.accent, t.deep), ([255, 0, 80], [0, 200, 255]));
        assert_eq!(loud.key(), "theme:Loud");
        let gold = holo_theme::preset("stark_gold").unwrap();
        let kept = CustomTheme::from_theme("Mine", &gold).theme();
        assert_eq!((kept.accent, kept.deep, kept.void), (gold.accent, gold.deep, gold.void));
    }

    #[test]
    fn saved_themes_are_found_by_what_he_calls_them() {
        let _one = holo_theme::test_lock();
        save_custom(CustomTheme { name: "Deep Sea".into(), core_primary: [0, 180, 255], core_secondary: [0, 255, 208], window_border: [0, 180, 255], background_tint: [2, 3, 5] }).unwrap();
        assert!(custom("my deep sea theme").is_some());
        assert_eq!(resolve("theme:Deep Sea").accent, [0, 180, 255]);
        assert!(save_custom(CustomTheme { name: " ".into(), core_primary: [0; 3], core_secondary: [0; 3], window_border: [0; 3], background_tint: [0; 3] }).is_err());
        let _ = std::fs::remove_file(store_path());
    }
}
