//! What plays, for the emblem and the lyric line (2026-11-12). While the music visuals or the
//! lyrics are on, the playing track is looked up every second and a half (the system's media
//! session: read only, never a command to the player). For the lyrics, the track's synced text
//! is fetched once from lrclib.net (only the title and the artist are sent) and the line that
//! is being sung is published for the HUD (`hud_feel::lyric_line`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The track that plays.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    /// Where in the song it was (seconds) when it was last looked up, and when that was.
    position: Option<f32>,
    looked: Instant,
    /// Since when this title has been seen (the fallback clock for players without a timeline).
    since: Instant,
}

impl Track {
    /// Where in the song it is now (seconds): the player's own timeline when it has one, else
    /// the time since this title came up.
    pub fn position(&self) -> f32 {
        match self.position {
            Some(at) => at + if self.playing { self.looked.elapsed().as_secs_f32() } else { 0.0 },
            None => self.since.elapsed().as_secs_f32(),
        }
    }
}

static TRACK: Mutex<Option<Track>> = Mutex::new(None);
static WANT_VISUALS: AtomicBool = AtomicBool::new(false);
static WANT_LYRICS: AtomicBool = AtomicBool::new(false);
static WANTED_AT: Mutex<Option<Instant>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);

/// The HUD says each frame what it shows of the music; the lookup runs while it shows anything.
pub fn want(visuals: bool, lyrics: bool, rt: &tokio::runtime::Handle) {
    WANT_VISUALS.store(visuals, Ordering::Release);
    WANT_LYRICS.store(lyrics, Ordering::Release);
    if (visuals || lyrics) && !crate::platform::in_test_session() {
        *lock(&WANTED_AT) = Some(Instant::now());
        if !RUNNING.swap(true, Ordering::AcqRel) {
            rt.spawn(async {
                follow().await;
                RUNNING.store(false, Ordering::Release);
            });
        }
    }
}

fn wanted() -> bool {
    (WANT_VISUALS.load(Ordering::Acquire) || WANT_LYRICS.load(Ordering::Acquire)) && lock(&WANTED_AT).is_some_and(|at| at.elapsed() < Duration::from_secs(5))
}

/// The playing track, as last looked up (None while nothing plays or nobody looks).
pub fn track() -> Option<Track> {
    lock(&TRACK).clone().filter(|t| t.playing)
}

/// "Back in Black — AC/DC" for the line under the emblem.
pub fn title_line() -> Option<String> {
    let t = track()?;
    Some(if t.artist.trim().is_empty() { t.title } else { format!("{} \u{2014} {}", t.title, t.artist) })
}

/// A song's title as the lyrics are filed under it: without what players add in brackets or
/// after a dash ("(Official Video)", "[Remastered 2011]", " - Live", " feat. X").
pub fn clean_title(title: &str) -> String {
    static EXTRAS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\s*[\(\[][^\)\]]*\b(official|video|audio|lyrics?|remaster(ed)?|live|version|edit|mix|explicit|feat|ft|hd|4k)\b[^\)\]]*[\)\]]|\s+-\s+(remaster(ed)?( \d{4})?|\d{4} remaster(ed)?|live.*|single version|radio edit|mono|stereo)$|\s+(feat\.?|ft\.?)\s+.*$").expect("valid title regex")
    });
    let mut t = title.to_string();
    for _ in 0..3 {
        let next = EXTRAS.replace_all(&t, "").trim().to_string();
        if next == t {
            break;
        }
        t = next;
    }
    t
}

/// Synced lyrics (LRC: "[01:23.45] the line") as (seconds, line), in order. Lines without a
/// time, tags like "[ar: …]" and empty lines are left out.
pub fn parse_lrc(text: &str) -> Vec<(f32, String)> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(close) = rest.strip_prefix('[').and_then(|r| r.find(']')) {
            let stamp = &rest[1..=close];
            let stamp = stamp.trim_end_matches(']');
            let secs = stamp.split_once(':').and_then(|(m, s)| Some(m.trim().parse::<f32>().ok()? * 60.0 + s.trim().parse::<f32>().ok()?));
            match secs {
                Some(at) => times.push(at),
                None => {
                    times.clear();
                    rest = "";
                    break;
                }
            }
            rest = rest[close + 2..].trim_start();
        }
        let line = rest.trim();
        if !line.is_empty() {
            lines.extend(times.into_iter().map(|at| (at, line.to_string())));
        }
    }
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    lines
}

/// The line being sung `position` seconds into the song: the last one that has begun, shown
/// from a little before its time; None before the first line and ten seconds after the last
/// one began (the song has moved on to its outro).
pub fn line_at(lines: &[(f32, String)], position: f32) -> Option<&str> {
    const EARLY: f32 = 0.25;
    let index = lines.iter().rposition(|(at, _)| *at - EARLY <= position)?;
    // (Until the next line begins, ten seconds at most: a long break in the singing shows nothing.)
    let ends = lines.get(index + 1).map_or(lines[index].0 + 10.0, |next| next.0.min(lines[index].0 + 10.0));
    (position < ends).then_some(lines[index].1.as_str())
}

/// The lyrics of a track: fetched once per track, None when there are none.
static LYRICS: Mutex<Option<(String, String, Option<Vec<(f32, String)>>)>> = Mutex::new(None);

async fn fetch(title: &str, artist: &str) -> Option<Vec<(f32, String)>> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(8)).user_agent("hud-lyrics/1.0").build().ok()?;
    let url = format!("https://lrclib.net/api/search?track_name={}&artist_name={}", urlencoding::encode(&clean_title(title)), urlencoding::encode(artist.trim()));
    let found: serde_json::Value = client.get(url).send().await.ok()?.json().await.ok()?;
    let synced = found.as_array()?.iter().find_map(|hit| hit["syncedLyrics"].as_str().filter(|s| !s.trim().is_empty()))?;
    let lines = parse_lrc(synced);
    (!lines.is_empty()).then_some(lines)
}

