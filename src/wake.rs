//! His name in what the recogniser wrote: "Jay", "J.A.I.", "JAI", said or spelt a little
//! wrong ("Jai", "Jae", "Dschey", "J A I", "Jay A.I."), at the start of a sentence (after a
//! greeting), as a vocative at its end ("Mach Spotify auf, Jay") or set off in the middle.
//!
//! A name of one syllable sits close to ordinary words and to other people's names, so three
//! things keep him from answering to them:
//! * Sir's own handle is not his name: "JayKaey", "Jay Kay", "JK" never wake him (nor "Jay-Z");
//! * without a pause after it ("Jay said he'd come") the name counts only when what follows
//!   is plainly for him: an order, a question to him, something the fast path knows;
//! * near misses ("Jake", "Jade", "Chay", a lone "J") count only in front of one of his
//!   commands.

use strsim::jaro_winkler;

#[derive(Debug, Clone, PartialEq)]
pub struct WakeMatch {
    pub matched: bool,
    pub score: f64,
    /// The transcript text that matched (e.g. "Jay").
    pub word: String,
    /// The command with the wake word (and leading greeting) removed.
    pub command: String,
}

/// His name as it is written wherever text is his to read (prompts, lists, the fast path).
pub const NAME: &str = "JAI";

const GREETINGS: &[&str] = &[
    "hey", "hi", "hallo", "hello", "ok", "okay", "yo", "na", "also", "äh", "ähm", "hm", "oh", "so", "and",
    "und", "guten", "morgen", "abend", "tag", "good", "morning", "evening", "bitte", "please", "ey", "he",
];
/// What the name sounds like once spellings are collapsed (`phonetic`).
const SOUNDS: &[&str] = &["jai", "jay", "jae", "jaye", "jaey", "jey", "jei"];
/// Other names and a lone letter the recogniser writes for it: his only in front of a command.
const NEAR: &[&str] = &["j", "jake", "jade", "jane", "jace", "jayce", "jase", "jayne", "jaya"];
/// The second half of Sir's handle (JayKaey): with it, "Jay" is Sir, not him.
const HANDLE_TAIL: &[&str] = &["kay", "k", "kaey", "key", "kai", "kei", "kae", "kaye", "que", "cay", "ke"];
/// Sir's handle and other names in one word.
const NOT_HIM: &[&str] = &["jaykaey", "jaykay", "jaykey", "jaykai", "jaykae", "jaykei", "jaykaye", "jayk", "jk", "jakey", "jkay", "jayz", "jayzee"];
/// Words the name is the object of ("play something for Jay", "call Jay"): no vocative.
const OBJECT_BEFORE: &[&str] = &[
    "for", "with", "by", "from", "of", "about", "call", "text", "message", "tell", "ask", "email", "mail", "named", "called", "like", "is", "am", "was", "and", "or", "friend", "brother",
    "the", "my", "a", "mr", "dj", "uncle", "cousin", "meet", "meeting", "invite", "für", "mit", "von", "über", "und", "oder", "heißt", "freund", "bruder", "mein", "der", "dem", "den",
];
/// "Switch back to JAI", "zurück zu JAI", "close JAI", "restart JAI": the name is the order's
/// object and stays in it.
const NAMED_IN_ORDER: &[&str] = &[
    "to", "zu", "as", "into", "als", "close", "quit", "exit", "terminate", "end", "kill", "restart", "reboot", "reload", "shutdown", "beende", "beend", "schließ", "schließe", "schliess", "schliesse",
    "starte",
];

pub struct WakeWordMatcher {
    /// Further names from the settings (cleaned, with their sound).
    extra: Vec<(String, String)>,
    /// Soft names (written "~they" in the settings): everyday words the recogniser writes for
    /// his name in Sir's voice. His name only as the first word of a short order he knows.
    soft: Vec<String>,
    threshold: f64,
}

/// A soft name counts in an utterance of this many words at most.
const SOFT_WORDS: usize = 10;

/// How surely a word is his name.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Form {
    None,
    /// Only in front of one of his commands.
    Near,
    Sure,
}

