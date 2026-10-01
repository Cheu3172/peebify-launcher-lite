// ------------ XXMI Mod Loader ------------
// The modding toolchain behind the Mods tab: one shared core (the 3DMigoto loader) plus a
// package per game (GIMI, SRMI, ZZMI, WWMI, HIMI, EFMI). It installs and updates them,
// keeps their settings file in order and starts the loader just before a game launches.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::{fs_util, game_profiles, http, process_utils, xxmi_update};
use peebify_helpers::mods::{describe_exit, Mode, EXE_NAME, EXIT_OK, EXIT_STOPPED, STOP_ALL_EVENT};

pub const LOADER_DLL: &str = "3dmloader.dll";
pub const CORE_KEY: &str = "xxmi";
pub const LOADER_LOG: &str = "loader.log";
pub const VERSION_FILE: &str = "VERSION.txt";
pub const OWNED_FILE: &str = "peebify-owned.json";
pub const OWN_SUBFOLDER: &str = "Peebify XXMI";

const CORE_LIBS: [&str; 2] = ["d3d11.dll", "d3dcompiler_47.dll"];
const CORE_FILES: [&str; 3] = [LOADER_DLL, "d3d11.dll", "d3dcompiler_47.dll"];
const LEGACY_FILES: [&str; 3] = ["Peebify Mod Loader.exe", "3dmloader.exe", "3dmloader.sha256"];

const PRESERVE_ON_UPDATE: [&str; 3] = ["Mods", "ShaderCache", "d3dx_user.ini"];
const KEEP_ON_UNINSTALL: [&str; 2] = ["Mods", "d3dx_user.ini"];
const RUNTIME_LEFTOVERS: [&str; 5] = [
    "Mods",
    "d3dx_user.ini",
    "ShaderCache",
    "d3d11_log.txt",
    "d3d11_profile_log.txt",
];
const XCMD_FILE: [&str; 2] = ["Core", "auto_update.xcmd"];
const XCMD_ROOTS: [&str; 2] = ["Core", "ShaderFixes"];
const ORPHAN_GRACE: Duration = Duration::from_secs(3);

const SHADERFIXES_EXTRAS: [&str; 4] = ["3dvision2sbs.ini", "help.ini", "mouse.ini", "upscale.ini"];
const ROGUE_MOD_FILES: [&str; 2] = ["d3dx.ini", "d3dx_user.ini"];
const ROGUE_SCAN_DEPTH: usize = 6;

const READY_TIMEOUT: Duration = Duration::from_secs(90);
const STOP_GRACE: Duration = Duration::from_millis(1500);
const WATCH_SLACK: Duration = Duration::from_secs(240);

// ------------ Folders And Records ------------
// Where the toolchain lives on disk, which GitHub repo each package comes from, and the
// small records of installed versions and files Peebify placed itself.
const PACKAGES: [(&str, &str); 7] = [
    (CORE_KEY, "SpectrumQT/XXMI-Libs-Package"),
    ("gimi", "SilentNightSound/GIMI-Package"),
    ("srmi", "SpectrumQT/SRMI-Package"),
    ("zzmi", "leotorrez/ZZMI-Package"),
    ("wwmi", "SpectrumQT/WWMI-Package"),
    ("himi", "leotorrez/HIMI-Package"),
    ("efmi", "SpectrumQT/EFMI-Package"),
];

pub fn repo_for(package: &str) -> Option<&'static str> {
    PACKAGES
        .iter()
        .find(|(key, _)| *key == package)
        .map(|(_, repo)| *repo)
}

fn is_known_variant(variant: &str) -> bool {
    variant != CORE_KEY && repo_for(variant).is_some()
}

pub fn root(config: &super::config::LauncherConfig, user_data: &Path) -> PathBuf {
    if let Value::String(custom) = config.get("behavior.modsPath") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    default_root(user_data)
}

pub fn default_root(user_data: &Path) -> PathBuf {
    user_data.join("mods").join(CORE_KEY)
}

fn variant_dir(root: &Path, variant: &str) -> PathBuf {
    root.join(variant)
}

pub fn package_dir(root: &Path, package: &str) -> PathBuf {
    if package == CORE_KEY {
        root.to_path_buf()
    } else {
        variant_dir(root, package)
    }
}

pub fn mods_dir(root: &Path, variant: &str) -> PathBuf {
    variant_dir(root, variant).join("Mods")
}

pub fn installed_versions(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(root.join(VERSION_FILE)) else {
        return out;
    };
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            let (key, value) = (key.trim(), value.trim());
            if !key.is_empty() && !value.is_empty() {
                out.insert(key.to_string(), value.to_string());
            }
        }
    }
    out
}

