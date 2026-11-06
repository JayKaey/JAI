//! "Stop listening" — Sir's privacy lock. "Jarvis, stop listening for a minute" / "for 5 minutes"
//! / "Hör für 2 Minuten nicht zu" pauses him for that long; "stop listening", "mute my
//! microphone", "don't listen", "Hör auf zuzuhören" until Sir lets him listen again. While locked
//! every word is ignored — commands, questions, even "Jarvis" — except the resume phrases ("you
//! can listen again", "resume listening", "unmute microphone", "Du kannst wieder zuhören",
//! "Jarvis hör wieder zu") and the header's `[ MIC: MUTED ]` button. A timed pause ends on
//! its own.

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListeningLockState {
    Listening,
    PausedUntil(Instant),
    LockedUntilWakePhrase,
}

/// What a phrase asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockCommand {
    /// Pause for this long.
    Pause(Duration),
    /// Until Sir says he may listen again.
    Lock,
    Resume,
}

static STATE: Mutex<ListeningLockState> = Mutex::new(ListeningLockState::Listening);

/// The state now (a timed pause that ran out reads as listening).
pub fn state() -> ListeningLockState {
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let ListeningLockState::PausedUntil(until) = *s {
        if Instant::now() >= until {
            *s = ListeningLockState::Listening;
        }
    }
    *s
}

pub fn is_locked() -> bool {
    state() != ListeningLockState::Listening
}

pub fn apply(cmd: LockCommand) {
    let next = match cmd {
        LockCommand::Pause(d) => ListeningLockState::PausedUntil(Instant::now() + d),
        LockCommand::Lock => ListeningLockState::LockedUntilWakePhrase,
        LockCommand::Resume => ListeningLockState::Listening,
    };
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = next;
    crate::gui::refresh();
}

/// The header's status: `LISTENING PAUSED (0:48) — SAY "YOU CAN LISTEN AGAIN" TO RESUME`.
pub fn status_line() -> Option<String> {
    match state() {
        ListeningLockState::Listening => None,
        ListeningLockState::PausedUntil(until) => {
            let left = until.saturating_duration_since(Instant::now()).as_secs();
            Some(format!("LISTENING PAUSED ({}:{:02}) — SAY \"YOU CAN LISTEN AGAIN\" TO RESUME", left / 60, left % 60))
        }
        ListeningLockState::LockedUntilWakePhrase => Some("LISTENING PAUSED — SAY \"YOU CAN LISTEN AGAIN\" TO RESUME".into()),
    }
}

fn words(text: &str) -> String {
    let s: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == ' ' { c } else { ' ' }).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const RESUME: &[&str] = &[
    "you can listen again",
    "you can now listen again",
    "you may listen again",
    "start listening again",
    "resume listening",
    "listen again",
    "unmute microphone",
    "unmute my microphone",
    "unmute the microphone",
    "unmute mic",
    "du kannst wieder zuhören",
    "du kannst wieder zuhoeren",
    "hör wieder zu",
    "hoer wieder zu",
    "du darfst wieder zuhören",
];

/// Whether `text` lets him listen again (the only words that get through the lock).
pub fn is_resume_phrase(text: &str) -> bool {
    let s = words(text);
    RESUME.iter().any(|p| s.contains(p))
}

/// "for a minute", "for 5 minutes", "für 2 Minuten", "for 30 seconds", "for an hour".
fn duration_in(s: &str) -> Option<Duration> {
    let w: Vec<&str> = s.split_whitespace().collect();
    for (i, unit) in w.iter().enumerate() {
        let secs = match *unit {
            "second" | "seconds" | "sekunde" | "sekunden" => 1,
            "minute" | "minutes" | "min" | "minuten" => 60,
            "hour" | "hours" | "stunde" | "stunden" => 3600,
            _ => continue,
        };
        let n = match i.checked_sub(1).map(|k| w[k]) {
            Some("a" | "an" | "one" | "eine" | "einer" | "einen" | "1") => 1,
            Some("two" | "zwei" | "couple") => 2,
            Some("three" | "drei") => 3,
            Some("five" | "fünf" | "fuenf") => 5,
            Some("ten" | "zehn") => 10,
            Some("fifteen" | "fünfzehn") => 15,
            Some("thirty" | "dreißig" | "dreissig") => 30,
            Some(n) => n.parse().ok()?,
            None => 1,
        };
        return Some(Duration::from_secs((n * secs).clamp(5, 24 * 3600)));
    }
    None
}

