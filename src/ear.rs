//! His ear for Sir's voice (Sir, 2026-11-13: "understand the words correctly even though my
//! pronunciation isn't good … while I am talking to him he learns how I pronounce things").
//!
//! Whisper writes Sir's accented English as plausible other words, and it writes the same
//! words for the same thing again and again (jai.log, 2026-11-12): "this card" for Discord
//! three times, "sum out" for zoom out, "They" and "Day" for his name. This puts such words
//! right before anything decides whom they are for or what they ask, from five sources:
//!
//! 1. what Sir taught him in so many words ("when I say this card I mean Discord");
//! 2. words he has learnt to take for other words (`Kind::Words`);
//! 3. sound: in an order, the application whose name sounds like the words where its name
//!    belongs ("open this card" → "open Discord");
//! 4. a whole order he has learnt to take for another (`Kind::Order`);
//! 5. Sir's own phrase book: an order that leads nowhere and sounds like one Sir has given
//!    before ("sum out" → "zoom out").
//!
//! Only what Sir taught is taken on its word. Everything else is a rewrite that must be an
//! order he knows (`routes`), sound alone never rewrites into an order that takes something
//! away, and the two by sound (3 and 5) are only for words spoken to him.
//!
//! He learns by himself: a rewrite by sound that Sir lets stand is counted and kept once it
//! was made twice; an order that led nowhere and is said again within moments in words that
//! do lead somewhere is paired with them when the two sound alike; what a second, careful
//! hearing of the same clip makes of it is paired with the first; and every order that led
//! somewhere joins the phrase book. "That's not what I said" right after a rewrite takes it
//! back, and one taken back is not made again until Sir has let it stand more often than
//! that. It is kept in the memory database on this PC (`ear_learnt`), nowhere else, and a
//! scripted session neither reads nor writes it. Every rewrite is logged as `[EAR]`.

use crate::intent_engine::{sound_key, sound_similarity};
use regex::Regex;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What kind of thing he has learnt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Words heard for other words, wherever they stand in an order.
    Words,
    /// A whole order heard as another sentence.
    Order,
    /// What Sir taught in so many words: it holds everywhere and at once.
    Taught,
    /// An order of Sir's that led somewhere, in his own words (the phrase book).
    Said,
    /// An everyday word his name arrives as in Sir's voice ("they").
    Name,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Words => "words",
            Kind::Order => "order",
            Kind::Taught => "taught",
            Kind::Said => "said",
            Kind::Name => "name",
        }
    }

    fn of(tag: &str) -> Option<Kind> {
        [Kind::Words, Kind::Order, Kind::Taught, Kind::Said, Kind::Name].into_iter().find(|k| k.tag() == tag)
    }
}

/// One thing he has learnt: what was heard, what it is taken for, how often it held, and how
/// often Sir took it back.
#[derive(Debug, Clone, PartialEq)]
pub struct Learnt {
    pub heard: String,
    pub meant: String,
    pub kind: Kind,
    pub seen: u32,
    pub taken_back: u32,
}

/// A rewrite he learnt by himself holds once it was seen this often …
const TRUSTED_AFTER: u32 = 2;
/// … and a word is a name of his in Sir's voice once it came this often.
pub const NAME_SIGHTINGS: u32 = 2;

impl Learnt {
    /// Whether it is applied without sound having to agree again.
    pub fn trusted(&self) -> bool {
        match self.kind {
            Kind::Taught => true,
            Kind::Said => self.seen >= 1,
            Kind::Name => self.seen >= NAME_SIGHTINGS,
            Kind::Words | Kind::Order => self.seen >= TRUSTED_AFTER && self.seen > 2 * self.taken_back,
        }
    }

    /// Sir took it back and has not let it stand often enough since: not made again.
    fn refused(&self) -> bool {
        self.taken_back > 0 && self.seen <= 2 * self.taken_back
    }
}

// ------------------------------------------------------------------------- the store ----

struct Store {
    /// The memory database, when what he learns is kept (never in a scripted session).
    db: Option<String>,
    loaded: bool,
    entries: Vec<Learnt>,
}

static STORE: Mutex<Store> = Mutex::new(Store { db: None, loaded: false, entries: Vec::new() });

/// The phrase book holds this many orders at most (the least used goes).
const PHRASES_MOST: usize = 300;

/// Where what he learns is kept. Without it (a scripted session, the tests) he learns for the
/// run only, and nothing of Sir's is read.
pub fn set_db(path: &str) {
    let mut store = lock(&STORE);
    store.db = Some(path.to_string());
    store.loaded = false;
}

fn open(db: &str) -> rusqlite::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS ear_learnt (heard TEXT NOT NULL, kind TEXT NOT NULL, meant TEXT NOT NULL, seen INTEGER NOT NULL, taken_back INTEGER NOT NULL, last TEXT NOT NULL, PRIMARY KEY (heard, kind));")?;
    Ok(conn)
}

fn load(db: &str) -> Vec<Learnt> {
    // (Read only: an older database has no such table yet, and looking never makes one.)
    let Ok(conn) = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Vec::new() };
    let Ok(mut rows) = conn.prepare("SELECT heard, kind, meant, seen, taken_back FROM ear_learnt ORDER BY last") else { return Vec::new() };
    let read = rows.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?)));
    let Ok(read) = read else { return Vec::new() };
    read.flatten().filter_map(|(heard, kind, meant, seen, taken_back)| Some(Learnt { heard, meant, kind: Kind::of(&kind)?, seen: seen.max(0) as u32, taken_back: taken_back.max(0) as u32 })).collect()
}

fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut store = lock(&STORE);
    if !store.loaded {
        store.loaded = true;
        if let Some(db) = store.db.clone() {
            store.entries = load(&db);
        }
    }
    f(&mut store)
}

fn write(store: &Store, entry: &Learnt) {
    let Some(db) = &store.db else { return };
    let done = open(db).and_then(|conn| {
        conn.execute(
            "INSERT INTO ear_learnt (heard, kind, meant, seen, taken_back, last) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(heard, kind) DO UPDATE SET meant = ?3, seen = ?4, taken_back = ?5, last = ?6",
            rusqlite::params![entry.heard, entry.kind.tag(), entry.meant, entry.seen as i64, entry.taken_back as i64, chrono::Utc::now().to_rfc3339()],
        )
    });
    if let Err(e) = done {
        tracing::warn!("[EAR] What was learnt was not written down: {e}");
    }
}

fn erase(store: &Store, heard: Option<(&str, Kind)>) {
    let Some(db) = &store.db else { return };
    let done = open(db).and_then(|conn| match heard {
        Some((heard, kind)) => conn.execute("DELETE FROM ear_learnt WHERE heard = ?1 AND kind = ?2", rusqlite::params![heard, kind.tag()]),
        None => conn.execute("DELETE FROM ear_learnt WHERE kind != 'name'", []),
    });
    if let Err(e) = done {
        tracing::warn!("[EAR] What was to be forgotten is still written down: {e}");
    }
}

/// Counts `heard` → `meant` (`by` sightings; another meaning for the same words starts over),
/// and what it stands at now.
fn count(heard: &str, meant: &str, kind: Kind, by: u32) -> Learnt {
    with(|store| {
        let at = match store.entries.iter().position(|e| e.kind == kind && e.heard == heard) {
            Some(at) => {
                if store.entries[at].meant != meant {
                    store.entries[at] = Learnt { heard: heard.to_string(), meant: meant.to_string(), kind, seen: 0, taken_back: 0 };
                }
                at
            }
            None => {
                store.entries.push(Learnt { heard: heard.to_string(), meant: meant.to_string(), kind, seen: 0, taken_back: 0 });
                store.entries.len() - 1
            }
        };
        store.entries[at].seen += by;
        let entry = store.entries[at].clone();
        write(store, &entry);
        entry
    })
}

/// Everything he has learnt (for the tests, the log and "what have you learnt").
pub fn learnt() -> Vec<Learnt> {
    with(|store| store.entries.clone())
}

