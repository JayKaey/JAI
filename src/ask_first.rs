//! He asks before a wrong guess changes something (Sir, 2026-11-13).
//!
//! That evening four orders the fast path did not know went to the language model, and the
//! model reached for whatever tool lay nearest: "save that desk layout as standard" made a
//! protocol called "no_think", "Jay Layout Standard" another called "jay_layout_standard",
//! "the Jay layout standard" switched to the orb layout, "open your pronunciation coach" woke
//! him up, "throw it on all monitors" stretched the board across them.
//!
//! So a call the model makes that creates, deletes or switches something is carried out only
//! when the order holds one of that tool's own words: "protocol" for a protocol, the layout's
//! name for a layout, "sleep" for his sleep, a word of deleting for anything that deletes.
//! Without one it is a guess, and he asks first (`question`): "Did you mean: create a protocol
//! called 'no think', Sir?" Yes carries it out; no, silence or anything else drops it
//! (`hold`, `answer`), and anything else is his next order as it stands.
//!
//! What only looks or reads is never held back (`tools::only_looks`), and a tool that is not
//! listed here runs as it always did: the list names what a wrong guess would cost him.

use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long he waits for the answer once the question is said.
pub const WAITS: Duration = Duration::from_secs(20);

/// What the language model is told of a call that is held for Sir's yes.
pub const HELD: &str = "Not done: this was a guess at what Sir meant, so he is asked first whether he meant it. Nothing was changed. Wait for his answer.";

// ---------------------------------------------------------------- a tool's own words ----

/// Words of deleting, for anything that deletes.
const DELETES: &[&str] = &[
    "delete", "remove", "forget", "erase", "wipe", "clear", "cancel", "drop", "scrap", "trash", "reset", "uninstall", "get rid", "throw away", "no longer", "dont need", "lösch", "loesch", "entfern", "vergiss",
    "vergessen", "streich", "storniere", "weg damit", "nicht mehr", "zurücksetz",
];
/// Words of closing and ending.
const CLOSES: &[&str] = &["close", "quit", "exit", "kill", "end", "shut", "stop", "terminate", "schließ", "schliess", "beend", "zumach", "mach zu"];

/// Whether one of `own` is in the order: a short word as a word of its own, a longer one as
/// the beginning of a word ("protocols", "spanning") or, longer still, inside one (German
/// joins its words: "Desktophintergrund"); a phrase as it stands.
fn carries(order: &[String], own: &[&str]) -> bool {
    let joined = format!(" {} ", order.join(" "));
    own.iter().any(|word| {
        if word.contains(' ') {
            return joined.contains(&format!(" {word}"));
        }
        let letters = word.chars().count();
        order.iter().any(|said| match letters {
            0..=3 => said == word,
            4..=5 => said.starts_with(word),
            _ => said.contains(word),
        })
    })
}

/// Every word of a name the tool was given ("movie night") is in the order.
fn names(order: &[String], name: &str) -> bool {
    let words: Vec<String> = crate::ear::words(&name.replace(['_', '-'], " ")).into_iter().filter(|w| !matches!(w.as_str(), "the" | "my" | "a" | "protocol" | "protokoll" | "mode" | "layout")).collect();
    !words.is_empty() && words.iter().all(|w| order.contains(w))
}

