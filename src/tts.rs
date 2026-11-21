//! Streaming text-to-speech: clause splitting of LLM tokens → speech normalisation →
//! bilingual eSpeak-NG phonemes (English and German runs, one IPA string) → Kokoro-ONNX
//! (24 kHz, one voice for both languages) → the film mastering chain (`jai_voice_dsp`, 48 kHz)
//! → resample → lock-free ring → cpal output.
//!
//! Barge-in: [`TtsController::stop`] bumps a generation counter (queued and in-flight clauses
//! of older generations are dropped) and sets a flush flag the audio callback honours on its
//! next buffer (≈10 ms), so playback stops well under 50 ms.

use crate::config::JaiConfig;
use crate::dsp;
use crate::ears::AudioControls;
use crate::phonemizer::Phonemizer;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use ort::session::Session;
use ort::value::Tensor;
use regex::Regex;
use ringbuf::{HeapConsumer, HeapProducer, HeapRb};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use crate::jai_voice_dsp;
use tracing::{error, info, warn};

pub const KOKORO_RATE: u32 = 24_000;
/// The film voice (`bm_jai_movie_1to1`): one style vector interpolated row by row at load time
/// from 75 % Fable (crisp, non-rhotic RP diction, dry cadence) and 25 % George (calm baritone chest
/// authority), matched to the film clip. A component the installed voice pack lacks (`bm_fable`
/// ships only with Kokoro v1.0) is skipped and the other weights are renormalised.
pub const JAI_VOICE: &str = "bm_jai_movie_1to1";
pub const JAI_VOICE_BLEND: &str = "bm_fable:0.75+bm_george:0.25";
/// The v15 film blend (config v17 moves it back to the alias).
pub const V15_VOICE_BLEND: &str = "bm_fable:0.62+bm_george:0.28+bm_lewis:0.10";
/// The previous stock blend (config migrations move it to `JAI_VOICE`).
pub const LEGACY_VOICE_BLEND: &str = "bm_fable:0.45+bm_george:0.4+bm_lewis:0.15";
/// The speaking tempo: the film's calm, unhurried 0.98×; the synthesis runs 3 % faster to make up
/// for the pitch lock's stretch (`jai_voice_dsp::synthesis_speed`).
pub const JAI_VOICE_SPEED: f32 = 0.98;
/// The v15 tempo (config v17 moves it to `JAI_VOICE_SPEED`).
pub const V15_VOICE_SPEED: f32 = 1.04;
const STYLE_DIM: usize = 256;
const MAX_TOKENS: usize = 510;

// ---------------------------------------------------------------------------------------------
// Text → speakable clauses
// ---------------------------------------------------------------------------------------------

static MD_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\([^)]+\)").expect("regex"));
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://(www\.)?([A-Za-z0-9.-]+)[^\s)]*").expect("regex"));
static LIST_MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*([-*+•]|\d+[.)])\s+").expect("regex"));
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*#{1,6}\s*").expect("regex"));
static EMOJI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\x{1F000}-\x{1FAFF}\x{2600}-\x{27BF}\x{FE0F}\x{200D}]").expect("regex")
});

/// A list item or heading without an end of its own: it gets a full stop, so the voice pauses
/// after it as after a sentence instead of running the lines together.
static LIST_LINE_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?mR)^([ \t]*(?:[-*+•]|\d+[.)]|#{1,6})[ \t]+.*?[^.!?:;,\s])[ \t]*$").expect("regex"));

/// A link as one says it: its site with "dot", and its first path part when that is a name
/// ("https://github.com/example/repo?x=1" → "github dot com slash example").
fn spoken_link(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let site = host.split(':').next().unwrap_or(host).replace('.', " dot ");
    let first = path.split(['/', '?', '#']).next().unwrap_or("");
    match first.chars().count() {
        1..=24 if first.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') => format!("{site} slash {first}"),
        _ => site,
    }
}

/// Removes markdown and symbols that must never be read aloud.
pub fn strip_markdown(text: &str) -> String {
    let s = MD_LINK.replace_all(text, "$1");
    let s = URL.replace_all(&s, |c: &regex::Captures| {
        let core = c[0].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        format!("{}{}", spoken_link(core), &c[0][core.len()..])
    });
    let s = EMOJI.replace_all(&s, "");
    let s = LIST_LINE_END.replace_all(&s, "$1.");
    let s = HEADING.replace_all(&s, "");
    let s = LIST_MARKER.replace_all(&s, "");
    let s: String = s
        .chars()
        .filter(|c| !matches!(c, '*' | '#' | '`' | '~' | '|' | '>' | '<'))
        .map(|c| if c == '_' { ' ' } else { c })
        .collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

static UNDERSCORE_BOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"__([^_\n]+?)__").expect("regex"));
static HEADING_MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^[ \t]*#{1,6}[ \t]*").expect("regex"));
static EMPTY_PARENS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(\s*\)").expect("regex"));
static SPACE_BEFORE_PUNCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]+([.,!?;:])").expect("regex"));
static BLANK_LINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").expect("regex"));
static LEADING_PUNCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[.,;:]+\s+").expect("regex"));

const META_WORDS: &[&str] = &["note", "notes", "translation", "translated", "meta", "internal", "aside", "ooc", "answered", "varied"];
const META_COLON_WORDS: &[&str] = &["action", "actions", "tool", "cmd", "command", "system", "instruction", "instructions", "response", "context"];
const STAGE_VERBS: &[&str] = &["sighs", "chuckles", "laughs", "coughs", "pauses", "smiles", "nods", "beeps", "whirs", "grins", "winks", "ahem"];

/// A separator nobody sees is a space: a language model now and then writes a zero-width
/// space where a space belongs, and "in Berlin" was shown and spoken as "inBerlin" (silent
/// session, 2026-11-06). A soft hyphen is nothing at all.
fn visible_spaces(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(['\u{200b}', '\u{200c}', '\u{2060}', '\u{feff}', '\u{00ad}']) {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(text.chars().filter(|c| *c != '\u{00ad}').map(|c| if matches!(c, '\u{200b}' | '\u{200c}' | '\u{2060}' | '\u{feff}') { ' ' } else { c }).collect())
}

/// Strips reasoning, bracketed notes, tags, meta asides, stage directions, fences, URLs and JSON.
pub fn sanitize_for_display_and_speech(text: &str) -> String {
    let spaced = visible_spaces(text);
    let text: &str = &spaced;
    let lower = text.to_ascii_lowercase();
    let start = match (lower.find("<think"), lower.find("</think")) {
        (None, Some(close)) => lower[close..].find('>').map(|q| close + q + 1).unwrap_or(text.len()),
        (Some(open), Some(close)) if close < open => lower[close..].find('>').map(|q| close + q + 1).unwrap_or(text.len()),
        _ => 0,
    };
    let (scanned, _) = scan_meta(&text[start..], true);
    let s = UNDERSCORE_BOLD.replace_all(&scanned, "$1");
    let s = HEADING_MARK.replace_all(&s, "");
    let s = EMPTY_PARENS.replace_all(&s, "").replace(']', "");
    let lines: Vec<String> = s.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect();
    let s = lines.join("\n");
    let s = SPACE_BEFORE_PUNCT.replace_all(&s, "$1");
    let s = BLANK_LINES.replace_all(&s, "\n\n");
    let s = s.trim();
    LEADING_PUNCT.replace(s, "").trim().to_string()
}

enum MetaSpan {
    Plain,
    Open,
    Closed(usize, String),
}

fn word_start(buf: &str, i: usize) -> bool {
    i == 0 || matches!(buf.as_bytes()[i - 1], b' ' | b'\t' | b'\n' | b'\r' | b'(' | b'"' | b'\'')
}

/// Removes complete meta spans; stops at the first span that is still open unless `last`.
fn scan_meta(buf: &str, last: bool) -> (String, usize) {
    let bytes = buf.as_bytes();
    let mut out = String::with_capacity(buf.len());
    let mut plain_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let span = match bytes[i] {
            b'[' => bracket_span(buf, i, last),
            b'<' => angle_span(buf, i, last),
            b'(' => paren_span(buf, i, last),
            b'*' => star_span(buf, i, last),
            b'`' => fence_span(buf, i, last),
            b'{' => json_span(buf, i, last),
            b'h' | b'H' | b'w' | b'W' if word_start(buf, i) => url_span(buf, i, last),
            _ => MetaSpan::Plain,
        };
        match span {
            MetaSpan::Plain => i += 1,
            MetaSpan::Open => {
                out.push_str(&buf[plain_start..i]);
                return (out, i);
            }
            MetaSpan::Closed(end, replacement) => {
                out.push_str(&buf[plain_start..i]);
                out.push_str(&replacement);
                i = end;
                plain_start = end;
            }
        }
    }
    out.push_str(&buf[plain_start..]);
    (out, buf.len())
}

fn unterminated(buf: &str, last: bool) -> MetaSpan {
    if last { MetaSpan::Closed(buf.len(), String::new()) } else { MetaSpan::Open }
}

fn bracket_span(buf: &str, i: usize, last: bool) -> MetaSpan {
    let mut depth = 0usize;
    for (k, c) in buf[i..].char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth > 0 {
                    continue;
                }
                let end = i + k + 1;
                if end == buf.len() && !last {
                    return MetaSpan::Open;
                }
                if buf[end..].starts_with('(') {
                    let label = scan_meta(&buf[i + 1..i + k], true).0;
                    return match buf[end..].find(')') {
                        Some(close) => MetaSpan::Closed(end + close + 1, label),
                        None if last => MetaSpan::Closed(buf.len(), label),
                        None => MetaSpan::Open,
                    };
                }
                return MetaSpan::Closed(end, String::new());
            }
            _ => {}
        }
    }
    if buf.len() - i > 1500 { MetaSpan::Closed(buf.len(), String::new()) } else { unterminated(buf, last) }
}

fn angle_span(buf: &str, i: usize, last: bool) -> MetaSpan {
    let rest = &buf[i..];
    match rest[1..].chars().next() {
        None => return if last { MetaSpan::Plain } else { MetaSpan::Open },
        Some(c) if c.is_ascii_alphabetic() || c == '/' || c == '|' => {}
        Some(_) => return MetaSpan::Plain,
    }
    let lower = rest.to_ascii_lowercase();
    if lower.len() < 6 && "<think".starts_with(lower.as_str()) && !last {
        return MetaSpan::Open;
    }
    if lower.starts_with("<think") {
        return match lower.find("</think") {
            Some(close) => match lower[close..].find('>') {
                Some(q) => MetaSpan::Closed(i + close + q + 1, String::new()),
                None => unterminated(buf, last),
            },
            None => unterminated(buf, last),
        };
    }
    match rest.find('>') {
        Some(p) if p <= 200 && !rest[1..p].contains('<') => MetaSpan::Closed(i + p + 1, String::new()),
        Some(_) => MetaSpan::Plain,
        None if !last && rest.len() <= 200 => MetaSpan::Open,
        None => MetaSpan::Plain,
    }
}

fn paren_span(buf: &str, i: usize, last: bool) -> MetaSpan {
    let rest = &buf[i + 1..];
    let trimmed = rest.trim_start();
    let lower = trimmed.to_lowercase();
    let word: String = lower.chars().take_while(|c| c.is_alphabetic()).collect();
    let complete = word.len() < lower.len();
    let url = ["https://", "http://", "www."];
    let candidates = || META_WORDS.iter().chain(META_COLON_WORDS).chain(&["stage"]);
    if !complete && !last {
        let partial_url = url.iter().any(|u| u.starts_with(lower.as_str()) || lower.starts_with(u));
        return if candidates().any(|m| m.starts_with(word.as_str())) || partial_url { MetaSpan::Open } else { MetaSpan::Plain };
    }
    let after = lower[word.len()..].trim_start();
    let meta = META_WORDS.contains(&word.as_str())
        || url.iter().any(|u| lower.starts_with(u))
        || lower.starts_with("stage direction")
        || lower.starts_with("tool call") && lower["tool call".len()..].trim_start().starts_with(':')
        || META_COLON_WORDS.contains(&word.as_str()) && after.starts_with(':');
    if !meta {
        let waiting = !last && META_COLON_WORDS.contains(&word.as_str()) && after.is_empty();
        return if waiting { MetaSpan::Open } else { MetaSpan::Plain };
    }
    let mut depth = 0usize;
    for (k, c) in buf[i..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return MetaSpan::Closed(i + k + 1, String::new());
                }
            }
            _ => {}
        }
    }
    unterminated(buf, last)
}

fn star_span(buf: &str, i: usize, last: bool) -> MetaSpan {
    let rest = &buf[i..];
    if let Some(body) = rest.strip_prefix("**") {
        let line = body.find('\n');
        return match body.find("**") {
            Some(p) if line.is_none_or(|n| p < n) => MetaSpan::Closed(i + 2 + p + 2, body[..p].to_string()),
            _ if line.is_some() || body.len() > 240 || last => MetaSpan::Closed(i + 2, String::new()),
            _ => MetaSpan::Open,
        };
    }
    let body = &rest[1..];
    let prev_alnum = i > 0 && buf[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric());
    match body.chars().next() {
        None => return if last { MetaSpan::Closed(buf.len(), String::new()) } else { MetaSpan::Open },
        Some(c) if c.is_whitespace() || c == '*' => return MetaSpan::Plain,
        Some(_) if prev_alnum => return MetaSpan::Plain,
        Some(_) => {}
    }
    let line = body.find('\n');
    match body.find('*') {
        Some(p) if p <= 120 && line.is_none_or(|n| p < n) && !body[..p].ends_with(char::is_whitespace) => {
            let inner = body[..p].trim();
            let stage = inner.contains(char::is_whitespace) || STAGE_VERBS.contains(&inner.to_lowercase().as_str());
            MetaSpan::Closed(i + 1 + p + 1, if stage { String::new() } else { inner.to_string() })
        }
        _ if line.is_some() || body.len() > 120 || last => MetaSpan::Closed(i + 1, String::new()),
        _ => MetaSpan::Open,
    }
}

