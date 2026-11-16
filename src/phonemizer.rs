//! Bilingual (British RP English + German) grapheme-to-phoneme conversion for Kokoro.
//!
//! Kokoro's voice identity lives entirely in its style vector; the pronunciation comes from the
//! IPA it is given. So one voice (the Fable/George blend) speaks German correctly when German
//! words arrive as German IPA: every phrase is split into English and German runs
//! (`language_runs`: umlauts, German function words, school/gym/calendar vocabulary, German
//! morphology, whole German sentences), each run is phonemised with its own eSpeak-NG voice
//! (`en-gb-x-rp` / `de`) and mapped into Kokoro's shared symbol table (`postprocess_en` /
//! `postprocess_de`), and the runs are joined into one IPA string for the same voice.
//!
//! Backends, fastest/best first:
//! 1. `libespeak-ng` loaded in-process (≈1 ms per clause instead of ≈100 ms per spawned
//!    `espeak-ng.exe`), punctuation preserved for Kokoro's prosody;
//! 2. the `espeak-ng` CLI;
//! 3. a small built-in lexicon + spelling rules (last resort, English rules only).

use regex::Regex;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::LazyLock;
use tracing::{info, warn};

const VOICE: &str = "en-gb-x-rp";
const GERMAN_VOICE: &str = "de";
const ESPEAK_CHARS_UTF8: c_int = 1;
/// IPA with '^' tying multi-letter phonemes ("a^ɪ"): bit 7 plus the tie in bits 8-23.
const ESPEAK_TIED_IPA: c_int = 0x02 | 0x80 | ('^' as c_int) << 8;
const AUDIO_OUTPUT_SYNCHRONOUS: c_int = 0x02;
const ESPEAK_INITIALIZE_DONT_EXIT: c_int = 0x8000;

type FnInitialize = unsafe extern "C" fn(c_int, c_int, *const c_char, c_int) -> c_int;
type FnSetVoiceByName = unsafe extern "C" fn(*const c_char) -> c_int;
type FnTextToPhonemes = unsafe extern "C" fn(*mut *const c_void, c_int, c_int) -> *const c_char;

/// The language of a run of words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    De,
}

/// The eSpeak voice a run is read with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Espeak {
    Rp = 0,
    German = 1,
}

impl Espeak {
    fn of(lang: Lang) -> Self {
        match lang {
            Lang::De => Self::German,
            Lang::En => Self::Rp,
        }
    }

    /// The voice's names, best first.
    fn names(self) -> &'static [&'static str] {
        match self {
            Self::Rp => &[VOICE, "en-gb", "en"],
            Self::German => &[GERMAN_VOICE],
        }
    }
}

struct EspeakLib {
    _lib: libloading::Library,
    set_voice: FnSetVoiceByName,
    text_to_phonemes: FnTextToPhonemes,
    /// The voice eSpeak currently has loaded (`Espeak as u8`).
    current: AtomicU8,
    /// eSpeak has no German voice here: German runs are read with English rules.
    german_missing: std::sync::atomic::AtomicBool,
}

impl EspeakLib {
    fn load(path: &Path, data_dir: Option<&Path>) -> Result<Self, String> {
        // SAFETY: loading a well-known C library; symbols are checked before use and the
        // library handle lives as long as the function pointers (stored together).
        unsafe {
            // Resolve the DLL's own dependencies from its install folder.
            #[cfg(windows)]
            let lib: libloading::Library = libloading::os::windows::Library::load_with_flags(
                path,
                libloading::os::windows::LOAD_WITH_ALTERED_SEARCH_PATH,
            )
            .map_err(|e| e.to_string())?
            .into();
            #[cfg(not(windows))]
            let lib = libloading::Library::new(path).map_err(|e| e.to_string())?;
            let init: FnInitialize = *lib.get::<FnInitialize>(b"espeak_Initialize\0").map_err(|e| e.to_string())?;
            let set_voice: FnSetVoiceByName =
                *lib.get::<FnSetVoiceByName>(b"espeak_SetVoiceByName\0").map_err(|e| e.to_string())?;
            let text_to_phonemes: FnTextToPhonemes =
                *lib.get::<FnTextToPhonemes>(b"espeak_TextToPhonemes\0").map_err(|e| e.to_string())?;

            let data = data_dir.and_then(|d| CString::new(d.to_string_lossy().as_bytes()).ok());
            let rate = init(
                AUDIO_OUTPUT_SYNCHRONOUS,
                0,
                data.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
                ESPEAK_INITIALIZE_DONT_EXIT,
            );
            if rate <= 0 {
                return Err(format!("espeak_Initialize failed ({rate})"));
            }
            let voice_ok = [VOICE, "en-gb", "en"].iter().any(|v| {
                CString::new(*v).map(|c| set_voice(c.as_ptr()) == 0).unwrap_or(false)
            });
            if !voice_ok {
                return Err("no English voice found in espeak-ng-data".into());
            }
            Ok(Self { _lib: lib, set_voice, text_to_phonemes, current: AtomicU8::new(Espeak::Rp as u8), german_missing: std::sync::atomic::AtomicBool::new(false) })
        }
    }

