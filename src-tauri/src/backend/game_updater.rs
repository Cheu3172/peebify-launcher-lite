// ------------ Game Auto Updater ------------
// Checks once an hour whether a game is due for its daily or weekly update check, and queues the update if auto update is on. If it is off, it only sends a notification.
// It waits for the game to be closed first, and does a first sweep shortly after Peebify starts.
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::state::BackendState;
use super::{game_profiles, http, process_utils};

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);
const STARTUP_DELAY: std::time::Duration = std::time::Duration::from_secs(20);
const POST_CLOSE_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
const PROCESS_WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

const BUSY_RUNNING: &str = "running";
const BUSY_LAUNCHING: &str = "launching";
const BUSY_EXTERNAL: &str = "external";
const BUSY_OTHER_SESSION: &str = "session";

pub(super) async fn busy_reason(app: &AppHandle, game_id: &str) -> Option<&'static str> {
    let game = app.state::<BackendState>().game.clone();
    if game.is_game_running_id(game_id) {
        return Some(BUSY_RUNNING);
    }
    if game.is_game_active_id(game_id) {
        return Some(BUSY_LAUNCHING);
    }
    let proc_name = game_profiles::client_process_name(game_profiles::profile(game_id));
    if !proc_name.is_empty() && process_utils::is_process_running(proc_name).await {
        return Some(BUSY_EXTERNAL);
    }
    if game.is_any_game_active() {
        return Some(BUSY_OTHER_SESSION);
    }
    None
}

fn replay_reason(reason: &str) -> &'static str {
    if reason.starts_with("startup") {
        "startup-deferred"
    } else {
        "deferred"
    }
}

fn force_update_check(reason: &str) -> bool {
    !matches!(reason, "startup" | "startup-online")
}

pub struct GameUpdater {
    app: AppHandle,
    sweeping: AtomicBool,
    startup_skipped: AtomicBool,
    deferred: Mutex<HashMap<String, &'static str>>,
    watching: Mutex<HashSet<String>>,
}

