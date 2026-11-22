//! Typo-tolerant input: the misspellings Sir types and Whisper writes for JAI's own domain
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
    // JAI's own views.
    ("systm", "system"), ("sytem", "system"), ("sistem", "system"), ("sysem", "system"), ("systen", "system"), ("sysytem", "system"),
    ("stast", "stats"), ("statts", "stats"), ("satts", "stats"),
    ("telemtry", "telemetry"), ("telemetery", "telemetry"),
    // ("Open the admin pannel", Sir's own typing, 2026-11-21.)
    ("pannel", "panel"), ("pannels", "panels"), ("panle", "panel"),
    // Apps Sir names every day.
    ("spotfy", "spotify"), ("spotifiy", "spotify"), ("spotfiy", "spotify"), ("discrod", "discord"), ("dicord", "discord"), ("disocrd", "discord"),
    // The verbs and words of every other command.
    ("opn", "open"), ("oepn", "open"), ("opne", "open"), ("clse", "close"), ("cloes", "close"), ("colse", "close"), ("clsoe", "close"),
    ("shwo", "show"), ("hsow", "show"), ("teh", "the"), ("hte", "the"), ("yur", "your"), ("yuor", "your"), ("trun", "turn"), ("tunr", "turn"),
    ("lowr", "lower"), ("lwoer", "lower"), ("loewr", "lower"), ("mvoe", "move"), ("moev", "move"),
    ("swich", "switch"), ("swtich", "switch"), ("switsh", "switch"),
    ("evrything", "everything"), ("everthing", "everything"), ("everyhting", "everything"), ("evreything", "everything"),
    ("feture", "feature"), ("featur", "feature"), ("fature", "feature"), ("feautre", "feature"), ("feautres", "features"), ("fetures", "features"),
    ("stauts", "status"), ("sttus", "status"), ("satus", "status"), ("whta", "what"), ("waht", "what"), ("hwat", "what"),
    ("thme", "theme"), ("tehme", "theme"), ("theem", "theme"), ("windwo", "window"), ("widnow", "window"), ("wnidow", "window"),
    ("widgte", "widget"), ("mixr", "mixer"), ("quiter", "quieter"), ("quiteer", "quieter"),
    ("musci", "music"), ("muisc", "music"), ("msuic", "music"), ("plya", "play"), ("timre", "timer"), ("tiemr", "timer"),
    // "Stop listeting" (2026-11-07, Sir's own typing).
    ("listeting", "listening"), ("listning", "listening"), ("lisening", "listening"), ("listenning", "listening"), ("listenig", "listening"),
    ("lisetning", "listening"), ("litening", "listening"),
];

/// Misspellings that are real words elsewhere ("Mut" is German for courage, "Staus" are traffic
/// jams): corrected only when they are the whole utterance.
const SHORT_ONLY: &[(&str, &str)] =
    &[("mut", "mute"), ("pos", "pause"), ("paws", "pause"), ("stp", "stop"), ("nxt", "next"), ("skp", "skip"), ("staus", "status")];

/// Domain keywords a fuzzy match may correct to (six letters or more, so short words are safe).
const FUZZY_TARGETS: &[&str] = &[
    "calendar", "kalender", "appointment", "schedule", "tomorrow", "yesterday", "monday", "tuesday", "wednesday", "thursday", "friday",
    "saturday", "sunday", "montag", "dienstag", "mittwoch", "donnerstag", "freitag", "samstag", "sonntag", "übermorgen", "volume",
    "lautstärke", "shutdown", "restart", "continue", "telemetry", "statistics", "spotify", "discord", "computer", "everything", "feature",
    "features", "widget", "widgets", "status", "switch",
];

