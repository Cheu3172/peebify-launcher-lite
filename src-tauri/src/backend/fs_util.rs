// ------------ File System Helpers ------------
// Shared odds and ends for touching the disk: finding bundled resources, safe atomic writes, working out why a write failed, path safety checks, MD5 hashing and reading the JSON stores.
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use md5::{Digest, Md5};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Network,
    DiskFull,
    Validation,
    Cancelled,
    Locked,
    AccessDenied,
    DeviceGone,
    FileTooLarge,
    ShareLost,
    Other,
}

pub const SHARE_LOST_ATTEMPTS: u32 = 3;

pub fn resource(app: &tauri::AppHandle, name: &str) -> Option<PathBuf> {
    use tauri::Manager;

    if let Ok(resource_dir) = app.path().resource_dir() {
        for candidate in [
            resource_dir.join("resources").join(name),
            resource_dir.join(name),
        ] {
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    if cfg!(debug_assertions) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for candidate in [
            root.join("resources").join(name),
            root.join("..").join("target").join("release").join(name),
            root.join("..").join("target").join("debug").join(name),
        ] {
            if candidate.exists() {
                log::info!(
                    "resolved {name} to {} (modified {:?})",
                    candidate.display(),
                    std::fs::metadata(&candidate)
                        .and_then(|m| m.modified())
                        .ok()
                );
                return Some(candidate);
            }
        }
    }
    None
}

pub fn allow_asset_dir(app: &tauri::AppHandle, dir: &Path) {
    if !asset_scope_accepts(dir) {
        log::warn!("asset scope: ignored the relative folder {}", dir.display());
        return;
    }
    if dir.parent().is_none() {
        log::warn!("asset scope: ignored the drive root {}", dir.display());
        return;
    }
    allow_asset_dir_shallow(app, dir);
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()) {
            allow_asset_dir_shallow(app, &entry.path());
        }
    }
}

pub fn allow_asset_dir_shallow(app: &tauri::AppHandle, dir: &Path) {
    use tauri::Manager;

    if already_allowed(dir) {
        return;
    }
    match app.asset_protocol_scope().allow_directory(dir, false) {
        Ok(()) => remember_allowed(dir),
        Err(e) => log::warn!("asset scope: could not allow {}: {e}", dir.display()),
    }
}

pub fn allow_asset_file(app: &tauri::AppHandle, file: &Path) {
    use tauri::Manager;

    if !asset_scope_accepts(file) {
        log::warn!("asset scope: ignored the relative file {}", file.display());
        return;
    }
    if already_allowed(file) {
        return;
    }
    match app.asset_protocol_scope().allow_file(file) {
        Ok(()) => remember_allowed(file),
        Err(e) => log::warn!("asset scope: could not allow {}: {e}", file.display()),
    }
}

static ALLOWED_ASSET_PATHS: parking_lot::Mutex<Vec<PathBuf>> =
    parking_lot::Mutex::new(Vec::new());

fn already_allowed(path: &Path) -> bool {
    ALLOWED_ASSET_PATHS.lock().iter().any(|p| p == path)
}

fn remember_allowed(path: &Path) {
    let mut allowed = ALLOWED_ASSET_PATHS.lock();
    if !allowed.iter().any(|p| p == path) {
        allowed.push(path.to_path_buf());
    }
}

fn asset_scope_accepts(path: &Path) -> bool {
    path.is_absolute()
}

pub fn classify(message: &str) -> FailureKind {
    let msg = message.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| msg.contains(n));

    if has(&[
        "cancelled",
        "canceled",
        "aborted by user",
        "repair aborted",
        "download aborted",
    ]) {
        return FailureKind::Cancelled;
    }
    let code = os_error_code(&msg);
    if matches!(code, Some(112) | Some(39)) || has(&["there is not enough space"]) {
        return FailureKind::DiskFull;
    }
    if matches!(code, Some(21) | Some(55) | Some(483) | Some(1167)) {
        return FailureKind::DeviceGone;
    }
    if code == Some(223) {
        return FailureKind::FileTooLarge;
    }
    if msg.contains("could not replace") && has(&["[os:32]", "[os:33]", "[os:5]"]) {
        return FailureKind::Locked;
    }
    if has(&["[os:5]", "(os error 5)", "access is denied"]) {
        return FailureKind::AccessDenied;
    }
    if has(&["validation failed", "failed verification"]) {
        return FailureKind::Validation;
    }
    if has(&[
        "request error",
        "stream error",
        "error sending request",
        "decoding response body",
        "stalled",
        "timed out",
        "timeout",
        "dns error",
        "getaddrinfo",
        "socket hang up",
        "econnreset",
        "connection refused",
        "connection reset",
        "connection closed",
        "connection aborted",
    ]) {
        return FailureKind::Network;
    }
    if matches!(code, Some(59) | Some(64)) {
        return FailureKind::ShareLost;
    }
    FailureKind::Other
}

