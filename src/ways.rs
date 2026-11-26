//! The many ways of saying one thing.
//!
//! Every order was written for a wording or two ("cancel the sleep timer"). People do not keep
//! to a wording: "could you kill that sleep timer", "I don't need the sleep timer any more",
//! "Schlaftimer aus", "is my sleep timer still running?". Here a sentence is taken for what it
//! is about (its ideas: a subject and an intent, in any order, in English or German) or for one
//! of many whole-sentence shapes, and turned into the wording the order was written for
//! (`wordings`). The fast path then reads that as it always did (`brain::another_way`), so
//! there is one place where an order's arguments are made, and an order said another way does
//! exactly what its own wording does.
//!
//! It only ever adds: a sentence the fast path understands as it stands is never reworded
//! (`brain::another_way` asks first), with one exception, the rules that take any words for
//! the name of an app or a window ("start the dictation" was an app called "dictation").
//!
//! What keeps it from taking talk for orders: a sentence that tells of something (yesterday,
//! in class, on my phone, he said) or asks about the world is nobody's order (`TOLD`, `WORLD`,
//! `PEOPLE`), an idea needs its subject's own noun, and what is vague on its own ("that's
//! enough", "the rest") means an order only while the thing it is about is going on.

use regex::Regex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::LazyLock;

/// What frames a request, and what is left of it: nothing ("could you ", "tell me "), or, for
/// a wish, the one verb every order that shows or gives something knows ("I need the mixer",
/// "can I see the settings", "ich hätte gern …" are "give me …"). Longest first.
static FRAMES: LazyLock<Vec<(&'static str, &'static str)>> = LazyLock::new(|| {
    const NOTHING: &[&str] = &[
        "i want you to ", "i need you to ", "i would like you to ", "id like you to ", "i wanna ", "i want to ", "i would like to ", "id like to ", "i need to ", "i have to ", "i gotta ",
        "can we ", "could we ", "lets ", "let us ", "go ahead and ", "just ", "quickly ", "would you mind ", "do you mind ", "mind ", "can you ", "could you ", "would you ", "will you ",
        "you can ", "you may ", "you should ", "time to ", "its time to ", "how about you ", "be so kind and ", "do me a favor and ", "do me a favour and ", "i think you should ", "okay so ",
        "ok so ", "alright so ", "okay ", "ok ", "alright ", "hey ", "yo ", "so ", "well ", "now ", "then ", "maybe ", "actually ", "please ", "kindly ", "quick ", "real quick ",
        "i was wondering if you could ", "is it possible to ", "is there a way to ", "any chance you could ", "do you think you could ", "would you be able to ", "are you able to ", "we need to ",
        "you need to ", "we should ", "if you could ", "wait ", "oh ", "and ", "also ", "from now on ", "can i ", "could i ", "may i ",
        // (What is asked in passing: "could you tell me where the subtitles are".)
        "tell me ", "i want to know ", "i need to know ", "id like to know ", "i would like to know ", "do you know ", "let me know ", "i wonder ",
        "kannst du mir ", "kannst du ", "könntest du mir ", "könntest du ", "würdest du mir ", "würdest du ", "magst du ", "ich möchte dass du ", "ich will dass du ", "ich möchte dass ",
        "ich will dass ", "ich muss ", "lass uns ", "lass mal ", "sei so gut und ", "ey ", "okay also ", "dann ", "jetzt ", "bitte ", "mal ", "eben ", "kurz ", "können wir ", "könnten wir ",
        "kann ich ", "könnte ich ", "darf ich ", "wir müssen ", "du kannst ", "du sollst ", "du musst ", "warte ", "ach ", "und ", "sag mir ", "mir sagen ", "ich will wissen ", "ich möchte wissen ",
        "weißt du ", "mir zeigen ", "ich will sehen ", "ich möchte sehen ", "dir merken ", "einstellen dass ", "dass ", "mir mal ", "mir ",
    ];
    const WISHES: &[&str] = &[
        "can i get ", "can i have ", "could i get ", "could i have ", "may i have ", "can i see ", "could i see ", "may i see ", "i want to see ", "i want to have ", "i want to hear ",
        "i would like to see ", "i would like to have ", "id like to see ", "id like to have ", "i need to see ", "i wanna see ", "let me see ", "let me have ", "i want ", "i need ",
        "i would like ", "id like ", "ich will ", "ich möchte ", "ich brauche ", "ich hätte ", "ich würde gerne ", "ich würde gern ",
    ];
    let mut frames: Vec<(&str, &str)> = NOTHING.iter().map(|f| (*f, "")).chain(WISHES.iter().map(|f| (*f, "give me "))).collect();
    frames.sort_by_key(|(frame, _)| std::cmp::Reverse(frame.len()));
    frames
});

/// A request without what only frames it: "could you maybe start the dictation for me real
/// quick" is "start the dictation", "I need the mixer" is "give me the mixer".
/// (`intents::normalize` took the commonest frames already.)
pub fn bare(s: &str) -> String {
    const TAIL: &[&str] = &[
        " please", " for me", " now", " right now", " right", " just", " real quick", " quickly", " if you can", " if possible", " thanks", " thank you", " would be nice", " would be great",
        " for a sec", " for a second", " will you", " would you", " can you", " okay", " ok", " alright", " again please", " bitte", " mal", " für mich", " jetzt", " sofort", " schnell", " danke",
        " wenn du kannst", " wenn möglich", " ja", " thx", " mate", " bro", " man", " dude", " digga", " alter", " already", " then", " dann", " cheers", " if you dont mind", " if you would",
        " when you can", " for a moment", " i guess", " i think", " or something", " from now on", " here", " hier",
    ];
    // (Hesitations are no words of an order, wherever they stand; a hyphen joins two; what is
    // clipped in speech is the word it was.)
    let mut s: String = s
        .replace('-', " ")
        .split_whitespace()
        .filter(|w| !matches!(*w, "uh" | "uhm" | "um" | "umm" | "erm" | "äh" | "ähm" | "öhm" | "hm" | "hmm" | "gern" | "gerne"))
        .map(|w| match w {
            "hab" => "habe",
            "brauch" => "brauche",
            "hätt" => "hätte",
            "nen" => "einen",
            "nem" => "einem",
            "whos" => "who is",
            "wheres" => "where is",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let asked_to_mind = s.contains("you mind ") || s.starts_with("mind ");
    for _ in 0..8 {
        let before = s.clone();
        if let Some((frame, left)) = FRAMES.iter().find(|(frame, _)| s.starts_with(frame)) {
            s = format!("{left}{}", &s[frame.len()..]);
        }
        for tail in TAIL {
            if let Some(rest) = s.strip_suffix(tail) {
                s = rest.to_string();
            }
        }
        // ("Ich will meine Notizen sehen": what a wish ends on in German.)
        if s.starts_with("give me ") {
            for tail in [" sehen", " haben", " hören"] {
                if let Some(rest) = s.strip_suffix(tail) {
                    s = rest.to_string();
                }
            }
        }
        if s == before {
            break;
        }
    }
    // "Would you mind opening …", "… only taking commands from me": the verb in its plain form.
    if asked_to_mind {
        let words: Vec<&str> = s.split(' ').collect();
        if let Some(at) = words.iter().take(2).position(|w| plain_verb(w).is_some()) {
            let mut turned: Vec<&str> = words.clone();
            turned[at] = plain_verb(words[at]).unwrap_or(words[at]);
            s = turned.join(" ");
        }
    }
    s.trim().to_string()
}

/// "Meine Notizen aufmachen", "das Diktat starten", "die Helligkeit runterdrehen": German puts
/// the verb last where it asks ("kannst du … aufmachen") or names what is to be done. The same
/// with the verb first, as an order has it ("öffne meine notizen", "dreh die helligkeit runter").
fn verb_first(s: &str) -> Option<String> {
    const LAST: &[(&str, &str)] = &[
        ("starten", "starte"), ("öffnen", "öffne"), ("aufmachen", "öffne"), ("anzeigen", "zeig"), ("zeigen", "zeig"), ("einblenden", "zeig"), ("schließen", "schließe"), ("zumachen", "schließe"),
        ("beenden", "beende"), ("stoppen", "stoppe"), ("anhalten", "stoppe"), ("pausieren", "pausiere"), ("löschen", "lösche"), ("entfernen", "entferne"), ("einschalten", "aktiviere"),
        ("anschalten", "aktiviere"), ("anmachen", "aktiviere"), ("aktivieren", "aktiviere"), ("ausschalten", "deaktiviere"), ("abschalten", "deaktiviere"), ("ausmachen", "deaktiviere"),
        ("deaktivieren", "deaktiviere"), ("speichern", "speichere"), ("minimieren", "minimiere"), ("wechseln", "wechsle"), ("machen", "mach"), ("stellen", "stell"), ("setzen", "setz"),
        ("drehen", "dreh"), ("spielen", "spiel"), ("abspielen", "spiel"), ("scrollen", "scrolle"), ("gehen", "geh"), ("springen", "spring"), ("laden", "lade"), ("drücken", "drück"),
        ("schreiben", "schreib"), ("tippen", "tipp"), ("suchen", "such"), ("finden", "finde"), ("lernen", "lerne"), ("vergessen", "vergiss"), ("sichern", "sichere"), ("erklären", "erkläre"),
        ("übersetzen", "übersetze"), ("dimmen", "dimme"), ("verschieben", "verschiebe"), ("schieben", "schieb"), ("erstellen", "erstelle"), ("notieren", "notiere"), ("aufschreiben", "notiere"),
        ("ändern", "ändere"), ("überspringen", "überspringe"), ("trainieren", "trainiere"), ("üben", "übe"), ("wiederherstellen", "stelle wieder her"), ("zoomen", "zoome"), ("geben", "gib"),
        ("holen", "hol"), ("bringen", "bring"), ("sagen", "sag"), ("nennen", "nenn"), ("auflisten", "liste"), ("erhöhen", "erhöhe"), ("verringern", "verringere"), ("merken", "merk dir"),
        ("abbrechen", "stoppe"), ("fortsetzen", "setze fort"), ("leeren", "leere"), ("reagieren", "reagiere"), ("schalten", "schalte"), ("erlauben", "erlaube"), ("behalten", "behalte"),
        ("werden", "werde"), ("packen", "pack"), ("hören", "hör"), ("gehorchen", "gehorche"), ("lesen", "lies"),
    ];
    // (What comes apart in an order: "runterdrehen" is "dreh … runter".)
    const PARTS: &[&str] = &["runter", "hoch", "rauf", "herunter", "zurück", "weiter", "zusammen", "vor", "ein", "aus", "an", "auf", "zu", "ab", "weg", "mit", "raus", "rein", "hin", "her", "um"];
    const STEMS: &[(&str, &str)] = &[
        ("drehen", "dreh"), ("machen", "mach"), ("stellen", "stell"), ("schalten", "schalte"), ("setzen", "setz"), ("fahren", "fahr"), ("regeln", "regle"), ("schieben", "schieb"),
        ("blenden", "blende"), ("lesen", "lies"), ("geben", "gib"), ("holen", "hol"), ("nehmen", "nimm"), ("gehen", "geh"), ("spielen", "spiel"), ("zoomen", "zoome"), ("scrollen", "scrolle"),
        ("laden", "lade"), ("schreiben", "schreib"), ("tippen", "tipp"), ("hören", "hör"), ("laufen", "lauf"), ("springen", "spring"), ("wechseln", "wechsle"), ("fassen", "fass"),
        ("legen", "leg"), ("packen", "pack"), ("docken", "docke"), ("fügen", "füge"), ("zählen", "zähl"),
    ];
    let (rest, last) = s.rsplit_once(' ')?;
    if let Some((_, verb)) = LAST.iter().find(|(infinitive, _)| *infinitive == last) {
        // ("dir die stimme … merken" is "merk dir die stimme …": one "dir" is enough.)
        let rest = if verb.ends_with(" dir") { rest.strip_prefix("dir ").unwrap_or(rest) } else { rest };
        return Some(format!("{verb} {rest}"));
    }
    let (part, verb) = PARTS.iter().find_map(|part| last.strip_prefix(part).and_then(|stem| STEMS.iter().find(|(infinitive, _)| *infinitive == stem)).map(|(_, verb)| (*part, *verb)))?;
    Some(format!("{verb} {rest} {part}"))
}

/// A sentence and the other shapes the same request has: a wish without its "give me", what is
/// wished done as the order to do it ("I want rule 2 gone" is "delete rule 2"), what is no
/// longer needed as the order to take it away, and each with the German verb put first.
fn variants(sentence: &str) -> Vec<String> {
    static DONE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^give me (?P<what>.+) (?P<done>gone|removed|deleted|closed|shut|muted|dimmed|undone|selected|refreshed|reloaded|cleared|stopped|paused|opened|open|minimi[sz]ed|weg|gelöscht|entfernt|geschlossen|gedimmt|stumm|pausiert|gestoppt|geöffnet|minimiert)$").expect("what is wished done")
    });
    static UNNEEDED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(?:i )?(?:dont|do not|no longer) (?:need|want)(?: to have)? (?P<a>.+?)(?: anymore| any more| any longer| at all)?$|^(?P<b>.+?) (?:brauche ich|will ich|möchte ich) nicht mehr$|^give me (?P<c>.+?) (?:nicht mehr|loswerden|weg)$|^(?P<d>.+?) (?:kann|können|soll) weg$",
        )
        .expect("what is no longer needed")
    });
    let mut all = vec![sentence.to_string()];
    if let Some(rest) = sentence.strip_prefix("give me ") {
        all.push(rest.to_string());
    }
    if let Some(c) = DONE.captures(sentence) {
        let verb = match &c["done"] {
            "gone" | "removed" | "deleted" | "weg" | "gelöscht" | "entfernt" => "delete",
            "closed" | "shut" | "geschlossen" => "close",
            "muted" | "stumm" => "mute",
            "dimmed" | "gedimmt" => "dim",
            "undone" => "undo",
            "selected" => "select",
            "refreshed" | "reloaded" => "reload",
            "cleared" => "clear",
            "stopped" | "gestoppt" => "stop",
            "paused" | "pausiert" => "pause",
            "opened" | "open" | "geöffnet" => "open",
            _ => "minimize",
        };
        all.push(format!("{verb} {}", &c["what"]));
    }
    if let Some(c) = UNNEEDED.captures(sentence) {
        if let Some(what) = c.name("a").or(c.name("b")).or(c.name("c")).or(c.name("d")) {
            all.push(format!("delete {}", what.as_str()));
        }
    }
    let turned: Vec<String> = all.iter().filter_map(|s| verb_first(s)).collect();
    all.extend(turned);
    all
}

/// "opening" → "open": the verbs orders begin with.
fn plain_verb(word: &str) -> Option<&'static str> {
    const VERBS: &[(&str, &str)] = &[
        ("opening", "open"), ("closing", "close"), ("showing", "show"), ("starting", "start"), ("stopping", "stop"), ("muting", "mute"), ("pausing", "pause"), ("turning", "turn"),
        ("switching", "switch"), ("setting", "set"), ("putting", "put"), ("reading", "read"), ("making", "make"), ("saving", "save"), ("deleting", "delete"), ("playing", "play"),
        ("typing", "type"), ("scrolling", "scroll"), ("going", "go"), ("taking", "take"), ("giving", "give"), ("telling", "tell"), ("cancelling", "cancel"), ("canceling", "cancel"),
        ("listing", "list"), ("removing", "remove"), ("clearing", "clear"), ("moving", "move"), ("dimming", "dim"), ("checking", "check"), ("reloading", "reload"), ("pasting", "paste"),
        ("pressing", "press"), ("selecting", "select"), ("writing", "write"), ("adding", "add"), ("backing", "back"), ("keeping", "keep"), ("forgetting", "forget"), ("explaining", "explain"),
        ("translating", "translate"), ("summarising", "summarise"), ("summarizing", "summarize"), ("learning", "learn"), ("letting", "let"), ("bringing", "bring"), ("restoring", "restore"),
        ("noting", "note"), ("searching", "search"), ("looking", "look"), ("finding", "find"), ("refreshing", "refresh"), ("continuing", "continue"), ("ending", "end"), ("killing", "kill"),
        ("skipping", "skip"), ("hiding", "hide"), ("listening", "listen"), ("minimizing", "minimize"), ("minimising", "minimise"), ("zooming", "zoom"), ("snapping", "snap"), ("resuming", "resume"), ("running", "run"), ("launching", "launch"), ("unlocking", "unlock"), ("practising", "practise"), ("practicing", "practice"), ("changing", "change"), ("getting", "get"), ("pulling", "pull"), ("reopening", "reopen"), ("obeying", "obey"), ("disabling", "disable"), ("enabling", "enable"), ("activating", "activate"), ("lowering", "lower"), ("raising", "raise"), ("brightening", "brighten"), ("dictating", "dictate"),
    ];
    VERBS.iter().find(|(ing, _)| *ing == word).map(|(_, verb)| *verb)
}

// --------------------------------------------------------------------------- the words ----

/// The words shapes and ideas are written with (`<STOP>` in a pattern stands for all of these).
const WORDS: &[(&str, &str)] = &[
    ("<ART>", r"(?:(?:the|my|your|a|an|that|this|those|these|our|some|all|all my|all the|den|die|das|der|dem|ein|eine|einen|meine|meinen|mein|meiner|meinem|deine|deinen|dein|deiner|deinem|diese|diesen|dieses|alle|alle meine) )?"),
    ("<START>", r"(?:start|begin|turn on|switch on|put on|enable|activate|launch|run|open|open up|fire up|kick off|set up|bring up|do|use|go into|enter|give me|i need|i want|starte|start|beginne|beginn|aktiviere|öffne|mach)"),
    ("<STOP>", r"(?:stop|end|cancel|kill|quit|exit|leave|finish|turn off|switch off|shut off|shut down|disable|deactivate|abort|call off|close|close down|get rid of|beende|stopp|stoppe|schließe|schließ|deaktiviere)"),
    ("<SHOW>", r"(?:show|show me|list|display|open|open up|pull up|bring up|give me|let me see|let me look at|see|view|read me|read|tell me|check|look at|get|get me|whats in|what is in|zeig|zeige|zeig mir|zeige mir|liste|öffne|gib mir|lass mich sehen|was steht in)"),
    ("<DEL>", r"(?:delete|remove|erase|get rid of|drop|scrap|trash|clear|wipe|forget|throw away|lösche|lösch|entferne|entfern|vergiss)"),
    ("<TIMER>", r"(?:sleep ?timer|sleeptimer|schlaftimer|einschlaftimer|music timer|musiktimer|bedtime timer)"),
    ("<SCREENS>", r"(?:monitors?|screens?|displays?|bildschirme?|monitore?)"),
    ("<BRIGHT>", r"(?:brightness|screen brightness|monitor brightness|display brightness|brightness level|helligkeit|bildschirmhelligkeit)"),
    ("<NOTES>", r"(?:notes|notebook|notepad|note book|note list|notizen|notizbuch|notizliste)"),
    ("<NOTE>", r"(?:note|memo|notiz|eintrag)"),
    ("<SNIP>", r"(?:snippet|text snippet|textbaustein|baustein)"),
    ("<SNIPS>", r"(?:snippets|text snippets|textbausteine|bausteine)"),
    ("<CLIP>", r"(?:clipboard|copied text|copied stuff|text i (?:just )?copied|what i (?:just )?copied|thing i (?:just )?copied|stuff i (?:just )?copied|what is in (?:my|the) clipboard|whats in (?:my|the) clipboard|zwischenablage|kopierten text|kopierte|was ich (?:gerade )?kopiert habe)"),
    ("<SUBS>", r"(?:subtitles?|subtitle bar|captions?|caption bar|subs|untertitel|untertitelleiste)"),
    ("<TAB>", r"(?:tab|browser tab|reiter)"),
    ("<PAGE>", r"(?:page|site|website|web page|webpage|tab|seite|webseite)"),
    ("<SPAN>", r"(?P<span>(?:\d+|a|an|one|two|three|four|five|six|ten|fifteen|twenty|thirty|forty|forty five|sixty|ninety|half an|half a|a quarter of an|eine|einer|einem|zwei|drei|vier|fünf|zehn|fünfzehn|zwanzig|dreißig|vierzig|sechzig|einer halben|eine halbe|halben) ?(?:minutes?|mins?|min|hours?|hrs?|hr|stunden?|std|minuten?)(?: and a half| (?:and |und )?\d+ ?(?:minutes?|mins?|minuten?))?)"),
    ("<NUM>", r"(?P<n>\d{1,3})(?: ?(?:percent|prozent|%))?"),
    ("<ON>", r"(?:on|an|ein|active|enabled|aktiv|aktiviert|eingeschaltet)"),
    ("<OFF>", r"(?:off|aus|inactive|disabled|deactivated|deaktiviert|ausgeschaltet|weg|away)"),
    ("<TURNON>", r"(?:turn on|switch on|put on|enable|activate|start|engage|go into|go to|enter|switch to|switch into|change to|use|set|bring up|give me|i want|i need|lets have|aktiviere|aktivier|schalte|schalt|mach|mache|starte|wechsle in|wechsel in|wechsle zu|geh in|gehe in|ich will|ich brauche|switch|change|put|wechsle|wechsel|stell|stelle|schalte in|schalt in|schalte auf)"),
    ("<TURNOFF>", r"(?:turn off|switch off|shut off|disable|deactivate|stop|end|exit|leave|quit|kill|cancel|get out of|go out of|drop|deaktiviere|deaktivier|beende|verlasse|verlass|raus aus|schluss mit)"),
    ("<NOMORE>", r"(?:dont need|do not need|dont want|do not want|no longer|no more|not anymore|any more|anymore|enough|nicht mehr|kein|keine|keinen|genug|brauche ich nicht|brauch ich nicht)"),
    ("<NOT>", r"(?:dont|do not|never|not|nicht|niemals|nie|kein|keine|keinen)"),
    ("<LANG>", r"(?P<lang>english|german|englisch|englische|deutsch|deutsche)"),
    ("<WHO>", r"(?P<name>[a-zäöüß]{2,20})"),
];

fn expand(pattern: &str) -> String {
    let mut out = pattern.to_string();
    for (name, words) in WORDS {
        out = out.replace(name, words);
    }
    out
}

// ---------------------------------------------------------------------------- the ways ----

/// One order and the ways of saying it.
struct Way {
    /// The wording the order was written for; `{n}`, `{span}`, `{text}`, `{name}`, `{app}`,
    /// `{then}`, `{place}` and `{lang}` are filled from the sentence.
    canon: &'static str,
    /// Whole-sentence shapes, any of which is it.
    shapes: Vec<Regex>,
    /// Or its ideas: every one of them somewhere in the sentence, in any order …
    ideas: Vec<Regex>,
    /// … and nothing of this.
    never: Option<Regex>,
    /// Only while this holds ("that's enough" means the coach only while he coaches).
    when: Option<fn() -> bool>,
    /// A sentence about a person is this order's own ("this is Tom, remember his voice").
    people: bool,
    /// The order is a question itself ("how do I use the sleep timer").
    asks: bool,
    /// It takes any name for an app or a window: asked last, and never for one clause alone.
    any: bool,
}

/// How many patterns did not compile (none may: `tests`). One that does not is left out.
static BROKEN: AtomicUsize = AtomicUsize::new(0);

fn pattern(canon: &str, source: &str) -> Option<Regex> {
    // ("[a-z ]{2,30}" is thirty copies of the class to the engine, which then gives up its
    // quick automaton for a slow one: the verifier measured 6 ms a sentence. The class may
    // run on; how long a name may be is checked where it is read, `Way::wording`.)
    static COUNTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\]\{\d+,\d+\}").expect("a counted class"));
    let source = COUNTED.replace_all(source, "]+");
    let source = source.as_ref();
    match Regex::new(source) {
        Ok(regex) => Some(regex),
        Err(e) => {
            BROKEN.fetch_add(1, Ordering::Relaxed);
            tracing::error!("[WAYS] A way of saying '{canon}' is no valid pattern and is left out: {e}");
            None
        }
    }
}

fn shape(canon: &'static str, patterns: &[&str]) -> Way {
    let expanded: Vec<String> = patterns.iter().map(|p| expand(p)).collect();
    // (Shapes that fill nothing in are asked as one pattern: a sentence is read once for all
    // of them. A name may stand once in a pattern, so the ones that capture stay apart.)
    let shapes = if expanded.iter().any(|p| p.contains("(?P<")) {
        expanded.iter().filter_map(|p| pattern(canon, &format!("^(?:{p})$"))).collect()
    } else {
        // (Each is tried alone first, so that one broken shape is found and left out.)
        let sound: Vec<&String> = expanded.iter().filter(|p| pattern(canon, &format!("^(?:{p})$")).is_some()).collect();
        pattern(canon, &format!("^(?:{})$", sound.iter().map(|p| format!("(?:{p})")).collect::<Vec<_>>().join("|"))).into_iter().collect()
    };
    Way { canon, shapes, ideas: Vec::new(), never: None, when: None, people: false, asks: false, any: false }
}

fn idea(canon: &'static str, ideas: &[&str], never: &str) -> Way {
    let somewhere = |words: &str| pattern(canon, &format!(r"(?:^| )(?:{})(?: |$)", expand(words)));
    let mut found: Vec<Regex> = ideas.iter().filter_map(|i| somewhere(i)).collect();
    let barred = if never.is_empty() { None } else { somewhere(never) };
    // (An idea that lost one of its parts would take far more for itself than it was written
    // for: without all of them it is none.)
    if found.len() != ideas.len() || (!never.is_empty() && barred.is_none()) {
        found.clear();
    }
    Way { canon, shapes: Vec::new(), ideas: found, never: barred, when: None, people: false, asks: false, any: false }
}

/// A switch of his by its names: on its own or with a word of switching on it is on, with a
/// word of switching off, off.
fn switch(on: &'static str, off: &'static str, names: &str) -> Vec<Way> {
    let name = format!("<ART>(?:{names})");
    vec![
        idea(off, &[names, "<TURNOFF>|<OFF>|<NOMORE>|ab|abschalten|ausschalten|ausmachen|beenden|deaktivieren|verlassen|back to normal|wieder normal"], "<NOT> (?:turn|switch|disable|stop|end|leave|exit|deactivate|kill|cancel)|nicht (?:aus|ab)"),
        shape(
            on,
            &[
                &format!("(?:<TURNON> )?{name}(?: <ON>)?(?: again| wieder)?"),
                &format!("{name} (?:einschalten|anmachen|aktivieren|anschalten|starten)"),
                &format!("(?:turn|switch|put|schalte|schalt|mach|mache|stell|stelle) {name} (?:<ON>|back on|wieder an)"),
            ],
        ),
        idea(on, &[names, "<TURNON>|<ON>|einschalten|anmachen|aktivieren|anschalten|back on|wieder an"], "<TURNOFF>|<OFF>|<NOMORE>|<NOT>|what|why|how|was|warum|wie|ab|abschalten|ausschalten|ausmachen|beenden|deaktivieren|verlassen"),
    ]
}

impl Way {
    fn while_(mut self, holds: fn() -> bool) -> Way {
        self.when = Some(holds);
        self
    }

    fn of_people(mut self) -> Way {
        self.people = true;
        self
    }

    fn asks(mut self) -> Way {
        self.asks = true;
        self
    }

    fn any_name(mut self) -> Way {
        self.any = true;
        self
    }

    /// The order's own wording for `sentence`, when the sentence is one of its ways.
    fn wording(&self, sentence: &str, original: &str) -> Option<String> {
        let mut slots: Vec<(&str, String)> = Vec::new();
        // (Whether a shape is it at all is asked first and costs next to nothing; what it
        // holds is only read out of the one that is, and only where the wording has a place
        // for it. Reading every shape's captures took 6 ms a sentence where this takes a
        // fraction of one: the verifier's own budget for the whole fast path is 1.5 ms.)
        if let Some(shape) = self.shapes.iter().find(|shape| shape.is_match(sentence)) {
            if self.canon.contains('{') {
                let captures = shape.captures(sentence)?;
                for name in ["n", "span", "text", "name", "app", "then", "place", "lang"] {
                    if let Some(found) = captures.name(name) {
                        slots.push((name, found.as_str().trim().to_string()));
                    }
                }
            }
        } else {
            if self.ideas.is_empty() || !self.ideas.iter().all(|idea| idea.is_match(sentence)) || self.never.as_ref().is_some_and(|never| never.is_match(sentence)) {
                return None;
            }
            if let Some(found) = SPAN.captures(sentence).and_then(|c| c.name("span")) {
                slots.push(("span", found.as_str().trim().to_string()));
            }
            if let Some(found) = NUM.captures(sentence).and_then(|c| c.name("n")) {
                slots.push(("n", found.as_str().to_string()));
            }
            if let Some(found) = PLACE.captures(sentence).and_then(|c| c.name("place")) {
                slots.push(("place", found.as_str().to_string()));
            }
        }
        // (A name is a word or a few, not a sentence: see `pattern`.)
        if slots.iter().any(|(name, value)| matches!(*name, "name" | "app") && !(2..=30).contains(&value.chars().count())) || slots.iter().any(|(name, value)| *name == "text" && value.chars().count() > 160) {
            return None;
        }
        // ("Go to the left" names no app called "go".)
        const NO_APP: &[&str] = &[
            "go", "move", "scroll", "turn", "look", "it", "this", "that", "more", "further", "back", "everything", "nothing", "something", "you", "me", "all", "page", "tab", "window", "windows",
            "swipe", "slide", "pan", "rotate", "walk", "run", "step", "shift", "geh", "alles", "nach", "weiter", "them", "these", "those", "one", "here", "there", "das", "es", "left", "right", "top", "bottom", "up", "down", "next", "previous", "last", "first", "sleep", "bed", "work", "school", "home", "links", "rechts",
        ];
        if slots.iter().any(|(name, value)| *name == "app" && NO_APP.contains(&value.as_str())) {
            return None;
        }
        // ("Mute everything" names no guest called "mute".)
        const NO_GUEST: &[&str] = &[
            "mute", "play", "stop", "pause", "open", "close", "give", "make", "set", "turn", "show", "start", "put", "let", "get", "the", "my", "your", "his", "her", "this", "that", "it", "me", "you",
            "everyone", "everybody", "anyone", "music", "volume", "sound", "please", "just", "only", "full", "access", "who", "what", "and", "all", "alle", "musik", "nur", "der", "die", "das",
        ];
        if (self.canon.contains(" may ") || self.canon.contains("voice")) && slots.iter().any(|(name, value)| *name == "name" && NO_GUEST.contains(&value.as_str())) {
            return None;
        }
        let mut wording = self.canon.to_string();
        for (name, value) in &slots {
            let value = match *name {
                // (What Sir wants kept, a note's words, keeps the capitals he gave it.)
                // ("Look for the word photosynthesis": the word is what is looked for.)
                "text" if self.canon.starts_with("search the page") => as_said(original, ["the word ", "the term ", "das wort ", "dem wort "].iter().find_map(|lead| value.strip_prefix(*lead)).unwrap_or(value.as_str())),
                "text" => as_said(original, value),
                "span" => span_in_english(value),
                // (An app's name as it was said: "7-Zip" keeps its hyphen.)
                "app" => as_said(original, value).to_lowercase(),
                "place" => place(value).to_string(),
                "lang" => language(value).to_string(),
                _ => counted(value),
            };
            wording = wording.replace(&format!("{{{name}}}"), &value);
        }
        // ("What's the weather like in Berlin" names nothing of his to explain: such a wording
        // only cost every question for the model another pass of the fast path, 0.6 ms.)
        if self.canon == "explain {text}" {
            let s = wording.to_lowercase();
            if crate::changes::command(&s).is_none() && crate::intents::explained(&s).is_none() {
                return None;
            }
        }
        // (A wording that still waits for something the sentence did not give is none.)
        (!wording.contains('{')).then_some(wording)
    }
}

