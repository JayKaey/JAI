//! AEGIS // SERVER DECK: is the Minecraft server up, who is on, how fast does it answer — and is
//! the VPS reachable. Minecraft servers are asked with the game's own Server List Ping (a
//! handshake and a status request over TCP, no plugin or RCON password needed); a VPS is checked
//! by opening its SSH port. The result goes on the board as one figure block per server.
//! (TPS / MSPT / RAM need RCON or SSH credentials, which Jarvis does not keep.)

use crate::hud_board::{self, Block, BlockKind, Report};
use crate::tools::ToolContext;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const TOOL: &str = "server_deck";
const TIMEOUT: Duration = Duration::from_secs(4);
/// The servers Sir asked about last (a bare "is my server online?" checks them again).
static LAST: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": TOOL,
        "description": "AEGIS server deck: live status of Minecraft servers (online, players, version, ping, MOTD — Server List Ping) and VPS reachability (SSH port), shown as board blocks. hosts: 'play.example.net', 'host:25565', 'vps:my.server.com'.",
        "parameters": {"type": "object", "properties": {
            "hosts": {"type": "array", "items": {"type": "string"}}
        }}
    }})]
}

/// A Minecraft server's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct McStatus {
    pub version: String,
    pub online: i64,
    pub max: i64,
    pub motd: String,
    pub sample: Vec<String>,
}

pub fn write_varint(out: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7F == 0 {
            out.push(v as u8);
            return;
        }
        out.push((v & 0x7F | 0x80) as u8);
        v >>= 7;
    }
}

/// (value, bytes used), or None when incomplete.
pub fn read_varint(buf: &[u8]) -> Option<(i32, usize)> {
    let mut value: u32 = 0;
    for (i, b) in buf.iter().take(5).enumerate() {
        value |= ((b & 0x7F) as u32) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value as i32, i + 1));
        }
    }
    None
}

fn packet(id: i32, body: &[u8]) -> Vec<u8> {
    let mut inner = Vec::new();
    write_varint(&mut inner, id);
    inner.extend_from_slice(body);
    let mut out = Vec::new();
    write_varint(&mut out, inner.len() as i32);
    out.extend(inner);
    out
}

/// The handshake (next state: status) and the status request.
pub fn status_request(host: &str, port: u16) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(&mut body, 767); // 1.21; servers answer the status to any version
    write_varint(&mut body, host.len() as i32);
    body.extend_from_slice(host.as_bytes());
    body.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut body, 1);
    let mut out = packet(0, &body);
    out.extend(packet(0, &[]));
    out
}

/// Plain text of a chat component (string, {"text", "extra"}), without § formatting codes.
fn chat_text(v: &Value) -> String {
    let raw = match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            let mut s = o.get("text").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(Value::Array(extra)) = o.get("extra") {
                for e in extra {
                    s.push_str(&chat_text(e));
                }
            }
            s
        }
        Value::Array(a) => a.iter().map(chat_text).collect(),
        _ => String::new(),
    };
    let mut out = String::new();
    let mut skip = false;
    for c in raw.chars() {
        if skip {
            skip = false;
        } else if c == '§' {
            skip = true;
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn parse_status(json_text: &str) -> Option<McStatus> {
    let v: Value = serde_json::from_str(json_text).ok()?;
    Some(McStatus {
        version: v["version"]["name"].as_str().unwrap_or("?").to_string(),
        online: v["players"]["online"].as_i64().unwrap_or(0),
        max: v["players"]["max"].as_i64().unwrap_or(0),
        motd: chat_text(&v["description"]),
        sample: v["players"]["sample"].as_array().map(|a| a.iter().filter_map(|p| p["name"].as_str().map(String::from)).collect()).unwrap_or_default(),
    })
}

/// (host, port) from "host", "host:port"; Minecraft's default port.
pub fn split_host(target: &str, default_port: u16) -> (String, u16) {
    match target.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().unwrap_or(default_port)),
        _ => (target.to_string(), default_port),
    }
}

async fn ping_minecraft(host: &str, port: u16) -> Result<(McStatus, u128), String> {
    let t0 = Instant::now();
    let mut stream = tokio::time::timeout(TIMEOUT, tokio::net::TcpStream::connect((host, port))).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.to_string())?;
    stream.write_all(&status_request(host, port)).await.map_err(|e| e.to_string())?;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let n = tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), stream.read(&mut chunk)).await.map_err(|_| "no answer".to_string())?.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("closed the connection".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        let Some((len, a)) = read_varint(&buf) else { continue };
        if buf.len() < a + len as usize {
            if buf.len() > 1 << 20 {
                return Err("answer too large".into());
            }
            continue;
        }
        let body = &buf[a..a + len as usize];
        let (_, b) = read_varint(body).ok_or("bad packet")?;
        let (slen, c) = read_varint(&body[b..]).ok_or("bad packet")?;
        let text = std::str::from_utf8(body.get(b + c..b + c + slen as usize).ok_or("bad packet")?).map_err(|e| e.to_string())?;
        return parse_status(text).map(|s| (s, t0.elapsed().as_millis())).ok_or_else(|| "not a Minecraft answer".into());
    }
}

/// A player count that fits a narrow block: 20797 → "20.8k", 200000 → "200k".
pub fn compact(n: i64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
        n if n >= 10_000 => format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        n => n.to_string(),
    }
}

async fn ping_tcp(host: &str, port: u16) -> Result<u128, String> {
    let t0 = Instant::now();
    tokio::time::timeout(TIMEOUT, tokio::net::TcpStream::connect((host, port))).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.to_string())?;
    Ok(t0.elapsed().as_millis())
}

