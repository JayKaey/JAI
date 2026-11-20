//! "What's on my screen?" — JAI reads the window Sir is looking at (or the whole screen) with
//! the OCR engine built into Windows (Windows.Media.Ocr, in Sir's own languages; on Linux
//! tesseract reads a KWin screenshot) and hands the text to the language model, which explains,
//! summarises or answers questions about it. Nothing leaves the machine.

#[cfg(windows)]
use std::time::Duration;
#[cfg(windows)]
use tracing::info;

#[cfg(windows)]
const OCR_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_CHARS: usize = 6000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The top application window (not JAI's own).
    ActiveWindow,
    /// The primary monitor.
    Screen,
}

impl Scope {
    pub fn parse(raw: &str) -> Self {
        let r = raw.to_lowercase();
        if r.contains("screen") || r.contains("monitor") || r.contains("bildschirm") || r.contains("desktop") {
            Self::Screen
        } else {
            Self::ActiveWindow
        }
    }
}

/// Standard base64 (for PowerShell's -EncodedCommand).
pub(crate) fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Capture `rect` (or the primary screen) and print the recognised lines.
#[cfg(windows)]
fn ocr_script(rect: Option<[i32; 4]>, png: &str) -> String {
    let bounds = match rect {
        Some([x, y, w, h]) => format!("$x={x}; $y={y}; $w={w}; $h={h}"),
        None => "$b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds; $x=$b.X; $y=$b.Y; $w=$b.Width; $h=$b.Height".to_string(),
    };
    let capture = format!(
        r#"{bounds}
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($x, $y, 0, 0, $bmp.Size)
$bmp.Save('{png}', [System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose()"#,
        png = png.replace('\'', "''"),
    );
    recognise_script(&capture, png)
}

/// Recognise the lines of the image at `png` after running `prepare` (which may write it).
#[cfg(windows)]
fn recognise_script(prepare: &str, png: &str) -> String {
    format!(
        r#"$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[System.Text.Encoding]::UTF8
Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public static class JDpi{{[DllImport("user32.dll")]public static extern bool SetProcessDPIAware();}}'
[JDpi]::SetProcessDPIAware() | Out-Null
Add-Type -AssemblyName System.Windows.Forms,System.Drawing,System.Runtime.WindowsRuntime
{prepare}
$asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {{ $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' }})[0]
function Await($op, $type) {{ $t = $asTask.MakeGenericMethod($type).Invoke($null, @($op)); $t.Wait(-1) | Out-Null; $t.Result }}
[void][Windows.Storage.StorageFile, Windows.Storage, ContentType = WindowsRuntime]
[void][Windows.Media.Ocr.OcrEngine, Windows.Foundation, ContentType = WindowsRuntime]
[void][Windows.Graphics.Imaging.BitmapDecoder, Windows.Graphics, ContentType = WindowsRuntime]
$file = Await ([Windows.Storage.StorageFile]::GetFileFromPathAsync('{png}')) ([Windows.Storage.StorageFile])
$stream = Await ($file.OpenAsync([Windows.Storage.FileAccessMode]::Read)) ([Windows.Storage.Streams.IRandomAccessStream])
$decoder = Await ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream)) ([Windows.Graphics.Imaging.BitmapDecoder])
$bitmap = Await ($decoder.GetSoftwareBitmapAsync()) ([Windows.Graphics.Imaging.SoftwareBitmap])
$engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages()
if ($engine -eq $null) {{ Write-Output 'JAI_NO_OCR'; exit 0 }}
$result = Await ($engine.RecognizeAsync($bitmap)) ([Windows.Media.Ocr.OcrResult])
$stream.Dispose()
$result.Lines | ForEach-Object {{ $_.Text }}
"#,
        png = png.replace('\'', "''"),
    )
}

/// Runs an OCR script hidden and without the busy cursor (`-EncodedCommand`, UTF-16LE).
#[cfg(windows)]
async fn powershell_encoded(script: &str) -> std::io::Result<crate::shell::HelperOutput> {
    let encoded: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let encoded = base64(&encoded);
    crate::shell::run_helper(
        std::path::Path::new("powershell"),
        ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encoded.as_str()],
        None,
        &[],
        OCR_TIMEOUT,
    )
    .await
}

/// OCR of an image file (a phone photo or scan: JPEG, PNG, BMP …), in Sir's own languages.
#[cfg(windows)]
pub async fn ocr_file(path: &std::path::Path) -> Result<String, String> {
    let owned = path.to_path_buf();
    match tokio::task::spawn_blocking(move || native::file(&owned)).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(text) => {
            info!("[SCREEN] OCR of {}: {} characters", path.display(), text.len());
            return Ok(text);
        }
        Err(why) if why == NO_OCR => return Err("Windows has no OCR language installed.".into()),
        Err(why) => info!("[SCREEN] Native OCR of {} failed ({why}): through PowerShell instead", path.display()),
    }
    let script = recognise_script("", &path.display().to_string());
    let out = powershell_encoded(&script).await.map_err(|e| if e.kind() == std::io::ErrorKind::TimedOut { "Reading the image timed out.".to_string() } else { e.to_string() })?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("Windows OCR failed: {}", err.lines().find(|l| !l.trim().is_empty()).unwrap_or("unknown error")));
    }
    if text == "JAI_NO_OCR" {
        return Err("Windows has no OCR language installed.".into());
    }
    info!("[SCREEN] OCR of {}: {} characters", path.display(), text.len());
    Ok(text)
}

