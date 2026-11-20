//! Whose voice it is (Sir, 2026-11-12: "only obey me"). A clip of speech becomes a 256-number
//! voice print with the WeSpeaker ResNet34 model (`models/speaker`, on the processor, offline);
//! Sir's own print is the mean of the clips of his voice set-up and lives in the memory
//! database on this PC. With the voice lock on, a spoken order is carried out only when its
//! voice is close enough to that print.
//!
//! He learns the voice as Sir speaks (Sir, 2026-11-13: "while I am talking to him he learns my
//! voice"). The set-up's six read lines are one way of speaking; the day after it the lock
//! turned Sir's own sentences away at a likeness of 0.32 and 0.43 against a bar of 0.55
//! (jai.log, 2026-11-12 23:35). So:
//! * a print is made of the stretches of a clip that carry a voice, not of its silence;
//! * every sentence that is his joins a second print, the voice as it has grown since the
//!   set-up (`grown`), and a clip is as like him as it is like the nearer of the two;
//! * the bar starts no higher than 0.42 and rises (to 0.50 at most) only once his own
//!   sentences have shown how alike they are (`own`);
//! * for half a minute after a sentence that was surely his, one that falls a little short
//!   still passes: the same person goes on talking;
//! * without a set-up the print is gathered from the orders given to him by name.
//! Nothing is learnt from a voice that is not his, and what grows never leaves the voice of
//! the set-up (`ANCHOR_HOLD`).
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

/// The stretches of a clip that carry a voice: its 20 ms windows that stand clearly above the
/// room (the clip's quietest tenth) and are not far below the voice at its loudest, each with
/// 40 ms either side. What the ear cuts a sentence out with (the half second before it, the
/// pauses, the wait after it) says nothing about a voice and dilutes its print. The whole clip
/// when too little is left.
pub fn voiced(pcm: &[f32]) -> Vec<f32> {
    const WIN: usize = RATE / 50;
    if pcm.len() < WIN * 10 {
        return pcm.to_vec();
    }
    let levels: Vec<f32> = pcm.chunks(WIN).map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt()).collect();
    let mut sorted = levels.clone();
    sorted.sort_by(f32::total_cmp);
    let room = sorted[sorted.len() / 10].max(1e-5);
    let top = sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)];
    let gate = (room * 3.0).max(top * 0.08);
    let mut keep = vec![false; levels.len()];
    for (i, level) in levels.iter().enumerate() {
        if *level >= gate {
            for k in keep.iter_mut().take((i + 3).min(levels.len())).skip(i.saturating_sub(2)) {
                *k = true;
            }
        }
    }
    let out: Vec<f32> = pcm.chunks(WIN).zip(&keep).filter(|(_, k)| **k).flat_map(|(c, _)| c.iter().copied()).collect();
    if out.len() >= MIN_SAMPLES { out } else { pcm.to_vec() }
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
    print_of_samples(&voiced(pcm))
}

/// The print of the samples as they are (`print_of` takes the voiced stretches first).
pub fn print_of_samples(pcm: &[f32]) -> Result<Vec<f32>, String> {
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
    /// The voice of the set-up (or of the orders it was gathered from): it never changes
    /// without a new one.
    pub print: Vec<f32>,
    /// How many clips it was made of.
    pub clips: usize,
    /// A voice at least this like it is his (before his own sentences say more: `bar`).
    pub threshold: f32,
    /// His voice as it has shown itself since: the mean of the sentences that were his (unit
    /// length; empty before the first), weighted towards the latest `GROWN_WINDOW` of them.
    pub grown: Vec<f32>,
    /// How many sentences went into `grown`.
    pub grown_clips: usize,
    /// How alike his own sentences have been lately (their running mean; 0 before any).
    pub own: f32,
}

/// The least a voice must be like the print, whatever the set-up's clips say, and the most
/// the set-up may ask for. (Six lines read off the screen are closer to each other than to
/// what Sir says the next day: at 0.55 his own sentences were turned away.)
const THRESHOLD_FLOOR: f32 = 0.30;
const THRESHOLD_CEILING: f32 = 0.42;
/// The most the bar rises to once his own sentences have shown how alike they are.
const BAR_CEILING: f32 = 0.50;
/// The bar stands this far under the mean likeness of his own sentences …
const OWN_MARGIN: f32 = 0.28;
/// … once this many of them have been learnt from.
const SETTLED_AFTER: usize = 12;
/// A short order ("stop", "mute") says less about a voice: it passes this much lower.
const SHORT_MS: u32 = 1100;
const SHORT_ALLOWANCE: f32 = 0.10;
/// For this long after a sentence that was surely his, one that falls this far short of the
/// bar still passes: the same person goes on talking.
pub const TRUST_LASTS: std::time::Duration = std::time::Duration::from_secs(30);
const TRUST_ALLOWANCE: f32 = 0.10;
/// A sentence shorter than this teaches nothing about a voice.
const LEARN_MS: u32 = 1000;
/// The grown print weighs its latest sentences: this many count.
const GROWN_WINDOW: usize = 40;
/// Sentences by name that fall this far short of the bar are near misses; this many in a row
/// (within `NEAR_MISS_WITHIN`) and the bar goes back to the set-up's, to be learnt afresh: a
/// cold or another microphone must not lock Sir out for good.
const NEAR_MISS: f32 = 0.15;
const NEAR_MISSES: u32 = 3;
const NEAR_MISS_WITHIN: std::time::Duration = std::time::Duration::from_secs(180);

