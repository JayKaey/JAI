//! Non-blocking shell execution (PowerShell on Windows, Bash elsewhere) with a hard timeout,
//! captured stdout/stderr, UTF-8 output and a guard against catastrophic commands.

use regex::Regex;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::{info, warn};

pub const SHELL_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_CHARS: usize = 4000;

/// Commands that can destroy data or the OS. The LLM gets a refusal instead of execution.
static BLOCKED: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"(?i)\bformat(\.com)?\s+[a-z]:", "formatting a drive"),
        (r"(?i)\b(diskpart|mkfs(\.\w+)?|fdisk|wipefs|bcdedit|vssadmin\s+delete|cipher\s+/w)\b", "disk/boot tooling"),
        (r"(?i)\brm\s+(-[a-z]*[rf][a-z]*\s+)+(/|~|\$home|/\*|\*)(\s|$)", "recursive deletion of a root/home directory"),
        (
            r#"(?i)\bremove-item\b.*-recurse\b.*([a-z]:\\(windows|users|program files)?\\?['"]?(\s|$)|\$env:(systemroot|windir|userprofile)\b)"#,
            "recursive deletion of a system/user root",
        ),
        (
            r#"(?i)\bremove-item\s+(-\w+\s+)*['"]?([a-z]:\\(windows|users|program files)?\\?['"]?(\s|$)|\$env:(systemroot|windir|userprofile)\b).*-recurse"#,
            "recursive deletion of a system/user root",
        ),
        (r"(?i)\b(rd|rmdir)\s+/s\s+.*[a-z]:\\(windows)?\\?\s*$", "recursive deletion of a system root"),
        (r"(?i)\bdd\s+.*of=/dev/(sd|nvme|mmcblk)", "raw disk overwrite"),
        (r":\(\)\s*\{\s*:\|:&\s*\};:", "fork bomb"),
        (r"(?i)\breg(\.exe)?\s+delete\s+hk(lm|ey_local_machine)", "deleting machine registry keys"),
    ]
    .into_iter()
    .map(|(p, why)| (Regex::new(p).expect("valid shell guard regex"), why))
    .collect()
});

pub fn blocked_reason(command: &str) -> Option<&'static str> {
    BLOCKED.iter().find(|(re, _)| re.is_match(command)).map(|(_, why)| *why)
}

/// Prevents a console window from flashing up for child processes on Windows.
pub fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

pub fn hide_console_std(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Joins stdout/stderr into one readable, length-capped string.
pub fn combine_output(stdout: &[u8], stderr: &[u8], max_chars: usize) -> String {
    let out = String::from_utf8_lossy(stdout).trim().to_string();
    let err = String::from_utf8_lossy(stderr).trim().to_string();
    let mut text = match (out.is_empty(), err.is_empty()) {
        (false, false) => format!("{out}\n[stderr] {err}"),
        (false, true) => out,
        (true, false) => format!("[stderr] {err}"),
        (true, true) => String::new(),
    };
    if text.chars().count() > max_chars {
        text = text.chars().take(max_chars).collect::<String>() + "\n…[output truncated]";
    }
    text
}

pub struct ShellResult {
    pub output: String,
    pub exit_code: Option<i32>,
}

pub async fn run(command: &str) -> Result<ShellResult, String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("The command is empty.".to_string());
    }
    if let Some(why) = blocked_reason(command) {
        warn!("[TOOL] Refused shell command ({why}): {command}");
        return Err(format!("Refused for safety ({why}). Ask Sir to run it manually if he really intends to."));
    }

    let (program, args): (&str, Vec<String>) = if cfg!(windows) {
        (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-Command".into(),
                format!("[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; $ProgressPreference='SilentlyContinue'; {command}"),
            ],
        )
    } else {
        ("bash", vec!["-c".into(), command.to_string()])
    };

    info!("[TOOL] Shell: {command}");
    let started = Instant::now();
    match run_helper(Path::new(program), &args, None, &[], SHELL_TIMEOUT).await {
        Ok(out) => {
            let mut output = combine_output(&out.stdout, &out.stderr, MAX_OUTPUT_CHARS);
            if output.is_empty() {
                output = format!("(no output, exit code {})", out.code.unwrap_or(-1));
            }
            info!("[TOOL] Shell finished in {} ms", started.elapsed().as_millis());
            Ok(ShellResult { output, exit_code: out.code })
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err(format!(
            "The command timed out after {} seconds and was terminated.",
            SHELL_TIMEOUT.as_secs()
        )),
        Err(e) => Err(format!("Could not start the shell: {e}")),
    }
}

