// ------------ Backend Root ------------
// Lists every backend module, holds the small response helpers they all share, and routes each UI command by
// channel name (like "launch-game" or "list-mods") to the module that handles it.

pub(crate) mod api_config;
pub(crate) mod bd2;
pub(crate) mod bluepoch;
pub(crate) mod capture;
pub(crate) mod config;
pub(crate) mod config_channels;
pub(crate) mod discord_audio;
pub(crate) mod download_engine;
pub(crate) mod file_channels;
pub(crate) mod fps_unlock;
pub(crate) mod fs_util;
mod game_file_ops;
pub(crate) mod game_manager;
pub(crate) mod game_path;
pub mod game_profiles;
pub mod game_updater;
pub(crate) mod gamebanana;
pub(crate) mod gf2;
pub(crate) mod hoyoplay;
pub mod http;
pub(crate) mod hypergryph;
pub(crate) mod hypergryph_reconcile;
mod install_preview;
pub mod logger;
pub(crate) mod mod_ini;
pub(crate) mod mod_profiles;
pub(crate) mod mods;
mod news;
pub mod notify;
pub(crate) mod nte;
pub(crate) mod overlay;
pub(crate) mod overlay_window;
pub(crate) mod perf;
mod playtime;
pub(crate) mod process_utils;
pub(crate) mod progress;
mod queue;
pub(crate) mod recorder;
pub(crate) mod recorder_audio;
pub(crate) mod repair_engine;
pub(crate) mod sophon;
pub mod state;
pub(crate) mod steam;
pub(crate) mod validator;
pub(crate) mod wallpaper_cache;
pub(crate) mod win_startup;
pub(crate) mod window_manager;
pub(crate) mod xxmi;
pub(crate) mod xxmi_update;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

pub const BUILD_TYPE: &str = match option_env!("PEEBIFY_BUILD_TYPE") {
    Some(v) if matches!(v.as_bytes(), b"beta") => v,
    _ => "stable",
};

