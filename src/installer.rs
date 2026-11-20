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

/// On Linux AMD's and Intel's drivers tell it in /sys, NVIDIA's through nvidia-smi.
#[cfg(not(windows))]
pub fn vram_mb() -> Option<u64> {
    let sys = std::fs::read_dir("/sys/class/drm").into_iter().flatten().flatten().filter_map(|card| {
        let bytes = std::fs::read_to_string(card.path().join("device").join("mem_info_vram_total")).ok()?;
        bytes.trim().parse::<u64>().ok().map(|b| b / (1024 * 1024))
    });
    let smi = std::process::Command::new("nvidia-smi").args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"]).output();
    let nvidia = smi.map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.trim().parse::<u64>().ok()).collect::<Vec<_>>()).unwrap_or_default();
    sys.chain(nvidia).max()
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

/// The program's own name in its folder.
const PROGRAM: &str = if cfg!(windows) { "JAI.exe" } else { "jai" };

/// Where J.A.I. is installed: %LOCALAPPDATA%\Programs\JAI, on Linux ~/.local/share/jai.
pub fn folder() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs"));
    #[cfg(not(windows))]
    let base = data_home();
    base.map_or_else(|| PathBuf::from("JAI"), |d| d.join(if cfg!(windows) { "JAI" } else { "jai" }))
}

/// Linux's folder for programs' data: $XDG_DATA_HOME, else ~/.local/share.
#[cfg(not(windows))]
pub(crate) fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
}

/// Whether a file of Sir's PC is for this one: Linux takes his data, never his Windows program,
/// its libraries or its engines (J.A.I. there is this program itself, his engine Ollama's own).
fn for_this_system(path: &str, windows: bool) -> bool {
    windows || !(path == "JAI.exe" || path.ends_with(".dll") || path.starts_with("runtime/"))
}

/// Streams `answer` into `to` through a `.part` file, telling `said` the bytes so far (every
/// half second, by `told`); a file that comes short is not kept.
async fn save(mut answer: reqwest::Response, to: &Path, size: u64, told: &mut Instant, said: &(dyn Fn(u64) + Sync)) -> Result<(), String> {
    let part = to.with_extension("part");
    let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut written = 0u64;
    while let Some(chunk) = answer.chunk().await.map_err(|e| e.to_string())? {
        std::io::Write::write_all(&mut file, &chunk).map_err(|e| e.to_string())?;
        written += chunk.len() as u64;
        if told.elapsed() > Duration::from_millis(500) {
            *told = Instant::now();
            said(written);
        }
    }
    drop(file);
    if written != size {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{} came incomplete ({written} of {size} bytes). Try again.", to.display()));
    }
    std::fs::rename(&part, to).map_err(|e| e.to_string())
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
pub async fn install(config: &ConfigStore, main: &str, said: impl Fn(String) + Sync) -> Result<(), String> {
    let key = crate::fleet::key(config).ok_or("Log in first.")?;
    let main = main.trim().trim_end_matches('/');
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(15)).read_timeout(Duration::from_secs(120)).build().map_err(|e| e.to_string())?;
    let list: Vec<Value> = http.get(format!("{main}/api/pc/files")).header("X-JAI-Key", &key).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    // (By Sir's numbering: a file is asked for by its place in his list.)
    let list: Vec<(usize, &Value)> = list.iter().enumerate().filter(|(_, f)| f["path"].as_str().is_some_and(|p| for_this_system(p, cfg!(windows)))).collect();
    let total: u64 = list.iter().filter_map(|(_, f)| f["size"].as_u64()).sum();
    let (mut done, mut told) = (0u64, Instant::now());
    for (i, f) in list {
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
        let answer = http.get(format!("{main}/api/pc/file/{i}")).header("X-JAI-Key", &key).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?;
        save(answer, to, size, &mut told, &|written| said(format!("Fetching J.A.I. from Sir's PC: {} of {} MB", (done + written) / 1_000_000, total / 1_000_000))).await?;
        done += size;
    }
    config.snapshot().save().map_err(|e| e.to_string())?;
    let exe = std::env::current_dir().map_err(|e| e.to_string())?.join(PROGRAM);
    #[cfg(not(windows))]
    linux_program_and_engine(&http, config, &exe, &said).await?;
    // His voice, his ears and his brain: from where every J.A.I. fetches them.
    said("Fetching his voice, his ears and his brain (this takes a while)…".into());
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

/// Linux: this program goes into the folder as `jai`, and the engine, when the folder has none,
/// comes from Ollama's own releases (Sir's PC has only its Windows build).
#[cfg(not(windows))]
async fn linux_program_and_engine(http: &reqwest::Client, config: &ConfigStore, exe: &Path, said: &(dyn Fn(String) + Sync)) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    // (Never copied onto itself: that would empty it.)
    if me.canonicalize().ok() != exe.canonicalize().ok() {
        let part = exe.with_extension("part");
        std::fs::copy(&me, &part).and_then(|_| std::fs::rename(&part, exe)).map_err(|e| format!("J.A.I. could not be put into {}: {e}", exe.display()))?;
    }
    let dir = PathBuf::from(config.read(|c| c.llm_runtime_dir.clone()));
    if crate::runtime::engine_binary(&dir).is_some() {
        return Ok(());
    }
    let arch = if std::env::consts::ARCH == "aarch64" { "arm64" } else { "amd64" };
    let url = format!("https://github.com/ollama/ollama/releases/latest/download/ollama-linux-{arch}.tar.zst");
    let answer = http.get(&url).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?;
    let size = answer.content_length().ok_or("Ollama's download did not say its size. Try again.")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let archive = dir.join("ollama.tar.zst");
    save(answer, &archive, size, &mut Instant::now(), &|got| said(format!("Fetching his engine (Ollama for Linux): {} of {} MB", got / 1_000_000, size / 1_000_000))).await?;
    said("Unpacking his engine…".into());
    let unpacked = tokio::process::Command::new("tar").arg("--zstd").arg("-xf").arg(&archive).arg("-C").arg(&dir).status().await;
    let _ = std::fs::remove_file(&archive);
    match unpacked {
        Ok(status) if status.success() && crate::runtime::engine_binary(&dir).is_some() => Ok(()),
        _ => Err("His engine could not be unpacked: install zstd (on Arch `sudo pacman -S zstd`, on Ubuntu `sudo apt install zstd`) and start the installer again.".into()),
    }
}

