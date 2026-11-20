//! Looking at the screen as in the reference videos. While JAI captures it ("explain what's
//! on my screen") his own windows are left out of the picture, so he reads what is behind them
//! (they stay on the monitor: Windows 10 2004+ `WDA_EXCLUDEFROMCAPTURE`); the HUD frosts over for
//! a moment as he looks, and a card under the emblem names what he saw ("VIDEO DETECTED") with
//! quick actions (SUMMARIZE · EXPLAIN · TRANSLATE) for a few seconds.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a card stays (the reference: about 2.6 s); the pointer on it keeps it.
pub const CARD_TIME: Duration = Duration::from_millis(2600);
/// How long the frost takes to clear after a capture.
pub const FROST_TIME: Duration = Duration::from_millis(450);

/// What the screen showed.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// "VIDEO", "ERROR", "CODE", "ARTICLE" or "PAGE" (shown as "… DETECTED").
    pub kind: &'static str,
    /// One line: the window's title or the first line he read.
    pub line: String,
    /// The quick actions: the word on the card and what JAI is asked when it is clicked.
    pub actions: [(&'static str, &'static str); 3],
}

#[derive(Default)]
struct State {
    card: Option<(Card, Instant)>,
    /// Captures running now (the HUD is frosted while any is) …
    looks: usize,
    /// … or the last one ended then (the frost clears).
    looked: Option<Instant>,
    /// His windows left out of captures and not yet let back in.
    excluded: Vec<isize>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(State::default))
}

/// What a capture of `label` (`the window "YouTube — Firefox"`, `Monitor 2 (primary)`) that
/// read `text` shows.
pub fn classify(label: &str, text: &str) -> Card {
    let title = label.split('"').nth(1).unwrap_or("").trim().to_string();
    let lower = format!("{} \n{}", title.to_lowercase(), text.to_lowercase());
    let words = text.split_whitespace().count();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    let code_lines = text.lines().filter(|l| {
        let l = l.trim();
        l.ends_with(';') || l.ends_with('{') || l == "}" || l.starts_with("fn ") || l.starts_with("def ") || l.starts_with("import ") || l.starts_with("use ") || l.contains("=>") || l.starts_with("#include") || l.starts_with("function ")
    });
    // An error shows as one: a line that starts the way a compiler's, a runtime's or a dialog's
    // does, or a short window title that names it; never the word inside code or prose
    // ("Result<(), Error>", "the first attempt failed").
    let title_words: Vec<String> = title.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    let error_title = title_words.len() <= 4 && title_words.iter().any(|w| matches!(w.as_str(), "error" | "fehler" | "exception" | "crash"));
    let kind = if has(&["youtube", "netflix", "twitch", "vimeo", "prime video", "disney+", "vlc media player", "mpv", "watch?v="]) {
        "VIDEO"
    } else if error_title || text.lines().any(error_line) {
        "ERROR"
    } else if code_lines.count() >= 3 || has(&["visual studio code", "rustrover", "intellij", "pycharm", "notepad++", ".rs -", ".py -", ".js -", ".cpp -"]) {
        "CODE"
    } else if words >= 120 {
        "ARTICLE"
    } else {
        "PAGE"
    };
    let first = text.lines().map(str::trim).find(|l| l.chars().filter(|c| c.is_alphabetic()).count() >= 4).unwrap_or("");
    let line = crate::crash_guard::clip(if title.is_empty() { first } else { title.as_str() }, 64).to_string();
    let actions = match kind {
        "VIDEO" => [("SUMMARIZE", "Summarize the video on my screen"), ("EXPLAIN", "Explain what's on my screen"), ("TRANSLATE", "Translate what's on my screen")],
        "ERROR" => [("EXPLAIN", "Explain the error on my screen"), ("FIX", "How do I fix the error on my screen?"), ("SEARCH", "Search the web for the error on my screen")],
        "CODE" => [("EXPLAIN", "Explain the code on my screen"), ("SUMMARIZE", "Summarize the code on my screen"), ("REVIEW", "Review the code on my screen")],
        _ => [("SUMMARIZE", "Summarize what's on my screen"), ("EXPLAIN", "Explain what's on my screen"), ("TRANSLATE", "Translate what's on my screen")],
    };
    Card { kind, line, actions }
}

