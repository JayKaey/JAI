//! The Minecraft Doctor: "why did my Minecraft crash?" Finds the newest `latest.log` / crash report
//! (vanilla launcher, Modrinth App, CurseForge, Prism / MultiMC instances, or a path Sir names),
//! recognises the classic failures (out of memory, wrong Java, missing or clashing mods, mixin
//! errors, port in use, lag), names the culprit mod and the fix, and lets the model explain the
//! rest. It also pings a server (players, version), sends RCON commands, and runs SSH commands.

use crate::features::{read_document, resolve_path, run, show_window, spoken, str_arg};
use crate::tools::ToolContext;
use regex::Regex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One recognised problem.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub problem: String,
    pub fix: String,
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
}

re!(CLASS_VERSION, r"class file version (\d+)(?:\.\d+)?");
re!(MIXIN, r"(?i)Mixin (?:apply|transformation) (?:for mod |of )?(\S+?)(?: failed|:)|from mod (\w[\w-]*)");
re!(REQUIRES, r"(?im)Mod '([^']+)' \(([\w-]+)\) \S+ requires (?:version )?(.*?)(?:of )?mod '?([\w .-]+?)'?(?: \(|,|!|$)");
re!(MISSING_FORGE, r"(?i)Missing or unsupported mandatory dependencies:\s*\n?\s*Mod ID: '?([\w-]+)'?");
re!(DUPLICATE, r"(?i)duplicate mods?(?: found)?:?\s*(.+)");
re!(SUSPECTED, r"(?i)Suspected Mods?:\s*(.+)");
re!(DESCRIPTION, r"(?m)^Description: (.+)$");
re!(CANT_KEEP_UP, r"Can't keep up! Is the server overloaded\? Running (\d+)ms or (\d+) ticks behind");

/// Java major version of a class-file version (61 → 17, 65 → 21).
fn java_of(class_version: u32) -> u32 {
    class_version.saturating_sub(44)
}

/// The classic failures in a log or crash report.
pub fn analyse(log: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut push = |problem: String, fix: String| {
        if !out.iter().any(|f: &Finding| f.problem == problem) {
            out.push(Finding { problem, fix });
        }
    };
    if log.contains("java.lang.OutOfMemoryError") || log.contains("OutOfMemoryError: Java heap space") {
        push("The game ran out of memory (OutOfMemoryError).".into(), "Give it more RAM: set -Xmx6G (or 8G for big modpacks) in the launcher's Java arguments, and close RAM-hungry apps.".into());
    }
    if let Some(c) = CLASS_VERSION.captures(log) {
        if log.contains("UnsupportedClassVersionError") {
            let v: u32 = c[1].parse().unwrap_or(0);
            push(format!("A mod or the server needs Java {} (class file {v}).", java_of(v)), format!("Switch the instance to Java {} (Adoptium Temurin) in the launcher settings.", java_of(v)));
        }
    }
    for c in REQUIRES.captures_iter(log).take(5) {
        let needs = c[4].trim();
        let version = c.get(3).map(|v| v.as_str().trim()).filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("any version")).map(|v| format!(" {v}")).unwrap_or_default();
        push(format!("{} needs {needs}{version}, which is missing or the wrong version.", c[1].trim()), format!("Install or update {needs} to the required version."));
    }
    for c in MISSING_FORGE.captures_iter(log).take(5) {
        push(format!("A mandatory dependency is missing: {}.", &c[1]), format!("Download {} for this Minecraft version and loader.", &c[1]));
    }
    if let Some(c) = DUPLICATE.captures(log) {
        push(format!("Duplicate mods: {}", c[1].trim().chars().take(160).collect::<String>()), "Delete the older copy from the mods folder.".into());
    }
    let mixin_mod = MIXIN.captures_iter(log).find_map(|c| c.get(1).or_else(|| c.get(2)).map(|m| m.as_str().trim_matches(['\'', ':', '.']).to_string())).filter(|m| !m.is_empty());
    if log.contains("MixinApplyError") || log.contains("Mixin apply failed") || log.contains("MixinTransformerError") {
        let m = mixin_mod.clone().unwrap_or_else(|| "one mod".into());
        push(format!("A mixin from {m} failed to apply — a mod conflict or a version mismatch."), format!("Update {m}, or remove it and start again to confirm; check for a second mod that patches the same code."));
    }
    if let Some(c) = SUSPECTED.captures(log) {
        let s = c[1].trim();
        if !s.to_lowercase().starts_with("none") && !s.to_lowercase().contains("unknown") {
            push(format!("The crash report suspects: {s}"), format!("Update or remove {s}."));
        }
    }
    if log.contains("FAILED TO BIND TO PORT") || log.contains("Address already in use") {
        push("The server's port is already in use.".into(), "Another server (or a stuck java.exe) is running: stop it, or change server-port in server.properties.".into());
    }
    let lag: Vec<u64> = CANT_KEEP_UP.captures_iter(log).filter_map(|c| c[1].parse().ok()).collect();
    if lag.len() >= 3 {
        push(format!("The server fell behind {} times (worst {} ms).", lag.len(), lag.iter().max().unwrap_or(&0)), "Lower view-distance / simulation-distance, pre-generate the world (Chunky), and add Lithium / FerriteCore.".into());
    }
    if log.contains("Pixel format not accelerated") || log.contains("GLFW error 65542") || log.contains("WGL: The driver does not appear to support OpenGL") {
        push("The graphics driver could not create an OpenGL context.".into(), "Update the GPU driver and make sure Java runs on the dedicated GPU (Windows Graphics settings → javaw.exe → High performance).".into());
    }
    if log.contains("Exception in server tick loop") {
        push("The server crashed inside the tick loop.".into(), "See the stack trace below; the first mod package in it is the usual culprit.".into());
    }
    out
}

