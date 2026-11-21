//! "Did you mean" (Sir, 2026-11-13): for an order he does not know, the two or three closest
//! orders he does know are offered, on the board and beside the emblem, to be taken with a
//! click or a word ("the first one", "number two", "none of them").
//!
//! A sentence the fast path does not read goes to the language model, as it always did. When
//! it looks like an order (it begins like one), the orders he knows are ranked against it at
//! once (`offer_for`): by the words they share, small words and endings left out, a word and
//! its other names counted as one ("wallpaper" is the background), the verbs every order has
//! ("open", "show", "put") counted for little. Nothing is offered below a sober bar: half of
//! what he said must be in the order, and half of the order's own words in what he said.
//! Never more than three, never for a plain question or small talk.
//!
//! The offer lapses after twenty seconds or with his next order; if the language model does
//! the right thing meanwhile, it is taken down at once (`dismiss`). Taking one runs that
//! order as if he had said it (`pick`, `answer`).

use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// How long an offer stands.
pub const STANDS: Duration = Duration::from_secs(20);
/// How many orders are offered at most.
pub const MOST: usize = 3;

/// One of the orders offered.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// What the chip says ("Open the mixer").
    pub label: String,
    /// The order it stands for, in the wording the fast path reads.
    pub order: String,
}

/// What is offered for a sentence he did not know.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    /// The sentence as he said it.
    pub said: String,
    pub choices: Vec<Choice>,
    /// When it was made.
    pub at: Instant,
}

impl Offer {
    /// How much of its time is left (1 … 0).
    pub fn left(&self) -> f32 {
        (1.0 - self.at.elapsed().as_secs_f32() / STANDS.as_secs_f32()).clamp(0.0, 1.0)
    }
}

/// The offer that stands, and the orders taken by a click that wait to be carried out (the
/// app runs them as if said).
struct Standing {
    offer: Option<Offer>,
    picked: Vec<String>,
}

#[cfg(not(test))]
static STANDING: std::sync::Mutex<Standing> = std::sync::Mutex::new(Standing { offer: None, picked: Vec::new() });
#[cfg(test)]
thread_local! {
    /// (A test has its offer to itself: other tests ask the fast path about "the first one"
    /// and "no" at the same time.)
    static STANDING: std::cell::RefCell<Standing> = const { std::cell::RefCell::new(Standing { offer: None, picked: Vec::new() }) };
}

fn standing<R>(f: impl FnOnce(&mut Standing) -> R) -> R {
    #[cfg(not(test))]
    {
        f(&mut STANDING.lock().unwrap_or_else(|e| e.into_inner()))
    }
    #[cfg(test)]
    {
        STANDING.with(|standing| f(&mut standing.borrow_mut()))
    }
}

// ------------------------------------------------------------------ the orders he knows ----