pub(super) fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let written = std::fs::File::create(&tmp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    let result = written.and_then(|_| {
        fs_util::finalize_replace(&tmp, path).map_err(std::io::Error::other)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn write_versions(root: &Path, versions: &BTreeMap<String, String>) -> Result<(), String> {
    let body: String = versions.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    write_replacing(&root.join(VERSION_FILE), body.as_bytes())
        .map_err(|e| format!("Could not write {VERSION_FILE}: {e}"))
}

fn read_owned(root: &Path) -> BTreeMap<String, BTreeSet<String>> {
    std::fs::read_to_string(root.join(OWNED_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_owned(root: &Path, owned: &BTreeMap<String, BTreeSet<String>>) -> Result<(), String> {
    let text = serde_json::to_string_pretty(owned).map_err(|e| e.to_string())?;
    write_replacing(&root.join(OWNED_FILE), text.as_bytes())
        .map_err(|e| format!("Could not write {OWNED_FILE}: {e}"))
}

pub fn record_owned(root: &Path, package: &str, names: &[String]) -> Result<(), String> {
    let mut owned = read_owned(root);
    owned
        .entry(package.to_string())
        .or_default()
        .extend(names.iter().filter(|n| is_plain_name(n)).cloned());
    write_owned(root, &owned)
}

fn forget_owned(root: &Path, package: &str) {
    let mut owned = read_owned(root);
    if owned.remove(package).is_some() {
        if let Err(e) = write_owned(root, &owned) {
            log::warn!("xxmi: {e}");
        }
    }
}

fn is_plain_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
        && !name.contains(':')
}

pub fn is_toolchain_folder(dir: &Path) -> bool {
    dir.join(VERSION_FILE).is_file() || dir.join(OWNED_FILE).is_file()
}

pub fn record_version(root: &Path, package: &str, version: &str) -> Result<(), String> {
    let mut versions = installed_versions(root);
    versions.insert(package.to_string(), version.to_string());
    write_versions(root, &versions)
}

const GAME_DIR_LEGACY_FILES: [&str; 4] = [LEGACY_FILES[0], LEGACY_FILES[1], LEGACY_FILES[2], LOADER_DLL];
const MIGOTO_GAME_FILES: [&str; 5] = [
    "d3d11.dll",
    "d3dcompiler_47.dll",
    "d3dx.ini",
    "d3dx_user.ini",
    "d3dxdm.ini",
];
const MIGOTO_GAME_DIRS: [&str; 2] = ["ShaderFixes", "ShaderCache"];

fn names_peebify(value: &str) -> bool {
    let value = value.trim().trim_matches('"');
    let name = value.rsplit(['\\', '/']).next().unwrap_or_default();
    name.to_ascii_lowercase().contains("peebify")
        || LEGACY_FILES
            .iter()
            .any(|legacy| legacy.eq_ignore_ascii_case(name))
}

fn peebify_placed_migoto(dir: &Path) -> bool {
    let Ok(bytes) = std::fs::read(dir.join("d3dx.ini")) else {
        return false;
    };
    LEGACY_FILES.iter().any(|name| dir.join(name).is_file())
        || String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(ini_key_value)
            .any(|(_, value)| names_peebify(value))
}

fn sweep_path(path: &Path, is_dir: bool, removed: &mut Vec<String>) -> bool {
    if std::fs::symlink_metadata(path).is_err() {
        return true;
    }
    let result = if is_dir {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Ok(()) => {
            removed.push(path.display().to_string());
            true
        }
        Err(e) => {
            log::warn!("xxmi: could not remove {}: {e}", path.display());
            false
        }
    }
}

pub fn sweep_game_dirs(dirs: &[PathBuf]) -> Vec<String> {
    let mut removed = Vec::new();
    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let owned_migoto = peebify_placed_migoto(dir);
        if !owned_migoto {
            for name in GAME_DIR_LEGACY_FILES {
                let path = dir.join(name);
                if path.is_file() && std::fs::remove_file(&path).is_ok() {
                    removed.push(path.display().to_string());
                }
            }
            for name in MIGOTO_GAME_FILES {
                let path = dir.join(name);
                if path.is_file() {
                    log::info!(
                        "xxmi: left {} in place because Peebify did not put it there",
                        path.display()
                    );
                }
            }
            for name in MIGOTO_GAME_DIRS {
                let path = dir.join(name);
                if path.is_dir() {
                    log::info!(
                        "xxmi: left {} in place because Peebify did not put it there",
                        path.display()
                    );
                }
            }
            continue;
        }
        let mut payload_gone = true;
        for name in MIGOTO_GAME_FILES.iter().filter(|name| **name != "d3dx.ini") {
            payload_gone &= sweep_path(&dir.join(name), false, &mut removed);
        }
        for name in MIGOTO_GAME_DIRS {
            let path = dir.join(name);
            if path.is_dir() {
                payload_gone &= sweep_path(&path, true, &mut removed);
            }
        }
        if !payload_gone || !sweep_path(&dir.join("d3dx.ini"), false, &mut removed) {
            continue;
        }
        for name in GAME_DIR_LEGACY_FILES {
            let path = dir.join(name);
            if path.is_file() {
                sweep_path(&path, false, &mut removed);
            }
        }
    }
    if !removed.is_empty() {
        log::info!(
            "xxmi: removed {} stray file(s) from game folders",
            removed.len()
        );
    }
    removed
}

pub fn core_installed(root: &Path) -> bool {
    CORE_FILES.iter().all(|name| root.join(name).is_file())
}

pub fn variant_installed(root: &Path, variant: &str) -> bool {
    core_installed(root) && variant_dir(root, variant).join("d3dx.ini").exists()
}

pub fn boot(app: &AppHandle) {
    let state = app.state::<super::state::BackendState>();
    let root = root(&state.config, &state.user_data);
    tauri::async_runtime::spawn_blocking(move || super::mods::sweep_replaced_copies(&root));
    if stop_all_event().is_none() {
        log::warn!("xxmi: could not create the mod loader's shared stop event");
    }
    xxmi_update::start_background(app);
}

// ------------ Uninstall ------------
// Removes only what Peebify put there and keeps the player's Mods folder and settings.
#[derive(Default, Debug)]
pub struct Uninstalled {
    pub kept: Vec<String>,
    pub leftovers: Vec<String>,
}

fn remove_owned_path(path: &Path, leftovers: &mut Vec<String>) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    let result = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    if let Err(e) = result {
        log::warn!("xxmi: could not remove {}: {e}", path.display());
        leftovers.push(path.display().to_string());
    }
}

fn has_mods(dir: &Path) -> bool {
    let mods = dir.join("Mods");
    mods.is_dir() && std::fs::read_dir(&mods).map(|d| d.count()).unwrap_or(0) > 0
}

fn is_package_key(name: &str) -> bool {
    PACKAGES.iter().any(|(key, _)| key.eq_ignore_ascii_case(name))
}

fn clear_variant_dir(
    root: &Path,
    variant: &str,
    recorded: Option<&BTreeSet<String>>,
    leftovers: &mut Vec<String>,
) -> bool {
    let dir = variant_dir(root, variant);
    if !dir.is_dir() {
        return false;
    }
    let kept = has_mods(&dir);
    let names: Vec<String> = match recorded {
        Some(recorded) => {
            let mut names: BTreeSet<String> = recorded.clone();
            names.extend(CORE_LIBS.iter().map(|n| n.to_string()));
            if !kept {
                names.extend(RUNTIME_LEFTOVERS.iter().map(|n| n.to_string()));
            }
            names.into_iter().collect()
        }
        None => match std::fs::read_dir(&dir) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect(),
            Err(e) => {
                log::warn!("xxmi: could not read {}: {e}", dir.display());
                leftovers.push(dir.display().to_string());
                return kept;
            }
        },
    };
    for name in names {
        if kept && KEEP_ON_UNINSTALL.iter().any(|k| k.eq_ignore_ascii_case(&name)) {
            continue;
        }
        remove_owned_path(&dir.join(&name), leftovers);
    }
    if !kept && std::fs::remove_dir(&dir).is_err() && dir.is_dir() {
        log::info!(
            "xxmi: left {} in place because it holds files Peebify did not install",
            dir.display()
        );
    }
    kept
}

pub fn uninstall_variant(root: &Path, variant: &str, owns_root: bool) -> Result<Uninstalled, String> {
    if !is_known_variant(variant) {
        return Err(format!("Peebify has no mod loader called \"{variant}\"."));
    }
    let mut result = Uninstalled::default();
    let owned = read_owned(root);
    let mut versions = installed_versions(root);
    let recorded = owned.get(variant);
    if owns_root || recorded.is_some() || versions.contains_key(variant) {
        let recorded = if owns_root { None } else { recorded };
        if clear_variant_dir(root, variant, recorded, &mut result.leftovers) {
            result.kept.push(variant.to_string());
        }
    }
    forget_owned(root, variant);

    if versions.remove(variant).is_some() {
        if let Err(e) = write_versions(root, &versions) {
            log::warn!("xxmi: {e}");
        }
    }

    let any_variant_left = versions
        .keys()
        .any(|key| key != CORE_KEY && package_looks_installed(root, key));
    if !any_variant_left {
        let core = uninstall(root, owns_root)?;
        result.leftovers.extend(core.leftovers);
    }

    log::info!(
        "xxmi: uninstalled the {variant} toolchain, kept mods: {}, left behind: {}",
        !result.kept.is_empty(),
        result.leftovers.len()
    );
    Ok(result)
}

pub fn uninstall(root: &Path, owns_root: bool) -> Result<Uninstalled, String> {
    let mut result = Uninstalled::default();
    if !root.is_dir() {
        return Ok(result);
    }
    let owned = read_owned(root);
    let versions = installed_versions(root);

    for (key, _) in PACKAGES.iter().filter(|(key, _)| *key != CORE_KEY) {
        let recorded = owned.get(*key);
        if !owns_root && recorded.is_none() && !versions.contains_key(*key) {
            continue;
        }
        let recorded = if owns_root { None } else { recorded };
        if clear_variant_dir(root, key, recorded, &mut result.leftovers) {
            result.kept.push(key.to_string());
        }
    }

    let mut names: BTreeSet<String> = CORE_FILES
        .iter()
        .chain(LEGACY_FILES.iter())
        .chain(
            [
                VERSION_FILE,
                LOADER_LOG,
                OWNED_FILE,
                xxmi_update::STATE_FILE,
                xxmi_update::STAGING_DIR,
            ]
            .iter(),
        )
        .map(|n| n.to_string())
        .collect();
    if owns_root {
        let entries =
            std::fs::read_dir(root).map_err(|e| format!("Could not read {root:?}: {e}"))?;
        names.extend(
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| !is_package_key(n)),
        );
    } else if let Some(core) = owned.get(CORE_KEY) {
        names.extend(
            core.iter()
                .filter(|n| is_plain_name(n) && !is_package_key(n) && !n.eq_ignore_ascii_case("Mods"))
                .cloned(),
        );
    }
    for name in names {
        remove_owned_path(&root.join(name), &mut result.leftovers);
    }

    log::info!(
        "xxmi: uninstalled the toolchain, kept mods for {:?}, left behind: {}",
        result.kept,
        result.leftovers.len()
    );
    Ok(result)
}

// ------------ Install And Update ------------
// Makes sure the core and a game's package are present and current, downloading and
// unpacking them under a per-package lock so two installs never collide.
static INSTALLING: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static INSTALL_DONE: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub struct InstallGuard(String);

impl Drop for InstallGuard {
    fn drop(&mut self) {
        if let Some(set) = INSTALLING.lock().as_mut() {
            set.remove(&self.0);
        }
        INSTALL_DONE.notify_waiters();
    }
}

pub fn try_lock_variant(variant: &str) -> Option<InstallGuard> {
    let mut guard = INSTALLING.lock();
    let set = guard.get_or_insert_with(HashSet::new);
    if !set.insert(variant.to_string()) {
        return None;
    }
    Some(InstallGuard(variant.to_string()))
}

pub async fn lock_variant_when_free(variant: &str, patience: Duration) -> Option<InstallGuard> {
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        let notified = INSTALL_DONE.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(guard) = try_lock_variant(variant) {
            return Some(guard);
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let _ = tokio::time::timeout(remaining, notified).await;
    }
}

pub async fn ensure_variant(
    app: &AppHandle,
    root: &Path,
    variant: &str,
    check_updates: bool,
    mut on_percent: impl FnMut(&str, f64),
) -> Result<bool, String> {
    if !is_known_variant(variant) {
        return Err(format!("Peebify has no mod loader called \"{variant}\"."));
    }

    let installed = installed_versions(root);
    let mut changed = false;
    let mut check_failed = None;
    for package in [CORE_KEY, variant] {
        let present = installed.contains_key(package) && package_looks_installed(root, package);
        if present && !check_updates {
            continue;
        }
        let damaged = present
            && package == CORE_KEY
            && match xxmi_update::verify_integrity(root) {
                Ok(_) => false,
                Err(e) => {
                    log::error!("xxmi: the core mod tools failed their checksum, reinstalling them: {e}");
                    true
                }
            };
        let release = match xxmi_update::check_package(root, package).await {
            Ok(release) => release,
            Err(e) if present && !damaged => {
                log::warn!("xxmi: could not check {package} for updates: {e}");
                check_failed.get_or_insert(e);
                continue;
            }
            Err(e) if damaged => {
                return Err(format!(
                    "Couldn't download a fresh copy of the mod tools to repair them: {e}"
                ));
            }
            Err(e) => return Err(e),
        };
        if present && !damaged && installed.get(package) == Some(&release.tag) {
            continue;
        }
        xxmi_update::install_package(app, root, package, &release, |p| on_percent(package, p))
            .await?;
        changed = true;
    }

    let (link_root, link_variant) = (root.to_path_buf(), variant.to_string());
    tauri::async_runtime::spawn_blocking(move || link_core_libs(&link_root, &link_variant, changed))
        .await
        .map_err(|e| format!("Could not place the mod tools: {e}"))??;
    std::fs::create_dir_all(mods_dir(root, variant))
        .map_err(|e| format!("Could not create the Mods folder: {e}"))?;
    if let Some(e) = check_failed.filter(|_| !changed) {
        return Err(format!("Couldn't check the mod tools for updates: {e}"));
    }
    Ok(changed)
}

fn package_looks_installed(root: &Path, package: &str) -> bool {
    if package == CORE_KEY {
        core_installed(root)
    } else {
        variant_dir(root, package).join("d3dx.ini").exists()
    }
}

static SOURCE_DIGESTS: Mutex<Vec<(PathBuf, u64, std::time::SystemTime, String)>> =
    Mutex::new(Vec::new());

fn source_sha256(path: &Path, meta: &std::fs::Metadata) -> std::io::Result<String> {
    let modified = meta.modified()?;
    let len = meta.len();
    if let Some((.., digest)) = SOURCE_DIGESTS
        .lock()
        .iter()
        .find(|(p, l, m, _)| p == path && *l == len && *m == modified)
    {
        return Ok(digest.clone());
    }
    let digest = xxmi_update::file_sha256(path)?;
    let mut cache = SOURCE_DIGESTS.lock();
    cache.retain(|(p, ..)| p != path);
    cache.push((path.to_path_buf(), len, modified, digest.clone()));
    Ok(digest)
}

fn same_contents(source: &Path, target: &Path) -> bool {
    let (Ok(ms), Ok(mt)) = (std::fs::metadata(source), std::fs::metadata(target)) else {
        return false;
    };
    if ms.len() != mt.len() {
        return false;
    }
    match (source_sha256(source, &ms), xxmi_update::file_sha256(target)) {
        (Ok(hs), Ok(ht)) => hs == ht,
        _ => false,
    }
}

fn link_core_libs(root: &Path, variant: &str, force: bool) -> Result<(), String> {
    let target_dir = variant_dir(root, variant);
    for lib in CORE_LIBS {
        let source = root.join(lib);
        if !source.exists() {
            return Err(format!(
                "The XXMI mod tools are missing {lib}. Install them again to fix this."
            ));
        }
        let target = target_dir.join(lib);
        if !force && same_contents(&source, &target) {
            continue;
        }
        std::fs::copy(&source, &target)
            .map_err(|e| format!("Could not place {lib} in {variant}: {e}"))?;
    }
    Ok(())
}

pub fn disable_shaderfixes_extras(variant_dir: &Path) {
    let dir = variant_dir.join("ShaderFixes");
    for name in SHADERFIXES_EXTRAS {
        let path = dir.join(name);
        if path.is_file() {
            let parked = dir.join(format!("DISABLED_{name}"));
            let _ = std::fs::remove_file(&parked);
            if std::fs::rename(&path, &parked).is_ok() {
                log::info!("xxmi: parked ShaderFixes/{name}");
            }
        }
    }
}

fn park_rogue_config_files(dir: &Path, depth: usize) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut parked = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if depth > 0 && !name.to_ascii_uppercase().starts_with("DISABLED") {
                parked += park_rogue_config_files(&path, depth - 1);
            }
            continue;
        }
        if ROGUE_MOD_FILES.iter().any(|r| r.eq_ignore_ascii_case(&name)) {
            let target = dir.join(format!("DISABLED {name}"));
            let _ = std::fs::remove_file(&target);
            if std::fs::rename(&path, &target).is_ok() {
                log::warn!("xxmi: parked a stray {name} inside Mods at {}", dir.display());
                parked += 1;
            }
        }
    }
    parked
}