fn fence_span(buf: &str, i: usize, last: bool) -> MetaSpan {
    let rest = &buf[i..];
    if let Some(body) = rest.strip_prefix("```") {
        return match body.find("```") {
            Some(p) => MetaSpan::Closed(i + 3 + p + 3, String::new()),
            None => unterminated(buf, last),
        };
    }
    if !last && "```".starts_with(rest) { MetaSpan::Open } else { MetaSpan::Plain }
}

fn json_span(buf: &str, i: usize, last: bool) -> MetaSpan {
    let after = buf[i + 1..].trim_start();
    match after.chars().next() {
        None => if last { MetaSpan::Plain } else { MetaSpan::Open },
        Some('}') => MetaSpan::Closed(buf.len() - after.len() + 1, String::new()),
        Some('"') => {
            let mut depth = 0usize;
            for (k, c) in buf[i..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return MetaSpan::Closed(i + k + 1, String::new());
                        }
                    }
                    _ => {}
                }
            }
            unterminated(buf, last)
        }
        Some(_) => MetaSpan::Plain,
    }
}

fn url_span(buf: &str, i: usize, last: bool) -> MetaSpan {
    let rest = &buf[i..];
    let head: String = rest.chars().take(8).collect::<String>().to_ascii_lowercase();
    let prefixes = ["https://", "http://", "www."];
    if prefixes.iter().any(|p| head.starts_with(p)) {
        let stop = match rest.find(char::is_whitespace) {
            Some(stop) => stop,
            None if !last => return MetaSpan::Open,
            None => rest.len(),
        };
        let url = &rest[..stop];
        let core = url.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\'']);
        return MetaSpan::Closed(i + stop, url[core.len()..].to_string());
    }
    if !last && rest.len() < 8 && prefixes.iter().any(|p| p.starts_with(head.as_str())) { MetaSpan::Open } else { MetaSpan::Plain }
}

/// East Asian writing and its punctuation. The local model slips into Chinese in the middle of
/// an English sentence now and then ("Why was the math book sad?因为它没有封面"; silent session,
/// 2026-11-06): the voice cannot say it, and he answers in English.
fn east_asian(c: char) -> bool {
    matches!(c, '\u{1100}'..='\u{11FF}' | '\u{2E80}'..='\u{2FDF}' | '\u{3000}'..='\u{31FF}' | '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{AC00}'..='\u{D7FF}' | '\u{F900}'..='\u{FAFF}' | '\u{FE30}'..='\u{FE4F}' | '\u{FF00}'..='\u{FFEF}' | '\u{20000}'..='\u{3FFFF}')
}

/// Streaming form of [`sanitize_for_display_and_speech`] for LLM tokens split at any point.
#[derive(Default)]
pub struct MetaStreamFilter {
    held: String,
    pending_space: String,
    started: bool,
    /// The last thing that came was East Asian writing (it may go on in the next token).
    in_east_asian: bool,
}

impl MetaStreamFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, token: &str) -> String {
        let token = self.without_east_asian(token);
        self.held.push_str(&visible_spaces(&token));
        let (ready, consumed) = scan_meta(&self.held, false);
        self.held.drain(..consumed);
        self.emit(&ready)
    }

    /// One space for a run of it, however the tokens fall.
    fn without_east_asian<'a>(&mut self, token: &'a str) -> std::borrow::Cow<'a, str> {
        if !self.in_east_asian && !token.chars().any(east_asian) {
            return std::borrow::Cow::Borrowed(token);
        }
        let mut kept = String::with_capacity(token.len());
        for c in token.chars() {
            if east_asian(c) {
                if !self.in_east_asian {
                    kept.push(' ');
                }
                self.in_east_asian = true;
            } else if !(self.in_east_asian && c.is_whitespace()) {
                self.in_east_asian = false;
                kept.push(c);
            }
        }
        std::borrow::Cow::Owned(kept)
    }

    pub fn flush(&mut self) -> String {
        let held = std::mem::take(&mut self.held);
        let (ready, _) = scan_meta(&held, true);
        let out = self.emit(&ready.replace(']', ""));
        self.pending_space.clear();
        self.started = false;
        self.in_east_asian = false;
        out
    }

    fn emit(&mut self, text: &str) -> String {
        let body = text.trim();
        if body.is_empty() {
            self.pending_space.push_str(text);
            return String::new();
        }
        let lead = &text[..text.len() - text.trim_start().len()];
        let gap = format!("{}{lead}", self.pending_space);
        let mut out = String::new();
        if self.started && !body.starts_with(['.', ',', '!', '?', ';', ':']) {
            out.push_str(match gap.matches('\n').count() {
                0 if gap.is_empty() => "",
                0 => " ",
                1 => "\n",
                _ => "\n\n",
            });
        }
        out.push_str(body);
        self.pending_space = text[text.trim_end().len()..].to_string();
        self.started = true;
        out
    }
}

static UNITS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"\bJ\.A\.R\.V\.I\.S\.?", "JAI"),
        (r"\bJAI\b", "JAI"),
        (r"°\s?C\b", " degrees Celsius"),
        (r"°\s?F\b", " degrees Fahrenheit"),
        (r"°", " degrees"),
        (r"(\d)\s?GB\b", "$1 gigabytes"),
        (r"(\d)\s?MB\b", "$1 megabytes"),
        (r"(\d)\s?TB\b", "$1 terabytes"),
        (r"(\d)\s?GHz\b", "$1 gigahertz"),
        (r"(\d)\s?MHz\b", "$1 megahertz"),
        (r"(\d)\s?ms\b", "$1 milliseconds"),
        (r"(\d)\s?W\b", "$1 watts"),
        (r"\bVRAM\b", "V-RAM"),
        (r"\be\.g\.", "for example"),
        (r"\bi\.e\.", "that is"),
        (r"\betc\.", "et cetera"),
        (r"\bvs\b\.?", "versus"),
        (r"\bMr\.", "Mister"),
        (r"\bMrs\.", "Missus"),
        (r"\bMs\.", "Miz"),
        (r"\bDr\.", "Doctor"),
        (r"\bProf\.", "Professor"),
        (r"\bapprox\.", "approximately"),
        (r"\ba\.k\.a\.", "also known as"),
        (r"#(\d)", "number $1"),
        // The letter A on its own is read as the article: "eh".
        (r"\bA (P Is?|M)\b", "eh $1"),
        (r"\s&\s", " and "),
        (r"(\w)\s?\+\s?(\w)", "$1 plus $2"),
    ]
    .into_iter()
    .map(|(p, r)| (Regex::new(p).expect("valid unit regex"), r))
    .collect()
});

/// Units, abbreviations and acronyms → words eSpeak reads naturally. German words stay German
/// ("LK Mathematik", "Herr Müller", "Arme"): the phonemizer gives them German pronunciation in
/// the same voice, so nothing is translated or respelled here.
pub fn normalize_for_speech(text: &str) -> String {
    // Names, acronyms, times, paths and identifiers first (the pronunciation lexicon).
    let mut s = crate::pronunciation_lexicon::PronunciationLexicon::normalize_for_speech(&strip_markdown(text));
    for (re, rep) in UNITS.iter() {
        s = re.replace_all(&s, *rep).into_owned();
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphanumeric())).count()
}

/// Streams LLM tokens into speakable clauses.
///
/// * The first clause leaves as soon as it is a complete short sentence ("Certainly, Sir." —
///   usually a pre-rendered phrase, so it plays with no synthesis at all) or four words ending
///   at a comma, and is cut at a natural breath (before a conjunction, else before an article
///   or preposition) once it runs to eight words without punctuation: a short first clause
///   synthesises in a fraction of the time, so speech starts while the model is still writing.
/// * The second clause needs four words at sentence punctuation (eight at a comma, cut at
///   eighteen); it is synthesised while the first one plays.
/// * Later clauses need eight words at sentence punctuation and fourteen at a comma.
/// * The vocative never splits off ("Let me check, | Sir." sounds choppy).
/// * Fenced code blocks and meta spans are dropped; numbers (3.5, 12:30, 1,000), acronyms,
///   common abbreviations and open brackets never split.
#[derive(Default)]
pub struct ClauseSplitter {
    raw: String,
    buf: String,
    in_code: bool,
    emitted: usize,
}

