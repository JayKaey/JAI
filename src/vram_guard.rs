//! Zero-VRAM gaming hibernation and the on-demand model loader.
//!
//! The RTX card belongs to Sir's games: whenever a game is running and JAI has been silent for
//! 15 s, Sir puts him to sleep or signs off, or he has been idle for 30 minutes, every model
//! leaves the GPU:
//! * Ollama unloads each resident model (`/api/generate` with `"keep_alive": 0`, verified with
//!   `/api/ps`), and every normal chat request keeps the model for 30 minutes (`"30m"`),
//! * the Whisper and Kokoro workers drop their engines (`Option::take` → `Drop` frees every
//!   whisper.cpp / ONNX Runtime buffer, CPU or GPU) on their next idle tick.
//!
//! While dormant only the light Silero VAD and a Win32 hotkey (`Alt+J`) are alive. A hotkey, the
//! standby button (HUD, orb, tray) or Sir talking to JAI wakes him: the workers re-create their
//! engines on the fly and the next language request loads the model again.

use crate::config::JaiConfig;
use crate::ears::AudioControls;
use crate::runtime::LlmRuntime;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tracing::{info, warn};

/// A worker that served a job while dormant (Sir spoke during a game) keeps its engine this long
/// before dropping it again, so a conversation does not reload Whisper on every sentence.
const DORMANT_LINGER: Duration = Duration::from_secs(30);
/// After a failed re-load a worker waits this long before trying again on its own.
pub const RELOAD_BACKOFF: Duration = Duration::from_secs(30);
/// While JAI is awake and no game runs, the model stays in VRAM this long after a request
/// (and the idle standby waits as long): consecutive sentences always meet a warm model.
pub const RESIDENT_SECS: u64 = 30 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VramPowerState {
    /// Models resident, Sir is talking to JAI.
    ActiveReady,
    /// Models resident; they leave the GPU at this instant unless something happens.
    IdleCountdown(Instant),
    /// Nothing of JAI is on the GPU.
    ZeroVramDormant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VramGuardConfig {
    pub enabled: bool,
    /// Seconds without an interaction before every model is evicted.
    pub idle_secs: u64,
    /// With a game running: seconds of silence before the eviction.
    pub game_silence_secs: u64,
    /// How often the game/idle check runs.
    pub poll_secs: u64,
    /// `keep_alive` of every normal chat request (Ollama never pins the model forever).
    pub keep_alive_secs: u64,
    /// Executables that count as a game (case-insensitive, with or without `.exe`).
    pub game_processes: Vec<String>,
    /// Global wake hotkeys ("Alt+J", "Ctrl+Shift+Space" …).
    pub hotkeys: Vec<String>,
    /// While dormant, speech the VAD hears still reaches Whisper (re-loaded on the CPU).
    pub dormant_voice_wake: bool,
}

impl Default for VramGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_secs: RESIDENT_SECS,
            game_silence_secs: 15,
            poll_secs: 5,
            keep_alive_secs: RESIDENT_SECS,
            game_processes: [
                "javaw.exe",
                "Minecraft.Windows.exe",
                "FortniteClient-Win64-Shipping.exe",
                "GTA5.exe",
                "BatmanAK.exe",
                "cs2.exe",
                "valorant.exe",
                "VALORANT-Win64-Shipping.exe",
                "r5apex.exe",
                "eldenring.exe",
            ]
            .map(String::from)
            .to_vec(),
            // Ctrl+Space is left out by default: a global registration would take it from every
            // IDE's autocomplete. Add it to `vram_guard.hotkeys` to use it.
            hotkeys: vec!["Alt+J".into()],
            dormant_voice_wake: true,
        }
    }
}