/// The orders he knows, each in the wording the fast path reads (`{span}`, `{n}` and `{name}`
/// are filled from the sentence), with other words people use for the same thing.
const KNOWN: &[(&str, &str)] = &[
    // Typing and the keys.
    ("start dictating", "dictation dictate diktat diktieren mitschreiben type write words say"),
    ("stop dictating", "dictation dictate diktat diktieren mitschreiben type write end"),
    ("list my snippets", "snippet textbaustein show"),
    ("next tab", "browser reiter forward"),
    ("previous tab", "browser reiter back last"),
    ("new tab", "browser reiter open another"),
    ("reopen the closed tab", "browser reiter restore back again"),
    ("scroll down", "page lower further"),
    ("scroll up", "page higher back"),
    ("go to the top", "page start beginning scroll"),
    ("go to the bottom", "page end scroll"),
    ("page back", "browser previous history"),
    ("page forward", "browser next history"),
    ("reload the page", "refresh website site browser"),
    ("select all", "everything mark highlight text"),
    ("paste it", "clipboard insert"),
    ("press enter", "key return hit"),
    // Notes, the sleep timer, rules.
    ("show my notes", "notebook notepad list read"),
    ("delete the last note", "remove notebook undo"),
    ("cancel the sleep timer", "music stop off"),
    ("how long is left on the sleep timer", "music time remaining"),
    ("sleep timer {span}", "music stop off asleep minutes hours"),
    ("list my rules", "automation whenever show"),
    // The monitors and the desk.
    ("make the screens brighter", "monitor display brightness light up dark"),
    ("make the screens darker", "monitor display brightness dim down bright"),
    ("how bright are my monitors", "screen display brightness level"),
    ("dim the side monitors", "screen display brightness darker outer"),
    ("brightness {n} percent", "monitor screen display set"),
    ("list my layouts", "desk window arrangement show saved"),
    ("save this layout as {name}", "desk window arrangement keep store setup"),
    ("portrait dashboard", "upright vertical monitor show"),
    ("portrait dashboard off", "upright vertical monitor hide close"),
    // Reading, the coach, hearing, guests.
    ("read this to me", "aloud selection text selected speak loud"),
    ("read my clipboard", "aloud copied text speak"),
    ("read on", "continue next rest reading further"),
    ("stop reading", "aloud enough end"),
    ("practise my pronunciation", "coach accent training english speaking trainer lesson practice"),
    ("stop the coach", "pronunciation practice training end enough"),
    ("show the hearing log", "heard understood history list"),
    ("what did you just hear", "heard understood said last"),
    ("who do you know", "guest voice list people recognise"),
    ("who is speaking", "voice talking recognise"),
    ("guest mode", "friend visitor company everyone listen"),
    ("guest mode off", "friend visitor gone alone left"),
    ("only obey me", "listen voice lock nobody else orders"),
    ("listen to everyone", "obey voice lock anyone all"),
    ("learn my voice", "remember recognise training print"),
    // What he tells and keeps.
    ("what did i do today", "recap day summary evening review"),
    ("why is my internet so slow", "wifi network connection diagnose lag doctor"),
    ("privacy board", "data keep store record about me"),
    ("stop keeping my voice clips", "recording audio save privacy"),
    ("keep my voice clips again", "recording audio save"),
    ("show the changes", "new changelog feature update list different"),
    ("back up your memory", "backup save copy data"),
    ("when was the last backup", "memory copy recent"),
    ("clear my notifications", "alert history delete empty"),
    ("what did i miss", "notification happened away catch up"),
    ("what is scheduled", "planned later order delay list"),
    ("cancel everything scheduled", "planned later order delay drop"),
    // The subtitles and the look.
    ("subtitles on", "caption show text words"),
    ("subtitles off", "caption hide text words"),
    ("subtitles to the top", "caption move put up"),
    ("subtitles to the bottom", "caption move put down"),
    ("subtitles follow my window", "caption monitor active track"),
    ("where are the subtitles", "caption monitor place"),
    ("forget where the windows were", "block place position reset default"),
    ("night mode", "dark dim quiet late"),
    ("night mode off", "dark dim normal day"),
    ("calm mode", "animation motion less quiet"),
    ("mini mode", "strip small shrink bar tiny"),
    ("live desktop on", "background wallpaper backdrop animated show"),
    ("live desktop off", "background wallpaper backdrop animated stop hide"),
    ("put the live background on all monitors", "desktop wallpaper backdrop every screen display"),
    ("use all monitors", "span stretch across every screen display board"),
    ("use one monitor", "span single main screen display board back"),
    // What he has selected.
    ("explain this", "selection selected text mean"),
    ("translate this", "selection selected text german english"),
    ("summarise this", "selection selected text summary short gist"),
    // The sound and the music.
    ("volume up", "louder sound loud turn quiet"),
    ("volume down", "quieter sound loud turn soft"),
    ("mute", "sound silence silent quiet audio"),
    ("set the volume to {n} percent", "sound level loud"),
    ("pause the music", "song stop playback halt"),
    ("resume the music", "song continue play again playback"),
    ("next song", "skip track music another"),
    ("previous song", "track music back last before"),
    ("whats playing", "song track music name title"),
    ("open the mixer", "volume audio sound app level"),
    ("show the lyrics", "song text words sing"),
    // His own views and the rest.
    ("open my system stats", "pc computer hardware cpu gpu usage performance"),
    ("system status", "pc computer hardware health report"),
    ("open settings", "preferences options configuration"),
    ("open the text console", "type chat keyboard write input"),
    ("show me my calendar", "schedule appointment agenda today"),
    ("show me the earth", "globe world map planet"),
    ("open the background window", "wallpaper backdrop picture video desktop"),
    ("open the protocol window", "routine steps edit"),
    ("open the hotkeys window", "keys shortcuts keyboard"),
    ("brief me", "briefing rundown morning report day update"),
    ("list your features", "abilities commands can do help"),
    ("minimize everything", "window desktop hide all"),
    ("close the windows you opened", "block panel tidy clean away"),
    ("undo that", "revert back take change last"),
    ("set a timer for {span}", "countdown alarm start minutes hours seconds"),
    ("what time is it", "clock late hour"),
    ("zoom out", "map earth wider further"),
    ("stop listening", "microphone mic privacy hear"),
];

