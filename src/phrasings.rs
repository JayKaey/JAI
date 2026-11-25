//! How many of the ways Sir might say an order are understood without the language model.
//!
//! The corpus (`tests/fixtures/phrasings_*.json`) was written by writers who had not seen the
//! parsers: for every order, the same thing said fourteen ways (English and German, polite,
//! terse, as a wish, as a question, with other verbs and nouns). Nine of each are for working
//! on ("dev"), five are held back ("heldout") and only counted. `not_commands` are sentences
//! that mention the same things and are no orders.
//!
//! An order counts as understood when the sentence takes the road of the wording it was written
//! for (`CANON`, or one of the other wordings of the same order in `ALSO`): the same tool with
//! the same arguments, by the fast path as the brain asks it (`route`). The same pass is made
//! with the ways silenced (`ways::silence`), which is the fast path as it was before them.

use crate::brain::Fast;
use crate::intents::Intent;
use serde_json::{json, Value};

const CORPORA: [&str; 3] = [
    include_str!("../tests/fixtures/phrasings_a.json"),
    include_str!("../tests/fixtures/phrasings_b.json"),
    include_str!("../tests/fixtures/phrasings_c.json"),
];

/// Each order by its id, said the one way it was written for.
const CANON: &[(&str, &str)] = &[
    // --- the kit, part one
    ("dictation.start", "start dictating"),
    ("dictation.stop", "stop dictating"),
    ("snippet.save_clipboard", "save my clipboard as snippet address"),
    ("snippet.type", "type my address"),
    ("snippet.list", "list my snippets"),
    ("snippet.delete", "delete the snippet address"),
    ("keys.next_tab", "next tab"),
    ("keys.previous_tab", "previous tab"),
    ("keys.new_tab", "new tab"),
    ("keys.reopen_tab", "reopen the closed tab"),
    ("keys.scroll_down", "scroll down"),
    ("keys.scroll_up", "scroll up"),
    ("keys.top", "go to the top"),
    ("keys.bottom", "go to the bottom"),
    ("keys.page_back", "page back"),
    ("keys.page_forward", "page forward"),
    ("keys.reload", "reload the page"),
    ("keys.select_all", "select all"),
    ("keys.paste", "paste it"),
    ("keys.enter", "press enter"),
    ("keys.find", "search the page for photosynthesis"),
    ("note.add", "note: buy milk on Friday"),
    ("note.show", "show my notes"),
    ("note.search", "what did I note about the test"),
    ("note.delete_last", "delete the last note"),
    ("sleep.set", "sleep timer 20 minutes"),
    ("sleep.cancel", "cancel the sleep timer"),
    ("sleep.status", "how long is left on the sleep timer"),
    ("rule.add_start", "whenever discord starts, mute the music"),
    ("rule.list", "list my rules"),
    ("rule.delete", "delete rule 2"),
    ("bright.set", "brightness 40 percent"),
    ("bright.up", "make the screens brighter"),
    ("bright.down", "make the screens darker"),
    ("bright.status", "how bright are my monitors"),
    ("layout.restore", "restore the study layout"),
    ("layout.list", "list my layouts"),
    ("layout.delete", "delete the study layout"),
    ("dashboard.on", "portrait dashboard"),
    ("dashboard.off", "portrait dashboard off"),
    ("read.selection", "read this to me"),
    ("read.clipboard", "read my clipboard"),
    // --- the kit, part two
    ("read.on", "read on"),
    ("read.stop", "stop reading"),
    ("coach.start", "practise my pronunciation"),
    ("coach.stop", "stop the coach"),
    ("hearing.show", "show the hearing log"),
    ("hearing.last", "what did you just hear"),
    ("guest.learn", "this is my friend Tom, remember his voice"),
    ("guest.music_only", "Tom may only control the music"),
    ("guest.everything", "Tom may do everything"),
    ("guest.forget", "forget Tom's voice"),
    ("guest.list", "who do you know"),
    ("guest.who", "who is speaking"),
    ("recap", "what did I do today"),
    ("doctor", "why is my internet so slow"),
    ("privacy.show", "privacy board"),
    ("privacy.clips_off", "stop keeping my voice clips"),
    ("privacy.clips_on", "keep my voice clips again"),
    ("changes.show", "show the changes"),
    ("changes.explain", "explain the sleep timer"),
    ("backup.now", "back up your memory"),
    ("backup.status", "when was the last backup"),
    ("subs.top", "subtitles to the top"),
    ("subs.monitor", "put the subtitles on monitor 2"),
    ("subs.follow", "subtitles follow my window"),
    ("subs.where", "where are the subtitles"),
    ("places.forget", "forget where the windows were"),
    ("notifications.clear", "clear my notifications"),
    ("selection.explain", "explain this"),
    ("selection.translate", "translate this into german"),
    ("selection.summarise", "summarise this"),
    ("guestmode.on", "guest mode"),
    ("guestmode.off", "guest mode off"),
    ("lock.on", "only obey me"),
    ("lock.off", "listen to everyone"),
    ("voice.learn", "learn my voice"),
    ("night.on", "night mode"),
    ("night.off", "night mode off"),
    ("background.on", "live desktop on"),
    ("background.off", "live desktop off"),
    ("missed", "what did I miss"),
    ("bright.side", "dim the side monitors"),
    // --- the older everyday orders
    ("app.open", "open Spotify"),
    ("app.close", "close Spotify"),
    ("app.switch", "switch to Spotify"),
    ("vol.up", "volume up"),
    ("vol.down", "volume down"),
    ("vol.set", "set the volume to 30 percent"),
    ("vol.mute", "mute"),
    ("music.pause", "pause the music"),
    ("music.resume", "resume the music"),
    ("music.next", "next song"),
    ("music.previous", "previous song"),
    ("music.what", "what's playing"),
    ("time", "what time is it"),
    ("win.minimize_all", "minimize everything"),
    ("win.snap_left", "snap Chrome to the left"),
    ("view.stats", "open my system stats"),
    ("view.settings", "open settings"),
    ("view.mixer", "open the mixer"),
    ("view.console", "open the text console"),
    ("theme", "change the theme to Arc Cyan"),
    ("protocol.run", "engage the study protocol"),
    ("system.status", "system status"),
    ("features", "list your features"),
    ("windows.close", "close the windows you opened"),
    ("hud.calm", "calm mode"),
    ("hud.mini", "mini mode"),
    ("subs.on", "subtitles on"),
    ("lyrics.on", "show the lyrics"),
    ("image", "show me a picture of the Eiffel Tower"),
    ("undo", "undo that"),
    ("timer.set", "set a timer for 10 minutes"),
    ("calendar", "show me my calendar"),
    ("earth.show", "show me the earth"),
    ("zoom.out", "zoom out"),
    ("mic.stop", "stop listening"),
    ("pc.shutdown", "shut down my PC"),
    ("news.country", "give me the most important news from germany"),
    ("news.country_de", "die wichtigsten nachrichten aus deutschland"),
    // --- J.A.I.OS: the system's own orders (`os_orders`)
    ("system.install", "install firefox"),
    ("system.remove", "uninstall vlc"),
    ("system.update", "update yourself"),
    ("system.check", "are there system updates"),
    ("system.undo_update", "undo the last update"),
    ("system.snapshots", "show the snapshots"),
    ("system.secure_boot", "finish secure boot"),
    ("jaios.room", "make room for J.A.I.OS"),
    ("jaios.stick", "make a J.A.I.OS stick"),
    ("jaios.ready", "is my PC ready for J.A.I.OS"),
];

