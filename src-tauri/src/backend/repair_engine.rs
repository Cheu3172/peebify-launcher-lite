// ------------ Game Repair ------------
// Checks an installed game against its file index and re-downloads whatever is missing
// or corrupt. There is one manager per game, and it has a quick mode (local index) and a
// full mode (fresh index). Games with their own installers each get their own flow.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::io::AsyncWriteExt;

use super::download_engine::combine_url;
use super::fs_util::ContentHasher;
use super::game_profiles::InstallMode;
use super::queue::{Phase, Update};
use super::state::BackendState;
use super::validator::{FileCheck, FileValidator, Resource};
use super::{
    bd2, download_engine, game_profiles, http, hypergryph_reconcile as reconcile, nte,
    progress, sophon, yostar,
};

pub mod status {
    pub const FETCHING_CONFIG: &str = "Fetching configuration...";
    pub const FETCHING_INDEX: &str = "Fetching file index...";
    pub const VALIDATING: &str = "Validating";
    pub const REPAIRING: &str = "Repairing";
    pub const UNPACKING: &str = "Unpacking the client package";
    pub const PAUSED: &str = "Paused";
    pub const COMPLETED: &str = "Completed";
    pub const CANCELLED: &str = "Cancelled";
    pub const ERROR: &str = "Error";
}

pub(super) const SCAN_PHASE: Phase = Phase::Verifying;

const MAX_REPAIR_RETRIES: u32 = 10;
const RETRY_DELAY_BASE_MS: u64 = 1000;
const LOGGED_INVALID_FILES: usize = 25;

pub struct GameRepairManager {
    app: AppHandle,
    tracker: Arc<progress::ProgressTracker>,
    profile: Mutex<&'static Value>,
    is_repairing: AtomicBool,
    starting: AtomicBool,
    gate: progress::RunGate,
    power: Mutex<std::sync::Weak<super::perf::TransferGuard>>,
}

impl GameRepairManager {
    pub fn new(app: AppHandle, profile: &'static Value) -> Arc<Self> {
        Arc::new(Self {
            app,
            tracker: Arc::new(progress::ProgressTracker::new()),
            profile: Mutex::new(profile),
            is_repairing: AtomicBool::new(false),
            starting: AtomicBool::new(false),
            gate: progress::RunGate::new(),
            power: Mutex::new(std::sync::Weak::new()),
        })
    }

    fn set_paused_power(&self, paused: bool) {
        if let Some(power) = self.power.lock().upgrade() {
            power.set_paused(paused);
        }
    }

    fn set_offline_power(&self, offline: bool) {
        if let Some(power) = self.power.lock().upgrade() {
            power.set_offline(offline);
        }
    }

    pub fn is_paused(&self) -> bool {
        self.is_repairing.load(Ordering::SeqCst) && self.gate.is_paused()
    }

    fn stage_phase(&self) -> Phase {
        match self.tracker.phase().as_str() {
            "repairing" => Phase::Repairing,
            "extracting" => Phase::Extracting,
            "validating" => SCAN_PHASE,
            _ => Phase::Downloading,
        }
    }

    pub fn pause_repair(&self) {
        if self.tracker.phase() == "extracting" {
            log::info!("Pause ignored because unpacking cannot be paused.");
            return;
        }
        if self.is_repairing.load(Ordering::SeqCst) && self.gate.pause() {
            if self.tracker.phase() == "extracting" {
                self.gate.resume();
                log::info!("Pause ignored because unpacking cannot be paused.");
                return;
            }
            self.set_paused_power(true);
            self.send_progress(self.stage_phase(), json!({ "status": status::PAUSED }));
            log::info!("Repair paused ({}).", self.profile_id());
        }
    }

    pub fn resume_repair(&self) {
        if self.is_repairing.load(Ordering::SeqCst) && self.gate.resume() {
            self.set_paused_power(false);
            self.tracker.reset_speed_baseline();
            let phase = self.stage_phase();
            let status = match phase {
                Phase::Repairing => status::REPAIRING,
                Phase::Extracting => status::UNPACKING,
                _ => status::VALIDATING,
            };
            self.send_progress(Update::new(phase).resumed(), json!({ "status": status }));
            log::info!("Repair resumed ({}).", self.profile_id());
        }
    }

    pub fn set_profile(&self, profile: &'static Value) {
        *self.profile.lock() = profile;
    }

