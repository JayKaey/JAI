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

/// Background scans (application index) must never compete with audio or speech recognition.
pub fn lower_thread_priority() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
        // SAFETY: adjusts only the current thread's priority.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
    }
}

/// `--console`: the live log in the terminal Jarvis was started from, or else in a new console
/// window (Jarvis itself never opens one).
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

/// Jarvis is a windowless (GUI-subsystem) program. Diagnostics (`--selftest`, `--check-models`,
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
            MessageBoxW(HWND(0), PCWSTR(wide.as_ptr()), w!("J.A.R.V.I.S."), MB_ICONERROR | MB_OK | MB_SETFOREGROUND);
        }
    }
}

/// Held for the lifetime of the app; a second `JARVIS.exe` sees it and only brings up the HUD.
pub struct SingleInstance {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
}

impl SingleInstance {
    /// None when another Jarvis is already running.
    pub fn acquire() -> Option<Self> {
        #[cfg(windows)]
        {
            use windows::core::w;
            use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
            use windows::Win32::System::Threading::CreateMutexW;
            // SAFETY: named mutex creation; the handle is owned by the returned guard.
            unsafe {
                match CreateMutexW(None, false, w!("Local\\JARVIS-V3-single-instance")) {
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

/// Whether a desktop session is available for the HUD window.
pub fn has_display() -> bool {
    cfg!(windows)
        || cfg!(target_os = "macos")
        || std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
}