pub fn unwrap_single_dir(dir: &Path) -> PathBuf {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return dir.to_path_buf();
    };
    let entries: Vec<_> = entries.flatten().collect();
    if entries.len() == 1 && entries[0].path().is_dir() {
        return entries[0].path();
    }
    dir.to_path_buf()
}

pub fn merge_dir(source: &Path, destination: &Path) -> Result<Vec<String>, String> {
    let entries =
        std::fs::read_dir(source).map_err(|e| format!("Could not read {source:?}: {e}"))?;
    let mut written = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        let target = destination.join(&name);

        if PRESERVE_ON_UPDATE
            .iter()
            .any(|p| p.eq_ignore_ascii_case(&name_str))
            && target.exists()
        {
            log::debug!("xxmi: keeping existing {name_str}");
            continue;
        }

        if entry.path().is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| format!("Could not create {target:?}: {e}"))?;
            merge_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .map_err(|e| format!("Could not copy {name_str}: {e}"))?;
        }
        written.push(name_str);
    }
    Ok(written)
}

fn xcmd_delete_target(package_dir: &Path, value: &str) -> Result<PathBuf, String> {
    let parts: Vec<&str> = value
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect();
    let Some(first) = parts.first() else {
        return Err("an empty path".to_string());
    };
    if parts.iter().any(|p| p.contains(':')) {
        return Err(format!("{value} is not a relative path"));
    }
    if !XCMD_ROOTS.iter().any(|r| r.eq_ignore_ascii_case(first)) {
        return Err(format!("{value} is outside Core and ShaderFixes"));
    }
    if parts.len() == 1 {
        return Err(format!("{value} would remove a whole folder"));
    }
    Ok(parts.iter().fold(package_dir.to_path_buf(), |path, part| path.join(part)))
}

fn xcmd_commands(text: &str, section: &str) -> Vec<(String, String)> {
    let mut current = String::new();
    let mut commands = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current = trimmed[1..trimmed.len() - 1].trim().to_string();
            continue;
        }
        if !current.eq_ignore_ascii_case(section) {
            continue;
        }
        if let Some((name, value)) = ini_key_value(line) {
            commands.push((name.to_string(), value.to_string()));
        }
    }
    commands
}

pub fn xcmd_path(package_dir: &Path) -> PathBuf {
    XCMD_FILE.iter().fold(package_dir.to_path_buf(), |path, part| path.join(part))
}

pub fn run_update_commands(xcmd: &Path, section: &str, package_dir: &Path) -> usize {
    let Ok(text) = std::fs::read_to_string(xcmd) else {
        return 0;
    };
    let mut removed = 0;
    for (name, value) in xcmd_commands(&text, section) {
        if !name.eq_ignore_ascii_case("delete") {
            log::warn!("xxmi: skipped the unknown {section} command {name} = {value}");
            continue;
        }
        let target = match xcmd_delete_target(package_dir, &value) {
            Ok(target) => target,
            Err(e) => {
                log::warn!("xxmi: skipped {section} delete = {value}: {e}");
                continue;
            }
        };
        let Ok(meta) = std::fs::symlink_metadata(&target) else {
            continue;
        };
        let result = if meta.is_dir() {
            std::fs::remove_dir_all(&target)
        } else {
            std::fs::remove_file(&target)
        };
        match result {
            Ok(()) => {
                log::info!("xxmi: {section} removed {}", target.display());
                removed += 1;
            }
            Err(e) => log::warn!("xxmi: {section} could not remove {}: {e}", target.display()),
        }
    }
    removed
}

pub async fn download_to(
    url: &str,
    destination: &Path,
    on_percent: impl FnMut(f64),
) -> Result<(), String> {
    let on_percent = Mutex::new(on_percent);
    http::with_retry(
        || download_attempt(url, destination, &on_percent),
        3,
        1000,
        "xxmi package download",
    )
    .await?
}

const MAX_PACKAGE_BYTES: u64 = 512 << 20;

fn retryable_download_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..600).contains(&status)
}

async fn download_attempt<F: FnMut(f64)>(
    url: &str,
    destination: &Path,
    on_percent: &Mutex<F>,
) -> Result<Result<(), String>, String> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let response = http::download_client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        let error = format!("HTTP {status} downloading {url}");
        return if retryable_download_status(status) {
            Err(error)
        } else {
            Ok(Err(error))
        };
    }
    let total = response.content_length().unwrap_or(0);
    if total > MAX_PACKAGE_BYTES {
        return Ok(Err(format!(
            "{url} is {total} bytes, more than any mod tools package should be"
        )));
    }

    let mut file = match tokio::fs::File::create(destination).await {
        Ok(file) => file,
        Err(e) => return Ok(Err(format!("Could not create {destination:?}: {e}"))),
    };
    let mut stream = response.bytes_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download interrupted: {e}"))?;
        if written.saturating_add(chunk.len() as u64) > MAX_PACKAGE_BYTES {
            return Ok(Err(format!(
                "{url} sent more than {MAX_PACKAGE_BYTES} bytes, more than any mod tools package should be"
            )));
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Write failed: {e}"))?;
        written += chunk.len() as u64;
        if total > 0 {
            (on_percent.lock())((written as f64 / total as f64) * 100.0);
        }
    }
    file.flush()
        .await
        .map_err(|e| format!("Flush failed: {e}"))?;
    Ok(Ok(()))
}

