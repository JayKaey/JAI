//! "Jay, learn my voice" (Sir, 2026-11-12): a one-minute set-up. Sir reads six lines off the
//! board; from them JAI learns three things: how the recogniser writes his name in Sir's voice
//! (new spellings join the names he answers to), how loud Sir's voice is over his room (the
//! gate that opens his ears is set to it), and what Sir's voice sounds like (the print the
//! voice lock compares with: `speaker_id`). While it runs, what is said goes to the set-up and
//! not to him as an order.

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontFamily, FontId, RichText, Rounding, Stroke};
use std::sync::Mutex;
use std::time::Instant;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What Sir reads, one after the other (each opens with the name, as his orders do).
pub const LINES: [&str; 6] = [
    "Jay, what time is it?",
    "Jay, open the mixer.",
    "Hey Jay, show me my calendar.",
    "Jay, mute everything except Discord.",
    "Jay, what is the weather tomorrow?",
    "Jay, good to meet you.",
];

/// What the set-up found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// New spellings of his name he now answers to.
    pub spellings: Vec<String>,
    /// The ear's gate before and after, when it was changed.
    pub gate: Option<(f32, f32)>,
    /// A voice print was made (the voice lock can be switched on).
    pub print: bool,
    /// Why no print was made, when none was.
    pub no_print: Option<String>,
}

impl Outcome {
    /// What he says when it is done.
    pub fn spoken(&self) -> String {
        let mut line = String::from("Thank you, Sir.");
        if self.print {
            line.push_str(" I know your voice now: say \"only obey me\" whenever I should answer to you alone.");
        } else if let Some(why) = &self.no_print {
            line.push_str(&format!(" I could not take a print of your voice: {why}."));
        }
        match self.spellings.as_slice() {
            [] => {}
            [one] => line.push_str(&format!(" I will also answer to \"{one}\", which is how your name for me reaches my ears.")),
            many => line.push_str(&format!(" I will also answer to {}.", many.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(" and "))),
        }
        if let Some((from, to)) = self.gate {
            line.push_str(if to < from { " Your voice is on the quiet side here, so I have opened my ears further." } else { " Your voice carries well here, so I have narrowed my ears against the room." });
        }
        line
    }
}

struct Setup {
    step: usize,
    /// The prints of the lines read so far.
    prints: Vec<Vec<f32>>,
    /// What the recogniser wrote for each line.
    heard: Vec<String>,
    /// For each line: the loudest Sir's voice was and the room's noise (dBFS).
    levels: Vec<(f32, f32)>,
    /// The line as it was last heard, and whether it was taken.
    last: Option<(String, bool)>,
    since: Instant,
    outcome: Option<Outcome>,
}

static SETUP: Mutex<Option<Setup>> = Mutex::new(None);

/// Whether the set-up is listening (what is said goes to it, not to JAI).
pub fn active() -> bool {
    lock(&SETUP).as_ref().is_some_and(|s| s.outcome.is_none())
}

pub fn begin() {
    *lock(&SETUP) = Some(Setup { step: 0, prints: Vec::new(), heard: Vec::new(), levels: Vec::new(), last: None, since: Instant::now(), outcome: None });
    tracing::info!("[VOICE] The voice set-up begins");
}

