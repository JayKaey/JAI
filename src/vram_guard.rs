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
//! While dormant only the light Silero VAD and a global hotkey (`Alt+J`; a KDE shortcut on
//! Linux) are alive. A hotkey, the
//! standby button (HUD, orb, tray) or Sir talking to JAI wakes him: the workers re-create their
//! engines on the fly and the next language request loads the model again.
//!
//! Game mode (a game runs, or Sir said "game mode on"): the model leaves the GPU at once and is
//! loaded into RAM to answer on the CPU (`num_gpu` 0, four threads, below-normal priority, a
//! short prompt); Whisper and Kokoro stay loaded, they run on the CPU and hold no VRAM. A turn
//! in a game never loads the GPU again: that was the lag (2026-11-15). 30 s after the game has
//! closed the model goes back into VRAM.

use crate::config::JaiConfig;
use crate::ears::AudioControls;
use chrono::Timelike;
use crate::runtime::LlmRuntime;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
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
    pub last_jai_activity: Instant,
    /// The running game's title ("Grand Theft Auto V", "Minecraft"), if any, and since when.
    pub game: Option<String>,
    pub game_since: Option<Instant>,
    /// When the last game closed (game mode lasts `GAME_GRACE` longer).
    pub game_left: Option<Instant>,
    /// The most VRAM one model has taken (MB): what a load onto the GPU needs free.
    pub model_vram_mb: u64,
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
            last_jai_activity: now,
            game: None,
            game_since: None,
            game_left: None,
            model_vram_mb: 0,
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
        Some(self.config.idle_secs.saturating_sub(self.last_user_interaction.elapsed().as_secs()))
    }

    /// A full GPU may switch game mode on: never against Sir's "off", never with the guard off.
    fn may_squeeze(&self, manual: u8) -> bool {
        self.config.enabled && manual != 2
    }

    /// Whether game mode should be on (`manual`: Sir's switch, `playing`: game mode is on now).
    fn wants_game(&self, manual: u8, squeezed: bool, playing: bool) -> bool {
        // The grace only keeps game mode on: after Sir's "off" it never switches it back on.
        let auto = self.game.is_some() || (playing && self.game_left.is_some_and(|t| t.elapsed() < GAME_GRACE)) || squeezed;
        self.config.enabled
            && match manual {
                1 => true,
                2 => false,
                _ => auto,
            }
    }

    /// The largest single model's VRAM is what a load needs: two models at once are no measure.
    fn note_model_vram(&mut self) {
        let largest = self.resident_models.iter().map(|(_, mb)| *mb).max().unwrap_or(0);
        self.model_vram_mb = self.model_vram_mb.max(largest);
    }
}

/// The guard, shared by the brain, the ears, STT, TTS and the HUD.
pub static GUARD: LazyLock<Arc<Mutex<VramGuardState>>> = LazyLock::new(|| Arc::new(Mutex::new(VramGuardState::new())));
/// Fast flags for the audio workers (no lock on their hot paths).
static DORMANT: AtomicBool = AtomicBool::new(false);
static FLUSH_EPOCH: AtomicU64 = AtomicU64::new(0);
/// Game mode (see the module doc).
static GAME_MODE: AtomicBool = AtomicBool::new(false);
/// Sir's own switch: 0 automatic, 1 on, 2 off (until the game closes).
static MANUAL: AtomicU8 = AtomicU8::new(0);
/// Free RAM fell under `RAM_FLOOR_MB` in game mode: the CPU brain stays out of RAM.
static RAM_TIGHT: AtomicBool = AtomicBool::new(false);
/// Free RAM at the last poll (MB; 0 until the first).
static FREE_RAM_MB: AtomicU64 = AtomicU64::new(0);
/// The running game's process (0: none), whose mixer channel dips under JAI's voice.
static GAME_PID: AtomicU32 = AtomicU32::new(0);
/// Game mode outlasts a game by this long, so restarting the game never bounces the model.
const GAME_GRACE: Duration = Duration::from_secs(30);
/// In a game the CPU brain needs this much free RAM, or it leaves (the game must never swap).
const RAM_FLOOR_MB: u64 = 6 * 1024;
/// The CPU brain: threads, context window (its short prompt fits easily).
pub const GAME_THREADS: i32 = 4;
pub const GAME_CTX: u32 = 8192;
/// What a load onto the GPU needs free when the model has never been measured (MB).
const MODEL_VRAM_GUESS_MB: u64 = 4500;

pub fn in_game() -> bool {
    GAME_MODE.load(Ordering::Acquire)
}

/// Whisper's threads: four in a game, so hearing Sir takes fewer cores from it.
pub fn ears_threads(threads: i32) -> i32 {
    if in_game() {
        threads.min(GAME_THREADS)
    } else {
        threads
    }
}

