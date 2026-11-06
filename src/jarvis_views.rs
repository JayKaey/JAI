//! Jarvis' own views: "open my system stats", "show my schedule on monitor 2", "open the
//! simulation lab", "the QR code for my phone", "my gym logs and finances", "my contacts". Each
//! opens as a ROSE block inside the HUD (`hud_dock`); "in a new / separate window" or a monitor
//! makes it its own native OS window.
//!
//! These names are Jarvis' panels, never programs: they must not reach the app launcher (which
//! once searched the PC for an application called "Message From Stats", Whisper's version of
//! "my system stats"). Every view becomes an egui deferred viewport — an independent, movable,
//! resizable OS window — through the window engine (`spawn_native_window`) or the view's own
//! window (Simulation Lab, satellite pairing, Gym coach).

use crate::tools::{str_arg, ToolContext};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JarvisSubWindow {
    /// Live CPU / GPU / RAM / VRAM / temperatures, the busiest processes and the network.
    SystemStatsMonitor,
    /// The WebUntis timetable and the calendar.
    ScheduleAndCalendar,
    /// The interactive physics and maths canvas.
    SimulationLab,
    /// The worldwide HTTPS link and its QR code.
    MobileSatelliteQr,
    /// Gym PR logs and the finance tracker.
    WorkoutAndFinance,
    /// Saved contacts.
    Contacts,
}

impl JarvisSubWindow {
    pub fn id(self) -> &'static str {
        match self {
            Self::SystemStatsMonitor => "system_stats",
            Self::ScheduleAndCalendar => "schedule",
            Self::SimulationLab => "simulation_lab",
            Self::MobileSatelliteQr => "mobile_qr",
            Self::WorkoutAndFinance => "workout_finance",
            Self::Contacts => "contacts",
        }
    }

    /// A view as Sir or the model names it ("system stats", "telemetry", "webuntis", "qr code").
    pub fn parse(name: &str) -> Option<Self> {
        let n = name.trim().to_lowercase().replace(['-', '_'], " ");
        let n = n.trim_start_matches("the ").trim_start_matches("my ").trim();
        let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
        Some(if has(&["system", "stats", "statistic", "telemetry", "telemetrie", "performance", "hardware", "pc status", "cpu", "gpu", "monitoring"]) {
            Self::SystemStatsMonitor
        } else if has(&["schedule", "calendar", "kalender", "webuntis", "timetable", "stundenplan", "agenda", "termine"]) {
            Self::ScheduleAndCalendar
        } else if has(&["simulation", "simlab", "sim lab", "physics", "canvas"]) {
            Self::SimulationLab
        } else if has(&["qr", "mobile", "satellite", "phone link", "handy"]) {
            Self::MobileSatelliteQr
        } else if has(&["gym", "workout", "finance", "finanz", "training", "money", "budget"]) {
            Self::WorkoutAndFinance
        } else if has(&["contact", "kontakt"]) {
            Self::Contacts
        } else {
            return None;
        })
    }

    fn title(self) -> &'static str {
        match self {
            Self::SystemStatsMonitor => "J.A.R.V.I.S. // SYSTEM TELEMETRY",
            Self::ScheduleAndCalendar => "J.A.R.V.I.S. // SCHEDULE & CALENDAR",
            Self::SimulationLab => "J.A.R.V.I.S. // SIMULATION LAB",
            Self::MobileSatelliteQr => "J.A.R.V.I.S. // MOBILE SATELLITE",
            Self::WorkoutAndFinance => "J.A.R.V.I.S. // FINANCE TRACKER",
            Self::Contacts => "J.A.R.V.I.S. // CONTACTS",
        }
    }
}

/// A tool's display text without its trailing "Spoken:" line.
fn display_part(output: &str) -> String {
    match output.rsplit_once("\nSpoken: ") {
        Some((display, _)) => display.trim().to_string(),
        None => output.trim().to_string(),
    }
}

/// "… on Monitor 2" for the spoken line.
fn on_monitor(args: &Value) -> String {
    match args.get("monitor") {
        Some(Value::Number(n)) => format!(" on Monitor {n}"),
        Some(Value::String(s)) if !s.trim().is_empty() => match s.trim().parse::<u32>() {
            Ok(n) => format!(" on Monitor {n}"),
            Err(_) => format!(" on the {s} monitor"),
        },
        _ => String::new(),
    }
}