/// Other wordings of an order whose road counts as its own (the same thing by another of the
/// fast path's tools, or by a step where the first wording names none).
const ALSO: &[(&str, &[&str])] = &[
    ("keys.scroll_down", &["scroll down a bit"]),
    ("keys.scroll_up", &["scroll up a bit"]),
    ("bright.side", &["make the side monitors darker"]),
    ("doctor", &["warum ist mein Internet so langsam", "diagnose my internet"]),
    ("missed", &["did I miss anything"]),
    ("calendar", &["open my calendar"]),
    ("music.pause", &["pause the track"]),
    ("vol.up", &["volume up a bit", "Lautstärke hoch", "turn it up", "turn it up a little"]),
    ("vol.down", &["volume down a bit", "Lautstärke runter", "turn it down", "turn it down a little"]),
    ("view.settings", &["open the interface settings"]),
    ("vol.set", &["put the volume at 30 percent"]),
];

/// Orders whose words are free (what a picture is of, what a note is searched for: "zum Test"
/// is "about the test"): the tool and what it is asked to do are what counts.
const FREE: &[&str] = &["image", "note.search"];

fn of(intent: &Intent) -> (String, Value) {
    match intent {
        Intent::Tool { name, args } => (name.to_string(), args.clone()),
        Intent::Silent(action) => (action.tool.to_string(), action.args.clone()),
        Intent::Answer(_) => ("(answer)".to_string(), json!({})),
        Intent::Stop => ("(stop)".to_string(), json!({})),
        Intent::Time => ("(time)".to_string(), json!({})),
        Intent::Date => ("(date)".to_string(), json!({})),
    }
}

