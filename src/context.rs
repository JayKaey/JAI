//! The live-system-context block injected into every LLM turn, so JAI can resolve "close
//! that window", "move it to the left" or "put that in a widget": how Sir is talking to him, the
//! reply mode, the foreground window and running apps, JAI's own windows and widgets, and
//! memories relevant to what Sir just said.

use crate::modality::{InputSource, VoiceGate};
use crate::tools::ToolContext;
use std::collections::BTreeSet;

pub struct Desktop {
    /// Title and process of the window Sir is working in (never JAI's own).
    pub foreground: Option<(String, String)>,
    /// Distinct processes with visible windows.
    pub apps: Vec<String>,
}

#[cfg(windows)]
pub async fn desktop() -> Desktop {
    crate::win32::call(|| {
        let foreground = crate::win32::user_window().map(|w| (w.title, w.process));
        let apps: BTreeSet<String> = crate::win32::app_windows()
            .into_iter()
            .map(|w| w.process)
            .filter(|p| !p.is_empty() && !matches!(p.as_str(), "jarvis" | "applicationframehost" | "textinputhost" | "explorer"))
            .collect();
        Desktop { foreground, apps: apps.into_iter().take(16).collect() }
    })
    .await
    .unwrap_or(Desktop { foreground: None, apps: Vec::new() })
}

#[cfg(not(windows))]
pub async fn desktop() -> Desktop {
    tokio::task::spawn_blocking(|| {
        let run = |p: &str, a: &[&str]| {
            std::process::Command::new(p).args(a).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        };
        if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
            let active = run("hyprctl", &["activewindow", "-j"]).and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok());
            let clients = run("hyprctl", &["clients", "-j"]).and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok());
            let apps: BTreeSet<String> = clients
                .as_ref()
                .and_then(|c| c.as_array())
                .map(|a| a.iter().filter_map(|c| c["class"].as_str().map(|s| s.to_lowercase())).collect())
                .unwrap_or_default();
            return Desktop {
                foreground: active.map(|a| (a["title"].as_str().unwrap_or("").to_string(), a["class"].as_str().unwrap_or("").to_string())),
                apps: apps.into_iter().take(16).collect(),
            };
        }
        let title = run("xdotool", &["getactivewindow", "getwindowname"]).map(|t| t.trim().to_string());
        let apps: BTreeSet<String> = run("wmctrl", &["-lx"])
            .map(|l| l.lines().filter_map(|line| line.split_whitespace().nth(2).map(|c| c.split('.').next().unwrap_or(c).to_lowercase())).collect())
            .unwrap_or_default();
        Desktop { foreground: title.map(|t| (t, String::new())), apps: apps.into_iter().take(16).collect() }
    })
    .await
    .unwrap_or(Desktop { foreground: None, apps: Vec::new() })
}

