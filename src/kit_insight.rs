//! Three looks at things (2026-11-13): the evening recap ("what did I do today?"), the
//! internet doctor ("why is my internet slow?": router, line, names, one after the other, and
//! where the fault lies in plain words), and the privacy board (everything J.A.I. keeps about
//! Sir on this PC, each with a key that deletes it).

use crate::kit::say;
use crate::tools::ToolContext;
use eframe::egui::{self, Color32, FontId, RichText};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid phrase")
}

// ------------------------------------------------------------------------ evening recap ----

static RECAP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:what\s+did\s+i\s+do\s+today|what\s+have\s+i\s+done\s+today|recap\s+(?:my|the)\s+day|(?:my\s+|the\s+)?(?:evening|daily|day)\s+recap|sum\s+up\s+my\s+day|how\s+did\s+my\s+day\s+go|tagesrückblick|was\s+habe\s+ich\s+heute\s+gemacht)$")
});

pub fn recap_command(s: &str) -> Option<Value> {
    RECAP.is_match(s).then(|| json!({}))
}

/// The day in a few lines: where the time went, what was asked of him, what was noted, what
/// comes tomorrow. (Pure: the gathered numbers in, the text and the spoken line out.)
pub fn recap_text(apps: &[(String, u64)], total_secs: u64, orders: (usize, usize), notes: &[String], tomorrow: &[String]) -> (String, String) {
    let mut lines: Vec<String> = Vec::new();
    let mut spoken: Vec<String> = Vec::new();
    if total_secs >= 60 {
        lines.push(format!("AT THE SCREEN  ·  {}", crate::screen_time::span(total_secs)));
        for (app, secs) in apps.iter().take(6) {
            let share = (*secs as f32 / total_secs.max(1) as f32 * 100.0).round() as usize;
            lines.push(format!("    {:<18} {:>9}   {share} %", crate::screen_time::app_name(app), crate::screen_time::span(*secs)));
        }
        let top = apps.first().map(|(app, secs)| format!(", most of it in {} ({})", crate::screen_time::app_name(app), crate::screen_time::span(*secs))).unwrap_or_default();
        spoken.push(format!("{} at the screen today{top}", crate::screen_time::span(total_secs)));
    } else {
        lines.push("AT THE SCREEN  ·  hardly at all today".to_string());
    }
    lines.push(String::new());
    let (asked, led) = orders;
    lines.push(match asked {
        0 => "ORDERS BY VOICE  ·  none today".to_string(),
        _ => format!("ORDERS BY VOICE  ·  {asked}, of which {led} I knew what to do with"),
    });
    if asked > 0 {
        spoken.push(format!("{asked} order{} by voice", if asked == 1 { "" } else { "s" }));
    }
    if !notes.is_empty() {
        lines.push(String::new());
        lines.push(format!("NOTED TODAY  ·  {}", notes.len()));
        lines.extend(notes.iter().take(6).map(|n| format!("    {n}")));
        spoken.push(format!("{} note{}", notes.len(), if notes.len() == 1 { "" } else { "s" }));
    }
    lines.push(String::new());
    if tomorrow.is_empty() {
        lines.push("TOMORROW  ·  no reminder is set".to_string());
    } else {
        lines.push(format!("TOMORROW  ·  {} reminder{}", tomorrow.len(), if tomorrow.len() == 1 { "" } else { "s" }));
        lines.extend(tomorrow.iter().take(6).map(|r| format!("    {r}")));
        spoken.push(format!("tomorrow: {}", tomorrow[0]));
    }
    let said = if spoken.is_empty() { "A quiet day, Sir: nothing to report.".to_string() } else { format!("{}, Sir.", capitalised(&spoken.join("; "))) };
    (lines.join("\n"), said)
}

fn capitalised(text: &str) -> String {
    let mut c = text.chars();
    c.next().map_or(String::new(), |first| first.to_uppercase().chain(c).collect())
}