// ------------ Loader Ini Settings ------------
// Writes Peebify's managed options into the loader's d3dx.ini and leaves any other line
// the player edited alone.
#[derive(Default)]
pub struct IniOptions {
    pub target: String,
    pub hunting_mode: u64,
    pub reload_key: String,
    pub defaults: Vec<(String, String, String)>,
    pub screen: Option<(i32, i32)>,
}

pub(super) enum IniEntry {
    Set(String),
    Unset,
    EnsureLine(String),
}

pub(super) fn is_reserved(section: &str, key: &str) -> bool {
    const RESERVED: [(&str, &str); 10] = [
        ("Loader", "target"),
        ("Loader", "loader"),
        ("Loader", "launch"),
        ("Include", "include_recursive"),
        ("Include", "exclude_recursive"),
        ("Hunting", "hunting"),
        ("Hunting", "reload_fixes"),
        ("Hunting", "reload_config"),
        ("System", "check_foreground_window"),
        ("System", "additional_foreground_window"),
    ];
    RESERVED
        .iter()
        .any(|(s, k)| s.eq_ignore_ascii_case(section) && k.eq_ignore_ascii_case(key))
}

pub const HUNTING_KEYBINDS_ONLY: u64 = 2;

pub const DEFAULT_RELOAD_KEY: &str = "no_modifiers VK_F10";

fn write_ini_entries(path: &Path, entries: &[(String, String, IniEntry)]) -> Result<bool, String> {
    let original =
        std::fs::read_to_string(path).map_err(|e| format!("Could not read d3dx.ini: {e}"))?;
    let updated = rewrite_ini(&original, entries);
    if updated == original {
        return Ok(false);
    }
    write_replacing(path, updated.as_bytes()).map_err(|e| format!("Could not write d3dx.ini: {e}"))?;
    Ok(true)
}

fn override_entries(overrides: &[(String, String, String)]) -> Vec<(String, String, IniEntry)> {
    overrides
        .iter()
        .filter(|(section, key, _)| !is_reserved(section, key))
        .map(|(section, key, value)| (section.clone(), key.clone(), IniEntry::Set(value.clone())))
        .collect()
}

fn apply_managed_ini(
    variant_dir: &Path,
    options: &IniOptions,
    overrides: &[(String, String, String)],
) -> Result<(), String> {
    let mut entries = override_entries(&options.defaults);
    if let Some((width, height)) = options.screen {
        entries.push(("System".into(), "screen_width".into(), IniEntry::Set(width.to_string())));
        entries.push(("System".into(), "screen_height".into(), IniEntry::Set(height.to_string())));
    }
    entries.append(&mut override_entries(overrides));

    let mut managed: Vec<(String, String, IniEntry)> = vec![
        (
            "Loader".into(),
            "loader".into(),
            IniEntry::Set(EXE_NAME.to_string()),
        ),
        ("Loader".into(), "launch".into(), IniEntry::Unset),
        (
            "Include".into(),
            "include_recursive".into(),
            IniEntry::Set("Mods".into()),
        ),
        (
            "Include".into(),
            "exclude_recursive".into(),
            IniEntry::EnsureLine("DISABLED*".into()),
        ),
        (
            "Hunting".into(),
            "hunting".into(),
            IniEntry::Set(options.hunting_mode.to_string()),
        ),
        (
            "Hunting".into(),
            "reload_fixes".into(),
            IniEntry::Set(options.reload_key.clone()),
        ),
        (
            "Hunting".into(),
            "reload_config".into(),
            IniEntry::Set(options.reload_key.clone()),
        ),
        (
            "System".into(),
            "check_foreground_window".into(),
            IniEntry::Set("1".into()),
        ),
        (
            "System".into(),
            "additional_foreground_window".into(),
            IniEntry::Set("Peebify Overlay".into()),
        ),
    ];
    if !options.target.is_empty() {
        managed.insert(
            0,
            (
                "Loader".into(),
                "target".into(),
                IniEntry::Set(options.target.clone()),
            ),
        );
    }

    entries.append(&mut managed);
    write_ini_entries(&variant_dir.join("d3dx.ini"), &entries).map(|_| ())
}

pub(super) fn stored_overrides(
    config: &super::config::LauncherConfig,
    game_id: &str,
) -> Vec<(String, String, String)> {
    let Value::Object(map) = config.get(&format!("games.{game_id}.modIni")) else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(compound, value)| {
            let sanitized = super::mod_ini::sanitize_stored(compound, value);
            if sanitized.is_none() {
                log::warn!("Ignoring the saved mod loader setting \"{compound}\" for {game_id}: it is not a setting Peebify manages or its value is not valid.");
            }
            let (section, key, text) = sanitized?;
            Some((section.to_string(), key.to_string(), text))
        })
        .collect()
}

pub(super) fn apply_ini_overrides(
    variant_dir: &Path,
    overrides: &[(String, String, String)],
) -> Result<bool, String> {
    let entries = override_entries(overrides);
    if entries.is_empty() {
        return Ok(false);
    }
    write_ini_entries(&variant_dir.join("d3dx.ini"), &entries)
}

fn ini_key_value(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    if trimmed.starts_with(';') || trimmed.starts_with('#') {
        return None;
    }
    let (k, v) = trimmed.split_once('=')?;
    Some((k.trim(), v.trim()))
}

fn rewrite_ini(original: &str, managed: &[(String, String, IniEntry)]) -> String {
    let uses_crlf = original.contains("\r\n");
    let newline = if uses_crlf { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();

    for (section, key, entry) in managed {
        let value_safe = match entry {
            IniEntry::Set(value) | IniEntry::EnsureLine(value) => {
                super::mod_ini::ini_text_is_safe(value)
            }
            IniEntry::Unset => true,
        };
        if !value_safe
            || !super::mod_ini::ini_text_is_safe(section)
            || !super::mod_ini::ini_text_is_safe(key)
        {
            log::warn!("Skipped a d3dx.ini entry in [{}] because it contains a line break or a bracket.", section.escape_debug());
            continue;
        }
        let mut section_start: Option<usize> = None;
        let mut section_end = lines.len();

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
                continue;
            }
            let name = trimmed[1..trimmed.len() - 1].trim();
            if section_start.is_some() {
                section_end = i;
                break;
            }
            if name.eq_ignore_ascii_case(section) {
                section_start = Some(i);
            }
        }

        let Some(start) = section_start else {
            let value = match entry {
                IniEntry::Set(value) | IniEntry::EnsureLine(value) => value,
                IniEntry::Unset => continue,
            };
            if !lines.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
                lines.push(String::new());
            }
            lines.push(format!("[{section}]"));
            lines.push(format!("{key} = {value}"));
            continue;
        };

        let existing = (start + 1..section_end).find(|&i| {
            ini_key_value(&lines[i])
                .map(|(k, _)| k.eq_ignore_ascii_case(key))
                .unwrap_or(false)
        });

        let insert_line = |lines: &mut Vec<String>, text: String| {
            let mut insert_at = section_end;
            while insert_at > start + 1 && lines[insert_at - 1].trim().is_empty() {
                insert_at -= 1;
            }
            lines.insert(insert_at, text);
        };

        match (existing, entry) {
            (Some(i), IniEntry::Set(value)) => lines[i] = format!("{key} = {value}"),
            (Some(i), IniEntry::Unset) => lines[i] = format!(";{}", lines[i]),
            (None, IniEntry::Set(value)) => insert_line(&mut lines, format!("{key} = {value}")),
            (None, IniEntry::Unset) => {}
            (_, IniEntry::EnsureLine(value)) => {
                let present = (start + 1..section_end).any(|i| {
                    ini_key_value(&lines[i])
                        .map(|(k, v)| k.eq_ignore_ascii_case(key) && v.eq_ignore_ascii_case(value))
                        .unwrap_or(false)
                });
                if !present {
                    insert_line(&mut lines, format!("{key} = {value}"));
                }
            }
        }
    }

    let mut out = lines.join(newline);
    if original.ends_with('\n') && !out.ends_with('\n') {
        out.push_str(newline);
    }
    out
}

fn screen_size() -> Option<(i32, i32)> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    (width > 0 && height > 0).then_some((width, height))
}

// ------------ Running The Loader ------------
// Starts the loader armed for a game, watches how it exits, and cleans up a stray one
// left over from a crash. Only one loader runs at a time.
struct ActiveLoader {
    game_id: String,
    stop: process_utils::NamedEvent,
    process: std::sync::Arc<process_utils::ProcessHandle>,
}