/// Forgets what he learnt about Sir's words (his names in Sir's voice stay). How much it was.
pub fn forget_all() -> usize {
    with(|store| {
        let before = store.entries.len();
        store.entries.retain(|e| e.kind == Kind::Name);
        erase(store, None);
        before - store.entries.len()
    })
}

/// Forgets everything his ear has learnt, his names in Sir's voice too (the privacy board).
pub fn forget_everything() -> usize {
    with(|store| {
        let n = store.entries.len();
        store.entries.clear();
        if let Some(db) = &store.db {
            if let Err(e) = open(db).and_then(|conn| conn.execute("DELETE FROM ear_learnt", [])) {
                tracing::warn!("[EAR] What was to be forgotten is still written down: {e}");
            }
        }
        n
    })
}

#[cfg(test)]
fn reset() {
    let mut store = lock(&STORE);
    *store = Store { db: None, loaded: true, entries: Vec::new() };
    *lock(&LAST_MISS) = None;
    *lock(&LAST_REWRITE) = None;
}

// --------------------------------------------------------------------------- the words ----

/// The words of a sentence as they are compared: lower case, no punctuation, apostrophes
/// dropped ("what's" → "whats").
pub fn words(text: &str) -> Vec<String> {
    text.to_lowercase().chars().filter(|c| !matches!(c, '\'' | '’')).map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>().split_whitespace().map(str::to_string).collect()
}

/// A word of the sentence as it was written, and which of its compared words it holds.
struct Tok {
    text: String,
    from: usize,
    to: usize,
}

fn tokens(text: &str) -> (Vec<Tok>, Vec<String>) {
    let mut all = Vec::new();
    let mut toks = Vec::new();
    for raw in text.split_whitespace() {
        let from = all.len();
        all.extend(words(raw));
        toks.push(Tok { text: raw.to_string(), from, to: all.len() });
    }
    (toks, all)
}

/// The sentence with its compared words `from..to` replaced by `with` (the punctuation that
/// closed them stays). None when they do not begin and end with written words.
fn replace(toks: &[Tok], from: usize, to: usize, with: &str) -> Option<String> {
    let first = toks.iter().position(|t| t.from == from && t.to > t.from)?;
    let last = toks.iter().rposition(|t| t.to == to && t.to > t.from)?;
    if last < first {
        return None;
    }
    let closing: String = toks[last].text.chars().rev().take_while(|c| !c.is_alphanumeric()).collect::<String>().chars().rev().collect();
    let mut out: Vec<String> = toks[..first].iter().map(|t| t.text.clone()).collect();
    out.push(format!("{with}{closing}"));
    out.extend(toks[last + 1..].iter().map(|t| t.text.clone()));
    Some(out.join(" "))
}

fn find(hay: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|i| hay[*i..*i + needle.len()] == *needle)
}

/// Words in front of an order that change nothing.
const LEAD: &[&str] = &["hey", "hi", "ok", "okay", "please", "now", "then", "and", "so", "just", "but", "well", "yes", "yeah", "no", "um", "uh", "bitte", "also"];
/// Words after it that change nothing.
const TAIL: &[&str] = &["please", "now", "sir", "thanks", "bitte", "danke"];

/// Where the order itself stands in the words: after his name (or a word that is his name in
/// Sir's voice) and a greeting, before a closing "please".
fn core(w: &[String]) -> (usize, usize) {
    let mut from = 0;
    while from < w.len().min(3) && (LEAD.contains(&w[from].as_str()) || crate::wake::name_word(&w[from])) {
        from += 1;
    }
    // (At the end only his name as it is written: "play Green Day" ends in a word that can
    // stand for his name at the front of a sentence, and is a band.)
    let mut to = w.len();
    while to > from + 1 && (TAIL.contains(&w[to - 1].as_str()) || crate::wake::real_name_word(&w[to - 1])) {
        to -= 1;
    }
    (from, to)
}

/// Whether the words are an order he knows, with or without what stands in front of it.
pub fn leads_somewhere(text: &str, routes: &dyn Fn(&str) -> bool) -> bool {
    if routes(text) {
        return true;
    }
    let (toks, w) = tokens(text);
    let (from, _) = core(&w);
    if from == 0 {
        return false;
    }
    let rest: Vec<&str> = toks.iter().filter(|t| t.from >= from).map(|t| t.text.as_str()).collect();
    !rest.is_empty() && routes(&rest.join(" "))
}

/// Whether words are an order he knows, by any of the routes that need no language model.
pub fn is_order(text: &str) -> bool {
    let t = crate::typos::normalize_user_input_typos(text);
    crate::command_corrector::routes(&t)
        || crate::ears::known_command(&t)
        || crate::intents::mixer_command(&t).is_some()
        || crate::intents::silent_quick_command(&t).is_some()
        || crate::intents::orb_mode_command(&t).is_some()
        || about_his_words(&t)
        || dispatchers_own(&t)
        || crate::brain::another_way(&t).is_some()
}

/// An order the dispatcher carries out itself, before any turn exists (`Jai::admit`): the
/// privacy lock and its lifting ("don't listen", "you can listen again"), also as one clause of
/// a longer sentence. No parser of the fast path knows these, so "Jay, don't listen" counted as
/// words that lead nowhere: it was heard a second time, and waited for like an unfinished
/// thought, before the lock came (2026-11-13).
pub fn dispatchers_own(text: &str) -> bool {
    crate::app::Jai::admit(text) != crate::app::Admission::Turn
}

/// Whether Sir is speaking about his own words: teaching, asking what was learnt, having it
/// forgotten (`command` answers).
pub fn about_his_words(text: &str) -> bool {
    let said = words(text).join(" ");
    TEACH.is_match(&said) || ASKED.is_match(&said) || FORGET.is_match(&said) || CLIPS.is_match(&said)
}

/// Orders that take something away or cannot be taken back: sound alone never leads to one.
const TAKES_AWAY: &[&str] = &["end", "text", "off", "yourself", "everyone", "everybody", "anyone", "aus", "alle", "weg"];
/// The same by the beginning of a word, so that every form of it counts ("closing", "closed",
/// "schließen", "löscht").
const TAKES_AWAY_STEMS: &[&str] = &[
    "clos", "quit", "kill", "exit", "delet", "remov", "shut", "restart", "reboot", "clear", "terminat", "forget", "lock", "sleep", "uninstall", "send", "email", "mail", "buy", "pay", "purchas", "order",
    "stop", "paus", "minimi", "hide", "hid", "eras", "wipe", "reset", "cancel", "disabl", "hibernat", "standby", "power", "log", "sign", "obey", "listen", "learn", "schließ", "schliess",
    "beend", "lösch", "loesch", "stopp", "sperr", "vergiss", "entfern", "kauf", "bezahl", "schick", "send", "herunterfahr", "neustart",
];

fn takes_away(w: &[String]) -> bool {
    w.iter().any(|x| TAKES_AWAY.contains(&x.as_str()) || TAKES_AWAY_STEMS.iter().any(|stem| x.starts_with(stem)))
}


// ------------------------------------------------------------------------ the reading ----

/// How a rewrite came about.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum By {
    /// Sir taught it.
    Taught,
    /// He had learnt it.
    Learnt,
    /// An application's name sounds like the words (how alike).
    Sound(f32),
    /// An order Sir has given before sounds like it (how alike).
    PhraseBook(f32),
}

/// What he takes the words for.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// The words as they were written.
    pub heard: String,
    pub text: String,
    pub by: By,
    /// What was replaced by what.
    pub swaps: Vec<(String, String)>,
}

impl Reading {
    /// The reason, for the log.
    pub fn why(&self) -> String {
        match self.by {
            By::Taught => "Sir taught it".to_string(),
            By::Learnt => "learnt from Sir's own words".to_string(),
            By::Sound(score) => format!("an application of that sound, {score:.2}"),
            By::PhraseBook(score) => format!("an order Sir has given before sounds like it, {score:.2}"),
        }
    }
}