/// The CPU brain may not load (RAM is short while a game runs).
pub fn ram_tight() -> bool {
    in_game() && RAM_TIGHT.load(Ordering::Acquire)
}

/// Room in RAM for the brain above the floor the game keeps.
fn ram_has_room(free_mb: u64) -> bool {
    // shortcut: the CPU brain is taken to need MODEL_VRAM_GUESS_MB of RAM; measure it (/api/ps `size`) if a bigger model or context flaps again.
    free_mb > RAM_FLOOR_MB + MODEL_VRAM_GUESS_MB
}

/// Sir's switch: `Some(true)` "game mode on", `Some(false)` "game mode off" (holds until the
/// game closes), `None` automatic again. Takes effect at once.
pub fn set_game_mode(on: Option<bool>) {
    // ("Off" holds until a game starts or ends: a card filled by something else never switches it
    // back on, and the next game still counts.)
    let manual = match on {
        Some(true) => 1,
        Some(false) => 2,
        None => 0,
    };
    MANUAL.store(manual, Ordering::Release);
    SQUEEZED.store(false, Ordering::Release);
    match on {
        Some(true) => enter_game("Sir's order"),
        Some(false) => leave_game("Sir's order"),
        None => {}
    }
}

/// The running game and how long it has run.
pub fn game_now() -> Option<(String, Duration)> {
    let g = guard();
    Some((g.game.clone()?, g.game_since.map(|t| t.elapsed()).unwrap_or_default()))
}

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

/// Starts the idle/game watchdog (the wake keys are `hotkeys`'). `on_flush` runs after an
/// eviction (the brain forgets its primed prompt), `on_wake` when Sir wakes JAI (the brain primes
/// again).
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
            "[VRAM] Guard armed: the model leaves the GPU after {} s idle, and for RAM the moment a game runs.",
            config.idle_secs
        );
    }
    rt.spawn(game_voice(Arc::clone(&controls)));
    rt.spawn(watchdog(controls, config));
}

/// The morning warm-up: Sir's first word of each day is kept (minutes after midnight); ten
/// minutes before the usual one (the middle of the last week's) the model loads, once a day.
fn note_first_use() {
    static NOTED: Mutex<String> = Mutex::new(String::new());
    let now = chrono::Local::now();
    let day = now.format("%Y-%m-%d").to_string();
    let mut noted = NOTED.lock().unwrap_or_else(|e| e.into_inner());
    if *noted == day || crate::platform::hands_off() {
        return;
    }
    *noted = day.clone();
    if crate::kit::get("first_use", &day).is_none() {
        let _ = crate::kit::put("first_use", &day, &json!(now.hour() * 60 + now.minute()));
    }
}

/// The usual first minute of his day: the middle of the week before `today` (three days at least).
fn usual_first_use(days: &[(String, Value, String)], today: chrono::NaiveDate) -> Option<u32> {
    let mut minutes: Vec<u32> = days
        .iter()
        .filter_map(|(day, minute, _)| {
            let ago = (today - chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?).num_days();
            (1..=7).contains(&ago).then(|| minute.as_u64().map(|m| m as u32)).flatten()
        })
        .collect();
    minutes.sort_unstable();
    (minutes.len() >= 3).then(|| minutes[minutes.len() / 2])
}

/// Whether to load the model now: no word from him yet today, from ten minutes before his usual
/// first one until half an hour after it, once (the database is read once a minute at most).
fn morning_due() -> bool {
    static SEEN: Mutex<(String, u32)> = Mutex::new((String::new(), u32::MAX));
    let now = chrono::Local::now();
    let (day, minute) = (now.format("%Y-%m-%d").to_string(), now.hour() * 60 + now.minute());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.0 == day || seen.1 == minute {
        return false;
    }
    seen.1 = minute;
    let days = crate::kit::list("first_use");
    let due = !days.iter().any(|(d, _, _)| *d == day) && usual_first_use(&days, now.date_naive()).is_some_and(|usual| (usual.saturating_sub(10)..usual + 30).contains(&minute));
    if due {
        seen.0 = day;
    }
    due
}

