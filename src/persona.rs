//! JAI's voice on every line he says: himself in the first person, Sir in the second. Small
//! models drift into narrating ("Sir's request seems unclear", "Sir wants the music louder"),
//! log fragments ("Set background opacity lower.", "Opened Spotify.") and slang ("Welp"). The
//! stream filter rewrites whole sentences before they reach the chat or the voice.

use regex::{Captures, Regex};
use std::sync::LazyLock;

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid persona regex")
}

/// "Sir is" → "you are" … (verb forms after a third-person "Sir").
const THIRD_TO_SECOND: &[(&str, &str)] = &[
    ("is", "are"), ("was", "were"), ("has", "have"), ("does", "do"), ("wants", "want"), ("seems", "seem"), ("needs", "need"),
    ("likes", "like"), ("asked", "asked"), ("asks", "ask"), ("said", "said"), ("says", "say"), ("requested", "requested"),
    ("requests", "request"), ("prefers", "prefer"), ("means", "mean"), ("meant", "meant"), ("would", "would"), ("will", "will"),
    ("can", "can"), ("should", "should"), ("might", "might"), ("may", "may"), ("must", "must"), ("had", "had"), ("did", "did"),
    ("appears", "appear"), ("intends", "intend"), ("thinks", "think"), ("knows", "know"), ("expects", "expect"), ("hopes", "hope"),
];

/// Past-tense log fragments → the participle after "I have" ("Set" → "set", "Opened" → "opened").
const FRAGMENT_VERBS: &[(&str, &str)] = &[
    ("set", "set"), ("opened", "opened"), ("closed", "closed"), ("moved", "moved"), ("changed", "changed"), ("updated", "updated"),
    ("lowered", "lowered"), ("raised", "raised"), ("increased", "increased"), ("decreased", "decreased"), ("reduced", "reduced"),
    ("turned", "turned"), ("switched", "switched"), ("enabled", "enabled"), ("disabled", "disabled"), ("hid", "hidden"),
    ("hidden", "hidden"), ("showed", "shown"), ("removed", "removed"), ("added", "added"), ("created", "created"), ("saved", "saved"),
    ("deleted", "deleted"), ("started", "started"), ("stopped", "stopped"), ("paused", "paused"), ("resumed", "resumed"),
    ("muted", "muted"), ("unmuted", "unmuted"), ("adjusted", "adjusted"), ("applied", "applied"), ("reset", "reset"),
    ("restored", "restored"), ("launched", "launched"), ("played", "played"), ("queued", "queued"), ("sent", "sent"),
    ("scheduled", "scheduled"), ("made", "made"), ("put", "put"), ("placed", "placed"), ("resized", "resized"),
    ("collapsed", "collapsed"), ("darkened", "darkened"), ("brightened", "brightened"), ("toggled", "toggled"), ("activated", "activated"),
    ("deactivated", "deactivated"), ("found", "found"), ("checked", "checked"), ("took", "taken"), ("gave", "given"),
];

/// Interjections a butler never uses → what he would say (empty = dropped).
const SLANG: &[(&str, &str)] = &[
    ("welp", "Well"), ("yikes", ""), ("whoops", ""), ("oopsie", ""), ("okey-dokey", "Very good"), ("okey dokey", "Very good"),
    ("gonna", "going to"), ("wanna", "want to"), ("gotta", "have to"), ("kinda", "rather"), ("sorta", "rather"), ("yep", "Yes"),
    ("yup", "Yes"), ("nope", "No"), ("nah", "No"), ("dude", ""), ("bro", ""), ("lol", ""), ("haha", ""), ("ya know", ""),
    ("no worries", "Not at all"), ("no prob", "Not at all"), ("cool beans", "Very good"), ("awesome", "splendid"),
];

