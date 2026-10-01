// ------------ Config Channels ------------
// The handlers behind the window's settings calls: reading and changing config, picking game folders, wallpapers and icons, opening logs and folders, and wiping launcher data.
// The window is only allowed to change a fixed list of keys, and each value is checked before it is saved.
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager};

use super::state::BackendState;
use super::{
    active_game_id, arg_str, backend, config, err_response, file_channels, fs_util,
    game_file_ops, game_path, game_profiles, ok_response, ok_with, win_startup, wrap_result,
};

pub(crate) fn set_config_value(app: &AppHandle, key: &str, value: Value) {
    backend(app).config.set(key, value);
}

pub(super) async fn get_config(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    if let Some(key) = arg_str(args, 0) {
        return Ok(ok_with(json!({ "value": backend(app).config.get(key) })));
    }
    if let Some(keys) = args.first().and_then(Value::as_array) {
        let config = &backend(app).config;
        let mut values = Map::new();
        for key in keys {
            let Some(key) = key.as_str().filter(|k| !k.is_empty()) else {
                return Ok(err_response("Config keys must be non-empty strings"));
            };
            values.insert(key.to_string(), config.get(key));
        }
        return Ok(ok_with(json!({ "values": values })));
    }
    Ok(ok_with(backend(app).config.get_for_interface()))
}