/// Real words that look like a domain keyword and must never be "corrected".
const PROTECTED: &[&str] = &[
    "sundae", "showdown", "restore", "restored", "restores", "continent", "continental", "computed", "computes", "frida",
    "fridge", "monkey", "money", "mondo", "volumen", "voluntary", "shutout", "discard", "discards", "discarded", "discords", "spotty",
    "commuter", "commuters", "continuum", "dienste", "diensten",
    "telemetrie", "kalendarium", "statistic", "statistik", "montage", "freitagabend", "samstagabend", "sonntagabend",
    "stats", "statue", "statues", "statur", "statut", "stratus", "staus", "states", "stated", "swatch", "feathers", "fetuses",
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
/// your call", "hide your for C-O-R-E", "show your cord", "schließ deinen Chor". JAI is a
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

/// "Would you mind opening the feature lab?" is "open the feature lab" (2026-11-07: the model
/// took it for a protocol). Only verbs JAI acts on; anything else is left as said.
fn polite_gerund(text: &str) -> Option<String> {
    static POLITE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:(?:hey\s+)?jai[,.]?\s+)?(?:would|do)\s+you\s+mind\s+([a-z]+ing)\b(.*)$").expect("polite gerund"));
    const VERBS: &[(&str, &str)] = &[
        ("opening", "open"), ("closing", "close"), ("turning", "turn"), ("muting", "mute"), ("unmuting", "unmute"), ("playing", "play"),
        ("pausing", "pause"), ("stopping", "stop"), ("starting", "start"), ("showing", "show"), ("hiding", "hide"), ("putting", "put"),
        ("making", "make"), ("setting", "set"), ("switching", "switch"), ("taking", "take"), ("moving", "move"), ("lowering", "lower"),
        ("raising", "raise"), ("skipping", "skip"), ("saving", "save"), ("loading", "load"), ("deleting", "delete"), ("removing", "remove"),
        ("adding", "add"), ("checking", "check"), ("reading", "read"), ("telling", "tell"), ("giving", "give"), ("bringing", "bring"),
        ("launching", "launch"), ("running", "run"), ("restarting", "restart"), ("changing", "change"), ("getting", "get"), ("keeping", "keep"),
        ("fading", "fade"), ("toggling", "toggle"), ("minimizing", "minimize"), ("maximizing", "maximize"), ("dimming", "dim"), ("resetting", "reset"),
        ("sending", "send"), ("writing", "write"), ("creating", "create"), ("finding", "find"), ("searching", "search"), ("looking", "look"),
        ("quitting", "quit"), ("leaving", "leave"), ("entering", "enter"), ("going", "go"), ("coming", "come"), ("listening", "listen"),
    ];
    let c = POLITE.captures(text.trim())?;
    let gerund = c[1].to_lowercase();
    let verb = VERBS.iter().find(|(ing, _)| *ing == gerund)?.1;
    let rest = c[2].trim_end().trim_end_matches(['?', '.', '!']).trim_end();
    Some(format!("{verb}{rest}"))
}

/// Commands as the recogniser mishears them (2026-11-07 verifier, matrix F: stock voices said
/// them, Whisper wrote "JAI Mude", "Java's Pulse", "monitor too", "start stop watch", "is
/// anything mutant", "Tugglespan mode", "was playing"). Whole commands and fixed pairs only,
/// none of them a sentence anyone means as it is written.
fn misheard(text: &str) -> Option<String> {
    let said = text.trim().trim_start_matches('\u{feff}').trim_end_matches(['.', '!', '?']).trim_end();
    let whole = match said.to_lowercase().as_str() {
        "mude" | "mewed" | "mewt" | "moot" => Some("mute"),
        "pulse" | "poles" | "paws" => Some("pause"),
        "was playing" => Some("what's playing"),
        "was open" | "watts open" | "what's opened" => Some("what's open"),
        "watts playing" => Some("what's playing"),
        _ => None,
    };
    if let Some(command) = whole {
        return Some(command.to_string());
    }
    static PAIRS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
        [
            (r"(?i)\b(monitor|screen|display)\s+(?:too|to)$", "$1 two"),
            // "Füge einen Termin hinzu" heard as "Flüge einen …" (only before an article: "Flüge
            // nach Berlin" stays the flights).
            (r"(?i)^flüge\s+(einen|ein|eine)\s+", "füge $1 "),
            (r"(?i)\bstop\s+watch\b", "stopwatch"),
            (r"(?i)\bsystems\s+status\b", "system status"),
            (r"(?i)^(is|are)\s+(.{1,30}?)\s+mutant$", "$1 $2 muted"),
            (r"(?i)\b(?:tuggle|tugle)[\s-]?span\b|\btoggle-span\b", "toggle span"),
            (r"(?i)^(stop|start|end|quit)\s+panning\b", "$1 spanning"),
            (r"(?i)^(save|load)\s+(this|the|my)\s+mix(?:'s|er's|\s+his|\s+is)\s+", "$1 $2 mix as "),
            (r"(?i)\bmonitons?\b", "monitors"),
            (r"(?i)\b(?:five\s+ocks|fire\s+fox)\b", "Firefox"),
            // "Set Discord to eighty percent" heard as "said Discord the 80%" and "said this
            // called the 80%" (only there: "what's this called?" stays a question).
            (r"(?i)^(?:set|said)\s+this\s+(?:called|cord|chord)\s+(?:the|to|at)\s+(\d{1,3})\s*(?:%|percent|prozent)$", "set discord to $1 %"),
            (r"(?i)^said\s+(.+?)\s+(?:the|to|at)\s+(\d{1,3})\s*(?:%|percent|prozent)$", "set $1 to $2 %"),
        ]
        .into_iter()
        .map(|(pattern, with)| (Regex::new(pattern).expect("misheard pair"), with))
        .collect()
    });
    let mut fixed = said.to_string();
    for (pattern, with) in PAIRS.iter() {
        fixed = pattern.replace_all(&fixed, *with).into_owned();
    }
    (fixed != said).then_some(fixed)
}

