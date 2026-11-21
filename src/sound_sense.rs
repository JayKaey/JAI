//! What a sound is (Sir, 2026-11-21: "JAI should be really good in detecting what kind of noises
//! there are … if I clap or if someone knocks or if I snap my fingers"). The acoustic radar
//! notices that something sounded within milliseconds; CED-mini names it: an audio tagger trained
//! on AudioSet's 527 kinds of sound (`models/sound`, 10 MB, on the processor, offline, a few
//! milliseconds a clip). So typing or a tap on the desk is no knock, a snap is no clap, and the
//! doorbell, an alarm, the phone, a dog, breaking glass and whistling are known for what they
//! are. Whether a voice is Sir's or someone else's is his voice print's (`speaker_id`).
//!
//! The features are the ones the model was trained on (sherpa-onnx's CED front end): 16 kHz,
//! 32 ms Hann frames every 10 ms, centred (the clip mirrored at its edges), a 512-point power
//! spectrum, 64 Kaldi mel bins up to 8 kHz, in decibels, none more than 120 below the loudest.
//!
//! Sir's rules and protocols wait on a `Sound` ("whenever I clap twice, …", "when you hear
//! knocking, tell me"); the protocol window shows what was heard last and what it was taken for.

use crate::acoustic_radar::AcousticEvent;
use ort::session::Session;
use ort::value::Tensor;
use regex::Regex;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The model's file and its list of the kinds of sound (in `models/sound`).
pub const MODEL: &str = "model.int8.onnx";
pub const LABELS: &str = "class_labels_indices.csv";

const RATE: usize = 16_000;
const FRAME: usize = 512;
const SHIFT: usize = 160;
const MELS: usize = 64;
/// The kinds of sound the model knows.
pub const CLASSES: usize = 527;
/// The longest clip that is looked at (the radar keeps the last three seconds).
const LONGEST: usize = RATE * 6;

// AudioSet's kinds, by their place in the model's answer.
const CLAP: &[usize] = &[61, 63, 67, 467]; // hands, clapping, applause, slap
const SNAP: &[usize] = &[62]; // finger snapping
const KNOCK: &[usize] = &[354, 359, 360, 468]; // door, knock, tap (at a door), whack
const TYPING: &[usize] = &[384, 385, 386]; // typing, typewriter, computer keyboard
const MUSIC: &[usize] = &[137];
const SHATTER: usize = 443;
/// What a sound that lasts may be, and how sure the model must be of it.
const LASTING: &[(Sound, &[usize], f32)] = &[
    (Sound::Doorbell, &[355, 356], 0.2),
    (Sound::Alarm, &[388, 395, 399, 400], 0.25),
    (Sound::Phone, &[389, 390, 391], 0.25),
    (Sound::Dog, &[74, 75], 0.3),
    (Sound::Glass, &[SHATTER], 0.25),
    (Sound::Whistle, &[40], 0.3),
];

/// A sound a rule or a protocol can wait on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sound {
    /// Two or three claps.
    Claps(u8),
    Snap,
    Knock,
    Doorbell,
    Alarm,
    Phone,
    Dog,
    Glass,
    Whistle,
    /// A voice that is clearly not Sir's.
    SomeoneElse,
}

impl Sound {
    /// Every one, in the order the protocol window offers them.
    pub const ALL: [Sound; 11] = [Sound::Claps(2), Sound::Claps(3), Sound::Snap, Sound::Knock, Sound::Doorbell, Sound::Phone, Sound::Alarm, Sound::Dog, Sound::Glass, Sound::Whistle, Sound::SomeoneElse];