#[cfg(not(windows))]
pub async fn ocr_file(path: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    crate::kde::blocking(move || tesseract(&bytes)).await?
}

/// Tesseract reads an encoded image from stdin; German too when its data is installed.
#[cfg(not(windows))]
fn tesseract(image: &[u8]) -> Result<String, String> {
    use std::io::Write;
    const HINT: &str = "Install it with: sudo pacman -S tesseract tesseract-data-eng tesseract-data-deu";
    for langs in ["eng+deu", "eng"] {
        let mut child = std::process::Command::new("tesseract")
            .args(["-", "-", "-l", langs])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| format!("tesseract is missing. {HINT}"))?;
        // A writer thread keeps a big image from deadlocking against the output pipe.
        let mut stdin = child.stdin.take().ok_or("tesseract has no input")?;
        let data = image.to_vec();
        let writer = std::thread::spawn(move || stdin.write_all(&data));
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let _ = writer.join();
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    Err(format!("tesseract could not read the image (language data missing?). {HINT}"))
}

/// OCR of a screenshot from KWin.
#[cfg(not(windows))]
pub fn ocr_shot(shot: &crate::kde::Shot) -> Result<String, String> {
    let image = crate::kde::screenshot(shot)?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    tesseract(&png.into_inner())
}

#[cfg(windows)]
pub async fn read(scope: Scope) -> Result<String, String> {
    let target = match scope {
        Scope::ActiveWindow => crate::win32::call(crate::win32::top_app_window).await?,
        Scope::Screen => None,
    };
    let (label, rect) = match (&target, scope) {
        (Some((title, rect)), _) => (format!("the window \"{}\"", title.chars().take(70).collect::<String>()), Some(*rect)),
        (None, Scope::ActiveWindow) => ("the screen (no application window was in front)".to_string(), None),
        (None, Scope::Screen) => ("the primary screen".to_string(), None),
    };
    ocr(rect, label).await
}

/// Windows' own OCR called from Rust (no PowerShell starting up): a GDI capture or a decoded
/// image handed to Windows.Media.Ocr as a bitmap. Blocking (WinRT's async is waited on).
#[cfg(windows)]
mod native {
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;

    fn err(e: windows::core::Error) -> String {
        e.message().to_string()
    }

    /// The largest side Windows' OCR takes (bigger pictures are scaled down to it).
    fn max_side() -> u32 {
        OcrEngine::MaxImageDimension().unwrap_or(2600).max(64)
    }

