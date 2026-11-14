//! Live subtitles (Sir, 2026-11-12): what is said in a video, a stream or a call on the PC,
//! written along in a bar at the bottom of the screen it plays on ("subtitles on"), in the
//! language spoken or translated into English ("translate the subtitles"). The speech comes
//! from `pc_audio`'s tap, is cut into clips at the pauses, and goes through the same Whisper
//! that hears Sir: a clip is called off the moment Sir speaks, so his words never wait behind
//! a video's. Paused while a game is in front and while JAI himself is speaking.

use eframe::egui::{self, pos2, vec2, Color32, FontFamily, FontId, Rounding, ViewportBuilder, ViewportId};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

const RATE: usize = crate::pc_audio::SPEECH_RATE;
/// A clip is cut at a pause once it is this long, and anyway when it is that long.
const CLIP_MIN: usize = RATE * 5 / 2;
const CLIP_MAX: usize = RATE * 7;
/// The pause a clip is cut at: this long below the quiet level.
const PAUSE: usize = RATE * 3 / 10;
const QUIET: f32 = 0.006;
/// A clip quieter than this overall holds no speech worth a decode.
const AUDIBLE: f32 = 0.004;
/// A line stays this long.
const LINE_STAYS: Duration = Duration::from_secs(6);

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() { 0.0 } else { (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt() }
}

/// Where to cut the audio heard so far into a clip (the number of samples the clip takes), if
/// it is time: at the end once there are `CLIP_MIN` of them and the last `PAUSE` are quiet, or
/// at the quietest spot of the last two seconds once there are `CLIP_MAX`.
pub fn cut(heard: &[f32]) -> Option<usize> {
    if heard.len() >= CLIP_MIN && rms(&heard[heard.len() - PAUSE..]) < QUIET {
        return Some(heard.len());
    }
    if heard.len() < CLIP_MAX {
        return None;
    }
    let from = heard.len() - 2 * RATE;
    let step = RATE / 10;
    let quietest = (from..heard.len() - step).step_by(step).min_by(|a, b| rms(&heard[*a..*a + step]).total_cmp(&rms(&heard[*b..*b + step])))?;
    Some(quietest + step / 2)
}

/// What Whisper writes for music, noise and silence when asked for words: never a subtitle.
pub fn phantom(text: &str) -> bool {
    // (A mark between two words parts them: "Amara.org-Community" is "amara org community".)
    let t: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    t.is_empty()
        || ["thank you", "thanks for watching", "thank you for watching", "thanks for watching and see you next time", "bye", "you", "music", "applause", "untertitel der amara org community", "untertitel im auftrag des zdf", "untertitelung des zdf 2020", "vielen dank", "bis zum nächsten mal", "subtitles by the amara org community"]
            .contains(&t.as_str())
}

struct Line {
    text: String,
    at: Instant,
}

static LINES: Mutex<VecDeque<Line>> = Mutex::new(VecDeque::new());
static RUNNING: AtomicBool = AtomicBool::new(false);
/// The clip being decoded right now (its call-off flag): Sir's speech stops it.
static DECODING: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// Sir began to speak: a clip of the video that Whisper is busy with is dropped at once.
pub fn sir_speaks() {
    if let Some(flag) = lock(&DECODING).as_ref() {
        flag.store(true, Ordering::Release);
    }
}

fn push(text: &str) {
    let mut lines = lock(&LINES);
    lines.push_back(Line { text: text.trim().to_string(), at: Instant::now() });
    while lines.len() > 2 {
        lines.pop_front();
    }
}

/// The lines on show: (text, strength 0-1), the oldest first.
pub fn lines() -> Vec<(String, f32)> {
    let mut lines = lock(&LINES);
    lines.retain(|l| l.at.elapsed() < LINE_STAYS);
    lines.iter().map(|l| (l.text.clone(), 1.0 - ((l.at.elapsed().as_secs_f32() - (LINE_STAYS.as_secs_f32() - 0.5)) / 0.5).clamp(0.0, 1.0))).collect()
}

/// Whether speech heard at `source` may be written along right now. What one app plays (a
/// video, a call, on a channel of its own) always; the mix of everything the PC plays carries
/// the microphone wherever a mixer adds its monitor to it (on Sir's PC it does), so while Sir
/// has told him not to listen that mix is neither heard nor written, unless Sir asks for
/// subtitles while the lock is on (`listening_lock::may_show_heard`).
fn may_caption(source: &crate::pc_audio::Source) -> bool {
    crate::listening_lock::may_show_heard() || matches!(source, crate::pc_audio::Source::Device(_))
}

