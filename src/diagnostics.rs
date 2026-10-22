//! Opt-in refresh diagnostics: fixed labels/numeric metrics only, never repository data.
//! Producers never perform file I/O or wait for the bounded writer queue.
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LIMIT: u64 = 64 * 1024 * 1024;
const QUEUE: usize = 4096;
static LOGGER: OnceLock<Logger> = OnceLock::new();
static SPANS: AtomicU64 = AtomicU64::new(1);

struct State {
    start: Instant,
    dropped: AtomicU64,
    stopped: AtomicBool,
}
struct Logger {
    send: mpsc::SyncSender<Message>,
    state: Arc<State>,
}
enum Message {
    Record(Record),
    Stop,
}
#[derive(Serialize)]
struct Record {
    v: u8,
    seq: u64,
    session_ms: f64,
    thread: String,
    event: &'static str,
    name: &'static str,
    span: Option<u64>,
    duration_ms: Option<f64>,
    outcome: Option<&'static str>,
    metrics: BTreeMap<&'static str, u64>,
}

impl State {
    fn record(&self, event: &'static str, name: &'static str) -> Record {
        Record {
            v: 1,
            seq: 0,
            session_ms: self.start.elapsed().as_secs_f64() * 1000.,
            thread: format!("{:?}", std::thread::current().id()),
            event,
            name,
            span: None,
            duration_ms: None,
            outcome: None,
            metrics: BTreeMap::new(),
        }
    }
}
impl Logger {
    fn record(&self, record: Record) {
        if self.state.stopped.load(Ordering::Relaxed) {
            return;
        }
        if self.send.try_send(Message::Record(record)).is_err() {
            self.state.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Keep alive through desktop shutdown so queued records are flushed on normal exit.
pub struct Guard {
    send: mpsc::SyncSender<Message>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.send.send(Message::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn write_record(file: &mut File, mut record: Record, seq: &mut u64) -> io::Result<u64> {
    *seq += 1;
    record.seq = *seq;
    let mut bytes = serde_json::to_vec(&record)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    Ok(bytes.len() as u64)
}

fn start(path: &Path, limit: u64) -> io::Result<(Logger, Guard)> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    // Do not overwrite a prior session or follow an existing log file.
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let state = Arc::new(State {
        start: Instant::now(),
        dropped: AtomicU64::new(0),
        stopped: AtomicBool::new(false),
    });
    let (send, receive) = mpsc::sync_channel(QUEUE);
    let writer_state = state.clone();
    let worker = std::thread::Builder::new()
        .name("refresh-log".into())
        .spawn(move || {
            let mut seq = 0;
            let mut bytes = 0;
            let mut flushed = Instant::now();
            let mut beginning = writer_state.record("event", "session_start");
            beginning.metrics.insert("pid", std::process::id() as u64);
            beginning.metrics.insert(
                "diagnostic_default",
                cfg!(feature = "refresh-diagnostics") as u64,
            );
            let result = (|| -> io::Result<()> {
                bytes += write_record(&mut file, beginning, &mut seq)?;
                file.flush()?;
                loop {
                    match receive.recv_timeout(Duration::from_millis(250)) {
                        Ok(Message::Record(record)) => {
                            // Reserve space for a terminal marker. Records are bounded by
                            // compile-time labels and small metric sets at the call sites.
                            if bytes.saturating_add(4096) >= limit {
                                let mut end = writer_state.record("event", "log_limit_reached");
                                end.metrics.insert(
                                    "dropped_events",
                                    writer_state.dropped.load(Ordering::Relaxed),
                                );
                                write_record(&mut file, end, &mut seq)?;
                                break;
                            }
                            bytes += write_record(&mut file, record, &mut seq)?;
                        }
                        Ok(Message::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                            let mut end = writer_state.record("event", "session_end");
                            end.metrics.insert(
                                "dropped_events",
                                writer_state.dropped.load(Ordering::Relaxed),
                            );
                            write_record(&mut file, end, &mut seq)?;
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if flushed.elapsed() >= Duration::from_millis(250) {
                        file.flush()?;
                        flushed = Instant::now();
                    }
                }
                file.flush()
            })();
            writer_state.stopped.store(true, Ordering::Relaxed);
            if result.is_err() {
                eprintln!("git-rustle: diagnostic log writing stopped (I/O failure)");
            }
        })?;
    Ok((
        Logger {
            send: send.clone(),
            state,
        },
        Guard {
            send,
            worker: Some(worker),
        },
    ))
}

pub fn init() -> io::Result<Option<Guard>> {
    let configured = std::env::var_os("GIT_RUSTLE_REFRESH_LOG");
    if configured.as_deref() == Some(std::ffi::OsStr::new("off")) {
        return Ok(None);
    }
    let path = match configured {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ if cfg!(feature = "refresh-diagnostics") => {
            let base = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            base.join("git-rustle")
                .join("diagnostics")
                .join(format!("refresh-{timestamp}-{}.jsonl", std::process::id()))
        }
        _ => return Ok(None),
    };
    let (logger, guard) = start(&path, LIMIT)?;
    if LOGGER.set(logger).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "diagnostics already initialized",
        ));
    }
    Ok(Some(guard))
}

pub fn enabled() -> bool {
    LOGGER
        .get()
        .is_some_and(|logger| !logger.state.stopped.load(Ordering::Relaxed))
}

pub fn event(name: &'static str, fields: &[(&'static str, u64)]) {
    if let Some(logger) = LOGGER.get().filter(|_| enabled()) {
        let mut record = logger.state.record("event", name);
        record.metrics.extend(fields.iter().copied());
        logger.record(record);
    }
}

pub struct Span {
    start: Instant,
    record: Option<Record>,
}
impl Span {
    pub fn new(name: &'static str) -> Self {
        let record = LOGGER.get().filter(|_| enabled()).map(|logger| {
            let mut begin = logger.state.record("begin", name);
            let id = SPANS.fetch_add(1, Ordering::Relaxed);
            begin.span = Some(id);
            logger.record(begin);
            let mut end = logger.state.record("end", name);
            end.span = Some(id);
            end.outcome = Some("completed");
            end
        });
        Self {
            start: Instant::now(),
            record,
        }
    }
    pub fn field(&mut self, key: &'static str, value: u64) {
        if let Some(record) = self.record.as_mut() {
            record.metrics.insert(key, value);
        }
    }
    pub fn outcome(&mut self, outcome: &'static str) {
        if let Some(record) = self.record.as_mut() {
            record.outcome = Some(outcome);
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let (Some(mut record), Some(logger)) = (self.record.take(), LOGGER.get()) {
            record.duration_ms = Some(self.start.elapsed().as_secs_f64() * 1000.);
            record.session_ms = logger.state.start.elapsed().as_secs_f64() * 1000.;
            if std::thread::panicking() {
                record.outcome = Some("unwinding");
            }
            logger.record(record);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_flushes_on_shutdown_and_refuses_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("refresh.jsonl");
        let (logger, guard) = start(&path, LIMIT).unwrap();
        let mut record = logger.state.record("event", "test_numeric");
        record.metrics.insert("count", 42);
        logger.record(record);
        drop(guard);
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["name"], "session_start");
        assert_eq!(lines[1]["metrics"]["count"], 42);
        assert_eq!(lines[2]["name"], "session_end");
        assert_eq!(lines[2]["metrics"]["dropped_events"], 0);
        assert!(start(&path, LIMIT).is_err());
    }

    #[test]
    fn file_limit_stops_logging_with_explicit_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limited.jsonl");
        let (logger, guard) = start(&path, 8192).unwrap();
        for _ in 0..100 {
            logger.record(logger.state.record("event", "bounded_event"));
        }
        drop(guard);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("log_limit_reached"));
        assert!(std::fs::metadata(path).unwrap().len() <= 8192);
    }

    #[test]
    fn full_queue_drops_instead_of_blocking() {
        let (send, _receive) = mpsc::sync_channel(1);
        let logger = Logger {
            send,
            state: Arc::new(State {
                start: Instant::now(),
                dropped: AtomicU64::new(0),
                stopped: AtomicBool::new(false),
            }),
        };
        logger.record(logger.state.record("event", "first"));
        logger.record(logger.state.record("event", "second"));
        assert_eq!(logger.state.dropped.load(Ordering::Relaxed), 1);
    }
}
