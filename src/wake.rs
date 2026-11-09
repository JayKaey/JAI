//! Fuzzy, phonetic wake-word detection on Whisper transcripts.
//!
//! Accepts mumbled/German-accented variants ("Javis", "Schawis", "Dscharvis", "Jarviz",
//! "Hey Jarvis", split tokens like "Jar vis") at the start of an utterance (after optional
//! greetings) or as a trailing vocative ("Mach Spotify auf, Jarvis").

use strsim::jaro_winkler;

#[derive(Debug, Clone, PartialEq)]
pub struct WakeMatch {
    pub matched: bool,
    pub score: f64,
    /// The transcript text that matched (e.g. "Javis").
    pub word: String,
    /// The command with the wake word (and leading greeting) removed.
    pub command: String,
}

const GREETINGS: &[&str] = &[
    "hey", "hi", "hallo", "hello", "ok", "okay", "yo", "na", "also", "äh", "ähm", "hm", "oh", "so", "and",
    "und", "guten", "morgen", "abend", "tag", "good", "morning", "evening", "bitte", "please", "ey", "he",
];

pub struct WakeWordMatcher {
    variants: Vec<(String, String)>,
    threshold: f64,
}

impl WakeWordMatcher {
    pub fn new(variants: &[String], threshold: f64) -> Self {
        let mut list: Vec<(String, String)> = variants
            .iter()
            .map(|v| clean(v))
            .filter(|v| !v.is_empty())
            .map(|v| {
                let p = phonetic(&v);
                (v, p)
            })
            .collect();
        if list.is_empty() {
            list.push(("jarvis".into(), phonetic("jarvis")));
        }
        Self { variants: list, threshold }
    }

    /// Jaro-Winkler on the phonetic forms, gated by length: raw-spelling similarity is too
    /// generous with long shared prefixes ("Charles" vs "charvis"), and very different lengths
    /// ("JavaScript", "Jahr") are never the wake word.
    fn score(&self, token: &str) -> f64 {
        if token.chars().count() < 4 {
            return 0.0;
        }
        let p = phonetic(token);
        let plen = p.chars().count() as i64;
        // Four letters ("Jays", "Jars") share too much with "Javis" by chance: they need a much
        // closer match than a full-length mumble does.
        let short = token.chars().count() <= 4;
        self.variants
            .iter()
            .map(|(raw, ph)| {
                if token == raw {
                    1.0
                } else if (plen - ph.chars().count() as i64).abs() > 2 {
                    0.0
                } else if ph.contains('v') && !p.contains('v') {
                    // Every way of saying "Jarvis" keeps its v ("Javis", "Schawis", "Jervis");
                    // "Schließ dich", "Jobs" or "Jules" have none.
                    0.0
                } else {
                    let s = jaro_winkler(&p, ph);
                    if short && s < 0.93 { 0.0 } else { s }
                }
            })
            .fold(0.0, f64::max)
    }

