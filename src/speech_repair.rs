//! Sir's words as he says them, cleaned before anything reads them: the fillers dropped ("uh",
//! "ähm"), a stutter said once ("the the", "I-I", "can you can you"), a correction followed
//! ("open Chrome, no wait, Firefox" → "open Firefox"; "set a timer for 5 minutes, no, 10";
//! "turn it up, no, down"; "öffne Discord, nein, Spotify") and an order taken back halfway
//! ("open the … never mind" → "never mind"). Words he dictates (a message, a note, a search)
//! are left as said: a correction there is part of what is to be written. `repaired` runs at
//! the head of `typos::normalize_user_input_typos`, so every router and the model read the
//! cleaned sentence; `dangling` tells the ear that a sentence still waits for its correction.

/// Sounds that carry no word.
const FILLERS: &[&str] = &["uh", "uhh", "uhm", "um", "umm", "erm", "hmm", "hm", "äh", "ähh", "ähm", "äähm", "öhm", "ehm"];

/// Words a stutter repeats (never a name's: "Duran Duran", "Talk Talk" stay; nor German's
/// der/die/das, which a relative clause may double).
const STUTTERED: &[&str] = &[
    "i", "a", "an", "the", "to", "and", "my", "it", "can", "you", "we", "me", "in", "on", "of", "for", "with", "at", "so", "but", "or", "open", "close", "play",
    "set", "turn", "make", "show", "put", "start", "stop", "take", "give", "what", "how", "where", "when", "why", "who", "could", "would", "please", "just",
    "let", "ich", "du", "und", "zu", "mit", "für", "auf", "mach", "öffne", "spiel", "zeig", "setz", "stell", "schalte", "kannst", "bitte", "was", "wie", "wo", "wann",
];

/// Orders a correction may start over with ("open Chrome, no wait, close it").
const VERBS: &[&str] = &[
    "open", "close", "play", "pause", "stop", "start", "set", "turn", "make", "show", "put", "move", "mute", "unmute", "skip", "go", "switch", "change", "search",
    "find", "call", "send", "remind", "create", "delete", "add", "remove", "take", "give", "launch", "quit", "exit", "restart", "shut", "lock", "increase",
    "decrease", "raise", "lower", "maximize", "maximise", "minimize", "minimise", "record", "save", "copy", "read", "check", "enable", "disable", "cancel",
    "öffne", "oeffne", "schließe", "schliesse", "schließ", "schliess", "spiel", "spiele", "starte", "stopp", "stoppe", "mach", "mache", "zeig", "zeige",
    "setz", "setze", "stell", "stelle", "schalte", "schalt", "leg", "lege", "such", "suche", "finde", "schick", "schicke", "erinnere", "lösch", "lösche",
    "pausiere", "beende", "sperr", "sperre", "wechsel", "wechsle", "ändere", "aendere", "ruf", "rufe",
];

/// Words a correction may begin at, cutting the sentence where it last said them ("move it
/// to the left monitor, no, the right one").
const ANCHORS: &[&str] = &[
    "to", "for", "the", "in", "on", "at", "with", "my", "into", "onto", "from", "by", "zu", "zum", "zur", "für", "auf", "im", "an", "am", "mit", "um", "nach",
    "den", "die", "das", "dem", "der", "meine", "mein", "meinen",
];

/// Words that stand against each other: "up, no, down".
const OPPOSITES: &[&[&str]] = &[
    &["up", "down"],
    &["left", "right"],
    &["on", "off"],
    &["louder", "quieter", "softer"],
    &["higher", "lower"],
    &["bigger", "smaller", "larger"],
    &["brighter", "darker"],
    &["more", "less"],
    &["next", "previous"],
    &["lauter", "leiser"],
    &["hoch", "runter"],
    &["links", "rechts"],
    &["an", "aus"],
    &["größer", "kleiner"],
    &["heller", "dunkler"],
    &["mehr", "weniger"],
];

/// What an order is about at its end, kept when the word before it is corrected ("play some
/// rock music, I mean jazz" → "play some jazz music").
const HEADS: &[&str] = &[
    "music", "song", "songs", "track", "playlist", "album", "video", "window", "app", "program", "monitor", "screen", "file", "folder", "theme", "colour",
    "color", "mode", "timer", "alarm", "musik", "lied", "titel", "fenster", "programm", "bildschirm", "datei", "ordner", "farbe", "modus", "wecker",
];

