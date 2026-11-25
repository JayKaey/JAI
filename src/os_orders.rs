//! J.A.I.OS by voice (the kit's tool `os_system`): "install Firefox", "uninstall VLC", "update
//! yourself", "are there system updates", "undo the last update", "show the snapshots", "go
//! back to snapshot 12", "finish Secure Boot", "run systemctl restart bluetooth as root". He
//! carries them out through his root core (`os_system`), a job in the background whose lines
//! stand in a block and whose end he tells.
//!
//! What cannot be taken back with a word is asked aloud first and done only on Sir's yes
//! (`ask_first`): removing a package, the way back to a snapshot, Secure Boot's keys, any
//! command as root, a package from the AUR (built by paru as the user, never as root). An
//! update is said by Sir himself ("update yourself"); snap-pac's snapshot comes first, and
//! never during a game. Once a day he looks what waits and tells it.
//!
//! Sir always may. An account needs the switch `os_system` (the accounts window, group
//! "System"): "everything" does not hold it (`accounts::Switches`). Off J.A.I.OS these orders
//! are not taken at all, and what installed and updated before still does.

use crate::kit::say;
use crate::os_system::Order;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

pub const TOOL: &str = "os_system";
/// What an account can be allowed one by one.
pub const ACTIONS: [&str; 9] = ["install", "remove", "update", "check", "undo_update", "snapshots", "rollback", "secure_boot", "run"];

fn re(pattern: &str) -> Regex {
    Regex::new(&format!("(?i){pattern}")).expect("system order pattern")
}

const NAME: &str = r"[a-z0-9][a-z0-9.+-]*(?: [a-z0-9][a-z0-9.+-]*){0,3}";

static UPDATE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:update|upgrade|aktualisiere|aktualisier) (?:yourself|the (?:whole )?system|the os|j\.?a\.?i\.?os|jaios|dich(?: selbst)?|das (?:ganze )?system)$|^(?:install|run|do|start|go ahead with) (?:the |all )?(?:system )?updates$|^(?:mach|mache|starte|installiere|installier) (?:die |alle )?(?:system ?)?updates$|^system ?update$")
});
static CHECK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:are there|any|do you have)(?: any)? (?:new )?(?:system )?updates(?: for (?:the system|you|yourself))?$|^(?:what|which) (?:system )?updates (?:are (?:there|waiting|pending)|wait)$|^check for system updates$|^whats new (?:for|in|on) the system$|^gibt es (?:neue )?(?:system ?)?updates(?: für (?:das system|dich))?$|^welche (?:system ?)?updates (?:gibt es|warten)$")
});
static UNDO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:undo|revert|roll back|take back) (?:the |that |my )?(?:last |latest )?(?:system )?update$|^(?:mach|mache|nimm) (?:das |die )?(?:letzte )?(?:system ?)?update (?:rückgängig|zurück)$|^(?:das )?(?:letzte )?update rückgängig(?: machen)?$")
});
static SNAPSHOTS: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:show|list) (?:me )?(?:the |my |all )?(?:system )?snapshots$|^(?:which|what) snapshots (?:are there|do (?:i|we) have)$|^zeig(?:e)? (?:mir )?(?:die |alle )?(?:system ?)?snapshots$|^welche snapshots gibt es$"));
static ROLLBACK: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:roll back|rollback|go back|return|restore) (?:the system )?to snapshot (?:number )?(?P<n>\d{1,6})$|^(?:geh |gehe |setz(?:e)? das system )?(?:zurück )?(?:zu|auf) snapshot (?:nummer )?(?P<n2>\d{1,6})(?: zurück)?$"));
static SECURE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:finish|complete|set up|enrol|enroll) (?:the )?secure boot(?: enrol+ment| setup| keys)?$|^secure boot (?:fertigstellen|einrichten|abschließen)$|^(?:richte|schließe) secure boot (?:ein|ab)$"));
static SECURE_STATE: LazyLock<Regex> = LazyLock::new(|| re(r"^is secure boot (?:on|enabled|active|set up|finished)$|^secure boot status$|^ist secure boot (?:an|aktiv|eingerichtet|fertig)$"));
static REMOVE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:uninstall|deinstalliere|deinstallier) (?:the )?(?:package |app |program |das paket |paket )?(?P<p>{NAME})$|^(?:remove|delete|entferne|lösche) (?:the |das )?(?:package|paket) (?P<p2>{NAME})$|^(?:remove|entferne) (?P<p3>{NAME}) (?:from the system|vom system)$|^(?:das paket |das programm )?(?P<p4>{NAME}) deinstallieren$"))
});
static INSTALL: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:install|installiere|installier) (?:the |das )?(?:package |app |program |paket |programm )?(?P<p>{NAME}(?: (?:and|und) {NAME})*)$|^(?:das paket |das programm )?(?P<p2>{NAME}) installieren$")));
/// The command's own words are read from what was said as it was said (a path keeps its
/// slashes, an option its capitals).
static RUN: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:\W*(?:hey )?(?:jai|jay)\W+)?(?:please )?(?:(?:run|execute|führe|starte) (?P<c>.+?) (?:as|als) root(?: aus)?|as root,? (?:run |execute )?(?P<c2>.+?)|sudo (?P<c3>.+?))[.!?]*$"));

