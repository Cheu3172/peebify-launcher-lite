// ------------ Game Manager ------------
// Launches a game, watches its process while it runs, and keeps track of which games are running so the rest of the launcher can react.
// It also answers the update check for each game and exposes the launch, running game and Steam commands the UI calls.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::process::Child;

use super::playtime::{PlaytimeTracker, ProcessIdentity};
use super::state::BackendState;
use super::{
    config_channels, err_response, fps_unlock, game_path, game_profiles, http, mods, ok_with,
    process_utils, sophon, steam, xxmi,
};
use super::download_engine::GAME_CONFIG_FILE;

const PROCESS_MONITOR_INTERVAL: Duration = Duration::from_secs(1);
const PID_RECONFIRM_INTERVAL: Duration = Duration::from_secs(4);
const LAUNCH_CONFIRM_TIMEOUT: Duration = Duration::from_secs(90);
const STEAM_LAUNCH_CONFIRM_TIMEOUT: Duration = Duration::from_secs(6 * 60);
const CUSTOM_LAUNCH_CONFIRM_TIMEOUT: Duration = Duration::from_secs(6 * 60);
const STEAM_UPDATE_WATCH_TIMEOUT: Duration = Duration::from_secs(3 * 60 * 60);
const PASSIVE_WATCH_PROBE_INTERVAL: Duration = Duration::from_secs(5);
const LAUNCH_FAILED_GRACE: Duration = Duration::from_secs(3);
const LAUNCH_HANDOFF_MAX_WAIT: Duration = Duration::from_secs(10 * 60);
const LAUNCH_NEVER_STARTED: &str = "The game never started. It may have been cancelled at the Windows permission prompt or blocked by security software.";
const STEAM_LAUNCH_NEVER_STARTED: &str =
    "Steam never started the game. Check Steam for an update or a prompt.";
const STEAM_LAUNCH_STILL_WAITING: &str = "Steam hasn't started the game yet. Check Steam for an update or a prompt. Peebify still tracks the game once Steam starts it.";
const FPS_PROMPT_DECLINED: &str = "You declined the Windows prompt.";
const UPDATE_CACHE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MISSING_GRACE: Duration = Duration::from_secs(10);

struct GameSession {
    proc_name: String,
    pid: Option<u32>,
    started_at: Option<i64>,
    last_pid_reconfirm: Instant,
    launch_deadline: Option<Instant>,
    passive: bool,
    via_steam: bool,
    handed_off: bool,
    last_seen_ms: i64,
    missing_since: Option<Instant>,
    next_probe: Instant,
}

impl GameSession {
    fn is_running(&self) -> bool {
        self.started_at.is_some()
    }

    fn probe_due(&mut self, now: Instant) -> bool {
        if !self.passive || self.is_running() {
            return true;
        }
        if now < self.next_probe {
            return false;
        }
        self.next_probe = now + PASSIVE_WATCH_PROBE_INTERVAL;
        true
    }

    fn counts_as_active(&self) -> bool {
        !self.passive || self.is_running()
    }
}

enum Transition {
    None,
    Started,
    Stopped { duration_ms: i64, ended_ms: i64 },
    LaunchFailed,
    SteamStillStarting,
    WatchExpired,
}

#[derive(Default)]
struct Monitor {
    sessions: HashMap<String, GameSession>,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
}