    fn profile(&self) -> &'static Value {
        *self.profile.lock()
    }

    fn profile_id(&self) -> String {
        game_profiles::profile_id(self.profile()).to_string()
    }

    fn is_cancelled(&self) -> bool {
        self.gate.is_cancelled()
    }

    async fn cancellable<T, F>(&self, operation: F) -> Result<T, String>
    where
        F: std::future::Future<Output = Result<T, String>>,
    {
        self.gate.cancellable(operation).await
    }

    async fn wait_while_paused(&self) {
        self.gate.wait_while_paused().await;
    }

    fn block_while_paused(&self) {
        while self.gate.is_paused() && !self.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    pub fn cancel_repair(&self) {
        if self.is_repairing.load(Ordering::SeqCst) || self.starting.load(Ordering::SeqCst) {
            log::info!("Cancelling repair ({})...", self.profile_id());
            self.gate.cancel();
        }
    }

    pub fn prepare_start(&self) {
        if self.is_repairing.load(Ordering::SeqCst) {
            return;
        }
        self.gate.reset();
        self.starting.store(true, Ordering::SeqCst);
    }

    pub fn abandon_start(&self) {
        self.starting.store(false, Ordering::SeqCst);
    }

    fn send_progress(&self, update: impl Into<Update>, data: Value) {
        let update = update.into();
        let paused = self.gate.is_paused() && !update.phase().is_terminal();
        let mut payload = data;
        if let Some(map) = payload.as_object_mut() {
            map.insert("gameId".to_string(), json!(self.profile_id()));
            if paused {
                map.insert("status".to_string(), json!(status::PAUSED));
            }
        }
        super::queue::publish(&self.app, "repair-progress", update.paused(paused), payload);
    }

    fn send_metrics_progress(&self, update: impl Into<Update>, status_text: &str, extra: Value) {
        let metrics = self.tracker.calculate_metrics();
        let mut payload = json!({ "status": status_text });
        if let (Some(target), Some(src)) = (payload.as_object_mut(), metrics.as_object()) {
            for (k, v) in src {
                target.entry(k.clone()).or_insert_with(|| v.clone());
            }
            if let Value::Object(extra_map) = extra {
                for (k, v) in extra_map {
                    target.insert(k, v);
                }
            }
        }
        self.send_progress(update, payload);
    }

    pub async fn repair_game(self: &Arc<Self>, game_path: &Path, mode: &str) {
        if self.is_repairing.swap(true, Ordering::SeqCst) {
            self.send_progress(Phase::Error, json!({
                "status": status::ERROR,
                "error": "Repair process is already running.",
            }));
            return;
        }
        if !self.starting.swap(false, Ordering::SeqCst) {
            self.gate.reset();
        }
        self.tracker.reset();
        let start = chrono::Utc::now().timestamp_millis();

        let power = Arc::new(super::perf::TransferGuard::acquire());
        *self.power.lock() = Arc::downgrade(&power);
        if self.gate.is_paused() {
            power.set_paused(true);
        }
        let result = if self.is_cancelled() {
            Err("Repair aborted".to_string())
        } else {
            self.run_repair(game_path, mode, start).await
        };
        if let Err(e) = result {
            self.handle_repair_error(&e);
        }

        self.is_repairing.store(false, Ordering::SeqCst);
        self.tracker.reset();
    }

    // ------------ Reverse 1999 Repair ------------
    // Reverse 1999 downloads its updates itself and publishes no file list, so repair can
    // only clear its leftover update staging folders and let the game fetch them again.
    async fn run_bluepoch_repair(
        self: &Arc<Self>,
        game_path: &Path,
        start_ms: i64,
    ) -> Result<(), String> {
        let profile = self.profile();
        let name = game_profiles::display_name(profile);

        if let Some(too_deep) = super::game_path::path_budget_error(game_path, profile) {
            return Err(format!(
                "{too_deep} {name} downloads its updates itself, and it cannot write them from here. That is what the \"Could not find a part of the path\" notice in the game means. Move the install from Settings, Games, Install location, then start the game again."
            ));
        }

        let exe = game_profiles::client_process_name(profile);

        self.send_progress(Phase::Repairing, json!({
            "status": status::REPAIRING,
            "message": format!("Clearing {name}'s update staging files..."),
        }));

        let root = game_path.to_path_buf();
        let exe_name = exe.to_string();
        let cleared =
            tauri::async_runtime::spawn_blocking(move || clear_bluepoch_staging(&root, &exe_name))
                .await
                .unwrap_or(0);

        let duration = (chrono::Utc::now().timestamp_millis() - start_ms) as f64 / 1000.0;
        let message = if cleared > 0 {
            format!(
                "Cleared {cleared} leftover update folder{} in {duration:.1}s. Start {name} to download its update again.",
                if cleared == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "Nothing to clean up. {name} publishes no per-file list, so Peebify cannot checksum it. If the game still reports damaged files, reinstall it."
            )
        };
        log::info!("{name} repair: {message}");

        let state = self.app.state::<BackendState>();
        state.game.clear_update_cache(&self.profile_id());
        self.send_progress(Phase::Done, json!({
            "status": status::COMPLETED,
            "message": message,
        }));
        Ok(())
    }

    // ------------ Brown Dust 2 Repair ------------
    // Brown Dust 2 comes as one package, so repair checks files against a saved manifest
    // and downloads and unpacks the package again when something is off.
    async fn run_bd2_repair(
        self: &Arc<Self>,
        game_path: &Path,
        start_ms: i64,
    ) -> Result<(), String> {
        let profile = self.profile();
        let name = game_profiles::display_name(profile);

        let manifest = bd2::load_manifest(game_path);
        let broken = match &manifest {
            Some(manifest) => {
                let file_count = manifest.files.len();
                let total_bytes: u64 = manifest.files.iter().map(|f| f.size).sum();

                self.tracker
                    .begin("validating", total_bytes as f64, file_count, None);
                self.send_metrics_progress(SCAN_PHASE, status::VALIDATING, json!({}));

                let scan_engine = Arc::clone(self);
                let scan_dir = game_path.to_path_buf();
                let scan_files = manifest.files.clone();
                let scan_cancel = Arc::clone(self.gate.flag());
                let broken = tauri::async_runtime::spawn_blocking(move || {
                    bd2::verify_files(&scan_dir, &scan_files, &scan_cancel, |bytes| {
                        scan_engine.block_while_paused();
                        scan_engine.tracker.update_validation_progress(bytes as f64);
                        if scan_engine.tracker.should_update_ui() {
                            scan_engine.send_metrics_progress(
                                SCAN_PHASE,
                                status::VALIDATING,
                                json!({}),
                            );
                        }
                    })
                })
                .await
                .map_err(|e| format!("{name} repair scan panicked: {e}"))??;

                if broken.is_empty() {
                    log::info!(
                        "{name} repair: all {file_count} files match version {}.",
                        manifest.version
                    );
                    self.clear_unfinished_update();
                    self.handle_repair_complete(start_ms, 0, file_count);
                    return Ok(());
                }

                log::info!(
                    "{name} repair: {} of {file_count} files do not match version {}; fetching the client package to replace them.",
                    broken.len(),
                    manifest.version
                );
                Some(broken)
            }
            None => {
                log::info!(
                    "{name} repair: no per-file record for this install, so the whole client package is unpacked over it to create one."
                );
                None
            }
        };

        let fetch_phase = if manifest.is_some() {
            SCAN_PHASE
        } else {
            Phase::Downloading
        };
        self.send_progress(fetch_phase, json!({ "status": status::FETCHING_CONFIG }));
        let package = self.cancellable(bd2::fetch_package(profile)).await?;

        if let Some(manifest) = manifest
            .as_ref()
            .filter(|m| !download_engine::versions_semver_equal(&m.version, &package.version))
        {
            log::info!(
                "{name} repair: the install is on version {} while the client package is {}.",
                manifest.version,
                package.version
            );
            return Err(format!(
                "An update for {name} is available. Update it instead, which replaces every file."
            ));
        }

        let archive = crate::backend::fs_util::safe_join(game_path, &package.file_name)?;
        let reusable = bd2::reusable_package(game_path, &package.file_name, package.download_bytes);
        download_engine::ensure_disk_space(
            game_path,
            if reusable.is_some() {
                0
            } else {
                package.download_bytes
            },
            1.0,
            download_engine::HEADROOM_REPAIR,
        )?;

        let fix_count = broken.as_ref().map_or(1, Vec::len);
        self.tracker.reset();
        self.tracker
            .set_totals(package.download_bytes as f64, fix_count);
        self.tracker.set_phase("repairing");
        self.send_metrics_progress(Phase::Repairing, status::REPAIRING, json!({}));

        if reusable.is_some() {
            log::info!(
                "{name} repair: {} is already complete on disk, so it is used without downloading it again.",
                package.file_name
            );
        } else {
            self.download_package(&package.url, &archive).await?;
        }

        let wanted: Option<std::collections::HashSet<String>> = broken
            .as_ref()
            .map(|broken| broken.iter().map(|f| f.path.clone()).collect());
        let extract_wanted = wanted.clone();

        self.wait_while_paused().await;
        if self.is_cancelled() {
            return Err("Repair aborted".to_string());
        }

        self.tracker.reset();
        self.tracker
            .set_totals(package.install_bytes as f64, fix_count);
        self.tracker.set_phase("extracting");
        if self.gate.resume() {
            self.set_paused_power(false);
            log::info!("Unpacking cannot pause, so the pending pause was lifted.");
        }
        self.tracker.force_next_update();
        self.send_metrics_progress(Phase::Extracting, status::UNPACKING, json!({}));

        let extract_engine = Arc::clone(self);
        let extract_dir = game_path.to_path_buf();
        let extract_archive = archive.clone();
        let extract_cancel = Arc::clone(self.gate.flag());
        let expect_crc = package.tied_crc;
        let expect_bytes = package.install_bytes;
        let files = tauri::async_runtime::spawn_blocking(move || {
            bd2::extract_package(
                &extract_archive,
                &extract_dir,
                extract_wanted.as_ref(),
                expect_crc,
                expect_bytes,
                &extract_cancel,
                |written| {
                    extract_engine
                        .tracker
                        .set_file_progress_absolute("package", written as f64);
                    if extract_engine.tracker.should_update_ui() {
                        extract_engine.send_metrics_progress(
                            Phase::Extracting,
                            status::UNPACKING,
                            json!({}),
                        );
                    }
                },
            )
        })
        .await
        .map_err(|e| format!("{name} unpack task panicked: {e}"))?;

        let spent = match &files {
            Ok(_) => true,
            Err(e) => bd2::is_damaged_package_error(e),
        };
        if spent {
            let _ = std::fs::remove_file(&archive);
        }
        let files = files?;

        let mut all_replaced = true;
        let repaired = match &wanted {
            Some(wanted) => {
                let written = files.iter().filter(|f| wanted.contains(&f.path)).count();
                if written < wanted.len() {
                    all_replaced = false;
                    log::warn!(
                        "{name} repair: {} damaged file(s) are not in the {} package.",
                        wanted.len() - written,
                        package.version
                    );
                }
                written
            }
            None => files.len(),
        };

        if let Err(e) = bd2::write_manifest(game_path, &package.version, &files) {
            log::warn!("{name} repair: could not refresh {}: {e}", bd2::MANIFEST_FILE);
        }
        if let Err(e) = bd2::write_launcher_settings(game_path, &package.settings) {
            log::warn!("{name} repair: {e}");
        }
        if manifest.is_none() {
            if let Err(e) = download_engine::update_game_config_file(game_path, &package.version) {
                log::warn!(
                    "{name} repair: could not record version {} for the install: {e}",
                    package.version
                );
            }
        }

        let validated = manifest.as_ref().map_or(files.len(), |m| m.files.len());
        for _ in 0..repaired {
            self.tracker.increment_repaired_files();
        }
        if all_replaced {
            self.clear_unfinished_update();
        }
        self.handle_repair_complete(start_ms, repaired, validated);
        Ok(())
    }

    // ------------ Standard Repair Flow ------------
    // The entry point for every repair. It hands off to the game-specific flows when the
    // game needs one, and otherwise runs the plain index, validate and re-download loop.
    /// Every file is checked against the build's published MD5 list. Pan Studio only publishes whole-game archives, so any damage
    /// is fixed by unpacking the full archive over the folder again, as the official launcher's repair does.
    async fn run_dna_repair(self: &Arc<Self>, game_path: &Path, start_ms: i64) -> Result<(), String> {
        let profile = self.profile();
        let name = game_profiles::display_name(profile);

        self.send_progress(SCAN_PHASE, json!({ "status": status::FETCHING_CONFIG }));
        let manifest = self.cancellable(super::dna::fetch_manifest(profile)).await?;
        let installed = super::dna::installed_version(game_path);
        if installed.is_some_and(|v| v != manifest.latest) {
            return Err(format!(
                "An update for {name} is available. Update it instead, which replaces every file."
            ));
        }
        let hashes = self.cancellable(super::dna::fetch_hashes(profile, &manifest)).await?;
        let file_count = hashes.len();
        let total_bytes = super::dna::listed_bytes(game_path, &hashes);

        self.tracker.begin("validating", total_bytes as f64, file_count, None);
        self.send_metrics_progress(SCAN_PHASE, status::VALIDATING, json!({}));
        let scan_engine = Arc::clone(self);
        let scan_dir = game_path.to_path_buf();
        let scan_cancel = Arc::clone(self.gate.flag());
        let scan_hashes = hashes.clone();
        let broken = tauri::async_runtime::spawn_blocking(move || {
            super::dna::verify_files(&scan_dir, &scan_hashes, &scan_cancel, |bytes| {
                scan_engine.block_while_paused();
                scan_engine.tracker.update_validation_progress(bytes as f64);
                if scan_engine.tracker.should_update_ui() {
                    scan_engine.send_metrics_progress(SCAN_PHASE, status::VALIDATING, json!({}));
                }
            })
        })
        .await
        .map_err(|e| format!("{name} repair scan panicked: {e}"))??;

        if broken.is_empty() {
            log::info!("{name} repair: all {file_count} files match build {}.", manifest.latest);
            if installed.is_none() {
                let _ = super::dna::write_installed_version(game_path, manifest.latest);
            }
            self.clear_unfinished_update();
            self.handle_repair_complete(start_ms, 0, file_count);
            return Ok(());
        }
        log::info!(
            "{name} repair: {} of {file_count} files do not match build {}; unpacking the full archive over the install.",
            broken.len(),
            manifest.latest
        );

        let hpatchz = super::fs_util::resource(&self.app, super::dna::HPATCHZ).ok_or_else(|| {
            format!("{name} is repaired with {}, which is missing from Peebify's install. Reinstall Peebify to restore it.", super::dna::HPATCHZ)
        })?;
        self.send_progress(Phase::Downloading, json!({ "status": status::FETCHING_CONFIG }));
        let (_, package) = self.cancellable(super::dna::fetch_package(profile, installed, true)).await?;
        let patch_dir = game_path.join(super::dna::PATCH_DIR);
        std::fs::create_dir_all(&patch_dir)
            .map_err(|e| crate::backend::fs_util::fmt_io("Could not create the download folder", &e))?;
        let archive = crate::backend::fs_util::safe_join(&patch_dir, &package.file_name)?;
        download_engine::ensure_disk_space(game_path, package.download_bytes, 1.0, download_engine::HEADROOM_REPAIR)?;

        self.tracker.reset();
        self.tracker.set_totals(package.download_bytes as f64, broken.len());
        self.tracker.set_phase("repairing");
        self.send_metrics_progress(Phase::Repairing, status::REPAIRING, json!({}));
        let url = package.urls.first().ok_or("no download mirror for the archive")?;
        self.download_package(url, &archive).await?;

        self.wait_while_paused().await;
        if self.is_cancelled() {
            return Err("Repair aborted".to_string());
        }
        if self.gate.resume() {
            self.set_paused_power(false);
            log::info!("Unpacking cannot pause, so the pending pause was lifted.");
        }
        self.tracker.reset();
        self.tracker.set_totals(package.install_bytes as f64, broken.len());
        self.tracker.set_phase("extracting");
        self.tracker.force_next_update();
        self.send_metrics_progress(Phase::Extracting, status::UNPACKING, json!({}));

        let patched = self
            .cancellable(download_engine::run_hpatchz(&hpatchz, None, &archive, game_path))
            .await;
        if patched.is_ok() {
            let _ = std::fs::remove_dir_all(&patch_dir);
        }
        patched?;

        if let Err(e) = super::dna::write_installed_version(game_path, package.version) {
            log::warn!("{name} repair: {e}");
        }
        if let Err(e) = download_engine::update_game_config_file(game_path, &package.version.to_string()) {
            log::warn!("{name} repair: could not record build {}: {e}", package.version);
        }
        for _ in 0..broken.len() {
            self.tracker.increment_repaired_files();
        }
        self.clear_unfinished_update();
        self.handle_repair_complete(start_ms, broken.len(), file_count);
        Ok(())
    }

    async fn run_repair(
        self: &Arc<Self>,
        game_path: &Path,
        mode: &str,
        start_ms: i64,
    ) -> Result<(), String> {
        let profile = self.profile();
        let mut base_url = String::new();
        let resources: Vec<Resource>;
        let mut from_remote_index = false;

        if let Some(message) = super::game_file_ops::running_refusal(&self.app, profile).await {
            return Err(message);
        }

        match InstallMode::of(profile) {
            InstallMode::Hypergryph => {
                return self.run_hypergryph_reconcile(game_path, start_ms).await;
            }
            InstallMode::Sophon => return self.run_sophon_repair(game_path, mode, start_ms).await,
            InstallMode::Netease => return self.run_nte_repair(game_path, mode, start_ms).await,
            InstallMode::Bluepoch => return self.run_bluepoch_repair(game_path, start_ms).await,
            InstallMode::Bd2 => return self.run_bd2_repair(game_path, start_ms).await,
            InstallMode::Dna => return self.run_dna_repair(game_path, start_ms).await,
            InstallMode::Default | InstallMode::Gf2 | InstallMode::Yostar | InstallMode::Unknown => {}
        }

        let mut remote_version: Option<String> = None;
        if mode == "quick" {
            match self.local_resources(game_path) {
                Ok(local) => {
                    resources = local;
                    log::info!("Quick repair using local index: {} files", resources.len());
                }
                Err(local_error) => {
                    log::warn!("Local index failed ({local_error}), falling back to remote index");
                    let config = self.fetch_game_config(game_path, false).await?;
                    resources = config.resources;
                    base_url = config.base_url;
                    remote_version = Some(config.version);
                    from_remote_index = true;
                    log::info!("Quick repair using remote index: {} files", resources.len());
                }
            }
        } else {
            let config = self.fetch_game_config(game_path, false).await?;
            resources = config.resources;
            base_url = config.base_url;
            remote_version = Some(config.version);
            from_remote_index = true;
            log::info!("Full repair using remote index: {} files", resources.len());
        }

        if let Some(version) = &remote_version {
            self.refuse_stale_repair(game_path, version)?;
        }

        if resources.is_empty() {
            return Err("Invalid game index structure.".to_string());
        }

        if from_remote_index {
            self.sync_local_index(game_path, &resources).await;
        }

        let corrupt = self.validate_files(&resources, game_path, mode).await?;

        if self.is_cancelled() {
            return Err("cancelled".to_string());
        }

        let records_scan = from_remote_index && mode != "quick" && self.prunes_removed_resources();

        if corrupt.is_empty() {
            if records_scan {
                self.record_scan(game_path, &resources).await;
                self.record_verified_version(game_path, remote_version.as_deref());
            }
            let client = self.maybe_reconcile_gf2_client(game_path, mode).await?;
            if from_remote_index && mode != "quick" && client.is_some() {
                self.clear_unfinished_update();
            }
            self.handle_repair_complete(start_ms, client.unwrap_or(0), resources.len());
            return Ok(());
        }

        if mode == "quick" && base_url.is_empty() {
            let config = self.fetch_game_config(game_path, true).await?;
            self.refuse_stale_repair(game_path, &config.version)?;
            base_url = config.base_url;
        }

        self.repair_corrupt_files(corrupt, &base_url, game_path)
            .await?;

        if self.is_cancelled() {
            return Err("cancelled".to_string());
        }
        if records_scan {
            self.record_scan(game_path, &resources).await;
            self.record_verified_version(game_path, remote_version.as_deref());
        }

        let mut repaired = self.tracker.repaired_files();
        let client = self.maybe_reconcile_gf2_client(game_path, mode).await?;
        repaired += client.unwrap_or(0);
        if from_remote_index && mode != "quick" && client.is_some() {
            self.clear_unfinished_update();
        }
        self.handle_repair_complete(start_ms, repaired, resources.len());
        Ok(())
    }

    async fn maybe_reconcile_gf2_client(
        self: &Arc<Self>,
        game_path: &Path,
        mode: &str,
    ) -> Result<Option<usize>, String> {
        let is_gf2 = InstallMode::of(self.profile()) == InstallMode::Gf2;
        if !is_gf2 {
            return Ok(Some(0));
        }
        if mode == "quick" {
            return Ok(None);
        }
        let Some(manifest) = reconcile::load_manifest(game_path) else {
            log::info!(
                "GF2: no client manifest recorded, so Full Repair covered asset bundles only. Reinstalling the game once records one."
            );
            return Ok(None);
        };

        log::info!(
            "GF2: checking {} client files against the recorded manifest...",
            manifest.files.len()
        );
        self.send_progress(SCAN_PHASE, json!({ "status": status::VALIDATING }));
        let hooks = ReconcileRepairHooks {
            mgr: Arc::clone(self),
            phase: Mutex::new("validating"),
        };
        let dir = game_path.to_path_buf();
        let cancelled = Arc::clone(self.gate.flag());
        let handle = tokio::runtime::Handle::current();
        let stats = match tauri::async_runtime::spawn_blocking(move || {
            reconcile::repair(&dir, &manifest, cancelled, &hooks, handle)
        })
        .await
        .map_err(|e| format!("gf2 client repair panicked: {e}"))?
        {
            Ok(stats) => stats,
            Err(e) if reconcile::is_unhosted(&e) => {
                log::warn!("GF2 client: {e} Asset bundles were still repaired.");
                return Ok(None);
            }
            Err(e) => return Err(e),
        };

        if stats.repaired > 0 {
            log::info!(
                "GF2 client: repaired {} of {} files.",
                stats.repaired,
                stats.validated
            );
        }
        Ok(Some(stats.repaired))
    }

    fn record_verified_version(&self, game_path: &Path, version: Option<&str>) {
        let Some(version) = version.filter(|v| !v.is_empty()) else {
            return;
        };
        if InstallMode::of(self.profile()) != InstallMode::Yostar {
            return;
        }
        if let Err(e) = download_engine::update_game_config_file(game_path, version) {
            log::warn!("Could not record version {version} after the full repair: {e}");
        }
    }

    fn refuse_stale_repair(&self, game_path: &Path, remote_version: &str) -> Result<(), String> {
        let profile = self.profile();
        if !matches!(
            InstallMode::of(profile),
            InstallMode::Default | InstallMode::Gf2 | InstallMode::Yostar
        ) {
            return Ok(());
        }
        let local = super::game_manager::local_game_version_for(profile, &game_path.to_string_lossy());
        match local {
            Some(local) if super::game_manager::is_version_newer(remote_version, &local) => {
                log::info!(
                    "Repair stopped: version {remote_version} is available while the install is on {local}."
                );
                Err("An update is available. Update the game first, then repair.".to_string())
            }
            _ => Ok(()),
        }
    }

    fn prunes_removed_resources(&self) -> bool {
        download_engine::prunes_removed(InstallMode::of(self.profile()))
    }

    async fn sync_local_index(&self, game_path: &Path, resources: &[Resource]) {
        if !self.prunes_removed_resources() {
            return;
        }
        let dir = game_path.to_path_buf();
        let next = resources.to_vec();
        let removed = tauri::async_runtime::spawn_blocking(move || {
            let removed = download_engine::prune_removed_resources(&dir, &next);
            if let Err(e) = download_engine::write_local_index(&dir, &next) {
                log::error!("Failed to refresh the local resources index: {e}");
            }
            removed
        })
        .await
        .unwrap_or(0);
        if removed > 0 {
            log::info!("Removed {removed} file(s) this build no longer ships.");
        }
    }

    async fn record_scan(&self, game_path: &Path, resources: &[Resource]) {
        let dir = game_path.to_path_buf();
        let files = resources.to_vec();
        let recorded = tauri::async_runtime::spawn_blocking(move || {
            download_engine::write_scan_record(
                &dir,
                files.iter().map(|r| (r.dest(), r.size, r.md5())),
            )
        })
        .await
        .map_err(|e| format!("scan record task panicked: {e}"))
        .and_then(|r| r);
        if let Err(e) = recorded {
            log::warn!(
                "Could not refresh the scan record for {}: {e}",
                self.profile_id()
            );
        }
    }

    fn local_resources(&self, game_path: &Path) -> Result<Vec<Resource>, String> {
        self.tracker.set_phase("fetching");
        self.send_progress(Phase::Downloading, json!({
            "status": status::FETCHING_INDEX,
            "message": "Reading local file index...",
        }));

        for name in [download_engine::LOCAL_INDEX_FILE, "OriginResource.json"] {
            let index_path = game_path.join(name);
            if !index_path.exists() {
                continue;
            }
            log::info!("Checking local index at: {}", index_path.display());
            let resources = download_engine::read_index_file(&index_path);
            if !resources.is_empty() {
                log::info!("Found {} files in {name}", resources.len());
                if resources.len() >= 100 {
                    return Ok(resources);
                }
                log::warn!(
                    "Only {} files found in {name}, checking next option...",
                    resources.len()
                );
            }
        }
        Err(
            "Quick Check failed: No valid local index found with sufficient files. Please run a Full Check."
                .to_string(),
        )
    }

    async fn fetch_game_config(
        &self,
        game_path: &Path,
        after_scan: bool,
    ) -> Result<download_engine::GameConfig, String> {
        let phase = if after_scan {
            SCAN_PHASE
        } else {
            self.tracker.set_phase("fetching");
            Phase::Downloading
        };
        self.send_progress(phase, json!({ "status": status::FETCHING_CONFIG }));

        let profile = self.profile();
        if InstallMode::of(profile) == InstallMode::Gf2 {
            return self
                .cancellable(download_engine::resolve_gf2_config(profile))
                .await;
        }
        if InstallMode::of(profile) == InstallMode::Yostar {
            return self.cancellable(yostar::resolve_config(profile)).await;
        }

        self.send_progress(phase, json!({ "status": status::FETCHING_INDEX }));
        let selected = download_engine::selected_quality(&self.app, profile);
        let quality = download_engine::installed_quality(profile, game_path, selected.as_deref())
            .or(selected);
        self.cancellable(download_engine::resolve_kuro_config(profile, quality.as_deref()))
            .await
    }

    // ------------ Validating And Re-Downloading ------------
    // Hashes every file in the index, collects the bad ones, then fetches them again with
    // retries, resume support and a disk space check.
    async fn validate_files(
        self: &Arc<Self>,
        resources: &[Resource],
        game_path: &Path,
        mode: &str,
    ) -> Result<Vec<Resource>, String> {
        let total_files = resources.len();
        let total_bytes: u64 = resources.iter().map(|r| r.size).sum();
        self.tracker
            .begin("validating", total_bytes as f64, total_files, None);

        let started = std::time::Instant::now();
        log::info!(
            "Repair {} ({mode}, {}): validating {total_files} files ({:.2}GB)",
            self.profile_id(),
            game_profiles::install_mode(self.profile()).unwrap_or("default"),
            progress::gib(total_bytes as f64)
        );
        self.send_progress(SCAN_PHASE, json!({
            "status": status::VALIDATING,
            "message": format!("Validating {total_files} files..."),
        }));

        let quick = mode == "quick";
        let resources_arc: Arc<Vec<Resource>> = Arc::new(resources.to_vec());
        let cursor = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let corrupt_shared: Arc<Mutex<Vec<(Resource, FileCheck)>>> =
            Arc::new(Mutex::new(Vec::new()));

        let workers = sophon::default_scan_workers().min(resources.len().max(1));
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let me = Arc::clone(self);
            let resources = Arc::clone(&resources_arc);
            let cursor = Arc::clone(&cursor);
            let corrupt_shared = Arc::clone(&corrupt_shared);
            let game_path = game_path.to_path_buf();

            handles.push(tauri::async_runtime::spawn_blocking(
                move || -> Result<(), String> {
                    loop {
                        let index = cursor.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let Some(resource) = resources.get(index) else {
                            return Ok(());
                        };
                        me.block_while_paused();
                        if me.is_cancelled() {
                            return Err("cancelled".to_string());
                        }
                        let dest = resource.dest();
                        let file_path = match crate::backend::fs_util::safe_join(&game_path, dest) {
                            Ok(p) => p,
                            Err(e) => {
                                log::warn!("repair: skipping resource ({e})");
                                continue;
                            }
                        };
                        let _ = std::fs::remove_file(repair_tmp_path(&file_path));
                        let expected_size = resource.size;

                        let mut hashed: u64 = 0;
                        let check = if quick {
                            quick_check(&file_path, expected_size)
                        } else {
                            let md5 = resource.md5();
                            FileValidator::check(
                                &file_path,
                                expected_size,
                                md5,
                                Some(me.gate.flag()),
                                |chunk| {
                                    me.block_while_paused();
                                    hashed += chunk;
                                    me.tracker.update_validation_progress(chunk as f64);
                                    if me.tracker.should_update_ui() {
                                        me.send_metrics_progress(
                                            SCAN_PHASE,
                                            status::VALIDATING,
                                            json!({}),
                                        );
                                    }
                                },
                            )
                            .map_err(|_| "cancelled".to_string())?
                        };

                        let unreported = if quick {
                            expected_size
                        } else {
                            expected_size.saturating_sub(hashed)
                        };
                        if unreported > 0 {
                            me.tracker.update_validation_progress(unreported as f64);
                        }

                        if check != FileCheck::Valid {
                            log::debug!("[repair] {dest} {}", check.describe());
                            corrupt_shared.lock().push((resource.clone(), check));
                        }
                        if me.tracker.should_update_ui() {
                            me.send_metrics_progress(SCAN_PHASE, status::VALIDATING, json!({}));
                        }
                    }
                },
            ));
        }

        progress::join_workers(handles, "validation task").await?;
        let flagged = std::mem::take(&mut *corrupt_shared.lock());

        self.tracker.force_next_update();
        self.send_metrics_progress(SCAN_PHASE, status::VALIDATING, json!({}));
        let count = |kind: FileCheck| flagged.iter().filter(|(_, check)| *check == kind).count();
        log::info!(
            "Validation complete for {} in {:.1}s: {} of {total_files} files need repair ({} missing, {} wrong size, {} bad checksum, {} unreadable)",
            self.profile_id(),
            started.elapsed().as_secs_f64(),
            flagged.len(),
            count(FileCheck::Missing),
            count(FileCheck::SizeMismatch),
            count(FileCheck::HashMismatch),
            count(FileCheck::Unreadable),
        );
        for (resource, check) in flagged.iter().take(LOGGED_INVALID_FILES) {
            log::info!(
                "[repair] Invalid file queued for repair: {} {}",
                resource.dest(),
                check.describe()
            );
        }
        if flagged.len() > LOGGED_INVALID_FILES {
            log::info!(
                "[repair] {} more invalid files are not listed.",
                flagged.len() - LOGGED_INVALID_FILES
            );
        }
        Ok(flagged.into_iter().map(|(resource, _)| resource).collect())
    }

    async fn repair_corrupt_files(
        self: &Arc<Self>,
        corrupt: Vec<Resource>,
        base_url: &str,
        game_path: &Path,
    ) -> Result<(), String> {
        let total_files = corrupt.len();
        let total_bytes: u64 = corrupt.iter().map(|r| r.size).sum();
        let space_needed = {
            let dir = game_path.to_path_buf();
            let files: Vec<(Box<str>, u64)> =
                corrupt.iter().map(|r| (r.dest.clone(), r.size)).collect();
            tauri::async_runtime::spawn_blocking(move || {
                let on_disk: Vec<(u64, u64)> = files
                    .iter()
                    .map(|(dest, size)| {
                        let existing = crate::backend::fs_util::safe_join(&dir, dest)
                            .ok()
                            .and_then(|p| std::fs::metadata(p).ok())
                            .map_or(0, |m| m.len());
                        (*size, existing)
                    })
                    .collect();
                repair_space_needed(&on_disk, super::perf::DOWNLOAD_CONCURRENCY)
            })
            .await
            .unwrap_or(total_bytes)
        };
        download_engine::ensure_disk_space(
            game_path,
            space_needed,
            1.0,
            download_engine::HEADROOM_REPAIR,
        )?;
        if let Err(e) = super::fs_util::probe_writable(game_path) {
            if super::fs_util::is_access_denied(&e) {
                log::warn!("{} is not writable: {e}", game_path.display());
                return Err(super::fs_util::not_writable_message(game_path));
            }
            log::warn!("Could not probe {} for write access: {e}", game_path.display());
        }

        self.tracker
            .set_totals(total_bytes as f64, self.tracker.total_files());
        self.tracker.set_phase("repairing");

        log::info!(
            "Starting repair of {total_files} files ({:.2}GB)",
            progress::gib(total_bytes as f64)
        );
        self.send_metrics_progress(
            Phase::Repairing,
            "Repairing missing/corrupted files",
            json!({}),
        );

        let started = std::time::Instant::now();
        let queue: Arc<Mutex<VecDeque<Resource>>> = Arc::new(Mutex::new(corrupt.into()));
        let mut workers = Vec::new();
        for _ in 0..super::perf::DOWNLOAD_CONCURRENCY.min(total_files.max(1)) {
            let me = Arc::clone(self);
            let queue = Arc::clone(&queue);
            let base_url = base_url.to_string();
            let game_path = game_path.to_path_buf();
            workers.push(tauri::async_runtime::spawn(async move {
                loop {
                    if me.is_cancelled() {
                        return Err("Repair aborted".to_string());
                    }
                    if me.gate.is_paused() {
                        me.wait_while_paused().await;
                        continue;
                    }
                    let resource = queue.lock().pop_front();
                    let Some(resource) = resource else {
                        return Ok(());
                    };
                    me.repair_file(&resource, &base_url, &game_path).await?;
                    me.tracker.increment_repaired_files();
                }
            }));
        }

        progress::join_workers(workers, "repair worker").await?;

        log::info!(
            "Repair complete for {}: {} of {total_files} files repaired, {:.2}GB fetched in {:.1}s",
            self.profile_id(),
            self.tracker.repaired_files(),
            progress::gib(total_bytes as f64),
            started.elapsed().as_secs_f64()
        );
        Ok(())
    }

    async fn repair_file(
        self: &Arc<Self>,
        resource: &Resource,
        base_url: &str,
        game_path: &Path,
    ) -> Result<(), String> {
        let file_path = crate::backend::fs_util::safe_join(game_path, resource.dest())?;
        let result = self
            .repair_file_attempts(resource, base_url, game_path, &file_path)
            .await;
        if result.is_err() {
            let _ = std::fs::remove_file(repair_tmp_path(&file_path));
        }
        result
    }

    async fn repair_file_attempts(
        self: &Arc<Self>,
        resource: &Resource,
        base_url: &str,
        game_path: &Path,
        file_path: &Path,
    ) -> Result<(), String> {
        let dest = resource.dest().to_string();
        let tmp_path = repair_tmp_path(file_path);
        let file_name = Path::new(&dest)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| dest.clone());
        let file_size = resource.size;
        let expected_md5 = resource.md5().to_string();

        log::debug!("[repair] Re-downloading {file_name}");

        let mut attempt: u32 = 1;
        let mut size_retry_used = false;
        loop {
            let result: Result<(), String> = async {
                if self.is_cancelled() {
                    return Err("Download aborted".to_string());
                }
                let url = resource
                    .url()
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        let mode = InstallMode::of(self.profile());
                        combine_url(base_url, &download_engine::remote_path(mode, &dest))
                    });
                if let Some(parent) = file_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                self.download_file(&url, &tmp_path, &expected_md5).await
            }
            .await;

            match result {
                Ok(()) => {
                    if FileValidator::quick_validate(&tmp_path, file_size) {
                        replace_file(&tmp_path, file_path)?;
                        log::debug!("[repair] Repaired {file_name}");
                        return Ok(());
                    }
                    let actual = std::fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);
                    self.tracker
                        .set_file_progress_absolute(&tmp_path.to_string_lossy(), 0.0);
                    let _ = std::fs::remove_file(&tmp_path);
                    if !size_retry_used {
                        size_retry_used = true;
                        log::warn!(
                            "{file_name} downloaded but its size ({actual}) doesn't match the index ({file_size}). Downloading it once more."
                        );
                        continue;
                    }
                    return Err(format!(
                        "{file_name} doesn't match the size in the file index even after re-downloading. The index is likely outdated (was the game updated?). Run a Full Repair, or update the game first."
                    ));
                }
                Err(e) => {
                    if self.is_cancelled() {
                        return Err(e);
                    }
                    if let Some(code) = http::permanent_client_status(&e) {
                        return Err(format!(
                            "The download server has no usable copy of {file_name} (HTTP {code}). The file index is likely outdated (was the game updated?). Update the game first, or run a Full Repair."
                        ));
                    }
                    if super::fs_util::classify(&e) == super::fs_util::FailureKind::Validation {
                        if !size_retry_used {
                            size_retry_used = true;
                            log::warn!(
                                "{file_name} failed its checksum after downloading, so it is downloaded once more: {e}"
                            );
                            continue;
                        }
                        return Err(format!(
                            "{file_name} doesn't match the file index even after re-downloading. The index is likely outdated (was the game updated?). Run a Full Repair, or update the game first."
                        ));
                    }
                    if attempt >= MAX_REPAIR_RETRIES {
                        return Err(e);
                    }
                    match super::fs_util::classify(&e) {
                        super::fs_util::FailureKind::DiskFull => {
                            return Err(format!(
                                "Not enough disk space while repairing {file_name}. Free up space and try again. ({e})"
                            ));
                        }
                        super::fs_util::FailureKind::AccessDenied => {
                            return Err(format!(
                                "{} ({e})",
                                super::fs_util::not_writable_message(game_path)
                            ));
                        }
                        kind => {
                            let give_up = kind != super::fs_util::FailureKind::ShareLost
                                || attempt >= super::fs_util::SHARE_LOST_ATTEMPTS;
                            if let Some(message) = super::fs_util::drive_failure_message(
                                kind, game_path, file_path, file_size,
                            )
                            .filter(|_| give_up)
                            {
                                return Err(format!("{message} ({e})"));
                            }
                        }
                    }
                    let delay = http::retry_delay(attempt, RETRY_DELAY_BASE_MS);
                    log::warn!(
                        "Repair of {file_name} failed (attempt {attempt}/{MAX_REPAIR_RETRIES}): {e}. Retrying in {}ms",
                        delay.as_millis()
                    );
                    self.retry_backoff(delay).await?;
                    attempt += 1;
                }
            }
        }
    }

    async fn retry_backoff(&self, delay: std::time::Duration) -> Result<(), String> {
        let _ = self
            .cancellable(async {
                tokio::time::sleep(delay).await;
                Ok::<(), String>(())
            })
            .await;
        self.wait_while_paused().await;
        if self.is_cancelled() {
            return Err("Repair aborted".to_string());
        }
        Ok(())
    }

    async fn download_package(&self, url: &str, archive: &Path) -> Result<(), String> {
        let mut attempt: u32 = 1;
        loop {
            let Err(e) = self.download_file(url, archive, "").await else {
                return Ok(());
            };
            if self.is_cancelled() || attempt >= MAX_REPAIR_RETRIES || !package_retryable(&e) {
                if self.is_cancelled() || !package_retryable(&e) {
                    let _ = std::fs::remove_file(archive);
                } else if archive.exists() {
                    log::info!(
                        "The partial client package is kept, so the next repair continues it."
                    );
                }
                return Err(e);
            }
            let delay = http::retry_delay(attempt, RETRY_DELAY_BASE_MS);
            log::warn!(
                "Package download failed (attempt {attempt}/{MAX_REPAIR_RETRIES}): {e}. Retrying in {}ms",
                delay.as_millis()
            );
            if let Err(e) = self.retry_backoff(delay).await {
                let _ = std::fs::remove_file(archive);
                return Err(e);
            }
            attempt += 1;
        }
    }

    async fn download_file(
        &self,
        url: &str,
        file_path: &Path,
        expected_md5: &str,
    ) -> Result<(), String> {
        let progress_id = file_path.to_string_lossy().to_string();
        let result = self
            .fetch_into(url, file_path, expected_md5, &progress_id)
            .await;
        match &result {
            Ok(()) => {
                self.tracker.force_next_update();
                self.send_repair_progress();
            }
            Err(e) if keeps_partial(e) && !self.is_cancelled() => {}
            Err(_) => {
                let _ = std::fs::remove_file(file_path);
                self.tracker.set_file_progress_absolute(&progress_id, 0.0);
            }
        }
        result
    }

    async fn hash_partial(&self, file_path: &Path, expected: &str) -> Result<ContentHasher, String> {
        let path = file_path.to_path_buf();
        let expected = expected.to_string();
        let cancelled = Arc::clone(self.gate.flag());
        tauri::async_runtime::spawn_blocking(move || {
            use std::io::Read;
            let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let mut hasher = ContentHasher::for_expected(&expected);
            let mut buffer = vec![0u8; 1 << 20];
            loop {
                if cancelled.load(Ordering::SeqCst) {
                    return Err("Repair aborted".to_string());
                }
                let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
                if n == 0 {
                    return Ok(hasher);
                }
                hasher.update(&buffer[..n]);
            }
        })
        .await
        .map_err(|e| format!("partial hash task panicked: {e}"))?
    }

    async fn fetch_into(
        &self,
        url: &str,
        file_path: &Path,
        expected_md5: &str,
        progress_id: &str,
    ) -> Result<(), String> {
        let mut resume_from = tokio::fs::metadata(file_path)
            .await
            .ok()
            .filter(|m| m.is_file())
            .map_or(0, |m| m.len());
        let mut hasher = (!expected_md5.is_empty()).then(|| ContentHasher::for_expected(expected_md5));
        if resume_from > 0 && hasher.is_some() {
            match self.hash_partial(file_path, expected_md5).await {
                Ok(partial) => hasher = Some(partial),
                Err(e) => {
                    if self.is_cancelled() {
                        return Err("Repair aborted".to_string());
                    }
                    log::warn!(
                        "[repair] Could not read the partial {}, so it starts over: {e}",
                        file_path.display()
                    );
                    let _ = std::fs::remove_file(file_path);
                    resume_from = 0;
                }
            }
        }
        self.tracker
            .set_file_progress_absolute(progress_id, resume_from as f64);

        let mut request = http::download_client().get(url);
        if resume_from > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("Request error: {e}"))?;
        let status = response.status().as_u16();
        let sink = if resume_from > 0 && status == 206 {
            let range_ok = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| resumes_at(v, resume_from));
            if !range_ok {
                let _ = std::fs::remove_file(file_path);
                self.tracker.set_file_progress_absolute(progress_id, 0.0);
                return Err(format!(
                    "Stream error: unexpected Content-Range resuming {url}, starting over"
                ));
            }
            log::debug!(
                "[repair] Resuming {} from {resume_from} bytes",
                file_path.display()
            );
            self.tracker.reset_speed_baseline();
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(file_path)
                .await
                .map_err(|e| format!("Write error: {e}"))?
        } else if status == 200 {
            if resume_from > 0 {
                log::debug!(
                    "[repair] Server ignored the Range request for {}, restarting it",
                    file_path.display()
                );
                hasher = (!expected_md5.is_empty()).then(|| ContentHasher::for_expected(expected_md5));
                self.tracker.set_file_progress_absolute(progress_id, 0.0);
            }
            tokio::fs::File::create(file_path)
                .await
                .map_err(|e| format!("Write error: {e}"))?
        } else if resume_from > 0 && status == 416 {
            let _ = std::fs::remove_file(file_path);
            self.tracker.set_file_progress_absolute(progress_id, 0.0);
            return Err(format!(
                "Stream error: the server refused to resume {url} (HTTP 416), starting over"
            ));
        } else {
            return Err(format!("HTTP {status} for {url}"));
        };
        let mut file =
            tokio::io::BufWriter::with_capacity(super::perf::WRITE_BUFFER_BYTES, sink);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if self.is_cancelled() {
                return Err("Repair aborted".to_string());
            }
            if self.gate.is_paused() {
                file.flush()
                    .await
                    .map_err(|e| format!("Write error: {e}"))?;
                self.wait_while_paused().await;
                if self.is_cancelled() {
                    return Err("Repair aborted".to_string());
                }
            }
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(e) => {
                    let _ = file.flush().await;
                    return Err(format!("Stream error: {e}"));
                }
            };
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("Write error: {e}"))?;
            if let Some(h) = hasher.as_mut() {
                h.update(&chunk);
            }
            self.tracker.add_bytes(progress_id, chunk.len() as i64);
            if self.tracker.should_update_ui() {
                self.send_repair_progress();
            }
        }
        file.flush()
            .await
            .map_err(|e| format!("Write error: {e}"))?;
        if let Some(h) = hasher {
            let got = h.finish();
            if !got.eq_ignore_ascii_case(expected_md5) {
                return Err(format!(
                    "failed verification after re-download (got {got}, expected {expected_md5})"
                ));
            }
        }
        Ok(())
    }

    fn send_repair_progress(&self) {
        self.send_metrics_progress(Phase::Repairing, status::REPAIRING, json!({}));
    }

    fn clear_unfinished_update(&self) {
        super::file_channels::clear_update_incomplete(&self.app, &self.profile_id());
    }

    fn handle_repair_complete(&self, start_ms: i64, repaired: usize, total_validated: usize) {
        let message = completion_message(start_ms, repaired, total_validated);
        log::info!("Repair completed for {}: {message}", self.profile_id());

        if repaired > 0 {
            log::info!(
                "Clearing GameManager update cache for {} after repair completion",
                self.profile_id()
            );
            let state = self.app.state::<BackendState>();
            state.game.clear_update_cache(&self.profile_id());
        }

        self.send_progress(Phase::Done, json!({
            "status": status::COMPLETED,
            "message": message,
        }));
    }

    fn handle_repair_error(&self, error: &str) {
        if self.is_cancelled() {
            self.send_progress(Phase::Cancelled, json!({ "status": status::CANCELLED }));
            log::info!("Repair process was cancelled ({}).", self.profile_id());
        } else {
            self.send_progress(Phase::Error, json!({ "status": status::ERROR, "error": error }));
            log::error!("Game repair failed for {}: {error}", self.profile_id());
        }
    }

    // ------------ Neverness To Everness Repair ------------
    // Netease's game uses its own manifest format, so it gets its own scan and fix.
    async fn run_nte_repair(
        self: &Arc<Self>,
        game_path: &Path,
        mode: &str,
        start_ms: i64,
    ) -> Result<(), String> {
        let profile = self.profile();

        self.send_progress(Phase::Downloading, json!({ "status": status::FETCHING_CONFIG }));
        let config = self.cancellable(nte::fetch_config(profile)).await?;
        let bases = nte::res_bases(profile);
        self.send_progress(Phase::Downloading, json!({ "status": status::FETCHING_INDEX }));
        let list = self
            .cancellable(nte::fetch_reslist(profile, &config))
            .await?;
        let wanted = super::game_profiles::content_tags(super::game_profiles::profile_id(profile));
        let mut resources: Vec<nte::Resource> = list.selected(wanted.as_deref()).cloned().collect();
        resources.extend(
            self.cancellable(nte::fetch_launcher(profile))
                .await?
                .resources,
        );

        self.tracker.set_totals(
            resources.iter().map(|r| r.size).sum::<u64>() as f64,
            resources.len(),
        );
        self.tracker.set_phase("validating");
        self.send_metrics_progress(SCAN_PHASE, status::VALIDATING, json!({}));

        let scan_hooks: Arc<dyn nte::Hooks> =
            Arc::new(RepairHooks::new(Arc::clone(self), RepairMode::Validate));
        let dir = game_path.to_path_buf();
        let scan_list = resources.clone();
        let quick = mode == "quick";
        let plan = tauri::async_runtime::spawn_blocking(move || {
            if quick {
                nte::plan_quick_scan(&dir, &scan_list, scan_hooks.as_ref())
            } else {
                nte::plan_scan_parallel(
                    &dir,
                    &scan_list,
                    nte::ScanMode::Blocks,
                    super::perf::validation_workers(),
                    scan_hooks.as_ref(),
                )
            }
        })
        .await
        .map_err(|e| format!("nte repair planning panicked: {e}"))??;

        let broken = plan.fetch.len();
        let validated = plan.unchanged + broken;

        if broken > 0 {
            download_engine::ensure_disk_space(
                game_path,
                plan.total_bytes,
                1.0,
                download_engine::HEADROOM_REPAIR,
            )?;
            self.tracker.reset();
            self.tracker.set_totals(plan.total_bytes as f64, broken);
            self.tracker.set_file_sizes(
                plan.fetch
                    .iter()
                    .map(|r| (r.dest.clone(), r.fetch_bytes() as f64))
                    .collect(),
            );
            self.tracker.set_phase("repairing");
            self.send_metrics_progress(Phase::Repairing, status::REPAIRING, json!({}));

            let hooks: Arc<dyn nte::Hooks> =
                Arc::new(RepairHooks::new(Arc::clone(self), RepairMode::Apply));
            nte::apply(
                game_path,
                &plan,
                &bases,
                super::perf::DOWNLOAD_CONCURRENCY,
                hooks,
                Arc::clone(self.gate.flag()),
                None,
            )
            .await?;

            self.app
                .state::<BackendState>()
                .game
                .clear_update_cache(&self.profile_id());
        }

        if !quick {
            self.record_nte_version(game_path, &config.res_version);
            self.clear_unfinished_update();
            self.record_nte_scan(game_path, resources).await;
        }

        self.handle_repair_complete(start_ms, broken, validated);
        Ok(())
    }

    async fn record_nte_scan(&self, game_path: &Path, resources: Vec<nte::Resource>) {
        let dir = game_path.to_path_buf();
        let recorded = tauri::async_runtime::spawn_blocking(move || {
            download_engine::write_scan_record(
                &dir,
                resources
                    .iter()
                    .map(|r| (r.dest.as_str(), r.size, r.md5.as_str())),
            )
        })
        .await
        .map_err(|e| format!("scan record task panicked: {e}"))
        .and_then(|r| r);
        if let Err(e) = recorded {
            log::warn!(
                "nte repair: could not refresh the scan record for {}: {e}",
                self.profile_id()
            );
        }
    }

    fn record_nte_version(&self, game_path: &Path, version: &str) {
        let recorded = super::game_manager::local_game_version(&game_path.to_string_lossy());
        if version.trim().is_empty() || recorded.as_deref() == Some(version.trim()) {
            return;
        }
        match download_engine::update_game_config_file(game_path, version) {
            Ok(()) => {
                log::info!(
                    "nte repair: every file matches version {} now, so it is recorded (was {}).",
                    version.trim(),
                    recorded.as_deref().unwrap_or("unknown")
                );
                self.app
                    .state::<BackendState>()
                    .game
                    .clear_update_cache(&self.profile_id());
            }
            Err(e) => log::warn!("nte repair: could not record version {version}: {e}"),
        }
    }

    fn take_sophon_verify_cache(
        &self,
        game_id: &str,
        tag: &str,
        audio_language: &str,
    ) -> Option<Vec<(sophon::Category, sophon::Plan)>> {
        let pools = self.app.state::<BackendState>().engine.clone();
        let mut slot = pools.sophon_verify_cache.lock();
        match slot.take() {
            Some(cache)
                if cache.game_id == game_id
                    && cache.tag == tag
                    && cache.audio_language == audio_language
                    && cache.at.elapsed() < std::time::Duration::from_secs(300) =>
            {
                Some(cache.plans)
            }
            Some(cache) if cache.game_id != game_id => {
                *slot = Some(cache);
                None
            }
            _ => None,
        }
    }

    // ------------ Hoyoverse Repair ------------
    // Genshin, Star Rail, Zenless and Honkai 3rd go through Sophon chunks (see sophon.rs).
    async fn run_sophon_repair(
        self: &Arc<Self>,
        game_path: &Path,
        mode: &str,
        start_ms: i64,
    ) -> Result<(), String> {
        let profile = self.profile();
        let quick = mode == "quick";

        self.send_progress(Phase::Downloading, json!({ "status": status::FETCHING_CONFIG }));
        let auth = self
            .cancellable(sophon::fetch_branch_auth_for_profile(profile))
            .await?;
        let build = self.cancellable(sophon::fetch_build(&auth)).await?;

        let audio_language = self
            .cancellable(async {
                Ok(super::file_channels::installed_audio_language(
                    &self.app,
                    profile,
                    game_path,
                    Some(&build),
                )
                .await)
            })
            .await?;
        let categories = sophon::install_categories(&build, &audio_language);
        let total_files: u64 = categories.iter().map(|c| c.file_count).sum();
        self.send_progress(Phase::Downloading, json!({
            "status": status::FETCHING_INDEX,
            "message": format!("Reading file lists ({total_files} files)..."),
        }));
        let manifests = self
            .cancellable(sophon::fetch_manifests(&categories))
            .await?;
        let applied = sophon::load_applied(game_path);

        let cached_plans =
            self.take_sophon_verify_cache(&self.profile_id(), &build.tag, &audio_language);

        let mut plans = Vec::new();
        let mut validated = 0usize;
        let mut broken = 0usize;
        let mut total_bytes = 0u64;

        if let Some(cached) = cached_plans {
            log::info!(
                "sophon repair: reusing the integrity scan from the verify run moments ago."
            );
            for (_, plan) in &cached {
                validated += plan.unchanged_files + plan.assets.len();
                broken += plan.assets.len();
                total_bytes += plan.total_bytes;
            }
            plans = cached;
        } else {
            let (scan_bytes, scan_files) = sophon::scan_totals(&manifests);
            self.tracker.set_totals(scan_bytes as f64, scan_files);
            self.tracker.set_phase("validating");
            self.send_metrics_progress(SCAN_PHASE, status::VALIDATING, json!({}));
            let scan_hooks: Arc<dyn sophon::Hooks> =
                Arc::new(RepairHooks::new(Arc::clone(self), RepairMode::Validate));

            let prev_files = if quick {
                applied
                    .as_ref()
                    .filter(|a| a.tag == build.tag)
                    .map(|a| Arc::new(a.file_map()))
            } else {
                None
            };
            if quick && prev_files.is_none() {
                log::info!(
                    "sophon quick repair: no recorded install for {}, so running a full chunk scan instead.",
                    build.tag
                );
            }

            for (category, assets) in categories.iter().zip(&manifests) {
                let dir = game_path.to_path_buf();
                let hooks = Arc::clone(&scan_hooks);
                let assets = assets.clone();
                let prev = prev_files.clone();
                let plan = tauri::async_runtime::spawn_blocking(move || {
                    let scan_mode = match prev.as_deref() {
                        Some(map) => sophon::ScanMode::Diff { prev: map },
                        None => sophon::ScanMode::Deep,
                    };
                    sophon::plan_scan_parallel(
                        &dir,
                        &assets,
                        scan_mode,
                        sophon::default_scan_workers(),
                        hooks.as_ref(),
                    )
                })
                .await
                .map_err(|e| format!("sophon repair planning panicked: {e}"))??;
                validated += plan.unchanged_files + plan.assets.len();
                broken += plan.assets.len();
                total_bytes += plan.total_bytes;
                plans.push(((*category).clone(), plan));
            }
        }

        if broken > 0 {
            let growth_bytes: u64 = plans.iter().map(|(_, p)| p.growth_bytes()).sum();
            download_engine::ensure_disk_space(
                game_path,
                growth_bytes,
                1.0,
                download_engine::HEADROOM_REPAIR,
            )?;
            let hooks: Arc<dyn sophon::Hooks> =
                Arc::new(RepairHooks::new(Arc::clone(self), RepairMode::Apply));
            self.tracker.reset();
            self.tracker.set_totals(total_bytes as f64, broken);
            self.tracker.set_file_sizes(
                plans
                    .iter()
                    .flat_map(|(_, p)| p.assets.iter())
                    .map(|a| (a.asset.name.clone(), a.bytes_to_fetch() as f64))
                    .collect(),
            );
            self.tracker.set_phase("repairing");
            self.send_metrics_progress(Phase::Repairing, status::REPAIRING, json!({}));
            sophon::apply_plans(
                game_path,
                &plans,
                super::perf::DOWNLOAD_CONCURRENCY,
                hooks,
                Arc::clone(self.gate.flag()),
                None,
            )
            .await?;
        }

        {
            let dir = game_path.to_path_buf();
            let tag = build.tag.clone();
            let languages = vec![audio_language.clone()];
            let categories_owned: Vec<sophon::Category> =
                categories.iter().map(|c| (*c).clone()).collect();
            let manifests_cloned = manifests.clone();
            let prev = applied.clone();
            let removed = tauri::async_runtime::spawn_blocking(move || {
                let category_refs: Vec<&sophon::Category> = categories_owned.iter().collect();
                sophon::finalize_applied_state(
                    &dir,
                    &tag,
                    &languages,
                    &category_refs,
                    &manifests_cloned,
                    prev.as_ref(),
                )
            })
            .await
            .unwrap_or(0);
            if removed > 0 {
                log::info!(
                    "sophon: removed {removed} orphaned file(s) left over from the previous version."
                );
            }
        }
        if let Err(e) = download_engine::update_game_config_file(game_path, &build.tag) {
            log::warn!(
                "sophon repair: could not record version {} for the install: {e}",
                build.tag
            );
        }
        self.app
            .state::<BackendState>()
            .game
            .clear_update_cache(&self.profile_id());
        self.clear_unfinished_update();

        let message = completion_message(start_ms, broken, validated);
        log::info!("Sophon repair completed: {message}");
        self.send_progress(Phase::Done, json!({
            "status": status::COMPLETED,
            "message": message,
        }));
        Ok(())
    }

    // ------------ Arknights Endfield Repair ------------
    // Hypergryph installs are reconciled against the manifest the launcher saved earlier.
    async fn run_hypergryph_reconcile(
        self: &Arc<Self>,
        game_path: &Path,
        start_ms: i64,
    ) -> Result<(), String> {
        let profile = self.profile();
        let name = game_profiles::display_name(profile);
        let recorded = reconcile::load_manifest(game_path);
        let latest = match super::hypergryph::get_latest_game(profile).await {
            Ok(latest) => reconcile::latest_packs(&latest),
            Err(e) => {
                log::warn!("Could not fetch the latest {name} packages: {e}");
                None
            }
        };

        self.send_progress(SCAN_PHASE, json!({ "status": status::VALIDATING }));
        let hooks = ReconcileRepairHooks {
            mgr: Arc::clone(self),
            phase: Mutex::new("validating"),
        };
        let dir = game_path.to_path_buf();
        let cancelled = Arc::clone(self.gate.flag());
        let handle = tokio::runtime::Handle::current();

        let Some(mut manifest) = recorded else {
            let Some((version, packs)) = latest else {
                return Err(format!(
                    "No install manifest found for {name}, and the latest packages could not be fetched to check this install. Check your connection and try again."
                ));
            };
            log::info!("{name}: no install manifest, checking this install against version {version}.");
            let (manifest, stats) = tauri::async_runtime::spawn_blocking(move || {
                reconcile::adopt(&dir, packs, &version, cancelled, &hooks, handle)
            })
            .await
            .map_err(|e| format!("repair task panicked: {e}"))??;
            reconcile::save_manifest(game_path, &manifest)?;
            download_engine::update_game_config_file(game_path, &manifest.version)?;
            log::info!(
                "{name}: install adopted at version {} ({} files tracked).",
                manifest.version,
                manifest.files.len()
            );
            let state = self.app.state::<BackendState>();
            state.game.clear_update_cache(&self.profile_id());
            self.finish_hypergryph_repair(start_ms, stats);
            return Ok(());
        };

        let mut newer = None;
        if let Some((version, packs)) = latest {
            if download_engine::versions_semver_equal(&version, &manifest.version) {
                let refreshed = manifest.packs.len() != packs.len()
                    || manifest
                        .packs
                        .iter()
                        .zip(&packs)
                        .any(|(old, new)| old.url != new.url || old.size != new.size);
                if refreshed {
                    manifest.packs = packs;
                    match reconcile::save_manifest(game_path, &manifest) {
                        Ok(()) => log::info!("{name}: refreshed the recorded package links."),
                        Err(e) => log::warn!("{name}: could not save refreshed package links: {e}"),
                    }
                }
            } else {
                newer = Some(version);
            }
        }

        let result = tauri::async_runtime::spawn_blocking(move || {
            reconcile::repair(&dir, &manifest, cancelled, &hooks, handle)
        })
        .await
        .map_err(|e| format!("repair task panicked: {e}"))?;
        let stats = result.map_err(|e| match &newer {
            Some(version) if reconcile::is_stale_packages(&e) => {
                format!("{e} Version {version} is available. Update the game instead.")
            }
            _ => e,
        })?;

        if stats.repaired > 0 {
            let state = self.app.state::<BackendState>();
            state.game.clear_update_cache(&self.profile_id());
        }
        self.finish_hypergryph_repair(start_ms, stats);
        Ok(())
    }

    fn finish_hypergryph_repair(&self, start_ms: i64, stats: reconcile::RepairStats) {
        self.clear_unfinished_update();
        let message = completion_message(start_ms, stats.repaired, stats.validated);
        log::info!("Repair completed for {}: {message}", self.profile_id());
        self.send_progress(Phase::Done, json!({
            "status": status::COMPLETED,
            "message": message,
        }));
    }
}

