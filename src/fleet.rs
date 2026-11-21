//! J.A.I. on other PCs (Sir, 2026-11-19: "My PC should always remain the MAIN pc"). A JAI on
//! another PC is a client of Sir's (`main_pc` in its settings): it logs in with an account as a
//! phone does (`accounts`, the window of `gui_login`), calls Sir's PC every few seconds
//! (`/api/pc`: its version, how its PC is, its user's voice print) and carries out what Sir
//! sends it ("open the earth on Max's PC"). A client sends orders nowhere, so nothing on Sir's
//! PC obeys another PC. Voice prints go both ways and are held in memory only
//! (`speaker_id::share_*`): each is kept on disk on its owner's PC alone. A client runs the
//! version Sir's PC runs: a new one is fetched at once and started (`update`); Sir's own PC
//! never updates itself.

use crate::app::Jai;
use crate::config::ConfigStore;
use crate::kit::say;
use crate::tools::ToolContext;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Which build this is: the commit it was built from and when (`build.rs`).
pub const BUILD: &str = env!("JAI_BUILD");
/// Where a client keeps its key to Sir's PC, and the name it logged in with: sealed in the vault.
pub const SESSION: &str = "jai-main-pc";
/// How often a client calls Sir's PC.
const EVERY: Duration = Duration::from_secs(3);
/// An order waits this long for its PC; then it is dropped (a "shut down" days later would be
/// a surprise).
const ORDER_KEEPS: Duration = Duration::from_secs(600);
/// A PC that called within this long is online (its device entry is written once a minute).
pub const ONLINE_WITHIN: i64 = 90;
/// The repository whose new versions a client hears of.
const GITHUB_HEAD: &str = "https://api.github.com/repos/JayKaey/JAI/commits/v3.2-tested-features";

