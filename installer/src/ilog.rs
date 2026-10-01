// ------------ Setup Log ------------
// Setup's own log (installer.log in the launcher's logs folder), written by install and uninstall alike.
// It rolls over to installer.log.1 past 1 MB, and every line is echoed to stderr too.

use std::io::Write;
use std::sync::Mutex;

static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

const MAX_LOG_BYTES: u64 = 1024 * 1024;

pub fn init() {
    let Some(dir) = crate::consts::user_data_dir().map(|d| d.join("logs")) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("installer.log");

    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_LOG_BYTES {
        let _ = std::fs::rename(&path, dir.join("installer.log.1"));
    }

    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        *LOG_FILE.lock().unwrap() = Some(file);
    }
}

pub fn log(msg: &str) {
    let line = format!(
        "[{}] {}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        msg
    );
    eprint!("{line}");
    if let Ok(mut guard) = LOG_FILE.lock() {
        if let Some(f) = guard.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

macro_rules! ilog {
    ($($arg:tt)*) => {
        $crate::ilog::log(&format!($($arg)*))
    };
}
pub(crate) use ilog;