/// A span of time or a number said anywhere in a sentence (for the ideas, which name no place
/// for it).
static SPAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(&expand("<SPAN>")).expect("the span"));
static NUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(&expand(r"(?:^| )<NUM>(?: |$)")).expect("the number"));
static PLACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^| )(?P<place>top|bottom|middle|centre|center|oben|unten|mitte)(?: |$)").expect("the place"));

/// "second" → "2": how the wordings count.
fn counted(value: &str) -> String {
    match value {
        "one" | "first" | "1st" | "eins" | "erste" | "ersten" | "main" | "primary" | "haupt" => "1",
        "two" | "second" | "2nd" | "zwei" | "zweite" | "zweiten" => "2",
        "three" | "third" | "3rd" | "drei" | "dritte" | "dritten" => "3",
        "four" | "fourth" | "4th" | "vier" | "vierte" | "vierten" => "4",
        "five" | "fifth" | "5th" | "fünf" | "fünfte" | "fünften" => "5",
        other => other,
    }
    .to_string()
}

/// "20 Minuten", "einer halben Stunde": a span as every timer reads it.
fn span_in_english(value: &str) -> String {
    let value = value.replace("einer halben stunde", "30 minutes").replace("eine halbe stunde", "30 minutes").replace("halben stunde", "30 minutes");
    value
        .split(' ')
        .map(|word| match word {
            "minuten" | "min" | "mins" => "minutes",
            "stunden" | "std" | "hrs" => "hours",
            "stunde" | "hr" => "hour",
            "eine" | "einer" | "einem" => "1",
            "zwei" => "2",
            "drei" => "3",
            "vier" => "4",
            "fünf" => "5",
            "zehn" => "10",
            "fünfzehn" => "15",
            "zwanzig" => "20",
            "dreißig" => "30",
            "vierzig" => "40",
            "sechzig" => "60",
            "und" => "and",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where on a screen: "up", "oben", "higher" are the top.
fn place(value: &str) -> &'static str {
    match value {
        "top" | "up" | "higher" | "hoch" | "höher" | "oben" => "top",
        "bottom" | "down" | "lower" | "runter" | "tiefer" | "unten" => "bottom",
        _ => "middle",
    }
}

fn language(value: &str) -> &'static str {
    if value.starts_with("ger") || value.starts_with("deu") {
        "german"
    } else {
        "english"
    }
}

/// The words of `found` as they stand in what was said (their capitals, commas, hyphens and
/// apostrophes), when they can be found there. (By going through what was said word by word:
/// a pattern made for the purpose cost 10 ms to build, every time.)
pub(crate) fn as_said(original: &str, found: &str) -> String {
    let wanted: Vec<&str> = found.split_whitespace().collect();
    if wanted.is_empty() {
        return found.to_string();
    }
    // Each word of what was said: where it stands, and as the phrase holds it ("don't" is
    // "dont" there).
    let mut words: Vec<(usize, usize, String)> = Vec::new();
    let mut start: Option<usize> = None;
    for (at, c) in original.char_indices().chain(std::iter::once((original.len(), ' '))) {
        let inside = c.is_alphanumeric() || c == '\'' || c == '’';
        match (inside, start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                words.push((from, at, original[from..at].to_lowercase().replace(['\'', '’'], "")));
                start = None;
            }
            _ => {}
        }
    }
    words
        .windows(wanted.len())
        .find(|window| window.iter().zip(&wanted).all(|((_, _, word), want)| word == want))
        .map_or_else(|| found.to_string(), |window| original[window[0].0..window[wanted.len() - 1].1].to_string())
}

/// Sentences that tell of something: nobody's order, whatever words of an order they hold.
static TOLD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^| )(?:yesterday|last night|last week|last year|(?:minutes|hours|days|weeks|months|years) ago|a while ago|the other day|back then|in class|at school|at work|on my phone|on his phone|on her phone|on the phone|on my iphone|on android|on tv|on the tv|on my tv|on netflix|on youtube|on my laptop|on my tablet|on the xbox|on the playstation|in the game|in that game|in this game|in the movie|in that movie|in the video|in that video|in the show|my teacher|my mom|my mum|my dad|my boss|my grandma|my grandpa|he was|she was|they were|there was|there were|he had|she had|we had|they had|used to|i told|i said|he said|she said|they said|someone said|people say|did you know|remember when|once upon|it turned|it went|it stopped|it started|it broke|it crashed|went|took|saw|thought|tried|bought|broke|crashed|corrupted|during the|twice|because|i had to|had to|weil|ich musste|musste|gestern|letzte woche|letztes jahr|in der schule|im unterricht|auf dem handy|auf meinem handy|im spiel|im film|im video|im fernsehen|mein lehrer|meine lehrerin|meine mutter|mein vater|meine oma|mein opa|er war|sie war|wir waren|da war|er hatte|wir hatten|früher|damals|hat gesagt|haben gesagt)(?: |$)",
    )
    .expect("the tellings")
});

/// "I left my notes on the bus", "ich hab meine Notizen im Bus liegen lassen": what he did, not
/// what he wants done.
static DID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:i|we) (?:left|forgot|lost|found|bought|sold|broke|saw|watched|finished|used|played|read|wrote|went|came|took|gave|told|said|thought|tried|did|made|got|had|needed|wanted)(?: |$)|^(?:ich|wir) (?:habe|hab|haben|bin|sind) .* (?:ge[a-zäöüß]+(?:t|en)|[a-zäöüß]+iert|lassen|vergessen|verloren|bekommen)$",
    )
    .expect("what he did")
});

/// Sentences about someone else: an order only where the order is about a person (a guest).
/// ("Her" is hers only where it stands as an English word: "wieder her" brings something back.)
static PEOPLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^| )(?:he|she|they|him|them|his|hers|their|theirs|hes|shes|theyre|theyve|er|ihn|ihm|ihre|ihrem|ihren|seine|seiner|seinem|seinen)(?: |$)|(?:^| )(?:tell|ask|give|show|remember|learn|forget|is|with|for|to|of|about|and|told|asked|let) her(?: |$)|(?:^| )her (?:voice|phone|name|own|friend|mom|mum|dad|house|room|pc|computer|laptop|car|birthday|sister|brother)(?: |$)",
    )
    .expect("the people")
});

/// Questions about the world: an order only where the order is itself a question.
static WORLD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:what is an?|whats an?|what does an?|what do [a-z]+ (?:do|eat|mean)|why do|why does|why are|why did|who invented|who is the|who was|who made|how do you say|how does an?|how do (?:you|i|we|they|people)|how can (?:you|i|we|they|people)|how much is|is it true|do you think|do you know (?:what|why|how|who|if|whether)|can you explain (?:what|why|how)|was ist eine?|was sind|warum sind|warum macht|wer hat|glaubst du|weißt du (?:was|warum|wie|wer|ob)|wie macht man|wie kann man)(?: |$)",
    )
    .expect("the world")
});