static SIR_POSSESSIVE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(?:the\s+)?sir['’]s\b"));
static SIR_VERB: LazyLock<Regex> = LazyLock::new(|| {
    let verbs: Vec<&str> = THIRD_TO_SECOND.iter().map(|(v, _)| *v).collect();
    re(&format!(r"(?i)\b(?:the\s+)?sir\s+({})\b", verbs.join("|")))
});
/// "for Sir", "to Sir", "help Sir": Sir as an object inside a sentence (not the vocative ", Sir").
static SIR_OBJECT: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(for|to|with|help|tell|show|give|remind|let|inform|ask|asking|telling|showing|giving)\s+sir\b(\s*[.!?]|\s+\w)"));
static SLANG_RE: LazyLock<Regex> = LazyLock::new(|| {
    let words: Vec<String> = SLANG.iter().map(|(w, _)| regex::escape(w)).collect();
    re(&format!(r"(?i)\b({})\b[,!.]?\s*", words.join("|")))
});

fn capitalise_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// "Consider it done, Sir." and its kin: an action's confirmation, alone in its sentence.
pub fn is_action_courtesy(sentence: &str) -> bool {
    let s: String = sentence.trim().to_lowercase().chars().filter(|c| c.is_alphabetic() || *c == ' ' || *c == '\'').collect();
    let s = s.trim_end_matches(" sir").trim();
    matches!(s, "consider it done" | "done" | "all done" | "it is done" | "its done" | "it's done" | "right away" | "on it" | "at once" | "very good" | "as you wish")
}

/// A character the model's tokens split in half (U+FFFD): an apostrophe between letters
/// ("It\u{fffd}s" → "It's"), dropped anywhere else.
pub fn repair_split_characters(text: &str) -> String {
    if !text.contains('\u{fffd}') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, c) in chars.iter().enumerate() {
        if *c != '\u{fffd}' {
            out.push(*c);
            continue;
        }
        let before = i > 0 && chars[i - 1].is_alphabetic();
        let after = chars.get(i + 1).is_some_and(|n| n.is_alphabetic());
        if before && after {
            out.push('\'');
        }
    }
    out
}

/// LaTeX a model writes into a spoken answer ("\(v = \sqrt{2gh}\)") as plain notation: "v = √(2gh)".
pub fn plain_maths(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_string();
    }
    static FRAC: LazyLock<Regex> = LazyLock::new(|| re(r"\\[dt]?frac\{([^{}]*)\}\{([^{}]*)\}"));
    static SQRT: LazyLock<Regex> = LazyLock::new(|| re(r"\\sqrt\{([^{}]*)\}"));
    static WRAPPED: LazyLock<Regex> = LazyLock::new(|| re(r"\\(?:text|mathrm|mathbf|mathit|operatorname)\{([^{}]*)\}"));
    static COMMAND: LazyLock<Regex> = LazyLock::new(|| re(r"\\([A-Za-z]+)"));
    let mut s = text.replace("\\(", "").replace("\\)", "").replace("\\[", "").replace("\\]", "").replace("\\,", " ").replace("\\;", " ").replace("\\!", "");
    for _ in 0..3 {
        s = FRAC.replace_all(&s, "($1)/($2)").into_owned();
        s = SQRT.replace_all(&s, "√($1)").into_owned();
        s = WRAPPED.replace_all(&s, "$1").into_owned();
    }
    s = COMMAND
        .replace_all(&s, |c: &Captures| {
            match &c[1] {
                "cdot" | "times" => "×",
                "approx" => "≈",
                "pm" => "±",
                "le" | "leq" => "≤",
                "ge" | "geq" => "≥",
                "Delta" => "Δ",
                "pi" => "π",
                "theta" => "θ",
                "alpha" => "α",
                "omega" => "ω",
                "circ" => "°",
                "left" | "right" | "quad" | "qquad" | "displaystyle" => "",
                other => return other.to_string(),
            }
            .to_string()
        })
        .into_owned();
    s.replace(['{', '}'], "").replace("  ", " ")
}

static SERVICE_CLOSER: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^(?:and\s+|so\s+)?(?:if (?:there(?:'s|’s| is) anything else|you (?:need|have|want|require|would like) (?:anything|any)(?: else| further| more)?)\b|(?:please\s+)?(?:let me know(?:\s+if\b|\s*[.!]?$)|feel free to\b|don['’]?t hesitate to\b|do not hesitate to\b)|is there anything else\b|(?:i )?hope (?:this|that) helps\b|happy to help\b|how (?:else )?(?:can|may) i (?:assist|help) you\b|i(?:'m| am) here (?:to help|if you need)\b)")
});

static MISADDRESSED: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^((?:done|of course|certainly|right away|straight away|at your service|very well|understood|yes|as you wish|consider it done|indeed|good (?:morning|afternoon|evening)|hello|welcome back)[^.!?]{0,40}?),\s+(?:dear\s+)?(?:earth|globe|world|planet|erde|globus)\s*([.!?])")
});