pub(super) fn parse_content_tags(value: &Value) -> Option<Vec<String>> {
    let list = value.as_array()?;
    Some(
        list.iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

// ------------ What the Window May Change ------------
// The list of settings the interface can write, with the type each one must have, and the handler that applies a change.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    Bool,
    Str,
    StrArray,
    StrArrayOrNull,
}

impl ValueKind {
    fn accepts(self, value: &Value) -> bool {
        match self {
            ValueKind::Bool => value.is_boolean(),
            ValueKind::Str => value.is_string(),
            ValueKind::StrArray => value
                .as_array()
                .is_some_and(|list| list.iter().all(Value::is_string)),
            ValueKind::StrArrayOrNull => value.is_null() || ValueKind::StrArray.accepts(value),
        }
    }
}

const WRITABLE_KEYS: &[(&str, ValueKind)] = &[
    ("behavior.modsEnabled", ValueKind::Bool),
    ("behavior.modsAutoUpdate", ValueKind::Bool),
    ("library.visible", ValueKind::StrArray),
    ("library.setupComplete", ValueKind::Bool),
];

const WRITABLE_GAME_KEYS: &[(&str, ValueKind)] = &[
    ("autoUpdate", ValueKind::Bool),
    ("autoUpdateOnStartup", ValueKind::Bool),
    ("autoUpdateSchedule", ValueKind::Str),
    ("contentTags", ValueKind::StrArrayOrNull),
    ("graphicsApi", ValueKind::Str),
    ("launchArgs", ValueKind::Str),
    ("launchViaSteam", ValueKind::Bool),
    ("playtimeColor", ValueKind::Str),
    ("resourceQuality", ValueKind::Str),
    ("voicePackLanguage", ValueKind::Str),
];

fn renderer_may_write(key: &str, value: &Value) -> Result<(), String> {
    if let Some((_, kind)) = WRITABLE_KEYS.iter().find(|(k, _)| *k == key) {
        return if kind.accepts(value) {
            Ok(())
        } else {
            Err(format!("the value for '{key}' has the wrong type"))
        };
    }

    if let Some(rest) = key.strip_prefix("games.") {
        if let Some((id, leaf)) = rest.split_once('.') {
            if !game_profiles::is_known_game_id(id) {
                return Err(format!("'{id}' is not a known game"));
            }
            if let Some((_, kind)) = WRITABLE_GAME_KEYS.iter().find(|(k, _)| *k == leaf) {
                return if kind.accepts(value) {
                    Ok(())
                } else {
                    Err(format!("the value for '{key}' has the wrong type"))
                };
            }
        }
    }

    Err(format!("'{key}' is not a setting the interface may change"))
}

const SETTING_LOG_MAX_CHARS: usize = 120;

fn setting_log_value(value: &str) -> String {
    if value.chars().count() <= SETTING_LOG_MAX_CHARS {
        return value.to_string();
    }
    let head: String = value.chars().take(SETTING_LOG_MAX_CHARS).collect();
    format!("{head}...")
}

fn library_change_needs_sync(key: &str, previous: &Value, value: &Value) -> bool {
    if key != "library.visible" {
        return previous != value;
    }
    let ids = |list: &Value| -> std::collections::BTreeSet<String> {
        list.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    ids(previous) != ids(value)
}

pub(super) async fn set_config(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let Some(key) = args
        .first()
        .and_then(|v| v.as_str())
        .filter(|k| !k.is_empty())
    else {
        return Ok(err_response("Failed to set config value"));
    };
    let value = args.get(1).cloned().unwrap_or(Value::Null);

    if let Err(reason) = renderer_may_write(key, &value) {
        log::warn!("[config] Rejected a set-config write: {reason}");
        return Ok(err_response(format!(
            "Failed to set config value: {reason}"
        )));
    }
    if let Some(id) = key
        .strip_prefix("games.")
        .and_then(|rest| rest.strip_suffix(".voicePackLanguage"))
    {
        super::game_profiles::set_audio_language(id, value.as_str().unwrap_or_default());
        super::install_preview::invalidate_preview(id);
    }
    if let Some(id) = key
        .strip_prefix("games.")
        .and_then(|rest| rest.strip_suffix(".contentTags"))
    {
        super::game_profiles::set_content_tags(id, parse_content_tags(&value));
        super::install_preview::invalidate_preview(id);
    }
    let previous = key
        .starts_with("library.")
        .then(|| backend(app).config.get(key));
    set_config_value(app, key, value.clone());
    let shown = value.as_str().map_or_else(|| value.to_string(), str::to_string);
    log::info!("[settings] {key} = {}", setting_log_value(&shown));
    if previous.is_some_and(|previous| library_change_needs_sync(key, &previous, &value)) {
        backend(app).wallpaper.wake("after a library change");
    }
    use tauri::Emitter;
    let _ = app.emit("settings-changed", json!({ "key": key, "value": value }));
    Ok(ok_response())
}

pub(super) async fn get_app_version(app: &AppHandle) -> Result<Value, String> {
    Ok(ok_with(json!({
        "version": app.package_info().version.to_string()
    })))
}

pub(super) async fn get_supported_games() -> Result<Value, String> {
    Ok(ok_with(json!({ "games": game_profiles::list_profiles() })))
}

// ------------ Game Selection and Paths ------------
// Switching the active game, browsing for or auto-detecting an install folder, and choosing a custom launcher.
pub(super) async fn set_active_game(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let requested = args.first().and_then(|v| v.as_str()).unwrap_or("");
    let profile = game_profiles::profile(requested);
    let id = game_profiles::profile_id(profile).to_string();
    set_config_value(app, "behavior.activeGameId", json!(id));
    backend(app).wallpaper.wake_if_uncached(&id);
    Ok(ok_with(json!({ "activeGameId": id })))
}

pub(super) fn browse_start_dir(app: &AppHandle, profile: &Value) -> Option<String> {
    let key = format!("games.{}.gamePath", game_profiles::profile_id(profile));
    let current = backend(app).config.get(&key);
    let current_parent = current
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| Path::new(s).parent().map(Path::to_path_buf));
    current_parent
        .into_iter()
        .chain(
            file_channels::default_install_dirs(profile)
                .into_iter()
                .filter_map(|(_, dir)| dir.parent().map(Path::to_path_buf)),
        )
        .find(|dir| dir.is_dir())
        .map(|dir| dir.to_string_lossy().into_owned())
}

fn refuse_locate_beside_job(app: &AppHandle, profile: &Value) -> Option<Value> {
    let id = game_profiles::profile_id(profile);
    if !backend(app).engine.queue.has_job_for(id) && !game_file_ops::is_uninstalling(id) {
        return None;
    }
    log::info!("Refusing to locate {id} because it has an operation running or queued.");
    Some(err_response(format!(
        "Finish or cancel {}'s current download, repair or move first.",
        game_profiles::display_name(profile)
    )))
}

fn adopt_located_path(app: &AppHandle, profile: &Value, resolved: &str, picked: Option<&str>) {
    let id = game_profiles::profile_id(profile);
    set_config_value(app, &format!("games.{id}.gamePath"), json!(resolved));
    super::file_channels::clear_pending_install(app, id);
    super::download_engine::clear_install_marker(Path::new(resolved));
    if let Some(picked) = picked.filter(|p| *p != resolved) {
        super::download_engine::clear_install_marker(Path::new(picked));
    }
}

pub(super) async fn browse_game_path(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let profile = match game_file_ops::known_profile_or_error(app, arg_str(args, 0)) {
        Ok(profile) => profile,
        Err(refusal) => return Ok(refusal),
    };
    if let Some(refusal) = refuse_locate_beside_job(app, profile) {
        return Ok(refusal);
    }

    let dialog_result = crate::backend::fs_util::dialog::show_open(
        app,
        json!({
            "title": format!("Select {} Installation Folder", game_profiles::display_name(profile)),
            "directory": true,
            "defaultPath": browse_start_dir(app, profile)
        }),
    )
    .await?;

    let selected = dialog_result["filePaths"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let Some(selected) = selected else {
        return Ok(json!({ "success": false, "cancelled": true }));
    };

    let selected_for_walk = selected.clone();
    let validation = tauri::async_runtime::spawn_blocking(move || {
        game_path::validate_game_path_for_profile(&selected_for_walk, profile)
    })
    .await
    .map_err(|e| e.to_string())?;

    if validation.is_valid {
        if let Some(refusal) = refuse_locate_beside_job(app, profile) {
            return Ok(refusal);
        }
        let resolved = validation.resolved_path.unwrap_or_else(|| selected.clone());
        adopt_located_path(app, profile, &resolved, Some(&selected));
        game_profiles::resolve_audio_language(app, profile, Path::new(&resolved), None).await;
        Ok(ok_with(json!({ "path": resolved })))
    } else {
        Ok(err_response(
            validation.error.as_deref().unwrap_or("Invalid game path"),
        ))
    }
}

const LAUNCHER_EXTENSIONS: [&str; 3] = ["exe", "bat", "cmd"];

pub(super) async fn set_custom_launcher(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let Some(game_id) = args.first().and_then(Value::as_str) else {
        return Ok(err_response("No game was given."));
    };
    if !game_profiles::is_known_game_id(game_id) {
        return Ok(err_response(format!("'{game_id}' is not a known game.")));
    }
    let key = format!("games.{game_id}.customLauncher");

    if args.get(1).and_then(Value::as_str) != Some("pick") {
        set_config_value(app, &key, Value::String(String::new()));
        log::info!("[config] the custom launcher for {game_id} was cleared");
        return Ok(ok_with(json!({ "path": "" })));
    }

    let picked = pick_single_file(
        app,
        json!({
            "title": "Choose the program that starts the game",
            "filters": [{ "name": "Program", "extensions": LAUNCHER_EXTENSIONS }],
        }),
    )
    .await?;
    let Some(path) = picked["path"].as_str().map(str::to_string) else {
        return Ok(picked);
    };

    let candidate = Path::new(&path);
    if !candidate
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            LAUNCHER_EXTENSIONS
                .iter()
                .any(|ok| e.eq_ignore_ascii_case(ok))
        })
    {
        return Ok(err_response("Pick a program: an .exe, .bat or .cmd file."));
    }
    if !candidate.is_file() {
        return Ok(err_response(format!("There is no file at {path}.")));
    }

    set_config_value(app, &key, Value::String(path.clone()));
    log::info!("[config] the custom launcher for {game_id} is now {path}");
    Ok(ok_with(json!({ "path": path })))
}

