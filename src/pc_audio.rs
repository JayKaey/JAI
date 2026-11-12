//! What the PC plays, heard once for two uses (2026-11-12): the music's spectrum for the emblem
//! (`bands`, `bass`) and the speech of a video, stream or call for the live subtitles
//! (`take_speech`). One loopback capture on the default output, open only while somebody wants
//! it (`want`), reading and never touching what plays. Nothing of it is kept or sent anywhere.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How many bands the spectrum has (low to high, logarithmic from 40 Hz to 12 kHz).
pub const BANDS: usize = 24;
/// (4096 samples: at 48 kHz a bin is 12 Hz wide, so each band from 40 Hz up has one of its own.
/// With 1024 the four lowest bands shared one bin and the bass moved as a block.)
const FFT: usize = 4096;
/// The speech tap's rate, and how much it holds at most.
pub const SPEECH_RATE: usize = 16_000;
const SPEECH_KEEP: usize = SPEECH_RATE * 30;

/// Who wants the capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Use {
    Spectrum,
    Speech,
}

/// Where a use listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Nowhere: nothing it should hear is playing.
    Nothing,
    /// Windows' default output: everything the PC plays, mixed (with whatever a mixer adds to
    /// it: on Sir's PC his own microphone's monitor).
    Default,
    /// One output device by its name: the one the music player, or the video, plays to.
    Device(String),
}

/// One use's tap: whether it is wanted, when that was last said, whether its capture runs,
/// and where it listens.
struct Tap {
    want: AtomicBool,
    running: AtomicBool,
    wanted_at: Mutex<Option<Instant>>,
    source: Mutex<Source>,
}

/// (The music's tap listens nowhere until `music_lyrics` has found what plays music; the
/// speech tap to everything until the subtitles name a device.)
static SPECTRUM_TAP: Tap = Tap { want: AtomicBool::new(false), running: AtomicBool::new(false), wanted_at: Mutex::new(None), source: Mutex::new(Source::Nothing) };
static SPEECH_TAP: Tap = Tap { want: AtomicBool::new(false), running: AtomicBool::new(false), wanted_at: Mutex::new(None), source: Mutex::new(Source::Default) };

fn tap(user: Use) -> &'static Tap {
    match user {
        Use::Spectrum => &SPECTRUM_TAP,
        Use::Speech => &SPEECH_TAP,
    }
}

static SPECTRUM: Mutex<[f32; BANDS]> = Mutex::new([0.0; BANDS]);
static BASS: AtomicU32 = AtomicU32::new(0);
static LEVEL: AtomicU32 = AtomicU32::new(0);
static SPEECH: Mutex<VecDeque<f32>> = Mutex::new(VecDeque::new());

/// `user` wants (or no longer wants) what the PC plays. Asked for every frame it is used; its
/// capture opens on the first wish and closes a few seconds after the last.
pub fn want(user: Use, on: bool) {
    let t = tap(user);
    t.want.store(on, Ordering::Release);
    if on {
        *lock(&t.wanted_at) = Some(Instant::now());
        if !t.running.swap(true, Ordering::AcqRel) {
            let _ = std::thread::Builder::new().name("jai-pc-audio".into()).spawn(move || {
                capture(user);
                tap(user).running.store(false, Ordering::Release);
            });
        }
    }
}

/// Where `user` listens from now on (its capture moves there by itself).
pub fn listen_to(user: Use, source: Source) {
    let mut now = lock(&tap(user).source);
    if *now != source {
        tracing::debug!("[AUDIO] {user:?} listens to {source:?}");
        *now = source;
    }
}

/// Where `user` listens right now.
pub fn source(user: Use) -> Source {
    lock(&tap(user).source).clone()
}

fn wanted(user: Use) -> bool {
    let t = tap(user);
    t.want.load(Ordering::Acquire) && lock(&t.wanted_at).is_some_and(|at| at.elapsed() < Duration::from_secs(4))
}

