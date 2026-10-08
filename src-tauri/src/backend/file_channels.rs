// ------------ Game Install Commands ------------
// The commands behind a game's Install, Repair, Verify, Move and Uninstall buttons, plus the disk space and default install folder lookups.
// Each one resolves the game profile, refuses if something else is already using the game, and hands the real work to the operation queue.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::download_engine::GameDownloadManager;
use super::queue::{OperationQueue, Phase, Update};
use super::repair_engine::GameRepairManager;
use super::state::BackendState;
use super::validator::{self, Resource, ValidationMeta};
use super::{
    arg_str, bd2, bluepoch, err_response, game_file_ops, game_path, game_profiles,
    hypergryph_reconcile as reconcile, nte, ok_response, ok_with, progress,
    resolve_profile, sophon, steam,
};

pub struct SophonVerifyCache {
    pub game_id: String,
    pub tag: String,
    pub audio_language: String,
    pub at: std::time::Instant,
    pub plans: Vec<(sophon::Category, sophon::Plan)>,
}

// ------------ Engine Pools ------------
// Keeps one download manager and one repair manager per game, along with the shared operation queue, so every command here talks to the same running jobs.
pub struct EnginePools {
    pub queue: Arc<OperationQueue>,
    downloads: Mutex<HashMap<String, Arc<GameDownloadManager>>>,
    repairs: Mutex<HashMap<String, Arc<GameRepairManager>>>,
    verify_cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub sophon_verify_cache: Mutex<Option<SophonVerifyCache>>,
}

impl EnginePools {
    pub fn new(app: AppHandle) -> Arc<Self> {
        let pools = Arc::new(Self {
            queue: OperationQueue::new(app),
            downloads: Mutex::new(HashMap::new()),
            repairs: Mutex::new(HashMap::new()),
            verify_cancels: Mutex::new(HashMap::new()),
            sophon_verify_cache: Mutex::new(None),
        });
        let weak: Weak<EnginePools> = Arc::downgrade(&pools);
        pools.queue.set_resume_hook(move |meta| {
            let Some(pools) = weak.upgrade() else {
                return;
            };
            if meta.op_type == "repair" {
                if let Some(mgr) = pools.repairs.lock().get(&meta.game_id) {
                    mgr.resume_repair();
                }
            } else if let Some(mgr) = pools.downloads.lock().get(&meta.game_id) {
                mgr.resume_download();
            }
        });
        pools
    }

    fn download_manager(
        &self,
        app: &AppHandle,
        profile: &'static Value,
    ) -> Arc<GameDownloadManager> {
        let id = game_profiles::profile_id(profile).to_string();
        let mut pool = self.downloads.lock();
        let mgr = pool
            .entry(id)
            .or_insert_with(|| GameDownloadManager::new(app.clone(), profile));
        mgr.set_profile(profile);
        Arc::clone(mgr)
    }

    fn existing_download_manager(&self, game_id: &str) -> Option<Arc<GameDownloadManager>> {
        self.downloads.lock().get(game_id).cloned()
    }

    fn repair_manager(&self, app: &AppHandle, profile: &'static Value) -> Arc<GameRepairManager> {
        let id = game_profiles::profile_id(profile).to_string();
        let mut pool = self.repairs.lock();
        let mgr = pool
            .entry(id)
            .or_insert_with(|| GameRepairManager::new(app.clone(), profile));
        mgr.set_profile(profile);
        Arc::clone(mgr)
    }
}

fn pools(app: &AppHandle) -> Arc<EnginePools> {
    app.state::<BackendState>().engine.clone()
}

fn configured_game_path(app: &AppHandle, profile_id: &str) -> String {
    app.state::<BackendState>()
        .config
        .get(&format!("games.{profile_id}.gamePath"))
        .as_str()
        .unwrap_or("")
        .to_string()
}

pub(super) async fn steam_copy_at(path: &str, profile: &'static Value) -> Option<String> {
    steam::app_id(profile)?;
    let path = PathBuf::from(path);
    tauri::async_runtime::spawn_blocking(move || steam::detect(&path, profile))
        .await
        .ok()
        .flatten()
}

// ------------ Pending Install Tracking ------------
// Remembers in the config where a fresh install is going, so an install that got interrupted can be picked up or cleaned up on the next start.
fn pending_install_key(profile_id: &str) -> String {
    format!("games.{profile_id}.pendingInstall")
}

fn record_pending_install(app: &AppHandle, profile_id: &str, path: &str, version_type: &str) {
    super::config_channels::set_config_value(
        app,
        &pending_install_key(profile_id),
        json!({
            "path": path,
            "versionType": version_type,
            "startedAt": chrono::Utc::now().to_rfc3339(),
        }),
    );
    app.state::<BackendState>().config.flush();
}

pub(super) fn clear_pending_install(app: &AppHandle, profile_id: &str) {
    forget_pending_install(app, profile_id);
    clear_update_incomplete(app, profile_id);
}

fn forget_pending_install(app: &AppHandle, profile_id: &str) {
    super::config_channels::set_config_value(app, &pending_install_key(profile_id), Value::Null);
    app.state::<BackendState>().config.flush();
}

pub(super) fn update_incomplete_key(profile_id: &str) -> String {
    format!("games.{profile_id}.updateIncomplete")
}

pub(super) fn mark_update_incomplete(app: &AppHandle, profile_id: &str) {
    super::config_channels::set_config_value(app, &update_incomplete_key(profile_id), json!(true));
    app.state::<BackendState>().config.flush();
}

pub(super) fn clear_update_incomplete(app: &AppHandle, profile_id: &str) {
    let state = app.state::<BackendState>();
    let key = update_incomplete_key(profile_id);
    if state.config.get(&key).is_null() {
        return;
    }
    super::config_channels::set_config_value(app, &key, Value::Null);
    state.config.flush();
    log::info!("{profile_id}: the installed files are complete again, so it can be played.");
    state.window.update_tray_menu();
}

pub(super) fn follow_moved_pending_install(
    app: &AppHandle,
    profile_id: &str,
    old_path: &str,
    new_path: &str,
) {
    let key = format!("{}.path", pending_install_key(profile_id));
    let recorded = app.state::<BackendState>().config.get(&key);
    if recorded.as_str().is_some_and(|p| same_path(p, old_path)) {
        super::config_channels::set_config_value(app, &key, json!(new_path));
        app.state::<BackendState>().config.flush();
    }
}

fn restore_pending_install(app: &AppHandle, profile_id: &str, previous: Value) {
    super::config_channels::set_config_value(app, &pending_install_key(profile_id), previous);
    app.state::<BackendState>().config.flush();
}

async fn discard_cancelled_install(profile_id: &str, install_path: &Path) {
    let discard_path = install_path.to_path_buf();
    let id = profile_id.to_string();
    let (files, bytes) = tauri::async_runtime::spawn_blocking(move || {
        super::download_engine::discard_partial_install_for(&discard_path, &id)
    })
    .await
    .unwrap_or((0, 0));
    if files > 0 {
        log::info!(
            "Cancelled {profile_id} install: discarded {files} file(s), {:.2} GB.",
            bytes as f64 / 1e9
        );
    }
}

fn same_path(a: &str, b: &str) -> bool {
    let norm = |p: &str| p.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

fn renderer_install_path_refusal(profile: &Value, path: &str) -> Option<String> {
    use std::path::Component;
    let target = Path::new(path);
    let plain = target.is_absolute()
        && !target
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir));
    if !plain {
        return Some("The install folder must be a full path.".to_string());
    }
    let folder = game_profiles::install_folder_name(profile);
    let leaf = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if folder.is_empty() || !leaf.eq_ignore_ascii_case(&folder) {
        return Some(format!(
            "Pick the install location again. Peebify installs into a \"{folder}\" folder."
        ));
    }
    None
}

fn with_game_folder(picked: &str, folder: &str) -> String {
    let trimmed = picked.trim_end_matches(['\\', '/']);
    if folder.is_empty() {
        return picked.to_string();
    }
    let leaf = trimmed.rsplit(['\\', '/']).next().unwrap_or("");
    if leaf.eq_ignore_ascii_case(folder) {
        return trimmed.to_string();
    }
    format!("{trimmed}\\{folder}")
}

fn nested_install_dir(profile: &Value, base: &Path) -> Option<PathBuf> {
    if !matches!(
        game_profiles::install_mode(profile),
        Some("hypergryph") | Some("sophon") | Some("bluepoch")
    ) {
        return None;
    }
    let exe_name = game_profiles::executable_name(profile);
    if base.join(exe_name).exists() {
        return Some(base.to_path_buf());
    }
    std::fs::read_dir(base)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|entry| entry.path())
        .find(|nested| nested.join(exe_name).exists())
}

struct PreparedStart<F: FnMut()>(F);

impl<F: FnMut()> Drop for PreparedStart<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}

fn cancelled_before_start(app: &AppHandle, game_id: &str) -> Value {
    log::info!("Download for {game_id} was cancelled before it started.");
    super::queue::publish(
        app,
        "download-progress",
        Phase::Cancelled,
        json!({
            "status": super::download_engine::status::CANCELLED,
            "gameId": game_id,
            "percentage": 0,
        }),
    );
    json!({
        "success": false,
        "cancelled": true,
        "gameId": game_id,
        "error": "Download aborted by user.",
    })
}

fn refuse_beside_download(app: &AppHandle, game_id: &str, op_type: &str) -> Option<Value> {
    if !matches!(op_type, "move" | "repair" | "verify") {
        return None;
    }
    let pools = pools(app);
    let name = game_profiles::display_name(game_profiles::profile(game_id));
    if pools.queue.download_in_flight(game_id) {
        log::info!("Refusing {op_type} for {game_id} because its download is still in progress.");
        return Some(err_response(format!(
            "{name} has a download in progress. Let it finish or cancel it on the Downloads page, then try again."
        )));
    }
    if pools.queue.has_job_for(game_id) {
        log::info!(
            "Refusing {op_type} for {game_id} because another operation for it is running or queued."
        );
        return Some(err_response(format!(
            "{name} already has an operation in progress. Let it finish or cancel it on the Downloads page, then try again."
        )));
    }
    None
}

type QueuedRun = std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + 'static>>;

async fn run_queued(
    app: &AppHandle,
    game_id: &str,
    op_type: &str,
    kind: &str,
    status_event: &str,
    run: QueuedRun,
) -> Value {
    match enqueue_or_refuse(app, game_id, op_type, kind, status_event, run) {
        Ok(rx) => rx
            .await
            .unwrap_or_else(|_| err_response("Queued operation was dropped.")),
        Err(reply) => reply,
    }
}

fn enqueue_or_refuse(
    app: &AppHandle,
    game_id: &str,
    op_type: &str,
    kind: &str,
    status_event: &str,
    run: QueuedRun,
) -> Result<tokio::sync::oneshot::Receiver<Value>, Value> {
    if game_file_ops::is_uninstalling(game_id) {
        return Err(err_response(format!(
            "{} is being uninstalled. Try again when it finishes.",
            game_profiles::display_name(game_profiles::profile(game_id))
        )));
    }
    if let Some(refusal) = refuse_beside_download(app, game_id, op_type) {
        return Err(refusal);
    }
    let pools = pools(app);
    if op_type == "download" && pools.queue.has_download_for(game_id) {
        log::info!("A download for {game_id} is already running or queued, so the new request joins it.");
        return Err(super::queue::already_queued(game_id));
    }
    if pools.queue.is_busy() || pools.queue.has_pending(game_id, None) {
        super::queue::publish(
            app,
            status_event,
            Update::new(Phase::Queued).kind(kind),
            json!({
                "status": "Queued",
                "percentage": 0,
                "gameId": game_id,
                "kind": kind,
            }),
        );
    }
    Ok(pools.queue.enqueue(game_id, op_type, kind, run))
}

