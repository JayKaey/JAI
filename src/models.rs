//! Model files: presence check + automatic download, and Kokoro voice extraction.
//!
//! Sources (sizes verified against the published files):
//! * Whisper ggml:   huggingface.co/ggerganov/whisper.cpp/resolve/main/<file>
//! * Silero VAD v5:  raw.githubusercontent.com/snakers4/silero-vad/v5.1.2/.../silero_vad.onnx
//! * Kokoro v1.0:    github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/
//!   (the v0.19 files from .../model-files/ still work when a config points at them)
//!
//! Downloads stream to `<file>.part` and are renamed only after the size checks out, so an
//! interrupted download never leaves a corrupt model behind; the next try goes on from there.

use crate::config::JaiConfig;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

const SILERO_URL: &str = "https://raw.githubusercontent.com/snakers4/silero-vad/v5.1.2/src/silero_vad/data/silero_vad.onnx";
const KOKORO_BASE: &str = "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files";
const KOKORO_V1_BASE: &str = "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0";

/// Kokoro v1.0 (the model with Fable, Daniel and the other v1.0 voices) and its voice pack.
pub const KOKORO_V1_MODEL: &str = "models/kokoro-v1.0.onnx";
pub const KOKORO_V1_VOICES: &str = "models/voices-v1.0.bin";
/// Kokoro v0.19, the earlier model; its voice tables do not fit v1.0 (and vice versa).
pub const KOKORO_V019_MODEL: &str = "models/kokoro-v0_19.onnx";
pub const KOKORO_V019_VOICES: &str = "models/voices_npz.bin";

/// Whether a model file is Kokoro v1.0 (by its file name).
pub fn is_kokoro_v1(model_path: &str) -> bool {
    file_name(model_path).contains("v1.0")
}

/// Where the extracted style table of `voice` is cached. The v0.19 archive keeps its tables
/// next to the models (`models/bm_george.bin`); every other archive gets its own folder
/// (`models/voices/voices-v1.0/bm_fable.bin`), so the tables of two model versions never mix.
pub fn voice_cache_path(models_dir: &Path, archive: &Path, voice: &str) -> PathBuf {
    let archive_name = archive.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    if matches!(archive_name.as_str(), "voices.bin" | "voices_npz.bin" | "") {
        models_dir.join(format!("{voice}.bin"))
    } else {
        let stem = archive.file_stem().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "voices".into());
        models_dir.join("voices").join(stem).join(format!("{voice}.bin"))
    }
}
const WHISPER_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
/// CED-mini, the model that tells what a sound is (`sound_sense`), as sherpa-onnx exported it.
const SOUND_BASE: &str = "https://huggingface.co/k2-fsa/sherpa-onnx-ced-mini-audio-tagging-2024-04-19/resolve/main";

#[derive(Debug, Clone)]
pub struct ModelFile {
    pub label: &'static str,
    pub path: PathBuf,
    pub url: String,
    pub expected_size: Option<u64>,
    pub required: bool,
}

