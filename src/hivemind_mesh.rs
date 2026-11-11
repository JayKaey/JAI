//! The Stark multi-node hivemind: Sir's other machines — a Raspberry Pi 5, a Linux VPS, a
//! Minecraft server — registered in `remote_nodes` (`jarvis_memory.db`) and commanded over SSH
//! (Windows ships OpenSSH; key-based login, `BatchMode` so nothing ever waits for a password).
//!
//! `manage_remote_node` registers and lists nodes, checks them (TCP reachability, uptime / memory
//! / disk over SSH, Minecraft's server-list ping for players online), starts / stops / restarts
//! systemd services, offloads a command, and streams a service's journal into a live window on
//! the PC. The phone and the iPad join the mesh through the mobile satellite (`satellite.rs`).

use crate::tools::{str_arg, ToolContext};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

const SSH_TIMEOUT: Duration = Duration::from_secs(25);
/// Live log windows stop following after this.
const LOG_STREAM_MAX: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RemoteNode {
    pub name: String,
    /// pi, vps, minecraft, pc, server.
    pub kind: String,
    pub host: String,
    pub ssh_port: u16,
    pub ssh_user: String,
    pub ssh_key: String,
    /// Default services (systemd units) of this node.
    pub services: Vec<String>,
    /// Minecraft port (25565) for `kind = minecraft`.
    pub game_port: u16,
    pub notes: String,
    pub last_status: String,
    pub last_seen: String,
}

impl RemoteNode {
    fn target(&self) -> String {
        if self.ssh_user.is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.ssh_user, self.host)
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} ({}, {}{}{}){}",
            self.name,
            if self.kind.is_empty() { "server" } else { &self.kind },
            self.target(),
            if self.ssh_port != 22 { format!(":{}", self.ssh_port) } else { String::new() },
            if self.services.is_empty() { String::new() } else { format!(", services {}", self.services.join(", ")) },
            if self.last_status.is_empty() { String::new() } else { format!(" — last: {} at {}", self.last_status, self.last_seen) }
        )
    }
}

#[derive(Clone)]
pub struct Hivemind {
    conn: Arc<Mutex<Connection>>,
}

