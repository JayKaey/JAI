//! Sir's admin (2026-11-20: "a clearly laid out menu of every window that JAI can open … the
//! windows that only I can use"). The admin window lists every window J.A.I. opens and opens
//! each with a click, his own first (`WINDOWS`, drawn by `gui_admin`).
//!
//! Sir's main PC (no `main_pc` in its settings) keeps the accounts and his admin account, made
//! there once: until then it answers nobody (`needs_setup`), from then on it is his. Every other
//! PC of his (`fleet`) is logged in with an account and does what that account may on its own
//! PC: Sir's switches in the accounts list, which it hears with each call to his PC (what it
//! may on his PC is apart: `guest_refusal`). A new account talks to J.A.I. and nothing more;
//! one allowed everything there opens his settings, the Feature Lab and Discord there too, but
//! never his admin window, the accounts or their report (`FLEET`), and gives no orders to other
//! PCs. A window it may not open does not open: a pop-up asks for Sir's
//! admin sign-in instead, without touching the PC's own login, and that window is his until it
//! closes. The admin window ("activate admin mode") makes everything his while it is open, his
//! accounts and his orders to other PCs included (through his main PC: `answer`). When the last
//! window his sign-in opened closes, the sign-in ends, here and at his main PC at once.

use crate::accounts::{self, Account};
use crate::config::ConfigStore;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// What a call this PC's account may not make is told.
pub const NOT_YOURS: &str = "Sir has not given you that permission.";
/// … and a window of Sir's, while his sign-in is asked for it.
pub const SIGN_IN: &str = "That is Sir's: the window for his admin sign-in is open.";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------------ his windows ----

/// How the admin window opens one of the list.
#[derive(Clone, Copy)]
pub enum Open {
    /// An order, carried out as if typed (and checked like one).
    Order(&'static str),
    /// One of the HUD's own that no order opens.
    Now(fn()),
}

pub struct Window {
    pub id: &'static str,
    pub title: &'static str,
    pub about: &'static str,
    /// Its group in the admin window (`GROUPS`).
    pub group: usize,
    pub open: Open,
    /// The calls that are this window's: tool (`*`: any), option and value (`true`: a switch
    /// turned on; no option: any call of the tool). Sir's own windows list what lives in them
    /// too (the Tool Forge in the Feature Lab, Discord's calls).
    calls: &'static [(&'static str, &'static str, &'static str)],
    /// Sir's own: no single switch of an account opens it; everything allowed on its own PC
    /// does there, but for `FLEET`.
    pub sirs: bool,
}

/// Sir's on every PC, whatever an account may: his admin window, the accounts and their report.
pub const FLEET: [&str; 3] = ["admin", "accounts", "account_report"];