/// Where a sentence goes without the language model: said another way first, as the brain asks
/// (`Brain::run_turn`), then as it stands. None: it is the language model's.
pub(crate) fn route(text: &str) -> Option<(String, Value)> {
    let road = |found: &Fast| match found {
        Fast::Intent(intent) => of(intent),
        Fast::Route(route) => (route.tool.to_string(), route.args.clone()),
        Fast::Selection(ask) => ("(selection)".to_string(), json!(ask)),
        Fast::Other => match crate::system_power::power_request(text) {
            // (The PC's power is the brain's double-checked dialogue, with no tool of its own.)
            Some(action) => ("(power)".to_string(), json!({ "action": action.id() })),
            None => ("(other)".to_string(), json!({})),
        },
    };
    if let Some((_, found)) = crate::brain::another_way(text) {
        return Some(road(&found));
    }
    // (His own pick for unclear words is the brain's last rule: "show me the moon".)
    crate::brain::fast_as_said(text).or_else(|| crate::show_me::pick(text).map(Fast::Intent)).as_ref().map(road)
}

/// Arguments as they are compared: words in small letters, without punctuation.
fn plain(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(crate::ear::words(s).join(" ")),
        Value::Array(items) => Value::Array(items.iter().map(plain).collect()),
        // (When a timer was started is no part of the order.)
        Value::Object(map) => Value::Object(map.iter().filter(|(k, _)| !matches!(k.as_str(), "started_at_ms" | "elapsed_ms")).map(|(k, v)| (k.clone(), plain(v))).collect()),
        other => other.clone(),
    }
}

fn same(a: &Option<(String, Value)>, b: &Option<(String, Value)>) -> bool {
    match (a, b) {
        (Some((tool_a, args_a)), Some((tool_b, args_b))) => tool_a == tool_b && plain(args_a) == plain(args_b),
        _ => false,
    }
}

/// How many phrasings there are and how many take the road of their order.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Count {
    pub said: usize,
    pub understood: usize,
}

impl Count {
    pub fn share(&self) -> f32 {
        self.understood as f32 / self.said.max(1) as f32
    }
}

/// What one pass over the collections finds.
#[derive(Debug, Default)]
pub(crate) struct Measured {
    /// Per collection (the kit's first half, its second half, the older everyday orders): the
    /// phrasings worked on, and the ones held back.
    pub parts: Vec<(Count, Count)>,
    /// Every sentence that is no order, and the road it takes all the same (None: the model's).
    pub talk: Vec<(String, Option<(String, Value)>)>,
}

impl Measured {
    pub fn dev(&self) -> Count {
        Count { said: self.parts.iter().map(|(d, _)| d.said).sum(), understood: self.parts.iter().map(|(d, _)| d.understood).sum() }
    }

    pub fn held(&self) -> Count {
        Count { said: self.parts.iter().map(|(_, h)| h.said).sum(), understood: self.parts.iter().map(|(_, h)| h.understood).sum() }
    }
}

