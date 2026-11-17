//! The True-OLED minimal look (the default since config v14) in the Stark way (v28): pure
//! `#000000` pixels-off black, blue-black cards with sharp 2 px corners and a 1 px steel hairline
//! that glows in Sir's accent when active, no scan lines or glass tint. The classic sci-fi HUD
//! stays one voice command ("switch to sci-fi UI") or one click on the title-bar pill away.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiDesignParadigm {
    #[default]
    ModernOledMinimal,
    ClassicSciFiHud,
}

impl UiDesignParadigm {
    pub fn id(self) -> &'static str {
        match self {
            Self::ModernOledMinimal => "modern_oled_minimal",
            Self::ClassicSciFiHud => "classic_scifi_hud",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ModernOledMinimal => "Minimal OLED",
            Self::ClassicSciFiHud => "Sci-Fi",
        }
    }

    pub fn spoken(self) -> &'static str {
        match self {
            Self::ModernOledMinimal => "the minimal OLED interface",
            Self::ClassicSciFiHud => "the classic sci-fi HUD",
        }
    }

    /// "oled", "minimal", "modern", "sci-fi", "classic", "stark hud" …
    pub fn parse(raw: &str) -> Option<Self> {
        let s: String = raw.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
        if s.contains("oled") || s.contains("minimal") || s.contains("modern") || s.contains("black") || s.contains("schwarz") {
            Some(Self::ModernOledMinimal)
        } else if s.contains("scifi") || s.contains("classic") || s.contains("klassisch") || s.contains("hud") || s.contains("futuristic") {
            Some(Self::ClassicSciFiHud)
        } else {
            None
        }
    }
}

/// The accent presets of the OLED look (name, #RRGGBB).
pub const ACCENTS: &[(&str, &str)] = &[
    ("Stark Red", "#FF2B3E"),
    ("Ice White", "#E4E4E7"),
    ("Electric Blue", "#3B82F6"),
    ("Stark Gold", "#F5B841"),
    ("Emerald", "#10B981"),
    ("Violet", "#8B5CF6"),
    ("Crimson", "#DC143C"),
    ("Sunset Orange", "#FB7A3C"),
];

/// "electric blue", "stark-gold", "Eisweiß" → the preset.
pub fn accent_preset(raw: &str) -> Option<(&'static str, &'static str)> {
    let s: String = raw.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    let alias = match s.as_str() {
        "stark" | "starkred" => "starkred",
        "white" | "icewhite" | "eisweiss" | "eisweiß" | "weiss" | "weiß" => "icewhite",
        "blue" | "electricblue" | "blau" | "elektrischblau" => "electricblue",
        "gold" | "starkgold" | "golden" => "starkgold",
        "green" | "emerald" | "grün" | "gruen" | "smaragd" => "emerald",
        "violet" | "purple" | "lila" | "violett" => "violet",
        "crimson" | "red" | "rot" | "karmesin" => "crimson",
        "orange" | "sunsetorange" | "sunset" => "sunsetorange",
        other => other,
    };
    ACCENTS.iter().copied().find(|(name, _)| name.to_lowercase().replace(' ', "") == alias)
}

/// The OLED palette (RGB triples, so the GUI and the tests share it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OledPalette {
    pub background: [u8; 3],
    pub surface: [u8; 3],
    pub well: [u8; 3],
    pub border_idle: [u8; 3],
    pub accent: [u8; 3],
    pub text: [u8; 3],
    pub muted: [u8; 3],
}

pub const BACKGROUND: [u8; 3] = [0, 0, 0];
pub const SURFACE: [u8; 3] = [6, 8, 12];
pub const BORDER_IDLE: [u8; 3] = [32, 40, 52];
/// Sharp, as a Stark panel is cut (the rounded 18 px cards of v14-v27 read as a phone app).
pub const CARD_ROUNDING: f32 = 2.0;

impl OledPalette {
    pub fn new(accent: [u8; 3]) -> Self {
        Self { background: BACKGROUND, surface: SURFACE, well: [10, 13, 19], border_idle: BORDER_IDLE, accent, text: [232, 242, 252], muted: [138, 156, 176] }
    }

    /// The hairline: idle zinc, the accent (softened) on hover/active.
    pub fn border(&self, active: bool) -> [u8; 3] {
        if active {
            crate::interface::mix(self.border_idle, self.accent, 0.65)
        } else {
            self.border_idle
        }
    }
}