/// Rewrites one sentence (or a chunk of sentences) into JAI's voice.
pub fn rewrite(text: &str) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    let text = plain_maths(&repair_split_characters(text));
    let text = text.as_str();
    let lead = &text[..text.len() - text.trim_start().len()];
    let tail = &text[text.trim_end().len()..];
    let mut s = text.trim().to_string();

    // Customer-service closers ("If there's anything else you'd like assistance with, please let
    // me know!"): the film butler never says them, so the whole sentence goes.
    if SERVICE_CLOSER.is_match(&s) {
        return String::new();
    }

    // Sir addressed by what is on screen (2026-11-09, the globe up: "Done, Earth.", "At your
    // service, Earth.").
    s = MISADDRESSED.replace(&s, "$1, Sir$2").into_owned();

    // Slang interjections.
    s = SLANG_RE
        .replace_all(&s, |c: &Captures| {
            let word = c[1].to_lowercase();
            let rep = SLANG.iter().find(|(w, _)| *w == word).map(|(_, r)| *r).unwrap_or("");
            if rep.is_empty() {
                String::new()
            } else if c.get(0).is_some_and(|m| m.as_str().contains(',') || m.as_str().ends_with(' ')) && rep.chars().next().is_some_and(char::is_uppercase) {
                format!("{rep}, ")
            } else {
                format!("{rep} ")
            }
        })
        .trim()
        .to_string();

    // Third person → second person.
    s = SIR_VERB
        .replace_all(&s, |c: &Captures| {
            let verb = c[1].to_lowercase();
            let second = THIRD_TO_SECOND.iter().find(|(v, _)| *v == verb).map(|(_, r)| *r).unwrap_or("are");
            format!("you {second}")
        })
        .into_owned();
    s = SIR_POSSESSIVE.replace_all(&s, "your").into_owned();
    s = SIR_OBJECT
        .replace_all(&s, |c: &Captures| format!("{} you{}", &c[1], &c[2]))
        .into_owned();

    // A log fragment as the opening: "Set background opacity lower." → "I have set …".
    let first_word: String = s.chars().take_while(|c| c.is_alphabetic()).collect();
    let lower_first = first_word.to_lowercase();
    if let Some((_, participle)) = FRAGMENT_VERBS.iter().find(|(v, _)| *v == lower_first) {
        let rest = &s[first_word.len()..];
        // Only a clause, not "Set to 50 %, the volume …" spoken as an adjective phrase.
        let next: String = rest.trim_start().chars().take_while(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        let short = s.split_whitespace().count() <= 6;
        if next == "to" && short {
            // "Set to 50 percent." → "I have set it to 50 percent."
            s = format!("I have {participle} it{rest}");
        } else {
            let object_follows = !next.is_empty() && !matches!(next.as_str(), "by" | "as" | "in" | "at" | "for" | "to");
            if object_follows || rest.trim_start().starts_with(['.', '!']) || rest.trim().is_empty() {
                s = format!("I have {participle}{rest}");
            }
        }
    }

    // Tidy what the rewrites left: a lower-case or doubled start, stray spaces.
    let s = s.replace("  ", " ").replace(" ,", ",").replace(" .", ".");
    let s = s.trim_start_matches([',', ' ']).to_string();
    let s = if s.starts_with("you ") || s.starts_with("your ") { capitalise_first(&s) } else { s };
    // "Done. you are …" → "Done. You are …" where a rewrite started a sentence.
    static SENTENCE_YOU: LazyLock<Regex> = LazyLock::new(|| re(r"([.!?]\s+)(you|your)\b"));
    let s = SENTENCE_YOU.replace_all(&s, |c: &Captures| format!("{}{}", &c[1], capitalise_first(&c[2]))).into_owned();
    format!("{lead}{s}{tail}")
}

/// Streaming form: holds text until a sentence ends (or a long clause is complete), so a
/// third-person "Sir is …" is rewritten before any of it is heard.
#[derive(Default)]
pub struct PersonaStream {
    held: String,
    /// The reply informs (after a screen, web or calendar read): action courtesies such as
    /// "Consider it done, Sir." are dropped.
    pub info_only: bool,
}

