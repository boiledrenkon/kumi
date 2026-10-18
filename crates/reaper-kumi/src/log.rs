//! The bridge's own log (REAPER keeps none for extensions): one file in the log folder the reference names, kept
//! under 1 MB by moving the old one aside. Until a folder is known, lines go to stderr.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 1024 * 1024;

static FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_dir(dir: Option<PathBuf>) {
    let path = dir.and_then(|dir| fs::create_dir_all(&dir).ok().map(|_| dir.join(format!("reaper-{}.log", std::process::id()))));
    *FILE.lock().unwrap_or_else(|e| e.into_inner()) = path;
}

pub fn line(message: &str) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let text = format!("{secs:.3} {message}\n");
    let file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(path) = file.as_ref() else {
        eprint!("reaper_kumi: {text}");
        return;
    };
    if fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(text.as_bytes());
    }
}