const fn w(id: &'static str, title: &'static str, about: &'static str, group: usize, open: Open, calls: &'static [(&'static str, &'static str, &'static str)], sirs: bool) -> Window {
    Window { id, title, about, group, open, calls, sirs }
}

/// The admin window's groups, in their order.
pub const GROUPS: [&str; 5] = ["Only you", "Views", "Panels", "Look and sound", "J.A.I."];

/// Every window J.A.I. opens, Sir's own first.
pub static WINDOWS: [Window; 30] = [
    w("admin", "Admin", "This menu. On another PC, all is yours while it is open.", 0, Open::Order("open the admin window"), &[("admin", "action", "show")], true),
    w("accounts", "Accounts", "Everyone who may use J.A.I.: passwords, permissions, PCs.", 0, Open::Order("open the accounts list"), &[("accounts", "action", "show")], true),
    w("account_report", "Report", "What J.A.I. did for everyone this week.", 0, Open::Order("what did you do for everyone this week"), &[("accounts", "action", "report")], true),
    w("settings", "Settings", "Every setting of his look, his voice and his presence.", 0, Open::Order("open settings"), &[("*", "settings_panel", "true")], true),
    w("hotkeys", "Hotkeys", "Keys that work in every program and game, mute included.", 0, Open::Order("open the hotkeys window"), &[("*", "hotkeys_window", "true")], true),
    w("feature_lab", "Feature Lab", "Every tool he has, each with TRY; the Tool Forge.", 0, Open::Order("open the feature lab"), &[("*", "feature_lab", "true"), ("forge_tool", "", ""), ("delete_forged_tool", "", "")], true),
    w("discord", "Discord", "His voice into a Discord call: the audio routing.", 0, Open::Now(crate::gui::show_discord), &[("discord_call", "", ""), ("discord_bot", "", "")], true),
    w("system_stats", "System stats", "CPU, graphics, memory, temperatures, busy programs.", 1, Open::Order("open my system stats"), &[("open_jai_view", "view", "system_stats")], false),
    w("schedule", "Schedule", "The timetable and the calendar.", 1, Open::Order("open my calendar"), &[("open_jai_view", "view", "schedule")], false),
    w("contacts", "Contacts", "The saved contacts.", 1, Open::Order("open my contacts"), &[("open_jai_view", "view", "contacts")], false),
    w("workout_finance", "Gym and money", "Gym records and the finance tracker.", 1, Open::Order("open my gym logs"), &[("open_jai_view", "view", "workout_finance")], false),
    w("simulation_lab", "Simulation lab", "Physics and maths to play with.", 1, Open::Order("open the simulation lab"), &[("open_jai_view", "view", "simulation_lab"), ("launch_interactive_simulation", "", "")], false),
    w("mobile_qr", "Phone link", "The QR code that pairs a phone.", 1, Open::Order("show the qr code"), &[("open_jai_view", "view", "mobile_qr"), ("show_mobile_satellite_instructions", "", "")], false),
    w("earth", "Earth", "The globe: quakes, storms, flights, satellites.", 2, Open::Order("show me the earth"), &[("open_jai_view", "view", "earth")], false),
    w("network_radar", "Network radar", "Who is on the network.", 2, Open::Order("open the network radar"), &[("open_jai_view", "view", "network_radar")], false),
    w("disk_map", "Disk map", "What takes the space on a drive.", 2, Open::Order("open the disk map"), &[("open_jai_view", "view", "disk_map")], false),
    w("departures", "Departures", "The next trains from a station.", 2, Open::Order("show the departures"), &[("open_jai_view", "view", "departures")], false),
    w("image_viewer", "Pictures", "Pictures and screenshots.", 2, Open::Order("show my latest screenshot"), &[("show_image", "", "")], false),
    w("webcams", "Live cameras", "Public cameras near a place.", 2, Open::Order("show me the camera nearest to me"), &[("show_live_camera", "", "")], false),
    w("street_view", "Street view", "A street as it looks.", 2, Open::Order("street view here"), &[("show_street_view", "", "")], false),
    w("model3d", "3D model", "Any thing in 3D, turned by hand.", 2, Open::Order("show me a 3d model of the earth"), &[("show_3d_model", "", "")], false),
    w("theme_studio", "Theme studio", "A theme of your own colours.", 3, Open::Order("open the theme studio"), &[("*", "theme_studio", "true")], false),
    w("mixer", "Mixer", "A fader for every program that plays sound.", 3, Open::Order("open the mixer"), &[("*", "mixer_view", "true")], false),
    w("screens", "Screens", "Which monitor he stands on.", 3, Open::Now(crate::gui::show_screens), &[], false),
    w("integrations_hub", "Hub", "Mail, calendars, school and feeds, set up.", 4, Open::Now(crate::gui::show_hub), &[], false),
    w("devices", "Devices", "Microphone, speaker, wake word, memory.", 4, Open::Now(crate::gui::show_devices), &[], false),
    w("missed", "Missed", "The notifications of the last hours.", 4, Open::Order("show my notifications"), &[("*", "missed", "true"), ("notification_history", "", "")], false),
    w("voice_setup", "Your voice", "He learns your voice, sentence by sentence.", 4, Open::Order("learn my voice"), &[("*", "voice_setup", "true")], false),
    w("changes", "Changes", "What is new in J.A.I., day by day.", 4, Open::Order("show the changes"), &[("show_changes", "", "")], false),
    w("notebook", "Notebook", "The notes.", 4, Open::Order("open my notebook"), &[("notebook", "action", "show")], false),
];

pub fn window(id: &str) -> Option<&'static Window> {
    WINDOWS.iter().find(|w| w.id == id)
}

/// The switch in an account that allows a window on its PC.
pub fn key(id: &str) -> String {
    format!("window:{id}")
}

/// Opens a window of the list as its tile in the admin window does.
pub fn open(w: &Window) {
    match w.open {
        Open::Order(order) => crate::gui::run_order(order.to_string(), false),
        Open::Now(open) => open(),
    }
}

/// The window of the list a call is about, and whether it opens it (or uses what is in it)
/// rather than closing it.
pub fn window_of(tool: &str, args: &Value) -> Option<(&'static Window, bool)> {
    // ("view": "the calendar" is the schedule.)
    let view = if tool == "open_jai_view" { args["view"].as_str().and_then(crate::jai_views::JaiSubWindow::parse).map(|v| v.id()) } else { None };
    WINDOWS.iter().find_map(|w| {
        w.calls.iter().find_map(|&(t, option, value)| {
            if t != "*" && t != tool {
                return None;
            }
            let given = &args[option];
            match (option, value) {
                ("", _) => Some((w, true)),
                (_, "true") => given.as_bool().map(|on| (w, on)),
                ("view", _) if tool == "open_jai_view" => (view == Some(value)).then_some((w, true)),
                _ => given.as_str().is_some_and(|g| g.trim().eq_ignore_ascii_case(value)).then_some((w, true)),
            }
        })
    })
}

/// Calls that only close something of his: anyone at the PC may.
fn only_closes(tool: &str, args: &Value) -> bool {
    let action = args["action"].as_str().unwrap_or("");
    tool == "close_native_window" || (tool == "close_application" && args["own_only"].as_bool() == Some(true)) || (matches!(tool, "hud_board" | "accounts" | "admin") && matches!(action, "close" | "close_all" | "clear"))
}

// ------------------------------------------------------------- this PC and its account ----

/// Whether this J.A.I. is another PC of Sir's, not his main PC.
fn other_pc(config: &ConfigStore) -> bool {
    crate::fleet::is_client(&config.read(|c| c.main_pc.clone()))
}

/// The account this PC is logged in with, as Sir's PC last described it; sealed in the vault
/// between starts, so that his switches hold while his PC is off. Read once, then from here.
static ACCOUNT: Mutex<Option<Option<Account>>> = Mutex::new(None);

pub fn account(config: &ConfigStore) -> Option<Account> {
    lock(&ACCOUNT).get_or_insert_with(|| crate::fleet::vault(config).and_then(|v| v.secret(crate::fleet::SESSION, "account")).and_then(|j| serde_json::from_str(&j).ok())).clone()
}

/// What Sir's PC says of this PC's account with every call (`fleet::client`); None: logged out.
pub fn hold(config: &ConfigStore, account: Option<Account>) {
    let mut held = lock(&ACCOUNT);
    if held.as_ref() == Some(&account) {
        return;
    }
    if let Some(vault) = crate::fleet::vault(config) {
        match account.as_ref().and_then(|a| serde_json::to_string(a).ok()) {
            Some(json) => {
                if let Err(e) = vault.set_secret(crate::fleet::SESSION, "account", &json) {
                    warn!("[ADMIN] This PC's account is not kept: {e}");
                }
            }
            None => vault.clear_secret(crate::fleet::SESSION, "account"),
        }
    }
    *held = Some(account);
}

/// Whether an account opens a window of the list on its own PC: by its switch, and one of Sir's
/// own only with everything allowed there (Sir, 2026-11-20: "only if i set their account
/// settings so that they are allowed to do everything that i can do but only on their pc").
pub fn opens(a: &Account, w: &Window) -> bool {
    !FLEET.contains(&w.id) && (!w.sirs || a.own.everything()) && a.own.allows(&key(w.id))
}

/// Why whoever sits at another PC of Sir's may not make this call (None: they may; on his main
/// PC everything may). A window of the list that is not theirs asks for his sign-in.
pub fn refusal(config: &ConfigStore, tool: &str, args: &Value) -> Option<String> {
    if !other_pc(config) || admin_mode() || only_closes(tool, args) {
        return None;
    }
    if let Some((w, opens_it)) = window_of(tool, args) {
        if !opens_it || granted(w.id) || account(config).is_some_and(|a| opens(&a, w) && !a.own.denies(tool, args)) {
            return None;
        }
        ask(w.id, Some((tool.to_string(), args.clone())));
        return Some(SIGN_IN.into());
    }
    // (His accounts and his orders to other PCs: through his main PC, in admin mode, or the
    // accounts while his list is open here.)
    if tool == "accounts" && granted("accounts") {
        return None;
    }
    if matches!(tool, "accounts" | "other_pc") {
        ask("admin", Some((tool.to_string(), args.clone())));
        return Some(SIGN_IN.into());
    }
    match account(config) {
        Some(a) => a.own.refusal(tool, args),
        None => Some(NOT_YOURS.into()),
    }
}

/// Why an account may not make this call on Sir's main PC (his phone page, its voice there): its
/// switches for his PC, never a window of his own (None: it may).
pub fn guest_refusal(db: &str, guest: &str, tool: &str, args: &Value) -> Option<String> {
    match accounts::get(db, guest) {
        Some(a) => a.here.refusal(tool, args).or_else(|| window_of(tool, args).filter(|(w, opens)| w.sirs && *opens).map(|_| NOT_YOURS.to_string())),
        None => Some(NOT_YOURS.into()),
    }
}

/// Whether whoever sits at another PC of Sir's may switch it off, restart it, put it to sleep
/// or lock it.
pub fn may_power(config: &ConfigStore) -> bool {
    !other_pc(config) || admin_mode() || account(config).is_some_and(|a| a.own.allows(accounts::POWER))
}

/// What whoever sits at another PC of Sir's is told instead of an answer: the account is
/// paused, or Sir closed that topic to it.
pub fn turned_away(config: &ConfigStore, text: &str) -> Option<String> {
    // (A window his sign-in was given for opens by its tile's order, whatever the account may.)
    if !other_pc(config) || admin_mode() || WINDOWS.iter().any(|w| matches!(w.open, Open::Order(order) if order == text) && granted(w.id)) {
        return None;
    }
    account(config)?.turned_away(text)
}

/// Sir's own order from his main PC (`Caller::Remote`): a window it opens here is his until it
/// closes. (Not the admin window: that is opened at the PC, with his sign-in.)
pub fn sirs_call(tool: &str, args: &Value) {
    if let Some((w, true)) = window_of(tool, args).filter(|(w, _)| w.id != "admin") {
        let mut state = lock(&STATE);
        state.grants.retain(|g| g.id != w.id);
        state.grants.push(Grant { id: w.id, seen: false, at: Instant::now() });
    }
}

// ------------------------------------------------------------- his sign-in on this PC ----

/// A window his sign-in opened here: seen open since, and when it was given.
struct Grant {
    id: &'static str,
    seen: bool,
    at: Instant,
}

/// The call to carry out once he is signed in (none: the window opens its own way).
type Then = Option<(String, Value)>;

struct State {
    /// His main PC's address and his key there, while his sign-in holds on this PC.
    key: Option<(String, String)>,
    grants: Vec<Grant>,
    /// The window his sign-in is asked for.
    asking: Option<(&'static str, Then)>,
    /// The windows of the list up on the last frame that were allowed.
    up: Vec<&'static str>,
}

static STATE: Mutex<State> = Mutex::new(State { key: None, grants: Vec::new(), asking: None, up: Vec::new() });
/// What his sign-in's calls to his main PC run on (the HUD's runtime).
static RT: OnceLock<tokio::runtime::Handle> = OnceLock::new();
/// The tests that change this PC's account and his sign-in hold this.
#[cfg(test)]
pub(crate) static TESTS: Mutex<()> = Mutex::new(());
/// How long a window his sign-in opened may take to appear.
const GRANT_WAITS: Duration = Duration::from_secs(20);

/// Whether the admin window is his here: everything is, while it is open.
pub fn admin_mode() -> bool {
    lock(&STATE).grants.iter().any(|g| g.id == "admin")
}

fn granted(id: &str) -> bool {
    lock(&STATE).grants.iter().any(|g| g.id == id || g.id == "admin")
}

fn ask(id: &'static str, then: Then) {
    lock(&STATE).asking = Some((id, then));
    crate::gui::show_admin_sign_in();
}

/// The window his sign-in is asked for, while it is.
pub fn asking() -> Option<&'static Window> {
    lock(&STATE).asking.as_ref().and_then(|(id, _)| window(id))
}

/// The pop-up closed without a sign-in.
pub fn cancel() {
    lock(&STATE).asking = None;
}

/// The windows of the list that were up on the HUD's last frame (the admin window's OPEN).
pub fn up() -> Vec<&'static str> {
    lock(&STATE).up.clone()
}