impl ClauseSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// For complete sentences (acknowledgements, fast-path replies): split at sentence
    /// boundaries only, so short replies stay one natural (and cacheable) clause.
    pub fn for_text() -> Self {
        Self::default()
    }

    pub fn push(&mut self, token: &str) -> Vec<String> {
        self.raw.push_str(token);
        let mut boundary_from_code = false;
        loop {
            if self.in_code {
                match self.raw.find("```") {
                    Some(end) => {
                        self.raw.drain(..end + 3);
                        self.in_code = false;
                    }
                    None => {
                        let keep = self.raw.chars().rev().take_while(|c| *c == '`').count().min(2);
                        let cut = self.raw.len() - keep;
                        self.raw.drain(..cut);
                        break;
                    }
                }
            } else if let Some(start) = self.raw.find("```") {
                let before: String = self.raw[..start].to_string();
                self.buf.push_str(&before);
                self.raw.drain(..start + 3);
                self.in_code = true;
                boundary_from_code = true;
            } else {
                let keep = self.raw.chars().rev().take_while(|c| *c == '`').count().min(2);
                let cut = self.raw.len() - keep;
                let moved: String = self.raw[..cut].to_string();
                self.buf.push_str(&moved);
                self.raw.drain(..cut);
                break;
            }
        }
        let mut out = self.drain_clauses();
        if boundary_from_code {
            if let Some(rest) = self.take_buffer() {
                out.push(rest);
            }
        }
        out
    }

    pub fn flush(&mut self) -> Option<String> {
        if !self.in_code {
            let rest = std::mem::take(&mut self.raw);
            self.buf.push_str(&rest);
        }
        self.raw.clear();
        self.in_code = false;
        self.take_buffer()
    }

    fn take_buffer(&mut self) -> Option<String> {
        let text = std::mem::take(&mut self.buf);
        let clean = strip_markdown(&sanitize_for_display_and_speech(&text));
        if clean.chars().any(|c| c.is_alphanumeric()) {
            self.emitted += 1;
            Some(clean)
        } else {
            None
        }
    }

    fn drain_clauses(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(end) = self.find_boundary() {
            let clause: String = self.buf[..end].to_string();
            self.buf.drain(..end);
            let clean = strip_markdown(&sanitize_for_display_and_speech(&clause));
            if clean.chars().any(|c| c.is_alphanumeric()) {
                self.emitted += 1;
                out.push(clean);
            }
        }
        out
    }

    fn find_boundary(&self) -> Option<usize> {
        let buf = &self.buf;
        let chars: Vec<(usize, char)> = buf.char_indices().collect();
        let mut open = 0usize;
        let mut last_closed_space = None;
        for (k, &(i, c)) in chars.iter().enumerate() {
            let next_char = chars.get(k + 1).map(|&(_, n)| n);
            match c {
                '[' | '(' => open += 1,
                '<' if next_char.is_some_and(|n| n.is_ascii_alphabetic() || n == '/' || n == '|') => open += 1,
                ']' | ')' | '>' => open = open.saturating_sub(1),
                _ => {}
            }
            if open == 0 && c.is_whitespace() && i < 380 {
                last_closed_space = Some(i + 1);
            }
            let strong = matches!(c, '.' | '!' | '?' | ';' | ':' | '…');
            let weak = c == ',';
            if (!strong && !weak) || open > 0 {
                continue;
            }
            let next = next_char;
            let prev = if k > 0 { Some(chars[k - 1].1) } else { None };
            if c != '\n' && c != '—' && c != '–' {
                match next {
                    None => return None, // need look-ahead (decimal point? abbreviation?)
                    Some(n) if !(n.is_whitespace() || matches!(n, '"' | '\'' | ')' | '”' | '’')) => continue,
                    _ => {}
                }
            }
            if matches!(c, '.' | ',' | ':') && prev.is_some_and(|p| p.is_ascii_digit()) && next.is_some_and(|n| n.is_ascii_digit()) {
                continue;
            }
            if c == '.' {
                let word: String = buf[..i]
                    .rsplit(|ch: char| ch.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                let abbreviations = ["mr", "mrs", "ms", "dr", "st", "vs", "etc", "e.g", "i.e", "approx", "no", "nr", "ca", "z.b", "bzw"];
                let is_acronym = word.len() >= 2 && word.split('.').all(|p| p.chars().count() <= 1);
                if abbreviations.contains(&word.as_str()) || is_acronym {
                    continue;
                }
            }
            let end = i + c.len_utf8();
            if weak {
                // Never split off a trailing vocative: "Let me check, | Sir." sounds choppy.
                let after = buf[end..].trim_start().to_lowercase();
                if after.is_empty() || "sir".starts_with(after.as_str()) {
                    return None; // wait for the next token
                }
                if after.starts_with("sir") && !after[3..].starts_with(|ch: char| ch.is_alphanumeric()) {
                    continue;
                }
            }
            let words = word_count(&buf[..end]);
            let enough = match (self.emitted, strong) {
                (0, true) => words >= 2,
                (0, false) => words >= 4,
                (1, true) => words >= 4,
                (1, false) => words >= 8,
                (_, true) => words >= 8,
                (_, false) => words >= 14,
            };
            if enough {
                return Some(end);
            }
        }
        // The first clauses must not wait for a long sentence to end: cut at a word boundary.
        let cap = match self.emitted {
            0 => Some((8, 4, true)),
            1 => Some((18, 6, false)),
            _ => None,
        };
        if let Some(cut) = cap.and_then(|(cap, min, soft)| self.early_cut(cap, min, soft)) {
            return Some(cut);
        }
        // Run-on sentence without punctuation: split at a word boundary outside brackets.
        let buf = &self.buf;
        if buf.chars().count() > 420 && word_count(buf) >= 40 {
            if last_closed_space.is_some() {
                return last_closed_space;
            }
            let cut = buf.char_indices().nth(380).map(|(i, _)| i).unwrap_or(buf.len());
            return buf[..cut].rfind(char::is_whitespace).map(|i| i + 1);
        }
        None
    }

    /// A cut once `cap` complete words outside brackets are in, keeping at least `min` words:
    /// before the last conjunction (where a speaker breathes), else — with `soft` — before the
    /// last article or preposition ("… the audio devices panel | the skills panel"), else right
    /// after the cap-th word.
    fn early_cut(&self, cap: usize, min: usize, soft: bool) -> Option<usize> {
        const BREATH: &[&str] = &["and", "but", "so", "because", "which", "while", "then", "or", "when", "where", "although", "und", "aber", "weil"];
        const SOFT: &[&str] = &["the", "a", "an", "for", "to", "with", "in", "on", "at", "from", "into", "about", "der", "die", "das", "mit", "für", "auf", "im"];
        let buf = &self.buf;
        // (start, end) of every complete word: one that whitespace follows, outside brackets.
        let mut words: Vec<(usize, usize)> = Vec::new();
        let mut open = 0usize;
        let mut start: Option<usize> = None;
        for (i, c) in buf.char_indices() {
            match c {
                '[' | '(' => open += 1,
                ']' | ')' => open = open.saturating_sub(1),
                _ => {}
            }
            if c.is_whitespace() {
                if let Some(s) = start.take() {
                    if open == 0 && buf[s..i].chars().any(char::is_alphanumeric) {
                        words.push((s, i));
                    }
                }
            } else if start.is_none() {
                start = Some(i);
            }
        }
        if words.len() < cap || open > 0 {
            return None;
        }
        let word = |k: usize| buf[words[k].0..words[k].1].trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        let breath = (min..cap)
            .rev()
            .find(|&k| BREATH.contains(&word(k).as_str()))
            // "… maths | and then you have …": before the first of adjacent conjunctions.
            .map(|mut k| {
                while k > min && BREATH.contains(&word(k - 1).as_str()) {
                    k -= 1;
                }
                k
            })
            .or_else(|| if soft { (min..cap).rev().find(|&k| SOFT.contains(&word(k).as_str())) } else { None });
        Some(match breath {
            Some(k) => words[k].0,
            None => {
                let end = words[cap - 1].1;
                end + buf[end..].chars().next().map(char::len_utf8).unwrap_or(0)
            }
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Kokoro-ONNX engine
// ---------------------------------------------------------------------------------------------

pub struct Kokoro {
    session: Session,
    vocab: HashMap<char, i64>,
    /// v0.19 learned eSpeak's two-letter spellings, not v1.0's single letters (misaki).
    legacy: bool,
    voices: HashMap<String, Arc<Vec<f32>>>,
    models_dir: PathBuf,
    archive: PathBuf,
}

impl Kokoro {
    pub fn load(cfg: &JaiConfig) -> Result<Self, String> {
        let path = Path::new(&cfg.kokoro_model_path);
        let env = crate::config::ort_env().map_err(|e| e.to_string())?;
        let mut builder = Session::builder(env)
            .map_err(|e| e.to_string())?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .unwrap_or_else(|e| e.recover())
            .with_intra_threads(cfg.tts_thread_count())
            .unwrap_or_else(|e| e.recover());
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut device = "CPU";
        #[cfg(windows)]
        if cfg.tts_gpu {
            use ort::ep::directml::{DirectML, PerformancePreference};
            let dml = DirectML::default()
                .with_performance_preference(PerformancePreference::HighPerformance)
                .build()
                .error_on_failure();
            builder = match builder.with_execution_providers([dml]) {
                Ok(b) => {
                    device = "GPU (DirectML)";
                    // DirectML requires memory patterns off.
                    b.with_memory_pattern(false).unwrap_or_else(|e| e.recover())
                }
                Err(e) => {
                    warn!("[TTS] DirectML unavailable ({e}); Kokoro runs on the CPU.");
                    e.recover()
                }
            };
        }
        let session = builder
            .commit_from_file(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let models_dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("models"));
        let vocab = load_vocab(&models_dir.join("kokoro_config.json"));
        info!(
            "[TTS] Kokoro loaded from {} on {device} ({} CPU threads, vocab {} symbols)",
            path.display(),
            cfg.tts_thread_count(),
            vocab.len()
        );
        Ok(Self {
            session,
            vocab,
            legacy: !crate::models::is_kokoro_v1(&cfg.kokoro_model_path),
            voices: HashMap::new(),
            models_dir,
            archive: PathBuf::from(&cfg.kokoro_voices_archive_path),
        })
    }

    /// Voice style table `[rows, 256]`: `models/<voice>.bin` (raw f32) or extracted on demand
    /// from the kokoro-onnx v0.19 `voices.bin` archive (and cached as `.bin`).
    pub fn voice(&mut self, name: &str) -> Result<Arc<Vec<f32>>, String> {
        if let Some(v) = self.voices.get(name) {
            return Ok(Arc::clone(v));
        }
        if name == JAI_VOICE {
            // The blend calibrated against the reference clip (`--voice-calibrate`), else the stock one.
            let blend = crate::fish_zero_shot_tts::calibrated_blend().unwrap_or_else(|| JAI_VOICE_BLEND.to_string());
            if blend != JAI_VOICE_BLEND {
                info!("[TTS] {JAI_VOICE} uses the calibrated blend {blend}");
            }
            let mix = self.voice(&blend)?;
            self.voices.insert(name.to_string(), Arc::clone(&mix));
            return Ok(mix);
        }
        if name.contains(['+', ',', ':']) {
            let mut loaded = Vec::new();
            for (component, weight) in voice_components(name) {
                match self.voice(&component) {
                    Ok(table) => loaded.push((table, weight)),
                    Err(e) => warn!("[TTS] Voice '{component}' of the blend '{name}' unavailable: {e}"),
                }
            }
            let rows = loaded.iter().map(|(t, _)| t.len() / STYLE_DIM).min().unwrap_or(0);
            if rows == 0 {
                return Err(format!("no voice of the blend '{name}' could be loaded"));
            }
            let total: f32 = loaded.iter().map(|(_, w)| w).sum();
            let mut mix = vec![0.0f32; rows * STYLE_DIM];
            for (table, weight) in &loaded {
                let share = weight / total;
                mix.iter_mut().zip(table.iter()).for_each(|(m, v)| *m += v * share);
            }
            let mix = Arc::new(mix);
            self.voices.insert(name.to_string(), Arc::clone(&mix));
            return Ok(mix);
        }
        let raw_path = crate::models::voice_cache_path(&self.models_dir, &self.archive, name);
        if let Some(dir) = raw_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let table = match std::fs::read(&raw_path) {
            Ok(bytes) if bytes.len() >= STYLE_DIM * 4 && bytes.len() % (STYLE_DIM * 4) == 0 => bytes_to_f32(&bytes),
            _ => {
                let table = crate::models::extract_voice(&self.archive, name)?;
                let bytes: Vec<u8> = table.iter().flat_map(|f| f.to_le_bytes()).collect();
                if let Err(e) = std::fs::write(&raw_path, bytes) {
                    warn!("[TTS] Could not cache voice {name}: {e}");
                }
                table
            }
        };
        let table = Arc::new(table);
        self.voices.insert(name.to_string(), Arc::clone(&table));
        Ok(table)
    }

    pub fn available_voices(&self) -> Vec<String> {
        let mut voices = crate::models::list_voices(&self.archive).unwrap_or_default();
        // Cached tables of this model version only (another version's tables would not fit).
        let cache_dir = crate::models::voice_cache_path(&self.models_dir, &self.archive, "x").parent().map(Path::to_path_buf).unwrap_or_else(|| self.models_dir.clone());
        if let Ok(entries) = std::fs::read_dir(&cache_dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(stem) = name.strip_suffix(".bin") {
                    let stem = stem.to_string();
                    let looks_like_voice = stem.len() > 3 && stem.as_bytes().get(2) == Some(&b'_');
                    if looks_like_voice && !voices.contains(&stem) {
                        voices.push(stem);
                    }
                }
            }
        }
        voices.sort();
        voices.insert(0, JAI_VOICE.to_string());
        voices
    }

    pub fn synthesize(&mut self, ipa: &str, voice: &str, speed: f32) -> Result<Vec<f32>, String> {
        let ipa = if self.legacy { crate::phonemizer::unmerge(ipa) } else { ipa.to_string() };
        let mut ids: Vec<i64> = ipa.chars().filter_map(|c| self.vocab.get(&c).copied()).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        ids.truncate(MAX_TOKENS);
        let style_table = self.voice(voice)?;
        let rows = style_table.len() / STYLE_DIM;
        let row = ids.len().min(rows.saturating_sub(1));
        let style = style_table[row * STYLE_DIM..(row + 1) * STYLE_DIM].to_vec();

        let mut tokens = Vec::with_capacity(ids.len() + 2);
        tokens.push(0);
        tokens.extend_from_slice(&ids);
        tokens.push(0);
        let n = tokens.len();
        let run = || -> ort::Result<Vec<f32>> {
            let outputs = self.session.run(ort::inputs![
                "tokens" => Tensor::from_array(([1usize, n], tokens))?,
                "style" => Tensor::from_array(([1usize, STYLE_DIM], style))?,
                "speed" => Tensor::from_array(([1usize], vec![speed]))?
            ])?;
            Ok(outputs[0].try_extract_tensor::<f32>()?.1.to_vec())
        };
        run().map_err(|e| e.to_string())
    }
}

/// "bm_george:0.45+bm_fable:0.35+bm_lewis:0.2" or "bm_george,bm_lewis" (equal weights) →
/// (voice, weight) pairs; malformed or non-positive parts are ignored.
fn voice_components(name: &str) -> Vec<(String, f32)> {
    name.split(['+', ','])
        .filter_map(|part| {
            let part = part.trim();
            let (voice, weight) = match part.split_once(':') {
                Some((voice, weight)) => (voice.trim(), weight.trim().parse::<f32>().ok()?),
                None => (part, 1.0),
            };
            (!voice.is_empty() && weight.is_finite() && weight > 0.0).then(|| (voice.to_string(), weight))
        })
        .collect()
}

fn bytes_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn load_vocab(config_path: &Path) -> HashMap<char, i64> {
    let parsed = std::fs::read_to_string(config_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("vocab").and_then(|m| m.as_object().cloned()));
    if let Some(map) = parsed {
        let vocab: HashMap<char, i64> = map
            .iter()
            .filter_map(|(k, v)| {
                let mut chars = k.chars();
                let c = chars.next()?;
                chars.next().is_none().then_some((c, v.as_i64()?))
            })
            .collect();
        if vocab.len() > 100 {
            return vocab;
        }
    }
    warn!("[TTS] kokoro_config.json vocab unavailable; using the built-in table.");
    builtin_vocab()
}

fn builtin_vocab() -> HashMap<char, i64> {
    const TABLE: &str = ";:,.!?—…\"()“” \u{303}ʣʥʦʨᵝ\u{AB67}AIOQSTWYᵊabcdefhijklmnopqrstuvwxyzɑɐɒæβɔɕçɖðʤəɚɛɜɟɡɥɨɪʝɯɰŋɳɲɴøɸθœɹɾɻʁɽʂʃʈʧʊʋʌɣɤχʎʒʔˈˌːʰʲ↓→↗↘ᵻ";
    const IDS: &[i64] = &[
        1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 31, 33, 35, 36, 39,
        41, 42, 43, 44, 45, 46, 47, 48, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67,
        68, 69, 70, 71, 72, 75, 76, 77, 78, 80, 81, 82, 83, 85, 86, 87, 90, 92, 99, 101, 102, 103, 110, 111, 112,
        113, 114, 115, 116, 118, 119, 120, 123, 125, 126, 128, 129, 130, 131, 132, 133, 135, 136, 138, 139, 140,
        142, 143, 147, 148, 156, 157, 158, 162, 164, 169, 171, 172, 173, 177,
    ];
    TABLE.chars().zip(IDS.iter().copied()).collect()
}

/// The breath after a clause: a 70 ms studio micro-pause after the film's beats ("Sir,", "Right,",
/// "Checking.", "On it, Sir.") and at a comma or a dash, 140 ms at a full stop (and ! ? …), 30 ms
/// where a long sentence was cut without punctuation.
fn breath_ms(text: &str) -> usize {
    if let Some(ms) = jai_voice_dsp::breath_cue_ms(text) {
        return ms;
    }
    match text.trim_end().chars().last() {
        Some('.' | '!' | '?' | '…') => 140,
        Some(',' | ';' | ':' | '—' | '–') => 70,
        _ => 30,
    }
}

/// Trims silence, runs the film mastering chain (`jai_voice_dsp::master`: 24 kHz → 48 kHz with
/// the −0.35 st pitch lock, the Bettany EQ and de-esser, the helmet micro-double, the broadcast
/// compressor and limiter), applies 25 ms raised-cosine fades and appends the clause's breath.
/// Returns `jai_voice_dsp::MASTER_RATE` audio.
pub(crate) fn finish_clause_audio(mut audio: Vec<f32>, text: &str) -> Vec<f32> {
    let thresh = 0.003;
    let first = audio.iter().position(|s| s.abs() > thresh).unwrap_or(0);
    let last = audio.iter().rposition(|s| s.abs() > thresh).unwrap_or(audio.len().saturating_sub(1));
    let start = first.saturating_sub(360); // keep 15 ms attack
    let end = (last + 960).min(audio.len()); // keep 40 ms decay
    if start < end {
        audio = audio[start..end].to_vec();
    }
    let mut audio = jai_voice_dsp::master(&audio, KOKORO_RATE, jai_voice_dsp::is_question(text));
    let rate = jai_voice_dsp::MASTER_RATE as usize;
    let fade = (rate * jai_voice_dsp::CROSSFADE_MS / 1000).min(audio.len() / 4);
    for (i, s) in audio.iter_mut().take(fade).enumerate() {
        *s *= raised_cosine(i, fade);
    }
    for (i, s) in audio.iter_mut().rev().take(fade).enumerate() {
        *s *= raised_cosine(i, fade);
    }
    audio.extend(std::iter::repeat_n(0.0, rate * breath_ms(text) / 1000));
    audio
}

fn raised_cosine(index: usize, length: usize) -> f32 {
    let t = index as f32 / length.saturating_sub(1).max(1) as f32;
    0.5 - 0.5 * (std::f32::consts::PI * t.clamp(0.0, 1.0)).cos()
}

/// Four-point cubic Hermite interpolation retains waveform curvature when converting the
/// model's 24 kHz PCM to 44.1/48/96 kHz. Lower device rates use the existing anti-alias FIR.
fn resample_voice(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 || to == 0 { return Vec::new(); }
    if from == to { return input.to_vec(); }
    if to < from { return dsp::resample(input, from, to); }
    let count = (input.len() as f64 * to as f64 / from as f64).round() as usize;
    let sample = |i: isize| input[i.clamp(0, input.len() as isize - 1) as usize];
    (0..count).map(|i| {
        let pos = i as f64 * from as f64 / to as f64;
        let k = pos.floor() as isize;
        let t = (pos - k as f64) as f32;
        let (a, b, c, d) = (sample(k - 1), sample(k), sample(k + 1), sample(k + 2));
        let c0 = b;
        let c1 = 0.5 * (c - a);
        let c2 = a - 2.5 * b + 2.0 * c - 0.5 * d;
        let c3 = 0.5 * (d - a) + 1.5 * (b - c);
        dsp::soft_clip(((c3 * t + c2) * t + c1) * t + c0)
    }).collect()
}

/// Keeps the final 25 ms outside the SPSC ring until the next clause arrives (stage 5 of the film
/// chain). Complementary
/// raised-cosine windows overlap actual consecutive PCM; ring memory is never rewritten while
/// cpal reads it. Generation/device changes discard the tail, so barge-in cannot leak old speech.
#[derive(Default)]
struct ClauseJoiner { tail: Vec<f32>, generation: u64, rate: u32 }

impl ClauseJoiner {
    fn push(&mut self, mut audio: Vec<f32>, generation: u64, rate: u32) -> Vec<f32> {
        if generation != self.generation || rate != self.rate { self.tail.clear(); }
        self.generation = generation;
        self.rate = rate;
        let overlap = (rate as usize * jai_voice_dsp::CROSSFADE_MS / 1000).max(1).min(audio.len());
        if !self.tail.is_empty() {
            let n = self.tail.len().min(overlap);
            let prefix = self.tail.len() - n;
            for (i, sample) in audio.iter_mut().take(n).enumerate() {
                let gain = raised_cosine(i, n);
                *sample = self.tail[prefix + i] * (1.0 - gain) + *sample * gain;
            }
            if prefix > 0 { audio.splice(..0, self.tail[..prefix].iter().copied()); }
        }
        let keep = overlap.min(audio.len());
        self.tail = audio.split_off(audio.len() - keep);
        audio
    }

    fn finish(&mut self, generation: u64) -> Vec<f32> {
        if generation != self.generation { return Vec::new(); }
        let mut tail = std::mem::take(&mut self.tail);
        let length = tail.len();
        for (i, sample) in tail.iter_mut().enumerate() { *sample *= 1.0 - raised_cosine(i, length); }
        tail
    }
}

// ---------------------------------------------------------------------------------------------
// Audio output
// ---------------------------------------------------------------------------------------------

/// Loudness of what JAI is saying right now (RMS of the latest output block, f32 bits): the
/// HUD's node core vibrates with his voice.
static OUTPUT_LEVEL: AtomicU32 = AtomicU32::new(0);
/// The same for the Discord copy of his voice (the HUD's routing meter).
static MIRROR_LEVEL: AtomicU32 = AtomicU32::new(0);

/// JAI's voice in a Discord call as his own account (`discord_bot`): every clause he renders
/// (48 kHz mono), and `None` when he is interrupted (the call drops what is still queued).
pub type VoiceTap = Box<dyn Fn(Option<&[f32]>) + Send + Sync>;
static VOICE_TAP: Mutex<Option<VoiceTap>> = Mutex::new(None);

/// Sir is in that call himself: he hears JAI through Discord, so the speaker at the PC plays
/// silence of the same length (no second copy of his voice a moment later).
static TAP_EXCLUSIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Where his voice goes besides the speaker (None: nowhere else).
pub fn set_voice_tap(tap: Option<VoiceTap>) {
    let mut slot = VOICE_TAP.lock().unwrap_or_else(|e| e.into_inner());
    if tap.is_none() {
        TAP_EXCLUSIVE.store(false, Ordering::Release);
    }
    *slot = tap;
}

pub fn set_voice_exclusive(on: bool) {
    TAP_EXCLUSIVE.store(on, Ordering::Release);
}

fn voice_exclusive() -> bool {
    TAP_EXCLUSIVE.load(Ordering::Acquire) && VOICE_TAP.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

fn tap_voice(clause: Option<&[f32]>) {
    if let Some(tap) = VOICE_TAP.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        tap(clause);
    }
}

/// The speaker path for the HUD board's earcons (set when the controller starts).
static EARCONS: std::sync::OnceLock<(Arc<Output>, Arc<AudioControls>)> = std::sync::OnceLock::new();

/// Plays a HUD earcon (report chime, confirmation tone, power-down) straight into the speaker
/// buffer, like the listening chime: no synthesis and no turn. The microphone treats it as
/// JAI's own sound.
pub fn play_earcon(kind: jai_voice_dsp::Earcon) {
    let Some((output, controls)) = EARCONS.get() else { return };
    let rate = output.device_rate.load(Ordering::Acquire).max(8_000);
    let samples = jai_voice_dsp::earcon(kind, rate);
    controls.guard_echo((samples.len() as u64 * 1000) / rate as u64);
    if let Some(producer) = output.producer.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        producer.push_slice(&samples);
    }
}

/// The board's sound cues are on (`interface.sound_cues`).
static CUES_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// The last cue and when it played.
static LAST_CUE: Mutex<Option<(jai_voice_dsp::Earcon, std::time::Instant)>> = Mutex::new(None);
/// The same cue is not repeated inside this window (twelve blocks arriving at once chime once).
const CUE_SPACING: Duration = Duration::from_millis(160);

pub fn set_cues(on: bool) {
    CUES_ON.store(on, Ordering::Release);
}

/// Whether `kind` may sound now: cues are on and it is not the cue that just played.
fn cue_due(kind: jai_voice_dsp::Earcon, now: std::time::Instant) -> bool {
    if !CUES_ON.load(Ordering::Acquire) {
        return false;
    }
    let mut last = LAST_CUE.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|(known, at)| known == kind && now.saturating_duration_since(at) < CUE_SPACING) {
        return false;
    }
    *last = Some((kind, now));
    true
}

/// His tempo at night, against the day's.
const NIGHT_TEMPO: f32 = 0.94;

/// A clause's tempo by its tone: a warning a little slower and weightier, good news a touch
/// brighter, anything else as it is. Cue words only; "no errors" is no warning.
fn tone_tempo(clause: &str) -> f32 {
    static WARNING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:warning|careful|critical|danger|overheating|too hot|failed|battery (?:is )?low|achtung|vorsicht|warnung|kritisch|fehlgeschlagen)\b").expect("valid tone pattern"));
    static GOOD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:good news|great news|all done|all set|well done|congratulations|geschafft|gute nachrichten|glückwunsch)\b").expect("valid tone pattern"));
    static NO_HARM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:no|nothing|never|without|kein|keine|nichts)\b[^.!?]{0,20}\b(?:warning|critical|danger|failed|warnung|kritisch|fehlgeschlagen)").expect("valid tone pattern"));
    if WARNING.is_match(clause) && !NO_HARM.is_match(clause) {
        0.95
    } else if GOOD.is_match(clause) {
        1.03
    } else {
        1.0
    }
}

