//! Microphone capture → 16 kHz mono → 512-sample frames → Silero VAD → [`VadMachine`].
//!
//! Threads:
//! * `jai-mic`: owns the cpal input stream(s). The real-time callback only down-mixes to
//!   mono and pushes into a lock-free SPSC ring buffer (no locks, no allocation).
//! * `jai-vad` (high priority): drains the ring, resamples to 16 kHz with an anti-aliasing
//!   FIR, cuts exact 512-sample frames, runs Silero + the state machine and emits events.
//!
//! In "Auto" mode the default microphone plus one more physical microphone are opened; if the
//! active one goes dead (muted headset) while the other carries signal, capture fails over.

use crate::acoustic_radar::{AcousticEvent, AcousticRadar};
use crate::config::{AcousticSettings, JaiConfig};
use crate::dsp::{self, StreamResampler};
use crate::vad::{self, EndReason, FrameContext, SileroVad, VadEvent, VadMachine, VadParams, VadPhase, FRAME};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use ringbuf::{HeapConsumer, HeapProducer, HeapRb};
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info, warn};

static CLOCK: LazyLock<Instant> = LazyLock::new(Instant::now);

/// These whole-utterance phrases deliberately bypass the ordinary wake-word gate. Keeping
/// the match anchored prevents a quoted lyric or a question about the protocol from firing it.
pub fn is_homecoming_phrase(text: &str) -> bool {
    let normalized = text.to_lowercase().replace(['\'', '’'], "").chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>();
    // His name may stand in front or behind ("JAI, I'm home": `wake::canon` puts it in front),
    // or be said as Sir says it ("I'm home, Jay").
    let is_name = |w: &str| matches!(w, "jai" | "jay");
    let named = normalized.split_whitespace().any(is_name);
    let said = normalized.split_whitespace().filter(|w| !is_name(w)).collect::<Vec<_>>().join(" ");
    matches!(said.as_str(), "wake up daddys home" | "daddys home" | "i am back") || (named && matches!(said.as_str(), "wake up" | "im home" | "i am home"))
}

#[cfg(test)]
mod homecoming_tests {
    #[test]
    fn protocol_requires_the_whole_utterance() {
        for text in ["Wake up, Daddy's home!", "Daddy’s home", "JAI wake up", "I'm home JAI", "I am back", "JAI, I'm home", "Jay, wake up, daddy's home", "I'm home, Jay."] {
            assert!(super::is_homecoming_phrase(text), "{text}");
        }
        // As the voice path hands them over: his name in its one spelling, in front.
        for said in ["I'm home, Jay.", "Jay, I am back", "Wake up, Jay"] {
            assert!(super::is_homecoming_phrase(&crate::wake::canon(said)), "{said} → {}", crate::wake::canon(said));
        }
        for text in ["what happens when I say daddy's home", "play daddy's home", "I am back at school tomorrow", "wake up", "I'm home"] {
            assert!(!super::is_homecoming_phrase(text), "{text}");
        }
    }
}

// ------------------------------------------------------------------ addressee detection ----
// Sir never has to say "JAI" when he is obviously talking to him. Four layers decide:
//   A. the conversation window: for 5 s after JAI finished speaking (or carried out a
//      command), and while he asks something; after that, room speech needs a reason again;
//   B. window focus: one of JAI's windows is in front;
//   C. direct intent: a command, a question, "can you" / "please" / "you", or anything the
//      fast path recognises as a request;
//   D. chatter rejection: with a game or a voice chat (Discord …) in front, fragments, callouts
//      and talk about third persons are ignored unless they are a clear command for JAI.

/// To whom an utterance was spoken, and why JAI thinks so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressee {
    /// Sir said "JAI" (anywhere, as a vocative).
    WakeWord,
    /// Layer A: inside the conversation window.
    Conversation,
    /// Layer B: JAI's own window is in front.
    Focus,
    /// Layer C: a direct command or question.
    DirectIntent,
    /// Hands-free mode and nothing speaks against it.
    HandsFree,
    /// Layer D: gamer or voice-chat talk not meant for JAI (ignored).
    Chatter,
    /// The wake word is required and no layer applies (ignored).
    Unaddressed,
}

impl Addressee {
    pub fn accepted(self) -> bool {
        !matches!(self, Self::Chatter | Self::Unaddressed)
    }

    /// The HUD transcript's source label.
    pub fn label(self) -> &'static str {
        match self {
            Self::WakeWord => "wake-word",
            Self::Conversation => "follow-up",
            Self::Focus => "jai in focus",
            Self::DirectIntent => "direct intent",
            Self::HandsFree => "voice",
            Self::Chatter => "ignored · chatter",
            Self::Unaddressed => "ignored",
        }
    }
}

/// What the app knows when an utterance arrives.
#[derive(Debug, Clone, Copy, Default)]
pub struct AddresseeContext {
    pub wake_word: bool,
    pub wake_required: bool,
    /// The conversation window is open (or a turn is running / Sir is finishing a sentence).
    pub conversation_open: bool,
    /// One of JAI's windows is the foreground window.
    pub jai_focused: bool,
    /// A game, a full-screen app or a voice chat (Discord, TeamSpeak …) is in front.
    pub gaming: bool,
}

/// Lower-case words without punctuation (apostrophes dropped: "what's" → "whats").
fn plain_words(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '\'' | '’'))
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

const FILLERS: &[&str] = &["ok", "okay", "hey", "so", "um", "uh", "ähm", "äh", "also", "und", "and", "na", "well", "alright", "right", "now", "then", "dann", "ja", "yes", "no", "nein", "oh"];
/// Imperatives JAI acts on (English and German).
pub(crate) const COMMAND_VERBS: &[&str] = &[
    "open", "close", "play", "pause", "stop", "resume", "skip", "next", "show", "hide", "remove", "make", "set", "turn", "put", "move", "start", "launch",
    "search", "find", "look", "check", "tell", "give", "read", "create", "add", "delete", "remind", "schedule", "send", "text", "mute", "unmute",
    "maximize", "maximise", "minimize", "minimise", "switch", "bring", "clean", "declutter", "translate", "explain", "summarize", "summarise",
    "write", "copy", "save", "run", "restart", "shut", "lock", "pick", "choose", "increase", "decrease", "raise", "lower", "enable", "disable", "zoom",
    "just", "keep", "leave", "display", "list", "repeat", "cancel", "brief", "calculate", "convert", "go", "take", "get", "plan", "book", "draft",
    "mach", "mache", "öffne", "oeffne", "schließe", "schliesse", "schließ", "spiel", "spiele", "zeig", "zeige", "starte", "stopp", "stoppe", "such",
    "suche", "finde", "sag", "erklär", "erkläre", "setz", "setze", "stell", "stelle", "schalte", "schalt", "trag", "trage", "erinnere", "lies", "schreib",
    "schreibe", "schick", "schicke", "wechsel", "wechsle", "pausiere", "leiser", "lauter", "entferne", "versteck", "verstecke", "blende", "lösche",
];
/// Openings of a question.
const QUESTION_WORDS: &[&str] = &[
    "what", "whats", "when", "how", "where", "who", "whos", "why", "which", "is", "are", "can", "could", "would", "will", "do", "does", "did", "should",
    "have", "has", "am", "wie", "wann", "wo", "was", "wer", "warum", "welche", "welcher", "welches", "hab", "habe", "hast", "kannst", "könntest",
    "ist", "sind", "gibt", "hat", "brauche", "muss",
];
/// Words that only make sense spoken to someone: JAI, when no game or chat is in front.
const SECOND_PERSON: &[&str] = &["you", "your", "yourself", "please", "du", "dich", "dir", "dein", "deine", "bitte"];
/// Callouts and banter of a game lobby or a voice chat.
const CHATTER_WORDS: &[&str] = &[
    "behind", "reload", "reloading", "push", "rush", "flank", "heal", "rez", "revive", "res", "enemy", "enemies", "sniper", "camp", "camping",
    "spawn", "loot", "gg", "lol", "lmao", "bro", "dude", "noob", "nice", "cracked", "one shot", "oneshot", "low", "lethal", "ult", "ultimate",
    "digga", "alter", "junge", "boah", "krass", "geil", "hilfe", "drauf", "rein", "raus", "links", "rechts", "kill", "killed", "dead", "tot",
    "team", "mate", "squad", "lobby", "match", "round", "runde", "wave", "boss", "respawn", "afk", "brb", "wtf", "omg",
];
const THIRD_PERSON: &[&str] = &["he", "hes", "she", "shes", "they", "theyre", "him", "her", "them", "his", "er", "ihn", "ihm"];

