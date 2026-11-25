//! App log: mpv's messages and the player's own, kept in memory for Settings › Logs and written to
//! <config>/zym-player/logs/latest.log. Bounded both ways: the last KEEP lines in memory, and
//! latest.log rotates to previous.log at FILE_MAX bytes (and at each start), so at most ~2×FILE_MAX
//! on disk. Also echoed to stderr.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Instant;

/// Most severe first, so `level <= threshold` filters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    /// mpv's log levels ("fatal", "error", "warn", "info", "status", "v", "debug", "trace").
    pub fn from_mpv(l: &str) -> Level {
        match l {
            "fatal" | "error" => Level::Error,
            "warn" => Level::Warn,
            "info" | "status" => Level::Info,
            _ => Level::Debug,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Line {
    /// Seconds since the player started.
    pub at: f64,
    pub level: Level,
    pub source: String,
    pub text: String,
}

impl Line {
    pub fn format(&self) -> String {
        format!("[{:>8.3}] {:<5} {}: {}", self.at, self.level.tag(), self.source, self.text)
    }
}

const KEEP: usize = 2000;
const FILE_MAX: u64 = 5 * 1024 * 1024;

static LINES: Mutex<VecDeque<Line>> = Mutex::new(VecDeque::new());
/// latest.log and the bytes written to it.
static FILE: Mutex<Option<(std::fs::File, u64)>> = Mutex::new(None);

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

pub fn dir() -> PathBuf {
    crate::config_dir().join("logs")
}

pub fn file_path() -> PathBuf {
    dir().join("latest.log")
}

/// latest.log -> previous.log, then a fresh latest.log.
fn rotate() -> Option<(std::fs::File, u64)> {
    std::fs::create_dir_all(dir()).ok()?;
    std::fs::rename(file_path(), dir().join("previous.log")).ok();
    std::fs::File::create(file_path()).ok().map(|f| (f, 0))
}

/// Start this run's log file (the previous run's becomes previous.log). Logging works without it.
pub fn init() {
    start();
    *FILE.lock().unwrap_or_else(PoisonError::into_inner) = rotate();
    info("app", &format!("{} {} started", crate::APP_NAME, crate::VERSION));
}

pub fn add(level: Level, source: &str, text: &str) {
    let line = Line { at: start().elapsed().as_secs_f64(), level, source: source.to_string(), text: text.trim_end().to_string() };
    let formatted = line.format();
    eprintln!("{formatted}");
    let mut file = FILE.lock().unwrap_or_else(PoisonError::into_inner);
    if file.as_ref().is_some_and(|(_, n)| *n >= FILE_MAX) {
        *file = rotate();
    }
    if let Some((f, n)) = file.as_mut()
        && writeln!(f, "{formatted}").is_ok() {
            *n += formatted.len() as u64 + 1;
        }
    drop(file);
    let mut lines = LINES.lock().unwrap_or_else(PoisonError::into_inner);
    if lines.len() == KEEP {
        lines.pop_front();
    }
    lines.push_back(line);
}

pub fn error(source: &str, text: &str) {
    add(Level::Error, source, text);
}
pub fn warn(source: &str, text: &str) {
    add(Level::Warn, source, text);
}
pub fn info(source: &str, text: &str) {
    add(Level::Info, source, text);
}
pub fn debug(source: &str, text: &str) {
    add(Level::Debug, source, text);
}

/// Newest first, at or above `threshold` (Error < Warn < …), at most `max`.
pub fn recent(threshold: Level, max: usize) -> Vec<Line> {
    LINES.lock().unwrap_or_else(PoisonError::into_inner).iter().rev().filter(|l| l.level <= threshold).take(max).cloned().collect()
}

/// Oldest first, as text (Copy).
pub fn text(threshold: Level) -> String {
    LINES.lock().unwrap_or_else(PoisonError::into_inner).iter().filter(|l| l.level <= threshold).map(Line::format).collect::<Vec<_>>().join("\n")
}

pub fn clear() {
    LINES.lock().unwrap_or_else(PoisonError::into_inner).clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_filter_and_cap() {
        assert_eq!(Level::from_mpv("fatal"), Level::Error);
        assert_eq!(Level::from_mpv("v"), Level::Debug);
        assert!(Level::Error < Level::Warn, "more severe sorts first");
        // Other tests log in parallel: only look at this test's own source.
        let mine = |v: Vec<Line>| v.into_iter().filter(|l| l.source == "logtest").map(|l| l.text).collect::<Vec<_>>();
        add(Level::Info, "logtest", "hello\n");
        add(Level::Error, "logtest", "boom");
        assert_eq!(mine(recent(Level::Error, KEEP)), ["boom"], "error filter");
        assert_eq!(mine(recent(Level::Debug, KEEP)), ["boom", "hello"], "newest first, newline trimmed");
        let all = text(Level::Debug);
        assert!(all.find("logtest: hello").unwrap() < all.find("logtest: boom").unwrap(), "copy text is oldest first");
        for i in 0..KEEP + 5 {
            add(Level::Debug, "t", &i.to_string());
        }
        assert_eq!(LINES.lock().unwrap_or_else(PoisonError::into_inner).len(), KEEP, "bounded");
    }
}