/// Orders Sir sent to an account's PC (by its name in lower case), until that PC fetches them.
static ORDERS: LazyLock<Mutex<HashMap<String, Vec<(String, Instant)>>>> = LazyLock::new(Default::default);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// "Open the earth on Max's PC", "mach die Musik leiser auf Max PC".
static ON_PC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?P<order>.+?)\s+(?:on|at|auf|bei)\s+(?P<word>[\w.'’-]+)\s+(?:pc|computer|laptop|rechner)$").expect("regex"));
/// "Move JAI on Max's PC to the background", "mach auf Max PC die Musik aus": its PC in the middle.
static AMID_PC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?P<order>.+?)\s+(?:on|auf|bei)\s+(?P<word>[\w.'’-]+)\s+(?:pc|computer|laptop|rechner)[,:]?\s+(?P<rest>.+)$").expect("regex"));
/// "Send to Max's PC: open the earth", "tell Max's PC to open the earth", "schick an Max PC
/// „mach die Musik aus“": the order after its PC (a message to a person is WhatsApp's).
static TO_PC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^(?:(?:hey\s+)?(?:jai|j\.a\.i\.|jay)\b[\s,.:!]*)?(?:(?:send|sende|schick|schicke)\s+(?:to|an)\s+(?P<to>[\w.'’-]+)\s+(?:pc|computer|laptop|rechner)[\s,:]+|(?:tell|ask|sag|sage)\s+(?P<tell>[\w.'’-]+)\s+(?:pc|computer|laptop|rechner)(?:\s*[,:]\s*|\s+to\s+|\s+["“„']))(?P<order>.+)$"#).expect("regex")
});
/// A question or a word about himself is no order for another PC: "what is on Max's PC", "I'm on
/// Max's PC right now".
const NO_ORDER: &[&str] = &["i", "im", "i'm", "i’m", "we", "were", "what", "whats", "what's", "how", "where", "who", "why", "when", "which", "is", "are", "was", "ich", "wir", "wie", "wo", "wer", "warum", "wann", "welche", "welcher", "ist", "sind", "bin"];

/// The names a possessive word may stand for ("max's", "maxs", "max'", "chris").
fn names_in(word: &str) -> Vec<String> {
    let bare = word.trim_end_matches(['\'', '’']);
    let mut names = vec![bare.to_string()];
    for suffix in ["'s", "’s", "s"] {
        if let Some(name) = bare.strip_suffix(suffix).filter(|n| !n.is_empty()) {
            names.push(name.to_string());
        }
    }
    names
}

/// "Shut down Max's PC", "restart Max's computer": an order about that PC itself (never this
/// one's power).
/// (The routers behind the brain read it without its apostrophe: "shut down maxs pc".)
static OF_PC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?P<order>.+?)\s+(?P<word>[\w.-]+?(?:'s|’s|s'|s’|s))\s+(?:pc|computer|laptop|rechner)$").expect("regex"));

/// An order for another PC: what, and whose PC.
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['.', '!', '?']);
    let (order, word) = if let Some(c) = TO_PC.captures(s) {
        (c["order"].trim().trim_matches(['"', '“', '”', '„', '\'']).trim().to_string(), c.name("to").or(c.name("tell"))?.as_str().to_lowercase())
    } else if let Some(c) = ON_PC.captures(s).or_else(|| AMID_PC.captures(s)) {
        (format!("{} {}", c["order"].trim(), c.name("rest").map_or("", |r| r.as_str())).trim().to_string(), c["word"].to_lowercase())
    } else {
        OF_PC.captures(s).map(|c| (format!("{} this PC", c["order"].trim()), c["word"].to_lowercase()))?
    };
    let first = order.split_whitespace().next().unwrap_or("").trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    if NO_ORDER.contains(&first.as_str())
        || [
            "my", "your", "this", "the", "that", "our", "his", "her", "their", "a", "its", "yours", "ours", "hers", "theirs", "every", "each", "all", "any", "another", "other", "both", "mein", "meinem", "meinen", "deinem", "seinem", "ihrem", "unserem", "eurem", "dem", "diesem", "des", "sir's", "sirs", "jai's", "jais", "jay's", "jays",
        ]
        .contains(&word.as_str())
    {
        return None;
    }
    Some(json!({ "order": order, "names": names_in(&word) }))
}

/// Whether this JAI is a client of Sir's PC.
pub fn is_client(main_pc: &str) -> bool {
    !main_pc.trim().is_empty()
}

/// Sends Sir's order to an account's PC (from another PC of his, signed in there: through his
/// own, which alone sends orders; nothing on his PC obeys another PC).
pub async fn run(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let (db, main) = ctx.config.read(|c| (c.memory_db_path.clone(), c.main_pc.clone()));
    if ctx.caller.guest().is_some() {
        return Err("Only Sir controls other PCs.".into());
    }
    if is_client(&main) {
        let v = crate::admin::remote(json!({ "action": "order", "args": args })).await?;
        return Ok(v["said"].as_str().unwrap_or("Sent.").to_string());
    }
    send(&db, args)
}

/// Sir's order, kept on his PC for the account's PC that it names.
pub fn send(db: &str, args: &Value) -> Result<String, String> {
    let order = args["order"].as_str().unwrap_or("").trim().to_string();
    let names: Vec<String> = serde_json::from_value(args["names"].clone()).unwrap_or_default();
    let account = names
        .iter()
        .find_map(|n| crate::accounts::get(db, n))
        .filter(|a| a.devices.iter().any(|d| d.kind == "pc"))
        .ok_or_else(|| format!("I know no PC of {} with J.A.I. on it, Sir.", crate::kit_voice::capitalised(names.last().map_or("that name", String::as_str))))?;
    if order.is_empty() {
        return Err(format!("What should {}'s PC do, Sir?", account.name));
    }
    let seen = account.devices.iter().filter(|d| d.kind == "pc").map(|d| d.seen).max().unwrap_or(0);
    let online = crate::accounts::now().saturating_sub(seen) < ONLINE_WITHIN;
    // (A test session sends nothing to a real PC.)
    if !crate::platform::in_test_session() && !crate::live_self_tester::dry() {
        lock(&ORDERS).entry(account.name.to_lowercase()).or_default().push((order.clone(), Instant::now()));
    }
    info!("[FLEET] '{order}' for {}'s PC ({})", account.name, if online { "online" } else { "offline" });
    let later = if online { "" } else { " It is offline: it gets it within ten minutes of coming back." };
    Ok(say(format!("Sent to {}'s PC: \"{order}\".{later}", account.name), format!("Sent to {}'s PC, Sir.{later}", account.name)))
}

/// The orders waiting for an account's PC (taken).
fn take(name: &str) -> Vec<String> {
    lock(&ORDERS).remove(&name.to_lowercase()).unwrap_or_default().into_iter().filter(|(_, at)| at.elapsed() < ORDER_KEEPS).map(|(order, _)| order).collect()
}

/// Sir's PC answers a client's call: what Sir sent it, his voice print, the build to run, what
/// its account may (Sir's switches, which that PC follows), and whether his sign-in there holds.
pub fn hello(db: &str, name: &str, body: &Value) -> Value {
    for done in body["done"].as_array().into_iter().flatten() {
        if let Some(order) = done.as_str() {
            crate::hud_notify::record(&format!("{name}'s PC has \"{order}\"."), false);
        }
    }
    if let Some(print) = body.get("print").filter(|p| p.is_object()) {
        crate::speaker_id::share_guest(name, print);
    }
    let account = crate::accounts::get(db, name).map(|a| crate::accounts::Account { devices: Vec::new(), ..a });
    let admin = body["admin"].as_str().is_some_and(crate::admin::holds);
    json!({ "orders": take(name), "sir": crate::speaker_id::print_to_share(db), "build": BUILD, "os": std::env::consts::OS, "account": account, "admin": admin })
}

/// How this PC is, in a line for Sir's list.
pub fn stats(app: &Jai) -> String {
    let t = app.tools().monitor.latest();
    let mut s = format!("CPU {:.0}% · RAM {:.1}/{:.0} GB", t.cpu_usage_pct, t.ram_used_gb, t.ram_total_gb);
    if let Some(g) = &t.gpu {
        s.push_str(&format!(" · {}", g.name.trim()));
        if let (Some(used), Some(total)) = (g.vram_used_mb, g.vram_total_mb) {
            s.push_str(&format!(" · VRAM {:.1}/{:.0} GB", used as f32 / 1024.0, total as f32 / 1024.0));
        }
        if let Some(c) = g.temperature_c {
            s.push_str(&format!(" · {c:.0}°C"));
        }
    }
    s.chars().take(200).collect()
}

/// This PC's name.
fn pc_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "PC".into())
}

