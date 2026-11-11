//! The Earth's sources (`crate::earth`): where its imagery, overlays, terrain, weather, peaks and
//! Sir's rough position come from, and the pixel work on what they send. Everything here is pure
//! and unit-tested; the fetching lives in `earth`.
//!
//! * Imagery to street level, keyless: Esri World Imagery (z19), EOX Sentinel-2 cloudless (z15)
//!   and NASA GIBS (Blue Marble, z8; daily VIIRS true colour by date, z9).
//! * Labels and borders: Esri "World Boundaries and Places" (transparent PNG).
//! * Terrain: AWS Terrain Tiles (Terrarium elevation PNGs) shaded here on the fly; peaks from
//!   OpenStreetMap through the Overpass API.
//! * Weather: RainViewer radar (the past ~2 h in 10-minute frames, z7 at most); infrared cloud
//!   tops from the GIBS geostationary layers (GOES-East, GOES-West, Himawari). RainViewer's own
//!   infrared list has been empty since its 2025 API change.
//! * Position: ipwho.is (keyless HTTPS) as the fallback behind the Windows location service.

use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use serde_json::Value;
use std::f64::consts::PI;

// ------------------------------------------------------------------------------ imagery ----

/// The photo layer of the map (and of the globe up close).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Imagery {
    /// Esri World Imagery: Maxar and others, to street level.
    Esri,
    /// EOX Sentinel-2 cloudless (2020): 10 m, the same season everywhere.
    Sentinel,
    /// NASA Blue Marble Next Generation (500 m).
    Nasa,
}

impl Imagery {
    #[cfg(test)]
    pub const ALL: [Imagery; 3] = [Imagery::Esri, Imagery::Sentinel, Imagery::Nasa];

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_lowercase();
        Some(if s.contains("esri") || s.contains("arcgis") || s.contains("world imagery") || s == "default" || s.contains("street") || s.contains("high") {
            Self::Esri
        } else if s.contains("sentinel") || s.contains("eox") || s.contains("cloudless") || s.contains("copernicus") {
            Self::Sentinel
        } else if s.contains("nasa") || s.contains("gibs") || s.contains("blue marble") || s.contains("bluemarble") {
            Self::Nasa
        } else {
            return None;
        })
    }

    pub fn max_level(self) -> u8 {
        match self {
            Self::Esri => 19,
            Self::Sentinel => 15,
            Self::Nasa => 8,
        }
    }

    /// The tile's URL (x east, y south, Web Mercator).
    pub fn url(self, z: u8, x: u32, y: u32) -> String {
        match self {
            Self::Esri => format!("https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}"),
            Self::Sentinel => format!("https://tiles.maps.eox.at/wmts/1.0.0/s2cloudless-2020_3857/default/g/{z}/{y}/{x}.jpg"),
            Self::Nasa => gibs_tile_url("BlueMarble_NextGeneration", None, 8, "jpeg", z, x, y),
        }
    }

    /// For the credits line.
    pub fn credit(self) -> &'static str {
        match self {
            Self::Esri => "Imagery: Esri, Maxar, Earthstar Geographics",
            Self::Sentinel => "Imagery: Sentinel-2 cloudless by EOX (Copernicus Sentinel data 2020)",
            Self::Nasa => "Imagery: NASA Blue Marble",
        }
    }

    /// For JAI.
    pub fn spoken(self) -> &'static str {
        match self {
            Self::Esri => "the high-resolution Esri imagery",
            Self::Sentinel => "the Sentinel-2 cloudless imagery",
            Self::Nasa => "NASA's Blue Marble",
        }
    }

}

/// A GIBS WMTS tile (Web Mercator): `time` None is the layer's default (its latest).
pub fn gibs_tile_url(layer: &str, time: Option<&str>, matrix_level: u8, ext: &str, z: u8, x: u32, y: u32) -> String {
    format!("https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/{layer}/default/{}/GoogleMapsCompatible_Level{matrix_level}/{z}/{y}/{x}.{ext}", time.unwrap_or("default"))
}

