//! The lines under the emblem (Sir, 2026-11-12): his own words while he speaks, what JAI is
//! working on while he thinks ("reading your calendar", and afterwards how long it took), the
//! song's lyrics, and the chip that asks before an order is carried out that came without his
//! name ("Heard: … – do it?"). Also the microphone's verdict for the voice ring ("too quiet").
//! The pipeline reports here (`heard`, `settled`, `step`, `took`, `offer`); the HUD paints.

use eframe::egui::{self, pos2, vec2, Color32, FontId, Pos2, Rect, Rounding, Ui};
use regex::Regex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------- live words ----

/// While Sir speaks, the words so far go to Whisper this often.
pub const LIVE_EVERY: Duration = Duration::from_millis(900);
/// A settled sentence stays this long before it fades.
const WORDS_STAY: Duration = Duration::from_millis(1400);

struct Words {
    text: String,
    at: Instant,
    /// The final transcript (no longer a guess at half a sentence).
    settled: bool,
}

static WORDS: Mutex<Option<Words>> = Mutex::new(None);

/// What of a transcript is shown: no sound tags ("[BLANK_AUDIO]", "(music)"), no full stop a
/// guess at half a sentence ends in, one space between words. None when nothing is left.
pub fn clean(text: &str, settled: bool) -> Option<String> {
    static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[[^\]]*\]|\([^)]*\)|\*[^*]*\*").expect("valid tag regex"));
    let text = TAGS.replace_all(text, " ");
    let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !settled {
        while text.ends_with(['.', ',', '…']) {
            text.pop();
        }
    }
    (text.chars().any(char::is_alphanumeric)).then_some(text)
}

/// A guess at the words so far (while he is still speaking). Not while Sir has told him not to
/// listen: what the microphone hears then is not put on screen (`listening_lock::may_show_heard`;
/// 2026-11-13: after "don't listen" a song in the room went on appearing here, line by line).
pub fn heard(text: &str) {
    if !crate::listening_lock::may_show_heard() {
        return;
    }
    if let Some(text) = clean(text, false) {
        let mut words = lock(&WORDS);
        // A settled sentence is never replaced by a late guess at part of it.
        if words.as_ref().is_none_or(|w| !w.settled || w.at.elapsed() > WORDS_STAY) {
            *words = Some(Words { text, at: Instant::now(), settled: false });
        }
    }
}

/// The sentence as it was understood.
pub fn settled(text: &str) {
    *lock(&WORDS) = clean(text, true).map(|text| Words { text, at: Instant::now(), settled: true });
}

/// Nothing came of what was heard (a blip, a discarded sound).
pub fn speech_over() {
    let mut words = lock(&WORDS);
    if words.as_ref().is_some_and(|w| !w.settled) {
        *words = None;
    }
}

/// The words to show now: (text, settled, strength 0-1).
fn words_now() -> Option<(String, bool, f32)> {
    let mut words = lock(&WORDS);
    let w = words.as_ref()?;
    // A guess that was on screen when Sir said "don't listen" goes with the lock: only the
    // sentence he was understood to say (the order itself) fades in its own time.
    if !w.settled && !crate::listening_lock::may_show_heard() {
        *words = None;
        return None;
    }
    let age = w.at.elapsed();
    // A guess nobody followed up on (the microphone went quiet) goes after a few seconds.
    let stay = if w.settled { WORDS_STAY } else { Duration::from_secs(4) };
    if age > stay + Duration::from_millis(250) {
        *words = None;
        return None;
    }
    let fade = 1.0 - (age.saturating_sub(stay).as_secs_f32() / 0.25).clamp(0.0, 1.0);
    Some((w.text.clone(), w.settled, fade))
}

// -------------------------------------------------------------------------------- steps ----

