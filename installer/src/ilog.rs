// ------------ Setup Log ------------
// Setup's own log (installer.log in the launcher's logs folder), written by install and uninstall alike.
// Each start trims it to the last few setup runs so it never grows without bound, and every line is echoed to
// stderr too.

use std::io::Write;
use std::sync::Mutex;

static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

const RUN_MARKER: &str = "---- peebify-installer start";
const KEEP_PREVIOUS_RUNS: usize = 9;
const MAX_KEPT_BYTES: usize = 256 * 1024;

fn run_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if line.contains(RUN_MARKER) {
            starts.push(at);
        }
        at += line.len();
    }
    starts
}

fn recent_runs(text: &str) -> &str {
    let starts = run_starts(text);
    let mut from = starts
        .len()
        .checked_sub(KEEP_PREVIOUS_RUNS)
        .map_or(0, |first| starts[first]);
    if text.len() - from > MAX_KEPT_BYTES {
        let limit = text.len() - MAX_KEPT_BYTES;
        from = starts
            .iter()
            .copied()
            .find(|start| *start >= limit)
            .unwrap_or(text.len());
    }
    &text[from..]
}

fn trim(path: &std::path::Path) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let text = String::from_utf8_lossy(&bytes);
    let kept = recent_runs(&text);
    if kept.len() < text.len() {
        let _ = std::fs::write(path, kept.as_bytes());
    }
}

pub fn init() {
    let Some(dir) = crate::consts::user_data_dir().map(|d| d.join("logs")) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("installer.log");
    let _ = std::fs::remove_file(dir.join("installer.log.1"));
    trim(&path);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(count: usize) -> String {
        (0..count)
            .map(|i| format!("[t] {RUN_MARKER}: run {i}\n[t] work {i}\n[t] ---- peebify-installer exit: 0\n"))
            .collect()
    }

    #[test]
    fn only_the_last_runs_are_kept() {
        let text = runs(30);
        let kept = recent_runs(&text);
        assert_eq!(run_starts(kept).len(), KEEP_PREVIOUS_RUNS);
        assert!(kept.starts_with(&format!("[t] {RUN_MARKER}: run 21\n")));
        assert!(kept.ends_with("run 29\n[t] work 29\n[t] ---- peebify-installer exit: 0\n"));
    }

    #[test]
    fn a_short_log_is_left_alone() {
        let text = format!("[t] stray line\n{}", runs(3));
        assert_eq!(recent_runs(&text), text);
    }

    #[test]
    fn huge_runs_are_cut_back_to_whole_runs_under_the_cap() {
        let filler = "x".repeat(200 * 1024);
        let text: String = (0..3)
            .map(|i| format!("{RUN_MARKER}: run {i}\n{filler}\n"))
            .collect();
        let kept = recent_runs(&text);
        assert!(kept.len() <= MAX_KEPT_BYTES);
        assert!(kept.starts_with(&format!("{RUN_MARKER}: run 2\n")));
    }
}