pub(super) async fn detect_default_game_path(
    app: &AppHandle,
    args: &[Value],
) -> Result<Value, String> {
    let profile = match game_file_ops::known_profile_or_error(app, arg_str(args, 0)) {
        Ok(profile) => profile,
        Err(refusal) => return Ok(refusal),
    };
    if let Some(refusal) = refuse_locate_beside_job(app, profile) {
        return Ok(refusal);
    }

    let (found, unfinished) = tauri::async_runtime::spawn_blocking(move || {
        let found = file_channels::install_in_default_dirs(profile);
        let unfinished = found.is_none()
            && file_channels::default_install_dirs(profile)
                .iter()
                .any(|(_, dir)| {
                    dir.is_dir() && file_channels::is_unfinished_fresh_install(dir, dir)
                });
        (found, unfinished)
    })
    .await
    .map_err(|e| e.to_string())?;

    if let Some(resolved) = found {
        if let Some(refusal) = refuse_locate_beside_job(app, profile) {
            return Ok(refusal);
        }
        adopt_located_path(app, profile, &resolved, None);
        game_profiles::resolve_audio_language(app, profile, Path::new(&resolved), None).await;
        return Ok(ok_with(json!({ "path": resolved })));
    }
    if unfinished {
        return Ok(err_response(
            "The install at Peebify's default location never finished. Install the game again to pick up where it stopped.",
        ));
    }
    Ok(err_response(
        "Nothing installed at Peebify's default install location.",
    ))
}

// ------------ Behavior Settings ------------
// Reads and saves the general launcher settings, merging in only what changed.
pub(super) async fn get_settings(app: &AppHandle) -> Result<Value, String> {
    let behavior = backend(app).config.get("behavior");
    let mut flat = Map::new();
    if let Some(b) = behavior.as_object() {
        for (k, v) in b {
            let s = match v {
                Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            flat.insert(k.clone(), json!(s));
        }
        let truthy = |v: Option<&Value>| match v {
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
            Some(Value::String(s)) => !s.is_empty(),
            _ => false,
        };
        flat.insert(
            "animatedWallpaper".to_string(),
            json!(if truthy(b.get("disableAnimations")) {
                "false"
            } else {
                "true"
            }),
        );
    }
    Ok(ok_with(json!({
        "settings": Value::Object(flat),
        "configNotice": backend(app).config.startup_notice(),
    })))
}

const BACKEND_OWNED_BEHAVIOR: [&str; 3] = ["modsPath", "overlayCaptureFolder", "activeGameId"];

fn setting_may_write(key: &str) -> Result<(), String> {
    if key.is_empty() || key.contains('.') {
        return Err(format!("'{key}' is not a setting id"));
    }
    if BACKEND_OWNED_BEHAVIOR.contains(&key)
        || super::overlay::HOTKEYS.iter().any(|(id, _, _)| *id == key)
    {
        return Err(format!("'{key}' has its own control and cannot be set directly"));
    }
    Ok(())
}

fn setting_delta(key: &str, value: &str) -> Map<String, Value> {
    let mut delta = Map::new();
    match key {
        "animatedWallpaper" => {
            delta.insert("disableAnimations".to_string(), json!(value != "true"));
        }
        _ if value == "true" || value == "false" => {
            delta.insert(key.to_string(), json!(value == "true"));
        }
        _ => {
            delta.insert(key.to_string(), json!(value));
        }
    }
    delta
}

pub(super) async fn set_setting(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let Some(key) = args
        .first()
        .and_then(|v| v.as_str())
        .filter(|k| !k.is_empty())
    else {
        return Ok(err_response("Missing setting id"));
    };
    let value = args
        .get(1)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if let Err(reason) = setting_may_write(key) {
        log::warn!("[config] Rejected a set-setting write: {reason}");
        return Ok(err_response(format!("Failed to save the setting: {reason}")));
    }

    let saved = save_behavior_settings(app, &[Value::Object(setting_delta(key, &value))]).await?;
    if saved.get("success") != Some(&Value::Bool(true)) {
        return Ok(saved);
    }
    log::info!("[settings] {key} = {}", setting_log_value(&value));
    if key == "animatedWallpaper" && value == "true" {
        backend(app).wallpaper.wake("after animated wallpapers were turned on");
    }
    use tauri::Emitter;
    if key.starts_with("overlay") {
        backend(app).overlay.on_setting_changed(key);
    }
    let _ = app.emit("settings-changed", json!({ "key": key, "value": value }));
    Ok(saved)
}

fn merge_behavior(behavior: &mut Value, delta: &Map<String, Value>) -> bool {
    let current = behavior.take();
    let mut updated = current.as_object().cloned().unwrap_or_default();
    for (k, v) in delta {
        updated.insert(k.clone(), v.clone());
    }
    let mut wrapper = json!({ "behavior": Value::Object(updated) });
    config::sanitize_behavior(&mut wrapper);
    *behavior = wrapper["behavior"].take();
    delta.get("rememberWindowState") == Some(&Value::Bool(true))
        && current["rememberWindowState"] != Value::Bool(true)
}

pub(super) async fn save_behavior_settings(
    app: &AppHandle,
    args: &[Value],
) -> Result<Value, String> {
    let Some(new_behavior) = args.first().and_then(|v| v.as_object()) else {
        return Ok(err_response("Invalid behavior settings payload"));
    };

    let before = backend(app).config.get("behavior");

    let mut note = None;
    if let Some(new_boot) = new_behavior.get("startOnBoot") {
        if *new_boot != before["startOnBoot"] {
            let enable = new_boot.as_bool().unwrap_or(false);
            match win_startup::manage_windows_startup(enable).await {
                Ok(partial) => {
                    log::info!(
                        "[config] Start on boot {}",
                        if enable { "enabled" } else { "disabled" }
                    );
                    note = partial;
                }
                Err(e) => {
                    log::error!("[config] Failed to manage startup methods: {e}");
                    return Ok(err_response(format!(
                        "Could not update \"start on boot\": {e}"
                    )));
                }
            }
        }
    }

    let mut remember_turned_on = false;
    backend(app).config.update("behavior", |behavior| {
        remember_turned_on = merge_behavior(behavior, new_behavior);
        true
    });
    if remember_turned_on {
        backend(app).window.schedule_window_state_save();
    }

    match note {
        Some(text) => Ok(ok_with(json!({
            "warningTitle": "Start with Windows",
            "warning": text,
        }))),
        None => Ok(ok_response()),
    }
}

// ------------ Wallpapers and Game Icons ------------
// Picking custom wallpapers and game icons from disk, and only trusting files that were actually picked in the dialog.
const WALLPAPER_EXTENSIONS: [&str; 9] =
    ["mp4", "webm", "mov", "m4v", "png", "jpg", "jpeg", "webp", "gif"];
const ICON_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];
const MEDIA_SECTIONS: [&str; 2] = ["wallpaper", "gameIcons"];