impl WakeWordMatcher {
    pub fn new(variants: &[String], threshold: f64) -> Self {
        let soft: Vec<String> = variants.iter().filter_map(|v| v.trim().strip_prefix('~')).map(clean).filter(|v| !v.is_empty()).collect();
        let extra = variants.iter().filter(|v| !v.trim().starts_with('~')).map(|v| clean(v)).filter(|v| !v.is_empty() && !SOUNDS.contains(&v.as_str())).map(|v| { let p = phonetic(&v); (v, p) }).collect();
        Self { extra, soft, threshold }
    }

    /// One word (cleaned) as his name, and how close it is.
    fn form(&self, token: &str) -> (Form, f64) {
        if token.is_empty() || NOT_HIM.contains(&token) {
            return (Form::None, 0.0);
        }
        // (A soft name: like the near ones, only in front of an order he knows; `locate` asks
        // more of it.)
        if self.soft.iter().any(|s| s == token) {
            return (Form::Near, 0.8);
        }
        let p = phonetic(token);
        let j_spelt = token.starts_with('j') || token.starts_with("dsch") || token.starts_with("dj");
        if SOUNDS.contains(&p.as_str()) {
            // "Chay", "Shay", "Tschey" sound alike but are written as other words.
            return if j_spelt { (Form::Sure, 1.0) } else if token.starts_with('y') { (Form::None, 0.0) } else { (Form::Near, 0.8) };
        }
        if NEAR.contains(&token) {
            return (Form::Near, 0.8);
        }
        // Names added in the settings: exactly, or (longer ones) mumbled.
        let best = self
            .extra
            .iter()
            .map(|(raw, ph)| {
                if token == raw {
                    1.0
                } else if raw.chars().count() < 5 || (p.chars().count() as i64 - ph.chars().count() as i64).abs() > 2 {
                    0.0
                } else {
                    jaro_winkler(&p, ph)
                }
            })
            .fold(0.0, f64::max);
        if best >= self.threshold { (Form::Sure, best) } else { (Form::None, best) }
    }

    /// The name starting at word `i`: how many words it takes ("J A I", "Jay A.I."), its form
    /// and score. Sir's handle ("Jay Kay") and "Jay Z" are no name of his.
    fn name_at(&self, cleaned: &[String], i: usize) -> Option<(usize, Form, f64)> {
        let at = |k: usize| cleaned.get(i + k).map(String::as_str).unwrap_or("");
        let first = at(0);
        // Spelt out: "J A I", "Jay A I", "J AI", "Jay ay eye".
        let spelt = if matches!(first, "j" | "jay") {
            if matches!(at(1), "a" | "ay") && matches!(at(2), "i" | "eye") {
                3
            } else if at(1) == "ai" {
                2
            } else {
                0
            }
        } else {
            0
        };
        if spelt > 0 {
            return Some((spelt, Form::Sure, 0.95));
        }
        let (form, score) = self.form(first);
        if form == Form::None {
            return None;
        }
        let next = at(1);
        if HANDLE_TAIL.contains(&next) || matches!(next, "z" | "zee" | "zed") {
            return None;
        }
        Some((1, form, score))
    }

    pub fn evaluate(&self, transcript: &str) -> WakeMatch {
        self.locate(transcript).0
    }

