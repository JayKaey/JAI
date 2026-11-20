//! Shutting down, restarting, sleeping and locking the PC on Sir's request.
//!
//! Shutdown and restart never run on the first word: the brain asks "Sir, please confirm …",
//! shows a crimson confirmation card and waits up to 30 s for a clear "yes" (or the card's
//! button). Only then JAI says goodbye and, 1.2 s later (the goodbye is heard), runs
//! `shutdown /s /t 0` or `shutdown /r /t 0` hidden. Sleep and lock are harmless and run at once.
//! "Shut down" on its own still quits JAI himself (`intents::SHUTDOWN`); only the PC named as
//! the object ("my PC", "the computer", "den Rechner") reaches this module.

use regex::Regex;
use std::sync::LazyLock;
use std::time::Duration;

/// How long a shutdown or restart question stays open.
pub const CONFIRM_WINDOW: Duration = Duration::from_secs(30);
/// How long after the question closed a bare "yes" or "no" is still taken as its late answer
/// (and told that nothing happened).
pub const LAPSED_ANSWER_WINDOW: Duration = Duration::from_secs(120);
/// Between the goodbye and the shutdown command, so the goodbye is heard.
pub const FAREWELL_DELAY: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Shutdown,
    Restart,
    Sleep,
    Lock,
}

impl PowerAction {
    /// Shutdown and restart end Sir's session: they are confirmed first.
    pub fn needs_confirmation(self) -> bool {
        matches!(self, Self::Shutdown | Self::Restart)
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
            Self::Restart => "restart",
            Self::Sleep => "sleep",
            Self::Lock => "lock",
        }
    }

    /// The double-check question.
    pub fn question(self) -> &'static str {
        match self {
            Self::Restart => "Sir, please confirm: would you like me to restart your PC right now?",
            _ => "Sir, please confirm: would you like me to shut down your PC right now?",
        }
    }

    /// The confirmation card's main button.
    pub fn card_label(self) -> &'static str {
        match self {
            Self::Restart => "Confirm PC Restart",
            _ => "Confirm PC Shutdown",
        }
    }

    /// What JAI says as he carries it out.
    pub fn farewell(self) -> &'static str {
        match self {
            Self::Shutdown => "Shutting down all systems now. Goodbye, Sir.",
            Self::Restart => "Restarting your PC now. I'll be back with it, Sir.",
            Self::Sleep => "Putting your PC to sleep, Sir.",
            Self::Lock => "Locking your PC, Sir.",
        }
    }

    /// What JAI says when Sir says no or the question times out.
    pub fn aborted(self) -> &'static str {
        match self {
            Self::Restart => "Restart aborted, Sir.",
            _ => "Shutdown aborted, Sir.",
        }
    }

    /// What it does to the PC ("what happens if I restart my PC?").
    pub fn consequence(self) -> &'static str {
        match self {
            Self::Shutdown => "Windows closes every program and switches the PC off, Sir; anything unsaved is lost.",
            Self::Restart => "Windows closes every program and starts again, Sir; anything unsaved is lost.",
            Self::Sleep => "The PC sleeps with everything left open, Sir, and wakes where you left it.",
            Self::Lock => "The PC locks, Sir, and everything keeps running behind the sign-in screen.",
        }
    }
}

/// A bare "yes" or "no" to a shutdown / restart question that has closed: what JAI says
/// (nothing runs). None for anything else, which is a request of its own.
pub fn late_answer(action: PowerAction, text: &str) -> Option<String> {
    let s = words(text);
    let s = s.trim_start_matches("jai ").trim_end_matches(" jai").trim();
    let (noun, deed) = match action {
        PowerAction::Restart => ("restart", "restart my PC"),
        _ => ("shutdown", "shut down my PC"),
    };
    if CONFIRM_WORDS.contains(&s) {
        return Some(format!("That {noun} question has lapsed, Sir, so nothing happened. Say \"{deed}\" again if you still want it."));
    }
    const NO: &[&str] = &[
        "no", "nope", "nah", "nein", "cancel", "cancel it", "abort", "abbrechen", "no thanks", "no thank you", "never mind", "nevermind", "forget it",
        "vergiss es", "lass es", "lieber nicht", "nein danke", "don't", "dont", "do not", "no don't", "no dont", "no cancel", "nein abbrechen",
    ];
    NO.contains(&s).then(|| "It's already called off, Sir. Your PC stays as it is.".to_string())
}

