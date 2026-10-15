//! Win32 console setup and teardown.
//!
//! Everything here talks to the console host (Windows Terminal / conhost) directly
//! through the Win32 console API. Output is plain VT escape sequences written with
//! a single `WriteFile` per frame.

use std::ptr::null_mut;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, COORD, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_ECHO_INPUT,
    ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_INPUT,
    ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT, GetConsoleMode, GetConsoleOutputCP,
    GetConsoleScreenBufferInfo, GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SMALL_RECT,
    SetConsoleMode, SetConsoleOutputCP, WriteConsoleA,
};

const CP_UTF8: u32 = 65001;

/// VT sequence emitted when entering the TUI.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H";
/// VT sequence emitted when leaving the TUI.
const LEAVE: &[u8] = b"\x1b[?2026l\x1b[0m\x1b[?25h\x1b[?1049l";

#[derive(Clone, Copy)]
struct Saved {
    stdin: usize,
    stdout: usize,
    in_mode: u32,
    out_mode: u32,
    out_cp: u32,
}

static SAVED: OnceLock<Saved> = OnceLock::new();

/// RAII guard over the console state. Dropping it restores the original
/// modes, code page and screen buffer.
pub struct Console {
    pub stdin: HANDLE,
    pub stdout: HANDLE,
}

#[derive(Debug)]
pub struct ConsoleError(pub &'static str);

impl std::fmt::Display for ConsoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ConsoleError {}

impl Console {
    pub fn new() -> Result<Self, ConsoleError> {
        // SAFETY: plain Win32 calls with valid out-pointers.
        unsafe {
            let stdin = GetStdHandle(STD_INPUT_HANDLE);
            let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
            if stdin == INVALID_HANDLE_VALUE || stdout == INVALID_HANDLE_VALUE {
                return Err(ConsoleError("no console handles"));
            }
            let mut in_mode = 0u32;
            let mut out_mode = 0u32;
            if GetConsoleMode(stdin, &mut in_mode) == 0 {
                return Err(ConsoleError("stdin is not a console"));
            }
            if GetConsoleMode(stdout, &mut out_mode) == 0 {
                return Err(ConsoleError("stdout is not a console"));
            }
            let out_cp = GetConsoleOutputCP();
            let _ = SAVED.set(Saved {
                stdin: stdin as usize,
                stdout: stdout as usize,
                in_mode,
                out_mode,
                out_cp,
            });

            let new_out = out_mode
                | ENABLE_PROCESSED_OUTPUT
                | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                | DISABLE_NEWLINE_AUTO_RETURN;
            if SetConsoleMode(stdout, new_out) == 0 {
                // DISABLE_NEWLINE_AUTO_RETURN is not supported on older hosts.
                SetConsoleMode(
                    stdout,
                    out_mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                );
            }

            // Raw-ish input: no line buffering, no echo, no Ctrl+C processing, no quick edit
            // (quick edit steals the mouse), but native mouse and resize records.
            let new_in = (in_mode
                & !(ENABLE_QUICK_EDIT_MODE
                    | ENABLE_LINE_INPUT
                    | ENABLE_ECHO_INPUT
                    | ENABLE_PROCESSED_INPUT
                    | ENABLE_VIRTUAL_TERMINAL_INPUT))
                | ENABLE_MOUSE_INPUT
                | ENABLE_WINDOW_INPUT
                | ENABLE_EXTENDED_FLAGS;
            SetConsoleMode(stdin, new_in);
            SetConsoleOutputCP(CP_UTF8);

            let console = Console { stdin, stdout };
            console.write_all(ENTER);
            install_panic_hook();
            Ok(console)
        }
    }

    /// Current visible window size in cells.
    pub fn size(&self) -> (u16, u16) {
        size_of(self.stdout)
    }

    /// Write the full buffer to the console.
    pub fn write_all(&self, bytes: &[u8]) {
        write_handle(self.stdout, bytes);
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        restore();
    }
}

fn size_of(stdout: HANDLE) -> (u16, u16) {
    let mut info = CONSOLE_SCREEN_BUFFER_INFO {
        dwSize: COORD { X: 0, Y: 0 },
        dwCursorPosition: COORD { X: 0, Y: 0 },
        wAttributes: 0,
        srWindow: SMALL_RECT {
            Left: 0,
            Top: 0,
            Right: 0,
            Bottom: 0,
        },
        dwMaximumWindowSize: COORD { X: 0, Y: 0 },
    };
    // SAFETY: valid handle and out-pointer.
    let ok = unsafe { GetConsoleScreenBufferInfo(stdout, &mut info) };
    if ok == 0 {
        return (80, 24);
    }
    let w = (info.srWindow.Right - info.srWindow.Left + 1).max(1) as u16;
    let h = (info.srWindow.Bottom - info.srWindow.Top + 1).max(1) as u16;
    (w, h)
}

fn write_handle(stdout: HANDLE, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let mut written = 0u32;
        let chunk = bytes.len().min(u32::MAX as usize) as u32;
        // SAFETY: pointer/len describe a live slice; `written` is a valid out-pointer.
        // The output code page is UTF-8, so the byte buffer is written verbatim.
        let ok = unsafe { WriteConsoleA(stdout, bytes.as_ptr(), chunk, &mut written, null_mut()) };
        if ok == 0 || written == 0 {
            return;
        }
        bytes = &bytes[written as usize..];
    }
}

/// Restore the console to the state captured by [`Console::new`]. Idempotent.
fn restore() {
    let Some(saved) = SAVED.get() else {
        return;
    };
    let stdout = saved.stdout as HANDLE;
    let stdin = saved.stdin as HANDLE;
    write_handle(stdout, LEAVE);
    // SAFETY: handles captured earlier from GetStdHandle.
    unsafe {
        SetConsoleMode(stdout, saved.out_mode);
        SetConsoleMode(stdin, saved.in_mode);
        SetConsoleOutputCP(saved.out_cp);
    }
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        default(info);
    }));
}