#[cfg(test)]
thread_local! {
    static SILENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ASSUMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// (Tests: with this on, nothing is said another way, so the fast path is what it was.)
#[cfg(test)]
pub fn silence(on: bool) {
    SILENT.with(|silent| silent.set(on));
}

/// (Tests: takes the coach, a reading and a dictation for going on.)
#[cfg(test)]
pub fn assume_running(on: bool) {
    ASSUMED.with(|assumed| assumed.set(on));
}

#[cfg(test)]
fn assumed() -> bool {
    ASSUMED.with(|assumed| assumed.get())
}

#[cfg(not(test))]
fn assumed() -> bool {
    false
}

static WAYS: LazyLock<Vec<Way>> = LazyLock::new(|| {
    let mut ways = first_half();
    ways.extend(second_half());
    ways.extend(everyday());
    ways.extend(more());
    ways.extend(by_idea());
    ways.extend(any_name().into_iter().map(Way::any_name));
    ways
});

/// The kit's first half: dictation, snippets, keys, the notebook, the sleep timer, rules,
/// brightness, desk layouts, the dashboard, reading aloud.
fn first_half() -> Vec<Way> {
    vec![
        // ---------------------------------------------------------------- dictation ----
        shape(
            "stop dictating",
            &[
                r"<STOP> <ART>(?:dictation|dictating|dictate|dictation mode|dictate mode|diktat|diktieren|diktiermodus)(?: mode)?",
                r"(?:dictation|dictation mode|dictate mode|diktat|diktiermodus) (?:off|aus|over|done|finished|stop|end|ende|beenden|stoppen)",
                r"(?:im |i am |we are |were |ich bin )?(?:done|finished|fertig)(?: with| mit)?(?: the| dem)? (?:dictating|dictation|diktat|diktieren)",
                r"(?:thats|that is|das ist) (?:all|it|enough|alles|genug)(?: for| für)?(?: the| das)? (?:dictation|dictating|diktat)",
                r"(?:hör|hoer) auf (?:zu )?(?:diktieren|mitzuschreiben|mit dem diktat|mit dem mitschreiben)",
                r"(?:nicht mehr|kein|keine) (?:mitschreiben|diktieren|diktat)(?: mehr)?",
                r"(?:dont|do not|no more|stop) (?:typing|writing)(?: down)?(?: what i say| what im saying| my words| everything i say)?(?: anymore| any more)?",
            ],
        ),
        shape(
            "start dictating",
            &[
                r"<START> <ART>(?:dictation|dictating|dictate|dictation mode|dictate mode|diktat|diktieren|diktiermodus)(?: mode| modus)?",
                r"(?:dictation|dictation mode|dictate mode|diktat|diktiermodus) (?:on|an|ein|start|starten|bitte)",
                r"(?:type|write|schreib|schreibe|tipp|tippe)(?: down| out| mit| auf)? (?:what i say|what im saying|what i am saying|everything i say|as i speak|as i talk|while i talk|while i speak|my words|was ich sage|alles was ich sage|mit was ich sage|mit)",
                r"(?:dictate|diktieren|diktiere)(?: something| a text| a message| some text| etwas| einen text| was)?",
                r"(?:take|nimm)(?: a| ein| this| down a)? (?:dictation|diktat)(?: auf)?",
                r"let me dictate(?: something| a text| a message)?",
                r"(?:lass mich |ich diktiere )?(?:diktieren|was diktieren|etwas diktieren|dir was diktieren|dir etwas diktieren)",
                r"(?:write|type|schreib|tipp) (?:for me|along|with me|mit)",
                r"(?:speech|voice) (?:to text|typing)(?: on| mode)?",
            ],
        ),
        // ------------------------------------------------------------------ snippets ----
        shape("list my snippets", &[r"<SHOW> <ART>(?:saved |stored |text )*<SNIPS>", r"(?:what|which|welche) <SNIPS>(?: do i have| have i (?:got|saved)| are there| are saved| exist| habe ich| gibt es)", r"(?:what are|was sind) (?:my|meine) <SNIPS>", r"(?:my |meine )?<SNIPS>", r"(?:do i have|have i got|habe ich)(?: any| irgendwelche)? <SNIPS>"]),
        shape(
            "delete the snippet {name}",
            &[r"<DEL> <ART><SNIP>(?: called| named| namens)? (?P<name>[a-zäöüß0-9 ]{2,30})", r"<DEL> <ART>(?P<name>[a-zäöüß0-9 ]{2,30}?) <SNIP>", r"<SNIP> (?P<name>[a-zäöüß0-9 ]{2,30}?) (?:delete|remove|löschen|entfernen|weg)"],
        ),
        shape(
            "save my clipboard as snippet {name}",
            &[
                r"(?:save|keep|store|remember|put|add|speicher|speichere|merk dir|leg|sichere) <ART><CLIP> (?:as|under|als|unter)(?: a| an| the| my| ein| einen| den)?(?: new| neuen)? ?(?:<SNIP>)?(?: called| named| namens)? ?(?P<name>[a-zäöüß0-9 ]{2,30}?)(?: <SNIP>)?(?: ab)?",
                r"(?:make|create|add|new|save|erstelle|erstell|neuer|neuen|mach)(?: me| mir)?(?: a| an| ein| einen)?(?: new| neuen)? <SNIP>(?: called| named| namens)? (?P<name>[a-zäöüß0-9 ]{2,30}?) (?:from|out of|with|using|aus|mit) <ART><CLIP>",
                r"(?:save|keep|store|remember|speichere|speicher|merk dir) (?:this|that|it|das|dies|es) (?:as|als)(?: a| an| my| the| ein| einen)?(?: new| neuen)? <SNIP>(?: called| named| namens)? (?P<name>[a-zäöüß0-9 ]{2,30})",
                r"(?:turn|make) <ART><CLIP> into (?:a |an |my )?(?:new )?<SNIP>(?: called| named)? (?P<name>[a-zäöüß0-9 ]{2,30})",
                r"(?:new|neuer) <SNIP> (?P<name>[a-zäöüß0-9 ]{2,30}?) (?:from|aus)(?: the| my| der)? (?:clipboard|zwischenablage)",
            ],
        ),
        // ---------------------------------------------------------------------- keys ----
        shape("next tab", &[r"(?:go to |switch to |jump to |move to |show |open |give me |geh zum |geh auf den |wechsle zum |wechsel zum |zum )?<ART>(?:next|following|nächsten|nächste|nächster) <TAB>", r"<TAB> (?:forward|right|weiter|vor|nach rechts)", r"(?:one |a |einen |ein )?<TAB> (?:to the right|over|further|forward|weiter|vor|nach rechts)", r"(?:go |switch |move |geh |wechsle )?(?:one |a |einen )?<TAB> (?:forward|ahead|further|weiter|vor)", r"next one in (?:the |my )?browser"]),
        shape("previous tab", &[r"(?:go to |switch to |jump to |move to |go back to |show |open |give me |geh zum |geh auf den |wechsle zum |wechsel zum |zum )?<ART>(?:previous|last|prior|former|vorherigen|vorherige|vorigen|vorige|letzten|letzte) <TAB>", r"<TAB> (?:back|backward|backwards|left|zurück|nach links)", r"(?:go |switch |move |geh |wechsle )?(?:back )?(?:one |a |einen |ein )?<TAB>(?: back| backwards| to the left| zurück| nach links)", r"(?:the )?<TAB> before(?: this one)?"]),
        shape("new tab", &[r"(?:open|open up|make|create|give me|start|add|öffne|mach|erstelle)(?: me| mir)?(?: up)?(?: a| an| ein| einen| another| noch einen)?(?: new| fresh| blank| empty| neuen| neues| weiteren| leeren)? <TAB>(?: auf)?", r"(?:new|another|fresh|blank|neuer|neuen|noch ein|noch einen) <TAB>", r"<TAB> (?:new|neu)"]),
        shape("reopen the closed tab", &[r"(?:reopen|restore|bring back|get back|recover|undo|unclose|reöffne) <ART>(?:last |just |recently |zuletzt |gerade )?(?:closed |geschlossenen )?<TAB>(?: i (?:just )?closed| that i (?:just )?closed| again| back)*", r"(?:open|öffne|mach|hol|stell)(?: mir)? <ART>(?:last |zuletzt |gerade |letzten )?(?:closed |geschlossenen )?<TAB>(?: i (?:just )?closed| that i (?:just )?closed)?(?: again| back| back up| wieder| wieder auf| zurück| wieder her| nochmal auf)+", r"(?:open|öffne|mach)(?: mir)? <ART>(?:last |just |zuletzt |gerade )?(?:closed|geschlossenen) <TAB>(?: wieder| auf| again| wieder auf)*", r"i (?:just )?closed (?:a|that|the|the wrong) <TAB>(?: by accident| by mistake| accidentally)?(?: bring it back| open it again| reopen it| get it back)?"]),
        shape("scroll down", &[r"(?:scroll|go|move|scrolle|scroll mal|geh)(?: the page| it| this| this page| further| more| weiter| on)? (?:down|downwards|lower|further down|runter|nach unten|hinunter|abwärts|weiter runter|weiter nach unten)", r"(?:down|runter|nach unten|weiter runter)(?: a page| the page| scrollen)?", r"(?:scroll|scrollen)(?: nach unten| runter)", r"(?:page|seite) (?:down|runter|nach unten)"]),
        shape("scroll up", &[r"(?:scroll|go|move|scrolle|scroll mal|geh)(?: the page| it| this| this page| back| further| weiter)? (?:up|upwards|higher|back up|further up|hoch|rauf|nach oben|hinauf|aufwärts|weiter hoch|weiter nach oben)", r"(?:up|hoch|rauf|nach oben)(?: a page| the page| scrollen)?", r"(?:scroll|scrollen)(?: nach oben| hoch| rauf)", r"(?:page|seite) (?:up|hoch|nach oben)"]),
        shape("go to the top", &[r"(?:go|jump|scroll|get|take me|bring me|geh|spring|scrolle|scroll)(?: all the way| right| straight| ganz| wieder)*(?: back)?(?: up)? (?:to |an |zum |nach |at )(?:the |den )?(?:very )?(?:top|start|beginning|anfang|seitenanfang)(?: of (?:the |this )?(?:page|site|document|website)| der seite)?", r"(?:top of (?:the |this )?page|page top|seitenanfang|ganz nach oben|all the way up|nach ganz oben|ganz hoch|ganz rauf|back to (?:the )?top|to the top)", r"(?:scroll|go|geh|scrolle)(?: mal)? (?:all the way up|ganz nach oben|ganz hoch|ganz rauf|to the very top)"]),
        shape("go to the bottom", &[r"(?:go|jump|scroll|get|take me|bring me|geh|spring|scrolle|scroll)(?: all the way| right| straight| ganz)*(?: down)? (?:to |an |zum |ans |nach )(?:the |das )?(?:very )?(?:bottom|end|ende|seitenende)(?: of (?:the |this )?(?:page|site|document|website)| der seite)?", r"(?:bottom of (?:the |this )?page|page bottom|seitenende|ganz nach unten|all the way down|nach ganz unten|ganz runter|to the bottom|to the end)", r"(?:scroll|go|geh|scrolle)(?: mal)? (?:all the way down|ganz nach unten|ganz runter|to the very bottom)"]),
        shape("page back", &[r"(?:go |navigate |geh |gehe )?(?:back|zurück)(?: a| one| to the previous| to the last| eine)? (?:page|seite)", r"(?:previous|last|vorherige|letzte|vorige) (?:page|seite)", r"(?:page|seite|browser|history) (?:back|backward|backwards|zurück)", r"(?:go |geh )?(?:back in (?:the |my )?(?:browser|history)|im browser zurück|eine seite zurück|zur vorherigen seite|zurück zur letzten seite|back to the (?:previous|last) page)", r"(?:hit|press|click|drück)(?: the| auf)? (?:back button|browser back|back|zurück)(?: button| im browser)?", r"browser zurück"]),
        shape("page forward", &[r"(?:go |navigate |geh |gehe )?(?:forward|forwards|vor|vorwärts)(?: a| one| to the next| eine)? (?:page|seite)", r"(?:next|nächste) (?:page|seite)", r"(?:page|seite|browser|history) (?:forward|forwards|vor|vorwärts)", r"(?:go |geh )?(?:forward in (?:the |my )?(?:browser|history)|im browser vor|im browser vorwärts|eine seite vor|eine seite weiter|zur nächsten seite|forward again)", r"(?:hit|press|click|drück)(?: the| auf)? (?:forward button|browser forward|forward|vorwärts)(?: button| im browser)?", r"browser (?:vor|vorwärts)"]),
        shape("reload the page", &[r"(?:reload|refresh|update|aktualisiere|aktualisier|lade|lad)(?: mal)? <ART>(?:current |web |whole |aktuelle |ganze )?<PAGE>(?: neu| again| nochmal| once more)?", r"<PAGE> (?:reload|refresh|neu laden|aktualisieren)", r"(?:hit|press|drück|drücke)(?: the)? f5", r"(?:reload|refresh|neu laden|aktualisieren) (?:this|that|the|die|diese) <PAGE>", r"(?:give (?:the|this) <PAGE> a (?:refresh|reload))", r"(?:lade|lad) (?:die |diese )?<PAGE> (?:neu|nochmal)", r"(?:die |diese )?<PAGE> (?:neu laden|aktualisieren|nochmal laden)"]),
        shape("select all", &[r"(?:select|mark|highlight|markiere|markier|wähle|wähl)(?: mal)? (?:all|everything|all of it|it all|the whole (?:thing|text|page|document)|the entire (?:text|page|document)|all (?:the |of the )?text|alles|den ganzen text|den gesamten text|alles aus|die ganze seite)(?: aus| on (?:the|this) page| here)?", r"(?:press |hit |drück |drücke )?(?:control|ctrl|strg|steuerung) a", r"(?:alles|den ganzen text) (?:markieren|auswählen)", r"select (?:the )?(?:whole|entire) (?:thing|lot)"]),
        shape("paste it", &[r"(?:paste|insert|drop|put|füge|füg|setz)(?: in)? (?:it|that|this|das|es|dies)(?: in| here| there| ein| hier| rein| hier ein)*", r"(?:paste|insert|füge|füg)(?: in| here| ein| hier ein)? (?:from |aus )?<ART><CLIP>(?: in| here| ein| hier ein)*", r"(?:paste|insert)(?: it| that)? (?:here|in here|right here|there)", r"(?:press |hit |drück |drücke )?(?:control|ctrl|strg|steuerung) v", r"(?:das |es )?(?:hier )?(?:einfügen|reinkopieren)", r"(?:put|drop) (?:what i copied|my clipboard|the clipboard) (?:in|here|in here)"]),
        shape("press enter", &[r"(?:press|hit|push|tap|smash|click|drück|drücke|tippe|tipp)(?: the| die| auf| auf die| mal)* (?:enter|return|eingabe|eingabetaste|enter taste|entertaste)(?: key| button| taste)?", r"(?:enter|return|eingabe)(?: key| taste)? (?:press|hit|drücken|bitte)", r"(?:the )?(?:enter|return) (?:key|taste|button)"]),
        shape(
            "search the page for {text}",
            &[
                r"(?:search|find|look for|look up|suche|such|finde|find)(?: for| nach)? (?P<text>.+?) (?:on|in|within|auf|in) <ART>(?:current |aktuellen |aktuelle )?<PAGE>",
                r"(?:search|suche|such|durchsuche|durchsuch) <ART>(?:current |aktuelle )?<PAGE> (?:for|nach) (?P<text>.+)",
                r"(?:find|search|look)(?: for)? (?P<text>.+?) (?:here|on here|hier|on this)",
                r"(?:control|ctrl|strg|steuerung) f (?P<text>.+)",
                r"(?:where does it say|where is|wo steht|wo ist) (?P<text>.+?) (?:on|auf|in) <ART><PAGE>",
                r"(?:find|search)(?: me)? (?:the word |the term |where it says )(?P<text>.+?)(?: on (?:the|this) <PAGE>| here)?",
                r"(?:is|does) (?:the word )?(?P<text>.+?) (?:appear |show up |mentioned |anywhere )+on <ART><PAGE>",
            ],
        ),
        // --------------------------------------------------------------------- notes ----
        shape("delete the last note", &[r"<DEL> <ART>(?:last|latest|newest|most recent|previous|recent|letzte|neueste|vorherige) <NOTE>(?: i (?:made|took|wrote|added)| again| wieder)?", r"(?:undo|scratch|take back|nimm|mach) <ART>(?:last |letzte )?<NOTE>(?: zurück| again| rückgängig| weg)?", r"(?:the |that )?(?:last|latest) <NOTE> (?:can go|should go|delete it|remove it|weg|löschen|kann weg)", r"(?:letzte|die letzte) <NOTE> (?:löschen|entfernen|weg)"]),
        shape(
            "what did i note about {text}",
            &[
                r"(?:what|was) (?:did|have|habe) i (?:note|noted|write|written|jot|jotted|put|save|saved)(?: down)? (?:about|on|regarding|for|concerning) (?P<text>.+)",
                r"(?:find|search|look for|look up|show|show me|get|zeig|zeig mir|suche|such|finde|find)(?: in| through| for| nach)? <ART>(?:<NOTE>|<NOTES>|notes?|notizen?) (?:about|on|regarding|with|mentioning|for|that mention|that mentions|über|zu|mit) (?P<text>.+)",
                r"(?:search|look through|check|go through|durchsuche|durchsuch) <ART><NOTES> (?:for|about|nach) (?P<text>.+)",
                r"(?:what do|what does|whats in) (?:my|the) <NOTES> (?:say |have |got )?(?:about|on) (?P<text>.+)",
                r"(?:do i have|have i got|is there|did i (?:make|take|write)|habe ich)(?: a| any| eine| irgendwelche)? (?:<NOTE>|notes|notizen) (?:about|on|regarding|über|zu) (?P<text>.+)",
                r"was habe ich(?: mir)? (?:über|zu|zum|zur) (?P<text>.+?) (?:notiert|aufgeschrieben|gemerkt)",
                r"(?:anything|something|irgendwas|etwas) (?:in|on) <ART><NOTES> (?:about|on|über|zu) (?P<text>.+)",
                r"(?:any )?(?:notes|notizen) (?:about|on|über|zu) (?P<text>.+)",
            ],
        ),
        shape("show my notes", &[r"<SHOW> <ART>(?:all |saved |latest |recent |of my )*<NOTES>", r"(?:what|which|was)(?: notes| notizen)? (?:did|have|habe) i (?:note|noted|written|write|jot|jotted|put|save|saved)(?: down)?(?: so far| lately| recently| bisher)?", r"(?:what is|whats|what are|what do i have|was steht|was ist) (?:in|on|auf) <ART><NOTES>", r"(?:my |meine |mein )?<NOTES>", r"(?:read|lies)(?: me| mir)? <ART><NOTES>(?: vor| out| aloud| out loud)?", r"was habe ich(?: mir)?(?: alles)? (?:notiert|aufgeschrieben)", r"(?:do i have|have i got|habe ich)(?: any| irgendwelche)? (?:notes|notizen)", r"(?:what notes|which notes|welche notizen) (?:do i have|have i got|are there|habe ich|gibt es)", r"(?:go to|open up|pull up) <ART><NOTES>"]),
        shape(
            "note: {text}",
            &[
                r"(?:add|put|write|save|stick|pop|schreib|schreibe|setz|setze|pack|füg|füge) (?P<text>.+?) (?:to|in|into|on|onto|auf|zu) <ART><NOTES>(?: hinzu| dazu)?",
                r"(?:add|make|create|write|take|save|put|start|new|schreib|mach|mache|erstelle|erstell|neue)(?: me| mir| down| auf)?(?: a| an| ein| eine| the| another)?(?: new| quick| short| neue| kurze)? <NOTE>(?: (?:that says|saying|which says|with the text|with|about|mit dem text|mit))? (?P<text>.+)",
                r"(?:write|jot|take|note|put|scribble|get|schreib|schreibe|notiere|notier)(?: this| that| it| das| dir| mir)*(?: down| auf)(?: that| dass)? (?P<text>.+)",
                r"(?:add|put)(?: this| that)? (?:to|in|into|on) <ART><NOTES>(?: that| saying)? (?P<text>.+)",
                r"(?:in |into |to |auf |in )<ART><NOTES>(?: write| put| add| schreib| notier)? (?P<text>.+)",
                r"(?:note|notiz|memo)(?: to self| an mich| für mich)? (?P<text>.+)",
                r"(?:remember|merk dir|merke dir) (?:this |that |das )?(?:in|into|on|auf|in) <ART><NOTES> (?P<text>.+)",
                r"(?:notiere|notier|schreib|schreibe)(?: dir| mir)?(?: mal)?(?: auf)? (?P<text>.+?)(?: auf| in (?:mein|das) notizbuch| in (?:meine|die) notizen)",
            ],
        ),
        // --------------------------------------------------------------- sleep timer ----
        idea("how long is left on the sleep timer", &["<TIMER>", "how long|how much time|how much longer|how many minutes|time left|left|remaining|still|when|status|what time|is there|is the|is my|is a|do i have|did i set|have i set|wie lange|wie viel zeit|wie viele minuten|wann|noch|übrig|restzeit|läuft|gibt es|habe ich|ist der|ist ein"], "set a|set the|start|make|create|<STOP>|<DEL>|dont need|do not need|no longer"),
        shape("how long is left on the sleep timer", &[r"(?:when|wann) (?:does|will|is|geht|stoppt|endet|hört)(?: the| my| die)? (?:music|musik|playback|sound) (?:going to |gonna )?(?:stop|end|go off|turn off|switch off|shut off|aus|auf|ausgehen)(?: gehen)?", r"(?:how long|wie lange)(?: does| will| is| läuft| geht| spielt)?(?: the| my| die)? (?:music|musik)(?: still| keep| going to| gonna| noch)* (?:play|playing|go|going|run|running|on|laufen|an|weiter)(?: for)?"]),
        idea("cancel the sleep timer", &["<TIMER>", "<STOP>|<DEL>|off|aus|weg|ab|abbrechen|beenden|ausschalten|ausmachen|dont need|do not need|dont want|do not want|no longer|no more|not anymore|any more|never mind|nevermind|forget about|kein|keinen|nicht mehr|doch nicht|brich|löschen|stoppen|deaktivieren"], "how long|how much|when|wie lange|wann|<SPAN>"),
        shape("cancel the sleep timer", &[r"(?:keep|leave|let) (?:the |my )?music (?:playing|on|running|going)(?: after all| all night| tonight)?(?: forget the timer| cancel the timer)?", r"(?:dont|do not) (?:stop|turn off|pause) the music (?:after all|anymore|later|tonight)", r"(?:lass|lasse) die musik (?:doch )?(?:an|laufen|weiterlaufen)(?: heute nacht)?"]),
        idea("sleep timer {span}", &["<TIMER>", "<SPAN>"], "<STOP>|<DEL>|how long|wie lange"),
        shape(
            "sleep timer {span}",
            &[
                r"(?:in|after|nach) <SPAN> (?:stop|pause|turn off|switch off|shut off|kill|end|cut|mute)(?: the| my)? (?:music|playback|sound|song|songs|audio|player|spotify)(?: off)?",
                r"(?:turn|switch|shut|cut|power)(?: the| my)? (?:music|sound|playback|audio|player|spotify) off (?:in|after) <SPAN>",
                r"(?:turn off|switch off|shut off|stop|pause|end|kill|cut|mute)(?: the| my| playing| playing the)? (?:music|sound|playback|audio|song|songs|player|spotify)(?: off)? (?:in|after) <SPAN>(?: from now)?",
                r"(?:let|play|keep|leave) (?:the |my )?(?:music|song|songs|playback|spotify)(?: play| playing| run| running| going| on)?(?: for)?(?: another| the next)? <SPAN>(?: more| longer)?(?: and)?(?: then)?(?: stop| stop it| turn it off| switch it off| off| pause| pause it)?",
                r"(?:stop|pause|end|kill) (?:playing|playback)(?: music)? (?:in|after) <SPAN>",
                r"(?:music|musik|sound|playback) (?:off|aus|stop|stopp|pause) (?:in|after|nach) <SPAN>",
                r"(?:mach|mache|schalt|schalte|stell|stelle|dreh)(?: die| mir die)? (?:musik|wiedergabe|playlist) (?:in|nach) <SPAN> (?:aus|ab|stumm|leise)",
                r"(?:stopp|stoppe|pausiere|pausier|beende|stop)(?: die| meine)? (?:musik|wiedergabe) (?:in|nach) <SPAN>",
                r"(?:in|nach) <SPAN>(?: soll| kann| bitte)?(?: die)? musik (?:aus|ausgehen|stoppen|aufhören|ausmachen|aus sein|ausschalten)",
                r"(?:die )?musik(?: soll| kann)? (?:in|nach) <SPAN> (?:aus|ausgehen|stoppen|aufhören|ausmachen|aus sein)",
                r"(?:i want to |ich will |ich möchte )?(?:fall asleep|go to sleep|einschlafen)(?: to music| with music| zu musik| mit musik)?(?: stop it| stop the music| turn it off)? (?:in|after|for|in|nach) <SPAN>",
                r"<SPAN> (?:and )?then (?:stop|pause|turn off|kill)(?: the)? (?:music|playback|sound)(?: off)?",
                r"<SPAN> (?:of music|more music|musik|noch musik)(?: then stop| then off| and then stop| dann aus| und dann aus| dann stopp)?",
                r"(?:musik|music)(?: noch| for)? <SPAN>(?: dann aus| und dann aus| then off| then stop| and then off)",
            ],
        ),
        // --------------------------------------------------------------------- rules ----
        shape("list my rules", &[r"<SHOW> <ART>(?:automatic |active |saved |current |whenever |of my )*(?:rules|automations|whenever rules|regeln|automatisierungen)", r"(?:what|which|welche) (?:rules|automations|regeln)(?: do i have| have i (?:got|set up|made|created)| are there| are active| are set| are set up| exist| habe ich| gibt es| sind aktiv)", r"(?:what are|was sind) (?:my|meine) (?:rules|automations|regeln)", r"(?:my |meine )?(?:rules|automations|regeln)", r"(?:do i have|have i got|habe ich)(?: any| irgendwelche)? (?:rules|automations|regeln)(?: set up| eingerichtet)?", r"(?:what|which) (?:rules|automations) (?:did|have) i (?:set up|make|create|made|created)"]),
        shape(
            "delete rule {n}",
            &[
                r"(?:<DEL>|turn off|switch off|disable|stop|deaktiviere|cancel|kill) <ART>(?:rule|automation|regel)(?: number| nummer| no| nr)? (?P<n>\d+|one|two|three|four|five|eins|zwei|drei|vier|fünf)",
                r"(?:<DEL>|turn off|switch off|disable|stop|deaktiviere|cancel|kill) <ART>(?P<n>first|second|third|fourth|fifth|1st|2nd|3rd|4th|5th|erste|zweite|dritte|vierte|fünfte|ersten|zweiten|dritten) (?:rule|automation|regel)",
                r"(?:rule|regel|automation)(?: number| nummer| nr)? (?P<n>\d+|one|two|three|four|five|eins|zwei|drei|vier|fünf) (?:delete|remove|off|löschen|weg|entfernen|can go|kann weg|should go)",
                r"(?:i )?(?:dont|do not|no longer) (?:need|want) (?:rule|regel)(?: number| nummer)? (?P<n>\d+|one|two|three|four|five)(?: anymore| any more)?",
                r"(?:ich brauche? )?(?:die )?regel(?: nummer)? (?P<n>\d+|eins|zwei|drei|vier|fünf) (?:nicht mehr|kann weg|löschen|entfernen)",
                r"(?:the )?(?P<n>first|second|third|fourth|fifth) (?:rule|one) (?:can go|should go|delete it|remove it)",
            ],
        ),
        shape(
            "whenever {app} starts {then}",
            &[
                r"(?:(?:make|create|add|set up|new|neue|erstelle|erstell|mach)(?: me| mir)?(?: a| an| eine| the)?(?: new| neue)? (?:rule|automation|regel|automatisierung)(?: that| so that| where| dass| damit)? )?(?:every ?time|each time|any ?time|as soon as|once|the moment|if|when|whenever|always when|sobald|jedes mal wenn|immer wenn|wenn|falls)(?: i (?:start|open|launch|run|start up|open up|fire up)| ich)? (?P<app>[a-z0-9äöüß][a-z0-9äöüß .+-]{1,24}?)(?: (?:starts|opens|launches|is started|is opened|is launched|gets started|gets opened|gets launched|comes up|comes on|boots|runs|is running|starts up|opens up|turns on|startet|gestartet wird|aufgeht|geöffnet wird|öffnet|öffne|starte|läuft|angeht|aufmache|hochfährt))?(?: up)?(?: then| dann| i want you to| you should| you need to| please| automatically)* (?P<then>(?:mute|unmute|pause|stop|set|open|close|turn|play|show|lower|raise|start|resume|skip|dim|mach|öffne|schließe|stell|spiel|pausiere|stoppe) .+)",
                r"(?P<then>(?:mute|unmute|pause|stop|set|open|close|turn|play|show|lower|raise|start|resume|skip|dim) .+?) (?:every ?time|each time|any ?time|whenever|when|as soon as|if|once|sobald|immer wenn|jedes mal wenn|wenn) (?:i (?:start|open|launch|run) )?(?P<app>[a-z0-9äöüß][a-z0-9äöüß .+-]{1,24}?)(?: (?:starts|opens|launches|is started|is opened|is launched|gets started|gets opened|comes up|comes on|runs|is running|starts up|opens up|startet|gestartet wird|aufgeht|geöffnet wird|läuft))?(?: up)?",
            ],
        ),
        // ---------------------------------------------------------------- brightness ----
        idea("how bright are my monitors", &["<SCREENS>|<BRIGHT>", "how bright|how dark|what brightness|which brightness|brightness level|what level|what percent|what percentage|current brightness|status|wie hell|wie dunkel|welche helligkeit|wie viel prozent|auf wie viel|aktuelle helligkeit"], "make|put|turn|stell|mach|setz|change|<NUM>"),
        shape("how bright are my monitors", &[r"(?:what is|whats|what are|tell me|check|show|show me|zeig|zeig mir|sag mir|wie ist)(?: the| my| die)? (?:current |aktuelle )?<BRIGHT>(?: of| on| for| der| von)?(?: the| my| all| all my| den| meiner| meinen)? ?(?:<SCREENS>)?(?: right now| at| set to| currently| gerade| eingestellt)*", r"<BRIGHT> (?:status|check|level|levels)"]),
        shape(
            "brightness {n} percent",
            &[
                r"(?:make|set|turn|put|change|adjust|bring|get|stell|stelle|mach|mache|setz|setze|regel|regle|dreh|drehe)(?: the| my| all| all my| all the| all of my| die| meine| alle)? (?:<SCREENS>|<BRIGHT>)(?: of (?:the |my |all |all my )?<SCREENS>| der <SCREENS>)? (?:to|at|down to|up to|on|auf) <NUM>(?: <BRIGHT>| brightness| hell)?",
                r"<NUM> (?:<BRIGHT>|brightness)(?: please| on (?:the|my|all) <SCREENS>| for (?:the|my|all) <SCREENS>)?",
                r"<BRIGHT>(?: level)?(?: of| on| for)?(?: the| my| all| all my)? ?(?:<SCREENS>)? (?:to|at|auf|on)? ?<NUM>",
                r"(?:<SCREENS>|<BRIGHT>) (?:at|on|to|auf) <NUM>(?: <BRIGHT>)?",
                r"(?:dim|darken|brighten|lower|raise|turn down|turn up|reduce|increase|dimme|dimm)(?: the| my| all| all my| die| meine| alle)? (?:<SCREENS>|<BRIGHT>)(?: brightness)? (?:to|down to|up to|auf) <NUM>",
                r"(?:<BRIGHT>)(?: der| von| aller)?(?: <SCREENS>)? (?:auf|zu) <NUM>(?: stellen| setzen| einstellen)?",
                r"(?:<SCREENS>) (?:auf|to) <NUM>(?: <BRIGHT>| hell| helligkeit)?(?: stellen| setzen)?",
            ],
        ),
        shape(
            "make the screens brighter",
            &[
                r"(?:make|turn|set|get|mach|mache|stell|stelle|dreh|drehe)(?: the| my| all| all the| all my| die| meine| alle| it| mal| es)* (?:<SCREENS>|<BRIGHT>)?(?: a bit| a little| a little bit| a notch| a touch| slightly| somewhat| ein bisschen| etwas| ein stück| ein wenig)* (?:brighter|lighter|up|higher|heller|hoch|höher|rauf)",
                r"(?:brighten|increase|raise|boost|turn up|bump up|crank up|up|erhöhe|erhöh)(?: up)?(?: the| my| all| all the| all my| die| meine)? (?:<SCREENS>|<BRIGHT>)(?: brightness)?(?: a bit| a little| a notch| up| slightly| etwas| ein bisschen)*",
                r"(?:<SCREENS>|<BRIGHT>) (?:brighter|up|higher|heller|hoch|höher|rauf)(?: a bit| a little| etwas| ein bisschen)*",
                r"(?:a bit |a little |etwas |ein bisschen )?(?:brighter|heller|more brightness|more light|mehr helligkeit|mehr licht)(?: <SCREENS>| screens| please| bitte)?",
                r"(?:its|it is|thats|that is|es ist|das ist|ist|is) (?:way |a bit |a little |viel |etwas )?too (?:dark|dim)(?: in here| on (?:the|my) <SCREENS>)?",
                r"(?:es ist |das ist |ist )?(?:viel |etwas |ein bisschen )?zu dunkel",
                r"i (?:cant|can not|cannot) see (?:anything|the screen|my screen|a thing)(?: its too dark| make it brighter)?",
            ],
        ),
        shape(
            "make the screens darker",
            &[
                r"(?:make|turn|set|get|mach|mache|stell|stelle|dreh|drehe)(?: the| my| all| all the| all my| die| meine| alle| it| mal| es)* (?:<SCREENS>|<BRIGHT>)?(?: a bit| a little| a little bit| a notch| a touch| slightly| somewhat| ein bisschen| etwas| ein stück| ein wenig)* (?:darker|dimmer|down|lower|less bright|dunkler|runter|niedriger|weniger hell)",
                r"(?:darken|decrease|lower|reduce|turn down|bring down|tone down|drop|verringere|senke|senk|reduziere)(?: down)?(?: the| my| all| all the| all my| die| meine)? (?:<SCREENS>|<BRIGHT>)(?: brightness)?(?: a bit| a little| a notch| down| slightly| etwas| ein bisschen)*",
                r"(?:dim|dimme|dimm)(?: the| my| all| all the| all my| die| meine| alle)? (?:<SCREENS>|<BRIGHT>)? ?(?:a bit|a little|a little bit|a notch|slightly|somewhat|down a bit|etwas|ein bisschen|ein wenig)",
                r"(?:<SCREENS>|<BRIGHT>) (?:darker|dimmer|down|lower|dunkler|runter|niedriger)(?: a bit| a little| etwas| ein bisschen)*",
                r"(?:a bit |a little |etwas |ein bisschen )?(?:darker|dimmer|dunkler|less brightness|less light|weniger helligkeit|weniger licht)(?: <SCREENS>| screens| please| bitte)?",
                r"(?:its|it is|thats|that is|es ist|das ist|ist|is) (?:way |a bit |a little |viel |etwas )?too bright(?: in here| on (?:the|my) <SCREENS>)?",
                r"(?:es ist |das ist |ist )?(?:viel |etwas |ein bisschen )?zu hell",
                r"(?:my eyes hurt|this is blinding me|the <SCREENS> (?:are|is) blinding me)",
            ],
        ),
        shape("dim the side monitors", &[r"(?:dim|darken|dimme|dimm|verdunkle|verdunkel)(?: just| only| nur)?(?: the| my| die| meine)?(?: two| both| beiden| zwei)? (?:side|outer|other two|left and right|seitlichen|äußeren|seiten|beiden anderen|anderen beiden)(?: |-)?<SCREENS>(?: only| nur)?", r"(?:make|turn|mach)(?: just| only| nur)?(?: the| my| die| meine)?(?: two| both| beiden)? (?:side|outer|seitlichen|äußeren|seiten)(?: |-)?<SCREENS> (?:dark|dim|dunkel)", r"(?:side|outer|seitliche|äußere|seiten)(?: |-)?<SCREENS> (?:dim|dimmen|dunkel|dark)"]),
        // ------------------------------------------------------------------- layouts ----
        shape("list my layouts", &[r"<SHOW> <ART>(?:saved |desk |window |of my )*(?:layouts|desk layouts|window layouts|fensterlayouts|anordnungen|fensteranordnungen)", r"(?:what|which|welche) (?:desk |window )?(?:layouts)(?: do i have| have i (?:got|saved)| are there| are saved| exist| did i save| habe ich| gibt es| sind gespeichert)", r"(?:what are|was sind) (?:my|meine) (?:desk |window )?layouts", r"(?:my |meine )?(?:desk |window )?layouts", r"(?:do i have|have i got|habe ich)(?: any| irgendwelche)? (?:saved )?(?:desk |window )?layouts(?: saved| gespeichert)?"]),
        shape("delete the {name} layout", &[r"<DEL> <ART>(?:saved )?(?:desk |window )?(?:layout|anordnung)(?: called| named| namens)? (?P<name>[a-zäöüß0-9 ]{2,30})", r"<DEL> <ART>(?:saved )?(?P<name>[a-zäöüß0-9 ]{2,30}?) (?:desk |window )?(?:layout|anordnung)", r"(?:the |das )?(?P<name>[a-zäöüß0-9 ]{2,30}?) layout (?:can go|should go|delete it|remove it|löschen|kann weg|weg)"]),
        shape(
            "restore the {name} layout",
            &[
                r"(?:switch to|go to|change to|use|activate|open|open up|put up|put|get|recall|pull up|bring up|bring back|show|start|launch|run|engage|restore|load|apply|set up|give me|i need|i want|wechsle zu|wechsel zu|wechsle auf|wechsel auf|nimm|lade|lad|aktiviere|öffne|starte|hol|stell)(?: me| mir)? <ART>(?:saved |old |gespeicherte |gespeichertes )?(?P<name>[a-zäöüß0-9]{2,20}(?: [a-zäöüß0-9]{2,20})?)(?: |-)?(?:layout|desk layout|window layout|desk setup|window setup|fensterlayout|anordnung|fensteranordnung)(?: again| back| back up| wieder| her| wieder her| wiederherstellen| laden)*",
                r"(?:put|move|arrange|set|place|bring|get|stell|ordne|schieb|pack)(?: all)?(?: my| the| meine| die| alle)? (?:windows|fenster|apps|programme)(?: back)? (?:like|as|how|the way|into|in|to|for|wie|so wie|ins|im|in das|auf|für)(?: in| for| they were in| they were for| it was in| bei| beim| the| my| das| dem)* (?P<name>[a-zäöüß0-9]{2,20}(?: [a-zäöüß0-9]{2,20})?)(?: |-)?(?:layout|setup|mode|modus|anordnung)?(?: again| an| hin| wieder)*",
                r"(?P<name>[a-zäöüß0-9]{2,20}(?: [a-zäöüß0-9]{2,20})?) (?:layout|anordnung) (?:please|bitte|wiederherstellen|laden|restore|again|back|wieder)",
                r"(?:back to|zurück zum|zurück zu)(?: the| my| dem| meinem)? (?P<name>[a-zäöüß0-9]{2,20}(?: [a-zäöüß0-9]{2,20})?) (?:layout|anordnung)",
            ],
        ),
        // ("Save that desk layout as standard", "save my desk as gaming": Sir, 2026-11-13. The
        // language model made a protocol called "no_think" of the first.)
        shape(
            "save this layout as {name}",
            &[
                r"(?:save|store|keep|capture|remember|record|snapshot|speicher|speichere|sichere|merk dir|merke dir) <ART>(?:current |whole |aktuelle |aktuelles |aktuellen )?(?:desk ?layout|desk setup|desk arrangement|desk|window layout|window arrangement|window setup|windows|layout|setup|workspace|arrangement|schreibtisch ?layout|schreibtisch|fensterlayout|fenster ?anordnung|anordnung|fenster) (?:as|under|als|unter)(?: the| a| my| mein| meine)? (?P<name>[a-zäöüß0-9]{2,20}(?: [a-zäöüß0-9]{2,20})??)(?: desk layout| layout| setup)?",
                r"(?:save|store|keep|capture|speicher|speichere|sichere) (?:this|that|it|das|dies|es) (?:as|als)(?: the| a| my| mein| meine| ein| eine)? (?P<name>[a-zäöüß0-9]{2,20}) (?:desk ?|window ?)?(?:layout|anordnung)",
            ],
        ),
        // (A desk layout asked for as his own, by "desk" or "my": his by that name, kept or
        // not. He says so when there is none; the HUD's own "standard" is not meant.)
        shape(
            "restore the desk layout {name}",
            &[
                r"(?:switch to|go to|change to|use|load|apply|activate|restore|bring back|bring up|set up|give me|i need|i want|wechsle zu|wechsel zu|wechsle auf|wechsel auf|nimm|lade|lad|aktiviere|stell|stelle)(?: me| mir)? <ART>(?:saved |gespeicherte |gespeichertes )?(?:desk ?layout|schreibtisch ?layout)(?: called| named| namens)? (?P<name>[a-zäöüß0-9]{2,20})(?: again| back| wieder| her| wieder her| wiederherstellen| laden)*",
                r"<ART>(?:desk ?layout|schreibtisch ?layout)(?: called| named| namens)? (?P<name>[a-zäöüß0-9]{2,20})(?: laden| wiederherstellen)?",
                r"(?:switch to|go to|change to|use|load|apply|activate|restore|bring back|give me|wechsle zu|wechsel zu|wechsle auf|nimm|lade|lad|aktiviere) (?:my|mein|meine|meinem|meinen) (?:saved |old |gespeichertes )?(?P<name>[a-zäöüß0-9]{2,20})(?: |-)?(?:desk ?layout|layout|anordnung)",
                r"<ART>(?P<name>[a-zäöüß0-9]{2,20}) (?:desk ?layout|schreibtisch ?layout)",
            ],
        ),
        // ("Jay Layout Standard": the name first, as he said it on 2026-11-13.)
        shape(
            "layout {name}",
            &[
                r"(?:window |fenster ?)?layout (?P<name>[a-zäöüß0-9]{2,20})",
                r"(?:switch to|go to|change to|use|load|apply|activate|restore|wechsle zu|wechsel zu|nimm|lade|lad|aktiviere) <ART>(?:window |fenster ?)?(?:layout|anordnung)(?: called| named| namens)? (?P<name>[a-zäöüß0-9]{2,20})(?: again| wieder| laden| wiederherstellen)?",
            ],
        ),
        // ----------------------------------------------------------------- dashboard ----
        shape("portrait dashboard off", &[r"(?:<STOP>|hide|remove|take away|take down|get rid of|turn off|switch off|blende|verstecke|entferne)(?: aus)? <ART>(?:portrait |upright |vertical |side |hochkant ?|hochformat ?)?(?:dashboard|dash board|info ?board)(?: aus| ab| weg)?", r"(?:portrait |upright |vertical |side |hochkant ?)?(?:dashboard|dash board) (?:off|aus|weg|away|ausblenden|ausschalten|schließen|hide|close)", r"(?:turn|switch|schalte|schalt|mach|mache) <ART>(?:portrait |upright |vertical |hochkant ?)?(?:dashboard|dash board) (?:off|aus|weg|zu)", r"(?:no more|kein|i dont need the|i dont want the) (?:portrait |upright )?dashboard(?: anymore| mehr)?"]),
        shape("portrait dashboard", &[r"(?:<START>|show|show me|put|put up|display|zeig|zeige|zeig mir|blende)(?: me| mir| up| on| ein)? <ART>(?:portrait |upright |vertical |side |hochkant ?|hochformat ?)?(?:dashboard|dash board|info ?board)(?: on| an| ein| einblenden| einschalten| anzeigen)?(?: (?:on|auf) (?:the |my |dem |den |meinem )?(?:upright|vertical|portrait|side|third|hochkant|hochformat|vertikalen|seitlichen|dritten) ?(?:monitor|screen|bildschirm))?(?: an| ein)?", r"(?:portrait |upright |vertical |side |hochkant ?)?(?:dashboard|dash board) (?:on|an|ein|bitte|einblenden|einschalten|anzeigen|show|up)", r"(?:turn|switch|schalte|schalt|mach|mache) <ART>(?:portrait |upright |vertical |hochkant ?)?(?:dashboard|dash board) (?:on|an|ein|auf)", r"(?:upright|vertical|portrait|hochkant) (?:monitor|screen|bildschirm) dashboard(?: on| an)?"]),
        // ------------------------------------------------------------------- reading ----
        shape("stop reading", &[r"<STOP> <ART>(?:reading|read aloud|reading aloud|reading out|vorlesen|lesen)(?: out loud| aloud| out| now| to me)?", r"(?:no more|enough|genug|kein)(?: reading| vorlesen| gelesen| mehr vorlesen)", r"(?:you can |du kannst )?(?:stop|aufhören)(?: reading| vorzulesen| zu lesen| mit dem vorlesen)+(?: now| jetzt)?", r"(?:thats|that is|das ist|das reicht) (?:enough|genug)(?: reading| vorlesen| gelesen)", r"(?:hör|hoer) auf (?:zu lesen|vorzulesen|mit dem vorlesen|mit vorlesen)", r"(?:dont|do not) read (?:any more|anymore|any further|on|the rest)", r"(?:nicht|nichts) (?:mehr |weiter )(?:vorlesen|lesen)"]),
        shape("read on", &[r"(?:read|lies)(?: me| mir)? (?:the |den |das |die )?(?:next|rest|following|remaining|nächsten|nächste|restlichen|rest|weiteren)(?: part| bit| section| paragraph| page| chunk| teil| abschnitt| stück| absatz| of it| of the text| of that| davon| vom text)*(?: vor| to me| aloud)?", r"(?:continue|resume|keep|go on|carry on|proceed|keep on|mach weiter|weiter)(?: with| mit)?(?: the| dem)? (?:reading|read|vorlesen|lesen)(?: aloud| out| it)?", r"(?:read|lies)(?: mal)? (?:on|further|more|some more|a bit more|the next bit|weiter|weiter vor|mehr|noch mehr|noch weiter)", r"(?:weiter ?lesen|weiter vorlesen|lies weiter vor|lies mal weiter|keep reading|continue reading|go on reading|carry on reading)"]),
        shape("read my clipboard", &[r"(?:read|speak|say|tell me)(?: me| out| aloud)?(?: the| my)? <CLIP>(?: out loud| aloud| out| to me| for me)*", r"lies(?: mir)? (?:die |den |das |meine |was in der )?(?:<CLIP>|zwischenablage steht)(?: mal)?(?: laut)?(?: vor)?", r"(?:what does|what do) (?:my|the) (?:clipboard|copied text) say", r"read (?:out )?(?:the text|the thing|the stuff|what) (?:i|ive) (?:just )?copied(?: out loud| aloud| to me)?"]),
        shape(
            "read this to me",
            &[
                r"(?:read|speak|say)(?: me| out| aloud)?(?: the| this| that| my)?(?: selected| highlighted| marked)? (?:text|selection|passage|paragraph|part|bit|section|sentence)(?: i (?:have |just )?(?:selected|highlighted|marked))?(?: out loud| aloud| out| to me| for me)+",
                r"(?:read|speak)(?: out| aloud| me)? (?:what i|what ive|what i have) (?:just )?(?:selected|highlighted|marked)(?: out loud| aloud| to me| for me)?",
                r"(?:read|speak) (?:this|that|it) (?:out|out loud|aloud|to me|for me|loud|to me out loud)",
                r"(?:read|speak)(?: out| me)?(?: the)? (?:selected|highlighted|marked) (?:text|part|bit|passage|paragraph|section)(?: out loud| aloud| to me)?",
                r"(?:read|speak) (?:out )?(?:the |my )?selection(?: out loud| aloud| to me| for me)?",
                r"lies(?: mir)? (?:das|dies|diesen text|den text|die auswahl|die markierung|den markierten text|den ausgewählten text|die stelle|den abschnitt|das hier|das markierte|den absatz)(?: mal)?(?: laut)? vor",
                r"(?:lies vor|lies das vor|vorlesen|das vorlesen|lies mal vor|lies mir vor)",
                r"(?:read aloud|read out loud|read it out|read that out|read this out)(?: what i (?:selected|marked|highlighted)| the selection| to me)?",
                r"(?:can i hear|let me hear|ich will hören) (?:this|that|the selected text|the selection|what i selected|das|die auswahl)",
            ],
        ),
    ]
}

/// The kit's second half: the coach, the hearing log, guests, the recap, the internet doctor,
/// the privacy board, the changes list, backups, where the subtitles stand, window places,
/// notifications, what he has selected, and the switches.
fn second_half() -> Vec<Way> {
    let mut ways = vec![
        // ------------------------------------------------------------------- the coach ----
        idea(
            "practise my pronunciation",
            &["pronunciation|aussprache|accent|akzent|pronounce|pronouncing|aussprechen|my english|mein englisch|speaking english|english speaking", "practise|practice|practicing|practising|train|training|coach|coaching|work on|improve|improving|exercise|drill|lesson|help me|help with|teach me|üben|übe|üb|trainieren|trainiere|verbessern|verbessere|hilf mir|lektion|übung"],
            "<STOP>|<NOMORE>|done|finished|fertig|learnt|learned|forget|what have you|what did you|vergiss|genug",
        ),
        shape(
            "practise my pronunciation",
            &[
                r"<START> <ART>(?:pronunciation |speech |accent |speaking )?(?:coach|coaching|trainer|practice|practise|training|lesson|exercise)(?: session| mode)?",
                // ("Jay opened your pronunciation code", 2026-11-13: the coach as the recogniser
                // wrote it. After "pronunciation" nothing else is meant.)
                r"<START> <ART>(?:pronunciation|aussprache) (?:code|codes|couch|coat|coats|coast|coke|coach|coaches)",
                r"(?:coach me|lets practise|lets practice|lets train|practise with me|practice with me|übe mit mir|lass uns üben|trainiere mit mir|trainier mit mir)",
                r"(?:starte|öffne|beginne|mach|aktiviere)(?: mal)? <ART>(?:aussprachetraining|aussprachetrainer|aussprachecoach|aussprache training|aussprache coach|sprechtraining|ausspracheübung|aussprache übung)(?: an| auf)?",
                r"(?:aussprachetraining|aussprachetrainer|aussprachecoach|aussprache training|aussprache üben|sprechtraining)(?: starten| an| beginnen)?",
            ],
        ),
        shape(
            "stop the coach",
            &[
                r"<STOP> <ART>(?:pronunciation |speech |accent |speaking |aussprache ?)?(?:coach|coaching|trainer|practice|practise|training|lesson|exercise|session|übung|aussprachetraining|aussprachetrainer|aussprachecoach)(?: session| mode)?",
                r"(?:no more|enough|genug|schluss mit)(?: of)?(?: the| dem| der)? ?(?:pronunciation |aussprache ?)?(?:practice|practise|practising|practicing|training|coaching|coach|übung|üben|aussprachetraining)(?: for today| for now| für heute)?",
                r"(?:im |i am |we are |were |ich bin |wir sind )?(?:done|finished|fertig) (?:with|mit)(?: the| my| dem| der| meinem)? ?(?:pronunciation |aussprache ?)?(?:practice|practise|practising|practicing|training|coaching|coach|lesson|übung|üben|aussprachetraining)(?: for today| for now| für heute)?",
                r"(?:hör|hoer) auf (?:mit )?(?:dem |der )?(?:aussprachetraining|aussprache ?training|aussprache ?coach|coaching|coach)",
                r"(?:aussprachetraining|aussprache ?training|aussprache ?coach|aussprachetrainer) (?:beenden|stoppen|aus|schließen|ende|stopp)",
                r"(?:beende|stoppe|stopp|schließe|schließ)(?: mal)? <ART>(?:aussprachetraining|aussprachetrainer|aussprachecoach|aussprache training|aussprache coach|coach|training)",
            ],
        ),
        shape(
            "stop the coach",
            &[
                r"(?:thats|that is|that will be|thatll be|das ist|das reicht|das wars|reicht) ?(?:enough|genug|it|all|alles)?(?: for today| for now| für heute| erstmal)?",
                r"(?:im |i am |we are |were |ich bin |wir sind )?(?:done|finished|fertig)(?: for today| for now| here| für heute)?",
                r"(?:lets|let us|lass uns|wir können) (?:stop|end|finish|wrap up|call it a day|aufhören|schluss machen)(?: here| there| for today| für heute)?",
                r"(?:enough|genug|aufhören|schluss)(?: practising| practicing| practice| practise| training| üben)?(?: for today| for now| für heute)?",
            ],
        )
        .while_(crate::kit_voice::coach_active),
        // ------------------------------------------------------------- the hearing log ----
        shape(
            "what did you just hear",
            &[
                r"what (?:did|do) you (?:just |think you |actually )?(?:hear|heard|understand|understood|get|catch|pick up|make of that)(?: just now| there| from me| me say| right now| a moment ago)?",
                r"what (?:did|do) you think i (?:just )?said",
                r"what did i (?:just )?say(?: just now| according to you)?",
                r"what was the last thing (?:you heard|you understood|i said|you picked up)",
                r"(?:did you|have you) (?:hear|heard|get|got|understand|understood|catch|caught) (?:me|that) (?:right|correctly|properly)",
                r"(?:how|what) did you (?:hear|understand) (?:me|that|it)(?: just now)?",
                r"was hast du(?: da| gerade| eben| jetzt| zuletzt)* (?:gehört|verstanden|mitbekommen)",
                r"was habe ich(?: gerade| eben)? gesagt",
                r"hast du (?:mich|das) (?:richtig|korrekt) (?:gehört|verstanden)",
                r"(?:tell me|show me|sag mir|zeig mir) what you (?:just )?(?:heard|understood)(?: just now)?",
                r"(?:the |your )?last thing you heard",
            ],
        ),
        idea(
            "show the hearing log",
            &["hearing log|hear log|heard log|hearing history|listening log|listening history|recognition log|transcription log|speech log|hearing protocol|hörprotokoll|hör protokoll|hörlog|hörverlauf|hearing list", "<SHOW>|whats|what is|pull|bring|need|want|zeig|zeige|öffne|mach|sehen|auf"],
            "<DEL>|<STOP>",
        ),
        shape(
            "show the hearing log",
            &[
                r"<ART>(?:hearing log|hearing history|hörprotokoll|hörlog|hörverlauf)",
                r"(?:<SHOW> )?(?:everything|all|what|the things|the stuff|alles) (?:that )?you(?: have|ve)? (?:heard|understood|picked up)(?: from me)?(?: today| so far| lately| recently| this session)?",
                r"what have you (?:been )?(?:heard|hearing|understood|understanding|picked up)(?: from me)?(?: today| so far| lately| recently)?",
                r"(?:zeig|zeige|sag)(?: mir)? (?:was|alles was) du(?: heute| bisher| so)? (?:gehört|verstanden) hast",
                r"was hast du(?: heute| bisher| so| alles)+ (?:gehört|verstanden)",
                r"(?:how well|how good) (?:do|did|can) you (?:hear|understand) me",
            ],
        ),
        // ---------------------------------------------------------------------- guests ----
        shape(
            "this is my friend {name} remember his voice",
            &[
                r"(?:this is|here is|heres|meet|say hi to|say hello to|das ist|hier ist|das hier ist)(?: my| mein| meine)?(?: friend| best friend| buddy| mate| pal| brother| sister| girlfriend| boyfriend| mom| mum| mother| dad| father| cousin| kumpel| freund| freundin| bruder| schwester| mama| papa| mutter| vater)? <WHO>(?: and| und| so| also| now| jetzt)?(?: please)? (?:remember|learn|save|store|keep|memori[sz]e|recogni[sz]e|get to know|merk dir|merke dir|lerne|lern|speicher|speichere|erkenne)(?: his| her| their| this| that| the| seine| ihre| die| diese)? (?:voice|stimme)(?: too| as well| auch)?",
                r"(?:remember|learn|save|store|memori[sz]e|recogni[sz]e|merk dir|merke dir|lerne|lern|speicher|speichere)(?: the| die)? (?:voice of|stimme von)(?: my| meinem| meiner)?(?: friend| buddy| mate| brother| sister| freund| freundin| kumpel| bruder| schwester)? <WHO>",
                r"(?:remember|learn|save|store|memori[sz]e|recogni[sz]e|merk dir|merke dir|lerne|lern|speicher|speichere)(?: my| meinen| meine)?(?: friend| buddy| mate| brother| sister| freund| freundin| kumpel| bruder| schwester)? (?P<name>[a-zäöüß]{2,20}?)s? (?:voice|stimme)",
                r"(?:add|register|set up|enrol|enroll|save|füge|füg|speicher|speichere)(?: my friend| my buddy| my brother| my sister| meinen freund| meine freundin)? <WHO> (?:as|als)(?: a| an| my| einen| meinen)? (?:new )?(?:guest|known guest|known voice|friend|voice|gast|freund|bekannte stimme)(?: hinzu)?",
                r"(?:get to know|meet|lerne|lern) (?:my friend |my buddy |my brother |my sister |meinen freund |meine freundin |meinen kumpel )?<WHO>(?: kennen)?",
                r"<WHO> (?:is here|is visiting|is over|is with me|ist da|ist hier|ist zu besuch)(?: today| heute)?(?: and| und| so| also)? (?:remember|learn|save|merk dir|lerne|lern|speichere)(?: his| her| their| seine| ihre)? (?:voice|stimme)",
                r"(?:learn|remember|save) (?:a |the )?(?:new )?voice(?: of| for| from)?(?: my friend| my buddy)? <WHO>",
                r"(?:new|neuer|neue) (?:guest|voice|gast|stimme) <WHO>",
            ],
        )
        .of_people(),
        shape(
            "{name} may only control the music",
            &[
                r"<WHO> (?:may|can|is allowed to|should|gets to|is permitted to|darf|kann|soll) (?:only|just|nur)(?: be able to)? ?(?:control|change|use|touch|do|manage|handle|steuern|ändern|bedienen)?(?: the| my| die)? ?(?:music|songs|player|playback|musik)(?: only| and nothing else| nothing else| steuern| ändern| bedienen| und sonst nichts| sonst nichts)*",
                r"<WHO> (?:may|can|is allowed to|should|darf|kann|soll) (?:control|change|use|touch|manage|handle)(?: only| just)? (?:the |my )?(?:music|songs|player|playback)(?: only| and nothing else| nothing else| but nothing else)+",
                r"(?:let|allow|permit|lass|erlaube)(?: my friend| my buddy)? <WHO> (?:only |just |nur )(?:to )?(?:control|change|use|touch|manage|handle|do)?(?: the| my| die)? ?(?:music|songs|player|playback|musik)(?: only| and nothing else| nothing else| steuern| bedienen| ändern)*",
                r"(?:only|just|nur) (?:let|allow|give|lass|erlaube) <WHO> (?:to )?(?:control|change|use|touch|access to|steuern)?(?: the| my| die)? ?(?:music|songs|player|playback|musik)(?: steuern| bedienen)?",
                r"(?:restrict|limit|beschränke|begrenze) <WHO> (?:to|auf)(?: just| only| nur)?(?: the| die)? (?:music|music controls?|songs|player|playback|musik)",
                r"(?:give|gib) <WHO> (?:music only|only music|music access only|only music access|music rights only|only access to the music|access to the music only|nur musik|nur zugriff auf die musik|nur musikrechte)(?: access| rights| permissions)?",
                r"<WHO> (?:only gets|gets only|just gets|only has|bekommt nur|hat nur) (?:the |access to the |control of the |zugriff auf die |die )?(?:music|musik)",
                r"(?:music only|only music|nur musik) (?:for|für) <WHO>",
            ],
        )
        .of_people(),
        shape(
            "{name} may do everything",
            &[
                r"<WHO> (?:may|can|is allowed to|should be able to|gets to|is permitted to|darf|kann|soll) (?:do|use|control|change|access|touch|command|ask for|ask you)?(?: you)? ?(?:everything|anything|it all|all of it|whatever (?:he|she|they) wants?|alles)(?: too| as well| now| machen| steuern| benutzen| auch)*",
                r"(?:let|allow|permit|lass|erlaube)(?: my friend| my buddy)? <WHO> (?:to )?(?:do|use|control|change|access|touch|command)?(?: you)? ?(?:everything|anything|it all|whatever (?:he|she|they) wants?|alles)(?: machen| steuern| benutzen)?",
                r"(?:give|grant|gib) <WHO> (?:full|complete|total|all|unrestricted|volle|vollen|alle) (?:access|control|rights|permissions|rechte|zugriff|kontrolle)",
                r"<WHO> (?:gets|has|should have|should get|bekommt|hat) (?:full|complete|total|all|unrestricted|volle|vollen|alle) (?:access|control|rights|permissions|rechte|zugriff|kontrolle)",
                r"(?:obey|listen to|hör auf|höre auf|gehorche) <WHO>(?: too| as well| like me| as you do me| auch| wie mir| wie auf mich)+",
                r"(?:full access|everything|alles|voller zugriff) (?:for|für) <WHO>",
            ],
        )
        .of_people(),
        shape(
            "forget {name}s voice",
            &[
                r"(?:forget|delete|remove|erase|drop|unlearn|wipe|clear|vergiss|lösche|lösch|entferne)(?: the| die)? (?:voice of|voice print of|voiceprint of|stimme von)(?: my friend| my buddy| meinem freund)? <WHO>",
                r"(?:forget|delete|remove|erase|drop|unlearn|wipe|vergiss|lösche|lösch|entferne) (?P<name>[a-zäöüß]{2,20}?)s? (?:voice|voice print|voiceprint|stimme|stimmabdruck)(?: again| wieder)?",
                r"(?:forget|delete|remove|erase|drop|kick out|throw out|vergiss|lösche|lösch|entferne)(?: the| den| my| meinen)? (?:guest|friend|gast|freund) <WHO>(?: again| wieder)?",
                r"(?:forget|remove|delete|drop|vergiss|lösche|entferne) <WHO>(?: again| wieder)? (?:as a guest|as guest|from (?:the |your |my )?(?:guests|guest list|known voices|friends)|als gast|aus (?:der |den |deinen )?(?:gästen|gästeliste))",
                r"<WHO> (?:is no longer|isnt|is not|ist kein|ist nicht mehr) (?:a |my |mein )?(?:guest|friend|welcome|gast|freund|willkommen)(?: anymore| any more| mehr)?",
                r"(?:dont|do not|stop) (?:listen|listening|obey|obeying) (?:to )?<WHO> (?:anymore|any more|any longer)",
            ],
        )
        .of_people(),
        shape(
            "who do you know",
            &[
                r"(?:who|wen)(?: all| alles)? (?:do you|can you|kennst du|kannst du)(?: already| schon| alles| so far)? ?(?:know|recogni[sz]e|tell apart|erkennen|unterscheiden)?(?: already| so far| by voice| by their voice| by their voices| an der stimme| schon| alles)*",
                r"(?:whose voices?|which (?:guests|people|voices|friends)|welche (?:gäste|stimmen|leute|freunde)|wessen stimmen?) (?:do you|can you|kennst du|kannst du)(?: already| schon)? ?(?:know|recogni[sz]e|remember|identify|erkennen|unterscheiden)?(?: already| so far| schon)*",
                r"(?:<SHOW> )?<ART>(?:known |saved |stored |registered |guest |bekannten |gespeicherten )*(?:guests|guest list|guest voices|known voices|voices you know|people you know|voice list|gäste|gästeliste|bekannten stimmen)",
                r"(?:who|wer) (?:is|are|ist|sind|steht) (?:on|in|auf) (?:the|your|my|der|deiner) (?:guest list|guests|list of guests|gästeliste)",
                r"(?:which|what|whose|welche) voices? (?:have you|did you|hast du) (?:learned|learnt|saved|stored|got|gelernt|gespeichert)",
                r"(?:do you know|kennst du) (?:anyone|anybody|any guests|any other voices|other voices|other people|irgendwen|jemanden|andere stimmen|noch jemanden)(?: else| besides me| apart from me| außer mir)?",
                r"(?:who else|wen sonst|wen noch) (?:do you know|can you recogni[sz]e|kennst du|erkennst du)(?: besides me| apart from me| außer mir)?",
                r"how many (?:guests|voices|people) do you know",
            ],
        ),
        shape(
            "who is speaking",
            &[
                r"(?:who|wer) (?:is|was|ist|war) (?:that|this|it|speaking|talking|das|dies)(?: speaking| talking| just now| right now| there| gerade| eben| da)*",
                r"(?:who|wer) (?:just )?(?:spoke|said that|talked|speaks|talks|spricht|redet|hat das gesagt|hat gesprochen|sprach)(?: just now| there| gerade| eben| da)*",
                r"(?:whose|wessen) (?:voice|stimme) (?:is|was|ist|war) (?:that|this|it|das)(?: just now| gerade| eben)?",
                r"(?:who|whom) (?:do|did|can) you (?:hear|think (?:is|was) (?:speaking|talking)|think that (?:is|was))(?: right now| just now| speaking| talking)*",
                r"(?:do you know|can you tell|did you recogni[sz]e|weißt du|erkennst du) (?:who|wer) (?:is|was|that is|that was|this is|i am|im|ich bin|das ist|das war)(?: speaking| talking| spricht| gesprochen hat| that| this)*",
                r"(?:do you|did you|can you) recogni[sz]e (?:me|my voice|this voice|that voice|the voice|the speaker|who is speaking)",
                r"(?:who am i|wer bin ich|is that me|am i speaking|is this me speaking|erkennst du mich|erkennst du meine stimme)",
                r"(?:wen hörst du|wer redet da|wer spricht da|wer war das|wer ist das)(?: gerade| eben)?",
            ],
        )
        .asks(),
        // ------------------------------------------------------------------- the recap ----
        shape(
            "what did i do today",
            &[
                r"(?:what|was) (?:did|have|habe) (?:i|we|ich) (?:do|done|get done|got done|achieve|achieved|accomplish|accomplished|work on|worked on|get up to|been doing|been up to|been working on)(?: so far)? ?(?:today|this evening|tonight|this afternoon|this morning|all day)?(?: so far)?",
                r"was habe ich(?: heute| bisher| so| alles| den ganzen tag)* (?:gemacht|geschafft|getan|erledigt)",
                r"(?:give me|show me|show|tell me|whats|what is|lets hear|lets see|i want|i need|gib mir|zeig mir|zeig|sag mir|ich will|ich brauche)(?: a| an| the| my| your| me| mir| eine| einen| die| den| meine| meinen)* (?:evening |daily |day |days |todays |end of day |abend ?|tages ?)?(?:recap|summary|rundown|wrap up|wrapup|review|report|digest|debrief|rückblick|zusammenfassung|bericht|tagesrückblick|tagesbericht|tageszusammenfassung|abendrückblick) (?:of|for|von|über|vom)(?: my| the| meinem| den| meinen)? ?(?:day|today|evening|tag|heute|heutigen tag)(?: so far)?",
                r"(?:give me|show me|show|tell me|lets hear|lets see|i want|i need|gib mir|zeig mir|zeig|ich will|ich brauche)(?: a| an| the| my| your| me| mir| eine| einen| die| den| meine| meinen)* (?:evening |daily |day |days |todays |end of day |abend ?|tages ?)(?:recap|summary|rundown|wrap up|wrapup|review|report|digest|debrief|rückblick|zusammenfassung|bericht)",
                r"(?:recap|summari[sz]e|sum up|review|wrap up|go over|run through)(?: for me)?(?: my| the)? (?:day|today|evening)(?: so far| for me)?",
                r"(?:fasse|fass)(?: mir)? (?:meinen|den) (?:tag|heutigen tag|abend) zusammen",
                r"(?:how was|how did|hows|wie war|wie lief) (?:my|mein) (?:day|tag)(?: go| gone| been| so far)?",
                r"(?:evening |daily |day |tages ?|abend ?)(?:recap|rundown|rückblick|bericht)",
                r"(?:what have i been (?:doing|up to|working on)|where did (?:my|the) (?:day|time) go)(?: today| all day)?",
                r"(?:recap|rückblick|tagesrückblick|abendrückblick)",
            ],
        ),
        // -------------------------------------------------------------------- the doctor ----
        idea(
            "why is my internet so slow",
            &["internet|wifi|wi fi|wlan|connection|network|netz|netzwerk|verbindung|internetverbindung|ping|lan|router", "slow|slower|lagging|laggy|lag|lags|bad|terrible|awful|crap|weak|unstable|dropping|drops|down|broken|dead|not working|isnt working|doesnt work|acting up|wrong|issue|issues|problem|problems|trouble|diagnose|diagnosis|troubleshoot|doctor|fix|whats up|what is up|whats wrong|what is wrong|whats going on|langsam|lahm|schlecht|hängt|laggt|ruckelt|kaputt|instabil|bricht ab|geht nicht|funktioniert nicht|spinnt|los|probleme|diagnose|doktor|reparier|repariere"],
            "<TIMER>|speed test|speedtest|password|passwort|who is on|whos on|devices|geräte|radar|scan|<NOT>",
        )
        .asks(),
        // ------------------------------------------------------------- the privacy board ----
        idea("privacy board", &["privacy|datenschutz|privatsphäre", "<SHOW>|board|panel|page|overview|übersicht|need|want|pull|bring|zeig|zeige|öffne"], "<DEL>|<STOP>|policy|laws?|gdpr|dsgvo|windows|chrome|browser|discord|google|facebook|instagram|whatsapp|settings|einstellungen"),
        shape(
            "privacy board",
            &[
                r"(?:what|which|was|welche)(?: kind of| sort of)? ?(?:data|info|information|things|stuff|recordings|daten|infos|informationen|aufnahmen)? ?(?:do you|are you|have you|did you|hast du)? ?(?:keep|keeping|store|storing|save|saving|saved|stored|kept|have|collect|collecting|collected|hold|holding|record|recording|recorded|gespeichert|gesammelt)(?: stored| saved)? (?:about|on|of|from|über|von) (?:me|mich|mir)(?: gespeichert| gesammelt)?",
                r"was (?:speicherst|behältst|sammelst|hast) du(?: alles)? (?:über|von) (?:mich|mir)(?: gespeichert| gesammelt)?",
                r"(?:show|show me|tell me|zeig mir|sag mir) (?:what|everything|all|was|alles was)(?: data)? you (?:keep|store|save|have|collect|kept|stored|saved|collected)(?: stored| saved)? (?:about|on|of) me",
                r"(?:are|do) you (?:recording|record|spying on|saving|storing|keeping) (?:me|everything i say|what i say|my voice|all i say)(?: all the time| right now)?",
                r"(?:what|which) (?:data|information) do you (?:keep|store|have|save|collect)",
            ],
        ),
        idea(
            "stop keeping my voice clips",
            &[
                "voice clips|clips of my voice|clips of me|voice recordings|recordings of my voice|recordings of me|audio of me|audio clips|audio recordings|sound clips|recordings|my voice|sprachclips|sprachaufnahmen|aufnahmen|stimmaufnahmen|tonaufnahmen|audioaufnahmen",
                "stop keeping|stop saving|stop storing|stop recording|stop collecting|quit keeping|quit saving|quit storing|quit recording|dont keep|dont save|dont store|dont record|do not keep|do not save|do not store|do not record|no more|keep no|save no|store no|never keep|never save|never store|keine|nicht mehr|nicht speichern|nicht behalten|nicht aufnehmen|hör auf|aufhören",
            ],
            "delete|lösche|lösch|again|wieder speichern|wieder behalten|how many|show|zeig|learn|lerne",
        ),
        idea(
            "keep my voice clips again",
            &["voice clips|clips of my voice|clips of me|voice recordings|recordings of my voice|recordings of me|audio of me|audio clips|audio recordings|sprachclips|sprachaufnahmen|aufnahmen|stimmaufnahmen", "keep|keeping|save|saving|store|storing|record|recording|collect|collecting|behalte|behalt|speichere|speicher|speichern|behalten|aufnehmen"],
            "<STOP>|<NOT>|<NOMORE>|quit|<DEL>|delete|how many|show|zeig|where|wo|what|was|which|why|warum|hör auf",
        ),
        // ------------------------------------------------------------- the changes list ----
        shape(
            "show the changes",
            &[
                r"(?:<SHOW> )?<ART>(?:list of |full |latest |recent |new |whole )*(?:changes|change ?log|changes list|change list|release notes|patch notes|update notes|update log|version history|update history|änderungen|änderungsliste|änderungsprotokoll|neuerungen|versionshinweise)",
                r"(?:what|was)(?: all| alles)? (?:has|have|hat|hast)(?: been)? ?(?:changed|you changed|been updated|sich geändert|sich verändert|du geändert)(?: recently| lately| since yesterday| since last time| since the update| since the last update| in the (?:last |latest |new )?(?:update|version|build)| zuletzt| seit gestern| seit dem update| seit dem letzten update)?",
                r"(?:what|which|was|welche)(?: new)? (?:changes|features|updates|änderungen|funktionen|neuerungen)(?: are there| did you get| have you got| were made| got added| were added| are new| have been made| have been added| gibt es| sind neu| hast du bekommen| wurden gemacht)(?: recently| lately| in the (?:last |latest |new )?(?:update|version))?",
                r"(?:what did|what have|was hat|was haben) (?:you|i|we|claude|the update|the new version|das update)(?: guys)? (?:change|changed|update|updated|add|added|geändert|hinzugefügt|verändert)(?: in you| on you| about you| recently| lately| last night| this time| an dir| bei dir)*",
                r"(?:whats|what is|was ist|was gibt es) (?:new|different|changed|neu|neues|anders) (?:in (?:the|this|your) (?:last |latest |new )?(?:update|version|build)|about you|with you|bei dir|an dir|in der neuen version|im update)",
                r"(?:what can you do now|what are your new features|what new features do you have|was kannst du jetzt|was sind deine neuen funktionen|was kannst du neues)",
            ],
        ),
        shape(
            "explain {text}",
            &[
                r"(?:explain|describe|erkläre|erklär|beschreibe|beschreib)(?: to me| mir| me)?(?: how| wie)? <ART>(?P<text>[a-zäöüß0-9 ]{3,40}?)(?: feature| function| funktion| works| work| funktioniert)?(?: to me| again| in detail| nochmal)?",
                r"(?:what does|what do|what can|how does|how do|how do i use|how can i use|what is|whats|tell me about|tell me more about|more about|more on|was macht|was machen|was kann|wie funktioniert|wie funktionieren|wie benutze ich|was ist|erzähl mir von|erzähl mir mehr über|mehr über|mehr zu) <ART>(?:new )?(?P<text>[a-zäöüß0-9 ]{3,40}?)(?: feature| function| funktion| thing)?(?: do| does| work| works| mean| about| good for| used for| again| exactly| funktioniert| genau| eigentlich)*",
                r"(?:i dont understand|i do not understand|i dont get|ich verstehe) <ART>(?P<text>[a-zäöüß0-9 ]{3,40}?)(?: feature| function| funktion)?(?: nicht)?",
            ],
        )
        .asks(),
        // --------------------------------------------------------------------- backups ----
        shape(
            "back up your memory",
            &[
                r"(?:back up|backup|save|sichere|sicher)(?: all| all of)? (?:your memory|yourself|your data|your brain|your database|everything you know|everything you remember|your memories|what you know|dein gedächtnis|deine daten|dich|deine erinnerungen|alles was du weißt)(?: now| right now)?",
                r"(?:make|create|do|run|start|take|trigger|perform|mach|mache|erstelle|erstell|starte)(?: me| mir)?(?: a| an| ein| eine| the| another)? ?(?:new |fresh |quick |neues |neue )?(?:memory )?(?:backup|back up|sicherung|sicherheitskopie|datensicherung)(?: of (?:your memory|yourself|your data|everything)| von (?:deinem gedächtnis|dir|deinen daten))?(?: now| right now)?",
                r"(?:backup|sicherung) (?:now|jetzt|bitte|starten|machen|erstellen)",
            ],
        ),
        idea(
            "when was the last backup",
            &["backup|backups|backed up|sicherung|sicherungen|sicherheitskopie|gesichert", "when|last|latest|recent|newest|how old|how many|show|list|do you have|have you got|is there|are there|any|status|check|wann|letzte|letztes|letzten|neueste|wie alt|wie viele|zeig|liste|hast du|gibt es"],
            "restore|wiederherstellen|<DEL>|make a|create a|do a|run a|mach ein|mach eine|erstelle",
        )
        .asks(),
        // ------------------------------------------------------- where the subtitles stand ----
        shape(
            "subtitles to the {place}",
            &[
                r"(?:put|move|show|place|set|have|display|shift|bring|get|i want|i need|schieb|schiebe|setz|setze|zeig|zeige|stell|stelle|pack|mach)(?: me| mir)? <ART><SUBS>(?: bar| leiste)? (?:to |at |on |in |into |up to |down to |up at |down at |nach |in die |in der |an den |an die |ganz nach |auf )?(?:the |den |die )?(?:very )?(?P<place>top|bottom|middle|centre|center|oben|unten|mitte)(?: of (?:the|my) (?:screen|monitor|display)| edge| am bildschirm| des bildschirms| hin)?",
                r"<ART><SUBS>(?: bar| leiste)? (?:should be |should go |should sit |should appear |should show |belong |go |goes |sollen |sollten |gehören |kommen )?(?:to |at |on |in |up |down |nach |in die |in der )?(?:the |den |die )?(?:very )?(?P<place>top|bottom|middle|centre|center|oben|unten|mitte)(?: of (?:the|my) (?:screen|monitor|display)| stehen| sein| hin)?",
                r"(?P<place>top|bottom|middle|centre|center|oben|unten|mitte) <SUBS>",
                r"(?:move|put|schieb|setz) <ART><SUBS> (?P<place>up|down|higher|lower|hoch|runter|höher|tiefer)",
            ],
        ),
        shape(
            "put the subtitles on monitor {n}",
            &[
                r"(?:put|move|show|place|have|display|shift|bring|get|send|i want|i need|schieb|schiebe|zeig|zeige|setz|setze|pack|stell|mach)(?: me| mir)? <ART><SUBS>(?: bar| leiste)? (?:on|to|onto|over to|auf|zu|zum|rüber auf)(?: my| the| den| dem| meinen| meinem)? ?<SCREENS>(?: number| nummer| nr)? (?P<n>\d|one|two|three|eins|zwei|drei)",
                r"(?:put|move|show|place|have|display|shift|bring|get|send|schieb|schiebe|zeig|zeige|setz|setze|pack|stell)(?: me| mir)? <ART><SUBS>(?: bar| leiste)? (?:on|to|onto|over to|auf|zu|zum)(?: my| the| den| dem| meinen| meinem)? (?P<n>first|second|third|main|primary|1st|2nd|3rd|ersten|zweiten|dritten|haupt) ?<SCREENS>",
                r"<ART><SUBS>(?: bar| leiste)? (?:on|to|onto|auf)(?: my| the| den| dem)? ?<SCREENS>(?: number| nummer)? (?P<n>\d|one|two|three|eins|zwei|drei)",
                r"<ART><SUBS>(?: bar| leiste)? (?:should be|should go|should show|should appear|sollen|sollten)(?: on| auf)(?: my| the| den| dem)? ?<SCREENS>(?: number)? (?P<n>\d|one|two|three|eins|zwei|drei)(?: sein| stehen| erscheinen)?",
                r"<SCREENS> (?P<n>\d|one|two|three) (?:for|für) <ART><SUBS>",
            ],
        ),
        idea(
            "subtitles follow my window",
            &["<SUBS>", "follow|follows|following|track|tracks|stick to|stay with|go with|come with|move with|wherever|whichever|same monitor as|same screen as|window in front|active window|active monitor|monitor i am on|monitor im on|screen i am on|screen im on|where i am|where im|where i work|folgen|folge|folgt|mitkommen|mitgehen|mitwandern|wo ich bin|wo ich gerade bin|aktiven fenster|aktives fenster|aktiven monitor"],
            "<NOT>|stop|<TURNOFF>",
        ),
        shape(
            "where are the subtitles",
            &[
                r"(?:where|wo)(?: exactly| genau)? (?:are|do|did|will|have|sind|stehen|erscheinen|werden|landen)(?: you)? ?(?:put|show|showing|display|displaying|placed)? ?<ART><SUBS>(?: bar| leiste)? ?(?:stand|show|show up|appear|go|gone|sit|placed|located|displayed|shown|right now|now|currently|at the moment|set to|angezeigt|gerade|eigentlich|hin)*",
                r"(?:which|what|welchem|welcher|auf welchem) (?:<SCREENS>|place|position|spot|stelle) (?:are |is |do |sind |stehen |erscheinen |haben )?<ART><SUBS>(?: bar)? ?(?:on|at|in|show on|appear on|showing on|set to|use|using|stehen|sein)*",
                r"(?:where|wo) (?:can i (?:find|see)|do i (?:find|see)|will i see|finde ich|sehe ich) <ART><SUBS>(?: bar| leiste)?",
                r"<ART><SUBS>(?: bar| leiste)? (?:position|place|placement|location)",
                r"(?:wo|where) (?:is|ist) (?:the |die )?(?:subtitle bar|caption bar|untertitelleiste)",
            ],
        )
        .asks(),
        // --------------------------------------------------------------- window places ----
        idea(
            "forget where the windows were",
            &[
                "window places|window positions|block places|block positions|where (?:the|my|your) (?:windows|blocks|panels) (?:were|are|stood|stand|go|went|belong)|where i (?:put|placed|moved|left) (?:the|my|your) (?:windows|blocks|panels)|remembered (?:places|positions|spots)|saved (?:places|positions|spots)|positions of (?:the|my|your) (?:windows|blocks|panels)|places of (?:the|my|your) (?:windows|blocks|panels)|fensterpositionen|fensterplätze|blockpositionen|wo die fenster (?:waren|standen|sind)|positionen der fenster",
                "forget|reset|clear|wipe|erase|delete|discard|drop|start fresh|start over|defaults?|stop remembering|dont remember|vergiss|setz|setze|lösche|lösch|zurücksetzen|zurück|standard",
            ],
            "",
        ),
        shape(
            "forget where the windows were",
            &[r"(?:reset|restore|put|bring)(?: all)?(?: the| my| your)? (?:windows|blocks|panels)(?: back)?(?: to| in| into)?(?: their| the)? (?:default|original|standard|normal|usual) (?:places|positions|spots|place|position)"],
        ),
        // ----------------------------------------------------------------- notifications ----
        idea(
            "clear my notifications",
            &["notifications?|notification history|notification list|notification log|alerts|toasts|benachrichtigungen|benachrichtigungsverlauf|mitteilungen|meldungen", "<DEL>|empty|dismiss|clean|clean up|clean out|flush|purge|leere|leer|weg|räum|räume|aufräumen|verwerfen|verwirf"],
            "<NOT>|show|list|what|which|read|zeig|was|welche|missed|verpasst|turn off|disable|mute|stumm",
        ),
        // ------------------------------------------------------------ what he has selected ----
        shape(
            "explain this",
            &[
                r"(?:explain|clarify|break down|unpack|decode|simplify|erkläre|erklär|erläutere)(?: to me| mir| for me)? (?:this|that|it|the selection|the selected text|the highlighted text|the marked text|what i (?:selected|marked|highlighted)|this text|this passage|this paragraph|this sentence|this part|this bit|this word|das|dies|das hier|den text|die auswahl|die stelle|den markierten text|diesen text|diesen satz|diesen absatz|dieses wort)(?: to me| for me| simply| in simple words| in simple terms| in plain english| in easy words| like im five| mir| einfach| in einfachen worten)*",
                r"(?:what does|what do|was bedeutet|was heißt|was bedeuten)(?: all)? (?:this|that|it|the selection|the selected text|the highlighted text|this text|this passage|this paragraph|this sentence|this word|this part|das|dies|das hier|der text|die auswahl|dieser satz|dieses wort|der markierte text)(?: here)? ?(?:mean|means|say|stand for)?(?: exactly| here| genau| eigentlich)?",
                r"(?:whats|what is) (?:this|that) (?:text |passage |paragraph |sentence |part |bit )?(?:about|saying|trying to say|supposed to mean)",
                r"(?:i dont understand|i do not understand|i dont get|i cant understand|ich verstehe|ich versteh|ich kapier) (?:this|that|it|this text|this passage|this part|this sentence|this paragraph|das|dies|das hier|den text|diesen satz|diesen absatz|die stelle)(?: nicht| at all| here)?",
                r"(?:help me understand|make sense of|hilf mir bei|hilf mir mit) (?:this|that|it|this text|this passage|the selection|what i (?:selected|marked|highlighted)|das|dies|dem text|der stelle|dem hier)",
                r"(?:worum geht es hier|worum geht es in dem text|was steht da|was steht hier)",
                r"(?:explain|erkläre|erklär)(?: mir)? (?:the |den |die )?(?:selected|highlighted|marked|markierten|ausgewählten|markierte) (?:text|part|bit|passage|section|stelle|teil|abschnitt)",
            ],
        )
        .asks(),
        shape(
            "translate this into {lang}",
            &[
                r"(?:translate|übersetze|übersetz)(?: mir| for me| me)? (?:this|that|it|the selection|the selected text|the highlighted text|the marked text|what i (?:selected|marked|highlighted)|this text|this passage|this paragraph|this sentence|this part|this word|das|dies|das hier|den text|die auswahl|die stelle|den markierten text|diesen text|diesen satz|diesen absatz|dieses wort)(?: for me| mir)? (?:into|to|in|auf|ins|nach) <LANG>(?: for me)?",
                r"(?:put|turn|say|give me|render|convert|get me|sag|gib mir)(?: mir)? (?:this|that|it|the selection|the selected text|this text|this passage|this sentence|das|dies|das hier|den text|die auswahl)(?: for me)? (?:into|in|to|auf|ins) <LANG>(?: for me)?",
                r"(?:whats|what is|was ist|was heißt|wie heißt) (?:this|that|it|das|dies|das hier)(?: text| word| sentence)? (?:in|auf) <LANG>",
                r"(?:how do you say|how would you say|how do i say|wie sagt man) (?:this|that|it|das|dies) (?:in|auf) <LANG>",
                r"(?:i need|i want|id like|give me|ich brauche|ich will) (?:this|that|it|the selection|this text|das|dies|den text)(?: translated)? (?:in|into|to|auf|ins) <LANG>(?: translated| übersetzt)?",
                r"<LANG> (?:translation|übersetzung)(?: of this| of that| of the selection| davon| von dem text)?",
                r"(?:translate|übersetze|übersetz)(?: mir)? (?:into|to|in|auf|ins) <LANG>",
            ],
        )
        .asks(),
        shape(
            "translate this",
            &[
                r"(?:translate|übersetze|übersetz)(?: mir| for me| me)? (?:this|that|it|the selection|the selected text|the highlighted text|the marked text|what i (?:selected|marked|highlighted)|this text|this passage|this paragraph|this sentence|this part|this word|das|dies|das hier|den text|die auswahl|die stelle|den markierten text|diesen text|diesen satz|diesen absatz|dieses wort)(?: for me| mir)?",
                r"(?:i need|i want|id like|give me|can i get|ich brauche|ich will)(?: a| an| eine| the| die)? (?:translation|übersetzung)(?: of this| of that| of the selection| of this text| davon| von dem text)?",
                r"(?:what does this say|what does that say|i cant read this|ich kann das nicht lesen)",
            ],
        ),
        shape(
            "summarise this",
            &[
                r"(?:summari[sz]e|sum up|shorten|condense|boil down|abridge|kürze|kürz)(?: for me| mir)? (?:this|that|it|the selection|the selected text|the highlighted text|the marked text|what i (?:selected|marked|highlighted)|this text|this passage|this paragraph|this article|this page|this part|this section|this whole thing|all this|all of this|das|dies|das hier|den text|die auswahl|die stelle|den markierten text|diesen text|diesen absatz|diesen artikel|das ganze)(?: for me| up| down| briefly| in short| in a few sentences| in a nutshell| mir)*",
                r"(?:fasse|fass)(?: mir)? (?:das|dies|das hier|den text|die auswahl|die stelle|den markierten text|diesen text|diesen absatz|diesen artikel|das ganze)(?: mal| kurz)* zusammen",
                r"(?:give me|i need|i want|id like|can i get|can i have|lets have|write me|gib mir|ich brauche|ich will)(?: a| an| the| eine| die)? (?:quick |short |brief |kurze |knappe )?(?:summary|tldr|tl dr|gist|main points|key points|short version|shorter version|quick version|brief version|cliff notes|essence|zusammenfassung|kurzfassung|kernaussagen|kernpunkte|kurzversion)(?: of| for| von| davon| dazu)?(?: this| that| it| the selection| the selected text| this text| this article| this passage| the text| das| dem text| dem hier| der auswahl)?",
                r"(?:whats|what is|what are|was ist|was sind) (?:the )?(?:gist|summary|tldr|main point|main points|key points|key takeaways|takeaways|bottom line|short version|kernaussage|kernaussagen|kernpunkte|quintessenz)(?: of| here| von)?(?: this| that| it| the selection| this text| das| dem text| dem hier)?",
                r"(?:tldr|tl dr|too long didnt read)(?: this| that)?",
                r"(?:this|that|it|das|der text) is (?:way |far |much |a bit |viel )?too (?:long|much)(?: to read)?(?: give me the (?:gist|main points|short version|summary)| summari[sz]e it| can you summari[sz]e it| shorten it| sum it up)?",
                r"(?:das|der text) ist(?: mir)? (?:viel )?zu lang(?: fass es zusammen| fass das zusammen| kürz das| gib mir die kurzfassung)?",
                r"(?:in short|in a nutshell|in brief|kurz gesagt|kurz und knapp|kurzfassung)",
                r"what does (?:this|that|it|the text) (?:say|boil down to) in (?:short|brief|a nutshell|a few words)",
            ],
        ),
        // ------------------------------------------------------- who he listens to ----
        shape(
            "only obey me",
            &[
                r"(?:only|just|nur) (?:obey|listen to|hear|react to|respond to|answer|take orders from|take commands from|accept orders from|follow|gehorche|höre auf|hör auf|reagiere auf|antworte) (?:me|my voice|my orders|my commands|mir|mich|meine stimme|meine befehle)(?: from now on| now| again| alone| ab jetzt| wieder)*",
                r"(?:obey|listen to|hear|react to|respond to|answer|take orders from|take commands from|follow|gehorche|höre auf|hör auf|reagiere auf)(?: only| just| nur)? (?:me|my voice|mir|mich|meine stimme)(?: only| alone| and nobody else| and no one else| and only me| nobody else| no one else| exclusively| und niemanden sonst| und sonst niemanden| und nur mich| allein)+",
                r"(?:dont|do not|never|stop) (?:listen|listening|obey|obeying|react|reacting|respond|responding|take|taking)(?: orders| commands)? (?:to|from) (?:anyone|anybody|everyone|everybody|other people|others|other voices|strangers)(?: else| but me| except me| other than me| apart from me| anymore| any more)*",
                r"(?:ignore|block out|ignoriere|ignorier) (?:everyone|everybody|anyone|anybody|all others|all other voices|other people|other voices|the others|strangers|alle|alle anderen|andere|andere stimmen|fremde|fremde stimmen)(?: else| but me| except me| apart from me| other than me| außer mir| außer mich| im raum)*",
                r"(?:i am|im|ich bin) the only one (?:you (?:should |may |can )?(?:listen to|obey)|who (?:can|may|gets to) (?:talk to you|give you orders|command you))",
                r"(?:nur ich|only i) (?:darf|kann|may|can|should be able to|get to) (?:dir befehle geben|mit dir reden|dich steuern|give you orders|talk to you|command you|control you)",
                r"(?:hör|höre|reagiere|reagier) nur (?:noch )?auf (?:mich|meine stimme)",
                r"gehorche nur (?:noch )?mir",
                r"(?:<TURNON> )?<ART>(?:voice lock|voice ?print lock|speaker lock|stimmsperre|sprechersperre)(?: <ON>)?",
                r"lock (?:on |onto |to |yourself to |your ears to )(?:me|my voice)",
            ],
        ),
        shape(
            "listen to everyone",
            &[
                r"(?:listen to|obey|hear|take orders from|take commands from|react to|respond to|answer|accept|hör auf|höre auf|gehorche|reagiere auf|antworte) (?:everyone|everybody|anyone|anybody|all voices|any voice|all people|other people too|others too|whoever speaks|whoever is speaking|whoever talks|alle|jeden|jedem|allen|alle stimmen|jede stimme|andere auch)(?: again| now| too| as well| in the room| here| wieder| auch| im raum)*",
                r"(?:you can|you may|you should|du kannst|du darfst|du sollst) (?:listen to|obey|hear|take orders from|react to|respond to|auf|wieder auf) (?:everyone|everybody|anyone|anybody|others|other people|alle|jeden|andere)(?: again| now| too| wieder| hören| reagieren)*",
                r"(?:let|allow|lass) (?:everyone|everybody|anyone|anybody|others|other people|alle|jeden|andere) (?:talk to you|speak to you|give you orders|give you commands|command you|control you|use you|mit dir reden|dich steuern|dich benutzen)(?: again| now| too| wieder)?",
                r"(?:dont|do not|you dont have to|you do not have to|stop|no need to) (?:only |just )?(?:listen|listening|obey|obeying|react|reacting|respond|responding)(?: only| just)? (?:to )?(?:only |just )?(?:me|my voice)(?: only| alone| anymore| any more| any longer)*",
                r"(?:hör|höre|gehorche|reagiere)(?: wieder)? (?:nicht (?:mehr )?nur (?:auf )?(?:mich|mir)|(?:auf )?alle|jedem|(?:auf )?jeden)(?: wieder)?",
                r"(?:<TURNOFF>|unlock) <ART>(?:voice lock|voice ?print lock|speaker lock|stimmsperre|sprechersperre)",
                r"(?:voice lock|speaker lock|stimmsperre|sprechersperre) <OFF>",
                r"(?:anyone|anybody|everyone|everybody|jeder|alle) (?:can|may|is allowed to|darf|dürfen|kann|können) (?:talk to you|speak to you|give you orders|command you|control you|use you|mit dir reden|dir befehle geben|dich steuern|dich benutzen)(?: again| now| wieder)?",
            ],
        ),
        shape(
            "learn my voice",
            &[
                r"(?:learn|remember|memori[sz]e|study|record|save|store|train on|get to know|recogni[sz]e|enrol|enroll|register|lerne|lern|merk dir|merke dir|speichere|trainiere|erkenne)(?: to recogni[sz]e)? (?:my voice|my voice print|my voiceprint|how i sound|what i sound like|meine stimme|meinen stimmabdruck|wie ich klinge)(?: again| now| anew| from scratch| properly| better| neu| nochmal| erneut| kennen)*",
                r"(?:redo|repeat|restart|rerun|run|start|do|begin|update|refresh|renew|set up|setup|wiederhole|starte|mach|erneuere)(?: the| my| a| das| die| mein)? ?(?:new )?(?:voice training|voice setup|voice set up|voice enrol?lment|voice print|voice profile|voice calibration|voice learning|voiceprint|stimmtraining|stimmabdruck|stimmprofil)(?: again| now| neu| nochmal)?",
                r"(?:teach you|let me teach you|i want to teach you)(?: to recogni[sz]e)? my voice",
                r"(?:train|trainiere) (?:yourself |dich )?(?:on|to|with|auf|mit) (?:my voice|meine stimme|meiner stimme)",
                r"(?:voice training|voice setup|voice enrol?lment|stimmtraining|stimme lernen|stimme neu lernen)",
            ],
        ),
        // ------------------------------------------------------- what he missed ----
        shape(
            "what did i miss",
            &[
                r"(?:what|was)(?: all| alles)? (?:did|have|habe) (?:i|ich)(?: just)? (?:miss|missed|verpasst)(?: while i was (?:away|gone|out|afk)| when i was (?:away|gone|out)| so far| today| earlier| während ich weg war| als ich weg war)?",
                r"(?:anything|something|irgendwas|etwas)(?: new| neues| important| wichtiges)? (?:happen|happened|come in|came in|passiert|reingekommen)(?: while i was (?:away|gone|out)| in the meantime| während ich weg war| in der zwischenzeit)",
                r"(?:what happened|what went on|whats happened|was ist passiert|was war los) (?:while i was (?:away|gone|out|afk)|when i was (?:away|gone|out)|in the meantime|während ich weg war|als ich weg war|in der zwischenzeit)",
                r"(?:catch me up|bring me up to speed|fill me in|get me up to speed|bring mich auf den neuesten stand|bring mich auf stand)(?: on what i missed| on what happened)?",
            ],
        ),
    ];
    ways.extend(switch("guest mode", "guest mode off", "guest mode|guests mode|guest modus|gästemodus|gäste modus|gastmodus|gast modus|visitor mode|besuchermodus"));
    ways.push(
        shape(
            "guest mode",
            &[
                r"(?:i have|ive got|we have|there are|ich habe|wir haben|es sind) (?:guests|visitors|friends|company|people|gäste|besuch|freunde)(?: over| here| coming| coming over| da| zu besuch| hier)*",
                r"(?:my |meine )?(?:friends|guests|visitors|mates|buddies|freunde|gäste) (?:are|sind) (?:here|over|coming|coming over|visiting|da|hier|zu besuch)",
            ],
        )
        .of_people(),
    );
    ways.push(
        shape(
            "guest mode off",
            &[
                r"(?:my |the |meine |die )?(?:friends|guests|visitors|gäste|freunde) (?:are|have|sind|haben)(?: all| alle)? (?:gone|left|leaving|away|weg|gegangen)(?: now| again| home| wieder)?",
                r"(?:der |mein )?besuch ist (?:weg|gegangen|wieder weg)",
                r"(?:everyone|everybody|alle) (?:has |have |is |are |sind )?(?:left|gone|gone home|weg|gegangen)",
                r"(?:im alone again|im on my own again|its just me again|ich bin wieder allein|wir sind wieder allein)",
            ],
        )
        .of_people(),
    );
    ways.extend(switch("night mode", "night mode off", "night mode|nightmode|night modus|nachtmodus|nacht modus|night time mode|nighttime mode"));
    ways.extend(switch("live desktop on", "live desktop off", "live desktop|live background|live wallpaper|desktop backdrop|animated background|animated wallpaper|animated desktop|live hintergrund|live desktop hintergrund|lebendiger hintergrund|animierter hintergrund|live backdrop"));
    ways.push(shape(
        "live desktop on",
        &[
            r"(?:show|put|display|zeig|zeige|setz|setze) (?:yourself|your emblem|the emblem|your core|dich|dein emblem) (?:on|as|auf|als) (?:my|the|meinem|meinen|dem|den) (?:desktop|wallpaper|background|desktop background|hintergrund|desktophintergrund)",
            r"(?:be|become|sei|werde) my (?:wallpaper|desktop background|background|hintergrund)",
            // ("Show me your live desktop background": Sir, 2026-11-13.)
            r"(?:show|show me|display|let me see|zeig|zeige|zeig mir|zeige mir) <ART>(?:live desktop(?: background)?|live background|live wallpaper|live backdrop|desktop backdrop|animated background|animated wallpaper|live hintergrund|live desktop hintergrund)",
        ],
    ));
    ways
}

/// The older everyday orders: the volume, the music, his own views, windows, timers, pictures.
/// (Last come the ones that take any name for an app: every other way is asked before them.)
fn everyday() -> Vec<Way> {
    let mut ways = vec![
        // ------------------------------------------------------------------ the volume ----
        shape(
            "set the volume to {n} percent",
            &[
                r"(?:set|put|make|turn|change|adjust|bring|get|stell|stelle|mach|setz|setze|dreh)(?: the| my| die)? (?:master )?(?:volume|sound|audio|lautstärke|ton)(?: level)? (?:to|at|on|up to|down to|auf) <NUM>",
                r"(?:volume|sound|lautstärke)(?: level)?(?: to| at| on| auf)? <NUM>",
                r"<NUM> (?:volume|lautstärke)",
                r"(?:turn|crank|dreh)(?: it| the volume| die lautstärke)? (?:up|down|hoch|runter) (?:to|auf) <NUM>",
            ],
        ),
        shape(
            "volume up",
            &[
                r"(?:turn|crank|pump|bump|put|make|dreh|mach|stell)(?: the volume| the sound| the audio| the music| die lautstärke| den ton| die musik)? (?:up|louder|lauter|hoch|höher|auf)(?: a bit| a little| a little bit| a notch| a tad| slightly| some| more| ein bisschen| etwas| ein wenig)*",
                r"(?:turn|crank|pump|bump|dreh)(?: up)? (?:the |my |die )?(?:volume|sound|audio|lautstärke|ton)(?: up| hoch| auf)?(?: a bit| a little| a notch| etwas| ein bisschen)*",
                r"(?:increase|raise|boost|up|higher|erhöhe|erhöh)(?: the| my| die)? (?:volume|sound|audio|lautstärke|ton)(?: a bit| a little| level| etwas)*",
                r"(?:volume|sound|audio|lautstärke|ton) (?:up|higher|louder|hoch|höher|lauter|rauf)(?: a bit| a little| etwas| ein bisschen)*",
                r"(?:a bit |a little |etwas |ein bisschen |much |way |viel )?(?:louder|lauter|more volume|mehr lautstärke)",
                r"(?:its|it is|thats|that is|this is|das ist|es ist) (?:way |far |much |a bit |a little |viel |etwas |ein bisschen )?(?:too (?:quiet|low|soft)|zu leise)",
                r"i (?:cant|can not|cannot|can barely|can hardly) hear (?:it|that|this|the music|anything|a thing)(?: properly| well| at all)?",
            ],
        ),
        shape(
            "volume down",
            &[
                r"(?:turn|bring|put|make|dreh|mach|stell)(?: the volume| the sound| the audio| the music| die lautstärke| den ton| die musik)? (?:down|quieter|lower|softer|leiser|runter|niedriger)(?: a bit| a little| a little bit| a notch| a tad| slightly| some| more| ein bisschen| etwas| ein wenig)*",
                r"(?:turn|bring|dreh)(?: down)? (?:the |my |die )?(?:volume|sound|audio|lautstärke|ton)(?: down| runter)?(?: a bit| a little| a notch| etwas| ein bisschen)*",
                r"(?:decrease|lower|reduce|drop|verringere|senke|senk|reduziere)(?: the| my| die)? (?:volume|sound|audio|lautstärke|ton)(?: a bit| a little| level| etwas)*",
                r"(?:volume|sound|audio|lautstärke|ton) (?:down|lower|quieter|runter|niedriger|leiser)(?: a bit| a little| etwas| ein bisschen)*",
                r"(?:a bit |a little |etwas |ein bisschen |much |way |viel )?(?:quieter|leiser|less volume|weniger lautstärke|not so loud|nicht so laut)",
                r"(?:its|it is|thats|that is|this is|das ist|es ist) (?:way |far |much |a bit |a little |viel |etwas |ein bisschen )?(?:too loud|zu laut)(?: turn (?:it|the volume) down)?",
            ],
        ),
        shape(
            "mute",
            &[
                r"(?:mute|silence|kill|cut)(?: the| all| all the| my)? (?:sound|audio|volume|speakers|output)",
                r"(?:sound|audio|volume|ton) (?:off|aus|weg|mute|stumm)",
                r"(?:turn|switch|schalte|schalt|mach)(?: the| my| den| die| all)? (?:sound|audio|volume|speakers|ton|lautsprecher) (?:off|aus|stumm|mute)",
                r"(?:stell|stelle|schalte|schalt|mach)(?: den ton| die lautstärke| alles)? stumm",
                r"(?:no sound|kein ton|ton aus|make it silent|go silent)",
            ],
        ),
        // ------------------------------------------------------------------- the music ----
        shape(
            "pause the music",
            &[
                r"(?:pause|stop|halt|hold|freeze|pausiere|pausier|stoppe|stopp|halte)(?: the| my| this| that| die| den| das| meine)? (?:music|song|track|tune|playback|player|audio|playlist|musik|lied|wiedergabe)(?: for a sec| for a moment| for now| an)*",
                r"(?:music|song|playback|musik|lied|wiedergabe) (?:pause|stop|off|aus|stopp|pausieren|anhalten|stoppen)",
                r"(?:make|mach) (?:the |die )?(?:music|musik) (?:stop|aus)",
                r"(?:turn|switch|schalte|schalt|mach)(?: the| die| my| meine)? (?:music|musik|song|playback) (?:off|aus)",
                r"(?:no more|enough|genug|keine) (?:music|musik)(?: for now| mehr)?",
                r"(?:shut|cut|kill)(?: off)? (?:the |that )?(?:music|song|track)(?: off)?",
            ],
        ),
        shape(
            "resume the music",
            &[
                r"(?:resume|continue|unpause|restart|setze|setz)(?: the| my| that| die| meine)? (?:music|song|track|playback|player|playlist|musik|lied|wiedergabe)(?: again| fort)*",
                r"(?:play|start|spiel|spiele)(?: the| my| that| die| meine)? (?:music|song|track|playback|musik|lied|wiedergabe) (?:again|back|back on|back up|weiter|wieder ab|wieder)",
                r"(?:turn|switch|put|bring|mach|schalte|schalt)(?: the| my| die| meine)? (?:music|song|playback|musik) (?:back on|on again|on|back|wieder an|an|wieder ein)",
                r"(?:music|musik|song|playback) (?:on|back on|again|an|weiter|wieder an|play)",
                r"(?:keep|carry on|go on|mach weiter|weiter)(?: playing| with)?(?: the| mit der| mit dem)? (?:music|song|musik|lied)",
                r"(?:keep playing|continue playing|resume playing|resume playback|continue playback|spiel weiter|weiterspielen|musik weiter)",
            ],
        ),
        shape(
            "next song",
            &[
                r"(?:skip|next|change|switch|überspringe|überspring|wechsle|wechsel)(?: to)?(?: the| this| that| den| das| diesen| dieses)?(?: next| current| nächsten| nächstes)? (?:song|track|tune|title|lied|titel|stück)",
                r"(?:go|jump|move|skip|switch|geh|spring)(?: on)? (?:to |zum |zur |auf den |auf das )(?:the )?(?:next|following|nächsten|nächste) (?:song|track|tune|one|title|lied|titel|stück)",
                r"(?:play|put on|give me|i want|spiel|spiele|mach|aktiviere)(?: me| mir)?(?: the| den| das| a| another| something)? (?:next|another|different|following|nächsten|nächste|nächstes|anderen|anderes|andere) (?:song|track|tune|one|title|lied|titel|stück)(?: an| ab)?",
                r"(?:i dont like|i hate|not|ich mag) (?:this|that|diesen|dieses|das) (?:song|track|one|lied)(?: nicht)?",
                r"(?:skip|überspring|überspringe) (?:it|this|that|this one|that one|das|dieses|den)",
                r"(?:something else|another one|was anderes|ein anderes lied|nächstes lied|nächster song|nächster titel|next track|next one)",
            ],
        ),
        shape(
            "previous song",
            &[
                r"(?:go back|back|previous|last|rewind|zurück|vorheriges|vorheriger|letztes|letzter)(?: to)?(?: the| a| one| den| das| zum| ein)?(?: previous| last| vorherigen| vorheriges| letzten| letztes)? (?:song|track|tune|title|lied|titel|stück)(?: again| back| zurück| nochmal)?",
                r"(?:play|put on|spiel|spiele)(?: me| mir)? (?:the |that |den |das )?(?:previous|last|song before|track before|one before|vorherigen|vorheriges|letzten|letztes|vorige|vorigen) ?(?:song|track|tune|one|title|lied|titel)?(?: again| nochmal| noch einmal)?",
                r"(?:one|ein|einen) (?:song|track|lied|titel) (?:back|zurück)",
                r"(?:song|track|lied|titel) (?:back|before|zurück|davor)",
                r"(?:the |den |das )?(?:song|track|one|lied|titel) (?:before|from before|davor|von davor|von eben)(?: again| nochmal)?",
            ],
        ),
        shape(
            "whats playing",
            &[
                r"(?:what|which|whats|was|welches|welcher) (?:is |s |ist |läuft )?(?:this |that |the |the current |the name of this |the name of the |das für ein |dieses |der |das )?(?:song|track|tune|music|title|lied|titel|stück|musik)(?: is this| is that| is playing| playing| called| on| right now| now| currently| läuft| ist das| gerade| da| heißt)*",
                r"(?:whats|what is|who is|whos|wer|was) (?:playing|this playing|that playing|singing|singing this|läuft|läuft da|läuft gerade|singt|singt da|spielt da)(?: right now| now| at the moment| currently| gerade| da)*",
                r"(?:name of|title of) (?:this|that|the) (?:song|track|tune)",
                r"(?:what am i|what are we) (?:listening to|hearing)(?: right now)?",
                r"(?:was höre ich|was hören wir)(?: da| gerade)*",
                r"wie heißt (?:das|dieses|der) (?:lied|song|titel|stück)(?: gerade)?",
            ],
        )
        .asks(),
        // --------------------------------------------------------------------- the time ----
        shape(
            "what time is it",
            &[
                r"(?:whats|what is|tell me|do you have|do you know|got|give me|sag mir|weißt du|hast du) (?:the )?(?:time|current time|uhrzeit|zeit)(?: is it| right now| now| it is| ist es| gerade)*",
                r"(?:how late is it|wie spät ist es|wie viel uhr ist es|wieviel uhr ist es|wie spät haben wir|wie viel uhr haben wir|wie spät|what time do we have|have you got the time|can you tell me the time|current time|time check|uhrzeit)",
            ],
        )
        .asks(),
        // ------------------------------------------------------------------ the windows ----
        shape(
            "minimize everything",
            &[
                r"(?:minimi[sz]e|hide|minimiere|minimier|verstecke|versteck)(?: all| all of| every| alle| alles)?(?: my| the| open| these| those| meine| die| offenen)* ?(?:windows|window|apps|programs|applications|everything|fenster|programme|anwendungen|alles)(?: away| weg)?",
                r"(?:show|go to|take me to|zeig|zeige|geh zum|geh auf den)(?: me| mir)? (?:the |my |den |meinen )?desktop",
                r"get (?:all )?(?:these|the|my|those) windows out of (?:the|my) way",
            ],
        ),
        shape(
            "snap {app} to the left",
            &[
                r"(?:snap|put|move|dock|place|pin|send|throw|push|shove|schieb|schiebe|pack|setz|setze|verschiebe|verschieb|zieh|ziehe)(?: the| my| den| die| das| mein)? (?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?)(?: window| fenster)? (?:to|on|onto|at|in|into|over to|nach|auf|an|zur|auf die|an die|an den|in die)(?: the| my| die| den)? ?(?:left|links|linke|linken)(?: side| half| edge| part| seite| hälfte| rand| bildschirmhälfte)?(?: of (?:the|my) (?:screen|monitor|display)| des bildschirms| vom bildschirm)?",
                r"(?:i want|i need|ich will) (?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?) (?:on|at|to|auf|an) (?:the |der |die )?(?:left|linken|links)(?: side| half| seite| hälfte)?(?: haben)?",
            ],
        ),
        shape(
            "snap {app} to the right",
            &[
                r"(?:snap|put|move|dock|place|pin|send|throw|push|shove|schieb|schiebe|pack|setz|setze|verschiebe|verschieb|zieh|ziehe)(?: the| my| den| die| das| mein)? (?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?)(?: window| fenster)? (?:to|on|onto|at|in|into|over to|nach|auf|an|zur|auf die|an die|an den|in die)(?: the| my| die| den)? ?(?:right|rechts|rechte|rechten)(?: side| half| edge| part| seite| hälfte| rand| bildschirmhälfte)?(?: of (?:the|my) (?:screen|monitor|display)| des bildschirms| vom bildschirm)?",
                r"(?:i want|i need|ich will) (?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?) (?:on|at|to|auf|an) (?:the |der |die )?(?:right|rechten|rechts)(?: side| half| seite| hälfte)?(?: haben)?",
            ],
        ),
        shape(
            "close the windows you opened",
            &[
                r"(?:close|shut|kill|hide|get rid of|clear|dismiss|remove|schließe|schließ|mach|entferne|räum)(?: all| all of| alle)?(?: your| those| these| deine| diese)+ ?(?:windows|panels|blocks|popups|pop ups|views|boards|panes|cards|fenster|blöcke|ansichten|karten)(?: you (?:opened|made|created|put up|showed|have open|just opened)| of yours| that you (?:opened|made|created|put up)| again| die du (?:geöffnet|aufgemacht) hast| von dir| wieder| zu| weg)*",
                r"(?:close|shut|kill|hide|get rid of|clear|dismiss|remove|schließe|schließ|entferne)(?: all| all of| alle)?(?: the| die)? (?:windows|panels|blocks|popups|pop ups|views|panes|cards|fenster|blöcke|ansichten|karten) (?:you (?:opened|made|created|put up|showed|have open|just opened)|of yours|that you (?:opened|made|created|put up)|die du (?:geöffnet|aufgemacht) hast|von dir)",
                r"(?:tidy|clean|clear)(?: up)? your (?:mess|windows|stuff|things|panels)(?: up| away)?",
                r"(?:put|take) (?:your|all your) (?:windows|panels|things|stuff) away",
            ],
        ),
        // ---------------------------------------------------------------- his own views ----
        shape(
            "open my system stats",
            &[r"(?:<SHOW>|pull up|bring up)(?: me| mir)? <ART>(?:system |pc |computer |hardware |performance |resource |live )*(?:stats|statistics|stat panel|stats panel|stats window|resource usage|resources|systemstatistiken|statistiken|systemwerte|systemauslastung|auslastung|leistungsdaten|hardware monitor)(?: panel| window| view| page| fenster| ansicht)?(?: in a new window| in a window)?"],
        ),
        shape(
            "system status",
            &[
                r"(?:system|pc|computer|hardware|machine|rechner|systems?) (?:status|state|health|report|check|zustand|bericht)",
                r"(?:give me|show me|tell me|whats|what is|run|do|gib mir|zeig mir|sag mir|wie ist|was ist|mach)(?: a| an| the| my| your| me| mir| einen| den| der)* (?:quick |full |short |kurzen )?(?:system |pc |computer |hardware |systems? )?(?:status|status report|status update|status check|health check|health report|diagnostics|statusbericht|systemstatus|systembericht|zustand)(?: report| update| check)?(?: of (?:the|my) (?:system|pc|computer|machine)| on (?:the|my) (?:system|pc|computer)| vom system| des systems)?",
                r"(?:how is|hows|how are|wie geht es|wie läuft)(?: my| the| meinem| dem| mein| der)? (?:pc|computer|system|machine|rig|hardware|rechner|systems)(?: doing| holding up| running| performing| so)*",
                r"(?:is|are) (?:everything|all systems|the system|my pc|my computer) (?:ok|okay|fine|running fine|running smoothly|running well|in order|nominal|good|alright|all right|healthy)",
                r"(?:all systems|systems?) (?:check|nominal|report)",
                r"(?:status report|status update|statusbericht|lagebericht)",
            ],
        )
        .asks(),
        shape(
            "open settings",
            &[
                r"(?:<SHOW>|pull up|bring up|go to|take me to|i need|geh zu|geh in)(?: me| mir)? <ART>(?:own |app |hud )?(?:settings|preferences|options|configuration|einstellungen|optionen|konfiguration|settings menu|settings panel|settings page|settings window|preferences menu|options menu|einstellungsmenü)(?: menu| panel| page| window| screen| fenster| menü)?",
                r"(?:i want to|let me|ich will|lass mich) (?:change|adjust|tweak|edit|configure) (?:some |a |your |the |my )?(?:settings|setting|preferences|options)",
                r"(?:ändere|änder) (?:ein paar |deine |die |meine )?(?:einstellungen|einstellung)",
            ],
        ),
        shape(
            "open the mixer",
            &[
                r"(?:<SHOW>|pull up|bring up)(?: me| mir)? <ART>(?:audio |volume |sound |app |lautstärke ?|audio ?|ton ?)?(?:mixer|mixing board|mixing desk|mixing panel|volume panel|volume controls|sound panel|sound controls|audio panel|audio controls|per app volume|per app volumes|app volumes|mischpult|lautstärkemixer|lautstärkeregler|audiomixer)(?: panel| window| view| fenster)?",
                r"(?:let me|i want to|i need to|ich will|lass mich) (?:see|adjust|change|control|set) (?:the |each |every |individual |my )?(?:app |application )?(?:volumes|volume levels|volume per app|volume of each app|sound levels)(?: per app| individually| for each app)*",
            ],
        ),
        shape(
            "open the text console",
            &[
                r"(?:<SHOW>|pull up|bring up|i need|i want)(?: me| mir)? <ART>(?:text |chat |typing |input |keyboard |text ?)?(?:console|chat box|chatbox|text box|textbox|text input|input box|input field|typing box|konsole|textkonsole|texteingabe|eingabefeld|chatfenster|textfeld)(?: window| panel| fenster)?",
                r"(?:i want to|let me|i need to|id like to|can i|ich will|lass mich|kann ich) (?:type|text|chat|enter text|type something|type to you|write to you|type a message|type instead|type it|tippen|dir schreiben|dir was schreiben|was tippen|etwas tippen|was eingeben|lieber tippen|lieber schreiben)(?: to you| with you| instead| mit dir)?",
            ],
        ),
        shape(
            "change the theme to {name}",
            &[
                r"(?:change|switch|set|make|turn|put|wechsle|wechsel|ändere|änder|stell|stelle|setz|setze|mach)(?: the| your| my| das| dein| mein| deine)? ?(?:theme|colou?r scheme|colou?rs|colou?r|skin|look|style|palette|design|farbschema|farbe|farben|aussehen|stil) (?:to|into|auf|zu|in) (?P<name>[a-zäöüß0-9 ]{3,24}?)(?: theme| colou?rs?| mode| um)?",
                r"(?:use|apply|activate|switch to|try|give me|i want|nimm|aktiviere|ich will)(?: the| das)? (?P<name>[a-zäöüß0-9 ]{3,24}?) (?:theme|colou?r scheme|skin|palette|look|farbschema|design)",
                r"(?P<name>[a-zäöüß0-9 ]{3,24}?) (?:theme|farbschema)",
            ],
        ),
        shape(
            "list your features",
            &[
                r"(?:<SHOW>|tell me|name|nenn|nenne|sag mir)(?: me| mir)?(?: all| a list of| all of)? ?(?:your|the|my|deine|die|alle deine)? ?(?:features|functions|abilities|capabilities|skills|commands|funktionen|fähigkeiten|befehle|kommandos|möglichkeiten)(?: list| you have| that you have| you can do| du hast| die du hast)?",
                r"(?:what|which|was|welche) (?:features|functions|abilities|capabilities|skills|commands|funktionen|fähigkeiten|befehle) (?:do you have|have you got|are there|can i use|do you know|do you offer|do you support|hast du|gibt es|kennst du|kann ich nutzen)",
                r"(?:what are you (?:capable of|able to do|good at|good for)|what are your (?:features|abilities|capabilities|skills|functions)|was kannst du(?: alles| so| eigentlich)*|was sind deine (?:funktionen|fähigkeiten))",
                r"(?:what|was)(?: all| alles| else)? (?:can|could|kannst) (?:you|du)(?: actually| even| really| eigentlich| alles| so)* ?(?:do|help me with|help with|machen|tun)?(?: for me| alles| so)*",
                r"(?:command list|feature list|list of commands|list of features|befehlsliste|funktionsliste)",
            ],
        )
        .asks(),
        shape(
            "show the lyrics",
            &[
                r"(?:<SHOW>|pull up|bring up|put up|find|i want|i need|hol|ich will|ich brauche)(?: me| mir)? <ART>(?:song |current |synced |live )?(?:lyrics|song text|songtext|liedtext|words of (?:this|the) song|text of (?:this|the) song|words to (?:this|the) song|text zum lied|text vom lied|lyrics panel|lyrics window)(?: for this song| for this| of this song| to this song| for the current song| dazu| davon| an| zum lied| von dem lied)?",
                r"(?:what are|whats|what is|was ist|wie ist|wie geht) (?:the |der )?(?:lyrics|songtext|liedtext)(?: of| to| for| von| zu)? ?(?:this|that|the|diesem|dem)? ?(?:song|track|lied)?",
                r"(?:lyrics|songtext|liedtext)(?: on| an)?",
                r"(?:i want to|let me|ich will) (?:sing along|read along|mitsingen|mitlesen)",
            ],
        ),
        shape(
            "show me a picture of {text}",
            &[
                r"(?:show|give|get|find|bring|pull up|display|fetch|search|look up|zeig|zeige|such|suche|finde|hol|gib)(?: me| mir| us| up)?(?: a| an| some| the| a few| ein| eine| einen| ein paar)? (?:picture|pictures|photo|photos|image|images|pic|pics|foto|fotos|bild|bilder|aufnahme|aufnahmen) (?:of|from|about|showing|vom|von|von der|von dem|von den|der|des|über)(?: a| an| the| some| einem| einer| dem| der| den)? (?P<text>.+)",
                r"(?:what does|what do|how does|how do|wie sieht|wie sehen) (?:a |an |the |der |die |das |ein |eine )?(?P<text>.+?) (?:look like|looks like|aus)",
            ],
        )
        .asks(),
        shape(
            "undo that",
            &[
                r"(?:undo|revert|reverse|take back|roll back|rollback|un do)(?: that| it| this| the last (?:thing|action|change|one|step)| what you (?:just )?did| your last (?:action|change|step)| that last (?:thing|one|action|change))?(?: again)?",
                r"(?:put|change|set|turn|switch|bring) (?:it|that|this|everything|things) back(?: the way it was| to how it was| to what it was| to normal| like before| again)?",
                r"(?:go|change it|switch|revert) back(?: to (?:how|the way) it was(?: before)?| to before| to what it was)",
                r"(?:mach|mache|nimm|macht) (?:das|es|dies|die letzte aktion|die letzte änderung|den letzten schritt|das letzte) (?:rückgängig|zurück|wieder rückgängig|ungeschehen)",
                r"(?:rückgängig|rückgängig machen|das rückgängig machen|zurück damit)",
                r"(?:stell|stelle|mach) (?:das|es|alles) (?:wieder )?(?:zurück|wie vorher|wie es war|so wie es war)(?: her)?",
            ],
        ),
        shape(
            "set a timer for {span}",
            &[
                r"(?:set|start|make|create|put|run|begin|give me|i need|i want|add|stell|stelle|starte|start|mach|mache|erstelle|setz|setze|ich brauche|ich will)(?: me| mir| up| on)?(?: a| an| the| another| einen| den| ein)? ?(?:new |quick |neuen |kurzen )?(?:timer|countdown|alarm|egg timer|kitchen timer|wecker|kurzzeitwecker)(?: for| of| to| at| auf| für| von| über| in| that goes off in| that rings in)? <SPAN>(?: from now| ab jetzt| an)?",
                r"<SPAN> (?:timer|countdown)",
                r"(?:timer|countdown|wecker)(?: for| auf| für| of)? <SPAN>",
                r"(?:remind|alert|ping|notify|wake|erinnere|erinner|weck|wecke)(?: me| mich)? (?:in|after|nach) <SPAN>",
                r"(?:count down|countdown|zähle|zähl)(?: from| for| von)? <SPAN>(?: runter| herunter| down)?",
                r"(?:let me know|tell me|sag mir|gib mir bescheid) (?:when|wenn|in) <SPAN> (?:are |is |have |has |sind |ist )?(?:up|over|passed|gone by|done|um|vorbei|rum)",
            ],
        ),
        shape(
            "show me my calendar",
            &[
                r"(?:<SHOW>|pull up|bring up|whats on|what is on|what do i have on|was steht in|was steht im|was habe ich im)(?: me| mir)? <ART>(?:calendar|schedule|agenda|appointments|diary|planner|kalender|terminkalender|termine|terminplan|zeitplan)(?: an| view| panel)?",
                r"(?:my |mein |meine )?(?:calendar|schedule|agenda|kalender|termine)",
            ],
        )
        .asks(),
        // ("In ten minutes show the briefing", "um 18:30 zeig mir das Briefing": Sir's own
        // examples of an order with a delay, 2026-11-13. The briefing was known as "brief me"
        // and "give me my briefing" only.)
        shape(
            "brief me",
            &[r"(?:<SHOW>|start|run|do|begin|starte|mach|hol)(?: me| mir)? <ART>(?:morning |evening |daily |executive |tages ?|morgen ?|abend ?)?(?:briefing|rundown|daily update|lagebericht|tagesbriefing|morgenbriefing)(?: now| jetzt| an)?"],
        ),
        shape(
            "show me the earth",
            &[
                r"(?:<SHOW>|pull up|bring up|put up|launch|start|load|i want to see|let me see|ich will)(?: me| mir)? <ART>(?:3d |live |interactive |real |whole |virtual |digital )*(?:earth|globe|planet|world|world map|planet earth|earth view|earth panel|earth globe|globus|erde|erdkugel|weltkugel|weltkarte|welt|erdansicht|planet erde)(?: view| panel| map| window| ansicht| an| sehen)*",
                r"(?:earth|globe|globus|erde|weltkugel|erdansicht)(?: view| ansicht)?",
                r"(?:take me to|go to|switch to|geh zur|wechsle zur) (?:the )?(?:earth|globe|earth view|erde|erdansicht|globus)(?: view| ansicht)?",
            ],
        ),
        shape(
            "zoom out",
            &[
                r"(?:zoom|pull|zoome)(?: it| the view| the map| the earth| the camera| the globe)? (?:out|back|away|further out|further away|further back|farther out|raus|heraus|weiter raus|weg|zurück|weiter weg)(?: a bit| a little| a little bit| more| further| some| ein bisschen| etwas| ein stück| mehr| weiter)*",
                r"(?:show|give me|i want|zeig|ich will)(?: me| mir)? (?:more of the (?:map|earth|globe|world|area)|a wider view|the bigger picture|the whole (?:earth|globe|world|planet|map)|mehr von der (?:karte|erde|welt)|die ganze (?:erde|welt|karte))",
                r"(?:wider|further out|farther out|further away|weiter raus|weiter weg|rauszoomen|herauszoomen|raus zoomen|zoom raus)",
            ],
        ),
    ];
    ways.extend(switch("calm mode", "calm mode off", "calm mode|calm modus|ruhemodus|ruhe modus|ruhiger modus|zen mode|relaxed mode"));
    ways.push(shape(
        "calm mode",
        &[r"(?:less|fewer|reduce|reduce the|weniger) (?:animations?|motion|movement|effects|animationen|bewegung|effekte)", r"(?:stop|turn off|disable|kill) (?:the |all the |all |your )?(?:animations|motion|moving stuff|effects)"],
    ));
    ways.extend(switch("mini mode", "mini mode off", "mini mode|mini modus|mini strip|ministrip|mini bar|minibar|mini hud|mini view|tiny mode|strip mode|mini ansicht|miniansicht|minimodus|minileiste|mini leiste"));
    ways.push(shape(
        "subtitles on",
        &[r"(?:show|display|write|print|zeig|zeige|schreib) (?:me )?(?:what you say|what youre saying|what you are saying|your words|your answers|your replies|was du sagst|deine antworten) (?:as text|on screen|on the screen|in text|als text|auf dem bildschirm)"],
    ));
    ways.extend(
        switch("subtitles on", "subtitles off", "subtitles?|subtitle bar|captions?|caption bar|subs|untertitel|untertitelleiste")
            .into_iter()
            .map(|mut way| {
                // (Where they stand is asked elsewhere: "subtitles to the top", "on monitor 2".)
                if !way.ideas.is_empty() {
                    way.ideas.push(Regex::new(r"^(?:[^0-9]*)$").expect("no number"));
                    if way.canon == "subtitles on" {
                        way.never = pattern(way.canon, &expand(r"(?:^| )(?:<TURNOFF>|<OFF>|<NOMORE>|<NOT>|what|why|how|was|warum|wie|ab|abschalten|ausschalten|ausmachen|beenden|deaktivieren|verlassen|top|bottom|middle|centre|center|oben|unten|mitte|follow|follows|folgen|where|wo|monitor|screen|bildschirm|window|fenster)(?: |$)"));
                    }
                }
                way
            }),
    );
    ways
}

/// What takes any name for an app or a window: last of all, when no other way was the one.
fn any_name() -> Vec<Way> {
    let mut ways = Vec::new();
    ways.push(shape(
        "open {app}",
        &[
            r"(?:fire up|boot up|start up|open up|load up|crank up|spin up|launch|run|boot|open|start|starte|öffne|aktiviere)(?: me| mir)?(?: the| my| den| die| das| mein| meine)? (?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?)(?: app| application| program| programm| anwendung)?(?: up)?",
            r"(?:mach|mache) (?:the |den |die |das |mein |meine )?(?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?) (?:auf|an)",
        ],
    ));
    ways.push(shape(
        "close {app}",
        &[
            r"(?:shut down|shut off|close down|kill|quit|exit|terminate|shut|close|beende|schließe|schließ|deaktiviere)(?: the| my| den| die| das| mein| meine)? (?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?)(?: app| application| program| programm| anwendung)?(?: down)?",
            r"(?:mach|mache) (?:the |den |die |das |mein |meine )?(?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?) (?:zu|aus)",
        ],
    ));
    ways.push(shape(
        "switch to {app}",
        &[
            r"(?:switch|jump|swap|flip|wechsle|wechsel|spring)(?: over| back| rüber| zurück)? (?:to|into|zu|zur|zum|auf|in) (?:the |my |den |die |das )?(?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?)(?: window| app| fenster)?",
            r"(?:focus|bring up|bring|show|give me|zeig|fokussiere)(?: me| mir)? (?:the |my |den |die |das )?(?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?)(?: window| app| fenster)? (?:to the front|to front|forward|in front|into focus|nach vorne|in den vordergrund)",
        ],
    ));
    ways
}

/// More ways of the same orders, found by going through sentences nobody had written a way
/// for: wishes ("I need the mixer"), what is asked in passing ("could you tell me where the
/// subtitles are"), the German that puts its verb last, and one order's own turns of phrase.
fn more() -> Vec<Way> {
    let mut ways = vec![
        // ---------------------------------------------------------------- brightness ----
        idea(
            "dim the side monitors",
            &["side|outer|outer two|left and right|other two|seitlichen|äußeren|seiten|seitenmonitore|beiden anderen", "<SCREENS>|seitenmonitore", "dim|dimmed|darker|dunkler|gedimmt|dimme|dimmen|turn down|brightness|helligkeit|dunkel|dark"],
            "<NUM>|brighter|heller|<NOT>",
        ),
        idea(
            "make the screens brighter",
            &["brighter|heller|more brightness|mehr helligkeit|too dark|too dim|zu dunkel|turn them up|turn it up|brightness up|helligkeit hoch|hochdrehen|hoch|higher brightness", "<SCREENS>|<BRIGHT>"],
            "<NUM>|how|wie|<NOT>|darker|dunkler|dimmer|lower|runter|less|weniger|side|seiten|outer|äußeren|volume|lautstärke|music|musik|room|office|school|class|classroom|store|shop|cinema|raum|büro|schule|kino",
        ),
        idea(
            "make the screens darker",
            &["darker|dimmer|dunkler|less bright|less brightness|weniger helligkeit|too bright|zu hell|dim|dimme|dimmen|gedimmt|dimmed|brightness down|helligkeit runter|runterdrehen|runter|reduce|verringere|down", "<SCREENS>|<BRIGHT>"],
            "<NUM>|how|wie|<NOT>|brighter|heller|side|seiten|outer|äußeren|left and right|other two|volume|lautstärke|music|musik|room|office|school|class|classroom|store|shop|cinema|raum|büro|schule|kino",
        ),
        shape(
            "make the screens brighter",
            &[r"(?:give me )?(?:more|higher|mehr) (?:<BRIGHT>|light|licht)(?: on| for| auf)?(?: <ART><SCREENS>)?", r"<ART><SCREENS> (?:is|are|ist|sind) (?:way |a bit |a little |viel |etwas )?(?:too (?:dark|dim)|zu dunkel)"],
        ),
        shape(
            "make the screens darker",
            &[r"(?:give me )?(?:less|lower|weniger) (?:<BRIGHT>|light|licht)(?: on| for| auf)?(?: <ART><SCREENS>)?", r"<ART><SCREENS> (?:is|are|ist|sind) (?:way |a bit |a little |viel |etwas )?(?:too bright|zu hell)"],
        ),
        shape("brightness {n} percent", &[r"(?:give me )?<ART>(?:<SCREENS>|<BRIGHT>) (?:at|on|to|auf) <NUM>(?: <BRIGHT>)?"]),
        shape("how bright are my monitors", &[r"how bright (?:is|are) (?:everything|it|things|they)(?: set)?(?: at the moment| currently)?", r"how bright <ART><SCREENS> (?:is|are)(?: set)?(?: at the moment| currently)?"]).asks(),
        // ----------------------------------------------------------------- dashboard ----
        idea("portrait dashboard off", &["dashboard|dash board", "hide|hiding|remove|take|off|aus|ausblenden|weg|<NOMORE>|<STOP>|<DEL>|verstecke|entferne|schließe"], "<NOT> (?:hide|remove|close|turn)"),
        idea("portrait dashboard", &["dashboard|dash board", "<START>|show|put|display|zeig|zeige|blende|ein|an|up|get|einblenden"], "hide|hiding|remove|take|off|aus|ausblenden|weg|<NOMORE>|<STOP>|<DEL>|verstecke|entferne|schließe|<NOT>|server|stats"),
        // ----------------------------------------------------------------- dictation ----
        shape(
            "start dictating",
            &[
                r"(?:start |begin |keep )?(?:typing|writing|type|write)(?: down| out)? (?:what i say|what im saying|what i am saying|everything i say|as i speak|as i talk|my words)",
                r"(?:mittippen|mitschreiben|tipp mit|schreib mit|tippe mit|schreibe mit)(?: was ich sage| was ich sag)?",
            ],
        ),
        shape(
            "stop dictating",
            &[
                r"(?:turn|switch|schalte|schalt|mach) <ART>(?:dictation|dictation mode|diktat|diktiermodus) (?:off|aus)",
                r"(?:aufhören|hör auf) (?:mitzutippen|mitzuschreiben|zu diktieren|zu tippen|zu schreiben)",
            ],
        ),
        // ---------------------------------------------------------------------- keys ----
        shape("go to the bottom", &[r"(?:end|bottom|ende) (?:of|der) (?:the |this )?(?:page|seite)"]),
        shape("go to the top", &[r"(?:top|start|beginning|anfang) (?:of|der) (?:the |this )?(?:page|seite)"]),
        shape("press enter", &[r"(?:einmal|once|1x) (?:enter|return|eingabe)"]),
        shape(
            "search the page for {text}",
            &[
                r"(?:where|wo) (?:does |is |do |steht |ist |kommt )?(?:the word |das wort )?(?P<text>.+?) (?:show up|shows up|appear|appears|come up|comes up|is mentioned|is written|occur|occurs|steht|vorkommt|auftaucht) (?:on|in|auf) <ART><PAGE>",
                r"(?:page search|search page|seitensuche|find on page|find in page) (?P<text>.+)",
                r"(?:such|suche|finde|find) (?:auf|in) <ART><PAGE>(?: nach)?(?: dem wort| das wort)? (?P<text>.+)",
                r"(?:wo steht|wo ist|where is|where does it say)(?: hier| here)? (?:auf|on|in) <ART><PAGE> (?P<text>.+)",
            ],
        )
        .asks(),
        shape(
            "next tab",
            &[
                r"(?:give me |go to |switch to |show |open )?<ART><TAB> (?:to the right|on the right|after this|after this one|right of this one|to the right of this one|rechts daneben|rechts|danach)",
                r"(?:go|jump|move|switch|skip|geh|spring|wechsle)(?: on| over)? (?:one |a |einen |ein )?<TAB> (?:ahead|forward|further|over|to the right|weiter|vor|nach rechts)",
                r"(?:move on|go on|go|jump|switch) to <ART><TAB> (?:after|following|to the right of) (?:this|this one|the current one)",
            ],
        ),
        shape(
            "previous tab",
            &[
                r"(?:give me |show me |go to |switch to |show |open )?<ART><TAB> (?:before|to the left|on the left|left of this one|to the left of this one|before this|before this one|davor|links daneben|links)",
                r"(?:go|jump|switch|move) to <ART><TAB> (?:on|to) the left",
                r"(?:wechsle|wechsel|geh|spring) (?:zum|auf den|in den) <TAB> (?:davor|links|vorher|links daneben)",
            ],
        ),
        shape(
            "page back",
            &[
                r"(?:take me|bring me|go|geh) (?:back|zurück) (?:one|a|eine) (?:page|seite)",
                r"(?:go |geh )?zurück zur (?:vorherigen|letzten|vorigen) seite",
                r"(?:get|go|take me|bring me) back to <ART><PAGE> (?:i was on )?(?:before|previously|earlier)",
            ],
        ),
        shape(
            "page forward",
            &[
                r"(?:hit|press|click|drück)(?: the| auf)? (?:forward button|forward|vorwärts)(?: button)? (?:in the browser|im browser)",
                r"(?:take me |bring me |go |geh )?(?:wieder )?(?:one |a |eine )?(?:page|seite) (?:forward|forwards|vor|vorwärts|weiter)(?: again| wieder)?",
                r"(?:take me|bring me|go) forward (?:one|a) page",
            ],
        ),
        shape(
            "reload the page",
            &[
                r"(?:load|lade) <ART><PAGE> (?:again|once more|nochmal|neu)",
                r"(?:hit|press|click|drück)(?: the| auf)? (?:refresh|reload|aktualisieren)(?: button)?",
                r"(?:reload|refresh|aktualisiere) (?:it|this|that|das|es)",
                r"<ART><PAGE> (?:braucht|needs)(?: mal)? (?:einen |a )?(?:refresh|reload)",
            ],
        ),
        shape(
            "reopen the closed tab",
            &[
                r"(?:give me |bring |get )?<ART><TAB> (?:i|that i|i just|that i just) closed(?: by accident)?(?: back)?",
                r"(?:get|bring|hol)(?: mir)? <ART>(?:last |just |recently )?(?:closed |geschlossenen )?<TAB>(?: i (?:just )?closed)? back",
                r"undo (?:close|closing|closed)(?: the| that)? <TAB>",
                r"(?:hol|bring)(?: mir)? <ART><TAB> (?:zurück|wieder)(?: den ich (?:eben )?(?:zugemacht|geschlossen) (?:habe|hab))?",
                r"(?:stelle wieder her|stell wieder her) <ART>(?:zuletzt |eben )?(?:geschlossenen )?<TAB>",
            ],
        ),
        shape(
            "scroll down",
            &[
                r"(?:scroll|go|move|scrolle|geh)(?: the page| it| this page)? (?:down|downwards|runter|nach unten)(?: a bit| a little| a little bit| ein bisschen| ein stück| etwas| more| further)*(?: on (?:the|this) page| auf der seite)?",
                r"(?:see|show me) (?:whats|what is) (?:further |farther )?(?:down|below|unten)(?: the page| on the page| here)?",
                r"(?:geh|scroll|scrolle)(?: auf der seite)?(?: ein stück| ein bisschen| etwas)?(?: weiter)? (?:runter|nach unten)",
            ],
        ),
        shape(
            "scroll up",
            &[
                r"(?:scroll|go|move|scrolle|geh)(?: the page| it| this page| back)? (?:up|upwards|hoch|rauf|nach oben)(?: a bit| a little| a little bit| ein bisschen| ein stück| etwas| more| further)*(?: on (?:the|this) page| auf der seite)?",
                r"(?:geh|scroll|scrolle)(?: auf der seite)?(?: ein stück| ein bisschen| etwas)?(?: weiter)? (?:rauf|hoch|nach oben)",
            ],
        ),
        // ------------------------------------------------------------------- layouts ----
        shape(
            "delete the {name} layout",
            &[
                r"(?:take|remove|strike|nimm) <ART>(?P<name>[a-zäöüß0-9 ]{2,30}?) (?:layout|anordnung) (?:off|from|out of|von|aus) (?:the|my|der|meiner) (?:list|liste)",
                r"<ART>(?:layout|anordnung)(?: called| named| namens)? (?P<name>[a-zäöüß0-9]{2,20}) (?:is|ist) (?:old|outdated|useless|veraltet|alt)(?: .*)?",
                r"<DEL> <ART>(?:layout|anordnung)(?: called| named| namens)? (?P<name>[a-zäöüß0-9]{2,20})(?: wieder| again)",
            ],
        ),
        shape(
            "list my layouts",
            &[
                r"(?:<SHOW> )?<ART>(?:saved |desk |window |gespeicherten |gespeicherte )*layouts(?: i saved| ive saved| i have saved| i have| that i saved| die ich gespeichert habe)?",
                r"(?:what|which|welche|was für) (?:desk |window )?layouts (?:i have|ive got|ive saved|i saved|habe ich gespeichert|ich habe|ich gespeichert habe|es gibt)",
            ],
        ),
        shape(
            "restore the {name} layout",
            &[
                r"(?:switch to|go to|change to|use|load|apply|activate|restore|wechsle zu|nimm|lade|lad|aktiviere) <ART>(?:layout|anordnung)(?: called| named| namens)? (?P<name>[a-zäöüß0-9]{2,20})",
                r"(?:give me|arrange|put|ordne|stell) (?:my |the |all my |meine |die |alle )?(?:windows|fenster|apps|everything|alles) (?:like|as|wie)(?: in| im| bei)?(?: the| my| dem| meinem)? (?P<name>[a-zäöüß0-9]{2,20}) (?:layout|anordnung)",
            ],
        ),
        // --------------------------------------------------------------------- notes ----
        shape(
            "note: {text}",
            &[r"give me (?:a |an |eine )?(?:new |quick |neue )?<NOTE>(?: that says| saying| with the text| mit dem text)? (?P<text>.+)", r"(?:notieren|aufschreiben) (?P<text>.+)"],
        ),
        shape(
            "delete the last note",
            &[r"<DEL> <ART><NOTE> (?:i|that i|i just|that i just) (?:made|took|wrote|added|saved|created)", r"(?:die |meine )?letzte <NOTE> (?:kann weg|soll weg|kannst du löschen)"],
        ),
        shape(
            "what did i note about {text}",
            &[
                r"(?:check|see|look|look up|schau|guck) (?:what|was) i (?:wrote|noted|jotted|put|saved|have written|have noted)(?: down)? (?:about|on|regarding|for) (?P<text>.+)",
                r"give me <ART>(?:notes|notizen) (?:about|on|regarding|über|zu) (?P<text>.+)",
                r"(?:did|have) i (?:note|noted|write|written|jot|save|saved)(?: down)? (?:anything|something|a note|notes) (?:about|on|regarding) (?P<text>.+)",
                r"(?:such|suche|durchsuche) (?:in )?<ART><NOTES> nach (?:dem |der |den )?(?P<text>.+)",
                r"(?:schauen|schau|guck|gucken|nachsehen|sieh nach) was ich (?:mir )?(?:über|zu|zum|zur) (?:den |die |das |dem |der )?(?P<text>.+?) (?:notiert|aufgeschrieben|gemerkt) (?:habe|hab)",
            ],
        )
        .asks(),
        // ------------------------------------------------------------------- reading ----
        shape(
            "read my clipboard",
            &[
                r"(?:hear|read|say|give me)(?: me)? (?:whats|what is|was) (?:in|on|auf|in der|in meiner) <ART>(?:clipboard|zwischenablage)(?: steht| ist)?",
                r"(?:whats|what is|was ist|was steht) (?:in|on|auf) <ART>(?:clipboard|zwischenablage)",
                r"(?:vorlesen|lies vor|lies mir vor|lies) was ich kopiert (?:habe|hab)",
                r"lies(?: mir)? was ich kopiert (?:habe|hab) vor",
                r"was (?:in|auf) <ART>zwischenablage (?:steht|ist)",
            ],
        ),
        shape(
            "read this to me",
            &[
                r"(?:hear|read|read me|speak|give me)(?: the| this| that)? (?:text|part|bit|passage|paragraph|selection|section|sentence) (?:i|that i|ive|i have)(?: just)? (?:selected|highlighted|marked)(?: out loud| aloud| to me)?",
                r"(?:lies vor|vorlesen|lies mir vor) was ich (?:markiert|ausgewählt) (?:habe|hab)",
                r"lies(?: mir)? was ich (?:markiert|ausgewählt) (?:habe|hab) vor",
            ],
        ),
        shape(
            "read on",
            &[
                r"(?:continue|go on|carry on|weiter)(?: with| mit)?(?: the| dem)? (?:text|rest|next part|next bit|next section|next paragraph|nächsten teil|nächsten abschnitt)",
                r"(?:hear|give me) (?:the |den )?(?:next|rest|nächsten)(?: part| bit| section| paragraph| teil| abschnitt| of it| of the text)*",
            ],
        ),
        shape(
            "read on",
            &[r"(?:the )?next (?:part|bit|section|paragraph|chunk)", r"(?:whats|what is|what comes|was kommt) next(?: in the text)?", r"(?:and then|and the rest|the rest|den rest|und der rest|und weiter|go on|weiter)"],
        )
        .while_(crate::kit_voice::reading_pending),
        shape(
            "stop reading",
            &[
                r"<STOP> <ART>(?:reading|read aloud|reading aloud|reading out|vorlesen|lesen)(?: that| this| it)?(?: out loud| aloud| out| to me)?",
                r"(?:i )?(?:dont|do not) want you to read(?: any further| anymore| any more| on| the rest)?",
                r"(?:you )?(?:dont|do not) (?:need|have) to read (?:the rest|any more|anymore|any further|on|further)",
                r"(?:stopp |stop )?(?:nicht|nichts) (?:mehr |weiter )?(?:weiterlesen|vorlesen|lesen)",
            ],
        ),
        // --------------------------------------------------------------------- rules ----
        shape(
            "whenever {app} starts {then}",
            &[r"(?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?) (?:starts|opens|launches|startet|öffnet) (?P<then>(?:mute|unmute|pause|stop|set|open|close|turn|play|show|lower|raise|start|resume|skip|dim) .+)"],
        ),
        shape(
            "delete rule {n}",
            &[r"(?:take|strike|remove|nimm) (?:rule|regel)(?: number)? (?P<n>\d+|one|two|three|four|five|eins|zwei|drei|vier|fünf) (?:off|from|von|aus) (?:the|my|der|meiner) (?:list|liste)"],
        ),
        shape(
            "list my rules",
            &[
                r"(?:<SHOW> )?(?:what|which|welche|was für) (?:automatic |active |saved )?(?:rules|automations|regeln|automatisierungen)(?: i have| ive got| ive set up| i set up| ive made| i made| did i make| did i set up| have i set up| are active| are set up| habe ich| habe ich eingerichtet| habe ich erstellt| ich eingerichtet habe| es gibt| gibt es)?",
                r"(?:rules|automations|regeln) (?:overview|list|übersicht|liste)",
            ],
        ),
        // --------------------------------------------------------------- sleep timer ----
        shape(
            "sleep timer {span}",
            &[r"(?:give me )?(?:the |my |die )?(?:music|musik|playback) (?:to |should |soll |must |needs to )?(?:turn off|stop|end|go off|switch off|shut off|ausgehen|aufhören|stoppen|aus) (?:in|after|nach) <SPAN>"],
        ),
        idea("cancel the sleep timer", &["<TIMER>", "nicht|kein|keinen"], "how long|how much|when|wie lange|wann|<SPAN>|lösch|stopp|cancel"),
        // ------------------------------------------------------------------ snippets ----
        shape(
            "list my snippets",
            &[r"(?:<SHOW> )?(?:mir |me )?<ART>(?:saved |stored |text )*<SNIPS>", r"(?:what|which|welche) <SNIPS> (?:i have|ive got|ive saved|i saved|are saved|ich habe|ich gespeichert habe|es gibt)"],
        ),
        shape(
            "save my clipboard as snippet {name}",
            &[
                r"(?:clipboard|zwischenablage) (?:to|as|into|als|zu) (?:a |an |ein |einen )?<SNIP>(?: called| named| namens)? (?P<name>[a-zäöüß0-9 ]{2,30})",
                r"(?:take|nimm) <ART><CLIP> (?:and|und) (?:save|keep|store|speichere) (?:it|that|es|das) (?:as|als)(?: a| an| ein| einen)? <SNIP>(?: called| named| namens)? (?P<name>[a-zäöüß0-9 ]{2,30})",
                r"(?:behalte|behalt|speichere|speicher) <ART><CLIP> (?:als|as)(?: ein| einen| a)? <SNIP>(?: namens| called| named)? (?P<name>[a-zäöüß0-9 ]{2,30})",
            ],
        ),
        shape(
            "type my {name}",
            &[
                r"(?:type|write|enter|insert|fill|put|drop|tipp|tippe|schreib|schreibe|füge|füg|setz)(?: in| out| down)? (?:my|the|mein|meine|meinen|das|den|die) (?:<SNIP> )?(?P<name>[a-zäöüß0-9]{2,20})(?: <SNIP>)?(?: in| here| in here| ein| hier| hier ein| hier rein| rein)*",
                r"give me(?: hier| here)? (?:my|the|mein|meine|das|den) (?:<SNIP> )?(?P<name>[a-zäöüß0-9]{2,20})(?: <SNIP>)?(?: typed| typed in| in| here| in here| hier)*",
            ],
        ),
        // ------------------------------------------------------------ the live desktop ----
        shape(
            "live desktop on",
            &[
                r"(?:be|become|go live as|go live on|werde|sei)(?: my| the| mein| meinem| meinen)? ?(?:live )?(?:desktop )?(?:background|wallpaper|backdrop|hintergrund|hintergrundbild|desktophintergrund)(?: on (?:my|the) desktop| auf dem desktop)?",
                r"(?:give me )?(?:dich|you) (?:als|as)(?: my| mein| meinen)? (?:live )?(?:desktop )?(?:hintergrundbild|hintergrund|background|wallpaper|backdrop)(?: auf dem desktop| on (?:my|the) desktop)?",
            ],
        ),
        shape(
            "live desktop off",
            &[
                r"(?:get|go|geh|verschwinde) (?:off|away from|out of|runter von|von|vom|aus)(?: my| the| meinem| dem)? (?:desktop )?(?:background|wallpaper|backdrop|desktop|hintergrund|desktophintergrund)",
                r"(?:aufhören|hör auf|stop being|quit being)(?: my| mein| meinen)? (?:live )?(?:desktop ?)?(?:background|wallpaper|backdrop|hintergrund|hintergrundbild|desktophintergrund)(?: zu sein)?",
            ],
        ),
        // --------------------------------------------------------------------- backups ----
        shape(
            "back up your memory",
            &[
                r"(?:save|keep|store) (?:a |an )?(?:backup|copy) of (?:your memory|yourself|your data|everything you know|everything you remember|your memories)",
                r"(?:make|create|do|run|take)(?: me)?(?: a| an)? ?(?:new |fresh |quick )?(?:backup|back up) of (?:everything you remember|everything you know|what you know|your memories|all you know)",
                r"(?:memory )?(?:backup|sicherung)",
            ],
        ),
        idea(
            "when was the last backup",
            &["backup|backups|back up|backed up|sicherung|sicherungen|gesichert", "when|last|latest|recent|newest|how old|how many|wann|letzte|letztes|letzten|neueste|wie alt|wie viele", "you|your|yourself|memory|du|dein|deine|deinem|dich|gedächtnis"],
            "restore|wiederherstellen|<DEL>|make a|create a|do a|run a|mach ein|mach eine|erstelle|go|scroll|page|geh|seite",
        )
        .asks(),
        // ------------------------------------------------------------- the changes list ----
        shape(
            "explain {text}",
            &[r"(?:what|how|was|wie) <ART>(?P<text>[a-zäöüß0-9 ]{3,40}?)(?: feature| function| funktion)? (?:does|is|is for|is good for|works|do|macht|ist|funktioniert|genau macht|eigentlich macht)"],
        )
        .asks(),
        shape(
            "show the changes",
            &[
                r"(?:show|tell)(?: me)? (?:whats|what is) (?:new|changed|different)(?: with you| in you| about you| lately)?",
                r"(?:<SHOW> )?(?:what|was)(?: has| have| hat)? (?:changed|sich geändert)(?: in you| on you| about you| with you| bei dir| an dir)?(?: lately| recently| zuletzt)?",
                r"(?:<SHOW> )?<ART>(?:liste mit den |liste der |list of |list of the )?(?:new |latest |recent |neuen |neue )*(?:features and fixes|changes and fixes|new features|new functions|new stuff|neuen funktionen|neue funktionen)",
            ],
        ),
        // ------------------------------------------------------------------- the coach ----
        shape(
            "stop the coach",
            &[
                r"(?:im |i am |we are |ich bin )?(?:done|finished|fertig)(?: with| mit)?(?: the| my| dem)? ?(?:pronunciation )?(?:practice|practise|practising|practicing|training|coaching|lesson|übung|üben)(?: for today| for now)?",
                r"(?:coach|coaching|pronunciation coach|aussprachecoach|aussprachetraining) (?:off|aus|stop|beenden)",
                r"(?:thats|that is) enough (?:pronunciation|practice|practise|training|coaching)(?: practice| training)?(?: for today)?",
                r"(?:nicht mehr|keine lust mehr zu) (?:üben|trainieren)",
            ],
        ),
        // ---------------------------------------------------------------------- guests ----
        shape(
            "{name} may do everything",
            &[
                r"(?:let|allow|permit|lass|erlaube) <WHO> (?:to )?(?:do|use|control|have) (?:everything|anything|it all|alles)(?: he wants| she wants| they want)?",
                r"(?:give me )?<WHO> (?:to be allowed to|should be allowed to|to be able to|to) (?:do|use|control) (?:everything|anything)",
                r"(?:can|may|could|darf|kann) <WHO> (?:have|get|haben|bekommen) (?:full|complete|total|all|vollen|volle) (?:access|control|rights|permissions|zugriff|kontrolle|rechte)(?: haben| bekommen)?",
                r"<WHO> (?:full access|full control|all access|voller zugriff)",
                r"erlaube <WHO> alles",
                r"<WHO> alles (?:machen|steuern|benutzen|tun) (?:darf|kann|soll)",
            ],
        )
        .of_people(),
        shape(
            "{name} may only control the music",
            &[
                r"the only thing <WHO> (?:gets to|may|can|is allowed to) (?:control|change|use|do) is the music",
                r"(?:give me )?<WHO> to be allowed to (?:do|control|change|use)(?: only| just)? the music(?: and nothing else| only)?",
                r"<WHO> (?:music only|only music|nur musik)",
                r"<WHO> (?:is|ist) (?:only|just|nur) allowed to (?:control|change|use|do) the music",
                r"<WHO> nur (?:die )?musik (?:machen|steuern|bedienen|ändern) (?:darf|kann|soll|dürfen)(?: sonst nichts)?",
                r"<WHO> (?:soll|darf|kann) nur (?:die )?musik(?: machen| steuern| bedienen| ändern)?(?: dürfen| können)?(?: sonst nichts| und sonst nichts)?",
            ],
        )
        .of_people(),
        shape(
            "forget {name}s voice",
            &[
                r"(?:forget|delete|remove|erase|drop|unlearn|wipe|vergiss|lösche|lösch|entferne) (?P<name>[a-zäöüß]{2,20}?)s? (?:voice|voice print|voiceprint|stimme|stimmabdruck) (?:from (?:your )?memory|from your list|aus deinem gedächtnis)",
                r"forget (?:what|how) <WHO> sounds(?: like)?",
            ],
        )
        .of_people(),
        shape(
            "this is my friend {name} remember his voice",
            &[
                r"(?:my |mein |meine )?(?:friend |buddy |mate |brother |sister |freund |freundin |kumpel |bruder |schwester )?<WHO> (?:is here|is visiting|is over|ist da|ist hier|ist zu besuch)(?: and| und)? (?:remember|learn|save|merk dir|lerne|lern|speichere)(?: his| her| their| seine| ihre)? (?:voice|stimme)",
                r"(?:merk dir|merke dir|merk|merke)(?: dir)? die stimme von(?: meinem| meiner)?(?: freund| freundin| kumpel| bruder| schwester)? <WHO>",
            ],
        )
        .of_people(),
        shape(
            "who do you know",
            &[r"(?:whose voices?|which (?:guests|people|voices|friends)|welche (?:gäste|stimmen|leute|freunde)|wessen stimmen?) (?:you|du)(?: know| recogni[sz]e| have learned| have learnt| have saved| kennst| gespeichert hast| gelernt hast)"],
        )
        .asks(),
        shape(
            "who is speaking",
            &[
                r"(?:who is|who was|wer ist|wer war) (?:talking|speaking|that)(?: just)?",
                r"(?:whose|wessen) (?:voice|stimme) (?:that|this|it|das) (?:is|was|ist|war)",
                r"(?:do you|did you|can you) recogni[sz]e (?:who is talking|who is speaking|who that is|who this is|who that was|who was talking|who was speaking)",
                r"wer (?:da |hier )?(?:eben )?(?:geredet|gesprochen|das gesagt) hat",
            ],
        )
        .asks(),
        shape(
            "guest mode",
            &[
                r"(?:answer to|answer|listen to|obey|react to|hör auf|höre auf|reagiere auf) (?:anyone|everyone|everybody|my guests|my friends|the guests|alle|meine gäste|meine freunde)(?: too| as well| auch)? (?:for a while|for now|for a bit|for the evening|tonight|for today|für eine weile|erstmal|heute abend|fürs erste)",
                r"(?:let|lass) (?:everybody|everyone|anyone|my guests|my friends|alle|meine gäste) (?:talk to you|speak to you|use you|mit dir reden) (?:for a while|for now|for a bit|for the evening|tonight|für eine weile|erstmal|heute abend)",
                r"(?:hör|höre) (?:für eine weile|erstmal|heute abend) auf alle(?: nicht nur auf mich)?",
            ],
        )
        .of_people(),
        // ------------------------------------------------------------- the hearing log ----
        shape(
            "show the hearing log",
            &[
                r"(?:<SHOW> )?<ART>(?:log|list|history|record|protokoll|liste|verlauf) (?:of|von|mit) (?:what|everything|all|dem was|allem was) (?:you|youve|you have|du)(?: have)? (?:heard|understood|picked up|gehört hast|verstanden hast)(?: from me| von mir)?(?: lately| recently| so far| today| zuletzt)?",
                r"(?:<SHOW> )?(?:everything|all|what) (?:youve|you have) (?:heard|understood|picked up)(?: from me)?(?: today| so far| lately| recently)?",
                r"(?:was|alles was) du(?: heute| bisher| so| in letzter zeit| zuletzt| alles)+(?: von mir)? (?:gehört|verstanden) hast",
            ],
        ),
        shape(
            "what did you just hear",
            &[
                r"(?:how|what) (?:did )?you (?:understood|understand|heard|hear|got|get) (?:my )?(?:last|previous) (?:sentence|words|order|command|thing i said)",
                r"what you (?:just )?(?:heard|understood)(?: a second ago| a moment ago| from me)?",
                r"was du(?: da| eben| zuletzt)*(?: von mir)? (?:gehört|verstanden) hast",
            ],
        ),
        // ------------------------------------------------------- who he listens to ----
        shape(
            "listen to everyone",
            &[
                r"(?:start |begin )?(?:obeying|listening to|reacting to) (?:everybody|everyone|anyone|anybody|all voices)(?: again)?",
                r"unlock(?: yourself| your ears)? (?:for|to) (?:everyone|everybody|all|anyone)",
            ],
        ),
        shape(
            "only obey me",
            &[
                r"(?:obey|listen to|hear|react to|respond to|take orders from|take commands from|follow) (?:only|just) (?:me|my voice)",
                r"(?:only|just) (?:take|taking|accept|accepting) (?:orders|commands) from (?:me|my voice)",
                r"nur (?:mir|auf mich) (?:gehorchst|gehorchen|hörst|hören|reagierst)",
            ],
        ),
        shape(
            "learn my voice",
            &[r"(?:learn|remember|memori[sz]e|study) (?:what|how) my voice sounds(?: like)?", r"(?:give me )?dir meine stimme beibringen", r"(?:ich bringe|ich bring) dir meine stimme bei"],
        ),
        // ------------------------------------------------------------ what he missed ----
        shape(
            "what did i miss",
            &[
                r"(?:what|was) (?:i|ich)(?: just)? (?:missed|verpasst habe)(?: while i was (?:away|gone|out|afk)| when i was (?:away|gone|out)| während ich weg war| als ich weg war)?",
                r"(?:which|what|welche) (?:notifications|alerts|messages|benachrichtigungen|meldungen) (?:came in|came|arrived|did i get|did i miss|i missed|i got|kamen|kamen rein|habe ich verpasst)(?: while i was (?:away|gone|out)| when i was (?:away|gone|out)| als ich (?:weg|nicht da) war| während ich weg war)?",
                r"(?:missed|verpasste) (?:notifications|alerts|messages|benachrichtigungen|meldungen)",
                r"(?:what|was) (?:came up|came in|happened|went on|ist passiert|war los|kam rein) (?:while i was (?:away|gone|out|afk)|when i was (?:away|gone|out)|in the meantime|während ich weg war|als ich weg war)",
                r"was (?:los war|passiert ist) (?:während|als) ich (?:weg|nicht da) war",
            ],
        )
        .asks(),
        // ------------------------------------------------------------- the privacy board ----
        shape(
            "privacy board",
            &[
                r"(?:see |give me |show me )?(?:what|which)(?: exactly)? (?:data|info|information) you (?:have|keep|store|save|hold) (?:on|about|of) me",
                r"(?:what|was)(?: exactly| genau)? (?:are you|do you) (?:saving|storing|keeping|recording|save|store|keep|record) (?:about|on|of) me",
                r"was du(?: alles)? (?:über|von) (?:mich|mir) (?:speicherst|gespeichert hast|sammelst|hast)",
                r"welche (?:daten|infos|informationen) du (?:über|von) (?:mich|mir) (?:hast|speicherst|gespeichert hast|sammelst)",
            ],
        )
        .asks(),
        idea(
            "stop keeping my voice clips",
            &["voice clips|clips of my voice|clips of me|voice recordings|recordings of my voice|recordings of me|audio of me|audio clips|clips von meiner stimme|aufnahmen von mir|aufnahmen meiner stimme|sprachclips", "dont want you to (?:keep|save|store|record)|do not want you to (?:keep|save|store|record)|dont want you (?:keeping|saving|storing|recording)|will nicht|nicht dass du|nicht will"],
            "delete|lösche|lösch",
        ),
        idea(
            "keep my voice clips again",
            &["voice clips|clips of my voice|clips of me|voice recordings|clips von meiner stimme|aufnahmen von mir|sprachclips", "back on|on again|wieder an|aufhebst|aufheben|aufhebe"],
            "<NOT>|<NOMORE>|<STOP>|delete|lösche",
        ),
        // ------------------------------------------------------------ what he has selected ----
        shape(
            "explain this",
            &[
                r"(?:explain|erkläre|erklär) (?:what ive|what i have) (?:selected|marked|highlighted)",
                r"(?:give me )?(?:an |a |eine )?(?:explanation|erklärung)(?: of| for| von| dazu| davon)?(?: this| that| it| the selection)?",
                r"(?:explain|erkläre|erklär) (?:the |den |die )?(?:bit|part|text|passage|section|stelle|teil) (?:i|that i|ive|i have)(?: just)? (?:selected|highlighted|marked)",
                r"(?:mir )?(?:erklären|erkläre|erklär)(?: mir)? was ich(?: da| hier)? (?:markiert|ausgewählt) (?:habe|hab)",
            ],
        )
        .asks(),
        shape(
            "summarise this",
            &[
                r"(?:give me|i need|write me)(?: a| an| the)? (?:quick |short |brief )?(?:summary|gist|short version) of (?:what i (?:highlighted|selected|marked)|the highlighted text|the bit i highlighted|the part i (?:selected|marked|highlighted))",
                r"sum (?:this|that|it) up",
            ],
        ),
        shape("translate this into {lang}", &[r"what does (?:this|that|it) say in <LANG>"]).asks(),
        // ------------------------------------------------------- where the subtitles stand ----
        idea("subtitles follow my window", &["<SUBS>", "move along with|go along with|come along|along with my"], "<NOT>|stop"),
        idea("where are the subtitles", &["where|wheres|where is|wo|cant find|can not find|which monitor|which screen", "<SUBS>"], "put|move|show|place|set|schieb|schiebe|setz|setze|zeig|zeige|stell|stelle|give me")
            .asks()
            .of_people(),
        shape("put the subtitles on monitor {n}", &[r"give me <ART><SUBS>(?: bar| leiste)? (?:on|auf)(?: my| the| den| dem)? ?<SCREENS>(?: number| nummer)? (?P<n>\d|one|two|three|eins|zwei|drei)"]),
        shape("subtitles to the {place}", &[r"give me <ART><SUBS>(?: bar| leiste)? (?:at |on |up at |an |am )?(?:the )?(?P<place>top|bottom|middle|centre|center|oben|unten|mitte)(?: of the screen| of my screen| am bildschirm)?"]),
        // ------------------------------------------------------- the older everyday orders ----
        shape(
            "switch to {app}",
            &[r"(?:give me |bring |put |hol )?(?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?) (?:in|to|into|nach|in den) (?:the )?(?:foreground|front|vordergrund|vorne)"],
        ),
        shape("open {app}", &[r"(?:get|bring) (?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?) (?:running|started|going|up and running|zum laufen)"]),
        shape(
            "list your features",
            &[r"what you (?:can|could) do", r"what (?:youre|you are) (?:able to do|capable of)", r"(?:zähl|zähle) (?:deine |alle deine )?(?:funktionen|fähigkeiten|befehle) auf"],
        )
        .asks(),
        shape(
            "calm mode",
            &[r"(?:make|mach)(?: the| your| das| die)? ?(?:interface|hud|screen|display|oberfläche|anzeige|it|things|yourself)? (?:calmer|less busy|ruhiger)", r"(?:give me )?(?:it|the hud|the interface) calmer(?: .*)?"],
        ),
        shape("mini mode", &[r"(?:shrink|shrink down|collapse)(?: down)?(?: to| into)?(?: the| a)? (?:small |mini |tiny )?(?:strip|bar|leiste)"]),
        shape(
            "show me a picture of {text}",
            &[
                r"(?:see|give me)(?: a| an| some| the| ein| eine)? (?:picture|pictures|photo|photos|image|images|pic|pics|foto|fotos|bild|bilder) (?:of|from|vom|von|von der|von dem)(?: a| an| the)? (?P<text>.+)",
                r"(?:ein |a )?(?:bild|foto|picture|photo|image) (?:vom|von|of|von der|von dem) (?P<text>.+)",
            ],
        ),
        shape("show the lyrics", &[r"(?:put|bring|get) <ART>lyrics (?:on screen|on the screen|up|auf den bildschirm)"]),
        shape(
            "pause the music",
            &[
                r"(?:give me )?(?:the |die )?(?:music|musik|song|playback) (?:to |should |soll )?(?:stop|pause|halt|stoppen|pausieren|anhalten|aufhören)",
                r"(?:pause|stop|halt) (?:whats|what is|whatever is|whatevers) playing",
                r"(?:put|set|setz) (?:the |this |that |die |den |das )?(?:music|song|track|playback|musik|lied) on (?:pause|hold)",
            ],
        ),
        shape(
            "previous song",
            &[
                r"(?:put|play|bring) (?:the |that )?(?:last|previous) (?:song|track|one) (?:back on|on again|again)",
                r"(?:give me )?(?:the |den |das )?(?:song|track|one|lied|titel) (?:before|from before|davor|von davor|von eben)(?: this one| this| that one)?(?: again| nochmal)?",
                r"(?:go )?back to (?:the |that )?(?:(?:song|track|one) (?:before|from before)|(?:last|previous) (?:song|track|one))",
                r"(?:geh|spring|go|jump) (?:ein|einen|one|a) (?:lied|song|titel|track) (?:zurück|back)",
            ],
        ),
        shape(
            "resume the music",
            &[r"(?:give me )?(?:the |die )?(?:music|musik|song|playback) (?:to |should |soll |can |kann )?(?:continue|resume|keep playing|go on|play again|weiterlaufen|weitergehen|weiterspielen|weiter)"],
        ),
        idea(
            "whats playing",
            &["song|track|tune|lied|titel|stück", "what|which|whats|name|called|welches|welcher|wie heißt|was für"],
            "play|spiel|spiele|next|previous|skip|pause|stop|resume|lyrics|like|mag|favorite|favourite|recommend|should|best|good|your|deine|lieblings|beste|last|before|davor|vorherige|nächste",
        )
        .asks(),
        idea(
            "system status",
            &["system|pc|computer|rechner|machine|rig", "how is|hows|how are|how the|how my|doing|holding up|status|health|wie geht|wie gehts|wie läuft|läuft|zustand"],
            "<STOP>|shut|restart|reboot|sleep|lock|sperr|herunterfahr|neustart|window|fenster|stats|statistics|internet|wifi|wlan",
        )
        .asks(),
        shape(
            "what time is it",
            &[r"(?:check |give me )?(?:the |die )?(?:time|uhrzeit)", r"what time it is", r"what the time is", r"whats the time", r"wie (?:viel uhr|spät) es ist"],
        )
        .asks(),
        shape(
            "set a timer for {span}",
            &[r"(?:put|set|give me|stell|mach)(?: a| an| einen| den)? (?:timer|countdown|wecker)(?: on| up| running)? (?:for|auf|für|of) <SPAN>"],
        ),
        shape("undo that", &[r"(?:nimm|mach) (?:das|es) wieder (?:zurück|rückgängig)"]),
        shape("open settings", &[r"(?:get into|go into|get to) <ART>(?:settings|preferences|options|einstellungen)"]),
        shape(
            "mute",
            &[r"(?:mute|silence) <ART>(?:pc|computer|system|everything|rechner|alles|whole thing)"],
        ),
        shape("minimize everything", &[r"(?:see|give me) (?:the |my |den |meinen )?desktop"]),
        shape(
            "snap {app} to the left",
            &[r"(?P<app>[a-z0-9äöüß]{3,20}) (?:to|on|nach|auf) (?:the )?(?:left|links)(?: side| half| seite| hälfte)?", r"(?:docke|dock|snap) (?P<app>[a-z0-9äöüß]{3,20}) (?:links|left)(?: an)?"],
        ),
        shape(
            "snap {app} to the right",
            &[r"(?P<app>[a-z0-9äöüß]{3,20}) (?:to|on|nach|auf) (?:the )?(?:right|rechts)(?: side| half| seite| hälfte)?", r"(?:docke|dock|snap) (?P<app>[a-z0-9äöüß]{3,20}) (?:rechts|right)(?: an)?"],
        ),
        shape(
            "close the windows you opened",
            &[r"(?:close|shut|kill|hide|get rid of|clear|dismiss|remove) (?:the |all the )?(?:stuff|things|everything) (?:you (?:opened|made|created|put up|showed|have open|just opened)|that you (?:opened|made|created|put up))"],
        ),
        shape("zoom out", &[r"(?:zoom|zoome)(?: mal)?(?: ein bisschen| etwas| ein stück| a bit| a little)? (?:out|raus|heraus|weiter raus)"]),
        shape("previous tab", &[r"(?:go|switch|move|jump|geh|wechsle) back (?:one |a )?<TAB>"]),
        shape("put the subtitles on monitor {n}", &[r"(?:give me )?<ART><SUBS>(?: bar| leiste)? (?:on|to|onto|auf)(?: my| the| den| dem)? (?P<n>first|second|third|main|primary|ersten|zweiten|dritten) ?<SCREENS>"]),
        shape("scroll down", &[r"(?:see |show me |give me )?(?:whats|what is) (?:further |farther )?(?:down|below|unten)(?: the page| on the page| here)?"]),
        shape(
            "whenever {app} starts {then}",
            &[r"(?:jedes|immer) wenn ich (?P<app>[a-z0-9äöüß][a-z0-9äöüß .+]{1,24}?) (?:starte|öffne|aufmache) (?P<then>(?:mute|unmute|pause|stop|set|open|close|turn|play|show|lower|raise|start|resume|skip|dim|mach|öffne|schließe|stell|spiel|pausiere|stoppe) .+)"],
        ),
        shape("list my rules", &[r"(?:what|which) (?:automatic |active |saved )?(?:rules|automations) (?:did|have) i (?:set up|make|create|made|created)(?: again)?"]),
        shape(
            "this is my friend {name} remember his voice",
            &[r"get to know(?: my| meinen| meine)?(?: friend| buddy| mate| brother| sister)? (?P<name>[a-zäöüß]{2,20}?)s? (?:voice|stimme)"],
        )
        .of_people(),
        idea("forget where the windows were", &["wo (?:die|deine|meine) fenster (?:waren|standen|sind|stehen)", "nicht mehr|vergiss|merk dir nicht"], ""),
        shape("stop reading", &[r"(?:thats|that is) enough reading(?: for now| for today)?"]),
        shape(
            "what did i do today",
            &[
                r"(?:summari[sz]e|recap|sum up|show me|give me) (?:what|was) (?:i|ich)(?: have)? (?:did|done|got done|worked on)(?: on (?:the|my) (?:pc|computer))?(?: today)?",
                r"todays (?:recap|summary|rundown|review|report)",
                r"(?:what|was) (?:was|were|have) (?:i|we)(?: been)? (?:up to|doing|working on)(?: on (?:the|my) (?:pc|computer))?(?: today| all day)?",
                r"(?:fass|fasse) zusammen was ich(?: heute)?(?: am (?:pc|computer|rechner))? (?:gemacht|getan|geschafft) (?:habe|hab)",
                r"(?:gib|zeig)(?: mir)? (?:einen |den |meinen )?(?:rückblick|tagesrückblick|bericht|zusammenfassung) (?:auf|über|von) (?:meinen|den|meinem) (?:tag|heutigen tag)",
            ],
        ),
        shape("open the text console", &[r"(?:let me |can i )?(?:type|chat|text)(?: something| to you| with you| a message| instead| it| mit dir)+"]),
    ];
    ways.extend(switch("calm mode", "calm mode off", "ruhigen modus|ruhige modus"));
    // The views of his own that a bare name asks for: "mixer please", "system stats".
    for (canon, names) in [
        ("open the mixer", "(?:audio |volume |sound )?mixer|audiomixer|mischpult"),
        ("open settings", "settings|settings menu|einstellungen"),
        ("open the text console", "text console|textkonsole"),
        ("open my system stats", "system stats|system statistics|pc stats|systemstatistiken"),
        ("show the lyrics", "lyrics|songtext|liedtext"),
        ("show my notes", "notes|notebook|notizen|notizbuch"),
        ("show the changes", "changes list|change ?log|list of changes|änderungsliste"),
        ("privacy board", "privacy board|privacy panel"),
        ("portrait dashboard", "portrait dashboard|hochkant dashboard"),
    ] {
        let named = format!("(?:give me )?<ART>(?:{names})");
        ways.push(shape(canon, &[named.as_str()]));
    }
    ways
}

/// Orders by what they are about alone: their own noun and a word of what is to happen to it,
/// wherever in the sentence and in whatever order. (Written from what each order means, not
/// from sentences: they are what reaches a wording nobody thought of.)
fn by_idea() -> Vec<Way> {
    vec![
        // (His own: "my notes", "which snippets". "Nobody read the notes" is somebody's story.)
        idea("list my snippets", &["my snippets|my text snippets|meine snippets|meine textbausteine|which snippets|what snippets|welche snippets|welche textbausteine|snippets i|snippets habe ich|snippet list|snippets list", "<SHOW>|what|which|welche|have|habe|saved|gespeichert|liste"], "<DEL>|type|tipp|schreib|<NOT>|called|named|namens"),
        idea(
            "show my notes",
            &["my notes|my notebook|my notepad|meine notizen|mein notizbuch|meinen notizen|meinem notizbuch|the notebook|das notizbuch", "<SHOW>|what|was|whats|lies|look"],
            "<DEL>|add|put|write|schreib|notier|notiere|about|über|zu|zum|zur|search|such|suche|find|last|letzte|<NOT>|nach",
        ),
        idea("delete the last note", &["<NOTE>", "<DEL>|undo|scratch|weg", "last|latest|newest|most recent|previous|just|letzte|neueste|eben"], "<NOT>|all|alle"),
        idea("new tab", &["<TAB>", "new|another|fresh|blank|empty|neuen|neuer|neues|weiteren|leeren|noch einen"], "<NOT>|close|schließ|schließe|window|fenster|<DEL>|closed|geschlossen"),
        idea("next tab", &["<TAB>", "next|following|right|forward|ahead|nächsten|nächste|nächster|rechts|weiter|vor"], "<NOT>|new|neuen|close|schließ|schließe|previous|back|left|last|vorherigen|zurück|links|song|lied|page|seite|closed"),
        idea("previous tab", &["<TAB>", "previous|prior|back|left|before|vorherigen|vorigen|vorherige|zurück|links|davor"], "<NOT>|new|neuen|close|closed|geschlossen|geschlossenen|next|nächsten|reopen|restore|page|seite|bring back|get back|zugemacht"),
        idea("reopen the closed tab", &["<TAB>", "reopen|restore|bring back|get back|recover|closed|geschlossenen|geschlossen|zugemacht|wiederherstellen|wieder auf|wieder her|zurückholen"], "<NOT>|new|neuen"),
        idea("reload the page", &["page|site|website|webpage|seite|webseite", "reload|refresh|refreshed|reloaded|aktualisiere|aktualisieren|aktualisier|neu laden|f5"], "<NOT>|auto|every|alle"),
        idea(
            "start dictating",
            &["dictation|dictate|dictating|diktat|diktieren|diktiere|diktiermodus", "<START>|take|let me|lass mich|give me|mode|modus"],
            "<STOP>|<OFF>|<NOMORE>|done|finished|fertig|<NOT>|aufhören|hör auf|ende|beenden|over|teacher|lehrer|lehrerin|class|school|test|exam|schule|klasse|grade",
        ),
        idea("stop dictating", &["dictation|dictate|dictating|diktat|diktieren|diktiermodus", "<STOP>|<OFF>|<NOMORE>|done|finished|fertig|aufhören|hör auf|ende|beenden|over"], "<NOT> (?:stop|end|quit)"),
        idea("list my rules", &["my rules|my automations|meine regeln|meine automatisierungen|which rules|what rules|welche regeln|rules i|rules ive|regeln ich|regeln habe ich|whenever rules|automatic rules", "<SHOW>|what|which|welche|was für|have|habe|active|aktiv|set up|eingerichtet|made|liste|overview|übersicht"], "<DEL>|rule \\d|regel \\d|whenever|wenn|when|add|new|neue|make|create|erstelle|<NOT>|of|for|von|für|game|spiel|school|schule"),
        idea("list my layouts", &["my layouts|meine layouts|which layouts|what layouts|welche layouts|saved layouts|desk layouts|window layouts|gespeicherte layouts|gespeicherten layouts|layouts i|layouts habe ich", "<SHOW>|what|which|welche|have|habe|saved|gespeichert"], "<DEL>|restore|load|switch|use|<NOT>|keyboard|tastatur"),
        idea("brightness {n} percent", &["<BRIGHT>|percent|prozent", "<SCREENS>|<BRIGHT>", "<NUM>"], "volume|lautstärke|sound|ton|opacity|transparenz|zoom|<SUBS>|timer|battery|akku|how|wie|side|seiten"),
        idea("show the changes", &["changelog|change log|release notes|patch notes|update notes|list of changes|changes list|your changes|änderungsliste|neuerungen|new features|neuen funktionen|neue funktionen", "<SHOW>|see|pull|bring|what|which|welche|was|liste"], "<NOT>|settings|einstellungen|save|speicher|undo|windows|driver|treiber|game|spiel|make|made|apply|keep"),
        idea("stop the coach", &["coach|coaching|pronunciation practice|pronunciation practise|pronunciation training|aussprachetraining|aussprachecoach|aussprache training", "<STOP>|<OFF>|<NOMORE>|done|finished|fertig|aufhören|hör auf|beenden|ende"], "<NOT> (?:stop|end|quit)|our|unser|football|soccer|team|bus|basketball|fußball|game|spiel|his|her"),
        idea("who do you know", &["guests|guest list|known voices|gäste|gästeliste|bekannten stimmen", "know|known|recogni[sz]e|learned|learnt|saved|kennst|gespeichert|gelernt|liste|<SHOW>|who|which|whose|wen|welche|wessen"], "<DEL>|forget|vergiss|add|new|neue|remember|merk|<NOT>|mode|modus|only|nur|here|da|over|coming")
            .asks(),
        idea("show the lyrics", &["lyrics|songtext|liedtext|song text", "<SHOW>|see|pull|bring|put|want|need|what|was|give me|sing along|mitsingen"], "<NOT>|<STOP>|<OFF>|hide|close|weg|aus"),
        idea("open the mixer", &["mixer|mischpult|audiomixer|lautstärkemixer", "<SHOW>|pull|bring|need|want|give me|see|öffne|zeig"], "<NOT>|<STOP>|close|hide|schließ|schließe|weg|wave link|elgato"),
        idea("set a timer for {span}", &["timer|countdown|wecker", "<SPAN>"], "<TIMER>|sleep|schlaf|<STOP>|<DEL>|cancel|how long|wie lange|left|übrig|remaining|music|musik"),
        idea("pause the music", &["music|song|track|playback|musik|lied|wiedergabe", "pause|paused|pausiere|pausier|pausieren|on hold|anhalten"], "<NOT>|<SPAN>|resume|continue|unpause|weiter"),
        idea("resume the music", &["music|song|track|playback|musik|lied|wiedergabe", "resume|continue|unpause|keep playing|carry on|back on|weiter|weiterspielen|weiterlaufen|fortsetzen|wieder an"], "<NOT>|<SPAN>|next|nächste|nächsten|nächstes|previous|vorherige|different|another|anderes|read|lesen"),
        idea(
            "next song",
            &["song|track|tune|title|lied|titel|stück", "next|skip|skipping|another|different|nächsten|nächstes|nächste|nächster|überspring|überspringe|überspringen|anderes"],
            "<NOT>|previous|last|before|back|vorherige|vorheriges|letzte|letztes|zurück|davor|what|which|welches|name|called|heißt|lyrics|tab|page|seite|queue|add|playlist|album|favorite|favourite|best|lieblings",
        ),
        idea(
            "previous song",
            &["song|track|tune|title|lied|titel|stück", "previous|before|vorherige|vorheriges|vorherigen|davor|vorige|vorigen"],
            "<NOT>|next|skip|nächste|what|which|welches|name|called|heißt|lyrics|tab|page|seite|liked|favourite|favorite",
        ),
        idea("minimize everything", &["windows|fenster|apps|programs|programme|everything|alles", "minimi[sz]e|minimi[sz]ed|minimiere|minimier|minimieren|minimiert"], "<NOT>|maximi[sz]e"),
        idea("subtitles to the {place}", &["<SUBS>", "top|bottom|middle|centre|center|oben|unten|mitte"], "<NOT>|where|wo|<TURNOFF>|<SCREENS> \\d"),
        idea("put the subtitles on monitor {n}", &["<SUBS>", "<SCREENS>", "<NUM>"], "<NOT>|where|wo|<TURNOFF>|percent|prozent"),
    ]
}

/// The orders' own wordings for a sentence that says one of them another way, the likeliest
/// first (the caller takes the first one the fast path reads: `brain::another_way`). `s`: the
/// fast path's normalised phrase; `original`: the words as they were said. Empty: not one of
/// them, or a sentence that tells of something instead of asking for it. (For the tests and the
/// corpus report; the brain asks `candidates`.)
#[cfg(test)]
pub fn wordings(s: &str, original: &str) -> Vec<String> {
    candidates(s, original).into_iter().map(|(wording, _)| wording).collect()
}

/// The same, each with whether it comes from a way that takes any name for an app or a window
/// ("fire up X" is "open X"): such a wording adds nothing to a sentence the fast path already
/// reads that way as it stands ("open 7-Zip" stays "open 7-Zip", hyphen and all).
pub fn candidates(s: &str, original: &str) -> Vec<(String, bool)> {
    #[cfg(test)]
    if SILENT.with(|silent| silent.get()) {
        return Vec::new();
    }
    let whole = of_sentence(s, original, true);
    if !whole.is_empty() {
        return whole;
    }
    // "They open your pronunciation coach", "Jay Layout Standard", "The Jay layout standard"
    // (Sir, 2026-11-13, each of them taken to the language model, which made a protocol of
    // one and woke him up for another): his name stands in front as the recogniser writes
    // it, and the order is what follows.
    if let Some((order, written)) = after_his_name(s) {
        // After his name as it is written, an order the fast path reads for good as it stands
        // is its own wording (never one that only takes words for the name of an app). After
        // a word that merely sounds like it, only an order with a way of its own counts:
        // "they play football on Sundays" names no song.
        let stands = written
            && matches!(
                crate::brain::fast_as_said(&order),
                Some(ref found @ (crate::brain::Fast::Intent(crate::intents::Intent::Tool { .. } | crate::intents::Intent::Silent(_)) | crate::brain::Fast::Route(_))) if !found.takes_any_name()
            );
        if stands {
            return vec![(order, false)];
        }
        let said_another_way = of_sentence(&order, original, false);
        if !said_another_way.is_empty() {
            return said_another_way;
        }
    }
    // "Okay my eyes hurt, take the brightness down a little", "that's it, quit the dictation":
    // the order is one clause of what was said, and the others only lead up to it (they say
    // the same, are a word or three, or ask for nothing themselves). "Tell Tom I'm late, and
    // turn the volume down" is two requests and stays whole.
    let clauses: Vec<&str> = original.split([',', ';', '—']).map(str::trim).filter(|c| !c.is_empty()).collect();
    if !(2..=3).contains(&clauses.len()) {
        return Vec::new();
    }
    let found: Vec<(usize, Vec<(String, bool)>)> = clauses.iter().enumerate().map(|(i, clause)| (i, of_sentence(&crate::intents::normalize_phrase(clause), clause, false))).filter(|(_, wordings)| !wordings.is_empty()).collect();
    for (at, wordings) in found.iter().rev() {
        let leads_up = |(i, clause): (usize, &&str)| {
            i == *at || found.iter().any(|(j, other)| *j == i && other.first() == wordings.first()) || clause.split_whitespace().count() <= 3 || !crate::ears::direct_intent(clause)
        };
        if clauses.iter().enumerate().all(leads_up) {
            return wordings.clone();
        }
    }
    Vec::new()
}

/// What follows his name where it stands in front as the recogniser writes it and the name's
/// own rules did not take it for the address it is (`wake::canon`): "jay layout standard",
/// "the jay layout standard", and in an accented voice "they open the mixer" (`wake`'s soft
/// names, `~they`). After his name as it is written an order heard as a report is the order
/// ("jay opened the mixer"); after a word that only sounds like it, what follows must be an
/// order as it was said: "they stopped the music" tells of other people. With whether the
/// name stood there as it is written.
fn after_his_name(s: &str) -> Option<(String, bool)> {
    // (As it is written, not the names of other people it is near to: "Jane", "Jake".)
    const WRITTEN: &[&str] = &["jai", "jay", "jae", "jey", "jei", "jaye"];
    let words: Vec<&str> = s.split(' ').collect();
    let at = usize::from(words.first() == Some(&"the") && words.get(1).is_some_and(|w| WRITTEN.contains(w)));
    let name = *words.get(at)?;
    let rest = words.get(at + 1..)?.join(" ");
    if rest.is_empty() {
        return None;
    }
    if WRITTEN.contains(&name) {
        return Some((crate::typos::imperative_after_name(&rest), true));
    }
    let first = words[at + 1];
    let an_order = rest.contains(' ') && first != "just" && crate::ears::COMMAND_VERBS.contains(&first);
    (at == 0 && crate::wake::accent_form(name) && an_order).then_some((rest, false))
}

/// The wordings for one sentence (`whole`) or one clause of it.
fn of_sentence(s: &str, original: &str, whole: bool) -> Vec<(String, bool)> {
    // ("No more voice clips": the fast path's phrase drops a "no" in front, which here is half
    // of what is said.)
    let kept = if original.trim().to_lowercase().starts_with("no more ") && !s.starts_with("no ") { format!("no {s}") } else { s.to_string() };
    let sentence = bare(&kept);
    if sentence.is_empty() || sentence.split(' ').count() > 18 || TOLD.is_match(&sentence) || DID.is_match(&sentence) {
        return Vec::new();
    }
    let (people, world) = (PEOPLE.is_match(&sentence), WORLD.is_match(&sentence));
    let shapes = variants(&sentence);
    let mut found: Vec<(String, bool)> = Vec::new();
    for way in WAYS.iter() {
        if (people && !way.people) || (world && !way.asks) || (way.any && !whole) || way.when.is_some_and(|holds| !holds() && !assumed()) {
            continue;
        }
        if let Some(wording) = shapes.iter().find_map(|shape| way.wording(shape, original)) {
            if !found.iter().any(|(known, _)| *known == wording) {
                found.push((wording, way.any));
                if found.len() == 4 {
                    break;
                }
            }
        }
    }
    // Last, what the sentence itself becomes without its frame where that changed its words:
    // "would you mind turning it down a little" is "turn it down a little", "I want Spotify
    // closed" is "close spotify". The fast path may read those as they stand.
    if whole && !people && !world {
        let minded = kept.starts_with("mind ") || kept.contains(" mind ");
        found.extend(shapes.iter().skip(if minded { 0 } else { 1 }).filter(|shape| !shape.starts_with("give me ") && shape.as_str() != s).take(3).map(|shape| (shape.clone(), true)));
    }
    found
}

/// Builds the patterns before the first order needs them: the ways themselves, and what a
/// sentence is asked before them (whether it tells of something, who it is about).
pub fn warm_up() {
    let _ = WAYS.len();
    let _ = of_sentence("warm up the ways of saying an order", "warm up the ways of saying an order", true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(said: &str) -> Option<String> {
        wordings(&crate::intents::normalize_phrase(said), said).into_iter().next()
    }

    /// What the brain makes of it: the first wording the fast path reads.
    fn taken(said: &str) -> Option<String> {
        crate::brain::another_way(said).map(|(wording, _)| wording)
    }

    #[test]
    fn a_request_is_what_is_left_without_its_frame() {
        for (said, left) in [
            ("could you maybe start the dictation for me real quick", "start the dictation"),
            ("i want you to cancel the sleep timer", "cancel the sleep timer"),
            ("would you mind opening a new tab", "open a new tab"),
            ("okay so id like to see my notes please", "give me my notes"),
            ("uh kannst du mir meine notizen zeigen", "meine notizen zeigen"),
            ("ich möchte gerne diktieren", "give me diktieren"),
            ("i need the mixer", "give me the mixer"),
            ("could you tell me where the subtitles are right now", "where the subtitles are"),
            ("would you mind only taking commands from me", "only take commands from me"),
            ("next tab", "next tab"),
            ("mach das hochkant-dashboard an", "mach das hochkant dashboard an"),
        ] {
            assert_eq!(bare(said), left, "{said}");
        }
        assert_eq!(verb_first("meine notizen zeigen").as_deref(), Some("zeig meine notizen"));
        assert_eq!(verb_first("das diktat starten").as_deref(), Some("starte das diktat"));
        assert_eq!(verb_first("die helligkeit ein bisschen runterdrehen").as_deref(), Some("dreh die helligkeit ein bisschen runter"));
        assert_eq!(verb_first("show my notes"), None);
        assert!(variants("give me rule 2 gone").contains(&"delete rule 2".to_string()));
        assert!(variants("i dont need the study layout anymore").contains(&"delete the study layout".to_string()));
    }

    #[test]
    fn every_way_is_a_valid_pattern_and_every_wording_one_the_fast_path_reads() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        // (Building the list compiles every pattern.)
        assert!(WAYS.len() > 120, "{} ways", WAYS.len());
        assert_eq!(BROKEN.load(Ordering::Relaxed), 0, "a pattern does not compile (see the log line it left)");
        let _ = crate::kit::put("layout", "study", &serde_json::json!({ "name": "study" }));
        let _ = crate::kit::put("snippet", "address", &serde_json::json!({ "text": "12 Example Road" }));
        let mut unread: Vec<String> = Vec::new();
        for way in WAYS.iter() {
            assert!(!way.shapes.is_empty() || !way.ideas.is_empty(), "'{}' has no way left", way.canon);
            let name = if way.canon.contains("layout") {
                "study"
            } else if way.canon.contains("theme") {
                "arc cyan"
            } else if way.canon.contains("snippet") || way.canon.starts_with("type my") {
                "address"
            } else {
                "tom"
            };
            let text = if way.canon.starts_with("explain ") { "the sleep timer" } else { "the test" };
            let wording = way.canon.replace("{n}", "2").replace("{span}", "20 minutes").replace("{text}", text).replace("{name}", name).replace("{app}", "discord").replace("{then}", "mute the music").replace("{place}", "top").replace("{lang}", "german");
            if crate::brain::fast_as_said(&wording).is_none() && !unread.contains(&wording) {
                unread.push(wording);
            }
        }
        crate::kit::remove("layout", "study");
        crate::kit::remove("snippet", "address");
        assert!(unread.is_empty(), "the fast path does not read these wordings as they stand: {unread:?}");
    }

    #[test]
    fn talk_is_not_taken_for_an_order() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        for said in [
            "we took so many notes in class today",
            "my teacher dictated the whole text",
            "the brightness of oled screens is insane",
            "what is a snippet in programming",
            "he set a sleep timer on his phone",
            "i was scrolling down for hours yesterday",
            "the next tab in the game is locked",
            "night mode on my phone makes everything look orange",
            "i like the night mode",
            "dont turn on night mode",
            "why do people use subtitles",
            "that song was way too loud at the party yesterday",
            "the last backup of my minecraft world got corrupted",
            "our internet went down twice during the storm",
            "i set a sleep timer on the tv and it turned off in the middle of the episode",
            "tell tom im late, and turn the volume down",
            "go to the left",
            "the screens in the computer room are way too bright",
            "mach mal die Tür zu, es zieht total",
            "what are the rules of chess",
            "that patch had so many changes nobody read the notes",
            "code snippets are saved on github",
            "i had to pause the song because mom called",
            "ich hab meine Notizen im Bus liegen lassen",
            "i forgot my notes at home",
            "did you hear me",
        ] {
            assert_eq!(taken(said), None, "{said}");
        }
    }

    #[test]
    fn the_same_order_said_other_ways() {
        for (said, wording) in [
            ("could you kill that sleep timer", "cancel the sleep timer"),
            ("i dont need the sleep timer anymore", "cancel the sleep timer"),
            ("schlaftimer aus", "cancel the sleep timer"),
            ("is my sleep timer still running", "how long is left on the sleep timer"),
            ("set me a sleep timer for 20 minutes", "sleep timer 20 minutes"),
            ("turn the music off in 20 minutes", "sleep timer 20 minutes"),
            ("add buy milk on Friday to my notes", "note: buy milk on Friday"),
            ("what have i written down so far", "show my notes"),
            ("open up a new tab", "new tab"),
            ("make my monitors a little brighter", "make the screens brighter"),
            ("its way too bright", "make the screens darker"),
            ("set all my monitors to 40 percent", "brightness 40 percent"),
            ("whenever i start discord then mute the music", "whenever discord starts mute the music"),
            ("mute the music every time discord opens", "whenever discord starts mute the music"),
            ("get rid of rule number two", "delete rule 2"),
            ("please stop typing what i say", "stop dictating"),
            ("kannst du mal meine Notizen aufmachen", "show my notes"),
            ("mach das Hochkant-Dashboard aus", "portrait dashboard off"),
            ("starte das Diktat", "start dictating"),
            ("forget Tom's voice", "forget toms voice"),
            ("this is my buddy Tom, please learn his voice", "this is my friend tom remember his voice"),
            ("Tom is only allowed to change the music", "tom may only control the music"),
            ("switch to guest mode", "guest mode"),
            ("turn the night mode off again", "night mode off"),
            ("move the subtitles up to the top", "subtitles to the top"),
            ("subtitles on the second monitor", "put the subtitles on monitor 2"),
            ("translate this into German for me", "translate this into german"),
            ("give me the gist of this", "summarise this"),
            ("could you turn the volume up a bit", "volume up"),
            ("its too loud", "volume down"),
            ("skip this song", "next song"),
            ("remind me in 10 minutes", "set a timer for 10 minutes"),
            ("fire up Spotify", "open spotify"),
            ("I need the mixer", "open the mixer"),
            ("can I see the settings?", "open settings"),
            ("could you tell me where the subtitles are right now", "where are the subtitles"),
            ("I want rule 2 gone", "delete rule 2"),
            ("das Layout study brauch ich nicht mehr", "delete the study layout"),
            ("kannst du die Helligkeit ein bisschen runterdrehen", "make the screens darker"),
            ("okay my eyes hurt, take the screen brightness down a little", "make the screens darker"),
            ("okay that's it, quit the dictation", "stop dictating"),
            ("stell das study Layout wieder her", "restore the study layout"),
            ("who was that just now", "who is speaking"),
            ("which rules are active right now?", "list my rules"),
            ("go back one tab", "previous tab"),
            ("ich brauch einen Timer für 10 Minuten", "set a timer for 10 minutes"),
            ("i want you to listen to all voices", "listen to everyone"),
            ("no more voice clips", "stop keeping my voice clips"),
            ("could you look for the word photosynthesis on the page", "search the page for photosynthesis"),
            ("kannst du mir vorlesen, was ich kopiert hab", "read my clipboard"),
            ("mute everything", "mute"),
        ] {
            assert_eq!(first(said).as_deref(), Some(wording), "{said}");
        }
    }

    /// What Sir said to his new build on 2026-11-13 and the language model guessed at (a
    /// protocol called "no_think", another called "jay_layout_standard", the orb layout, a
    /// wake-up): each sentence as Whisper wrote it, by the road the dispatcher takes.
    #[test]
    fn the_orders_of_the_thirteenth_that_went_to_the_model_are_understood() {
        use serde_json::json;
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        let road = |said: &str| crate::phrasings::route(&crate::wake::canon(said));
        // The pronunciation coach.
        let coach = Some(("pronunciation_coach".to_string(), json!({"action": "start"})));
        for said in [
            "Jay opened your pronunciation code.",
            "They open your pronunciation coach.",
            "Open your pronunciation coach.",
            "open the pronunciation coach",
            "open my pronunciation coach",
            "start the coach",
            "Jay, start your pronunciation coach.",
            "Day open your pronunciation coach",
            "Öffne deinen Aussprachecoach.",
            "Starte das Aussprachetraining",
        ] {
            assert_eq!(road(said), coach, "{said}");
        }
        // Keeping a desk layout: the workspace is captured under that name.
        let kept = |name: &str| Some(("manage_stark_protocol".to_string(), json!({"action": "capture_current_workspace", "protocol_name": name})));
        for (said, name) in [
            ("Jay, save that desk layout as standard.", "standard"),
            ("save this layout as work", "work"),
            ("save my desk as gaming", "gaming"),
            ("Save this as my gaming layout", "gaming"),
            ("Speichere dieses Layout als Arbeit.", "arbeit"),
            ("Speicher das Schreibtisch-Layout als Standard", "standard"),
        ] {
            assert_eq!(road(said), kept(name), "{said}");
        }
        // Bringing one back by its name. None is kept under "standard" here: the order is the
        // desk layouts' all the same, and their tool says that there is none of that name.
        let back = |name: &str| Some(("desk_layout".to_string(), json!({"action": "restore", "name": name})));
        for (said, name) in [
            ("Jay Layout Standard.", "standard"),
            ("The Jay layout standard.", "standard"),
            ("layout standard", "standard"),
            ("apply the desk layout standard", "standard"),
            ("switch to my gaming layout", "gaming"),
            ("Lade das Layout Standard", "standard"),
            ("Layout Standard laden", "standard"),
            ("wechsle zu meinem Gaming-Layout", "gaming"),
        ] {
            assert_eq!(road(said), back(name), "{said}");
        }
        // The HUD's own layouts keep their meaning, whichever way round they are said.
        assert_eq!(road("compact layout"), Some(("configure_interface".to_string(), json!({"layout": "compact"}))));
        for said in ["the standard layout", "switch to my standard layout"] {
            assert_eq!(road(said), Some(("configure_interface".to_string(), json!({"layout": "full"}))), "{said}: no desk layout is called that yet, and the fast path read it so before");
        }
        for said in ["layout compact", "layout orb", "load the layout compact"] {
            assert!(!matches!(road(said), Some((tool, _)) if tool == "desk_layout"), "{said}");
        }
        // Once he has kept one under that name, the name is his.
        let _ = crate::kit::put("layout", "standard", &json!({ "name": "standard" }));
        for said in ["the standard layout", "standard layout", "Jay, the standard layout.", "layout standard", "switch to my standard layout", "use the standard layout"] {
            assert_eq!(road(said), back("standard"), "{said}");
        }
        crate::kit::remove("layout", "standard");
    }

    /// A word that only sounds like his name in front of talk makes no order of it.
    #[test]
    fn talk_that_begins_like_his_name_stays_talk() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        for said in [
            "They open the shop at eight.",
            "They stopped the music at the party.",
            "They play football on Sundays.",
            "They opened your pronunciation coach in the next town.",
            "Day off tomorrow, finally.",
            "They said open the mixer.",
            "Jane opened the mixer in her room.",
            "The jay is a clever bird.",
            "Layout is the hardest part of web design.",
        ] {
            assert_eq!(taken(said), None, "{said}");
        }
        assert_eq!(after_his_name("they open your pronunciation coach"), Some(("open your pronunciation coach".to_string(), false)));
        assert_eq!(after_his_name("the jay layout standard"), Some(("layout standard".to_string(), true)));
        assert_eq!(after_his_name("jay opened the mixer"), Some(("open the mixer".to_string(), true)));
        assert_eq!(after_his_name("they stopped the music"), None, "a report about other people");
        assert_eq!(after_his_name("the layout standard"), None);
    }

    /// "Open Spotify" was understood before, if only by the rule that takes any name for an
    /// app: it stays exactly what it was. What that rule did not know is added.
    #[test]
    fn an_app_asked_for_in_the_old_words_is_left_as_it_was() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        for said in ["Open Spotify.", "Close Spotify.", "open 7-Zip", "switch to Spotify"] {
            assert_eq!(taken(said), None, "{said}");
        }
        assert_eq!(taken("get 7-Zip running").as_deref(), Some("open 7-zip"));
        assert_eq!(taken("I want Spotify closed").as_deref(), Some("close spotify"));
    }

    /// (Where the time of a sentence goes; `cargo test where_the_time_goes -- --ignored --nocapture`.)
    #[test]
    #[ignore]
    fn where_the_time_goes() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        let _ = WAYS.len();
        for said in ["tell me a joke about cats", "open the pod bay doors please", "could you kill that sleep timer", "what is the capital of france", "kannst du mal meine Notizen aufmachen"] {
            let phrase = crate::intents::normalize_phrase(said);
            let time = |what: &mut dyn FnMut()| {
                what();
                let started = std::time::Instant::now();
                for _ in 0..20 {
                    what();
                }
                started.elapsed().as_micros() / 20
            };
            let bare_us = time(&mut || {
                let _ = variants(&bare(&phrase));
            });
            let sentence = bare(&phrase);
            let shapes = variants(&sentence);
            let (mut by_shape, mut by_idea) = (0u128, 0u128);
            for way in WAYS.iter() {
                let started = std::time::Instant::now();
                for _ in 0..20 {
                    let _ = shapes.iter().find_map(|shape| way.wording(shape, said));
                }
                let took = started.elapsed().as_micros();
                if way.ideas.is_empty() { by_shape += took } else { by_idea += took }
            }
            let all_us = time(&mut || {
                let _ = candidates(&phrase, said);
            });
            let parse_us = time(&mut || {
                let _ = crate::intents::parse_as_said(said);
            });
            let fast_us = time(&mut || {
                let _ = crate::brain::fast_as_said(said);
            });
            let normal_us = time(&mut || {
                let _ = crate::intents::normalize_phrase(said);
            });
            eprintln!("{said}: normalize {normal_us} µs, bare+variants {bare_us} µs ({} variants), shapes {} µs, ideas {} µs, candidates {all_us} µs | parse_as_said {parse_us} µs, fast_as_said {fast_us} µs", shapes.len(), by_shape / 20, by_idea / 20);
        }
        let mut slowest: Vec<(u128, &str, usize)> = WAYS
            .iter()
            .map(|way| {
                let started = std::time::Instant::now();
                for _ in 0..50 {
                    let _ = way.wording("open the pod bay doors", "open the pod bay doors");
                }
                (started.elapsed().as_micros() / 50, way.canon, way.shapes.len())
            })
            .collect();
        slowest.sort_by_key(|(took, _, _)| std::cmp::Reverse(*took));
        for (took, canon, shapes) in slowest.iter().take(12) {
            eprintln!("  {took} µs  {canon} ({shapes} shapes)");
        }
    }

    #[test]
    fn what_is_vague_means_an_order_only_while_its_thing_goes_on() {
        assert_eq!(first("thats enough for today"), None, "no coach is open");
        assume_running(true);
        assert_eq!(first("thats enough for today").as_deref(), Some("stop the coach"));
        assume_running(false);
    }

    /// Sir, 2026-11-22: "in most cases he actually is able to do the things but just doesn't get
    /// the correct phrasing for it". What he said from 2026-11-19 to 2026-11-22 that went to the
    /// language model, though an order of his own was meant.
    #[test]
    fn his_misses_of_the_week_are_understood() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        let route = |said: &str| crate::phrasings::route(&crate::wake::canon(said));
        let tool = |said: &str| route(said).map(|(name, _)| name).unwrap_or_default();
        let model = route("Jay, can you make your own 3D model of the Empire State Building?").expect("a model of his own");
        assert_eq!((model.0.as_str(), model.1["query"].as_str()), (crate::model3d::TOOL, Some("empire state building")));
        let suit = route("Can you show me a model of the Iron Man suit in the new window?").expect("a model in a window");
        assert_eq!((suit.0.as_str(), suit.1["query"].as_str()), (crate::model3d::TOOL, Some("iron man suit")));
        assert_eq!(tool("Jay. Show me the function of a sine wave."), "launch_interactive_simulation");
        assert_eq!(route("Jay. Show the board."), route("show yourself"));
        assert_eq!(route("Jay liked this song."), route("like this song"));
        assert_eq!(route("Jay stop making the wallpaper transparent.").map(|(_, a)| a), Some(serde_json::json!({"opacity": 100})));
        assert_eq!(crate::apps::find_spec("sound settings").map(|s| s.id), Some("sound_settings"));
        assert_eq!(crate::apps::find_spec("the Wi-Fi settings").map(|s| s.id), Some("network_settings"));
        assert_eq!(crate::apps::find_spec("settings").map(|s| s.id), Some("settings"));
        // What already went its way stays there.
        for (said, name) in [("Jay, show me a sine wave.", "launch_interactive_simulation"), ("Jay, hide yourself.", "configure_interface"), ("Jay closed the earth.", "hud_board")] {
            assert_eq!(tool(said), name, "{said}");
        }
    }
}