/// What the lock makes of a sentence's voice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Judged {
    /// It is taken for Sir's.
    pub his: bool,
    /// By its own likeness (not by the moment of trust after another sentence).
    pub sure: bool,
    /// How like his print it is, and how like it had to be.
    pub score: f32,
    pub bar: f32,
    /// A sentence of full length that passed by its own likeness with no allowance: only such
    /// a one teaches the print and opens the moment of trust.
    pub plain: bool,
    /// How like the voice of the set-up alone it is (not what has grown since).
    pub anchor: f32,
}

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
    Some(VoicePrint { threshold: (least - 0.25).clamp(THRESHOLD_FLOOR, THRESHOLD_CEILING), print, clips: clips.len(), grown: Vec::new(), grown_clips: 0, own: 0.0 })
}

impl VoicePrint {
    /// How like Sir's voice a clip's print is: like the set-up's print or like the voice as it
    /// has grown since, whichever is nearer.
    pub fn likeness_of(&self, clip: &[f32]) -> f32 {
        let anchor = likeness(clip, &self.print);
        if self.grown.is_empty() { anchor } else { anchor.max(likeness(clip, &self.grown)) }
    }

    /// How like his print a sentence must be: the set-up's bar, raised (never lowered) once his
    /// own sentences have shown how alike they are.
    pub fn bar(&self) -> f32 {
        if self.grown_clips >= SETTLED_AFTER && self.own > 0.0 { self.threshold.max((self.own - OWN_MARGIN).min(BAR_CEILING)) } else { self.threshold }
    }

    /// What the lock makes of a clip's print. `speech_ms`: how long he spoke (a short order
    /// passes a little lower); `trusted`: a sentence was surely his a moment ago.
    pub fn judge(&self, clip: &[f32], speech_ms: u32, trusted: bool) -> Judged {
        let score = self.likeness_of(clip);
        let full = self.bar();
        let bar = full - if speech_ms < SHORT_MS { SHORT_ALLOWANCE } else { 0.0 };
        let sure = score >= bar;
        Judged { his: sure || (trusted && score >= bar - TRUST_ALLOWANCE), sure, score, bar, plain: speech_ms >= SHORT_MS && score >= full, anchor: likeness(clip, &self.print) }
    }

    /// Whether a clip's print is Sir's voice by its own likeness, and how alike they are.
    #[cfg(test)]
    pub fn is_his(&self, clip: &[f32], speech_ms: u32) -> (bool, f32) {
        let judged = self.judge(clip, speech_ms, false);
        (judged.sure, judged.score)
    }

    /// Learns from a sentence that was surely his (`judged.plain`: full length, by its own
    /// likeness, no allowance) and that is itself like the voice of the set-up: it joins the
    /// grown print. A sentence that passed on trust, a short one, and one that is only like
    /// what has grown since teach nothing: what grows is made of sentences each of which the
    /// set-up's print would have let in, so it cannot wander to another voice. Whether
    /// anything was learnt.
    pub fn learn(&mut self, clip: &[f32], speech_ms: u32, judged: Judged) -> bool {
        if !judged.plain || speech_ms < LEARN_MS || clip.len() != self.print.len() {
            return false;
        }
        if judged.anchor < self.threshold {
            tracing::debug!("[VOICE] Not learnt from: like what has grown ({:.2}) but only {:.2} like the voice of the set-up (from {:.2})", judged.score, judged.anchor, self.threshold);
            return false;
        }
        let before = self.grown_clips.min(GROWN_WINDOW) as f32;
        let next = if self.grown.is_empty() { clip.to_vec() } else { self.grown.iter().zip(clip).map(|(g, c)| g * before + c).collect() };
        let next = normalised(next);
        self.grown = next;
        self.grown_clips += 1;
        self.own = if self.own <= 0.0 { judged.score } else { self.own * 0.9 + judged.score * 0.1 };
        true
    }
}

/// When a sentence was last surely Sir's.
static LAST_SURE: Mutex<Option<std::time::Instant>> = Mutex::new(None);
/// How many sentences the grown print has learnt from since it was last written down.
static UNSAVED: Mutex<(usize, Option<std::time::Instant>)> = Mutex::new((0, None));
/// The grown print is written down after this many sentences, or this long after the last.
const SAVE_EVERY: usize = 4;
const SAVE_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether a sentence was surely Sir's within the last half minute.
pub fn trusted_now() -> bool {
    lock(&LAST_SURE).is_some_and(|at| at.elapsed() < TRUST_LASTS)
}