impl GameUpdater {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app,
            sweeping: AtomicBool::new(false),
            startup_skipped: AtomicBool::new(false),
            deferred: Mutex::new(HashMap::new()),
            watching: Mutex::new(HashSet::new()),
        })
    }

    fn config(&self) -> Arc<super::config::LauncherConfig> {
        self.app.state::<BackendState>().config.clone()
    }

    fn game_key(&self, game_id: &str, key: &str) -> Value {
        self.config().get(&format!("games.{game_id}.{key}"))
    }

    fn auto_update_enabled(&self, game_id: &str) -> bool {
        self.game_key(game_id, "autoUpdate") != Value::Bool(false)
    }

    fn any_enabled(&self) -> bool {
        game_profiles::GAME_IDS
            .iter()
            .any(|id| self.auto_update_enabled(id))
    }

    async fn already_queued(&self, game_id: &str) -> bool {
        let queue = self.app.state::<BackendState>().engine.queue.clone();
        queue
            .current_meta()
            .map(|m| m.game_id == game_id)
            .unwrap_or(false)
            || queue.has_pending(game_id, None)
            || queue.has_parked(game_id)
    }

    fn defer(self: &Arc<Self>, game_id: &str, busy: &str, reason: &str) {
        let name = game_profiles::display_name(game_profiles::profile(game_id));
        match busy {
            BUSY_RUNNING => log::info!(
                "Auto-update: {name} is running, the update will retry when it closes ({reason})."
            ),
            BUSY_EXTERNAL => {
                log::info!(
                    "Auto-update: {name} is running, the update will retry when it closes ({reason})."
                );
                self.watch_until_closed(game_id, reason);
            }
            BUSY_LAUNCHING => {
                log::info!("Auto-update: {name} launch in progress, deferring update ({reason}).");
                self.deferred
                    .lock()
                    .insert(game_id.to_string(), replay_reason(reason));
            }
            _ => {
                log::info!("Auto-update: a game session is running, deferring {name} ({reason}).");
                self.deferred
                    .lock()
                    .insert(game_id.to_string(), replay_reason(reason));
            }
        }
    }

    fn watch_until_closed(self: &Arc<Self>, game_id: &str, reason: &str) {
        if !self.watching.lock().insert(game_id.to_string()) {
            return;
        }
        let me = Arc::clone(self);
        let game_id = game_id.to_string();
        let replay = replay_reason(reason);
        tauri::async_runtime::spawn(async move {
            let proc_name = game_profiles::client_process_name(game_profiles::profile(&game_id));
            loop {
                tokio::time::sleep(PROCESS_WATCH_INTERVAL).await;
                if !process_utils::is_process_running(proc_name).await {
                    break;
                }
            }
            me.watching.lock().remove(&game_id);
            me.maybe_update_game(&game_id, replay).await;
        });
    }

    async fn maybe_update_game(self: &Arc<Self>, game_id: &str, reason: &str) {
        if !self.auto_update_enabled(game_id) {
            return;
        }
        if reason.starts_with("startup")
            && self.game_key(game_id, "autoUpdateOnStartup") == Value::Bool(false)
        {
            return;
        }
        let profile = game_profiles::profile(game_id);
        if !game_profiles::is_managed(profile) {
            return;
        }
        let game_path = self.game_key(game_id, "gamePath");
        if game_path.as_str().map(str::is_empty).unwrap_or(true) {
            return;
        }
        if self.already_queued(game_id).await {
            log::info!("Auto-update: {game_id} already downloading/queued — skipping ({reason}).");
            return;
        }
        if !http::is_online_cached() {
            return;
        }
        if let Some(busy) = busy_reason(&self.app, game_id).await {
            self.defer(game_id, busy, reason);
            return;
        }
        self.deferred.lock().remove(game_id);

        let state = self.app.state::<BackendState>();
        let result = state
            .game
            .check_for_updates(force_update_check(reason), Some(game_id), reason)
            .await;
        if result["success"] != Value::Bool(true) {
            return;
        }
        let stamp_key = format!("games.{game_id}.lastAutoUpdateCheck");
        let previous_stamp = self.config().get(&stamp_key);
        super::config_channels::set_config_value(
            &self.app,
            &stamp_key,
            json!(chrono::Utc::now().timestamp_millis()),
        );
        if result["updateAvailable"] != Value::Bool(true) {
            return;
        }
        let _ = self.app.emit("update-available", result.clone());
        if result["steamManaged"] == Value::Bool(true) {
            log::info!(
                "Auto-update: {} is a Steam copy, leaving the update to Steam ({reason}).",
                game_profiles::display_name(profile)
            );
            return;
        }
        log::info!(
            "Auto-update: {} {} -> {}, queuing download ({reason}).",
            game_profiles::display_name(profile),
            result["currentVersion"].as_str().unwrap_or("?"),
            result["latestVersion"].as_str().unwrap_or("?")
        );
        let me = Arc::clone(self);
        let game_id = game_id.to_string();
        let reason = reason.to_string();
        tauri::async_runtime::spawn(async move {
            let args = [json!({ "gameId": game_id, "versionType": "default", "deferIfBusy": true })];
            match super::file_channels::start_download(&me.app, &args).await {
                Ok(outcome) => {
                    if let Some(busy) = outcome.get("deferredBusy").and_then(Value::as_str) {
                        super::config_channels::set_config_value(
                            &me.app,
                            &stamp_key,
                            previous_stamp,
                        );
                        log::info!(
                            "Auto-update: {} became busy before its queued update ran.",
                            game_profiles::display_name(game_profiles::profile(&game_id))
                        );
                        me.defer(&game_id, busy, &reason);
                    } else if outcome.get("success") == Some(&Value::Bool(false)) {
                        log::warn!(
                            "Auto-update start-download failed for {game_id}: {}",
                            outcome.get("error").and_then(Value::as_str).unwrap_or("(no detail)")
                        );
                    }
                }
                Err(e) => log::warn!("Auto-update start-download failed for {game_id}: {e}"),
            }
        });
    }

    async fn notify_update(&self, game_id: &str) {
        let profile = game_profiles::profile(game_id);
        if !game_profiles::is_managed(profile) {
            return;
        }
        let game_path = self.game_key(game_id, "gamePath");
        if game_path.as_str().map(str::is_empty).unwrap_or(true) {
            return;
        }
        if !http::is_online_cached() {
            return;
        }
        let state = self.app.state::<BackendState>();
        let result = state.game.check_for_updates(true, Some(game_id), "notify").await;
        if result["success"] != Value::Bool(true) {
            return;
        }
        super::config_channels::set_config_value(
            &self.app,
            &format!("games.{game_id}.lastUpdateNotifyCheck"),
            json!(chrono::Utc::now().timestamp_millis()),
        );
        if result["updateAvailable"] == Value::Bool(true) {
            log::info!(
                "Update available for {} with auto-update off, notifying only.",
                game_profiles::display_name(profile)
            );
            let _ = self.app.emit("update-available", result);
        }
    }

    async fn run_sweep(self: &Arc<Self>, reason: &str) {
        if self.sweeping.swap(true, Ordering::SeqCst) {
            return;
        }
        if !self.any_enabled() {
            self.sweeping.store(false, Ordering::SeqCst);
            return;
        }
        if !http::is_online_cached() {
            log::info!("Auto-update sweep skipped ({reason}) — offline.");
            if reason == "startup" {
                self.startup_skipped.store(true, Ordering::SeqCst);
            }
            self.sweeping.store(false, Ordering::SeqCst);
            return;
        }

        log::info!("Running game auto-update sweep ({reason}).");
        for game_id in game_profiles::GAME_IDS {
            if !self.auto_update_enabled(game_id) {
                continue;
            }
            self.maybe_update_game(game_id, reason).await;
        }
        self.sweeping.store(false, Ordering::SeqCst);
    }

    pub fn resume_skipped_startup_sweep(self: &Arc<Self>) {
        if !self.startup_skipped.swap(false, Ordering::SeqCst) {
            return;
        }
        let me = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            me.run_sweep("startup-online").await;
        });
    }

    async fn replay_deferred(self: &Arc<Self>) {
        if self.app.state::<BackendState>().game.is_any_game_active() {
            return;
        }
        let pending: Vec<(String, &'static str)> = self.deferred.lock().drain().collect();
        for (game_id, reason) in pending {
            self.maybe_update_game(&game_id, reason).await;
        }
    }

    pub fn start(self: &Arc<Self>) {
        if self.any_enabled() {
            let me = Arc::clone(self);
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(STARTUP_DELAY).await;
                me.run_sweep("startup").await;
            });
        }

        let me = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(POLL_INTERVAL).await;
                let now = chrono::Utc::now().timestamp_millis();
                for game_id in game_profiles::GAME_IDS {
                    if !me.auto_update_enabled(game_id) {
                        let last_ts = me
                            .game_key(game_id, "lastUpdateNotifyCheck")
                            .as_i64()
                            .unwrap_or(0);
                        if daily_weekly_due("daily", last_ts, now) {
                            me.notify_update(game_id).await;
                        }
                        continue;
                    }
                    let schedule = me.game_key(game_id, "autoUpdateSchedule");
                    let schedule = match schedule.as_str() {
                        Some("off") => "off",
                        Some("weekly") => "weekly",
                        _ => "daily",
                    };
                    if schedule == "off" {
                        continue;
                    }
                    let last_ts = me
                        .game_key(game_id, "lastAutoUpdateCheck")
                        .as_i64()
                        .unwrap_or(0);
                    if daily_weekly_due(schedule, last_ts, now) {
                        me.maybe_update_game(game_id, &format!("schedule:{schedule}"))
                            .await;
                    }
                }
            }
        });
    }

    pub fn on_game_stopped(self: &Arc<Self>, game_id: &str) {
        let me = Arc::clone(self);
        let game_id = game_id.to_string();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(POST_CLOSE_DELAY).await;
            if !game_id.is_empty() {
                me.maybe_update_game(&game_id, "game-stopped").await;
            }
            me.replay_deferred().await;
        });
    }
}

