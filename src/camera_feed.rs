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
        .name("jai-camera".into())
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
#[cfg_attr(not(windows), allow(dead_code))]
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

/// Video4Linux2 through ioctls on /dev/video*: MJPEG when the camera offers it, else YUYV.
#[cfg(not(windows))]
mod platform {
    use eframe::egui::ColorImage;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    const fn ioc(dir: u32, nr: u32, size: usize) -> libc::c_ulong {
        ((dir << 30) | ((size as u32) << 16) | (0x56 << 8) | nr) as libc::c_ulong
    }
    const QUERYCAP: libc::c_ulong = ioc(2, 0, size_of::<Capability>());
    const S_FMT: libc::c_ulong = ioc(3, 5, size_of::<Format>());
    const REQBUFS: libc::c_ulong = ioc(3, 8, size_of::<RequestBuffers>());
    const QUERYBUF: libc::c_ulong = ioc(3, 9, size_of::<Buffer>());
    const QBUF: libc::c_ulong = ioc(3, 15, size_of::<Buffer>());
    const DQBUF: libc::c_ulong = ioc(3, 17, size_of::<Buffer>());
    const STREAMON: libc::c_ulong = ioc(1, 18, 4);
    const STREAMOFF: libc::c_ulong = ioc(1, 19, 4);
    const CAPTURE: u32 = 1;
    const MMAP: u32 = 1;
    const MJPEG: u32 = u32::from_le_bytes(*b"MJPG");
    const YUYV: u32 = u32::from_le_bytes(*b"YUYV");

    /// linux/videodev2.h `struct v4l2_capability` (104 bytes).
    #[repr(C)]
    struct Capability {
        driver: [u8; 16],
        card: [u8; 32],
        bus_info: [u8; 32],
        version: u32,
        capabilities: u32,
        device_caps: u32,
        reserved: [u32; 3],
    }

    /// `struct v4l2_format` with the pixel-format member of its union (208 bytes on 64-bit).
    #[repr(C)]
    struct Format {
        kind: u32,
        _pad: u32,
        width: u32,
        height: u32,
        pixelformat: u32,
        field: u32,
        bytesperline: u32,
        sizeimage: u32,
        rest: [u32; 44],
    }

    /// `struct v4l2_requestbuffers` (20 bytes).
    #[repr(C)]
    struct RequestBuffers {
        count: u32,
        kind: u32,
        memory: u32,
        capabilities: u32,
        flags: u8,
        reserved: [u8; 3],
    }

    /// `struct v4l2_buffer` (88 bytes on 64-bit); `offset` is the `m` union.
    #[repr(C)]
    struct Buffer {
        index: u32,
        kind: u32,
        bytesused: u32,
        flags: u32,
        field: u32,
        _pad: u32,
        timestamp: [i64; 2],
        timecode: [u8; 16],
        sequence: u32,
        memory: u32,
        offset: u64,
        length: u32,
        reserved2: u32,
        request_fd: u32,
        _pad2: u32,
    }

    fn zeroed<T>() -> T {
        // SAFETY: only used for the plain-integer ioctl structs above, for which zero is valid.
        unsafe { std::mem::zeroed() }
    }

