//! Title, artist and album of an audio file from its own tags, read without any decoder:
//! ID3v2.2–2.4 and ID3v1 (MP3), Vorbis comments (FLAC, Ogg Vorbis, Opus), the iTunes `ilst`
//! atoms (M4A / AAC) and RIFF `LIST/INFO` (WAV). A file without usable tags falls back to its
//! name ("AC/DC - Highway to Hell.mp3", "01 - Back in Black.flac").

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags {
    pub title: String,
    pub artist: String,
    pub album: String,
}

impl Tags {
    fn is_empty(&self) -> bool {
        self.title.trim().is_empty() && self.artist.trim().is_empty()
    }
}

/// The best title / artist for `path`: its tags, else its file name.
pub fn read(path: &Path) -> Tags {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let tags = match ext.as_str() {
        "mp3" => read_id3(path),
        "flac" => read_flac(path),
        "ogg" | "opus" | "oga" => read_ogg(path),
        "m4a" | "mp4" | "aac" | "m4b" | "alac" => read_mp4(path),
        "wav" => read_wav(path),
        _ => None,
    }
    .unwrap_or_default();
    let (name_artist, name_title) = from_file_name(path);
    Tags {
        title: clean(&tags.title).unwrap_or(name_title),
        artist: clean(&tags.artist).unwrap_or(name_artist),
        album: clean(&tags.album).unwrap_or_default(),
    }
}

fn clean(s: &str) -> Option<String> {
    let s = s.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!s.is_empty()).then(|| s.to_string())
}

/// "AC/DC - Highway to Hell" → ("AC/DC", "Highway to Hell"); "01 - Back in Black" and
/// "01. Back in Black" lose their track number; "2 Unlimited - No Limit" keeps its artist.
pub fn from_file_name(path: &Path) -> (String, String) {
    static NUMBER: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^(?:\d{1,3}\s*[.\-)]\s+|\d{2}\s+)").expect("valid track number regex"));
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default().replace('_', " ");
    let base = NUMBER.replace(stem.trim(), "").trim().to_string();
    let base = if base.is_empty() { stem.trim().to_string() } else { base };
    match base.split_once(" - ") {
        Some((artist, title)) if !artist.trim().is_empty() && !title.trim().is_empty() => (artist.trim().to_string(), title.trim().to_string()),
        _ => (String::new(), base),
    }
}

