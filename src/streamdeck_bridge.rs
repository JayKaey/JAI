//! DECK // STREAM DECK BRIDGE: a tiny HTTP endpoint on 127.0.0.1:19840 so an Elgato Stream Deck
//! button (its "Website" / API action, "open in background") or a G915 macro tool can trigger
//! Jarvis without the microphone:
//!
//! * `GET /run?token=…&text=open%20my%20timetable` — any request, exactly as if typed;
//! * `GET /theme?token=…&name=stark_gold` — one of the HUD presets;
//! * `GET /view?token=…&name=sim_lab` — a HUD view (home, board, core, sim_lab, telemetry, …);
//! * `GET /status` — "ok" (no token), so the button can show the bridge is up.
//!
//! Localhost only. Every command needs the bridge token (`streamdeck_token` in the config,
//! generated on first start), and a request a web page made (it carries an `Origin` header) is
//! refused, so a website cannot drive Jarvis through the browser.

use crate::app::Jarvis;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

pub const ADDRESS: &str = "127.0.0.1:19840";

/// A parsed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeckCommand {
    Status,
    Run(String),
    Theme(String),
    View(String),
}

/// What a raw HTTP request asks for, or the status line to refuse it with.
pub fn parse_request(raw: &str, token: &str) -> Result<DeckCommand, &'static str> {
    let mut lines = raw.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    if parts.next() != Some("GET") {
        return Err("405 Method Not Allowed");
    }
    let target = parts.next().unwrap_or("/");
    // A browser page's request (fetch, image, form) carries an Origin: never a Stream Deck.
    if lines.take_while(|l| !l.is_empty()).any(|l| l.to_ascii_lowercase().starts_with("origin:")) {
        return Err("403 Forbidden");
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let param = |key: &str| {
        query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key).then(|| urlencoding::decode(&v.replace('+', " ")).map(|s| s.into_owned()).unwrap_or_default())
        })
    };
    if path == "/status" {
        return Ok(DeckCommand::Status);
    }
    if token.is_empty() || param("token").as_deref() != Some(token) {
        return Err("401 Unauthorized");
    }
    let value = |key: &str| param(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).ok_or("400 Bad Request");
    match path {
        "/run" => Ok(DeckCommand::Run(value("text")?)),
        "/theme" => Ok(DeckCommand::Theme(value("name")?)),
        "/view" => Ok(DeckCommand::View(value("name")?)),
        _ => Err("404 Not Found"),
    }
}

/// A fresh random token.
fn new_token() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 12];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Opens the bridge (once, at start-up; a busy port only logs).
pub fn start(app: Jarvis) {
    let token = {
        let existing = app.config().read(|c| c.streamdeck_token.clone());
        if existing.len() >= 16 {
            existing
        } else {
            let t = new_token();
            let keep = t.clone();
            app.config().update(|c| c.streamdeck_token = keep);
            t
        }
    };
    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(ADDRESS).await {
            Ok(l) => l,
            Err(e) => {
                warn!("[DECK] Stream Deck bridge not started on {ADDRESS}: {e}");
                return;
            }
        };
        info!("[DECK] Stream Deck bridge on http://{ADDRESS} (e.g. /run?token={token}&text=open%20my%20timetable)");
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { continue };
            let (app, token) = (app.clone(), token.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = match tokio::time::timeout(std::time::Duration::from_secs(3), socket.read(&mut buf)).await {
                    Ok(Ok(n)) => n,
                    _ => return,
                };
                let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                let (status, body) = match parse_request(&raw, &token) {
                    Ok(cmd) => {
                        info!("[DECK] {cmd:?}");
                        match cmd {
                            DeckCommand::Status => ("200 OK", "ok".to_string()),
                            DeckCommand::Run(text) => {
                                let app = app.clone();
                                tokio::spawn(async move { app.handle_command(crate::hud::HudCommand::Chat { text }).await });
                                ("200 OK", "queued".into())
                            }
                            DeckCommand::Theme(name) => match crate::theme_engine::preset(&name) {
                                Some(_) => {
                                    let n = name.trim().to_lowercase().replace([' ', '-'], "_");
                                    app.config().update(|c| c.interface.hud_palette = n);
                                    crate::gui::refresh();
                                    ("200 OK", "theme set".into())
                                }
                                None => ("400 Bad Request", "unknown theme".into()),
                            },
                            DeckCommand::View(name) => match crate::hud_dock::HudViewMode::parse(&name.replace('_', " ")) {
                                Some(mode) => {
                                    crate::hud_dock::request_view(mode);
                                    ("200 OK", "view requested".into())
                                }
                                None => ("400 Bad Request", "unknown view".into()),
                            },
                        }
                    }
                    Err(status) => (status, String::new()),
                };
                let reply = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_deck_requests_need_the_token_and_no_browser_origin() {
        let t = "abcdef0123456789";
        assert_eq!(parse_request("GET /status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n", t), Ok(DeckCommand::Status));
        assert_eq!(parse_request("GET /run?token=abcdef0123456789&text=open%20my%20timetable HTTP/1.1\r\n\r\n", t), Ok(DeckCommand::Run("open my timetable".into())));
        assert_eq!(parse_request("GET /theme?token=abcdef0123456789&name=stark_gold HTTP/1.1\r\n\r\n", t), Ok(DeckCommand::Theme("stark_gold".into())));
        assert_eq!(parse_request("GET /view?name=sim_lab&token=abcdef0123456789 HTTP/1.1\r\n\r\n", t), Ok(DeckCommand::View("sim_lab".into())));
        assert_eq!(parse_request("GET /run?token=wrong&text=x HTTP/1.1\r\n\r\n", t), Err("401 Unauthorized"));
        assert_eq!(parse_request("GET /run?text=shutdown HTTP/1.1\r\n\r\n", t), Err("401 Unauthorized"));
        assert_eq!(parse_request("GET /run?token=abcdef0123456789&text=x HTTP/1.1\r\nOrigin: https://evil.example\r\n\r\n", t), Err("403 Forbidden"), "a web page cannot drive Jarvis");
        assert_eq!(parse_request("POST /run HTTP/1.1\r\n\r\n", t), Err("405 Method Not Allowed"));
        assert_eq!(parse_request("GET /run?token=abcdef0123456789 HTTP/1.1\r\n\r\n", t), Err("400 Bad Request"));
        assert_eq!(parse_request("GET /nope?token=abcdef0123456789 HTTP/1.1\r\n\r\n", t), Err("404 Not Found"));
        assert_eq!(parse_request("GET /run?token=&text=x HTTP/1.1\r\n\r\n", ""), Err("401 Unauthorized"), "no token configured: nothing runs");
        assert_eq!(new_token().len(), 24);
    }
}
