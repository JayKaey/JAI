//! Four PC questions Sir asks J.A.I. (2026-11-16), answered from Windows itself:
//! - which app hangs ("is anything frozen?", IsHungAppWindow),
//! - who holds a port or a file ("who is using port 8080", GetExtendedTcpTable / the Restart
//!   Manager),
//! - what starts with Windows (the Run keys and Startup folders with Task Manager's on/off),
//! - whether Windows waits for a restart (the servicing keys), told once, best as a game starts.
//!
//! On Linux the same questions read `/proc` (ports, open files), the XDG autostart folders and
//! `/var/run/reboot-required`; frozen apps are the windows KWin marks "Not Responding", app
//! updates come from `checkupdates` and Flatpak (Discover installs them); the microphone check
//! tells what PipeWire shows but cannot meter the input.
//!
//! Nothing is ended or switched off on the question: he is asked (`hud_lines::offer`), and only
//! his yes carries out the order the offer holds ("end process 4321 discord").

use crate::kit::say;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

#[cfg(windows)]
use crate::win32::process_name;

/// The system the answers name ("what starts with Linux").
const OS: &str = if cfg!(windows) { "Windows" } else { "Linux" };

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid PC phrase")
}

static FROZEN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:is (?:anything|something|an app|a program|an application) (?:frozen|hanging|stuck|not responding)|what(?:'s|s| s| is) (?:frozen|hanging|stuck|not responding)|which (?:app|program|window|application) (?:is )?(?:frozen|hanging|stuck|not responding)|hängt (?:da )?(?:etwas|was|ein programm)|was hängt(?: da)?|welches programm (?:hängt|reagiert nicht))$")
});
static PORT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:who|what|which (?:app|program))(?:'s|s| s| is)? (?:using|on|holding|blocking|listening on) port (?P<p>\d{1,5})$|^wer (?:benutzt|blockiert|belegt|hat|hört auf) (?:den )?port (?P<p2>\d{1,5})$|^was (?:läuft|lauscht) auf port (?P<p3>\d{1,5})$")
});
static FILE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:who|what|which (?:app|program))(?: still)? (?:has|keeps) (?P<f>.+?) open$|^(?:who|what|which (?:app|program))(?:'s|s| s| is) (?:using|locking|blocking|holding) (?:the file )?(?P<f2>.+?\.\w{1,5})$|^wer hat (?P<f3>.+?) (?:noch )?offen$|^welches programm (?:blockiert|benutzt|sperrt) (?:die datei )?(?P<f4>.+?\.\w{1,5})$")
});
static STARTUP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:what|which (?:apps|programs)) (?:starts?|runs?|opens?|launch(?:es)?) (?:with (?:windows|linux)|at (?:startup|boot|sign in|login))$|^(?:show|list) (?:me )?(?:my )?(?:startup|autostart) (?:apps|programs|entries|items)$|^was startet (?:alles )?(?:mit (?:windows|linux)|beim (?:hochfahren|start|anmelden))$|^zeig(?:e)? (?:mir )?(?:meine |die )?autostart(?:[\s-]?programme|[\s-]?einträge)?$")
});
static STARTUP_OFF: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:stop|disable|switch off|turn off) (?P<n>.+?) (?:from )?(?:starting with (?:windows|linux)|at startup|on startup|in autostart|at boot)$|^(?:don't|do not) start (?P<n2>.+?) with (?:windows|linux)$|^(?P<n3>.+?) soll nicht mehr (?:mit (?:windows|linux)|automatisch) starten$|^nimm (?P<n4>.+?) aus dem autostart$")
});
static STARTUP_OFF_ORDER: LazyLock<Regex> = LazyLock::new(|| re(r"^switch off startup entry (?P<n>.+)$"));
static REBOOT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:does|do) (?:windows|linux|my pc|the pc|my computer) (?:need|want) (?:a |to )?restart$|^is a restart (?:pending|needed|due)$|^(?:muss|soll) (?:ich (?:den pc |windows )?|der pc |windows )?neu ?(?:starten|gestartet werden)$")
});
static END: LazyLock<Regex> = LazyLock::new(|| re(r"^end process (?P<pid>\d{1,7}) (?P<name>[\w.-]+)$"));
static UPDATES: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:are there |any )?(?:app |program |software )?(?:updates|upgrades) (?:for|of) (?:my )?(?:apps|programs|software)$|^(?:check|look) for (?:app |program |software )?updates$|^(?:which|what) (?:apps|programs) (?:can|need to|should) (?:be )?(?:updated|upgraded)$|^update (?:my |all )?(?:apps|programs)$|^gibt es updates für (?:meine )?(?:apps|programme)$|^(?:aktualisiere|update) (?:meine |alle )?(?:apps|programme)$")
});
static UPGRADE_ORDER: LazyLock<Regex> = LazyLock::new(|| re(r"^upgrade all apps with (?:winget|discover)$"));
static MIC: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:my |the )?(?:mic|microphone) (?:doesn'?t|doesn t|does not|isn'?t|isn t|is not|won'?t|won t) (?:work|working|work anymore)$|^(?:check|test) (?:my |the )?(?:mic|microphone)$|^(?:what(?:'s|s| s| is) wrong with|is something wrong with) (?:my |the )?(?:mic|microphone)$|^(?:mein |das )?(?:mikro|mikrofon|mic) (?:geht|funktioniert) nicht(?: mehr)?$|^(?:prüf|prüfe|teste?) (?:mein |das )?(?:mikro|mikrofon)$")
});

/// An order about the PC: the tool's arguments.
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim();
    let pick = |c: &regex::Captures, names: &[&str]| names.iter().find_map(|n| c.name(n)).map(|m| m.as_str().trim().to_string());
    if FROZEN.is_match(s) {
        return Some(json!({"action": "frozen"}));
    }
    if let Some(c) = PORT.captures(s) {
        let port: u16 = pick(&c, &["p", "p2", "p3"])?.parse().ok()?;
        return Some(json!({"action": "port", "port": port}));
    }
    if let Some(c) = FILE.captures(s) {
        return Some(json!({"action": "file", "file": pick(&c, &["f", "f2", "f3", "f4"])?}));
    }
    if STARTUP.is_match(s) {
        return Some(json!({"action": "startup"}));
    }
    if let Some(c) = STARTUP_OFF_ORDER.captures(s) {
        return Some(json!({"action": "startup_off", "name": pick(&c, &["n"])?, "confirmed": true}));
    }
    if let Some(c) = STARTUP_OFF.captures(s) {
        let name = pick(&c, &["n", "n2", "n3", "n4"])?;
        // ("Stop yourself from starting with Windows" is his own switch: `hud_feel`.)
        if name.split_whitespace().next().is_some_and(|w| ["yourself", "you", "jai", "j.a.i.", "j.a.i", "jay", "dich"].contains(&w)) {
            return None;
        }
        return Some(json!({"action": "startup_off", "name": name}));
    }
    if REBOOT.is_match(s) {
        return Some(json!({"action": "reboot"}));
    }
    if UPDATES.is_match(s) {
        return Some(json!({"action": "updates"}));
    }
    if UPGRADE_ORDER.is_match(s) {
        return Some(json!({"action": "upgrade"}));
    }
    if MIC.is_match(s) {
        return Some(json!({"action": "mic"}));
    }
    let c = END.captures(s)?;
    Some(json!({"action": "end", "pid": pick(&c, &["pid"])?.parse::<u32>().ok()?, "name": pick(&c, &["name"])?}))
}