pub struct GameManager {
    app: AppHandle,
    pub playtime: PlaytimeTracker,
    monitor: Mutex<Monitor>,
    launching: Mutex<HashSet<String>>,
    update_cache: Mutex<HashMap<String, (Value, Instant)>>,
    update_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

struct LaunchingGuard {
    manager: Arc<GameManager>,
    game_id: String,
}

impl Drop for LaunchingGuard {
    fn drop(&mut self) {
        self.manager.launching.lock().remove(&self.game_id);
        if let Some(state) = self.manager.app.try_state::<BackendState>() {
            state.window.update_tray_menu();
        }
    }
}

impl GameManager {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app,
            playtime: PlaytimeTracker::new(),
            monitor: Mutex::new(Monitor::default()),
            launching: Mutex::new(HashSet::new()),
            update_cache: Mutex::new(HashMap::new()),
            update_locks: Mutex::new(HashMap::new()),
        })
    }

    pub fn is_game_active_id(&self, game_id: &str) -> bool {
        self.monitor
            .lock()
            .sessions
            .get(game_id)
            .is_some_and(GameSession::counts_as_active)
            || self.launching.lock().contains(game_id)
    }

    pub fn is_any_game_active(&self) -> bool {
        self.monitor
            .lock()
            .sessions
            .values()
            .any(GameSession::counts_as_active)
            || !self.launching.lock().is_empty()
    }

    fn has_other_session(&self, game_id: &str) -> bool {
        self.monitor
            .lock()
            .sessions
            .iter()
            .any(|(id, session)| id != game_id && session.counts_as_active())
            || self.launching.lock().iter().any(|id| id != game_id)
    }

    pub fn is_game_running(&self) -> bool {
        self.monitor
            .lock()
            .sessions
            .values()
            .any(GameSession::is_running)
    }

    pub fn is_game_running_id(&self, game_id: &str) -> bool {
        self.monitor
            .lock()
            .sessions
            .get(game_id)
            .is_some_and(GameSession::is_running)
    }

    pub fn running_pid(&self, game_id: &str) -> Option<u32> {
        self.monitor
            .lock()
            .sessions
            .get(game_id)
            .filter(|s| s.is_running())
            .and_then(|s| s.pid)
    }

    pub fn running_game(&self) -> Option<(String, Option<u32>)> {
        self.monitor
            .lock()
            .sessions
            .iter()
            .find(|(_, s)| s.is_running())
            .map(|(id, s)| (id.clone(), s.pid))
    }

    fn running_games(&self) -> Vec<String> {
        let mut games: Vec<String> = self
            .monitor
            .lock()
            .sessions
            .iter()
            .filter(|(_, s)| s.is_running())
            .map(|(id, _)| id.clone())
            .collect();
        games.sort();
        games
    }

    fn config(&self) -> Arc<super::config::LauncherConfig> {
        self.app.state::<BackendState>().config.clone()
    }

    fn active_game_id(&self) -> String {
        self.config().active_game_id()
    }

    fn resolve_game_id(&self, target: Option<&str>) -> String {
        match target {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => self.active_game_id(),
        }
    }

    async fn emit_game_event(&self, channel: &str, payload: Value) {
        if let Err(e) = self.app.emit(channel, payload.clone()) {
            log::warn!("failed to emit '{channel}' to webview: {e}");
        }
    }

    fn graphics_api_args(&self, profile_id: &str, profile: &Value) -> Vec<String> {
        if mods::active_for(&self.app, profile_id, profile) {
            let forced = game_profiles::mod_force_dx11_args(profile);
            if !forced.is_empty() {
                log::info!("Mods are on for {profile_id} — forcing DX11 ({forced:?})");
                return forced;
            }
        }

        let Some(by_api) = profile.get("graphicsApiArgs") else {
            return Vec::new();
        };
        let choice = match self
            .config()
            .get(&format!("games.{profile_id}.graphicsApi"))
        {
            Value::String(s) if by_api.get(&s).is_some() => s,
            _ => profile
                .get("graphicsApiDefault")
                .and_then(Value::as_str)
                .unwrap_or("dx11")
                .to_string(),
        };
        by_api
            .get(&choice)
            .and_then(Value::as_array)
            .map(|args| {
                args.iter()
                    .filter_map(|a| a.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn resource_quality_args(&self, profile: &Value, install_root: &Path) -> Vec<String> {
        let Some(by_quality) = profile.get("resourceQualityArgs") else {
            return Vec::new();
        };
        let Some(selected) = super::download_engine::selected_quality(&self.app, profile) else {
            return Vec::new();
        };
        let choice =
            super::download_engine::installed_quality(profile, install_root, Some(&selected))
                .unwrap_or(selected);
        by_quality
            .get(&choice)
            .and_then(Value::as_array)
            .map(|args| {
                args.iter()
                    .filter_map(|a| a.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn resolve_install_root(
        &self,
        profile_id: &str,
        profile: &'static Value,
    ) -> Option<PathBuf> {
        let game_path_str = match self.config().get(&format!("games.{profile_id}.gamePath")) {
            Value::String(s) if !s.is_empty() => s,
            _ => {
                log::debug!("{profile_id} has no configured game path");
                return None;
            }
        };

        let game_path_for_walk = game_path_str.clone();
        let install_root = tauri::async_runtime::spawn_blocking(move || {
            if let Some(marker) = game_profiles::install_root_marker(profile) {
                game_path::find_install_root_by_marker(Path::new(&game_path_for_walk), &marker, 4)
            } else {
                game_path::find_game_install_root(
                    Path::new(&game_path_for_walk),
                    game_profiles::executable_name(profile),
                    4,
                )
            }
        })
        .await
        .ok()
        .flatten();

        let Some(install_root) = install_root else {
            log::error!("Game executable not found under {game_path_str}");
            return None;
        };
        let install_root_str = install_root.to_string_lossy().to_string();
        if install_root_str != game_path_str {
            log::info!("Resolved {profile_id} install root: {install_root_str}");
            config_channels::set_config_value(
                &self.app,
                &format!("games.{profile_id}.gamePath"),
                json!(install_root_str),
            );
        }
        Some(install_root)
    }

    pub async fn steam_app_id(&self, profile_id: &str, profile: &'static Value) -> Option<String> {
        steam::app_id(profile)?;
        let root = self.resolve_install_root(profile_id, profile).await?;
        steam::detect(&root, profile)
    }

    pub async fn steam_install_info(&self, target_game_id: Option<&str>) -> Value {
        let game_id = self.resolve_game_id(target_game_id);
        let profile = game_profiles::profile(&game_id);
        let profile_id = game_profiles::profile_id(profile).to_string();

        let app_id = self.steam_app_id(&profile_id, profile).await;

        ok_with(json!({
            "gameId": profile_id,
            "isSteamInstall": app_id.is_some(),
            "appId": app_id,
            "steamFound": app_id.is_some() && steam::steam_exe().is_some(),
        }))
    }

    // ------------ Launching Games ------------
    // Works out the install folder, launch arguments, FPS unlocker, mods and Steam or custom launcher hand off, then starts the game and begins watching it.
    pub async fn launch_game(self: &Arc<Self>, target_game_id: Option<&str>) -> Value {
        let game_id = self.resolve_game_id(target_game_id);
        let profile = game_profiles::profile(&game_id);
        let profile_id = game_profiles::profile_id(profile).to_string();
        let display_name = game_profiles::display_name(profile).to_string();

        match self.monitor.lock().sessions.get(&profile_id) {
            Some(session) if session.is_running() => {
                log::warn!("Launch attempted while {profile_id} is already running");
                return err_response(format!("{display_name} is already running."));
            }
            Some(session) if session.passive => {}
            Some(_) => {
                log::warn!("Launch attempted while {profile_id} is still starting");
                return err_response(format!("{display_name} is already starting."));
            }
            None => {}
        }
        if let Some(op) = self
            .app
            .state::<BackendState>()
            .engine
            .queue
            .launch_blocker(&profile_id)
        {
            log::warn!("Launch attempted while a {op} for {profile_id} is queued or running");
            return err_response(busy_launch_message(&display_name, &op));
        }
        if self.update_left_unfinished(&profile_id) {
            log::warn!("Launch refused: the last update of {profile_id} didn't finish");
            return err_response(format!(
                "{display_name}'s last update didn't finish. Finish the update before playing."
            ));
        }
        if !self.launching.lock().insert(profile_id.clone()) {
            log::warn!("Launch attempted while {profile_id} is still starting");
            return err_response(format!("{display_name} is already starting."));
        }
        let _launching = LaunchingGuard {
            manager: Arc::clone(self),
            game_id: profile_id.clone(),
        };
        self.app.state::<BackendState>().window.update_tray_menu();

        let Some(install_root) = self.resolve_install_root(&profile_id, profile).await else {
            return err_response("Game executable not found. Please verify game files.");
        };

        let proc_name = client_process_name(profile);
        if let Some(pid) = process_utils::find_by_name_under(&proc_name, &install_root).await {
            log::info!(
                "{profile_id} is already running outside Peebify (pid {pid}), tracking it instead of starting another copy"
            );
            if self.adopt_running(&profile_id, proc_name, pid) {
                self.handle_game_started(&profile_id).await;
            }
            return ok_with(json!({ "viaSteam": false, "alreadyRunning": true }));
        }

        let (executable_path, mut launch_args) = game_path::resolve_launch(&install_root, profile);
        launch_args.extend(self.graphics_api_args(&profile_id, profile));
        launch_args.extend(self.resource_quality_args(profile, &install_root));
        if let Value::String(custom) = self.config().get(&format!("games.{profile_id}.launchArgs"))
        {
            launch_args.extend(split_launch_args(&custom));
        }

        if !executable_path.exists() {
            log::error!("Game executable not found at {}", executable_path.display());
            return err_response("Game executable not found. Please verify game files.");
        }

        let args_suffix = if launch_args.is_empty() {
            String::new()
        } else {
            format!(" {}", launch_args.join(" "))
        };

        let custom_launcher = self.custom_launcher(&profile_id, &display_name);
        let via_steam = match custom_launcher {
            Some(_) => None,
            None => self.steam_launch_target(&profile_id, &install_root, profile),
        };
        let steam_launch = via_steam.is_some();
        let loader_wait = if custom_launcher.is_some() {
            CUSTOM_LAUNCH_CONFIRM_TIMEOUT
        } else if via_steam.is_some() {
            STEAM_LAUNCH_CONFIRM_TIMEOUT
        } else {
            LAUNCH_CONFIRM_TIMEOUT
        };

        let mut mods_armed = false;
        if mods::active_for(&self.app, &profile_id, profile) {
            let state = self.app.state::<BackendState>();
            let root = xxmi::root(&state.config, &state.user_data);
            mods::log_launch_state(&self.app, &profile_id, profile);

            match xxmi::prepare_and_spawn(&self.app, &root, profile, loader_wait).await {
                Ok(()) => mods_armed = true,
                Err(e) => {
                    log::error!("Mod loader failed to start for {profile_id}: {e}");
                    super::notify::notify_if_backgrounded(
                        &self.app,
                        "Mods didn't load",
                        &format!("{display_name} is starting without mods. {e}"),
                    );
                    self.emit_game_event(
                        "mods-load-failed",
                        json!({ "gameId": profile_id, "error": e }),
                    )
                    .await;
                }
            }
        }

        let unlock = match via_steam {
            Some(_) => None,
            None => match fps_unlock::prepare(
                &self.app,
                &profile_id,
                profile,
                &executable_path,
                &launch_args,
                custom_launcher
                    .is_some()
                    .then(|| game_profiles::client_process_name(profile)),
            ) {
                Ok(prepared) => prepared,
                Err(e) => {
                    fps_unlock::report_failure(&self.app, &profile_id, &display_name, &e);
                    None
                }
            },
        };
        let mut unlocked = unlock.is_some();
        let via_helper = custom_launcher.is_none() && via_steam.is_none() && unlocked;

        let (child, confirm_timeout) = match custom_launcher {
            Some(launcher) => {
                if let Some(unlock) = &unlock {
                    log::info!(
                        "Starting the FPS unlocker first so it can attach once the game appears"
                    );
                    if let Err(e) = process_utils::launch_game_via_shell(
                        &unlock.helper,
                        &unlock.arguments,
                        unlock.helper.parent(),
                    )
                    .await
                    {
                        let reason = if e.declined {
                            FPS_PROMPT_DECLINED.to_string()
                        } else {
                            e.message
                        };
                        fps_unlock::end_session(&self.app, &profile_id);
                        fps_unlock::report_failure(&self.app, &profile_id, &display_name, &reason);
                        unlocked = false;
                    }
                }
                log::info!(
                    "Handing the launch to {}, which starts the game itself{}",
                    launcher.display(),
                    if launch_args.is_empty() {
                        String::new()
                    } else {
                        format!(" (leaving{args_suffix} behind)")
                    }
                );
                let launched =
                    process_utils::launch_game_via_shell(&launcher, &[], launcher.parent())
                        .await
                        .map(|()| None);
                (launched, CUSTOM_LAUNCH_CONFIRM_TIMEOUT)
            }
            None => match via_steam {
                Some((steam_exe, app_id)) => {
                    log::info!("Launching game through Steam (app {app_id}):{args_suffix}");
                    let launched = steam::launch(&steam_exe, &app_id, &launch_args)
                        .await
                        .map(Some)
                        .map_err(|message| process_utils::SpawnError {
                            declined: false,
                            message,
                        });
                    (launched, STEAM_LAUNCH_CONFIRM_TIMEOUT)
                }
                None => match unlock {
                    Some(unlock) => {
                        log::info!(
                            "Launching game through the FPS unlocker: {}{args_suffix}",
                            executable_path.display()
                        );
                        let launched = process_utils::launch_game_via_shell(
                            &unlock.helper,
                            &unlock.arguments,
                            executable_path.parent(),
                        )
                        .await
                        .map(|()| None);
                        (launched, LAUNCH_CONFIRM_TIMEOUT)
                    }
                    None => {
                        let hide_front_end =
                            game_profiles::launches_hidden_front_end(profile, &executable_path);
                        log::info!(
                            "Launching game from: {}{args_suffix}{}",
                            executable_path.display(),
                            if hide_front_end {
                                " (official front end kept hidden)"
                            } else {
                                ""
                            }
                        );
                        let launched = if hide_front_end {
                            process_utils::launch_game_minimized(
                                &executable_path,
                                &launch_args,
                                executable_path.parent(),
                            )
                            .await
                        } else {
                            process_utils::launch_game_via_shell(
                                &executable_path,
                                &launch_args,
                                executable_path.parent(),
                            )
                            .await
                        };
                        if hide_front_end && launched.is_ok() {
                            super::process_utils::front_end::conceal_until_client(
                                game_profiles::hidden_front_end(profile),
                                game_profiles::client_process_name(profile).to_string(),
                            );
                        }
                        (launched.map(|()| None), LAUNCH_CONFIRM_TIMEOUT)
                    }
                },
            },
        };
        let child = match child {
            Ok(child) => child,
            Err(e) => {
                log::error!(
                    "Failed to launch game ({profile_id}): {}{}",
                    e.message,
                    if e.declined { " (permission prompt declined)" } else { "" }
                );
                if unlocked {
                    fps_unlock::end_session(&self.app, &profile_id);
                }
                if mods_armed && !self.has_other_session(&profile_id) {
                    xxmi::kill_orphan_loader().await;
                }
                return err_response(launch_error(&display_name, &e, via_helper));
            }
        };

        self.begin_launch(&profile_id, proc_name, child, confirm_timeout, steam_launch);
        if unlocked {
            fps_unlock::watch(&self.app, &profile_id);
        }
        ok_with(json!({ "viaSteam": steam_launch }))
    }

    fn custom_launcher(&self, profile_id: &str, display_name: &str) -> Option<PathBuf> {
        let Value::String(configured) = self
            .config()
            .get(&format!("games.{profile_id}.customLauncher"))
        else {
            return None;
        };
        let trimmed = configured.trim();
        if trimmed.is_empty() {
            return None;
        }

        let path = PathBuf::from(trimmed);
        if path.is_file() {
            return Some(path);
        }

        log::warn!(
            "{profile_id}: the custom launcher {trimmed} is missing, starting the game directly"
        );
        super::notify::notify_if_backgrounded(
            &self.app,
            "The custom launcher is missing",
            &format!("{trimmed} is no longer there, so {display_name} is starting on its own."),
        );
        let _ = self.app.emit(
            "custom-launcher-missing",
            json!({ "gameId": profile_id, "path": trimmed }),
        );
        None
    }

    fn steam_launch_target(
        &self,
        profile_id: &str,
        install_root: &Path,
        profile: &Value,
    ) -> Option<(PathBuf, String)> {
        if self
            .config()
            .get(&format!("games.{profile_id}.launchViaSteam"))
            == Value::Bool(false)
        {
            if let Some((app_id, signal)) = steam::detect_signal(install_root, profile) {
                log::info!(
                    "{profile_id}: Steam copy detected (app {app_id} via {signal}) but \
                     launchViaSteam is off, launching directly"
                );
            }
            return None;
        }
        let (app_id, signal) = steam::detect_signal(install_root, profile)?;
        log::info!("{profile_id}: Steam copy detected (app {app_id} via {signal})");
        match steam::steam_exe() {
            Some(exe) => Some((exe, app_id)),
            None => {
                log::warn!(
                    "{profile_id} is the Steam build (app {app_id}) but steam.exe could not be \
                     found — launching the game directly, without the Steam overlay."
                );
                None
            }
        }
    }

    fn begin_launch(
        self: &Arc<Self>,
        game_id: &str,
        proc_name: String,
        child: Option<Child>,
        confirm_timeout: Duration,
        via_steam: bool,
    ) {
        {
            let mut monitor = self.monitor.lock();
            if monitor
                .sessions
                .get(game_id)
                .is_some_and(GameSession::is_running)
            {
                log::info!(
                    "{game_id} was already picked up by the Steam update watch, keeping that session"
                );
                return;
            }
            log::info!("Watching for {game_id} process ({proc_name})");
            monitor.sessions.insert(
                game_id.to_string(),
                GameSession {
                    proc_name,
                    pid: None,
                    started_at: None,
                    last_pid_reconfirm: Instant::now(),
                    launch_deadline: child.is_none().then(|| Instant::now() + confirm_timeout),
                    passive: false,
                    via_steam,
                    handed_off: child.is_none(),
                    last_seen_ms: 0,
                    missing_since: None,
                    next_probe: Instant::now(),
                },
            );
            self.ensure_ticker(&mut monitor);
        }
        if let Some(child) = child {
            self.watch_launch_handoff(game_id, child, confirm_timeout, via_steam);
        }
    }

    fn adopt_running(self: &Arc<Self>, game_id: &str, proc_name: String, pid: u32) -> bool {
        let mut monitor = self.monitor.lock();
        if monitor
            .sessions
            .get(game_id)
            .is_some_and(GameSession::is_running)
        {
            return false;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        monitor.sessions.insert(
            game_id.to_string(),
            GameSession {
                proc_name,
                pid: Some(pid),
                started_at: Some(now_ms),
                last_pid_reconfirm: Instant::now(),
                launch_deadline: None,
                passive: false,
                via_steam: false,
                handed_off: true,
                last_seen_ms: now_ms,
                missing_since: None,
                next_probe: Instant::now(),
            },
        );
        self.ensure_ticker(&mut monitor);
        true
    }

    pub fn update_left_unfinished(&self, game_id: &str) -> bool {
        self.config()
            .get(&super::file_channels::update_incomplete_key(game_id))
            .as_bool()
            == Some(true)
    }

    fn watch_for_steam_launch(self: &Arc<Self>, game_id: &str, proc_name: String) {
        let mut monitor = self.monitor.lock();
        if monitor.sessions.contains_key(game_id) {
            return;
        }
        log::info!("Watching for Steam to start {game_id} ({proc_name}) after its update");
        monitor.sessions.insert(
            game_id.to_string(),
            GameSession {
                proc_name,
                pid: None,
                started_at: None,
                last_pid_reconfirm: Instant::now(),
                launch_deadline: Some(Instant::now() + STEAM_UPDATE_WATCH_TIMEOUT),
                passive: true,
                via_steam: true,
                handed_off: true,
                last_seen_ms: 0,
                missing_since: None,
                next_probe: Instant::now(),
            },
        );
        self.ensure_ticker(&mut monitor);
    }

    fn ensure_ticker(self: &Arc<Self>, monitor: &mut Monitor) {
        if monitor.task.is_some() {
            return;
        }
        log::info!("Starting game process monitoring");
        let me = Arc::clone(self);
        monitor.task = Some(tauri::async_runtime::spawn(async move {
            let mut ticker = tokio::time::interval(PROCESS_MONITOR_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if !me.check_game_processes().await {
                    log::info!("Stopped game process monitoring");
                    break;
                }
            }
        }));
    }

    fn watch_launch_handoff(
        self: &Arc<Self>,
        game_id: &str,
        mut child: Child,
        confirm_timeout: Duration,
        via_steam: bool,
    ) {
        let me = Arc::clone(self);
        let game_id = game_id.to_string();
        let spawned = Instant::now();
        tauri::async_runtime::spawn(async move {
            let handed_off = match tokio::time::timeout(LAUNCH_HANDOFF_MAX_WAIT, child.wait()).await
            {
                Ok(Ok(status)) if !status.success() => {
                    log::warn!("Shell hand-off for {game_id} failed ({status})");
                    false
                }
                _ => true,
            };
            let deadline =
                handoff_deadline(spawned, Instant::now(), handed_off, confirm_timeout, via_steam);
            if let Some(session) = me.monitor.lock().sessions.get_mut(&game_id) {
                if !session.is_running() && !session.passive {
                    session.launch_deadline = Some(deadline);
                    session.handed_off = handed_off;
                }
            }
        });
    }

    // ------------ Process Watching ------------
    // A once a second ticker that notices when a game starts, stops or restarts itself, and picks up games that were already running when Peebify opened.
    async fn check_game_processes(self: &Arc<Self>) -> bool {
        struct Probe {
            game_id: String,
            proc_name: String,
            by_name: bool,
            via_steam: bool,
            missing: bool,
        }

        let probes: Vec<Probe> = {
            let mut monitor = self.monitor.lock();
            if monitor.sessions.is_empty() {
                monitor.task = None;
                return false;
            }
            let now = Instant::now();
            monitor
                .sessions
                .iter_mut()
                .filter_map(|(game_id, session)| {
                    if !session.probe_due(now) {
                        return None;
                    }
                    let pid_alive = session.is_running()
                        && session.pid.is_some_and(process_utils::is_pid_alive);
                    let reconfirm_due =
                        session.last_pid_reconfirm.elapsed() >= PID_RECONFIRM_INTERVAL;
                    Some(Probe {
                        game_id: game_id.clone(),
                        proc_name: session.proc_name.clone(),
                        by_name: !pid_alive || reconfirm_due,
                        via_steam: session.via_steam,
                        missing: session.missing_since.is_some(),
                    })
                })
                .collect()
        };
        let held: Vec<String> = probes
            .iter()
            .filter(|probe| probe.missing)
            .map(|probe| probe.game_id.clone())
            .collect();
        self.playtime.tick_holding(&held);

        let names: Vec<String> = probes
            .iter()
            .filter(|probe| probe.by_name)
            .map(|probe| probe.proc_name.clone())
            .collect();
        let Some(found) = process_utils::pids_by_name(&names).await else {
            return true;
        };

        for probe in probes {
            let (seen, pid) = if probe.by_name {
                match found.get(&probe.proc_name.to_lowercase()) {
                    Some(&pid) => (true, Some(pid)),
                    None => (false, None),
                }
            } else {
                (true, None)
            };
            match self.apply_probe(&probe.game_id, seen, pid, probe.by_name) {
                Transition::Started => self.handle_game_started(&probe.game_id).await,
                Transition::Stopped {
                    duration_ms,
                    ended_ms,
                } => {
                    self.handle_game_stopped(&probe.game_id, duration_ms, ended_ms)
                        .await
                }
                Transition::LaunchFailed => {
                    self.handle_launch_failed(&probe.game_id, probe.via_steam)
                        .await
                }
                Transition::SteamStillStarting => {
                    self.handle_steam_still_starting(&probe.game_id).await
                }
                Transition::WatchExpired => {
                    log::info!("Stopped waiting for Steam to start {}", probe.game_id)
                }
                Transition::None => {}
            }
        }
        true
    }

    fn apply_probe(
        &self,
        game_id: &str,
        seen: bool,
        pid: Option<u32>,
        by_name: bool,
    ) -> Transition {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut monitor = self.monitor.lock();
        let Some(session) = monitor.sessions.get_mut(game_id) else {
            return Transition::None;
        };
        let before = session.pid;
        let transition = fold_probe(session, seen, pid, by_name, now_ms, Instant::now());
        let moved = session
            .pid
            .filter(|now| session.is_running() && before != Some(*now));
        if matches!(
            transition,
            Transition::Stopped { .. } | Transition::LaunchFailed | Transition::WatchExpired
        ) {
            monitor.sessions.remove(game_id);
        }
        drop(monitor);
        if let (Transition::None, Some(pid)) = (&transition, moved) {
            self.note_game_process(game_id, pid);
        }
        transition
    }

    fn note_game_process(&self, game_id: &str, pid: u32) {
        let created_ms = process_identity(pid).map(|id| id.created_ms);
        if created_ms.is_none() {
            log::debug!("Could not read when {game_id}'s process (pid {pid}) started");
        }
        self.playtime.set_process(game_id, pid, created_ms);
    }

    async fn handle_game_started(self: &Arc<Self>, game_id: &str) {
        let pid = self.running_pid(game_id);
        log::info!(
            "Game process started ({game_id}, pid {}, also running: {})",
            pid.map_or_else(|| "unknown".to_string(), |p| p.to_string()),
            describe_games(&self.other_running_games(game_id))
        );

        let proc_name = self
            .monitor
            .lock()
            .sessions
            .get(game_id)
            .map(|session| session.proc_name.clone())
            .unwrap_or_default();
        self.playtime.start_tracking(game_id, &proc_name);
        if let Some(pid) = pid {
            self.note_game_process(game_id, pid);
        }
        self.emit_game_event("game-started", json!({ "gameId": game_id, "pid": pid }))
            .await;
        let state = self.app.state::<BackendState>();
        state.overlay.on_game_started(game_id, pid);
        state.window.update_tray_menu();
        state.window.perform_launch_action();
    }

    async fn handle_game_stopped(
        self: &Arc<Self>,
        game_id: &str,
        session_duration: i64,
        ended_ms: i64,
    ) {
        let session_ms = self
            .playtime
            .stop_tracking_at(&self.app, game_id, ended_ms)
            .unwrap_or(session_duration)
            .max(0);
        log::info!(
            "Game process stopped ({game_id}). Session duration: {}s, active: {}s, still running: {}",
            session_duration / 1000,
            session_ms / 1000,
            describe_games(&self.other_running_games(game_id))
        );

        fps_unlock::end_session(&self.app, game_id);
        self.close_exit_companions(game_id).await;
        self.emit_game_event("game-stopped", json!({ "gameId": game_id }))
            .await;
        self.clear_update_cache(game_id);

        let state = self.app.state::<BackendState>();
        match state.overlay.owner() {
            Some(owner) if owner != game_id && self.is_game_running_id(&owner) => {
                log::info!("overlay: staying on {owner} after {game_id} stopped");
            }
            _ => {
                state.overlay.on_game_stopped();
                if let Some(next) = self.latest_running_game() {
                    log::info!("overlay: moving from {game_id} to {next}, which is still running");
                    state.overlay.on_game_started(&next, self.running_pid(&next));
                }
            }
        }
        state.game_updater.on_game_stopped(game_id);
        state.window.update_tray_menu();
        if !self.is_any_game_active() {
            state.window.reopen_after_game();
        }

        if !self.is_any_game_active() {
            xxmi::kill_orphan_loader().await;
            super::xxmi_update::on_game_stopped(&self.app);
            self.app
                .state::<BackendState>()
                .window
                .exit_if_closed_for_game();
        }
    }

    pub fn resume_open_sessions(self: &Arc<Self>) {
        let leftovers = self.playtime.take_leftovers();
        if leftovers.is_empty() {
            return;
        }
        let me = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let names: Vec<String> = leftovers
                .iter()
                .filter(|session| !session.proc_name.is_empty())
                .map(|session| session.proc_name.clone())
                .collect();
            let found = process_utils::pids_by_name(&names).await.unwrap_or_default();
            let mut adopted = Vec::new();
            for session in leftovers {
                let by_name = found.get(&session.proc_name.to_lowercase()).copied();
                let pid = session.resumable_pid(by_name, process_identity);
                if pid.is_none() && by_name.is_some() {
                    log::info!(
                        "{} is running, but not as the process its open session tracked, so that session ends at its last checkpoint.",
                        session.game_id
                    );
                }
                let adoptable = pid.is_some() && {
                    let mut monitor = me.monitor.lock();
                    if monitor.sessions.contains_key(&session.game_id) {
                        false
                    } else {
                        monitor.sessions.insert(
                            session.game_id.clone(),
                            GameSession {
                                proc_name: session.proc_name.clone(),
                                pid,
                                started_at: Some(session.start_ms),
                                last_pid_reconfirm: Instant::now(),
                                launch_deadline: None,
                                passive: false,
                                via_steam: false,
                                handed_off: true,
                                last_seen_ms: chrono::Utc::now().timestamp_millis(),
                                missing_since: None,
                                next_probe: Instant::now(),
                            },
                        );
                        me.ensure_ticker(&mut monitor);
                        true
                    }
                };
                if adoptable {
                    let game_id = session.game_id.clone();
                    me.playtime.adopt(session);
                    if let Some(pid) = pid {
                        me.note_game_process(&game_id, pid);
                    }
                    adopted.push(game_id);
                } else {
                    me.playtime.close_leftover(&me.app, &session);
                }
            }
            me.playtime.checkpoint();
            if !adopted.is_empty() {
                me.resume_started(&adopted).await;
            }
        });
    }

    async fn resume_started(self: &Arc<Self>, adopted: &[String]) {
        for game_id in adopted {
            if !self.is_game_running_id(game_id) {
                continue;
            }
            let pid = self.running_pid(game_id);
            log::info!(
                "Game process picked up again after a restart ({game_id}, pid {})",
                pid.map_or_else(|| "unknown".to_string(), |p| p.to_string())
            );
            self.emit_game_event("game-started", json!({ "gameId": game_id, "pid": pid }))
                .await;
        }
        let state = self.app.state::<BackendState>();
        if state.overlay.owner().is_none() {
            if let Some(id) = self.latest_running_game() {
                state.overlay.on_game_started(&id, self.running_pid(&id));
            }
        }
        state.window.update_tray_menu();
    }

    fn other_running_games(&self, game_id: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .monitor
            .lock()
            .sessions
            .iter()
            .filter(|(id, session)| id.as_str() != game_id && session.is_running())
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    fn latest_running_game(&self) -> Option<String> {
        self.monitor
            .lock()
            .sessions
            .iter()
            .filter_map(|(id, s)| s.started_at.map(|t| (id.clone(), t)))
            .max_by_key(|(_, t)| *t)
            .map(|(id, _)| id)
    }

    async fn close_exit_companions(&self, game_id: &str) {
        let profile = game_profiles::profile(game_id);
        let client = game_profiles::client_process_name(profile).to_lowercase();
        let root = match self.config().get(&format!("games.{game_id}.gamePath")) {
            Value::String(s) if !s.is_empty() => PathBuf::from(s),
            _ => {
                log::debug!("{game_id} has no configured game path, so its exit companions stay");
                return;
            }
        };
        for name in game_profiles::exit_companions(profile) {
            if name.to_lowercase() == client {
                continue;
            }
            let closed = process_utils::terminate_by_name_under(&name, &root).await;
            if closed > 0 {
                log::info!("Closed {closed} leftover {name} process(es) after {game_id} exited.");
            }
        }
    }

    async fn handle_launch_failed(self: &Arc<Self>, game_id: &str, via_steam: bool) {
        log::warn!("Launch of {game_id} never produced a running process");
        let reason = if via_steam {
            STEAM_LAUNCH_NEVER_STARTED
        } else {
            LAUNCH_NEVER_STARTED
        };
        fps_unlock::end_session(&self.app, game_id);
        if !self.is_any_game_active() {
            xxmi::kill_orphan_loader().await;
        }
        self.emit_game_event(
            "game-launch-failed",
            json!({
                "gameId": game_id,
                "reason": reason,
            }),
        )
        .await;
        let profile = game_profiles::profile(game_id);
        super::notify::notify_if_backgrounded(
            &self.app,
            &format!("{} did not start", game_profiles::display_name(profile)),
            reason,
        );
        self.app.state::<BackendState>().window.update_tray_menu();
    }

    async fn handle_steam_still_starting(self: &Arc<Self>, game_id: &str) {
        log::warn!(
            "Steam hasn't started {game_id} yet, watching for it for up to {} h",
            STEAM_UPDATE_WATCH_TIMEOUT.as_secs() / 3600
        );
        fps_unlock::end_session(&self.app, game_id);
        if !self.is_any_game_active() {
            xxmi::kill_orphan_loader().await;
        }
        self.emit_game_event(
            "game-launch-waiting",
            json!({
                "gameId": game_id,
                "reason": STEAM_LAUNCH_STILL_WAITING,
            }),
        )
        .await;
        let profile = game_profiles::profile(game_id);
        super::notify::notify_if_backgrounded(
            &self.app,
            &format!("{} hasn't started yet", game_profiles::display_name(profile)),
            STEAM_LAUNCH_STILL_WAITING,
        );
        self.app.state::<BackendState>().window.update_tray_menu();
    }

    // ------------ Update Checks ------------
    // Asks each game's server for its latest version and compares it with what is installed, caching answers briefly so the UI does not hammer the servers.
    pub async fn check_for_updates(
        &self,
        force_check: bool,
        target_game_id: Option<&str>,
        reason: &str,
    ) -> Value {
        let game_id = self.resolve_game_id(target_game_id);
        let profile = game_profiles::profile(&game_id);
        let profile_id = game_profiles::profile_id(profile).to_string();

        if !game_profiles::is_managed(profile) {
            return ok_with(json!({
                "gameId": profile_id,
                "updateAvailable": false,
                "currentVersion": null,
                "latestVersion": null,
            }));
        }

        if !force_check {
            if let Some((cached, at)) = self.cached_update_entry(&profile_id) {
                log::debug!(
                    "Using cached update check result for {profile_id} ({reason}, {}s old)",
                    at.elapsed().as_secs()
                );
                return ok_with(cached);
            }
        }

        if !http::is_online_cached() {
            log::info!("No internet connection, skipping game update check ({profile_id}, {reason}).");
            return err_response("No internet connection.");
        }

        let requested_at = Instant::now();
        let flight = self.update_lock(&profile_id);
        let _flight = flight.lock().await;
        if let Some((cached, at)) = self.cached_update_entry(&profile_id) {
            if reuse_cached_check(force_check, at, requested_at) {
                log::debug!(
                    "Reusing the update check for {profile_id} that finished while this one waited ({reason})"
                );
                return ok_with(cached);
            }
        }

        let started = Instant::now();
        let game_path = self.config().get(&format!("games.{profile_id}.gamePath"));
        let local_version = local_game_version_for(profile, game_path.as_str().unwrap_or(""));
        let local_label = local_version_label(
            local_version.as_deref(),
            game_path.as_str().is_some_and(|p| !p.is_empty()),
        );
        let feed = game_profiles::install_mode(profile).unwrap_or("game config");
        log::debug!("Checking for updates ({profile_id}, {reason}). Local version: {local_label}");

        let game_config = match fetch_game_config(profile, force_check).await {
            Ok(config) => config,
            Err(e) => {
                log::warn!("Game update check failed ({game_id}, {reason}, via {feed}): {e}");
                return err_response(e);
            }
        };

        let raw_remote = game_config["default"]
            .get("version")
            .filter(|v| !v.is_null())
            .or_else(|| game_config.get("version"))
            .and_then(|v| match v {
                Value::String(s) => Some(s.trim().to_string()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .or_else(|| super::download_engine::bundle_config_version(&game_config));
        let Some(remote_version) = raw_remote.as_deref() else {
            let msg = "No default game configuration found in remote.";
            log::error!("Game update check failed ({game_id}, {reason}, via {feed}): {msg}");
            return err_response(msg);
        };

        let update_available = local_version
            .as_deref()
            .map(|local| is_version_newer(remote_version, local))
            .unwrap_or(false)
            || (local_version.is_some()
                && gf2_client_update_pending(
                    profile,
                    &game_config,
                    game_path.as_str().unwrap_or(""),
                ));

        let steam_app_id = self.steam_app_id(&profile_id, profile).await;
        if update_available && steam_app_id.is_some() {
            log::info!(
                "{profile_id} is a Steam copy (app {}) — Steam owns this update.",
                steam_app_id.as_deref().unwrap_or("?")
            );
        }

        let result = json!({
            "gameId": profile_id,
            "updateAvailable": update_available,
            "currentVersion": local_version,
            "latestVersion": remote_version,
            "steamManaged": steam_app_id.is_some(),
        });
        self.update_cache
            .lock()
            .insert(profile_id.clone(), (result.clone(), Instant::now()));

        log::info!(
            "Update check ({profile_id}, {reason}): local {local_label}, remote {remote_version} via {feed}, update available: {update_available}, {} ms",
            started.elapsed().as_millis()
        );
        ok_with(result)
    }

    pub async fn update_via_steam(self: &Arc<Self>, target_game_id: Option<&str>) -> Value {
        let game_id = self.resolve_game_id(target_game_id);
        let profile = game_profiles::profile(&game_id);
        let profile_id = game_profiles::profile_id(profile).to_string();
        let display_name = game_profiles::display_name(profile);

        let Some(app_id) = self.steam_app_id(&profile_id, profile).await else {
            return err_response(format!("{display_name} is not a Steam installation."));
        };
        let Some(steam_exe) = steam::steam_exe() else {
            return err_response(
                "Steam couldn't be found on this PC. Open Steam and update the game from your library.",
            );
        };

        log::info!("Handing the {profile_id} update to Steam (app {app_id}).");
        match steam::request_update(&steam_exe, &app_id).await {
            Ok(_) => {
                self.clear_update_cache(&profile_id);
                self.watch_for_steam_launch(&profile_id, client_process_name(profile));
                ok_with(json!({ "gameId": profile_id, "appId": app_id }))
            }
            Err(e) => {
                log::error!("Failed to hand the {profile_id} update to Steam: {e}");
                err_response(format!("Couldn't start Steam: {e}"))
            }
        }
    }

    fn cached_update_entry(&self, game_id: &str) -> Option<(Value, Instant)> {
        let cache = self.update_cache.lock();
        let (result, at) = cache.get(game_id)?;
        (at.elapsed() < UPDATE_CACHE_TIMEOUT).then(|| (result.clone(), *at))
    }

    fn update_lock(&self, game_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        flight_lock(&self.update_locks, game_id)
    }

    pub fn clear_update_cache(&self, game_id: &str) {
        self.update_cache.lock().remove(game_id);
    }
}

fn fold_probe(
    session: &mut GameSession,
    seen: bool,
    pid: Option<u32>,
    by_name: bool,
    now_ms: i64,
    now: Instant,
) -> Transition {
    if by_name {
        session.last_pid_reconfirm = now;
        session.pid = pid;
    }
    match (session.started_at, seen) {
        (None, true) => {
            session.started_at = Some(now_ms);
            session.last_seen_ms = now_ms;
            Transition::Started
        }
        (Some(_), true) => {
            if session.missing_since.take().is_some() {
                log::info!("{} is back, keeping its session", session.proc_name);
            }
            session.last_seen_ms = now_ms;
            Transition::None
        }
        (Some(start), false) => {
            let since = *session.missing_since.get_or_insert_with(|| {
                log::info!(
                    "{} is gone, waiting {}s in case it restarts itself",
                    session.proc_name,
                    MISSING_GRACE.as_secs()
                );
                now
            });
            if now.saturating_duration_since(since) < MISSING_GRACE {
                return Transition::None;
            }
            let ended_ms = session.last_seen_ms.max(start);
            Transition::Stopped {
                duration_ms: ended_ms - start,
                ended_ms,
            }
        }
        (None, false)
            if session
                .launch_deadline
                .is_some_and(|deadline| now >= deadline) =>
        {
            if session.passive {
                Transition::WatchExpired
            } else if session.via_steam && session.handed_off {
                session.passive = true;
                session.launch_deadline = Some(now + STEAM_UPDATE_WATCH_TIMEOUT);
                Transition::SteamStillStarting
            } else {
                Transition::LaunchFailed
            }
        }
        _ => Transition::None,
    }
}

fn launch_error(display_name: &str, error: &process_utils::SpawnError, via_helper: bool) -> String {
    match (error.declined, via_helper) {
        (true, true) => format!(
            "You declined the Windows prompt for the FPS unlocker, so {display_name} didn't start."
        ),
        (true, false) => format!("You declined the Windows prompt, so {display_name} didn't start."),
        (false, _) => format!("Failed to launch: {}", error.message),
    }
}

fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const UNIX_EPOCH_MS: i64 = 11_644_473_600_000;

    if pid == 0 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut code: u32 = 0;
        let alive = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE as u32;
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        let image = (QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            buf.as_mut_ptr(),
            &mut len,
        ) != 0)
            .then(|| String::from_utf16_lossy(&buf[..len as usize]));
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        let timed =
            GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0;
        CloseHandle(handle);
        if !alive || !timed {
            return None;
        }
        let image = Path::new(&image?).file_name()?.to_string_lossy().to_string();
        let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        Some(ProcessIdentity {
            image,
            created_ms: (ticks / 10_000) as i64 - UNIX_EPOCH_MS,
        })
    }
}

fn handoff_deadline(
    spawned: Instant,
    now: Instant,
    handed_off: bool,
    confirm_timeout: Duration,
    via_steam: bool,
) -> Instant {
    let deadline = now
        + if handed_off {
            confirm_timeout
        } else {
            LAUNCH_FAILED_GRACE
        };
    if via_steam {
        deadline.min(spawned + LAUNCH_HANDOFF_MAX_WAIT)
    } else {
        deadline
    }
}

fn client_process_name(profile: &Value) -> String {
    game_profiles::client_process_name(profile).to_string()
}

fn split_launch_args(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in input.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

// ------------ Version Helpers ------------
// Reads the installed version off disk, which differs by game (a config file, an ini, a manifest), and compares version strings.
pub(super) fn local_game_version(game_path: &str) -> Option<String> {
    if game_path.is_empty() {
        return None;
    }
    local_game_version_at(Path::new(game_path))
}

pub(super) fn local_game_version_at(install_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(install_path.join(GAME_CONFIG_FILE)).ok()?;
    let config: Value = serde_json::from_str(&text).ok()?;
    match &config["version"] {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub(super) fn local_game_version_for(profile: &Value, game_path: &str) -> Option<String> {
    if !game_path.is_empty() && game_profiles::install_mode(profile) == Some("bluepoch") {
        return super::bluepoch::installed_version(Path::new(game_path))
            .or_else(|| local_game_version(game_path));
    }
    if let Some(version) = local_game_version(game_path) {
        return Some(version);
    }
    if game_path.is_empty() {
        return None;
    }
    if game_profiles::install_mode(profile) == Some("bd2") {
        return super::bd2::installed_version(Path::new(game_path));
    }
    if game_profiles::install_mode(profile) == Some("hypergryph") {
        return super::hypergryph_reconcile::load_manifest(Path::new(game_path))
            .map(|manifest| manifest.version);
    }
    if game_profiles::install_mode(profile) == Some("sophon") {
        return sophon::applied_tag(Path::new(game_path)).or_else(|| {
            std::fs::read_to_string(Path::new(game_path).join("config.ini"))
                .ok()
                .and_then(|text| hoyoplay_game_version(&text))
        });
    }
    None
}

fn gf2_client_update_pending(profile: &Value, game_config: &Value, game_path: &str) -> bool {
    if game_path.is_empty() || game_profiles::install_mode(profile) != Some("gf2") {
        return false;
    }
    game_config["clientVersion"]
        .as_str()
        .filter(|v| !v.is_empty())
        .is_some_and(|client| super::gf2::client_update_pending(Path::new(game_path), client))
}

fn hoyoplay_game_version(ini: &str) -> Option<String> {
    let mut section = String::new();
    for line in ini.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_ascii_lowercase();
            continue;
        }
        if !(section.is_empty() || section == "general") {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("game_version") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn busy_launch_message(display_name: &str, op: &str) -> String {
    let doing = match op {
        "repair" => "being repaired",
        "move" => "being moved",
        _ => "being updated",
    };
    format!("{display_name} is {doing}. Launch it when that finishes.")
}

fn describe_games(ids: &[String]) -> String {
    if ids.is_empty() {
        "none".to_string()
    } else {
        ids.join(", ")
    }
}

fn flight_lock(
    locks: &Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    game_id: &str,
) -> Arc<tokio::sync::Mutex<()>> {
    Arc::clone(locks.lock().entry(game_id.to_string()).or_default())
}

fn reuse_cached_check(force_check: bool, cached_at: Instant, requested_at: Instant) -> bool {
    !force_check || cached_at >= requested_at
}

fn local_version_label(local_version: Option<&str>, has_game_path: bool) -> String {
    match local_version {
        Some(version) => version.to_string(),
        None if has_game_path => "unknown (no version file)".to_string(),
        None => "not installed".to_string(),
    }
}

pub(super) fn is_version_newer(remote: &str, local: &str) -> bool {
    if remote.is_empty() || local.is_empty() {
        return false;
    }
    let parse = |v: &str| -> Vec<i64> {
        v.trim()
            .split('.')
            .map(|p| p.parse::<i64>().unwrap_or(0))
            .collect()
    };
    let r = parse(remote);
    let l = parse(local);
    for i in 0..r.len().max(l.len()) {
        let rv = r.get(i).copied().unwrap_or(0);
        let lv = l.get(i).copied().unwrap_or(0);
        if rv > lv {
            return true;
        }
        if rv < lv {
            return false;
        }
    }
    false
}

async fn fetch_game_config(profile: &Value, force_check: bool) -> Result<Value, String> {
    match game_profiles::InstallMode::of(profile) {
        game_profiles::InstallMode::Sophon => {
            let auth = sophon::cached_branch_auth_for_profile(profile).await?;
            if !auth.tag.is_empty() {
                return Ok(json!({ "version": auth.tag }));
            }
            let build = sophon::fetch_build(&auth).await?;
            Ok(json!({ "version": build.tag }))
        }
        game_profiles::InstallMode::Netease => {
            let config = super::nte::fetch_config(profile).await?;
            Ok(json!({ "version": config.res_version }))
        }
        game_profiles::InstallMode::Gf2 => {
            let versions = super::gf2::fetch_versions_with(profile, force_check).await?;
            Ok(json!({
                "version": versions.ab_version,
                "clientVersion": versions.client_version,
            }))
        }
        game_profiles::InstallMode::Bluepoch => {
            let latest = super::bluepoch::latest_version(profile).await?;
            Ok(json!({ "version": latest }))
        }
        game_profiles::InstallMode::Bd2 => {
            let version = super::bd2::fetch_version(profile).await?;
            Ok(json!({ "version": version }))
        }
        game_profiles::InstallMode::Hypergryph => {
            let latest = super::hypergryph::get_latest_game(profile).await?;
            Ok(json!({
                "version": latest.get("version").cloned().unwrap_or(Value::Null),
            }))
        }
        game_profiles::InstallMode::Default | game_profiles::InstallMode::Unknown => {
            let url = profile
                .get("gameConfigUrl")
                .and_then(|v| v.as_str())
                .ok_or("no gameConfigUrl for profile")?;
            log::debug!("Fetching game config from: {url}");
            http::get_json(url).await
        }
    }
}

// ------------ Command Handlers ------------
// The thin functions the UI's launch, running game, Steam and update check commands call into.
fn game(app: &AppHandle) -> Arc<GameManager> {
    app.state::<BackendState>().game.clone()
}

pub(super) async fn launch_game(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let target = args.first().and_then(|v| v.as_str()).map(str::to_string);
    Ok(game(app).launch_game(target.as_deref()).await)
}

pub(super) async fn get_running_game(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let manager = game(app);
    let Some(id) = super::arg_str(args, 0) else {
        return Ok(running_game_reply(&manager.running_games()));
    };
    Ok(super::ok_with(json!({ "running": manager.is_game_running_id(id) })))
}

fn running_game_reply(ids: &[String]) -> Value {
    super::ok_with(json!({
        "running": !ids.is_empty(),
        "runningIds": ids,
    }))
}

pub(super) async fn get_steam_install(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let target = args.first().and_then(|v| v.as_str()).map(str::to_string);
    Ok(game(app).steam_install_info(target.as_deref()).await)
}

pub(super) async fn update_via_steam(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let target = args.first().and_then(|v| v.as_str()).map(str::to_string);
    Ok(game(app).update_via_steam(target.as_deref()).await)
}

pub(super) async fn check_for_updates(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let target = args.first().and_then(|v| v.as_str()).map(str::to_string);
    let force_check = args.get(1).and_then(Value::as_bool).unwrap_or(false);
    let reason =
        super::arg_str(args, 2).unwrap_or(if force_check { "manual" } else { "ui" });
    Ok(game(app)
        .check_for_updates(force_check, target.as_deref(), reason)
        .await)
}

// ------------ Game Manager Tests ------------
// Covers session tracking, launch hand offs and version checks.
#[cfg(test)]
mod tests {
    use super::*;

    fn session(passive: bool, deadline: Option<Instant>) -> GameSession {
        GameSession {
            proc_name: "Client-Win64-Shipping.exe".to_string(),
            pid: None,
            started_at: None,
            last_pid_reconfirm: Instant::now(),
            launch_deadline: deadline,
            passive,
            via_steam: false,
            handed_off: true,
            last_seen_ms: 0,
            missing_since: None,
            next_probe: Instant::now(),
        }
    }

    #[test]
    fn running_game_reply_lists_every_running_id() {
        let none = running_game_reply(&[]);
        assert_eq!(none["running"], false);
        assert_eq!(none["runningIds"], json!([]));

        let games = ["wuwa".to_string(), "zzz".to_string()];
        let both = running_game_reply(&games);
        assert_eq!(both["success"], true);
        assert_eq!(both["running"], true);
        assert_eq!(both["runningIds"], json!(["wuwa", "zzz"]));
    }

    #[test]
    fn only_an_idle_passive_watch_skips_ticks() {
        let now = Instant::now();
        let mut launch = session(false, Some(now + Duration::from_secs(60)));
        assert!(launch.probe_due(now));
        assert!(launch.probe_due(now));

        let mut watch = session(true, Some(now + STEAM_UPDATE_WATCH_TIMEOUT));
        watch.next_probe = now;
        assert!(watch.probe_due(now));
        assert!(!watch.probe_due(now + Duration::from_secs(1)));
        assert!(watch.probe_due(now + PASSIVE_WATCH_PROBE_INTERVAL));

        watch.started_at = Some(1_000);
        assert!(watch.probe_due(now + PASSIVE_WATCH_PROBE_INTERVAL + Duration::from_secs(1)));
    }

    #[test]
    fn a_game_that_restarts_itself_keeps_its_session() {
        let now = Instant::now();
        let mut game = session(false, None);
        assert!(matches!(
            fold_probe(&mut game, true, Some(7), true, 1_000, now),
            Transition::Started
        ));
        assert!(matches!(
            fold_probe(&mut game, true, None, false, 60_000, now),
            Transition::None
        ));
        let gone = now + Duration::from_secs(1);
        assert!(matches!(
            fold_probe(&mut game, false, None, true, 61_000, gone),
            Transition::None
        ));
        assert!(matches!(
            fold_probe(&mut game, false, None, true, 66_000, gone + Duration::from_secs(5)),
            Transition::None
        ));
        assert!(matches!(
            fold_probe(&mut game, true, Some(8), true, 67_000, gone + Duration::from_secs(6)),
            Transition::None
        ));
        assert!(game.missing_since.is_none());
        assert_eq!(game.pid, Some(8));
        assert_eq!(game.last_seen_ms, 67_000);
    }

    #[test]
    fn a_closed_game_ends_when_it_was_last_seen() {
        let now = Instant::now();
        let mut game = session(false, None);
        fold_probe(&mut game, true, Some(7), true, 1_000, now);
        fold_probe(&mut game, true, None, false, 60_000, now);
        let gone = now + Duration::from_secs(1);
        fold_probe(&mut game, false, None, true, 61_000, gone);
        assert!(matches!(
            fold_probe(&mut game, false, None, true, 71_000, gone + MISSING_GRACE),
            Transition::Stopped {
                duration_ms: 59_000,
                ended_ms: 60_000,
            }
        ));
    }

    #[test]
    fn a_slow_steam_start_becomes_a_background_watch() {
        let now = Instant::now();
        let mut launch = session(false, Some(now));
        launch.via_steam = true;
        assert!(matches!(
            fold_probe(&mut launch, false, None, true, 0, now),
            Transition::SteamStillStarting
        ));
        assert!(launch.passive);
        assert!(!launch.counts_as_active());
        assert_eq!(launch.launch_deadline, Some(now + STEAM_UPDATE_WATCH_TIMEOUT));
        assert!(matches!(
            fold_probe(&mut launch, true, Some(9), true, 1_000, now),
            Transition::Started
        ));
    }

    #[test]
    fn a_failed_steam_hand_off_still_fails() {
        let now = Instant::now();
        let mut launch = session(false, Some(now));
        launch.via_steam = true;
        launch.handed_off = false;
        assert!(matches!(
            fold_probe(&mut launch, false, None, true, 0, now),
            Transition::LaunchFailed
        ));
    }

    #[test]
    fn a_declined_prompt_is_named_in_the_launch_error() {
        let declined = process_utils::SpawnError {
            declined: true,
            message: "The operation was canceled by the user.".to_string(),
        };
        assert_eq!(
            launch_error("Genshin Impact", &declined, false),
            "You declined the Windows prompt, so Genshin Impact didn't start."
        );
        assert!(launch_error("Genshin Impact", &declined, true).contains("FPS unlocker"));
        let failed = process_utils::SpawnError {
            declined: false,
            message: "not found".to_string(),
        };
        assert_eq!(launch_error("ZZZ", &failed, true), "Failed to launch: not found");
    }

    #[test]
    fn a_quick_steam_hand_off_keeps_the_steam_confirm_window() {
        let spawned = Instant::now();
        let now = spawned + Duration::from_secs(2);
        assert_eq!(
            handoff_deadline(spawned, now, true, STEAM_LAUNCH_CONFIRM_TIMEOUT, true),
            now + STEAM_LAUNCH_CONFIRM_TIMEOUT
        );
    }

    #[test]
    fn a_cold_steam_start_becomes_a_background_watch_at_the_hand_off_cap() {
        let spawned = Instant::now();
        let now = spawned + LAUNCH_HANDOFF_MAX_WAIT;
        let deadline = handoff_deadline(spawned, now, true, STEAM_LAUNCH_CONFIRM_TIMEOUT, true);
        assert_eq!(deadline, spawned + LAUNCH_HANDOFF_MAX_WAIT);
        let mut launch = session(false, Some(deadline));
        launch.via_steam = true;
        assert!(matches!(
            fold_probe(&mut launch, false, None, true, 0, now),
            Transition::SteamStillStarting
        ));
        assert!(launch.passive);
        let slow = spawned + Duration::from_secs(7 * 60);
        assert_eq!(
            handoff_deadline(spawned, slow, true, STEAM_LAUNCH_CONFIRM_TIMEOUT, true),
            spawned + LAUNCH_HANDOFF_MAX_WAIT
        );
    }

    #[test]
    fn direct_launches_keep_the_full_confirm_window_after_the_hand_off() {
        let spawned = Instant::now();
        let now = spawned + LAUNCH_HANDOFF_MAX_WAIT;
        assert_eq!(
            handoff_deadline(spawned, now, true, LAUNCH_CONFIRM_TIMEOUT, false),
            now + LAUNCH_CONFIRM_TIMEOUT
        );
    }

    #[test]
    fn a_failed_hand_off_fails_after_the_grace_period() {
        let spawned = Instant::now();
        let now = spawned + Duration::from_secs(1);
        assert_eq!(
            handoff_deadline(spawned, now, false, STEAM_LAUNCH_CONFIRM_TIMEOUT, true),
            now + LAUNCH_FAILED_GRACE
        );
        assert_eq!(
            handoff_deadline(spawned, now, false, LAUNCH_CONFIRM_TIMEOUT, false),
            now + LAUNCH_FAILED_GRACE
        );
    }

    #[test]
    fn passive_watch_expires_quietly_instead_of_failing() {
        let now = Instant::now();
        let mut watch = session(true, Some(now));
        assert!(matches!(
            fold_probe(&mut watch, false, None, true, 0, now),
            Transition::WatchExpired
        ));
        let mut launch = session(false, Some(now));
        assert!(matches!(
            fold_probe(&mut launch, false, None, true, 0, now),
            Transition::LaunchFailed
        ));
    }

    #[test]
    fn passive_watch_adopts_the_game_when_it_appears() {
        let now = Instant::now();
        let mut watch = session(true, Some(now + STEAM_UPDATE_WATCH_TIMEOUT));
        assert!(!watch.counts_as_active());
        assert!(matches!(
            fold_probe(&mut watch, false, None, true, 0, now),
            Transition::None
        ));
        assert!(matches!(
            fold_probe(&mut watch, true, Some(42), true, 1_000, now),
            Transition::Started
        ));
        assert!(watch.is_running());
        assert!(watch.counts_as_active());
        assert_eq!(watch.pid, Some(42));
    }

    #[test]
    fn launched_sessions_count_as_active_before_the_game_appears() {
        assert!(session(false, None).counts_as_active());
    }

    #[test]
    fn hoyoplay_version_comes_from_the_general_section() {
        let ini = "\u{feff}[general]\r\nchannel=1\r\ngame_version = 5.1.0 \r\n[other]\r\ngame_version=9.9.9\r\n";
        assert_eq!(hoyoplay_game_version(ini), Some("5.1.0".to_string()));
    }

    #[test]
    fn hoyoplay_version_ignores_empty_and_foreign_sections() {
        assert_eq!(hoyoplay_game_version("[general]\ngame_version=\n"), None);
        assert_eq!(hoyoplay_game_version("[launcher]\ngame_version=1.0\n"), None);
        assert_eq!(hoyoplay_game_version("GAME_VERSION=2.0"), Some("2.0".to_string()));
        assert_eq!(hoyoplay_game_version(""), None);
    }

    #[test]
    fn bluepoch_version_prefers_the_game_ini_over_the_launcher_json() {
        let dir = std::env::temp_dir().join(format!(
            "peebify-bluepoch-version-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_string_lossy().to_string();
        let profile = serde_json::json!({ "installMode": "bluepoch" });

        std::fs::write(dir.join(GAME_CONFIG_FILE), r#"{"version":"2.0.0"}"#).unwrap();
        assert_eq!(local_game_version_for(&profile, &path), Some("2.0.0".to_string()));

        super::super::bluepoch::record_version(&dir, "2.1.0").unwrap();
        assert_eq!(local_game_version_for(&profile, &path), Some("2.1.0".to_string()));

        let other = serde_json::json!({ "installMode": "bd2" });
        assert_eq!(local_game_version_for(&other, &path), Some("2.0.0".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn busy_launch_message_names_the_blocking_op() {
        assert_eq!(
            busy_launch_message("Genshin Impact", "download"),
            "Genshin Impact is being updated. Launch it when that finishes."
        );
        assert!(busy_launch_message("ZZZ", "repair").contains("being repaired"));
        assert!(busy_launch_message("ZZZ", "move").contains("being moved"));
    }

    #[test]
    fn forced_checks_reuse_only_results_that_finished_while_waiting() {
        let requested = Instant::now();
        let earlier = requested - Duration::from_secs(30);
        let later = requested + Duration::from_millis(5);
        assert!(reuse_cached_check(false, earlier, requested));
        assert!(!reuse_cached_check(true, earlier, requested));
        assert!(reuse_cached_check(true, later, requested));
        assert!(reuse_cached_check(true, requested, requested));
    }

    #[test]
    fn local_version_label_tells_missing_file_from_not_installed() {
        assert_eq!(local_version_label(Some("7.0.0"), true), "7.0.0");
        assert_eq!(local_version_label(None, true), "unknown (no version file)");
        assert_eq!(local_version_label(None, false), "not installed");
    }

    #[test]
    fn describe_games_lists_ids_or_none() {
        assert_eq!(describe_games(&[]), "none");
        assert_eq!(
            describe_games(&["hsr".to_string(), "zzz".to_string()]),
            "hsr, zzz"
        );
    }

    #[tokio::test]
    async fn concurrent_update_checks_share_one_lock_per_game() {
        let locks = Mutex::new(HashMap::new());
        let get = |id: &str| flight_lock(&locks, id);
        let first = get("zzz");
        let held = first.lock().await;
        assert!(get("zzz").try_lock().is_err());
        assert!(get("hsr").try_lock().is_ok());
        drop(held);
        assert!(get("zzz").try_lock().is_ok());
    }
}