/// What "install …" is never a package for.
const NOT_PACKAGES: [&str; 22] = ["it", "them", "this", "that", "these", "those", "all", "everything", "something", "anything", "update", "updates", "skill", "skills", "es", "sie", "das", "dies", "alles", "etwas", "new", "neue"];

/// The packages a phrase names: "firefox and obs studio" → firefox, obs-studio.
fn packages(phrase: &str) -> Option<Vec<String>> {
    let names: Vec<String> = phrase.split(" and ").flat_map(|p| p.split(" und ")).map(|p| p.split_whitespace().collect::<Vec<_>>().join("-")).collect();
    let ours = names.iter().all(|n| crate::os_system::valid_package(n)) && !phrase.split_whitespace().any(|w| NOT_PACKAGES.contains(&w));
    ours.then_some(names)
}

/// An order for the system in Sir's words: the tool's arguments. Only on J.A.I.OS.
pub fn command(s: &str, original: &str) -> Option<Value> {
    command_on(s, original, crate::os_system::is_jaios())
}

/// (The tests read the orders wherever they run; `jaios`: the apps' updates are the system's
/// too, as they are there.)
fn command_on(s: &str, original: &str, jaios: bool) -> Option<Value> {
    if !(jaios || cfg!(test)) {
        return None;
    }
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim();
    let number = |c: &regex::Captures| ["n", "n2"].iter().find_map(|n| c.name(n)).and_then(|m| m.as_str().parse::<u32>().ok());
    let named = |c: &regex::Captures, names: &[&str]| names.iter().find_map(|n| c.name(n)).and_then(|m| packages(m.as_str()));
    if let Some(c) = RUN.captures(original.trim()) {
        let command: Vec<&str> = ["c", "c2", "c3"].iter().find_map(|n| c.name(n))?.as_str().split_whitespace().collect();
        return Some(json!({"action": "run", "command": command}));
    }
    let action = |action: &str| Some(json!({ "action": action }));
    if UPDATE.is_match(s) {
        return action("update");
    }
    if CHECK.is_match(s) {
        return action("check");
    }
    if UNDO.is_match(s) {
        return action("undo_update");
    }
    if SNAPSHOTS.is_match(s) {
        return action("snapshots");
    }
    if let Some(c) = ROLLBACK.captures(s) {
        return Some(json!({"action": "rollback", "snapshot": number(&c)?}));
    }
    if SECURE.is_match(s) {
        return action("secure_boot");
    }
    if SECURE_STATE.is_match(s) {
        return action("secure_boot_status");
    }
    if let Some(c) = REMOVE.captures(s) {
        return Some(json!({"action": "remove", "packages": named(&c, &["p", "p2", "p3", "p4"])?}));
    }
    if let Some(c) = INSTALL.captures(s) {
        return Some(json!({"action": "install", "packages": named(&c, &["p", "p2"])?}));
    }
    // "Check for updates", "update my apps", and the yes to "Shall I install them?": on
    // J.A.I.OS the apps come from pacman and Flatpak, which is the system's update.
    if !jaios {
        return None;
    }
    match crate::kit_pc::command(s)?["action"].as_str()? {
        "updates" => action("check"),
        "upgrade" => action("update"),
        _ => None,
    }
}

