//! KDE Plasma's own services over the session bus, for J.A.I. on Linux: KWin's screenshots and
//! window shortcuts, global keys (KGlobalAccel), the screens' brightness (PowerDevil), the lock
//! screen and notifications. A missing service (another desktop, a test container) is an error
//! for the caller to report, never a panic.

use std::collections::HashMap;
use std::sync::OnceLock;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const KGLOBALACCEL: &str = "org.kde.kglobalaccel";

/// The session bus, opened once.
pub fn bus() -> Result<&'static Connection, String> {
    static BUS: OnceLock<Result<Connection, String>> = OnceLock::new();
    BUS.get_or_init(|| Connection::session().map_err(|e| format!("no session bus: {e}"))).as_ref().map_err(Clone::clone)
}

/// One method call on the session bus: `call::<_, R>(service, path, interface, method, &(args,))`.
pub fn call<B, R>(service: &str, path: &str, interface: &str, method: &str, body: &B) -> Result<R, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    let reply = bus()?.call_method(Some(service), path, Some(interface), method, body).map_err(|e| e.to_string())?;
    reply.body().deserialize::<R>().map_err(|e| e.to_string())
}

/// One property of an object on the session bus.
pub fn property<T>(service: &str, path: &str, interface: &str, name: &str) -> Result<T, String>
where
    T: TryFrom<OwnedValue>,
    T::Error: Into<zbus::Error>,
{
    Proxy::new(bus()?, service, path, interface).and_then(|p| p.get_property(name)).map_err(|e| e.to_string())
}

/// Blocking work (a bus call, a process) off the async runtime: `win32::call`'s Linux twin.
pub async fn blocking<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Result<R, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())
}

// ------------------------------------------------------------------------------ shortcuts ----

/// Runs one of KWin's own shortcuts by its name ("Window Quick Tile Left", "Window to Next
/// Screen", "Window Close"): each acts on the window in front.
pub fn kwin_shortcut(name: &str) -> Result<(), String> {
    call::<_, ()>(KGLOBALACCEL, "/component/kwin", "org.kde.kglobalaccel.Component", "invokeShortcut", &(name,))
}

/// A key as Qt numbers it: `Alt+J`, `Ctrl+Alt+K`, `F9` … (KGlobalAccel's own coding). None for
/// a name it does not know.
pub fn qt_key(text: &str) -> Option<i32> {
    let mut code = 0;
    for part in text.split('+').map(str::trim) {
        code |= match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "strg" => 0x0400_0000,
            "alt" => 0x0800_0000,
            "shift" => 0x0200_0000,
            "win" | "super" | "meta" => 0x1000_0000,
            "space" => 0x20,
            key if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() => key.to_ascii_uppercase().as_bytes()[0] as i32,
            key => match key.strip_prefix('f').and_then(|n| n.parse::<i32>().ok()) {
                Some(n @ 1..=24) => 0x0100_0030 + n - 1,
                _ => return None,
            },
        };
    }
    (code & 0x01FF_FFFF != 0).then_some(code)
}

/// Registers J.A.I.'s global keys with KDE (they then show in System Settings › Shortcuts, where
/// Sir may give them other keys) and calls `pressed` with the action's name each time its keys
/// are pressed, on a thread of its own. `keys` are (action, its name in the settings, Qt key).
pub fn global_keys(keys: &[(&str, &str, i32)], pressed: impl Fn(&str) + Send + 'static) -> Result<(), String> {
    const API: &str = "org.kde.KGlobalAccel";
    for (action, friendly, key) in keys {
        let id = ["jai", *action, "J.A.I.", *friendly];
        call::<_, ()>(KGLOBALACCEL, "/kglobalaccel", API, "doRegister", &(id,))?;
        let sequence: Vec<(Vec<i32>,)> = vec![(vec![*key],)];
        // (IsDefault, then SetPresent: a key Sir chose in the settings stays his.)
        for flags in [8u32, 2] {
            call::<_, Vec<(Vec<i32>,)>>(KGLOBALACCEL, "/kglobalaccel", API, "setShortcutKeys", &(id, &sequence, flags))?;
        }
    }
    let component: OwnedObjectPath = call(KGLOBALACCEL, "/kglobalaccel", API, "getComponent", &("jai",))?;
    let proxy = Proxy::new(bus()?, KGLOBALACCEL, component, "org.kde.kglobalaccel.Component").map_err(|e| e.to_string())?;
    std::thread::Builder::new()
        .name("jai-keys".into())
        .spawn(move || {
            let Ok(signals) = proxy.receive_signal("globalShortcutPressed") else { return };
            for signal in signals {
                if let Ok((_, action, _)) = signal.body().deserialize::<(String, String, i64)>() {
                    pressed(&action);
                }
            }
        })
        .map(drop)
        .map_err(|e| e.to_string())
}