pub async fn recap(ctx: &ToolContext) -> Result<String, String> {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let (apps, total) = ctx.stark.features.bench.screen.today();
    let day = today.clone();
    let (orders, notes) = tokio::task::spawn_blocking(move || {
        let clips: Vec<crate::voice_clips::Clip> = crate::voice_clips::list(crate::voice_clips::KEPT).into_iter().filter(|c| c.at.starts_with(&day)).collect();
        let notes: Vec<String> = crate::kit::list("note").into_iter().filter(|(_, _, at)| at.starts_with(&day)).filter_map(|(_, v, _)| v["text"].as_str().map(str::to_string)).collect();
        ((clips.len(), clips.iter().filter(|c| c.led).count()), notes)
    })
    .await
    .map_err(|e| e.to_string())?;
    let tomorrow_day = (chrono::Local::now() + chrono::Duration::days(1)).date_naive();
    let mut reminders = ctx.reminders.list();
    reminders.sort_by_key(|r| r.due_ms);
    let tomorrow: Vec<String> = reminders.iter().filter(|r| r.due().date_naive() == tomorrow_day).map(|r| format!("{}  {}", r.due().format("%H:%M"), r.text)).collect();
    let (text, spoken) = recap_text(&apps, total, orders, &notes, &tomorrow);
    crate::kit::show_text("kit_recap", &format!("Recap // {today}"), &text, [520.0, 420.0]);
    Ok(say(text, spoken))
}

// ---------------------------------------------------------------------- internet doctor ----

static DOCTOR: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:why\s+is\s+(?:my\s+|the\s+)?(?:internet|connection|wi\s*fi|wlan|network)\s+(?:so\s+)?(?:slow|bad|lagging|laggy|unstable|down)|(?:my\s+|the\s+)?(?:internet|connection|wi\s*fi|wlan)\s+is\s+(?:so\s+)?(?:slow|bad|lagging|laggy|unstable)|internet\s+doctor|(?:check|test|diagnose)\s+(?:my\s+|the\s+)?(?:internet|connection)\s+(?:properly|thoroughly|step\s+by\s+step)|what\s+is\s+wrong\s+with\s+(?:my\s+|the\s+)?(?:internet|connection|wi\s*fi|wlan)|warum\s+ist\s+(?:mein\s+|das\s+)?(?:internet|wlan)\s+(?:so\s+)?(?:langsam|schlecht)|(?:mein\s+|das\s+)?(?:internet|wlan)\s+ist\s+(?:so\s+)?(?:langsam|schlecht))$")
});

pub fn doctor_command(s: &str) -> Option<Value> {
    DOCTOR.is_match(s).then(|| json!({}))
}

/// What one stretch of the way answered: how many of the asks came back, and how fast.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stretch {
    pub avg_ms: Option<f64>,
    pub loss: f64,
    pub jitter_ms: f64,
}

impl Stretch {
    fn of(stats: &crate::network_guardian::PingStats) -> Stretch {
        Stretch { avg_ms: stats.avg(), loss: stats.loss(), jitter_ms: stats.jitter().unwrap_or(0.0) }
    }

    fn words(&self) -> String {
        match self.avg_ms {
            Some(avg) => format!("{avg:.0} ms, {:.0} % lost, jitter {:.0} ms", self.loss * 100.0, self.jitter_ms),
            None => "no answer".to_string(),
        }
    }
}