/// The spectrum right now: `BANDS` values 0-1, low to high (zeros while nothing plays).
pub fn bands() -> [f32; BANDS] {
    *lock(&SPECTRUM)
}

/// How strong the bass is right now (0-1) and how loud everything is (0-1).
pub fn bass_and_level() -> (f32, f32) {
    (f32::from_bits(BASS.load(Ordering::Relaxed)), f32::from_bits(LEVEL.load(Ordering::Relaxed)))
}

/// Takes what was heard since the last call (16 kHz mono), for the subtitles.
pub fn take_speech() -> Vec<f32> {
    lock(&SPEECH).drain(..).collect()
}

fn silence() {
    *lock(&SPECTRUM) = [0.0; BANDS];
    BASS.store(0f32.to_bits(), Ordering::Relaxed);
    LEVEL.store(0f32.to_bits(), Ordering::Relaxed);
}

/// The band edges (Hz) for a spectrum of `BANDS` bands: logarithmic from 40 Hz to 12 kHz.
pub fn band_edges() -> [f32; BANDS + 1] {
    std::array::from_fn(|k| 40.0 * (12_000.0f32 / 40.0).powf(k as f32 / BANDS as f32))
}

/// The spectrum of one window of `FFT` samples at `rate` Hz: each band's level 0-1 (-70 dB …
/// -15 dB of full scale), low to high.
pub fn spectrum(window: &[f32], rate: f32) -> [f32; BANDS] {
    let n = FFT.min(window.len());
    let mut re = vec![0.0f32; FFT];
    let mut im = vec![0.0f32; FFT];
    for (i, s) in window[..n].iter().enumerate() {
        // Hann.
        let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (n.max(2) - 1) as f32).cos();
        re[i] = s * w;
    }
    crate::fish_zero_shot_tts::fft(&mut re, &mut im, false);
    let edges = band_edges();
    let bin = rate / FFT as f32;
    std::array::from_fn(|b| {
        // (From the first bin at or above the band's lower edge up to the next band's first.)
        let (from, to) = (((edges[b] / bin).ceil() as usize).max(1), ((edges[b + 1] / bin).ceil() as usize).min(FFT / 2));
        let peak = (from..to.max(from + 1)).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt() / (FFT as f32 / 4.0)).fold(0.0f32, f32::max);
        let db = 20.0 * peak.max(1e-7).log10();
        ((db + 70.0) / 55.0).clamp(0.0, 1.0)
    })
}

/// The least the top of the scale ever is: quieter than this is quiet, not music to be blown up.
const LEAST_TOP: f32 = 0.22;

/// The spectrum on a scale whose top is the loudest band of the last seconds (`top`, which
/// rises at once and sinks slowly): a player's channel set low moves the ring as much as one
/// set high.
pub fn scaled(measured: [f32; BANDS], top: &mut f32, dt: f32) -> [f32; BANDS] {
    let loudest = measured.iter().copied().fold(0.0f32, f32::max);
    *top = (*top - 0.03 * dt).max(loudest).max(LEAST_TOP);
    measured.map(|b| (b / *top).clamp(0.0, 1.0))
}

/// One step of the shown spectrum towards the measured one: up at once, down slowly (the bars
/// of a level meter).
pub fn settle(shown: &mut [f32; BANDS], measured: &[f32; BANDS], dt: f32) {
    for (s, m) in shown.iter_mut().zip(measured) {
        *s = if m > s { *m } else { (*s - 1.6 * dt).max(*m) };
    }
}

/// 48 kHz (or whatever the device runs at) down to the speech tap's 16 kHz: every output sample
/// the mean of the input samples it covers.
pub fn to_speech_rate(samples: &[f32], rate: usize, carry: &mut (f32, f32, f32)) -> Vec<f32> {
    // carry: (the position in the input of the next output sample's start, sum, count)
    let step = rate as f32 / SPEECH_RATE as f32;
    let mut out = Vec::with_capacity(samples.len() * SPEECH_RATE / rate.max(1) + 1);
    for s in samples {
        carry.1 += s;
        carry.2 += 1.0;
        carry.0 += 1.0;
        if carry.0 >= step {
            out.push(carry.1 / carry.2.max(1.0));
            carry.0 -= step;
            carry.1 = 0.0;
            carry.2 = 0.0;
        }
    }
    out
}

