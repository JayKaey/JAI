//! "Where am I?" on the Earth (`earth::Action::Locate`): Sir's position for the "YOU" marker.
//!
//! In order: the Windows location service (Wi-Fi, GPS where there is one; nothing happens when
//! Sir has location access switched off), then a keyless IP lookup (ipwho.is: only his IP address
//! goes there, never a position), then the home saved in the settings. The fix stays on this PC:
//! the Earth draws it, and nothing but the map tiles needed to draw the view around it is asked
//! for (`earth` keeps its place and weather lookups away from it).
//!
//! Dry runs, the verifier and the tests never ask anyone: they get the demo's position.

use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// How long the Windows location service may take.
const DEVICE_TIMEOUT: Duration = Duration::from_secs(7);
/// A fix is used again for this long (a device fix less long: Sir may be on the move).
const DEVICE_TTL: Duration = Duration::from_secs(120);
const OTHER_TTL: Duration = Duration::from_secs(1800);
/// ipwho.is gives no accuracy: a city's size.
const NETWORK_ACCURACY_M: f64 = 25_000.0;
const HOME_ACCURACY_M: f64 = 3_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The Windows location service.
    Device,
    /// The internet connection's address (ipwho.is).
    Network,
    /// The home in the settings.
    Home,
    /// The verifier's and the tests' demo.
    Demo,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Device => if cfg!(windows) { "Windows location" } else { "GeoClue location" },
            Source::Network => "network location",
            Source::Home => "home in the settings",
            Source::Demo => "demo",
        }
    }
}

/// Where Sir is.
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// The radius he is within, metres.
    pub accuracy_m: f64,
    pub source: Source,
    /// "Lyon, France" when the source named it (the IP lookup does; nothing is asked to).
    pub label: Option<String>,
}

impl Fix {
    /// The camera's altitude to show him with his accuracy circle (street level for a GPS fix).
    pub fn altitude_km(&self) -> f64 {
        (self.accuracy_m / 1000.0 * 9.0).clamp(1.2, 900.0)
    }

    /// "48.7758, 9.1829 (±20 m, Windows location)".
    pub fn display(&self) -> String {
        format!("You: {} ({}, {})", crate::earth::coords_label(self.lat, self.lon), accuracy_label(self.accuracy_m), self.source.label())
    }

    /// What JAI says as the camera flies there.
    pub fn spoken(&self) -> String {
        let within = spoken_accuracy(self.accuracy_m);
        match (self.source, &self.label) {
            (Source::Device, _) => format!("There you are, Sir: the {} location service has you to within {within}.", if cfg!(windows) { "Windows" } else { "system" }),
            (Source::Network, Some(place)) => format!("There you are, Sir: around {place}, going by your internet connection, so only to within {within}."),
            (Source::Network, None) => format!("There you are, Sir, going by your internet connection: to within {within}."),
            (Source::Home, _) => "There you are, Sir: at the home location in your settings. The live location service didn't answer.".to_string(),
            (Source::Demo, _) => "There you are, Sir.".to_string(),
        }
    }
}

/// "±20 m", "±25 km".
pub fn accuracy_label(m: f64) -> String {
    if m < 1000.0 {
        format!("\u{b1}{} m", m.max(1.0).round() as i64)
    } else {
        format!("\u{b1}{} km", (m / 1000.0).round() as i64)
    }
}

/// "about 20 metres", "about 25 kilometres".
pub fn spoken_accuracy(m: f64) -> String {
    if m < 1000.0 {
        let rounded = if m < 100.0 { (m / 5.0).round().max(1.0) * 5.0 } else { (m / 50.0).round() * 50.0 };
        format!("about {} metres", rounded as i64)
    } else {
        let km = (m / 1000.0).round() as i64;
        if km == 1 { "about a kilometre".to_string() } else { format!("about {km} kilometres") }
    }
}

/// The demo's Sir: in New York, where the demo's marker is.
pub fn demo_fix() -> Fix {
    Fix { lat: 40.7128, lon: -74.006, accuracy_m: 12_000.0, source: Source::Demo, label: Some("New York".into()) }
}

static LAST: Mutex<Option<(Fix, Instant)>> = Mutex::new(None);

/// The last fix while it is fresh.
pub fn cached() -> Option<Fix> {
    let last = crate::crash_guard::lock(&LAST);
    last.as_ref().filter(|(fix, at)| at.elapsed() < if fix.source == Source::Device { DEVICE_TTL } else { OTHER_TTL }).map(|(fix, _)| fix.clone())
}