/// One frame of the HUD on another PC of Sir's, with the windows of the list that are up: those
/// to close now. A window that has just opened and is not this PC's asks for his sign-in; one
/// that lost its right (his sign-in ended) closes without a word. Once the last window his
/// sign-in opened is gone, the sign-in ends, at his main PC too.
pub fn gate(config: &ConfigStore, rt: &tokio::runtime::Handle, up: &[&'static str]) -> Vec<&'static str> {
    RT.get_or_init(|| rt.clone());
    if !other_pc(config) {
        lock(&STATE).up = up.to_vec();
        return Vec::new();
    }
    let account = account(config);
    let mut state = lock(&STATE);
    for g in &mut state.grants {
        g.seen |= up.contains(&g.id);
    }
    state.grants.retain(|g| if g.seen { up.contains(&g.id) } else { g.at.elapsed() < GRANT_WAITS });
    let mode = state.grants.iter().any(|g| g.id == "admin");
    let (mut shut, mut tried) = (Vec::new(), None);
    for &id in up {
        let Some(w) = window(id) else { continue };
        let allowed = mode || state.grants.iter().any(|g| g.id == id) || account.as_ref().is_some_and(|a| opens(a, w));
        if !allowed {
            shut.push(id);
            if !state.up.contains(&id) {
                tried = Some(id);
            }
        }
    }
    state.up = up.iter().copied().filter(|id| !shut.contains(id)).collect();
    let tried = tried.filter(|_| state.asking.is_none());
    if let Some(id) = tried {
        state.asking = Some((id, None));
    }
    let ended = if state.grants.is_empty() { state.key.take() } else { None };
    drop(state);
    if let Some((main, key)) = ended {
        give_back(main, key);
    }
    if tried.is_some() {
        crate::gui::show_admin_sign_in();
    }
    shut
}

/// His sign-in ends: his main PC forgets its key at once.
fn give_back(main: String, key: String) {
    info!("[ADMIN] Sir's sign-in on this PC ends");
    let Some(rt) = RT.get() else { return };
    rt.spawn(async move {
        if let Err(e) = post(&main, "/api/admin", Some(&key), json!({ "action": "logout" })).await {
            warn!("[ADMIN] Sir's PC did not hear that the sign-in ended (it lapses in two minutes): {e}");
        }
    });
}

/// Sir signs in at the pop-up; his main PC checks name and password. The window asked for is
/// his here from now on (and opens) until it closes. What to carry out now: the call that asked
/// for it, if a call did.
pub async fn sign_in(config: &ConfigStore, name: &str, password: &str) -> Result<Then, String> {
    let main = config.read(|c| c.main_pc.trim().trim_end_matches('/').to_string());
    let answer = post(&main, "/api/admin/login", None, json!({ "name": name.trim(), "password": password })).await?;
    let key = answer["key"].as_str().filter(|k| k.len() == 64).ok_or("Sir's PC gave no key.")?.to_string();
    let mut state = lock(&STATE);
    let Some((id, then)) = state.asking.take() else {
        drop(state);
        give_back(main, key);
        return Err("Nothing waits for your sign-in any more.".into());
    };
    let old = state.key.replace((main, key));
    state.grants.retain(|g| g.id != id);
    state.grants.push(Grant { id, seen: false, at: Instant::now() });
    drop(state);
    if let Some((main, key)) = old {
        give_back(main, key);
    }
    info!("[ADMIN] Sir signed in on this PC for '{id}'");
    // (The admin window opens as well when the call was one of his accounts or orders.)
    if then.as_ref().is_none_or(|(tool, _)| id == "admin" && tool != "admin") {
        if let Some(w) = window(id) {
            open(w);
        }
    }
    Ok(then)
}

/// His key, to keep his sign-in alive at his main PC with this PC's every call there.
pub fn key_held() -> Option<String> {
    lock(&STATE).key.as_ref().map(|(_, key)| key.clone())
}

/// His main PC's word on the key a call carried (`sent`): gone (it restarted, or it lapsed),
/// everything his sign-in opened closes on the next frame. (A sign-in made while that call was
/// on its way is not what it answered about.)
pub fn heard(sent: Option<&str>, holds: bool) {
    let mut state = lock(&STATE);
    if !holds && sent.is_some() && state.key.as_ref().map(|(_, key)| key.as_str()) == sent {
        warn!("[ADMIN] Sir's PC no longer knows his sign-in here: it ends");
        state.key = None;
        state.grants.clear();
    }
}

/// One call to Sir's main PC under his sign-in on this PC.
pub async fn remote(body: Value) -> Result<Value, String> {
    let (main, key) = lock(&STATE).key.clone().ok_or("Your admin sign-in on this PC has ended.")?;
    post(&main, "/api/admin", Some(&key), body).await
}

async fn post(main: &str, path: &str, key: Option<&str>, body: Value) -> Result<Value, String> {
    if !main.starts_with("https://") && !main.starts_with("http://") {
        return Err("This PC does not know Sir's PC's address.".into());
    }
    let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    let mut request = http.post(format!("{main}{path}")).json(&body);
    if let Some(key) = key {
        request = request.header("X-JAI-Key", key);
    }
    let response = request.send().await.map_err(|e| format!("Sir's PC is not reached: {e}"))?;
    let ok = response.status().is_success();
    let answer: Value = response.json().await.map_err(|e| e.to_string())?;
    if ok {
        Ok(answer)
    } else {
        Err(answer["error"].as_str().unwrap_or("That did not work.").to_string())
    }
}

/// Sir's accounts as his main PC last sent them here (his accounts list in admin mode).
#[derive(Clone)]
pub struct Remote {
    pub accounts: Vec<Account>,
    pub waiting: Vec<(String, String, u64)>,
    /// What J.A.I. did this week for the account unfolded.
    pub log: Vec<accounts::Done>,
    /// A password he asked to see: whose, and what.
    pub shown: Option<(String, String)>,
    /// What went wrong last, if anything.
    pub error: String,
    at: Option<Instant>,
    busy: bool,
}

static REMOTE: Mutex<Remote> = Mutex::new(Remote { accounts: Vec::new(), waiting: Vec::new(), log: Vec::new(), shown: None, error: String::new(), at: None, busy: false });

/// The accounts at Sir's main PC for his list on this PC: what came last, and another look
/// every two seconds while the list asks.
pub fn remote_accounts(chosen: &str) -> Remote {
    let mut remote = lock(&REMOTE);
    if let Some(rt) = RT.get().filter(|_| !remote.busy && remote.at.is_none_or(|at| at.elapsed() > Duration::from_secs(2))) {
        remote.busy = true;
        let chosen = chosen.to_string();
        rt.spawn(async move {
            let got = self::remote(json!({ "action": "accounts", "name": chosen })).await;
            let mut remote = lock(&REMOTE);
            (remote.busy, remote.at) = (false, Some(Instant::now()));
            match got {
                Ok(v) => {
                    remote.accounts = serde_json::from_value(v["accounts"].clone()).unwrap_or_default();
                    remote.waiting = serde_json::from_value(v["waiting"].clone()).unwrap_or_default();
                    remote.log = serde_json::from_value(v["log"].clone()).unwrap_or_default();
                    remote.error.clear();
                }
                Err(e) => remote.error = e,
            }
            drop(remote);
            crate::gui::refresh();
        });
    }
    remote.clone()
}

/// A change to the accounts at Sir's main PC from his list here (one of `answer`'s actions);
/// the list looks again once it is done.
pub fn remote_change(body: Value) {
    let Some(rt) = RT.get() else { return };
    // (A switch shows its new state at once, not when the list comes again.)
    if let Ok(changed) = serde_json::from_value::<Account>(body["account"].clone()) {
        if let Some(a) = lock(&REMOTE).accounts.iter_mut().find(|a| a.name == changed.name) {
            *a = changed;
        }
    }
    rt.spawn(async move {
        let name = body["name"].as_str().unwrap_or("").to_string();
        let password = body["action"] == "password";
        let got = remote(body).await;
        let mut remote = lock(&REMOTE);
        remote.at = None;
        match got {
            Ok(v) if password => remote.shown = v["password"].as_str().map(|p| (name, p.to_string())),
            Ok(_) => remote.error.clear(),
            Err(e) => remote.error = e,
        }
        drop(remote);
        crate::gui::refresh();
    });
}

/// Hides a password shown in the list.
pub fn hide_password() {
    lock(&REMOTE).shown = None;
}

// ------------------------------------------------------------ his main PC: the setup ----

/// Whether this is Sir's main PC without his admin account yet: it answers nobody until he
/// makes it. (A scripted session and the tests go on without one.)
pub fn needs_setup(config: &ConfigStore) -> bool {
    !cfg!(test) && !crate::platform::in_test_session() && !other_pc(config) && accounts::admin(&config.read(|c| c.memory_db_path.clone())).is_none()
}

/// Sir makes his admin account on his main PC (the pop-up, once).
pub fn set_up(config: &ConfigStore, name: &str, password: &str) -> Result<(), String> {
    let db = config.read(|c| c.memory_db_path.clone());
    let vault = crate::vault::Vault::open(&db)?;
    accounts::make_admin(&db, &vault, name, password)
}

// ----------------------------------------------------------- his main PC: the sign-ins ----

/// Sir's sign-ins on his other PCs: key → when that PC last called about it.
static KEYS: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Default::default);
/// A sign-in its PC no longer calls about is void (it calls every few seconds while it holds).
const KEY_LASTS: Duration = Duration::from_secs(120);

/// Sir signs in on another PC (`/api/admin/login`): a key for that PC until it gives it back.
pub fn grant_key(db: &str, vault: &crate::vault::Vault, name: &str, password: &str, from: &str) -> Result<String, String> {
    let account = accounts::verify(db, vault, name, password, from)?;
    if !account.admin {
        return Err("That is not Sir's admin account.".into());
    }
    let key = accounts::fresh_key();
    lock(&KEYS).insert(key.clone(), Instant::now());
    info!("[ADMIN] Sir signed in on another PC ({from})");
    Ok(key)
}

/// Whether `key` is one of Sir's sign-ins that holds (asked about now, it holds a while longer).
pub fn holds(key: &str) -> bool {
    let mut keys = lock(&KEYS);
    keys.retain(|_, at| at.elapsed() < KEY_LASTS);
    keys.get_mut(key).map(|at| *at = Instant::now()).is_some()
}

/// What Sir asks of his main PC from another PC he is signed in on (`/api/admin`). His own
/// admin account is changed on his main PC alone.
pub fn answer(db: &str, key: &str, body: &Value) -> Result<Value, String> {
    let name = body["name"].as_str().unwrap_or("").trim();
    let other = || accounts::get(db, name).filter(|a| !a.admin).ok_or_else(|| format!("There is no account called {name}."));
    let vault = || crate::vault::Vault::open(db);
    match body["action"].as_str().unwrap_or("") {
        "logout" => {
            lock(&KEYS).remove(key);
            info!("[ADMIN] Sir's sign-in on another PC ended");
            Ok(json!({ "ok": true }))
        }
        "accounts" => {
            let log = if name.is_empty() { Vec::new() } else { accounts::log(db, Some(name), accounts::now() - 7 * 86_400) };
            Ok(json!({ "accounts": accounts::all(db), "waiting": accounts::waiting(), "log": log }))
        }
        "save" => {
            let given: Account = serde_json::from_value(body["account"].clone()).map_err(|e| e.to_string())?;
            accounts::update(db, &other()?.name, |a| (a.own, a.here, a.topics, a.until) = (given.own, given.here, given.topics, given.until))?;
            Ok(json!({ "ok": true }))
        }
        "password" => Ok(json!({ "password": accounts::password(&vault()?, &other()?.name) })),
        "remove" => {
            accounts::remove(db, &vault()?, &other()?.name);
            Ok(json!({ "ok": true }))
        }
        "refuse" => {
            accounts::refuse(name);
            Ok(json!({ "ok": true }))
        }
        "act" => accounts::act(db, &body["args"]).map(|(said, text)| json!({ "said": said, "text": text })),
        "order" => crate::fleet::send(db, &body["args"]).map(|said| json!({ "said": said })),
        other => Err(format!("There is no admin action '{other}'.")),
    }
}

// ------------------------------------------------------------------------ his orders ----

static OPENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:open|show|bring\s+up|display|start)\s+(?:me\s+)?(?:the\s+|my\s+)?admin(?:istrator)?(?:\s*-?\s*(?:window|menu|panel|mode))?|(?:activate|enable|enter|turn\s+on|switch\s+on|go\s+(?:in)?to)\s+(?:the\s+)?admin(?:istrator)?\s*-?\s*mode|admin(?:istrator)?\s*-?\s*mode(?:\s+on)?|(?:öffne|oeffne|zeig(?:e)?|starte?)\s+(?:mir\s+)?(?:das\s+|den\s+|mein(?:en)?\s+)?admin(?:istrator)?\s*-?\s*(?:fenster|menü|menue|modus)?|(?:aktiviere|aktivier)\s+(?:den\s+)?admin(?:istrator)?\s*-?\s*modus|admin(?:istrator)?\s*-?\s*modus(?:\s+an)?)$").expect("admin opens")
});
static CLOSES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:close|hide|end|exit|leave|quit|deactivate|disable|stop|turn\s+off|switch\s+off)\s+(?:the\s+|my\s+)?admin(?:istrator)?(?:\s*-?\s*(?:window|menu|panel|mode))?|admin(?:istrator)?\s*-?\s*mode\s+off|(?:schlie(?:ß|ss)e?|beende?|verlasse?|deaktiviere?)\s+(?:das\s+|den\s+|mein(?:en)?\s+)?admin(?:istrator)?\s*-?\s*(?:fenster|menü|menue|modus)?|admin(?:istrator)?\s*-?\s*modus\s+aus)$").expect("admin closes")
});

