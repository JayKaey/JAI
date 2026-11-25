//! Who may use J.A.I. besides Sir, and for what (Sir, 2026-11-20: "not just everyone should be
//! able to use JAI"; "I want to be able to control every aspect of their permissions").
//!
//! An account is a name and a password (3-20 characters each) that Sir let in with a key shown
//! on his PC. It may do what Sir allowed it and nothing more: a new one only asks the language
//! model. Sir allows or denies a tool, or one value of one of its options (`open_jai_view` with
//! `view=earth`, but not `action=show_satellites`), closes topics to it (where it is, maths …)
//! and may pause it. What it may is set apart for two places (Sir, 2026-11-20: "use all of JAIs
//! features on their PCs but they arent allowed to controll my PC or others than theirs"): its
//! own PC (`own`, everything at one switch) and Sir's PC (`here`: his phone page, its voice at
//! his PC), all off at first. All of it is kept on the main PC: the accounts in
//! `jai_memory.db`, their passwords sealed in the vault so Sir can read them in his list.

use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::vault::Vault;

/// Who a turn is for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Caller {
    /// Sir: his voice, his keyboard, his phone. No limit at all on his main PC; on another PC
    /// of his, whoever sits there, as far as its account and his sign-in go (`admin`).
    #[default]
    Sir,
    /// Sir's order from his main PC, carried out on another PC of his: his, whatever that PC's
    /// account may.
    Remote,
    /// Anyone else, by account name.
    Guest(String),
}

impl Caller {
    pub fn is_sir(&self) -> bool {
        matches!(self, Caller::Sir | Caller::Remote)
    }

    pub fn guest(&self) -> Option<&str> {
        match self {
            Caller::Guest(name) => Some(name),
            Caller::Sir | Caller::Remote => None,
        }
    }
}

impl std::fmt::Display for Caller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.guest().unwrap_or("Sir"))
    }
}

/// A phone or a PC an account used.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Device {
    /// Its own id (a PC's install id; a phone's key).
    pub id: String,
    /// "phone" or "pc".
    pub kind: String,
    /// What it is ("Android phone", the PC's name and system).
    pub label: String,
    /// The J.A.I. it runs (a PC).
    pub version: String,
    /// Its system and load, one line (a PC).
    pub stats: String,
    /// Last heard from (unix seconds).
    pub seen: i64,
}

/// What an account may do in one place.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Switches {
    /// A tool (`open_jai_view`), one value of one of its options (`open_jai_view:view=earth`),
    /// or `*` (everything).
    pub allow: Vec<String>,
    /// What it may not even where its tool is allowed (`open_jai_view:action=show_satellites`).
    pub deny: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Account {
    pub name: String,
    /// What it may do on its own PC, the one logged in with it (kept where the switches of
    /// every account were before they were set apart: those stay its PC's).
    #[serde(flatten)]
    pub own: Switches,
    /// What it may do on Sir's PC: through his phone page, or by its voice at his PC.
    pub here: Switches,
    /// What it gets no answer on: a topic of `TOPICS` by its id, or Sir's own words.
    pub topics: Vec<String>,
    /// Paused until (unix seconds); 0: not paused.
    pub until: i64,
    pub created: i64,
    pub devices: Vec<Device>,
    /// Sir's own admin account, made once on his main PC: it signs in at another PC's admin
    /// window and nowhere else (`admin`).
    pub admin: bool,
}

/// The topics Sir can close to an account (id, label, the words that ask about it).
pub const TOPICS: [(&str, &str); 6] = [("location", "Where they are"), ("maths", "Maths and sums"), ("weather", "Weather"), ("time", "Time and date"), ("news", "News"), ("sir", "Sir himself")];

fn re(pattern: &str) -> Regex {
    Regex::new(&format!("(?i){pattern}")).expect("topic pattern")
}

static TOPIC_WORDS: LazyLock<[Regex; 6]> = LazyLock::new(|| {
    [
        re(r"\bwhere\s+(?:am\s+i|are\s+we|is\s+(?:this|here|my))\b|\bmy\s+(?:location|address|position|coordinates)\b|\bwo\s+(?:bin\s+ich|sind\s+wir)\b|\bstandort\b|\bgps\b|\bnear\s+me\b|\bnearby\b|\bin\s+der\s+n[aä]he\b|\bhow\s+far\b"),
        re(r"\d\s*(?:[+*/×÷^%]|plus|minus|times|mal|durch|geteilt)\s*\d|\d\s+[-x]\s+\d|\b(?:calculate|compute|solve|equation|square\s+root|squared|percent\s+of|rechne|berechne|wurzel|integral|derivative|ableitung)\b"),
        re(r"\b(?:weather|forecast|rain(?:ing)?|snow(?:ing)?|temperature|wetter|regen|schnee|temperatur)\b"),
        re(r"\bwhat\s+time\b|\bwhat\s+day\b|\bthe\s+date\b|\btoday'?s\s+date\b|\bwhich\s+day\b|\buhrzeit\b|\bwie\s+sp[aä]t\b|\bwelcher\s+tag\b|\bdatum\b"),
        re(r"\b(?:news|headlines?|nachrichten|schlagzeilen)\b"),
        re(r"\bsir\b|\byour\s+(?:owner|master|boss|creator|user)\b|\bwho\s+(?:owns|made|built)\s+you\b"),
    ]
});

/// The keys a call answers to: its tool, then each of its options' values (`view=earth`).
fn keys(tool: &str, args: &Value) -> Vec<String> {
    let mut keys = vec![tool.to_string()];
    for (option, value) in args.as_object().into_iter().flatten() {
        if let Some(value) = value.as_str().map(|v| v.trim().to_lowercase()).filter(|v| !v.is_empty()) {
            keys.push(format!("{tool}:{option}={value}"));
        }
    }
    keys
}

impl Switches {
    /// Whether everything is allowed (but what is switched off one by one).
    pub fn everything(&self) -> bool {
        self.allow.iter().any(|a| a == "*")
    }

    /// Why this call may not be made; None: it may.
    pub fn refusal(&self, tool: &str, args: &Value) -> Option<String> {
        let keys = keys(tool, args);
        // ("Everything" is everything but root on J.A.I.OS: that is a switch of its own.)
        let allowed = self.allow.iter().any(|a| (a == "*" && tool != crate::os_orders::TOOL) || keys.contains(a));
        (self.denies(tool, args) || !allowed).then(|| "Sir has not given you that permission.".to_string())
    }