// ----------------------------------------------------------------------------- screenshots ----

/// What a screenshot takes.
#[derive(Debug, Clone, PartialEq)]
pub enum Shot {
    /// Every monitor, as they stand side by side.
    Workspace,
    /// One monitor by its output name ("DP-1").
    Screen(String),
    /// The window in front.
    ActiveWindow,
}

/// A screenshot through KWin (his own windows left out), else through Spectacle.
pub fn screenshot(shot: &Shot) -> Result<image::RgbaImage, String> {
    kwin_shot(shot).or_else(|kwin| spectacle(shot).map_err(|spectacle| format!("{kwin}; {spectacle}")))
}

fn kwin_shot(shot: &Shot) -> Result<image::RgbaImage, String> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let mut ends = [0; 2];
    // SAFETY: pipe2 fills two descriptors on success; each is owned exactly once below.
    if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: both were just opened by pipe2 and are owned by nothing else.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
    let options: HashMap<&str, Value> = HashMap::from([("native-resolution", true.into()), ("include-cursor", false.into()), ("hide-caller-windows", true.into())]);
    let pipe = zbus::zvariant::Fd::from(&write);
    let (service, path, api) = ("org.kde.KWin.ScreenShot2", "/org/kde/KWin/ScreenShot2", "org.kde.KWin.ScreenShot2");
    let results: HashMap<String, OwnedValue> = match shot {
        Shot::Workspace => call(service, path, api, "CaptureWorkspace", &(&options, pipe)),
        Shot::Screen(name) => call(service, path, api, "CaptureScreen", &(name.as_str(), &options, pipe)),
        Shot::ActiveWindow => call(service, path, api, "CaptureActiveWindow", &(&options, pipe)),
    }?;
    // (KWin writes the pixels after its answer; the copy kept here would hold the pipe open.)
    drop(write);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::fs::File::from(read), &mut bytes).map_err(|e| e.to_string())?;
    let number = |key: &str| results.get(key).and_then(|v| u32::try_from(v.try_clone().ok()?).ok()).ok_or_else(|| format!("KWin's screenshot has no {key}"));
    raw_image(&bytes, number("width")?, number("height")?, number("stride")?, number("format")?)
}

/// KWin's raw pixels (a QImage's format number) as RGBA.
fn raw_image(bytes: &[u8], width: u32, height: u32, stride: u32, format: u32) -> Result<image::RgbaImage, String> {
    // QImage: 4 RGB32, 5 ARGB32, 6 ARGB32 premultiplied (B G R A in memory); 16 RGBX8888,
    // 17 RGBA8888, 18 RGBA8888 premultiplied (R G B A in memory).
    let bgra = match format {
        4..=6 => true,
        16..=18 => false,
        other => return Err(format!("KWin's screenshot came in an unknown format ({other})")),
    };
    let opaque = matches!(format, 4 | 16);
    let (w, h, stride) = (width as usize, height as usize, stride as usize);
    if stride < w * 4 || bytes.len() < stride * h.saturating_sub(1) + w * 4 {
        return Err("KWin's screenshot came short".into());
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for row in bytes.chunks(stride).take(h) {
        for px in row[..w * 4].chunks_exact(4) {
            let (r, b) = if bgra { (px[2], px[0]) } else { (px[0], px[2]) };
            out.extend_from_slice(&[r, px[1], b, if opaque { 255 } else { px[3] }]);
        }
    }
    image::RgbaImage::from_raw(width, height, out).ok_or_else(|| "KWin's screenshot did not fit its size".into())
}

/// Spectacle, KDE's screenshot program, without its window or notification.
fn spectacle(shot: &Shot) -> Result<image::RgbaImage, String> {
    let file = std::env::temp_dir().join(format!("jai-shot-{}.png", std::process::id()));
    let what = match shot {
        Shot::ActiveWindow => "--activewindow",
        Shot::Screen(_) => "--current",
        _ => "--fullscreen",
    };
    let ran = std::process::Command::new("spectacle").args(["--background", "--nonotify", what, "--output"]).arg(&file).status();
    let picture = match ran {
        Ok(status) if status.success() => image::open(&file).map(|p| p.to_rgba8()).map_err(|e| format!("Spectacle's picture: {e}")),
        Ok(_) => Err("Spectacle could not take the picture".into()),
        Err(_) => Err("no screenshot service (KWin or Spectacle)".into()),
    };
    let _ = std::fs::remove_file(&file);
    picture
}

// ---------------------------------------------------------------------------- brightness ----

/// A screen PowerDevil dims: its bus name, label ("Built-in screen", the monitor's model),
/// whether it is the laptop's own, and its brightness out of its maximum.
#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    pub name: String,
    pub label: String,
    pub internal: bool,
    pub value: i32,
    pub max: i32,
}

