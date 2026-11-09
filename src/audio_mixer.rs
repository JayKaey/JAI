//! Windows' volume mixer by voice: the master volume of the default output device and the
//! per-application audio sessions ("set Sonora to 30 % and Discord to 100 %", "mach Spotify auf
//! 20 Prozent", "mute Chrome") through Core Audio: IAudioEndpointVolume for the master,
//! IAudioSessionManager2 → IAudioSessionEnumerator → IAudioSessionControl2 → ISimpleAudioVolume
//! for every app on every active output device.
//!
//! Three layers, so a volume request never silently fails:
//! 1. Core Audio COM on the Win32 worker thread, with COM (re)initialised on the calling thread
//!    first (`CoInitializeEx` — a thread without it fails with CO_E_NOTINITIALIZED) and the
//!    console endpoint falling back to the multimedia one. Every session of a matching app is
//!    changed (Discord and Chrome run several audio processes), matched by executable, session
//!    display name and session identifier, then by window title and a close spelling.
//! 2. The same endpoint calls from a hidden PowerShell with inline C# (CREATE_NO_WINDOW) when COM
//!    fails inside Jarvis.
//! 3. The keyboard's volume keys (2 % per step) as the last resort for the master volume.

use crate::tools::{bool_arg, str_arg};
use serde_json::{json, Value};
use tracing::warn;

/// One application's audio session.
#[derive(Debug, Clone, PartialEq)]
pub struct AppSession {
    /// The process' executable name without ".exe" ("Discord", "chrome"), or the session's
    /// display name.
    pub name: String,
    pub pid: u32,
    /// 0.0-1.0.
    pub level: f32,
    pub muted: bool,
    /// Other names of the session: its display name and the executable in its session
    /// identifier (protected processes hide their path from `OpenProcess`).
    pub labels: Vec<String>,
}

/// One output device and what plays on it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputDevice {
    pub name: String,
    /// Windows' default output: where an app plays that was told nothing else.
    pub default: bool,
    /// 0.0-1.0.
    pub level: f32,
    pub muted: bool,
    /// The device's own meter right now (0-1): above silence, sound is leaving through it.
    pub peak: f32,
    /// The apps with a live session on it.
    pub apps: Vec<String>,
}

/// One fader of the HUD's Mixer block.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    /// What the mixer knows it by ("master", "discord").
    pub target: String,
    /// What the block calls it.
    pub label: String,
    pub percent: u8,
    pub muted: bool,
}

/// The name the master volume goes by.
const MASTER: &str = "master";

impl Channel {
    pub fn is_master(&self) -> bool {
        self.target == MASTER
    }
}

/// Names Sir uses for an app, and the executables they mean (lower case, no ".exe").
const ALIASES: &[(&str, &[&str])] = &[
    ("sonora", &["sonora"]),
    ("spotify", &["spotify"]),
    ("discord", &["discord", "discordptb", "discordcanary"]),
    ("chrome", &["chrome"]),
    ("google chrome", &["chrome"]),
    ("firefox", &["firefox"]),
    ("edge", &["msedge"]),
    ("microsoft edge", &["msedge"]),
    ("brave", &["brave"]),
    ("opera", &["opera", "opera_gx"]),
    ("opera gx", &["opera"]),
    ("browser", &["chrome", "msedge", "firefox", "brave", "opera"]),
    ("youtube", &["chrome", "msedge", "firefox", "brave", "opera"]),
    ("the browser", &["chrome", "msedge", "firefox", "brave", "opera"]),
    ("music", &["sonora", "spotify"]),
    ("musik", &["sonora", "spotify"]),
    ("the music", &["sonora", "spotify"]),
    ("steam", &["steam", "steamwebhelper"]),
    ("vlc", &["vlc"]),
    ("teams", &["ms-teams", "teams"]),
    ("obs", &["obs64", "obs"]),
    ("whatsapp", &["whatsapp", "whatsapp.root"]),
    ("telegram", &["telegram"]),
    ("skype", &["skype"]),
    ("minecraft", &["javaw", "minecraft"]),
    ("game", &[GAME]),
    ("spiel", &[GAME]),
    ("das spiel", &[GAME]),
    ("jarvis", &["jarvis"]),
    ("system sounds", &["system sounds"]),
    ("systemsounds", &["system sounds"]),
];

/// "The game": no name of its own, it is what the mixer finds at that moment (`game_sessions`).
const GAME: &str = "<the game>";

/// Everyday programs with a mixer channel: whatever else has one is "the game". (So a game
/// nobody listed still counts; a routers' session never does, see `keeps_playing`.)
const NOT_GAMES: &[&str] = &[
    "firefox", "chrome", "msedge", "brave", "opera", "vivaldi", "discord", "teamspeak", "ts3client", "mumble", "guilded", "sonora", "spotify", "vlc", "itunes", "foobar2000", "ms-teams", "teams", "zoom",
    "obs", "whatsapp", "telegram", "skype", "steam", "epicgameslauncher", "battle.net", "system sounds", "explorer", "applicationframehost", "shellexperiencehost", "searchhost", "nvcontainer", "nvidia",
    "claude", "code", "audiodg",
    // The agents hardware makers leave running: a channel, and no game behind it.
    "lghub", "logi", "razer", "corsair", "icue", "armoury", "asus", "msi", "nahimic", "realtek", "rtk", "powertoys", "onedrive", "dropbox", "streamdeck", "elgato", "voicemeeter", "steelseries",
];

/// What "the game" is right now, as the mixer names it: the known games among the sessions,
/// else everything that is no everyday program.
fn game_sessions(apps: &[AppSession]) -> Vec<String> {
    let left: Vec<String> = apps.iter().map(|a| a.name.to_lowercase()).filter(|name| !keeps_playing(name) && !NOT_GAMES.iter().any(|program| name.starts_with(program))).collect();
    let known: Vec<String> = left.iter().filter(|name| crate::ears::is_game_or_voice_chat(name)).cloned().collect();
    if known.is_empty() {
        left
    } else {
        known
    }
}

fn clean(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .trim_end_matches(".exe")
        .trim_start_matches("the ")
        .trim_start_matches("my ")
        .trim_end_matches(" app")
        .trim_end_matches(" volume")
        .trim()
        .to_string()
}

/// Things Sir sets "to N %" that are not apps (the HUD, the voice, simulations).
const NOT_APPS: &[&str] = &[
    "volume", "it", "sound", "the sound", "everything", "timer", "brightness", "zoom", "zoom level", "opacity", "transparency", "scale", "text",
    "font", "font size", "text size", "orb", "the orb", "background", "window", "interface", "gui", "hud", "angle", "velocity", "gravity", "height",
    "speed", "voice", "your voice", "yourself", "you", "battery", "fan", "fans", "light", "lights", "mic", "microphone", "darkness", "rounding",
    // The whole PC: the master volume, never an app.
    "system", "system volume", "master", "master volume", "pc", "computer", "rechner", "audio", "loudness", "lautstärke",
];

/// One of the names Sir uses for an app ("browser", "discord", "music").
pub fn is_alias(name: &str) -> bool {
    let n = clean(name);
    ALIASES.iter().any(|(alias, _)| *alias == n)
}

/// A name the mixer understands: a known alias or an installed application. (Looking a name
/// up among the installed applications costs about a millisecond; what was asked in the last
/// ten minutes is remembered, so a sentence is never parsed slower the second time.)
pub fn is_app_name(name: &str) -> bool {
    static SEEN: std::sync::Mutex<Vec<(String, bool, std::time::Instant)>> = std::sync::Mutex::new(Vec::new());
    let n = clean(name);
    if n.is_empty() || n.split_whitespace().count() > 3 || NOT_APPS.contains(&n.as_str()) {
        return false;
    }
    if ALIASES.iter().any(|(alias, _)| *alias == n) {
        return true;
    }
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    seen.retain(|(_, _, at)| at.elapsed() < std::time::Duration::from_secs(600));
    if let Some((_, known, _)) = seen.iter().find(|(asked, _, _)| *asked == n) {
        return *known;
    }
    let known = crate::appindex::is_known(&n) || crate::apps::find_spec(&n).is_some();
    if seen.len() >= 256 {
        seen.remove(0);
    }
    seen.push((n, known, std::time::Instant::now()));
    known
}

/// The apps a phrase names, one by one ("discord and spotify", "chrome, firefox").
fn parts(name: &str) -> Vec<String> {
    name.split(',').flat_map(|p| p.split(" and ")).flat_map(|p| p.split(" und ")).map(clean).filter(|p| !p.is_empty()).collect()
}

/// The executable names `name` means (several apps: all of theirs).
fn targets(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in parts(name) {
        match ALIASES.iter().find(|(alias, _)| *alias == n) {
            Some((_, exes)) => out.extend(exes.iter().map(|e| e.to_string())),
            None => out.push(n.replace(' ', "")),
        }
    }
    out
}

/// "Browser", "Discord and Spotify": the apps as Sir named them, for the answer.
fn spoken(name: &str) -> String {
    parts(name).iter().map(|p| display_name(p)).collect::<Vec<_>>().join(" and ")
}

/// Sessions "mute everything except …" never mutes: Jarvis' own voice, and the audio routers
/// whose one session carries every other app's sound to the headphones (muting Wave Link's
/// session would silence the app that was to stay).
fn keeps_playing(session: &str) -> bool {
    let s = session.to_lowercase();
    ["jarvis", "wavelink", "elgato", "voicemeeter", "sonar", "goxlr", "audiorelay", "vb-audio"].iter().any(|k| s.contains(k))
}

/// A change to a volume: a level (0-1), a step from where it is (a signed fraction) and/or a
/// mute state. The default changes nothing (and still writes what is there: the write path's
/// check).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Change {
    pub level: Option<f32>,
    pub step: Option<f32>,
    pub mute: Option<bool>,
}

impl Change {
    /// The (level, muted) a volume is left in. Raising it ends a mute; lowering never does.
    pub fn on(&self, (level, muted): (f32, bool)) -> (f32, bool) {
        let next = (self.level.unwrap_or(level) + self.step.unwrap_or(0.0)).clamp(0.0, 1.0);
        let raised = self.level.is_some_and(|l| l > 0.0) || self.step.is_some_and(|s| s > 0.0);
        (next, self.mute.unwrap_or(muted && !raised))
    }
}

/// A volume before and after a change.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub name: String,
    pub before: (f32, bool),
    pub after: (f32, bool),
}

/// A pretend mixer that takes Core Audio's place while it is set: the tests and the self-test
/// run every volume scenario against it, so no real level is touched and nothing is heard.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bench {
    pub master: (f32, bool),
    pub apps: Vec<AppSession>,
}

static BENCH: std::sync::Mutex<Option<Bench>> = std::sync::Mutex::new(None);

fn bench_lock() -> std::sync::MutexGuard<'static, Option<Bench>> {
    BENCH.lock().unwrap_or_else(|e| e.into_inner())
}

/// Puts a pretend mixer in Core Audio's place (None: the real one again); what was there.
pub fn set_bench(bench: Option<Bench>) -> Option<Bench> {
    std::mem::replace(&mut *bench_lock(), bench)
}

/// The pretend mixer as it stands.
pub fn bench() -> Option<Bench> {
    bench_lock().clone()
}

fn bench_master(change: Change) -> Option<Applied> {
    let mut guard = bench_lock();
    let bench = guard.as_mut()?;
    let before = bench.master;
    bench.master = change.on(before);
    Some(Applied { name: "Master".into(), before, after: bench.master })
}

/// The sessions of `wanted` (or, with `others`, every other session that may be muted).
fn bench_apply(wanted: &[String], others: bool, change: Change) -> Option<Vec<Applied>> {
    let mut guard = bench_lock();
    let bench = guard.as_mut()?;
    let mut out = Vec::new();
    for s in bench.apps.iter_mut().filter(|s| if others { !session_matches(s, wanted) && !keeps_playing(&s.name) } else { session_matches(s, wanted) }) {
        let before = (s.level, s.muted);
        (s.level, s.muted) = change.on(before);
        out.push(Applied { name: s.name.clone(), before, after: (s.level, s.muted) });
    }
    Some(out)
}

/// What a volume request is about.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Master,
    /// An app (or several) as Sir named it: "browser", "discord and spotify".
    App(String),
}

/// How long "it" still means what the last volume request was about.
const PRONOUN_WINDOW: std::time::Duration = std::time::Duration::from_secs(180);

/// What the mixer remembers of its own changes.
struct Memory {
    /// What the last request was about ("make it louder").
    last: Option<(Target, std::time::Instant)>,
    /// Each level as it was before the mixer first changed it: what "make everything loud
    /// again" puts back.
    before: Vec<(Target, f32, bool)>,
    /// The sessions "mute everything except …" muted (their levels were left alone): what an
    /// unmute, or the undo, lets play again.
    silenced: Vec<String>,
}

static MEMORY: std::sync::Mutex<Memory> = std::sync::Mutex::new(Memory { last: None, before: Vec::new(), silenced: Vec::new() });

fn memory() -> std::sync::MutexGuard<'static, Memory> {
    MEMORY.lock().unwrap_or_else(|e| e.into_inner())
}

/// The last change alone, volume by volume, as each was just before it: what "undo that" puts
/// back. (One step; the memory's `before` is every level before its *first* change, which is
/// what "make everything loud again" restores.)
static LAST_CHANGE: std::sync::Mutex<Vec<Applied>> = std::sync::Mutex::new(Vec::new());

fn last_change() -> std::sync::MutexGuard<'static, Vec<Applied>> {
    LAST_CHANGE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A change was carried out: the volumes it really moved are the step an undo takes back.
fn note_change(hit: &[Applied]) {
    let moved: Vec<Applied> = hit.iter().filter(|h| h.before != h.after).cloned().collect();
    // (A request that moved nothing, "raise it" at 100 %, leaves the last real change standing.)
    if !moved.is_empty() {
        CHANGES.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        *last_change() = moved;
    }
}

/// How many changes the mixer has carried out so far ("undo that" sees a volume request by
/// this count moving, whichever tool it came through).
static CHANGES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn changes() -> u64 {
    CHANGES.load(std::sync::atomic::Ordering::Acquire)
}

/// Whether the last volume change can still be taken back.
pub fn can_undo() -> bool {
    !last_change().is_empty()
}

/// The last volume change as it was asked for: what "more", "less" and "same for Firefox" go on
/// from, right after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Step {
    /// An app's own volume, not the master.
    pub app: bool,
    pub delta: Option<i64>,
    pub level: Option<i64>,
    pub mute: Option<bool>,
}

/// How long after a volume change a bare "more" still means the volume.
const STEP_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

static LAST_STEP: std::sync::Mutex<Option<(Step, std::time::Instant)>> = std::sync::Mutex::new(None);

fn last_step_slot() -> std::sync::MutexGuard<'static, Option<(Step, std::time::Instant)>> {
    LAST_STEP.lock().unwrap_or_else(|e| e.into_inner())
}

fn note_step(app: bool, change: Change) {
    let points = |share: f32| (share * 100.0).round() as i64;
    *last_step_slot() = Some((Step { app, delta: change.step.map(points), level: change.level.map(points), mute: change.mute }, std::time::Instant::now()));
}

/// The change a short follow-up is about, while it is fresh.
pub fn last_step() -> Option<Step> {
    last_step_slot().filter(|(_, at)| at.elapsed() < STEP_WINDOW).map(|(step, _)| step)
}

/// Any other request: "more" means something else from here on.
pub fn forget_step() {
    *last_step_slot() = None;
}

/// Forgets what was changed and what "it" means (a new bench starts clean).
pub fn forget() {
    let mut m = memory();
    m.last = None;
    m.before.clear();
    m.silenced.clear();
    drop(m);
    timed_mutes().clear();
    faded().clear();
    FADE_RUN.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    last_change().clear();
    forget_step();
}