/// What the bar shows right now and on which monitor (it starts the listener while the
/// subtitles are on); None while there is nothing to show.
fn bar_now(app: &crate::app::Jai, rt: &tokio::runtime::Handle) -> Option<(Vec<(String, f32)>, [i32; 4])> {
    crate::hud_feel::subtitles()?;
    // "Don't listen": lines that came from a mix with the microphone in it go at once.
    if !may_caption(&crate::pc_audio::source(crate::pc_audio::Use::Speech)) {
        lock(&LINES).clear();
        return None;
    }
    if crate::platform::in_test_session() {
        // A scripted session neither listens to the PC nor writes over the screen Sir works on:
        // asked to (`JAI_DEMO=subtitles`), it shows two lines of its own on the monitor it names
        // (`JAI_BACKDROP_MONITOR=2`).
        let monitor = demo_monitor()?;
        return Some((vec![("So the first thing we need is a plan for the weekend.".to_string(), 0.7), ("And then we look at the weather, because nobody wants to stand in the rain.".to_string(), 1.0)], monitor));
    }
    if !RUNNING.swap(true, Ordering::AcqRel) {
        let app = app.clone();
        rt.spawn(async move {
            listen(app).await;
            RUNNING.store(false, Ordering::Release);
        });
    }
    let shown = lines();
    if shown.is_empty() {
        return None;
    }
    Some((shown, bar_monitor()?))
}

/// Keeps the subtitles going while they are on (asked every HUD frame) and shows the bar while
/// there is a line.
pub fn show(ctx: &egui::Context, app: &crate::app::Jai, rt: &tokio::runtime::Handle) {
    let Some((shown, monitor)) = bar_now(app, rt) else {
        BAR_OPEN.store(false, Ordering::Release);
        return;
    };
    let [l, t, r, b] = monitor;
    let ppp = ctx.native_pixels_per_point().unwrap_or(1.0).max(0.5);
    // The bar hugs its lines: its window is a plain dark box (a see-through window is not to be
    // had everywhere), so over a picture it covers as little as it can.
    let widest = ((r - l) as f32 / ppp * 0.62).min(1500.0);
    let wrap = widest - 48.0;
    let size = bar_size(ctx, &shown, wrap);
    let (width, height) = (size.x, size.y);
    let at = pos2(((l + r) as f32 / 2.0) / ppp - width / 2.0, bar_top(place().0, t as f32 / ppp, b as f32 / ppp, height));
    let builder = ViewportBuilder::default()
        .with_title(TITLE)
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_mouse_passthrough(true)
        .with_taskbar(false)
        .with_resizable(false)
        .with_active(false)
        .with_position(at)
        .with_inner_size(vec2(width, height));
    // (Said once per showing, so the log tells where the bar stands and that its window drew.)
    if !BAR_OPEN.swap(true, Ordering::AcqRel) {
        BAR_DRAWN.store(false, Ordering::Release);
        *lock(&BAR_SINCE) = Some(Instant::now());
        tracing::info!("[SUBTITLES] The bar opens at {:.0},{:.0} ({:.0}x{:.0}) on the monitor at {l},{t}", at.x * ppp, at.y * ppp, width * ppp, height * ppp);
    }
    ctx.show_viewport_immediate(ViewportId::from_hash_of("jai_subtitles"), builder, |ctx, class| {
        let embedded = class == egui::ViewportClass::Embedded;
        if !BAR_DRAWN.swap(true, Ordering::AcqRel) {
            tracing::info!("[SUBTITLES] The bar draws ({:.0}x{:.0}, {})", ctx.screen_rect().width(), ctx.screen_rect().height(), if embedded { "inside the HUD: it got no window of its own" } else { "in its own window" });
        }
        if embedded {
            paint_embedded(ctx, &shown, size, wrap);
        } else {
            paint_bar(ctx, &shown, Some(wrap));
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    });
    // Once its window stands: over everything, clicks pass through it, and it never takes the
    // focus. (Set by hand as well: what a window is asked to be at its creation is not always
    // what it gets.)
    let stands = lock(&BAR_SINCE).is_some_and(|at| at.elapsed() > Duration::from_millis(400));
    if stands && lock(&BAR_SINCE).take().is_some() {
        dress();
    }
    ctx.request_repaint_after(Duration::from_millis(200));
}

// ----------------------------------------------------------------- where the bar stands ----

/// Where the bar stands on its screen: at the bottom (as films have it), at the top, or across
/// the middle.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Place {
    #[default]
    Bottom,
    Top,
    Middle,
}

