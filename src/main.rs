// A desktop app, never a console program: no terminal window opens, whichever way Jarvis is
// started. `--console` attaches one on request (tests keep the console subsystem).
#![cfg_attr(not(test), windows_subsystem = "windows")]
#![recursion_limit = "256"]

//! J.A.R.V.I.S. — local, offline, low-latency voice assistant.
//!
//! Pipeline: microphone → 16 kHz → Silero VAD state machine → Whisper → wake word →
//! instant-command fast path or streaming LLM (built-in engine) with tools → clause splitter →
//! Kokoro TTS → speaker, with barge-in at every stage. UI: a native egui HUD window (no browser)
//! that Jarvis restyles on request, plus a tray icon.

mod acoustic_radar;
mod aegis_server_deck;
mod airdrop_bridge;
mod announcements;
mod anti_repeat;
mod app;
mod app_icon;
mod audio_tags;
mod appindex;
mod audio_mixer;
mod block_actions;
mod board_research;
mod apps;
mod brain;
mod briefing;
mod calc;
mod calendar_client;
mod camera_feed;
mod change_notifier;
mod chem_helper;
mod clipboard;
mod clipboard_history;
mod command_palette;
mod commute_advisor;
mod compound_planner;
mod config;
mod contacts;
mod context;
mod context_graph;
mod core_sphere_3d;
mod crash_guard;
mod deal_sniper;
mod deep_research_engine;
mod deep_researcher;
mod diagnostics;
mod disk_map;
mod earth;
mod shadow_memory;
mod streamdeck_bridge;
mod discord_bridge;
mod wave_link;
mod dsp;
mod dynamic_popups;
mod ears;
mod email_client;
mod exam_coach;
mod exhaustive_scenario_verifier;
mod features;
mod fish_zero_shot_tts;
mod file_janitor;
mod file_ops;
mod finance_tracker;
mod flashcards;
mod focus;
mod formula_sheet;
mod guardian;
mod gui;
mod hivemind_mesh;
mod hud;
mod hud_dock;
mod hud_board;
mod hud_suggestions;
mod hyper_cortex;
mod instant_replay;
mod integration_registry;
mod integration_router;
mod intent_engine;
mod intents;
mod itslearning_bw_sso;
mod interface;
mod jarvis_voice_dsp;
mod jarvis_views;
mod lifecycle;
mod live_self_tester;
mod listening_lock;
mod llm_reboot;
mod lms_client;
mod location_engine;
mod macros;
mod media;
mod media_grabber;
mod memory;
mod minecraft_doctor;
mod mobile_scanner;
mod modality;
mod models;
mod monitors;
mod music;
mod network_guardian;
mod network_radar;
mod oled_minimal_ui;
mod omni_inbox;
mod persona;
mod phonemizer;
mod pim;
mod pim_config;
mod platform;
mod pronunciation_lexicon;
mod proactive_butler;
mod project_architect;
mod protocols;
mod reminders;
mod remote_guardian;
mod routine_learner;
mod research_graph_view;
mod rose_emblem_renderer;
mod runtime;
mod sandbox_runner;
mod satellite;
mod screen;
mod screen_context;
mod screen_time;
mod self_improve;
mod selftest;
mod shell;
mod shortcut_overlay;
mod simulation_lab;
mod sites;
mod spatial_scenes;
mod skills;
mod smart_alarm;
mod stark;
mod stt;
mod stress_tester;
mod study_assistant;
mod system_power;
mod teleport;
mod telemetry;
mod theme_engine;
mod thermal_advisor;
mod ultron_mode;
mod thinking_guard;
mod tool_forge;
mod tools;
mod tray;
mod tts;
mod ui_snapshot;
mod typos;
mod audio_mixes;
mod release_notes;
mod undo;
mod vad;
mod vault;
mod venue_guide;
mod visual_ui_verifier;
mod voice_features;
mod voice_reference;
mod vram_guard;
mod wake;
mod web_connector;
mod websearch;
mod webuntis_client;
mod whatsapp_client;
mod widgets;
mod window_drill;
mod window_physics;
mod window_tiler;
mod workbench;
mod workout_coach;
mod workspace_canvas;
mod worldwide_tunnel;
// Checks against the real PC (mixer, power, give-way, voices): ignored unless asked for.
#[cfg(test)]
mod live_checks;
#[cfg(windows)]
mod win32;
mod winctl;

use crate::config::{ConfigStore, JarvisConfig};
use crate::tray::TrayAction;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

