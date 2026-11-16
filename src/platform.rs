//! Small OS-specific helpers.

/// Raises the calling thread's scheduling priority (audio/VAD: highest, STT: above normal).
pub fn raise_thread_priority(audio: bool) {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_HIGHEST,
        };
        let priority = if audio { THREAD_PRIORITY_HIGHEST } else { THREAD_PRIORITY_ABOVE_NORMAL };
        // SAFETY: adjusts only the current thread's priority.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), priority) };
    }
    #[cfg(not(windows))]
    {
        // Raising priority needs CAP_SYS_NICE on Linux; the default scheduler is fine for
        // the VAD (≈1 % of a core) so this is intentionally a no-op.
        let _ = audio;
    }
}

/// While it lives the whole process runs above normal priority. whisper.cpp starts its own
/// worker threads at normal priority whatever the thread that called it has, so with a compile
/// or a game on every core a half-second decode took one to three seconds (jai.log,
/// 2026-11-09, rustc at 99 %). Above normal, all of them are served first for the half second
/// a decode lasts. A process started below normal on purpose (a test run beside Sir's game)
/// stays where it was put.
pub struct Urgent(Option<u32>);

/// The process's priority class (0 where there is none to read).
pub fn priority_class() -> u32 {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{GetCurrentProcess, GetPriorityClass};
        // SAFETY: reads this process's own priority class.
        unsafe { GetPriorityClass(GetCurrentProcess()) }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

pub fn urgent() -> Urgent {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS};
        let was = priority_class();
        // SAFETY: sets this process's own priority class.
        if was == NORMAL_PRIORITY_CLASS.0 && unsafe { SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS) }.is_ok() {
            return Urgent(Some(was));
        }
    }
    Urgent(None)
}

impl Drop for Urgent {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(was) = self.0 {
            use windows::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass, PROCESS_CREATION_FLAGS};
            // SAFETY: puts this process's own priority class back.
            let _ = unsafe { SetPriorityClass(GetCurrentProcess(), PROCESS_CREATION_FLAGS(was)) };
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::{ABOVE_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS};

    /// A decode runs above normal and leaves the process as it found it; a run started below
    /// normal (beside Sir's game) is never raised.
    #[test]
    fn a_decode_is_urgent_only_for_as_long_as_it_lasts() {
        let before = priority_class();
        {
            let _urgent = urgent();
            let during = priority_class();
            if before == NORMAL_PRIORITY_CLASS.0 {
                assert_eq!(during, ABOVE_NORMAL_PRIORITY_CLASS.0);
            } else {
                assert_eq!(during, before, "a lowered process stays lowered");
            }
        }
        assert_eq!(priority_class(), before);
    }
}

/// Background scans (application index) must never compete with audio or speech recognition.
pub fn lower_thread_priority() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
        // SAFETY: adjusts only the current thread's priority.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
    }
}

/// Game mode: JAI and the language engine (`ollama*`; a runner it starts later inherits the
/// class) drop below normal so the game always wins the CPU; `false` puts back what was lowered.
pub fn game_priority(low: bool) {
    #[cfg(windows)]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_SET_INFORMATION};
        static LOWERED: AtomicBool = AtomicBool::new(false);
        let class = if low { BELOW_NORMAL_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS };
        // (A process started below normal on purpose, a test run beside his game, stays so.)
        if low != LOWERED.load(Ordering::Acquire) && (!low || priority_class() == NORMAL_PRIORITY_CLASS.0) {
            // SAFETY: sets this process's own priority class.
            if unsafe { SetPriorityClass(GetCurrentProcess(), class) }.is_ok() {
                LOWERED.store(low, Ordering::Release);
            }
        }
        if hands_off() {
            return;
        }
        let mut sys = sysinfo::System::new();
        sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, sysinfo::ProcessRefreshKind::new());
        for (pid, p) in sys.processes() {
            if p.name().to_string_lossy().to_lowercase().starts_with("ollama") {
                // SAFETY: a handle with only the right to set the priority, closed at once.
                unsafe {
                    if let Ok(h) = OpenProcess(PROCESS_SET_INFORMATION, false, pid.as_u32()) {
                        let _ = SetPriorityClass(h, class);
                        let _ = CloseHandle(h);
                    }
                }
            }
        }
    }
    #[cfg(not(windows))]
    let _ = low;
}

/// `--console`: the live log in the terminal JAI was started from, or else in a new console
/// window (JAI itself never opens one).
pub fn open_console() {
    attach_parent_console();
    #[cfg(windows)]
    {
        use windows::Win32::System::Console::{AllocConsole, GetConsoleWindow};
        // SAFETY: console allocation for this process only.
        unsafe {
            if GetConsoleWindow().0 == 0 && !stdout_redirected() {
                let _ = AllocConsole();
            }
        }
    }
}

/// JAI is a windowless (GUI-subsystem) program. Diagnostics (`--selftest`, `--check-models`,
/// …) started from a terminal print into that terminal instead.
pub fn attach_parent_console() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Console::{AttachConsole, GetConsoleWindow, ATTACH_PARENT_PROCESS};
        // SAFETY: console queries/attachment for this process only.
        unsafe {
            if GetConsoleWindow().0 == 0 && !stdout_redirected() {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
    }
}

