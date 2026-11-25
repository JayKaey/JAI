//! The installer (Sir, 2026-11-19: "the installer should look JAI themed and automaticly download
//! everything that is needed ... if it has less than 8GB of VRAM than it should download a smaller
//! LLM. The minium VRAM needed to be able to install JAI should be 4GB"; 2026-11-20: "when
//! installing JAI everything should get downloaded from the github repo"). It is JAI.exe itself,
//! started by the setup from GitHub (`setup/`), on its own or with `--install`. Its window
//! (`gui::run_installer`) logs in at Sir's PC like the phone page (for the account alone), puts
//! this program into J.A.I.'s folder, unpacks eSpeak NG out of the setup, fetches the engine and
//! the voice lock's model from GitHub, then the voice, the ears and the brain that fits the
//! graphics card (`--download-models`), puts J.A.I. in the Start menu and starts him. Nothing is
//! downloaded from Sir's PC.

use crate::config::{ConfigStore, JaiConfig};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

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

/// What a new PC fetches from GitHub (address, size, SHA-256): the engine, the Ollama this J.A.I.
/// runs with, from Ollama's own release; and the voice lock's model from sherpa-onnx's (its tag
/// is spelt so there).
#[cfg(windows)]
const ENGINE: (&str, u64, &str) = ("https://github.com/ollama/ollama/releases/download/v0.40.5/ollama-windows-amd64.zip", 1_471_226_485, "5134ce7e3bc8e94d0494721c799ec293b429450d0c52461b0ee3647dd3bb4bb7");
const VOICE_LOCK: (&str, u64, &str) = ("https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx", 26_530_550, "e9848563da86f263117134dfd7ad63c92355b37de492b55e325400c9d9c39012");

/// The setup a new Windows PC starts with: the newest on J.A.I.'s releases page (the phone page's
/// `/install` sends there).
pub const SETUP: &str = "https://github.com/JayKaey/JAI/releases/latest/download/JAI-setup.exe";

/// A file from GitHub into `to`, its size and SHA-256 checked (one that is here and right
/// already stays, a damaged one goes); `what` names it in the window.
async fn fetch(http: &reqwest::Client, (url, size, sha): (&str, u64, &str), to: &Path, what: &str, said: &(dyn Fn(String) + Sync)) -> Result<(), String> {
    if std::fs::metadata(to).is_ok_and(|m| m.len() == size) && sha256(to).is_ok_and(|s| s == sha) {
        return Ok(());
    }
    crate::models::download_to(http, url, to, Some(size), &|got, _| said(format!("Fetching {what} from GitHub: {} of {} MB", got / 1_000_000, size / 1_000_000)))
        .await
        .map_err(|e| format!("{what} did not come ({e}). Check the internet connection and start the installer again; it goes on where it stopped."))?;
    if sha256(to)? != sha {
        let _ = std::fs::remove_file(to);
        return Err(format!("{what} came damaged. Start the installer again."));
    }
    Ok(())
}

/// A file's SHA-256, in hex.
pub(crate) fn sha256(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        match std::io::Read::read(&mut file, &mut buf).map_err(|e| e.to_string())? {
            0 => break,
            n => hash.update(&buf[..n]),
        }
    }
    Ok(hash.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// This program (the setup's) into J.A.I.'s folder as `exe`: a copy, never onto itself.
fn program(exe: &Path) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if me.canonicalize().ok() != exe.canonicalize().ok() {
        let part = exe.with_extension("part");
        std::fs::copy(&me, &part).and_then(|_| std::fs::rename(&part, exe)).map_err(|e| format!("J.A.I. could not be put into {}: {e}", exe.display()))?;
    }
    Ok(())
}

/// Windows' own tar (Windows 10 and later carry one that reads zip).
#[cfg(windows)]
fn tar() -> PathBuf {
    std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from("tar.exe"), |root| PathBuf::from(root).join("System32").join("tar.exe"))
}

/// Unpacks a zip (Windows) or a .tar.zst (Linux, with zstd installed) into `into`.
async fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let mut command = tokio::process::Command::new(tar());
    #[cfg(not(windows))]
    let mut command = tokio::process::Command::new("tar");
    #[cfg(not(windows))]
    command.arg("--zstd");
    crate::shell::hide_console(&mut command);
    let status = command.arg("-xf").arg(archive).arg("-C").arg(into).status().await.map_err(|e| format!("tar did not start: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} could not be unpacked ({status}).", archive.display()))
    }
}

