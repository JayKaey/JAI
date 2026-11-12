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

static WANT_SPECTRUM: AtomicBool = AtomicBool::new(false);
static WANT_SPEECH: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
/// The last time a consumer said it still wants the capture (it closes when nobody has for a
/// few seconds, so a consumer that goes away without a word costs nothing).
static WANTED_AT: Mutex<Option<Instant>> = Mutex::new(None);

static SPECTRUM: Mutex<[f32; BANDS]> = Mutex::new([0.0; BANDS]);
static BASS: AtomicU32 = AtomicU32::new(0);
static LEVEL: AtomicU32 = AtomicU32::new(0);
static SPEECH: Mutex<VecDeque<f32>> = Mutex::new(VecDeque::new());

/// `user` wants (or no longer wants) what the PC plays. Asked for every frame it is used; the
/// capture opens on the first wish and closes a few seconds after the last.
pub fn want(user: Use, on: bool) {
    match user {
        Use::Spectrum => WANT_SPECTRUM.store(on, Ordering::Release),
        Use::Speech => WANT_SPEECH.store(on, Ordering::Release),
    }
    if on {
        *lock(&WANTED_AT) = Some(Instant::now());
        if !RUNNING.swap(true, Ordering::AcqRel) {
            let _ = std::thread::Builder::new().name("jai-pc-audio".into()).spawn(|| {
                capture();
                RUNNING.store(false, Ordering::Release);
            });
        }
    }
}

fn wanted() -> bool {
    (WANT_SPECTRUM.load(Ordering::Acquire) || WANT_SPEECH.load(Ordering::Acquire)) && lock(&WANTED_AT).is_some_and(|at| at.elapsed() < Duration::from_secs(4))
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

fn capture() {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        tracing::warn!("[AUDIO] No output device to listen to");
        return;
    };
    let Ok(config) = device.default_output_config() else { return };
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
        _ => return,
    };
    let Ok(stream) = stream else {
        tracing::warn!("[AUDIO] Could not listen to what the PC plays");
        return;
    };
    if stream.play().is_err() {
        return;
    }
    tracing::info!("[AUDIO] Listening to what the PC plays ({rate} Hz, loopback; read only)");
    let mut window: VecDeque<f32> = VecDeque::with_capacity(FFT * 2);
    let mut shown = [0.0f32; BANDS];
    let mut carry = (0.0f32, 0.0f32, 0.0f32);
    let mut last = Instant::now();
    let mut bass_avg = 0.0f32;
    while wanted() {
        let chunk = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => chunk,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Nothing plays: WASAPI's loopback delivers nothing then.
                silence();
                shown = [0.0; BANDS];
                continue;
            }
            Err(_) => break,
        };
        if WANT_SPEECH.load(Ordering::Acquire) {
            let speech = to_speech_rate(&chunk, rate, &mut carry);
            let mut tap = lock(&SPEECH);
            tap.extend(speech);
            let over = tap.len().saturating_sub(SPEECH_KEEP);
            tap.drain(..over);
        }
        if WANT_SPECTRUM.load(Ordering::Acquire) {
            window.extend(chunk.iter().copied());
            let over = window.len().saturating_sub(FFT);
            window.drain(..over);
            // Sixty spectra a second at most.
            if window.len() == FFT && last.elapsed() >= Duration::from_millis(16) {
                let dt = last.elapsed().as_secs_f32().min(0.1);
                last = Instant::now();
                let frame: Vec<f32> = window.iter().copied().collect();
                let measured = spectrum(&frame, rate as f32);
                settle(&mut shown, &measured, dt);
                *lock(&SPECTRUM) = shown;
                // The bass: the three lowest bands, against their own average of the last second
                // (so a beat stands out of a constant hum).
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
    drop(stream);
    silence();
    lock(&SPEECH).clear();
    tracing::info!("[AUDIO] No longer listening to what the PC plays");
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
        want(Use::Spectrum, true);
        want(Use::Speech, true);
        let (mut loudest, mut speech) = (0.0f32, 0usize);
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            want(Use::Spectrum, true);
            loudest = loudest.max(bass_and_level().1);
            speech += take_speech().len();
        }
        let up = RUNNING.load(Ordering::Acquire);
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
    fn nobody_wanting_it_closes_it() {
        *lock(&WANTED_AT) = Some(Instant::now() - Duration::from_secs(10));
        WANT_SPECTRUM.store(true, Ordering::Release);
        assert!(!wanted(), "a wish nobody has renewed for seconds has lapsed");
        WANT_SPECTRUM.store(false, Ordering::Release);
    }
}