pub fn only_looks(args: &Value) -> bool {
    matches!(args["action"].as_str().unwrap_or(""), "check" | "snapshots" | "secure_boot_status")
}

/// What an order comes to before anything is touched.
#[derive(Debug, PartialEq)]
enum Step {
    /// Asked aloud first: the question. His yes brings the same order back with `confirmed`.
    Ask(String),
    /// A job of the root core: what he says now, and when it is done.
    Job(Order, String, String),
    /// Packages from the AUR, built by paru as the user.
    Aur(Vec<String>),
}

fn names(args: &Value) -> Result<Vec<String>, String> {
    let names: Vec<String> = args["packages"].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(str::to_string)).collect();
    (!names.is_empty() && names.iter().all(|n| crate::os_system::valid_package(n))).then_some(names).ok_or_else(|| "Which package, Sir?".to_string())
}

/// `missing`: the packages asked for that the repositories do not hold; `in_game`: a game runs.
fn step(args: &Value, missing: &[String], in_game: bool) -> Result<Step, String> {
    let confirmed = args["confirmed"].as_bool().unwrap_or(false);
    let ask = |question: String, order: Order, now: String, done: String| if confirmed { Step::Job(order, now, done) } else { Step::Ask(question) };
    Ok(match args["action"].as_str().unwrap_or("") {
        "install" => {
            let names = names(args)?;
            let list = crate::briefing::join_and(&names);
            if missing.is_empty() {
                Step::Job(Order::Install(names), format!("Installing {list}, Sir."), format!("{list} is installed, Sir."))
            } else if missing.len() < names.len() {
                return Err(format!("{} is not in the repositories, Sir: ask for it on its own and I look in the AUR.", crate::briefing::join_and(missing)));
            } else if confirmed && args["aur"].as_bool() == Some(true) {
                Step::Aur(names)
            } else {
                Step::Ask(format!("{list} is not in the repositories, Sir. Shall I take it from the AUR?"))
            }
        }
        "remove" => {
            let names = names(args)?;
            let list = crate::briefing::join_and(&names);
            ask(format!("Remove {list} and what only it needed, Sir?"), Order::Remove(names), format!("Removing {list}, Sir."), format!("{list} is removed, Sir."))
        }
        "update" if in_game => return Err("Not during a game, Sir. Ask me again when you are done.".to_string()),
        "update" => Step::Job(Order::Update, "Updating, Sir: a snapshot first. I will tell you when it is done.".to_string(), "The system is up to date, Sir. A new kernel starts with the next restart.".to_string()),
        "undo_update" => ask(
            "Take back the last update, Sir? The system returns to how it stood before it.".to_string(),
            Order::UndoLastUpdate,
            "Taking the last update back, Sir.".to_string(),
            "The last update is taken back, Sir. Restart the PC to finish.".to_string(),
        ),
        "rollback" => {
            let n = args["snapshot"].as_u64().filter(|n| (1..=u32::MAX as u64).contains(n)).ok_or("Which snapshot, Sir?")? as u32;
            ask(
                format!("Go back to snapshot {n}, Sir? Everything the system changed since then is undone; your own files stay."),
                Order::Rollback(n),
                format!("Going back to snapshot {n}, Sir."),
                format!("The system stands at snapshot {n} again, Sir. Restart the PC to finish."),
            )
        }
        "secure_boot" => ask(
            "Enrol my own Secure Boot keys beside Microsoft's, Sir? That changes what this PC may start.".to_string(),
            Order::FinishSecureBoot,
            "Enrolling the keys, Sir.".to_string(),
            "The keys are enrolled and the boot files signed, Sir. Switch Secure Boot on in the firmware at the next start.".to_string(),
        ),
        "run" => {
            let command: Vec<String> = args["command"].as_array().into_iter().flatten().filter_map(|w| w.as_str().map(str::to_string)).collect();
            if command.is_empty() {
                return Err("Which command, Sir?".to_string());
            }
            let line = command.join(" ");
            ask(format!("Run \"{line}\" as root, Sir?"), Order::Run(command), format!("Running {} as root, Sir.", line.split(' ').next().unwrap_or("it")), format!("{line} has run, Sir."))
        }
        other => return Err(format!("The system has no action '{other}'.")),
    })
}