/// The sessions "mute everything except …" muted play again (and are forgotten).
async fn unsilence() {
    let silenced = std::mem::take(&mut memory().silenced);
    if !silenced.is_empty() {
        let wanted: Vec<String> = silenced.iter().map(|s| s.to_lowercase()).collect();
        let _ = apps_apply(wanted, false, Change { mute: Some(false), ..Change::default() }).await;
    }
}

/// The master volume, or an app's session by its own name ("firefox", whether Sir said
/// "browser" or "Firefox"), was changed from `before`: its first state is kept for the undo.
fn remember(target: Target, before: (f32, bool)) {
    let mut m = memory();
    if !m.before.iter().any(|(t, _, _)| *t == target) {
        m.before.push((target, before.0, before.1));
    }
}

/// What "it" means from now on.
fn about(target: Target) {
    memory().last = Some((target, std::time::Instant::now()));
}

/// Whom a request is about: the master volume, the app Sir named, or, for "it", what the last
/// request was about (the master volume when that is long ago).
fn target_of(app: Option<&str>) -> Target {
    let Some(named) = app.map(clean).filter(|a| !a.is_empty()) else { return Target::Master };
    if matches!(named.as_str(), "it" | "that" | "this" | "them" | "those" | "es" | "das" | "last") {
        return memory().last.clone().filter(|(_, at)| at.elapsed() < PRONOUN_WINDOW).map_or(Target::Master, |(t, _)| t);
    }
    if NOT_APPS.contains(&named.as_str()) {
        return Target::Master;
    }
    Target::App(named)
}

/// Whether a session belongs to an app Sir named.
fn matches_target(session: &str, wanted: &[String]) -> bool {
    let s = session.to_lowercase().replace(' ', "");
    wanted.iter().any(|w| {
        let w = w.replace(' ', "");
        s == w || (w.len() >= 4 && (s.starts_with(&w) || s.contains(&w))) || (s.len() >= 4 && w.starts_with(&s))
    })
}

/// A session Sir named by its executable, display name or session identifier.
fn session_matches(session: &AppSession, wanted: &[String]) -> bool {
    matches_target(&session.name, wanted) || session.labels.iter().any(|l| matches_target(l, wanted))
}

/// The second pass, when no session matched by name: a window of the session's process whose
/// title names the app ("YouTube – Google Chrome"), or a close spelling of the executable
/// ("dicsord", "sonoraa"; Jaro-Winkler ≥ 0.88 with the same first letter).
fn session_matches_loosely(session: &AppSession, wanted: &[String], titles: &[String]) -> bool {
    let exe = session.name.to_lowercase().replace(' ', "");
    titles.iter().any(|t| {
        let t = t.to_lowercase();
        wanted.iter().any(|w| w.len() >= 4 && t.contains(w.as_str()))
    }) || wanted.iter().any(|w| {
        let w = w.replace(' ', "");
        w.len() >= 4 && exe.chars().next() == w.chars().next() && strsim::jaro_winkler(&exe, &w) >= 0.88
    })
}

/// The executable named in a session identifier ("…|\Device\HarddiskVolume3\…\Spotify.exe%b{…}"
/// → "Spotify").
#[cfg_attr(not(windows), allow(dead_code))]
fn exe_in_identifier(identifier: &str) -> Option<String> {
    let path = identifier.split('|').nth(1)?;
    let file = path.rsplit(['\\', '/']).next()?;
    let file = file.split('%').next()?;
    let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file);
    (!stem.is_empty()).then(|| stem.to_string())
}