// ------------ Repair Helpers ------------
// Small shared pieces: file checks, retry rules, temp file handling and the progress
// hooks that report repair events back to the UI.
fn clear_bluepoch_staging(install_path: &Path, executable_name: &str) -> u64 {
    let mut cleared = 0u64;
    for dir in super::bluepoch::staging_dirs(install_path, executable_name) {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => cleared += 1,
            Err(e) => log::warn!("Could not clear {}: {e}", dir.display()),
        }
    }
    cleared
}

fn completion_message(start_ms: i64, repaired: usize, validated: usize) -> String {
    let duration = (chrono::Utc::now().timestamp_millis() - start_ms) as f64 / 1000.0;
    if repaired > 0 {
        format!(
            "Successfully repaired {repaired} files out of {validated} validated in {duration:.1}s"
        )
    } else {
        format!("All {validated} files are valid!")
    }
}

fn quick_check(path: &Path, expected_size: u64) -> FileCheck {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() == expected_size => FileCheck::Valid,
        Ok(_) => FileCheck::SizeMismatch,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileCheck::Missing,
        Err(_) => FileCheck::Unreadable,
    }
}

fn repair_space_needed(files: &[(u64, u64)], in_flight: usize) -> u64 {
    let growth: u64 = files
        .iter()
        .map(|(size, existing)| size.saturating_sub(*existing))
        .sum();
    let mut old_copies: Vec<u64> = files
        .iter()
        .map(|(size, existing)| (*existing).min(*size))
        .collect();
    old_copies.sort_unstable_by(|a, b| b.cmp(a));
    growth + old_copies.iter().take(in_flight).sum::<u64>()
}

