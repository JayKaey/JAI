//! Sonora's own remote control: one JSON line per connection over its instance socket, the
//! protocol its `sonora <command>` line speaks (wire version 2, Sonora 0.50): the named pipe
//! `\\.\pipe\sonora.sock` on Windows, `sonora.sock` in the runtime folder on Linux. Every order
//! Sonora can carry out goes this way, in a few milliseconds, with no window touched and nothing
//! typed into its search: play and pause, next and previous, seeking, shuffle, repeat, like, and
//! the queue (read it, add at the end, play next, play now: a song,
//! an album, a playlist by its link, private or not downloaded, or its best search hit). What the
//! protocol has no request for (clearing the queue, removing or moving an entry, editing a
//! playlist) only Sonora's own window can do.

use serde_json::{json, Value};
use std::io::{BufRead as _, BufReader, Write as _};
use std::time::Duration;

/// The wire version this client speaks first (a Sonora that speaks another one names it).
const VERSION: u64 = 2;
/// A playback order is answered at once; a queue order after its tracks are fetched.
pub const QUICK: Duration = Duration::from_secs(4);
pub const FETCH: Duration = Duration::from_secs(45);

/// Why Sonora did not do it, in words Sir can be told.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// No Sonora answers on its socket (it is not running).
    Away,
    /// Sonora took too long to answer.
    Slow,
    /// No provider is signed in yet (a Sonora started a moment ago signs in after it opens).
    SignedOut,
    /// Sonora answered with a failure (said as a sentence).
    Refused(String),
    /// The provider of a link is not the one Sonora is on (`link`'s, `current`).
    OtherProvider { link: String, current: String },
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Away => write!(f, "Sonora isn't running"),
            Failure::Slow => write!(f, "Sonora didn't answer in time"),
            Failure::SignedOut => write!(f, "Sonora isn't signed in to its music provider"),
            Failure::Refused(why) => write!(f, "{why}"),
            Failure::OtherProvider { link, current } => write!(f, "that is {link}'s and Sonora is on {current}"),
        }
    }
}

/// One request (`body` without its envelope) and Sonora's answer: `Value::String("done")`, or
/// what `{"value": …}` held. Blocking (call it from a blocking thread); never waits longer than
/// `wait`. Nothing reaches the real Sonora from a test or a scripted session.
pub fn ask(body: &Value, wait: Duration) -> Result<Value, Failure> {
    if crate::platform::hands_off() {
        return Err(Failure::Away);
    }
    let mut version = VERSION;
    for _ in 0..2 {
        let reply = exchange(&json!({ "v": version, "body": body }), wait)?;
        match answer(&reply) {
            // A Sonora that speaks another version says which: once more in that one.
            Err(Answer::Version(theirs)) if theirs != version => version = theirs,
            Err(Answer::Version(_)) => return Err(Failure::Refused("Sonora speaks another version of its remote control".into())),
            Err(Answer::Failed(f)) => return Err(f),
            Ok(value) => return Ok(value),
        }
    }
    Err(Failure::Refused("Sonora speaks another version of its remote control".into()))
}

enum Answer {
    Version(u64),
    Failed(Failure),
}

/// The reply line's body read: done, a value, or a failure in words.
fn answer(reply: &Value) -> Result<Value, Answer> {
    let body = &reply["body"];
    if body.as_str() == Some("done") {
        return Ok(Value::String("done".into()));
    }
    if let Some(value) = body.get("value") {
        return Ok(value.clone());
    }
    let Some(failed) = body.get("failed") else {
        return Err(Answer::Failed(Failure::Refused("Sonora gave an answer I don't understand".into())));
    };
    if let Some(v) = failed.get("version").and_then(Value::as_u64) {
        return Err(Answer::Version(v));
    }
    if failed.as_str() == Some("signed-out") {
        return Err(Answer::Failed(Failure::SignedOut));
    }
    if let Some(o) = failed.get("other-provider") {
        let text = |k: &str| o[k].as_str().unwrap_or("another provider").to_string();
        return Err(Answer::Failed(Failure::OtherProvider { link: text("link"), current: text("current") }));
    }
    Err(Answer::Failed(Failure::Refused(failure_words(failed))))
}