pub(crate) fn vault(config: &ConfigStore) -> Option<crate::vault::Vault> {
    crate::vault::Vault::open(config.read(|c| c.memory_db_path.clone())).ok()
}

/// This PC's key to Sir's PC, once it is logged in.
pub fn key(config: &ConfigStore) -> Option<String> {
    vault(config).and_then(|v| v.secret(SESSION, "key"))
}

/// Why this J.A.I. answers nobody yet: it is another PC of Sir's and not logged in (the login
/// window opens), or Sir's main PC before he made his admin account there (its window opens).
pub fn locked(config: &ConfigStore) -> Option<&'static str> {
    if crate::admin::needs_setup(config) {
        crate::gui::show_admin_sign_in();
        return Some("Make your admin account first, please: the window for it is open.");
    }
    let waits = is_client(&config.read(|c| c.main_pc.clone())) && key(config).is_none();
    waits.then(|| {
        crate::gui::show_login();
        "Log in first, please: the window for it is open."
    })
}

/// The client: calls Sir's PC every few seconds while this JAI is one of his others. Returns at
/// once on Sir's own PC.
pub async fn client(app: Jai) {
    if !is_client(&app.config().read(|c| c.main_pc.clone())) {
        return;
    }
    // (What an update left behind.)
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("old.exe"));
    }
    let Ok(http) = reqwest::Client::builder().timeout(Duration::from_secs(20)).build() else { return };
    let mut done: Vec<String> = Vec::new();
    let (mut said_offline, mut github, mut github_at) = (false, None::<String>, None::<Instant>);
    loop {
        tokio::time::sleep(EVERY).await;
        let main = app.config().read(|c| c.main_pc.trim().trim_end_matches('/').to_string());
        let Some(key) = key(app.config()) else {
            crate::gui::show_login();
            continue;
        };
        let db = app.config().read(|c| c.memory_db_path.clone());
        let admin = crate::admin::key_held();
        let body = json!({ "kind": "pc", "device": pc_name(), "version": BUILD, "stats": stats(&app), "done": done, "print": crate::speaker_id::print_to_share(&db), "admin": admin });
        let answer = match http.post(format!("{main}/api/pc")).header("X-JAI-Key", &key).json(&body).send().await {
            Ok(r) if r.status().as_u16() == 403 || r.status().as_u16() == 401 => {
                // The account is gone, paused or its key no longer holds: log in again.
                warn!("[FLEET] Sir's PC no longer knows this PC's key ({}): logging in again", r.status());
                if let Some(v) = vault(app.config()) {
                    v.clear_secret(SESSION, "key");
                }
                crate::admin::hold(app.config(), None);
                continue;
            }
            Ok(r) => r.json::<Value>().await.ok(),
            Err(e) => {
                if !said_offline {
                    warn!("[FLEET] Sir's PC is not reached: {e}");
                    said_offline = true;
                }
                None
            }
        };
        let Some(answer) = answer else { continue };
        said_offline = false;
        done.clear();
        crate::speaker_id::share_sir(answer.get("sir").filter(|s| s.is_object()));
        // (What this PC's account may, as Sir set it; and whether his sign-in here still holds.)
        if let Some(account) = answer.get("account") {
            crate::admin::hold(app.config(), serde_json::from_value(account.clone()).ok());
        }
        crate::admin::heard(admin.as_deref(), answer["admin"].as_bool().unwrap_or(false));
        for order in answer["orders"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            info!("[FLEET] Sir's order: '{order}'");
            crate::hud_notify::record(&format!("Sir, from his PC: \"{order}\""), false);
            app.dispatch_sirs_order(order.to_string());
            done.push(order.to_string());
        }
        // (Only from a PC of its own kind: a Linux JAI's new version comes from GitHub, not from Sir's Windows.)
        let same_system = answer["os"].as_str().is_none_or(|os| os == std::env::consts::OS);
        if let Some(build) = answer["build"].as_str().filter(|b| *b != BUILD && !b.is_empty() && same_system) {
            match update(&http, &main, &key, build).await {
                Ok(()) => return,
                Err(e) => warn!("[FLEET] The new version ({build}) did not come: {e}"),
            }
        }
        // A new version on GitHub: said once per commit (the version itself comes from Sir's PC).
        if github_at.is_none_or(|at| at.elapsed() > Duration::from_secs(1800)) {
            github_at = Some(Instant::now());
            if let Some((sha, message)) = github_head(&http).await {
                if github.as_ref().is_some_and(|seen| *seen != sha) {
                    crate::hud_notify::record(&format!("J.A.I. was updated on GitHub: {message}"), false);
                }
                github = Some(sha);
            }
        }
    }
}