/// Whether the call only reads (a question may ask for it).
pub fn only_looks(args: &Value) -> bool {
    matches!(args["action"].as_str().unwrap_or(""), "frozen" | "port" | "file" | "startup" | "reboot" | "updates" | "mic")
}

/// The microphone checked from the system's side: the block's rows and the one line to say (the
/// first fault, in the order it would stop the sound). `blocked_for`: whom Windows' privacy
/// setting shuts out. (Linux's sound server keeps no meter on it: whether it hears is not said.)
pub fn mic_verdict(blocked_for: Option<&str>, mic: Option<&crate::audio_mixer::MicState>) -> (String, String) {
    let metered = cfg!(windows);
    let side = if metered { "Windows'" } else { "Linux's" };
    let yes_no = |b: bool, yes: &str, no: &str| if b { yes.to_string() } else { no.to_string() };
    let mut rows = vec![format!("Privacy   {}", blocked_for.map_or("allowed".to_string(), |w| format!("BLOCKED for {w}")))];
    if let Some(m) = mic {
        rows.push(format!("Device    {}", m.name));
        rows.push(format!("Muted     {}", yes_no(m.muted, "YES", "no")));
        rows.push(format!("Level     {:.0} %", m.level * 100.0));
        if metered {
            rows.push(format!("Hearing   {}", yes_no(m.peak > 0.0, &format!("yes (peak {:.2})", m.peak), "nothing at all")));
        }
    } else {
        rows.push("Device    NONE set as the default recording device".to_string());
    }
    let line = match (blocked_for, mic) {
        (Some(who), _) => format!("Windows' privacy setting blocks the microphone for {who}, Sir: Settings, Privacy, Microphone."),
        (None, None) => format!("No microphone is set as {side} default recording device, Sir."),
        (None, Some(m)) if m.muted => format!("{} is muted in {OS}, Sir.", m.name),
        (None, Some(m)) if m.level < 0.1 => format!("{}'s level is down at {:.0} percent, Sir.", m.name, m.level * 100.0),
        (None, Some(m)) if metered && m.peak <= 0.0 => format!("{} hears nothing at all, Sir: it may be unplugged, switched off on the device, or muted in its own software.", m.name),
        (None, Some(m)) => format!(
            "{side} side is fine, Sir: {} is on at {:.0} percent{}. If an app still hears nothing, check that app's own input setting.",
            m.name,
            m.level * 100.0,
            if metered { " and hears sound" } else { "" }
        ),
    };
    (rows.join("\n"), line)
}

/// Whom Windows' microphone privacy switch shuts out, if anyone (the PC's, his, desktop apps').
#[cfg(windows)]
fn mic_blocked_for() -> Option<&'static str> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    const STORE: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone";
    let denied = |root, path: &str| reg_text(root, path, "Value").is_some_and(|v| v.eq_ignore_ascii_case("Deny"));
    if denied(HKEY_LOCAL_MACHINE, STORE) {
        Some("every account on this PC")
    } else if denied(HKEY_CURRENT_USER, STORE) {
        Some("every app")
    } else if denied(HKEY_CURRENT_USER, &format!("{STORE}\\NonPackaged")) {
        Some("desktop apps")
    } else {
        None
    }
}

#[cfg(windows)]
pub(crate) fn reg_text(root: windows::Win32::System::Registry::HKEY, path: &str, name: &str) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_SZ};
    let (path, name) = (wide(path), wide(name));
    let mut buf = [0u16; 64];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: a sized read into a local buffer.
    unsafe { RegGetValueW(root, PCWSTR(path.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) }.ok().ok()?;
    let chars = (size as usize / 2).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..chars]).trim_end_matches('\0').to_string())
}

/// `winget upgrade`'s table: (name, id, installed, available) per row. The columns are cut where
/// the header's words start (the header is in Windows' language), counted in characters.
#[cfg(windows)]
pub fn upgrade_rows(out: &str) -> Vec<(String, String, String, String)> {
    let lines: Vec<&str> = out.lines().map(|l| l.rsplit('\r').next().unwrap_or(l)).collect();
    let Some(dash) = lines.iter().position(|l| l.len() > 20 && l.trim().chars().all(|c| c == '-')) else { return Vec::new() };
    let Some(header) = dash.checked_sub(1).map(|i| lines[i].chars().collect::<Vec<char>>()) else { return Vec::new() };
    let starts: Vec<usize> = (0..header.len()).filter(|&i| header[i] != ' ' && (i == 0 || header[i - 1] == ' ')).collect();
    if starts.len() < 4 {
        return Vec::new();
    }
    let cell = |row: &[char], c: usize| -> String {
        let (from, to) = (starts[c], starts.get(c + 1).copied().unwrap_or(usize::MAX));
        row.get(from.min(row.len())..to.min(row.len())).map(|s| s.iter().collect::<String>().trim().to_string()).unwrap_or_default()
    };
    let mut rows = Vec::new();
    for line in &lines[dash + 1..] {
        let row: Vec<char> = line.chars().collect();
        let (name, id, from, to) = (cell(&row, 0), cell(&row, 1), cell(&row, 2), cell(&row, 3));
        if id.is_empty() || to.is_empty() || id.contains(' ') {
            break;
        }
        rows.push((name, id, from, to));
    }
    rows
}

/// The line for the update list: how many, the first three names, the question.
fn updates_line(rows: &[(String, String, String, String)]) -> String {
    let names: Vec<String> = rows.iter().take(3).map(|r| r.0.clone()).collect();
    let more = rows.len().saturating_sub(3);
    let list = if more > 0 { format!("{} and {more} more", names.join(", ")) } else { crate::briefing::join_and(&names) };
    match rows.len() {
        1 => format!("{list} has an update, Sir. Shall I install it{BACKGROUND}?"),
        n => format!("{} apps have updates, Sir: {list}. Shall I install them{BACKGROUND}?", crate::earth::count_word(n)),
    }
}

/// The order his yes carries out: the process by its id and its name (a name that no longer
/// matches the id is not ended: the id may have gone to another process meanwhile).
fn end_order(pid: u32, name: &str) -> String {
    format!("end process {pid} {}", name.split_whitespace().collect::<Vec<_>>().join("-"))
}

fn ask_to_end(pid: u32, name: &str, why: &str) -> String {
    crate::hud_lines::offer(&end_order(pid, name));
    let line = format!("{why} Shall I end {name}, Sir?");
    say(&line, &line)
}

