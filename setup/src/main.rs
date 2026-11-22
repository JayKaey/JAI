//! J.A.I.'s setup: one file that installs him on a new PC or updates him where he is installed.
//! It carries J.A.I.'s own installer (his program, with the libraries it starts with), unpacks it
//! into a folder of its own and starts it with `--install`: that window logs in at Sir's PC (for
//! the account alone) and fetches everything else from GitHub and from where his models come
//! (eSpeak NG rides in this file as `espeak-ng.zip`, beside the installer). A J.A.I. that runs from
//! the install folder is asked to quit first, so his program can be replaced (and he starts again
//! when the update is called off). On Arch the packages he needs come first (pacman, the password
//! asked by polkit). `--check` (the release workflow's): unpack and start the installer with
//! `--help` only, nothing asked, nothing stopped.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[allow(dead_code)] // (build.rs packs, this unpacks)
mod pack;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.bin"));

/// J.A.I.'s program, in the payload and in his folder.
const PROGRAM: &str = if cfg!(windows) { "JAI.exe" } else { "jai" };
const TITLE: &str = "J.A.I. setup";

fn main() -> ExitCode {
    let check = env::args().skip(1).any(|a| a == "--check");
    match run(check) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tell(&e, check);
            ExitCode::FAILURE
        }
    }
}

fn run(check: bool) -> Result<(), String> {
    let files = pack::unpack(PAYLOAD)?;
    if !files.iter().any(|(name, _)| name == PROGRAM) {
        return Err("This setup holds no J.A.I. (it was built without him).".into());
    }
    #[cfg(target_os = "linux")]
    if let Err(e) = packages::install(check) {
        if check {
            return Err(e);
        }
        tell(&e, check);
    }
    let folder = folder();
    let stopped = !check && make_way(&folder)?;
    let temp = env::temp_dir().join(format!("jai-setup-{}", std::process::id()));
    let result = unpack_into(&files, &temp).and_then(|program| {
        #[cfg(windows)]
        if !check {
            libraries(&files, &folder);
        }
        let status = Command::new(&program).arg(if check { "--help" } else { "--install" }).status().map_err(|e| format!("J.A.I.'s installer did not start: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("J.A.I.'s installer stopped ({status}). Start the setup again."))
        }
    });
    remove(&temp);
    // (The update was called off: he runs again as he did.)
    if stopped && running_from(&folder).is_empty() {
        let _ = Command::new(folder.join(PROGRAM)).current_dir(&folder).spawn();
    }
    result
}

/// Where J.A.I. is installed (as his `installer::folder`): %LOCALAPPDATA%\Programs\JAI, on Linux
/// $XDG_DATA_HOME/jai or ~/.local/share/jai.
fn folder() -> PathBuf {
    #[cfg(windows)]
    let base = env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs"));
    #[cfg(not(windows))]
    let base = env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")));
    base.map_or_else(|| PathBuf::from("JAI"), |d| d.join(if cfg!(windows) { "JAI" } else { "jai" }))
}

/// Whether the program at `path` lies in `folder` (Windows' paths in any case).
fn inside(path: &Path, folder: &Path) -> bool {
    if cfg!(windows) {
        let (path, folder) = (path.to_string_lossy().to_lowercase(), folder.to_string_lossy().to_lowercase());
        path.strip_prefix(folder.trim_end_matches('\\')).is_some_and(|rest| rest.starts_with('\\'))
    } else {
        path.starts_with(folder)
    }
}

/// The processes that run a program from `folder` (J.A.I. and his engine).
fn running_from(folder: &Path) -> Vec<(u32, PathBuf)> {
    os::processes().into_iter().filter(|(pid, path)| *pid != std::process::id() && inside(path, folder)).collect()
}

/// Waits up to `secs` until nothing runs from `folder`: whether it came to that.
fn gone(folder: &Path, secs: u64) -> bool {
    let until = Instant::now() + Duration::from_secs(secs);
    loop {
        if running_from(folder).is_empty() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Asks a J.A.I. that runs from `folder` to quit and waits until nothing of his runs there:
/// whether one was running. A J.A.I. that runs from elsewhere (Sir's own PC) is left alone.
fn make_way(folder: &Path) -> Result<bool, String> {
    if let Some(there) = os::elsewhere(folder) {
        return Err(format!(
            "J.A.I. already runs on this PC, from {}.\n\nThis setup puts him into {} (on a PC other than Sir's). Quit the J.A.I. that runs, then start the setup again.",
            there.parent().unwrap_or(&there).display(),
            folder.display()
        ));
    }
    if running_from(folder).is_empty() {
        return Ok(false);
    }
    os::ask_to_quit();
    if gone(folder, 45) {
        return Ok(true);
    }
    #[cfg(unix)]
    {
        for (pid, _) in running_from(folder) {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
        if gone(folder, 15) {
            return Ok(true);
        }
    }
    Err("J.A.I. is still running, so he cannot be updated. Quit him (his tray icon: Quit J.A.I.), then start the setup again.".into())
}

/// Unpacks the installer into `dir`: its program.
fn unpack_into(files: &[(String, Vec<u8>)], dir: &Path) -> Result<PathBuf, String> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).map_err(|e| format!("The setup could not make {}: {e}", dir.display()))?;
    for (name, data) in files {
        fs::write(dir.join(name), data).map_err(|e| format!("The setup could not unpack {name} into {}: {e}", dir.display()))?;
    }
    let program = dir.join(PROGRAM);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).map_err(|e| format!("{}: {e}", program.display()))?;
    }
    Ok(program)
}