/// A dictation is no order: nothing longer than this is touched.
const MAX_WORDS: usize = 14;
/// By sound, an application's name must be this like the words …
const SOUND_ALIKE: f32 = 0.84;
/// … and this much more like them than any other application's.
const SOUND_MARGIN: f32 = 0.07;
/// An order of the phrase book must sound this like the words, and this much more than the
/// next one; one that takes something away must sound the same.
const PHRASE_ALIKE: f32 = 0.86;
const PHRASE_MARGIN: f32 = 0.05;
const PHRASE_SAME: f32 = 0.96;

/// Verbs that are given an application by name ("open …", "switch to …"). Only these: after
/// "run", "show me", "put" or "set" a noun that sounds like an application is far more often
/// just that noun ("run this code", "show me the brief").
const GIVEN_A_NAME: &[&str] = &["open", "launch", "start", "focus", "switch", "bring", "mute", "unmute", "öffne", "starte"];
/// Words that stand for something Sir points at: a sentence that differs from an order of his
/// only by one of these means that thing, not the order ("show me that" is not "show me the
/// Earth").
const POINTING: &[&str] = &["that", "this", "it", "them", "there", "those", "these", "one", "here", "him", "her", "das", "es", "dies", "dieses", "den", "sie", "ihn"];
/// Words that name nothing.
const HOLLOW: &[&str] = &[
    "the", "my", "your", "a", "an", "to", "up", "me", "everything", "all", "except", "but", "for", "and", "on", "in", "of", "it", "this", "that", "please", "now", "can", "you", "could", "would", "volume",
    "sound", "louder", "quieter", "down", "off", "with", "at", "by", "percent", "window", "app", "application",
];

/// A name he can be given in an order: how Sir says it, how it is written, how it sounds, and
/// what it names (several names can name one thing).
struct Term {
    said: String,
    display: String,
    key: String,
    id: String,
}

/// The applications on this PC by name: the built-in ones with what Sir calls them, and the
/// installed ones.
fn vocabulary() -> Vec<Term> {
    let mut out: Vec<Term> = Vec::new();
    let mut add = |said: &str, display: &str, id: &str| {
        let said = words(said).join(" ");
        let letters = said.chars().filter(|c| c.is_alphabetic()).count();
        let key = sound_key(&said);
        if letters >= 4 && key.len() >= 4 && said.split(' ').count() <= 2 && !out.iter().any(|t| t.said == said) {
            out.push(Term { said, display: display.to_string(), key, id: id.to_string() });
        }
    };
    // The built-in applications that are installed here (all of them while the index of
    // installed programs is still being made: nothing is known yet then; and in tests, which
    // must not hang on what the PC they run on has installed).
    let installed = if cfg!(test) { Vec::new() } else { crate::appindex::names() };
    let indexed = !installed.is_empty();
    for spec in crate::apps::APPS {
        if indexed && !(crate::appindex::is_known(spec.display) || crate::appindex::is_known(spec.id)) {
            continue;
        }
        add(spec.display, spec.display, spec.id);
        for alias in spec.aliases {
            add(alias, spec.display, spec.id);
        }
    }
    for name in installed {
        let id = words(&name).join(" ");
        add(&name, &name, &id);
    }
    out
}

/// 3: the application whose name sounds like the words where a name belongs.
fn by_sound(toks: &[Tok], w: &[String], entries: &[Learnt], routes: &dyn Fn(&str) -> bool) -> Option<Reading> {
    let verb = w.iter().take(6).position(|x| GIVEN_A_NAME.contains(&x.as_str()))?;
    let terms = vocabulary();
    let mut best: Option<(f32, usize, usize, usize)> = None;
    for from in verb + 1..w.len() {
        for len in (1..=3usize).rev() {
            let to = from + len;
            if to > w.len() || HOLLOW.contains(&w[from].as_str()) && len == 1 || HOLLOW.contains(&w[to - 1].as_str()) {
                continue;
            }
            let span = w[from..to].join(" ");
            // (Words that name something already are what they are.)
            if terms.iter().any(|t| t.said == span) || crate::wake::name_word(&span) {
                continue;
            }
            let key = sound_key(&span);
            if key.len() < 4 {
                continue;
            }
            let mut scored: Vec<(f32, usize)> = terms.iter().enumerate().map(|(i, t)| (sound_similarity(&key, &t.key), i)).collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            let Some(&(score, term)) = scored.first() else { continue };
            let rival = scored.iter().skip(1).find(|(_, i)| terms[*i].id != terms[term].id).map_or(0.0, |(s, _)| *s);
            if score >= SOUND_ALIKE && score - rival >= SOUND_MARGIN && best.is_none_or(|b| score > b.0) {
                best = Some((score, from, to, term));
            }
        }
    }
    let (score, from, to, term) = best?;
    let span = w[from..to].join(" ");
    if entries.iter().any(|e| e.kind == Kind::Words && e.heard == span && e.refused()) {
        return None;
    }
    // (An order that takes something away is not led to by sound: he must have learnt it.)
    if takes_away(w) {
        return None;
    }
    let text = replace(toks, from, to, &terms[term].display)?;
    let heard = toks.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
    leads_somewhere(&text, routes).then(|| Reading { heard, text, by: By::Sound(score), swaps: vec![(span, terms[term].said.clone())] })
}

/// 5: the order of Sir's phrase book that the words sound like.
fn by_phrase(toks: &[Tok], w: &[String], entries: &[Learnt], routes: &dyn Fn(&str) -> bool) -> Option<Reading> {
    let (from, to) = core(w);
    if !(1..=7).contains(&(to - from)) {
        return None;
    }
    let heard = w[from..to].join(" ");
    let key = sound_key(&heard);
    if key.len() < 5 {
        return None;
    }
    let mut scored: Vec<(f32, &str)> = entries.iter().filter(|e| e.kind == Kind::Said && e.trusted()).map(|e| e.meant.as_str()).chain(SEEDS.iter().copied()).filter(|p| *p != heard).map(|p| (sound_similarity(&key, &sound_key(p)), p)).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let &(score, phrase) = scored.first()?;
    let rival = scored.iter().skip(1).find(|(_, p)| *p != phrase).map_or(0.0, |(s, _)| *s);
    // Where the words differ from the order only by a word that points at something ("show me
    // that"), Sir means that thing.
    let said = &w[from..to];
    let meant = words(phrase);
    let shorter = said.len().min(meant.len());
    let front = (0..shorter).take_while(|i| said[*i] == meant[*i]).count();
    let back = (0..shorter - front).take_while(|i| said[said.len() - 1 - i] == meant[meant.len() - 1 - i]).count();
    let differing = &said[front..said.len() - back];
    if !differing.is_empty() && differing.iter().all(|word| POINTING.contains(&word.as_str())) {
        return None;
    }
    let needed = if takes_away(&meant) { PHRASE_SAME } else { PHRASE_ALIKE };
    if score < needed || score - rival < PHRASE_MARGIN || entries.iter().any(|e| e.kind == Kind::Order && e.heard == heard && e.refused()) {
        return None;
    }
    let text = replace(toks, from, to, phrase)?;
    let written = toks.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
    leads_somewhere(&text, routes).then(|| Reading { heard: written, text, by: By::PhraseBook(score), swaps: vec![(heard, phrase.to_string())] })
}