/// The block for one turn (built once per turn; it is not stored in the history). `carry` =
/// a follow-up: the conversation focus and the working memory of earlier turns are included; a
/// new, self-contained request gets the live state only.
pub async fn build(tools: &ToolContext, source: InputSource, voice: VoiceGate, request: &str, earlier: Option<String>, carry: bool) -> String {
    let now = chrono::Local::now();
    let mode = tools.modality.get();
    let delivery = match voice {
        VoiceGate::Speak => "this reply is spoken aloud and shown in the chat",
        VoiceGate::Silent => "this reply is shown as text in the chat only (Sir will not hear it); plain sentences, short lists allowed",
        VoiceGate::SpeakIfAlert => "this reply is text only unless it is an alert",
    };
    // The desktop and the media session: read ahead while Sir was still speaking
    // (`hyper_cortex::prefetch_context`), else side by side now (the media session answers in
    // milliseconds, but is never allowed to hold the turn up).
    let fusing = std::time::Instant::now();
    let (desk, playing, ahead) = match crate::hyper_cortex::take_prefetched() {
        Some(p) => (p.desktop, p.music, true),
        None => {
            let (desk, playing) = tokio::join!(desktop(), tokio::time::timeout(std::time::Duration::from_millis(300), crate::music::now_playing()));
            (desk, playing.ok().flatten().map(|np| np.describe()), false)
        }
    };
    // The window Sir was working in before this request (captured at the start of the turn); the
    // raw foreground is JAI's own HUD whenever Sir typed into it.
    let foreground = tools.focus.foreground().map(|w| (w.title, w.process)).or(desk.foreground.clone());
    let (windows, widgets) = tools.widgets.describe();
    // The core facts about Sir (per turn here, so the cached system prompt never changes) plus
    // the ones relevant to this request.
    let core = tools.memory.core_for_prompt(tools.config.read(|c| c.core_memories_in_prompt));
    let mut memories = core;
    for m in tools.memory.recall(Some(request), None, 5).unwrap_or_default() {
        if !memories.iter().any(|k| k.key == m.key) {
            memories.push(m);
        }
    }
    // Plain headings: a bracketed block invites the model to write bracketed notes of its own.
    let mut out = String::from("LIVE SYSTEM CONTEXT (private notes for you; never read, quote or mention them):\n");
    out.push_str(&format!("Time: {}\n", now.format("%A %-d %B %Y, %H:%M")));
    out.push_str(&format!("Reply language: {}\n", language_rule(request)));
    out.push_str(&format!(
        "Input: {}. Response mode: {}{} → {delivery}.\n",
        source.label(),
        mode.display(),
        if tools.modality.voice_muted() { " (voice muted)" } else { "" }
    ));
    match &foreground {
        Some((title, process)) if !title.is_empty() => out.push_str(&format!(
            "Sir's window (before this request): \"{}\"{}\n",
            title.chars().take(90).collect::<String>(),
            if process.is_empty() { String::new() } else { format!(" ({process})") }
        )),
        _ => out.push_str("Sir's window (before this request): desktop\n"),
    }
    if !desk.apps.is_empty() {
        out.push_str(&format!("Open apps: {}\n", desk.apps.join(", ")));
    }
    out.push_str(&crate::monitors::context_line(&crate::monitors::list()));
    out.push('\n');
    if let Some(music) = playing {
        out.push_str(&format!("Music: {music}\n"));
    }
    if carry {
        out.push_str(&tools.focus.describe());
        out.push('\n');
    }
    out.push_str(&format!("JAI windows: {windows}\nWidgets: {widgets}\n"));
    // The HUD board: what is on screen, block by block (Sir's questions about it are answered from
    // these figures), with the blocks the question is about pulsing.
    if crate::hud_board::is_active() {
        crate::hud_board::focus_for_question(request);
    }
    out.push_str(&crate::hud_board::live_context());
    // Sir's Discord call while the bridge listens (what the others said, never Sir).
    out.push_str(&crate::discord_bridge::live_context());
    // JAI's own Discord account: the call he is in, unread messages; Away Mode; watches.
    out.push_str(&crate::discord_bot::live_context());
    out.push_str(&crate::away_mode::live_context());
    out.push_str(&crate::watch_and_tell::live_context());
    let reminders = tools.reminders.list();
    if !reminders.is_empty() {
        let list: Vec<String> = reminders
            .iter()
            .take(6)
            .map(|r| format!("#{} {} \"{}\"", r.id, crate::reminders::spoken_when(r.due(), now), r.text))
            .collect();
        out.push_str(&format!("Pending reminders: {}\n", list.join("; ")));
    }
    if let Some(ms) = tools.protocols.focus_remaining_ms() {
        out.push_str(&format!("Focus mode: active, {} min left\n", ms / 60_000 + 1));
    }
    out.push_str(&format!(
        "Protocols: {}\n",
        tools.protocols.list().iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
    ));
    // Working memory (people, artifacts, location), WhatsApp, simulations, macros, nodes.
    if carry {
        out.push_str(&tools.stark.live_context());
    }
    if let Some(summary) = earlier.filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("Earlier in this conversation: {summary}\n"));
    }
    if !memories.is_empty() {
        out.push_str("What you know about Sir:\n");
        for m in memories {
            out.push_str(&format!("- {}: {}\n", m.key, m.value));
        }
    }
    out.push_str("END OF LIVE SYSTEM CONTEXT");
    tracing::debug!("[CORTEX] Live context fused in {} ms ({})", fusing.elapsed().as_millis(), if ahead { "desktop and media read ahead" } else { "read now" });
    out
}

/// The language rule of this turn: British English, whatever language Sir used, unless he asked
/// for German himself.
pub fn language_rule(request: &str) -> &'static str {
    if crate::brain::german_requested(request) {
        "German, because Sir explicitly asked for it in this message"
    } else if crate::brain::looks_german(request) {
        "British English only. Sir spoke German or Denglisch: understand it, but answer in English"
    } else {
        "British English only"
    }
}