/// The daily true-colour mosaic (clouds and all) of a day.
pub const DAILY_LAYER: &str = "VIIRS_SNPP_CorrectedReflectance_TrueColor";
pub const DAILY_MAX_LEVEL: u8 = 9;
/// The lights of the night side.
pub const NIGHT_LAYER: &str = "VIIRS_Black_Marble";
/// The photo globe by day.
pub const DAY_LAYER: &str = "BlueMarble_NextGeneration";
/// Relief and sea depth: the holographic mask is classified from it, and it is the terrain globe.
pub const RELIEF_LAYER: &str = "BlueMarble_ShadedRelief_Bathymetry";
/// Borders and coastlines as thin lines (transparent PNG).
pub const LINES_LAYER: &str = "Reference_Features_15m";

/// Labels and borders over the imagery (transparent PNG).
pub fn reference_url(z: u8, x: u32, y: u32) -> String {
    format!("https://server.arcgisonline.com/ArcGIS/rest/services/Reference/World_Boundaries_and_Places/MapServer/tile/{z}/{y}/{x}")
}
pub const REFERENCE_MAX_LEVEL: u8 = 19;
pub const REFERENCE_CREDIT: &str = "Labels: Esri";

// ------------------------------------------------------------------------------ terrain ----

/// AWS Terrain Tiles: elevation encoded in the colour (Terrarium).
pub fn terrarium_url(z: u8, x: u32, y: u32) -> String {
    format!("https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png")
}
pub const TERRAIN_MAX_LEVEL: u8 = 15;
pub const TERRAIN_CREDIT: &str = "Terrain: Mapzen / AWS Terrain Tiles";

/// Metres above sea level of a Terrarium pixel.
pub fn terrarium_metres([r, g, b]: [u8; 3]) -> f32 {
    r as f32 * 256.0 + g as f32 + b as f32 / 256.0 - 32_768.0
}

/// Ground metres per pixel of a 256 px Web Mercator tile at level `z` and latitude `lat`.
pub fn metres_per_px(z: u8, lat: f64) -> f64 {
    2.0 * PI * 6_378_137.0 * lat.to_radians().cos().abs().max(1e-6) / (256.0 * (1u64 << z) as f64)
}

/// The light on a height field from the north-west, 45 degrees up: 0 (in shadow) to 1 (facing
/// the light); flat ground is `cos(45°)` ≈ 0.71. `exaggeration` steepens the slopes.
pub fn hillshade(heights: &[f32], w: usize, h: usize, metres_per_px: f64, exaggeration: f32) -> Vec<f32> {
    let at = |x: isize, y: isize| heights[(y.clamp(0, h as isize - 1) as usize) * w + x.clamp(0, w as isize - 1) as usize];
    // (The sun's compass azimuth 315° as a math angle: 360 - 315 + 90 = 135°.)
    let (zenith, azimuth) = (45f32.to_radians(), 135f32.to_radians());
    let scale = exaggeration / (8.0 * metres_per_px.max(0.01) as f32);
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h as isize {
        for x in 0..w as isize {
            // Horn's method.
            let dzdx = ((at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x - 1, y) + at(x - 1, y + 1))) * scale;
            let dzdy = ((at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x, y - 1) + at(x + 1, y - 1))) * scale;
            let slope = (dzdx * dzdx + dzdy * dzdy).sqrt().atan();
            let aspect = if dzdx != 0.0 || dzdy != 0.0 { dzdy.atan2(-dzdx) } else { 0.0 };
            // (The aspect is the math angle the ground falls away to.)
            let lit = zenith.cos() * slope.cos() + zenith.sin() * slope.sin() * (azimuth - aspect).cos();
            out.push(lit.clamp(0.0, 1.0));
        }
    }
    out
}

/// A shade as a premultiplied overlay pixel: darker than flat ground is black over the map,
/// brighter is white (an "over" blend of it multiplies and lifts the imagery).
pub fn shade_pixel(shade: f32, strength: f32) -> [u8; 4] {
    const FLAT: f32 = 0.707;
    if shade < FLAT {
        let a = ((FLAT - shade) / FLAT * strength).clamp(0.0, 0.85);
        [0, 0, 0, (a * 255.0).round() as u8]
    } else {
        let a = ((shade - FLAT) / (1.0 - FLAT) * strength * 0.45).clamp(0.0, 0.5);
        let v = (a * 255.0).round() as u8;
        [v, v, v, v]
    }
}

