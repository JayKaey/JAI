//! How he powers up (Sir, 2026-11-12: "sometimes short and sometimes long, depending on how long
//! I haven't spoken with him and what time it is; when I haven't talked with him in a long time
//! he should also give me updates on what is new"). `plan` decides once per start which
//! sequence plays and whether a catch-up follows the greeting; `paint` draws it over the HUD.

use chrono::{DateTime, Local, Timelike};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Rounding, Shape, Stroke};
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Which sequence plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Back within a couple of hours: the rings assemble, and he is there.
    Quick,
    /// Back the same day: the rings assemble while his systems tick on.
    Short,
    /// After a long time away, or the first start of the day: the full power-up.
    Long,
}

impl Kind {
    /// How long the sequence runs.
    pub fn length(self) -> Duration {
        Duration::from_millis(match self {
            Kind::Quick => 1200,
            Kind::Short => 2200,
            Kind::Long => 5000,
        })
    }

    /// When in it the emblem ignites (the greeting is spoken from here).
    pub fn ignition(self) -> Duration {
        Duration::from_millis(match self {
            Kind::Quick => 500,
            Kind::Short => 1300,
            Kind::Long => 3700,
        })
    }
}

/// A long time away: from here on the full power-up plays and a catch-up follows.
pub const LONG_AWAY: Duration = Duration::from_secs(12 * 3600);
const SOME_TIME: Duration = Duration::from_secs(2 * 3600);

/// What this start does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub kind: Kind,
    /// How long ago Sir last spoke with him (None: never).
    pub away: Option<Duration>,
    /// After the greeting he tells what is new (a long time away, and not at night).
    pub catch_up: bool,
}

/// The sequence for a start `away` after the last conversation, at `hour`, varied by `seed`
/// (two starts under the same conditions need not look the same).
pub fn choose(away: Option<Duration>, hour: u32, seed: u64) -> Plan {
    let night = !(6..22).contains(&hour);
    let morning = (5..=10).contains(&hour);
    let long_away = away.is_none_or(|a| a >= LONG_AWAY);
    let kind = match away {
        None => Kind::Long,
        Some(a) if a >= LONG_AWAY => Kind::Long,
        // The first start of the morning after a night's rest is a full one.
        Some(a) if a >= Duration::from_secs(6 * 3600) && morning => Kind::Long,
        Some(a) if a >= SOME_TIME => if seed % 4 == 0 { Kind::Long } else { Kind::Short },
        Some(_) => if seed % 3 == 0 { Kind::Short } else { Kind::Quick },
    };
    // At night he keeps it down: nothing longer than the short one, and no report aloud.
    let kind = if night && kind == Kind::Long { Kind::Short } else { kind };
    Plan { kind, away, catch_up: long_away && !night }
}

static PLAN: OnceLock<Plan> = OnceLock::new();

/// This start's plan, decided on the first call from when Sir last spoke with him (`last`).
pub fn plan(last: Option<DateTime<chrono::Utc>>) -> Plan {
    *PLAN.get_or_init(|| {
        let now = Local::now();
        let away = last.and_then(|at| (now.with_timezone(&chrono::Utc) - at).to_std().ok());
        let plan = choose(away, now.hour(), now.timestamp() as u64 / 61);
        tracing::info!("[BOOT] {:?} start-up ({}){}", plan.kind, away_words(away).unwrap_or_else(|| "first conversation".into()), if plan.catch_up { ", with a catch-up" } else { "" });
        plan
    })
}

/// The plan, if this start has one (the HUD draws nothing before it is decided).
pub fn current() -> Option<Plan> {
    PLAN.get().copied()
}

/// "three days", "a week", "five hours": how long he was not spoken to, as he says it. None
/// for less than an hour (nothing worth mentioning).
pub fn away_words(away: Option<Duration>) -> Option<String> {
    let hours = away?.as_secs() / 3600;
    let count = |n: u64, unit: &str| match n {
        1 => format!("{} {unit}", if unit == "hour" { "an" } else { "a" }),
        n => format!("{} {unit}s", crate::hud_board::number_words(n as usize)),
    };
    Some(match hours {
        0 => return None,
        1..=23 => count(hours, "hour"),
        24..=167 => count(hours / 24, "day"),
        168..=729 => count(hours / 168, "week"),
        _ => count(hours / 730, "month"),
    })
}