/// Whether `line` reads as an error message: "error[E0308]: …", "error: …", "TypeError: …",
/// "java.lang.IllegalStateException: …", "Traceback (most recent call last):", "thread 'main'
/// panicked at …", "fatal: …", "Fehler: …", "test result: FAILED".
fn error_line(line: &str) -> bool {
    let l = line.trim().to_lowercase();
    let head = l.split(':').next().unwrap_or("");
    let named = l.contains(':') && !head.is_empty() && !head.contains(char::is_whitespace) && (head.ends_with("error") || head.ends_with("exception") || head.starts_with("error["));
    named
        || l.contains(" panicked at ")
        || ["fatal:", "fatal error", "traceback (most recent call last)", "exception in thread", "unhandled exception", "uncaught exception", "uncaught error", "fehler:", "build failed", "test result: failed"].iter().any(|p| l.starts_with(p))
}

/// He read `text` on `label`: its card, from now (not in silent test runs).
pub fn seen(label: &str, text: &str) {
    if crate::live_self_tester::running() || crate::live_self_tester::dry() {
        return;
    }
    let card = classify(label, text);
    with(|s| s.card = Some((card, Instant::now())));
    crate::gui::refresh();
}

/// Where in its time (fractions of `CARD_TIME`) a card has fully faded in, and how long its
/// fade-out at the end takes.
const FADE_IN: f32 = 0.04;
const FADE_OUT: f32 = 0.12;

/// A card's opacity `age` (0-1) into its time.
pub fn card_alpha(age: f32) -> f32 {
    (age / FADE_IN).min((1.0 - age) / FADE_OUT).clamp(0.0, 1.0)
}

/// The card while it shows, and how far through its time it is (0-1). `held`: the pointer is
/// on it (it stays fully shown, its time starting again from there).
pub fn card(held: bool) -> Option<(Card, f32)> {
    with(|s| {
        let (card, at) = s.card.as_mut()?;
        if held {
            let now = Instant::now();
            *at = now.checked_sub(CARD_TIME.mul_f32(FADE_IN)).unwrap_or(now);
        }
        let age = at.elapsed();
        if age >= CARD_TIME {
            s.card = None;
            return None;
        }
        Some((card.clone(), age.as_secs_f32() / CARD_TIME.as_secs_f32()))
    })
}

/// The card goes (one of its actions was chosen).
pub fn dismiss() {
    with(|s| s.card = None);
}

/// How frosted the HUD is right now (0-1): fully while he captures the screen, clearing over
/// `FROST_TIME` after.
pub fn frost() -> f32 {
    with(|s| {
        if s.looks > 0 {
            return Some(1.0);
        }
        let t = s.looked?.elapsed().as_secs_f32() / FROST_TIME.as_secs_f32();
        if t >= 1.0 {
            s.looked = None;
            return None;
        }
        Some((1.0 - t).powi(2))
    })
    .unwrap_or(0.0)
}

/// While this lives, JAI's own windows are left out of screen captures (they stay on the
/// monitor) and the HUD is frosted; once the last one is dropped, they are captured again and
/// the frost clears.
pub struct Looking(());