/// Sir interacted with JAI (a spoken or typed turn): wakes him from standby.
pub fn touch() {
    note_first_use();
    let dormant = {
        let mut g = guard();
        let now = Instant::now();
        g.last_user_interaction = now;
        g.last_jai_activity = now;
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
/// awake, never "forever" while the guard is on (a game or sleep evicts it at once). In a game it
/// waits in RAM for good: JAI evicts it himself when RAM runs short or the game is over.
pub fn keep_alive_value(cfg: &JaiConfig) -> Value {
    keep_alive_for(cfg, in_game())
}

fn keep_alive_for(cfg: &JaiConfig, game: bool) -> Value {
    if game {
        return json!(-1);
    }
    if !cfg.vram_guard.enabled {
        return json!(cfg.keep_alive);
    }
    let cap = cfg.vram_guard.keep_alive_secs.max(1);
    let secs = u64::try_from(cfg.keep_alive).map_or(cap, |s| s.min(cap));
    json!(if secs > 0 && secs % 60 == 0 { format!("{}m", secs / 60) } else { format!("{secs}s") })
}

pub fn report_stt(loaded: bool) {
    guard().stt_loaded = loaded;
    if loaded {
        crate::startup_sequence::up(1);
    }
}

pub fn report_tts(loaded: bool) {
    guard().tts_loaded = loaded;
    if loaded {
        crate::startup_sequence::up(2);
    }
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
    // In a game the ears and the voice stay: they run on the CPU and hold no VRAM.
    let dormant = is_dormant() && !in_game();
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
        g.note_model_vram();
    }
    // Step B: the STT/TTS workers drop their engines on their next idle tick (≤ 250 ms).
    DORMANT.store(true, Ordering::Release);
    FLUSH_EPOCH.fetch_add(1, Ordering::AcqRel);
    if in_game() {
        info!("[VRAM] Zero-VRAM ({reason}): evicting the language model; the ears and the voice stay.");
    } else {
        info!("[VRAM] Zero-VRAM standby ({reason}): evicting the language model, Whisper and Kokoro.");
    }
    let Some(hooks) = HOOKS.get() else { return };
    (hooks.on_flush)();
    // Step A: Ollama lets go of every resident model.
    let llm = Arc::clone(&hooks.llm);
    let models: Vec<String> = guard().resident_models.iter().map(|(name, _)| name.clone()).collect();
    hooks.rt.spawn(async move {
        let freed = evict_ollama(&llm).await;
        let left = resident_models(&llm).await;
        let left_mb: u64 = left.iter().map(|(_, mb)| mb).sum();
        info!("[VRAM] Ollama released {freed} MB; {left_mb} MB still resident.");
        guard().resident_models = left;
        if in_game() {
            // Game mode: the brain comes back into RAM, on the CPU (`Brain::options`).
            if !ram_tight() {
                (hooks.on_wake)();
            }
        } else if is_dormant() && ram_has_room(FREE_RAM_MB.load(Ordering::Acquire)) {
            // Standby: the model waits in RAM (mapped, on no GPU), so waking reads it from RAM in
            // seconds instead of 30-117 s from the disk.
            for model in models {
                let warm = client().post(format!("{}/api/generate", llm.url())).json(&crate::llm_reboot::warm_payload(&model, json!(-1))).send().await;
                info!("[VRAM] '{model}' waits in RAM for the wake-up ({}).", if warm.is_ok_and(|r| r.status().is_success()) { "loaded" } else { "failed" });
            }
        }
    });
    crate::gui::refresh();
}

/// A game started (or Sir said so): the model moves from VRAM to RAM, JAI drops below normal.
fn enter_game(why: &str) {
    if GAME_MODE.swap(true, Ordering::AcqRel) {
        return;
    }
    info!("[GAME] Game mode ({why}): the language model moves to RAM and answers on the CPU.");
    crate::platform::game_priority(true);
    flush_with_reason(&format!("game mode: {why}"));
}

/// Game mode is over: the model goes back into VRAM (from RAM, so in seconds).
fn leave_game(why: &str) {
    if !in_game() {
        return;
    }
    // Standby ends before game mode does: standby without a game makes the workers drop the ears.
    DORMANT.store(false, Ordering::Release);
    if !GAME_MODE.swap(false, Ordering::AcqRel) {
        return;
    }
    RAM_TIGHT.store(false, Ordering::Release);
    crate::platform::game_priority(false);
    info!("[GAME] Game mode over ({why}): the language model goes back into VRAM.");
    wake("game mode over");
}

/// Leaves standby: the workers re-load, the brain primes the model again. In game mode the
/// model stays in RAM (a wake never loads the GPU mid-game).
pub fn wake(source: &str) {
    // (Standby is either flag: leaving game mode clears DORMANT before it calls this.)
    let squeeze = {
        let g = guard();
        (g.power == VramPowerState::ZeroVramDormant || is_dormant()) && g.may_squeeze(MANUAL.load(Ordering::Acquire))
    };
    if !in_game() && squeeze {
        if let Some(free) = vram_short() {
            info!("[GAME] Only {free} MB of VRAM free ({source}): another program holds the GPU, so the language model stays in RAM.");
            SQUEEZED.store(true, Ordering::Release);
            enter_game("the GPU is full");
        }
    }
    if in_game() {
        let mut g = guard();
        g.last_user_interaction = Instant::now();
        g.last_jai_activity = Instant::now();
        return;
    }
    let was_dormant = {
        let mut g = guard();
        let now = Instant::now();
        g.last_user_interaction = now;
        g.last_jai_activity = now;
        let standby = std::mem::replace(&mut g.power, VramPowerState::ActiveReady) == VramPowerState::ZeroVramDormant;
        DORMANT.swap(false, Ordering::AcqRel) || standby
    };
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

/// Where launchers install games: any program started from one of these folders is a game.
const GAME_FOLDERS: &[&str] = &[
    "\\steamapps\\common\\",
    "\\epic games\\",
    "\\xboxgames\\",
    "\\ea games\\",
    "\\ubisoft game launcher\\games\\",
    "\\riot games\\",
    "\\gog galaxy\\games\\",
    "\\gog games\\",
];
/// Programs in those folders that are no game (launchers, helpers, wallpapers, VR, tools).
const NOT_GAMES: &[&str] = &[
    "wallpaper", "steamvr", "vrserver", "vrmonitor", "vrcompositor", "vrdashboard", "launcher", "crash", "setup", "redist", "unins",
    "helper", "anticheat", "beservice", "riotclient", "leagueclient", "overlay", "lossless", "obs", "soundpad", "updater", "installer",
    "service", "cef", "report", "eosbootstrapper", "ue4prereq",
];

/// The game a process is, by its listed name or the folder it was started from: the install
/// folder's name where there is one ("Grand Theft Auto V"), else the program's ("Minecraft").
pub fn game_title(exe_path: &str, name: &str, listed: &[String]) -> Option<String> {
    let stem = name.to_ascii_lowercase();
    let stem = stem.trim_end_matches(".exe");
    let path = exe_path.replace('/', "\\");
    let lower = path.to_ascii_lowercase();
    // (`to_ascii_lowercase` keeps byte offsets, so a hit in `lower` cuts `path` at the same place.)
    let folder = GAME_FOLDERS
        .iter()
        .find_map(|f| lower.find(f).map(|at| at + f.len()))
        .and_then(|from| path[from..].split('\\').next().filter(|s| !s.is_empty() && from + s.len() < path.len()).map(str::to_string));
    let is_listed = listed.iter().any(|g| g.trim().to_ascii_lowercase().trim_end_matches(".exe") == stem);
    if !is_listed && (folder.is_none() || NOT_GAMES.iter().any(|n| stem.contains(n))) {
        return None;
    }
    Some(folder.unwrap_or_else(|| match stem {
        "javaw" | "minecraft.windows" => "Minecraft".to_string(),
        _ => name.trim_end_matches(".exe").trim_end_matches(".EXE").to_string(),
    }))
}

/// Games Sir switched off in the settings ("counts as a game"), by folder title, lowercased;
/// read from the kit store once.
static NOT_A_GAME: Mutex<Option<Vec<String>>> = Mutex::new(None);

pub fn counts_as_game(title: &str) -> bool {
    let mut off = NOT_A_GAME.lock().unwrap_or_else(|e| e.into_inner());
    let off = off.get_or_insert_with(|| crate::kit::list("not_a_game").into_iter().map(|(key, _, _)| key).collect());
    !off.contains(&title.to_lowercase())
}

pub fn set_counts_as_game(title: &str, counts: bool) {
    let key = title.to_lowercase();
    let _ = counts_as_game(&key);
    if let Some(off) = NOT_A_GAME.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        off.retain(|k| *k != key);
        if !counts {
            off.push(key.clone());
        }
    }
    // (Written off the window thread: the switch is clicked there.)
    std::thread::spawn(move || {
        if counts {
            crate::kit::remove("not_a_game", &key);
        } else {
            let _ = crate::kit::put("not_a_game", &key, &json!(true));
        }
    });
}

/// The running game, if any: a listed program or one started from a game folder.
fn detect_game(sys: &mut System, games: &[String]) -> Option<String> {
    sys.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new().with_exe(sysinfo::UpdateKind::OnlyIfNotSet));
    let (pid, title) = sys
        .processes()
        .iter()
        .find_map(|(pid, p)| {
            let path = p.exe().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
            Some((pid.as_u32(), game_title(&path, &p.name().to_string_lossy(), games).filter(|t| counts_as_game(t))?))
        })
        .unzip();
    GAME_PID.store(pid.unwrap_or(0), Ordering::Release);
    title
}