/// "Switch to Sci-Fi UI", "switch to modern minimal UI", "OLED mode", "accent electric blue",
/// "wechsel zum Sci-Fi Design" → `configure_interface` arguments.
pub fn voice_command(text: &str) -> Option<Value> {
    use regex::Regex;
    use std::sync::LazyLock;
    static SWITCH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:(?:switch|change|go|turn|set)\s+(?:back\s+)?(?:to|into|on)\s+|(?:use|enable|activate)\s+|(?:wechsel|wechsle|schalte?|stell|mach)\s+(?:zum|zur|auf|in den|das)?\s*)?(?:the\s+|das\s+|den\s+)?(true\s+oled\s+(?:pitch\s+)?black|modern\s+minimal|minimal\s+oled|oled\s+minimal|modern\s+oled|oled|minimal|modern|classic\s+sci\s*-?\s*fi|sci\s*-?\s*fi|classic|klassische?n?)(?:\s+(?:ui|hud|interface|design|look|mode|modus|oberfläche|theme))*(?:\s+(?:again|wieder|um|bitte|please))*$").expect("paradigm expression")
    });
    static ACCENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:(?:set|change|make|switch)\s+(?:the\s+|my\s+)?(?:oled\s+)?accent(?:\s+colou?r)?\s+(?:to\s+)?|accent(?:\s+colou?r)?\s+|akzent(?:farbe)?\s+(?:auf\s+)?)(.+)$").expect("accent expression")
    });
    let s: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == '-' || c == ' ' { c } else { ' ' }).collect();
    let s = s.split_whitespace().filter(|w| !matches!(*w, "jai" | "please" | "bitte" | "sir" | "can" | "you" | "could")).collect::<Vec<_>>().join(" ");
    if let Some(c) = SWITCH.captures(&s) {
        let paradigm = UiDesignParadigm::parse(&c[1])?;
        return Some(json!({ "ui_paradigm": paradigm.id() }));
    }
    if let Some(c) = ACCENT.captures(&s) {
        let (name, _) = accent_preset(c[1].trim())?;
        return Some(json!({ "accent_preset": name }));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::InterfaceSettings;

    #[test]
    fn oled_minimal_is_the_default_and_pitch_black() {
        let s = InterfaceSettings::default();
        assert_eq!(s.ui_paradigm, UiDesignParadigm::ModernOledMinimal);
        let p = OledPalette::new(s.accent_rgb());
        assert_eq!(p.background, [0, 0, 0]);
        assert_eq!(p.surface, [6, 8, 12]);
        assert_eq!(p.border(false), [32, 40, 52]);
        assert_ne!(p.border(true), p.border(false));
        assert_eq!(CARD_ROUNDING, 2.0, "Stark panels are cut sharp");
        // Text and secondary text read on the cards (WCAG 4.5:1).
        let c = |c: [u8; 3]| eframe::egui::Color32::from_rgb(c[0], c[1], c[2]);
        for ink in [p.text, p.muted] {
            assert!(crate::hud_style::contrast(c(ink), c(p.well)) >= 4.5, "{ink:?}");
        }
        // Old configs without the field get the new default.
        let old: InterfaceSettings = serde_json::from_value(json!({"theme": "stark"})).unwrap();
        assert_eq!(old.ui_paradigm, UiDesignParadigm::ModernOledMinimal);
    }

    #[test]
    fn switching_paradigms_and_accents() {
        let s = InterfaceSettings::default();
        let c = crate::interface::configure(&s, &json!({"ui_paradigm": "classic_scifi_hud"})).unwrap();
        assert_eq!(c.settings.ui_paradigm, UiDesignParadigm::ClassicSciFiHud);
        let back = crate::interface::configure(&c.settings, &json!({"ui_paradigm": "oled"})).unwrap();
        assert_eq!(back.settings.ui_paradigm, UiDesignParadigm::ModernOledMinimal);
        let gold = crate::interface::configure(&back.settings, &json!({"accent_preset": "stark gold"})).unwrap();
        assert_eq!(gold.settings.accent, "#F5B841");
        assert_eq!(OledPalette::new(gold.settings.accent_rgb()).accent, [0xF5, 0xB8, 0x41]);
        assert!(crate::interface::configure(&s, &json!({"accent_preset": "plaid"})).is_err());
    }

    #[test]
    fn voice_switches() {
        assert_eq!(voice_command("Switch to Sci-Fi UI"), Some(json!({"ui_paradigm": "classic_scifi_hud"})));
        assert_eq!(voice_command("JAI, switch to modern minimal UI"), Some(json!({"ui_paradigm": "modern_oled_minimal"})));
        assert_eq!(voice_command("OLED mode"), Some(json!({"ui_paradigm": "modern_oled_minimal"})));
        assert_eq!(voice_command("wechsel zum Sci-Fi Design"), Some(json!({"ui_paradigm": "classic_scifi_hud"})));
        assert_eq!(voice_command("set the accent color to electric blue"), Some(json!({"accent_preset": "Electric Blue"})));
        assert_eq!(voice_command("Akzentfarbe auf Smaragd"), Some(json!({"accent_preset": "Emerald"})));
        assert_eq!(voice_command("switch to Discord"), None);
        assert_eq!(voice_command("play some classic rock"), None);
    }

    #[test]
    fn accent_presets_cover_the_spec() {
        for name in ["Stark Red", "Ice White", "Electric Blue", "Stark Gold", "Emerald", "Violet", "Crimson", "Sunset Orange"] {
            assert_eq!(accent_preset(name).map(|a| a.0), Some(name));
        }
        assert_eq!(accent_preset("Stark").map(|a| a.1), Some("#FF2B3E"));
    }
}
