//! Anti-repetition: JAI never restates what Sir heard a moment ago and never opens three
//! replies in a row with the same "Very good, Sir."
//!
//! * [`explicit_repeat_requested`]: "repeat that", "say that again", "bitte wiederholen",
//!   "nochmal" — the only case in which a reply may be repeated verbatim.
//! * [`RecentResponsesRingBuffer`]: the last six replies (with Sir's request), the source of the
//!   per-turn "Recently said" section of the live context and of the deduplication below.
//! * [`SentenceGate`]: streamed reply text passes through it before it reaches the TTS or the
//!   chat. Every sentence is compared with the sentences of the last three replies (and with the
//!   ones already said in this reply) using Jaro-Winkler; one that is more than
//!   [`DEDUP_SIMILARITY`] alike is dropped. Text that is clearly new is released as soon as a few
//!   words of it are known, so the first audio is not held back.
//! * [`vary_opening`]: the canned fast-path confirmations rotate their courtesy opener (or skip
//!   it and go straight to the point) when the same one was used recently.

use std::collections::VecDeque;
use std::time::Instant;

/// A sentence this similar (Jaro-Winkler) to one said in the last three replies is not said again.
pub const DEDUP_SIMILARITY: f64 = 0.78;
/// Two whole replies this similar ([`reply_similarity`]) are the same answer: the older copy
/// leaves the context window, and a fresh reply this close to the previous one is re-asked.
pub const REPLY_SIMILARITY: f64 = 0.65;
/// Replies kept in the ring buffer.
const RING_CAPACITY: usize = 6;
/// Replies a new one is compared with.
pub const COMPARE_TURNS: usize = 3;

/// British acknowledgements JAI rotates through instead of always saying "Very good, Sir."
pub const ACKNOWLEDGEMENTS: &[&str] = &[
    "Right away, Sir.",
    "On it.",
    "Consider it done, Sir.",
    "At once.",
    "Of course, Sir.",
    "Certainly, Sir.",
    "Straight away, Sir.",
    "As you wish, Sir.",
    "Very good, Sir.",
];

/// Short courtesy sentences that only acknowledge (the openers `vary_opening` may swap).
const COURTESY: &[&str] = &[
    "very good sir", "right away sir", "of course sir", "certainly sir", "done sir", "on it", "on it sir", "at once", "at once sir",
    "consider it done sir", "consider it done", "straight away sir", "as you wish sir", "very well sir", "absolutely sir", "understood sir",
    "noted sir", "there you are sir", "of course", "certainly", "right away", "very good", "done",
];

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn numbers(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn word_overlap(a: &str, b: &str) -> f64 {
    let wa: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let wb: std::collections::HashSet<&str> = b.split_whitespace().collect();
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    wa.intersection(&wb).count() as f64 / wa.union(&wb).count() as f64
}

/// Word-level Dice similarity of two whole replies (0-1): the share of distinct words they have
/// in common. Unrelated replies that only share "the", "Sir" and "you" stay well below 0.4;
/// a reply parroted with a word or two changed scores above 0.8.
pub fn reply_similarity(a: &str, b: &str) -> f64 {
    let (na, nb) = (normalize(a), normalize(b));
    let wa: std::collections::HashSet<&str> = na.split_whitespace().collect();
    let wb: std::collections::HashSet<&str> = nb.split_whitespace().collect();
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    2.0 * wa.intersection(&wb).count() as f64 / (wa.len() + wb.len()) as f64
}

/// [`reply_similarity`] of a reply still being written with the same number of opening words of
/// `previous` (a parrot is recognised before it has finished).
pub fn prefix_similarity(partial: &str, previous: &str) -> f64 {
    let n = normalize(partial).split_whitespace().count();
    let prefix: String = normalize(previous).split_whitespace().take(n.max(1)).collect::<Vec<_>>().join(" ");
    reply_similarity(partial, &prefix)
}

/// Sir explicitly asked to hear the last reply again.
pub fn explicit_repeat_requested(text: &str) -> bool {
    let full = normalize(text);
    let full = full.trim_start_matches("jai ").trim_end_matches(" jai").trim().to_string();
    let s = full.trim_end_matches(" please").trim_end_matches(" bitte").trim();
    let s = s.strip_prefix("please ").or_else(|| s.strip_prefix("bitte ")).unwrap_or(s).trim();
    if s.is_empty() || full.split_whitespace().count() > 9 {
        return false;
    }
    const EXACT: &[&str] = &[
        "repeat", "repeat that", "repeat it", "repeat please", "pardon", "pardon me", "come again", "sorry what", "what", "say again",
        "again please", "nochmal", "noch mal", "nochmals", "noch einmal", "wie bitte", "bitte wiederholen", "wiederholen", "wiederhol das",
        "wiederhole das", "was", "sag nochmal", "sag das nochmal", "sag es nochmal", "sag das noch mal", "sag das noch einmal", "one more time",
    ];
    if EXACT.contains(&s) || EXACT.contains(&full.as_str()) {
        return true;
    }
    const PHRASES: &[&str] = &[
        "repeat that", "repeat what you", "repeat your last", "repeat the last", "say that again", "say it again", "say that one more time",
        "what did you just say", "what did you say", "what was that", "didnt catch that", "did not catch that", "didnt hear that",
        "didnt hear you", "can you repeat", "could you repeat", "would you repeat", "please repeat", "kannst du das wiederholen",
        "kannst du das nochmal sagen", "was hast du gesagt", "was hast du gerade gesagt", "was hast du eben gesagt", "wiederhol das",
        "wiederhole das", "bitte wiederholen", "sag das nochmal", "sag das noch mal", "sag es nochmal", "hab ich nicht verstanden",
        "habe ich nicht verstanden", "nicht verstanden",
    ];
    PHRASES.iter().any(|p| s.contains(p))
}

/// Splits a reply into sentences (keeps their punctuation, drops empty pieces).
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(end) = sentence_end(rest) {
        let piece = rest[..end].trim();
        if piece.chars().any(char::is_alphanumeric) {
            out.push(piece.to_string());
        }
        rest = &rest[end..];
    }
    if rest.chars().any(char::is_alphanumeric) {
        out.push(rest.trim().to_string());
    }
    out
}