static ACTIVE_LOADER: Mutex<Option<ActiveLoader>> = Mutex::new(None);
static LOADER_SERIAL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static STOP_ALL: std::sync::OnceLock<Option<process_utils::NamedEvent>> = std::sync::OnceLock::new();

fn stop_all_event() -> Option<&'static process_utils::NamedEvent> {
    STOP_ALL
        .get_or_init(|| process_utils::NamedEvent::create(STOP_ALL_EVENT))
        .as_ref()
}

fn loader_log_path(app: &AppHandle) -> PathBuf {
    app.state::<super::state::BackendState>()
        .logs_dir
        .join("mod-loader.log")
}

fn loader_log_tail(log_path: &Path, limit: usize) -> String {
    let Ok(text) = std::fs::read_to_string(log_path) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().rev().take(limit).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join(" | ")
}

const HOOK_ASSUMED: &str = "assumed to have landed";

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn loader_failure_message(display: &str, code: i32, game_running: bool) -> String {
    let reason = capitalized(&describe_exit(code));
    if code == peebify_helpers::mods::EXIT_TARGET_EXITED && game_running {
        format!("{display} restarted itself before mods loaded. Close it and launch it again from Peebify.")
    } else if game_running {
        format!("{display} is running without mods. {reason}")
    } else {
        format!("Mods were not loaded for {display}. {reason}")
    }
}

fn log_loader_finish(game_id: &str, outcome: &str, log_path: &Path) {
    let detail = loader_log_tail(log_path, usize::MAX);
    if detail.contains(HOOK_ASSUMED) {
        log::warn!("xxmi: the mod loader {outcome} for {game_id} without confirming the hook: {detail}");
    } else {
        log::info!("xxmi: the mod loader {outcome} for {game_id}: {detail}");
    }
}

fn announce_tools_busy(app: &AppHandle, game_id: &str, announced: &mut bool) {
    if std::mem::replace(announced, true) {
        return;
    }
    log::info!("xxmi: {game_id} is waiting for the mod tools to finish updating");
    let _ = app.emit("mods-tools-updating", json!({ "gameId": game_id }));
}

pub fn loader_active() -> bool {
    ACTIVE_LOADER
        .lock()
        .as_ref()
        .is_some_and(|a| a.process.exit_code().is_none())
}

fn checksum_arg(integrity: &BTreeMap<String, String>, option: &str, name: &str) -> Vec<String> {
    match integrity
        .get(name)
        .filter(|digest| peebify_helpers::mods::parse_sha256(digest).is_some())
    {
        Some(digest) => vec![option.to_string(), digest.clone()],
        None => {
            log::warn!("xxmi: no checksum is on hand for {name}, so the mod loader will not check it");
            Vec::new()
        }
    }
}

pub async fn prepare_and_spawn(
    app: &AppHandle,
    root: &Path,
    profile: &Value,
    confirm_timeout: Duration,
) -> Result<(), String> {
    let variant = game_profiles::mod_variant(profile)
        .ok_or("This game does not support mods.")?
        .to_string();
    let profile_id = game_profiles::profile_id(profile).to_string();
    let target = game_profiles::client_process_name(profile).to_string();

    let integrity = {
        let mut announced = false;
        let guard = match try_lock_variant(&variant) {
            Some(guard) => Some(guard),
            None => {
                announce_tools_busy(app, &profile_id, &mut announced);
                lock_variant_when_free(&variant, Duration::from_secs(300)).await
            }
        };
        let Some(_guard) = guard else {
            return Err(format!(
                "The mod tools for {variant} are still installing. Try again in a moment."
            ));
        };
        let _update = match xxmi_update::UPDATE_LOCK.try_lock() {
            Ok(update) => update,
            Err(_) => {
                announce_tools_busy(app, &profile_id, &mut announced);
                xxmi_update::UPDATE_LOCK.lock().await
            }
        };
        ensure_variant(app, root, &variant, false, |package, percent| {
            log::debug!("xxmi: {package} {percent:.0}%");
        })
        .await?;
        let verify_root = root.to_path_buf();
        tauri::async_runtime::spawn_blocking(move || xxmi_update::verify_integrity(&verify_root))
            .await
            .map_err(|e| format!("Could not check the mod tools: {e}"))??
    };

    let state = app.try_state::<super::state::BackendState>();
    let overrides = state
        .as_ref()
        .map(|state| stored_overrides(&state.config, &profile_id))
        .unwrap_or_default();
    let reload_key = overrides
        .iter()
        .find(|(s, k, _)| s.eq_ignore_ascii_case("Hunting") && k.eq_ignore_ascii_case("reload_fixes"))
        .map(|(_, _, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_RELOAD_KEY.to_string());

    let dir = variant_dir(root, &variant);
    apply_managed_ini(
        &dir,
        &IniOptions {
            target: target.clone(),
            hunting_mode: HUNTING_KEYBINDS_ONLY,
            reload_key,
            defaults: game_profiles::mod_ini_defaults(profile),
            screen: screen_size(),
        },
        &overrides,
    )?;

    park_rogue_config_files(&mods_dir(root, &variant), ROGUE_SCAN_DEPTH);

    let host = fs_util::resource(app, EXE_NAME).ok_or_else(|| {
        "The mod loader is missing from this Peebify install. Reinstall Peebify to restore it."
            .to_string()
    })?;
    let mode = Mode::parse(game_profiles::mod_loader_mode(profile)).unwrap_or(Mode::Hook);

    displace_active_loader(app, &profile_id).await;

    let serial = LOADER_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let event = format!("Local\\PeebifyModLoader-{}-{serial}", std::process::id());
    let ready = process_utils::NamedEvent::create(&format!(
        "{event}{}",
        peebify_helpers::mods::READY_EVENT_SUFFIX
    ))
    .ok_or("Could not create the mod loader's handshake event.")?;
    let stop = process_utils::NamedEvent::create(&format!(
        "{event}{}",
        peebify_helpers::mods::STOP_EVENT_SUFFIX
    ))
    .ok_or("Could not create the mod loader's stop event.")?;

    let log_path = loader_log_path(app);
    let elevated = |path: &Path| {
        process_utils::resolve_subst(path)
            .to_string_lossy()
            .to_string()
    };
    let mut args = vec!["--dll".to_string(), elevated(&root.join(LOADER_DLL))];
    args.extend(checksum_arg(&integrity, "--dll-sha256", LOADER_DLL));
    args.extend(["--module".to_string(), elevated(&dir.join("d3d11.dll"))]);
    args.extend(checksum_arg(&integrity, "--module-sha256", "d3d11.dll"));
    args.extend([
        "--target".to_string(),
        target.clone(),
        "--mode".to_string(),
        mode.as_str().to_string(),
        "--timeout".to_string(),
        confirm_timeout.as_secs().to_string(),
        "--event".to_string(),
        event.clone(),
        "--log".to_string(),
        elevated(&log_path),
    ]);
    let versions = installed_versions(root);
    let version_of = |key: &str| versions.get(key).map(String::as_str).unwrap_or("unknown");
    log::info!(
        "Starting mod loader for {variant} (mode={}, target={target}, timeout={}s, {CORE_KEY}={}, {variant}={}, root={})",
        mode.as_str(),
        confirm_timeout.as_secs(),
        version_of(CORE_KEY),
        version_of(&variant),
        root.display()
    );

    if let Some(stop_all) = stop_all_event() {
        stop_all.reset();
    }
    let process = {
        let (host, args, root) = (host.clone(), args.clone(), root.to_path_buf());
        tauri::async_runtime::spawn_blocking(move || {
            process_utils::spawn_tool_elevated_handle(&host, &args, &root)
        })
        .await
        .map_err(|e| format!("The mod loader could not be started: {e}"))?
    }
    .map_err(|e| {
        if e.declined {
            "You declined the Windows prompt, so the game is starting without mods.".to_string()
        } else {
            format!("The mod loader could not be started: {}", e.message)
        }
    })?;
    let process = std::sync::Arc::new(process);

    let outcome = {
        let ready = ready.clone();
        let process = process.clone();
        tauri::async_runtime::spawn_blocking(move || {
            process_utils::wait_ready_or_exit(&ready, &process, READY_TIMEOUT)
        })
        .await
        .map_err(|e| format!("The mod loader wait failed: {e}"))?
    };

    match outcome {
        process_utils::ReadyOutcome::Ready => {}
        process_utils::ReadyOutcome::Exited(code) => {
            let reason = describe_exit(code as i32);
            log::error!(
                "xxmi: the mod loader exited early for {profile_id}: {reason} {}",
                loader_log_tail(&log_path, 4)
            );
            return Err(format!("The mod loader stopped before the game started: {reason}"));
        }
        process_utils::ReadyOutcome::TimedOut => {
            stop.set();
            return Err(
                "The mod loader started but never reported ready. Security software may be \
                 scanning it. Try launching again."
                    .to_string(),
            );
        }
    }

    *ACTIVE_LOADER.lock() = Some(ActiveLoader {
        game_id: profile_id.clone(),
        stop,
        process: process.clone(),
    });
    watch_loader(app.clone(), profile_id, log_path, process, confirm_timeout);
    log::info!("{EXE_NAME} is armed for {target} — starting the game");
    Ok(())
}

fn watch_loader(
    app: AppHandle,
    game_id: String,
    log_path: PathBuf,
    process: std::sync::Arc<process_utils::ProcessHandle>,
    confirm_timeout: Duration,
) {
    tauri::async_runtime::spawn(async move {
        let waited = process.clone();
        let code = tauri::async_runtime::spawn_blocking(move || {
            waited.wait(confirm_timeout + WATCH_SLACK)
        })
        .await
        .ok()
        .flatten();

        {
            let mut active = ACTIVE_LOADER.lock();
            if active
                .as_ref()
                .is_some_and(|a| std::sync::Arc::ptr_eq(&a.process, &process))
            {
                *active = None;
            }
        }

        match code.map(|c| c as i32) {
            Some(EXIT_OK) => log_loader_finish(&game_id, "finished", &log_path),
            Some(EXIT_STOPPED) => log_loader_finish(&game_id, "was stopped", &log_path),
            Some(code) => {
                let error = capitalized(&describe_exit(code));
                let detail = loader_log_tail(&log_path, 4);
                log::error!("xxmi: mod loader failed for {game_id}: {error} {detail}");
                let display = game_profiles::display_name(game_profiles::profile(&game_id));
                let game_running = app
                    .try_state::<super::state::BackendState>()
                    .is_some_and(|state| state.game.is_game_running_id(&game_id));
                let message = loader_failure_message(display, code, game_running);
                super::notify::notify_if_backgrounded(&app, "Mods didn't load", &message);
                let _ = app.emit(
                    "mods-load-failed",
                    json!({ "gameId": game_id, "error": error, "message": message }),
                );
            }
            None => log::warn!("xxmi: the mod loader for {game_id} is still running after its window"),
        }
    });
}

async fn stop_active_loader() {
    let active = ACTIVE_LOADER.lock().take();
    let Some(active) = active else {
        return;
    };
    if active.process.exit_code().is_some() {
        return;
    }
    active.stop.set();
    let process = active.process.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || process.wait(STOP_GRACE)).await;
}