/// "Discord" for "discord", "Sonora" for "sonora.exe".
fn display_name(exe: &str) -> String {
    const SPOKEN: &[(&str, &str)] = &[("javaw", "Minecraft"), ("lghub_agent", "Logitech G Hub"), ("lghub", "Logitech G Hub"), ("msedge", "Microsoft Edge"), ("steamwebhelper", "Steam")];
    if let Some((_, name)) = SPOKEN.iter().find(|(known, _)| exe.eq_ignore_ascii_case(known)) {
        return (*name).to_string();
    }
    let spaced = exe.replace('_', " ");
    let mut c = spaced.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

#[cfg(windows)]
mod imp {
    use super::AppSession;
    use windows::core::Interface;
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioMeterInformation};
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, EDataFlow, AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl2, IAudioSessionManager2, IMMDevice,
        IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};
    use windows::Win32::Media::Audio::eMultimedia;

    fn err(e: windows::core::Error) -> String {
        e.message().to_string()
    }

    /// COM on the calling thread. The Win32 worker initialised it already (the call then returns
    /// S_FALSE or RPC_E_CHANGED_MODE, both harmless); any other thread gets it here, so no call
    /// fails with CO_E_NOTINITIALIZED (0x800401F0).
    fn ensure_com() {
        // SAFETY: plain per-thread initialisation; repeated calls only report the existing state.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    }

    /// A UTF-16 string the API allocated, copied and freed.
    unsafe fn take_string(p: windows::core::PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        // SAFETY: `p` is a valid, NUL-terminated CoTaskMem string from the audio API.
        let s = unsafe { p.to_string() }.unwrap_or_default();
        // SAFETY: the API requires the caller to free it with CoTaskMemFree.
        unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
        s
    }

    fn master() -> Result<IAudioEndpointVolume, String> {
        ensure_com();
        // SAFETY: COM is initialised on this thread (above).
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(err)?;
            // The console endpoint, else the multimedia one (some drivers only expose that role).
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .or_else(|_| enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia))
                .map_err(err)?;
            device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).map_err(err)
        }
    }

    pub fn master_state() -> Result<(f32, bool), String> {
        let ep = master()?;
        // SAFETY: valid interface from `master`.
        unsafe { Ok((ep.GetMasterVolumeLevelScalar().map_err(err)?, ep.GetMute().map_err(err)?.as_bool())) }
    }

    /// Writes the master volume's level and mute state where they differ from `before` (with
    /// `all`, both whatever they are: the write path's check, which changes nothing).
    pub fn master_write(before: (f32, bool), (level, muted): (f32, bool), all: bool) -> Result<(), String> {
        let ep = master()?;
        // SAFETY: valid interface; a null event context is allowed.
        unsafe {
            if all || (level - before.0).abs() > 1e-4 {
                ep.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null()).map_err(err)?;
            }
            if all || muted != before.1 {
                ep.SetMute(BOOL::from(muted), std::ptr::null()).map_err(err)?;
            }
        }
        Ok(())
    }

    /// Every active output device (a headset and speakers each have their own sessions).
    fn render_devices() -> Result<Vec<IMMDevice>, String> {
        endpoints(eRender)
    }

    /// Every active device of one direction (outputs or microphones).
    fn endpoints(flow: EDataFlow) -> Result<Vec<IMMDevice>, String> {
        ensure_com();
        // SAFETY: COM is initialised on this thread; indices come from GetCount.
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(err)?;
            let collection = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE).map_err(err)?;
            let count = collection.GetCount().map_err(err)?;
            Ok((0..count).filter_map(|i| collection.Item(i).ok()).collect())
        }
    }

    /// Every live session with its volume control.
    fn sessions() -> Result<Vec<(AppSession, ISimpleAudioVolume)>, String> {
        let mut out = Vec::new();
        for device in render_devices()? {
            // SAFETY: interfaces come from the enumerations above; strings the API allocates are
            // freed with CoTaskMemFree (`take_string`).
            unsafe {
                let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                let Ok(list) = manager.GetSessionEnumerator() else { continue };
                let count = list.GetCount().unwrap_or(0);
                for i in 0..count {
                    let Ok(control) = list.GetSession(i) else { continue };
                    if control.GetState().map(|s| s == AudioSessionStateExpired).unwrap_or(true) {
                        continue;
                    }
                    let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                    let Ok(volume) = control.cast::<ISimpleAudioVolume>() else { continue };
                    let pid = control2.GetProcessId().unwrap_or(0);
                    let system = control2.IsSystemSoundsSession().0 == 0;
                    let display = control2.GetDisplayName().map(|p| take_string(p)).unwrap_or_default();
                    let identifier = control2.GetSessionIdentifier().map(|p| take_string(p)).unwrap_or_default();
                    let in_identifier = super::exe_in_identifier(&identifier).unwrap_or_default();
                    let name = if system {
                        "System sounds".to_string()
                    } else {
                        let exe = crate::win32::process_path(pid)
                            .and_then(|p| std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().to_string()))
                            .unwrap_or_default();
                        [exe, in_identifier.clone(), display.clone()].into_iter().find(|n| !n.is_empty()).unwrap_or_default()
                    };
                    if name.is_empty() {
                        continue;
                    }
                    let labels: Vec<String> = [display, in_identifier].into_iter().filter(|l| !l.is_empty() && !l.starts_with('@') && *l != name).collect();
                    let level = volume.GetMasterVolume().unwrap_or(1.0);
                    let muted = volume.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                    out.push((AppSession { name, pid, level, muted, labels }, volume));
                }
            }
        }
        Ok(out)
    }

    /// The friendly name of an output device ("Voice chat (Elgato Virtual Audio)").
    unsafe fn friendly_name(device: &IMMDevice) -> Option<String> {
        use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
        use windows::Win32::System::Com::STGM_READ;
        use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
        // PKEY_Device_FriendlyName.
        let key = PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
        // SAFETY: the device comes from the enumerator; the returned string is CoTaskMem-owned.
        unsafe {
            let store = device.OpenPropertyStore(STGM_READ).ok()?;
            let value = store.GetValue(&key).ok()?;
            let text = PropVariantToStringAlloc(&value).ok()?;
            Some(take_string(text)).filter(|s| !s.is_empty())
        }
    }

    /// The output device `app` (a process name like "Discord") plays to: the device holding its
    /// active session, else any of its sessions. Read-only: nothing is moved or changed.
    pub fn output_device_of(app: &str) -> Result<Option<String>, String> {
        device_of(app, eRender)
    }

    /// The microphone `app` records from (only while it records: Discord in a call).
    pub fn input_device_of(app: &str) -> Result<Option<String>, String> {
        device_of(app, eCapture)
    }

    fn device_of(app: &str, flow: EDataFlow) -> Result<Option<String>, String> {
        let mut fallback: Option<String> = None;
        for device in endpoints(flow)? {
            // SAFETY: interfaces come from the enumerations above.
            unsafe {
                let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                let Ok(list) = manager.GetSessionEnumerator() else { continue };
                for i in 0..list.GetCount().unwrap_or(0) {
                    let Ok(control) = list.GetSession(i) else { continue };
                    let state = control.GetState().unwrap_or(AudioSessionStateExpired);
                    if state == AudioSessionStateExpired {
                        continue;
                    }
                    let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                    let pid = control2.GetProcessId().unwrap_or(0);
                    let exe = crate::win32::process_path(pid)
                        .and_then(|p| std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().to_string()))
                        .unwrap_or_default();
                    if !exe.eq_ignore_ascii_case(app) {
                        continue;
                    }
                    if state == AudioSessionStateActive {
                        return Ok(friendly_name(&device));
                    }
                    if fallback.is_none() {
                        fallback = friendly_name(&device);
                    }
                }
            }
        }
        Ok(fallback)
    }

    /// Every active output device: its level, whether sound is leaving through it right now,
    /// and the apps that play on it. Read-only.
    pub fn outputs() -> Result<Vec<super::OutputDevice>, String> {
        ensure_com();
        let mut out = Vec::new();
        // SAFETY: COM is initialised on this thread; every interface comes from the enumerator,
        // and strings the API allocates are freed with CoTaskMemFree (`take_string`).
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(err)?;
            let default_id = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .or_else(|_| enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia))
                .ok()
                .and_then(|d| d.GetId().ok())
                .map(|p| take_string(p))
                .unwrap_or_default();
            let collection = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE).map_err(err)?;
            for i in 0..collection.GetCount().map_err(err)? {
                let Ok(device) = collection.Item(i) else { continue };
                let id = device.GetId().map(|p| take_string(p)).unwrap_or_default();
                let (level, muted) = match device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
                    Ok(ep) => (ep.GetMasterVolumeLevelScalar().unwrap_or(0.0), ep.GetMute().map(|b| b.as_bool()).unwrap_or(false)),
                    Err(_) => (0.0, false),
                };
                let peak = device.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None).ok().and_then(|m| m.GetPeakValue().ok()).unwrap_or(0.0);
                let mut apps: Vec<String> = Vec::new();
                if let Ok(list) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None).and_then(|m| m.GetSessionEnumerator()) {
                    for n in 0..list.GetCount().unwrap_or(0) {
                        let Ok(control) = list.GetSession(n) else { continue };
                        if control.GetState().map(|s| s == AudioSessionStateExpired).unwrap_or(true) {
                            continue;
                        }
                        let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                        let name = if control2.IsSystemSoundsSession().0 == 0 {
                            "System sounds".to_string()
                        } else {
                            crate::win32::process_path(control2.GetProcessId().unwrap_or(0))
                                .and_then(|p| std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().to_string()))
                                .or_else(|| control2.GetSessionIdentifier().ok().map(|p| take_string(p)).and_then(|id| super::exe_in_identifier(&id)))
                                .unwrap_or_default()
                        };
                        if !name.is_empty() && !apps.iter().any(|a| a.eq_ignore_ascii_case(&name)) {
                            apps.push(name);
                        }
                    }
                }
                out.push(super::OutputDevice { name: friendly_name(&device).unwrap_or_else(|| "Unnamed output".to_string()), default: !id.is_empty() && id == default_id, level, muted, peak, apps });
            }
        }
        Ok(out)
    }

    pub fn list() -> Result<Vec<AppSession>, String> {
        let mut seen: Vec<AppSession> = Vec::new();
        for (s, _) in sessions()? {
            // One row per app (Chrome has a session per tab process and per device).
            if !seen.iter().any(|x| x.name.eq_ignore_ascii_case(&s.name)) {
                seen.push(s);
            }
        }
        Ok(seen)
    }

    /// (process name, pid) of every app whose audio is playing right now: an active session with
    /// a peak above silence on any output device (system sounds excluded).
    pub fn audible() -> Result<Vec<(String, u32)>, String> {
        let mut out: Vec<(String, u32)> = Vec::new();
        for device in render_devices()? {
            // SAFETY: interfaces come from the enumerations above; COM is initialised on the
            // Win32 worker thread these functions run on.
            unsafe {
                let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                let Ok(list) = manager.GetSessionEnumerator() else { continue };
                for i in 0..list.GetCount().unwrap_or(0) {
                    let Ok(control) = list.GetSession(i) else { continue };
                    if control.GetState().map(|s| s != AudioSessionStateActive).unwrap_or(true) {
                        continue;
                    }
                    let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                    // S_OK (0) means this is the system sounds session.
                    if control2.IsSystemSoundsSession().0 == 0 {
                        continue;
                    }
                    let Ok(meter) = control.cast::<IAudioMeterInformation>() else { continue };
                    if meter.GetPeakValue().unwrap_or(0.0) < 0.001 {
                        continue;
                    }
                    let pid = control2.GetProcessId().unwrap_or(0);
                    let name = crate::win32::process_path(pid)
                        .and_then(|p| std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().to_string()))
                        .unwrap_or_default();
                    if pid != 0 && !name.is_empty() && !out.iter().any(|(_, p)| *p == pid) {
                        out.push((name, pid));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Applies `change` to every session of the wanted apps (all of them: Discord and Chrome
    /// play through several processes) or, with `others`, to every other session that may be
    /// muted. One entry per app: how it was and how it is now.
    pub fn apply(wanted: &[String], others: bool, change: super::Change) -> Result<Vec<super::Applied>, String> {
        let all = sessions()?;
        let mut chosen: Vec<&(AppSession, ISimpleAudioVolume)> = if others {
            all.iter().filter(|(s, _)| !super::session_matches(s, wanted) && !super::keeps_playing(&s.name)).collect()
        } else {
            all.iter().filter(|(s, _)| super::session_matches(s, wanted)).collect()
        };
        if chosen.is_empty() && !others {
            // No executable or session name matched: window titles and close spellings.
            let windows = crate::win32::app_windows();
            chosen = all
                .iter()
                .filter(|(s, _)| {
                    let titles: Vec<String> = windows.iter().filter(|w| w.pid == s.pid && s.pid != 0).map(|w| w.title.clone()).collect();
                    super::session_matches_loosely(s, wanted, &titles)
                })
                .collect();
        }
        let touch = change == super::Change::default();
        let mut hit: Vec<super::Applied> = Vec::new();
        let mut failure: Option<String> = None;
        for (s, volume) in chosen {
            let before = (s.level, s.muted);
            let after = change.on(before);
            // SAFETY: valid ISimpleAudioVolume; a null event context is allowed.
            let result = unsafe {
                let mut r = Ok(());
                if touch || (after.0 - before.0).abs() > 1e-4 {
                    r = volume.SetMasterVolume(after.0, std::ptr::null());
                }
                if r.is_ok() && (touch || after.1 != before.1) {
                    r = volume.SetMute(BOOL::from(after.1), std::ptr::null());
                }
                r
            };
            // One stubborn session (a protected process) never stops the others.
            match result {
                Ok(()) => {
                    if !hit.iter().any(|h| h.name.eq_ignore_ascii_case(&s.name)) {
                        hit.push(super::Applied { name: s.name.clone(), before, after });
                    }
                }
                Err(e) => failure = Some(format!("{}: {}", s.name, err(e))),
            }
        }
        match (hit.is_empty(), failure) {
            (true, Some(why)) => Err(why),
            _ => Ok(hit),
        }
    }
}

#[cfg(windows)]
async fn com<R: Send + 'static>(f: impl FnOnce() -> Result<R, String> + Send + 'static) -> Result<R, String> {
    crate::win32::call(f).await?
}

#[cfg(not(windows))]
async fn com<R: Send + 'static>(_f: impl FnOnce() -> Result<R, String> + Send + 'static) -> Result<R, String> {
    Err("The volume mixer needs Windows.".into())
}

#[cfg(not(windows))]
mod imp {
    use super::AppSession;
    pub fn output_device_of(_: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    pub fn input_device_of(_: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    pub fn master_state() -> Result<(f32, bool), String> {
        Err("The volume mixer needs Windows.".into())
    }
    pub fn master_write(_: (f32, bool), _: (f32, bool), _: bool) -> Result<(), String> {
        master_state().map(|_| ())
    }
    pub fn outputs() -> Result<Vec<super::OutputDevice>, String> {
        master_state().map(|_| Vec::new())
    }
    pub fn list() -> Result<Vec<AppSession>, String> {
        master_state().map(|_| Vec::new())
    }
    pub fn apply(_: &[String], _: bool, _: super::Change) -> Result<Vec<super::Applied>, String> {
        master_state().map(|_| Vec::new())
    }
    pub fn audible() -> Result<Vec<(String, u32)>, String> {
        master_state().map(|_| Vec::new())
    }
}

/// 30, "30", "30%" → 0.3 (whole numbers are always per cent, so 1 is 1 %); 0.3 → 0.3.
fn level_of(v: &Value) -> Option<f32> {
    let (x, whole) = match v {
        Value::Number(n) if n.is_u64() || n.is_i64() => (n.as_f64()? as f32, true),
        Value::Number(n) => (n.as_f64()? as f32, false),
        Value::String(s) => {
            let t = s.trim().trim_end_matches('%').trim().trim_end_matches("percent").trim();
            (t.parse().ok()?, !t.contains('.') && !t.contains(','))
        }
        _ => return None,
    };
    Some(if whole || x > 1.0 { x / 100.0 } else { x }.clamp(0.0, 1.0))
}

fn pct(level: f32) -> String {
    format!("{:.0} %", level * 100.0)
}

/// Layers 2 and 3 for the master volume: the endpoint from a hidden PowerShell with inline C#,
/// then the keyboard's volume keys.
#[cfg(windows)]
mod fallback {
    use base64::Engine;
    use std::path::PathBuf;
    use std::time::Duration;

    /// IAudioEndpointVolume through COM interop: the same calls as layer 1, in another process.
    const CSHARP: &str = r#"using System;
using System.Runtime.InteropServices;
[Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IAudioEndpointVolume {
  int f(); int g(); int h(); int i();
  int SetMasterVolumeLevelScalar(float fLevel, Guid pguidEventContext);
  int j();
  int GetMasterVolumeLevelScalar(out float pfLevel);
  int k(); int l(); int m(); int n();
  int SetMute([MarshalAs(UnmanagedType.Bool)] bool bMute, Guid pguidEventContext);
  int GetMute(out bool pbMute);
}
[Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMMDevice {
  int Activate(ref Guid id, int clsCtx, int activationParams, out IAudioEndpointVolume aev);
}
[Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMMDeviceEnumerator {
  int f();
  int GetDefaultAudioEndpoint(int dataFlow, int role, out IMMDevice endpoint);
}
[ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] class MMDeviceEnumeratorComObject { }
public class JarvisAudio {
  static IAudioEndpointVolume Vol() {
    var enumerator = new MMDeviceEnumeratorComObject() as IMMDeviceEnumerator;
    IMMDevice dev = null;
    if (enumerator.GetDefaultAudioEndpoint(0, 0, out dev) != 0) { Marshal.ThrowExceptionForHR(enumerator.GetDefaultAudioEndpoint(0, 1, out dev)); }
    IAudioEndpointVolume epv = null;
    var epvid = typeof(IAudioEndpointVolume).GUID;
    Marshal.ThrowExceptionForHR(dev.Activate(ref epvid, 23, 0, out epv));
    return epv;
  }
  public static void SetVolume(float v) { Marshal.ThrowExceptionForHR(Vol().SetMasterVolumeLevelScalar(Math.Max(0f, Math.Min(1f, v)), Guid.Empty)); }
  public static void SetMute(bool m) { Marshal.ThrowExceptionForHR(Vol().SetMute(m, Guid.Empty)); }
  public static float GetVolume() { float v = -1; Marshal.ThrowExceptionForHR(Vol().GetMasterVolumeLevelScalar(out v)); return v; }
  public static bool GetMute() { bool m; Marshal.ThrowExceptionForHR(Vol().GetMute(out m)); return m; }
}"#;

    fn powershell() -> PathBuf {
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
    }

    /// Runs `commands` after loading the interop class; returns "level|muted" read afterwards.
    async fn run(commands: &str) -> Result<(f32, bool), String> {
        let script = format!(
            "$ErrorActionPreference = 'Stop'\nAdd-Type -TypeDefinition @'\n{CSHARP}\n'@\n{commands}\n[Console]::Out.Write(([JarvisAudio]::GetVolume()).ToString([Globalization.CultureInfo]::InvariantCulture) + '|' + [JarvisAudio]::GetMute())"
        );
        let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
        let args = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encoded.as_str()];
        let out = crate::shell::run_helper(&powershell(), args, None, &[], Duration::from_secs(20)).await.map_err(|e| format!("PowerShell could not start: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.success() {
            let why = String::from_utf8_lossy(&out.stderr).lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            return Err(if why.is_empty() { format!("PowerShell ended with code {:?}", out.code) } else { why });
        }
        let (level, muted) = text.rsplit_once('|').ok_or_else(|| format!("unexpected PowerShell output '{text}'"))?;
        Ok((level.trim().parse().map_err(|_| format!("unexpected volume '{level}'"))?, muted.trim().eq_ignore_ascii_case("true")))
    }

    pub async fn set(level: Option<f32>, mute: Option<bool>, delta: Option<f32>) -> Result<(f32, bool), String> {
        let mut commands = String::new();
        if let Some(d) = delta {
            commands.push_str(&format!("[JarvisAudio]::SetVolume([JarvisAudio]::GetVolume() + ({:.4}))\n", d / 100.0));
        }
        if let Some(l) = level {
            commands.push_str(&format!("[JarvisAudio]::SetVolume({:.4})\n", l.clamp(0.0, 1.0)));
            if l > 0.0 && mute.is_none() {
                commands.push_str("[JarvisAudio]::SetMute($false)\n");
            }
        }
        if let Some(m) = mute {
            commands.push_str(if m { "[JarvisAudio]::SetMute($true)\n" } else { "[JarvisAudio]::SetMute($false)\n" });
        }
        run(&commands).await
    }

    /// The last resort: volume keys (each step is 2 % on Windows). An absolute level goes to
    /// zero first; mute can only be toggled.
    pub async fn keys(level: Option<f32>, mute: Option<bool>, delta: Option<f32>) -> Result<String, String> {
        use crate::win32::{tap_key, VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP};
        crate::win32::call(move || -> Result<String, String> {
            let taps = |key, n: u32| -> Result<(), String> {
                for _ in 0..n {
                    tap_key(key)?;
                    std::thread::sleep(Duration::from_millis(4));
                }
                Ok(())
            };
            if let Some(l) = level {
                taps(VK_VOLUME_DOWN, 50)?;
                taps(VK_VOLUME_UP, (l.clamp(0.0, 1.0) * 50.0).round() as u32)?;
                return Ok(format!("Master volume set to about {:.0} % with the volume keys.", l * 100.0));
            }
            if let Some(d) = delta {
                taps(if d >= 0.0 { VK_VOLUME_UP } else { VK_VOLUME_DOWN }, (d.abs() / 2.0).round().max(1.0) as u32)?;
                return Ok(format!("Master volume {} by about {:.0} % with the volume keys.", if d >= 0.0 { "raised" } else { "lowered" }, d.abs()));
            }
            if let Some(m) = mute {
                taps(VK_VOLUME_MUTE, 1)?;
                return Ok(format!("Toggled the mute key (meant to {}).", if m { "mute" } else { "unmute" }));
            }
            Ok("Nothing to change.".into())
        })
        .await?
    }
}

/// A Core Audio call, asked up to three times: a wireless headset's driver answers a call now
/// and then with "a device attached to the system is not functioning" and takes the next one.
fn retrying<R>(call: impl Fn() -> Result<R, String>) -> Result<R, String> {
    let mut last = call();
    for _ in 0..2 {
        if last.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
        last = call();
    }
    last
}

/// The master volume changed through Core Audio (or on the bench): how it was and is.
async fn master_apply(change: Change) -> Result<Applied, String> {
    if let Some(done) = bench_master(change) {
        return Ok(done);
    }
    com(move || {
        let before = retrying(imp::master_state)?;
        retrying(|| imp::master_write(before, change.on(before), change == Change::default()))?;
        Ok(Applied { name: "Master".into(), before, after: retrying(imp::master_state)? })
    })
    .await
}

/// An app's sessions changed (or, with `others`, every other app's): how each was and is.
async fn apps_apply(wanted: Vec<String>, others: bool, change: Change) -> Result<Vec<Applied>, String> {
    // "The game" is only known to the mixer, at this moment.
    let wanted = if wanted.iter().any(|w| w == GAME) {
        let game = game_sessions(&list_audio_apps().await?);
        if game.is_empty() {
            return Err("No game has a mixer channel right now.".to_string());
        }
        wanted.into_iter().filter(|w| w != GAME).chain(game).collect()
    } else {
        wanted
    };
    if let Some(done) = bench_apply(&wanted, others, change) {
        return Ok(done);
    }
    com(move || retrying(|| imp::apply(&wanted, others, change))).await
}

/// A master-volume change through the three layers; `describe` phrases the result of layer 1
/// or 2 from the level and mute state afterwards.
async fn master_change(level: Option<f32>, mute: Option<bool>, delta: Option<f32>, describe: fn(f32, bool) -> String) -> Result<String, String> {
    let change = Change { level, step: delta.map(|d| d / 100.0), mute };
    match master_apply(change).await {
        Ok(done) => {
            remember(Target::Master, done.before);
            about(Target::Master);
            note_change(std::slice::from_ref(&done));
            // (A mute of everything leaves nothing for "more" or "same for Firefox" to go on from.)
            if level.is_some() || delta.is_some() {
                note_step(false, change);
            }
            let (now, muted) = done.after;
            // The Dynamic-Island volume bar shows where it landed.
            crate::dynamic_popups::volume("Master", (now * 100.0).round() as u8, muted);
            // (A step with nowhere left to go moved nothing, and "done" would be a claim.)
            if delta.is_some() && (now - done.before.0).abs() < 0.005 && muted == done.before.1 {
                return Ok(format!("The master volume is already at {}.", pct(now)));
            }
            Ok(describe(now, muted))
        }
        Err(error) => master_fallback(level, mute, delta, describe, error).await,
    }
}

/// Layers 2 and 3 after Core Audio failed with `error`.
#[cfg(windows)]
async fn master_fallback(level: Option<f32>, mute: Option<bool>, delta: Option<f32>, describe: fn(f32, bool) -> String, error: String) -> Result<String, String> {
    warn!("[MIXER] Core Audio failed ({error}); trying the PowerShell endpoint");
    match fallback::set(level, mute, delta).await {
        Ok((now, muted)) => return Ok(describe(now, muted)),
        Err(e) => warn!("[MIXER] PowerShell endpoint failed ({e}); using the volume keys"),
    }
    fallback::keys(level, mute, delta).await.map_err(|e| format!("{error}; the volume keys failed too ({e})"))
}

#[cfg(not(windows))]
async fn master_fallback(_: Option<f32>, _: Option<bool>, _: Option<f32>, _: fn(f32, bool) -> String, error: String) -> Result<String, String> {
    Err(error)
}

pub async fn set_master_volume(level: f32) -> Result<String, String> {
    master_change(Some(level.clamp(0.0, 1.0)), None, None, |now, _| format!("Master volume set to {}.", pct(now))).await
}

/// `delta` in volume points (-100..100).
pub async fn change_master_volume(delta: f32) -> Result<String, String> {
    let unmute = (delta > 0.0).then_some(false);
    master_change(None, unmute, Some(delta), |now, _| format!("Master volume now {}.", pct(now))).await
}

/// Mutes that end by themselves ("mute Discord for 10 minutes"): whom, and the timer that counts.
/// A later mute or unmute of the same target takes the timer's job away.
static TIMED_MUTES: std::sync::Mutex<Vec<(String, u64)>> = std::sync::Mutex::new(Vec::new());
static MUTE_TIMERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn timed_mutes() -> std::sync::MutexGuard<'static, Vec<(String, u64)>> {
    TIMED_MUTES.lock().unwrap_or_else(|e| e.into_inner())
}

/// The name a target goes by in the mixer's own lists.
fn target_key(target: &Target) -> String {
    match target {
        Target::Master => "Master".to_string(),
        Target::App(app) => clean(app),
    }
}

/// The mute just made ends in `seconds`, without a word and without touching what "it" means.
fn unmute_after(target: Target, seconds: u64) {
    let key = target_key(&target);
    let id = MUTE_TIMERS.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    timed_mutes().push((key, id));
    let on_bench = bench().is_some();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
        let due = {
            let mut timers = timed_mutes();
            let before = timers.len();
            timers.retain(|(_, timer)| *timer != id);
            timers.len() != before
        };
        // (A timer started on the pretend mixer never reaches the real one.)
        if !due || bench().is_some() != on_bench {
            return;
        }
        let back = Change { mute: Some(false), ..Change::default() };
        let done = match &target {
            Target::Master => master_apply(back).await.map(|_| ()),
            Target::App(app) => apps_apply(targets(app), false, back).await.map(|_| ()),
        };
        if let Err(error) = done {
            warn!("[MIXER] A timed mute could not be ended: {error}");
        }
    });
}

/// "10 minutes", "a minute", "half an hour", as a mute's length is said.
fn span(seconds: u64) -> String {
    match seconds {
        60 => "a minute".to_string(),
        1800 => "half an hour".to_string(),
        3600 => "an hour".to_string(),
        s if s % 3600 == 0 => format!("{} hours", s / 3600),
        s if s % 60 == 0 => format!("{} minutes", s / 60),
        s => format!("{s} seconds"),
    }
}

/// How long a fade takes, and in how many steps.
const FADE_TIME: std::time::Duration = std::time::Duration::from_secs(5);
const FADE_STEPS: u32 = 25;

/// The fade that counts: any other, an undo or "loud again" ends one that is still running.
static FADE_RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Where each volume stood before it was faded out: what "fade it back in" goes to.
static FADED: std::sync::Mutex<Vec<(String, f32)>> = std::sync::Mutex::new(Vec::new());

fn faded() -> std::sync::MutexGuard<'static, Vec<(String, f32)>> {
    FADED.lock().unwrap_or_else(|e| e.into_inner())
}

/// One volume set in passing, with nothing remembered: a step of a fade.
async fn set_in_passing(name: &str, change: Change) -> Result<(), String> {
    if name == "Master" {
        master_apply(change).await.map(|_| ())
    } else {
        apps_apply(vec![name.to_lowercase()], false, change).await.map(|_| ())
    }
}

/// How far along a fade is at each of its steps, the last one all the way.
fn fade_points(steps: u32) -> impl Iterator<Item = f32> {
    (1..=steps).map(move |step| step as f32 / steps as f32)
}

/// "Fade the music out", "fade it back in", "turn it down slowly" (`delta`, in volume points):
/// the volume moves over a few seconds, in the background. One step for "undo that", and a volume
/// change he asks for meanwhile ends it. On the pretend mixer nobody is listening: it arrives at
/// once.
async fn fade(target: Target, way: &str, delta: Option<f32>) -> Result<String, String> {
    let (name, volumes): (String, Vec<(String, f32, bool)>) = match &target {
        Target::Master => {
            let (level, muted) = master_state().await?;
            ("The sound".to_string(), vec![("Master".to_string(), level, muted)])
        }
        Target::App(app) => (spoken(app), sessions_of(app).await?.into_iter().map(|s| (s.name, s.level, s.muted)).collect()),
    };
    if volumes.is_empty() {
        return Err(format!("{name} is not playing any audio right now, so it has no mixer channel to fade."));
    }
    let rising = delta.map_or(way == "in", |d| d > 0.0);
    let plan: Vec<Applied> = volumes
        .iter()
        .map(|(volume, level, muted)| {
            let to = match delta {
                Some(d) => (level + d / 100.0).clamp(0.0, 1.0),
                // Nothing to go back to: an app's own level is full by nature, the master's is not.
                None if rising => faded().iter().find(|(faded, _)| faded == volume).map_or(if volume == "Master" { 0.5 } else { 1.0 }, |(_, level)| *level),
                None => 0.0,
            };
            Applied { name: volume.clone(), before: (*level, *muted), after: (to, *muted && !rising) }
        })
        .collect();
    if plan.iter().all(|p| (p.after.0 - p.before.0).abs() < 0.005) {
        return Ok(format!("{name} is already at {}.", pct(plan[0].after.0)));
    }
    if delta.is_none() && !rising {
        let mut kept = faded();
        kept.retain(|(volume, _)| !plan.iter().any(|p| p.name == *volume));
        kept.extend(plan.iter().map(|p| (p.name.clone(), p.before.0)));
    }
    note_change(&plan);
    for p in &plan {
        remember(if p.name == "Master" { Target::Master } else { Target::App(p.name.to_lowercase()) }, p.before);
    }
    about(target);
    forget_step();
    let said = match (delta, rising) {
        (Some(_), true) => format!("Bringing {} up slowly.", lowered_name(&name)),
        (Some(_), false) => format!("Turning {} down slowly.", lowered_name(&name)),
        (None, true) => format!("Fading {} in.", lowered_name(&name)),
        (None, false) => format!("Fading {} out.", lowered_name(&name)),
    };
    let run = FADE_RUN.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    if bench().is_some() {
        for p in &plan {
            set_in_passing(&p.name, Change { level: Some(p.after.0), step: None, mute: Some(p.after.1) }).await?;
        }
        return Ok(said);
    }
    let seen = changes();
    tokio::spawn(async move {
        for along in fade_points(FADE_STEPS) {
            tokio::time::sleep(FADE_TIME / FADE_STEPS).await;
            if FADE_RUN.load(std::sync::atomic::Ordering::Acquire) != run || changes() != seen || bench().is_some() {
                return;
            }
            for p in &plan {
                let level = p.before.0 + (p.after.0 - p.before.0) * along;
                if let Err(error) = set_in_passing(&p.name, Change { level: Some(level), step: None, mute: Some(p.after.1) }).await {
                    warn!("[MIXER] A fade stopped: {error}");
                    return;
                }
            }
            if let Some(p) = plan.first() {
                let level = p.before.0 + (p.after.0 - p.before.0) * along;
                crate::dynamic_popups::volume(&display_name(&p.name), (level * 100.0).round() as u8, p.after.1);
            }
        }
    });
    Ok(said)
}

/// "The sound", "the music", "the game" inside a sentence; a program's name stays as it is.
fn lowered_name(name: &str) -> String {
    match name {
        "The sound" => "the sound".to_string(),
        "Music" | "Musik" => "the music".to_string(),
        "Game" => "the game".to_string(),
        name => name.to_string(),
    }
}

pub async fn set_master_mute(muted: bool) -> Result<String, String> {
    timed_mutes().retain(|(key, _)| key != "Master");
    let out = master_change(None, Some(muted), None, |_, m| if m { "Master output muted.".into() } else { "Master output unmuted.".into() }).await?;
    if !muted {
        // "Unmute" also ends "mute everything except …": the apps it silenced play again.
        unsilence().await;
    }
    Ok(out)
}

/// The master volume (0.0-1.0) and mute state, through Core Audio or the PowerShell endpoint.
pub async fn master_state() -> Result<(f32, bool), String> {
    if let Some(bench) = bench() {
        return Ok(bench.master);
    }
    let error = match com(|| retrying(imp::master_state)).await {
        Ok(state) => return Ok(state),
        Err(e) => e,
    };
    #[cfg(windows)]
    let state = {
        warn!("[MIXER] Core Audio failed ({error}); reading the master volume through PowerShell");
        fallback::set(None, None, None).await
    };
    #[cfg(not(windows))]
    let state = Err(error);
    state
}

/// A change to an app's own volume (every session of it), remembered for the undo and "it".
/// None of its sessions: the app plays nothing, which is said as it is.
async fn app_change(app: &str, change: Change) -> Result<Vec<Applied>, String> {
    let hit = apps_apply(targets(app), false, change).await?;
    if hit.is_empty() {
        return Err(format!("{} is not playing any audio right now, so it has no mixer channel to set.", spoken(app)));
    }
    for h in &hit {
        remember(Target::App(h.name.to_lowercase()), h.before);
    }
    about(Target::App(clean(app)));
    note_change(&hit);
    note_step(true, change);
    let (level, muted) = hit[0].after;
    crate::dynamic_popups::volume(&spoken(app), (level * 100.0).round() as u8, muted);
    Ok(hit)
}

/// "Discord, Firefox": the sessions a change reached, as the mixer names them.
fn names(hit: &[Applied]) -> String {
    hit.iter().map(|h| display_name(&h.name)).collect::<Vec<_>>().join(", ")
}

pub async fn set_app_volume(app: &str, level: f32) -> Result<String, String> {
    let level = level.clamp(0.0, 1.0);
    let hit = app_change(app, Change { level: Some(level), ..Change::default() }).await?;
    Ok(format!("{} set to {}.", names(&hit), pct(level)))
}

/// `delta` in volume points (-100..100), from where the app's volume is.
pub async fn change_app_volume(app: &str, delta: f32) -> Result<String, String> {
    let hit = app_change(app, Change { step: Some(delta / 100.0), ..Change::default() }).await?;
    let (before, after) = (hit[0].before.0, hit[0].after.0);
    Ok(if (after - before).abs() < 0.005 {
        format!("{} is already at {}.", names(&hit), pct(after))
    } else {
        format!("{} {} to {}.", names(&hit), if after > before { "raised" } else { "lowered" }, pct(after))
    })
}

pub async fn set_app_mute(app: &str, muted: bool) -> Result<String, String> {
    let key = clean(app);
    timed_mutes().retain(|(timed, _)| *timed != key);
    // ("The game" is an alias here, so it is known by the error, not by the name.)
    let hit = app_change(app, Change { mute: Some(muted), ..Change::default() }).await.map_err(|e| if e.starts_with("No game") { e } else { format!("{} has no audio session right now.", spoken(app)) })?;
    Ok(format!("{} {}.", names(&hit), if muted { "muted" } else { "unmuted" }))
}

/// "Mute everything except Discord": every other app's own volume is muted. The master
/// volume, Jarvis' voice and an audio router's session stay as they are.
pub async fn mute_others(keep: &str) -> Result<String, String> {
    let hit = apps_apply(targets(keep), true, Change { mute: Some(true), ..Change::default() }).await?;
    let newly: Vec<&Applied> = hit.iter().filter(|h| !h.before.1).collect();
    note_change(&hit);
    {
        let mut m = memory();
        for h in &newly {
            if !m.silenced.iter().any(|s| s.eq_ignore_ascii_case(&h.name)) {
                m.silenced.push(h.name.clone());
            }
        }
    }
    Ok(if newly.is_empty() {
        format!("Nothing but {} is playing.", spoken(keep))
    } else {
        format!("Everything but {} is muted: {}.", spoken(keep), newly.iter().map(|h| display_name(&h.name)).collect::<Vec<_>>().join(", "))
    })
}

/// "Make everything loud again": every level the mixer changed goes back to what it was
/// before its first change. With nothing to put back, at least nothing stays muted.
pub async fn restore() -> Result<String, String> {
    // Everything goes back: there is no single step left to undo after it.
    last_change().clear();
    FADE_RUN.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    let silenced = !memory().silenced.is_empty();
    unsilence().await;
    let before = std::mem::take(&mut memory().before);
    if before.is_empty() && silenced {
        return Ok("Everything plays again.".to_string());
    }
    if before.is_empty() {
        let master = master_apply(Change { mute: Some(false), ..Change::default() }).await?;
        return Ok(format!("Nothing had been turned down by me. The master volume is at {}.", pct(master.after.0)));
    }
    let mut back: Vec<String> = Vec::new();
    for (target, level, muted) in before {
        let change = Change { level: Some(level), step: None, mute: Some(muted) };
        match target {
            Target::Master => {
                master_apply(change).await?;
                back.push(format!("master volume {}", pct(level)));
            }
            Target::App(session) => {
                // An app that has stopped playing since has no level left to put back.
                if apps_apply(vec![session.clone()], false, change).await.is_ok_and(|hit| !hit.is_empty()) {
                    back.push(format!("{} {}", display_name(&session), pct(level)));
                }
            }
        }
    }
    memory().last = None;
    Ok(if back.is_empty() { "Nothing of it is playing any more.".to_string() } else { format!("Back as it was: {}.", back.join(", ")) })
}

/// "Make Discord loud again": that app's sessions go back to what they were before his first
/// change to them, and the rest stays as it stands. With nothing of his to put back, at least
/// it is not muted.
async fn restore_app(app: &str) -> Result<String, String> {
    let wanted = targets(app);
    let taken: Vec<(String, f32, bool)> = {
        let mut m = memory();
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut m.before).into_iter().partition(|(target, _, _)| matches!(target, Target::App(session) if matches_target(session, &wanted)));
        m.before = rest;
        m.silenced.retain(|session| !matches_target(session, &wanted));
        mine.into_iter().filter_map(|(target, level, muted)| if let Target::App(session) = target { Some((session, level, muted)) } else { None }).collect()
    };
    if taken.is_empty() {
        let hit = app_change(app, Change { mute: Some(false), ..Change::default() }).await?;
        return Ok(format!("{} is at {}. I had not turned it down.", names(&hit), pct(hit[0].after.0)));
    }
    let mut moved: Vec<Applied> = Vec::new();
    let mut back: Vec<String> = Vec::new();
    for (session, level, muted) in taken {
        let change = Change { level: Some(level), step: None, mute: Some(muted) };
        if let Ok(hit) = apps_apply(vec![session.clone()], false, change).await {
            if !hit.is_empty() {
                back.push(format!("{} {}", display_name(&session), pct(level)));
                moved.extend(hit);
            }
        }
    }
    note_change(&moved);
    Ok(if back.is_empty() { format!("{} is not playing any more.", spoken(app)) } else { format!("Back as it was: {}.", back.join(", ")) })
}

/// "Undo that" after a volume request: every volume the last change moved goes back to what
/// it was just before it, mute state included. One step: what was changed before that stays
/// changed ("make everything loud again" takes all of it back).
pub async fn undo_last() -> Result<String, String> {
    let last = std::mem::take(&mut *last_change());
    if last.is_empty() {
        return Err("There is no volume change of mine left to take back.".to_string());
    }
    FADE_RUN.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    let mut back: Vec<String> = Vec::new();
    for moved in last {
        let (level, muted) = moved.before;
        let put = Change { level: Some(level), step: None, mute: Some(muted) };
        let said = |name: &str| format!("{name} {}{}", pct(level), if muted { " (muted)" } else { "" });
        if moved.name == "Master" {
            master_apply(put).await?;
            back.push(said("master volume"));
        } else if apps_apply(vec![moved.name.to_lowercase()], false, put).await.is_ok_and(|hit| !hit.is_empty()) {
            // (An app that has stopped playing since has no level left to put back.)
            back.push(said(&display_name(&moved.name)));
            // A session "mute everything except …" had silenced plays again: it is no longer
            // one the next unmute has to look after.
            if !muted {
                memory().silenced.retain(|s| !s.eq_ignore_ascii_case(&moved.name));
            }
        }
    }
    Ok(if back.is_empty() { "Nothing of it is playing any more.".to_string() } else { format!("Back as it was: {}.", back.join(", ")) })
}

/// The mixer's sessions of an app Sir named.
async fn sessions_of(app: &str) -> Result<Vec<AppSession>, String> {
    let sessions = list_audio_apps().await?;
    let mut wanted = targets(app);
    if wanted.iter().any(|w| w == GAME) {
        wanted = game_sessions(&sessions);
    }
    Ok(sessions.into_iter().filter(|s| session_matches(s, &wanted)).collect())
}

/// "Make Firefox as loud as Discord", "make it quieter than Discord": `app` goes to the level the
/// other app has (`offset`: volume points from it).
async fn match_app(app: &str, other: &str, offset: f32) -> Result<String, String> {
    let level = sessions_of(other).await?.first().map(|s| s.level).ok_or_else(|| format!("{} is not playing any audio right now, so it has no mixer channel to go by.", spoken(other)))?;
    set_app_volume(app, level + offset / 100.0).await
}

/// "Set all apps to 100 %": every app's own level, the master volume left where it is. Jarvis'
/// voice and an audio router's session stay as they are.
async fn set_all_apps(level: f32) -> Result<String, String> {
    let level = level.clamp(0.0, 1.0);
    let hit = apps_apply(Vec::new(), true, Change { level: Some(level), ..Change::default() }).await?;
    if hit.is_empty() {
        return Err("No app has a mixer channel right now.".to_string());
    }
    for h in &hit {
        remember(Target::App(h.name.to_lowercase()), h.before);
    }
    note_change(&hit);
    forget_step();
    Ok(format!("Every app set to {}.", pct(level)))
}

/// "How loud is Discord?", "is my browser muted?": one row per app, read off the mixer.
async fn read_app(app: &str) -> Result<String, String> {
    let mut rows: Vec<String> = Vec::new();
    for session in &sessions_of(app).await? {
        let row = format!("{} is at {}{}", display_name(&session.name), pct(session.level), if session.muted { ", muted" } else { "" });
        if !rows.contains(&row) {
            rows.push(row);
        }
    }
    if rows.is_empty() {
        // ("Is a game running?": none to be found is an answer, not a failure.)
        if targets(app).iter().any(|wanted| wanted == GAME) {
            return Ok("I can't see a game on the mixer, Sir.".to_string());
        }
        return Err(format!("{} is not playing any audio right now, so it has no mixer channel to read.", spoken(app)));
    }
    Ok(format!("{}.", rows.join("; ")))
}

/// Below this a volume counts as off when Sir says he hears nothing.
const NEARLY_OFF: f32 = 0.05;

/// "I can't hear anything", "there's no sound", "I can't hear Discord": whatever stands in the
/// way on the mixer is put right, as one change ("undo that" takes it back), and said. With
/// nothing in the way, what the devices show is said as it is (no app making a sound, sound that
/// never reaches the default output, sound that leaves it), never a guess.
async fn find_the_sound(app: Option<&str>) -> Result<String, String> {
    let (level, muted) = master_state().await?;
    let sessions = list_audio_apps().await?;
    let named: Option<Vec<String>> = app.map(|app| {
        let wanted = targets(app);
        if wanted.iter().any(|w| w == GAME) {
            game_sessions(&sessions)
        } else {
            wanted
        }
    });
    let mut moved: Vec<Applied> = Vec::new();
    let mut found: Vec<String> = Vec::new();
    if muted || level < NEARLY_OFF {
        let done = master_apply(Change { level: (level < NEARLY_OFF).then_some(0.3), step: None, mute: Some(false) }).await?;
        remember(Target::Master, done.before);
        about(Target::Master);
        found.push(format!("The master volume was {}; it is at {} now.", if muted { "muted" } else { "nearly off" }, pct(done.after.0)));
        crate::dynamic_popups::volume("Master", (done.after.0 * 100.0).round() as u8, false);
        moved.push(done);
    }
    let mine: Vec<&AppSession> = sessions.iter().filter(|s| named.as_ref().is_none_or(|wanted| session_matches(s, wanted))).collect();
    let mut seen: Vec<String> = Vec::new();
    for session in &mine {
        // Nearly off counts for the app he named; a quiet app among many may be meant to be.
        let quiet = named.is_some() && session.level < NEARLY_OFF;
        let name = session.name.to_lowercase();
        if !(session.muted || quiet) || seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        let hit = apps_apply(vec![name.clone()], false, Change { level: quiet.then_some(0.5), step: None, mute: Some(false) }).await?;
        for h in &hit {
            remember(Target::App(h.name.to_lowercase()), h.before);
        }
        memory().silenced.retain(|s| !s.eq_ignore_ascii_case(&name));
        if let Some(h) = hit.first() {
            found.push(format!("{} was {}; it plays at {} now.", display_name(&h.name), if session.muted { "muted" } else { "nearly off" }, pct(h.after.0)));
        }
        moved.extend(hit);
    }
    note_change(&moved);
    if !found.is_empty() {
        return Ok(format!("Found it, Sir. {}", found.join(" ")));
    }
    if let (Some(app), true) = (app, mine.is_empty()) {
        return Ok(format!("{} is not playing any audio right now, Sir, so the mixer has nothing to turn up.", spoken(app)));
    }
    // Nothing of the mixer's. What the devices show, as it is (read-only).
    let outputs = if cfg!(test) { Vec::new() } else { output_devices().await.unwrap_or_default() };
    let mut playing: Vec<String> = if cfg!(test) { Vec::new() } else { audible_apps().await.into_iter().map(|(name, _)| name).filter(|name| !keeps_playing(name)).map(|name| display_name(&name)).collect() };
    playing.sort();
    playing.dedup();
    let levels = match mine.first().filter(|_| app.is_some()) {
        Some(session) => format!("{} is at {} and the master volume at {}", display_name(&session.name), pct(session.level), pct(level)),
        None => format!("the master volume is at {}", pct(level)),
    };
    let default = outputs.iter().find(|d| d.default);
    let who = names_are(&playing);
    // The app he named has a channel, all open, and sends nothing through it.
    let idle = mine.first().filter(|_| app.is_some()).map(|session| display_name(&session.name)).filter(|name| !playing.contains(name));
    Ok(match default {
        _ if idle.is_some() => format!("Nothing is muted, Sir, and {levels}. {} is not sending any sound right now.", idle.unwrap_or_default()),
        _ if playing.is_empty() => format!("Nothing is muted, Sir, and {levels}. No app is making a sound right now."),
        Some(device) if device.peak > 0.001 => format!("Nothing is muted, Sir, and {levels}. {who} playing, and the sound is leaving through {}. That leaves the device itself: its own volume, its power, its connection.", device.name),
        Some(device) => format!("Nothing is muted, Sir, and {levels}. {who} playing, but nothing is reaching {}. That points at the routing: Wave Link's mix or the app's output device.", device.name),
        None => format!("Nothing is muted, Sir, and {levels}, but Windows has no default output device right now."),
    })
}

/// "Discord is", "Discord and Sonora are", "Discord, Sonora and Firefox are".
fn names_are(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("{one} is"),
        [first @ .., last] => format!("{} and {last} are", first.join(", ")),
    }
}

/// "Is anything muted?": what is, or that nothing is. (An audio router and his own voice are no
/// apps of Sir's.)
async fn read_muted() -> Result<String, String> {
    let (_, master) = master_state().await?;
    let mut apps: Vec<String> = list_audio_apps().await?.iter().filter(|a| a.muted && !keeps_playing(&a.name)).map(|a| display_name(&a.name)).collect();
    apps.sort();
    apps.dedup();
    Ok(match (master, apps.is_empty()) {
        (false, true) => "Nothing is muted, Sir.".to_string(),
        (true, true) => "The master volume is muted, Sir.".to_string(),
        (false, false) => format!("{} muted, Sir.", names_are(&apps)),
        (true, false) => format!("The master volume is muted, Sir, and {} muted too.", names_are(&apps)),
    })
}

/// "Save this mix as gaming": the master volume and every app of Sir's as they stand now.
/// (An audio router and Jarvis' own voice are no part of a mix.)
async fn save_mix(name: &str) -> Result<String, String> {
    let name = crate::audio_mixes::clean_name(name).ok_or("A mix needs a short name.")?;
    let master = master_state().await?;
    let mut apps: Vec<crate::audio_mixes::Channel> = Vec::new();
    for session in list_audio_apps().await?.into_iter().filter(|s| !keeps_playing(&s.name)) {
        // (Discord and browsers run several sessions: one channel each.)
        if !apps.iter().any(|c| c.app.eq_ignore_ascii_case(&session.name)) {
            apps.push(crate::audio_mixes::Channel { app: session.name, level: session.level, muted: session.muted });
        }
    }
    let rows: Vec<String> = apps.iter().map(|c| format!("{} {}{}", display_name(&c.app), pct(c.level), if c.muted { " (muted)" } else { "" })).collect();
    let said = format!("The {name} mix is saved: master {}{}{}.", pct(master.0), if master.1 { " (muted)" } else { "" }, if rows.is_empty() { String::new() } else { format!(", {}", rows.join(", ")) });
    crate::audio_mixes::store(crate::audio_mixes::Mix { name, master, apps });
    Ok(said)
}

/// "Load my gaming mix": every level of it in place, as one change ("undo that" takes all of
/// it back). An app of the mix that has no session now is said, its level waits in the mix.
async fn load_mix(name: &str) -> Result<String, String> {
    let Some(mix) = crate::audio_mixes::find(name) else {
        let known = crate::audio_mixes::names();
        let wanted = crate::audio_mixes::clean_name(name).unwrap_or_else(|| name.to_string());
        return Err(if known.is_empty() { format!("There is no mix called {wanted}: none is saved yet.") } else { format!("There is no mix called {wanted}. Saved: {}.", known.join(", ")) });
    };
    let mut moved = vec![master_apply(Change { level: Some(mix.master.0), step: None, mute: Some(mix.master.1) }).await?];
    remember(Target::Master, moved[0].before);
    let mut waiting: Vec<String> = Vec::new();
    for channel in &mix.apps {
        let put = Change { level: Some(channel.level), step: None, mute: Some(channel.muted) };
        match apps_apply(vec![channel.app.to_lowercase()], false, put).await {
            Ok(hit) if !hit.is_empty() => {
                for h in &hit {
                    remember(Target::App(h.name.to_lowercase()), h.before);
                }
                moved.extend(hit);
            }
            _ => waiting.push(display_name(&channel.app)),
        }
    }
    note_change(&moved);
    crate::dynamic_popups::volume("Master", (mix.master.0 * 100.0).round() as u8, mix.master.1);
    Ok(if waiting.is_empty() { format!("The {} mix is on.", mix.name) } else { format!("The {} mix is on. Not playing right now, so left out: {}.", mix.name, waiting.join(", ")) })
}

/// "Which mixes do I have?"
fn list_mixes() -> String {
    let known = crate::audio_mixes::names();
    if known.is_empty() {
        "No mix is saved yet. \"Save this mix as gaming\" keeps the volumes as they stand.".to_string()
    } else {
        format!("Saved mixes: {}.", known.join(", "))
    }
}

/// A level stated with a sign ("+30", "-50", -50) is a step from where the volume is, in
/// volume points, never a level: "-50" was once read as the level 0 and silenced the PC.
pub fn signed_step(v: &Value) -> Option<f32> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| *x < 0.0).map(|x| x as f32),
        Value::String(s) => {
            let t = s.trim().trim_end_matches('%').trim().trim_end_matches("percent").trim();
            (t.starts_with('+') || t.starts_with('-')).then(|| t.parse::<f32>().ok()).flatten()
        }
        _ => None,
    }
}