/// Where the fault lies: the first stretch of the way that does not answer as it should, in
/// plain words (what stands in the block, and what he says).
pub fn diagnose(router: Option<Stretch>, internet: Stretch, dns_ms: Option<u128>, web_ms: Option<u128>) -> (String, String) {
    let dead = |s: &Stretch| s.avg_ms.is_none() || s.loss >= 0.4;
    match router {
        None => return ("This PC has no way to a router: the network cable is out or the Wi-Fi is not connected.".to_string(), "This PC is not connected to your router at all, Sir. The cable or the Wi-Fi is the fault.".to_string()),
        Some(r) if dead(&r) => {
            return (
                "The router does not answer (or loses most of what is sent): the fault is between this PC and the router. Check the cable or the Wi-Fi signal, then restart the router.".to_string(),
                "Your router hardly answers, Sir. The fault is between this PC and the router: the cable or the Wi-Fi.".to_string(),
            )
        }
        _ => {}
    }
    let r = router.expect("a router that answers");
    if dead(&internet) {
        return (
            "The router answers, the internet beyond it does not: the fault is the router's line or your provider. Restarting the router is the first thing to try.".to_string(),
            "Your router is fine, Sir, but nothing beyond it answers. That is the line or your provider; restart the router first.".to_string(),
        );
    }
    if r.avg_ms.unwrap_or(0.0) > 30.0 || r.jitter_ms > 15.0 || r.loss > 0.0 {
        return (
            format!("The way to your own router is slow or unsteady ({}): that slows everything. On Wi-Fi, move closer or use a cable; on a cable, try another one.", r.words()),
            "The way to your own router is unsteady, Sir. That is your Wi-Fi or the cable, not the internet.".to_string(),
        );
    }
    if internet.loss > 0.05 || internet.avg_ms.unwrap_or(0.0) > 120.0 || internet.jitter_ms > 30.0 {
        return (
            format!("The line to the internet is slow or loses packets right now ({}): your provider, or something on your network is using it up (a download, a stream).", internet.words()),
            "Your line to the internet is struggling right now, Sir. Either the provider, or something here is using it up.".to_string(),
        );
    }
    match dns_ms {
        None => {
            return (
                "The internet answers, but names are not found (DNS): websites will not open although the line is fine. Restart the router, or set another DNS server.".to_string(),
                "The line is fine, Sir, but names are not being looked up. The DNS server is the fault.".to_string(),
            )
        }
        Some(ms) if ms > 400 => {
            return (format!("The line is fine, but looking up names takes {ms} ms (DNS): every page starts late. Another DNS server would help."), "The line is fine, Sir, but looking up names is slow. That makes every page start late.".to_string());
        }
        _ => {}
    }
    if web_ms.is_none() {
        return ("Router, line and names answer, but a website did not open: something blocks the web (a firewall, a VPN, a proxy).".to_string(), "Everything answers, Sir, yet a website would not open. Something is blocking the web: a firewall or a VPN.".to_string());
    }
    (
        "Everything answers quickly. If one site or game is slow, it is that site or its server, not your connection.".to_string(),
        format!("Your connection is fine, Sir: the router answers in {:.0} milliseconds and the internet in {:.0}. If something is slow, it is that site.", r.avg_ms.unwrap_or(0.0), internet.avg_ms.unwrap_or(0.0)),
    )
}

pub async fn doctor() -> Result<String, String> {
    use crate::network_guardian::{gateway, ping};
    let router_at = gateway().await;
    let router_host = router_at.clone().unwrap_or_default();
    let dns_started = Instant::now();
    let (router, internet, second, dns) = tokio::join!(
        async {
            match &router_at {
                Some(host) => Some(ping(host, "Router", 5).await),
                None => None,
            }
        },
        ping("1.1.1.1", "Internet", 5),
        ping("8.8.8.8", "Internet (second)", 3),
        tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host("example.com:443")),
    );
    let dns_ms = matches!(dns, Ok(Ok(_))).then(|| dns_started.elapsed().as_millis());
    let web_started = Instant::now();
    let web = tokio::time::timeout(Duration::from_secs(4), tokio::net::TcpStream::connect("example.com:443")).await;
    let web_ms = matches!(web, Ok(Ok(_))).then(|| web_started.elapsed().as_millis());
    // (The better of the two far ends: one of them resting says nothing about the line.)
    let far = if internet.avg().is_none() && second.avg().is_some() { &second } else { &internet };
    let (router_stretch, far_stretch) = (router.as_ref().map(Stretch::of), Stretch::of(far));
    let (verdict, spoken) = diagnose(router_stretch, far_stretch, dns_ms, web_ms);
    let lines = [
        format!("ROUTER {:<17} {}", router_host, router_stretch.map_or("not found".to_string(), |s| s.words())),
        format!("INTERNET {:<15} {}", far.host, far_stretch.words()),
        format!("NAMES (DNS)              {}", dns_ms.map_or("no answer".to_string(), |ms| format!("{ms} ms"))),
        format!("A WEBSITE                {}", web_ms.map_or("did not open".to_string(), |ms| format!("opened in {ms} ms"))),
        String::new(),
        verdict,
    ];
    let text = lines.join("\n");
    tracing::info!("[DOCTOR] router {:?}, internet {:?}, DNS {:?} ms, web {:?} ms", router_stretch.map(|s| s.words()), far_stretch.words(), dns_ms, web_ms);
    crate::kit::show_text("kit_doctor", "Internet doctor // where the fault lies", &text, [560.0, 300.0]);
    Ok(say(text, spoken))
}

// ------------------------------------------------------------------------ privacy board ----