// ------------ Download Commands ------------
// Start, pause, resume, cancel and prioritize a game download or update.
pub(super) async fn start_download(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let opts = args.first().cloned().unwrap_or(Value::Null);
    let requested_game_id = opts
        .get("gameId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let version_type = opts
        .get("versionType")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string();
    if let Some(refusal) = super::download_engine::unsupported_channel(&version_type) {
        return Ok(err_response(refusal));
    }
    let profile = resolve_profile(app, requested_game_id.as_deref());
    let profile_id = game_profiles::profile_id(profile).to_string();

    if !game_profiles::is_managed(profile) {
        return Ok(err_response(format!(
            "{} install/update isn't managed by Peebify Launcher yet. Install it with the official launcher, then use \"Locate existing install\".",
            game_profiles::display_name(profile)
        )));
    }

    if pools(app).queue.has_download_for(&profile_id) {
        log::info!("A download for {profile_id} is already running or queued, so the new request joins it.");
        return Ok(super::queue::already_queued(&profile_id));
    }

    let state = app.state::<BackendState>();
    let previous_path = state
        .config
        .get(&format!("games.{profile_id}.gamePath"))
        .as_str()
        .unwrap_or("")
        .to_string();
    let was_installed = if previous_path.is_empty() {
        false
    } else {
        let prev = previous_path.clone();
        tauri::async_runtime::spawn_blocking(move || {
            game_path::validate_game_path_for_profile(&prev, profile).is_valid
        })
        .await
        .unwrap_or(false)
    };

    let mut selected_path = opts
        .get("installPath")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| previous_path.clone());

    let recorded_pending = state
        .config
        .get(&format!("{}.path", pending_install_key(&profile_id)))
        .as_str()
        .map(str::to_string);
    let known_path = same_path(&selected_path, &previous_path)
        || recorded_pending
            .as_deref()
            .is_some_and(|p| same_path(p, &selected_path));
    if !selected_path.is_empty() && !known_path {
        if let Some(refusal) = renderer_install_path_refusal(profile, &selected_path) {
            log::warn!("Refusing to download {profile_id} into {selected_path}: {refusal}");
            return Ok(err_response(refusal));
        }
    }

    if was_installed && !same_path(&selected_path, &previous_path) {
        let pending_path = state
            .config
            .get(&format!("{}.path", pending_install_key(&profile_id)));
        if pending_path.as_str().is_some_and(|p| same_path(p, &selected_path)) {
            log::info!(
                "{profile_id} is installed at {previous_path}, so its stale unfinished-install path {selected_path} is not used."
            );
            selected_path = previous_path.clone();
        }
    }

    if selected_path.is_empty() {
        let dialog_result = crate::backend::fs_util::dialog::show_open(
            app,
            json!({
                "title": "Select Installation Folder",
                "directory": true,
                "defaultPath": super::config_channels::browse_start_dir(app, profile),
            }),
        )
        .await?;
        let picked = dialog_result["filePaths"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .map(str::to_string);
        match picked {
            Some(p) => {
                selected_path = with_game_folder(&p, &game_profiles::install_folder_name(profile));
                let target = Path::new(&selected_path);
                if !target.exists() {
                    if let Some(too_deep) = game_path::path_budget_error(target, profile) {
                        return Ok(err_response(too_deep));
                    }
                }
            }
            None => {
                return Ok(json!({
                    "success": false,
                    "cancelled": true,
                    "error": "Installation path not selected.",
                }))
            }
        }
    }

    if let Some(offline) = drive_offline_error(game_profiles::display_name(profile), &selected_path)
    {
        log::warn!("Refusing to download {profile_id}: {offline}");
        return Ok(err_response(offline));
    }

    if !was_installed && game_file_ops::inside_launcher_payload(Path::new(&selected_path)) {
        log::warn!("Refusing to download {profile_id} into the launcher's own folder: {selected_path}");
        return Ok(err_response(game_file_ops::LAUNCHER_PAYLOAD));
    }

    if let Some(app_id) = steam_copy_at(&selected_path, profile).await {
        log::info!(
            "Refusing to download {profile_id} into the Steam copy (app {app_id}) because Steam updates it."
        );
        return Ok(err_response(format!(
            "{} is installed through Steam. Steam installs and updates it, so use \"Update in Steam\".",
            game_profiles::display_name(profile)
        )));
    }

    let defer_if_busy = opts.get("deferIfBusy") == Some(&Value::Bool(true));
    if was_installed && !defer_if_busy {
        if let Some(refusal) = game_file_ops::refuse_if_running(app, profile).await {
            return Ok(refusal);
        }
    }

    let previous_pending = state.config.get(&pending_install_key(&profile_id));
    let had_pending = !previous_pending.is_null();
    record_pending_install(app, &profile_id, &selected_path, &version_type);

    let manager = pools(app).download_manager(app, profile);
    let install_path = PathBuf::from(&selected_path);
    let path_was_empty = previous_path.is_empty();
    let in_place = was_installed && same_path(&selected_path, &previous_path);
    let ran = Arc::new(AtomicBool::new(false));
    let run = Box::pin({
        let manager = Arc::clone(&manager);
        let install_path = install_path.clone();
        let app = app.clone();
        let game_id = profile_id.clone();
        let ran = Arc::clone(&ran);
        async move {
            ran.store(true, Ordering::SeqCst);
            manager.prepare_start();
            let _prepared = PreparedStart(|| manager.abandon_start());
            if defer_if_busy {
                if let Some(busy) = super::game_updater::busy_reason(&app, &game_id).await {
                    forget_pending_install(&app, &game_id);
                    if manager.start_cancelled() {
                        return cancelled_before_start(&app, &game_id);
                    }
                    super::queue::publish(
                        &app,
                        "download-progress",
                        Phase::Deferred,
                        json!({ "status": "Deferred", "gameId": game_id, "percentage": 0 }),
                    );
                    return json!({
                        "success": false,
                        "deferredBusy": busy,
                        "gameId": game_id,
                        "error": "Update deferred while a game is running.",
                    });
                }
            }
            if was_installed {
                if let Some(message) =
                    game_file_ops::running_refusal(&app, game_profiles::profile(&game_id)).await
                {
                    if !had_pending {
                        forget_pending_install(&app, &game_id);
                    }
                    if manager.start_cancelled() {
                        return cancelled_before_start(&app, &game_id);
                    }
                    super::queue::publish(
                        &app,
                        "download-progress",
                        Phase::Error,
                        json!({
                            "status": "Error",
                            "gameId": game_id,
                            "percentage": 0,
                            "error": message,
                        }),
                    );
                    return json!({
                        "success": false,
                        "gameRunning": true,
                        "gameId": game_id,
                        "error": message,
                    });
                }
            } else {
                create_shared_root_for(&install_path, &system_drive_games_root());
            }
            let on_complete: super::download_engine::CompletionHook = {
                let app = app.clone();
                let game_id = game_id.clone();
                Box::new(move |path: PathBuf| {
                    let final_path = nested_install_dir(profile, &path).unwrap_or(path);
                    super::config_channels::set_config_value(
                        &app,
                        &format!("games.{game_id}.gamePath"),
                        json!(final_path.to_string_lossy()),
                    );
                    forget_pending_install(&app, &game_id);
                })
            };
            let result = manager
                .download_game_with(&install_path, in_place, Some(on_complete))
                .await;
            if result.get("cancelled") == Some(&Value::Bool(true)) {
                if !was_installed {
                    discard_cancelled_install(&game_id, &install_path).await;
                    if path_was_empty {
                        super::config_channels::set_config_value(
                            &app,
                            &format!("games.{game_id}.gamePath"),
                            json!(""),
                        );
                    }
                }
                forget_pending_install(&app, &game_id);
            }
            result
        }
    });
    let kind = if was_installed { "update" } else { "install" };
    let result = run_queued(app, &profile_id, "download", kind, "download-progress", run).await;

    if result.get("alreadyQueued") == Some(&Value::Bool(true)) {
        return Ok(result);
    }

    if result.get("deferredBusy").is_some_and(Value::is_string)
        || result.get("gameRunning") == Some(&Value::Bool(true))
    {
        return Ok(result);
    }

    if result["success"] == Value::Bool(true) {
        forget_pending_install(app, &profile_id);
        let mut final_path = result
            .get("installPath")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| selected_path.clone());

        let base = PathBuf::from(&selected_path);
        let resolved =
            tauri::async_runtime::spawn_blocking(move || nested_install_dir(profile, &base))
                .await
                .ok()
                .flatten();
        if let Some(resolved) = resolved {
            final_path = resolved.to_string_lossy().to_string();
        }

        super::config_channels::set_config_value(
            app,
            &format!("games.{profile_id}.gamePath"),
            json!(final_path),
        );
        let _ = app.emit(
            "installation-complete",
            json!({
                "gamePath": final_path,
                "version": result.get("version").cloned().unwrap_or(Value::Null),
                "gameId": profile_id,
            }),
        );
        super::notify::notify_if_backgrounded(
            app,
            &format!("{} is ready", game_profiles::display_name(profile)),
            "Download complete. The game is ready to play.",
        );
    } else {
        let error = result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cancelled = result.get("cancelled") == Some(&Value::Bool(true));
        if !cancelled && !error.is_empty() {
            super::notify::notify_if_backgrounded(
                app,
                &format!("{} download failed", game_profiles::display_name(profile)),
                &error,
            );
        }
        if cancelled && !ran.load(Ordering::SeqCst) {
            restore_pending_install(app, &profile_id, previous_pending);
        }
        if !cancelled && !was_installed && path_was_empty {
            super::config_channels::set_config_value(
                app,
                &format!("games.{profile_id}.gamePath"),
                json!(""),
            );
        }
    }

    Ok(result)
}

fn profile_arg(app: &AppHandle, args: &[Value]) -> (&'static Value, &'static str) {
    let profile = resolve_profile(app, arg_str(args, 0));
    (profile, game_profiles::profile_id(profile))
}

fn cancel_queued(
    app: &AppHandle,
    profile_id: &str,
    kind: &str,
    event: &str,
    percentage: bool,
) -> bool {
    let pools = pools(app);
    if !pools.queue.cancel_pending(profile_id, Some(kind)) {
        return false;
    }
    let running = pools
        .queue
        .current_meta()
        .is_some_and(|m| m.game_id == profile_id)
        || pools.queue.has_parked(profile_id);
    if running {
        log::info!(
            "Removed a queued {kind} for {profile_id} while another of its operations runs."
        );
        return true;
    }
    let mut payload = json!({ "status": "Cancelled", "gameId": profile_id });
    if percentage {
        if let Some(map) = payload.as_object_mut() {
            map.insert("percentage".to_string(), json!(0));
        }
    }
    super::queue::publish(app, event, Phase::Cancelled, payload);
    true
}

pub(super) async fn pause_download(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let (_, profile_id) = profile_arg(app, args);
    if let Some(mgr) = pools(app).existing_download_manager(profile_id) {
        mgr.pause_download();
    }
    Ok(ok_response())
}

pub(super) async fn resume_download(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let (_, profile_id) = profile_arg(app, args);
    let pools = pools(app);
    if pools.queue.has_parked(profile_id) {
        log::info!(
            "resume-download for {profile_id}: parked behind a prioritized op, it resumes when that finishes."
        );
        return Ok(ok_response());
    }
    if let Some(mgr) = pools.existing_download_manager(profile_id) {
        mgr.resume_download();
    }
    Ok(ok_response())
}

pub(super) async fn cancel_download(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let (_, profile_id) = profile_arg(app, args);
    if cancel_queued(app, profile_id, "download", "download-progress", true) {
        return Ok(ok_response());
    }
    match pools(app).existing_download_manager(profile_id) {
        Some(mgr) => mgr.cancel_download(),
        None => log::info!("cancel-download for {profile_id}: nothing to cancel."),
    }
    Ok(ok_response())
}

pub(super) async fn prioritize_download(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let profile = resolve_profile(app, arg_str(args, 0));
    let profile_id = game_profiles::profile_id(profile);
    let pools = pools(app);
    if pools
        .queue
        .current_meta()
        .map(|m| m.game_id == profile_id)
        .unwrap_or(false)
    {
        return Ok(ok_response());
    }
    if !pools.queue.prioritize(profile_id) {
        return Ok(ok_response());
    }
    let Some(active) = pools.queue.current_meta() else {
        return Ok(ok_response());
    };
    if active.game_id == profile_id {
        return Ok(ok_response());
    }
    let defer = |other: &str| -> Result<Value, String> {
        log::info!(
            "Moved {profile_id} to the front of the queue; it starts after the current {other} for {} finishes.",
            active.game_id
        );
        Ok(ok_with(json!({
            "deferred": format!(
                "{} starts after the current {other} finishes.",
                game_profiles::display_name(profile)
            ),
        })))
    };
    match active.op_type.as_str() {
        "download" => {
            let Some(mgr) = pools.existing_download_manager(&active.game_id) else {
                return defer("download");
            };
            if mgr.is_extracting() {
                log::info!(
                    "Moved {profile_id} to the front of the queue; it starts after {} finishes unpacking.",
                    active.game_id
                );
                return Ok(ok_with(json!({
                    "deferred": format!(
                        "{} starts after the current install finishes unpacking.",
                        game_profiles::display_name(profile)
                    ),
                })));
            }
            let was_paused = mgr.is_paused();
            mgr.pause_download();
            if !mgr.is_paused() {
                return defer("download");
            }
            if !pools.queue.park_current(active.id, was_paused) && !was_paused {
                mgr.resume_download();
            }
        }
        "repair" => {
            let mgr = pools.repairs.lock().get(&active.game_id).cloned();
            let Some(mgr) = mgr else {
                return defer("repair");
            };
            let was_paused = mgr.is_paused();
            mgr.pause_repair();
            if !mgr.is_paused() {
                return defer("repair");
            }
            if !pools.queue.park_current(active.id, was_paused) && !was_paused {
                mgr.resume_repair();
            }
        }
        other => return defer(other),
    }
    Ok(ok_response())
}

