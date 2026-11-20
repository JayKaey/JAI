//! The Media Grabber: "download this video as MP3 from 1:20 to 2:05." Takes a URL (or the address
//! of the browser tab in front), runs yt-dlp in the background — MP4 into Downloads, MP3/WAV into
//! Music — and cuts sections with ffmpeg. Local files are cut / converted with ffmpeg directly.
//! Missing tools are installed on request through winget (yt-dlp, FFmpeg); Linux gets the pacman line.

use crate::features::{find_tool, run, spoken, str_arg, user_folder};
use crate::stark::StarkEvent;
use crate::tools::ToolContext;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct Job {
    pub id: u64,
    pub source: String,
    pub format: String,
    pub state: String,
    pub file: Option<PathBuf>,
}

#[derive(Clone, Default)]
pub struct MediaJobs {
    jobs: Arc<Mutex<Vec<Job>>>,
}

/// "1:20" / "01:02:03" / "75" → seconds.
pub fn seconds(t: &str) -> Option<u64> {
    let parts: Vec<u64> = t.trim().split(':').map(|p| p.trim().parse().ok()).collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [s] => Some(*s),
        [m, s] => Some(m * 60 + s),
        [h, m, s] => Some(h * 3600 + m * 60 + s),
        _ => None,
    }
}

/// yt-dlp arguments for a format and an optional section.
pub fn ytdlp_args(url: &str, format: &str, dir: &Path, section: Option<(u64, u64)>) -> Vec<String> {
    let mut a: Vec<String> = vec!["--no-playlist".into(), "--newline".into(), "-P".into(), dir.display().to_string(), "-o".into(), "%(title).120B.%(ext)s".into(), "--print".into(), "after_move:filepath".into()];
    match format {
        "mp3" | "wav" => a.extend(["-x".into(), "--audio-format".into(), format.into(), "--audio-quality".into(), "0".into()]),
        _ => a.extend(["-f".into(), "bv*[ext=mp4]+ba[ext=m4a]/b[ext=mp4]/bv*+ba/b".into(), "--merge-output-format".into(), "mp4".into()]),
    }
    if let Some((from, to)) = section {
        a.extend(["--download-sections".into(), format!("*{from}-{to}"), "--force-keyframes-at-cuts".into()]);
    }
    a.push(url.to_string());
    a
}

/// The address of the browser tab in front (Ctrl+L, Ctrl+C; the clipboard is restored).
#[cfg(windows)]
async fn active_browser_url() -> Result<String, String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_C, VK_CONTROL, VK_ESCAPE, VK_L};
    let browser = crate::win32::call(|| {
        crate::win32::app_windows().into_iter().find(|w| {
            let p = w.process.to_lowercase();
            ["chrome", "msedge", "firefox", "brave", "opera", "vivaldi", "arc"].iter().any(|b| p.contains(b))
        })
    })
    .await?
    .ok_or("No browser window is open, Sir.")?;
    let before = crate::clipboard::read().await.unwrap_or_default();
    let hwnd = browser.hwnd;
    // The keys go to the browser or nowhere: when it would not come to the front they are not
    // sent (Ctrl+L, Ctrl+C and Escape would land in whatever Sir is using).
    let in_front = crate::win32::call(move || {
        for wait in [150, 300] {
            crate::win32::focus(hwnd);
            std::thread::sleep(Duration::from_millis(wait));
            if crate::win32::is_foreground(hwnd) {
                let _ = crate::win32::send_chord(&[VK_CONTROL], VK_L);
                std::thread::sleep(Duration::from_millis(80));
                let _ = crate::win32::send_chord(&[VK_CONTROL], VK_C);
                std::thread::sleep(Duration::from_millis(120));
                let _ = crate::win32::tap_key(VK_ESCAPE);
                return true;
            }
        }
        false
    })
    .await?;
    if !in_front {
        return Err("The browser would not come to the front, so I couldn't read its address. Give me the link, Sir.".into());
    }
    let url = crate::clipboard::read().await.unwrap_or_default();
    let _ = crate::clipboard::write(&before).await;
    address(&url)
}

/// A link from what the address bar held.
fn address(copied: &str) -> Result<String, String> {
    let url = copied.trim().to_string();
    if url.starts_with("http") {
        Ok(url)
    } else if !url.is_empty() && url.contains('.') && !url.contains(' ') {
        Ok(format!("https://{url}"))
    } else {
        Err("I couldn't read the address of the browser tab.".into())
    }
}