/// What a tool is doing, in the words shown under the emblem.
pub fn step_phrase(tool: &str) -> &'static str {
    let has = |words: &[&str]| words.iter().any(|w| tool.contains(w));
    if has(&["calendar"]) {
        "reading your calendar"
    } else if has(&["school", "webuntis", "timetable"]) {
        "reading your timetable"
    } else if has(&["itslearning", "lms"]) {
        "checking itslearning"
    } else if has(&["mail", "inbox"]) {
        "reading your mail"
    } else if has(&["research"]) {
        "researching"
    } else if has(&["search", "browse", "web_"]) {
        "searching the web"
    } else if has(&["weather"]) {
        "checking the weather"
    } else if has(&["music", "spotify", "sonora", "playlist"]) {
        "at the music"
    } else if has(&["volume", "mixer", "audio"]) {
        "at the mixer"
    } else if has(&["3d", "model"]) {
        "building the model"
    } else if has(&["image", "picture", "photo", "camera", "street"]) {
        "finding pictures"
    } else if has(&["earth", "globe", "satellite"]) {
        "turning the globe"
    } else if has(&["network", "radar"]) {
        "sweeping the network"
    } else if has(&["disk"]) {
        "reading the drive"
    } else if has(&["memor", "remember", "recall"]) {
        "looking through my notes"
    } else if has(&["file", "folder"]) {
        "at your files"
    } else if has(&["screen", "watch"]) {
        "looking at your screen"
    } else if has(&["whatsapp", "discord", "message", "contact"]) {
        "at your messages"
    } else if has(&["transit", "departure", "commute", "traffic"]) {
        "checking the roads and trains"
    } else if has(&["timer", "alarm", "reminder"]) {
        "setting the clock"
    } else if has(&["calc", "math"]) {
        "doing the sum"
    } else if has(&["window", "application", "app"]) {
        "at your windows"
    } else if has(&["interface", "hud", "view", "widget", "board"]) {
        "arranging the board"
    } else {
        "working on it"
    }
}

/// A turn as the lines follow it.
struct Turn {
    /// Since when he has been thinking (set by the HUD when the state turns to it).
    began: Option<Instant>,
    step: Option<&'static str>,
    /// How long the last turn took (ms) and when it ended.
    took: Option<(u64, Instant)>,
}

static TURN: Mutex<Turn> = Mutex::new(Turn { began: None, step: None, took: None });

/// A step shows once the turn has run this long (an instant command shows none)…
const STEP_AFTER: Duration = Duration::from_millis(350);
/// …and "done in" only after a turn that was worth a line, for this long.
const TOOK_FROM_MS: u64 = 600;
const TOOK_STAYS: Duration = Duration::from_millis(2500);

/// A tool begins its work.
pub fn step(tool: &str) {
    lock(&TURN).step = Some(step_phrase(tool));
}

/// The turn is over after `ms` (interrupted turns leave no line).
pub fn took(ms: u64, interrupted: bool) {
    let mut turn = lock(&TURN);
    turn.took = (!interrupted && turn.began.is_some() && ms >= TOOK_FROM_MS).then(|| (ms, Instant::now()));
    turn.began = None;
    turn.step = None;
}

/// "1.2 s", "14 s", "2 min 05 s".
pub fn spoken_duration(ms: u64) -> String {
    match ms {
        0..=9_949 => format!("{:.1} s", ms as f32 / 1000.0),
        9_950..=59_499 => format!("{} s", (ms + 500) / 1000),
        _ => format!("{} min {:02} s", ms / 60_000, (ms % 60_000) / 1000),
    }
}

/// The step line for `state` now: (text, still working).
fn step_now(state: &str) -> Option<(String, bool)> {
    let mut turn = lock(&TURN);
    if state == "thinking" {
        let began = *turn.began.get_or_insert_with(Instant::now);
        turn.took = None;
        return (began.elapsed() >= STEP_AFTER).then(|| (turn.step.unwrap_or("thinking it over").to_string(), true));
    }
    if state != "speaking" {
        turn.began = None;
        turn.step = None;
    }
    match turn.took {
        Some((ms, at)) if at.elapsed() < TOOK_STAYS => Some((format!("done in {}", spoken_duration(ms)), false)),
        Some(_) => {
            turn.took = None;
            None
        }
        None => None,
    }
}

// -------------------------------------------------------------------------------- fault ----

