//! "Translate good morning into Japanese", "how do you say thank you in Spanish", "was heißt
//! Schmetterling auf Französisch" (Sir, 2026-11-22: "JAI translates anything into anything,
//! extremely fast"): one short request to the language model with nothing else in it (no
//! tools, no history, no prompt of his), the translation shown, said and put on the clipboard.

use regex::Regex;
use std::sync::LazyLock;

/// The language a word names, in English: "japanese", "Japanisch", "ins Englische" → its name.
pub fn language(word: &str) -> Option<&'static str> {
    const NAMES: &[(&str, &[&str])] = &[
        ("English", &["english", "englisch"]),
        ("German", &["german", "deutsch"]),
        ("French", &["french", "französisch", "franzoesisch"]),
        ("Spanish", &["spanish", "spanisch"]),
        ("Italian", &["italian", "italienisch"]),
        ("Portuguese", &["portuguese", "portugiesisch"]),
        ("Dutch", &["dutch", "niederländisch", "holländisch"]),
        ("Polish", &["polish", "polnisch"]),
        ("Russian", &["russian", "russisch"]),
        ("Ukrainian", &["ukrainian", "ukrainisch"]),
        ("Turkish", &["turkish", "türkisch", "tuerkisch"]),
        ("Greek", &["greek", "griechisch"]),
        ("Swedish", &["swedish", "schwedisch"]),
        ("Norwegian", &["norwegian", "norwegisch"]),
        ("Danish", &["danish", "dänisch"]),
        ("Finnish", &["finnish", "finnisch"]),
        ("Czech", &["czech", "tschechisch"]),
        ("Hungarian", &["hungarian", "ungarisch"]),
        ("Romanian", &["romanian", "rumänisch"]),
        ("Croatian", &["croatian", "kroatisch"]),
        ("Arabic", &["arabic", "arabisch"]),
        ("Hebrew", &["hebrew", "hebräisch"]),
        ("Persian", &["persian", "farsi", "persisch"]),
        ("Hindi", &["hindi"]),
        ("Chinese", &["chinese", "mandarin", "chinesisch"]),
        ("Japanese", &["japanese", "japanisch"]),
        ("Korean", &["korean", "koreanisch"]),
        ("Vietnamese", &["vietnamese", "vietnamesisch"]),
        ("Thai", &["thai", "thailändisch"]),
        ("Indonesian", &["indonesian", "indonesisch"]),
        ("Latin", &["latin", "latein", "lateinisch"]),
        ("Swahili", &["swahili", "suaheli"]),
        ("Irish", &["irish", "irisch"]),
        ("Esperanto", &["esperanto"]),
    ];
    let w = word.trim().to_lowercase();
    // ("ins Englische", "ins Spanische")
    let w = w.strip_suffix('e').filter(|s| s.ends_with("sch")).unwrap_or(&w);
    NAMES.iter().find(|(_, words)| words.contains(&w)).map(|(name, _)| *name)
}

/// (what to translate, into which language) when Sir asks for a translation of words he says;
/// "translate this" (his selection) and "the clipboard" are the selection's.
pub fn parse(text: &str) -> Option<(String, &'static str)> {
    static ASKED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r#"(?i)^(?:jai,?\s+|jay,?\s+)?(?:please\s+|can\s+you\s+|could\s+you\s+|bitte\s+)?(?:translate|übersetze?|uebersetze?)\s+["'„“]?(?P<a>.+?)["'“”]?\s+(?:into|in|to|auf|ins|nach)\s+(?P<to>[\p{L}]+)(?:\s+(?:please|for\s+me|bitte))?[.!?]?$"#,
            r#"|(?i)^(?:jai,?\s+|jay,?\s+)?(?:how\s+do\s+(?:you|i|we)\s+say|how\s+would\s+you\s+say|how\s+to\s+say|what(?:'s|\s+is)|what\s+does)\s+["'„“]?(?P<b>.+?)["'“”]?\s+(?:mean\s+)?in\s+(?P<to2>[\p{L}]+)[.!?]?$"#,
            r#"|(?i)^(?:jai,?\s+|jay,?\s+)?(?:was\s+heißt|was\s+heisst|was\s+bedeutet|wie\s+sagt\s+man)\s+["'„“]?(?P<c>.+?)["'“”]?\s+(?:auf|in)\s+(?P<to3>[\p{L}]+)[.!?]?$"#,
        ))
        .expect("valid translation regex")
    });
    const SELECTION: &[&str] = &[
        "this", "that", "it", "the selection", "the selected text", "what i selected", "what i copied", "my clipboard", "the clipboard", "das", "das hier", "die auswahl",
        "die zwischenablage", "den markierten text", "das markierte",
    ];
    let c = ASKED.captures(text.trim())?;
    let what = c.name("a").or(c.name("b")).or(c.name("c"))?.as_str().trim();
    let into = language(c.name("to").or(c.name("to2")).or(c.name("to3"))?.as_str())?;
    (!what.is_empty() && !SELECTION.contains(&what.to_lowercase().as_str())).then(|| (what.to_string(), into))
}

/// What the model is told: the translation alone, and how it sounds when its script is not ours.
pub fn instruction(into: &str) -> String {
    format!("Translate the text into {into}. Reply with the translation only: no quotes, no notes. If {into} is not written in Latin letters, add how it is pronounced in Latin letters in round brackets after it.")
}

/// (shown, said) for a translation the model gave: "Japanese: おはよう (ohayō)" shown, its
/// sound said.
pub fn reply(into: &str, translation: &str) -> (String, String) {
    let t = translation.trim().trim_matches(['"', '“', '”', '„']).trim();
    let sound = t.rsplit_once('(').filter(|_| t.ends_with(')') && !t.is_ascii()).map(|(_, s)| s.trim_end_matches(')').trim()).unwrap_or(t);
    (format!("{into}: {t}"), format!("In {into}: {sound}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sir's ways of asking for a translation, in English and German, into any language; his
    /// selection, a question about the weather and a sum stay theirs.
    #[test]
    fn any_words_into_any_language() {
        for (text, what, into) in [
            ("Translate good morning into Japanese", "good morning", "Japanese"),
            ("translate 'where is the station' to French please", "where is the station", "French"),
            ("How do you say thank you in Spanish?", "thank you", "Spanish"),
            ("What's butterfly in Italian", "butterfly", "Italian"),
            ("What does merci beaucoup mean in English?", "merci beaucoup", "English"),
            ("Was heißt Schmetterling auf Französisch?", "Schmetterling", "French"),
            ("Übersetze Guten Morgen ins Englische", "Guten Morgen", "English"),
            ("Jay, wie sagt man danke auf Japanisch", "danke", "Japanese"),
        ] {
            assert_eq!(parse(text), Some((what.to_string(), into)), "{text}");
        }
        for text in ["Translate this into Spanish", "translate my clipboard to German", "What's the weather in Berlin?", "What is 5 euros in dollars?", "translate the page", "Übersetze das hier auf Englisch"] {
            assert_eq!(parse(text), None, "{text}");
        }
        assert_eq!(reply("Japanese", "おはようございます (ohayō gozaimasu)"), ("Japanese: おはようございます (ohayō gozaimasu)".into(), "In Japanese: ohayō gozaimasu".into()));
        assert_eq!(reply("French", "\"Merci (beaucoup)\""), ("French: Merci (beaucoup)".into(), "In French: Merci (beaucoup)".into()));
    }
}