/// A request the fast path, the integration router or the layout parser recognises: clearly
/// meant for JAI, even over a game.
pub fn known_command(text: &str) -> bool {
    crate::intents::parse(text).is_some()
        || crate::intents::ui_layout_command(text).is_some()
        || crate::integration_router::route_text(text).is_some()
        || crate::integration_router::calendar_creation_intent(text)
        || crate::integration_router::capability_topic(text).is_some()
        || crate::intents::live_info_query(text).is_some()
        || crate::intents::upfront_tool(text).is_some()
        || crate::intents::is_memory_command(text)
        || is_homecoming_phrase(text)
        || music_request(text)
}

/// "Play Highway to Hell", "spiel Back in Black": a play verb and something to play.
fn music_request(text: &str) -> bool {
    let words = plain_words(text);
    let list: Vec<&str> = words.split_whitespace().filter(|w| !FILLERS.contains(w)).collect();
    matches!(list.first(), Some(&("play" | "spiel" | "spiele" | "shuffle" | "queue"))) && list.len() >= 2
}

/// Layer C on the words alone: a command, a question or a direct address.
pub fn direct_intent(text: &str) -> bool {
    let words = plain_words(text);
    let mut list: Vec<&str> = words.split_whitespace().collect();
    while list.len() > 1 && FILLERS.contains(&list[0]) {
        list.remove(0);
    }
    let Some(first) = list.first() else { return false };
    if COMMAND_VERBS.contains(first) || QUESTION_WORDS.contains(first) {
        return true;
    }
    let joined = format!(" {} ", list.join(" "));
    ["can you", "could you", "would you", "will you", "kannst du", "könntest du", "do i ", "hab ich", "habe ich", "i need", "i want", "id like", "ich brauche", "ich will", "ich möchte"]
        .iter()
        .any(|p| joined.contains(&format!(" {p}")))
        || list.iter().any(|w| SECOND_PERSON.contains(w))
        || text.trim_end().ends_with('?')
}

/// Layer D: fragments, callouts and talk about third persons, as heard over a game or a chat.
pub fn looks_like_chatter(text: &str) -> bool {
    let words = plain_words(text);
    let list: Vec<&str> = words.split_whitespace().collect();
    if list.len() <= 2 {
        return true;
    }
    let joined = format!(" {words} ");
    let callout = CHATTER_WORDS.iter().any(|w| joined.contains(&format!(" {w} ")));
    let addresses_someone = list.iter().any(|w| SECOND_PERSON.contains(w));
    let third = list.iter().any(|w| THIRD_PERSON.contains(w));
    callout || (third && !addresses_someone) || addresses_someone
}

/// "Close", "minimise" on their own: a word anyone in the room or on Discord says. They act on
/// JAI's own window only when he is addressed (his name, an open conversation, his window).
fn bare_window_word(text: &str) -> bool {
    crate::intents::silent_quick_command(text).is_some_and(|a| matches!(a.tool, "configure_interface" | "window_management"))
}

/// The four layers: who Sir is talking to.
pub fn classify_addressee(text: &str, ctx: &AddresseeContext) -> Addressee {
    if ctx.wake_word {
        return Addressee::WakeWord;
    }
    if ctx.jai_focused {
        // His window in front makes a sentence his to hear, but not every word in the room:
        // a fragment, a callout or talk about a third person ("he said he'd come later") that
        // is no order he knows, no question and no address stays with whoever it was for.
        let for_someone_else = !ctx.conversation_open && !known_command(text) && !direct_intent(text) && looks_like_chatter(text);
        return if for_someone_else { Addressee::Chatter } else { Addressee::Focus };
    }
    if bare_window_word(text) && !ctx.conversation_open {
        return if ctx.gaming { Addressee::Chatter } else { Addressee::Unaddressed };
    }
    if ctx.gaming {
        // Over a game or a voice chat only clear commands, and follow-ups that are not banter.
        if known_command(text) {
            return Addressee::DirectIntent;
        }
        // "Yes, please" / "no thanks" to what JAI just asked.
        if ctx.conversation_open && (crate::intents::is_affirmation(text) || crate::intents::is_negation(text)) {
            return Addressee::Conversation;
        }
        if ctx.conversation_open && !looks_like_chatter(text) {
            return Addressee::Conversation;
        }
        return Addressee::Chatter;
    }
    if ctx.conversation_open {
        return Addressee::Conversation;
    }
    if known_command(text) || direct_intent(text) {
        return Addressee::DirectIntent;
    }
    if !ctx.wake_required {
        return Addressee::HandsFree;
    }
    Addressee::Unaddressed
}

// ------------------------------------------------------------- hands-free relevance ----
// Without the wake word JAI only reacts to speech that is plausibly meant for him: humming,
// sung syllables, Whisper's all-caps hallucinations ("HABIT FOOTBALL") and lone fragments are
// dropped silently (logged as "[Background Speech Ignored]").

/// Below this mean log-probability Whisper is unsure of what it wrote.
pub const UNSURE_LOGPROB: f32 = -0.5;

/// A song through the microphone: while a player is playing, words Whisper is unsure of and that
/// are no command JAI knows are the song's, not Sir's. (jai.log 2026-11-05 19:45: "Fix that
/// my heart, hope it never stops war. Yeah." was answered as a request.) With the wake word,
/// push-to-talk or a barge-in this is never asked.
pub fn sung_not_said(text: &str, avg_logprob: f32, player_playing: bool) -> bool {
    player_playing && avg_logprob < UNSURE_LOGPROB && !known_command(text)
}

