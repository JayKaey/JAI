//! Typo-tolerant input: the misspellings Sir types and Whisper writes for Jarvis' own domain
//! words ("calender", "tommorw", "volum", "shudown", "systm stats") become the real word before
//! any routing, so the deterministic fast paths see what he meant.
//!
//! Known misspellings come from a table. Any other word is only corrected when it is a close,
//! same-start variant of a domain keyword: Jaro-Winkler ≥ 0.84 **and** at most one edit (two for
//! words of eight letters or more). Inflections ("restarted", "Mondays") and ordinary look-alikes
//! ("restore", "sundae", "showdown") stay as they are.
//!
//! Omitted nouns are repaired too: "add a new in my calendar" reads as "add a new event in my
//! calendar".

use regex::Regex;
use std::sync::LazyLock;

/// Misspelling → word, anywhere in a sentence (lower case).
const KNOWN: &[(&str, &str)] = &[
    // Calendar.
    ("calender", "calendar"), ("calandar", "calendar"), ("calander", "calendar"), ("calendr", "calendar"), ("calandr", "calendar"),
    ("kalendar", "calendar"), ("callendar", "calendar"), ("calenda", "calendar"), ("calnedar", "calendar"), ("clendar", "calendar"),
    ("kalnder", "kalender"), ("kallender", "kalender"), ("kalener", "kalender"),
    ("appointmnt", "appointment"), ("apointment", "appointment"), ("appoitment", "appointment"), ("appointement", "appointment"),
    ("evnt", "event"), ("evetn", "event"), ("eveent", "event"),
    ("shedule", "schedule"), ("schedual", "schedule"), ("scedule", "schedule"), ("schdule", "schedule"), ("schedulle", "schedule"),
    ("remidner", "reminder"), ("reminer", "reminder"), ("remider", "reminder"),
    // Days.
    ("tommorw", "tomorrow"), ("tomorow", "tomorrow"), ("tommorow", "tomorrow"), ("tmrw", "tomorrow"), ("tmrrw", "tomorrow"),
    ("tomrrow", "tomorrow"), ("tomorrw", "tomorrow"), ("tommorrow", "tomorrow"), ("tomoro", "tomorrow"), ("tomorro", "tomorrow"),
    ("2morrow", "tomorrow"), ("2moro", "tomorrow"), ("tomrow", "tomorrow"), ("tommrow", "tomorrow"), ("tomorrrow", "tomorrow"),
    ("morgn", "morgen"), ("moregn", "morgen"), ("übermorgn", "übermorgen"), ("ubermorgen", "übermorgen"),
    ("yestaday", "yesterday"), ("yesterdy", "yesterday"), ("yesturday", "yesterday"), ("yday", "yesterday"), ("yesteday", "yesterday"),
    ("wednsday", "wednesday"), ("wensday", "wednesday"), ("wedensday", "wednesday"), ("wendsday", "wednesday"), ("wednessday", "wednesday"),
    ("tuesdy", "tuesday"), ("tusday", "tuesday"), ("teusday", "tuesday"), ("thursdy", "thursday"), ("thurday", "thursday"),
    ("thrusday", "thursday"), ("firday", "friday"), ("fridy", "friday"), ("saterday", "saturday"), ("saturdy", "saturday"),
    ("sundy", "sunday"), ("mondy", "monday"), ("monady", "monday"),
    ("mitwoch", "mittwoch"), ("donnerstg", "donnerstag"), ("freitg", "freitag"), ("samtag", "samstag"), ("sonntg", "sonntag"),
    // Volume and media.
    ("volum", "volume"), ("voume", "volume"), ("volme", "volume"), ("vollume", "volume"), ("valume", "volume"), ("volune", "volume"),
    ("lautstarke", "lautstärke"), ("lautsärke", "lautstärke"),
    ("paus", "pause"), ("puase", "pause"), ("pasue", "pause"),
    ("continoue", "continue"), ("contine", "continue"), ("continu", "continue"), ("contiune", "continue"), ("coninue", "continue"),
    ("continnue", "continue"),
    ("unmut", "unmute"), ("umute", "unmute"),
    // Power.
    ("shutdwn", "shutdown"), ("shudown", "shutdown"), ("shutdon", "shutdown"), ("shutdonw", "shutdown"), ("shtdown", "shutdown"),
    ("shutdowm", "shutdown"), ("restar", "restart"), ("restat", "restart"), ("rstart", "restart"), ("resart", "restart"),
    ("comupter", "computer"), ("compter", "computer"), ("computr", "computer"), ("copmuter", "computer"),
    // Jarvis' own views.
    ("systm", "system"), ("sytem", "system"), ("sistem", "system"), ("sysem", "system"), ("systen", "system"), ("sysytem", "system"),
    ("stast", "stats"), ("statts", "stats"), ("satts", "stats"),
    ("telemtry", "telemetry"), ("telemetery", "telemetry"),
    // Apps Sir names every day.
    ("spotfy", "spotify"), ("spotifiy", "spotify"), ("spotfiy", "spotify"), ("discrod", "discord"), ("dicord", "discord"), ("disocrd", "discord"),
];