/// Words that say nothing about which order is meant.
const SMALL: &[&str] = &[
    "the", "a", "an", "my", "your", "our", "this", "that", "these", "those", "it", "its", "me", "you", "i", "we", "to", "of", "on", "in", "at", "for", "as", "is", "are", "was", "be", "do", "does", "did", "can", "could",
    "would", "will", "should", "please", "now", "just", "and", "or", "with", "so", "then", "there", "here", "up", "want", "need", "like", "let", "lets", "some", "one", "jay", "jai", "they", "hey", "ok", "okay", "sir", "s",
    "something", "anything", "thing", "about", "bit", "little",
    "der", "die", "das", "den", "dem", "ein", "eine", "einen", "mein", "meine", "meinen", "dein", "deine", "ich", "du", "mir", "mich", "dir", "dich", "bitte", "mal", "und", "oder", "mit", "zu", "auf", "im", "ist",
    "kannst", "könntest", "jetzt", "es", "wir", "will", "möchte", "brauche",
];
/// The verbs nearly every order has: they count for little.
const EVERY_ORDER: &[&str] = &[
    "open", "show", "start", "stop", "close", "turn", "set", "make", "put", "give", "get", "switch", "go", "change", "use", "take", "move", "bring", "pull", "run", "do", "throw", "send", "have", "see", "tell", "list",
    // ("Play chess with me" is not "what's playing".)
    "play", "spiel",
    "zeig", "öffn", "mach", "start", "stell", "setz", "schalt", "gib", "hol", "nimm", "geh", "wechsl",
];
/// One thing and its other names (each word as it is said; all of a row count as its first).
const SAME: &[&[&str]] = &[
    &["music", "song", "songs", "track", "tracks", "tune", "musik", "lied", "lieder", "titel"],
    &["volume", "sound", "loudness", "lautstärke", "ton", "audio"],
    &["monitor", "monitors", "screen", "screens", "display", "displays", "bildschirm", "bildschirme", "monitore"],
    &["all", "every", "each", "both", "alle", "allen", "jeden", "jedem", "beide", "beiden"],
    &["layout", "layouts", "arrangement", "anordnung", "setup"],
    &["window", "windows", "fenster"],
    &["timer", "countdown", "wecker"],
    &["note", "notes", "notebook", "notepad", "notiz", "notizen", "notizbuch", "memo"],
    &["brightness", "bright", "brighter", "helligkeit", "heller", "hell"],
    &["darker", "dim", "dimmer", "dunkler", "dunkel", "dimmen"],
    &["subtitles", "subtitle", "captions", "caption", "subs", "untertitel"],
    &["background", "wallpaper", "backdrop", "hintergrund", "hintergrundbild", "desktophintergrund"],
    &["hotkeys", "hotkey", "keybinds", "keybind", "keybindings", "keybinding", "tastenkürzel"],
    &["desktop", "schreibtisch"],
    &["pronunciation", "accent", "aussprache", "akzent"],
    &["coach", "trainer", "coaching", "aussprachetraining", "aussprachecoach"],
    &["dictating", "dictation", "dictate", "diktat", "diktieren", "diktiere"],
    &["clipboard", "copied", "zwischenablage", "kopiert"],
    &["guest", "guests", "visitor", "visitors", "gast", "gäste", "besuch"],
    &["internet", "wifi", "wlan", "network", "connection", "netz", "netzwerk", "verbindung"],
    &["backup", "backups", "sicherung", "sicherungskopie"],
    &["notifications", "notification", "alerts", "benachrichtigungen", "meldungen"],
    &["tab", "tabs", "reiter"],
    &["page", "site", "website", "webpage", "seite", "webseite"],
    &["theme", "colour", "color", "colours", "colors", "farbe", "farben", "design", "farbschema"],
    &["mixer", "mischpult", "audiomixer"],
    &["settings", "preferences", "options", "einstellungen", "optionen"],
    &["calendar", "kalender", "appointments", "termine", "agenda"],
    &["brief", "briefing", "rundown", "lagebericht", "tagesbriefing"],
    &["earth", "globe", "world", "erde", "globus", "welt", "weltkugel"],
    &["lyrics", "songtext", "liedtext"],
    &["voice", "stimme"],
    &["night", "nacht", "nachtmodus"],
    &["rules", "rule", "regel", "regeln", "automation", "automations"],
    &["snippets", "snippet", "textbaustein", "textbausteine"],
    &["hear", "heard", "hearing", "gehört", "hören", "verstanden"],
    &["privacy", "datenschutz", "privatsphäre"],
    &["changes", "changelog", "änderungen", "neuerungen"],
    &["scheduled", "schedule", "planned", "geplant", "geplante"],
    &["louder", "lauter"],
    &["quieter", "leiser"],
    &["mute", "muted", "silence", "stumm"],
    &["pause", "paused", "pausiere", "pausieren", "anhalten"],
    &["resume", "continue", "weiter", "fortsetzen"],
    &["next", "nächste", "nächsten", "nächster", "nächstes"],
    &["previous", "vorherige", "vorherigen", "vorheriges", "letzte", "letzten"],
    &["delete", "remove", "erase", "lösche", "lösch", "löschen", "entferne", "entfernen"],
    &["cancel", "abbrechen", "storniere", "streich", "streiche"],
    &["save", "keep", "store", "speichere", "speicher", "speichern", "sichere"],
    &["read", "lies", "vorlesen", "lesen"],
    &["scroll", "scrolle", "scrollen"],
    &["reload", "refresh", "aktualisiere", "aktualisieren"],
    &["explain", "erkläre", "erklär", "erklären"],
    &["translate", "übersetze", "übersetz", "übersetzen"],
    &["summarise", "summarize", "summary", "zusammenfassen", "zusammenfassung", "fasse"],
    &["listen", "listening", "obey", "hör", "höre", "zuhören", "gehorche"],
    &["practise", "practice", "train", "üben", "übe", "trainieren", "trainiere"],
    &["minimize", "minimise", "minimiere", "minimieren"],
    &["playing", "plays", "läuft", "spielt"],
    &["brighter", "lighter"],
    &["today", "heute"],
    &["memory", "gedächtnis", "erinnerung"],
    &["clips", "clip", "recordings", "recording", "aufnahmen", "sprachaufnahmen"],
    &["stats", "statistics", "statistiken", "auslastung"],
    &["status", "state", "health", "zustand"],
    &["features", "feature", "abilities", "funktionen", "fähigkeiten", "befehle", "commands"],
    &["console", "konsole", "textkonsole"],
    &["everything", "alles"],
    &["slow", "slower", "langsam", "lahm", "laggy", "lagging"],
    &["miss", "missed", "verpasst"],
    &["span", "stretch", "spread", "spanne", "strecke", "erstrecke"],
    &["off", "away", "gone", "aus", "weg"],
];