/// What he takes `heard` for, when that is something else than was written. `addressed`: the
/// words were spoken to him (his name, an open conversation, his window in front); `routes`:
/// whether words are an order he knows.
pub fn read(heard: &str, addressed: bool, routes: &dyn Fn(&str) -> bool) -> Option<Reading> {
    let (toks, w) = tokens(heard);
    if w.is_empty() || w.len() > MAX_WORDS {
        return None;
    }
    let entries = learnt();
    // 1 and 2: what Sir taught holds as it is; words he has learnt hold where they make an
    // order he knows.
    let mut text = heard.to_string();
    let mut swaps: Vec<(String, String)> = Vec::new();
    let mut by = By::Learnt;
    for entry in entries.iter().filter(|e| matches!(e.kind, Kind::Taught | Kind::Words) && e.trusted()) {
        let (needle, meant) = (words(&entry.heard), words(&entry.meant));
        // (A meaning that holds the words themselves would be found again and again.)
        if find(&meant, &needle).is_some() {
            continue;
        }
        let (toks, w) = tokens(&text);
        let Some(at) = find(&w, &needle) else { continue };
        let Some(next) = replace(&toks, at, at + needle.len(), &entry.meant) else { continue };
        if entry.kind == Kind::Taught || leads_somewhere(&next, routes) {
            if entry.kind == Kind::Taught {
                by = By::Taught;
            }
            swaps.push((entry.heard.clone(), entry.meant.clone()));
            text = next;
        }
    }
    if !swaps.is_empty() {
        return Some(Reading { heard: heard.to_string(), text, by, swaps });
    }
    // 3: by sound, an application where its name belongs.
    if addressed {
        if let Some(reading) = by_sound(&toks, &w, &entries, routes) {
            return Some(reading);
        }
    }
    // From here on only words that lead nowhere as they are.
    if leads_somewhere(heard, routes) {
        return None;
    }
    // 4: a whole order he has learnt.
    let (from, to) = core(&w);
    let order = w[from..to].join(" ");
    if let Some(entry) = entries.iter().find(|e| e.kind == Kind::Order && e.trusted() && e.heard == order) {
        if let Some(text) = replace(&toks, from, to, &entry.meant).filter(|t| leads_somewhere(t, routes)) {
            return Some(Reading { heard: heard.to_string(), text, by: By::Learnt, swaps: vec![(entry.heard.clone(), entry.meant.clone())] });
        }
    }
    // 5: Sir's phrase book, by sound.
    if addressed {
        return by_phrase(&toks, &w, &entries, routes);
    }
    None
}

// ----------------------------------------------------------------------- the learning ----

/// The last order that led nowhere, and when.
struct Miss {
    words: Vec<String>,
    at: Instant,
    unsure: bool,
}

static LAST_MISS: Mutex<Option<Miss>> = Mutex::new(None);
/// The last rewrite that was carried out, and when.
static LAST_REWRITE: Mutex<Option<(Reading, Instant)>> = Mutex::new(None);

/// An order said again counts as the same one within this long …
const SAID_AGAIN_WITHIN: Duration = Duration::from_secs(25);
/// … and "that's not what I said" takes a rewrite back within this long.
const TAKEN_BACK_WITHIN: Duration = Duration::from_secs(40);
/// Two stretches of words are one thing said twice when they sound this alike …
const PAIR_ALIKE: f32 = 0.45;
/// … and count double when they sound this alike.
const PAIR_CLOSE: f32 = 0.7;

/// A rewrite was carried out: remembered for "that's not what I said", and one from the
/// phrase book counted. (An application read by sound is read by sound each time: it becomes
/// no rule of its own, which would hold without his name and for "close …" too.)
pub fn stood(reading: &Reading) {
    match reading.by {
        By::Sound(_) => {}
        By::PhraseBook(_) => {
            for (heard, meant) in &reading.swaps {
                count(heard, meant, Kind::Order, 1);
            }
        }
        By::Learnt | By::Taught => {}
    }
    *lock(&LAST_REWRITE) = Some((reading.clone(), Instant::now()));
}

/// "That's not what I said" a moment after a rewrite: it is taken back (what it was, for the
/// log). What Sir taught is his to take back in words.
pub fn taken_back() -> Option<String> {
    let (reading, at) = lock(&LAST_REWRITE).take()?;
    if at.elapsed() > TAKEN_BACK_WITHIN || reading.by == By::Taught {
        return None;
    }
    // The words that were swapped, and the order as a whole (so that no other way of reading
    // leads to the same thing again).
    let (was, now) = (words(&reading.heard), words(&reading.text));
    let ((wf, wt), (nf, nt)) = (core(&was), core(&now));
    let mut refused: Vec<(String, String, Kind)> = reading.swaps.iter().map(|(heard, meant)| (heard.clone(), meant.clone(), Kind::Words)).collect();
    refused.push((was[wf..wt].join(" "), now[nf..nt].join(" "), Kind::Order));
    with(|store| {
        for (heard, meant, kind) in &refused {
            let found: Vec<usize> = store.entries.iter().enumerate().filter(|(_, e)| matches!(e.kind, Kind::Words | Kind::Order) && e.heard == *heard && e.meant == *meant).map(|(i, _)| i).collect();
            for i in &found {
                store.entries[*i].taken_back += 1;
                let entry = store.entries[*i].clone();
                write(store, &entry);
            }
            if found.is_empty() && !heard.is_empty() && heard != meant {
                let entry = Learnt { heard: heard.clone(), meant: meant.clone(), kind: *kind, seen: 0, taken_back: 1 };
                store.entries.push(entry.clone());
                write(store, &entry);
            }
        }
    });
    Some(reading.swaps.iter().map(|(heard, meant)| format!("'{heard}' is not '{meant}'")).collect::<Vec<_>>().join(", "))
}

/// Two orders, one that led nowhere and one that did: the stretch in which they differ, when
/// that is a few words on either side and (`same_clip`: both are hearings of one clip, or)
/// the two sound alike. Learnt as words for words and as an order for an order; what was
/// paired, for the log.
fn pair(nowhere: &[String], somewhere: &[String], same_clip: bool, unsure: bool) -> Option<(String, String)> {
    if nowhere == somewhere || !(1..=8).contains(&nowhere.len()) || !(1..=8).contains(&somewhere.len()) {
        return None;
    }
    let shorter = nowhere.len().min(somewhere.len());
    let front = (0..shorter).take_while(|i| nowhere[*i] == somewhere[*i]).count();
    let back = (0..shorter - front).take_while(|i| nowhere[nowhere.len() - 1 - i] == somewhere[somewhere.len() - 1 - i]).count();
    let (x, y) = (&nowhere[front..nowhere.len() - back], &somewhere[front..somewhere.len() - back]);
    let hollow = |stretch: &[String]| stretch.iter().all(|word| HOLLOW.contains(&word.as_str()) || POINTING.contains(&word.as_str()));
    if x.is_empty() || y.is_empty() || x.len() > 3 || y.len() > 3 || hollow(x) || hollow(y) {
        return None;
    }
    let (heard, meant) = (x.join(" "), y.join(" "));
    let alike = sound_similarity(&sound_key(&heard), &sound_key(&meant));
    // Two orders said one after the other are one order said twice only when the words in
    // which they differ sound alike ("show me the news", then "show me the weather" are two
    // orders). Two hearings of one clip are one order whatever they sound like.
    let _ = unsure;
    if !(same_clip || alike >= PAIR_ALIKE) {
        return None;
    }
    let by = if alike >= PAIR_CLOSE { 2 } else { 1 };
    // (Words for words only when something stands around them: a whole short order is an order.)
    if front + back > 0 {
        count(&heard, &meant, Kind::Words, by);
    }
    count(&nowhere.join(" "), &somewhere.join(" "), Kind::Order, by);
    Some((nowhere.join(" "), somewhere.join(" ")))
}

/// What a spoken order came to. One that led somewhere joins Sir's phrase book, and when an
/// order that led nowhere was said moments before, the two are paired (what was paired).
/// `unsure`: the recogniser was unsure of the words.
pub fn turn(order: &str, routed: bool, unsure: bool) -> Option<(String, String)> {
    let all = words(order);
    let (from, to) = core(&all);
    let w = all[from..to].to_vec();
    if !routed {
        if (1..=8).contains(&w.len()) {
            *lock(&LAST_MISS) = Some(Miss { words: w, at: Instant::now(), unsure });
        }
        return None;
    }
    let miss = lock(&LAST_MISS).take();
    if (2..=8).contains(&w.len()) && !w.iter().any(|x| x.chars().any(|c| c.is_ascii_digit())) {
        let phrase = w.join(" ");
        count(&phrase, &phrase, Kind::Said, 1);
        with(|store| {
            while store.entries.iter().filter(|e| e.kind == Kind::Said).count() > PHRASES_MOST {
                let Some(least) = store.entries.iter().enumerate().filter(|(_, e)| e.kind == Kind::Said).min_by_key(|(_, e)| e.seen).map(|(i, _)| i) else { break };
                let gone = store.entries.remove(least);
                erase(store, Some((&gone.heard, Kind::Said)));
            }
        });
    }
    let miss = miss.filter(|m| m.at.elapsed() <= SAID_AGAIN_WITHIN)?;
    pair(&miss.words, &w, false, miss.unsure || unsure)
}