    /// The match, and whether the name is the order's own object ("switch back to JAI").
    fn locate(&self, transcript: &str) -> (WakeMatch, bool) {
        // "Jaymute,", "JaySystem status.": his name and the order written as one word.
        let unglued = unglue(transcript);
        let transcript = unglued.as_deref().unwrap_or(transcript);
        let words: Vec<&str> = transcript.split_whitespace().collect();
        let cleaned: Vec<String> = words.iter().map(|w| clean(w)).collect();
        let none = |score: f64| WakeMatch { matched: false, score, word: String::new(), command: transcript.trim().to_string() };
        if words.is_empty() {
            return (none(0.0), false);
        }
        let is_punct = |c: char| c.is_whitespace() || matches!(c, ',' | '.' | ':' | ';' | '-' | '!' | '?');
        let set_off_after = |i: usize| words[i].ends_with([',', '.', '!', '?', ':', ';']);
        // A possessive is a mention ("Jay's playlist").
        let possessive = |i: usize| words[i].trim_end_matches(|c: char| !c.is_alphanumeric()).to_lowercase().ends_with("'s") || words[i].to_lowercase().contains("’s");
        let found = |i: usize, n: usize, score: f64, command: String| WakeMatch {
            matched: true,
            score,
            word: words[i..i + n].join(" ").trim_matches(|c: char| !c.is_alphanumeric()).to_string(),
            command,
        };

        // Skip up to three leading greetings/fillers ("Hey", "Okay so", "Guten Morgen").
        let mut start = 0;
        while start < words.len().min(3) && GREETINGS.contains(&cleaned[start].as_str()) {
            start += 1;
        }
        let mut best = 0.0f64;

        // In front: "Jay, open Spotify", "Hey JAI what time is it", and one stray word before a
        // name that is set off ("In Jay, can you show me my storage?").
        for i in start..(start + 2).min(words.len()) {
            let Some((n, form, score)) = self.name_at(&cleaned, i).filter(|_| !possessive(i)) else { continue };
            // (A soft name is the very first word of something short, or it is the word it is.)
            if self.soft.contains(&cleaned[i]) && (i != start || words.len() - start > SOFT_WORDS) {
                continue;
            }
            best = best.max(score);
            let last = i + n - 1;
            let paused = set_off_after(last);
            // Keep a trailing '?' (questions read differently), drop '.'/'!'. With his name as
            // the subject the recogniser writes an order as a report ("Jay opened the mixer").
            let rest = words[last + 1..].join(" ");
            let rest = rest.trim_start_matches(is_punct).trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '!' | ',' | ';' | ':'));
            // (A stray word counts only before a name that is set off and followed by the order:
            // "close JAI." names him as its object.)
            if i > start && !(paused && form == Form::Sure && !rest.is_empty()) {
                continue;
            }
            let command = crate::typos::imperative_after_name(rest);
            let his = match form {
                Form::Sure => command.is_empty() || paused || for_him(&command),
                _ => !command.is_empty() && his_order(&command),
            };
            if his {
                return (found(i, n, score, command), false);
            }
        }

        // At the end: "What time is it, Jay?", "turn the music down Jay".
        let last = words.len() - 1;
        if last >= start + 1 {
            for n in [3usize, 2, 1] {
                if n > last - start {
                    continue;
                }
                let i = last + 1 - n;
                let Some((len, form, score)) = self.name_at(&cleaned, i) else { continue };
                if len != n || form != Form::Sure || possessive(i) {
                    continue;
                }
                best = best.max(score);
                let before = cleaned[i - 1].as_str();
                let comma = words[i - 1].ends_with([',', ';', ':']);
                let switched_off = i >= 2 && matches!(before, "down" | "off") && matches!(cleaned[i - 2].as_str(), "shut" | "turn" | "switch" | "power");
                if (NAMED_IN_ORDER.contains(&before) || switched_off) && !comma {
                    // "Switch back to JAI": the order names him and keeps its last word.
                    let whole = words[start..].join(" ");
                    return (found(i, n, score, whole.trim_end_matches(|c: char| c.is_whitespace() || c == '.' || c == '!').to_string()), true);
                }
                let head = words[start..i].join(" ");
                let head = head.trim_end_matches(is_punct).to_string();
                if head.is_empty() || (!comma && (OBJECT_BEFORE.contains(&before) || !for_him(&head))) {
                    continue;
                }
                // "What time is it, Jay?" → "What time is it?"
                let command = if transcript.trim_end().ends_with('?') { format!("{head}?") } else { head };
                return (found(i, n, score, command), false);
            }
        }

        // In the middle, set off: "Thank you Jay, that's all for now".
        let addresses = cleaned.iter().any(|w| matches!(w.as_str(), "you" | "your" | "yourself" | "please" | "du" | "dich" | "dir" | "bitte" | "thanks" | "danke"));
        let vocative = (start..words.len()).find(|&i| {
            let set_off = set_off_after(i) || (i > 0 && words[i - 1].ends_with([',', '.', '!', '?', ':']));
            let before = if i > 0 { cleaned[i - 1].as_str() } else { "" };
            matches!(self.name_at(&cleaned, i), Some((1, Form::Sure, _))) && !possessive(i) && set_off && (addresses || set_off_after(i)) && !OBJECT_BEFORE.contains(&before)
        });
        let Some(i) = vocative else { return (none(best), false) };
        let head = words[..i].join(" ");
        let tail = words[i + 1..].join(" ");
        let head = head.trim_end_matches(is_punct);
        let tail = tail.trim_start_matches(is_punct);
        let command = match (head.is_empty(), tail.is_empty()) {
            (true, _) => tail.to_string(),
            (false, true) => head.to_string(),
            (false, false) => format!("{head}, {tail}"),
        };
        (found(i, 1, 1.0, command.trim_end_matches(|c: char| c.is_whitespace() || c == '.' || c == '!').to_string()), false)
    }
}

