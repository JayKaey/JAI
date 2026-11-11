//! Stark Air-Drop: content between the PC and Sir's phone through the mobile satellite. "Send this
//! to my phone" beams the clipboard, the highlighted text, a screenshot or a file (the phone gets a
//! download card); from the phone, text and links open on the PC, files land in
//! Downloads\Jarvis_AirDrop, and a photo can be read (OCR) straight into the clipboard.

use crate::features::{show_window, spoken, str_arg, user_folder};
use crate::tools::ToolContext;
use base64::Engine;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::info;

/// A file offered to the phone.
#[derive(Debug, Clone)]
pub struct Shared {
    pub id: u64,
    pub path: PathBuf,
    pub name: String,
}

#[derive(Default)]
struct State {
    next: u64,
    shared: Vec<Shared>,
    /// What arrived from the phone (newest last).
    received: Vec<String>,
}

#[derive(Clone, Default)]
pub struct AirDrop {
    state: Arc<Mutex<State>>,
}

/// The satellite page turns `AIRDROP|<id>|<name>` into a download card.
pub fn file_card(id: u64, name: &str) -> String {
    format!("AIRDROP|{id}|{name}")
}

pub fn mime_of(name: &str) -> &'static str {
    match Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "pdf" => "application/pdf",
        "txt" | "md" | "log" => "text/plain; charset=utf-8",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

impl AirDrop {
    fn share(&self, path: PathBuf) -> Shared {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.next += 1;
        let item = Shared { id: s.next, name: path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string(), path };
        s.shared.push(item.clone());
        if s.shared.len() > 30 {
            s.shared.remove(0);
        }
        item
    }

    /// A file the phone may download (only ones JAI offered).
    pub fn shared(&self, id: u64) -> Option<Shared> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).shared.iter().find(|s| s.id == id).cloned()
    }

    fn note_received(&self, what: String) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.received.push(what);
        if s.received.len() > 20 {
            s.received.remove(0);
        }
    }
}

/// A screenshot of the primary screen as PNG (for "beam my screen to my phone").
async fn screenshot() -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!("jarvis_beam_{}.png", crate::features::stamp()));
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms,System.Drawing\n$b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds\n$bmp=New-Object System.Drawing.Bitmap $b.Width,$b.Height\n$g=[System.Drawing.Graphics]::FromImage($bmp)\n$g.CopyFromScreen($b.X,$b.Y,0,0,$bmp.Size)\n$bmp.Save('{}',[System.Drawing.Imaging.ImageFormat]::Png)\n$g.Dispose();$bmp.Dispose()",
        path.display().to_string().replace('\'', "''")
    );
    crate::features::powershell(&script, std::time::Duration::from_secs(20)).await?;
    Ok(path)
}

/// `beam_to_device`.
pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let target = str_arg(args, &["target", "device"]).unwrap_or("phone").to_lowercase();
    if target.contains("pc") || target.contains("computer") {
        return Ok(spoken(
            "Open the satellite on the phone → Stark Air-Drop: type or paste text, pick a file, or take a photo (tick \"Photo → text\" to read it into the PC's clipboard).",
            "Open the satellite on your phone, Sir, and use the Air-Drop panel. Text and links open here, photos can be read straight into my clipboard.",
        ));
    }
    let content = str_arg(args, &["content", "text", "file"]).unwrap_or("clipboard").to_string();
    let stark = &tools.stark;
    let drop = &stark.features.airdrop;
    let lower = content.to_lowercase();
    let (shown, said) = if matches!(lower.as_str(), "screenshot" | "screen" | "my screen") {
        let shot = screenshot().await?;
        let s = drop.share(shot);
        stark.push_to_satellite(&file_card(s.id, &s.name));
        (format!("Screenshot beamed to the satellite ({}).", s.name), "Screenshot sent to your phone, Sir.".to_string())
    } else if let Some(path) = crate::features::resolve_path(&content).filter(|p| p.is_file()) {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let s = drop.share(path);
        stark.push_to_satellite(&file_card(s.id, &s.name));
        (format!("File beamed to the satellite: {} ({:.1} MB). The phone shows a download card.", s.name, size as f64 / 1_048_576.0), format!("{} is on its way to your phone, Sir.", s.name))
    } else {
        let text = match lower.as_str() {
            "clipboard" | "this" | "that" | "it" => crate::clipboard::read().await.map_err(|e| format!("I couldn't read the clipboard: {e}"))?,
            "selection" | "selected" | "highlighted" => {
                let foreground = tools.focus.foreground().map(|f| f.handle);
                let raw = crate::screen::inspect(&json!({ "mode": "selection" }), foreground).await?;
                raw.split_once(":\n").map(|(_, t)| t.to_string()).unwrap_or(raw)
            }
            _ => content.clone(),
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("There's nothing to send, Sir — the clipboard is empty.".into());
        }
        // A file path on the clipboard (Explorer's "copy as path") sends the file itself.
        if let Some(path) = crate::features::resolve_path(text.trim_matches('"')).filter(|p| p.is_file()) {
            let s = drop.share(path);
            stark.push_to_satellite(&file_card(s.id, &s.name));
            (format!("File beamed: {}", s.name), format!("{} is on its way to your phone, Sir.", s.name))
        } else {
            stark.push_to_satellite(&text);
            let kind = if text.starts_with("http") { "link" } else { "text" };
            (format!("Beamed {} characters of {kind} to the satellite:\n{}", text.chars().count(), text.chars().take(400).collect::<String>()), format!("The {kind} is on your phone, Sir."))
        }
    };
    stark.pulse(&["brain", "airdrop", "hivemind"], false);
    info!("[AIRDROP] {shown}");
    Ok(spoken(shown, said))
}