fn media_extensions(section: &str) -> &'static [&'static str] {
    match section {
        "wallpaper" => &WALLPAPER_EXTENSIONS,
        _ => &ICON_EXTENSIONS,
    }
}

fn media_label(section: &str) -> &'static str {
    match section {
        "wallpaper" => "wallpaper",
        _ => "game icon",
    }
}

static PICKED_MEDIA: parking_lot::Mutex<Vec<PathBuf>> = parking_lot::Mutex::new(Vec::new());

fn record_picked_media(path: PathBuf) {
    let mut picked = PICKED_MEDIA.lock();
    if !picked.contains(&path) {
        picked.push(path);
    }
}

fn was_picked(path: &Path) -> bool {
    PICKED_MEDIA.lock().iter().any(|p| p == path)
}

fn remember_picked_media(app: &AppHandle, section: &str, picked: &Value) {
    let Some(path) = picked.get("path").and_then(Value::as_str).map(PathBuf::from) else {
        return;
    };
    if custom_media_path_ok(section, &path) {
        fs_util::allow_asset_file(app, &path);
        record_picked_media(path);
    }
}

fn custom_media_path_ok(section: &str, path: &Path) -> bool {
    path.is_absolute()
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                media_extensions(section)
                    .iter()
                    .any(|allowed| ext.eq_ignore_ascii_case(allowed))
            })
}

fn parse_media_slice(section: &str, slice: &Value) -> Result<(Value, Option<PathBuf>), String> {
    let label = media_label(section);
    match slice.get("type").and_then(Value::as_str) {
        Some("default") => Ok((json!({ "type": "default", "path": null }), None)),
        Some("custom") => {
            let raw = slice.get("path").and_then(Value::as_str).unwrap_or_default();
            let path = PathBuf::from(raw);
            if !custom_media_path_ok(section, &path) {
                return Err(format!("That file can't be used as a {label}."));
            }
            Ok((json!({ "type": "custom", "path": raw }), Some(path)))
        }
        _ => Err(format!("The {label} setting was not understood.")),
    }
}

pub(super) fn allow_custom_media(app: &AppHandle, config: &config::LauncherConfig) {
    let mut files: Vec<(&'static str, PathBuf)> = Vec::new();
    for section in MEDIA_SECTIONS {
        for id in game_profiles::GAME_IDS {
            let slice = config.get(&format!("{section}.{id}"));
            if let Ok((_, Some(file))) = parse_media_slice(section, &slice) {
                if !files.iter().any(|(_, seen)| *seen == file) {
                    files.push((section, file));
                }
            }
        }
    }
    for (section, file) in files {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            if file.is_file() {
                fs_util::allow_asset_file(&app, &file);
            } else {
                log::info!(
                    "asset scope: the custom {section} {} is not reachable",
                    file.display()
                );
            }
        });
    }
}

fn media_dialog_filters(section: &str) -> Value {
    match section {
        "wallpaper" => json!([
            { "name": "Images and videos", "extensions": WALLPAPER_EXTENSIONS },
            { "name": "Images",            "extensions": ["png", "jpg", "jpeg", "webp", "gif"] },
            { "name": "Videos",            "extensions": ["mp4", "webm", "mov", "m4v"] }
        ]),
        _ => json!([
            { "name": "All icons", "extensions": ICON_EXTENSIONS },
            { "name": "Images",    "extensions": ["png", "jpg", "jpeg"] },
            { "name": "Animated",  "extensions": ["gif", "webp"] }
        ]),
    }
}