// ------------ Repair Commands ------------
// Full and quick repair, plus pause, resume and cancel for repairs, moves and verifies.
async fn handle_repair_request(app: &AppHandle, mode: &str, game_id: Option<&str>) -> Value {
    let profile = resolve_profile(app, game_id);
    let profile_id = game_profiles::profile_id(profile).to_string();
    let game_path = configured_game_path(app, &profile_id);
    if game_path.is_empty() {
        log::warn!(
            "Repair attempted without a configured game path (mode: {mode}, game: {profile_id})."
        );
        return err_response("Game path is not configured.");
    }
    if !game_profiles::is_managed(profile) {
        return err_response(format!(
            "{} repair is not managed by Peebify Launcher yet.",
            game_profiles::display_name(profile)
        ));
    }
    if let Some(missing) = missing_install(profile, &game_path).await {
        log::warn!("Refusing to repair {profile_id}: {missing}");
        return err_response(missing);
    }
    if let Some(app_id) = steam_copy_at(&game_path, profile).await {
        log::info!("Refusing to repair {profile_id} because it is the Steam copy (app {app_id}).");
        return err_response(format!(
            "{} is installed through Steam. Use Verify integrity of game files in Steam to repair it.",
            game_profiles::display_name(profile)
        ));
    }

    let pools = pools(app);
    let repair_active = pools
        .queue
        .current_meta()
        .is_some_and(|m| m.game_id == profile_id && m.op_type == "repair");
    if repair_active || pools.queue.has_pending(&profile_id, Some("repair")) {
        log::info!("A repair for {profile_id} is already running or queued; ignoring the new {mode} request.");
        return ok_response();
    }
    if let Some(refusal) = refuse_beside_download(app, &profile_id, "repair") {
        return refusal;
    }

    let manager = pools.repair_manager(app, profile);
    let mode = mode.to_string();
    let run = Box::pin({
        let manager = Arc::clone(&manager);
        let app = app.clone();
        let profile_id = profile_id.clone();
        async move {
            manager.prepare_start();
            let _prepared = PreparedStart(|| manager.abandon_start());
            let game_path = configured_game_path(&app, &profile_id);
            if game_path.is_empty() {
                super::queue::publish(
                    &app,
                    "repair-progress",
                    Phase::Error,
                    json!({
                        "status": super::repair_engine::status::ERROR,
                        "gameId": profile_id,
                        "error": "Game path is not configured.",
                    }),
                );
                return err_response("Game path is not configured.");
            }
            if let Some(missing) = missing_install(profile, &game_path).await {
                log::warn!("Repair of {profile_id} stopped before it began: {missing}");
                super::queue::publish(
                    &app,
                    "repair-progress",
                    Phase::Error,
                    json!({
                        "status": super::repair_engine::status::ERROR,
                        "gameId": profile_id,
                        "error": missing,
                    }),
                );
                return err_response(missing);
            }
            manager.repair_game(Path::new(&game_path), &mode).await;
            Value::Null
        }
    });
    match enqueue_or_refuse(app, &profile_id, "repair", "repair", "repair-progress", run) {
        Ok(_) => ok_response(),
        Err(refusal) => {
            log::info!("Repair of {profile_id} was refused before it was queued.");
            refusal
        }
    }
}

pub(super) async fn start_repair(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    Ok(handle_repair_request(app, "full", arg_str(args, 0)).await)
}

pub(super) async fn start_quick_repair(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    Ok(handle_repair_request(app, "quick", arg_str(args, 0)).await)
}

pub(super) async fn cancel_repair(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let (_, profile_id) = profile_arg(app, args);
    if cancel_queued(app, profile_id, "repair", "repair-progress", false) {
        return Ok(ok_response());
    }
    let mgr = pools(app).repairs.lock().get(profile_id).cloned();
    match mgr {
        Some(mgr) => mgr.cancel_repair(),
        None => log::info!("cancel-repair for {profile_id}: nothing to cancel."),
    }
    Ok(ok_response())
}

pub(super) async fn cancel_move(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let (_, profile_id) = profile_arg(app, args);
    if cancel_queued(app, profile_id, "move", "move-progress", true) {
        return Ok(ok_response());
    }
    if !game_file_ops::cancel_running_move(profile_id) {
        log::info!("cancel-move for {profile_id}: no move to cancel.");
    }
    Ok(ok_response())
}

pub(super) async fn pause_repair(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let (_, profile_id) = profile_arg(app, args);
    if let Some(mgr) = pools(app).repairs.lock().get(profile_id) {
        mgr.pause_repair();
    }
    Ok(ok_response())
}

pub(super) async fn resume_repair(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let (_, profile_id) = profile_arg(app, args);
    let pools = pools(app);
    if pools.queue.has_parked(profile_id) {
        log::info!(
            "resume-repair for {profile_id}: parked behind a prioritized op, it resumes when that finishes."
        );
        return Ok(ok_response());
    }
    if let Some(mgr) = pools.repairs.lock().get(profile_id) {
        mgr.resume_repair();
    }
    Ok(ok_response())
}

pub(super) async fn cancel_verify(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let (_, profile_id) = profile_arg(app, args);
    if cancel_queued(app, profile_id, "verify", "download-progress", true) {
        return Ok(ok_response());
    }
    let pools = pools(app);
    let flag = pools.verify_cancels.lock().get(profile_id).cloned();
    if let Some(flag) = flag {
        flag.store(true, Ordering::SeqCst);
        return Ok(ok_response());
    }
    if pools.queue.download_in_flight(profile_id) {
        if let Some(mgr) = pools.existing_download_manager(profile_id) {
            log::info!(
                "cancel-verify for {profile_id}: no verify running, forwarded to the download."
            );
            mgr.cancel_download();
            return Ok(ok_response());
        }
    }
    log::info!("cancel-verify for {profile_id}: nothing to cancel.");
    Ok(ok_response())
}

// ------------ File Verification ------------
// Checks the installed files against what the game's server says they should be, with a separate path for each kind of install (Sophon, Hypergryph, NTE, BD2 and so on).
fn drive_offline_error(name: &str, game_path: &str) -> Option<String> {
    let root = drive_root(game_path)?;
    if Path::new(&root).is_dir() {
        return None;
    }
    Some(format!(
        "The drive that holds {name} ({root}) is not connected. Reconnect it and try again."
    ))
}

async fn missing_install(profile: &'static Value, game_path: &str) -> Option<String> {
    let path = game_path.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        missing_install_error(game_profiles::display_name(profile), &path)
    })
    .await
    .unwrap_or(None)
}

fn missing_install_error(name: &str, game_path: &str) -> Option<String> {
    if Path::new(game_path).is_dir() {
        return None;
    }
    Some(drive_offline_error(name, game_path).unwrap_or_else(|| {
        format!(
            "The {name} folder {game_path} is missing. Reconnect the drive, use Locate existing install, or reinstall the game."
        )
    }))
}

fn broken_files_error(count: usize) -> String {
    if count == 1 {
        "1 file needs repair. Run a repair to replace it.".to_string()
    } else {
        format!("{count} files need repair. Run a repair to replace them.")
    }
}

fn verify_result_payload(profile_id: &str, broken: usize) -> Value {
    let mut payload = json!({
        "gameId": profile_id,
        "status": if broken == 0 { "Verification Complete" } else { "Verification Failed" },
        "percentage": 100,
    });
    if broken > 0 {
        payload["error"] = json!(broken_files_error(broken));
    }
    payload
}

fn verify_update(phase: Phase) -> Update {
    Update::new(phase).kind("verify")
}

fn verify_result_update(broken: usize) -> Update {
    verify_update(if broken == 0 { Phase::Done } else { Phase::Error })
}

pub(super) fn verify_cancelled_response() -> Value {
    json!({ "success": false, "cancelled": true, "error": "Cancelled." })
}

pub(super) async fn verify_game_integrity(
    app: &AppHandle,
    args: &[Value],
) -> Result<Value, String> {
    let profile = resolve_profile(app, arg_str(args, 0));
    let profile_id = game_profiles::profile_id(profile).to_string();
    let game_path = configured_game_path(app, &profile_id);
    if game_path.is_empty() {
        return Ok(err_response("Game path not set."));
    }
    if let Some(missing) = missing_install(profile, &game_path).await {
        log::warn!("Refusing to verify {profile_id}: {missing}");
        return Ok(err_response(missing));
    }

    let app_for_job = app.clone();
    let profile_id_for_job = profile_id.clone();
    let run = Box::pin(async move {
        let game_path = configured_game_path(&app_for_job, &profile_id_for_job);
        if game_path.is_empty() {
            super::queue::publish(
                &app_for_job,
                "download-progress",
                verify_update(Phase::Error),
                json!({
                    "status": "Verification Failed",
                    "gameId": profile_id_for_job,
                    "percentage": 0,
                    "error": "Game path not set.",
                }),
            );
            return err_response("Game path not set.");
        }
        if let Some(missing) = missing_install(profile, &game_path).await {
            log::warn!("Verify of {profile_id_for_job} stopped before it began: {missing}");
            super::queue::publish(
                &app_for_job,
                "download-progress",
                verify_update(Phase::Error),
                json!({
                    "status": "Verification Failed",
                    "gameId": profile_id_for_job,
                    "percentage": 0,
                    "error": missing,
                }),
            );
            return err_response(missing);
        }
        run_verify(&app_for_job, profile, &profile_id_for_job, &game_path).await
    });
    Ok(run_queued(app, &profile_id, "verify", "verify", "download-progress", run).await)
}

