// ------------ Overlay Helper Log ------------
// Writes overlay-helper.log in the launcher's logs folder and rolls it over past 1 MB.

const MAX_LOG_BYTES: u64 = 1024 * 1024;

pub fn rotate_log() {
    if let Some(path) = log_path() {
        rotate_if_larger(&path, MAX_LOG_BYTES);
    }
}

fn rotate_if_larger(path: &std::path::Path, limit: u64) -> bool {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size <= limit {
        return false;
    }
    let rotated = path.with_extension("old.log");
    let _ = std::fs::remove_file(&rotated);
    std::fs::rename(path, rotated).is_ok()
}

pub fn log_line(message: &str) {
    eprintln!("[overlay-helper] {message}");
    let Some(path) = log_path() else { return };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    use std::io::Write;
    let _ = writeln!(file, "[{}] {message}", stamp());
}

fn log_path() -> Option<std::path::PathBuf> {
    let roaming = std::env::var_os("APPDATA")?;
    let dir = std::path::PathBuf::from(roaming)
        .join("Peebify Launcher")
        .join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("overlay-helper.log"))
}

fn stamp() -> String {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetSystemTime;
    let mut now: SYSTEMTIME = unsafe { core::mem::zeroed() };
    unsafe { GetSystemTime(&mut now) };
    format_stamp(&now)
}

fn format_stamp(now: &windows_sys::Win32::Foundation::SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        now.wYear,
        now.wMonth,
        now.wDay,
        now.wHour,
        now.wMinute,
        now.wSecond,
        now.wMilliseconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_matches_the_launch_log_format() {
        let now = windows_sys::Win32::Foundation::SYSTEMTIME {
            wYear: 2026,
            wMonth: 9,
            wDayOfWeek: 2,
            wDay: 22,
            wHour: 20,
            wMinute: 58,
            wSecond: 49,
            wMilliseconds: 925,
        };
        assert_eq!(format_stamp(&now), "2026-09-22T20:58:49.925Z");
    }

    #[test]
    fn rotation_moves_only_an_oversized_log_aside() {
        let dir = std::env::temp_dir().join(format!("peebify-helper-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlay-helper.log");
        let rotated = dir.join("overlay-helper.old.log");
        std::fs::write(&rotated, b"stale").unwrap();

        std::fs::write(&path, [b'x'; 8]).unwrap();
        assert!(!rotate_if_larger(&path, 8));
        assert!(path.exists());

        std::fs::write(&path, [b'x'; 9]).unwrap();
        assert!(rotate_if_larger(&path, 8));
        assert!(!path.exists());
        assert_eq!(std::fs::read(&rotated).unwrap().len(), 9);

        assert!(!rotate_if_larger(&path, 8));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