impl Hivemind {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let conn = Connection::open(path.as_ref()).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Self::with(conn)
    }

    pub fn in_memory() -> Self {
        Self::with(Connection::open_in_memory().expect("in-memory SQLite")).expect("in-memory node schema")
    }

    fn with(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS remote_nodes (
                 name TEXT PRIMARY KEY,
                 kind TEXT NOT NULL DEFAULT '',
                 host TEXT NOT NULL,
                 ssh_port INTEGER NOT NULL DEFAULT 22,
                 ssh_user TEXT NOT NULL DEFAULT '',
                 ssh_key TEXT NOT NULL DEFAULT '',
                 services TEXT NOT NULL DEFAULT '[]',
                 game_port INTEGER NOT NULL DEFAULT 0,
                 notes TEXT NOT NULL DEFAULT '',
                 last_status TEXT NOT NULL DEFAULT '',
                 last_seen TEXT NOT NULL DEFAULT ''
             );",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteNode> {
        Ok(RemoteNode {
            name: r.get(0)?,
            kind: r.get(1)?,
            host: r.get(2)?,
            ssh_port: r.get::<_, i64>(3)? as u16,
            ssh_user: r.get(4)?,
            ssh_key: r.get(5)?,
            services: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
            game_port: r.get::<_, i64>(7)? as u16,
            notes: r.get(8)?,
            last_status: r.get(9)?,
            last_seen: r.get(10)?,
        })
    }

    pub fn list(&self) -> Vec<RemoteNode> {
        let db = self.db();
        let Ok(mut stmt) = db.prepare("SELECT name, kind, host, ssh_port, ssh_user, ssh_key, services, game_port, notes, last_status, last_seen FROM remote_nodes ORDER BY name") else {
            return Vec::new();
        };
        stmt.query_map([], Self::row).map(|r| r.filter_map(Result::ok).collect()).unwrap_or_default()
    }

    /// A node by name (fuzzy: "the pi", "my minecraft server").
    pub fn get(&self, name: &str) -> Option<RemoteNode> {
        let n = name.trim().to_lowercase().trim_start_matches("the ").trim_start_matches("my ").to_string();
        let list = self.list();
        if let Some(exact) = list.iter().find(|x| x.name.to_lowercase() == n) {
            return Some(exact.clone());
        }
        let kind_word = |k: &str| match k {
            "pi" => ["pi", "raspberry", "raspi"].iter().any(|w| n.contains(w)),
            "minecraft" => n.contains("minecraft") || n.contains("mc server"),
            "vps" => n.contains("vps") || n.contains("cloud"),
            _ => false,
        };
        list.iter()
            .find(|x| x.name.to_lowercase().contains(&n) || n.contains(&x.name.to_lowercase()) || kind_word(&x.kind))
            .or_else(|| list.iter().max_by(|a, b| strsim::jaro_winkler(&a.name.to_lowercase(), &n).total_cmp(&strsim::jaro_winkler(&b.name.to_lowercase(), &n))).filter(|x| strsim::jaro_winkler(&x.name.to_lowercase(), &n) > 0.85))
            .cloned()
    }

    pub fn save(&self, n: &RemoteNode) -> Result<(), String> {
        if n.name.trim().is_empty() || n.host.trim().is_empty() {
            return Err("A node needs a name and a host (IP address or hostname).".into());
        }
        self.db()
            .execute(
                "INSERT INTO remote_nodes (name, kind, host, ssh_port, ssh_user, ssh_key, services, game_port, notes) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(name) DO UPDATE SET kind = ?2, host = ?3, ssh_port = ?4, ssh_user = ?5, ssh_key = ?6, services = ?7, game_port = ?8, notes = ?9",
                params![
                    n.name.trim(),
                    n.kind,
                    n.host.trim(),
                    n.ssh_port as i64,
                    n.ssh_user,
                    n.ssh_key,
                    serde_json::to_string(&n.services).unwrap_or_default(),
                    n.game_port as i64,
                    n.notes
                ],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn remove(&self, name: &str) -> bool {
        self.db().execute("DELETE FROM remote_nodes WHERE name = ?1", params![name]).map(|n| n > 0).unwrap_or(false)
    }

    fn note_status(&self, name: &str, status: &str) {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        let _ = self.db().execute("UPDATE remote_nodes SET last_status = ?1, last_seen = ?2 WHERE name = ?3", params![status, now, name]);
    }
}

/// Runs one command on a node over SSH (no password prompts, bounded time).
pub async fn ssh(node: &RemoteNode, command: &str) -> Result<String, String> {
    let mut args: Vec<String> = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=8", "-o", "StrictHostKeyChecking=accept-new", "-o", "ServerAliveInterval=5"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.extend(["-p".to_string(), node.ssh_port.max(1).to_string()]);
    if !node.ssh_key.trim().is_empty() {
        args.extend(["-i".to_string(), node.ssh_key.trim().to_string()]);
    }
    args.extend([node.target(), command.to_string()]);
    let out = match crate::shell::run_helper(Path::new("ssh"), &args, None, &[], SSH_TIMEOUT).await {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err(format!("{} did not answer in time", node.name)),
        Err(e) => return Err(format!("cannot start ssh ({e}); install the OpenSSH client")),
    };
    let text = crate::shell::combine_output(&out.stdout, &out.stderr, 6000);
    if out.success() {
        Ok(text)
    } else if text.contains("Permission denied") {
        Err(format!("{} refused the SSH login (set up key-based login for {}: ssh-copy-id).", node.name, node.target()))
    } else {
        Err(format!("{} (exit {}): {}", node.name, out.code.unwrap_or(-1), text.trim()))
    }
}

/// Whether a TCP port answers, and how fast.
pub async fn tcp_ping(host: &str, port: u16) -> Result<u128, String> {
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(4), tokio::net::TcpStream::connect((host, port)))
        .await
        .map_err(|_| format!("{host}:{port} timed out"))?
        .map_err(|e| format!("{host}:{port} refused ({e})"))?;
    Ok(started.elapsed().as_millis())
}

fn varint(mut v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (v & 0x7f) as u8;
        v = ((v as u32) >> 7) as i32;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            return out;
        }
    }
}