pub(super) async fn select_wallpaper_file(app: &AppHandle) -> Result<Value, String> {
    let picked = pick_single_file(
        app,
        json!({
            "title": "Select Custom Wallpaper",
            "filters": media_dialog_filters("wallpaper")
        }),
    )
    .await?;
    remember_picked_media(app, "wallpaper", &picked);
    Ok(picked)
}

pub(super) async fn select_game_icon_file(app: &AppHandle) -> Result<Value, String> {
    let picked = pick_single_file(
        app,
        json!({
            "title": "Select Custom Game Icon",
            "filters": media_dialog_filters("gameIcons")
        }),
    )
    .await?;
    remember_picked_media(app, "gameIcons", &picked);
    Ok(picked)
}

async fn pick_single_file(app: &AppHandle, params: Value) -> Result<Value, String> {
    let result = crate::backend::fs_util::dialog::show_open(app, params).await?;
    let picked = result["filePaths"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(str::to_string);
    match picked {
        Some(path) => Ok(ok_with(json!({ "path": path }))),
        None => Ok(json!({ "success": false, "cancelled": true })),
    }
}

pub(super) async fn save_game_icon(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    save_media_slice(app, args, "gameIcons").await
}

pub(super) async fn save_wallpaper(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    save_media_slice(app, args, "wallpaper").await
}

async fn save_media_slice(app: &AppHandle, args: &[Value], section: &str) -> Result<Value, String> {
    let label = media_label(section);
    let payload = args.first().cloned().unwrap_or(Value::Null);
    if !payload.is_object() {
        return Ok(err_response(format!("The {label} setting was not understood.")));
    }

    let game_id = match payload.get("gameId") {
        None | Some(Value::Null) => active_game_id(app),
        Some(Value::String(id)) if id.is_empty() => active_game_id(app),
        Some(Value::String(id)) if game_profiles::is_known_game_id(id) => id.clone(),
        Some(other) => {
            log::warn!(
                "[config] Rejected a {section} save for an unknown game: {}",
                setting_log_value(&other.to_string())
            );
            return Ok(err_response("That is not a known game."));
        }
    };

    let slice_config = match payload.get("config") {
        Some(c) if !c.is_null() => c,
        _ => &payload,
    };
    let (slice_config, file) = match parse_media_slice(section, slice_config) {
        Ok(parsed) => parsed,
        Err(reason) => {
            log::warn!("[config] Rejected a {section} save for {game_id}: {reason}");
            return Ok(err_response(reason));
        }
    };
    if let Some(file) = &file {
        let stored = backend(app).config.get(&format!("{section}.{game_id}.path"));
        if stored.as_str() != file.to_str() && !was_picked(file) {
            log::warn!(
                "[config] Rejected a {section} save for {game_id}: {} was not picked",
                setting_log_value(&file.display().to_string())
            );
            return Ok(err_response(format!("Choose the {label} with the file picker.")));
        }
        if file.is_file() {
            fs_util::allow_asset_file(app, file);
        }
    }

    let mut slices = match backend(app).config.get(section) {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    slices.retain(|id, _| game_profiles::is_known_game_id(id));
    slices.insert(game_id, slice_config);
    set_config_value(app, section, Value::Object(slices));

    Ok(ok_response())
}

// ------------ Logs, Folders and Links ------------
// Opening the logs, game and screenshot folders and external links, wiping launcher data, and listing applied voice packs.
fn newest_launch_log(dir: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("launch-") && name.ends_with(".log"))
        .max()
        .map(|name| dir.join(name))
}

fn reveal_in_explorer(file: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", file.to_string_lossy()))
        .spawn()
        .is_ok()
}

pub(super) async fn open_logs_folder(app: &AppHandle) -> Result<Value, String> {
    super::logger::flush();
    let logs_dir = backend(app).logs_dir.clone();
    let _ = std::fs::create_dir_all(&logs_dir);
    let logs_str = logs_dir.to_string_lossy().to_string();

    if newest_launch_log(&logs_dir).is_some_and(|current| reveal_in_explorer(&current)) {
        return Ok(ok_with(json!({ "path": logs_str })));
    }

    match crate::backend::fs_util::dialog::open_path(json!({ "path": logs_str })).await {
        Ok(_) => Ok(ok_with(json!({ "path": logs_str }))),
        Err(e) => {
            log::error!("[config] Failed to open logs folder: {e}");
            Ok(err_response(e))
        }
    }
}

const WIPE_BUSY_MSG: &str =
    "Finish or cancel the running download or uninstall before clearing launcher data.";

pub(super) async fn wipe_launcher_data(app: &AppHandle) -> Result<Value, String> {
    let uninstalling = game_profiles::GAME_IDS
        .iter()
        .any(|id| game_file_ops::is_uninstalling(id));
    if backend(app).engine.queue.is_busy()
        || uninstalling
        || super::gamebanana::any_in_flight()
    {
        return Ok(err_response(WIPE_BUSY_MSG));
    }
    if let Some((game_id, _)) = backend(app).game.running_game() {
        return Ok(err_response(format!(
            "Close {} before clearing launcher data.",
            game_profiles::display_name(game_profiles::profile(&game_id))
        )));
    }

    let partials: Vec<(&'static str, std::path::PathBuf)> = {
        let config = &backend(app).config;
        game_profiles::GAME_IDS
            .iter()
            .filter(|id| {
                config
                    .get(&format!("games.{id}.gamePath"))
                    .as_str()
                    .is_none_or(str::is_empty)
            })
            .filter_map(|id| {
                config
                    .get(&format!("games.{id}.pendingInstall.path"))
                    .as_str()
                    .filter(|p| !p.is_empty())
                    .map(|p| (*id, std::path::PathBuf::from(p)))
            })
            .collect()
    };
    if !partials.is_empty() {
        let (files, bytes) = tauri::async_runtime::spawn_blocking(move || {
            partials
                .iter()
                .map(|(id, path)| super::download_engine::discard_partial_install_for(path, id))
                .fold((0, 0), |acc, (f, b)| (acc.0 + f, acc.1 + b))
        })
        .await
        .unwrap_or((0, 0));
        if files > 0 {
            log::info!(
                "[config] Discarded {files} unfinished download file(s), {:.2} GB, before the data wipe",
                bytes as f64 / 1e9
            );
        }
    }

    match win_startup::manage_windows_startup(false).await {
        Ok(note) => {
            log::info!("[config] Cleaned up startup methods during data wipe");
            if let Some(note) = note {
                log::warn!("[config] {note}");
            }
        }
        Err(e) => log::warn!("[config] Could not clean up startup methods during wipe: {e}"),
    }

    if let Err(e) = backend(app).config.wipe() {
        log::error!("[config] Failed to wipe launcher data: {e}");
        return Ok(err_response(e));
    }

    super::logger::end_session("restart after the data wipe");
    app.restart()
}

async fn open_external_url(url: &str) -> Result<Value, String> {
    if url.trim().is_empty() {
        return Ok(err_response("Invalid URL provided."));
    }
    Ok(wrap_result(
        &format!("Failed to open external URL: {url}"),
        crate::backend::fs_util::dialog::open_external(json!({ "url": url })).await,
    ))
}

pub(super) async fn open_external_url_channel(args: &[Value]) -> Result<Value, String> {
    let url = args.first().and_then(|v| v.as_str()).unwrap_or("");
    open_external_url(url).await
}

pub(super) async fn get_game_links() -> Result<Value, String> {
    let mut links = serde_json::Map::new();
    for id in game_profiles::GAME_IDS {
        let profile = game_profiles::profile(id);
        let show_lunite = profile.get("showLuniteSocial") != Some(&Value::Bool(false));
        let lunite_as_hoyolab = profile.get("luniteUsesHoyolabIcon") == Some(&Value::Bool(true));

        let mut socials = Vec::new();
        if let Some(map) = profile.get("socialUrls").and_then(|v| v.as_object()) {
            for (platform, url) in map {
                let Some(url) = url.as_str().filter(|u| !u.is_empty()) else {
                    continue;
                };
                let platform = if platform == "lunite" {
                    if !show_lunite {
                        continue;
                    }
                    if lunite_as_hoyolab {
                        "hoyolab"
                    } else {
                        "lunite"
                    }
                } else {
                    platform.as_str()
                };
                socials.push(json!({ "platform": platform, "url": url }));
            }
        }

        let mut tools = Vec::new();
        if let Some(ct) = profile.get("communityTools").and_then(|v| v.as_object()) {
            for group in ["official", "community"] {
                if let Some(arr) = ct.get(group).and_then(|v| v.as_array()) {
                    for t in arr {
                        if let (Some(name), Some(url)) = (
                            t.get("name").and_then(|v| v.as_str()),
                            t.get("url").and_then(|v| v.as_str()),
                        ) {
                            tools.push(json!({ "name": name, "url": url }));
                        }
                    }
                }
            }
        }

        links.insert(
            id.to_string(),
            json!({ "socials": socials, "communityTools": tools }),
        );
    }
    Ok(super::ok_with(json!({ "links": links })))
}

pub(super) async fn open_game_folder(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let profile = match game_file_ops::known_profile_or_error(app, arg_str(args, 0)) {
        Ok(profile) => profile,
        Err(refusal) => return Ok(refusal),
    };
    let key = format!("games.{}.gamePath", game_profiles::profile_id(profile));
    let game_path = app.state::<BackendState>().config.get(key.as_str());
    let game_path = game_path.as_str().unwrap_or("");
    if game_path.is_empty() {
        return Ok(err_response("Game path not configured."));
    }
    Ok(wrap_result(
        "open-game-folder",
        crate::backend::fs_util::dialog::open_path(json!({ "path": game_path })).await,
    ))
}

fn screenshot_folder_in(root: std::path::PathBuf, game_folder: &str) -> std::path::PathBuf {
    let per_game = root.join(game_folder);
    if per_game.is_dir() {
        per_game
    } else {
        root
    }
}

pub(super) async fn open_screenshot_folder(
    app: &AppHandle,
    args: &[Value],
) -> Result<Value, String> {
    let game_id = arg_str(args, 0)
        .filter(|id| game_profiles::is_known_game_id(id))
        .map(str::to_string)
        .unwrap_or_else(|| active_game_id(app));
    let dir = screenshot_folder_in(
        super::overlay::capture_dir(app),
        &super::capture::game_folder(&game_id),
    );
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Ok(err_response(format!("Could not open that folder: {e}")));
    }
    crate::backend::fs_util::dialog::open_path(json!({ "path": dir.to_string_lossy() })).await?;
    Ok(ok_response())
}