/// How many updates wait and which, in one line (None: none).
fn waiting_line(names: &[String]) -> Option<String> {
    let first: Vec<String> = names.iter().take(3).cloned().collect();
    let list = match names.len().saturating_sub(3) {
        0 => crate::briefing::join_and(&first),
        more => format!("{} and {more} more", first.join(", ")),
    };
    match names.len() {
        0 => None,
        1 => Some(format!("One update waits, Sir: {list}. Say \"update yourself\" and I install it.")),
        n => Some(format!("{n} updates wait, Sir: {list}. Say \"update yourself\" and I install them.")),
    }
}

/// Secure Boot as it stands, in a sentence.
fn secure_line((keys, setup_mode, on): (bool, bool, bool)) -> &'static str {
    match (on, setup_mode, keys) {
        (true, _, true) => "Secure Boot is on, Sir, with my own keys beside Microsoft's.",
        (true, _, false) => "Secure Boot is on, Sir, but my keys are not enrolled: the firmware's own still decide.",
        (false, true, _) => "The firmware is in Setup Mode, Sir. Say \"finish Secure Boot\" and I enrol the keys.",
        (false, false, _) => "Secure Boot is not finished, Sir. Restart into the firmware's setup, clear its Secure Boot keys so that it stands in Setup Mode, start again and tell me to finish Secure Boot.",
    }
}

fn outside_is_left_alone() -> bool {
    crate::platform::hands_off() || crate::live_self_tester::dry() || crate::tools::silent_session()
}

/// The last lines of a job, in his block.
fn show(lines: &std::collections::VecDeque<String>) {
    let text: Vec<&str> = lines.iter().map(String::as_str).collect();
    crate::kit::show_text(TOOL, "SYSTEM", &text.join("\n"), [560.0, 300.0]);
}

/// Runs `work` in the background, its lines in the block; its end is told.
fn job(done: String, work: impl FnOnce(&mut dyn FnMut(&str)) -> Result<(), String> + Send + 'static) {
    tokio::task::spawn_blocking(move || {
        let mut lines = std::collections::VecDeque::new();
        let mut line = |text: &str| {
            if lines.len() == 14 {
                lines.pop_front();
            }
            lines.push_back(text.chars().take(96).collect::<String>());
            show(&lines);
        };
        let told = match work(&mut line) {
            Ok(()) => done,
            Err(why) => format!("That did not work, Sir: {}", why.lines().last().unwrap_or("the root core gave no reason").trim()),
        };
        tracing::info!("[OS] {told}");
        crate::announcements::say(told);
    });
}

/// paru as the user; what it needs root for (pacman alone) goes through `jai --as-root`.
fn aur(names: Vec<String>) -> Result<(), String> {
    let paru = crate::apps::which("paru").ok_or("paru is not installed, Sir.")?;
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let done = format!("{} is built and installed, Sir.", crate::briefing::join_and(&names));
    job(done, move |line| {
        let out = std::process::Command::new(paru)
            .args(["-S", "--noconfirm", "--skipreview", "--sudo"])
            .arg(me)
            .args(["--sudoflags", "--as-root", "--"])
            .args(&names)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("paru did not start: {e}"))?;
        let said = String::from_utf8_lossy(if out.status.success() { &out.stdout } else { &out.stderr }).to_string();
        said.lines().filter(|l| !l.trim().is_empty()).for_each(|l| line(l));
        out.status.success().then_some(()).ok_or(said)
    });
    Ok(())
}