    /// Whether Sir switched off something this call names (a value of one of its options).
    pub fn denies(&self, tool: &str, args: &Value) -> bool {
        keys(tool, args).iter().any(|k| self.deny.contains(k))
    }

    /// Whether this one call would be allowed: the tool, or one value of an option.
    pub fn allows(&self, key: &str) -> bool {
        let tool = key.split(':').next().unwrap_or(key);
        !self.deny.iter().any(|d| d == key) && self.allow.iter().any(|a| (a == "*" && tool != crate::os_orders::TOOL) || a == key || a == tool)
    }

    /// Allows (or takes back) one key: a tool, or one value of an option.
    pub fn set(&mut self, key: &str, on: bool) {
        let tool = key.split(':').next().unwrap_or(key).to_string();
        self.allow.retain(|a| a != key);
        self.deny.retain(|d| d != key);
        if key == tool {
            // A whole tool: its values follow it from now on.
            self.allow.retain(|a| !a.starts_with(&format!("{tool}:")));
            self.deny.retain(|d| !d.starts_with(&format!("{tool}:")));
            if on {
                self.allow.push(tool);
            } else if self.allow.iter().any(|a| a == "*") {
                // (Everything but this.)
                self.deny.push(tool);
            }
        } else if on {
            if !self.allows(&tool) {
                self.allow.push(key.to_string());
            }
        } else {
            // (Denied outright: a call that names an allowed value too stays refused — the Earth
            // allowed, its satellites not.)
            self.deny.push(key.to_string());
        }
    }
}

impl Account {
    fn new(name: &str) -> Self {
        Self { name: name.to_string(), created: now(), ..Default::default() }
    }

    /// The topic of `text` it gets no answer on (its label), if any.
    pub fn closed_topic(&self, text: &str) -> Option<String> {
        self.topics.iter().find_map(|topic| {
            let asked = match TOPICS.iter().position(|(id, _)| id == topic) {
                Some(i) => TOPIC_WORDS[i].is_match(text),
                None => re(&format!(r"\b{}\b", regex::escape(topic.trim()))).is_match(text),
            };
            asked.then(|| TOPICS.iter().find(|(id, _)| id == topic).map_or(topic.clone(), |(_, label)| label.to_string()))
        })
    }

    /// Paused right now: until when (local "HH:MM", or a date when it is not today).
    pub fn paused(&self) -> Option<String> {
        (self.until > now()).then(|| {
            let at = chrono::DateTime::from_timestamp(self.until, 0).map(|t| t.with_timezone(&chrono::Local));
            match at {
                Some(t) if t.date_naive() == chrono::Local::now().date_naive() => t.format("%H:%M").to_string(),
                Some(t) => t.format("%d.%m. %H:%M").to_string(),
                None => "later".into(),
            }
        })
    }

    /// What it is told instead of an answer, if anything: paused, or a topic it may not ask about.
    pub fn turned_away(&self, text: &str) -> Option<String> {
        if let Some(until) = self.paused() {
            return Some(format!("Sir has paused your access until {until}."));
        }
        self.closed_topic(text).map(|topic| format!("Sir has not allowed me to talk with you about that ({}).", topic.to_lowercase()))
    }
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// -------------------------------------------------------------------------- the store ----

/// The accounts, read from the database once per path, then from here.
static ACCOUNTS: Mutex<Option<(String, Vec<Account>)>> = Mutex::new(None);
/// Where the passwords are sealed in the vault.
const PASSWORDS: &str = "jai-account";

fn open(db: &str) -> rusqlite::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db)?;
    conn.busy_timeout(Duration::from_secs(3))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS accounts (name TEXT PRIMARY KEY COLLATE NOCASE, data TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS account_keys (key TEXT PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE, made INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS account_log (at INTEGER NOT NULL, name TEXT NOT NULL COLLATE NOCASE, via TEXT NOT NULL, asked TEXT NOT NULL, answered TEXT NOT NULL, tools TEXT NOT NULL);",
    )?;
    Ok(conn)
}

/// Every account, oldest first.
pub fn all(db: &str) -> Vec<Account> {
    let mut cache = lock(&ACCOUNTS);
    if cache.as_ref().is_none_or(|(path, _)| path != db) {
        let read = open(db)
            .and_then(|conn| {
                let mut rows = conn.prepare("SELECT data FROM accounts")?;
                let read = rows.query_map([], |r| r.get::<_, String>(0))?.flatten().filter_map(|j| serde_json::from_str::<Account>(&j).ok()).collect::<Vec<_>>();
                Ok(read)
            })
            .unwrap_or_default();
        let mut read = read;
        read.sort_by_key(|a| a.created);
        lock(&NAMES).extend(read.iter().map(|a| a.name.to_lowercase()));
        *cache = Some((db.to_string(), read));
    }
    cache.as_ref().map(|(_, a)| a.clone()).unwrap_or_default()
}

pub fn get(db: &str, name: &str) -> Option<Account> {
    all(db).into_iter().find(|a| a.name.eq_ignore_ascii_case(name))
}

/// Keeps an account (one of that name is replaced).
pub fn save(db: &str, account: &Account) -> Result<(), String> {
    let data = serde_json::to_string(account).map_err(|e| e.to_string())?;
    open(db)
        .and_then(|conn| conn.execute("INSERT INTO accounts (name, data) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET data = ?2", rusqlite::params![account.name, data]))
        .map_err(|e| e.to_string())?;
    lock(&NAMES).insert(account.name.to_lowercase());
    let mut all = all(db);
    match all.iter_mut().find(|a| a.name.eq_ignore_ascii_case(&account.name)) {
        Some(kept) => *kept = account.clone(),
        None => all.push(account.clone()),
    }
    *lock(&ACCOUNTS) = Some((db.to_string(), all));
    Ok(())
}

/// Changes one account (nothing if there is none of that name).
pub fn update(db: &str, name: &str, change: impl FnOnce(&mut Account)) -> Result<Option<Account>, String> {
    let Some(mut account) = get(db, name) else { return Ok(None) };
    change(&mut account);
    save(db, &account)?;
    Ok(Some(account))
}