static FAULT: AtomicBool = AtomicBool::new(false);

/// An order failed: the emblem flashes once.
pub fn fault() {
    FAULT.store(true, Ordering::Release);
}

pub fn take_fault() -> bool {
    FAULT.swap(false, Ordering::AcqRel)
}

// -------------------------------------------------------------------------------- offer ----

/// How long "Heard: … – do it?" waits for an answer.
pub const OFFER_FOR: Duration = Duration::from_secs(7);

static OFFER: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// An order he knows was heard without his name: the HUD asks, he says nothing.
pub fn offer(order: &str) {
    let order = order.trim().trim_end_matches(['.', '!']).trim();
    if !order.is_empty() {
        *lock(&OFFER) = Some((order.to_string(), Instant::now()));
    }
}

/// The offer on screen: the order and how much of its time is left (1 … 0).
pub fn offered() -> Option<(String, f32)> {
    let mut offer = lock(&OFFER);
    match offer.as_ref() {
        Some((order, at)) if at.elapsed() < OFFER_FOR => Some((order.clone(), 1.0 - at.elapsed().as_secs_f32() / OFFER_FOR.as_secs_f32())),
        Some(_) => {
            *offer = None;
            None
        }
        None => None,
    }
}

/// Sir took the offer (its `[ YES ]`, or a spoken yes): the order to carry out.
pub fn accept() -> Option<String> {
    let (order, at) = lock(&OFFER).take()?;
    (at.elapsed() < OFFER_FOR).then_some(order)
}

pub fn dismiss_offer() {
    *lock(&OFFER) = None;
}

/// A plain yes to the offer on screen ("yes", "do it", "ja, mach das"): the order to carry out.
/// Anything else leaves the offer where it is; a plain no takes it away.
pub fn accept_by_voice(text: &str) -> Option<String> {
    static YES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:(?:yes|yeah|yep|yup|sure|ok|okay|ja|jo|jep)(?:\s+(?:please|bitte|do\s+it|do\s+that|go\s+ahead|mach\s+das|mach\s+es|mach))?|do\s+it|do\s+that|go\s+ahead|go\s+on|please\s+do|mach\s+das|mach\s+es|mach\s+schon)$").expect("valid yes regex"));
    static NO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:no|nope|nah|no\s+thanks|don'?t|leave\s+it|never\s+mind|nein|lass\s+(?:es|das))$").expect("valid no regex"));
    offered()?;
    let said: String = text.to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '\'').collect();
    let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
    if NO.is_match(&said) {
        dismiss_offer();
        return None;
    }
    if YES.is_match(&said) { accept() } else { None }
}

// ---------------------------------------------------------------------------- microphone ----

/// What is wrong with the way he hears Sir, from the loudest his voice was in this utterance
/// (`peak_db`, dBFS) and the room's own noise (`floor_db`): None when all is well.
pub fn mic_verdict(peak_db: f32, floor_db: f32) -> Option<&'static str> {
    if peak_db > -2.5 {
        Some("TOO LOUD")
    } else if floor_db > -34.0 {
        Some("NOISY ROOM")
    } else if peak_db < -30.0 && peak_db - floor_db < 12.0 {
        Some("TOO QUIET")
    } else {
        None
    }
}

// --------------------------------------------------------------------------------- paint ----

const INK: Color32 = Color32::from_rgb(0xE8, 0xF2, 0xFC);
const MUTED: Color32 = Color32::from_rgb(0x8E, 0xA4, 0xB8);
const ROW: f32 = 26.0;

/// The mark of the song's line: three small bars that move with what plays, the lows on the
/// left (they stand low while nothing does). Drawn: the fonts' note is a pictograph.
fn music_mark(p: &egui::Painter, left_middle: Pos2, colour: Color32) {
    let bands = crate::pc_audio::bands();
    let third = crate::pc_audio::BANDS / 3;
    let foot = left_middle.y + 6.0;
    for k in 0..3 {
        let level = bands[k * third..(k + 1) * third].iter().sum::<f32>() / third as f32;
        let x = left_middle.x + k as f32 * 4.5;
        p.rect_filled(Rect::from_min_max(pos2(x, foot - 3.0 - 9.0 * level.clamp(0.0, 1.0)), pos2(x + 2.5, foot)), Rounding::ZERO, colour);
    }
}