async fn follow() {
    tracing::debug!("[MUSIC] Following what plays");
    while wanted() {
        let now = tokio::time::timeout(Duration::from_millis(800), crate::music::now_playing()).await.ok().flatten();
        let track = now.filter(|n| !n.title.trim().is_empty()).map(|n| {
            let known = lock(&TRACK).clone().filter(|t| t.title == n.title && t.artist == n.artist);
            Track { title: n.title, artist: n.artist, playing: n.playing, position: n.position, looked: Instant::now(), since: known.map_or_else(Instant::now, |t| t.since) }
        });
        *lock(&TRACK) = track.clone();
        if WANT_LYRICS.load(Ordering::Acquire) {
            match track.filter(|t| t.playing) {
                Some(t) => {
                    let known = lock(&LYRICS).as_ref().is_some_and(|(title, artist, _)| *title == t.title && *artist == t.artist);
                    if !known {
                        // (Marked first, so a slow answer is not asked for twice.)
                        *lock(&LYRICS) = Some((t.title.clone(), t.artist.clone(), None));
                        let lines = fetch(&t.title, &t.artist).await;
                        tracing::info!("[MUSIC] Lyrics for '{}': {}", t.title, lines.as_ref().map_or("none found".to_string(), |l| format!("{} lines", l.len())));
                        *lock(&LYRICS) = Some((t.title.clone(), t.artist.clone(), lines));
                    }
                    // The line changes between two looks: published four times a second.
                    for _ in 0..6 {
                        let line = lock(&LYRICS).as_ref().and_then(|(_, _, lines)| lines.as_ref().and_then(|l| line_at(l, t.position()).map(str::to_string)));
                        crate::hud_feel::publish_lyric_line(line);
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    continue;
                }
                None => crate::hud_feel::publish_lyric_line(None),
            }
        } else {
            crate::hud_feel::publish_lyric_line(None);
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    *lock(&TRACK) = None;
    crate::hud_feel::publish_lyric_line(None);
}

/// Whether lyrics were looked for and none were found for the playing track (he says so once).
pub fn none_found() -> bool {
    lock(&LYRICS).as_ref().is_some_and(|(_, _, lines)| lines.is_none()) && track().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lyrics service for real: one request with a famous song's title and artist, and
    /// what came back in numbers. `cargo test --bin jai the_lyrics_service -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_lyrics_service_answers_with_timed_lines() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let lines = rt.block_on(fetch("What a Wonderful World", "Louis Armstrong")).expect("timed lyrics for a famous song");
        println!("{} lines, the first at {:.1} s, the last at {:.1} s", lines.len(), lines[0].0, lines[lines.len() - 1].0);
        assert!(lines.len() > 8 && lines.windows(2).all(|w| w[0].0 <= w[1].0), "timed and in order");
        assert!(line_at(&lines, lines[3].0 + 0.1).is_some());
    }

    #[test]
    fn synced_lyrics_are_read_and_the_sung_line_found() {
        let lrc = "[ar: Somebody]\n[00:12.50] First line\n[00:17.00]Second line\n\n[00:21.10][01:02.00] The chorus\nno time on this one\n[00:30.00]\n";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.iter().map(|(t, l)| (*t, l.as_str())).collect::<Vec<_>>(), [(12.5, "First line"), (17.0, "Second line"), (21.1, "The chorus"), (62.0, "The chorus")]);
        assert_eq!(line_at(&lines, 5.0), None, "before the first line");
        assert_eq!(line_at(&lines, 12.3), Some("First line"), "a moment early");
        assert_eq!(line_at(&lines, 16.7), Some("First line"));
        assert_eq!(line_at(&lines, 16.9), Some("Second line"), "a moment early, like every line");
        assert_eq!(line_at(&lines, 17.0), Some("Second line"));
        assert_eq!(line_at(&lines, 40.0), None, "ten seconds after a line with nothing behind it for a while");
        assert_eq!(line_at(&lines, 63.0), Some("The chorus"));
        assert_eq!(line_at(&lines, 80.0), None, "the song has moved on");
        assert!(parse_lrc("just words\nno times").is_empty());
    }

    #[test]
    fn a_title_as_the_lyrics_are_filed_under_it() {
        for (raw, want) in [
            ("Back in Black", "Back in Black"),
            ("Bohemian Rhapsody (Official Video)", "Bohemian Rhapsody"),
            ("Heroes - 2017 Remaster", "Heroes"),
            ("Paranoid [Remastered 2012]", "Paranoid"),
            ("Stan feat. Dido", "Stan"),
            ("Smoke on the Water - Live", "Smoke on the Water"),
            ("(I Can't Get No) Satisfaction", "(I Can't Get No) Satisfaction"),
        ] {
            assert_eq!(clean_title(raw), want, "{raw}");
        }
    }

    #[test]
    fn a_track_keeps_time_by_the_players_clock_or_its_own() {
        let now = Instant::now();
        let with_timeline = Track { title: "x".into(), artist: "y".into(), playing: true, position: Some(60.0), looked: now - Duration::from_secs(2), since: now - Duration::from_secs(500) };
        assert!((with_timeline.position() - 62.0).abs() < 0.2);
        let paused = Track { playing: false, ..with_timeline.clone() };
        assert!((paused.position() - 60.0).abs() < 0.01, "a paused song stands still");
        let without = Track { position: None, since: now - Duration::from_secs(30), ..with_timeline };
        assert!((without.position() - 30.0).abs() < 0.2);
    }
}
