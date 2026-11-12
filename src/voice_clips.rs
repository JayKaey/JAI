//! The last clips of Sir's own orders, kept on this PC (Sir, 2026-11-13, asked whether he
//! should keep them: "Keep them"). What his ear learns it learns from words; with the clips
//! themselves his hearing can be tested on Sir's real voice (`ear_check`), and a line of the
//! hearing log can be corrected with the sound to hand.
//!
//! Kept: a sentence spoken to him that was carried out, in a voice that was Sir's (or, while
//! no print of it is known, with his name), at most 8 s long: the audio as a 16 kHz WAV in
//! `voice_clips` beside the memory database, and a line in that database (`voice_clips`) with
//! what was heard, what it was taken for and how sure both were. The newest `KEPT` stay; older
//! ones go, file and line. Nothing is ever sent anywhere, a scripted session keeps nothing,
//! and "delete my voice clips" removes them all.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How many clips stay.
pub const KEPT: usize = 200;
const RATE: u32 = 16_000;

/// A kept clip, as its line says.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub id: i64,
    /// When it was said (local time, "2026-11-13 21:40:12").
    pub at: String,
    /// The file's name inside the clips folder.
    pub file: String,
    /// What the recogniser wrote at first.
    pub heard: String,
    /// What he took it for (after his ear, and a second hearing).
    pub meant: String,
    /// Whether that was an order he knew.
    pub led: bool,
    /// The recogniser's own sureness (mean log-probability, 0 = certain).
    pub logprob: f32,
    /// How like Sir's print the voice was (0 when no print was known).
    pub likeness: f32,
    pub ms: u32,
}

/// The memory database and the folder beside it (None: nothing is kept).
static HOME: Mutex<Option<(String, PathBuf)>> = Mutex::new(None);

/// Where clips are kept: beside the memory database. Not called in a scripted session.
pub fn set_home(db: &str) {
    let dir = Path::new(db).parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).join("voice_clips");
    *lock(&HOME) = Some((db.to_string(), dir));
}

fn home() -> Option<(String, PathBuf)> {
    lock(&HOME).clone()
}

fn open(db: &str) -> rusqlite::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS voice_clips (id INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT NOT NULL, file TEXT NOT NULL, heard TEXT NOT NULL, meant TEXT NOT NULL, led INTEGER NOT NULL, logprob REAL NOT NULL, likeness REAL NOT NULL, ms INTEGER NOT NULL);",
    )?;
    Ok(conn)
}

/// A clip as a 16-bit mono WAV file's bytes.
pub fn wav(pcm: &[f32]) -> Vec<u8> {
    let data = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    out
}

/// The samples of a 16-bit mono WAV as `wav` writes it (None for anything else).
pub fn unwav(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" || &bytes[36..40] != b"data" {
        return None;
    }
    Some(bytes[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect())
}