/// A sentence's voice against Sir's print, and what is learnt from it: judged (with the moment
/// of trust), and when it is his, taken into the grown print. `keep`: write what was learnt
/// to the database now and then (never from a scripted session). None while no print is known.
pub fn hear(db: &str, clip: &[f32], speech_ms: u32, keep: bool) -> Option<Judged> {
    let mut print = known(db)?;
    // A print that cannot be compared with this clip (another length, a number that is none)
    // judges nothing: the voice is then unknown, not "not his".
    if clip.len() != print.print.len() || clip.iter().chain(print.print.iter()).any(|x| !x.is_finite()) {
        static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !SAID.swap(true, std::sync::atomic::Ordering::AcqRel) {
            tracing::warn!("[VOICE LOCK] The stored voice print cannot be compared with what the model gives now ({} against {} numbers): voices are not judged until the set-up is done again", print.print.len(), clip.len());
        }
        return None;
    }
    let judged = print.judge(clip, speech_ms, trusted_now());
    // On another PC Sir's own voice (shared from his PC) has every right too; nothing of it is
    // learnt into this PC's print.
    let judged = match lock(&SHARED_SIR).as_ref().filter(|sir| sir.print.len() == clip.len()).map(|sir| sir.judge(clip, speech_ms, false)) {
        Some(sir) if sir.sure && !judged.his => Judged { plain: false, ..sir },
        _ => judged,
    };
    // Only a full sentence that was surely his opens the moment of trust.
    if judged.plain {
        *lock(&LAST_SURE) = Some(std::time::Instant::now());
        *lock(&MISSES) = (0, None);
    }
    // While guests are welcome or one is being introduced, other voices speak: nothing is
    // learnt about Sir's.
    let others_speak = guests_welcome() || crate::kit_voice::guest_learning();
    if !others_speak && print.learn(clip, speech_ms, judged) {
        *lock(&KNOWN) = Some(Some(print.clone()));
        let due = {
            let mut unsaved = lock(&UNSAVED);
            unsaved.0 += 1;
            let due = unsaved.0 >= SAVE_EVERY || unsaved.1.is_none_or(|at| at.elapsed() > SAVE_AFTER);
            if due && keep {
                *unsaved = (0, Some(std::time::Instant::now()));
            }
            due
        };
        if due && keep {
            if let Err(e) = keep_grown(db, &print) {
                tracing::warn!("[VOICE] What was learnt of Sir's voice was not written down: {e}");
            }
        }
    }
    Some(judged)
}

/// Near misses by name in a row, and when the last was.
static MISSES: Mutex<(u32, Option<std::time::Instant>)> = Mutex::new((0, None));

/// The lock turned a sentence away. `named`: it was an order by his name. Sentences by name
/// that fall just short, three in a row, mean the bar stands too high for Sir's voice as it is
/// today (a cold, another microphone): it goes back to the set-up's and is learnt afresh from
/// his next sentences. Whether it did.
pub fn turned_away(db: &str, judged: Judged, named: bool, keep: bool) -> bool {
    if !named || judged.score < judged.bar - NEAR_MISS {
        return false;
    }
    let reached = {
        let mut misses = lock(&MISSES);
        let fresh = misses.1.is_none_or(|at| at.elapsed() > NEAR_MISS_WITHIN);
        *misses = (if fresh { 1 } else { misses.0 + 1 }, Some(std::time::Instant::now()));
        misses.0 >= NEAR_MISSES
    };
    if !reached {
        return false;
    }
    *lock(&MISSES) = (0, None);
    let Some(mut print) = known(db) else { return false };
    if print.bar() <= print.threshold {
        return false;
    }
    let was = print.bar();
    print.own = 0.0;
    tracing::info!("[VOICE LOCK] Three orders by name fell just short of {was:.2}: the bar goes back to the set-up's {:.2} and follows Sir's voice afresh", print.threshold);
    *lock(&KNOWN) = Some(Some(print.clone()));
    if keep {
        let _ = keep_grown(db, &print);
    }
    true
}

// ------------------------------------------------------------------------------ guests ----

/// Until when he answers to anyone although the lock is on ("guest mode", "let my friend
/// speak for half an hour"). Not kept across a restart: a guest's welcome ends with the day.
static GUESTS_UNTIL: Mutex<Option<std::time::Instant>> = Mutex::new(None);

pub fn welcome_guests(span: std::time::Duration) {
    *lock(&GUESTS_UNTIL) = Some(std::time::Instant::now() + span);
    tracing::info!("[VOICE LOCK] Guests are welcome for {} min", span.as_secs() / 60);
}

pub fn end_guests() {
    if lock(&GUESTS_UNTIL).take().is_some() {
        tracing::info!("[VOICE LOCK] Guests are no longer answered");
    }
}

/// Whether the guests' time has just run out (once per welcome): the moment to say so.
pub fn guests_just_left() -> bool {
    let mut until = lock(&GUESTS_UNTIL);
    if until.is_some_and(|at| std::time::Instant::now() >= at) {
        *until = None;
        tracing::info!("[VOICE LOCK] The guests' time is up: he answers to Sir alone again");
        return true;
    }
    false
}

/// Whether he answers to anyone right now on Sir's word.
pub fn guests_welcome() -> bool {
    lock(&GUESTS_UNTIL).is_some_and(|until| std::time::Instant::now() < until)
}

/// Until what time of day guests are welcome ("21:40"), while they are.
pub fn guests_until() -> Option<String> {
    let until = (*lock(&GUESTS_UNTIL))?;
    let left = until.checked_duration_since(std::time::Instant::now())?;
    Some((chrono::Local::now() + chrono::Duration::from_std(left).ok()?).format("%H:%M").to_string())
}

// ------------------------------------------------------------------- guests he knows ----

/// A guest Sir introduced by name ("this is my friend Tom, remember his voice").
#[derive(Clone, Debug, PartialEq)]
pub struct Guest {
    pub name: String,
    pub print: Vec<f32>,
    pub clips: usize,
    /// What the guest may ask for: "everything", "music" (players and volume only) or
    /// "questions" (nothing else). Its account holds what it may since accounts came.
    pub may: String,
}