/// Leaves JAI's own windows out of screen captures until the guard is dropped. The guard
/// exists from the start: a turn cancelled while this waits (ESC, "stop") ends the look all the
/// same. Both changes run on the Win32 worker in the order they were asked for, so a window is
/// never left out after its look has ended.
pub async fn look() -> Looking {
    with(|s| s.looks += 1);
    let guard = Looking(());
    crate::gui::refresh();
    #[cfg(windows)]
    let left_out = crate::win32::call(leave_out_own_windows).await.unwrap_or(false);
    #[cfg(not(windows))]
    let left_out = leave_out_own_windows();
    if left_out {
        // (The change shows from the next composition of the desktop.)
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    guard
}

impl Drop for Looking {
    fn drop(&mut self) {
        let last = with(|s| {
            s.looks = s.looks.saturating_sub(1);
            if s.looks == 0 {
                s.looked = Some(Instant::now());
            }
            s.looks == 0
        });
        if last {
            #[cfg(windows)]
            crate::win32::run_detached(let_own_windows_back);
            #[cfg(not(windows))]
            let_own_windows_back();
        }
        crate::gui::refresh();
    }
}

/// (Win32 worker) His windows leave captures, while a look is still on: true when any did.
fn leave_out_own_windows() -> bool {
    if with(|s| s.looks) == 0 {
        return false;
    }
    let handles = set_capture_affinity(&own_windows(), true);
    let any = !handles.is_empty();
    with(|s| {
        for h in handles {
            if !s.excluded.contains(&h) {
                s.excluded.push(h);
            }
        }
    });
    any
}

/// (Win32 worker) His windows are captured again, unless a new look has begun meanwhile.
fn let_own_windows_back() {
    let handles = with(|s| if s.looks == 0 { std::mem::take(&mut s.excluded) } else { Vec::new() });
    if !handles.is_empty() {
        set_capture_affinity(&handles, false);
    }
}

#[cfg(not(windows))]
fn own_windows() -> Vec<isize> {
    Vec::new()
}

#[cfg(not(windows))]
fn set_capture_affinity(_handles: &[isize], _out: bool) -> Vec<isize> {
    Vec::new()
}

/// JAI's own visible top-level windows (the HUD and every window of his).
#[cfg(windows)]
fn own_windows() -> Vec<isize> {
    let pid = std::process::id();
    let mut handles: Vec<isize> = crate::win32::app_windows().into_iter().filter(|w| w.pid == pid).map(|w| w.hwnd).collect();
    let hud = crate::gui::hud_hwnd();
    if hud != 0 && !handles.contains(&hud) {
        handles.push(hud);
    }
    handles
}

/// Leaves `handles` out of captures (`out`) or lets them be captured again; the handles it
/// changed.
#[cfg(windows)]
fn set_capture_affinity(handles: &[isize], out: bool) -> Vec<isize> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};
    let affinity = if out { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
    handles
        .iter()
        .copied()
        .filter(|h| {
            // SAFETY: a window handle of this process; a stale one only makes the call fail.
            let done = unsafe { SetWindowDisplayAffinity(HWND(*h), affinity) };
            if let Err(e) = &done {
                tracing::debug!("[SCREEN] Display affinity of window {h:#x}: {e}");
            }
            done.is_ok()
        })
        .collect()
}