/// The music player's own volume, as `control_music` states it: a level, or a signed step.
/// The player's mixer channel when it has one, else the master volume.
pub async fn player_volume(level: &Value) -> Result<String, String> {
    let playing = list_audio_apps().await.unwrap_or_default().iter().any(|a| session_matches(a, &targets("music")));
    match (signed_step(level), level_of(level), playing) {
        (Some(step), _, true) => change_app_volume("music", step).await,
        (Some(step), _, false) => change_master_volume(step).await,
        (None, Some(l), true) => set_app_volume("music", l).await,
        (None, Some(l), false) => set_master_volume(l).await,
        (None, None, _) => Err("set_volume needs 'level': 0-100, or a step such as \"+10\" or \"-10\".".into()),
    }
}

/// A slider's changes, the latest only: a drag makes a hundred a second, and each is a call to
/// the device's driver. One call runs at a time; what arrives meanwhile replaces what waits.
pub async fn slide(target: &str, percent: u8) {
    static WAITING: std::sync::Mutex<Vec<(String, u8)>> = std::sync::Mutex::new(Vec::new());
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    {
        let mut waiting = WAITING.lock().unwrap_or_else(|e| e.into_inner());
        waiting.retain(|(t, _)| t != target);
        waiting.push((target.to_string(), percent));
    }
    if RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    loop {
        loop {
            let next = WAITING.lock().unwrap_or_else(|e| e.into_inner()).pop();
            let Some((target, percent)) = next else { break };
            let level = percent as f32 / 100.0;
            let done = match target_of(Some(&target)) {
                Target::Master => set_master_volume(level).await,
                Target::App(app) => set_app_volume(&app, level).await,
            };
            if let Err(e) = done {
                warn!("[MIXER] The slider could not set {target}: {e}");
            }
        }
        RUNNING.store(false, std::sync::atomic::Ordering::Release);
        // A change that arrived just as the last one finished is not left waiting.
        if WAITING.lock().unwrap_or_else(|e| e.into_inner()).is_empty() || RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
            break;
        }
    }
}