/// A voice is a known guest's from this likeness to the guest's print.
pub const GUEST_BAR: f32 = 0.45;
/// A guest's print is made of this many sentences.
pub const GUEST_CLIPS: usize = 3;

static GUESTS: Mutex<Option<Vec<Guest>>> = Mutex::new(None);

fn open_guests(db: &str) -> rusqlite::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS voice_guests (name TEXT PRIMARY KEY, print BLOB NOT NULL, clips INTEGER NOT NULL, may TEXT NOT NULL, made TEXT NOT NULL);")?;
    Ok(conn)
}

/// The guests he knows: those kept on this PC, and the voices of the accounts' PCs shared with
/// it (`share_guest`).
pub fn guests(db: &str) -> Vec<Guest> {
    let mut all = kept_guests(db);
    for shared in lock(&SHARED_GUESTS).iter() {
        if !all.iter().any(|g| g.name.eq_ignore_ascii_case(&shared.name)) {
            all.push(shared.clone());
        }
    }
    all
}

/// The guests kept on this PC (read from the database once, then from here).
fn kept_guests(db: &str) -> Vec<Guest> {
    let mut known = lock(&GUESTS);
    if known.is_none() {
        let read = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()
            .and_then(|conn| {
                let mut rows = conn.prepare("SELECT name, print, clips, may FROM voice_guests ORDER BY made").ok()?;
                let read = rows.query_map([], |r| Ok(Guest { name: r.get(0)?, print: floats(&r.get::<_, Vec<u8>>(1)?), clips: r.get::<_, i64>(2)? as usize, may: r.get(3)? })).ok()?;
                Some(read.flatten().collect::<Vec<Guest>>())
            })
            .unwrap_or_default();
        *known = Some(read);
    }
    known.clone().unwrap_or_default()
}