/// Words that dictate: what follows is to be written or searched as said.
const DICTATION: &[&str] = &[
    "saying", "says", "message", "text", "note", "write", "type", "dictate", "tell", "reply", "email", "e-mail", "tweet", "post", "search", "google",
    "remind", "reminder", "schreib", "schreibe", "nachricht", "sag", "sage", "antworte", "notiz", "notiere", "such", "suche", "googel", "googeln",
    "erinnere", "erinnerung",
];

/// The words that take an order back.
const TAKEN_BACK: &[&[&str]] = &[
    &["never", "mind"],
    &["nevermind"],
    &["forget", "it"],
    &["forget", "that"],
    &["don't"],
    &["do", "not"],
    &["nothing"],
    &["cancel", "that"],
    &["scratch", "that"],
    &["vergiss", "es"],
    &["vergiss", "das"],
    &["lass", "es"],
    &["doch", "nicht"],
    &["lieber", "nicht"],
    &["egal"],
    &["schon", "gut"],
    &["nichts"],
];

/// A correction's own words. `clear`: it needs no comma to be one ("no wait"); the others are
/// only a correction as a clause of their own ("open Chrome, no, Firefox", never "the mixer,
/// no sound").
struct Marker {
    words: &'static [&'static str],
    clear: bool,
}

const MARKERS: &[Marker] = &[
    Marker { words: &["no", "wait"], clear: true },
    Marker { words: &["wait", "no"], clear: true },
    Marker { words: &["nein", "warte"], clear: true },
    Marker { words: &["warte", "nein"], clear: true },
    Marker { words: &["sorry", "i", "mean"], clear: false },
    Marker { words: &["no", "i", "mean"], clear: false },
    Marker { words: &["no", "sorry"], clear: false },
    Marker { words: &["i", "mean"], clear: false },
    Marker { words: &["i", "meant"], clear: false },
    Marker { words: &["or", "rather"], clear: false },
    Marker { words: &["make", "that"], clear: false },
    Marker { words: &["ich", "meine"], clear: false },
    Marker { words: &["ich", "meinte"], clear: false },
    Marker { words: &["besser", "gesagt"], clear: false },
    Marker { words: &["actually"], clear: false },
    Marker { words: &["rather"], clear: false },
    Marker { words: &["lieber"], clear: false },
    Marker { words: &["no"], clear: false },
    Marker { words: &["nope"], clear: false },
    Marker { words: &["sorry"], clear: false },
    Marker { words: &["wait"], clear: false },
    Marker { words: &["nein"], clear: false },
    Marker { words: &["nee"], clear: false },
    Marker { words: &["quatsch"], clear: false },
];

/// The single words that are a correction only between two commas ("open Chrome, no,
/// Firefox"): otherwise they are words of the order ("no sound", "sorry for the noise").
const SHUT_IN: &[&str] = &["no", "nope", "sorry", "wait", "nein", "nee", "quatsch"];