/// A HUD micro-interaction's cue (a block materialising, the spotlight, a mode change, an
/// alert): played like an earcon, unless Sir turned the cues off or it has just sounded.
pub fn play_cue(kind: jai_voice_dsp::Earcon) {
    // At night the board makes no sound (the edge light and the cards say it).
    if crate::hud_feel::night_active() {
        return;
    }
    if cue_due(kind, std::time::Instant::now()) {
        play_earcon(kind);
    }
}

pub fn output_level() -> f32 {
    f32::from_bits(OUTPUT_LEVEL.load(Ordering::Relaxed))
}

/// Loudness of the copy of his voice going into the Discord call right now.
pub fn mirror_level() -> f32 {
    f32::from_bits(MIRROR_LEVEL.load(Ordering::Relaxed))
}

pub fn list_output_devices() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

struct Output {
    producer: Mutex<Option<HeapProducer<f32>>>,
    device_rate: AtomicU32,
    flush: Arc<AtomicBool>,
    volume: Arc<AtomicU8>,
    switch: AtomicBool,
    failed: Arc<AtomicBool>,
    requested: Mutex<Option<String>>,
    active: Mutex<String>,
    /// The Discord copy of his voice: open only on the device asked for (never the default
    /// speaker), closed while none is.
    mirror: bool,
    level: &'static AtomicU32,
}

impl Output {
    fn new(requested: Option<String>, volume: Arc<AtomicU8>, mirror: bool) -> Self {
        Self {
            producer: Mutex::new(None),
            device_rate: AtomicU32::new(48_000),
            flush: Arc::new(AtomicBool::new(false)),
            volume,
            switch: AtomicBool::new(false),
            failed: Arc::new(AtomicBool::new(false)),
            requested: Mutex::new(requested),
            active: Mutex::new("—".into()),
            mirror,
            level: if mirror { &MIRROR_LEVEL } else { &OUTPUT_LEVEL },
        }
    }

    /// A clause (at the master rate) into this output at its device's rate, without waiting
    /// for room: the Discord copy plays alongside the speaker.
    fn push_clause(&self, audio48: &[f32]) {
        if let Some(p) = self.producer.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            p.push_slice(&resample_voice(audio48, jai_voice_dsp::MASTER_RATE, self.device_rate.load(Ordering::Acquire)));
        }
    }

    fn buffered(&self) -> usize {
        self.producer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|p| p.len())
            .unwrap_or(0)
    }
}

/// The output named `pref` (exactly, else the first whose name contains it).
fn find_output_device(pref: &str) -> Option<(cpal::Device, String)> {
    let p = pref.to_lowercase();
    let devices: Vec<(cpal::Device, String)> = cpal::default_host()
        .output_devices()
        .map(|it| it.filter_map(|d| d.name().ok().map(|n| (d, n))).collect())
        .unwrap_or_default();
    let hit = devices
        .iter()
        .position(|(_, n)| n.to_lowercase() == p)
        .or_else(|| devices.iter().position(|(_, n)| n.to_lowercase().contains(&p)));
    devices.into_iter().nth(hit?)
}

fn select_output_device(preferred: Option<&str>) -> Option<(cpal::Device, String)> {
    let host = cpal::default_host();
    if let Some(pref) = preferred {
        if let Some(hit) = find_output_device(pref) {
            return Some(hit);
        }
        warn!("[TTS] Preferred speaker '{pref}' not found; using the system default output.");
    }
    host.default_output_device().map(|d| {
        let n = d.name().unwrap_or_else(|_| "Default output".into());
        (d, n)
    })
}

/// A stop under way at the speaker: `left` of `total` frames of the fade to go, then the `drop`
/// frames of the old speech still queued behind them are skipped (speech queued after the stop
/// stays).
#[derive(Clone, Copy, Debug, PartialEq)]
struct SoftStop {
    left: usize,
    total: usize,
    drop: usize,
}

impl SoftStop {
    /// `queued` frames were waiting when the stop came; the fade takes `fade` of them.
    fn new(queued: usize, fade: usize) -> Option<Self> {
        let total = fade.min(queued);
        (total > 0).then_some(SoftStop { left: total, total, drop: queued - total })
    }

    /// The next frame's gain, and the stop after it (None: the fade is over).
    fn step(self) -> (f32, Option<Self>) {
        (self.left as f32 / self.total as f32, (self.left > 1).then_some(SoftStop { left: self.left - 1, ..self }))
    }
}

fn build_output<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut consumer: HeapConsumer<f32>,
    flush: Arc<AtomicBool>,
    volume: Arc<AtomicU8>,
    failed: Arc<AtomicBool>,
    level: &'static AtomicU32,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = config.channels.max(1) as usize;
    // A stop fades what is playing out over 50 ms (a hard cut clicks in a headset).
    let fade = (config.sample_rate.0 as usize / 20).max(1);
    let mut fading: Option<SoftStop> = None;
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            if flush.swap(false, Ordering::AcqRel) {
                fading = SoftStop::new(consumer.len(), fade);
            }
            // (At night he speaks more quietly.)
            let gain = volume.load(Ordering::Relaxed) as f32 / 100.0 * crate::hud_feel::night_gain();
            let mut energy = 0.0f32;
            let mut frames = 0usize;
            for frame in data.chunks_mut(channels) {
                let mut s = consumer.pop().map(|v| dsp::soft_clip(v * gain)).unwrap_or(0.0);
                if let Some(stop) = fading {
                    let (fade_gain, next) = stop.step();
                    s *= fade_gain;
                    if next.is_none() {
                        consumer.skip(stop.drop);
                    }
                    fading = next;
                }
                energy += s * s;
                frames += 1;
                let v = T::from_sample(s);
                for (i, out) in frame.iter_mut().enumerate() {
                    // Front L/R only; extra channels (5.1/7.1 headsets) stay silent.
                    *out = if i < 2 { v } else { T::EQUILIBRIUM };
                }
            }
            level.store((energy / frames.max(1) as f32).sqrt().to_bits(), Ordering::Relaxed);
        },
        move |err| {
            warn!("[TTS] Output stream error: {err}");
            failed.store(true, Ordering::Release);
        },
        None,
    )
}

