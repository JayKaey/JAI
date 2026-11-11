//! Whose voice it is (Sir, 2026-11-12: "only obey me"). A clip of speech becomes a 256-number
//! voice print with the WeSpeaker ResNet34 model (`models/speaker`, on the processor, offline);
//! Sir's own print is the mean of the clips of his voice set-up and lives in the memory
//! database on this PC. With the voice lock on, a spoken order is carried out only when its
//! voice is close enough to that print.
//!
//! The features are Kaldi's filter bank as the model was trained on it: 16 kHz, 25 ms frames
//! every 10 ms, a Hamming window, 80 mel bins from 20 Hz, the samples at 16-bit scale, and the
//! mean of every bin over the clip taken off.

use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The model's file name (in `models/speaker`).
pub const MODEL: &str = "wespeaker_en_voxceleb_resnet34_LM.onnx";

const RATE: usize = 16_000;
const FRAME: usize = 400;
const SHIFT: usize = 160;
const FFT: usize = 512;
pub const MELS: usize = 80;
/// A clip shorter than this says too little about a voice; a longer one is cut to that.
const MIN_SAMPLES: usize = RATE / 2;
const MAX_SAMPLES: usize = RATE * 8;

fn mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

/// The mel filters: for each of the `MELS` bins, its weight on each of the 256 FFT bins.
fn filters() -> &'static Vec<Vec<(usize, f32)>> {
    static FILTERS: OnceLock<Vec<Vec<(usize, f32)>>> = OnceLock::new();
    FILTERS.get_or_init(|| {
        let (low, high) = (mel(20.0), mel(RATE as f32 / 2.0));
        let step = (high - low) / (MELS + 1) as f32;
        (0..MELS)
            .map(|b| {
                let (left, centre, right) = (low + b as f32 * step, low + (b + 1) as f32 * step, low + (b + 2) as f32 * step);
                (0..FFT / 2)
                    .filter_map(|k| {
                        let m = mel(k as f32 * RATE as f32 / FFT as f32);
                        let w = if m <= left || m >= right {
                            0.0
                        } else if m <= centre {
                            (m - left) / (centre - left)
                        } else {
                            (right - m) / (right - centre)
                        };
                        (w > 0.0).then_some((k, w))
                    })
                    .collect()
            })
            .collect()
    })
}

/// The filter-bank features of a clip (16 kHz, -1 … 1): one row of `MELS` log energies per
/// 10 ms, the clip's mean taken off every bin. Empty for a clip shorter than one frame.
pub fn fbank(pcm: &[f32]) -> Vec<[f32; MELS]> {
    if pcm.len() < FRAME {
        return Vec::new();
    }
    let window: Vec<f32> = (0..FRAME).map(|i| 0.54 - 0.46 * (std::f32::consts::TAU * i as f32 / (FRAME - 1) as f32).cos()).collect();
    let frames = 1 + (pcm.len() - FRAME) / SHIFT;
    let mut rows: Vec<[f32; MELS]> = Vec::with_capacity(frames);
    let (mut re, mut im) = (vec![0.0f32; FFT], vec![0.0f32; FFT]);
    for f in 0..frames {
        let frame = &pcm[f * SHIFT..f * SHIFT + FRAME];
        // At 16-bit scale, its own mean taken off, pre-emphasised, windowed.
        let mean = frame.iter().sum::<f32>() / FRAME as f32 * 32768.0;
        let at = |i: usize| frame[i] * 32768.0 - mean;
        re.iter_mut().for_each(|v| *v = 0.0);
        im.iter_mut().for_each(|v| *v = 0.0);
        for i in 0..FRAME {
            let previous = at(i.saturating_sub(1));
            re[i] = (at(i) - 0.97 * previous) * window[i];
        }
        crate::fish_zero_shot_tts::fft(&mut re, &mut im, false);
        let mut row = [0.0f32; MELS];
        for (b, taps) in filters().iter().enumerate() {
            let energy: f32 = taps.iter().map(|(k, w)| w * (re[*k] * re[*k] + im[*k] * im[*k])).sum();
            row[b] = energy.max(f32::EPSILON).ln();
        }
        rows.push(row);
    }
    let mut means = [0.0f32; MELS];
    for row in &rows {
        for (m, v) in means.iter_mut().zip(row) {
            *m += v / frames as f32;
        }
    }
    for row in rows.iter_mut() {
        for (v, m) in row.iter_mut().zip(&means) {
            *v -= m;
        }
    }
    rows
}