fn known_size(file_name: &str) -> Option<u64> {
    match file_name {
        "silero_vad.onnx" => Some(2_327_524),
        "kokoro-v0_19.onnx" => Some(325_525_180),
        "voices.bin" | "voices_npz.bin" => Some(5_758_648),
        "kokoro-v1.0.onnx" => Some(325_532_387),
        "voices-v1.0.bin" => Some(28_214_398),
        "ggml-small-q8_0.bin" => Some(264_464_607),
        "ggml-base-q8_0.bin" => Some(81_768_585),
        "ggml-small.bin" => Some(487_601_967),
        "ggml-large-v3-turbo-q5_0.bin" => Some(574_041_195),
        "model.int8.onnx" => Some(10_451_715),
        "class_labels_indices.csv" => Some(14_675),
        _ => None,
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Every model file JAI needs for the current configuration.
pub fn required_files(cfg: &JaiConfig) -> Vec<ModelFile> {
    let mut files = Vec::new();

    let whisper_primary = Path::new(&cfg.whisper_model_path);
    let whisper_fallback = Path::new(&cfg.whisper_fallback_model_path);
    // One working Whisper model is enough; prefer fetching the configured primary.
    if !whisper_primary.exists() && !whisper_fallback.exists() {
        let name = file_name(&cfg.whisper_model_path);
        if name.starts_with("ggml-") && name.ends_with(".bin") {
            files.push(ModelFile {
                label: "Whisper speech recognition",
                path: whisper_primary.to_path_buf(),
                url: format!("{WHISPER_BASE}/{name}"),
                expected_size: known_size(&name),
                required: true,
            });
        } else {
            warn!("[MODELS] Whisper model {name} is not a standard ggml file name; cannot auto-download it.");
        }
    }

    // The model Whisper hears with on a graphics card (optional: without it the CPU's goes there).
    let name = file_name(&cfg.whisper_gpu_model_path);
    if cfg.whisper_gpu && name.starts_with("ggml-") && name.ends_with(".bin") && crate::stt::has_card() {
        files.push(ModelFile {
            label: "Whisper (graphics card)",
            path: PathBuf::from(&cfg.whisper_gpu_model_path),
            url: format!("{WHISPER_BASE}/{name}"),
            expected_size: known_size(&name),
            required: false,
        });
    }

    files.push(ModelFile {
        label: "Silero VAD v5",
        path: PathBuf::from(&cfg.silero_vad_model_path),
        url: SILERO_URL.to_string(),
        expected_size: Some(2_327_524),
        required: true,
    });
    // What a sound is (optional: without it the radar tells claps and knocks by itself).
    let sound = Path::new(&cfg.silero_vad_model_path).parent().unwrap_or(Path::new("models")).join("sound");
    for (label, name) in [("Sound model (CED-mini)", crate::sound_sense::MODEL), ("Sound model's labels", crate::sound_sense::LABELS)] {
        files.push(ModelFile { label, path: sound.join(name), url: format!("{SOUND_BASE}/{name}"), expected_size: known_size(name), required: false });
    }
    let v1 = is_kokoro_v1(&cfg.kokoro_model_path);
    let model_name = file_name(&cfg.kokoro_model_path);
    files.push(ModelFile {
        label: if v1 { "Kokoro TTS v1.0" } else { "Kokoro TTS v0.19" },
        path: PathBuf::from(&cfg.kokoro_model_path),
        url: if v1 { format!("{KOKORO_V1_BASE}/{model_name}") } else { format!("{KOKORO_BASE}/kokoro-v0_19.onnx") },
        expected_size: known_size(&model_name),
        required: true,
    });

    // Voice tables: the extracted raw tables of every voice in the blend, or the npz archive to
    // extract them from.
    let models_dir = Path::new(&cfg.kokoro_model_path).parent().unwrap_or(Path::new("models"));
    let archive = Path::new(&cfg.kokoro_voices_archive_path);
    let voices: Vec<String> = cfg
        .kokoro_voice
        .split(['+', ','])
        .map(|part| part.split(':').next().unwrap_or("").trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();
    if voices.is_empty() || voices.iter().any(|v| !voice_cache_path(models_dir, archive, v).exists()) {
        let archive_name = file_name(&cfg.kokoro_voices_archive_path);
        files.push(ModelFile {
            label: "Kokoro voice archive",
            path: archive.to_path_buf(),
            url: if v1 { format!("{KOKORO_V1_BASE}/{archive_name}") } else { format!("{KOKORO_BASE}/voices.bin") },
            expected_size: known_size(&archive_name),
            required: true,
        });
    }
    files
}

fn is_present(f: &ModelFile) -> bool {
    match std::fs::metadata(&f.path) {
        // Tiny files are failed/HTML-error downloads, not models.
        Ok(m) => m.len() > 64 * 1024 || f.expected_size.is_some_and(|s| m.len() == s),
        Err(_) => false,
    }
}

pub fn missing(cfg: &JaiConfig) -> Vec<ModelFile> {
    required_files(cfg).into_iter().filter(|f| !is_present(f)).collect()
}

/// Logs the model status; returns false if something required is missing.
pub fn report(cfg: &JaiConfig) -> bool {
    let mut ok = true;
    for f in required_files(cfg) {
        if is_present(&f) {
            let size = std::fs::metadata(&f.path).map(|m| m.len()).unwrap_or(0);
            info!("[MODELS] OK   {:<26} {} ({:.1} MB)", f.label, f.path.display(), size as f64 / 1e6);
        } else {
            ok &= !f.required;
            warn!("[MODELS] MISSING {:<26} {} (missing)", f.label, f.path.display());
        }
    }
    for (label, path) in [
        ("Whisper (primary)", &cfg.whisper_model_path),
        ("Whisper (fallback)", &cfg.whisper_fallback_model_path),
    ] {
        if Path::new(path).exists() {
            info!("[MODELS] OK   {label:<26} {path}");
        }
    }

    // Built-in language engine and its private model store.
    match crate::runtime::engine_binary(Path::new(&cfg.llm_runtime_dir)) {
        Some(engine) => {
            info!("[MODELS] OK   {:<26} {}", "LLM engine (built-in)", engine.display());
            for (label, name, required) in [("LLM (primary)", &cfg.primary_model, true), ("LLM (fallback)", &cfg.fallback_model, false)] {
                if name.is_empty() {
                    continue;
                }
                if crate::runtime::has_local_manifest(Path::new(&cfg.llm_models_dir), name) {
                    info!("[MODELS] OK   {label:<26} {name} ({})", cfg.llm_models_dir);
                } else if required {
                    ok = false;
                    warn!("[MODELS] MISSING {label:<26} {name} (installed automatically on start)");
                }
            }
        }
        None => warn!(
            "[MODELS] MISSING {:<26} {} (missing; a separately installed Ollama is used instead)",
            "LLM engine (built-in)", cfg.llm_runtime_dir
        ),
    }
    let espeak = Path::new("runtime").join("espeak-ng").join(if cfg!(windows) { "libespeak-ng.dll" } else { "libespeak-ng.so.1" });
    if espeak.is_file() {
        info!("[MODELS] OK   {:<26} {}", "eSpeak NG (built-in)", espeak.display());
    }
    // The voice lock's model ("only obey me"): optional, there is no lock without it.
    let speaker = Path::new(&cfg.silero_vad_model_path).parent().unwrap_or(Path::new("models")).join("speaker").join(crate::speaker_id::MODEL);
    match std::fs::metadata(&speaker) {
        Ok(m) if m.len() > 64 * 1024 => info!("[MODELS] OK   {:<26} {} ({:.1} MB)", "Voice lock (speaker)", speaker.display(), m.len() as f64 / 1e6),
        _ => info!("[MODELS] --   {:<26} {} (optional: \"only obey me\" needs it)", "Voice lock (speaker)", speaker.display()),
    }
    ok
}

/// Downloads every missing model. Returns the files that are still missing afterwards.
pub async fn ensure(cfg: &JaiConfig) -> Vec<ModelFile> {
    let todo = missing(cfg);
    if todo.is_empty() {
        return Vec::new();
    }
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .user_agent("jai-model-fetcher/3.1")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!("[MODELS] Could not create HTTP client: {e}");
            return todo;
        }
    };
    let mut still_missing = Vec::new();
    for f in todo {
        info!("[MODELS] Downloading {} → {} …", f.label, f.path.display());
        let label = f.label;
        let told = |n: u64, of: Option<u64>| match of {
            Some(t) if t > 0 => info!("[MODELS]   {label} {:.0}% ({:.0}/{:.0} MB)", n as f64 * 100.0 / t as f64, n as f64 / 1e6, t as f64 / 1e6),
            _ => info!("[MODELS]   {label} {:.0} MB", n as f64 / 1e6),
        };
        match download_to(&client, &f.url, &f.path, f.expected_size, &told).await {
            Ok(bytes) => info!("[MODELS] {} downloaded ({:.1} MB)", f.label, bytes as f64 / 1e6),
            Err(e) => {
                error!("[MODELS] Download of {} failed: {e}", f.label);
                still_missing.push(f);
            }
        }
    }
    still_missing
}

/// `url` into `to` through `<to>.part`. A stream that breaks off (reqwest: "error decoding
/// response body") goes on where it stopped, by a Range, until five tries in a row bring no
/// byte; `size` (else the server's) must come whole. `got(bytes, of)` hears it each second.
pub(crate) async fn download_to(client: &reqwest::Client, url: &str, to: &Path, size: Option<u64>, got: &(dyn Fn(u64, Option<u64>) + Sync)) -> Result<u64, String> {
    if let Some(dir) = to.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut part = to.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let mut fruitless = 0;
    loop {
        let mut have = std::fs::metadata(&part).map_or(0, |m| m.len());
        if size.is_some_and(|s| have >= s) {
            let _ = std::fs::remove_file(&part);
            have = 0;
        }
        let asked = if have > 0 { client.get(url).header(reqwest::header::RANGE, format!("bytes={have}-")) } else { client.get(url) };
        let tried = match asked.send().await.and_then(|a| a.error_for_status()) {
            Ok(answer) => save(answer, &part, have, size, got).await,
            Err(e) => {
                if e.status() == Some(reqwest::StatusCode::RANGE_NOT_SATISFIABLE) {
                    let _ = std::fs::remove_file(&part);
                }
                Err(e.to_string())
            }
        };
        match tried {
            Ok(bytes) => return std::fs::rename(&part, to).map(|()| bytes).map_err(|e| e.to_string()),
            Err(e) => {
                fruitless = if std::fs::metadata(&part).map_or(0, |m| m.len()) > have { 0 } else { fruitless + 1 };
                if fruitless == 5 {
                    return Err(e);
                }
                warn!("[MODELS] {url} broke off ({e}); going on where it stopped");
                tokio::time::sleep(Duration::from_secs(if cfg!(test) { 0 } else { 3 })).await;
            }
        }
    }
}

/// One answer into `part`: after its `have` bytes when it is the Range asked for, else afresh.
async fn save(mut answer: reqwest::Response, part: &Path, have: u64, size: Option<u64>, got: &(dyn Fn(u64, Option<u64>) + Sync)) -> Result<u64, String> {
    let start = if answer.status() == reqwest::StatusCode::PARTIAL_CONTENT { have } else { 0 };
    let theirs = answer.content_length().map(|rest| start + rest);
    if let Some((want, has)) = size.zip(theirs).filter(|(w, h)| w != h) {
        return Err(format!("it has {has} bytes, not {want}"));
    }
    let size = size.or(theirs);
    let mut file = std::fs::OpenOptions::new().create(true).write(true).append(start > 0).truncate(start == 0).open(part).map_err(|e| e.to_string())?;
    let mut written = start;
    let mut told = Instant::now();
    while let Some(chunk) = answer.chunk().await.map_err(|e| e.to_string())? {
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        written += chunk.len() as u64;
        if told.elapsed() >= Duration::from_secs(1) {
            told = Instant::now();
            got(written, size);
        }
    }
    match size {
        Some(size) if written != size => {
            if written > size {
                let _ = std::fs::remove_file(part);
            }
            Err(format!("got {written} of {size} bytes"))
        }
        _ => Ok(written),
    }
}

// ---------------------------------------------------------------------------------------------
// Kokoro voices from the voice packs (uncompressed numpy .npz = zip of .npy files)
// ---------------------------------------------------------------------------------------------

struct ZipEntry {
    name: String,
    data_offset: usize,
    size: usize,
}

fn u16_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as usize)
}