/// The crash report's own one-line description.
pub fn description(log: &str) -> Option<String> {
    DESCRIPTION.captures(log).map(|c| c[1].trim().to_string())
}

/// Every Minecraft instance folder a launcher has made on this machine (the configured one
/// first).
fn instance_roots(tools: &ToolContext) -> Vec<PathBuf> {
    instance_roots_from(&tools.config.read(|c| c.features.minecraft_dir.clone()))
}

/// The same from the configured folder (empty: the launchers' own places only).
pub(crate) fn instance_roots_from(configured: &str) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if !configured.trim().is_empty() {
        roots.push(PathBuf::from(crate::apps::expand_env(configured.trim())));
    }
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    let home = crate::apps::home_dir();
    if let Some(a) = &appdata {
        roots.push(a.join(".minecraft"));
        for launcher in ["ModrinthApp\\profiles", "PrismLauncher\\instances", ".technic\\modpacks", "com.modrinth.theseus\\profiles"] {
            if let Ok(rd) = std::fs::read_dir(a.join(launcher)) {
                roots.extend(rd.flatten().map(|e| e.path()).flat_map(|p| [p.join(".minecraft"), p]));
            }
        }
    }
    if let Some(h) = &home {
        for launcher in ["curseforge\\minecraft\\Instances", "AppData\\Roaming\\.feather"] {
            if let Ok(rd) = std::fs::read_dir(h.join(launcher)) {
                roots.extend(rd.flatten().map(|e| e.path()));
            }
        }
    }
    roots.retain(|r| r.is_dir());
    roots
}

/// The newest crash report of any instance and when it was written (the butler watches for a
/// new one).
pub(crate) fn newest_crash_report(tools: &ToolContext) -> Option<(PathBuf, SystemTime)> {
    instance_roots(tools)
        .iter()
        .filter_map(|root| std::fs::read_dir(root.join("crash-reports")).ok())
        .flatten()
        .flatten()
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .max_by_key(|(_, written)| *written)
}