struct Cli {
    headless: bool,
    hidden: bool,
    console: bool,
    check_models: bool,
    download_models: bool,
    selftest: bool,
    speak: bool,
    bench: bool,
    /// With --selftest: stop before the language-model stage (no GPU load, no second engine).
    no_llm: bool,
    /// Started by Jarvis himself (restart): wait for the old instance to let go.
    restarted: bool,
    /// Typed requests to run once Jarvis is up (diagnostics, scripted demos).
    chat: Vec<String>,
    /// Pause after each scripted request's answer.
    chat_delay: Duration,
    /// Start with the microphone muted (scripted runs must not answer the room).
    no_mic: bool,
    /// `--silent`: a scripted `--chat` session nobody sees, hears or finds in the history. The
    /// voice is at 0, the HUD is drawn offscreen, the microphone is deaf, the settings file and
    /// the conversation log stay as they are; every answer is written to jarvis.log (`[CLI]`),
    /// and Jarvis quits after the last request.
    silent: bool,
    /// `--show` with `--silent`: the session runs in the real HUD window (what a monitor
    /// change or a span really does can only be seen there).
    show: bool,
    /// `--window-drill` with `--silent --show`: the real window's SCREENS buttons and F11
    /// driven through its message queue and checked against the monitors (`window_drill`).
    window_drill: bool,
    /// Test every integration and print the report, then exit.
    diagnose: bool,
    /// Pick the Kokoro blend closest to the reference clip (silent), then exit.
    voice_calibrate: bool,
    /// Synthesise the clip's sentence and measure it against the clip (silent), then exit.
    voice_verify: bool,
    /// `--voice-calibrate ultron` / `--voice-verify ultron`: Ultron's voice alone (Jarvis' own
    /// calibration is not touched).
    voice_ultron_only: bool,
    /// Check that Jarvis' voice reaches Sir's Discord microphone (plays one sentence), then exit.
    call_check: bool,
    /// Start Jarvis, run the live self-test in the running app (silent), write the report, exit.
    live_test: bool,
    /// `--exhaustive [runs]`: the live self-test and the exhaustive matrices, that many runs in
    /// a row, with the HUD drawn offscreen.
    exhaustive: Option<usize>,
    /// `--monitor <n>`: a test session's window opens on monitor n (Sir plays on the primary).
    monitor: Option<usize>,
}

impl Cli {
    fn parse() -> Result<Self, String> {
        let mut cli = Cli {
            headless: false,
            hidden: false,
            console: false,
            check_models: false,
            download_models: false,
            selftest: false,
            voice_calibrate: false,
            voice_verify: false,
            voice_ultron_only: false,
            call_check: false,
            speak: false,
            bench: false,
            no_llm: false,
            restarted: false,
            chat: Vec::new(),
            chat_delay: Duration::from_secs(2),
            no_mic: false,
            silent: false,
            show: false,
            window_drill: false,
            diagnose: false,
            live_test: false,
            exhaustive: None,
            monitor: None,
        };
        let mut args = std::env::args().skip(1).peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--headless" => cli.headless = true,
                // `--no-browser` is what older shortcuts pass.
                "--hidden" | "--tray" | "--minimized" | "--no-browser" => cli.hidden = true,
                "--console" => cli.console = true,
                "--check-models" => cli.check_models = true,
                "--download-models" => cli.download_models = true,
                "--selftest" => cli.selftest = true,
                "--voice-calibrate" => {
                    cli.voice_calibrate = true;
                    cli.voice_ultron_only |= args.next_if(|next| next == "ultron").is_some();
                }
                "--voice-verify" => {
                    cli.voice_verify = true;
                    cli.voice_ultron_only |= args.next_if(|next| next == "ultron").is_some();
                }
                "--call-check" => cli.call_check = true,
                "--speak" => cli.speak = true,
                "--bench" => cli.bench = true,
                "--no-llm" => cli.no_llm = true,
                "--restarted" => cli.restarted = true,
                "--no-mic" => cli.no_mic = true,
                "--silent" => cli.silent = true,
                "--show" => cli.show = true,
                "--window-drill" => cli.window_drill = true,
                "--diagnose" => cli.diagnose = true,
                "--live-test" => cli.live_test = true,
                "--exhaustive" => {
                    // An optional number of runs follows.
                    let runs = args.next_if(|next| next.parse::<usize>().is_ok()).and_then(|n| n.parse().ok());
                    cli.exhaustive = Some(runs.unwrap_or(exhaustive_scenario_verifier::DEFAULT_RUNS).clamp(1, 50));
                }
                "--monitor" => {
                    let n = args.next().and_then(|s| s.parse::<usize>().ok()).filter(|n| *n >= 1);
                    cli.monitor = Some(n.ok_or("--monitor needs a monitor number (1 = the primary)")?);
                }
                "--chat" => cli.chat.push(args.next().ok_or("--chat needs a message")?),
                "--chat-delay" => {
                    let secs: f64 = args.next().and_then(|s| s.parse().ok()).ok_or("--chat-delay needs seconds")?;
                    cli.chat_delay = Duration::from_secs_f64(secs.clamp(0.0, 600.0));
                }
                "-h" | "--help" => return Err(String::new()),
                other => return Err(format!("unknown argument '{other}'")),
            }
        }
        // The exhaustive verifier is the live self-test with the HUD drawn offscreen.
        if cli.exhaustive.is_some() {
            cli.live_test = true;
            cli.headless = true;
        }
        // The self-test must not answer the room while it runs.
        cli.no_mic |= cli.live_test;
        // A silent session has no window and no ears either.
        if cli.silent {
            if cli.chat.is_empty() {
                return Err("--silent needs at least one --chat request".into());
            }
            // (`--show`: with the real window, for what only a window shows.)
            cli.headless = !cli.show;
            cli.no_mic = true;
        }
        // The drill drives a real window, and only a silent session's (its settings stay in memory).
        if cli.window_drill && !(cli.silent && cli.show) {
            return Err("--window-drill needs --silent --show and one --chat request".into());
        }
        // Where a test window opens is that session's alone: its settings are never saved.
        if cli.monitor.is_some() && !(cli.silent || cli.live_test) {
            return Err("--monitor is for test sessions (--silent --show or --live-test), whose settings are never saved".into());
        }
        Ok(cli)
    }

    /// One-shot diagnostics that print and exit (no tray icon, no single-instance lock).
    fn is_diagnostic(&self) -> bool {
        self.check_models || self.download_models || self.selftest || self.diagnose || self.voice_calibrate || self.voice_verify || self.call_check
    }
}

