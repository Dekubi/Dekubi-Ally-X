// Adapted from victory-fork (https://github.com/fxceless/victory-fork, commit 9376fd6 + 106 VIctory fixes), a fork of
// VIctory (https://github.com/Marvisak/VIctory). GPL-3.0-or-later, by Marvisak, fxceless
// and contributors. Renamed for Dekubi X.
//! One tracing setup for every binary.
//!
//! Filter with `DEKUBI_LOG` (falls back to `RUST_LOG`), e.g.
//! `DEKUBI_LOG=ally_loader=trace,info`. Debug and trace output exist
//! only in builds with the `diagnostics` feature; a plain release build
//! logs info and above whatever the filter asks for.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The most verbose level this build can log.
const CAP: LevelFilter = if cfg!(feature = "diagnostics") {
    LevelFilter::TRACE
} else {
    LevelFilter::INFO
};

fn env_filter(default_level: &str) -> EnvFilter {
    std::env::var("DEKUBI_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .and_then(|v| EnvFilter::try_new(v).ok())
        .unwrap_or_else(|| EnvFilter::new(default_level))
}

pub fn init(default_level: &str) {
    let filter = env_filter(default_level);
    let _ = tracing_subscriber::registry()
        .with(CAP)
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_writer(AsyncStderr::new()),
        )
        .try_init();
}

/// Picks the per-launch logs directory: prefers `<exe_dir>/logs` when
/// `exe_dir` is outside the git repository, otherwise falls back to
/// `<data_dir>/logs`.
pub fn launch_log_dir(exe_dir: Option<&Path>, data_dir: &Path) -> PathBuf {
    if let Some(dir) = exe_dir {
        let candidate = dir.join("logs");
        if crate::policy::ensure_outside_repo(&candidate).is_ok() {
            return candidate;
        }
    }
    data_dir.join("logs")
}

/// Builds a timestamped per-launch log file path inside `log_dir`
/// (`dekubi-x-YYYYMMDD-HHMMSS.log`, UTC).
pub fn launch_log_path(log_dir: &Path) -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    log_dir.join(format!("dekubi-x-{}.log", format_utc_timestamp(secs)))
}

/// Initializes tracing to write to both stderr and `log_file` (ANSI disabled
/// on the file layer, unbuffered OS writes via `Mutex<File>`).
///
/// Refuses paths inside the repository via [`crate::policy::ensure_outside_repo`]
/// and creates parent directories as needed.
pub fn init_with_file(default_level: &str, log_file: &Path) -> io::Result<()> {
    crate::policy::ensure_outside_repo(log_file)
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e))?;
    if let Some(parent) = log_file.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)?;

    let filter = env_filter(default_level);
    let _ = tracing_subscriber::registry()
        .with(CAP)
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_writer(AsyncStderr::new()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_ansi(false)
                .with_writer(AsyncFile::new(file)),
        )
        .try_init();
    Ok(())
}

/// Asynchronous stderr writer for INFO and below (`DEKUBI_LOG_SYNC=1`: synchronous).
/// Ported from `neptune-dolberth-nvidia-8gb` commit `658d561` (GPL-3.0-or-later):
/// keeps disk/paging stalls off the GPU submitting thread while WARN/ERROR still
/// flush synchronously so crash logs are never lost.
struct AsyncStderr(Option<std::sync::mpsc::Sender<Vec<u8>>>);

impl AsyncStderr {
    fn new() -> Self {
        if std::env::var_os("DEKUBI_LOG_SYNC").is_some_and(|v| v == "1") {
            return Self(None);
        }
        let (to, from) = std::sync::mpsc::channel::<Vec<u8>>();
        let spawned = std::thread::Builder::new()
            .name("dekubi-log-err".into())
            .spawn(move || {
                use std::io::Write;
                let mut batch = Vec::with_capacity(64 << 10);
                while let Ok(line) = from.recv() {
                    batch.extend_from_slice(&line);
                    while batch.len() < 256 << 10 {
                        match from.try_recv() {
                            Ok(more) => batch.extend_from_slice(&more),
                            Err(_) => break,
                        }
                    }
                    let _ = std::io::stderr().lock().write_all(&batch);
                    batch.clear();
                }
            });
        Self(spawned.ok().map(|_| to))
    }
}

enum StderrLine {
    Queued(Vec<u8>, std::sync::mpsc::Sender<Vec<u8>>),
    Now(std::io::Stderr),
}

impl io::Write for StderrLine {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            StderrLine::Queued(line, _) => {
                line.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            StderrLine::Now(err) => err.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            StderrLine::Queued(..) => Ok(()),
            StderrLine::Now(err) => err.flush(),
        }
    }
}