pub fn os_error_code(message: &str) -> Option<i32> {
    ["(os error ", "[os:"].iter().find_map(|marker| {
        let start = message.rfind(marker)? + marker.len();
        let digits: String = message[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    })
}

pub fn drive_failure_message(
    kind: FailureKind,
    dir: &Path,
    file: &Path,
    file_size: u64,
) -> Option<String> {
    match kind {
        FailureKind::DeviceGone => Some(format!(
            "The drive holding {} is no longer available. Reconnect it and try again.",
            dir.display()
        )),
        FailureKind::FileTooLarge => Some(fat_limit_message(file, file_size).unwrap_or_else(|| {
            format!(
                "{} is too large for the drive it is on. Pick an NTFS or exFAT drive.",
                file.display()
            )
        })),
        FailureKind::ShareLost => Some(format!(
            "The network drive holding {} stopped responding. Check the connection and try again.",
            dir.display()
        )),
        _ => None,
    }
}

pub fn fmt_io(context: &str, e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => format!("{context}: {e} [os:{code}]"),
        None => format!("{context}: {e}"),
    }
}

const REPLACE_ATTEMPTS: u32 = 5;
const REPLACE_RETRY_BASE_MS: u64 = 200;

pub fn finalize_replace(tmp_path: &Path, dest: &Path) -> Result<(), String> {
    replace_retrying(tmp_path, dest).map_err(|e| {
        fmt_io(
            &format!(
                "Could not replace {}. Is the game running, or is a file locked by antivirus?",
                dest.display()
            ),
            &e,
        )
    })
}

pub fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

pub fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| fmt_io(&format!("Could not create {}", parent.display()), &e))?;
    }
    let tmp = tmp_sibling(dest);
    let written = std::fs::File::create(&tmp).and_then(|mut file| {
        use std::io::Write;
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(fmt_io(&format!("Could not write {}", tmp.display()), &e));
    }
    replace_retrying(&tmp, dest).map_err(|e| {
        if dest.exists() {
            let _ = std::fs::remove_file(&tmp);
        }
        fmt_io(&format!("Could not save {}", dest.display()), &e)
    })
}

fn replace_retrying(tmp_path: &Path, dest: &Path) -> std::io::Result<()> {
    let mut attempt = 0u32;
    loop {
        match try_replace(tmp_path, dest) {
            Ok(()) => return Ok(()),
            Err(e) => {
                attempt += 1;
                let retryable = matches!(e.raw_os_error(), Some(5) | Some(32));
                if attempt >= REPLACE_ATTEMPTS || !retryable {
                    return Err(e);
                }
                log::warn!(
                    "Replacing {} is blocked ({e}), retry {attempt}/{REPLACE_ATTEMPTS}",
                    dest.display()
                );
                std::thread::sleep(Duration::from_millis(
                    REPLACE_RETRY_BASE_MS * u64::from(attempt),
                ));
            }
        }
    }
}

pub fn retry_while_locked<T>(
    mut op: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let mut attempt = 0u32;
    loop {
        match op() {
            Ok(value) => return Ok(value),
            Err(e) => {
                attempt += 1;
                let retryable = matches!(e.raw_os_error(), Some(5) | Some(32) | Some(33));
                if attempt >= REPLACE_ATTEMPTS || !retryable {
                    return Err(e);
                }
                log::warn!("A file is locked ({e}), retry {attempt}/{REPLACE_ATTEMPTS}");
                std::thread::sleep(Duration::from_millis(
                    REPLACE_RETRY_BASE_MS * u64::from(attempt),
                ));
            }
        }
    }
}

pub fn manifest_key(rel: &str) -> String {
    rel.replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
        .to_lowercase()
}

const WRITE_PROBE_NAME: &str = ".peebify-write-test";

pub fn probe_writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(WRITE_PROBE_NAME);
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)
}

pub fn is_access_denied(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(5)
}