pub struct VramGuardState {
    pub power: VramPowerState,
    pub last_user_interaction: Instant,
    /// JAI spoke or ran a turn.
    pub last_jarvis_activity: Instant,
    /// The running game, if any.
    pub game: Option<String>,
    /// When and why the GPU was last cleared.
    pub last_flush: Option<(Instant, String)>,
    /// Models Ollama holds right now (name, VRAM MB), from `/api/ps`.
    pub resident_models: Vec<(String, u64)>,
    pub stt_loaded: bool,
    pub tts_loaded: bool,
    pub flushes: u64,
    pub config: VramGuardConfig,
}

impl VramGuardState {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            power: VramPowerState::ActiveReady,
            last_user_interaction: now,
            last_jarvis_activity: now,
            game: None,
            last_flush: None,
            resident_models: Vec::new(),
            stt_loaded: true,
            tts_loaded: true,
            flushes: 0,
            config: VramGuardConfig::default(),
        }
    }

    /// Ollama's VRAM in MB.
    pub fn ollama_vram_mb(&self) -> u64 {
        self.resident_models.iter().map(|(_, mb)| mb).sum()
    }

    /// Seconds until the automatic eviction, while models are resident.
    pub fn seconds_to_clear(&self) -> Option<u64> {
        if self.power == VramPowerState::ZeroVramDormant || !self.config.enabled {
            return None;
        }
        let idle = self.config.idle_secs.saturating_sub(self.last_user_interaction.elapsed().as_secs());
        let silent_for = self.last_jarvis_activity.elapsed().min(self.last_user_interaction.elapsed()).as_secs();
        Some(match self.game {
            Some(_) => idle.min(self.config.game_silence_secs.saturating_sub(silent_for)),
            None => idle,
        })
    }
}

/// The guard, shared by the brain, the ears, STT, TTS and the HUD.
pub static GUARD: LazyLock<Arc<Mutex<VramGuardState>>> = LazyLock::new(|| Arc::new(Mutex::new(VramGuardState::new())));
/// Fast flags for the audio workers (no lock on their hot paths).
static DORMANT: AtomicBool = AtomicBool::new(false);
static FLUSH_EPOCH: AtomicU64 = AtomicU64::new(0);