pub const PRIVACY: &str = "kit_privacy";

static PRIVACY_SHOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:show|open)\s+(?:me\s+)?(?:the\s+|my\s+)?privacy\s+(?:board|panel|page)|privacy\s+board|what\s+do\s+you\s+(?:keep|store|save)\s+(?:about|on|of)\s+me|what\s+data\s+do\s+you\s+(?:keep|store|have)(?:\s+(?:about|on)\s+me)?|was\s+speicherst\s+du\s+über\s+mich)$")
});

/// "Stop keeping my voice clips" / "keep my voice clips again".
static CLIPS_OFF: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:stop\s+keeping|don'?t\s+keep|do\s+not\s+keep|keep\s+no(?:\s+more)?)\s+(?:my\s+|the\s+|any\s+)?(?:voice\s+clips|clips\s+of\s+my\s+voice|recordings\s+of\s+(?:me|my\s+voice))$|^(?:behalte?|speichere?)\s+keine\s+(?:sprachclips|aufnahmen)(?:\s+mehr)?(?:\s+von\s+mir)?$"));
static CLIPS_ON: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:keep|start\s+keeping)\s+(?:my\s+|the\s+)?(?:voice\s+clips|clips\s+of\s+my\s+voice)(?:\s+again)?$|^(?:behalte?|speichere?)\s+(?:wieder\s+)?(?:meine\s+)?(?:sprachclips|aufnahmen)(?:\s+wieder)?$"));

pub fn privacy_command(s: &str) -> Option<Value> {
    if CLIPS_OFF.is_match(s) {
        return Some(json!({"action": "keep_clips", "on": false}));
    }
    if CLIPS_ON.is_match(s) {
        return Some(json!({"action": "keep_clips", "on": true}));
    }
    PRIVACY_SHOW.is_match(s).then(|| json!({"action": "show"}))
}

/// One thing he keeps: what it is, how much, and the name it is deleted by (None: it is
/// shown, and deleted elsewhere).
#[derive(Debug, Clone, PartialEq)]
pub struct Kept {
    pub what: &'static str,
    pub detail: String,
    pub delete: Option<&'static str>,
}