/// A word without its ending: "monitors" and "monitor", "opened" and "open", "Notizen" and
/// "Notiz" are one word here.
fn stem(word: &str) -> String {
    let letters = word.chars().count();
    for ending in ["ing", "ed", "es", "en", "s", "e"] {
        if letters >= ending.len() + 4 {
            if let Some(cut) = word.strip_suffix(ending) {
                return cut.to_string();
            }
        }
    }
    word.to_string()
}

/// A word as it is compared: the first name of the thing it names, without its ending.
fn compared(word: &str) -> String {
    static FIRST_NAME: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
        let mut names = HashMap::new();
        for row in SAME {
            for word in row.iter() {
                names.entry(stem(word)).or_insert_with(|| stem(row[0]));
                names.entry(word.to_string()).or_insert_with(|| stem(row[0]));
            }
        }
        names
    });
    FIRST_NAME.get(word).or_else(|| FIRST_NAME.get(&stem(word))).cloned().unwrap_or_else(|| stem(word))
}

/// The words of a sentence that say which order is meant, as they are compared (each once).
fn telling(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in crate::ear::words(text) {
        if SMALL.contains(&word.as_str()) || word.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let word = compared(&word);
        if !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

fn weight(word: &str) -> f32 {
    if EVERY_ORDER.contains(&word) || EVERY_ORDER.contains(&stem(word).as_str()) { 0.3 } else { 1.0 }
}

/// One order he knows, ready to be compared.
struct Known {
    order: &'static str,
    /// The telling words of its own wording, and the ones among them that are no everyday verb.
    own: Vec<String>,
    nouns: Vec<String>,
    /// Other words for the same thing.
    other: Vec<String>,
}

static ORDERS: LazyLock<Vec<Known>> = LazyLock::new(|| {
    // (And every window of his that an "open …" order opens, by its title: "open the admin
    // pannel" found no program and nothing close, 2026-11-21.)
    let windows = crate::admin::WINDOWS.iter().filter_map(|w| match w.open {
        crate::admin::Open::Order(order) if order.starts_with("open ") && !KNOWN.iter().any(|(known, _)| *known == order) => Some((order, format!("{} {}", w.title, w.id.replace('_', " ")))),
        _ => None,
    });
    KNOWN
        .iter()
        .map(|(order, other)| (*order, other.to_string()))
        .chain(windows)
        .map(|(order, other)| {
            let own = telling(&order.replace("{span}", "").replace("{n}", "").replace("{name}", ""));
            let nouns: Vec<String> = own.iter().filter(|w| weight(w) >= 1.0).cloned().collect();
            Known { order, nouns: if nouns.is_empty() { own.clone() } else { nouns }, own, other: telling(&other) }
        })
        .collect()
});

static SPAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^| )((?:\d{1,3}|a|an|one|two|three|four|five|six|ten|fifteen|twenty|thirty|forty five|sixty|ninety|half an|eine|einer|zwei|drei|fünf|zehn|zwanzig|dreißig) ?(?:seconds?|minutes?|mins?|hours?|sekunden?|minuten?|stunden?))(?: |$)").expect("a span of time")
});
static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^| )(\d{1,3})(?: ?(?:percent|prozent|%))?(?: |$)").expect("a number"));
static NAMED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^| )(?:as|called|named|als|namens|unter) (?:the |my |a )?([a-zäöüß0-9]{2,20})(?: layout)?$").expect("a name"));