fn keeps_partial(error: &str) -> bool {
    error.starts_with("Request error")
        || error.starts_with("Stream error")
        || error.starts_with("HTTP ")
}

fn resumes_at(content_range: &str, from: u64) -> bool {
    content_range
        .trim()
        .strip_prefix("bytes ")
        .and_then(|range| range.split('-').next())
        .and_then(|start| start.trim().parse::<u64>().ok())
        == Some(from)
}

fn package_retryable(error: &str) -> bool {
    use super::fs_util::FailureKind;
    http::permanent_client_status(error).is_none()
        && !matches!(
            super::fs_util::classify(error),
            FailureKind::DiskFull
                | FailureKind::AccessDenied
                | FailureKind::DeviceGone
                | FailureKind::FileTooLarge
                | FailureKind::Cancelled
        )
}

fn repair_tmp_path(file_path: &Path) -> std::path::PathBuf {
    let mut os = file_path.as_os_str().to_owned();
    os.push(".peebify_tmp");
    std::path::PathBuf::from(os)
}

fn replace_file(tmp_path: &Path, file_path: &Path) -> Result<(), String> {
    super::fs_util::finalize_replace(tmp_path, file_path).inspect_err(|_| {
        let _ = std::fs::remove_file(tmp_path);
    })
}

enum RepairMode {
    Validate,
    Apply,
}