/// A failure of Sonora's as a sentence.
fn failure_words(failed: &Value) -> String {
    let named = |key: &str| failed.get(key).and_then(Value::as_str).map(str::to_string);
    if let Some(s) = failed.as_str() {
        return match s {
            "malformed" => "Sonora didn't understand the request".into(),
            "unsupported" => "Sonora can't do that for this track".into(),
            "no-current-track" => "nothing is playing in Sonora".into(),
            "missing-track-id" => "Sonora has no id for this track".into(),
            "busy" => "Sonora is busy with that already".into(),
            "favorite-update-failed" => "Sonora couldn't save the like".into(),
            other => format!("Sonora refused it ({other})"),
        };
    }
    if let Some(link) = named("unqueueable") {
        return format!("Sonora can't queue {link}");
    }
    if let Some(path) = named("unreadable") {
        return format!("Sonora can't read {path}");
    }
    if let Some(what) = named("unfetched") {
        return format!("Sonora couldn't fetch {what}");
    }
    if let Some(what) = named("nothing-found") {
        return format!("Sonora found nothing for \"{what}\"");
    }
    if let Some(provider) = named("unknown-provider") {
        return format!("Sonora doesn't know the provider {provider}");
    }
    if let Some(provider) = named("no-account").or_else(|| named("sign-in-failed")) {
        return format!("Sonora isn't signed in to {provider}");
    }
    format!("Sonora refused it ({failed})")
}

/// The request line out, the reply line in.
fn exchange(envelope: &Value, wait: Duration) -> Result<Value, Failure> {
    let line = format!("{envelope}\n");
    let (tx, rx) = std::sync::mpsc::channel();
    // (A pipe read cannot time out on its own: the exchange runs on a thread of its own, which
    // ends when Sonora answers or hangs up, whatever happened to the wait.)
    std::thread::Builder::new()
        .name("jai-sonora".into())
        .spawn(move || {
            let result = (|| {
                let mut stream = socket_open().map_err(|_| Failure::Away)?;
                stream.write_all(line.as_bytes()).map_err(|_| Failure::Away)?;
                stream.flush().map_err(|_| Failure::Away)?;
                let mut reply = String::new();
                BufReader::new(stream).read_line(&mut reply).map_err(|_| Failure::Away)?;
                serde_json::from_str::<Value>(reply.trim()).map_err(|_| Failure::Refused("Sonora's answer was cut off".into()))
            })();
            let _ = tx.send(result);
        })
        .map_err(|_| Failure::Away)?;
    rx.recv_timeout(wait).unwrap_or(Err(Failure::Slow))
}

#[cfg(windows)]
fn socket_open() -> std::io::Result<std::fs::File> {
    const PIPE: &str = r"\\.\pipe\sonora.sock";
    const PIPE_BUSY: i32 = 231;
    let mut tries = 0;
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(PIPE) {
            // (Every instance of the pipe is serving another client this moment.)
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) && tries < 25 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            other => return other,
        }
    }
}

#[cfg(not(windows))]
fn socket_open() -> std::io::Result<std::os::unix::net::UnixStream> {
    let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, "no Sonora socket");
    for path in socket_paths() {
        match std::os::unix::net::UnixStream::connect(&path) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Where a Sonora on Linux keeps its socket: the runtime folder, a Flatpak's own folder in it,
/// else its cache folder (upstream `crates/sonora/src/single.rs`, `private_dir`).
#[cfg(not(windows))]
fn socket_paths() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut out = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()).map(PathBuf::from) {
        out.push(runtime.join("sonora.sock"));
        if let Ok(apps) = std::fs::read_dir(runtime.join("app")) {
            out.extend(apps.flatten().map(|app| app.path().join("sonora.sock")).filter(|p| p.exists()));
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        out.push(home.join(".cache/sonora/sonora.sock"));
    }
    out
}