async fn displace_active_loader(app: &AppHandle, game_id: &str) {
    let displaced = ACTIVE_LOADER
        .lock()
        .as_ref()
        .filter(|a| a.game_id != game_id && a.process.exit_code().is_none())
        .map(|a| a.game_id.clone());
    stop_active_loader().await;
    let Some(other) = displaced else {
        return;
    };
    let display = game_profiles::display_name(game_profiles::profile(&other));
    let error = "Another game with mods was launched before it finished starting.".to_string();
    log::warn!("xxmi: stopped the mod loader for {other} because {game_id} launched with mods");
    let message = format!("{display} will run without mods. {error}");
    super::notify::notify_if_backgrounded(app, "Mods didn't load", &message);
    let _ = app.emit(
        "mods-load-failed",
        json!({ "gameId": other, "error": error, "message": message }),
    );
}

pub async fn kill_orphan_loader() {
    stop_active_loader().await;
    if !process_utils::is_process_running(EXE_NAME).await {
        return;
    }
    let Some(stop_all) = stop_all_event() else {
        log::warn!("An orphaned {EXE_NAME} is still running. It exits on its own once its wait runs out.");
        return;
    };
    stop_all.set();
    let deadline = tokio::time::Instant::now() + ORPHAN_GRACE;
    let mut running = true;
    while running && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        running = process_utils::is_process_running(EXE_NAME).await;
    }
    stop_all.reset();
    if running {
        log::warn!("An orphaned {EXE_NAME} did not stop when asked. It exits on its own once its wait runs out.");
    } else {
        log::info!("Stopped an orphaned {EXE_NAME}");
    }
}

