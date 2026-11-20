//! The installer (Sir, 2026-11-19: "the installer should look JAI themed and automaticly download
//! everything that is needed ... if it has less than 8GB of VRAM than it should download a smaller
//! LLM. The minium VRAM needed to be able to install JAI should be 4GB"). It is JAI.exe itself:
//! started on its own (downloaded from Sir's phone page, `/install`, with nothing of his beside
//! it) or with `--install`. Its window (`gui::run_installer`) logs in at Sir's PC like the phone
//! page, fetches the program and its engines from there (the version Sir runs), then the voice,
//! the ears and the brain that fits the graphics card (`--download-models`), puts J.A.I. in the
//! Start menu and starts him.

use crate::config::{ConfigStore, JaiConfig};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// A graphics card needs this much memory for J.A.I., and this much for the full brain.
const VRAM_AT_LEAST_MB: u64 = 4096;
const VRAM_FULL_BRAIN_MB: u64 = 8192;
/// The brain of a card under 8 GB: Qwen3.5 2B (about 3 GB instead of 4).
pub const SMALL_BRAIN: &str = "qwen3.5:2b";

/// The graphics memory of this PC's largest card in MB, from the display drivers' own entries
/// (every maker's, not only NVIDIA's).
#[cfg(windows)]
pub fn vram_mb() -> Option<u64> {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RRF_RT_REG_QWORD};
    let class = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    (0..16)
        .filter_map(|i| {
            let key = HSTRING::from(format!(r"{class}\{i:04}"));
            let mut value = 0u64;
            let mut size = 8u32;
            // (The 64-bit entry; the 32-bit one stops at 4 GB.)
            let wide = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, &key, &HSTRING::from("HardwareInformation.qwMemorySize"), RRF_RT_REG_QWORD, None, Some(std::ptr::addr_of_mut!(value).cast()), Some(&mut size)) };
            if wide.is_ok() {
                return Some(value);
            }
            let (mut narrow, mut size) = (0u32, 4u32);
            unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, &key, &HSTRING::from("HardwareInformation.MemorySize"), RRF_RT_REG_DWORD | RRF_RT_REG_BINARY, None, Some(std::ptr::addr_of_mut!(narrow).cast()), Some(&mut size)) }.is_ok().then_some(narrow as u64)
        })
        .max()
        .map(|bytes| bytes / (1024 * 1024))
}

#[cfg(not(windows))]
pub fn vram_mb() -> Option<u64> {
    None
}

/// The brain for a card: the full one from 8 GB, the smaller one from 4 GB, none below.
pub fn brain_for(vram_mb: Option<u64>) -> Result<&'static str, String> {
    match vram_mb {
        Some(mb) if mb >= VRAM_FULL_BRAIN_MB => Ok(crate::config::PRIMARY_MODEL),
        Some(mb) if mb >= VRAM_AT_LEAST_MB => Ok(SMALL_BRAIN),
        Some(mb) => Err(format!("J.A.I. needs a graphics card with 4 GB of memory or more; this PC's has {:.1} GB.", mb as f64 / 1024.0)),
        None => Err("J.A.I. needs a graphics card with 4 GB of memory or more; this PC shows none.".into()),
    }
}

/// Where J.A.I. is installed: %LOCALAPPDATA%\Programs\JAI.
pub fn folder() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("JAI"), |d| PathBuf::from(d).join("Programs").join("JAI"))
}