/// Which of `names` the repositories do not hold (`pacman -Si`, which needs no root).
async fn missing(names: &[String]) -> Vec<String> {
    let mut missing = Vec::new();
    for name in names {
        let known = crate::shell::run_helper(std::path::Path::new("pacman"), ["-Si", "--", name], None, &[], std::time::Duration::from_secs(20)).await;
        if !known.is_ok_and(|out| out.code == Some(0)) {
            missing.push(name.clone());
        }
    }
    missing
}

pub async fn run(args: &Value) -> Result<String, String> {
    if !crate::os_system::is_jaios() {
        return Err(crate::os_system::NOT_HERE.into());
    }
    match args["action"].as_str().unwrap_or("") {
        "check" => {
            let rows = crate::kit_pc::app_updates().await?;
            let text: Vec<String> = rows.iter().map(|(name, _, from, to)| format!("{name}  {from} → {to}")).collect();
            let names: Vec<String> = rows.into_iter().map(|r| r.0).collect();
            let Some(line) = waiting_line(&names) else { return Ok(say("The system is up to date, Sir.", "The system is up to date, Sir.")) };
            crate::kit::show_text("os_updates", "SYSTEM UPDATES", &text.join("\n"), [460.0, 60.0 + 22.0 * text.len().min(16) as f32]);
            // (His plain yes within the next seconds is the go.)
            crate::hud_lines::offer("update yourself");
            Ok(say(&line, &line))
        }
        "snapshots" => {
            let snaps = tokio::task::spawn_blocking(crate::os_system::snapshots).await.map_err(|e| e.to_string())??;
            let Some(last) = snaps.last() else { return Ok(say("There is no snapshot yet, Sir.", "There is no snapshot yet, Sir.")) };
            let text: Vec<String> = snaps.iter().rev().take(40).map(|s| format!("{:>4}  {:<6}  {}  {}", s.number, s.kind, s.date, s.about)).collect();
            crate::kit::show_text("os_snapshots", "SNAPSHOTS", &text.join("\n"), [640.0, 60.0 + 20.0 * text.len().min(18) as f32]);
            let line = format!("{} snapshots, Sir. The last is number {}, from {}.", snaps.len(), last.number, last.date);
            Ok(say(&line, &line))
        }
        "secure_boot_status" => {
            let line = secure_line(tokio::task::spawn_blocking(crate::os_system::secure_boot).await.map_err(|e| e.to_string())??);
            Ok(say(line, line))
        }
        action => {
            let missing = if action == "install" { missing(&names(args)?).await } else { Vec::new() };
            match step(args, &missing, crate::vram_guard::in_game())? {
                Step::Ask(question) => {
                    let mut yes = args.clone();
                    yes["confirmed"] = json!(true);
                    if action == "install" {
                        yes["aur"] = json!(true);
                    }
                    crate::ask_first::hold(action, TOOL, &yes, &question);
                    Ok(say(&question, &question))
                }
                Step::Job(..) | Step::Aur(_) if outside_is_left_alone() => Ok(say("Noted, not carried out: this session leaves the system alone.", "Noted, Sir.")),
                Step::Job(order, now, done) => {
                    job(done, move |line| crate::os_system::ask(&order, line).map(drop));
                    Ok(say(&now, &now))
                }
                Step::Aur(names) => {
                    let line = format!("Building {} from the AUR, Sir. I will tell you when it is in.", crate::briefing::join_and(&names));
                    aur(names)?;
                    Ok(say(&line, &line))
                }
            }
        }
    }
}