    pub fn evaluate(&self, transcript: &str) -> WakeMatch {
        let words: Vec<&str> = transcript.split_whitespace().collect();
        let cleaned: Vec<String> = words.iter().map(|w| clean(w)).collect();
        let none = |score: f64| WakeMatch {
            matched: false,
            score,
            word: String::new(),
            command: transcript.trim().to_string(),
        };
        if words.is_empty() {
            return none(0.0);
        }

        // Skip up to three leading greetings/fillers ("Hey", "Okay so", "Guten Morgen").
        let mut start = 0;
        while start < words.len().min(3) && GREETINGS.contains(&cleaned[start].as_str()) {
            start += 1;
        }

        let mut best = (0.0f64, 0usize, 0usize); // (score, first index, token count)
        let consider = |i: usize, n: usize, best: &mut (f64, usize, usize)| {
            if i + n > words.len() {
                return;
            }
            let joined: String = cleaned[i..i + n].concat();
            let s = self.score(&joined);
            if s > best.0 {
                *best = (s, i, n);
            }
        };
        for i in start..(start + 2).min(words.len()) {
            consider(i, 1, &mut best);
            consider(i, 2, &mut best); // "Jar vis", "Dschar wis"
        }
        let last = words.len() - 1;
        if last >= start + 2 {
            consider(last, 1, &mut best);
            if last >= 1 {
                consider(last - 1, 2, &mut best);
            }
        }

        let is_punct = |c: char| c.is_whitespace() || matches!(c, ',' | '.' | ':' | ';' | '-' | '!' | '?');
        let (score, idx, n) = best;
        if score < self.threshold {
            // "Thank you Jarvis, that's all for now": the name as a vocative in the middle — set
            // off by punctuation or in a sentence that addresses someone, and spelled closely.
            let addresses = cleaned.iter().any(|w| matches!(w.as_str(), "you" | "your" | "yourself" | "please" | "du" | "dich" | "dir" | "bitte" | "thanks" | "danke"));
            let vocative = (start..words.len()).find(|&i| {
                let set_off = words[i].ends_with([',', '.', '!', '?', ':']) || (i > 0 && words[i - 1].ends_with([',', '.', '!', '?', ':']));
                self.score(&cleaned[i]) >= self.threshold.max(0.92) && (set_off || addresses)
            });
            let Some(i) = vocative else { return none(score) };
            let head = words[..i].join(" ");
            let tail = words[i + 1..].join(" ");
            let head = head.trim_end_matches(is_punct);
            let tail = tail.trim_start_matches(is_punct);
            let command = match (head.is_empty(), tail.is_empty()) {
                (true, _) => tail.to_string(),
                (false, true) => head.to_string(),
                (false, false) => format!("{head}, {tail}"),
            };
            return WakeMatch {
                matched: true,
                score: self.score(&cleaned[i]),
                word: words[i].trim_matches(|c: char| !c.is_alphanumeric()).to_string(),
                command: command.trim_end_matches(|c: char| c.is_whitespace() || c == '.' || c == '!').to_string(),
            };
        }
        let word = words[idx..idx + n].join(" ");
        let at_end = idx + n == words.len() && idx > start;
        // "Switch back to Jarvis", "zurück zu Jarvis": the name is the object, not a vocative.
        let object = at_end && matches!(cleaned[idx - 1].as_str(), "to" | "zu" | "as" | "into" | "als") && !words[idx - 1].ends_with([',', ';', ':']);
        let command = if object {
            words[start..].join(" ").trim_end_matches(|c: char| c.is_whitespace() || c == '.' || c == '!').to_string()
        } else if at_end {
            // "What time is it, Jarvis?" → "What time is it?"
            let head = words[start..idx].join(" ");
            let head = head.trim_end_matches(is_punct).to_string();
            if !head.is_empty() && transcript.trim_end().ends_with('?') {
                format!("{head}?")
            } else {
                head
            }
        } else {
            // Keep a trailing '?' (questions read differently), drop '.'/'!'. With his name as
            // the subject the recogniser writes an order as a report ("Jarvis opened the mixer").
            let rest = words[idx + n..].join(" ");
            let rest = rest.trim_start_matches(is_punct).trim_end_matches(|c: char| c.is_whitespace() || c == '.' || c == '!');
            crate::typos::imperative_after_name(rest)
        };
        WakeMatch {
            matched: true,
            score,
            word: word.trim_matches(|c: char| !c.is_alphanumeric()).to_string(),
            command,
        }
    }
}