/// What follows the name without a pause is plainly for him: an order, a question to him, a
/// polite request, or anything the fast path knows. ("Jay said he'd come", "Jay is late" are
/// about someone.)
fn for_him(rest: &str) -> bool {
    let plain: Vec<String> = rest.split_whitespace().map(clean).filter(|w| !w.is_empty()).collect();
    let polite = plain.first().is_some_and(|w| matches!(w.as_str(), "please" | "bitte"));
    let from = plain.iter().position(|w| !matches!(w.as_str(), "please" | "bitte" | "hey" | "ok" | "okay" | "now" | "so")).unwrap_or(plain.len());
    let Some(first) = plain.get(from).map(String::as_str) else { return polite };
    let second = plain.get(from + 1).map(String::as_str).unwrap_or("");
    const ASKS: &[&str] = &["what", "whats", "when", "where", "wheres", "who", "whos", "why", "which", "how", "hows", "wie", "wann", "wo", "was", "wer", "warum", "welche", "welcher", "welches", "wieviel"];
    polite
        || (first != "just" && crate::ears::COMMAND_VERBS.contains(&first))
        || ASKS.contains(&first)
        || matches!((first, second), ("can" | "could" | "would" | "will" | "do" | "are" | "have" | "did" | "should" | "may", "you" | "i" | "we") | ("kannst" | "könntest" | "würdest" | "hast" | "bist" | "machst", "du") | ("i", "need" | "want") | ("id", "like") | ("ich", "brauche" | "will" | "möchte"))
        || his_order(rest)
}

/// "Jaymute," → "Jay mute,": his name written together with the first word of an order, among
/// the first words of what was heard (the recogniser does that with a one-syllable name said
/// without a pause: "JaySystem status.", "J-Pause,", "Jmute"). Only in front of an order he
/// knows: "JayKaey" (Sir's handle), "Jaywalking", "Jaipur", "JList" and "Just open …" stay as
/// they are.
fn unglue(transcript: &str) -> Option<String> {
    // His name as the word begins, longest first; a bare "J" needs more behind it (below).
    const WRITTEN: &[&str] = &["jaey", "jay", "jai", "j"];
    let words: Vec<&str> = transcript.split_whitespace().collect();
    for (i, word) in words.iter().enumerate().take(3) {
        let lead = word.len() - word.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
        let core = word[lead..].trim_end_matches(|c: char| !c.is_alphanumeric());
        let (lower, whole) = (core.to_lowercase(), clean(word));
        // A word that is a name by itself ("Jay", "Jake", Sir's handle) is not two words.
        if NOT_HIM.contains(&whole.as_str()) || NEAR.contains(&whole.as_str()) || SOUNDS.contains(&phonetic(&whole).as_str()) {
            continue;
        }
        // (ASCII on both sides: the cut is at the same byte in the word as it was written.)
        let Some(name) = WRITTEN.iter().find(|n| lower.starts_with(**n) && core.is_char_boundary(n.len())) else { continue };
        let hyphen = core[name.len()..].starts_with('-');
        let cut = lead + name.len() + usize::from(hyphen);
        let tail = clean(&word[cut..]);
        if tail.chars().count() < 3 || HANDLE_TAIL.contains(&tail.as_str()) || !tail.starts_with(|c: char| c.is_alphabetic()) {
            continue;
        }
        let verb = tail != "just" && crate::ears::COMMAND_VERBS.contains(&tail.as_str());
        // The order as it goes on after his name (the word keeps its own punctuation).
        let order = std::iter::once(&word[cut..]).chain(words[i + 1..].iter().copied()).collect::<Vec<_>>().join(" ");
        let known = || his_order(order.trim_end_matches([',', ';', ':', '.', '!']));
        let his = match (*name, hyphen) {
            // "Jmute": one of his verbs, and the whole of it an order ("JList is a Swing class").
            ("j", false) => verb && known(),
            // "J-System status.": the recogniser set the name off itself.
            ("j", true) => known(),
            _ => verb || (tail.chars().count() >= 4 && known()),
        };
        if his {
            let before = words[..i].join(" ");
            return Some(format!("{before} {}Jay {order}", &word[..lead]).trim().to_string());
        }
    }
    None
}