/// Installs J.A.I. into the current folder (the install folder) from GitHub, after the login at
/// Sir's PC: this program, his engines, the voice lock's model, then his models; puts him in the
/// Start menu and starts him. `said`: each step, for the window.
pub async fn install(config: &ConfigStore, said: impl Fn(String) + Sync) -> Result<(), String> {
    crate::fleet::key(config).ok_or("Log in first.")?;
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(15)).read_timeout(Duration::from_secs(120)).build().map_err(|e| e.to_string())?;
    config.snapshot().save().map_err(|e| e.to_string())?;
    let exe = std::env::current_dir().map_err(|e| e.to_string())?.join(PROGRAM);
    program(&exe)?;
    engines(&http, config, &said).await?;
    let speaker = Path::new(&config.read(|c| c.silero_vad_model_path.clone())).parent().unwrap_or(Path::new("models")).join("speaker").join(crate::speaker_id::MODEL);
    fetch(&http, VOICE_LOCK, &speaker, "the voice lock", &said).await?;
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

/// Windows: eSpeak NG out of the setup (beside this program) and the engine from Ollama's
/// release, each where it is missing.
#[cfg(windows)]
async fn engines(http: &reqwest::Client, config: &ConfigStore, said: &(dyn Fn(String) + Sync)) -> Result<(), String> {
    let espeak = Path::new("runtime").join("espeak-ng");
    if !espeak.join("libespeak-ng.dll").is_file() {
        let zip = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("espeak-ng.zip");
        if !zip.is_file() {
            return Err(format!("eSpeak NG is missing: install J.A.I. with the setup from GitHub ({SETUP})."));
        }
        unpack(&zip, &espeak).await?;
    }
    let dir = PathBuf::from(config.read(|c| c.llm_runtime_dir.clone()));
    if crate::runtime::engine_binary(&dir).is_some() {
        return Ok(());
    }
    let zip = dir.with_extension("zip");
    fetch(http, ENGINE, &zip, "his engine (Ollama)", said).await?;
    said("Unpacking his engine…".into());
    let unpacked = unpack(&zip, &dir).await;
    let _ = std::fs::remove_file(&zip);
    unpacked?;
    crate::runtime::engine_binary(&dir).map(|_| ()).ok_or("His engine came without its program. Start the installer again.".into())
}