    /// Loads `voice` (only when it changes: switching reloads a dictionary). False when it is
    /// not installed: eSpeak then keeps the voice it had (a German run is read with English
    /// rules).
    fn select(&self, voice: Espeak) -> bool {
        if voice == Espeak::German && self.german_missing.load(Ordering::Acquire) {
            return false;
        }
        if self.current.load(Ordering::Acquire) == voice as u8 {
            return true;
        }
        // SAFETY: the function pointer belongs to the library held by `self`.
        let ok = voice.names().iter().any(|v| CString::new(*v).map(|c| unsafe { (self.set_voice)(c.as_ptr()) } == 0).unwrap_or(false));
        if ok {
            self.current.store(voice as u8, Ordering::Release);
        } else if voice == Espeak::German {
            warn!("[TTS] eSpeak-NG has no German voice; German words are read with English rules.");
            self.german_missing.store(true, Ordering::Release);
        }
        ok
    }

    fn phonemize(&self, text: &str) -> String {
        let Ok(c_text) = CString::new(text.replace('\0', " ")) else {
            return String::new();
        };
        let mut ptr: *const c_void = c_text.as_ptr() as *const c_void;
        let mut out = String::new();
        // SAFETY: `ptr` points into `c_text` (alive for the loop); espeak advances it and sets
        // it to NULL at the end. The returned buffer is copied immediately.
        unsafe {
            let mut guard = 0;
            while !ptr.is_null() && guard < 64 {
                guard += 1;
                let res = (self.text_to_phonemes)(&mut ptr, ESPEAK_CHARS_UTF8, ESPEAK_TIED_IPA);
                if res.is_null() {
                    break;
                }
                let chunk = CStr::from_ptr(res).to_string_lossy();
                let chunk = chunk.trim();
                if !chunk.is_empty() {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(chunk);
                }
            }
        }
        out
    }
}

enum Backend {
    Library(EspeakLib),
    Cli(PathBuf),
    Rules,
}

pub struct Phonemizer {
    backend: Backend,
}

