//! The HUD's colours. A theme is one hue of light on black glass with white structure: the
//! accent says what is live, selected or measured, the deep tone is the same light further
//! back, ink draws frames, rules and text, and the alert colour is kept for faults alone.
//! (The Mark III rule: white graphics, colour only on information.)

use eframe::egui::Color32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, RwLock};
use std::time::{Duration, Instant};

/// How long one theme fades into the next.
pub const FADE: Duration = Duration::from_millis(320);

/// One theme: five colours, everything else is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Live, selected, measured: the light itself.
    pub accent: [u8; 3],
    /// The same light further back: far shells, counter-rings, a chart's second series.
    pub deep: [u8; 3],
    /// Structure and text.
    pub ink: [u8; 3],
    /// The canvas.
    pub void: [u8; 3],
    /// Faults only.
    pub alert: [u8; 3],
}

const fn rgb(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

const fn theme(accent: u32, deep: u32, ink: u32, void: u32, alert: u32) -> Theme {
    Theme { accent: rgb(accent), deep: rgb(deep), ink: rgb(ink), void: rgb(void), alert: rgb(alert) }
}

/// The presets, his yellow first.
pub const PRESETS: [(&str, &str, Theme); 8] = [
    ("stark_gold", "STARK GOLD", theme(0xFFD400, 0xFF8A00, 0xEEF3F7, 0x020304, 0xFF3B30)),
    ("mono", "MONO", theme(0xFFFFFF, 0x8E99A3, 0xF4F6F8, 0x000000, 0xFF3B30)),
    ("arc_reactor", "ARC REACTOR", theme(0x5CE1FF, 0x2B7CFF, 0xEAF6FF, 0x01040A, 0xFF4D3D)),
    ("hot_rod", "HOT ROD", theme(0xFF2D2D, 0xFFC21A, 0xFFF1EC, 0x050202, 0xFFE000)),
    ("gamma", "GAMMA", theme(0x6CFF9A, 0x18B98A, 0xEAF7EF, 0x010503, 0xFF4D3D)),
    ("vibranium", "VIBRANIUM", theme(0xB89CFF, 0x5B6CFF, 0xF1EEFF, 0x030208, 0xFF4D6A)),
    ("ember", "EMBER", theme(0xFF7A1A, 0xFF2D2D, 0xFFF3EA, 0x050302, 0xFFE000)),
    ("graphite", "GRAPHITE", theme(0x9FB4C8, 0x5F7388, 0xD5DEE6, 0x000000, 0xFF3B30)),
];

pub const DEFAULT_PRESET: &str = "stark_gold";

/// Names older settings files carry, and the preset each one is now.
const FORMER: [(&str, &str); 10] = [
    ("jai_gold", "stark_gold"),
    ("stark_red", "hot_rod"),
    ("rose_crimson", "hot_rod"),
    ("arc_cyan", "arc_reactor"),
    ("emerald_matrix", "gamma"),
    ("ultraviolet", "vibranium"),
    ("crimson_overdrive", "hot_rod"),
    ("solar_orange", "ember"),
    ("ice_monochrome", "mono"),
    ("oled", "mono"),
];

fn key(name: &str) -> String {
    name.trim().to_lowercase().replace([' ', '-'], "_")
}

/// A preset's own name for any spelling of it ("Arc Reactor", "arc-reactor", a former name).
pub fn preset_name(name: &str) -> Option<&'static str> {
    let key = key(name);
    let key = FORMER.iter().find(|(old, _)| *old == key).map_or(key.as_str(), |(_, now)| now);
    PRESETS.iter().map(|(n, _, _)| *n).find(|n| *n == key)
}

pub fn preset(name: &str) -> Option<Theme> {
    let name = preset_name(name)?;
    PRESETS.iter().find(|(n, _, _)| *n == name).map(|(_, _, t)| *t)
}