/// Sounds, not words: humming, "la la la", "mm-hm".
const HUM_WORDS: &[&str] = &[
    "hm", "hmm", "hmmm", "hmmmm", "mm", "mmm", "mmmm", "mhm", "mmhm", "uh", "uhh", "um", "umm", "ah", "ahh", "oh", "ohh", "ooh", "oooh", "la", "lala",
    "na", "nana", "da", "dada", "doo", "dum", "dee", "di", "ba", "bam", "pa", "ta", "tra", "yeah", "hey", "ha", "haha", "hahaha", "he", "hehe", "ay",
    "äh", "ähm", "öhm", "mh", "hmhm", "shh", "psst", "woo", "whoa", "wow", "ugh", "huh", "eh",
];
/// Words that carry no request on their own.
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "so", "to", "of", "in", "on", "at", "it", "its", "is", "was", "be", "i", "me", "my", "we", "this", "that", "just",
    "ok", "okay", "yeah", "yes", "no", "well", "like", "um", "uh", "oh", "der", "die", "das", "und", "oder", "ich", "ja", "nein", "ist", "es", "so", "na",
    "also", "halt", "mal", "doch", "eben", "hey", "right", "then", "now", "too", "very", "really",
];

fn alpha_words(text: &str) -> Vec<String> {
    plain_words(text).split_whitespace().map(str::to_string).collect()
}

/// Content words (no fillers, stop words or sounds).
fn meaningful_words(text: &str) -> Vec<String> {
    alpha_words(text).into_iter().filter(|w| w.chars().count() > 1 && !STOP_WORDS.contains(&w.as_str()) && !HUM_WORDS.contains(&w.as_str())).collect()
}

/// Humming or sung syllables: every word a sound, or musical notes.
pub fn is_humming(text: &str) -> bool {
    if text.contains('\u{266A}') || text.contains('\u{266B}') || text.contains("\u{1F3B5}") {
        return true;
    }
    let words = alpha_words(text);
    !words.is_empty()
        && words.iter().all(|w| {
            HUM_WORDS.contains(&w.as_str())
                // "Mmmmmm", "lalala", "hmhmhm": one or two letters repeated.
                || { let letters: std::collections::BTreeSet<char> = w.chars().collect(); letters.len() <= 2 && w.chars().count() >= 4 }
        })
}

/// "La la la la", "ich bin cool, ich bin cool, ich bin cool": the same few words over and over.
pub fn is_repetition(text: &str) -> bool {
    let words = alpha_words(text);
    if words.len() < 4 {
        return false;
    }
    let distinct: std::collections::BTreeSet<&str> = words.iter().map(String::as_str).collect();
    distinct.len() * 10 <= words.len() * 4
}

/// Whisper's hallucination pattern on noise: a short line in capitals ("HABIT FOOTBALL").
pub fn is_caps_hallucination(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().filter(|w| w.chars().any(char::is_alphabetic)).collect();
    if words.is_empty() || words.len() > 6 {
        return false;
    }
    let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
    // A single acronym ("GPU", "OK") is not a hallucination of a whole line.
    letters.len() >= 5 && letters.iter().all(|c| c.is_uppercase()) && !(words.len() == 1 && letters.len() <= 4)
}

/// Why an utterance without the wake word is not for JAI, or `None` when it may be.
/// `hands_free`: the stricter checks of hands-free mode (no open conversation): fewer than two
/// meaningful words (unless it is a quick command) and no plausible link to a request or to the
/// last two turns (`recent`).
pub fn background_speech(text: &str, hands_free: bool, recent: &[String]) -> Option<&'static str> {
    if is_humming(text) {
        return Some("humming");
    }
    if is_caps_hallucination(text) {
        return Some("all-caps hallucination");
    }
    if is_repetition(text) {
        return Some("repeated syllables");
    }
    if !hands_free {
        return None;
    }
    let quick = crate::intents::silent_quick_command(text).is_some() || known_command(text);
    let meaningful = meaningful_words(text);
    if meaningful.len() < 2 && !quick {
        return Some("fragment");
    }
    if quick || direct_intent(text) {
        return None;
    }
    // Continuity: the same subject as the last exchange.
    let related = recent.iter().any(|turn| {
        let earlier = meaningful_words(turn);
        meaningful.iter().any(|w| w.chars().count() >= 4 && earlier.contains(w))
    });
    if related || (meaningful.len() >= 5 && !looks_like_chatter(text)) {
        return None;
    }
    Some("implausible")
}

/// The last few things heard (addressed or not), for the retroactive wake word: "…turn the
/// music down. JAI, do that." acts on the words spoken before the name.
#[derive(Debug, Default)]
pub struct RollingSpeechTranscriptBuffer {
    segments: std::collections::VecDeque<HeardSegment>,
}

#[derive(Debug, Clone)]
struct HeardSegment {
    text: String,
    at: Instant,
    /// Not addressed to JAI when it was heard.
    ignored: bool,
}

impl RollingSpeechTranscriptBuffer {
    pub const CAPACITY: usize = 5;
    pub const MAX_AGE: Duration = Duration::from_secs(60);
    /// "JAI, do that" refers back to speech at most this old …
    pub const REFERENCE_WINDOW: Duration = Duration::from_secs(12);
    /// … and stitches everything ignored within this span.
    pub const STITCH_SPAN: Duration = Duration::from_secs(25);

    pub fn push(&mut self, text: &str, ignored: bool) {
        self.push_at(text, ignored, Instant::now());
    }

    fn push_at(&mut self, text: &str, ignored: bool, at: Instant) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.segments.push_back(HeardSegment { text: text.to_string(), at, ignored });
        while self.segments.len() > Self::CAPACITY {
            self.segments.pop_front();
        }
        self.segments.retain(|s| at.duration_since(s.at) <= Self::MAX_AGE);
    }

    /// The ignored speech a bare "JAI" / "JAI, do that for me" refers to: every ignored
    /// segment of the last 25 s (oldest first), when the newest is at most 12 s old. Taken out of
    /// the buffer so it is never acted on twice.
    pub fn take_referenced(&mut self) -> Option<String> {
        self.take_referenced_at(Instant::now())
    }

    fn take_referenced_at(&mut self, now: Instant) -> Option<String> {
        let newest = self.segments.iter().rev().find(|s| s.ignored)?;
        if now.duration_since(newest.at) > Self::REFERENCE_WINDOW {
            return None;
        }
        let picked: Vec<String> = self
            .segments
            .iter()
            .filter(|s| s.ignored && now.duration_since(s.at) <= Self::STITCH_SPAN && background_speech(&s.text, false, &[]).is_none())
            .map(|s| s.text.trim().trim_end_matches(['.', ',']).to_string())
            .collect();
        self.segments.retain(|s| !s.ignored);
        (!picked.is_empty()).then(|| picked.join(". "))
    }

    /// The texts heard most recently (newest last).
    #[cfg(test)]
    fn recent(&self, n: usize) -> Vec<String> {
        let skip = self.segments.len().saturating_sub(n);
        self.segments.iter().skip(skip).map(|s| s.text.clone()).collect()
    }
}

/// What Sir says after "JAI" to mean the words he just spoke: nothing, "do that (for me)",
/// "you heard me", "mach das".
pub fn is_retro_reference(command: &str) -> bool {
    let w = plain_words(command);
    let w = w.trim_start_matches("please ").trim_end_matches(" please").trim_start_matches("bitte ").trim_end_matches(" bitte").trim();
    w.is_empty()
        || matches!(
            w,
            "do that" | "do that for me" | "do it" | "do it for me" | "do this" | "do this for me" | "that" | "you heard me" | "you heard that" | "did you hear that"
                | "can you do that" | "could you do that" | "go ahead" | "make it so" | "that one" | "exactly that" | "mach das" | "mach das für mich"
                | "mach es" | "genau das" | "hast du gehört" | "du hast es gehört" | "erledige das" | "mach mal"
        )
}