pub async fn run(args: &Value) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("") {
        "frozen" => {
            let hung = hung_windows();
            match hung.as_slice() {
                [] => Ok(say("Nothing is frozen, Sir: every window answers.", "Nothing is frozen, Sir.")),
                [(pid, process, title)] => Ok(ask_to_end(*pid, process, &format!("{process} (\"{title}\") is not responding."))),
                many => {
                    let names: Vec<&str> = many.iter().map(|(_, p, _)| p.as_str()).collect();
                    let (pid, process, _) = &many[0];
                    Ok(ask_to_end(*pid, process, &format!("{} are not responding.", names.join(", "))))
                }
            }
        }
        "port" => {
            let port = args["port"].as_u64().filter(|p| *p > 0 && *p < 65536).ok_or("Which port, Sir?")? as u16;
            let owners = tokio::task::spawn_blocking(move || port_owners(port)).await.map_err(|e| e.to_string())?;
            match owners.first() {
                None => Ok(say(&format!("Nothing is using port {port}, Sir."), &format!("Nothing is using port {port}, Sir."))),
                Some(&pid) => {
                    let name = process_name(pid).unwrap_or_else(|| format!("process {pid}"));
                    if pid == std::process::id() {
                        return Ok(say(&format!("Port {port} is mine, Sir."), &format!("Port {port} is mine, Sir.")));
                    }
                    Ok(ask_to_end(pid, &name, &format!("{name} (process {pid}) holds port {port}.")))
                }
            }
        }
        "file" => {
            let raw = args["file"].as_str().ok_or("Which file, Sir?")?;
            let path = crate::file_ops::resolve_existing(raw, None).await?;
            let shown = path.display().to_string();
            let owners = tokio::task::spawn_blocking(move || file_owners(&path)).await.map_err(|e| e.to_string())??;
            match owners.first() {
                None => Ok(say(&format!("No program has {shown} open, Sir."), "No program has it open, Sir.")),
                Some((pid, app)) => {
                    let name = process_name(*pid).unwrap_or_else(|| app.clone());
                    Ok(ask_to_end(*pid, &name, &format!("{app} has {shown} open.")))
                }
            }
        }
        "end" => {
            let pid = args["pid"].as_u64().ok_or("Which process, Sir?")? as u32;
            let name = args["name"].as_str().unwrap_or("");
            match process_name(pid) {
                Some(now) if end_order(pid, &now) == end_order(pid, name) && pid != std::process::id() => {
                    end_process(pid)?;
                    Ok(say(&format!("Ended {now}, Sir."), &format!("Ended {now}, Sir.")))
                }
                Some(_) | None => Ok(say("That process has gone already, Sir.", "That process has gone already, Sir.")),
            }
        }
        "startup" => {
            let entries = tokio::task::spawn_blocking(startup_entries).await.map_err(|e| e.to_string())?;
            if entries.is_empty() {
                let line = format!("Nothing starts with {OS}, Sir.");
                return Ok(say(&line, &line));
            }
            let text: Vec<String> = entries.iter().map(|e| format!("{}  {}  ·  {}", if e.on { "ON " } else { "off" }, e.name, e.place)).collect();
            crate::kit::show_text("startup_apps", &format!("STARTS WITH {}", OS.to_uppercase()), &text.join("\n"), [420.0, 260.0]);
            let on = entries.iter().filter(|e| e.on).count();
            let line = format!("{on} of {} entries start with {OS}, Sir. Say \"stop <name> from starting with {OS}\" to switch one off.", entries.len());
            Ok(say(&line, &format!("{on} of {} entries start with {OS}, Sir.", entries.len())))
        }
        "startup_off" => {
            let said = args["name"].as_str().ok_or("Which entry, Sir?")?.to_string();
            let entries = tokio::task::spawn_blocking(startup_entries).await.map_err(|e| e.to_string())?;
            let entry = best_entry(&entries, &said).ok_or_else(|| format!("No startup entry is called {said}, Sir."))?;
            if !entry.on {
                return Ok(say(&format!("{} is switched off already, Sir.", entry.name), &format!("{} is off already, Sir.", entry.name)));
            }
            if !args["confirmed"].as_bool().unwrap_or(false) {
                crate::hud_lines::offer(&format!("switch off startup entry {}", entry.name));
                let line = format!("{} starts with {OS} ({}). Shall I switch it off, Sir?", entry.name, entry.place);
                return Ok(say(&line, &line));
            }
            let entry = entry.clone();
            tokio::task::spawn_blocking(move || switch_off(&entry)).await.map_err(|e| e.to_string())??;
            let how = if cfg!(windows) { "Task Manager's switch" } else { "hidden in your autostart" };
            Ok(say(&format!("Done, Sir: {said} no longer starts with {OS} ({how}, nothing deleted)."), "Done, Sir."))
        }
        "reboot" => Ok(match restart_pending() {
            true => say(&format!("Yes, Sir: {OS} has finished an update and waits for a restart."), &format!("Yes, Sir: {OS} waits for a restart.")),
            false => say(&format!("No, Sir: {OS} waits for no restart."), "No, Sir."),
        }),
        "mic" => {
            #[cfg(windows)]
            let blocked = tokio::task::spawn_blocking(mic_blocked_for).await.map_err(|e| e.to_string())?;
            #[cfg(not(windows))]
            let blocked = None;
            let mic = crate::audio_mixer::microphone_state().await;
            let (rows, line) = mic_verdict(blocked, mic.as_ref());
            crate::kit::show_text("mic_check", "MICROPHONE", &rows, [420.0, 170.0]);
            Ok(say(&line, &line))
        }
        "updates" => {
            let rows = app_updates().await?;
            if rows.is_empty() {
                return Ok(say("Every app is up to date, Sir.", "Every app is up to date, Sir."));
            }
            let text: Vec<String> = rows.iter().map(|(name, _, from, to)| format!("{name}  {from} → {to}")).collect();
            crate::kit::show_text("app_updates", UPDATES_TITLE, &text.join("
"), [420.0, 60.0 + 22.0 * rows.len().min(16) as f32]);
            crate::hud_lines::offer(UPGRADE_ORDER_TEXT);
            let line = updates_line(&rows);
            Ok(say(&line, &line))
        }
        "upgrade" => upgrade_all(),
        other => Err(format!("The PC helper has no action '{other}'.")),
    }
}

#[cfg(windows)]
const UPDATES_TITLE: &str = "UPDATES // WINGET";
#[cfg(not(windows))]
const UPDATES_TITLE: &str = "UPDATES // PACMAN, FLATPAK";
/// Windows installs behind his back; Discover opens a window of its own.
const BACKGROUND: &str = if cfg!(windows) { " in the background" } else { "" };
const UPGRADE_ORDER_TEXT: &str = if cfg!(windows) { "upgrade all apps with winget" } else { "upgrade all apps with discover" };

/// (name, id, installed, available) of every app with an update.
#[cfg(windows)]
async fn app_updates() -> Result<Vec<(String, String, String, String)>, String> {
    let winget = crate::apps::which("winget").ok_or("winget is not on this PC, Sir.")?;
    let out = crate::shell::run_helper(&winget, ["upgrade", "--accept-source-agreements", "--disable-interactivity"], None, &[], std::time::Duration::from_secs(120)).await.map_err(|e| format!("winget did not run, Sir: {e}"))?;
    Ok(upgrade_rows(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(windows)]
fn upgrade_all() -> Result<String, String> {
    let winget = crate::apps::which("winget").ok_or("winget is not on this PC, Sir.")?;
    tokio::spawn(async move {
        let args = ["upgrade", "--all", "--silent", "--accept-source-agreements", "--accept-package-agreements", "--disable-interactivity"];
        let line = match crate::shell::run_helper(&winget, args, None, &[], std::time::Duration::from_secs(3600)).await {
            Ok(out) if out.code == Some(0) => "The app updates are installed, Sir.".to_string(),
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                let last = text.lines().rev().map(|l| l.rsplit('\r').next().unwrap_or(l).trim()).find(|l| !l.is_empty()).unwrap_or("no reason given").to_string();
                format!("Some app updates failed, Sir: {last}")
            }
            Err(e) => format!("winget did not run, Sir: {e}"),
        };
        crate::announcements::say(line);
    });
    Ok(say("Installing the updates in the background, Sir. I'll tell you when they're done.", "Installing them in the background, Sir."))
}

// ------------------------------------------------------------------------- frozen apps ----

/// (pid, process, title) of every application window that has stopped answering, his own aside.
#[cfg(windows)]
fn hung_windows() -> Vec<(u32, String, String)> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::IsHungAppWindow;
    let mut out: Vec<(u32, String, String)> = Vec::new();
    for w in crate::win32::app_windows() {
        // SAFETY: a plain query on a handle from the enumeration.
        if !crate::win32::is_jai_window(&w) && unsafe { IsHungAppWindow(HWND(w.hwnd)) }.as_bool() && !out.iter().any(|(pid, _, _)| *pid == w.pid) {
            out.push((w.pid, w.process, w.title));
        }
    }
    out
}

#[cfg(windows)]
fn end_process(pid: u32) -> Result<(), String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    // SAFETY: the handle is checked and closed.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, false, pid).map_err(|e| format!("I may not end that process, Sir ({e})."))?;
        let ended = TerminateProcess(handle, 1);
        let _ = CloseHandle(handle);
        ended.map_err(|e| format!("Windows refused to end it, Sir ({e})."))
    }
}