// ------------------------------------------------------------------------- requests ----

/// `{"playback": …}`: play, pause, toggle, next, previous, current-track.
pub fn playback(op: &str) -> Value {
    json!({ "playback": op })
}

/// Seeking: to a second, by seconds (negative is back), or to a percentage of the track.
pub enum Seek {
    To(u64),
    By(i64),
    Percent(u8),
}

pub fn seek(how: Seek) -> Value {
    match how {
        Seek::To(s) => json!({ "playback": { "seek-time": { "to": s } } }),
        Seek::By(s) => json!({ "playback": { "seek-time": { "by": s } } }),
        Seek::Percent(p) => json!({ "playback": { "seek": p.min(100) } }),
    }
}

/// A switch of Sonora's (`mute`, `shuffle`: on, off, toggle; `repeat`: off, all, one; `like`:
/// on, off, toggle); `None` reads it.
pub fn switch(name: &str, to: Option<&str>) -> Value {
    if name == "like" {
        return json!({ "like": to });
    }
    let mut op = serde_json::Map::new();
    op.insert(name.to_string(), to.map_or(Value::Null, |t| Value::String(t.into())));
    json!({ "playback": op })
}

/// What a queue order carries: a provider link, a file, or the best hit of a search on Sonora's
/// provider (`kind`: song, album, artist, playlist).
pub enum Item {
    Link(String),
    Path(String),
    Search { text: String, kind: &'static str },
}

impl Item {
    fn wire(&self) -> Value {
        match self {
            Item::Link(link) => json!({ "link": link }),
            Item::Path(path) => json!({ "path": path }),
            Item::Search { text, kind } => json!({ "search": { "text": text, "kind": kind } }),
        }
    }
}

/// Where queued items go: `play` (right now, the queue kept after them), `play-next` (after
/// the song that is playing), `add` (after what was queued by hand).
pub fn queue(place: &str, items: &[Item]) -> Value {
    let items: Vec<Value> = items.iter().map(Item::wire).collect();
    let mut order = serde_json::Map::new();
    order.insert(place.to_string(), Value::Array(items));
    json!({ "queue": order })
}

pub fn queue_snapshot() -> Value {
    json!({ "queue": "snapshot" })
}

/// Switches Sonora to a provider (its slug: "spotify", "youtube" …).
pub fn provider(slug: &str) -> Value {
    json!({ "provider": { "current": slug } })
}

/// The share link of a `spotify:` URI the library keeps ("spotify:playlist:ID" →
/// "https://open.spotify.com/playlist/ID"); a link is kept as it is.
pub fn link_of(uri: &str) -> Option<String> {
    if uri.starts_with("https://") {
        return Some(uri.to_string());
    }
    let mut parts = uri.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("spotify"), Some(kind @ ("track" | "album" | "playlist" | "artist")), Some(id), None) if !id.is_empty() && id.chars().all(char::is_alphanumeric) => {
            Some(format!("https://open.spotify.com/{kind}/{id}"))
        }
        _ => None,
    }
}

// ------------------------------------------------------------------------- answers ----