fn u32_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize)
}

/// Reads the zip central directory; only STORED (method 0) entries are supported, which is
/// what `numpy.savez` writes.
fn zip_entries(bytes: &[u8]) -> Result<Vec<ZipEntry>, String> {
    let eocd = (0..bytes.len().saturating_sub(21))
        .rev()
        .take(70_000)
        .find(|&i| bytes[i..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
        .ok_or("not a zip archive (no end-of-central-directory record)")?;
    let count = u16_at(bytes, eocd + 10).ok_or("truncated archive")?;
    let mut pos = u32_at(bytes, eocd + 16).ok_or("truncated archive")?;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if !bytes.get(pos..).is_some_and(|b| b.starts_with(&[0x50, 0x4b, 0x01, 0x02])) {
            return Err("corrupt central directory".into());
        }
        let method = u16_at(bytes, pos + 10).ok_or("truncated")?;
        let size = u32_at(bytes, pos + 20).ok_or("truncated")?;
        let name_len = u16_at(bytes, pos + 28).ok_or("truncated")?;
        let extra_len = u16_at(bytes, pos + 30).ok_or("truncated")?;
        let comment_len = u16_at(bytes, pos + 32).ok_or("truncated")?;
        let local = u32_at(bytes, pos + 42).ok_or("truncated")?;
        let name = String::from_utf8_lossy(bytes.get(pos + 46..pos + 46 + name_len).ok_or("truncated")?).to_string();
        pos += 46 + name_len + extra_len + comment_len;
        if method != 0 {
            return Err(format!("{name} is compressed (method {method}); only stored entries are supported"));
        }
        let local_name_len = u16_at(bytes, local + 26).ok_or("truncated")?;
        let local_extra_len = u16_at(bytes, local + 28).ok_or("truncated")?;
        entries.push(ZipEntry {
            name,
            data_offset: local + 30 + local_name_len + local_extra_len,
            size,
        });
    }
    Ok(entries)
}

/// Parses a little-endian float32 `.npy` payload.
fn parse_npy_f32(npy: &[u8]) -> Result<Vec<f32>, String> {
    if !npy.starts_with(b"\x93NUMPY") {
        return Err("not an .npy array".into());
    }
    let major = *npy.get(6).ok_or("truncated npy")?;
    let (header_len, start) = if major == 1 {
        (u16_at(npy, 8).ok_or("truncated npy")?, 10)
    } else {
        (u32_at(npy, 8).ok_or("truncated npy")?, 12)
    };
    let header = String::from_utf8_lossy(npy.get(start..start + header_len).ok_or("truncated npy")?);
    if !header.contains("'<f4'") {
        return Err(format!("unsupported dtype in npy header: {header}"));
    }
    if header.contains("'fortran_order': True") {
        return Err("fortran-ordered arrays are not supported".into());
    }
    let data = &npy[start + header_len..];
    Ok(data
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

pub fn list_voices(archive: &Path) -> Result<Vec<String>, String> {
    let bytes = std::fs::read(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    Ok(zip_entries(&bytes)?
        .into_iter()
        .filter_map(|e| e.name.strip_suffix(".npy").map(String::from))
        .collect())
}

/// Style table (`[rows, 256]` floats) for `voice` from the npz archive.
pub fn extract_voice(archive: &Path, voice: &str) -> Result<Vec<f32>, String> {
    let bytes = std::fs::read(archive).map_err(|e| format!("voice archive {}: {e}", archive.display()))?;
    let entry = zip_entries(&bytes)?
        .into_iter()
        .find(|e| e.name == format!("{voice}.npy"))
        .ok_or_else(|| format!("voice '{voice}' is not in {}", archive.display()))?;
    let npy = bytes
        .get(entry.data_offset..entry.data_offset + entry.size)
        .ok_or("voice entry is truncated")?;
    let table = parse_npy_f32(npy)?;
    if table.len() < 256 || table.len() % 256 != 0 {
        return Err(format!("voice '{voice}' has an unexpected size ({} floats)", table.len()));
    }
    info!("[TTS] Extracted voice '{voice}' ({} style rows) from {}", table.len() / 256, archive.display());
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn npy(values: &[f32]) -> Vec<u8> {
        let mut header = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': ({},), }}", values.len());
        while (10 + header.len() + 1) % 64 != 0 {
            header.push(' ');
        }
        header.push('\n');
        let mut out = b"\x93NUMPY\x01\x00".to_vec();
        out.extend((header.len() as u16).to_le_bytes());
        out.extend(header.as_bytes());
        out.extend(values.iter().flat_map(|v| v.to_le_bytes()));
        out
    }

    /// Minimal STORED zip writer (local headers + central directory + EOCD).
    fn zip(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in files {
            let offset = out.len() as u32;
            out.extend([0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend([0u8; 4]); // crc (unchecked)
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(0u16.to_le_bytes());
            out.extend(name.as_bytes());
            out.extend(data);
            central.extend([0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend([0u8; 4]);
            central.extend((data.len() as u32).to_le_bytes());
            central.extend((data.len() as u32).to_le_bytes());
            central.extend((name.len() as u16).to_le_bytes());
            central.extend([0u8; 12]);
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        let cd_len = central.len() as u32;
        out.extend(central);
        out.extend([0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend(cd_len.to_le_bytes());
        out.extend(cd_offset.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    #[test]
    fn extracts_voice_from_npz() {
        let george: Vec<f32> = (0..512).map(|i| i as f32 * 0.5).collect();
        let archive = zip(&[("af.npy", npy(&[1.0; 256])), ("bm_george.npy", npy(&george))]);
        let path = std::env::temp_dir().join(format!("jai_voices_{}.bin", uuid::Uuid::new_v4()));
        std::fs::write(&path, archive).unwrap();
        assert_eq!(list_voices(&path).unwrap(), vec!["af", "bm_george"]);
        assert_eq!(extract_voice(&path, "bm_george").unwrap(), george);
        assert!(extract_voice(&path, "bm_lewis").is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn voice_tables_of_two_model_versions_never_share_a_cache() {
        let models = Path::new("models");
        assert_eq!(voice_cache_path(models, Path::new("models/voices_npz.bin"), "bm_george"), models.join("bm_george.bin"));
        assert_eq!(
            voice_cache_path(models, Path::new("models/voices-v1.0.bin"), "bm_fable"),
            models.join("voices").join("voices-v1.0").join("bm_fable.bin")
        );
        assert!(is_kokoro_v1(KOKORO_V1_MODEL) && !is_kokoro_v1(KOKORO_V019_MODEL));
    }

    #[test]
    fn real_v1_voice_pack_has_fable_if_present() {
        let path = Path::new(KOKORO_V1_VOICES);
        if !path.exists() {
            return;
        }
        let voices = list_voices(path).unwrap();
        for v in ["bm_fable", "bm_george", "bm_lewis", "bm_daniel"] {
            assert!(voices.contains(&v.to_string()), "{v} in {voices:?}");
        }
        assert_eq!(extract_voice(path, "bm_fable").unwrap().len(), 510 * 256);
    }

    #[test]
    fn real_voice_archive_if_present() {
        let path = Path::new("models/voices_npz.bin");
        if !path.exists() {
            return;
        }
        let voices = list_voices(path).unwrap();
        assert!(voices.contains(&"bm_george".to_string()), "{voices:?}");
        let table = extract_voice(path, "bm_george").unwrap();
        assert_eq!(table.len(), 511 * 256);
        if let Ok(raw) = std::fs::read("models/bm_george.bin") {
            let raw: Vec<f32> = raw.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            assert_eq!(raw, table, "extracted table equals the previously exported raw table");
        }
    }
}
