//! Predictive command completion for the text console (Ctrl+K, or just start typing).
//!
//! What Sir has typed so far is matched against two sources, his own first: the requests he
//! sent recently (so the fourth "open my WebUntis schedule" is three keystrokes), then JAI's
//! command vocabulary — one sentence per thing he can do, English and German, every one of
//! them a sentence the routers really take (the test below sends each through them).
//!
//! Matching is by word prefixes in order ("op sch" finds "open my schedule"), so abbreviating
//! works; a line that begins the same way as the typed text ranks first, a recent request
//! before a vocabulary entry, a shorter completion before a longer one. Pure and synchronous:
//! the console asks again only when the line changes.

/// JAI's command vocabulary: what the palette offers when Sir's own history has nothing.
pub const VOCABULARY: &[&str] = &[
    // The board, the blocks and the search.
    "close the HUD board",
    "clear the board",
    "next page",
    "previous page",
    "maximize the chart",
    "walk me through it",
    "tidy the windows",
    "find voltage on the board",
    "clear the search",
    "pin the chart",
    "export the chart as JSON",
    "close the feature lab",
    // His own views moved and sized.
    "move it to the middle",
    "full screen the Earth",
    "put the earth on the left half",
    "move the radar to my second monitor",
    "put them side by side",
    "exit full screen",
    "minimise it",
    "restore it",
    // The look.
    "set theme to Arc Cyan",
    "set theme to Stark Gold",
    "make your theme red",
    "move your orb to the top right",
    "move your orb a little bit to the left",
    "show me your core",
    "close your core",
    "open the text console",
    "turn off the sound cues",
    "use all monitors",
    // Privacy.
    "stop listening",
    "stop listening for 5 minutes",
    "you can listen again",
    // Music and volume.
    "pause the music",
    "next song",
    "volume 40",
    "mute",
    "what's playing",
    // Schedule, mail and the day.
    "what classes do I have tomorrow",
    "open my WebUntis schedule",
    "what's on my calendar today",
    "any new emails",
    "brief me",
    "what time is it",
    // The workbench.
    "quiz me on my flashcards",
    "add a flashcard: mitosis means cell division into two identical cells",
    "how many flashcards are due",
    "show me the formula for kinetic energy",
    "what is the formula for the pq formula",
    "what is the molar mass of H2SO4",
    "how many moles are 36 grams of water",
    "show shortcuts for this app",
    "what is the shortcut for split screen",
    "how long have I been at the screen",
    "what did I use today",
    "what do I usually do now",
    "how are my temperatures",
    "show my clipboard history",
    "tile my windows side by side",
    "put my windows in a grid",
    // Research and the web.
    "research solid state batteries",
    "search the web for the Abitur dates in Baden-Württemberg",
    // System.
    "open my system stats",
    "diagnose my network",
    "open the feature lab",
    "what can you do",
    // The 3D studio.
    "render me an Iron Man suit in 3D",
    "show me a 3D model of the Earth",
    "open the 3D studio",
    // Deutsch.
    "mach das Theme blau",
    "nächste Seite",
    "schließ das HUD Board",
    "räum die Fenster auf",
    "frag mich meine Karteikarten ab",
    "zeig mir die Formel für die kinetische Energie",
    "wie lange sitze ich schon am Bildschirm",
    "wie sind meine Temperaturen",
    "zeig meinen Zwischenablage-Verlauf",
    "hör nicht mehr zu",
];

/// Lower-case words of a line (letters, digits and apostrophes).
fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).map(String::from).collect()
}

/// How well `candidate` completes the typed words: None when it does not; otherwise smaller is
/// better. Every typed word must begin a word of the candidate, in order.
fn score(typed: &[String], typed_line: &str, candidate: &str) -> Option<u32> {
    let lower = candidate.to_lowercase();
    if lower == typed_line {
        // Nothing left to complete.
        return None;
    }
    if lower.starts_with(typed_line) {
        return Some(0);
    }
    let have = words(candidate);
    let mut at = 0;
    let mut skipped = 0u32;
    for want in typed {
        let found = have[at..].iter().position(|w| w.starts_with(want.as_str()))?;
        skipped += found as u32;
        at += found + 1;
    }
    Some(10 + skipped)
}