/// Where the lines stand under an emblem at `centre` with `radius` (the first row's middle):
/// clear of its outermost ring and of the music's ticks round it.
pub fn top_under(centre: Pos2, radius: f32) -> Pos2 {
    pos2(centre.x, centre.y + radius * 1.34 + 26.0)
}

use crate::hud_style::stroke;

/// Text centred on `at` on a soft dark band, so it reads over whatever is behind it. `apart`:
/// how far its letters stand apart.
fn line(p: &egui::Painter, at: Pos2, text: &str, font: FontId, colour: Color32, width: f32, apart: f32) -> Rect {
    let mut job = egui::text::LayoutJob::default();
    job.append(text, 0.0, egui::TextFormat { font_id: font, color: colour, extra_letter_spacing: apart, ..Default::default() });
    job.wrap.max_width = width;
    job.wrap.max_rows = 2;
    job.wrap.overflow_character = Some('…');
    job.halign = egui::Align::Center;
    let galley = p.layout_job(job);
    let rect = Rect::from_center_size(at, galley.size()).expand2(vec2(12.0, 4.0));
    crate::hud_style::caption_band(p, rect, colour.a() as f32 / 255.0);
    p.galley(pos2(at.x, at.y - galley.size().y * 0.5), galley, colour);
    rect
}

/// When he last heard his name (the emblem answers with a ripple instead of a sound).
static WOKE: Mutex<Option<Instant>> = Mutex::new(None);

/// He heard his name.
pub fn woke() {
    *lock(&WOKE) = Some(Instant::now());
}

/// How long ago he heard his name (seconds), while the ripple it starts is still running.
pub fn woke_age() -> Option<f32> {
    let mut woke = lock(&WOKE);
    let age = woke.as_ref()?.elapsed().as_secs_f32();
    if age >= crate::hud_style::RIPPLE_SECS {
        *woke = None;
        return None;
    }
    Some(age)
}