const HELP: &str = "J.A.R.V.I.S. — local voice assistant

USAGE: JARVIS.exe [OPTIONS]

  (no options)        start Jarvis with his HUD window (closing it keeps him in the tray)
  --hidden            start in the tray only (click the tray icon for the HUD)
  --headless          voice only, no window (e.g. a Raspberry Pi without a display)
  --console           show the live log in a console window
  --check-models      check the speech models and the built-in language engine, then exit
  --download-models   install anything missing (speech models and language model), then exit
  --selftest          offline pipeline test (TTS → VAD → Whisper → wake word → intents → LLM)
  --speak             with --selftest: also play a test sentence on the speakers
  --bench             with --selftest: time Whisper across settings (threads, encoder window, ...)
  --no-llm            with --selftest: skip the language-model stage (while gaming / Jarvis runs)
  --voice-calibrate   pick the Kokoro blend closest to the reference clip (silent);
                      `--voice-calibrate ultron`: Ultron's base voice alone
  --voice-verify      measure the voice chain against the reference clip (silent);
                      `--voice-verify ultron`: Ultron's voice alone
  --call-check        check that Jarvis' voice reaches your Discord mic (Wave Link Chat Mix; plays one sentence)
  --chat \"<text>\"     type a request into Jarvis once he is up (repeatable)
  --chat-delay <secs> pause after each --chat answer (default 2), e.g. to look at a new window
  --no-mic            start with the microphone muted (unmute in the HUD)
  --silent            with --chat: a session nobody sees or hears (voice at 0, the HUD drawn
                      offscreen, settings file and conversation log untouched); every answer
                      goes to jarvis.log as [CLI] lines, then Jarvis quits; with --show the
                      session runs in the real HUD window instead of offscreen
  --window-drill      with --silent --show: click the SCREENS buttons and press F11 in the real
                      window (posted messages, your mouse untouched), check where Windows put it
                      on every monitor ([DRILL] lines in jarvis.log), then quit
  --monitor <n>       with --silent --show or --live-test: the test window opens on monitor n
                      (1 = the primary), this session only; e.g. --monitor 2 while you play
  --diagnose         test every integration (WebUntis, calendar, e-mail, WhatsApp …) and exit
  --live-test         start Jarvis, run every probe in the running app (routing, tools, HUD; silent,
                      nothing is sent or changed), write live_selftest_report.json and exit;
                      with --headless the HUD is drawn offscreen (no window at all)
  --exhaustive [n]    the live self-test plus the exhaustive matrices (every command family in five
                      passes, the interface inspected as geometry on four screen sizes, both voices,
                      the privacy lock, every Feature Lab button), n runs in a row (default 5), no
                      window, no sound, the settings file untouched; writes exhaustive_report.json

ENV: JARVIS_LOG (e.g. jarvis=debug), JARVIS_PROFILE, OLLAMA_URL (external engine)";

/// Finds the project folder (models/, runtime/) even when started from target\release.
fn ensure_working_directory() {
    let is_home = |p: &Path| p.join("models").is_dir() || p.join("runtime").is_dir() || p.join(config::CONFIG_FILE).is_file();
    if std::env::current_dir().map(|d| is_home(&d)).unwrap_or(false) {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        while let Some(d) = dir {
            if is_home(d) {
                let _ = std::env::set_current_dir(d);
                return;
            }
            dir = d.parent();
        }
    }
}

struct LocalTimer;

impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%H:%M:%S%.3f"))
    }
}

fn enable_ansi() -> bool {
    std::io::stdout().is_terminal() && enable_vt_mode()
}

/// Windows consoles need virtual-terminal processing switched on for ANSI colours.
#[cfg(windows)]
fn enable_vt_mode() -> bool {
    use windows::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE,
    };
    // SAFETY: querying/setting the mode of this process' own stdout console handle.
    unsafe {
        let Ok(handle) = GetStdHandle(STD_OUTPUT_HANDLE) else { return false };
        let mut mode = CONSOLE_MODE(0);
        GetConsoleMode(handle, &mut mode).is_ok() && SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING).is_ok()
    }
}

#[cfg(not(windows))]
fn enable_vt_mode() -> bool {
    true
}