pub async fn dispatch(
    app: &AppHandle,
    channel: &str,
    args: &[Value],
) -> Option<Result<Value, String>> {
    use config_channels as cfg;

    Some(match channel {
        "get-build-info" => Ok(json!({ "buildType": BUILD_TYPE })),

        "get-config" => cfg::get_config(app, args).await,
        "set-config" => cfg::set_config(app, args).await,
        "get-settings" => cfg::get_settings(app).await,
        "set-setting" => cfg::set_setting(app, args).await,
        "get-app-version" => cfg::get_app_version(app).await,
        "get-supported-games" => cfg::get_supported_games().await,
        "set-active-game" => cfg::set_active_game(app, args).await,
        "browse-game-path" => cfg::browse_game_path(app, args).await,
        "detect-default-game-path" => cfg::detect_default_game_path(app, args).await,
        "set-custom-launcher" => cfg::set_custom_launcher(app, args).await,
        "select-wallpaper-file" => cfg::select_wallpaper_file(app).await,
        "select-game-icon-file" => cfg::select_game_icon_file(app).await,
        "save-game-icon" => cfg::save_game_icon(app, args).await,
        "save-wallpaper" => cfg::save_wallpaper(app, args).await,
        "open-logs-folder" => cfg::open_logs_folder(app).await,
        "wipe-launcher-data" => cfg::wipe_launcher_data(app).await,

        "minimize-window" => window_manager::minimize_window(app).await,
        "close-window" => window_manager::close_window(app).await,
        "toggle-maximize-window" => window_manager::toggle_maximize_window(app).await,
        "get-window-state" => window_manager::get_window_state(app).await,
        "window-ready-to-show" => window_manager::window_ready_to_show(app).await,

        "open-external-url" => config_channels::open_external_url_channel(args).await,
        "get-game-links" => config_channels::get_game_links().await,
        "open-game-folder" => config_channels::open_game_folder(app, args).await,
        "open-screenshot-folder" => config_channels::open_screenshot_folder(app, args).await,
        "get-applied-voice-packs" => config_channels::get_applied_voice_packs(app, args).await,

        "log-message" => Ok(logger::handle_log_message_channel(args)),

        "get-news-data" => news::get_news_data(app, args).await,

        "get-wallpaper-media" => wallpaper_cache::get_wallpaper_media(app).await,

        "get-network-status" => Ok(ok_with(json!({ "isOnline": http::is_online_cached() }))),
        "network-recheck" => http::network_recheck(app).await,

        "launch-game" => game_manager::launch_game(app, args).await,
        "get-running-game" => game_manager::get_running_game(app, args).await,
        "check-for-updates" => game_manager::check_for_updates(app, args).await,
        "get-install-preview" => install_preview::get_install_preview(app, args).await,
        "get-resource-quality" => install_preview::get_resource_quality(app, args).await,
        "get-content-packs" => install_preview::get_content_packs(app, args).await,
        "discord-audio-clients" => discord_audio::list_audio_clients().await,
        "get-steam-install" => game_manager::get_steam_install(app, args).await,
        "steam-update" => game_manager::update_via_steam(app, args).await,
        "get-fps-unlock" => fps_unlock::get_fps_unlock(app, args).await,
        "set-fps-unlock" => fps_unlock::set_fps_unlock(app, args).await,
        "get-playtime-data" => playtime::get_playtime_data(app).await,
        "get-playtime-sessions" => playtime::get_playtime_sessions(app, args).await,
        "playtime-delete-session" => playtime::delete_session(app, args).await,

        "get-download-queue-state" => queue::get_state().await,
        "start-download" => file_channels::start_download(app, args).await,
        "pause-download" => file_channels::pause_download(app, args).await,
        "resume-download" => file_channels::resume_download(app, args).await,
        "cancel-download" => file_channels::cancel_download(app, args).await,
        "prioritize-download" => file_channels::prioritize_download(app, args).await,
        "start-repair" => file_channels::start_repair(app, args).await,
        "start-quick-repair" => file_channels::start_quick_repair(app, args).await,
        "cancel-repair" => file_channels::cancel_repair(app, args).await,
        "pause-repair" => file_channels::pause_repair(app, args).await,
        "resume-repair" => file_channels::resume_repair(app, args).await,
        "cancel-verify" => file_channels::cancel_verify(app, args).await,
        "cancel-move" => file_channels::cancel_move(app, args).await,
        "verify-game-integrity" => file_channels::verify_game_integrity(app, args).await,
        "select-install-directory" => file_channels::select_install_directory(app).await,
        "get-disk-space" => file_channels::get_disk_space(app, args).await,
        "get-default-install-path" => file_channels::get_default_install_path(app, args).await,
        "get-install-path-health" => file_channels::get_install_path_health(app, args).await,
        "move-game-location" => file_channels::move_game_location(app, args).await,
        "uninstall-game" => file_channels::uninstall_game(app, args).await,
        "open-leftover-folder" => file_channels::open_leftover_folder(args).await,

        "mods-status" => mods::mods_status(app, args).await,
        "mod-ini-settings" => mod_ini::mod_ini_settings(app, args).await,
        "set-mod-ini-setting" => mod_ini::set_mod_ini_setting(app, args).await,
        "install-mod-toolchain" => mods::install_mod_toolchain(app, args).await,
        "uninstall-mod-toolchain" => mods::uninstall_mod_toolchain(app, args).await,
        "set-game-mods-enabled" => mods::set_game_mods_enabled(app, args).await,
        "list-mods" => mods::list_mods(app, args).await,
        "import-mod-archive" => mods::import_mod_archive(app, args).await,
        "set-mod-enabled" => mods::set_mod_enabled(app, args).await,
        "delete-mod" => mods::delete_mod(app, args).await,
        "open-mods-folder" => mods::open_mods_folder(app, args).await,
        "set-mods-path" => mods::set_mods_path(app, args).await,
        "set-mods-enabled-bulk" => mods::set_mods_enabled_bulk(app, args).await,

        "list-mod-profiles" => mod_profiles::list_mod_profiles(app, args).await,
        "create-mod-profile" => mod_profiles::create_mod_profile(app, args).await,
        "rename-mod-profile" => mod_profiles::rename_mod_profile(app, args).await,
        "delete-mod-profile" => mod_profiles::delete_mod_profile(app, args).await,
        "duplicate-mod-profile" => mod_profiles::duplicate_mod_profile(app, args).await,
        "set-mod-profile-members" => mod_profiles::set_mod_profile_members(app, args).await,
        "apply-mod-profile" => mod_profiles::apply_mod_profile(app, args).await,

        "get-overlay-config" => overlay::get_overlay_config(app).await,
        "set-overlay-hotkey" => overlay::set_overlay_hotkey(app, args).await,
        "set-overlay-capture-folder" => overlay::set_overlay_capture_folder(app, args).await,
        "open-capture-folder" => overlay::open_capture_folder(app).await,
        "get-overlay-status" => overlay::get_overlay_status(app).await,
        "overlay-screenshot" => capture::take_screenshot(app, args).await,
        "overlay-capture-status" => capture::capture_status(app).await,
        "overlay-list-captures" => capture::list_captures(app).await,
        "overlay-delete-capture" => capture::delete_capture(app, args).await,
        "overlay-reveal-capture" => capture::reveal_capture(app, args).await,
        "overlay-open-capture" => capture::open_capture(app, args).await,
        "overlay-record" => overlay::overlay_record(app).await,
        "suspend-overlay-hotkeys" => overlay::suspend_overlay_hotkeys(app, args).await,
        "overlay-toggle" => overlay::overlay_toggle(app, args).await,
        "overlay-open-launcher" => overlay::open_launcher(app).await,

        "gamebanana-feed" => gamebanana::feed(app, args).await,
        "gamebanana-categories" => gamebanana::categories(app, args).await,
        "gamebanana-mod-profile" => gamebanana::mod_profile(app, args).await,
        "install-gamebanana-mod" => gamebanana::install(app, args).await,
        "cancel-gamebanana-install" => gamebanana::cancel_install(app, args).await,
        "gamebanana-backfill-thumbnails" => gamebanana::backfill_thumbnails(app, args).await,
        "check-mod-updates" => gamebanana::check_updates(app, args).await,
        "update-gamebanana-mod" => gamebanana::update_mod(app, args).await,

        _ => return None,
    })
}

