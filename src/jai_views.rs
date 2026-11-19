//! JAI's own views: "open my system stats", "show my schedule on monitor 2", "open the
//! simulation lab", "the QR code for my phone", "my gym logs and finances", "my contacts". Each
//! opens as a ROSE block inside the HUD (`hud_dock`); "in a new / separate window" or a monitor
//! makes it its own native OS window.
//!
//! These names are JAI's panels, never programs: they must not reach the app launcher (which
//! once searched the PC for an application called "Message From Stats", Whisper's version of
//! "my system stats"). Every view becomes an egui deferred viewport — an independent, movable,
//! resizable OS window — through the window engine (`spawn_native_window`) or the view's own
//! window (Simulation Lab, satellite pairing, Gym coach).

use crate::tools::{str_arg, ToolContext};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JaiSubWindow {
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
    /// The home network swept as a radar: devices by how fast they answer, the adapters, the
    /// latency to the router and the internet.
    NetworkRadar,
    /// A drive scanned live into a treemap: what takes the space, by folder and by file type.
    DiskMap,
    /// A holographic globe with earthquakes, storms, a satellite and flights.
    Earth,
    /// live layers (C1): a station's live departures board.
    Departures,
    /// live layers (C1): answers from the live data, no window (satellite passes, road traffic,
    /// another day's weather, rain in the next hour, the heads-ups).
    Live,
}

impl JaiSubWindow {
    pub fn id(self) -> &'static str {
        match self {
            Self::SystemStatsMonitor => "system_stats",
            Self::ScheduleAndCalendar => "schedule",
            Self::SimulationLab => "simulation_lab",
            Self::MobileSatelliteQr => "mobile_qr",
            Self::WorkoutAndFinance => "workout_finance",
            Self::Contacts => "contacts",
            Self::NetworkRadar => "network_radar",
            Self::DiskMap => "disk_map",
            Self::Earth => "earth",
            Self::Departures => "departures",
            Self::Live => "live",
        }
    }

    /// A view as Sir or the model names it ("system stats", "telemetry", "webuntis", "qr code").
    pub fn parse(name: &str) -> Option<Self> {
        let n = name.trim().to_lowercase().replace(['-', '_'], " ");
        let n = n.trim_start_matches("the ").trim_start_matches("my ").trim();
        let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
        // live layers (C1)
        if matches!(n, "live" | "live data") {
            return Some(Self::Live);
        }
        // (The Earth's "satellite view" before the phone's satellite link.)
        Some(if has(&["earth", "globe", "globus", "erde", "weltkugel", "world map", "weltkarte", "satellite view", "satellite map", "satellite imagery", "satellitenansicht", "satellitenbild"]) {
            Self::Earth
        } else if has(&["radar", "network", "netzwerk", "lan scan"]) {
            Self::NetworkRadar
        } else if has(&["disk map", "drive map", "storage map", "space map", "disk usage", "treemap", "speicherkarte", "festplattenkarte"]) {
            Self::DiskMap
        } else if has(&["departure", "abfahrt", "train board"]) {
            // live layers (C1)
            Self::Departures
        } else if has(&["system", "stats", "statistic", "telemetry", "telemetrie", "performance", "hardware", "pc status", "cpu", "gpu", "monitoring"]) {
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
            Self::SystemStatsMonitor => "J.A.I. // SYSTEM TELEMETRY",
            Self::ScheduleAndCalendar => "J.A.I. // SCHEDULE & CALENDAR",
            Self::SimulationLab => "J.A.I. // SIMULATION LAB",
            Self::MobileSatelliteQr => "J.A.I. // MOBILE SATELLITE",
            Self::WorkoutAndFinance => "J.A.I. // FINANCE TRACKER",
            Self::Contacts => "J.A.I. // CONTACTS",
            Self::NetworkRadar => "J.A.I. // NETWORK RADAR",
            Self::DiskMap => "J.A.I. // DISK MAP",
            Self::Earth => "J.A.I. // EARTH",
            Self::Departures => "J.A.I. // DEPARTURES",
            Self::Live => "J.A.I. // LIVE",
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
async fn markdown_window(ctx: &ToolContext, view: JaiSubWindow, markdown: String, args: &Value, size: (u32, u32)) -> Result<String, String> {
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
    let outcome = Box::pin(ctx.execute("spawn_native_window", &spec)).await;
    if outcome.success {
        Ok(outcome.output)
    } else {
        Err(outcome.output.trim_start_matches("Error: ").to_string())
    }
}

/// The views JAI opens of his own accord, as he names them: each stands on its own and changes
/// nothing (the disk map scans a whole drive and the QR code shows his link, so not those).
const OWN_CHOICE: [(&str, &str); 7] = [
    ("earth", "the Earth"),
    ("system_stats", "the system telemetry"),
    ("network_radar", "the network radar"),
    ("simulation_lab", "the simulation lab"),
    ("schedule", "your schedule"),
    ("contacts", "your contacts"),
    ("workout_finance", "the finance tracker"),
];

/// His choice: the views in an order `seed` decides, and how many of them to open (as said,
/// else two to four).
fn own_choice(seed: u64, said: Option<u64>) -> (Vec<usize>, usize) {
    let mut state = seed | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let count = said.unwrap_or(2 + next() % 3).clamp(1, OWN_CHOICE.len() as u64) as usize;
    let mut order: Vec<usize> = (0..OWN_CHOICE.len()).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, (next() % (i as u64 + 1)) as usize);
    }
    (order, count)
}

/// "Open a few random windows": he decides how many and which (a view that will not open gives
/// way to the next one).
async fn open_own_choice(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let said = args.get("count").and_then(|c| c.as_u64().or_else(|| c.as_str()?.trim().parse().ok()));
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
    let (order, count) = own_choice(seed, said);
    let mut opened = Vec::new();
    for (id, name) in order.into_iter().map(|i| OWN_CHOICE[i]) {
        if opened.len() == count {
            break;
        }
        let mut one = json!({ "view": id });
        if let Some(m) = args.get("monitor") {
            one["monitor"] = m.clone();
        }
        if Box::pin(ctx.execute("open_jai_view", &one)).await.success {
            opened.push(name);
        }
    }
    let list = match opened.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        Some((only, _)) => only.to_string(),
        None => return Err("none of my views would open".into()),
    };
    let mut number = crate::hud_board::number_words(opened.len());
    number[..1].make_ascii_uppercase();
    let spoken = if said.is_some_and(|n| n as usize > opened.len()) {
        format!("That is every view I could open, Sir: {list}.")
    } else {
        format!("{number} of my choosing, Sir: {list}.")
    };
    Ok(format!("Opened: {list}.\nSpoken: {spoken}"))
}