async fn read_varint(stream: &mut tokio::net::TcpStream) -> Result<i32, String> {
    let mut value: i32 = 0;
    for i in 0..5 {
        let b = stream.read_u8().await.map_err(|e| e.to_string())?;
        value |= ((b & 0x7f) as i32) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("bad varint".into())
}

/// Minecraft's server-list ping: (players online, max, version, description).
pub async fn minecraft_status(host: &str, port: u16) -> Result<(i64, i64, String, String), String> {
    let work = async {
        let mut stream = tokio::net::TcpStream::connect((host, port)).await.map_err(|e| e.to_string())?;
        let mut handshake = vec![0x00];
        handshake.extend(varint(765));
        handshake.extend(varint(host.len() as i32));
        handshake.extend(host.as_bytes());
        handshake.extend(port.to_be_bytes());
        handshake.extend(varint(1));
        let mut packet = varint(handshake.len() as i32);
        packet.extend(handshake);
        packet.extend([0x01, 0x00]);
        stream.write_all(&packet).await.map_err(|e| e.to_string())?;
        let _len = read_varint(&mut stream).await?;
        let _id = read_varint(&mut stream).await?;
        let size = read_varint(&mut stream).await?.clamp(0, 1 << 20) as usize;
        let mut buf = vec![0u8; size];
        stream.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_slice(&buf).map_err(|e| e.to_string())?;
        let desc = match &v["description"] {
            Value::String(s) => s.clone(),
            d => d["text"].as_str().unwrap_or("").to_string(),
        };
        Ok::<_, String>((v["players"]["online"].as_i64().unwrap_or(0), v["players"]["max"].as_i64().unwrap_or(0), v["version"]["name"].as_str().unwrap_or("?").to_string(), desc))
    };
    tokio::time::timeout(Duration::from_secs(5), work).await.map_err(|_| "the Minecraft server did not answer in time".to_string())?
}

fn clean_service(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '@'))).then(|| s.to_string())
}

async fn status(node: &RemoteNode) -> String {
    let mut parts = Vec::new();
    match tcp_ping(&node.host, node.ssh_port.max(1)).await {
        Ok(ms) => parts.push(format!("reachable (SSH port answers in {ms} ms)")),
        Err(e) => parts.push(format!("NOT reachable: {e}")),
    }
    if node.kind == "minecraft" || node.game_port > 0 {
        let port = if node.game_port > 0 { node.game_port } else { 25565 };
        match minecraft_status(&node.host, port).await {
            Ok((on, max, version, motd)) => parts.push(format!("Minecraft {version}: {on}/{max} players online{}", if motd.is_empty() { String::new() } else { format!(" (\"{motd}\")") })),
            Err(e) => parts.push(format!("Minecraft port {port}: {e}")),
        }
    }
    if parts[0].starts_with("reachable") {
        match ssh(node, "uptime -p 2>/dev/null || uptime; free -m 2>/dev/null | awk 'NR==2{printf \"memory %s/%s MB used\\n\", $3, $2}'; df -h / 2>/dev/null | awk 'NR==2{print \"disk \" $5 \" used of \" $2}'; (vcgencmd measure_temp 2>/dev/null || cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null)").await {
            Ok(out) => parts.push(out.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| {
                // Raw millidegrees from the thermal zone.
                l.parse::<f64>().ok().filter(|v| *v > 1000.0).map(|v| format!("temperature {:.1}°C", v / 1000.0)).unwrap_or_else(|| l.to_string())
            }).collect::<Vec<_>>().join("; ")),
            Err(e) => parts.push(format!("SSH: {e}")),
        }
    }
    parts.join(" · ")
}