pub(super) async fn get_applied_voice_packs(
    app: &AppHandle,
    args: &[Value],
) -> Result<Value, String> {
    let profile = match game_file_ops::known_profile_or_error(app, arg_str(args, 0)) {
        Ok(profile) => profile,
        Err(refusal) => return Ok(refusal),
    };
    if game_profiles::install_mode(profile) != Some("sophon") {
        return Ok(ok_with(json!({ "languages": Value::Null })));
    }
    let key = format!("games.{}.gamePath", game_profiles::profile_id(profile));
    let game_path = app.state::<BackendState>().config.get(key.as_str());
    let game_path = game_path.as_str().unwrap_or("").to_string();
    if game_path.is_empty() {
        return Ok(ok_with(json!({ "languages": Value::Null })));
    }
    let languages = tauri::async_runtime::spawn_blocking(move || {
        super::sophon::load_applied(Path::new(&game_path))
            .map(|applied| super::sophon::applied_voice_languages(&applied))
    })
    .await
    .unwrap_or(None);
    Ok(ok_with(json!({ "languages": languages })))
}

// ------------ Tests ------------
// Covers the write allowlist, screenshot folder choice and wallpaper or icon settings.
#[cfg(test)]
mod write_allowlist_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_keys_the_interface_actually_writes_are_allowed() {
        for (key, value) in [
            ("behavior.modsEnabled", json!(true)),
            ("behavior.modsAutoUpdate", json!(false)),
            ("library.setupComplete", json!(true)),
            ("library.visible", json!(["wuwa", "zzz"])),
            ("games.wuwa.launchArgs", json!("-dx11")),
            ("games.wuwa.launchViaSteam", json!(false)),
            ("games.zzz.graphicsApi", json!("dx12")),
            ("games.genshin.voicePackLanguage", json!("ja-jp")),
            ("games.hsr.contentTags", json!(["en-us"])),
            ("games.pgr.autoUpdate", json!(true)),
            ("games.pgr.autoUpdateOnStartup", json!(false)),
            ("games.pgr.autoUpdateSchedule", json!("daily")),
        ] {
            assert!(
                renderer_may_write(key, &value).is_ok(),
                "the interface writes {key} but the allowlist rejects it"
            );
        }
    }

    #[test]
    fn state_the_backend_owns_is_refused() {
        for (key, value) in [
            ("games.wuwa.gamePath", json!(r"C:\Windows\System32")),
            ("behavior.modsPath", json!(r"C:\Users\me")),
            ("games.wuwa.pendingInstall", json!({})),
        ] {
            assert!(
                renderer_may_write(key, &value).is_err(),
                "{key} must not be renderer-writable"
            );
        }
    }

    #[test]
    fn a_whole_subtree_cannot_be_written_in_one_go() {
        assert!(
            renderer_may_write("games", &json!({ "wuwa": { "gamePath": r"C:\Windows" } })).is_err()
        );
        assert!(renderer_may_write("behavior", &json!({ "modsEnabled": true })).is_err());
        assert!(renderer_may_write("library", &json!({ "visible": [] })).is_err());
    }

    #[test]
    fn logged_setting_values_are_clipped() {
        assert_eq!(setting_log_value("tray"), "tray");
        let exact = "a".repeat(SETTING_LOG_MAX_CHARS);
        assert_eq!(setting_log_value(&exact), exact);
        let long = "\u{e9}".repeat(SETTING_LOG_MAX_CHARS + 30);
        let shown = setting_log_value(&long);
        assert!(shown.ends_with("..."));
        assert_eq!(shown.chars().count(), SETTING_LOG_MAX_CHARS + 3);
    }

    #[test]
    fn content_tags_accept_null_to_mean_every_pack() {
        assert!(renderer_may_write("games.nte.contentTags", &Value::Null).is_ok());
        assert_eq!(parse_content_tags(&Value::Null), None);
        assert!(renderer_may_write("games.nte.launchArgs", &Value::Null).is_err());
        assert!(renderer_may_write("games.nte.contentTags", &json!([1])).is_err());
    }

    #[test]
    fn set_setting_accepts_every_key_the_interface_writes() {
        for key in [
            "startOnBoot",
            "startOnBootAction",
            "launchAction",
            "closeAction",
            "animatedWallpaper",
            "timeFormat",
            "showNsfwMods",
            "overlayEnabled",
            "overlayAudioTracks",
            "overlayAudioDiscord",
            "modsEnabled",
            "modsPerPage",
        ] {
            assert!(setting_may_write(key).is_ok(), "the interface writes {key}");
        }
    }

    #[test]
    fn set_setting_refuses_state_with_its_own_channel() {
        for key in [
            "modsPath",
            "overlayCaptureFolder",
            "activeGameId",
            "overlayHotkey",
            "overlayShotHotkey",
            "overlayRecHotkey",
            "",
            "games.wuwa.gamePath",
        ] {
            assert!(setting_may_write(key).is_err(), "{key} must not be set-setting writable");
        }
    }

    #[test]
    fn set_setting_sends_only_the_changed_leaf() {
        assert_eq!(
            Value::Object(setting_delta("animatedWallpaper", "false")),
            json!({ "disableAnimations": true })
        );
        assert_eq!(
            Value::Object(setting_delta("startOnBoot", "true")),
            json!({ "startOnBoot": true })
        );
        assert_eq!(
            Value::Object(setting_delta("launchAction", "tray")),
            json!({ "launchAction": "tray" })
        );
    }

    #[test]
    fn a_behavior_merge_keeps_the_other_keys_and_reports_its_side_effects() {
        let delta = |v: Value| v.as_object().cloned().unwrap();
        let mut behavior = json!({
            "hideSocials": true,
            "rememberWindowState": false
        });
        let turned_on = merge_behavior(&mut behavior, &delta(json!({ "rememberWindowState": true })));
        assert!(turned_on);
        assert_eq!(behavior["hideSocials"], json!(true));
        assert_eq!(behavior["rememberWindowState"], json!(true));
        assert_eq!(behavior["closeAction"], json!("close"));

        let turned_on = merge_behavior(&mut behavior, &delta(json!({ "hideSocials": false })));
        assert!(!turned_on);
        assert_eq!(behavior["rememberWindowState"], json!(true));
        assert_eq!(behavior["hideSocials"], json!(false));
    }

    #[test]
    fn the_newest_launch_log_is_the_current_one() {
        let dir = std::env::temp_dir().join(format!("peebify-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(newest_launch_log(&dir), None);
        for name in [
            "launch-2026-09-24T10-00-00.000Z.log",
            "launch-2026-09-25T08-00-00.000Z.1.log",
            "launch-2026-09-25T08-00-00.000Z.log",
            "overlay-helper.log",
            "mod-loader.log",
        ] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        assert_eq!(
            newest_launch_log(&dir),
            Some(dir.join("launch-2026-09-25T08-00-00.000Z.log"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_games_and_wrong_types_are_refused() {
        assert!(renderer_may_write("games.notagame.launchArgs", &json!("x")).is_err());
        assert!(renderer_may_write("games.wuwa.launchArgs", &json!(42)).is_err());
        assert!(renderer_may_write("games.wuwa.launchViaSteam", &json!("true")).is_err());
        assert!(renderer_may_write("library.visible", &json!([1, 2])).is_err());
        assert!(renderer_may_write("games.wuwa", &json!({})).is_err());
    }
}

#[cfg(test)]
mod screenshot_folder_tests {
    use super::screenshot_folder_in;

    #[test]
    fn opens_the_game_subfolder_when_it_exists_and_the_root_otherwise() {
        let root = std::env::temp_dir().join(format!("peebify-shots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Wuthering Waves")).unwrap();
        assert_eq!(
            screenshot_folder_in(root.clone(), "Wuthering Waves"),
            root.join("Wuthering Waves")
        );
        assert_eq!(screenshot_folder_in(root.clone(), "Genshin Impact"), root);
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod media_slice_tests {
    use super::*;

    #[test]
    fn a_default_slice_is_stored_without_a_path_or_extra_fields() {
        let (slice, file) = parse_media_slice(
            "wallpaper",
            &json!({ "type": "default", "path": r"C:\x.png", "junk": [1, 2, 3] }),
        )
        .unwrap();
        assert_eq!(slice, json!({ "type": "default", "path": null }));
        assert_eq!(file, None);
    }

    #[test]
    fn a_custom_slice_keeps_only_its_type_and_path() {
        let (slice, file) = parse_media_slice(
            "wallpaper",
            &json!({ "type": "custom", "path": r"D:\Walls\Night.MP4", "extra": { "big": "x" } }),
        )
        .unwrap();
        assert_eq!(slice, json!({ "type": "custom", "path": r"D:\Walls\Night.MP4" }));
        assert_eq!(file, Some(PathBuf::from(r"D:\Walls\Night.MP4")));
        assert!(
            parse_media_slice("gameIcons", &json!({ "type": "custom", "path": r"C:\i\icon.WebP" }))
                .is_ok()
        );
    }

    #[test]
    fn custom_paths_must_be_absolute_and_a_type_their_picker_offers() {
        for (section, path) in [
            ("wallpaper", json!("")),
            ("wallpaper", json!("wall.png")),
            ("wallpaper", json!(r"C:\Users\me\.ssh\id_rsa")),
            ("wallpaper", json!(r"C:\Users\me\notes.txt")),
            ("gameIcons", json!(r"C:\clips\intro.mp4")),
            ("gameIcons", json!(7)),
            ("gameIcons", Value::Null),
        ] {
            let slice = json!({ "type": "custom", "path": path });
            assert!(parse_media_slice(section, &slice).is_err(), "{section} {slice}");
        }
        assert!(parse_media_slice("wallpaper", &json!({ "type": "custom" })).is_err());
    }

    #[test]
    fn unknown_slice_shapes_are_refused() {
        for slice in [
            json!({}),
            json!({ "type": "remote", "path": null }),
            json!({ "type": 1 }),
            json!("custom"),
            Value::Null,
        ] {
            assert!(parse_media_slice("gameIcons", &slice).is_err(), "{slice}");
        }
    }

    #[test]
    fn the_all_files_filters_offer_what_a_save_accepts() {
        for ext in WALLPAPER_EXTENSIONS {
            let path = PathBuf::from(format!(r"C:\w\a.{ext}"));
            assert!(custom_media_path_ok("wallpaper", &path), "{ext}");
        }
        for ext in ICON_EXTENSIONS {
            let path = PathBuf::from(format!(r"C:\w\a.{ext}"));
            assert!(custom_media_path_ok("gameIcons", &path), "{ext}");
        }
    }

    #[test]
    fn media_dialogs_open_on_every_accepted_file() {
        for section in MEDIA_SECTIONS {
            let filters = media_dialog_filters(section);
            let first: Vec<&str> = filters[0]["extensions"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert_eq!(first, media_extensions(section), "{section}");
        }
    }

    #[test]
    fn the_video_filter_matches_what_the_wallpaper_cache_calls_video() {
        let filters = media_dialog_filters("wallpaper");
        let videos = filters
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "Videos")
            .unwrap();
        let videos: Vec<&str> = videos["extensions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for ext in WALLPAPER_EXTENSIONS {
            let video = super::super::wallpaper_cache::is_video_ext(&format!(".{ext}"));
            assert_eq!(videos.contains(&ext), video, "{ext}");
        }
    }

    #[test]
    fn only_files_from_the_picker_are_remembered() {
        let picked = PathBuf::from(r"C:\peebify-media-test\picked.png");
        let other = PathBuf::from(r"C:\peebify-media-test\other.png");
        record_picked_media(picked.clone());
        record_picked_media(picked.clone());
        assert!(was_picked(&picked));
        assert!(!was_picked(&other));
        assert_eq!(PICKED_MEDIA.lock().iter().filter(|p| **p == picked).count(), 1);
    }
}