/// The newest log or crash report in every known Minecraft location.
fn newest_log(tools: &ToolContext) -> Option<PathBuf> {
    let mut candidates: Vec<(PathBuf, SystemTime)> = Vec::new();
    for root in &instance_roots(tools) {
        let latest = root.join("logs").join("latest.log");
        if let Ok(t) = std::fs::metadata(&latest).and_then(|m| m.modified()) {
            candidates.push((latest, t));
        }
        if let Ok(rd) = std::fs::read_dir(root.join("crash-reports")) {
            candidates.extend(rd.flatten().filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?))));
        }
    }
    candidates.sort_by_key(|(_, t)| *t);
    candidates.pop().map(|(p, _)| p)
}

/// The error lines and the tail of a log (what the model gets to read).
fn excerpt(log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    let errors: Vec<&str> = lines.iter().copied().filter(|l| l.contains("ERROR") || l.contains("Exception") || l.contains("Caused by") || l.contains("FATAL")).take(40).collect();
    let tail = lines[lines.len().saturating_sub(80)..].join("\n");
    format!("Error lines:\n{}\n\nLast lines:\n{tail}", errors.join("\n"))
}

async fn diagnose(tools: &ToolContext, given: Option<&str>) -> Result<String, String> {
    let path = match given.and_then(resolve_path) {
        Some(p) if p.is_dir() => {
            let crash = std::fs::read_dir(p.join("crash-reports")).ok().and_then(|rd| rd.flatten().max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok()).map(|e| e.path()));
            crash.unwrap_or_else(|| p.join("logs").join("latest.log"))
        }
        Some(p) => p,
        None => newest_log(tools).ok_or("I couldn't find any Minecraft log, Sir. Tell me the instance folder or the log file.")?,
    };
    let log = read_document(&path).await?;
    let findings = analyse(&log);
    let desc = description(&log);
    let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs() / 60).unwrap_or(0);
    let explanation = if findings.is_empty() {
        tools
            .pim
            .complete("You are a Minecraft modding expert. From this log excerpt, name the most likely cause of the crash or failure (the culprit mod if any) and the concrete fix, in 3-5 sentences. English.", &excerpt(&log), 320)
            .await
    } else {
        None
    };
    let mut md = format!("## Minecraft Doctor\n`{}` ({} min old){}\n\n", path.display(), age, desc.as_ref().map(|d| format!("\n**Crash:** {d}")).unwrap_or_default());
    for f in &findings {
        md.push_str(&format!("- **{}**\n  Fix: {}\n", f.problem, f.fix));
    }
    if let Some(e) = &explanation {
        md.push_str(&format!("\n{e}\n"));
    }
    if findings.is_empty() && explanation.is_none() {
        md.push_str("No known failure pattern found, and the language model is not available.\n");
    }
    show_window(tools, "minecraft_doctor", "Minecraft Doctor", &md, 700.0, 560.0);
    let said = match (findings.first(), &explanation) {
        (Some(f), _) => format!("{} {}{}", f.problem, f.fix, if findings.len() > 1 { format!(" I found {} more issues; they're on screen.", findings.len() - 1) } else { String::new() }),
        (None, Some(e)) => e.split(". ").take(2).collect::<Vec<_>>().join(". "),
        (None, None) => "The log shows no known failure, Sir. The details are on screen.".into(),
    };
    Ok(spoken(md, said))
}

// ------------------------------------------------------------------ server protocols ----

fn varint(mut v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut b = (v & 0x7F) as u8;
        v = ((v as u32) >> 7) as i32;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            return out;
        }
    }
}

async fn read_varint(s: &mut tokio::net::TcpStream) -> Result<i32, String> {
    let mut result = 0i32;
    for i in 0..5 {
        let b = s.read_u8().await.map_err(|e| e.to_string())?;
        result |= ((b & 0x7F) as i32) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(result);
        }
    }
    Err("bad VarInt".into())
}