struct RepairHooks {
    mgr: Arc<GameRepairManager>,
    mode: RepairMode,
}

impl RepairHooks {
    fn new(mgr: Arc<GameRepairManager>, mode: RepairMode) -> Self {
        Self { mgr, mode }
    }
}

impl progress::Control for RepairHooks {
    fn is_cancelled(&self) -> bool {
        self.mgr.is_cancelled()
    }

    fn gate(&self) -> Option<&progress::RunGate> {
        Some(&self.mgr.gate)
    }
}

impl progress::FileHooks for RepairHooks {
    fn event(&self, event: progress::FileEvent) {
        match (&self.mode, event) {
            (RepairMode::Validate, progress::FileEvent::Bytes { delta, .. }) => {
                self.mgr.tracker.update_validation_progress(delta as f64);
                if self.mgr.tracker.should_update_ui() {
                    self.mgr
                        .send_metrics_progress(SCAN_PHASE, status::VALIDATING, json!({}));
                }
            }
            (RepairMode::Validate, progress::FileEvent::FileDone { .. }) => {}
            (RepairMode::Apply, progress::FileEvent::Bytes { path, delta }) => {
                self.mgr
                    .tracker
                    .update_file_progress(path, delta as f64, false);
                if self.mgr.tracker.should_update_ui() {
                    self.mgr.send_metrics_progress(Phase::Repairing, status::REPAIRING, json!({}));
                }
            }
            (RepairMode::Apply, progress::FileEvent::FileDone { path }) => {
                self.mgr.tracker.update_file_progress(path, 0.0, true);
                self.mgr.tracker.increment_repaired_files();
            }
        }
    }

