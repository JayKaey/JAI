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
    let t: String = text.to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    t.is_empty()
        || ["thank you", "thanks for watching", "thank you for watching", "thanks for watching and see you next time", "bye", "you", "music", "applause", "untertitel der amaraorg community", "untertitel im auftrag des zdf", "untertitelung des zdf 2020", "vielen dank", "bis zum nächsten mal", "subtitles by the amaraorg community"]
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

/// Keeps the subtitles going while they are on (asked every HUD frame): starts the listener,
/// and shows the bar while there is a line.
pub fn show(ctx: &egui::Context, app: &crate::app::Jai, rt: &tokio::runtime::Handle) {
    if crate::hud_feel::subtitles().is_none() || crate::platform::in_test_session() {
        return;
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
        return;
    }
    let Some([l, t, r, b]) = bar_monitor() else { return };
    let ppp = ctx.native_pixels_per_point().unwrap_or(1.0).max(0.5);
    let (width, height) = (((r - l) as f32 / ppp * 0.62).min(1500.0), 104.0);
    let at = pos2(((l + r) as f32 / 2.0) / ppp - width / 2.0, b as f32 / ppp - height - 70.0);
    let _ = t;
    let builder = ViewportBuilder::default()
        .with_title("J.A.I. // subtitles")
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_mouse_passthrough(true)
        .with_taskbar(false)
        .with_resizable(false)
        .with_active(false)
        .with_position(at)
        .with_inner_size(vec2(width, height));
    ctx.show_viewport_immediate(ViewportId::from_hash_of("jai_subtitles"), builder, |ctx, _class| {
        paint_bar(ctx, &shown);
        ctx.request_repaint_after(Duration::from_millis(100));
    });
    ctx.request_repaint_after(Duration::from_millis(200));
}

/// Paints the subtitles into the bar's own window: the newest line at the bottom, the one
/// before it above and fainter, each on a dark band so it reads over any picture.
pub fn paint_bar(ctx: &egui::Context, shown: &[(String, f32)]) {
    let rect = ctx.screen_rect();
    let p = ctx.layer_painter(egui::LayerId::background());
    let font = FontId::new(25.0, FontFamily::Name("display".into()));
    let mut y = rect.bottom() - 8.0;
    for (k, (text, strength)) in shown.iter().rev().enumerate() {
        let strength = strength * if k == 0 { 1.0 } else { 0.72 };
        let mut job = egui::text::LayoutJob::default();
        job.append(text, 0.0, egui::TextFormat { font_id: font.clone(), color: Color32::WHITE.gamma_multiply(strength), extra_letter_spacing: 0.4, ..Default::default() });
        job.wrap.max_width = rect.width() - 48.0;
        job.wrap.max_rows = 2;
        job.wrap.overflow_character = Some('…');
        job.halign = egui::Align::Center;
        let galley = p.layout_job(job);
        let size = galley.size();
        let back = egui::Rect::from_min_size(pos2(rect.center().x - size.x / 2.0 - 16.0, y - size.y - 6.0), vec2(size.x + 32.0, size.y + 8.0));
        if back.top() < rect.top() {
            break;
        }
        p.rect_filled(back, Rounding::same(3.0), Color32::from_black_alpha((215.0 * strength) as u8));
        p.galley(pos2(rect.center().x, y - size.y - 2.0), galley, Color32::WHITE);
        y = back.top() - 4.0;
    }
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
    let rect = crate::win32::foreground_monitor().or_else(|| crate::win32::displays().first().map(|d| d.rect));
    *seen = Some((Instant::now(), rect));
    rect
}

#[cfg(not(windows))]
fn bar_monitor() -> Option<[i32; 4]> {
    None
}

async fn listen(app: crate::app::Jai) {
    tracing::info!("[SUBTITLES] On");
    let mut heard: Vec<f32> = Vec::new();
    while let Some(translate) = crate::hud_feel::subtitles() {
        // Not beside a game, and not his own voice.
        if crate::hud_feel::game_in_front() || app.hud_state().state == "speaking" {
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
            if !phantom(&t.text) && crate::hud_feel::subtitles().is_some() {
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