/// Misspellings that are real words elsewhere ("Mut" is German for courage): corrected only when
/// they are the whole utterance.
const SHORT_ONLY: &[(&str, &str)] = &[("mut", "mute"), ("pos", "pause"), ("paws", "pause"), ("stp", "stop"), ("nxt", "next"), ("skp", "skip")];

/// Domain keywords a fuzzy match may correct to (six letters or more, so short words are safe).
const FUZZY_TARGETS: &[&str] = &[
    "calendar", "kalender", "appointment", "schedule", "tomorrow", "yesterday", "monday", "tuesday", "wednesday", "thursday", "friday",
    "saturday", "sunday", "montag", "dienstag", "mittwoch", "donnerstag", "freitag", "samstag", "sonntag", "übermorgen", "volume",
    "lautstärke", "shutdown", "restart", "continue", "telemetry", "statistics", "spotify", "discord", "computer",
];

/// Real words that look like a domain keyword and must never be "corrected".
const PROTECTED: &[&str] = &[
    "sundae", "showdown", "restore", "restored", "restores", "continent", "continental", "computed", "computes", "frida",
    "fridge", "monkey", "money", "mondo", "volumen", "voluntary", "shutout", "discard", "discards", "discarded", "discords", "spotty",
    "commuter", "commuters", "continuum", "dienste", "diensten",
    "telemetrie", "kalendarium", "statistic", "statistik", "montage", "freitagabend", "samstagabend", "sonntagabend",
];

/// "add a new in my calendar", "create a new in my kalender": the noun Sir left out.
static MISSING_NOUN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(add|create|make|put|schedule|set up)\s+(an?\s+)?(new)\s+(in|into|to|on|onto)\s+(my|the)\s+(calendar|kalender)\b")
        .expect("valid missing-noun regex")
});

/// Whether `word` is one fuzzy step away from `target` (see the module docs).
fn fuzzy_fix(word: &str) -> Option<&'static str> {
    let len = word.chars().count();
    if len < 5 || !word.chars().all(char::is_alphabetic) || PROTECTED.contains(&word) || FUZZY_TARGETS.contains(&word) {
        return None;
    }
    let mut best: Option<(&'static str, f64)> = None;
    for target in FUZZY_TARGETS {
        let tlen = target.chars().count();
        // Inflections and compounds of a keyword ("restarted", "Mondays", "Kalendereintrag").
        if word.starts_with(target) || len.abs_diff(tlen) > 2 {
            continue;
        }
        if !word.chars().take(2).eq(target.chars().take(2)) {
            continue;
        }
        let jw = strsim::jaro_winkler(word, target);
        let edits = strsim::damerau_levenshtein(word, target);
        let allowed = if len.max(tlen) >= 8 { 2 } else { 1 };
        if jw >= 0.84 && edits <= allowed && best.is_none_or(|(_, b)| jw > b) {
            best = Some((target, jw));
        }
    }
    best.map(|(t, _)| t)
}

/// `replacement` in the case shape of `original` ("Tommorw" → "Tomorrow", "CALENDER" → "CALENDAR").
fn same_case(original: &str, replacement: &str) -> String {
    if original.chars().all(|c| !c.is_alphabetic() || c.is_uppercase()) && original.chars().filter(|c| c.is_alphabetic()).count() > 1 {
        return replacement.to_uppercase();
    }
    if original.chars().next().is_some_and(char::is_uppercase) {
        let mut c = replacement.chars();
        return c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default();
    }
    replacement.to_string()
}