fn clean(word: &str) -> String {
    word.trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Collapses German/English spellings of the same sounds: dsch/tsch/sch/ch/sh/zh → j,
/// w → v, ph → f, z/ß → s, non-rhotic "ar" → "a", doubled letters → single.
pub fn phonetic(word: &str) -> String {
    let mut s = clean(word);
    for (from, to) in [
        ("dsch", "j"), ("tsch", "j"), ("sch", "j"), ("ch", "j"), ("sh", "j"), ("zh", "j"), ("dj", "j"),
        ("ph", "f"), ("ß", "s"), ("z", "s"), ("w", "v"), ("ä", "e"), ("ö", "o"), ("ü", "u"), ("ie", "i"),
        ("ee", "i"), ("ah", "a"), ("ar", "a"), ("er", "a"), ("ur", "a"),
    ] {
        s = s.replace(from, to);
    }
    if let Some(rest) = s.strip_prefix('y') {
        s = format!("j{rest}");
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if !out.ends_with(c) {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher() -> WakeWordMatcher {
        let variants: Vec<String> = ["jarvis", "javis", "charvis", "schawis", "jarviz", "dscharvis", "yarvis"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        WakeWordMatcher::new(&variants, 0.80)
    }

    #[test]
    fn mumbled_variants_trigger() {
        let m = matcher();
        for (input, command) in [
            ("Jarvis, open Spotify.", "open Spotify"),
            ("Hey Jarvis, wie spät ist es?", "wie spät ist es?"),
            ("Javis mach Musik an", "mach Musik an"),
            ("Schawis, öffne Discord", "öffne Discord"),
            ("Charvis what's the weather", "what's the weather"),
            ("Jarviz. Lautstärke auf 50", "Lautstärke auf 50"),
            ("Okay Dscharvis starte Steam", "starte Steam"),
            ("Hallo Jervis, how are you?", "how are you?"),
            ("Dschar wis, mach das Licht aus", "mach das Licht aus"),
            ("Mach mal Spotify auf, Jarvis.", "Mach mal Spotify auf"),
            ("What time is it, Jarvis?", "What time is it?"),
            ("Jarvis.", ""),
            // His name as the subject: the order written as a report (verifier, 2026-11-07).
            ("Jarvis opened the mixer.", "open the mixer"),
            ("Jarvis stops spanning.", "stop spanning"),
            ("Jarvis saved this mix as gaming.", "save this mix as gaming"),
        ] {
            let r = m.evaluate(input);
            assert!(r.matched, "should match: {input} (score {:.2})", r.score);
            assert_eq!(r.command, command, "command for {input}");
        }
    }

    #[test]
    fn german_words_without_the_v_are_never_the_name() {
        let m = matcher();
        for input in ["Schließ dich selbst.", "Schließe dich.", "Jobs, Jobs", "Schalt das aus"] {
            assert!(!m.evaluate(input).matched, "should not match: {input} ({:.2})", m.evaluate(input).score);
        }
        assert!(m.evaluate("Jervis, open Spotify").matched);
        assert!(m.evaluate("Schawis, öffne Discord").matched);
    }

    #[test]
    fn short_soundalikes_do_not_trigger() {
        let m = matcher();
        for input in ["Jays rock is my playlist", "Play Jays rock", "Jars of honey", "Chase rock again"] {
            assert!(!m.evaluate(input).matched, "should not match: {input} ({:.2})", m.evaluate(input).score);
        }
        let r = m.evaluate("I want you to turn the music down Jarvis");
        assert!(r.matched, "the name at the very end");
        assert_eq!(r.command, "I want you to turn the music down");
        // The screenshot bug: "… switch back to Jarvis." lost its last word ("… back to").
        for (input, command) in [("Change back to Jarvis.", "Change back to Jarvis"), ("Wechsel zurück zu Jarvis", "Wechsel zurück zu Jarvis")] {
            let r = m.evaluate(input);
            assert!(r.matched, "{input}");
            assert_eq!(r.command, command, "{input}");
        }
        assert_eq!(m.evaluate("Play it again, Jarvis.").command, "Play it again");
    }

    #[test]
    fn the_name_as_a_vocative_mid_sentence_triggers() {
        let m = matcher();
        let r = m.evaluate("Thank you Jarvis, that's all for now, can you please close yourself without saying goodbye?");
        assert!(r.matched);
        assert_eq!(r.command, "Thank you, that's all for now, can you please close yourself without saying goodbye?");
        let r = m.evaluate("Okay so, Jarvis, what's next?");
        assert!(r.matched);
        assert!(!m.evaluate("I told my friend about Charles yesterday").matched);
        assert!(!m.evaluate("My brother watches Jarvis clips all day").matched, "a mention without address");
    }

    #[test]
    fn ordinary_speech_does_not_trigger() {
        let m = matcher();
        for input in [
            "Ich gehe jetzt einkaufen.",
            "What are you doing tonight?",
            "Das ist ein Service für alle.",
            "Open the JavaScript file please",
            "Hast du die Nachrichten gesehen?",
            "Charles is coming over later",
        ] {
            let r = m.evaluate(input);
            assert!(!r.matched, "should NOT match: {input} (score {:.2}, word {})", r.score, r.word);
        }
    }

    #[test]
    fn phonetic_normalisation() {
        assert_eq!(phonetic("Schawis"), phonetic("Jarvis"));
        assert_eq!(phonetic("Dscharvis"), phonetic("jarvis"));
        assert_eq!(phonetic("Yarvis"), phonetic("jarvis"));
        assert_eq!(phonetic("Jarviz"), phonetic("jarvis"));
    }
}
