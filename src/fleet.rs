//! J.A.I. on other PCs (Sir, 2026-11-19: "My PC should always remain the MAIN pc"). A JAI on
//! another PC is a client of Sir's (`main_pc` in its settings): it logs in with an account as a
//! phone does (`accounts`, the window of `gui_login`), calls Sir's PC every few seconds
//! (`/api/pc`: its version, how its PC is, its user's voice print) and carries out what Sir
//! sends it ("open the earth on Max's PC"). A client sends orders nowhere, so nothing on Sir's
//! PC obeys another PC. Voice prints go both ways and are held in memory only
//! (`speaker_id::share_*`): each is kept on disk on its owner's PC alone. A client runs the
//! version Sir's PC runs: a new one is fetched at once and started (`update`). A newer release
//! on GitHub comes too, even while Sir's PC is out of reach, as long as this PC's key was good
//! when Sir's PC last answered. Sir's own PC never updates itself.

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
/// This version's number ("3.4.1"): an update never goes back to an older one.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// This build as a friend reads it ("3.4.1 · 6cb2469"): what a PC tells Sir's, shown in his
/// account dashboard, and said when asked ("which version are you on?").
pub fn release() -> String {
    format!("{VERSION} \u{b7} {}", BUILD.split('-').next().unwrap_or(BUILD))
}
/// The newest release, looked at every half hour by a client.
const GITHUB_RELEASE: &str = "https://api.github.com/repos/JayKaey/JAI/releases/latest";
/// The release's file that is J.A.I. himself on this system (`release.yml`).
const RELEASE_ASSET: &str = if cfg!(windows) { "JAI.exe" } else { "jai-linux" };

/// Orders Sir sent to an account's PCs (by its name in lower case), with the PCs that took each
/// already: every PC the account is logged in on gets it once (Sir, 2026-11-26: "one account
/// should be able to be locked into more than one PC").
static ORDERS: LazyLock<Mutex<HashMap<String, Vec<(String, Instant, Vec<String>)>>>> = LazyLock::new(Default::default);

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
pub(crate) fn names_in(word: &str) -> Vec<String> {
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

/// "Wake up Max's PC", "turn on Max's computer", "weck Max PC auf": waking it (`wake_pc`), not an
/// order it could only read once it is up.
static WAKE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:please\s+)?(?:(?:wake(?:\s+up)?|turn\s+on|switch\s+on|power\s+on|power\s+up|boot(?:\s+up)?)\s+(?P<word>[\w.'’-]+)\s+(?:pc|computer|laptop|rechner)(?:\s+up)?|(?:weck(?:e)?|fahr(?:e)?|schalte?)\s+(?P<word2>[\w.'’-]+)\s+(?:pc|computer|laptop|rechner)\s+(?:auf|hoch|ein|an))$").expect("regex")
});
/// Words that name no other PC's owner ("my PC", "this PC", "Sir's PC").
const NOT_A_NAME: &[&str] = &[
    "my", "your", "this", "the", "that", "our", "his", "her", "their", "a", "its", "yours", "ours", "hers", "theirs", "every", "each", "all", "any", "another", "other", "both", "mein", "meinem", "meinen", "deinem", "seinem", "ihrem", "unserem", "eurem", "dem", "diesem", "des", "sir's", "sirs", "jai's", "jais", "jay's", "jays",
];