const BRIGHTNESS: &str = "org.kde.ScreenBrightness";

/// The screens whose brightness KDE controls (the laptop's own and, over DDC/CI, monitors).
pub fn screens() -> Result<Vec<Screen>, String> {
    let names: Vec<String> = property(BRIGHTNESS, "/org/kde/ScreenBrightness", BRIGHTNESS, "DisplaysDBusNames")?;
    names
        .into_iter()
        .map(|name| {
            let path = format!("/org/kde/ScreenBrightness/{name}");
            let api = "org.kde.ScreenBrightness.Display";
            Ok(Screen {
                label: property(BRIGHTNESS, &path, api, "Label")?,
                internal: property(BRIGHTNESS, &path, api, "IsInternal")?,
                value: property(BRIGHTNESS, &path, api, "Brightness")?,
                max: property(BRIGHTNESS, &path, api, "MaxBrightness")?,
                name,
            })
        })
        .collect()
}

/// Sets one screen's brightness (0 ..= its maximum) without KDE's on-screen indicator.
pub fn set_brightness(name: &str, value: i32) -> Result<(), String> {
    call::<_, ()>(BRIGHTNESS, &format!("/org/kde/ScreenBrightness/{name}"), "org.kde.ScreenBrightness.Display", "SetBrightness", &(value, 1u32))
}

// ------------------------------------------------------------------------- the session ----

/// Whether the screen is locked (KDE's screen locker answers on the freedesktop name).
pub fn screen_locked() -> Option<bool> {
    call("org.freedesktop.ScreenSaver", "/ScreenSaver", "org.freedesktop.ScreenSaver", "GetActive", &()).ok()
}

/// A desktop notification from J.A.I. (KDE shows it in its corner and keeps it in the history).
pub fn notify(summary: &str, body: &str) -> Result<(), String> {
    let hints: HashMap<&str, Value> = HashMap::new();
    let none: Vec<&str> = Vec::new();
    call::<_, u32>("org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications", "Notify", &("J.A.I.", 0u32, "", summary, body, none, hints, -1i32)).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_coded_the_way_kde_codes_them() {
        assert_eq!(qt_key("Alt+J"), Some(0x0800_004A));
        assert_eq!(qt_key("Ctrl+Alt+K"), Some(0x0C00_004B));
        assert_eq!(qt_key("ctrl + shift + f9"), Some(0x0600_0000 | 0x0100_0038));
        assert_eq!(qt_key("Alt"), None, "a modifier alone is no key");
        assert_eq!(qt_key("Alt+Banana"), None);
    }

    #[test]
    fn kwins_raw_pixels_become_rgba_in_either_order() {
        // Two pixels a row, stride padded to 12 bytes: red then half-transparent green.
        let bgra = [0, 0, 255, 255, 0, 255, 0, 128, 9, 9, 9, 9];
        let picture = raw_image(&bgra, 2, 1, 12, 6).unwrap();
        assert_eq!(picture.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(picture.get_pixel(1, 0).0, [0, 255, 0, 128]);
        let rgbx = [255, 0, 0, 7, 0, 255, 0, 7];
        assert_eq!(raw_image(&rgbx, 2, 1, 8, 16).unwrap().get_pixel(0, 0).0, [255, 0, 0, 255], "RGBX is opaque");
        assert!(raw_image(&rgbx, 2, 2, 8, 16).is_err(), "too few bytes for two rows");
        assert!(raw_image(&rgbx, 2, 1, 8, 13).is_err(), "RGB888 is not read");
    }
}
