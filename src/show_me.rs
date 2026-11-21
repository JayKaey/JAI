//! Unclear "show me" orders ("show me the moon", "zeig mir das Sonnensystem", "draw the solar
//! system", "make me a drawing of a cat"): nothing of his is named, so he picks the likeliest way
//! to show it, does it at once and says in a few words what he chose (the owner, 2026-11-21:
//! "pick and do it"). A curve is plotted, what moves is a simulation, a thing worth turning by
//! hand a 3D model, anything else a picture; he cannot draw, so a drawing asked for is a picture
//! as well. Asked last, after every other rule of the fast path (`Brain::run_turn`; so does
//! `phrasings::route`), and never for what the model's turn looks up first.

use crate::intents::Intent;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

/// The argument carrying the line that names his pick: said once the tool has worked, and taken
/// out of the arguments before it runs (`take_line`).
pub const PICKED: &str = "picked";

/// Things worth turning by hand in the 3D studio: as said (English or German), the studio's
/// query, and how he names it.
const MODELS: &[(&str, &str, &str)] = &[
    ("solar system", "solar system", "The solar system"), ("sonnensystem", "solar system", "The solar system"),
    ("planets", "solar system", "The solar system"), ("planeten", "solar system", "The solar system"),
    ("moon", "moon", "The Moon"), ("mond", "moon", "The Moon"), ("sun", "sun", "The Sun"), ("sonne", "sun", "The Sun"),
    ("mars", "mars", "Mars"), ("jupiter", "jupiter", "Jupiter"), ("saturn", "saturn", "Saturn"), ("venus", "venus", "Venus"),
    ("mercury", "mercury", "Mercury"), ("merkur", "mercury", "Mercury"), ("neptune", "neptune", "Neptune"),
    ("neptun", "neptune", "Neptune"), ("uranus", "uranus", "Uranus"),
    ("heart", "human heart", "A human heart"), ("herz", "human heart", "A human heart"), ("brain", "human brain", "A human brain"),
    ("gehirn", "human brain", "A human brain"), ("skull", "skull", "A skull"), ("schädel", "skull", "A skull"),
    ("skeleton", "skeleton", "A skeleton"), ("skelett", "skeleton", "A skeleton"), ("eye", "human eye", "A human eye"),
    ("auge", "human eye", "A human eye"), ("atom", "atom", "An atom"), ("molecule", "molecule", "A molecule"),
    ("molekül", "molecule", "A molecule"), ("dna", "dna double helix", "DNA"), ("dns", "dna double helix", "DNA"),
    ("cell", "animal cell", "A cell"), ("zelle", "animal cell", "A cell"), ("virus", "virus", "A virus"),
    ("engine", "engine", "An engine"), ("motor", "engine", "An engine"), ("jet engine", "jet engine", "A jet engine"),
    ("triebwerk", "jet engine", "A jet engine"), ("turbine", "turbine", "A turbine"), ("gearbox", "gearbox", "A gearbox"),
    ("getriebe", "gearbox", "A gearbox"), ("car", "car", "A car"), ("auto", "car", "A car"), ("plane", "airplane", "An aeroplane"),
    ("airplane", "airplane", "An aeroplane"), ("aeroplane", "airplane", "An aeroplane"), ("flugzeug", "airplane", "An aeroplane"),
    ("helicopter", "helicopter", "A helicopter"), ("hubschrauber", "helicopter", "A helicopter"), ("rocket", "rocket", "A rocket"),
    ("rakete", "rocket", "A rocket"), ("spaceship", "spaceship", "A spaceship"), ("raumschiff", "spaceship", "A spaceship"),
    ("space shuttle", "space shuttle", "The Space Shuttle"), ("satellite", "satellite", "A satellite"),
    ("satellit", "satellite", "A satellite"), ("space station", "international space station", "The International Space Station"),
    ("raumstation", "international space station", "The International Space Station"), ("submarine", "submarine", "A submarine"),
    ("u-boot", "submarine", "A submarine"), ("robot", "robot", "A robot"), ("roboter", "robot", "A robot"), ("drone", "drone", "A drone"),
    ("drohne", "drone", "A drone"), ("motorcycle", "motorcycle", "A motorcycle"), ("motorrad", "motorcycle", "A motorcycle"),
    ("iron man suit", "iron man suit", "An Iron Man suit"), ("iron man", "iron man suit", "An Iron Man suit"),
    ("arc reactor", "arc reactor", "An arc reactor"), ("castle", "castle", "A castle"), ("burg", "castle", "A castle"),
    ("pyramid", "pyramid", "A pyramid"), ("pyramide", "pyramid", "A pyramid"), ("eiffel tower", "eiffel tower", "The Eiffel Tower"),
    ("eiffelturm", "eiffel tower", "The Eiffel Tower"),
];