/// The preset after `name` (the first one after a theme of his own).
pub fn next_preset(name: &str) -> &'static str {
    let i = preset_name(name).and_then(|n| PRESETS.iter().position(|(p, _, _)| *p == n)).map_or(0, |i| i + 1);
    PRESETS[i % PRESETS.len()].0
}

/// A theme's name as the HUD prints it ("STARK GOLD").
pub fn label(name: &str) -> String {
    match preset_name(name) {
        Some(n) => PRESETS.iter().find(|(p, _, _)| *p == n).map(|(_, l, _)| l.to_string()).unwrap_or_default(),
        None => name.trim_start_matches(CUSTOM_PREFIX).replace('_', " ").to_uppercase(),
    }
}

/// The preset a spoken colour word means (English and German).
pub fn preset_for_word(word: &str) -> Option<&'static str> {
    let w = word.trim().to_lowercase();
    let w = w.trim_end_matches(['.', '!', '?']);
    if let Some(name) = preset_name(w) {
        return Some(name);
    }
    Some(match w {
        "yellow" | "gelb" | "gold" | "golden" | "golden yellow" | "goldgelb" | "jai" | "jai gold" | "jai yellow" | "default" | "standard" | "stark" => "stark_gold",
        "white" | "weiß" | "weiss" | "black and white" | "schwarz weiß" | "schwarz weiss" | "schwarzweiß" | "monochrome" | "silver" | "silber" | "ice" | "eis" => "mono",
        "cyan" | "blue" | "blau" | "türkis" | "tuerkis" | "turquoise" | "aqua" | "arc" | "hellblau" => "arc_reactor",
        // ("Make the interface rows": the recogniser's "rose".)
        "red" | "rot" | "blood red" | "blutrot" | "crimson" | "stark red" | "iron man" | "rose" | "roses" | "rows" | "pink" | "magenta" | "rosa" | "pinke" => "hot_rod",
        "green" | "grün" | "gruen" | "emerald" | "smaragd" | "matrix" | "hulk" | "banner" => "gamma",
        "purple" | "violet" | "lila" | "violett" | "lilac" | "ultraviolet" => "vibranium",
        "orange" | "amber" | "bernstein" | "solar" => "ember",
        "grey" | "gray" | "grau" | "steel" | "stahl" | "graphit" => "graphite",
        _ => return None,
    })
}

/// Themes of his own are stored under this prefix ("theme:midnight").
pub const CUSTOM_PREFIX: &str = "theme:";

impl Theme {
    pub fn accent(&self) -> Color32 {
        c32(self.accent)
    }

    pub fn deep(&self) -> Color32 {
        c32(self.deep)
    }

    pub fn ink(&self) -> Color32 {
        c32(self.ink)
    }

    pub fn void(&self) -> Color32 {
        c32(self.void)
    }

    pub fn alert(&self) -> Color32 {
        c32(self.alert)
    }

    /// The filament at the middle of a line of light: the accent, mostly white.
    pub fn hot(&self) -> Color32 {
        c32(mix(self.accent, [255, 255, 255], 0.62))
    }

    /// Text that steps back: captions, units, what is not selected.
    pub fn muted(&self) -> Color32 {
        c32(mix(self.void, self.ink, 0.58))
    }

    /// The faintest ink that still reads as a label.
    pub fn faint(&self) -> Color32 {
        c32(mix(self.void, self.ink, 0.34))
    }

    /// A caution short of a fault: between the accent's warmth and the alert.
    pub fn warn(&self) -> Color32 {
        c32(mix(self.deep, self.alert, 0.35))
    }

    /// The glass a panel is made of: the void, a breath lighter, mostly opaque.
    pub fn glass(&self) -> Color32 {
        let [r, g, b] = mix(self.void, self.ink, 0.035);
        Color32::from_rgba_unmultiplied(r, g, b, 232)
    }

