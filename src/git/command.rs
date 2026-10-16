//! Git CLI operations live on workers, never the rendering thread.
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub fn run(
    path: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output, String> {
    run_env(path, args, input, timeout, &[])
}

pub fn run_env(
    path: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    timeout: Duration,
    environment: &[(&str, &std::ffi::OsStr)],
) -> Result<Output, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in environment {
        cmd.env(name, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|_| {
        "Could not start Git. Install Git for Windows and add it to PATH.".to_string()
    })?;
    #[cfg(windows)]
    let _job = Job::attach(&child);
    if let Some(input) = input {
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(input);
    }
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    // Drain both pipes concurrently. Never wait on an inherited pipe after a timeout.
    let (tx, rx) = std::sync::mpsc::channel();
    for (is_out, mut pipe) in [
        (true, Box::new(stdout) as Box<dyn Read + Send>),
        (false, Box::new(stderr) as Box<dyn Read + Send>),
    ] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut buf = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let keep = n.min((16 * 1024 * 1024usize).saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buf[..keep]);
            }
            let _ = tx.send((is_out, bytes));
        });
    }
    drop(tx);
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Git operation timed out. Check connectivity and Git authentication outside Git Rustle.".into());
            }
        }
    };
    let mut output = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    for _ in 0..2 {
        let (is_out, data) = rx
            .recv_timeout(timeout.saturating_sub(start.elapsed()))
            .map_err(|_| "Git helper timed out".to_string())?;
        if is_out {
            output.stdout = data;
        } else {
            output.stderr = data;
        }
    }
    Ok(output)
}

pub fn checked(path: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = run(path, args, None, Duration::from_secs(300))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(clean(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// Don't let error messages or branch names inject terminal control sequences.
pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(1000)
        .collect()
}

#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Job {
    fn attach(child: &std::process::Child) -> Option<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        // SAFETY: valid owned handles and sized, initialized information structure.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return None;
            }
            let job = Self(handle);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            ) == 0
                || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
            {
                return None;
            }
            Some(job)
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: this handle is owned by this guard.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