/// Removes an account for good: its keys (phones and PCs are logged out), its password, its
/// voice. Whether there was one.
pub fn remove(db: &str, vault: &Vault, name: &str) -> bool {
    let gone = open(db)
        .map(|conn| {
            let _ = conn.execute("DELETE FROM account_keys WHERE name = ?1", [name]);
            conn.execute("DELETE FROM accounts WHERE name = ?1", [name]).unwrap_or(0) > 0
        })
        .unwrap_or(false);
    vault.clear_secret(PASSWORDS, &name.to_lowercase());
    let mut all = all(db);
    all.retain(|a| !a.name.eq_ignore_ascii_case(name));
    *lock(&ACCOUNTS) = Some((db.to_string(), all));
    gone
}

/// Its password, for Sir's list.
pub fn password(vault: &Vault, name: &str) -> Option<String> {
    vault.secret(PASSWORDS, &name.to_lowercase())
}

/// The account a guest's voice belongs to: kept for a voice Sir introduced (what it may at his
/// PC: the music, or everything) or one that told its name; made when there is none yet.
pub fn for_voice(db: &str, name: &str, may: &str) -> Option<Account> {
    // (Never his admin account: a voice of that name is somebody else's.)
    if let Some(account) = get(db, name) {
        return (!account.admin).then_some(account);
    }
    let mut account = Account::new(name);
    account.here.allow = voice_allow(may);
    save(db, &account).ok().map(|_| account)
}

/// What Sir's word for a voice ("Tom may do everything", "… only the music") allows.
pub fn voice_allow(may: &str) -> Vec<String> {
    match may {
        "everything" => vec!["*".into()],
        "music" => ["control_music", "control_audio_mixer", "media_and_volume_control"].map(String::from).to_vec(),
        _ => Vec::new(),
    }
}

// ------------------------------------------------------------------- names and logins ----

/// A name or a password as Sir asked for them: 3 to 20 characters (a name: letters, digits,
/// `_ - .`; a password: anything but control characters).
pub fn check(text: &str, name: bool) -> Result<(), String> {
    shape(text, name)?;
    // (Nobody signs up as the one they would be taken for.)
    if name && ["sir", "jai", "jay", "j.a.i.", "owner", "admin", "root", "system"].contains(&text.to_lowercase().as_str()) {
        return Err("That name is taken.".into());
    }
    Ok(())
}

/// A name or a password of the right length and characters (Sir's own admin account may take
/// any name: "admin" too).
fn shape(text: &str, name: bool) -> Result<(), String> {
    let what = if name { "A name" } else { "A password" };
    let count = text.chars().count();
    if !(3..=20).contains(&count) {
        return Err(format!("{what} has 3 to 20 characters."));
    }
    let fits = |c: char| if name { c.is_alphanumeric() || "_-.".contains(c) } else { !c.is_control() };
    if !text.chars().all(fits) {
        return Err(if name { "A name has letters, digits, _ - and . only.".into() } else { "A password has no control characters.".into() });
    }
    Ok(())
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn fresh_key() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn issue_key(db: &str, name: &str) -> Result<String, String> {
    let key = fresh_key();
    open(db).and_then(|conn| conn.execute("INSERT INTO account_keys (key, name, made) VALUES (?1, ?2, ?3)", rusqlite::params![key, name, now()])).map_err(|e| e.to_string())?;
    Ok(key)
}

/// The account a key (a phone's or a PC's) is logged in as.
pub fn session(db: &str, key: &str) -> Option<String> {
    if key.len() != 64 {
        return None;
    }
    let name: String = open(db).ok()?.query_row("SELECT name FROM account_keys WHERE key = ?1", [key], |r| r.get(0)).ok()?;
    get(db, &name).map(|a| a.name)
}

pub fn logout(db: &str, key: &str) {
    if let Ok(conn) = open(db) {
        let _ = conn.execute("DELETE FROM account_keys WHERE key = ?1", [key]);
    }
}

/// A login: the key its phone or PC sends from now on. `from`: where it came from (its address).
/// Sir's admin account logs in nowhere: it signs in at an admin window (`admin`), for as long
/// as that window is open.
pub fn login(db: &str, vault: &Vault, name: &str, given: &str, from: &str) -> Result<String, String> {
    let account = verify(db, vault, name, given, from)?;
    if account.admin {
        return Err("Sir's admin account opens his admin windows only; log in with an account of this PC's user.".into());
    }
    issue_key(db, &account.name)
}

/// The account whose name and password these are, within the limits on tries. `from`: where
/// they came from (its address).
pub fn verify(db: &str, vault: &Vault, name: &str, given: &str, from: &str) -> Result<Account, String> {
    let name = name.trim();
    if !knock("login", from, 20, Duration::from_secs(600)) || !knock("login", "", 120, Duration::from_secs(60)) {
        return Err("Too many tries. Wait a few minutes.".into());
    }
    if strikes(&format!("name:{}", name.to_lowercase())) >= 5 || strikes(&format!("from:{from}")) >= 10 {
        return Err("Too many wrong passwords. Wait ten minutes.".into());
    }
    let account = get(db, name);
    let right = account.as_ref().and_then(|a| password(vault, &a.name)).is_some_and(|p| same(&p, given));
    account.filter(|_| right).ok_or_else(|| {
        strike(&format!("name:{}", name.to_lowercase()));
        strike(&format!("from:{from}"));
        "That name and password do not match.".to_string()
    })
}

/// Sir's admin account, once he made it.
pub fn admin(db: &str) -> Option<Account> {
    all(db).into_iter().find(|a| a.admin)
}

/// Sir makes his admin account on his main PC: once, with a name no other account has.
pub fn make_admin(db: &str, vault: &Vault, name: &str, password: &str) -> Result<(), String> {
    let name = name.trim();
    shape(name, true)?;
    shape(password, false)?;
    if admin(db).is_some() {
        return Err("Your admin account is made already.".into());
    }
    if get(db, name).is_some() {
        return Err("An account of that name is someone else's.".into());
    }
    vault.set_secret(PASSWORDS, &name.to_lowercase(), password)?;
    save(db, &Account { admin: true, ..Account::new(name) })
}

// ------------------------------------------------------------------------- signing up ----

/// An account asked for and not yet let in: its key is on Sir's screen.
struct Pending {
    name: String,
    password: String,
    code: String,
    at: Instant,
    tries: u8,
}

static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
/// How long a key on Sir's screen holds, and how many requests wait at once.
const KEY_LASTS: Duration = Duration::from_secs(300);
const PENDING_AT_MOST: usize = 3;

/// Someone asks for an account: its key, to be shown on Sir's PC (and nowhere else). Limited so
/// that nobody can flood his screen: one request a minute from one address, one key window
/// every 20 seconds and three waiting at most.
pub fn request(db: &str, name: &str, password: &str, from: &str) -> Result<String, String> {
    let name = name.trim();
    check(name, true)?;
    check(password, false)?;
    let mut pending = lock(&PENDING);
    pending.retain(|p| p.at.elapsed() < KEY_LASTS && p.tries < 3);
    if get(db, name).is_some() || pending.iter().any(|p| p.name.eq_ignore_ascii_case(name)) {
        return Err("That name is taken.".into());
    }
    if pending.len() >= PENDING_AT_MOST || !knock("signup", from, 1, Duration::from_secs(60)) || !knock("signup", "", 1, Duration::from_secs(20)) {
        return Err("Too many requests just now. Try again in a minute.".into());
    }
    let n = u32::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().unwrap_or([0; 4]));
    let code = format!("{:06}", n % 1_000_000);
    pending.push(Pending { name: name.to_string(), password: password.to_string(), code: code.clone(), at: Instant::now(), tries: 0 });
    Ok(code)
}