/// `open_jai_view`: the view as a HUD block (or its own window). The result ends with a
/// "Spoken:" line.
pub async fn run(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let raw = str_arg(args, &["view", "window_kind", "name", "target"]).unwrap_or("system stats");
    if matches!(raw.trim().to_lowercase().as_str(), "random" | "surprise" | "any") {
        if !crate::gui::windows_supported() {
            return Err("JAI is running without a display (headless), so he cannot open windows.".into());
        }
        return open_own_choice(ctx, args).await;
    }
    let view = JaiSubWindow::parse(raw)
        .ok_or_else(|| format!("'{raw}' is not one of my views (system stats, schedule, simulation lab, mobile QR, gym and finances, contacts, network radar, disk map, earth)."))?;
    // (live layers (C1): view `live` answers in words and needs no display.)
    if view != JaiSubWindow::Live && !crate::gui::windows_supported() {
        return Err("JAI is running without a display (headless), so he cannot open windows.".into());
    }
    let on = on_monitor(args);
    match view {
        // live layers (C1): satellite passes, road traffic, another day's weather, rain soon.
        JaiSubWindow::Live => crate::live_layers::run_view(ctx, args).await,
        // The panels with their own state and actions (sweep, scan, globe): theirs to carry out.
        JaiSubWindow::NetworkRadar | JaiSubWindow::DiskMap | JaiSubWindow::Earth | JaiSubWindow::Departures => {
            // Asked for again while it was leaving: it stays.
            crate::hud_dock::cancel_close(view.id());
            match view {
                JaiSubWindow::NetworkRadar => crate::network_radar::run(args).await,
                JaiSubWindow::DiskMap => crate::disk_map::run(args).await,
                JaiSubWindow::Departures => crate::transit::run(ctx, args).await, // live layers (C1)
                _ => crate::earth::run(args).await,
            }
        }
        JaiSubWindow::SystemStatsMonitor => {
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
        JaiSubWindow::ScheduleAndCalendar => {
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
        JaiSubWindow::SimulationLab => {
            let kind = str_arg(args, &["sim_type"]).unwrap_or("projectile");
            let outcome = Box::pin(ctx.execute("launch_interactive_simulation", &json!({ "sim_type": kind, "title": "Simulation Lab" }))).await;
            if !outcome.success {
                return Err(outcome.output.trim_start_matches("Error: ").to_string());
            }
            Ok(format!("{}\nSpoken: The Simulation Lab is open{on}, Sir. Drag the sliders, or just tell me what to change.", outcome.output))
        }
        JaiSubWindow::MobileSatelliteQr => {
            let outcome = Box::pin(ctx.execute("show_mobile_satellite_instructions", &json!({}))).await;
            if !outcome.success {
                return Err(outcome.output.trim_start_matches("Error: ").to_string());
            }
            // The satellite tool opens the pairing window (QR code + link) itself.
            let display = display_part(&outcome.output);
            Ok(format!("{display}\nSpoken: Your pairing QR code is up{on}, Sir. Scan it with your phone's camera."))
        }
        JaiSubWindow::WorkoutAndFinance => {
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
        JaiSubWindow::Contacts => {
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

/// The panels that carry their own state (network radar, disk map, Earth): id, title, whether it
/// is open, and how it closes.
const PANELS: [(&str, &str, fn() -> bool, fn()); 8] = [
    ("network_radar", "Network radar", crate::network_radar::is_open, crate::network_radar::close),
    ("disk_map", "Disk map", crate::disk_map::is_open, crate::disk_map::close),
    ("earth", "Earth", crate::earth::is_open, crate::earth::close),
    ("departures", "Departures", crate::transit::is_open, crate::transit::close), // live layers (C1)
    // Pictures, live cameras, street view.
    ("image_viewer", "Pictures", crate::image_viewer::is_open, crate::image_viewer::close),
    ("webcams", "Live cameras", crate::webcams::is_open, crate::webcams::close),
    ("street_view", "Street view", crate::street_view::is_open, crate::street_view::close),
    ("model3d", "3D model", crate::model3d::is_open, crate::model3d::close),
];

/// Whether `id` is one of the panels with their own state (network radar, disk map, Earth).
pub fn is_panel(id: &str) -> bool {
    PANELS.iter().any(|(panel, ..)| *panel == id)
}

/// Whether panel `id` is up, as an in-app block or in its own OS window.
pub fn panel_is_open(id: &str) -> bool {
    PANELS.iter().any(|(panel, _, open, _)| *panel == id && open()) || crate::hud_dock::is_open(id)
}

/// "Close the globe" while the Earth has its own OS window (an in-app block fades out through
/// `hud_dock::close_named` instead): the panel the name means closes; its title, if one did.
pub fn close_panel(name: &str) -> Option<&'static str> {
    let prefix = crate::hud_dock::id_prefix_for(name)?;
    let (_, title, open, close) = PANELS.iter().find(|(id, ..)| *id == prefix)?;
    open().then(|| {
        close();
        crate::gui::refresh();
        *title
    })
}

/// "Close the windows you opened": the panels in their own OS windows close (the in-app ones
/// fade out with the other blocks). How many closed.
pub fn close_detached_panels() -> usize {
    let mut closed = 0;
    for (_, _, open, close) in PANELS {
        if open() && crate::hud_dock::all_own() {
            close();
            closed += 1;
        }
    }
    if closed > 0 {
        crate::gui::refresh();
    }
    closed
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "open_jai_view",
        "description": "Open one of YOUR OWN views as its own movable window (never an installed program): system stats / telemetry, schedule (WebUntis + calendar), simulation lab, mobile QR code, gym & finance tracker, contacts, network radar (\"who is on my network\"), disk map (\"what is taking up space\"), earth (globe: quakes, storms, satellites, flights, trains, traffic; satellite view; where Sir is; terrain; rain radar and clouds; past days; a world tour), departures (next trains), live (spoken answers, no window), random (\"a few random windows\": your own pick; count only if Sir said one). \"Open my system stats in a new window\" → view system_stats.",
        "parameters": {"type": "object", "properties": {
            "view": {"type": "string", "enum": ["system_stats", "schedule", "simulation_lab", "mobile_qr", "workout_finance", "contacts", "network_radar", "disk_map", "earth", "departures", "live", "random"]},
            "count": {"type": "integer"},
            "action": {"type": "string", "description": "network_radar: show, rescan, check_security. disk_map: show (path), rescan, back. earth: show, show_satellite, show_holo, show_place (place), show_quakes (place, radius_km), show_flights (place), show_storms, show_fires, show_iss, show_satellites (name: group), find_satellite (name), show_overhead, show_trains (place), show_traffic (name or place), show_my_location (where am I), show_mountains (place optional), exaggerate_terrain, show_radar, animate_radar, show_clouds (date optional), show_labels / hide_labels, show_buildings (place optional: the houses in 3D), set_imagery (imagery), show_date (date, place optional: a past day's imagery), show_today, show_world_tour, stop_tour, hide_layer (layer), hide_layers, zoom_in, zoom_out, home, spin, spin_reverse (the other direction), stop_spin. departures: show (name). live: passes (name, place), traffic (name or place), weather_on (date, place), rain_soon (place)."},
            "layer": {"type": "string", "description": "earth, hide_layer: quakes, flights, storms, fires, iss, satellites, trains, traffic, radar, clouds, terrain, labels, buildings"},
            "name": {"type": "string", "description": "satellite, group (starlink), station or motorway (A8)"},
            "date": {"type": "string", "description": "a day as said (\"yesterday\", \"1 March 2024\", 2024-03-01)"},
            "imagery": {"type": "string", "description": "earth, set_imagery: esri, sentinel, nasa"},
            "path": {"type": "string", "description": "disk_map: the drive or folder to map, e.g. C:\\ or D:\\Games"},
            "place": {"type": "string", "description": "earth: where to fly to or search around (city, country, landmark; \"here\" = the point in view)"},
            "radius_km": {"type": "number", "description": "earth: radius of an area search (default 500)"},
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
            ("system stats", JaiSubWindow::SystemStatsMonitor),
            ("system_stats", JaiSubWindow::SystemStatsMonitor),
            ("my telemetry", JaiSubWindow::SystemStatsMonitor),
            ("webuntis", JaiSubWindow::ScheduleAndCalendar),
            ("the calendar", JaiSubWindow::ScheduleAndCalendar),
            ("simulation lab", JaiSubWindow::SimulationLab),
            ("QR code", JaiSubWindow::MobileSatelliteQr),
            ("mobile_qr", JaiSubWindow::MobileSatelliteQr),
            ("gym logs", JaiSubWindow::WorkoutAndFinance),
            ("finance tracker", JaiSubWindow::WorkoutAndFinance),
            ("contacts", JaiSubWindow::Contacts),
            ("network_radar", JaiSubWindow::NetworkRadar),
            ("the network radar", JaiSubWindow::NetworkRadar),
            ("disk_map", JaiSubWindow::DiskMap),
            ("drive map", JaiSubWindow::DiskMap),
            ("earth", JaiSubWindow::Earth),
            ("the globe", JaiSubWindow::Earth),
            ("satellite view", JaiSubWindow::Earth),
            ("satellite", JaiSubWindow::MobileSatelliteQr),
            ("departures", JaiSubWindow::Departures),
            ("departure board", JaiSubWindow::Departures),
            ("abfahrten", JaiSubWindow::Departures),
            ("live", JaiSubWindow::Live),
        ] {
            assert_eq!(JaiSubWindow::parse(name), Some(view), "{name}");
        }
        assert_eq!(JaiSubWindow::parse("discord"), None);
    }

    /// "A few random windows": two to four different views unless a number is said, never more
    /// than there are, and another pick the next time.
    #[test]
    fn his_own_choice_of_views() {
        let mut orders = std::collections::HashSet::new();
        for seed in 1..200u64 {
            let (order, count) = own_choice(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), None);
            assert!((2..=4).contains(&count), "{count}");
            let mut seen = order.clone();
            seen.sort_unstable();
            assert_eq!(seen, (0..OWN_CHOICE.len()).collect::<Vec<_>>(), "every view once");
            orders.insert((order, count));
        }
        assert!(orders.len() > 100, "the choice varies: {}", orders.len());
        assert_eq!(own_choice(7, Some(3)).1, 3);
        assert_eq!(own_choice(7, Some(0)).1, 1);
        assert_eq!(own_choice(7, Some(40)).1, OWN_CHOICE.len());
        // Every view he may choose is one of his, and none of them is the heavy or private one.
        for (id, _) in OWN_CHOICE {
            let view = JaiSubWindow::parse(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(view.id(), id);
            assert!(!matches!(view, JaiSubWindow::DiskMap | JaiSubWindow::MobileSatelliteQr | JaiSubWindow::Live | JaiSubWindow::Departures));
        }
    }

    #[test]
    fn spoken_monitor_suffix() {
        assert_eq!(on_monitor(&json!({ "monitor": 2 })), " on Monitor 2");
        assert_eq!(on_monitor(&json!({ "monitor": "2" })), " on Monitor 2");
        assert_eq!(on_monitor(&json!({ "monitor": "left" })), " on the left monitor");
        assert_eq!(on_monitor(&json!({})), "");
    }
}