/// What follows his name, as an order: with "JAI" heard as the subject the recogniser writes
/// the order as a report ("JAI opened the mixer", "JAI stops spanning", "JAI saved
/// this mix as gaming"). Only verbs he takes orders with, only the first word, and only when
/// something follows it. (The wake word calls this for what comes after his name.)
pub fn imperative_after_name(command: &str) -> String {
    const VERBS: &[(&str, &str)] = &[
        ("opened", "open"), ("opens", "open"), ("closed", "close"), ("closes", "close"), ("saved", "save"), ("saves", "save"),
        ("loaded", "load"), ("loads", "load"), ("stopped", "stop"), ("stops", "stop"), ("started", "start"), ("starts", "start"),
        ("muted", "mute"), ("mutes", "mute"), ("unmuted", "unmute"), ("unmutes", "unmute"), ("showed", "show"), ("shows", "show"),
        ("hid", "hide"), ("hides", "hide"), ("moved", "move"), ("moves", "move"), ("turned", "turn"), ("turns", "turn"),
        ("switched", "switch"), ("switches", "switch"), ("toggled", "toggle"), ("toggles", "toggle"), ("paused", "pause"),
        ("pauses", "pause"), ("lowered", "lower"), ("lowers", "lower"), ("raised", "raise"), ("raises", "raise"), ("removed", "remove"),
        ("removes", "remove"), ("added", "add"), ("adds", "add"), ("sets", "set"), ("minimized", "minimize"), ("minimizes", "minimize"),
        ("maximized", "maximize"), ("maximizes", "maximize"), ("cleared", "clear"), ("clears", "clear"), ("faded", "fade"), ("fades", "fade"),
        ("skipped", "skip"), ("skips", "skip"), ("resumed", "resume"), ("resumes", "resume"), ("spans", "span"), ("spanned", "span"),
        // (Not "played", "made", "woke", "copied" or "locked": "Jay, I played football today" and
        // the like are told, not ordered.)
        ("liked", "like"), ("likes", "like"), ("plays", "play"), ("activated", "activate"), ("activates", "activate"),
        ("enabled", "enable"), ("enables", "enable"), ("disabled", "disable"), ("disables", "disable"), ("makes", "make"),
        ("deleted", "delete"), ("deletes", "delete"), ("renamed", "rename"), ("renames", "rename"), ("printed", "print"), ("prints", "print"),
        ("zipped", "zip"), ("zips", "zip"), ("copies", "copy"), ("locks", "lock"),
        ("translated", "translate"), ("translates", "translate"), ("plotted", "plot"), ("plots", "plot"), ("wakes", "wake"),
        ("öffnet", "öffne"), ("schließt", "schließe"), ("schliesst", "schliesse"), ("zeigt", "zeig"), ("startet", "starte"),
        ("stoppt", "stopp"), ("beendet", "beende"),
    ];
    let trimmed = command.trim_start();
    let Some((first, rest)) = trimmed.split_once(char::is_whitespace) else {
        return command.to_string();
    };
    let bare = first.trim_end_matches([',', '.', '!', '?', ';', ':']);
    let lower = bare.to_lowercase();
    match VERBS.iter().find(|(said, _)| *said == lower) {
        Some((_, verb)) if !rest.trim().is_empty() => {
            let mut letters = verb.chars();
            let verb: String = if bare.chars().next().is_some_and(char::is_uppercase) {
                letters.next().map_or(String::new(), |c| c.to_uppercase().chain(letters).collect())
            } else {
                verb.to_string()
            };
            format!("{verb} {}", rest.trim_start())
        }
        _ => command.to_string(),
    }
}