/// In a game the game's own sound dips under JAI's voice and comes back 0.9 s after he stops
/// (sentence gaps keep it down). Out of games it looks once a second.
async fn game_voice(controls: Arc<AudioControls>) {
    let (mut dipped, mut quiet) = (false, 0u8);
    loop {
        tokio::time::sleep(Duration::from_millis(if in_game() || dipped { 150 } else { 1000 })).await;
        let speaking = in_game() && controls.is_speaking.load(Ordering::Acquire);
        quiet = if speaking { 0 } else { quiet.saturating_add(1) };
        if speaking != dipped && (speaking || quiet >= 6) && !crate::platform::hands_off() {
            dipped = speaking;
            crate::audio_mixer::dip_game(GAME_PID.load(Ordering::Acquire), speaking).await;
        }
    }
}

/// What the brain still needs on the graphics card (MB): nothing while it is there, else what it
/// took last time. Whisper goes onto the card only with this left free (`stt::Models`).
pub fn brain_reserve_mb() -> u64 {
    let g = guard();
    if g.ollama_vram_mb() > 0 {
        0
    } else {
        g.model_vram_mb.max(MODEL_VRAM_GUESS_MB)
    }
}

/// The GPU is too full for the model (free VRAM under what it took last time): an unknown game
/// or program holds it, so the brain stays in RAM.
fn vram_short() -> Option<u64> {
    if cfg!(test) {
        return None;
    }
    let need = guard().model_vram_mb.max(MODEL_VRAM_GUESS_MB);
    crate::guardian::vram_free_mb().filter(|free| *free < need)
}