#[cfg(test)]
mod relevance_tests {
    use super::*;

    #[test]
    fn lyrics_through_the_microphone_are_the_songs_not_sirs() {
        // jai.log 2026-11-05 19:45:25-29, with Whisper's own log-probabilities.
        for (text, logprob) in [
            ("Fix that lahat how it never stops war.", -0.63),
            ("Fix that my heart, hope it never stops war. Yeah.", -0.57),
            ("Fix that lahard how it never stops war. Yeah. Baby.", -0.84),
        ] {
            assert!(sung_not_said(text, logprob, true), "{text}");
            assert!(!sung_not_said(text, logprob, false), "no player playing: Sir may well have said it");
        }
        // What Sir really said without the wake word, while unsure or not (same log, 11-03/04).
        assert!(!sung_not_said("Close yourself.", -0.60, true), "a command is a command, however unsure");
        assert!(!sung_not_said("Can you test the Discord bars?", -0.34, true), "Whisper is sure of it");
        assert!(!sung_not_said("That's not your voice.", -0.21, true));
        assert!(!sung_not_said("Yes.", -0.29, true));
    }

    #[test]
    fn humming_hallucinations_and_fragments_are_background() {
        for (text, why) in [
            ("Hmm hmm hmm.", "humming"),
            ("La la la la la", "humming"),
            ("Mmmmmm", "humming"),
            ("\u{266A} \u{266A}", "humming"),
            ("HABIT FOOTBALL", "all-caps hallucination"),
            ("Ich bin cool, ich bin cool, ich bin cool, ich bin cool.", "repeated syllables"),
            ("Welp.", "fragment"),
            ("Banana", "fragment"),
        ] {
            assert_eq!(background_speech(text, true, &[]), Some(why), "{text}");
        }
        assert_eq!(background_speech("I was talking to my mum about the weekend", true, &[]), Some("implausible"));
    }

    #[test]
    fn requests_and_follow_ups_pass() {
        for text in ["Pause", "What's the weather tomorrow?", "Open Spotify", "Can you make it darker", "Volume 40"] {
            assert_eq!(background_speech(text, true, &[]), None, "{text}");
        }
        // An acronym is not a hallucinated line.
        assert_eq!(background_speech("GPU", false, &[]), None);
        let recent = vec!["Your maths exam is on Friday at nine.".to_string()];
        assert_eq!(background_speech("The maths exam moved to Monday actually", true, &recent), None, "continues the subject");
        // In an open conversation only noise is filtered.
        assert_eq!(background_speech("Banana", false, &[]), None);
    }

    #[test]
    fn retroactive_wake_word_stitches_recent_ignored_speech() {
        let mut buf = RollingSpeechTranscriptBuffer::default();
        let start = Instant::now();
        buf.push_at("Turn the music down a bit", true, start);
        buf.push_at("and open Discord", true, start + Duration::from_secs(3));
        assert_eq!(buf.take_referenced_at(start + Duration::from_secs(8)).as_deref(), Some("Turn the music down a bit. and open Discord"));
        assert!(buf.take_referenced_at(start + Duration::from_secs(9)).is_none(), "taken once");
        buf.push_at("Turn the music down", true, start);
        assert!(buf.take_referenced_at(start + Duration::from_secs(30)).is_none(), "too old to refer to");
        for i in 0..8 {
            buf.push_at(&format!("line {i}"), false, start);
        }
        assert_eq!(buf.recent(10).len(), RollingSpeechTranscriptBuffer::CAPACITY);
        assert!(is_retro_reference("") && is_retro_reference("do that for me") && is_retro_reference("Mach das, bitte") && !is_retro_reference("open Spotify"));
    }
}

/// Process names of games and voice chats (Layer D's "something else is talking").
pub fn is_game_or_voice_chat(process: &str) -> bool {
    let p = process.to_lowercase();
    let p = p.trim_end_matches(".exe");
    [
        "discord", "discordptb", "discordcanary", "teamspeak", "ts3client_win64", "ts3client", "mumble", "guilded", "javaw", "minecraft", "minecraftlauncher",
        "valorant", "valorant-win64-shipping", "cs2", "csgo", "fortniteclient-win64-shipping", "r5apex", "league of legends", "leagueclient", "overwatch",
        "rocketleague", "gta5", "eldenring", "rainbowsix", "cod", "modernwarfare", "destiny2", "dota2", "pubg", "tslgame", "apex", "robloxplayerbeta",
        "witcher3", "cyberpunk2077", "starfield", "bg3", "helldivers2", "deadlock", "marvelrivals",
    ]
    .iter()
    // Short names only exactly ("cod" must not catch VS Code's "code").
    .any(|g| p == *g || (g.len() >= 6 && p.starts_with(g)))
}

#[cfg(test)]
mod addressee_tests {
    use super::*;

    fn ctx() -> AddresseeContext {
        AddresseeContext { wake_required: true, ..Default::default() }
    }

    #[test]
    fn follow_ups_need_no_wake_word() {
        let open = AddresseeContext { conversation_open: true, ..ctx() };
        let text = "Just leave the normal chat window, but remove the Audio Devices tab and the Skills tab, please.";
        assert_eq!(classify_addressee(text, &open), Addressee::Conversation);
        // Even after the window closed: a direct request is still for JAI.
        assert_eq!(classify_addressee(text, &ctx()), Addressee::DirectIntent);
        assert_eq!(classify_addressee("Ich bin cool, ich bin cool, ich bin cool.", &ctx()), Addressee::Unaddressed);
        assert_eq!(classify_addressee("What classes do I have tomorrow?", &ctx()), Addressee::DirectIntent);
        assert_eq!(classify_addressee("Hab ich morgen Mathe?", &ctx()), Addressee::DirectIntent);
        assert_eq!(classify_addressee("What is 5 plus 5?", &ctx()), Addressee::DirectIntent);
        assert_eq!(classify_addressee("Can you create a new event in my calendar?", &ctx()), Addressee::DirectIntent);
        let focused = AddresseeContext { jai_focused: true, ..ctx() };
        assert_eq!(classify_addressee("the weather in Paris", &focused), Addressee::Focus);
        // With his window in front: orders, questions and anything said to "you" are his;
        // talk about someone else and loose fragments are not.
        for his in ["Next song", "Stop", "What time is it?", "Can you open Discord", "Mach die Musik leiser", "Tell me a joke about school"] {
            assert_eq!(classify_addressee(his, &focused), Addressee::Focus, "{his}");
        }
        for theirs in ["He said he would come later", "She is not at home right now", "Oh man", "Yeah right"] {
            assert_eq!(classify_addressee(theirs, &focused), Addressee::Chatter, "{theirs}");
        }
        // In an open conversation everything is his, as before.
        let talking = AddresseeContext { jai_focused: true, conversation_open: true, ..ctx() };
        assert_eq!(classify_addressee("He said he would come later", &talking), Addressee::Focus);
        let hands_free = AddresseeContext { wake_required: false, ..ctx() };
        assert_eq!(classify_addressee("I was thinking about the holidays", &hands_free), Addressee::HandsFree);
    }