/// One word, its punctuation kept ("tommorw." → "tomorrow.").
fn fix_token(token: &str, whole_utterance: bool) -> Option<String> {
    let start = token.find(|c: char| c.is_alphanumeric())?;
    let end = token.rfind(|c: char| c.is_alphanumeric()).map(|i| i + token[i..].chars().next().map_or(1, char::len_utf8))?;
    let (lead, core, trail) = (&token[..start], &token[start..end], &token[end..]);
    // Paths, links, e-mail addresses, times and file names stay untouched.
    if core.contains(['/', '\\', '@', ':', '.', '_']) {
        return None;
    }
    let lower = core.to_lowercase();
    let fixed = KNOWN
        .iter()
        .find(|(typo, _)| *typo == lower)
        .map(|(_, word)| *word)
        .or_else(|| whole_utterance.then(|| SHORT_ONLY.iter().find(|(typo, _)| *typo == lower).map(|(_, word)| *word)).flatten())
        .or_else(|| fuzzy_fix(&lower))?;
    Some(format!("{lead}{}{trail}", same_case(core, fixed)))
}

/// A view verb, an optional "me", then "your" (any near-miss of "core" after it) …
static CORE_AFTER_YOUR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(show|open|hide|close|bring up|bring back|pull up|display|rotate|spin|resize|expand|shrink|enlarge|collapse|minimi[sz]e|maximi[sz]e|zeig|zeige|öffne|oeffne|schließ|schließe|schliess|schliesse|versteck|verstecke)(\s+(?:me|mir)\b)?\s+(your|ur|yo|dein|deinen)\s+(?:(?:for|four|the)\s+)?(c[\s.\-]*o[\s.\-]*r[\s.\-]*e|cars?|calls?|cords?|corps?|chores?|kor|chor|kohr|kore|coar|cor|kern|orb|reactor)\b(\s+mode\b)?")
        .expect("valid core-homophone regex")
});
/// … or "the" / "my" / "den" before the unmistakable near-misses only ("close the call" may be a
/// real call, "close the c-o-r-e" is not).
static CORE_AFTER_THE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(show|open|hide|close|bring up|bring back|pull up|display|rotate|spin|resize|zeig|zeige|öffne|schließ|schließe|versteck|verstecke)(\s+(?:me|mir)\b)?\s+(the|my|den)\s+(?:for\s+)?(c[\s.\-]+o[\s.\-]+r[\s.\-]+e|corps|kor|chor|kohr|kore|coar)\b")
        .expect("valid core-homophone regex")
});
/// "C-O-R-E" / "c o r e" / "C.O.R.E." spelt out anywhere, with Whisper's "for" in front.
static CORE_SPELT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:for\s+)?c[\s.\-]+o[\s.\-]+r[\s.\-]+e\b\.?").expect("valid spelt-core regex")
});

