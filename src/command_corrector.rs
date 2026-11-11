//! What Sir most likely said, when Whisper wrote a command as other words.
//!
//! Sir's accented English reaches Whisper as plausible English that is no command: "JAI, told
//! me the world" (show me the world), "Call me Earth" (close the Earth, said over JAI's voice),
//! "Can you full-screen the off-window?" (the Earth window) — all 2026-11-08. When a short spoken
//! sentence routes nowhere, it is compared with the commands that make sense on the screen as it
//! is right now (the views of his that are up, the ones he can open), and rewritten only when one
//! of them is a strong match and the rewrite routes:
//!
//! 1. its sound is near-identical to a command for what is on screen (`intent_engine::sound_key`);
//! 2. it ends in one of his views by name and its verb is one Whisper is known to write for a
//!    command verb ("call me" / "clothes" → "close the", "told me" → "show me");
//! 3. a view of his is up and a word Whisper writes for its name ("off" for "Earth") stands where
//!    the object of a command verb belongs.
//!
//! Never for a question, never for dictation-length text, never into anything that cannot be
//! taken back (no shutdown, no "close yourself"): the worst a wrong rewrite does is show or hide
//! a view. Every rewrite is logged as `[CORRECT] 'x' → 'y' (score)`.
//!
//! `whisper_prompt` would give Whisper the names of the views that are up, so that it writes
//! "Earth" rather than "off" in the first place. It is measured, not used: on the bench's clips
//! it cost English accuracy (`stt_bench`, "one pass, Earth up"), so the screen is consulted
//! after decoding only.

use crate::intent_engine::{sound_key, sound_similarity};
use crate::intents::Intent;

/// One of JAI's views as Sir names it.
struct View {
    /// Its panel or block id (what is open).
    id: &'static str,
    /// The words that name it at the end of a command.
    names: &'static [&'static str],
    /// How a corrected command names it.
    spoken: &'static str,
    /// What Whisper writes for its name in Sir's accent, in a command's object place.
    misheard: &'static [&'static str],
    /// The words Whisper would be given when it is up (`whisper_prompt`, measured only).
    #[cfg_attr(not(test), allow(dead_code))]
    prompt: &'static str,
}

const VIEWS: &[View] = &[
    View { id: "earth", names: &["earth", "world", "globe", "planet"], spoken: "the Earth", misheard: &["off", "of", "erth", "earths", "worth", "birth"], prompt: "Earth, globe, satellite view." },
    View { id: "network_radar", names: &["radar", "network radar"], spoken: "the radar", misheard: &["radio", "rader", "raider"], prompt: "Radar." },
    View { id: "disk_map", names: &["disk map", "drive map", "disc map"], spoken: "the disk map", misheard: &[], prompt: "Disk map." },
    View { id: "system_telemetry", names: &["stats", "system stats", "telemetry"], spoken: "my stats", misheard: &["steps", "stacks"], prompt: "Stats." },
    View { id: "schedule", names: &["schedule", "timetable"], spoken: "my schedule", misheard: &[], prompt: "Schedule." },
];

/// What is on screen right now: the ids of the views of his that are up.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scene {
    pub open: Vec<&'static str>,
}

impl Scene {
    pub fn live() -> Self {
        let open = VIEWS
            .iter()
            .map(|v| v.id)
            .filter(|id| if crate::jarvis_views::is_panel(id) { crate::jarvis_views::panel_is_open(id) } else { crate::hud_dock::is_open(id) })
            .collect();
        Self { open }
    }

    fn is_open(&self, id: &str) -> bool {
        self.open.contains(&id)
    }
}

/// Whisper's prompt for the next clip: Sir's vocabulary prompt and the names of the views that
/// are up (a few nouns, never a command: a sentence in the prompt is what Whisper invents on
/// unclear audio). For the bench only (see the module notes).
#[cfg_attr(not(test), allow(dead_code))]
pub fn whisper_prompt(base: &str, scene: &Scene) -> String {
    let extra: Vec<&str> = VIEWS.iter().filter(|v| scene.is_open(v.id)).map(|v| v.prompt).take(2).collect();
    match (base.trim().is_empty(), extra.is_empty()) {
        (_, true) => base.to_string(),
        (true, false) => extra.join(" "),
        (false, false) => format!("{} {}", base.trim(), extra.join(" ")),
    }
}