pub(crate) fn backend(app: &AppHandle) -> tauri::State<'_, state::BackendState> {
    app.state::<state::BackendState>()
}

pub(crate) fn active_game_id(app: &AppHandle) -> String {
    backend(app).config.active_game_id()
}

pub(crate) fn ok_response() -> Value {
    json!({ "success": true })
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn ok_with(data: Value) -> Value {
    let mut response = serde_json::Map::new();
    response.insert("success".to_string(), Value::Bool(true));
    if let Value::Object(map) = data {
        for (k, v) in map {
            response.insert(k, v);
        }
    }
    Value::Object(response)
}

pub(crate) fn err_response(error: impl Into<String>) -> Value {
    json!({ "success": false, "error": error.into() })
}

pub(crate) fn arg_str(args: &[Value], index: usize) -> Option<&str> {
    args.get(index)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

pub(crate) fn resolve_profile(app: &AppHandle, game_id: Option<&str>) -> &'static Value {
    let id = match game_id {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => app.state::<state::BackendState>().config.active_game_id(),
    };
    game_profiles::profile(&id)
}

pub(crate) fn wrap_result(label: &str, outcome: Result<Value, String>) -> Value {
    match outcome {
        Ok(_) => ok_response(),
        Err(e) => {
            log::error!("{label}: {e}");
            err_response(e)
        }
    }
}
