//! The reference voice: a recording of the film's J.A.R.V.I.S. (`Voicy_J.A.R.V.I.S..mp3`, "Formally
//! known as Just A Rather Very Intelligent System…").
//!
//! Drop the clip at `assets/jarvis_voice_reference.mp3` (or keep `Voicy_J.A.R.V.I.S..mp3` /
//! `jarvis voice.mp3` next to JARVIS.exe). This module finds it, decodes it (MP3 or WAV, pure
//! Rust via `symphonia`) and measures the level side of a voice: speech RMS, crest factor and a
//! coarse six-band balance. `fish_zero_shot_tts` builds the 128-band envelope transfer, the F0
//! analysis and the level lock on top of it.

use crate::jarvis_voice_dsp::{gain_to_db, Bq};
use std::path::{Path, PathBuf};

/// Where the clip is looked for, in order (relative to the folder JARVIS.exe runs from).
pub const REFERENCE_PATHS: [&str; 5] = [
    "assets/jarvis_voice_reference.mp3",
    "assets/jarvis_voice_reference.wav",
    "Voicy_J.A.R.V.I.S..mp3",
    "jarvis voice.mp3",
    "jarvis_voice_reference.mp3",
];
/// Band centres of the spectral envelope (Hz).
pub const BANDS: [f32; 6] = [135.0, 420.0, 1000.0, 2850.0, 5000.0, 7600.0];
/// The band the others are measured against (1 kHz).
const REFERENCE_BAND: usize = 2;
const BAND_Q: f32 = 1.0;
/// Speech frames: 20 ms, louder than −45 dBFS and within 35 dB of the loudest frame.
const FRAME_MS: f32 = 20.0;
const GATE_DB: f32 = -45.0;
const RELATIVE_GATE_DB: f32 = 35.0;
/// What a voice sounds like on average.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceProfile {
    /// Band levels relative to the 1 kHz band (dB).
    pub band_db: [f32; 6],
    /// RMS of the speech frames (dBFS).
    pub rms_db: f32,
    /// Peak over the speech RMS (dB).
    pub crest_db: f32,
    /// Seconds of speech analysed.
    pub speech_secs: f32,
}

/// The speech profile of `samples` (mono) at `rate`; None when there is under 0.25 s of speech.
pub fn analyze(samples: &[f32], rate: u32) -> Option<VoiceProfile> {
    let frame = ((FRAME_MS / 1000.0) * rate as f32) as usize;
    if frame == 0 || samples.len() < frame {
        return None;
    }
    let frame_db: Vec<f32> = samples.chunks(frame).map(|c| gain_to_db(crate::dsp::rms(c))).collect();
    let loudest = frame_db.iter().copied().fold(f32::MIN, f32::max);
    let keep: Vec<bool> = frame_db.iter().map(|db| *db > GATE_DB && *db > loudest - RELATIVE_GATE_DB).collect();
    let kept = keep.iter().filter(|k| **k).count();
    let speech_secs = kept as f32 * FRAME_MS / 1000.0;
    if speech_secs < 0.25 {
        return None;
    }
    let (mut energy, mut peak) = (0.0f64, 0.0f32);
    for (chunk, k) in samples.chunks(frame).zip(&keep) {
        if *k {
            energy += chunk.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>();
            peak = chunk.iter().fold(peak, |m, s| m.max(s.abs()));
        }
    }
    let count = (kept * frame) as f64;
    let rms = (energy / count).sqrt() as f32;
    let mut band = [0.0f32; 6];
    for (i, centre) in BANDS.iter().enumerate() {
        let mut filter = Bq::band_pass(*centre, BAND_Q, rate as f32);
        let mut e = 0.0f64;
        for (chunk, k) in samples.chunks(frame).zip(&keep) {
            for s in chunk {
                let y = filter.process(*s);
                if *k {
                    e += (y as f64) * (y as f64);
                }
            }
        }
        band[i] = gain_to_db(((e / count).sqrt() as f32).max(1e-9));
    }
    let reference = band[REFERENCE_BAND];
    Some(VoiceProfile { band_db: band.map(|b| b - reference), rms_db: gain_to_db(rms), crest_db: gain_to_db(peak / rms.max(1e-9)), speech_secs })
}

/// Decodes an MP3 or WAV file to mono f32: (samples, sample rate).
pub fn decode(path: &Path) -> Result<(Vec<f32>, u32), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe().format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default()).map_err(|e| e.to_string())?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("no audio track")?;
    let (track_id, rate) = (track.id, track.codec_params.sample_rate.unwrap_or(44_100));
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| e.to_string())?;
    let mut mono = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(_)) => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let channels = spec.channels.count().max(1);
                let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                buf.copy_interleaved_ref(decoded);
                mono.extend(buf.samples().chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32));
            }
            // A damaged frame is skipped.
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    if mono.is_empty() {
        return Err("no audio decoded".into());
    }
    Ok((mono, rate))
}