fn normalised(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

/// How alike two voice prints are (-1 … 1; both of unit length).
pub fn likeness(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

static MODEL_PATH: OnceLock<PathBuf> = OnceLock::new();
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// Where the model is (set once at start from the models folder).
pub fn set_models_dir(dir: &Path) {
    let _ = MODEL_PATH.set(dir.join("speaker").join(MODEL));
}

/// Whether the model is there (without it there is no voice lock).
pub fn available() -> bool {
    MODEL_PATH.get().is_some_and(|p| p.is_file())
}

/// The voice print of a clip of speech (16 kHz): 256 numbers of unit length. Loads the model on
/// first use. Runs for a tenth of a second or two on the processor: never on the GUI thread.
pub fn print_of(pcm: &[f32]) -> Result<Vec<f32>, String> {
    if pcm.len() < MIN_SAMPLES {
        return Err("too short to tell a voice by".to_string());
    }
    let pcm = &pcm[..pcm.len().min(MAX_SAMPLES)];
    let rows = fbank(pcm);
    let path = MODEL_PATH.get().filter(|p| p.is_file()).ok_or("the voice model is not installed")?;
    let mut guard = lock(&SESSION);
    if guard.is_none() {
        let env = crate::config::ort_env().map_err(|e| e.to_string())?;
        let session = Session::builder(env)
            .map_err(|e| e.to_string())?
            .with_intra_threads(2)
            .unwrap_or_else(|e| e.recover())
            .commit_from_file(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        tracing::info!("[VOICE] Speaker model loaded from {}", path.display());
        *guard = Some(session);
    }
    let session = guard.as_mut().ok_or("the voice model is not loaded")?;
    let flat: Vec<f32> = rows.iter().flat_map(|r| r.iter().copied()).collect();
    let input = Tensor::from_array(([1usize, rows.len(), MELS], flat)).map_err(|e| e.to_string())?;
    let outputs = session.run(ort::inputs![input]).map_err(|e| e.to_string())?;
    let (_, embedding) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
    if embedding.is_empty() || embedding.iter().any(|x| !x.is_finite()) {
        return Err("the voice model returned nothing usable".to_string());
    }
    Ok(normalised(embedding.to_vec()))
}

/// Sir's voice as JAI knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct VoicePrint {
    pub print: Vec<f32>,
    /// How many clips it was made of.
    pub clips: usize,
    /// A voice at least this like it is his.
    pub threshold: f32,
}

/// The least a voice must be like the print, whatever the set-up's clips say, and the most.
const THRESHOLD_FLOOR: f32 = 0.30;
const THRESHOLD_CEILING: f32 = 0.55;
/// A short order ("stop", "mute") says less about a voice: it passes this much lower.
const SHORT_MS: u32 = 1100;
const SHORT_ALLOWANCE: f32 = 0.10;

/// Sir's print from the clips of his set-up: their mean, and a threshold set below the clip
/// that is least like it (his own voice varies; a stranger's is far below). None without at
/// least three clips.
pub fn enrol(clips: &[Vec<f32>]) -> Option<VoicePrint> {
    if clips.len() < 3 || clips.iter().any(|c| c.len() != clips[0].len() || c.is_empty()) {
        return None;
    }
    let mut mean = vec![0.0f32; clips[0].len()];
    for clip in clips {
        for (m, v) in mean.iter_mut().zip(clip) {
            *m += v;
        }
    }
    let print = normalised(mean);
    let least = clips.iter().map(|c| likeness(c, &print)).fold(f32::INFINITY, f32::min);
    Some(VoicePrint { threshold: (least - 0.22).clamp(THRESHOLD_FLOOR, THRESHOLD_CEILING), print, clips: clips.len() })
}

impl VoicePrint {
    /// Whether a clip's print is Sir's voice, and how alike they are. `speech_ms`: how long he
    /// spoke (a short order passes a little lower).
    pub fn is_his(&self, clip: &[f32], speech_ms: u32) -> (bool, f32) {
        let score = likeness(clip, &self.print);
        let bar = if speech_ms < SHORT_MS { self.threshold - SHORT_ALLOWANCE } else { self.threshold };
        (score >= bar, score)
    }
}

// ------------------------------------------------------------------------- the database ----

fn open(db: &str) -> rusqlite::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS voice_print (id INTEGER PRIMARY KEY CHECK (id = 1), print BLOB NOT NULL, clips INTEGER NOT NULL, threshold REAL NOT NULL, made TEXT NOT NULL);")?;
    Ok(conn)
}

/// Sir's print as it is kept (read from the database once, then from here).
static KNOWN: Mutex<Option<Option<VoicePrint>>> = Mutex::new(None);

/// Sir's voice print, if the set-up has been done.
pub fn known(db: &str) -> Option<VoicePrint> {
    let mut known = lock(&KNOWN);
    if known.is_none() {
        // (Read only: looking for a print never writes to the database.)
        let read = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok().and_then(|conn| {
            conn.query_row("SELECT print, clips, threshold FROM voice_print WHERE id = 1", [], |r| {
                let bytes: Vec<u8> = r.get(0)?;
                Ok(VoicePrint { print: bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect(), clips: r.get::<_, i64>(1)? as usize, threshold: r.get::<_, f64>(2)? as f32 })
            })
            .ok()
        });
        *known = Some(read.filter(|p| !p.print.is_empty()));
    }
    known.clone().flatten()
}

/// Keeps Sir's print (replacing an older one).
pub fn keep(db: &str, print: &VoicePrint) -> Result<(), String> {
    let bytes: Vec<u8> = print.print.iter().flat_map(|v| v.to_le_bytes()).collect();
    let conn = open(db).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO voice_print (id, print, clips, threshold, made) VALUES (1, ?1, ?2, ?3, ?4) ON CONFLICT(id) DO UPDATE SET print = ?1, clips = ?2, threshold = ?3, made = ?4",
        rusqlite::params![bytes, print.clips as i64, print.threshold as f64, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|e| e.to_string())?;
    *lock(&KNOWN) = Some(Some(print.clone()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, secs: f32) -> Vec<f32> {
        (0..(RATE as f32 * secs) as usize).map(|i| 0.3 * (std::f32::consts::TAU * freq * i as f32 / RATE as f32).sin()).collect()
    }

    #[test]
    fn the_filter_bank_has_kaldis_shape() {
        let rows = fbank(&tone(1000.0, 1.0));
        assert_eq!(rows.len(), 1 + (RATE - FRAME) / SHIFT, "a row every 10 ms");
        // The mean is taken off: every bin sums to nothing over the clip.
        for b in 0..MELS {
            let sum: f32 = rows.iter().map(|r| r[b]).sum();
            assert!(sum.abs() < 0.05, "bin {b} sums to {sum}");
        }
        // The filters: eighty triangles from 20 Hz to 8 kHz, each on its neighbours' shoulders.
        let f = filters();
        assert_eq!(f.len(), MELS);
        assert!(f.iter().all(|taps| !taps.is_empty() && taps.iter().all(|(k, w)| *k < FFT / 2 && *w > 0.0 && *w <= 1.0)));
        assert!(f[0][0].0 <= 2 && f[MELS - 1].last().unwrap().0 >= FFT / 2 - 3);
        assert!(fbank(&[0.0; 100]).is_empty(), "less than a frame is nothing");
    }

    /// Before the mean is taken off, a tone's energy sits in the bin of its pitch.
    #[test]
    fn a_tone_lands_in_its_mel_bin() {
        // Two tones one after the other: where the first is louder than the clip's mean, the
        // second is quieter, each in its own bin.
        let mut clip = tone(400.0, 0.5);
        clip.extend(tone(3000.0, 0.5));
        let rows = fbank(&clip);
        let loudest = |row: &[f32; MELS]| row.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        let (early, late) = (loudest(&rows[10]), loudest(&rows[rows.len() - 10]));
        assert!(early < late, "400 Hz in bin {early}, 3 kHz in bin {late}");
        let edge = |b: usize| 700.0 * (((mel(20.0) + (b + 1) as f32 * (mel(8000.0) - mel(20.0)) / (MELS + 1) as f32) / 1127.0).exp() - 1.0);
        assert!((edge(early) - 400.0).abs() < 60.0 && (edge(late) - 3000.0).abs() < 200.0, "{} Hz / {} Hz", edge(early), edge(late));
    }

    #[test]
    fn his_print_is_the_mean_of_his_clips_and_a_stranger_falls_below_it() {
        let unit = |v: Vec<f32>| normalised(v);
        // Three clips of one voice (close together), one of another (elsewhere).
        let his = [unit(vec![1.0, 0.1, 0.0, 0.05]), unit(vec![0.95, 0.2, 0.05, 0.0]), unit(vec![1.0, 0.0, 0.1, 0.1])];
        let stranger = unit(vec![0.1, 1.0, 0.3, 0.0]);
        let print = enrol(&his).expect("three clips make a print");
        assert_eq!(print.clips, 3);
        assert!((THRESHOLD_FLOOR..=THRESHOLD_CEILING).contains(&print.threshold));
        for clip in &his {
            assert!(print.is_his(clip, 2500).0);
        }
        let (ok, score) = print.is_his(&stranger, 2500);
        assert!(!ok && score < print.threshold, "a stranger scores {score}");
        // A short order passes a little lower than a sentence.
        let borderline = unit(vec![0.62, 0.78, 0.0, 0.0]);
        let score = likeness(&borderline, &print.print);
        let between = VoicePrint { threshold: score + 0.05, ..print.clone() };
        assert!(!between.is_his(&borderline, 2500).0 && between.is_his(&borderline, 600).0);
        assert!(enrol(&his[..2]).is_none(), "two clips are too few");
        assert_eq!(likeness(&his[0], &[1.0]), 0.0, "prints of different models never match");
    }

    #[test]
    fn the_print_survives_the_database() {
        let dir = std::env::temp_dir().join(format!("jai-voice-print-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("print.db");
        let db = db.to_string_lossy().to_string();
        *lock(&KNOWN) = None;
        assert_eq!(known(&db), None);
        let print = VoicePrint { print: normalised(vec![0.3, -0.2, 0.9]), clips: 6, threshold: 0.41 };
        keep(&db, &print).unwrap();
        *lock(&KNOWN) = None;
        assert_eq!(known(&db), Some(print.clone()));
        let second = VoicePrint { clips: 5, ..print };
        keep(&db, &second).unwrap();
        *lock(&KNOWN) = None;
        assert_eq!(known(&db).map(|p| p.clips), Some(5), "a new set-up replaces the old print");
        *lock(&KNOWN) = None;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