/// What a background helper printed, and how it ended.
#[derive(Debug, Default, Clone)]
pub struct HelperOutput {
    /// The exit code (None when the process was killed or its code is unknown).
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl HelperOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Sets the flag when the awaiting future is dropped (a cancelled turn), so the blocking
/// helper loop kills its process at once instead of at the timeout.
#[cfg(windows)]
struct CancelOnDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(windows)]
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Runs a background helper (PowerShell, `arp`, `ping`, `ssh`, `nvidia-smi` …) and collects its
/// output. On Windows it starts hidden **and** with the "working in background" cursor forced
/// off (`STARTF_FORCEOFFFEEDBACK`, which std's spawner cannot set), inside a job object that dies
/// with JAI. Batch files and other platforms use the ordinary spawner. `Err(TimedOut)` once
/// `timeout` passes (the process is killed); dropping the future kills it as well.
pub async fn run_helper<I, S>(program: &Path, args: I, cwd: Option<&Path>, envs: &[(String, String)], timeout: Duration) -> std::io::Result<HelperOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args.into_iter().map(|a| a.as_ref().to_os_string()).collect();
    #[cfg(windows)]
    if let Some(exe) = quiet::resolve(program) {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _cancel = CancelOnDrop(std::sync::Arc::clone(&flag));
        let (cwd, envs) = (cwd.map(Path::to_path_buf), envs.to_vec());
        return tokio::task::spawn_blocking(move || quiet::output(&exe, &args, cwd.as_deref(), &envs, timeout, &flag))
            .await
            .map_err(std::io::Error::other)?;
    }
    let mut cmd = Command::new(program);
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    hide_console(&mut cmd);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(out)) => Ok(HelperOutput { code: out.status.code(), stdout: out.stdout, stderr: out.stderr }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("{} timed out", program.display()))),
    }
}

/// `run_helper` for synchronous callers (the TTS thread, telemetry sampling). Outside Windows the
/// ordinary spawner waits for the process without a deadline.
pub fn run_helper_blocking<I, S>(program: &Path, args: I, timeout: Duration) -> std::io::Result<HelperOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args.into_iter().map(|a| a.as_ref().to_os_string()).collect();
    #[cfg(windows)]
    if let Some(exe) = quiet::resolve(program) {
        return quiet::output(&exe, &args, None, &[], timeout, &std::sync::atomic::AtomicBool::new(false));
    }
    #[cfg(not(windows))]
    let _ = timeout;
    let mut cmd = std::process::Command::new(program);
    cmd.args(&args).stdin(std::process::Stdio::null());
    hide_console_std(&mut cmd);
    let out = cmd.output()?;
    Ok(HelperOutput { code: out.status.code(), stdout: out.stdout, stderr: out.stderr })
}

/// A long-running hidden helper (the worldwide tunnel) on Windows: no feedback cursor, killed
/// with JAI by the helper job and on drop. `capture` pipes its stdout and stderr; `envs` are
/// added to its environment (a secret travels there, not on the command line, which every
/// program on the PC can read).
#[cfg(windows)]
pub fn spawn_helper(program: &Path, args: &[OsString], envs: &[(String, String)], capture: bool) -> std::io::Result<quiet::Child> {
    let exe = quiet::resolve(program).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("{} is not an executable", program.display())))?;
    quiet::spawn(&exe, args, None, envs, capture)
}