struct Hooks {
    llm: Arc<LlmRuntime>,
    rt: tokio::runtime::Handle,
    on_flush: Box<dyn Fn() + Send + Sync>,
    on_wake: Box<dyn Fn() + Send + Sync>,
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();

fn guard() -> std::sync::MutexGuard<'static, VramGuardState> {
    GUARD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts the idle/game watchdog and the wake hotkeys. `on_flush` runs after an eviction (the
/// brain forgets its primed prompt), `on_wake` when Sir wakes JAI (the brain primes again).
pub fn start(
    cfg: &JaiConfig,
    controls: Arc<AudioControls>,
    llm: Arc<LlmRuntime>,
    on_flush: impl Fn() + Send + Sync + 'static,
    on_wake: impl Fn() + Send + Sync + 'static,
) {
    let rt = tokio::runtime::Handle::current();
    guard().config = cfg.vram_guard.clone();
    if HOOKS.set(Hooks { llm, rt: rt.clone(), on_flush: Box::new(on_flush), on_wake: Box::new(on_wake) }).is_err() {
        return;
    }
    let config = cfg.vram_guard.clone();
    if config.enabled {
        info!(
            "[VRAM] Guard armed: models leave the GPU after {} s idle, or {} s of silence while a game runs.",
            config.idle_secs, config.game_silence_secs
        );
    }
    rt.spawn(watchdog(controls, config.clone()));
    #[cfg(windows)]
    hotkeys::spawn(&config.hotkeys);
}

/// Sir interacted with JAI (a spoken or typed turn): wakes him from standby.
pub fn touch() {
    let dormant = {
        let mut g = guard();
        let now = Instant::now();
        g.last_user_interaction = now;
        g.last_jarvis_activity = now;
        g.power == VramPowerState::ZeroVramDormant
    };
    if dormant {
        wake("conversation");
    } else {
        guard().power = VramPowerState::ActiveReady;
    }
}

pub fn is_dormant() -> bool {
    DORMANT.load(Ordering::Acquire)
}

pub fn flush_epoch() -> u64 {
    FLUSH_EPOCH.load(Ordering::Acquire)
}

/// Whether speech heard while dormant may reach Whisper.
pub fn voice_wake_allowed() -> bool {
    !is_dormant() || guard().config.dormant_voice_wake
}

/// Background jobs (deep research synthesis) may use the language model: no game is running.
pub fn llm_allowed() -> bool {
    guard().game.is_none()
}

/// The `keep_alive` of a normal language request ("30m"): the model stays resident while JAI is
/// awake, never "forever" while the guard is on (a game or sleep evicts it at once).
pub fn keep_alive_value(cfg: &JaiConfig) -> Value {
    if !cfg.vram_guard.enabled {
        return json!(cfg.keep_alive);
    }
    let cap = cfg.vram_guard.keep_alive_secs.max(1);
    let secs = u64::try_from(cfg.keep_alive).map_or(cap, |s| s.min(cap));
    json!(if secs > 0 && secs % 60 == 0 { format!("{}m", secs / 60) } else { format!("{secs}s") })
}

pub fn report_stt(loaded: bool) {
    guard().stt_loaded = loaded;
}

pub fn report_tts(loaded: bool) {
    guard().tts_loaded = loaded;
}

/// What an audio worker should do with its engine on an idle tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerAction {
    Keep,
    /// Drop the engine (`Option` → `None`).
    Unload,
    /// Re-create the engine (JAI was woken).
    Reload,
}

/// `seen_epoch` is the worker's own copy of the flush counter; `retry_at` a back-off after a
/// failed re-load.
pub fn worker_action(loaded: bool, seen_epoch: &mut u64, last_job: Instant, retry_at: Option<Instant>) -> WorkerAction {
    let epoch = flush_epoch();
    let fresh_flush = epoch != *seen_epoch;
    *seen_epoch = epoch;
    let dormant = is_dormant();
    if loaded && dormant && (fresh_flush || last_job.elapsed() >= DORMANT_LINGER) {
        WorkerAction::Unload
    } else if !loaded && !dormant && retry_at.is_none_or(|t| Instant::now() >= t) {
        WorkerAction::Reload
    } else {
        WorkerAction::Keep
    }
}

/// Carries out a worker action on an engine slot: `Unload` sets it to `None` (its `Drop` frees
/// every buffer), `Reload` re-creates it with `load`. True when the slot changed.
pub fn apply_worker_action<T>(slot: &mut Option<T>, action: WorkerAction, load: impl FnOnce() -> Option<T>) -> bool {
    match action {
        WorkerAction::Unload if slot.is_some() => {
            *slot = None;
            true
        }
        WorkerAction::Reload if slot.is_none() => {
            *slot = load();
            slot.is_some()
        }
        _ => false,
    }
}

/// The request that makes Ollama drop a model from VRAM at once.
pub fn eviction_payload(model: &str) -> Value {
    json!({ "model": model, "prompt": "", "keep_alive": 0 })
}

/// Evicts everything JAI holds on the GPU right now.
pub fn flush_all_vram_now() {
    flush_with_reason("requested by Sir");
}

pub fn flush_with_reason(reason: &str) {
    {
        let mut g = guard();
        g.power = VramPowerState::ZeroVramDormant;
        g.last_flush = Some((Instant::now(), reason.to_string()));
        g.flushes += 1;
    }
    // Step B: the STT/TTS workers drop their engines on their next idle tick (≤ 250 ms).
    DORMANT.store(true, Ordering::Release);
    FLUSH_EPOCH.fetch_add(1, Ordering::AcqRel);
    info!("[VRAM] Zero-VRAM standby ({reason}): evicting the language model, Whisper and Kokoro.");
    let Some(hooks) = HOOKS.get() else { return };
    (hooks.on_flush)();
    // Step A: Ollama lets go of every resident model.
    let llm = Arc::clone(&hooks.llm);
    hooks.rt.spawn(async move {
        let freed = evict_ollama(&llm).await;
        let left = resident_models(&llm).await;
        let left_mb: u64 = left.iter().map(|(_, mb)| mb).sum();
        info!("[VRAM] Ollama released {freed} MB; {left_mb} MB still resident.");
        guard().resident_models = left;
    });
    crate::gui::refresh();
}

/// Leaves standby: the workers re-load, the brain primes the model again.
pub fn wake(source: &str) {
    let was_dormant = DORMANT.swap(false, Ordering::AcqRel);
    {
        let mut g = guard();
        let now = Instant::now();
        g.power = VramPowerState::ActiveReady;
        g.last_user_interaction = now;
        g.last_jarvis_activity = now;
    }
    if was_dormant {
        info!("[VRAM] Waking from zero-VRAM standby ({source}): re-loading Whisper, Kokoro and the language model.");
        if let Some(hooks) = HOOKS.get() {
            (hooks.on_wake)();
        }
    }
    crate::gui::refresh();
}

fn client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(20))
                .no_proxy()
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        })
        .clone()
}