async fn run_verify(
    app: &AppHandle,
    profile: &'static Value,
    profile_id: &str,
    game_path: &str,
) -> Value {
    let _power = super::perf::TransferGuard::acquire();
    let pools = pools(app);
    let cancel_flag = Arc::new(AtomicBool::new(false));
    pools
        .verify_cancels
        .lock()
        .insert(profile_id.to_string(), Arc::clone(&cancel_flag));

    let result = async {
        if !game_profiles::is_managed(profile) {
            return Err(VerifyError::Other(format!(
                "{} integrity verification is not supported yet.",
                game_profiles::display_name(profile)
            )));
        }

        if game_profiles::install_mode(profile) == Some("hypergryph") {
            let recorded = reconcile::load_manifest(Path::new(game_path));
            let adopting = recorded.is_none();
            let manifest = match recorded {
                Some(manifest) => manifest,
                None => {
                    let latest = match super::hypergryph::get_latest_game(profile).await {
                        Ok(latest) => reconcile::latest_packs(&latest),
                        Err(e) => {
                            log::warn!("Could not fetch the latest packages for {profile_id}: {e}");
                            None
                        }
                    };
                    let Some((version, packs)) = latest else {
                        return Err(VerifyError::Other(format!(
                            "No install manifest found for {}, and the latest packages could not be fetched to check this install. Check your connection and try again.",
                            game_profiles::display_name(profile)
                        )));
                    };
                    let cancelled = Arc::clone(&cancel_flag);
                    let handle = tokio::runtime::Handle::current();
                    tauri::async_runtime::spawn_blocking(move || {
                        reconcile::manifest_from_remote(packs, &version, cancelled, handle)
                    })
                    .await
                    .map_err(|e| VerifyError::Other(format!("verify task panicked: {e}")))?
                    .map_err(|e| verify_failure(&cancel_flag, e))?
                }
            };
            let tracker = Arc::new(progress::ProgressTracker::new());
            let hooks = VerifyHooks {
                app: app.clone(),
                tracker: Arc::clone(&tracker),
                game_id: profile_id.to_string(),
                cancel: Arc::clone(&cancel_flag),
                status: "Verifying files...",
            };
            let dir = PathBuf::from(game_path);
            let files = manifest.files.clone();
            let invalid = tauri::async_runtime::spawn_blocking(move || {
                reconcile::verify_files(&dir, &files, &hooks)
            })
            .await
            .map_err(|e| VerifyError::Other(format!("verify task panicked: {e}")))?
            .map_err(|e| verify_failure(&cancel_flag, e))?;

            let final_status = if invalid.is_empty() {
                "Verification Complete"
            } else {
                "Verification Failed"
            };
            tracker.force_completion();
            let mut payload = tracker.calculate_metrics();
            if let Some(map) = payload.as_object_mut() {
                map.insert("status".to_string(), json!(final_status));
                map.insert("percentage".to_string(), json!(100));
                map.insert("gameId".to_string(), json!(profile_id));
                if !invalid.is_empty() {
                    map.insert("error".to_string(), json!(broken_files_error(invalid.len())));
                }
            }
            super::queue::publish(
                app,
                "download-progress",
                verify_result_update(invalid.len()),
                payload,
            );

            log::info!(
                "Reconcile verify for {profile_id}: {} files need repair.",
                invalid.len()
            );
            let mut body = json!({ "updatePending": false });
            if adopting && invalid.is_empty() {
                let dir = Path::new(game_path);
                match reconcile::save_manifest(dir, &manifest).and_then(|()| {
                    super::download_engine::update_game_config_file(dir, &manifest.version)
                }) {
                    Ok(()) => {
                        log::info!(
                            "{profile_id}: install matches version {} and is now tracked.",
                            manifest.version
                        );
                        app.state::<BackendState>()
                            .game
                            .clear_update_cache(profile_id);
                    }
                    Err(e) => log::warn!("{profile_id}: could not record the install manifest: {e}"),
                }
            } else if adopting {
                body["message"] = json!(format!(
                    "{} files differ from the latest version ({}). Run Full repair to bring this install up to date.",
                    invalid.len(),
                    manifest.version
                ));
            }
            body["invalidFiles"] = Value::Array(
                invalid
                    .iter()
                    .map(|f| json!({ "dest": f.path, "size": f.size }))
                    .collect(),
            );
            return Ok(ok_with(body));
        }

        if game_profiles::install_mode(profile) == Some("bluepoch") {
            return bluepoch_verify(app, profile, profile_id, game_path, &cancel_flag).await;
        }

        if game_profiles::install_mode(profile) == Some("bd2") {
            return bd2_verify(app, profile, profile_id, game_path, &cancel_flag).await;
        }

        if game_profiles::install_mode(profile) == Some("dna") {
            return dna_verify(app, profile, profile_id, game_path, &cancel_flag).await;
        }

        if game_profiles::install_mode(profile) == Some("netease") {
            let (invalid, remote_version) = nte_invalid_files(
                app,
                game_path,
                profile,
                profile_id,
                Arc::clone(&cancel_flag),
            )
            .await
            .map_err(|e| verify_failure(&cancel_flag, e))?;
            let count = invalid.len();
            if count > 0 {
                super::download_engine::clear_scan_record(Path::new(game_path));
            }
            let local_version = super::game_manager::local_game_version(game_path);
            let pending = pending_update_note(count, local_version.as_deref(), &remote_version);
            let mut payload = verify_result_payload(profile_id, count);
            let mut body = json!({ "invalidFiles": invalid, "updatePending": pending.is_some() });
            if let Some(note) = &pending {
                payload["error"] = json!(note);
                body["message"] = json!(note);
            }
            super::queue::publish(app, "download-progress", verify_result_update(count), payload);
            log::info!(
                "NTE verify for {profile_id}: {count} files need repair (installed {}, latest {remote_version}).",
                local_version.as_deref().unwrap_or("unknown")
            );
            return Ok(ok_with(body));
        }

        if game_profiles::install_mode(profile) == Some("sophon") {
            let SophonVerify {
                broken,
                pending,
                installed_tag,
                tag,
            } = sophon_invalid_files(
                app,
                game_path,
                profile,
                profile_id,
                Arc::clone(&cancel_flag),
            )
            .await
            .map_err(|e| verify_failure(&cancel_flag, e))?;
            let count = broken.len();
            let local_version = installed_tag
                .clone()
                .or_else(|| super::game_manager::local_game_version_for(profile, game_path));
            let mut payload = verify_result_payload(profile_id, count);
            let mut body = json!({ "invalidFiles": broken, "updatePending": false });
            if installed_tag.is_some() {
                body["updatePending"] = json!(count == 0 && pending > 0);
                if let Some(note) = pending_update_note(pending, local_version.as_deref(), &tag) {
                    let text = if count == 0 {
                        note
                    } else {
                        format!("{} {note}", broken_files_error(count))
                    };
                    payload[if count == 0 { "message" } else { "error" }] = json!(text);
                    body["message"] = json!(text);
                }
            } else if let Some(note) = pending_update_note(count, local_version.as_deref(), &tag) {
                payload["error"] = json!(note);
                body["message"] = json!(note);
                body["updatePending"] = json!(true);
            }
            super::queue::publish(app, "download-progress", verify_result_update(count), payload);
            log::info!(
                "Sophon verify for {profile_id}: {count} files need repair, {pending} wait for the update (installed {}, latest {tag}).",
                local_version.as_deref().unwrap_or("unknown")
            );
            return Ok(ok_with(body));
        }

        let manager = pools.download_manager(app, profile);
        let (resources, latest_version) = manager
            .remote_resources()
            .await
            .map_err(|e| verify_failure(&cancel_flag, e))?;

        let tracker = Arc::new(progress::ProgressTracker::new());
        let meta = ValidationMeta {
            is_final: true,
            version: None,
        };
        let invalid = validator::validate_resources(
            app,
            &tracker,
            Arc::new(resources),
            Path::new(game_path),
            &cancel_flag,
            None,
            &meta,
            profile_id,
        )
        .await
        .map_err(|e| verify_failure(&cancel_flag, e))?;
        if !invalid.is_empty() {
            super::download_engine::clear_scan_record(Path::new(game_path));
        }

        let final_status = if invalid.is_empty() {
            "Verification Complete"
        } else {
            "Verification Failed"
        };
        tracker.force_completion();
        let mut payload = tracker.calculate_metrics();
        if let Some(map) = payload.as_object_mut() {
            map.insert("status".to_string(), json!(final_status));
            map.insert("percentage".to_string(), json!(100));
            map.insert("gameId".to_string(), json!(profile_id));
            if !invalid.is_empty() {
                map.insert("error".to_string(), json!(broken_files_error(invalid.len())));
            }
        }
        super::queue::publish(
            app,
            "download-progress",
            verify_result_update(invalid.len()),
            payload,
        );

        let local_version = super::game_manager::local_game_version_for(profile, game_path);
        let update_pending =
            older_install_note(invalid.len(), local_version.as_deref(), &latest_version)
                .is_some();
        let invalid_json: Vec<Value> = invalid.iter().map(Resource::to_json).collect();
        Ok(ok_with(json!({
            "invalidFiles": invalid_json,
            "updatePending": update_pending,
        })))
    }
    .await;

    pools.verify_cancels.lock().remove(profile_id);

    match result {
        Ok(mut response) => {
            if response["success"] == Value::Bool(true) && response.get("updatePending").is_none()
            {
                response["updatePending"] = json!(false);
            }
            if verify_left_no_broken_files(profile, game_path, &response) {
                clear_update_incomplete(app, profile_id);
            }
            response
        }
        Err(VerifyError::Cancelled) => {
            log::info!("Integrity verification cancelled for {profile_id}.");
            super::queue::publish(
                app,
                "download-progress",
                verify_update(Phase::Cancelled),
                json!({ "status": "Verification Cancelled", "percentage": 0, "gameId": profile_id }),
            );
            verify_cancelled_response()
        }
        Err(VerifyError::Other(e)) => {
            log::error!("Game integrity verification failed for {profile_id}: {e}");
            super::queue::publish(
                app,
                "download-progress",
                verify_update(Phase::Error),
                json!({ "status": "Verification Failed", "error": e, "gameId": profile_id }),
            );
            err_response(e)
        }
    }
}

fn verify_left_no_broken_files(profile: &Value, game_path: &str, response: &Value) -> bool {
    let checks_every_file = match game_profiles::install_mode(profile) {
        Some("bluepoch" | "gf2") => false,
        Some("bd2") => bd2::load_manifest(Path::new(game_path)).is_some(),
        _ => true,
    };
    checks_every_file
        && response["success"] == Value::Bool(true)
        && response["updatePending"] != Value::Bool(true)
        && response["invalidFiles"].as_array().is_some_and(Vec::is_empty)
}

enum VerifyError {
    Cancelled,
    Other(String),
}

fn verify_failure(cancel: &AtomicBool, error: String) -> VerifyError {
    if cancel.load(Ordering::SeqCst) {
        VerifyError::Cancelled
    } else {
        VerifyError::Other(error)
    }
}

fn fmt_gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
}

fn folder_bytes(dir: &Path, cancel: &AtomicBool) -> Result<u64, VerifyError> {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        if cancel.load(Ordering::SeqCst) {
            return Err(VerifyError::Cancelled);
        }
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(entry.path()),
                Ok(t) if t.is_file() => {
                    total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
                }
                _ => {}
            }
        }
    }
    Ok(total)
}

const BLUEPOCH_MIN_SIZE_RATIO: f64 = 0.90;
const BD2_MIN_SIZE_RATIO: f64 = 0.90;

async fn bluepoch_verify(
    app: &AppHandle,
    profile: &'static Value,
    profile_id: &str,
    game_path: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, VerifyError> {
    let name = game_profiles::display_name(profile);
    let exe = game_profiles::executable_name(profile);
    let root = PathBuf::from(game_path);

    let tick = |percentage: u32, status: &str| {
        super::queue::publish(
            app,
            "download-progress",
            verify_update(Phase::Verifying),
            json!({ "gameId": profile_id, "status": status, "percentage": percentage }),
        );
    };

    tick(5, "Verifying installed files...");

    if !root.is_dir() {
        return Err(VerifyError::Other(format!(
            "{name} is no longer in {game_path}. Point Peebify at it again with Locate existing install, or reinstall it."
        )));
    }
    let launch_exe = game_path::launch_executable_path(&root, profile);
    if !launch_exe.exists() {
        let base = launch_exe
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| exe.to_string());
        return Err(VerifyError::Other(format!(
            "{base} is missing from {game_path}, so this install cannot start. Reinstall {name}."
        )));
    }
    if !bluepoch::data_dir(&root, exe).is_dir() {
        return Err(VerifyError::Other(format!(
            "{name}'s game data folder is missing from {game_path}, so the install is not usable. Reinstall it."
        )));
    }

    let mut problems: Vec<String> = Vec::new();

    if let Some(too_deep) = game_path::path_budget_error(&root, profile) {
        problems.push(too_deep);
    }

    let installed = bluepoch::installed_version(&root);
    if installed.is_none() {
        problems.push(format!(
            "{name}'s version file ({}) is missing or unreadable, so Peebify cannot tell which version is installed or whether an update is due.",
            bluepoch::VERSION_FILE
        ));
    }

    tick(20, "Verifying installed files: checking staged folders...");
    let staged = bluepoch::staging_dirs(&root, exe);
    if !staged.is_empty() {
        problems.push(format!(
            "{} leftover update folder{} from an interrupted hot update. Run a repair to clear {} so the game starts its next update clean.",
            staged.len(),
            if staged.len() == 1 { "" } else { "s" },
            if staged.len() == 1 { "it" } else { "them" },
        ));
    }

    tick(35, "Verifying installed files: measuring the install...");
    let walk_root = root.clone();
    let walk_cancel = Arc::clone(cancel);
    let on_disk =
        tauri::async_runtime::spawn_blocking(move || folder_bytes(&walk_root, &walk_cancel))
            .await
            .map_err(|e| VerifyError::Other(format!("verify task panicked: {e}")))??;

    if cancel.load(Ordering::SeqCst) {
        return Err(VerifyError::Cancelled);
    }

    tick(
        80,
        "Verifying installed files: comparing with the published size...",
    );
    match bluepoch::fetch_package(profile, "").await {
        Ok(package) if package.install_bytes > 0 => {
            let ratio = on_disk as f64 / package.install_bytes as f64;
            log::info!(
                "{name} verify: {} on disk against a published install size of {} ({:.1}% of it).",
                fmt_gb(on_disk),
                fmt_gb(package.install_bytes),
                ratio * 100.0,
            );
            if ratio < BLUEPOCH_MIN_SIZE_RATIO {
                problems.push(format!(
                    "The folder holds {}, but {name} {} installs to about {}, so files are missing. Reinstall to replace them.",
                    fmt_gb(on_disk),
                    package.version,
                    fmt_gb(package.install_bytes),
                ));
            }
        }
        Ok(_) => log::info!(
            "{name} verify: Bluepoch published no install size for this package; skipped the size check."
        ),
        Err(e) => log::warn!(
            "{name} verify: could not reach Bluepoch for the published size ({e}); skipped the size check."
        ),
    }

    if !problems.is_empty() {
        log::warn!(
            "{name} verify found {} problem(s): {}",
            problems.len(),
            problems.join(" | ")
        );
        return Err(VerifyError::Other(problems.join(" ")));
    }

    let message = format!(
        "{name} looks intact with {} installed, version {}. Bluepoch publishes no per-file checksums, so this checks the shape of the install rather than each file.",
        fmt_gb(on_disk),
        installed.as_deref().unwrap_or("unknown"),
    );
    log::info!("{name} verify: {message}");
    super::queue::publish(
        app,
        "download-progress",
        verify_update(Phase::Done),
        json!({
            "gameId": profile_id,
            "status": "Verification Complete",
            "percentage": 100,
            "message": message,
        }),
    );
    Ok(ok_with(json!({ "invalidFiles": [], "message": message })))
}

