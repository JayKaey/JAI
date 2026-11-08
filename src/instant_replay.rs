//! Instant Replay: "Jarvis, clip that." A native rolling screen recorder keeps the last minute of
//! the primary monitor in memory — 5 frames a second by default (light on the machine), scaled
//! to 1280 px with GDI's halftone StretchBlt and JPEG-compressed through the Windows Imaging
//! Component on a low-priority thread — and the conversation of the same time.
//!
//! The capture is a plain `SRCCOPY` of the composed desktop: never `CAPTUREBLT` (which makes
//! Windows hide and redraw the hardware mouse cursor on every frame — a visible cursor flicker)
//! and never the cursor icon itself. Saving writes the frames
//! to `Videos\Jarvis_Clips\<time>\` as an MP4 (ffmpeg fed through an image pipe) or, without
//! ffmpeg, as a Motion-JPEG AVI, checks that a real video came out, and adds the transcript.

use crate::config::ConfigStore;
use crate::features::{num_arg, spoken, user_folder};
use crate::tools::ToolContext;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// What was said lately (time, who, text), for the clip's transcript.
static TRANSCRIPT: Mutex<VecDeque<(chrono::DateTime<chrono::Local>, String, String)>> = Mutex::new(VecDeque::new());

/// A saved video smaller than this is not a clip (a frame or two, or a failed encode).
const MIN_VIDEO_BYTES: u64 = 100 * 1024;
/// JPEG quality of the buffered frames (WIC ImageQuality, 0-1).
const JPEG_QUALITY: f32 = 0.6;

/// Notes a line of the conversation (Sir's words, Jarvis' replies).
pub fn note(who: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let mut t = TRANSCRIPT.lock().unwrap_or_else(|e| e.into_inner());
    t.push_back((chrono::Local::now(), who.to_string(), text.chars().take(2000).collect()));
    let cutoff = chrono::Local::now() - chrono::Duration::minutes(15);
    while t.front().is_some_and(|(at, _, _)| *at < cutoff) || t.len() > 200 {
        t.pop_front();
    }
}

/// One buffered screen frame.
struct Frame {
    at: Instant,
    jpeg: Vec<u8>,
}

/// The rolling in-memory buffer and its recorder thread.
#[derive(Default)]
struct Recorder {
    frames: Mutex<VecDeque<Frame>>,
    bytes: AtomicU64,
    running: AtomicBool,
    /// Frames the recorder could not capture or encode (logged once in a while).
    failures: AtomicU64,
}

impl Recorder {
    fn push(&self, frame: Frame, keep: Duration) {
        let mut frames = self.frames.lock().unwrap_or_else(|e| e.into_inner());
        self.bytes.fetch_add(frame.jpeg.len() as u64, Ordering::Relaxed);
        let now = frame.at;
        frames.push_back(frame);
        while frames.front().is_some_and(|f| now.duration_since(f.at) > keep) {
            if let Some(old) = frames.pop_front() {
                self.bytes.fetch_sub(old.jpeg.len() as u64, Ordering::Relaxed);
            }
        }
    }

    /// The frames of the last `span` (oldest first) and how many seconds they cover.
    fn recent(&self, span: Duration) -> (Vec<Vec<u8>>, f32) {
        let frames = self.frames.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let picked: Vec<&Frame> = frames.iter().filter(|f| now.duration_since(f.at) <= span).collect();
        let covered = match (picked.first(), picked.last()) {
            (Some(a), Some(b)) => b.at.duration_since(a.at).as_secs_f32(),
            _ => 0.0,
        };
        (picked.iter().map(|f| f.jpeg.clone()).collect(), covered)
    }
}

#[derive(Clone, Default)]
pub struct Replay {
    recorder: Arc<Recorder>,
}

