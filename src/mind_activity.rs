//! Which parts of J.A.I. are working right now. Code that runs says so (`pulse("stt.rs")`,
//! `chain(&["ears.rs", "stt.rs", "brain.rs"])`); the complex core lights the layer of his
//! mind that part belongs to, and the core view names the parts as they work.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// How long a part stays lit after it last ran.
pub const GLOW_SECS: f32 = 3.0;

/// The layers of his mind, inside out: the complex core's four shells.
pub const LAYERS: [&str; 4] = ["VOICE", "LANGUAGE", "TOOLS", "MEMORY"];

/// The layer a source file belongs to: 0 hearing and speaking, 1 understanding and deciding,
/// 2 acting (tools, the screen, the services he reaches), 3 remembering and the machine.
pub fn layer(file: &str) -> usize {
    let stem = file.trim_end_matches(".rs");
    let has = |words: &[&str]| words.iter().any(|w| stem.contains(w));
    if has(&["ears", "stt", "tts", "vad", "wake", "phonem", "pronunc", "voice", "audio", "dsp", "acoustic", "modality", "fish", "speaker"]) {
        0
    } else if matches!(stem, "main" | "app" | "brain" | "ear" | "ways" | "typos" | "later" | "english") || has(&["intent", "context", "runtime", "llm_reboot", "thinking_guard", "persona", "anti_repeat", "hyper_cortex", "phrasings", "did_you_mean", "command_corrector", "compound", "ask_first"]) {
        1
    } else if has(&["memory", "config", "vault", "backup", "telemetry", "vram", "platform", "shell", "file", "disk", "clipboard", "crash", "diagnostic", "lifecycle", "undo"]) {
        3
    } else {
        2
    }
}

#[derive(Default)]
struct Bus {
    last: HashMap<&'static str, Instant>,
}

static BUS: LazyLock<Mutex<Bus>> = LazyLock::new(|| Mutex::new(Bus::default()));

fn bus() -> std::sync::MutexGuard<'static, Bus> {
    BUS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Marks `file` as running now.
pub fn pulse(file: &'static str) {
    bus().last.insert(file, Instant::now());
    crate::gui::refresh();
}

/// Parts handing work to each other ("ears.rs", "stt.rs", "brain.rs").
pub fn chain(files: &[&'static str]) {
    {
        let mut bus = bus();
        let now = Instant::now();
        for file in files {
            bus.last.insert(file, now);
        }
    }
    crate::gui::refresh();
}

fn freshness(at: Instant, now: Instant) -> f32 {
    (1.0 - now.saturating_duration_since(at).as_secs_f32() / GLOW_SECS).clamp(0.0, 1.0)
}

/// How hard each layer works at `now` (0-1): its most recent part, fading over [`GLOW_SECS`].
pub fn layers(now: Instant) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for (file, at) in &bus().last {
        let l = layer(file);
        out[l] = out[l].max(freshness(*at, now));
    }
    out
}

/// The parts that ran within [`GLOW_SECS`] of `now`, freshest first: (file, layer, 1 just
/// now … 0 fading out).
pub fn recent(now: Instant) -> Vec<(&'static str, usize, f32)> {
    let mut out: Vec<_> = bus().last.iter().map(|(file, at)| (*file, layer(file), freshness(*at, now))).filter(|(.., f)| *f > 0.0).collect();
    out.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(b.0)));
    out
}

/// Whether anything is still lit (the HUD keeps drawing meanwhile).
pub fn active(now: Instant) -> bool {
    bus().last.values().any(|at| freshness(*at, now) > 0.0)
}

/// The part a tool runs in (so a tool call lights the right one).
pub fn module_for_tool(tool: &str) -> Option<&'static str> {
    let t = tool.to_lowercase();
    let has = |w: &[&str]| w.iter().any(|x| t.contains(x));
    Some(if has(&["music", "playlist", "track", "sonora", "spotify"]) {
        "music.rs"
    } else if has(&["volume", "mixer", "mute"]) {
        "audio_mixer.rs"
    } else if has(&["hud_board", "board"]) {
        "hud_board.rs"
    } else if has(&["school", "webuntis", "timetable"]) {
        "webuntis_client.rs"
    } else if has(&["course", "itslearning", "homework"]) {
        "itslearning_bw_sso.rs"
    } else if has(&["calendar", "event"]) {
        "calendar_client.rs"
    } else if has(&["email", "mail"]) {
        "email_client.rs"
    } else if has(&["whatsapp", "message"]) {
        "whatsapp_client.rs"
    } else if has(&["web_search", "search_web"]) {
        "websearch.rs"
    } else if has(&["vram", "gpu"]) {
        "vram_guard.rs"
    } else if has(&["forge"]) {
        "tool_forge.rs"
    } else if has(&["research"]) {
        "deep_research_engine.rs"
    } else if has(&["reboot", "language_model"]) {
        "llm_reboot.rs"
    } else if has(&["interface", "configure", "window_style", "gui"]) {
        "interface.rs"
    } else if has(&["window", "focus"]) {
        "winctl.rs"
    } else if has(&["file", "folder"]) {
        "file_ops.rs"
    } else if has(&["memory", "remember"]) {
        "memory.rs"
    } else if has(&["reminder", "timer", "alarm"]) {
        "reminders.rs"
    } else if has(&["replay", "clip"]) {
        "instant_replay.rs"
    } else if has(&["widget"]) {
        "widgets.rs"
    } else {
        return None;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn every_part_has_its_layer() {
        for (file, l) in [("ears.rs", 0), ("stt.rs", 0), ("tts.rs", 0), ("jai_voice_dsp.rs", 0), ("brain.rs", 1), ("intents.rs", 1), ("runtime.rs", 1), ("ways.rs", 1), ("tools.rs", 2), ("music.rs", 2), ("hud_board.rs", 2), ("webuntis_client.rs", 2), ("memory.rs", 3), ("config.rs", 3), ("vram_guard.rs", 3), ("file_ops.rs", 3)] {
            assert_eq!(layer(file), l, "{file}");
        }
    }

    #[test]
    fn a_part_lights_its_layer_and_fades() {
        let now = Instant::now();
        let mut bus = Bus::default();
        bus.last.insert("stt.rs", now);
        bus.last.insert("memory.rs", now - Duration::from_millis(1500));
        bus.last.insert("music.rs", now - Duration::from_secs(10));
        let fresh = |file: &str| freshness(bus.last[file], now);
        assert_eq!(fresh("stt.rs"), 1.0);
        assert!((fresh("memory.rs") - 0.5).abs() < 0.01);
        assert_eq!(fresh("music.rs"), 0.0);
    }

    #[test]
    fn a_tool_lights_the_part_it_runs_in() {
        assert_eq!(module_for_tool("play_music"), Some("music.rs"));
        assert_eq!(module_for_tool("configure_interface"), Some("interface.rs"));
        assert_eq!(module_for_tool("say_hello"), None);
    }
}