/// The key from Sir's screen: the account is made (it asks the language model only) and logged
/// in. Three wrong keys and the request is void.
pub fn finish(db: &str, vault: &Vault, name: &str, code: &str, from: &str) -> Result<String, String> {
    if !knock("finish", from, 10, Duration::from_secs(600)) {
        return Err("Too many tries. Wait a few minutes.".into());
    }
    let mut pending = lock(&PENDING);
    pending.retain(|p| p.at.elapsed() < KEY_LASTS && p.tries < 3);
    let Some(i) = pending.iter().position(|p| p.name.eq_ignore_ascii_case(name.trim())) else {
        return Err("No request of that name is waiting (a key holds five minutes).".into());
    };
    if !same(&pending[i].code, code.trim()) {
        pending[i].tries += 1;
        return Err(format!("Wrong key. {} tries left.", 3 - pending[i].tries));
    }
    let asked = pending.remove(i);
    drop(pending);
    let account = Account::new(&asked.name);
    vault.set_secret(PASSWORDS, &asked.name.to_lowercase(), &asked.password)?;
    save(db, &account)?;
    issue_key(db, &account.name)
}

/// Turns a waiting request away (Sir closed its key).
pub fn refuse(name: &str) {
    lock(&PENDING).retain(|p| !p.name.eq_ignore_ascii_case(name));
}

/// The requests waiting for their key: name, key, seconds it still holds.
pub fn waiting() -> Vec<(String, String, u64)> {
    let mut pending = lock(&PENDING);
    pending.retain(|p| p.at.elapsed() < KEY_LASTS && p.tries < 3);
    pending.iter().map(|p| (p.name.clone(), p.code.clone(), KEY_LASTS.saturating_sub(p.at.elapsed()).as_secs())).collect()
}

/// A phone or PC of an account was heard from: kept in its list, the newest eight (written at
/// most once a minute per device).
pub fn seen(db: &str, name: &str, device: Device) {
    let Some(account) = get(db, name) else { return };
    if account.devices.iter().any(|d| d.id == device.id && device.seen - d.seen < 60 && d.version == device.version && d.stats == device.stats) {
        return;
    }
    let _ = update(db, name, |a| {
        a.devices.retain(|d| d.id != device.id);
        a.devices.push(device);
        a.devices.sort_by_key(|d| std::cmp::Reverse(d.seen));
        a.devices.truncate(8);
    });
}

// ------------------------------------------------------------------ what he did for whom ----

/// One thing J.A.I. did for someone, kept for Sir ("what did you do for Max today?").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Done {
    pub at: i64,
    pub name: String,
    /// "phone", "voice", "pc".
    pub via: String,
    pub asked: String,
    pub answered: String,
    /// The tools it ran (refused ones too).
    pub tools: String,
}

/// Keeps what was asked and answered (ninety days).
pub fn note(db: &str, name: &str, via: &str, asked: &str, answered: &str, tools: &[String]) {
    if crate::platform::in_test_session() {
        return;
    }
    if let Ok(conn) = open(db) {
        let _ = conn.execute("INSERT INTO account_log (at, name, via, asked, answered, tools) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", rusqlite::params![now(), name, via, asked, answered, tools.join(", ")]);
        let _ = conn.execute("DELETE FROM account_log WHERE at < ?1", [now() - 90 * 86_400]);
    }
}

/// What was done for `name` (every account: `None`) since `since`, oldest first (the newest 500).
pub fn log(db: &str, name: Option<&str>, since: i64) -> Vec<Done> {
    let read = |conn: rusqlite::Connection| -> rusqlite::Result<Vec<Done>> {
        let mut rows = conn.prepare("SELECT at, name, via, asked, answered, tools FROM account_log WHERE at >= ?1 AND (?2 = '' OR name = ?2) ORDER BY at DESC LIMIT 500")?;
        let done = rows.query_map(rusqlite::params![since, name.unwrap_or("")], |r| Ok(Done { at: r.get(0)?, name: r.get(1)?, via: r.get(2)?, asked: r.get(3)?, answered: r.get(4)?, tools: r.get(5)? }))?.flatten().collect();
        Ok(done)
    };
    let mut done = open(db).and_then(read).unwrap_or_default();
    done.reverse();
    done
}

// --------------------------------------------------------------------------- the gates ----

/// Knocks: (kind, who) → when each knock came.
static KNOCKS: LazyLock<Mutex<HashMap<String, VecDeque<Instant>>>> = LazyLock::new(Default::default);

/// One more knock of `kind` from `who` (empty: anyone): whether it is let through, at most `n`
/// in `window`.
fn knock(kind: &str, who: &str, n: usize, window: Duration) -> bool {
    let mut knocks = lock(&KNOCKS);
    if knocks.len() > 4096 {
        knocks.retain(|_, at| at.back().is_some_and(|t| t.elapsed() < Duration::from_secs(3600)));
    }
    let at = knocks.entry(format!("{kind}|{who}")).or_default();
    while at.front().is_some_and(|t| t.elapsed() >= window) {
        at.pop_front();
    }
    if at.len() >= n {
        return false;
    }
    at.push_back(Instant::now());
    true
}

/// Wrong passwords for `what` in the last ten minutes.
fn strikes(what: &str) -> usize {
    let mut knocks = lock(&KNOCKS);
    let at = knocks.entry(format!("strike|{what}")).or_default();
    while at.front().is_some_and(|t| t.elapsed() >= Duration::from_secs(600)) {
        at.pop_front();
    }
    at.len()
}