/// (name, VRAM MB) of every model the engine holds.
async fn resident_models(llm: &LlmRuntime) -> Vec<(String, u64)> {
    let Ok(resp) = client().get(format!("{}/api/ps", llm.url())).timeout(Duration::from_secs(3)).send().await else {
        return Vec::new();
    };
    let ps: Value = resp.json().await.unwrap_or_default();
    ps["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| Some((m["name"].as_str()?.to_string(), m["size_vram"].as_u64().unwrap_or(0) / 1_048_576)))
        .collect()
}

/// Unloads every resident model; returns the VRAM freed (MB). Only models `/api/ps` lists are
/// addressed: an unload request for a model that is not loaded would load it first.
async fn evict_ollama(llm: &LlmRuntime) -> u64 {
    let mut freed = 0;
    for (name, mb) in resident_models(llm).await {
        let done = client()
            .post(format!("{}/api/generate", llm.url()))
            .json(&eviction_payload(&name))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        if done {
            freed += mb;
        } else {
            warn!("[VRAM] Ollama did not unload '{name}'.");
        }
    }
    freed
}

/// The running game, if any of the configured executables is alive.
fn detect_game(sys: &mut System, games: &[String]) -> Option<String> {
    let wanted: Vec<String> = games.iter().map(|g| g.trim().to_lowercase().trim_end_matches(".exe").to_string()).filter(|g| !g.is_empty()).collect();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new());
    sys.processes().values().find_map(|p| {
        let name = p.name().to_string_lossy().to_lowercase();
        let stem = name.trim_end_matches(".exe");
        wanted.iter().any(|w| w == stem).then(|| p.name().to_string_lossy().into_owned())
    })
}

/// Once a second: notes when JAI speaks; every `poll_secs`: the game and idle checks.
async fn watchdog(controls: Arc<AudioControls>, config: VramGuardConfig) {
    let mut sys = System::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let poll = config.poll_secs.max(1);
    let mut n: u64 = 0;
    loop {
        tick.tick().await;
        n += 1;
        let busy = controls.is_speaking.load(Ordering::Acquire) || controls.turn_active.load(Ordering::Acquire);
        if busy {
            guard().last_jarvis_activity = Instant::now();
        }
        if !n.is_multiple_of(poll) {
            continue;
        }
        let games = config.game_processes.clone();
        let (found, back) = tokio::task::spawn_blocking(move || {
            let found = detect_game(&mut sys, &games);
            (found, sys)
        })
        .await
        .unwrap_or_else(|_| (None, System::new()));
        sys = back;
        let resident = match HOOKS.get() {
            Some(h) if !is_dormant() => Some(resident_models(&h.llm).await),
            _ => None,
        };
        let decision = {
            let mut g = guard();
            if g.game != found {
                match &found {
                    Some(game) => info!("[VRAM] Game detected: {game}"),
                    None => info!("[VRAM] Game closed"),
                }
                g.game = found.clone();
            }
            if let Some(models) = resident {
                g.resident_models = models;
            }
            if !g.config.enabled || g.power == VramPowerState::ZeroVramDormant || busy {
                None
            } else {
                let idle = g.last_user_interaction.elapsed();
                let silent = g.last_jarvis_activity.elapsed().min(idle);
                match &g.game {
                    Some(game) if silent >= Duration::from_secs(g.config.game_silence_secs) => Some(format!("{game} is running")),
                    _ if idle >= Duration::from_secs(g.config.idle_secs) => Some(format!("{} s idle", g.config.idle_secs)),
                    _ => {
                        let left = g.seconds_to_clear().unwrap_or(0);
                        g.power = VramPowerState::IdleCountdown(Instant::now() + Duration::from_secs(left));
                        None
                    }
                }
            }
        };
        if let Some(reason) = decision {
            flush_with_reason(&reason);
        }
    }
}