/// `POST /api/airdrop` from the satellite: {text} or {name, data (base64), ocr}.
pub async fn receive(tools: &ToolContext, body: &Value) -> Value {
    let stark = &tools.stark;
    stark.pulse(&["hivemind", "airdrop", "brain"], false);
    if let Some(text) = body["text"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
        let summary = if text.starts_with("http://") || text.starts_with("https://") {
            let _ = crate::apps::open_default_browser(text).await;
            format!("Opened the link from your phone: {text}")
        } else {
            let _ = crate::clipboard::write(text).await;
            show_window(tools, "airdrop_inbox", "Stark Air-Drop", &format!("## From your phone\n\n{text}\n\n*(also on the clipboard)*"), 520.0, 420.0);
            format!("Text from your phone is on the clipboard ({} characters).", text.chars().count())
        };
        stark.features.airdrop.note_received(summary.clone());
        let _ = stark.events.send(crate::stark::StarkEvent::Notice(summary.clone()));
        return json!({ "ok": true, "message": summary });
    }
    let Some(data) = body["data"].as_str() else {
        return json!({ "ok": false, "error": "text or data required" });
    };
    let data = data.split_once(',').filter(|(head, _)| head.starts_with("data:")).map(|(_, d)| d).unwrap_or(data);
    let bytes = match base64::engine::general_purpose::STANDARD.decode(data.trim()) {
        Ok(b) => b,
        Err(e) => return json!({ "ok": false, "error": format!("bad base64: {e}") }),
    };
    let name = crate::features::safe_name(body["name"].as_str().unwrap_or("from_phone.jpg"));
    let dir = user_folder("Downloads").join("Jarvis_AirDrop");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return json!({ "ok": false, "error": e.to_string() });
    }
    let mut path = dir.join(&name);
    if path.exists() {
        path = dir.join(format!("{}_{name}", crate::features::stamp()));
    }
    if let Err(e) = std::fs::write(&path, &bytes) {
        return json!({ "ok": false, "error": e.to_string() });
    }
    let image = mime_of(&name).starts_with("image/");
    let summary = if body["ocr"].as_bool().unwrap_or(false) && image {
        match crate::screen::ocr_file(&path).await {
            Ok(text) if !text.trim().is_empty() => {
                let _ = crate::clipboard::write(&text).await;
                show_window(tools, "airdrop_inbox", "Stark Air-Drop — Photo text", &format!("## Text from the photo\n\n{text}\n\n*(on the clipboard · {})*", path.display()), 560.0, 520.0);
                format!("I read {} characters from your photo; they're on the clipboard.", text.chars().count())
            }
            Ok(_) => "The photo arrived, but I found no text on it.".to_string(),
            Err(e) => format!("The photo arrived, but reading it failed: {e}"),
        }
    } else {
        let _ = crate::win32::shell_execute(&path.display().to_string(), None, None);
        format!("{name} arrived from your phone ({:.1} MB) and is open.", bytes.len() as f64 / 1_048_576.0)
    };
    info!("[AIRDROP] {summary} ({})", path.display());
    stark.features.airdrop.note_received(summary.clone());
    let _ = stark.events.send(crate::stark::StarkEvent::Notice(summary.clone()));
    json!({ "ok": true, "message": summary, "saved": path.display().to_string() })
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "beam_to_device",
        "description": "Stark Air-Drop: send content to Sir's phone (mobile satellite) — content \"clipboard\", \"selection\", \"screenshot\", a file path, or literal text/link. target \"pc\" explains how to send from the phone to the PC (photos can be read into the clipboard).",
        "parameters": {"type": "object", "properties": {
            "content": {"type": "string"},
            "target": {"type": "string", "enum": ["phone", "pc"]}
        }, "required": ["content"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_and_cards() {
        let d = AirDrop::default();
        let s = d.share(PathBuf::from("C:/x/report.pdf"));
        assert_eq!(file_card(s.id, &s.name), "AIRDROP|1|report.pdf");
        assert!(d.shared(1).is_some() && d.shared(2).is_none());
        assert_eq!(mime_of("a.JPG"), "image/jpeg");
    }
}