/// The wording with what it waits for taken from the sentence; None when the sentence does
/// not say it ("set a timer" names no span: nothing to offer).
fn filled(order: &str, said: &str) -> Option<String> {
    let said = crate::ear::words(said).join(" ");
    let mut out = order.to_string();
    for (slot, found) in [("{span}", &*SPAN), ("{n}", &*NUMBER), ("{name}", &*NAMED)] {
        if out.contains(slot) {
            out = out.replace(slot, found.captures(&said)?.get(1)?.as_str());
        }
    }
    Some(out)
}

fn label(order: &str) -> String {
    let mut letters = order.chars();
    letters.next().map_or(String::new(), |first| first.to_uppercase().chain(letters).collect())
}

/// The orders he knows that are closest to `said`, the closest first: at most three, and none
/// that is not close (see the module's head).
pub fn closest(said: &str) -> Vec<Choice> {
    let words = telling(said);
    let all: f32 = words.iter().map(|w| weight(w)).sum();
    if words.is_empty() || all < 1.0 {
        return Vec::new();
    }
    let mut ranked: Vec<(f32, Choice)> = Vec::new();
    for known in ORDERS.iter() {
        let shared: f32 = words.iter().filter(|w| known.own.contains(w) || known.other.contains(w)).map(|w| weight(w)).sum();
        let of_its_own = known.nouns.iter().filter(|w| words.contains(w)).count() as f32 / known.nouns.len().max(1) as f32;
        let of_his = shared / all;
        // The bar: a word of weight in common, half of what he said in the order, and half
        // of the order's own words in what he said.
        if shared < 1.0 || of_his < 0.5 || of_its_own < 0.5 {
            continue;
        }
        let Some(order) = filled(known.order, said) else { continue };
        ranked.push((of_his + of_its_own, Choice { label: label(&order), order }));
    }
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    let best = ranked.first().map_or(0.0, |(score, _)| *score);
    ranked.into_iter().filter(|(score, _)| *score >= best - 0.6).take(MOST).map(|(_, choice)| choice).collect()
}