pub fn not_writable_message(dir: &Path) -> String {
    format!(
        "Peebify cannot write to {}. Pick a folder you own, or copy the game into one and use Locate.",
        dir.display()
    )
}

pub fn dir_denies_writes(dir: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;

    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!("{WRITE_PROBE_NAME}-{}", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).custom_flags(0x0400_0000);
    match options.open(&probe) {
        Ok(file) => {
            drop(file);
            false
        }
        Err(e) => is_access_denied(&e),
    }
}

pub const FAT_MAX_FILE_BYTES: u64 = u32::MAX as u64;

fn is_fat_name(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "FAT" | "FAT12" | "FAT16" | "FAT32"
    )
}

pub fn volume_fs_name(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};
    let existing = path.ancestors().find(|p| p.exists())?;
    let wide: Vec<u16> = existing
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut root = [0u16; 1024];
    if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return None;
    }
    let mut fs_name = [0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = fs_name.iter().position(|&c| c == 0).unwrap_or(fs_name.len());
    Some(String::from_utf16_lossy(&fs_name[..len]))
}

pub fn fat_limit_message(path: &Path, file_size: u64) -> Option<String> {
    if file_size <= FAT_MAX_FILE_BYTES {
        return None;
    }
    let name = volume_fs_name(path)?;
    is_fat_name(&name).then(|| {
        format!(
            "This drive is formatted as {name}, which cannot store files larger than 4 GB. Pick an NTFS or exFAT drive."
        )
    })
}

pub fn explain_disk_full(path: &Path, file_size: u64, message: String) -> String {
    if classify(&message) != FailureKind::DiskFull {
        return message;
    }
    match fat_limit_message(path, file_size) {
        Some(hint) => format!("{hint} ({message})"),
        None => message,
    }
}

pub fn write_all_at(file: &std::fs::File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let written = file.seek_write(buf, offset)?;
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "wrote zero bytes",
            ));
        }
        buf = &buf[written..];
        offset += written as u64;
    }
    Ok(())
}

fn try_replace(tmp_path: &Path, dest: &Path) -> std::io::Result<()> {
    let first = match std::fs::rename(tmp_path, dest) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    let Ok(meta) = std::fs::metadata(dest) else {
        return Err(first);
    };
    let mut perms = meta.permissions();
    if !perms.readonly() {
        return Err(first);
    }
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    if std::fs::set_permissions(dest, perms).is_err() {
        return Err(first);
    }
    std::fs::rename(tmp_path, dest)
}

pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let normalised = rel.replace('\\', "/");
    let trimmed = normalised.trim_start_matches('/');
    if trimmed.is_empty() {
        return Err("refusing empty path from manifest".to_string());
    }

    let mut out = root.to_path_buf();
    let mut pushed = 0usize;
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                if name.contains(':') {
                    return Err(format!(
                        "refusing manifest path '{rel}': ':' is not allowed in a name"
                    ));
                }
                if let Some(problem) = unsafe_name_problem(&name) {
                    return Err(format!("refusing manifest path '{rel}': {problem}"));
                }
                out.push(part);
                pushed += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("refusing manifest path '{rel}': contains '..'"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "refusing manifest path '{rel}': not a relative path"
                ));
            }
        }
    }

    if pushed == 0 {
        return Err(format!(
            "refusing manifest path '{rel}': it names no file under the install"
        ));
    }

    Ok(out)
}

const RESERVED_DEVICE_NAMES: [&str; 24] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "CONIN$",
    "CONOUT$",
];

fn unsafe_name_problem(name: &str) -> Option<&'static str> {
    if name.trim_end_matches(['.', ' ']).is_empty() {
        return Some("a name made only of dots or spaces");
    }
    if name
        .chars()
        .any(|c| (c as u32) < 0x20 || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return Some("a name with characters Windows does not allow");
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    RESERVED_DEVICE_NAMES
        .contains(&stem.as_str())
        .then_some("a reserved device name")
}

pub fn find_link(dir: &Path) -> Option<PathBuf> {
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                return Some(path);
            }
            if meta.is_dir() {
                pending.push(path);
            }
        }
    }
    None
}

const STREAM_CHUNK_SIZE: usize = 1 << 20;
thread_local! {
    static HASH_BUF: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}
pub const CANCELLED_MSG: &str = "Operation cancelled by user.";