fn speaker_thread(out: Arc<Output>) {
    loop {
        let preferred = out.requested.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let selected = if out.mirror { preferred.as_deref().and_then(find_output_device) } else { select_output_device(preferred.as_deref()) };
        if out.mirror && selected.is_none() {
            // The Discord copy: only on the device asked for; idle until one is (or it appears).
            *out.active.lock().unwrap_or_else(|e| e.into_inner()) = "—".into();
            MIRROR_LEVEL.store(0, Ordering::Relaxed);
            let since = Instant::now();
            while !out.switch.swap(false, Ordering::AcqRel) && (preferred.is_none() || since.elapsed() < Duration::from_secs(5)) {
                std::thread::sleep(Duration::from_millis(100));
            }
            continue;
        }
        let Some((device, name)) = selected else {
            warn!("[TTS] No audio output device; retrying in 2 s.");
            std::thread::sleep(Duration::from_secs(2));
            continue;
        };
        let supported = match device.default_output_config() {
            Ok(c) => c,
            Err(e) => {
                warn!("[TTS] Output device '{name}' unusable: {e}");
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let rate = supported.sample_rate().0;
        let channels = supported.channels();
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let (producer, consumer) = HeapRb::<f32>::new(rate as usize * 30).split();
        let (flush, volume, failed, level) = (Arc::clone(&out.flush), Arc::clone(&out.volume), Arc::clone(&out.failed), out.level);
        let stream = match format {
            SampleFormat::F32 => build_output::<f32>(&device, &config, consumer, flush, volume, failed, level),
            SampleFormat::I16 => build_output::<i16>(&device, &config, consumer, flush, volume, failed, level),
            SampleFormat::U16 => build_output::<u16>(&device, &config, consumer, flush, volume, failed, level),
            SampleFormat::I32 => build_output::<i32>(&device, &config, consumer, flush, volume, failed, level),
            SampleFormat::F64 => build_output::<f64>(&device, &config, consumer, flush, volume, failed, level),
            other => {
                warn!("[TTS] Unsupported output sample format {other:?} on '{name}'");
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                warn!("[TTS] Could not open output on '{name}': {e}");
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        if let Err(e) = stream.play() {
            warn!("[TTS] Could not start output on '{name}': {e}");
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        out.device_rate.store(rate, Ordering::Release);
        *out.producer.lock().unwrap_or_else(|e| e.into_inner()) = Some(producer);
        *out.active.lock().unwrap_or_else(|e| e.into_inner()) = name.clone();
        out.failed.store(false, Ordering::Release);
        info!("[TTS] {} output: '{name}' ({rate} Hz, {channels} ch, {format:?})", if out.mirror { "Discord voice" } else { "Speaker" });

        while !out.switch.swap(false, Ordering::AcqRel) {
            if out.failed.swap(false, Ordering::AcqRel) {
                warn!("[TTS] Output stream failed (device removed?); reopening.");
                std::thread::sleep(Duration::from_millis(500));
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        *out.producer.lock().unwrap_or_else(|e| e.into_inner()) = None;
        drop(stream);
    }
}

// ---------------------------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TtsEvent {
    /// First audio of a turn entered the speaker buffer.
    PlaybackStarted { generation: u64, synth_ms: u64 },
    /// Everything queued for the turn has been played.
    PlaybackFinished { generation: u64 },
}

enum Job {
    /// `released`: when the reply let the clause go (the gap to its first sound is measured).
    Clause { text: String, generation: u64, released: Instant },
    Finish { generation: u64 },
}

/// The most a reply's first clause should wait for its first sound.
pub const FIRST_SOUND_BUDGET: Duration = Duration::from_millis(250);
/// The last replies' gaps from first clause to first sound (ms), newest last.
static FIRST_SOUNDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn note_first_sound(gap: Duration) {
    let ms = gap.as_millis().min(u32::MAX as u128) as u32;
    let mut kept = FIRST_SOUNDS.lock().unwrap_or_else(|e| e.into_inner());
    if kept.len() >= 20 {
        kept.remove(0);
    }
    kept.push(ms);
    tracing::debug!("[TTS] First clause to first sound {ms} ms (budget {} ms)", FIRST_SOUND_BUDGET.as_millis());
}

/// The median gap from a reply's first clause to its first sound over the last replies (ms),
/// and how many of them missed `FIRST_SOUND_BUDGET`.
pub fn first_sound() -> Option<(u32, usize)> {
    let mut kept = FIRST_SOUNDS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if kept.is_empty() {
        return None;
    }
    kept.sort_unstable();
    Some((kept[kept.len() / 2], kept.iter().filter(|ms| u128::from(**ms) > FIRST_SOUND_BUDGET.as_millis()).count()))
}

/// The reply being spoken: its clauses, each with when its sound ends (None: not yet played).
static SAID: Mutex<(u64, Vec<(String, Option<Instant>)>)> = Mutex::new((0, Vec::new()));
/// What a stop cut off, and when ("go on" says it again: `cut_off`).
static CUT: Mutex<Option<(Instant, String)>> = Mutex::new(None);

/// The text a stop at `now` cut off: from the start of the sentence that was playing to the end
/// of the reply. None when everything had been heard.
fn cut_from(clauses: &[(String, Option<Instant>)], now: Instant) -> Option<String> {
    let mut first = clauses.iter().position(|(_, end)| end.is_none_or(|end| end > now))?;
    while first > 0 && !clauses[first - 1].0.trim_end().ends_with(['.', '!', '?', '…']) {
        first -= 1;
    }
    Some(clauses[first..].iter().map(|(text, _)| text.trim()).collect::<Vec<_>>().join(" "))
}

/// What he was cut off saying within the last `within`, taken (said once).
pub fn cut_off(within: Duration) -> Option<String> {
    let mut cut = CUT.lock().unwrap_or_else(|e| e.into_inner());
    cut.take_if(|(at, _)| at.elapsed() <= within).map(|(_, text)| text)
}

struct Inner {
    jobs: mpsc::Sender<Job>,
    generation: AtomicU64,
    output: Arc<Output>,
    /// The copy of his voice for Sir's Discord call (`set_mirror_device`).
    mirror: Arc<Output>,
    controls: Arc<AudioControls>,
    voice: RwLock<String>,
    voices: RwLock<Vec<String>>,
    status: RwLock<String>,
    /// Speaking rate (f32 bits), changed live ("speak faster").
    speed: Arc<AtomicU32>,
    /// The last clause queued (generation, normalised words): the same clause twice in a row
    /// within one reply is spoken once.
    last_clause: Mutex<(u64, String)>,
}

/// Lower-case words only, for comparing clauses.
fn clause_key(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone)]
pub struct TtsController {
    inner: Arc<Inner>,
}

impl TtsController {
    pub fn start(cfg: &JaiConfig, controls: Arc<AudioControls>, events: UnboundedSender<TtsEvent>) -> Self {
        let output = Arc::new(Output::new(cfg.preferred_speaker.clone(), Arc::new(AtomicU8::new(cfg.loudness_pct.min(200))), false));
        // Same loudness; no device until the Discord bridge (or the HUD's test) asks for one.
        let mirror = Arc::new(Output::new(None, Arc::clone(&output.volume), true));
        for (name, out) in [("jai-speaker", &output), ("jai-discord-voice", &mirror)] {
            let out_thread = Arc::clone(out);
            if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(move || crate::crash_guard::supervise(name, || speaker_thread(Arc::clone(&out_thread)))) {
                error!("[TTS] Could not start {name} thread: {e}");
            }
        }

        let _ = EARCONS.set((Arc::clone(&output), Arc::clone(&controls)));
        let (jobs, rx) = mpsc::channel::<Job>();
        let inner = Arc::new(Inner {
            jobs,
            generation: AtomicU64::new(1),
            output,
            mirror,
            controls,
            voice: RwLock::new(cfg.kokoro_voice.clone()),
            voices: RwLock::new(vec![cfg.kokoro_voice.clone()]),
            status: RwLock::new("loading".into()),
            speed: Arc::new(AtomicU32::new(cfg.kokoro_speed.clamp(0.5, 2.0).to_bits())),
            last_clause: Mutex::new((0, String::new())),
        });
        let worker_inner = Arc::clone(&inner);
        let cfg = cfg.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("jai-tts".into())
            .spawn(move || crate::crash_guard::supervise("jai-tts", || synth_worker(cfg.clone(), Arc::clone(&worker_inner), &rx, events.clone())))
        {
            error!("[TTS] Could not start synthesis thread: {e}");
        }
        Self { inner }
    }

    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    /// His voice is playing right now.
    pub fn is_speaking(&self) -> bool {
        self.inner.controls.is_speaking.load(Ordering::Acquire)
    }

    /// Barge-in / interrupt: drops everything queued or playing. Returns the new generation.
    pub fn stop(&self) -> u64 {
        let said = SAID.lock().unwrap_or_else(|e| e.into_inner()).1.clone();
        if let Some(text) = cut_from(&said, Instant::now()) {
            *CUT.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), text));
        }
        let next = self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.inner.output.flush.store(true, Ordering::Release);
        self.inner.mirror.flush.store(true, Ordering::Release);
        tap_voice(None);
        if self.inner.controls.is_speaking.load(Ordering::Acquire) {
            self.inner.controls.set_speaking(false);
        }
        next
    }

    pub fn enqueue_clause(&self, text: String, generation: u64) {
        let text = sanitize_for_display_and_speech(&text);
        if !text.chars().any(char::is_alphanumeric) || generation != self.generation() {
            return;
        }
        // Anti-repetition safety net: an acknowledgement and an LLM opener that say the same
        // thing back to back ("Right away, Sir. Right away, Sir.") are heard once.
        let key = clause_key(&text);
        {
            let mut last = self.inner.last_clause.lock().unwrap_or_else(|e| e.into_inner());
            if last.0 == generation && !key.is_empty() && last.1 == key {
                tracing::debug!("[TTS] Skipped a clause repeated back to back: '{text}'");
                return;
            }
            *last = (generation, key);
        }
        {
            let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
            if said.0 != generation {
                *said = (generation, Vec::new());
            }
            said.1.push((text.clone(), None));
        }
        let _ = self.inner.jobs.send(Job::Clause { text, generation, released: Instant::now() });
    }

    /// Splits a complete text into clauses and queues them.
    pub fn speak(&self, text: &str, generation: u64) {
        let mut splitter = ClauseSplitter::for_text();
        let mut clauses = splitter.push(text);
        clauses.extend(splitter.flush());
        for c in clauses {
            self.enqueue_clause(c, generation);
        }
    }

    /// Marks the end of a turn; `PlaybackFinished` fires once the audio has drained.
    pub fn finish(&self, generation: u64) {
        let _ = self.inner.jobs.send(Job::Finish { generation });
    }

    /// A soft two-note chime (the "I'm listening" cue after a bare "JAI" or a double clap),
    /// written straight into the speaker buffer: no synthesis and no turn. The microphone treats
    /// it, plus the usual echo tail, as JAI's own sound.
    pub fn chime(&self) {
        let rate = self.inner.output.device_rate.load(Ordering::Acquire).max(8_000) as f32;
        let mut samples: Vec<f32> = Vec::with_capacity((rate * 0.2) as usize);
        for (freq, secs) in [(1046.5f32, 0.075f32), (1568.0, 0.11)] {
            let n = (rate * secs) as usize;
            for i in 0..n {
                let t = i as f32 / rate;
                let attack = (i as f32 / (rate * 0.006)).min(1.0);
                let decay = (-t * 22.0).exp();
                samples.push((std::f32::consts::TAU * freq * t).sin() * 0.07 * attack * decay);
            }
        }
        let ms = (samples.len() as f32 / rate * 1000.0) as u64;
        self.inner.controls.guard_echo(ms);
        if let Some(producer) = self.inner.output.producer.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            producer.push_slice(&samples);
        }
    }

    pub fn set_voice(&self, voice: &str) {
        *self.inner.voice.write().unwrap_or_else(|e| e.into_inner()) = voice.trim().to_string();
    }

    pub fn voice(&self) -> String {
        self.inner.voice.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn voices(&self) -> Vec<String> {
        self.inner.voices.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn status(&self) -> String {
        self.inner.status.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn speed_handle(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.inner.speed)
    }

    pub fn volume_handle(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.inner.output.volume)
    }

    pub fn volume(&self) -> u8 {
        self.inner.output.volume.load(Ordering::Relaxed)
    }

    pub fn set_volume(&self, pct: u8) {
        self.inner.output.volume.store(pct.min(200), Ordering::Relaxed);
    }

    pub fn set_output_device(&self, name: Option<String>) {
        let name = name.filter(|n| !n.trim().is_empty() && !n.eq_ignore_ascii_case("auto"));
        *self.inner.output.requested.lock().unwrap_or_else(|e| e.into_inner()) = name;
        self.inner.output.switch.store(true, Ordering::Release);
    }

    /// Also plays his voice on `name` (the output that reaches Sir's Discord call; resampled to
    /// that device), or stops doing so. The normal speaker keeps playing either way.
    pub fn set_mirror_device(&self, name: Option<String>) {
        let name = name.filter(|n| !n.trim().is_empty());
        *self.inner.mirror.requested.lock().unwrap_or_else(|e| e.into_inner()) = name;
        self.inner.mirror.switch.store(true, Ordering::Release);
    }

    /// The device the Discord copy of his voice plays on ("—" while there is none).
    pub fn mirror_device(&self) -> String {
        self.inner.mirror.active.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn requested_device(&self) -> Option<String> {
        self.inner.output.requested.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn active_device(&self) -> String {
        self.inner.output.active.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

enum Engine {
    Kokoro(Box<Kokoro>),
    /// Windows SAPI voices (emergency fallback when Kokoro cannot load).
    Sapi,
    None,
}

/// Openers JAI uses constantly (wake acknowledgements, fast-path confirmations, typical LLM
/// first clauses). They are rendered while idle and served from the cache, so the first audio
/// of most turns needs no synthesis at all.
const WARM_PHRASES: &[&str] = &[
    "Yes, Sir?", "At your service, Sir.", "Sir?", "Right away, Sir.", "Of course, Sir.", "Certainly, Sir.",
    "Very good, Sir.", "One moment, Sir.", "Let me check, Sir.", "Checking now, Sir.", "On it, Sir.",
    "Done, Sir.", "Working on it, Sir.", "Leave it with me, Sir.", "Sound restored, Sir.", "Is this better, Sir?",
    "Good day, Sir.", "All systems are online.", "There you are, Sir.", "I'm afraid not, Sir.",
    "Noted, Sir.", "Understood, Sir.", "Consider it forgotten, Sir.", "As you wish, Sir.", "Powering down.",
    "Shutting down now. Goodbye, Sir.", "All secondary windows have been closed.",
    "I'm back online, Sir.",
    // The rotating acknowledgements of the anti-repetition engine, and the Stark systems' lines.
    "On it.", "At once.", "Consider it done, Sir.", "Straight away, Sir.", "Reply sent, Sir.", "Welcome home, Sir.",
    "Checking the sky above you, Sir.", "Nothing new since my last answer, Sir.",
    // Executive-initiative openers and the interface/calendar confirmations.
    "Excellent choice, Sir.", "Allow me, Sir.", "Yes, Sir.", "Indeed, Sir.", "Very well, Sir.", "Absolutely, Sir.",
];
const CACHE_LIMIT: usize = 160;

/// Rendered (48 kHz, mastered) clause audio keyed by voice + speed + text.
struct ClauseCache {
    map: HashMap<String, Arc<Vec<f32>>>,
    order: std::collections::VecDeque<String>,
}

impl ClauseCache {
    fn key(voice: &str, speed: f32, speech: &str) -> String {
        format!("{voice}|{speed:.2}|{}", speech.trim().to_lowercase())
    }

    fn cacheable(speech: &str) -> bool {
        speech.split_whitespace().count() <= 8
    }

    fn get(&self, key: &str) -> Option<Arc<Vec<f32>>> {
        self.map.get(key).cloned()
    }

    fn insert(&mut self, key: String, audio: Arc<Vec<f32>>) {
        if self.map.insert(key.clone(), audio).is_none() {
            self.order.push_back(key);
            while self.order.len() > CACHE_LIMIT {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }

    /// The common phrases as rendered last time, kept beside the model: a start (one during a
    /// game too) plays them at once instead of synthesising them all again. Their first entry
    /// is the build and voice that rendered them (`phrase_stamp`): another's may sound different.
    fn load(&mut self, path: &Path, build: &str) -> usize {
        let bytes = std::fs::read(path).unwrap_or_default();
        let mut rest = bytes.as_slice();
        let mut kept = Vec::new();
        while let Some((key, audio, next)) = next_phrase(rest) {
            kept.push((key, audio));
            rest = next;
        }
        if kept.first().is_none_or(|(key, _)| key != build) {
            return 0;
        }
        let n = kept.len() - 1;
        for (key, audio) in kept.into_iter().skip(1) {
            self.insert(key, Arc::new(audio));
        }
        n
    }

    fn save(&self, path: &Path, build: &str, keys: &[String]) -> std::io::Result<()> {
        let mut out = Vec::new();
        let kept = keys.iter().filter_map(|k| Some((k.as_str(), self.map.get(k)?.as_slice())));
        for (key, audio) in std::iter::once((build, &[][..])).chain(kept) {
            out.extend((key.len() as u32).to_le_bytes());
            out.extend(key.as_bytes());
            out.extend((audio.len() as u32).to_le_bytes());
            out.extend(audio.iter().flat_map(|s| s.to_le_bytes()));
        }
        let part = path.with_extension("part");
        std::fs::write(&part, out)?;
        std::fs::rename(part, path)
    }
}

/// One phrase of the phrase file: its key, its samples and what follows.
fn next_phrase(b: &[u8]) -> Option<(String, Vec<f32>, &[u8])> {
    let n = u32::from_le_bytes(b.get(..4)?.try_into().ok()?) as usize;
    let key = String::from_utf8(b.get(4..4 + n)?.to_vec()).ok()?;
    let b = &b[4 + n..];
    let m = u32::from_le_bytes(b.get(..4)?.try_into().ok()?) as usize * 4;
    let audio = b.get(4..4 + m)?.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    Some((key, audio, &b[4 + m..]))
}

/// This build, as its exe's size and time: the phrase file of another build is not used.
fn build_stamp() -> String {
    std::env::current_exe().and_then(std::fs::metadata).map(|m| format!("{} {:?}", m.len(), m.modified().ok())).unwrap_or_default()
}

/// The phrases' stamp: the build, model and voice (flattening, calibration, clip) that rendered them.
fn phrase_stamp(build: &str, cfg: &JaiConfig, voice: Option<&crate::fish_zero_shot_tts::ReferenceProfile>) -> String {
    format!("{build} {} {} {:?}", cfg.kokoro_model_path, cfg.voice_f0_flatten, voice.map(|p| (&p.blend, p.flatten, p.source)))
}

/// Phonemise + synthesise + post-process one clause (cache-aware).
fn render_clause(
    engine: &mut Engine,
    phonemizer: &Phonemizer,
    cache: &mut ClauseCache,
    speech: &str,
    voice: &str,
    speed: f32,
) -> Option<Arc<Vec<f32>>> {
    let cacheable = ClauseCache::cacheable(speech);
    let key = ClauseCache::key(voice, speed, speech);
    if cacheable {
        if let Some(hit) = cache.get(&key) {
            return Some(hit);
        }
    }
    crate::mind_activity::chain(&["pronunciation_lexicon.rs", "phonemizer.rs", "tts.rs", "jai_voice_dsp.rs", "fish_zero_shot_tts.rs"]);
    let audio = match engine {
        Engine::Kokoro(k) => {
            // "Sir, the report's done.": the vocative gets its own 85 ms breath.
            let parts = match jai_voice_dsp::split_leading_cue(speech) {
                Some((head, rest)) => vec![head, rest],
                None => vec![speech.to_string()],
            };
            let mut audio = Vec::new();
            for part in parts {
                let ipa = phonemizer.phonemize(&part);
                match k.synthesize(&ipa, voice, jai_voice_dsp::synthesis_speed(speed)) {
                    Ok(a) if !a.is_empty() => audio.extend(finish_clause_audio(a, &part)),
                    Ok(_) => {}
                    Err(e) => {
                        warn!("[TTS] Synthesis failed for '{part}': {e}");
                        return None;
                    }
                }
            }
            audio
        }
        Engine::Sapi => resample_voice(&sapi_synthesize(speech), KOKORO_RATE, jai_voice_dsp::MASTER_RATE),
        Engine::None => return None,
    };
    if audio.is_empty() {
        return None;
    }
    let audio = Arc::new(audio);
    if cacheable {
        cache.insert(key, Arc::clone(&audio));
    }
    Some(audio)
}

/// Re-creates Kokoro after the zero-VRAM standby.
fn reload_kokoro(cfg: &JaiConfig, engine: &mut Engine, unloaded: &mut bool) {
    let t0 = Instant::now();
    match Kokoro::load(cfg) {
        Ok(k) => {
            *engine = Engine::Kokoro(Box::new(k));
            *unloaded = false;
            crate::vram_guard::report_tts(true);
            info!("[TTS] Kokoro re-loaded in {} ms.", t0.elapsed().as_millis());
        }
        Err(e) => warn!("[TTS] Kokoro could not be re-loaded: {e}"),
    }
}

fn synth_worker(cfg: JaiConfig, inner: Arc<Inner>, rx: &mpsc::Receiver<Job>, events: UnboundedSender<TtsEvent>) {
    let phonemizer = Phonemizer::new(cfg.espeak_path.as_deref());
    let mut engine = match Kokoro::load(&cfg) {
        Ok(k) => Engine::Kokoro(Box::new(k)),
        Err(e) => {
            error!("[TTS] Kokoro unavailable: {e}");
            if cfg!(windows) {
                warn!("[TTS] Falling back to Windows SAPI voices.");
                Engine::Sapi
            } else {
                Engine::None
            }
        }
    };
    if let Engine::Kokoro(k) = &mut engine {
        let voices = k.available_voices();
        if !voices.is_empty() {
            *inner.voices.write().unwrap_or_else(|e| e.into_inner()) = voices;
        }
        let t0 = Instant::now();
        let voice = inner.voice.read().unwrap_or_else(|e| e.into_inner()).clone();
        match k.synthesize(&phonemizer.phonemize("Sir."), &voice, cfg.kokoro_speed) {
            Ok(_) => info!("[TTS] Kokoro warm-up {} ms (G2P: {})", t0.elapsed().as_millis(), phonemizer.backend_name()),
            Err(e) => warn!("[TTS] Kokoro warm-up failed for voice '{voice}': {e}"),
        }
        crate::vram_guard::report_tts(true);
    }
    *inner.status.write().unwrap_or_else(|e| e.into_inner()) = match &engine {
        Engine::Kokoro(_) => format!("kokoro + {}", phonemizer.backend_name()),
        Engine::Sapi => "windows sapi (fallback)".into(),
        Engine::None => "unavailable".into(),
    };

    let mut cache = ClauseCache { map: HashMap::new(), order: std::collections::VecDeque::new() };
    let (phrase_file, build) = (Path::new(&cfg.kokoro_model_path).with_file_name("jai_phrases.cache"), phrase_stamp(&build_stamp(), &cfg, crate::fish_zero_shot_tts::profile()));
    let (mut warm_keys, mut warm_rendered) = (Vec::new(), false);
    let mut warm_voice = String::new();
    let mut warm_next = 0usize;
    let mut started_generation = 0u64;
    // When the sound queued so far will have played (for what a stop cuts off: `SAID`).
    let mut plays_until = Instant::now();
    let mut joiner = ClauseJoiner::default();
    // Zero-VRAM standby: Kokoro is dropped (`Engine::None`, its ONNX session and any DirectML
    // buffers freed) and re-created on the next clause or wake-up.
    let mut unloaded = false;
    let mut last_job = Instant::now();
    let mut epoch = crate::vram_guard::flush_epoch();
    let mut retry_at: Option<Instant> = None;
    loop {
        let job = match rx.recv_timeout(Duration::from_millis(150)) {
            Ok(job) => {
                last_job = Instant::now();
                if unloaded && matches!(job, Job::Clause { .. }) {
                    reload_kokoro(&cfg, &mut engine, &mut unloaded);
                }
                job
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let kokoro = matches!(engine, Engine::Kokoro(_));
                if kokoro || unloaded {
                    match crate::vram_guard::worker_action(kokoro, &mut epoch, last_job, retry_at) {
                        crate::vram_guard::WorkerAction::Unload => {
                            engine = Engine::None;
                            unloaded = true;
                            cache.map.clear();
                            cache.order.clear();
                            warm_next = 0;
                            crate::vram_guard::report_tts(false);
                            info!("[TTS] Kokoro unloaded (zero-VRAM standby).");
                            continue;
                        }
                        crate::vram_guard::WorkerAction::Reload => {
                            reload_kokoro(&cfg, &mut engine, &mut unloaded);
                            retry_at = unloaded.then(|| Instant::now() + crate::vram_guard::RELOAD_BACKOFF);
                        }
                        crate::vram_guard::WorkerAction::Keep => {}
                    }
                }
                // Idle: pre-render one common phrase per tick for the current voice — never during
                // a turn, where a clause arriving mid-render would wait for it.
                if unloaded || inner.controls.turn_active.load(Ordering::Acquire) {
                    continue;
                }
                let voice = inner.voice.read().unwrap_or_else(|e| e.into_inner()).clone();
                if voice != warm_voice {
                    warm_voice = voice.clone();
                    warm_next = 0;
                }
                let kokoro = matches!(engine, Engine::Kokoro(_));
                if let Some(phrase) = WARM_PHRASES.get(warm_next) {
                    if warm_next == 0 && kokoro {
                        let kept = cache.load(&phrase_file, &build);
                        tracing::debug!("[TTS] {kept} common phrases read from {}", phrase_file.display());
                        (warm_keys, warm_rendered) = (Vec::new(), false);
                    }
                    warm_next += 1;
                    let speech = normalize_for_speech(phrase);
                    // (At the tempo a reply will ask for it: "All done, Sir." is good news.)
                    let speed = f32::from_bits(inner.speed.load(Ordering::Relaxed)) * tone_tempo(phrase);
                    let key = ClauseCache::key(&voice, speed, &speech);
                    warm_rendered |= cache.get(&key).is_none();
                    let _ = render_clause(&mut engine, &phonemizer, &mut cache, &speech, &voice, speed);
                    warm_keys.push(key);
                    if warm_next == WARM_PHRASES.len() {
                        tracing::debug!("[TTS] {} common phrases pre-rendered for '{voice}'", WARM_PHRASES.len());
                        if warm_rendered && kokoro {
                            if let Err(e) = cache.save(&phrase_file, &build, &warm_keys) {
                                warn!("[TTS] The common phrases could not be kept on disk: {e}");
                            }
                        }
                    }
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match job {
            Job::Clause { text, generation, released } => {
                if generation != inner.generation.load(Ordering::Acquire) {
                    continue;
                }
                let t0 = Instant::now();
                let speech = normalize_for_speech(&text);
                let voice = inner.voice.read().unwrap_or_else(|e| e.into_inner()).clone();
                // At night he speaks a little slower, as one does late in the evening.
                let speed = f32::from_bits(inner.speed.load(Ordering::Relaxed)) * if crate::hud_feel::night_active() { NIGHT_TEMPO } else { 1.0 } * tone_tempo(&text);
                let Some(audio48) = render_clause(&mut engine, &phonemizer, &mut cache, &speech, &voice, speed)
                else {
                    continue;
                };
                let synth_ms = t0.elapsed().as_millis() as u64;
                if generation != inner.generation.load(Ordering::Acquire) {
                    continue;
                }
                let rate = inner.output.device_rate.load(Ordering::Acquire);
                inner.mirror.push_clause(&audio48);
                tap_voice(Some(&audio48));
                let mut audio = joiner.push(resample_voice(&audio48, jai_voice_dsp::MASTER_RATE, rate), generation, rate);
                // Sir hears him through the call: the speaker keeps the timing, not the voice.
                if voice_exclusive() {
                    audio.fill(0.0);
                }
                tracing::debug!("[TTS] '{speech}' → {:.2}s audio in {synth_ms} ms", audio48.len() as f32 / jai_voice_dsp::MASTER_RATE as f32);
                plays_until = plays_until.max(Instant::now()) + Duration::from_secs_f32(audio.len() as f32 / rate.max(1) as f32);
                {
                    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
                    if said.0 == generation {
                        if let Some(clause) = said.1.iter_mut().find(|(said, end)| end.is_none() && *said == text) {
                            clause.1 = Some(plays_until);
                        }
                    }
                }

                let mut offset = 0;
                let mut announced = started_generation == generation;
                while offset < audio.len() {
                    if generation != inner.generation.load(Ordering::Acquire) {
                        break;
                    }
                    let pushed = inner
                        .output
                        .producer
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_mut()
                        .map(|p| p.push_slice(&audio[offset..]))
                        .unwrap_or(0);
                    offset += pushed;
                    if pushed > 0 && !announced {
                        announced = true;
                        started_generation = generation;
                        note_first_sound(released.elapsed());
                        inner.controls.set_speaking(true);
                        let _ = events.send(TtsEvent::PlaybackStarted { generation, synth_ms });
                    }
                    if offset < audio.len() {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }
            Job::Finish { generation } => {
                let mut tail = joiner.finish(generation);
                if voice_exclusive() {
                    tail.fill(0.0);
                }
                let mut offset = 0;
                while offset < tail.len() && generation == inner.generation.load(Ordering::Acquire) {
                    offset += inner.output.producer.lock().unwrap_or_else(|e| e.into_inner())
                        .as_mut().map(|p| p.push_slice(&tail[offset..])).unwrap_or(0);
                    if offset < tail.len() { std::thread::sleep(Duration::from_millis(5)); }
                }
                while generation == inner.generation.load(Ordering::Acquire) && inner.output.buffered() > 0 {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if generation == inner.generation.load(Ordering::Acquire) {
                    if inner.controls.is_speaking.load(Ordering::Acquire) {
                        inner.controls.set_speaking(false);
                    }
                    let _ = events.send(TtsEvent::PlaybackFinished { generation });
                }
            }
        }
    }
}

/// Emergency fallback on Linux: the espeak-ng program's own voice (22.05 kHz WAV on stdout, a
/// 44-byte header) at 24 kHz. Nothing when espeak-ng is not installed (sudo pacman -S espeak-ng).
#[cfg(not(windows))]
fn sapi_synthesize(text: &str) -> Vec<f32> {
    let Some(cli) = crate::apps::which("espeak-ng") else { return Vec::new() };
    match crate::shell::run_helper_blocking(&cli, ["-v", "en-gb", "--stdout", text], Duration::from_secs(30)) {
        Ok(out) if out.success() && out.stdout.len() > 44 => {
            let pcm: Vec<f32> = out.stdout[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
            resample_voice(&pcm, 22_050, KOKORO_RATE)
        }
        _ => Vec::new(),
    }
}

/// Emergency fallback when Kokoro cannot load: Windows SAPI rendered to 24 kHz PCM.
#[cfg(windows)]
fn sapi_synthesize(text: &str) -> Vec<f32> {
    let safe = text.replace(['\'', '"', '`', '$'], " ");
    let script = format!(
        "Add-Type -AssemblyName System.Speech; $s=New-Object System.Speech.Synthesis.SpeechSynthesizer; \
         $m=New-Object System.IO.MemoryStream; \
         $f=New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(24000,[System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,[System.Speech.AudioFormat.AudioChannel]::Mono); \
         $s.SetOutputToAudioStream($m,$f); $s.Speak('{safe}'); $b=$m.ToArray(); [Console]::OpenStandardOutput().Write($b,0,$b.Length)"
    );
    match crate::shell::run_helper_blocking(Path::new("powershell"), ["-NoProfile", "-NonInteractive", "-Command", script.as_str()], Duration::from_secs(30)) {
        Ok(out) if out.success() => out
            .stdout
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect(),
        _ => Vec::new(),
    }
}

/// The two-sink fan-out, checked without opening a device (the exhaustive verifier's dual-sink
/// check): one clause goes to the speaker output and to the copy for Sir's call, each at its
/// own device rate and under one shared loudness; a sink that has no device takes nothing and
/// holds nothing up.
pub fn dual_sink_probe() -> Result<String, String> {
    let volume = Arc::new(AtomicU8::new(100));
    let speaker = Output::new(None, Arc::clone(&volume), false);
    let call = Output::new(None, Arc::clone(&volume), true);
    let clause: Vec<f32> = (0..jai_voice_dsp::MASTER_RATE / 2).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
    call.push_clause(&clause);
    if call.buffered() != 0 {
        return Err("a sink without a device queued audio".to_string());
    }
    // The ring buffers a device would drain; the consumers stay alive for the check.
    let mut drains = Vec::new();
    for (out, rate) in [(&speaker, 48_000u32), (&call, 44_100)] {
        let (producer, consumer) = HeapRb::<f32>::new(rate as usize * 4).split();
        *out.producer.lock().unwrap_or_else(|e| e.into_inner()) = Some(producer);
        out.device_rate.store(rate, Ordering::Release);
        drains.push(consumer);
    }
    for out in [&speaker, &call] {
        out.push_clause(&clause);
    }
    let (to_speaker, to_call) = (speaker.buffered() as i64, call.buffered() as i64);
    if (to_speaker - 24_000).abs() > 2 || (to_call - 22_050).abs() > 2 {
        return Err(format!("half a second of speech became {to_speaker} samples at 48 kHz and {to_call} at 44.1 kHz"));
    }
    if !Arc::ptr_eq(&speaker.volume, &call.volume) || !call.mirror || speaker.mirror {
        return Err("the two sinks do not share one loudness".to_string());
    }
    drop(drains);
    Ok(format!("one clause reaches both sinks: {to_speaker} samples at 48 kHz for the speaker, {to_call} at 44.1 kHz for the call, one loudness"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cut off mid-reply, he keeps what was not heard from the start of the sentence that was
    /// playing; a reply heard to its end leaves nothing.
    #[test]
    fn a_stop_keeps_the_rest_from_the_start_of_its_sentence() {
        let now = Instant::now();
        let (past, later) = (now - Duration::from_secs(1), now + Duration::from_secs(1));
        let reply = |ends: [Option<Instant>; 4]| ["Your calendar is clear.", "Tomorrow at nine,", "you have maths,", "then sport."].into_iter().map(str::to_string).zip(ends).collect::<Vec<_>>();
        assert_eq!(cut_from(&reply([Some(past), Some(past), Some(later), None]), now).as_deref(), Some("Tomorrow at nine, you have maths, then sport."));
        assert_eq!(cut_from(&reply([Some(later), None, None, None]), now).as_deref(), Some("Your calendar is clear. Tomorrow at nine, you have maths, then sport."));
        assert_eq!(cut_from(&reply([Some(past); 4]), now), None);
        *CUT.lock().unwrap() = Some((now, "then sport.".into()));
        assert_eq!(cut_off(Duration::from_secs(30)).as_deref(), Some("then sport."));
        assert_eq!(cut_off(Duration::from_secs(30)), None, "said once");
    }

    #[test]
    fn a_warning_is_said_slower_and_good_news_brighter() {
        assert_eq!(tone_tempo("Warning, Sir: the GPU is overheating."), 0.95);
        assert_eq!(tone_tempo("Achtung, der Akku ist fast leer."), 0.95);
        assert_eq!(tone_tempo("Good news, Sir: the build passed."), 1.03);
        assert_eq!(tone_tempo("No errors, nothing failed, all clear."), 1.0, "a warning word denied is no warning");
        assert_eq!(tone_tempo("Your calendar is clear."), 1.0);
    }

    /// A reply's first clause to its first sound: the median of the last twenty, and how many
    /// missed the 250 ms budget.
    #[test]
    fn the_gap_to_the_first_sound_is_kept_against_its_budget() {
        for ms in [120, 180, 900, 200, 240] {
            note_first_sound(Duration::from_millis(ms));
        }
        assert_eq!(first_sound(), Some((200, 1)));
        for _ in 0..30 {
            note_first_sound(Duration::from_millis(100));
        }
        assert_eq!(first_sound(), Some((100, 0)), "only the last twenty count");
    }

    /// Talked over, his voice fades out over 50 ms instead of stopping dead, and only the speech
    /// that was queued then is dropped.
    /// A warm phrase too long to keep was "new" at every start and rewrote the phrase file.
    #[test]
    fn every_warm_phrase_can_be_kept() {
        for phrase in WARM_PHRASES {
            assert!(ClauseCache::cacheable(&normalize_for_speech(phrase)), "{phrase}");
        }
    }

    #[test]
    fn common_phrases_are_kept_on_disk_for_this_build_only() {
        let path = std::env::temp_dir().join(format!("jai_phrases_test_{}.cache", std::process::id()));
        let mut cache = ClauseCache { map: HashMap::new(), order: std::collections::VecDeque::new() };
        let (yes, done) = (ClauseCache::key("bf_fable", 1.0, "Yes, Sir?"), ClauseCache::key("bf_fable", 1.0, "Done, Sir."));
        cache.insert(yes.clone(), Arc::new(vec![0.25, -0.5, 1.0]));
        cache.insert(done.clone(), Arc::new(vec![0.125; 3]));
        cache.save(&path, "build 1", &[yes.clone(), done.clone(), "not rendered".into()]).unwrap();
        let mut fresh = ClauseCache { map: HashMap::new(), order: std::collections::VecDeque::new() };
        assert_eq!(fresh.load(&path, "build 2"), 0, "another build's voice is not used");
        assert_eq!(fresh.load(&path, "build 1"), 2);
        assert_eq!(fresh.get(&yes).as_deref(), Some(&vec![0.25, -0.5, 1.0]));
        assert_eq!(fresh.get(&done).as_deref(), Some(&vec![0.125; 3]));
        // A file cut short keeps what is whole.
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 2]).unwrap();
        let mut cut = ClauseCache { map: HashMap::new(), order: std::collections::VecDeque::new() };
        assert_eq!(cut.load(&path, "build 1"), 1);
        let _ = std::fs::remove_file(&path);
        assert_eq!(cut.load(&path, "build 1"), 0, "no file, nothing kept");
    }

    #[test]
    fn kept_phrases_belong_to_the_voice_and_model_that_rendered_them() {
        use crate::fish_zero_shot_tts::{F0Stats, ReferenceProfile, MEL_BANDS};
        let measured = |blend: &str, modified: u64| ReferenceProfile {
            envelope_db: [-30.0; MEL_BANDS],
            rms_db: -20.0,
            crest_db: 12.0,
            speech_secs: 4.0,
            f0: F0Stats { mean_hz: 110.0, median_hz: 108.0, std_st: 2.0, voiced_ratio: 0.6 },
            blend: Some(blend.to_string()),
            flatten: Some(0.3),
            source: (96_000, modified),
        };
        let cfg = JaiConfig { kokoro_model_path: crate::models::KOKORO_V1_MODEL.to_string(), ..JaiConfig::default() };
        let clip = measured("bm_fable:0.5+bm_lewis:0.5", 1_763_000_000);
        let stamp = phrase_stamp("build 1", &cfg, Some(&clip));
        assert_eq!(stamp, phrase_stamp("build 1", &cfg, Some(&measured("bm_fable:0.5+bm_lewis:0.5", 1_763_000_000))), "the same build and voice keep their phrases");
        assert_ne!(stamp, phrase_stamp("build 2", &cfg, Some(&clip)), "another build");
        assert_ne!(stamp, phrase_stamp("build 1", &cfg, Some(&measured("bm_fable:0.75+bm_george:0.25", 1_763_000_000))), "a new calibration");
        assert_ne!(stamp, phrase_stamp("build 1", &cfg, Some(&measured("bm_fable:0.5+bm_lewis:0.5", 1_763_086_400))), "another clip");
        assert_ne!(stamp, phrase_stamp("build 1", &cfg, None), "no clip");
        let other = JaiConfig { kokoro_model_path: crate::models::KOKORO_V019_MODEL.to_string(), ..cfg.clone() };
        assert_ne!(stamp, phrase_stamp("build 1", &other, Some(&clip)), "another model");
        let other = JaiConfig { voice_f0_flatten: 0.5, ..cfg.clone() };
        assert_ne!(stamp, phrase_stamp("build 1", &other, Some(&clip)), "Sir's own flattening");
    }

    #[test]
    fn a_stop_fades_out_and_drops_only_the_old_speech() {
        assert_eq!(SoftStop::new(0, 2400), None, "nothing playing: nothing to fade");
        let mut stop = SoftStop::new(100_000, 2400);
        assert_eq!(stop.map(|s| s.drop), Some(97_600));
        let mut gains = Vec::new();
        while let Some(s) = stop {
            let (gain, next) = s.step();
            gains.push(gain);
            stop = next;
        }
        assert_eq!(gains.len(), 2400, "50 ms at 48 kHz");
        assert_eq!(gains[0], 1.0);
        assert!(gains.windows(2).all(|w| w[1] < w[0]) && gains[2399] < 0.001, "an even ramp down to silence");
        // A clip shorter than the fade fades over what is left of it.
        assert_eq!(SoftStop::new(300, 2400), Some(SoftStop { left: 300, total: 300, drop: 0 }));
    }

    #[test]
    fn the_discord_copy_gets_every_clause_at_its_own_rate_and_stops_with_him() {
        let mirror = Output::new(None, Arc::new(AtomicU8::new(100)), true);
        let clause: Vec<f32> = (0..jai_voice_dsp::MASTER_RATE / 2).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        // No device: nothing is queued (and nothing blocks the speaker).
        mirror.push_clause(&clause);
        assert_eq!(mirror.buffered(), 0);
        // A 44.1 kHz virtual cable: half a second at its rate.
        let (producer, consumer) = HeapRb::<f32>::new(44_100 * 30).split();
        *mirror.producer.lock().unwrap() = Some(producer);
        mirror.device_rate.store(44_100, Ordering::Release);
        mirror.push_clause(&clause);
        let queued = mirror.buffered() as i64;
        assert!((queued - 22_050).abs() <= 2, "{queued} samples at 44.1 kHz");
        assert!(mirror.mirror && std::ptr::eq(mirror.level, &MIRROR_LEVEL));
        assert!(std::ptr::eq(Output::new(None, Arc::new(AtomicU8::new(100)), false).level, &OUTPUT_LEVEL));
        drop(consumer);
    }

    #[test]
    fn german_words_survive_speech_normalisation() {
        // Kept for the bilingual phonemizer, never translated ("Advanced Mathematics") or
        // respelled ("Mueller").
        assert_eq!(normalize_for_speech("You have LK Mathematik first."), "You have LK Mathematik first.");
        assert_eq!(normalize_for_speech("Then Geschichte with Herr Müller."), "Then Geschichte with Herr Müller.");
        assert_eq!(normalize_for_speech("Today is **Arme**, 5 GB free."), "Today is Arme, 5 gigabytes free.");
    }

    #[test]
    fn breaths_follow_punctuation_and_the_film_beats() {
        assert_eq!((breath_ms("Done."), breath_ms("First,"), breath_ms("and then")), (140, 70, 30));
        assert_eq!((breath_ms("On it, Sir."), breath_ms("Checking."), breath_ms("Sir,")), (70, 70, 70), "70 ms studio micro-pauses");
    }

    fn stream(tokens: &[&str]) -> Vec<String> {
        let mut s = ClauseSplitter::new();
        let mut out: Vec<String> = tokens.iter().flat_map(|t| s.push(t)).collect();
        out.extend(s.flush());
        out
    }

    #[test]
    fn the_opener_leaves_at_once_so_speech_starts_early() {
        let mut s = ClauseSplitter::new();
        assert_eq!(s.push("Right away, Sir. "), vec!["Right away, Sir."], "a pre-rendered opener plays immediately");
        assert!(s.push("Opening your favourite").is_empty());
        assert_eq!(s.push(" Spotify playlist now. Enjoy"), vec!["Opening your favourite Spotify playlist now."]);
        assert_eq!(s.flush().as_deref(), Some("Enjoy"));
    }

    #[test]
    fn vocative_sir_is_never_split_off() {
        // Streaming: 3 words before the comma, but ", Sir." must stay attached.
        let mut s = ClauseSplitter::new();
        assert!(s.push("Let me check,").is_empty());
        assert!(s.push(" S").is_empty(), "could still become 'Sir'");
        assert!(s.push("ir.").is_empty(), "a full stop needs look-ahead");
        assert_eq!(s.push(" The"), vec!["Let me check, Sir."]);
        assert_eq!(s.flush().as_deref(), Some("The"));
        // Four words at a comma leave at once (after the look-ahead for "Sir"); three wait.
        let mut s = ClauseSplitter::new();
        assert_eq!(s.push("The weather in Berlin, "), Vec::<String>::new());
        assert_eq!(s.push("according to"), vec!["The weather in Berlin,"]);
        let mut s = ClauseSplitter::new();
        assert_eq!(s.push("In Berlin, "), Vec::<String>::new());
        assert!(s.push("it is").is_empty());
    }

    #[test]
    fn complete_texts_split_at_sentences_with_the_opener_first() {
        let mut s = ClauseSplitter::for_text();
        let mut out = s.push("Let me check, Sir. It's 21 minutes to 6 in the afternoon, Sir.");
        out.extend(s.flush());
        assert_eq!(out, vec!["Let me check, Sir.", "It's 21 minutes to 6 in the afternoon, Sir."]);
    }

    #[test]
    fn long_first_sentences_are_cut_at_a_breath() {
        let mut s = ClauseSplitter::new();
        let out = s.push("I have collapsed the audio devices panel the skills panel and the telemetry sidebar for a clean workspace ");
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0], "I have collapsed the audio devices panel", "cut before the article at eight words");
        assert_eq!(s.flush().as_deref(), Some("the skills panel and the telemetry sidebar for a clean workspace"));
        let mut s = ClauseSplitter::new();
        let out = s.push("Your next lesson is maths and then you have history with Mr Smith ");
        assert_eq!(out, vec!["Your next lesson is maths"], "a conjunction wins over an article");
    }

    #[test]
    fn voice_blends_parse_with_weights() {
        assert_eq!(
            voice_components(JAI_VOICE_BLEND),
            vec![("bm_fable".to_string(), 0.75), ("bm_george".to_string(), 0.25)]
        );
        assert_eq!(voice_components("bm_george, bm_lewis"), vec![("bm_george".to_string(), 1.0), ("bm_lewis".to_string(), 1.0)]);
        assert!(voice_components("bm_george:-1+:2").is_empty());
    }

    #[test]
    fn bracketed_notes_tags_and_stage_directions_are_never_spoken() {
        let raw = "<think>plan</think>Natürlich, Sir. [Answered your request with varied phrasing as per instructions.] *clears throat* Done. (Note: internal) [TOOL_CALL: x] See https://example.org now.";
        let clean = sanitize_for_display_and_speech(raw);
        assert_eq!(clean, "Natürlich, Sir. Done. See now.");
        // The same text split into arbitrary streaming pieces gives the same result.
        let mut f = MetaStreamFilter::new();
        let mut streamed = String::new();
        let chars: Vec<char> = raw.chars().collect();
        for piece in chars.chunks(3) {
            streamed.push_str(&f.push(&piece.iter().collect::<String>()));
        }
        streamed.push_str(&f.flush());
        assert_eq!(streamed, clean);
        assert_eq!(sanitize_for_display_and_speech("[CMD: open spotify] Opening Spotify, Sir."), "Opening Spotify, Sir.");
        assert_eq!(sanitize_for_display_and_speech("The ratio is 3 * 4 (roughly)."), "The ratio is 3 * 4 (roughly).");
    }

    /// "The current temperature inBerlin is 7°C" (silent session, 2026-11-06): whatever the
    /// model wraps a word in, taking the wrapping away must leave the words apart, in one piece
    /// and in a stream cut anywhere.
    #[test]
    fn taking_the_markup_away_never_glues_two_words() {
        let clean = "The current temperature in Berlin is 7°C, feeling like 4°C.";
        for raw in [
            "The current temperature in **Berlin** is 7°C, feeling like 4°C.",
            "The current temperature in *Berlin* is 7°C, feeling like 4°C.",
            "The current temperature in __Berlin__ is 7°C, feeling like 4°C.",
            "The current temperature in `Berlin` is 7°C, feeling like 4°C.",
            "The current temperature in\u{a0}Berlin is 7°C, feeling like 4°C.",
            "The current temperature in\u{200b} Berlin is 7°C, feeling like 4°C.",
            "The current temperature in \u{200b}Berlin is 7°C, feeling like 4°C.",
            "The current temperature in\u{200b}Berlin is 7°C, feeling like 4°C.",
            "The current temperature in\u{2060}Berlin is 7°C, feel\u{00ad}ing like 4°C.",
            "The current temperature in\tBerlin is 7°C, feeling like 4°C.",
            "The current temperature in\nBerlin is 7°C, feeling like 4°C.",
        ] {
            // (A code span keeps its ticks on screen, and underscore emphasis is only taken away
            // from the finished reply: neither is a glued word.)
            let words = |text: &str| text.replace(['`', '_'], "").split_whitespace().collect::<Vec<_>>().join(" ");
            assert_eq!(words(&sanitize_for_display_and_speech(raw)), clean, "{raw:?} in one piece");
            for size in [1, 2, 3, 5, 7, 11] {
                let mut filter = MetaStreamFilter::new();
                let mut streamed = String::new();
                let chars: Vec<char> = raw.chars().collect();
                for piece in chars.chunks(size) {
                    streamed.push_str(&filter.push(&piece.iter().collect::<String>()));
                }
                streamed.push_str(&filter.flush());
                assert_eq!(words(&streamed), clean, "{raw:?} streamed {size} characters at a time: {streamed:?}");
            }
        }
    }

    /// Repro (silent session, 2026-11-06): a joke that slipped into Chinese half way through.
    #[test]
    fn the_models_chinese_never_reaches_the_voice_or_the_screen() {
        let raw = "Why was the math book sad?因为它没有封面（它是一本无解的书）。但这可能不是您期望听到的答案，让我们来个更轻松一点的吧：Why did the chicken cross the road? To get to the other side!  哪个笑话更适合您？";
        let chars: Vec<char> = raw.chars().collect();
        for size in [1, 2, 3, 5, 7, 11, chars.len()] {
            let mut filter = MetaStreamFilter::new();
            let mut streamed = String::new();
            for piece in chars.chunks(size) {
                streamed.push_str(&filter.push(&piece.iter().collect::<String>()));
            }
            streamed.push_str(&filter.flush());
            assert_eq!(streamed.trim(), "Why was the math book sad? Why did the chicken cross the road? To get to the other side!", "{size} characters at a time");
        }
        // A reply of nothing else leaves nothing, so the turn's own retry takes over.
        let mut filter = MetaStreamFilter::new();
        let mut all = filter.push("这是一个笑话。");
        all.push_str(&filter.flush());
        assert_eq!(all.trim(), "");
        // What stays is everything else he writes: names, accents, degrees, German.
        let mut filter = MetaStreamFilter::new();
        let mut kept = filter.push("Müller in Köln: 7°C, café, 50 % — fine.");
        kept.push_str(&filter.flush());
        assert_eq!(kept, "Müller in Köln: 7°C, café, 50 % — fine.");
    }

    #[test]
    fn comma_requires_fourteen_words_and_never_detaches_sir() {
        let out = stream(&["I have checked every connected system and all fourteen separate services are operating normally, ", "so we are ready."]);
        assert_eq!(out.len(), 2);
        assert!(out[0].ends_with("normally,"));
    }

    #[test]
    fn hermite_resampling_preserves_dc_length_and_intermediate_curvature() {
        let constant = resample_voice(&[0.2; 100], 24_000, 48_000);
        assert_eq!(constant.len(), 200);
        assert!(constant.iter().all(|x| (*x - 0.2).abs() < 1e-6));
        let tone: Vec<f32> = (0..1200).map(|i| (i as f32 * 0.13).sin() * 0.4).collect();
        let output = resample_voice(&tone, 24_000, 48_000);
        let error: f32 = output[4..2396].iter().enumerate().map(|(i, x)| (*x - ((i + 4) as f32 * 0.065).sin() * 0.4).abs()).sum();
        assert!(error / 2392.0 < 0.0001);
    }

    #[test]
    fn joining_overlaps_twenty_five_ms_and_drops_cancelled_generation() {
        let mut joiner = ClauseJoiner::default();
        let first = joiner.push(vec![0.3; 4800], 1, 48_000);
        let second = joiner.push(vec![-0.3; 4800], 1, 48_000);
        assert_eq!(first.len(), 3600);
        assert!((second[0] - 0.3).abs() < 1e-6);
        assert!((second[1199] + 0.3).abs() < 1e-6);
        assert!(second[..1200].windows(2).all(|p| (p[1] - p[0]).abs() < 0.002));
        assert_eq!(joiner.push(vec![0.1; 4800], 2, 48_000)[0], 0.1);
        assert!(joiner.finish(1).is_empty());
        assert_eq!(joiner.finish(2).len(), 1200);
    }

    #[test]
    fn numbers_acronyms_and_abbreviations_do_not_split() {
        let out = stream(&["The CPU runs at 3.5 GHz ", "and it is 12:30 now, Sir. ", "J.A.I. is ", "online, e.g. fully."]);
        assert_eq!(out[0], "The CPU runs at 3.5 GHz and it is 12:30 now, Sir.");
        assert_eq!(out[1], "J.A.I. is online, e.g. fully.");
    }

    #[test]
    fn code_blocks_and_markdown_are_never_spoken() {
        let out = stream(&[
            "**Certainly**, Sir. Here it is:\n```power",
            "shell\nGet-Process | Sort-Object CPU\n```",
            "\n- I have *saved* it as `top_cpu`. See [docs](https://example.com/x).",
        ]);
        let joined = out.join(" | ");
        assert!(!joined.contains("Get-Process") && !joined.contains("powershell"), "{joined}");
        assert!(!joined.contains('*') && !joined.contains('`') && !joined.contains("https"), "{joined}");
        assert!(joined.contains("Certainly, Sir."), "{joined}");
        assert!(joined.contains("I have saved it as top cpu."), "{joined}");
        assert!(joined.contains("See docs."), "{joined}");
    }

    #[test]
    fn clause_keys_ignore_case_and_punctuation() {
        assert_eq!(clause_key("Right away, Sir."), clause_key("right away sir"));
        assert_ne!(clause_key("Right away, Sir."), clause_key("On it."));
    }

    #[test]
    fn speech_normalisation() {
        // The pronunciation lexicon respells the acronyms ("G P U", "V-Ram").
        assert_eq!(normalize_for_speech("GPU at 48°C, 6.5 GB VRAM"), "G P U at 48 degrees Celsius, 6.5 gigabytes V-Ram");
        assert_eq!(normalize_for_speech("**Spotify** at 15:00 on the RTX 3060 Ti"), "Spot-ify at three P M on the R T X thirty sixty T I");
        assert_eq!(normalize_for_speech("J.A.I. online & ready"), "Jay online and ready");
        // A heading and each list line end like a sentence: the voice pauses after each.
        assert_eq!(strip_markdown("## Status\n* **CPU**: 12% \u{1F680}"), "Status. CPU: 12%.");
        assert_eq!(strip_markdown("Three things:\r\n- milk\r\n- eggs!\r\n1. bread"), "Three things: milk. eggs! bread.");
        assert_eq!(strip_markdown("A sentence that wraps\nonto a second line"), "A sentence that wraps onto a second line", "a wrapped line is no list");
        // A link is its site and its first part, said with "dot" and "slash".
        assert_eq!(strip_markdown("See https://github.com/example/repo?tab=1 now"), "See github dot com slash example now");
        assert_eq!(strip_markdown("At https://www.example.org."), "At example dot org.");
        assert_eq!(strip_markdown("Open http://localhost:8080/"), "Open localhost");
        assert_eq!(spoken_link("https://news.example.de/a-very-long-article-title-that-goes-on"), "news dot example dot de");
    }

    #[test]
    fn builtin_vocab_matches_kokoro_table() {
        let v = builtin_vocab();
        assert_eq!(v.len(), 114);
        assert_eq!(v[&'ɹ'], 123);
        assert_eq!(v[&'ˈ'], 156);
        assert_eq!(v[&' '], 16);
    }

    #[test]
    fn clause_audio_gets_pause_and_fades() {
        let tone: Vec<f32> = (0..24_000).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let out = finish_clause_audio(tone, "Done.");
        // 1 s at 24 kHz → ~1.02 s at 48 kHz (pitch lock) plus the 140 ms breath.
        assert!(out.len() > 48_000 + 6_000, "{}", out.len());
        assert!(out[out.len() - 100..].iter().all(|s| *s == 0.0));
        assert!(dsp::peak(&out) <= 0.9, "the limiter holds −1 dBFS");
        assert!(out[0].abs() < 1e-3, "zero-click fade-in");
    }
}