impl Replay {
    /// Starts the rolling recorder (when enabled in the settings).
    pub fn start(&self, config: ConfigStore) {
        let settings = config.read(|c| c.features.clone());
        if !settings.replay_enabled || !cfg!(windows) {
            info!("[REPLAY] Rolling recorder off.");
            return;
        }
        // A test never records Sir's screen (nor spends his game's frame time on it).
        if crate::platform::hands_off() {
            info!("[REPLAY] Rolling recorder off: a test session never records Sir's screen.");
            return;
        }
        if self.recorder.running.swap(true, Ordering::AcqRel) {
            return;
        }
        // At most 5 frames a second in the background: plenty for a replay, light on the GPU.
        let fps = settings.replay_fps.clamp(1.0, 5.0);
        let keep = Duration::from_secs(settings.replay_seconds.clamp(10, 300) as u64 + 2);
        let width = settings.replay_width.clamp(320, 1920);
        let recorder = Arc::clone(&self.recorder);
        let spawned = std::thread::Builder::new().name("jarvis-replay".into()).spawn(move || {
            #[cfg(windows)]
            capture::run(&recorder, fps, width, keep);
            recorder.running.store(false, Ordering::Release);
        });
        match spawned {
            Ok(_) => info!("[REPLAY] Rolling recorder: {fps} fps, {width} px wide, {} s in memory", keep.as_secs() - 2),
            Err(e) => {
                self.recorder.running.store(false, Ordering::Release);
                warn!("[REPLAY] The recorder thread could not start: {e}");
            }
        }
    }

    pub fn is_running(&self) -> bool {
        self.recorder.running.load(Ordering::Acquire)
    }

    /// `save_instant_replay`: the last `seconds` of screen and conversation.
    pub async fn save(&self, tools: &ToolContext, args: &Value) -> Result<String, String> {
        let settings = tools.config.read(|c| c.features.clone());
        let seconds = num_arg(args, &["seconds", "duration", "last_seconds"]).unwrap_or(settings.replay_seconds as f64).clamp(5.0, 300.0);
        let (frames, covered) = self.recorder.recent(Duration::from_secs_f64(seconds));
        let transcript: Vec<String> = {
            let t = TRANSCRIPT.lock().unwrap_or_else(|e| e.into_inner());
            let from = chrono::Local::now() - chrono::Duration::seconds(seconds as i64 + 30);
            t.iter().filter(|(at, _, _)| *at >= from).map(|(at, who, text)| format!("[{}] {who}: {text}", at.format("%H:%M:%S"))).collect()
        };
        if frames.len() < 2 {
            return Err(if !settings.replay_enabled {
                "Instant Replay is switched off (features.replay_enabled in jarvis_config.json).".into()
            } else if self.is_running() {
                "The replay buffer is still filling, Sir; give it a few seconds.".into()
            } else {
                "The screen recorder is not running, so there is nothing to clip yet.".into()
            });
        }
        // The real capture rate (a busy machine drops frames): the video plays in real time.
        let fps = if covered > 0.5 { ((frames.len() - 1) as f32 / covered).clamp(1.0, 30.0) } else { settings.replay_fps.clamp(1.0, 5.0) };
        let folder = user_folder("Videos").join("Jarvis_Clips").join(crate::features::stamp());
        std::fs::create_dir_all(&folder).map_err(|e| format!("{} could not be created: {e}", folder.display()))?;
        let mut notes = Vec::new();
        let mut video: Option<PathBuf> = None;
        if let Some(ffmpeg) = crate::features::find_tool("ffmpeg") {
            let mp4 = folder.join("clip.mp4");
            match encode_mp4(&ffmpeg, &frames, fps, &mp4).await {
                Ok(()) if file_size(&mp4) >= MIN_VIDEO_BYTES => video = Some(mp4),
                Ok(()) => notes.push(format!("ffmpeg wrote only {} KB", file_size(&mp4) / 1024)),
                Err(e) => notes.push(format!("ffmpeg failed: {e}")),
            }
        }
        if video.is_none() {
            let avi = folder.join("clip.avi");
            let bytes = mjpeg_avi(&frames, fps).ok_or("The buffered frames could not be read as JPEG.")?;
            std::fs::write(&avi, bytes).map_err(|e| format!("{} could not be written: {e}", avi.display()))?;
            if file_size(&avi) >= MIN_VIDEO_BYTES {
                video = Some(avi);
            } else {
                notes.push(format!("the AVI is only {} KB", file_size(&avi) / 1024));
            }
        }
        if !transcript.is_empty() {
            let _ = std::fs::write(folder.join("transcript.txt"), transcript.join("\n"));
        }
        let Some(video) = video else {
            return Err(format!("The clip came out empty ({}). Nothing usable was saved.", notes.join("; ")));
        };
        let size_mb = file_size(&video) as f64 / 1_048_576.0;
        info!("[REPLAY] Saved {} frames ({covered:.1} s at {fps:.1} fps, {size_mb:.1} MB) to {}", frames.len(), video.display());
        tools.stark.pulse(&["brain", "replay", "vision"], false);
        let display = format!(
            "Instant Replay saved: the last {:.0} s ({} frames at {fps:.1} fps, {size_mb:.1} MB{}).\nVideo: {}\nFolder: {}",
            covered.max(1.0),
            frames.len(),
            if transcript.is_empty() { String::new() } else { ", with transcript".into() },
            video.display(),
            folder.display(),
        );
        Ok(spoken(display, format!("Clipped, Sir. The last {:.0} seconds are saved; the video is in your chat.", covered.max(1.0).round())))
    }
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// ffmpeg reads the JPEG frames from a pipe (`image2pipe`) and writes an H.264 MP4.
async fn encode_mp4(ffmpeg: &Path, frames: &[Vec<u8>], fps: f32, out: &Path) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let rate = format!("{fps:.3}");
    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.args(["-y", "-loglevel", "error", "-f", "image2pipe", "-c:v", "mjpeg", "-framerate", &rate, "-i", "-"])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p", "-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2", "-movflags", "+faststart"])
        .arg(out)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    crate::shell::hide_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("no pipe to ffmpeg")?;
    let data: Vec<u8> = frames.concat();
    let write = async move {
        stdin.write_all(&data).await?;
        stdin.shutdown().await
    };
    let fed = tokio::time::timeout(Duration::from_secs(120), write).await;
    let done = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output()).await.map_err(|_| "ffmpeg timed out".to_string())?.map_err(|e| e.to_string())?;
    match fed {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("the pipe broke: {e}")),
        Err(_) => return Err("feeding ffmpeg timed out".into()),
    }
    if done.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&done.stderr).lines().next().unwrap_or("unknown error").to_string())
    }
}