pub fn md5_hex(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn md5_file(
    path: &Path,
    should_cancel: &mut dyn FnMut() -> bool,
    on_progress: &mut dyn FnMut(u64),
) -> Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| super::fs_util::fmt_io(&format!("Could not read {}", path.display()), &e))?;
    let mut hasher = Md5::new();
    let mut hash_with = |buf: &mut [u8]| -> Result<(), String> {
        loop {
            if should_cancel() {
                return Err(CANCELLED_MSG.to_string());
            }
            let read = file.read(buf).map_err(|e| {
                super::fs_util::fmt_io(&format!("Could not read {}", path.display()), &e)
            })?;
            if read == 0 {
                return Ok(());
            }
            hasher.update(&buf[..read]);
            on_progress(read as u64);
        }
    };
    HASH_BUF.with(|cell| match cell.try_borrow_mut() {
        Ok(mut buf) => {
            if buf.len() < STREAM_CHUNK_SIZE {
                buf.resize(STREAM_CHUNK_SIZE, 0);
            }
            hash_with(&mut buf)
        }
        Err(_) => hash_with(&mut vec![0u8; STREAM_CHUNK_SIZE]),
    })?;
    Ok(hex::encode(hasher.finalize()))
}

pub fn sanitize_folder_name(name: &str) -> String {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']).trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let capped: String = trimmed.chars().take(120).collect();
    let stem = capped.split('.').next().unwrap_or(&capped).to_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return format!("{capped}_");
    }
    capped
}

pub fn plain_path(path: &Path) -> String {
    let text = path.to_string_lossy().to_string();
    match text.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_string(),
    }
}

pub fn read_json_store(path: &Path) -> Result<Option<serde_json::Value>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(unfinished_save(path)),
        Err(e) => return Err(fmt_io(&format!("Could not read {}", path.display()), &e)),
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(Some(value)),
        Err(e) => {
            let aside = PathBuf::from(format!(
                "{}.corrupt-{}",
                path.display(),
                chrono::Utc::now().timestamp_millis()
            ));
            match std::fs::rename(path, &aside) {
                Ok(()) => log::warn!(
                    "{} could not be parsed ({e}), so it was set aside as {} and starts empty",
                    path.display(),
                    aside.display()
                ),
                Err(re) => log::warn!(
                    "{} could not be parsed ({e}) and could not be set aside ({re}), so it starts empty",
                    path.display()
                ),
            }
            Ok(None)
        }
    }
}

fn unfinished_save(path: &Path) -> Option<serde_json::Value> {
    let tmp = tmp_sibling(path);
    let bytes = std::fs::read(&tmp).ok()?;
    let value = serde_json::from_slice(&bytes).ok()?;
    log::warn!(
        "{} is missing, so the copy an unfinished save left in {} was used",
        path.display(),
        tmp.display()
    );
    Some(value)
}