/// stdout goes to a pipe or file (e.g. a launcher capturing the log).
#[cfg(windows)]
fn stdout_redirected() -> bool {
    use windows::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    // SAFETY: reads this process' own standard handle.
    unsafe { GetStdHandle(STD_OUTPUT_HANDLE).is_ok_and(|h| h.0 != 0 && h.0 != -1) }
}

/// True when log output is visible somewhere (a console or a redirected stdout).
pub fn has_console() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::System::Console::GetConsoleWindow;
        // SAFETY: plain query.
        unsafe { GetConsoleWindow().0 != 0 || stdout_redirected() }
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Reports a fatal start-up problem: logged, and shown in a dialog when there is no console.
pub fn alert(text: &str) {
    tracing::error!("{text}");
    #[cfg(windows)]
    if !has_console() {
        use windows::core::{w, PCWSTR};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND};
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: the text buffer outlives the (blocking) call.
        unsafe {
            MessageBoxW(HWND(0), PCWSTR(wide.as_ptr()), w!("J.A.I."), MB_ICONERROR | MB_OK | MB_SETFOREGROUND);
        }
    }
}

/// Held for the lifetime of the app; a second `JAI.exe` sees it and only brings up the HUD.
pub struct SingleInstance {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
}

impl SingleInstance {
    /// None when another JAI is already running.
    pub fn acquire() -> Option<Self> {
        #[cfg(windows)]
        {
            use windows::core::w;
            use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
            use windows::Win32::System::Threading::CreateMutexW;
            // SAFETY: named mutex creation; the handle is owned by the returned guard.
            unsafe {
                match CreateMutexW(None, false, w!("Local\\JAI-V3-single-instance")) {
                    Ok(handle) if GetLastError() == ERROR_ALREADY_EXISTS => {
                        let _ = CloseHandle(handle);
                        None
                    }
                    Ok(handle) => Some(Self { handle }),
                    // Cannot tell: rather start than refuse to.
                    Err(_) => Some(Self { handle: HANDLE(0) }),
                }
            }
        }
        #[cfg(not(windows))]
        {
            // The HTTP port is the guard on other platforms (a second bind fails).
            Some(Self {})
        }
    }
}

#[cfg(windows)]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        if self.handle.0 != 0 {
            // SAFETY: the handle is owned by this guard.
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }
}

/// A scripted session (a self-test, a silent conversation) holds this as well. It runs where
/// Sir cannot see it, so the JAI he starts himself asks it to give way (`tray`) rather than
/// asking it for a HUD that is on no screen.
#[cfg(windows)]
const TEST_SESSION: windows::core::PCWSTR = windows::core::w!("Local\\JAI-V3-test-session");

static IN_TEST_SESSION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This process is a scripted session, until it ends.
pub fn mark_test_session() {
    IN_TEST_SESSION.store(true, std::sync::atomic::Ordering::Release);
    // SAFETY: named mutex creation; the handle stays open until the process ends, on purpose.
    #[cfg(windows)]
    unsafe {
        let _ = windows::Win32::System::Threading::CreateMutexW(None, false, TEST_SESSION);
    }
}

pub fn in_test_session() -> bool {
    IN_TEST_SESSION.load(std::sync::atomic::Ordering::Acquire)
}

/// Whether a scripted session was asked to show `what` (`JAI_DEMO=boot-long,idle,night,music`),
/// so that it can be looked at there. Never outside a scripted session.
pub fn demo(what: &str) -> bool {
    in_test_session() && std::env::var("JAI_DEMO").is_ok_and(|v| v.split(',').any(|d| d.trim() == what))
}

/// A scripted session that never moves the focus, its own windows' or anyone else's: Sir may
/// be playing on another monitor. Every test session but the window drill's, which drives the
/// HUD's own keys.
pub fn quiet_session() -> bool {
    in_test_session() && !crate::window_drill::enabled()
}

/// Tests keep their hands off Sir's players: neither `cargo test` nor any scripted session (the
/// window drill too) pauses, resumes or skips his music and videos, presses a media key, starts
/// a player or opens a music link. (2026-11-08: the exhaustive verifier's "Pause." went through
/// the dispatcher's instant stop and paused his players three times while he used the PC.)
pub fn hands_off() -> bool {
    cfg!(test) || in_test_session()
}

/// Whether a scripted session is running on this desktop.
pub fn test_session_running() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
        // SAFETY: named mutex creation; the handle is closed again right here.
        unsafe {
            match windows::Win32::System::Threading::CreateMutexW(None, false, TEST_SESSION) {
                Ok(handle) => {
                    let running = GetLastError() == ERROR_ALREADY_EXISTS;
                    let _ = CloseHandle(handle);
                    running
                }
                Err(_) => false,
            }
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Whether a desktop session is available for the HUD window.
pub fn has_display() -> bool {
    cfg!(windows)
        || cfg!(target_os = "macos")
        || std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
}