/// An order he knows, as it was heard or as the dispatcher will read it: one the recogniser
/// misheard ("Tugglespan Mode", "said this called the 80%", "Watts Open") is still an order.
fn his_order(text: &str) -> bool {
    crate::ears::known_command(text) || crate::ears::known_command(&crate::typos::normalize_user_input_typos(text))
}

/// What was heard with his name as it is written for the parsers: "Jay, wake up" and "wake up,
/// J.A.I." become "JAI, wake up" (lists of fixed phrases know that one spelling). Anything not
/// said to him by name stays as it is.
pub fn canon(text: &str) -> String {
    static BUILT_IN: std::sync::LazyLock<WakeWordMatcher> = std::sync::LazyLock::new(|| WakeWordMatcher::new(&[], 0.8));
    // Spelt with its stops it is one word wherever it stands ("close J.A.I." → "close JAI").
    static SPELT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)\bJ\.\s?A\.\s?I\b\.?").expect("the spelt name"));
    let text = SPELT.replace_all(text, |c: &regex::Captures| if c[0].ends_with('.') && text[c.get(0).map_or(0, |m| m.end())..].trim().is_empty() { format!("{NAME}.") } else { NAME.to_string() });
    let text = text.as_ref();
    let (m, named_in_order) = BUILT_IN.locate(text);
    if !m.matched {
        return text.to_string();
    }
    // An order that names him keeps its words ("switch back to Jay"): only the spelling changes.
    if named_in_order {
        let mut words: Vec<&str> = m.command.split_whitespace().collect();
        words.truncate(words.len().saturating_sub(m.word.split_whitespace().count().max(1)));
        return format!("{} {NAME}", words.join(" ")).trim().to_string();
    }
    if m.command.is_empty() { NAME.to_string() } else { format!("{NAME}, {}", m.command) }
}