/// Once a day, never during a game: what updates wait, told once. And once per state: how to
/// finish Secure Boot when the firmware would not take the keys at the installation.
pub fn start(stop: tokio_util::sync::CancellationToken) {
    if cfg!(test) || crate::platform::in_test_session() || !crate::os_system::is_jaios() {
        return;
    }
    tokio::spawn(async move {
        let pause = |d: u64| tokio::time::sleep(std::time::Duration::from_secs(d));
        tokio::select! { _ = stop.cancelled() => return, _ = pause(180) => {} }
        if let Ok(Ok(state)) = tokio::task::spawn_blocking(crate::os_system::secure_boot).await {
            let told = crate::kit::get("os_system", "secure_boot").and_then(|v| v.as_str().map(str::to_string));
            let now = format!("{state:?}");
            if !(state.0 && state.2) && told.as_deref() != Some(now.as_str()) {
                crate::announcements::say(secure_line(state));
            }
            let _ = crate::kit::put("os_system", "secure_boot", &json!(now));
        }
        loop {
            let today = chrono::Local::now().date_naive().to_string();
            let told = crate::kit::get("os_system", "updates_told").and_then(|v| v.as_str().map(str::to_string));
            if told.as_deref() != Some(today.as_str()) && !crate::vram_guard::in_game() {
                if let Ok(rows) = crate::kit_pc::app_updates().await {
                    let _ = crate::kit::put("os_system", "updates_told", &json!(today));
                    let names: Vec<String> = rows.into_iter().map(|r| r.0).collect();
                    if let Some(line) = waiting_line(&names) {
                        tracing::info!("[OS] {line}");
                        crate::announcements::say(line);
                    }
                }
            }
            tokio::select! { _ = stop.cancelled() => return, _ = pause(1800) => {} }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routed(said: &str) -> Option<(String, Value)> {
        crate::phrasings::route(&crate::wake::canon(said))
    }

    /// Every order by the road the brain takes it (`phrasings::route`): English, German, with
    /// his name, as a question.
    #[test]
    fn the_systems_orders_are_read_in_sirs_words() {
        let action = |a: &str| json!({ "action": a });
        let cases: Vec<(&str, Value)> = vec![
            ("install firefox", json!({"action": "install", "packages": ["firefox"]})),
            ("Jay, install Firefox please", json!({"action": "install", "packages": ["firefox"]})),
            ("can you install obs studio?", json!({"action": "install", "packages": ["obs-studio"]})),
            ("install the package python-numpy", json!({"action": "install", "packages": ["python-numpy"]})),
            ("install vlc and gimp", json!({"action": "install", "packages": ["vlc", "gimp"]})),
            ("installiere Firefox", json!({"action": "install", "packages": ["firefox"]})),
            ("installier bitte das Paket htop", json!({"action": "install", "packages": ["htop"]})),
            ("uninstall vlc", json!({"action": "remove", "packages": ["vlc"]})),
            ("remove the package vlc", json!({"action": "remove", "packages": ["vlc"]})),
            ("remove vlc from the system", json!({"action": "remove", "packages": ["vlc"]})),
            ("deinstalliere VLC", json!({"action": "remove", "packages": ["vlc"]})),
            ("entferne das Paket vlc", json!({"action": "remove", "packages": ["vlc"]})),
            ("update yourself", action("update")),
            ("JAI, update the system", action("update")),
            ("install the updates", action("update")),
            ("aktualisiere dich", action("update")),
            ("aktualisiere das System", action("update")),
            ("installiere die Updates", action("update")),
            ("are there system updates?", action("check")),
            ("any new updates for the system", action("check")),
            ("which updates are waiting", action("check")),
            ("gibt es Updates für das System?", action("check")),
            ("welche Updates gibt es", action("check")),
            ("undo the last update", action("undo_update")),
            ("roll back the last update", action("undo_update")),
            ("mach das letzte Update rückgängig", action("undo_update")),
            ("show the snapshots", action("snapshots")),
            ("zeig mir die Snapshots", action("snapshots")),
            ("go back to snapshot 12", json!({"action": "rollback", "snapshot": 12})),
            ("geh zurück zu Snapshot 12", json!({"action": "rollback", "snapshot": 12})),
            ("finish Secure Boot", action("secure_boot")),
            ("richte Secure Boot ein", action("secure_boot")),
            ("is Secure Boot on?", action("secure_boot_status")),
            ("run systemctl restart bluetooth as root", json!({"action": "run", "command": ["systemctl", "restart", "bluetooth"]})),
            ("Jay, run pacman -Sc as root.", json!({"action": "run", "command": ["pacman", "-Sc"]})),
            ("sudo chown max /srv/Daten", json!({"action": "run", "command": ["chown", "max", "/srv/Daten"]})),
            ("führe fstrim -av als root aus", json!({"action": "run", "command": ["fstrim", "-av"]})),
        ];
        for (said, args) in cases {
            assert_eq!(routed(said), Some((TOOL.to_string(), args)), "{said}");
        }
    }

    /// What sounds alike and is another order's, or no order at all.
    #[test]
    fn other_orders_stay_where_they_were() {
        for said in ["install it", "install them", "install the update for discord", "remove the timer", "remove that", "update the page", "undo that", "what's new", "show my notes", "run the morning protocol", "is the root of 16 four"] {
            assert_ne!(routed(said).map(|(tool, _)| tool).as_deref(), Some(TOOL), "{said}");
        }
        // Another PC's order is that PC's (`fleet`), whatever it asks for.
        for said in ["install firefox on Max's PC", "update the system on Max's PC", "run reboot as root on Max's PC"] {
            assert_eq!(routed(said).map(|(tool, _)| tool).as_deref(), Some("other_pc"), "{said}");
            assert!(crate::kit::command(&crate::intents::normalize_phrase(said), said).is_some_and(|(tool, _)| tool == "other_pc"), "{said}");
        }
        // The apps' updates are the system's on J.A.I.OS alone; elsewhere they stay the PC helper's.
        let apps = |said: &str, jaios: bool| command_on(said, said, jaios);
        assert_eq!(apps("check for updates", true), Some(json!({"action": "check"})));
        assert_eq!(apps("update my apps", true), Some(json!({"action": "check"})));
        assert_eq!(apps("upgrade all apps with discover", true), Some(json!({"action": "update"})));
        assert_eq!(apps("check for updates", false), None);
        assert_eq!(routed("check for updates").map(|(tool, _)| tool).as_deref(), Some("pc_helper"));
    }

    /// Nothing that cannot be taken back with a word runs before his yes; the yes is the same
    /// order with `confirmed`.
    #[test]
    fn what_is_destructive_is_asked_first() {
        let none: [String; 0] = [];
        let asked = |args: Value| matches!(step(&args, &none, false), Ok(Step::Ask(_)));
        let confirmed = |mut args: Value| {
            args["confirmed"] = json!(true);
            step(&args, &none, false)
        };
        for args in [json!({"action": "remove", "packages": ["vlc"]}), json!({"action": "undo_update"}), json!({"action": "rollback", "snapshot": 12}), json!({"action": "secure_boot"}), json!({"action": "run", "command": ["rm", "-rf", "/srv/alt"]})] {
            assert!(asked(args.clone()), "{args} must be asked first");
            assert!(matches!(confirmed(args.clone()), Ok(Step::Job(..))), "{args} runs on his yes");
        }
        assert_eq!(confirmed(json!({"action": "run", "command": ["rm", "-rf", "/srv/alt"]})).map(|s| matches!(s, Step::Job(Order::Run(c), ..) if c == ["rm", "-rf", "/srv/alt"])), Ok(true));
        assert!(matches!(step(&json!({"action": "rollback", "snapshot": 12}), &none, false), Ok(Step::Ask(q)) if q.contains("snapshot 12")));
        // From the repositories at once; from the AUR only after the question, and never as a job of root.
        assert!(matches!(step(&json!({"action": "install", "packages": ["firefox"]}), &none, false), Ok(Step::Job(Order::Install(_), ..))));
        let aur = ["spotify".to_string()];
        assert!(matches!(step(&json!({"action": "install", "packages": ["spotify"]}), &aur, false), Ok(Step::Ask(q)) if q.contains("AUR")));
        assert!(matches!(step(&json!({"action": "install", "packages": ["spotify"], "confirmed": true}), &aur, false), Ok(Step::Ask(_))), "a yes to something else is no yes to the AUR");
        assert_eq!(step(&json!({"action": "install", "packages": ["spotify"], "confirmed": true, "aur": true}), &aur, false), Ok(Step::Aur(vec!["spotify".into()])));
        assert!(step(&json!({"action": "install", "packages": ["firefox", "spotify"]}), &aur, false).is_err());
        // The update is his own word, but never during a game.
        assert!(matches!(step(&json!({"action": "update"}), &none, false), Ok(Step::Job(Order::Update, ..))));
        assert!(step(&json!({"action": "update"}), &none, true).unwrap_err().contains("game"));
        // No package, no command, no snapshot: a question back, nothing run.
        for args in [json!({"action": "install"}), json!({"action": "remove", "packages": ["--nodeps"]}), json!({"action": "run", "command": []}), json!({"action": "rollback"}), json!({"action": "format"})] {
            assert!(step(&args, &none, false).is_err(), "{args}");
        }
    }

    #[test]
    fn the_question_waits_for_his_yes_and_elsewhere_nothing_is_taken() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        // Off J.A.I.OS the tool says so (and a test never reaches a root core).
        if !crate::os_system::is_jaios() {
            assert_eq!(rt.block_on(run(&json!({"action": "update"}))), Err(crate::os_system::NOT_HERE.to_string()));
        }
        // The held call is the order itself, confirmed: what his yes carries out.
        crate::ask_first::hold("remove", TOOL, &json!({"action": "remove", "packages": ["vlc"], "confirmed": true}), "Remove vlc and what only it needed, Sir?");
        assert!(matches!(crate::ask_first::answer("yes"), Some(crate::ask_first::Answer::Yes(held)) if held.tool == TOOL && held.args["confirmed"] == json!(true)));
        crate::ask_first::hold("remove", TOOL, &json!({}), "Remove vlc and what only it needed, Sir?");
        assert_eq!(crate::ask_first::answer("no"), Some(crate::ask_first::Answer::No));
        assert!(only_looks(&json!({"action": "check"})) && !only_looks(&json!({"action": "update"})) && !crate::kit::stays_inside(TOOL, &json!({"action": "install"})));
    }

    #[test]
    fn what_waits_and_how_secure_boot_stands_is_said_in_a_line() {
        let names = |n: usize| (1..=n).map(|i| format!("paket{i}")).collect::<Vec<_>>();
        assert_eq!(waiting_line(&[]), None);
        assert!(waiting_line(&names(1)).unwrap().starts_with("One update waits, Sir: paket1."));
        assert!(waiting_line(&names(14)).unwrap().starts_with("14 updates wait, Sir: paket1, paket2, paket3 and 11 more."));
        assert!(secure_line((true, false, true)).contains("is on"));
        assert!(secure_line((false, true, false)).contains("finish Secure Boot"));
        assert!(secure_line((true, false, false)).contains("Setup Mode"));
    }

    /// The switch in an account's list: off until Sir gives it, and "everything" does not hold it.
    #[test]
    fn root_is_a_switch_of_its_own() {
        let update = json!({"action": "update"});
        let mut all = crate::accounts::Switches { allow: vec!["*".into()], ..Default::default() };
        assert!(all.refusal("open_application", &json!({})).is_none() && all.refusal(TOOL, &update).is_some() && !all.allows(TOOL));
        all.set(TOOL, true);
        assert!(all.refusal(TOOL, &update).is_none() && all.allows(TOOL));
        let mut one = crate::accounts::Switches::default();
        one.set(&format!("{TOOL}:action=install"), true);
        assert!(one.refusal(TOOL, &json!({"action": "install", "packages": ["vlc"]})).is_none() && one.refusal(TOOL, &update).is_some());
        let grant = crate::accounts::grants(&[]).into_iter().find(|g| g.tool == TOOL).expect("the switch is in the list");
        assert!(grant.values.contains(&"action=run".to_string()));
        assert!(crate::tools::JAI_CAPABILITY_MANIFEST.iter().any(|c| c.name == "System" && c.tools.contains(&TOOL)));
    }
}