    fn status(&self, message: &str) {
        self.mgr.set_offline_power(!message.is_empty());
        if message.is_empty() {
            self.mgr.tracker.reset_speed_baseline();
            self.mgr.send_metrics_progress(Phase::Repairing, status::REPAIRING, json!({}));
        } else {
            self.mgr.send_progress(
                Update::new(Phase::Repairing).waiting_network(true),
                json!({
                    "status": status::REPAIRING,
                    "speed": 0,
                    "subStatus": message,
                }),
            );
        }
    }
}

struct ReconcileRepairHooks {
    mgr: Arc<GameRepairManager>,
    phase: Mutex<&'static str>,
}

impl ReconcileRepairHooks {
    fn status_text(&self) -> &'static str {
        if *self.phase.lock() == "repairing" {
            status::REPAIRING
        } else {
            status::VALIDATING
        }
    }

    fn stage(&self) -> Phase {
        if *self.phase.lock() == "repairing" {
            Phase::Repairing
        } else {
            SCAN_PHASE
        }
    }
}

impl progress::Control for ReconcileRepairHooks {
    fn is_cancelled(&self) -> bool {
        self.mgr.is_cancelled()
    }

    fn gate(&self) -> Option<&progress::RunGate> {
        Some(&self.mgr.gate)
    }
}