// ------------------------------------------------------------------------- port owners ----

/// The processes listening on (TCP) or bound to (UDP) `port`, IPv4 and IPv6, each once.
#[cfg(windows)]
fn port_owners(port: u16) -> Vec<u32> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, MIB_UDP6ROW_OWNER_PID, MIB_UDPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
        UDP_TABLE_OWNER_PID,
    };
    use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    // A table: its row count (u32) then the rows; the caller's closure reads (port, pid) from a row.
    fn table<R: Copy>(fill: impl Fn(Option<*mut core::ffi::c_void>, *mut u32) -> u32, row: impl Fn(&R) -> (u32, u32)) -> Vec<(u16, u32)> {
        let mut size = 0u32;
        let _ = fill(None, &mut size);
        if size == 0 {
            return Vec::new();
        }
        // (u64 cells: the rows' alignment.)
        let mut buf = vec![0u64; (size as usize).div_ceil(8) + 1];
        if fill(Some(buf.as_mut_ptr().cast()), &mut size) != 0 {
            return Vec::new();
        }
        // SAFETY: the table was filled by Windows into `buf` (row count, then that many rows).
        unsafe {
            let base = buf.as_ptr().cast::<u8>();
            let count = *base.cast::<u32>() as usize;
            let rows = base.add(std::mem::align_of::<R>().max(4)).cast::<R>();
            (0..count).map(|i| row(&*rows.add(i))).map(|(p, pid)| (u16::from_be((p & 0xFFFF) as u16), pid)).collect()
        }
    }
    let mut all = Vec::new();
    // SAFETY (each closure): the size and buffer come from `table`, which sizes the buffer first.
    all.extend(table::<MIB_TCPROW_OWNER_PID>(|b, s| unsafe { GetExtendedTcpTable(b, s, false, AF_INET.0 as u32, TCP_TABLE_OWNER_PID_LISTENER, 0) }, |r| (r.dwLocalPort, r.dwOwningPid)));
    all.extend(table::<MIB_TCP6ROW_OWNER_PID>(|b, s| unsafe { GetExtendedTcpTable(b, s, false, AF_INET6.0 as u32, TCP_TABLE_OWNER_PID_LISTENER, 0) }, |r| (r.dwLocalPort, r.dwOwningPid)));
    all.extend(table::<MIB_UDPROW_OWNER_PID>(|b, s| unsafe { GetExtendedUdpTable(b, s, false, AF_INET.0 as u32, UDP_TABLE_OWNER_PID, 0) }, |r| (r.dwLocalPort, r.dwOwningPid)));
    all.extend(table::<MIB_UDP6ROW_OWNER_PID>(|b, s| unsafe { GetExtendedUdpTable(b, s, false, AF_INET6.0 as u32, UDP_TABLE_OWNER_PID, 0) }, |r| (r.dwLocalPort, r.dwOwningPid)));
    let mut pids: Vec<u32> = Vec::new();
    for pid in all.into_iter().filter(|(p, pid)| *p == port && *pid != 0).map(|(_, pid)| pid) {
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    pids
}

// ------------------------------------------------------------------------- file owners ----

/// (pid, app name) of the processes that have `path` open (the Restart Manager's list).
#[cfg(windows)]
fn file_owners(path: &std::path::Path) -> Result<Vec<(u32, String)>, String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_MORE_DATA;
    use windows::Win32::System::RestartManager::{RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY, RM_PROCESS_INFO};
    let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect();
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    let mut session = 0u32;
    // SAFETY: the session is ended on every path out; buffers are sized and outlive the calls.
    unsafe {
        if RmStartSession(&mut session, 0, PWSTR(key.as_mut_ptr())).is_err() {
            return Err("The Restart Manager would not start, Sir.".into());
        }
        let result = (|| {
            RmRegisterResources(session, Some(&[PCWSTR(wide.as_ptr())]), None, None).ok().map_err(|e| e.to_string())?;
            let (mut needed, mut count, mut reasons) = (0u32, 0u32, 0u32);
            let first = RmGetList(session, &mut needed, &mut count, None, &mut reasons);
            if first.is_ok() || needed == 0 {
                return Ok(Vec::new());
            }
            if first != ERROR_MORE_DATA {
                return Err(format!("{first:?}"));
            }
            let mut infos = vec![RM_PROCESS_INFO::default(); needed as usize];
            count = needed;
            RmGetList(session, &mut needed, &mut count, Some(infos.as_mut_ptr()), &mut reasons).ok().map_err(|e| e.to_string())?;
            Ok(infos[..count as usize]
                .iter()
                .map(|i| {
                    let n = i.strAppName.iter().position(|&c| c == 0).unwrap_or(i.strAppName.len());
                    (i.Process.dwProcessId, String::from_utf16_lossy(&i.strAppName[..n]))
                })
                .collect())
        })();
        let _ = RmEndSession(session);
        result
    }
}

// -------------------------------------------------------------------------- startup ----

#[derive(Debug, Clone, PartialEq)]
struct StartupEntry {
    name: String,
    /// "Run (you)", "Run (all users)", "Startup folder".
    place: &'static str,
    on: bool,
    /// The StartupApproved key (under HKCU or HKLM) and whether it lies under HKLM.
    #[cfg(windows)]
    approved: &'static str,
    #[cfg(windows)]
    machine: bool,
    /// Its `.desktop` file (Linux).
    #[cfg(not(windows))]
    file: std::path::PathBuf,
}

#[cfg(windows)]
const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
#[cfg(windows)]
const APPROVED_RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
#[cfg(windows)]
const APPROVED_FOLDER: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\StartupFolder";

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The names of the values under `path`.
#[cfg(windows)]
fn value_names(root: windows::Win32::System::Registry::HKEY, path: &str) -> Vec<String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, KEY_READ};
    let mut out = Vec::new();
    let path = wide(path);
    // SAFETY: the key is opened and closed here; the name buffer is sized and passed with its length.
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(root, PCWSTR(path.as_ptr()), 0, KEY_READ, &mut key) != ERROR_SUCCESS {
            return out;
        }
        for i in 0..500u32 {
            let mut name = [0u16; 512];
            let mut len = name.len() as u32;
            if RegEnumValueW(key, i, PWSTR(name.as_mut_ptr()), &mut len, None, None, None, None) != ERROR_SUCCESS {
                break;
            }
            let name = String::from_utf16_lossy(&name[..len as usize]);
            if !name.is_empty() {
                out.push(name);
            }
        }
        let _ = RegCloseKey(key);
    }
    out
}