/// Entered game mode because the GPU was full, not because of a game.
static SQUEEZED: AtomicBool = AtomicBool::new(false);

/// Once a second: notes when JAI speaks; every `poll_secs`: the game and idle checks.
/// Input idle this long is "away" (the PC locked, a walk to the kitchen).
const AWAY_SECS: u64 = 300;

/// (away now, back this moment): away once input rests `AWAY_SECS`, back when the idle time
/// falls again (input came).
fn came_back(away: bool, last_idle: u64, idle: u64) -> (bool, bool) {
    match (away, idle >= AWAY_SECS) {
        (_, true) => (true, false),
        (true, false) => (false, idle < last_idle),
        (false, false) => (false, false),
    }
}

async fn watchdog(controls: Arc<AudioControls>, config: VramGuardConfig) {
    let mut sys = System::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let poll = config.poll_secs.max(1);
    // The first tick looks at once: JAI started during a game never loads the GPU.
    let mut n: u64 = poll - 1;
    let (mut away, mut last_idle) = (false, 0u64);
    loop {
        tick.tick().await;
        n += 1;
        let busy = controls.is_speaking.load(Ordering::Acquire) || controls.turn_active.load(Ordering::Acquire);
        if busy {
            guard().last_jai_activity = Instant::now();
        }
        if !n.is_multiple_of(poll) {
            continue;
        }
        let games = config.game_processes.clone();
        let (found, free_ram_mb, back) = tokio::task::spawn_blocking(move || {
            let found = detect_game(&mut sys, &games);
            sys.refresh_memory();
            let free = sys.available_memory() / 1_048_576;
            (found, free, sys)
        })
        .await
        .unwrap_or_else(|_| (None, u64::MAX, System::new()));
        sys = back;
        FREE_RAM_MB.store(free_ram_mb, Ordering::Release);
        let resident = match HOOKS.get() {
            Some(h) if !is_dormant() => Some(resident_models(&h.llm).await),
            _ => None,
        };
        let (decision, want_game, changed) = {
            let mut g = guard();
            let mut changed = None;
            if g.game != found {
                match (&g.game, &found) {
                    (_, Some(game)) => info!("[GAME] Game detected: {game}"),
                    (Some(game), None) => info!("[GAME] Game closed: {game}"),
                    _ => {}
                }
                let now = Instant::now();
                if found.is_none() {
                    g.game_left = Some(now);
                }
                // "Game mode off" holds for that game (or that stretch without one) only.
                let _ = MANUAL.compare_exchange(2, 0, Ordering::AcqRel, Ordering::Acquire);
                g.game_since = found.as_ref().map(|_| now);
                changed = Some((g.game.take(), found.clone()));
                g.game = found.clone();
            }
            if let Some(models) = resident {
                g.resident_models = models;
                g.note_model_vram();
            }
            let want_game = g.wants_game(MANUAL.load(Ordering::Acquire), SQUEEZED.load(Ordering::Acquire), in_game());
            let decision = if !g.config.enabled || g.power == VramPowerState::ZeroVramDormant || busy || want_game {
                None
            } else if g.last_user_interaction.elapsed() >= Duration::from_secs(g.config.idle_secs) {
                Some(format!("{} s idle", g.config.idle_secs))
            } else {
                let left = g.seconds_to_clear().unwrap_or(0);
                g.power = VramPowerState::IdleCountdown(Instant::now() + Duration::from_secs(left));
                None
            };
            (decision, want_game, changed)
        };
        if let Some((was, now)) = changed {
            crate::kit_games::on_change(was, now);
        }
        crate::kit_games::poll();
        if is_dormant() && !in_game() && !crate::platform::in_test_session() && morning_due() {
            wake("the morning warm-up");
        }
        // Back at the PC after a while away (an unlock, a hand on the mouse): the brain warms
        // up while he sits down, not when he first speaks.
        let idle = crate::platform::idle_seconds().unwrap_or(0);
        let back;
        (away, back) = came_back(away, last_idle, idle);
        last_idle = idle;
        if back && is_dormant() && !in_game() && !crate::platform::in_test_session() {
            wake("Sir is back at the PC");
        }
        // Mid-turn the switch waits for the turn to finish (a reply is never cut off).
        if want_game && !in_game() && !busy {
            let why = guard().game.clone().unwrap_or_else(|| "Sir's order".into());
            enter_game(&why);
        } else if !want_game && in_game() {
            leave_game("the game has closed");
        }
        if in_game() {
            // A full GPU without a game: back to VRAM once the room is free again.
            if SQUEEZED.load(Ordering::Acquire) && guard().game.is_none() && vram_short().is_none() {
                SQUEEZED.store(false, Ordering::Release);
                leave_game("the GPU has room again");
            }
            // The game must never swap: the CPU brain leaves RAM below the floor (and comes back
            // once it fits above it again).
            if free_ram_mb < RAM_FLOOR_MB && !RAM_TIGHT.swap(true, Ordering::AcqRel) {
                warn!("[GAME] Only {free_ram_mb} MB of RAM free: the language model leaves RAM; commands still work.");
                flush_with_reason("RAM is short in a game");
            } else if ram_has_room(free_ram_mb) && RAM_TIGHT.swap(false, Ordering::AcqRel) {
                info!("[GAME] {free_ram_mb} MB of RAM free again: the language model comes back into RAM.");
                if let Some(h) = HOOKS.get() {
                    (h.on_wake)();
                }
            }
        }
        if let Some(reason) = decision {
            flush_with_reason(&reason);
        }
    }
}

