//! The Mobile Scanner: the phone's camera as JAI's scanner. QR codes and barcodes are read on the
//! phone (the browser's BarcodeDetector) and acted on here — links open, Wi-Fi codes show their
//! password, product codes are looked up; documents are photographed, read with Windows OCR, saved
//! to Documents\Jarvis_Scans and put on the clipboard.

use crate::features::{show_window, spoken, str_arg, user_folder};
use crate::tools::ToolContext;
use base64::Engine;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tracing::info;

#[derive(Clone, Default)]
pub struct Scanner {
    last: Arc<Mutex<Option<String>>>,
}

/// What a scanned code is.
#[derive(Debug, Clone, PartialEq)]
pub enum Code {
    Link(String),
    Wifi { ssid: String, password: String, security: String },
    Product(String),
    Text(String),
}

pub fn classify(value: &str) -> Code {
    let v = value.trim();
    if v.starts_with("http://") || v.starts_with("https://") {
        return Code::Link(v.to_string());
    }
    if let Some(rest) = v.strip_prefix("WIFI:") {
        let field = |k: &str| rest.split(';').find_map(|p| p.strip_prefix(k)).unwrap_or("").replace("\\;", ";").to_string();
        return Code::Wifi { ssid: field("S:"), password: field("P:"), security: field("T:") };
    }
    if (8..=14).contains(&v.len()) && v.chars().all(|c| c.is_ascii_digit()) {
        return Code::Product(v.to_string());
    }
    Code::Text(v.to_string())
}

impl Scanner {
    pub fn tool(&self, tools: &ToolContext, args: &Value) -> Result<String, String> {
        let mode = str_arg(args, &["mode", "action"]).unwrap_or("code").to_lowercase();
        if mode == "last" || mode == "result" {
            let last = self.last.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or("Nothing has been scanned yet, Sir.")?;
            return Ok(spoken(last.clone(), last));
        }
        let mode = if mode.contains("doc") || mode.contains("page") { "document" } else { "code" };
        tools.stark.push_to_satellite(&format!("SCAN|{mode}"));
        tools.stark.pulse(&["brain", "scanner", "hivemind"], false);
        Ok(spoken(
            format!("Scanner request sent to the satellite ({mode}). Open the satellite page on the phone; the camera starts there."),
            if mode == "document" { "The document scanner is waiting on your phone, Sir. Photograph the page and I'll read it." } else { "The code scanner is open on your phone, Sir. Point it at the code." },
        ))
    }

    fn remember(&self, what: &str) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(what.to_string());
    }
}

/// `POST /api/scan`: {kind: "code", value} or {kind: "document", image (base64)}.
pub async fn receive(tools: &ToolContext, body: &Value) -> Value {
    let scanner = &tools.stark.features.scanner;
    tools.stark.pulse(&["hivemind", "scanner", "brain"], false);
    let kind = body["kind"].as_str().unwrap_or("code");
    let message = if kind == "document" {
        let Some(data) = body["image"].as_str() else {
            return json!({ "ok": false, "error": "image required" });
        };
        let data = data.split_once(',').map(|(_, d)| d).unwrap_or(data);
        let bytes = match base64::engine::general_purpose::STANDARD.decode(data.trim()) {
            Ok(b) => b,
            Err(e) => return json!({ "ok": false, "error": e.to_string() }),
        };
        let dir = user_folder("Documents").join("Jarvis_Scans");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("scan_{}.jpg", crate::features::stamp()));
        if let Err(e) = std::fs::write(&path, &bytes) {
            return json!({ "ok": false, "error": e.to_string() });
        }
        match crate::screen::ocr_file(&path).await {
            Ok(text) if !text.trim().is_empty() => {
                let _ = std::fs::write(path.with_extension("txt"), &text);
                let _ = crate::clipboard::write(&text).await;
                show_window(tools, "mobile_scanner", "Mobile Scanner — Document", &format!("## Scanned document\n`{}`\n\n{text}", path.display()), 600.0, 640.0);
                format!("Document scanned: {} characters, saved to JAI Scans and copied.", text.chars().count())
            }
            Ok(_) => format!("The page is saved ({}), but I found no text on it.", path.display()),
            Err(e) => format!("The page is saved, but reading it failed: {e}"),
        }
    } else {
        let value = body["value"].as_str().unwrap_or("").trim().to_string();
        if value.is_empty() {
            return json!({ "ok": false, "error": "value required" });
        }
        match classify(&value) {
            Code::Link(url) => {
                let _ = crate::apps::open_default_browser(&url).await;
                format!("Opened the scanned link: {url}")
            }
            Code::Wifi { ssid, password, security } => {
                let _ = crate::clipboard::write(&password).await;
                show_window(tools, "mobile_scanner", "Mobile Scanner — Wi-Fi", &format!("## Wi-Fi\n**Network:** {ssid}\n\n**Password:** `{password}`\n\n**Security:** {security}\n\n*(password on the clipboard)*"), 420.0, 300.0);
                format!("Wi-Fi code for {ssid}; the password is on the clipboard.")
            }
            Code::Product(ean) => {
                let _ = crate::clipboard::write(&ean).await;
                let url = format!("https://geizhals.de/?fs={ean}");
                show_window(tools, "mobile_scanner", "Mobile Scanner — Product", &format!("## Barcode {ean}\n\n[Prices on Geizhals]({url})\n\n[Search the web](https://duckduckgo.com/?q={ean})"), 420.0, 260.0);
                format!("Product code {ean} scanned; price links are on screen.")
            }
            Code::Text(text) => {
                let _ = crate::clipboard::write(&text).await;
                show_window(tools, "mobile_scanner", "Mobile Scanner", &format!("## Scanned code\n\n{text}\n\n*(on the clipboard)*"), 480.0, 320.0);
                format!("Scanned: {}", text.chars().take(80).collect::<String>())
            }
        }
    };
    info!("[SCANNER] {message}");
    scanner.remember(&message);
    let _ = tools.stark.events.send(crate::stark::StarkEvent::Notice(message.clone()));
    json!({ "ok": true, "message": message })
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "scan_with_phone",
        "description": "Mobile Scanner: open the camera scanner on Sir's phone (satellite). mode code (QR / barcode: links open on the PC, Wi-Fi passwords, product prices) or document (photo → OCR → text on the PC clipboard + Documents\\Jarvis_Scans); last = the latest result.",
        "parameters": {"type": "object", "properties": {
            "mode": {"type": "string", "enum": ["code", "document", "last"]}
        }, "required": ["mode"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_classified() {
        assert_eq!(classify("https://example.org"), Code::Link("https://example.org".into()));
        assert_eq!(classify("WIFI:S:Stark Tower;T:WPA;P:j4rv1s;;"), Code::Wifi { ssid: "Stark Tower".into(), password: "j4rv1s".into(), security: "WPA".into() });
        assert_eq!(classify("4006381333931"), Code::Product("4006381333931".into()));
        assert_eq!(classify("hello"), Code::Text("hello".into()));
    }
}