/// Sir's position: the location service, his connection's, or his home's.
pub async fn locate(offline: bool) -> Result<Fix, String> {
    if offline {
        return Ok(demo_fix());
    }
    if let Some(fix) = cached() {
        return Ok(fix);
    }
    let fix = match device().await {
        Ok(fix) => fix,
        Err(e) => {
            debug!("[EARTH] No fix from the Windows location service: {e}");
            match network().await {
                Ok(fix) => fix,
                Err(e2) => {
                    debug!("[EARTH] No network location either: {e2}");
                    home().ok_or_else(|| format!("no location: {e}; {e2}"))?
                }
            }
        }
    };
    info!("[EARTH] Located Sir ({}, {})", fix.source.label(), accuracy_label(fix.accuracy_m));
    *crate::crash_guard::lock(&LAST) = Some((fix.clone(), Instant::now()));
    Ok(fix)
}

static NO_DEVICE: Mutex<Option<Instant>> = Mutex::new(None);

/// The Windows location service's fix alone (the last one while fresh). None while it is off
/// or gave nothing in the last ten minutes: no question waits on it twice.
pub async fn device_fix() -> Option<Fix> {
    if let Some(fix) = cached().filter(|f| f.source == Source::Device) {
        return Some(fix);
    }
    if crate::crash_guard::lock(&NO_DEVICE).is_some_and(|at| at.elapsed() < Duration::from_secs(600)) {
        return None;
    }
    match device().await {
        Ok(fix) => {
            *crate::crash_guard::lock(&LAST) = Some((fix.clone(), Instant::now()));
            Some(fix)
        }
        Err(e) => {
            debug!("[EARTH] No fix from the Windows location service: {e}");
            *crate::crash_guard::lock(&NO_DEVICE) = Some(Instant::now());
            None
        }
    }
}

/// The Windows location service, on a thread of its own (WinRT wants its apartment), waited for
/// `DEVICE_TIMEOUT` at most.
async fn device() -> Result<Fix, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("jai-geolocation".into())
        .spawn(move || {
            let _ = tx.send(device_position(DEVICE_TIMEOUT));
        })
        .map_err(|e| e.to_string())?;
    let waited = tokio::task::spawn_blocking(move || rx.recv_timeout(DEVICE_TIMEOUT + Duration::from_secs(2))).await.map_err(|e| e.to_string())?;
    let (lat, lon, accuracy_m) = waited.map_err(|_| "the location service took too long".to_string())??;
    if !(lat.is_finite() && lon.is_finite()) || (lat == 0.0 && lon == 0.0) {
        return Err("the location service gave no position".into());
    }
    Ok(Fix { lat, lon, accuracy_m: accuracy_m.max(1.0), source: Source::Device, label: None })
}

#[cfg(windows)]
fn device_position(timeout: Duration) -> Result<(f64, f64, f64), String> {
    use windows::Devices::Geolocation::{GeolocationAccessStatus, Geolocator, PositionAccuracy};
    use windows::Foundation::TimeSpan;
    // SAFETY: once, at the start of this thread of our own, before any WinRT call on it.
    let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
    let access = Geolocator::RequestAccessAsync().and_then(|op| op.get()).map_err(|e| e.to_string())?;
    if access != GeolocationAccessStatus::Allowed {
        return Err("location access is switched off".into());
    }
    let locator = Geolocator::new().map_err(|e| e.to_string())?;
    let _ = locator.SetDesiredAccuracy(PositionAccuracy::High);
    let ticks = |d: Duration| TimeSpan { Duration: d.as_millis() as i64 * 10_000 };
    let position = locator.GetGeopositionAsyncWithAgeAndTimeout(ticks(Duration::from_secs(60)), ticks(timeout)).and_then(|op| op.get()).map_err(|e| e.to_string())?;
    let coordinate = position.Coordinate().map_err(|e| e.to_string())?;
    let point = coordinate.Point().and_then(|p| p.Position()).map_err(|e| e.to_string())?;
    let accuracy = coordinate.Accuracy().unwrap_or(1000.0);
    Ok((point.Latitude, point.Longitude, accuracy))
}