fn strike(what: &str) {
    lock(&KNOCKS).entry(format!("strike|{what}")).or_default().push_back(Instant::now());
}

// ------------------------------------------------------------------------ Sir's orders ----

static LIST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(open|show|display|bring\s+up|zeig(?:e)?|öffne|oeffne|close|hide|schlie(?:ß|ss)e?)\s+(?:me\s+|mir\s+)?(?:the\s+|my\s+|die\s+|meine\s+)?(?:accounts?(?:\s+list)?|list\s+of\s+accounts|konten(?:liste)?|benutzer(?:liste)?|users?(?:\s+list)?)|who\s+(?:uses|is\s+using|may\s+use)\s+(?:you|jai)|wer\s+(?:benutzt|nutzt)\s+(?:dich|jai))$"));
static REPORT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:what\s+(?:did|have)\s+you\s+(?:do(?:ne)?|been\s+doing)\s+for\s+|what\s+(?:did|has)\s+(?:you\s+do\s+for\s+)?|report\s+on\s+|was\s+hast\s+du\s+(?:für|fuer)\s+)(?:the\s+)?([\w.-]+?)(?:'s)?(?:\s+(?:asked(?:\s+you)?|wanted|done|gemacht|getan))?(?:\s+(today|yesterday|this\s+week|so\s+far|ever|heute|gestern|diese\s+woche))?$")
});
static PAUSE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:pause|suspend|time\s*out|lock|sperre?)\s+([\w.-]+?)(?:'s)?(?:\s+account)?\s+(?:for|für|fuer)\s+(\d+|an?|one|two|three|four|five|six|seven|eight|nine|ten|twelve|eine?n?|zwei|drei|vier|fünf|fuenf|sechs)\s+(minutes?|mins?|hours?|stunden?|days?|tage?n?|weeks?|wochen?)$"));
static RESUME: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:unpause|resume|unlock|unblock|entsperre?)\s+([\w.-]+?)(?:'s)?(?:\s+account)?$|^end\s+([\w.-]+?)'s\s+time\s*out$"));
static REMOVE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:remove|delete|lösche|loesche)\s+([\w.-]+?)(?:'s)?\s+(?:account|konto)$"));
/// (Compiled once: `command` reads every sentence, and a regex built per call made routing a
/// sentence twice as slow.)
static SAYS_ACCOUNT: LazyLock<Regex> = LazyLock::new(|| re(r"\baccount\b|\bkonto\b|\btime\s*out\b"));
static CLOSES: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:close|hide|schlie)"));

/// Every account name read or made since J.A.I. started (lower case).
static NAMES: LazyLock<Mutex<std::collections::HashSet<String>>> = LazyLock::new(Default::default);

/// Whether `name` is an account's: "resume Spotify" is the music's.
fn known(name: &str) -> bool {
    lock(&NAMES).contains(&name.to_lowercase())
}

/// Sir's orders about the accounts: the list, a report of what J.A.I. did for someone, a pause,
/// removing one. (A name that is no account's is not taken, unless the order says "account".)
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim();
    let says_account = SAYS_ACCOUNT.is_match(s);
    if let Some(c) = LIST.captures(s) {
        let closes = c.get(1).is_some_and(|m| CLOSES.is_match(m.as_str()));
        return Some(serde_json::json!({ "action": if closes { "close" } else { "show" } }));
    }
    if let Some(c) = REPORT.captures(s) {
        let who = c[1].to_lowercase();
        let everyone = ["others", "everyone", "everybody", "anyone", "people", "guests", "anderen", "alle"].contains(&who.as_str());
        // ("What did you do today", "report on the weather": no report on someone.)
        if !everyone && !known(&who) {
            return None;
        }
        let since = match c.get(2).map(|m| m.as_str().to_lowercase()) {
            Some(w) if w.contains("week") || w.contains("woche") => "week",
            Some(w) if w.starts_with("yesterday") || w.starts_with("gestern") => "yesterday",
            Some(w) if w.contains("far") || w == "ever" => "all",
            _ => "today",
        };
        return Some(serde_json::json!({ "action": "report", "name": if everyone { "" } else { &c[1] }, "since": since }));
    }
    if let Some(c) = PAUSE.captures(s).filter(|c| says_account || known(&c[1])) {
        let n: i64 = c[2].parse().unwrap_or_else(|_| match c[2].to_lowercase().as_str() {
            "two" | "zwei" => 2,
            "three" | "drei" => 3,
            "four" | "vier" => 4,
            "five" | "fünf" | "fuenf" => 5,
            "six" | "sechs" => 6,
            "seven" => 7,
            "eight" => 8,
            "nine" => 9,
            "ten" => 10,
            "twelve" => 12,
            _ => 1,
        });
        let unit = c[3].to_lowercase();
        let minutes = n * if unit.starts_with('h') || unit.starts_with("st") { 60 } else if unit.starts_with('d') || unit.starts_with('t') { 1440 } else if unit.starts_with('w') { 10_080 } else { 1 };
        return Some(serde_json::json!({ "action": "pause", "name": &c[1], "minutes": minutes }));
    }
    if let Some(name) = RESUME.captures(s).and_then(|c| c.get(1).or(c.get(2))).map(|m| m.as_str()).filter(|n| says_account || known(n)) {
        return Some(serde_json::json!({ "action": "pause", "name": name, "minutes": 0 }));
    }
    REMOVE.captures(s).map(|c| serde_json::json!({ "action": "remove", "name": &c[1] }))
}

/// Carries out one of Sir's orders about the accounts (nobody else's: a guest allowed
/// everything still never sees them).
pub async fn run(ctx: &crate::tools::ToolContext, args: &Value) -> Result<String, String> {
    if !ctx.caller.is_sir() {
        return Err("Only Sir can see or change the accounts.".into());
    }
    let (db, main) = ctx.config.read(|c| (c.memory_db_path.clone(), c.main_pc.clone()));
    // (On another PC of his, signed in there: the accounts are his main PC's.)
    let here = !crate::fleet::is_client(&main);
    let (said, text) = match args["action"].as_str().unwrap_or("show") {
        "show" if here => {
            crate::gui::show_accounts(Some(db.clone()));
            let n = all(&db).len();
            return Ok(crate::kit::say(format!("The accounts list is open ({n} accounts)."), "The accounts are on your screen, Sir."));
        }
        "show" => {
            crate::gui::show_remote_accounts();
            return Ok(crate::kit::say("Your accounts list, from your main PC.", "Your accounts, Sir, from your main PC."));
        }
        "close" => {
            crate::gui::show_accounts(None);
            return Ok(crate::kit::say("The accounts list is closed.", "Closed, Sir."));
        }
        _ if here => act(&db, args)?,
        _ => {
            let v = crate::admin::remote(serde_json::json!({ "action": "act", "args": args })).await?;
            (v["said"].as_str().unwrap_or("Done.").to_string(), serde_json::from_value(v["text"].clone()).ok())
        }
    };
    if let Some((title, text)) = text {
        crate::kit::show_text("account_report", &title, &text, [560.0, 480.0]);
    }
    Ok(said)
}