/// What Sir's PC hands a new PC: (where it goes there, where it is here, its size). The program,
/// its libraries, its engines, the voice lock's model and his voice's profile: never Sir's
/// settings, memories, logs or the models every JAI fetches itself.
pub fn files() -> Vec<(String, PathBuf, u64)> {
    let size = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        out.push(("JAI.exe".to_string(), exe.clone(), size(&exe)));
    }
    let root = std::env::current_dir().unwrap_or_default();
    let dlls = std::fs::read_dir(&root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll")));
    let mut found: Vec<PathBuf> = dlls.collect();
    for dir in ["runtime", "models/speaker"] {
        walk(&root.join(dir), &mut found);
    }
    found.push(root.join("assets").join("jai_reference_profile.bin"));
    found.sort();
    for p in found.into_iter().filter(|p| p.is_file()) {
        if let Ok(to) = p.strip_prefix(&root) {
            out.push((to.to_string_lossy().replace('\\', "/"), p.clone(), size(&p)));
        }
    }
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// A path from Sir's list that stays inside the install folder.
fn inside(path: &str) -> Option<&Path> {
    let p = Path::new(path);
    (!path.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)))).then_some(p)
}

/// Fetches J.A.I. from Sir's PC into the current folder (the install folder), then his models,
/// puts him in the Start menu and starts him. `said`: each step, for the window.
pub async fn install(config: &ConfigStore, main: &str, said: impl Fn(String)) -> Result<(), String> {
    let key = crate::fleet::key(config).ok_or("Log in first.")?;
    let main = main.trim().trim_end_matches('/');
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(15)).read_timeout(Duration::from_secs(120)).build().map_err(|e| e.to_string())?;
    let list: Vec<Value> = http.get(format!("{main}/api/pc/files")).header("X-JAI-Key", &key).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    let total: u64 = list.iter().filter_map(|f| f["size"].as_u64()).sum();
    let (mut done, mut told) = (0u64, Instant::now());
    for (i, f) in list.iter().enumerate() {
        let size = f["size"].as_u64().unwrap_or(0);
        let to = f["path"].as_str().and_then(inside).ok_or("Sir's PC named a file outside J.A.I.'s folder")?;
        // (Already here from an earlier try.)
        if std::fs::metadata(to).is_ok_and(|m| m.len() == size) {
            done += size;
            continue;
        }
        if let Some(dir) = to.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let part = to.with_extension("part");
        let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut answer = http.get(format!("{main}/api/pc/file/{i}")).header("X-JAI-Key", &key).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?;
        let mut written = 0u64;
        while let Some(chunk) = answer.chunk().await.map_err(|e| e.to_string())? {
            std::io::Write::write_all(&mut file, &chunk).map_err(|e| e.to_string())?;
            written += chunk.len() as u64;
            if told.elapsed() > Duration::from_millis(500) {
                told = Instant::now();
                said(format!("Fetching J.A.I. from Sir's PC: {} of {} MB", (done + written) / 1_000_000, total / 1_000_000));
            }
        }
        drop(file);
        if written != size {
            let _ = std::fs::remove_file(&part);
            return Err(format!("{} came incomplete ({written} of {size} bytes). Try again.", to.display()));
        }
        std::fs::rename(&part, to).map_err(|e| e.to_string())?;
        done += size;
    }
    config.snapshot().save().map_err(|e| e.to_string())?;
    // His voice, his ears and his brain: from where every J.A.I. fetches them.
    said("Fetching his voice, his ears and his brain (this takes a while)…".into());
    let exe = std::env::current_dir().map_err(|e| e.to_string())?.join("JAI.exe");
    let mut child = std::process::Command::new(&exe).arg("--download-models").spawn().map_err(|e| format!("J.A.I. did not start: {e}"))?;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if let Some(line) = std::fs::read_to_string("jai.log").ok().and_then(|log| log.lines().rev().find(|l| l.contains("[MODELS]") || l.contains("[LLM]")).map(str::to_string)) {
            said(line.split_once("] ").map_or(line.as_str(), |(_, rest)| rest).trim().chars().take(90).collect());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    if !status.success() {
        return Err("Some of his models did not come: start the installer again to finish.".into());
    }
    if let Err(e) = start_menu(&exe) {
        tracing::warn!("[INSTALL] No Start menu entry: {e}");
    }
    std::process::Command::new(&exe).spawn().map_err(|e| format!("J.A.I. did not start: {e}"))?;
    said("J.A.I. is installed and starts now.".into());
    Ok(())
}