impl Place {
    fn id(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::Top => "top",
            Self::Middle => "middle",
        }
    }

    fn of(word: &str) -> Option<Self> {
        match word {
            "bottom" | "unten" => Some(Self::Bottom),
            "top" | "oben" => Some(Self::Top),
            "middle" | "centre" | "center" | "mitte" => Some(Self::Middle),
            _ => None,
        }
    }
}

/// (Where on the screen, on which monitor: None follows the window in front.) Read from the
/// kit's store once; the place before the last change, for "undo that".
static PLACE: Mutex<Option<(Place, Option<usize>)>> = Mutex::new(None);
static PLACE_BEFORE: Mutex<Option<(Place, Option<usize>)>> = Mutex::new(None);

pub fn place() -> (Place, Option<usize>) {
    *lock(&PLACE).get_or_insert_with(|| {
        let kept = crate::kit::get("subtitles", "place");
        let place = kept.as_ref().and_then(|v| v["place"].as_str()).and_then(Place::of).unwrap_or_default();
        let monitor = kept.as_ref().and_then(|v| v["monitor"].as_u64()).map(|n| n as usize).filter(|n| *n >= 1);
        (place, monitor)
    })
}

fn set_place(to: (Place, Option<usize>)) {
    *lock(&PLACE_BEFORE) = Some(place());
    *lock(&PLACE) = Some(to);
    let _ = crate::kit::put("subtitles", "place", &serde_json::json!({ "place": to.0.id(), "monitor": to.1 }));
}

/// The bar's top edge on a screen that reaches from `top` to `bottom` (points), for a bar
/// `height` high.
fn bar_top(place: Place, top: f32, bottom: f32, height: f32) -> f32 {
    match place {
        Place::Bottom => bottom - height - BAR_LIFT,
        Place::Top => top + BAR_LIFT,
        Place::Middle => (top + bottom - height) / 2.0,
    }
}

/// "Subtitles to the top", "put the subtitles on monitor 2", "subtitles follow my window",
/// "where are the subtitles": the kit's phrases for the bar's place.
pub fn place_command(s: &str) -> Option<serde_json::Value> {
    use regex::Regex;
    use std::sync::LazyLock;
    const BAR: &str = r"(?:the\s+|die\s+)?(?:subtitles?|captions?|subtitle\s+bar|untertitel)";
    static WHERE: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^(?:(?:put|move|show|place|set|have|schieb|schiebe|setz|setze|zeig)\s+)?{BAR}\s+(?:(?:to|at|on|in|nach|in\s+die|in\s+der)\s+)?(?:the\s+)?(?P<place>top|bottom|middle|centre|center|oben|unten|mitte)(?:\s+of\s+(?:the|my)\s+screen)?$")).expect("valid regex"));
    static MONITOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^(?:(?:put|move|show|place|have|schieb|schiebe|zeig)\s+)?{BAR}\s+(?:on|to|onto|auf)\s+(?:my\s+|the\s+|den\s+|dem\s+)?(?:monitor|screen|display|bildschirm)\s+(?P<n>\d)$")).expect("valid regex"));
    static FOLLOW: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^{BAR}\s+(?:follow|should\s+follow)\s+(?:my\s+window|me|the\s+window\s+in\s+front)$|^(?:(?:put|move|show)\s+)?{BAR}\s+(?:on|to)\s+(?:the\s+)?(?:(?:monitor|screen)\s+(?:in\s+front|i\s+am\s+(?:on|using))|(?:monitor|screen)\s+of\s+the\s+window\s+in\s+front)$|^untertitel\s+folgen\s+(?:meinem\s+fenster|mir)$")).expect("valid regex"));
    static ASKED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^where\s+(?:are|do)\s+the\s+subtitles(?:\s+(?:stand|show|appear))?$|^wo\s+(?:sind|stehen)\s+die\s+untertitel$").expect("valid regex"));
    let s = s.trim();
    if let Some(c) = WHERE.captures(s) {
        return Some(serde_json::json!({ "action": "set", "place": Place::of(&c["place"])?.id() }));
    }
    if let Some(c) = MONITOR.captures(s) {
        return Some(serde_json::json!({ "action": "set", "monitor": c["n"].parse::<u64>().ok()? }));
    }
    if FOLLOW.is_match(s) {
        return Some(serde_json::json!({ "action": "set", "monitor": 0 }));
    }
    ASKED.is_match(s).then(|| serde_json::json!({ "action": "status" }))
}