/// Task Manager's switch for `name`: on unless its StartupApproved bytes start odd (3 = off).
#[cfg(windows)]
fn approved_on(root: windows::Win32::System::Registry::HKEY, path: &str, name: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_BINARY};
    let (path, name) = (wide(path), wide(name));
    let mut buf = [0u8; 16];
    let mut size = buf.len() as u32;
    // SAFETY: a sized read into a local buffer.
    let read = unsafe { RegGetValueW(root, PCWSTR(path.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) };
    !(read.is_ok() && size > 0 && buf[0] & 1 == 1)
}

#[cfg(windows)]
fn startup_entries() -> Vec<StartupEntry> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let mut out = Vec::new();
    for (root, machine, place) in [(HKEY_CURRENT_USER, false, "Run (you)"), (HKEY_LOCAL_MACHINE, true, "Run (all users)")] {
        for name in value_names(root, RUN) {
            let on = approved_on(root, APPROVED_RUN, &name);
            out.push(StartupEntry { name, place, on, approved: APPROVED_RUN, machine });
        }
    }
    let folders = [
        (std::env::var("APPDATA").ok().map(|d| std::path::PathBuf::from(d).join("Microsoft\\Windows\\Start Menu\\Programs\\Startup")), false),
        (std::env::var("ProgramData").ok().map(|d| std::path::PathBuf::from(d).join("Microsoft\\Windows\\Start Menu\\Programs\\StartUp")), true),
    ];
    for (dir, machine) in folders {
        let Some(dir) = dir else { continue };
        for file in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = file.file_name().to_string_lossy().to_string();
            if name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            let root = if machine { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER };
            let on = approved_on(root, APPROVED_FOLDER, &name);
            out.push(StartupEntry { name, place: "Startup folder", on, approved: APPROVED_FOLDER, machine });
        }
    }
    out
}

/// The entry he means: the same name, else one whose name holds his word (shortest first).
fn best_entry<'a>(entries: &'a [StartupEntry], said: &str) -> Option<&'a StartupEntry> {
    let said = said.trim().to_lowercase();
    let plain = |n: &str| n.to_lowercase().trim_end_matches(".lnk").to_string();
    entries.iter().find(|e| plain(&e.name) == said).or_else(|| entries.iter().filter(|e| plain(&e.name).contains(&said)).min_by_key(|e| e.name.len()))
}

/// Task Manager's own "Disable": the StartupApproved bytes 03 00 … (the entry itself stays).
#[cfg(windows)]
fn switch_off(entry: &StartupEntry) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_BINARY, REG_OPTION_NON_VOLATILE};
    let root = if entry.machine { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER };
    let (path, name) = (wide(entry.approved), wide(&entry.name));
    let mut bytes = [0u8; 12];
    bytes[0] = 3;
    // SAFETY: the key is created/opened and closed here; the data slice outlives the call.
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(root, PCWSTR(path.as_ptr()), 0, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None) != ERROR_SUCCESS {
            return Err(if entry.machine { "That entry belongs to all users: switching it off needs administrator rights, Sir (Task Manager's Startup page can).".into() } else { "Windows refused the change, Sir.".into() });
        }
        let set = RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_BINARY, Some(&bytes));
        let _ = RegCloseKey(key);
        (set == ERROR_SUCCESS).then_some(()).ok_or_else(|| "Windows refused the change, Sir.".to_string())
    }
}

// --------------------------------------------------------------------- J.A.I. himself ----

/// His own entry: his Run value (Windows), his autostart file (Linux).
const OWN: &str = if cfg!(windows) { "JAI" } else { "jai.desktop" };

/// A test or a scripted session never changes what starts with the system.
fn not_a_test() -> Result<(), String> {
    if crate::platform::hands_off() || crate::live_self_tester::dry() {
        return Err("Not from a test session.".into());
    }
    Ok(())
}

/// Whether J.A.I. starts by himself when Sir logs in: his Run value, Task Manager's switch on.
#[cfg(windows)]
pub fn starts_with_system() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_ANY};
    let (path, name) = (wide(RUN), wide(OWN));
    let mut size = 0u32;
    // SAFETY: only the value's size is read, into a local.
    let there = unsafe { RegGetValueW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_ANY, None, None, Some(&mut size)) }.is_ok();
    there && approved_on(HKEY_CURRENT_USER, APPROVED_RUN, OWN)
}

/// Puts this J.A.I. (where it runs now) into Sir's start of Windows, or takes him out. Task
/// Manager's "Disabled" goes either way, so the switch is what it says.
#[cfg(windows)]
pub fn start_with_system(on: bool) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ};
    const REFUSED: &str = "Windows refused the change, Sir.";
    not_a_test()?;
    let name = wide(OWN);
    // SAFETY: the key is created/opened and closed here; every buffer outlives its call.
    unsafe {
        let gone = |path: &str| {
            let path = wide(path);
            let e = RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), PCWSTR(name.as_ptr()));
            e == ERROR_SUCCESS || e == ERROR_FILE_NOT_FOUND
        };
        if !gone(APPROVED_RUN) {
            return Err(REFUSED.into());
        }
        if !on {
            return gone(RUN).then_some(()).ok_or_else(|| REFUSED.to_string());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let command: Vec<u8> = wide(&format!("\"{}\"", exe.display())).iter().flat_map(|u| u.to_le_bytes()).collect();
        let (path, mut key) = (wide(RUN), HKEY::default());
        if RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), 0, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None) != ERROR_SUCCESS {
            return Err(REFUSED.into());
        }
        let set = RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(&command));
        let _ = RegCloseKey(key);
        (set == ERROR_SUCCESS).then_some(()).ok_or_else(|| REFUSED.to_string())
    }
}

/// His own autostart entry: the menu entry, so KWin's grants hold for him there too.
#[cfg(not(windows))]
fn own_entry() -> Option<std::path::PathBuf> {
    user_autostart().map(|d| d.join(OWN))
}

/// Whether J.A.I. starts by himself when Sir logs in: his autostart entry, not hidden.
#[cfg(not(windows))]
pub fn starts_with_system() -> bool {
    own_entry().and_then(|f| std::fs::read_to_string(f).ok()).is_some_and(|text| desktop_starts(&text))
}

/// Puts this J.A.I. (where it runs now) into Sir's autostart, or takes him out.
#[cfg(not(windows))]
pub fn start_with_system(on: bool) -> Result<(), String> {
    not_a_test()?;
    let file = own_entry().ok_or("I can't find your home folder, Sir.")?;
    if !on {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("I couldn't remove {}: {e}", file.display())),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(file.parent().unwrap_or(&file)).map_err(|e| e.to_string())?;
    std::fs::write(&file, crate::installer::menu_entry(&exe)).map_err(|e| format!("I couldn't write {}: {e}", file.display()))
}

// ------------------------------------------------------------------------ restart due ----

/// Whether Windows waits for a restart (Windows Update's or the servicing stack's flag).
#[cfg(windows)]
pub fn restart_pending() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ};
    ["SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\WindowsUpdate\\Auto Update\\RebootRequired", "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Component Based Servicing\\RebootPending"].iter().any(|path| {
        let path = wide(path);
        let mut key = HKEY::default();
        // SAFETY: the key is only opened to see that it exists, and closed.
        unsafe {
            let open = RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), 0, KEY_READ, &mut key) == ERROR_SUCCESS;
            if open {
                let _ = RegCloseKey(key);
            }
            open
        }
    })
}

/// Once per Windows session, as a game starts: a restart Windows waits for (a game is a long
/// stretch it would otherwise interrupt). Returns the line to say, if one is due.
pub fn restart_note_for_game() -> Option<String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static TOLD: AtomicBool = AtomicBool::new(false);
    let why = if cfg!(windows) { "an update could restart the PC on its own later" } else { "an update needs one" };
    (restart_pending() && !TOLD.swap(true, Ordering::AcqRel)).then(|| format!("{OS} is waiting for a restart, Sir: {why}."))
}