// ------------ File System Tests ------------
// Covers the helpers above.
#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peebify-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn plain_path_strips_verbatim_prefixes() {
        assert_eq!(plain_path(Path::new(r"\\?\C:\x")), r"C:\x");
        assert_eq!(plain_path(Path::new(r"\\?\UNC\server\share")), r"\\server\share");
        assert_eq!(plain_path(Path::new(r"D:\Games")), r"D:\Games");
    }

    #[test]
    fn only_absolute_paths_can_widen_the_asset_scope() {
        assert!(asset_scope_accepts(Path::new(r"C:\Users\me\Videos\Peebify")));
        assert!(asset_scope_accepts(Path::new(r"\\?\D:\wall.png")));
        assert!(asset_scope_accepts(Path::new(r"\\server\share\caps")));
        assert!(!asset_scope_accepts(Path::new("")));
        assert!(!asset_scope_accepts(Path::new("wall.png")));
        assert!(!asset_scope_accepts(Path::new(r"\Users\me")));
        assert!(!asset_scope_accepts(Path::new("C:caps")));
    }

    #[test]
    fn the_static_asset_scope_is_only_the_launchers_own_folders() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        let allow: Vec<&str> = config["app"]["security"]["assetProtocol"]["scope"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(allow.contains(&"$DATA/Peebify Launcher/wallpaper-cache/**"));
        for pattern in allow {
            assert!(
                pattern.starts_with("$DATA/Peebify Launcher/") || pattern == "$VIDEO/Peebify/**",
                "{pattern}"
            );
        }
    }

    #[test]
    fn os_codes_are_read_from_both_spellings() {
        assert_eq!(os_error_code("write error: gone (os error 1167)"), Some(1167));
        assert_eq!(os_error_code("could not save x: denied [os:5]"), Some(5));
        assert_eq!(os_error_code("http 503 for https://a/b"), None);
        assert_eq!(os_error_code("(os error x)"), None);
    }

    #[test]
    fn drive_failures_are_classified_by_code() {
        let gone = fmt_io("Write error", &std::io::Error::from_raw_os_error(1167));
        assert_eq!(classify(&gone), FailureKind::DeviceGone);
        assert_eq!(classify("Write error: x (os error 21)"), FailureKind::DeviceGone);
        assert_eq!(classify("Write error: x (os error 223)"), FailureKind::FileTooLarge);
        assert_eq!(classify("Write error: x (os error 112)"), FailureKind::DiskFull);
        assert_eq!(classify("Write error: x (os error 64)"), FailureKind::ShareLost);
        assert_eq!(
            classify("Stream error: error decoding response body (os error 64)"),
            FailureKind::Network
        );
        let dir = Path::new("E:\\g");
        let file = dir.join("a.pak");
        assert!(drive_failure_message(FailureKind::DeviceGone, dir, &file, 1)
            .is_some_and(|m| m.contains("E:\\g")));
        assert!(drive_failure_message(FailureKind::Other, dir, &file, 1).is_none());
    }

    #[test]
    fn a_failed_replace_keeps_the_destination() {
        let dir = scratch("replace");
        let dest = dir.join("store.json");
        std::fs::write(&dest, b"old").unwrap();

        let missing_tmp = finalize_replace(&dir.join("absent.tmp"), &dest);
        let kept = std::fs::read(&dest).unwrap();

        let tmp = dir.join("store.json.tmp");
        std::fs::write(&tmp, b"new").unwrap();
        let mut perms = std::fs::metadata(&dest).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&dest, perms).unwrap();
        let replaced = finalize_replace(&tmp, &dest);
        let after = std::fs::read(&dest).unwrap();
        let tmp_left = tmp.exists();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(missing_tmp.is_err());
        assert_eq!(kept, b"old");
        assert!(replaced.is_ok());
        assert_eq!(after, b"new");
        assert!(!tmp_left);
    }

    #[test]
    fn atomic_writes_replace_and_leave_no_tmp() {
        let dir = scratch("atomic");
        let dest = dir.join("nested").join("state.json");

        let first = write_atomic(&dest, b"{\"a\":1}");
        let second = write_atomic(&dest, b"{\"a\":2}");
        let body = std::fs::read_to_string(&dest).unwrap();
        let tmp_left = tmp_sibling(&dest).exists();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(first.is_ok());
        assert!(second.is_ok());
        assert_eq!(body, "{\"a\":2}");
        assert!(!tmp_left);
    }

    #[test]
    fn a_store_whose_swap_never_finished_loads_from_its_tmp() {
        let dir = scratch("recover");
        let path = dir.join("profiles.json");
        std::fs::write(tmp_sibling(&path), b"{\"a\":1}").unwrap();

        let recovered = read_json_store(&path);
        std::fs::write(tmp_sibling(&path), b"{\"a\":").unwrap();
        let partial = read_json_store(&path);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(recovered, Ok(Some(serde_json::json!({ "a": 1 }))));
        assert_eq!(partial, Ok(None));
    }

    #[test]
    fn only_fat_family_names_hit_the_four_gigabyte_cap() {
        assert!(is_fat_name("FAT32"));
        assert!(is_fat_name("fat"));
        assert!(is_fat_name("FAT16"));
        assert!(!is_fat_name("exFAT"));
        assert!(!is_fat_name("NTFS"));
        assert!(!is_fat_name("ReFS"));
    }

    #[test]
    fn small_files_and_other_failures_skip_the_fat_hint() {
        let dir = std::env::temp_dir();
        assert_eq!(fat_limit_message(&dir, FAT_MAX_FILE_BYTES), None);
        let other = "Write error: stream error".to_string();
        assert_eq!(explain_disk_full(&dir, u64::MAX, other.clone()), other);
    }

    #[cfg(windows)]
    #[test]
    fn the_volume_of_an_existing_or_missing_path_is_named() {
        let missing = std::env::temp_dir().join("peebify-missing-volume-probe").join("x.pak");
        let name = volume_fs_name(&missing).unwrap();
        assert!(!name.is_empty());
    }

    #[test]
    fn json_stores_separate_missing_corrupt_and_unreadable() {
        let dir = std::env::temp_dir().join(format!("peebify-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.json");

        let missing = read_json_store(&path);
        std::fs::write(&path, b"{\"a\":1}").unwrap();
        let good = read_json_store(&path);
        std::fs::write(&path, b"{\"a\":").unwrap();
        let corrupt = read_json_store(&path);
        let set_aside = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("store.json.corrupt-"));
        let original_left = path.exists();
        std::fs::create_dir_all(&path).unwrap();
        let unreadable = read_json_store(&path);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(missing, Ok(None));
        assert_eq!(good, Ok(Some(serde_json::json!({ "a": 1 }))));
        assert_eq!(corrupt, Ok(None));
        assert!(set_aside);
        assert!(!original_left);
        assert!(unreadable.is_err());
    }

    #[test]
    fn a_store_with_invalid_utf8_is_set_aside_and_starts_empty() {
        let dir = scratch("store-utf8");
        let path = dir.join("library.json");
        std::fs::write(&path, b"{\"a\":\"\xff\xfe\"}").unwrap();

        let torn = read_json_store(&path);
        let set_aside = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("library.json.corrupt-"));
        let original_left = path.exists();
        std::fs::write(tmp_sibling(&path), b"{\"a\":\"\xff\"}").unwrap();
        let torn_tmp = read_json_store(&path);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(torn, Ok(None));
        assert!(set_aside);
        assert!(!original_left);
        assert_eq!(torn_tmp, Ok(None));
    }

    #[test]
    fn folder_names_keep_real_game_titles_readable() {
        assert_eq!(
            sanitize_folder_name("Honkai: Star Rail"),
            "Honkai_ Star Rail"
        );
        assert_eq!(
            sanitize_folder_name("Girls' Frontline 2: Exilium"),
            "Girls' Frontline 2_ Exilium"
        );
        assert_eq!(sanitize_folder_name("Wuthering Waves"), "Wuthering Waves");
    }

    #[test]
    fn folder_names_can_never_be_a_path() {
        for name in ["../escape", r"..\escape", "a/b", r"a\b", "C:/x"] {
            let safe = sanitize_folder_name(name);
            assert!(
                !safe.contains('/') && !safe.contains('\\') && !safe.contains(':'),
                "{name} sanitized to {safe}, which still has a separator"
            );
        }
    }

    #[test]
    fn folder_names_windows_would_reject_are_fixed() {
        assert_eq!(sanitize_folder_name("trailing..."), "trailing");
        assert_eq!(sanitize_folder_name("  padded  "), "padded");
        assert_eq!(sanitize_folder_name("NUL"), "NUL_");
        assert_eq!(sanitize_folder_name("con"), "con_");
        assert_eq!(sanitize_folder_name("COM1"), "COM1_");
        assert_eq!(sanitize_folder_name("NULL"), "NULL");
    }

    #[test]
    fn folder_names_with_nothing_usable_are_empty_for_the_caller_to_handle() {
        assert!(sanitize_folder_name("").is_empty());
        assert!(sanitize_folder_name("   ").is_empty());
        assert!(sanitize_folder_name("...").is_empty());
    }

    #[test]
    fn joined_manifest_paths_use_one_native_separator() {
        let root = Path::new(if cfg!(windows) { r"C:\game" } else { "/game" });
        let joined = safe_join(root, "Client/Content/Paks/pakchunk0.pak").unwrap();

        let rebuilt = joined.parent().unwrap().join(joined.file_name().unwrap());
        assert_eq!(joined, rebuilt);
        assert_eq!(joined.to_string_lossy(), rebuilt.to_string_lossy());

        if cfg!(windows) {
            assert!(
                !joined.to_string_lossy().contains('/'),
                "a joined path kept a foreign separator: {}",
                joined.display()
            );
        }
    }

    #[test]
    fn backslash_and_slash_manifests_agree() {
        let root = Path::new(if cfg!(windows) { r"C:\game" } else { "/game" });
        assert_eq!(
            safe_join(root, "a/b/c.pak").unwrap(),
            safe_join(root, r"a\b\c.pak").unwrap()
        );
    }

    #[test]
    fn traversal_and_empty_paths_are_refused() {
        let root = Path::new(if cfg!(windows) { r"C:\game" } else { "/game" });
        assert!(safe_join(root, "../escape.pak").is_err());
        assert!(safe_join(root, "a/../../escape.pak").is_err());
        assert!(safe_join(root, "").is_err());
        assert!(safe_join(root, "/").is_err());
        assert!(safe_join(root, ".").is_err());
        assert!(safe_join(root, "./").is_err());
    }

    #[test]
    fn names_windows_would_rewrite_or_treat_as_devices_are_refused() {
        let root = Path::new(r"C:\game");
        assert!(safe_join(root, "a/.. /escape.pak").is_err());
        assert!(safe_join(root, "a/.../b.pak").is_err());
        assert!(safe_join(root, "a/ ./b.pak").is_err());
        assert!(safe_join(root, "a/NUL").is_err());
        assert!(safe_join(root, "a/con.txt").is_err());
        assert!(safe_join(root, "a/b?.pak").is_err());
        assert!(safe_join(root, "a/b\u{1}.pak").is_err());
        assert!(safe_join(root, "Content/console.pak").is_ok());
        assert!(safe_join(root, "Content/.hidden/nul_table.bin").is_ok());
    }

    #[test]
    fn links_inside_an_extracted_tree_are_found() {
        let dir = std::env::temp_dir().join(format!("peebify-links-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("a").join("b")).unwrap();
        std::fs::write(dir.join("a").join("b").join("f.txt"), "x").unwrap();
        assert_eq!(find_link(&dir), None);
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.join("a").join("j"))
            .arg(std::env::temp_dir())
            .output()
            .is_ok_and(|o| o.status.success());
        if made {
            assert_eq!(find_link(&dir), Some(dir.join("a").join("j")));
            std::fs::remove_dir(dir.join("a").join("j")).unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_keys_agree_for_every_spelling_of_one_file() {
        let key = manifest_key("Client/Content/Paks/pakchunk0.pak");
        assert_eq!(manifest_key(r"/client\Content/./Paks/PAKCHUNK0.pak"), key);
        assert_eq!(manifest_key("CLIENT//content/paks/pakchunk0.pak"), key);
        assert_ne!(manifest_key("Client/Content/Paks/pakchunk1.pak"), key);
    }

    #[test]
    fn locked_and_denied_failures_are_told_apart() {
        let locked = fmt_io(
            "Could not replace C:\\g\\a.pak. Is the game running, or is a file locked by antivirus?",
            &std::io::Error::from_raw_os_error(32),
        );
        assert_eq!(classify(&format!("Finalize error for a.pak: {locked}")), FailureKind::Locked);
        let av = fmt_io("Could not replace C:\\g\\a.pak", &std::io::Error::from_raw_os_error(5));
        assert_eq!(classify(&av), FailureKind::Locked);
        assert_eq!(
            classify("Write error: Access is denied. (os error 5)"),
            FailureKind::AccessDenied
        );
        assert_eq!(classify("Write error: something (os error 50)"), FailureKind::Other);
        assert_eq!(classify("Download aborted by user."), FailureKind::Cancelled);
    }

    #[test]
    fn a_writable_folder_passes_both_probes_and_leaves_nothing() {
        let dir = std::env::temp_dir().join(format!("peebify-probe-{}", uuid::Uuid::new_v4()));
        let nested = dir.join("a").join("b");

        let probed = probe_writable(&nested);
        let denied = dir_denies_writes(&nested);
        let leftovers = std::fs::read_dir(&nested).map(|rd| rd.count()).unwrap_or(99);
        let missing_denied = dir_denies_writes(&dir.join("missing"));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(probed.is_ok());
        assert!(!denied);
        assert_eq!(leftovers, 0);
        assert!(!missing_denied);
    }

    #[test]
    fn md5_file_reuses_its_buffer_across_files_and_nested_calls() {
        let dir = scratch("md5");
        let small = dir.join("small.bin");
        let large = dir.join("large.bin");
        let large_bytes: Vec<u8> = (0..STREAM_CHUNK_SIZE * 2 + 7).map(|i| i as u8).collect();
        std::fs::write(&small, b"abc").unwrap();
        std::fs::write(&large, &large_bytes).unwrap();

        let large_md5 = md5_file(&large, &mut || false, &mut |_| {}).unwrap();
        let small_md5 = md5_file(&small, &mut || false, &mut |_| {}).unwrap();
        let mut nested = None;
        let outer = md5_file(&large, &mut || false, &mut |_| {
            if nested.is_none() {
                nested = Some(md5_file(&small, &mut || false, &mut |_| {}).unwrap());
            }
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(large_md5, md5_hex(&large_bytes));
        assert_eq!(small_md5, md5_hex(b"abc"));
        assert_eq!(outer, large_md5);
        assert_eq!(nested.as_deref(), Some(small_md5.as_str()));
    }
}

// ------------ Native Dialogs ------------
// File and folder pickers, plus opening a path or a web link with Windows.
pub mod dialog {
    use serde::Deserialize;
    use serde_json::{json, Value};
    use tauri::{AppHandle, Manager};
    use tauri_plugin_dialog::DialogExt;

    #[derive(Debug, Deserialize)]
    struct PathParam {
        path: String,
    }

    #[derive(Debug, Deserialize)]
    struct UrlParam {
        url: String,
    }

    #[derive(Debug, Deserialize, Default)]
    struct ShowOpenParams {
        #[serde(default)]
        title: String,
        #[serde(default)]
        directory: bool,
        #[serde(default)]
        multiple: bool,
        #[serde(rename = "defaultPath", default)]
        default_path: Option<String>,
        #[serde(default)]
        filters: Vec<FilterSpec>,
        #[serde(default)]
        owner: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct FilterSpec {
        name: String,
        extensions: Vec<String>,
    }

    pub async fn show_open(app: &AppHandle, params: Value) -> Result<Value, String> {
        let p: ShowOpenParams = serde_json::from_value(params).map_err(|e| e.to_string())?;
        let app = app.clone();

        let result = tauri::async_runtime::spawn_blocking(move || {
            let mut builder = app.dialog().file();
            let owner = p.owner.as_deref().filter(|s| !s.is_empty()).unwrap_or("main");
            if let Some(win) = app.get_webview_window(owner).filter(|w| {
                w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(true)
            }) {
                builder = builder.set_parent(&win);
            }
            if !p.title.is_empty() {
                builder = builder.set_title(&p.title);
            }
            if let Some(dp) = p.default_path.as_deref().filter(|s| !s.is_empty()) {
                builder = builder.set_directory(dp);
            }
            for f in &p.filters {
                let exts: Vec<&str> = f.extensions.iter().map(|s| s.as_str()).collect();
                builder = builder.add_filter(&f.name, &exts);
            }

            if p.directory {
                let folder = builder.blocking_pick_folder();
                map_paths(folder.map(|f| vec![f]))
            } else if p.multiple {
                let files = builder.blocking_pick_files();
                map_paths(files)
            } else {
                let file = builder.blocking_pick_file();
                map_paths(file.map(|f| vec![f]))
            }
        })
        .await
        .map_err(|e| e.to_string())?;

        Ok(result)
    }

    fn map_paths<I>(paths: Option<I>) -> Value
    where
        I: IntoIterator<Item = tauri_plugin_dialog::FilePath>,
    {
        match paths {
            Some(iter) => {
                let strs: Vec<String> = iter.into_iter().map(|p| p.to_string()).collect();
                json!({"canceled": false, "filePaths": strs})
            }
            None => json!({"canceled": true, "filePaths": []}),
        }
    }

    pub async fn open_path(params: Value) -> Result<Value, String> {
        let p: PathParam = serde_json::from_value(params).map_err(|e| e.to_string())?;

        let resolved =
            std::fs::canonicalize(&p.path).map_err(|_| format!("Folder not found: {}", p.path))?;
        if !resolved.is_dir() {
            return Err("Only folders can be opened.".to_string());
        }

        open::that_detached(super::plain_path(&resolved)).map_err(|e| e.to_string())?;
        Ok(Value::Null)
    }

    pub async fn open_external(params: Value) -> Result<Value, String> {
        let p: UrlParam = serde_json::from_value(params).map_err(|e| e.to_string())?;
        let url = validate_external_url(&p.url)?;

        open::that_detached(url).map_err(|e| e.to_string())?;
        Ok(Value::Null)
    }

    fn validate_external_url(raw: &str) -> Result<String, String> {
        let parsed = url::Url::parse(raw.trim()).map_err(|_| "Invalid URL.".to_string())?;
        match parsed.scheme() {
            "http" | "https" => {}
            other => return Err(format!("Refusing to open a {other}: link.")),
        }

        if parsed.host_str().is_none_or(str::is_empty) {
            return Err("Refusing to open a URL with no host.".to_string());
        }
        Ok(parsed.to_string())
    }
}
