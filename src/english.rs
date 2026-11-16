//! His timetable and calendar read out in English: the German words of a title in English
//! ("Klassenarbeit Deutsch" → "German class test", "Oma anrufen" → "call Grandma"), word by
//! word from the lists below, offline. Names and words not on them stay as they are, and a
//! title with no German-only word in it is left alone. (Sir, 2026-11-16: "many things are in
//! German on my schedule and in my calendar … translate it to English and read that out".)

/// German → English, lower case. Nouns and small words; the verbs (said last in German, first
/// in English) are in `VERBS`.
const WORDS: &[(&str, &str)] = &[
    // school
    ("klassenarbeit", "class test"),
    ("klausur", "exam"),
    ("prüfung", "exam"),
    ("test", "test"),
    ("kurzarbeit", "short test"),
    ("hausaufgabe", "homework"),
    ("hausaufgaben", "homework"),
    ("aufgabe", "task"),
    ("aufgaben", "tasks"),
    ("arbeitsblatt", "worksheet"),
    ("referat", "presentation"),
    ("präsentation", "presentation"),
    ("vortrag", "talk"),
    ("abgabe", "hand-in"),
    ("abgabetermin", "deadline"),
    ("frist", "deadline"),
    ("abitur", "final exams"),
    ("nachschreiben", "resit"),
    ("nachschreibtermin", "resit"),
    ("nachhilfe", "tutoring"),
    ("unterricht", "lessons"),
    ("stunde", "lesson"),
    ("doppelstunde", "double lesson"),
    ("vertretung", "substitute lesson"),
    ("entfall", "cancelled"),
    ("ausfall", "cancelled"),
    ("raumänderung", "room change"),
    ("raum", "room"),
    ("klasse", "class"),
    ("kurs", "course"),
    ("lehrer", "teacher"),
    ("lehrerin", "teacher"),
    ("schule", "school"),
    ("ferien", "holidays"),
    ("feiertag", "public holiday"),
    ("pause", "break"),
    ("mittagspause", "lunch break"),
    ("elternabend", "parents' evening"),
    ("elternsprechtag", "parents' day"),
    ("zeugnis", "report card"),
    ("zeugnisse", "report cards"),
    ("ausflug", "trip"),
    ("klassenfahrt", "class trip"),
    ("exkursion", "excursion"),
    ("projekttag", "project day"),
    ("projektwoche", "project week"),
    ("wandertag", "hiking day"),
    ("sportfest", "sports day"),
    ("schulfest", "school fair"),
    ("praktikum", "work placement"),
    ("bewerbung", "application"),
    ("vorstellungsgespräch", "job interview"),
    // appointments
    ("termin", "appointment"),
    ("arzt", "doctor"),
    ("ärztin", "doctor"),
    ("arzttermin", "doctor's appointment"),
    ("zahnarzt", "dentist"),
    ("zahnärztin", "dentist"),
    ("kieferorthopäde", "orthodontist"),
    ("augenarzt", "eye doctor"),
    ("hausarzt", "family doctor"),
    ("friseur", "hairdresser"),
    ("frisör", "hairdresser"),
    ("krankenhaus", "hospital"),
    ("apotheke", "pharmacy"),
    ("geburtstag", "birthday"),
    ("hochzeit", "wedding"),
    ("feier", "party"),
    ("besprechung", "meeting"),
    ("sitzung", "meeting"),
    ("konzert", "concert"),
    ("kino", "cinema"),
    ("training", "practice"),
    ("spiel", "match"),
    ("turnier", "tournament"),
    ("probe", "rehearsal"),
    ("fahrstunde", "driving lesson"),
    ("führerschein", "driving licence"),
    ("urlaub", "holiday"),
    ("reise", "trip"),
    ("flug", "flight"),
    ("zug", "train"),
    ("abendessen", "dinner"),
    ("mittagessen", "lunch"),
    ("frühstück", "breakfast"),
    ("einkauf", "shopping"),
    ("müll", "rubbish"),
    ("abgesagt", "cancelled"),
    ("verschoben", "postponed"),
    ("wichtig", "important"),
    // people
    ("oma", "Grandma"),
    ("opa", "Grandpa"),
    ("mama", "Mum"),
    ("papa", "Dad"),
    ("mutter", "mother"),
    ("vater", "father"),
    ("bruder", "brother"),
    ("schwester", "sister"),
    ("tante", "aunt"),
    ("onkel", "uncle"),
    ("freund", "friend"),
    ("freundin", "friend"),
    ("freunde", "friends"),
    ("familie", "family"),
    ("nachbar", "neighbour"),
    // days
    ("montag", "Monday"),
    ("dienstag", "Tuesday"),
    ("mittwoch", "Wednesday"),
    ("donnerstag", "Thursday"),
    ("freitag", "Friday"),
    ("samstag", "Saturday"),
    ("sonntag", "Sunday"),
    ("heute", "today"),
    ("morgen", "tomorrow"),
    ("woche", "week"),
    // small words
    ("mit", "with"),
    ("bei", "at"),
    ("beim", "at the"),
    ("im", "in the"),
    ("am", "on"),
    ("um", "at"),
    ("uhr", "o'clock"),
    ("und", "and"),
    ("oder", "or"),
    ("für", "for"),
    ("zum", "to the"),
    ("zur", "to the"),
    ("nach", "after"),
    ("vor", "before"),
    ("von", "from"),
    ("bis", "until"),
    ("der", "the"),
    ("die", "the"),
    ("das", "the"),
    ("den", "the"),
    ("dem", "the"),
    ("des", "the"),
    ("ein", "a"),
    ("eine", "a"),
    ("einen", "a"),
    ("mein", "my"),
    ("meine", "my"),
    ("meinen", "my"),
    ("kein", "no"),
    ("neu", "new"),
    ("neue", "new"),
];