struct PrivacyUi {
    open: bool,
    rows: Vec<Kept>,
    /// The key that asks "sure?", and since when.
    asked: Option<(&'static str, Instant)>,
}

static PRIVACY_UI: Mutex<PrivacyUi> = Mutex::new(PrivacyUi { open: false, rows: Vec::new(), asked: None });

fn megabytes(path: &str) -> String {
    std::fs::metadata(path).map(|m| format!("{:.1} MB", m.len() as f64 / 1_048_576.0)).unwrap_or_else(|_| "not there".to_string())
}

/// Everything he keeps about Sir on this PC, counted (off the HUD's thread).
pub fn gather(db: &str, log: &str) -> Vec<Kept> {
    let learnt = crate::ear::learnt();
    let count = |kind: crate::ear::Kind| learnt.iter().filter(|e| e.kind == kind).count();
    let (clips, clips_mb) = crate::voice_clips::kept();
    let guests = crate::speaker_id::guests(db);
    let n = |kind: &str| crate::kit::list(kind).len();
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let typed = crate::kit::get("console", "history").and_then(|v| v.as_array().map(Vec::len)).unwrap_or(0);
    let notes = crate::hud_notify::recent(chrono::Local::now()).len();
    let (backup_days, backup_mb) = crate::backups::kept();
    vec![
        Kept { what: "Your voice print", detail: capitalised(&crate::speaker_id::known_in_words(db).replace("I have ", "").replace("I know your voice ", "")), delete: (crate::speaker_id::known(db).is_some() || crate::speaker_id::set_aside()).then_some("voice_print") },
        Kept { what: "Clips of your voice", detail: format!("{} ({clips_mb:.1} MB), the newest {} stay; \"stop keeping my voice clips\" ends it", plural(clips, "clip", "clips"), crate::voice_clips::KEPT), delete: (clips > 0).then_some("voice_clips") },
        Kept { what: "Guests' voices", detail: if guests.is_empty() { "none".to_string() } else { guests.iter().map(|g| g.name.clone()).collect::<Vec<_>>().join(", ") }, delete: (!guests.is_empty()).then_some("guests") },
        Kept {
            what: "What I learnt of your words",
            detail: format!("{} taught or learnt, {} of your orders, {}", plural(count(crate::ear::Kind::Words) + count(crate::ear::Kind::Order) + count(crate::ear::Kind::Taught), "pair", "pairs"), count(crate::ear::Kind::Said), plural(count(crate::ear::Kind::Name), "name", "names")),
            delete: (!learnt.is_empty()).then_some("ear"),
        },
        Kept { what: "Notes", detail: plural(n("note"), "note", "notes"), delete: (n("note") > 0).then_some("note") },
        Kept { what: "Snippets", detail: plural(n("snippet"), "snippet", "snippets"), delete: (n("snippet") > 0).then_some("snippet") },
        Kept { what: "Rules", detail: plural(n("rule"), "rule", "rules"), delete: (n("rule") > 0).then_some("rule") },
        // (A layout is a saved workspace: deleted by name, "delete the study layout".)
        Kept { what: "Desk layouts", detail: format!("{}; \"delete the … layout\" removes one", plural(n("layout"), "layout", "layouts")), delete: None },
        Kept { what: "Typed lines", detail: format!("{} the console recalls with the arrow keys", plural(typed, "line", "lines")), delete: (typed > 0).then_some("console") },
        Kept { what: "Notifications", detail: format!("{} of the last twelve hours", plural(notes, "note", "notes")), delete: (notes > 0).then_some("notifications") },
        Kept { what: "Window places", detail: format!("where {} stood", plural(n("place"), "block", "blocks")), delete: (n("place") > 0).then_some("place") },
        Kept { what: "Backups", detail: format!("{} ({backup_mb:.1} MB), copies of my memory and the settings", plural(backup_days, "day", "days")), delete: (backup_days > 0).then_some("backups") },
        Kept { what: "Memory database", detail: format!("{} (conversations, memories, screen time, accounts sealed)", megabytes(db)), delete: None },
        Kept { what: "Log file", detail: format!("{} (starts afresh past 10 MB)", megabytes(log)), delete: None },
    ]
}

/// Deletes what a key of the board names; whether it was one of them.
pub fn delete(db: &str, what: &str) -> bool {
    match what {
        "voice_print" => crate::speaker_id::forget(db).is_ok(),
        "voice_clips" => {
            crate::voice_clips::delete_all();
            true
        }
        "guests" => {
            for guest in crate::speaker_id::guests(db) {
                crate::speaker_id::forget_guest(db, &guest.name, true);
            }
            true
        }
        "ear" => {
            crate::ear::forget_everything();
            true
        }
        "note" | "snippet" | "rule" | "layout" => {
            crate::kit::clear(what);
            true
        }
        "console" => {
            crate::gui::forget_console_history();
            true
        }
        "notifications" => {
            crate::hud_notify::clear();
            true
        }
        "place" => {
            crate::block_places::forget_all();
            true
        }
        "backups" => {
            crate::backups::delete_all();
            true
        }
        _ => false,
    }
}

pub fn privacy(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let (db, log) = ctx.config.read(|c| (c.memory_db_path.clone(), c.log_file.clone()));
    // Whether clips of his voice are kept at all: his word switches it, now and for later starts.
    if args["action"].as_str() == Some("keep_clips") {
        let on = args["on"].as_bool().unwrap_or(false);
        ctx.config.update(|c| c.keep_voice_clips = on);
        if on && !crate::platform::in_test_session() {
            crate::voice_clips::set_home(&db);
        } else if !on {
            crate::voice_clips::stop_keeping();
        }
        tracing::info!("[PRIVACY] Clips of Sir's voice are {} from now on", if on { "kept" } else { "no longer kept" });
        return Ok(if on {
            say("Clips of your orders are kept again: the newest two hundred, on this PC only.", "I keep clips of your orders again, Sir. On this PC only.")
        } else {
            say("No more clips of your voice are kept. The ones that are there stay until you delete them (the privacy board has the key).", "No more clips from now on, Sir. The ones I have stay until you delete them.")
        });
    }
    let rows = gather(&db, &log);
    let text = rows.iter().map(|r| format!("{}: {}", r.what, r.detail)).collect::<Vec<_>>().join("\n");
    {
        let mut ui = lock(&PRIVACY_UI);
        (ui.open, ui.rows, ui.asked) = (true, rows, None);
    }
    *lock(&PATHS) = Some((db, log));
    crate::hud_dock::cancel_close(PRIVACY);
    crate::gui::refresh();
    Ok(say(text, "Everything I keep about you is on the board, Sir, each with a key to delete it. None of it leaves this PC."))
}

/// The board as it stands with nothing kept yet, for a picture of it (the gallery test).
#[cfg(test)]
pub(crate) fn open_for_picture(open: bool) {
    let rows = if open { gather("no-such.db", "no-such.log") } else { Vec::new() };
    let mut ui = lock(&PRIVACY_UI);
    (ui.open, ui.rows, ui.asked) = (open, rows, None);
    if !open {
        drop(ui);
        crate::hud_dock::forget(PRIVACY);
    }
}

/// The database and the log, for the board's own keys.
static PATHS: Mutex<Option<(String, String)>> = Mutex::new(None);
/// A delete key asks "sure?" for this long.
const SURE_FOR: Duration = Duration::from_secs(4);

fn privacy_block(ctx: &egui::Context) {
    let (open, rows, asked) = {
        let ui = lock(&PRIVACY_UI);
        (ui.open, ui.rows.clone(), ui.asked.filter(|(_, at)| at.elapsed() < SURE_FOR))
    };
    if !open {
        return;
    }
    let ink = Color32::from_rgb(0xE8, 0xF2, 0xFC);
    let muted = Color32::from_rgb(0x8E, 0xA4, 0xB8);
    let mut clicked: Option<&'static str> = None;
    let closed = crate::gui::block(ctx, PRIVACY, "Privacy // what I keep about you", [640.0, 430.0], |ui| {
        ui.label(RichText::new("Kept on this PC only. Nothing here is ever sent anywhere.").color(muted).size(12.5));
        ui.add_space(6.0);
        crate::holo_block::faded(ui, egui::ScrollArea::vertical().auto_shrink([false; 2]), |ui| {
            for row in &rows {
                ui.horizontal(|ui| {
                    // (A column of its own width, its words at the left edge.)
                    ui.allocate_ui_with_layout(egui::vec2(196.0, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(row.what).color(ink).size(14.0)).truncate());
                    });
                    if let Some(key) = row.delete {
                        let sure = asked.is_some_and(|(k, _)| k == key);
                        let label = if sure { "SURE?" } else { "DELETE" };
                        let colour = if sure { crate::holo::theme().warn() } else { muted };
                        if crate::holo_ui::key(ui, RichText::new(label).font(FontId::monospace(11.0)).color(colour)).on_hover_text("Click twice to delete").clicked() {
                            clicked = Some(key);
                        }
                    }
                    ui.label(RichText::new(&row.detail).color(muted).size(13.0));
                });
                ui.add_space(4.0);
            }
        });
    });
    if let Some(key) = clicked {
        let sure = asked.is_some_and(|(k, _)| k == key);
        if sure {
            lock(&PRIVACY_UI).asked = None;
            if let Some((db, log)) = lock(&PATHS).clone() {
                // (Off the HUD's thread: files and the database.)
                let _ = std::thread::Builder::new().name("jai-privacy-delete".into()).spawn(move || {
                    if delete(&db, key) {
                        tracing::info!("[PRIVACY] Deleted on Sir's click: {key}");
                        if key == "voice_print" {
                            crate::hud_feel::set_voice_known(false);
                        }
                    }
                    let rows = gather(&db, &log);
                    lock(&PRIVACY_UI).rows = rows;
                    crate::gui::refresh();
                });
            }
        } else {
            lock(&PRIVACY_UI).asked = Some((key, Instant::now()));
        }
    }
    if asked.is_some() {
        ctx.request_repaint_after(Duration::from_millis(500));
    }
    if closed {
        let mut ui = lock(&PRIVACY_UI);
        (ui.open, ui.asked) = (false, None);
        drop(ui);
        crate::hud_dock::forget(PRIVACY);
    }
}

