//! The HUD's CAMERA block: the PC's webcam through Media Foundation's source reader (RGB32 with
//! the reader's own video processing), on a capture thread of its own. It only runs while Sir
//! has the block open: `[ CAMERA ]` starts it, a second click (or closing the block) stops it
//! and releases the device.
//!
//! `CAMERA SOURCE: [ DEFAULT CAMERA v ]` lists the capture devices (`devices`).

use eframe::egui::ColorImage;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

struct Feed {
    frame: Option<Arc<ColorImage>>,
    status: String,
    devices: Vec<String>,
}

static FEED: LazyLock<Mutex<Feed>> = LazyLock::new(|| Mutex::new(Feed { frame: None, status: "OFF".into(), devices: Vec::new() }));
static RUNNING: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(0);

fn feed() -> std::sync::MutexGuard<'static, Feed> {
    FEED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Capture devices by friendly name (enumerated in the background; empty until then).
pub fn devices() -> Vec<String> {
    feed().devices.clone()
}

/// Enumerates the capture devices on a helper thread.
pub fn refresh_devices() {
    std::thread::spawn(|| {
        let list = platform::enumerate();
        feed().devices = list;
        crate::gui::refresh();
    });
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

/// "LIVE", "STARTING", "NO CAMERA", "OFF" …
pub fn status() -> String {
    feed().status.clone()
}

/// The newest frame and its sequence number.
pub fn frame() -> Option<(u64, Arc<ColorImage>)> {
    feed().frame.clone().map(|f| (SEQ.load(Ordering::Acquire), f))
}

/// Starts capturing from device `index` (0 = the default camera).
pub fn start(index: usize) {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    feed().status = "STARTING".into();
    std::thread::Builder::new()
        .name("jarvis-camera".into())
        .spawn(move || {
            let result = platform::capture(index, &|image| {
                SEQ.fetch_add(1, Ordering::AcqRel);
                let mut f = feed();
                f.frame = Some(Arc::new(image));
                f.status = "LIVE".into();
                drop(f);
                crate::gui::refresh();
                RUNNING.load(Ordering::Acquire)
            });
            let mut f = feed();
            f.status = match result {
                Ok(()) => "OFF".into(),
                Err(e) => {
                    tracing::warn!("[CAMERA] {e}");
                    e
                }
            };
            f.frame = None;
            drop(f);
            RUNNING.store(false, Ordering::Release);
            crate::gui::refresh();
        })
        .ok();
}

/// Stops the capture (the thread releases the device after its current frame).
pub fn stop() {
    RUNNING.store(false, Ordering::Release);
}

/// BGRA / BGRX rows → an RGBA image, every second pixel when wider than 960 px.
pub fn bgra_to_image(data: &[u8], width: usize, height: usize, pitch: isize) -> Option<ColorImage> {
    if width == 0 || height == 0 {
        return None;
    }
    let step = if width > 960 { 2 } else { 1 };
    let (w, h) = (width / step, height / step);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = (y * step) as isize * pitch;
        let start = if pitch < 0 { (data.len() as isize + row + pitch) as usize } else { row as usize };
        for x in 0..w {
            let i = start + x * step * 4;
            let px = data.get(i..i + 4)?;
            rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
    }
    Some(ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}

#[cfg(windows)]
mod platform {
    use super::bgra_to_image;
    use eframe::egui::ColorImage;
    use windows::core::{Interface, PWSTR};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED};

    struct Session;

    impl Session {
        fn open() -> Result<Self, String> {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| format!("Media Foundation unavailable: {e}"))?;
            }
            Ok(Self)
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            unsafe {
                let _ = MFShutdown();
                CoUninitialize();
            }
        }
    }

    fn activates() -> Result<Vec<IMFActivate>, String> {
        unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 1).map_err(|e| e.to_string())?;
            let attrs = attrs.ok_or("no attributes")?;
            attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID).map_err(|e| e.to_string())?;
            let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFEnumDeviceSources(&attrs, &mut list, &mut count).map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            if !list.is_null() {
                for i in 0..count as usize {
                    if let Some(a) = (*list.add(i)).take() {
                        out.push(a);
                    }
                }
                CoTaskMemFree(Some(list as *const _));
            }
            Ok(out)
        }
    }

    fn name_of(a: &IMFActivate) -> String {
        unsafe {
            let mut name = PWSTR::null();
            let mut len = 0u32;
            if a.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut name, &mut len).is_err() || name.is_null() {
                return "Camera".into();
            }
            let text = name.to_string().unwrap_or_else(|_| "Camera".into());
            CoTaskMemFree(Some(name.0 as *const _));
            text
        }
    }

    pub fn enumerate() -> Vec<String> {
        let Ok(_session) = Session::open() else { return Vec::new() };
        activates().map(|list| list.iter().map(name_of).collect()).unwrap_or_default()
    }

    /// Reads frames until `on_frame` returns false.
    pub fn capture(index: usize, on_frame: &dyn Fn(ColorImage) -> bool) -> Result<(), String> {
        let _session = Session::open()?;
        let list = activates()?;
        let activate = list.get(index).or(list.first()).ok_or("NO CAMERA")?;
        unsafe {
            let source: IMFMediaSource = activate.ActivateObject().map_err(|e| format!("CAMERA BUSY ({e})"))?;
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 1).map_err(|e| e.to_string())?;
            let attrs = attrs.ok_or("no attributes")?;
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1).map_err(|e| e.to_string())?;
            let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs).map_err(|e| e.to_string())?;
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let wanted = MFCreateMediaType().map_err(|e| e.to_string())?;
            wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
            wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(|e| e.to_string())?;
            reader.SetCurrentMediaType(stream, None, &wanted).map_err(|e| format!("RGB32 unsupported ({e})"))?;
            let current = reader.GetCurrentMediaType(stream).map_err(|e| e.to_string())?;
            let size = current.GetUINT64(&MF_MT_FRAME_SIZE).map_err(|e| e.to_string())?;
            let (width, height) = ((size >> 32) as usize, (size & 0xFFFF_FFFF) as usize);
            let default_stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32 as isize).unwrap_or((width * 4) as isize);
            loop {
                let mut flags = 0u32;
                let mut sample: Option<IMFSample> = None;
                reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample)).map_err(|e| e.to_string())?;
                if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 | MF_SOURCE_READERF_ERROR.0 as u32) != 0 {
                    break;
                }
                let Some(sample) = sample else { continue };
                let buffer = sample.ConvertToContiguousBuffer().map_err(|e| e.to_string())?;
                let image = match buffer.cast::<IMF2DBuffer>() {
                    Ok(b2) => {
                        let mut scan0: *mut u8 = std::ptr::null_mut();
                        let mut pitch = 0i32;
                        b2.Lock2D(&mut scan0, &mut pitch).map_err(|e| e.to_string())?;
                        let len = pitch.unsigned_abs() as usize * height;
                        // A negative pitch: scan line 0 is the top row, stored last.
                        let base = if pitch < 0 { scan0.offset(pitch as isize * (height as isize - 1)) } else { scan0 };
                        let data = std::slice::from_raw_parts(base, len);
                        let image = bgra_to_image(data, width, height, pitch as isize);
                        let _ = b2.Unlock2D();
                        image
                    }
                    Err(_) => {
                        let mut ptr: *mut u8 = std::ptr::null_mut();
                        let mut length = 0u32;
                        buffer.Lock(&mut ptr, None, Some(&mut length)).map_err(|e| e.to_string())?;
                        let data = std::slice::from_raw_parts(ptr, length as usize);
                        let image = bgra_to_image(data, width, height, default_stride);
                        let _ = buffer.Unlock();
                        image
                    }
                };
                if let Some(image) = image {
                    if !on_frame(image) {
                        break;
                    }
                }
            }
            let _ = source.Shutdown();
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use eframe::egui::ColorImage;

    pub fn enumerate() -> Vec<String> {
        Vec::new()
    }

    pub fn capture(_index: usize, _on_frame: &dyn Fn(ColorImage) -> bool) -> Result<(), String> {
        Err("NO CAMERA SUPPORT".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_rows_convert_to_rgba_in_both_orientations() {
        // 2 × 2: top row blue, red; bottom row green, white (BGRA).
        let top_down = [255, 0, 0, 0, 0, 0, 255, 0, 0, 255, 0, 0, 255, 255, 255, 0];
        let img = bgra_to_image(&top_down, 2, 2, 8).unwrap();
        assert_eq!(img.pixels[0], eframe::egui::Color32::from_rgb(0, 0, 255));
        assert_eq!(img.pixels[1], eframe::egui::Color32::from_rgb(255, 0, 0));
        // Bottom-up storage (negative pitch): the last stored row is the top.
        let bottom_up = [0, 255, 0, 0, 255, 255, 255, 0, 255, 0, 0, 0, 0, 0, 255, 0];
        let img = bgra_to_image(&bottom_up, 2, 2, -8).unwrap();
        assert_eq!(img.pixels[0], eframe::egui::Color32::from_rgb(0, 0, 255));
        assert_eq!(img.pixels[3], eframe::egui::Color32::WHITE);
        assert!(bgra_to_image(&top_down, 3, 3, 12).is_none(), "short buffers are rejected");
    }
}