pub fn is_tool(name: &str) -> bool {
    name == "manage_remote_node"
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "manage_remote_node",
        "description": "Stark hivemind: Sir's other machines (Raspberry Pi 5, Linux VPS, Minecraft server …) over SSH. register (name, host, ssh_user, kind, services), list, status (reachability, uptime, memory, disk, temperature; players online for Minecraft), start_service / stop_service / restart_service / service_status (systemd), logs (streams a service's journal into a live window on the PC), run (offload a command), remove.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["register", "list", "status", "start_service", "stop_service", "restart_service", "service_status", "logs", "run", "remove"]},
            "node_name": {"type": "string"},
            "host": {"type": "string"},
            "ssh_user": {"type": "string"},
            "ssh_port": {"type": "integer"},
            "ssh_key_path": {"type": "string"},
            "kind": {"type": "string", "enum": ["pi", "vps", "minecraft", "pc", "server"]},
            "services": {"type": "array", "items": {"type": "string"}},
            "service_name": {"type": "string"},
            "command": {"type": "string"}
        }, "required": ["action"]}
    }})
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let mesh = &tools.stark.hivemind;
    let action = str_arg(args, &["action"]).unwrap_or("list").to_lowercase();
    let name = str_arg(args, &["node_name", "name", "node"]).unwrap_or("").to_string();
    tools.stark.pulse(&["brain", "hivemind"], false);
    let node = || mesh.get(&name).ok_or_else(|| {
        let known: Vec<String> = mesh.list().into_iter().map(|n| n.name).collect();
        if known.is_empty() {
            format!("I have no node called '{name}'. Register it first (name, host, SSH user).")
        } else {
            format!("I have no node called '{name}'. Known nodes: {}.", known.join(", "))
        }
    });
    let service = || {
        str_arg(args, &["service_name", "service"])
            .and_then(clean_service)
            .or_else(|| mesh.get(&name).and_then(|n| n.services.first().cloned()))
            .ok_or_else(|| "Which service (systemd unit)?".to_string())
    };
    match action.as_str() {
        "register" | "add" | "update" => {
            let existing = mesh.get(&name).filter(|n| n.name.eq_ignore_ascii_case(&name));
            let mut n = existing.unwrap_or_default();
            n.name = if name.is_empty() { str_arg(args, &["host"]).unwrap_or("").to_string() } else { name.clone() };
            if let Some(h) = str_arg(args, &["host", "ip", "address"]) {
                n.host = h.to_string();
            }
            if let Some(u) = str_arg(args, &["ssh_user", "user", "username"]) {
                n.ssh_user = u.to_string();
            }
            if let Some(k) = str_arg(args, &["ssh_key_path", "ssh_key", "key"]) {
                n.ssh_key = k.to_string();
            }
            n.ssh_port = args.get("ssh_port").and_then(Value::as_u64).map(|p| p as u16).unwrap_or(if n.ssh_port == 0 { 22 } else { n.ssh_port });
            if let Some(k) = str_arg(args, &["kind", "type"]) {
                n.kind = k.to_lowercase();
            }
            if let Some(list) = args.get("services").and_then(Value::as_array) {
                n.services = list.iter().filter_map(|v| v.as_str().and_then(clean_service)).collect();
            }
            if n.kind == "minecraft" && n.game_port == 0 {
                n.game_port = 25565;
            }
            mesh.save(&n)?;
            info!("[HIVEMIND] Registered node {}", n.describe());
            Ok(format!("Registered {}. JAI logs in with SSH keys (no passwords): run `ssh-copy-id {}` once if he can't.", n.describe(), n.target()))
        }
        "remove" | "delete" | "forget" => {
            let n = node()?;
            mesh.remove(&n.name);
            Ok(format!("Removed {} from the hivemind.", n.name))
        }
        "status" | "check" | "ping" => {
            let targets: Vec<RemoteNode> = if name.is_empty() || name == "all" { mesh.list() } else { vec![node()?] };
            if targets.is_empty() {
                let tunnel = crate::worldwide_tunnel::snapshot();
                return Ok(format!("No SSH nodes registered yet. Mobile worldwide link: {} — {}. Use show_mobile_satellite_instructions for the pairing QR code.", tunnel.state, tunnel.detail));
            }
            let mut lines = Vec::new();
            for n in targets {
                let s = status(&n).await;
                mesh.note_status(&n.name, if s.starts_with("reachable") { "online" } else { "offline" });
                lines.push(format!("{}: {s}", n.name));
            }
            let tunnel = crate::worldwide_tunnel::snapshot();
            lines.push(format!("Worldwide mobile satellite: {} — {}", tunnel.state, tunnel.detail));
            Ok(lines.join("\n"))
        }
        "start_service" | "stop_service" | "restart_service" | "service_status" => {
            let n = node()?;
            let svc = service()?;
            let verb = action.trim_end_matches("_service").trim_start_matches("service_");
            let command = if verb == "status" {
                format!("systemctl is-active {svc}; systemctl status {svc} --no-pager -n 5 2>/dev/null | head -n 12")
            } else {
                format!("sudo -n systemctl {verb} {svc} && systemctl is-active {svc}")
            };
            let out = ssh(&n, &command).await?;
            mesh.note_status(&n.name, "online");
            let done = match verb {
                "start" => "Started",
                "stop" => "Stopped",
                "restart" => "Restarted",
                _ => "Status of",
            };
            Ok(format!("{done} {svc} on {}: {}", n.name, out.trim()))
        }
        "logs" | "stream_logs" | "tail" => {
            let n = node()?;
            let svc = service().ok();
            let command = match &svc {
                Some(s) => format!("journalctl -u {s} -n 80 --no-pager 2>/dev/null || sudo -n journalctl -u {s} -n 80 --no-pager"),
                None => "journalctl -n 80 --no-pager 2>/dev/null || tail -n 80 /var/log/syslog".to_string(),
            };
            let first = ssh(&n, &command).await?;
            let id = format!("logs_{}", n.name.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "_"));
            let title = format!("Logs · {}{}", n.name, svc.as_ref().map(|s| format!(" · {s}")).unwrap_or_default());
            if crate::gui::windows_supported() {
                tools.widgets.spawn_window(&json!({ "id": id, "title": title, "code": first, "language": "log", "replace_content": true, "width": 760, "height": 480 }))?;
                crate::gui::sync_windows();
                stream_logs(tools.clone(), n.clone(), command, id.clone());
            }
            Ok(format!("Streaming {}'s {} into a live window on your PC (refreshes every 5 seconds). Latest lines:\n{}", n.name, svc.unwrap_or_else(|| "system log".into()), first.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")))
        }
        "run" | "offload" | "exec" => {
            let n = node()?;
            let command = str_arg(args, &["command", "cmd"]).ok_or("Which command should the node run?")?;
            if let Some(why) = crate::shell::blocked_reason(command) {
                return Err(format!("I won't run that on {} ({why}).", n.name));
            }
            let out = ssh(&n, command).await?;
            mesh.note_status(&n.name, "online");
            Ok(format!("{} ran `{command}`:\n{}", n.name, out.trim()))
        }
        _ => {
            let all = mesh.list();
            if all.is_empty() {
                Ok("No remote nodes yet. Register one: \"add my Raspberry Pi at 192.168.1.50, user pi\".".into())
            } else {
                Ok(format!("Hivemind nodes:\n{}", all.iter().map(|n| format!("- {}", n.describe())).collect::<Vec<_>>().join("\n")))
            }
        }
    }
}