pub async fn list_audio_apps() -> Result<Vec<AppSession>, String> {
    if let Some(bench) = bench() {
        return Ok(bench.apps);
    }
    com(imp::list).await
}

/// The Mixer block's faders: the master first, then one per app in the order of their names (an
/// app with several sessions is one fader, at its loudest, muted when all of them are). The
/// audio routers and Jarvis' own session are not there to be pulled down by accident.
pub async fn channels() -> Result<Vec<Channel>, String> {
    let percent = |level: f32| (level.clamp(0.0, 1.0) * 100.0).round() as u8;
    let (level, muted) = master_state().await?;
    let mut apps: Vec<Channel> = Vec::new();
    for session in list_audio_apps().await?.into_iter().filter(|s| !keeps_playing(&s.name)) {
        let target = session.name.to_lowercase();
        match apps.iter_mut().find(|c| c.target == target) {
            Some(channel) => {
                channel.percent = channel.percent.max(percent(session.level));
                channel.muted &= session.muted;
            }
            None => apps.push(Channel { label: crate::apps::display_name(&session.name), target, percent: percent(session.level), muted: session.muted }),
        }
    }
    apps.sort_by_key(|c| c.label.to_lowercase());
    apps.insert(0, Channel { target: MASTER.to_string(), label: "Master".to_string(), percent: percent(level), muted });
    Ok(apps)
}

/// Every output device with what plays on it (read-only).
pub async fn output_devices() -> Result<Vec<OutputDevice>, String> {
    com(imp::outputs).await
}

/// The output device an app plays to ("Discord" → "Voice chat (Elgato Virtual Audio)"), if it
/// has an audio session.
pub async fn app_output_device(app: &str) -> Option<String> {
    let app = app.to_string();
    com(move || imp::output_device_of(&app)).await.ok().flatten()
}

/// The microphone an app records from ("Discord" → "Chat Mix (Elgato Virtual Audio)") while it
/// records.
pub async fn app_input_device(app: &str) -> Option<String> {
    let app = app.to_string();
    com(move || imp::input_device_of(&app)).await.ok().flatten()
}

/// (process name, pid) of every app making sound right now (empty when unknown).
pub async fn audible_apps() -> Vec<(String, u32)> {
    com(imp::audible).await.unwrap_or_default()
}