/// Paints the lines under the emblem, top row centred on `top`, within `width`. `state`: what
/// he is doing; `live_words`: Sir's words are shown while he speaks; `lyrics`: the song's
/// current line. Returns the order of an offer Sir just took by its `[ YES ]`.
pub fn paint(ui: &mut Ui, top: Pos2, width: f32, state: &str, live_words: bool, lyrics: Option<&str>) -> Option<String> {
    let ctx = ui.ctx().clone();
    let p = ui.painter().clone();
    let time = ctx.input(|i| i.time);
    let accent = crate::hud_board::active_colour();
    let mut y = top.y;
    let mut moving = false;
    // Row one: his words while he speaks; what JAI is working on while he thinks.
    let step = step_now(state);
    if let Some((text, working)) = &step {
        let dots = if *working { ".".repeat(1 + ((time * 2.5) as usize) % 3) } else { String::new() };
        line(&p, pos2(top.x, y), &format!("{}{dots}", text.to_uppercase()), FontId::new(13.5, egui::FontFamily::Name("label".into())), if *working { accent } else { MUTED }, width, 1.6);
        y += ROW;
        moving = true;
    } else if let Some((text, settled, strength)) = words_now().filter(|_| live_words) {
        let colour = if settled { INK } else { INK.gamma_multiply(0.82) };
        line(&p, pos2(top.x, y), &text, FontId::new(18.0, egui::FontFamily::Name("display".into())), colour.gamma_multiply(strength), width, 0.6);
        y += ROW + 2.0;
        moving = true;
    }
    // Row two: the song's line.
    if let Some(lyric) = lyrics.filter(|l| !l.trim().is_empty()) {
        let rect = line(&p, pos2(top.x, y), lyric, FontId::new(15.5, egui::FontFamily::Name("label".into())), accent.gamma_multiply(0.95), width, 0.5);
        music_mark(&p, pos2(rect.left() - 5.0, rect.center().y), accent);
        y += ROW;
    }
    // The offer: what was heard without his name, and its two keys.
    let mut taken = None;
    if let Some((order, left)) = offered() {
        let text = format!("HEARD:  \u{201C}{order}\u{201D}  \u{2014}  DO IT?");
        let font = FontId::monospace(11.0);
        let galley = p.layout_no_wrap(crate::hud_lines::fit(&p, &text, &font, width - 150.0), font.clone(), INK);
        let keys = crate::hud_style::key_width(&p, "[ YES ]") + crate::hud_style::key_width(&p, "[ NO ]") + 4.0;
        let chip = Rect::from_center_size(pos2(top.x, y + 6.0), vec2(galley.size().x + keys + 34.0, 26.0));
        p.rect_filled(chip, Rounding::ZERO, Color32::from_rgba_unmultiplied(4, 8, 13, 240));
        p.rect_stroke(chip.shrink(0.5), Rounding::ZERO, stroke(1.0, accent));
        crate::hud_board::brackets(&p, chip.shrink(0.75), crate::hud_board::bracket_colour());
        p.galley(pos2(chip.left() + 10.0, chip.center().y - galley.size().y * 0.5), galley, INK);
        // Its time runs out along the bottom edge.
        p.rect_filled(Rect::from_min_size(pos2(chip.left(), chip.bottom() - 2.0), vec2(chip.width() * left.clamp(0.0, 1.0), 2.0)), Rounding::ZERO, accent.gamma_multiply(0.8));
        let (clicked, _) = crate::hud_style::keys(ui, egui::Id::new("hud_offer"), chip.right() - 8.0, chip.center().y, &[("[ NO ]", false, "Leave it"), ("[ YES ]", true, "Carry it out")], 1.0);
        match clicked {
            Some(0) => dismiss_offer(),
            Some(1) => taken = accept(),
            _ => {}
        }
        y += 34.0;
        moving = true;
    }
    // "Did you mean: create a protocol called 'no think', Sir?" (`ask_first`): the question he
    // asked aloud before a guess changed anything, with its two keys. Yes is said for Sir.
    if let Some((question, left)) = crate::ask_first::pending() {
        let text = format!("{}?", question.trim_end_matches('?').trim_end_matches(", Sir").to_uppercase());
        let font = FontId::monospace(11.0);
        let galley = p.layout_no_wrap(fit(&p, &text, &font, width - 150.0), font.clone(), INK);
        let keys = crate::hud_style::key_width(&p, "[ YES ]") + crate::hud_style::key_width(&p, "[ NO ]") + 4.0;
        let chip = Rect::from_center_size(pos2(top.x, y + 6.0), vec2(galley.size().x + keys + 34.0, 26.0));
        p.rect_filled(chip, Rounding::ZERO, Color32::from_rgba_unmultiplied(4, 8, 13, 240));
        p.rect_stroke(chip.shrink(0.5), Rounding::ZERO, stroke(1.0, accent));
        crate::hud_board::brackets(&p, chip.shrink(0.75), crate::hud_board::bracket_colour());
        p.galley(pos2(chip.left() + 10.0, chip.center().y - galley.size().y * 0.5), galley, INK);
        p.rect_filled(Rect::from_min_size(pos2(chip.left(), chip.bottom() - 2.0), vec2(chip.width() * left.clamp(0.0, 1.0), 2.0)), Rounding::ZERO, accent.gamma_multiply(0.8));
        let (clicked, _) = crate::hud_style::keys(ui, egui::Id::new("hud_ask_first"), chip.right() - 8.0, chip.center().y, &[("[ NO ]", false, "Leave it as it is"), ("[ YES ]", true, "Carry it out")], 1.0);
        match clicked {
            Some(0) => crate::ask_first::drop_it(),
            Some(1) => taken = Some("yes".to_string()),
            _ => {}
        }
        y += 34.0;
        moving = true;
    }
    // "Did you mean" (`did_you_mean`): the orders he does know that are closest to one he did
    // not, each a key of its own (a click carries it out), and one that takes them all down.
    if let Some(offer) = crate::did_you_mean::current() {
        let head = p.layout_no_wrap("DID YOU MEAN".to_string(), FontId::monospace(11.0), MUTED);
        let labels: Vec<String> = offer.choices.iter().enumerate().map(|(k, choice)| format!("[ {}  {} ]", k + 1, fit(&p, &choice.label.to_uppercase(), &FontId::monospace(11.0), (width - 90.0).max(120.0)))).collect();
        let none = crate::hud_style::key_width(&p, "[ NONE ]");
        let widest = labels.iter().map(|label| crate::hud_style::key_width(&p, label)).fold(head.size().x + none + 24.0, f32::max);
        let rows = labels.len() as f32;
        let chip = Rect::from_min_size(pos2(top.x - widest * 0.5 - 14.0, y - 7.0), vec2(widest + 28.0, 30.0 + rows * 22.0));
        p.rect_filled(chip, Rounding::ZERO, Color32::from_rgba_unmultiplied(4, 8, 13, 240));
        p.rect_stroke(chip.shrink(0.5), Rounding::ZERO, stroke(1.0, accent.gamma_multiply(0.7)));
        crate::hud_board::brackets(&p, chip.shrink(0.75), crate::hud_board::bracket_colour());
        let first_row = chip.top() + 13.0;
        p.galley(pos2(chip.left() + 10.0, first_row - head.size().y * 0.5), head, MUTED);
        // Its time runs out along the bottom edge.
        p.rect_filled(Rect::from_min_size(pos2(chip.left(), chip.bottom() - 2.0), vec2(chip.width() * offer.left(), 2.0)), Rounding::ZERO, accent.gamma_multiply(0.8));
        if crate::hud_style::keys(ui, egui::Id::new("hud_meant_none"), chip.right() - 8.0, first_row, &[("[ NONE ]", false, "None of them")], 1.0).0.is_some() {
            crate::did_you_mean::dismiss();
        }
        for (k, label) in labels.iter().enumerate() {
            let right = chip.left() + 10.0 + crate::hud_style::key_width(&p, label);
            let clicked = crate::hud_style::keys(ui, egui::Id::new(("hud_meant", k)), right, first_row + 22.0 * (k as f32 + 1.0), &[(label.as_str(), true, "Carry this one out")], 1.0).0;
            if clicked.is_some() {
                taken = crate::did_you_mean::take(k);
            }
        }
        moving = true;
    }
    if moving {
        ctx.request_repaint_after(Duration::from_millis(50));
    }
    taken
}