/// The libraries J.A.I. starts with (DirectML, the Visual C++ runtime), into his folder where
/// they are missing.
#[cfg(windows)]
fn libraries(files: &[(String, Vec<u8>)], folder: &Path) {
    if fs::create_dir_all(folder).is_err() {
        return;
    }
    for (name, data) in files.iter().filter(|(name, _)| name.to_lowercase().ends_with(".dll")) {
        let to = folder.join(name);
        if !to.exists() {
            let _ = fs::write(&to, data);
        }
    }
}

/// Removes the unpacked installer (Windows lets go of a program a moment after it ended).
fn remove(dir: &Path) {
    for _ in 0..10 {
        if fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Tells the person at the PC (a check only writes it out).
fn tell(text: &str, check: bool) {
    eprintln!("{text}");
    if !check {
        os::message(text);
    }
}

#[cfg(windows)]
mod os {
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, MessageBoxW, PostMessageW, RegisterWindowMessageW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// The running J.A.I.'s tray window and its process (the class is his `tray.rs`'s).
    fn tray() -> Option<(windows_sys::Win32::Foundation::HWND, u32)> {
        // SAFETY: a window looked up by its class; the strings live through the call.
        let hwnd = unsafe { FindWindowW(wide("JaiTrayWindow").as_ptr(), std::ptr::null()) };
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        // SAFETY: `pid` outlives the call.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        Some((hwnd, pid))
    }

    /// The program a process runs.
    fn image(pid: u32) -> Option<PathBuf> {
        // SAFETY: the handle is closed here; the buffer and its length outlive the call.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
            CloseHandle(process);
            (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
        }
    }

    pub fn processes() -> Vec<(u32, PathBuf)> {
        let mut out = Vec::new();
        // SAFETY: the snapshot is closed here; `entry` carries its own size as the API asks.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snapshot, &mut entry) != 0;
            while more {
                if let Some(path) = image(entry.th32ProcessID) {
                    out.push((entry.th32ProcessID, path));
                }
                more = Process32NextW(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
        }
        out
    }

    /// The program of a J.A.I. that runs from somewhere other than `folder`.
    pub fn elsewhere(folder: &Path) -> Option<PathBuf> {
        tray().and_then(|(_, pid)| image(pid)).filter(|path| !super::inside(path, folder))
    }

    /// His tray takes "JAI_GIVE_WAY" as Quit.
    pub fn ask_to_quit() {
        if let Some((hwnd, _)) = tray() {
            // SAFETY: a posted message to a window; the string lives through the call.
            unsafe { PostMessageW(hwnd, RegisterWindowMessageW(wide("JAI_GIVE_WAY").as_ptr()), 0, 0) };
        }
    }

    pub fn message(text: &str) {
        // SAFETY: both strings live through the call.
        unsafe { MessageBoxW(std::ptr::null_mut(), wide(text).as_ptr(), wide(super::TITLE).as_ptr(), MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND) };
    }
}

#[cfg(unix)]
mod os {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    pub fn processes() -> Vec<(u32, PathBuf)> {
        let entries = std::fs::read_dir("/proc").into_iter().flatten().flatten();
        entries.filter_map(|e| Some((e.file_name().to_str()?.parse().ok()?, std::fs::read_link(e.path().join("exe")).ok()?))).collect()
    }

    /// The program of a J.A.I. that runs from somewhere other than `folder` (a replaced program
    /// shows as "jai (deleted)").
    pub fn elsewhere(folder: &Path) -> Option<PathBuf> {
        let jai = |path: &Path| path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n == "jai" || n == "jai (deleted)");
        processes().into_iter().map(|(_, path)| path).find(|path| jai(path) && !super::inside(path, folder))
    }

    /// His D-Bus object takes Quit (`tray.rs`, `org.jai.JAI`).
    pub fn ask_to_quit() {
        let _ = Command::new("busctl").args(["--user", "call", "org.jai.JAI", "/org/jai/JAI", "org.jai.JAI", "Quit"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }

    /// KDE's dialog, else GNOME's; the text is on stderr either way.
    pub fn message(text: &str) {
        let shown = |program: &str, args: &[&str]| Command::new(program).args(args).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        let _ = shown("kdialog", &["--title", super::TITLE, "--msgbox", text]) || shown("zenity", &["--info", "--no-markup", "--title", super::TITLE, "--text", text]);
    }
}

#[cfg(target_os = "linux")]
mod packages {
    use std::os::unix::fs::MetadataExt;
    use std::process::{Command, Stdio};

    /// What J.A.I. needs on Arch (the README's Linux section): the libraries he starts with, his
    /// voice, Ollama's unpacking, the mixer, reading the screen, updates, where the PC is, links
    /// and the media grabber.
    pub const NEEDED: &[&str] = &["alsa-lib", "opus", "espeak-ng", "zstd", "libpulse", "tesseract", "tesseract-data-eng", "tesseract-data-deu", "pacman-contrib", "discover", "geoclue", "xdg-utils", "yt-dlp", "ffmpeg", "wl-clipboard"];

    /// Installs those not installed yet (pacman as root asks for the password through polkit).
    pub fn install(check: bool) -> Result<(), String> {
        let Ok(test) = Command::new("pacman").arg("-T").args(NEEDED).stderr(Stdio::null()).output() else {
            return Err(format!("This setup installs what J.A.I. needs with Arch's pacman, which this PC does not have. Install these yourself: {}", NEEDED.join(" ")));
        };
        let missing: Vec<String> = String::from_utf8_lossy(&test.stdout).split_whitespace().map(str::to_string).collect();
        if missing.is_empty() {
            return Ok(());
        }
        let root = std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0);
        if !check {
            let _ = Command::new("notify-send").args([super::TITLE, "Installing the programs J.A.I. needs…"]).stderr(Stdio::null()).status();
        }
        let mut pacman = Command::new(if root { "pacman" } else { "pkexec" });
        if !root {
            pacman.arg("pacman");
        }
        match pacman.args(["-S", "--needed", "--noconfirm"]).args(&missing).status() {
            Ok(status) if status.success() => Ok(()),
            _ => Err(format!("J.A.I. needs a few more programs. Install them in a terminal:\n\nsudo pacman -Syu --needed {}", missing.join(" "))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_comes_out_as_it_went_in() {
        let files = vec![("JAI.exe".to_string(), b"MZ program".repeat(1000)), ("msvcp140.dll".to_string(), vec![]), ("vcruntime140.dll".to_string(), (0..=255).collect())];
        assert_eq!(pack::unpack(&pack::pack(&files)).unwrap(), files);
        assert_eq!(pack::unpack(&pack::pack(&[])).unwrap(), vec![]);
        // A cut or foreign payload, and names that would leave the folder, are refused.
        let packed = pack::pack(&files);
        assert!(pack::unpack(&packed[..packed.len() / 2]).is_err());
        assert!(pack::unpack(b"not a payload").is_err());
        for name in ["../jai", "a/b", r"..\jai", "C:x", "", ".."] {
            assert!(pack::unpack(&pack::pack(&[(name.to_string(), vec![1])])).is_err(), "{name}");
        }
        // (A build without JAI_SETUP_PAYLOAD holds nothing, and says so.)
        assert!(pack::unpack(PAYLOAD).is_ok());
    }

    #[test]
    fn only_his_own_folder_counts() {
        if cfg!(windows) {
            let folder = Path::new(r"C:\Users\Max\AppData\Local\Programs\JAI");
            assert!(inside(Path::new(r"C:\Users\Max\AppData\Local\Programs\JAI\JAI.exe"), folder));
            assert!(inside(Path::new(r"c:\users\max\appdata\local\programs\jai\runtime\ollama\ollama.exe"), folder));
            assert!(!inside(Path::new(r"C:\Users\Max\AppData\Local\Programs\JAI-old\JAI.exe"), folder));
            assert!(!inside(Path::new(r"D:\JAI\JAI.exe"), folder));
            assert!(super::folder().ends_with(r"Programs\JAI"));
        } else {
            let folder = Path::new("/home/max/.local/share/jai");
            assert!(inside(Path::new("/home/max/.local/share/jai/jai (deleted)"), folder));
            assert!(inside(Path::new("/home/max/.local/share/jai/runtime/ollama/bin/ollama"), folder));
            assert!(!inside(Path::new("/home/max/.local/share/jai-old/jai"), folder));
            assert!(super::folder().ends_with("jai"));
        }
        // This test runs from neither, and its own processes are seen.
        assert!(running_from(&folder().join("no such folder")).is_empty());
        assert!(os::processes().iter().any(|(pid, _)| *pid == std::process::id()));
    }

    #[test]
    fn unpacked_the_program_can_start() {
        let dir = env::temp_dir().join(format!("jai-setup-test-{}", std::process::id()));
        let program = unpack_into(&[(PROGRAM.to_string(), b"x".to_vec()), ("DirectML.dll".to_string(), vec![2; 10])], &dir).unwrap();
        assert_eq!(program, dir.join(PROGRAM));
        assert_eq!(fs::read(dir.join("DirectML.dll")).unwrap(), vec![2; 10]);
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&program).unwrap().permissions()) & 0o111, 0o111);
        remove(&dir);
        assert!(!dir.exists());
    }
}