    /// The lines Windows reads in BGRA rows (top-down), in Sir's languages.
    pub(super) fn pixels(bgra: &[u8], width: u32, height: u32) -> Result<String, String> {
        let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| super::NO_OCR.to_string())?;
        let writer = DataWriter::new().map_err(err)?;
        writer.WriteBytes(bgra).map_err(err)?;
        let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(&writer.DetachBuffer().map_err(err)?, BitmapPixelFormat::Bgra8, width as i32, height as i32, BitmapAlphaMode::Ignore).map_err(err)?;
        let lines = engine.RecognizeAsync(&bitmap).map_err(err)?.get().map_err(err)?.Lines().map_err(err)?;
        Ok(lines.into_iter().filter_map(|line| line.Text().ok()).map(|text| text.to_string()).collect::<Vec<_>>().join("\n"))
    }

    /// `rect` (left, top, width, height in physical pixels; None: the primary screen).
    pub fn screen(rect: Option<[i32; 4]>) -> Result<String, String> {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        // SAFETY: plain metric queries.
        let [x, y, w, h] = rect.unwrap_or_else(|| unsafe { [0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)] });
        let side = max_side() as i64;
        let width = (w as i64).min(side).min(side * w as i64 / (h as i64).max(1)).max(2);
        let (bgra, width, height) = crate::instant_replay::screen_bgra([x, y, x + w, y + h], width as u32).ok_or("the screen could not be captured")?;
        pixels(&bgra, width, height)
    }

    /// An image file (PNG, JPEG, WebP).
    pub fn file(path: &std::path::Path) -> Result<String, String> {
        let image = image::open(path).map_err(|e| e.to_string())?;
        let side = max_side();
        let image = if image.width() > side || image.height() > side { image.resize(side, side, image::imageops::FilterType::Triangle) } else { image };
        let rgba = image.to_rgba8();
        let (width, height) = rgba.dimensions();
        let mut bgra = rgba.into_raw();
        bgra.chunks_exact_mut(4).for_each(|px| px.swap(0, 2));
        pixels(&bgra, width, height)
    }
}

#[cfg(windows)]
const NO_OCR: &str = "Windows has no OCR language installed (Settings → Time & language → Language → add the language's optional 'Optical character recognition').";

/// OCR of `rect` (left, top, width, height in physical pixels; None = primary screen).
#[cfg(windows)]
async fn ocr(rect: Option<[i32; 4]>, label: String) -> Result<String, String> {
    let started = std::time::Instant::now();
    // His own windows stay out of the picture while he looks (he reads what is behind them;
    // they stay on the monitor), and the HUD frosts over meanwhile.
    let looking = crate::screen_context::look().await;
    let text = match tokio::task::spawn_blocking(move || native::screen(rect)).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(text) => {
            drop(looking);
            text
        }
        Err(why) if why == NO_OCR => return Err(why),
        Err(why) => {
            info!("[SCREEN] Native OCR failed ({why}): through PowerShell instead");
            let png = std::env::temp_dir().join(format!("jai_ocr_{}.png", std::process::id()));
            let out = powershell_encoded(&ocr_script(rect, &png.display().to_string())).await.map_err(|e| if e.kind() == std::io::ErrorKind::TimedOut { "Reading the screen timed out.".to_string() } else { e.to_string() });
            drop(looking);
            let _ = std::fs::remove_file(&png);
            let out = out?;
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !out.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                return Err(format!("Windows OCR failed: {}", err.lines().find(|l| !l.trim().is_empty()).unwrap_or("unknown error")));
            }
            if text == "JAI_NO_OCR" {
                return Err(NO_OCR.into());
            }
            text
        }
    };
    info!("[SCREEN] OCR of {label}: {} characters in {} ms", text.len(), started.elapsed().as_millis());
    if text.is_empty() {
        return Ok(format!("No readable text was found on {label}."));
    }
    // The card under the emblem: what he saw, with its quick actions.
    crate::screen_context::seen(&label, &text);
    let count = text.chars().count();
    Ok(format!(
        "Text visible on {label} (OCR):\n{}{}",
        text.chars().take(MAX_CHARS).collect::<String>(),
        if count > MAX_CHARS { "\n…" } else { "" }
    ))
}