/// The first reference clip on disk.
pub fn reference_path() -> Option<PathBuf> {
    REFERENCE_PATHS.iter().map(PathBuf::from).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jarvis_voice_dsp::db_to_gain;
    use std::f32::consts::TAU;

    const R: u32 = 48_000;

    /// A "voice": harmonics of `f0` with a spectral tilt, syllable envelopes and pauses.
    fn voice(f0: f32, tilt_db_per_octave: f32, level: f32, secs: f32) -> Vec<f32> {
        let n = (secs * R as f32) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / R as f32;
                let syllable = ((TAU * 3.0 * t).sin() * 0.5 + 0.5).powf(0.7) * if (t % 1.0) < 0.85 { 1.0 } else { 0.0 };
                let mut s = 0.0;
                let mut k = 1.0;
                while f0 * k < 9000.0 {
                    s += (TAU * f0 * k * t).sin() * db_to_gain(tilt_db_per_octave * k.log2()) / k.sqrt();
                    k += 1.0;
                }
                s * syllable * level
            })
            .collect()
    }

    #[test]
    fn analysis_reads_level_crest_and_tilt() {
        let bright = analyze(&voice(120.0, 0.0, 0.05, 2.0), R).unwrap();
        let dark = analyze(&voice(120.0, -4.0, 0.05, 2.0), R).unwrap();
        assert!(bright.speech_secs > 1.2 && bright.speech_secs <= 2.0, "{}", bright.speech_secs);
        assert_eq!(bright.band_db[REFERENCE_BAND], 0.0);
        assert!(dark.band_db[5] < bright.band_db[5] - 6.0, "a darker voice has less sibilance band: {:?} vs {:?}", dark.band_db, bright.band_db);
        assert!(dark.band_db[0] > bright.band_db[0], "and relatively more fundamental");
        assert!(bright.crest_db > 3.0 && bright.crest_db < 30.0);
        // Twice the amplitude: about +6 dB of speech RMS (the −45 dBFS gate lets a few more quiet
        // frames of the louder take in, so slightly less).
        let loud = analyze(&voice(120.0, 0.0, 0.1, 2.0), R).unwrap();
        let gain = loud.rms_db - bright.rms_db;
        assert!((5.0..6.1).contains(&gain), "{gain}");
        // Silence is no speech.
        assert!(analyze(&vec![0.0; 48_000], R).is_none());
    }

    #[test]
    fn decodes_a_wav_and_knows_where_to_look() {
        assert_eq!(REFERENCE_PATHS[0], "assets/jarvis_voice_reference.mp3");
        assert!(REFERENCE_PATHS.contains(&"Voicy_J.A.R.V.I.S..mp3"));
        // A 16-bit stereo WAV written by hand decodes to the mono average.
        let dir = std::env::temp_dir().join(format!("jarvis_ref_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ref.wav");
        let frames: Vec<(i16, i16)> = (0..22_050).map(|i| (((i as f32 * 0.05).sin() * 8000.0) as i16, 0)).collect();
        let mut wav = Vec::new();
        let data_len = (frames.len() * 4) as u32;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        {
            let v = 16u32;
            wav.extend_from_slice(&v.to_le_bytes());
        }
        for v in [1u16, 2] {
            wav.extend_from_slice(&v.to_le_bytes());
        }
        for v in [22_050u32, 22_050 * 4] {
            wav.extend_from_slice(&v.to_le_bytes());
        }
        for v in [4u16, 16] {
            wav.extend_from_slice(&v.to_le_bytes());
        }
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for (l, r) in &frames {
            wav.extend_from_slice(&l.to_le_bytes());
            wav.extend_from_slice(&r.to_le_bytes());
        }
        std::fs::write(&path, wav).unwrap();
        let (pcm, rate) = decode(&path).unwrap();
        assert_eq!(rate, 22_050);
        assert_eq!(pcm.len(), 22_050);
        let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 8000.0 / 32768.0 / 2.0).abs() < 0.01, "mono average of L and a silent R: {peak}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Run with `JARVIS_VOICE_REFERENCE=<path to the mp3> cargo test -- --ignored reference_clip`.
    #[test]
    #[ignore]
    fn reference_clip_profile() {
        let path = std::env::var("JARVIS_VOICE_REFERENCE").expect("set JARVIS_VOICE_REFERENCE");
        let (pcm, rate) = decode(Path::new(&path)).unwrap();
        let profile = analyze(&pcm, rate).unwrap();
        println!("{} Hz, {:.1} s: {profile:?}", rate, pcm.len() as f32 / rate as f32);
        assert!(profile.speech_secs > 3.0);
    }
}