/// The words of the interface's switches, by the argument that sets each.
fn interface_words(key: &str, value: &Value) -> Option<Vec<&'static str>> {
    let text = value.as_str().unwrap_or("").to_lowercase();
    Some(match key {
        // A layout by its own name ("the Jay layout standard" named none and got the orb).
        "layout" => match text.as_str() {
            "compact" => vec!["compact", "small", "chat", "kompakt", "klein"],
            "orb" => vec!["orb", "minimal", "reactor", "reaktor", "kugel"],
            _ => vec!["full", "normal", "default", "complete", "expanded", "everything", "voll", "alles"],
        },
        "span_all_monitors" => vec!["span", "stretch", "streck", "spread", "verteil", "extend", "expand", "across", "one monitor", "single monitor", "main monitor", "first monitor", "einen monitor", "über alle"],
        "fullscreen" => vec!["fullscreen", "full screen", "vollbild", "windowed", "window mode", "fenstermodus", "maximi"],
        "mini_strip" => vec!["mini", "strip", "leiste", "shrink"],
        "desktop_background" | "background_all_monitors" => vec!["background", "backdrop", "wallpaper", "hintergrund", "desktop"],
        "reset" => vec!["reset", "default", "zurücksetz", "original", "factory", "werkseinstellung"],
        "window" => match text.as_str() {
            "hide" => vec!["hide", "tray", "close", "away", "versteck", "schließ", "weg", "background"],
            "show" => vec!["show", "open", "back", "zeig", "öffne", "front"],
            "minimize" => vec!["minimi", "smaller", "kleiner"],
            "maximize" => vec!["maximi", "bigger", "größer", "full"],
            _ => return None,
        },
        "always_on_top" => vec!["top", "front", "pin", "vordergrund", "above", "über"],
        "orb_board_position" => vec!["orb", "emblem", "icon", "logo", "core", "sphere", "kugel", "way", "aside", "side", "middle", "centre", "center", "mitte", "platz", "weg", "corner", "ecke", "left", "right", "top", "bottom", "links", "rechts", "oben", "unten"],
        "night_now" | "night_mode" => vec!["night", "nacht", "day", "tagmodus", "morning"],
        "calm_hud" => vec!["calm", "ruhe", "ruhig", "quiet", "readout", "anzeigen", "dense", "busy", "less on"],
        "live_words" => vec!["words", "worte", "transcript", "mitschrift", "live caption"],
        "music_visuals" => vec!["visual", "dance", "pulse", "move with", "tanz", "beweg"],
        "edge_light" => vec!["edge", "glow", "randlicht"],
        "idle_screen" => vec!["idle", "saver", "calm screen", "ruhebildschirm"],
        "startup_sequence" => vec!["startup", "start up", "boot", "power up", "startsequenz"],
        "subtitles" => vec!["subtitle", "caption", "subs", "untertitel"],
        "lyrics" => vec!["lyric", "songtext", "liedtext", "sing along", "mitsingen"],
        "voice_guest" => vec!["guest", "gast", "gäste", "besuch", "friend", "freund", "visitor", "company"],
        "voice_lock" => vec!["obey", "listen", "lock", "gehorch", "sperre", "everyone", "only me", "nur mir", "nur auf mich"],
        "allow_window_overlap" => vec!["overlap", "stack", "cascade", "apart", "überlapp", "stapel"],
        "sound_cues" => vec!["sound", "cue", "effect", "ton", "töne"],
        _ => return None,
    })
}