async fn bd2_verify(
    app: &AppHandle,
    profile: &'static Value,
    profile_id: &str,
    game_path: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, VerifyError> {
    let name = game_profiles::display_name(profile);
    let exe = game_profiles::executable_name(profile);
    let root = PathBuf::from(game_path);

    let tick = |percentage: u32, status: &str| {
        super::queue::publish(
            app,
            "download-progress",
            verify_update(Phase::Verifying),
            json!({ "gameId": profile_id, "status": status, "percentage": percentage }),
        );
    };

    tick(3, "Verifying installed files...");

    if !root.is_dir() {
        return Err(VerifyError::Other(format!(
            "{name} is no longer in {game_path}. Point Peebify at it again with Locate existing install, or reinstall it."
        )));
    }
    let launch_exe = game_path::launch_executable_path(&root, profile);
    if !launch_exe.exists() {
        let base = launch_exe
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| exe.to_string());
        return Err(VerifyError::Other(format!(
            "{base} is missing from {game_path}, so this install cannot start. Reinstall {name}."
        )));
    }
    if !bd2::data_dir(&root, exe).is_dir() {
        return Err(VerifyError::Other(format!(
            "{name}'s game data folder is missing from {game_path}, so the install is not usable. Reinstall it."
        )));
    }

    let mut problems: Vec<String> = Vec::new();
    if let Some(too_deep) = game_path::path_budget_error(&root, profile) {
        problems.push(too_deep);
    }
    if !root.join(bd2::SETTINGS_FILE).is_file() {
        problems.push(format!(
            "{} is missing from {game_path}, so {name} cannot tell which service to sign in to. Reinstall it.",
            bd2::SETTINGS_FILE
        ));
    }

    let Some(manifest) = bd2::load_manifest(&root) else {
        tick(40, "Verifying installed files: measuring the install...");
        let walk_root = root.clone();
        let walk_cancel = Arc::clone(cancel);
        let on_disk =
            tauri::async_runtime::spawn_blocking(move || folder_bytes(&walk_root, &walk_cancel))
                .await
                .map_err(|e| VerifyError::Other(format!("verify task panicked: {e}")))??;

        if cancel.load(Ordering::SeqCst) {
            return Err(VerifyError::Cancelled);
        }

        tick(
            85,
            "Verifying installed files: comparing with the published size...",
        );
        match bd2::fetch_package(profile).await {
            Ok(package) if package.install_bytes > 0 => {
                let ratio = on_disk as f64 / package.install_bytes as f64;
                log::info!(
                    "{name} verify: {} on disk against a published install size of {} ({:.1}% of it).",
                    fmt_gb(on_disk),
                    fmt_gb(package.install_bytes),
                    ratio * 100.0,
                );
                if ratio < BD2_MIN_SIZE_RATIO {
                    problems.push(format!(
                        "The folder holds {}, but {name} {} installs to about {}, so files are missing. Reinstall to replace them.",
                        fmt_gb(on_disk),
                        package.version,
                        fmt_gb(package.install_bytes),
                    ));
                }
            }
            Ok(_) => log::info!(
                "{name} verify: Neowiz published no install size for this package; skipped the size check."
            ),
            Err(e) => log::warn!(
                "{name} verify: could not reach Neowiz for the published size ({e}); skipped the size check."
            ),
        }

        if !problems.is_empty() {
            log::warn!(
                "{name} verify found {} problem(s): {}",
                problems.len(),
                problems.join(" | ")
            );
            return Err(VerifyError::Other(problems.join(" ")));
        }

        let message = format!(
            "{name} looks intact with {} installed. Peebify did not install this copy, so it has no per-file checksums for it. This checks the shape of the install rather than each file. Update or reinstall through Peebify once and every later check compares every file.",
            fmt_gb(on_disk),
        );
        log::info!("{name} verify: {message}");
        super::queue::publish(
            app,
            "download-progress",
            verify_update(Phase::Done),
            json!({
                "gameId": profile_id,
                "status": "Verification Complete",
                "percentage": 100,
                "message": message,
            }),
        );
        return Ok(ok_with(json!({ "invalidFiles": [], "message": message })));
    };

    if !problems.is_empty() {
        log::warn!(
            "{name} verify found {} problem(s): {}",
            problems.len(),
            problems.join(" | ")
        );
        return Err(VerifyError::Other(problems.join(" ")));
    }

    let total_bytes: u64 = manifest.files.iter().map(|f| f.size).sum();
    let file_count = manifest.files.len();
    log::info!(
        "{name} verify: checksumming {file_count} file(s) recorded for version {}.",
        manifest.version
    );

    let emitter = app.clone();
    let game_id = profile_id.to_string();
    let verify_root = root.clone();
    let verify_cancel = Arc::clone(cancel);
    let files = manifest.files.clone();
    let hashed = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&hashed);
    let last_tick = Mutex::new(std::time::Instant::now());

    let broken = tauri::async_runtime::spawn_blocking(move || {
        bd2::verify_files(&verify_root, &files, &verify_cancel, |bytes| {
            let done = counter.fetch_add(bytes, Ordering::Relaxed) + bytes;
            let mut guard = last_tick.lock();
            if guard.elapsed() < std::time::Duration::from_millis(120) {
                return;
            }
            *guard = std::time::Instant::now();
            drop(guard);
            let percentage = if total_bytes > 0 {
                ((done as f64 / total_bytes as f64) * 100.0).min(100.0)
            } else {
                0.0
            };
            super::queue::publish(
                &emitter,
                "download-progress",
                verify_update(Phase::Verifying),
                json!({
                    "gameId": game_id,
                    "status": "Verifying file integrity...",
                    "percentage": percentage,
                    "processedBytes": done,
                    "totalBytes": total_bytes,
                }),
            );
        })
    })
    .await
    .map_err(|e| VerifyError::Other(format!("verify task panicked: {e}")))?
    .map_err(|e| verify_failure(cancel, e))?;

    let invalid: Vec<Value> = broken
        .iter()
        .map(|f| json!({ "dest": f.path, "size": f.size }))
        .collect();

    let message = if broken.is_empty() {
        format!(
            "{name} is intact. All {file_count} files match the checksums from version {}.",
            manifest.version
        )
    } else {
        format!(
            "{} of {file_count} {name} files do not match version {}. Repair replaces them.",
            broken.len(),
            manifest.version
        )
    };
    log::info!("{name} verify: {message}");
    super::queue::publish(
        app,
        "download-progress",
        verify_result_update(broken.len()),
        json!({
            "gameId": profile_id,
            "status": if broken.is_empty() { "Verification Complete" } else { "Verification Failed" },
            "percentage": 100,
            "message": message,
            "error": if broken.is_empty() { Value::Null } else { json!(message) },
        }),
    );
    Ok(ok_with(
        json!({ "invalidFiles": invalid, "message": message }),
    ))
}

/// Checks every file against the MD5 list Pan Studio publishes for the build. A build behind the latest is not checked against
/// the latest's list; it is reported as needing the update.
async fn dna_verify(
    app: &AppHandle,
    profile: &'static Value,
    profile_id: &str,
    game_path: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, VerifyError> {
    let name = game_profiles::display_name(profile);
    let root = PathBuf::from(game_path);
    let tick = |percentage: u32, status: &str| {
        super::queue::publish(
            app,
            "download-progress",
            verify_update(Phase::Verifying),
            json!({ "gameId": profile_id, "status": status, "percentage": percentage }),
        );
    };
    tick(3, "Verifying installed files...");

    if !root.is_dir() {
        return Err(VerifyError::Other(format!(
            "{name} is no longer in {game_path}. Point Peebify at it again with Locate existing install, or reinstall it."
        )));
    }
    let manifest = super::dna::fetch_manifest(profile)
        .await
        .map_err(|e| VerifyError::Other(format!("Could not reach Pan Studio for the {name} file list: {e}")))?;
    let installed = super::dna::installed_version(&root);
    if installed.is_some_and(|v| v != manifest.latest) {
        let message = format!(
            "{name} is on build {} while build {} is out, so its files are checked after the update.",
            installed.unwrap_or_default(),
            manifest.latest
        );
        log::info!("{name} verify: {message}");
        return Ok(ok_with(json!({ "invalidFiles": [], "message": message, "updatePending": true })));
    }

    tick(6, "Verifying installed files: fetching the file list...");
    let hashes = super::dna::fetch_hashes(profile, &manifest)
        .await
        .map_err(|e| VerifyError::Other(format!("Could not load the {name} file list: {e}")))?;
    let file_count = hashes.len();
    let total_bytes = super::dna::listed_bytes(&root, &hashes);
    log::info!("{name} verify: checksumming {file_count} file(s) listed for build {}.", manifest.latest);

    let emitter = app.clone();
    let game_id = profile_id.to_string();
    let verify_root = root.clone();
    let verify_cancel = Arc::clone(cancel);
    let files = hashes.clone();
    let hashed = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&hashed);
    let last_tick = Mutex::new(std::time::Instant::now());
    let broken = tauri::async_runtime::spawn_blocking(move || {
        super::dna::verify_files(&verify_root, &files, &verify_cancel, |bytes| {
            let done = counter.fetch_add(bytes, Ordering::Relaxed) + bytes;
            let mut guard = last_tick.lock();
            if guard.elapsed() < std::time::Duration::from_millis(120) {
                return;
            }
            *guard = std::time::Instant::now();
            drop(guard);
            let percentage = if total_bytes > 0 {
                ((done as f64 / total_bytes as f64) * 100.0).min(100.0)
            } else {
                0.0
            };
            super::queue::publish(
                &emitter,
                "download-progress",
                verify_update(Phase::Verifying),
                json!({
                    "gameId": game_id,
                    "status": "Verifying file integrity...",
                    "percentage": percentage,
                    "processedBytes": done,
                    "totalBytes": total_bytes,
                }),
            );
        })
    })
    .await
    .map_err(|e| VerifyError::Other(format!("verify task panicked: {e}")))?
    .map_err(|e| verify_failure(cancel, e))?;

    if installed.is_none() && broken.is_empty() {
        let _ = super::dna::write_installed_version(&root, manifest.latest);
    }
    let invalid: Vec<Value> = broken.iter().map(|f| json!({ "dest": f.path, "size": 0 })).collect();
    let message = if broken.is_empty() {
        format!("{name} is intact. All {file_count} files match the checksums for build {}.", manifest.latest)
    } else {
        format!(
            "{} of {file_count} {name} files do not match build {}. Repair replaces them.",
            broken.len(),
            manifest.latest
        )
    };
    log::info!("{name} verify: {message}");
    super::queue::publish(
        app,
        "download-progress",
        verify_result_update(broken.len()),
        json!({
            "gameId": profile_id,
            "status": if broken.is_empty() { "Verification Complete" } else { "Verification Failed" },
            "percentage": 100,
            "message": message,
            "error": if broken.is_empty() { Value::Null } else { json!(message) },
        }),
    );
    Ok(ok_with(json!({ "invalidFiles": invalid, "message": message })))
}

struct VerifyHooks {
    app: AppHandle,
    tracker: Arc<progress::ProgressTracker>,
    game_id: String,
    cancel: Arc<AtomicBool>,
    status: &'static str,
}

impl VerifyHooks {
    fn emit(&self, metrics: &mut Value) {
        if let Some(map) = metrics.as_object_mut() {
            map.insert("status".to_string(), json!(self.status));
            map.insert("gameId".to_string(), json!(self.game_id));
        }
        super::queue::publish(
            &self.app,
            "download-progress",
            verify_update(Phase::Verifying),
            metrics.take(),
        );
    }
}

impl progress::Control for VerifyHooks {
    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

impl reconcile::Hooks for VerifyHooks {
    fn event(&self, event: reconcile::Event) {
        match event {
            reconcile::Event::Phase { .. } => {}
            reconcile::Event::Totals { total_bytes, sizes } => {
                let total_files = sizes.len();
                self.tracker.begin(
                    "validating",
                    total_bytes as f64,
                    total_files,
                    Some(
                        sizes
                            .into_iter()
                            .map(|(path, size)| (path, size as f64))
                            .collect(),
                    ),
                );
            }
            reconcile::Event::Bytes { path, delta, done } => {
                self.tracker.update_file_progress(path, delta as f64, done);
                if self.tracker.should_update_ui() {
                    self.emit(&mut self.tracker.calculate_metrics());
                }
            }
        }
    }
}

impl progress::FileHooks for VerifyHooks {
    fn event(&self, event: progress::FileEvent) {
        match event {
            progress::FileEvent::Bytes { delta, .. } => {
                self.tracker.update_validation_progress(delta as f64);
                if self.tracker.should_update_ui() {
                    self.emit(&mut self.tracker.calculate_metrics());
                }
            }
            progress::FileEvent::FileDone { .. } => {}
        }
    }
}

pub(super) async fn move_game_location(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let profile = match game_file_ops::named_known_profile(arg_str(args, 0)) {
        Ok(profile) => profile,
        Err(refusal) => return Ok(refusal),
    };
    if let Some(refusal) = game_file_ops::refuse_if_running(app, profile).await {
        return Ok(refusal);
    }
    let profile_id = game_profiles::profile_id(profile).to_string();
    if let Some(refusal) = refuse_beside_download(app, &profile_id, "move") {
        return Ok(refusal);
    }
    let state = app.state::<BackendState>();
    let old_path = state
        .config
        .get(&format!("games.{profile_id}.gamePath"))
        .as_str()
        .unwrap_or("")
        .to_string();
    if old_path.is_empty() {
        return Ok(err_response("Current game path is not set."));
    }
    if let Some(app_id) = steam_copy_at(&old_path, profile).await {
        log::info!("Refusing to move {profile_id} because it is the Steam copy (app {app_id}).");
        return Ok(err_response(format!(
            "{} is installed through Steam. Move it from Steam under Properties, Installed Files, then use Locate to point Peebify at the new folder.",
            game_profiles::display_name(profile)
        )));
    }

    let new_path = match game_file_ops::pick_move_destination(app, profile, &old_path).await? {
        Ok(path) => path,
        Err(early_response) => return Ok(early_response),
    };

    let app_for_job = app.clone();
    let run = Box::pin(async move {
        game_file_ops::run_move(&app_for_job, profile, PathBuf::from(old_path), new_path).await
    });
    Ok(run_queued(app, &profile_id, "move", "move", "move-progress", run).await)
}

pub(super) async fn uninstall_game(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let target = arg_str(args, 0).map(str::to_string);
    Ok(game_file_ops::uninstall_game(app, target.as_deref()).await)
}

pub(super) async fn open_leftover_folder(args: &[Value]) -> Result<Value, String> {
    Ok(game_file_ops::open_leftover_folder(arg_str(args, 0)).await)
}

pub(super) async fn select_install_directory(app: &AppHandle) -> Result<Value, String> {
    let dialog_result = crate::backend::fs_util::dialog::show_open(
        app,
        json!({ "title": "Select Custom Installation Folder", "directory": true }),
    )
    .await?;
    let canceled = dialog_result["canceled"].as_bool().unwrap_or(true);
    let path = dialog_result["filePaths"]
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Null);
    Ok(json!({ "canceled": canceled, "path": path }))
}