/// `text` as it fits `width` in `font`: whole, or cut and ended with "…".
pub fn fit(p: &egui::Painter, text: &str, font: &FontId, width: f32) -> String {
    let fits = |s: &str| p.layout_no_wrap(s.to_string(), font.clone(), Color32::PLACEHOLDER).size().x <= width;
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut low, mut high) = (0usize, chars.len());
    while low < high {
        let mid = (low + high).div_ceil(2);
        let cut: String = chars[..mid].iter().collect::<String>() + "\u{2026}";
        if fits(&cut) { low = mid } else { high = mid - 1 }
    }
    chars[..low].iter().collect::<String>() + "\u{2026}"
}

/// The microphone's verdict as a label over the voice ring (amber), `strength` 0-1.
pub fn paint_mic_label(p: &egui::Painter, centre: Pos2, radius: f32, verdict: &str, strength: f32) {
    paint_label(p, centre, radius, verdict, crate::hud_style::warn_colour(), strength);
}

/// When the voice lock last turned an order away (it was not Sir's voice).
static TURNED_AWAY: Mutex<Option<Instant>> = Mutex::new(None);
/// How long the emblem says so (seconds).
const TURNED_AWAY_STAYS: f32 = 3.0;

/// The voice lock turned an order away: the emblem says why nothing happens.
pub fn turned_away() {
    *lock(&TURNED_AWAY) = Some(Instant::now());
}