/// What a call would do, in the words of an order ("create a protocol called 'no think'"),
/// and the tool's own words of which the order must hold one. None: this call is not held
/// to the rule.
fn guess(order: &[String], tool: &str, args: &Value) -> Option<(String, Vec<&'static str>)> {
    let action = args["action"].as_str().unwrap_or("").to_lowercase().replace([' ', '-'], "_");
    let action = action.as_str();
    let text = |keys: &[&str]| keys.iter().find_map(|k| args[*k].as_str()).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("").replace('_', " ");
    let quoted = |keys: &[&str], or: &str| {
        let name = text(keys);
        if name.is_empty() { or.to_string() } else { format!("'{name}'") }
    };
    let own = |words: &[&'static str]| words.to_vec();
    // (A thing run or loaded by its name: the name in the order is word enough.)
    let named = |keys: &[&str]| names(order, &text(keys));
    Some(match (tool, action) {
        // ------------------------------------------------ protocols, routines, scenes ----
        ("manage_stark_protocol", "create" | "save" | "add" | "update") => (format!("create a protocol called {}", quoted(&["protocol_name", "name", "protocol"], "something")), own(&["protocol", "protokoll", "routine", "macro", "makro"])),
        ("manage_stark_protocol", "delete" | "remove") => (format!("delete the protocol {}", quoted(&["protocol_name", "name", "protocol"], "you mean")), own(&["protocol", "protokoll", "routine", "macro", "makro"])),
        ("manage_stark_protocol", "add_step" | "remove_step" | "rename" | "change") => (format!("change the protocol {}", quoted(&["protocol_name", "name", "protocol"], "you mean")), own(&["protocol", "protokoll", "routine", "macro", "makro"])),
        ("manage_stark_protocol", "capture_current_workspace" | "capture" | "save_workspace" | "snapshot" | "capture_workspace" | "save_current_setup") => (
            format!("save your windows as they stand as {}", quoted(&["protocol_name", "name", "protocol"], "a protocol")),
            own(&["protocol", "protokoll", "layout", "setup", "workspace", "desk", "arrangement", "windows", "fenster", "anordnung", "arbeitsplatz", "arbeitsbereich", "schreibtisch"]),
        ),
        ("manage_stark_protocol", "execute_protocol" | "run" | "engage" | "start" | "activate" | "execute" | "initiate") if !named(&["protocol_name", "name", "protocol"]) => {
            (format!("run the protocol {}", quoted(&["protocol_name", "name", "protocol"], "you mean")), own(&["protocol", "protokoll", "routine", "macro", "makro"]))
        }
        ("manage_voice_macro", "create" | "save" | "add" | "teach" | "update") => (format!("teach me a routine called {}", quoted(&["name", "trigger_phrase"], "something")), own(&["macro", "makro", "routine", "when i say", "whenever i say", "wenn ich sage", "teach", "shortcut", "bring dir bei"])),
        ("manage_voice_macro", "delete" | "remove" | "forget") => (format!("delete the routine {}", quoted(&["name"], "you mean")), own(&["macro", "makro", "routine"])),
        ("manage_voice_macro", "run" | "start" | "execute") if !named(&["name"]) => (format!("run the routine {}", quoted(&["name"], "you mean")), own(&["macro", "makro", "routine"])),
        ("manage_spatial_scene", "save") => (format!("save this arrangement as the scene {}", quoted(&["name"], "you mean")), own(&["scene", "szene", "layout", "workspace", "arrangement", "anordnung"])),
        ("manage_spatial_scene", "delete") => (format!("delete the scene {}", quoted(&["name"], "you mean")), own(&["scene", "szene"])),
        ("manage_spatial_scene", "load") if !named(&["name"]) => (format!("load the scene {}", quoted(&["name"], "you mean")), own(&["scene", "szene", "layout", "workspace"])),
        // ------------------------------------------------------------------ himself ----
        ("shutdown_jai", _) | ("control_jai_lifecycle", "shutdown_jai" | "shutdown" | "shut_down" | "quit" | "exit" | "close" | "power_off") => ("shut yourself down".to_string(), own(&["quit", "exit", "shut", "close", "goodbye", "good bye", "beend", "schließ", "schliess", "fahr dich", "terminate", "kill"])),
        ("control_jai_lifecycle", "restart_jai" | "restart" | "reboot") => ("restart yourself".to_string(), own(&["restart", "reboot", "relaunch", "neustart", "neu start", "starte dich neu", "start again"])),
        ("control_jai_lifecycle", "sleep_mode" | "sleep" | "standby") => ("go to sleep".to_string(), own(&["sleep", "standby", "stand by", "schlaf", "nap", "rest", "ruh dich"])),
        ("control_jai_lifecycle", "wake_up" | "wake" | "awake") => ("wake up".to_string(), own(&["wake", "awake", "wach", "aufwach", "get up", "are you there", "bist du da"])),
        ("control_jai_lifecycle", "mute_tts") => ("stop speaking aloud".to_string(), own(&["mute", "quiet", "silent", "silence", "stumm", "leise", "still", "ruhig", "speak", "talk", "voice", "sprich", "stimme", "shut up"])),
        ("control_jai_lifecycle", "unmute_tts") => ("speak aloud again".to_string(), own(&["unmute", "speak", "talk", "voice", "aloud", "loud", "sprich", "stimme", "laut", "rede"])),
        ("set_listening_mode", _) => (format!("change how you listen to {}", quoted(&["mode"], "another mode")), own(&["listen", "hands free", "handsfree", "wake word", "wakeword", "zuhör", "hör", "freisprech", "name"])),
        ("set_response_modality", _) => (format!("change how you answer to {}", quoted(&["mode"], "another way")), own(&["speak", "talk", "text", "silent", "quiet", "answer", "reply", "respond", "voice", "aloud", "mute", "write", "antwort", "sprich", "schreib", "stumm", "leise", "rede", "stimme", "say"])),
        ("control_orb_mode", _) if args.get("enabled").is_some_and(|v| !v.is_null()) => (if args["enabled"] == false { "leave orb mode" } else { "switch to orb mode" }.to_string(), own(&["orb", "kugel", "reaktor", "reactor", "emblem"])),
        // -------------------------------------------------------------- the interface ----
        ("configure_interface", _) => {
            // Every switch among the arguments needs a word of its own; the first without one
            // is what he asks about.
            let unbacked = args.as_object()?.iter().filter(|(_, value)| !value.is_null()).find_map(|(key, value)| {
                let words = interface_words(key, value)?;
                (!carries(order, &words)).then(|| (key.as_str(), value, words))
            })?;
            let (key, value, words) = unbacked;
            let on = value != &Value::Bool(false) && value != "off";
            let deed = match key {
                "layout" => format!("switch to the {} layout", value.as_str().unwrap_or("other")),
                "span_all_monitors" => if on { "stretch the board across all monitors" } else { "bring the board back to one monitor" }.to_string(),
                "fullscreen" => if on { "go full screen" } else { "leave full screen" }.to_string(),
                "mini_strip" => if on { "shrink to the mini strip" } else { "leave the mini strip" }.to_string(),
                "desktop_background" => if on { "be your desktop background" } else { "stop being your desktop background" }.to_string(),
                "background_all_monitors" => if on { "put the live background on all monitors" } else { "keep the live background to one monitor" }.to_string(),
                "reset" => "reset the interface".to_string(),
                "window" => format!("{} my window", value.as_str().unwrap_or("change")),
                "always_on_top" => if on { "stay on top of your windows" } else { "stop staying on top" }.to_string(),
                "orb_board_position" => "move the emblem".to_string(),
                other => format!("switch {} {}", other.replace('_', " "), if on { "on" } else { "off" }),
            };
            (deed, words)
        }
        // ------------------------------------------------------- what makes or sends ----
        ("create_persistent_skill" | "forge_tool" | "create_skill", _) => (format!("build a new skill called {}", quoted(&["name", "title"], "something")), own(&["skill", "tool", "forge", "learn", "teach", "ability", "fähigkeit", "werkzeug", "lerne", "bring dir bei", "routine"])),
        ("create_calendar_event", _) => (format!("put {} in your calendar", quoted(&["title"], "an entry")), own(&["calendar", "kalender", "event", "appointment", "termin", "meeting", "entry", "eintrag", "schedule", "trag", "book", "plan"])),
        ("reminders", "set" | "add" | "create") => (format!("set a reminder: {}", quoted(&["text"], "that")), own(&["remind", "reminder", "erinner", "dont let me forget", "vergiss nicht"])),
        ("set_smart_morning_alarm", "set") => ("set a morning alarm".to_string(), own(&["alarm", "wake me", "wecker", "weck"])),
        ("create_or_update_widget", _) => (
            format!("make a widget called {}", quoted(&["title", "id"], "something")),
            own(&["widget", "timer", "stopwatch", "countdown", "note", "checklist", "list", "todo", "to do", "gauge", "chart", "graph", "counter", "clock", "track", "notiz", "liste", "stoppuhr", "zähler", "uhr", "tick", "check off", "add", "füg"]),
        ),
        ("spawn_coding_project", _) => (format!("start a new coding project called {}", quoted(&["project_name"], "something")), own(&["project", "projekt", "scaffold", "repo", "app", "code", "program"])),
        ("run_shell_command", _) => (format!("run the command {}", quoted(&["command"], "you mean")), own(&["shell", "command", "terminal", "powershell", "cmd", "console", "run", "execute", "befehl", "kommando", "führe"])),
        ("create_or_overwrite_file", _) => (format!("write the file {}", quoted(&["file_path"], "you mean")), own(&["file", "datei", "write", "create", "save", "schreib", "erstell", "speicher", "script", "document", "dokument"])),
        ("edit_file_surgical", _) => (format!("change the file {}", quoted(&["file_path"], "you mean")), own(&["file", "datei", "edit", "change", "replace", "fix", "rename", "refactor", "änder", "ersetz", "line", "zeile", "code"])),
        ("patch_own_source" | "recompile_and_restart" | "restore_source_backup", _) => ("change my own code".to_string(), own(&["source", "code", "patch", "recompile", "rebuild", "yourself", "your own", "backup", "quellcode", "dich selbst"])),
        ("smart_folder_cleanup", "apply") => (format!("tidy the folder {}", quoted(&["target_folder"], "you mean")), own(&["clean", "tidy", "sort", "organi", "aufräum", "sortier", "ordne"])),
        ("manage_vram", "flush") => ("clear the graphics memory".to_string(), own(&["vram", "gpu", "graphics memory", "video memory", "grafikspeicher", "flush", "unload"])),
        ("reboot_language_model", _) => ("restart my language model".to_string(), own(&["reboot", "restart", "model", "modell", "llm", "engine", "brain", "neustart"])),
        ("focus_mode", "start") => ("start a focus session".to_string(), own(&["focus", "fokus", "pomodoro", "concentrat", "konzentr", "do not disturb", "nicht stören"])),
        ("set_auto_responder", "start") => ("answer your messages for you while you are busy".to_string(), own(&["auto", "respond", "reply", "answer", "away", "busy", "abwesen", "antwort", "beschäftigt"])),
        ("control_remote_pc", "lock" | "sleep" | "wake") => (format!("{action} the remote PC"), own(&["remote", "other pc", "second pc", "laptop", "server", "anderen pc"])),
        ("manage_remote_node", "register" | "start_service" | "stop_service" | "restart_service" | "run") => (format!("{} on the server {}", action.replace('_', " "), quoted(&["node_name"], "you mean")), own(&["server", "node", "service", "dienst", "ssh", "remote", "raspberry", "minecraft", "host"])),
        ("manage_location_settings", "set_home_here" | "set_home" | "set_school_here" | "set_school" | "geofence_on" | "geofence_off" | "set_arrival_playlist" | "set_arrival_protocol") => {
            (format!("change your location settings ({})", action.replace('_', " ")), own(&["home", "school", "location", "geofence", "arrival", "arrive", "zuhause", "schule", "standort", "ankunft", "address", "adresse"]))
        }
        ("manage_contacts", "add" | "update") => (format!("save the contact {}", quoted(&["name"], "you mean")), own(&["contact", "kontakt", "number", "nummer", "phone", "email", "address book", "adressbuch"])),
        ("manage_finances_and_goals", "add_expense" | "add_income" | "set_goal" | "add_to_goal") => (format!("note {} in your finances", action.replace('_', " ").replace("add ", "an ").replace("set ", "a ")), own(&["spent", "paid", "bought", "cost", "expense", "income", "earned", "goal", "euro", "money", "budget", "saving", "ausgegeben", "bezahlt", "gekauft", "geld", "sparziel", "einnahme", "ausgabe"])),
        ("manage_price_sniper", "add") => (format!("watch the price of {}", quoted(&["search_query"], "that")), own(&["price", "deal", "watch", "track", "sniper", "alert", "preis", "angebot", "cheaper", "billiger"])),
        ("configure_productivity_service", "enable" | "disable") => (format!("{action} the {} integration", text(&["service_name"])), own(&["enable", "disable", "connect", "disconnect", "integration", "turn on", "turn off", "switch on", "switch off", "aktivier", "deaktivier", "verbind", "trenn"])),
        // --------------------------------------------- what closes, and what deletes ----
        ("close_application", _) if !crate::focus::is_pronoun(&text(&["app_name", "name", "app"])) => {
            let app = text(&["app_name", "name", "app"]);
            // (With the program's own name in the order, the small words of closing count:
            // "mach Firefox zu", "Firefox aus".)
            let mut words = own(CLOSES);
            if names(order, &app) {
                words.extend(["zu", "aus", "off", "weg", "away", "down"]);
            }
            (format!("close {}", crate::apps::display_name(&app)), words)
        }
        ("window_management", "close" | "close_tab") => (format!("close {}", if text(&["target_window"]).is_empty() { "that window".to_string() } else { crate::apps::display_name(&text(&["target_window"])) }), own(CLOSES)),
        ("delete_memory", _) => (format!("forget {}", quoted(&["id_or_key", "key"], "that")), own(DELETES)),
        ("remove_widget", _) => (format!("remove the widget {}", quoted(&["id", "title"], "you mean")), own(DELETES)),
        ("delete_skill" | "delete_forged_tool", _) => (format!("delete the skill {}", quoted(&["name"], "you mean")), own(DELETES)),
        ("manage_contacts", "delete") => (format!("delete the contact {}", quoted(&["name"], "you mean")), own(DELETES)),
        ("modify_or_delete_calendar_event", "delete") => (format!("delete the calendar entry {}", quoted(&["title_or_uid", "title"], "you mean")), own(DELETES)),
        ("reminders", "cancel") => ("cancel that reminder".to_string(), own(DELETES)),
        _ if deletes(tool, action) => {
            // Anything else that deletes, said as plainly as its name allows: "delete memory
            // 'favourite song'", "delete 'Max Mustermann' (contacts)".
            let what = quoted(&["name", "id", "id_or_key", "title_or_uid", "title", "key", "node_name", "search_query"], "");
            let thing = tool.replace('_', " ");
            let deed = if action.is_empty() { format!("{thing} {what}") } else { format!("{} {what} ({})", action.replace('_', " "), thing.trim_start_matches("manage ").trim_start_matches("control ")) };
            (deed.split_whitespace().collect::<Vec<_>>().join(" "), own(DELETES))
        }
        _ => return None,
    })
}

/// Whether a call deletes: by its action where it has one ("delete", "cancel_rest"), else by
/// its name ("delete_memory", "remove_widget").
fn deletes(tool: &str, action: &str) -> bool {
    const WORDS: &[&str] = &["delete", "remove", "forget", "erase", "wipe", "clear", "purge", "uninstall", "cancel"];
    let named = |name: &str| name.split('_').any(|part| WORDS.contains(&part));
    if action.is_empty() { named(tool) } else { named(action) }
}

/// The question he asks before a call the language model made for an order the fast path
/// did not understand: "Did you mean: create a protocol called 'no think', Sir?". None: the
/// call runs as it always did (it only looks; the order holds a word that is the tool's own;
/// the tool is not one a wrong guess does harm with). A bare yes is asked about like any other
/// order: the brain lets it through only as the answer to a question of his.
pub fn question(order: &str, tool: &str, args: &Value) -> Option<String> {
    if crate::tools::only_looks(tool, args) {
        return None;
    }
    let said = crate::ear::words(order);
    let (deed, own) = guess(&said, tool, args)?;
    if carries(&said, &own) {
        return None;
    }
    Some(format!("Did you mean: {deed}, Sir?"))
}

// --------------------------------------------------------------- the call that waits ----

/// A call held until Sir says yes.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    pub tool: String,
    pub args: Value,
    /// What he asked.
    pub question: String,
    /// The order it was a guess at.
    pub order: String,
    until: Instant,
}