/// Whether a sentence begins like an order ("open …", "can you …", "ich will …"): one he
/// does not know is worth an offer. A plain question and small talk are not.
pub fn looks_like_an_order(text: &str) -> bool {
    const LEADS: &[&str] = &["hey", "ok", "okay", "so", "well", "and", "now", "then", "please", "jay", "jai", "uh", "um", "also", "und", "dann", "bitte", "mal"];
    const ASKS_FOR: &[&str] = &["can you", "could you", "would you", "will you", "i want", "i need", "id like", "i would like", "lets", "let us", "kannst du", "könntest du", "würdest du", "ich will", "ich möchte", "ich brauche", "lass uns"];
    // (Verbs of orders beside the ones the microphone listens for: `ears::COMMAND_VERBS`.)
    const MORE_VERBS: &[&str] = &[
        "throw", "push", "toss", "drop", "load", "apply", "use", "activate", "deactivate", "dim", "brighten", "darken", "snap", "scroll", "reload", "refresh", "paste", "press", "select", "type", "dictate",
        "practise", "practice", "learn", "forget", "listen", "obey", "undo", "kill", "quit", "exit", "end", "clear", "wipe", "restore", "update", "change", "engage", "initiate", "nimm", "lade", "aktiviere",
        "deaktiviere", "beende", "dreh", "speichere", "speicher", "lösch", "vergiss", "übe", "hör", "pack", "schieb", "wirf",
    ];
    if crate::intents::only_asks(text) {
        return false;
    }
    let words = crate::ear::words(text);
    let from = words.iter().position(|w| !LEADS.contains(&w.as_str())).unwrap_or(words.len());
    let rest = words[from..].join(" ");
    let Some(first) = words.get(from) else { return false };
    (first != "just" && (crate::ears::COMMAND_VERBS.contains(&first.as_str()) || MORE_VERBS.contains(&first.as_str()))) || ASKS_FOR.iter().any(|lead| rest.starts_with(&format!("{lead} ")))
}

// ----------------------------------------------------------------------------- the offer ----

/// Offers the closest orders he knows for a sentence the fast path did not read (when there
/// are any). True when an offer stands now.
pub fn offer_for(said: &str) -> bool {
    if crate::intents::only_asks(said) {
        return false;
    }
    let choices = closest(said);
    if choices.is_empty() {
        return false;
    }
    tracing::info!("[MEANT] '{}': offering {}", said.chars().take(60).collect::<String>(), choices.iter().map(|c| format!("'{}'", c.order)).collect::<Vec<_>>().join(", "));
    let offer = Offer { said: said.trim().to_string(), choices, at: Instant::now() };
    standing(|s| s.offer = Some(offer));
    crate::gui::refresh();
    true
}

/// What an order that found nothing most likely was ("open the backgrounds window": no program
/// of that name): the one close order he knows, when all it does is open or show something.
/// More than one, or one that does anything else, is only ever offered. (Sir, 2026-11-21:
/// "make it so that JAI ... tries to guess if he doesn't find something".)
pub fn guess(said: &str) -> Option<Choice> {
    let choices = closest(said);
    let [only] = choices.as_slice() else { return None };
    let own = only.order != crate::ear::words(said).join(" ");
    (own && (only.order.starts_with("open ") || only.order.starts_with("show "))).then(|| only.clone())
}

/// The offer that stands, if one does.
pub fn current() -> Option<Offer> {
    standing(|s| {
        if s.offer.as_ref().is_some_and(|o| o.at.elapsed() >= STANDS) {
            s.offer = None;
        }
        s.offer.clone()
    })
}

/// The offer is taken down (his next order, "none of them", the model did the right thing).
pub fn dismiss() {
    standing(|s| s.offer = None);
}

/// Takes choice `n` (0 is the first) of the offer that stands: the order it stands for, and
/// the offer is gone.
pub fn take(n: usize) -> Option<String> {
    let offer = current()?;
    let order = offer.choices.get(n)?.order.clone();
    dismiss();
    Some(order)
}

/// Choice `n` (0 is the first) was clicked: its order runs as if he had said it (the app
/// carries out what waits here, `Jai::run_picked`).
pub fn pick(n: usize) {
    if let Some(order) = take(n) {
        tracing::info!("[MEANT] Taken by a click: '{order}'");
        standing(|s| s.picked.push(order));
    }
}

/// The orders taken by a click since the last call.
pub fn picked() -> Vec<String> {
    standing(|s| std::mem::take(&mut s.picked))
}

/// What a sentence says to the offer that stands.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// One of them: the order to carry out.
    Order(String),
    /// None of them.
    None,
}

static NTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(?:take |i meant |i mean |ich meinte |nimm )?(?:the |number |nummer |option |die |das |den )?(?P<n>first|1st|one|1|second|2nd|two|2|third|3rd|three|3|last|erste[ns]?|eins|zweite[ns]?|zwei|dritte[ns]?|drei|letzte[ns]?)(?: one| option| choice)?)$").expect("which of them")
});
static NONE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:none|none of them|none of those|neither|neither of them|no|nope|nah|no thanks|not that|nothing|never mind|keins|keine|keine davon|keins davon|nichts davon|nein|nee|weder noch)$").expect("none of them"));

fn plain(text: &str) -> String {
    let words = crate::ear::words(text);
    let words: Vec<&str> = words.iter().map(String::as_str).filter(|w| !matches!(*w, "jay" | "jai" | "please" | "bitte" | "sir")).collect();
    words.join(" ")
}