fn init_logging(log_file: &str) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_env("JARVIS_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("jarvis=info,whisper_rs=warn,ort=warn"));

    // Simple size-based rotation: keep one previous log.
    if std::fs::metadata(log_file).map(|m| m.len() > 10 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(log_file, format!("{log_file}.1"));
    }
    let file_layer = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .ok()
        .map(|f| {
            fmt::layer()
                .with_ansi(false)
                .with_timer(LocalTimer)
                .with_target(false)
                .with_writer(std::sync::Mutex::new(f))
        });
    let console_layer = fmt::layer().with_ansi(enable_ansi()).with_timer(LocalTimer).with_target(false);
    tracing_subscriber::registry()
        .with(filter)
        .with(console_layer)
        .with(file_layer)
        .init();

    std::panic::set_hook(Box::new(|info| {
        error!("[PANIC] {info}\n{}", std::backtrace::Backtrace::force_capture());
    }));
}

fn main() -> ExitCode {
    let parsed = Cli::parse();
    match &parsed {
        Ok(cli) if cli.console => platform::open_console(),
        Ok(cli) if cli.is_diagnostic() => platform::attach_parent_console(),
        Ok(_) => {}
        Err(_) => platform::attach_parent_console(),
    }
    let cli = match parsed {
        Ok(c) => c,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("error: {msg}\n");
            }
            println!("{HELP}");
            return if msg.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(2) };
        }
    };
    ensure_working_directory();

    // Starting Jarvis again only brings the running instance's HUD to the front. After a restart
    // the previous instance may still be finishing its shutdown, so wait for it. A test session
    // runs where nobody sees it: it gives way to the Jarvis Sir starts himself.
    let instance = if cli.is_diagnostic() {
        None
    } else {
        let mut deadline = std::time::Instant::now() + Duration::from_secs(if cli.restarted { 30 } else { 0 });
        let mut asked = cli.live_test || cli.silent;
        loop {
            match platform::SingleInstance::acquire() {
                Some(guard) => break Some(guard),
                None if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(250)),
                None if !asked && platform::test_session_running() && tray::ask_test_session_to_give_way() => {
                    asked = true;
                    deadline = std::time::Instant::now() + Duration::from_secs(30);
                }
                None => {
                    tray::show_running_instance();
                    return ExitCode::SUCCESS;
                }
            }
        }
    };

    init_logging("jarvis.log");
    whisper_rs::install_whisper_tracing_trampoline();

    // A self-test (or a silent scripted session) leaves the settings file as it is from the
    // first read on (loading writes the file back otherwise): whatever is switched, is switched
    // in memory.
    if cli.live_test || cli.silent {
        config::set_read_only(true);
    }
    let mut cfg = JarvisConfig::load();
    if cli.live_test || cli.silent {
        // ... and it makes no sound.
        cfg.loudness_pct = 0;
        cfg.startup_greeting = false;
        platform::mark_test_session();
        // `--monitor`: its window opens on that monitor only (not across all of them), so it
        // stays off the one Sir is playing on.
        if let Some(n) = cli.monitor {
            cfg.interface.span_all_monitors = false;
            cfg.interface.orb_monitor = u8::try_from(n).unwrap_or(u8::MAX);
            gui::start_on_monitor(n);
        }
    }
    if cli.silent {
        // What a script asks is not part of Sir's conversations, and what it sets off stays
        // inside Jarvis (`tools::stays_inside`).
        memory::set_off_the_record(true);
        tools::set_silent_session(true);
    }
    if cli.window_drill {
        window_drill::enable();
    }
    let runtime = match crash_guard::runtime_builder().build() {
        Ok(rt) => rt,
        Err(e) => {
            platform::alert(&format!("Could not start the async runtime: {e}"));
            return ExitCode::FAILURE;
        }
    };
    // Boxed: the start-up future is large, and on the main thread's stack (with the calls it makes)
    // it overflowed it.
    let session = match runtime.block_on(Box::pin(start(&cli, cfg))) {
        Started::Running(session) => session,
        Started::Done(code) => return code,
    };
    let hud = if cli.headless || !platform::has_display() {
        if cli.live_test || cli.silent {
            // The self-test still has a HUD to test (and a silent session one to work in): the
            // real one, drawn offscreen at the main monitor's size.
            let size = monitors::list().iter().find(|m| m.primary).map_or([2560.0, 1440.0], |m| [m.width() as f32, m.height() as f32]);
            gui::offscreen::start(session.app.clone(), runtime.handle().clone(), size.into());
        } else {
            info!("[HUD] Headless: no window, voice and tray only.");
        }
        None
    } else {
        Some(gui::Hud::attach(session.app.clone(), runtime.handle().clone()))
    };
    // Scripted --chat runs stay silent (Sir may be listening to music or in a call).
    if session.greet && cli.chat.is_empty() && !cli.live_test {
        let _rt = runtime.enter();
        let line = session.app.startup_greeting(cli.restarted);
        session.app.speak_standalone(&line, false, modality::Trigger::System);
    }
    // The Stream Deck / macro-key bridge on 127.0.0.1:19840 (token-guarded, localhost only).
    {
        let _rt = runtime.enter();
        streamdeck_bridge::start(session.app.clone());
    }
    if !cli.chat.is_empty() {
        let (app, messages, delay, silent) = (session.app.clone(), cli.chat.clone(), cli.chat_delay, cli.silent);
        runtime.spawn(async move {
            // Give Whisper, the TTS voice and the language engine a moment to load.
            tokio::time::sleep(Duration::from_secs(4)).await;
            if silent {
                // Nothing of a silent session reaches the PC around Jarvis: its volume
                // requests work on a copy of the mixer, its other outward tools are noted.
                audio_mixer::work_on_a_copy().await;
            }
            let mut events = app.subscribe();
            for text in messages {
                info!("[CLI] --chat \"{text}\"");
                let asked = std::time::Instant::now();
                app.handle_command(hud::HudCommand::Chat { text }).await;
                // One request at a time: wait for the answer (spoken or written) to finish.
                let deadline = std::time::Instant::now() + Duration::from_secs(if silent { 240 } else { 90 });
                tokio::time::sleep(Duration::from_millis(500)).await;
                while app.busy() && std::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                tokio::time::sleep(delay).await;
                // A silent session leaves its transcript in the log: what he did and said.
                while let Ok(event) = events.try_recv() {
                    match event {
                        hud::HudEvent::AssistantDone { text, model, latency_ms, interrupted, .. } if silent => {
                            info!("[CLI] reply ({model}, first token {latency_ms} ms{}): \"{}\"", if interrupted { ", interrupted" } else { "" }, text.replace('\n', " "));
                        }
                        hud::HudEvent::ToolLog { tool, result, success, duration_ms, .. } if silent => {
                            info!("[CLI] tool {tool} {} in {duration_ms} ms: {}", if success { "ok" } else { "FAILED" }, crash_guard::clip(&result.replace('\n', " "), 700));
                        }
                        hud::HudEvent::QuickAction { label, ok } if silent => info!("[CLI] quick action '{label}' {}", if ok { "ok" } else { "FAILED" }),
                        hud::HudEvent::Notice { message } if silent => info!("[CLI] notice: {message}"),
                        hud::HudEvent::Error { message } if silent => info!("[CLI] error shown: {message}"),
                        _ => {}
                    }
                }
                if silent {
                    info!("[CLI] turn took {} ms{}", asked.elapsed().saturating_sub(delay).as_millis(), if app.busy() { " and is STILL BUSY" } else { "" });
                }
            }
            if silent {
                // `--window-drill`: the session lasts until the drill is through.
                let until = std::time::Instant::now() + Duration::from_secs(300);
                while window_drill::running() && std::time::Instant::now() < until {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                app.handle_command(hud::HudCommand::Shutdown).await;
            }
        });
    }
    // Every command path is run once as a dry run, so the first real command of each kind does
    // not pay for compiling its patterns.
    {
        let app = session.app.clone();
        runtime.spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            live_self_tester::warm_up(&app).await;
        });
    }
    if cli.live_test {
        // A run cut short (the tray's Quit, Sir starting Jarvis) proves nothing: until it has
        // finished, the exit code says it failed.
        LIVE_TEST_FAILED.store(true, Ordering::Release);
        let (app, runs) = (session.app.clone(), cli.exhaustive);
        runtime.spawn(async move {
            // Whisper, the voice and the HUD's first frames.
            tokio::time::sleep(Duration::from_secs(5)).await;
            let failed = match runs {
                Some(runs) => {
                    let report = exhaustive_scenario_verifier::run(&app, runs).await;
                    let json = serde_json::to_string_pretty(&serde_json::json!({ "exhaustive": report, "last_run": serde_json::from_str::<serde_json::Value>(&live_self_tester::report_json()).unwrap_or_default() })).unwrap_or_default();
                    if let Err(e) = std::fs::write(EXHAUSTIVE_REPORT, json) {
                        warn!("[PROBE] Could not write {EXHAUSTIVE_REPORT}: {e}");
                    }
                    if report.clean() {
                        info!("[PROBE] Exhaustive verifier: {} of {} runs clean", report.clean_in_a_row, report.runs.len());
                    } else {
                        error!("[PROBE] Exhaustive verifier: {} failing or slow probe(s) over {} runs; {} clean in a row at the end", report.failures.len(), report.runs.len(), report.clean_in_a_row);
                    }
                    !report.clean()
                }
                None => {
                    let summary = live_self_tester::run(&app).await;
                    if let Err(e) = std::fs::write(LIVE_TEST_REPORT, live_self_tester::report_json()) {
                        warn!("[PROBE] Could not write {LIVE_TEST_REPORT}: {e}");
                    }
                    !summary.clean()
                }
            };
            LIVE_TEST_FAILED.store(failed, Ordering::Release);
            app.handle_command(hud::HudCommand::Shutdown).await;
        });
    }
    if let Some(hud) = hud {
        // The window system wants the main thread; the HUD keeps it until Jarvis quits.
        hud.run(session.show_hud && !cli.hidden);
    }
    // The shutdown sequence (voice, HUD, tray or Ctrl+C) also ends the HUD.
    if let Err(e) = runtime.block_on(session.finished) {
        error!("Shutdown sequence failed: {e}");
    }
    // Audio/STT/TTS threads end with the process; don't wait on blocking tasks.
    runtime.shutdown_timeout(Duration::from_secs(2));
    drop(instance);
    // "Restart yourself" / a finished self-rebuild: the next instance waits for this one.
    lifecycle::spawn_pending_restart();
    info!("J.A.R.V.I.S. offline.");
    // `--live-test`: the exit code says whether every probe passed.
    std::process::exit(i32::from(LIVE_TEST_FAILED.load(Ordering::Acquire)))
}