impl PersonaStream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, token: &str) -> String {
        self.held.push_str(token);
        // The last sentence end followed by whitespace (or a newline).
        let bytes = self.held.as_bytes();
        let mut cut = None;
        for i in 0..bytes.len() {
            let end = matches!(bytes[i], b'.' | b'!' | b'?') && bytes.get(i + 1).is_some_and(|b| *b == b' ' || *b == b'\n');
            if end || bytes[i] == b'\n' {
                cut = Some(i + 1);
            }
        }
        // A long clause without punctuation goes out at a word boundary, unless it ends on "Sir"
        // (the next word decides how that is phrased).
        if cut.is_none() && self.held.split_whitespace().count() >= 14 && self.held.ends_with(' ') {
            let last = self.held.split_whitespace().last().unwrap_or("").trim_matches(|c: char| !c.is_alphabetic()).to_lowercase();
            if !matches!(last.as_str(), "sir" | "sirs" | "the") {
                cut = Some(self.held.len());
            }
        }
        match cut {
            Some(at) => {
                let ready: String = self.held.drain(..at).collect();
                self.finish(&ready)
            }
            None => String::new(),
        }
    }

    pub fn flush(&mut self) -> String {
        let held = std::mem::take(&mut self.held);
        self.finish(&held)
    }

    fn finish(&self, text: &str) -> String {
        let out = rewrite(text);
        if self.info_only && is_action_courtesy(&out) {
            return String::new();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn customer_service_closers_are_dropped() {
        for closer in [
            "If there's anything else you'd like assistance with, please let me know!",
            "Please let me know if you need anything else.",
            "Feel free to ask if you have more questions.",
            "Is there anything else I can help you with?",
            "I hope this helps!",
        ] {
            assert_eq!(rewrite(closer), "", "{closer}");
        }
        assert_eq!(rewrite("Let me know which playlist, Sir."), "Let me know which playlist, Sir.");
        assert_eq!(rewrite("Right away, Sir."), "Right away, Sir.");
        assert_eq!(rewrite("Done, Earth. The Iron Man suit is back."), "Done, Sir. The Iron Man suit is back.");
        assert_eq!(rewrite("At your service, Earth."), "At your service, Sir.");
        assert_eq!(rewrite("Mercury, Venus, Earth."), "Mercury, Venus, Earth.");
    }

    #[test]
    fn third_person_becomes_second_person() {
        assert_eq!(rewrite("Sir's request seems unclear."), "Your request seems unclear.");
        assert_eq!(rewrite("Sir wants the music louder, so I raised it."), "You want the music louder, so I raised it.");
        assert_eq!(rewrite("I believe Sir is busy."), "I believe you are busy.");
        assert_eq!(rewrite("Very good, Sir. I've done it for Sir."), "Very good, Sir. I've done it for you.");
        // The vocative stays.
        assert_eq!(rewrite("Right away, Sir."), "Right away, Sir.");
        assert_eq!(rewrite("Of course, Sir, the lights are on."), "Of course, Sir, the lights are on.");
    }

    #[test]
    fn fragments_and_slang_become_full_sentences() {
        assert_eq!(rewrite("Set background opacity lower."), "I have set background opacity lower.");
        assert_eq!(rewrite("Opened Spotify."), "I have opened Spotify.");
        assert_eq!(rewrite("Welp, that didn't work."), "Well, that didn't work.");
        assert_eq!(rewrite("I'm gonna check that."), "I'm going to check that.");
        assert_eq!(rewrite("Set to 50 percent by default, it plays quietly."), "Set to 50 percent by default, it plays quietly.");
        assert_eq!(rewrite("Settings are ready."), "Settings are ready.");
        assert_eq!(rewrite("Set to 50 percent."), "I have set it to 50 percent.");
        assert_eq!(rewrite("It uses 3.5 gigabytes."), "It uses 3.5 gigabytes.");
    }

    #[test]
    fn split_characters_and_misplaced_courtesies() {
        assert_eq!(repair_split_characters("It\u{fffd}s currently 12 degrees."), "It's currently 12 degrees.");
        assert_eq!(repair_split_characters("Done \u{fffd} really."), "Done  really.");
        let mut p = PersonaStream::new();
        p.info_only = true;
        let mut out = p.push("Your screen shows a code editor. ");
        out.push_str(&p.push("Consider it done, Sir. "));
        out.push_str(&p.flush());
        assert_eq!(out.trim_end(), "Your screen shows a code editor.");
        assert!(is_action_courtesy("Consider it done, Sir.") && !is_action_courtesy("Done with the build at nine."));
    }

    #[test]
    fn latex_becomes_plain_notation() {
        assert_eq!(plain_maths(r"So \(v = \sqrt{2 \cdot g \cdot h}\) m/s."), "So v = √(2 × g × h) m/s.");
        assert_eq!(plain_maths(r"\[t = \frac{v}{g}\]"), "t = (v)/(g)");
        assert_eq!(plain_maths(r"about 9.9 \text{m/s}"), "about 9.9 m/s");
        assert_eq!(plain_maths("No maths here."), "No maths here.");
    }

    #[test]
    fn the_stream_rewrites_before_release() {
        let mut p = PersonaStream::new();
        let mut out = String::new();
        for t in ["Sir", "'s re", "quest seems", " unclear. ", "Shall I ", "try again?"] {
            out.push_str(&p.push(t));
        }
        out.push_str(&p.flush());
        assert_eq!(out, "Your request seems unclear. Shall I try again?");
    }
}
