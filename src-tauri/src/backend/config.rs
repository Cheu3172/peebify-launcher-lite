// ------------ Launcher Config ------------
// Owns launcher-config.json in the app data folder: every setting, each game's path and playtime, and the library.
// It fills in defaults, repairs bad or old values, saves with a small delay, and keeps backups so a damaged file can be restored.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde_json::{json, Map, Value};

use super::game_path;
use super::game_profiles::{self, GAME_IDS};

const CONFIG_FILE: &str = "launcher-config.json";
const SAVE_DEBOUNCE_MS: u64 = 100;
const EMPTY_OK: [&str; 1] = ["overlayAudioTracks"];
const READ_ATTEMPTS: u32 = 3;
const READ_RETRY_MS: u64 = 200;
const SAVE_ATTEMPTS: u32 = 3;
const SAVE_RETRY_MS: u64 = 100;

// ------------ Default Settings ------------
// What a fresh config looks like, for the launcher as a whole and for each game.
fn default_playtime_full() -> Value {
    json!({
        "totalPlaytime": 0,
        "dailyPlaytime": {},
        "sessionCount": 0,
        "mostRecentSession": null,
        "sessions": []
    })
}

fn default_playtime_slice() -> Value {
    json!({
        "totalPlaytime": 0,
        "dailyPlaytime": {},
        "sessionCount": 0,
        "mostRecentSession": null
    })
}

fn default_media_slice() -> Value {
    json!({ "type": "default", "path": null })
}

fn per_game_media() -> Value {
    Value::Object(
        GAME_IDS
            .iter()
            .map(|id| (id.to_string(), default_media_slice()))
            .collect(),
    )
}

fn graphics_api_default(profile: &Value) -> Option<&str> {
    profile.get("graphicsApiArgs")?;
    Some(
        profile
            .get("graphicsApiDefault")
            .and_then(Value::as_str)
            .unwrap_or("dx11"),
    )
}

fn default_game(id: &str) -> Value {
    let profile = game_profiles::profile(id);
    let mut game = Map::new();
    game.insert("gamePath".into(), json!(""));
    if let Some(api) = graphics_api_default(profile) {
        game.insert("graphicsApi".into(), json!(api));
    }
    if let Some(quality) = game_profiles::resource_quality_default(profile) {
        game.insert("resourceQuality".into(), json!(quality));
    }
    if profile.get("steamAppId").is_some() {
        game.insert("launchViaSteam".into(), json!(true));
    }
    if game_profiles::mod_config(profile).is_some() {
        game.insert("modsEnabled".into(), json!(false));
    }
    if profile.get("fpsUnlock") == Some(&Value::Bool(true)) {
        game.insert("fpsUnlock".into(), json!(false));
        game.insert("fpsUnlockTarget".into(), json!(120));
        game.insert("fpsUnlockPowerSave".into(), json!(false));
        game.insert("fpsUnlockBackgroundFps".into(), json!(30));
    }
    game.insert("playtime".into(), default_playtime_full());
    Value::Object(game)
}

fn default_config() -> Value {
    let games: Map<String, Value> = GAME_IDS
        .iter()
        .map(|id| (id.to_string(), default_game(id)))
        .collect();
    json!({
        "games": games,
        "window": {
            "width": 1280,
            "height": 720,
            "maximized": false
        },
        "behavior": default_behavior(),
        "wallpaper": per_game_media(),
        "gameIcons": per_game_media(),
        "library": {
            "visible": [],
            "setupComplete": false
        }
    })
}

fn default_behavior() -> Value {
    json!({
        "closeAction": "close",
        "minimizeAction": "minimize",
        "startOnBoot": false,
        "startOnBootAction": "open",
        "launchAction": "minimize",
        "reopenAfterGameClose": true,
        "rememberWindowState": true,
        "hideSocials": false,
        "hideBottomRightButtons": false,
        "hidePlaytime": false,
        "hideNewsPanel": false,
        "disableAnimations": false,
        "osNotifications": true,
        "uiScale": "100",
        "activeGameId": "wuwa",

        "modsEnabled": false,
        "modsPath": "",
        "modsAutoUpdate": true,
        "modsPerPage": "16",
        "showNsfwMods": false,

        "overlayEnabled": false,
        "overlayHotkey": "Alt+P",
        "overlayShotHotkey": "Alt+S",
        "overlayRecHotkey": "Alt+R",
        "overlayCaptureFolder": "",
        "overlayShotFormat": "png",
        "overlayShotQuality": "90",
        "overlayShotToast": true,
        "overlayRecRes": "native",
        "overlayRecFps": "60",
        "overlayRecCodec": "h264",
        "overlayRecQuality": "balanced",
        "overlayAudioTracks": "desktop,mic",
        "overlayAudioDiscord": ""
    })
}

