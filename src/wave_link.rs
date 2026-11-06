//! Reads Elgato Wave Link 3's routing — read-only — so the Discord bridge knows which channel
//! reaches the Chat Mix without playing a test tone.
//!
//! Wave Link serves JSON-RPC 2.0 over a local WebSocket (the one its Stream Deck plugin uses); the
//! port is in `%LOCALAPPDATA%\Packages\Elgato.WaveLink_*\LocalState\ws-info.json`. Only
//! `getMixes` and `getChannels` are ever sent: Jarvis never changes Wave Link.

use crate::itslearning_bw_sso::{decode_frame, encode_frame};
use serde_json::{json, Value};
use std::time::Duration;

/// A Wave Link channel as far as the bridge cares.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub name: String,
    /// Apps Wave Link assigned to it ("Sonora", "Discord", "J.A.R.V.I.S").
    pub apps: Vec<String>,
    /// Is it audible in the Chat Mix (routed, turned up, nothing muted)?
    pub in_chat_mix: bool,
    /// A software channel (an output apps play to), not a microphone.
    pub software: bool,
}

/// The channels from `getMixes` + `getChannels` results.
pub fn parse(mixes: &Value, channels: &Value) -> Vec<Channel> {
    let chat: Vec<&str> = mixes["mixes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["name"].as_str().is_some_and(|n| n.to_lowercase().contains("chat")))
        .filter_map(|m| m["id"].as_str())
        .collect();
    channels["channels"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let on = !c["isMuted"].as_bool().unwrap_or(false) && c["level"].as_f64().unwrap_or(1.0) > 0.01;
            let routed = c["mixes"].as_array().into_iter().flatten().any(|m| {
                m["id"].as_str().is_some_and(|id| chat.contains(&id)) && !m["isMuted"].as_bool().unwrap_or(false) && m["level"].as_f64().unwrap_or(0.0) > 0.01
            });
            Channel {
                name: c["name"].as_str().unwrap_or_default().to_string(),
                apps: c["apps"].as_array().into_iter().flatten().filter_map(|a| a["name"].as_str().map(String::from)).collect(),
                in_chat_mix: on && routed,
                software: c["type"].as_str() == Some("Software"),
            }
        })
        .collect()
}

fn is_jarvis_app(app: &str) -> bool {
    app.to_lowercase().replace('.', "").contains("jarvis")
}

/// The channel Jarvis may talk through: in the Chat Mix and carrying no other app's audio (so the
/// call hears only him — never Sir's music or the call itself). One named for Jarvis first.
pub fn talk_channel(channels: &[Channel]) -> Option<&Channel> {
    let mut fit: Vec<&Channel> = channels.iter().filter(|c| c.software && c.in_chat_mix && c.apps.iter().all(|a| is_jarvis_app(a))).collect();
    fit.sort_by_key(|c| !c.name.to_lowercase().contains("jarvis"));
    fit.into_iter().next()
}

/// Channels in the Chat Mix that carry other apps (their audio reaches the call too).
pub fn leaking(channels: &[Channel]) -> Vec<&Channel> {
    channels.iter().filter(|c| c.in_chat_mix && c.apps.iter().any(|a| !is_jarvis_app(a))).collect()
}

/// The software channel Wave Link assigned `app` to ("Discord" → "Voice chat").
pub fn channel_of_app<'a>(channels: &'a [Channel], app: &str) -> Option<&'a Channel> {
    channels.iter().find(|c| c.software && c.apps.iter().any(|a| a.eq_ignore_ascii_case(app)))
}

/// The Windows output device of a channel ("Jarvis" → "Jarvis (Elgato Virtual Audio)").
pub fn device_of(channel: &str, outputs: &[String]) -> Option<String> {
    let prefix = format!("{} (", channel.to_lowercase());
    outputs.iter().find(|o| o.to_lowercase().starts_with(&prefix) && o.to_lowercase().contains("elgato")).cloned()
}

fn port() -> Option<u16> {
    let packages = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Packages");
    let dir = std::fs::read_dir(packages).ok()?.flatten().find(|e| e.file_name().to_string_lossy().starts_with("Elgato.WaveLink_"))?;
    let info: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("LocalState").join("ws-info.json")).ok()?).ok()?;
    info["port"].as_u64().and_then(|p| u16::try_from(p).ok())
}

/// Wave Link's channels, or None when Wave Link isn't running (2 s at most).
pub async fn channels() -> Option<Vec<Channel>> {
    tokio::time::timeout(Duration::from_secs(2), fetch()).await.ok()?.ok()
}