/// `inspect_screen_or_selection`: what Sir is looking at — his highlighted text (copied out of
/// the window he was working in, his clipboard restored afterwards), the clipboard, the active
/// window, or a whole monitor (OCR plus the titles of the windows on it).
pub async fn inspect(args: &serde_json::Value, foreground: Option<isize>) -> Result<String, String> {
    let mode = crate::tools::str_arg(args, &["mode", "scope", "target"]).unwrap_or("").to_lowercase();
    let monitor = args.get("target_monitor").or_else(|| args.get("monitor")).filter(|v| !v.is_null() && v.as_str() != Some(""));
    match mode.as_str() {
        "selection" | "selected" | "highlighted" | "selected_text" | "selection_text" => selection(foreground).await,
        "clipboard" | "copied" => crate::clipboard::tool(&serde_json::json!({ "action": "read" })).await,
        "copy_text" | "copy" => copy_text(monitor).await,
        "active_window" | "window" | "app" if monitor.is_none() => read(Scope::ActiveWindow).await,
        _ => match monitor {
            Some(m) => monitor_text(m).await,
            None if mode.contains("screen") || mode.contains("monitor") || mode == "all" => monitor_text(&serde_json::json!("primary")).await,
            None => read(Scope::ActiveWindow).await,
        },
    }
}

/// Copies Sir's highlighted text out of the window he was working in (Ctrl+C), then puts his
/// clipboard back the way it was.
#[cfg(windows)]
async fn selection(foreground: Option<isize>) -> Result<String, String> {
    let hwnd = match foreground.filter(|h| crate::win32::is_window(*h)) {
        Some(h) => h,
        None => crate::win32::call(|| crate::win32::user_window().map(|w| w.hwnd)).await?.ok_or("There is no window with a selection to read.")?,
    };
    let before = crate::clipboard::read().await.unwrap_or_default();
    // A marker makes "nothing was selected" detectable even when the selection equals the old
    // clipboard text.
    let marker = format!("\u{2063}jai-{}", crate::widgets::now_ms());
    crate::clipboard::write(&marker).await?;
    async fn read_back(marker: &str) -> String {
        let mut text = String::new();
        for _ in 0..6 {
            tokio::time::sleep(std::time::Duration::from_millis(70)).await;
            text = crate::clipboard::read().await.unwrap_or_default();
            if text != marker {
                break;
            }
        }
        text
    }
    // Ctrl+Insert first (it never interrupts a terminal); Ctrl+C for apps that ignore it, but
    // never into a console, where it would stop the running program.
    let mut copied = crate::win32::call(move || crate::win32::copy_selection_from(hwnd, false)).await?;
    let mut text = read_back(&marker).await;
    if copied && text == marker && !crate::win32::call(move || crate::win32::is_terminal_window(hwnd)).await? {
        copied = crate::win32::call(move || crate::win32::copy_selection_from(hwnd, true)).await?;
        text = read_back(&marker).await;
    }
    // Sir's clipboard as it was (text; an image on it cannot be restored).
    if !before.is_empty() || text == marker {
        let _ = crate::clipboard::write(&before).await;
    }
    if !copied {
        return Err("The window would not come to the front, so I could not copy the selection.".into());
    }
    if text == marker || text.trim().is_empty() {
        return Ok(format!(
            "Nothing was highlighted (Ctrl+C copied nothing).{}",
            if before.trim().is_empty() { String::new() } else { format!(" The clipboard holds:\n{}", before.chars().take(4000).collect::<String>()) }
        ));
    }
    let count = text.chars().count();
    info!("[SCREEN] Selection read: {count} characters");
    Ok(format!("Sir's highlighted text ({count} characters):\n{}{}", text.chars().take(MAX_CHARS).collect::<String>(), if count > MAX_CHARS { "\n…" } else { "" }))
}