/// Where `--live-test` writes its report.
const LIVE_TEST_REPORT: &str = "live_selftest_report.json";
/// Where `--exhaustive` writes its report (every run's tally, every failure, the last run's probes).
const EXHAUSTIVE_REPORT: &str = "exhaustive_report.json";
/// `--live-test` found a failing or slow probe.
static LIVE_TEST_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct Session {
    app: app::Jarvis,
    /// Completes once the shutdown sequence has run.
    finished: tokio::task::JoinHandle<()>,
    show_hud: bool,
    greet: bool,
}

enum Started {
    Running(Session),
    /// A diagnostic ran, or start-up failed.
    Done(ExitCode),
}

async fn start(cli: &Cli, cfg: JarvisConfig) -> Started {
    let engine = match runtime::engine_binary(Path::new(&cfg.llm_runtime_dir)) {
        Some(_) => format!("built-in engine ({})", cfg.llm_runtime_dir),
        None => format!("external Ollama at {}", cfg.ollama_url),
    };
    info!("═══════════════════════════════════════════════════════════════");
    info!(" J.A.R.V.I.S. v{} — {} ({})", env!("CARGO_PKG_VERSION"), cfg.hardware_profile.display_name(), std::env::consts::OS);
    info!(" LLM {} (fallback {}) via the {engine}", cfg.primary_model, cfg.fallback_model);
    info!(
        " VAD start {:.2} / release {:.2}, noise ×{:.1}, silence {} ms (+{} ms for unfinished thoughts, stitching {}), max {} s",
        cfg.vad_start_threshold,
        cfg.vad_release_threshold,
        cfg.vad_noise_multiplier,
        cfg.silence_hangover_ms,
        cfg.incomplete_thought_wait_ms,
        if cfg.utterance_stitching { "on" } else { "off" },
        cfg.max_utterance_secs
    );
    info!("═══════════════════════════════════════════════════════════════");

    if cli.check_models {
        return Started::Done(if models::report(&cfg) { ExitCode::SUCCESS } else { ExitCode::FAILURE });
    }
    if cli.diagnose {
        return Started::Done(integration_registry::cli(&cfg).await);
    }
    if cli.download_models || cfg.auto_download_models {
        let still_missing = models::ensure(&cfg).await;
        if cli.download_models {
            // The language model is installed by the engine itself (local import or download).
            let llm = runtime::LlmRuntime::start(&cfg);
            let llm_ready = llm.settled().await;
            llm.shutdown();
            return Started::Done(if still_missing.is_empty() && llm_ready && models::report(&cfg) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
    }
    if !models::report(&cfg) {
        warn!("[MODELS] Some models are missing; Jarvis installs what it can (or run `JARVIS.exe --download-models`).");
    }
    if cli.selftest {
        return Started::Done(selftest::run(&cfg, cli.speak, cli.bench, !cli.no_llm).await);
    }
    if cli.call_check {
        return Started::Done(discord_bridge::call_check(&cfg).await);
    }
    if cli.voice_calibrate || cli.voice_verify {
        fish_zero_shot_tts::set_f0_flatten(cfg.voice_f0_flatten);
        // Ultron has his own clip: his base voice is calibrated and measured against it too.
        let ultron = ultron_mode::reference_path().is_some();
        let jarvis = !cli.voice_ultron_only;
        if cli.voice_calibrate {
            if jarvis {
                match fish_zero_shot_tts::calibrate(&cfg) {
                    Ok((blend, m)) => info!("[VOICE] Calibrated: {blend} — {} (distance {:.3})", m.describe(), m.distance()),
                    Err(e) => {
                        error!("[VOICE] Calibration failed: {e}");
                        return Started::Done(ExitCode::FAILURE);
                    }
                }
            }
            if ultron {
                match ultron_mode::calibrate(&cfg) {
                    Ok((voice, m)) => info!("[ULTRON] Calibrated: {voice} — {} (distance {:.3})", m.describe(), m.distance()),
                    Err(e) => {
                        error!("[ULTRON] Calibration failed: {e}");
                        return Started::Done(ExitCode::FAILURE);
                    }
                }
            }
        }
        if cli.voice_verify {
            let mut reached = true;
            if jarvis {
                match fish_zero_shot_tts::verify(&cfg) {
                    Ok(v) => {
                        info!("[VOICE] Verification\n{}", v.describe());
                        reached &= v.passed();
                    }
                    Err(e) => {
                        error!("[VOICE] Verification failed: {e}");
                        reached = false;
                    }
                }
            }
            if ultron {
                match ultron_mode::verify(&cfg) {
                    Ok(v) => {
                        info!("[ULTRON] Verification\n{}", v.describe());
                        reached &= v.passed();
                    }
                    Err(e) => {
                        error!("[ULTRON] Verification failed: {e}");
                        reached = false;
                    }
                }
            }
            return Started::Done(if reached { ExitCode::SUCCESS } else { ExitCode::FAILURE });
        }
        return Started::Done(ExitCode::SUCCESS);
    }

    // The language engine starts first so it loads in parallel with Whisper and Kokoro.
    let llm = runtime::LlmRuntime::start(&cfg);
    let shutdown = CancellationToken::new();

    let store = ConfigStore::new(cfg.clone());
    store.spawn_autosave();

    let memory = match memory::MemoryManager::open(&cfg.memory_db_path) {
        Ok(m) => m,
        Err(e) => {
            platform::alert(&format!("J.A.R.V.I.S. cannot open its memory database {}: {e}", cfg.memory_db_path));
            return Started::Done(ExitCode::FAILURE);
        }
    };
    let skills = match skills::SkillManager::open(&cfg.memory_db_path, &cfg.skills_dir) {
        Ok(s) => s,
        Err(e) => {
            platform::alert(&format!("J.A.R.V.I.S. cannot open its skill library: {e}"));
            return Started::Done(ExitCode::FAILURE);
        }
    };
    // Self-forged native tools live next to the skills (`skills/forged_tools`).
    tool_forge::init(std::path::Path::new(&cfg.memory_db_path), std::path::Path::new(&cfg.skills_dir));
    let modality = match modality::ModalityStore::open(&cfg.memory_db_path) {
        Ok(m) => m,
        Err(e) => {
            warn!("[MODE] Reply mode cannot be persisted ({e}); using Mirror Input for this session.");
            modality::ModalityStore::in_memory()
        }
    };
    // A session of Sir's starts clean and remembers what he opens ("bring back my windows").
    // A self-test or a scripted one-off (`--chat`) is no session of his: it shows what he kept
    // for the start and leaves his recent windows as they are.
    let widgets = match if cli.live_test || !cli.chat.is_empty() { widgets::WidgetEngine::open_untouched(&cfg.memory_db_path) } else { widgets::WidgetEngine::open(&cfg.memory_db_path) } {
        Ok(w) => w,
        Err(e) => {
            warn!("[WIDGET] Widgets cannot be persisted ({e}); they last for this session only.");
            widgets::WidgetEngine::in_memory()
        }
    };
    let reminders = match reminders::ReminderStore::open(&cfg.memory_db_path, widgets.clone()) {
        Ok(r) => r,
        Err(e) => {
            warn!("[REMIND] Reminders cannot be persisted ({e}); they last for this session only.");
            reminders::ReminderStore::in_memory()
        }
    };
    reminders.start_runtime();
    let protocols = match protocols::ProtocolStore::open(&cfg.memory_db_path) {
        Ok(p) => p,
        Err(e) => {
            warn!("[PROTOCOL] Protocols cannot be persisted ({e}); using the built-in ones.");
            protocols::ProtocolStore::in_memory()
        }
    };
    let monitor = telemetry::SystemMonitor::start(cfg.hardware_profile);
    widgets.start_runtime(monitor.clone());
    // "Open <anything installed>" resolves against this index; "find my …" against the file index.
    appindex::start();
    file_ops::start();
    let controls = Arc::new(ears::AudioControls::new(&cfg));
    if cli.no_mic {
        controls.mic_enabled.store(false, std::sync::atomic::Ordering::Release);
        info!("[MIC] Started with the microphone muted (--no-mic).");
    }
    if cli.live_test || cli.silent {
        // The self-test clicks the microphone's switch; what is said in the room meanwhile
        // must never become a command.
        controls.make_deaf();
    }

    // The film-voice reference clip (if present) is analysed while the voice loads.
    fish_zero_shot_tts::set_f0_flatten(cfg.voice_f0_flatten);
    fish_zero_shot_tts::preload();
    let (tts_tx, tts_rx) = tokio::sync::mpsc::unbounded_channel();
    let tts = tts::TtsController::start(&cfg, Arc::clone(&controls), tts_tx);
    let stt = stt::SttEngine::start(&cfg);
    let pim = pim::Pim::open(&cfg.memory_db_path, store.clone(), Some(Arc::clone(&llm)));
    let tools = tools::ToolContext {
        memory,
        skills,
        monitor,
        voice_volume: tts.volume_handle(),
        config: store.clone(),
        modality,
        widgets,
        reminders,
        protocols,
        voice_speed: tts.speed_handle(),
        focus: focus::FocusTracker::new(),
        music: music::MusicEngine::open(&cfg.memory_db_path).unwrap_or_else(|e| {
            warn!("[MUSIC] Music aliases cannot be persisted ({e}); using the library only.");
            music::MusicEngine::in_memory()
        }),
        workout: match workout_coach::WorkoutCoach::open(&cfg.memory_db_path) {
            Ok(coach) => coach,
            Err(e) => {
                platform::alert(&format!("Cannot persist workout records: {e}"));
                return Started::Done(ExitCode::FAILURE);
            }
        },
        pim: pim.clone(),
        stark: stark::StarkSystems::open(&cfg.memory_db_path),
    };
    let stark_tools = tools.clone();
    let brain = brain::Brain::new(store.clone(), tools, Arc::clone(&llm));
    // Zero-VRAM standby: idle or gaming evicts every model; a hotkey or a word wakes Jarvis.
    {
        let (flushed, woken, rt) = (brain.clone(), brain.clone(), tokio::runtime::Handle::current());
        vram_guard::start(
            &cfg,
            Arc::clone(&controls),
            Arc::clone(&llm),
            move || flushed.forget_primed_prefix(),
            move || {
                let brain = woken.clone();
                rt.spawn(async move { brain.prime().await });
            },
        );
    }
    // Multi-hour research missions (interrupted ones resume) and the spatial mission profiles.
    deep_research_engine::start(&cfg.memory_db_path, stark_tools.clone());
    spatial_scenes::init(&cfg.memory_db_path);

    let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel();
    let app = app::Jarvis::start(
        store.clone(),
        Arc::clone(&controls),
        brain.clone(),
        stt,
        tts,
        app::Channels { audio: audio_rx, tts: tts_rx },
        shutdown.clone(),
    );
    ears::spawn(&cfg, Arc::clone(&controls), audio_tx);
    // itslearning keep-alive heartbeat and the saved change watchers.
    pim.start_background();
    // WhatsApp listener + auto-responder, and the mobile satellite (phone GPS, chat).
    stark_tools.stark.start_background(&stark_tools, store.clone());
    satellite::start(app.clone(), shutdown.clone());
    remote_guardian::start(app.clone(), shutdown.clone());
    // What his own jobs have to say once done (the radar's sweep, the disk map's scan), spoken
    // as soon as he is free.
    {
        let (speaker, stop) = (app.clone(), shutdown.clone());
        tokio::spawn(async move {
            while !stop.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(400)).await;
                for line in announcements::take() {
                    speaker.raise_alert(line);
                }
            }
        });
    }

    // `[ ⟳ REBOOT LLM ]` and "reboot your language model" reach the engine, the brain and the turn.
    {
        let (interrupting, noticing, saying) = (app.clone(), app.clone(), app.clone());
        llm_reboot::install(llm_reboot::RebootContext {
            llm: Arc::clone(&llm),
            brain: brain.clone(),
            config: store.clone(),
            interrupt: Box::new(move || interrupting.interrupt("LLM reboot")),
            notice: Box::new(move |m| noticing.notice(m)),
            say: Box::new(move |line| saying.speak_standalone(line, false, modality::Trigger::Alert)),
        });
    }
    // Load the model and cache the prompt prefix as soon as the engine is up (the live self-test
    // asks the model nothing, so it leaves the GPU alone).
    if !cli.live_test {
        let (warm, engine) = (brain.clone(), Arc::clone(&llm));
        tokio::spawn(async move {
            if engine.settled().await {
                warm.prime().await;
            }
        });
    }
    // Regex compilation for the fast path / speech filters, off the critical path.
    tokio::task::spawn_blocking(|| {
        intents::warm_up();
        let _ = tts::normalize_for_speech("Warm-up: 5 GB at 40°C, e.g. J.A.R.V.I.S.");
        let _ = shell::blocked_reason("echo warm-up");
    });

    let (tray_tx, tray_rx) = tokio::sync::mpsc::unbounded_channel();
    let mic = Arc::clone(&controls);
    tray::spawn(tray_tx, move || mic.mic_enabled.load(Ordering::Relaxed));
    spawn_tray_handler(tray_rx, app.clone(), shutdown.clone());

    info!("[READY] Say \"Jarvis …\". Quit by voice (\"Jarvis, shut down\"), from the tray icon or the HUD.");
    let finished = tokio::spawn(shutdown_sequence(app.clone(), store, llm, shutdown));
    Started::Running(Session { app, finished, show_hud: cfg.open_ui_on_start, greet: cfg.startup_greeting })
}