    #[test]
    fn game_and_discord_banter_is_ignored_but_commands_are_not() {
        let gaming = AddresseeContext { gaming: true, ..ctx() };
        for chatter in ["He's behind you, push him!", "nice", "Can you heal me?", "lol that was cracked", "Er ist links, drauf!"] {
            assert_eq!(classify_addressee(chatter, &gaming), Addressee::Chatter, "{chatter}");
        }
        for command in ["Play Highway to Hell", "Pause the music", "What classes do I have tomorrow?", "Open Spotify", "Make the orb bigger"] {
            assert_eq!(classify_addressee(command, &gaming), Addressee::DirectIntent, "{command}");
        }
        let open = AddresseeContext { conversation_open: true, ..gaming };
        assert_eq!(classify_addressee("Yes, do that for the whole week", &open), Addressee::Conversation);
        assert_eq!(classify_addressee("Yes, please.", &open), Addressee::Conversation, "an answer to JAI's question");
        assert_eq!(classify_addressee("Yes, please.", &gaming), Addressee::Chatter, "no open question: banter");
        assert!(is_game_or_voice_chat("Discord.exe") && is_game_or_voice_chat("javaw") && !is_game_or_voice_chat("Code"));
        // "Close" from a Discord friend never hides JAI; said to him it does. (With nothing of
        // his up: a block another test has on the board makes a bare "close" an order for it.)
        let (_dock, _board) = (crate::hud_dock::test_lock(), crate::hud_board::test_lock());
        if !crate::hud_board::is_active() && crate::hud_dock::open_blocks().is_empty() {
            assert_eq!(classify_addressee("Close", &gaming), Addressee::Chatter);
        }
        let hands_free = AddresseeContext { wake_required: false, ..ctx() };
        assert_eq!(classify_addressee("Minimize", &hands_free), Addressee::Unaddressed);
        assert_eq!(classify_addressee("Close", &AddresseeContext { wake_word: true, ..ctx() }), Addressee::WakeWord);
        assert_eq!(classify_addressee("Pause", &gaming), Addressee::DirectIntent, "music commands still work over a game");
    }
}

/// Monotonic milliseconds since process start.
pub fn now_ms() -> u64 {
    CLOCK.elapsed().as_millis() as u64
}

fn f32_atomic(v: f32) -> AtomicU32 {
    AtomicU32::new(v.to_bits())
}