/// A Terrarium tile (RGBA bytes, 256 px) as a shaded-relief overlay (premultiplied RGBA bytes).
pub fn terrain_overlay(rgba: &[u8], w: usize, h: usize, z: u8, lat: f64, exaggeration: f32) -> Vec<u8> {
    let heights: Vec<f32> = rgba.chunks_exact(4).map(|p| terrarium_metres([p[0], p[1], p[2]]).max(0.0)).collect();
    let shades = hillshade(&heights, w, h, metres_per_px(z, lat), exaggeration);
    let strength = if exaggeration > 1.5 { 1.0 } else { 0.75 };
    shades.into_iter().flat_map(|s| shade_pixel(s, strength)).collect()
}

/// A peak in view.
#[derive(Debug, Clone, PartialEq)]
pub struct Peak {
    pub name: String,
    pub ele_m: f64,
    pub lat: f64,
    pub lon: f64,
}

/// The Overpass query for the named peaks with an elevation in a box (south, west, north, east),
/// of `min_m` metres and more (1000, 1500, 2000, 3000 or 4000; anything else means all):
/// Overpass cannot sort by height, so a wide box asks for the high ones only.
pub fn peaks_query_from(south: f64, west: f64, north: f64, east: f64, min_m: u32) -> String {
    let ele = match min_m {
        1000 => "[\"ele\"~\"^[1-8][0-9]{3}([.,][0-9]+)?$\"]",
        1500 => "[\"ele\"~\"^(1[5-9]|[2-8][0-9])[0-9]{2}([.,][0-9]+)?$\"]",
        2000 => "[\"ele\"~\"^[2-8][0-9]{3}([.,][0-9]+)?$\"]",
        3000 => "[\"ele\"~\"^[3-8][0-9]{3}([.,][0-9]+)?$\"]",
        4000 => "[\"ele\"~\"^[4-8][0-9]{3}([.,][0-9]+)?$\"]",
        _ => "[\"ele\"]",
    };
    format!("[out:json][timeout:20];node[\"natural\"=\"peak\"]{ele}[\"name\"]({south:.3},{west:.3},{north:.3},{east:.3});out body 400;")
}

/// How high a peak must be to be asked for in a box this many degrees across.
pub fn peaks_floor(span_deg: f64) -> u32 {
    if span_deg > 6.0 {
        3000
    } else if span_deg > 3.0 {
        2000
    } else if span_deg > 1.2 {
        1000
    } else {
        0
    }
}

/// The public Overpass servers, the main one first.
pub const OVERPASS_HOSTS: [&str; 2] = ["https://overpass-api.de/api/interpreter", "https://maps.mail.ru/osm/tools/overpass/api/interpreter"];
pub const PEAKS_CREDIT: &str = "Peaks: OpenStreetMap contributors";

pub fn overpass_url(host: &str, query: &str) -> String {
    format!("{host}?data={}", urlencoding::encode(query))
}

/// "2962", "2962 m", "2 962", "8848.86": metres.
fn elevation(s: &str) -> Option<f64> {
    let digits: String = s.trim().trim_end_matches(['m', 'M']).trim().replace([' ', '\u{a0}'], "").replace(',', ".");
    digits.parse::<f64>().ok().filter(|e| (-500.0..9_000.0).contains(e))
}

/// Overpass JSON: the peaks with a name and an elevation, highest first.
pub fn parse_peaks(v: &Value) -> Vec<Peak> {
    let Some(list) = v["elements"].as_array() else { return Vec::new() };
    let mut peaks: Vec<Peak> = list
        .iter()
        .filter_map(|e| {
            let tags = &e["tags"];
            let name = tags["name:en"].as_str().or_else(|| tags["name"].as_str())?.trim().to_string();
            (!name.is_empty()).then_some(())?;
            Some(Peak { name, ele_m: elevation(tags["ele"].as_str()?)?, lat: e["lat"].as_f64()?, lon: e["lon"].as_f64()? })
        })
        .collect();
    peaks.sort_by(|a, b| b.ele_m.total_cmp(&a.ele_m));
    peaks
}