#[cfg(not(windows))]
async fn selection(_foreground: Option<isize>) -> Result<String, String> {
    // X11/Wayland keep the selection in the PRIMARY buffer: no key press needed.
    tokio::task::spawn_blocking(|| {
        for (p, a) in [("wl-paste", &["--primary", "--no-newline"][..]), ("xclip", &["-selection", "primary", "-o"][..])] {
            if let Ok(out) = std::process::Command::new(p).args(a).output() {
                if out.status.success() {
                    return Ok(format!("Sir's highlighted text:\n{}", String::from_utf8_lossy(&out.stdout).chars().take(MAX_CHARS).collect::<String>()));
                }
            }
        }
        Err("No selection tool found (install wl-clipboard or xclip).".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// OCR of one whole monitor, with the titles of the windows on it.
#[cfg(windows)]
async fn monitor_text(target: &serde_json::Value) -> Result<String, String> {
    let monitors = crate::monitors::list();
    let index = crate::monitors::resolve(target, None, &monitors).ok_or_else(|| format!("There is no monitor {target}; {} are connected.", monitors.len()))?;
    let m = monitors[index - 1].clone();
    let bounds = m.rect;
    let titles: Vec<String> = crate::win32::call(move || {
        crate::win32::app_windows()
            .into_iter()
            .filter(|w| !crate::win32::is_jai_window(w) && !matches!(w.class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd"))
            .filter_map(|w| {
                let (r, _, minimised) = crate::win32::window_state(w.hwnd)?;
                let (cx, cy) = (r[0] + r[2] / 2, r[1] + r[3] / 2);
                (!minimised && cx >= bounds[0] && cx < bounds[2] && cy >= bounds[1] && cy < bounds[3]).then(|| format!("\"{}\" ({})", w.title.chars().take(70).collect::<String>(), w.process))
            })
            .take(12)
            .collect()
    })
    .await?;
    let label = format!("Monitor {index}{}", if m.primary { " (primary)" } else { "" });
    let text = ocr(Some([m.rect[0], m.rect[1], m.width(), m.height()]), label.clone()).await?;
    Ok(format!(
        "{label}, {}×{}. Windows on it (top first): {}.\n{text}",
        m.width(),
        m.height(),
        if titles.is_empty() { "none (desktop)".to_string() } else { titles.join(", ") }
    ))
}

#[cfg(not(windows))]
async fn monitor_text(target: &serde_json::Value) -> Result<String, String> {
    let outputs = crate::plasma::outputs();
    let number = target.as_u64().or_else(|| target.as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(1) as usize;
    let name = outputs.get(number.max(1) - 1).ok_or_else(|| format!("There is no monitor {target}; {} are connected.", outputs.len()))?.clone();
    let titles: Vec<String> = crate::plasma::windows().into_iter().filter(|w| !w.is(crate::plasma::state::MINIMIZED) && !w.title.is_empty()).take(12).map(|w| format!("\"{}\"", w.title.chars().take(70).collect::<String>())).collect();
    let text = crate::kde::blocking(move || ocr_shot(&crate::kde::Shot::Screen(name))).await??;
    Ok(format!("Monitor {number}. Open windows: {}.\n{text}", if titles.is_empty() { "none".to_string() } else { titles.join(", ") }))
}

/// "Copy the text of this window": what Windows reads in the window in front (or on a monitor)
/// put on the clipboard as plain text.
#[cfg(windows)]
async fn copy_text(monitor: Option<&serde_json::Value>) -> Result<String, String> {
    let (label, rect) = match monitor {
        Some(target) => {
            let monitors = crate::monitors::list();
            let index = crate::monitors::resolve(target, None, &monitors).ok_or_else(|| format!("There is no monitor {target}; {} are connected.", monitors.len()))?;
            let m = &monitors[index - 1];
            (format!("monitor {index}"), [m.rect[0], m.rect[1], m.width(), m.height()])
        }
        None => match crate::win32::call(crate::win32::top_app_window).await? {
            Some((title, rect)) => (format!("\"{}\"", title.chars().take(70).collect::<String>()), rect),
            None => return Err("No window of yours is in front.".into()),
        },
    };
    let looking = crate::screen_context::look().await;
    let text = tokio::task::spawn_blocking(move || native::screen(Some(rect))).await.unwrap_or_else(|e| Err(e.to_string()));
    drop(looking);
    let text = text?;
    if text.trim().is_empty() {
        return Ok(format!("I found no text to copy in {label}, Sir."));
    }
    crate::clipboard::write(&text).await?;
    info!("[SCREEN] Copied {} characters read in {label}", text.len());
    Ok(format!("Copied, Sir: {} lines from {label} are on the clipboard.", text.lines().count()))
}

#[cfg(not(windows))]
async fn copy_text(monitor: Option<&serde_json::Value>) -> Result<String, String> {
    let outputs = crate::plasma::outputs();
    let shot = match monitor {
        Some(target) => {
            let number = target.as_u64().or_else(|| target.as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(1) as usize;
            crate::kde::Shot::Screen(outputs.get(number.max(1) - 1).ok_or_else(|| format!("There is no monitor {target}; {} are connected.", outputs.len()))?.clone())
        }
        None => crate::kde::Shot::ActiveWindow,
    };
    let text = crate::kde::blocking(move || ocr_shot(&shot)).await??;
    if text.trim().is_empty() {
        return Ok("I found no text to copy, Sir.".into());
    }
    crate::clipboard::write(&text).await?;
    Ok(format!("Copied, Sir: {} lines are on the clipboard.", text.lines().count()))
}

pub fn inspect_tool_definition() -> serde_json::Value {
    serde_json::json!({"type": "function", "function": {
        "name": "inspect_screen_or_selection",
        "description": "See what Sir sees: mode selection (the text he highlighted — for 'explain/translate/refactor/debug the code I selected'), clipboard, active_window (the app in front: 'what does this error mean'), or monitor with target_monitor (1, 2, left, right: 'look at my second monitor', 'summarise what's on my screen'). Returns the text (OCR) and window titles; then answer from it. copy_text: the text read in the window in front (or target_monitor) onto the clipboard ('copy the text of this window').",
        "parameters": {"type": "object", "properties": {
            "mode": {"type": "string", "enum": ["selection", "clipboard", "active_window", "monitor", "copy_text"]},
            "target_monitor": {"type": "string", "description": "1, 2, 3, left, right, primary"}
        }, "required": ["mode"]}
    }})
}

#[cfg(not(windows))]
pub async fn read(scope: Scope) -> Result<String, String> {
    let (shot, label) = match scope {
        Scope::ActiveWindow => (crate::kde::Shot::ActiveWindow, "the window in front"),
        Scope::Screen => (crate::plasma::outputs().into_iter().next().map_or(crate::kde::Shot::Workspace, crate::kde::Shot::Screen), "the screen"),
    };
    let looking = crate::screen_context::look().await;
    let text = crate::kde::blocking(move || ocr_shot(&shot)).await?;
    drop(looking);
    let text = text?;
    crate::screen_context::seen(label, &text);
    Ok(format!("Text visible on {label} (OCR):\n{}", text.chars().take(MAX_CHARS).collect::<String>()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows reads text from Rust alone: black words drawn by GDI on white, handed over as
    /// BGRA rows. (Skipped where Windows has no OCR language.)
    #[cfg(windows)]
    #[test]
    fn windows_reads_text_without_powershell() {
        use windows::core::w;
        use windows::Win32::Graphics::Gdi::*;
        let (w, h) = (900i32, 160i32);
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let info = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() }, ..Default::default() };
        // SAFETY: a memory DC with a DIB section of w×h×4 bytes; everything is released below.
        let bgra = unsafe {
            let dc = CreateCompatibleDC(None);
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(dc, bitmap);
            std::slice::from_raw_parts_mut(bits.cast::<u8>(), (w * h * 4) as usize).fill(255);
            let font = CreateFontW(72, 0, 0, 0, 400, 0, 0, 0, 0, 0, 0, 0, 0, w!("Arial"));
            let old_font = SelectObject(dc, font);
            SetBkMode(dc, TRANSPARENT);
            let _ = TextOutW(dc, 30, 40, &"HELLO WORLD 2026".encode_utf16().collect::<Vec<_>>());
            let _ = GdiFlush();
            let copy = std::slice::from_raw_parts(bits.cast::<u8>(), (w * h * 4) as usize).to_vec();
            SelectObject(dc, old_font);
            SelectObject(dc, old);
            let _ = DeleteObject(font);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(dc);
            copy
        };
        match native::pixels(&bgra, w as u32, h as u32) {
            Err(why) if why == NO_OCR => eprintln!("skipped: no OCR language on this PC"),
            read => assert_eq!(read.map(|t| t.to_uppercase()).as_deref(), Ok("HELLO WORLD 2026")),
        }
    }

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(base64(b""), "");
        assert_eq!(Scope::parse("whole screen"), Scope::Screen);
        assert_eq!(Scope::parse("active_window"), Scope::ActiveWindow);
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "captures the real screen"]
    async fn reads_the_screen() {
        let text = read(Scope::Screen).await.unwrap();
        println!("OCR returned {} characters", text.chars().count());
        assert!(text.starts_with("Text visible") || text.starts_with("No readable"));
    }
}