/// What he adds to his greeting after a long time away ("It has been three days, Sir.").
pub fn away_line(plan: &Plan) -> Option<String> {
    let away = plan.away.filter(|a| *a >= LONG_AWAY)?;
    Some(format!("It has been {}, Sir.", away_words(Some(away))?))
}

// --------------------------------------------------------------------------------- paint ----

/// What a start really waits for, in the order it comes up; each line says READY only once its
/// part is (Sir's choice of 2026-11-15: real progress, not a fixed animation).
const PARTS: [&str; 4] = ["LANGUAGE ENGINE", "EARS · WHISPER", "VOICE · KOKORO", "BRAIN · MODEL"];
static UP: [AtomicBool; 4] = [const { AtomicBool::new(false) }; 4];

/// Part `k` of `PARTS` is up: 0 the language engine, 1 the ears, 2 the voice, 3 the brain.
pub fn up(k: usize) {
    if let Some(part) = UP.get(k) {
        part.store(true, Ordering::Relaxed);
    }
}

/// Blocks a background thread until the brain is loaded, `max` at most: the indexers' walks wait
/// for the model's cold load, which reads gigabytes from the same disk.
pub fn wait_for_brain(max: Duration) {
    let until = std::time::Instant::now() + max;
    while !UP[3].load(Ordering::Relaxed) && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// How much is up, not counting the brain: its cold load outlasts the sequence.
fn share_up(up: [bool; 4]) -> f32 {
    up[..3].iter().filter(|u| **u).count() as f32 / 3.0
}

/// Line `k`'s word until its part is up (then a drawn tick stands there): LOADING for the
/// brain, running dots for the rest.
fn line_state(k: usize, begun: f32) -> &'static str {
    match k {
        3 => "LOADING",
        _ => ["·", "··", "···"][((begun * 9.0) as usize) % 3],
    }
}

/// Seconds per letter as a line types itself on.
const TYPE_PER_CHAR: f32 = 0.022;

/// How much of `text` has typed itself on `begun` seconds after its line began (one letter at
/// least, the whole line at most).
fn typed(text: &str, begun: f32) -> &str {
    let n = ((begun / TYPE_PER_CHAR) as usize + 1).min(text.chars().count());
    text.char_indices().nth(n).map_or(text, |(at, _)| &text[..at])
}

/// A small tick, its right end at `right`, on the line through `y`.
fn tick(p: &egui::Painter, right: f32, y: f32, colour: Color32) {
    p.add(Shape::line(vec![pos2(right - 9.0, y), pos2(right - 6.0, y + 3.0), pos2(right, y - 4.0)], stroke(1.4, colour)));
}

fn stroke(width: f32, colour: Color32) -> Stroke {
    Stroke::new(width, colour)
}

fn arc(centre: egui::Pos2, radius: f32, from: f32, sweep: f32) -> Vec<egui::Pos2> {
    let n = ((sweep.abs() * radius / 6.0) as usize).clamp(4, 160);
    (0..=n).map(|k| centre + vec2((from + sweep * k as f32 / n as f32).cos(), (from + sweep * k as f32 / n as f32).sin()) * radius).collect()
}

/// How far ring `k` of `n` has drawn itself `t` of the way through the assembly (0-1): the
/// rings start one after the other and each closes smoothly.
pub fn ring_progress(k: usize, n: usize, t: f32) -> f32 {
    let lead = 0.5 * k as f32 / n.max(1) as f32;
    crate::hud_style::motion::ease_out(((t - lead) / 0.5).clamp(0.0, 1.0))
}