/// "Highway to Hell by AC/DC (playing, 1:12 of 3:28, liked)" from `current-track`'s value.
pub fn describe_current(value: &Value) -> Option<String> {
    let current = value.get("current")?;
    if current.is_null() {
        return None;
    }
    let track = &current["track"];
    let title = track["title"].as_str().unwrap_or("").trim();
    if title.is_empty() {
        return None;
    }
    let artists: Vec<&str> = track["artists"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut said = if artists.is_empty() { title.to_string() } else { format!("{title} by {}", artists.join(", ")) };
    let mut notes = vec![current["state"].as_str().unwrap_or("playing").to_string()];
    if let (Some(at), Some(length)) = (current["position"].as_f64(), track["duration"].as_f64()) {
        notes.push(format!("{} of {}", clock(at), clock(length)));
    }
    if current["liked"].as_bool() == Some(true) {
        notes.push("liked".into());
    }
    if let Some(name) = current["origin"]["name"].as_str().filter(|n| !n.is_empty()) {
        notes.push(format!("from {name}"));
    }
    said.push_str(&format!(" ({})", notes.join(", ")));
    Some(said)
}

/// The queue in a sentence: what plays now and the next few.
pub fn describe_queue(value: &Value, shown: usize) -> String {
    let q = &value["queue"];
    let name = |entry: &Value| {
        let t = &entry["track"];
        let title = t["title"].as_str().unwrap_or("?");
        match t["artists"].as_array().and_then(|a| a.first()).and_then(Value::as_str) {
            Some(by) => format!("{title} by {by}"),
            None => title.to_string(),
        }
    };
    let upcoming: Vec<&Value> = q["upcoming"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let now = q.get("current").filter(|c| !c.is_null()).map(|c| format!("Now: {}.", name(c))).unwrap_or_else(|| "Nothing is playing.".into());
    if upcoming.is_empty() {
        return format!("{now} Nothing is queued after it.");
    }
    let next: Vec<String> = upcoming.iter().take(shown).map(|e| name(e)).collect();
    let more = upcoming.len().saturating_sub(shown);
    let more = match (more, q["truncated"].as_bool() == Some(true)) {
        (0, false) => String::new(),
        (n, false) => format!(", and {n} more"),
        (_, true) => ", and more after them".to_string(),
    };
    format!("{now} Next: {}{more}.", next.join("; "))
}

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every request as Sonora 0.50's `cli::commands::Request` reads it (serde, kebab-case,
    /// externally tagged; upstream `crates/cli/src/commands.rs`).
    #[test]
    fn requests_are_written_as_sonora_reads_them() {
        let is = |request: Value, wire: &str| assert_eq!(request, serde_json::from_str::<Value>(wire).unwrap(), "{wire}");
        is(playback("next"), r#"{"playback":"next"}"#);
        is(seek(Seek::To(90)), r#"{"playback":{"seek-time":{"to":90}}}"#);
        is(seek(Seek::By(-10)), r#"{"playback":{"seek-time":{"by":-10}}}"#);
        is(seek(Seek::Percent(150)), r#"{"playback":{"seek":100}}"#);
        is(switch("shuffle", Some("toggle")), r#"{"playback":{"shuffle":"toggle"}}"#);
        is(switch("repeat", Some("one")), r#"{"playback":{"repeat":"one"}}"#);
        is(switch("like", Some("on")), r#"{"like":"on"}"#);
        is(switch("mute", None), r#"{"playback":{"mute":null}}"#);
        is(queue("play-next", &[Item::Search { text: "highway to hell".into(), kind: "song" }]), r#"{"queue":{"play-next":[{"search":{"text":"highway to hell","kind":"song"}}]}}"#);
        is(queue("add", &[Item::Link("https://open.spotify.com/track/abc".into())]), r#"{"queue":{"add":[{"link":"https://open.spotify.com/track/abc"}]}}"#);
        is(queue("play", &[Item::Search { text: "Road Trip".into(), kind: "playlist" }]), r#"{"queue":{"play":[{"search":{"text":"Road Trip","kind":"playlist"}}]}}"#);
        is(queue("add", &[Item::Path(r"C:\Music\a.flac".into())]), r#"{"queue":{"add":[{"path":"C:\\Music\\a.flac"}]}}"#);
        is(queue_snapshot(), r#"{"queue":"snapshot"}"#);
        is(provider("spotify"), r#"{"provider":{"current":"spotify"}}"#);
        // The envelope: version 2, the body under "body".
        is(json!({ "v": VERSION, "body": playback("toggle") }), r#"{"v":2,"body":{"playback":"toggle"}}"#);
    }

    #[test]
    fn library_uris_become_the_links_sonora_queues() {
        assert_eq!(link_of("spotify:playlist:ExamplePlaylistId00000").as_deref(), Some("https://open.spotify.com/playlist/ExamplePlaylistId00000"));
        assert_eq!(link_of("spotify:track:08onVqQ8YicJ98Ycm1qoLf").as_deref(), Some("https://open.spotify.com/track/08onVqQ8YicJ98Ycm1qoLf"));
        assert_eq!(link_of("https://open.spotify.com/album/x").as_deref(), Some("https://open.spotify.com/album/x"));
        assert_eq!(link_of("spotify:user:max:playlist:x"), None);
        assert_eq!(link_of("sonora-local:C:/x.m3u:Road Trip"), None);
    }

    /// Sonora's answers (upstream `crates/cli/src/wire.rs`) read back.
    #[test]
    fn answers_and_failures_are_read() {
        let reply = |body: &str| serde_json::from_str::<Value>(&format!(r#"{{"v":2,"body":{body}}}"#)).unwrap();
        assert_eq!(answer(&reply(r#""done""#)).ok(), Some(Value::String("done".into())));
        assert_eq!(answer(&reply(r#"{"value":{"volume":80}}"#)).ok(), Some(json!({"volume": 80})));
        assert!(matches!(answer(&reply(r#"{"failed":{"version":3}}"#)), Err(Answer::Version(3))));
        let refused = |body: &str| match answer(&reply(body)) {
            Err(Answer::Failed(f)) => f.to_string(),
            _ => String::new(),
        };
        assert_eq!(refused(r#"{"failed":"no-current-track"}"#), "nothing is playing in Sonora");
        assert_eq!(refused(r#"{"failed":{"nothing-found":"xyzzy"}}"#), "Sonora found nothing for \"xyzzy\"");
        assert_eq!(refused(r#"{"failed":{"other-provider":{"link":"Spotify","current":"Deezer"}}}"#), "that is Spotify's and Sonora is on Deezer");
        assert_eq!(refused(r#"{"failed":"signed-out"}"#), "Sonora isn't signed in to its music provider");
    }

    #[test]
    fn what_plays_and_what_comes_are_said() {
        let current = json!({"current": {"state": "playing", "track": {"id": "x", "title": "Highway to Hell", "artists": ["AC/DC"], "album": "Highway to Hell", "cover": null, "duration": 208},
            "position": 72, "volume": 80, "shuffle": false, "repeat": "off", "provider": "spotify", "liked": true, "origin": {"kind": "playlist", "id": "p", "name": "Jays Rock"}}});
        assert_eq!(describe_current(&current).as_deref(), Some("Highway to Hell by AC/DC (playing, 1:12 of 3:28, liked, from Jays Rock)"));
        assert_eq!(describe_current(&json!({"current": null})), None);
        let track = |title: &str, by: &str| json!({"track": {"title": title, "artists": [by]}, "kind": "queued"});
        let q = json!({"queue": {"previous": [], "current": track("Urgent", "Foreigner"), "upcoming": [track("Back In Black", "AC/DC"), track("Thunderstruck", "AC/DC"), track("Hells Bells", "AC/DC")], "truncated": false}});
        assert_eq!(describe_queue(&q, 2), "Now: Urgent by Foreigner. Next: Back In Black by AC/DC; Thunderstruck by AC/DC, and 1 more.");
        let empty = json!({"queue": {"previous": [], "current": null, "upcoming": [], "truncated": false}});
        assert_eq!(describe_queue(&empty, 3), "Nothing is playing. Nothing is queued after it.");
    }

    /// Nothing reaches the real Sonora from a test.
    #[test]
    fn a_test_never_reaches_the_real_sonora() {
        assert_eq!(ask(&playback("next"), QUICK), Err(Failure::Away));
    }
}
