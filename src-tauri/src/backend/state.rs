// ------------ Backend State ------------
// The shared state every command can reach. Startup creates it: it finds the user data folder, sets up logging,
// loads the config and builds the managers for games, downloads, windows and the overlay.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Manager};

use super::api_config::ApiConfig;
use super::config::LauncherConfig;
use super::file_channels::EnginePools;
use super::game_manager::GameManager;
use super::game_updater::GameUpdater;
use super::overlay::OverlayManager;
use super::wallpaper_cache::WallpaperCache;
use super::window_manager::WindowManager;

pub struct BackendState {
    pub user_data: PathBuf,
    pub logs_dir: PathBuf,
    pub config: Arc<LauncherConfig>,
    pub api_config: Arc<ApiConfig>,
    pub wallpaper: Arc<WallpaperCache>,
    pub game: Arc<GameManager>,
    pub engine: Arc<EnginePools>,
    pub window: Arc<WindowManager>,
    pub game_updater: Arc<GameUpdater>,
    pub overlay: Arc<OverlayManager>,
}

impl BackendState {
    pub fn init(app: &AppHandle) -> Result<Self, String> {
        let user_data = match debug_user_data_override() {
            Some(dir) => dir,
            None => app
                .path()
                .data_dir()
                .map_err(|e| format!("could not resolve data dir: {e}"))?
                .join("Peebify Launcher"),
        };
        let logs_dir = user_data.join("logs");
        let _ = std::fs::create_dir_all(&user_data);
        let _ = std::fs::create_dir_all(&logs_dir);

        super::logger::init(&logs_dir, Some(super::win_startup::is_boot_launch()));

        let config = LauncherConfig::load(&user_data);
        for id in super::game_profiles::GAME_IDS {
            if let serde_json::Value::String(lang) =
                config.get(&format!("games.{id}.voicePackLanguage"))
            {
                super::game_profiles::set_audio_language(id, &lang);
            }
            let tags = config.get(&format!("games.{id}.contentTags"));
            if let Some(list) = super::config_channels::parse_content_tags(&tags) {
                super::game_profiles::set_content_tags(id, Some(list));
            }
        }
        super::sophon::set_manifest_cache_dir(user_data.join("manifest-cache").join("sophon"));
        tauri::async_runtime::spawn_blocking(|| {
            super::sophon::sweep_manifest_cache(std::time::Duration::from_secs(60 * 60 * 24 * 14));
        });

        let now_ms = chrono::Utc::now().timestamp_millis();
        let install_roots: Vec<(&'static str, std::path::PathBuf)> =
            super::game_profiles::GAME_IDS
                .iter()
                .filter(|id| {
                    part_sweep_due(&config.get(&format!("games.{id}.lastPartSweep")), now_ms)
                })
                .filter_map(|id| match config.get(&format!("games.{id}.gamePath")) {
                    serde_json::Value::String(path) if !path.trim().is_empty() => {
                        Some((*id, std::path::PathBuf::from(path)))
                    }
                    _ => None,
                })
                .collect();
        let sweep_config = Arc::clone(&config);
        tauri::async_runtime::spawn_blocking(move || {
            for (id, root) in install_roots {
                super::download_engine::sweep_orphaned_parts(
                    &root,
                    std::time::Duration::from_secs(60 * 60 * 24 * 7),
                );
                sweep_config.set(
                    &format!("games.{id}.lastPartSweep"),
                    serde_json::json!(chrono::Utc::now().timestamp_millis()),
                );
            }
        });
        let api_config = ApiConfig::new(&user_data);
        let wallpaper = WallpaperCache::new(app.clone(), &user_data);
        let game = GameManager::new(app.clone());
        let engine = EnginePools::new(app.clone());
        let window = WindowManager::new(app.clone());
        let game_updater = GameUpdater::new(app.clone());
        let overlay = OverlayManager::new(app.clone());

        Ok(Self {
            user_data,
            logs_dir,
            config,
            api_config,
            wallpaper,
            game,
            engine,
            window,
            game_updater,
            overlay,
        })
    }

    pub fn allow_asset_paths(&self, app: &AppHandle) {
        super::fs_util::allow_asset_dir(app, &self.user_data.join("wallpaper-cache"));
        super::config_channels::allow_custom_media(app, &self.config);
        let app = app.clone();
        let config = Arc::clone(&self.config);
        tauri::async_runtime::spawn_blocking(move || {
            let captures = super::overlay::capture_dir_in(&app, &config);
            if captures.is_dir() {
                super::fs_util::allow_asset_dir(&app, &captures);
            } else {
                log::info!(
                    "asset scope: the capture folder {} is not reachable yet",
                    captures.display()
                );
            }
        });
    }
}

const PART_SWEEP_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

fn part_sweep_due(last: &serde_json::Value, now_ms: i64) -> bool {
    match last.as_i64() {
        Some(last) if last <= now_ms => now_ms - last >= PART_SWEEP_INTERVAL_MS,
        _ => true,
    }
}

pub(crate) fn user_data_dir_before_setup() -> Option<std::path::PathBuf> {
    debug_user_data_override().or_else(|| {
        std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("Peebify Launcher"))
    })
}

fn debug_user_data_override() -> Option<std::path::PathBuf> {
    #[cfg(debug_assertions)]
    {
        let raw = std::env::var("PEEBIFY_USER_DATA").ok()?;
        let path = std::path::PathBuf::from(raw.trim());
        if path.is_absolute() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_sweep_due_without_stamp() {
        assert!(part_sweep_due(&serde_json::Value::Null, 1_000));
        assert!(part_sweep_due(&serde_json::json!("x"), 1_000));
    }

    #[test]
    fn part_sweep_waits_for_interval() {
        let now = 10 * PART_SWEEP_INTERVAL_MS;
        assert!(!part_sweep_due(&serde_json::json!(now - 1), now));
        assert!(!part_sweep_due(
            &serde_json::json!(now - PART_SWEEP_INTERVAL_MS + 1),
            now
        ));
        assert!(part_sweep_due(
            &serde_json::json!(now - PART_SWEEP_INTERVAL_MS),
            now
        ));
    }

    #[test]
    fn part_sweep_due_when_stamp_is_in_the_future() {
        assert!(part_sweep_due(&serde_json::json!(5_000), 1_000));
    }
}