/// J.A.I. in the Start menu.
#[cfg(windows)]
fn start_menu(exe: &Path) -> Result<(), String> {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    let programs = PathBuf::from(std::env::var_os("APPDATA").ok_or("no APPDATA")?).join(r"Microsoft\Windows\Start Menu\Programs");
    let folder = exe.parent().unwrap_or(Path::new("."));
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        link.SetPath(&HSTRING::from(exe.as_os_str())).map_err(|e| e.to_string())?;
        link.SetWorkingDirectory(&HSTRING::from(folder.as_os_str())).map_err(|e| e.to_string())?;
        link.SetDescription(&HSTRING::from("J.A.I.")).map_err(|e| e.to_string())?;
        link.cast::<IPersistFile>().map_err(|e| e.to_string())?.Save(&HSTRING::from(programs.join("JAI.lnk").as_os_str()), true).map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
fn start_menu(_exe: &Path) -> Result<(), String> {
    Ok(())
}

/// Sir's PC's address, when this program was downloaded from his phone page (Windows notes
/// where a download came from).
fn downloaded_from() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let note = std::fs::read_to_string(format!("{}:Zone.Identifier", exe.display())).ok()?;
    let url = note.lines().find_map(|l| l.strip_prefix("HostUrl="))?.trim();
    url.ends_with("/install").then(|| url.trim_end_matches("/install").to_string())
}

/// The installer: its window, in the install folder.
pub fn run() -> ExitCode {
    let into = folder();
    if let Err(e) = std::fs::create_dir_all(&into).and_then(|_| std::env::set_current_dir(&into)) {
        eprintln!("J.A.I. cannot be installed into {}: {e}", into.display());
        return ExitCode::FAILURE;
    }
    let vram = vram_mb();
    let brain = brain_for(vram);
    let mut cfg = JaiConfig::load();
    if let Ok(brain) = brain {
        cfg.primary_model = brain.into();
        cfg.fallback_model = brain.into();
    }
    if let Some(main) = downloaded_from().filter(|_| cfg.main_pc.is_empty()) {
        cfg.main_pc = main;
    }
    let card = vram.map_or_else(|| "No graphics card found".to_string(), |mb| format!("Graphics memory {:.0} GB", mb as f64 / 1024.0));
    let Ok(rt) = tokio::runtime::Runtime::new() else { return ExitCode::FAILURE };
    match crate::gui::run_installer(rt.handle().clone(), ConfigStore::new(cfg), card, brain, into) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("The installer's window did not open: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brain_fits_the_graphics_card() {
        assert_eq!(brain_for(Some(12 * 1024)), Ok(crate::config::PRIMARY_MODEL));
        assert_eq!(brain_for(Some(8 * 1024)), Ok(crate::config::PRIMARY_MODEL));
        assert_eq!(brain_for(Some(6 * 1024)), Ok(SMALL_BRAIN));
        assert_eq!(brain_for(Some(4 * 1024)), Ok(SMALL_BRAIN));
        assert!(brain_for(Some(2 * 1024)).unwrap_err().contains("2.0 GB"));
        assert!(brain_for(None).is_err());
        // (This PC's own card is read.)
        assert!(vram_mb().is_some_and(|mb| mb >= 1024), "{:?}", vram_mb());
    }

    #[test]
    fn a_new_pc_gets_the_program_and_never_sirs_own_files() {
        for (to, from, _) in files() {
            assert!(inside(&to).is_some(), "{to}");
            let lower = to.to_lowercase();
            for private in ["jai_config", "jai_memory", ".log", "conversation", "models/llm", "src/", "target/"] {
                assert!(!lower.contains(private), "{to} ({})", from.display());
            }
        }
        for bad in ["../x.dll", "/etc/passwd", r"C:\Windows\x.dll", "", "runtime/../../x"] {
            assert!(inside(bad).is_none(), "{bad}");
        }
        assert!(inside("runtime/ollama/ollama.exe").is_some());
    }
}