/// Keeps a clip (on a thread of its own: the turn does not wait for the disk) and lets the
/// oldest go. `heard`: the recogniser's first words; `meant`: what he took them for.
pub fn keep(pcm: Vec<f32>, heard: String, meant: String, led: bool, logprob: f32, likeness: f32) {
    let Some((db, dir)) = home() else { return };
    if pcm.len() < RATE as usize / 2 || meant.trim().is_empty() {
        return;
    }
    let _ = std::thread::Builder::new().name("jai-voice-clip".into()).spawn(move || {
        if let Err(e) = keep_now(&db, &dir, &pcm, &heard, &meant, led, logprob, likeness) {
            tracing::warn!("[EAR] A clip of Sir's voice was not kept: {e}");
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn keep_now(db: &str, dir: &Path, pcm: &[f32], heard: &str, meant: &str, led: bool, logprob: f32, likeness: f32) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let now = chrono::Local::now();
    let file = format!("{}_{:03}.wav", now.format("%Y%m%d_%H%M%S"), now.timestamp_subsec_millis());
    std::fs::write(dir.join(&file), wav(pcm)).map_err(|e| e.to_string())?;
    let conn = open(db).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO voice_clips (at, file, heard, meant, led, logprob, likeness, ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![now.format("%Y-%m-%d %H:%M:%S").to_string(), file, heard, meant, led as i64, logprob as f64, likeness as f64, (pcm.len() as u64 * 1000 / RATE as u64) as i64],
    )
    .map_err(|e| e.to_string())?;
    // The newest stay; what is older goes, file and line.
    let mut old = conn.prepare("SELECT id, file FROM voice_clips ORDER BY id DESC LIMIT -1 OFFSET ?1").map_err(|e| e.to_string())?;
    let gone: Vec<(i64, String)> = old.query_map([KEPT as i64], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?.flatten().collect();
    for (id, file) in gone {
        let _ = std::fs::remove_file(dir.join(file));
        let _ = conn.execute("DELETE FROM voice_clips WHERE id = ?1", [id]);
    }
    Ok(())
}

/// The newest clips first, `most` at most.
pub fn list(most: usize) -> Vec<Clip> {
    let Some((db, _)) = home() else { return Vec::new() };
    // (Read only: looking makes no table.)
    let Ok(conn) = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Vec::new() };
    let Ok(mut rows) = conn.prepare("SELECT id, at, file, heard, meant, led, logprob, likeness, ms FROM voice_clips ORDER BY id DESC LIMIT ?1") else { return Vec::new() };
    let read = rows.query_map([most as i64], |r| {
        Ok(Clip { id: r.get(0)?, at: r.get(1)?, file: r.get(2)?, heard: r.get(3)?, meant: r.get(4)?, led: r.get::<_, i64>(5)? != 0, logprob: r.get::<_, f64>(6)? as f32, likeness: r.get::<_, f64>(7)? as f32, ms: r.get::<_, i64>(8)? as u32 })
    });
    read.map(|rows| rows.flatten().collect()).unwrap_or_default()
}

/// The audio of a kept clip (16 kHz).
pub fn audio(clip: &Clip) -> Option<Vec<f32>> {
    let (_, dir) = home()?;
    unwav(&std::fs::read(dir.join(&clip.file)).ok()?)
}

/// What a clip was really meant to be (Sir corrected its line in the hearing log).
pub fn correct(id: i64, meant: &str) {
    let Some((db, _)) = home() else { return };
    if let Ok(conn) = open(&db) {
        let _ = conn.execute("UPDATE voice_clips SET meant = ?2, led = 1 WHERE id = ?1", rusqlite::params![id, meant]);
    }
}

/// How many clips are kept, and how many megabytes they take.
pub fn kept() -> (usize, f64) {
    let Some((_, dir)) = home() else { return (0, 0.0) };
    let files: Vec<u64> = std::fs::read_dir(dir).map(|d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "wav")).filter_map(|e| e.metadata().ok()).map(|m| m.len()).collect()).unwrap_or_default();
    (files.len(), files.iter().sum::<u64>() as f64 / 1_048_576.0)
}

/// Deletes every kept clip, file and line. How many there were.
pub fn delete_all() -> usize {
    let Some((db, dir)) = home() else { return 0 };
    let mut n = 0;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "wav")) {
            if std::fs::remove_file(entry.path()).is_ok() {
                n += 1;
            }
        }
    }
    if let Ok(conn) = open(&db) {
        let _ = conn.execute("DELETE FROM voice_clips", []);
    }
    tracing::info!("[EAR] {n} clips of Sir's voice deleted on his word");
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32) -> Vec<f32> {
        (0..(RATE as f32 * secs) as usize).map(|i| 0.4 * (std::f32::consts::TAU * 220.0 * i as f32 / RATE as f32).sin()).collect()
    }

    #[test]
    fn a_clip_comes_back_from_its_file_as_it_went_in() {
        let pcm = tone(0.25);
        let bytes = wav(&pcm);
        assert_eq!(bytes.len(), 44 + pcm.len() * 2);
        let back = unwav(&bytes).expect("a WAV");
        assert_eq!(back.len(), pcm.len());
        assert!(pcm.iter().zip(&back).all(|(a, b)| (a - b).abs() < 1e-3));
        assert_eq!(unwav(b"not a wav at all, just some bytes that are long enough to look at"), None);
    }

    #[test]
    fn the_newest_clips_stay_and_all_go_on_his_word() {
        static ONE: Mutex<()> = Mutex::new(());
        let _one = lock(&ONE);
        let dir = std::env::temp_dir().join(format!("jai-voice-clips-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("memory.db").to_string_lossy().to_string();
        *lock(&HOME) = None;
        assert!(list(10).is_empty() && kept() == (0, 0.0), "nothing is kept without a home");
        set_home(&db);
        let clips = dir.join("voice_clips");
        assert!(list(10).is_empty(), "a database without the table is read as no clips");
        for k in 0..3 {
            keep_now(&db, &clips, &tone(1.0), &format!("they open the mixa {k}"), &format!("open the mixer {k}"), k != 1, -0.2, 0.61).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        let all = list(10);
        assert_eq!(all.len(), 3);
        assert_eq!((all[0].heard.as_str(), all[0].meant.as_str(), all[0].led, all[0].ms), ("they open the mixa 2", "open the mixer 2", true, 1000));
        assert!(!all[1].led && (all[0].likeness - 0.61).abs() < 1e-6);
        assert_eq!(audio(&all[0]).map(|pcm| pcm.len()), Some(RATE as usize));
        assert_eq!(kept().0, 3);
        // Sir corrects a line: it is what he meant, and an order.
        correct(all[1].id, "open the mixer");
        assert_eq!(list(10)[1].meant, "open the mixer");
        assert!(list(10)[1].led);
        assert_eq!(delete_all(), 3);
        assert!(list(10).is_empty() && kept().0 == 0);
        *lock(&HOME) = None;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