    /// How a rule or a protocol's line says it ("whenever I clap twice").
    pub fn words(self) -> &'static str {
        match self {
            Sound::Claps(3) => "I clap three times",
            Sound::Claps(_) => "I clap twice",
            Sound::Snap => "I snap my fingers",
            Sound::Knock => "someone knocks",
            Sound::Doorbell => "the doorbell rings",
            Sound::Alarm => "an alarm goes off",
            Sound::Phone => "the phone rings",
            Sound::Dog => "a dog barks",
            Sound::Glass => "glass breaks",
            Sound::Whistle => "I whistle",
            Sound::SomeoneElse => "someone else talks",
        }
    }

    /// What it is called among what was heard.
    pub fn name(self) -> &'static str {
        match self {
            Sound::Claps(3) => "three claps",
            Sound::Claps(_) => "two claps",
            Sound::Snap => "a finger snap",
            Sound::Knock => "knocking",
            Sound::Doorbell => "the doorbell",
            Sound::Alarm => "an alarm",
            Sound::Phone => "a phone ringing",
            Sound::Dog => "a dog barking",
            Sound::Glass => "glass breaking",
            Sound::Whistle => "whistling",
            Sound::SomeoneElse => "someone else talking",
        }
    }

    /// What J.A.I. says when he was asked to tell Sir.
    pub fn news(self) -> &'static str {
        match self {
            Sound::Claps(3) => "You clapped three times, Sir.",
            Sound::Claps(_) => "You clapped twice, Sir.",
            Sound::Snap => "You snapped your fingers, Sir.",
            Sound::Knock => "Sir, someone is knocking.",
            Sound::Doorbell => "Sir, the doorbell is ringing.",
            Sound::Alarm => "Sir, an alarm is going off.",
            Sound::Phone => "Sir, your phone is ringing.",
            Sound::Dog => "Sir, a dog is barking.",
            Sound::Glass => "Sir, I heard glass breaking.",
            Sound::Whistle => "You whistled, Sir.",
            Sound::SomeoneElse => "Sir, someone else is talking.",
        }
    }

    /// Its name where it is kept (a rule's JSON).
    pub fn key(self) -> &'static str {
        match self {
            Sound::Claps(3) => "clap3",
            Sound::Claps(_) => "clap2",
            Sound::Snap => "snap",
            Sound::Knock => "knock",
            Sound::Doorbell => "doorbell",
            Sound::Alarm => "alarm",
            Sound::Phone => "phone",
            Sound::Dog => "dog",
            Sound::Glass => "glass",
            Sound::Whistle => "whistle",
            Sound::SomeoneElse => "someone_else",
        }
    }

    pub fn of_key(key: &str) -> Option<Sound> {
        Sound::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// How Sir says each sound, in English and German, after "when": the words of `ear::words`.
static SAID: LazyLock<Vec<(Sound, Regex)>> = LazyLock::new(|| {
    const HEAR: &str = r"(?:you\s+hear\s+|u\s+hear\s+|i\s+hear\s+|there\s+is\s+|theres\s+|there\s+s\s+)?";
    let twice = r"(?:twice|two\s+times|2\s+times|double)";
    let thrice = r"(?:three\s+times|3\s+times|thrice|triple)";
    let claps = |n: &str, de: &str, adj: &str| {
        format!(
            r"(?:i\s+|we\s+|someone\s+|somebody\s+)?claps?\s+(?:my\s+hands\s+|our\s+hands\s+)?{n}|(?:a\s+)?{adj}\s+clap(?:s|ping)?|i\s+{adj}\s+clap|(?:ich\s+)?(?:in\s+die\s+hände\s+)?{de}\s+(?:in\s+die\s+hände\s+)?klatsche|{de}\s+geklatscht\s+wird"
        )
    };
    let door = r"(?:\s+(?:on|at)\s+(?:the|my)\s+door)?";
    let table: Vec<(Sound, String)> = vec![
        (Sound::Claps(2), claps(twice, r"(?:zweimal|zwei\s+mal|2\s+mal|doppelt)", r"(?:double|two)")),
        (Sound::Claps(3), claps(thrice, r"(?:dreimal|drei\s+mal|3\s+mal)", r"(?:triple|three)")),
        (Sound::Snap, r"(?:i\s+)?(?:snap|click)\s+(?:my\s+)?fingers?|i\s+snap|(?:a\s+)?(?:finger\s*snap(?:ping)?|snap\s+of\s+(?:my\s+)?fingers?)|(?:ich\s+)?(?:mit\s+den\s+fingern\s+)?(?:schnipse|schnippe)|(?:ein\s+)?(?:finger)?(?:schnippen|schnipsen)".into()),
        (Sound::Knock, format!(r"(?:someone|somebody|anyone|anybody)\s+(?:knocks|is\s+knocking){door}|(?:a\s+|the\s+)?knock(?:ing|s)?{door}|(?:es|jemand)\s+(?:an\s+der\s+tür\s+|an\s+die\s+tür\s+)?klopft|du\s+(?:ein\s+|jemanden\s+)?klopfen\s+hörst|(?:ein\s+)?klopfen")),
        (Sound::Phone, r"(?:the\s+|my\s+)?(?:phone|telephone|cell\s*phone|mobile|handy)\s+(?:rings|is\s+ringing|ringing)|(?:das|mein)\s+(?:telefon|handy)\s+klingelt".into()),
        (Sound::Doorbell, r"(?:the\s+|my\s+)?(?:door\s*bell|bell)\s+(?:rings|is\s+ringing|ringing)|(?:someone|somebody)\s+rings\s+(?:the\s+|my\s+)?(?:door\s*)?bell|(?:the\s+|my\s+)?door\s*bell|(?:es\s+)?(?:an\s+der\s+tür\s+)?klingelt(?:\s+an\s+der\s+tür)?|(?:die\s+)?(?:tür)?klingel\s+(?:geht|läutet|klingelt)|du\s+(?:die\s+)?(?:tür)?klingel\s+hörst".into()),
        (Sound::Alarm, r"(?:an?\s+|the\s+|my\s+)?(?:smoke\s+|fire\s+)?alarm(?:\s+clock)?(?:\s+(?:goes\s+off|rings|sounds|beeps|is\s+ringing))?|(?:ein|der|mein)\s+(?:rauchmelder|alarm|wecker|feueralarm)\s+(?:losgeht|piept|klingelt|angeht)|du\s+(?:einen\s+)?(?:alarm|rauchmelder|wecker)\s+hörst".into()),
        (Sound::Dog, r"(?:a\s+|the\s+|my\s+)?dogs?\s+(?:barks|bark|is\s+barking|barking)|barking|(?:ein|der|mein)\s+hund\s+bellt|du\s+(?:einen\s+)?hund\s+(?:bellen\s+)?hörst".into()),
        (Sound::Glass, r"(?:some\s+|a\s+)?glass\s+(?:breaks|shatters|breaking|shattering)|(?:breaking|shattering)\s+glass|(?:ein\s+)?glas\s+(?:zerbricht|zerspringt|kaputt\s+geht)".into()),
        (Sound::Whistle, r"i\s+whistle|(?:me\s+|someone\s+)?whistl(?:e|es|ing)|ich\s+pfeife".into()),
        (Sound::SomeoneElse, r"(?:someone|somebody|anyone|anybody)\s+(?:else|other\s+than\s+me)\s+(?:talks|speaks|is\s+talking|is\s+speaking)|(?:another\s+(?:person|voice)|someone\s+else|somebody\s+else|a\s+stranger)(?:\s+(?:talking|speaking|talks|speaks))?|(?:a\s+)?stranger\s+(?:talks|speaks)|jemand\s+(?:anderes|anders|fremdes)\s+(?:spricht|redet)|du\s+(?:eine\s+)?(?:fremde|andere)\s+stimme\s+hörst".into()),
    ];
    table.into_iter().map(|(sound, pattern)| (sound, Regex::new(&format!(r"^{HEAR}(?:{pattern})(?:\s+(?:then|dann|so))?(?:\s+(?P<rest>.+))?$")).expect("sound pattern"))).collect()
});

/// The sound said at the start of `s` (words as `ear::words` gives them: "i clap twice turn on
/// the lights", "you hear knocking tell me", "ich zweimal klatsche"), and what follows it.
pub fn said(s: &str) -> Option<(Sound, String)> {
    SAID.iter().find_map(|(sound, re)| re.captures(s).map(|c| (*sound, c.name("rest").map_or("", |m| m.as_str()).trim().to_string())))
}

// ------------------------------------------------------------------------------ the model ----

static DIR: OnceLock<PathBuf> = OnceLock::new();
/// The model, and whether it wants its input with the mel bins before the frames.
static SESSION: Mutex<Option<(Session, bool)>> = Mutex::new(None);

/// Where the model is (set once at start from the models folder).
pub fn set_models_dir(dir: &Path) {
    let _ = DIR.set(dir.join("sound"));
}

/// Whether the model is there (without it the radar's own word holds for claps and knocks).
pub fn available() -> bool {
    DIR.get().is_some_and(|d| d.join(MODEL).is_file())
}

fn mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

/// Kaldi's mel filters from 0 to 8 kHz: for each bin, its weight on each FFT bin.
fn filters() -> &'static Vec<Vec<(usize, f32)>> {
    static FILTERS: OnceLock<Vec<Vec<(usize, f32)>>> = OnceLock::new();
    FILTERS.get_or_init(|| {
        let (low, high) = (mel(0.0), mel(RATE as f32 / 2.0));
        let step = (high - low) / (MELS + 1) as f32;
        (0..MELS)
            .map(|b| {
                let (left, centre, right) = (low + b as f32 * step, low + (b + 1) as f32 * step, low + (b + 2) as f32 * step);
                (0..FRAME / 2)
                    .filter_map(|k| {
                        let m = mel(k as f32 * RATE as f32 / FRAME as f32);
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

/// The model's features of a clip (16 kHz, -1 … 1): a row of `MELS` decibels every 10 ms.
pub fn features(pcm: &[f32]) -> Vec<[f32; MELS]> {
    let n = pcm.len() as i64;
    if n == 0 {
        return Vec::new();
    }
    let window: Vec<f32> = (0..FRAME).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (FRAME - 1) as f32).cos()).collect();
    // (Kaldi's frames with unsnipped edges: one every 10 ms, centred on it, the clip mirrored at
    // its ends.)
    let frames = (pcm.len() + SHIFT / 2) / SHIFT;
    let sample = |mut i: i64| {
        while i < 0 || i >= n {
            i = if i < 0 { -i - 1 } else { 2 * n - 1 - i };
        }
        pcm[i as usize]
    };
    let (mut re, mut im) = (vec![0.0f32; FRAME], vec![0.0f32; FRAME]);
    let mut rows = Vec::with_capacity(frames);
    for f in 0..frames {
        let start = (f * SHIFT + SHIFT / 2) as i64 - (FRAME / 2) as i64;
        for (i, (r, w)) in re.iter_mut().zip(&window).enumerate() {
            *r = sample(start + i as i64) * w;
        }
        im.iter_mut().for_each(|v| *v = 0.0);
        crate::fish_zero_shot_tts::fft(&mut re, &mut im, false);
        let mut row = [0.0f32; MELS];
        for (b, taps) in filters().iter().enumerate() {
            row[b] = taps.iter().map(|(k, w)| w * (re[*k] * re[*k] + im[*k] * im[*k])).sum();
        }
        rows.push(row);
    }
    let db = |x: f32| 10.0 * x.max(1e-10).log10();
    let top = rows.iter().flatten().map(|x| db(*x)).fold(f32::MIN, f32::max);
    for x in rows.iter_mut().flatten() {
        *x = db(*x).max(top - 120.0);
    }
    rows
}

/// How sure the model is of each of its 527 kinds of sound in a clip (16 kHz). Loads the model
/// on first use; never on the GUI thread.
pub fn scores(pcm: &[f32]) -> Result<Vec<f32>, String> {
    let rows = features(&pcm[pcm.len().saturating_sub(LONGEST)..]);
    if rows.len() < 16 {
        return Err("too short to tell a sound by".to_string());
    }
    let path = DIR.get().map(|d| d.join(MODEL)).filter(|p| p.is_file()).ok_or("the sound model is not installed")?;
    let mut guard = lock(&SESSION);
    if guard.is_none() {
        let env = crate::config::ort_env().map_err(|e| e.to_string())?;
        let session = Session::builder(env)
            .map_err(|e| e.to_string())?
            .with_intra_threads(1)
            .unwrap_or_else(|e| e.recover())
            .commit_from_file(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        // (sherpa-onnx hands it [1, bins, frames]: only an input that says otherwise gets frames first.)
        let bins_first = session.inputs().first().and_then(|i| i.dtype().tensor_shape()).is_none_or(|shape| shape.get(2) != Some(&(MELS as i64)));
        tracing::info!("[SOUND] Sound model loaded from {}", path.display());
        *guard = Some((session, bins_first));
    }
    let (session, bins_first) = guard.as_mut().ok_or("the sound model is not loaded")?;
    let frames = rows.len();
    let input = if *bins_first {
        let flat: Vec<f32> = (0..MELS).flat_map(|b| rows.iter().map(move |r| r[b])).collect();
        Tensor::from_array(([1usize, MELS, frames], flat))
    } else {
        Tensor::from_array(([1usize, frames, MELS], rows.iter().flatten().copied().collect::<Vec<f32>>()))
    }
    .map_err(|e| e.to_string())?;
    let outputs = session.run(ort::inputs![input]).map_err(|e| e.to_string())?;
    let (_, p) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
    if p.len() < CLASSES || p.iter().any(|x| !x.is_finite()) {
        return Err("the sound model returned nothing usable".to_string());
    }
    // (Probabilities; should a model give raw scores, they are made so.)
    let raw = p.iter().any(|x| !(0.0..=1.0).contains(x));
    Ok(p[..CLASSES].iter().map(|&x| if raw { 1.0 / (1.0 + (-x).exp()) } else { x }).collect())
}

/// What each of the model's kinds of sound is called ("Knock", "Finger snapping").
pub fn label(index: usize) -> String {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    let names = NAMES.get_or_init(|| {
        let text = DIR.get().and_then(|d| std::fs::read_to_string(d.join(LABELS)).ok()).unwrap_or_default();
        text.lines().skip(1).filter_map(|line| line.splitn(3, ',').nth(2)).map(|name| name.trim().trim_matches('"').to_string()).collect()
    });
    names.get(index).cloned().unwrap_or_else(|| format!("sound {index}"))
}

/// The `n` kinds the model is surest of, surest first.
pub fn top(scores: &[f32], n: usize) -> Vec<(usize, f32)> {
    let mut all: Vec<(usize, f32)> = scores.iter().copied().enumerate().collect();
    all.sort_by(|a, b| b.1.total_cmp(&a.1));
    all.truncate(n);
    all
}

/// What an event of the radar was, by the model's scores for its clip (None: no model): a sound
/// a rule or a protocol can wait on, or nothing worth one (typing, music, a tap on the desk).
pub fn judge(event: AcousticEvent, scores: Option<&[f32]>) -> Option<Sound> {
    let Some(p) = scores.filter(|p| p.len() >= CLASSES) else {
        // Without the model the radar's own word holds for what it can tell.
        return match event {
            AcousticEvent::Claps { count } => Some(Sound::Claps(count as u8)),
            AcousticEvent::Knock { .. } => Some(Sound::Knock),
            _ => None,
        };
    };
    let of = |ids: &[usize]| ids.iter().map(|&i| p[i]).fold(0.0f32, f32::max);
    let (clap, snap, knock, typing, music) = (of(CLAP), of(SNAP), of(KNOCK), of(TYPING), of(MUSIC));
    match event {
        // (When the model is unsure of everything, the radar's even run of claps holds.)
        AcousticEvent::Claps { count } => {
            if snap > clap.max(0.08) {
                Some(Sound::Snap)
            } else if typing > clap.max(0.15) || (music > 0.5 && clap < 0.1) {
                None
            } else if knock > clap.max(0.2) {
                Some(Sound::Knock)
            } else {
                Some(Sound::Claps(count as u8))
            }
        }
        AcousticEvent::Knock { .. } => (typing <= knock.max(0.1) && !(music > 0.5 && knock < 0.1)).then_some(Sound::Knock),
        AcousticEvent::Single => {
            if snap >= 0.15 && snap > clap {
                Some(Sound::Snap)
            } else {
                (p[SHATTER] >= 0.2).then_some(Sound::Glass)
            }
        }
        AcousticEvent::Sound => LASTING.iter().map(|(sound, ids, bar)| (*sound, of(ids), *bar)).filter(|(_, score, bar)| score >= bar).max_by(|a, b| a.1.total_cmp(&b.1)).map(|(sound, ..)| sound),
    }
}

// ------------------------------------------------------------------ what he heard lately ----

/// One thing heard, for the protocol window.
#[derive(Clone, Debug, PartialEq)]
pub struct Heard {
    /// When ("18:02:11").
    pub at: String,
    /// What it was taken for ("knocking"), or what was heard and let be ("two knocks: typing").
    pub what: String,
    /// A sound a rule or a protocol can wait on.
    pub sound: bool,
    /// What the model was surest of ("Knock 54 % · Door 31 %").
    pub detail: String,
}

static HEARD: Mutex<VecDeque<Heard>> = Mutex::new(VecDeque::new());
const KEPT: usize = 6;

fn keep(what: String, sound: bool, detail: String) {
    let mut heard = lock(&HEARD);
    heard.push_front(Heard { at: chrono::Local::now().format("%H:%M:%S").to_string(), what, sound, detail });
    heard.truncate(KEPT);
}

/// What was heard lately, the newest first.
pub fn recent() -> Vec<Heard> {
    lock(&HEARD).iter().cloned().collect()
}

/// One test at a time with what was heard (it is the whole process's).
#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    lock(&ONE)
}

/// Keeps what the radar heard and what it was taken for.
pub fn note(event: AcousticEvent, sound: Option<Sound>, scores: Option<&[f32]>) {
    let detail = scores.map_or_else(String::new, |p|top(p, 2).into_iter().map(|(i, s)| format!("{} {:.0} %", label(i), s * 100.0)).collect::<Vec<_>>().join(" · "));
    let heard = match event {
        AcousticEvent::Claps { count } => format!("{count} claps"),
        AcousticEvent::Knock { count } => format!("{count} knocks"),
        AcousticEvent::Single => "a click".to_string(),
        AcousticEvent::Sound => "a sound".to_string(),
    };
    match sound {
        Some(sound) => keep(sound.name().to_string(), true, detail),
        None => keep(format!("{heard}, let be"), false, detail),
    }
}

/// A sentence's voice was judged against Sir's print: kept among what was heard. Whether it is
/// someone else's (clearly not his) and nobody else was told of within the minute.
pub fn voice(his: bool, score: f32, bar: f32) -> bool {
    static LAST_OTHER: Mutex<Option<Instant>> = Mutex::new(None);
    let other = !his && score < bar * 0.6;
    let what = if his { "your voice" } else if other { "someone else talking" } else { "a voice not surely yours" };
    keep(what.to_string(), other, format!("{:.0} % like yours, {:.0} % needed", score * 100.0, bar * 100.0));
    let mut last = lock(&LAST_OTHER);
    let due = other && last.is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
    if due {
        *last = Some(Instant::now());
    }
    due
}

static AWAITED: AtomicBool = AtomicBool::new(false);
static WATCHED: Mutex<Option<Instant>> = Mutex::new(None);

/// Whether a rule or a protocol waits on a sound (`kit_notes` looks every few seconds).
pub fn set_awaited(on: bool) {
    AWAITED.store(on, Ordering::Release);
}

/// The protocol window shows what is heard: the radar listens while it does.
pub fn watch() {
    *lock(&WATCHED) = Some(Instant::now());
}

/// Whether the radar should listen for every kind of sound (not only J.A.I.'s own claps and
/// knocks).
pub fn wanted() -> bool {
    AWAITED.load(Ordering::Acquire) || lock(&WATCHED).is_some_and(|t| t.elapsed() < Duration::from_secs(3))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AudioSet's speech.
    const SPEECH: usize = 0;

    /// Every sound in Sir's words, English and German, with the order after it.
    #[test]
    fn sounds_are_heard_in_his_words() {
        let cases: &[(&str, Sound, &str)] = &[
            ("i clap twice turn on the lights", Sound::Claps(2), "turn on the lights"),
            ("i clap my hands twice then pause the music", Sound::Claps(2), "pause the music"),
            ("you hear a double clap open spotify", Sound::Claps(2), "open spotify"),
            ("i clap three times", Sound::Claps(3), ""),
            ("ich zweimal klatsche mach das licht an", Sound::Claps(2), "mach das licht an"),
            ("ich dreimal in die hände klatsche", Sound::Claps(3), ""),
            ("i snap my fingers next song", Sound::Snap, "next song"),
            ("i snap", Sound::Snap, ""),
            ("you hear knocking tell me", Sound::Knock, "tell me"),
            ("someone knocks on the door pause the music", Sound::Knock, "pause the music"),
            ("es klopft sag mir bescheid", Sound::Knock, "sag mir bescheid"),
            ("the doorbell rings tell me", Sound::Doorbell, "tell me"),
            ("es klingelt", Sound::Doorbell, ""),
            ("my phone rings mute the music", Sound::Phone, "mute the music"),
            ("das handy klingelt", Sound::Phone, ""),
            ("the smoke alarm goes off tell me", Sound::Alarm, "tell me"),
            ("a dog barks", Sound::Dog, ""),
            ("glass breaks warn me", Sound::Glass, "warn me"),
            ("i whistle play music", Sound::Whistle, "play music"),
            ("someone else talks pause the music", Sound::SomeoneElse, "pause the music"),
            ("jemand anderes spricht", Sound::SomeoneElse, ""),
        ];
        for (text, sound, rest) in cases {
            assert_eq!(said(text), Some((*sound, rest.to_string())), "{text}");
        }
        for text in ["i clapped", "snapshot", "minecraft starts", "max comes online", "knockout city starts", "i start a game"] {
            assert_eq!(said(text), None, "{text}");
        }
        for sound in Sound::ALL {
            assert_eq!(said(&crate::ear::words(sound.words()).join(" ")), Some((sound, String::new())), "{sound:?} is said as it is written");
            assert_eq!(Sound::of_key(sound.key()), Some(sound));
        }
    }

    fn scores_with(pairs: &[(usize, f32)]) -> Vec<f32> {
        let mut p = vec![0.01f32; CLASSES];
        for (i, s) in pairs {
            p[*i] = *s;
        }
        p
    }

    /// The model's word on what the radar heard: typing is no knock, a snap is no clap, a lasting
    /// sound is what the model is sure of; without the model the radar's claps and knocks hold.
    #[test]
    fn the_model_decides_what_the_radar_heard() {
        let claps = AcousticEvent::Claps { count: 2 };
        let knocks = AcousticEvent::Knock { count: 3 };
        assert_eq!(judge(claps, None), Some(Sound::Claps(2)));
        assert_eq!(judge(AcousticEvent::Claps { count: 3 }, None), Some(Sound::Claps(3)));
        assert_eq!(judge(knocks, None), Some(Sound::Knock));
        assert_eq!(judge(AcousticEvent::Single, None), None);
        assert_eq!(judge(AcousticEvent::Sound, None), None);
        assert_eq!(judge(claps, Some(&scores_with(&[(63, 0.6)]))), Some(Sound::Claps(2)));
        assert_eq!(judge(claps, Some(&scores_with(&[(63, 0.1), (62, 0.4)]))), Some(Sound::Snap));
        assert_eq!(judge(claps, Some(&scores_with(&[(386, 0.5), (63, 0.05)]))), None, "typing");
        assert_eq!(judge(claps, Some(&scores_with(&[]))), Some(Sound::Claps(2)), "unsure: the radar holds");
        assert_eq!(judge(knocks, Some(&scores_with(&[(359, 0.5)]))), Some(Sound::Knock));
        assert_eq!(judge(knocks, Some(&scores_with(&[(384, 0.6), (359, 0.1)]))), None, "typing");
        assert_eq!(judge(knocks, Some(&scores_with(&[(137, 0.8)]))), None, "a beat");
        assert_eq!(judge(AcousticEvent::Single, Some(&scores_with(&[(62, 0.5)]))), Some(Sound::Snap));
        assert_eq!(judge(AcousticEvent::Single, Some(&scores_with(&[(SHATTER, 0.5)]))), Some(Sound::Glass));
        assert_eq!(judge(AcousticEvent::Single, Some(&scores_with(&[(63, 0.5)]))), None, "a lone clap");
        assert_eq!(judge(AcousticEvent::Sound, Some(&scores_with(&[(355, 0.5), (0, 0.3)]))), Some(Sound::Doorbell));
        assert_eq!(judge(AcousticEvent::Sound, Some(&scores_with(&[(390, 0.3), (75, 0.6)]))), Some(Sound::Dog));
        assert_eq!(judge(AcousticEvent::Sound, Some(&scores_with(&[(137, 0.9), (SPEECH, 0.5)]))), None, "music and talk");
    }

    /// What was heard is kept, newest first, six at most; a voice clearly not his is someone
    /// else's, told once a minute.
    #[test]
    fn what_was_heard_is_kept() {
        let _one = test_lock();
        note(AcousticEvent::Knock { count: 2 }, None, Some(&scores_with(&[(384, 0.7)])));
        note(AcousticEvent::Claps { count: 2 }, Some(Sound::Claps(2)), None);
        let heard = recent();
        assert_eq!((heard[0].what.as_str(), heard[0].sound), ("two claps", true));
        assert!(heard[1].what.starts_with("2 knocks") && !heard[1].sound, "{:?}", heard[1]);
        assert!(!voice(true, 0.6, 0.45));
        assert!(voice(false, 0.1, 0.45), "clearly another voice");
        assert!(!voice(false, 0.1, 0.45), "once a minute");
        assert!(!voice(false, 0.4, 0.45), "not surely his, not surely another's");
        for _ in 0..8 {
            note(AcousticEvent::Single, None, None);
        }
        assert_eq!(recent().len(), KEPT);
    }

    /// The model on clips whose kind is beyond doubt: a voice is speech, a pure tone a tone, hiss
    /// noise and nothing silence (the front end and the model agree). Only where the model is.
    #[test]
    fn the_model_hears_what_a_sound_is() {
        set_models_dir(Path::new("models"));
        if !available() {
            eprintln!("models/sound is not here: skipped");
            return;
        }
        let names = |p: &[f32]| top(p, 3).into_iter().map(|(i, s)| format!("{} {s:.2}", label(i))).collect::<Vec<_>>().join(", ");
        let (voice, rate) = crate::voice_reference::decode(Path::new("assets/jai_voice_reference.mp3")).unwrap();
        let voice = crate::dsp::resample(&voice, rate, RATE as u32);
        let p = scores(&voice[..voice.len().min(RATE * 4)]).unwrap();
        println!("voice: {}", names(&p));
        assert!(top(&p, 2).iter().any(|(i, _)| *i == SPEECH), "voice: {}", names(&p));
        let tone: Vec<f32> = (0..RATE * 2).map(|i| (i as f32 * std::f32::consts::TAU * 1000.0 / RATE as f32).sin() * 0.3).collect();
        let p = scores(&tone).unwrap();
        println!("tone: {}", names(&p));
        assert!(top(&p, 3).iter().any(|(i, _)| [501, 481, 393, 503, 518, 502, 496, 516].contains(i)), "tone: {}", names(&p));
        let mut seed = 7u32;
        let hiss: Vec<f32> = (0..RATE * 2)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.2
            })
            .collect();
        let p = scores(&hiss).unwrap();
        println!("hiss: {}", names(&p));
        assert!(top(&p, 3).iter().any(|(i, _)| [513, 514, 515, 520, 521, 84, 285, 488].contains(i)), "hiss: {}", names(&p));
        let p = scores(&vec![0.0; RATE * 2]).unwrap();
        println!("nothing: {}", names(&p));
        assert!(top(&p, 3).iter().any(|(i, _)| *i == 500), "nothing: {}", names(&p));
    }
}