async fn nte_invalid_files(
    app: &AppHandle,
    game_path: &str,
    profile: &'static Value,
    profile_id: &str,
    cancel: Arc<AtomicBool>,
) -> Result<(Vec<Value>, String), String> {
    let config = nte::fetch_config(profile).await?;
    let list = nte::fetch_reslist(profile, &config).await?;
    let wanted = game_profiles::content_tags(profile_id);
    let mut resources: Vec<nte::Resource> = list.selected(wanted.as_deref()).cloned().collect();
    resources.extend(nte::fetch_launcher(profile).await?.resources);

    let tracker = Arc::new(progress::ProgressTracker::new());
    tracker.set_totals(
        resources.iter().map(|r| r.size).sum::<u64>() as f64,
        resources.len(),
    );
    tracker.set_phase("validating");
    let hooks: Arc<dyn nte::Hooks> = Arc::new(VerifyHooks {
        app: app.clone(),
        tracker,
        game_id: profile_id.to_string(),
        cancel,
        status: "Verifying integrity...",
    });

    let dir = PathBuf::from(game_path);
    let plan = tauri::async_runtime::spawn_blocking(move || {
        nte::plan_scan_parallel(
            &dir,
            &resources,
            nte::ScanMode::Deep,
            super::perf::validation_workers(),
            hooks.as_ref(),
        )
    })
    .await
    .map_err(|e| format!("nte verify planning panicked: {e}"))??;

    let invalid = plan
        .fetch
        .iter()
        .map(|r| json!({ "dest": r.dest, "size": r.size }))
        .collect();
    Ok((invalid, config.res_version))
}

fn pending_update_note(broken: usize, installed: Option<&str>, latest: &str) -> Option<String> {
    let latest = latest.trim();
    let installed = installed.map(str::trim).filter(|v| !v.is_empty())?;
    if broken == 0 || latest.is_empty() || installed == latest {
        return None;
    }
    Some(if broken == 1 {
        format!("1 file differs from version {latest}. An update is available, and it replaces that file.")
    } else {
        format!("{broken} files differ from version {latest}. An update is available, and it replaces these files.")
    })
}

fn older_install_note(broken: usize, installed: Option<&str>, latest: &str) -> Option<String> {
    installed
        .filter(|v| super::game_manager::is_version_newer(latest, v))
        .and_then(|_| pending_update_note(broken, installed, latest))
}

fn installed_language(selected: String, applied: Option<&sophon::AppliedManifest>) -> String {
    let recorded = applied.map(sophon::applied_voice_languages).unwrap_or_default();
    if recorded.is_empty() || recorded.contains(&selected) {
        return selected;
    }
    recorded
        .into_iter()
        .find(|lang| game_profiles::VOICE_LANGUAGES.contains(&lang.as_str()))
        .unwrap_or(selected)
}

async fn load_applied_off_thread(game_path: &Path) -> Option<sophon::AppliedManifest> {
    let dir = game_path.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || sophon::load_applied(&dir))
        .await
        .ok()
        .flatten()
}

async fn installed_language_with(
    app: &AppHandle,
    profile: &Value,
    game_path: &Path,
    build: Option<&sophon::Build>,
    applied: Option<&sophon::AppliedManifest>,
) -> String {
    let selected = game_profiles::resolve_audio_language(app, profile, game_path, build).await;
    let installed = installed_language(selected.clone(), applied);
    if installed != selected {
        log::info!(
            "[voice] {}: checking the installed {installed} voice pack; {selected} is only downloaded on Apply.",
            game_profiles::profile_id(profile)
        );
    }
    installed
}

pub(super) async fn installed_audio_language(
    app: &AppHandle,
    profile: &Value,
    game_path: &Path,
    build: Option<&sophon::Build>,
) -> String {
    let applied = load_applied_off_thread(game_path).await;
    installed_language_with(app, profile, game_path, build, applied.as_ref()).await
}

fn awaits_update(asset: &sophon::Asset, installed: &HashMap<String, sophon::AppliedFile>) -> bool {
    installed
        .get(&asset.name)
        .is_none_or(|f| f.size != asset.size || !f.md5.eq_ignore_ascii_case(&asset.md5))
}

struct SophonVerify {
    broken: Vec<Value>,
    pending: usize,
    installed_tag: Option<String>,
    tag: String,
}

async fn sophon_invalid_files(
    app: &AppHandle,
    game_path: &str,
    profile: &Value,
    profile_id: &str,
    cancel: Arc<AtomicBool>,
) -> Result<SophonVerify, String> {
    let auth = sophon::fetch_branch_auth_for_profile(profile).await?;
    let build = sophon::fetch_build(&auth).await?;

    let applied = load_applied_off_thread(Path::new(game_path)).await;
    let audio_language = installed_language_with(
        app,
        profile,
        Path::new(game_path),
        Some(&build),
        applied.as_ref(),
    )
    .await;
    let installed_tag = applied
        .as_ref()
        .map(|a| a.tag.trim().to_string())
        .filter(|t| !t.is_empty());
    let installed = applied
        .as_ref()
        .filter(|_| installed_tag.as_deref().is_some_and(|t| t != build.tag.trim()))
        .map(sophon::AppliedManifest::file_map);
    let categories = sophon::install_categories(&build, &audio_language);
    let manifests = sophon::fetch_manifests(&categories).await?;

    let (scan_bytes, scan_files) = sophon::scan_totals(&manifests);
    let tracker = Arc::new(progress::ProgressTracker::new());
    tracker.set_totals(scan_bytes as f64, scan_files);
    tracker.set_phase("validating");
    let hooks: Arc<dyn sophon::Hooks> = Arc::new(VerifyHooks {
        app: app.clone(),
        tracker,
        game_id: profile_id.to_string(),
        cancel,
        status: "Verifying integrity...",
    });

    let mut broken = Vec::new();
    let mut pending = 0;
    let mut plans: Vec<(sophon::Category, sophon::Plan)> = Vec::new();
    for (category, assets) in categories.iter().zip(&manifests) {
        let dir = PathBuf::from(game_path);
        let hooks = Arc::clone(&hooks);
        let assets = assets.clone();
        let plan = tauri::async_runtime::spawn_blocking(move || {
            sophon::plan_scan_parallel(
                &dir,
                &assets,
                sophon::ScanMode::Deep,
                sophon::default_scan_workers(),
                hooks.as_ref(),
            )
        })
        .await
        .map_err(|e| format!("sophon verify planning panicked: {e}"))??;
        for a in &plan.assets {
            if installed.as_ref().is_some_and(|m| awaits_update(&a.asset, m)) {
                pending += 1;
            } else {
                broken.push(json!({ "dest": a.asset.name, "size": a.asset.size }));
            }
        }
        plans.push(((*category).clone(), plan));
    }

    *pools(app).sophon_verify_cache.lock() = Some(SophonVerifyCache {
        game_id: profile_id.to_string(),
        tag: build.tag.clone(),
        audio_language,
        at: std::time::Instant::now(),
        plans,
    });
    Ok(SophonVerify {
        broken,
        pending,
        installed_tag,
        tag: build.tag,
    })
}

// ------------ Disk Space And Default Folders ------------
// Free space lookups, the default places Peebify installs games into, and the startup scan that adopts a game it finds already sitting in one of them.
pub(super) async fn get_disk_space(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let target = arg_str(args, 0).unwrap_or("");
    let probe = existing_probe_dir(Path::new(target))
        .or_else(|| existing_probe_dir(&app.state::<BackendState>().user_data))
        .or_else(|| std::env::var("USERPROFILE").ok());
    let Some(probe) = probe else {
        return Ok(err_response("Could not resolve a probe path."));
    };

    match disk_free_total(&probe) {
        Ok((free, total)) => Ok(ok_with(
            json!({ "free": free, "total": total, "path": probe }),
        )),
        Err(e) => {
            log::error!("Failed to read disk space: {e}");
            Ok(err_response(e))
        }
    }
}

pub(super) fn drive_root(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let mut components = Path::new(path).components();
    match components.next() {
        Some(std::path::Component::Prefix(prefix)) => {
            Some(format!("{}\\", prefix.as_os_str().to_string_lossy()))
        }
        _ => None,
    }
}

fn existing_probe_dir(path: &Path) -> Option<String> {
    let dir = path
        .ancestors()
        .find(|p| !p.as_os_str().is_empty() && p.is_dir())?;
    let text = dir.to_string_lossy();
    Some(if text.ends_with('\\') || text.ends_with('/') {
        text.into_owned()
    } else {
        format!("{text}\\")
    })
}

pub(super) fn free_bytes_for(path: &Path) -> Option<u64> {
    let probe = existing_probe_dir(path)?;
    disk_free_total(&probe).ok().map(|(free, _)| free)
}

fn disk_free_total(root: &str) -> Result<(u64, u64), String> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let mut free_available: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_available,
            &mut total,
            &mut total_free,
        )
    };
    if ok == 0 {
        return Err(format!("GetDiskFreeSpaceExW failed for {root}"));
    }
    Ok((free_available, total))
}

pub(super) const GAMES_DIR_NAME: &str = "games";

pub(super) fn launcher_dir() -> PathBuf {
    if cfg!(debug_assertions) {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default()
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_default()
    }
}

pub(super) fn is_protected_root(dir: &Path) -> bool {
    let protected_roots: Vec<String> = [
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "SystemRoot",
        "windir",
    ]
    .iter()
    .filter_map(|var| std::env::var(var).ok())
    .filter(|v| !v.is_empty())
    .map(|v| v.to_lowercase())
    .collect();
    let dir_lower = dir.to_string_lossy().to_lowercase();
    protected_roots
        .iter()
        .any(|root| dir_lower == *root || dir_lower.starts_with(&format!("{root}\\")))
}

pub(super) fn default_install_dirs(profile: &Value) -> Vec<(&'static str, PathBuf)> {
    let folder_name = game_profiles::install_folder_name(profile);

    let mut candidates: Vec<(&'static str, PathBuf)> = Vec::with_capacity(3);

    let dir = launcher_dir();
    if !is_protected_root(&dir) {
        candidates.push(("launcher", dir.join(GAMES_DIR_NAME).join(&folder_name)));
    }

    let home = std::env::var("USERPROFILE").unwrap_or_default();
    if !home.is_empty() {
        candidates.push((
            "user-home",
            Path::new(&home).join("Peebify Games").join(&folder_name),
        ));
    }

    candidates.push(("system-drive", system_drive_games_root().join(&folder_name)));

    candidates.sort_by_key(|(_, dir)| !game_path::fits_path_budget(dir, profile));
    candidates
}

fn system_drive_games_root() -> PathBuf {
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    Path::new(&format!("{system_drive}\\")).join("Peebify Games")
}

fn create_shared_root_for(install_path: &Path, shared_root: &Path) -> bool {
    let root = shared_root.to_string_lossy();
    if !install_path
        .ancestors()
        .any(|dir| same_path(&dir.to_string_lossy(), &root))
    {
        return false;
    }
    match create_restricted_dir(shared_root) {
        Ok(true) => {
            log::info!(
                "Created {} writable by this account only.",
                shared_root.display()
            );
            true
        }
        Ok(false) => false,
        Err(e) => {
            log::warn!(
                "Could not create {} with restricted access: {e}",
                shared_root.display()
            );
            false
        }
    }
}

pub(super) fn install_in_default_dirs(profile: &'static Value) -> Option<String> {
    default_install_dirs(profile)
        .into_iter()
        .find_map(|(_, dir)| {
            if !dir.is_dir() {
                return None;
            }
            let picked = dir.to_string_lossy().to_string();
            let validation = game_path::validate_game_path_for_profile(&picked, profile);
            if !validation.is_valid {
                return None;
            }
            let resolved = validation.resolved_path.unwrap_or(picked);
            if is_unfinished_fresh_install(&dir, Path::new(&resolved)) {
                log::info!(
                    "Startup scan: skipped {resolved} because a fresh install there never finished."
                );
                return None;
            }
            Some(resolved)
        })
}

pub(super) fn is_unfinished_fresh_install(dir: &Path, resolved: &Path) -> bool {
    super::download_engine::install_marker_owns_dir(dir) == Some(true)
        || super::download_engine::install_marker_owns_dir(resolved) == Some(true)
}

fn has_pending_install(config: &super::config::LauncherConfig, profile_id: &str) -> bool {
    config
        .get(&pending_install_key(profile_id))
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|p| !p.is_empty())
}