// ------------------------------------------------------------------------------ Linux ----

/// The windows KWin marks as hung: it adds "(Not Responding)", in the desktop's language, to the title.
#[cfg(not(windows))]
fn hung_windows() -> Vec<(u32, String, String)> {
    let mut out: Vec<(u32, String, String)> = Vec::new();
    for w in crate::plasma::windows() {
        let title = w.title.to_lowercase();
        let hung = ["not responding", "reagiert nicht", "antwortet nicht"].iter().any(|m| title.contains(m));
        if hung && w.pid != std::process::id() && !out.iter().any(|(pid, _, _)| *pid == w.pid) {
            out.push((w.pid, process_name(w.pid).unwrap_or_else(|| w.resource_name.clone()), w.title));
        }
    }
    out
}

/// `checkupdates`' lines ("name 1.0-1 -> 1.1-1") as (name, "pacman", installed, available).
#[cfg(not(windows))]
fn pacman_rows(out: &str) -> Vec<(String, String, String, String)> {
    out.lines()
        .filter_map(|l| {
            let c: Vec<&str> = l.split_whitespace().collect();
            (c.len() == 4 && c[2] == "->").then(|| (c[0].to_string(), "pacman".to_string(), c[1].to_string(), c[3].to_string()))
        })
        .collect()
}

/// `flatpak remote-ls --updates --columns=name,application,version`: tab-separated rows.
#[cfg(not(windows))]
fn flatpak_rows(out: &str) -> Vec<(String, String, String, String)> {
    out.lines()
        .filter_map(|l| {
            let c: Vec<&str> = l.split('\t').map(str::trim).collect();
            (c.len() >= 2 && !c[0].is_empty()).then(|| (c[0].to_string(), c[1].to_string(), String::new(), c.get(2).copied().filter(|v| !v.is_empty()).unwrap_or("new").to_string()))
        })
        .collect()
}

/// The updates pacman and Flatpak know of. `checkupdates` (pacman-contrib) reads a copy of the
/// package database, so nothing needs root; exit code 2 means there are none.
#[cfg(not(windows))]
async fn app_updates() -> Result<Vec<(String, String, String, String)>, String> {
    let wait = std::time::Duration::from_secs(120);
    let check = crate::apps::which("checkupdates").ok_or("checkupdates is not installed, Sir. Install it with: sudo pacman -S pacman-contrib")?;
    let out = crate::shell::run_helper(&check, std::iter::empty::<&str>(), None, &[], wait).await.map_err(|e| format!("checkupdates did not run, Sir: {e}"))?;
    if !matches!(out.code, Some(0) | Some(2)) {
        return Err(format!("checkupdates failed, Sir: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let mut rows = pacman_rows(&String::from_utf8_lossy(&out.stdout));
    if let Some(flatpak) = crate::apps::which("flatpak") {
        if let Ok(out) = crate::shell::run_helper(&flatpak, ["remote-ls", "--updates", "--columns=name,application,version"], None, &[], wait).await {
            rows.extend(flatpak_rows(&String::from_utf8_lossy(&out.stdout)));
        }
    }
    Ok(rows)
}

/// Installing is Discover's job (it asks for the password itself); J.A.I. never runs pacman as root.
#[cfg(not(windows))]
fn upgrade_all() -> Result<String, String> {
    let mut child = std::process::Command::new("plasma-discover")
        .args(["--mode", "update"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "Discover is not installed, Sir. Install it with: sudo pacman -S discover".to_string())?;
    std::thread::spawn(move || drop(child.wait()));
    Ok(say("Discover's update page is open, Sir: it installs them and asks for your password itself.", "Discover's update page is open, Sir."))
}

#[cfg(not(windows))]
fn process_name(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
}

#[cfg(not(windows))]
fn end_process(pid: u32) -> Result<(), String> {
    let ended = std::process::Command::new("kill").arg(pid.to_string()).status().map_err(|e| e.to_string())?;
    ended.success().then_some(()).ok_or_else(|| "Linux refused to end it, Sir (it may belong to another user).".to_string())
}

/// The processes holding an open descriptor that `wanted` accepts (`/proc/<pid>/fd`: another
/// user's only when JAI runs as root).
#[cfg(not(windows))]
fn fd_owners(wanted: impl Fn(&std::path::Path) -> bool) -> Vec<u32> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let mut fds = std::fs::read_dir(entry.path().join("fd")).into_iter().flatten().flatten();
        if fds.any(|fd| std::fs::read_link(fd.path()).is_ok_and(|target| wanted(&target))) {
            pids.push(pid);
        }
    }
    pids
}

/// The sockets of one `/proc/net` table bound to `port` (TCP: listening only), named as
/// `/proc/<pid>/fd` names them. A row: "sl local_address rem_address st … inode", the local
/// address "0100007F:1F90" (the port in hex), st 0A = LISTEN.
#[cfg(any(not(windows), test))]
fn sockets_on(table: &str, port: u16, listening_only: bool) -> Vec<String> {
    let row = |line: &str| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (local, state, inode) = (*cols.get(1)?, *cols.get(3)?, *cols.get(9)?);
        let bound = local.rsplit(':').next().and_then(|hex| u16::from_str_radix(hex, 16).ok()) == Some(port);
        (bound && (!listening_only || state == "0A") && inode != "0").then(|| format!("socket:[{inode}]"))
    };
    table.lines().skip(1).filter_map(row).collect()
}

/// The processes listening on (TCP) or bound to (UDP) `port`: the sockets from `/proc/net`,
/// then the processes holding them.
#[cfg(not(windows))]
fn port_owners(port: u16) -> Vec<u32> {
    let tables = [("tcp", true), ("tcp6", true), ("udp", false), ("udp6", false)];
    let sockets: Vec<String> = tables.iter().flat_map(|(t, listening)| sockets_on(&std::fs::read_to_string(format!("/proc/net/{t}")).unwrap_or_default(), port, *listening)).collect();
    if sockets.is_empty() {
        return Vec::new();
    }
    fd_owners(|target| sockets.iter().any(|s| target.as_os_str() == s.as_str()))
}

/// (pid, program) of the processes that have `path` open.
#[cfg(not(windows))]
fn file_owners(path: &std::path::Path) -> Result<Vec<(u32, String)>, String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    Ok(fd_owners(|target| target == path).into_iter().map(|pid| (pid, process_name(pid).unwrap_or_else(|| format!("process {pid}")))).collect())
}

/// His own autostart folder (XDG): `$XDG_CONFIG_HOME/autostart`, else `~/.config/autostart`.
#[cfg(not(windows))]
fn user_autostart() -> Option<std::path::PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()).map(std::path::PathBuf::from);
    config.or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))).map(|d| d.join("autostart"))
}

/// The XDG autostart entries: his own, then every user's (`/etc/xdg/autostart`), where a file of
/// his with the same name stands in for the system's.
#[cfg(not(windows))]
fn startup_entries() -> Vec<StartupEntry> {
    let mut out: Vec<StartupEntry> = Vec::new();
    for (dir, place) in [(user_autostart(), "Autostart (you)"), (Some(std::path::PathBuf::from("/etc/xdg/autostart")), "Autostart (all users)")] {
        for file in dir.and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "desktop") || out.iter().any(|e| e.file.file_name() == path.file_name()) {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let name = desktop_key(&text, "Name").filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
            out.push(StartupEntry { name, place, on: desktop_starts(&text), file: path });
        }
    }
    out
}