/// A second, careful hearing of one clip made other words of it, and those led somewhere: the
/// first hearing is paired with them (he will read it right at once the next time).
pub fn heard_again(first: &str, second: &str) -> Option<(String, String)> {
    let (a, b) = (words(first), words(second));
    let ((af, at), (bf, bt)) = (core(&a), core(&b));
    pair(&a[af..at], &b[bf..bt], true, true)
}

/// A second hearing must still be the same sentence: this like the first by sound.
const SECOND_ALIKE: f32 = 0.5;

/// Whether a second hearing may stand for the first: it sounds like it (a recogniser that
/// was given names to listen for sometimes writes one of them for anything), it takes nothing
/// away, and Sir has not refused this very pair before.
pub fn second_hearing_fits(first: &str, second: &str) -> bool {
    let (a, b) = (words(first), words(second));
    if a.is_empty() || b.is_empty() || takes_away(&b) {
        return false;
    }
    let ((af, at), (bf, bt)) = (core(&a), core(&b));
    let (was, now) = (a[af..at].join(" "), b[bf..bt].join(" "));
    let refused = learnt().iter().any(|e| e.kind == Kind::Order && e.heard == was && e.meant == now && e.refused());
    !refused && sound_similarity(&sound_key(&was), &sound_key(&now)) >= SECOND_ALIKE
}

/// A second hearing was carried out, in Sir's voice: learnt, and remembered for "that's not
/// what I said".
pub fn second_hearing_stood(first: &str, second: &str) {
    heard_again(first, second);
    *lock(&LAST_REWRITE) = Some((Reading { heard: first.to_string(), text: second.to_string(), by: By::Learnt, swaps: Vec::new() }, Instant::now()));
}

/// A spoken sentence was taken as it was written: "that's not what I said" after it is about
/// that sentence, not about a rewrite from before.
pub fn no_rewrite() {
    *lock(&LAST_REWRITE) = None;
}

/// Sir put a line of the hearing log right: what was heard was `meant`. The order is his for
/// it at once, and so are the words in which the two differ.
pub fn corrected(heard: &str, meant: &str) -> bool {
    let (a, b) = (words(heard), words(meant));
    let ((af, at), (bf, bt)) = (core(&a), core(&b));
    let (was, is) = (a[af..at].to_vec(), b[bf..bt].to_vec());
    if was.is_empty() || is.is_empty() || was == is {
        return false;
    }
    // (Sir said so himself: twice over, so that it holds from now on.)
    for _ in 0..2 {
        if pair(&was, &is, true, true).is_none() {
            count(&was.join(" "), &is.join(" "), Kind::Order, 1);
        }
    }
    tracing::info!("[EAR] Sir put a line right: '{}' is '{}'", was.join(" "), is.join(" "));
    true
}

/// Sir teaches him: `heard` is to be taken for `meant`, from now on and everywhere.
pub fn teach(heard: &str, meant: &str) -> bool {
    let (heard, meant) = (words(heard).join(" "), words(meant).join(" "));
    if heard.is_empty() || meant.is_empty() || heard == meant || heard.split(' ').count() > 5 || meant.split(' ').count() > 6 {
        return false;
    }
    count(&heard, &meant, Kind::Taught, 1);
    true
}

/// A word stood where his name stands, in Sir's own voice, in front of an order he knows: it
/// is counted, and true the moment it has come often enough to be a name of his from now on.
pub fn name_heard(word: &str) -> bool {
    let word = words(word).join("");
    !word.is_empty() && count(&word, "jai", Kind::Name, 1).seen == NAME_SIGHTINGS
}

// ------------------------------------------------------------------- what Sir can say ----

static TEACH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:jai\s+)?(?:when|if|whenever)\s+(?:i\s+say|you\s+hear|i\s+tell\s+you)\s+(.+?)\s+(?:i\s+mean|it\s+means|that\s+means|it\s+is|its|thats|that\s+is|take\s+it\s+as|understand)\s+(.+)$|^(?:jai\s+)?wenn\s+(?:ich\s+(.+?)\s+sage|du\s+(.+?)\s+hörst)\s+(?:meine\s+ich|heißt\s+das|ist\s+das)\s+(.+)$").expect("the teaching phrases")
});
static ASKED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:jai\s+)?(?:what\s+(?:have|did)\s+you\s+(?:learnt|learned|learn)\s+(?:about|of|from)|what\s+do\s+you\s+know\s+about)\s+(?:my\s+(?:voice|pronunciation|accent|speech|words)|how\s+i\s+(?:speak|talk|pronounce\s+(?:things|words))|the\s+way\s+i\s+(?:speak|talk))$|^(?:jai\s+)?was\s+hast\s+du\s+über\s+meine\s+(?:stimme|aussprache)\s+gelernt$").expect("the question")
});
static FORGET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:jai\s+)?forget\s+(?:what\s+you\s+(?:have\s+)?(?:learnt|learned)\s+about\s+|everything\s+about\s+)?(?:my\s+(?:pronunciation|accent|words)|how\s+i\s+(?:speak|talk|pronounce\s+(?:things|words)))$|^(?:jai\s+)?vergiss\s+(?:was\s+du\s+über\s+)?meine\s+aussprache(?:\s+gelernt\s+hast)?$").expect("the forgetting phrase")
});
static CLIPS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:jai\s+)?(?:delete|erase|remove|forget)\s+(?:all\s+)?(?:my\s+|the\s+|your\s+)?(?:voice\s+(?:clips|recordings|samples)|recordings\s+of\s+my\s+voice|clips\s+of\s+my\s+voice)$|^(?:jai\s+)?lösche\s+meine\s+(?:sprachaufnahmen|stimmaufnahmen)$").expect("the clips phrase")
});
static INSTEAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:jai\s+)?(?:no\s+|nein\s+)?(?:i\s+(?:said|meant)|what\s+i\s+(?:said|meant)\s+(?:was|is)|ich\s+(?:sagte|meinte)|ich\s+habe\s+gesagt)\s+(.+)$").expect("the correction phrase"));

/// What Sir says about his own words: teaching ("when I say this card I mean Discord"),
/// asking what was learnt, having it forgotten. His answer; None when it is none of these.
pub fn command(text: &str) -> Option<String> {
    let said = words(text).join(" ");
    if let Some(c) = TEACH.captures(&said) {
        let heard = c.get(1).or(c.get(3)).or(c.get(4))?.as_str();
        let meant = c.get(2).or(c.get(5))?.as_str();
        return Some(if teach(heard, meant) {
            tracing::info!("[EAR] Sir taught: '{heard}' is '{meant}'");
            format!("Understood, Sir. When I hear \"{heard}\", I'll take it as \"{meant}\".")
        } else {
            "I could not make a rule of that, Sir. Tell me as: when I say this, I mean that.".to_string()
        });
    }
    if ASKED.is_match(&said) {
        return Some(in_words());
    }
    if FORGET.is_match(&said) {
        let n = forget_all();
        tracing::info!("[EAR] Forgotten on Sir's word: {n} things learnt about his words");
        return Some(if n == 0 { "There was nothing to forget, Sir. I listen with fresh ears as it is.".to_string() } else { "Forgotten, Sir. I listen with fresh ears.".to_string() });
    }
    if CLIPS.is_match(&said) {
        return Some(match crate::voice_clips::delete_all() {
            0 => "I keep no clips of your voice, Sir.".to_string(),
            1 => "Deleted, Sir: the one clip of your voice I had.".to_string(),
            n => format!("Deleted, Sir: all {n} clips of your voice."),
        });
    }
    None
}

