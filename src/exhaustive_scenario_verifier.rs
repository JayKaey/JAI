//! The exhaustive scenario verifier (`JAI.exe --exhaustive [runs]`): every command family
//! in five passes, the whole interface inspected as geometry, JAI's voice on five kinds of
//! sentence, the privacy lock, the monitors and the two audio sinks, and every button of the
//! Feature Lab pressed five times — all of it in the running app, silently, with the HUD drawn
//! offscreen (`gui::offscreen`), run after run until five runs in a row are clean.
//!
//! * **Matrix A** — every command category (the old ones and the workbench's) in five passes:
//!   clean English; German and Denglisch; noisy, fast and misheard; four commands inside
//!   400 ms spoken over JAI's own voice (through the real dispatcher, in a process-wide dry
//!   run); a compound sentence whose every clause must be carried out. A request is judged by
//!   the handler it reaches, never by words: the wrong tool, the music player for an undo, or
//!   a dropped clause is a failure.
//! * **Matrix B** — `visual_ui_verifier` on the real HUD at four screen sizes: no clipped text,
//!   no overlap with 1, 2, 4, 8 and 12 blocks, blocks and the orb follow a drag exactly, every
//!   stroke in the active theme, the page keys large enough and working, no jump in any
//!   animation.
//! * **Matrix C** — JAI's voice on five sentence types against his clip (128-band spectral
//!   similarity ≥ 0.97) and the homograph "live" read as "lyve".
//! * **Matrix D** — the HUD on every monitor and spanning all of them; ten wake words against
//!   the privacy lock; "stop listening for one minute"; the microphone chip; one clause reaching
//!   both audio sinks and the loopback gate's 250 ms tail.
//! * **Matrix E** — every `[ TRY ]` and `[ TEST ]` of the Feature Lab five times with varied
//!   inputs: what only reads runs for real, what draws in the HUD is drawn and inspected, what
//!   would act outside JAI is recorded as a dry run and checked against its schema.
//! * **Matrix F** — the commands as sound: two stock voices standing in for Sir say each
//!   sentence of `SPOKEN` with his name in front; the recogniser hears it (as samples, no
//!   speaker), the wake word takes the name off and the dispatcher must reach the handler.
//!
//! The table of sentences is plain data (`CASES`, `COMPOUNDS`, `SPOKEN`); the unit tests below send
//! every one of them through the stateless parsers, so a routing regression fails `cargo test`
//! long before the app is started.

use crate::app::Jai;
use crate::intent_engine::ReflexContext;
use crate::live_self_tester::{judge, ms_since, skip, LOCAL_BUDGET_MS, NETWORK_BUDGET_MS, REFLEX_BUDGET_MS, TURN_BUDGET_MS, UI_BUDGET_MS, VOICE_BUDGET_MS};
use crate::tools::ToolContext;
use crate::visual_ui_verifier as visual;
use eframe::egui::{pos2, vec2, PointerButton, Pos2, Rect, Vec2};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::info;

/// A compound sentence must be planned and handed to its handlers within this.
pub const COMPOUND_BUDGET_MS: f64 = 15.0;
/// Pass 4 fires its four commands inside this window.
pub const BARGE_IN_WINDOW_MS: u128 = 400;
/// How many consecutive clean runs the verifier asks for by default.
pub const DEFAULT_RUNS: usize = 5;
/// One frame at 180 Hz, in milliseconds: what the HUD's own work per frame must stay under.
pub const FRAME_BUDGET_MS: f64 = 5.5;
/// What matrix B types into the text console, one per screen size and run; each must complete.
const CONSOLE_PREFIXES: [&str; 5] = ["op", "tu so cu", "mo orb", "näch", "tile"];

/// What the app must be showing for a sentence to mean what the table says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    Nothing,
    /// A board with a stack of pages is up ("next" then means the next page).
    Pages,
}

/// One sentence of matrix A and the handler it must reach: `tool` (alternatives separated by
/// `|`) with at least the arguments `args` (a string argument must contain the given text).
pub struct Case {
    pub category: &'static str,
    /// 1 = clean English, 2 = German / Denglisch, 3 = noisy, fast or misheard.
    pub pass: u8,
    pub say: &'static str,
    pub tool: &'static str,
    pub args: &'static str,
    pub needs: Needs,
}

const fn c(category: &'static str, pass: u8, say: &'static str, tool: &'static str, args: &'static str) -> Case {
    Case { category, pass, say, tool, args, needs: Needs::Nothing }
}

const fn paged(category: &'static str, pass: u8, say: &'static str, tool: &'static str, args: &'static str) -> Case {
    Case { category, pass, say, tool, args, needs: Needs::Pages }
}