/// A key of a `.desktop` entry ("Name", "Hidden"), trimmed; a translation ("Name[de]") is not it.
#[cfg(any(not(windows), test))]
fn desktop_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.strip_prefix(key).and_then(|v| v.strip_prefix('=')).map(str::trim))
}

/// Whether an autostart entry starts: neither hidden nor switched off in GNOME's way.
#[cfg(any(not(windows), test))]
fn desktop_starts(text: &str) -> bool {
    desktop_key(text, "Hidden") != Some("true") && desktop_key(text, "X-GNOME-Autostart-enabled") != Some("false")
}

/// The entry's text with `Hidden=true` at the top of its [Desktop Entry] group (an old
/// `Hidden=` goes).
#[cfg(any(not(windows), test))]
fn hidden(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().filter(|l| !l.trim_start().starts_with("Hidden=")).collect();
    let at = lines.iter().position(|l| l.trim() == "[Desktop Entry]").map_or(0, |i| i + 1);
    lines.insert(at, "Hidden=true");
    lines.join("\n") + "\n"
}

/// The XDG way to switch one off: his own copy of the entry says `Hidden=true` (nothing is
/// deleted; for an entry of every user's, it is hidden for him alone).
#[cfg(not(windows))]
fn switch_off(entry: &StartupEntry) -> Result<(), String> {
    let dir = user_autostart().ok_or("I can't find your home folder, Sir.")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let copy = dir.join(entry.file.file_name().ok_or("That entry has no file, Sir.")?);
    let text = std::fs::read_to_string(&entry.file).map_err(|e| e.to_string())?;
    std::fs::write(&copy, hidden(&text)).map_err(|e| format!("I couldn't write {}: {e}", copy.display()))
}