/// Which of them a sentence picks while an offer stands: "the first one", "the second",
/// "number two", "yes" when there is one choice, "none of them" / "no". None: it is no answer
/// to the offer (and nothing is taken down: the caller does that for every new order).
fn reading(text: &str) -> Option<Result<usize, ()>> {
    let offer = current()?;
    let said = plain(text);
    if NONE.is_match(&said) {
        return Some(Err(()));
    }
    // (A yes with one choice on offer; with a question of his own waiting, the yes is that
    // question's: `ask_first`.)
    if offer.choices.len() == 1 && crate::ask_first::pending().is_none() && crate::intents::is_affirmation(text) {
        return Some(Ok(0));
    }
    let n = NTH.captures(&said)?.name("n")?.as_str().to_string();
    let n = match n.trim_end_matches(['n', 's']) {
        "first" | "1st" | "one" | "1" | "erste" | "ei" => 0,
        "second" | "2nd" | "two" | "2" | "zweite" | "zwei" => 1,
        "third" | "3rd" | "three" | "3" | "dritte" | "drei" => 2,
        _ => offer.choices.len().saturating_sub(1),
    };
    (n < offer.choices.len()).then_some(Ok(n))
}

/// Whether a sentence would answer the offer that stands (nothing is taken).
pub fn would_answer(text: &str) -> bool {
    reading(text).is_some()
}