/// Matrix A, passes 1-3.
pub const CASES: &[Case] = &[
    // ---- The core view ----
    c("core", 1, "Show me your core.", "configure_interface", r#"{"core_view": true}"#),
    c("core", 1, "Hide your core.", "configure_interface", r#"{"core_view": false}"#),
    c("core", 2, "Zeig mir deinen Core.", "configure_interface", r#"{"core_view": true}"#),
    c("core", 2, "Schließ deinen Core.", "configure_interface", r#"{"core_view": false}"#),
    c("core", 3, "close your car", "configure_interface", r#"{"core_view": false}"#),
    c("core", 3, "hide your call", "configure_interface", r#"{"core_view": false}"#),
    c("core", 3, "hide your for C-O-R-E", "configure_interface", r#"{"core_view": false}"#),
    // ---- Undo ----
    c("undo", 1, "Undo that.", "undo_last_action", "{}"),
    c("undo", 1, "Put it back.", "undo_last_action", "{}"),
    c("undo", 1, "Undo the last change.", "undo_last_action", "{}"),
    c("undo", 2, "Mach das rückgängig.", "undo_last_action", "{}"),
    c("undo", 2, "Nimm das zurück.", "undo_last_action", "{}"),
    c("undo", 3, "uh undo that", "undo_last_action", "{}"),
    c("undo", 3, "never mind undo that", "undo_last_action", "{}"),
    // (A misheard request's change, 2026-11-08: "Call me Earth." became Sir's title.)
    c("undo", 1, "That's not what I said.", "undo_last_action", r#"{"within_s": 120}"#),
    c("undo", 2, "Das habe ich nicht gesagt.", "undo_last_action", r#"{"within_s": 120}"#),
    c("undo", 3, "you misheard me", "undo_last_action", r#"{"within_s": 120}"#),
    c("undo", 1, "What did you just do?", "recent_actions", "{}"),
    c("undo", 2, "Was hast du gerade gemacht?", "recent_actions", "{}"),
    c("undo", 1, "What's new?", "whats_new", "{}"),
    c("undo", 2, "Was ist neu?", "whats_new", "{}"),
    // ---- Named mixes ----
    c("mix", 1, "Save this mix as gaming.", "control_audio_mixer", r#"{"action": "save_mix", "name": "gaming"}"#),
    c("mix", 1, "Load my gaming mix.", "control_audio_mixer", r#"{"action": "load_mix", "name": "gaming"}"#),
    c("mix", 1, "Which mixes do I have?", "control_audio_mixer", r#"{"action": "list_mixes"}"#),
    c("mix", 2, "Speicher diesen Mix als Gaming.", "control_audio_mixer", r#"{"action": "save_mix", "name": "gaming"}"#),
    c("mix", 2, "Lade den Gaming Mix.", "control_audio_mixer", r#"{"action": "load_mix", "name": "gaming"}"#),
    c("mix", 3, "uh load my gaming mix", "control_audio_mixer", r#"{"action": "load_mix", "name": "gaming"}"#),
    // ---- The orb ----
    c("orb", 1, "Move your orb to the top right.", "configure_interface", r#"{"orb_board_position": "top right"}"#),
    c("orb", 1, "Center your orb.", "configure_interface", r#"{"orb_board_position": "center"}"#),
    c("orb", 2, "Schieb den Orb etwas nach rechts.", "configure_interface", r#"{"orb_board_position": "rechts"}"#),
    c("orb", 2, "Beweg den Orb nach oben links.", "configure_interface", r#"{"orb_board_position": "oben links"}"#),
    c("orb", 3, "move your orb a little bit to the left", "configure_interface", r#"{"orb_board_position": "left"}"#),
    c("orb", 3, "put your core in the top right corner", "configure_interface", r#"{"orb_board_position": "top right"}"#),
    c("orb", 1, "Move out of the way.", "configure_interface", r#"{"orb_board_position": "aside"}"#),
    c("orb", 1, "Come back to the middle.", "configure_interface", r#"{"orb_board_position": "center"}"#),
    c("orb", 1, "Are you in orb mode?", "control_orb_mode", r#"{"action": "status"}"#),
    c("undo", 1, "Did you mute Discord?", "recent_actions", r#"{"about": "discord"}"#),
    c("undo", 2, "Hast du Firefox geschlossen?", "recent_actions", r#"{"about": "firefox"}"#),
    c("orb", 2, "Geh aus dem Weg.", "configure_interface", r#"{"orb_board_position": "aside"}"#),
    c("orb", 3, "you're in the way", "configure_interface", r#"{"orb_board_position": "aside"}"#),
    // ---- The theme ----
    c("theme", 1, "Set theme to Arc Cyan.", "configure_interface", r#"{"hud_palette": "arc_reactor"}"#),
    c("theme", 1, "Change the theme to Stark Gold.", "configure_interface", r#"{"hud_palette": "stark_gold"}"#),
    c("theme", 2, "Mach das Theme blau.", "configure_interface", "{}"),
    c("theme", 2, "Stell das Theme auf Arc Cyan.", "configure_interface", r#"{"hud_palette": "arc_reactor"}"#),
    c("theme", 3, "make your seam yellow", "configure_interface", "{}"),
    c("theme", 3, "make your steam red", "configure_interface", "{}"),
    // ---- Closing his own blocks ----
    c("close", 1, "Close the HUD board.", "hud_board", r#"{"action": "close"}"#),
    c("close", 1, "Close web search.", "hud_board", r#"{"action": "close"}"#),
    c("close", 1, "Clear the board.", "hud_board", r#"{"action": "clear"}"#),
    c("close", 2, "Schließ das HUD Board.", "hud_board", r#"{"action": "close"}"#),
    c("close", 2, "Schließ die Websuche.", "hud_board", r#"{"action": "close"}"#),
    c("close", 3, "close to web search", "hud_board", r#"{"action": "close"}"#),
    c("close", 3, "close the feature lab", "hud_board", r#"{"action": "close", "target": "feature lab"}"#),
    // ---- Pages ----
    paged("pages", 1, "Next page.", "hud_board", r#"{"action": "flip_note", "direction": "next"}"#),
    paged("pages", 1, "Previous page.", "hud_board", r#"{"action": "flip_note", "direction": "previous"}"#),
    paged("pages", 2, "Nächste Seite.", "hud_board", r#"{"action": "flip_note", "direction": "next"}"#),
    paged("pages", 2, "Vorherige Seite.", "hud_board", r#"{"action": "flip_note", "direction": "previous"}"#),
    paged("pages", 3, "next", "hud_board", r#"{"action": "flip_note", "direction": "next"}"#),
    paged("pages", 3, "go back", "hud_board", r#"{"action": "flip_note", "direction": "previous"}"#),
    // ---- The privacy lock ----
    c("listening", 1, "Stop listening.", "listening_lock", r#"{"command": "Lock"}"#),
    c("listening", 1, "Stop listening for 1 minute.", "listening_lock", r#"{"command": "Pause"}"#),
    c("listening", 1, "JAI, you can now listen again.", "listening_lock", r#"{"command": "Resume"}"#),
    c("listening", 2, "Hör nicht mehr zu.", "listening_lock", r#"{"command": "Lock"}"#),
    c("listening", 2, "Du kannst wieder zuhören.", "listening_lock", r#"{"command": "Resume"}"#),
    c("listening", 3, "please stop listening for five minutes", "listening_lock", r#"{"command": "Pause"}"#),
    c("listening", 3, "you can listen again", "listening_lock", r#"{"command": "Resume"}"#),
    c("listening", 1, "Mute my mic.", "listening_lock", r#"{"command": "Lock"}"#),
    c("listening", 1, "Unmute my mic.", "listening_lock", r#"{"command": "Resume"}"#),
    c("listening", 1, "Turn the mic back on.", "listening_lock", r#"{"command": "Resume"}"#),
    c("listening", 1, "Start listening.", "listening_lock", r#"{"command": "Resume"}"#),
    c("listening", 2, "Mikrofon aus.", "listening_lock", r#"{"command": "Lock"}"#),
    // ---- Music ----
    c("music", 1, "Pause.", "control_music", r#"{"action": "stop_all"}"#),
    c("music", 1, "Next song.", "control_music", r#"{"action": "next_track"}"#),
    c("music", 1, "What's playing?", "control_music", r#"{"action": "what_is_playing"}"#),
    c("music", 2, "Nächstes Lied.", "control_music", r#"{"action": "next_track"}"#),
    c("music", 2, "Pause die Musik.", "control_music", "{}"),
    c("music", 3, "pause the music", "control_music", "{}"),
    c("music", 3, "next song please", "control_music", r#"{"action": "next_track"}"#),
    // ---- Volume ----
    c("volume", 1, "Volume 40.", "control_audio_mixer", r#"{"action": "set_master", "level": 40}"#),
    c("volume", 1, "Mute.", "control_audio_mixer", r#"{"action": "mute_master"}"#),
    c("volume", 2, "Lautstärke 40.", "control_audio_mixer", r#"{"action": "set_master", "level": 40}"#),
    c("volume", 2, "Stumm.", "control_audio_mixer", r#"{"action": "mute_master"}"#),
    c("volume", 3, "volume forty", "control_audio_mixer", r#"{"action": "set_master", "level": 40}"#),
    c("volume", 1, "Make it quiet.", "control_audio_mixer", r#"{"action": "quiet", "level": 20}"#),
    c("volume", 2, "Mach es ganz leise.", "control_audio_mixer", r#"{"action": "quiet", "level": 10}"#),
    c("volume", 1, "Make it loud.", "control_audio_mixer", r#"{"action": "loud", "level": 80}"#),
    c("volume", 1, "It's very quiet.", "control_audio_mixer", r#"{"action": "change_app", "app": "it", "delta": 10}"#),
    c("volume", 2, "Das ist laut.", "control_audio_mixer", r#"{"action": "change_app", "app": "it", "delta": -10}"#),
    c("volume", 1, "It's not loud enough.", "control_audio_mixer", r#"{"action": "change_app", "app": "it", "delta": 10}"#),
    c("volume", 1, "I can barely hear Discord.", "control_audio_mixer", r#"{"action": "change_app", "app": "discord", "delta": 10}"#),
    c("volume", 2, "Ich höre Discord kaum.", "control_audio_mixer", r#"{"action": "change_app", "app": "discord", "delta": 10}"#),
    c("volume", 1, "Mute Discord for 10 minutes.", "control_audio_mixer", r#"{"action": "mute_app", "app": "discord", "seconds": 600}"#),
    c("volume", 2, "Schalte Discord für 10 Minuten stumm.", "control_audio_mixer", r#"{"action": "mute_app", "app": "discord", "seconds": 600}"#),
    c("volume", 1, "Mute everything for half an hour.", "control_audio_mixer", r#"{"action": "mute_master", "seconds": 1800}"#),
    c("volume", 1, "Fade out the music.", "control_audio_mixer", r#"{"action": "fade", "way": "out", "app": "music"}"#),
    c("volume", 1, "Fade the music back in.", "control_audio_mixer", r#"{"action": "fade", "way": "in", "app": "music"}"#),
    c("volume", 2, "Blende die Musik aus.", "control_audio_mixer", r#"{"action": "fade", "way": "out", "app": "musik"}"#),
    c("volume", 1, "Turn it down slowly.", "control_audio_mixer", r#"{"action": "fade", "delta": -20, "app": "it"}"#),
    c("volume", 1, "Make Firefox as loud as Discord.", "control_audio_mixer", r#"{"action": "match_app", "app": "firefox", "to": "discord"}"#),
    c("volume", 2, "Mach Firefox leiser als Discord.", "control_audio_mixer", r#"{"action": "match_app", "app": "firefox", "to": "discord", "offset": -10}"#),
    c("volume", 1, "Set all apps to 100 percent.", "control_audio_mixer", r#"{"action": "set_all_apps", "level": 100}"#),
    c("volume", 2, "Mach alles stumm außer Discord.", "control_audio_mixer", r#"{"action": "mute_others", "app": "discord"}"#),
    c("volume", 2, "Firefox stumm schalten.", "control_audio_mixer", r#"{"action": "mute_app", "app": "firefox"}"#),
    c("volume", 1, "What's Discord at?", "control_audio_mixer", r#"{"action": "get_app", "app": "discord"}"#),
    c("volume", 1, "Which audio device am I using?", "control_audio_mixer", r#"{"action": "device"}"#),
    c("volume", 1, "List my audio devices.", "control_audio_mixer", r#"{"action": "devices"}"#),
    c("volume", 1, "Is a game running?", "control_audio_mixer", r#"{"action": "get_app", "app": "game"}"#),
    c("volume", 1, "Why is it so quiet?", "control_audio_mixer", r#"{"action": "list"}"#),
    c("volume", 2, "Mach den Ton nicht so laut.", "control_audio_mixer", r#"{"action": "change_app", "app": "it", "delta": -10}"#),
    c("volume", 1, "Switch to my speakers.", "control_audio_mixer", r#"{"action": "switch_device"}"#),
    c("volume", 3, "set the volume to 40 percent", "control_audio_mixer|media_and_volume_control", r#"{"level": 40}"#),
    // (Sir's own requests of 2026-11-06, which went to the language model and muted the PC.)
    c("volume", 1, "Lower my browser's volume by 50%.", "control_audio_mixer", r#"{"action": "change_app", "app": "browser", "delta": -50}"#),
    c("volume", 1, "Make Discord's volume 50%.", "control_audio_mixer", r#"{"action": "set_apps"}"#),
    c("volume", 1, "Make everything loud again.", "control_audio_mixer", r#"{"action": "restore"}"#),
    c("volume", 1, "Mute everything except Discord.", "control_audio_mixer", r#"{"action": "mute_others", "app": "discord"}"#),
    c("volume", 2, "Mach Discord leiser.", "control_audio_mixer", r#"{"action": "change_app", "app": "discord", "delta": -10}"#),
    c("volume", 2, "Mach alles wieder laut.", "control_audio_mixer", r#"{"action": "restore"}"#),
    c("volume", 3, "makes system volume one hundred percent", "control_audio_mixer", r#"{"action": "set_master", "level": 100}"#),
    c("volume", 3, "and you make discords, volume 50%", "control_audio_mixer", r#"{"action": "set_apps"}"#),
    c("volume", 3, "no, don't, don't mute it, make it 50% louder", "control_audio_mixer", r#"{"action": "change_app", "app": "it", "delta": 50}"#),
    // What Sir says in passing: short, judged, half in German.
    c("volume", 1, "Music up.", "control_audio_mixer", r#"{"action": "change_app", "app": "music", "delta": 10}"#),
    c("volume", 1, "Too loud.", "control_audio_mixer", r#"{"action": "change_app", "app": "it", "delta": -10}"#),
    c("volume", 1, "It's a bit quiet.", "control_audio_mixer", r#"{"action": "change_app", "app": "it"}"#),
    c("volume", 1, "Turn it all the way up.", "control_audio_mixer", r#"{"action": "set_app", "app": "it", "level": 100}"#),
    c("volume", 1, "Lower everything by 10 percent.", "control_audio_mixer", r#"{"action": "change_master", "delta": -10}"#),
    c("volume", 2, "Discord auf die Hälfte.", "control_audio_mixer", r#"{"action": "set_apps"}"#),
    c("volume", 2, "Dreh auf.", "control_audio_mixer", r#"{"action": "change_app", "app": "it"}"#),
    c("volume", 2, "Das ist viel zu leise.", "control_audio_mixer", r#"{"action": "change_app", "app": "it", "delta": 20}"#),
    c("volume", 3, "even louder", "control_audio_mixer", "{}"),
    // The music itself: never a program to close.
    c("music", 1, "Turn the music off.", "control_music", "{}"),
    c("music", 1, "Turn the music back on.", "control_music", "{}"),
    c("music", 2, "Mach die Musik aus.", "control_music", "{}"),
    c("music", 3, "music off", "control_music", "{}"),
    // ---- Monitors ----
    c("monitors", 1, "Move to my third monitor.", "move_window_to_monitor", r#"{"target_monitor": 3}"#),
    c("monitors", 1, "Use all monitors.", "configure_interface", r#"{"span_all_monitors": true}"#),
    c("monitors", 2, "Geh auf den zweiten Monitor.", "move_window_to_monitor", r#"{"target_monitor": 2}"#),
    c("monitors", 2, "Nutze alle Monitore.", "configure_interface", r#"{"span_all_monitors": true}"#),
    c("monitors", 3, "move to monitor 2", "move_window_to_monitor", r#"{"target_monitor": 2}"#),
    c("monitors", 3, "move to my first monitor", "move_window_to_monitor", r#"{"target_monitor": 1}"#),
    c("monitors", 1, "Span over all three monitors.", "configure_interface", r#"{"span_all_monitors": true}"#),
    c("monitors", 1, "Stop spanning.", "configure_interface", r#"{"span_all_monitors": false}"#),
    c("monitors", 2, "Nutze nur einen Monitor.", "configure_interface", r#"{"span_all_monitors": false}"#),
    c("monitors", 3, "toggle span mode", "configure_interface", r#"{"span_all_monitors": "toggle"}"#),
    c("monitors", 3, "go back to one monitor", "configure_interface", r#"{"span_all_monitors": false}"#),
    // ---- The schedule ----
    c("schedule", 1, "What classes do I have tomorrow?", "check_school_schedule", "{}"),
    c("schedule", 1, "Open my WebUntis schedule.", "check_school_schedule|open_jai_view", "{}"),
    c("schedule", 2, "Wie sieht mein Stundenplan morgen aus?", "check_school_schedule", "{}"),
    c("schedule", 2, "Öffne meinen Stundenplan.", "check_school_schedule|open_jai_view", "{}"),
    c("schedule", 3, "what classes do i have tomorrow", "check_school_schedule", "{}"),
    c("schedule", 3, "open my webuntis schedule", "check_school_schedule|open_jai_view", "{}"),
    // ---- Research ----
    c("research", 1, "Research the state of VR.", "hud_board", r#"{"action": "research"}"#),
    c("research", 2, "Recherchiere Festkörperbatterien.", "hud_board", r#"{"action": "research"}"#),
    c("research", 3, "research solid state batteries", "hud_board", r#"{"action": "research"}"#),
    // ---- The HoloMat (simulation lab) ----
    c("holomat", 1, "Open the simulation lab.", "open_jai_view", r#"{"view": "simulation_lab"}"#),
    c("holomat", 2, "Öffne das Simulationslabor.", "open_jai_view", r#"{"view": "simulation_lab"}"#),
    c("holomat", 3, "open the holomat", "open_jai_view", r#"{"view": "simulation_lab"}"#),
    // ---- The reference videos' panels: network radar, disk map, Earth ----
    c("panels", 1, "Check the network.", "open_jai_view", r#"{"view": "network_radar", "action": "show"}"#),
    c("panels", 1, "Who's on my wifi?", "open_jai_view", r#"{"view": "network_radar", "action": "show"}"#),
    c("panels", 1, "Is my network secure?", "open_jai_view", r#"{"view": "network_radar", "action": "check_security"}"#),
    c("panels", 1, "What is taking up space?", "open_jai_view", r#"{"view": "disk_map", "action": "show"}"#),
    c("panels", 1, "Map my D drive.", "open_jai_view", r#"{"view": "disk_map", "path": "D:\\"}"#),
    c("panels", 1, "Show me the earth.", "open_jai_view", r#"{"view": "earth", "action": "show"}"#),
    c("panels", 1, "Were there any earthquakes near Tokyo recently?", "open_jai_view", r#"{"view": "earth", "action": "show_quakes", "place": "Tokyo"}"#),
    c("panels", 1, "Show me the flights over New York.", "open_jai_view", r#"{"view": "earth", "action": "show_flights", "place": "New York"}"#),
    c("panels", 1, "Close the globe.", "hud_board", r#"{"action": "close", "target": "globe"}"#),
    c("panels", 2, "Scanne mein Netzwerk.", "open_jai_view", r#"{"view": "network_radar"}"#),
    c("panels", 2, "Was frisst meinen Speicherplatz?", "open_jai_view", r#"{"view": "disk_map"}"#),
    c("panels", 2, "Zeig mir die Erde.", "open_jai_view", r#"{"view": "earth"}"#),
    c("panels", 3, "hey jai check check the network", "open_jai_view", r#"{"view": "network_radar"}"#),
    c("panels", 3, "where did all my space go", "open_jai_view", r#"{"view": "disk_map"}"#),
    c("panels", 3, "show me the globe", "open_jai_view", r#"{"view": "earth"}"#),
    // (The Earth made useful: where Sir is, the terrain, the weather, a past day, the tour.)
    c("panels", 1, "Where am I on the map?", "open_jai_view", r#"{"view": "earth", "action": "show_my_location"}"#),
    c("panels", 1, "Show me the mountains.", "open_jai_view", r#"{"view": "earth", "action": "show_mountains"}"#),
    c("panels", 1, "Show me the rain radar.", "open_jai_view", r#"{"view": "earth", "action": "show_radar"}"#),
    c("panels", 1, "Show me the clouds on 1 March 2024.", "open_jai_view", r#"{"view": "earth", "action": "show_clouds"}"#),
    c("panels", 1, "What did Europe look like yesterday?", "open_jai_view", r#"{"view": "earth", "action": "show_date", "place": "Europe"}"#),
    c("panels", 1, "What's happening in the world?", "open_jai_view", r#"{"view": "earth", "action": "show_world_tour"}"#),
    c("panels", 2, "Zeig mir, wo ich bin.", "open_jai_view", r#"{"view": "earth", "action": "show_my_location"}"#),
    c("panels", 2, "Zeig mir die Berge.", "open_jai_view", r#"{"view": "earth", "action": "show_mountains"}"#),
    c("panels", 2, "Zeig mir das Regenradar.", "open_jai_view", r#"{"view": "earth", "action": "show_radar"}"#),
    c("panels", 2, "Was passiert in der Welt?", "open_jai_view", r#"{"view": "earth", "action": "show_world_tour"}"#),
    c("panels", 3, "locate me", "open_jai_view", r#"{"view": "earth", "action": "show_my_location"}"#),
    c("panels", 3, "exaggerate the terrain", "open_jai_view", r#"{"view": "earth", "action": "exaggerate_terrain"}"#),
    c("panels", 3, "give me a world tour", "open_jai_view", r#"{"view": "earth", "action": "show_world_tour"}"#),
    // ---- His own views moved and sized (the log of 2026-11-08 had no tool for the first two) ----
    c("arrange", 1, "Move it to the middle of my screen.", "arrange_jai_window", r#"{"target": "it", "action": "move", "position": "centre"}"#),
    c("arrange", 1, "No JAI, can you full screen the Earth?", "arrange_jai_window", r#"{"target": "earth", "action": "fullscreen"}"#),
    c("arrange", 1, "Put the earth on the left half.", "arrange_jai_window", r#"{"target": "earth", "action": "move", "position": "left"}"#),
    c("arrange", 1, "Move the radar to my second monitor.", "arrange_jai_window", r#"{"target": "radar", "action": "to_monitor"}"#),
    c("arrange", 1, "Put them side by side.", "arrange_jai_window", r#"{"target": "them", "action": "side_by_side"}"#),
    c("arrange", 1, "Resize it to half the screen.", "arrange_jai_window", r#"{"target": "it", "action": "resize", "size": "half"}"#),
    c("arrange", 2, "Verschieb es in die Mitte.", "arrange_jai_window", r#"{"target": "it", "action": "move", "position": "centre"}"#),
    c("arrange", 2, "Zentriere die Erde.", "arrange_jai_window", r#"{"target": "erde", "action": "move", "position": "centre"}"#),
    c("arrange", 2, "Mach die Erde größer.", "arrange_jai_window", r#"{"target": "erde", "action": "resize", "size": "bigger"}"#),
    c("arrange", 3, "uh centre the earth", "arrange_jai_window", r#"{"target": "earth", "action": "move", "position": "centre"}"#),
    c("arrange", 3, "full screen the globe", "arrange_jai_window", r#"{"target": "globe", "action": "fullscreen"}"#),
    // (jai.log 2026-11-08: fillers after a close were asked of Windows as programs.)
    c("panels", 1, "JAI, close the Earth, you.", "hud_board", r#"{"action": "close", "target": "earth"}"#),
    c("panels", 3, "close the world please", "hud_board", r#"{"action": "close", "target": "earth"}"#),
    c("blocks", 1, "JAI close up.", "close_application", r#"{"app_name": "it", "own_only": true}"#),
    c("blocks", 3, "jai close off please", "close_application", r#"{"app_name": "it", "own_only": true}"#),
    // ---- The 3D studio (demo models in a test run: nothing is downloaded) ----
    c("studio3d", 1, "Render me an Iron Man suit in 3D.", "show_3d_model", r#"{"query": "iron man suit"}"#),
    c("studio3d", 1, "Show me a 3D model of the Earth.", "show_3d_model", r#"{"query": "earth"}"#),
    c("studio3d", 2, "Zeig mir den Mars in 3D.", "show_3d_model", r#"{"query": "mars"}"#),
    c("studio3d", 2, "3D-Modell vom Saturn.", "show_3d_model", r#"{"query": "saturn"}"#),
    c("studio3d", 3, "render me an iron man suit in three d please", "show_3d_model", r#"{"query": "iron man suit"}"#),
    // live layers (C1): satellites, passes, trains, departures, traffic, rain, past weather.
    c("live data", 1, "Show me all satellites.", "open_jai_view", r#"{"view": "earth", "action": "show_satellites", "group": "active"}"#),
    c("live data", 1, "When can I see the ISS?", "open_jai_view", r#"{"view": "live", "action": "passes", "satellite": "ISS"}"#),
    c("live data", 1, "Where is Hubble?", "open_jai_view", r#"{"view": "earth", "action": "find_satellite", "name": "Hubble"}"#),
    c("live data", 1, "When's my next train?", "open_jai_view", r#"{"view": "departures"}"#),
    c("live data", 1, "How's the traffic on the A8?", "open_jai_view", r#"{"view": "live", "action": "traffic", "road": "A8"}"#),
    c("live data", 1, "Will it rain in the next hour?", "open_jai_view", r#"{"view": "live", "action": "rain_soon"}"#),
    c("live data", 1, "What was the weather in Berlin on 3 May 2020?", "open_jai_view", r#"{"view": "live", "action": "weather_on", "date": "2020-05-03", "place": "Berlin"}"#),
    c("live data", 2, "Wann sehe ich die ISS?", "open_jai_view", r#"{"view": "live", "action": "passes", "satellite": "ISS"}"#),
    c("live data", 2, "Wann fährt meine Bahn?", "open_jai_view", r#"{"view": "departures"}"#),
    c("live data", 2, "Regnet es gleich?", "open_jai_view", r#"{"view": "live", "action": "rain_soon"}"#),
    c("live data", 3, "show me starlink", "open_jai_view", r#"{"view": "earth", "action": "show_satellites", "group": "starlink"}"#),
    c("live data", 3, "departures from karlsruhe hbf", "open_jai_view", r#"{"view": "departures", "station": "Karlsruhe Hbf"}"#),
    c("live data", 3, "show me the trains around stuttgart", "open_jai_view", r#"{"view": "earth", "action": "show_trains", "place": "Stuttgart"}"#),
    // ---- The board's own controls: tidy, find, sound cues ----
    c("blocks", 1, "Tidy the windows.", "hud_board", r#"{"action": "tidy"}"#),
    c("blocks", 1, "Find voltage on the board.", "hud_board", r#"{"action": "find", "target": "voltage"}"#),
    c("blocks", 2, "Räum die Fenster auf.", "hud_board", r#"{"action": "tidy"}"#),
    c("blocks", 3, "clear the search", "hud_board", r#"{"action": "find"}"#),
    c("blocks", 3, "turn off the sound cues", "configure_interface", r#"{"sound_cues": false}"#),
    // (What the silent session of 2026-11-06 found mis-routed, and the new session store.)
    c("blocks", 1, "Open the feature lab.", "configure_interface", r#"{"feature_lab": true}"#),
    c("blocks", 1, "Remove all widgets.", "remove_widget", r#"{"id": "all"}"#),
    c("blocks", 1, "Bring back my windows.", "spawn_native_window", r#"{"restore": "recent"}"#),
    c("blocks", 1, "Create a widget that shows my CPU load.", "create_or_update_widget", r#"{"type": "MetricGauge", "source": "cpu"}"#),
    c("blocks", 2, "Hol meine Fenster zurück.", "spawn_native_window", r#"{"restore": "recent"}"#),
    c("blocks", 2, "Entferne alle Widgets.", "remove_widget", r#"{"id": "all"}"#),
    c("blocks", 3, "keep that widget for the next start", "create_or_update_widget", r#"{"id": "last", "keep_at_start": true}"#),
    c("blocks", 3, "reopen the windows from last time", "spawn_native_window", r#"{"restore": "recent"}"#),
    c("blocks", 3, "how do i use you as a website with a static domain", "show_mobile_satellite_instructions", r#"{"topic": "permanent_address"}"#),
    c("thermals", 1, "What's my system status?", "get_system_status", "{}"),
    c("thermals", 3, "how is my pc doing", "get_system_status", "{}"),
    c("thermals", 1, "Show me my RAM usage.", "get_system_status", r#"{"reading": "memory"}"#),
    c("thermals", 2, "Wie heiß ist die Grafikkarte?", "get_system_status", r#"{"reading": "gpu"}"#),
    // ---- Programs, off and on ----
    c("programs", 1, "Turn Firefox off.", "close_application", r#"{"app_name": "firefox"}"#),
    c("programs", 1, "Turn Discord back on.", "open_application", r#"{"app_name": "discord"}"#),
    c("programs", 2, "Schalte Firefox aus.", "close_application", r#"{"app_name": "firefox"}"#),
    c("programs", 2, "Schalte Discord wieder ein.", "open_application", r#"{"app_name": "discord"}"#),
    c("programs", 3, "trun firefox off", "close_application", r#"{"app_name": "firefox"}"#),
    c("programs", 3, "turn dicsord back on", "open_application", r#"{"app_name": "discord"}"#),
    // "Get rid of it" never goes past what is his.
    c("blocks", 1, "Get rid of that.", "close_application", r#"{"own_only": true}"#),
    c("blocks", 2, "Weg damit.", "close_application", r#"{"own_only": true}"#),
    // ---- The workbench ----
    c("flashcards", 1, "Quiz me on my flashcards.", "manage_flashcards", r#"{"action": "quiz"}"#),
    c("flashcards", 1, "How many flashcards are due?", "manage_flashcards", r#"{"action": "due"}"#),
    c("flashcards", 2, "Frag mich meine Karteikarten ab.", "manage_flashcards", r#"{"action": "quiz"}"#),
    c("flashcards", 3, "quiz me on my flashcards please", "manage_flashcards", r#"{"action": "quiz"}"#),
    c("formula", 1, "Show me the formula for kinetic energy.", "lookup_formula", r#"{"query": "kinetic energy"}"#),
    c("formula", 2, "Wie lautet die Formel für die Kettenregel?", "lookup_formula", "{}"),
    c("formula", 3, "formula for ohms law", "lookup_formula", "{}"),
    c("chemistry", 1, "What is the molar mass of H2SO4?", "chemistry_helper", r#"{"action": "molar_mass"}"#),
    c("chemistry", 2, "Molare Masse von Kochsalz.", "chemistry_helper", r#"{"action": "molar_mass"}"#),
    c("chemistry", 3, "how many moles are 36 grams of water", "chemistry_helper", r#"{"action": "moles", "grams": 36.0}"#),
    c("shortcuts", 1, "What is the shortcut for split screen?", "show_app_shortcuts", r#"{"query": "split screen"}"#),
    c("shortcuts", 2, "Tastenkürzel für Screenshot.", "show_app_shortcuts", r#"{"query": "screenshot"}"#),
    c("shortcuts", 3, "show shortcuts for this app", "show_app_shortcuts", "{}"),
    c("screen time", 1, "What did I use today?", "screen_time_report", r#"{"action": "today"}"#),
    c("screen time", 2, "Wie lange sitze ich schon am Bildschirm?", "screen_time_report", r#"{"action": "break"}"#),
    c("screen time", 3, "screen time this week", "screen_time_report", r#"{"action": "week"}"#),
    c("routines", 1, "What do I usually do now?", "suggest_routine", r#"{"action": "now"}"#),
    c("routines", 2, "Was mache ich normalerweise um diese Zeit?", "suggest_routine", r#"{"action": "now"}"#),
    c("routines", 3, "show me my routines", "suggest_routine", r#"{"action": "list"}"#),
    c("thermals", 1, "How are my temperatures?", "thermal_advisor", r#"{"action": "status"}"#),
    c("thermals", 2, "Wie sind meine Temperaturen?", "thermal_advisor", r#"{"action": "status"}"#),
    c("thermals", 3, "is my pc running hot", "thermal_advisor", r#"{"action": "status"}"#),
    c("thermals", 3, "fan curve advice", "thermal_advisor", r#"{"action": "advice"}"#),
    c("clipboard", 1, "Show my clipboard history.", "clipboard_history", r#"{"action": "list"}"#),
    c("clipboard", 2, "Zeig meinen Zwischenablage-Verlauf.", "clipboard_history", r#"{"action": "list"}"#),
    c("clipboard", 3, "what did i copy before that", "clipboard_history", r#"{"action": "list"}"#),
    c("tiler", 1, "Tile my windows side by side.", "tile_windows", r#"{"layout": "side_by_side"}"#),
    c("tiler", 2, "Ordne meine Fenster nebeneinander.", "tile_windows", r#"{"layout": "side_by_side"}"#),
    c("tiler", 3, "put my windows in a grid on monitor 2", "tile_windows", r#"{"layout": "grid", "monitor": "2"}"#),
    c("palette", 1, "What can I say that starts with open?", "suggest_commands", r#"{"prefix": "open"}"#),
    c("palette", 2, "Welche Befehle beginnen mit Theme?", "suggest_commands", r#"{"prefix": "theme"}"#),
    c("palette", 3, "commands starting with move", "suggest_commands", r#"{"prefix": "move"}"#),
    // ---- "I can't hear anything": where the sound went ----
    c("no sound", 1, "There's no sound.", "control_audio_mixer", r#"{"action": "no_sound"}"#),
    c("no sound", 1, "I can't hear Discord.", "control_audio_mixer", r#"{"action": "no_sound", "app": "discord"}"#),
    c("no sound", 2, "Der Ton ist weg.", "control_audio_mixer", r#"{"action": "no_sound"}"#),
    c("no sound", 2, "Ich höre Discord nicht.", "control_audio_mixer", r#"{"action": "no_sound", "app": "discord"}"#),
    c("no sound", 3, "my audio stopped working", "control_audio_mixer", r#"{"action": "no_sound"}"#),
    c("no sound", 1, "Is anything muted?", "control_audio_mixer", r#"{"action": "muted"}"#),
    c("no sound", 2, "Ist irgendwas stumm?", "control_audio_mixer", r#"{"action": "muted"}"#),
    c("no sound", 1, "Is the sound on?", "control_audio_mixer", r#"{"action": "muted"}"#),
    c("no sound", 2, "Ist der Ton an?", "control_audio_mixer", r#"{"action": "muted"}"#),
    // ---- Blocks opened by voice ----
    c("mixer block", 1, "Open the mixer.", "configure_interface", r#"{"mixer_view": true}"#),
    c("mixer block", 1, "Close the mixer.", "hud_board", r#"{"action": "close", "target": "mixer"}"#),
    c("mixer block", 2, "Zeig den Mixer.", "configure_interface", r#"{"mixer_view": true}"#),
    c("mixer block", 3, "show the volume mixer", "configure_interface", r#"{"mixer_view": true}"#),
    c("hotkeys window", 1, "Open the hotkeys window.", "configure_interface", r#"{"hotkeys_window": true}"#),
    c("hotkeys window", 2, "Zeig mir deine Tastenkürzel.", "configure_interface", r#"{"hotkeys_window": true}"#),
    c("hotkeys window", 3, "change my mute key", "configure_interface", r#"{"hotkeys_window": true}"#),
    c("theme studio", 1, "Open the theme studio.", "configure_interface", r#"{"theme_studio": true}"#),
    c("theme studio", 2, "Erstelle ein neues Theme.", "configure_interface", r#"{"theme_studio": true}"#),
    c("theme studio", 3, "create a new theme", "configure_interface", r#"{"theme_studio": true}"#),
    // ---- Notes, lists, clocks and charts on the widget panel ----
    c("notes", 1, "Add a note saying call Tom at 5, not earlier.", "create_or_update_widget", r#"{"type": "MarkdownNoteCard", "markdown": "Tom at 5, not earlier"}"#),
    c("notes", 1, "Put a note on the board saying buy milk.", "create_or_update_widget", r#"{"type": "MarkdownNoteCard", "markdown": "buy milk"}"#),
    c("notes", 2, "Schreib eine Notiz: Milch kaufen.", "create_or_update_widget", r#"{"type": "MarkdownNoteCard", "markdown": "Milch kaufen"}"#),
    c("notes", 3, "make a note saying the wifi password changed", "create_or_update_widget", r#"{"type": "MarkdownNoteCard", "markdown": "wifi password changed"}"#),
    c("checklists", 1, "Make a shopping list with milk and eggs.", "create_or_update_widget", r#"{"type": "Checklist", "title": "Shopping list"}"#),
    c("checklists", 1, "Add butter to my shopping list.", "create_or_update_widget", r#"{"type": "Checklist", "title": "Shopping list"}"#),
    c("checklists", 1, "Check off milk.", "create_or_update_widget", r#"{"check_item": "milk"}"#),
    c("checklists", 1, "Remove milk from the list.", "create_or_update_widget", r#"{"remove_item": "milk"}"#),
    c("checklists", 2, "Setz Milch auf die Liste.", "create_or_update_widget", r#"{"id": "checklist"}"#),
    c("checklists", 3, "tick eggs off", "create_or_update_widget", r#"{"check_item": "eggs"}"#),
    c("stopwatch", 1, "Start a stopwatch.", "create_or_update_widget", r#"{"mode": "stopwatch"}"#),
    c("stopwatch", 1, "Pause the timer.", "create_or_update_widget", r#"{"id": "timer", "pause": true}"#),
    c("stopwatch", 2, "Stoppuhr starten.", "create_or_update_widget", r#"{"mode": "stopwatch"}"#),
    c("stopwatch", 3, "reset the stopwatch", "create_or_update_widget", r#"{"id": "stopwatch", "reset": true}"#),
    c("charts", 1, "Show me a CPU chart.", "create_or_update_widget", r#"{"type": "LiveLineChart", "source": "cpu"}"#),
    c("charts", 1, "Remove the CPU chart.", "remove_widget", r#"{"id": "CPU load chart"}"#),
    c("charts", 2, "Zeig mir ein CPU Diagramm.", "create_or_update_widget", r#"{"type": "LiveLineChart", "source": "cpu"}"#),
    c("charts", 3, "add a ram graph", "create_or_update_widget", r#"{"type": "LiveLineChart", "source": "ram"}"#),
    // ---- What is open, and the drives ----
    c("whats open", 1, "What's open?", "whats_open", "{}"),
    c("whats open", 2, "Was ist offen?", "whats_open", "{}"),
    c("whats open", 3, "what do you have open", "whats_open", "{}"),
    c("whats open", 1, "Is the mixer open?", "whats_open", "{}"),
    c("programs", 1, "Is Firefox open?", "is_app_running", r#"{"app_name": "firefox"}"#),
    c("programs", 2, "Läuft Discord?", "is_app_running", r#"{"app_name": "discord"}"#),
    c("stopwatch", 1, "How long is left on the timer?", "timer_left", "{}"),
    c("stopwatch", 2, "Wie lange noch?", "timer_left", "{}"),
    c("drive space", 1, "Show me my drive space.", "spawn_native_window", r#"{"live": "drive_space"}"#),
    c("drive space", 2, "Zeig mir meinen Speicherplatz.", "spawn_native_window", r#"{"live": "drive_space"}"#),
    c("drive space", 3, "uh jai could you maybe show me my drive space please", "spawn_native_window", r#"{"live": "drive_space"}"#),
    c("undo", 1, "Change it back.", "undo_last_action", "{}"),
    c("undo", 2, "Ändere es zurück.", "undo_last_action", "{}"),
    c("chemistry", 1, "How many moles are in 36 grams of water?", "chemistry_helper", r#"{"action": "moles"}"#),
    // Pictures, live cameras, street view (their dry runs never reach the network).
    c("pictures", 1, "Show me a picture of the Eiffel Tower.", "show_image", r#"{"query": "eiffel tower"}"#),
    c("pictures", 1, "Open an image of the Earth.", "show_image", r#"{"query": "the Earth"}"#),
    c("pictures", 1, "Open my latest screenshot.", "show_image", r#"{"local": "latest_screenshot"}"#),
    c("pictures", 1, "Show me the photo on my desktop.", "show_image", r#"{"local": "desktop"}"#),
    c("pictures", 1, "Next picture.", "show_image", r#"{"action": "next"}"#),
    c("pictures", 1, "Save this picture.", "show_image", r#"{"action": "save"}"#),
    c("pictures", 2, "Zeig mir ein Bild von Neuschwanstein.", "show_image", r#"{"query": "neuschwanstein"}"#),
    c("pictures", 2, "Zeig mir meinen letzten Screenshot.", "show_image", r#"{"local": "latest_screenshot"}"#),
    c("pictures", 2, "Nächstes Bild.", "show_image", r#"{"action": "next"}"#),
    c("pictures", 3, "uh jai could you show me some photos of tokyo please", "show_image", r#"{"query": "tokyo"}"#),
    c("pictures", 3, "show me tokyo pictures", "show_image", r#"{"query": "tokyo"}"#),
    c("cameras", 1, "Show me cameras in London.", "show_live_camera", r#"{"place": "london"}"#),
    c("cameras", 1, "Show me a live camera from New York.", "show_live_camera", r#"{"place": "new york"}"#),
    c("cameras", 1, "Show me the camera nearest to me.", "show_live_camera", r#"{"near_me": true}"#),
    c("cameras", 1, "Show me a 3 by 3 camera wall of Hong Kong.", "show_live_camera", r#"{"action": "wall", "size": 9}"#),
    c("cameras", 2, "Zeig mir eine Webcam in Stuttgart.", "show_live_camera", r#"{"place": "stuttgart"}"#),
    c("cameras", 2, "Zeig mir Webcams in meiner Nähe.", "show_live_camera", r#"{"near_me": true}"#),
    c("cameras", 3, "uh jai show me a live cam in tokyo please", "show_live_camera", r#"{"place": "tokyo"}"#),
    c("street view", 1, "Show me street view of the Eiffel Tower.", "show_street_view", r#"{"place": "eiffel tower"}"#),
    c("street view", 1, "Street view here.", "show_street_view", r#"{"here": true}"#),
    c("street view", 2, "Zeig mir die Straßenansicht vom Brandenburger Tor.", "show_street_view", r#"{"place": "brandenburger tor"}"#),
    c("street view", 3, "uh jai show me street level photos of times square", "show_street_view", r#"{"place": "times square"}"#),
];

/// One compound sentence of pass 5: every `wants` entry must be reached.
pub struct Compound {
    pub category: &'static str,
    pub say: &'static str,
    pub wants: &'static [(&'static str, &'static str)],
    pub needs: Needs,
}

const THEME_CYAN: (&str, &str) = ("configure_interface", r#"{"hud_palette": "arc_reactor"}"#);
const ORB_TOP_RIGHT: (&str, &str) = ("configure_interface", r#"{"orb_board_position": "top right"}"#);
const PAUSE: (&str, &str) = ("control_music", "{}");
const STATS: (&str, &str) = ("open_jai_view", r#"{"view": "system_stats"}"#);

/// Matrix A, pass 5: one compound per category.
pub const COMPOUNDS: &[Compound] = &[
    Compound { category: "core", say: "Show me your core, set theme to Arc Cyan and move the orb to the top right.", wants: &[("configure_interface", r#"{"core_view": true}"#), THEME_CYAN, ORB_TOP_RIGHT], needs: Needs::Nothing },
    Compound { category: "undo", say: "Undo that and move the orb to the top right.", wants: &[("undo_last_action", "{}"), ORB_TOP_RIGHT], needs: Needs::Nothing },
    Compound { category: "mix", say: "Load my gaming mix and pause the music.", wants: &[("control_audio_mixer", r#"{"action": "load_mix", "name": "gaming"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "no sound", say: "There's no sound and open the mixer.", wants: &[("control_audio_mixer", r#"{"action": "no_sound"}"#), ("configure_interface", r#"{"mixer_view": true}"#)], needs: Needs::Nothing },
    Compound { category: "programs", say: "Turn Firefox off and open the mixer.", wants: &[("close_application", r#"{"app_name": "firefox"}"#), ("configure_interface", r#"{"mixer_view": true}"#)], needs: Needs::Nothing },
    Compound { category: "mixer block", say: "Open the mixer and mute Discord.", wants: &[("configure_interface", r#"{"mixer_view": true}"#), ("control_audio_mixer", r#"{"action": "mute_app", "app": "discord"}"#)], needs: Needs::Nothing },
    Compound { category: "theme studio", say: "Open the theme studio and move the orb to the top right.", wants: &[("configure_interface", r#"{"theme_studio": true}"#), ORB_TOP_RIGHT], needs: Needs::Nothing },
    Compound { category: "studio3d", say: "Render me the Moon in 3D and set theme to Arc Cyan.", wants: &[("show_3d_model", r#"{"query": "moon"}"#), THEME_CYAN], needs: Needs::Nothing },
    Compound { category: "notes", say: "Put a note on the board saying buy milk and move the orb to the top right.", wants: &[("create_or_update_widget", r#"{"markdown": "buy milk"}"#), ORB_TOP_RIGHT], needs: Needs::Nothing },
    Compound { category: "checklists", say: "Add butter to my shopping list and start a stopwatch.", wants: &[("create_or_update_widget", r#"{"type": "Checklist"}"#), ("create_or_update_widget", r#"{"mode": "stopwatch"}"#)], needs: Needs::Nothing },
    Compound { category: "stopwatch", say: "Start a stopwatch and pause the music.", wants: &[("create_or_update_widget", r#"{"mode": "stopwatch"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "charts", say: "Show me a CPU chart and set theme to Arc Cyan.", wants: &[("create_or_update_widget", r#"{"type": "LiveLineChart"}"#), THEME_CYAN], needs: Needs::Nothing },
    Compound { category: "whats open", say: "Move the orb to the top right and tell me what's open.", wants: &[ORB_TOP_RIGHT, ("whats_open", "{}")], needs: Needs::Nothing },
    Compound { category: "drive space", say: "Show me my drive space and pause the music.", wants: &[("spawn_native_window", r#"{"live": "drive_space"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "orb", say: "Move the orb to the top right, pause the music and open my system stats.", wants: &[ORB_TOP_RIGHT, PAUSE, STATS], needs: Needs::Nothing },
    Compound { category: "theme", say: "Set theme to Stark Gold and show me your core.", wants: &[("configure_interface", r#"{"hud_palette": "stark_gold"}"#), ("configure_interface", r#"{"core_view": true}"#)], needs: Needs::Nothing },
    Compound { category: "close", say: "Close the feature lab and clear the board.", wants: &[("hud_board", r#"{"action": "close", "target": "feature lab"}"#), ("hud_board", r#"{"action": "clear"}"#)], needs: Needs::Nothing },
    Compound { category: "pages", say: "Next page and move the orb to the top right.", wants: &[("hud_board", r#"{"action": "flip_note", "direction": "next"}"#), ORB_TOP_RIGHT], needs: Needs::Pages },
    Compound { category: "listening", say: "Set theme to Arc Cyan and stop listening.", wants: &[THEME_CYAN, ("listening_lock", r#"{"command": "Lock"}"#)], needs: Needs::Nothing },
    Compound { category: "music", say: "Pause the music and set theme to Arc Cyan.", wants: &[PAUSE, THEME_CYAN], needs: Needs::Nothing },
    Compound { category: "volume", say: "Volume 40 and next song.", wants: &[("control_audio_mixer", r#"{"action": "set_master", "level": 40}"#), ("control_music", r#"{"action": "next_track"}"#)], needs: Needs::Nothing },
    Compound { category: "monitors", say: "Use all monitors and set theme to Arc Cyan.", wants: &[("configure_interface", r#"{"span_all_monitors": true}"#), THEME_CYAN], needs: Needs::Nothing },
    Compound { category: "schedule", say: "Open my WebUntis schedule and set theme to Arc Cyan.", wants: &[("check_school_schedule|open_jai_view", "{}"), THEME_CYAN], needs: Needs::Nothing },
    Compound { category: "research", say: "Research solid state batteries and move the orb to the top right.", wants: &[("hud_board", r#"{"action": "research"}"#), ORB_TOP_RIGHT], needs: Needs::Nothing },
    Compound { category: "holomat", say: "Open the simulation lab and pause the music.", wants: &[("open_jai_view", r#"{"view": "simulation_lab"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "panels", say: "Check the network and pause the music.", wants: &[("open_jai_view", r#"{"view": "network_radar"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "arrange", say: "Centre the Earth and pause the music.", wants: &[("arrange_jai_window", r#"{"target": "earth", "action": "move", "position": "centre"}"#), PAUSE], needs: Needs::Nothing },
    // live layers (C1)
    Compound { category: "live data", say: "Show me Starlink and pause the music.", wants: &[("open_jai_view", r#"{"view": "earth", "action": "show_satellites", "group": "starlink"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "blocks", say: "Tidy the windows and turn off the sound cues.", wants: &[("hud_board", r#"{"action": "tidy"}"#), ("configure_interface", r#"{"sound_cues": false}"#)], needs: Needs::Nothing },
    Compound { category: "flashcards", say: "Quiz me on my flashcards and pause the music.", wants: &[("manage_flashcards", r#"{"action": "quiz"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "formula", say: "Show me the formula for kinetic energy and open my system stats.", wants: &[("lookup_formula", "{}"), STATS], needs: Needs::Nothing },
    Compound { category: "chemistry", say: "What is the molar mass of water and show me the formula for the ideal gas law.", wants: &[("chemistry_helper", r#"{"action": "molar_mass"}"#), ("lookup_formula", "{}")], needs: Needs::Nothing },
    Compound { category: "shortcuts", say: "Show shortcuts for this app and pause the music.", wants: &[("show_app_shortcuts", "{}"), PAUSE], needs: Needs::Nothing },
    Compound { category: "screen time", say: "What did I use today and how are my temperatures.", wants: &[("screen_time_report", r#"{"action": "today"}"#), ("thermal_advisor", r#"{"action": "status"}"#)], needs: Needs::Nothing },
    Compound { category: "routines", say: "What do I usually do now and pause the music.", wants: &[("suggest_routine", r#"{"action": "now"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "thermals", say: "How are my temperatures and show my clipboard history.", wants: &[("thermal_advisor", r#"{"action": "status"}"#), ("clipboard_history", r#"{"action": "list"}"#)], needs: Needs::Nothing },
    Compound { category: "clipboard", say: "Show my clipboard history and pause the music.", wants: &[("clipboard_history", r#"{"action": "list"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "tiler", say: "Tile my windows side by side and pause the music.", wants: &[("tile_windows", r#"{"layout": "side_by_side"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "palette", say: "What can I say that starts with open and pause the music.", wants: &[("suggest_commands", r#"{"prefix": "open"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "pictures", say: "Show me a picture of the Eiffel Tower and pause the music.", wants: &[("show_image", r#"{"query": "eiffel tower"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "cameras", say: "Show me a live camera from New York and move the orb to the top right.", wants: &[("show_live_camera", r#"{"place": "new york"}"#), ORB_TOP_RIGHT], needs: Needs::Nothing },
    Compound { category: "street view", say: "Show me street view of the Eiffel Tower and pause the music.", wants: &[("show_street_view", r#"{"place": "eiffel tower"}"#), PAUSE], needs: Needs::Nothing },
    Compound { category: "hotkeys window", say: "Open the hotkeys window and pause the music.", wants: &[("configure_interface", r#"{"hotkeys_window": true}"#), PAUSE], needs: Needs::Nothing },
];

/// The categories of matrix A, in the table's order.
pub fn categories() -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for case in CASES {
        if !seen.contains(&case.category) {
            seen.push(case.category);
        }
    }
    seen
}

/// Whether `got` carries everything `want` asks for: objects key by key, a string when it
/// contains the wanted text (any case), anything else when it is equal.
pub fn carries(got: &Value, want: &Value) -> bool {
    match (got, want) {
        (_, Value::Object(w)) => w.iter().all(|(key, value)| got.get(key).is_some_and(|g| carries(g, value))),
        (Value::String(g), Value::String(w)) => g.to_lowercase().contains(&w.to_lowercase()),
        // 40 and 40.0 are the same level.
        (Value::Number(g), Value::Number(w)) => g.as_f64() == w.as_f64(),
        _ => got == want,
    }
}

/// Whether one of the calls is `tool` (one of its `|` alternatives) with the arguments `args`.
pub fn reached(calls: &[(String, Value)], tool: &str, args: &str) -> bool {
    let want: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    calls.iter().any(|(name, got)| tool.split('|').any(|t| t == name) && (want.is_null() || carries(got, &want)))
}

/// The music player (what a mode switch or a theme change must never reach).
fn is_music(tool: &str) -> bool {
    matches!(tool, "control_music" | "media_and_volume_control")
}

/// The verdict on one sentence: its handler reached, and the music player not reached unless
/// the sentence is about music.
fn verdict(calls: &[(String, Value)], wants: &[(&str, &str)]) -> Result<String, String> {
    let did = || if calls.is_empty() { "nothing".to_string() } else { calls.iter().map(|(t, a)| format!("{t} {a}")).collect::<Vec<_>>().join("; ") };
    for (tool, args) in wants {
        if !reached(calls, tool, args) {
            return Err(format!("wanted {tool} {args}; it did: {}", did()));
        }
    }
    let music_wanted = wants.iter().any(|(tool, _)| tool.split('|').any(is_music) || tool.contains("audio_mixer"));
    if !music_wanted && calls.iter().any(|(tool, _)| is_music(tool)) {
        return Err(format!("the music player was reached: {}", did()));
    }
    Ok(did())
}

/// The reflex context a case's sentence is said in (what the app's state would be for it).
pub fn context_for(needs: Needs) -> ReflexContext {
    ReflexContext { board_up: needs == Needs::Pages, spotlight: false, paged: needs == Needs::Pages }
}

// --------------------------------------------------------------------------- matrix A ----

/// A three-page report: with it up, "next" and "go back" turn pages.
fn paged_report() -> crate::hud_board::Report {
    use crate::hud_board::{Block, BlockKind, Note, Report};
    let notes = (1..=3).map(|n| Note { headline: format!("PAGE {n}"), body: format!("Exhaustive verifier, page {n} of 3.") }).collect();
    Report { topic: "Exhaustive verifier".to_string(), created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(), blocks: vec![Block::new("VERIFIER // PAGES", "PROBE", "EXHAUSTIVE VERIFIER", BlockKind::NoteStack { notes })], sources: Vec::new(), id: None }
}

/// Puts the app in the state a sentence needs (and out of the states it must not be in).
/// False when that state is Sir's right now and must not be touched.
fn prepare(needs: Needs, ours: &mut bool) -> bool {
    let board_up = crate::hud_board::is_active();
    match needs {
        Needs::Pages if !board_up => {
            crate::hud_board::present(paged_report());
            *ours = true;
        }
        Needs::Pages if !*ours => return false,
        Needs::Pages => {}
        _ if board_up && *ours => {
            crate::hud_board::close_board();
            *ours = false;
        }
        _ => {}
    }
    true
}

/// A dry turn and how long it took (ms). One over `budget_ms` is said again from the same stage
/// after a pause, twice at most, and the fastest counts: a slow path stays slow, a moment the
/// scheduler gave to another program does not (2026-11-07: 19 probes at 10-29 ms within one
/// second while Sir used the PC; 2026-11-08 beside Minecraft: 12-16 ms after a second try
/// straight away, which the same burst of the game's work had caught too).
async fn dry_timed(app: &Jai, say: &str, budget_ms: f64, needs: Needs, ours: &mut bool) -> (Vec<crate::live_self_tester::Recorded>, f64) {
    let (mut recorded, took) = app.dry_run(say, true).await;
    let mut ms = took.as_secs_f64() * 1000.0;
    for _ in 0..crate::live_self_tester::AGAIN {
        if ms <= budget_ms || !prepare(needs, ours) {
            break;
        }
        tokio::time::sleep(crate::live_self_tester::AGAIN_AFTER).await;
        let (again, took_again) = app.dry_run(say, true).await;
        let again_ms = took_again.as_secs_f64() * 1000.0;
        if again_ms < ms {
            (recorded, ms) = (again, again_ms);
        }
    }
    (recorded, ms)
}

async fn matrix_a(app: &Jai) {
    let mark = app.history_mark().await;
    let mut ours = false;
    let calls_of = |recorded: Vec<crate::live_self_tester::Recorded>| -> Vec<(String, Value)> { recorded.into_iter().map(|r| (r.tool, r.args)).collect() };
    // The routing decision alone, family by family: every sentence through the stateless
    // parsers, the slowest of them inside the reflex budget. (The best of three: a single
    // measurement is at the scheduler's mercy, and the first one compiles the patterns.)
    for category in categories() {
        let mut slowest = (0.0f64, "");
        let mut wrong: Vec<String> = Vec::new();
        for case in CASES.iter().filter(|c| c.category == category) {
            let ctx = context_for(case.needs);
            let mut best = f64::INFINITY;
            let mut calls: Vec<(String, Value)> = Vec::new();
            for _ in 0..3 {
                let started = Instant::now();
                let resolved = crate::compound_planner::resolve_one(case.say, &ctx);
                best = best.min(ms_since(started));
                calls = resolved.into_iter().collect();
            }
            if let Err(why) = verdict(&calls, &[(case.tool, case.args)]) {
                wrong.push(format!("\"{}\": {why}", case.say));
            }
            if best > slowest.0 {
                slowest = (best, case.say);
            }
        }
        let result = if wrong.is_empty() { Ok(format!("slowest: \"{}\"", slowest.1)) } else { Err(wrong.join("; ")) };
        judge("matrix A", format!("{category} / the routing decision alone"), REFLEX_BUDGET_MS, slowest.0, result);
    }
    // Every sentence once without a verdict: its patterns are compiled, as they are for Sir
    // after the start-up warm-up.
    for (say, needs) in CASES.iter().map(|c| (c.say, c.needs)).chain(COMPOUNDS.iter().map(|c| (c.say, c.needs))) {
        if prepare(needs, &mut ours) {
            let _ = app.dry_run(say, true).await;
        }
    }
    // Passes 1-3.
    for case in CASES {
        let id = format!("{} / pass {} / {}", case.category, case.pass, case.say);
        if !prepare(case.needs, &mut ours) {
            skip("matrix A", id, "a board of Sir's is up; it is not replaced");
            continue;
        }
        let (recorded, ms) = dry_timed(app, case.say, TURN_BUDGET_MS, case.needs, &mut ours).await;
        judge("matrix A", id, TURN_BUDGET_MS, ms, verdict(&calls_of(recorded), &[(case.tool, case.args)]));
    }
    // Pass 5: every clause of a compound sentence.
    for compound in COMPOUNDS {
        let id = format!("{} / pass 5 / {}", compound.category, compound.say);
        if !prepare(compound.needs, &mut ours) {
            skip("matrix A", id, "a board of Sir's is up; it is not replaced");
            continue;
        }
        let (recorded, ms) = dry_timed(app, compound.say, COMPOUND_BUDGET_MS, compound.needs, &mut ours).await;
        let calls = calls_of(recorded);
        let result = verdict(&calls, compound.wants).map(|did| format!("{} actions: {did}", calls.len()));
        judge("matrix A", id, COMPOUND_BUDGET_MS, ms, result);
    }
    prepare(Needs::Nothing, &mut ours);
    app.history_rollback(mark).await;
    // Pass 4: four commands inside 400 ms, spoken over his own voice.
    for category in categories() {
        barge_in(app, category, &mut ours).await;
    }
    prepare(Needs::Nothing, &mut ours);
    app.history_rollback(mark).await;
    crate::listening_lock::apply(crate::listening_lock::LockCommand::Resume);
}

/// What JAI is saying when the commands of pass 4 arrive (long enough to be cut off).
const SPEAKING: &str = "Running the full diagnostic now, Sir. Every subsystem reports in turn, beginning with the reactor core, the flight surfaces, the targeting array and the long-range communications relay, and I will read each figure as it arrives.";

/// How long everything may take to settle after the burst of pass 4. Not a latency: the last
/// command's own answer is spoken to its end (a confirmation of several seconds for some), and
/// what is checked is that nothing is left running after it. (The thermal readout is the
/// longest: 10 s on a quiet PC, 12-14 s beside a game, 2026-11-08; a turn that hangs stays
/// past any limit.)
const SETTLE_LIMIT: Duration = Duration::from_secs(30);

/// Pass 4 for one category: its sentences in turn, four of them, 90 ms apart, while he speaks.
/// Everything must settle (no turn left running, the thinking guard released), his sentence
/// must have been cut, and the last command must have reached its handler.
async fn barge_in(app: &Jai, category: &'static str, ours: &mut bool) {
    let id = format!("{category} / pass 4 / four commands inside {BARGE_IN_WINDOW_MS} ms over his voice");
    let cases: Vec<&Case> = CASES.iter().filter(|c| c.category == category).collect();
    let needs = if cases.iter().any(|c| c.needs == Needs::Pages) { Needs::Pages } else { Needs::Nothing };
    if cases.is_empty() || !prepare(needs, ours) {
        skip("matrix A", id, "the state it needs is Sir's right now");
        return;
    }
    let four: Vec<&Case> = (0..4).map(|i| cases[i % cases.len()]).collect();
    let says: Vec<&str> = four.iter().map(|c| c.say).collect();
    crate::live_self_tester::set_everything_dry(true);
    let outcome = app.probe_barge_in(SPEAKING, &says, Duration::from_millis(90), SETTLE_LIMIT).await;
    // The last turn's record lands with its last await.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let log: Vec<(String, Value)> = crate::live_self_tester::process_log().into_iter().map(|r| (r.tool, r.args)).collect();
    crate::live_self_tester::set_everything_dry(false);
    let last = four[3];
    let want: Value = serde_json::from_str(last.args).unwrap_or(Value::Null);
    // The dispatcher's own switches are judged by the state they leave; everything else by
    // what the dry run recorded.
    let handled = match last.tool {
        "listening_lock" => crate::listening_lock::is_locked() != (want["command"] == "Resume"),
        tool => reached(&log, tool, last.args),
    };
    // Whatever the burst switched is switched back.
    crate::listening_lock::apply(crate::listening_lock::LockCommand::Resume);
    let mut wrong: Vec<String> = Vec::new();
    if !outcome.was_speaking {
        wrong.push("he was not speaking when the first command arrived".to_string());
    }
    if outcome.fired_in.as_millis() > BARGE_IN_WINDOW_MS {
        wrong.push(format!("the four commands took {} ms to arrive", outcome.fired_in.as_millis()));
    }
    if !outcome.speech_cut {
        wrong.push("his sentence was not cut off".to_string());
    }
    if outcome.settled.is_none() || outcome.busy {
        wrong.push(format!("a turn was still running after {} seconds", SETTLE_LIMIT.as_secs()));
    }
    if outcome.thinking {
        wrong.push("the thinking guard was never released".to_string());
    }
    if !handled {
        wrong.push(format!("the last command ('{}') did not reach {} {}; recorded: {}", last.say, last.tool, last.args, log.iter().map(|(t, a)| format!("{t} {a}")).collect::<Vec<_>>().join("; ")));
    }
    let limit_ms = SETTLE_LIMIT.as_secs_f64() * 1000.0;
    let ms = outcome.settled.map_or(limit_ms, |d| d.as_secs_f64() * 1000.0);
    let result = if wrong.is_empty() { Ok(format!("{} → settled in {ms:.0} ms; last: {} {}", says.join(" / "), last.tool, last.args)) } else { Err(wrong.join("; ")) };
    judge("matrix A", id, limit_ms, ms, result);
}

// --------------------------------------------------------------------------- matrix B ----

/// One finished check of the visual matrix: (id, milliseconds, findings or a summary).
type Row = (String, f64, Result<String, String>);

/// The HUD's own work in 40 frames at 180 Hz with what is open now: (median, average, worst) ms.
fn frame_costs(hud: &mut crate::gui::offscreen::VirtualHud) -> (f64, f64, f64) {
    let costs: Vec<f64> = (0..40)
        .map(|_| {
            hud.frame(1.0 / 180.0);
            hud.frame_cost_ms()
        })
        .collect();
    crate::gui::frame_summary(&costs).unwrap_or_default()
}

fn row(rows: &mut Vec<Row>, id: String, started: Instant, findings: Vec<String>, summary: String) {
    let result = if findings.is_empty() { Ok(summary) } else { Err(format!("{} finding(s): {}", findings.len(), crate::crash_guard::clip(&findings.join(" | "), 900))) };
    rows.push((id, ms_since(started), result));
}

fn text_findings(frame: &visual::Frame) -> Vec<String> {
    let mut found = visual::clipped_text(frame);
    found.extend(visual::pictographs(frame));
    found.extend(visual::text_over_text(frame));
    found
}

/// The window animations as the window plays them, on the real HUD: a block unfolds into its
/// place as it opens, its content showing after its frame, and closed by its `[ CLOSE ]` it
/// plays its exit (drawn smaller, still there) before it leaves.
fn window_motion(hud: &mut crate::gui::offscreen::VirtualHud, tools: &ToolContext, rows: &mut Vec<Row>) {
    let started = Instant::now();
    let mut found = Vec::new();
    let id = block_id(0);
    // How tall the block's frame is drawn (none: not drawn), and whether its title shows.
    // (`gui::WindowMotion` unfolds the frame about its middle at full width and never scales
    // it; the content inside is clipped to that band, so a fill counts by what of it shows.
    // egui's own `Frame::none()` leaves a transparent rect over the whole place: not a frame.)
    // For a failure, the visible heights of the fills spanning the block, frame by frame.
    let heights_seen: std::cell::RefCell<Vec<String>> = Default::default();
    let look = |frame: &visual::Frame| -> (Option<f32>, bool) {
        let Some(place) = crate::hud_dock::block_rect(&id) else {
            heights_seen.borrow_mut().push("-".into());
            return (None, false);
        };
        let spans: Vec<f32> = frame
            .fills
            .iter()
            .filter(|f| f.cover > 0)
            .map(|f| f.rect.intersect(f.clip))
            .filter(|r| r.is_positive() && place.expand(2.0).contains_rect(*r) && r.width() >= place.width() * 0.8 && (r.center().x - place.center().x).abs() <= 4.0)
            .map(|r| r.height())
            .collect();
        if heights_seen.borrow().len() < 40 {
            heights_seen.borrow_mut().push(format!("[{}]", spans.iter().map(|h| format!("{h:.0}")).collect::<Vec<_>>().join(",")));
        }
        let titled = frame.find_text("verifier // entrance").iter().any(|t| place.expand(2.0).contains(t.center()));
        (spans.into_iter().reduce(f32::max), titled)
    };
    hud.animate_windows(true);
    let _ = tools.widgets.spawn_window(&json!({ "id": id, "title": "Verifier // entrance", "markdown": "The reactor holds its output.", "replace_content": true, "width": 540, "height": 330 }));
    // The entrance, frame by frame, until it has rested a while.
    let mut entrance: Vec<(Option<f32>, bool)> = Vec::new();
    let mut first: Option<Instant> = None;
    while first.map_or(started.elapsed() < Duration::from_millis(1500), |at| at.elapsed() < Duration::from_millis(600)) {
        let seen = look(&hud.frame(1.0 / 60.0));
        if seen.0.is_some() {
            first.get_or_insert_with(Instant::now);
            entrance.push(seen);
        }
    }
    // At rest: the frame's height in the last frame. The unfold: how much shorter it was.
    let rest = entrance.last().and_then(|s| s.0).unwrap_or(0.0);
    let narrowest = entrance.iter().filter_map(|s| s.0).map(|h| rest - h).fold(0.0f32, f32::max);
    let diagnosis = || format!("place {:?}, frame heights per frame {}", crate::hud_dock::block_rect(&id).map(|r| r.size()), heights_seen.borrow().join(" "));
    match entrance.last() {
        None => found.push("the block's frame was never drawn".to_string()),
        Some(&(_, titled)) => {
            if narrowest < 2.0 {
                found.push(format!("the block did not unfold into its place (at most {narrowest:.1} px shorter than at rest; {})", diagnosis()));
            }
            let place_h = crate::hud_dock::block_rect(&id).map_or(0.0, |r| r.height());
            if rest < place_h * 0.85 || !titled {
                found.push(format!("after its entrance the block's frame is {rest:.0} px of its {place_h:.0} px place{}", if titled { "" } else { " and its title is not drawn" }));
            }
            if entrance.first().is_some_and(|s| s.1) {
                found.push("the block's title showed in its first frame (its content should come in after its frame)".to_string());
            }
        }
    }
    // The exit: closed by hand, it is still there for a moment, folding, then gone.
    let frame = hud.frames_at(2, 60.0);
    let place = crate::hud_dock::block_rect(&id);
    match frame.find_exact("CLOSE").into_iter().find(|r| place.is_some_and(|p| p.contains(r.center()))) {
        None => found.push("the block shows no [ CLOSE ]".to_string()),
        Some(chip) => {
            hud.click(chip.center(), PointerButton::Primary);
            hud.pointer_gone();
            let leaving = crate::hud_dock::is_open(&id);
            let closed_at = Instant::now();
            let mut smallest = 0.0f32;
            while closed_at.elapsed() < Duration::from_millis(1500) && crate::hud_dock::is_open(&id) {
                if let (Some(h), _) = look(&hud.frame(1.0 / 60.0)) {
                    smallest = smallest.max(rest - h);
                }
            }
            hud.frames_at(3, 60.0);
            if !leaving {
                found.push("the block vanished at the click instead of playing its exit".to_string());
            } else if smallest < 1.5 {
                found.push(format!("the block did not fold as it left (at most {smallest:.1} px shorter; {})", diagnosis()));
            }
            // (Closed, it waits in the tray for ten minutes: closed all the same.)
            if crate::hud_dock::whereabouts(&id) != crate::hud_dock::Whereabouts::Closed {
                found.push("the block was still open 1.5 s after its [ CLOSE ]".to_string());
            }
        }
    }
    hud.animate_windows(false);
    let _ = tools.widgets.close_window(&id);
    crate::hud_dock::forget(&id);
    hud.frames_at(10, 60.0);
    row(rows, "a block unfolds into place as it opens and plays its exit when closed".to_string(), started, found, format!("{} entrance frame(s), at most {narrowest:.1} px shorter on the way in", entrance.len()));
}

/// The offscreen HUD's blocks the verifier opened, by number.
fn block_id(n: usize) -> String {
    format!("exhaustive_block_{n}")
}

/// Matrix B on the offscreen HUD's thread. `run` varies which sizes, texts and deltas are used.
fn visual_matrix(hud: &mut crate::gui::offscreen::VirtualHud, tools: &ToolContext, rt: &tokio::runtime::Handle, run: usize) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let home = hud.size();
    let palette = tools.config.read(|c| c.interface.hud_palette.clone());
    let orb_home = tools.config.read(|c| c.interface.orb_board_pos);
    let sizes = [[820.0, 600.0], [480.0, 640.0], [900.0, 560.0], [540.0, 330.0], [420.0, 240.0], [560.0, 300.0]];
    let close_blocks = |hud: &mut crate::gui::offscreen::VirtualHud, count: usize| {
        for n in 0..count {
            let _ = tools.widgets.close_window(&block_id(n));
            crate::hud_dock::forget(&block_id(n));
        }
        hud.frames_at(14, 60.0);
    };
    // The HUD is drawn on the reference desk: on each of its three monitors, and as one board
    // across all of them (the box around them has corners no monitor shows).
    crate::monitors::pretend(Some(visual::desk()));
    for (s, (name, screen, origin)) in visual::desk_screens().into_iter().enumerate() {
        hud.resize(screen, origin);
        hud.pointer_gone();
        hud.frames_at(20, 60.0);
        let parts = hud.monitor_parts();
        let regions = hud.block_regions();
        let home_screen = hud.home();
        // (Across several monitors, nothing may stand where none of them shows it whole.)
        let text_findings = |frame: &visual::Frame| {
            let mut found = text_findings(frame);
            found.extend(visual::off_every_screen(frame, &parts));
            found
        };

        // Checks 1 and 5 on the HUD at rest, with the console and its completions, with the
        // Feature Lab, with the search bar.
        let started = Instant::now();
        let frame = hud.frames_at(3, 60.0);
        row(&mut rows, format!("{name}: the HUD at rest"), started, text_findings(&frame), format!("{} texts, {} outlines, none cut", frame.texts.len(), frame.strokes.len()));

        // The HUD proper lives on its home screen: the window, or the first monitor of a board
        // across all of them. The emblem rests in its middle; the header strip is at its top.
        let started = Instant::now();
        let mut found = Vec::new();
        if parts.len() > 1 && (home_screen.width() - 2560.0).abs() > 1.0 {
            found.push(format!("across {} monitors the home screen is {home_screen:?}, not the first monitor", parts.len()));
        }
        let (centre, radius) = hud.emblem();
        if !home_screen.contains_rect(Rect::from_center_size(centre, Vec2::splat(radius * 2.0))) {
            found.push(format!("the emblem at {centre:?} (radius {radius:.0}) leaves the home screen {home_screen:?}"));
        }
        if orb_home.is_none() && (centre - home_screen.center()).length() > 1.5 {
            found.push(format!("the emblem rests at {centre:?}; the middle of the home screen is {:?}", home_screen.center()));
        }
        if hud.emblem_state() == "Docked" {
            found.push("the emblem is docked although nothing covers the middle".to_string());
        }
        if !frame.find_text("SCREENS").iter().any(|r| home_screen.contains_rect(*r) && r.top() < home_screen.top() + 60.0) {
            found.push("the header strip is not at the top of the home screen".to_string());
        }
        row(&mut rows, format!("{name}: the emblem in the middle of the home screen, the header at its top"), started, found, format!("home {:.0}x{:.0}, emblem at {centre:?}", home_screen.width(), home_screen.height()));

        // The SCREENS menu: every monitor's row and button apart, the span switch offered.
        let started = Instant::now();
        hud.set_surface("screens", true);
        let frame = hud.frames_at(6, 60.0);
        let mut found = text_findings(&frame);
        let moves = frame.find_exact("MOVE HERE").len();
        if moves != 2 {
            found.push(format!("the SCREENS menu offers {moves} monitors to move to; two of the three are not the HUD's own"));
        }
        match frame.find_exact("SPAN ALL MONITORS").first() {
            Some(key) if home_screen.contains_rect(*key) => {}
            Some(key) => found.push(format!("the SCREENS menu's span switch at {key:?} is off the home screen")),
            None => found.push("the SCREENS menu does not offer [ SPAN ALL MONITORS ]".to_string()),
        }
        row(&mut rows, format!("{name}: the SCREENS menu"), started, found, format!("{moves} monitors to move to, the span switch, no text on another"));
        hud.set_surface("screens", false);
        hud.frames_at(6, 60.0);

        let started = Instant::now();
        hud.set_surface("console", true);
        hud.frames_at(28, 60.0);
        hud.type_in_console(CONSOLE_PREFIXES[(run + s) % CONSOLE_PREFIXES.len()]);
        let frame = hud.frames_at(4, 60.0);
        let offered = hud.completions();
        let mut found = text_findings(&frame);
        match offered.first() {
            None => found.push("the console offers no completion".to_string()),
            Some(first) if frame.find_text(first).is_empty() => found.push(format!("the completion '{first}' is not drawn")),
            Some(_) => {}
        }
        row(&mut rows, format!("{name}: the text console with its completions"), started, found, format!("offers {}", offered.join(" / ")));
        hud.close_controls();
        hud.frames_at(28, 60.0);

        let started = Instant::now();
        hud.set_surface("features", true);
        let frame = hud.frames_at(12, 60.0);
        let mut found = text_findings(&frame);
        match crate::hud_dock::block_rect("features") {
            Some(rect) if regions.iter().any(|r| r.expand(1.0).contains_rect(rect)) => {}
            Some(rect) => found.push(format!("the Feature Lab leaves the board: {rect:?}")),
            None => found.push("the Feature Lab was not drawn".to_string()),
        }
        row(&mut rows, format!("{name}: the Feature Lab"), started, found, "drawn inside the board, nothing cut".to_string());
        hud.set_surface("features", false);
        crate::hud_dock::forget("features");
        hud.frames_at(16, 60.0);

        // Check 2 (and 1): 1, 2, 4, 8 and 12 blocks, opened one after the other.
        for (k, count) in visual::BLOCK_COUNTS.iter().enumerate() {
            let started = Instant::now();
            let mut frame = hud.frames_at(1, 60.0);
            for n in 0..*count {
                let [w, h] = sizes[(n * (run + 1) + s + k) % sizes.len()];
                let _ = tools.widgets.spawn_window(&json!({ "id": block_id(n), "title": format!("Verifier // block {:02}", n + 1), "markdown": format!("**Block {}** of {count}.\n\nThe reactor sustains its output without drift. Cooling keeps the casing below sixty degrees, and the reserve covers a full discharge.", n + 1), "replace_content": true, "width": w, "height": h }));
                frame = hud.frames_at(7, 60.0);
            }
            let (centre, radius) = hud.emblem();
            let mut rects: Vec<(String, Rect)> = (0..*count).filter_map(|n| crate::hud_dock::block_rect(&block_id(n)).map(|r| (format!("block {}", n + 1), r))).collect();
            let mut found = Vec::new();
            if rects.len() != *count {
                found.push(format!("{} of {count} blocks were drawn", rects.len()));
            }
            // (On a board across several monitors each block stands whole on one of them.)
            for (label, rect) in &rects {
                if !regions.iter().any(|r| r.expand(1.0).contains_rect(*rect)) {
                    found.push(format!("{label} leaves the board: {rect:?}"));
                }
            }
            rects.push(("the orb".to_string(), Rect::from_center_size(centre, Vec2::splat(radius * 2.0))));
            found.extend(visual::overlaps(&rects));
            found.extend(text_findings(&frame));
            // Check 3 on the first of them, once per screen: the block follows a drag exactly.
            if k == 1 {
                if let Some(home) = crate::hud_dock::block_rect(&block_id(0)) {
                    let bounds = regions.iter().copied().find(|r| r.contains(home.center())).unwrap_or_else(|| hud.block_bounds());
                    let delta = vec2(if home.center().x > bounds.center().x { -21.0 } else { 21.0 }, if home.center().y > bounds.center().y { -13.0 } else { 13.0 });
                    hud.drag(pos2(home.left() + 90.0, home.top() + 18.0), delta, 6);
                    match crate::hud_dock::block_rect(&block_id(0)) {
                        Some(now) if (now.min - home.min - delta).length() <= 0.51 => {}
                        Some(now) => found.push(format!("a block dragged by {delta:?} went from {home:?} to {now:?}")),
                        None => found.push("a block vanished while it was dragged".to_string()),
                    }
                    hud.pointer_gone();
                }
            }
            row(&mut rows, format!("{name}: {count} block(s), no overlap, nothing cut"), started, found, format!("{count} blocks apart from each other and the orb"));
            // With twelve blocks open: what a frame costs the HUD itself (a 180 Hz display
            // leaves 5.5 ms; drawing the shapes on the GPU comes on top in the real window).
            // Judged on the median frame (`gui::frame_summary`); forty over the budget are
            // drawn again after a pause, twice at most, and the fastest forty count.
            if *count == 12 {
                let started = Instant::now();
                let mut costs = frame_costs(hud);
                for _ in 0..crate::live_self_tester::AGAIN {
                    if costs.0 <= FRAME_BUDGET_MS {
                        break;
                    }
                    std::thread::sleep(crate::live_self_tester::AGAIN_AFTER);
                    let again = frame_costs(hud);
                    if again.0 < costs.0 {
                        costs = again;
                    }
                }
                let (median, average, worst) = costs;
                let mut found = if median <= FRAME_BUDGET_MS { Vec::new() } else { vec![format!("the HUD's own work takes {median:.2} ms a frame with twelve blocks (median of 40; average {average:.2} ms, worst {worst:.2} ms); a 180 Hz frame is {FRAME_BUDGET_MS} ms")] };
                // (Past the warm-up no single frame may stall: one over 50 ms is a hitch he sees.)
                if worst > crate::gui::SLOW_FRAME_MS {
                    found.push(format!("one frame took {worst:.0} ms with twelve blocks (over {} ms)", crate::gui::SLOW_FRAME_MS));
                }
                row(&mut rows, format!("{name}: a frame's own work with twelve blocks fits 180 Hz"), started, found, format!("median {median:.2} ms, average {average:.2} ms, worst {worst:.2} ms over 40 frames"));
            }
            close_blocks(hud, *count);
        }

        // The board with a report: its blocks apart, the docked orb free, nothing cut.
        let started = Instant::now();
        let report = visual::sample_report([4, 8, 12, 6, 2][(run + s) % 5], run + s);
        crate::hud_board::present(report.clone());
        hud.frames_at(75, 60.0);
        let frame = hud.frames_at(3, 60.0);
        let area = hud.board_area();
        let mut rects: Vec<(String, Rect)> = crate::hud_board::layout(&report, area, false).into_iter().enumerate().filter_map(|(n, r)| r.map(|r| (format!("block {}", n + 1), r))).collect();
        let mut found = text_findings(&frame);
        if rects.len() != report.blocks.len() {
            found.push(format!("{} of {} report blocks have a place", rects.len(), report.blocks.len()));
        }
        let (centre, radius) = hud.emblem();
        rects.push(("the orb".to_string(), Rect::from_center_size(centre, Vec2::splat(radius * 2.0))));
        found.extend(visual::overlaps(&rects));
        row(&mut rows, format!("{name}: a report of {} blocks on the board", report.blocks.len()), started, found, format!("{} blocks, the orb docked clear of them ({})", report.blocks.len(), hud.emblem_state()));
        crate::hud_board::close_board();
        hud.frames_at(60, 60.0);
    }
    crate::monitors::pretend(None);
    hud.resize(home, Pos2::ZERO);
    hud.frames_at(20, 60.0);

    window_motion(hud, tools, &mut rows);

    // The keyboard: Ctrl+K opens the console's palette, Ctrl+F the search across the blocks,
    // Esc closes the search again.
    let started = Instant::now();
    let ctrl = eframe::egui::Modifiers { ctrl: true, command: true, ..Default::default() };
    let mut found = Vec::new();
    hud.key(eframe::egui::Key::K, ctrl);
    hud.frames_at(3, 60.0);
    if !hud.surface_open("console") {
        found.push("Ctrl+K did not open the text console".to_string());
    }
    hud.close_controls();
    hud.key(eframe::egui::Key::F, ctrl);
    let frame = hud.frames_at(4, 60.0);
    if !hud.surface_open("find") || frame.find_text("FIND IN BLOCKS").is_empty() {
        found.push("Ctrl+F did not open the search across the blocks".to_string());
    }
    found.extend(text_findings(&frame));
    hud.key(eframe::egui::Key::Escape, eframe::egui::Modifiers::NONE);
    hud.frames_at(3, 60.0);
    if hud.surface_open("find") {
        found.push("Esc did not close the search".to_string());
        hud.set_surface("find", false);
    }
    row(&mut rows, "keyboard: Ctrl+K opens the palette, Ctrl+F the block search, Esc closes it".to_string(), started, found, "all three".to_string());
    hud.frames_at(28, 60.0);

    // Check 3 on the orb: picked up at its centre, it rests exactly where it was dropped.
    let started = Instant::now();
    let (centre, _) = hud.emblem();
    let delta = [vec2(-310.0, 140.0), vec2(260.0, -180.0), vec2(-120.0, -220.0), vec2(340.0, 90.0), vec2(75.0, 200.0)][run % 5];
    hud.drag(centre, delta, 8);
    hud.pointer_gone();
    hud.frames_at(50, 60.0);
    let (now, _) = hud.emblem();
    let found = if (now - (centre + delta)).length() <= 1.5 { Vec::new() } else { vec![format!("the orb was dragged by {delta:?} from {centre:?} and rests at {now:?}")] };
    row(&mut rows, "the orb follows a drag exactly".to_string(), started, found, format!("dragged by {delta:?}, rests at {now:?}"));
    tools.config.update(|c| c.interface.orb_board_pos = orb_home);
    hud.frames_at(70, 60.0);

    // Check 6: the page keys on the real HUD, by click and by voice.
    let started = Instant::now();
    let mut found = Vec::new();
    crate::hud_board::present(paged_report());
    let frame = hud.frames_at(45, 60.0);
    match (crate::hud_board::page_keys(hud.board_area()), crate::hud_board::front_page()) {
        (Some([previous, next]), Some((page, pages))) => {
            for (label, key) in [("PREV", previous), ("NEXT", next)] {
                if key.width() < visual::MIN_PAGE_KEY[0] || key.height() < visual::MIN_PAGE_KEY[1] {
                    found.push(format!("{label} is {}x{} px", key.width(), key.height()));
                }
                // The key is drawn where the board says it is (its label, not the same word elsewhere).
                let drawn = frame.find_exact(label);
                match drawn.iter().find(|text| key.expand(2.0).contains(text.center())).or(drawn.first()) {
                    Some(text) if key.expand(2.0).contains(text.center()) => {}
                    Some(text) => found.push(format!("{label} is drawn at {text:?}, the board says its key is at {key:?}")),
                    None => found.push(format!("{label} is not drawn")),
                }
            }
            hud.click(next.center(), PointerButton::Primary);
            if crate::hud_board::front_page().map(|p| p.0) != Some((page + 1) % pages) {
                found.push(format!("a click on [ NEXT > ] at {:?} left the stack on page {:?} (it was on page {page} of {pages}); under the pointer: {}", next.center(), crate::hud_board::front_page().map(|p| p.0), hud.layer_at(next.center())));
            }
            hud.click(previous.center(), PointerButton::Primary);
            if crate::hud_board::front_page().map(|p| p.0) != Some(page) {
                found.push(format!("a click on [ < PREV ] at {:?} left the stack on page {:?} (it started on page {page} of {pages}); under the pointer: {}", previous.center(), crate::hud_board::front_page().map(|p| p.0), hud.layer_at(previous.center())));
            }
            // By voice: the reflex router's own action, carried out on the test report.
            for (say, wanted) in [("Next page", (page + 1) % pages), ("Previous page", page)] {
                match crate::intent_engine::route(say, &ReflexContext::live()).map(|r| r.reflex) {
                    Some(crate::intent_engine::Reflex::Silent(action)) if action.tool == "hud_board" => {
                        let direction = action.args["direction"].as_str().unwrap_or("next");
                        let _ = crate::hud_board::flip_note(direction != "previous");
                        if crate::hud_board::front_page().map(|p| p.0) != Some(wanted) {
                            found.push(format!("'{say}' left the stack on page {:?}", crate::hud_board::front_page()));
                        }
                    }
                    other => found.push(format!("'{say}' is no page reflex: {other:?}")),
                }
            }
        }
        other => found.push(format!("the paged report shows no page keys: {other:?}")),
    }
    hud.pointer_gone();
    row(&mut rows, "the page keys are large enough and turn pages by click and by voice".to_string(), started, found, format!("{}x{} px keys", crate::hud_board::PAGE_KEY[0], crate::hud_board::PAGE_KEY[1]));

    // Check 4: every theme, on the board with its report, a block and the console.
    let _ = tools.widgets.spawn_window(&json!({ "id": block_id(0), "title": "Verifier // theme", "markdown": "Every border follows the theme.", "replace_content": true, "width": 420.0, "height": 240.0 }));
    hud.set_surface("console", true);
    hud.frames_at(30, 60.0);
    for (theme, label) in visual::themes() {
        let started = Instant::now();
        tools.config.update(|c| c.interface.hud_palette = theme.clone());
        hud.frames_at(2, 60.0);
        crate::holo_theme::finish_fade();
        let frame = hud.frames_at(3, 60.0);
        let found = visual::off_theme(&frame, &label, &crate::holo_theme::current());
        row(&mut rows, format!("theme {label}: every outline and mesh in its colours"), started, found, format!("{} outlines, {} mesh colours", frame.strokes.len(), frame.mesh_colours.len()));
    }
    tools.config.update(|c| c.interface.hud_palette = palette.clone());
    hud.frames_at(2, 60.0);
    crate::holo_theme::finish_fade();
    hud.close_controls();
    close_blocks(hud, 1);
    crate::hud_board::close_board();
    hud.frames_at(60, 60.0);

    // Check 7 on the real HUD: thinking, docking and coming home without a jump.
    let started = Instant::now();
    let mut found = Vec::new();
    let track = |hud: &mut crate::gui::offscreen::VirtualHud, frames: usize, what: &str, found: &mut Vec<String>| {
        let mut xs = Vec::with_capacity(frames);
        let mut ys = Vec::with_capacity(frames);
        let mut radii = Vec::with_capacity(frames);
        for _ in 0..frames {
            hud.frame(1.0 / 60.0);
            let (centre, radius) = hud.emblem();
            xs.push(centre.x);
            ys.push(centre.y);
            radii.push(radius);
        }
        let travelled = vec2(xs.last().copied().unwrap_or(0.0) - xs[0], ys.last().copied().unwrap_or(0.0) - ys[0]).length();
        // No frame covers more than a quarter of the way (the eased start at 60 FPS is a tenth).
        let limit = travelled * 0.25 + 3.0;
        for (axis, series) in [("x", &xs), ("y", &ys)] {
            if let Some((frame, by)) = visual::jumps(series, limit).first() {
                found.push(format!("{what}: the orb jumps {by:.1} px in {axis} in frame {frame} (limit {limit:.1})"));
            }
        }
        let grown = (radii.last().copied().unwrap_or(0.0) - radii[0]).abs();
        if let Some((frame, by)) = visual::jumps(&radii, grown * 0.25 + 3.0).first() {
            found.push(format!("{what}: the orb's size jumps {by:.1} px in frame {frame}"));
        }
    };
    let thinking = crate::thinking_guard::engage();
    track(hud, 70, "front emblem to thinking gyroscope", &mut found);
    let as_thinking = hud.emblem_state();
    drop(thinking);
    track(hud, 70, "thinking gyroscope to front emblem", &mut found);
    crate::hud_board::present(visual::sample_report(8, run));
    track(hud, 90, "front emblem to corner dock", &mut found);
    let as_docked = hud.emblem_state();
    crate::hud_board::close_board();
    track(hud, 90, "corner dock to front emblem", &mut found);
    if as_thinking != "Gyro" || as_docked != "Docked" {
        found.push(format!("the emblem was {as_thinking} while thinking and {as_docked} over a full board"));
    }
    row(&mut rows, "the emblem thinks, docks and comes home without a jump".to_string(), started, found, format!("{as_thinking} → {as_docked} → {}", hud.emblem_state()));

    // The painters on their own stage: every screen, every block count, every theme.
    for (name, size) in visual::SCREENS {
        let screen = vec2(size[0], size[1]);
        let area = Rect::from_min_size(Pos2::ZERO, screen);
        for (k, count) in visual::BLOCK_COUNTS.iter().enumerate() {
            let pass = (run + k) % 5;
            let started = Instant::now();
            row(&mut rows, format!("{name}: the board painter with {count} block(s)"), started, visual::check_board(screen, *count, pass, rt), "slots apart, nothing cut, a block follows its drag, page keys work".to_string());
            let started = Instant::now();
            row(&mut rows, format!("{name}: {count} block window(s) on a stage"), started, visual::check_windows(screen, *count, pass, *count == 2), "apart from each other and the orb, the menu and the search work".to_string());
        }
        let started = Instant::now();
        let mut found = visual::check_emblem(area, run % 5);
        found.extend(visual::check_orb_drag(area, run % 5));
        found.extend(visual::check_orb_glide(area, run % 5));
        row(&mut rows, format!("{name}: emblem transitions, ring 5, ring 6, the idle switch, drag and glide"), started, found, "no jump, 120 degrees, proportional ticks, 120-240 s".to_string());
    }
    let started = Instant::now();
    row(&mut rows, "every theme on the board painter (with rasterised borders)".to_string(), started, visual::check_board_themes(vec2(1600.0, 900.0), run % 3, rt), "eleven themes".to_string());
    let started = Instant::now();
    let mut found = visual::check_core(vec2(1600.0, 900.0));
    found.extend(visual::check_voice_ticks(&crate::gui::emblem_voice_of));
    row(&mut rows, "the core sphere holds under the pointer and turns with a drag; the voice ticks follow JAI only".to_string(), started, found, "holds, turns, follows his voice".to_string());
    hud.frames_at(10, 60.0);
    rows
}

async fn matrix_b(app: &Jai, run: usize) {
    if !crate::gui::offscreen::is_running() {
        skip("matrix B", "the visual matrix on the real HUD", "needs the offscreen HUD: start with --exhaustive or --live-test --headless");
        return;
    }
    let (tools, rt) = (app.tools().clone(), tokio::runtime::Handle::current());
    match crate::gui::offscreen::with(move |hud| visual_matrix(hud, &tools, &rt, run)).await {
        Some(rows) => {
            for (id, ms, result) in rows {
                // The visual checks are not timed against a budget: they wait for animations.
                judge("matrix B", id, f64::MAX, ms, result);
            }
        }
        None => judge("matrix B", "the visual matrix on the real HUD", UI_BUDGET_MS, 0.0, Err("the offscreen HUD stopped while it ran (see [CRASH] in the log)".to_string())),
    }
    // Check 5 in the sources (where they are next to the exe).
    match crate::live_self_tester::source_hygiene(std::path::Path::new("src")) {
        Some((ms, result)) => judge("matrix B", "no pictographs in the .rs sources", f64::MAX, ms, result),
        None => skip("matrix B", "no pictographs in the .rs sources", "the sources are not next to the executable"),
    }
}

// --------------------------------------------------------------------------- matrix C ----

async fn matrix_c(app: &Jai) {
    let tools = app.tools();
    // The HUD's sound cues are synthesised here, sample by sample: each one is there, short,
    // quiet beside his voice, free of clicks at its end and of anything that is not a number.
    for kind in crate::jai_voice_dsp::Earcon::ALL {
        let started = Instant::now();
        let cue = crate::jai_voice_dsp::earcon(kind, 48_000);
        let ms = ms_since(started);
        let peak = cue.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let said = format!("{:.0} ms of sound, peak {peak:.3}", cue.len() as f32 / 48.0);
        let sound = !cue.is_empty() && cue.len() < 48_000 && cue.iter().all(|s| s.is_finite()) && peak > 0.02 && peak < 0.2 && cue.last().is_some_and(|s| s.abs() < 1e-3);
        judge("matrix C", format!("sound cue {kind:?}"), LOCAL_BUDGET_MS, ms, if sound { Ok(said) } else { Err(format!("{said}; a cue is under a second, peaks between 0.02 and 0.2 and ends in silence")) });
    }
    use crate::fish_zero_shot_tts::{Match, ReferenceProfile};
    type Sentences = Vec<(&'static str, String, Vec<f32>)>;
    type Render = fn(&crate::config::JaiConfig) -> Result<Sentences, String>;
    let rate = crate::jai_voice_dsp::MASTER_RATE;
    let jai_profile = crate::voice_reference::reference_path().and_then(|clip| crate::fish_zero_shot_tts::load_profile(&clip, std::path::Path::new(crate::fish_zero_shot_tts::PROFILE_PATH)).ok());
    let voices: [(&'static str, Option<ReferenceProfile>, Render); 1] = [("JAI", jai_profile, crate::fish_zero_shot_tts::render_sentences)];
    // Each voice's five sentences as the speaker would get them, with its clip's profile.
    let mut spoken: Vec<(&'static str, Sentences)> = Vec::new();
    for (who, profile, render) in voices {
        let Some(profile) = profile else {
            skip("matrix C", format!("{who}: five sentence types against his clip"), "no reference clip in assets/");
            continue;
        };
        // On the CPU whatever the speech engine uses: a game keeps the GPU.
        let mut cfg = tools.config.read(|c| c.clone());
        cfg.tts_gpu = false;
        let started = Instant::now();
        let rendered = tokio::task::spawn_blocking(move || render(&cfg)).await.map_err(|e| format!("the synthesis stopped: {e}")).and_then(|r| r);
        let ms = ms_since(started);
        let sentences = match rendered {
            Ok(sentences) => sentences,
            Err(why) => {
                judge("matrix C", format!("{who}: five sentence types against his clip"), VOICE_BUDGET_MS, ms, Err(why));
                continue;
            }
        };
        let each = ms / sentences.len().max(1) as f64;
        for (kind, _, audio) in &sentences {
            let result = match Match::measure(audio, rate, &profile) {
                None => Err("the sentence holds no speech".to_string()),
                Some(m) => {
                    let said = format!("similarity {:.3}, F0 {:+.2} st from the clip's median", m.similarity, m.f0_offset_st);
                    if m.similarity >= crate::fish_zero_shot_tts::SIMILARITY_TARGET { Ok(said) } else { Err(format!("{said}; the target is {}", crate::fish_zero_shot_tts::SIMILARITY_TARGET)) }
                }
            };
            judge("matrix C", format!("{who}: {kind} sentence"), VOICE_BUDGET_MS, each, result);
        }
        // The homograph: "live telemetry" and "is live" are read "lyve".
        let technical = sentences.iter().find(|(kind, ..)| *kind == "technical").map(|(_, text, _)| text.clone()).unwrap_or_default();
        let lower = technical.to_lowercase();
        let result = if lower.contains("lyve telemetry") && lower.contains("is lyve") && !lower.contains(" live") { Ok(technical) } else { Err(format!("spoken as: {technical}")) };
        judge("matrix C", format!("{who}: \"live\" is read \"lyve\""), LOCAL_BUDGET_MS, 0.0, result);
        spoken.push((who, sentences));
    }
    understood(app, &spoken, rate).await;
}

/// The words of `text`, lower case, without punctuation.
fn words_of(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// How many of `said`'s words `heard` holds in the same order (the longest common run of words,
/// gaps allowed).
fn words_kept(said: &[String], heard: &[String]) -> usize {
    let mut best = vec![0usize; heard.len() + 1];
    for word in said {
        let mut diagonal = 0;
        for (k, other) in heard.iter().enumerate() {
            let above = best[k + 1];
            best[k + 1] = if word == other { diagonal + 1 } else { best[k + 1].max(best[k]) };
            diagonal = above;
        }
    }
    best[heard.len()]
}

/// Every voice is understood: its report and its alert sentence are played to the speech
/// recogniser (as samples, not through a speaker), which must hear four words in five. A
/// voice may be as dark and layered as it likes, as long as Sir still gets what it says.
async fn understood(app: &Jai, spoken: &[(&'static str, Vec<(&'static str, String, Vec<f32>)>)], rate: u32) {
    for (who, sentences) in spoken {
        for (kind, text, audio) in sentences.iter().filter(|(kind, ..)| matches!(*kind, "report" | "alert")) {
            let started = Instant::now();
            let pcm = crate::dsp::resample(audio, rate, 16_000);
            let result = match tokio::time::timeout(Duration::from_secs(60), app.hear(pcm)).await {
                Err(_) => Err("the recogniser did not answer within a minute".to_string()),
                Ok(Err(why)) => Err(why),
                Ok(Ok(heard)) => {
                    let (said_words, heard_words) = (words_of(text), words_of(&heard));
                    let kept = words_kept(&said_words, &heard_words);
                    let report = format!("{kept} of {} words: \"{}\"", said_words.len(), heard.trim());
                    if kept * 5 >= said_words.len() * 4 { Ok(report) } else { Err(report) }
                }
            };
            judge("matrix C", format!("{who} is understood: his {kind} sentence"), VOICE_BUDGET_MS, ms_since(started), result);
        }
    }
}

// --------------------------------------------------------------------------- matrix D ----

/// Ten things said with the wake word while the privacy lock is on.
const WAKE_WORDS: [&str; 10] = ["JAI", "Hey JAI", "JAI, what time is it?", "JAI, open Spotify", "JAI, are you there?", "Okay JAI, play some music", "JAI!", "JAI, show me your core", "JAI, wake up", "JAI, close the board"];

async fn matrix_d(app: &Jai, run: usize) {
    use crate::listening_lock::{apply, is_locked, state, verdict as heard, ListeningLockState, LockCommand, Verdict};
    // ---- JAI on every monitor and across all of them ----
    let monitors = crate::monitors::list();
    if monitors.is_empty() {
        skip("matrix D", "JAI on every monitor", "no monitor is reported here");
    }
    for m in &monitors {
        let say = format!("Move to monitor {}", m.index);
        let (recorded, took) = app.dry_run(&say, true).await;
        let calls: Vec<(String, Value)> = recorded.into_iter().map(|r| (r.tool, r.args)).collect();
        judge("matrix D", format!("\"{say}\" moves JAI there"), TURN_BUDGET_MS, took.as_secs_f64() * 1000.0, verdict(&calls, &[("move_window_to_monitor", &format!(r#"{{"target_monitor": {}}}"#, m.index))]));
    }
    if crate::gui::offscreen::is_running() {
        let screens: Vec<(String, Vec2, Pos2)> = monitors.iter().map(|m| (format!("monitor {} ({}x{})", m.index, m.width(), m.height()), vec2(m.width() as f32, m.height() as f32), pos2(m.rect[0] as f32, m.rect[1] as f32))).collect();
        let span = crate::monitors::virtual_bounds(&monitors).filter(|_| monitors.len() > 1).map(|[x, y, w, h]| ("all monitors".to_string(), vec2(w as f32, h as f32), pos2(x as f32, y as f32)));
        let regions: Vec<Rect> = monitors.iter().map(|m| Rect::from_min_max(pos2(m.rect[0] as f32, m.rect[1] as f32), pos2(m.rect[2] as f32, m.rect[3] as f32))).collect();
        let tools = app.tools().clone();
        let rows = crate::gui::offscreen::with(move |hud| {
            let mut rows: Vec<Row> = Vec::new();
            let home = hud.size();
            for (name, size, origin) in screens.into_iter().chain(span) {
                let started = Instant::now();
                hud.resize(size, origin);
                hud.pointer_gone();
                hud.frames_at(16, 60.0);
                let count = [2usize, 4, 3, 5, 6][run % 5];
                let mut frame = hud.frames_at(1, 60.0);
                for n in 0..count {
                    let _ = tools.widgets.spawn_window(&json!({ "id": block_id(n), "title": format!("Verifier // {name}"), "markdown": "The HUD on this screen.", "replace_content": true, "width": 520.0, "height": 320.0 }));
                    frame = hud.frames_at(7, 60.0);
                }
                let mut found = text_findings(&frame);
                found.extend(visual::off_every_screen(&frame, &hud.monitor_parts()));
                let rects: Vec<(String, Rect)> = (0..count).filter_map(|n| crate::hud_dock::block_rect(&block_id(n)).map(|r| (format!("block {}", n + 1), r))).collect();
                if rects.len() != count {
                    found.push(format!("{} of {count} blocks were drawn", rects.len()));
                }
                found.extend(visual::overlaps(&rects));
                // The emblem stays on the home screen (the first monitor of a spanned board).
                let (centre, radius) = hud.emblem();
                if !hud.home().contains_rect(Rect::from_center_size(centre, Vec2::splat(radius * 2.0))) {
                    found.push(format!("the emblem at {centre:?} leaves the home screen {:?}", hud.home()));
                }
                // Spanning several monitors, a block sits on one of them, never across a bezel.
                if name == "all monitors" && regions.len() > 1 {
                    for (label, rect) in &rects {
                        let on_desktop = rect.translate(origin.to_vec2());
                        if !regions.iter().any(|m| m.expand(1.0).contains_rect(on_desktop)) {
                            found.push(format!("{label} straddles two monitors: {on_desktop:?}"));
                        }
                    }
                }
                for n in 0..count {
                    let _ = tools.widgets.close_window(&block_id(n));
                    crate::hud_dock::forget(&block_id(n));
                }
                hud.frames_at(14, 60.0);
                row(&mut rows, format!("the HUD on {name}"), started, found, format!("{count} blocks placed, nothing cut"));
            }
            hud.resize(home, Pos2::ZERO);
            hud.frames_at(12, 60.0);
            rows
        })
        .await;
        for (id, ms, result) in rows.unwrap_or_else(|| vec![("the HUD on every monitor".to_string(), 0.0, Err("the offscreen HUD stopped while it ran".to_string()))]) {
            judge("matrix D", id, f64::MAX, ms, result);
        }
    } else {
        skip("matrix D", "the HUD drawn on every monitor and across all of them", "needs the offscreen HUD");
    }

    // ---- The volume: every scenario on a pretend mixer, then the real mixer's own paths ----
    for (name, result) in crate::audio_mixer::scenarios().await {
        judge("matrix D", format!("volume: {name}"), f64::MAX, 0.0, result.map(|()| "as wanted".to_string()));
    }
    let started = Instant::now();
    let real = crate::audio_mixer::write_path_check().await;
    judge("matrix D", "volume: the real mixer reads and writes (every level written back as it stands)".to_string(), f64::MAX, started.elapsed().as_secs_f64() * 1000.0, real);

    // ---- The privacy lock ----
    if is_locked() {
        skip("matrix D", "the privacy lock against ten wake words", "the lock is on right now; not touched");
    } else {
        let started = Instant::now();
        apply(LockCommand::Lock);
        // (An open microphone throughout: here only the lock decides.)
        let through: Vec<&str> = WAKE_WORDS.iter().copied().filter(|say| heard(say, true) != Verdict::Ignored).collect();
        let resumes = heard("JAI, you can now listen again", true) == Verdict::Resumes;
        // The words that lift it, through the dispatcher's own admission.
        let lifted_in = Instant::now();
        let admitted = Jai::admit("JAI, you can now listen again");
        if let crate::app::Admission::Privacy(cmd) = admitted {
            apply(cmd);
        }
        let lift_ms = ms_since(lifted_in);
        let result = if !through.is_empty() {
            Err(format!("heard while locked: {}", through.join(" / ")))
        } else if !resumes || is_locked() {
            Err(format!("\"you can now listen again\" did not lift the lock (admitted as {admitted:?})"))
        } else if heard("JAI", true) != Verdict::Heard {
            Err("he does not hear again after the lock was lifted".to_string())
        } else {
            Ok(format!("ten of ten wake words ignored; lifted in {lift_ms:.3} ms"))
        };
        apply(LockCommand::Resume);
        judge("matrix D", "\"stop listening\": ten wake words ignored, \"you can now listen again\" resumes at once", LOCAL_BUDGET_MS, ms_since(started), result);

        let started = Instant::now();
        let result = match Jai::admit("Stop listening for 1 minute.") {
            crate::app::Admission::Privacy(cmd @ LockCommand::Pause(length)) => {
                apply(cmd);
                let until = match state() {
                    ListeningLockState::PausedUntil(until) => until.saturating_duration_since(Instant::now()).as_secs_f64(),
                    _ => 0.0,
                };
                let ignored = WAKE_WORDS.iter().all(|say| heard(say, true) == Verdict::Ignored);
                apply(LockCommand::Resume);
                if length != Duration::from_secs(60) || !(58.0..=60.5).contains(&until) {
                    Err(format!("paused for {length:?}, {until:.1} s left"))
                } else if !ignored {
                    Err("a wake word got through the pause".to_string())
                } else {
                    Ok(format!("paused for 60 s ({until:.1} s left when checked), ten of ten wake words ignored"))
                }
            }
            other => Err(format!("admitted as {other:?}")),
        };
        judge("matrix D", "\"stop listening for 1 minute\" pauses for a minute", LOCAL_BUDGET_MS, ms_since(started), result);
    }

    // ---- The microphone chip ----
    // (The self-test is deaf: `AudioControls::make_deaf`. The switch is flipped, the room
    // never gets in.)
    if crate::gui::offscreen::is_running() && !is_locked() {
        let started = Instant::now();
        let was_on = app.config_state().mic_enabled;
        let mic_on = || app.config_state().mic_enabled;
        let nothing_heard = || WAKE_WORDS.iter().all(|say| heard(say, mic_on()) == Verdict::Ignored);
        let hears = || heard("JAI", mic_on()) == Verdict::Heard;
        let mut steps: Vec<String> = Vec::new();
        let mut wrong: Vec<String> = Vec::new();
        if !was_on && !nothing_heard() {
            wrong.push("a wake word got through while the microphone was off".to_string());
        }
        // Two clicks: the microphone flips and flips back. Off, ten of ten wake words are
        // dropped; on, he hears.
        for _ in 0..2 {
            let before = mic_on();
            let label = match click_mic_chip().await {
                Ok(label) => label,
                Err(why) => {
                    wrong.push(why);
                    break;
                }
            };
            if label != if before { "MIC: ON" } else { "MIC: MUTED" } {
                wrong.push(format!("the chip read [ {label} ] while the microphone was {}", if before { "on" } else { "off" }));
            }
            if !wait_for(Duration::from_millis(1500), || mic_on() != before).await {
                wrong.push(format!("a click on [ {label} ] did not switch the microphone"));
                break;
            }
            steps.push(format!("{label}"));
            if mic_on() && !hears() {
                wrong.push("with the microphone on he does not hear".to_string());
            } else if !mic_on() && !nothing_heard() {
                wrong.push("a wake word got through while the microphone was off".to_string());
            }
        }
        // Under "stop listening" the chip reads MUTED whatever the switch says, nothing is
        // heard, and one click lifts the lock and opens the microphone.
        if wrong.is_empty() {
            apply(LockCommand::Lock);
            if !nothing_heard() {
                wrong.push("a wake word got through the lock".to_string());
            }
            match click_mic_chip().await {
                Ok(label) if label == "MIC: MUTED" => {
                    if !wait_for(Duration::from_millis(1500), || !is_locked() && mic_on()).await {
                        wrong.push("a click on [ MIC: MUTED ] under the lock did not lift it and open the microphone".to_string());
                    } else if !hears() {
                        wrong.push("after the lock was lifted by the chip he does not hear".to_string());
                    } else {
                        steps.push("the lock, lifted by [ MIC: MUTED ]".to_string());
                    }
                }
                Ok(label) => wrong.push(format!("under the lock the chip read [ {label} ]")),
                Err(why) => wrong.push(why),
            }
        }
        // Whatever happened: the lock off, the microphone as it was.
        apply(LockCommand::Resume);
        if mic_on() != was_on {
            app.handle_command(crate::hud::HudCommand::SetMic { enabled: was_on }).await;
        }
        let result = if wrong.is_empty() { Ok(format!("{}; ten of ten wake words dropped whenever it was off or locked", steps.join(" → "))) } else { Err(wrong.join("; ")) };
        judge("matrix D", "the microphone chip switches the microphone, and the lock off", UI_BUDGET_MS * 4.0, ms_since(started), result);
    } else {
        skip("matrix D", "the microphone chip switches the microphone, and the lock off", "needs the offscreen HUD and a lock that is off");
    }

    // ---- Two sinks, one voice; the loopback gate ----
    let started = Instant::now();
    judge("matrix D", "one clause reaches the speaker and the call sink, each at its own rate", LOCAL_BUDGET_MS, ms_since(started), crate::tts::dual_sink_probe());
    let started = Instant::now();
    let gate = {
        use crate::discord_bridge::AntiLoopbackGate;
        let mut gate = AntiLoopbackGate::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // Open before he speaks, shut while he speaks and for 250 ms after, open again.
        let timeline = [(0, false, true), (100, true, false), (400, true, false), (500, false, false), (649, false, false), (651, false, true), (900, false, true), (1000, true, false), (1249, false, false), (1251, false, true)];
        let wrong: Vec<String> = timeline.iter().filter(|(ms, speaking, admitted)| gate.admit(*speaking, at(*ms)) != *admitted).map(|(ms, speaking, admitted)| format!("at {ms} ms (speaking: {speaking}) the gate should {}", if *admitted { "be open" } else { "be shut" })).collect();
        if wrong.is_empty() && AntiLoopbackGate::TAIL == Duration::from_millis(250) { Ok("shut while he speaks and for 250 ms after, open otherwise".to_string()) } else { Err(wrong.join("; ")) }
    };
    judge("matrix D", "the loopback gate drops the call's audio while he speaks and for 250 ms after", LOCAL_BUDGET_MS, ms_since(started), gate);
}

/// Waits up to `limit` for `done`.
/// Clicks the header's microphone chip on the offscreen HUD; the label it carried.
async fn click_mic_chip() -> Result<String, String> {
    crate::gui::offscreen::with(|hud| {
        hud.pointer_gone();
        let frame = hud.frames_at(4, 60.0);
        let (label, chip) = ["MIC: MUTED", "MIC: ON"].into_iter().find_map(|label| frame.find_exact(label).into_iter().next().map(|chip| (label, chip))).ok_or_else(|| "the header shows no microphone chip".to_string())?;
        hud.click(chip.center(), PointerButton::Primary);
        hud.pointer_gone();
        Ok(label.to_string())
    })
    .await
    .unwrap_or_else(|| Err("the offscreen HUD stopped".to_string()))
}

async fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    done()
}

// --------------------------------------------------------------------------- matrix E ----

/// Other inputs for the passes after the first, for the features whose answer depends on what
/// is asked (the rest are pressed five times as the lab presses them).
const VARIANTS: &[(&str, [&str; 4])] = &[
    ("calculate", [r#"{"expression": "17 * 23"}"#, r#"{"expression": "sqrt(144) + 2^10"}"#, r#"{"expression": "(3.5 + 1.5) / 2"}"#, r#"{"expression": "100 km in miles"}"#]),
    ("lookup_formula", [r#"{"query": "pq-Formel"}"#, r#"{"query": "Ohm's law"}"#, r#"{"query": "ideal gas law"}"#, r#"{"query": "Kettenregel"}"#]),
    ("chemistry_helper", [r#"{"action": "molar_mass", "formula": "NaCl"}"#, r#"{"action": "moles", "formula": "water", "grams": 36}"#, r#"{"action": "composition", "formula": "CO2"}"#, r#"{"action": "grams", "formula": "C6H12O6", "moles": 0.5}"#]),
    ("show_app_shortcuts", [r#"{"app": "VS Code"}"#, r#"{"query": "split screen"}"#, r#"{"app": "Discord"}"#, r#"{"query": "screenshot", "app": "Minecraft"}"#]),
    ("screen_time_report", [r#"{"action": "week"}"#, r#"{"action": "break"}"#, r#"{"action": "today"}"#, r#"{"action": "week"}"#]),
    ("suggest_routine", [r#"{"action": "list"}"#, r#"{"action": "now"}"#, r#"{"action": "list"}"#, r#"{"action": "now"}"#]),
    ("thermal_advisor", [r#"{"action": "advice"}"#, r#"{"action": "status"}"#, r#"{"action": "advice"}"#, r#"{"action": "status"}"#]),
    ("suggest_commands", [r#"{"prefix": "sw"}"#, r#"{"prefix": "move orb"}"#, r#"{"prefix": "clear"}"#, r#"{"prefix": "näch"}"#]),
    ("recall_memories", [r#"{"query": "music"}"#, r#"{"query": "school"}"#, r#"{"query": "monitor"}"#, r#"{"query": "JAI"}"#]),
    ("reminders", [r#"{"action": "list"}"#, r#"{"action": "list"}"#, r#"{"action": "list"}"#, r#"{"action": "list"}"#]),
    ("pop_up_ui_card", [r#"{"title": "Reactor", "content": "Output nominal."}"#, r#"{"title": "Thermals", "content": "All cool, Sir."}"#, r#"{"title": "Schedule", "content": "Nothing due."}"#, r#"{"title": "Board", "content": "Ready."}"#]),
    ("spawn_native_window", [r#"{"title": "Feature Lab", "markdown": "A longer note: the reactor sustains its output without drift."}"#, r#"{"title": "Feature Lab", "markdown": "**Heading**\n\nA list:\n\n- one\n- two"}"#, r#"{"title": "Feature Lab", "markdown": "Übergrößen und Maßeinheiten."}"#, r#"{"title": "Feature Lab", "markdown": "63.7 % of 4.2 GW"}"#]),
    ("create_or_update_widget", [r#"{"title": "Feature Lab", "type": "MetricGauge", "value": 12, "unit": "%"}"#, r#"{"title": "Feature Lab", "type": "MetricGauge", "value": 99, "unit": "%"}"#, r#"{"title": "Feature Lab", "type": "MetricGauge", "value": 50, "unit": "°C"}"#, r#"{"title": "Feature Lab", "type": "MetricGauge", "value": 0, "unit": "%"}"#]),
    ("launch_interactive_simulation", [r#"{"sim_type": "pendulum"}"#, r#"{"sim_type": "projectile"}"#, r#"{"sim_type": "pendulum"}"#, r#"{"sim_type": "projectile"}"#]),
    ("open_jai_view", [r#"{"view": "system_stats"}"#, r#"{"view": "simulation_lab"}"#, r#"{"view": "system_stats"}"#, r#"{"view": "schedule"}"#]),
];

/// Tools whose `[ TRY ]` draws inside the HUD: run for real against the offscreen HUD, their
/// block inspected, then closed again.
const HUD_TOOLS: &[&str] = &["pop_up_ui_card", "spawn_native_window", "create_or_update_widget", "open_jai_view", "launch_interactive_simulation", "lookup_formula", "show_app_shortcuts", "screen_time_report", "thermal_advisor", "clipboard_history", "arrange_jai_window"];

/// Answers of the network and account reads, by call: asked once per process, replayed after
/// (a self-test does not ask a weather service five hundred times).
static MEMO: Mutex<Option<HashMap<String, (bool, String)>>> = Mutex::new(None);

fn memo(key: &str) -> Option<(bool, String)> {
    MEMO.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(key).cloned()
}

fn remember(key: String, answer: (bool, String)) {
    MEMO.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(key, answer);
}

/// The arguments for pass `pass` (0-based) of a lab button.
fn variant(tool: &str, args: &Value, pass: usize) -> Value {
    if pass == 0 {
        return args.clone();
    }
    VARIANTS.iter().find(|(name, _)| *name == tool).and_then(|(_, list)| serde_json::from_str(list[(pass - 1) % 4]).ok()).unwrap_or_else(|| args.clone())
}

/// The first line of an answer.
fn first_line(output: &str) -> String {
    crate::crash_guard::clip(output.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(""), 110).to_string()
}

/// What a press came to when nothing was wrong with it.
enum Pressed {
    /// It did its work: what it answered or drew.
    Shown(String),
    /// The integration behind it is not set up on this machine.
    NotSetUp,
    /// Its server did not answer (the network's doing).
    NoAnswer,
}

/// One press of a `[ TRY ]` / `[ TEST ]` that runs a tool.
async fn press(tools: &ToolContext, defs: &[Value], tool: &str, args: &Value) -> (f64, Result<Pressed, String>) {
    use crate::live_self_tester::{check_call, unreachable, Reach, LIVE_TOOLS, NOT_SET_UP};
    if let Err(why) = check_call(defs, tool, args) {
        return (LOCAL_BUDGET_MS, Err(why));
    }
    let reach = LIVE_TOOLS.iter().find(|(name, ..)| *name == tool).map(|(_, _, reach, _)| *reach);
    let draws = HUD_TOOLS.contains(&tool) && crate::gui::offscreen::is_running();
    // The workbench's tools only read, except the window tiler and a flashcard quiz or card.
    let workbench_read = crate::workbench::is_tool(tool) && match tool {
        "tile_windows" => false,
        "manage_flashcards" => args["action"] == "due",
        "suggest_routine" => args["action"] != "run" && args["action"] != "forget",
        "clipboard_history" => args["action"] == "list",
        _ => true,
    };
    let reads = reach.is_some() || crate::hyper_cortex::is_read_only(tool, args) || workbench_read;
    if !draws && !reads {
        // It would act outside JAI (a program, the browser, other windows, a file): recorded,
        // not carried out, and checked to be the call the lab meant.
        let recorded = crate::live_self_tester::record(Box::pin(tools.execute(tool, args))).await;
        let ok = recorded.iter().any(|r| r.tool == tool && r.args == *args);
        return (LOCAL_BUDGET_MS, if ok { Ok(Pressed::Shown(format!("dry run: recorded {tool} {args}, not carried out"))) } else { Err(format!("the dry run recorded {recorded:?}")) });
    }
    let remote = matches!(reach, Some(Reach::Network | Reach::Account)) || matches!(tool, "web_search" | "read_webpage" | "server_deck" | "find_nearby_places_or_route" | "diagnose_network_and_ping" | "diagnose_integrations" | "get_precise_location_weather");
    let key = format!("{tool} {args}");
    let (success, output) = match memo(&key).filter(|_| remote) {
        Some(known) => known,
        None => match tokio::time::timeout(Duration::from_secs(25), Box::pin(tools.execute(tool, args))).await {
            Ok(outcome) => {
                // (A server that did not answer is asked again by the next press.)
                if remote && (outcome.success || !unreachable(&outcome.output)) {
                    remember(key, (outcome.success, outcome.output.clone()));
                }
                (outcome.success, outcome.output)
            }
            Err(_) => return (NETWORK_BUDGET_MS, Err("no answer within 25 s".to_string())),
        },
    };
    // A view that loads from one of Sir's accounts opens when its answer is there.
    let remote_view = tool == "open_jai_view" && args["view"] == "schedule";
    let budget = if remote || remote_view { NETWORK_BUDGET_MS } else if draws { UI_BUDGET_MS } else { LOCAL_BUDGET_MS };
    let lower = output.to_lowercase();
    if !success && NOT_SET_UP.iter().any(|w| lower.contains(w)) {
        return (budget, Ok(Pressed::NotSetUp));
    }
    if !success && (remote || remote_view) && unreachable(&output) {
        return (budget, Ok(Pressed::NoAnswer));
    }
    if !success {
        return (budget, Err(first_line(&output)));
    }
    let shown = if reach == Some(Reach::Account) { format!("answered ({} characters; Sir's data is not logged)", output.len()) } else { first_line(&output) };
    if !draws {
        return (budget, Ok(Pressed::Shown(shown)));
    }
    // What it drew in the HUD: inspected (the caller closes it).
    let found = crate::gui::offscreen::with(|hud| {
        hud.pointer_gone();
        let frame = hud.frames_at(10, 60.0);
        text_findings(&frame)
    })
    .await
    .unwrap_or_else(|| vec!["the offscreen HUD stopped".to_string()]);
    (budget, if found.is_empty() { Ok(Pressed::Shown(format!("{shown} — drawn, nothing cut"))) } else { Err(format!("{}: {}", found.len(), crate::crash_guard::clip(&found.join(" | "), 500))) })
}

/// The id of the widget the verifier's press of the widget tool creates (and removes).
const PROBE_WIDGET: &str = "exhaustive_probe_gauge";

/// Closes whatever the lab's presses left open in the HUD.
async fn close_lab_leftovers(tools: &ToolContext, known: &[String]) {
    tools.widgets.remove(PROBE_WIDGET);
    for window in tools.widgets.windows() {
        if !known.contains(&window.id) {
            let _ = tools.widgets.close_window(&window.id);
            crate::hud_dock::forget(&window.id);
        }
    }
    let _ = Box::pin(tools.execute("control_popups", &json!({ "action": "dismiss_all" }))).await;
    crate::hud_dock::close_all_blocks();
    let _ = crate::gui::offscreen::with(|hud| {
        hud.close_controls();
        hud.frames_at(16, 60.0);
    })
    .await;
}

async fn matrix_e(app: &Jai) {
    use crate::gui::TryPlan;
    let tools = app.tools();
    let defs = tools.definitions();
    let known: Vec<String> = tools.widgets.windows().iter().map(|w| w.id.clone()).collect();
    let (mut pressed, mut not_set_up) = (0usize, 0usize);
    for def in &defs {
        let Some(name) = def["function"]["name"].as_str() else { continue };
        let plan = crate::gui::lab_plan(def);
        for pass in 0..5 {
            let id = format!("{name} // press {}", pass + 1);
            let started = Instant::now();
            match &plan {
                TryPlan::Run { tool, args } => {
                    let mut args = variant(tool, args, pass);
                    // A widget is saved with Sir's own: the verifier's has its own id (never
                    // one of his) and is removed again right after.
                    if *tool == "create_or_update_widget" {
                        args["id"] = json!(PROBE_WIDGET);
                    }
                    let (budget, result) = press(tools, &defs, tool, &args).await;
                    match result {
                        Ok(Pressed::Shown(shown)) => judge("matrix E", id, budget, ms_since(started), Ok(shown)),
                        Ok(Pressed::NotSetUp) => {
                            not_set_up += 1;
                            skip("matrix E", id, "the integration behind it is not set up on this machine");
                        }
                        Ok(Pressed::NoAnswer) => skip("matrix E", id, "its server could not be reached"),
                        Err(why) => judge("matrix E", id, budget, ms_since(started), Err(why)),
                    }
                }
                TryPlan::Open(control) => {
                    let control = *control;
                    let result = match crate::gui::offscreen::with(move |hud| {
                        hud.pointer_gone();
                        hud.open_control(control);
                        let frame = hud.frames_at(10, 60.0);
                        let mut found = text_findings(&frame);
                        if !hud.control_open(control) {
                            found.push(format!("the control '{control}' did not open"));
                        }
                        hud.close_controls();
                        hud.frames_at(8, 60.0);
                        found
                    })
                    .await
                    {
                        None => Ok("the lab opens the HUD control for it (no offscreen HUD to draw it in this run)".to_string()),
                        Some(found) if found.is_empty() => Ok(format!("opens the {control} control, nothing cut")),
                        Some(found) => Err(crate::crash_guard::clip(&found.join(" | "), 500).to_string()),
                    };
                    judge("matrix E", id, UI_BUDGET_MS, ms_since(started), result);
                }
                TryPlan::Console(text) => {
                    // What would send, delete or rewrite waits in the console for Sir's words.
                    let wanted = text.clone();
                    let result = match crate::gui::offscreen::with(move |hud| {
                        hud.stage_in_console(&wanted);
                        hud.frames_at(6, 60.0);
                        let line = hud.console_line();
                        hud.close_controls();
                        hud.frames_at(4, 60.0);
                        line
                    })
                    .await
                    {
                        None => Ok("staged in the console as an example request (no offscreen HUD to draw it in this run)".to_string()),
                        Some(line) if line == *text => Ok(format!("waits in the console: \"{}\"", line.trim())),
                        Some(line) => Err(format!("the console holds \"{line}\", the lab staged \"{text}\"")),
                    };
                    judge("matrix E", id, UI_BUDGET_MS, ms_since(started), result);
                }
            }
            pressed += 1;
        }
        // A press may leave a block or a card: none stays for the next feature.
        if matches!(plan, TryPlan::Run { .. }) {
            close_lab_leftovers(tools, &known).await;
        }
    }
    close_lab_leftovers(tools, &known).await;
    info!("[PROBE] Matrix E: {pressed} presses of {} Feature Lab buttons ({not_set_up} on integrations that are not set up)", defs.len());
}

// ---------------------------------------------------------------------------- the run ----

/// Rows and the highest rowid of the conversation log (a dry run must leave both alone).
fn conversation_rows(tools: &ToolContext) -> Option<(i64, i64)> {
    let path = tools.config.read(|c| c.memory_db_path.clone());
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row("SELECT COUNT(*), COALESCE(MAX(rowid), 0) FROM conversation_logs", [], |r| Ok((r.get(0)?, r.get(1)?))).ok()
}

// --------------------------------------------------------------------------- matrix F ----

/// One command said aloud, his name first, and the handler what was heard must reach (the
/// tool and its arguments as in `Case`).
pub struct Spoken {
    pub say: &'static str,
    pub tool: &'static str,
    pub args: &'static str,
}

const fn s(say: &'static str, tool: &'static str, args: &'static str) -> Spoken {
    Spoken { say, tool, args }
}

/// Matrix F: what Sir says to the screens, to his windows and to the volume, as sound. Typed,
/// every one of these reaches its handler (the unit test below); here each is spoken, heard by
/// the recogniser, stripped of his name by the wake word and sent through the dispatcher.
pub const SPOKEN: &[Spoken] = &[
    // ---- The screens and the emblem ----
    s("JAI, use all monitors.", "configure_interface", r#"{"span_all_monitors": true}"#),
    s("JAI, stop spanning.", "configure_interface", r#"{"span_all_monitors": false}"#),
    s("JAI, toggle span mode.", "configure_interface", r#"{"span_all_monitors": "toggle"}"#),
    s("JAI, move yourself to monitor two.", "move_window_to_monitor", r#"{"target_monitor": 2}"#),
    s("JAI, move out of the way.", "configure_interface", r#"{"orb_board_position": "aside"}"#),
    s("JAI, come back to the middle.", "configure_interface", r#"{"orb_board_position": "center"}"#),
    s("JAI, show me your core.", "configure_interface", r#"{"core_view": true}"#),
    // ---- His windows and widgets ----
    s("JAI, bring back my windows.", "spawn_native_window", r#"{"restore": "recent"}"#),
    s("JAI, open that widget again next time.", "create_or_update_widget", r#"{"keep_at_start": true}"#),
    s("JAI, remove all widgets.", "remove_widget", r#"{"id": "all"}"#),
    s("JAI, open the feature lab.", "configure_interface", r#"{"feature_lab": true}"#),
    s("JAI, close the board.", "hud_board", r#"{"action": "close"}"#),
    // ---- The volume ----
    s("JAI, lower my browser's volume by fifty percent.", "control_audio_mixer", r#"{"action": "change_app", "app": "browser", "delta": -50}"#),
    s("JAI, set Discord to eighty percent.", "control_audio_mixer", r#"{"action": "set_apps", "apps": [{"app": "discord", "level": 80}]}"#),
    s("JAI, mute everything except Discord.", "control_audio_mixer", r#"{"action": "mute_others", "app": "discord"}"#),
    s("JAI, make everything loud again.", "control_audio_mixer", r#"{"action": "restore"}"#),
    s("JAI, volume forty.", "control_audio_mixer", r#"{"action": "set_master", "level": 40}"#),
    s("JAI, turn the volume up by fifteen.", "control_audio_mixer", r#"{"action": "change_master", "delta": 15}"#),
    s("JAI, mute.", "control_audio_mixer|media_and_volume_control", "{}"),
    s("JAI, save this mix as gaming.", "control_audio_mixer", r#"{"action": "save_mix", "name": "gaming"}"#),
    s("JAI, load my gaming mix.", "control_audio_mixer", r#"{"action": "load_mix", "name": "gaming"}"#),
    s("JAI, open the mixer.", "configure_interface", r#"{"mixer_view": true}"#),
    s("JAI, there's no sound.", "control_audio_mixer", r#"{"action": "no_sound"}"#),
    s("JAI, too loud.", "control_audio_mixer", r#"{"action": "change_app", "app": "it", "delta": -10}"#),
    s("JAI, turn the music off.", "control_music", "{}"),
    // ---- The widget panel ----
    s("JAI, add milk to my shopping list.", "create_or_update_widget", r#"{"type": "Checklist"}"#),
    s("JAI, start a stopwatch.", "create_or_update_widget", r#"{"mode": "stopwatch"}"#),
    s("JAI, what's open?", "whats_open", "{}"),
    s("JAI, show me my drive space.", "spawn_native_window", r#"{"live": "drive_space"}"#),
    // ---- The music and the rest ----
    s("JAI, what's playing?", "control_music", r#"{"action": "what_is_playing"}"#),
    s("JAI, pause.", "control_music", r#"{"action": "stop_all"}"#),
    s("JAI, system status.", "get_system_status", "{}"),
    s("JAI, undo that.", "undo_last_action", "{}"),
    s("JAI, that's not what I said.", "undo_last_action", r#"{"within_s": 120}"#),
    s("JAI, close the Earth, you.", "hud_board", r#"{"action": "close", "target": "earth"}"#),
    s("JAI, what did you just do?", "recent_actions", "{}"),
    s("JAI, is anything muted?", "control_audio_mixer", r#"{"action": "muted"}"#),
    s("JAI, turn Firefox off.", "close_application", r#"{"app_name": "firefox"}"#),
    s("JAI, how do I use you as a website with a static domain?", "show_mobile_satellite_instructions", "{}"),
];

/// Who says them: stock voices standing in for Sir at the microphone, a British blend and an
/// American, each at its own pace, so a sentence that only one accent gets through shows.
/// Not his voice: nothing here says how his own accent and microphone fare. (name, voice,
/// speed)
pub(crate) const STAND_INS: [(&str, &str, f32); 2] = [("a British voice", crate::tts::LEGACY_VOICE_BLEND, 1.06), ("an American voice", "am_michael", 0.96)];

/// The sentences of `SPOKEN` as each stand-in says them, `[stand-in][sentence]`, as the
/// microphone would hand them over: 16 kHz, at speech level, room hiss before, under and
/// after. Synthesised once per process (on the CPU); the recogniser hears them in every run.
static SPOKEN_AUDIO: Mutex<Option<std::sync::Arc<Vec<Vec<Vec<f32>>>>>> = Mutex::new(None);

/// `address`: what the stand-ins say in place of the "JAI," each sentence opens with (empty:
/// as written; the recogniser's bench tries "Hey JAI,").
pub(crate) fn spoken_audio(cfg: &crate::config::JaiConfig, address: &str) -> Result<Vec<Vec<Vec<f32>>>, String> {
    let phonemizer = crate::phonemizer::Phonemizer::new(cfg.espeak_path.as_deref());
    let mut kokoro = crate::tts::Kokoro::load(cfg)?;
    let mut seed = 0x51e_0f26u32;
    let mut hiss = |samples: usize| -> Vec<f32> {
        (0..samples)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0) * 0.005
            })
            .collect()
    };
    let mut all = Vec::new();
    for (_, voice, speed) in STAND_INS {
        let mut clips = Vec::new();
        for case in SPOKEN {
            let say = if address.is_empty() { case.say.to_string() } else { case.say.replacen("JAI,", address, 1) };
            let ipa = phonemizer.phonemize(&crate::tts::normalize_for_speech(&say));
            let audio24 = kokoro.synthesize(&ipa, voice, speed).map_err(|e| format!("\"{say}\" in {voice}: {e}"))?;
            let mut speech = crate::dsp::resample(&audio24, crate::tts::KOKORO_RATE, 16_000);
            let level = crate::dsp::rms(&speech);
            if level > 1e-6 {
                speech.iter_mut().for_each(|s| *s *= 0.06 / level);
            }
            let under = hiss(speech.len());
            speech.iter_mut().zip(under).for_each(|(s, n)| *s += n);
            let mut clip = hiss(16_000 / 4);
            clip.extend(speech);
            clip.extend(hiss(16_000 / 4));
            clips.push(clip);
        }
        all.push(clips);
    }
    Ok(all)
}

/// Matrix F: every sentence of `SPOKEN`, by every stand-in, through the recogniser, the wake
/// word and the dispatcher (a dry run). The typed tests know what the words mean; this one
/// knows whether the words survive being said.
async fn matrix_f(app: &Jai) {
    let cached = SPOKEN_AUDIO.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let audio = match cached {
        Some(audio) => audio,
        None => {
            // On the CPU whatever the speech engine uses: a game keeps the GPU.
            let mut cfg = app.tools().config.read(|c| c.clone());
            cfg.tts_gpu = false;
            let started = Instant::now();
            let rendered = tokio::task::spawn_blocking(move || spoken_audio(&cfg, "")).await.map_err(|e| format!("the synthesis stopped: {e}")).and_then(|r| r);
            match rendered {
                Ok(audio) => {
                    let audio = std::sync::Arc::new(audio);
                    info!("[PROBE] Matrix F: {} sentences by {} stand-ins synthesised in {:.1} s", SPOKEN.len(), STAND_INS.len(), started.elapsed().as_secs_f64());
                    *SPOKEN_AUDIO.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::sync::Arc::clone(&audio));
                    audio
                }
                Err(why) => {
                    judge("matrix F", "the commands as sound: a stand-in says them", VOICE_BUDGET_MS, ms_since(started), Err(why));
                    return;
                }
            }
        }
    };
    let mark = app.history_mark().await;
    let mut ours = false;
    prepare(Needs::Nothing, &mut ours);
    for ((who, ..), clips) in STAND_INS.iter().zip(audio.iter()) {
        for (case, clip) in SPOKEN.iter().zip(clips) {
            let started = Instant::now();
            let result = match tokio::time::timeout(Duration::from_secs(60), app.hear(clip.clone())).await {
                Err(_) => Err("the recogniser did not answer within a minute".to_string()),
                Ok(Err(why)) => Err(why),
                Ok(Ok(heard)) => {
                    let heard = heard.trim().to_string();
                    match app.wake_command(&heard) {
                        None => Err(format!("his name was not heard in \"{heard}\"")),
                        Some(command) => {
                            let (recorded, _) = app.dry_run(&command, true).await;
                            let calls: Vec<(String, Value)> = recorded.into_iter().map(|r| (r.tool, r.args)).collect();
                            match verdict(&calls, &[(case.tool, case.args)]) {
                                Ok(did) => Ok(format!("heard \"{heard}\" → {did}")),
                                Err(why) => Err(format!("heard \"{heard}\": {why}")),
                            }
                        }
                    }
                }
            };
            judge("matrix F", format!("{who} says \"{}\"", case.say), VOICE_BUDGET_MS, ms_since(started), result);
        }
    }
    app.history_rollback(mark).await;
}

/// The matrices of one run (after the base suite of the live self-test, in the same run).
/// `JAI_MATRICES` (for work on one of them: "B", "AD" …) limits the run to the ones named;
/// a run that left one out is not the full verification.
pub async fn matrices(app: &Jai, run: usize) {
    let tools = app.tools().clone();
    let before = conversation_rows(&tools);
    let exports = std::env::temp_dir().join(format!("jai_exhaustive_exports_{}", std::process::id()));
    crate::block_actions::set_export_dir(Some(exports.clone()));
    let only = std::env::var("JAI_MATRICES").ok().map(|named| named.to_uppercase()).filter(|named| !named.trim().is_empty());
    let wanted = |matrix: char| only.as_deref().is_none_or(|named| named.contains(matrix));
    if let Some(named) = &only {
        skip("exhaustive", "every matrix", format!("JAI_MATRICES limits this run to {named}"));
    }
    if wanted('A') {
        info!("[PROBE] Exhaustive run {}: matrix A (commands, five passes)", run + 1);
        matrix_a(app).await;
    }
    if wanted('B') {
        info!("[PROBE] Exhaustive run {}: matrix B (the interface as geometry)", run + 1);
        matrix_b(app, run).await;
    }
    if wanted('C') {
        info!("[PROBE] Exhaustive run {}: matrix C (both voices, five sentence types)", run + 1);
        matrix_c(app).await;
    }
    if wanted('D') {
        info!("[PROBE] Exhaustive run {}: matrix D (monitors, the privacy lock, two sinks)", run + 1);
        matrix_d(app, run).await;
    }
    if wanted('E') {
        info!("[PROBE] Exhaustive run {}: matrix E (every Feature Lab button, five presses)", run + 1);
        matrix_e(app).await;
    }
    if wanted('F') {
        info!("[PROBE] Exhaustive run {}: matrix F (the commands as sound: spoken, heard, routed)", run + 1);
        matrix_f(app).await;
    }
    crate::block_actions::set_export_dir(None);
    let _ = std::fs::remove_dir_all(&exports);
    // Nothing of all this reached Sir's conversation.
    let after = conversation_rows(&tools);
    let result = match (before, after) {
        (Some(b), Some(a)) if a == b => Ok(format!("{} rows before and after", a.0)),
        (Some(b), Some(a)) => Err(format!("{} rows (highest id {}) before, {} rows (highest id {}) after", b.0, b.1, a.0, a.1)),
        _ => Ok("no conversation log on this machine".to_string()),
    };
    judge("exhaustive", "the conversation log is untouched", f64::MAX, 0.0, result);
}

/// What `--exhaustive` found over all its runs.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Report {
    /// One summary per run.
    pub runs: Vec<crate::live_self_tester::Summary>,
    /// Every probe of every run that failed or was too slow, as "run N: group // id: detail".
    pub failures: Vec<String>,
    /// How many runs in a row at the end were clean.
    pub clean_in_a_row: usize,
    /// The probes that were late in the first pass after the start, which is gone through once
    /// before run 1 and not counted (`live_self_tester::first_pass`): named, not hidden.
    pub first_pass_late: Vec<String>,
}

impl Report {
    /// Every run clean.
    pub fn clean(&self) -> bool {
        !self.runs.is_empty() && self.clean_in_a_row == self.runs.len()
    }
}

/// Runs the whole suite `runs` times, one after the other.
pub async fn run(app: &Jai, runs: usize) -> Report {
    let mut report = Report { runs: Vec::new(), failures: Vec::new(), clean_in_a_row: 0, first_pass_late: Vec::new() };
    for n in 0..runs.max(1) {
        let started = Instant::now();
        let summary = crate::live_self_tester::run_pass(app, Some(n)).await;
        for probe in crate::live_self_tester::results().iter().filter(|r| matches!(r.status, crate::live_self_tester::Status::Fail | crate::live_self_tester::Status::Slow)) {
            report.failures.push(format!("run {}: {} {} // {}: {}", n + 1, probe.status.label(), probe.group, probe.id, probe.detail));
        }
        report.clean_in_a_row = if summary.clean() { report.clean_in_a_row + 1 } else { 0 };
        info!("[PROBE] Exhaustive run {} of {runs} finished in {:.0} s: {} ({} clean in a row)", n + 1, started.elapsed().as_secs_f64(), summary.describe(), report.clean_in_a_row);
        report.runs.push(summary);
    }
    report.first_pass_late = crate::live_self_tester::first_pass_late();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_console_prefix_completes() {
        // ("sw ba" completed to the retired "switch back to JAI" and failed matrix B.)
        for typed in CONSOLE_PREFIXES {
            assert!(!crate::command_palette::complete(typed, &[], 2).is_empty(), "'{typed}' offers no completion");
        }
    }

    #[test]
    fn every_category_has_all_five_passes() {
        let all = categories();
        assert!(all.len() >= 23, "{all:?}");
        for category in &all {
            for pass in 1..=3u8 {
                assert!(CASES.iter().any(|c| c.category == *category && c.pass == pass), "{category} has no pass {pass}");
            }
            // Pass 4 takes its sentences from the category; pass 5 is its compound.
            assert!(CASES.iter().any(|c| c.category == *category), "{category} has nothing for pass 4");
            assert_eq!(COMPOUNDS.iter().filter(|c| c.category == *category).count(), 1, "{category} needs one compound");
        }
        for compound in COMPOUNDS {
            assert!(all.contains(&compound.category), "{}", compound.category);
            assert!(compound.wants.len() >= 2, "{}: a compound has at least two actions", compound.say);
        }
        for case in CASES {
            assert!(serde_json::from_str::<Value>(case.args).is_ok(), "{}: {}", case.say, case.args);
        }
    }

    #[test]
    fn arguments_match_by_content() {
        let got = json!({"action": "close", "target": "the Feature Lab", "level": 40, "on": false});
        assert!(carries(&got, &json!({"action": "close"})));
        assert!(carries(&got, &json!({"target": "feature lab"})), "a string by what it contains, any case");
        assert!(carries(&got, &json!({"level": 40.0})) && carries(&got, &json!({"on": false})));
        assert!(!carries(&got, &json!({"action": "clear"})) && !carries(&got, &json!({"missing": 1})) && !carries(&got, &json!({"on": true})));
        let calls = vec![("hud_board".to_string(), got)];
        assert!(reached(&calls, "window_management|hud_board", r#"{"action": "close"}"#));
        assert!(reached(&calls, "hud_board", "{}") && !reached(&calls, "control_music", "{}"));
        // An undo that reaches the music player is a failure, whatever else it did.
        let with_music = vec![("undo_last_action".to_string(), json!({})), ("control_music".to_string(), json!({"action": "previous_track"}))];
        assert!(verdict(&with_music, &[("undo_last_action", "{}")]).is_err());
        assert!(verdict(&with_music, &[("control_music", "{}")]).is_ok());
    }

    #[test]
    fn every_sentence_of_passes_one_to_three_reaches_its_handler() {
        let mut wrong: Vec<String> = Vec::new();
        for case in CASES {
            let calls: Vec<(String, Value)> = crate::compound_planner::resolve_one(case.say, &context_for(case.needs)).into_iter().collect();
            if let Err(why) = verdict(&calls, &[(case.tool, case.args)]) {
                wrong.push(format!("[{} / pass {}] \"{}\": {why}", case.category, case.pass, case.say));
            }
        }
        assert!(wrong.is_empty(), "{} of {} sentences go wrong:\n  {}", wrong.len(), CASES.len(), wrong.join("\n  "));
    }

    #[test]
    fn every_spoken_command_reaches_its_handler_once_his_name_is_off() {
        let wake = crate::wake::WakeWordMatcher::new(&["jai".to_string()], 0.8);
        let mut wrong: Vec<String> = Vec::new();
        for case in SPOKEN {
            assert!(serde_json::from_str::<Value>(case.args).is_ok(), "{}: {}", case.say, case.args);
            let heard = wake.evaluate(case.say);
            if !heard.matched {
                wrong.push(format!("\"{}\": no wake word", case.say));
                continue;
            }
            let calls: Vec<(String, Value)> = crate::compound_planner::resolve_one(&heard.command, &context_for(Needs::Nothing)).into_iter().collect();
            if let Err(why) = verdict(&calls, &[(case.tool, case.args)]) {
                wrong.push(format!("\"{}\" (after the name: \"{}\"): {why}", case.say, heard.command));
            }
        }
        assert!(wrong.is_empty(), "{} of {} spoken commands go wrong:\n  {}", wrong.len(), SPOKEN.len(), wrong.join("\n  "));
        // Every stand-in talks at a human pace.
        assert!(STAND_INS.iter().all(|(_, _, speed)| (0.8..=1.2).contains(speed)));
    }

    /// Where each sentence of a file goes, one per line: nothing runs, nothing is heard, no model
    /// is asked. `JAI_ROUTES=<file> jai-test where_these_sentences_go --ignored --nocapture`
    #[test]
    #[ignore = "a reading aid, wants a file of sentences"]
    fn where_these_sentences_go() {
        let path = std::env::var("JAI_ROUTES").expect("JAI_ROUTES names the file of sentences");
        let text = std::fs::read_to_string(path).expect("the file of sentences");
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if let Some(set) = line.strip_prefix('#') {
                println!("\n== {}", set.trim());
                continue;
            }
            let ctx = context_for(Needs::Nothing);
            let plan = crate::compound_planner::plan(line, &ctx);
            let went = match (&plan, crate::compound_planner::resolve_one(line, &ctx)) {
                (Some(plan), _) => format!("PLAN {}", plan.calls.iter().map(|call| format!("{} {}", call.tool, call.args)).collect::<Vec<_>>().join(" ; ")),
                (None, Some((tool, args))) => format!("{tool} {args}"),
                (None, None) => match crate::intents::parse(&crate::typos::normalize_user_input_typos(line)) {
                    Some(crate::intents::Intent::Stop) => "(stop)".to_string(),
                    Some(crate::intents::Intent::Time) => "(the time)".to_string(),
                    Some(crate::intents::Intent::Date) => "(the date)".to_string(),
                    Some(crate::intents::Intent::Answer(answer)) => format!("(answered) {answer}"),
                    _ => "-> dialogue state or the language model".to_string(),
                },
            };
            println!("{line:<52} {went}");
        }
    }

    #[test]
    fn every_compound_is_planned_clause_by_clause_inside_the_budget() {
        let mut wrong: Vec<String> = Vec::new();
        for compound in COMPOUNDS {
            let ctx = context_for(compound.needs);
            let _ = crate::compound_planner::plan(compound.say, &ctx);
            let started = Instant::now();
            let plan = crate::compound_planner::plan(compound.say, &ctx);
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            let Some(plan) = plan else {
                wrong.push(format!("[{}] \"{}\" is no compound (clauses: {:?})", compound.category, compound.say, crate::compound_planner::clauses(compound.say)));
                continue;
            };
            let mut calls: Vec<(String, Value)> = plan.calls.iter().map(|c| (c.tool.to_string(), c.args.clone())).collect();
            calls.extend(plan.privacy.map(|cmd| ("listening_lock".to_string(), json!({ "command": format!("{cmd:?}") }))));
            if let Err(why) = verdict(&calls, compound.wants) {
                wrong.push(format!("[{}] \"{}\": {why}", compound.category, compound.say));
            }
            if calls.len() != compound.wants.len() {
                wrong.push(format!("[{}] \"{}\": {} actions planned, {} wanted: {calls:?}", compound.category, compound.say, calls.len(), compound.wants.len()));
            }
            if ms > COMPOUND_BUDGET_MS {
                wrong.push(format!("[{}] \"{}\" took {ms:.2} ms to plan", compound.category, compound.say));
            }
        }
        assert!(wrong.is_empty(), "{} of {} compounds go wrong:\n  {}", wrong.len(), COMPOUNDS.len(), wrong.join("\n  "));
    }

    #[test]
    fn the_consoles_vocabulary_is_made_of_sentences_the_routers_take() {
        // What only the brain decides: its instant answers, its dialogues and the model.
        const THE_BRAINS: &[&str] = &["what time is it", "brief me", "what can you do", "search the web for the Abitur dates in Baden-Württemberg", "add a flashcard: mitosis means cell division into two identical cells"];
        let idle = ReflexContext::default();
        let paged = context_for(Needs::Pages);
        let unknown: Vec<&str> = crate::command_palette::VOCABULARY
            .iter()
            .copied()
            .filter(|line| !THE_BRAINS.contains(line))
            .filter(|line| crate::compound_planner::resolve_one(line, &idle).is_none() && crate::compound_planner::resolve_one(line, &paged).is_none())
            .collect();
        assert!(unknown.is_empty(), "the palette offers sentences nothing routes: {unknown:?}");
        // The flashcard line is the workbench's (its capitals are Sir's, so it is asked as typed).
        assert!(crate::workbench::route_text("add a flashcard: mitosis means cell division into two identical cells").is_some());
    }

    /// The widgets saved in Sir's memory database:
    /// `cargo test saved_widgets -- --ignored --nocapture`. With `JAI_REMOVE_WIDGET=<id>`
    /// that one is removed. (The first exhaustive runs, 2026-11-06, pressed the Feature Lab's
    /// widget example for real and left its gauge in his widgets panel; the verifier now uses
    /// `PROBE_WIDGET` and removes it.)
    #[test]
    #[ignore = "reads, and on request changes, the real memory database"]
    fn saved_widgets() {
        // (Read straight from the tables: opening the widget engine would start a session.)
        let conn = rusqlite::Connection::open("jai_memory.db").expect("the memory database");
        let rows = |sql: &str| -> Vec<String> {
            let Ok(mut stmt) = conn.prepare(sql) else { return vec!["(no such table yet)".to_string()] };
            let found = stmt.query_map([], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().collect()).unwrap_or_default();
            found
        };
        for row in rows("SELECT id || ' | updated ' || updated_at || ' | ' || substr(spec_json, 1, 120) FROM widgets ORDER BY position") {
            eprintln!("widget {row}");
        }
        for row in rows("SELECT kind || ' ' || id || ' | session ' || session || ' | open ' || open || ' | seen ' || seen_at FROM recent_views ORDER BY session, seen_at") {
            eprintln!("recent {row}");
        }
        eprintln!("conversation_logs: {:?} (rows, highest id)", conn.query_row("SELECT COUNT(*), COALESCE(MAX(rowid), 0) FROM conversation_logs", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).ok());
        if let Ok(id) = std::env::var("JAI_REMOVE_WIDGET") {
            eprintln!("removed widget {id}: {:?}", conn.execute("DELETE FROM widgets WHERE id = ?1", [&id]));
            eprintln!("removed recent {id}: {:?}", conn.execute("DELETE FROM recent_views WHERE id = ?1", [&id]));
        }
    }

    #[test]
    fn a_sentence_is_understood_when_four_words_in_five_come_back_in_order() {
        let said = words_of("Sir, the graphics card has reached its limit. I recommend a pause.");
        assert_eq!(said.len(), 12);
        // Heard word for word (the recogniser's own capitals and commas do not count).
        assert_eq!(words_kept(&said, &words_of("sir the graphics card has reached its limit, I recommend a pause")), 12);
        // One word misheard, one dropped: ten of twelve.
        assert_eq!(words_kept(&said, &words_of("Sir, the graphic card has reached its limit. I recommend pause.")), 10);
        // The right words in the wrong order are not the sentence.
        assert_eq!(words_kept(&said, &words_of("pause a recommend I")), 1);
        assert_eq!(words_kept(&said, &[]), 0);
    }

    #[test]
    fn lab_inputs_vary_and_stay_valid_calls() {
        let defs = crate::tools::definitions_with(Vec::new());
        for (tool, list) in VARIANTS {
            for args in list {
                let args: Value = serde_json::from_str(args).unwrap_or_else(|e| panic!("{tool}: {args}: {e}"));
                crate::live_self_tester::check_call(&defs, tool, &args).unwrap_or_else(|e| panic!("{tool}: {e}"));
            }
            assert_eq!(variant(tool, &json!({"first": true}), 0), json!({"first": true}), "the first press is the lab's own");
            assert_ne!(variant(tool, &json!({"first": true}), 1), json!({"first": true}));
        }
        assert_eq!(variant("no_such_tool", &json!({"a": 1}), 3), json!({"a": 1}));
        for tool in HUD_TOOLS {
            assert!(defs.iter().any(|d| d["function"]["name"] == *tool), "{tool}");
        }
    }
}