/// Sir's sentence with JAI's domain words spelt right (unchanged when nothing needed fixing).
pub fn normalize_user_input_typos(text: &str) -> String {
    // "Open Ch-Chrome, uh, I mean Firefox" is "Open Firefox": said once, as finally meant.
    if let Some(meant) = crate::speech_repair::repaired(text) {
        return normalize_user_input_typos(&meant);
    }
    if let Some(order) = polite_gerund(text) {
        return normalize_user_input_typos(&order);
    }
    if let Some(heard) = misheard(text) {
        return normalize_user_input_typos(&heard);
    }
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

    /// Repro (verifier matrix F, 2026-11-07): what the recogniser wrote for stock voices.
    #[test]
    fn commands_as_the_recogniser_mishears_them() {
        for (heard, meant) in [
            ("Mude.", "mute"),
            ("Mewed.", "mute"),
            ("Pulse.", "pause"),
            ("Poles.", "pause"),
            ("was playing.", "what's playing"),
            ("was open.", "what's open"),
            ("move yourself to monitor too.", "move yourself to monitor two"),
            ("start stop watch.", "start stopwatch"),
            ("systems status.", "system status"),
            ("is anything mutant.", "is anything muted"),
            ("Tugglespan Mode", "toggle span Mode"),
            ("said discord the 80%.", "set discord to 80 %"),
            ("said this called the 80%.", "set discord to 80 %"),
            ("stop panning.", "stop spanning"),
            ("save this mix's gaming.", "save this mix as gaming"),
            // 2026-11-11, the same sentences after his name became one syllable.
            ("save this mix his gaming.", "save this mix as gaming"),
            ("save this mixer's gaming.", "save this mix as gaming"),
            ("Watts Open.", "what's open"),
            ("use all Monitons.", "use all monitors"),
            ("turn five ocks off.", "turn Firefox off"),
        ] {
            assert_eq!(normalize_user_input_typos(heard), normalize_user_input_typos(meant), "{heard}");
        }
        // As it is, where it is a sentence of its own.
        for said in ["The pulse is fine", "I paused it", "Is the mutant movie good?", "said nothing", "Stop watching me", "What's this called?", "What's this mix's name?"] {
            assert_eq!(misheard(said), None, "{said}");
        }
    }

    #[test]
    fn the_order_after_his_name_is_an_order() {
        assert_eq!(imperative_after_name("opened the mixer."), "open the mixer.");
        assert_eq!(imperative_after_name("stops spanning."), "stop spanning.");
        assert_eq!(imperative_after_name("saved this mix as gaming."), "save this mix as gaming.");
        assert_eq!(imperative_after_name("Öffnet den Mixer"), "Öffne den Mixer");
        // Nothing after the verb, or not one of his verbs: as heard.
        for heard in ["muted?", "played football today", "was playing", "is anything muted", "open the mixer"] {
            assert_eq!(imperative_after_name(heard), heard, "{heard}");
        }
    }

    #[test]
    fn an_order_in_good_manners_is_the_order() {
        assert_eq!(normalize_user_input_typos("Would you mind opening the feature lab?"), "open the feature lab");
        assert_eq!(normalize_user_input_typos("JAI, do you mind turning it down?"), "turn it down");
        assert_eq!(normalize_user_input_typos("Would you mind closing Firefox"), "close Firefox");
        // Not one of his verbs: as said.
        assert_eq!(normalize_user_input_typos("Would you mind dancing?"), "Would you mind dancing?");
        assert_eq!(normalize_user_input_typos("Would you mind if I left?"), "Would you mind if I left?");
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
        assert_eq!(normalize_user_input_typos("opn the feture lab"), "open the feature lab");
        assert_eq!(normalize_user_input_typos("clse the feature lab"), "close the feature lab");
        assert_eq!(normalize_user_input_typos("shwo me your core"), "show me your core");
        assert_eq!(normalize_user_input_typos("lowr discord by 10 percent"), "lower discord by 10 percent");
        assert_eq!(normalize_user_input_typos("whats teh time"), "whats the time");
        assert_eq!(normalize_user_input_typos("move yur orb to the left"), "move your orb to the left");
        assert_eq!(normalize_user_input_typos("mute evrything except discord"), "mute everything except discord");
        // (It found no program "Admin Pannel", 2026-11-21: corrected, it is his admin window.)
        let admin = normalize_user_input_typos("open the admin pannel");
        assert!(crate::phrasings::route(&admin).is_some_and(|(tool, _)| tool == "admin"), "{admin}");
    }

    #[test]
    fn short_only_words_need_the_whole_utterance() {
        assert_eq!(normalize_user_input_typos("mut"), "mute");
        assert_eq!(normalize_user_input_typos("Mut."), "Mute.");
        assert_eq!(normalize_user_input_typos("ich habe keinen mut mehr"), "ich habe keinen mut mehr");
        assert_eq!(normalize_user_input_typos("system staus"), "system status");
        assert_eq!(normalize_user_input_typos("gibt es heute viele staus auf der autobahn"), "gibt es heute viele staus auf der autobahn");
    }

    #[test]
    fn fuzzy_corrections_stay_narrow() {
        assert_eq!(normalize_user_input_typos("add it to my calendqr"), "add it to my calendar");
        assert_eq!(normalize_user_input_typos("next tuesdya"), "next tuesday");
        for untouched in [
            "restore the window", "I had a sundae", "the showdown starts", "the continent is big", "restarted it",
            "every Mondays meeting", "money matters", "open C:/Users/calender.txt", "meet me at 15:00",
            "the united states", "a statue of liberty", "system stats", "a swatch watch", "birds of a feather", "white feathers",
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