impl reconcile::Hooks for ReconcileRepairHooks {
    fn event(&self, event: reconcile::Event) {
        match event {
            reconcile::Event::Phase { message, name } => {
                let phase = if name == "repairing" {
                    "repairing"
                } else {
                    "validating"
                };
                *self.phase.lock() = phase;
                self.mgr.tracker.set_phase(phase);
                self.mgr.send_progress(
                    self.stage(),
                    json!({
                        "status": self.status_text(),
                        "message": message,
                        "percentage": 0,
                        "processedBytes": 0,
                    }),
                );
            }
            reconcile::Event::Totals { total_bytes, sizes } => {
                let phase = *self.phase.lock();
                self.mgr.tracker.reset();
                self.mgr.tracker.set_totals(total_bytes as f64, sizes.len());
                self.mgr.tracker.set_file_sizes(
                    sizes
                        .into_iter()
                        .map(|(path, size)| (path, size as f64))
                        .collect(),
                );
                self.mgr.tracker.set_phase(phase);
            }
            reconcile::Event::Bytes { path, delta, done } => {
                self.mgr
                    .tracker
                    .update_file_progress(path, delta as f64, done);
                if done && *self.phase.lock() == "repairing" {
                    self.mgr.tracker.increment_repaired_files();
                }
                if self.mgr.tracker.should_update_ui() {
                    self.mgr
                        .send_metrics_progress(self.stage(), self.status_text(), json!({}));
                }
            }
        }
    }

