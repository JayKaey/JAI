//! Conversational focus: what "it", "that", "full screen", "close it", "move it to the left" and
//! "undo that edit" refer to.
//!
//! JAI remembers the app, website, file and JAI window the conversation was last about, and
//! the window Sir was working in right before he spoke or typed (captured at the start of every
//! turn, never JAI's own HUD). Requests without a name resolve against it, so "open Discord" …
//! "full screen" puts Discord in full screen and never JAI: JAI is only a target when Sir
//! says so ("fullscreen yourself", "maximize JAI").

use crate::winctl::WindowTarget;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// "Full screen" a while after "open Discord" still means Discord; later it means the window Sir
/// is working in.
const APP_FOCUS_TTL: Duration = Duration::from_secs(10 * 60);
/// "Undo that edit" and "open that file in VS Code" work for longer.
const FILE_FOCUS_TTL: Duration = Duration::from_secs(2 * 60 * 60);
/// "Yes, do that" answers an offer for this long.
const OFFER_TTL: Duration = Duration::from_secs(3 * 60);
/// The longest a turn waits for the foreground capture on the shared Win32 thread.
#[cfg_attr(not(windows), allow(dead_code))]
const FOREGROUND_WAIT: Duration = Duration::from_millis(250);

/// Something JAI offered or announced without doing it yet ("Shall I open a live telemetry
/// window?"): "yes" / "do it" runs `tool` with `args`; "terminate it" closes `terminate`.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingOffer {
    pub tool: String,
    pub args: serde_json::Value,
    pub terminate: Option<String>,
}

/// What the conversation is about right now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConversationalFocus {
    /// "discord", "spotify", "browser" (a website was opened), "explorer" (a folder) …
    pub last_target_app: Option<String>,
    pub last_opened_url_or_site: Option<String>,
    pub last_edited_or_found_file: Option<PathBuf>,
    pub last_active_jai_window_id: Option<String>,
    /// `"<title>" (<process>)` of the window Sir was in before his latest request.
    pub last_os_foreground_window: Option<String>,
}

/// The window Sir was working in (`handle` is the native window handle, 0 where there is none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundWindow {
    pub handle: isize,
    pub title: String,
    pub process: String,
}

#[derive(Debug, Clone)]
struct Stamped<T> {
    value: T,
    at: Instant,
}

impl<T> Stamped<T> {
    fn now(value: T) -> Self {
        Self { value, at: Instant::now() }
    }

    fn fresh(&self, ttl: Duration) -> bool {
        self.at.elapsed() < ttl
    }
}

/// Something of his on screen that is no window: a view of the HUD (the `configure_interface`
/// key that opens it) or a widget (its id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnThing {
    View(String),
    Widget(String),
    /// One of his panels opened by name (`open_jai_view`: "earth", "network_radar", …).
    Panel(String),
}

#[derive(Default)]
struct State {
    app: Option<Stamped<String>>,
    site: Option<Stamped<String>>,
    file: Option<Stamped<PathBuf>>,
    window: Option<Stamped<String>>,
    own: Option<Stamped<OwnThing>>,
    foreground: Option<Stamped<ForegroundWindow>>,
    offer: Option<Stamped<PendingOffer>>,
}

/// "It", "that", "the window" … — words that name whatever the conversation is about.
pub fn is_pronoun(target: &str) -> bool {
    let t = target.trim().to_lowercase();
    let t = t.trim_matches(|c: char| !c.is_alphanumeric() && c != ' ');
    matches!(
        t,
        "" | "it" | "that" | "this" | "that one" | "this one" | "that app" | "this app" | "the app" | "that window" | "this window"
            | "the window" | "window" | "that program" | "this program" | "the program" | "the active window" | "active window"
            | "the current window" | "current window" | "the current app" | "current app" | "es" | "das" | "dies" | "dieses"
            | "ihn" | "sie" | "das fenster" | "dieses fenster" | "das programm" | "die app" | "das ding"
    )
}

#[derive(Clone, Default)]
pub struct FocusTracker {
    state: Arc<RwLock<State>>,
}

impl FocusTracker {
    pub fn new() -> Self {
        Self::default()
    }

    fn read<R>(&self, f: impl FnOnce(&State) -> R) -> R {
        f(&self.state.read().unwrap_or_else(|e| e.into_inner()))
    }