pub(crate) async fn adopt_default_location_installs(app: &AppHandle) -> Vec<String> {
    game_file_ops::recover_interrupted_moves(app).await;
    let config = app.state::<BackendState>().config.clone();
    let mut adopted = Vec::new();

    for id in game_profiles::GAME_IDS {
        let configured = config.get(&format!("games.{id}.gamePath"));
        if !configured.as_str().unwrap_or("").is_empty() {
            continue;
        }
        if has_pending_install(&config, id) {
            log::debug!("Startup scan: {id} has an unfinished install, so it is not adopted.");
            continue;
        }
        let profile = game_profiles::profile(id);
        let found = tauri::async_runtime::spawn_blocking(move || install_in_default_dirs(profile))
            .await
            .unwrap_or(None);
        let Some(path) = found else {
            continue;
        };
        log::info!(
            "Startup scan: {} is already installed at {path}, adding it to the library.",
            game_profiles::display_name(profile)
        );
        super::config_channels::set_config_value(app, &format!("games.{id}.gamePath"), json!(path));
        game_profiles::resolve_audio_language(app, profile, Path::new(&path), None).await;
        adopted.push(id.to_string());
    }

    if adopted.is_empty() {
        log::debug!("Startup scan: no unregistered games in Peebify's default install folders.");
    }
    adopted
}

pub(super) async fn get_install_path_health(
    app: &AppHandle,
    args: &[Value],
) -> Result<Value, String> {
    let (profile, profile_id) = profile_arg(app, args);
    let game_path = app
        .state::<BackendState>()
        .config
        .get(&format!("games.{profile_id}.gamePath"))
        .as_str()
        .unwrap_or("")
        .to_string();

    let too_deep = (!game_path.is_empty())
        .then(|| game_path::path_budget_error(Path::new(&game_path), profile))
        .flatten();
    let (not_writable, missing) = if game_path.is_empty() {
        (false, None)
    } else {
        let path = game_path.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let not_writable = super::fs_util::dir_denies_writes(Path::new(&path));
            let missing = missing_install_error(game_profiles::display_name(profile), &path);
            (not_writable, missing)
        })
        .await
        .unwrap_or((false, None))
    };
    let message = missing.clone().or_else(|| too_deep.clone()).or_else(|| {
        not_writable.then(|| super::fs_util::not_writable_message(Path::new(&game_path)))
    });

    Ok(ok_with(json!({
        "gameId": profile_id,
        "path": game_path,
        "tooDeep": too_deep.is_some(),
        "notWritable": not_writable,
        "missing": missing.is_some(),
        "message": message,
    })))
}

pub(super) async fn get_default_install_path(
    app: &AppHandle,
    args: &[Value],
) -> Result<Value, String> {
    let opts = args.first().cloned().unwrap_or(Value::Null);
    let profile = resolve_profile(app, opts.get("gameId").and_then(|v| v.as_str()));

    let attempts = default_install_dirs(profile);
    let mut last_error: Option<String> = None;
    for (_, dir) in &attempts {
        match preview_writable(dir) {
            Ok(()) => {
                return Ok(ok_with(json!({
                    "path": dir.to_string_lossy(),
                    "maxRootLength": game_path::max_install_root_len(profile),
                    "folderName": game_profiles::install_folder_name(profile),
                })));
            }
            Err(e) => {
                log::warn!(
                    "Default install path candidate {} unwritable: {e}",
                    dir.display()
                );
                last_error = Some(e);
            }
        }
    }

    let message = match &last_error {
        Some(e) if e.to_lowercase().contains("denied") || e.to_lowercase().contains("access") => {
            "Could not create the default install folder. Pick a folder you have write access to."
                .to_string()
        }
        Some(e) => e.clone(),
        None => "Could not resolve a writable default install path.".to_string(),
    };
    Ok(err_response(message))
}

fn preview_writable(dir: &Path) -> Result<(), String> {
    let Some(existing) = dir
        .ancestors()
        .find(|p| !p.as_os_str().is_empty() && p.is_dir())
    else {
        return Err(format!("{} is not reachable.", dir.display()));
    };
    if existing == dir {
        return if super::fs_util::dir_denies_writes(dir) {
            Err(format!("Access is denied to {}.", dir.display()))
        } else {
            Ok(())
        };
    }
    let probe = existing.join(format!(
        ".peebify-folder-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir(&probe).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::remove_dir(&probe) {
        log::warn!("Could not remove the write probe {}: {e}", probe.display());
    }
    Ok(())
}

fn create_restricted_dir(dir: &Path) -> Result<bool, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE};
    use windows_sys::Win32::Security::{
        AddAccessAllowedAceEx, CreateWellKnownSid, GetLengthSid, GetTokenInformation,
        InitializeAcl, InitializeSecurityDescriptor, SetSecurityDescriptorControl,
        SetSecurityDescriptorDacl, TokenUser, WinAuthenticatedUserSid,
        WinBuiltinAdministratorsSid, WinLocalSystemSid, ACL, ACL_REVISION,
        CONTAINER_INHERIT_ACE, OBJECT_INHERIT_ACE, PSID, SECURITY_ATTRIBUTES,
        SECURITY_DESCRIPTOR, SECURITY_MAX_SID_SIZE, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
        WELL_KNOWN_SID_TYPE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateDirectoryW, FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
    let failed = |what: &str| format!("{what} failed: {}", std::io::Error::last_os_error());

    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(failed("OpenProcessToken"));
    }
    let mut user_buf = vec![0u64; 64];
    let mut needed = 0u32;
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            user_buf.as_mut_ptr().cast(),
            (user_buf.len() * std::mem::size_of::<u64>()) as u32,
            &mut needed,
        )
    };
    let read_error = (read == 0).then(|| failed("GetTokenInformation"));
    unsafe { CloseHandle(token) };
    if let Some(e) = read_error {
        return Err(e);
    }
    let user_sid: PSID = unsafe { (*(user_buf.as_ptr() as *const TOKEN_USER)).User.Sid };

    let well_known = |kind: WELL_KNOWN_SID_TYPE| -> Result<Vec<u8>, String> {
        let mut sid = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = SECURITY_MAX_SID_SIZE;
        let ok = unsafe {
            CreateWellKnownSid(kind, std::ptr::null_mut(), sid.as_mut_ptr().cast(), &mut size)
        };
        if ok == 0 {
            return Err(failed("CreateWellKnownSid"));
        }
        Ok(sid)
    };
    let mut system = well_known(WinLocalSystemSid)?;
    let mut admins = well_known(WinBuiltinAdministratorsSid)?;
    let mut everyone_signed_in = well_known(WinAuthenticatedUserSid)?;
    let entries: [(PSID, u32); 4] = [
        (user_sid, FILE_ALL_ACCESS),
        (system.as_mut_ptr().cast(), FILE_ALL_ACCESS),
        (admins.as_mut_ptr().cast(), FILE_ALL_ACCESS),
        (
            everyone_signed_in.as_mut_ptr().cast(),
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        ),
    ];

    let acl_len = std::mem::size_of::<ACL>() as u32
        + entries
            .iter()
            .map(|(sid, _)| 8 + unsafe { GetLengthSid(*sid) })
            .sum::<u32>();
    let mut acl_buf = vec![0u32; (acl_len as usize).div_ceil(4)];
    let acl = acl_buf.as_mut_ptr() as *mut ACL;
    if unsafe { InitializeAcl(acl, acl_len, ACL_REVISION) } == 0 {
        return Err(failed("InitializeAcl"));
    }
    for (sid, mask) in entries {
        let ok = unsafe {
            AddAccessAllowedAceEx(
                acl,
                ACL_REVISION,
                OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                mask,
                sid,
            )
        };
        if ok == 0 {
            return Err(failed("AddAccessAllowedAceEx"));
        }
    }

    let mut descriptor: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    let descriptor_ptr = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
    if unsafe { InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION) } == 0 {
        return Err(failed("InitializeSecurityDescriptor"));
    }
    if unsafe { SetSecurityDescriptorDacl(descriptor_ptr, 1, acl, 0) } == 0 {
        return Err(failed("SetSecurityDescriptorDacl"));
    }
    if unsafe {
        SetSecurityDescriptorControl(descriptor_ptr, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
    } == 0
    {
        return Err(failed("SetSecurityDescriptorControl"));
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor_ptr,
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
            return Ok(false);
        }
        return Err(format!("CreateDirectoryW failed: {error}"));
    }
    Ok(true)
}

// ------------ Default Location Tests ------------
// Covers the default install folder rules.
#[cfg(test)]
mod default_location_tests {
    use super::*;

    fn dir_for<'a>(dirs: &'a [(&'static str, PathBuf)], kind: &str) -> Option<&'a std::path::Path> {
        dirs.iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, dir)| dir.as_path())
    }