/// "Open the admin window", "activate admin mode", "schließ das Admin-Fenster".
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['.', '!', '?']).trim();
    if OPENS.is_match(s) {
        return Some(json!({ "action": "show" }));
    }
    CLOSES.is_match(s).then(|| json!({ "action": "close" }))
}

/// Opens or closes the admin window (on another PC the check before it asked for his sign-in).
pub fn run(ctx: &crate::tools::ToolContext, args: &Value) -> Result<String, String> {
    if ctx.caller.guest().is_some() {
        return Err("Only Sir has an admin window.".into());
    }
    if ctx.caller == crate::accounts::Caller::Remote && args["action"].as_str() != Some("close") {
        return Err("The admin window of another PC opens at that PC, with your sign-in.".into());
    }
    if args["action"].as_str() == Some("close") {
        crate::gui::show_admin(false);
        return Ok(crate::kit::say("The admin window is closed.", "Closed, Sir."));
    }
    crate::gui::show_admin(true);
    Ok(if other_pc(&ctx.config) {
        crate::kit::say("Admin mode: everything on this PC is yours until you close the admin window.", "Admin mode, Sir. It ends when you close the window.")
    } else {
        crate::kit::say("The admin window is open: every window I have, yours first.", "Your admin window, Sir.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_window_is_named_once_and_sirs_own_come_first() {
        let mut seen = std::collections::HashSet::new();
        for w in &WINDOWS {
            assert!(seen.insert(w.id), "{} twice", w.id);
            assert!(w.group < GROUPS.len() && !w.title.is_empty() && w.about.len() <= 60, "{}", w.id);
            assert_eq!(w.sirs, w.group == 0, "{}: Sir's own windows are the first group", w.id);
        }
        assert!(WINDOWS.windows(2).all(|p| p[0].group <= p[1].group), "in the order of their groups");
        for id in ["admin", "accounts", "account_report", "settings", "feature_lab", "discord"] {
            assert!(window(id).is_some_and(|w| w.sirs), "{id} is Sir's alone");
        }
    }

    /// Each tile's order, read as the brain reads it, is a call of its own window: what the
    /// permission check and the pop-up go by.
    #[test]
    fn every_tiles_order_lands_on_its_window() {
        let missed: Vec<String> = WINDOWS
            .iter()
            .filter_map(|w| match w.open {
                Open::Order(order) => Some((w.id, order)),
                Open::Now(_) => None,
            })
            .filter_map(|(id, order)| match crate::phrasings::route(order) {
                None => Some(format!("{id}: '{order}' reaches the model")),
                Some((tool, args)) => (window_of(&tool, &args).map(|(found, opens)| (found.id, opens)) != Some((id, true))).then(|| format!("{id}: '{order}' → {tool} {args}")),
            })
            .collect();
        assert!(missed.is_empty(), "{missed:#?}");
    }

    #[test]
    fn his_orders_in_english_and_german() {
        for said in ["open the admin window", "activate admin mode", "admin mode", "show me the admin menu", "start admin mode", "öffne das admin fenster", "aktiviere den adminmodus", "admin modus an", "open admin-mode"] {
            assert_eq!(command(said), Some(json!({ "action": "show" })), "{said}");
        }
        for said in ["close the admin window", "deactivate admin mode", "exit admin mode", "admin mode off", "schließe das admin fenster", "beende den admin modus", "adminmodus aus"] {
            assert_eq!(command(said), Some(json!({ "action": "close" })), "{said}");
        }
        for said in ["open the administration of my school", "admin of the server", "who is the admin", "open adminer"] {
            assert_eq!(command(said), None, "{said}");
        }
        for said in ["open the admin window", "activate admin mode", "close the admin window"] {
            assert_eq!(crate::phrasings::route(said).map(|(tool, _)| tool).as_deref(), Some("admin"), "{said}");
        }
    }

    #[test]
    fn calls_are_read_as_the_window_they_open_or_close() {
        let of = |tool: &str, args: Value| window_of(tool, &args).map(|(w, opens)| (w.id, opens));
        assert_eq!(of("open_jai_view", json!({"view": "the calendar"})), Some(("schedule", true)));
        assert_eq!(of("open_jai_view", json!({"view": "earth", "action": "show_satellites"})), Some(("earth", true)));
        assert_eq!(of("configure_interface", json!({"settings_panel": true})), Some(("settings", true)));
        assert_eq!(of("configure_interface", json!({"feature_lab": false})), Some(("feature_lab", false)));
        assert_eq!(of("forge_tool", json!({"name": "x"})), Some(("feature_lab", true)));
        assert_eq!(of("discord_call", json!({"action": "join"})), Some(("discord", true)));
        assert_eq!(of("accounts", json!({"action": "report", "name": ""})), Some(("account_report", true)));
        assert_eq!(of("open_jai_view", json!({"view": "live"})), None);
        assert_eq!(of("get_weather", json!({})), None);
    }

    /// Another PC of Sir's: a new account talks and nothing more; his switches open a window of
    /// the list there, never one of his own; a window asks for his sign-in, which holds until it
    /// closes; the admin window makes everything his, and its end takes everything back.
    #[test]
    fn another_pc_follows_its_account_and_his_sign_in() {
        let _tests = lock(&TESTS);
        let dir = std::env::temp_dir().join(format!("jai_admin_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = ConfigStore::new(crate::config::JaiConfig { main_pc: "https://main.example".into(), memory_db_path: dir.join("jai_memory.db").to_string_lossy().to_string(), ..Default::default() });
        *lock(&ACCOUNT) = Some(Some(Account { name: "Max".into(), ..Default::default() }));
        *lock(&STATE) = State { key: None, grants: Vec::new(), asking: None, up: Vec::new() };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let earth = json!({"view": "earth"});
        // New: nothing but talking; a window asks for his sign-in (and is not opened).
        assert_eq!(refusal(&config, "get_weather", &json!({})).as_deref(), Some(NOT_YOURS));
        assert_eq!(refusal(&config, "open_jai_view", &earth).as_deref(), Some(SIGN_IN));
        assert_eq!(asking().map(|w| w.id), Some("earth"));
        assert!(!may_power(&config));
        // Closing is anyone's.
        assert_eq!(refusal(&config, "close_native_window", &json!({"id": "earth"})), None);
        assert_eq!(refusal(&config, "configure_interface", &json!({"feature_lab": false})), None);
        // His switches: the Earth (not its satellites when he denied those), a tool, the power.
        cancel();
        let mut max = Account { name: "Max".into(), ..Default::default() };
        max.own.set(&key("earth"), true);
        max.own.set("open_jai_view:action=show_satellites", false);
        max.own.set("get_weather", true);
        max.own.set(&key("settings"), true);
        max.own.set(accounts::POWER, true);
        // (What he may on Sir's PC is no switch here.)
        max.here.set("control_music", true);
        assert_eq!(refusal(&config, "control_music", &json!({})).as_deref(), Some(NOT_YOURS));
        *lock(&ACCOUNT) = Some(Some(max));
        assert_eq!(refusal(&config, "open_jai_view", &earth), None);
        assert_eq!(refusal(&config, "open_jai_view", &json!({"view": "earth", "action": "show_satellites"})).as_deref(), Some(SIGN_IN));
        assert_eq!(refusal(&config, "get_weather", &json!({})), None);
        assert!(may_power(&config));
        // Sir's own windows never by a switch: the settings ask for his sign-in all the same.
        assert_eq!(refusal(&config, "configure_interface", &json!({"settings_panel": true})).as_deref(), Some(SIGN_IN));
        assert_eq!(refusal(&config, "accounts", &json!({"action": "pause", "name": "Erika"})).as_deref(), Some(SIGN_IN));
        assert_eq!(asking().map(|w| w.id), Some("admin"), "his accounts are admin mode's");
        // The frame: the settings, opened by a click, close at once and ask; the Earth stays.
        cancel();
        assert_eq!(gate(&config, rt.handle(), &["settings", "earth"]), vec!["settings"]);
        assert_eq!(asking().map(|w| w.id), Some("settings"));
        // Signed in for the settings: they stay up while open, and only they.
        {
            let mut state = lock(&STATE);
            state.asking = None;
            state.key = Some(("https://main.example".into(), "k".repeat(64)));
            state.grants.push(Grant { id: "settings", seen: false, at: Instant::now() });
        }
        assert!(gate(&config, rt.handle(), &["settings"]).is_empty());
        assert_eq!(refusal(&config, "configure_interface", &json!({"settings_panel": true})), None);
        assert_eq!(refusal(&config, "configure_interface", &json!({"feature_lab": true})).as_deref(), Some(SIGN_IN), "one window's sign-in is that window's");
        cancel();
        // Closed: the sign-in ends with it, and opening it again asks again.
        assert!(gate(&config, rt.handle(), &[]).is_empty());
        assert!(key_held().is_none() && !granted("settings"));
        assert_eq!(gate(&config, rt.handle(), &["settings"]), vec!["settings"]);
        cancel();
        // Everything on his own PC: every feature, and Sir's settings, labs and Discord there,
        // but one taken back stays shut; never Sir's admin window, accounts or report, and no
        // orders to other PCs.
        let saved = account(&config);
        let mut all = Account { name: "Max".into(), ..Default::default() };
        all.own.allow = vec!["*".into()];
        all.own.set(&key("discord"), false);
        *lock(&ACCOUNT) = Some(Some(all));
        for (tool, args) in [("control_music", json!({"action": "pause"})), ("configure_interface", json!({"settings_panel": true})), ("forge_tool", json!({"name": "x"})), ("open_jai_view", json!({"view": "network_radar"}))] {
            assert_eq!(refusal(&config, tool, &args), None, "{tool} {args}");
        }
        assert!(may_power(&config) && gate(&config, rt.handle(), &["settings", "feature_lab", "earth"]).is_empty());
        for (tool, args) in [("discord_call", json!({"action": "join"})), ("admin", json!({"action": "show"})), ("accounts", json!({"action": "show"})), ("accounts", json!({"action": "report", "name": ""})), ("other_pc", json!({"order": "x"}))] {
            assert_eq!(refusal(&config, tool, &args).as_deref(), Some(SIGN_IN), "{tool} {args}");
            cancel();
        }
        assert_eq!(gate(&config, rt.handle(), &["admin", "accounts", "discord"]), vec!["admin", "accounts", "discord"]);
        cancel();
        *lock(&ACCOUNT) = Some(saved);
        // Admin mode: everything while the admin window is up; once it closes, what it opened
        // closes without asking.
        {
            let mut state = lock(&STATE);
            state.key = Some(("https://main.example".into(), "k".repeat(64)));
            state.grants.push(Grant { id: "admin", seen: false, at: Instant::now() });
        }
        assert!(gate(&config, rt.handle(), &["admin", "feature_lab", "settings"]).is_empty());
        assert!(admin_mode() && refusal(&config, "other_pc", &json!({"order": "x"})).is_none());
        assert!(gate(&config, rt.handle(), &["feature_lab", "settings"]) == vec!["feature_lab", "settings"]);
        assert!(asking().is_none() && !admin_mode() && key_held().is_none(), "lost, not tried: no pop-up");
        // His main PC forgot the sign-in: it ends here too.
        {
            let mut state = lock(&STATE);
            state.key = Some(("https://main.example".into(), "k".repeat(64)));
            state.grants.push(Grant { id: "admin", seen: true, at: Instant::now() });
        }
        heard(None, false);
        heard(Some(&"o".repeat(64)), false);
        assert!(admin_mode(), "an answer about no key, or another, leaves it alone");
        heard(Some(&"k".repeat(64)), false);
        assert!(!admin_mode());
        // Paused, or a closed topic: no answer at all.
        let mut max = account(&config).unwrap();
        max.until = accounts::now() + 600;
        *lock(&ACCOUNT) = Some(Some(max));
        assert!(turned_away(&config, "tell me a joke").is_some_and(|w| w.contains("paused")));
        // … but his order from his main PC opens a window all the same (never the admin window),
        // and the tile's order of a window he signed in for is carried out.
        sirs_call("open_jai_view", &json!({"view": "disk_map"}));
        sirs_call("admin", &json!({"action": "show"}));
        assert!(granted("disk_map") && !admin_mode());
        assert!(turned_away(&config, "open the disk map").is_none() && turned_away(&config, "open the network radar").is_some());
        // On his main PC: everything.
        let main = ConfigStore::new(crate::config::JaiConfig { memory_db_path: config.read(|c| c.memory_db_path.clone()), ..Default::default() });
        assert_eq!(refusal(&main, "configure_interface", &json!({"settings_panel": true})), None);
        assert!(may_power(&main) && turned_away(&main, "tell me a joke").is_none() && gate(&main, rt.handle(), &["settings"]).is_empty());
        *lock(&ACCOUNT) = None;
        *lock(&STATE) = State { key: None, grants: Vec::new(), asking: None, up: Vec::new() };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// His main PC: the sign-in needs his admin account and its password, and its key holds
    /// until given back; his own account is never changed from another PC.
    #[test]
    fn his_main_pc_signs_him_in_and_keeps_his_account_his() {
        let dir = std::env::temp_dir().join(format!("jai_admin_main_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("jai_memory.db").to_string_lossy().to_string();
        let vault = crate::vault::Vault::in_memory();
        accounts::make_admin(&db, &vault, "Mustermann", "geheim123").unwrap();
        assert!(accounts::make_admin(&db, &vault, "Other", "geheim123").is_err(), "made once");
        let max = accounts::for_voice(&db, "Max", "").unwrap();
        vault.set_secret("jai-account", "max", "maxpass").unwrap();
        assert!(grant_key(&db, &vault, "Mustermann", "wrong", "10.0.0.9").is_err());
        assert!(grant_key(&db, &vault, "Max", "maxpass", "10.0.0.9").is_err(), "an account that is not his");
        let key = grant_key(&db, &vault, "mustermann", "geheim123", "10.0.0.9").unwrap();
        assert!(holds(&key) && !holds(&"0".repeat(64)));
        // The list, a change, his own account left alone.
        let list = answer(&db, &key, &json!({"action": "accounts", "name": "Max"})).unwrap();
        assert_eq!(list["accounts"].as_array().map(Vec::len), Some(2));
        let mut changed = max.clone();
        changed.own.set("get_weather", true);
        changed.here.set("control_music", true);
        answer(&db, &key, &json!({"action": "save", "name": "Max", "account": changed})).unwrap();
        let saved = accounts::get(&db, "Max").unwrap();
        assert!(saved.own.allows("get_weather") && saved.here.allows("control_music") && !saved.here.allows("get_weather"));
        // On this, his PC, Max may what Sir allowed him here, and never Sir's own windows.
        assert_eq!(guest_refusal(&db, "Max", "control_music", &json!({"action": "pause"})), None);
        assert_eq!(guest_refusal(&db, "Max", "get_weather", &json!({})).as_deref(), Some(NOT_YOURS), "allowed on his own PC, not on Sir's");
        accounts::update(&db, "Max", |a| a.here.allow = vec!["*".into()]).unwrap();
        assert_eq!(guest_refusal(&db, "Max", "get_weather", &json!({})), None);
        assert_eq!(guest_refusal(&db, "Max", "configure_interface", &json!({"settings_panel": true})).as_deref(), Some(NOT_YOURS));
        assert_eq!(guest_refusal(&db, "Nobody", "get_weather", &json!({})).as_deref(), Some(NOT_YOURS));
        assert!(answer(&db, &key, &json!({"action": "remove", "name": "Mustermann"})).is_err());
        assert!(answer(&db, &key, &json!({"action": "password", "name": "Mustermann"})).is_err());
        // Given back: gone.
        answer(&db, &key, &json!({"action": "logout"})).unwrap();
        assert!(!holds(&key));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