/// The HUD header badge: (text, hover detail, dormant).
pub fn badge() -> (String, String, bool) {
    let g = guard();
    let key = g.config.hotkeys.first().cloned().unwrap_or_else(|| "Alt+J".into()).to_uppercase();
    let parts = format!(
        "Whisper {} · Kokoro {} · Ollama {} MB{}",
        if g.stt_loaded { "loaded" } else { "unloaded" },
        if g.tts_loaded { "loaded" } else { "unloaded" },
        g.ollama_vram_mb(),
        g.game.as_ref().map(|game| format!(" · game: {game}")).unwrap_or_default()
    );
    match g.power {
        VramPowerState::ZeroVramDormant => {
            let why = g.last_flush.as_ref().map(|(at, why)| format!("\nCleared {} s ago ({why}).", at.elapsed().as_secs())).unwrap_or_default();
            (format!("VRAM: 0 MB (GAMING STANDBY — CLICK OR PRESS {key} TO WAKE)"), format!("{parts}{why}"), true)
        }
        _ => {
            let left = g.seconds_to_clear().map(|s| format!(" (AUTO-CLEAR IN {s}s)")).unwrap_or_default();
            (format!("VRAM: ACTIVE{left}"), format!("{parts}\nClick to clear the GPU now."), false)
        }
    }
}

/// A spoken/LLM summary of the guard.
pub fn status_line() -> String {
    let g = guard();
    let state = match g.power {
        VramPowerState::ActiveReady => "active".to_string(),
        VramPowerState::IdleCountdown(_) => format!("active, auto-clear in {} s", g.seconds_to_clear().unwrap_or(0)),
        VramPowerState::ZeroVramDormant => "zero-VRAM standby".to_string(),
    };
    let models = if g.resident_models.is_empty() {
        "no model resident".to_string()
    } else {
        g.resident_models.iter().map(|(n, mb)| format!("{n} {mb} MB")).collect::<Vec<_>>().join(", ")
    };
    format!(
        "VRAM guard: {state}. Ollama: {models}. Whisper {}, Kokoro {}.{} {} evictions so far.",
        if g.stt_loaded { "loaded" } else { "unloaded" },
        if g.tts_loaded { "loaded" } else { "unloaded" },
        g.game.as_ref().map(|game| format!(" Game running: {game}.")).unwrap_or_default(),
        g.flushes
    )
}

pub fn is_tool(name: &str) -> bool {
    name == "manage_vram"
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": "manage_vram",
        "description": "Zero-VRAM gaming standby: 'flush' evicts the language model, Whisper and Kokoro from the GPU right now (0 MB), 'wake' loads them again, 'status' reports what is resident and whether a game runs.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["status", "flush", "wake"]}
        }, "required": ["action"]}
    }})]
}