/// Server List Ping: the server's status JSON (players, version, MOTD).
pub async fn server_status(address: &str) -> Result<Value, String> {
    let (host, port) = match address.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(25565)),
        None => (address.to_string(), 25565),
    };
    let mut s = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect((host.as_str(), port))).await.map_err(|_| format!("{address} did not answer."))?.map_err(|e| format!("{address}: {e}"))?;
    let mut hs = varint(0x00);
    hs.extend(varint(767));
    hs.extend(varint(host.len() as i32));
    hs.extend(host.as_bytes());
    hs.extend(port.to_be_bytes());
    hs.extend(varint(1));
    let mut packet = varint(hs.len() as i32);
    packet.extend(hs);
    packet.extend([0x01, 0x00]);
    s.write_all(&packet).await.map_err(|e| e.to_string())?;
    let read = async {
        let _len = read_varint(&mut s).await?;
        let _id = read_varint(&mut s).await?;
        let n = read_varint(&mut s).await? as usize;
        let mut buf = vec![0u8; n.min(1 << 20)];
        s.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
        serde_json::from_slice::<Value>(&buf).map_err(|e| e.to_string())
    };
    tokio::time::timeout(Duration::from_secs(5), read).await.map_err(|_| "The server did not send its status.".to_string())?
}