/// The newest commit of the repository: its id and the first line of its message.
async fn github_head(http: &reqwest::Client) -> Option<(String, String)> {
    let v: Value = http.get(GITHUB_HEAD).header("User-Agent", "JAI-client").send().await.ok()?.json().await.ok()?;
    let message = v["commit"]["message"].as_str()?.lines().next()?.to_string();
    Some((v["sha"].as_str()?.to_string(), message))
}

/// Sir's version, fetched from his PC and put in place of this one, then started. (A running
/// program may be renamed on Windows, not overwritten.)
async fn update(http: &reqwest::Client, main: &str, key: &str, build: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    info!("[FLEET] Sir's PC runs {build}, this one {BUILD}: fetching it");
    let bytes = http.get(format!("{main}/api/pc/update")).header("X-JAI-Key", key).timeout(Duration::from_secs(900)).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.bytes().await.map_err(|e| e.to_string())?;
    if !runs_here(&bytes) {
        return Err("what came is no program for this PC".into());
    }
    let (new, old) = (exe.with_extension("new.exe"), exe.with_extension("old.exe"));
    std::fs::write(&new, &bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    std::fs::set_permissions(&new, std::os::unix::fs::PermissionsExt::from_mode(0o755)).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(e.to_string());
    }
    crate::lifecycle::request_restart(crate::lifecycle::Restart::Same);
    crate::lifecycle::signal(crate::lifecycle::Signal::Alert("Sir's PC has a new version of me: restarting with it.".into()));
    crate::lifecycle::signal(crate::lifecycle::Signal::Shutdown);
    Ok(())
}