pub fn run_tool(args: &Value) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("status") {
        "flush" | "clear" | "free" | "sleep" | "hibernate" => {
            flush_all_vram_now();
            Ok("GPU cleared: the language model, Whisper and Kokoro are unloaded (0 MB VRAM). Press Alt+J, click or talk to me to wake me.".into())
        }
        "wake" | "load" | "activate" => {
            wake("tool");
            Ok("Waking up: Whisper, Kokoro and the language model are loading.".into())
        }
        _ => Ok(status_line()),
    }
}

#[cfg(windows)]
mod hotkeys {
    use tracing::{info, warn};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN};
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

    /// "Alt+J" → (modifiers, virtual key).
    pub fn parse(spec: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
        let mut mods = HOT_KEY_MODIFIERS(0);
        let mut key = None;
        for part in spec.split('+').map(|p| p.trim().to_lowercase()) {
            match part.as_str() {
                "alt" => mods |= MOD_ALT,
                "ctrl" | "control" | "strg" => mods |= MOD_CONTROL,
                "shift" => mods |= MOD_SHIFT,
                "win" | "super" | "meta" => mods |= MOD_WIN,
                "space" => key = Some(0x20),
                "enter" | "return" => key = Some(0x0D),
                "home" => key = Some(0x24),
                "end" => key = Some(0x23),
                "insert" => key = Some(0x2D),
                "pause" => key = Some(0x13),
                f if f.len() >= 2 && f.starts_with('f') && f[1..].parse::<u32>().is_ok_and(|n| (1..=24).contains(&n)) => {
                    key = f[1..].parse::<u32>().ok().map(|n| 0x6F + n);
                }
                c if c.len() == 1 && c.chars().all(|ch| ch.is_ascii_alphanumeric()) => {
                    key = c.chars().next().map(|ch| ch.to_ascii_uppercase() as u32);
                }
                _ => return None,
            }
        }
        key.map(|k| (mods, k))
    }

    /// Registers the hotkeys on a thread with its own message loop (WM_HOTKEY → wake).
    pub fn spawn(specs: &[String]) {
        let parsed: Vec<(String, HOT_KEY_MODIFIERS, u32)> = specs
            .iter()
            .filter_map(|s| match parse(s) {
                Some((m, k)) => Some((s.clone(), m, k)),
                None => {
                    warn!("[VRAM] Unknown hotkey '{s}'");
                    None
                }
            })
            .collect();
        if parsed.is_empty() {
            return;
        }
        let spawned = std::thread::Builder::new().name("jarvis-wake-hotkey".into()).spawn(move || {
            let mut any = false;
            for (i, (name, mods, vk)) in parsed.iter().enumerate() {
                // SAFETY: thread-owned hotkey (no window); the id is unique within this thread.
                match unsafe { RegisterHotKey(HWND(0), 0x4A00 + i as i32, *mods | MOD_NOREPEAT, *vk) } {
                    Ok(()) => {
                        any = true;
                        info!("[VRAM] Wake hotkey {name} registered.");
                    }
                    Err(e) => warn!("[VRAM] Hotkey {name} is taken by another program ({e})."),
                }
            }
            if !any {
                return;
            }
            let mut msg = MSG::default();
            // SAFETY: standard message loop on this thread's own queue.
            while unsafe { GetMessageW(&mut msg, HWND(0), 0, 0) }.as_bool() {
                if msg.message == WM_HOTKEY {
                    super::wake("hotkey");
                }
            }
        });
        if let Err(e) = spawned {
            warn!("[VRAM] No hotkey thread: {e}");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_hotkeys() {
            assert_eq!(parse("Alt+J"), Some((MOD_ALT, 'J' as u32)));
            assert_eq!(parse("ctrl + space"), Some((MOD_CONTROL, 0x20)));
            assert_eq!(parse("Ctrl+Shift+F9"), Some((MOD_CONTROL | MOD_SHIFT, 0x78)));
            assert_eq!(parse("Alt+Banana"), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repro 2026-11-03: every LLM turn after a pause hit a cold model (keep_alive 90 s, idle
    /// eviction 90 s), and BatmanAK.exe filled the GPU without counting as a game.
    #[test]
    fn the_model_stays_resident_for_30_minutes_and_batman_is_a_game() {
        let cfg = JaiConfig::default();
        assert_eq!(keep_alive_value(&cfg), json!("30m"));
        assert!(cfg.vram_guard.idle_secs >= 1800, "idle eviction after {} s", cfg.vram_guard.idle_secs);
        assert!(cfg.vram_guard.game_processes.iter().any(|g| g.eq_ignore_ascii_case("BatmanAK.exe")));
    }

    #[test]
    fn keep_alive_never_forever() {
        let cfg = |keep_alive, enabled| {
            let mut c = JaiConfig { keep_alive, ..JaiConfig::default() };
            c.vram_guard.enabled = enabled;
            c
        };
        assert_eq!(keep_alive_value(&cfg(-1, true)), json!("30m"));
        assert_eq!(keep_alive_value(&cfg(30, true)), json!("30s"));
        assert_eq!(keep_alive_value(&cfg(-1, false)), json!(-1));
    }

    /// Counts drops: the engine's buffers are freed the moment the slot becomes `None`.
    struct Probe(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn flush_and_wake_cycle_drops_and_reloads_the_engines() {
        // The only test that touches the global guard.
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut stt: Option<Probe> = Some(Probe(Arc::clone(&drops)));
        let mut tts: Option<Probe> = Some(Probe(Arc::clone(&drops)));
        let (mut stt_epoch, mut tts_epoch) = (flush_epoch(), flush_epoch());
        let recent = Instant::now();
        assert_eq!(worker_action(true, &mut stt_epoch, recent, None), WorkerAction::Keep, "awake: engines stay");

        flush_with_reason("test");
        assert!(is_dormant());
        assert_eq!(guard().power, VramPowerState::ZeroVramDormant);
        assert!(badge().0.starts_with("VRAM: 0 MB (GAMING STANDBY"));
        // A fresh flush unloads at once, even right after a job.
        let a = worker_action(stt.is_some(), &mut stt_epoch, recent, None);
        let b = worker_action(tts.is_some(), &mut tts_epoch, recent, None);
        assert_eq!((a, b), (WorkerAction::Unload, WorkerAction::Unload));
        assert!(apply_worker_action(&mut stt, a, || None));
        assert!(apply_worker_action(&mut tts, b, || None));
        assert!(stt.is_none() && tts.is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 2, "both engines were dropped");
        // Dormant and unloaded: nothing reloads on its own.
        assert_eq!(worker_action(false, &mut stt_epoch, recent, None), WorkerAction::Keep);

        wake("test");
        assert!(!is_dormant());
        assert_eq!(guard().power, VramPowerState::ActiveReady);
        let a = worker_action(stt.is_some(), &mut stt_epoch, recent, None);
        assert_eq!(a, WorkerAction::Reload);
        assert!(apply_worker_action(&mut stt, a, || Some(Probe(Arc::clone(&drops)))));
        assert!(stt.is_some());
        // A failed reload waits for its back-off.
        let later = Some(Instant::now() + RELOAD_BACKOFF);
        assert_eq!(worker_action(false, &mut tts_epoch, recent, later), WorkerAction::Keep);
        assert!(badge().0.starts_with("VRAM: ACTIVE"));
    }

    #[test]
    fn eviction_payload_is_exact() {
        assert_eq!(eviction_payload("qwen3:30b-a3b"), json!({ "model": "qwen3:30b-a3b", "prompt": "", "keep_alive": 0 }));
        assert_eq!(eviction_payload("x")["keep_alive"], 0);
    }

    #[test]
    fn countdown_uses_the_game_window() {
        let mut s = VramGuardState::new();
        assert_eq!(s.seconds_to_clear(), Some(RESIDENT_SECS));
        s.game = Some("cs2.exe".into());
        assert_eq!(s.seconds_to_clear(), Some(15));
        s.power = VramPowerState::ZeroVramDormant;
        assert_eq!(s.seconds_to_clear(), None);
    }
}