// ------------ Repairing Old Settings ------------
// Brings configs from older versions up to date and replaces anything missing or the wrong type with its default.
fn migrate_library(merged: &mut Value, had_existing_config: bool) {
    if !merged["library"].is_object() {
        merged["library"] = json!({ "visible": [], "setupComplete": false });
    }

    let known: Vec<String> = merged["library"]["visible"]
        .as_array()
        .map(|ids| {
            let mut seen = std::collections::HashSet::new();
            ids.iter()
                .filter_map(Value::as_str)
                .filter(|id| GAME_IDS.contains(id))
                .filter(|id| seen.insert(id.to_string()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let library = merged["library"]
        .as_object_mut()
        .expect("library was just made an object");

    let complete = matches!(library.get("setupComplete"), Some(Value::Bool(true)));
    if !complete && had_existing_config {
        library.insert("visible".into(), json!(GAME_IDS));
        library.insert("setupComplete".into(), json!(true));
        log::info!(
            "[config] Existing install predates the games library — showing every game and skipping the picker."
        );
        return;
    }

    library.insert("visible".into(), json!(known));
    if !matches!(library.get("setupComplete"), Some(Value::Bool(_))) {
        library.insert("setupComplete".into(), json!(false));
    }
}

fn prune_corrupt_backups(user_data: &Path, keep: Option<&Path>) {
    let prefix = format!("{CONFIG_FILE}.corrupt-");
    let keep = keep.and_then(Path::file_name);
    let Ok(entries) = std::fs::read_dir(user_data) else {
        return;
    };
    for entry in entries.flatten() {
        if keep == Some(entry.file_name().as_os_str()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(&prefix) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => log::info!("[config] Removed stale corrupt-config backup {name}"),
            Err(e) => log::warn!("[config] Could not remove {name}: {e}"),
        }
    }
}

fn deep_merge(target: &mut Value, source: &Value) {
    let Some(source_map) = source.as_object() else {
        return;
    };
    let Some(target_map) = target.as_object_mut() else {
        return;
    };
    for (key, sv) in source_map {
        match target_map.get_mut(key) {
            Some(tv) if tv.is_object() && sv.is_object() => deep_merge(tv, sv),
            _ => {
                target_map.insert(key.clone(), sv.clone());
            }
        }
    }
}

pub fn sanitize_behavior(merged: &mut Value) {
    let defaults = default_behavior();
    let defaults_map = defaults
        .as_object()
        .expect("behavior defaults are an object");

    if !merged["behavior"].is_object() {
        merged["behavior"] = defaults.clone();
        return;
    }
    let behavior = merged["behavior"].as_object_mut().expect("checked above");

    for (key, default_value) in defaults_map {
        let value = behavior.get(key);
        let heal = match default_value {
            Value::Bool(_) => !matches!(value, Some(Value::Bool(_))),
            Value::String(_) => !matches!(
                value,
                Some(Value::String(s))
                    if (!s.is_empty() || EMPTY_OK.contains(&key.as_str()))
                        && s != "null"
                        && s != "undefined"
            ),
            Value::Number(_) => !matches!(value, Some(Value::Number(_))),
            _ => matches!(value, None | Some(Value::Null)),
        };
        if heal {
            behavior.insert(key.clone(), default_value.clone());
        }
    }

    if behavior
        .get("activeGameId")
        .and_then(Value::as_str)
        .is_some_and(|id| !game_profiles::is_known_game_id(id))
    {
        behavior.insert(
            "activeGameId".to_string(),
            json!(game_profiles::DEFAULT_GAME_ID),
        );
    }
}

fn heal_games(merged: &mut Value) {
    if !merged["games"].is_object() {
        merged["games"] = json!({});
    }
    for id in GAME_IDS {
        if !merged["games"][id].is_object() {
            merged["games"][id] = json!({ "gamePath": "" });
        }
        if !merged["games"][id]["playtime"].is_object() {
            merged["games"][id]["playtime"] = default_playtime_slice();
        }
    }
}

fn heal_graphics_api(merged: &mut Value) {
    const VALID: [&str; 2] = ["dx11", "dx12"];

    for id in GAME_IDS {
        let Some(game) = merged["games"].get_mut(id).and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(default) = graphics_api_default(game_profiles::profile(id)) else {
            game.remove("graphicsApi");
            continue;
        };
        let current = game.get("graphicsApi").and_then(Value::as_str);
        if !current.is_some_and(|v| VALID.contains(&v)) {
            game.insert("graphicsApi".into(), json!(default));
        }
    }
}

fn heal_resource_quality(merged: &mut Value) {
    for id in GAME_IDS {
        let Some(game) = merged["games"].get_mut(id).and_then(Value::as_object_mut) else {
            continue;
        };
        let profile = game_profiles::profile(id);
        let Some(default) = game_profiles::resource_quality_default(profile) else {
            game.remove("resourceQuality");
            continue;
        };
        let current = game.get("resourceQuality").and_then(Value::as_str);
        if !current.is_some_and(|v| profile["resourceQualityArgs"].get(v).is_some()) {
            game.insert("resourceQuality".into(), json!(default));
        }
    }
}

fn heal_mods(merged: &mut Value) {
    for id in GAME_IDS {
        let Some(game) = merged["games"].get_mut(id).and_then(Value::as_object_mut) else {
            continue;
        };
        if game_profiles::mod_config(game_profiles::profile(id)).is_none() {
            game.remove("modsEnabled");
            continue;
        }
        if !matches!(game.get("modsEnabled"), Some(Value::Bool(_))) {
            game.insert("modsEnabled".into(), json!(false));
        }
    }
}

fn heal_media(merged: &mut Value, section: &str) {
    if !merged[section].is_object() {
        merged[section] = per_game_media();
        return;
    }
    for id in GAME_IDS {
        heal_media_slice(&mut merged[section], id);
    }
}

fn heal_media_slice(section: &mut Value, id: &str) {
    if !section[id].is_object() {
        section[id] = default_media_slice();
        return;
    }
    let slice = section[id].as_object_mut().expect("checked above");
    let type_ok = matches!(slice.get("type"), Some(Value::String(s)) if !s.is_empty());
    if !type_ok {
        slice.insert("type".to_string(), json!("default"));
    }
    if !slice.contains_key("path") {
        slice.insert("path".to_string(), Value::Null);
    }
}

// ------------ Safe Reading and Writing ------------
// Reads the file with retries, sets a broken one aside, restores the newest good copy, and writes through temp files so a crash cannot leave half a config.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.{suffix}", path.display()))
}

fn read_with_retry(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut attempt = 1;
    loop {
        match std::fs::read(path) {
            Err(e)
                if attempt < READ_ATTEMPTS && matches!(e.raw_os_error(), Some(32) | Some(33)) =>
            {
                log::warn!(
                    "[config] {} is held by another program ({e}), retry {attempt}/{READ_ATTEMPTS}",
                    path.display()
                );
                std::thread::sleep(std::time::Duration::from_millis(READ_RETRY_MS));
                attempt += 1;
            }
            other => return other,
        }
    }
}

fn parse_config_bytes(bytes: &[u8]) -> Result<Value, String> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match serde_json::from_slice::<Value>(bytes) {
        Ok(v) if v.is_object() => Ok(v),
        Ok(_) => Err("it does not hold a settings object".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

fn parse_config_copy(path: &Path) -> Option<Value> {
    parse_config_bytes(&std::fs::read(path).ok()?).ok()
}

fn newest_good_copy(path: &Path) -> Option<(&'static str, Value)> {
    ["tmp", "bak"]
        .into_iter()
        .filter_map(|suffix| {
            let copy = sibling(path, suffix);
            let value = parse_config_copy(&copy)?;
            let modified = std::fs::metadata(&copy).and_then(|m| m.modified()).ok();
            Some((modified, suffix, value))
        })
        .max_by_key(|(modified, _, _)| *modified)
        .map(|(_, suffix, value)| (suffix, value))
}

fn set_aside(path: &Path, read: &[u8]) -> std::io::Result<Option<PathBuf>> {
    let aside = sibling(
        path,
        &format!("corrupt-{}", chrono::Utc::now().timestamp_millis()),
    );
    let kept = match std::fs::rename(path, &aside) {
        Ok(()) => aside,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => match write_synced(&aside, read) {
            Ok(()) => aside,
            Err(_) => return Err(e),
        },
    };
    if let Some(dir) = path.parent() {
        prune_corrupt_backups(dir, Some(&kept));
    }
    Ok(Some(kept))
}

fn keep_last_good_copy(path: &Path, bytes: Vec<u8>) -> Option<std::thread::JoinHandle<()>> {
    let bak = sibling(path, "bak");
    let spawned = std::thread::Builder::new()
        .name("config-backup".into())
        .spawn(move || {
            if std::fs::read(&bak).is_ok_and(|held| held == bytes) {
                let touched = std::fs::File::options()
                    .write(true)
                    .open(&bak)
                    .and_then(|f| f.set_modified(std::time::SystemTime::now()));
                if let Err(e) = touched {
                    log::warn!("[config] Could not refresh the last good copy of the config: {e}");
                }
                return;
            }
            let tmp = sibling(&bak, "tmp");
            let result = write_synced(&tmp, &bytes).and_then(|()| rename_with_retry(&tmp, &bak));
            if let Err(e) = result {
                let _ = std::fs::remove_file(&tmp);
                log::warn!("[config] Could not keep a last good copy of the config: {e}");
            }
        });
    match spawned {
        Ok(handle) => Some(handle),
        Err(e) => {
            log::warn!("[config] Could not start keeping a last good copy of the config: {e}");
            None
        }
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn retry_while_held(mut op: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let mut attempt = 1;
    loop {
        match op() {
            Err(e) if attempt < SAVE_ATTEMPTS && matches!(e.raw_os_error(), Some(5) | Some(32)) => {
                std::thread::sleep(std::time::Duration::from_millis(
                    SAVE_RETRY_MS * u64::from(attempt),
                ));
                attempt += 1;
            }
            other => return other,
        }
    }
}

fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    retry_while_held(|| std::fs::rename(from, to))
}

fn volume_reachable(path: &str) -> bool {
    match super::file_channels::drive_root(path) {
        Some(root) => std::fs::metadata(&root).is_ok(),
        None => true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadIssue {
    Missing,
    Damaged,
    Unreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Recovery {
    issue: LoadIssue,
    restored: bool,
}

fn kept_note(kept: &std::io::Result<Option<PathBuf>>) -> String {
    match kept {
        Ok(Some(aside)) => format!("backed up to {}", aside.display()),
        Ok(None) => "it is gone".to_string(),
        Err(e) => format!("it could not be moved aside ({e})"),
    }
}

fn restore_newest_copy(path: &Path, problem: &str) -> Option<Value> {
    match newest_good_copy(path) {
        Some((suffix, value)) => {
            log::warn!("[config] {problem}; restored from the .{suffix} copy.");
            Some(value)
        }
        None => {
            log::warn!("[config] {problem}; no usable copy, so defaults are used.");
            None
        }
    }
}

// ------------ Config Store ------------
// The live config the rest of the backend reads and writes by dotted key, like behavior.modsEnabled. Changes are saved in the background.
pub struct LauncherConfig {
    path: PathBuf,
    user_data: PathBuf,
    data: RwLock<Value>,
    dirty: AtomicBool,
    save_pending: AtomicBool,
    read_failed: AtomicBool,
    read_failed_warned: AtomicBool,
    read_failed_kept: AtomicBool,
    save_lock: Mutex<()>,
    recovery: Option<Recovery>,
    backup: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl LauncherConfig {
    pub fn load(user_data: &Path) -> Arc<Self> {
        let path = user_data.join(CONFIG_FILE);
        let _ = std::fs::create_dir_all(user_data);

        let mut existing = Value::Object(Map::new());
        let mut unreadable = false;
        let mut recovery = None;
        let mut has_bom = false;
        let mut backup = None;

        match read_with_retry(&path) {
            Ok(bytes) => match parse_config_bytes(&bytes) {
                Ok(v) => {
                    existing = v;
                    has_bom = bytes.starts_with(b"\xEF\xBB\xBF");
                    log::info!("[config] Existing configuration loaded");
                    backup = keep_last_good_copy(&path, bytes);
                }
                Err(e) => {
                    let kept = set_aside(&path, &bytes);
                    let restored = restore_newest_copy(
                        &path,
                        &format!("Config was corrupt ({e}); {}", kept_note(&kept)),
                    );
                    recovery = Some(Recovery {
                        issue: LoadIssue::Damaged,
                        restored: restored.is_some(),
                    });
                    if let Some(v) = restored {
                        existing = v;
                    }
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match newest_good_copy(&path) {
                Some((suffix, v)) => {
                    existing = v;
                    recovery = Some(Recovery {
                        issue: LoadIssue::Missing,
                        restored: true,
                    });
                    log::warn!(
                        "[config] No config found, but its .{suffix} copy was; restored from that copy."
                    );
                }
                None => log::info!(
                    "[config] No existing config found — starting from defaults (first run)."
                ),
            },
            Err(e) => {
                unreadable = true;
                let restored = restore_newest_copy(
                    &path,
                    &format!(
                        "Failed to read existing config ({e}); it is left alone and changes in this session are not saved"
                    ),
                );
                recovery = Some(Recovery {
                    issue: LoadIssue::Unreadable,
                    restored: restored.is_some(),
                });
                if let Some(v) = restored {
                    existing = v;
                }
            }
        }

        let predates_library = existing.get("library").is_none() && existing.get("games").is_some();
        let mut merged = default_config();
        if existing.is_object() {
            deep_merge(&mut merged, &existing);
        }
        sanitize_behavior(&mut merged);
        migrate_library(&mut merged, predates_library);
        heal_games(&mut merged);
        heal_media(&mut merged, "wallpaper");
        heal_media(&mut merged, "gameIcons");
        heal_graphics_api(&mut merged);
        heal_resource_quality(&mut merged);
        heal_mods(&mut merged);

        let needs_save = recovery.is_some() || has_bom || merged != existing;

        let cfg = Arc::new(Self {
            path,
            user_data: user_data.to_path_buf(),
            data: RwLock::new(merged),
            dirty: AtomicBool::new(false),
            save_pending: AtomicBool::new(false),
            read_failed: AtomicBool::new(unreadable),
            read_failed_warned: AtomicBool::new(false),
            read_failed_kept: AtomicBool::new(false),
            save_lock: Mutex::new(()),
            recovery,
            backup: Mutex::new(backup),
        });

        if !unreadable && needs_save {
            cfg.dirty.store(true, Ordering::SeqCst);
            if let Err(e) = cfg.save_now() {
                log::warn!("[config] Could not save merged config immediately: {e}");
            }
        }

        cfg
    }

    pub fn active_game_id(&self) -> String {
        match self.get("behavior.activeGameId") {
            Value::String(s) if !s.is_empty() => s,
            _ => game_profiles::DEFAULT_GAME_ID.to_string(),
        }
    }

    pub fn startup_notice(&self) -> Option<Value> {
        let recovery = self.recovery?;
        let loaded = if recovery.restored {
            "the last good copy was loaded"
        } else {
            "Peebify started with default settings"
        };
        let save_blocked = self.read_failed.load(Ordering::SeqCst);
        let (kind, title, text) = if save_blocked {
            (
                "warning",
                "Settings can't be saved",
                format!(
                    "Peebify couldn't open its settings file, most likely because another program is using it, so {loaded}. Changes you make now won't be saved. Close that program and restart Peebify."
                ),
            )
        } else {
            let problem = match recovery.issue {
                LoadIssue::Missing => "was missing",
                LoadIssue::Damaged => "was damaged",
                LoadIssue::Unreadable => "couldn't be opened",
            };
            if recovery.restored {
                (
                    "info",
                    "Settings restored from a backup",
                    format!(
                        "Peebify's settings file {problem}, so {loaded}. Changes from your last session may be missing."
                    ),
                )
            } else {
                (
                    "warning",
                    "Settings were reset",
                    format!(
                        "Peebify's settings file {problem} and no backup of it could be used, so your settings were reset."
                    ),
                )
            }
        };
        Some(json!({ "type": kind, "title": title, "text": text, "saveBlocked": save_blocked }))
    }

    pub fn sanitize_installed_paths_deferred(self: &Arc<Self>) -> bool {
        let mut changed = false;
        for id in GAME_IDS {
            let game_path = match self.get(&format!("games.{id}.gamePath")) {
                Value::String(s) if !s.is_empty() => s,
                _ => continue,
            };
            if self.get(&format!("games.{id}.pendingMove")).is_object() {
                log::info!("[config] {id} has an unfinished move, keeping the path for its recovery: {game_path}");
                continue;
            }
            if !volume_reachable(&game_path) {
                log::warn!(
                    "[config] Install drive for {id} is not reachable, keeping the path: {game_path}"
                );
                continue;
            }
            let profile = game_profiles::profile(id);
            let validation = game_path::validate_game_path_for_profile(&game_path, profile);
            if !validation.is_valid {
                log::info!(
                    "[config] Clearing invalid install path for {id} ({}): {game_path}",
                    validation.error.as_deref().unwrap_or("not a valid install")
                );
                self.set(&format!("games.{id}.gamePath"), json!(""));
                changed = true;
            } else if let Some(resolved) = validation.resolved_path {
                if resolved != game_path {
                    self.set(&format!("games.{id}.gamePath"), json!(resolved));
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn get(&self, key: &str) -> Value {
        let data = self.data.read();
        if key.is_empty() {
            return data.clone();
        }
        let mut cur: &Value = &data;
        for k in key.split('.') {
            match cur.get(k) {
                Some(v) => cur = v,
                None => return Value::Null,
            }
        }
        cur.clone()
    }

    pub fn get_for_interface(&self) -> Value {
        let data = self.data.read();
        let Some(map) = data.as_object() else {
            return data.clone();
        };
        let mut out = Map::with_capacity(map.len());
        for (key, value) in map {
            match (key.as_str(), value.as_object()) {
                ("games", Some(games)) => {
                    let games = games
                        .iter()
                        .map(|(id, game)| {
                            let game = match game.as_object() {
                                Some(fields) => Value::Object(
                                    fields
                                        .iter()
                                        .filter(|(field, _)| *field != "playtime")
                                        .map(|(field, v)| (field.clone(), v.clone()))
                                        .collect(),
                                ),
                                None => game.clone(),
                            };
                            (id.clone(), game)
                        })
                        .collect();
                    out.insert(key.clone(), Value::Object(games));
                }
                _ => {
                    out.insert(key.clone(), value.clone());
                }
            }
        }
        Value::Object(out)
    }

    pub fn set(self: &Arc<Self>, key: &str, value: Value) -> bool {
        if key.is_empty() {
            return false;
        }
        {
            let mut data = self.data.write();
            if lookup(&data, key) == Some(&value) {
                return true;
            }
            set_path(&mut data, key, value);
        }
        self.schedule_save();
        true
    }

    pub fn update(self: &Arc<Self>, key: &str, apply: impl FnOnce(&mut Value) -> bool) -> bool {
        if key.is_empty() {
            return false;
        }
        {
            let mut data = self.data.write();
            let mut value = lookup(&data, key).cloned().unwrap_or(Value::Null);
            if !apply(&mut value) {
                return false;
            }
            if lookup(&data, key) == Some(&value) {
                return true;
            }
            set_path(&mut data, key, value);
        }
        self.schedule_save();
        true
    }

    fn schedule_save(self: &Arc<Self>) {
        self.dirty.store(true, Ordering::SeqCst);
        if self.save_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(SAVE_DEBOUNCE_MS)).await;
            me.save_pending.store(false, Ordering::SeqCst);
            match tauri::async_runtime::spawn_blocking(move || me.save_now()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => log::error!("[config] debounced save failed: {e}"),
                Err(e) => log::error!("[config] debounced save did not run: {e}"),
            }
        });
    }

    pub fn flush(&self) {
        if let Err(e) = self.save_now() {
            log::error!("[config] flush failed: {e}");
        }
    }

    fn save_now(&self) -> Result<(), String> {
        let _guard = self.save_lock.lock();
        self.save_locked()
    }

    fn save_locked(&self) -> Result<(), String> {
        if self.read_failed.load(Ordering::SeqCst) {
            self.dirty.store(true, Ordering::SeqCst);
            if !self.unread_config_replaceable() {
                return Ok(());
            }
            self.read_failed.store(false, Ordering::SeqCst);
        }

        if !self.dirty.swap(false, Ordering::SeqCst) {
            return Ok(());
        }

        let text = serde_json::to_string_pretty(&*self.data.read())
            .map_err(|e| format!("serialize failed: {e}"))?;

        let restore_dirty = |e: String| {
            self.dirty.store(true, Ordering::SeqCst);
            e
        };

        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = sibling(&self.path, "tmp");
        retry_while_held(|| write_synced(&tmp, text.as_bytes()))
            .map_err(|e| restore_dirty(format!("write failed for {}: {e}", tmp.display())))?;
        rename_with_retry(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            restore_dirty(format!("rename failed for {}: {e}", self.path.display()))
        })?;
        Ok(())
    }

    fn unread_config_replaceable(&self) -> bool {
        if self.read_failed_kept.load(Ordering::SeqCst) {
            return false;
        }
        let reason = if !self.recovery.is_some_and(|r| r.restored) {
            "this session runs on defaults".to_string()
        } else {
            match std::fs::read(&self.path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    log::info!(
                        "[config] The config that could not be read at startup is gone, so settings are saved again."
                    );
                    return true;
                }
                Err(e) => format!("it still can't be read ({e})"),
                Ok(bytes) => match parse_config_bytes(&bytes) {
                    Ok(_) => {
                        self.read_failed_kept.store(true, Ordering::SeqCst);
                        "it can be read now, so it is kept as it is".to_string()
                    }
                    Err(problem) => match set_aside(&self.path, &bytes) {
                        Ok(kept) => {
                            log::warn!(
                                "[config] The config that could not be read at startup is damaged ({problem}); {}, so settings are saved again.",
                                kept_note(&Ok(kept))
                            );
                            return true;
                        }
                        Err(e) => {
                            format!("it is damaged ({problem}) but can't be moved aside ({e})")
                        }
                    },
                },
            }
        };
        if !self.read_failed_warned.swap(true, Ordering::SeqCst) {
            log::error!(
                "[config] The configuration file could not be read at startup and {reason}, so \
                 it is not overwritten and settings changed in this session are not saved. \
                 Restart the launcher once nothing else is holding the file."
            );
        }
        false
    }

    fn wait_for_backup(&self) {
        if let Some(handle) = self.backup.lock().take() {
            let _ = handle.join();
        }
    }

    pub fn wipe(&self) -> Result<(), String> {
        log::info!("[config] Wiping launcher configuration data...");
        self.wait_for_backup();
        let _guard = self.save_lock.lock();
        self.read_failed.store(false, Ordering::SeqCst);

        let fresh = {
            let old = self.data.read();
            let mut fresh = default_config();
            for id in GAME_IDS {
                if let Some(path) = old["games"][id]["gamePath"]
                    .as_str()
                    .filter(|p| !p.is_empty())
                {
                    fresh["games"][id]["gamePath"] = json!(path);
                }
            }
            fresh
        };

        let _ = std::fs::remove_file(sibling(&self.path, "tmp"));
        let _ = std::fs::remove_file(sibling(&self.path, "bak"));
        let _ = std::fs::remove_file(sibling(&self.path, "bak.tmp"));
        prune_corrupt_backups(&self.user_data, None);

        for cache in [
            "api-config-cache.json",
            "api-config-cache.json.tmp",
            "mods/profiles.json",
            "mods/profiles.json.tmp",
        ] {
            let _ = std::fs::remove_file(self.user_data.join(cache));
        }
        for dir in ["wallpaper-cache", "manifest-cache"] {
            let path = self.user_data.join(dir);
            if path.exists() {
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => log::info!("[config] Removed cache directory {dir}"),
                    Err(e) => log::warn!("[config] Could not remove {dir}: {e}"),
                }
            }
        }

        *self.data.write() = fresh;
        self.dirty.store(true, Ordering::SeqCst);
        self.save_locked()?;

        log::info!(
            "[config] Configuration data wiped successfully, install locations were kept"
        );
        Ok(())
    }
}

fn lookup<'a>(root: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(root, |cur, k| cur.get(k))
}

fn set_path(root: &mut Value, key: &str, value: Value) {
    if !root.is_object() {
        *root = Value::Object(Map::new());
    }
    let keys: Vec<&str> = key.split('.').collect();
    let mut cur = root;
    for k in &keys[..keys.len() - 1] {
        let Some(map) = cur.as_object_mut() else {
            return;
        };
        let entry = map
            .entry((*k).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        cur = entry;
    }
    if let Some(map) = cur.as_object_mut() {
        map.insert(keys[keys.len() - 1].to_string(), value);
    }
}

// ------------ Tests ------------
// Covers loading, recovery from damaged or locked files, saving and the default tables.
#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "peebify-config-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_empty_track_list_is_kept() {
        let mut merged = json!({ "behavior": {
            "overlayAudioTracks": "",
            "closeAction": ""
        } });
        sanitize_behavior(&mut merged);
        assert_eq!(merged["behavior"]["overlayAudioTracks"], json!(""));
        assert_eq!(merged["behavior"]["closeAction"], json!("close"));
    }

    #[test]
    fn a_missing_or_null_track_list_still_heals() {
        let mut merged = json!({ "behavior": { "overlayAudioTracks": "null" } });
        sanitize_behavior(&mut merged);
        assert_eq!(merged["behavior"]["overlayAudioTracks"], json!("desktop,mic"));
    }

    #[test]
    fn a_device_id_only_config_still_shows_the_picker() {
        let dir = scratch("device-only");
        std::fs::write(dir.join(CONFIG_FILE), r#"{"deviceId":"abcd1234"}"#).unwrap();
        let cfg = LauncherConfig::load(&dir);
        assert_eq!(cfg.get("library.setupComplete"), json!(false));
        assert_eq!(cfg.get("library.visible"), json!([]));
        assert_eq!(cfg.get("deviceId"), json!("abcd1234"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_config_from_before_the_library_shows_every_game() {
        let dir = scratch("legacy");
        std::fs::write(
            dir.join(CONFIG_FILE),
            r#"{"games":{"wuwa":{"gamePath":""}}}"#,
        )
        .unwrap();
        let cfg = LauncherConfig::load(&dir);
        assert_eq!(cfg.get("library.setupComplete"), json!(true));
        assert_eq!(cfg.get("library.visible"), json!(GAME_IDS));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_config_is_restored_from_the_last_good_copy() {
        let dir = scratch("restore");
        let good = r#"{"deviceId":"abcd1234","library":{"visible":["wuwa"],"setupComplete":true}}"#;
        std::fs::write(dir.join(CONFIG_FILE), good).unwrap();
        LauncherConfig::load(&dir).wait_for_backup();
        assert!(dir.join(format!("{CONFIG_FILE}.bak")).exists());

        std::fs::write(dir.join(CONFIG_FILE), vec![0u8; 64]).unwrap();
        let cfg = LauncherConfig::load(&dir);
        assert_eq!(cfg.get("deviceId"), json!("abcd1234"));
        assert_eq!(cfg.get("library.visible"), json!(["wuwa"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn corrupt_copies(dir: &Path) -> Vec<PathBuf> {
        let prefix = format!("{CONFIG_FILE}.corrupt-");
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .map(|e| e.path())
            .collect()
    }

    fn on_disk(dir: &Path, name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(dir.join(name)).unwrap()).unwrap()
    }

    const GOOD: &str =
        r#"{"deviceId":"abcd1234","library":{"visible":["wuwa"],"setupComplete":true}}"#;

    #[test]
    fn a_config_that_is_not_utf8_is_set_aside_restored_and_saved_again() {
        let dir = scratch("not-utf8");
        std::fs::write(dir.join(CONFIG_FILE), GOOD).unwrap();
        LauncherConfig::load(&dir).wait_for_backup();

        let damaged = b"{\"deviceId\":\"caf\xE9\"}";
        std::fs::write(dir.join(CONFIG_FILE), damaged).unwrap();
        let cfg = LauncherConfig::load(&dir);
        assert_eq!(cfg.get("library.visible"), json!(["wuwa"]));
        assert!(!cfg.read_failed.load(Ordering::SeqCst));
        assert_eq!(on_disk(&dir, CONFIG_FILE)["deviceId"], json!("abcd1234"));
        let kept = corrupt_copies(&dir);
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read(&kept[0]).unwrap(), damaged);
        assert_eq!(cfg.startup_notice().unwrap()["type"], json!("info"));

        drop(cfg);
        assert!(LauncherConfig::load(&dir).startup_notice().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_config_saved_with_a_byte_order_mark_still_loads_and_is_saved_without_it() {
        let dir = scratch("bom");
        std::fs::write(dir.join(CONFIG_FILE), b"\xEF\xBB\xBF{\"deviceId\":\"abcd1234\"}").unwrap();
        let cfg = LauncherConfig::load(&dir);
        assert_eq!(cfg.get("deviceId"), json!("abcd1234"));
        assert!(cfg.startup_notice().is_none());
        assert!(corrupt_copies(&dir).is_empty());
        assert!(!std::fs::read(dir.join(CONFIG_FILE)).unwrap().starts_with(b"\xEF\xBB\xBF"));
        assert_eq!(on_disk(&dir, CONFIG_FILE)["deviceId"], json!("abcd1234"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_config_that_is_not_an_object_counts_as_damaged() {
        let dir = scratch("not-object");
        std::fs::write(dir.join(CONFIG_FILE), GOOD).unwrap();
        LauncherConfig::load(&dir).wait_for_backup();
        std::fs::write(dir.join(CONFIG_FILE), "null").unwrap();
        let cfg = LauncherConfig::load(&dir);
        assert_eq!(cfg.get("deviceId"), json!("abcd1234"));
        assert_eq!(corrupt_copies(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_first_run_has_nothing_to_report() {
        let dir = scratch("first-run");
        let cfg = LauncherConfig::load(&dir);
        assert!(cfg.startup_notice().is_none());
        assert!(dir.join(CONFIG_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_config_is_restored_from_its_backup_instead_of_replacing_it() {
        let dir = scratch("missing");
        std::fs::write(dir.join(CONFIG_FILE), GOOD).unwrap();
        LauncherConfig::load(&dir).wait_for_backup();
        std::fs::remove_file(dir.join(CONFIG_FILE)).unwrap();

        let cfg = LauncherConfig::load(&dir);
        assert_eq!(cfg.get("library.visible"), json!(["wuwa"]));
        assert_eq!(on_disk(&dir, CONFIG_FILE)["deviceId"], json!("abcd1234"));
        assert_eq!(
            on_disk(&dir, &format!("{CONFIG_FILE}.bak"))["deviceId"],
            json!("abcd1234")
        );
        assert_eq!(
            cfg.startup_notice().unwrap()["title"],
            json!("Settings restored from a backup")
        );

        drop(cfg);
        let cfg = LauncherConfig::load(&dir);
        assert!(cfg.startup_notice().is_none());
        assert_eq!(cfg.get("library.visible"), json!(["wuwa"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_newer_of_the_tmp_and_bak_copies_is_restored() {
        let dir = scratch("newest-copy");
        let path = dir.join(CONFIG_FILE);
        std::fs::write(sibling(&path, "bak"), r#"{"deviceId":"from-bak"}"#).unwrap();
        std::fs::write(sibling(&path, "tmp"), r#"{"deviceId":"from-tmp"}"#).unwrap();
        let age = |suffix: &str, secs: u64| {
            let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
            std::fs::File::options()
                .write(true)
                .open(sibling(&path, suffix))
                .unwrap()
                .set_modified(when)
                .unwrap();
        };
        age("tmp", 3600);
        assert_eq!(newest_good_copy(&path).unwrap().0, "bak");
        age("bak", 7200);
        assert_eq!(newest_good_copy(&path).unwrap().0, "tmp");
        std::fs::write(sibling(&path, "tmp"), "{\"deviceId\":").unwrap();
        assert_eq!(newest_good_copy(&path).unwrap().0, "bak");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    fn hold(path: &Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .unwrap()
    }

    #[cfg(windows)]
    const NEWER: &str =
        r#"{"deviceId":"abcd1234","library":{"visible":["zzz"],"setupComplete":true}}"#;

    #[cfg(windows)]
    fn load_while_held(name: &str) -> (PathBuf, Arc<LauncherConfig>) {
        let dir = scratch(name);
        std::fs::write(dir.join(CONFIG_FILE), GOOD).unwrap();
        LauncherConfig::load(&dir).wait_for_backup();
        std::fs::write(dir.join(CONFIG_FILE), NEWER).unwrap();

        let holder = hold(&dir.join(CONFIG_FILE));
        let cfg = LauncherConfig::load(&dir);
        assert_eq!(cfg.get("library.visible"), json!(["wuwa"]));
        assert!(cfg.read_failed.load(Ordering::SeqCst));
        assert_eq!(cfg.startup_notice().unwrap()["type"], json!("warning"));
        assert_eq!(cfg.startup_notice().unwrap()["saveBlocked"], json!(true));
        cfg.flush();
        drop(holder);
        assert_eq!(std::fs::read_to_string(dir.join(CONFIG_FILE)).unwrap(), NEWER);
        assert!(corrupt_copies(&dir).is_empty());
        (dir, cfg)
    }

    #[cfg(windows)]
    #[test]
    fn a_config_held_by_another_program_is_kept_after_it_is_let_go() {
        let (dir, cfg) = load_while_held("held");
        set_path(&mut cfg.data.write(), "window.width", json!(1234));
        cfg.flush();
        assert!(cfg.read_failed.load(Ordering::SeqCst));
        assert!(cfg.read_failed_kept.load(Ordering::SeqCst));
        assert_eq!(std::fs::read_to_string(dir.join(CONFIG_FILE)).unwrap(), NEWER);
        assert!(corrupt_copies(&dir).is_empty());
        assert_eq!(cfg.startup_notice().unwrap()["type"], json!("warning"));

        std::fs::write(dir.join(CONFIG_FILE), "{\"deviceId\":").unwrap();
        cfg.flush();
        assert!(cfg.read_failed.load(Ordering::SeqCst));
        assert!(corrupt_copies(&dir).is_empty());
        std::fs::write(dir.join(CONFIG_FILE), NEWER).unwrap();

        drop(cfg);
        let cfg = LauncherConfig::load(&dir);
        assert_eq!(cfg.get("library.visible"), json!(["zzz"]));
        assert!(cfg.startup_notice().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_held_config_with_no_backup_is_never_saved_over() {
        let dir = scratch("held-no-backup");
        std::fs::write(dir.join(CONFIG_FILE), NEWER).unwrap();
        let holder = hold(&dir.join(CONFIG_FILE));
        let cfg = LauncherConfig::load(&dir);
        assert_eq!(cfg.get("library.setupComplete"), json!(false));
        assert!(cfg.read_failed.load(Ordering::SeqCst));
        assert_eq!(cfg.startup_notice().unwrap()["type"], json!("warning"));
        assert_eq!(cfg.startup_notice().unwrap()["saveBlocked"], json!(true));

        drop(holder);
        set_path(&mut cfg.data.write(), "library.setupComplete", json!(true));
        cfg.flush();
        assert!(cfg.read_failed.load(Ordering::SeqCst));
        assert_eq!(std::fs::read_to_string(dir.join(CONFIG_FILE)).unwrap(), NEWER);

        std::fs::write(dir.join(CONFIG_FILE), "{\"deviceId\":").unwrap();
        cfg.flush();
        assert!(cfg.read_failed.load(Ordering::SeqCst));
        assert_eq!(
            std::fs::read_to_string(dir.join(CONFIG_FILE)).unwrap(),
            "{\"deviceId\":"
        );
        assert!(corrupt_copies(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_held_config_that_is_gone_once_let_go_is_saved_from_the_restored_copy() {
        let (dir, cfg) = load_while_held("held-gone");
        std::fs::remove_file(dir.join(CONFIG_FILE)).unwrap();
        cfg.flush();
        assert!(!cfg.read_failed.load(Ordering::SeqCst));
        assert_eq!(on_disk(&dir, CONFIG_FILE)["library"]["visible"], json!(["wuwa"]));
        assert!(corrupt_copies(&dir).is_empty());
        assert_eq!(cfg.startup_notice().unwrap()["type"], json!("info"));
        assert_eq!(cfg.startup_notice().unwrap()["saveBlocked"], json!(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_held_config_that_is_damaged_once_let_go_is_set_aside() {
        let (dir, cfg) = load_while_held("held-damaged");
        std::fs::write(dir.join(CONFIG_FILE), "{\"deviceId\":").unwrap();
        cfg.flush();
        assert!(!cfg.read_failed.load(Ordering::SeqCst));
        assert_eq!(on_disk(&dir, CONFIG_FILE)["library"]["visible"], json!(["wuwa"]));
        let kept = corrupt_copies(&dir);
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), "{\"deviceId\":");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_saves_never_collide_on_the_temp_file() {
        let dir = scratch("concurrent");
        let cfg = LauncherConfig::load(&dir);
        let workers: Vec<_> = (0..8)
            .map(|n| {
                let cfg = Arc::clone(&cfg);
                std::thread::spawn(move || {
                    for i in 0..25 {
                        set_path(&mut cfg.data.write(), "window.width", json!(n * 100 + i));
                        cfg.dirty.store(true, Ordering::SeqCst);
                        cfg.save_now().unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(CONFIG_FILE)).unwrap()).unwrap();
        assert_eq!(on_disk["window"]["width"], cfg.get("window.width"));
        assert!(!dir.join(format!("{CONFIG_FILE}.tmp")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_that_changes_nothing_is_not_saved() {
        let dir = scratch("no-op-write");
        std::fs::write(dir.join(CONFIG_FILE), GOOD).unwrap();
        let cfg = LauncherConfig::load(&dir);
        cfg.flush();
        assert!(!cfg.dirty.load(Ordering::SeqCst));

        assert!(cfg.set("behavior.activeGameId", cfg.get("behavior.activeGameId")));
        assert!(cfg.set("library.visible", json!(["wuwa"])));
        assert!(cfg.update("window", |window| {
            window["width"] = window["width"].clone();
            true
        }));
        assert!(!cfg.dirty.load(Ordering::SeqCst));

        assert!(cfg.update("window", |window| {
            window["width"] = json!(1600);
            true
        }));
        assert!(cfg.dirty.load(Ordering::SeqCst));
        cfg.flush();
        assert_eq!(on_disk(&dir, CONFIG_FILE)["window"]["width"], json!(1600));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_interface_copy_leaves_out_playtime() {
        let dir = scratch("interface-copy");
        std::fs::write(
            dir.join(CONFIG_FILE),
            r#"{"deviceId":"abcd1234","games":{"wuwa":{"gamePath":"D:/WW","playtime":{"totalPlaytime":99}}}}"#,
        )
        .unwrap();
        let cfg = LauncherConfig::load(&dir);
        let shown = cfg.get_for_interface();
        assert_eq!(shown["games"]["wuwa"]["gamePath"], json!("D:/WW"));
        assert_eq!(shown["games"]["wuwa"]["graphicsApi"], json!("dx12"));
        assert!(shown["games"]["wuwa"].get("playtime").is_none());
        assert_eq!(shown["library"], cfg.get("library"));
        assert_eq!(shown["deviceId"], json!("abcd1234"));
        assert_eq!(cfg.get("games.wuwa.playtime.totalPlaytime"), json!(99));
        cfg.wait_for_backup();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_startup_backup_is_replaced_whole_and_skipped_when_unchanged() {
        let dir = scratch("backup");
        let bak = dir.join(format!("{CONFIG_FILE}.bak"));
        std::fs::write(dir.join(CONFIG_FILE), GOOD).unwrap();
        LauncherConfig::load(&dir).wait_for_backup();
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), GOOD);
        assert!(!dir.join(format!("{CONFIG_FILE}.bak.tmp")).exists());

        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&bak).unwrap().set_modified(when).unwrap();
        std::fs::write(dir.join(CONFIG_FILE), on_disk(&dir, CONFIG_FILE).to_string()).unwrap();
        let saved = std::fs::read(dir.join(CONFIG_FILE)).unwrap();
        LauncherConfig::load(&dir).wait_for_backup();
        assert_eq!(std::fs::read(&bak).unwrap(), saved);

        let written = std::fs::metadata(&bak).unwrap().modified().unwrap();
        std::fs::File::options().write(true).open(&bak).unwrap().set_modified(when).unwrap();
        LauncherConfig::load(&dir).wait_for_backup();
        assert_eq!(std::fs::read(&bak).unwrap(), saved);
        assert!(!dir.join(format!("{CONFIG_FILE}.bak.tmp")).exists());
        assert!(std::fs::metadata(&bak).unwrap().modified().unwrap() > when);
        assert!(written > when);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unchanged_backup_still_outranks_an_older_leftover_tmp() {
        let dir = scratch("backup-vs-tmp");
        let path = dir.join(CONFIG_FILE);
        std::fs::write(&path, GOOD).unwrap();
        LauncherConfig::load(&dir).wait_for_backup();
        std::fs::write(&path, on_disk(&dir, CONFIG_FILE).to_string()).unwrap();
        LauncherConfig::load(&dir).wait_for_backup();
        let bak = sibling(&path, "bak");
        let tmp = sibling(&path, "tmp");
        let held = std::fs::read(&bak).unwrap();
        assert_eq!(held, std::fs::read(&path).unwrap());
        std::fs::write(&tmp, r#"{"deviceId":"from-tmp"}"#).unwrap();
        let date = |file: &Path, secs: u64| {
            let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
            std::fs::File::options().write(true).open(file).unwrap().set_modified(when).unwrap();
        };
        date(&bak, 7200);
        date(&tmp, 3600);
        assert_eq!(newest_good_copy(&path).unwrap().0, "tmp");

        LauncherConfig::load(&dir).wait_for_backup();
        assert_eq!(std::fs::read(&bak).unwrap(), held);
        assert_eq!(newest_good_copy(&path).unwrap().0, "bak");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wipe_keeps_install_locations() {
        let dir = scratch("wipe");
        std::fs::write(
            dir.join(CONFIG_FILE),
            r#"{"games":{"wuwa":{"gamePath":"D:/Games/Wuthering Waves","playtime":{"totalPlaytime":99}}},"library":{"visible":["wuwa"],"setupComplete":true}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("mods")).unwrap();
        std::fs::write(dir.join("mods").join("profiles.json"), "{}").unwrap();
        std::fs::write(dir.join("mods").join("library.json"), "{}").unwrap();
        let cfg = LauncherConfig::load(&dir);
        cfg.wipe().unwrap();

        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(CONFIG_FILE)).unwrap()).unwrap();
        assert_eq!(on_disk["games"]["wuwa"]["gamePath"], json!("D:/Games/Wuthering Waves"));
        assert_eq!(on_disk["games"]["wuwa"]["playtime"]["totalPlaytime"], json!(0));
        assert_eq!(on_disk["library"]["setupComplete"], json!(false));
        assert!(!dir.join(format!("{CONFIG_FILE}.bak")).exists());
        assert!(!dir.join("mods").join("profiles.json").exists());
        assert!(dir.join("mods").join("library.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_install_on_a_missing_drive_is_kept() {
        let Some(letter) = (b'F'..=b'Z')
            .rev()
            .map(char::from)
            .find(|l| std::fs::metadata(format!("{l}:\\")).is_err())
        else {
            return;
        };
        let dir = scratch("missing-drive");
        let cfg = LauncherConfig::load(&dir);
        let kept = format!("{letter}:\\Games\\Wuthering Waves");
        set_path(&mut cfg.data.write(), "games.wuwa.gamePath", json!(kept));
        assert!(!volume_reachable(&kept));
        assert!(!cfg.sanitize_installed_paths_deferred());
        assert_eq!(cfg.get("games.wuwa.gamePath"), json!(kept));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_game_with_an_unfinished_move_keeps_its_path_for_recovery() {
        let dir = scratch("pending-move");
        let cfg = LauncherConfig::load(&dir);
        let from = dir.join("old").to_string_lossy().into_owned();
        let to = dir.join("new").to_string_lossy().into_owned();
        set_path(&mut cfg.data.write(), "games.wuwa.gamePath", json!(from));
        set_path(
            &mut cfg.data.write(),
            "games.wuwa.pendingMove",
            json!({ "from": from, "to": to }),
        );
        assert!(!cfg.sanitize_installed_paths_deferred());
        assert_eq!(cfg.get("games.wuwa.gamePath"), json!(from));

        set_path(&mut cfg.data.write(), "games.wuwa.pendingMove", Value::Null);
        assert!(cfg.sanitize_installed_paths_deferred());
        assert_eq!(cfg.get("games.wuwa.gamePath"), json!(""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn derived_game_defaults_match_the_hand_written_table() {
        let expected = json!({
            "wuwa": { "gamePath": "", "graphicsApi": "dx12", "resourceQuality": "hd", "launchViaSteam": true, "modsEnabled": false, "playtime": default_playtime_full() },
            "zzz": { "gamePath": "", "graphicsApi": "dx11", "launchViaSteam": true, "modsEnabled": false, "playtime": default_playtime_full() },
            "hsr": { "gamePath": "", "modsEnabled": false, "playtime": default_playtime_full() },
            "nte": { "gamePath": "", "playtime": default_playtime_full() },
            "endfield": { "gamePath": "", "modsEnabled": false, "playtime": default_playtime_full() },
            "genshin": {
                "gamePath": "",
                "modsEnabled": false,
                "fpsUnlock": false,
                "fpsUnlockTarget": 120,
                "fpsUnlockPowerSave": false,
                "fpsUnlockBackgroundFps": 30,
                "playtime": default_playtime_full()
            },
            "hi3": { "gamePath": "", "launchViaSteam": true, "modsEnabled": false, "playtime": default_playtime_full() },
            "pgr": { "gamePath": "", "graphicsApi": "dx11", "launchViaSteam": true, "playtime": default_playtime_full() },
            "gf2": { "gamePath": "", "launchViaSteam": true, "playtime": default_playtime_full() },
            "gf1": { "gamePath": "", "launchViaSteam": true, "playtime": default_playtime_full() },
            "re1999": { "gamePath": "", "playtime": default_playtime_full() },
            "bd2": { "gamePath": "", "playtime": default_playtime_full() },
            "arknights": { "gamePath": "", "graphicsApi": "dx11", "playtime": default_playtime_full() },
            "bluearchive": { "gamePath": "", "launchViaSteam": true, "playtime": default_playtime_full() },
            "dna": { "gamePath": "", "graphicsApi": "dx12", "playtime": default_playtime_full() },
        });
        let defaults = default_config();
        assert_eq!(defaults["games"], expected);
        for section in ["wallpaper", "gameIcons"] {
            let map = defaults[section].as_object().unwrap();
            assert_eq!(map.len(), GAME_IDS.len());
            for id in GAME_IDS {
                assert_eq!(map[id], default_media_slice(), "{section}.{id}");
            }
        }
    }

    #[test]
    fn missing_behavior_defaults_are_filled() {
        let mut merged = json!({
            "behavior": {
                "closeAction": "tray",
                "showNsfwMods": "yes"
            }
        });
        sanitize_behavior(&mut merged);
        let behavior = merged["behavior"].as_object().unwrap();
        assert_eq!(behavior["closeAction"], json!("tray"));
        assert_eq!(behavior["showNsfwMods"], json!(false));
        assert_eq!(behavior["rememberWindowState"], json!(true));

        let mut fresh = json!({});
        sanitize_behavior(&mut fresh);
        assert_eq!(fresh["behavior"]["closeAction"], json!("close"));
    }

    #[test]
    fn graphics_api_choice_follows_the_profiles() {
        let mut merged = json!({
            "games": {
                "wuwa": {},
                "zzz": { "graphicsApi": "dx12" },
                "pgr": { "graphicsApi": "vulkan" },
                "hsr": { "graphicsApi": "dx12" }
            }
        });
        heal_graphics_api(&mut merged);
        assert_eq!(merged["games"]["wuwa"]["graphicsApi"], json!("dx12"));
        assert_eq!(merged["games"]["zzz"]["graphicsApi"], json!("dx12"));
        assert_eq!(merged["games"]["pgr"]["graphicsApi"], json!("dx11"));
        assert!(merged["games"]["hsr"].get("graphicsApi").is_none());
    }

    #[test]
    fn resource_quality_choice_follows_the_profiles() {
        let mut merged = json!({
            "games": {
                "wuwa": { "resourceQuality": "8k" },
                "zzz": { "resourceQuality": "uhd" }
            }
        });
        heal_resource_quality(&mut merged);
        assert_eq!(merged["games"]["wuwa"]["resourceQuality"], json!("hd"));
        assert!(merged["games"]["zzz"].get("resourceQuality").is_none());

        merged["games"]["wuwa"]["resourceQuality"] = json!("uhd");
        heal_resource_quality(&mut merged);
        assert_eq!(merged["games"]["wuwa"]["resourceQuality"], json!("uhd"));
    }

    #[test]
    fn every_profile_names_its_wallpaper_folder() {
        let mut seen = std::collections::HashSet::new();
        for id in GAME_IDS {
            let slug = game_profiles::profile(id)["wallpaperSlug"]
                .as_str()
                .unwrap_or_default();
            assert!(!slug.is_empty(), "{id} has no wallpaperSlug");
            assert!(seen.insert(slug), "{id} reuses the slug {slug}");
        }
    }

    #[test]
    fn webui_game_table_agrees_with_the_profiles() {
        let source = include_str!("../../../webui/src/data/games.ts");
        let blocks: Vec<&str> = source.split("\n    id: \"").skip(1).collect();
        let mut ids = Vec::new();
        for block in blocks {
            let id = &block[..block.find('"').unwrap()];
            ids.push(id.to_string());
            let profile = game_profiles::profile(id);
            let flag = |text: &str| block.contains(text);
            assert_eq!(
                !flag("managed: false"),
                game_profiles::is_managed(profile),
                "{id} managed"
            );
            let api = graphics_api_default(profile);
            assert_eq!(flag("graphicsApiChoice: true"), api.is_some(), "{id} graphicsApiChoice");
            if let Some(api) = api {
                let ts_default = if flag("graphicsApiDefault: \"dx12\"") { "dx12" } else { "dx11" };
                assert_eq!(ts_default, api, "{id} graphicsApiDefault");
            }
            assert_eq!(
                flag("resourceQualityChoice: true"),
                game_profiles::resource_quality_default(profile).is_some(),
                "{id} resourceQualityChoice"
            );
            assert_eq!(
                flag("voicePackChoice: true"),
                game_profiles::install_mode(profile) == Some("sophon"),
                "{id} voicePackChoice"
            );
            assert_eq!(
                flag("fpsUnlock: true"),
                profile.get("fpsUnlock") == Some(&Value::Bool(true)),
                "{id} fpsUnlock"
            );
        }
        let mut expected: Vec<String> = GAME_IDS.iter().map(|id| id.to_string()).collect();
        expected.sort();
        ids.sort();
        assert_eq!(ids, expected);
    }

    #[test]
    fn install_folder_names_match_the_install_modal() {
        let modal = include_str!("../../../webui/src/components/ui/InstallModalBody.tsx");
        let start = modal.find("const INVALID_PATH_CHARS = /[").unwrap() + "const INVALID_PATH_CHARS = /[".len();
        let end = start + modal[start..].find("]/g;").unwrap();
        let mut invalid = Vec::new();
        let mut chars = modal[start..end].chars();
        while let Some(c) = chars.next() {
            invalid.push(if c == '\u{5c}' { chars.next().unwrap() } else { c });
        }
        let ts_folder = |name: &str| -> String {
            name.chars()
                .filter(|c| !invalid.contains(c))
                .collect::<String>()
                .trim()
                .to_string()
        };
        let rust_folder = |profile: &Value| -> String {
            let dirs = super::super::file_channels::default_install_dirs(profile);
            let (_, dir) = dirs.last().unwrap();
            dir.file_name().unwrap().to_string_lossy().to_string()
        };

        let games = include_str!("../../../webui/src/data/games.ts");
        for block in games.split("\n    id: \"").skip(1) {
            let id = &block[..block.find('"').unwrap()];
            let name_start = block.find("\n    name: \"").unwrap() + "\n    name: \"".len();
            let name = &block[name_start..name_start + block[name_start..].find('"').unwrap()];
            let profile = game_profiles::profile(id);
            assert_eq!(game_profiles::display_name(profile), name, "{id} name");
            assert_eq!(rust_folder(profile), ts_folder(name), "{id} folder");
        }

        for name in [" Odd<Game>: Part/2\u{5c}Final|Cut?* ", "A \"Quoted\" Title", "Plain Name"] {
            let profile = json!({ "displayName": name });
            assert_eq!(rust_folder(&profile), ts_folder(name), "{name}");
        }
    }
}