/// The HUD header badge: (text, hover detail, dormant).
pub fn badge() -> (String, String, bool) {
    let g = guard();
    let wake = crate::hotkeys::first(crate::hotkeys::Act::Wake).map(|key| format!("CLICK OR PRESS {} TO WAKE", key.to_uppercase())).unwrap_or_else(|| "CLICK TO WAKE".into());
    let parts = format!(
        "Whisper {} · Kokoro {} · Ollama {} MB{}",
        if g.stt_loaded { "loaded" } else { "unloaded" },
        if g.tts_loaded { "loaded" } else { "unloaded" },
        g.ollama_vram_mb(),
        g.game.as_ref().map(|game| format!(" · game: {game}")).unwrap_or_default()
    );
    match g.power {
        VramPowerState::ZeroVramDormant if in_game() => {
            let brain = if ram_tight() { "BRAIN OUT (RAM SHORT)" } else { "BRAIN IN RAM" };
            (format!("GAME MODE · VRAM 0 MB · {brain}"), format!("{parts}\nThe language model answers on the CPU until the game has closed."), true)
        }
        VramPowerState::ZeroVramDormant => {
            let why = g.last_flush.as_ref().map(|(at, why)| format!("\nCleared {} s ago ({why}).", at.elapsed().as_secs())).unwrap_or_default();
            (format!("VRAM: 0 MB (GAMING STANDBY — {wake})"), format!("{parts}{why}"), true)
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
        VramPowerState::ZeroVramDormant if in_game() => "game mode: 0 MB VRAM, the language model answers from RAM on the CPU".to_string(),
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
            let wake = crate::hotkeys::first(crate::hotkeys::Act::Wake).map(|key| format!("Press {key}, click")).unwrap_or_else(|| "Click".into());
            Ok(format!("GPU cleared: the language model, Whisper and Kokoro are unloaded (0 MB VRAM). {wake} or talk to me to wake me."))
        }
        "wake" | "load" | "activate" => {
            wake("tool");
            Ok("Waking up: Whisper, Kokoro and the language model are loading.".into())
        }
        // (Phrase-only, not in the schema: "game mode on/off".)
        "game_on" => {
            set_game_mode(Some(true));
            Ok("Game mode on: 0 MB of VRAM, my mind answers from RAM until you switch it off.".into())
        }
        "game_off" => {
            set_game_mode(Some(false));
            Ok("Game mode off: the language model goes back onto the GPU.".into())
        }
        // ("Is game mode on?": a short answer, no model.)
        "game_status" => Ok(match (in_game(), game_now()) {
            (true, Some((game, _))) => format!("Game mode is on for {}, Sir: 0 MB of VRAM, my mind answers from RAM.", game),
            (true, None) => "Game mode is on, Sir: 0 MB of VRAM, my mind answers from RAM.".to_string(),
            (false, _) => "Game mode is off, Sir. It switches itself on when a game starts.".to_string(),
        }),
        _ => Ok(status_line()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn he_is_back_when_input_comes_after_a_while_away() {
        // At the PC all along: never "back".
        assert_eq!(came_back(false, 3, 8), (false, false));
        assert_eq!(came_back(false, 8, 1), (false, false));
        // Five minutes without input: away; the hand on the mouse: back, once.
        assert_eq!(came_back(false, 299, 300), (true, false));
        assert_eq!(came_back(true, 300, 1800), (true, false));
        assert_eq!(came_back(true, 1800, 2), (false, true));
        assert_eq!(came_back(false, 2, 0), (false, false));
    }

    /// Repro 2026-11-03: every LLM turn after a pause hit a cold model (keep_alive 90 s, idle
    /// eviction 90 s), and BatmanAK.exe filled the GPU without counting as a game.
    #[test]
    fn the_model_stays_resident_for_30_minutes_and_batman_is_a_game() {
        let cfg = JaiConfig::default();
        assert_eq!(keep_alive_for(&cfg, false), json!("30m"));
        assert!(cfg.vram_guard.idle_secs >= 1800, "idle eviction after {} s", cfg.vram_guard.idle_secs);
        assert!(cfg.vram_guard.game_processes.iter().any(|g| g.eq_ignore_ascii_case("BatmanAK.exe")));
    }

    #[test]
    fn the_morning_warm_up_knows_his_usual_first_word() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let day = |d: u32, minute: u32| (format!("2026-11-{d:02}"), json!(minute), String::new());
        // Today and anything older than a week don't count; the middle of the rest does.
        let days = [day(16, 300), day(15, 450), day(14, 460), day(13, 455), day(2, 100)];
        assert_eq!(usual_first_use(&days, today), Some(455));
        assert_eq!(usual_first_use(&days[..3], today), None, "two days are no habit");
    }

    #[test]
    fn keep_alive_never_forever() {
        let cfg = |keep_alive, enabled| {
            let mut c = JaiConfig { keep_alive, ..JaiConfig::default() };
            c.vram_guard.enabled = enabled;
            c
        };
        assert_eq!(keep_alive_for(&cfg(-1, true), false), json!("30m"));
        assert_eq!(keep_alive_for(&cfg(30, true), false), json!("30s"));
        assert_eq!(keep_alive_for(&cfg(-1, false), false), json!(-1));
        assert_eq!(keep_alive_for(&cfg(30, true), true), json!(-1), "in a game it waits in RAM");
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

        // 2026-11-15 repro: in a game the ears and the voice were unloaded although they run on
        // the CPU (the next order waited ~8 s for Whisper), and every turn put the model back
        // onto the GPU mid-game (the lag).
        GAME_MODE.store(true, Ordering::Release);
        flush_with_reason("test game");
        assert!(is_dormant());
        assert_eq!(worker_action(true, &mut stt_epoch, recent, None), WorkerAction::Keep, "in a game the ears stay");
        assert_eq!(worker_action(false, &mut tts_epoch, recent, None), WorkerAction::Reload, "and come back if they were out");
        touch();
        wake("orb");
        assert!(is_dormant(), "a turn or a click in a game never loads the GPU");
        assert!(badge().0.starts_with("GAME MODE · VRAM 0 MB"));
        // Ollama dropped the RAM copy after 30 quiet minutes: the next answer, and the reload
        // after the game, came from the disk (30-117 s).
        assert_eq!(keep_alive_value(&JaiConfig::default()), json!(-1), "the brain stays in RAM the whole game");
        // 2026-11-15: leaving game mode ended "game" ~20 ms before "standby"; a worker that looked in
        // between dropped Whisper or Kokoro (~8 s to reload). Here a worker looks at every log line.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let looks = Arc::clone(&seen);
        let quiet = Instant::now() - DORMANT_LINGER;
        let worker = tracing_subscriber::fmt()
            .with_writer(move || {
                let mut epoch = flush_epoch();
                looks.lock().unwrap().push(worker_action(true, &mut epoch, quiet, None));
                std::io::sink()
            })
            .finish();
        tracing::subscriber::with_default(worker, || leave_game("test"));
        let seen = seen.lock().unwrap().clone();
        assert!(!seen.is_empty() && !seen.contains(&WorkerAction::Unload), "leaving game mode keeps the ears and the voice: {seen:?}");
        assert!(!is_dormant(), "after the game the model goes back to VRAM");
    }

    #[test]
    fn eviction_payload_is_exact() {
        assert_eq!(eviction_payload("qwen3:30b-a3b"), json!({ "model": "qwen3:30b-a3b", "prompt": "", "keep_alive": 0 }));
        assert_eq!(eviction_payload("x")["keep_alive"], 0);
    }

    #[test]
    fn countdown_is_the_idle_window() {
        let mut s = VramGuardState::new();
        assert_eq!(s.seconds_to_clear(), Some(RESIDENT_SECS));
        s.power = VramPowerState::ZeroVramDormant;
        assert_eq!(s.seconds_to_clear(), None);
    }

    /// 2026-11-15: only ten exes counted as games; any program started from a launcher's game
    /// folder does now, and launchers, wallpapers and helpers there do not.
    #[test]
    fn games_are_found_by_name_or_folder() {
        let listed: Vec<String> = VramGuardConfig::default().game_processes;
        let t = |path: &str, name: &str| game_title(path, name, &listed);
        assert_eq!(t(r"D:\SteamLibrary\steamapps\common\Grand Theft Auto V\GTA5.exe", "GTA5.exe").as_deref(), Some("Grand Theft Auto V"));
        assert_eq!(t(r"C:\Program Files\Epic Games\BatmanArkhamKnight\Binaries\Win64\BatmanAK.exe", "BatmanAK.exe").as_deref(), Some("BatmanArkhamKnight"));
        assert_eq!(t(r"C:\XboxGames\Forza Horizon 5\Content\ForzaHorizon5.exe", "ForzaHorizon5.exe").as_deref(), Some("Forza Horizon 5"));
        assert_eq!(t(r"C:\Users\max\AppData\Roaming\.minecraft\runtime\java-runtime-gamma\bin\javaw.exe", "javaw.exe").as_deref(), Some("Minecraft"));
        assert_eq!(t("", "GTA5.exe").as_deref(), Some("GTA5"), "listed, path unreadable");
        // Not games: Wallpaper Engine lives under steamapps too; launchers, helpers, Steam itself.
        assert_eq!(t(r"C:\Program Files (x86)\Steam\steamapps\common\wallpaper_engine\wallpaper64.exe", "wallpaper64.exe"), None);
        assert_eq!(t(r"C:\Program Files (x86)\Steam\steamapps\common\SteamVR\bin\win64\vrserver.exe", "vrserver.exe"), None);
        assert_eq!(t(r"C:\Program Files\Epic Games\Launcher\Portal\Binaries\Win64\EpicGamesLauncher.exe", "EpicGamesLauncher.exe"), None);
        assert_eq!(t(r"C:\Riot Games\Riot Client\RiotClientServices.exe", "RiotClientServices.exe"), None);
        assert_eq!(t(r"C:\Program Files (x86)\Steam\steam.exe", "steam.exe"), None);
        assert_eq!(t(r"C:\Program Files\Mozilla Firefox\firefox.exe", "firefox.exe"), None);
        // A file directly in a game folder is no install folder of its own.
        assert_eq!(t(r"D:\SteamLibrary\steamapps\common\readme.exe", "readme.exe"), None);
    }

    /// 2026-11-15: "game mode off" in a heavy game looped every few seconds: each wake found the
    /// GPU full and switched game mode back on, and the next poll, which obeys "off", left it again.
    #[test]
    fn a_full_gpu_never_overrules_game_mode_off() {
        // Said with no game running (a card filled by another program): it holds all the same.
        set_game_mode(Some(false));
        assert_eq!(MANUAL.load(Ordering::Acquire), 2);
        MANUAL.store(0, Ordering::Release);
        let mut s = VramGuardState::new();
        assert!(s.may_squeeze(0), "a full GPU keeps the brain in RAM");
        assert!(!s.may_squeeze(2), "but never against Sir's \"game mode off\"");
        s.config.enabled = false;
        assert!(!s.may_squeeze(0), "nor with the guard switched off");
    }

    /// 2026-11-15: short on RAM in a game the brain left under 6 GB free and came back over 7 GB,
    /// but leaving freed ~4 GB by itself: it loaded and unloaded every few seconds.
    #[test]
    fn the_brain_comes_back_into_ram_only_with_room_for_itself() {
        let left_at = RAM_FLOOR_MB - 200;
        assert!(!ram_has_room(left_at + 4_000), "the RAM it freed itself is no room to come back");
        assert!(ram_has_room(RAM_FLOOR_MB + MODEL_VRAM_GUESS_MB + 500), "it comes back once the game gives RAM back");
    }

    /// 2026-11-15: "game mode off" did not hold: when the game closed (or at once, said in the 30 s
    /// after one) the grace after a game switched game mode back on.
    #[test]
    fn the_grace_never_switches_game_mode_back_on() {
        let mut s = VramGuardState::new();
        s.game_left = Some(Instant::now());
        assert!(s.wants_game(0, false, true), "game mode outlasts the game by the grace");
        assert!(!s.wants_game(0, false, false), "but after Sir's \"off\" the grace never brings it back");
    }

    /// 2026-11-15: what a load onto the GPU needs was the most VRAM all models took at once (~10 GB
    /// the time the fallback model ran beside the main one): a normal wake found the card "full"
    /// and he answered on the CPU without a game.
    #[test]
    fn the_gpu_need_is_the_largest_single_model() {
        let mut s = VramGuardState::new();
        s.resident_models = vec![("main-model".into(), 5_000), ("fallback-model".into(), 5_200)];
        s.note_model_vram();
        assert_eq!(s.model_vram_mb, 5_200, "two models at once are no measure of one load");
        s.resident_models = vec![("main-model".into(), 4_800)];
        s.note_model_vram();
        assert_eq!(s.model_vram_mb, 5_200, "the largest one is remembered");
    }
}