const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;

fn local_day_key(ts_ms: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(ts_ms) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            dt.format("%Y-%m-%d").to_string()
        }
        chrono::LocalResult::None => String::new(),
    }
}

pub(crate) fn daily_weekly_due(kind: &str, last_ts: i64, now: i64) -> bool {
    match kind {
        "daily" => last_ts == 0 || local_day_key(last_ts) != local_day_key(now),
        "weekly" => last_ts == 0 || last_ts > now || (now - last_ts) >= WEEK_MS,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000_000;

    #[test]
    fn weekly_due_after_a_week() {
        assert!(daily_weekly_due("weekly", 0, NOW));
        assert!(!daily_weekly_due("weekly", NOW - WEEK_MS + 1, NOW));
        assert!(daily_weekly_due("weekly", NOW - WEEK_MS, NOW));
    }

    #[test]
    fn future_stamp_counts_as_due() {
        assert!(daily_weekly_due("weekly", NOW + 365 * 24 * 60 * 60 * 1000, NOW));
        assert!(daily_weekly_due("daily", NOW + 365 * 24 * 60 * 60 * 1000, NOW));
    }

    #[test]
    fn unknown_schedule_is_never_due() {
        assert!(!daily_weekly_due("off", 0, NOW));
    }

    #[test]
    fn replayed_startup_keeps_startup_prefix() {
        assert_eq!(replay_reason("startup"), "startup-deferred");
        assert_eq!(replay_reason("startup-deferred"), "startup-deferred");
        assert_eq!(replay_reason("schedule:daily"), "deferred");
        assert_eq!(replay_reason("game-stopped"), "deferred");
    }

    #[test]
    fn only_the_launch_sweep_reuses_cached_checks() {
        assert!(!force_update_check("startup"));
        assert!(!force_update_check("startup-online"));
        assert!(force_update_check("startup-deferred"));
        assert!(force_update_check("deferred"));
        assert!(force_update_check("game-stopped"));
        assert!(force_update_check("schedule:daily"));
    }
}