/// Linux: the engine, when the folder has none, from Ollama's own releases (the newest).
#[cfg(not(windows))]
async fn engines(http: &reqwest::Client, config: &ConfigStore, said: &(dyn Fn(String) + Sync)) -> Result<(), String> {
    let dir = PathBuf::from(config.read(|c| c.llm_runtime_dir.clone()));
    if crate::runtime::engine_binary(&dir).is_some() {
        return Ok(());
    }
    let arch = if std::env::consts::ARCH == "aarch64" { "arm64" } else { "amd64" };
    let url = format!("https://github.com/ollama/ollama/releases/latest/download/ollama-linux-{arch}.tar.zst");
    let archive = dir.join("ollama.tar.zst");
    crate::models::download_to(http, &url, &archive, None, &|got, of| said(format!("Fetching his engine (Ollama for Linux): {} of {} MB", got / 1_000_000, of.unwrap_or(0) / 1_000_000)))
        .await
        .map_err(|e| format!("His engine did not come ({e}). Check the internet connection and start the installer again; it goes on where it stopped."))?;
    said("Unpacking his engine…".into());
    let unpacked = unpack(&archive, &dir).await;
    let _ = std::fs::remove_file(&archive);
    match unpacked {
        Ok(()) if crate::runtime::engine_binary(&dir).is_some() => Ok(()),
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
pub(crate) fn menu_entry(exe: &Path) -> String {
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

    /// A new PC takes nothing from Sir's PC (Sir, 2026-11-20): what it fetches is a file of a
    /// release on GitHub, held to its size and SHA-256.
    #[test]
    fn everything_a_new_pc_fetches_comes_from_github() {
        #[cfg(windows)]
        let all = [ENGINE, VOICE_LOCK];
        #[cfg(not(windows))]
        let all = [VOICE_LOCK];
        for (url, size, sha) in all {
            assert!(url.starts_with("https://github.com/") && url.contains("/releases/download/"), "{url}");
            assert!(size > 1_000_000, "{url}");
            assert!(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{url}");
        }
        assert!(SETUP.starts_with("https://github.com/JayKaey/JAI/releases/"));
        assert!(folder().ends_with(if cfg!(windows) { "JAI" } else { "jai" }));
    }

    /// A file is kept only whole and as published: a damaged one is thrown away (from a server
    /// on this PC that answers "abc").
    #[tokio::test]
    async fn a_download_is_kept_only_when_its_digest_is_right() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = [0u8; 2048];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc").await;
            }
        });
        let dir = std::env::temp_dir().join(format!("jai-fetch-{}", std::process::id()));
        let to = dir.join("abc.bin");
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        fetch(&http, (&url, 3, abc), &to, "a test file", &|_| {}).await.unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"abc");
        // Here and right already: it stays.
        fetch(&http, ("http://127.0.0.1:1/nothing", 3, abc), &to, "a test file", &|_| {}).await.unwrap();
        // Not what was published: it goes, and the window says so.
        std::fs::remove_file(&to).unwrap();
        let damaged = fetch(&http, (&url, 3, &"0".repeat(64)), &to, "a test file", &|_| {}).await.unwrap_err();
        assert!(damaged.contains("damaged"), "{damaged}");
        assert!(!to.exists() && !dir.join("abc.bin.part").exists());
        // Another size than published: not fetched again and again, not kept.
        let other = fetch(&http, (&url, 4, abc), &to, "a test file", &|_| {}).await.unwrap_err();
        assert!(other.contains("it has 3 bytes, not 4"), "{other}");
        assert!(!to.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stream that breaks off (reqwest: "error decoding response body") goes on by a Range.
    #[tokio::test]
    async fn a_broken_download_goes_on_where_it_stopped() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let ranges = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = ranges.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = [0u8; 2048];
                let n = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]).to_lowercase();
                let answer: &[u8] = if request.contains("range: bytes=2-") {
                    seen.lock().unwrap().push(2);
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nConnection: close\r\n\r\nc"
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nab" // the line drops after two bytes
                };
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, answer).await;
            }
        });
        let dir = std::env::temp_dir().join(format!("jai-resume-{}", std::process::id()));
        let to = dir.join("abc.bin");
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        // What 3.4.0 did, one GET: the cut stream is the error a friend's installer showed.
        let mut answer = http.get(&url).send().await.unwrap();
        let cut = loop {
            match answer.chunk().await {
                Ok(Some(_)) => continue,
                Ok(None) => panic!("the stream should break off"),
                Err(e) => break e.to_string(),
            }
        };
        assert!(cut.starts_with("error decoding response body"), "{cut}");
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        fetch(&http, (&url, 3, abc), &to, "a test file", &|_| {}).await.unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"abc");
        assert_eq!(*ranges.lock().unwrap(), vec![2], "it went on from byte 2, once");
        assert!(!dir.join("abc.bin.part").exists());
        // A model, its size unknown: the server's own length decides when it is whole.
        let model = dir.join("model.onnx");
        assert_eq!(crate::models::download_to(&http, &url, &model, None, &|_, _| {}).await, Ok(3));
        assert_eq!(std::fs::read(&model).unwrap(), b"abc");
        assert_eq!(*ranges.lock().unwrap(), vec![2, 2]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Windows' own tar unpacks a zip with its folders (eSpeak NG's data, the engine's libraries).
    #[cfg(windows)]
    #[tokio::test]
    async fn a_zip_is_unpacked_with_its_folders() {
        let dir = std::env::temp_dir().join(format!("jai-unpack-{}", std::process::id()));
        let (from, into, zip) = (dir.join("from"), dir.join("into"), dir.join("espeak-ng.zip"));
        std::fs::create_dir_all(from.join("espeak-ng-data")).unwrap();
        std::fs::write(from.join("libespeak-ng.dll"), b"dll").unwrap();
        std::fs::write(from.join("espeak-ng-data").join("en_dict"), b"dict").unwrap();
        assert!(std::process::Command::new(tar()).arg("-a").arg("-cf").arg(&zip).arg("-C").arg(&from).arg(".").status().unwrap().success());
        unpack(&zip, &into).await.unwrap();
        assert_eq!(std::fs::read(into.join("libespeak-ng.dll")).unwrap(), b"dll");
        assert_eq!(std::fs::read(into.join("espeak-ng-data").join("en_dict")).unwrap(), b"dict");
        assert!(unpack(&dir.join("no such.zip"), &into).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