/// German infinitives, said last ("Mathe lernen"), in English said first ("study Maths").
const VERBS: &[(&str, &str)] = &[
    ("lernen", "study"),
    ("üben", "practise"),
    ("wiederholen", "revise"),
    ("vorbereiten", "prepare"),
    ("lesen", "read"),
    ("schreiben", "write"),
    ("abgeben", "hand in"),
    ("mitbringen", "bring"),
    ("anrufen", "call"),
    ("treffen", "meet"),
    ("abholen", "pick up"),
    ("einkaufen", "go shopping"),
    ("aufräumen", "tidy up"),
    ("kochen", "cook"),
    ("putzen", "clean"),
    ("fahren", "drive"),
    ("besuchen", "visit"),
    ("bezahlen", "pay"),
    ("zahlen", "pay"),
    ("kaufen", "buy"),
    ("buchen", "book"),
    ("packen", "pack"),
    ("reparieren", "repair"),
    ("ausfüllen", "fill in"),
    ("unterschreiben", "sign"),
];

/// Words that are English as well (or names): they never make a title German.
const ALSO_ENGLISH: &[&str] = &["test", "training", "in", "am", "die", "kino", "party", "probe", "pause", "spiel", "an", "so", "also"];

/// A school subject by one of its names ("Mathe", "Deutsch", "Bio"; no two-letter codes).
fn subject(word: &str) -> Option<String> {
    let w = word.to_lowercase();
    (w.chars().count() > 2 && crate::pim::SUBJECTS.iter().any(|group| group.contains(&w.as_str()))).then(|| crate::pim::english_subject(&w)).flatten()
}

fn word(w: &str) -> Option<String> {
    let lower = w.to_lowercase();
    WORDS.iter().find(|(de, _)| *de == lower).map(|(_, en)| en.to_string()).or_else(|| subject(w))
}

fn verb(w: &str) -> Option<&'static str> {
    let lower = w.to_lowercase();
    VERBS.iter().find(|(de, _)| *de == lower).map(|(_, en)| *en)
}