/// The address of the browser in front (Ctrl+L, Ctrl+C, Escape; the clipboard is restored). The
/// keys go only when a browser really has the focus: they would land in whatever Sir is using.
#[cfg(not(windows))]
async fn active_browser_url() -> Result<String, String> {
    let copied = crate::kde::blocking(|| {
        let front = crate::plasma::active().ok_or("I see no window in front, Sir.")?;
        let who = format!("{} {}", front.app_id, front.resource_name).to_lowercase();
        if !["firefox", "chrom", "brave", "vivaldi", "opera", "zen", "librewolf", "falkon", "epiphany"].iter().any(|b| who.contains(b)) {
            return Err("No browser is in front, so I couldn't read an address. Give me the link, Sir.".to_string());
        }
        let layout = crate::plasma::layout();
        let key = |name: &str| crate::plasma::key_code(name, &layout).ok_or_else(|| format!("no key named {name}"));
        let (ctrl, l, c, esc) = (key("ctrl")?, key("l")?, key("c")?, key("escape")?);
        let before = crate::clipboard::read_now().ok();
        crate::plasma::chord(&[ctrl], l)?;
        std::thread::sleep(Duration::from_millis(80));
        crate::plasma::chord(&[ctrl], c)?;
        std::thread::sleep(Duration::from_millis(120));
        crate::plasma::chord(&[], esc)?;
        let copied = crate::clipboard::read_now().unwrap_or_default();
        if let Some(before) = before {
            let _ = crate::clipboard::write_now(&before);
        }
        Ok(copied)
    })
    .await??;
    address(&copied)
}

#[cfg(not(windows))]
async fn install_tools() -> Result<String, String> {
    Err("Install them with: sudo pacman -S yt-dlp ffmpeg. I cannot use sudo myself, Sir.".into())
}

#[cfg(windows)]
async fn install_tools() -> Result<String, String> {
    let winget = crate::apps::which("winget").ok_or("winget is not available; install yt-dlp and FFmpeg manually.")?;
    let mut done = Vec::new();
    for (id, name) in [("yt-dlp.yt-dlp", "yt-dlp"), ("Gyan.FFmpeg", "ffmpeg")] {
        if find_tool(name).is_some() {
            done.push(format!("{name} already installed"));
            continue;
        }
        let out = run(&winget, &["install", "--id", id, "-e", "--silent", "--accept-source-agreements", "--accept-package-agreements"], None, Duration::from_secs(600)).await?;
        done.push(if out.ok { format!("{name} installed") } else { format!("{name} failed: {}", out.error_line()) });
    }
    Ok(done.join("; "))
}

impl MediaJobs {
    pub fn describe(&self) -> Option<String> {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let running: Vec<String> = jobs.iter().filter(|j| j.state == "downloading").map(|j| format!("{} ({})", j.source.chars().take(60).collect::<String>(), j.format)).collect();
        (!running.is_empty()).then(|| format!("Media downloads running: {}", running.join(", ")))
    }