/// A rewrite of what Whisper heard.
#[derive(Debug, Clone, PartialEq)]
pub struct Correction {
    pub text: String,
    /// How strong the match was (1 = certain).
    pub score: f32,
    /// Which kind of match it was.
    pub why: &'static str,
}

/// The intent a sentence has on the fast path's deterministic routes (the ones a correction may
/// lead to): commands, his views, his own blocks.
pub fn routed_intent(text: &str) -> Option<Intent> {
    let t = crate::typos::normalize_user_input_typos(text);
    crate::intents::hud_view_command(&t)
        .or_else(|| crate::intents::internal_view_command(&t).map(|args| Intent::Tool { name: "open_jarvis_view", args }))
        .or_else(|| crate::intents::parse(&t))
        .or_else(|| match crate::intent_engine::route(&t, &crate::intent_engine::ReflexContext::live()).map(|r| r.reflex) {
            Some(crate::intent_engine::Reflex::Silent(action)) => Some(Intent::Silent(action)),
            _ => None,
        })
}

/// Whether `text` routes on the fast path's deterministic parsers.
pub fn routes(text: &str) -> bool {
    routed_intent(text).is_some()
}

/// A rewrite needs this much (sound alike, whole sentence) …
const SOUND_ALIKE: f32 = 0.82;
/// … and this much more than the next command that would do something else.
const MARGIN: f32 = 0.06;

/// Verbs Whisper writes for Sir's command verbs in front of a view's name, as heard → meant.
/// "close" kinds only when that view is up, "show" kinds only when it is not.
const VERB_SLIPS: &[(&str, &str)] = &[
    ("call me", "close"),
    ("call the", "close"),
    ("cold the", "close"),
    ("clothes the", "close"),
    ("clothes", "close"),
    ("closed the", "close"),
    ("closed", "close"),
    ("closer", "close"),
    ("glow's the", "close"),
    ("told me", "show me"),
    ("so me", "show me"),
    ("sure me", "show me"),
    ("show", "show me"),
    ("shall me", "show me"),
    ("showed me", "show me"),
    ("open up", "open"),
];

/// Words in front of a verb that change nothing ("can you", "please", "jarvis").
const LEAD: &[&str] = &["jai", "hey", "ok", "okay", "please", "can", "could", "would", "you", "now", "then", "and", "so", "just", "no", "yes", "yeah", "um", "uh", "the"];

/// Commands that make sense on this screen, as (sentence, meaning id).
fn scene_commands(scene: &Scene) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for v in VIEWS {
        if scene.is_open(v.id) {
            for verb in ["close", "hide"] {
                out.push((format!("{verb} {}", v.spoken), format!("close:{}", v.id)));
            }
            if v.id == "earth" {
                out.push(("switch to satellite view".to_string(), "earth:satellite".to_string()));
                out.push(("switch to map view".to_string(), "earth:map".to_string()));
            }
        } else {
            out.push((format!("show me {}", v.spoken), format!("open:{}", v.id)));
            out.push((format!("open {}", v.spoken), format!("open:{}", v.id)));
        }
    }
    out.push(("what's playing".to_string(), "music:playing".to_string()));
    out
}

fn clean_words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|w| w.trim_matches('\'').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// A question or anything that is no short order is never rewritten.
fn untouchable(heard: &str, words: &[String]) -> bool {
    const ASKING: &[&str] = &["what", "why", "how", "who", "when", "where", "which", "is", "are", "do", "does", "did", "was", "were", "should", "wie", "was", "warum", "wer", "wann", "wo", "welche"];
    heard.trim_end().ends_with('?') || crate::intents::only_asks(heard) || words.is_empty() || words.len() > 9 || words.first().is_some_and(|w| ASKING.contains(&w.as_str()))
}

/// The words after a leading "JAI", "can you", "please".
fn order_words(words: &[String]) -> &[String] {
    let start = words.iter().position(|w| !LEAD.contains(&w.as_str())).unwrap_or(words.len());
    let mut end = words.len();
    while end > start && matches!(words[end - 1].as_str(), "please" | "jai" | "sir" | "now" | "you") {
        end -= 1;
    }
    &words[start..end]
}