fn place_in_words((place, monitor): (Place, Option<usize>)) -> String {
    let at = match place {
        Place::Bottom => "at the bottom",
        Place::Top => "at the top",
        Place::Middle => "across the middle",
    };
    match monitor {
        Some(n) => format!("{at} of monitor {n}"),
        None => format!("{at} of the screen your window is on"),
    }
}

/// Carries out `place_command`'s arguments.
pub fn place_tool(args: &serde_json::Value) -> Result<String, String> {
    let now = place();
    match args["action"].as_str().unwrap_or("status") {
        "set" => {
            let to_place = args["place"].as_str().and_then(Place::of).unwrap_or(now.0);
            let to_monitor = match args["monitor"].as_u64() {
                Some(0) => None,
                Some(n) => {
                    let monitors = crate::monitors::list().len();
                    if n as usize > monitors {
                        return Err(format!("You have {monitors} monitor{}, Sir.", if monitors == 1 { "" } else { "s" }));
                    }
                    Some(n as usize)
                }
                None => now.1,
            };
            set_place((to_place, to_monitor));
            let words = place_in_words((to_place, to_monitor));
            Ok(crate::kit::say(format!("The subtitles stand {words}."), format!("Subtitles {words}, Sir.")))
        }
        "back" => {
            let before = lock(&PLACE_BEFORE).take().ok_or("The subtitles stand where they stood, Sir.")?;
            set_place(before);
            Ok(crate::kit::say(format!("The subtitles stand {} again.", place_in_words(before)), "Back where they were, Sir."))
        }
        _ => {
            let words = place_in_words(now);
            Ok(crate::kit::say(format!("The subtitles stand {words}. Say \"subtitles to the top\", \"subtitles on monitor 2\" or \"subtitles follow my window\"."), format!("They stand {words}, Sir.")))
        }
    }
}

/// The bar's window: its title (it is found by it), how far it stands above the screen's bottom
/// edge (points), and how much of what is under it shows through (of 255: a little).
const TITLE: &str = "J.A.I. // subtitles";
const BAR_LIFT: f32 = 70.0;
const BAR_ALPHA: u8 = 228;

/// The bar is on show (its window was asked for in the last frame), it has drawn, and when it
/// opened (until its window was dressed).
static BAR_OPEN: AtomicBool = AtomicBool::new(false);
static BAR_DRAWN: AtomicBool = AtomicBool::new(false);
static BAR_SINCE: Mutex<Option<Instant>> = Mutex::new(None);

#[cfg(windows)]
fn dress() {
    crate::win32::run_detached(|| match crate::win32::own_window(TITLE) {
        Some(hwnd) => match crate::win32::make_overlay(hwnd, BAR_ALPHA) {
            Ok(()) => tracing::info!("[SUBTITLES] The bar's window stands over everything and lets clicks through"),
            Err(why) => tracing::warn!("[SUBTITLES] The bar's window could not be made an overlay: {why}"),
        },
        None => tracing::warn!("[SUBTITLES] The bar has no window of its own; my windows: {}", crate::win32::own_windows().join("; ")),
    });
}

#[cfg(not(windows))]
fn dress() {}

/// Paints the subtitles into the bar's own window: the newest line at the bottom, the one
/// before it above and fainter, each on a dark band so it reads over any picture.
pub fn paint_bar(ctx: &egui::Context, shown: &[(String, f32)], wrap: Option<f32>) {
    let rect = ctx.screen_rect();
    paint_lines(&ctx.layer_painter(egui::LayerId::background()), rect, shown, wrap.unwrap_or(rect.width() - 48.0));
}