/// An order for another PC: what, and whose PC.
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['.', '!', '?']);
    if let Some(c) = WAKE.captures(s) {
        let word = c.name("word").or(c.name("word2"))?.as_str().to_lowercase();
        return (!NOT_A_NAME.contains(&word.as_str())).then(|| json!({ "wake": true, "names": names_in(&word) }));
    }
    let (order, word) = if let Some(c) = TO_PC.captures(s) {
        (c["order"].trim().trim_matches(['"', '“', '”', '„', '\'']).trim().to_string(), c.name("to").or(c.name("tell"))?.as_str().to_lowercase())
    } else if let Some(c) = ON_PC.captures(s).or_else(|| AMID_PC.captures(s)) {
        (format!("{} {}", c["order"].trim(), c.name("rest").map_or("", |r| r.as_str())).trim().to_string(), c["word"].to_lowercase())
    } else {
        OF_PC.captures(s).map(|c| (format!("{} this PC", c["order"].trim()), c["word"].to_lowercase()))?
    };
    let first = order.split_whitespace().next().unwrap_or("").trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    if NO_ORDER.contains(&first.as_str()) || NOT_A_NAME.contains(&word.as_str()) {
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
    if args["wake"] == true {
        let names: Vec<String> = serde_json::from_value(args["names"].clone()).unwrap_or_default();
        return crate::wake_pc::wake(ctx, &names).await;
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
        lock(&ORDERS).entry(account.name.to_lowercase()).or_default().push((order.clone(), Instant::now(), Vec::new()));
    }
    info!("[FLEET] '{order}' for {}'s PC ({})", account.name, if online { "online" } else { "offline" });
    let later = if online { "" } else { " It is offline: it gets it within ten minutes of coming back." };
    Ok(say(format!("Sent to {}'s PC: \"{order}\".{later}", account.name), format!("Sent to {}'s PC, Sir.{later}", account.name)))
}

/// The orders waiting for this PC (`pc`: its id) of an account: each taken once by each PC.
fn take(name: &str, pc: &str) -> Vec<String> {
    let mut orders = lock(&ORDERS);
    let Some(waiting) = orders.get_mut(&name.to_lowercase()) else { return Vec::new() };
    waiting.retain(|(_, at, _)| at.elapsed() < ORDER_KEEPS);
    waiting
        .iter_mut()
        .filter(|(_, _, took)| !took.iter().any(|t| t == pc))
        .map(|(order, _, took)| {
            took.push(pc.to_string());
            order.clone()
        })
        .collect()
}

/// What another PC did, as Sir's PC heard it (`events`), for the rules and protocols that wait
/// on it (`kit_notes::When::Pc`). In a rule an empty game is any game.
#[derive(Debug, Clone, PartialEq)]
pub enum PcEvent {
    Online,
    Offline,
    Plays(String),
    Stops(String),
}

/// Each PC of each account as Sir's PC last heard of it (the account's name in lower case, the
/// PC's id): when, and the game it played. (Two PCs of one account: one's game is not ended by
/// the other's calls.)
static SEEN: LazyLock<Mutex<HashMap<(String, String), (Instant, Option<String>)>>> = LazyLock::new(Default::default);
/// What the PCs did since the rules last looked.
static EVENTS: Mutex<Vec<(String, PcEvent)>> = Mutex::new(Vec::new());

/// Notes a client's call: online again, a game started or ended. (`learning`: in the first
/// minutes after this J.A.I. started nobody knows what was before, so a PC is only noted.)
fn note(name: &str, pc: &str, game: Option<String>, learning: bool) {
    let name = name.to_lowercase();
    let before = lock(&SEEN).insert((name.clone(), pc.to_string()), (Instant::now(), game.clone()));
    if learning && before.is_none() {
        return;
    }
    let mut events = lock(&EVENTS);
    let played = match before {
        Some((_, played)) => played,
        None => {
            events.push((name.clone(), PcEvent::Online));
            None
        }
    };
    if played != game {
        events.extend(played.map(|g| (name.clone(), PcEvent::Stops(g))));
        events.extend(game.map(|g| (name, PcEvent::Plays(g))));
    }
}

/// Whether an account's PC called since `since`.
pub fn heard_since(name: &str, since: Instant) -> bool {
    let name = name.to_lowercase();
    lock(&SEEN).iter().any(|((n, _), (at, _))| *n == name && *at >= since)
}

/// What the PCs did since the last call; one unheard of for `ONLINE_WITHIN` has gone offline.
pub fn events() -> Vec<(String, PcEvent)> {
    let mut events = std::mem::take(&mut *lock(&EVENTS));
    lock(&SEEN).retain(|(name, _), (at, game)| {
        let gone = at.elapsed().as_secs() >= ONLINE_WITHIN as u64;
        if gone {
            events.extend(game.take().map(|g| (name.clone(), PcEvent::Stops(g))));
            events.push((name.clone(), PcEvent::Offline));
        }
        !gone
    });
    events
}