/// Words of what moves: a subject holding one is the Simulation Lab's, where it has such a kind.
const MOVES: &[&str] = &[
    "wave", "waves", "welle", "wellen", "ripple", "ripples", "doppler", "interference", "interferenz", "slit", "doppelspalt",
    "pendulum", "pendel", "orbit", "orbits", "umlaufbahn", "gravity", "gravitation", "schwerkraft", "projectile", "trajectory",
    "flugbahn", "collision", "planets", "planeten", "solar", "sonnensystem",
];

/// Curves asked for by name: the word, the plotter's formula, how he names it.
const CURVES: &[(&str, &str, &str)] = &[
    ("sine", "sin(x)", "A sine curve"), ("sinus", "sin(x)", "A sine curve"), ("sinuskurve", "sin(x)", "A sine curve"),
    ("cosine", "cos(x)", "A cosine curve"), ("kosinus", "cos(x)", "A cosine curve"), ("cosinus", "cos(x)", "A cosine curve"),
    ("parabola", "x^2", "A parabola"), ("parabel", "x^2", "A parabola"), ("tangent", "tan(x)", "A tangent curve"),
    ("tangens", "tan(x)", "A tangent curve"),
];

/// Words that make the subject no thing to look at but his, Sir's or the PC's (a file, the
/// weather, a view of his) or no thing at all ("that", "how to …"): the language model's.
const NOT_A_THING: &[&str] = &[
    "my", "your", "his", "her", "our", "their", "its", "me", "us", "him", "them", "it", "this", "that", "these", "those", "mein", "meine",
    "meinen", "meinem", "meiner", "dein", "deine", "deinen", "deinem", "deiner", "sein", "seine", "ihr", "ihre", "unser", "unsere",
    "es", "dies", "diese", "dieses", "something", "anything", "everything", "nothing", "etwas", "irgendwas", "alles", "nichts",
    "what", "how", "where", "who", "when", "which", "why", "whether", "was", "wo", "wer", "wann", "welche", "welcher", "welches",
    "warum", "wie", "ob", "to", "man", "file", "files", "folder", "datei", "dateien", "ordner", "email", "emails", "mail", "mails",
    "inbox", "message", "messages", "nachricht", "nachrichten", "chat", "calendar", "kalender", "termin", "termine", "weather",
    "wetter", "forecast", "news", "schedule", "stundenplan", "timetable", "homework", "hausaufgaben", "task", "tasks", "aufgaben",
    "todo", "notes", "note", "notiz", "notizen", "reminder", "reminders", "timer", "alarm", "window", "windows", "fenster", "app",
    "apps", "program", "programm", "screen", "bildschirm", "desktop", "monitor", "settings", "einstellungen", "options", "menu",
    "code", "log", "logs", "history", "verlauf", "stats", "statistics", "status", "list", "liste", "playlist", "song", "songs",
    "track", "music", "musik", "queue", "warteschlange", "video", "videos", "clip", "movie", "film", "results", "result", "ergebnis",
    "ergebnisse", "page", "website", "site", "seite", "link", "tab", "tabs", "time", "zeit", "uhr", "date", "datum", "way", "weg",
    "difference", "unterschied", "example", "beispiel", "steps", "answer", "antwort", "solution", "lösung", "keys", "hotkeys",
    "shortcuts", "tastenkürzel", "commands", "befehle", "features", "help", "hilfe", "around", "more", "mehr", "next", "previous",
    "last", "latest", "recent", "new", "neu", "neues", "again", "wieder", "nochmal", "tutorial", "guide", "anleitung", "password",
    "account", "accounts", "konto", "price", "prices", "stock", "stocks", "aktie", "aktien", "map", "karte", "route", "directions",
    "traffic", "verkehr", "location", "standort", "camera", "kamera", "webcam", "picture", "pictures", "photo", "photos", "image",
    "images", "bild", "bilder", "foto", "fotos", "screenshot", "model", "modell", "simulation", "studio", "earth", "erde", "globe",
    "board", "core", "orb", "hud", "interface", "theme", "mixer", "volume", "lautstärke", "sound", "mic", "microphone", "mikrofon",
    "battery", "akku", "cpu", "gpu", "ram", "memory", "speicher", "disk", "network", "wifi", "wlan", "internet", "temperature",
    "temperatur", "progress", "fortschritt", "console", "konsole", "terminal", "dashboard", "panel", "view", "lab", "clock",
    "widget", "widgets", "snippets", "layout", "layouts", "mode", "modus", "protocol", "protokoll", "radar", "replay", "overlay",
    "recording", "aufnahme", "graph", "chart", "diagram", "diagramm", "report", "bericht", "summary", "zusammenfassung",
    "overview", "übersicht", "details", "info", "information", "infos", "data", "daten", "numbers", "zahlen", "usage", "space",
    "drive", "drives", "laufwerk", "processes", "prozesse", "services", "devices", "geräte", "friends", "freunde", "contacts",
    "kontakte", "people", "users", "admin", "changes", "today", "heute", "tomorrow", "morgen", "yesterday", "gestern", "week",
    "woche", "month", "monat", "year", "jahr", "restaurant", "restaurants", "nearby", "near", "here", "hier", "nähe",
];

