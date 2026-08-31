use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};

static TEST_LOG_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
static LOG_DISPATCHER: OnceLock<DiagnosticLogDispatcher> = OnceLock::new();

const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
const LOG_BACKUP_COUNT: usize = 2;
const LOG_QUEUE_CAPACITY: usize = 4096;
const LOG_FLUSH_BATCH_SIZE: usize = 64;
const LOG_FLUSH_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Serialize)]
struct DiagnosticRecord {
    timestamp_ms: u64,
    pid: u32,
    event: String,
    detail: Value,
}

#[derive(Default)]
struct DiagnosticWriter {
    path: Option<PathBuf>,
    file: Option<BufWriter<File>>,
    bytes_written: u64,
}

impl DiagnosticWriter {
    #[cfg(test)]
    fn append_with_max_bytes(
        &mut self,
        path: &Path,
        line: &str,
        max_log_bytes: u64,
    ) -> std::io::Result<()> {
        self.append_buffered_with_max_bytes(path, line, max_log_bytes)?;
        self.flush()
    }

    fn append_buffered(&mut self, path: &Path, line: &str) -> std::io::Result<()> {
        self.append_buffered_with_max_bytes(path, line, MAX_LOG_BYTES)
    }

    fn append_buffered_with_max_bytes(
        &mut self,
        path: &Path,
        line: &str,
        max_log_bytes: u64,
    ) -> std::io::Result<()> {
        if self.path.as_deref() != Some(path) || self.file.is_none() {
            self.open(path)?;
        }

        let original_line_bytes = u64::try_from(line.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let oversized_line;
        let line = if original_line_bytes > max_log_bytes {
            oversized_line = oversized_record_line(original_line_bytes, max_log_bytes);
            oversized_line.as_str()
        } else {
            line
        };
        let line_bytes = u64::try_from(line.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        if self.bytes_written > 0 && self.bytes_written.saturating_add(line_bytes) > max_log_bytes {
            self.rotate(path, max_log_bytes)?;
        }

        let file = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("diagnostic log writer is unavailable"))?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        self.bytes_written = self.bytes_written.saturating_add(line_bytes);
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = self.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }

    fn open(&mut self, path: &Path) -> std::io::Result<()> {
        self.flush()?;
        self.file.take();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        self.bytes_written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        self.path = Some(path.to_path_buf());
        self.file = Some(BufWriter::new(file));
        Ok(())
    }

    fn rotate(&mut self, path: &Path, max_log_bytes: u64) -> std::io::Result<()> {
        self.flush()?;
        self.file.take();
        for generation in (1..=LOG_BACKUP_COUNT).rev() {
            let destination = rotated_log_path(path, generation);
            if generation == LOG_BACKUP_COUNT {
                remove_if_exists(&destination)?;
            } else {
                let source = rotated_log_path(path, generation);
                let destination = rotated_log_path(path, generation + 1);
                if source.exists() {
                    remove_if_exists(&destination)?;
                    move_bounded_log(&source, &destination, max_log_bytes)?;
                }
            }
        }
        if path.exists() {
            move_bounded_log(path, &rotated_log_path(path, 1), max_log_bytes)?;
        }
        self.path = None;
        self.bytes_written = 0;
        self.open(path)
    }
}

enum LogCommand {
    Append {
        path: PathBuf,
        line: String,
        dropped_before: u64,
    },
    Flush(mpsc::Sender<std::io::Result<()>>),
}

struct DiagnosticLogDispatcher {
    sender: SyncSender<LogCommand>,
    dropped: AtomicU64,
}

impl DiagnosticLogDispatcher {
    fn start() -> Self {
        let (sender, receiver) = mpsc::sync_channel(LOG_QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("codey-diagnostic-log".to_string())
            .spawn(move || diagnostic_log_worker(receiver))
            .expect("diagnostic log worker should start");
        Self {
            sender,
            dropped: AtomicU64::new(0),
        }
    }

    fn enqueue(&self, path: PathBuf, line: String) -> std::io::Result<()> {
        let dropped_before = self.dropped.swap(0, Ordering::AcqRel);
        match self.sender.try_send(LogCommand::Append {
            path,
            line,
            dropped_before,
        }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(LogCommand::Append { dropped_before, .. })) => {
                self.dropped
                    .fetch_add(dropped_before.saturating_add(1), Ordering::Relaxed);
                Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "diagnostic log queue is full",
                ))
            }
            Err(TrySendError::Disconnected(_)) => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "diagnostic log worker stopped",
            )),
            Err(TrySendError::Full(LogCommand::Flush(_))) => unreachable!(),
        }
    }

    fn flush(&self) -> std::io::Result<()> {
        let (sender, receiver) = mpsc::channel();
        self.sender.send(LogCommand::Flush(sender)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "diagnostic log worker stopped",
            )
        })?;
        receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("timed out flushing diagnostic log: {error}"),
                )
            })?
    }
}