/// What Sir's orders about the accounts change or find on his main PC: what he is told, and the
/// lines of a report to put on his screen. His own admin account is not paused or removed.
pub fn act(db: &str, args: &Value) -> Result<(String, Option<(String, String)>), String> {
    let name = args["name"].as_str().unwrap_or("").trim();
    let account = || get(db, name).filter(|a| !a.admin).ok_or_else(|| format!("There is no account called {name}."));
    let said = match args["action"].as_str().unwrap_or("") {
        "report" => return report(db, name, args["since"].as_str().unwrap_or("today")),
        "pause" => {
            let minutes = args["minutes"].as_i64().unwrap_or(60).clamp(0, 525_600);
            let a = account()?;
            update(db, &a.name, |a| a.until = if minutes > 0 { now() + minutes * 60 } else { 0 })?;
            match minutes {
                0 => crate::kit::say(format!("{} may use J.A.I. again.", a.name), format!("{} may use me again, Sir.", a.name)),
                m => {
                    let span = if m % 1440 == 0 { format!("{} day(s)", m / 1440) } else if m % 60 == 0 { format!("{} hour(s)", m / 60) } else { format!("{m} minute(s)") };
                    crate::kit::say(format!("{} is paused for {span}.", a.name), format!("{} is paused for {span}, Sir.", a.name))
                }
            }
        }
        "remove" => {
            let a = account()?;
            let vault = Vault::open(db)?;
            remove(db, &vault, &a.name);
            crate::kit::say(format!("{}'s account is removed; their phones and PCs are logged out.", a.name), format!("{}'s account is gone, Sir.", a.name))
        }
        other => return Err(format!("The accounts know no action '{other}'.")),
    };
    Ok((said, None))
}

/// What J.A.I. did for someone (everyone: an empty name) today, yesterday, this week or so far:
/// in a sentence, and line by line for Sir's screen.
fn report(db: &str, name: &str, since: &str) -> Result<(String, Option<(String, String)>), String> {
    let midnight = chrono::Local::now().date_naive().and_hms_opt(0, 0, 0).and_then(|t| t.and_local_timezone(chrono::Local).earliest()).map_or(now() - 86_400, |t| t.timestamp());
    let (from, until, span) = match since {
        "yesterday" => (midnight - 86_400, midnight, "yesterday"),
        "week" => (now() - 7 * 86_400, i64::MAX, "this week"),
        "all" => (0, i64::MAX, "so far"),
        _ => (midnight, i64::MAX, "today"),
    };
    let who = if name.is_empty() { None } else { Some(get(db, name).map(|a| a.name).ok_or_else(|| format!("There is no account called {name}."))?) };
    let done: Vec<Done> = log(db, who.as_deref(), from).into_iter().filter(|d| d.at < until).collect();
    let whom = who.clone().unwrap_or_else(|| "the others".into());
    if done.is_empty() {
        return Ok((crate::kit::say(format!("Nothing for {whom} {span}."), format!("I did nothing for {whom} {span}, Sir.")), None));
    }
    let cut = |t: &str, n: usize| if t.chars().count() > n { format!("{}…", t.chars().take(n).collect::<String>()) } else { t.to_string() };
    let lines: Vec<String> = done
        .iter()
        .map(|d| {
            let at = chrono::DateTime::from_timestamp(d.at, 0).map(|t| t.with_timezone(&chrono::Local).format("%d.%m. %H:%M").to_string()).unwrap_or_default();
            let ran = if d.tools.is_empty() { String::new() } else { format!("\n   ran: {}", d.tools) };
            format!("{at}  {} ({})\n   asked: {}\n   answer: {}{ran}", d.name, d.via, cut(&d.asked, 200), cut(&d.answered, 240))
        })
        .collect();
    let mut names: Vec<&str> = done.iter().map(|d| d.name.as_str()).collect();
    names.dedup();
    names.sort_unstable();
    names.dedup();
    let acted = done.iter().filter(|d| !d.tools.is_empty()).count();
    let spoken = format!(
        "{} {} from {} {span}{}; the list is on your screen, Sir.",
        done.len(),
        if done.len() == 1 { "request" } else { "requests" },
        if who.is_some() { whom.clone() } else { names.join(", ") },
        if acted > 0 { format!(", {acted} with a tool") } else { String::new() }
    );
    Ok((crate::kit::say(format!("{} requests from {} {span}.", done.len(), names.join(", ")), spoken), Some((format!("What I did for {whom}"), lines.join("\n\n")))))
}

// ------------------------------------------------------------------ what can be allowed ----

/// One tool as Sir sees it in an account's permissions: its name, what it does, and the values
/// of its options that can be allowed or denied one by one (`view=earth`).
pub struct Grant {
    pub tool: String,
    pub about: String,
    pub values: Vec<String>,
}