impl Phonemizer {
    pub fn new(override_path: Option<&str>) -> Self {
        let mut libs: Vec<PathBuf> = Vec::new();
        let mut clis: Vec<PathBuf> = Vec::new();
        let lib_name = if cfg!(windows) { "libespeak-ng.dll" } else { "libespeak-ng.so.1" };
        let cli_name = if cfg!(windows) { "espeak-ng.exe" } else { "espeak-ng" };
        if let Some(p) = override_path.map(PathBuf::from) {
            if p.is_dir() {
                libs.push(p.join(lib_name));
                clis.push(p.join(cli_name));
            } else {
                libs.push(p);
            }
        }
        // The copy shipped with JAI, then system installs. Absolute, because the Windows
        // loader resolves the library's own dependencies relative to its full path.
        let bundled = std::path::absolute(Path::new("runtime").join("espeak-ng")).unwrap_or_default();
        if bundled.is_dir() {
            libs.push(bundled.join(lib_name));
            clis.push(bundled.join(cli_name));
        }
        if cfg!(windows) {
            for var in ["ProgramFiles", "ProgramFiles(x86)"] {
                if let Ok(base) = std::env::var(var) {
                    let dir = PathBuf::from(base).join("eSpeak NG");
                    libs.push(dir.join("libespeak-ng.dll"));
                    clis.push(dir.join("espeak-ng.exe"));
                }
            }
        } else {
            for lib in [
                "libespeak-ng.so.1",
                "libespeak-ng.so",
                "/usr/lib/x86_64-linux-gnu/libespeak-ng.so.1",
                "/usr/lib/aarch64-linux-gnu/libespeak-ng.so.1",
                "/usr/lib/libespeak-ng.so.1",
            ] {
                libs.push(PathBuf::from(lib));
            }
        }
        if let Some(cli) = crate::apps::which("espeak-ng") {
            clis.push(cli);
        }

        for lib in &libs {
            let is_bare_name = lib.components().count() == 1;
            if !is_bare_name && !lib.exists() {
                continue;
            }
            // On Windows espeak needs the directory containing `espeak-ng-data`.
            let data_dir = lib.parent().filter(|d| d.join("espeak-ng-data").is_dir());
            match EspeakLib::load(lib, data_dir) {
                Ok(l) => {
                    info!("[TTS] eSpeak-NG library loaded in-process: {}", lib.display());
                    return Self { backend: Backend::Library(l) };
                }
                Err(e) => tracing::debug!("[TTS] eSpeak-NG library {} unusable: {e}", lib.display()),
            }
        }
        if let Some(cli) = clis.into_iter().find(|c| c.exists()) {
            warn!("[TTS] Using the eSpeak-NG CLI ({}); adds ≈100 ms per clause.", cli.display());
            return Self { backend: Backend::Cli(cli) };
        }
        warn!(
            "[TTS] eSpeak-NG not found — using the basic built-in phonemizer (lower quality). Install eSpeak NG ({}).",
            if cfg!(windows) { "https://github.com/espeak-ng/espeak-ng/releases" } else { "sudo apt install espeak-ng" }
        );
        Self { backend: Backend::Rules }
    }

    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            Backend::Library(_) => "espeak-ng (library)",
            Backend::Cli(_) => "espeak-ng (cli)",
            Backend::Rules => "built-in rules",
        }
    }

    /// IPA for a (normalised) sentence in English, German or both, with punctuation kept as
    /// prosody markers: every English and German run in its own language's pronunciation,
    /// all for the same Kokoro voice.
    pub fn phonemize(&self, text: &str) -> String {
        let phrases = split_phrases(text);
        // A whole German sentence stays German across its commas.
        let german_sentence = sentence_is_german(text);
        let mut out = String::new();
        for (phrase, punct) in phrases {
            let mut ipa = String::new();
            if !phrase.trim().is_empty() {
                let runs = if german_sentence { vec![(Lang::De, phrase.clone())] } else { language_runs(&phrase) };
                for (lang, run) in runs {
                    let part = self.phonemize_run(&run, lang);
                    if part.is_empty() {
                        continue;
                    }
                    if !ipa.is_empty() {
                        ipa.push(' ');
                    }
                    ipa.push_str(&part);
                }
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&ipa);
            out.push_str(&punct);
        }
        tidy(&out)
    }

    /// One single-language run, mapped into Kokoro's symbols.
    fn phonemize_run(&self, text: &str, lang: Lang) -> String {
        match &self.backend {
            Backend::Library(lib) => {
                let german = lang == Lang::De && lib.select(Espeak::German);
                if !german {
                    lib.select(Espeak::Rp);
                }
                let raw = lib.phonemize(text);
                if german { postprocess_de(&raw) } else { postprocess_en(&raw) }
            }
            Backend::Cli(cli) => match cli_phonemize(cli, text, Espeak::of(lang)) {
                Some(raw) if lang == Lang::De => postprocess_de(raw.trim_end_matches(['.', '!', '?'])),
                Some(raw) => postprocess_en(raw.trim_end_matches(['.', '!', '?'])),
                None => rules_en(text),
            },
            Backend::Rules => rules_en(text),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Language runs
// ---------------------------------------------------------------------------------------------

/// Unambiguous German function words (none of them is an English word).
const GERMAN_STRONG: &[&str] = &[
    "und", "ich", "du", "nicht", "ist", "ein", "eine", "einen", "einem", "einer", "eines", "zu", "mit", "für", "fuer", "auf", "dem", "des", "sind", "bist",
    "habe", "haben", "hast", "wird", "werden", "wurde", "kannst", "können", "koennen", "muss", "musst", "müssen", "soll", "sollst", "bitte", "danke",
    "heute", "morgen", "gestern", "übermorgen", "jetzt", "noch", "schon", "auch", "aber", "oder", "wenn", "dass", "wie", "warum", "wer", "wo", "mein",
    "meine", "meinen", "meinem", "dein", "deine", "deinen", "uhr", "sehr", "nur", "doch", "nein", "hier", "dann", "sie", "wir", "ihr", "es",
    "viel", "keine", "kein", "mich", "dich", "mir", "sich", "uns", "euch", "sein", "seine", "ihre", "unser", "über", "ueber", "schön", "gerade",
    "immer", "nie", "etwas", "nichts", "alles", "vom", "zum", "zur", "beim", "aus", "bei", "nach", "vor", "seit", "ohne", "gegen",
    "durch", "wieder", "mehr", "weniger", "ganz", "gibt", "geht", "mache", "machen", "gehen", "kommen", "sagen", "der", "das", "diese",
    "dieser", "dieses", "nächste", "nächsten", "naechste", "welche", "welcher", "hallo", "tschüss", "guten", "abend", "nacht", "woche",
];
/// German words that are also English (they count only beside unambiguous German).
const GERMAN_WEAK: &[&str] = &[
    "die", "was", "will", "so", "also", "in", "am", "an", "man", "war", "bin", "den", "um", "da", "ja", "hand", "kind", "rat", "hat", "gut", "bald", "ins",
    "dir", "mach",
];
/// German content words JAI says inside English sentences (school, gym, calendar, people).
const GERMAN_TERMS: &[&str] = &[
    "mathematik", "mathe", "geschichte", "klausur", "klausuren", "probeklausur", "klassenarbeit", "hausaufgaben", "hausaufgabe", "stundenplan",
    "vertretung", "vertretungsplan", "freistunde", "entfall", "ausfall", "unterricht", "leistungskurs", "grundkurs", "lk", "gk", "physik", "chemie",
    "biologie", "erdkunde", "geographie", "informatik", "kunst", "musik", "deutsch", "englisch", "französisch", "franzoesisch", "spanisch", "latein",
    "ethik", "politik", "wirtschaft", "philosophie", "pädagogik", "sozialkunde", "beine", "arme", "brust", "rücken", "schultern", "bauch", "uhr",
    "herr", "frau", "straße", "strasse", "termin", "termine", "zahnarzt", "arzt", "schule", "lehrer", "lehrerin", "klasse", "raum", "kalender",
    "geburtstag", "abitur", "abi", "stunde", "stunden", "minuten", "mittagspause", "sporthalle", "turnhalle",
];

fn word_key(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

fn has_umlaut(word: &str) -> bool {
    word.chars().any(|c| matches!(c, 'ä' | 'ö' | 'ü' | 'Ä' | 'Ö' | 'Ü' | 'ß'))
}

/// A word that is German inside an English sentence: umlauts, JAI's German vocabulary
/// (compounds included: "Deutschprobeklausur"), unambiguous function words, German noun
/// morphology ("Vertretung", "Gesundheit") and German names ("Schmidt", "Schneider").
fn is_german_word(word: &str) -> bool {
    let key = word_key(word);
    if key.chars().count() < 2 || key.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if has_umlaut(&key) {
        return true;
    }
    let bare = word.trim_matches(|c: char| !c.is_alphanumeric());
    // "MIT", "DER": an acronym, not a German word (the school's "LK" / "GK" are listed terms).
    if bare.chars().count() >= 2 && bare.chars().all(|c| !c.is_alphabetic() || c.is_uppercase()) {
        return matches!(key.as_str(), "lk" | "gk");
    }
    if GERMAN_TERMS.contains(&key.as_str()) || GERMAN_STRONG.contains(&key.as_str()) || crate::pronunciation_lexicon::is_german_term(&key) {
        return true;
    }
    let capital = bare.chars().next().is_some_and(char::is_uppercase);
    let compound = ["klausur", "arbeit", "unterricht", "stunde", "plan", "kurs", "termin", "schule", "lehrer"].iter().any(|tail| key.len() > tail.len() + 3 && key.ends_with(tail));
    let morphology = capital && key.chars().count() >= 7 && ["ung", "heit", "keit", "schaft"].iter().any(|s| key.ends_with(s));
    let name = capital && (key.starts_with("sch") && key.chars().nth(3).is_some_and(|c| "lmnrw".contains(c)) || key.ends_with("mann") && key.len() > 5);
    // German towns by their endings ("Musterheim", "Offenbach", "Tübingen", "Neudorf").
    let town = capital && key.chars().count() >= 7 && ["heim", "bach", "ingen", "hausen", "dorf", "burg"].iter().any(|s| key.ends_with(s));
    (compound && capital) || morphology || name || town
}

/// A whole German sentence (or reply): at least two unambiguous German words, or one with
/// enough ambiguous ones around it that they make up most of the sentence.
pub fn sentence_is_german(text: &str) -> bool {
    let words: Vec<String> = text.split_whitespace().map(word_key).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return false;
    }
    let strong = words.iter().filter(|w| GERMAN_STRONG.contains(&w.as_str()) || has_umlaut(w)).count();
    let weak = words.iter().filter(|w| GERMAN_WEAK.contains(&w.as_str())).count();
    let terms = words.iter().filter(|w| GERMAN_TERMS.contains(&w.as_str())).count();
    let share = (strong + weak + terms) as f32 / words.len() as f32;
    (strong >= 2 && share >= 0.3) || (strong >= 1 && share >= 0.5 && words.len() >= 3)
}

/// The phrase as runs of consecutive English and German words.
pub fn language_runs(phrase: &str) -> Vec<(Lang, String)> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    let mut tags: Vec<Lang> = words.iter().map(|w| if is_german_word(w) { Lang::De } else { Lang::En }).collect();
    // "LK Mathematik", "Herr Müller", "um 16 Uhr": a number or an acronym between German words
    // belongs to them.
    for i in 0..words.len() {
        let numeric = words[i].chars().all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | ','));
        let between = i > 0 && tags[i - 1] == Lang::De && tags.get(i + 1) == Some(&Lang::De);
        let before_german = tags.get(i + 1) == Some(&Lang::De) && words[i].chars().all(|c| c.is_ascii_uppercase()) && words[i].len() <= 3;
        if tags[i] == Lang::En && ((numeric && between) || before_german) {
            tags[i] = Lang::De;
        }
    }
    let mut runs: Vec<(Lang, String)> = Vec::new();
    for (word, lang) in words.iter().zip(tags) {
        match runs.last_mut() {
            Some((last, text)) if *last == lang => {
                text.push(' ');
                text.push_str(word);
            }
            _ => runs.push((lang, word.to_string())),
        }
    }
    runs
}