    fn ioctl<T>(fd: &OwnedFd, request: libc::c_ulong, arg: &mut T) -> Result<(), String> {
        // SAFETY: `arg` is a live repr(C) struct of the size the request number encodes.
        if unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg as *mut T) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }

    fn open(path: &str) -> Result<OwnedFd, String> {
        let c = std::ffi::CString::new(path).map_err(|e| e.to_string())?;
        // SAFETY: a valid C string; the descriptor is owned right away.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(format!("CAMERA BUSY ({})", std::io::Error::last_os_error()));
        }
        // SAFETY: just opened, owned by nothing else.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// (device node, name) of every capture node, /dev/video0 first (metadata nodes left out).
    fn nodes() -> Vec<(String, String)> {
        let mut numbers: Vec<u32> = std::fs::read_dir("/dev").into_iter().flatten().flatten().filter_map(|e| e.file_name().to_str()?.strip_prefix("video")?.parse().ok()).collect();
        numbers.sort_unstable();
        numbers
            .into_iter()
            .filter_map(|n| {
                let path = format!("/dev/video{n}");
                let fd = open(&path).ok()?;
                let mut cap: Capability = zeroed();
                ioctl(&fd, QUERYCAP, &mut cap).ok()?;
                let caps = if cap.capabilities & 0x8000_0000 != 0 { cap.device_caps } else { cap.capabilities };
                let name = String::from_utf8_lossy(&cap.card).trim_end_matches('\0').to_string();
                (caps & 1 != 0).then_some((path, name))
            })
            .collect()
    }

    pub fn enumerate() -> Vec<String> {
        nodes().into_iter().map(|(_, name)| name).collect()
    }

    /// RGBA from packed YUYV (BT.601, limited range); None when the buffer is too short.
    pub(super) fn yuyv_to_rgba(data: &[u8], width: usize, height: usize) -> Option<Vec<u8>> {
        let data = data.get(..width * height * 2)?;
        let clamp = |v: i32| v.clamp(0, 255) as u8;
        let mut out = Vec::with_capacity(width * height * 4);
        for quad in data.chunks_exact(4) {
            let (u, v) = (i32::from(quad[1]) - 128, i32::from(quad[3]) - 128);
            for y in [quad[0], quad[2]] {
                let c = 298 * (i32::from(y) - 16);
                out.extend_from_slice(&[clamp((c + 409 * v + 128) >> 8), clamp((c - 100 * u - 208 * v + 128) >> 8), clamp((c + 516 * u + 128) >> 8), 255]);
            }
        }
        Some(out)
    }

    struct Mapped(*mut libc::c_void, usize);

    impl Drop for Mapped {
        fn drop(&mut self) {
            // SAFETY: the pointer and length came from one successful mmap.
            unsafe { libc::munmap(self.0, self.1) };
        }
    }

    /// Reads frames until `on_frame` returns false.
    pub fn capture(index: usize, on_frame: &dyn Fn(ColorImage) -> bool) -> Result<(), String> {
        let nodes = nodes();
        let (path, _) = nodes.get(index).or(nodes.first()).ok_or("NO CAMERA")?;
        let fd = open(path)?;
        let mut format: Format = zeroed();
        let mut chosen = 0;
        for (wanted, width, height) in [(MJPEG, 1280, 720), (YUYV, 640, 480)] {
            (format.kind, format.width, format.height, format.pixelformat, format.field) = (CAPTURE, width, height, wanted, 1);
            if ioctl(&fd, S_FMT, &mut format).is_ok() && format.pixelformat == wanted {
                chosen = wanted;
                break;
            }
        }
        if chosen == 0 {
            return Err("CAMERA FORMAT UNSUPPORTED".into());
        }
        let (width, height) = (format.width as usize, format.height as usize);
        let mut request: RequestBuffers = zeroed();
        (request.count, request.kind, request.memory) = (4, CAPTURE, MMAP);
        ioctl(&fd, REQBUFS, &mut request).map_err(|e| format!("CAMERA BUFFERS ({e})"))?;
        let mut maps = Vec::new();
        for index in 0..request.count {
            let mut buf: Buffer = zeroed();
            (buf.index, buf.kind, buf.memory) = (index, CAPTURE, MMAP);
            ioctl(&fd, QUERYBUF, &mut buf)?;
            // SAFETY: maps the driver's own buffer at the offset it reported; checked below.
            let ptr = unsafe { libc::mmap(std::ptr::null_mut(), buf.length as usize, libc::PROT_READ, libc::MAP_SHARED, fd.as_raw_fd(), buf.offset as libc::off_t) };
            if ptr == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error().to_string());
            }
            maps.push(Mapped(ptr, buf.length as usize));
            ioctl(&fd, QBUF, &mut buf)?;
        }
        let mut on = CAPTURE as i32;
        ioctl(&fd, STREAMON, &mut on).map_err(|e| format!("CAMERA BUSY ({e})"))?;
        let mut bad = 0;
        let result = loop {
            let mut ready = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd.
            if unsafe { libc::poll(&mut ready, 1, 3000) } <= 0 {
                break Err("CAMERA STOPPED".to_string());
            }
            let mut buf: Buffer = zeroed();
            (buf.kind, buf.memory) = (CAPTURE, MMAP);
            if let Err(e) = ioctl(&fd, DQBUF, &mut buf) {
                break Err(e);
            }
            let map = &maps[buf.index as usize];
            // SAFETY: the mapping is `map.1` bytes long and stays alive until `maps` drops.
            let data = unsafe { std::slice::from_raw_parts(map.0 as *const u8, (buf.bytesused as usize).min(map.1)) };
            let image = if chosen == MJPEG {
                image::load_from_memory_with_format(data, image::ImageFormat::Jpeg).ok().map(|i| {
                    let rgba = i.to_rgba8();
                    ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())
                })
            } else {
                yuyv_to_rgba(data, width, height).map(|rgba| ColorImage::from_rgba_unmultiplied([width, height], &rgba))
            };
            let _ = ioctl(&fd, QBUF, &mut buf);
            match image {
                Some(image) => {
                    bad = 0;
                    if !on_frame(image) {
                        break Ok(());
                    }
                }
                None => {
                    bad += 1;
                    if bad > 30 {
                        break Err("CAMERA SENDS BAD FRAMES".to_string());
                    }
                }
            }
        };
        let _ = ioctl(&fd, STREAMOFF, &mut on);
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn v4l2_structs_match_videodev2_h_and_yuyv_converts() {
            assert_eq!(size_of::<Capability>(), 104);
            assert_eq!(size_of::<Format>(), 208);
            assert_eq!(size_of::<RequestBuffers>(), 20);
            assert_eq!(size_of::<Buffer>(), 88);
            assert_eq!(QUERYCAP, 0x8068_5600);
            assert_eq!(S_FMT, 0xC0D0_5605);
            assert_eq!(DQBUF, 0xC058_5611);
            // Two pixels: white (Y 235) and black (Y 16) with neutral chroma.
            let rgba = yuyv_to_rgba(&[235, 128, 16, 128], 2, 1).unwrap();
            assert_eq!(&rgba[..4], &[255, 255, 255, 255]);
            assert_eq!(&rgba[4..], &[0, 0, 0, 255]);
            assert!(yuyv_to_rgba(&[0; 3], 2, 1).is_none());
        }
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