fn clean(word: &str) -> String {
    word.trim().to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
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

    #[test]
    fn a_soft_name_is_his_only_at_the_start_of_a_short_order_he_knows() {
        let m = WakeWordMatcher::new(&["~they".to_string(), "~day".to_string()], 0.8);
        let hit = m.evaluate("They open the mixer.");
        assert!(hit.matched, "an order he knows after the soft name");
        assert_eq!(hit.command.to_lowercase(), "open the mixer");
        assert!(m.evaluate("Day what time is it?").matched);
        // Not an order; not the first word; a long sentence; and without the set-up, never.
        assert!(!m.evaluate("They said the film was better than the book.").matched);
        assert!(!m.evaluate("I think they open the mixer.").matched);
        assert!(!m.evaluate("They open the mixer and then they all went home because it was late already.").matched);
        assert!(!WakeWordMatcher::new(&[], 0.8).evaluate("They open the mixer.").matched);
        // His real name is as sure as ever.
        assert!(m.evaluate("Jay, open the mixer.").matched);
    }

    fn matcher() -> WakeWordMatcher {
        let variants: Vec<String> = ["jai", "jay"].iter().map(|s| s.to_string()).collect();
        WakeWordMatcher::new(&variants, 0.80)
    }

    #[test]
    fn his_name_said_or_spelt_a_little_wrong_wakes_him() {
        let m = matcher();
        for (input, command) in [
            ("Jay, open Spotify.", "open Spotify"),
            ("JAI, open Spotify.", "open Spotify"),
            ("J.A.I., open Spotify.", "open Spotify"),
            ("Jai open Spotify", "open Spotify"),
            ("Jae, what's the weather?", "what's the weather?"),
            ("Hey Jay, wie spät ist es?", "wie spät ist es?"),
            ("Jay mach Musik an", "mach Musik an"),
            ("Dschey, öffne Discord", "öffne Discord"),
            ("Jey. Lautstärke auf 50", "Lautstärke auf 50"),
            ("Okay Jay starte Steam", "starte Steam"),
            ("J A I, how are you?", "how are you?"),
            ("Jay A.I., mach das Licht aus", "mach das Licht aus"),
            ("Jay ay eye, close the board", "close the board"),
            ("Mach mal Spotify auf, Jay.", "Mach mal Spotify auf"),
            ("What time is it, Jay?", "What time is it?"),
            ("Jay.", ""),
            ("J.A.I.", ""),
            ("Hey Jay", ""),
            // Whisper's stray word before the name (2026-11-09: "In JAI, can you show me …").
            ("In Jay, can you show me my storage?", "can you show me my storage?"),
            ("And Jay, can you clear my recycle bin?", "can you clear my recycle bin?"),
            // His name as the subject: the order written as a report (verifier, 2026-11-07).
            ("Jay opened the mixer.", "open the mixer"),
            ("JAI stops spanning.", "stop spanning"),
            ("Jay saved this mix as gaming.", "save this mix as gaming"),
            ("Jay can you make the Earth spin?", "can you make the Earth spin?"),
            ("Jay please be quiet", "please be quiet"),
        ] {
            let r = m.evaluate(input);
            assert!(r.matched, "should match: {input} (score {:.2})", r.score);
            assert_eq!(r.command, command, "command for {input}");
        }
        assert_eq!(m.evaluate("Jay A.I., mach das Licht aus").word, "Jay A.I");
    }

    /// Sir's handle on many platforms is JayKaey: it is his own name, not the assistant's.
    #[test]
    fn sirs_own_handle_never_wakes_him() {
        let m = matcher();
        for input in [
            "JayKaey",
            "JayKaey, open Spotify",
            "Jay Kay, open Spotify",
            "Jay Kay open Spotify",
            "Jay K, what's the weather?",
            "JK, open Spotify",
            "Hey JayKay",
            "My name is Jay Kay",
            "Add me on Discord, JayKaey",
            "Sign in as Jay Kay.",
            "Jakey, open Spotify",
            "Play Jay-Z",
            "Jay Z, open Spotify",
        ] {
            let r = m.evaluate(input);
            assert!(!r.matched, "should not match: {input} (word '{}', score {:.2})", r.word, r.score);
            assert_eq!(r.command, input.trim(), "nothing is cut out of {input}");
            assert_eq!(canon(input), input, "and nothing rewritten");
        }
    }

    /// 2026-11-11 (the verifier's spoken commands, stock voices): the recogniser wrote his name
    /// and the order as one word, or misheard the order after it, and his name "was not heard".
    #[test]
    fn his_name_glued_to_the_order_or_in_front_of_a_misheard_one_wakes_him() {
        let m = matcher();
        for (input, command) in [
            ("Jaymute,", "mute"),
            ("JaySystem status.", "System status"),
            ("Hey Jayopen the mixer.", "open the mixer"),
            ("Jay Tugglespan Mode.", "Tugglespan Mode"),
            ("Jay said this called the 80%.", "said this called the 80%"),
            ("Jay Watts Open.", "Watts Open"),
            ("Jay use all Monitons.", "use all Monitons"),
            // What other prompts made of the same clips (stt::bench::spoken_commands_keep_his_name).
            ("J-Pause,", "Pause"),
            ("J-System status.", "System status"),
            ("J-Mute everything except Discord.", "Mute everything except Discord"),
            ("Jmute", "mute"),
            ("Jay-mute.", "mute"),
            ("Jaey, lower my browser's volume by 50%.", "lower my browser's volume by 50%"),
            ("Jaey set discord to 80%.", "set discord to 80%"),
            ("J volume 40.", "volume 40"),
        ] {
            let r = m.evaluate(input);
            assert!(r.matched, "should match: {input}");
            assert_eq!(r.command, command, "{input}");
        }
        assert_eq!(canon("Jaymute,"), "JAI, mute");
        // One word that only begins like his name, and Sir's handle with an order behind it.
        for input in [
            "Jaywalking is illegal here",
            "Jaipur weather tomorrow",
            "Jailbreak my phone",
            "Jayden open Spotify",
            "JayKaey mute",
            "JayKaeymute",
            "Jaymes said hello",
            "JList is a Swing component",
            "Just open Spotify",
            "Join the Discord call",
            "Jump to the next page",
            "J-Lo is on tour",
            "Jaey Kaey, open Spotify",
        ] {
            let r = m.evaluate(input);
            assert!(!r.matched, "should not match: {input} (command '{}')", r.command);
            assert_eq!(canon(input), input, "and nothing rewritten");
        }
    }

    #[test]
    fn other_people_called_jay_and_soundalikes_do_not_wake_him() {
        let m = matcher();
        for input in [
            "Jay said he would come over later",
            "Jay is my friend from school",
            "Jay and I went to the cinema",
            "Jay's playlist is great",
            "Jays rock is my playlist",
            "Play Jays rock",
            "Play something for Jay",
            "Call Jay",
            "I was talking with Jay",
            "My brother watches Jay clips all day",
            "Yay, we won!",
            "Ja, mach das.",
            "Je mehr desto besser",
            "Jake is coming over later",
            "Jade said hello",
            "Chase rock again",
            "Ich gehe jetzt einkaufen.",
            "What are you doing tonight?",
            "Open the JavaScript file please",
            "Charles is coming over later",
            "J",
        ] {
            let r = m.evaluate(input);
            assert!(!r.matched, "should NOT match: {input} (score {:.2}, word {})", r.score, r.word);
        }
        // A near miss in front of one of his commands is him ("Jake, open Spotify" was "Jay, …").
        for (input, command) in [("Jake, open Spotify", "open Spotify"), ("Jade, close all windows", "close all windows"), ("Chay, open Spotify", "open Spotify")] {
            let r = m.evaluate(input);
            assert!(r.matched && r.command == command, "{input}: {r:?}");
        }
    }

    #[test]
    fn the_name_at_the_end_and_in_the_middle() {
        let m = matcher();
        let r = m.evaluate("I want you to turn the music down Jay");
        assert!(r.matched, "the name at the very end");
        assert_eq!(r.command, "I want you to turn the music down");
        // The screenshot bug: "… switch back to JAI." lost its last word ("… back to").
        for (input, command) in [("Change back to JAI.", "Change back to JAI"), ("Wechsel zurück zu JAI", "Wechsel zurück zu JAI")] {
            let r = m.evaluate(input);
            assert!(r.matched, "{input}");
            assert_eq!(r.command, command, "{input}");
        }
        assert_eq!(m.evaluate("Play it again, Jay.").command, "Play it again");
        // He is the order's object: it keeps his name ("close" alone would close a window).
        for (input, command) in [("Close Jay.", "Close Jay"), ("Restart JAI", "Restart JAI"), ("Shut down Jay", "Shut down Jay"), ("Beende Jay", "Beende Jay")] {
            let r = m.evaluate(input);
            assert!(r.matched && r.command == command, "{input}: {r:?}");
        }
        let r = m.evaluate("Thank you Jay, that's all for now, can you please close yourself without saying goodbye?");
        assert!(r.matched);
        assert_eq!(r.command, "Thank you, that's all for now, can you please close yourself without saying goodbye?");
        assert!(m.evaluate("Okay so, Jay, what's next?").matched);
    }

    #[test]
    fn a_name_added_in_the_settings_still_wakes_him() {
        let m = WakeWordMatcher::new(&["friday".to_string()], 0.8);
        assert_eq!(m.evaluate("Friday, open Spotify").command, "open Spotify");
        assert_eq!(m.evaluate("Fryday, open Spotify").command, "open Spotify");
        assert!(m.evaluate("Jay, open Spotify").matched, "his own name always does");
        assert!(!m.evaluate("Fried eggs, please").matched);
    }

    #[test]
    fn what_was_said_to_him_reaches_the_parsers_with_one_spelling() {
        assert_eq!(canon("Jay, wake up, daddy's home"), "JAI, wake up, daddy's home");
        assert_eq!(canon("J.A.I."), "JAI");
        assert_eq!(canon("close J.A.I."), "close JAI");
        assert_eq!(canon("Shut down Jay"), "Shut down JAI");
        assert_eq!(canon("What does J.A.I. stand for?"), "What does JAI stand for?");
        assert_eq!(canon("What time is it, Jay?"), "JAI, What time is it?");
        assert_eq!(canon("Switch back to Jay."), "Switch back to JAI");
        assert_eq!(canon("Jay said he would come"), "Jay said he would come");
        assert_eq!(canon("open Spotify"), "open Spotify");
    }

    #[test]
    fn phonetic_normalisation() {
        assert_eq!(phonetic("Dschey"), "jey");
        assert_eq!(phonetic("Jäi"), "jei");
        assert_eq!(phonetic("Chay"), phonetic("jay"));
        assert_eq!(phonetic("Jaii"), "jai");
    }
}