/// Process creation without std: `CreateProcessW` with `STARTF_FORCEOFFFEEDBACK` (no busy
/// cursor), `CREATE_NO_WINDOW`, an explicit inherit list (only the child's own stdio handles,
/// inheritable just for the call) and a kill-on-close job; pipes are read without blocking.
#[cfg(windows)]
pub mod quiet {
    use super::HelperOutput;
    use std::ffi::{c_void, OsStr, OsString};
    use std::io::{self, Read};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT, TRUE, WAIT_OBJECT_0};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    };
    use windows::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
    use windows::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList, ResumeThread, TerminateProcess,
        UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
        LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_FORCEOFFFEEDBACK, STARTF_USESHOWWINDOW,
        STARTF_USESTDHANDLES, STARTUPINFOEXW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    fn raw(h: &impl AsRawHandle) -> HANDLE {
        HANDLE(h.as_raw_handle() as isize)
    }

    fn os_error(e: windows::core::Error) -> io::Error {
        io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
    }

    /// Every helper joins this job: when JAI exits or crashes, its last handle closes and the
    /// helpers die with it (no orphaned `cloudflared`). Processes the helpers start themselves
    /// break away silently, so an app launched from a shell command keeps running.
    struct HelperJob(HANDLE);

    // SAFETY: a job object handle may be used from any thread.
    unsafe impl Send for HelperJob {}
    unsafe impl Sync for HelperJob {}

    fn job() -> Option<&'static HelperJob> {
        static JOB: OnceLock<Option<HelperJob>> = OnceLock::new();
        JOB.get_or_init(|| {
            // SAFETY: plain object creation; `info` outlives the call.
            unsafe {
                let handle = CreateJobObjectW(None, PCWSTR::null()).ok()?;
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
                let set = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if set.is_err() {
                    let _ = CloseHandle(handle);
                    return None;
                }
                Some(HelperJob(handle))
            }
        })
        .as_ref()
    }

    /// The executable to start: an existing path, or a bare name found on PATH (System32 and
    /// Windows PowerShell as a fallback). Batch files return None: they need the command
    /// interpreter's argument escaping, which std's spawner does safely.
    pub fn resolve(program: &Path) -> Option<PathBuf> {
        let found = if program.is_absolute() || program.components().count() > 1 {
            program.is_file().then(|| std::path::absolute(program).unwrap_or_else(|_| program.to_path_buf()))
        } else {
            let name = program.to_string_lossy();
            crate::apps::which(&name).or_else(|| {
                let root = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()));
                let stem = name.trim_end_matches(".exe");
                [root.join("System32").join(format!("{stem}.exe")), root.join("System32").join("WindowsPowerShell").join("v1.0").join(format!("{stem}.exe"))]
                    .into_iter()
                    .find(|p| p.is_file())
            })
        }?;
        let ext = found.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        matches!(ext.as_str(), "exe" | "com").then_some(found)
    }

    /// One argument, quoted the way the MSVC runtime (and std) parse command lines.
    fn append_arg(line: &mut Vec<u16>, arg: &OsStr) {
        let wide: Vec<u16> = arg.encode_wide().collect();
        let quote = wide.is_empty() || wide.iter().any(|&c| c == b' ' as u16 || c == b'\t' as u16 || c == b'\n' as u16 || c == 0x0B);
        if quote {
            line.push(b'"' as u16);
        }
        let mut backslashes = 0usize;
        for &c in &wide {
            if c == b'\\' as u16 {
                backslashes += 1;
            } else {
                if c == b'"' as u16 {
                    line.extend(std::iter::repeat_n(b'\\' as u16, backslashes + 1));
                }
                backslashes = 0;
            }
            line.push(c);
        }
        if quote {
            line.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            line.push(b'"' as u16);
        }
    }

    pub(super) fn command_line(program: &Path, args: &[OsString]) -> Vec<u16> {
        let mut line = vec![b'"' as u16];
        line.extend(program.as_os_str().encode_wide());
        line.push(b'"' as u16);
        for arg in args {
            line.push(b' ' as u16);
            append_arg(&mut line, arg);
        }
        line.push(0);
        line
    }

    /// The current environment plus `extra` (replacing variables of the same name), sorted as
    /// Windows expects, double-NUL terminated.
    fn environment(extra: &[(String, String)]) -> Vec<u16> {
        let mut vars: Vec<(OsString, OsString)> = std::env::vars_os().filter(|(k, _)| !extra.iter().any(|(e, _)| OsStr::new(e).eq_ignore_ascii_case(k))).collect();
        vars.extend(extra.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))));
        vars.sort_by_key(|(k, _)| k.to_ascii_uppercase());
        let mut block = Vec::new();
        for (k, v) in vars {
            block.extend(k.encode_wide());
            block.push(b'=' as u16);
            block.extend(v.encode_wide());
            block.push(0);
        }
        block.push(0);
        block
    }

    /// An anonymous pipe as (read end, write end); neither end is inheritable yet.
    fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
        let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
        // SAFETY: out-pointers to locals; the handles are owned right after.
        unsafe {
            CreatePipe(&mut read, &mut write, None, 64 * 1024).map_err(os_error)?;
            Ok((OwnedHandle::from_raw_handle(read.0 as RawHandle), OwnedHandle::from_raw_handle(write.0 as RawHandle)))
        }
    }

    struct AttributeList(LPPROC_THREAD_ATTRIBUTE_LIST);

    impl Drop for AttributeList {
        fn drop(&mut self) {
            // SAFETY: initialised by InitializeProcThreadAttributeList; its storage outlives this.
            unsafe { DeleteProcThreadAttributeList(self.0) };
        }
    }

    /// A running helper.
    pub struct Child {
        process: OwnedHandle,
        stdout: Option<OwnedHandle>,
        stderr: Option<OwnedHandle>,
        pid: u32,
    }

    /// Serialises the moment a quiet spawn's handles are inheritable.
    static SPAWNING: Mutex<()> = Mutex::new(());

    pub fn spawn(program: &Path, args: &[OsString], cwd: Option<&Path>, envs: &[(String, String)], capture: bool) -> io::Result<Child> {
        let application: Vec<u16> = program.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut command = command_line(program, args);
        let env_block = (!envs.is_empty()).then(|| environment(envs));
        let directory: Option<Vec<u16>> = cwd.map(|d| d.as_os_str().encode_wide().chain(Some(0)).collect());
        let null: OwnedHandle = std::fs::OpenOptions::new().read(true).write(true).open("NUL")?.into();
        let (out_read, out_write) = if capture { pipe().map(|(r, w)| (Some(r), Some(w)))? } else { (None, None) };
        let (err_read, err_write) = if capture { pipe().map(|(r, w)| (Some(r), Some(w)))? } else { (None, None) };
        let child_in = raw(&null);
        let child_out = out_write.as_ref().map_or(child_in, raw);
        let child_err = err_write.as_ref().map_or(child_in, raw);
        let mut inherit: Vec<HANDLE> = vec![child_in];
        for h in [child_out, child_err] {
            if !inherit.contains(&h) {
                inherit.push(h);
            }
        }

        let mut size = 0usize;
        // SAFETY: the first call only reports the buffer size the list needs.
        unsafe {
            let _ = InitializeProcThreadAttributeList(LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()), 1, 0, &mut size);
        }
        let mut storage = vec![0u64; size.div_ceil(8).max(1)];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        // SAFETY: `storage` holds `size` bytes, pointer-aligned, and outlives `_list`.
        unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) }.map_err(os_error)?;
        let _list = AttributeList(list);
        // SAFETY: `inherit` outlives CreateProcessW below, as the attribute requires.
        unsafe {
            UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize, Some(inherit.as_ptr().cast()), std::mem::size_of_val(inherit.as_slice()), None, None)
        }
        .map_err(os_error)?;

        let mut info = STARTUPINFOEXW::default();
        info.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        info.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW | STARTF_FORCEOFFFEEDBACK;
        info.StartupInfo.wShowWindow = SW_HIDE.0 as u16;
        info.StartupInfo.hStdInput = child_in;
        info.StartupInfo.hStdOutput = child_out;
        info.StartupInfo.hStdError = child_err;
        info.lpAttributeList = list;
        let mut pi = PROCESS_INFORMATION::default();
        let flags = CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED;
        {
            let _serial = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: valid handles owned above; every pointer outlives the call.
            unsafe {
                for h in &inherit {
                    SetHandleInformation(*h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT).map_err(os_error)?;
                }
                let created = CreateProcessW(
                    PCWSTR(application.as_ptr()),
                    PWSTR(command.as_mut_ptr()),
                    None,
                    None,
                    TRUE,
                    flags,
                    env_block.as_ref().map(|b| b.as_ptr() as *const c_void),
                    directory.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
                    &info.StartupInfo,
                    &mut pi,
                );
                for h in &inherit {
                    let _ = SetHandleInformation(*h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
                }
                created.map_err(os_error)?;
            }
        }
        // SAFETY: CreateProcessW returned owned process and thread handles.
        let (process, thread) = unsafe { (OwnedHandle::from_raw_handle(pi.hProcess.0 as RawHandle), OwnedHandle::from_raw_handle(pi.hThread.0 as RawHandle)) };
        // SAFETY: valid job, process and (suspended) thread handles.
        unsafe {
            if let Some(job) = job() {
                let _ = AssignProcessToJobObject(job.0, raw(&process));
            }
            ResumeThread(raw(&thread));
        }
        // The child's ends close here, so the pipes report EOF once it exits.
        drop((null, out_write, err_write, thread));
        Ok(Child { process, stdout: out_read, stderr: err_read, pid: pi.dwProcessId })
    }

    /// Reads what a pipe holds right now without blocking; None once the writer has gone.
    fn drain(pipe: &mut Option<OwnedHandle>, into: &mut Vec<u8>) -> usize {
        let Some(handle) = pipe.as_ref().map(|h| h.as_raw_handle()) else { return 0 };
        let mut total = 0;
        loop {
            let mut available = 0u32;
            // SAFETY: a valid pipe handle; only the byte count is queried.
            if unsafe { PeekNamedPipe(HANDLE(handle as isize), None, 0, None, Some(&mut available), None) }.is_err() {
                *pipe = None;
                return total;
            }
            if available == 0 {
                return total;
            }
            let mut buf = vec![0u8; available.min(1 << 16) as usize];
            // SAFETY: borrows the handle owned by `pipe` without closing it (ManuallyDrop).
            let mut file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(handle) });
            match file.read(&mut buf) {
                Ok(0) | Err(_) => {
                    *pipe = None;
                    return total;
                }
                Ok(n) => {
                    into.extend_from_slice(&buf[..n]);
                    total += n;
                }
            }
        }
    }

    impl Child {
        pub fn id(&self) -> u32 {
            self.pid
        }

        /// The exit code once the process has ended.
        pub fn try_wait(&self) -> io::Result<Option<i32>> {
            // SAFETY: a valid process handle owned by `self`.
            unsafe {
                if WaitForSingleObject(raw(&self.process), 0) != WAIT_OBJECT_0 {
                    return Ok(None);
                }
                let mut code = 0u32;
                GetExitCodeProcess(raw(&self.process), &mut code).map_err(os_error)?;
                Ok(Some(code as i32))
            }
        }

        pub fn kill(&self) {
            // SAFETY: a valid process handle; terminating an ended process is harmless.
            unsafe {
                let _ = TerminateProcess(raw(&self.process), 1);
            }
        }

        /// Appends whatever stdout and stderr hold now; returns the number of bytes read.
        pub fn read_available(&mut self, stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) -> usize {
            drain(&mut self.stdout, stdout) + drain(&mut self.stderr, stderr)
        }
    }

    impl Drop for Child {
        fn drop(&mut self) {
            if self.try_wait().ok().flatten().is_none() {
                self.kill();
            }
        }
    }

    /// Runs the helper to its end (or `timeout` / `cancel`, which kill it) and collects stdout and
    /// stderr while it runs, so a chatty process never stalls on a full pipe.
    pub fn output(program: &Path, args: &[OsString], cwd: Option<&Path>, envs: &[(String, String)], timeout: Duration, cancel: &AtomicBool) -> io::Result<HelperOutput> {
        let mut child = spawn(program, args, cwd, envs, true)?;
        let deadline = Instant::now() + timeout;
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let mut idle = Duration::from_millis(1);
        loop {
            let read = child.read_available(&mut stdout, &mut stderr);
            if let Some(code) = child.try_wait()? {
                child.read_available(&mut stdout, &mut stderr);
                return Ok(HelperOutput { code: Some(code), stdout, stderr });
            }
            if cancel.load(Ordering::Acquire) {
                child.kill();
                return Err(io::Error::new(io::ErrorKind::Interrupted, format!("{} was cancelled", program.display())));
            }
            if Instant::now() >= deadline {
                child.kill();
                return Err(io::Error::new(io::ErrorKind::TimedOut, format!("{} timed out", program.display())));
            }
            idle = if read > 0 { Duration::from_millis(1) } else { (idle * 2).min(Duration::from_millis(15)) };
            std::thread::sleep(idle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_blocks_destructive_commands_only() {
        for bad in [
            "format C: /q",
            "rm -rf /",
            "rm -rf ~",
            "sudo mkfs.ext4 /dev/sda1",
            "Remove-Item -Recurse -Force C:\\Windows",
            "Remove-Item C:\\ -Recurse -Force",
            "diskpart",
            "reg delete HKLM\\Software\\X /f",
        ] {
            assert!(blocked_reason(bad).is_some(), "should block: {bad}");
        }
        for ok in [
            "Get-Process | Select-Object -First 5",
            "rm -rf ./target/tmp",
            "Remove-Item -Recurse .\\build",
            "ls -la ~",
            "shutdown /s /t 600",
        ] {
            assert!(blocked_reason(ok).is_none(), "should allow: {ok}");
        }
    }

    #[test]
    fn output_is_combined_and_capped() {
        let text = combine_output(b"hello", b"warn", 100);
        assert_eq!(text, "hello\n[stderr] warn");
        let long = combine_output(&[b'x'; 50], b"", 10);
        assert!(long.starts_with("xxxxxxxxxx") && long.contains("truncated"));
    }

    #[tokio::test]
    async fn runs_a_command_and_times_out_cleanly() {
        let res = run(if cfg!(windows) { "Write-Output 'ok'" } else { "echo ok" }).await.unwrap();
        assert_eq!(res.output, "ok");
        assert_eq!(res.exit_code, Some(0));
    }

    #[cfg(windows)]
    #[test]
    fn command_lines_quote_like_the_msvc_runtime() {
        let line = |args: &[&str]| {
            let args: Vec<OsString> = args.iter().map(OsString::from).collect();
            String::from_utf16_lossy(&quiet::command_line(Path::new("C:\\x\\p.exe"), &args)).trim_end_matches('\0').to_string()
        };
        assert_eq!(line(&["-a", "two words", ""]), r#""C:\x\p.exe" -a "two words" """#);
        assert_eq!(line(&[r#"say "hi""#]), r#""C:\x\p.exe" "say \"hi\"""#);
        assert_eq!(line(&[r"C:\dir\ x\"]), r#""C:\x\p.exe" "C:\dir\ x\\""#);
        assert!(quiet::resolve(Path::new("powershell")).is_some_and(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn helpers_capture_output_exit_codes_and_time_out() {
        let ps = Path::new("powershell");
        let out = run_helper(ps, ["-NoProfile", "-NonInteractive", "-Command", "Write-Output 'quiet ok'; [Console]::Error.WriteLine('err'); exit 3"], None, &[], Duration::from_secs(30)).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "quiet ok");
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "err");
        assert_eq!(out.code, Some(3));
        let env = run_helper(ps, ["-NoProfile", "-Command", "Write-Output $env:JAI_TEST_VAR"], None, &[("JAI_TEST_VAR".into(), "visible".into())], Duration::from_secs(30)).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&env.stdout).trim(), "visible");
        let started = Instant::now();
        let slow = run_helper(ps, ["-NoProfile", "-Command", "Start-Sleep -Seconds 20"], None, &[], Duration::from_millis(1500)).await;
        assert_eq!(slow.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(8), "killed at the deadline");
        let blocking = run_helper_blocking(ps, ["-NoProfile", "-Command", "Write-Output 42"], Duration::from_secs(30)).unwrap();
        assert!(blocking.success() && String::from_utf8_lossy(&blocking.stdout).trim() == "42");
    }
}