pub fn cancel() {
    if lock(&SETUP).take().is_some() {
        tracing::info!("[VOICE] The voice set-up was closed");
    }
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// Whether `heard` is the line `expected` as the recogniser might write it: most of the line's
/// words after the name are there (the name itself is what is being learnt).
pub fn is_the_line(heard: &str, expected: &str) -> bool {
    let want: Vec<String> = words(expected).into_iter().filter(|w| !matches!(w.as_str(), "jay" | "hey")).collect();
    let got = words(heard);
    let found = want.iter().filter(|w| got.iter().any(|g| g == *w || strsim::jaro_winkler(g, w) >= 0.88)).count();
    !want.is_empty() && found * 10 >= want.len() * 6
}

/// How the recogniser wrote the name in `heard`, given the line that was read: the word in
/// front of the line's first word after the name ("Chay, what time is it?" → "chay"). None
/// when the line's own words are not found or nothing stands in front of them.
pub fn name_as_written(heard: &str, expected: &str) -> Option<String> {
    let first = words(expected).into_iter().find(|w| !matches!(w.as_str(), "jay" | "hey"))?;
    let got = words(heard);
    let at = got.iter().position(|g| *g == first || strsim::jaro_winkler(g, &first) >= 0.9)?;
    let before: Vec<&String> = got[..at].iter().filter(|w| !matches!(w.as_str(), "hey" | "hi" | "okay" | "ok")).collect();
    match before.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

/// Words that are never learnt as his name, however they were heard: too common in talk.
const NEVER: [&str; 22] = ["day", "they", "hey", "say", "way", "may", "pay", "the", "a", "ok", "okay", "yes", "j", "hay", "ja", "je", "yeah", "hi", "i", "he", "we", "ya"];

/// The spellings worth learning from what was heard for the lines: written like the name at
/// least twice (once is a slip of the recogniser), short, sounding like it, not a common word,
/// and not one he answers to already (`known`).
pub fn learnt_spellings(heard: &[String], known: impl Fn(&str) -> bool) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (text, line) in heard.iter().zip(LINES) {
        let Some(name) = name_as_written(text, line) else { continue };
        match counts.iter_mut().find(|(n, _)| *n == name) {
            Some(entry) => entry.1 += 1,
            None => counts.push((name, 1)),
        }
    }
    counts
        .into_iter()
        .filter(|(name, n)| {
            *n >= 2
                && (2..=6).contains(&name.chars().count())
                && name.chars().all(char::is_alphabetic)
                && !NEVER.contains(&name.as_str())
                && strsim::jaro_winkler(name, "jay") >= 0.55
                && !known(name)
        })
        .map(|(name, _)| name)
        .collect()
}

/// The ear's gate (how many times the room's noise a sound must be to open it) for a voice
/// `over_db` decibels over the room: opened for a quiet voice, narrowed for a strong one, and
/// left as it is (None) when the change would be small.
pub fn gate_for(over_db: f32, current: f32) -> Option<f32> {
    let wanted = 10f32.powf((over_db - 14.0) / 20.0).clamp(1.5, 4.0);
    ((wanted - current).abs() / current.max(0.1) > 0.2).then_some((wanted * 10.0).round() / 10.0)
}

/// The loudest a clip's voice is (the loudest tenth of a second, dBFS).
pub fn peak_db(pcm: &[f32]) -> f32 {
    let loudest = pcm.chunks(1600).map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len().max(1) as f32).sqrt()).fold(0.0f32, f32::max);
    20.0 * loudest.max(1e-6).log10()
}

/// A sentence heard while the set-up runs: taken as the next line when it is that line (its
/// print and its levels with it), else asked for again. Returns whether the set-up took it.
pub fn heard(text: &str, print: Option<Vec<f32>>, levels: (f32, f32)) -> bool {
    let mut guard = lock(&SETUP);
    let Some(setup) = guard.as_mut().filter(|s| s.outcome.is_none()) else { return false };
    let Some(line) = LINES.get(setup.step) else { return true };
    let ok = is_the_line(text, line);
    setup.last = Some((text.trim().to_string(), ok));
    if ok {
        setup.heard.push(text.to_string());
        setup.levels.push(levels);
        setup.prints.extend(print);
        setup.step += 1;
        setup.since = Instant::now();
    }
    tracing::info!("[VOICE] Set-up line {}: heard '{}' ({})", setup.step.min(LINES.len()), text.trim(), if ok { "taken" } else { "not the line" });
    true
}

/// When every line is read: what the set-up has (taken once, for the app to learn from).
pub fn take_finished() -> Option<(Vec<Vec<f32>>, Vec<String>, Vec<(f32, f32)>)> {
    let mut guard = lock(&SETUP);
    let setup = guard.as_mut().filter(|s| s.outcome.is_none() && s.step >= LINES.len())?;
    setup.outcome = Some(Outcome::default());
    Some((std::mem::take(&mut setup.prints), std::mem::take(&mut setup.heard), std::mem::take(&mut setup.levels)))
}