/// `control_audio_mixer`.
pub async fn tool(args: &Value) -> Result<String, String> {
    let action = str_arg(args, &["action", "command"]).unwrap_or("list").to_lowercase().replace([' ', '-'], "_");
    let app = str_arg(args, &["app", "app_name", "application", "target"]);
    let raw_level = args.get("level").or_else(|| args.get("volume")).or_else(|| args.get("value"));
    let level = raw_level.and_then(level_of);
    // A step, in volume points: `delta`, or a level stated with a sign ("-50" is never a level).
    let step = args
        .get("delta")
        .or_else(|| args.get("step"))
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.trim().trim_end_matches('%').trim().parse().ok()))
        .map(|d| d as f32)
        .or_else(|| raw_level.and_then(signed_step));
    // Whom it is about: an app Sir named, "it" (what the last request was about), the master.
    let about = target_of(app);
    match action.as_str() {
        "set_master" | "set_volume" | "master" | "set_app" | "set_app_volume" | "app_volume" | "set" => match (&about, raw_level.and_then(signed_step)) {
            (Target::Master, Some(step)) => change_master_volume(step).await,
            (Target::App(app), Some(step)) => change_app_volume(app, step).await,
            (Target::Master, None) if action.contains("app") && app.is_none() => Err("set_app needs app.".into()),
            (Target::Master, None) => set_master_volume(level.ok_or("set_master needs level (0-100).")?).await,
            (Target::App(app), None) => set_app_volume(app, level.ok_or("set_app needs level (0-100).")?).await,
        },
        "change_master" | "volume_up" | "volume_down" | "change" | "change_app" | "change_app_volume" | "app_volume_up" | "app_volume_down" => {
            let delta = step.unwrap_or(10.0);
            let delta = if action.ends_with("down") { -delta.abs() } else { delta };
            match &about {
                Target::Master => change_master_volume(delta).await,
                Target::App(app) => change_app_volume(app, delta).await,
            }
        }
        "mute_master" | "mute" | "mute_app" | "unmute_master" | "unmute" | "unmute_app" => {
            let muted = !action.starts_with("unmute");
            let said = match &about {
                Target::Master if action.ends_with("app") && app.is_none() => Err(format!("{action} needs app.")),
                Target::Master => set_master_mute(muted).await,
                Target::App(app) => set_app_mute(app, muted).await,
            }?;
            // "Mute Discord for 10 minutes": it ends by itself.
            Ok(match args.get("seconds").and_then(Value::as_u64).filter(|seconds| muted && *seconds > 0) {
                Some(seconds) => {
                    unmute_after(about.clone(), seconds);
                    format!("{} for {}.", said.trim_end_matches('.'), span(seconds))
                }
                None => said,
            })
        }
        // "Fade the music out", "turn it down slowly".
        "fade" => fade(about, str_arg(args, &["way", "direction"]).unwrap_or("out"), step).await,
        // "Make Firefox as loud as Discord".
        "match_app" => {
            let other = str_arg(args, &["to", "like", "as"]).ok_or("match_app needs to (the app to go by).")?;
            match_app(app.ok_or("match_app needs app.")?, other, args.get("offset").and_then(Value::as_f64).unwrap_or(0.0) as f32).await
        }
        "set_all_apps" => set_all_apps(level.ok_or("set_all_apps needs level (0-100).")?).await,
        // "Which audio device am I using?": the one, not the list.
        "device" | "output_device" => {
            let now = output_devices().await?.into_iter().find(|d| d.default).map(|d| d.name);
            Ok(now.map_or("No output device is active, Sir.".to_string(), |device| format!("The sound is leaving through {device}, Sir.")))
        }
        // Which device plays is Windows' and the audio router's to say, not his.
        "switch_device" => {
            let now = output_devices().await.ok().and_then(|devices| devices.into_iter().find(|d| d.default)).map(|d| d.name);
            Ok(match now {
                Some(device) => format!("I can't switch the output device myself, Sir. The sound is leaving through {device}."),
                None => "I can't switch the output device myself, Sir.".to_string(),
            })
        }
        "mute_others" | "mute_all_except" | "solo" => mute_others(app.ok_or("mute_others needs app (the one that keeps playing).")?).await,
        "restore" | "undo" | "reset" => match &about {
            Target::App(app) => restore_app(app).await,
            Target::Master => restore().await,
        },
        // One step back ("undo that" right after a volume request).
        "undo_last" | "undo_last_change" | "put_back" => undo_last().await,
        // What a volume is at ("how loud is Discord?").
        "get_master" | "master_level" => {
            let (level, muted) = master_state().await?;
            Ok(format!("The master volume is at {}{}.", pct(level), if muted { ", muted" } else { "" }))
        }
        "get_app" | "app_level" => read_app(app.ok_or("get_app needs app.")?).await,
        // "Is anything muted?": that alone, not the whole mixer.
        "muted" | "list_muted" => read_muted().await,
        // "Make it quiet": down to that level, never up to it; "make it loud" the other way round.
        "quiet" | "loud" => {
            let up = action == "loud";
            let wanted = level.unwrap_or(if up { 0.8 } else { 0.2 });
            let (now, _) = master_state().await?;
            let said = if (up && now >= wanted) || (!up && now <= wanted) { Ok(format!("The master volume is already at {}.", pct(now))) } else { set_master_volume(wanted).await };
            // These have a way, unlike a level: "more" after "make it quiet" is quieter still.
            if said.is_ok() {
                note_step(false, Change { step: Some(if up { 0.1 } else { -0.1 }), ..Change::default() });
            }
            said
        }
        // "I can't hear anything": found and put right where it is the mixer's to.
        "no_sound" | "cant_hear" | "find_sound" => find_the_sound(app).await,
        // Named mixes ("save this mix as gaming").
        "save_mix" => save_mix(str_arg(args, &["name", "mix"]).ok_or("save_mix needs name.")?).await,
        "load_mix" | "apply_mix" => load_mix(str_arg(args, &["name", "mix"]).ok_or("load_mix needs name.")?).await,
        "delete_mix" | "remove_mix" => {
            let name = str_arg(args, &["name", "mix"]).ok_or("delete_mix needs name.")?;
            if crate::audio_mixes::remove(name) {
                Ok(format!("The {} mix is gone.", crate::audio_mixes::clean_name(name).unwrap_or_default()))
            } else {
                Err(format!("There is no mix called {}.", crate::audio_mixes::clean_name(name).unwrap_or_else(|| name.to_string())))
            }
        }
        "list_mixes" | "mixes" => Ok(list_mixes()),
        "set_apps" => {
            let items = args.get("apps").and_then(Value::as_array).ok_or("set_apps needs apps: [{app, level}].")?;
            let mut done = Vec::new();
            let mut failed = Vec::new();
            for item in items {
                let (Some(name), Some(l)) = (str_arg(item, &["app", "name"]), item.get("level").and_then(level_of)) else { continue };
                match set_app_volume(name, l).await {
                    Ok(out) => done.push(out),
                    Err(e) => failed.push(e),
                }
            }
            if done.is_empty() {
                return Err(if failed.is_empty() { "No app volumes given.".into() } else { failed.join(" ") });
            }
            Ok([done.join(" "), failed.join(" ")].join(" ").trim().to_string())
        }
        "devices" | "outputs" | "output_devices" => {
            let rows: Vec<String> = output_devices()
                .await?
                .iter()
                .map(|d| format!("{}{} {}{}{}", d.name, if d.default { " (the default output)" } else { "" }, pct(d.level), if d.muted { " muted" } else { "" }, if d.apps.is_empty() { String::new() } else { format!(": {}", d.apps.join(", ")) }))
                .collect();
            Ok(if rows.is_empty() { "No output device is active.".to_string() } else { format!("Output devices: {}.", rows.join("; ")) })
        }
        "list" | "list_apps" | "status" => {
            let (master, muted) = master_state().await?;
            let apps = list_audio_apps().await?;
            // What is making sound this moment, and what merely has a channel in the mixer (a
            // paused player, Steam). Jarvis' own voice and an audio router's mix are no apps
            // of Sir's.
            // (Asked of the real mixer even while the levels are a copy: it only reads.)
            let audible: Vec<String> = if cfg!(test) { Vec::new() } else { audible_apps().await.into_iter().map(|(name, _)| name.to_lowercase()).collect() };
            let (playing, quiet): (Vec<&AppSession>, Vec<&AppSession>) = apps.iter().filter(|a| !keeps_playing(&a.name)).partition(|a| audible.contains(&a.name.to_lowercase()));
            let rows = |list: &[&AppSession]| list.iter().map(|a| format!("{} {}{}", display_name(&a.name), pct(a.level), if a.muted { " (muted)" } else { "" })).collect::<Vec<_>>().join(", ");
            let head = format!("Master {}{}.", pct(master), if muted { " (muted)" } else { "" });
            Ok(match (playing.is_empty(), quiet.is_empty()) {
                (true, true) => format!("{head} No app has a mixer channel right now."),
                (true, false) => format!("{head} Nothing is playing audio right now. In the mixer: {}.", rows(&quiet)),
                (false, true) => format!("{head} Apps playing audio: {}.", rows(&playing)),
                (false, false) => format!("{head} Apps playing audio: {}. Silent, but in the mixer: {}.", rows(&playing), rows(&quiet)),
            })
        }
        other => Err(format!("Unknown mixer action '{other}' (set_master, change_master, mute_master, unmute_master, set_app, set_apps, change_app, mute_app, unmute_app, mute_others, restore, undo_last, get_master, get_app, muted, quiet, loud, fade, match_app, set_all_apps, no_sound, save_mix, load_mix, delete_mix, list_mixes, list, devices, switch_device).")),
    }
}

/// A silent scripted session's mixer: a copy of the real one as it stands now. Every volume
/// request of the session is carried out on the copy; the real levels are read once and never
/// written (Sir may be listening to something while a script asks for "volume 10").
pub async fn work_on_a_copy() {
    let master = com(|| retrying(imp::master_state)).await.unwrap_or((0.8, false));
    let apps = com(imp::list).await.unwrap_or_default();
    set_bench(Some(Bench { master, apps }));
    forget();
}

/// The pretend mixer the volume scenarios start from: Sir's desk on an ordinary evening (the
/// browser, Discord and the music play; Wave Link's session carries their mix to the headset).
pub fn scenario_bench() -> Bench {
    let app = |name: &str, level: f32| AppSession { name: name.to_string(), pid: 1, level, muted: false, labels: Vec::new() };
    Bench { master: (0.8, false), apps: vec![app("firefox", 1.0), app("Discord", 1.0), app("sonora", 0.6), app("steam", 1.0), app("System sounds", 1.0), app("Elgato.WaveLink", 1.0), app("jarvis", 1.0)] }
}

/// One volume request carried out as Jarvis carries it out: the sentence through the fast
/// path's parser, its tool call through the mixer (or the media tool's volume half).
async fn say(sentence: &str) -> Result<String, String> {
    // (In the order he asks: the quick commands, "more" among them, come first.)
    if let Some(action) = crate::intents::silent_quick_command(sentence).filter(|a| a.tool == "control_audio_mixer") {
        return tool(&action.args).await;
    }
    match crate::intents::parse(sentence) {
        Some(crate::intents::Intent::Tool { name: "control_audio_mixer", args }) => tool(&args).await,
        Some(crate::intents::Intent::Tool { name: "media_and_volume_control", args }) => {
            let action = str_arg(&args, &["action"]).unwrap_or("");
            let level = args.get("level").and_then(Value::as_i64);
            match crate::media::MediaAction::parse(action, level) {
                Some(a) => crate::media::execute(a).await,
                None => Err(format!("the media tool does not know '{action}'")),
            }
        }
        other => Err(format!("not routed to the mixer: {other:?}")),
    }
}