/// Up to `max` bytes from `offset` (negative = from the end).
fn chunk(path: &Path, offset: i64, max: usize) -> Option<Vec<u8>> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = if offset < 0 { len.saturating_sub(offset.unsigned_abs()) } else { (offset as u64).min(len) };
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity(max.min(len as usize));
    f.take(max as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn u32_be(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u32_le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn syncsafe(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(((s[0] as u32 & 0x7f) << 21) | ((s[1] as u32 & 0x7f) << 14) | ((s[2] as u32 & 0x7f) << 7) | (s[3] as u32 & 0x7f))
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().take_while(|b| **b != 0).map(|&b| b as char).collect()
}

fn utf16(bytes: &[u8], big_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|p| if big_endian { u16::from_be_bytes([p[0], p[1]]) } else { u16::from_le_bytes([p[0], p[1]]) })
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// An ID3v2 text frame body: encoding byte + text.
fn id3_text(body: &[u8]) -> String {
    let Some((&encoding, text)) = body.split_first() else { return String::new() };
    match encoding {
        0 => latin1(text),
        1 => match text {
            [0xff, 0xfe, rest @ ..] => utf16(rest, false),
            [0xfe, 0xff, rest @ ..] => utf16(rest, true),
            other => utf16(other, false),
        },
        2 => utf16(text, true),
        _ => String::from_utf8_lossy(text.split(|b| *b == 0).next().unwrap_or(text)).into_owned(),
    }
}

fn read_id3(path: &Path) -> Option<Tags> {
    let head = chunk(path, 0, 10)?;
    let mut tags = Tags::default();
    if head.starts_with(b"ID3") && head.len() == 10 {
        let version = head[3];
        let flags = head[5];
        let size = syncsafe(&head, 6)? as usize;
        // Cover art can be megabytes; the text frames come first in practice.
        let body = chunk(path, 10, size.min(512 * 1024))?;
        let mut at = 0usize;
        if flags & 0x40 != 0 {
            // Extended header: v2.3 size excludes its own 4 bytes, v2.4 includes them (syncsafe).
            at = if version >= 4 { syncsafe(&body, 0)? as usize } else { u32_be(&body, 0)? as usize + 4 };
        }
        let (id_len, header_len) = if version == 2 { (3, 6) } else { (4, 10) };
        while at + header_len <= body.len() {
            let id = &body[at..at + id_len];
            if id.iter().all(|b| *b == 0) {
                break;
            }
            let frame_size = match version {
                2 => ((body[at + 3] as usize) << 16) | ((body[at + 4] as usize) << 8) | body[at + 5] as usize,
                3 => u32_be(&body, at + 4)? as usize,
                _ => syncsafe(&body, at + 4)? as usize,
            };
            let start = at + header_len;
            let end = start.checked_add(frame_size)?;
            if frame_size == 0 || end > body.len() {
                break;
            }
            let frame = &body[start..end];
            match id {
                b"TIT2" | b"TT2" => tags.title = id3_text(frame),
                b"TPE1" | b"TP1" => tags.artist = id3_text(frame),
                b"TALB" | b"TAL" => tags.album = id3_text(frame),
                _ => {}
            }
            at = end;
        }
    }
    if tags.is_empty() {
        // ID3v1 at the end of the file.
        let tail = chunk(path, -128, 128)?;
        if tail.len() == 128 && tail.starts_with(b"TAG") {
            tags = Tags { title: latin1(&tail[3..33]), artist: latin1(&tail[33..63]), album: latin1(&tail[63..93]) };
        }
    }
    Some(tags)
}

/// A Vorbis comment block (vendor string, then `KEY=value` entries; little-endian lengths).
fn vorbis_comments(b: &[u8]) -> Tags {
    let mut tags = Tags::default();
    let Some(vendor) = u32_le(b, 0) else { return tags };
    let mut at = 4 + vendor as usize;
    let Some(count) = u32_le(b, at) else { return tags };
    at += 4;
    for _ in 0..count.min(512) {
        let Some(len) = u32_le(b, at) else { break };
        let Some(entry) = b.get(at + 4..at + 4 + len as usize) else { break };
        at += 4 + len as usize;
        let text = String::from_utf8_lossy(entry);
        if let Some((key, value)) = text.split_once('=') {
            match key.to_ascii_uppercase().as_str() {
                "TITLE" => tags.title = value.to_string(),
                "ARTIST" if tags.artist.is_empty() => tags.artist = value.to_string(),
                "ALBUMARTIST" if tags.artist.is_empty() => tags.artist = value.to_string(),
                "ALBUM" => tags.album = value.to_string(),
                _ => {}
            }
        }
    }
    tags
}

fn read_flac(path: &Path) -> Option<Tags> {
    let b = chunk(path, 0, 1024 * 1024)?;
    if !b.starts_with(b"fLaC") {
        return None;
    }
    let mut at = 4;
    while at + 4 <= b.len() {
        let header = b[at];
        let len = ((b[at + 1] as usize) << 16) | ((b[at + 2] as usize) << 8) | b[at + 3] as usize;
        let body = b.get(at + 4..(at + 4 + len).min(b.len()))?;
        if header & 0x7f == 4 {
            return Some(vorbis_comments(body));
        }
        if header & 0x80 != 0 {
            break;
        }
        at += 4 + len;
    }
    None
}

/// Ogg Vorbis / Opus: the comment packet follows its magic in the second page. Page headers
/// that split a long comment packet are removed first.
fn read_ogg(path: &Path) -> Option<Tags> {
    let raw = chunk(path, 0, 256 * 1024)?;
    // Concatenate the page payloads so a packet spanning pages reads continuously.
    let mut data = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at + 27 <= raw.len() && &raw[at..at + 4] == b"OggS" {
        let segments = raw[at + 26] as usize;
        let table = raw.get(at + 27..at + 27 + segments)?;
        let payload_len: usize = table.iter().map(|s| *s as usize).sum();
        let start = at + 27 + segments;
        let end = (start + payload_len).min(raw.len());
        data.extend_from_slice(&raw[start..end]);
        at = start + payload_len;
    }
    for (magic, skip) in [(&b"OpusTags"[..], 8usize), (&b"\x03vorbis"[..], 7usize)] {
        if let Some(pos) = data.windows(magic.len()).position(|w| w == magic) {
            return Some(vorbis_comments(&data[pos + skip..]));
        }
    }
    None
}

/// iTunes metadata: `©nam` / `©ART` / `©alb` atoms, each holding a `data` atom with the text.
fn read_mp4(path: &Path) -> Option<Tags> {
    let mut tags = Tags::default();
    // `moov` sits at the start of streamable files and at the end of others.
    for (offset, max) in [(0i64, 2 * 1024 * 1024usize), (-2 * 1024 * 1024, 2 * 1024 * 1024)] {
        let b = chunk(path, offset, max)?;
        for (atom, field) in [(b"\xa9nam", 0), (b"\xa9ART", 1), (b"\xa9alb", 2)] {
            let Some(pos) = b.windows(4).position(|w| w == atom) else { continue };
            // [size]["©nam"][size]["data"][type][locale][text]
            let data = pos + 4;
            if b.get(data + 4..data + 8) != Some(b"data") {
                continue;
            }
            let Some(size) = u32_be(&b, data) else { continue };
            let Some(text) = b.get(data + 16..data + size as usize) else { continue };
            let value = String::from_utf8_lossy(text).into_owned();
            match field {
                0 => tags.title = value,
                1 => tags.artist = value,
                _ => tags.album = value,
            }
        }
        if !tags.is_empty() {
            break;
        }
    }
    Some(tags)
}

/// RIFF `LIST`/`INFO`: `INAM` (title), `IART` (artist), `IPRD` (album).
fn read_wav(path: &Path) -> Option<Tags> {
    let b = chunk(path, 0, 1024 * 1024)?;
    let pos = b.windows(4).position(|w| w == b"INFO")?;
    let mut tags = Tags::default();
    let mut at = pos + 4;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = u32_le(&b, at + 4)? as usize;
        let Some(text) = b.get(at + 8..at + 8 + len) else { break };
        match id {
            b"INAM" => tags.title = latin1(text),
            b"IART" => tags.artist = latin1(text),
            b"IPRD" => tags.album = latin1(text),
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    Some(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("jai-tags-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn id3v23(frames: &[(&[u8; 4], &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, text) in frames {
            let mut payload = vec![3u8];
            payload.extend_from_slice(text.as_bytes());
            body.extend_from_slice(*id);
            body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            body.extend_from_slice(&[0, 0]);
            body.extend_from_slice(&payload);
        }
        let size = body.len() as u32;
        let mut out = b"ID3\x03\x00\x00".to_vec();
        out.extend_from_slice(&[((size >> 21) & 0x7f) as u8, ((size >> 14) & 0x7f) as u8, ((size >> 7) & 0x7f) as u8, (size & 0x7f) as u8]);
        out.extend(body);
        out.extend_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        out
    }

    #[test]
    fn id3_titles_and_artists_are_read() {
        let path = temp("whatever.mp3", &id3v23(&[(b"TIT2", "Highway to Hell"), (b"TPE1", "AC/DC"), (b"TALB", "Highway to Hell")]));
        let tags = read(&path);
        assert_eq!((tags.title.as_str(), tags.artist.as_str()), ("Highway to Hell", "AC/DC"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn flac_vorbis_comments_are_read() {
        let mut comment = Vec::new();
        comment.extend_from_slice(&3u32.to_le_bytes());
        comment.extend_from_slice(b"ven");
        comment.extend_from_slice(&2u32.to_le_bytes());
        for entry in ["TITLE=Back in Black", "ARTIST=AC/DC"] {
            comment.extend_from_slice(&(entry.len() as u32).to_le_bytes());
            comment.extend_from_slice(entry.as_bytes());
        }
        let mut flac = b"fLaC".to_vec();
        flac.push(0x84);
        flac.extend_from_slice(&[(comment.len() >> 16) as u8, (comment.len() >> 8) as u8, comment.len() as u8]);
        flac.extend(comment);
        let path = temp("track.flac", &flac);
        let tags = read(&path);
        assert_eq!((tags.title.as_str(), tags.artist.as_str()), ("Back in Black", "AC/DC"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn untagged_files_use_their_names() {
        assert_eq!(from_file_name(Path::new("AC_DC - Highway to Hell.mp3")), ("AC DC".into(), "Highway to Hell".into()));
        assert_eq!(from_file_name(Path::new("01 - Back in Black.flac")), (String::new(), "Back in Black".into()));
        assert_eq!(from_file_name(Path::new("07. Thunderstruck.mp3")), (String::new(), "Thunderstruck".into()));
        assert_eq!(from_file_name(Path::new("2 Unlimited - No Limit.mp3")), ("2 Unlimited".into(), "No Limit".into()));
        let path = temp("Foreigner - Urgent.ogg", b"not really ogg");
        assert_eq!(read(&path), Tags { title: "Urgent".into(), artist: "Foreigner".into(), album: String::new() });
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