/// Whisper's near-misses of "core" next to a view verb become "core": "close your car", "hide
/// your call", "hide your for C-O-R-E", "show your cord", "schließ deinen Chor". Jarvis is a
/// desktop AI: with "show / hide / close / open your …" Sir never means a vehicle or a phone
/// call. "Only show the orb" and "orb mode" stay the pure orb mode.
pub fn core_homophones(text: &str) -> String {
    let lower = text.to_lowercase();
    let orb_mode = lower.contains("mode") || lower.contains("only") || lower.contains("just");
    let out = CORE_AFTER_YOUR.replace_all(text, |c: &regex::Captures| {
        let word = c[4].to_lowercase();
        if (word == "orb" && orb_mode) || c.get(5).is_some() {
            return c[0].to_string();
        }
        let core = if c[3].eq_ignore_ascii_case("dein") || c[3].eq_ignore_ascii_case("deinen") { "Kern" } else { "core" };
        format!("{}{} {} {core}", &c[1], c.get(2).map_or("", |m| m.as_str()), &c[3])
    });
    let out = CORE_AFTER_THE.replace_all(&out, |c: &regex::Captures| format!("{}{} {} core", &c[1], c.get(2).map_or("", |m| m.as_str()), &c[3]));
    let out = CORE_SPELT.replace_all(&out, "core");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Sir's sentence with Jarvis' domain words spelt right (unchanged when nothing needed fixing).
pub fn normalize_user_input_typos(text: &str) -> String {
    let core = core_homophones(text);
    if core != text.split_whitespace().collect::<Vec<_>>().join(" ") {
        return normalize_user_input_typos(&core);
    }
    let trimmed = text.trim();
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    // Code, pasted text and long dictation are left exactly as written.
    if tokens.is_empty() || tokens.len() > 40 || trimmed.contains("```") {
        return text.to_string();
    }
    let whole = tokens.len() <= 2;
    let mut changed = false;
    let words: Vec<String> = tokens
        .iter()
        .map(|t| match fix_token(t, whole) {
            Some(fixed) => {
                changed = true;
                fixed
            }
            None => t.to_string(),
        })
        .collect();
    let joined = if changed { words.join(" ") } else { trimmed.to_string() };
    let repaired = MISSING_NOUN.replace(&joined, "$1 $2$3 event $4 $5 $6").to_string();
    let repaired = repaired.split_whitespace().collect::<Vec<_>>().join(" ");
    if !changed && repaired == trimmed.split_whitespace().collect::<Vec<_>>().join(" ") {
        return text.to_string();
    }
    repaired
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_sentence_is_repaired() {
        assert_eq!(
            normalize_user_input_typos("add a new in my calender called test for 15h tommorw"),
            "add a new event in my calendar called test for 15h tomorrow"
        );
    }

    #[test]
    fn known_misspellings() {
        assert_eq!(normalize_user_input_typos("Whats in my Calender tomorow?"), "Whats in my Calendar tomorrow?");
        assert_eq!(normalize_user_input_typos("set the volum to 40"), "set the volume to 40");
        assert_eq!(normalize_user_input_typos("shudown my pc"), "shutdown my pc");
        assert_eq!(normalize_user_input_typos("open my systm stats"), "open my system stats");
        assert_eq!(normalize_user_input_typos("continoue"), "continue");
        assert_eq!(normalize_user_input_typos("paus"), "pause");
        assert_eq!(normalize_user_input_typos("yestaday on wednsday"), "yesterday on wednesday");
        assert_eq!(normalize_user_input_typos("trag morgn um 15 uhr test in meinen kalender ein"), "trag morgen um 15 uhr test in meinen kalender ein");
    }

    #[test]
    fn short_only_words_need_the_whole_utterance() {
        assert_eq!(normalize_user_input_typos("mut"), "mute");
        assert_eq!(normalize_user_input_typos("Mut."), "Mute.");
        assert_eq!(normalize_user_input_typos("ich habe keinen mut mehr"), "ich habe keinen mut mehr");
    }

    #[test]
    fn fuzzy_corrections_stay_narrow() {
        assert_eq!(normalize_user_input_typos("add it to my calendqr"), "add it to my calendar");
        assert_eq!(normalize_user_input_typos("next tuesdya"), "next tuesday");
        for untouched in [
            "restore the window", "I had a sundae", "the showdown starts", "the continent is big", "restarted it",
            "every Mondays meeting", "money matters", "open C:/Users/calender.txt", "meet me at 15:00",
        ] {
            assert_eq!(normalize_user_input_typos(untouched), untouched, "{untouched}");
        }
    }

    #[test]
    fn untouched_text_keeps_its_spacing() {
        assert_eq!(normalize_user_input_typos("  hello   there "), "  hello   there ");
    }

    #[test]
    fn misheard_core_becomes_core_next_to_a_view_verb() {
        // Sir's transcript (image_1ffda8): every near-miss of "core".
        assert_eq!(core_homophones("Close your car."), "Close your core.");
        assert_eq!(core_homophones("No. Close your car."), "No. Close your core.");
        assert_eq!(core_homophones("Right, hide your call."), "Right, hide your core.");
        assert_eq!(core_homophones("Hide your for C-O-R-E."), "Hide your core.");
        assert_eq!(core_homophones("hide your c o r e"), "hide your core");
        for heard in ["show your car", "open your call", "hide your cord", "close your corps", "show your chore", "hide your kor", "close your chor", "show your kohr", "close the kohr", "show me your orb", "hide your reactor"] {
            assert!(core_homophones(heard).ends_with(" core"), "{heard} → {}", core_homophones(heard));
        }
        assert_eq!(core_homophones("Schließ deinen Chor"), "Schließ deinen Kern");
        // Ordinary sentences and the orb mode stay as they are.
        for keep in ["Park the car in the garage", "Call my mom", "close the call", "only show the orb", "switch to orb mode", "show your cards", "the car is red"] {
            assert_eq!(core_homophones(keep), keep);
        }
        // The full normaliser applies it before routing.
        assert_eq!(normalize_user_input_typos("Close your car."), "Close your core.");
        assert_eq!(normalize_user_input_typos("hide your for C-O-R-E"), "hide your core");
    }
}