/// The app has learnt from the set-up: its block shows what came of it.
pub fn finished(outcome: Outcome) {
    if let Some(setup) = lock(&SETUP).as_mut() {
        setup.outcome = Some(outcome);
        setup.since = Instant::now();
    }
}

/// The set-up's block id.
pub const ID: &str = "voice_setup";

/// The voice as the block hears it: the microphone's level of the last seconds (0 … 1, the
/// newest last) and when the last one was taken (the frame clock).
static TRACE: Mutex<(f64, std::collections::VecDeque<f32>)> = Mutex::new((0.0, std::collections::VecDeque::new()));
const TRACE_LEN: usize = 72;

/// Takes the microphone's level into the trace, twenty times a second.
fn trace(now: f64, level: f32) -> Vec<f32> {
    let mut t = lock(&TRACE);
    if now - t.0 >= 0.05 {
        t.0 = now;
        t.1.push_back(level.clamp(0.0, 1.0));
        while t.1.len() > TRACE_LEN {
            t.1.pop_front();
        }
    }
    t.1.iter().copied().collect()
}

/// The trace set by hand (the gallery).
#[cfg(test)]
pub fn show_trace(levels: &[f32]) {
    *lock(&TRACE) = (f64::MAX, levels.iter().copied().collect());
}

/// Paints the trace: one bar per twentieth of a second, the newest on the right, mirrored
/// about its middle line.
fn paint_trace(p: &egui::Painter, centre: egui::Pos2, levels: &[f32], accent: Color32) {
    const STEP: f32 = 4.0;
    let left = centre.x - TRACE_LEN as f32 * STEP / 2.0;
    p.line_segment([pos2(left, centre.y), pos2(left + TRACE_LEN as f32 * STEP, centre.y)], Stroke::new(1.0f32, crate::hud_style::white(0.08)));
    let offset = TRACE_LEN - levels.len().min(TRACE_LEN);
    for (k, level) in levels.iter().rev().take(TRACE_LEN).rev().enumerate() {
        let half = 1.0 + 11.0 * level.sqrt();
        let x = left + (offset + k) as f32 * STEP + 1.0;
        // (Older bars fade towards the left.)
        let age = 0.35 + 0.65 * (offset + k) as f32 / TRACE_LEN as f32;
        p.rect_filled(egui::Rect::from_min_max(pos2(x, centre.y - half), pos2(x + 2.0, centre.y + half)), Rounding::ZERO, accent.gamma_multiply((0.3 + 0.7 * level) * age));
    }
}