/// End (exclusive, after the following whitespace) of the first complete sentence in `s`. A
/// full stop needs one character of look-ahead ("3.5", "e.g.").
fn sentence_end(s: &str) -> Option<usize> {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    for (k, &(i, c)) in chars.iter().enumerate() {
        if !matches!(c, '.' | '!' | '?' | '\n' | '…') {
            continue;
        }
        let next = chars.get(k + 1).map(|&(_, n)| n);
        if c != '\n' && !next.is_some_and(char::is_whitespace) {
            continue;
        }
        if c == '.' {
            let word: String = s[..i].rsplit(char::is_whitespace).next().unwrap_or("").to_lowercase();
            if matches!(word.as_str(), "mr" | "mrs" | "dr" | "st" | "vs" | "e.g" | "i.e" | "etc" | "ca" | "approx") || (word.len() >= 2 && word.split('.').all(|p| p.chars().count() <= 1)) {
                continue;
            }
        }
        let mut end = i + c.len_utf8();
        while let Some(ch) = s[end..].chars().next().filter(|ch| ch.is_whitespace()) {
            end += ch.len_utf8();
        }
        return Some(end);
    }
    None
}

/// Sentences that carry an answer of their own ("Yes, Sir.", "I'm afraid not.") are never
/// dropped, however often they come up.
fn is_answer_word(norm: &str) -> bool {
    let first = norm.split_whitespace().next().unwrap_or("");
    matches!(first, "yes" | "no" | "nope" | "yep" | "ja" | "nein" | "correct" | "indeed") || norm.contains(" not ") || norm.ends_with(" not") || norm.contains("afraid")
}

/// Whether `sentence` restates one of `recent` (numbers must match too: "CPU at 20 %" and "CPU at
/// 45 %" are different facts).
pub fn is_redundant(sentence: &str, recent: &[String]) -> bool {
    let s = normalize(sentence);
    let words = s.split_whitespace().count();
    if words == 0 || is_answer_word(&s) {
        return false;
    }
    let digits = numbers(&s);
    recent.iter().any(|r| {
        let rn = normalize(r);
        if rn.is_empty() || numbers(&rn) != digits {
            return false;
        }
        let sim = strsim::jaro_winkler(&s, &rn);
        if words < 4 {
            // Short pieces are only "the same" when they really are ("Very good, Sir.").
            sim > 0.92
        } else {
            sim > DEDUP_SIMILARITY && (word_overlap(&s, &rn) >= 0.5 || sim > 0.95)
        }
    })
}