/// Whether `hwnd` is left out of screen captures right now.
#[cfg(windows)]
pub fn left_out(hwnd: isize) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE};
    let mut affinity = 0u32;
    // SAFETY: plain query; a stale handle only makes it fail.
    unsafe { GetWindowDisplayAffinity(HWND(hwnd), &mut affinity) }.is_ok() && affinity == WDA_EXCLUDEFROMCAPTURE.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_names_what_was_on_the_screen() {
        let video = classify("the window \"How CPUs work - YouTube — Mozilla Firefox\"", "Subscribe\n1.2M views");
        assert_eq!((video.kind, video.line.as_str()), ("VIDEO", "How CPUs work - YouTube — Mozilla Firefox"));
        assert_eq!(video.actions.map(|a| a.0), ["SUMMARIZE", "EXPLAIN", "TRANSLATE"]);
        assert_eq!(classify("the window \"cmd\"", "error[E0308]: mismatched types").kind, "ERROR");
        for shown in ["Traceback (most recent call last):\n  File \"x.py\", line 1", "TypeError: x is not a function", "thread 'main' panicked at src/main.rs:2:5:", "java.lang.IllegalStateException: closed", "fatal: not a git repository"] {
            assert_eq!(classify("the window \"Terminal\"", shown).kind, "ERROR", "{shown}");
        }
        assert_eq!(classify("the window \"Application Error\"", "The program stopped.").kind, "ERROR", "a dialog titled so");
        let code = classify("Monitor 1 (primary)", "use std::io;\nfn main() {\n    let x = 1;\n}\n");
        assert_eq!(code.kind, "CODE");
        assert_eq!(code.line, "use std::io;", "no title: the first line it read");
        // The word alone is no error: in code, in a file's name, in prose.
        let errors_in_code = "fn parse(s: &str) -> Result<u32, Error> {\n    s.parse().map_err(|_| Error::Bad)\n}\nuse std::io;\n";
        assert_eq!(classify("the window \"parse.rs - Visual Studio Code\"", errors_in_code).kind, "CODE");
        assert_eq!(classify("the window \"error.rs - jai - Visual Studio Code\"", "let x = 1;").kind, "CODE");
        let long = "word ".repeat(150);
        assert_eq!(classify("the window \"Wikipedia — Firefox\"", &long).kind, "ARTICLE");
        let prose = format!("{} The first attempt failed, and the error was in the design. {}", "word ".repeat(80), "word ".repeat(60));
        assert_eq!(classify("the window \"History of flight — Firefox\"", &prose).kind, "ARTICLE");
        assert_eq!(classify("the window \"Settings\"", "Bluetooth & devices").kind, "PAGE");
        assert!(classify("the window \"x\"", "").actions.iter().all(|(word, ask)| !word.is_empty() && ask.contains("screen")));
    }

    /// The state is global: the tests that change it run one at a time.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn the_frost_clears_after_the_capture() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with(|s| *s = State::default());
        assert_eq!(frost(), 0.0);
        with(|s| s.looks = 1);
        assert_eq!(frost(), 1.0, "frosted while he looks");
        with(|s| {
            s.looks = 0;
            s.looked = Some(Instant::now() - FROST_TIME / 2);
        });
        let half = frost();
        assert!(half > 0.1 && half < 0.5, "{half}");
        with(|s| s.looked = Some(Instant::now() - FROST_TIME));
        assert_eq!(frost(), 0.0);
    }

    #[test]
    fn a_card_under_the_pointer_stays_fully_shown() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with(|s| *s = State::default());
        let shown = || classify("the window \"How CPUs work - YouTube\"", "Subscribe");
        with(|s| s.card = Some((shown(), Instant::now())));
        let (_, age) = card(false).expect("a card");
        assert!(card_alpha(age) < 0.5, "it fades in");
        for _ in 0..3 {
            let (_, age) = card(true).expect("held");
            assert_eq!(card_alpha(age), 1.0, "held: fully shown, frame after frame");
        }
        with(|s| s.card = Some((shown(), Instant::now() - CARD_TIME.mul_f32(0.95))));
        let (_, age) = card(false).expect("still there");
        assert!(card_alpha(age) < 0.5, "it fades out at the end");
        dismiss();
        assert!(card(false).is_none());
    }

    #[test]
    fn every_look_ends_and_overlapping_looks_share_the_frost() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with(|s| *s = State::default());
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().expect("runtime");
        let (a, b) = rt.block_on(async { (look().await, look().await) });
        assert_eq!(with(|s| s.looks), 2);
        drop(a);
        assert_eq!(frost(), 1.0, "the other look still runs: still frosted, still left out");
        drop(b);
        assert!(frost() < 1.0 && with(|s| s.looks) == 0);
        // A look whose turn is cancelled before it got anywhere: its guard is there from the first
        // step, and dropping it ends the look.
        with(|s| s.looks += 1);
        drop(Looking(()));
        assert_eq!(with(|s| s.looks), 0);
        // Once the worker has caught up, nothing of his is left out.
        #[cfg(windows)]
        let _ = rt.block_on(crate::win32::call(|| ()));
        assert!(with(|s| s.excluded.is_empty()));
    }
}