/// GDI capture of the primary monitor and WIC JPEG encoding, on the recorder's own thread.
#[cfg(windows)]
mod capture {
    use super::{Frame, Recorder, JPEG_QUALITY};
    use std::time::{Duration, Instant};
    use tracing::{info, warn};
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{BOOL, HGLOBAL, HWND};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject, SetStretchBltMode, StretchBlt,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HALFTONE, HBITMAP, HDC, SRCCOPY,
    };
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR, IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache,
    };
    use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream, IPropertyBag2, PROPBAG2};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, STREAM_SEEK_CUR};
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};

    /// GDI objects of one capture size, reused frame after frame.
    struct Surface {
        screen: HDC,
        memory: HDC,
        bitmap: HBITMAP,
        old: windows::Win32::Graphics::Gdi::HGDIOBJ,
        source: [i32; 4],
        size: (i32, i32),
    }

    impl Surface {
        fn new(source: [i32; 4], width: u32) -> Option<Self> {
            let (sw, sh) = (source[2] - source[0], source[3] - source[1]);
            if sw <= 0 || sh <= 0 {
                return None;
            }
            let w = (width as i32).min(sw) & !1;
            let h = ((sh as i64 * w as i64 / sw as i64) as i32) & !1;
            // SAFETY: screen DC of the desktop, a compatible memory DC and bitmap; released in Drop.
            unsafe {
                let screen = GetDC(HWND(0));
                if screen.is_invalid() {
                    return None;
                }
                let memory = CreateCompatibleDC(screen);
                let bitmap = CreateCompatibleBitmap(screen, w, h);
                if memory.is_invalid() || bitmap.is_invalid() {
                    let _ = ReleaseDC(HWND(0), screen);
                    return None;
                }
                let old = SelectObject(memory, bitmap);
                SetStretchBltMode(memory, HALFTONE);
                Some(Self { screen, memory, bitmap, old, source, size: (w, h) })
            }
        }

        /// The current screen as 24-bit BGR rows, top-down.
        fn grab(&self) -> Option<Vec<u8>> {
            let (w, h) = self.size;
            let [l, t, r, b] = self.source;
            // SAFETY: valid DCs and bitmap owned by `self`; the buffer holds w*h*4 bytes.
            unsafe {
                // SRCCOPY only: with desktop composition the screen DC already holds layered
                // windows, and CAPTUREBLT would hide and redraw the mouse cursor every frame.
                if !StretchBlt(self.memory, 0, 0, w, h, self.screen, l, t, r - l, b - t, SRCCOPY).as_bool() {
                    return None;
                }
                let mut info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        biHeight: -h,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut bgra = vec![0u8; (w * h * 4) as usize];
                let lines = GetDIBits(self.memory, self.bitmap, 0, h as u32, Some(bgra.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
                if lines != h {
                    return None;
                }
                Some(bgra.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]).collect())
            }
        }
    }

    impl Drop for Surface {
        fn drop(&mut self) {
            // SAFETY: the objects were created in `new` and are released exactly once.
            unsafe {
                SelectObject(self.memory, self.old);
                let _ = DeleteObject(self.bitmap);
                let _ = DeleteDC(self.memory);
                let _ = ReleaseDC(HWND(0), self.screen);
            }
        }
    }

    /// A JPEG of `bgr` (24-bit rows, top-down) through WIC.
    fn encode(factory: &IWICImagingFactory, bgr: &[u8], (w, h): (i32, i32)) -> windows::core::Result<Vec<u8>> {
        // SAFETY: COM is initialised on this thread; every interface comes from WIC itself and the
        // HGLOBAL is read only while locked.
        unsafe {
            let stream = CreateStreamOnHGlobal(HGLOBAL::default(), BOOL::from(true))?;
            let encoder = factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut frame: Option<IWICBitmapFrameEncode> = None;
            let mut options: Option<IPropertyBag2> = None;
            encoder.CreateNewFrame(&mut frame, &mut options)?;
            let frame = frame.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
            if let Some(options) = &options {
                let mut name: Vec<u16> = "ImageQuality".encode_utf16().chain(std::iter::once(0)).collect();
                let bag = PROPBAG2 { pstrName: PWSTR(name.as_mut_ptr()), ..Default::default() };
                let quality = windows::core::VARIANT::from(JPEG_QUALITY);
                let _ = options.Write(1, &bag, &quality);
            }
            frame.Initialize(options.as_ref())?;
            frame.SetSize(w as u32, h as u32)?;
            let mut format = GUID_WICPixelFormat24bppBGR;
            frame.SetPixelFormat(&mut format)?;
            if format != GUID_WICPixelFormat24bppBGR {
                return Err(windows::core::Error::from(windows::Win32::Foundation::E_FAIL));
            }
            frame.WritePixels(h as u32, (w * 3) as u32, bgr)?;
            frame.Commit()?;
            encoder.Commit()?;
            let mut written = 0u64;
            stream.Seek(0, STREAM_SEEK_CUR, Some(&mut written as *mut u64))?;
            let global = GetHGlobalFromStream(&stream)?;
            let ptr = GlobalLock(global) as *const u8;
            if ptr.is_null() {
                return Err(windows::core::Error::from_win32());
            }
            let bytes = std::slice::from_raw_parts(ptr, written as usize).to_vec();
            let _ = GlobalUnlock(global);
            Ok(bytes)
        }
    }

    /// The recorder loop: capture, encode, keep — at `fps`, at below-normal priority.
    pub(super) fn run(recorder: &Recorder, fps: f32, width: u32, keep: Duration) {
        // SAFETY: called once at the start of this thread.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }
        // SAFETY: WIC factory on this COM-initialised thread.
        let factory: IWICImagingFactory = match unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) } {
            Ok(f) => f,
            Err(e) => {
                warn!("[REPLAY] The Windows Imaging Component is unavailable: {}", e.message());
                return;
            }
        };
        let interval = Duration::from_secs_f32(1.0 / fps);
        let mut surface: Option<Surface> = None;
        let mut checked = Instant::now() - Duration::from_secs(10);
        let mut logged = Instant::now();
        loop {
            let started = Instant::now();
            // The primary monitor (re-read every few seconds: resolution or layout changes).
            if surface.is_none() || checked.elapsed() > Duration::from_secs(5) {
                checked = Instant::now();
                let primary = crate::win32::displays().into_iter().find(|d| d.primary).map(|d| d.rect);
                if let Some(rect) = primary {
                    if surface.as_ref().is_none_or(|s| s.source != rect) {
                        surface = Surface::new(rect, width);
                        if let Some(s) = &surface {
                            info!("[REPLAY] Capturing {}×{} → {}×{}", rect[2] - rect[0], rect[3] - rect[1], s.size.0, s.size.1);
                        }
                    }
                }
            }
            let frame = surface.as_ref().and_then(|s| s.grab().map(|bgr| (bgr, s.size)));
            match frame.map(|(bgr, size)| encode(&factory, &bgr, size)) {
                Some(Ok(jpeg)) => recorder.push(Frame { at: Instant::now(), jpeg }, keep),
                Some(Err(e)) => {
                    recorder.failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if logged.elapsed() > Duration::from_secs(60) {
                        logged = Instant::now();
                        warn!("[REPLAY] Frame encoding failed: {}", e.message());
                    }
                }
                None => {
                    recorder.failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    // A locked workstation or a secure desktop: try again in a moment.
                    surface = None;
                }
            }
            if logged.elapsed() > Duration::from_secs(300) {
                logged = Instant::now();
                let frames = recorder.frames.lock().unwrap_or_else(|e| e.into_inner()).len();
                info!(
                    "[REPLAY] Buffer: {frames} frames, {:.1} MB, {} failures",
                    recorder.bytes.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1_048_576.0,
                    recorder.failures.load(std::sync::atomic::Ordering::Relaxed)
                );
            }
            let spent = started.elapsed();
            std::thread::sleep(interval.saturating_sub(spent).max(Duration::from_millis(5)));
        }
    }
}