/// Sir's PC answers a client's call: what Sir sent it, his voice print, the build to run, what
/// its account may (Sir's switches, which that PC follows), and whether his sign-in there holds.
pub fn hello(db: &str, name: &str, pc: &str, body: &Value) -> Value {
    let game = body["game"].as_str().map(str::trim).filter(|g| !g.is_empty()).map(String::from);
    note(name, pc, game, crate::PROCESS_START.elapsed().as_secs() < ONLINE_WITHIN as u64);
    for done in body["done"].as_array().into_iter().flatten() {
        if let Some(order) = done.as_str() {
            crate::hud_notify::record(&format!("{name}'s PC has \"{order}\"."), false);
        }
    }
    if let Some(print) = body.get("print").filter(|p| p.is_object()) {
        crate::speaker_id::share_guest(name, print);
    }
    crate::wake_pc::learn(name, body);
    let account = crate::accounts::get(db, name).map(|a| crate::accounts::Account { devices: Vec::new(), ..a });
    let admin = body["admin"].as_str().is_some_and(crate::admin::holds);
    json!({ "orders": take(name, pc), "sir": crate::speaker_id::print_to_share(db), "build": BUILD, "version": VERSION, "os": std::env::consts::OS, "account": account, "admin": admin })
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
    let (mut said_offline, mut github_at) = (false, None::<Instant>);
    loop {
        tokio::time::sleep(EVERY).await;
        let main = app.config().read(|c| c.main_pc.trim().trim_end_matches('/').to_string());
        let Some(key) = key(app.config()) else {
            crate::gui::show_login();
            continue;
        };
        let db = app.config().read(|c| c.memory_db_path.clone());
        let admin = crate::admin::key_held();
        // (The game it plays: for Sir's rules and protocols that wait on this PC.)
        let game = crate::vram_guard::game_now().map(|(game, _)| game);
        let body = json!({ "kind": "pc", "device": pc_name(), "version": release(), "stats": stats(&app), "done": done, "print": crate::speaker_id::print_to_share(&db), "admin": admin, "game": game, "mac": crate::wake_pc::this_pc_mac() });
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
        // A newer release on GitHub, looked for every half hour: it comes even while Sir's PC
        // is out of reach (this PC still holds its key, so its account was good when last heard).
        if github_at.is_none_or(|at| at.elapsed() > Duration::from_secs(1800)) {
            github_at = Some(Instant::now());
            let release = http.get(GITHUB_RELEASE).header("User-Agent", "JAI-client").send().await.ok();
            let release = match release {
                Some(r) => r.json::<Value>().await.ok().as_ref().and_then(release_of),
                None => None,
            };
            if let Some((tag, url)) = release.filter(|(tag, _)| newer(tag)) {
                match update(http.get(&url).header("User-Agent", "JAI-client"), &format!("GitHub ({tag})")).await {
                    Ok(()) => return,
                    Err(e) => warn!("[FLEET] The new version ({tag}) did not come from GitHub: {e}"),
                }
            }
        }
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
        // (Never an older one: a PC that took a newer release from GitHub keeps it.)
        let not_older = answer["version"].as_str().is_some_and(|v| version(v) >= version(VERSION));
        if let Some(build) = answer["build"].as_str().filter(|b| *b != BUILD && !b.is_empty() && same_system && not_older) {
            match update(http.get(format!("{main}/api/pc/update")).header("X-JAI-Key", &key), &format!("Sir's PC ({build})")).await {
                Ok(()) => return,
                Err(e) => warn!("[FLEET] The new version ({build}) did not come: {e}"),
            }
        }
    }
}

/// "v3.4.1" as numbers, to tell a newer version from an older one.
fn version(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.trim().trim_start_matches('v').split(['.', '-', '+']).map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next().flatten().unwrap_or(0)))
}

fn newer(v: &str) -> bool {
    version(v) > version(VERSION)
}

/// A release's version and the address of its file for this system.
fn release_of(v: &Value) -> Option<(String, String)> {
    let asset = v["assets"].as_array()?.iter().find(|a| a["name"] == RELEASE_ASSET)?;
    Some((v["tag_name"].as_str()?.to_string(), asset["browser_download_url"].as_str()?.to_string()))
}