/// How strongly the lock's word shows right now (it fades over its last moments), while it does.
pub fn turned_away_strength() -> Option<f32> {
    let mut at = lock(&TURNED_AWAY);
    let age = at.as_ref()?.elapsed().as_secs_f32();
    if age >= TURNED_AWAY_STAYS {
        *at = None;
        return None;
    }
    Some(((TURNED_AWAY_STAYS - age) / 0.4).clamp(0.0, 1.0))
}

/// The voice lock's word over the emblem.
pub fn paint_lock_label(p: &egui::Painter, centre: Pos2, radius: f32, strength: f32) {
    paint_label(p, centre, radius, "VOICE LOCK  ·  NOT YOUR VOICE", crate::hud_style::FAULT, strength);
}

fn paint_label(p: &egui::Painter, centre: Pos2, radius: f32, verdict: &str, colour: Color32, strength: f32) {
    let colour = colour.gamma_multiply(strength);
    let galley = p.layout_no_wrap(verdict.to_string(), FontId::monospace(11.0), colour);
    let rect = Rect::from_center_size(pos2(centre.x, centre.y - radius * 1.2 - 8.0), galley.size()).expand2(vec2(7.0, 3.0));
    // (On a dark plate, so it reads over the voice ring's ticks.)
    p.rect_filled(rect, Rounding::ZERO, Color32::from_black_alpha((150.0 * strength) as u8));
    p.galley(rect.min + vec2(7.0, 3.0), galley, colour);
    p.rect_stroke(rect, Rounding::ZERO, stroke(1.0, colour.gamma_multiply(0.7)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test at a time on the shared lines.
    static ONE: Mutex<()> = Mutex::new(());

    #[test]
    fn what_is_shown_of_a_transcript() {
        assert_eq!(clean(" open  the mixer. ", false).as_deref(), Some("open the mixer"));
        assert_eq!(clean("Open the mixer.", true).as_deref(), Some("Open the mixer."));
        assert_eq!(clean("[BLANK_AUDIO]", false), None);
        assert_eq!(clean("(music) *sighs* ...", false), None);
        assert_eq!(clean("Jay, [noise] what's open", false).as_deref(), Some("Jay, what's open"));
    }

    #[test]
    fn a_guess_is_replaced_and_a_settled_sentence_stays() {
        // (He is listening throughout: the privacy lock's own tests switch it meanwhile.)
        let _listening = crate::listening_lock::test_lock();
        crate::listening_lock::apply(crate::listening_lock::LockCommand::Resume);
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        *lock(&WORDS) = None;
        heard("Jay open");
        heard("Jay open the mix");
        assert_eq!(words_now().map(|(t, s, _)| (t, s)), Some(("Jay open the mix".to_string(), false)));
        settled("Jay, open the mixer.");
        heard("Jay open the");
        assert_eq!(words_now().map(|(t, s, _)| (t, s)), Some(("Jay, open the mixer.".to_string(), true)), "a late guess never replaces the sentence");
        *lock(&WORDS) = None;
        heard("something");
        speech_over();
        assert!(words_now().is_none(), "a blip leaves nothing");
    }

    /// Sir, 2026-11-13: after "don't listen" what the microphone heard (a song in the room)
    /// went on appearing under the emblem. Not while he is told not to listen, unless Sir asks
    /// to see it while the lock is on.
    #[test]
    fn nothing_he_hears_is_shown_while_he_is_told_not_to_listen() {
        use crate::listening_lock::{apply, show_asked, LockCommand};
        let _listening = crate::listening_lock::test_lock();
        apply(LockCommand::Resume);
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        *lock(&WORDS) = None;
        // His order stands as he said it, and fades in its own time; a guess that was up goes.
        settled("JAI, don't listen.");
        apply(LockCommand::Lock);
        assert_eq!(words_now().map(|(t, s, _)| (t, s)), Some(("JAI, don't listen.".to_string(), true)));
        *lock(&WORDS) = Some(Words { text: "a guess from before".into(), at: Instant::now(), settled: false });
        assert!(words_now().is_none(), "a guess goes with the lock");
        // What is heard under the lock never gets there.
        heard("The river keeps the count of all my wanderings");
        assert!(lock(&WORDS).is_none() && words_now().is_none());
        // "Subtitles on" / "show my words" while the lock is on: Sir's explicit word.
        show_asked();
        heard("you can see this");
        assert_eq!(words_now().map(|(t, _, _)| t).as_deref(), Some("you can see this"));
        // The lock ends: everything is as his settings say; the next lock hides again.
        apply(LockCommand::Resume);
        heard("and this");
        assert_eq!(words_now().map(|(t, _, _)| t).as_deref(), Some("and this"));
        apply(LockCommand::Lock);
        assert!(words_now().is_none());
        apply(LockCommand::Resume);
        *lock(&WORDS) = None;
    }

    #[test]
    fn steps_are_named_in_plain_words() {
        for (tool, want) in [
            ("get_calendar_schedule", "reading your calendar"),
            ("check_school_schedule", "reading your timetable"),
            ("web_search", "searching the web"),
            ("start_deep_research", "researching"),
            ("control_audio_mixer", "at the mixer"),
            ("show_3d_model", "building the model"),
            ("recall_memories", "looking through my notes"),
            ("configure_interface", "arranging the board"),
            ("something_new", "working on it"),
        ] {
            assert_eq!(step_phrase(tool), want, "{tool}");
        }
        assert_eq!(spoken_duration(1234), "1.2 s");
        assert_eq!(spoken_duration(14_400), "14 s");
        assert_eq!(spoken_duration(125_000), "2 min 05 s");
    }

    /// An instant command shows no step and no "done in"; a turn worth a line shows both.
    #[test]
    fn only_a_turn_worth_a_line_gets_one() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        *lock(&TURN) = Turn { began: None, step: None, took: None };
        assert!(step_now("thinking").is_none(), "nothing in the first moment");
        step("get_calendar_schedule");
        took(18, false);
        assert!(step_now("idle").is_none(), "an instant command leaves no line");
        lock(&TURN).began = Some(Instant::now() - Duration::from_millis(900));
        step("get_calendar_schedule");
        assert_eq!(step_now("thinking"), Some(("reading your calendar".to_string(), true)));
        took(1250, false);
        assert_eq!(step_now("idle"), Some(("done in 1.2 s".to_string(), false)));
        lock(&TURN).began = Some(Instant::now() - Duration::from_millis(900));
        took(5000, true);
        assert!(step_now("idle").is_none(), "an interrupted turn leaves no line");
    }

    #[test]
    fn an_offer_is_taken_by_a_plain_yes_only() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        dismiss_offer();
        assert_eq!(accept_by_voice("yes"), None, "nothing is on offer");
        offer("Open Spotify.");
        assert_eq!(offered().map(|(o, _)| o).as_deref(), Some("Open Spotify"));
        assert_eq!(accept_by_voice("yes I think the weather is nice"), None);
        assert!(offered().is_some(), "talk leaves the offer where it is");
        assert_eq!(accept_by_voice("Yes, do it.").as_deref(), Some("Open Spotify"));
        assert!(offered().is_none() && accept().is_none(), "taken once");
        offer("mute discord");
        assert_eq!(accept_by_voice("No."), None);
        assert!(offered().is_none(), "a plain no takes it away");
        offer("mute discord");
        assert_eq!(accept_by_voice("Ja, mach das").as_deref(), Some("mute discord"));
        // An offer nobody answers lapses.
        *lock(&OFFER) = Some(("old".into(), Instant::now() - OFFER_FOR - Duration::from_millis(10)));
        assert!(offered().is_none() && accept_by_voice("yes").is_none());
    }

    #[test]
    fn the_microphones_verdict() {
        assert_eq!(mic_verdict(-18.0, -55.0), None);
        assert_eq!(mic_verdict(-1.0, -55.0), Some("TOO LOUD"));
        assert_eq!(mic_verdict(-38.0, -48.0), Some("TOO QUIET"));
        assert_eq!(mic_verdict(-20.0, -30.0), Some("NOISY ROOM"));
        assert_eq!(mic_verdict(-36.0, -60.0), None, "quiet, but well over a silent room");
    }
}