/// The set-up's block while it runs and for a while after: the line to read, large; what was
/// heard; how far it is.
pub fn show(ctx: &egui::Context, mic: f32) {
    if crate::hud_feel::take_voice_setup() {
        begin();
        crate::hud_dock::cancel_close(ID);
    }
    let Some((step, last, outcome, age)) = lock(&SETUP).as_ref().map(|s| (s.step, s.last.clone(), s.outcome.clone(), s.since.elapsed())) else {
        crate::hud_dock::forget(ID);
        return;
    };
    let accent = crate::hud_board::active_colour();
    let ink = Color32::from_rgb(0xE8, 0xF2, 0xFC);
    let muted = Color32::from_rgb(0x8E, 0xA4, 0xB8);
    let closed = crate::gui::block(ctx, ID, "Voice set-up // learn my voice", [620.0, 330.0], |ui| {
        let rect = ui.available_rect_before_wrap();
        let p = ui.painter_at(rect);
        // How far it is: six marks.
        for k in 0..LINES.len() {
            let mark = egui::Rect::from_min_size(pos2(rect.left() + k as f32 * 30.0, rect.top() + 4.0), vec2(24.0, 4.0));
            p.rect_filled(mark, Rounding::ZERO, if k < step { accent } else { crate::hud_style::white(0.12) });
        }
        let centre = pos2(rect.center().x, rect.top() + rect.height() * 0.42);
        match &outcome {
            None => {
                p.text(pos2(rect.left(), rect.top() + 26.0), Align2::LEFT_TOP, format!("READ THIS ALOUD  ·  {} OF {}", (step + 1).min(LINES.len()), LINES.len()), FontId::monospace(10.5), muted);
                let line = LINES.get(step).copied().unwrap_or("");
                let galley = p.layout(line.to_string(), FontId::new(30.0, FontFamily::Name("display".into())), ink, rect.width() - 20.0);
                p.galley(pos2(centre.x - galley.size().x / 2.0, centre.y - galley.size().y / 2.0), galley, ink);
                if let Some((text, ok)) = &last {
                    let (mark, colour) = if *ok { ("HEARD", crate::hud_style::GOOD) } else { ("ONCE MORE, PLEASE  ·  I HEARD", crate::hud_style::warn_colour()) };
                    p.text(pos2(rect.center().x, rect.bottom() - 34.0), Align2::CENTER_CENTER, format!("{mark}:  \u{201C}{text}\u{201D}"), FontId::monospace(11.0), colour);
                }
                p.text(pos2(rect.center().x, rect.bottom() - 12.0), Align2::CENTER_CENTER, "Speak as you always do. Close the block to stop.", FontId::proportional(11.5), muted);
                // His voice as it comes in.
                let levels = trace(ctx.input(|i| i.time), mic);
                paint_trace(&p, pos2(rect.center().x, rect.bottom() - 66.0), &levels, accent);
            }
            Some(o) if *o == Outcome::default() => {
                p.text(centre, Align2::CENTER_CENTER, "LEARNING…", FontId::new(22.0, FontFamily::Name("display".into())), accent);
            }
            Some(o) => {
                let mut y = rect.top() + 30.0;
                let mut say = |text: String, colour: Color32| {
                    let galley = p.layout(text, FontId::proportional(14.0), colour, rect.width() - 10.0);
                    let h = galley.size().y;
                    p.galley(pos2(rect.left(), y), galley, colour);
                    y += h + 10.0;
                };
                say(if o.print { "I know your voice now. Say \u{201C}only obey me\u{201D} and I answer to you alone.".to_string() } else { format!("No voice print: {}.", o.no_print.clone().unwrap_or_else(|| "the lines were too short".into())) }, if o.print { ink } else { crate::hud_style::warn_colour() });
                say(if o.spellings.is_empty() { "Your name for me reaches my ears as I knew it: nothing new to learn there.".to_string() } else { format!("I will also answer to: {}.", o.spellings.join(", ")) }, ink);
                say(match o.gate { Some((from, to)) => format!("My ears' gate went from {from:.1} to {to:.1} times the room's noise."), None => "My ears' gate fits your voice and your room as it is.".to_string() }, ink);
            }
        }
        let _ = (RichText::new(""), Stroke::NONE);
    });
    // Closed by hand, or done and looked at for a while: it goes.
    if closed || (outcome.as_ref().is_some_and(|o| *o != Outcome::default()) && age.as_secs() > 25) {
        cancel();
        crate::hud_dock::forget(ID);
    }
    if outcome.is_none() {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_known_by_its_words_whatever_the_name_was_heard_as() {
        assert!(is_the_line("Jay, what time is it?", LINES[0]));
        assert!(is_the_line("Chay what time is it", LINES[0]));
        assert!(is_the_line("J, what time is that?", LINES[0]), "most of its words");
        assert!(!is_the_line("Jay, open the mixer.", LINES[0]));
        assert!(!is_the_line("what", LINES[0]));
        assert!(is_the_line("Hey Jae, show me my calender.", LINES[2]));
    }

    #[test]
    fn the_name_is_what_stands_in_front_of_the_line() {
        assert_eq!(name_as_written("Chay, what time is it?", LINES[0]).as_deref(), Some("chay"));
        assert_eq!(name_as_written("Hey Jae, show me my calendar.", LINES[2]).as_deref(), Some("jae"));
        assert_eq!(name_as_written("What time is it?", LINES[0]), None, "no name was written");
        assert_eq!(name_as_written("So Jay then what time is it", LINES[0]), None, "more than a name in front");
        assert_eq!(name_as_written("Jay, the mixer please.", LINES[0]), None, "another line");
    }

    #[test]
    fn a_spelling_is_learnt_when_it_came_twice_and_is_no_common_word() {
        let heard = |names: [&str; 6]| -> Vec<String> { names.iter().zip(LINES).map(|(n, line)| line.replacen("Jay", n, 1)).collect() };
        let known = |w: &str| matches!(w, "jay" | "jai" | "jae");
        assert_eq!(learnt_spellings(&heard(["Jay", "Chay", "Jay", "Chay", "Jay", "Jay"]), known), ["chay"], "what the recogniser writes for his name twice is learnt");
        assert_eq!(learnt_spellings(&heard(["Kevin", "Kevin", "Kevin", "Jay", "Jay", "Jay"]), known), Vec::<String>::new(), "a word that sounds nothing like it is not");
        assert_eq!(learnt_spellings(&heard(["Jey", "Jay", "Jey", "Jay", "Jay", "Jay"]), known), ["jey"]);
        assert_eq!(learnt_spellings(&heard(["Jey", "Jay", "Jay", "Jay", "Jay", "Jay"]), known), Vec::<String>::new(), "once is a slip");
        assert_eq!(learnt_spellings(&heard(["Day", "Day", "Day", "Jay", "Jay", "Jay"]), known), Vec::<String>::new(), "a common word is never his name");
        assert_eq!(learnt_spellings(&heard(["Jae", "Jae", "Jae", "Jay", "Jay", "Jay"]), known), Vec::<String>::new(), "he answers to it already");
        assert_eq!(learnt_spellings(&heard(["JayKaey", "JayKaey", "Jay", "Jay", "Jay", "Jay"]), known), Vec::<String>::new(), "seven letters are no name of his");
    }

    #[test]
    fn the_gate_follows_the_voice_over_the_room() {
        assert_eq!(gate_for(16.0, 3.0), Some(1.5), "a quiet voice: the gate opens as far as it goes");
        assert_eq!(gate_for(24.0, 3.0), None, "a voice that fits the gate leaves it alone");
        assert_eq!(gate_for(40.0, 2.5), Some(4.0), "a strong voice: narrowed against the room");
        assert_eq!(gate_for(22.0, 2.5), None);
        let loud: Vec<f32> = (0..16_000).map(|i| 0.5 * (i as f32 * 0.2).sin()).collect();
        assert!((peak_db(&loud) + 9.0).abs() < 0.5, "{}", peak_db(&loud));
    }

    #[test]
    fn the_setup_takes_its_lines_in_order_and_asks_again_for_a_wrong_one() {
        static ONE: Mutex<()> = Mutex::new(());
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        *lock(&SETUP) = None;
        assert!(!heard("Jay, what time is it?", None, (-20.0, -50.0)), "nothing is taken while no set-up runs");
        begin();
        assert!(active());
        assert!(heard("Jay, open the mixer.", Some(vec![1.0]), (-20.0, -50.0)), "a wrong line is the set-up's too");
        assert_eq!(lock(&SETUP).as_ref().map(|s| (s.step, s.prints.len())), Some((0, 0)));
        for line in LINES {
            assert!(heard(line, Some(vec![1.0, 0.0]), (-20.0, -50.0)));
        }
        let (prints, texts, levels) = take_finished().expect("all six lines read");
        assert_eq!((prints.len(), texts.len(), levels.len()), (6, 6, 6));
        assert!(take_finished().is_none() && !active(), "taken once; nothing more is listened to");
        finished(Outcome { print: true, ..Outcome::default() });
        assert!(lock(&SETUP).as_ref().unwrap().outcome.as_ref().unwrap().print);
        let all = Outcome { spellings: vec!["jey".into()], gate: Some((3.0, 1.8)), print: true, no_print: None };
        assert!(all.spoken().contains("only obey me") && all.spoken().contains("\"jey\"") && all.spoken().contains("opened my ears"));
        cancel();
        assert!(!active());
    }
}
