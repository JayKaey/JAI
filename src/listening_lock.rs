//! "Stop listening" — Sir's privacy lock. "JAI, stop listening for a minute" / "for 5 minutes"
//! / "Hör für 2 Minuten nicht zu" pauses him for that long; "stop listening", "mute my
//! microphone", "don't listen", "Hör auf zuzuhören" until Sir lets him listen again. While locked
//! every word is ignored — commands, questions, even "JAI" — except the resume phrases ("you
//! can listen again", "resume listening", "unmute microphone", "Du kannst wieder zuhören",
//! "JAI hör wieder zu") and the header's `[ MIC: MUTED ]` button. A timed pause ends on
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

/// "Is my mic muted?", "are you listening?" in his words.
pub fn spoken_state() -> String {
    match state() {
        ListeningLockState::Listening => "I'm listening, Sir.".to_string(),
        ListeningLockState::PausedUntil(until) => {
            let left = until.saturating_duration_since(Instant::now()).as_secs();
            let span = if left >= 120 { format!("{} more minutes", left.div_ceil(60)) } else if left >= 60 { "another minute".to_string() } else { format!("{left} more seconds") };
            format!("I've stopped listening for {span}, Sir.")
        }
        ListeningLockState::LockedUntilWakePhrase => "I'm not listening, Sir, until you tell me I can listen again.".to_string(),
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
    "you can listen now",
    "you may listen now",
    "start listening",
    "begin listening",
    "continue listening",
    "keep listening",
    "resume listening",
    "listen again",
    "unmute microphone",
    "unmute my microphone",
    "unmute the microphone",
    "unmute your microphone",
    "unmute mic",
    "unmute my mic",
    "unmute the mic",
    "unmute your mic",
    "turn on the mic",
    "turn on my mic",
    "turn on your mic",
    "turn the mic on",
    "turn my mic on",
    "turn the mic back on",
    "turn my mic back on",
    "turn on the microphone",
    "turn on my microphone",
    "turn the microphone on",
    "turn my microphone on",
    "turn the microphone back on",
    "turn my microphone back on",
    "mikro an",
    "mikrofon an",
    "mikro wieder an",
    "mikrofon wieder an",
    "du kannst wieder zuhören",
    "du kannst wieder zuhoeren",
    "hör wieder zu",
    "hoer wieder zu",
    "du darfst wieder zuhören",
    "fang an zuzuhören",
    "fang wieder an zuzuhören",
];

/// Whether `text` lets him listen again (the only words that get through the lock).
pub fn is_resume_phrase(text: &str) -> bool {
    let s = words(text);
    RESUME.iter().any(|p| s.contains(p))
}

/// What happens to something Sir said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Not locked: it is heard.
    Heard,
    /// Locked, and these are the words that lift the lock.
    Resumes,
    /// Locked, or the microphone is switched off: dropped unheard, the wake word included.
    Ignored,
}

/// The one decision the microphone path makes for every transcript. `mic_on`: the microphone
/// is switched on, or the words were said with the push-to-talk key held. Under the lock only
/// the words that lift it get through; with the microphone switched off nothing does (words
/// recorded a moment before the `[ MIC ]` chip was clicked are not acted on after it).
pub fn verdict(text: &str, mic_on: bool) -> Verdict {
    if is_locked() {
        if is_resume_phrase(text) {
            Verdict::Resumes
        } else {
            Verdict::Ignored
        }
    } else if mic_on {
        Verdict::Heard
    } else {
        Verdict::Ignored
    }
}