/// Every tool and its options' values, from the tools' own descriptions: the options with a
/// fixed list of values, and an `action` described as a list ("earth: show, show_satellites …").
pub fn grants(definitions: &[Value]) -> Vec<Grant> {
    static LISTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|[,.;/]\s*)(?:[a-z_]+:\s*)?([a-z][a-z_]*[a-z])\b").expect("listed actions"));
    // (What an action needs, in brackets, is no action: "show_place (place)".)
    static NEEDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\([^)]*\)").expect("brackets"));
    let mut grants: Vec<Grant> = definitions
        .iter()
        .filter_map(|d| {
            let f = &d["function"];
            let tool = f["name"].as_str()?.to_string();
            let mut values = Vec::new();
            for (option, spec) in f["parameters"]["properties"].as_object().into_iter().flatten() {
                let listed: Vec<String> = match spec["enum"].as_array() {
                    Some(fixed) => fixed.iter().filter_map(|v| v.as_str().map(str::to_lowercase)).collect(),
                    None if option == "action" => LISTED.captures_iter(&NEEDS.replace_all(spec["description"].as_str().unwrap_or(""), "")).map(|c| c[1].to_string()).collect(),
                    None => Vec::new(),
                };
                for value in listed {
                    let key = format!("{option}={value}");
                    if !values.contains(&key) {
                        values.push(key);
                    }
                }
            }
            let about = f["description"].as_str().unwrap_or("").split(['.', '\n']).next().unwrap_or("").chars().take(140).collect();
            Some(Grant { tool, about, values })
        })
        .collect();
    // (The PC's power is no tool of the model's: asked for, it is the brain's own dialogue.)
    grants.push(Grant { tool: POWER.into(), about: "Shut down, restart, sleep or lock this PC".into(), values: Vec::new() });
    // (Root on J.A.I.OS is the kit's, with no schema of the model's: its actions one by one.)
    grants.push(Grant { tool: crate::os_orders::TOOL.into(), about: "Root on J.A.I.OS: install, remove, update, roll back, run as root".into(), values: crate::os_orders::ACTIONS.iter().map(|a| format!("action={a}")).collect() });
    grants.sort_by(|a, b| a.tool.cmp(&b.tool));
    grants
}