    fn set(&self, id: u64, state: &str, file: Option<PathBuf>) {
        if let Some(j) = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|j| j.id == id) {
            j.state = state.to_string();
            if file.is_some() {
                j.file = file;
            }
        }
    }

    /// `download_or_extract_media`.
    pub async fn tool(&self, tools: &ToolContext, args: &Value) -> Result<String, String> {
        let action = str_arg(args, &["action"]).unwrap_or("download").to_lowercase();
        if action.contains("install") {
            let report = install_tools().await?;
            return Ok(spoken(report.clone(), format!("Done, Sir: {report}.")));
        }
        if action.contains("status") || action == "list" {
            let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if jobs.is_empty() {
                return Ok(spoken("No media jobs yet.", "No downloads so far, Sir."));
            }
            let lines: Vec<String> = jobs.iter().rev().take(8).map(|j| format!("#{} {} [{}] {}{}", j.id, j.format, j.state, j.source, j.file.as_ref().map(|f| format!(" → {}", f.display())).unwrap_or_default())).collect();
            return Ok(spoken(lines.join("\n"), format!("{} jobs; the latest is {}.", jobs.len(), jobs.last().map(|j| j.state.as_str()).unwrap_or("idle"))));
        }
        let format = str_arg(args, &["format"]).unwrap_or("mp4").to_lowercase().trim_start_matches('.').to_string();
        let format = if ["mp3", "wav", "mp4"].contains(&format.as_str()) { format } else { "mp4".into() };
        let section = match (str_arg(args, &["start_time", "start"]).and_then(seconds), str_arg(args, &["end_time", "end"]).and_then(seconds)) {
            (Some(a), Some(b)) if b > a => Some((a, b)),
            (Some(a), None) => Some((a, a + 3600 * 6)),
            _ => None,
        };
        let raw = str_arg(args, &["url_or_active_browser", "url", "source"]).unwrap_or("active_browser").to_string();
        let dir = if format == "mp4" { user_folder("Downloads") } else { user_folder("Music") };

        // A local file: ffmpeg cuts / converts it.
        if let Some(path) = crate::features::resolve_path(&raw).filter(|p| p.is_file()) {
            let ffmpeg = find_tool("ffmpeg").ok_or("FFmpeg is not installed; say \"install the media tools\", Sir.")?;
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("clip");
            let out = dir.join(format!("{stem}_cut.{format}"));
            let mut a: Vec<String> = vec!["-y".into(), "-loglevel".into(), "error".into()];
            if let Some((from, to)) = section {
                a.extend(["-ss".into(), from.to_string(), "-to".into(), to.to_string()]);
            }
            a.extend(["-i".into(), path.display().to_string()]);
            if format != "mp4" {
                a.push("-vn".into());
            }
            a.push(out.display().to_string());
            let refs: Vec<&str> = a.iter().map(String::as_str).collect();
            let r = run(&ffmpeg, &refs, None, Duration::from_secs(900)).await?;
            if !r.ok {
                return Err(format!("FFmpeg failed: {}", r.error_line()));
            }
            return Ok(spoken(format!("Saved {}", out.display()), format!("Done, Sir. It's in your {} folder.", if format == "mp4" { "Downloads" } else { "Music" })));
        }

        let url = if raw.starts_with("http") { raw.clone() } else { active_browser_url().await? };
        let ytdlp = find_tool("yt-dlp").ok_or("yt-dlp is not installed. Say \"install the media tools\" and I'll set up yt-dlp and FFmpeg, Sir.")?;
        if (format != "mp4" || section.is_some()) && find_tool("ffmpeg").is_none() {
            return Err("Audio extraction and cutting need FFmpeg. Say \"install the media tools\", Sir.".into());
        }
        let id = {
            let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            let id = jobs.last().map(|j| j.id + 1).unwrap_or(1);
            jobs.push(Job { id, source: url.clone(), format: format.clone(), state: "downloading".into(), file: None });
            id
        };
        let args = ytdlp_args(&url, &format, &dir, section);
        let me = self.clone();
        let events = tools.stark.events.clone();
        let stark = tools.stark.clone();
        let fmt = format.clone();
        tokio::spawn(async move {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            info!("[MEDIA] yt-dlp {}", refs.join(" "));
            match run(&ytdlp, &refs, None, Duration::from_secs(3600)).await {
                Ok(out) if out.ok => {
                    let file = out.stdout.lines().rev().map(str::trim).find(|l| !l.is_empty() && Path::new(l).exists()).map(PathBuf::from);
                    let name = file.as_ref().and_then(|f| f.file_name()).and_then(|n| n.to_str()).unwrap_or("the file").to_string();
                    me.set(id, "done", file);
                    stark.pulse(&["media", "music"], false);
                    let _ = events.send(StarkEvent::Alert(format!("Your {} is ready, Sir: {name}.", fmt.to_uppercase())));
                }
                Ok(out) => {
                    warn!("[MEDIA] yt-dlp failed: {}", out.error_line());
                    me.set(id, "failed", None);
                    let _ = events.send(StarkEvent::Notice(format!("Download failed: {}", out.error_line())));
                }
                Err(e) => {
                    me.set(id, "failed", None);
                    let _ = events.send(StarkEvent::Notice(format!("Download failed: {e}")));
                }
            }
        });
        let span = section.map(|(a, b)| format!(" ({}–{})", clock(a), if b > a + 3600 * 5 { "end".into() } else { clock(b) })).unwrap_or_default();
        Ok(spoken(
            format!("Download #{id} started: {url} → {}{span} into {}", format.to_uppercase(), dir.display()),
            format!("Downloading it as {}{span}, Sir. I'll tell you when it's ready.", format.to_uppercase()),
        ))
    }
}

fn clock(s: u64) -> String {
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "download_or_extract_media",
        "description": "Media Grabber: download a video or its audio from a URL — or from the browser tab in front (url_or_active_browser = \"active_browser\") — as mp4 (Downloads) or mp3/wav (Music), optionally only start_time to end_time (\"1:20\"). Also cuts/converts a local file. action install_tools sets up yt-dlp and FFmpeg; status lists the jobs.",
        "parameters": {"type": "object", "properties": {
            "url_or_active_browser": {"type": "string"},
            "format": {"type": "string", "enum": ["mp4", "mp3", "wav"]},
            "start_time": {"type": "string"},
            "end_time": {"type": "string"},
            "action": {"type": "string", "enum": ["download", "status", "install_tools"]}
        }, "required": ["url_or_active_browser"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_arguments() {
        assert_eq!(seconds("1:20"), Some(80));
        assert_eq!(seconds("01:02:03"), Some(3723));
        assert_eq!(seconds("x"), None);
        let a = ytdlp_args("https://youtu.be/x", "mp3", Path::new("C:/Music"), Some((80, 125)));
        assert!(a.windows(2).any(|w| w[0] == "--audio-format" && w[1] == "mp3"));
        assert!(a.windows(2).any(|w| w[0] == "--download-sections" && w[1] == "*80-125"));
        assert_eq!(a.last().unwrap(), "https://youtu.be/x");
        assert_eq!(clock(125), "2:05");
    }
}