// ------------------------------------------------------------------------------ weather ----

/// One RainViewer radar frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadarFrame {
    /// Unix seconds.
    pub time: i64,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RadarFrames {
    pub host: String,
    /// Oldest first; the last is the latest.
    pub frames: Vec<RadarFrame>,
}

pub const RAINVIEWER_URL: &str = "https://api.rainviewer.com/public/weather-maps.json";
/// RainViewer's tiles past z7 are a "zoom level not supported" picture.
pub const RADAR_MAX_LEVEL: u8 = 7;
pub const RADAR_CREDIT: &str = "Radar: RainViewer";

/// RainViewer's `weather-maps.json`: the past radar frames (the nowcast is not drawn).
pub fn parse_rainviewer(v: &Value) -> Option<RadarFrames> {
    let host = v["host"].as_str()?.trim_end_matches('/').to_string();
    let mut frames: Vec<RadarFrame> = v["radar"]["past"]
        .as_array()?
        .iter()
        .filter_map(|f| Some(RadarFrame { time: f["time"].as_i64()?, path: f["path"].as_str()?.to_string() }))
        .collect();
    frames.sort_by_key(|f| f.time);
    (!frames.is_empty()).then_some(RadarFrames { host, frames })
}

/// A radar tile: 256 px, colour scheme 2 ("Universal Blue"), smoothed, with snow.
pub fn radar_tile_url(host: &str, path: &str, z: u8, x: u32, y: u32) -> String {
    format!("{host}{path}/256/{z}/{x}/{y}/2/1_1.png")
}

/// The geostationary infrared layers (brighter is colder, i.e. higher cloud tops).
pub const IR_LAYERS: [&str; 3] = ["GOES-East_ABI_Band13_Clean_Infrared", "GOES-West_ABI_Band13_Clean_Infrared", "Himawari_AHI_Band13_Clean_Infrared"];
pub const IR_MAX_LEVEL: u8 = 6;
pub const IR_CREDIT: &str = "Infrared: NASA GIBS (GOES, Himawari)";

/// An infrared pixel (its grey) as cloud: white with an alpha that grows with the cloud's cold.
pub fn ir_cloud_pixel(grey: u8, alpha: u8) -> [u8; 4] {
    let g = grey as f32 / 255.0;
    let a = ((g - 0.42) / 0.38).clamp(0.0, 1.0).powf(0.8) * 0.92 * alpha as f32 / 255.0;
    let v = (a * 255.0).round() as u8;
    [v, v, v, v]
}

/// The three infrared tiles of one place (RGBA bytes, any of them missing) as one cloud overlay:
/// the coldest (highest) reading wins where the disks overlap.
pub fn merge_ir(tiles: &[Option<Vec<u8>>], pixels: usize) -> Vec<u8> {
    let mut out = vec![0u8; pixels * 4];
    for tile in tiles.iter().flatten() {
        for (i, px) in tile.chunks_exact(4).take(pixels).enumerate() {
            let cloud = ir_cloud_pixel(px[0], px[3]);
            if cloud[3] > out[i * 4 + 3] {
                out[i * 4..i * 4 + 4].copy_from_slice(&cloud);
            }
        }
    }
    out
}

// ----------------------------------------------------------------------------- position ----

pub const IP_LOCATION_URL: &str = "https://ipwho.is/?fields=success,latitude,longitude,city,region,country";

/// ipwho.is: (lat, lon, "City, Country").
pub fn parse_ipwho(v: &Value) -> Option<(f64, f64, Option<String>)> {
    if v["success"].as_bool() == Some(false) {
        return None;
    }
    let (lat, lon) = (v["latitude"].as_f64()?, v["longitude"].as_f64()?);
    let label = [v["city"].as_str(), v["country"].as_str()].into_iter().flatten().map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
    (lat.abs() <= 90.0 && lon.abs() <= 180.0).then(|| (lat, lon, (!label.is_empty()).then_some(label)))
}

// -------------------------------------------------------------------------------- dates ----