#[cfg(not(test))]
static WAITING: Mutex<Option<Held>> = Mutex::new(None);
#[cfg(test)]
thread_local! {
    /// (A test has the question it asks to itself: other tests ask the fast path about "yes"
    /// at the same time.)
    static WAITING: std::cell::RefCell<Option<Held>> = const { std::cell::RefCell::new(None) };
}

fn waiting<R>(f: impl FnOnce(&mut Option<Held>) -> R) -> R {
    #[cfg(not(test))]
    {
        f(&mut lock(&WAITING))
    }
    #[cfg(test)]
    {
        WAITING.with(|held| f(&mut held.borrow_mut()))
    }
}

/// The question is asked: the call waits for his answer, `WAITS` from when the question has
/// been said (a second for every fifteen letters of it).
pub fn hold(order: &str, tool: &str, args: &Value, question: &str) {
    let speaking = Duration::from_millis(question.chars().count() as u64 * 66);
    let held = Held { tool: tool.to_string(), args: args.clone(), question: question.to_string(), order: order.to_string(), until: Instant::now() + speaking + WAITS };
    waiting(|waits| *waits = Some(held));
}

/// What he is waiting to hear a yes to: the question and how much of its time is left (1 … 0).
/// For the HUD, which shows it with its two keys.
pub fn pending() -> Option<(String, f32)> {
    waiting(|held| match held.as_ref() {
        Some(h) if h.until > Instant::now() => Some((h.question.clone(), ((h.until - Instant::now()).as_secs_f32() / WAITS.as_secs_f32()).clamp(0.0, 1.0))),
        Some(_) => {
            *held = None;
            None
        }
        None => None,
    })
}