/// The courtesy opener of a reply ("Very good, Sir."), if it starts with one.
pub fn opener_of(text: &str) -> Option<String> {
    let first = split_sentences(text).into_iter().next()?;
    let norm = normalize(&first);
    COURTESY.contains(&norm.as_str()).then_some(first)
}

/// One reply JAI gave.
#[derive(Debug, Clone)]
pub struct RecentResponse {
    pub request: String,
    pub text: String,
    pub sentences: Vec<String>,
    pub opener: Option<String>,
    pub at: Instant,
}

/// The last six replies, newest last.
#[derive(Debug, Default)]
pub struct RecentResponsesRingBuffer {
    items: VecDeque<RecentResponse>,
}

impl RecentResponsesRingBuffer {
    pub fn push(&mut self, request: &str, reply: &str) {
        let text = reply.trim().trim_end_matches('…').trim().to_string();
        if text.is_empty() {
            return;
        }
        self.items.push_back(RecentResponse {
            request: request.trim().to_string(),
            sentences: split_sentences(&text),
            opener: opener_of(&text),
            text,
            at: Instant::now(),
        });
        while self.items.len() > RING_CAPACITY {
            self.items.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn last(&self) -> Option<&RecentResponse> {
        self.items.back()
    }

    fn recent(&self, turns: usize) -> impl Iterator<Item = &RecentResponse> {
        self.items.iter().rev().take(turns)
    }

    /// Sentences of the last `turns` replies.
    pub fn recent_sentences(&self, turns: usize) -> Vec<String> {
        self.recent(turns).flat_map(|r| r.sentences.iter().cloned()).collect()
    }

    /// Courtesy openers of the last `turns` replies, normalised.
    pub fn recent_openers(&self, turns: usize) -> Vec<String> {
        self.recent(turns).filter_map(|r| r.opener.as_deref().map(normalize)).collect()
    }

    /// Sir asks the very same thing again (he wants the answer again, so it is not stripped).
    pub fn same_request_again(&self, request: &str) -> bool {
        let n = normalize(request);
        n.split_whitespace().count() >= 2
            && self.recent(COMPARE_TURNS).any(|r| r.at.elapsed() < std::time::Duration::from_secs(15 * 60) && strsim::jaro_winkler(&normalize(&r.request), &n) > 0.93)
    }

    /// The per-turn "Recently said" section of the live context (None before the first reply).
    pub fn do_not_repeat_block(&self) -> Option<String> {
        if self.items.is_empty() {
            return None;
        }
        let openers: Vec<String> = self.recent(COMPARE_TURNS).filter_map(|r| r.opener.clone()).collect();
        let facts: Vec<String> = self
            .recent(COMPARE_TURNS)
            .flat_map(|r| r.sentences.iter().filter(|s| opener_of(s).is_none()).cloned())
            .filter(|s| s.split_whitespace().count() >= 3)
            .take(8)
            .map(|s| format!("- \"{}\"", s.chars().take(150).collect::<String>()))
            .collect();
        // Plain text on purpose: bracketed headers make small models answer with bracketed notes
        // of their own ("[Answered your request with varied phrasing …]").
        let mut out = String::from(
            "Recently said: Sir already heard your last replies. Answer only what he asks now; do not restate the facts below unless he asks again, and do not reuse an opening you just used — vary it (\"Right away, Sir.\", \"On it.\", \"Consider it done, Sir.\", \"At once.\") or go straight to the answer.\n",
        );
        if !openers.is_empty() {
            out.push_str(&format!("Openings just used: {}\n", openers.iter().map(|o| format!("\"{o}\"")).collect::<Vec<_>>().join(", ")));
        }
        if !facts.is_empty() {
            out.push_str("Already stated:\n");
            out.push_str(&facts.join("\n"));
            out.push('\n');
        }
        Some(out.trim_end().to_string())
    }
}

/// A fast-path confirmation with a fresh opener: when its courtesy opener was used in the last
/// three replies it becomes another one, or (every third time) is dropped altogether.
pub fn vary_opening(line: &str, recent_openers: &[String], seed: usize) -> String {
    let Some(opener) = opener_of(line) else { return line.to_string() };
    let used = normalize(&opener);
    if !recent_openers.contains(&used) {
        return line.to_string();
    }
    let rest = line.trim_start()[opener.len().min(line.trim_start().len())..].trim_start().to_string();
    if !rest.is_empty() && seed % 3 == 2 {
        return rest;
    }
    let fresh = (0..ACKNOWLEDGEMENTS.len())
        .map(|i| ACKNOWLEDGEMENTS[(seed + i) % ACKNOWLEDGEMENTS.len()])
        .find(|a| !recent_openers.contains(&normalize(a)) && normalize(a) != used);
    match fresh {
        Some(a) if rest.is_empty() => a.to_string(),
        Some(a) => format!("{a} {rest}"),
        None if !rest.is_empty() => rest,
        None => line.to_string(),
    }
}

/// Streamed reply text → only what has not been said already.
#[derive(Debug, Default)]
pub struct SentenceGate {
    pending: String,
    /// The pending sentence started out new and was already released (its rest follows).
    committed: bool,
    /// Sentences of the last three replies plus everything released in this one.
    recent: Vec<String>,
    /// Repeats are wanted (Sir asked again).
    allow: bool,
    stripped: Vec<String>,
    released_any: bool,
    current_released: String,
}

impl SentenceGate {
    pub fn new(recent: Vec<String>, allow_repeats: bool) -> Self {
        Self { recent, allow: allow_repeats, ..Self::default() }
    }

    /// Sentences dropped so far.
    pub fn stripped(&self) -> &[String] {
        &self.stripped
    }

    pub fn released_any(&self) -> bool {
        self.released_any
    }

    /// Sir asked for a repeat (or asked the same thing again).
    pub fn allows_repeats(&self) -> bool {
        self.allow
    }

    fn release(&mut self, text: &str, out: &mut String) {
        if text.is_empty() {
            return;
        }
        out.push_str(text);
        self.current_released.push_str(text);
        if text.chars().any(char::is_alphanumeric) {
            self.released_any = true;
        }
    }

    /// The pending sentence is over: remember it for in-reply duplicates.
    fn close_sentence(&mut self) {
        let done = std::mem::take(&mut self.current_released);
        if done.chars().any(char::is_alphanumeric) {
            self.recent.push(done.trim().to_string());
        }
    }

    /// The partial sentence is clearly not the start of something said recently.
    fn partial_is_novel(&self) -> bool {
        let p = normalize(&self.pending);
        if p.split_whitespace().count() < 4 {
            return false;
        }
        !self.recent.iter().any(|r| {
            let rn = normalize(r);
            let prefix: String = rn.chars().take(p.chars().count()).collect();
            strsim::jaro_winkler(&p, &prefix) > DEDUP_SIMILARITY
        })
    }

    /// Feeds streamed text; returns the text that may be spoken and shown now.
    pub fn push(&mut self, token: &str) -> String {
        self.pending.push_str(token);
        let mut out = String::new();
        while let Some(end) = sentence_end(&self.pending) {
            let sentence: String = self.pending[..end].to_string();
            self.pending.drain(..end);
            if self.committed {
                self.committed = false;
                self.release(&sentence, &mut out);
                self.close_sentence();
            } else if self.allow || !is_redundant(&sentence, &self.recent) {
                self.release(&sentence, &mut out);
                self.close_sentence();
            } else {
                tracing::info!("[REPEAT] Dropped a sentence Sir already heard: \"{}\"", sentence.trim());
                self.stripped.push(sentence.trim().to_string());
            }
        }
        if !self.committed && !self.pending.trim().is_empty() && (self.allow || self.partial_is_novel()) {
            self.committed = true;
        }
        if self.committed {
            let rest = std::mem::take(&mut self.pending);
            self.release(&rest, &mut out);
        }
        out
    }

    /// A draft that was cut off and will be asked again: whatever it left unjudged is forgotten.
    pub fn discard_pending(&mut self) {
        self.pending.clear();
        self.committed = false;
        self.current_released.clear();
    }

    /// The end of the stream: the last (unterminated) sentence is judged like the others.
    pub fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.pending);
        let mut out = String::new();
        if self.committed {
            self.committed = false;
            self.release(&rest, &mut out);
            self.close_sentence();
        } else if rest.chars().any(char::is_alphanumeric) {
            if self.allow || !is_redundant(&rest, &self.recent) {
                self.release(&rest, &mut out);
                self.close_sentence();
            } else {
                tracing::info!("[REPEAT] Dropped a sentence Sir already heard: \"{}\"", rest.trim());
                self.stripped.push(rest.trim().to_string());
            }
        } else {
            self.release(&rest, &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_repeat_requests_are_recognised() {
        for t in ["Repeat that, please.", "JAI, say that again", "What did you just say?", "Bitte wiederholen", "nochmal", "Wie bitte?", "Was hast du gerade gesagt?"] {
            assert!(explicit_repeat_requested(t), "{t}");
        }
        for t in ["Play that song again", "mach das nochmal", "open Spotify", "repeat the timer every day at 8 and remind me to stretch"] {
            assert!(!explicit_repeat_requested(t), "{t}");
        }
    }

    #[test]
    fn whole_reply_similarity_separates_parrots_from_new_answers() {
        let previous = "The Audio Devices tab has been disabled in the main J.A.I. window, Sir.";
        assert!(reply_similarity(previous, "The Audio Devices tab has been disabled in the main window, Sir.") > REPLY_SIMILARITY);
        assert!(reply_similarity(previous, "Your first lesson tomorrow is maths at eight, Sir.") < 0.4);
        assert!(prefix_similarity("The Audio Devices tab has been", previous) > REPLY_SIMILARITY);
        assert!(prefix_similarity("Right away, Sir. Jays Rock", previous) < REPLY_SIMILARITY);
        assert_eq!(reply_similarity("", previous), 0.0);
    }

    #[test]
    fn redundant_sentences_are_dropped_but_new_facts_stay() {
        let recent = vec!["Your CPU is at 20 percent and memory is fine.".to_string(), "Very good, Sir.".to_string()];
        assert!(is_redundant("Your CPU is at 20 percent and the memory is fine.", &recent));
        assert!(!is_redundant("Your CPU is at 45 percent and memory is fine.", &recent), "a changed number is news");
        assert!(is_redundant("Very good, Sir.", &recent));
        assert!(!is_redundant("Yes, Sir.", &["Yes, Sir.".to_string()]), "answers are never dropped");
        assert!(!is_redundant("Discord is on your second monitor now.", &recent));
    }

    #[test]
    fn gate_streams_new_text_early_and_drops_repeats() {
        let recent = vec!["The weather in Musterstadt is 12 degrees and cloudy.".to_string()];
        let mut g = SentenceGate::new(recent, false);
        let mut said = String::new();
        for t in ["The weather in Musterstadt ", "is 12 degrees and cloudy. ", "Tomorrow brings ", "rain from noon, ", "so take a jacket."] {
            said.push_str(&g.push(t));
        }
        said.push_str(&g.finish());
        assert_eq!(said.trim(), "Tomorrow brings rain from noon, so take a jacket.");
        assert_eq!(g.stripped().len(), 1);
        // A sentence repeated inside one reply is said once.
        let mut g = SentenceGate::new(Vec::new(), false);
        let mut said = g.push("Right away, Sir. Discord is open. Right away, Sir. ");
        said.push_str(&g.finish());
        assert_eq!(said.trim(), "Right away, Sir. Discord is open.");
        // When Sir asks again, nothing is held back.
        let mut g = SentenceGate::new(vec!["Discord is open.".into()], true);
        assert_eq!(format!("{}{}", g.push("Discord is open."), g.finish()), "Discord is open.");
    }

    #[test]
    fn openers_rotate_and_ring_builds_the_block() {
        let mut ring = RecentResponsesRingBuffer::default();
        ring.push("play jaysrock", "Very good, Sir. Jays Rock is playing.");
        let openers = ring.recent_openers(COMPARE_TURNS);
        assert_eq!(openers, vec!["very good sir".to_string()]);
        let varied = vary_opening("Very good, Sir. Back In Black is playing.", &openers, 0);
        assert!(!varied.starts_with("Very good"), "{varied}");
        assert!(varied.ends_with("Back In Black is playing."), "{varied}");
        assert_eq!(vary_opening("Paused, Sir.", &openers, 0), "Paused, Sir.");
        let block = ring.do_not_repeat_block().unwrap();
        assert!(block.starts_with("Recently said:") && !block.contains('[') && block.contains("Jays Rock is playing"));
        assert!(ring.same_request_again("Play JaysRock"));
        assert_eq!(split_sentences("It's 3.5 GHz. Done! Next"), vec!["It's 3.5 GHz.", "Done!", "Next"]);
    }
}