// ------------ Tests ------------
// Runs the install, uninstall and ini rewriting logic against scratch folders.
#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peebify-xxmi-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("zzmi")).unwrap();
        std::fs::write(dir.join("zzmi").join("d3dx.ini"), "[Loader]\n").unwrap();
        dir
    }

    fn install_core(root: &Path) {
        for name in CORE_FILES {
            std::fs::write(root.join(name), b"MZ").unwrap();
        }
    }

    #[test]
    fn the_core_needs_all_three_libraries() {
        let root = scratch("partial");
        std::fs::write(root.join("d3d11.dll"), b"MZ").unwrap();
        std::fs::write(root.join("d3dcompiler_47.dll"), b"MZ").unwrap();
        assert!(!variant_installed(&root, "zzmi"));
        std::fs::write(root.join(LOADER_DLL), b"MZ").unwrap();
        assert!(variant_installed(&root, "zzmi"));
        assert!(!variant_installed(&root, "wwmi"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_loader_at_all_is_not_installed() {
        let root = scratch("none");
        assert!(!variant_installed(&root, "zzmi"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_launch_key_is_commented_out_rather_than_left_empty() {
        let original = "[Loader]\nloader = 3dmloader.exe\nlaunch = Game.exe\nrequire_admin = 1\n";
        let managed = vec![
            (
                "Loader".to_string(),
                "loader".to_string(),
                IniEntry::Set(EXE_NAME.to_string()),
            ),
            ("Loader".to_string(), "launch".to_string(), IniEntry::Unset),
        ];
        let out = rewrite_ini(original, &managed);
        assert!(out.contains(";launch = Game.exe"), "{out}");
        assert!(!out.lines().any(|l| l.trim() == "launch ="), "{out}");
        assert!(out.contains(&format!("loader = {EXE_NAME}")), "{out}");
    }

    #[test]
    fn exclude_recursive_keeps_the_other_patterns() {
        let original = "[Include]\ninclude_recursive = Mods\nexclude_recursive = desktop.ini\n\n[Hunting]\nhunting = 0\n";
        let managed = vec![(
            "Include".to_string(),
            "exclude_recursive".to_string(),
            IniEntry::EnsureLine("DISABLED*".to_string()),
        )];
        let out = rewrite_ini(original, &managed);
        assert!(out.contains("exclude_recursive = desktop.ini"), "{out}");
        assert!(out.contains("exclude_recursive = DISABLED*"), "{out}");
        let again = rewrite_ini(&out, &managed);
        assert_eq!(again, out);
        assert_eq!(out.matches("DISABLED*").count(), 1);
    }

    #[test]
    fn entries_with_line_breaks_or_brackets_are_never_written() {
        let original = "[Hunting]\nhunting = 0\n";
        let managed = vec![
            (
                "Hunting".to_string(),
                "reload_fixes".to_string(),
                IniEntry::Set("no_modifiers VK_F10\r\n[System]\nproxy_d3d11 = x.dll".to_string()),
            ),
            (
                "System]\n[Loader".to_string(),
                "launch".to_string(),
                IniEntry::Set("x.exe".to_string()),
            ),
            (
                "Hunting".to_string(),
                "hunting".to_string(),
                IniEntry::Set("2".to_string()),
            ),
        ];
        let out = rewrite_ini(original, &managed);
        assert_eq!(out, "[Hunting]\nhunting = 2\n");
    }

    #[test]
    fn defaults_lose_to_user_overrides_and_managed_keys() {
        let dir = scratch("ini");
        let variant = dir.join("zzmi");
        std::fs::write(
            variant.join("d3dx.ini"),
            "[Loader]\ntarget = Game.exe\nloader = XXMI Launcher.exe\nrequire_admin = true\n\n[Rendering]\ntexture_hash = 0\n\n[Logging]\nshow_warnings = 1\n",
        )
        .unwrap();
        apply_managed_ini(
            &variant,
            &IniOptions {
                target: "Real.exe".into(),
                hunting_mode: HUNTING_KEYBINDS_ONLY,
                reload_key: DEFAULT_RELOAD_KEY.into(),
                defaults: vec![
                    ("Rendering".into(), "texture_hash".into(), "1".into()),
                    ("Logging".into(), "show_warnings".into(), "0".into()),
                    ("Loader".into(), "target".into(), "Ignored.exe".into()),
                ],
                screen: Some((2560, 1440)),
            },
            &[("Logging".into(), "show_warnings".into(), "1".into())],
        )
        .unwrap();
        let out = std::fs::read_to_string(variant.join("d3dx.ini")).unwrap();
        assert!(out.contains("target = Real.exe"), "{out}");
        assert!(out.contains("texture_hash = 1"), "{out}");
        assert!(out.contains("show_warnings = 1"), "{out}");
        assert!(out.contains("screen_width = 2560"), "{out}");
        assert!(out.contains(&format!("loader = {EXE_NAME}")), "{out}");
        assert!(out.contains("require_admin = true"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn install_zzmi(root: &Path) {
        install_core(root);
        record_version(root, CORE_KEY, "v1").unwrap();
        record_version(root, "zzmi", "v1").unwrap();
        record_owned(root, CORE_KEY, &CORE_FILES.map(String::from)).unwrap();
        record_owned(root, "zzmi", &["d3dx.ini".to_string(), "ShaderFixes".to_string(), "Mods".to_string()]).unwrap();
        std::fs::create_dir_all(root.join("zzmi").join("ShaderFixes")).unwrap();
        std::fs::write(root.join("zzmi").join("ShaderFixes").join("a.txt"), "x").unwrap();
        std::fs::write(root.join("zzmi").join("d3d11.dll"), b"MZ").unwrap();
        std::fs::write(root.join(LOADER_LOG), "log").unwrap();
    }

    #[test]
    fn uninstalling_a_picked_folder_leaves_foreign_files_alone() {
        let root = scratch("foreign");
        install_zzmi(&root);
        std::fs::create_dir_all(root.join("Game").join("Mods")).unwrap();
        std::fs::write(root.join("Game").join("Mods").join("keep.txt"), "x").unwrap();
        std::fs::write(root.join("Game").join("data.pak"), "x").unwrap();
        std::fs::write(root.join("notes.txt"), "x").unwrap();
        std::fs::create_dir_all(root.join("GIMI")).unwrap();
        std::fs::write(root.join("GIMI").join("d3dx.ini"), "[Loader]").unwrap();

        let result = uninstall_variant(&root, "zzmi", false).unwrap();
        assert!(result.leftovers.is_empty(), "{:?}", result.leftovers);
        assert!(result.kept.is_empty());
        assert!(!root.join("zzmi").exists());
        for name in CORE_FILES.iter().chain([VERSION_FILE, OWNED_FILE, LOADER_LOG].iter()) {
            assert!(!root.join(name).exists(), "{name}");
        }
        assert!(root.join("notes.txt").exists());
        assert!(root.join("Game").join("data.pak").exists());
        assert!(root.join("Game").join("Mods").join("keep.txt").exists());
        assert!(root.join("GIMI").join("d3dx.ini").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_full_uninstall_of_a_picked_folder_leaves_foreign_files_alone() {
        let root = scratch("foreign-full");
        install_zzmi(&root);
        std::fs::create_dir_all(root.join("Game")).unwrap();
        std::fs::write(root.join("Game").join("data.pak"), "x").unwrap();
        std::fs::write(root.join("notes.txt"), "x").unwrap();
        let result = uninstall(&root, false).unwrap();
        assert!(result.leftovers.is_empty(), "{:?}", result.leftovers);
        assert!(!root.join("zzmi").exists());
        assert!(!root.join(LOADER_DLL).exists());
        assert!(root.join("notes.txt").exists());
        assert!(root.join("Game").join("data.pak").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn kept_mods_keep_their_saved_settings() {
        let root = scratch("keep");
        install_zzmi(&root);
        let variant = root.join("zzmi");
        std::fs::create_dir_all(variant.join("Mods").join("Cool Mod")).unwrap();
        std::fs::write(variant.join("d3dx_user.ini"), "$x = 1").unwrap();
        std::fs::write(variant.join("mine.txt"), "x").unwrap();
        let result = uninstall_variant(&root, "zzmi", false).unwrap();
        assert_eq!(result.kept, vec!["zzmi".to_string()]);
        assert!(variant.join("Mods").join("Cool Mod").exists());
        assert!(variant.join("d3dx_user.ini").exists());
        assert!(variant.join("mine.txt").exists());
        assert!(!variant.join("d3dx.ini").exists());
        assert!(!variant.join("ShaderFixes").exists());
        assert!(!variant.join("d3d11.dll").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn peebifys_own_folder_is_cleared_completely() {
        let root = scratch("owned");
        install_core(&root);
        record_version(&root, "zzmi", "v1").unwrap();
        std::fs::write(root.join("extra.dll"), b"MZ").unwrap();
        std::fs::write(root.join("zzmi").join("mine.txt"), "x").unwrap();
        let result = uninstall_variant(&root, "zzmi", true).unwrap();
        assert!(result.leftovers.is_empty(), "{:?}", result.leftovers);
        let empty = || std::fs::read_dir(&root).unwrap().next().is_none();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_peebify_never_installed_into_is_not_touched() {
        let root = scratch("untouched");
        std::fs::write(root.join("zzmi").join("mine.txt"), "x").unwrap();
        let result = uninstall(&root, false).unwrap();
        assert!(result.kept.is_empty());
        assert!(root.join("zzmi").join("d3dx.ini").exists());
        assert!(root.join("zzmi").join("mine.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn game_dir_with_migoto(root: &Path, name: &str, ini: &str) -> PathBuf {
        let game = root.join(name);
        std::fs::create_dir_all(game.join("ShaderFixes")).unwrap();
        std::fs::write(game.join("d3d11.dll"), b"MZ").unwrap();
        std::fs::write(game.join("d3dcompiler_47.dll"), b"MZ").unwrap();
        std::fs::write(game.join("d3dx.ini"), ini).unwrap();
        std::fs::write(game.join("Game.exe"), b"MZ").unwrap();
        game
    }

    #[test]
    fn the_game_sweep_leaves_other_tools_d3d11_alone() {
        let root = scratch("sweep-foreign");
        let dxvk = root.join("dxvk");
        std::fs::create_dir_all(dxvk.join("ShaderCache")).unwrap();
        std::fs::write(dxvk.join("d3d11.dll"), b"MZ").unwrap();
        std::fs::write(dxvk.join("dxgi.dll"), b"MZ").unwrap();
        std::fs::write(dxvk.join("d3dcompiler_47.dll"), b"MZ").unwrap();
        let manual = game_dir_with_migoto(&root, "manual", "[Loader]\nloader = 3DMigoto Loader.exe\n");
        let xxmi_libs = game_dir_with_migoto(&root, "xxmi-libs", "[Loader]\nloader = XXMI Launcher.exe\n");
        std::fs::write(xxmi_libs.join(LOADER_DLL), b"MZ").unwrap();
        let in_peebify_games = game_dir_with_migoto(
            &root,
            "in-peebify-games",
            "[Loader]\nlaunch = C:\\Users\\X\\Peebify Games\\Game\\Game.exe\ntarget = \"C:\\Users\\X\\Peebify Games\\Game\\Game.exe\"\n",
        );

        let removed = sweep_game_dirs(&[
            dxvk.clone(),
            manual.clone(),
            xxmi_libs.clone(),
            in_peebify_games.clone(),
        ]);
        assert_eq!(removed, vec![xxmi_libs.join(LOADER_DLL).display().to_string()]);
        for name in ["d3d11.dll", "dxgi.dll", "d3dcompiler_47.dll", "ShaderCache"] {
            assert!(dxvk.join(name).exists(), "{name}");
        }
        for dir in [&manual, &xxmi_libs, &in_peebify_games] {
            for name in ["d3d11.dll", "d3dcompiler_47.dll", "d3dx.ini", "ShaderFixes"] {
                assert!(dir.join(name).exists(), "{name}");
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_game_sweep_removes_a_setup_peebify_placed() {
        let root = scratch("sweep-owned");
        let by_ini = game_dir_with_migoto(
            &root,
            "by-ini",
            "[Loader]\nloader = C:\\Old\\3dmloader.exe\n[System]\nadditional_foreground_window = Peebify Overlay\n",
        );
        let by_legacy = game_dir_with_migoto(&root, "by-legacy", "[Loader]\nloader = x.exe\n");
        std::fs::write(by_legacy.join("Peebify Mod Loader.exe"), b"MZ").unwrap();
        let loose = root.join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        std::fs::write(loose.join("3dmloader.exe"), b"MZ").unwrap();
        std::fs::write(loose.join("d3d11.dll"), b"MZ").unwrap();

        let removed = sweep_game_dirs(&[by_ini.clone(), by_legacy.clone(), loose.clone()]);
        assert_eq!(removed.len(), 4 + 5 + 1, "{removed:?}");
        for dir in [&by_ini, &by_legacy] {
            for name in ["d3d11.dll", "d3dcompiler_47.dll", "d3dx.ini", "ShaderFixes"] {
                assert!(!dir.join(name).exists(), "{name}");
            }
            assert!(dir.join("Game.exe").exists());
        }
        assert!(!by_legacy.join("Peebify Mod Loader.exe").exists());
        assert!(!loose.join("3dmloader.exe").exists());
        assert!(loose.join("d3d11.dll").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_game_sweep_keeps_its_proof_while_a_file_is_stuck() {
        let root = scratch("sweep-stuck");
        let game = game_dir_with_migoto(&root, "game", "[Loader]\nloader = x.exe\n");
        std::fs::write(game.join("3dmloader.exe"), b"MZ").unwrap();
        std::fs::remove_file(game.join("d3d11.dll")).unwrap();
        std::fs::create_dir_all(game.join("d3d11.dll").join("held")).unwrap();

        sweep_game_dirs(std::slice::from_ref(&game));
        assert!(game.join("d3dx.ini").is_file());
        assert!(game.join("3dmloader.exe").is_file());
        assert!(!game.join("d3dcompiler_47.dll").exists());
        assert!(!game.join("ShaderFixes").exists());

        std::fs::remove_dir_all(game.join("d3d11.dll")).unwrap();
        std::fs::write(game.join("d3d11.dll"), b"MZ").unwrap();
        sweep_game_dirs(std::slice::from_ref(&game));
        for name in ["d3d11.dll", "d3dx.ini", "3dmloader.exe"] {
            assert!(!game.join(name).exists(), "{name}");
        }
        assert!(game.join("Game.exe").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn updates_merge_into_shaderfixes_and_report_what_they_wrote() {
        let root = scratch("merge");
        let source = root.join("pkg");
        std::fs::create_dir_all(source.join("ShaderFixes")).unwrap();
        std::fs::create_dir_all(source.join("Mods")).unwrap();
        std::fs::write(source.join("ShaderFixes").join("fix.ini"), "new").unwrap();
        std::fs::write(source.join("d3dx.ini"), "[Loader]").unwrap();
        let destination = root.join("zzmi");
        std::fs::create_dir_all(destination.join("ShaderFixes")).unwrap();
        std::fs::create_dir_all(destination.join("Mods")).unwrap();
        std::fs::write(destination.join("ShaderFixes").join("fix.ini"), "old").unwrap();
        std::fs::write(destination.join("ShaderFixes").join("dump.txt"), "mine").unwrap();
        let mut written = merge_dir(&source, &destination).unwrap();
        written.sort();
        assert_eq!(written, vec!["ShaderFixes".to_string(), "d3dx.ini".to_string()]);
        let fix = std::fs::read_to_string(destination.join("ShaderFixes").join("fix.ini")).unwrap();
        assert_eq!(fix, "new");
        assert!(destination.join("ShaderFixes").join("dump.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn update_commands_follow_xxmi_path_rules() {
        let dir = Path::new("X");
        assert_eq!(
            xcmd_delete_target(dir, "Core\\GIMI\\old.ini").unwrap(),
            dir.join("Core").join("GIMI").join("old.ini")
        );
        assert_eq!(
            xcmd_delete_target(dir, "../ShaderFixes/./x.txt").unwrap(),
            dir.join("ShaderFixes").join("x.txt")
        );
        assert!(xcmd_delete_target(dir, "Core").is_err());
        assert!(xcmd_delete_target(dir, "Mods/a").is_err());
        assert!(xcmd_delete_target(dir, "C:\\Windows\\x").is_err());
        assert!(xcmd_delete_target(dir, "").is_err());
        let text = "[PreInstall]\ndelete = Core/a.ini\n; delete = Core/b.ini\n[PostInstall]\ndelete = ShaderFixes/c.ini\nrun = x\n";
        assert_eq!(
            xcmd_commands(text, "PreInstall"),
            vec![("delete".to_string(), "Core/a.ini".to_string())]
        );
        assert_eq!(xcmd_commands(text, "postinstall").len(), 2);
    }

    #[test]
    fn update_commands_delete_inside_the_package_only() {
        let root = scratch("xcmd");
        let variant = root.join("zzmi");
        std::fs::create_dir_all(variant.join("Core").join("Old")).unwrap();
        std::fs::write(variant.join("Core").join("Old").join("a.ini"), "x").unwrap();
        std::fs::write(variant.join("Core").join("keep.ini"), "x").unwrap();
        std::fs::write(
            xcmd_path(&variant),
            "[PostInstall]\ndelete = Core\\Old\ndelete = Core\ndelete = ..\\..\\zzmi\\d3dx.ini\n",
        )
        .unwrap();
        assert_eq!(run_update_commands(&xcmd_path(&variant), "PostInstall", &variant), 1);
        assert!(!variant.join("Core").join("Old").exists());
        assert!(variant.join("Core").join("keep.ini").exists());
        assert!(variant.join("d3dx.ini").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stray_configs_inside_mods_are_parked() {
        let dir = scratch("rogue");
        let mods = dir.join("zzmi").join("Mods");
        std::fs::create_dir_all(mods.join("Cool Mod").join("sub")).unwrap();
        std::fs::write(mods.join("Cool Mod").join("mod.ini"), "[TextureOverride]").unwrap();
        std::fs::write(mods.join("Cool Mod").join("sub").join("d3dx.ini"), "[Loader]").unwrap();
        std::fs::create_dir_all(mods.join("DISABLED Old")).unwrap();
        std::fs::write(mods.join("DISABLED Old").join("d3dx.ini"), "[Loader]").unwrap();
        assert_eq!(park_rogue_config_files(&mods, ROGUE_SCAN_DEPTH), 1);
        assert!(mods.join("Cool Mod").join("sub").join("DISABLED d3dx.ini").exists());
        assert!(mods.join("Cool Mod").join("mod.ini").exists());
        assert!(mods.join("DISABLED Old").join("d3dx.ini").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_variant_copy_with_the_same_size_and_time_is_still_replaced() {
        let dir = scratch("relink");
        install_core(&dir);
        link_core_libs(&dir, "zzmi", false).unwrap();
        let copy = dir.join("zzmi").join("d3d11.dll");
        let modified = std::fs::metadata(&copy).unwrap().modified().unwrap();
        std::fs::write(&copy, b"XX").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&copy)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        link_core_libs(&dir, "zzmi", false).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"MZ");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_loader_is_handed_the_checksums_that_were_verified() {
        use peebify_helpers::mods::{parse_options, sha256_hex};
        let dir = scratch("loader-checksums");
        for name in CORE_FILES {
            std::fs::write(dir.join(name), name.as_bytes()).unwrap();
        }
        std::fs::write(dir.join("zzmi").join("d3d11.dll"), b"stale").unwrap();
        let integrity = xxmi_update::verify_integrity(&dir).unwrap();
        link_core_libs(&dir, "zzmi", false).unwrap();
        let module = dir.join("zzmi").join("d3d11.dll");
        let mut args = vec!["--dll".to_string(), dir.join(LOADER_DLL).display().to_string()];
        args.extend(checksum_arg(&integrity, "--dll-sha256", LOADER_DLL));
        args.extend(["--module".to_string(), module.display().to_string()]);
        args.extend(checksum_arg(&integrity, "--module-sha256", "d3d11.dll"));
        args.extend(
            ["--target", "ZenlessZoneZero.exe", "--mode", "hook", "--timeout", "30", "--event", "Local\\E"]
                .map(String::from),
        );
        let options = parse_options(&args).unwrap();
        let loader = xxmi_update::file_sha256(&dir.join(LOADER_DLL)).unwrap();
        let linked = xxmi_update::file_sha256(&module).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(sha256_hex(&options.dll_sha256.unwrap()), loader);
        assert_eq!(sha256_hex(&options.module_sha256.unwrap()), linked);
        assert_eq!(args[2], "--dll-sha256");
        assert_eq!(args[6], "--module-sha256");

        let mut partial = integrity.clone();
        partial.remove("d3d11.dll");
        partial.insert(LOADER_DLL.to_string(), "not a digest".to_string());
        assert!(checksum_arg(&partial, "--dll-sha256", LOADER_DLL).is_empty());
        assert!(checksum_arg(&partial, "--module-sha256", "d3d11.dll").is_empty());
    }

    #[test]
    fn loader_failures_read_as_one_capitalised_sentence() {
        use peebify_helpers::mods::{EXIT_NOT_MAPPED, EXIT_TARGET_EXITED, EXIT_TARGET_TIMEOUT};
        assert_eq!(capitalized("the game"), "The game");
        assert_eq!(capitalized(""), "");
        assert_eq!(
            loader_failure_message("ZZZ", EXIT_TARGET_EXITED, true),
            "ZZZ restarted itself before mods loaded. Close it and launch it again from Peebify."
        );
        assert_eq!(
            loader_failure_message("ZZZ", EXIT_NOT_MAPPED, true),
            format!("ZZZ is running without mods. {}", capitalized(&describe_exit(EXIT_NOT_MAPPED)))
        );
        let idle = loader_failure_message("ZZZ", EXIT_TARGET_TIMEOUT, false);
        assert!(idle.starts_with("Mods were not loaded for ZZZ. The game never started"));
        assert!(!idle.contains('|'));
    }

    #[test]
    fn the_loader_log_reads_whole_or_as_a_tail() {
        let dir = scratch("loaderlog");
        let log = dir.join("mod-loader.log");
        std::fs::write(&log, "a\nb\nc\nd\ne\n").unwrap();
        assert_eq!(loader_log_tail(&log, 2), "d | e");
        assert_eq!(loader_log_tail(&log, usize::MAX), "a | b | c | d | e");
        assert_eq!(loader_log_tail(&dir.join("missing.log"), 4), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_rate_limits_timeouts_and_server_errors_are_retried() {
        assert!(retryable_download_status(503));
        assert!(retryable_download_status(429));
        assert!(retryable_download_status(408));
        assert!(!retryable_download_status(404));
        assert!(!retryable_download_status(403));
    }
}