const ARTICLES: &[&str] = &["the", "a", "an", "some", "der", "die", "das", "den", "dem", "ein", "eine", "einen", "einem", "einer"];

/// "Show me …", "visualise …", "draw me …", "make me a drawing of …", "zeig mir …", "zeichne …",
/// "(kannst du) mir … zeigen": the subject, and (groups `draw…`) whether a drawing is asked for.
/// (On `intents::normalize_phrase`'s words: "can you", "kannst du" and "mal" are gone already.)
static ORDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:(?:show|display|visuali[sz]e|render)|(?P<draw>draw|sketch|paint|illustrate))(?:\s+(?:me|us))?\s+(?P<s>.+)$|^(?P<draw2>make|draw|paint|create|do|sketch)(?:\s+(?:me|us))?\s+(?:a|an)\s+(?:drawing|painting|sketch|illustration)\s+of\s+(?P<s2>.+)$|^(?:(?:zeig|zeige|visualisiere)|(?P<draw3>zeichne|male))(?:\s+(?:mir|uns))?\s+(?P<s3>.+)$|^(?:mir\s+|uns\s+)?(?P<s4>.+?)\s+(?:zeigen|visualisieren|darstellen|(?P<draw4>zeichnen|malen))$|^(?:mach|mache|erstelle)(?:\s+mir)?\s+(?:eine|ein)\s+(?P<draw5>zeichnung|gemälde|skizze)\s+(?:von|vom)\s+(?P<s5>.+)$",
    )
    .expect("show-me expression")
});

/// "How a heart works", "how the planets move": the thing, and whether its motion is asked for.
static HOW_IT_GOES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:how|wie)\s+(?P<s>.+?)\s+(?:(?:works?|runs?|functions?|beats?|funktioniert|funktionieren|schlägt|läuft)|(?P<moves>moves?|orbits?|spins?|fly|flies|swings?|sich bewegt|sich bewegen|kreisen|kreist|fliegt|schwingt))$")
        .expect("how-it-goes expression")
});