/// Lower-case words, punctuation gone, umlauts kept.
fn words(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The PC itself as the object of the sentence ("my system" too, never "your system": that is
/// JAI). ("By PC": how the recogniser heard "my PC", 2026-11-16.)
const PC: &str = r"(?:(?:my |by |the |this |your |our |den |meinen |meinem |dem |mein |deinen )?(?:pc|computer|laptop|rechner|machine|desktop|workstation|gaming pc|gaming rig|rig)|(?:my |the |this |our |my whole |my entire |the whole |the entire |mein |meinen |das )system)";

static SHUTDOWN_PC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        // ("Shut on my PC": how the recogniser hears "shut down" said quickly.)
        r"\b(?:shut\s*down|shutdown|shut on|shut off|turn off|switch off|power off|power down)\s+{PC}\b|\b{PC}\s+(?:shut\s*down|shutdown|herunterfahren|runterfahren|ausschalten|abschalten)\b|\b(?:fahr|fahre)\s+{PC}\s+(?:herunter|runter)\b|\b(?:schalt|schalte|mach|mache)\s+{PC}\s+aus\b|\b(?:put|send|set)\s+{PC}\s+(?:to|into|in)\s+(?:shut\s*down|shutdown|power off)(?:\s+mode)?\b"
    ))
    .expect("valid shutdown regex")
});
static RESTART_PC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\b(?:restart|reboot)\s+{PC}\b|\b{PC}\s+(?:neu starten|neustarten|neu booten|rebooten)\b|\b(?:starte|start|boote)\s+{PC}\s+neu\b|\bneustart (?:des|vom) (?:pcs?|computers?|rechners?)\b"
    ))
    .expect("valid restart regex")
});
static SLEEP_PC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\b(?:put|send|set)\s+{PC}\s+(?:to|into|in)\s+(?:sleep|standby|sleep mode)\b|\b(?:sleep|suspend|hibernate)\s+{PC}\b|\b{PC}\s+(?:in den )?(?:ruhezustand|standby|energiesparmodus|schlafmodus)\b|\b(?:schick|schicke)\s+{PC}\s+schlafen\b"
    ))
    .expect("valid sleep regex")
});
static LOCK_PC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\block\s+(?:{PC}|(?:my |the )?screen)\b|\b(?:{PC}|(?:den )?bildschirm)\s+sperren\b|\b(?:sperr|sperre)\s+(?:{PC}|(?:den )?bildschirm)\b"
    ))
    .expect("valid lock regex")
});
static NEGATED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:don't|dont|do not|never|nicht|nie|kein|keine|without)\b").expect("valid negation regex"));
/// "How do I shut down my PC?" asks for an explanation, not for the deed.
static EXPLANATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:how|why|what|when|wie|warum|wieso|was|wann)\b").expect("valid explanation regex"));

/// The power action Sir asks for, if any ("shut down my PC", "fahre meinen PC herunter",
/// "restart the computer", "put my PC to sleep", "lock my screen").
pub fn power_request(text: &str) -> Option<PowerAction> {
    let s = words(text);
    // "Should I lock my PC?", "is it safe to restart the computer?": a question, never the deed
    // (2026-11-07: sleep and lock run without a double-check).
    if s.is_empty() || NEGATED.is_match(&s) || EXPLANATION.is_match(&s) || crate::intents::only_asks(text) {
        return None;
    }
    if RESTART_PC.is_match(&s) {
        return Some(PowerAction::Restart);
    }
    if SHUTDOWN_PC.is_match(&s) {
        return Some(PowerAction::Shutdown);
    }
    if SLEEP_PC.is_match(&s) {
        return Some(PowerAction::Sleep);
    }
    if LOCK_PC.is_match(&s) {
        return Some(PowerAction::Lock);
    }
    None
}

/// A whole answer that says yes (the late answer only takes these, not "ok, play music").
const CONFIRM_WORDS: &[&str] = &[
    "yes", "yeah", "yep", "yup", "ja", "jawohl", "jo", "confirm", "confirmed", "i confirm", "do it", "go ahead", "go", "yes please", "yes do it",
    "please do", "shut it down", "restart it", "reboot it", "mach es", "mach das", "mach", "machs", "mach's", "tu es", "bestätigen", "bestätige",
    "bestaetigen", "ok", "okay", "sure", "affirmative", "absolutely", "of course", "klar", "genau", "los", "ja bitte", "ja mach", "ja mach es",
    "proceed", "yes sir", "do that", "make it so", "engage", "execute", "yes shut it down", "yes restart it", "ja fahr ihn runter",
];