/// Whether bytes are a program for this PC (a Windows executable here, ELF on Linux).
fn runs_here(bytes: &[u8]) -> bool {
    bytes.len() > 1_000_000 && if cfg!(windows) { bytes.starts_with(b"MZ") } else { bytes.starts_with(b"\x7fELF") }
}

/// Sir's PC's address as typed: without a trailing slash, and a web address.
fn address(main: &str) -> Result<String, String> {
    let main = main.trim().trim_end_matches('/').to_string();
    if main.starts_with("https://") || main.starts_with("http://") {
        Ok(main)
    } else {
        Err("Sir's PC's address starts with https://".into())
    }
}

/// Logs in at Sir's PC (or, with `key`, finishes asking for an account there): the account's
/// name, its key kept in the vault.
pub async fn log_in(config: &ConfigStore, main: &str, name: &str, password: &str, key: Option<&str>) -> Result<String, String> {
    let main = address(main)?;
    let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    let mut body = json!({ "name": name, "password": password, "kind": "pc", "device": pc_name(), "version": BUILD });
    let path = match key {
        Some(k) => {
            body["key"] = json!(k.replace(' ', ""));
            "/api/signup/finish"
        }
        None => "/api/login",
    };
    let answer: Value = http.post(format!("{main}{path}")).json(&body).send().await.map_err(|e| format!("Sir's PC is not reached: {e}"))?.json().await.map_err(|e| e.to_string())?;
    let Some(session) = answer["key"].as_str() else { return Err(answer["error"].as_str().or(answer["message"].as_str()).unwrap_or("That did not work.").to_string()) };
    let v = vault(config).ok_or("the vault cannot be opened")?;
    v.set_secret(SESSION, "key", session)?;
    config.update(|c| c.main_pc = main.clone());
    Ok(answer["name"].as_str().unwrap_or(name).to_string())
}