/// What he picks for words that name nothing of his: the tool, its arguments and (under
/// `PICKED`) the line naming his choice. None for anything else.
pub fn pick(text: &str) -> Option<Intent> {
    // A formula is the plotter's own rule (or the model's): its signs are gone once normalised.
    if text.contains(['=', '^', '*', '/', '+', '(']) {
        return None;
    }
    let said = crate::intents::normalize_phrase(text);
    let c = ORDER.captures(&said)?;
    let draw = ["draw", "draw2", "draw3", "draw4", "draw5"].iter().any(|g| c.name(g).is_some());
    let german = ["s3", "s4", "s5"].iter().any(|g| c.name(g).is_some());
    let mut subject = ["s", "s2", "s3", "s4", "s5"].iter().find_map(|g| c.name(g))?.as_str().trim().to_string();
    for tail in [" sir", " jay", " quickly", " dar"] {
        if let Some(rest) = subject.strip_suffix(tail) {
            subject = rest.trim().to_string();
        }
    }
    let mut moving = false;
    if let Some(how) = HOW_IT_GOES.captures(&subject) {
        moving = how.name("moves").is_some();
        subject = how["s"].to_string();
    }
    let words: Vec<&str> = subject.split_whitespace().collect();
    let bare = strip_article(&subject);
    if bare.is_empty()
        || ARTICLES.contains(&bare.as_str())
        || words.len() > 5
        || words.iter().any(|w| NOT_A_THING.contains(w))
        || crate::apps::find_spec(&bare).is_some()
        || crate::appindex::is_known(&bare)
        // What the model's turn looks up first: places nearby, live facts, the screen.
        || crate::location_engine::upfront(text).is_some()
        || crate::intents::live_info_query(text).is_some()
        || crate::intents::upfront_tool(text).is_some()
    {
        return None;
    }
    // Sir's own words in his own case ("Saturn V", "a Vulkan"), where they stand in the sentence.
    let own = |words: &str| as_said(text, words);
    // A table's entry: the longest word of it the thing is, or ends on ("a V8 engine").
    let entry = |table: &'static [(&'static str, &'static str, &'static str)]| {
        table.iter().filter(|(k, ..)| bare == *k || bare.ends_with(&format!(" {k}")) || words.contains(k)).max_by_key(|(k, ..)| k.len())
    };
    // A curve by name: the plotter.
    if let Some((_, formula, name)) = entry(CURVES) {
        let args = crate::simulation_lab::simulation_request(&format!("plot y = {formula}"), &[])?;
        return Some(chosen("launch_interactive_simulation", args, format!("{name}, plotted, Sir.")));
    }
    let model = entry(MODELS).filter(|(k, ..)| bare == *k || bare.ends_with(&format!(" {k}")));
    // How he names the thing: Sir's own words in English, unless they are just the thing's name.
    let named = |fallback: String| match model {
        Some((k, _, name)) if german || bare == *k => name.to_string(),
        _ if german => fallback,
        _ => sentence_start(&own(&subject)),
    };
    // What moves: a simulation, where the Lab has one of the kind.
    if moving || words.iter().any(|w| MOVES.contains(w)) {
        if let Some(args) = crate::simulation_lab::simulation_request(&format!("simulate {bare}"), &[]) {
            let title = args["title"].as_str().unwrap_or("It").to_string();
            return Some(chosen("launch_interactive_simulation", args, format!("{}, as a simulation, Sir.", named(title))));
        }
    }
    // A thing to turn by hand: the 3D studio (a drawing is never one).
    if let (Some((key, query, _)), false) = (model, draw) {
        // English keeps Sir's own words ("a V8 engine"); a bare name gets the studio's.
        let query = if german || bare == *key { query.to_string() } else { bare.clone() };
        return Some(chosen(crate::model3d::TOOL, json!({ "query": query }), format!("{}, in 3D, Sir.", named(String::new()))));
    }
    // Anything else: pictures of it, by its English name where he knows one.
    let query = model.filter(|_| german).map_or_else(|| bare.clone(), |(_, query, _)| query.to_string());
    let what = in_sentence(&named(own(&bare)));
    let line = if draw { format!("I can't draw, Sir, so here's a picture of {what}.") } else { format!("A picture of {what}, Sir.") };
    Some(chosen("show_image", json!({ "query": query }), line))
}

/// The line naming his pick, taken out of a tool's arguments (None for any other order).
pub fn take_line(args: &mut Value) -> Option<String> {
    args.as_object_mut()?.remove(PICKED)?.as_str().map(str::to_string)
}

fn chosen(name: &'static str, mut args: Value, line: String) -> Intent {
    args[PICKED] = json!(line);
    Intent::Tool { name, args }
}

fn strip_article(s: &str) -> String {
    let t = s.trim();
    ARTICLES.iter().find_map(|a| t.strip_prefix(a).and_then(|rest| rest.strip_prefix(' '))).unwrap_or(t).trim().to_string()
}

/// `words` (lower case, as normalised) as Sir wrote them in `text`, where they stand there.
fn as_said(text: &str, words: &str) -> String {
    let lower = text.to_lowercase();
    lower.find(words).and_then(|at| text.get(at..at + words.len())).filter(|found| found.to_lowercase() == words).unwrap_or(words).to_string()
}