/// Opens a markdown window through the window engine (refreshed when it is open already).
async fn markdown_window(ctx: &ToolContext, view: JarvisSubWindow, markdown: String, args: &Value, size: (u32, u32)) -> Result<String, String> {
    let mut spec = json!({
        "id": view.id(),
        "title": view.title(),
        "markdown": markdown,
        "replace_content": true,
        "width": size.0,
        "height": size.1,
    });
    if let Some(m) = args.get("monitor") {
        spec["monitor"] = m.clone();
    }
    if separate(args) {
        spec["separate_window"] = json!(true);
    }
    let outcome = Box::pin(ctx.execute("spawn_native_window", &spec)).await;
    if outcome.success {
        Ok(outcome.output)
    } else {
        Err(outcome.output.trim_start_matches("Error: ").to_string())
    }
}

/// Sir asked for an OS window ("in a new window") or a monitor.
fn separate(args: &Value) -> bool {
    args.get("separate_window").and_then(Value::as_bool) == Some(true) || args.get("monitor").is_some_and(|m| !m.is_null())
}

/// "in its own window on Monitor 2" or "on your HUD" for the spoken line.
fn placed(args: &Value) -> String {
    if separate(args) {
        format!(" in its own window{}", on_monitor(args))
    } else {
        " on your HUD".to_string()
    }
}

