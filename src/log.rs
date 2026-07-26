//! Tiny file logger for debugging without corrupting the TUI (stdout/stderr are
//! owned by the alternate screen). Logs go to `klawply.log` in the data dir
//! (the folder you launch from, same place as `fields.json`).
//!
//! Level is controlled by `KLAWPLY_LOG` = `off|error|warn|info|debug`
//! (default `info`). Never log secrets — pass token *lengths* or `redact()`ed
//! values, not the tokens themselves.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config;

const OFF: u8 = 0;
const ERROR: u8 = 1;
const WARN: u8 = 2;
const INFO: u8 = 3;
const DEBUG: u8 = 4;

/// Rotate (truncate) the log if it grows past this, so it never balloons.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

fn level() -> u8 {
    static LEVEL: OnceLock<u8> = OnceLock::new();
    *LEVEL.get_or_init(|| {
        match std::env::var("KLAWPLY_LOG")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" | "none" => OFF,
            "error" => ERROR,
            "warn" | "warning" => WARN,
            "debug" | "trace" => DEBUG,
            _ => INFO,
        }
    })
}

fn log_path() -> PathBuf {
    config::data_dir().join("klawply.log")
}

/// Write a session banner and rotate the file if it's grown too large.
pub fn init() {
    let path = log_path();
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > MAX_BYTES {
            let _ = fs::write(&path, b"");
        }
    }
    info(format!(
        "──── klawply session start (pid {}, log level {}) ────",
        std::process::id(),
        std::env::var("KLAWPLY_LOG").unwrap_or_else(|_| "info".into()),
    ));
}

pub fn error(msg: impl AsRef<str>) {
    write(ERROR, "ERROR", msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    write(WARN, "WARN ", msg.as_ref());
}

pub fn info(msg: impl AsRef<str>) {
    write(INFO, "INFO ", msg.as_ref());
}

pub fn debug(msg: impl AsRef<str>) {
    write(DEBUG, "DEBUG", msg.as_ref());
}

/// Redact a secret for logging: keep length and a 4-char prefix, mask the rest.
/// Use for anything token-shaped you *must* reference in a log line. Kept
/// available as the safe way to reference secrets even when nothing does yet.
#[allow(dead_code)]
pub fn redact(secret: &str) -> String {
    let n = secret.chars().count();
    if n == 0 {
        return "<empty>".into();
    }
    let head: String = secret.chars().take(4).collect();
    format!("{head}…<{n} chars>")
}

fn write(msg_level: u8, tag: &str, message: &str) {
    if msg_level > level() {
        return;
    }
    let _ = try_write(tag, message);
}

fn try_write(tag: &str, message: &str) -> std::io::Result<()> {
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{} [{tag}] {message}", timestamp())?;
    file.flush()
}

/// UTC time-of-day `HH:MM:SS.mmm` — enough to correlate events within a run
/// without pulling in a date crate.
fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let ms = now.subsec_millis();
    let tod = secs % 86_400;
    format!("{:02}:{:02}:{:02}.{:03}", tod / 3600, (tod % 3600) / 60, tod % 60, ms)
}