/// The volume scenarios, on the pretend mixer (no real level is touched, nothing is heard):
/// each request as Sir says it, and what the mixer must look like afterwards. One entry per
/// scenario: its name and what went wrong, if anything.
pub async fn scenarios() -> Vec<(String, Result<(), String>)> {
    let earlier = set_bench(Some(scenario_bench()));
    forget();
    let mut out: Vec<(String, Result<(), String>)> = Vec::new();
    let level = |name: &str| bench().and_then(|b| b.apps.iter().find(|a| a.name.eq_ignore_ascii_case(name)).map(|a| ((a.level * 100.0).round() as i32, a.muted)));
    let master = || bench().map(|b| ((b.master.0 * 100.0).round() as i32, b.master.1));
    // (sentence, the app or "master" to look at, the level and mute state it must have, words
    // the answer must contain; an expected refusal starts with '!'.)
    let script: [(&str, &str, (i32, bool), &str); 30] = [
        ("Lower my browser's volume by 50%", "firefox", (50, false), "lowered to 50 %"),
        ("No, don't, don't mute it, make it 50% louder.", "firefox", (100, false), "raised to 100 %"),
        ("And you make discords, volume 50%.", "Discord", (50, false), "set to 50 %"),
        ("Make it 20 percent louder", "Discord", (70, false), "raised to 70 %"),
        ("Mute it", "Discord", (70, true), "muted"),
        ("Unmute it", "Discord", (70, false), "unmuted"),
        ("Makes system volume one hundred percent.", "master", (100, false), "100 %"),
        ("Volume 40", "master", (40, false), "40"),
        ("Louder", "master", (50, false), "50"),
        ("Mute", "master", (50, true), "muted"),
        ("a little quieter", "master", (45, true), "45 %"),
        ("Unmute", "master", (45, false), "unmuted"),
        ("Turn down the music a bit", "sonora", (55, false), "lowered to 55 %"),
        ("browser volume thirty percent", "firefox", (30, false), "set to 30 %"),
        ("Set Discord and Spotify to 40 percent", "Discord", (40, false), "set to 40 %"),
        ("Raise Discord by 90", "Discord", (100, false), "raised to 100 %"),
        ("Raise Discord by 10", "Discord", (100, false), "already at 100 %"),
        ("Mute my browser", "firefox", (30, true), "muted"),
        ("Unmute my browser", "firefox", (30, false), "unmuted"),
        ("Mute everything except Discord", "sonora", (55, true), "Everything but Discord is muted"),
        ("Mute everything except Discord", "Discord", (100, false), "Nothing but Discord"),
        ("Mute everything except Discord", "Elgato.WaveLink", (100, false), ""),
        ("Mute everything except Discord", "jarvis", (100, false), ""),
        ("Unmute", "sonora", (55, false), "unmuted"),
        ("Mute Spotify", "master", (45, false), "!no audio session"),
        ("Raise Chrome by 20", "firefox", (30, false), "!not playing any audio"),
        ("Lower the volume by 5%", "master", (40, false), "40 %"),
        ("Make everything loud again", "firefox", (100, false), "Back as it was"),
        ("Make everything loud again", "master", (80, false), "Nothing had been turned down"),
        ("Full volume", "master", (100, false), "100"),
    ];
    for (sentence, look_at, wanted, words) in script {
        let answer = say(sentence).await;
        let now = if look_at == "master" { master() } else { level(look_at) };
        let result = match (&answer, words.strip_prefix('!')) {
            (Err(why), Some(refusal)) if why.contains(refusal) && now == Some(wanted) => Ok(()),
            (Ok(said), None) if said.contains(words) && now == Some(wanted) => Ok(()),
            _ => Err(format!("{look_at} is at {now:?} (wanted {wanted:?}); he answered {answer:?} (wanted \"{words}\")")),
        };
        out.push((format!("\"{sentence}\" ({look_at})"), result));
    }
    // After the undo the whole mixer is as it was found, the master's later level aside.
    let mut left = bench().unwrap_or_default();
    left.master = scenario_bench().master;
    out.push(("the undo leaves every app as it was found".to_string(), if left == scenario_bench() { Ok(()) } else { Err(format!("left as {left:?}")) }));
    // A level with a sign is a step, whichever tool states it: "-50" never becomes the level 0.
    set_bench(Some(scenario_bench()));
    forget();
    let seen = |name: &str, answer: Result<String, String>, look_at: &str, wanted: (i32, bool)| {
        let now = if look_at == "master" { master() } else { level(look_at) };
        (name.to_string(), if answer.is_ok() && now == Some(wanted) { Ok(()) } else { Err(format!("{look_at} is at {now:?} (wanted {wanted:?}); answered {answer:?}")) })
    };
    let answer = player_volume(&json!("-50")).await;
    out.push(seen("the music tool's set_volume \"-50\" lowers the player by 50", answer, "sonora", (10, false)));
    let answer = player_volume(&json!("+30")).await;
    out.push(seen("the music tool's set_volume \"+30\" raises the player by 30", answer, "sonora", (40, false)));
    let answer = player_volume(&json!(20)).await;
    out.push(seen("the music tool's set_volume 20 sets the player, not the PC", answer, "sonora", (20, false)));
    out.push(seen("the music tool's volume leaves the master volume alone", Ok(String::new()), "master", (80, false)));
    let answer = tool(&json!({"action": "set_master", "level": "-50"})).await;
    out.push(seen("the mixer's set_master \"-50\" is a step down", answer, "master", (30, false)));
    let answer = tool(&json!({"action": "set_app", "app": "browser", "level": -20})).await;
    out.push(seen("the mixer's set_app -20 for the browser is a step down", answer, "firefox", (80, false)));
    // "It" means the last request only for a while; later it is the master volume again.
    memory().last = std::time::Instant::now().checked_sub(PRONOUN_WINDOW + std::time::Duration::from_secs(5)).map(|at| (Target::App("browser".into()), at));
    out.push(("\"it\" long after the last request is the master volume".to_string(), if target_of(Some("it")) == Target::Master { Ok(()) } else { Err("still the browser".to_string()) }));
    // A slider's drag: the last level it sent is the one that stands.
    for percent in [10u8, 35, 62, 48, 51] {
        slide("Master", percent).await;
    }
    out.push(("a slider's drag ends on its last level".to_string(), if master() == Some((51, false)) { Ok(()) } else { Err(format!("the master volume is at {:?}", master())) }));
    // With no player running the music tool's volume is the master's.
    set_bench(Some(Bench { master: (0.6, false), apps: Vec::new() }));
    let answer = player_volume(&json!("-50")).await;
    out.push(("without a player the music tool's \"-50\" lowers the master volume by 50".to_string(), if answer.is_ok() && master() == Some((10, false)) { Ok(()) } else { Err(format!("the master volume is at {:?}; answered {answer:?}", master())) }));
    // "Undo that": the last change and nothing before it, once; a request that moved nothing
    // leaves the last real change standing.
    set_bench(Some(scenario_bench()));
    forget();
    let undo = json!({"action": "undo_last"});
    let _ = say("Volume 40").await;
    let _ = say("Lower my browser's volume by 50%").await;
    let _ = say("Raise Discord by 10").await;
    let answer = tool(&undo).await;
    out.push(seen("\"undo that\" takes the browser's change back", answer, "firefox", (100, false)));
    out.push(seen("\"undo that\" leaves the change before it standing", Ok(String::new()), "master", (40, false)));
    let answer = tool(&undo).await;
    out.push(("a second \"undo that\" finds nothing, it never redoes".to_string(), if answer.is_err() && level("firefox") == Some((100, false)) { Ok(()) } else { Err(format!("firefox is at {:?}; answered {answer:?}", level("firefox"))) }));
    let _ = say("Mute everything except Discord").await;
    let answer = tool(&undo).await;
    out.push(seen("\"undo that\" after muting the others plays them again", answer, "sonora", (60, false)));
    let answer = say("Unmute").await;
    out.push(seen("they are no longer the next unmute's to look after", answer, "master", (40, false)));
    let _ = say("Mute").await;
    let answer = tool(&undo).await;
    out.push(seen("\"undo that\" after a mute unmutes the master", answer, "master", (40, false)));
    // Named mixes: saved as they stand, back in one go, taken back in one go; an app of the
    // mix that is gone is left out, the routers and Jarvis are no part of one.
    set_bench(Some(scenario_bench()));
    forget();
    let _ = say("Set Discord to 30 percent").await;
    let _ = say("Mute my browser").await;
    let _ = say("Volume 40").await;
    let answer = say("Save this mix as scenario test").await;
    let kept = crate::audio_mixes::find("scenario test");
    out.push(("the mix keeps Sir's apps as they stand and nothing else".to_string(), match &kept {
        Some(mix) if answer.is_ok() && mix.master == (0.4, false) && mix.apps.iter().any(|c| c.app == "Discord" && (c.level - 0.3).abs() < 0.01 && !c.muted) && mix.apps.iter().any(|c| c.app == "firefox" && c.muted) && !mix.apps.iter().any(|c| keeps_playing(&c.app)) => Ok(()),
        other => Err(format!("kept {other:?}; answered {answer:?}")),
    }));
    let _ = say("Full volume").await;
    let _ = say("Set Discord to 90 percent").await;
    let _ = say("Unmute my browser").await;
    let answer = say("Load my scenario test mix").await;
    out.push(seen("the mix brings Discord back", answer, "Discord", (30, false)));
    out.push(seen("the mix brings the mute back", Ok(String::new()), "firefox", (100, true)));
    out.push(seen("the mix brings the master volume back", Ok(String::new()), "master", (40, false)));
    let answer = tool(&undo).await;
    out.push(seen("\"undo that\" takes a whole mix back: the master", answer, "master", (100, false)));
    out.push(seen("\"undo that\" takes a whole mix back: Discord", Ok(String::new()), "Discord", (90, false)));
    out.push(seen("\"undo that\" takes a whole mix back: the browser", Ok(String::new()), "firefox", (100, false)));
    let answer = say("Scenario test mix").await;
    out.push(seen("the mix's name alone loads it", answer, "Discord", (30, false)));
    if let Some(mut bench) = bench() {
        bench.apps.retain(|a| a.name != "Discord");
        set_bench(Some(bench));
    }
    let answer = say("Load my scenario test mix").await;
    out.push(("an app that has gone is said to be left out".to_string(), if answer.as_ref().is_ok_and(|said| said.contains("left out") && said.contains("Discord")) { Ok(()) } else { Err(format!("answered {answer:?}")) }));
    let answer = say("Which mixes do I have").await;
    out.push(("the saved mixes are listed".to_string(), if answer.as_ref().is_ok_and(|said| said.contains("scenario test")) { Ok(()) } else { Err(format!("answered {answer:?}")) }));
    let answer = say("Delete the scenario test mix").await;
    let again = say("Load my scenario test mix").await;
    out.push(("a deleted mix is gone".to_string(), if answer.is_ok() && again.as_ref().is_err_and(|why| why.contains("no mix called scenario test")) { Ok(()) } else { Err(format!("answered {answer:?}, then {again:?}")) }));
    // "The game": what has a channel and is no everyday program, listed or not.
    let session = |name: &str| AppSession { name: name.to_string(), pid: 2, level: 1.0, muted: false, labels: Vec::new() };
    for game in ["javaw", "BatmanAK"] {
        let mut desk = scenario_bench();
        desk.apps.push(session(game));
        set_bench(Some(desk));
        forget();
        let answer = say("Turn the game down by 30 percent").await;
        out.push(seen(&format!("\"the game\" is {game}"), answer, game, (70, false)));
        out.push(seen(&format!("\"the game\" ({game}) leaves Discord alone"), Ok(String::new()), "Discord", (100, false)));
        let answer = say("Mute everything except the game").await;
        out.push(seen(&format!("\"everything except the game\" keeps {game}"), answer, game, (70, false)));
        out.push(seen(&format!("\"everything except the game\" ({game}) silences Discord"), Ok(String::new()), "Discord", (100, true)));
    }
    set_bench(Some(scenario_bench()));
    forget();
    let answer = say("Mute the game").await;
    out.push(("without a game, \"mute the game\" says so and changes nothing".to_string(), if answer.as_ref().is_err_and(|e| e.starts_with("No game")) && bench() == Some(scenario_bench()) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    let answer = say("Mute everything except the game").await;
    out.push(("without a game, nothing is muted for its sake".to_string(), if answer.is_err() && bench() == Some(scenario_bench()) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    // What a volume is at, read and said, nothing moved.
    let _ = say("Set Discord to 30 percent").await;
    let _ = say("Mute my browser").await;
    for (sentence, words) in [("What's my volume?", "master volume is at 80 %"), ("How loud is Discord?", "Discord is at 30 %"), ("Is my browser muted?", "Firefox is at 100 %, muted"), ("Is anything muted?", "muted")] {
        let answer = say(sentence).await;
        out.push((format!("\"{sentence}\" is read off the mixer"), if answer.as_ref().is_ok_and(|said| said.contains(words)) { Ok(()) } else { Err(format!("answered {answer:?} (wanted \"{words}\")")) }));
    }
    let answer = say("How loud is the game?").await;
    out.push(("without a game, its volume is not made up".to_string(), if answer.as_ref().is_ok_and(|said| said.contains("can't see a game")) { Ok(()) } else { Err(format!("answered {answer:?}")) }));
    // "Is anything muted?" names what is, and only that.
    let answer = say("Is anything muted?").await;
    out.push(("\"is anything muted?\" says what is, not the whole mixer".to_string(), if answer.as_deref() == Ok("Firefox is muted, Sir.") { Ok(()) } else { Err(format!("answered {answer:?}")) }));
    set_bench(Some(scenario_bench()));
    forget();
    let answer = say("Is anything muted?").await;
    out.push(("with nothing muted, he says so".to_string(), if answer.as_deref() == Ok("Nothing is muted, Sir.") { Ok(()) } else { Err(format!("answered {answer:?}")) }));
    // "Make it quiet" goes down to a level and never up to it.
    let answer = say("Make it quiet").await;
    out.push(("\"make it quiet\" goes down to a quiet level".to_string(), if bench().is_some_and(|b| (b.master.0 - 0.2).abs() < 0.005) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    let _ = say("Volume 10").await;
    let answer = say("Make it quiet").await;
    out.push(("below that level, \"make it quiet\" raises nothing".to_string(), if bench().is_some_and(|b| (b.master.0 - 0.1).abs() < 0.005) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    let answer = say("Make it loud").await;
    out.push(("\"make it loud\" goes up to a loud level".to_string(), if bench().is_some_and(|b| (b.master.0 - 0.8).abs() < 0.005) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    let _ = say("Volume 90").await;
    let answer = say("Make it loud").await;
    out.push(("above that level, \"make it loud\" lowers nothing".to_string(), if bench().is_some_and(|b| (b.master.0 - 0.9).abs() < 0.005) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    // Right after a change a bare direction is the volume's; after anything else it is nobody's.
    let _ = say("Volume 50").await;
    let answer = say("Up a bit").await;
    out.push(("\"up a bit\" right after a change goes on from it".to_string(), if bench().is_some_and(|b| (b.master.0 - 0.55).abs() < 0.005) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    let answer = say("Down").await;
    out.push(("\"down\" right after a change goes on from it".to_string(), if bench().is_some_and(|b| (b.master.0 - 0.45).abs() < 0.005) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    // A mute for a while ends by itself; one he ends himself takes the timer's job away.
    set_bench(Some(scenario_bench()));
    forget();
    let answer = say("Mute Discord for 10 minutes").await;
    out.push(seen("\"mute Discord for 10 minutes\" mutes it", answer.clone(), "Discord", (100, true)));
    out.push(("and says for how long".to_string(), if answer.as_ref().is_ok_and(|said| said.ends_with("for 10 minutes.")) { Ok(()) } else { Err(format!("answered {answer:?}")) }));
    let answer = say("Unmute Discord").await;
    out.push(seen("\"unmute Discord\" within the time plays it again", answer, "Discord", (100, false)));
    out.push(("and ends the timer".to_string(), if timed_mutes().is_empty() { Ok(()) } else { Err(format!("timers left: {:?}", timed_mutes())) }));
    let answer = tool(&json!({ "action": "mute_app", "app": "discord", "seconds": 1 })).await;
    out.push(seen("a timed mute mutes", answer, "Discord", (100, true)));
    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
    out.push(seen("and ends by itself", Ok(String::new()), "Discord", (100, false)));
    // A fade arrives (on the pretend mixer, at once), comes back, and is one step for the undo.
    let _ = say("Discord to 60 percent").await;
    let answer = say("Fade out Discord").await;
    out.push(seen("\"fade out Discord\" goes to silence", answer, "Discord", (0, false)));
    let answer = say("Fade Discord back in").await;
    out.push(seen("\"fade Discord back in\" goes back to where it was", answer, "Discord", (60, false)));
    let _ = say("Fade out Discord").await;
    let answer = tool(&undo).await;
    out.push(seen("\"undo that\" takes a fade back in one step", answer, "Discord", (60, false)));
    let answer = say("Turn Discord down slowly").await;
    out.push(seen("\"turn Discord down slowly\" is a step", answer, "Discord", (40, false)));
    // Against another app's level, and every app's own level.
    set_bench(Some(scenario_bench()));
    forget();
    let _ = say("Discord to 60 percent").await;
    let answer = say("Make Firefox as loud as Discord").await;
    out.push(seen("\"as loud as Discord\" is Discord's level", answer, "Firefox", (60, false)));
    let answer = say("Make Firefox quieter than Discord").await;
    out.push(seen("\"quieter than Discord\" is below it", answer, "Firefox", (50, false)));
    let answer = say("Set all apps to 70 percent").await;
    out.push(seen("\"all apps\" are the apps", answer, "Discord", (70, false)));
    out.push(("and not the master".to_string(), if bench().is_some_and(|b| (b.master.0 - scenario_bench().master.0).abs() < 0.005) { Ok(()) } else { Err(format!("the mixer is {:?}", bench())) }));
    // Questions never move anything.
    set_bench(Some(scenario_bench()));
    forget();
    let _ = say("Mute").await;
    let answer = say("Is the sound on?").await;
    out.push(("\"is the sound on?\" unmutes nothing".to_string(), if bench().is_some_and(|b| b.master.1) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    set_bench(Some(scenario_bench()));
    forget();
    // "Make it quiet" has a way, and "more" goes on along it.
    let _ = say("Volume 60").await;
    let _ = say("Make it quiet").await;
    let answer = say("More").await;
    out.push(("\"more\" after \"make it quiet\" is quieter still".to_string(), if bench().is_some_and(|b| (b.master.0 - 0.1).abs() < 0.005) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    let _ = say("Make it loud").await;
    let answer = say("A bit more").await;
    out.push(("\"a bit more\" after \"make it loud\" is louder still".to_string(), if bench().is_some_and(|b| (b.master.0 - 0.85).abs() < 0.005) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    set_bench(Some(scenario_bench()));
    forget();
    // A vendor's agent with a channel of its own is no game (Logitech's made "mute the game" hit it).
    let mut agents = scenario_bench();
    agents.apps.push(AppSession { name: "lghub_agent".to_string(), pid: 2, level: 1.0, muted: false, labels: Vec::new() });
    set_bench(Some(agents.clone()));
    forget();
    let answer = say("Mute the game").await;
    out.push(("a hardware maker's agent is no game".to_string(), if answer.is_err() && bench() == Some(agents) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    // Sound in loose words: no number, often no "volume" either.
    set_bench(Some(scenario_bench()));
    forget();
    let loose: [(&str, &str, (i32, bool)); 24] = [
        ("Too loud", "master", (70, false)),
        ("It's way too loud", "master", (50, false)),
        ("Too quiet", "master", (60, false)),
        // "Too loud" after a request about the browser is about the browser.
        ("Lower my browser's volume by 50%", "firefox", (50, false)),
        ("Too loud", "firefox", (40, false)),
        ("Discord is too loud", "Discord", (90, false)),
        ("Discord to half", "Discord", (50, false)),
        ("Put Discord at max", "Discord", (100, false)),
        ("Set Discord to 30", "Discord", (30, false)),
        ("Turn it all the way up", "Discord", (100, false)),
        ("Turn it all the way down", "Discord", (0, false)),
        ("Turn the volume all the way up", "master", (100, false)),
        ("Turn off the sound", "master", (100, true)),
        ("Turn the sound back on", "master", (100, false)),
        ("Lower everything by 10 percent", "master", (90, false)),
        ("Everything to 50 percent", "master", (50, false)),
        ("Turn everything up", "master", (60, false)),
        ("Mach den Ton aus", "master", (60, true)),
        ("Mach den Ton wieder an", "master", (60, false)),
        ("Das ist viel zu laut", "master", (40, false)),
        ("Dreh die Musik auf", "sonora", (70, false)),
        ("Mach die Musik ganz leise", "sonora", (10, false)),
        ("Discord auf Maximum", "Discord", (100, false)),
        ("Stell Discord auf 50", "Discord", (50, false)),
    ];
    for (sentence, look_at, wanted) in loose {
        let answer = say(sentence).await;
        out.push(seen(&format!("\"{sentence}\" ({look_at})"), answer, look_at, wanted));
    }
    // Right after a change: the same way, back a little, past where it was, or the same to
    // another app.
    set_bench(Some(scenario_bench()));
    forget();
    let answers: [(&str, &str, (i32, bool)); 15] = [
        ("Turn Discord down", "Discord", (90, false)),
        ("More", "Discord", (80, false)),
        ("A bit more", "Discord", (75, false)),
        ("Less", "Discord", (80, false)),
        ("Same for the music", "sonora", (65, false)),
        ("Volume 40", "master", (40, false)),
        ("More", "master", (50, false)),
        ("A bit less", "master", (45, false)),
        ("The other way", "master", (55, false)),
        ("Noch mehr", "master", (65, false)),
        ("Set Discord to 30", "Discord", (30, false)),
        ("And the music too", "sonora", (30, false)),
        ("Mute Discord", "Discord", (30, true)),
        ("Firefox too", "firefox", (100, true)),
        ("Make Discord loud again", "Discord", (100, false)),
    ];
    for (sentence, look_at, wanted) in answers {
        let answer = say(sentence).await;
        out.push(seen(&format!("\"{sentence}\" right after a change ({look_at})"), answer, look_at, wanted));
    }
    // Only his own change goes back: the browser stays muted, the music where it is.
    out.push(seen("\"make Discord loud again\" leaves the browser", Ok(String::new()), "firefox", (100, true)));
    out.push(seen("\"make Discord loud again\" leaves the music", Ok(String::new()), "sonora", (30, false)));
    let answer = tool(&undo).await;
    out.push(seen("\"undo that\" takes Discord's way back back", answer, "Discord", (30, true)));
    // Any other request ends it: "more" is not the volume's any more.
    let before = bench();
    crate::intent_engine::forget_follow_up();
    let answer = say("More").await;
    out.push(("after anything else, \"more\" moves no volume".to_string(), if answer.is_err() && bench() == before { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    // "I can't hear anything": what is in the way is put right as one change; with nothing in
    // the way nothing moves, and that is what he hears.
    let mut blocked = scenario_bench();
    blocked.master = (0.45, true);
    blocked.apps.iter_mut().filter(|a| a.name == "Discord").for_each(|a| a.muted = true);
    set_bench(Some(blocked.clone()));
    forget();
    let answer = say("I can't hear anything").await;
    out.push(seen("\"I can't hear anything\" unmutes the master volume", answer.clone(), "master", (45, false)));
    out.push(seen("\"I can't hear anything\" unmutes an app too", Ok(String::new()), "Discord", (100, false)));
    out.push(("what was in the way is said".to_string(), if answer.as_ref().is_ok_and(|said| said.contains("master volume was muted") && said.contains("Discord was muted")) { Ok(()) } else { Err(format!("answered {answer:?}")) }));
    let answer = tool(&undo).await;
    out.push(("\"undo that\" takes the whole fix back".to_string(), if answer.is_ok() && bench() == Some(blocked) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    set_bench(Some(scenario_bench()));
    forget();
    let answer = say("There's no sound").await;
    out.push(("with nothing in the way nothing moves, and he says so".to_string(), if answer.as_ref().is_ok_and(|said| said.contains("Nothing is muted")) && bench() == Some(scenario_bench()) { Ok(()) } else { Err(format!("answered {answer:?}; the mixer is {:?}", bench())) }));
    let mut low = scenario_bench();
    low.master = (0.02, false);
    set_bench(Some(low));
    forget();
    let answer = say("Ich höre nichts").await;
    out.push(seen("a master volume nearly at zero comes up to where it can be heard", answer, "master", (30, false)));
    let mut quiet = scenario_bench();
    quiet.apps.iter_mut().for_each(|a| match a.name.as_str() {
        "Discord" => a.level = 0.01,
        "firefox" => a.muted = true,
        _ => {}
    });
    set_bench(Some(quiet));
    forget();
    let answer = say("I can't hear Discord").await;
    out.push(seen("\"I can't hear Discord\" brings Discord up", answer, "Discord", (50, false)));
    out.push(seen("\"I can't hear Discord\" leaves the muted browser alone", Ok(String::new()), "firefox", (100, true)));
    let answer = say("I can't hear Spotify").await;
    out.push(("an app that plays nothing is said to play nothing".to_string(), if answer.as_ref().is_ok_and(|said| said.contains("not playing any audio")) { Ok(()) } else { Err(format!("answered {answer:?}")) }));
    // The Mixer block: the master first, an app once however many sessions it has, and no fader
    // for what must keep playing.
    let mut sessions = scenario_bench();
    sessions.apps.push(AppSession { name: "Discord".to_string(), pid: 2, level: 0.4, muted: true, labels: Vec::new() });
    set_bench(Some(sessions));
    forget();
    let faders = channels().await.map(|list| list.iter().map(|c| (c.target.clone(), c.percent, c.muted)).collect::<Vec<_>>());
    let wanted = [("master", 80, false), ("discord", 100, false), ("firefox", 100, false), ("sonora", 60, false), ("steam", 100, false), ("system sounds", 100, false)].map(|(t, p, m)| (t.to_string(), p, m)).to_vec();
    out.push(("the Mixer block has a fader for every channel, and none for what must keep playing".to_string(), if faders.as_ref() == Ok(&wanted) { Ok(()) } else { Err(format!("found {faders:?}")) }));
    forget();
    set_bench(earlier);
    out
}

/// The real mixer's read and write paths without a change: the master volume and every app's
/// own volume are written back exactly as they stand (no level moves, nothing is heard), and
/// read again. What it reached, or what failed.
pub async fn write_path_check() -> Result<String, String> {
    if bench().is_some() {
        return Err("the pretend mixer is in Core Audio's place".into());
    }
    let (before, after) = com(|| {
        let before = retrying(imp::master_state)?;
        retrying(|| imp::master_write(before, before, true))?;
        Ok((before, retrying(imp::master_state)?))
    })
    .await?;
    if (before.0 - after.0).abs() > 0.004 || before.1 != after.1 {
        return Err(format!("the master volume moved from {before:?} to {after:?} when it was written back as it was"));
    }
    // Every app session but the routers' and Jarvis' own (which nothing ever writes to).
    let apps = com(|| retrying(|| imp::apply(&[], true, Change::default()))).await?;
    let now = com(imp::list).await?;
    for app in &apps {
        if let Some(live) = now.iter().find(|s| s.name.eq_ignore_ascii_case(&app.name)) {
            if (live.level - app.before.0).abs() > 0.004 || live.muted != app.before.1 {
                return Err(format!("{} moved from {:?} to ({}, {}) when it was written back as it was", app.name, app.before, live.level, live.muted));
            }
        }
    }
    let outputs = com(imp::outputs).await?;
    let default = outputs.iter().find(|d| d.default).map_or("no default output".to_string(), |d| d.name.clone());
    Ok(format!("master {} on {default} and {} app channel(s) written back unchanged; {} output device(s)", pct(after.0), apps.len(), outputs.len()))
}

/// Spoken confirmation of a mixer call (short; silent quick commands say nothing at all).
pub fn ack(args: &Value, output: &str) -> String {
    let action = str_arg(args, &["action"]).unwrap_or("");
    match action {
        "list" | "list_apps" | "status" | "list_mixes" | "mixes" | "get_master" | "get_app" | "muted" | "list_muted" | "no_sound" | "cant_hear" | "find_sound" | "device" | "output_device" | "devices" | "outputs" | "output_devices" | "switch_device" => output.to_string(),
        _ if bool_arg(args, "quiet") == Some(true) => String::new(),
        // (Nothing was done, so nothing is reported as done.)
        _ if output.contains(" already at ") => output.to_string(),
        _ => format!("Done, Sir. {output}"),
    }
}

/// The `control_audio_mixer` tool schema.
pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "control_audio_mixer",
        "description": "EVERY volume request goes here (never control_music): the PC's master volume and each app's own (browser, Discord, Sonora, Spotify, games …). \"Set Sonora to 30 % and Discord to 100 %\" → set_apps; \"lower my browser by 50 %\" → change_app browser delta -50; \"make it 20 % louder\" → change_app app it delta 20 (it = the last request's app); \"mute Chrome\" → mute_app; \"mute everything except Discord\" → mute_others discord; \"lower the volume by 5 %\" → change_master delta -5; \"make everything loud again\" → restore; save_mix / load_mix (name); \"what's playing audio\" → list; \"I can't hear anything\" → no_sound (fixes what is muted or nearly off). A level is 0-100 and absolute; a change is ALWAYS a delta.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["set_master", "change_master", "mute_master", "unmute_master", "set_app", "set_apps", "change_app", "mute_app", "unmute_app", "mute_others", "restore", "undo_last", "get_master", "get_app", "muted", "quiet", "loud", "fade", "match_app", "set_all_apps", "no_sound", "save_mix", "load_mix", "delete_mix", "list_mixes", "list", "device", "devices", "switch_device"]},
            "seconds": {"type": "number", "description": "mute_app / mute_master: the mute ends by itself after this long"},
            "way": {"type": "string", "enum": ["out", "in"], "description": "fade: out to silence, in back to where it was"},
            "to": {"type": "string", "description": "match_app: the app whose level to go by"},
            "offset": {"type": "number", "description": "match_app: volume points above (or below) the other app"},
            "name": {"type": "string", "description": "save_mix / load_mix / delete_mix: the mix's name (gaming, movie, late night)"},
            "app": {"type": "string", "description": "App name for set_app / change_app / mute_app / unmute_app / mute_others (the one that keeps playing) / restore (that app alone); \"it\" = the app of the last volume request"},
            "level": {"type": "number", "description": "0-100 percent, absolute"},
            "delta": {"type": "number", "description": "change_master / change_app: volume points from where it is, negative = quieter"},
            "apps": {"type": "array", "items": {"type": "object", "properties": {"app": {"type": "string"}, "level": {"type": "number"}}}}
        }, "required": ["action"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_names_and_targets() {
        assert!(is_app_name("Sonora"));
        assert!(is_app_name("discord"));
        assert!(is_app_name("the browser"));
        assert!(!is_app_name("volume"));
        assert!(!is_app_name("timer"));
        assert_eq!(targets("Edge"), vec!["msedge".to_string()]);
        assert!(matches_target("Discord", &targets("discord")));
        assert!(matches_target("chrome", &targets("browser")));
        assert!(!matches_target("Discord", &targets("spotify")));
        assert!(matches_target("JARVIS", &targets("jarvis")));
        assert!(!is_app_name("zoom") && !is_app_name("your voice"), "the HUD zoom and Jarvis' voice are not apps");
    }

    /// Reads the real mixer (nothing is changed: no sound, no volume).
    #[cfg(windows)]
    #[test]
    fn the_real_mixer_lists_its_sessions() {
        let listed = std::thread::spawn(|| {
            // SAFETY: COM for this test thread, as the Win32 worker does for itself.
            let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED) };
            (imp::master_state(), imp::list())
        })
        .join()
        .unwrap();
        // A machine without an output device has no mixer; otherwise both reads succeed.
        if let (Ok((master, _)), Ok(apps)) = listed {
            assert!((0.0..=1.0).contains(&master));
            assert!(apps.iter().all(|a| !a.name.is_empty() && (0.0..=1.0).contains(&a.level)));
        }
    }

    /// The machine's output devices as the mixer sees them (nothing is changed, no sound):
    /// `cargo test --release mixer_lab -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore = "prints this machine's output devices"]
    fn mixer_lab() {
        let seen = std::thread::spawn(|| (imp::outputs(), imp::master_state(), imp::list())).join().unwrap();
        let (outputs, master, apps) = seen;
        println!("master (default endpoint): {master:?}");
        for d in outputs.unwrap_or_default() {
            println!("{}{}: {:.0} %{}, peak {:.3}, apps: {}", d.name, if d.default { " [default]" } else { "" }, d.level * 100.0, if d.muted { " muted" } else { "" }, d.peak, d.apps.join(", "));
        }
        for a in apps.unwrap_or_default() {
            println!("session {} (pid {}): {:.0} %{} labels {:?}", a.name, a.pid, a.level * 100.0, if a.muted { " muted" } else { "" }, a.labels);
        }
    }

    /// Every volume scenario on the pretend mixer: Sir's own sentences of 2026-11-06, steps,
    /// "it", mutes, "everything except", the undo, signed levels, the slider.
    #[tokio::test]
    async fn every_volume_scenario_ends_where_it_should() {
        let results = scenarios().await;
        let failed: Vec<String> = results.iter().filter_map(|(name, r)| r.as_ref().err().map(|why| format!("{name}: {why}"))).collect();
        assert!(failed.is_empty(), "{} of {} volume scenarios failed:\n  {}", failed.len(), results.len(), failed.join("\n  "));
        assert!(results.len() >= 40, "only {} scenarios ran", results.len());
        assert_eq!(bench(), None, "the pretend mixer is put away afterwards");
    }

    #[test]
    fn a_change_raises_lowers_and_mutes_as_said() {
        let step = |points: f32| Change { step: Some(points / 100.0), ..Change::default() };
        // A step up ends a mute, a step down keeps it; both stop at the ends of the scale.
        assert_eq!(step(20.0).on((0.9, true)), (1.0, false));
        assert_eq!(step(-20.0).on((0.1, true)), (0.0, true));
        // A level above zero ends a mute; the level zero and an explicit mute do not.
        assert_eq!(Change { level: Some(0.4), ..Change::default() }.on((0.9, true)), (0.4, false));
        assert_eq!(Change { level: Some(0.0), ..Change::default() }.on((0.9, true)), (0.0, true));
        assert_eq!(Change { level: Some(0.4), mute: Some(true), ..Change::default() }.on((0.9, false)), (0.4, true));
        // The default changes nothing.
        assert_eq!(Change::default().on((0.37, true)), (0.37, true));
        // A level with a sign is a step; a plain level is none.
        assert_eq!(signed_step(&json!("-50")), Some(-50.0));
        assert_eq!(signed_step(&json!("+30%")), Some(30.0));
        assert_eq!(signed_step(&json!(-20)), Some(-20.0));
        assert_eq!(signed_step(&json!(40)), None);
        assert_eq!(signed_step(&json!("40")), None);
        // Several apps in one name, and the sessions that are never muted with "everything".
        assert_eq!(targets("Discord and Spotify"), vec!["discord", "discordptb", "discordcanary", "spotify"]);
        assert_eq!(spoken("my browser and discord"), "Browser and Discord");
        assert!(keeps_playing("Elgato.WaveLink") && keeps_playing("JARVIS") && !keeps_playing("firefox"));
    }

    #[test]
    fn levels_in_percent_or_fraction() {
        assert_eq!(level_of(&json!(30)), Some(0.3));
        assert_eq!(level_of(&json!("30%")), Some(0.3));
        assert_eq!(level_of(&json!(0.5)), Some(0.5));
        assert_eq!(level_of(&json!(100)), Some(1.0));
        assert_eq!(level_of(&json!(1)), Some(0.01), "a whole number is per cent");
        assert_eq!(level_of(&json!(1.0)), Some(1.0), "a fraction");
        assert_eq!(level_of(&json!(250)), Some(1.0));
    }
}