/// A Minecraft RCON command (login, then the command; the reply text).
pub async fn rcon(address: &str, password: &str, command: &str) -> Result<String, String> {
    let mut s = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(address)).await.map_err(|_| format!("RCON at {address} did not answer."))?.map_err(|e| format!("RCON {address}: {e}"))?;
    async fn send(s: &mut tokio::net::TcpStream, id: i32, kind: i32, body: &str) -> Result<(i32, String), String> {
        let mut p = Vec::new();
        p.extend(id.to_le_bytes());
        p.extend(kind.to_le_bytes());
        p.extend(body.as_bytes());
        p.extend([0, 0]);
        let mut framed = (p.len() as i32).to_le_bytes().to_vec();
        framed.extend(p);
        s.write_all(&framed).await.map_err(|e| e.to_string())?;
        let len = s.read_i32_le().await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; len.clamp(0, 1 << 20) as usize];
        s.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
        let rid = i32::from_le_bytes(buf[0..4].try_into().unwrap_or([0; 4]));
        let text = String::from_utf8_lossy(&buf[8..buf.len().saturating_sub(2)]).to_string();
        Ok((rid, text))
    }
    let (rid, _) = tokio::time::timeout(Duration::from_secs(5), send(&mut s, 7, 3, password)).await.map_err(|_| "RCON login timed out.".to_string())??;
    if rid == -1 {
        return Err("RCON refused the password.".into());
    }
    let (_, reply) = tokio::time::timeout(Duration::from_secs(8), send(&mut s, 8, 2, command.trim_start_matches('/'))).await.map_err(|_| "RCON command timed out.".to_string())??;
    // Strip Minecraft colour codes.
    let clean: String = {
        let mut out = String::new();
        let mut skip = false;
        for c in reply.chars() {
            if skip {
                skip = false;
            } else if c == '§' {
                skip = true;
            } else {
                out.push(c);
            }
        }
        out
    };
    Ok(clean)
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let action = str_arg(args, &["action"]).unwrap_or("diagnose").to_lowercase();
    let arg = str_arg(args, &["log_path_or_command", "command", "path"]);
    let settings = tools.config.read(|c| c.features.clone());
    match action.as_str() {
        "status" | "ping" | "players" => {
            let address = arg.filter(|a| a.contains('.') || a.contains(':')).map(String::from).unwrap_or(settings.minecraft_server.clone());
            if address.trim().is_empty() {
                return Err("Which server, Sir? Set features.minecraft_server (host:port) or name it.".into());
            }
            let v = server_status(&address).await?;
            let online = v["players"]["online"].as_i64().unwrap_or(0);
            let max = v["players"]["max"].as_i64().unwrap_or(0);
            let names: Vec<String> = v["players"]["sample"].as_array().map(|a| a.iter().filter_map(|p| p["name"].as_str().map(String::from)).collect()).unwrap_or_default();
            let version = v["version"]["name"].as_str().unwrap_or("?").to_string();
            Ok(spoken(
                format!("{address}: online · {version} · {online}/{max} players{}", if names.is_empty() { String::new() } else { format!(" ({})", names.join(", ")) }),
                format!("The server is up, Sir: {online} of {max} players{}.", if names.is_empty() { String::new() } else { format!(", including {}", names.join(", ")) }),
            ))
        }
        "rcon" | "command" => {
            let command = arg.ok_or("Which server command, Sir?")?;
            if settings.rcon_address.trim().is_empty() || settings.rcon_password.is_empty() {
                return Err("RCON is not set up: add features.rcon_address and features.rcon_password to jai_config.json (and enable-rcon in server.properties).".into());
            }
            let reply = rcon(&settings.rcon_address, &settings.rcon_password, command).await?;
            Ok(spoken(format!("/{} → {}", command.trim_start_matches('/'), if reply.is_empty() { "(no output)" } else { &reply }), if reply.is_empty() { "Done, Sir.".to_string() } else { reply.chars().take(240).collect() }))
        }
        "ssh" => {
            let command = arg.ok_or("Which command shall I run on the server, Sir?")?;
            if settings.ssh_target.trim().is_empty() {
                return Err("No SSH target: set features.ssh_target (user@host) — key-based login only.".into());
            }
            let ssh = crate::features::find_tool("ssh").ok_or("The OpenSSH client is not installed.")?;
            let out = run(&ssh, &["-o", "BatchMode=yes", "-o", "ConnectTimeout=8", &settings.ssh_target, command], None, Duration::from_secs(40)).await?;
            let text = format!("{}{}", out.stdout, out.stderr);
            let findings = analyse(&text);
            let mut md = format!("$ {command}\n```\n{}\n```", text.chars().rev().take(4000).collect::<String>().chars().rev().collect::<String>());
            for f in &findings {
                md.push_str(&format!("\n- **{}** Fix: {}", f.problem, f.fix));
            }
            let said = findings.first().map(|f| format!("{} {}", f.problem, f.fix)).unwrap_or_else(|| if out.ok { "Done, Sir. The output is on screen.".into() } else { format!("The command failed: {}", out.error_line()) });
            show_window(tools, "minecraft_doctor", "Minecraft Doctor — SSH", &md, 720.0, 520.0);
            Ok(spoken(md, said))
        }
        _ => diagnose(tools, arg).await,
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "diagnose_minecraft_or_server",
        "description": "Minecraft Doctor: action diagnose (read the newest latest.log / crash report — or the path given — and name the cause, culprit mod and fix), status (server ping: players, version), rcon (run a server command), ssh (run a shell command on the server host).",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["diagnose", "status", "rcon", "ssh"]},
            "log_path_or_command": {"type": "string", "description": "Log/crash file or instance folder (diagnose), host:port (status), or the command (rcon/ssh)"}
        }, "required": ["action"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_classic_failures() {
        let oom = "[Render thread/ERROR]: java.lang.OutOfMemoryError: Java heap space";
        assert!(analyse(oom)[0].fix.contains("-Xmx"));
        let java = "java.lang.UnsupportedClassVersionError: net/minecraft/Main has been compiled by a more recent version of the Java Runtime (class file version 65.0)";
        assert!(analyse(java)[0].problem.contains("Java 21"));
        let fabric = "Mod resolution encountered an incompatible mod set!\n - Mod 'Sodium Extra' (sodium-extra) 0.5.4 requires version 0.5.8 or later of mod 'Sodium' (sodium), which is missing!";
        let f = analyse(fabric);
        assert!(f.iter().any(|x| x.problem.starts_with("Sodium Extra needs")), "{f:?}");
        let crash = "---- Minecraft Crash Report ----\nDescription: Ticking entity\n\nSuspected Mods: Create (create)\n";
        assert_eq!(description(crash).as_deref(), Some("Ticking entity"));
        assert!(analyse(crash).iter().any(|x| x.problem.contains("Create")));
        assert_eq!(varint(300), vec![0xAC, 0x02]);
        assert_eq!(java_of(61), 17);
    }
}