    fn write(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.write().unwrap_or_else(|e| e.into_inner()));
    }

    // ------------------------------------------------------------------- updates ----

    /// An app the conversation is now about ("discord", "browser", "vs code").
    pub fn note_app(&self, app: &str) {
        let app = crate::apps::normalize(app);
        if app.is_empty() || is_pronoun(&app) || crate::tools::names_jai(&app) {
            return;
        }
        self.write(|s| s.app = Some(Stamped::now(app)));
    }

    /// A website, channel or search was opened: the browser is now the target.
    pub fn note_site(&self, url: &str) {
        let now = Instant::now();
        self.write(|s| {
            s.app = Some(Stamped { value: "browser".into(), at: now });
            s.site = Some(Stamped { value: url.to_string(), at: now });
        });
    }

    pub fn note_file(&self, path: &Path) {
        self.write(|s| s.file = Some(Stamped::now(path.to_path_buf())));
    }

    pub fn note_jai_window(&self, id: &str) {
        if !id.is_empty() {
            self.write(|s| s.window = Some(Stamped::now(id.to_string())));
        }
    }

    /// A view or a widget of his went up or was changed: "close it" means that from now on.
    pub fn note_own_thing(&self, thing: OwnThing) {
        self.write(|s| s.own = Some(Stamped::now(thing)));
    }

    pub fn forget_own_thing(&self, thing: &OwnThing) {
        self.write(|s| {
            if s.own.as_ref().is_some_and(|own| own.value == *thing) {
                s.own = None;
            }
        });
    }

    /// How long ago the view, widget or panel of his that "it" may mean came up.
    pub fn own_thing_age(&self) -> Option<Duration> {
        self.read(|s| s.own.as_ref().map(|own| own.at.elapsed()))
    }

    /// The view or widget of his that "it" means: nothing has come up since, no app, no window.
    pub fn own_thing_is_latest(&self) -> Option<OwnThing> {
        self.read(|s| {
            let own = s.own.as_ref()?;
            let after_app = s.app.as_ref().is_none_or(|a| own.at >= a.at || !a.fresh(APP_FOCUS_TTL));
            let after_window = s.window.as_ref().is_none_or(|w| own.at >= w.at);
            (after_app && after_window).then(|| own.value.clone())
        })
    }

    /// What `open_application` opened: a URL or website (the browser), a folder or file, or an app.
    pub fn note_opened(&self, name: &str) {
        let trimmed = name.trim().trim_matches('"');
        if let Some(url) = crate::apps::as_url(trimmed) {
            self.note_site(&url);
        } else if crate::apps::find_spec(trimmed).is_some_and(|s| s.id == "browser") {
            self.note_app("browser");
        } else if let Some(site) =
            crate::sites::find(trimmed).filter(|_| crate::apps::find_spec(trimmed).is_none() && !crate::appindex::is_known(trimmed))
        {
            self.note_site(&site.home_url());
        } else if Path::new(trimmed).exists() {
            let path = Path::new(trimmed);
            self.note_file(path);
            if path.is_dir() {
                self.note_app("explorer");
            }
        } else {
            self.note_app(trimmed);
        }
    }

    /// The app was closed: "full screen" no longer means it.
    pub fn forget_app(&self, app: &str) {
        let app = crate::apps::normalize(app);
        self.write(|s| {
            if s.app.as_ref().is_some_and(|a| a.value == app) {
                s.app = None;
            }
        });
    }

    pub fn forget_jai_window(&self, id: &str) {
        self.write(|s| {
            if id == "all" || s.window.as_ref().is_some_and(|w| w.value == id) {
                s.window = None;
            }
        });
    }

    pub fn note_foreground(&self, window: ForegroundWindow) {
        self.write(|s| s.foreground = Some(Stamped::now(window)));
    }

    /// JAI offered (or claimed) an action; Sir's "yes" carries it out.
    pub fn note_offer(&self, offer: PendingOffer) {
        self.write(|s| s.offer = Some(Stamped::now(offer)));
    }

    /// The offer "yes" answers (consumed).
    pub fn take_offer(&self) -> Option<PendingOffer> {
        let mut out = None;
        self.write(|s| out = s.offer.take().filter(|o| o.fresh(OFFER_TTL)).map(|o| o.value));
        out
    }

    pub fn clear_offer(&self) {
        self.write(|s| s.offer = None);
    }

    /// Remembers the window Sir is working in, skipping JAI's own (called as a turn starts).
    pub async fn capture_foreground(&self) {
        #[cfg(windows)]
        {
            // The capture runs on the shared Win32 thread. A turn waits for it at most
            // FOREGROUND_WAIT: when that thread is busy, the capture still lands when it runs and
            // the turn goes on without it (only an unnamed "full screen" could miss it).
            let tracker = self.clone();
            let asked = Instant::now();
            let job = crate::win32::call(move || {
                let queued = asked.elapsed();
                let started = Instant::now();
                if let Some(w) = crate::win32::user_window() {
                    tracker.note_foreground(ForegroundWindow { handle: w.hwnd, title: w.title, process: w.process });
                }
                (queued, started.elapsed())
            });
            match tokio::time::timeout(FOREGROUND_WAIT, job).await {
                Ok(Ok((queued, ran))) if queued + ran > Duration::from_millis(150) => {
                    tracing::warn!("[FOCUS] Foreground capture took {} ms ({} ms queued on the Win32 thread, {} ms enumerating windows)", (queued + ran).as_millis(), queued.as_millis(), ran.as_millis());
                }
                Err(_) => tracing::warn!("[FOCUS] The Win32 thread is busy; the turn goes on without waiting for the foreground capture (> {} ms)", FOREGROUND_WAIT.as_millis()),
                _ => {}
            }
        }
        #[cfg(not(windows))]
        {
            let window = crate::context::desktop()
                .await
                .foreground
                .filter(|(title, process)| !title.contains("J.A.I.") && !process.eq_ignore_ascii_case("jai"))
                .map(|(title, process)| ForegroundWindow { handle: 0, title, process });
            if let Some(w) = window {
                self.note_foreground(w);
            }
        }
    }

    // ------------------------------------------------------------------- queries ----

    pub fn snapshot(&self) -> ConversationalFocus {
        self.read(|s| ConversationalFocus {
            last_target_app: s.app.as_ref().map(|a| a.value.clone()),
            last_opened_url_or_site: s.site.as_ref().map(|a| a.value.clone()),
            last_edited_or_found_file: s.file.as_ref().map(|a| a.value.clone()),
            last_active_jai_window_id: s.window.as_ref().map(|a| a.value.clone()),
            last_os_foreground_window: s.foreground.as_ref().map(|f| format!("\"{}\" ({})", f.value.title, f.value.process)),
        })
    }

    /// The app "it" means, when the conversation was about one recently.
    pub fn focused_app(&self) -> Option<String> {
        self.read(|s| s.app.as_ref().filter(|a| a.fresh(APP_FOCUS_TTL)).map(|a| a.value.clone()))
    }

    /// How long ago the app in focus was opened or named.
    pub fn focused_app_age(&self) -> Option<Duration> {
        self.read(|s| s.app.as_ref().map(|a| a.at.elapsed()))
    }

    /// The file "that file" means.
    pub fn last_file(&self) -> Option<PathBuf> {
        self.read(|s| s.file.as_ref().filter(|f| f.fresh(FILE_FOCUS_TTL)).map(|f| f.value.clone()))
    }

    pub fn foreground(&self) -> Option<ForegroundWindow> {
        self.read(|s| s.foreground.as_ref().map(|f| f.value.clone()))
    }

    /// The last thing opened was a website (so "close it" closes its tab, not the browser).
    pub fn website_is_latest(&self) -> bool {
        self.read(|s| match (&s.site, &s.app) {
            (Some(site), Some(app)) => app.value == "browser" && site.at >= app.at && site.fresh(APP_FOCUS_TTL),
            _ => false,
        })
    }

    /// A JAI window was dealt with more recently than any app ("close that window").
    pub fn jai_window_is_latest(&self) -> Option<String> {
        self.read(|s| {
            let window = s.window.as_ref()?;
            let newer = s.app.as_ref().is_none_or(|a| window.at >= a.at || !a.fresh(APP_FOCUS_TTL));
            newer.then(|| window.value.clone())
        })
    }

    /// Where an unnamed window action goes, best first: the app the conversation is about, the
    /// window Sir was working in before this request, the window he is working in now. Never
    /// JAI himself.
    pub fn window_targets(&self) -> Vec<WindowTarget> {
        let mut targets = Vec::new();
        if let Some(app) = self.focused_app() {
            targets.push(WindowTarget::Focus(app));
        }
        if let Some(fg) = self.foreground() {
            targets.push(WindowTarget::Handle { id: fg.handle, title: fg.title, process: fg.process });
        }
        targets.push(WindowTarget::Active);
        targets
    }

    /// The focus lines of the live context block.
    pub fn describe(&self) -> String {
        fn ago(at: Instant) -> String {
            match at.elapsed().as_secs() {
                0..=59 => "just now".into(),
                s => format!("{} min ago", s / 60),
            }
        }
        self.read(|s| {
            let mut parts = Vec::new();
            if let Some(a) = s.app.as_ref().filter(|a| a.fresh(APP_FOCUS_TTL)) {
                parts.push(format!("app {} ({})", crate::apps::display_name(&a.value), ago(a.at)));
            }
            if let Some(site) = s.site.as_ref().filter(|a| a.fresh(APP_FOCUS_TTL)) {
                parts.push(format!("website {} ({})", site.value, ago(site.at)));
            }
            if let Some(f) = s.file.as_ref().filter(|f| f.fresh(FILE_FOCUS_TTL)) {
                parts.push(format!("file {} ({})", f.value.display(), ago(f.at)));
            }
            if let Some(w) = &s.window {
                parts.push(format!("JAI window {} ({})", w.value, ago(w.at)));
            }
            let mut out = if parts.is_empty() { "Conversation focus: nothing yet".to_string() } else { format!("Conversation focus: {}", parts.join("; ")) };
            out.push_str(" — \"it\", \"that\", \"full screen\", \"close it\" mean this, never you.");
            out
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A busy Win32 thread (a slow window or COM call queued before the turn) never holds a
    /// turn for longer than FOREGROUND_WAIT.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_busy_win32_thread_does_not_stall_the_turn() {
        crate::win32::run_detached(|| std::thread::sleep(Duration::from_millis(700)));
        let tracker = FocusTracker::default();
        let started = Instant::now();
        tracker.capture_foreground().await;
        let waited = started.elapsed();
        assert!(waited >= FOREGROUND_WAIT && waited < Duration::from_millis(450), "waited {waited:?}");
    }

    #[test]
    fn pronouns_name_the_focus() {
        for p in ["it", "That", "this window", "the app", "es", "das Fenster", ""] {
            assert!(is_pronoun(p), "{p}");
        }
        for p in ["discord", "the browser", "spotify app", "yourself"] {
            assert!(!is_pronoun(p), "{p}");
        }
    }

    #[test]
    fn targets_prefer_the_app_then_the_window_sir_was_in() {
        let f = FocusTracker::new();
        assert_eq!(f.window_targets(), vec![WindowTarget::Active]);
        f.note_foreground(ForegroundWindow { handle: 42, title: "Notes".into(), process: "notepad".into() });
        f.note_app("Discord");
        assert_eq!(
            f.window_targets(),
            vec![
                WindowTarget::Focus("discord".into()),
                WindowTarget::Handle { id: 42, title: "Notes".into(), process: "notepad".into() },
                WindowTarget::Active
            ]
        );
        f.forget_app("discord");
        assert_eq!(f.focused_app(), None);
    }

    #[test]
    fn a_view_or_widget_of_his_is_it_until_something_else_comes_up() {
        let f = FocusTracker::new();
        assert_eq!(f.own_thing_is_latest(), None);
        f.note_app("firefox");
        let mixer = OwnThing::View("mixer_view".into());
        f.note_own_thing(mixer.clone());
        assert_eq!(f.own_thing_is_latest().as_ref(), Some(&mixer), "newer than the app");
        f.note_jai_window("drive_space");
        assert_eq!(f.own_thing_is_latest(), None, "a window came up after it");
        f.note_own_thing(OwnThing::Widget("timer_300".into()));
        assert_eq!(f.own_thing_is_latest(), Some(OwnThing::Widget("timer_300".into())));
        f.note_app("discord");
        assert_eq!(f.own_thing_is_latest(), None, "the conversation moved to an app");
        f.note_own_thing(mixer.clone());
        f.forget_own_thing(&OwnThing::View("core_view".into()));
        assert_eq!(f.own_thing_is_latest().as_ref(), Some(&mixer), "only that thing is forgotten");
        f.forget_own_thing(&mixer);
        assert_eq!(f.own_thing_is_latest(), None);
    }

    #[test]
    fn websites_focus_the_browser_and_jai_is_never_the_app() {
        let f = FocusTracker::new();
        f.note_opened("https://www.youtube.com/@BastiGHG");
        assert_eq!(f.focused_app().as_deref(), Some("browser"));
        assert!(f.website_is_latest());
        f.note_app("spotify");
        assert!(!f.website_is_latest(), "an app opened after the website is the focus now");
        f.note_app("yourself");
        f.note_app("JAI");
        assert_eq!(f.focused_app().as_deref(), Some("spotify"));
        f.note_jai_window("window_1");
        assert_eq!(f.jai_window_is_latest().as_deref(), Some("window_1"));
        f.note_app("discord");
        assert_eq!(f.jai_window_is_latest(), None);
        assert!(f.describe().contains("Discord"));
    }
}