/// GeoClue2 on the system bus (KDE and GNOME both run it): a client of its own, started, then the
/// location it settles on. It refuses a program the system has not allowed (geoclue.conf).
#[cfg(not(windows))]
fn device_position(timeout: Duration) -> Result<(f64, f64, f64), String> {
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{ObjectPath, OwnedObjectPath};
    const SERVICE: &str = "org.freedesktop.GeoClue2";
    let bus = Connection::system().map_err(|e| format!("no system bus: {e}"))?;
    let manager = Proxy::new(&bus, SERVICE, "/org/freedesktop/GeoClue2/Manager", "org.freedesktop.GeoClue2.Manager").map_err(|e| e.to_string())?;
    let client_path: OwnedObjectPath = manager.call("GetClient", &()).map_err(|e| format!("no location service on this system: {e}"))?;
    let client = Proxy::new(&bus, SERVICE, client_path.as_ref(), "org.freedesktop.GeoClue2.Client").map_err(|e| e.to_string())?;
    // 8 = exact; the desktop id names this program to the system's list of allowed ones.
    client.set_property("DesktopId", "jai").map_err(|e| e.to_string())?;
    client.set_property("RequestedAccuracyLevel", 8u32).map_err(|e| e.to_string())?;
    client.call::<_, _, ()>("Start", &()).map_err(|e| format!("location access is switched off: {e}"))?;
    let deadline = std::time::Instant::now() + timeout;
    let found = loop {
        let place: OwnedObjectPath = client.get_property("Location").map_err(|e| e.to_string())?;
        if place.as_str() != "/" {
            break place;
        }
        if std::time::Instant::now() >= deadline {
            let _ = client.call::<_, _, ()>("Stop", &());
            return Err("the location service gave no position in time".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let _ = client.call::<_, _, ()>("Stop", &());
    let location = Proxy::new(&bus, SERVICE, ObjectPath::from(found.as_ref()), "org.freedesktop.GeoClue2.Location").map_err(|e| e.to_string())?;
    let value = |name: &str| location.get_property::<f64>(name).map_err(|e| e.to_string());
    Ok((value("Latitude")?, value("Longitude")?, value("Accuracy")?))
}

/// ipwho.is (keyless, HTTPS): the connection's city.
async fn network() -> Result<Fix, String> {
    let v = crate::earth::get_json(crate::earth_sources::IP_LOCATION_URL, Duration::from_secs(6)).await?;
    let (lat, lon, label) = crate::earth_sources::parse_ipwho(&v).ok_or("the IP lookup gave no position")?;
    Ok(Fix { lat, lon, accuracy_m: NETWORK_ACCURACY_M, source: Source::Network, label })
}

/// The home saved in the settings (`location.home_lat`, `location.home_lon`).
fn home() -> Option<Fix> {
    let raw = std::fs::read_to_string(crate::config::CONFIG_FILE).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    home_of(&v)
}

fn home_of(v: &Value) -> Option<Fix> {
    let (lat, lon) = (v["location"]["home_lat"].as_f64()?, v["location"]["home_lon"].as_f64()?);
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) && !(lat == 0.0 && lon == 0.0)).then(|| Fix { lat, lon, accuracy_m: HOME_ACCURACY_M, source: Source::Home, label: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fixes_are_shown_and_said_by_their_source() {
        let device = Fix { lat: 45.7578, lon: 4.8320, accuracy_m: 18.0, source: Source::Device, label: None };
        assert_eq!(device.display(), "You: 45.7578, 4.8320 (\u{b1}18 m, Windows location)");
        assert_eq!(device.spoken(), "There you are, Sir: the Windows location service has you to within about 20 metres.");
        assert!(device.altitude_km() < 2.0, "street level for a precise fix");
        let net = Fix { lat: 45.76, lon: 4.84, accuracy_m: NETWORK_ACCURACY_M, source: Source::Network, label: Some("Lyon, France".into()) };
        assert_eq!(net.spoken(), "There you are, Sir: around Lyon, France, going by your internet connection, so only to within about 25 kilometres.");
        assert!(net.altitude_km() > 150.0 && net.altitude_km() <= 900.0);
        assert_eq!(accuracy_label(25_000.0), "\u{b1}25 km");
        assert_eq!(spoken_accuracy(1_200.0), "about a kilometre");
        assert_eq!(spoken_accuracy(430.0), "about 450 metres");
    }

    #[test]
    fn the_home_in_the_settings_and_the_demo() {
        let fix = home_of(&json!({"location": {"home_lat": 45.8, "home_lon": 4.8}})).unwrap();
        assert_eq!((fix.lat, fix.lon, fix.source), (45.8, 4.8, Source::Home));
        assert!(home_of(&json!({"location": {"home_lat": 0.0, "home_lon": 0.0}})).is_none());
        assert!(home_of(&json!({"location": {}})).is_none());
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        // Offline (the tests always are) nothing is asked: the demo's position.
        assert_eq!(rt.block_on(locate(true)), Ok(demo_fix()));
    }
}