    fn status(&self, message: &str) {
        self.mgr.set_offline_power(!message.is_empty());
        if message.is_empty() {
            self.mgr.tracker.reset_speed_baseline();
            self.mgr
                .send_metrics_progress(self.stage(), self.status_text(), json!({}));
        } else {
            self.mgr.send_progress(
                Update::new(self.stage()).waiting_network(true),
                json!({
                    "status": self.status_text(),
                    "speed": 0,
                    "subStatus": message,
                }),
            );
        }
    }
}

// ------------ Tests ------------
// Retry rules, resume handling, space estimates and quick check messages.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_http_errors_stop_on_client_errors() {
        let status = http::permanent_client_status;
        assert_eq!(status("HTTP 404 for https://cdn/x.pak"), Some(404));
        assert_eq!(status("HTTP 403 for https://cdn/x.pak"), Some(403));
        assert_eq!(status("HTTP 410 for https://cdn/x.pak"), Some(410));
        assert!(!package_retryable("HTTP 404 for https://cdn/x.pak"));
    }

    #[test]
    fn repair_http_errors_keep_retrying_transient_codes() {
        let status = http::permanent_client_status;
        assert_eq!(status("HTTP 408 for https://cdn/x.pak"), None);
        assert_eq!(status("HTTP 429 for https://cdn/x.pak"), None);
        assert_eq!(status("HTTP 503 for https://cdn/x.pak"), None);
        assert_eq!(status("HTTP 206 for https://cdn/x.pak"), None);
        assert!(package_retryable("HTTP 503 for https://cdn/x.pak"));
    }

    #[test]
    fn repair_http_status_ignores_other_errors() {
        let status = http::permanent_client_status;
        assert_eq!(status("Request error: timed out"), None);
        assert_eq!(
            status("failed verification after re-download (got a, expected b)"),
            None
        );
        assert_eq!(status("Write error: HTTP 404"), None);
    }

    #[test]
    fn repair_tmp_path_appends_the_suffix() {
        let path = Path::new("Client").join("pakchunk1.pak");
        assert_eq!(
            repair_tmp_path(&path),
            Path::new("Client").join("pakchunk1.pak.peebify_tmp")
        );
    }

    #[test]
    fn checksum_failures_classify_as_validation() {
        assert_eq!(
            super::super::fs_util::classify(
                "failed verification after re-download (got a, expected b)"
            ),
            super::super::fs_util::FailureKind::Validation
        );
    }

    #[test]
    fn package_downloads_retry_transient_failures() {
        assert!(package_retryable("Request error: error sending request"));
        assert!(package_retryable("Stream error: timed out"));
        assert!(package_retryable("HTTP 503 for https://cdn/client.zip"));
        assert!(package_retryable("HTTP 429 for https://cdn/client.zip"));
    }

    #[test]
    fn package_downloads_stop_on_permanent_failures() {
        assert!(!package_retryable("HTTP 404 for https://cdn/client.zip"));
        assert!(!package_retryable("Repair aborted"));
        assert!(!package_retryable(
            "Write error: There is not enough space on the disk. (os error 112)"
        ));
        assert!(!package_retryable("Write error: Access is denied. (os error 5)"));
    }

    #[test]
    fn only_network_failures_keep_a_partial_download() {
        assert!(keeps_partial("Request error: error sending request"));
        assert!(keeps_partial("Stream error: connection reset"));
        assert!(keeps_partial("HTTP 503 for https://cdn/x.pak"));
        assert!(!keeps_partial("Write error: Access is denied. (os error 5)"));
        assert!(!keeps_partial(
            "failed verification after re-download (got a, expected b)"
        ));
        assert!(!keeps_partial("Repair aborted"));
    }

    #[test]
    fn a_resume_needs_the_range_to_start_where_the_file_ends() {
        assert!(resumes_at("bytes 1024-4095/4096", 1024));
        assert!(resumes_at(" bytes 1024-4095/* ", 1024));
        assert!(!resumes_at("bytes 0-4095/4096", 1024));
        assert!(!resumes_at("bytes 10240-4095/4096", 1024));
        assert!(!resumes_at("garbage", 1024));
    }

    #[test]
    fn repair_space_counts_growth_plus_old_copies_in_flight() {
        let hashed = [(10u64 << 30, 10u64 << 30), (10 << 30, 10 << 30)];
        assert_eq!(repair_space_needed(&hashed, 1), 10 << 30);
        assert_eq!(repair_space_needed(&hashed, 24), 20 << 30);
        assert_eq!(repair_space_needed(&[(500, 0)], 24), 500);
        assert_eq!(repair_space_needed(&[(500, 200)], 24), 300 + 200);
        assert_eq!(repair_space_needed(&[(100, 400)], 24), 100);
        assert_eq!(repair_space_needed(&[], 24), 0);
    }

    #[test]
    fn quick_check_names_why_a_file_fails() {
        let dir =
            std::env::temp_dir().join(format!("peebify-quick-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data.pak");
        std::fs::write(&file, b"abcd").unwrap();
        assert_eq!(quick_check(&file, 4), FileCheck::Valid);
        assert_eq!(quick_check(&file, 5), FileCheck::SizeMismatch);
        assert_eq!(quick_check(&dir.join("gone.pak"), 4), FileCheck::Missing);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