/// Waits for the quit signal (voice, HUD power button, tray, Ctrl+C), then shuts down in order.
async fn shutdown_sequence(app: app::Jarvis, store: ConfigStore, llm: Arc<runtime::LlmRuntime>, shutdown: CancellationToken) {
    tokio::select! {
        _ = shutdown.cancelled() => {}
        result = tokio::signal::ctrl_c() => {
            if let Err(e) = result {
                warn!("Could not listen for Ctrl+C ({e}); quit from the tray icon or the HUD.");
                shutdown.cancelled().await;
            }
        }
    }
    shutdown.cancel();
    info!("Shutting down — goodbye, Sir.");
    app.announce_shutdown();
    gui::remember_placement(&store);
    store.save_now();
    tray::remove();
    // Lets the HUD show that Jarvis is going offline.
    tokio::time::sleep(Duration::from_millis(250)).await;
    llm.shutdown();
    gui::quit();
    // Safety net: a window that cannot be closed must not keep the process alive.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(6));
        warn!("The HUD did not close in time; exiting.");
        std::process::exit(0);
    });
}

/// Tray menu actions → app.
fn spawn_tray_handler(mut actions: UnboundedReceiver<TrayAction>, app: app::Jarvis, shutdown: CancellationToken) {
    tokio::spawn(async move {
        while let Some(action) = actions.recv().await {
            match action {
                TrayAction::OpenHud => gui::show(),
                TrayAction::WakeJarvis => vram_guard::wake("tray"),
                TrayAction::ToggleMic => {
                    let enabled = !app.config_state().mic_enabled;
                    app.handle_command(hud::HudCommand::SetMic { enabled }).await;
                }
                TrayAction::Quit => {
                    info!("[TRAY] Quit requested");
                    shutdown.cancel();
                }
            }
        }
    });
}