fn diagnostic_log_dispatcher() -> &'static DiagnosticLogDispatcher {
    LOG_DISPATCHER.get_or_init(DiagnosticLogDispatcher::start)
}

fn diagnostic_log_worker(receiver: Receiver<LogCommand>) {
    let mut writer = DiagnosticWriter::default();
    let mut pending_records = 0usize;
    loop {
        match receiver.recv_timeout(LOG_FLUSH_INTERVAL) {
            Ok(LogCommand::Append {
                path,
                line,
                dropped_before,
            }) => {
                if dropped_before > 0 {
                    let dropped_line = dropped_record_line(dropped_before);
                    if writer.append_buffered(&path, &dropped_line).is_ok() {
                        pending_records = pending_records.saturating_add(1);
                    }
                }
                if let Err(error) = writer.append_buffered(&path, &line) {
                    eprintln!("写入 Codey 诊断日志失败：{error}");
                } else {
                    pending_records = pending_records.saturating_add(1);
                }
                if pending_records >= LOG_FLUSH_BATCH_SIZE {
                    if let Err(error) = writer.flush() {
                        eprintln!("刷新 Codey 诊断日志失败：{error}");
                    }
                    pending_records = 0;
                }
            }
            Ok(LogCommand::Flush(reply)) => {
                let result = writer.flush();
                pending_records = 0;
                let _ = reply.send(result);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if pending_records > 0 {
                    if let Err(error) = writer.flush() {
                        eprintln!("刷新 Codey 诊断日志失败：{error}");
                    }
                    pending_records = 0;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = writer.flush();
                break;
            }
        }
    }
}

pub fn append_diagnostic_log(event: &str, detail: impl Serialize) -> std::io::Result<()> {
    let path = diagnostic_log_path();
    let detail = serde_json::to_value(detail).unwrap_or_else(|error| {
        json!({
            "serialization_error": error.to_string()
        })
    });
    let record = DiagnosticRecord {
        timestamp_ms: now_ms(),
        pid: std::process::id(),
        event: event.to_string(),
        detail,
    };
    let line = serde_json::to_string(&record).unwrap_or_else(|error| {
        json!({
            "timestamp_ms": now_ms(),
            "pid": std::process::id(),
            "event": "diagnostic_log.serialization_failed",
            "detail": {
                "message": error.to_string()
            }
        })
        .to_string()
    });
    diagnostic_log_dispatcher().enqueue(path, line)
}

pub fn flush_diagnostic_log() -> std::io::Result<()> {
    diagnostic_log_dispatcher().flush()
}

pub fn diagnostic_log_path() -> PathBuf {
    if let Some(lock) = TEST_LOG_PATH.get()
        && let Ok(guard) = lock.lock()
        && let Some(path) = &*guard
    {
        return path.clone();
    }
    crate::paths::default_diagnostic_log_path()
}

#[doc(hidden)]
pub fn set_diagnostic_log_path_for_tests(path: Option<PathBuf>) {
    if let Some(dispatcher) = LOG_DISPATCHER.get() {
        let _ = dispatcher.flush();
    }
    let lock = TEST_LOG_PATH.get_or_init(|| Mutex::new(None));
    *lock.lock().expect("test log path lock poisoned") = path;
}

fn rotated_log_path(path: &Path, generation: usize) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(format!(".{generation}"));
    PathBuf::from(value)
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn move_bounded_log(source: &Path, destination: &Path, max_bytes: u64) -> std::io::Result<()> {
    let source_bytes = std::fs::metadata(source)?.len();
    if source_bytes <= max_bytes {
        return std::fs::rename(source, destination);
    }

    let mut reader = BufReader::new(File::open(source)?);
    let start = source_bytes.saturating_sub(max_bytes);
    if start > 0 {
        reader.seek(SeekFrom::Start(start - 1))?;
        let mut previous = [0u8; 1];
        reader.read_exact(&mut previous)?;
        if previous[0] != b'\n' {
            reader.seek(SeekFrom::Start(start))?;
            let mut byte = [0u8; 1];
            while reader.read(&mut byte)? != 0 && byte[0] != b'\n' {}
        }
    }

    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    std::io::copy(&mut reader.take(max_bytes), &mut output)?;
    output.flush()?;
    std::fs::remove_file(source)
}

fn oversized_record_line(original_bytes: u64, max_bytes: u64) -> String {
    let detail = json!({
        "timestamp_ms": now_ms(),
        "pid": std::process::id(),
        "event": "diagnostic_log.record_too_large",
        "detail": {
            "original_bytes": original_bytes,
        },
    })
    .to_string();
    if u64::try_from(detail.len())
        .unwrap_or(u64::MAX)
        .saturating_add(1)
        <= max_bytes
    {
        detail
    } else if max_bytes >= 3 {
        "{}".to_string()
    } else {
        String::new()
    }
}

fn dropped_record_line(count: u64) -> String {
    json!({
        "timestamp_ms": now_ms(),
        "pid": std::process::id(),
        "event": "diagnostic_log.records_dropped",
        "detail": {
            "count": count,
            "reason": "queue_full",
        },
    })
    .to_string()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_writer_rotates_to_two_bounded_backups() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("codey.log");
        let mut writer = DiagnosticWriter::default();
        let max_bytes = 32;

        writer
            .append_with_max_bytes(&path, &"a".repeat(31), max_bytes)
            .unwrap();
        writer
            .append_with_max_bytes(&path, "first", max_bytes)
            .unwrap();
        writer
            .append_with_max_bytes(&path, &"b".repeat(25), max_bytes)
            .unwrap();
        writer
            .append_with_max_bytes(&path, "second", max_bytes)
            .unwrap();

        let first_backup = rotated_log_path(&path, 1);
        let second_backup = rotated_log_path(&path, 2);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
        assert_eq!(
            std::fs::read_to_string(&first_backup).unwrap(),
            format!("first\n{}\n", "b".repeat(25))
        );
        assert_eq!(
            std::fs::read_to_string(&second_backup).unwrap(),
            format!("{}\n", "a".repeat(31))
        );
        for log in [&path, &first_backup, &second_backup] {
            assert!(std::fs::metadata(log).unwrap().len() <= max_bytes);
        }

        writer
            .append_with_max_bytes(&path, &"c".repeat(24), max_bytes)
            .unwrap();
        writer
            .append_with_max_bytes(&path, "third", max_bytes)
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "third\n");
        assert_eq!(
            std::fs::read_to_string(&first_backup).unwrap(),
            format!("second\n{}\n", "c".repeat(24))
        );
        assert_eq!(
            std::fs::read_to_string(&second_backup).unwrap(),
            format!("first\n{}\n", "b".repeat(25))
        );
    }

    #[test]
    fn oversized_records_and_legacy_logs_are_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("codey.log");
        let mut writer = DiagnosticWriter::default();

        writer
            .append_with_max_bytes(&path, &"x".repeat(1024), 256)
            .unwrap();
        let record = std::fs::read_to_string(&path).unwrap();
        assert!(record.contains("diagnostic_log.record_too_large"));
        assert!(std::fs::metadata(&path).unwrap().len() <= 256);

        writer = DiagnosticWriter::default();
        std::fs::write(&path, "legacy-record\n".repeat(20)).unwrap();
        writer.append_with_max_bytes(&path, "new", 32).unwrap();
        let backup = rotated_log_path(&path, 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert!(std::fs::metadata(&backup).unwrap().len() <= 32);
        assert!(
            std::fs::read_to_string(&backup)
                .unwrap()
                .lines()
                .all(|line| line == "legacy-record")
        );
    }

    #[test]
    fn queued_diagnostic_records_are_visible_after_an_explicit_flush() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queued.log");
        set_diagnostic_log_path_for_tests(Some(path.clone()));

        for index in 0..100 {
            append_diagnostic_log("test.queued", json!({ "index": index })).unwrap();
        }
        flush_diagnostic_log().unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        // The test sink is process-global, so unrelated parallel tests may add
        // their own records while this path is active.
        let queued_record_count = contents
            .lines()
            .filter(|line| line.contains("\"event\":\"test.queued\""))
            .count();
        assert_eq!(queued_record_count, 100);
        assert!(contents.contains("\"event\":\"test.queued\""));
        set_diagnostic_log_path_for_tests(None);
    }
}