/// The output device a source names, with its name (the default one when the named one is
/// gone).
fn device_for(source: &Source) -> Option<(cpal::Device, String)> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let named = match source {
        Source::Device(name) => host.output_devices().ok().and_then(|mut all| all.find(|d| d.name().is_ok_and(|n| n == *name))),
        _ => None,
    };
    let device = named.or_else(|| host.default_output_device())?;
    let name = device.name().unwrap_or_else(|_| "the output".to_string());
    Some((device, name))
}

/// One use's capture: it listens where its tap says, for as long as it is wanted.
fn capture(user: Use) {
    while wanted(user) {
        let source = source(user);
        if source == Source::Nothing {
            if user == Use::Spectrum {
                silence();
            }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        if !listen(user, &source) {
            // (The device could not be opened: not asked again at once.)
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    match user {
        Use::Spectrum => silence(),
        Use::Speech => lock(&SPEECH).clear(),
    }
}

/// What was last said in the log about each use's device (the HUD's capture closes whenever
/// it rests and opens again with its next frame: said once per device, not every time).
static SAID: Mutex<[Option<String>; 2]> = Mutex::new([None, None]);

/// Listens to `source` until the tap names another one or is no longer wanted. False when the
/// device could not be opened.
fn listen(user: Use, source: &Source) -> bool {
    use cpal::traits::{DeviceTrait, StreamTrait};
    let Some((device, name)) = device_for(source) else {
        tracing::warn!("[AUDIO] No output device to listen to");
        return false;
    };
    let Ok(config) = device.default_output_config() else { return false };
    let (rate, channels) = (config.sample_rate().0 as usize, config.channels() as usize);
    let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
    let mono = move |d: &[f32]| -> Vec<f32> { if channels <= 1 { d.to_vec() } else { d.chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect() } };
    let err = |e: cpal::StreamError| tracing::debug!("[AUDIO] Loopback: {e}");
    // (An input stream on an output device is WASAPI's loopback: it hears what is played.)
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(&config.config(), move |d: &[f32], _| { let _ = tx.send(mono(d)); }, err, None),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.config(),
            move |d: &[i16], _| {
                let f: Vec<f32> = d.iter().map(|s| *s as f32 / 32768.0).collect();
                let _ = tx.send(mono(&f));
            },
            err,
            None,
        ),
        _ => return false,
    };
    let Ok(stream) = stream else {
        tracing::warn!("[AUDIO] Could not listen to '{name}'");
        return false;
    };
    if stream.play().is_err() {
        return false;
    }
    let what = match user {
        Use::Spectrum => "the music",
        Use::Speech => "what is said",
    };
    {
        let mut said = lock(&SAID);
        let slot = &mut said[user as usize];
        if slot.as_deref() != Some(name.as_str()) {
            tracing::info!("[AUDIO] Listening to {what} on '{name}' ({rate} Hz, loopback; read only)");
            *slot = Some(name.clone());
        }
    }
    let mut window: VecDeque<f32> = VecDeque::with_capacity(FFT * 2);
    let mut shown = [0.0f32; BANDS];
    let mut carry = (0.0f32, 0.0f32, 0.0f32);
    let mut last = Instant::now();
    let mut bass_avg = 0.0f32;
    // The loudest band of the last seconds: the top of the scale, however quiet the player's
    // channel is set (measured on Sir's PC: the loudest band at 0.28 of full scale).
    let mut top = LEAST_TOP;
    while wanted(user) && *lock(&tap(user).source) == *source {
        let chunk = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => chunk,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Nothing plays: WASAPI's loopback delivers nothing then.
                if user == Use::Spectrum {
                    silence();
                    shown = [0.0; BANDS];
                }
                continue;
            }
            Err(_) => break,
        };
        match user {
            Use::Speech => {
                let speech = to_speech_rate(&chunk, rate, &mut carry);
                let mut tap = lock(&SPEECH);
                tap.extend(speech);
                let over = tap.len().saturating_sub(SPEECH_KEEP);
                tap.drain(..over);
            }
            Use::Spectrum => {
                window.extend(chunk.iter().copied());
                let over = window.len().saturating_sub(FFT);
                window.drain(..over);
                // Sixty spectra a second at most.
                if window.len() == FFT && last.elapsed() >= Duration::from_millis(16) {
                    let dt = last.elapsed().as_secs_f32().min(0.1);
                    last = Instant::now();
                    let frame: Vec<f32> = window.iter().copied().collect();
                    let measured = scaled(spectrum(&frame, rate as f32), &mut top, dt);
                    settle(&mut shown, &measured, dt);
                    *lock(&SPECTRUM) = shown;
                    // The bass: the three lowest bands, against their own average of the last
                    // second (so a beat stands out of a constant hum).
                    let bass = (measured[0] + measured[1] + measured[2]) / 3.0;
                    bass_avg += (bass - bass_avg) * (dt / 1.0).min(1.0);
                    let beat = ((bass - bass_avg * 0.85) * 3.0).clamp(0.0, 1.0) * bass;
                    let held = f32::from_bits(BASS.load(Ordering::Relaxed));
                    BASS.store((if beat > held { beat } else { (held - 3.5 * dt).max(beat) }).to_bits(), Ordering::Relaxed);
                    let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
                    LEVEL.store((((20.0 * rms.max(1e-7).log10()) + 60.0) / 50.0).clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                }
            }
        }
    }
    drop(stream);
    tracing::debug!("[AUDIO] No longer listening to {what} on '{name}'");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the sound is right now: every output device with its meter (read only).
    /// `cargo test --bin jai where_the_sound_is -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn where_the_sound_is() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for d in rt.block_on(crate::audio_mixer::output_devices()).unwrap_or_default() {
            println!("{} {}: meter {:.3}, {} app(s) with a session", if d.default { "*" } else { " " }, d.name, d.peak, d.apps.len());
        }
    }

    /// The loopback for real, two seconds of it (it only listens, and says what it heard in
    /// numbers): `cargo test --bin jai the_loopback_opens -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_loopback_opens_and_hears_what_plays() {
        // (Both to everything the PC plays, as a check of the capture itself.)
        listen_to(Use::Spectrum, Source::Default);
        listen_to(Use::Speech, Source::Default);
        want(Use::Spectrum, true);
        want(Use::Speech, true);
        let (mut loudest, mut speech) = (0.0f32, 0usize);
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            want(Use::Spectrum, true);
            loudest = loudest.max(bass_and_level().1);
            speech += take_speech().len();
        }
        let up = SPECTRUM_TAP.running.load(Ordering::Acquire) && SPEECH_TAP.running.load(Ordering::Acquire);
        println!("capture up: {up}; loudest level {loudest:.3}; {speech} speech samples in 2 s; bands {:?}", bands().map(|b| (b * 100.0) as u32));
        want(Use::Spectrum, false);
        want(Use::Speech, false);
        assert!(up, "the capture did not open (or closed again)");
    }

    fn tone(freq: f32, amp: f32, rate: f32) -> Vec<f32> {
        (0..FFT).map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / rate).sin()).collect()
    }

    #[test]
    fn a_tone_lights_its_own_band() {
        let edges = band_edges();
        assert!((edges[0] - 40.0).abs() < 0.01 && (edges[BANDS] - 12_000.0).abs() < 1.0);
        for freq in [60.0f32, 440.0, 3000.0, 9000.0] {
            let s = spectrum(&tone(freq, 0.3, 48_000.0), 48_000.0);
            let loudest = s.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
            let home = (0..BANDS).find(|b| edges[*b] <= freq && freq < edges[b + 1]).unwrap();
            assert!(loudest.abs_diff(home) <= 1, "{freq} Hz lit band {loudest}, its own is {home}");
            assert!(s[loudest] > 0.6, "{freq} Hz at 0.3 reads {}", s[loudest]);
        }
        assert!(spectrum(&vec![0.0; FFT], 48_000.0).iter().all(|b| *b == 0.0), "silence is dark");
    }

    #[test]
    fn the_bars_rise_at_once_and_fall_slowly() {
        let mut shown = [0.0f32; BANDS];
        let mut measured = [0.0f32; BANDS];
        measured[3] = 0.9;
        settle(&mut shown, &measured, 1.0 / 60.0);
        assert_eq!(shown[3], 0.9);
        measured[3] = 0.0;
        settle(&mut shown, &measured, 1.0 / 60.0);
        assert!(shown[3] > 0.85 && shown[3] < 0.9, "one frame later: {}", shown[3]);
        for _ in 0..60 {
            settle(&mut shown, &measured, 1.0 / 60.0);
        }
        assert_eq!(shown[3], 0.0, "dark within a second");
    }

    #[test]
    fn the_speech_tap_is_a_third_of_48k_and_keeps_the_level() {
        let mut carry = (0.0, 0.0, 0.0);
        let input: Vec<f32> = (0..4800).map(|i| (std::f32::consts::TAU * 300.0 * i as f32 / 48_000.0).sin() * 0.5).collect();
        let (a, b) = input.split_at(1000);
        let mut out = to_speech_rate(a, 48_000, &mut carry);
        out.extend(to_speech_rate(b, 48_000, &mut carry));
        assert!((1599..=1600).contains(&out.len()), "a tenth of a second at 16 kHz, whatever the chunking: {}", out.len());
        let rms = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!((rms - 0.5 / 2f32.sqrt()).abs() < 0.03, "a 300 Hz tone keeps its level: {rms}");
        let mut carry = (0.0, 0.0, 0.0);
        assert!((1599..=1600).contains(&to_speech_rate(&vec![0.1; 4410], 44_100, &mut carry).len()));
    }

    #[test]
    fn a_quiet_channel_moves_the_ring_as_much_as_a_loud_one() {
        let mut quiet = [0.0f32; BANDS];
        quiet[3] = 0.28;
        quiet[9] = 0.14;
        let mut top = LEAST_TOP;
        let shown = scaled(quiet, &mut top, 0.016);
        assert!((shown[3] - 1.0).abs() < 1e-5 && (shown[9] - 0.5).abs() < 1e-5, "{shown:?}");
        // The scale's top sinks slowly once it gets quieter, and never under its floor.
        let later = scaled([0.05; BANDS], &mut top, 1.0);
        assert!(top < 0.28 && top >= LEAST_TOP && later[0] < 0.25);
        // A loud channel is not clipped away: its loudest band is the top too.
        let mut top = LEAST_TOP;
        let mut loud = [0.3f32; BANDS];
        loud[0] = 0.9;
        assert!((scaled(loud, &mut top, 0.016)[1] - 1.0 / 3.0).abs() < 1e-4);
    }

    #[test]
    fn nobody_wanting_it_closes_it() {
        *lock(&SPEECH_TAP.wanted_at) = Some(Instant::now() - Duration::from_secs(10));
        SPEECH_TAP.want.store(true, Ordering::Release);
        assert!(!wanted(Use::Speech), "a wish nobody has renewed for seconds has lapsed");
        SPEECH_TAP.want.store(false, Ordering::Release);
        // The music's tap listens nowhere until something plays music; the speech tap to
        // everything until the subtitles name a device.
        assert_eq!(source(Use::Spectrum), Source::Nothing);
        listen_to(Use::Spectrum, Source::Device("Music".into()));
        assert_eq!(source(Use::Spectrum), Source::Device("Music".into()));
        listen_to(Use::Spectrum, Source::Nothing);
    }
}