/// Asks Sir's PC for an account: a key appears on his screen.
pub async fn ask_for_account(main: &str, name: &str, password: &str) -> Result<String, String> {
    let main = address(main)?;
    let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    let answer: Value = http.post(format!("{main}/api/signup")).json(&json!({ "name": name, "password": password })).send().await.map_err(|e| format!("Sir's PC is not reached: {e}"))?.json().await.map_err(|e| e.to_string())?;
    match answer["message"].as_str() {
        Some(m) if answer["ok"].as_bool() == Some(true) => Ok(m.to_string()),
        _ => Err(answer["error"].as_str().unwrap_or("That did not work.").to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sirs_orders_for_another_pc_are_read_in_his_words() {
        for (said, order, name) in [
            ("open the earth on Max's PC", "open the earth", "max"),
            ("open the earth on maxs pc", "open the earth", "max"),
            ("turn the volume down on Max's computer.", "turn the volume down", "max"),
            ("mach die Musik leiser auf Max PC", "mach die Musik leiser", "max"),
            ("show the weather on chris pc", "show the weather", "chris"),
            // (jai.log 2026-11-20: its PC in the middle went to the model, the order after it to WhatsApp.)
            ("move jai on maxs pc to the background", "move jai to the background", "max"),
            ("mach auf Max PC die Musik aus", "mach die Musik aus", "max"),
            ("send to maxs pc \"Jay move to the background\"", "Jay move to the background", "max"),
            ("JAI, send to Max's PC: open the earth", "open the earth", "max"),
            ("tell Max's PC to open the earth", "open the earth", "max"),
            ("schick an Max PC „mach die Musik aus“", "mach die Musik aus", "max"),
        ] {
            let got = command(said).unwrap_or_else(|| panic!("{said}"));
            assert_eq!(got["order"], order, "{said}");
            let names: Vec<String> = serde_json::from_value(got["names"].clone()).unwrap();
            assert!(names.contains(&name.to_string()), "{said}: {names:?}");
        }
        for said in [
            "open the earth on my PC", "play music on this computer", "open the earth", "what is on the pc", "show the files on the laptop", "shut down the pc", "restart my computer", "lock sir's pc",
            // (A question or a word about himself is no order, and a message to a person is WhatsApp's.)
            "what is on Max's PC", "I'm on Max's PC right now", "is the music on maxs pc loud", "tell max pc is broken", "send Max a message that my pc is broken", "send to Max: I'm on my PC", "mach auf seinem PC die Musik aus", "tell your pc to open the earth",
        ] {
            assert_eq!(command(said), None, "{said}");
        }
        assert_eq!(command("shut down Max's PC").unwrap()["order"], "shut down this PC");
        assert_eq!(command("shut down maxs pc").unwrap()["names"][1], "max");
        for said in ["shut down this pc", "lock his computer", "restart jais pc", "turn off its laptop"] {
            assert_eq!(command(said), None, "{said}");
        }
        assert_eq!(command("restart Max’s computer!").unwrap()["order"], "restart this PC");
    }

    /// The whole router, as the brain asks it: an order for another PC is that PC's and never
    /// carried out here ("open the earth" alone opens his own; "shut down" his own power).
    #[test]
    fn an_order_for_another_pc_never_lands_on_this_one() {
        for said in ["open the earth on Max's PC", "turn the volume down on Max's computer", "mach die Musik leiser auf Max PC", "shut down Max's PC", "restart Max's computer", "lock Max's PC", "play music on Max's laptop", "close all windows on Max's PC", "mute Max's PC", "move JAI on Max's PC to the background", "send to Max's PC \"Jay move to the background\"", "tell Max's PC to open the earth"] {
            let (tool, args) = crate::phrasings::route(said).unwrap_or_else(|| panic!("{said}: the model"));
            assert!(format!("{tool} {args}").contains("other_pc"), "{said} → {tool} {args}");
        }
    }

    #[test]
    fn an_order_waits_for_its_pc_and_is_taken_once() {
        lock(&ORDERS).insert("erika".into(), vec![("open the earth".into(), Instant::now()), ("old".into(), Instant::now() - ORDER_KEEPS - Duration::from_secs(1))]);
        assert_eq!(take("Erika"), vec!["open the earth".to_string()], "the stale one is dropped");
        assert!(take("erika").is_empty(), "taken once");
        assert!(runs_here(&[b"MZ".as_slice(), &vec![0u8; 1_000_001]].concat()) == cfg!(windows));
        assert!(!runs_here(b"<html>not found</html>"));
    }
}