/// His answer to the offer that stands, taken: the order he picked, or that he wants none.
/// None for anything else.
pub fn answer(text: &str) -> Option<Answer> {
    match reading(text)? {
        Ok(n) => take(n).map(Answer::Order),
        Err(()) => {
            dismiss();
            Some(Answer::None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orders(said: &str) -> Vec<String> {
        closest(said).into_iter().map(|c| c.order).collect()
    }

    #[test]
    fn every_order_on_offer_is_one_the_fast_path_reads() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        let mut unread: Vec<String> = Vec::new();
        for (order, _) in KNOWN {
            let order = order.replace("{span}", "10 minutes").replace("{n}", "40").replace("{name}", "study");
            if crate::brain::another_way(&order).is_none() && crate::brain::fast_as_said(&order).is_none() {
                unread.push(order);
            }
        }
        assert!(unread.is_empty(), "offered, and not understood when taken: {unread:?}");
        assert!(KNOWN.len() > 90 && ORDERS.iter().all(|known| !known.own.is_empty() && !known.nouns.is_empty()));
    }

    #[test]
    fn the_closest_orders_he_knows_are_offered() {
        // "Throw it in all monitors" (2026-11-13) with nothing recent behind its "it".
        assert_eq!(orders("Throw it in all monitors."), ["use all monitors", "put the live background on all monitors"]);
        assert_eq!(orders("can you put the wallpaper on every screen"), ["put the live background on all monitors", "use all monitors"]);
        assert_eq!(orders("keep that desk arrangement as standard").first().map(String::as_str), Some("save this layout as standard"));
        assert_eq!(orders("give me a countdown of 10 minutes or so, will you").first().map(String::as_str), Some("set a timer for 10 minutes"));
        assert_eq!(orders("the captions should go away").first().map(String::as_str), Some("subtitles off"));
        assert_eq!(orders("mach mal die Untertitel weg").first().map(String::as_str), Some("subtitles off"));
        assert_eq!(orders("do something about the screen brightness").len(), 3, "never more than three: {:?}", orders("do something about the screen brightness"));
        assert!(orders("switch the briefing thing on").contains(&"brief me".to_string()));
        assert_eq!(closest("Throw it in all monitors.")[0].label, "Use all monitors");
    }

    #[test]
    fn nothing_is_offered_that_is_not_close() {
        for said in [
            "tell me a joke",
            "explain quantum physics",
            "write a poem about cats",
            "search the web for cheap flights to Lisbon",
            "translate good morning into French",
            "order a pizza",
            "book a table for two at eight",
            "open the pod bay doors",
            "make me a sandwich",
            "play chess with me",
            "set a timer",
            "hello there",
            "thank you very much",
            "",
        ] {
            assert_eq!(orders(said), Vec::<String>::new(), "{said}");
        }
        // A plain question and small talk get no offer, however close their words are.
        for said in ["are the subtitles on?", "why is the music so loud?", "what is a sleep timer?"] {
            assert!(!offer_for(said) && current().is_none(), "{said}");
        }
        assert!(looks_like_an_order("Throw it in all monitors.") && looks_like_an_order("can you put the wallpaper on every screen") && looks_like_an_order("Jay, mach mal die Untertitel weg"));
        for said in ["the weather is nice today", "I think the subtitles are great", "are the subtitles on?", "good morning", "that was a long day"] {
            assert!(!looks_like_an_order(said), "{said}");
        }
    }

    #[test]
    fn an_open_order_that_found_nothing_is_taken_as_the_one_close_view() {
        // "open the backgrounds window" found no program of that name (Sir, 2026-11-21).
        for (said, meant) in [("open the backgrounds window", "open the background window"), ("Jay, open the keybinds window", "open the hotkeys window"), ("open the protocols window please", "open the protocol window"), ("open the admin pannel", "open the admin window"), ("open the accounts", "open the accounts list")] {
            assert_eq!(guess(said).map(|c| c.order).as_deref(), Some(meant), "{said}");
            // (Carried out by the fast path in the same turn: it must be an order there.)
            assert!(matches!(crate::brain::fast_as_said(meant), Some(crate::brain::Fast::Intent(_))), "{meant}");
        }
        // Its own words, nothing close, more than one close, or one that does more than open.
        for said in ["open the background window", "open teams", "open the pod bay doors", "Throw it in all monitors.", "the captions should go away"] {
            assert_eq!(guess(said).map(|c| c.order), None, "{said}");
        }
    }

    #[test]
    fn an_offer_is_taken_by_a_word_or_a_click_and_lapses() {
        assert!(current().is_none() && answer("the first one").is_none() && !would_answer("yes"));
        assert!(offer_for("Throw it in all monitors."));
        let offer = current().expect("an offer stands");
        assert_eq!((offer.said.as_str(), offer.choices.len()), ("Throw it in all monitors.", 2));
        assert!(offer.left() > 0.9);
        // By voice: which of them, in English and German.
        for (said, order) in [("the first one", "use all monitors"), ("The second.", "put the live background on all monitors"), ("number two", "put the live background on all monitors"), ("Jay, the first one please", "use all monitors"), ("die zweite", "put the live background on all monitors"), ("das erste", "use all monitors"), ("the last one", "put the live background on all monitors")] {
            assert!(offer_for("Throw it in all monitors."));
            assert!(would_answer(said), "{said}");
            assert_eq!(answer(said), Some(Answer::Order(order.to_string())), "{said}");
            assert!(current().is_none(), "{said}: taken once");
        }
        // "Yes" picks only when there is one choice; "the third" of two is no answer.
        assert!(offer_for("Throw it in all monitors."));
        assert_eq!((answer("yes"), answer("the third one")), (None, None));
        assert!(current().is_some(), "talk leaves the offer where it is until his next order");
        for none in ["none of them", "No.", "neither", "keins davon", "nein"] {
            assert!(offer_for("Throw it in all monitors."));
            assert_eq!(answer(none), Some(Answer::None), "{none}");
            assert!(current().is_none());
        }
        standing(|s| s.offer = Some(Offer { said: "x".into(), choices: vec![Choice { label: "Subtitles off".into(), order: "subtitles off".into() }], at: Instant::now() }));
        assert_eq!(answer("yes"), Some(Answer::Order("subtitles off".to_string())), "one choice: yes takes it");
        // (With a question of his own waiting for a yes, the yes is that question's.)
        standing(|s| s.offer = Some(Offer { said: "x".into(), choices: vec![Choice { label: "Subtitles off".into(), order: "subtitles off".into() }], at: Instant::now() }));
        crate::ask_first::hold("x", "configure_interface", &serde_json::json!({"layout": "orb"}), "Did you mean: switch to the orb layout, Sir?");
        assert_eq!(answer("yes"), None);
        crate::ask_first::drop_it();
        dismiss();
        // By a click: the order waits for the app to carry it out.
        assert!(offer_for("Throw it in all monitors."));
        pick(1);
        assert_eq!(picked(), ["put the live background on all monitors"]);
        assert!(picked().is_empty() && current().is_none());
        pick(0);
        assert!(picked().is_empty(), "nothing on offer: nothing to take");
        // It lapses by itself.
        standing(|s| s.offer = Some(Offer { said: "x".into(), choices: vec![Choice { label: "Mute".into(), order: "mute".into() }], at: Instant::now() - STANDS - Duration::from_millis(5) }));
        assert!(current().is_none() && answer("the first one").is_none());
    }
}