/// Width and height of a baseline/progressive JPEG (its SOF marker).
fn jpeg_size(data: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 9 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

/// A Motion-JPEG AVI (RIFF, one video stream, an idx1 index) of `frames` at `fps`.
pub fn mjpeg_avi(frames: &[Vec<u8>], fps: f32) -> Option<Vec<u8>> {
    let (w, h) = jpeg_size(frames.first()?)?;
    let le = |v: u32| v.to_le_bytes();
    let chunk = |id: &[u8; 4], body: &[u8]| -> Vec<u8> {
        let mut c = Vec::with_capacity(body.len() + 9);
        c.extend_from_slice(id);
        c.extend_from_slice(&le(body.len() as u32));
        c.extend_from_slice(body);
        if body.len() % 2 == 1 {
            c.push(0);
        }
        c
    };
    let list = |kind: &[u8; 4], body: &[u8]| -> Vec<u8> {
        let mut inner = kind.to_vec();
        inner.extend_from_slice(body);
        chunk(b"LIST", &inner)
    };
    let n = frames.len() as u32;
    let biggest = frames.iter().map(Vec::len).max().unwrap_or(0) as u32;
    let rate = (fps * 1000.0).round() as u32;
    let micro = (1_000_000.0 / fps) as u32;
    let mut avih = Vec::new();
    for v in [micro, biggest.saturating_mul(fps.ceil() as u32), 0, 0x10, n, 0, 1, biggest, w, h, 0, 0, 0, 0] {
        avih.extend_from_slice(&le(v));
    }
    let mut strh = Vec::new();
    strh.extend_from_slice(b"vidsMJPG");
    for v in [0u32, 0, 0, 1000, rate, 0, n, biggest, u32::MAX, 0] {
        strh.extend_from_slice(&le(v));
    }
    for v in [0u16, 0, w as u16, h as u16] {
        strh.extend_from_slice(&v.to_le_bytes());
    }
    let mut strf = Vec::new();
    strf.extend_from_slice(&le(40));
    strf.extend_from_slice(&le(w));
    strf.extend_from_slice(&le(h));
    strf.extend_from_slice(&1u16.to_le_bytes());
    strf.extend_from_slice(&24u16.to_le_bytes());
    strf.extend_from_slice(b"MJPG");
    for v in [w * h * 3, 0, 0, 0, 0] {
        strf.extend_from_slice(&le(v));
    }
    let strl = list(b"strl", &[chunk(b"strh", &strh), chunk(b"strf", &strf)].concat());
    let hdrl = list(b"hdrl", &[chunk(b"avih", &avih), strl].concat());
    let mut movi = Vec::new();
    let mut idx = Vec::new();
    for f in frames {
        // Offsets count from the 'movi' fourcc.
        let offset = 4 + movi.len() as u32;
        movi.extend_from_slice(&chunk(b"00dc", f));
        idx.extend_from_slice(b"00dc");
        idx.extend_from_slice(&le(0x10));
        idx.extend_from_slice(&le(offset));
        idx.extend_from_slice(&le(f.len() as u32));
    }
    let body = [b"AVI ".to_vec(), hdrl, list(b"movi", &movi), chunk(b"idx1", &idx)].concat();
    Some(chunk(b"RIFF", &body))
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "save_instant_replay",
        "description": "Instant Replay: save the last minute (or `seconds`) of the screen as a real video (MP4, or AVI without ffmpeg) plus the conversation's transcript in Videos\\Jarvis_Clips. Use for 'clip that', 'clip the last 30 seconds', 'save that', 'instant replay'.",
        "parameters": {"type": "object", "properties": {
            "seconds": {"type": "integer", "description": "How far back (default: the whole buffer, 60 s)"}
        }}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    #[ignore = "records Sir's real screen: run by hand while he is away"]
    fn a_few_seconds_make_a_real_video() {
        let r = Arc::new(Recorder::default());
        let rr = Arc::clone(&r);
        std::thread::spawn(move || capture::run(&rr, 5.0, 1280, Duration::from_secs(10)));
        std::thread::sleep(Duration::from_secs(4));
        let (frames, covered) = r.recent(Duration::from_secs(60));
        // A machine without a desktop session has nothing to record.
        if frames.len() < 8 {
            return;
        }
        let fps = (frames.len() - 1) as f32 / covered.max(0.1);
        // 5 fps alone; the whole test suite running in parallel can slow it.
        assert!(fps > 2.0 && fps < 6.5, "the light background rate: {fps:.1} fps");
        let avi = mjpeg_avi(&frames, fps).unwrap();
        assert!(avi.len() as u64 > MIN_VIDEO_BYTES, "{} bytes", avi.len());
    }

    #[test]
    fn mjpeg_avi_is_a_well_formed_riff() {
        // A minimal JPEG header: SOI, SOF0 (8-bit, 4×2 pixels), EOI.
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x02, 0x00, 0x04, 0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01, 0xFF, 0xD9];
        assert_eq!(jpeg_size(&jpeg), Some((4, 2)));
        let avi = mjpeg_avi(&[jpeg.clone(), jpeg.clone(), jpeg], 2.0).unwrap();
        assert_eq!(&avi[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(avi[4..8].try_into().unwrap()) as usize, avi.len() - 8);
        assert_eq!(&avi[8..12], b"AVI ");
        let movi = avi.windows(4).position(|w| w == b"movi").unwrap();
        // The first index entry points at the first frame chunk.
        let idx = avi.windows(4).position(|w| w == b"idx1").unwrap();
        let first = u32::from_le_bytes(avi[idx + 16..idx + 20].try_into().unwrap()) as usize;
        assert_eq!(&avi[movi + first..movi + first + 4], b"00dc");
        note("Sir", "clip that");
        assert!(TRANSCRIPT.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(_, w, t)| w == "Sir" && t == "clip that"));
    }

    #[test]
    fn the_ring_keeps_only_the_last_minute() {
        let r = Recorder::default();
        let start = Instant::now();
        for i in 0..10u64 {
            r.push(Frame { at: start + Duration::from_secs(i * 10), jpeg: vec![0; 100] }, Duration::from_secs(60));
        }
        let frames = r.frames.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(frames.len(), 7, "70 s … 90 s back are gone");
        assert_eq!(r.bytes.load(Ordering::Relaxed), 700);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "records Sir's real screen: run by hand while he is away"]
    fn a_real_frame_is_captured_and_encoded() {
        let r = Arc::new(Recorder::default());
        let rr = Arc::clone(&r);
        std::thread::spawn(move || capture::run(&rr, 10.0, 640, Duration::from_secs(5)));
        let deadline = Instant::now() + Duration::from_secs(10);
        while r.frames.lock().unwrap_or_else(|e| e.into_inner()).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        let frames = r.frames.lock().unwrap_or_else(|e| e.into_inner());
        // A machine without a desktop (CI service) has nothing to capture.
        if let Some(f) = frames.front() {
            assert_eq!(&f.jpeg[..2], &[0xFF, 0xD8], "a JPEG");
            let (w, h) = jpeg_size(&f.jpeg).expect("a JPEG frame header");
            assert!(w <= 640 && h > 0);
        }
    }
}