/// German words for the English-only fallbacks: spelled the way an English voice says them
/// ("Müller" → "Mueller", "Schmidt" → "Shmidt").
fn english_spelling(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            let lower = word.to_lowercase();
            let sch = lower.ends_with("sch") || lower.starts_with("sch") && lower.chars().nth(3).is_some_and(|c| "lmnrwt".contains(c));
            if !has_umlaut(word) && !sch {
                return word.to_string();
            }
            let mut out = String::with_capacity(word.len() + 4);
            for c in word.chars() {
                match c {
                    'ä' => out.push_str("eh"),
                    'Ä' => out.push_str("Eh"),
                    'ö' => out.push_str("er"),
                    'Ö' => out.push_str("Er"),
                    'ü' => out.push_str("ue"),
                    'Ü' => out.push_str("Ue"),
                    'ß' => out.push_str("ss"),
                    other => out.push(other),
                }
            }
            out.replace("Sch", "Sh").replace("sch", "sh")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits on prosodic punctuation (keeping it), but not inside numbers ("3.5", "12:30", "1,000").
fn split_phrases(text: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let is_punct = matches!(c, ',' | '.' | '!' | '?' | ';' | ':' | '—' | '…');
        let in_number = matches!(c, '.' | ',' | ':')
            && i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
        if is_punct && !in_number {
            out.push((cur.trim().to_string(), c.to_string()));
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        out.push((cur.trim().to_string(), String::new()));
    }
    out
}

fn cli_phonemize(cli: &Path, text: &str, voice: Espeak) -> Option<String> {
    let mut args: Vec<String> = Vec::new();
    // A bundled CLI must read its own data, not that of a (possibly removed) system install.
    if let Some(dir) = cli.parent().filter(|d| d.join("espeak-ng-data").is_dir()) {
        args.push(format!("--path={}", dir.display()));
    }
    args.extend(["-q".into(), "--ipa".into(), "--tie=^".into(), "-v".into(), voice.names()[0].into(), text.into()]);
    let out = crate::shell::run_helper_blocking(cli, &args, std::time::Duration::from_secs(10)).ok()?;
    if !out.success() {
        return None;
    }
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    let end = text.trim().chars().last().filter(|c| matches!(c, '.' | '!' | '?')).unwrap_or('.');
    Some(format!("{}{end}", lines.join(", ")))
}

static LANG_SWITCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\([a-z]{2,3}(-[a-z0-9]+)*\)").expect("regex"));
static Z_JOIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#" z([;:,.!?—…" ]|$)"#).expect("regex"));
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("regex"));
static SPACE_BEFORE_PUNCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+([,.!?;:—…])").expect("regex"));

static SYLLABIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\S)\x{329}").expect("regex"));

/// eSpeak's multi-letter phonemes and the single letters Kokoro v1.0 learned for them (misaki).
/// The affricate ts (last) is German only: English "its" stays two sounds.
const MERGED: [(char, char, char); 8] = [('e', 'ɪ', 'A'), ('a', 'ɪ', 'I'), ('a', 'ʊ', 'W'), ('ɔ', 'ɪ', 'Y'), ('ə', 'ʊ', 'Q'), ('t', 'ʃ', 'ʧ'), ('d', 'ʒ', 'ʤ'), ('t', 's', 'ʦ')];

/// Multi-letter phonemes joined by `tie` as their single letters.
fn merge(ipa: &str, tie: &str, letters: &[(char, char, char)]) -> String {
    letters.iter().fold(ipa.to_string(), |s, (a, b, one)| s.replace(&format!("{a}{tie}{b}"), &one.to_string()))
}

/// Kokoro v0.19's two-letter spellings back from v1.0's single letters.
pub fn unmerge(ipa: &str) -> String {
    let mut out = String::with_capacity(ipa.len() + 8);
    for c in ipa.chars() {
        match MERGED.iter().find(|m| m.2 == c) {
            Some((a, b, _)) => out.extend([*a, *b]),
            None => out.push(match c { 'ᵊ' => 'ə', 'T' => 'ɾ', _ => c }),
        }
    }
    out
}

/// The built-in rules spell diphthongs untied.
fn rules_en(text: &str) -> String {
    postprocess_en(&merge(&rules_phonemize(&english_spelling(text)), "", &MERGED[..7]))
}