/// A compound of two known words ("Mathearbeit", "Deutschklausur", "Physikreferat"): a subject
/// with "-arbeit" is a test; the joining "s" or "n" is allowed for.
fn compound(w: &str) -> Option<String> {
    let lower = w.to_lowercase();
    let chars: Vec<(usize, char)> = lower.char_indices().collect();
    for &(i, _) in chars.iter().skip(3) {
        let (head, tail) = lower.split_at(i);
        if tail.chars().count() < 4 {
            break;
        }
        let tail_en = if tail == "arbeit" { Some("test".to_string()) } else { word(tail) };
        let Some(tail_en) = tail_en else { continue };
        let head_en = word(head).or_else(|| head.strip_suffix(['s', 'n']).and_then(word));
        if let Some(head_en) = head_en {
            return Some(format!("{head_en} {tail_en}"));
        }
    }
    None
}

fn is_german_only(w: &str) -> bool {
    let lower = w.to_lowercase();
    !ALSO_ENGLISH.contains(&lower.as_str())
        && (lower.chars().any(|c| matches!(c, 'ä' | 'ö' | 'ü' | 'ß'))
            || WORDS.iter().any(|(de, _)| *de == lower)
            || VERBS.iter().any(|(de, _)| *de == lower)
            || compound(w).is_some()
            || (subject(w).is_some() && crate::pim::english_subject(w).is_some_and(|en| !en.eq_ignore_ascii_case(w))))
}

/// A title of his timetable or calendar in English, for reading out and showing. Left as it is
/// when nothing in it is German.
pub fn in_english(text: &str) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let bare = |t: &str| t.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').to_string();
    if !tokens.iter().any(|t| is_german_only(&bare(t))) {
        return text.to_string();
    }
    // Each token in English, its punctuation kept; a verb at the end goes to the front.
    let mut out: Vec<String> = Vec::new();
    let mut lead: Option<&str> = None;
    for (i, t) in tokens.iter().enumerate() {
        let w = bare(t);
        let (pre, post) = t.split_once(w.as_str()).unwrap_or(("", ""));
        if i == tokens.len() - 1 && i > 0 && post.is_empty() {
            if let Some(v) = verb(&w) {
                lead = Some(v);
                continue;
            }
        }
        let en = word(&w).or_else(|| compound(&w)).or_else(|| verb(&w).map(str::to_string)).unwrap_or_else(|| w.clone());
        out.push(format!("{pre}{en}{post}"));
    }
    // "Klassenarbeit Deutsch" → "German class test": a subject named after its test.
    for i in 1..out.len() {
        let test = ["class test", "exam", "test", "short test", "presentation", "homework"].contains(&out[i - 1].as_str());
        if test && subject(&bare(tokens[i])).is_some() && !tokens[i].ends_with([',', ':', ';']) {
            out.swap(i - 1, i);
        }
    }
    let mut s = lead.map(|v| format!("{v} ")).unwrap_or_default() + &out.join(" ");
    // A title starts with a capital, as it did.
    if tokens.first().and_then(|t| t.chars().next()).is_some_and(char::is_uppercase) {
        s = crate::pim::capitalise(&s);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made-up titles of the kinds his timetable and calendar hold.
    #[test]
    fn german_titles_are_read_in_english() {
        for (de, en) in [
            ("Mathe lernen", "Study Mathematics"),
            ("Klassenarbeit Deutsch", "German class test"),
            ("Mathearbeit", "Mathematics test"),
            ("Zahnarzt", "Dentist"),
            ("Geburtstag Oma", "Birthday Grandma"),
            ("Oma anrufen", "Call Grandma"),
            ("Training mit Max", "Practice with Max"),
            ("Elternabend um 19 Uhr", "Parents' evening at 19 o'clock"),
            ("Referat Geschichte abgeben", "Hand in History presentation"),
            ("Physik: Arbeitsblatt 4", "Physics: worksheet 4"),
        ] {
            assert_eq!(in_english(de), en, "{de}");
        }
    }

    /// English titles and names stay as they are.
    #[test]
    fn english_titles_and_names_are_left_alone() {
        for same in ["Team meeting", "Die Hard night", "Max Mustermann", "Test drive", "Party at Max's", "Lesson 4", ""] {
            assert_eq!(in_english(same), same);
        }
    }
}