/// The same inside the HUD's own window, for when the bar cannot have a window of its own: at
/// the bottom of the screen the HUD is on, over everything it shows.
fn paint_embedded(ctx: &egui::Context, shown: &[(String, f32)], size: egui::Vec2, wrap: f32) {
    let home = ctx.screen_rect();
    let rect = egui::Rect::from_min_size(pos2(home.center().x - size.x / 2.0, home.bottom() - size.y - BAR_LIFT), size);
    paint_lines(&ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hud_subtitles"))), rect, shown, wrap);
}

/// One line of the bar: `wrap` wide at most, two rows at most.
fn line_job(text: &str, strength: f32, wrap: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(text, 0.0, egui::TextFormat { font_id: FontId::new(25.0, FontFamily::Name("display".into())), color: Color32::WHITE.gamma_multiply(strength), extra_letter_spacing: 0.4, ..Default::default() });
    job.wrap.max_width = wrap;
    job.wrap.max_rows = 2;
    job.wrap.overflow_character = Some('…');
    job.halign = egui::Align::Center;
    job
}

/// How strongly line `k` (0: the newest) stands.
fn line_strength(k: usize, strength: f32) -> f32 {
    strength * if k == 0 { 1.0 } else { 0.72 }
}

/// How large the bar is for these lines (it hugs them), each `wrap` wide at most.
fn bar_size(ctx: &egui::Context, shown: &[(String, f32)], wrap: f32) -> egui::Vec2 {
    // (As `paint_lines` lays them: 6 under the newest line's band and over the oldest's, each
    // band 8 taller than its text, 4 between two.)
    let mut size = vec2(0.0f32, 12.0);
    for (k, (text, strength)) in shown.iter().rev().enumerate() {
        let line = ctx.fonts(|f| f.layout_job(line_job(text, line_strength(k, *strength), wrap))).size();
        size.x = size.x.max(line.x + 32.0);
        size.y += line.y + 8.0 + if k > 0 { 4.0 } else { 0.0 };
    }
    vec2((size.x + 16.0).clamp(160.0, wrap + 48.0), size.y.max(44.0))
}

fn paint_lines(p: &egui::Painter, rect: egui::Rect, shown: &[(String, f32)], wrap: f32) {
    let mut y = rect.bottom() - 8.0;
    for (k, (text, strength)) in shown.iter().rev().enumerate() {
        let strength = line_strength(k, *strength);
        let galley = p.layout_job(line_job(text, strength, wrap));
        let size = galley.size();
        let back = egui::Rect::from_min_size(pos2(rect.center().x - size.x / 2.0 - 16.0, y - size.y - 6.0), vec2(size.x + 32.0, size.y + 8.0));
        if back.top() < rect.top() - 0.5 {
            break;
        }
        p.rect_filled(back, Rounding::same(3.0), Color32::from_black_alpha((215.0 * strength) as u8));
        p.galley(pos2(rect.center().x, y - size.y - 2.0), galley, Color32::WHITE);
        y = back.top() - 4.0;
    }
}

/// The monitor a scripted session shows the bar on, when it was asked to show it.
#[cfg(windows)]
fn demo_monitor() -> Option<[i32; 4]> {
    if !crate::platform::demo("subtitles") {
        return None;
    }
    let n = std::env::var("JAI_BACKDROP_MONITOR").ok()?.trim().parse::<usize>().ok()?;
    crate::win32::displays().get(n.checked_sub(1)?).map(|d| d.rect)
}

#[cfg(not(windows))]
fn demo_monitor() -> Option<[i32; 4]> {
    None
}

/// The monitor the bar stands on: the one the window in front is on.
#[cfg(windows)]
fn bar_monitor() -> Option<[i32; 4]> {
    static SEEN: Mutex<Option<(Instant, Option<[i32; 4]>)>> = Mutex::new(None);
    let mut seen = lock(&SEEN);
    if let Some((at, rect)) = *seen {
        if at.elapsed() < Duration::from_secs(1) {
            return rect;
        }
    }
    // The monitor Sir chose for the bar, else the one of the window in front.
    let chosen = place().1.and_then(|n| crate::monitors::list().into_iter().find(|m| m.index == n).map(|m| m.rect));
    let rect = chosen.or_else(crate::win32::foreground_monitor).or_else(|| crate::win32::displays().first().map(|d| d.rect));
    *seen = Some((Instant::now(), rect));
    rect
}

#[cfg(not(windows))]
fn bar_monitor() -> Option<[i32; 4]> {
    None
}

/// Where the speech to write along is played: the output device of the app in front, else of
/// a player, browser or call that is making sound, else everything the PC plays. (On a PC
/// with a mixer in front of the speakers each app has a channel of its own, with nothing else
/// in it; the mix of everything carries Sir's own microphone on his.)
async fn speech_source() -> crate::pc_audio::Source {
    use crate::pc_audio::Source;
    let stem = |process: &str| process.trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
    #[cfg(windows)]
    let front = crate::win32::foreground_info().map(|(process, _)| stem(&process));
    #[cfg(not(windows))]
    let front: Option<String> = None;
    if let Some(app) = front {
        if let Some(device) = crate::audio_mixer::app_output_device(&app).await {
            return Source::Device(device);
        }
    }
    for (app, _) in crate::audio_mixer::audible_apps().await {
        if crate::hud_feel::is_player(&app) || crate::ears::is_game_or_voice_chat(&stem(&app).to_lowercase()) && !crate::hud_feel::is_game(&app) {
            if let Some(device) = crate::audio_mixer::app_output_device(&stem(&app)).await {
                return Source::Device(device);
            }
        }
    }
    Source::Default
}

async fn listen(app: crate::app::Jai) {
    tracing::info!("[SUBTITLES] On");
    let mut heard: Vec<f32> = Vec::new();
    let mut said_first = false;
    let mut looked: Option<Instant> = None;
    while let Some(translate) = crate::hud_feel::subtitles() {
        // Not beside a game, and not his own voice.
        if crate::hud_feel::game_in_front() || app.hud_state().state == "speaking" {
            crate::pc_audio::want(crate::pc_audio::Use::Speech, false);
            heard.clear();
            let _ = crate::pc_audio::take_speech();
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }
        // Where the speech is: looked up once a second.
        if looked.is_none_or(|at: Instant| at.elapsed() > Duration::from_secs(1)) {
            looked = Some(Instant::now());
            crate::pc_audio::listen_to(crate::pc_audio::Use::Speech, speech_source().await);
        }
        // "Don't listen": the mix that carries his microphone is not even heard meanwhile.
        if !may_caption(&crate::pc_audio::source(crate::pc_audio::Use::Speech)) {
            crate::pc_audio::want(crate::pc_audio::Use::Speech, false);
            heard.clear();
            let _ = crate::pc_audio::take_speech();
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }
        crate::pc_audio::want(crate::pc_audio::Use::Speech, true);
        tokio::time::sleep(Duration::from_millis(200)).await;
        heard.extend(crate::pc_audio::take_speech());
        let Some(n) = cut(&heard) else { continue };
        let clip: Vec<f32> = heard.drain(..n).collect();
        if rms(&clip) < AUDIBLE {
            continue;
        }
        let Some((called_off, reply)) = app.subtitle_ahead(clip, translate) else {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        *lock(&DECODING) = Some(Arc::clone(&called_off));
        let result = reply.await;
        *lock(&DECODING) = None;
        if let Ok(Ok(t)) = result {
            // (Still wanted, and still his to see: the lock may have come while Whisper heard it.)
            if !phantom(&t.text) && crate::hud_feel::subtitles().is_some() && may_caption(&crate::pc_audio::source(crate::pc_audio::Use::Speech)) {
                // (Once per switching on, so the log says the path works; never what was said.)
                if !said_first {
                    said_first = true;
                    tracing::info!("[SUBTITLES] The first line stands ({} characters, heard in {} ms)", t.text.chars().count(), t.stt_ms);
                }
                push(&t.text);
            }
        }
    }
    crate::pc_audio::want(crate::pc_audio::Use::Speech, false);
    lock(&LINES).clear();
    tracing::info!("[SUBTITLES] Off");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_is_placed_by_plain_words() {
        use serde_json::json;
        for (said, want) in [
            ("subtitles to the top", json!({"action": "set", "place": "top"})),
            ("put the subtitles at the bottom", json!({"action": "set", "place": "bottom"})),
            ("move the captions to the middle of the screen", json!({"action": "set", "place": "middle"})),
            ("untertitel nach oben", json!({"action": "set", "place": "top"})),
            ("untertitel in die mitte", json!({"action": "set", "place": "middle"})),
            ("put the subtitles on monitor 2", json!({"action": "set", "monitor": 2})),
            ("untertitel auf bildschirm 1", json!({"action": "set", "monitor": 1})),
            ("subtitles follow my window", json!({"action": "set", "monitor": 0})),
            ("where are the subtitles", json!({"action": "status"})),
        ] {
            assert_eq!(place_command(said), Some(want), "{said}");
        }
        // Switching them on and off, and translating, are the older orders.
        for said in ["subtitles on", "subtitles off", "untertitel an", "subtitles", "subtitles in english", "caption this", "move the window to the top"] {
            assert_eq!(place_command(said), None, "{said}");
        }
        // On a screen from 0 to 1080: lifted off the bottom edge, the same off the top, or centred.
        assert_eq!(bar_top(Place::Bottom, 0.0, 1080.0, 60.0), 1080.0 - 60.0 - BAR_LIFT);
        assert_eq!(bar_top(Place::Top, 0.0, 1080.0, 60.0), BAR_LIFT);
        assert_eq!(bar_top(Place::Middle, 0.0, 1080.0, 60.0), 510.0);
        // A monitor above the main one has a negative top.
        assert_eq!(bar_top(Place::Top, -1080.0, 0.0, 60.0), -1080.0 + BAR_LIFT);
    }

    fn speech(secs: f32) -> Vec<f32> {
        (0..(RATE as f32 * secs) as usize).map(|i| 0.1 * (i as f32 * 0.3).sin()).collect()
    }

    #[test]
    fn a_clip_is_cut_at_a_pause_or_when_it_is_long_enough() {
        // Two seconds of talk: too short for a clip, pause or not.
        let mut short = speech(2.0);
        short.extend(vec![0.0; PAUSE]);
        assert_eq!(cut(&short), None);
        // Three seconds of talk and a pause: the whole of it is the clip.
        let mut sentence = speech(3.0);
        sentence.extend(vec![0.0; PAUSE]);
        assert_eq!(cut(&sentence), Some(sentence.len()));
        // Talk without a pause: nothing until seven seconds, then a cut at the quietest spot of
        // the last two.
        assert_eq!(cut(&speech(6.5)), None);
        let mut long = speech(7.2);
        let dip = long.len() - RATE;
        long[dip..dip + RATE / 10].iter_mut().for_each(|s| *s = 0.0);
        let at = cut(&long).expect("a cut");
        assert!(at.abs_diff(dip + RATE / 20) <= RATE / 10, "cut at {at}, the dip is at {dip}");
    }

    #[test]
    fn what_whisper_makes_of_music_and_silence_is_no_subtitle() {
        for text in ["", "Thank you.", " Thanks for watching!", "[Music]", "(applause)", "Untertitel der Amara.org-Community", "you"] {
            assert!(phantom(text), "{text}");
        }
        for text in ["Thank you for the flowers, they are lovely.", "And that is how the engine works.", "Danke, das reicht."] {
            assert!(!phantom(text), "{text}");
        }
    }

    /// While Sir has told him not to listen, the mix that carries the microphone is not written
    /// along; what one app plays on its own channel is (a video's sound is no listening to the
    /// room). Asked for under the lock, both are.
    #[test]
    fn under_the_privacy_lock_only_an_apps_own_sound_is_written_along() {
        use crate::listening_lock::{apply, show_asked, LockCommand};
        use crate::pc_audio::Source;
        let _listening = crate::listening_lock::test_lock();
        apply(LockCommand::Resume);
        let (mix, video) = (Source::Default, Source::Device("Browser".into()));
        assert!(may_caption(&mix) && may_caption(&video));
        apply(LockCommand::Lock);
        assert!(!may_caption(&mix), "the mix carries his microphone");
        assert!(may_caption(&video), "an app's own channel does not");
        show_asked();
        assert!(may_caption(&mix), "\"subtitles on\" while the lock is on");
        apply(LockCommand::Resume);
        assert!(may_caption(&mix) && may_caption(&video));
    }

    #[test]
    fn two_lines_at_most_and_each_goes_after_its_time() {
        lock(&LINES).clear();
        push("one");
        push("two");
        push("three");
        assert_eq!(lines().iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(), ["two", "three"]);
        lock(&LINES).front_mut().unwrap().at = Instant::now() - LINE_STAYS - Duration::from_millis(10);
        assert_eq!(lines().len(), 1);
        lock(&LINES).clear();
    }
}