async fn fetch() -> Result<Vec<Channel>, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let port = port().ok_or("Wave Link isn't installed")?;
    let addr = format!("127.0.0.1:{port}");
    let mut stream = tokio::net::TcpStream::connect(&addr).await.map_err(|e| e.to_string())?;
    let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, uuid::Uuid::new_v4().as_bytes());
    let request = format!("GET / HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nOrigin: streamdeck://\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n");
    stream.write_all(request.as_bytes()).await.map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65_536];
    let head_end = loop {
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("Wave Link closed the connection".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    if !String::from_utf8_lossy(&buf[..head_end]).starts_with("HTTP/1.1 101") {
        return Err("Wave Link refused the connection".into());
    }
    buf.drain(..head_end);
    let mut results = [Value::Null, Value::Null];
    for (id, method) in [(1u64, "getMixes"), (2, "getChannels")] {
        let text = json!({ "jsonrpc": "2.0", "method": method, "id": id }).to_string();
        let mask = *uuid::Uuid::new_v4().as_bytes().first_chunk::<4>().unwrap_or(&[1, 2, 3, 4]);
        stream.write_all(&encode_frame(text.as_bytes(), 0x1, mask)).await.map_err(|e| e.to_string())?;
        // Wave Link also pushes events; read until this call's answer.
        let mut message = Vec::new();
        loop {
            if let Some((fin, opcode, payload, used)) = decode_frame(&buf) {
                buf.drain(..used);
                if opcode == 0x8 {
                    return Err("Wave Link closed the session".into());
                }
                if opcode == 0x9 || opcode == 0xA {
                    continue;
                }
                message.extend_from_slice(&payload);
                if !fin {
                    continue;
                }
                let v: Value = serde_json::from_slice(&message).unwrap_or(Value::Null);
                message.clear();
                if v["id"].as_u64() == Some(id) {
                    results[(id - 1) as usize] = v["result"].clone();
                    break;
                }
                continue;
            }
            let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("Wave Link closed the connection".into());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }
    let close = encode_frame(&[], 0x8, [3, 1, 4, 1]);
    let _ = stream.write_all(&close).await;
    Ok(parse(&results[0], &results[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(jarvis_mix: &str, music_in_chat: bool) -> (Value, Value) {
        let mixes = json!({"mixes": [
            {"id": "SD1", "name": "Personal Mix", "level": 1, "isMuted": false},
            {"id": "SD2", "name": "Audio Mix", "level": 1, "isMuted": false},
            {"id": "SD3", "name": "Chat Mix", "level": 1, "isMuted": false}]});
        let mut music = vec![json!({"id": "SD1", "level": 1, "isMuted": false})];
        if music_in_chat {
            music.push(json!({"id": "SD3", "level": 0.8, "isMuted": false}));
        }
        let channels = json!({"channels": [
            {"name": "System", "type": "Software", "level": 0.96, "isMuted": false, "apps": [], "mixes": [{"id": "SD1", "level": 1, "isMuted": false}]},
            {"name": "Music", "type": "Software", "level": 0.12, "isMuted": false, "apps": [{"name": "Sonora"}], "mixes": music},
            {"name": "Voice chat", "type": "Software", "level": 1, "isMuted": false, "apps": [{"name": "Discord"}], "mixes": [{"id": "SD1", "level": 1, "isMuted": false}]},
            {"name": "Microphone (fifine Microphone)", "type": "Hardware", "level": 1, "isMuted": false, "apps": [], "mixes": [{"id": "SD2", "level": 1, "isMuted": false}, {"id": "SD3", "level": 1, "isMuted": false}]},
            {"name": "Jarvis", "type": "Software", "level": 1, "isMuted": false, "apps": [{"name": "J.A.R.V.I.S"}], "mixes": [{"id": "SD1", "level": 1, "isMuted": false}, {"id": jarvis_mix, "level": 1, "isMuted": false}]}]});
        (mixes, channels)
    }

    #[test]
    fn finds_the_jarvis_channel_in_the_chat_mix() {
        let (m, c) = fixture("SD3", false);
        let channels = parse(&m, &c);
        assert_eq!(talk_channel(&channels).map(|c| c.name.as_str()), Some("Jarvis"));
        assert!(leaking(&channels).is_empty(), "the mic carries no app");
        let outputs: Vec<String> = ["Speakers (Logitech G733 Gaming Headset)", "Jarvis (Elgato Virtual Audio)", "Music (Elgato Virtual Audio)"].iter().map(|s| s.to_string()).collect();
        assert_eq!(device_of("Jarvis", &outputs).as_deref(), Some("Jarvis (Elgato Virtual Audio)"));
    }

    #[test]
    fn finds_the_channel_discord_plays_to() {
        let (m, c) = fixture("SD3", false);
        let channels = parse(&m, &c);
        assert_eq!(channel_of_app(&channels, "discord").map(|c| c.name.as_str()), Some("Voice chat"));
        assert_eq!(channel_of_app(&channels, "Spotify"), None);
    }

    #[test]
    fn never_talks_through_a_channel_that_carries_other_apps() {
        // Jarvis routed to the Audio Mix only, Music in the Chat Mix: no talk channel, one leak.
        let (m, c) = fixture("SD2", true);
        let channels = parse(&m, &c);
        assert_eq!(talk_channel(&channels), None, "Sonora's channel would send Sir's music too");
        assert_eq!(leaking(&channels).iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Music"]);
    }

    #[test]
    fn a_muted_route_does_not_count() {
        let (m, mut c) = fixture("SD3", false);
        c["channels"][4]["mixes"][1]["isMuted"] = json!(true);
        assert_eq!(talk_channel(&parse(&m, &c)), None);
    }
}