/// J.A.I. in the applications menu.
#[cfg(not(windows))]
fn start_menu(exe: &Path) -> Result<(), String> {
    let apps = data_home().ok_or("no HOME")?.join("applications");
    std::fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    std::fs::write(apps.join("jai.desktop"), menu_entry(exe)).map_err(|e| e.to_string())
}

/// The menu entry for the program at `exe`. KDE gives Plasma's window list, pressed keys and
/// screenshots only to a program whose entry asks for them (KWin finds it by the path in Exec).
#[cfg(not(windows))]
fn menu_entry(exe: &Path) -> String {
    let folder = exe.parent().unwrap_or(Path::new("."));
    format!(
        "[Desktop Entry]\nType=Application\nName=J.A.I.\nComment=Local voice assistant\nExec=\"{}\"\nPath={}\nTerminal=false\nCategories=Utility;\nX-KDE-Wayland-Interfaces=org_kde_plasma_window_management,org_kde_kwin_fake_input\nX-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2\n",
        exe.display(),
        folder.display()
    )
}

/// Keeps the menu entry naming this program where it runs now (written when it did not):
/// whether it was written.
#[cfg(not(windows))]
pub fn keep_menu_entry() -> bool {
    let (Ok(exe), Some(home)) = (std::env::current_exe(), data_home()) else { return false };
    if std::fs::read_to_string(home.join("applications").join("jai.desktop")).is_ok_and(|now| now == menu_entry(&exe)) {
        return false;
    }
    start_menu(&exe).is_ok()
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
    // (An update keeps the brain this PC has: it may be one Sir chose.)
    let update = Path::new(crate::config::CONFIG_FILE).is_file();
    let mut cfg = JaiConfig::load();
    if let (Ok(brain), false) = (&brain, update) {
        cfg.primary_model = (*brain).into();
        cfg.fallback_model = (*brain).into();
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

    #[test]
    fn a_linux_pc_takes_sirs_data_and_none_of_his_windows_programs() {
        for windows_only in ["JAI.exe", "onnxruntime.dll", "runtime/ollama/ollama.exe", "runtime/ollama/lib/ollama/LLAMA_CPP_LICENSE", "runtime/espeak-ng/espeak-ng-data/en_dict"] {
            assert!(!for_this_system(windows_only, false), "{windows_only}");
            assert!(for_this_system(windows_only, true), "{windows_only}");
        }
        for data in ["models/speaker/voxceleb_resnet34.onnx", "assets/jai_reference_profile.bin"] {
            assert!(for_this_system(data, false), "{data}");
        }
        assert!(folder().ends_with(if cfg!(windows) { "JAI" } else { "jai" }));
    }
}