impl Drop for StderrLine {
    fn drop(&mut self) {
        if let StderrLine::Queued(line, to) = self
            && !line.is_empty()
            && let Err(lost) = to.send(std::mem::take(line))
        {
            use std::io::Write;
            let _ = std::io::stderr().write_all(&lost.0);
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for AsyncStderr {
    type Writer = StderrLine;

    fn make_writer(&'a self) -> StderrLine {
        match &self.0 {
            Some(to) => StderrLine::Queued(Vec::with_capacity(256), to.clone()),
            None => StderrLine::Now(std::io::stderr()),
        }
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> StderrLine {
        if *meta.level() <= tracing::Level::WARN {
            StderrLine::Now(std::io::stderr())
        } else {
            self.make_writer()
        }
    }
}

/// Asynchronous file writer for per-launch logs (`DEKUBI_LOG_SYNC=1`: synchronous).
/// Batches INFO/below writes up to 256 KiB on a dedicated thread so the GPU
/// submitting thread never blocks on disk I/O; WARN/ERROR write synchronously.
struct AsyncFile {
    file: std::sync::Arc<Mutex<std::fs::File>>,
    tx: Option<std::sync::mpsc::Sender<Vec<u8>>>,
}

impl AsyncFile {
    fn new(file: std::fs::File) -> Self {
        let file = std::sync::Arc::new(Mutex::new(file));
        if std::env::var_os("DEKUBI_LOG_SYNC").is_some_and(|v| v == "1") {
            return Self { file, tx: None };
        }
        let (to, from) = std::sync::mpsc::channel::<Vec<u8>>();
        let bg = std::sync::Arc::clone(&file);
        let spawned = std::thread::Builder::new()
            .name("dekubi-log-file".into())
            .spawn(move || {
                use std::io::Write;
                let mut batch = Vec::with_capacity(64 << 10);
                while let Ok(line) = from.recv() {
                    batch.extend_from_slice(&line);
                    while batch.len() < 256 << 10 {
                        match from.try_recv() {
                            Ok(more) => batch.extend_from_slice(&more),
                            Err(_) => break,
                        }
                    }
                    let mut guard = bg.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = guard.write_all(&batch);
                    batch.clear();
                }
            });
        Self {
            file,
            tx: spawned.ok().map(|_| to),
        }
    }
}

enum FileLine {
    Queued(
        Vec<u8>,
        std::sync::mpsc::Sender<Vec<u8>>,
        std::sync::Arc<Mutex<std::fs::File>>,
    ),
    Now(std::sync::Arc<Mutex<std::fs::File>>),
}

impl io::Write for FileLine {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            FileLine::Queued(line, _, _) => {
                line.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            FileLine::Now(file) => {
                let mut guard = file.lock().unwrap_or_else(|e| e.into_inner());
                guard.write(bytes)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            FileLine::Queued(..) => Ok(()),
            FileLine::Now(file) => {
                let mut guard = file.lock().unwrap_or_else(|e| e.into_inner());
                guard.flush()
            }
        }
    }
}

impl Drop for FileLine {
    fn drop(&mut self) {
        if let FileLine::Queued(line, to, file) = self
            && !line.is_empty()
            && let Err(lost) = to.send(std::mem::take(line))
        {
            use std::io::Write;
            let mut guard = file.lock().unwrap_or_else(|e| e.into_inner());
            let _ = guard.write_all(&lost.0);
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for AsyncFile {
    type Writer = FileLine;

    fn make_writer(&'a self) -> FileLine {
        match &self.tx {
            Some(to) => FileLine::Queued(
                Vec::with_capacity(256),
                to.clone(),
                std::sync::Arc::clone(&self.file),
            ),
            None => FileLine::Now(std::sync::Arc::clone(&self.file)),
        }
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> FileLine {
        if *meta.level() <= tracing::Level::WARN {
            FileLine::Now(std::sync::Arc::clone(&self.file))
        } else {
            self.make_writer()
        }
    }
}

/// Formats seconds since the Unix epoch as `YYYYMMDD-HHMMSS` (UTC).
fn format_utc_timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hour = rem / 3_600;
    let min = (rem % 3_600) / 60;
    let sec = rem % 60;

    // Howard Hinnant's civil-from-days (days since 1970-01-01).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}-{hour:02}{min:02}{sec:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_timestamp_formatting_matches_known_dates() {
        assert_eq!(format_utc_timestamp(0), "19700101-000000");
        // 2026-11-25 17:25:42 UTC
        assert_eq!(format_utc_timestamp(1_795_627_542), "20261125-172542");
    }

    #[test]
    fn launch_log_dir_and_path_create_outside_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let exe_dir = tmp.path().join("release_out");
        let data_dir = tmp.path().join("appdata");

        let log_dir = launch_log_dir(Some(&exe_dir), &data_dir);
        assert_eq!(log_dir, exe_dir.join("logs"));

        let path = launch_log_path(&log_dir);
        assert_eq!(path.parent(), Some(log_dir.as_path()));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("dekubi-x-"), "unexpected name: {name}");
        assert!(name.ends_with(".log"), "unexpected name: {name}");
        assert_eq!(name.len(), "dekubi-x-YYYYMMDD-HHMMSS.log".len());

        init_with_file("info", &path).unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn launch_log_dir_falls_back_when_exe_is_inside_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_repo = tmp.path().join("repo");
        let exe_dir = fake_repo.join("target").join("release");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::write(fake_repo.join(crate::policy::ROOT_MARKER), "").unwrap();
        let data_dir = tmp.path().join("appdata");

        assert_eq!(
            launch_log_dir(Some(&exe_dir), &data_dir),
            data_dir.join("logs")
        );
    }
}