/// Keeps a log window current until Sir closes it.
fn stream_logs(tools: ToolContext, node: RemoteNode, command: String, window: String) {
    tokio::spawn(async move {
        let started = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if started.elapsed() > LOG_STREAM_MAX || tools.widgets.find_window_named(&window).is_none() {
                break;
            }
            match ssh(&node, &command).await {
                Ok(text) => {
                    if tools.widgets.update_window(&json!({ "id": window, "code": text, "language": "log", "replace_content": true })).is_err() {
                        break;
                    }
                    crate::gui::refresh();
                    tools.stark.pulse(&["hivemind", "windows"], false);
                }
                Err(e) => warn!("[HIVEMIND] Log stream from {}: {e}", node.name),
            }
        }
        info!("[HIVEMIND] Stopped following the logs of {}", node.name);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_register_and_resolve_fuzzily() {
        let mesh = Hivemind::in_memory();
        mesh.save(&RemoteNode { name: "Pi5".into(), kind: "pi".into(), host: "192.168.1.50".into(), ssh_port: 22, ssh_user: "pi".into(), ..Default::default() }).unwrap();
        mesh.save(&RemoteNode { name: "Craft".into(), kind: "minecraft".into(), host: "mc.example.org".into(), ssh_port: 22, game_port: 25565, ..Default::default() }).unwrap();
        assert_eq!(mesh.get("the raspberry pi").map(|n| n.name), Some("Pi5".into()));
        assert_eq!(mesh.get("my minecraft server").map(|n| n.name), Some("Craft".into()));
        assert_eq!(mesh.get("Pi5").unwrap().target(), "pi@192.168.1.50");
        assert!(mesh.remove("Pi5"));
        assert_eq!(clean_service("minecraft.service").as_deref(), Some("minecraft.service"));
        assert_eq!(clean_service("x; rm -rf /"), None);
        assert_eq!(varint(300), vec![0xac, 0x02]);
    }
}