/// Paints the sequence over `screen` at `age` into it, the emblem assembling at `centre` with
/// `radius`. Returns false once it is over (nothing was painted).
pub fn paint(ctx: &egui::Context, screen: Rect, centre: egui::Pos2, radius: f32, kind: Kind, age: Duration) -> bool {
    let total = kind.length().as_secs_f32();
    let t = age.as_secs_f32();
    if t >= total {
        return false;
    }
    let ignite = kind.ignition().as_secs_f32();
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("hud_startup")));
    let accent = crate::hud_board::active_colour();
    // The dark the HUD comes out of: solid until the emblem ignites, then it clears.
    let cover = 1.0 - crate::hud_style::motion::ease_out(((t - ignite) / (total - ignite)).clamp(0.0, 1.0));
    p.rect_filled(screen, Rounding::ZERO, Color32::from_rgba_unmultiplied(2, 4, 8, (255.0 * cover) as u8));
    // (After the ignition the drawn rings give way to the emblem's own as the dark clears.)
    let alpha = cover.min(1.0) * cover.min(1.0);
    // The rings draw themselves, one after the other, until the ignition: the emblem's own
    // seven, at their own radii.
    let radii = [0.314, 0.4, 0.56, 0.64, 0.743, 0.869, 1.0];
    let assembly = (t / ignite).clamp(0.0, 1.0);
    for (k, share) in radii.iter().enumerate() {
        let progress = ring_progress(k, radii.len(), assembly);
        if progress <= 0.0 {
            continue;
        }
        let from = -TAU / 4.0 + k as f32 * 0.9 + t * if k % 2 == 0 { 0.5 } else { -0.35 };
        let width = if k == 2 || k == 6 { 2.4 } else { 1.2 };
        let colour = if k % 3 == 1 { Color32::WHITE } else { accent };
        p.add(Shape::line(arc(centre, radius * *share, from, TAU * progress), stroke(width, colour.gamma_multiply((0.35 + 0.65 * progress) * alpha))));
        // The point that draws the ring is white-hot.
        if progress < 1.0 {
            let head = from + TAU * progress;
            p.circle_filled(centre + vec2(head.cos(), head.sin()) * (radius * *share), 2.2, Color32::WHITE.gamma_multiply(alpha));
        }
    }
    // The ignition: one flash out of the centre.
    let since = t - ignite;
    if (0.0..0.45).contains(&since) {
        let flash = 1.0 - since / 0.45;
        p.circle_filled(centre, radius * (0.2 + 0.9 * (1.0 - flash)), Color32::WHITE.gamma_multiply(0.18 * flash * flash));
        p.circle_stroke(centre, radius * (1.0 + 0.5 * (1.0 - flash)), stroke(2.0, accent.gamma_multiply(0.6 * flash)));
    }
    // What is coming up, line by line: the real parts, each READY once it is (the quick one has
    // no time for words; the long one adds a bar).
    let lines: &[&str] = if kind == Kind::Quick { &[] } else { &PARTS };
    if !lines.is_empty() {
        let font = FontId::monospace(11.0);
        let left = centre.x - 150.0;
        // (Under the emblem's outermost ring, which stands at 1.28 of its radius.)
        let top = centre.y + radius * 1.3 + 46.0;
        let head = crate::hud_style::title_galley(&p, "J.A.I. // power-up", 13.0, accent.gamma_multiply(alpha), 300.0);
        p.galley(pos2(left, top - 30.0), head, Color32::PLACEHOLDER);
        // How much is really up, and the word once all of it is.
        let up: [bool; 4] = std::array::from_fn(|k| UP[k].load(Ordering::Relaxed));
        let share = share_up(up);
        let (reading, ink) = if share >= 1.0 { ("ONLINE".to_string(), Color32::WHITE) } else { (format!("{:.0} %", share * 100.0), accent) };
        p.text(pos2(left + 300.0, top - 21.0), Align2::RIGHT_CENTER, reading, font.clone(), ink.gamma_multiply(alpha));
        let per = ignite / lines.len() as f32;
        for (k, text) in lines.iter().enumerate() {
            let begun = t - k as f32 * per;
            if begun < 0.0 {
                break;
            }
            let done = up[k];
            let y = top + k as f32 * 17.0;
            let ink = if done { Color32::from_rgb(0x9A, 0xB0, 0xC2) } else { Color32::WHITE };
            // Each line types itself on; once its part is up a tick stands at its end.
            p.text(pos2(left, y), Align2::LEFT_CENTER, typed(text, begun), font.clone(), ink.gamma_multiply(alpha));
            if done {
                tick(&p, left + 300.0, y, accent.gamma_multiply(alpha));
            } else {
                p.text(pos2(left + 300.0, y), Align2::RIGHT_CENTER, line_state(k, begun), font.clone(), ink.gamma_multiply(alpha));
            }
        }
        if kind == Kind::Long {
            let bar = Rect::from_min_size(pos2(left, top + lines.len() as f32 * 17.0 + 6.0), vec2(300.0, 2.0));
            p.rect_filled(bar, Rounding::ZERO, accent.gamma_multiply(0.2 * alpha));
            p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * share, bar.height())), Rounding::ZERO, accent.gamma_multiply(alpha));
        }
    }
    ctx.request_repaint();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3600;

    #[test]
    fn the_longer_he_was_away_the_fuller_the_start() {
        let at = |hours: u64, hour: u32, seed: u64| choose(Some(Duration::from_secs(hours * H)), hour, seed);
        // Back within the hour: quick, now and then the short one.
        assert_eq!(at(0, 15, 1).kind, Kind::Quick);
        assert_eq!(at(1, 15, 3).kind, Kind::Short);
        // The same day: short, now and then the long one.
        assert_eq!(at(5, 15, 1).kind, Kind::Short);
        assert_eq!(at(5, 15, 4).kind, Kind::Long);
        // Days away: the full power-up and the news.
        assert_eq!(at(72, 15, 1), Plan { kind: Kind::Long, away: Some(Duration::from_secs(72 * H)), catch_up: true });
        assert_eq!(choose(None, 15, 1).kind, Kind::Long, "the first start ever");
        // The first start of the morning.
        assert_eq!(at(9, 7, 1).kind, Kind::Long);
        assert!(!at(9, 7, 1).catch_up, "a night's rest is not a long time away");
        // At night he keeps it down, however long it has been, and reports nothing aloud.
        assert_eq!(at(72, 23, 1), Plan { kind: Kind::Short, away: Some(Duration::from_secs(72 * H)), catch_up: false });
        assert_eq!(at(0, 2, 1).kind, Kind::Quick);
        // Not every start under the same conditions is the same.
        let kinds: std::collections::HashSet<Kind> = (0..12).map(|seed| at(1, 15, seed).kind).collect();
        assert_eq!(kinds.len(), 2);
    }

    #[test]
    fn how_long_it_has_been_in_his_words() {
        let words = |hours: u64| away_words(Some(Duration::from_secs(hours * H)));
        assert_eq!(words(0), None);
        assert_eq!(words(1).as_deref(), Some("an hour"));
        assert_eq!(words(5).as_deref(), Some("five hours"));
        assert_eq!(words(30).as_deref(), Some("a day"));
        assert_eq!(words(80).as_deref(), Some("three days"));
        assert_eq!(words(24 * 16).as_deref(), Some("two weeks"));
        assert_eq!(words(24 * 70).as_deref(), Some("two months"));
        let long = choose(Some(Duration::from_secs(80 * H)), 15, 1);
        assert_eq!(away_line(&long).as_deref(), Some("It has been three days, Sir."));
        assert_eq!(away_line(&choose(Some(Duration::from_secs(5 * H)), 15, 1)), None);
    }

    #[test]
    fn the_sequences_fit_their_time_and_the_rings_close_in_turn() {
        for kind in [Kind::Quick, Kind::Short, Kind::Long] {
            assert!(kind.ignition() < kind.length());
        }
        assert!(Kind::Quick.length() < Kind::Short.length() && Kind::Short.length() < Kind::Long.length());
        assert!(Kind::Long.length() <= Duration::from_secs(5));
        assert_eq!(ring_progress(0, 6, 0.0), 0.0);
        assert!(ring_progress(0, 6, 0.3) > ring_progress(3, 6, 0.3));
        assert_eq!(ring_progress(5, 6, 1.0), 1.0);
    }

    #[test]
    fn the_counter_fills_with_what_the_start_waits_for_and_the_brain_says_it_is_loading() {
        // The brain's cold load outlasts the sequence: it is not counted, so the rest reach 100 %.
        assert_eq!(share_up([true, true, true, false]), 1.0);
        assert_eq!(share_up([true, false, false, true]), 1.0 / 3.0);
        assert_eq!(share_up([false; 4]), 0.0);
        assert_eq!(line_state(3, 0.5), "LOADING");
        assert_eq!(line_state(1, 0.5), "··");
    }

    /// A line types itself on, a letter every 22 ms: one letter at once, never past its end,
    /// and whole letters only (the middle dot is two bytes).
    #[test]
    fn a_line_types_itself_on() {
        assert_eq!(typed("EARS · WHISPER", 0.0), "E");
        assert_eq!(typed("EARS · WHISPER", 0.12), "EARS ·");
        assert_eq!(typed("EARS · WHISPER", 5.0), "EARS · WHISPER");
    }
}