    /// A theme whose whole light is one colour of his choosing: the deep tone is the same
    /// hue turned a little and darker, ink and void take a breath of it.
    pub fn of_colour(colour: [u8; 3]) -> Theme {
        let (h, s, v) = to_hsv(colour);
        let deep = from_hsv((h - 22.0).rem_euclid(360.0), (s * 1.05).min(1.0), v * 0.82);
        let alert = if !(25.0..335.0).contains(&h) && s > 0.4 { [0xFF, 0xE0, 0x00] } else { [0xFF, 0x3B, 0x30] };
        Theme { accent: colour, deep, ink: mix([0xEE, 0xF3, 0xF7], colour, 0.05), void: mix([2, 3, 4], colour, 0.012), alert }
    }
}

fn c32(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// `a` towards `b` by `t` (0-1).
pub fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

/// RGB → (hue 0-360, saturation 0-1, value 0-1).
pub fn to_hsv(c: [u8; 3]) -> (f32, f32, f32) {
    let [r, g, b] = c.map(|v| v as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

pub fn from_hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r, g, b].map(|v| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

fn lerp(a: &Theme, b: &Theme, t: f32) -> Theme {
    Theme { accent: mix(a.accent, b.accent, t), deep: mix(a.deep, b.deep, t), ink: mix(a.ink, b.ink, t), void: mix(a.void, b.void, t), alert: mix(a.alert, b.alert, t) }
}

// ------------------------------------------------------------------------- the live theme ----

struct Live {
    name: String,
    from: Theme,
    to: Theme,
    since: Instant,
}

static LIVE: LazyLock<RwLock<Live>> = LazyLock::new(|| {
    let t = PRESETS[0].2;
    RwLock::new(Live { name: DEFAULT_PRESET.to_string(), from: t, to: t, since: Instant::now() - FADE })
});
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Counts every change of theme (painters that cache colours watch it).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

fn live() -> std::sync::RwLockReadGuard<'static, Live> {
    LIVE.read().unwrap_or_else(|e| e.into_inner())
}

/// The theme as it is drawn right now (mid-fade: between the two).
pub fn current() -> Theme {
    #[cfg(test)]
    if let Some((look, _)) = LOOK.with(|l| l.get()) {
        return look;
    }
    let live = live();
    let t = (live.since.elapsed().as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0);
    if t >= 1.0 {
        live.to
    } else {
        lerp(&live.from, &live.to, 1.0 - (1.0 - t).powi(3))
    }
}

/// True while one theme fades into the next (the HUD keeps drawing).
pub fn fading() -> bool {
    live().since.elapsed() < FADE
}

pub fn active_name() -> String {
    live().name.clone()
}

/// Fades to `theme` under `name`; false when it is the theme already shown.
pub fn show(name: &str, theme: Theme) -> bool {
    let now = current();
    let mut live = LIVE.write().unwrap_or_else(|e| e.into_inner());
    if live.to == theme && live.name == name {
        return false;
    }
    *live = Live { name: name.to_string(), from: now, to: theme, since: Instant::now() };
    GENERATION.fetch_add(1, Ordering::Relaxed);
    true
}

/// Ends a fade at once (pictures are taken of the finished theme).
pub fn finish_fade() {
    let mut live = LIVE.write().unwrap_or_else(|e| e.into_inner());
    if live.since.elapsed() < FADE {
        live.since = Instant::now().checked_sub(FADE).unwrap_or(live.since);
        live.from = live.to;
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// The live theme is global: tests that change it run one at a time.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
thread_local! {
    /// The theme this thread takes its pictures in, whatever another test does to the live
    /// theme meanwhile, and the name of the folder they go to.
    static LOOK: std::cell::Cell<Option<(Theme, &'static str)>> = const { std::cell::Cell::new(None) };
}

/// Test pictures in one theme until the guard goes: his yellow, or the theme `JAI_LOOK` names
/// ("mono", "arc_reactor", …). They are saved in a folder of that theme's name
/// (`ui_snapshot::Canvas::save`), so the sets lie side by side.
#[cfg(test)]
pub(crate) fn look_for_pictures() -> LookGuard {
    let wanted = std::env::var("JAI_LOOK").unwrap_or_default();
    let name = preset_name(&wanted).unwrap_or(DEFAULT_PRESET);
    LOOK.with(|l| l.set(Some((preset(name).unwrap_or(PRESETS[0].2), name))));
    LookGuard
}

/// The folder this thread's pictures go to, if it takes them in a look.
#[cfg(test)]
pub(crate) fn picture_set() -> Option<&'static str> {
    LOOK.with(|l| l.get()).map(|(_, name)| name)
}

#[cfg(test)]
pub(crate) struct LookGuard;

#[cfg(test)]
impl Drop for LookGuard {
    fn drop(&mut self) {
        LOOK.with(|l| l.set(None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn his_yellow_is_the_default_and_black_and_white_is_a_preset() {
        assert_eq!(preset(DEFAULT_PRESET).unwrap().accent, [0xFF, 0xD4, 0x00]);
        let mono = preset("mono").unwrap();
        assert_eq!((mono.accent, mono.void), ([255, 255, 255], [0, 0, 0]));
    }

    #[test]
    fn former_names_and_spoken_words_find_a_preset() {
        for (old, now) in [("jai_gold", "stark_gold"), ("Arc Cyan", "arc_reactor"), ("ice-monochrome", "mono"), ("stark_red", "hot_rod")] {
            assert_eq!(preset_name(old), Some(now), "{old}");
        }
        for (word, now) in [("yellow", "stark_gold"), ("Gelb", "stark_gold"), ("black and white", "mono"), ("weiß", "mono"), ("blue", "arc_reactor"), ("rot", "hot_rod"), ("green", "gamma"), ("lila", "vibranium")] {
            assert_eq!(preset_for_word(word), Some(now), "{word}");
        }
        assert_eq!(preset_for_word("louder"), None);
        assert_eq!(next_preset("graphite"), "stark_gold");
        assert_eq!(label("jai_gold"), "STARK GOLD");
        assert_eq!(label("theme:deep_sea"), "DEEP SEA");
    }

    /// Every preset reads: ink and accent stand well clear of the void, the alert is not the
    /// accent, and the deep tone is not the accent either.
    #[test]
    fn every_preset_reads_on_its_own_canvas() {
        let lum = |c: [u8; 3]| {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2])
        };
        let contrast = |a: [u8; 3], b: [u8; 3]| (lum(a).max(lum(b)) + 0.05) / (lum(a).min(lum(b)) + 0.05);
        for (name, _, t) in PRESETS {
            assert!(contrast(t.ink, t.void) >= 12.0, "{name}: ink");
            assert!(contrast(t.accent, t.void) >= 4.5, "{name}: accent");
            let muted = t.muted();
            assert!(contrast([muted.r(), muted.g(), muted.b()], t.void) >= 4.5, "{name}: muted text");
            assert_ne!(t.alert, t.accent, "{name}");
            assert_ne!(t.deep, t.accent, "{name}");
        }
    }

    #[test]
    fn a_theme_of_one_colour_keeps_it_and_derives_the_rest() {
        let t = Theme::of_colour([0x00, 0xE5, 0x8A]);
        assert_eq!(t.accent, [0x00, 0xE5, 0x8A]);
        assert_ne!(t.deep, t.accent);
        assert_eq!(t.alert, [0xFF, 0x3B, 0x30]);
        // A red light takes a yellow alert: a red fault would not show on it.
        assert_eq!(Theme::of_colour([0xFF, 0x20, 0x20]).alert, [0xFF, 0xE0, 0x00]);
    }

    #[test]
    fn hsv_round_trips() {
        for c in [[255, 212, 0], [92, 225, 255], [12, 200, 90], [255, 255, 255], [0, 0, 0]] {
            let (h, s, v) = to_hsv(c);
            let back = from_hsv(h, s, v);
            for i in 0..3 {
                assert!((back[i] as i32 - c[i] as i32).abs() <= 1, "{c:?} -> {back:?}");
            }
        }
    }
}