/// English runs into misaki's British notation, the one Kokoro v1.0 was trained on (its eSpeak
/// fallback): tied diphthongs and affricates as single letters, RP's "air" as ɛː, syllabic
/// consonants with ᵊ, glottal stops and flaps as t and T.
fn postprocess_en(ipa: &str) -> String {
    let s = LANG_SWITCH.replace_all(ipa, "");
    let s = s.replace("e^ə", "ɛː").replace("ʔˌn\u{329}", "ʔn").replace("ʔn\u{329}", "ʔn").replace("ə^l", "ᵊl");
    let s = merge(&s, "^", &MERGED[..7]).replace("ʲo", "jo").replace("ʲə", "jə").replace('ɚ', "əɹ").replace("iə", "ɪə");
    let s: String = SYLLABIC
        .replace_all(&s, "ᵊ$1")
        .chars()
        .filter_map(|c| match c {
            '^' | 'ʲ' | '\u{303}' | '\u{329}' => None,
            'e' => Some('A'),
            'r' => Some('ɹ'),
            'x' | 'ç' => Some('k'),
            'ɐ' => Some('ə'),
            'ɬ' => Some('l'),
            'g' => Some('ɡ'),
            'o' => Some('ɔ'),
            'ɾ' => Some('T'),
            'ʔ' => Some('t'),
            _ => Some(c),
        })
        .collect();
    let s = Z_JOIN.replace_all(&s, "z$1");
    tidy(&s)
}

/// German runs into Kokoro's shared symbol table: the uvular r (ʁ) instead of the English
/// approximant, "ach"/"ich" (x, ç) kept, ʏ (absent from the table) as its nearest vowel y, and
/// non-syllabic marks dropped.
fn postprocess_de(ipa: &str) -> String {
    let s = merge(&LANG_SWITCH.replace_all(ipa, ""), "^", &MERGED);
    let s: String = s
        .chars()
        .filter(|c| !matches!(c, '^' | '\u{32F}' | '\u{361}' | '\u{35C}'))
        .map(|c| match c {
            'r' => 'ʁ',
            'ʏ' => 'y',
            'ʲ' => 'j',
            'ɬ' => 'l',
            'g' => 'ɡ',
            _ => c,
        })
        .collect();
    tidy(&s)
}

/// Single spaces, none before punctuation.
fn tidy(ipa: &str) -> String {
    let s = SPACE_BEFORE_PUNCT.replace_all(ipa, "$1");
    SPACES.replace_all(s.trim(), " ").into_owned()
}

// ---------------------------------------------------------------------------------------------
// Built-in fallback (only used without eSpeak-NG): small RP lexicon + spelling rules.
// ---------------------------------------------------------------------------------------------

static LEXICON: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        ("jai", "dʒˈɑːvɪs"), ("sir", "sˈɜː"), ("yes", "jˈɛs"), ("no", "nˈəʊ"), ("hello", "həlˈəʊ"),
        ("good", "ɡˈʊd"), ("morning", "mˈɔːnɪŋ"), ("afternoon", "ˌɑːftənˈuːn"), ("evening", "ˈiːvnɪŋ"),
        ("day", "dˈeɪ"), ("night", "nˈaɪt"), ("now", "nˈaʊ"), ("right", "ɹˈaɪt"), ("away", "əwˈeɪ"),
        ("certainly", "sˈɜːtənli"), ("course", "kˈɔːs"), ("indeed", "ɪndˈiːd"), ("very", "vˈɛɹi"),
        ("well", "wˈɛl"), ("thank", "θˈæŋk"), ("you", "jˈuː"), ("your", "jˈɔː"), ("i", "ˈaɪ"),
        ("am", "æm"), ("are", "ɑː"), ("is", "ɪz"), ("was", "wɒz"), ("be", "bˈiː"), ("have", "hˈæv"),
        ("has", "hˈæz"), ("do", "dˈuː"), ("done", "dˈʌn"), ("will", "wˈɪl"), ("shall", "ʃˈæl"),
        ("would", "wˈʊd"), ("should", "ʃˈʊd"), ("can", "kˈæn"), ("could", "kˈʊd"), ("the", "ðə"),
        ("a", "ə"), ("an", "æn"), ("to", "tə"), ("of", "ɒv"), ("in", "ɪn"), ("on", "ɒn"), ("at", "æt"),
        ("for", "fɔː"), ("with", "wɪð"), ("from", "fɹɒm"), ("it", "ˈɪt"), ("its", "ɪts"), ("and", "ænd"),
        ("or", "ɔː"), ("but", "bʌt"), ("not", "nˈɒt"), ("this", "ðˈɪs"), ("that", "ðˈæt"), ("there", "ðˈɛə"),
        ("here", "hˈɪə"), ("what", "wˈɒt"), ("how", "hˈaʊ"), ("my", "mˈaɪ"), ("me", "mˈiː"), ("we", "wˈiː"),
        ("all", "ˈɔːl"), ("one", "wˈʌn"), ("two", "tˈuː"), ("three", "θɹˈiː"), ("four", "fˈɔː"), ("five", "fˈaɪv"),
        ("six", "sˈɪks"), ("seven", "sˈɛvən"), ("eight", "ˈeɪt"), ("nine", "nˈaɪn"), ("ten", "tˈɛn"),
        ("zero", "zˈɪəɹəʊ"), ("percent", "pəsˈɛnt"), ("degrees", "dɪɡɹˈiːz"), ("open", "ˈəʊpən"),
        ("opening", "ˈəʊpənɪŋ"), ("close", "klˈəʊz"), ("closing", "klˈəʊzɪŋ"), ("system", "sˈɪstəm"),
        ("systems", "sˈɪstəmz"), ("online", "ˌɒnlˈaɪn"), ("ready", "ɹˈɛdi"), ("service", "sˈɜːvɪs"),
        ("volume", "vˈɒljuːm"), ("spotify", "spˈɒtɪfaɪ"), ("discord", "dˈɪskɔːd"), ("browser", "bɹˈaʊzə"),
        ("please", "plˈiːz"), ("memory", "mˈɛməɹi"), ("time", "tˈaɪm"), ("today", "tədˈeɪ"),
    ])
});