/// "Yes", "ja", "confirm", "do it", "shut it down", "yes please", "mach es": the go-ahead.
pub fn is_confirmation(text: &str) -> bool {
    let s = words(text);
    if s.is_empty() || is_cancellation(text) {
        return false;
    }
    let trimmed = s.trim_start_matches("jai ").trim_end_matches(" jai").trim();
    CONFIRM_WORDS.contains(&trimmed)
        || trimmed.starts_with("confirm")
        || trimmed.starts_with("yes ")
        || trimmed.starts_with("ja ")
        || trimmed.starts_with("ok ")
        || trimmed.starts_with("okay ")
}

/// "No", "cancel", "stop", "abbrechen", "wait": the question is withdrawn.
pub fn is_cancellation(text: &str) -> bool {
    let s = words(text);
    static CANCEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b(?:no|nope|nah|nein|cancel|cancelled|stop|stopp|abort|abbrechen|abbruch|halt|wait|warte|never mind|nevermind|forget it|vergiss es|don't|dont|do not|nicht|lass es|lieber nicht)\b")
            .expect("valid cancel regex")
    });
    CANCEL.is_match(&s)
}

/// Carries the action out (after the confirmation, where one is needed).
pub async fn execute(action: PowerAction) -> Result<String, String> {
    imp::execute(action).await
}

/// `shutdown.exe` run exactly as `execute` runs it, with other arguments: the real-PC check
/// (`live_checks`) schedules a shutdown and a restart ten minutes out and calls each off at once.
#[cfg(all(test, windows))]
pub(crate) async fn run_shutdown_exe(args: &[&str]) -> Result<(), String> {
    imp::shutdown(args).await
}

#[cfg(windows)]
pub(crate) use imp::{lock_pc, sleep_pc};

#[cfg(windows)]
mod imp {
    use super::PowerAction;
    use std::path::PathBuf;
    use std::time::Duration;

    fn shutdown_exe() -> PathBuf {
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        root.join("System32").join("shutdown.exe")
    }

    /// `shutdown.exe` hidden (CREATE_NO_WINDOW, no busy cursor) with `args`.
    pub(super) async fn shutdown(args: &[&str]) -> Result<(), String> {
        let out = crate::shell::run_helper(&shutdown_exe(), args, None, &[], Duration::from_secs(10)).await.map_err(|e| format!("shutdown.exe could not start: {e}"))?;
        if out.success() {
            Ok(())
        } else {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if why.is_empty() { format!("shutdown.exe ended with code {:?}", out.code) } else { why })
        }
    }

    pub(crate) fn lock_pc() -> Result<(), String> {
        #[link(name = "user32")]
        extern "system" {
            fn LockWorkStation() -> i32;
        }
        // SAFETY: documented parameterless API acting on this interactive session.
        if unsafe { LockWorkStation() } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }

    pub(crate) fn sleep_pc() -> Result<(), String> {
        #[link(name = "PowrProf")]
        extern "system" {
            fn SetSuspendState(hibernate: u8, force: u8, disable_wake_events: u8) -> u8;
        }
        // SAFETY: documented BOOLEAN arguments: suspend (not hibernate), no forced close.
        if unsafe { SetSuspendState(0, 0, 0) } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }

    pub async fn execute(action: PowerAction) -> Result<String, String> {
        match action {
            PowerAction::Shutdown => shutdown(&["/s", "/t", "0"]).await.map(|_| "The PC is shutting down.".to_string()),
            PowerAction::Restart => shutdown(&["/r", "/t", "0"]).await.map(|_| "The PC is restarting.".to_string()),
            PowerAction::Sleep => tokio::task::spawn_blocking(sleep_pc).await.map_err(|e| e.to_string())?.map(|_| "The PC went to sleep.".to_string()),
            PowerAction::Lock => tokio::task::spawn_blocking(lock_pc).await.map_err(|e| e.to_string())?.map(|_| "The PC is locked.".to_string()),
        }
    }
}

/// logind's Manager on the system bus: the PC's own power verbs (polkit decides who may).
#[cfg(not(windows))]
fn logind(method: &str) -> Result<(), String> {
    let bus = zbus::blocking::Connection::system().map_err(|e| format!("no system bus: {e}"))?;
    bus.call_method(Some("org.freedesktop.login1"), "/org/freedesktop/login1", Some("org.freedesktop.login1.Manager"), method, &(false,)).map(drop).map_err(|e| e.to_string())
}

/// KDE's own screen locker, by the freedesktop name.
#[cfg(not(windows))]
pub(crate) fn lock_pc() -> Result<(), String> {
    crate::kde::call::<_, ()>("org.freedesktop.ScreenSaver", "/ScreenSaver", "org.freedesktop.ScreenSaver", "Lock", &())
}

#[cfg(not(windows))]
pub(crate) fn sleep_pc() -> Result<(), String> {
    logind("Suspend")
}

#[cfg(not(windows))]
mod imp {
    use super::PowerAction;