/// (2) "<slipped verb> [the|my] <view name>".
fn verb_slip(core: &[String], scene: &Scene) -> Option<Correction> {
    for v in VIEWS {
        for name in v.names {
            let name_words: Vec<&str> = name.split_whitespace().collect();
            if core.len() <= name_words.len() || core[core.len() - name_words.len()..].iter().map(String::as_str).ne(name_words.iter().copied()) {
                continue;
            }
            let mut verb: Vec<&str> = core[..core.len() - name_words.len()].iter().map(String::as_str).collect();
            if matches!(verb.last(), Some(&"the") | Some(&"my") | Some(&"your")) && verb.len() > 1 {
                // "told me the world": the article stays with the slip only when it is part of it.
                let with = verb.join(" ");
                if !VERB_SLIPS.iter().any(|(heard, _)| *heard == with) {
                    verb.pop();
                }
            }
            let verb = verb.join(" ");
            let Some((_, meant)) = VERB_SLIPS.iter().find(|(heard, _)| *heard == verb) else { continue };
            let open = scene.is_open(v.id);
            let fits = if *meant == "close" { open } else { !open };
            if !fits {
                continue;
            }
            return Some(Correction { text: format!("{meant} {}", v.spoken), score: 0.9, why: "verb slip before a view's name" });
        }
    }
    None
}

/// (3) "full-screen the off-window" with the Earth up: the misheard name in an object place.
fn object_slip(words: &[String], scene: &Scene) -> Option<Correction> {
    const VERBS: &[&str] = &["close", "hide", "full", "fullscreen", "full-screen", "maximize", "maximise", "minimize", "minimise", "move", "make", "enlarge", "shrink", "show"];
    if !words.iter().any(|w| VERBS.iter().any(|v| w.starts_with(v))) {
        return None;
    }
    for v in VIEWS.iter().filter(|v| scene.is_open(v.id) && !v.misheard.is_empty()) {
        // Whisper hyphenates the object onto "window": "off-window".
        let split: Vec<String> = words.iter().flat_map(|w| w.split('-').map(str::to_string).collect::<Vec<_>>()).collect();
        let Some(at) = split.iter().enumerate().position(|(i, w)| {
            v.misheard.contains(&w.as_str()) && i > 0 && matches!(split[i - 1].as_str(), "the" | "my" | "your" | "screen" | "close" | "hide") && split.get(i + 1).is_none_or(|n| matches!(n.as_str(), "window" | "view" | "please" | "for" | "now" | "jai"))
        }) else {
            continue;
        };
        let name = v.spoken.trim_start_matches("the ").trim_start_matches("my ");
        let mut fixed = split.clone();
        fixed[at] = name.to_string();
        return Some(Correction { text: fixed.join(" "), score: 0.85, why: "misheard view name in a command" });
    }
    None
}

/// (1) Near-identical in sound to one command for this screen, and clearly nearer than to any
/// command that would do something else.
fn sounds_like(core: &[String], scene: &Scene) -> Option<Correction> {
    if !(2..=6).contains(&core.len()) {
        return None;
    }
    let key = sound_key(&core.join(" "));
    if key.len() < 5 {
        return None;
    }
    let scored: Vec<(String, String, f32)> = scene_commands(scene).into_iter().map(|(text, meaning)| {
        let s = sound_similarity(&key, &sound_key(&text));
        (text, meaning, s)
    }).collect();
    let (text, meaning, score) = scored.iter().max_by(|a, b| a.2.total_cmp(&b.2))?.clone();
    let rival = scored.iter().filter(|(_, m, _)| *m != meaning).map(|(_, _, s)| *s).fold(0.0f32, f32::max);
    (score >= SOUND_ALIKE && score - rival >= MARGIN).then(|| Correction { text, score, why: "sounds like a command for this screen" })
}