/// Month names in English and German, and their short forms.
const MONTHS: [(&str, u32); 37] = [
    ("january", 1), ("february", 2), ("march", 3), ("april", 4), ("may", 5), ("june", 6), ("july", 7), ("august", 8), ("september", 9), ("october", 10), ("november", 11), ("december", 12),
    ("januar", 1), ("jänner", 1), ("februar", 2), ("märz", 3), ("maerz", 3), ("mai", 5), ("juni", 6), ("juli", 7), ("oktober", 10), ("dezember", 12),
    ("jan", 1), ("feb", 2), ("mar", 3), ("mär", 3), ("apr", 4), ("jun", 6), ("jul", 7), ("aug", 8), ("sep", 9), ("sept", 9), ("oct", 10), ("okt", 10), ("nov", 11), ("dec", 12), ("dez", 12),
];

fn month_of(word: &str) -> Option<u32> {
    let w = word.trim_matches(|c: char| !c.is_alphabetic()).to_lowercase();
    MONTHS.iter().find(|(name, _)| *name == w).map(|(_, m)| *m)
}

fn day_of(word: &str) -> Option<u32> {
    let w = word.trim_end_matches(['.', ',']).to_lowercase();
    let digits = w.trim_end_matches("st").trim_end_matches("nd").trim_end_matches("rd").trim_end_matches("th");
    digits.parse::<u32>().ok().filter(|d| (1..=31).contains(d))
}

/// The day a sentence names: "on 1 March 2024", "March 1st, 2024", "2024-03-01", "1. März 2024",
/// "yesterday", "the day before yesterday", "3 days ago", "last week", "gestern", "vor 3 Tagen".
/// A day without a year is the last one that has been. None: no day, or one still to come.
pub fn parse_date(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let t = text.to_lowercase();
    let past = |d: NaiveDate| (d <= today).then_some(d);
    if t.contains("day before yesterday") || t.contains("vorgestern") {
        return Some(today - Duration::days(2));
    }
    if t.contains("yesterday") || t.contains("gestern") {
        return Some(today - Duration::days(1));
    }
    if t.contains("last week") || t.contains("a week ago") || t.contains("letzte woche") || t.contains("letzten woche") || t.contains("vor einer woche") {
        return Some(today - Duration::days(7));
    }
    let words: Vec<&str> = t.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).collect();
    for (i, w) in words.iter().enumerate() {
        // "3 days ago", "vor 3 tagen".
        if let Ok(n) = w.parse::<i64>() {
            let next = words.get(i + 1).copied().unwrap_or("");
            if (next.starts_with("day") && words.get(i + 2) == Some(&"ago")) || (next.starts_with("tag") && i > 0 && words[i - 1] == "vor") {
                return (1..=3650).contains(&n).then(|| today - Duration::days(n));
            }
        }
        // ISO "2024-03-01".
        if let Ok(d) = NaiveDate::parse_from_str(w.trim_end_matches(['.', '?', '!']), "%Y-%m-%d") {
            return past(d);
        }
    }
    // "1 march 2024", "1st of march", "1. märz 2024", "march 1st 2024".
    for (i, w) in words.iter().enumerate() {
        let Some(month) = month_of(w) else { continue };
        let before = i.checked_sub(1).and_then(|j| words.get(j)).and_then(|w| if *w == "of" { i.checked_sub(2).and_then(|j| words.get(j)) } else { Some(w) });
        let day = before.and_then(|w| day_of(w)).or_else(|| words.get(i + 1).and_then(|w| day_of(w)));
        let year = words[i + 1..].iter().take(3).find_map(|w| w.trim_end_matches(['.', '?', '!']).parse::<i32>().ok().filter(|y| (1990..=2100).contains(y)));
        let Some(day) = day else { continue };
        return match year {
            Some(y) => NaiveDate::from_ymd_opt(y, month, day).and_then(past),
            None => NaiveDate::from_ymd_opt(today.year(), month, day).and_then(|d| if d <= today { Some(d) } else { NaiveDate::from_ymd_opt(today.year() - 1, month, day) }),
        };
    }
    if t.contains("today") || t.contains("heute") {
        return Some(today);
    }
    None
}

/// "1 March 2024".
pub fn spoken_date(d: NaiveDate) -> String {
    d.format("%-d %B %Y").to_string()
}

// ---------------------------------------------------------------------------------- sun ----