    #[test]
    fn renderer_install_paths_must_name_the_games_own_folder() {
        let profile = game_profiles::profile("wuwa");
        assert_eq!(
            renderer_install_path_refusal(profile, r"D:\Games\Wuthering Waves"),
            None
        );
        assert_eq!(
            renderer_install_path_refusal(profile, r"D:\Games\wuthering waves\"),
            None
        );
        for bad in [
            r"D:\Projects",
            r"C:\",
            r"C:\Users\me",
            r"Games\Wuthering Waves",
            r"D:\x\..\Wuthering Waves",
            r"D:\Games\Wuthering Waves\..",
        ] {
            assert!(
                renderer_install_path_refusal(profile, bad).is_some(),
                "{bad} was accepted"
            );
        }
        for dir in default_install_dirs(profile) {
            assert_eq!(renderer_install_path_refusal(profile, &dir.1.to_string_lossy()), None);
        }
    }

    #[test]
    fn a_game_folder_is_offered_next_to_the_launcher_and_under_the_profile() {
        let dirs = default_install_dirs(game_profiles::profile("wuwa"));

        assert_eq!(dirs.len(), 3, "tests do not run from a protected root");
        assert!(dir_for(&dirs, "launcher")
            .expect("a launcher folder")
            .ends_with(Path::new("games").join("Wuthering Waves")));
        assert!(dir_for(&dirs, "user-home")
            .expect("a profile folder")
            .ends_with(Path::new("Peebify Games").join("Wuthering Waves")));
        assert!(dir_for(&dirs, "system-drive")
            .expect("a drive-root folder")
            .ends_with(Path::new("Peebify Games").join("Wuthering Waves")));
    }

    #[test]
    fn a_name_windows_cannot_use_is_stripped_the_same_way_on_both_sides() {
        let dirs = default_install_dirs(game_profiles::profile("gf2"));

        let launcher = dir_for(&dirs, "launcher").expect("a launcher folder");
        assert!(
            launcher.ends_with(Path::new("games").join("Girls' Frontline 2 Exilium")),
            "unexpected folder: {}",
            launcher.display()
        );
    }

    #[test]
    fn a_game_that_needs_room_is_not_offered_the_deepest_folder_first() {
        let profile = game_profiles::profile("re1999");
        let dirs = default_install_dirs(profile);

        let launcher = dir_for(&dirs, "launcher").expect("a launcher folder");
        if !game_path::fits_path_budget(launcher, profile) {
            assert_ne!(
                dirs[0].0,
                "launcher",
                "offered a folder the game cannot write from: {}",
                launcher.display()
            );
        }
        assert!(
            game_path::fits_path_budget(dirs[0].1.as_path(), profile),
            "the first choice has no room for the game's own paths: {}",
            dirs[0].1.display()
        );
        assert!(
            dirs.iter().any(|(kind, _)| *kind == "launcher"),
            "detection can no longer find installs next to the launcher"
        );
    }

    #[test]
    fn nothing_is_adopted_from_a_folder_that_holds_no_game() {
        let empty = std::env::temp_dir().join(format!(
            "peebify-detect-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&empty).unwrap();

        let validation = game_path::validate_game_path_for_profile(
            &empty.to_string_lossy(),
            game_profiles::profile("wuwa"),
        );

        let _ = std::fs::remove_dir_all(&empty);
        assert!(!validation.is_valid);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "peebify-fc-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn free_space_is_measured_at_the_nearest_folder_that_exists() {
        let dir = scratch("free");
        let missing = dir.join("not").join("yet").join("created");

        let probe = existing_probe_dir(&missing).expect("an existing ancestor");
        assert_eq!(
            Path::new(probe.trim_end_matches('\\')),
            dir.as_path(),
            "probed {probe}"
        );
        assert!(probe.ends_with('\\'));
        assert!(free_bytes_for(&missing).is_some());
        assert!(existing_probe_dir(Path::new("")).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_fresh_install_that_never_finished_blocks_adoption() {
        let dir = scratch("marker");
        let nested = dir.join("Game");
        std::fs::create_dir_all(&nested).unwrap();

        assert!(!is_unfinished_fresh_install(&dir, &nested));

        std::fs::write(dir.join(".peebify-install"), r#"{"ownedDir": false}"#).unwrap();
        assert!(!is_unfinished_fresh_install(&dir, &nested));

        std::fs::write(dir.join(".peebify-install"), r#"{"ownedDir": true}"#).unwrap();
        assert!(is_unfinished_fresh_install(&dir, &nested));

        std::fs::remove_file(dir.join(".peebify-install")).unwrap();
        std::fs::write(nested.join(".peebify-install"), r#"{"ownedDir": true}"#).unwrap();
        assert!(is_unfinished_fresh_install(&dir, &nested));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    fn dacl_is_protected(dir: &Path) -> bool {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::{
            GetFileSecurityW, GetSecurityDescriptorControl, DACL_SECURITY_INFORMATION,
            SE_DACL_PROTECTED,
        };

        let wide: Vec<u16> = dir
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut needed = 0u32;
        unsafe {
            GetFileSecurityW(
                wide.as_ptr(),
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        let read = unsafe {
            GetFileSecurityW(
                wide.as_ptr(),
                DACL_SECURITY_INFORMATION,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut needed,
            )
        };
        assert_ne!(read, 0, "could not read the DACL of {}", dir.display());
        let (mut control, mut revision) = (0u16, 0u32);
        let ok = unsafe {
            GetSecurityDescriptorControl(buf.as_mut_ptr().cast(), &mut control, &mut revision)
        };
        assert_ne!(ok, 0);
        control & SE_DACL_PROTECTED != 0
    }

    #[test]
    fn a_restricted_root_stays_fully_usable_by_this_account() {
        let dir = scratch("acl");
        let root = dir.join("Peebify Games");

        assert_eq!(create_restricted_dir(&root), Ok(true));
        #[cfg(windows)]
        assert!(dacl_is_protected(&root), "the drive root's access was inherited");
        let game = root.join("Some Game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("data.bin"), b"x").unwrap();
        assert_eq!(std::fs::read(game.join("data.bin")).unwrap(), b"x");
        assert_eq!(create_restricted_dir(&root), Ok(false));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_fresh_install_creates_only_a_missing_shared_root() {
        let dir = scratch("shared-root");
        let root = dir.join("Peebify Games");

        assert!(!create_shared_root_for(&dir.join("Elsewhere").join("Game"), &root));
        assert!(!root.exists(), "a folder outside the shared root created it");

        let install = dir.join("peebify games").join("Game");
        assert!(create_shared_root_for(&install, &root));
        assert!(root.is_dir());
        assert!(!install.exists(), "the game folder is left to the install");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_shared_root_the_user_already_has_keeps_its_access() {
        let dir = scratch("shared-root-kept");
        let root = dir.join("Peebify Games");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("notes.txt"), b"mine").unwrap();

        assert!(!create_shared_root_for(&root.join("Game"), &root));
        #[cfg(windows)]
        assert!(!dacl_is_protected(&root), "a folder Peebify did not create was tightened");
        assert_eq!(std::fs::read(root.join("notes.txt")).unwrap(), b"mine");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

// ------------ Verify Result Tests ------------
// Covers how a verify run reports its result.
#[cfg(test)]
mod verify_result_tests {
    use super::*;

    #[test]
    fn only_a_verify_that_checked_every_file_clears_an_unfinished_update() {
        let sophon = json!({ "installMode": "sophon" });
        let clean = json!({ "success": true, "invalidFiles": [], "updatePending": false });
        assert!(verify_left_no_broken_files(&sophon, "", &clean));
        let waiting = json!({ "success": true, "invalidFiles": [], "updatePending": true });
        assert!(!verify_left_no_broken_files(&sophon, "", &waiting));
        let broken = json!({
            "success": true,
            "invalidFiles": [{ "dest": "a.pak" }],
            "updatePending": false,
        });
        assert!(!verify_left_no_broken_files(&sophon, "", &broken));
        let failed = json!({ "success": false, "error": "offline" });
        assert!(!verify_left_no_broken_files(&sophon, "", &failed));
        let bluepoch = json!({ "installMode": "bluepoch" });
        assert!(!verify_left_no_broken_files(&bluepoch, "", &clean));
        let gf2 = json!({ "installMode": "gf2" });
        assert!(!verify_left_no_broken_files(&gf2, "", &clean));
        let bd2 = json!({ "installMode": "bd2" });
        let unrecorded = std::env::temp_dir().join("peebify-verify-no-bd2-manifest");
        assert!(!verify_left_no_broken_files(&bd2, &unrecorded.to_string_lossy(), &clean));
    }

    #[test]
    fn a_clean_verify_is_complete_without_an_error() {
        let payload = verify_result_payload("genshin", 0);
        assert_eq!(payload["status"], "Verification Complete");
        assert!(payload.get("error").is_none());
    }

    #[test]
    fn a_verify_with_broken_files_explains_itself() {
        let payload = verify_result_payload("genshin", 3);
        assert_eq!(payload["status"], "Verification Failed");
        assert_eq!(payload["error"], "3 files need repair. Run a repair to replace them.");
        assert_eq!(broken_files_error(1), "1 file needs repair. Run a repair to replace it.");
    }

    #[test]
    fn only_the_cancel_flag_turns_a_verify_failure_into_a_cancel() {
        let flag = AtomicBool::new(false);
        let aborted = "Write error: The I/O operation has been aborted because of either a thread exit or an application request. (os error 995)".to_string();
        assert!(matches!(verify_failure(&flag, aborted), VerifyError::Other(_)));
        assert!(matches!(
            verify_failure(&flag, "Operation cancelled by user.".to_string()),
            VerifyError::Other(_)
        ));
        flag.store(true, Ordering::SeqCst);
        assert!(matches!(
            verify_failure(&flag, "anything".to_string()),
            VerifyError::Cancelled
        ));
    }

    #[test]
    fn a_pending_update_is_named_only_when_the_recorded_version_is_older() {
        assert_eq!(pending_update_note(3, Some("1.2.0"), "1.2.0"), None);
        assert_eq!(pending_update_note(0, Some("1.1.0"), "1.2.0"), None);
        assert_eq!(pending_update_note(3, None, "1.2.0"), None);
        assert_eq!(pending_update_note(3, Some("1.1.0"), ""), None);
        assert_eq!(
            pending_update_note(3, Some("1.1.0"), "1.2.0").as_deref(),
            Some("3 files differ from version 1.2.0. An update is available, and it replaces these files.")
        );
        assert_eq!(
            pending_update_note(1, Some(" 1.1.0 "), "1.2.0").as_deref(),
            Some("1 file differs from version 1.2.0. An update is available, and it replaces that file.")
        );
    }

    #[test]
    fn only_an_older_install_reads_its_differences_as_the_pending_update() {
        assert!(older_install_note(3, Some("1.1.0"), "1.2.0").is_some());
        assert_eq!(older_install_note(3, Some("1.2.0"), "1.2.0"), None);
        assert_eq!(older_install_note(3, Some("1.2"), "1.2.0"), None);
        assert_eq!(older_install_note(3, Some("1.3.0"), "1.2.0"), None);
        assert_eq!(older_install_note(3, None, "1.2.0"), None);
        assert_eq!(older_install_note(0, Some("1.1.0"), "1.2.0"), None);
    }

    fn applied(voice: &[&str]) -> sophon::AppliedManifest {
        let mut categories = vec![json!({
            "matchingField": "game",
            "manifestId": "m-game",
            "files": [{ "path": "GenshinImpact.exe", "size": 10, "md5": "aaaa" }],
        })];
        for lang in voice {
            categories.push(json!({ "matchingField": lang, "manifestId": "m-voice", "files": [] }));
        }
        serde_json::from_value(json!({
            "format": "sophon-v1",
            "tag": "5.3.0",
            "audioLanguages": voice,
            "categories": categories,
        }))
        .expect("applied manifest")
    }

    #[test]
    fn verify_checks_the_installed_voice_pack_not_one_only_picked() {
        let installed = applied(&["en-us"]);
        assert_eq!(installed_language("ja-jp".to_string(), Some(&installed)), "en-us");
        assert_eq!(installed_language("en-us".to_string(), Some(&installed)), "en-us");
        let both = applied(&["en-us", "ja-jp"]);
        assert_eq!(installed_language("ja-jp".to_string(), Some(&both)), "ja-jp");
        assert_eq!(installed_language("ja-jp".to_string(), None), "ja-jp");
        assert_eq!(installed_language("ja-jp".to_string(), Some(&applied(&[]))), "ja-jp");
    }

    #[test]
    fn only_files_the_installed_build_already_had_count_as_broken() {
        let installed = applied(&[]).file_map();
        let asset = |name: &str, size: u64, md5: &str| sophon::Asset {
            name: name.to_string(),
            chunks: Vec::new(),
            asset_type: 0,
            size,
            md5: md5.to_string(),
        };
        assert!(!awaits_update(&asset("GenshinImpact.exe", 10, "AAAA"), &installed));
        assert!(awaits_update(&asset("GenshinImpact.exe", 10, "bbbb"), &installed));
        assert!(awaits_update(&asset("GenshinImpact.exe", 11, "aaaa"), &installed));
        assert!(awaits_update(&asset("NewInPatch.pak", 5, "cccc"), &installed));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "peebify-u054-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_install_folder_is_named_before_any_work_starts() {
        let dir = scratch("present");
        assert_eq!(missing_install_error("Wuthering Waves", &dir.to_string_lossy()), None);
        let gone = dir.join("Wuthering Waves");
        let message = missing_install_error("Wuthering Waves", &gone.to_string_lossy()).unwrap();
        assert!(message.starts_with("The Wuthering Waves folder "));
        assert!(message.contains("is missing"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn an_unplugged_drive_is_told_apart_from_a_missing_folder() {
        let Some(letter) = ('D'..='Z')
            .rev()
            .find(|l| !Path::new(&format!("{l}:\\")).exists())
        else {
            return;
        };
        let path = format!("{letter}:\\Games\\Wuthering Waves");
        let message = missing_install_error("Wuthering Waves", &path).unwrap();
        assert_eq!(
            message,
            format!("The drive that holds Wuthering Waves ({letter}:\\) is not connected. Reconnect it and try again.")
        );
        assert_eq!(drive_offline_error("Wuthering Waves", &path), Some(message));
    }

    #[test]
    fn a_connected_drive_is_never_reported_offline() {
        let dir = scratch("online");
        let gone = dir.join("Wuthering Waves");
        assert_eq!(drive_offline_error("Wuthering Waves", &gone.to_string_lossy()), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_raw_picked_folder_gets_the_games_own_subfolder() {
        assert_eq!(with_game_folder(r"D:\Games", "Wuthering Waves"), r"D:\Games\Wuthering Waves");
        assert_eq!(with_game_folder(r"D:\Games\", "Wuthering Waves"), r"D:\Games\Wuthering Waves");
        assert_eq!(with_game_folder(r"D:\", "Wuthering Waves"), r"D:\Wuthering Waves");
        assert_eq!(
            with_game_folder(r"D:\Games\wuthering waves\", "Wuthering Waves"),
            r"D:\Games\wuthering waves"
        );
    }

    #[test]
    fn paths_compare_without_case_slashes_or_a_trailing_separator() {
        assert!(same_path(r"D:\Games\WW\", "d:/games/ww"));
        assert!(!same_path(r"D:\Games\WW", r"E:\Games\WW"));
    }

    #[test]
    fn a_preview_probe_never_leaves_a_folder_behind() {
        let dir = scratch("preview");
        let target = dir.join("Peebify Games").join("Game");
        assert_eq!(preview_writable(&target), Ok(()));
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(preview_writable(&dir), Ok(()));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_games_folder_matches_the_one_the_installer_spares() {
        let installer = include_str!("../../../installer/src/consts.rs");
        let declared = format!("pub const GAMES_DIR_NAME: &str = \"{GAMES_DIR_NAME}\";");
        assert!(installer.contains(&declared));
    }
}