async fn check(target: &str) -> (Block, String) {
    if let Some(vps) = target.strip_prefix("vps:") {
        let (host, port) = split_host(vps, 22);
        return match ping_tcp(&host, port).await {
            Ok(ms) => (Block::new(format!("AEGIS // VPS {host}"), "ONLINE", "SOURCE: SSH PORT", BlockKind::HeroFigure { header: format!("{host} · port {port}"), figure: format!("{ms} ms"), note: "SSH is answering.".into() }), format!("{host} is up ({ms} ms)")),
            Err(e) => (Block::new(format!("AEGIS // VPS {host}"), "OFFLINE", "SOURCE: SSH PORT", BlockKind::HeroFigure { header: format!("{host} · port {port}"), figure: "OFFLINE".into(), note: e.clone() }), format!("{host} is not answering ({e})")),
        };
    }
    let (host, port) = split_host(target, 25565);
    match ping_minecraft(&host, port).await {
        Ok((s, ms)) => {
            let players = if s.sample.is_empty() { String::new() } else { format!(" On: {}.", s.sample.join(", ")) };
            (
                Block::new(format!("AEGIS // SERVER {host}"), "ONLINE", "SOURCE: SERVER LIST PING", BlockKind::HeroFigure { header: format!("{host} · {} · {ms} ms", s.version), figure: format!("{} / {}", compact(s.online), compact(s.max)), note: format!("{}{players}", s.motd) }),
                format!("{host} is online with {} of {} players", s.online, s.max),
            )
        }
        Err(e) => (Block::new(format!("AEGIS // SERVER {host}"), "OFFLINE", "SOURCE: SERVER LIST PING", BlockKind::HeroFigure { header: format!("{host}:{port}"), figure: "OFFLINE".into(), note: e.clone() }), format!("{host} is offline ({e})")),
    }
}

pub async fn run_tool(_tools: &ToolContext, args: &Value) -> Result<String, String> {
    let mut hosts: Vec<String> = args["hosts"].as_array().map(|a| a.iter().filter_map(|h| h.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    if let Some(h) = args["host"].as_str().filter(|h| !h.trim().is_empty()) {
        hosts.push(h.trim().to_string());
    }
    if hosts.is_empty() {
        hosts = crate::crash_guard::lock(&LAST).clone();
    }
    if hosts.is_empty() {
        return Err("Which server, Sir? Give me its address, e.g. \"check server play.example.net\".".into());
    }
    *crate::crash_guard::lock(&LAST) = hosts.clone();
    let results = futures_util::future::join_all(hosts.iter().map(|h| check(h))).await;
    let lines: Vec<String> = results.iter().map(|(_, l)| l.clone()).collect();
    hud_board::present(Report { topic: "Server deck".into(), created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(), blocks: results.into_iter().map(|(b, _)| b).collect(), sources: Vec::new(), id: None });
    Ok(format!("{}\nSpoken: {}, Sir.", lines.join("\n"), lines.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_the_status_request_follow_the_protocol() {
        for (v, bytes) in [(0, vec![0x00]), (1, vec![0x01]), (127, vec![0x7F]), (128, vec![0x80, 0x01]), (25565, vec![0xDD, 0xC7, 0x01]), (-1, vec![0xFF, 0xFF, 0xFF, 0xFF, 0x0F])] {
            let mut out = Vec::new();
            write_varint(&mut out, v);
            assert_eq!(out, bytes, "{v}");
            assert_eq!(read_varint(&out), Some((v, out.len())));
        }
        assert_eq!(read_varint(&[0x80]), None, "incomplete");
        let req = status_request("mc.test", 25565);
        // Handshake: length, id 0, protocol 767 (0xFF 0x05), host, port, next state 1; then [1, 0].
        assert_eq!(&req[1..4], &[0x00, 0xFF, 0x05]);
        assert_eq!(&req[4..12], b"\x07mc.test");
        assert_eq!(&req[12..15], &[0x63, 0xDD, 0x01]);
        assert_eq!(&req[req.len() - 2..], &[0x01, 0x00]);
        assert_eq!(req[0] as usize, req.len() - 3, "the handshake's length prefix");
    }

    #[test]
    fn a_status_answer_reads_players_version_and_motd() {
        let json = r#"{"version":{"name":"1.21.1","protocol":767},"players":{"max":20,"online":3,"sample":[{"name":"Jay","id":"0"},{"name":"Max","id":"1"}]},"description":{"text":"§aCreate ","extra":[{"text":"Modpack"}]}}"#;
        let s = parse_status(json).unwrap();
        assert_eq!((s.version.as_str(), s.online, s.max), ("1.21.1", 3, 20));
        assert_eq!(s.motd, "Create Modpack");
        assert_eq!(s.sample, vec!["Jay", "Max"]);
        assert_eq!(parse_status(r#"{"description":"Hello §lworld","players":{"online":0,"max":10},"version":{"name":"Paper"}}"#).unwrap().motd, "Hello world");
        assert!(parse_status("not json").is_none());
        assert_eq!(split_host("play.x.net:25570", 25565), ("play.x.net".to_string(), 25570));
        assert_eq!(split_host("play.x.net", 25565), ("play.x.net".to_string(), 25565));
        assert_eq!((compact(20_797), compact(200_000), compact(20), compact(1_500_000)), ("20.8k".into(), "200k".into(), "20".into(), "1.5M".into()));
    }

    #[test]
    fn a_closed_port_is_reported_offline() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (block, line) = rt.block_on(check("127.0.0.1:1"));
        assert!(line.contains("offline"), "{line}");
        assert!(matches!(block.kind, BlockKind::HeroFigure { ref figure, .. } if figure == "OFFLINE"));
    }
}