/// A word as the rules read it: lower case, without the punctuation around it.
fn core(token: &str) -> String {
    token.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// A token that ends a clause ("Chrome,", "the…", a dash of its own).
fn breaks(token: &str) -> bool {
    matches!(token, "-" | "–" | "—") || token.ends_with([',', '.', ';', ':', '-', '–', '—', '…', '!', '?'])
}

fn dash(token: &str) -> bool {
    matches!(token, "-" | "–" | "—")
}

fn is_number(core: &str) -> bool {
    const WORDS: &[&str] = &[
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "fifteen", "twenty", "thirty", "forty", "fifty",
        "sixty", "hundred", "null", "eins", "zwei", "drei", "vier", "fünf", "sechs", "sieben", "acht", "neun", "zehn", "elf", "zwölf", "fünfzehn", "zwanzig",
        "dreißig", "vierzig", "fünfzig", "sechzig", "hundert",
    ];
    !core.is_empty() && (core.replace(',', ".").parse::<f64>().is_ok() || WORDS.contains(&core))
}

/// Sir's sentence cleaned, when anything in it was a filler, a stutter, a correction or an
/// order taken back; None when it stands as said. Cleaning a cleaned sentence changes nothing.
pub fn repaired(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let tokens: Vec<String> = trimmed.split_whitespace().map(str::to_string).collect();
    // Code, pasted text and long dictation are left exactly as written.
    if tokens.is_empty() || tokens.len() > 40 || trimmed.contains("```") {
        return None;
    }
    let tokens = corrected(said_once(without_fillers(tokens)));
    let out = tokens.join(" ");
    let out = out.trim_start_matches([',', ';', ' ']).trim().to_string();
    (!out.is_empty() && out != tokens_of(trimmed)).then_some(out)
}

fn tokens_of(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the word at `i` is a filler. "um" is one only set off by a comma or at the start and
/// marked itself, or last ("Um, open …", "for, um, ten", "open the, um"): otherwise it is
/// German ("um 15 Uhr", "schalt um", "dreh dich um.").
fn filler(tokens: &[String], i: usize) -> bool {
    let c = core(&tokens[i]);
    FILLERS.contains(&c.as_str())
        && (c != "um" || ((i == 0 || tokens[i - 1].ends_with(',')) && (i + 1 == tokens.len() || tokens[i].ends_with([',', '.', '…']))))
}

fn without_fillers(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    for (i, token) in tokens.iter().enumerate() {
        if !filler(&tokens, i) {
            out.push(token.clone());
            continue;
        }
        // "for, uh, ten" → "for ten"; "what's that hmm?" keeps its question mark; "Chrome, uh,
        // I mean Firefox" keeps the comma that makes the correction one.
        let end = token.trim_end_matches(|c: char| c.is_alphanumeric() || c == ',').chars().last().filter(|c| matches!(c, '.' | '?' | '!'));
        let correction_next = tokens.get(i + 1).is_some_and(|t| MARKERS.iter().any(|m| m.words[0] == core(t)));
        if let Some(previous) = out.last_mut() {
            if !correction_next {
                let kept = previous.trim_end_matches(',').to_string();
                *previous = kept;
            }
            if let Some(end) = end.filter(|_| !previous.ends_with(['.', '?', '!'])) {
                previous.push(end);
            }
        }
    }
    out
}

fn said_once(mut tokens: Vec<String>) -> Vec<String> {
    // "I-I", "Th-the", "o-open": the word, once (in the case it began with).
    for token in tokens.iter_mut() {
        loop {
            let Some((left, right)) = token.split_once('-') else { break };
            let (l, r) = (left.to_lowercase(), right.to_lowercase());
            if l.is_empty() || l.chars().count() > 3 || !l.chars().all(char::is_alphabetic) || !r.starts_with(&l) {
                break;
            }
            let upper = left.chars().next().is_some_and(char::is_uppercase);
            let mut word: String = right.to_string();
            if upper {
                word = word.chars().take(1).flat_map(char::to_uppercase).chain(word.chars().skip(1)).collect();
            }
            *token = word;
        }
    }
    // "the the", "can you can you", "I want to I want to".
    let mut i = 0;
    while i < tokens.len() {
        let mut dropped = false;
        for n in (1..=4).rev() {
            if i + 2 * n > tokens.len() {
                continue;
            }
            let first: Vec<String> = tokens[i..i + n].iter().map(|t| core(t)).collect();
            let second: Vec<String> = tokens[i + n..i + 2 * n].iter().map(|t| core(t)).collect();
            if first == second && STUTTERED.contains(&first[0].as_str()) && first.iter().all(|w| !w.is_empty()) {
                // The first saying stays (its capital), the end of the second its full stop.
                let end: String = tokens[i + 2 * n - 1].chars().rev().take_while(|c| matches!(c, '.' | '?' | '!')).collect::<Vec<_>>().into_iter().rev().collect();
                tokens.drain(i + n..i + 2 * n);
                let kept = &mut tokens[i + n - 1];
                *kept = kept.trim_end_matches(',').to_string();
                if !end.is_empty() && !kept.ends_with(['.', '?', '!']) {
                    kept.push_str(&end);
                }
                dropped = true;
                break;
            }
        }
        if !dropped {
            i += 1;
        }
    }
    tokens
}

/// Where a correction's words stand at `i` (their number), if they are one there.
fn marker_at(tokens: &[String], i: usize) -> Option<usize> {
    let cores: Vec<String> = tokens[i..].iter().take(3).map(|t| core(t)).collect();
    MARKERS.iter().find_map(|m| {
        let n = m.words.len();
        if cores.len() < n || cores[..n].iter().zip(m.words).any(|(c, w)| c != w) {
            return None;
        }
        // Inside a word of the order, never: a correction stands after a clause's end
        // ("Chrome, no wait" / "Chrome. Wait,"), the clear ones anywhere.
        let after_break = i > 0 && breaks(&tokens[i - 1]);
        if !m.clear && !after_break {
            return None;
        }
        // "no", "sorry", "wait", "nein" alone: only between two commas (or a dash).
        if n == 1 && SHUT_IN.contains(&m.words[0]) {
            let closed = tokens[i].ends_with([',', '.', '!', '-', '–', '—']) || tokens.get(i + 1).is_some_and(|t| dash(t));
            if !closed {
                return None;
            }
        }
        Some(n)
    })
}

fn corrected(tokens: Vec<String>) -> Vec<String> {
    let lower = tokens.iter().map(|t| core(t)).collect::<Vec<_>>().join(" ");
    // Teaching his ear ("when I say this card I mean Discord") is no correction.
    if lower.starts_with("when i say") || lower.starts_with("if i say") || lower.starts_with("wenn ich") {
        return tokens;
    }
    let original = tokens.clone();
    let mut tokens = tokens;
    let mut from = 1;
    loop {
        let Some((at, len)) = (from..tokens.len()).find_map(|i| marker_at(&tokens, i).map(|n| (i, n))) else { break };
        let left: Vec<String> = tokens[..at].to_vec();
        // The correction runs to the next correction or the end.
        let start = at + len;
        let end = (start..tokens.len()).find(|&j| j > start && marker_at(&tokens, j).is_some()).unwrap_or(tokens.len());
        let right: Vec<String> = tokens[start..end].iter().filter(|t| !dash(t)).cloned().collect();
        // Dictated words keep their corrections; so does a question about meaning ("what I mean").
        let dictated = left.iter().any(|t| DICTATION.contains(&core(t).as_str()));
        let asks = left.last().is_some_and(|t| matches!(core(t).as_str(), "what" | "was" | "how" | "wie" | "why" | "warum" | "know" | "weißt"));
        if dictated || asks || right.iter().all(|t| core(t).is_empty()) {
            return original;
        }
        let Some(mut fixed) = applied(left, right) else { return original };
        let rest = tokens[end..].to_vec();
        from = fixed.len().max(1);
        fixed.extend(rest);
        tokens = fixed;
    }
    taken_back(tokens)
}

/// The order with its correction in place; None when the words cannot be lined up (the
/// sentence then reaches the model as said).
fn applied(mut left: Vec<String>, right: Vec<String>) -> Option<Vec<String>> {
    while left.last().is_some_and(|t| dash(t)) {
        left.pop();
    }
    let last = left.last_mut()?;
    *last = last.trim_end_matches([',', ';', ':', '-', '–', '—', '.', '…']).to_string();
    if left.iter().all(|t| core(t).is_empty()) {
        return None;
    }
    let rc: Vec<String> = right.iter().map(|t| core(t)).collect();
    // The correction's own clause: up to "and" / "then" or the end of its clause.
    let mut core_len = right.len();
    for (k, t) in right.iter().enumerate() {
        if k > 0 && matches!(rc[k].as_str(), "and" | "und" | "then" | "dann") {
            core_len = k;
            break;
        }
        if breaks(t) {
            core_len = k + 1;
            break;
        }
    }
    let (fix, rest) = right.split_at(core_len);
    let fix_cores = &rc[..core_len];
    // A fix set into the middle of the order leaves its full stop at the clause's end.
    let mark: String = fix.last().map(|t| t.chars().rev().take_while(|c| matches!(c, '.' | '?' | '!')).collect::<Vec<_>>().into_iter().rev().collect()).unwrap_or_default();
    let bare: Vec<String> = fix.iter().map(|t| t.trim_end_matches(['.', '?', '!', '…', ',', ';', ':']).to_string()).collect();
    if fix_cores.iter().all(|c| matches!(c.as_str(), "danke" | "thanks" | "thank" | "please" | "bitte" | "")) {
        return None;
    }
    // Taken back: "open Spotify, no wait, don't" → "never mind" (or the order that follows it).
    if let Some(n) = TAKEN_BACK.iter().find(|p| fix_cores.len() >= p.len() && fix_cores[..p.len()].iter().zip(p.iter()).all(|(c, w)| c == w)).map(|p| p.len()) {
        let after: Vec<String> = right[n..].iter().filter(|t| !core(t).is_empty()).cloned().collect();
        return match after.first() {
            None => Some(vec!["never mind".to_string()]),
            Some(first) if VERBS.contains(&core(first).as_str()) => Some(after),
            Some(_) => None,
        };
    }
    let first = fix_cores[0].as_str();
    let asks_anew = matches!(first, "please" | "bitte") || (fix_cores.len() >= 2 && matches!((first, fix_cores[1].as_str()), ("can" | "could" | "would", "you") | ("kannst" | "könntest", "du")));
    // A new order: it stands instead of the old one.
    if VERBS.contains(&first) || asks_anew {
        return Some(right);
    }
    // "…to the left monitor, no, the right one": from where the left side last said "the".
    if fix_cores.len() >= 2 && ANCHORS.contains(&first) {
        if let Some(p) = left.iter().rposition(|t| core(t) == first).filter(|&p| p >= 1) {
            let mut out = left[..p].to_vec();
            out.extend(right);
            return Some(out);
        }
    }
    let replace = |left: &[String], p: usize, span: usize| -> Vec<String> {
        let mut out = left[..p].to_vec();
        out.extend(bare.iter().cloned());
        out.extend(left[(p + span).min(left.len())..].iter().cloned());
        if let Some(last) = out.last_mut().filter(|_| !mark.is_empty()) {
            last.push_str(&mark);
        }
        out.extend(rest.iter().cloned());
        out
    };
    // A number for a number: "a timer for 5 minutes, no, 10" → "a timer for 10 minutes".
    if is_number(first) {
        if let Some(p) = left.iter().rposition(|t| is_number(&core(t))) {
            return Some(replace(&left, p, fix.len()));
        }
    }
    // A word for its opposite: "turn it up, no, down".
    if fix.len() == 1 {
        if let Some(group) = OPPOSITES.iter().find(|g| g.contains(&first)) {
            if let Some(p) = left.iter().rposition(|t| group.contains(&core(t).as_str())) {
                return Some(replace(&left, p, 1));
            }
        }
    }
    // A name for a name: "open Visual Studio Code, no, Discord" (never the sentence's first word).
    let capital = |t: &String| t.chars().next().is_some_and(char::is_uppercase);
    if fix.first().is_some_and(capital) && left.len() >= 2 && left.last().is_some_and(capital) {
        let run = left[1..].iter().rev().take_while(|t| capital(t)).take(4).count();
        let p = left.len() - run;
        let mut out = left[..p].to_vec();
        out.extend(right);
        return Some(out);
    }
    // A word or two for the last ones of an order: "play rock, I mean jazz"; "play some rock
    // music, I mean jazz" → "play some jazz music" (never a remark's: "it's nice, actually, cold").
    let an_order = left.iter().take(3).any(|t| VERBS.contains(&core(t).as_str()));
    if an_order && (1..=3).contains(&fix.len()) {
        let head = usize::from(left.last().is_some_and(|t| HEADS.contains(&core(t).as_str())));
        if left.len() > fix.len() + head {
            return Some(replace(&left, left.len() - head - fix.len(), fix.len()));
        }
    }
    None
}

/// "Open the … never mind", "Remind me to, ach, vergiss es": the whole order is taken back.
fn taken_back(tokens: Vec<String>) -> Vec<String> {
    // (The last word's own full stop is no break: "Open the... never mind.")
    let Some(cut) = tokens[..tokens.len().saturating_sub(1)].iter().rposition(|t| breaks(t)) else { return tokens };
    let tail: Vec<String> = tokens[cut + 1..].iter().map(|t| core(t)).filter(|c| !c.is_empty() && !matches!(c.as_str(), "jai" | "sir" | "please" | "bitte")).collect();
    let before = &tokens[..=cut];
    let said_before = before.iter().any(|t| !core(t).is_empty());
    // A reminder's words are dictated, but "remind me to, ach, vergiss es" leaves none.
    let dictated = before.iter().map(|t| core(t)).any(|c| DICTATION.contains(&c.as_str()) && !matches!(c.as_str(), "remind" | "reminder" | "erinnere" | "erinnerung"));
    if said_before && !dictated && TAKEN_BACK.iter().any(|p| tail.len() == p.len() && tail.iter().zip(p.iter()).all(|(c, w)| c == w)) {
        return vec!["never mind".to_string()];
    }
    tokens
}

/// The sentence stops where something must still come: on a filler ("set a timer for, uh") or
/// on a correction whose new words are still to be said ("open Chrome, no wait").
pub fn dangling(text: &str) -> bool {
    let t = text.trim();
    let tokens: Vec<String> = t.split_whitespace().map(str::to_string).collect();
    !t.ends_with('?') && (!tokens.is_empty() && filler(&tokens, tokens.len() - 1) || withdrawn(t))
}

/// A correction with nothing after it ("Chrome, no wait" / "Chrome, I mean"): the order is
/// taken back until its new words come, and never carried out as it stood.
pub fn withdrawn(text: &str) -> bool {
    let t = text.trim();
    if t.ends_with('?') {
        return false;
    }
    let tokens: Vec<String> = t.split_whitespace().map(str::to_string).collect();
    (1..tokens.len()).any(|i| {
        marker_at(&tokens, i).is_some_and(|n| i + n == tokens.len() && !matches!(core(&tokens[i + n - 1]).as_str(), "meant" | "no" | "nope" | "nein" | "nee" | "sorry" | "quatsch"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(text: &str) -> String {
        repaired(text).unwrap_or_else(|| text.to_string())
    }

    #[test]
    fn fillers_and_stutters_are_said_once() {
        for (said, meant) in [
            ("Set a timer for, uh, ten minutes.", "Set a timer for ten minutes."),
            ("Uh, open Spotify.", "open Spotify."),
            ("open the the browser", "open the browser"),
            ("I-I want to play music", "I want to play music"),
            ("Th-the weather in Berlin", "The weather in Berlin"),
            ("I want to I want to play music", "I want to play music"),
            ("can you can you open Discord", "can you open Discord"),
            ("Mach, ähm, die Musik lauter", "Mach die Musik lauter"),
            ("Öffne öffne den Browser", "Öffne den Browser"),
            ("what's that hmm?", "what's that?"),
            ("Um, open Chrome", "open Chrome"),
            ("Set a timer for, um, ten minutes", "Set a timer for ten minutes"),
        ] {
            assert_eq!(clean(said), meant, "{said}");
        }
        // German "um" is no filler: "um 15 Uhr", "schalt um", "dreh dich um".
        for said in ["Trag morgen um 15 Uhr Test in meinen Kalender ein", "Was mache ich normalerweise um diese Zeit?", "Schalt auf Spotify um.", "Dreh dich um, und dann öffne Chrome", "Um 15 Uhr erinnere mich"] {
            assert_eq!(repaired(said), None, "{said}");
        }
    }

    #[test]
    fn a_correction_is_followed() {
        for (said, meant) in [
            ("Open Chrome, no wait, Firefox.", "Open Firefox."),
            ("open Chrome no wait Firefox", "open Firefox"),
            // (Joined after a pause: "Open Chrome, no wait" … "Firefox".)
            ("Open Chrome, no wait Firefox", "Open Firefox"),
            ("Öffne Discord, nein warte Spotify", "Öffne Spotify"),
            ("Open Visual Studio Code, no, Discord.", "Open Discord."),
            ("Set a timer for 5 minutes, no, 10.", "Set a timer for 10 minutes."),
            ("Set a timer for 5 minutes, no, 10. And play music.", "Set a timer for 10 minutes. And play music."),
            ("Open Chrome, uh, I mean Firefox.", "Open Firefox."),
            ("Is it raining in Berlin, no, Hamburg?", "Is it raining in Hamburg?"),
            ("set a timer for 5 minutes, no, 10 minutes", "set a timer for 10 minutes"),
            ("Set the volume to 30, no, 40 percent.", "Set the volume to 40 percent."),
            ("Turn it up, no, down.", "Turn it down."),
            ("Move it to the left monitor, no, the right one.", "Move it to the right one."),
            ("Play rock, I mean jazz.", "Play jazz."),
            ("Play some rock music, I mean jazz.", "Play some jazz music."),
            ("Open Spotify, no wait, close Spotify.", "close Spotify."),
            ("Open Chrome, actually, can you open Edge?", "can you open Edge?"),
            ("Open Chrome, no, Firefox, no wait, Edge.", "Open Edge."),
            ("Öffne Discord, nein, Spotify.", "Öffne Spotify."),
            ("Mach die Musik lauter, nein, leiser.", "Mach die Musik leiser."),
            ("Zeig mir das Wetter in Berlin, ich meine in Hamburg.", "Zeig mir das Wetter in Hamburg."),
            ("Stell einen Timer auf 5 Minuten, nein warte, 10 Minuten.", "Stell einen Timer auf 10 Minuten."),
            ("Open Spotify. No wait, don't.", "never mind"),
            ("Open Spotify, no wait, never mind, play some music.", "play some music."),
        ] {
            assert_eq!(clean(said), meant, "{said}");
        }
    }

    /// What he carries out is the order as meant: the cleaning runs before every router.
    #[test]
    fn a_corrected_order_goes_where_the_meant_one_does() {
        // (A timer's start is the clock's: it may differ by a millisecond between the two.)
        let route = |text: &str| {
            crate::phrasings::route(&crate::typos::normalize_user_input_typos(text)).map(|(tool, mut args)| {
                args.as_object_mut().map(|a| a.remove("started_at_ms"));
                (tool, args)
            })
        };
        for (said, meant) in [
            ("Open Chrome, uh, I mean Firefox.", "Open Firefox."),
            ("Set a timer for 5 minutes, no, 10.", "Set a timer for 10 minutes."),
            ("Mach die Musik lauter, nein, leiser.", "Mach die Musik leiser."),
            ("Open the, uh, the mixer", "Open the mixer"),
        ] {
            assert!(route(meant).is_some(), "{meant}");
            assert_eq!(route(said), route(meant), "{said}");
        }
    }

    #[test]
    fn an_order_taken_back_halfway_is_never_mind() {
        for said in ["Open the... never mind.", "Remind me to, ach, vergiss es.", "Turn off the - forget it", "Set the theme to, doch nicht."] {
            assert_eq!(clean(said), "never mind", "{said}");
        }
        // Not taken back: the words go on, or "never mind" leads the order.
        for said in ["Never mind, undo that.", "forget that I like jazz", "never mind the weather, play music"] {
            assert_eq!(clean(said), said, "{said}");
        }
    }

    /// What is no correction stays as said: words of the order, dictation, teaching his ear,
    /// questions about meaning, names that double.
    #[test]
    fn what_is_no_correction_stays_as_said() {
        for said in [
            "Open the mixer, no sound",
            "There's no sound and open the mixer.",
            "Play No Doubt",
            "Send Max a message saying I'm late, sorry, the bus was late.",
            "Remind me to call mom, no, dad.",
            "Write a note: buy milk, no, eggs.",
            "When I say this card, I mean Discord.",
            "You know what I mean",
            "That's not what I meant.",
            "Do you know what I mean by that?",
            "Play Duran Duran",
            "Play New York, New York",
            "Talk Talk",
            "No, open Spotify.",
            "I mean it",
            "Ja, nein, danke.",
            "Er hat gesagt, die die Musik mögen, kommen.",
            "Make that window bigger",
            "Search for sorry, no, wait by Kodaline",
            "Is it raining, no, really?",
            "The weather is nice, actually, it's cold.",
        ] {
            assert_eq!(clean(said), tokens_of(said), "{said}");
        }
    }

    #[test]
    fn cleaning_twice_changes_nothing() {
        for said in ["Set a timer for, uh, ten minutes.", "Open Chrome, no wait, Firefox.", "Open the... never mind.", "I-I want to I want to play music", "Turn it up, no, down."] {
            let once = clean(said);
            assert_eq!(repaired(&once), None, "{said} → {once}");
        }
    }

    #[test]
    fn a_correction_still_to_come_dangles() {
        for said in ["Open Chrome, no wait", "Open Chrome, I mean", "Set a timer for five minutes and uh", "Open the, ähm", "Öffne Discord, nein warte", "Open Chrome. Wait,"] {
            assert!(dangling(said), "{said}");
        }
        for said in ["Open Chrome", "That's not what I meant.", "You know what I mean", "Is it raining, no?", "Open the radar? No.", "Play No Doubt", "Hold on"] {
            assert!(!dangling(said), "{said}");
        }
        // A filler only pauses the order; a correction takes it back until its words come.
        for said in ["Open Chrome, no wait", "Open Chrome, I mean", "Öffne Discord, nein warte"] {
            assert!(withdrawn(said), "{said}");
        }
        for said in ["Set a timer for five minutes and uh", "Open the, ähm", "Open Chrome", "Open Chrome, no wait, Firefox"] {
            assert!(!withdrawn(said), "{said}");
        }
    }
}