fn sentence_start(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// A name inside a sentence: "The Moon" → "the Moon", "A human heart" → "a human heart".
fn in_sentence(name: &str) -> String {
    match name.split_once(' ') {
        Some((first, rest)) if matches!(first, "The" | "A" | "An") => format!("{} {rest}", first.to_lowercase()),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(text: &str) -> (String, Value) {
        match pick(text) {
            Some(Intent::Tool { name, args }) => (name.to_string(), args),
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn unclear_show_orders_are_shown_the_likeliest_way() {
        // A thing to turn by hand.
        for (said, query, line) in [
            ("Show me the moon", "moon", "The Moon, in 3D, Sir."),
            ("show me how a heart works", "human heart", "A human heart, in 3D, Sir."),
            ("Show me DNA", "dna double helix", "DNA, in 3D, Sir."),
            ("show me a V8 engine", "v8 engine", "A V8 engine, in 3D, Sir."),
            ("Kannst du mir ein Herz zeigen?", "human heart", "A human heart, in 3D, Sir."),
            ("Can you show me the Eiffel Tower?", "eiffel tower", "The Eiffel Tower, in 3D, Sir."),
        ] {
            let (tool, args) = picked(said);
            assert_eq!((tool.as_str(), args["query"].as_str(), args[PICKED].as_str()), (crate::model3d::TOOL, Some(query), Some(line)), "{said}");
        }
        // What moves.
        for (said, line) in [
            ("show me waves", "Waves, as a simulation, Sir."),
            ("visualise ripples in water", "Ripples in water, as a simulation, Sir."),
            ("Zeig mir eine stehende Welle", "Standing wave, as a simulation, Sir."),
            ("show me how a pendulum swings", "A pendulum, as a simulation, Sir."),
            // The planets orbit: the Lab's gravity, not a model standing still.
            ("Zeig mir das Sonnensystem", "The solar system, as a simulation, Sir."),
            ("show me the planets", "The solar system, as a simulation, Sir."),
        ] {
            let (tool, args) = picked(said);
            assert_eq!((tool.as_str(), args[PICKED].as_str()), ("launch_interactive_simulation", Some(line)), "{said}: {args}");
        }
        let (tool, args) = picked("show me a sine wave");
        assert_eq!((tool.as_str(), args["sim_type"].as_str(), args["formula_or_params"].as_str()), ("launch_interactive_simulation", Some("function"), Some("sin(x)")));
        assert_eq!(args[PICKED], "A sine curve, plotted, Sir.");
        // Anything else: pictures, and a drawing asked for is a picture too.
        for (said, query, line) in [
            ("show me a volcano", "volcano", "A picture of a volcano, Sir."),
            ("Show me a tornado.", "tornado", "A picture of a tornado, Sir."),
            ("show me Paris", "paris", "A picture of Paris, Sir."),
            ("make me a drawing of a cat", "cat", "I can't draw, Sir, so here's a picture of a cat."),
            ("draw the moon", "moon", "I can't draw, Sir, so here's a picture of the Moon."),
            ("Zeichne den Mond", "moon", "I can't draw, Sir, so here's a picture of the Moon."),
            ("Zeig mir einen Vulkan", "vulkan", "A picture of Vulkan, Sir."),
        ] {
            let (tool, args) = picked(said);
            assert_eq!((tool.as_str(), args["query"].as_str(), args[PICKED].as_str()), ("show_image", Some(query), Some(line)), "{said}");
        }
    }

    #[test]
    fn what_is_his_sirs_or_nothing_at_all_is_left_alone() {
        for said in [
            "show me my calendar", "show me the weather", "show me how to tie a tie", "show me what you can do", "show me that",
            "show me something cool", "show me the files in my downloads", "zeig mir deine Einstellungen", "show me the way",
            "show me z = x^2 - y^2", "show me the latest news about Tesla", "open the moon", "what is the moon", "show me around",
            "show me a picture of the moon", "show me the hotkeys", "show me the queue", "kannst du mir das zeigen",
            "show me the text console", "zeig mir wie man Nudeln kocht", "show me a good restaurant nearby", "plot sin(x)",
        ] {
            assert!(pick(said).is_none(), "{said}");
        }
    }

    #[test]
    fn the_whole_road_ends_at_the_pick_only_where_no_rule_knows_the_words() {
        let road = |said: &str| crate::phrasings::route(said).map(|(tool, args)| (tool, args[PICKED].as_str().map(str::to_string)));
        // German "zeig mir" brought up a window called "Sonnensystem".
        assert_eq!(road("Zeig mir das Sonnensystem"), Some(("launch_interactive_simulation".to_string(), Some("The solar system, as a simulation, Sir.".to_string()))));
        assert_eq!(road("zeig mir Discord").map(|r| r.0).as_deref(), Some("window_management"));
        // Every rule before it keeps its words.
        assert_eq!(road("Show me a picture of the Eiffel Tower").map(|r| r.1), Some(None));
        assert_eq!(road("Show me the moon in 3D").map(|r| r.1), Some(None));
        assert_eq!(road("Show me the queue").map(|r| r.0).as_deref(), Some("control_music"));
    }

    #[test]
    fn the_line_is_taken_out_before_the_tool_runs() {
        let Some(Intent::Tool { mut args, .. }) = pick("show me the moon") else { panic!("a pick") };
        assert_eq!(take_line(&mut args).as_deref(), Some("The Moon, in 3D, Sir."));
        assert_eq!(args, json!({ "query": "moon" }));
        assert_eq!(take_line(&mut json!({ "query": "moon" })), None);
    }
}