/// What an account needs to switch the PC off, restart it, put it to sleep or lock it.
pub const POWER: &str = "pc_power";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn db() -> String {
        let dir = std::env::temp_dir().join(format!("jai_accounts_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("jai_memory.db").to_string_lossy().to_string()
    }

    #[test]
    fn a_new_account_asks_the_model_only_until_sir_allows_more() {
        let mut max = Switches::default();
        let earth = json!({"view": "earth"});
        let satellites = json!({"view": "earth", "action": "show_satellites", "name": "starlink"});
        // New: nothing at all.
        assert!(max.refusal("open_jai_view", &earth).is_some());
        assert!(max.refusal("shutdown_pc", &json!({})).is_some());
        // The Earth, but not its satellites (Sir's own example).
        max.set("open_jai_view:view=earth", true);
        assert!(max.refusal("open_jai_view", &earth).is_none());
        max.set("open_jai_view:action=show_satellites", false);
        assert!(max.refusal("open_jai_view", &satellites).is_some());
        assert!(max.refusal("open_jai_view", &json!({"view": "disk_map"})).is_some(), "another view of the same tool stays shut");
        // The whole tool, then one value taken back.
        max.set("open_jai_view", true);
        assert!(max.refusal("open_jai_view", &json!({"view": "disk_map"})).is_none());
        assert!(max.allows("open_jai_view") && !max.deny.iter().any(|d| d.contains("satellites")), "a whole tool clears its values");
        max.set("open_jai_view:view=disk_map", false);
        assert!(max.refusal("open_jai_view", &json!({"view": "disk_map"})).is_some());
        assert!(!max.allows("open_jai_view:view=disk_map") && max.allows("open_jai_view:view=earth"));
        // Everything.
        let all = Switches { allow: vec!["*".into()], ..Default::default() };
        assert!(all.everything() && !max.everything());
        assert!(all.refusal("shutdown_pc", &json!({"action": "shutdown"})).is_none());
    }

    /// Everything on its own PC is nothing on Sir's (Sir, 2026-11-20): the two places are kept
    /// apart, and an account saved before they were keeps its switches for its own PC.
    #[test]
    fn its_own_pc_and_sirs_are_allowed_apart() {
        let mut max = Account::new("Max");
        max.own.set("control_music", true);
        assert!(max.own.refusal("control_music", &json!({})).is_none() && max.here.refusal("control_music", &json!({})).is_some());
        max.own = Switches { allow: vec!["*".into()], ..Default::default() };
        assert!(max.here.refusal("control_music", &json!({})).is_some() && max.here.refusal("open_jai_view", &json!({"view": "earth"})).is_some());
        max.here.set("open_jai_view:view=earth", true);
        assert!(max.here.refusal("open_jai_view", &json!({"view": "earth"})).is_none() && max.here.refusal("control_music", &json!({})).is_some());
        // Kept as before beside its name; Sir's PC's apart.
        let saved = serde_json::to_value(&max).unwrap();
        assert_eq!(saved["allow"], json!(["*"]));
        assert_eq!(saved["here"]["allow"], json!(["open_jai_view:view=earth"]));
        assert_eq!(serde_json::from_value::<Account>(saved).unwrap(), max);
        let older: Account = serde_json::from_str(r#"{"name":"Anna","allow":["control_music"],"deny":["control_music:action=pause"],"topics":["maths"],"until":0,"created":1}"#).unwrap();
        assert_eq!(older.own.allow, vec!["control_music".to_string()]);
        assert!(older.own.deny.len() == 1 && older.here == Switches::default() && older.topics.len() == 1);
    }

    #[test]
    fn closed_topics_are_turned_away_in_english_and_german() {
        let mut max = Account::new("Max");
        assert!(max.turned_away("where am I").is_none());
        max.topics = vec!["location".into(), "maths".into(), "pizza".into()];
        for asked in ["Where am I?", "wo bin ich gerade", "what's my location", "what is 1 + 1", "was ist 7 mal 8", "calculate the square root of 2", "I want a Pizza"] {
            assert!(max.turned_away(asked).is_some(), "{asked}");
        }
        for free in ["tell me a joke", "who wrote Faust", "what is a planet", "the history of pizzas", "what happened on 2026-11-20"] {
            assert!(max.turned_away(free).is_none(), "{free}");
        }
        max.until = now() + 600;
        assert!(max.turned_away("tell me a joke").is_some_and(|w| w.contains("paused")));
    }

    #[test]
    fn names_and_passwords_have_three_to_twenty_characters() {
        assert!(check("Max", true).is_ok() && check("max_mustermann.1", true).is_ok());
        assert!(check("Mx", true).is_err() && check("a_name_far_too_long_x", true).is_err());
        assert!(check("Max Mustermann", true).is_err(), "no spaces in a name");
        assert!(check("p@ss word!", false).is_ok() && check("pw", false).is_err() && check("abc\n", false).is_err());
    }

    #[test]
    fn signing_up_needs_the_key_from_sirs_screen_and_cannot_flood_it() {
        let (db, vault) = (db(), Vault::in_memory());
        let code = request(&db, "Max", "secret1", "10.0.0.7").unwrap();
        assert_eq!(code.len(), 6);
        // The same address again at once, and anyone within 20 s: turned away.
        assert!(request(&db, "Anna", "secret2", "10.0.0.7").is_err());
        assert!(request(&db, "Anna", "secret2", "10.0.0.8").is_err());
        // A wrong key three times voids it.
        let wrong = if code == "000000" { "111111" } else { "000000" };
        assert!(finish(&db, &vault, "Max", wrong, "10.0.0.7").unwrap_err().contains("2 tries"));
        let key = finish(&db, &vault, "Max", &code, "10.0.0.7").unwrap();
        assert_eq!(session(&db, &key).as_deref(), Some("Max"));
        let max = get(&db, "max").unwrap();
        assert!(max.own == Switches::default() && max.here == Switches::default(), "a new account asks the model only");
        assert_eq!(password(&vault, "Max").as_deref(), Some("secret1"), "Sir can read it in his list");
        // Logging in again; a wrong password five times locks the name.
        assert!(login(&db, &vault, "MAX", "secret1", "10.0.0.9").is_ok());
        for _ in 0..5 {
            assert!(login(&db, &vault, "max", "nope", "10.0.0.10").is_err());
        }
        assert!(login(&db, &vault, "max", "secret1", "10.0.0.11").unwrap_err().contains("Too many"));
        // Removed: its keys are dead.
        assert!(remove(&db, &vault, "Max"));
        assert!(session(&db, &key).is_none() && password(&vault, "Max").is_none());
    }

    #[test]
    fn every_tool_offers_its_options_values_to_allow_one_by_one() {
        let definitions = [crate::jai_views::tool_definition()];
        let grants = grants(&definitions);
        let earth = grants.iter().find(|g| g.tool == "open_jai_view").unwrap();
        for value in ["view=earth", "view=disk_map", "action=show_satellites", "action=show_buildings", "action=rescan"] {
            assert!(earth.values.iter().any(|v| v == value), "{value} in {:?}", earth.values);
        }
        assert!(!earth.values.iter().any(|v| v == "action=network_radar" || v == "action=place"), "{:?}", earth.values);
    }

    #[test]
    fn a_voice_sir_introduced_keeps_what_it_was_allowed() {
        let db = db();
        // (A voice speaks at Sir's PC: what it may is for his PC.)
        let tom = for_voice(&db, "Tom", "music").unwrap();
        assert!(tom.here.allow.len() == 3 && tom.own == Switches::default());
        assert_eq!(for_voice(&db, "Lena", "everything").unwrap().here.allow, vec!["*".to_string()]);
        assert!(for_voice(&db, "Kai", "").unwrap().here.allow.is_empty());
        assert_eq!(for_voice(&db, "tom", "everything").unwrap().here.allow.len(), 3, "an account it has stays as Sir set it");
    }

    /// Everything but one tool: a whole tool switched off under "everything" is denied.
    #[test]
    fn everything_but_one_tool() {
        let mut lena = Switches { allow: vec!["*".into()], ..Default::default() };
        lena.set("open_jai_view", false);
        assert!(lena.refusal("open_jai_view", &json!({"view": "earth"})).is_some() && lena.refusal("control_music", &json!({})).is_none());
        lena.set("open_jai_view", true);
        assert!(lena.refusal("open_jai_view", &json!({"view": "earth"})).is_none());
    }

    /// "Open the accounts list", "what did you do for Max today", "time out Max for two hours":
    /// read in Sir's words; the music's "resume Spotify" and a question about the weather are not.
    #[test]
    fn sirs_orders_about_the_accounts_are_read_in_his_words() {
        let db = db();
        for_voice(&db, "Max", "").unwrap();
        let _ = all(&db);
        let action = |s: &str| command(s).map(|v| format!("{} {} {} {}", v["action"].as_str().unwrap_or(""), v["name"].as_str().unwrap_or(""), v["since"].as_str().unwrap_or(""), v["minutes"].as_i64().map_or(String::new(), |m| m.to_string())).trim().to_string());
        for (said, read) in [
            ("open the accounts list", "show"),
            ("Show me the accounts", "show"),
            ("zeig mir die Kontenliste", "show"),
            ("who uses you?", "show"),
            ("close the accounts list", "close"),
            ("what did you do for Max today?", "report Max today"),
            ("what has max asked you this week", "report max week"),
            ("what did you do for the others yesterday", "report  yesterday"),
            ("was hast du für Max gemacht", "report Max today"),
            ("time out Max for two hours", "pause Max  120"),
            ("time out Max for 2 hours", "pause Max  120"),
            ("pause Max's account for a day", "pause Max  1440"),
            ("pause max for 30 minutes", "pause max  30"),
            ("resume Max", "pause Max  0"),
            ("delete Max's account", "remove Max"),
        ] {
            assert_eq!(action(said).unwrap_or_default(), read, "{said}");
        }
        for not_ours in ["resume spotify", "pause spotify for 10 minutes", "report on the weather", "what did you do today", "pause the music for 5 minutes", "remove the widget"] {
            assert!(command(not_ours).is_none(), "{not_ours}");
        }
    }

    /// Sir's admin account: made once, under any name; it logs in nowhere (no PC or phone holds
    /// it), no voice is taken for it, and his orders neither pause nor remove it.
    #[test]
    fn his_admin_account_signs_in_at_admin_windows_only() {
        let (db, vault) = (db(), Vault::in_memory());
        assert!(check("admin", true).is_err(), "nobody else is called admin");
        make_admin(&db, &vault, "admin", "geheim123").unwrap();
        assert!(admin(&db).is_some_and(|a| a.admin && a.name == "admin"));
        assert!(make_admin(&db, &vault, "Max", "geheim123").is_err(), "once");
        assert!(login(&db, &vault, "admin", "geheim123", "10.0.1.1").unwrap_err().contains("admin windows"));
        assert!(verify(&db, &vault, "ADMIN", "geheim123", "10.0.1.1").is_ok_and(|a| a.admin));
        assert!(for_voice(&db, "Admin", "everything").is_none(), "a voice of his account's name is not his");
        for args in [json!({"action": "pause", "name": "admin", "minutes": 60}), json!({"action": "remove", "name": "admin"})] {
            assert!(act(&db, &args).is_err(), "{args}");
        }
        assert!(get(&db, "admin").is_some_and(|a| a.until == 0));
        assert!(act(&db, &json!({"action": "report", "name": "", "since": "week"})).is_ok_and(|(_, lines)| lines.is_none()), "nothing done yet: no list");
    }
}