/// "for a minute", "for 5 minutes", "für 2 Minuten", "for 30 seconds", "for an hour", "for half
/// an hour", "für eine halbe Stunde".
pub(crate) fn duration_in(s: &str) -> Option<Duration> {
    let w: Vec<&str> = s.split_whitespace().collect();
    for (i, unit) in w.iter().enumerate() {
        let secs = match *unit {
            "second" | "seconds" | "sekunde" | "sekunden" => 1,
            "minute" | "minutes" | "min" | "mins" | "minuten" => 60,
            "hour" | "hours" | "stunde" | "stunden" => 3600,
            _ => continue,
        };
        // ("Half an hour" has the "an" right before the hour.)
        if matches!(i.checked_sub(1).map(|k| w[k]), Some("halbe" | "halben")) || (matches!(i.checked_sub(1).map(|k| w[k]), Some("a" | "an")) && i.checked_sub(2).is_some_and(|k| w[k] == "half")) {
            return Some(Duration::from_secs((secs / 2).clamp(5, 24 * 3600)));
        }
        let n = match i.checked_sub(1).map(|k| w[k]) {
            Some("a" | "an" | "one" | "eine" | "einer" | "einen" | "1") => 1,
            Some("two" | "zwei" | "couple") => 2,
            Some("three" | "drei") => 3,
            Some("five" | "fünf" | "fuenf") => 5,
            Some("ten" | "zehn") => 10,
            Some("fifteen" | "fünfzehn") => 15,
            Some("twenty" | "zwanzig") => 20,
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
        "mute the mic",
        "mute your mic",
        "mute mic",
        "turn off the mic",
        "turn off my mic",
        "turn off your mic",
        "turn the mic off",
        "turn my mic off",
        "turn off the microphone",
        "turn off my microphone",
        "turn the microphone off",
        "turn my microphone off",
        "mikro aus",
        "mikrofon aus",
        "mikro stumm",
        "mikrofon stumm",
        "hör auf zuzuhören",
        "hoer auf zuzuhoeren",
        "hör auf zu zuhören",
        "hör nicht zu",
        "hoer nicht zu",
        "hör nicht mehr zu",
        "hoer nicht mehr zu",
        "hör mir nicht mehr zu",
        "hör mir nicht zu",
        "nicht mehr zuhören",
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
        assert_eq!(parse("JAI, stop listening for a minute"), Some(LockCommand::Pause(Duration::from_secs(60))));
        assert_eq!(parse("Stop listening for 5 minutes"), Some(LockCommand::Pause(Duration::from_secs(300))));
        assert_eq!(parse("Hör für 2 Minuten nicht zu"), Some(LockCommand::Pause(Duration::from_secs(120))));
        assert_eq!(parse("stop listening for thirty seconds"), Some(LockCommand::Pause(Duration::from_secs(30))));
        for lock in ["JAI, stop listening", "Mute my microphone", "Don't listen", "Hör auf zuzuhören"] {
            assert_eq!(parse(lock), Some(LockCommand::Lock), "{lock}");
        }
        // ("Start listening" once went on to the app launcher, with him still paused.)
        for resume in ["You can listen again", "JAI, you can now listen again", "Start listening again", "Start listening", "Keep listening", "Resume listening", "Unmute microphone", "Du kannst wieder zuhören", "Fang wieder an zuzuhören", "JAI hör wieder zu"] {
            assert_eq!(parse(resume), Some(LockCommand::Resume), "{resume}");
            assert!(is_resume_phrase(resume));
        }
        // The Discord bridge keeps its own phrase.
        assert_eq!(parse("stop listening to the call"), None);
        assert_eq!(parse("what's the weather"), None);
    }

    /// Repro (route reader, 2026-11-07): "unmute my mic" holds "mute my mic", so it locked him.
    #[test]
    fn the_microphone_goes_on_and_off_as_said() {
        for off in ["Mute my mic", "Mute the mic", "Mute the microphone", "Turn off the mic", "Turn the microphone off", "Mikrofon aus", "Mikro stumm"] {
            assert_eq!(parse(off), Some(LockCommand::Lock), "{off}");
        }
        for on in ["Unmute my mic", "Unmute the mic", "Unmute your microphone", "Turn on the mic", "Turn the microphone back on", "Mikrofon an", "Mikro wieder an"] {
            assert_eq!(parse(on), Some(LockCommand::Resume), "{on}");
            assert!(is_resume_phrase(on), "{on} must get through the lock");
        }
    }

    #[test]
    fn half_an_hour_is_half_an_hour() {
        assert_eq!(parse("Stop listening for half an hour"), Some(LockCommand::Pause(Duration::from_secs(1800))));
        assert_eq!(parse("Hör für eine halbe Stunde nicht zu"), Some(LockCommand::Pause(Duration::from_secs(1800))));
        assert_eq!(parse("Stop listening for an hour"), Some(LockCommand::Pause(Duration::from_secs(3600))));
        assert_eq!(parse("Stop listening for twenty minutes"), Some(LockCommand::Pause(Duration::from_secs(1200))));
    }

    #[test]
    fn while_locked_only_the_resume_phrase_gets_through() {
        let _g = test_lock();
        apply(LockCommand::Lock);
        assert!(is_locked());
        assert!(status_line().unwrap().contains("YOU CAN LISTEN AGAIN"));
        for ignored in ["JAI", "JAI, what time is it?", "JAI, stop listening"] {
            assert!(!is_resume_phrase(ignored), "{ignored} must be ignored");
        }
        assert!(is_resume_phrase("JAI, you can listen again"));
        // The microphone path's one decision: under the lock only the lifting words pass,
        // whatever the microphone's switch says.
        for mic_on in [true, false] {
            assert_eq!(verdict("JAI, what time is it?", mic_on), Verdict::Ignored);
            assert_eq!(verdict("JAI, you can listen again", mic_on), Verdict::Resumes);
        }
        apply(LockCommand::Resume);
        assert!(!is_locked() && status_line().is_none());
        // Unlocked: heard with the microphone on (or the push-to-talk key held); words that
        // were recorded just before it was switched off are dropped.
        assert_eq!(verdict("JAI", true), Verdict::Heard);
        assert_eq!(verdict("JAI", false), Verdict::Ignored);
        assert_eq!(verdict("JAI, you can listen again", false), Verdict::Ignored);
        // A timed pause runs out on its own; the status counts down.
        apply(LockCommand::Pause(Duration::from_millis(60)));
        assert!(matches!(state(), ListeningLockState::PausedUntil(_)));
        assert!(status_line().unwrap().starts_with("LISTENING PAUSED (0:00)"));
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(state(), ListeningLockState::Listening);
        assert_eq!(reply(LockCommand::Pause(Duration::from_secs(300))), "Understood, Sir. I'll stop listening for 5 minutes.");
    }
}