fn rules_phonemize(text: &str) -> String {
    let mut out = String::new();
    for (phrase, punct) in split_phrases(text) {
        let words: Vec<String> = phrase
            .split_whitespace()
            .map(|w| w.chars().filter(|c| c.is_alphanumeric() || *c == '\'').collect::<String>().to_lowercase())
            .filter(|w| !w.is_empty())
            .map(|w| {
                LEXICON
                    .get(w.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| spell_word(&w))
            })
            .collect();
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&words.join(" "));
        out.push_str(&punct);
    }
    out
}

fn spell_word(word: &str) -> String {
    if let Ok(n) = word.parse::<u64>() {
        return n
            .to_string()
            .chars()
            .filter_map(|d| {
                let name = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine"]
                    [d.to_digit(10)? as usize];
                LEXICON.get(name).map(|s| s.to_string())
            })
            .collect::<Vec<_>>()
            .join(" ");
    }
    let chars: Vec<char> = word.chars().collect();
    let n = chars.len();
    let mut ipa = String::new();
    if n >= 3 {
        ipa.push('ˈ');
    }
    let mut i = 0;
    while i < n {
        let rest: String = chars[i..].iter().collect();
        let digraphs: [(&str, &str); 22] = [
            ("tion", "ʃən"), ("sion", "ʒən"), ("igh", "aɪ"), ("tch", "tʃ"), ("dge", "dʒ"), ("ch", "tʃ"),
            ("sh", "ʃ"), ("th", "θ"), ("ph", "f"), ("qu", "kw"), ("ng", "ŋ"), ("ck", "k"), ("wh", "w"),
            ("ee", "iː"), ("ea", "iː"), ("oo", "uː"), ("ou", "aʊ"), ("ow", "aʊ"), ("ai", "eɪ"), ("ay", "eɪ"),
            ("oa", "əʊ"), ("ar", "ɑː"),
        ];
        if let Some((pat, rep)) = digraphs.iter().find(|(p, _)| rest.starts_with(p)) {
            ipa.push_str(rep);
            i += pat.chars().count();
            continue;
        }
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let magic_e = i + 2 == n - 1 && chars[n - 1] == 'e' && next.is_some_and(|x| !"aeiou".contains(x));
        match c {
            'a' => ipa.push_str(if magic_e { "eɪ" } else { "æ" }),
            'e' if i + 1 == n && n > 3 => {}
            'e' => ipa.push('ɛ'),
            'i' => ipa.push_str(if magic_e { "aɪ" } else { "ɪ" }),
            'o' => ipa.push_str(if magic_e { "əʊ" } else { "ɒ" }),
            'u' => ipa.push_str(if magic_e { "juː" } else { "ʌ" }),
            'y' => ipa.push(if i == 0 { 'j' } else { 'i' }),
            'c' => ipa.push(if matches!(next, Some('e' | 'i' | 'y')) { 's' } else { 'k' }),
            'g' => ipa.push('ɡ'),
            'j' => ipa.push_str("dʒ"),
            'q' => ipa.push('k'),
            'x' => ipa.push_str("ks"),
            'r' => {
                if i == 0 || matches!(next, Some('a' | 'e' | 'i' | 'o' | 'u' | 'y')) {
                    ipa.push('ɹ');
                }
            }
            c if c.is_ascii_alphabetic() => ipa.push(c),
            _ => {}
        }
        i += 1;
    }
    ipa
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_phrases_but_not_numbers() {
        let p = split_phrases("Right away, Sir. It is 3.5 degrees at 12:30!");
        assert_eq!(p[0], ("Right away".to_string(), ",".to_string()));
        assert_eq!(p[1], ("Sir".to_string(), ".".to_string()));
        assert_eq!(p[2], ("It is 3.5 degrees at 12:30".to_string(), "!".to_string()));
    }

    #[test]
    fn postprocess_matches_kokoro_conventions() {
        // misaki's notation for v1.0: tied diphthongs and affricates as one letter each.
        assert_eq!(postprocess_en("ɹˈa^ɪt ɐwˈe^ɪ , sˈɜː ."), "ɹˈIt əwˈA, sˈɜː.");
        assert_eq!(postprocess_en("ˈɔːl sˈɪstəmz ɑːɹ ˌɒnlˈa^ɪn ."), "ˈɔːl sˈɪstəmz ɑːɹ ˌɒnlˈIn.");
        assert_eq!(postprocess_en("ɪt z ɡʊd"), "ɪtz ɡʊd");
        assert_eq!(postprocess_en("(en)həlˈə^ʊ"), "həlˈQ");
        assert_eq!(postprocess_en("d^ʒˈɑːvɪs t^ʃˈe^ə"), "ʤˈɑːvɪs ʧˈɛː");
        assert_eq!(postprocess_en("bˈʌʔn\u{329} bˈɒtə^l kˈɔːtʃɪp"), "bˈʌtn bˈɒtᵊl kˈɔːtʃɪp", "untied t+ʃ stays two sounds");
        assert_eq!(rules_en("Right away"), "ɹˈIt əwˈA");
        // v0.19 hears its old spellings.
        assert_eq!(unmerge("ɹˈIt əwˈA, ʤˈɑːvɪs bˈɒtᵊl"), "ɹˈaɪt əwˈeɪ, dʒˈɑːvɪs bˈɒtəl");
        // German keeps its own sounds: uvular r, "ach" and "ich", and ʏ as y.
        assert_eq!(postprocess_de("rˈʏkən bˈax ɪç t^sˈa^ɪt"), "ʁˈykən bˈax ɪç ʦˈIt");
        assert_eq!(postprocess_de("(de)ʃtɾˈɑːsə"), "ʃtɾˈɑːsə");
    }

    #[test]
    fn german_words_and_sentences_are_found() {
        let runs = language_runs("Your WebUntis shows LK Mathematik and Geschichte");
        assert_eq!(runs, vec![(Lang::En, "Your WebUntis shows".into()), (Lang::De, "LK Mathematik".into()), (Lang::En, "and".into()), (Lang::De, "Geschichte".into())]);
        let runs = language_runs("Today is Arme with Herr Müller at 16 Uhr");
        assert!(runs.contains(&(Lang::De, "Arme".into())) && runs.contains(&(Lang::De, "Herr Müller".into())), "{runs:?}");
        assert_eq!(language_runs("Your Deutschprobeklausur is on Friday")[1], (Lang::De, "Deutschprobeklausur".into()));
        // English words that look German stay English.
        for english in ["The kitchen timer is set", "Pause the music in the gym", "He has a hat and a bald spot", "MIT and DER are acronyms", "Your school schedule in Germany is musical"] {
            assert!(language_runs(english).iter().all(|(l, _)| *l == Lang::En), "{english}: {:?}", language_runs(english));
        }
        assert!(sentence_is_german("Ich habe nicht viel Zeit für dich."));
        assert!(sentence_is_german("Morgen um 16 Uhr hast du Mathe."));
        assert!(!sentence_is_german("You have Mathematik and Geschichte tomorrow."));
        assert!(!sentence_is_german("I will die on that hill, man."));
    }

    #[test]
    fn english_only_fallbacks_respell_german() {
        assert_eq!(english_spelling("Frau Schmidt aus Köln"), "Frau Shmidt aus Kerln");
        assert_eq!(english_spelling("school schedule"), "school schedule");
    }

    #[test]
    fn fallback_rules_produce_ipa() {
        let ipa = rules_phonemize("Opening Spotify now, Sir.");
        assert!(ipa.contains("spˈɒtɪfaɪ") && ipa.ends_with('.'), "{ipa}");
    }

    #[test]
    fn espeak_backend_if_installed() {
        let p = Phonemizer::new(None);
        let ipa = p.phonemize("Right away, Sir. Opening Spotify now.");
        assert!(!ipa.is_empty());
        assert!(ipa.contains(',') && ipa.ends_with('.'), "punctuation kept: {ipa}");
        println!("[{}] {ipa}", p.backend_name());
        if matches!(p.backend, Backend::Library(_)) {
            // One string, two languages: the German words carry German sounds (ç, x, ʁ).
            let mixed = p.phonemize("Your WebUntis shows LK Mathematik and Geschichte, Sir.");
            assert!(mixed.contains("ɡəʃˈɪçtə") && mixed.contains("atematˈiːk") && mixed.contains("ˌɛlkˈɑː") && mixed.ends_with('.'), "{mixed}");
            assert!(mixed.contains("ʃˈQz") || mixed.contains("ʃQz"), "English stays English: {mixed}");
            let hello = p.phonemize("Good evening, Sir. All systems are online.");
            assert!(hello.replace('ˈ', "").contains("ɒnlIn") && !hello.contains(['^', 'e']) && !hello.contains("aɪ"), "{hello}");
            let german = p.phonemize("Ich habe nicht viel Zeit, morgen um acht.");
            assert!(german.contains("ɪç") && german.contains("ˈaxt"), "{german}");
            // Switching back to English after German.
            assert_eq!(p.phonemize("Right away, Sir."), p.phonemize("Right away, Sir."));
            assert!(!p.phonemize("Right away, Sir.").contains('ʁ'));
        }
    }
}
