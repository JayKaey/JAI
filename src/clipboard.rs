//! The system clipboard: "what's in my clipboard?", "summarise what I copied", "copy that".
//! Windows uses the native clipboard API on the Win32 worker thread; Linux uses wl-clipboard
//! (Wayland) or xclip (X11).

const MAX_CHARS: usize = 20_000;

#[cfg(windows)]
mod native {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    /// Another program may hold the clipboard for a moment.
    fn open() -> Result<(), String> {
        for _ in 0..10 {
            // SAFETY: opening the clipboard without an owner window.
            if unsafe { OpenClipboard(HWND(0)) }.is_ok() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        Err("the clipboard is in use by another program".into())
    }

    pub fn read() -> Result<String, String> {
        open()?;
        // SAFETY: the clipboard is open; the handle is only used while locked and before closing.
        let result = unsafe {
            match GetClipboardData(CF_UNICODETEXT.0 as u32) {
                Ok(handle) if handle.0 != 0 => {
                    let mem = HGLOBAL(handle.0 as *mut core::ffi::c_void);
                    let ptr = GlobalLock(mem) as *const u16;
                    if ptr.is_null() {
                        Err("the clipboard text could not be read".to_string())
                    } else {
                        let mut len = 0usize;
                        while *ptr.add(len) != 0 && len < 4_000_000 {
                            len += 1;
                        }
                        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
                        let _ = GlobalUnlock(mem);
                        Ok(text)
                    }
                }
                _ => Ok(String::new()),
            }
        };
        // SAFETY: balanced with the successful OpenClipboard above.
        unsafe {
            let _ = CloseClipboard();
        }
        result
    }

    pub fn write(text: &str) -> Result<(), String> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        open()?;
        // SAFETY: the allocation is filled while locked and handed to the clipboard, which owns it.
        let result = unsafe {
            (|| -> Result<(), String> {
                EmptyClipboard().map_err(|e| e.message().to_string())?;
                let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).map_err(|e| e.message().to_string())?;
                let dst = GlobalLock(mem) as *mut u16;
                if dst.is_null() {
                    return Err("could not lock clipboard memory".into());
                }
                std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
                let _ = GlobalUnlock(mem);
                SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(mem.0 as isize)).map_err(|e| e.message().to_string())?;
                Ok(())
            })()
        };
        // SAFETY: balanced with the successful OpenClipboard above.
        unsafe {
            let _ = CloseClipboard();
        }
        result
    }
}

#[cfg(windows)]
pub async fn read() -> Result<String, String> {
    crate::win32::call(native::read).await?
}

#[cfg(windows)]
pub async fn write(text: &str) -> Result<(), String> {
    let text: String = text.chars().take(MAX_CHARS).collect();
    crate::win32::call(move || native::write(&text)).await?
}

#[cfg(not(windows))]
pub async fn read() -> Result<String, String> {
    tokio::task::spawn_blocking(|| {
        let attempts: [(&str, &[&str]); 2] = [("wl-paste", &["--no-newline"]), ("xclip", &["-selection", "clipboard", "-o"])];
        for (program, args) in attempts {
            if let Ok(out) = std::process::Command::new(program).args(args).output() {
                if out.status.success() {
                    return Ok(String::from_utf8_lossy(&out.stdout).to_string());
                }
            }
        }
        Err("No clipboard tool found (install wl-clipboard or xclip).".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(not(windows))]
pub async fn write(text: &str) -> Result<(), String> {
    use std::io::Write;
    let text: String = text.chars().take(MAX_CHARS).collect();
    tokio::task::spawn_blocking(move || {
        let attempts: [(&str, &[&str]); 2] = [("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])];
        for (program, args) in attempts {
            if let Ok(mut child) = std::process::Command::new(program).args(args).stdin(std::process::Stdio::piped()).spawn() {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                if child.wait().map(|s| s.success()).unwrap_or(false) {
                    return Ok(());
                }
            }
        }
        Err("No clipboard tool found (install wl-clipboard or xclip).".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The `clipboard` tool: read (for summarising, translating …) or write.
pub async fn tool(args: &serde_json::Value) -> Result<String, String> {
    let action = crate::tools::str_arg(args, &["action"]).unwrap_or("read").to_lowercase();
    match action.as_str() {
        "write" | "copy" | "set" => {
            let text = crate::tools::str_arg(args, &["text", "content"]).ok_or("clipboard(write) needs 'text'.")?;
            write(text).await?;
            Ok(format!("Copied {} characters to the clipboard.", text.chars().count()))
        }
        _ => {
            let text = read().await?;
            if text.trim().is_empty() {
                return Ok("The clipboard is empty (or holds no text).".into());
            }
            let count = text.chars().count();
            let shown: String = text.chars().take(6000).collect();
            Ok(format!("Clipboard ({count} characters):\n{shown}{}", if count > 6000 { "\n…" } else { "" }))
        }
    }
}

pub fn tool_definition() -> serde_json::Value {
    serde_json::json!({"type": "function", "function": {
        "name": "clipboard",
        "description": "Sir's clipboard: action read (returns the copied text, e.g. to summarise, translate or explain it) or write (text) to copy something for him.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["read", "write"]},
            "text": {"type": "string"}
        }, "required": ["action"]}
    }})
}

#[cfg(all(test, windows))]
mod tests {
    #[tokio::test]
    #[ignore = "uses the real clipboard"]
    async fn round_trip() {
        let before = super::read().await.unwrap_or_default();
        if before.is_empty() {
            return; // may hold an image: nothing we could restore losslessly
        }
        super::write("J.A.R.V.I.S. clipboard test \u{2713} äöü").await.unwrap();
        assert_eq!(super::read().await.unwrap(), "J.A.R.V.I.S. clipboard test \u{2713} äöü");
        super::write(&before).await.unwrap();
    }
}