/// The lock command in `text`, if any. Never one about the Discord call ("stop listening to the
/// call" ends the call bridge instead).
pub fn parse(text: &str) -> Option<LockCommand> {
    let s = words(text);
    if is_resume_phrase(&s) {
        return Some(LockCommand::Resume);
    }
    if ["call", "discord", "anruf", "the others", "my friends"].iter().any(|w| s.contains(w)) {
        return None;
    }
    let stop = [
        "stop listening",
        "don t listen",
        "dont listen",
        "do not listen",
        "mute my microphone",
        "mute the microphone",
        "mute your microphone",
        "mute microphone",
        "mute my mic",
        "hör auf zuzuhören",
        "hoer auf zuzuhoeren",
        "hör auf zu zuhören",
        "hör nicht zu",
        "hoer nicht zu",
    ];
    // "Hör für 2 Minuten nicht zu": the German pause wraps the duration.
    let german_pause = (s.starts_with("hör für") || s.contains(" hör für") || s.starts_with("hoer für")) && s.ends_with("nicht zu");
    if !stop.iter().any(|p| s.contains(p)) && !german_pause {
        return None;
    }
    Some(match duration_in(&s) {
        Some(d) => LockCommand::Pause(d),
        None => LockCommand::Lock,
    })
}

/// What he says as he goes quiet / comes back.
pub fn reply(cmd: LockCommand) -> String {
    match cmd {
        LockCommand::Pause(d) if d.as_secs() >= 120 => format!("Understood, Sir. I'll stop listening for {} minutes.", d.as_secs() / 60),
        LockCommand::Pause(d) if d.as_secs() >= 60 => "Understood, Sir. I'll stop listening for a minute.".into(),
        LockCommand::Pause(d) => format!("Understood, Sir. I'll stop listening for {} seconds.", d.as_secs()),
        LockCommand::Lock => "Understood, Sir. I'm not listening until you tell me I can listen again.".into(),
        LockCommand::Resume => "I'm listening again, Sir.".into(),
    }
}

#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: Mutex<()> = Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_pause_lock_and_resume_in_english_and_german() {
        assert_eq!(parse("Jarvis, stop listening for a minute"), Some(LockCommand::Pause(Duration::from_secs(60))));
        assert_eq!(parse("Stop listening for 5 minutes"), Some(LockCommand::Pause(Duration::from_secs(300))));
        assert_eq!(parse("Hör für 2 Minuten nicht zu"), Some(LockCommand::Pause(Duration::from_secs(120))));
        assert_eq!(parse("stop listening for thirty seconds"), Some(LockCommand::Pause(Duration::from_secs(30))));
        for lock in ["Jarvis, stop listening", "Mute my microphone", "Don't listen", "Hör auf zuzuhören"] {
            assert_eq!(parse(lock), Some(LockCommand::Lock), "{lock}");
        }
        for resume in ["You can listen again", "Jarvis, you can now listen again", "Start listening again", "Resume listening", "Unmute microphone", "Du kannst wieder zuhören", "Jarvis hör wieder zu"] {
            assert_eq!(parse(resume), Some(LockCommand::Resume), "{resume}");
            assert!(is_resume_phrase(resume));
        }
        // The Discord bridge keeps its own phrase.
        assert_eq!(parse("stop listening to the call"), None);
        assert_eq!(parse("what's the weather"), None);
    }

    #[test]
    fn while_locked_only_the_resume_phrase_gets_through() {
        let _g = test_lock();
        apply(LockCommand::Lock);
        assert!(is_locked());
        assert!(status_line().unwrap().contains("YOU CAN LISTEN AGAIN"));
        for ignored in ["Jarvis", "Jarvis, what time is it?", "Jarvis, stop listening"] {
            assert!(!is_resume_phrase(ignored), "{ignored} must be ignored");
        }
        assert!(is_resume_phrase("Jarvis, you can listen again"));
        apply(LockCommand::Resume);
        assert!(!is_locked() && status_line().is_none());
        // A timed pause runs out on its own; the status counts down.
        apply(LockCommand::Pause(Duration::from_millis(60)));
        assert!(matches!(state(), ListeningLockState::PausedUntil(_)));
        assert!(status_line().unwrap().starts_with("LISTENING PAUSED (0:00)"));
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(state(), ListeningLockState::Listening);
        assert_eq!(reply(LockCommand::Pause(Duration::from_secs(300))), "Understood, Sir. I'll stop listening for 5 minutes.");
    }
}