/// Up to `limit` completions for what Sir has typed, best first. `recent`: his own requests,
/// newest first. Nothing for an empty line.
pub fn complete(typed: &str, recent: &[String], limit: usize) -> Vec<String> {
    let line = typed.trim().to_lowercase();
    let typed_words = words(&line);
    if typed_words.is_empty() || limit == 0 {
        return Vec::new();
    }
    // (score, source rank, order within the source, length, text)
    let mut ranked: Vec<(u32, u8, usize, usize, String)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let sources = recent.iter().map(|r| (0u8, r.trim())).chain(VOCABULARY.iter().map(|v| (1u8, *v)));
    for (order, (source, candidate)) in sources.enumerate() {
        // A pasted paragraph is not a command to offer again.
        if candidate.is_empty() || candidate.chars().count() > 90 {
            continue;
        }
        let key = candidate.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        if let Some(points) = score(&typed_words, &line, candidate) {
            seen.push(key);
            ranked.push((points, source, order, candidate.chars().count(), candidate.to_string()));
        }
    }
    ranked.sort_by(|a, b| (a.0, a.1, a.2, a.3).cmp(&(b.0, b.1, b.2, b.3)));
    ranked.into_iter().take(limit).map(|(_, _, _, _, text)| text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recent(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn what_is_typed_completes_from_sirs_own_requests_first() {
        let mine = recent(&["open my WebUntis schedule", "Set Sonora to 30 %", "open the feature lab"]);
        assert_eq!(complete("op", &mine, 3), ["open my WebUntis schedule", "open the feature lab", "open the text console"], "his own, newest first, then the vocabulary");
        // The vocabulary's copy of his own request is not offered twice.
        assert_eq!(complete("open my web", &mine, 4), ["open my WebUntis schedule"]);
        // Word prefixes in order: abbreviations work.
        assert_eq!(complete("tu so cu", &[], 2), ["turn off the sound cues"]);
        assert_eq!(complete("mo orb top", &[], 1), ["move your orb to the top right"]);
        // German completes too.
        assert_eq!(complete("näch", &[], 1), ["nächste Seite"]);
        assert_eq!(complete("frag mich", &[], 1), ["frag mich meine Karteikarten ab"]);
    }

    #[test]
    fn nothing_is_offered_for_an_empty_line_a_finished_line_or_a_stranger() {
        assert!(complete("", &[], 4).is_empty() && complete("   ", &[], 4).is_empty());
        assert!(complete("next page", &[], 4).is_empty(), "nothing left to complete");
        assert!(complete("xylophone quartz", &[], 4).is_empty());
        assert!(complete("open", &[], 0).is_empty());
        // A pasted paragraph in the history is never offered.
        let pasted = recent(&[&"open ".repeat(40)]);
        assert!(!complete("open", &pasted, 8).iter().any(|c| c.len() > 90));
        // The limit holds.
        assert_eq!(complete("s", &[], 3).len(), 3);
    }

    #[test]
    fn a_line_that_starts_as_typed_beats_one_that_only_carries_the_words() {
        let got = complete("clear", &[], 3);
        assert_eq!(got[0], "clear the board");
        assert!(got.contains(&"clear the search".to_string()), "{got:?}");
        // Typing more narrows it down.
        assert_eq!(complete("clear the s", &[], 3), ["clear the search"]);
    }

    #[test]
    fn the_vocabulary_has_no_duplicates_and_stays_short_enough_for_a_key() {
        let mut seen = std::collections::HashSet::new();
        for line in VOCABULARY {
            assert!(seen.insert(line.to_lowercase()), "twice: {line}");
            assert!(line.chars().count() <= 90 && !line.trim().is_empty(), "{line}");
            assert!(!line.chars().any(crate::live_self_tester::is_pictograph), "{line}");
        }
        assert!(VOCABULARY.len() >= 60);
    }
}