/// "No, I said open the mixer" after an order that led nowhere: the order he meant (to be
/// carried out), and the one that led nowhere is paired with it at once.
pub fn meant_instead(text: &str, routes: &dyn Fn(&str) -> bool) -> Option<String> {
    let said = words(text).join(" ");
    let meant = INSTEAD.captures(&said)?.get(1)?.as_str().to_string();
    if !leads_somewhere(&meant, routes) {
        return None;
    }
    let miss = lock(&LAST_MISS).take().filter(|m| m.at.elapsed() <= TAKEN_BACK_WITHIN)?;
    let now = words(&meant);
    let (from, to) = core(&now);
    let nowhere = miss.words.join(" ");
    if miss.words != now[from..to] {
        // (Sir said so himself: it holds at once.)
        count(&nowhere, &now[from..to].join(" "), Kind::Order, TRUSTED_AFTER);
        tracing::info!("[EAR] Sir said what he meant: '{nowhere}' is '{}'", now[from..to].join(" "));
    }
    Some(meant)
}

/// What he has learnt, as he says it.
pub fn in_words() -> String {
    let all = learnt();
    let pairs: Vec<String> = all.iter().filter(|e| matches!(e.kind, Kind::Taught | Kind::Words) && e.trusted()).rev().take(5).map(|e| format!("\"{}\" for \"{}\"", e.heard, e.meant)).collect();
    let names: Vec<String> = all.iter().filter(|e| e.kind == Kind::Name && e.trusted()).map(|e| format!("\"{}\"", capitalised(&e.heard))).collect();
    let phrases = all.iter().filter(|e| e.kind == Kind::Said).count();
    let mut line = String::new();
    if !pairs.is_empty() {
        line.push_str(&format!("I take {}. ", pairs.join(", ")));
    }
    if !names.is_empty() {
        line.push_str(&format!("My name reaches me as {} in your voice, and I answer to it. ", names.join(" and ")));
    }
    if phrases > 0 {
        line.push_str(&format!("I know {phrases} of your orders by the way you put them. "));
    }
    if line.is_empty() {
        "Nothing yet, Sir. I learn from every order you give me.".to_string()
    } else {
        format!("{}That is what I have learnt of your words so far, Sir.", line)
    }
}

fn capitalised(word: &str) -> String {
    let mut c = word.chars();
    c.next().map_or(String::new(), |first| first.to_uppercase().chain(c).collect())
}

// ------------------------------------------------------------------ a second hearing ----

/// The bar a first hearing must clear to stand without a second one, by the recogniser's own
/// numbers: the mean log-probability of its words at least this, and the probability that the
/// clip holds no speech at most that.
pub struct Sure {
    pub logprob_at_least: f32,
    pub no_speech_at_most: f32,
}

/// When the first hearing is sure enough that a second, careful one is not worth its second
/// and a half (Sir, 2026-11-13: quicker when he is sure).
///
/// From Sir's own log: his clear sentences score -0.01 … -0.30 (`stt::BEAM_RETRY_LOGPROB`'s
/// note), and "Jay, don't listen." that evening -0.18 with no-speech 0.11: sure. What the
/// second hearing is for scored "They loud nothing." -0.42 and "Jay Layout Standard." -0.47
/// with no-speech 0.59: unsure, by either number. The line between them is drawn at -0.35,
/// where the brain already draws it for what his ear may learn from a sentence and where
/// `stt::HeardQuality::confident` draws it for a change of Sir's name; and at 0.40 for
/// no-speech, as there. (The language's probability is left out on purpose: Sir's accented
/// English scores low as English even when every word is right, and a second hearing listens
/// in the same language anyway.)
pub const SURE: Sure = Sure { logprob_at_least: -0.35, no_speech_at_most: 0.40 };

/// Whether the recogniser was sure of its first hearing (`SURE`).
pub fn heard_surely(heard: crate::stt::HeardQuality) -> bool {
    heard.avg_logprob >= SURE.logprob_at_least && heard.no_speech <= SURE.no_speech_at_most
}

/// Whether words spoken to him that lead nowhere deserve a second, careful hearing of their
/// clip: an order by its shape, a few words, or words the recogniser was unsure of. Not a
/// question that is plainly one for the language model, never an order he knows (`routes`: the
/// dispatcher's own count, `is_order`), and not what the recogniser was sure of the first time
/// (`heard`: its numbers for the first hearing).
pub fn worth_hearing_again(text: &str, heard: crate::stt::HeardQuality, routes: &dyn Fn(&str) -> bool) -> bool {
    let all = words(text);
    let (from, to) = core(&all);
    let w = &all[from..to];
    if w.is_empty() || w.len() > 10 || leads_somewhere(text, routes) {
        return false;
    }
    // (What it was sure of it would write again: the careful hearing of "Jay, don't listen."
    // took 1.7 s to say "Jay, don't listen.")
    if heard_surely(heard) {
        return false;
    }
    const ASKING: &[&str] = &["what", "whats", "why", "how", "who", "whos", "when", "where", "which", "is", "are", "do", "does", "did", "was", "were", "should", "wie", "was", "warum", "wer", "wann", "wo", "welche"];
    let question = ASKING.contains(&w[0].as_str()) && w.len() > 3;
    // (A request that merely begins with a verb, "tell me a joke about school", is one for the
    // language model: a second hearing of every such sentence would hold the next one up.)
    !question && (w.len() <= 4 || heard.avg_logprob < -0.45)
}

/// The names worth giving the recogniser for a careful hearing: the applications Sir has, and
/// his own views.
pub fn hint() -> String {
    // (Tests take the names below: what this PC has installed is no part of them.)
    let mut names: Vec<&str> = crate::apps::APPS.iter().filter(|spec| !cfg!(test) && (crate::appindex::is_known(spec.display) || crate::appindex::is_known(spec.id))).map(|spec| spec.display).take(8).collect();
    if names.is_empty() {
        names = vec!["Discord", "Spotify", "Firefox", "Steam"];
    }
    // His own names for things (snippets, desk layouts) are words no dictionary has.
    let own: Vec<String> = crate::kit::list("snippet").into_iter().chain(crate::kit::list("layout")).map(|(name, _, _)| name).filter(|n| n.chars().count() >= 3).take(8).collect();
    let own = if own.is_empty() { String::new() } else { format!(", {}", own.join(", ")) };
    format!("{}, Earth, mixer, calendar, radar, settings, zoom in, zoom out{own}.", names.join(", "))
}