/// A new version, fetched and put in place of this one, then started. (A running program may
/// be renamed on Windows, not overwritten.)
async fn update(request: reqwest::RequestBuilder, from: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    info!("[FLEET] A new version from {from}, this one {VERSION} ({BUILD}): fetching it");
    let bytes = request.timeout(Duration::from_secs(900)).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.bytes().await.map_err(|e| e.to_string())?;
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
    crate::lifecycle::signal(crate::lifecycle::Signal::Alert(format!("A new version of me came from {from}: restarting with it.")));
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
    let mut body = json!({ "name": name, "password": password, "kind": "pc", "device": pc_name(), "version": release() });
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
    fn a_newer_release_on_github_is_taken_and_an_older_never() {
        let release = json!({ "tag_name": "v99.0.0", "assets": [
            { "name": "JAI-setup.exe", "browser_download_url": "https://example.invalid/setup" },
            { "name": RELEASE_ASSET, "browser_download_url": "https://example.invalid/jai" },
        ] });
        assert_eq!(release_of(&release), Some(("v99.0.0".into(), "https://example.invalid/jai".into())));
        assert_eq!(release_of(&json!({ "tag_name": "v99.0.0", "assets": [] })), None);
        assert!(newer("v99.0.0") && newer("99.1"));
        assert!(!newer(&format!("v{VERSION}")) && !newer("v0.9.9") && !newer("nightly"));
        assert!(version("v3.10.0") > version("v3.9.2"));
        assert_eq!(version("v3.4.1-rc1"), Some((3, 4, 1)));
    }

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
        lock(&ORDERS).insert("erika".into(), vec![("open the earth".into(), Instant::now(), Vec::new()), ("old".into(), Instant::now() - ORDER_KEEPS - Duration::from_secs(1), Vec::new())]);
        assert_eq!(take("Erika", "desk0001"), vec!["open the earth".to_string()], "the stale one is dropped");
        assert!(take("erika", "desk0001").is_empty(), "taken once");
        // (Her laptop, logged in with the same account, gets it too: once.)
        assert_eq!(take("erika", "lap00002"), vec!["open the earth".to_string()]);
        assert!(take("erika", "lap00002").is_empty());
        assert!(runs_here(&[b"MZ".as_slice(), &vec![0u8; 1_000_001]].concat()) == cfg!(windows));
        assert!(!runs_here(b"<html>not found</html>"));
    }

    /// What another PC does reaches the rules once: online, a game started and ended, gone.
    #[test]
    fn another_pc_coming_playing_and_going_is_heard_once() {
        let heard = || events().into_iter().filter(|(name, _)| name == "erika").map(|(_, e)| e).collect::<Vec<_>>();
        note("Erika", "desk0001", None, true);
        assert!(heard().is_empty(), "just after J.A.I. started a PC is only noted");
        note("Erika", "desk0001", Some("Minecraft".into()), false);
        assert_eq!(heard(), [PcEvent::Plays("Minecraft".into())]);
        note("erika", "desk0001", Some("Minecraft".into()), false);
        assert!(heard().is_empty(), "the same game again is no news");
        // (Her laptop calling without a game ends nothing on her desktop.)
        note("Erika", "lap00002", None, false);
        assert_eq!(heard(), [PcEvent::Online]);
        note("erika", "desk0001", Some("Minecraft".into()), false);
        assert!(heard().is_empty());
        lock(&SEEN).remove(&("erika".to_string(), "lap00002".to_string()));
        note("Erika", "desk0001", None, false);
        assert_eq!(heard(), [PcEvent::Stops("Minecraft".into())]);
        note("Erika", "desk0001", Some("Fortnite".into()), false);
        assert_eq!(heard(), [PcEvent::Plays("Fortnite".into())]);
        // Unheard of for ONLINE_WITHIN: gone, its game with it.
        lock(&SEEN).get_mut(&("erika".to_string(), "desk0001".to_string())).expect("noted").0 = Instant::now().checked_sub(Duration::from_secs(ONLINE_WITHIN as u64)).expect("a clock");
        assert_eq!(heard(), [PcEvent::Stops("Fortnite".into()), PcEvent::Offline]);
        note("Erika", "desk0001", None, false);
        assert_eq!(heard(), [PcEvent::Online]);
    }
}