/// Draws this module's blocks.
pub fn show(ctx: &egui::Context) {
    privacy_block(ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_day_is_told_in_a_few_lines() {
        assert_eq!(recap_command("what did i do today"), Some(json!({})));
        assert_eq!(recap_command("recap my day"), Some(json!({})));
        assert_eq!(recap_command("tagesrückblick"), Some(json!({})));
        assert_eq!(recap_command("what should i do today"), None);
        let apps = [("firefox".to_string(), 5400u64), ("javaw".to_string(), 3600), ("discord".to_string(), 900)];
        let (text, spoken) = recap_text(&apps, 9900, (12, 10), &["Maths test on Friday".to_string()], &["07:30  Bus to school".to_string()]);
        assert!(text.contains("AT THE SCREEN") && text.contains("ORDERS BY VOICE  ·  12, of which 10") && text.contains("Maths test on Friday") && text.contains("07:30  Bus to school"), "{text}");
        assert!(spoken.contains("12 orders by voice") && spoken.contains("1 note") && spoken.contains("tomorrow: 07:30") && spoken.ends_with(", Sir."), "{spoken}");
        let (text, spoken) = recap_text(&[], 0, (0, 0), &[], &[]);
        assert!(text.contains("hardly at all") && text.contains("none today") && text.contains("no reminder"));
        assert_eq!(spoken, "A quiet day, Sir: nothing to report.");
    }

    #[test]
    fn the_doctor_names_the_first_stretch_that_fails() {
        for said in ["why is my internet so slow", "my wifi is lagging", "internet doctor", "what is wrong with my connection", "warum ist mein internet so langsam"] {
            assert_eq!(doctor_command(said), Some(json!({})), "{said}");
        }
        assert_eq!(doctor_command("is the internet a good thing"), None);
        let good = Stretch { avg_ms: Some(2.0), loss: 0.0, jitter_ms: 1.0 };
        let far = Stretch { avg_ms: Some(14.0), loss: 0.0, jitter_ms: 2.0 };
        let dead = Stretch { avg_ms: None, loss: 1.0, jitter_ms: 0.0 };
        assert!(diagnose(None, far, Some(20), Some(80)).1.contains("not connected to your router"));
        assert!(diagnose(Some(dead), far, Some(20), Some(80)).1.contains("between this PC and the router"));
        assert!(diagnose(Some(good), dead, None, None).1.contains("provider"));
        assert!(diagnose(Some(Stretch { avg_ms: Some(48.0), loss: 0.0, jitter_ms: 22.0 }), far, Some(20), Some(80)).1.contains("your Wi-Fi or the cable"));
        assert!(diagnose(Some(good), Stretch { avg_ms: Some(210.0), loss: 0.2, jitter_ms: 40.0 }, Some(20), Some(80)).1.contains("struggling"));
        assert!(diagnose(Some(good), far, None, Some(80)).1.contains("DNS"));
        assert!(diagnose(Some(good), far, Some(900), Some(80)).1.contains("looking up names is slow"));
        assert!(diagnose(Some(good), far, Some(20), None).1.contains("blocking the web"));
        let (text, spoken) = diagnose(Some(good), far, Some(20), Some(80));
        assert!(text.contains("Everything answers quickly") && spoken.contains("fine") && spoken.contains("2 milliseconds") && spoken.contains("14"));
    }

    #[test]
    fn the_privacy_board_counts_what_is_kept_and_deletes_by_key() {
        let _one = crate::kit::test_lock();
        assert_eq!(privacy_command("what do you keep about me"), Some(json!({"action": "show"})));
        assert_eq!(privacy_command("privacy board"), Some(json!({"action": "show"})));
        assert_eq!(privacy_command("what do you know about the moon"), None);
        crate::kit::put("note", "1", &json!({"text": "x"})).unwrap();
        crate::kit::put("note", "2", &json!({"text": "y"})).unwrap();
        crate::kit::put("rule", "1", &json!({})).unwrap();
        let rows = gather("no-such-database.db", "no-such.log");
        let row = |what: &str| rows.iter().find(|r| r.what == what).cloned().expect(what);
        assert_eq!((row("Notes").detail, row("Notes").delete), ("2 notes".to_string(), Some("note")));
        assert_eq!((row("Rules").detail, row("Snippets").delete), ("1 rule".to_string(), None));
        assert!(row("Memory database").detail.starts_with("not there") && row("Memory database").delete.is_none());
        assert!(delete("no-such-database.db", "note") && !delete("no-such-database.db", "everything"));
        assert_eq!(crate::kit::list("note").len(), 0);
        assert_eq!(crate::kit::list("rule").len(), 1, "another kind is left alone");
    }
}