    pub async fn execute(action: PowerAction) -> Result<String, String> {
        let (done, work): (&str, fn() -> Result<(), String>) = match action {
            PowerAction::Shutdown => ("The PC is shutting down.", || super::logind("PowerOff")),
            PowerAction::Restart => ("The PC is restarting.", || super::logind("Reboot")),
            PowerAction::Sleep => ("The PC went to sleep.", super::sleep_pc),
            PowerAction::Lock => ("The PC is locked.", super::lock_pc),
        };
        crate::kde::blocking(work).await??;
        Ok(done.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_in_english_and_german() {
        for t in ["JAI, shut down my PC", "Turn off the computer", "Fahre meinen PC herunter", "shutdown my pc please", "schalte den Rechner aus", "PC herunterfahren"] {
            assert_eq!(power_request(t), Some(PowerAction::Shutdown), "{t}");
        }
        for t in ["Restart my PC", "reboot the computer", "Starte den PC neu", "Rechner neu starten"] {
            assert_eq!(power_request(t), Some(PowerAction::Restart), "{t}");
        }
        assert_eq!(power_request("put my pc to sleep"), Some(PowerAction::Sleep));
        assert_eq!(power_request("PC in den Ruhezustand"), Some(PowerAction::Sleep));
        assert_eq!(power_request("lock my screen"), Some(PowerAction::Lock));
        assert_eq!(power_request("sperre den PC"), Some(PowerAction::Lock));
        // An order in good manners is still the order.
        assert_eq!(power_request("Can you lock my PC?"), Some(PowerAction::Lock));
        assert_eq!(power_request("Could you restart the computer?"), Some(PowerAction::Restart));
        // Sir's own words (2026-11-07): his system, a shutdown mode, and "shut on" as heard.
        for t in ["Please shutdown my system.", "shut down my system", "Can you put my PC into shutdown mode?", "can you shut on my PC please?", "restart the whole system"] {
            assert!(power_request(t).is_some(), "{t}");
        }
        assert_eq!(power_request("restart the whole system"), Some(PowerAction::Restart));
        // 2026-11-16: "shut off" closed an app called "off my pc"; "by PC" is "my PC" misheard.
        for t in ["please shut off my PC", "please power down by PC", "shut off my computer", "power off my PC", "power down my PC", "turn off my PC", "fahr den PC runter", "schalte den PC aus"] {
            assert_eq!(power_request(t), Some(PowerAction::Shutdown), "{t}");
        }
    }

    #[test]
    fn not_requests() {
        for t in ["shut down", "close yourself", "don't shut down my PC", "how do I shut down my PC?", "turn down the volume", "shut down Discord", "restart Spotify", "Should I lock my PC?", "Should I restart my PC?", "Is it safe to put my PC to sleep?", "Soll ich den PC sperren?", "shut down your system", "shut down all systems", "shut off Spotify", "shut Discord", "close Spotify"] {
            assert_eq!(power_request(t), None, "{t}");
        }
    }

    #[test]
    fn a_late_yes_is_told_the_question_lapsed_and_nothing_else_is_taken() {
        let line = late_answer(PowerAction::Restart, "Yes").expect("a late yes");
        assert!(line.contains("lapsed") && line.contains("\"restart my PC\""), "{line}");
        assert!(late_answer(PowerAction::Shutdown, "Yes, please").unwrap().contains("\"shut down my PC\""));
        assert!(late_answer(PowerAction::Shutdown, "No").unwrap().contains("called off"));
        // A request of its own is no answer to the old question.
        for t in ["ok play some music", "stop the music", "yes open Spotify", "what's the time?", "restart my PC"] {
            assert_eq!(late_answer(PowerAction::Restart, t), None, "{t}");
        }
    }

    #[test]
    fn confirmations_and_cancellations() {
        for t in ["Yes", "Ja", "Confirm", "Do it", "Shut it down", "Yes please", "Mach es", "confirm pc shutdown", "Yes, do it."] {
            assert!(is_confirmation(t), "{t}");
        }
        for t in ["No", "Cancel", "Stop", "Abbrechen", "no, don't", "wait", "cancel pc shutdown"] {
            assert!(is_cancellation(t), "{t}");
            assert!(!is_confirmation(t), "{t}");
        }
        assert!(!is_confirmation("what's the weather"));
    }

    #[test]
    fn only_shutdown_and_restart_are_double_checked() {
        assert!(PowerAction::Shutdown.needs_confirmation() && PowerAction::Restart.needs_confirmation());
        assert!(!PowerAction::Sleep.needs_confirmation() && !PowerAction::Lock.needs_confirmation());
    }
}