fn load_f32(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

/// State shared between capture, TTS, the orchestrator and the HUD.
pub struct AudioControls {
    pub mic_enabled: AtomicBool,
    /// The self-test's guarantee: the microphone's audio is dropped whatever its switch or the
    /// push-to-talk key say (the test clicks the `[ MIC ]` chip; the room must not get in).
    deaf: AtomicBool,
    pub ptt_active: AtomicBool,
    pub wake_word_required: AtomicBool,
    /// IS_SPEAKING: JAI audio is being played.
    pub is_speaking: AtomicBool,
    /// An LLM/tool turn is in progress (thinking or speaking).
    pub turn_active: AtomicBool,
    /// Sir is mid-utterance (VAD speaking/hangover or push-to-talk held).
    pub in_speech: AtomicBool,
    pub mic_test_requested: AtomicBool,
    pub recalibrate_requested: AtomicBool,
    device_switch: AtomicBool,
    speech_tail_until: AtomicU64,
    window_until: AtomicU64,
    start_threshold: AtomicU32,
    release_threshold: AtomicU32,
    noise_multiplier: AtomicU32,
    hangover_ms: AtomicU32,
    noise_floor: AtomicU32,
    requested_mic: Mutex<Option<String>>,
    active_mic: Mutex<String>,
    /// Acoustic radar gestures: bit 0 = double clap, bit 1 = door knock.
    acoustic_flags: AtomicU8,
    acoustic_sensitivity: AtomicU32,
}

const CLAP: u8 = 1;
const KNOCK: u8 = 2;
/// How long after JAI's audio ends the microphone still counts what it hears as his echo.
const ECHO_TAIL_MS: u64 = 350;

fn acoustic_bits(a: &AcousticSettings) -> u8 {
    (if a.double_clap { CLAP } else { 0 }) | (if a.knock_alert { KNOCK } else { 0 })
}

impl AudioControls {
    pub fn new(cfg: &JaiConfig) -> Self {
        Self {
            mic_enabled: AtomicBool::new(true),
            deaf: AtomicBool::new(false),
            ptt_active: AtomicBool::new(false),
            wake_word_required: AtomicBool::new(cfg.wake_word_required),
            is_speaking: AtomicBool::new(false),
            turn_active: AtomicBool::new(false),
            in_speech: AtomicBool::new(false),
            mic_test_requested: AtomicBool::new(false),
            recalibrate_requested: AtomicBool::new(false),
            device_switch: AtomicBool::new(false),
            speech_tail_until: AtomicU64::new(0),
            window_until: AtomicU64::new(0),
            start_threshold: f32_atomic(cfg.vad_start_threshold),
            release_threshold: f32_atomic(cfg.vad_release_threshold),
            noise_multiplier: f32_atomic(cfg.vad_noise_multiplier),
            hangover_ms: AtomicU32::new(cfg.silence_hangover_ms),
            noise_floor: f32_atomic(0.0),
            requested_mic: Mutex::new(cfg.preferred_mic.clone()),
            active_mic: Mutex::new("—".to_string()),
            acoustic_flags: AtomicU8::new(acoustic_bits(&cfg.acoustic)),
            acoustic_sensitivity: f32_atomic(cfg.acoustic.sensitivity),
        }
    }

    /// Applies changed acoustic-radar settings live.
    pub fn set_acoustic(&self, a: &AcousticSettings) {
        self.acoustic_flags.store(acoustic_bits(a), Ordering::Release);
        self.acoustic_sensitivity.store(a.sensitivity.clamp(0.5, 2.0).to_bits(), Ordering::Relaxed);
    }

    pub fn vad_live(&self) -> (f32, f32, f32, u32) {
        (
            load_f32(&self.start_threshold),
            load_f32(&self.release_threshold),
            load_f32(&self.noise_multiplier),
            self.hangover_ms.load(Ordering::Relaxed),
        )
    }

    /// The room's noise as the VAD measures it right now (linear, 0-1).
    pub fn noise_floor_now(&self) -> f32 {
        f32::from_bits(self.noise_floor.load(Ordering::Relaxed))
    }

    pub fn set_vad(&self, start: Option<f32>, release: Option<f32>, hangover_ms: Option<u32>, multiplier: Option<f32>) {
        if let Some(s) = start {
            self.start_threshold.store(s.clamp(0.05, 0.98).to_bits(), Ordering::Relaxed);
        }
        let start = load_f32(&self.start_threshold);
        let release = release.unwrap_or_else(|| load_f32(&self.release_threshold)).clamp(0.02, start);
        self.release_threshold.store(release.to_bits(), Ordering::Relaxed);
        if let Some(h) = hangover_ms {
            self.hangover_ms.store(h.clamp(96, 3000), Ordering::Relaxed);
        }
        if let Some(m) = multiplier {
            self.noise_multiplier.store(m.clamp(1.0, 10.0).to_bits(), Ordering::Relaxed);
        }
    }

    /// Marks playback start/stop. Stopping leaves a 350 ms echo-tail guard (room reverb, the
    /// speaker's last syllable reaching the mic): JAI never hears himself as a wake word.
    pub fn set_speaking(&self, speaking: bool) {
        self.is_speaking.store(speaking, Ordering::Release);
        if !speaking {
            self.speech_tail_until.store(now_ms() + ECHO_TAIL_MS, Ordering::Release);
        }
    }

    /// JAI makes a short sound of his own (a chime) for `ms`: the microphone treats it and the
    /// echo tail after it as his voice.
    pub fn guard_echo(&self, ms: u64) {
        self.speech_tail_until.fetch_max(now_ms() + ms + ECHO_TAIL_MS, Ordering::AcqRel);
    }

    pub fn speaking_or_tail(&self) -> bool {
        self.is_speaking.load(Ordering::Acquire) || now_ms() < self.speech_tail_until.load(Ordering::Acquire)
    }

    /// Opens the active conversation window (wake word not needed until it expires).
    pub fn open_window(&self, secs: u64) {
        self.window_until.store(now_ms() + secs * 1000, Ordering::Release);
    }

    pub fn close_window(&self) {
        self.window_until.store(0, Ordering::Release);
    }

    pub fn window_remaining_ms(&self) -> u64 {
        self.window_until.load(Ordering::Acquire).saturating_sub(now_ms())
    }

    /// From now on nothing the microphone picks up is processed, whatever its switch says (the
    /// self-test, which clicks that switch).
    pub fn make_deaf(&self) {
        self.deaf.store(true, Ordering::Release);
    }

    pub fn request_mic(&self, name: Option<String>) {
        let name = name.filter(|n| !n.trim().is_empty() && !n.eq_ignore_ascii_case("auto"));
        *self.requested_mic.lock().unwrap_or_else(|e| e.into_inner()) = name;
        self.device_switch.store(true, Ordering::Release);
    }

    pub fn requested_mic(&self) -> Option<String> {
        self.requested_mic.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn active_mic(&self) -> String {
        self.active_mic.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_active_mic(&self, name: &str) {
        *self.active_mic.lock().unwrap_or_else(|e| e.into_inner()) = name.to_string();
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioLevel {
    pub rms: f32,
    pub rms_db: f32,
    pub probability: f32,
    pub noise_floor: f32,
    pub start_gate: f32,
    pub start_threshold: f32,
    pub release_threshold: f32,
    pub phase: &'static str,
    pub speaking: bool,
}

#[derive(Debug)]
pub struct Utterance {
    pub pcm: Vec<f32>,
    pub speech_ms: u32,
    pub silence_ms: u32,
    pub reason: EndReason,
    pub barge_in: bool,
    pub ptt: bool,
    /// Speech started inside the conversation window or during an active turn.
    pub in_window: bool,
    pub ended_at: Instant,
}

#[derive(Debug)]
pub enum AudioEvent {
    Level(AudioLevel),
    SpeechStarted { barge_in: bool, ptt: bool },
    /// A pause that may be the end of the utterance: the words so far, exactly as `Utterance`
    /// will carry them if nothing more is said, so Whisper can start on them now.
    Paused { pcm: Vec<f32>, speech_ms: u32 },
    /// While Sir is still speaking: the words so far, for the live line on the HUD (sent every
    /// `hud_lines::LIVE_EVERY`; nothing waits for their transcript).
    SoFar { pcm: Vec<f32>, speech_ms: u32 },
    /// Sir carried on after such a pause.
    Resumed,
    Utterance(Utterance),
    Discarded { speech_ms: u32 },
    Calibrated { noise_floor: f32 },
    MicTest { pcm: Vec<f32>, peak: f32, rms: f32, mic: String },
    DeviceActive { name: String, sample_rate: u32, channels: u16 },
    /// The acoustic radar: a double clap or a knock on the door.
    Acoustic(AcousticEvent),
}

pub fn list_input_devices() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

fn is_virtual(name: &str) -> bool {
    let l = name.to_lowercase();
    ["stereo mix", "stereomix", "what u hear", "vb-audio", "cable output", "voicemeeter", "steam streaming", "loopback", "monitor of"]
        .iter()
        .any(|k| l.contains(k))
}

/// Preferred device (exact, then substring match); otherwise the default input plus one more
/// physical microphone for failover.
fn select_input_devices(preferred: Option<&str>) -> Vec<(cpal::Device, String)> {
    let host = cpal::default_host();
    let all: Vec<(cpal::Device, String)> = host
        .input_devices()
        .map(|it| it.filter_map(|d| d.name().ok().map(|n| (d, n))).collect())
        .unwrap_or_default();

    if let Some(pref) = preferred {
        let p = pref.to_lowercase();
        let hit = all
            .iter()
            .position(|(_, n)| n.to_lowercase() == p)
            .or_else(|| all.iter().position(|(_, n)| n.to_lowercase().contains(&p)));
        if let Some(i) = hit {
            return all.into_iter().skip(i).take(1).collect();
        }
        warn!("[MIC] Preferred microphone '{pref}' not found; using automatic selection.");
    }

    let default_name = host.default_input_device().and_then(|d| d.name().ok());
    let mut chosen: Vec<(cpal::Device, String)> = Vec::new();
    let mut rest: Vec<(cpal::Device, String)> = Vec::new();
    for (d, n) in all {
        if Some(&n) == default_name.as_ref() && chosen.is_empty() {
            chosen.push((d, n));
        } else if !is_virtual(&n) && d.default_input_config().is_ok() {
            rest.push((d, n));
        }
    }
    if chosen.is_empty() {
        if let Some(first) = rest.pop() {
            chosen.push(first);
        }
    }
    if let Some(backup) = rest.into_iter().next() {
        chosen.push(backup);
    }
    chosen
}

struct DeviceFeed {
    name: String,
    sample_rate: u32,
    channels: u16,
    consumer: HeapConsumer<f32>,
}

fn build_input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut producer: HeapProducer<f32>,
    failed: Arc<AtomicBool>,
    name: String,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            if channels == 1 {
                for &s in data {
                    let _ = producer.push(f32::from_sample(s));
                }
                return;
            }
            // Some USB mics deliver signal on one channel of a stereo stream only; averaging
            // would halve the level, so use the live channel when the others are (near) silent.
            let tracked = channels.min(8);
            let mut energy = [0.0f32; 8];
            for frame in data.chunks_exact(channels) {
                for (c, &s) in frame.iter().take(tracked).enumerate() {
                    let v = f32::from_sample(s);
                    energy[c] += v * v;
                }
            }
            let (best, max_e) = energy[..tracked]
                .iter()
                .enumerate()
                .fold((0, 0.0f32), |acc, (i, &e)| if e > acc.1 { (i, e) } else { acc });
            let min_e = energy[..tracked].iter().copied().fold(f32::MAX, f32::min);
            let single = max_e > 1e-9 && min_e < max_e * 0.01;
            for frame in data.chunks_exact(channels) {
                let v = if single {
                    f32::from_sample(frame[best])
                } else {
                    frame.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / channels as f32
                };
                let _ = producer.push(v);
            }
        },
        move |err| {
            warn!("[MIC] Stream error on '{name}': {err}");
            failed.store(true, Ordering::Release);
        },
        None,
    )
}

fn open_input(device: &cpal::Device, name: &str, failed: &Arc<AtomicBool>) -> Result<(cpal::Stream, DeviceFeed), String> {
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let (producer, consumer) = HeapRb::<f32>::new(sample_rate as usize * 2).split();
    let f = Arc::clone(failed);
    let n = name.to_string();
    let stream = match format {
        SampleFormat::F32 => build_input::<f32>(device, &config, producer, f, n),
        SampleFormat::I16 => build_input::<i16>(device, &config, producer, f, n),
        SampleFormat::U16 => build_input::<u16>(device, &config, producer, f, n),
        SampleFormat::I32 => build_input::<i32>(device, &config, producer, f, n),
        SampleFormat::U8 => build_input::<u8>(device, &config, producer, f, n),
        SampleFormat::F64 => build_input::<f64>(device, &config, producer, f, n),
        other => return Err(format!("unsupported sample format {other:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    info!("[MIC] Capturing '{name}' ({sample_rate} Hz, {channels} ch, {format:?})");
    Ok((
        stream,
        DeviceFeed {
            name: name.to_string(),
            sample_rate,
            channels,
            consumer,
        },
    ))
}

fn mic_thread(controls: Arc<AudioControls>, feeds_tx: mpsc::Sender<Vec<DeviceFeed>>) {
    let failed = Arc::new(AtomicBool::new(false));
    let mut warned_empty = false;
    loop {
        let requested = controls.requested_mic();
        let devices = select_input_devices(requested.as_deref());
        if devices.is_empty() {
            if !warned_empty {
                warn!("[MIC] No microphone found; retrying every 2 s.");
                warned_empty = true;
            }
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        warned_empty = false;

        let mut streams = Vec::new();
        let mut feeds = Vec::new();
        for (device, name) in &devices {
            match open_input(device, name, &failed) {
                Ok((stream, feed)) => {
                    streams.push(stream);
                    feeds.push(feed);
                }
                Err(e) => warn!("[MIC] Could not open '{name}': {e}"),
            }
        }
        if feeds.is_empty() {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        if feeds_tx.send(feeds).is_err() {
            return;
        }
        failed.store(false, Ordering::Release);
        while !controls.device_switch.swap(false, Ordering::AcqRel) {
            if failed.swap(false, Ordering::AcqRel) {
                warn!("[MIC] Input stream failed (device unplugged?); reopening.");
                std::thread::sleep(Duration::from_millis(500));
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(streams);
    }
}

struct FeedState {
    feed: DeviceFeed,
    resampler: StreamResampler,
    last_signal: Instant,
}

/// Starts capture + VAD. Events are delivered on `events`.
pub fn spawn(cfg: &JaiConfig, controls: Arc<AudioControls>, events: UnboundedSender<AudioEvent>) {
    let (feeds_tx, feeds_rx) = mpsc::channel::<Vec<DeviceFeed>>();
    let mic_controls = Arc::clone(&controls);
    if let Err(e) = std::thread::Builder::new()
        .name("jai-mic".into())
        .spawn(move || crate::crash_guard::supervise("jai-mic", || mic_thread(Arc::clone(&mic_controls), feeds_tx.clone())))
    {
        error!("[MIC] Could not start capture thread: {e}");
        return;
    }

    let cfg = cfg.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("jai-vad".into())
        .spawn(move || crate::crash_guard::supervise("jai-vad", || vad_thread(cfg.clone(), Arc::clone(&controls), &feeds_rx, events.clone())))
    {
        error!("[VAD] Could not start VAD thread: {e}");
    }
}

fn vad_thread(
    cfg: JaiConfig,
    controls: Arc<AudioControls>,
    feeds_rx: &mpsc::Receiver<Vec<DeviceFeed>>,
    events: UnboundedSender<AudioEvent>,
) {
    crate::platform::raise_thread_priority(true);
    let mut silero = match SileroVad::load(Path::new(&cfg.silero_vad_model_path)) {
        Ok(s) => Some(s),
        Err(e) => {
            error!("[VAD] Silero VAD unavailable ({e}); falling back to energy-only detection.");
            None
        }
    };
    let mut machine = VadMachine::new(VadParams::from_config(&cfg));
    // The energy gate listens to the voice band (100 Hz – 3.5 kHz), not the whole room.
    let mut band = vad::VoiceBand::new();
    let mut radar = AcousticRadar::new(cfg.acoustic.sensitivity);
    let mut feeds: Vec<FeedState> = Vec::new();
    let mut active = 0usize;
    let mut pcm16: Vec<f32> = Vec::with_capacity(8192);
    let mut scratch = vec![0.0f32; 8192];
    let mut mic_test: Option<Vec<f32>> = None;
    let mut last_level = Instant::now();
    // When the words so far last went to the live line (None between utterances).
    let mut live_at: Option<Instant> = None;
    let mut started_in_window = false;
    let mut muted_reported = false;

    loop {
        match feeds_rx.try_recv() {
            Ok(new_feeds) => {
                feeds = new_feeds
                    .into_iter()
                    .map(|f| FeedState {
                        resampler: StreamResampler::new(f.sample_rate, vad::SAMPLE_RATE),
                        feed: f,
                        last_signal: Instant::now(),
                    })
                    .collect();
                active = 0;
                pcm16.clear();
                if let Some(s) = silero.as_mut() {
                    s.reset();
                }
                machine.recalibrate();
                let label = if feeds.len() > 1 {
                    format!("{} (auto, failover: {})", feeds[0].feed.name, feeds[1].feed.name)
                } else {
                    feeds[0].feed.name.clone()
                };
                controls.set_active_mic(&label);
                let _ = events.send(AudioEvent::DeviceActive {
                    name: feeds[0].feed.name.clone(),
                    sample_rate: feeds[0].feed.sample_rate,
                    channels: feeds[0].feed.channels,
                });
            }
            Err(mpsc::TryRecvError::Disconnected) if feeds.is_empty() => return,
            Err(_) => {}
        }
        if feeds.is_empty() {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        let mut got_audio = false;
        for (i, f) in feeds.iter_mut().enumerate() {
            loop {
                let n = f.feed.consumer.pop_slice(&mut scratch);
                if n == 0 {
                    break;
                }
                got_audio = true;
                if dsp::rms(&scratch[..n]) > 0.0008 {
                    f.last_signal = Instant::now();
                }
                if i == active {
                    f.resampler.process(&scratch[..n], &mut pcm16);
                }
            }
        }

        // Failover between microphones (only when the active one is dead and another is live).
        if feeds.len() > 1 && feeds[active].last_signal.elapsed() > Duration::from_millis(1500) {
            if let Some((i, _)) = feeds
                .iter()
                .enumerate()
                .find(|(i, f)| *i != active && f.last_signal.elapsed() < Duration::from_millis(300))
            {
                info!(
                    "[MIC] '{}' is silent; switched to '{}'.",
                    feeds[active].feed.name, feeds[i].feed.name
                );
                active = i;
                feeds[i].resampler.reset();
                pcm16.clear();
                machine.reset();
                controls.set_active_mic(&format!("{} (auto failover)", feeds[i].feed.name));
            }
        }

        let mut offset = 0;
        while pcm16.len() - offset >= FRAME {
            let frame = &pcm16[offset..offset + FRAME];
            offset += FRAME;

            let (start, release, multiplier, hangover) = controls.vad_live();
            machine.params.apply_live(start, release, multiplier, hangover);
            if controls.recalibrate_requested.swap(false, Ordering::AcqRel) {
                info!("[VAD] Recalibrating noise floor…");
                machine.recalibrate();
            }
            if controls.mic_test_requested.swap(false, Ordering::AcqRel) {
                info!("[MIC] Microphone test: recording 2.5 s…");
                mic_test = Some(Vec::with_capacity(40_000));
            }
            if let Some(buf) = mic_test.as_mut() {
                buf.extend_from_slice(frame);
                if buf.len() >= 40_000 {
                    let pcm = mic_test.take().unwrap_or_default();
                    let _ = events.send(AudioEvent::MicTest {
                        peak: dsp::peak(&pcm),
                        rms: dsp::rms(&pcm),
                        mic: controls.active_mic(),
                        pcm,
                    });
                }
            }

            let rms = band.rms(frame);
            let ptt = controls.ptt_active.load(Ordering::Acquire);
            if controls.deaf.load(Ordering::Acquire) || (!controls.mic_enabled.load(Ordering::Acquire) && !ptt) {
                if machine.in_utterance() {
                    machine.reset();
                }
                controls.in_speech.store(false, Ordering::Release);
                if !muted_reported {
                    muted_reported = true;
                    let _ = events.send(AudioEvent::Level(AudioLevel {
                        rms: 0.0,
                        rms_db: -120.0,
                        probability: 0.0,
                        noise_floor: machine.noise_floor(),
                        start_gate: machine.start_gate(),
                        start_threshold: start,
                        release_threshold: release,
                        phase: "muted",
                        speaking: false,
                    }));
                }
                continue;
            }
            muted_reported = false;

            let prob = match silero.as_mut() {
                Some(s) => s.predict(frame),
                None => vad::energy_probability(rms, machine.noise_floor()),
            };
            let assistant_speaking = controls.speaking_or_tail();
            // Claps and knocks only count while nobody is talking (JAI or Sir).
            let flags = controls.acoustic_flags.load(Ordering::Acquire);
            if flags != 0 {
                radar.set_sensitivity(load_f32(&controls.acoustic_sensitivity));
                let listening = !assistant_speaking && !ptt && mic_test.is_none() && !machine.in_utterance();
                if let Some(ev) = radar.process(frame, listening) {
                    let wanted = match ev {
                        AcousticEvent::DoubleClap => flags & CLAP != 0,
                        AcousticEvent::Knock { .. } => flags & KNOCK != 0,
                    };
                    if wanted {
                        info!("[RADAR] {ev:?}");
                        let _ = events.send(AudioEvent::Acoustic(ev));
                    }
                }
            } else {
                radar.reset_patterns();
            }
            let ctx = FrameContext {
                assistant_speaking,
                ptt,
                suspended: mic_test.is_some(),
            };

            if let Some(event) = machine.process(frame, prob, rms, ctx) {
                match event {
                    VadEvent::Calibrated { noise_floor } => {
                        info!("[VAD] Noise floor calibrated: rms {noise_floor:.4} ({:.1} dBFS)", dsp::to_dbfs(noise_floor));
                        let _ = events.send(AudioEvent::Calibrated { noise_floor });
                    }
                    VadEvent::SpeechStarted { barge_in, ptt } => {
                        started_in_window = controls.window_remaining_ms() > 0
                            || controls.turn_active.load(Ordering::Acquire);
                        info!(
                            "[VAD] Speech started (p={prob:.2}, rms={rms:.4}, floor={:.4}, {:.0} dB over the room{}{})",
                            machine.noise_floor(),
                            dsp::to_dbfs(rms) - dsp::to_dbfs(machine.noise_floor()),
                            if barge_in { ", BARGE-IN" } else { "" },
                            if ptt { ", PTT" } else { "" }
                        );
                        let _ = events.send(AudioEvent::SpeechStarted { barge_in, ptt });
                    }
                    VadEvent::SpeechEnded { pcm, speech_ms, silence_ms, reason, barge_in, ptt } => {
                        info!(
                            "[VAD] Speech ended ({}; {} ms voiced, {} ms audio)",
                            match reason {
                                EndReason::Silence => format!("{silence_ms}ms silence"),
                                EndReason::MaxLength => "max length".to_string(),
                                EndReason::PttReleased => "push-to-talk released".to_string(),
                            },
                            speech_ms,
                            pcm.len() / 16
                        );
                        let _ = events.send(AudioEvent::Utterance(Utterance {
                            pcm,
                            speech_ms,
                            silence_ms,
                            reason,
                            barge_in,
                            ptt,
                            in_window: started_in_window,
                            ended_at: Instant::now(),
                        }));
                    }
                    VadEvent::SpeechPaused { pcm, speech_ms } => {
                        debug!("[VAD] A pause after {speech_ms} ms of voice: the words so far go ahead to Whisper");
                        let _ = events.send(AudioEvent::Paused { pcm, speech_ms });
                    }
                    VadEvent::SpeechResumed => {
                        debug!("[VAD] Sir carried on");
                        let _ = events.send(AudioEvent::Resumed);
                    }
                    VadEvent::Discarded { speech_ms } => {
                        debug!("[VAD] Discarded {speech_ms} ms blip");
                        let _ = events.send(AudioEvent::Discarded { speech_ms });
                    }
                }
            }
            controls.noise_floor.store(machine.noise_floor().to_bits(), Ordering::Relaxed);
            controls.in_speech.store(machine.in_utterance(), Ordering::Release);
            // The live line: while he speaks, the words so far go out now and then.
            if !machine.in_utterance() {
                live_at = None;
            } else if live_at.get_or_insert_with(Instant::now).elapsed() >= crate::hud_lines::LIVE_EVERY {
                live_at = Some(Instant::now());
                if let Some((pcm, speech_ms)) = machine.so_far() {
                    let _ = events.send(AudioEvent::SoFar { pcm, speech_ms });
                }
            }

            if last_level.elapsed() >= Duration::from_millis(50) {
                last_level = Instant::now();
                let _ = events.send(AudioEvent::Level(AudioLevel {
                    rms,
                    rms_db: dsp::to_dbfs(rms),
                    probability: prob,
                    noise_floor: machine.noise_floor(),
                    start_gate: machine.start_gate(),
                    start_threshold: machine.params.start_threshold,
                    release_threshold: machine.params.release_threshold,
                    phase: if machine.phase() == VadPhase::Idle && mic_test.is_some() {
                        "mic_test"
                    } else {
                        machine.phase().as_str()
                    },
                    speaking: assistant_speaking,
                }));
            }
        }
        pcm16.drain(..offset);

        if !got_audio {
            std::thread::sleep(Duration::from_millis(4));
        }
    }
}