/// One pass over the collections. `report`: the phrasings worked on that are not understood
/// are listed there with where they went (the ones held back are only ever counted).
pub(crate) fn measure(mut report: Option<&mut Vec<String>>) -> Measured {
    let canon = |id: &str| CANON.iter().find(|(known, _)| *known == id).map(|(_, say)| *say);
    let mut found = Measured::default();
    for corpus in CORPORA {
        let corpus: Value = serde_json::from_str(corpus).expect("a corpus is valid JSON");
        let (mut dev, mut held) = (Count::default(), Count::default());
        for (split, count) in [("dev", &mut dev), ("heldout", &mut held)] {
            let Some(orders) = corpus[split].as_object() else { continue };
            for (id, ways) in orders {
                let Some(known) = canon(id) else { panic!("the corpus names an order this list does not: {id}") };
                let mut wanted = vec![route(known)];
                wanted.extend(ALSO.iter().filter(|(of, _)| of == id).flat_map(|(_, more)| more.iter()).map(|say| route(say)));
                if wanted[0].is_none() && split == "dev" {
                    if let Some(report) = report.as_mut() {
                        report.push(format!("!! {id}: the wording it was written for, '{known}', is not understood itself"));
                    }
                }
                let free = FREE.contains(&id.as_str());
                let mut missed: Vec<String> = Vec::new();
                for way in ways.as_array().into_iter().flatten().filter_map(Value::as_str) {
                    count.said += 1;
                    let got = route(way);
                    let understood = wanted.iter().any(|w| same(&got, w) || (free && matches!((&got, w), (Some((a, did)), Some((b, asked))) if a == b && did.get("action") == asked.get("action"))));
                    if understood {
                        count.understood += 1;
                    } else {
                        // (With the wordings it was taken for, where none of them was read.)
                        let tried = crate::ways::wordings(&crate::intents::normalize_phrase(way), way);
                        missed.push(format!("{way}  ->  {}{}", got.map_or_else(|| "the model".to_string(), |(tool, args)| format!("{tool} {args}")), if tried.is_empty() { String::new() } else { format!("   [as: {}]", tried.join(" | ")) }));
                    }
                }
                if split == "dev" && !missed.is_empty() {
                    if let Some(report) = report.as_mut() {
                        report.push(format!("-- {id} ({} of {} missed; wanted {})", missed.len(), ways.as_array().map_or(0, Vec::len), wanted[0].as_ref().map_or_else(|| "nothing".to_string(), |(tool, args)| format!("{tool} {args}"))));
                        report.extend(missed.iter().map(|line| format!("     {line}")));
                    }
                }
            }
        }
        found.parts.push((dev, held));
        for sentence in corpus["not_commands"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            found.talk.push((sentence.to_string(), route(sentence)));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The share of the phrasings that must be understood without the language model: the ones
    /// worked on, and the ones held back. (Raise them when they rise; never lower them to make
    /// a change pass.)
    const DEV_AT_LEAST: f32 = 0.95;
    const HELD_AT_LEAST: f32 = 0.60;
    /// How many of the sentences that are no orders the ways may newly take for one.
    const TALK_AT_MOST: usize = 4;

    #[test]
    fn the_ways_sir_might_say_an_order_are_understood() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        crate::hud_dock::begin_frame();
        crate::hud_dock::begin_frame();
        // (What the fast path knows by name: a desk layout of his and a snippet.)
        let _ = crate::kit::put("layout", "study", &json!({ "name": "study" }));
        let _ = crate::kit::put("snippet", "address", &json!({ "text": "12 Example Road" }));
        crate::ways::assume_running(true);
        // The fast path as it was (nothing said another way), then as it is.
        crate::ways::silence(true);
        let before = measure(None);
        crate::ways::silence(false);
        let mut report: Vec<String> = Vec::new();
        let started = std::time::Instant::now();
        let now = measure(Some(&mut report));
        let took = started.elapsed();
        crate::ways::assume_running(false);
        crate::kit::remove("layout", "study");
        crate::kit::remove("snippet", "address");
        let mut summary: Vec<String> = Vec::new();
        for (name, ((dev_before, held_before), (dev, held))) in ["the kit, first half", "the kit, second half", "older everyday orders"].iter().zip(before.parts.iter().zip(&now.parts)) {
            summary.push(format!(
                "{name}: worked on {} -> {} of {} ({:.1} % -> {:.1} %), held back {} -> {} of {} ({:.1} % -> {:.1} %)",
                dev_before.understood, dev.understood, dev.said, dev_before.share() * 100.0, dev.share() * 100.0, held_before.understood, held.understood, held.said, held_before.share() * 100.0, held.share() * 100.0
            ));
        }
        let (dev, held) = (now.dev(), now.held());
        // Sentences that are no orders: what the ways newly take for one. (Every second one is
        // worked on and listed; the others are only counted.)
        let newly: Vec<usize> = (0..now.talk.len()).filter(|&at| !same_road(&before.talk[at].1, &now.talk[at].1)).collect();
        for &at in newly.iter().filter(|at| *at % 2 == 0) {
            let (sentence, road) = &now.talk[at];
            report.push(format!("?? no order, and newly taken for one: '{sentence}' -> {}", road.as_ref().map_or_else(|| "the model".to_string(), |(tool, args)| format!("{tool} {args}"))));
        }
        let taken_before = before.talk.iter().filter(|(_, road)| road.is_some()).count();
        summary.push(format!(
            "phrasings understood without the model: worked on {} -> {} of {} ({:.1} % -> {:.1} %), held back {} -> {} of {} ({:.1} % -> {:.1} %)",
            before.dev().understood, dev.understood, dev.said, before.dev().share() * 100.0, dev.share() * 100.0, before.held().understood, held.understood, held.said, before.held().share() * 100.0, held.share() * 100.0
        ));
        summary.push(format!(
            "of {} sentences that are no orders, {taken_before} were taken for one before; the ways newly take {} ({} of them among the ones only counted)",
            now.talk.len(),
            newly.len(),
            newly.iter().filter(|at| *at % 2 == 1).count()
        ));
        summary.push(format!("one pass over {} sentences took {} ms", dev.said + held.said + now.talk.len(), took.as_millis()));
        for line in &summary {
            eprintln!("{line}");
        }
        if let Ok(path) = std::env::var("JAI_PHRASINGS") {
            report.extend(summary);
            let _ = std::fs::write(path, report.join("\n"));
        }
        assert!(dev.share() >= DEV_AT_LEAST, "worked on: {:.3}", dev.share());
        assert!(held.share() >= HELD_AT_LEAST, "held back: {:.3}", held.share());
        assert!(newly.len() <= TALK_AT_MOST, "sentences that are no orders, newly taken for one: {}", newly.len());
    }

    /// Sir's words of 2026-11-16: "please shut off my PC" was reworded to "close off my pc" and
    /// closed an app of that name; "power down by PC" ("by" heard for "my") reached the model.
    #[test]
    fn pc_power_orders_reach_the_power_dialogue() {
        let power = Some(("(power)".to_string(), json!({ "action": "shutdown" })));
        for said in [
            "please shut off my PC",
            "JAI, please shut off my PC",
            "please power down by PC",
            "shut down my PC",
            "shut off my computer",
            "power off my PC",
            "power down my PC",
            "turn off my PC",
            "fahr den PC runter",
            "schalte den PC aus",
        ] {
            assert_eq!(route(said), power, "{said}");
        }
        // Apps are still closed, "shut off" one too.
        for (said, app) in [("close Spotify", "spotify"), ("shut Discord", "discord"), ("shut off Spotify", "spotify")] {
            let (tool, args) = route(said).unwrap_or_else(|| panic!("{said}: the model"));
            assert_eq!((tool.as_str(), plain(&args["app_name"])), ("close_application", json!(app)), "{said}");
        }
    }

    fn same_road(a: &Option<(String, Value)>, b: &Option<(String, Value)>) -> bool {
        match (a, b) {
            (None, None) => true,
            _ => same(a, b),
        }
    }

}