/// Orders every phrase book starts with: the everyday ones, as Sir says them.
const SEEDS: &[&str] = &[
    "zoom out",
    "zoom in",
    "show me the earth",
    "close the earth",
    "open the mixer",
    "what time is it",
    "show me my calendar",
    "open your settings",
    "hide your core",
    "show me your core",
    "whats playing",
    "next song",
    "system status",
    "clear the board",
    "show my notifications",
    "mini mode",
    "night mode",
    "open discord",
    "open spotify",
    "volume up",
    "volume down",
    // The same everyday orders as he says them in German.
    "wie spät ist es",
    "nächstes lied",
    "öffne discord",
    "öffne spotify",
    "nachtmodus",
    "was habe ich verpasst",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// One test at a time: they share what was learnt.
    static ONE: Mutex<()> = Mutex::new(());

    /// A router for the tests: a handful of orders, with or without "can you" / "please".
    fn routes(text: &str) -> bool {
        let w = words(text);
        let w: Vec<&str> = w.iter().map(String::as_str).filter(|x| !matches!(*x, "can" | "you" | "please" | "could")).collect();
        matches!(
            w.as_slice(),
            ["open", "discord"] | ["open", "spotify"] | ["open", "the", "mixer"] | ["zoom", "out"] | ["zoom", "in"] | ["show", "me", "the", "earth"] | ["close", "discord"] | ["mute", "everything", "except", "discord"] | ["what", "time", "is", "it"]
                | ["close", "the", "earth"] | ["mute", "everything", "except", _, _]
        )
    }

    #[test]
    fn sirs_own_mishearings_of_2026_11_12_are_read_right() {
        let _one = lock(&ONE);
        reset();
        // "Jay, can you please open this card?" (three times that evening): Discord.
        let reading = read("Jay, can you please open this card?", true, &routes).expect("Discord");
        assert_eq!(reading.text, "Jay, can you please open Discord?");
        assert!(matches!(reading.by, By::Sound(score) if score > 0.9), "{:?}", reading.by);
        assert_eq!(reading.swaps, [("this card".to_string(), "discord".to_string())]);
        // His name as an everyday word in front changes nothing.
        assert_eq!(read("They open this card.", true, &routes).map(|r| r.text).as_deref(), Some("They open Discord."));
        // An order that names it where another name belongs, though the words lead somewhere
        // as they are (to an application that does not exist).
        assert_eq!(read("They mute everything except this card.", true, &routes).map(|r| r.text).as_deref(), Some("They mute everything except Discord."));
        // "Jay, sum out.": the order of the phrase book that sounds the same.
        let reading = read("Jay, sum out.", true, &routes).expect("zoom out");
        assert_eq!(reading.text, "Jay, zoom out.");
        assert!(matches!(reading.by, By::PhraseBook(score) if score > 0.95), "{:?}", reading.by);
        // What is right stays; so do a question, plain talk and words not spoken to him.
        for text in ["Jay, open Discord.", "Jay, what time is it?", "Jay, how was your day?", "I played cards with my friends yesterday.", "Jay, open the door for my friend."] {
            assert_eq!(read(text, true, &routes), None, "{text}");
        }
        assert_eq!(read("Can you open this card?", false, &routes), None, "not spoken to him: sound alone decides nothing");
        // Sound never leads to an order that takes something away.
        assert_eq!(read("Jay, close this card.", true, &routes), None);
    }

    #[test]
    fn a_rewrite_that_stands_twice_is_learnt_and_one_taken_back_is_not_made_again() {
        let _one = lock(&ONE);
        reset();
        let first = read("Jay, open this card.", true, &routes).expect("by sound");
        stood(&first);
        // Not spoken to him: nothing. (Sound is for words spoken to him.)
        assert_eq!(read("open this card", false, &routes), None);
        let second = read("Jay, open this card please.", true, &routes).expect("by sound again");
        stood(&second);
        // An application read by sound is read by sound each time: it becomes no rule of its
        // own, which would hold without his name and for an order that takes something away.
        assert!(!learnt().iter().any(|e| e.kind == Kind::Words), "{:?}", learnt());
        assert_eq!(read("close this card", false, &routes), None);
        assert_eq!(read("I lost this card in the game.", false, &routes), None);
        // What Sir teaches holds everywhere, for "close" too.
        assert!(teach("this card", "Discord"));
        let taught = read("close this card", false, &routes).expect("taught words");
        assert_eq!((taught.text.as_str(), taught.by), ("close discord", By::Taught));
        reset();
        // "That's not what I said" takes the last rewrite back: three times, and it is refused.
        for _ in 0..3 {
            let again = read("Jay, open this card.", true, &routes);
            let Some(again) = again else { break };
            stood(&again);
            assert!(taken_back().is_some_and(|what| what.contains("this card")));
        }
        assert_eq!(read("Jay, open this card.", true, &routes), None, "refused: by sound neither");
        assert_eq!(taken_back(), None, "nothing to take back twice");
    }

    #[test]
    fn an_order_said_again_in_words_that_lead_somewhere_is_paired() {
        let _one = lock(&ONE);
        reset();
        // "Show me the Erth" led nowhere; six seconds later "show me the Earth" does.
        assert_eq!(turn("show me the erth", false, false), None);
        assert_eq!(turn("show me the Earth", true, false), Some(("show me the erth".to_string(), "show me the earth".to_string())));
        // They sound the same: counted double, so it holds at once, as words and as an order.
        let reading = read("Jay, show me the Erth.", true, &routes).expect("learnt");
        assert_eq!((reading.text.as_str(), reading.by), ("Jay, show me the earth.", By::Learnt));
        // "The rough" for "the Earth" (2026-11-12) sounds nothing like it: said one after the
        // other, the two are two orders, sure of the words or not ("show me the news", then
        // "show me the weather" are two as well).
        assert_eq!(turn("show me the rough", false, true), None);
        assert_eq!(turn("show me the earth", true, true), None, "other words: another order, not the same one again");
        assert_eq!(turn("show me the news", false, true), None);
        assert_eq!(turn("show me the weather", true, true), None);
        assert_eq!(read("Jay, show me the rough", true, &routes), None);
        // What pairs them is one clip heard twice: the careful second hearing made "the Earth"
        // of it, and that was carried out. Twice, and it is read right at once.
        assert!(second_hearing_fits("show me the rough", "show me the earth") || sound_similarity(&sound_key("show me the rough"), &sound_key("show me the earth")) < SECOND_ALIKE);
        second_hearing_stood("show me the rough", "show me the earth");
        second_hearing_stood("show me the rough", "show me the earth");
        assert_eq!(read("Jay, show me the rough", true, &routes).map(|r| r.text).as_deref(), Some("Jay, show me the earth"));
        // A second hearing that sounds nothing like the first, or that takes something away,
        // does not stand for it.
        assert!(!second_hearing_fits("what a lovely day it is", "zoom out"));
        assert!(!second_hearing_fits("prost yourself", "close yourself"));
        assert!(second_hearing_fits("sum out", "zoom out"));
        // An order that sounds like one Sir has given before needs none of that: the phrase
        // book reads it right the first time.
        assert!(matches!(read("Jay, show me the erf", true, &routes), Some(Reading { by: By::PhraseBook(_), ref text, .. }) if text == "Jay, show me the earth"));
        // Two orders that have nothing to do with each other are not paired …
        assert_eq!(turn("tell me a joke", false, false), None);
        assert_eq!(turn("open the mixer", true, false), None);
        // … nor one said again long after (the miss is taken once).
        assert_eq!(turn("zoom out", true, false), None);
        // Every order that led somewhere is in the phrase book, with how often it was given.
        let book: Vec<(String, u32)> = learnt().into_iter().filter(|e| e.kind == Kind::Said).map(|e| (e.heard, e.seen)).collect();
        assert_eq!(book, [("show me the earth".to_string(), 2), ("show me the weather".to_string(), 1), ("open the mixer".to_string(), 1), ("zoom out".to_string(), 1)]);
        // A line of the hearing log put right holds at once.
        assert!(corrected("Jay, open the mix up.", "open the mixer"));
        assert_eq!(read("Jay, open the mix up", true, &routes).map(|r| r.text).as_deref(), Some("Jay, open the mixer"));
        assert!(!corrected("open the mixer", "Jay, open the mixer."), "nothing to put right");
        // A second hearing of the same clip is paired whatever it sounds like.
        assert_eq!(heard_again("Jay, prost yourself.", "Jay, close yourself."), Some(("prost yourself".to_string(), "close yourself".to_string())));
    }

    #[test]
    fn sir_teaches_asks_and_has_it_forgotten() {
        let _one = lock(&ONE);
        reset();
        assert!(in_words().starts_with("Nothing yet"));
        let reply = command("When I say this card, I mean Discord.").expect("taught");
        assert!(reply.contains("\"this card\"") && reply.contains("\"discord\""), "{reply}");
        // Taught holds everywhere and at once, his name or not.
        let reading = read("what is new on this card", false, &routes).expect("taught");
        assert_eq!((reading.text.as_str(), reading.by), ("what is new on discord", By::Taught));
        assert!(command("Wenn ich Mixa sage, meine ich Mixer").is_some());
        assert!(learnt().iter().any(|e| e.kind == Kind::Taught && e.heard == "mixa" && e.meant == "mixer"));
        assert!(command("what have you learnt about my pronunciation").is_some_and(|said| said.contains("\"this card\" for \"discord\"")));
        assert!(command("What do you know about how I speak?").is_some());
        // Not teaching: ordinary sentences with the same words.
        for text in ["when I say go, you start the timer", "what did you learn about France", "forget it", "I mean it", "delete my voice mail"] {
            assert_eq!(command(text), None, "{text}");
        }
        // (Asked only: the clips themselves are another test's, `voice_clips`.)
        assert!(about_his_words("Delete my voice clips.") && about_his_words("delete my voice recordings") && about_his_words("lösche meine Sprachaufnahmen"));
        assert!(command("forget how I speak").is_some_and(|said| said.contains("fresh ears")));
        assert!(learnt().is_empty());
        // "No, I said …" after an order that led nowhere: the order, learnt at once.
        assert_eq!(turn("open the mix up", false, false), None);
        assert_eq!(meant_instead("No, I said open the mixer.", &routes).as_deref(), Some("open the mixer"));
        assert_eq!(read("Jay, open the mix up", true, &routes).map(|r| r.text).as_deref(), Some("Jay, open the mixer"));
        assert_eq!(meant_instead("I said I would call her tomorrow", &routes), None, "no order, no miss");
    }

    #[test]
    fn his_name_in_sirs_voice_is_a_name_after_two_sightings() {
        let _one = lock(&ONE);
        reset();
        assert!(!name_heard("They"));
        assert!(name_heard("they"), "the second time");
        assert!(!name_heard("They"), "said once it is learnt");
        assert!(in_words().contains("\"They\""));
        // Forgetting his words leaves his names.
        forget_all();
        assert!(learnt().iter().any(|e| e.kind == Kind::Name));
    }

    #[test]
    fn what_was_learnt_survives_the_database() {
        let _one = lock(&ONE);
        reset();
        let dir = std::env::temp_dir().join(format!("jai-ear-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("ear.db").to_string_lossy().to_string();
        set_db(&db);
        assert!(learnt().is_empty(), "a database without the table is read as nothing learnt");
        assert!(teach("this card", "discord"));
        count("sum out", "zoom out", Kind::Order, 2);
        assert!(name_heard("day") || name_heard("day"));
        // Read again from the file.
        set_db(&db);
        let read_back = learnt();
        assert_eq!(read_back.len(), 3);
        assert!(read_back.iter().any(|e| e.kind == Kind::Taught && e.heard == "this card" && e.meant == "discord"));
        assert!(read_back.iter().any(|e| e.kind == Kind::Order && e.seen == 2 && e.trusted()));
        assert!(read_back.iter().any(|e| e.kind == Kind::Name && e.heard == "day" && e.trusted()));
        assert_eq!(forget_all(), 2);
        set_db(&db);
        assert_eq!(learnt().len(), 1, "the name stays, in the file too");
        reset();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The recogniser's numbers for a first hearing (the language is not asked).
    fn heard(avg_logprob: f32, no_speech: f32) -> crate::stt::HeardQuality {
        crate::stt::HeardQuality { avg_logprob, language_prob: 0.9, no_speech }
    }

    #[test]
    fn a_second_hearing_is_for_orders_that_lead_nowhere() {
        assert!(worth_hearing_again("Jay, sum out.", heard(-0.4, 0.1), &routes), "a few words");
        assert!(!worth_hearing_again("Jay, tell me a joke about my school and my teachers", heard(-0.1, 0.1), &routes), "a request for the model, heard well");
        assert!(worth_hearing_again("Jay, show me dear us in the big window now", heard(-0.62, 0.1), &routes), "unsure");
        assert!(!worth_hearing_again("Jay, open Discord.", heard(-0.1, 0.1), &routes), "it leads somewhere");
        assert!(!worth_hearing_again("Jay, what is the capital of France?", heard(-0.2, 0.1), &routes), "a question for the model");
        assert!(!worth_hearing_again("Jay, I think the film we saw yesterday was better than the book", heard(-0.2, 0.1), &routes), "talk");
        assert!(hint().contains("Discord") && hint().contains("zoom out"));
    }

    /// Sir, 2026-11-13: a careful second hearing only when the first was unsure. The numbers
    /// are his log's of that evening.
    #[test]
    fn a_second_hearing_is_only_for_a_first_one_that_was_unsure() {
        // 18:12:20 "Jay, don't listen." (log-probability -0.18, no-speech 0.11): sure. Whatever
        // the words are, a second hearing would write them again.
        assert!(heard_surely(heard(-0.18, 0.11)));
        assert!(!worth_hearing_again("JAI, don't listen", heard(-0.18, 0.11), &routes), "sure of the words: heard once");
        assert!(!worth_hearing_again("Jay, sum out.", heard(-0.2, 0.05), &routes), "a few words that lead nowhere, but heard surely");
        // 18:09 "Jay Layout Standard." (-0.47, no-speech 0.59) and "They loud nothing." (-0.42):
        // unsure, and still heard twice.
        assert!(!heard_surely(heard(-0.47, 0.59)) && !heard_surely(heard(-0.42, 0.1)));
        assert!(worth_hearing_again("Jay Layout Standard.", heard(-0.47, 0.59), &routes));
        assert!(worth_hearing_again("They loud nothing.", heard(-0.42, 0.1), &routes));
        // Either number alone makes a hearing unsure: words Whisper liked in a clip it took
        // for no speech, and the other way round.
        assert!(worth_hearing_again("Jay, sum out.", heard(-0.2, 0.59), &routes));
        assert!(worth_hearing_again("Jay, sum out.", heard(-0.36, 0.05), &routes));
        assert!(!worth_hearing_again("Jay, sum out.", heard(-0.35, 0.40), &routes), "the bar itself is sure");
        // A longer sentence is heard again only when it was very unsure, as before.
        assert!(!worth_hearing_again("Jay, show me dear us in the big window now", heard(-0.40, 0.1), &routes));
        assert!(worth_hearing_again("Jay, show me dear us in the big window now", heard(-0.46, 0.1), &routes));
    }

    /// The dispatcher's own orders are orders he knows: never heard twice, however unsure,
    /// and never rewritten by sound. (The real router.)
    #[test]
    fn what_the_dispatcher_carries_out_is_an_order_he_knows() {
        let _one = lock(&ONE);
        reset();
        let unsure = heard(-0.7, 0.5);
        // The privacy lock and its lifting, and the stop that silences everything.
        for order in ["JAI, don't listen", "Jay, stop listening.", "Stop listening for ten minutes", "You can listen again", "JAI, hör zehn Minuten nicht zu", "Mic off for 20 minutes", "Jay, stop.", "Jay, pause."] {
            assert!(leads_somewhere(order, &is_order), "{order}");
            assert!(!worth_hearing_again(order, unsure, &is_order), "{order}: an order he knows is heard once");
            assert_eq!(read(order, true, &is_order), None, "{order}: taken as it was said");
        }
        // The lock as one clause of a longer sentence is the dispatcher's too.
        assert!(is_order("Stop listening and open the mixer"));
        assert!(dispatchers_own("don't listen") && dispatchers_own("you can listen again") && !dispatchers_own("open the mixer") && !dispatchers_own("what a lovely evening"));
        // Words that lead nowhere are still heard again when the recogniser was unsure of them,
        // and only then.
        assert!(worth_hearing_again("Jay, flurb the wozzle.", unsure, &is_order));
        assert!(!worth_hearing_again("Jay, flurb the wozzle.", heard(-0.18, 0.11), &is_order));
        // A question about the lock is no order of the lock's (it holds "listen again").
        assert!(!dispatchers_own("How long until you listen again?"));
    }

    /// The real router: every order a phrase book starts with leads somewhere, and Sir's
    /// mishearings come right through it too.
    #[test]
    fn the_seeds_are_orders_he_knows() {
        let _one = lock(&ONE);
        reset();
        let nowhere: Vec<&str> = SEEDS.iter().copied().filter(|seed| !is_order(seed)).collect();
        assert!(nowhere.is_empty(), "no order he knows: {nowhere:?}");
        assert_eq!(read("Jay, sum out.", true, &is_order).map(|r| r.text).as_deref(), Some("Jay, zoom out."));
        assert_eq!(read("Jay, can you open this card?", true, &is_order).map(|r| r.text).as_deref(), Some("Jay, can you open Discord?"));
        assert_eq!(read("Jay, what time is it?", true, &is_order), None);
        assert!(is_order("when I say this card I mean Discord"), "teaching is an order of his");
    }
}
