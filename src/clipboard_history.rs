//! Clipboard history: the last twenty-five things Sir copied, so "what did I copy before that"
//! has an answer and an earlier copy can be put back ("show my clipboard history", "restore
//! the third one").
//!
//! The history lives in memory only: it is never written to disk, never logged and gone when
//! Jarvis quits. Anything that looks like a secret — a password-shaped token, an API key, a
//! private key block, a card or bank number — is not kept at all. Sir can pause it ("pause the
//! clipboard history") and clear it at any time.

use crate::features::{spoken, str_arg};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// How many entries are kept, and how long one may be.
pub const KEEP: usize = 25;
pub const MAX_CHARS: usize = 4_000;

/// Prefixes of well-known secrets.
const SECRET_PREFIXES: &[&str] = &["sk-", "sk_live_", "sk_test_", "ghp_", "gho_", "ghs_", "github_pat_", "xoxb-", "xoxp-", "AKIA", "AIza", "eyJ", "-----BEGIN", "glpat-", "hf_", "ya29."];

/// Whether a run of digits passes the Luhn check (payment cards).
fn luhn(digits: &str) -> bool {
    let mut sum = 0;
    for (i, c) in digits.chars().rev().enumerate() {
        let Some(mut d) = c.to_digit(10) else { return false };
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum % 10 == 0
}

/// Whether a copied text looks like something that must not be kept: a key or token by its
/// prefix, a private key block, a card number, an IBAN, or one long word mixing letters, digits
/// and symbols the way generated passwords do.
pub fn looks_secret(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if SECRET_PREFIXES.iter().any(|p| t.starts_with(p)) || t.contains("PRIVATE KEY-----") {
        return true;
    }
    let compact: String = t.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    // A payment card: 13-19 digits that pass the Luhn check.
    if (13..=19).contains(&compact.len()) && compact.chars().all(|c| c.is_ascii_digit()) && luhn(&compact) {
        return true;
    }
    // An IBAN: two letters, two digits, then 11-30 letters and digits.
    let iban = compact.to_uppercase();
    if (15..=34).contains(&iban.len()) && iban.chars().take(2).all(|c| c.is_ascii_uppercase()) && iban.chars().skip(2).take(2).all(|c| c.is_ascii_digit()) && iban.chars().skip(4).all(|c| c.is_ascii_alphanumeric()) && iban.chars().skip(4).filter(|c| c.is_ascii_digit()).count() >= 8 {
        return true;
    }
    // One word that reads like a generated password or a token.
    if !t.contains(char::is_whitespace) && !t.contains("://") && !t.contains('\\') && !t.contains('/') {
        let (lower, upper, digit, symbol) = (t.chars().any(|c| c.is_lowercase()), t.chars().any(|c| c.is_uppercase()), t.chars().any(|c| c.is_ascii_digit()), t.chars().any(|c| !c.is_alphanumeric()));
        let classes = [lower, upper, digit, symbol].iter().filter(|b| **b).count();
        let length = t.chars().count();
        // Long and mixed, or shorter with all four kinds of character.
        if (length >= 20 && classes >= 3 && digit) || ((10..20).contains(&length) && classes == 4) || (length >= 32 && t.chars().all(|c| c.is_ascii_hexdigit())) {
            return true;
        }
    }
    false
}

/// The history, newest first.
#[derive(Clone, Default)]
pub struct Ring {
    entries: Arc<Mutex<VecDeque<String>>>,
    paused: Arc<AtomicBool>,
}

impl Ring {
    /// Notes a copied text. False when it was not kept (paused, empty, a secret, the same as
    /// the newest entry).
    pub fn note(&self, text: &str) -> bool {
        if self.paused.load(Ordering::Acquire) || text.trim().is_empty() || looks_secret(text) {
            return false;
        }
        let kept: String = text.chars().take(MAX_CHARS).collect();
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.front() == Some(&kept) {
            return false;
        }
        // An older copy of the same text moves to the front instead of appearing twice.
        entries.retain(|e| *e != kept);
        entries.push_front(kept);
        entries.truncate(KEEP);
        true
    }

    pub fn entries(&self) -> Vec<String> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
    }

    pub fn clear(&self) -> usize {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let n = entries.len();
        entries.clear();
        n
    }

    pub fn set_paused(&self, on: bool) {
        self.paused.store(on, Ordering::Release);
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// The `clipboard_history` tool. `restore` returns the text to put back as its second value
    /// (the hub writes it to the clipboard).
    pub fn tool(&self, args: &Value) -> Result<(String, Option<String>), String> {
        let one_line = |text: &str| {
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            crate::crash_guard::clip(&flat, 90).to_string()
        };
        match str_arg(args, &["action"]).unwrap_or("list").to_lowercase().as_str() {
            "clear" | "empty" | "forget" => {
                let n = self.clear();
                Ok((spoken(format!("Clipboard history cleared ({n} entries)."), "Clipboard history cleared, Sir."), None))
            }
            "pause" => {
                self.set_paused(true);
                Ok((spoken("Clipboard history paused.", "Paused, Sir. Nothing you copy is kept until you say so."), None))
            }
            "resume" => {
                self.set_paused(false);
                Ok((spoken("Clipboard history resumed.", "Keeping your copies again, Sir."), None))
            }
            "restore" | "get" | "copy" => {
                let entries = self.entries();
                // 1 is the newest; "the one before" is 2.
                let index = crate::features::num_arg(args, &["index", "number", "entry"]).map(|n| n as usize).unwrap_or(2);
                let text = entries.get(index.saturating_sub(1)).ok_or_else(|| format!("There is no entry {index} in the clipboard history, Sir. It holds {}.", entries.len()))?.clone();
                Ok((spoken(format!("Back on the clipboard: {}", one_line(&text)), "It's back on your clipboard, Sir."), Some(text)))
            }
            _ => {
                let entries = self.entries();
                if entries.is_empty() {
                    let why = if self.paused() { "The clipboard history is paused, Sir." } else { "Nothing copied since I started, Sir." };
                    return Ok((spoken("Clipboard history: empty.", why), None));
                }
                let lines: Vec<String> = entries.iter().enumerate().map(|(i, e)| format!("{:>2}  {}", i + 1, one_line(e))).collect();
                Ok((spoken(format!("Clipboard history ({} entries, in memory only{})\n{}", entries.len(), if self.paused() { ", paused" } else { "" }, lines.join("\n")), format!("Your last {} copies are on the board, Sir.", entries.len())), None))
            }
        }
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "clipboard_history",
        "description": "The last 25 texts Sir copied (kept in memory only; secrets are never kept). action list, restore (index: 1 = newest, 2 = the one before … puts it back on the clipboard), clear, pause, resume.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["list", "restore", "clear", "pause", "resume"]},
            "index": {"type": "integer", "description": "restore: which entry (1 = newest)."}
        }, "required": []}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_recognised_and_ordinary_text_is_not() {
        for secret in [
            "sk-proj-AbCdEf1234567890",
            "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
            "AKIAIOSFODNN7EXAMPLE",
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.e30.abc",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk=\n-----END OPENSSH PRIVATE KEY-----",
            "4111 1111 1111 1111",
            "DE89 3704 0044 0532 0130 00",
            "Tr0ub4dor&3xQ",
            "x9!Kq2#Lm7$Wp4zR8vN",
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        ] {
            assert!(looks_secret(secret), "{secret}");
        }
        for plain in [
            "Ohm's law: U = R * I",
            "https://example.com/path?query=1&x=AbC123",
            "C:\\Users\\max\\Documents\\notes.txt",
            "E_kin = 1/2 * m * v^2",
            "Thunderstruck",
            "1234 5678 9012 3456",
            "2026-11-06 10:15",
            "function plotter",
            "Donaudampfschifffahrtsgesellschaft",
            "",
        ] {
            assert!(!looks_secret(plain), "{plain}");
        }
    }

    #[test]
    fn the_ring_keeps_the_newest_first_without_secrets_or_repeats() {
        let ring = Ring::default();
        assert!(ring.note("first"));
        assert!(ring.note("second"));
        assert!(!ring.note("second"), "the same copy again is not a new entry");
        assert!(!ring.note("ghp_16C7e42F292c6912E7710c838347Ae178B4a"), "never a token");
        assert!(!ring.note("   "));
        assert!(ring.note("first"), "an older text copied again moves to the front");
        assert_eq!(ring.entries(), ["first", "second"]);
        for n in 0..40 {
            ring.note(&format!("entry {n}"));
        }
        assert_eq!(ring.entries().len(), KEEP);
        assert_eq!(ring.entries()[0], "entry 39");
        assert!(ring.note(&"x".repeat(10_000)));
        assert_eq!(ring.entries()[0].chars().count(), MAX_CHARS);
        // Paused: nothing is kept.
        ring.set_paused(true);
        assert!(!ring.note("while paused"));
        ring.set_paused(false);
        assert_eq!(ring.clear(), KEEP);
        assert!(ring.entries().is_empty());
    }

    #[test]
    fn the_tool_lists_restores_pauses_and_clears() {
        let ring = Ring::default();
        assert!(ring.tool(&json!({})).unwrap().0.contains("Nothing copied since I started"));
        ring.note("alpha");
        ring.note("beta\nwith a second line");
        ring.note("gamma");
        let (listed, _) = ring.tool(&json!({"action": "list"})).unwrap();
        assert!(listed.contains(" 1  gamma\n 2  beta with a second line\n 3  alpha") && listed.contains("in memory only"), "{listed}");
        // "The one before": entry 2 by default.
        let (said, text) = ring.tool(&json!({"action": "restore"})).unwrap();
        assert_eq!(text.as_deref(), Some("beta\nwith a second line"));
        assert!(said.contains("Back on the clipboard: beta with a second line"));
        assert_eq!(ring.tool(&json!({"action": "restore", "index": 3})).unwrap().1.as_deref(), Some("alpha"));
        assert!(ring.tool(&json!({"action": "restore", "index": 9})).is_err());
        ring.tool(&json!({"action": "pause"})).unwrap();
        assert!(ring.paused() && !ring.note("delta"));
        ring.tool(&json!({"action": "resume"})).unwrap();
        assert!(ring.tool(&json!({"action": "clear"})).unwrap().0.contains("cleared (3 entries)"));
    }
}
