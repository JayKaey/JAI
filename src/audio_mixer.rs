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
    ("jarvis", &["jarvis"]),
    ("system sounds", &["system sounds"]),
    ("systemsounds", &["system sounds"]),
];

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

/// A name the mixer understands: a known alias or an installed application.
pub fn is_app_name(name: &str) -> bool {
    let n = clean(name);
    !n.is_empty()
        && n.split_whitespace().count() <= 3
        && !NOT_APPS.contains(&n.as_str())
        && (ALIASES.iter().any(|(alias, _)| *alias == n) || crate::appindex::is_known(&n) || crate::apps::find_spec(&n).is_some())
}

/// The executable names `name` means.
fn targets(name: &str) -> Vec<String> {
    let n = clean(name);
    match ALIASES.iter().find(|(alias, _)| *alias == n) {
        Some((_, exes)) => exes.iter().map(|e| e.to_string()).collect(),
        None => vec![n.replace(' ', "")],
    }
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
    let mut c = exe.chars();
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

    pub fn set_master(level: f32) -> Result<(), String> {
        let ep = master()?;
        // SAFETY: valid interface; a null event context is allowed.
        unsafe {
            if level > 0.0 {
                let _ = ep.SetMute(BOOL::from(false), std::ptr::null());
            }
            ep.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null()).map_err(err)
        }
    }

    pub fn set_master_mute(muted: bool) -> Result<(), String> {
        let ep = master()?;
        // SAFETY: valid interface; a null event context is allowed.
        unsafe { ep.SetMute(BOOL::from(muted), std::ptr::null()).map_err(err) }
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

    /// Sets volume and/or mute on every session of the wanted apps (all of them: Discord and
    /// Chrome play through several processes); returns the session names.
    pub fn apply(wanted: &[String], level: Option<f32>, mute: Option<bool>) -> Result<Vec<String>, String> {
        let all = sessions()?;
        let mut chosen: Vec<&(AppSession, ISimpleAudioVolume)> = all.iter().filter(|(s, _)| super::session_matches(s, wanted)).collect();
        if chosen.is_empty() {
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
        let mut hit: Vec<String> = Vec::new();
        let mut failure: Option<String> = None;
        for (s, volume) in chosen {
            // SAFETY: valid ISimpleAudioVolume; a null event context is allowed.
            let result = unsafe {
                let mut r = Ok(());
                if let Some(l) = level {
                    r = volume.SetMasterVolume(l.clamp(0.0, 1.0), std::ptr::null());
                    if r.is_ok() && l > 0.0 && mute.is_none() {
                        let _ = volume.SetMute(BOOL::from(false), std::ptr::null());
                    }
                }
                if let (Some(m), true) = (mute, r.is_ok()) {
                    r = volume.SetMute(BOOL::from(m), std::ptr::null());
                }
                r
            };
            // One stubborn session (a protected process) never stops the others.
            match result {
                Ok(()) => {
                    if !hit.iter().any(|h| h.eq_ignore_ascii_case(&s.name)) {
                        hit.push(s.name.clone());
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
    pub fn set_master(_: f32) -> Result<(), String> {
        master_state().map(|_| ())
    }
    pub fn set_master_mute(_: bool) -> Result<(), String> {
        master_state().map(|_| ())
    }
    pub fn list() -> Result<Vec<AppSession>, String> {
        master_state().map(|_| Vec::new())
    }
    pub fn apply(_: &[String], _: Option<f32>, _: Option<bool>) -> Result<Vec<String>, String> {
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

/// A master-volume change through the three layers; `describe` phrases the result of layer 1
/// or 2 from the level and mute state afterwards.
async fn master_change(level: Option<f32>, mute: Option<bool>, delta: Option<f32>, describe: fn(f32, bool) -> String) -> Result<String, String> {
    let layer1 = com(move || {
        if let Some(d) = delta {
            let (now, _) = imp::master_state()?;
            imp::set_master((now + d / 100.0).clamp(0.0, 1.0))?;
        }
        if let Some(l) = level {
            imp::set_master(l)?;
        }
        if let Some(m) = mute {
            imp::set_master_mute(m)?;
        }
        imp::master_state()
    })
    .await;
    match layer1 {
        Ok((now, muted)) => {
            // The Dynamic-Island volume bar shows where it landed.
            crate::dynamic_popups::volume("Master", (now * 100.0).round() as u8, muted);
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

pub async fn set_master_mute(muted: bool) -> Result<String, String> {
    master_change(None, Some(muted), None, |_, m| if m { "Master output muted.".into() } else { "Master output unmuted.".into() }).await
}

/// The master volume (0.0-1.0) and mute state, through Core Audio or the PowerShell endpoint.
pub async fn master_state() -> Result<(f32, bool), String> {
    let error = match com(imp::master_state).await {
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

pub async fn set_app_volume(app: &str, level: f32) -> Result<String, String> {
    let wanted = targets(app);
    let level = level.clamp(0.0, 1.0);
    let hit = com(move || imp::apply(&wanted, Some(level), None)).await?;
    if hit.is_empty() {
        return Err(format!("{} is not playing any audio right now, so it has no mixer channel to set.", display_name(&clean(app))));
    }
    crate::dynamic_popups::volume(&display_name(&clean(app)), (level * 100.0).round() as u8, false);
    Ok(format!("{} set to {}.", hit.join(", "), pct(level)))
}

pub async fn set_app_mute(app: &str, muted: bool) -> Result<String, String> {
    let wanted = targets(app);
    let hit = com(move || imp::apply(&wanted, None, Some(muted))).await?;
    if hit.is_empty() {
        return Err(format!("{} has no audio session right now.", display_name(&clean(app))));
    }
    Ok(format!("{} {}.", hit.join(", "), if muted { "muted" } else { "unmuted" }))
}

pub async fn list_audio_apps() -> Result<Vec<AppSession>, String> {
    com(imp::list).await
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
    let level = args.get("level").or_else(|| args.get("volume")).or_else(|| args.get("value")).and_then(level_of);
    match action.as_str() {
        "set_master" | "set_volume" | "master" => set_master_volume(level.ok_or("set_master needs level (0-100).")?).await,
        "change_master" | "volume_up" | "volume_down" | "change" => {
            let delta = args.get("delta").or_else(|| args.get("step")).and_then(|v| v.as_f64().or_else(|| v.as_str()?.trim().trim_end_matches('%').parse().ok())).unwrap_or(10.0) as f32;
            let delta = if action == "volume_down" { -delta.abs() } else { delta };
            change_master_volume(delta).await
        }
        "mute_master" | "mute" if app.is_none() => set_master_mute(true).await,
        "unmute_master" | "unmute" if app.is_none() => set_master_mute(false).await,
        "set_app" | "set_app_volume" | "app_volume" => {
            let app = app.ok_or("set_app needs app.")?;
            set_app_volume(app, level.ok_or("set_app needs level (0-100).")?).await
        }
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
        "mute_app" | "mute" => set_app_mute(app.ok_or("mute_app needs app.")?, true).await,
        "unmute_app" | "unmute" => set_app_mute(app.ok_or("unmute_app needs app.")?, false).await,
        "list" | "list_apps" | "status" => {
            let (master, muted) = master_state().await?;
            let apps = list_audio_apps().await?;
            let rows: Vec<String> = apps.iter().map(|a| format!("{} {}{}", a.name, pct(a.level), if a.muted { " (muted)" } else { "" })).collect();
            Ok(format!(
                "Master {}{}. Apps playing audio: {}.",
                pct(master),
                if muted { " (muted)" } else { "" },
                if rows.is_empty() { "none".into() } else { rows.join(", ") }
            ))
        }
        other => Err(format!("Unknown mixer action '{other}' (set_master, change_master, mute_master, unmute_master, set_app, set_apps, mute_app, unmute_app, list).")),
    }
}

/// Spoken confirmation of a mixer call (short; silent quick commands say nothing at all).
pub fn ack(args: &Value, output: &str) -> String {
    let action = str_arg(args, &["action"]).unwrap_or("");
    match action {
        "list" | "list_apps" | "status" => output.to_string(),
        _ if bool_arg(args, "quiet") == Some(true) => String::new(),
        _ => format!("Done, Sir. {output}"),
    }
}

/// The `control_audio_mixer` tool schema.
pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "control_audio_mixer",
        "description": "Windows volume mixer: the master volume and each app's own volume (Sonora, Spotify, Discord, Chrome, games …). \"Set Sonora to 30 % and Discord to 100 %\" → set_apps; \"mute Chrome\" → mute_app; \"lower the volume by 5 %\" → change_master delta -5; \"what's playing audio\" → list.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["set_master", "change_master", "mute_master", "unmute_master", "set_app", "set_apps", "mute_app", "unmute_app", "list"]},
            "app": {"type": "string", "description": "App name for set_app / mute_app / unmute_app"},
            "level": {"type": "number", "description": "0-100 percent"},
            "delta": {"type": "number", "description": "change_master: volume points, negative = quieter"},
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