/// Whether an update waits for a restart: Debian's flag file, or (Arch) the running kernel's
/// modules are gone from disk because pacman replaced the kernel.
#[cfg(not(windows))]
pub fn restart_pending() -> bool {
    let running = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let replaced = !running.trim().is_empty() && std::path::Path::new("/usr/lib/modules").is_dir() && !std::path::Path::new("/usr/lib/modules").join(running.trim()).exists();
    replaced || std::path::Path::new("/var/run/reboot-required").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pc_questions_are_heard_in_english_and_german() {
        for (said, want) in [
            ("is anything frozen?", json!({"action": "frozen"})),
            ("welches programm hängt", json!({"action": "frozen"})),
            ("who is using port 8080", json!({"action": "port", "port": 8080})),
            ("wer benutzt port 3000", json!({"action": "port", "port": 3000})),
            ("who has report.docx open", json!({"action": "file", "file": "report.docx"})),
            ("wer hat notes.txt offen", json!({"action": "file", "file": "notes.txt"})),
            ("what starts with windows", json!({"action": "startup"})),
            ("zeig mir meine autostart programme", json!({"action": "startup"})),
            ("stop discord from starting with windows", json!({"action": "startup_off", "name": "discord"})),
            ("nimm steam aus dem autostart", json!({"action": "startup_off", "name": "steam"})),
            ("does windows need a restart", json!({"action": "reboot"})),
            ("muss ich neu starten", json!({"action": "reboot"})),
            ("what starts with linux", json!({"action": "startup"})),
            ("was startet mit linux", json!({"action": "startup"})),
            ("stop steam from starting with linux", json!({"action": "startup_off", "name": "steam"})),
            ("does linux need a restart", json!({"action": "reboot"})),
        ] {
            assert_eq!(command(&crate::intents::normalize_phrase(said)), Some(want.clone()), "{said}");
        }
        for not in ["port 8080", "open discord", "start discord", "what's using my gpu", "who is frozen in the game"] {
            assert_eq!(command(&crate::intents::normalize_phrase(not)), None, "{not}");
        }
    }

    /// The order his yes runs is the offer's own words, through the same normaliser.
    #[test]
    fn the_offered_orders_come_back_as_themselves() {
        let order = end_order(4321, "Discord PTB");
        assert_eq!(command(&crate::intents::normalize_phrase(&order)), Some(json!({"action": "end", "pid": 4321, "name": "discord-ptb"})), "{order}");
        assert_eq!(command(&crate::intents::normalize_phrase("switch off startup entry Steam")), Some(json!({"action": "startup_off", "name": "steam", "confirmed": true})));
        assert!(only_looks(&json!({"action": "port"})) && !only_looks(&json!({"action": "end"})) && !only_looks(&json!({"action": "startup_off"})));
        assert_eq!(command(&crate::intents::normalize_phrase("upgrade all apps with winget")), Some(json!({"action": "upgrade"})));
        assert!(only_looks(&json!({"action": "updates"})) && !only_looks(&json!({"action": "upgrade"})), "the list only reads; his yes installs");
    }

    /// Words about his own start are his switch's (`hud_feel`), never an entry of that name; and
    /// no test touches the registry or the autostart folder.
    #[test]
    fn his_own_start_is_his_switch_and_no_test_changes_it() {
        for said in ["stop yourself from starting with windows", "don't start yourself with windows", "stop JAI from starting at startup", "nimm dich aus dem Autostart"] {
            assert_eq!(command(&crate::intents::normalize_phrase(said)), None, "{said}");
        }
        assert_eq!(command(&crate::intents::normalize_phrase("stop Steam from starting with Windows")), Some(json!({"action": "startup_off", "name": "steam"})));
        let before = starts_with_system();
        assert!(start_with_system(!before).is_err() && start_with_system(before).is_err());
        assert_eq!(starts_with_system(), before);
    }

    #[test]
    fn the_microphone_check_names_the_first_thing_that_stops_the_sound() {
        for said in ["my mic doesn't work", "my microphone isn't working", "check my mic", "what's wrong with my microphone", "mein mikro geht nicht", "mein Mikrofon funktioniert nicht mehr", "teste mein mikrofon"] {
            assert_eq!(command(&crate::intents::normalize_phrase(said)), Some(json!({"action": "mic"})), "{said}");
        }
        assert_eq!(command(&crate::intents::normalize_phrase("my mic")), None);
    }

    #[cfg(windows)]
    #[test]
    fn the_microphone_verdict_names_the_first_fault() {
        let mic = |muted: bool, level: f32, peak: f32| crate::audio_mixer::MicState { name: "Headset Microphone".into(), level, muted, peak };
        let say = |blocked: Option<&str>, m: Option<crate::audio_mixer::MicState>| mic_verdict(blocked, m.as_ref()).1;
        assert!(say(Some("desktop apps"), Some(mic(true, 0.0, 0.0))).starts_with("Windows' privacy setting blocks the microphone for desktop apps"), "privacy first: nothing else matters while it blocks");
        assert_eq!(say(None, None), "No microphone is set as Windows' default recording device, Sir.");
        assert_eq!(say(None, Some(mic(true, 0.8, 0.3))), "Headset Microphone is muted in Windows, Sir.");
        assert_eq!(say(None, Some(mic(false, 0.04, 0.3))), "Headset Microphone's level is down at 4 percent, Sir.");
        assert!(say(None, Some(mic(false, 0.8, 0.0))).contains("hears nothing at all"));
        assert!(say(None, Some(mic(false, 0.8, 0.2))).starts_with("Windows' side is fine, Sir: Headset Microphone is on at 80 percent"));
        let (rows, _) = mic_verdict(None, Some(&mic(false, 0.8, 0.2)));
        assert_eq!(rows, "Privacy   allowed\nDevice    Headset Microphone\nMuted     no\nLevel     80 %\nHearing   yes (peak 0.20)");
        assert!(rows.lines().all(|l| l.chars().count() <= 48), "fits the 420 px block");
    }

    #[cfg(not(windows))]
    #[test]
    fn linuxs_microphone_verdict_claims_nothing_it_cannot_measure() {
        // (PipeWire keeps no meter on the microphone: its peak always reads 0 there.)
        let mic = |muted: bool, level: f32| crate::audio_mixer::MicState { name: "Headset Microphone".into(), level, muted, peak: 0.0 };
        let say = |m: Option<crate::audio_mixer::MicState>| mic_verdict(None, m.as_ref()).1;
        assert_eq!(say(None), "No microphone is set as Linux's default recording device, Sir.");
        assert_eq!(say(Some(mic(true, 0.8))), "Headset Microphone is muted in Linux, Sir.");
        assert_eq!(say(Some(mic(false, 0.04))), "Headset Microphone's level is down at 4 percent, Sir.");
        assert!(say(Some(mic(false, 0.8))).starts_with("Linux's side is fine, Sir: Headset Microphone is on at 80 percent. "), "never 'hears nothing'");
        assert!(!mic_verdict(None, Some(&mic(false, 0.8))).0.contains("Hearing"));
    }

    #[test]
    fn winget_updates_are_read_from_its_table_in_any_language() {
        for said in ["are there updates for my apps", "any updates for my programs", "check for app updates", "update my apps", "which apps can be updated", "gibt es updates für meine programme", "aktualisiere alle apps"] {
            assert_eq!(command(&crate::intents::normalize_phrase(said)), Some(json!({"action": "updates"})), "{said}");
        }
        for not in ["update me", "any updates", "updates for minecraft"] {
            assert_eq!(command(&crate::intents::normalize_phrase(not)), None, "{not}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_winget_table_is_read_in_any_language() {
        let english ="   - \r   \\ \rName                       Id                      Version     Available   Source\n\
            ----------------------------------------------------------------------------------------\n\
            Git                        Git.Git                 2.45.0      2.46.0      winget\n\
            Mozilla Firefox (x64 de)   Mozilla.Firefox         131.0       132.0.1     winget\n\
            Visual Studio Code         Microsoft.VisualStudio… 1.94.0      1.95.2      winget\n\
            3 upgrades available.\n\n\
            The following packages have an upgrade available, but require explicit targeting:\n";
        let rows = upgrade_rows(english);
        assert_eq!(rows.iter().map(|r| (r.0.as_str(), r.2.as_str(), r.3.as_str())).collect::<Vec<_>>(), [("Git", "2.45.0", "2.46.0"), ("Mozilla Firefox (x64 de)", "131.0", "132.0.1"), ("Visual Studio Code", "1.94.0", "1.95.2")]);
        assert_eq!(rows[2].1, "Microsoft.VisualStudio…", "a cut id with a multi-byte ellipsis stays in its column");
        let german = "Name        ID          Version  Verfügbar  Quelle\n---------------------------------------------------\nÄrzte-Tool  Max.Tool    1.0      1.1        winget\n1 Aktualisierungen verfügbar.\n";
        assert_eq!(upgrade_rows(german), [("Ärzte-Tool".to_string(), "Max.Tool".to_string(), "1.0".to_string(), "1.1".to_string())]);
        assert!(upgrade_rows("No installed package found matching input criteria.").is_empty());
        assert!(upgrade_rows("").is_empty());
        assert_eq!(updates_line(&rows), "Three apps have updates, Sir: Git, Mozilla Firefox (x64 de) and Visual Studio Code. Shall I install them in the background?");
        assert_eq!(updates_line(&rows[..1]), "Git has an update, Sir. Shall I install it in the background?");
        let five: Vec<_> = (0..5).map(|i| rows[i % 3].clone()).collect();
        assert!(updates_line(&five).contains("Git, Mozilla Firefox (x64 de), Visual Studio Code and 2 more"), "{}", updates_line(&five));
    }

    #[test]
    fn the_entry_he_means_is_the_closest_name() {
        let e = |name: &str| StartupEntry {
            name: name.into(),
            place: "Run (you)",
            on: true,
            #[cfg(windows)]
            approved: APPROVED_RUN,
            #[cfg(windows)]
            machine: false,
            #[cfg(not(windows))]
            file: Default::default(),
        };
        let entries = [e("Discord"), e("Steam"), e("SteamVR Helper"), e("OneDrive.lnk")];
        assert_eq!(best_entry(&entries, "steam").map(|e| e.name.as_str()), Some("Steam"));
        assert_eq!(best_entry(&entries, "onedrive").map(|e| e.name.as_str()), Some("OneDrive.lnk"));
        assert_eq!(best_entry(&entries, "vr").map(|e| e.name.as_str()), Some("SteamVR Helper"));
        assert_eq!(best_entry(&entries, "spotify"), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn linuxs_update_lists_are_read() {
        let rows = pacman_rows("linux 6.11.1-1 -> 6.11.2-1
firefox 131.0-1 -> 132.0-1
noise
");
        assert_eq!(rows, [("linux".to_string(), "pacman".to_string(), "6.11.1-1".to_string(), "6.11.2-1".to_string()), ("firefox".to_string(), "pacman".to_string(), "131.0-1".to_string(), "132.0-1".to_string())]);
        assert!(pacman_rows("").is_empty());
        let flat = flatpak_rows("Musterapp	org.example.Musterapp	1.2
Ohne Version	org.example.Plain	
");
        assert_eq!((flat[0].0.as_str(), flat[0].3.as_str(), flat[1].3.as_str()), ("Musterapp", "1.2", "new"));
    }

    #[test]
    fn linuxs_ports_and_autostart_entries_are_read() {
        // /proc/net/tcp: a socket listening on 8080 (0x1F90) and a connection to it.
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4711 1 0000000000000000 100 0 0 10 0\n   1: 0100007F:1F90 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 4712 1 0000000000000000 20 4 30 10 -1\n";
        assert_eq!(sockets_on(tcp, 8080, true), ["socket:[4711]"], "the listener, not the connection");
        assert_eq!(sockets_on(tcp, 8080, false).len(), 2, "UDP's sockets count in every state");
        assert!(sockets_on(tcp, 8081, true).is_empty() && sockets_on("", 8080, true).is_empty());
        let entry = "[Desktop Entry]\nType=Application\nName[de]=Dampf\nName=Steam\nExec=steam -silent\nHidden=false\n";
        assert_eq!(desktop_key(entry, "Name"), Some("Steam"), "the plain name, not a translation");
        assert!(desktop_starts(entry));
        let off = hidden(entry);
        assert!(!desktop_starts(&off) && off.starts_with("[Desktop Entry]\nHidden=true\n") && off.matches("Hidden=").count() == 1, "{off}");
        assert!(!desktop_starts("[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n"));
    }

    /// Read-only probes of this PC: nothing is ended or switched.
    #[test]
    fn the_pc_answers_without_changing_anything() {
        assert!(port_owners(1).len() <= 1);
        let _ = hung_windows();
        let _ = restart_pending();
        let entries = startup_entries();
        assert!(entries.iter().all(|e| !e.name.is_empty()));
        let missing = std::env::temp_dir().join("jai_no_such_file_for_kit_pc.txt");
        let _ = std::fs::write(&missing, "x");
        assert_eq!(file_owners(&missing).map(|o| o.len()), Ok(0), "a closed file has no owner");
        let _ = std::fs::remove_file(&missing);
    }
}