/// The command Sir most likely said, when `heard` routes nowhere and one that makes sense on
/// `scene` is a strong match whose rewrite routes (`routes`). None otherwise.
pub fn correct(heard: &str, scene: &Scene, routes: &dyn Fn(&str) -> bool) -> Option<Correction> {
    let words = clean_words(heard);
    if untouchable(heard, &words) || routes(heard) {
        return None;
    }
    let core = order_words(&words);
    if core.is_empty() {
        return None;
    }
    // Never into anything that cannot be taken back.
    let safe = |c: &Correction| crate::intent_engine::deterministic_action(&c.text).is_none() && !c.text.contains("yourself");
    [verb_slip(core, scene), object_slip(&words, scene), sounds_like(core, scene)]
        .into_iter()
        .flatten()
        .find(|c| safe(c) && clean_words(&c.text) != words && routes(&c.text))
}

/// `correct` on the live screen, ready to run: the intent and the corrected sentence.
pub fn corrected_command(heard: &str) -> Option<(Intent, Correction)> {
    let fixed = correct(heard, &Scene::live(), &routes)?;
    let intent = routed_intent(&fixed.text)?;
    Some((intent, fixed))
}

/// The view a "close" intent names.
#[cfg(test)]
fn close_target(intent: &Intent) -> Option<String> {
    match intent {
        Intent::Silent(a) if a.tool == "hud_board" && a.args["action"] == "close" => a.args["target"].as_str().map(str::to_string),
        Intent::Tool { name: "hud_board", args } if args["action"] == "close" => args["target"].as_str().map(str::to_string),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn earth_up() -> Scene {
        Scene { open: vec!["earth"] }
    }

    #[test]
    fn misheard_view_commands_from_the_log_are_corrected() {
        // 23:28:00 "JAI told me the world." (the Earth was not up yet).
        let fixed = correct("told me the world", &Scene::default(), &routes).expect("show me the world");
        assert_eq!(fixed.text, "show me the Earth");
        assert!(matches!(routed_intent(&fixed.text), Some(Intent::Tool { name: "open_jarvis_view", ref args }) if args["view"] == "earth"), "{:?}", routed_intent(&fixed.text));
        // 23:28:28 "Call me Earth." over JAI's voice, with the Earth up: "close the Earth".
        let fixed = correct("Call me Earth.", &earth_up(), &routes).expect("close the Earth");
        assert_eq!(fixed.text, "close the Earth");
        assert_eq!(routed_intent(&fixed.text).as_ref().and_then(close_target).as_deref(), Some("earth"));
        // … but never with the Earth down (then it is no command for this screen).
        assert_eq!(correct("Call me Earth.", &Scene::default(), &routes), None);
        // 23:29:34 "Can you full-screen the off-window please?" with the Earth up.
        let full = |t: &str| t.contains("full") && t.contains("earth");
        let fixed = correct("Can you full-screen the off-window please", &earth_up(), &|t: &str| full(&t.to_lowercase())).expect("the Earth window");
        assert_eq!(fixed.text, "can you full screen the Earth window please");
    }

    #[test]
    fn questions_dictation_commands_that_route_and_irreversible_ones_stay() {
        assert_eq!(correct("What is the world population?", &earth_up(), &routes), None, "a question");
        assert_eq!(correct("Told me the world is round and that the moon is made of cheese, write that down", &Scene::default(), &routes), None, "dictation length");
        assert_eq!(correct("Show me the world.", &Scene::default(), &routes), None, "already a command");
        assert_eq!(correct("Include yourself.", &earth_up(), &routes), None, "never into 'close yourself'");
        assert_eq!(correct("Call me Tony.", &earth_up(), &routes), None, "a real name request is the memory's");
        assert_eq!(correct("I told my friend about the world cup", &Scene::default(), &routes), None);
    }

    #[test]
    fn the_prompt_names_what_is_up_and_nothing_else() {
        let base = "Hey JAI. Orb, HUD, board.";
        assert_eq!(whisper_prompt(base, &Scene::default()), base);
        assert_eq!(whisper_prompt(base, &earth_up()), "Hey JAI. Orb, HUD, board. Earth, globe, satellite view.");
        assert_eq!(whisper_prompt("", &earth_up()), "Earth, globe, satellite view.");
        // Never a command sentence in the prompt.
        assert!(!whisper_prompt(base, &Scene { open: VIEWS.iter().map(|v| v.id).collect() }).to_lowercase().contains("close"));
    }
}