/// The question is withdrawn (his `[ NO ]`, or his next order came instead of an answer).
pub fn drop_it() {
    waiting(|held| *held = None);
}

/// His answer to the question.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// Yes: the call to carry out now.
    Yes(Held),
    /// No: it is dropped, and he says so.
    No,
}

/// What a sentence answers while a call waits. A plain yes ("yes", "do it", "ja", "mach das")
/// carries it out, a plain no drops it. Anything else drops it too and is no answer (None):
/// it is his next order. None as well when nothing waits, or it waited too long.
pub fn answer(text: &str) -> Option<Answer> {
    let held = waiting(|held| held.take())?;
    if held.until <= Instant::now() {
        return None;
    }
    if crate::intents::is_affirmation(text) {
        Some(Answer::Yes(held))
    } else if crate::intents::is_negation(text) {
        Some(Answer::No)
    } else {
        None
    }
}

/// Whether a sentence would answer a call that waits (without taking it): the fast path's
/// mirror asks, and so does whoever decides whom a "yes" in the room is for.
pub fn would_answer(text: &str) -> bool {
    waiting(|held| held.as_ref().is_some_and(|h| h.until > Instant::now())) && (crate::intents::is_affirmation(text) || crate::intents::is_negation(text))
}

/// A tool's name for as long as the program runs (the fast path's orders carry such names).
pub fn lasting_name(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = lock(&NAMES);
    match names.iter().find(|known| **known == name) {
        Some(known) => *known,
        None => {
            // (As many as there are tools he can be asked about: a few dozen names at most.)
            let kept: &'static str = Box::leak(name.to_string().into_boxed_str());
            names.push(kept);
            kept
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The five calls of 2026-11-13, each with the sentence it was the model's answer to.
    #[test]
    fn the_guesses_of_the_thirteenth_end_in_a_question() {
        for (order, tool, args, asks) in [
            ("Jay, save that desk layout as standard.", "manage_stark_protocol", json!({"action": "create", "protocol_name": "no_think", "steps": []}), "Did you mean: create a protocol called 'no think', Sir?"),
            ("Jay Layout Standard.", "manage_stark_protocol", json!({"action": "create", "protocol_name": "jay_layout_standard"}), "Did you mean: create a protocol called 'jay layout standard', Sir?"),
            ("The Jay layout standard.", "configure_interface", json!({"layout": "orb"}), "Did you mean: switch to the orb layout, Sir?"),
            ("Jay opened your pronunciation code.", "control_jai_lifecycle", json!({"action": "wake_up"}), "Did you mean: wake up, Sir?"),
            ("They open your pronunciation coach.", "control_jai_lifecycle", json!({"action": "wake_up"}), "Did you mean: wake up, Sir?"),
            ("Throw it in all monitors.", "configure_interface", json!({"span_all_monitors": true}), "Did you mean: stretch the board across all monitors, Sir?"),
        ] {
            assert_eq!(question(order, tool, &args).as_deref(), Some(asks), "{order}");
        }
    }

    /// 2026-11-16, Sir's console: "Yes, sir." after "There are no secondary windows open"
    /// closed Firefox. A yes to nothing he asked is asked about too (the brain lets a yes
    /// through only after a question of his).
    #[test]
    fn a_bare_yes_is_asked_about_before_it_closes_a_program() {
        let asked = question("Yes, sir.", "close_application", &json!({"app_name": "Firefox"})).expect("asked");
        assert!(asked.starts_with("Did you mean: ") && asked.to_lowercase().contains("firefox"), "{asked}");
        assert_eq!(question("Yes, sir.", "web_search", &json!({"query": "weather"})), None, "a look needs no question");
    }

    /// An order that holds the tool's own words is carried out as it always was.
    #[test]
    fn an_order_in_the_tools_own_words_is_not_asked_about() {
        for (order, tool, args) in [
            ("create a protocol called movie night that opens Netflix and dims the screens", "manage_stark_protocol", json!({"action": "create", "protocol_name": "movie_night"})),
            ("delete the gaming protocol", "manage_stark_protocol", json!({"action": "delete", "protocol_name": "gaming"})),
            ("lösch das Gaming-Protokoll", "manage_stark_protocol", json!({"action": "delete", "protocol_name": "gaming"})),
            ("save my current setup as the gaming protocol", "manage_stark_protocol", json!({"action": "capture_current_workspace", "protocol_name": "gaming"})),
            ("save that desk layout as standard", "manage_stark_protocol", json!({"action": "capture_current_workspace", "protocol_name": "standard"})),
            ("it's movie night", "manage_stark_protocol", json!({"action": "execute_protocol", "protocol_name": "movie_night"})),
            ("initiate the house party protocol", "manage_stark_protocol", json!({"action": "execute_protocol", "protocol_name": "house party"})),
            ("when I say study time, open my physics folder and play lo-fi", "manage_voice_macro", json!({"action": "create", "name": "study time", "steps": ["open my physics folder"]})),
            ("switch to the compact layout", "configure_interface", json!({"layout": "compact"})),
            ("make yourself small, just the chat", "configure_interface", json!({"layout": "compact"})),
            ("nur noch die Kugel bitte", "configure_interface", json!({"layout": "orb"})),
            ("span across all my monitors", "configure_interface", json!({"span_all_monitors": true})),
            ("erstreck dich über alle Bildschirme", "configure_interface", json!({"span_all_monitors": true})),
            ("go back to one monitor", "configure_interface", json!({"span_all_monitors": false})),
            ("go full screen", "configure_interface", json!({"fullscreen": true})),
            ("be my desktop background", "configure_interface", json!({"desktop_background": true})),
            ("werde mein Desktophintergrund", "configure_interface", json!({"desktop_background": true})),
            ("make the interface red", "configure_interface", json!({"accent_color": "red"})),
            ("go to sleep", "control_jai_lifecycle", json!({"action": "sleep_mode"})),
            ("wake up, daddy's home", "control_jai_lifecycle", json!({"action": "wake_up"})),
            ("restart yourself", "control_jai_lifecycle", json!({"action": "restart_jai"})),
            ("please be quiet for a while", "control_jai_lifecycle", json!({"action": "mute_tts"})),
            ("schlaf jetzt", "control_jai_lifecycle", json!({"action": "sleep_mode"})),
            ("forget that I like jazz", "delete_memory", json!({"id_or_key": "music_taste"})),
            ("remove the shopping list", "remove_widget", json!({"id": "shopping_list"})),
            ("delete my dentist appointment", "modify_or_delete_calendar_event", json!({"action": "delete", "title_or_uid": "dentist"})),
            ("trag Zahnarzt am Freitag um 9 in den Kalender ein", "create_calendar_event", json!({"title": "Zahnarzt", "date": "Friday", "start_time": "09:00"})),
            ("remind me to call the dentist tomorrow", "reminders", json!({"action": "set", "text": "call the dentist"})),
            ("close Firefox", "close_application", json!({"app_name": "firefox"})),
            ("mach Firefox zu", "close_application", json!({"app_name": "firefox"})),
            ("make me a checklist for the trip", "create_or_update_widget", json!({"type": "Checklist", "title": "Trip"})),
            // What only looks is never held back, whatever the order's words are.
            ("anything going on?", "get_calendar_schedule", json!({"date_or_range": "today"})),
            ("what about my protocols", "manage_stark_protocol", json!({"action": "list"})),
            ("is the timer done", "timer_left", json!({})),
            ("where does that leave us", "recall_memories", json!({"query": "plans"})),
            // A tool that is not one of these runs as it always did.
            ("something nice please", "control_music", json!({"action": "play", "playlist_name": "chill"})),
            ("tell Max I'm late", "send_whatsapp_message", json!({"recipient_name_or_phone": "Max", "message_text": "I'm late"})),
            ("louder", "media_and_volume_control", json!({"action": "volume_up"})),
            // (His yes to a question he was just asked never reaches here: brain's
            // `yes_to_his_question`. A yes to nothing is asked about, see below.)
        ] {
            assert_eq!(question(order, tool, &args), None, "{order} → {tool} {args}");
        }
    }

    /// A call without a word of its own in the order is asked about, whichever tool it is.
    #[test]
    fn a_guess_is_asked_about_in_plain_words() {
        for (order, tool, args, asks) in [
            ("that was a long day", "control_jai_lifecycle", json!({"action": "sleep_mode"}), "Did you mean: go to sleep, Sir?"),
            ("enough for today", "shutdown_jai", json!({}), "Did you mean: shut yourself down, Sir?"),
            ("I hate this song", "delete_memory", json!({"id_or_key": "favourite_song"}), "Did you mean: forget 'favourite song', Sir?"),
            ("what a mess", "smart_folder_cleanup", json!({"action": "apply", "target_folder": "Downloads"}), "Did you mean: tidy the folder 'Downloads', Sir?"),
            ("I'm done with the browser", "close_application", json!({"app_name": "firefox"}), "Did you mean: close Firefox, Sir?"),
            ("dentist on Friday at nine", "manage_voice_macro", json!({"action": "create", "name": "dentist"}), "Did you mean: teach me a routine called 'dentist', Sir?"),
            ("make it look nicer", "configure_interface", json!({"layout": "compact", "accent_color": "blue"}), "Did you mean: switch to the compact layout, Sir?"),
            ("put it on the big one", "configure_interface", json!({"fullscreen": true}), "Did you mean: go full screen, Sir?"),
            ("the gaming thing", "manage_stark_protocol", json!({"action": "delete", "protocol_name": "gaming"}), "Did you mean: delete the protocol 'gaming', Sir?"),
            ("I only want to hear you when it matters", "set_response_modality", json!({"mode": "text_only"}), "Did you mean: change how you answer to 'text only', Sir?"),
            ("dieses Chaos hier", "manage_contacts", json!({"action": "delete", "name": "Max Mustermann"}), "Did you mean: delete the contact 'Max Mustermann', Sir?"),
            ("das war nichts", "modify_or_delete_calendar_event", json!({"action": "delete", "title_or_uid": "Zahnarzt"}), "Did you mean: delete the calendar entry 'Zahnarzt', Sir?"),
            ("that one is old", "manage_price_sniper", json!({"action": "remove", "search_query": "graphics card"}), "Did you mean: remove 'graphics card' (price sniper), Sir?"),
        ] {
            assert_eq!(question(order, tool, &args).as_deref(), Some(asks), "{order}");
        }
    }

    #[test]
    fn yes_carries_it_out_and_no_silence_or_anything_else_drops_it() {
        let args = json!({"action": "create", "protocol_name": "no_think"});
        let ask = "Did you mean: create a protocol called 'no think', Sir?";
        assert_eq!(answer("yes"), None, "nothing waits");
        // Yes, in his words.
        for yes in ["Yes.", "yes please", "do it", "Ja", "mach das", "go ahead"] {
            hold("save that desk layout as standard", "manage_stark_protocol", &args, ask);
            assert!(would_answer(yes), "{yes}");
            assert!(matches!(answer(yes), Some(Answer::Yes(held)) if held.tool == "manage_stark_protocol" && held.args == args), "{yes}");
            assert_eq!(answer(yes), None, "{yes}: carried out once");
        }
        // No drops it.
        for no in ["No.", "nope", "nein", "no thanks"] {
            hold("save that desk layout as standard", "manage_stark_protocol", &args, ask);
            assert_eq!(answer(no), Some(Answer::No), "{no}");
            assert_eq!(pending(), None);
        }
        // Anything else drops it and is his next order: a later yes finds nothing.
        hold("save that desk layout as standard", "manage_stark_protocol", &args, ask);
        assert!(pending().is_some_and(|(question, left)| question == ask && left > 0.9));
        assert!(!would_answer("open the mixer"));
        assert_eq!(answer("open the mixer"), None);
        assert_eq!(answer("yes"), None, "what it would have been a yes to is gone");
        // Silence: it lapses.
        hold("save that desk layout as standard", "manage_stark_protocol", &args, ask);
        waiting(|held| held.as_mut().unwrap().until = Instant::now() - Duration::from_millis(1));
        assert!(!would_answer("yes") && pending().is_none());
        assert_eq!(answer("yes"), None);
        assert_eq!(lasting_name("manage_stark_protocol"), "manage_stark_protocol");
        assert!(std::ptr::eq(lasting_name("control_jai_lifecycle"), lasting_name("control_jai_lifecycle")), "one copy of a name is kept");
    }
}