/// Where the sun is overhead at `t` (degrees): its declination and the longitude where it is
/// local apparent noon (the NOAA low-precision formulas, within a few tenths of a degree).
pub fn subsolar_point(t: DateTime<Utc>) -> (f64, f64) {
    let n = t.timestamp() as f64 / 86_400.0 + 2_440_587.5 - 2_451_545.0;
    let l = (280.460 + 0.985_647_4 * n).rem_euclid(360.0);
    let g = (357.528 + 0.985_600_3 * n).rem_euclid(360.0).to_radians();
    let lambda = (l + 1.915 * g.sin() + 0.020 * (2.0 * g).sin()).to_radians();
    let eps = (23.439 - 0.000_000_4 * n).to_radians();
    let decl = (eps.sin() * lambda.sin()).asin();
    let ra = (eps.cos() * lambda.sin()).atan2(lambda.cos()).to_degrees();
    // The equation of time in degrees of the sun's travel (4 minutes each).
    let eot = ((l - ra + 540.0).rem_euclid(360.0)) - 180.0;
    let hours = t.hour() as f64 + t.minute() as f64 / 60.0 + t.second() as f64 / 3600.0;
    let lon = -15.0 * (hours - 12.0) - eot;
    (decl.to_degrees(), (lon + 180.0).rem_euclid(360.0) - 180.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn imagery_urls_and_names() {
        assert_eq!(Imagery::Esri.url(19, 281_452, 193_740), "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/19/193740/281452");
        assert_eq!(Imagery::Sentinel.url(8, 134, 90), "https://tiles.maps.eox.at/wmts/1.0.0/s2cloudless-2020_3857/default/g/8/90/134.jpg");
        assert!(Imagery::Nasa.url(3, 1, 2).ends_with("/BlueMarble_NextGeneration/default/default/GoogleMapsCompatible_Level8/3/2/1.jpeg"));
        assert_eq!(Imagery::parse("Sentinel-2"), Some(Imagery::Sentinel));
        assert_eq!(Imagery::parse("nasa blue marble"), Some(Imagery::Nasa));
        assert_eq!(Imagery::parse("esri"), Some(Imagery::Esri));
        assert_eq!(Imagery::parse("crayons"), None);
        assert!(Imagery::ALL.iter().all(|i| i.max_level() >= 8 && !i.credit().is_empty()));
        assert_eq!(reference_url(5, 16, 11), "https://server.arcgisonline.com/ArcGIS/rest/services/Reference/World_Boundaries_and_Places/MapServer/tile/5/11/16");
        assert_eq!(terrarium_url(8, 134, 90), "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/8/134/90.png");
        assert_eq!(
            gibs_tile_url(DAILY_LAYER, Some("2024-03-01"), 9, "jpg", 4, 8, 5),
            "https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/VIIRS_SNPP_CorrectedReflectance_TrueColor/default/2024-03-01/GoogleMapsCompatible_Level9/4/5/8.jpg"
        );
    }

    #[test]
    fn terrain_is_decoded_and_shaded() {
        // 0 m is (128, 0, 0); 8848 m is (162, 144, 0).
        assert_eq!(terrarium_metres([128, 0, 0]), 0.0);
        assert_eq!(terrarium_metres([162, 144, 0]), 8848.0);
        assert!((metres_per_px(0, 0.0) - 156_543.03).abs() < 0.1);
        assert!((metres_per_px(10, 60.0) - 76.437).abs() < 0.01);
        // Flat ground: the flat shade everywhere and no overlay at all.
        let flat = hillshade(&[100.0; 16], 4, 4, 30.0, 1.0);
        assert!(flat.iter().all(|s| (s - 0.7071).abs() < 1e-3));
        assert_eq!(shade_pixel(0.7071, 1.0), [0, 0, 0, 0]);
        // Ground rising to the east faces west, towards the north-western light: brighter.
        // Rising to the west it faces east, away from it: darker. The same north and south.
        let east: Vec<f32> = (0..16).map(|i| (i % 4) as f32 * 30.0).collect();
        let west: Vec<f32> = (0..16).map(|i| (3 - i % 4) as f32 * 30.0).collect();
        let (e, w) = (hillshade(&east, 4, 4, 30.0, 1.0)[5], hillshade(&west, 4, 4, 30.0, 1.0)[5]);
        assert!(e > 0.75 && w < 0.65, "rising east {e}, rising west {w}");
        let south: Vec<f32> = (0..16).map(|i| (i / 4) as f32 * 30.0).collect();
        let north: Vec<f32> = (0..16).map(|i| (3 - i / 4) as f32 * 30.0).collect();
        let (s, n) = (hillshade(&south, 4, 4, 30.0, 1.0)[5], hillshade(&north, 4, 4, 30.0, 1.0)[5]);
        assert!(s > 0.75 && n < 0.65, "rising south (facing north) {s}, rising north {n}");
        // Exaggerated relief: the dark side darker.
        assert!(hillshade(&west, 4, 4, 30.0, 2.5)[5] < w);
        assert_eq!(shade_pixel(0.0, 1.0)[0..3], [0, 0, 0]);
        let bright = shade_pixel(1.0, 1.0);
        assert!(bright[3] > 0 && bright[0] == bright[3], "premultiplied white: {bright:?}");
        let tile = terrain_overlay(&[128, 0, 0, 255].repeat(16), 4, 4, 10, 45.0, 1.0);
        assert_eq!(tile.len(), 64);
    }

    #[test]
    fn peaks_parse_highest_first() {
        let v = json!({"elements": [
            {"type": "node", "lat": 47.42, "lon": 10.98, "tags": {"natural": "peak", "name": "Zugspitze", "ele": "2962"}},
            {"type": "node", "lat": 47.5, "lon": 11.0, "tags": {"natural": "peak", "name": "Kleiner", "ele": "1 800 m"}},
            {"type": "node", "lat": 27.98, "lon": 86.92, "tags": {"natural": "peak", "name": "Sagarmāthā", "name:en": "Mount Everest", "ele": "8848.86"}},
            {"type": "node", "lat": 1.0, "lon": 1.0, "tags": {"natural": "peak", "name": "No height", "ele": "high"}},
            {"type": "node", "lat": 1.0, "lon": 1.0, "tags": {"natural": "peak", "ele": "100"}}
        ]});
        let peaks = parse_peaks(&v);
        assert_eq!(peaks.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["Mount Everest", "Zugspitze", "Kleiner"]);
        assert_eq!(peaks[1].ele_m, 2962.0);
        assert_eq!(peaks[2].ele_m, 1800.0);
        let q = peaks_query_from(47.2, 10.8, 47.6, 11.3, 0);
        assert!(q.contains("node[\"natural\"=\"peak\"][\"ele\"][\"name\"](47.200,10.800,47.600,11.300)"), "{q}");
        assert!(overpass_url(OVERPASS_HOSTS[0], &q).starts_with("https://overpass-api.de/api/interpreter?data=%5Bout%3Ajson%5D"));
        let high = peaks_query_from(45.0, 6.0, 48.0, 14.0, peaks_floor(8.0));
        assert!(high.contains("[\"ele\"~\"^[3-8][0-9]{3}([.,][0-9]+)?$\"][\"name\"](45.000,6.000,48.000,14.000)"), "{high}");
        assert_eq!((peaks_floor(0.8), peaks_floor(2.0), peaks_floor(4.0)), (0, 1000, 2000));
    }

    #[test]
    fn rainviewer_frames_and_infrared_clouds() {
        let v = json!({"version": "2.0", "generated": 1_794_178_523, "host": "https://tilecache.rainviewer.com",
            "radar": {"past": [{"time": 1_794_178_200, "path": "/v2/radar/2c5"}, {"time": 1_794_171_000, "path": "/v2/radar/37c"}], "nowcast": []},
            "satellite": {"infrared": []}});
        let frames = parse_rainviewer(&v).unwrap();
        assert_eq!(frames.frames.len(), 2);
        assert_eq!(frames.frames[0].time, 1_794_171_000, "oldest first");
        assert_eq!(radar_tile_url(&frames.host, &frames.frames[1].path, 3, 4, 2), "https://tilecache.rainviewer.com/v2/radar/2c5/256/3/4/2/2/1_1.png");
        assert!(parse_rainviewer(&json!({"host": "x", "radar": {"past": []}})).is_none());
        // Cold (bright) cloud tops are clouds; warm ground is clear.
        assert_eq!(ir_cloud_pixel(90, 255)[3], 0);
        assert!(ir_cloud_pixel(230, 255)[3] > 180);
        assert_eq!(ir_cloud_pixel(230, 0)[3], 0, "outside the satellite's disk");
        let a = Some([230u8, 230, 230, 255].repeat(2));
        let b = Some([[90u8, 90, 90, 255], [255, 255, 255, 255]].concat());
        let merged = merge_ir(&[a, None, b], 2);
        assert!(merged[3] > 180 && merged[7] >= merged[3]);
    }

    #[test]
    fn ip_position() {
        let v = json!({"success": true, "latitude": 45.76, "longitude": 4.84, "city": "Lyon", "region": "Auvergne-Rhone-Alpes", "country": "France"});
        assert_eq!(parse_ipwho(&v), Some((45.76, 4.84, Some("Lyon, France".into()))));
        assert_eq!(parse_ipwho(&json!({"success": false, "message": "reserved range"})), None);
        assert_eq!(parse_ipwho(&json!({"success": true, "latitude": 1.0, "longitude": 2.0})), Some((1.0, 2.0, None)));
    }

    #[test]
    fn dates_in_english_and_german() {
        let today = day(2026, 11, 9);
        assert_eq!(parse_date("show me the clouds on 1 March 2024", today), Some(day(2024, 3, 1)));
        assert_eq!(parse_date("clouds on the 1st of March 2024", today), Some(day(2024, 3, 1)));
        assert_eq!(parse_date("what did it look like on March 1st, 2024", today), Some(day(2024, 3, 1)));
        assert_eq!(parse_date("2024-03-01", today), Some(day(2024, 3, 1)));
        assert_eq!(parse_date("zeig mir die Wolken am 1. März 2024", today), Some(day(2024, 3, 1)));
        assert_eq!(parse_date("what did Europe look like yesterday", today), Some(day(2026, 11, 8)));
        assert_eq!(parse_date("the day before yesterday", today), Some(day(2026, 11, 7)));
        assert_eq!(parse_date("wie sah es gestern aus", today), Some(day(2026, 11, 8)));
        assert_eq!(parse_date("3 days ago", today), Some(day(2026, 11, 6)));
        assert_eq!(parse_date("vor 3 Tagen", today), Some(day(2026, 11, 6)));
        assert_eq!(parse_date("last week", today), Some(day(2026, 11, 2)));
        // Without a year: the last one that has been.
        assert_eq!(parse_date("on 24 December", today), Some(day(2025, 12, 24)));
        assert_eq!(parse_date("on 1 November", today), Some(day(2026, 11, 1)));
        assert_eq!(parse_date("on 1 March 2030", today), None, "the future has no imagery");
        assert_eq!(parse_date("show me the clouds", today), None);
        assert_eq!(parse_date("may I see the clouds", today), None);
        assert_eq!(spoken_date(day(2024, 3, 1)), "1 March 2024");
    }

    #[test]
    fn the_sun_is_overhead_where_it_should_be() {
        // The March equinox 2024 (03:06 UTC on the 20th): at noon UTC the sun is over the equator
        // near the prime meridian (the equation of time puts it ~2 degrees east).
        let (lat, lon) = subsolar_point(Utc.with_ymd_and_hms(2024, 3, 20, 12, 0, 0).unwrap());
        assert!(lat.abs() < 0.5 && (lon - 1.9).abs() < 1.0, "{lat}, {lon}");
        // The June solstice: over the Tropic of Cancer; six hours later 90 degrees further west.
        let (lat, lon) = subsolar_point(Utc.with_ymd_and_hms(2024, 6, 20, 18, 0, 0).unwrap());
        assert!((lat - 23.44).abs() < 0.1 && (lon + 89.6).abs() < 0.8, "{lat}, {lon}");
        // December: over the Tropic of Capricorn.
        let (lat, _) = subsolar_point(Utc.with_ymd_and_hms(2025, 12, 21, 0, 0, 0).unwrap());
        assert!((lat + 23.43).abs() < 0.1, "{lat}");
    }
}