/// `open_jarvis_view`: the view as a HUD block (or its own window). The result ends with a
/// "Spoken:" line.
pub async fn run(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let raw = str_arg(args, &["view", "window_kind", "name", "target"]).unwrap_or("system stats");
    let view = JarvisSubWindow::parse(raw).ok_or_else(|| format!("'{raw}' is not one of my views (system stats, schedule, simulation lab, mobile QR, gym and finances, contacts)."))?;
    if !crate::gui::windows_supported() {
        return Err("Jarvis is running without a display (headless), so he cannot open windows.".into());
    }
    let on = placed(args);
    match view {
        JarvisSubWindow::SystemStatsMonitor => {
            let mut spec = json!({
                "id": "system_telemetry",
                "title": view.title(),
                "live": ["pc_stats", "processes", "network"],
                "width": 620,
                "height": 760,
                "resizable": true,
            });
            if let Some(m) = args.get("monitor") {
                spec["monitor"] = m.clone();
            }
            if separate(args) {
                spec["separate_window"] = json!(true);
            }
            let outcome = Box::pin(ctx.execute("spawn_native_window", &spec)).await;
            if !outcome.success {
                return Err(outcome.output.trim_start_matches("Error: ").to_string());
            }
            let spoken = if outcome.output.contains("already open") {
                "Your system telemetry is already up, Sir. I've brought it to the front.".to_string()
            } else {
                format!("Right away, Sir. Your system telemetry is live{on}.")
            };
            Ok(format!("{}\nSpoken: {spoken}", outcome.output))
        }
        JarvisSubWindow::ScheduleAndCalendar => {
            let calendar = Box::pin(ctx.execute("get_calendar_schedule", &json!({ "date_or_range": "this week" }))).await;
            let school = Box::pin(ctx.execute("check_school_schedule", &json!({ "target_date": "today" }))).await;
            let section = |name: &str, ok: bool, out: &str| {
                let body = display_part(out.trim_start_matches("Error: "));
                if ok {
                    format!("## {name}\n\n{body}")
                } else {
                    format!("## {name}\n\n_Not available: {body}_")
                }
            };
            let markdown = format!(
                "{}\n\n{}",
                section("Today's timetable (WebUntis)", school.success, &school.output),
                section("Calendar — this week", calendar.success, &calendar.output)
            );
            let out = markdown_window(ctx, view, markdown, args, (640, 720)).await?;
            Ok(format!("{out}\nSpoken: There you are, Sir. Your timetable and this week's calendar{on}."))
        }
        JarvisSubWindow::SimulationLab => {
            let kind = str_arg(args, &["sim_type"]).unwrap_or("projectile");
            let outcome = Box::pin(ctx.execute("launch_interactive_simulation", &json!({ "sim_type": kind, "title": "Simulation Lab", "separate_window": separate(args) }))).await;
            if !outcome.success {
                return Err(outcome.output.trim_start_matches("Error: ").to_string());
            }
            Ok(format!("{}\nSpoken: The Simulation Lab is open{on}, Sir. Drag the sliders, or just tell me what to change.", outcome.output))
        }
        JarvisSubWindow::MobileSatelliteQr => {
            if separate(args) {
                crate::hud_dock::detach("pairing");
            }
            let outcome = Box::pin(ctx.execute("show_mobile_satellite_instructions", &json!({}))).await;
            if !outcome.success {
                return Err(outcome.output.trim_start_matches("Error: ").to_string());
            }
            // The satellite tool opens the pairing window (QR code + link) itself.
            let display = display_part(&outcome.output);
            Ok(format!("{display}\nSpoken: Your pairing QR code is up{on}, Sir. Scan it with your phone's camera."))
        }
        JarvisSubWindow::WorkoutAndFinance => {
            crate::gui::open_workout();
            let finance = Box::pin(ctx.execute("manage_finances_and_goals", &json!({ "action": "summary" }))).await;
            let markdown = if finance.success {
                display_part(&finance.output)
            } else {
                format!("_The finance tracker is not available: {}_", display_part(finance.output.trim_start_matches("Error: ")))
            };
            let out = markdown_window(ctx, view, markdown, args, (520, 560)).await?;
            Ok(format!("{out}\nSpoken: Your gym coach and this month's finances are up{on}, Sir."))
        }
        JarvisSubWindow::Contacts => {
            let contacts = Box::pin(ctx.execute("manage_contacts", &json!({ "action": "list" }))).await;
            let markdown = if contacts.success {
                format!("## Contacts\n\n{}", display_part(&contacts.output))
            } else {
                format!("_Contacts are not available: {}_", display_part(contacts.output.trim_start_matches("Error: ")))
            };
            let out = markdown_window(ctx, view, markdown, args, (480, 560)).await?;
            Ok(format!("{out}\nSpoken: Your contacts{on}, Sir."))
        }
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "open_jarvis_view",
        "description": "Open one of YOUR OWN views as a separate, movable native window (never an installed program): system stats / telemetry / system monitor, schedule (WebUntis timetable + calendar), simulation lab, mobile QR code, gym & finance tracker, contacts. \"Open my system stats in a new window\" → view system_stats.",
        "parameters": {"type": "object", "properties": {
            "view": {"type": "string", "enum": ["system_stats", "schedule", "simulation_lab", "mobile_qr", "workout_finance", "contacts"]},
            "monitor": {"type": "string", "description": "1 = primary, 2 = second …, left, right"}
        }, "required": ["view"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_names() {
        for (name, view) in [
            ("system stats", JarvisSubWindow::SystemStatsMonitor),
            ("system_stats", JarvisSubWindow::SystemStatsMonitor),
            ("my telemetry", JarvisSubWindow::SystemStatsMonitor),
            ("webuntis", JarvisSubWindow::ScheduleAndCalendar),
            ("the calendar", JarvisSubWindow::ScheduleAndCalendar),
            ("simulation lab", JarvisSubWindow::SimulationLab),
            ("QR code", JarvisSubWindow::MobileSatelliteQr),
            ("mobile_qr", JarvisSubWindow::MobileSatelliteQr),
            ("gym logs", JarvisSubWindow::WorkoutAndFinance),
            ("finance tracker", JarvisSubWindow::WorkoutAndFinance),
            ("contacts", JarvisSubWindow::Contacts),
        ] {
            assert_eq!(JarvisSubWindow::parse(name), Some(view), "{name}");
        }
        assert_eq!(JarvisSubWindow::parse("discord"), None);
    }

    #[test]
    fn spoken_monitor_suffix() {
        assert_eq!(on_monitor(&json!({ "monitor": 2 })), " on Monitor 2");
        assert_eq!(on_monitor(&json!({ "monitor": "2" })), " on Monitor 2");
        assert_eq!(on_monitor(&json!({ "monitor": "left" })), " on the left monitor");
        assert_eq!(on_monitor(&json!({})), "");
    }
}