/// Keeps a guest (a guest of that name is replaced). `keep`: in the database too.
pub fn keep_guest(db: &str, guest: &Guest, keep: bool) -> Result<(), String> {
    if keep {
        let conn = open_guests(db).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO voice_guests (name, print, clips, may, made) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(name) DO UPDATE SET print = ?2, clips = ?3, may = ?4",
            rusqlite::params![guest.name, bytes(&guest.print), guest.clips as i64, guest.may, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
    }
    let mut all = kept_guests(db);
    all.retain(|g| !g.name.eq_ignore_ascii_case(&guest.name));
    all.push(guest.clone());
    *lock(&GUESTS) = Some(all);
    Ok(())
}

/// Forgets a guest; whether there was one of that name.
pub fn forget_guest(db: &str, name: &str, keep: bool) -> bool {
    let mut all = kept_guests(db);
    let before = all.len();
    all.retain(|g| !g.name.eq_ignore_ascii_case(name));
    let gone = all.len() != before;
    *lock(&GUESTS) = Some(all);
    if gone && keep {
        if let Ok(conn) = open_guests(db) {
            let _ = conn.execute("DELETE FROM voice_guests WHERE lower(name) = lower(?1)", [name]);
        }
    }
    gone
}

/// The known guest whose voice a clip's print is, and how alike they are.
pub fn guest_of(db: &str, clip: &[f32]) -> Option<(Guest, f32)> {
    guests(db).into_iter().map(|g| { let score = likeness(clip, &g.print); (g, score) }).filter(|(_, score)| *score >= GUEST_BAR).max_by(|a, b| a.1.total_cmp(&b.1))
}

/// A guest's print from the sentences he or she said for it: their mean.
pub fn guest_print(name: &str, clips: &[Vec<f32>]) -> Option<Guest> {
    if clips.len() < GUEST_CLIPS || clips.iter().any(|c| c.len() != clips[0].len() || c.is_empty()) {
        return None;
    }
    let mut mean = vec![0.0f32; clips[0].len()];
    for clip in clips {
        for (m, v) in mean.iter_mut().zip(clip) {
            *m += v;
        }
    }
    // (A new voice asks questions only, until Sir says what else: his word of 2026-11-20.)
    Some(Guest { name: name.to_string(), print: normalised(mean), clips: clips.len(), may: "questions".to_string() })
}

// ------------------------------------------------------- voices shared between PCs ----
// (`fleet`.) Held here and never written down: each print is kept on disk on its owner's PC
// alone. On another PC: Sir's own print. On Sir's PC: the voices of the accounts' PCs.

static SHARED_SIR: Mutex<Option<VoicePrint>> = Mutex::new(None);
static SHARED_GUESTS: Mutex<Vec<Guest>> = Mutex::new(Vec::new());

/// This PC's own print (its user's voice), to be shared.
pub fn print_to_share(db: &str) -> Option<serde_json::Value> {
    let p = known(db)?;
    Some(serde_json::json!({ "print": p.print, "threshold": p.threshold }))
}

fn shared_print(v: &serde_json::Value) -> Option<VoicePrint> {
    let print: Vec<f32> = serde_json::from_value(v["print"].clone()).ok()?;
    let threshold = v["threshold"].as_f64()? as f32;
    (!print.is_empty() && print.iter().all(|x| x.is_finite())).then(|| VoicePrint { print, clips: 3, threshold: threshold.clamp(THRESHOLD_FLOOR, THRESHOLD_CEILING), grown: Vec::new(), grown_clips: 0, own: 0.0 })
}

/// On another PC: Sir's voice, from his PC (None: not known there).
pub fn share_sir(v: Option<&serde_json::Value>) {
    *lock(&SHARED_SIR) = v.and_then(shared_print);
}

/// On Sir's PC: the voice of an account's PC, by the account's name (its account says what it
/// may).
pub fn share_guest(name: &str, v: &serde_json::Value) {
    let Some(p) = shared_print(v) else { return };
    let mut all = lock(&SHARED_GUESTS);
    all.retain(|g| !g.name.eq_ignore_ascii_case(name));
    all.push(Guest { name: name.to_string(), print: p.print, clips: 3, may: "questions".into() });
}

// ------------------------------------------------------------ a print without a set-up ----

/// The prints of the orders given to him by name while no print is known.
static GATHERED: Mutex<Vec<Vec<f32>>> = Mutex::new(Vec::new());
/// This many are looked at together …
const GATHER_ENOUGH: usize = 8;
/// … two of them are one voice when they are this alike …
const PAIR_ALIKE: f32 = 0.35;
/// … and this many must be one voice for a print.
const GATHER_KEEP: usize = 6;
/// No more than this are held (the oldest goes).
const GATHER_MOST: usize = 14;

/// The print of the clips that are one voice: the clip most of the others are like, and those
/// that are like it. (Likeness to the mean of all would not do: the mean of two voices is 0.7
/// like each of them.) None when too few agree.
pub fn agreed(clips: &[Vec<f32>]) -> Option<VoicePrint> {
    if clips.len() < GATHER_ENOUGH || clips.iter().any(|c| c.len() != clips[0].len() || c.is_empty()) {
        return None;
    }
    let company = |i: usize| -> Vec<usize> { (0..clips.len()).filter(|j| *j == i || likeness(&clips[i], &clips[*j]) >= PAIR_ALIKE).collect() };
    let seed = (0..clips.len()).max_by_key(|i| company(*i).len())?;
    let members = company(seed);
    if members.len() < GATHER_KEEP {
        return None;
    }
    let alike: Vec<Vec<f32>> = members.iter().map(|i| clips[*i].clone()).collect();
    enrol(&alike)
}

/// An order given to him by name while no print is known: its voice is gathered, and once
/// enough of them agree they become Sir's print (kept in the database when `keep`). The print,
/// the moment it is made.
pub fn gather(db: &str, clip: Vec<f32>, speech_ms: u32, keep_it: bool) -> Option<VoicePrint> {
    if speech_ms < LEARN_MS || known(db).is_some() {
        return None;
    }
    let mut gathered = lock(&GATHERED);
    gathered.push(clip);
    while gathered.len() > GATHER_MOST {
        gathered.remove(0);
    }
    let print = agreed(&gathered)?;
    gathered.clear();
    drop(gathered);
    if keep_it {
        if let Err(e) = keep(db, &print) {
            tracing::warn!("[VOICE] The print gathered from Sir's orders was not kept: {e}");
            return None;
        }
    } else {
        *lock(&KNOWN) = Some(Some(print.clone()));
    }
    Some(print)
}

// ------------------------------------------------------------------------- the database ----

fn open(db: &str) -> rusqlite::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS voice_print (id INTEGER PRIMARY KEY CHECK (id = 1), print BLOB NOT NULL, clips INTEGER NOT NULL, threshold REAL NOT NULL, made TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS voice_grown (id INTEGER PRIMARY KEY CHECK (id = 1), print BLOB NOT NULL, clips INTEGER NOT NULL, own REAL NOT NULL, updated TEXT NOT NULL);",
    )?;
    Ok(conn)
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

fn bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Sir's print as it is kept (read from the database once, then from here).
static KNOWN: Mutex<Option<Option<VoicePrint>>> = Mutex::new(None);

/// Sir's voice print, if the set-up has been done.
pub fn known(db: &str) -> Option<VoicePrint> {
    let mut known = lock(&KNOWN);
    if known.is_none() {
        // (Read only: looking for a print never writes to the database.)
        // (A database that cannot be read right now is asked again the next time: "no print"
        // is only remembered when the database said so.)
        let conn = match rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(conn) => conn,
            Err(_) => return None,
        };
        let stored = conn.query_row("SELECT print, clips, threshold, made FROM voice_print WHERE id = 1", [], |r| {
            let raw: Vec<u8> = r.get(0)?;
            let made: String = r.get(3)?;
            // (A bar from before 2026-11-13 may stand higher than a set-up may ask for.)
            Ok((VoicePrint { print: floats(&raw), clips: r.get::<_, i64>(1)? as usize, threshold: (r.get::<_, f64>(2)? as f32).min(THRESHOLD_CEILING), grown: Vec::new(), grown_clips: 0, own: 0.0 }, made))
        });
        let stored = match stored {
            // A print from before clips were cut to their voiced stretches was made of other
            // stuff than what it would be compared with now, and the lock is on by default
            // since: such a print is set aside (it stays in the database until a new one
            // replaces it), and Sir's voice is gathered afresh from his orders by name, or
            // read in with "learn my voice". Until then he answers to his name, as before.
            Ok((_, made)) if made_before_trimming(&made) => {
                if !SET_ASIDE.swap(true, std::sync::atomic::Ordering::AcqRel) {
                    tracing::info!("[VOICE LOCK] The stored voice print ({made}) is from before clips were trimmed to their voiced stretches: it is set aside, and Sir's voice is learnt afresh from his orders by name");
                    if !crate::platform::in_test_session() {
                        crate::hud_notify::record("Voice lock: my print of your voice is from an older build. I am learning your voice afresh from the orders you give me by name; \"learn my voice\" does it in a minute.", false);
                    }
                }
                None
            }
            Ok((print, _)) => Some(print),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) if e.to_string().contains("no such table") => None,
            Err(e) => {
                tracing::warn!("[VOICE] Sir's voice print could not be read just now: {e}");
                return None;
            }
        };
        let read = stored.and_then(|mut print| {
            // (What has grown since: absent before the first sentence, and in an older database.)
            if let Ok((grown, clips, own)) = conn.query_row("SELECT print, clips, own FROM voice_grown WHERE id = 1", [], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?))) {
                let grown = floats(&grown);
                if grown.len() == print.print.len() {
                    (print.grown, print.grown_clips, print.own) = (grown, clips as usize, own as f32);
                }
            }
            Some(print)
        });
        *known = Some(read.filter(|p| !p.print.is_empty()));
    }
    known.clone().flatten()
}

/// Prints made before this moment came from whole clips, silence and all (`print_of` cuts a
/// clip to its voiced stretches since).
const TRIMMED_SINCE: &str = "2026-11-13T05:00:00+00:00";
/// A stored print was set aside as older than that (said once, and told when he is asked).
static SET_ASIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether an older print lies set aside in the database (the privacy board can delete it).
pub fn set_aside() -> bool {
    SET_ASIDE.load(std::sync::atomic::Ordering::Acquire)
}

fn made_before_trimming(made: &str) -> bool {
    match (chrono::DateTime::parse_from_rfc3339(made), chrono::DateTime::parse_from_rfc3339(TRIMMED_SINCE)) {
        (Ok(made), Ok(since)) => made < since,
        _ => false,
    }
}

/// Keeps Sir's print (replacing an older one, and with it what had grown from that one).
pub fn keep(db: &str, print: &VoicePrint) -> Result<(), String> {
    let conn = open(db).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO voice_print (id, print, clips, threshold, made) VALUES (1, ?1, ?2, ?3, ?4) ON CONFLICT(id) DO UPDATE SET print = ?1, clips = ?2, threshold = ?3, made = ?4",
        rusqlite::params![bytes(&print.print), print.clips as i64, print.threshold as f64, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM voice_grown", []).map_err(|e| e.to_string())?;
    *lock(&KNOWN) = Some(Some(VoicePrint { grown: Vec::new(), grown_clips: 0, own: 0.0, ..print.clone() }));
    *lock(&LAST_SURE) = None;
    SET_ASIDE.store(false, std::sync::atomic::Ordering::Release);
    Ok(())
}

/// Forgets Sir's voice: the print of the set-up and what grew from it (the privacy board).
pub fn forget(db: &str) -> Result<(), String> {
    let conn = open(db).map_err(|e| e.to_string())?;
    conn.execute_batch("DELETE FROM voice_print; DELETE FROM voice_grown;").map_err(|e| e.to_string())?;
    *lock(&KNOWN) = Some(None);
    *lock(&LAST_SURE) = None;
    SET_ASIDE.store(false, std::sync::atomic::Ordering::Release);
    lock(&GATHERED).clear();
    tracing::info!("[VOICE] Sir's voice print is forgotten on his word");
    Ok(())
}

/// Writes down what Sir's voice has grown into since the set-up.
pub fn keep_grown(db: &str, print: &VoicePrint) -> Result<(), String> {
    if print.grown.is_empty() {
        return Ok(());
    }
    let conn = open(db).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO voice_grown (id, print, clips, own, updated) VALUES (1, ?1, ?2, ?3, ?4) ON CONFLICT(id) DO UPDATE SET print = ?1, clips = ?2, own = ?3, updated = ?4",
        rusqlite::params![bytes(&print.grown), print.grown_clips as i64, print.own as f64, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// What he knows of Sir's voice, in a sentence (for "what have you learnt about my voice").
pub fn known_in_words(db: &str) -> String {
    match known(db) {
        None if SET_ASIDE.load(std::sync::atomic::Ordering::Acquire) => "I have set aside my older print of your voice and am learning it afresh from your orders by name".to_string(),
        None => "I have no print of your voice yet".to_string(),
        Some(p) if p.grown_clips == 0 => format!("I know your voice from {} sentences of the set-up", p.clips),
        Some(p) => format!("I know your voice from {} sentences of the set-up and {} more you have said to me since", p.clips, p.grown_clips),
    }
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

    fn unit(v: Vec<f32>) -> Vec<f32> {
        normalised(v)
    }

    /// A print as the set-up of 2026-11-12 left it: six read lines close together.
    fn setup_print() -> VoicePrint {
        enrol(&[unit(vec![1.0, 0.05, 0.0, 0.0]), unit(vec![1.0, -0.05, 0.0, 0.0]), unit(vec![1.0, 0.0, 0.05, 0.0]), unit(vec![1.0, 0.0, -0.05, 0.0])]).expect("a print")
    }

    #[test]
    fn the_bar_starts_low_and_sirs_own_sentences_pass_it() {
        let print = setup_print();
        // The clips of a set-up are close to their mean: the bar is the ceiling, and that is
        // 0.42, not 0.55.
        assert!((print.threshold - THRESHOLD_CEILING).abs() < 1e-6, "{}", print.threshold);
        // 23:35:41 on 2026-11-12: a sentence of Sir's at a likeness of 0.43 was turned away.
        let his_next_day = unit(vec![0.43, 0.9028, 0.0, 0.0]);
        let judged = print.judge(&his_next_day, 2900, false);
        assert!((judged.score - 0.43).abs() < 0.01 && judged.his && judged.sure, "{judged:?}");
        // 23:35:32: one at 0.32 falls short by itself, and passes in the half minute after a
        // sentence that was surely his.
        let weaker = unit(vec![0.33, 0.944, 0.0, 0.0]);
        assert!(!print.judge(&weaker, 4300, false).his);
        let on_trust = print.judge(&weaker, 4300, true);
        assert!(on_trust.his && !on_trust.sure, "{on_trust:?}");
        // A stranger stays out, trust or not.
        let stranger = unit(vec![0.1, 0.3, 0.95, 0.0]);
        assert!(!print.judge(&stranger, 2500, true).his);
    }

    #[test]
    fn his_voice_is_learnt_as_he_speaks() {
        let mut print = setup_print();
        // How he speaks every day: a way off the read lines, but his.
        let everyday = |k: usize| unit(vec![0.62, 0.74 + 0.01 * (k % 3) as f32, 0.2, 0.05]);
        let first = print.judge(&everyday(0), 2400, false);
        assert!(first.sure && first.score < 0.7, "{first:?}");
        for k in 0..14 {
            let clip = everyday(k);
            let judged = print.judge(&clip, 2400, false);
            assert!(judged.his, "sentence {k}: {judged:?}");
            assert!(print.learn(&clip, 2400, judged));
        }
        // He is known by his everyday voice now: the same sentence is far more like him …
        let later = print.judge(&everyday(1), 2400, false);
        assert!(later.score > 0.95 && later.score > first.score + 0.25, "{later:?}");
        assert_eq!(print.grown_clips, 14);
        // … the bar has risen with it, but never over its ceiling …
        assert!(print.bar() > print.threshold && print.bar() <= BAR_CEILING, "{}", print.bar());
        // … and a stranger is no nearer than before.
        let stranger = unit(vec![0.05, -0.3, 0.95, 0.0]);
        assert!(!print.judge(&stranger, 2500, true).his);
        // Nothing is learnt from a short word, from a voice that is not his, or from one that
        // would lead away from the voice of the set-up.
        let before = print.clone();
        let judged = print.judge(&everyday(2), 600, false);
        assert!(!print.learn(&everyday(2), 600, judged), "too short");
        let judged = print.judge(&stranger, 2500, false);
        assert!(!print.learn(&stranger, 2500, judged), "not his");
        let mut fresh = setup_print();
        let far = unit(vec![0.3, 0.2, 0.93, 0.0]);
        assert!(!fresh.learn(&far, 2500, Judged { his: true, sure: true, score: 0.5, bar: 0.4, plain: true, anchor: 0.2 }), "like what has grown, but not like the voice of the set-up");
        assert_eq!(print, before);
        // A sentence that only passed in the moment of trust after another teaches nothing,
        // and opens no trust of its own: what grows is made of sentences that were surely his.
        let mut fresh = setup_print();
        let weaker = unit(vec![0.33, 0.944, 0.0, 0.0]);
        let on_trust = fresh.judge(&weaker, 4300, true);
        assert!(on_trust.his && !on_trust.plain && !fresh.learn(&weaker, 4300, on_trust), "{on_trust:?}");
        // A short order that passed with the short allowance is his, and teaches nothing either.
        let short = fresh.judge(&unit(vec![0.36, 0.933, 0.0, 0.0]), 800, false);
        assert!(!short.plain, "{short:?}");
        // The sentence of the log, 0.43 like the set-up: his by itself, and the first one learnt
        // from (it was not: the grown print had to be 0.50 like the set-up from its first clip).
        let his_next_day = unit(vec![0.43, 0.9028, 0.0, 0.0]);
        let judged = fresh.judge(&his_next_day, 2900, false);
        assert!(judged.plain && fresh.learn(&his_next_day, 2900, judged) && fresh.grown_clips == 1, "{judged:?}");
    }

    #[test]
    fn a_print_from_before_the_trimming_is_set_aside() {
        assert!(made_before_trimming("2026-11-12T22:35:15.378922200+00:00"), "the print of the evening before");
        assert!(!made_before_trimming("2026-11-13T06:50:00+00:00"));
        assert!(!made_before_trimming(&chrono::Utc::now().to_rfc3339()), "what is kept now is kept");
        assert!(!made_before_trimming("no date at all"), "a date that cannot be read sets nothing aside");
    }

    #[test]
    fn a_print_is_gathered_from_orders_that_agree() {
        let his = |k: usize| unit(vec![1.0, 0.1 + 0.02 * k as f32, 0.0, 0.05]);
        let other = unit(vec![0.0, 0.2, 1.0, 0.1]);
        let mut clips: Vec<Vec<f32>> = (0..6).map(his).collect();
        assert!(agreed(&clips).is_none(), "six are too few to look at");
        clips.push(other.clone());
        clips.push(his(6));
        let print = agreed(&clips).expect("seven of eight agree");
        assert_eq!(print.clips, 7, "the other voice is left out");
        assert!(print.judge(&his(3), 2000, false).his && !print.judge(&other, 2000, false).his);
        // Two voices by turns agree on nothing.
        let mixed: Vec<Vec<f32>> = (0..8).map(|k| if k % 2 == 0 { his(k) } else { other.clone() }).collect();
        assert!(agreed(&mixed).is_none());
    }

    #[test]
    fn a_guest_is_known_by_name_and_forgotten_on_sirs_word() {
        let dir = std::env::temp_dir().join(format!("jai-voice-guests-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("guests.db").to_string_lossy().to_string();
        *lock(&GUESTS) = None;
        assert!(guests(&db).is_empty());
        let said = |k: usize| unit(vec![0.1, 1.0, 0.05 * k as f32, 0.0]);
        assert!(guest_print("Tom", &[said(0), said(1)]).is_none(), "two sentences are too few");
        let tom = guest_print("Tom", &[said(0), said(1), said(2)]).expect("three make a print");
        assert_eq!((tom.name.as_str(), tom.clips, tom.may.as_str()), ("Tom", 3, "questions"), "a new voice may only ask");
        keep_guest(&db, &tom, true).unwrap();
        keep_guest(&db, &Guest { may: "music".into(), ..tom.clone() }, true).unwrap();
        *lock(&GUESTS) = None;
        let read = guests(&db);
        assert_eq!(read.len(), 1, "a guest of the same name is replaced");
        assert_eq!(read[0].may, "music");
        // His voice is his; another is nobody's.
        let (who, score) = guest_of(&db, &said(1)).expect("Tom's voice");
        assert!(who.name == "Tom" && score > 0.9);
        assert!(guest_of(&db, &unit(vec![1.0, 0.0, 0.0, 0.3])).is_none());
        assert!(forget_guest(&db, "tom", true) && !forget_guest(&db, "tom", true));
        *lock(&GUESTS) = None;
        assert!(guests(&db).is_empty());
        *lock(&GUESTS) = None;
        let _ = std::fs::remove_dir_all(&dir);
        // Until when guests are welcome is a time of day while they are.
        end_guests();
        assert_eq!(guests_until(), None);
        welcome_guests(std::time::Duration::from_secs(600));
        assert!(guests_until().is_some_and(|t| t.len() == 5 && t.contains(':')));
        end_guests();
    }

    #[test]
    fn the_voiced_stretches_are_what_a_print_is_made_of() {
        let hush: Vec<f32> = (0..RATE / 2).map(|i| 0.001 * ((i * 7919) % 13) as f32 / 13.0).collect();
        let mut clip = hush.clone();
        clip.extend(tone(220.0, 1.2));
        clip.extend(hush.iter().copied());
        clip.extend(tone(330.0, 0.8));
        clip.extend(hush.iter().copied());
        let kept = voiced(&clip);
        // The two seconds of voice stay (with their edges), the 1.5 s of room go.
        let secs = kept.len() as f32 / RATE as f32;
        assert!((1.95..2.4).contains(&secs), "{secs} s kept of {} s", clip.len() as f32 / RATE as f32);
        // Speech from the first sample to the last is kept whole; a blip too short for a print
        // comes back as it was.
        let steady = tone(200.0, 1.5);
        assert_eq!(voiced(&steady).len(), steady.len());
        let mut blip = vec![0.0f32; RATE];
        blip.extend(tone(300.0, 0.2));
        assert_eq!(voiced(&blip).len(), blip.len());
    }

    #[test]
    fn the_print_survives_the_database() {
        let dir = std::env::temp_dir().join(format!("jai-voice-print-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("print.db");
        let db = db.to_string_lossy().to_string();
        *lock(&KNOWN) = None;
        assert_eq!(known(&db), None);
        let print = VoicePrint { print: normalised(vec![0.3, -0.2, 0.9]), clips: 6, threshold: 0.41, grown: Vec::new(), grown_clips: 0, own: 0.0 };
        keep(&db, &print).unwrap();
        *lock(&KNOWN) = None;
        assert_eq!(known(&db), Some(print.clone()));
        // What has grown since is kept beside it and read back with it.
        let grown = VoicePrint { grown: normalised(vec![0.35, -0.1, 0.9]), grown_clips: 9, own: 0.71, ..print.clone() };
        keep_grown(&db, &grown).unwrap();
        *lock(&KNOWN) = None;
        assert_eq!(known(&db), Some(grown.clone()));
        assert!(known_in_words(&db).contains("9 more"));
        // A new set-up replaces the old print, and what grew from that one goes with it.
        let second = VoicePrint { clips: 5, ..grown };
        keep(&db, &second).unwrap();
        *lock(&KNOWN) = None;
        let read = known(&db).expect("the second print");
        assert_eq!((read.clips, read.grown_clips, read.grown.len()), (5, 0, 0), "a new set-up replaces the old print");
        // A bar kept before the ceiling was lowered is read at the ceiling.
        rusqlite::Connection::open(&db).unwrap().execute("UPDATE voice_print SET threshold = 0.55", []).unwrap();
        *lock(&KNOWN) = None;
        assert!((known(&db).unwrap().threshold - THRESHOLD_CEILING).abs() < 1e-6);
        *lock(&KNOWN) = None;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
