// ------------ Download Engine ------------
// Installs and updates games. One download manager per game picks the right path for it (Kuro, HoYoverse, Arknights: Endfield, Girls' Frontline 2, Neverness to Everness, Brown Dust II, Reverse: 1999) and runs it.
// The shared part resumes interrupted downloads, splits big files into pieces, retries and switches mirrors, and reports progress to the queue.
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use md5::{Digest, Md5};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::io::AsyncWriteExt;

use super::game_profiles::InstallMode;
use super::queue::{Phase, Update};
use super::state::BackendState;
use super::validator::{self, Resource, ValidationMeta};
use super::{
    bd2, bluepoch, err_response, game_path, game_profiles, gf2, http, hypergryph,
    hypergryph_reconcile as reconcile, nte, ok_with, progress, sophon,
};

pub mod status {
    pub const FETCHING_CONFIG: &str = "Fetching remote configuration...";
    pub const DOWNLOADING: &str = "Downloading...";
    pub const PAUSED: &str = "Paused";
    pub const CANCELLED: &str = "Cancelled";
    pub const COMPLETED: &str = "Completed";
    pub const ERROR: &str = "Error";
    pub const WAITING_NETWORK: &str = "Waiting for connection...";
}

const WRITE_BUFFER_BYTES: usize = 1 << 20;
const MAX_RETRIES: u32 = 10;
const MAX_VALIDATION_REFETCHES: u32 = 2;
const PROGRESS_RESETS_RETRIES_BYTES: u64 = 1 << 20;
const RETRY_DELAY_BASE_MS: u64 = 1000;
const MAX_PACKAGE_REFETCH_ROUNDS: u32 = 3;
const STALL_TIMEOUT: Duration = Duration::from_secs(15);
const NETWORK_RECHECK_INTERVAL: Duration = Duration::from_secs(3);
const NETWORK_PROBE_ROUNDS: u32 = 10;
const HOST_OUTAGE_BUDGET: Duration = Duration::from_secs(5 * 60);
const HOST_UNREACHABLE: &str = "Could not reach the download server";
const RANGE_PROBE_ATTEMPTS: u32 = 2;
const IDENTITY_MIN_BYTES: u64 = 16 << 20;
const SEGMENT_MIN_BYTES: u64 = 256 << 20;
const SEGMENT_TARGET_BYTES: u64 = 128 << 20;
const MAX_SEGMENTS_PER_FILE: usize = 8;
const MAX_CONCURRENT_BIG_FILES: usize = 3;
const SIDECAR_FORMAT: &str = "seg-v1";
const SIDECAR_PERSIST_INTERVAL: Duration = Duration::from_secs(1);
const FSYNC_ON_FINALIZE: bool = true;
pub(super) const GAME_CONFIG_FILE: &str = "launcherDownloadConfig.json";
pub(super) const LOCAL_INDEX_FILE: &str = "LocalGameResources.json";
pub(super) const INSTALL_MARKER_FILE: &str = ".peebify-install";
const LIVE_CHANNEL: &str = "default";

pub const HOYO_AUDIO_LANGUAGE: &str = "en-us";

pub(super) fn unsupported_channel(version_type: &str) -> Option<String> {
    (version_type != LIVE_CHANNEL).then(|| {
        format!(
            "The '{version_type}' version cannot be installed yet. Only the live version is supported, so nothing was written to the game folder."
        )
    })
}

pub fn combine_url(base: &str, part: &str) -> String {
    let base = base.strip_suffix('/').unwrap_or(base);
    let part = part.strip_prefix('/').unwrap_or(part);
    format!("{base}/{part}")
}

// ------------ Resume Files ------------
// Everything that lets a stopped download pick up where it left off: the .part files, the sidecars that remember each piece, and how big files are cut into ranges.
fn part_path_for(file_path: &Path) -> PathBuf {
    let mut os = file_path.as_os_str().to_os_string();
    os.push(".part");
    PathBuf::from(os)
}

fn sidecar_path_for(file_path: &Path) -> PathBuf {
    let mut os = file_path.as_os_str().to_os_string();
    os.push(".part.segments.json");
    PathBuf::from(os)
}

fn identity_path_for(file_path: &Path) -> PathBuf {
    let mut os = file_path.as_os_str().to_os_string();
    os.push(".part.id.json");
    PathBuf::from(os)
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".tmp");
    PathBuf::from(os)
}

fn discard_part(file_path: &Path) {
    let _ = std::fs::remove_file(part_path_for(file_path));
    remove_sidecar(file_path);
    remove_identity(file_path);
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct PartIdentity {
    size: u64,
    #[serde(default)]
    md5: String,
    #[serde(default)]
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_modified: Option<String>,
}

impl PartIdentity {
    fn expected(url: &str, size: u64, md5: &str) -> Self {
        Self {
            size,
            md5: md5.to_ascii_lowercase(),
            path: reqwest::Url::parse(url)
                .map(|u| u.path().to_string())
                .unwrap_or_default(),
            etag: None,
            last_modified: None,
        }
    }

    fn with_validators(mut self, headers: &reqwest::header::HeaderMap) -> Self {
        let header = |name: reqwest::header::HeaderName| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        self.etag = header(reqwest::header::ETAG).filter(|e| !e.starts_with("W/"));
        self.last_modified = header(reqwest::header::LAST_MODIFIED);
        self
    }

    fn matches(&self, current: &PartIdentity) -> bool {
        let differs = |a: &Option<String>, b: &Option<String>| {
            matches!((a, b), (Some(a), Some(b)) if a != b)
        };
        self.size == current.size
            && self.md5.eq_ignore_ascii_case(&current.md5)
            && (!current.md5.is_empty()
                || (self.path == current.path
                    && !differs(&self.etag, &current.etag)
                    && !differs(&self.last_modified, &current.last_modified)))
    }

    fn if_range(&self) -> Option<&str> {
        self.etag.as_deref().or(self.last_modified.as_deref())
    }
}

fn records_identity(size: u64, md5: &str) -> bool {
    md5.is_empty() || size >= IDENTITY_MIN_BYTES
}

fn load_identity(file_path: &Path) -> Option<PartIdentity> {
    let text = std::fs::read_to_string(identity_path_for(file_path)).ok()?;
    serde_json::from_str(&text).ok()
}

fn persist_identity(file_path: &Path, identity: &PartIdentity) {
    persist_json(&identity_path_for(file_path), identity);
}

fn remove_identity(file_path: &Path) {
    let path = identity_path_for(file_path);
    let _ = std::fs::remove_file(tmp_path_for(&path));
    let _ = std::fs::remove_file(path);
}

fn persist_json(path: &Path, value: &impl serde::Serialize) {
    let tmp = tmp_path_for(path);
    let Ok(text) = serde_json::to_string(value) else {
        return;
    };
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct SegmentState {
    start: u64,
    end: u64,
    done: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct SegmentSidecar {
    format: String,
    size: u64,
    segments: Vec<SegmentState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<PartIdentity>,
}

fn plan_segments(size: u64, target: u64, max_segments: usize) -> Vec<(u64, u64)> {
    if size == 0 || target == 0 || max_segments == 0 {
        return Vec::new();
    }
    let wanted = size.div_ceil(target).max(1);
    let count = wanted.min(max_segments as u64).max(1);
    let base = size / count;
    let remainder = size % count;
    let mut out = Vec::with_capacity(count as usize);
    let mut start = 0u64;
    for i in 0..count {
        let len = base + u64::from(i < remainder);
        out.push((start, start + len));
        start += len;
    }
    out
}

fn segments_per_file(connections: usize) -> usize {
    (connections / MAX_CONCURRENT_BIG_FILES).clamp(1, MAX_SEGMENTS_PER_FILE)
}

fn load_sidecar(file_path: &Path, expected_size: u64) -> Option<SegmentSidecar> {
    let text = std::fs::read_to_string(sidecar_path_for(file_path)).ok()?;
    let sidecar: SegmentSidecar = serde_json::from_str(&text).ok()?;
    (sidecar.format == SIDECAR_FORMAT
        && sidecar.size == expected_size
        && !sidecar.segments.is_empty()
        && sidecar
            .segments
            .iter()
            .all(|s| s.start <= s.end && s.done <= s.end - s.start))
    .then_some(sidecar)
}

fn persist_sidecar(file_path: &Path, sidecar: &SegmentSidecar) {
    persist_json(&sidecar_path_for(file_path), sidecar);
}

fn remove_sidecar(file_path: &Path) {
    let path = sidecar_path_for(file_path);
    let _ = std::fs::remove_file(tmp_path_for(&path));
    let _ = std::fs::remove_file(path);
}

fn bytes_on_disk(file_path: &Path, expected_size: u64) -> u64 {
    if let Some(sidecar) = load_sidecar(file_path, expected_size) {
        return sidecar.segments.iter().map(|s| s.done).sum();
    }
    std::fs::metadata(part_path_for(file_path))
        .map(|m| m.len())
        .unwrap_or(0)
}

fn url_host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "unknown host".to_string())
}

fn md5_mismatch(path: &Path, expected_size: u64, expected_md5: &str) -> Result<Option<String>, String> {
    let len = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(_) => return Ok(Some("missing".to_string())),
    };
    if len != expected_size {
        return Ok(Some(format!("{len} bytes instead of {expected_size}")));
    }
    let got = super::fs_util::md5_file(path, &mut || false, &mut |_| {})?;
    Ok((!got.eq_ignore_ascii_case(expected_md5)).then_some(got))
}

fn log_checksum_mismatch(file_id: &str, url: &str, expected_md5: &str, got: &str, bytes: u64) {
    log::warn!(
        "{file_id} failed its checksum: expected {expected_md5}, got {got}, {bytes} bytes from {}.",
        url_host(url)
    );
}

fn set_sparse(file: &std::fs::File, sparse: bool) -> bool {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;

    #[link(name = "kernel32")]
    extern "system" {
        fn DeviceIoControl(
            device: *mut c_void,
            code: u32,
            in_buffer: *const c_void,
            in_size: u32,
            out_buffer: *mut c_void,
            out_size: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }

    let flag: u8 = u8::from(sparse);
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            (&flag as *const u8).cast(),
            1,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        ) != 0
    }
}

async fn finalize_off_thread(part_path: &Path, file_path: &Path, file_id: &str) -> Result<(), String> {
    let part = part_path.to_path_buf();
    let dest = file_path.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || super::fs_util::finalize_replace(&part, &dest))
        .await
        .map_err(|e| format!("Finalize task panicked: {e}"))?
        .map_err(|e| format!("Finalize error for {file_id}: {e}"))
}

// ------------ Error Wording and Disk Space ------------
// Turns raw network and disk errors into plain advice, decides how connections are shared between files, and checks there is enough free space before writing.
fn user_facing_error(error: &str) -> Option<String> {
    if let Some(rest) = error.strip_prefix("HTTP Error: ") {
        let code = rest.split_whitespace().next().unwrap_or_default();
        return Some(format!(
            "The download server answered with HTTP {code}. Press Retry, or try again later if it keeps happening."
        ));
    }
    is_network_error(error).then(|| {
        "Lost the connection to the download server. Check your internet connection and press Retry."
            .to_string()
    })
}

fn is_network_error(message: &str) -> bool {
    super::fs_util::classify(message) == super::fs_util::FailureKind::Network
}

fn host_unreachable_message(host: &str) -> String {
    format!(
        "{HOST_UNREACHABLE} {host}. Your internet connection works, so the server may be down or blocked. Try again later."
    )
}

fn is_host_outage(message: &str) -> bool {
    message.starts_with(HOST_UNREACHABLE)
}

fn is_disk_full_error(message: &str) -> bool {
    message.starts_with("Not enough disk space")
        || super::fs_util::classify(message) == super::fs_util::FailureKind::DiskFull
}

fn is_damaged_archive_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "data error",
        "crc failed",
        "headers error",
        "unexpected end of archive",
        "can not open the file as archive",
        "is not archive",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn small_pool_workers(big_count: usize, connections: usize) -> usize {
    connections
        .saturating_sub(MAX_CONCURRENT_BIG_FILES.min(big_count) * segments_per_file(connections))
        .max(2)
}

enum PoolSlots {
    Fixed,
    Gated(Arc<tokio::sync::Semaphore>),
    Releases(Arc<SlotHandoff>),
}

struct SlotHandoff {
    small_slots: Arc<tokio::sync::Semaphore>,
    big_active: std::sync::atomic::AtomicUsize,
    connections: usize,
}

impl SlotHandoff {
    fn new(big_count: usize, connections: usize) -> Self {
        Self {
            small_slots: Arc::new(tokio::sync::Semaphore::new(small_pool_workers(
                big_count,
                connections,
            ))),
            big_active: std::sync::atomic::AtomicUsize::new(MAX_CONCURRENT_BIG_FILES.min(big_count)),
            connections,
        }
    }

    fn big_worker_done(&self) {
        let Ok(before) = self
            .big_active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        else {
            return;
        };
        let freed = small_pool_workers(before - 1, self.connections)
            .saturating_sub(small_pool_workers(before, self.connections));
        if freed > 0 {
            self.small_slots.add_permits(freed);
        }
    }
}

fn replacement_write_bytes(files: &[(u64, u64)], connections: usize) -> u64 {
    let growth: u64 = files
        .iter()
        .map(|(size, on_disk)| size.saturating_sub(*on_disk))
        .sum();
    let overlaps = |big: bool| -> Vec<u64> {
        let mut out: Vec<u64> = files
            .iter()
            .filter(|(size, _)| (*size >= SEGMENT_MIN_BYTES) == big)
            .map(|(size, on_disk)| (*size).min(*on_disk))
            .collect();
        out.sort_unstable_by(|a, b| b.cmp(a));
        out
    };
    let big = overlaps(true);
    let small = overlaps(false);
    let in_flight: u64 = (0..=MAX_CONCURRENT_BIG_FILES.min(big.len()))
        .map(|active| {
            big.iter().take(active).sum::<u64>()
                + small
                    .iter()
                    .take(small_pool_workers(active, connections))
                    .sum::<u64>()
        })
        .max()
        .unwrap_or(0);
    growth.saturating_add(in_flight)
}

pub(super) fn required_free_bytes(write_bytes: u64, multiplier: f64, headroom_floor: u64) -> u64 {
    if write_bytes == 0 {
        return 0;
    }
    let scaled = (write_bytes as f64 * multiplier) as u64;
    let headroom = (write_bytes / 20).max(headroom_floor);
    scaled.saturating_add(headroom)
}

pub(super) const SPLIT_ARCHIVE_MULTIPLIER: f64 = 2.1;

pub(super) fn splits_archives(mode: InstallMode) -> bool {
    matches!(mode, InstallMode::Hypergryph | InstallMode::Bluepoch)
}

pub(super) fn ensure_disk_space(
    install_path: &Path,
    write_bytes: u64,
    multiplier: f64,
    headroom_floor: u64,
) -> Result<(), String> {
    if write_bytes == 0 {
        return Ok(());
    }
    let required = required_free_bytes(write_bytes, multiplier, headroom_floor);
    let Some(free) = super::file_channels::free_bytes_for(install_path) else {
        log::warn!(
            "Could not read the free space for {}, so the disk space check was skipped.",
            install_path.display()
        );
        return Ok(());
    };
    if free < required {
        let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
        return Err(format!(
            "Not enough disk space in {}. This needs about {:.1} GB free but only {:.1} GB is available. Free up space and try again.",
            install_path.display(),
            gib(required),
            gib(free)
        ));
    }
    Ok(())
}

pub(super) const HEADROOM_INSTALL: u64 = 2 << 30;
pub(super) const HEADROOM_REPAIR: u64 = 256 << 20;

// ------------ Per-Game Download Config ------------
// Works out what to download for each publisher: the file list, base URLs and version, including Kuro's quality bundles and the Bluepoch and Girls' Frontline 2 lookups.
pub(super) struct GameConfig {
    pub(super) resources: Vec<Resource>,
    pub(super) base_url: String,
    pub(super) version: String,
    pub(super) mirrors: Vec<String>,
    pub(super) bundle: Option<KuroBundle>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KuroBundle {
    pub(super) name: String,
    pub(super) packs: Vec<String>,
}

pub(super) async fn resolve_bluepoch_config(
    profile: &Value,
    install_path: Option<&Path>,
) -> Result<GameConfig, String> {
    let current = install_path
        .and_then(bluepoch::installed_version)
        .unwrap_or_default();
    let package = bluepoch::fetch_package(profile, &current).await?;
    Ok(GameConfig {
        resources: package.resources,
        base_url: String::new(),
        version: package.version,
        mirrors: Vec::new(),
        bundle: None,
    })
}

pub(super) async fn resolve_gf2_config(profile: &Value) -> Result<GameConfig, String> {
    let config = gf2::fetch_config(profile).await?;
    Ok(GameConfig {
        resources: config.resources(),
        base_url: config.ab_base.clone(),
        version: config.ab_version,
        mirrors: Vec::new(),
        bundle: None,
    })
}

pub(super) async fn resolve_kuro_config(
    profile: &Value,
    quality: Option<&str>,
) -> Result<GameConfig, String> {
    let config_url = profile
        .get("gameConfigUrl")
        .and_then(|v| v.as_str())
        .ok_or("no gameConfigUrl for profile")?;
    let name = game_profiles::display_name(profile);
    let game_config =
        http::with_retry(|| http::get_json(config_url), 3, 1000, "Kuro game config").await?;
    if is_bundle_config(&game_config) {
        let quality = quality
            .or_else(|| game_profiles::resource_quality_default(profile))
            .ok_or_else(|| format!("{name}: no resource quality is set for this game."))?;
        return resolve_kuro_bundle(name, &game_config, &bundle_name(quality)).await;
    }
    let channel = kuro_channel(&game_config)?;

    let cdns = kuro_cdn_urls(channel);
    if cdns.is_empty() {
        return Err("game config has no cdnList url".to_string());
    }
    let index_file = channel["config"]["indexFile"].as_str().unwrap_or("");
    if index_file.trim().is_empty() {
        return Err(format!(
            "{name}: the server returned a configuration without a file list."
        ));
    }
    let base_rel = channel["config"]["baseUrl"].as_str().unwrap_or("");

    let (primary, index) = fetch_kuro_index(name, &cdns, index_file).await?;
    let resources = kuro_index_resources(&index);
    if resources.is_empty() {
        return Err(format!("{name}: the server returned an empty file list."));
    }
    let (base_url, mirrors) = kuro_mirrors(&cdns, primary, base_rel);
    Ok(GameConfig {
        resources,
        base_url,
        version: channel_version(channel),
        mirrors,
        bundle: None,
    })
}

async fn resolve_kuro_bundle(
    name: &str,
    game_config: &Value,
    bundle: &str,
) -> Result<GameConfig, String> {
    let packs = bundle_packs(game_config, bundle).map_err(|e| format!("{name}: {e}."))?;
    let cdns = kuro_cdn_urls(game_config);
    if cdns.is_empty() {
        return Err("game config has no cdnList url".to_string());
    }

    let mut resources = Vec::new();
    let mut seen = HashSet::new();
    let mut base_rel: Option<&str> = None;
    let mut primary = 0;
    let mut version = String::new();
    for pack_name in &packs {
        let pack = &game_config["resourcePacks"][pack_name];
        let index_file = pack["indexFile"].as_str().unwrap_or("").trim();
        if index_file.is_empty() {
            return Err(format!(
                "{name}: the {pack_name} pack of the {bundle} quality has no file list."
            ));
        }
        let pack_base = pack["baseUrl"].as_str().unwrap_or("");
        match base_rel {
            None => base_rel = Some(pack_base),
            Some(base) if base != pack_base => {
                return Err(format!(
                    "{name}: the {bundle} packs download from different folders, which Peebify can't handle yet."
                ));
            }
            Some(_) => {}
        }
        let (cdn, index) = fetch_kuro_index(name, &cdns, index_file).await?;
        if version.is_empty() {
            version = channel_version(pack);
            primary = cdn;
        }
        for resource in kuro_index_resources(&index) {
            if seen.insert(crate::backend::fs_util::manifest_key(resource.dest())) {
                resources.push(resource);
            }
        }
    }
    if resources.is_empty() {
        return Err(format!("{name}: the server returned an empty file list."));
    }
    log::info!(
        "{name}: {bundle} quality is {} ({} files).",
        packs.join(" + "),
        resources.len()
    );
    let (base_url, mirrors) = kuro_mirrors(&cdns, primary, base_rel.unwrap_or(""));
    Ok(GameConfig {
        resources,
        base_url,
        version,
        mirrors,
        bundle: Some(KuroBundle {
            name: bundle.to_string(),
            packs,
        }),
    })
}

async fn fetch_kuro_index(
    name: &str,
    cdns: &[String],
    index_file: &str,
) -> Result<(usize, Value), String> {
    let mut last_error = String::new();
    for (i, cdn_url) in cdns.iter().enumerate() {
        let resource_list_url = combine_url(cdn_url, index_file);
        match http::with_retry(
            || http::get_json(&resource_list_url),
            3,
            1000,
            "Kuro index",
        )
        .await
        {
            Ok(index) => return Ok((i, index)),
            Err(e) => {
                if i + 1 < cdns.len() {
                    log::warn!(
                        "{name}: the file list from {} failed ({e}), trying the next CDN.",
                        url_host(cdn_url)
                    );
                }
                last_error = e;
            }
        }
    }
    Err(last_error)
}

fn kuro_mirrors(cdns: &[String], primary: usize, base_rel: &str) -> (String, Vec<String>) {
    let mut mirrors: Vec<String> = std::iter::once(primary)
        .chain((0..cdns.len()).filter(|i| *i != primary))
        .map(|i| combine_url(&cdns[i], base_rel))
        .collect();
    let base_url = mirrors[0].clone();
    if mirrors.len() < 2 {
        mirrors.clear();
    }
    (base_url, mirrors)
}

pub(super) fn is_bundle_config(game_config: &Value) -> bool {
    game_config["bundles"].is_object() && game_config["resourcePacks"].is_object()
}

pub(super) fn bundle_name(quality: &str) -> String {
    quality.to_ascii_uppercase()
}

fn bundle_packs(game_config: &Value, bundle: &str) -> Result<Vec<String>, String> {
    let packs: Vec<String> = game_config["bundles"][bundle]["resourcePacks"]
        .as_array()
        .ok_or_else(|| format!("the server offers no {bundle} quality"))?
        .iter()
        .filter_map(|p| p.as_str().map(str::to_string))
        .collect();
    if packs.is_empty() {
        return Err(format!("the {bundle} quality lists no resource packs"));
    }
    Ok(packs)
}

pub(super) fn bundle_config_version(game_config: &Value) -> Option<String> {
    let first_pack = game_config["bundles"]
        .as_object()?
        .values()
        .find_map(|bundle| bundle["resourcePacks"].get(0)?.as_str())?;
    Some(channel_version(&game_config["resourcePacks"][first_pack])).filter(|v| !v.is_empty())
}

pub(super) fn bundle_bytes(game_config: &Value, bundle: &str) -> Option<(u64, u64)> {
    let mut download = 0u64;
    let mut install = 0u64;
    for pack_name in bundle_packs(game_config, bundle).ok()? {
        let pack = &game_config["resourcePacks"][pack_name.as_str()];
        download += config_size_at(pack, &["size"])?;
        install += config_size_at(pack, &["unCompressSize", "size"])?;
    }
    Some((download, install))
}

pub(super) fn selected_quality(app: &AppHandle, profile: &Value) -> Option<String> {
    let default = game_profiles::resource_quality_default(profile)?;
    let key = format!("games.{}.resourceQuality", game_profiles::profile_id(profile));
    let chosen = app.state::<BackendState>().config.get(&key);
    Some(match chosen.as_str() {
        Some(q) if profile["resourceQualityArgs"].get(q).is_some() => q.to_string(),
        _ => default.to_string(),
    })
}

fn recorded_qualities(install_path: &Path) -> Vec<String> {
    std::fs::read_to_string(install_path.join(GAME_CONFIG_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|config| {
            config["bundles"]
                .as_object()
                .map(|bundles| bundles.keys().map(|k| k.to_ascii_lowercase()).collect())
        })
        .unwrap_or_default()
}

pub(super) fn qualities_on_disk(profile: &Value, install_path: &Path) -> Vec<String> {
    let Some(default) = game_profiles::resource_quality_default(profile) else {
        return Vec::new();
    };
    let recorded: Vec<String> = recorded_qualities(install_path)
        .into_iter()
        .filter(|q| profile["resourceQualityArgs"].get(q).is_some())
        .collect();
    if recorded.is_empty() && install_path.join(GAME_CONFIG_FILE).is_file() {
        return vec![default.to_string()];
    }
    recorded
}

pub(super) fn installed_quality(
    profile: &Value,
    install_path: &Path,
    preferred: Option<&str>,
) -> Option<String> {
    let on_disk = qualities_on_disk(profile, install_path);
    preferred
        .filter(|p| on_disk.iter().any(|q| q == p))
        .map(str::to_string)
        .or_else(|| on_disk.into_iter().next())
}

fn kuro_cdn_urls(channel: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for url in channel["cdnList"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["url"].as_str())
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        if !out.iter().any(|seen| seen == url) {
            out.push(url.to_string());
        }
    }
    out
}

fn kuro_channel(game_config: &Value) -> Result<&Value, String> {
    game_config
        .get(LIVE_CHANNEL)
        .filter(|c| !c.is_null())
        .ok_or_else(|| format!("Could not find a '{LIVE_CHANNEL}' configuration."))
}

fn channel_version(channel: &Value) -> String {
    match channel.get("version") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn kuro_index_resources(index: &Value) -> Vec<Resource> {
    index
        .get("resource")
        .or_else(|| index.get("resources"))
        .and_then(|v| v.as_array())
        .map(|list| list.iter().map(Resource::from_json).collect())
        .unwrap_or_default()
}

pub(super) fn config_size_at(config: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| match config.get(*key) {
            Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
            Some(Value::Number(n)) => n.as_u64(),
            _ => None,
        })
        .filter(|n| *n > 0)
}

pub(super) struct KuroHeadline {
    pub(super) download_bytes: Option<u64>,
    pub(super) install_bytes: Option<u64>,
    pub(super) version: String,
}

pub(super) async fn kuro_headline(
    profile: &Value,
    quality: Option<&str>,
) -> Result<KuroHeadline, String> {
    let config_url = profile
        .get("gameConfigUrl")
        .and_then(|v| v.as_str())
        .ok_or("no gameConfigUrl for profile")?;
    let game_config = http::get_json(config_url).await?;
    if is_bundle_config(&game_config) {
        let bytes = quality
            .or_else(|| game_profiles::resource_quality_default(profile))
            .and_then(|q| bundle_bytes(&game_config, &bundle_name(q)));
        return Ok(KuroHeadline {
            download_bytes: bytes.map(|(download, _)| download),
            install_bytes: bytes.map(|(_, install)| install),
            version: bundle_config_version(&game_config).unwrap_or_default(),
        });
    }
    let channel = kuro_channel(&game_config)?;
    let config = channel.get("config").unwrap_or(&Value::Null);

    Ok(KuroHeadline {
        download_bytes: config_size_at(config, &["size", "fullSize"]),
        install_bytes: config_size_at(config, &["unCompressSize", "size", "fullSize"]),
        version: channel_version(channel),
    })
}

// ------------ Download Manager ------------
// The object that owns one game's download: its state, pause, resume and cancel, and the entry point that starts an install or update.
pub type CompletionHook = Box<dyn FnOnce(PathBuf) + Send + 'static>;

#[derive(Default)]
struct HostOutages(Mutex<HashMap<String, std::time::Instant>>);

impl HostOutages {
    fn down_for(&self, host: &str, now: std::time::Instant) -> Duration {
        let mut down = self.0.lock();
        let since = *down.entry(host.to_string()).or_insert(now);
        now.saturating_duration_since(since)
    }

    fn clear(&self, host: &str) {
        let mut down = self.0.lock();
        if !down.is_empty() {
            down.remove(host);
        }
    }

    fn clear_all(&self) {
        self.0.lock().clear();
    }
}

#[derive(Default)]
struct Mirrors {
    bases: Vec<String>,
    active: Option<String>,
    tried_hosts: HashSet<String>,
}

pub struct GameDownloadManager {
    app: AppHandle,
    pub tracker: Arc<progress::ProgressTracker>,
    profile: Mutex<&'static Value>,
    is_downloading: AtomicBool,
    starting: AtomicBool,
    extracting: AtomicBool,
    gate: progress::RunGate,
    outage: http::OutageGate,
    host_down_since: HostOutages,
    mirrors: Mutex<Mirrors>,
    on_complete: Mutex<Option<CompletionHook>>,
    power: Mutex<std::sync::Weak<super::perf::TransferGuard>>,
    current_patch_version: Mutex<Option<String>>,
    kuro_bundle: Mutex<Option<KuroBundle>>,
    verified_this_session: Mutex<HashSet<String>>,
    install_writes: InstallWrites,
}

#[derive(Default)]
struct InstallWrites {
    in_place: AtomicBool,
    touched: AtomicBool,
    recording: Mutex<()>,
}

impl InstallWrites {
    fn begin(&self, in_place: bool) {
        self.in_place.store(in_place, Ordering::SeqCst);
        self.touched.store(false, Ordering::SeqCst);
    }

    fn end(&self) {
        self.in_place.store(false, Ordering::SeqCst);
    }

    fn before_write(&self, record: impl FnOnce()) {
        if !self.in_place.load(Ordering::SeqCst) || self.touched.load(Ordering::SeqCst) {
            return;
        }
        let _recording = self.recording.lock();
        if !self.touched.load(Ordering::SeqCst) {
            record();
            self.touched.store(true, Ordering::SeqCst);
        }
    }
}

fn replaces_live_file(mode: InstallMode, dest: &str) -> bool {
    match mode {
        InstallMode::Default | InstallMode::Unknown => true,
        InstallMode::Gf2 => gf2::is_bundle_resource(&Resource::new(dest, 0, "")),
        _ => false,
    }
}

struct ExtractingGuard<'a>(&'a AtomicBool);

impl Drop for ExtractingGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl GameDownloadManager {
    pub fn new(app: AppHandle, profile: &'static Value) -> Arc<Self> {
        Arc::new(Self {
            app,
            tracker: Arc::new(progress::ProgressTracker::new()),
            profile: Mutex::new(profile),
            is_downloading: AtomicBool::new(false),
            starting: AtomicBool::new(false),
            extracting: AtomicBool::new(false),
            gate: progress::RunGate::new(),
            outage: http::OutageGate::new(),
            host_down_since: HostOutages::default(),
            mirrors: Mutex::new(Mirrors::default()),
            on_complete: Mutex::new(None),
            power: Mutex::new(std::sync::Weak::new()),
            current_patch_version: Mutex::new(None),
            kuro_bundle: Mutex::new(None),
            verified_this_session: Mutex::new(HashSet::new()),
            install_writes: InstallWrites::default(),
        })
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

    fn install_mode(&self) -> Option<&'static str> {
        game_profiles::install_mode(self.profile())
    }

    fn mode(&self) -> InstallMode {
        InstallMode::of(self.profile())
    }

    fn is_hypergryph(&self) -> bool {
        self.mode() == InstallMode::Hypergryph
    }

    fn is_sophon(&self) -> bool {
        self.mode() == InstallMode::Sophon
    }

    fn is_gf2(&self) -> bool {
        self.mode() == InstallMode::Gf2
    }

    fn is_nte(&self) -> bool {
        self.mode() == InstallMode::Netease
    }

    fn is_bluepoch(&self) -> bool {
        self.mode() == InstallMode::Bluepoch
    }

    fn is_bd2(&self) -> bool {
        self.mode() == InstallMode::Bd2
    }

    fn uses_split_archives(&self) -> bool {
        splits_archives(self.mode())
    }

    fn prunes_removed_resources(&self) -> bool {
        self.mode() == InstallMode::Default
    }

    fn is_cancelled(&self) -> bool {
        self.gate.is_cancelled()
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

    fn validation_control(self: &Arc<Self>) -> Arc<dyn progress::Control> {
        Arc::new(EngineHooks::transfer(Arc::clone(self)))
    }

    fn downloading_status(&self) -> String {
        match self.current_patch_version.lock().as_ref() {
            Some(v) => format!("Downloading Patch {v}"),
            None => status::DOWNLOADING.to_string(),
        }
    }

    fn send_progress(&self, update: impl Into<Update>, status_text: &str, extra: Value) {
        let update = update.into();
        let paused = self.gate.is_paused() && !update.phase().is_terminal();
        let status_text = if paused { status::PAUSED } else { status_text };

        let mut progress_data = self.tracker.calculate_metrics();
        if let Some(map) = progress_data.as_object_mut() {
            map.insert("status".to_string(), json!(status_text));
            if let Value::Object(extra_map) = extra {
                for (k, v) in extra_map {
                    map.insert(k, v);
                }
            }
            map.insert("gameId".to_string(), json!(self.profile_id()));
        }
        super::queue::publish(
            &self.app,
            "download-progress",
            update.paused(paused),
            progress_data,
        );
    }

    pub fn is_paused(&self) -> bool {
        self.is_downloading.load(Ordering::SeqCst) && self.gate.is_paused()
    }

    fn cancel_aware(&self, error: String) -> String {
        if self.is_cancelled() {
            "Download aborted by user.".to_string()
        } else {
            error
        }
    }

    pub fn is_extracting(&self) -> bool {
        self.extracting.load(Ordering::SeqCst)
    }

    fn begin_extracting(&self) -> ExtractingGuard<'_> {
        self.extracting.store(true, Ordering::SeqCst);
        if self.gate.is_paused() {
            self.gate.resume();
            self.set_paused_power(false);
            log::info!("Unpacking cannot pause, so the pending pause was lifted.");
        }
        ExtractingGuard(&self.extracting)
    }

    pub fn pause_download(&self) {
        if self.is_extracting() {
            log::info!("Pause ignored because unpacking cannot be paused.");
            return;
        }
        if self.is_downloading.load(Ordering::SeqCst) && self.gate.pause() {
            self.set_paused_power(true);
            self.send_progress(Phase::Downloading, status::PAUSED, json!({}));
            log::info!("Download paused ({}).", self.profile_id());
        }
    }

    pub fn resume_download(&self) {
        if self.is_downloading.load(Ordering::SeqCst) && self.gate.resume() {
            self.set_paused_power(false);
            self.tracker.reset_speed_baseline();
            self.send_progress(
                Update::new(Phase::Downloading).resumed(),
                &self.downloading_status(),
                json!({}),
            );
            log::info!("Download resumed ({}).", self.profile_id());
        }
    }

    pub fn cancel_download(&self) {
        if self.is_downloading.load(Ordering::SeqCst) || self.starting.load(Ordering::SeqCst) {
            log::info!("Cancelling download ({})...", self.profile_id());
            self.gate.cancel();
        }
    }

    pub fn prepare_start(&self) {
        if self.is_downloading.load(Ordering::SeqCst) {
            return;
        }
        self.gate.reset();
        self.starting.store(true, Ordering::SeqCst);
    }

    pub fn abandon_start(&self) {
        self.starting.store(false, Ordering::SeqCst);
    }

    pub fn start_cancelled(&self) -> bool {
        self.starting.load(Ordering::SeqCst) && self.gate.is_cancelled()
    }

    async fn wait_while_paused(&self) {
        self.gate.wait_while_paused().await;
    }

    fn note_install_write(&self) {
        self.install_writes.before_write(|| {
            log::info!(
                "Update of {} is writing into the installed game, so it can't be played until the update or a repair finishes.",
                self.profile_id()
            );
            super::file_channels::mark_update_incomplete(&self.app, &self.profile_id());
        });
    }

    fn before_write(self: &Arc<Self>) -> sophon::BeforeWrite {
        let me = Arc::clone(self);
        Some(Arc::new(move || me.note_install_write()))
    }

    async fn finalize_download(
        &self,
        part_path: &Path,
        file_path: &Path,
        file_id: &str,
    ) -> Result<(), String> {
        if replaces_live_file(self.mode(), file_id) {
            self.note_install_write();
        }
        finalize_off_thread(part_path, file_path, file_id).await
    }

    pub async fn download_game_with(
        self: &Arc<Self>,
        install_path: &Path,
        in_place: bool,
        on_complete: Option<CompletionHook>,
    ) -> Value {
        if self.is_downloading.swap(true, Ordering::SeqCst) {
            return err_response("A download is already in progress.");
        }
        self.install_writes.begin(in_place);
        if !self.starting.swap(false, Ordering::SeqCst) {
            self.gate.reset();
        }
        self.tracker.reset();
        *self.current_patch_version.lock() = None;
        *self.on_complete.lock() = on_complete;
        *self.mirrors.lock() = Mirrors::default();
        self.host_down_since.clear_all();

        let power = Arc::new(super::perf::TransferGuard::acquire());
        *self.power.lock() = Arc::downgrade(&power);
        if self.gate.is_paused() {
            power.set_paused(true);
        }
        log::info!(
            "Download for {} starting: mode {}, installed version {}, install path {}",
            self.profile_id(),
            self.install_mode().unwrap_or("default"),
            super::game_manager::local_game_version_for(
                self.profile(),
                &install_path.to_string_lossy()
            )
            .as_deref()
            .unwrap_or("none"),
            install_path.display()
        );
        let result = if self.is_cancelled() {
            Err("Download aborted by user.".to_string())
        } else {
            self.run_download(install_path).await
        };

        self.is_downloading.store(false, Ordering::SeqCst);
        self.install_writes.end();
        self.gate.clear_paused();
        self.tracker.reset();
        *self.current_patch_version.lock() = None;
        self.on_complete.lock().take();

        match result {
            Ok(response) => response,
            Err(e) => self.handle_download_error(&e),
        }
    }

    async fn run_download(self: &Arc<Self>, install_path: &Path) -> Result<Value, String> {
        if self.is_sophon() {
            prepare_install_dir(install_path, &self.profile_id())?;
            return self.sophon_install(install_path).await;
        }

        if self.is_nte() {
            prepare_install_dir(install_path, &self.profile_id())?;
            return self.nte_install(install_path).await;
        }

        if self.is_bd2() {
            prepare_install_dir(install_path, &self.profile_id())?;
            return self.bd2_install(install_path).await;
        }

        self.send_progress(Phase::Scanning, status::FETCHING_CONFIG, json!({}));
        let mut config = self.get_game_config(Some(install_path)).await?;
        *self.current_patch_version.lock() = Some(config.version.clone());
        self.mirrors.lock().bases = config.mirrors.clone();
        prepare_install_dir(install_path, &self.profile_id())?;

        if self.is_hypergryph() {
            if let Some(response) = self.hypergryph_delta_update(install_path, &config).await? {
                return Ok(response);
            }
        }

        if self.is_gf2() {
            if let Some(client) = self.gf2_client_resource(install_path).await? {
                config.resources.insert(0, client);
            }
        }

        self.verified_this_session.lock().clear();
        let files_to_download = self
            .get_files_to_download(&config.resources, install_path)
            .await?;

        if files_to_download.is_empty() {
            log::info!(
                "All files are valid for {}, no download needed.",
                self.profile_id()
            );
            return self
                .complete_download(install_path, &config.version, &config.resources)
                .await;
        }

        let download_bytes: u64 = files_to_download.iter().map(|r| r.size).sum();
        let unpacked_bytes = if self.is_hypergryph() {
            self.hypergryph_unpacked_bytes(&config.resources).await
        } else {
            None
        };
        let (write_bytes, multiplier) = match unpacked_bytes {
            Some(unpacked) => (download_bytes.saturating_add(unpacked), 1.0),
            None if self.uses_split_archives() => (download_bytes, SPLIT_ARCHIVE_MULTIPLIER),
            None => (
                self.update_write_bytes(install_path, &files_to_download)
                    .await?,
                1.0,
            ),
        };
        ensure_disk_space(install_path, write_bytes, multiplier, HEADROOM_INSTALL)?;
        let largest = files_to_download.iter().map(|r| r.size).max().unwrap_or(0);
        if let Some(hint) = super::fs_util::fat_limit_message(install_path, largest) {
            return Err(hint);
        }

        if self.prunes_removed_resources() {
            clear_scan_record(install_path);
        }
        let mut pending = files_to_download;
        self.execute_download(pending.clone(), &config.base_url, install_path)
            .await?;

        if self.is_gf2() {
            log::info!(
                "GF2: every fetched file was checksummed as it landed; skipping the second full pass over {} resources.",
                config.resources.len()
            );
            return self
                .complete_download(install_path, &config.version, &config.resources)
                .await;
        }

        let meta = ValidationMeta {
            is_final: true,
            version: Some(config.version.clone()),
        };
        let mut round = 0u32;
        loop {
            if self.is_cancelled() {
                return Err("Download aborted by user.".to_string());
            }

            let verified = self.verified_this_session.lock().clone();
            let needs_hash: Vec<Resource> = pending
                .iter()
                .filter(|r| {
                    let dest = r.dest();
                    let md5 = r.md5();
                    !dest.is_empty() && !md5.is_empty() && !verified.contains(dest)
                })
                .cloned()
                .collect();

            let mut invalid = if needs_hash.is_empty() {
                log::info!(
                    "Every downloaded file was checksummed while streaming — no full re-read needed."
                );
                Vec::new()
            } else {
                log::info!(
                    "Verifying {} file(s) that could not be hashed while streaming...",
                    needs_hash.len()
                );
                validator::validate_resources(
                    &self.app,
                    &self.tracker,
                    Arc::new(needs_hash),
                    install_path,
                    self.gate.flag(),
                    Some(self.validation_control()),
                    &meta,
                    &self.profile_id(),
                )
                .await?
            };

            if round == 0 {
                let mut flagged: HashSet<String> =
                    invalid.iter().map(|r| r.dest().to_string()).collect();
                let listed: Vec<(String, u64)> = config
                    .resources
                    .iter()
                    .filter(|r| !r.dest().is_empty() && !flagged.contains(r.dest()))
                    .map(|r| (r.dest().to_string(), r.size))
                    .collect();
                let dir = install_path.to_path_buf();
                let short =
                    tauri::async_runtime::spawn_blocking(move || undersized_files(&dir, listed))
                        .await
                        .map_err(|e| format!("size check task panicked: {e}"))?;
                for resource in &config.resources {
                    let dest = resource.dest();
                    if short.contains(dest) && flagged.insert(dest.to_string()) {
                        invalid.push(resource.clone());
                    }
                }
            }

            if invalid.is_empty() {
                break;
            }
            round += 1;
            if round > MAX_PACKAGE_REFETCH_ROUNDS {
                return Err(format!(
                    "Validation failed: {} file(s) are still corrupt after {MAX_PACKAGE_REFETCH_ROUNDS} re-fetch attempts.",
                    invalid.len()
                ));
            }

            let count = invalid.len();
            log::warn!(
                "Validation found {count} incomplete file(s); re-fetching them (round {round}/{MAX_PACKAGE_REFETCH_ROUNDS})."
            );
            let refetch_bytes: u64 = invalid.iter().map(|r| r.size).sum();
            self.tracker.reset();
            self.tracker.set_totals(refetch_bytes as f64, count);
            self.tracker.set_file_sizes(
                invalid
                    .iter()
                    .map(|r| (r.dest().to_string(), r.size as f64))
                    .collect(),
            );
            self.send_progress(
                Phase::Downloading,
                &format!(
                    "Re-fetching {count} incomplete package{}...",
                    if count == 1 { "" } else { "s" }
                ),
                json!({}),
            );

            {
                let mut verified = self.verified_this_session.lock();
                for resource in &invalid {
                    verified.remove(resource.dest());
                }
            }
            self.execute_download(invalid.clone(), &config.base_url, install_path)
                .await?;
            pending = invalid;
        }

        self.complete_download(install_path, &config.version, &config.resources)
            .await
    }

    async fn update_write_bytes(
        &self,
        install_path: &Path,
        files: &[Resource],
    ) -> Result<u64, String> {
        let gf2 = self.is_gf2();
        let dir = install_path.to_path_buf();
        let listed: Vec<(String, u64, bool)> = files
            .iter()
            .map(|r| (r.dest().to_string(), r.size, gf2 && !gf2::is_bundle_resource(r)))
            .collect();
        tauri::async_runtime::spawn_blocking(move || {
            let sized: Vec<(u64, u64)> = listed
                .iter()
                .map(|(dest, size, full)| {
                    let on_disk = if *full {
                        0
                    } else {
                        super::fs_util::safe_join(&dir, dest)
                            .ok()
                            .and_then(|p| std::fs::metadata(p).ok())
                            .filter(|m| m.is_file())
                            .map_or(0, |m| m.len())
                    };
                    (*size, on_disk)
                })
                .collect();
            replacement_write_bytes(&sized, super::perf::DOWNLOAD_CONCURRENCY)
        })
        .await
        .map_err(|e| format!("disk space task panicked: {e}"))
    }

    pub async fn remote_resources(&self) -> Result<(Vec<Resource>, String), String> {
        let c = self.get_game_config(None).await?;
        Ok((c.resources, c.version))
    }

    async fn get_game_config(&self, install_path: Option<&Path>) -> Result<GameConfig, String> {
        *self.kuro_bundle.lock() = None;
        let profile = self.profile();
        if !game_profiles::is_managed(profile) {
            return Err(
                "This game is not supported for managed install in Peebify Launcher yet."
                    .to_string(),
            );
        }

        if self.is_hypergryph() {
            let latest = hypergryph::get_latest_game(profile).await?;
            let version = latest["version"].as_str().unwrap_or("");
            if version.is_empty() {
                return Err(format!(
                    "Failed to resolve {} package metadata from the Gryphline API.",
                    game_profiles::display_name(profile)
                ));
            }
            let resources = hypergryph::packs_as_resources(&latest["packs"]);
            let total_bytes: u64 = resources.iter().map(|r| r.size).sum();
            log::info!(
                "{} package {} resolved with {} part(s), {:.2} GB.",
                game_profiles::display_name(profile),
                version,
                resources.len(),
                total_bytes as f64 / 1_073_741_824.0
            );
            return Ok(GameConfig {
                resources,
                base_url: String::new(),
                version: version.to_string(),
                mirrors: Vec::new(),
                bundle: None,
            });
        }

        if self.is_bluepoch() {
            let config = resolve_bluepoch_config(profile, install_path).await?;
            log::info!(
                "{} package {} resolved with {} archive(s).",
                game_profiles::display_name(profile),
                config.version,
                config.resources.len()
            );
            return Ok(config);
        }

        if self.is_gf2() {
            let config = resolve_gf2_config(profile).await?;
            log::info!(
                "{} resources {} resolved with {} files.",
                game_profiles::display_name(profile),
                config.version,
                config.resources.len()
            );
            return Ok(config);
        }

        let quality = selected_quality(&self.app, profile);
        let config = resolve_kuro_config(profile, quality.as_deref()).await?;
        *self.kuro_bundle.lock() = config.bundle.clone();
        Ok(config)
    }

    // ------------ Shared Download Core ------------
    // The part every plain file-list game uses: working out which files are missing or wrong, the worker pool, retries, mirror switching, and streaming each file to disk, in pieces when it is huge.
    async fn get_files_to_download(
        self: &Arc<Self>,
        resources: &[Resource],
        install_path: &Path,
    ) -> Result<Vec<Resource>, String> {
        if self.uses_split_archives() {
            let local = super::game_manager::local_game_version_at(install_path);
            let remote = self.current_patch_version.lock().clone();
            if let (Some(local), Some(remote)) = (local, remote) {
                if versions_semver_equal(&local, &remote) {
                    log::info!(
                        "Split-archive install: installed version {local} matches remote {remote}; skipping package validation (installer archives may already be removed after extraction)."
                    );
                    self.tracker.reset();
                    self.tracker.set_totals(0.0, 0);
                    return Ok(Vec::new());
                }
            }
        }

        let mut resources = resources.to_vec();
        if self.is_gf2() {
            let before = resources.len();
            let path = install_path.to_path_buf();
            let scan = resources.clone();
            let unchanged = tauri::async_runtime::spawn_blocking(move || {
                gf2::unchanged_resources(&path, &scan)
            })
            .await
            .map_err(|e| format!("GF2 bundle scan task panicked: {e}"))?;
            resources.retain(|r| !unchanged.contains(r.dest()));
            if before != resources.len() {
                log::info!(
                    "GF2: {} of {before} resources are already recorded in the install's Version.txt.",
                    before - resources.len()
                );
            }
        }

        let mut known_stale: Vec<Resource> = Vec::new();
        if self.prunes_removed_resources() {
            let before = resources.len();
            let path = install_path.to_path_buf();
            let scan = resources.clone();
            let verdicts = tauri::async_runtime::spawn_blocking(move || {
                scan_record_verdicts(
                    &path,
                    scan.iter().map(|r| (r.dest(), r.size, r.md5())),
                )
            })
            .await
            .map_err(|e| format!("scan record task panicked: {e}"))?;
            if !verdicts.trusted.is_empty() || !verdicts.stale.is_empty() {
                let (stale, rest): (Vec<Resource>, Vec<Resource>) = resources
                    .into_iter()
                    .filter(|r| {
                        !verdicts
                            .trusted
                            .contains(&super::fs_util::manifest_key(r.dest()))
                    })
                    .partition(|r| {
                        verdicts
                            .stale
                            .contains(&super::fs_util::manifest_key(r.dest()))
                    });
                resources = rest;
                known_stale = stale;
                log::info!(
                    "{}: {} of {before} files are unchanged since the last verified install and skip hashing ({} of them changed in this version); checking {}.",
                    self.profile_id(),
                    before - resources.len(),
                    known_stale.len(),
                    resources.len()
                );
            }
        }

        let meta = ValidationMeta {
            is_final: false,
            version: self.current_patch_version.lock().clone(),
        };
        let mut invalid = validator::validate_resources(
            &self.app,
            &self.tracker,
            Arc::new(resources),
            install_path,
            self.gate.flag(),
            Some(self.validation_control()),
            &meta,
            &self.profile_id(),
        )
        .await?;
        invalid.extend(known_stale);

        let total_size: u64 = invalid.iter().map(|r| r.size).sum();
        self.tracker.reset();
        self.tracker.set_totals(total_size as f64, invalid.len());
        self.tracker.set_file_sizes(
            invalid
                .iter()
                .map(|r| (r.dest().to_string(), r.size as f64))
                .collect(),
        );

        log::info!(
            "Files to download for {}: {}, Total size: {:.2}GB",
            self.profile_id(),
            invalid.len(),
            progress::gib(total_size as f64)
        );
        Ok(invalid)
    }

    async fn execute_download(
        self: &Arc<Self>,
        files: Vec<Resource>,
        base_url: &str,
        install_path: &Path,
    ) -> Result<(), String> {
        log::info!(
            "Starting download of {} files for {}...",
            files.len(),
            self.profile_id()
        );
        self.tracker
            .set_phase("downloading");
        self.send_progress(Phase::Downloading, &self.downloading_status(), json!({}));

        let (big, small): (Vec<Resource>, Vec<Resource>) =
            files.into_iter().partition(|r| r.size >= SEGMENT_MIN_BYTES);

        if !big.is_empty() {
            log::info!(
                "{} large file(s) will use segmented downloads, {} concurrently.",
                big.len(),
                MAX_CONCURRENT_BIG_FILES.min(big.len())
            );
        }
        if !big.is_empty() && !small.is_empty() {
            let connections = super::perf::DOWNLOAD_CONCURRENCY;
            let handoff = Arc::new(SlotHandoff::new(big.len(), connections));
            let small_slots = PoolSlots::Gated(Arc::clone(&handoff.small_slots));
            let (big_result, small_result) = tokio::join!(
                self.run_download_pool(
                    big,
                    MAX_CONCURRENT_BIG_FILES,
                    base_url,
                    install_path,
                    PoolSlots::Releases(handoff),
                ),
                self.run_download_pool(small, connections, base_url, install_path, small_slots)
            );
            big_result?;
            small_result?;
        } else if !big.is_empty() {
            self.run_download_pool(
                big,
                MAX_CONCURRENT_BIG_FILES,
                base_url,
                install_path,
                PoolSlots::Fixed,
            )
            .await?;
        } else if !small.is_empty() {
            self.run_download_pool(
                small,
                super::perf::DOWNLOAD_CONCURRENCY,
                base_url,
                install_path,
                PoolSlots::Fixed,
            )
            .await?;
        }
        if self.is_cancelled() {
            return Err("Download was cancelled by the user.".to_string());
        }
        Ok(())
    }

    async fn run_download_pool(
        self: &Arc<Self>,
        files: Vec<Resource>,
        worker_count: usize,
        base_url: &str,
        install_path: &Path,
        slots: PoolSlots,
    ) -> Result<(), String> {
        let worker_count = worker_count.min(files.len()).max(1);
        let queue: Arc<Mutex<VecDeque<Resource>>> = Arc::new(Mutex::new(files.into()));
        let (gate, handoff) = match slots {
            PoolSlots::Fixed => (None, None),
            PoolSlots::Gated(gate) => (Some(gate), None),
            PoolSlots::Releases(handoff) => (None, Some(handoff)),
        };
        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let me = Arc::clone(self);
            let queue = Arc::clone(&queue);
            let gate = gate.clone();
            let handoff = handoff.clone();
            let base_url = base_url.to_string();
            let install_path = install_path.to_path_buf();
            workers.push(tauri::async_runtime::spawn(async move {
                let _permit = match &gate {
                    Some(gate) => Some(
                        gate.acquire()
                            .await
                            .map_err(|e| format!("Download slots closed: {e}"))?,
                    ),
                    None => None,
                };
                loop {
                    if me.is_cancelled() {
                        return Err("Download aborted by user.".to_string());
                    }
                    if me.gate.is_paused() {
                        me.wait_while_paused().await;
                        continue;
                    }
                    let resource = queue.lock().pop_front();
                    let Some(resource) = resource else {
                        if let Some(handoff) = &handoff {
                            handoff.big_worker_done();
                        }
                        return Ok(());
                    };
                    me.download_file_with_retry(&resource, &base_url, &install_path)
                        .await?;
                }
            }));
        }

        progress::join_workers(workers, "worker").await
    }

    async fn download_file_with_retry(
        self: &Arc<Self>,
        resource: &Resource,
        base_url: &str,
        install_path: &Path,
    ) -> Result<(), String> {
        let dest = resource.dest().to_string();
        let file_path = crate::backend::fs_util::safe_join(install_path, &dest)?;
        let file_size = resource.size;
        let md5 = resource.md5().to_string();

        let mut attempt: u32 = 0;
        let mut validation_failures: u32 = 0;
        let mut share_failures: u32 = 0;
        loop {
            if self.is_cancelled() {
                return Err("Download aborted by user.".to_string());
            }
            let url = self.resource_url(resource, base_url, &dest);
            let pause_epoch = self.gate.pause_epoch();
            let bytes_before = bytes_on_disk(&file_path, file_size);

            let result: Result<(), String> = async {
                if let Some(parent) = file_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        super::fs_util::fmt_io(
                            &format!("Write error: could not create {}", parent.display()),
                            &e,
                        )
                    })?;
                }

                let should_segment =
                    file_size >= SEGMENT_MIN_BYTES || load_sidecar(&file_path, file_size).is_some();
                let verified_inline = if should_segment {
                    match self
                        .download_file_segmented(&url, &file_path, &dest, file_size, &md5)
                        .await?
                    {
                        Some(verified) => verified,
                        None => {
                            self.stream_to_file(&url, &file_path, &dest, file_size, &md5)
                                .await?
                        }
                    }
                } else {
                    self.stream_to_file(&url, &file_path, &dest, file_size, &md5)
                        .await?
                };

                if file_size == 0 && md5.is_empty() {
                    return Ok(());
                }
                let hashed = if verified_inline {
                    true
                } else if !md5.is_empty() {
                    let path = file_path.clone();
                    let expected = md5.clone();
                    let mismatch = tauri::async_runtime::spawn_blocking(move || {
                        md5_mismatch(&path, file_size, &expected)
                    })
                    .await
                    .map_err(|e| e.to_string())??;
                    if let Some(got) = mismatch {
                        log_checksum_mismatch(&dest, &url, &md5, &got, file_size);
                        return Err("File validation failed after download.".to_string());
                    }
                    true
                } else {
                    if !validator::FileValidator::quick_validate(&file_path, file_size) {
                        let len = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
                        log::warn!(
                            "{dest} is {len} bytes after download, expected {file_size}, from {}.",
                            url_host(&url)
                        );
                        return Err("File validation failed after download.".to_string());
                    }
                    false
                };
                if hashed {
                    self.verified_this_session.lock().insert(dest.clone());
                }
                Ok(())
            }
            .await;

            match result {
                Ok(()) => {
                    self.tracker.update_file_progress(&dest, 0.0, true);
                    self.clear_host_outage(&url);
                    return Ok(());
                }
                Err(e) => {
                    if self.is_cancelled() {
                        return Err("Download aborted by user.".to_string());
                    }
                    let advanced = bytes_on_disk(&file_path, file_size).saturating_sub(bytes_before);
                    if advanced >= PROGRESS_RESETS_RETRIES_BYTES {
                        self.clear_host_outage(&url);
                    }
                    if http::permanent_client_status(&e).is_some() {
                        log::warn!("Download of {dest} refused by the server, not retrying: {e}");
                        return Err(e);
                    }
                    if is_host_outage(&e) {
                        if self.switch_mirror(resource, &url) {
                            self.tracker.reset_speed_baseline();
                            continue;
                        }
                        return Err(e);
                    }
                    let kind = super::fs_util::classify(&e);
                    if kind == super::fs_util::FailureKind::Validation {
                        if replaces_live_file(self.mode(), &dest) && file_path.exists() {
                            self.note_install_write();
                        }
                        let _ = std::fs::remove_file(&file_path);
                        discard_part(&file_path);
                        validation_failures += 1;
                        if validation_failures > MAX_VALIDATION_REFETCHES {
                            return Err(format!(
                                "The server sent a copy of {dest} that does not match its checksum. Try again later."
                            ));
                        }
                        log::warn!(
                            "{dest} failed validation ({validation_failures}/{MAX_VALIDATION_REFETCHES}), downloading it again."
                        );
                        tokio::time::sleep(http::retry_delay(1, RETRY_DELAY_BASE_MS)).await;
                        continue;
                    }
                    if kind == super::fs_util::FailureKind::DiskFull {
                        if let Some(hint) = super::fs_util::fat_limit_message(&file_path, file_size) {
                            return Err(format!("{hint} ({e})"));
                        }
                        return Err(format!(
                            "Not enough disk space while downloading {dest}. Free up space and try again. ({e})"
                        ));
                    }
                    if kind == super::fs_util::FailureKind::Locked {
                        return Err(format!(
                            "{dest} is in use and could not be replaced. Close {} and press Update again. ({e})",
                            game_profiles::display_name(self.profile())
                        ));
                    }
                    if kind == super::fs_util::FailureKind::AccessDenied {
                        return Err(format!(
                            "{} ({e})",
                            super::fs_util::not_writable_message(install_path)
                        ));
                    }
                    if kind == super::fs_util::FailureKind::ShareLost {
                        share_failures += 1;
                        if share_failures < super::fs_util::SHARE_LOST_ATTEMPTS {
                            log::warn!(
                                "The drive holding {dest} stopped responding ({share_failures}/{}), trying again: {e}",
                                super::fs_util::SHARE_LOST_ATTEMPTS
                            );
                            tokio::time::sleep(http::retry_delay(share_failures, RETRY_DELAY_BASE_MS))
                                .await;
                            continue;
                        }
                    }
                    if let Some(message) = super::fs_util::drive_failure_message(
                        kind,
                        install_path,
                        &file_path,
                        file_size,
                    ) {
                        return Err(format!("{message} ({e})"));
                    }
                    if self.gate.is_paused() {
                        self.wait_while_paused().await;
                        continue;
                    }
                    if self.gate.pause_epoch() != pause_epoch {
                        log::info!("Download of {dest} was interrupted by a pause, retrying without counting it: {e}");
                        continue;
                    }
                    if is_network_error(&e) && !http::can_reach(&url).await {
                        if let Err(waited) = self.wait_for_network(&url).await {
                            if is_host_outage(&waited) && self.switch_mirror(resource, &url) {
                                self.tracker.reset_speed_baseline();
                                continue;
                            }
                            return Err(waited);
                        }
                        self.tracker.reset_speed_baseline();
                        continue;
                    }
                    if advanced >= PROGRESS_RESETS_RETRIES_BYTES && attempt > 0 {
                        log::info!(
                            "{dest} advanced {advanced} bytes before failing, so its retry count starts over."
                        );
                        attempt = 0;
                    }
                    attempt += 1;
                    if attempt >= MAX_RETRIES {
                        return Err(e);
                    }
                    log::warn!("Download attempt {attempt} for {dest} failed: {e}. Retrying...");
                    tokio::time::sleep(http::retry_delay(attempt, RETRY_DELAY_BASE_MS)).await;
                }
            }
        }
    }

    fn resource_url(&self, resource: &Resource, base_url: &str, dest: &str) -> String {
        if let Some(url) = resource.url() {
            return url.to_string();
        }
        match self.mirrors.lock().active.as_deref() {
            Some(active) => combine_url(active, dest),
            None => combine_url(base_url, dest),
        }
    }

    fn clear_host_outage(&self, url: &str) {
        self.host_down_since.clear(&url_host(url));
    }

    fn switch_mirror(&self, resource: &Resource, failed_url: &str) -> bool {
        if resource.url().is_some() {
            return false;
        }
        let failed_host = url_host(failed_url);
        let mut mirrors = self.mirrors.lock();
        if mirrors
            .active
            .as_deref()
            .is_some_and(|active| url_host(active) != failed_host)
        {
            return true;
        }
        mirrors.tried_hosts.insert(failed_host.clone());
        let next = mirrors
            .bases
            .iter()
            .find(|base| !mirrors.tried_hosts.contains(&url_host(base)))
            .cloned();
        let Some(base) = next else {
            return false;
        };
        log::warn!(
            "{failed_host} is unreachable, switching the download to {}.",
            url_host(&base)
        );
        mirrors.active = Some(base);
        drop(mirrors);
        self.send_progress(Phase::Downloading, &self.downloading_status(), json!({}));
        true
    }

    async fn wait_for_network(&self, url: &str) -> Result<(), String> {
        let waited = self
            .outage
            .single_flight(|| async {
                self.set_offline_power(true);
                let outcome = self.wait_for_network_inner(url).await;
                self.set_offline_power(false);
                outcome
            })
            .await;
        match waited {
            Some(outcome) => outcome,
            None if self.is_cancelled() => Err("Download aborted by user.".to_string()),
            None => Ok(()),
        }
    }

    async fn wait_for_network_inner(&self, url: &str) -> Result<(), String> {
        log::warn!("Download paused, waiting for the internet connection to return.");
        http::note_unreachable();
        self.send_progress(
            Update::new(Phase::Downloading).waiting_network(true),
            status::WAITING_NETWORK,
            json!({
                "speed": 0,
                "subStatus": sophon::STATUS_OFFLINE
            }),
        );
        let host = url_host(url);
        let mut rounds = 0u32;
        while !self.is_cancelled() {
            tokio::time::sleep(NETWORK_RECHECK_INTERVAL).await;
            if http::can_reach(url).await {
                log::info!("Internet connection restored, resuming download.");
                http::note_reachable();
                self.clear_host_outage(url);
                self.send_progress(Phase::Downloading, &self.downloading_status(), json!({}));
                return Ok(());
            }
            if http::reaches_home().await {
                let down_for = self
                    .host_down_since
                    .down_for(&host, std::time::Instant::now());
                if down_for >= HOST_OUTAGE_BUDGET {
                    log::warn!(
                        "{host} has been unreachable for {}s while the internet connection works.",
                        down_for.as_secs()
                    );
                    return Err(host_unreachable_message(&host));
                }
            } else {
                self.host_down_since.clear(&host);
            }
            rounds += 1;
            if rounds >= NETWORK_PROBE_ROUNDS {
                log::info!(
                    "The connection probe to {} still fails, so the download itself is tried again.",
                    url_host(url)
                );
                self.send_progress(Phase::Downloading, &self.downloading_status(), json!({}));
                return Ok(());
            }
        }
        Err("Download aborted by user.".to_string())
    }

    async fn stream_to_file(
        &self,
        url: &str,
        file_path: &Path,
        file_id: &str,
        expected_size: u64,
        expected_md5: &str,
    ) -> Result<bool, String> {
        let part_path = part_path_for(file_path);
        let had_sidecar = sidecar_path_for(file_path).exists();
        remove_sidecar(file_path);
        let wanted = PartIdentity::expected(url, expected_size, expected_md5);
        let keep_identity = records_identity(expected_size, expected_md5);
        let part_meta = std::fs::metadata(&part_path).ok();
        let recorded = part_meta.as_ref().and_then(|_| load_identity(file_path));
        let same_version = recorded.as_ref().is_none_or(|r| r.matches(&wanted));
        if !same_version && part_meta.is_some() {
            log::info!("{file_id}: the part on disk belongs to another version, starting over.");
        }
        let mut has_identity = recorded.is_some();
        let mut resume_from: u64 = match part_meta {
            Some(meta)
                if !had_sidecar
                    && same_version
                    && expected_size > 0
                    && meta.len() < expected_size =>
            {
                meta.len()
            }
            Some(meta)
                if !had_sidecar
                    && same_version
                    && expected_size > 0
                    && meta.len() == expected_size
                    && !expected_md5.is_empty() =>
            {
                let part = part_path.clone();
                let md5 = expected_md5.to_string();
                let mismatch = tauri::async_runtime::spawn_blocking(move || {
                    md5_mismatch(&part, expected_size, &md5)
                })
                .await
                .map_err(|e| e.to_string())??;
                if mismatch.is_none() {
                    log::info!("{file_id}: a complete verified download is already on disk, finishing it.");
                    self.tracker
                        .set_file_progress_absolute(file_id, expected_size as f64);
                    self.finalize_download(&part_path, file_path, file_id).await?;
                    if has_identity {
                        remove_identity(file_path);
                    }
                    return Ok(true);
                }
                discard_part(file_path);
                has_identity = false;
                0
            }
            Some(_) => {
                discard_part(file_path);
                has_identity = false;
                0
            }
            None => 0,
        };

        let mut request = http::download_client().get(url);
        if resume_from > 0 {
            request = request.header("Range", format!("bytes={resume_from}-"));
            if expected_md5.is_empty() {
                if let Some(validator) = recorded.as_ref().and_then(PartIdentity::if_range) {
                    request = request.header(reqwest::header::IF_RANGE, validator);
                }
            }
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("Request error: {}", http::describe(&e)))?;

        let status = response.status().as_u16();
        http::note_protocol(url, response.version());
        if status == 416 && resume_from > 0 {
            discard_part(file_path);
            return Err(format!(
                "The server could not continue {file_id} from where it stopped, so it starts over."
            ));
        }
        let fresh_identity = wanted.clone().with_validators(response.headers());
        let sink = if status == 206 && resume_from > 0 {
            let range_ok = response
                .headers()
                .get("Content-Range")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim_start().starts_with(&format!("bytes {resume_from}-")))
                .unwrap_or(false);
            if !range_ok {
                discard_part(file_path);
                return Err(format!(
                    "Server sent an unexpected Content-Range for {file_id}"
                ));
            }
            log::info!("Resuming {file_id} from {resume_from} bytes.");
            if !has_identity && keep_identity {
                persist_identity(file_path, &fresh_identity);
                has_identity = true;
            }
            tokio::fs::File::options()
                .append(true)
                .open(&part_path)
                .await
                .map_err(|e| format!("Write error: {e}"))?
        } else if status == 200 {
            if resume_from > 0 {
                log::info!("Server ignored Range for {file_id} — restarting the file.");
                resume_from = 0;
            }
            let sink = tokio::fs::File::create(&part_path)
                .await
                .map_err(|e| format!("Write error: {e}"))?;
            if keep_identity {
                persist_identity(file_path, &fresh_identity);
                has_identity = true;
            } else if has_identity {
                remove_identity(file_path);
                has_identity = false;
            }
            sink
        } else {
            return Err(format!("HTTP Error: {status} for URL {url}"));
        };
        let mut file = tokio::io::BufWriter::with_capacity(super::perf::WRITE_BUFFER_BYTES, sink);
        let mut hasher = (resume_from == 0 && !expected_md5.is_empty()).then(Md5::new);

        self.tracker
            .set_file_progress_absolute(file_id, resume_from as f64);
        if resume_from > 0 {
            self.tracker.reset_speed_baseline();
        }
        let mut stream = response.bytes_stream();
        let mut failure: Option<String> = None;

        loop {
            if self.is_cancelled() {
                failure = Some("Download aborted by user.".to_string());
                break;
            }
            if self.gate.is_paused() {
                file.flush()
                    .await
                    .map_err(|e| format!("Write error: {e}"))?;
                self.wait_while_paused().await;
                if self.is_cancelled() {
                    failure = Some("Download aborted by user.".to_string());
                    break;
                }
            }

            let chunk = match tokio::time::timeout(STALL_TIMEOUT, stream.next()).await {
                Err(_) => {
                    if self.gate.is_paused() {
                        continue;
                    }
                    log::warn!(
                        "Stream for {file_id} stalled for {}ms — tearing down to retry.",
                        STALL_TIMEOUT.as_millis()
                    );
                    failure = Some("Download stalled. The connection went idle.".to_string());
                    break;
                }
                Ok(None) => break,
                Ok(Some(Ok(chunk))) => chunk,
                Ok(Some(Err(e))) => {
                    failure = Some(format!("Stream error: {}", http::describe(&e)));
                    break;
                }
            };

            file.write_all(&chunk)
                .await
                .map_err(|e| format!("Write error: {e}"))?;
            if let Some(h) = hasher.as_mut() {
                h.update(&chunk);
            }
            self.tracker
                .update_file_progress(file_id, chunk.len() as f64, false);

            if !self.gate.is_paused() && self.tracker.should_update_ui() {
                self.send_progress(Phase::Downloading, &self.downloading_status(), json!({}));
            }
        }

        file.flush()
            .await
            .map_err(|e| format!("Write error: {e}"))?;
        let sink = file.into_inner();
        if let Some(e) = failure {
            drop(sink);
            return Err(e);
        }

        let verified_inline = match hasher {
            Some(h) => {
                let got = hex::encode(h.finalize());
                if !got.eq_ignore_ascii_case(expected_md5) {
                    drop(sink);
                    let bytes = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
                    log_checksum_mismatch(file_id, url, expected_md5, &got, bytes);
                    return Err("File validation failed after download.".to_string());
                }
                true
            }
            None => false,
        };

        if FSYNC_ON_FINALIZE {
            sink.sync_data()
                .await
                .map_err(|e| format!("Write error: {e}"))?;
        }
        drop(sink);
        self.finalize_download(&part_path, file_path, file_id).await?;
        if has_identity {
            remove_identity(file_path);
        }
        if !self.gate.is_paused() && self.tracker.should_update_ui() {
            self.send_progress(Phase::Downloading, &self.downloading_status(), json!({}));
        }
        Ok(verified_inline)
    }

    async fn download_file_segmented(
        self: &Arc<Self>,
        url: &str,
        file_path: &Path,
        file_id: &str,
        expected_size: u64,
        expected_md5: &str,
    ) -> Result<Option<bool>, String> {
        if expected_size < SEGMENT_TARGET_BYTES {
            return Ok(None);
        }
        let part_path = part_path_for(file_path);
        let sidecar_exists = sidecar_path_for(file_path).exists();
        let part_len = std::fs::metadata(&part_path).map_or(0, |m| m.len());
        if !sidecar_exists && part_len > 0 {
            log::info!("{file_id}: continuing the {part_len} bytes a single stream left on disk.");
            return Ok(None);
        }
        let loaded = load_sidecar(file_path, expected_size);
        let has_progress = loaded
            .as_ref()
            .is_some_and(|s| s.segments.iter().any(|seg| seg.done > 0));

        let mut probe_round = 0u32;
        let probe_headers = loop {
            probe_round += 1;
            let probe = http::download_client()
                .get(url)
                .header("Range", "bytes=0-0")
                .send()
                .await
                .map_err(|e| format!("Request error: {}", http::describe(&e)))?;
            let probe_status = probe.status().as_u16();
            http::note_protocol(url, probe.version());
            let headers = probe.headers().clone();
            drop(probe);
            match probe_status {
                206 => break headers,
                200 if has_progress && probe_round < RANGE_PROBE_ATTEMPTS => {
                    tokio::time::sleep(http::retry_delay(1, RETRY_DELAY_BASE_MS)).await;
                }
                200 => {
                    log::info!(
                        "{file_id}: the server did not honor a Range probe (HTTP 200) — using a single stream."
                    );
                    if sidecar_exists {
                        discard_part(file_path);
                    }
                    return Ok(None);
                }
                _ => return Err(format!("HTTP Error: {probe_status} for URL {url}")),
            }
        };
        let wanted =
            PartIdentity::expected(url, expected_size, expected_md5).with_validators(&probe_headers);

        let part_ready = part_len == expected_size;
        let sidecar = match loaded {
            Some(s)
                if part_ready
                    && s.identity.as_ref().is_none_or(|recorded| recorded.matches(&wanted)) =>
            {
                log::info!(
                    "{file_id}: resuming a segmented download with {} of {} bytes already on disk.",
                    s.segments.iter().map(|seg| seg.done).sum::<u64>(),
                    expected_size
                );
                s
            }
            loaded => {
                if loaded.is_some() && part_ready {
                    log::info!(
                        "{file_id}: the segments on disk belong to another version, starting over."
                    );
                }
                let segments: Vec<SegmentState> =
                    plan_segments(
                        expected_size,
                        SEGMENT_TARGET_BYTES,
                        segments_per_file(super::perf::DOWNLOAD_CONCURRENCY),
                    )
                        .into_iter()
                        .map(|(start, end)| SegmentState {
                            start,
                            end,
                            done: 0,
                        })
                        .collect();
                if segments.len() <= 1 {
                    return Ok(None);
                }
                let fresh = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&part_path)
                    .map_err(|e| format!("Write error: {e}"))?;
                if !set_sparse(&fresh, true) {
                    log::debug!("{file_id}: the drive has no sparse files, preallocating the part.");
                }
                fresh
                    .set_len(expected_size)
                    .map_err(|e| format!("Write error: {e}"))?;
                drop(fresh);
                remove_identity(file_path);
                let s = SegmentSidecar {
                    format: SIDECAR_FORMAT.to_string(),
                    size: expected_size,
                    segments,
                    identity: Some(wanted),
                };
                persist_sidecar(file_path, &s);
                log::info!(
                    "{file_id}: downloading {} segments in parallel ({expected_size} bytes).",
                    s.segments.len()
                );
                s
            }
        };

        let done_total: u64 = sidecar.segments.iter().map(|s| s.done).sum();
        self.tracker
            .set_file_progress_absolute(file_id, done_total as f64);
        if done_total > 0 {
            self.tracker.reset_speed_baseline();
        }

        let file = Arc::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&part_path)
                .map_err(|e| format!("Write error: {e}"))?,
        );
        let segment_count = sidecar.segments.len();
        let state = Arc::new(Mutex::new(sidecar));
        let last_persist = Arc::new(Mutex::new(std::time::Instant::now()));

        let mut workers = Vec::with_capacity(segment_count);
        for seg_idx in 0..segment_count {
            let me = Arc::clone(self);
            let url = url.to_string();
            let file_path = file_path.to_path_buf();
            let file_id = file_id.to_string();
            let file = Arc::clone(&file);
            let state = Arc::clone(&state);
            let last_persist = Arc::clone(&last_persist);
            workers.push(tauri::async_runtime::spawn(async move {
                me.run_segment(
                    seg_idx,
                    &url,
                    &file_path,
                    &file_id,
                    file,
                    state,
                    last_persist,
                )
                .await
            }));
        }

        let outcome = progress::join_workers(workers, "segment worker").await;
        persist_sidecar(file_path, &state.lock().clone());
        outcome?;

        set_sparse(&file, false);
        if FSYNC_ON_FINALIZE {
            let f = Arc::clone(&file);
            tauri::async_runtime::spawn_blocking(move || f.sync_data())
                .await
                .map_err(|e| format!("fsync task panicked: {e}"))?
                .map_err(|e| format!("Write error: {e}"))?;
        }
        drop(file);

        let verified = if !expected_md5.is_empty() {
            let part = part_path.clone();
            let md5 = expected_md5.to_string();
            let mismatch = tauri::async_runtime::spawn_blocking(move || {
                md5_mismatch(&part, expected_size, &md5)
            })
            .await
            .map_err(|e| e.to_string())??;
            if let Some(got) = mismatch {
                log_checksum_mismatch(file_id, url, expected_md5, &got, expected_size);
                return Err("File validation failed after download.".to_string());
            }
            true
        } else {
            false
        };

        self.finalize_download(&part_path, file_path, file_id).await?;
        remove_sidecar(file_path);
        if !self.gate.is_paused() && self.tracker.should_update_ui() {
            self.send_progress(Phase::Downloading, &self.downloading_status(), json!({}));
        }
        Ok(Some(verified))
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_segment(
        self: &Arc<Self>,
        seg_idx: usize,
        url: &str,
        file_path: &Path,
        file_id: &str,
        file: Arc<std::fs::File>,
        state: Arc<Mutex<SegmentSidecar>>,
        last_persist: Arc<Mutex<std::time::Instant>>,
    ) -> Result<(), String> {
        let mut attempt = 0u32;
        loop {
            if self.is_cancelled() {
                return Err("Download aborted by user.".to_string());
            }
            if self.gate.is_paused() {
                self.wait_while_paused().await;
                continue;
            }

            let (start, end, done) = {
                let s = &state.lock().segments[seg_idx];
                (s.start, s.end, s.done)
            };
            if start + done >= end {
                return Ok(());
            }
            let from = start + done;
            let pause_epoch = self.gate.pause_epoch();

            let attempt_result = self
                .stream_segment_range(
                    seg_idx,
                    url,
                    file_path,
                    file_id,
                    from,
                    end,
                    &file,
                    &state,
                    &last_persist,
                )
                .await;

            match attempt_result {
                Ok(()) => {
                    self.clear_host_outage(url);
                    return Ok(());
                }
                Err(e) => {
                    if self.is_cancelled()
                        || super::fs_util::classify(&e) == super::fs_util::FailureKind::Cancelled
                    {
                        return Err(e);
                    }
                    let advanced = state.lock().segments[seg_idx]
                        .done
                        .saturating_sub(done);
                    if advanced >= PROGRESS_RESETS_RETRIES_BYTES {
                        self.clear_host_outage(url);
                    }
                    if matches!(
                        super::fs_util::classify(&e),
                        super::fs_util::FailureKind::DiskFull
                            | super::fs_util::FailureKind::AccessDenied
                            | super::fs_util::FailureKind::DeviceGone
                            | super::fs_util::FailureKind::FileTooLarge
                            | super::fs_util::FailureKind::ShareLost
                    ) || http::permanent_client_status(&e).is_some()
                    {
                        return Err(e);
                    }
                    if self.gate.pause_epoch() != pause_epoch {
                        continue;
                    }
                    if is_network_error(&e) && !http::can_reach(url).await {
                        self.wait_for_network(url).await?;
                        self.tracker.reset_speed_baseline();
                        continue;
                    }
                    if advanced >= PROGRESS_RESETS_RETRIES_BYTES {
                        attempt = 0;
                    }
                    attempt += 1;
                    if attempt >= MAX_RETRIES {
                        return Err(e);
                    }
                    log::warn!(
                        "Segment {seg_idx} of {file_id} failed (attempt {attempt}/{MAX_RETRIES}): {e} — retrying."
                    );
                    tokio::time::sleep(http::retry_delay(attempt, RETRY_DELAY_BASE_MS)).await;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn stream_segment_range(
        &self,
        seg_idx: usize,
        url: &str,
        file_path: &Path,
        file_id: &str,
        from: u64,
        end: u64,
        file: &Arc<std::fs::File>,
        state: &Arc<Mutex<SegmentSidecar>>,
        last_persist: &Arc<Mutex<std::time::Instant>>,
    ) -> Result<(), String> {
        let response = http::download_client()
            .get(url)
            .header("Range", format!("bytes={from}-{}", end - 1))
            .send()
            .await
            .map_err(|e| format!("Request error: {}", http::describe(&e)))?;
        if response.status().as_u16() != 206 {
            return Err(format!(
                "HTTP Error: {} for URL {url}",
                response.status().as_u16()
            ));
        }

        let mut stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::with_capacity(WRITE_BUFFER_BYTES);
        let mut write_offset = from;

        let commit =
            |buffer: &mut Vec<u8>,
             write_offset: &mut u64|
             -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
                let chunk = if buffer.is_empty() {
                    Vec::new()
                } else {
                    std::mem::replace(buffer, Vec::with_capacity(WRITE_BUFFER_BYTES))
                };
                let offset = *write_offset;
                *write_offset += chunk.len() as u64;
                let file = Arc::clone(file);
                let state = Arc::clone(state);
                let last_persist = Arc::clone(last_persist);
                let file_path = file_path.to_path_buf();
                let done_after = *write_offset;
                Box::pin(async move {
                    if chunk.is_empty() {
                        return Ok(());
                    }
                    tauri::async_runtime::spawn_blocking(move || {
                        super::fs_util::write_all_at(&file, &chunk, offset)
                    })
                    .await
                    .map_err(|e| format!("segment write task panicked: {e}"))?
                    .map_err(|e| super::fs_util::fmt_io("Write error", &e))?;
                    {
                        let mut s = state.lock();
                        let seg = &mut s.segments[seg_idx];
                        seg.done = done_after - seg.start;
                    }
                    let now = std::time::Instant::now();
                    let should_persist = {
                        let mut guard = last_persist.lock();
                        if now.duration_since(*guard) >= SIDECAR_PERSIST_INTERVAL {
                            *guard = now;
                            true
                        } else {
                            false
                        }
                    };
                    if should_persist {
                        let snapshot = state.lock().clone();
                        persist_sidecar(&file_path, &snapshot);
                    }
                    Ok(())
                })
            };

        loop {
            if self.is_cancelled() {
                commit(&mut buffer, &mut write_offset).await?;
                return Err("Download aborted by user.".to_string());
            }
            if self.gate.is_paused() {
                commit(&mut buffer, &mut write_offset).await?;
                self.wait_while_paused().await;
                if self.is_cancelled() {
                    return Err("Download aborted by user.".to_string());
                }
            }

            let chunk = match tokio::time::timeout(STALL_TIMEOUT, stream.next()).await {
                Err(_) => {
                    if self.gate.is_paused() {
                        continue;
                    }
                    commit(&mut buffer, &mut write_offset).await?;
                    return Err("Download stalled. The connection went idle.".to_string());
                }
                Ok(None) => break,
                Ok(Some(Ok(chunk))) => chunk,
                Ok(Some(Err(e))) => {
                    commit(&mut buffer, &mut write_offset).await?;
                    return Err(format!("Stream error: {}", http::describe(&e)));
                }
            };

            if write_offset + buffer.len() as u64 + chunk.len() as u64 > end {
                commit(&mut buffer, &mut write_offset).await?;
                return Err(format!(
                    "Server sent more bytes than requested for segment {seg_idx} of {file_id}"
                ));
            }
            if !buffer.is_empty() && buffer.len() + chunk.len() > WRITE_BUFFER_BYTES {
                commit(&mut buffer, &mut write_offset).await?;
            }
            buffer.extend_from_slice(&chunk);
            self.tracker
                .update_file_progress(file_id, chunk.len() as f64, false);
            if !self.gate.is_paused() && self.tracker.should_update_ui() {
                self.send_progress(Phase::Downloading, &self.downloading_status(), json!({}));
            }

            if buffer.len() >= WRITE_BUFFER_BYTES {
                commit(&mut buffer, &mut write_offset).await?;
            }
        }

        commit(&mut buffer, &mut write_offset).await?;
        if write_offset != end {
            return Err(format!(
                "Stream ended early for segment {seg_idx} of {file_id} ({write_offset} of {end} bytes)"
            ));
        }
        Ok(())
    }

    async fn complete_download(
        self: &Arc<Self>,
        install_path: &Path,
        version: &str,
        resources: &[Resource],
    ) -> Result<Value, String> {
        log::info!(
            "Game download completed successfully ({}).",
            self.profile_id()
        );
        let manifest = if self.is_hypergryph() {
            self.harvest_reconcile_manifest(install_path, version, resources)
                .await
        } else {
            None
        };
        let packages = if self.uses_split_archives() {
            self.extract_split_archives(install_path, resources).await?
        } else {
            Vec::new()
        };
        if self.is_gf2() {
            self.gf2_finish_install(install_path, resources).await?;
        }
        let unpacked = !packages.is_empty();
        if self.is_cancelled() {
            if !unpacked {
                return Err("Download aborted by user.".to_string());
            }
            log::info!(
                "Cancel arrived after the packages were unpacked, so the install is finished instead."
            );
        }

        let dir = install_path.to_path_buf();
        let dests: Vec<String> = resources.iter().map(|r| r.dest().to_string()).collect();
        if let Err(e) =
            tauri::async_runtime::spawn_blocking(move || remove_part_files(&dir, &dests)).await
        {
            log::warn!("Leftover .part cleanup did not finish: {e}");
        }

        if !unpacked && self.is_cancelled() {
            return Err("Download aborted by user.".to_string());
        }
        if let Some(manifest) = manifest {
            let dir = install_path.to_path_buf();
            let result = tauri::async_runtime::spawn_blocking(move || {
                reconcile::save_manifest(&dir, &manifest).map(|()| manifest.files.len())
            })
            .await;
            match result {
                Ok(Ok(count)) => {
                    log::info!("Endfield: reconcile manifest saved ({count} files tracked).")
                }
                Ok(Err(e)) => log::warn!("Endfield: could not save the reconcile manifest: {e}"),
                Err(e) => log::warn!("Endfield: manifest save task failed: {e}"),
            }
        }
        let bundle = self.kuro_bundle.lock().clone();
        write_game_config(install_path, version, kuro_app_id(self.profile()), bundle.as_ref())?;
        log::info!(
            "Game config for {} updated to version {version}",
            self.profile_id()
        );

        let mut version_recorded = true;
        if self.is_bluepoch() {
            if let Err(e) = bluepoch::record_version(install_path, version) {
                log::warn!("Could not write {}: {e}", bluepoch::VERSION_FILE);
                version_recorded = false;
            }
        }
        if version_recorded {
            remove_downloaded_packages(&packages);
        } else if unpacked {
            log::warn!(
                "Kept the {} downloaded package(s) so the next update can unpack them again.",
                packages.len()
            );
        }

        if self.is_hypergryph() {
            self.finish_completion(install_path).await;
            return Ok(ok_with(json!({
                "installPath": install_path.to_string_lossy(),
                "version": version,
            })));
        }
        let index_resources = resources.to_vec();
        if self.prunes_removed_resources() {
            let dir = install_path.to_path_buf();
            let next = index_resources.clone();
            let removed =
                tauri::async_runtime::spawn_blocking(move || prune_removed_resources(&dir, &next))
                    .await
                    .unwrap_or(0);
            if removed > 0 {
                log::info!("Removed {removed} file(s) this build no longer ships.");
            }
        }
        match write_local_index(install_path, &index_resources) {
            Ok(()) => {
                log::info!("Local game resources index saved for quick repair.");
                if self.prunes_removed_resources() {
                    let dir = install_path.to_path_buf();
                    let recorded = tauri::async_runtime::spawn_blocking(move || {
                        write_scan_record(
                            &dir,
                            index_resources.iter().map(|r| (r.dest(), r.size, r.md5())),
                        )
                    })
                    .await
                    .map_err(|e| format!("scan record task panicked: {e}"))
                    .and_then(|r| r);
                    if let Err(e) = recorded {
                        log::warn!("Could not save {SCAN_RECORD_FILE}: {e}");
                    }
                }
            }
            Err(e) => log::error!("Failed to save local resources index: {e}"),
        }

        self.finish_completion(install_path).await;
        Ok(ok_with(json!({
            "installPath": install_path.to_string_lossy(),
            "version": version,
        })))
    }

    async fn finish_completion(&self, install_path: &Path) {
        clear_install_marker(install_path);
        super::file_channels::clear_update_incomplete(&self.app, &self.profile_id());
        self.app
            .state::<BackendState>()
            .game
            .clear_update_cache(&self.profile_id());
        let hook = self.on_complete.lock().take();
        if let Some(hook) = hook {
            let path = install_path.to_path_buf();
            if let Err(e) = tauri::async_runtime::spawn_blocking(move || hook(path)).await {
                log::warn!("The install completion step for {} failed: {e}", self.profile_id());
            }
        }
        self.send_progress(Phase::Done, status::COMPLETED, json!({ "percentage": 100 }));
    }

    async fn record_nte_scan(&self, install_path: &Path, resources: Vec<nte::Resource>) {
        let dir = install_path.to_path_buf();
        let recorded = tauri::async_runtime::spawn_blocking(move || {
            write_scan_record(
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
                "Could not save {SCAN_RECORD_FILE} for {}: {e}",
                self.profile_id()
            );
        }
    }

    // ------------ Arknights: Endfield Downloader ------------
    // Endfield needs its packs unpacked after download, so this sizes that extra space and does the delta update against the last installed manifest when there is one.
    async fn hypergryph_unpacked_bytes(&self, resources: &[Resource]) -> Option<u64> {
        let packs = reconcile::packs_from_resources(resources);
        if packs.is_empty() || packs.len() != resources.len() {
            return None;
        }
        let cancelled = Arc::clone(self.gate.flag());
        let handle = tokio::runtime::Handle::current();
        let result = tauri::async_runtime::spawn_blocking(move || {
            reconcile::remote_totals(packs, cancelled, handle)
        })
        .await;
        match result {
            Ok(Ok((bytes, _))) if bytes > 0 => Some(bytes),
            Ok(Ok(_)) => None,
            Ok(Err(e)) => {
                log::warn!("Endfield: could not read the package index for the disk space check: {e}");
                None
            }
            Err(e) => {
                log::warn!("Endfield: package index task for the disk space check failed: {e}");
                None
            }
        }
    }

    async fn harvest_reconcile_manifest(
        &self,
        install_path: &Path,
        version: &str,
        resources: &[Resource],
    ) -> Option<reconcile::Manifest> {
        let mut parts: Vec<PathBuf> = resources
            .iter()
            .filter_map(|r| super::fs_util::safe_join(install_path, r.dest()).ok())
            .filter(|p| p.exists())
            .collect();
        parts.sort();
        if parts.is_empty() {
            return None;
        }
        if parts.len() != resources.len() {
            log::warn!(
                "Endfield: only {}/{} downloaded parts on disk — skipping manifest harvest.",
                parts.len(),
                resources.len()
            );
            return None;
        }
        let packs = reconcile::packs_from_resources(resources);
        let ver = version.to_string();
        let result = tauri::async_runtime::spawn_blocking(move || {
            reconcile::manifest_from_parts(&parts, packs, &ver)
        })
        .await;
        match result {
            Ok(Ok(manifest)) => Some(manifest),
            Ok(Err(e)) => {
                log::warn!("Endfield: manifest harvest failed: {e}");
                None
            }
            Err(e) => {
                log::warn!("Endfield: manifest harvest task failed: {e}");
                None
            }
        }
    }

    async fn hypergryph_delta_update(
        self: &Arc<Self>,
        install_path: &Path,
        config: &GameConfig,
    ) -> Result<Option<Value>, String> {
        let Some(old_manifest) = reconcile::load_manifest(install_path) else {
            return Ok(None);
        };
        if versions_semver_equal(&old_manifest.version, &config.version) {
            return Ok(None);
        }
        let new_packs = reconcile::packs_from_resources(&config.resources);
        if new_packs.len() != config.resources.len() {
            log::warn!("Endfield delta: pack metadata incomplete — using full download.");
            return Ok(None);
        }

        log::info!(
            "Endfield delta update {} -> {} starting.",
            old_manifest.version,
            config.version
        );
        let hooks = EngineHooks::transfer(Arc::clone(self));
        let path = install_path.to_path_buf();
        let version = config.version.clone();
        let cancelled = Arc::clone(self.gate.flag());
        let handle = tokio::runtime::Handle::current();
        let result = tauri::async_runtime::spawn_blocking(move || {
            reconcile::delta_update(
                &path,
                &old_manifest,
                new_packs,
                &version,
                cancelled,
                &hooks,
                handle,
            )
        })
        .await
        .map_err(|e| format!("delta update task panicked: {e}"))?;

        match result {
            Ok(stats) => {
                update_game_config_file(install_path, &config.version)?;
                log::info!(
                    "Endfield delta complete: fetched {} files ({:.2}GB downloaded, {:.2}GB unpacked), removed {} orphans, {} files tracked.",
                    stats.fetched_files,
                    progress::gib(stats.downloaded_bytes as f64),
                    progress::gib(stats.fetched_bytes as f64),
                    stats.deleted_files,
                    stats.total_files
                );
                self.finish_completion(install_path).await;
                Ok(Some(ok_with(json!({
                    "installPath": install_path.to_string_lossy(),
                    "version": config.version,
                }))))
            }
            Err(e) => {
                if self.is_cancelled() {
                    return Err("Download was cancelled by the user.".to_string());
                }
                if is_disk_full_error(&e) {
                    log::warn!("Endfield delta update ran out of disk space: {e}");
                    return Err(e);
                }
                if !reconcile::is_structural_failure(&e) {
                    log::warn!("Endfield delta update failed, files fetched so far are kept: {e}");
                    return Err(format!(
                        "The update stopped before it finished: {e}. Press Update again to continue where it left off."
                    ));
                }
                log::warn!("Endfield delta update cannot run ({e}), switching to a full download.");
                self.tracker.reset();
                self.send_progress(Phase::Scanning, "Switching to a full download...", json!({}));
                Ok(None)
            }
        }
    }

    // ------------ Girls' Frontline 2 Downloader ------------
    // Girls' Frontline 2 adds its client archive to the file list and unpacks it once everything has landed.
    async fn gf2_client_resource(&self, install_path: &Path) -> Result<Option<Resource>, String> {
        let profile = self.profile();
        let client = gf2::fetch_client_info(profile).await?;

        if gf2::client_is_current(
            install_path,
            game_profiles::executable_name(profile),
            &client.version,
        ) {
            log::info!(
                "GF2 client {} already installed — only resources need checking.",
                client.version
            );
            return Ok(None);
        }

        resolve_7z_binary(&self.app).await?;
        let size = http::content_length(&client.url)
            .await
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                log::warn!(
                    "GF2 client {}: {} gave no size for the package.",
                    client.version,
                    url_host(&client.url)
                );
                "Could not read the size of the GF2 client package. Try again in a moment."
                    .to_string()
            })?;
        log::info!(
            "GF2 client {} queued with the resources ({size} bytes).",
            client.version
        );
        Ok(Some(
            Resource::new(client.file.clone(), size, String::new())
                .with_url(Some(client.url.clone())),
        ))
    }

    async fn gf2_finish_install(
        &self,
        install_path: &Path,
        resources: &[Resource],
    ) -> Result<(), String> {
        if let Some(client) = resources.iter().find(|r| !gf2::is_bundle_resource(r)) {
            self.gf2_extract_client(install_path, client).await?;
        }

        let bundles: Vec<Resource> = resources
            .iter()
            .filter(|r| gf2::is_bundle_resource(r))
            .cloned()
            .collect();

        let recorded = bundles.len();
        let path = install_path.to_path_buf();
        let carried = tauri::async_runtime::spawn_blocking(move || {
            let (removed, bytes) = gf2::prune_stale_bundles(&path, &bundles);
            if removed > 0 {
                log::info!(
                    "GF2: removed {removed} superseded bundle(s) we installed, {:.2} GB reclaimed.",
                    bytes as f64 / 1e9
                );
            }
            gf2::write_local_manifest(&path, &bundles)
        })
        .await
        .map_err(|e| format!("GF2 bundle finish task panicked: {e}"))??;
        log::info!(
            "GF2: recorded {recorded} resources in the install's Version.txt for the game to read ({carried} line(s) the game added kept)."
        );
        Ok(())
    }

    async fn gf2_extract_client(
        &self,
        install_path: &Path,
        resource: &Resource,
    ) -> Result<(), String> {
        let file = resource.dest().to_string();
        let archive = super::fs_util::safe_join(install_path, &file)?;
        let url = resource.url().unwrap_or_default();
        let version = gf2::client_version_from_url(url)
            .ok_or_else(|| format!("Could not read the GF2 client version from {url}"))?
            .to_string();

        if !archive.is_file() {
            return Err(format!(
                "The GF2 client package {file} is missing after download. Try installing again."
            ));
        }

        let seven_zip = resolve_7z_binary(&self.app).await?;
        log::info!("Using extractor binary: {}", seven_zip.display());
        let message = format!("Unpacking {file}...");
        let _extracting = self.begin_extracting();
        self.note_install_write();
        self.send_progress(
            Phase::Extracting,
            "Unpacking game client...",
            json!({ "percentage": 0, "message": message }),
        );
        self.gate
            .cancellable(run_7z_extract(&seven_zip, &archive, install_path, |percent| {
                if percent >= 100.0 || self.tracker.should_update_ui() {
                    self.send_progress(
                        Phase::Extracting,
                        "Unpacking game client...",
                        json!({ "percentage": percent, "message": message }),
                    );
                }
            }))
            .await
            .map_err(|e| self.cancel_aware(e))
            .inspect_err(|e| {
                if !self.is_cancelled() && is_damaged_archive_error(e) {
                    let _ = std::fs::remove_file(&archive);
                    discard_part(&archive);
                    log::warn!("GF2: removed the damaged {file} so the next attempt downloads it again.");
                }
            })?;
        if self.is_cancelled() {
            return Err("Download aborted by user.".to_string());
        }

        match reconcile::manifest_from_local_zip(&archive, url, &version) {
            Ok(manifest) => match reconcile::save_manifest(install_path, &manifest) {
                Ok(()) => log::info!(
                    "GF2: recorded a client manifest with {} files so Full Repair can cover the client.",
                    manifest.files.len()
                ),
                Err(e) => log::warn!("GF2: could not save the client manifest: {e}"),
            },
            Err(e) => log::warn!("GF2: could not read the client archive for a manifest: {e}"),
        }

        let exe = game_profiles::executable_name(self.profile());
        let unpacked = install_path.join(exe).is_file();
        if unpacked {
            gf2::record_client_version(install_path, &version)?;
        }

        if let Err(e) = std::fs::remove_file(&archive) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("GF2: could not remove {file}: {e}");
            }
        }
        discard_part(&archive);

        if !unpacked {
            return Err(format!(
                "The GF2 client package unpacked without {exe}. The download may be incomplete. Try installing again."
            ));
        }
        log::info!("GF2 client {version} unpacked.");
        Ok(())
    }

    // ------------ HoYoverse Downloader ------------
    // Genshin, Honkai: Star Rail, Zenless Zone Zero and Honkai Impact 3rd all go through here, using HoYoverse's chunked Sophon system instead of the shared file-list path.
    async fn sophon_install(self: &Arc<Self>, install_path: &Path) -> Result<Value, String> {
        let profile = self.profile();
        let name = game_profiles::display_name(profile);
        let started = std::time::Instant::now();

        self.send_progress(Phase::Scanning, status::FETCHING_CONFIG, json!({}));
        let auth = sophon::fetch_branch_auth_for_profile(profile).await?;
        let build = sophon::fetch_build(&auth).await?;
        *self.current_patch_version.lock() = Some(build.tag.clone());

        let audio_language =
            game_profiles::resolve_audio_language(&self.app, profile, install_path, Some(&build))
                .await;
        let categories = sophon::install_categories(&build, &audio_language);
        if categories.is_empty() {
            return Err(format!("{name}: sophon build has no usable categories."));
        }

        let applied = sophon::load_applied(install_path);
        if sophon::can_short_circuit(applied.as_ref(), &build.tag, &audio_language) {
            let manifest = applied.clone().expect("checked by can_short_circuit");
            let dir = install_path.to_path_buf();
            let intact = tauri::async_runtime::spawn_blocking(move || {
                sophon::quick_verify_applied(&dir, &manifest)
            })
            .await
            .unwrap_or(false);
            if intact {
                log::info!(
                    "{name} is already on sophon {} and every recorded file is in place — nothing to do.",
                    build.tag
                );
                update_game_config_file(install_path, &build.tag)?;
                self.finish_completion(install_path).await;
                return Ok(ok_with(json!({
                    "installPath": install_path.to_string_lossy(),
                    "version": build.tag,
                })));
            }
            log::info!("{name}: the recorded install no longer matches the disk — rescanning.");
        }

        let total_files: u64 = categories.iter().map(|c| c.file_count).sum();
        self.send_progress(
            Phase::Scanning,
            &format!("Reading {name} file lists ({total_files} files)..."),
            json!({}),
        );
        let manifests = sophon::fetch_manifests(&categories).await?;

        let (scan_bytes, scan_files) = sophon::scan_totals(&manifests);
        self.tracker
            .begin("validating", scan_bytes as f64, scan_files, None);
        let scan_hooks = Arc::new(EngineHooks::scan(Arc::clone(self), String::new()));

        let prev_files = applied.as_ref().map(|a| Arc::new(a.file_map()));
        let scan_mode = if prev_files.is_some() { "Diff" } else { "Deep" };

        let mut plans = Vec::new();
        let mut total_bytes = 0u64;
        let mut unchanged = 0usize;
        for (i, (category, assets)) in categories.iter().zip(&manifests).enumerate() {
            let label = format!(
                "Checking existing files ({}, {} of {})...",
                sophon::category_label(&category.matching_field),
                i + 1,
                categories.len()
            );
            scan_hooks.set_scan_status(&label);
            self.send_progress(Phase::Scanning, &label, json!({}));
            let dir = install_path.to_path_buf();
            let hooks: Arc<dyn sophon::Hooks> = Arc::clone(&scan_hooks) as _;
            let assets = assets.clone();
            let prev = prev_files.clone();
            let plan = tauri::async_runtime::spawn_blocking(move || {
                let mode = match prev.as_deref() {
                    Some(map) => sophon::ScanMode::Diff { prev: map },
                    None => sophon::ScanMode::Deep,
                };
                sophon::plan_scan_parallel(
                    &dir,
                    &assets,
                    mode,
                    sophon::default_scan_workers(),
                    hooks.as_ref(),
                )
            })
            .await
            .map_err(|e| format!("sophon planning task panicked: {e}"))??;

            total_bytes += plan.total_bytes;
            unchanged += plan.unchanged_files;
            plans.push(((*category).clone(), plan));
        }

        log::info!(
            "{name} sophon {}: {} files to fetch ({:.2}GB), {unchanged} already current.",
            build.tag,
            plans.iter().map(|(_, p)| p.assets.len()).sum::<usize>(),
            progress::gib(total_bytes as f64)
        );

        if plans.iter().all(|(_, p)| p.assets.is_empty()) && unchanged > 0 {
            let removed = self
                .finalize_sophon_state(
                    install_path,
                    &build.tag,
                    &audio_language,
                    &categories,
                    &manifests,
                    applied.as_ref(),
                )
                .await;
            log_sophon_summary(&SophonSummary {
                name,
                tag: &build.tag,
                scan_mode,
                files: 0,
                bytes: 0,
                removed,
                started,
                transfer_secs: 0.0,
            });
            update_game_config_file(install_path, &build.tag)?;
            self.finish_completion(install_path).await;
            return Ok(ok_with(json!({
                "installPath": install_path.to_string_lossy(),
                "version": build.tag,
            })));
        }

        let growth_bytes: u64 = plans.iter().map(|(_, p)| p.growth_bytes()).sum();
        ensure_disk_space(install_path, growth_bytes, 1.0, HEADROOM_INSTALL)?;

        let file_sizes: HashMap<String, f64> = plans
            .iter()
            .flat_map(|(_, p)| p.assets.iter())
            .map(|a| (a.asset.name.clone(), a.bytes_to_fetch() as f64))
            .collect();

        self.tracker.begin(
            "downloading",
            total_bytes as f64,
            file_sizes.len(),
            Some(file_sizes),
        );

        let hooks: Arc<dyn sophon::Hooks> = Arc::new(EngineHooks::transfer(Arc::clone(self)));
        let transfer_started = std::time::Instant::now();
        sophon::apply_plans(
            install_path,
            &plans,
            super::perf::DOWNLOAD_CONCURRENCY,
            hooks,
            Arc::clone(self.gate.flag()),
            self.before_write(),
        )
        .await?;
        let transfer_secs = transfer_started.elapsed().as_secs_f64();

        let removed = self
            .finalize_sophon_state(
                install_path,
                &build.tag,
                &audio_language,
                &categories,
                &manifests,
                applied.as_ref(),
            )
            .await;
        log_sophon_summary(&SophonSummary {
            name,
            tag: &build.tag,
            scan_mode,
            files: plans.iter().map(|(_, p)| p.assets.len()).sum(),
            bytes: total_bytes,
            removed,
            started,
            transfer_secs,
        });
        update_game_config_file(install_path, &build.tag)?;
        self.finish_completion(install_path).await;
        Ok(ok_with(json!({
            "installPath": install_path.to_string_lossy(),
            "version": build.tag,
        })))
    }

    async fn finalize_sophon_state(
        &self,
        install_path: &Path,
        tag: &str,
        audio_language: &str,
        categories: &[&sophon::Category],
        manifests: &[Vec<Arc<sophon::Asset>>],
        prev: Option<&sophon::AppliedManifest>,
    ) -> usize {
        let dir = install_path.to_path_buf();
        let tag = tag.to_string();
        let languages = vec![audio_language.to_string()];
        let categories_owned: Vec<sophon::Category> =
            categories.iter().map(|c| (*c).clone()).collect();
        let manifests = manifests.to_vec();
        let prev = prev.cloned();
        let removed = tauri::async_runtime::spawn_blocking(move || {
            let category_refs: Vec<&sophon::Category> = categories_owned.iter().collect();
            sophon::finalize_applied_state(
                &dir,
                &tag,
                &languages,
                &category_refs,
                &manifests,
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
        removed
    }

    // ------------ Neverness to Everness Downloader ------------
    // Neverness to Everness has its own resource index and CDN bases, so it gets its own install routine.
    async fn nte_install(self: &Arc<Self>, install_path: &Path) -> Result<Value, String> {
        let profile = self.profile();
        let name = game_profiles::display_name(profile);

        self.send_progress(Phase::Scanning, status::FETCHING_CONFIG, json!({}));
        let config = nte::fetch_config(profile).await?;
        let bases = nte::res_bases(profile);
        *self.current_patch_version.lock() = Some(config.res_version.clone());

        self.send_progress(
            Phase::Scanning,
            &format!("Reading the {name} file list..."),
            json!({}),
        );
        let list = nte::fetch_reslist(profile, &config).await?;
        let wanted = game_profiles::content_tags(game_profiles::profile_id(profile));
        let mut resources: Vec<nte::Resource> = list.selected(wanted.as_deref()).cloned().collect();
        if resources.is_empty() {
            return Err(format!("{name}: resource list contained no base files."));
        }

        let launcher = nte::fetch_launcher(profile).await?;
        log::info!(
            "{name} launcher runtime {} adds {} files ({:.2}GB packed).",
            launcher.version,
            launcher.resources.len(),
            progress::gib(launcher.download_bytes() as f64)
        );
        resources.extend(launcher.resources);

        let dir = install_path.to_path_buf();
        let listed = resources.clone();
        let trusted = tauri::async_runtime::spawn_blocking(move || {
            trusted_by_scan_record(
                &dir,
                listed.iter().map(|r| (r.dest.as_str(), r.size, r.md5.as_str())),
            )
        })
        .await
        .map_err(|e| format!("scan record task panicked: {e}"))?;
        let scan_list: Vec<nte::Resource> = resources
            .iter()
            .filter(|r| !trusted.contains(&super::fs_util::manifest_key(&r.dest)))
            .cloned()
            .collect();
        let trusted_count = resources.len() - scan_list.len();

        let scan_bytes: u64 = scan_list.iter().map(|r| r.size).sum();
        self.tracker
            .begin("validating", scan_bytes as f64, scan_list.len(), None);
        let scan_hooks = Arc::new(EngineHooks::scan(
            Arc::clone(self),
            format!("Checking existing {name} files..."),
        ));
        self.send_progress(Phase::Scanning, &scan_hooks.scan_status(), json!({}));

        let dir = install_path.to_path_buf();
        let hooks: Arc<dyn nte::Hooks> = Arc::clone(&scan_hooks) as _;
        let scan_started = std::time::Instant::now();
        let mut plan = tauri::async_runtime::spawn_blocking(move || {
            nte::plan_scan_parallel(
                &dir,
                &scan_list,
                nte::ScanMode::Blocks,
                super::perf::validation_workers(),
                hooks.as_ref(),
            )
        })
        .await
        .map_err(|e| format!("nte planning task panicked: {e}"))??;
        plan.unchanged += trusted_count;

        log::info!(
            "{name} {} (list {}): {} files to fetch ({:.2}GB), {} already current ({trusted_count} unchanged since the last verified install, not hashed), scanned in {:.1}s.",
            config.res_version,
            list.version,
            plan.fetch.len(),
            progress::gib(plan.total_bytes as f64),
            plan.unchanged,
            scan_started.elapsed().as_secs_f64()
        );

        if plan.fetch.is_empty() {
            update_game_config_file(install_path, &config.res_version)?;
            self.record_nte_scan(install_path, resources).await;
            self.finish_completion(install_path).await;
            return Ok(ok_with(json!({
                "installPath": install_path.to_string_lossy(),
                "version": config.res_version,
            })));
        }

        let write_bytes: u64 = plan
            .fetch
            .iter()
            .map(|r| {
                let current = super::fs_util::safe_join(install_path, &r.dest)
                    .ok()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map_or(0, |m| m.len());
                r.size.saturating_sub(current)
            })
            .sum();
        ensure_disk_space(install_path, write_bytes, 1.0, HEADROOM_INSTALL)?;

        let file_sizes: HashMap<String, f64> = plan
            .fetch
            .iter()
            .map(|r| (r.dest.clone(), r.fetch_bytes() as f64))
            .collect();
        self.tracker.begin(
            "downloading",
            plan.total_bytes as f64,
            file_sizes.len(),
            Some(file_sizes),
        );

        clear_scan_record(install_path);
        let hooks: Arc<dyn nte::Hooks> = Arc::new(EngineHooks::transfer(Arc::clone(self)));
        let transfer_started = std::time::Instant::now();
        nte::apply(
            install_path,
            &plan,
            &bases,
            super::perf::DOWNLOAD_CONCURRENCY,
            hooks,
            Arc::clone(self.gate.flag()),
            self.before_write(),
        )
        .await?;
        let seconds = transfer_started.elapsed().as_secs_f64();
        log::info!(
            "{name}: transferred {:.2}GB in {seconds:.1}s ({:.1} MB/s).",
            progress::gib(plan.total_bytes as f64),
            plan.total_bytes as f64 / 1_000_000.0 / seconds.max(0.001)
        );

        update_game_config_file(install_path, &config.res_version)?;
        self.record_nte_scan(install_path, resources).await;
        self.finish_completion(install_path).await;
        Ok(ok_with(json!({
            "installPath": install_path.to_string_lossy(),
            "version": config.res_version,
        })))
    }

    // ------------ Brown Dust II Downloader ------------
    // Downloads Brown Dust II's single package, reuses a partial one if it is still good, then unpacks it and updates the manifest.
    async fn bd2_install(self: &Arc<Self>, install_path: &Path) -> Result<Value, String> {
        let profile = self.profile();
        let name = game_profiles::display_name(profile);

        self.send_progress(Phase::Scanning, status::FETCHING_CONFIG, json!({}));
        let package = bd2::fetch_package(profile).await?;
        *self.current_patch_version.lock() = Some(package.version.clone());

        log::info!(
            "{name} package {} — {:.2}GB to download, {:.2}GB installed.",
            package.version,
            progress::gib(package.download_bytes as f64),
            progress::gib(package.install_bytes as f64),
        );

        let archive = super::fs_util::safe_join(install_path, &package.file_name)?;

        let previous = bd2::load_manifest(install_path)
            .map(|m| m.files)
            .unwrap_or_default();
        let previous_bytes: u64 = previous.iter().map(|f| f.size).sum();
        let reusable =
            bd2::reusable_package(install_path, &package.file_name, package.download_bytes);
        let fetch_bytes = if reusable.is_some() {
            0
        } else {
            package.download_bytes
        };

        ensure_disk_space(
            install_path,
            fetch_bytes + package.install_bytes.saturating_sub(previous_bytes),
            1.0,
            HEADROOM_INSTALL,
        )?;

        if reusable.is_some() {
            log::info!(
                "{name}: {} is already complete on disk, so it is unpacked without downloading it again.",
                package.file_name
            );
        } else {
            let resource = Resource::new(package.file_name.clone(), package.download_bytes, "")
                .with_url(Some(package.url.clone()));

            self.tracker.reset();
            self.tracker
                .set_totals(package.download_bytes as f64, 1);
            self.tracker.set_file_sizes(HashMap::from([(
                package.file_name.clone(),
                package.download_bytes as f64,
            )]));

            self.execute_download(vec![resource], "", install_path).await?;
            if self.is_cancelled() {
                return Err("Download was cancelled by the user.".to_string());
            }
        }
        discard_part(&archive);

        let total_bytes = package.install_bytes as f64;
        let extraction = Arc::new(Mutex::new(ExtractionProgress::new(1, HashMap::new())));

        let _extracting = self.begin_extracting();
        self.note_install_write();
        self.tracker
            .begin("extracting", package.install_bytes as f64, 1, None);
        self.emit_extraction_progress(
            &extraction,
            &archive,
            0.0,
            total_bytes,
            &format!("Unpacking {name}..."),
        );

        let dir = install_path.to_path_buf();
        let archive_path = archive.clone();
        let expect_crc = package.tied_crc;
        let expect_bytes = package.install_bytes;
        let cancel = Arc::clone(self.gate.flag());
        let engine = Arc::clone(self);
        let progress_archive = archive.clone();
        let label = format!("Unpacking {name}...");
        let files = tauri::async_runtime::spawn_blocking(move || {
            bd2::extract_package(
                &archive_path,
                &dir,
                None,
                expect_crc,
                expect_bytes,
                &cancel,
                |done| {
                    let percent = if expect_bytes > 0 {
                        (done as f64 / expect_bytes as f64) * 100.0
                    } else {
                        0.0
                    };
                    engine.emit_extraction_progress(
                        &extraction,
                        &progress_archive,
                        percent,
                        total_bytes,
                        &label,
                    );
                },
            )
        })
        .await
        .map_err(|e| format!("{name} unpack task panicked: {e}"))?
        .inspect_err(|e| {
            if bd2::is_damaged_package_error(e) {
                match std::fs::remove_file(&archive) {
                    Ok(()) => log::warn!(
                        "{name}: removed the damaged {} so the next attempt downloads it again.",
                        package.file_name
                    ),
                    Err(err) => log::warn!(
                        "{name}: could not remove the damaged {} ({err}).",
                        archive.display()
                    ),
                }
            }
        })?;

        bd2::write_launcher_settings(install_path, &package.settings)?;

        let removed = bd2::prune_replaced(install_path, &previous, &files);
        if removed > 0 {
            log::info!("{name}: removed {removed} file(s) this build no longer ships.");
        }

        bd2::write_manifest(install_path, &package.version, &files)?;

        match std::fs::remove_file(&archive) {
            Ok(()) => log::info!(
                "{name}: removed the {} package now that it is unpacked.",
                package.file_name
            ),
            Err(e) => log::warn!(
                "{name}: could not remove {} after unpacking ({e}); it is safe to delete by hand.",
                archive.display()
            ),
        }
        let stale = bd2::remove_stale_packages(install_path, &package.version);
        if stale > 0 {
            log::info!("{name}: removed {stale} package(s) left by older builds.");
        }

        update_game_config_file(install_path, &package.version)?;
        self.finish_completion(install_path).await;
        Ok(ok_with(json!({
            "installPath": install_path.to_string_lossy(),
            "version": package.version,
        })))
    }

    fn handle_download_error(&self, error: &str) -> Value {
        let cancelled = self.is_cancelled();
        if cancelled {
            log::info!("Download cancelled ({}).", self.profile_id());
        } else {
            log::error!("Download failed ({}): {error}", self.profile_id());
        }
        if cancelled {
            self.send_progress(Phase::Cancelled, status::CANCELLED, json!({ "error": error }));
            return json!({ "success": false, "cancelled": true, "error": error });
        }
        let shown = user_facing_error(error).unwrap_or_else(|| error.to_string());
        self.send_progress(
            Phase::Error,
            status::ERROR,
            json!({ "error": shown, "detail": error }),
        );
        json!({ "success": false, "cancelled": false, "error": shown, "detail": error })
    }

    // ------------ Archive Extraction ------------
    // Unpacks archives that arrive in parts using 7-Zip (the bundled copy, or one on the system) and reports overall progress weighted by archive size.
    async fn extract_split_archives(
        &self,
        install_path: &Path,
        resources: &[Resource],
    ) -> Result<Vec<PathBuf>, String> {
        let dest_set: HashSet<String> = resources
            .iter()
            .map(|r| r.dest().to_string())
            .filter(|d| !d.is_empty())
            .collect();
        let disk_files: Vec<String> = std::fs::read_dir(install_path)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();

        let seven_zip = resolve_7z_binary(&self.app).await?;
        log::info!("Using extractor binary: {}", seven_zip.display());

        let is_first_part = |name: &str| {
            let lower = name.to_lowercase();
            lower.ends_with(".zip.001") || lower.ends_with(".7z.001")
        };

        let split_first_parts: Vec<PathBuf> = disk_files
            .iter()
            .filter(|f| is_first_part(f) && (dest_set.is_empty() || dest_set.contains(*f)))
            .map(|f| install_path.join(f))
            .collect();

        let archive_ext = |name: &str| {
            let lower = name.to_lowercase();
            lower.ends_with(".zip") || lower.ends_with(".7z")
        };
        let mut standalone: Vec<PathBuf> = Vec::new();
        for dest in &dest_set {
            if !archive_ext(dest) {
                continue;
            }
            let stem = dest.rsplit_once('.').map(|(s, _)| s).unwrap_or(dest);
            if dest_set.contains(&format!("{stem}.zip.001"))
                || dest_set.contains(&format!("{stem}.7z.001"))
            {
                continue;
            }
            let Ok(fp) = crate::backend::fs_util::safe_join(install_path, dest) else {
                continue;
            };
            if fp.exists() {
                standalone.push(fp);
            }
        }

        let mut archives = split_first_parts;
        archives.extend(standalone);

        if archives.is_empty() {
            log::warn!(
                "No archives (.zip / .zip.001 / .7z / .7z.001) found to extract for {}.",
                self.profile_id()
            );
            return Ok(Vec::new());
        }

        let _extracting = self.begin_extracting();
        self.note_install_write();
        self.send_progress(
            Phase::Extracting,
            "Extracting packages...",
            json!({ "message": "Downloaded packages complete. Extracting game files..." }),
        );

        let total_archive_bytes: f64 = resources.iter().map(|r| r.size as f64).sum();
        let extraction = Arc::new(Mutex::new(ExtractionProgress::new(
            archives.len(),
            archive_weights(&archives, resources),
        )));

        for archive in &archives {
            let base_name = archive
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            log::info!("Extracting package: {base_name}");
            let prior_percent = extraction
                .lock()
                .per_archive
                .get(archive)
                .copied()
                .unwrap_or(0.0);
            self.emit_extraction_progress(
                &extraction,
                archive,
                prior_percent,
                total_archive_bytes,
                &format!("Extracting {base_name}..."),
            );

            self.gate
                .cancellable(run_7z_extract(&seven_zip, archive, install_path, |percent| {
                    self.emit_extraction_progress(
                        &extraction,
                        archive,
                        percent,
                        total_archive_bytes,
                        &format!("Extracting {base_name}..."),
                    );
                }))
                .await
                .map_err(|e| self.cancel_aware(e))?;
            if self.is_cancelled() {
                return Err("Download aborted by user.".to_string());
            }

            self.emit_extraction_progress(
                &extraction,
                archive,
                100.0,
                total_archive_bytes,
                &format!("Extracted {base_name}"),
            );
        }

        Ok(downloaded_package_paths(install_path, resources))
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_extraction_progress(
        &self,
        extraction: &Arc<Mutex<ExtractionProgress>>,
        archive: &Path,
        archive_percent: f64,
        total_bytes: f64,
        phase_text: &str,
    ) {
        let (total_percent, speed, extracted, eta) = {
            let mut ex = extraction.lock();
            ex.per_archive
                .insert(archive.to_path_buf(), archive_percent.clamp(0.0, 100.0));
            let total_percent = ex.overall_percent();
            let extracted = total_bytes * (total_percent / 100.0);
            let now = std::time::Instant::now();
            let dt = now.duration_since(ex.last_tick_time).as_secs_f64();
            if dt >= 0.25 {
                let instant = ((extracted - ex.last_tick_bytes) / dt).max(0.0);
                ex.smoothed_speed = if ex.smoothed_speed > 0.0 {
                    ex.smoothed_speed * 0.6 + instant * 0.4
                } else {
                    instant
                };
                ex.last_tick_bytes = extracted;
                ex.last_tick_time = now;
            }
            let remaining = (total_bytes - extracted).max(0.0);
            let eta = if ex.smoothed_speed > 0.0 {
                remaining / ex.smoothed_speed
            } else {
                0.0
            };
            (total_percent, ex.smoothed_speed, extracted, eta)
        };

        if archive_percent < 100.0 && !self.tracker.should_update_ui() {
            return;
        }
        self.send_progress(
            Phase::Extracting,
            "Extracting packages...",
            json!({
                "percentage": total_percent,
                "speed": speed,
                "processedBytes": extracted,
                "totalBytes": total_bytes,
                "eta": eta,
                "message": phase_text,
            }),
        );
    }
}

pub async fn run_7z_extract(
    seven_zip: &Path,
    archive: &Path,
    install_path: &Path,
    mut on_percent: impl FnMut(f64),
) -> Result<(), String> {
    use tokio::io::AsyncReadExt;

    let mut cmd = tokio::process::Command::new(seven_zip);
    cmd.arg("x")
        .arg(archive)
        .arg("-y")
        .arg("-bsp1")
        .arg("-sccUTF-8")
        .arg(format!("-o{}", install_path.display()))
        .current_dir(install_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    cmd.creation_flags(0x0800_0000);

    let mut child = cmd.spawn().map_err(|e| format!("7z spawn failed: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("failed to capture 7z stdout")?;
    let mut stderr_pipe = child.stderr.take().ok_or("failed to capture 7z stderr")?;

    let stderr_task = tauri::async_runtime::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    });

    let percent_re = regex::Regex::new(r"(\d{1,3})%").expect("static regex");
    let mut tail = String::new();
    let mut buf = vec![0u8; 4096];
    loop {
        let n = stdout
            .read(&mut buf)
            .await
            .map_err(|e| format!("7z stdout read failed: {e}"))?;
        if n == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&buf[..n]);
        let combined = format!("{tail}{text}");
        if let Some(m) = percent_re
            .captures_iter(&combined)
            .last()
            .and_then(|c| c[1].parse::<f64>().ok())
        {
            on_percent(m);
        }
        tail = combined
            .chars()
            .rev()
            .take(8)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
    }

    let status = child
        .wait()
        .await
        .map_err(|e| format!("7z wait failed: {e}"))?;
    let stderr_text = stderr_task.await.unwrap_or_default();
    if !status.success() {
        let code = status.code().unwrap_or(-1);
        return Err(format!(
            "Process exited with code {code} ({}) while extracting {}: {}",
            seven_zip_exit_meaning(code),
            archive.display(),
            if stderr_text.trim().is_empty() {
                "unknown error"
            } else {
                stderr_text.trim()
            }
        ));
    }
    Ok(())
}

fn seven_zip_exit_meaning(code: i32) -> &'static str {
    match code {
        1 => "warning",
        2 => "fatal error",
        7 => "command line error",
        8 => "not enough memory",
        255 => "stopped by user",
        _ => "unexpected exit",
    }
}

struct ExtractionProgress {
    per_archive: HashMap<PathBuf, f64>,
    weights: HashMap<PathBuf, f64>,
    archive_count: usize,
    last_tick_bytes: f64,
    last_tick_time: std::time::Instant,
    smoothed_speed: f64,
}

impl ExtractionProgress {
    fn new(archive_count: usize, weights: HashMap<PathBuf, f64>) -> Self {
        Self {
            per_archive: HashMap::new(),
            weights,
            archive_count: archive_count.max(1),
            last_tick_bytes: 0.0,
            last_tick_time: std::time::Instant::now(),
            smoothed_speed: 0.0,
        }
    }

    fn overall_percent(&self) -> f64 {
        let weighted = self.weights.len() == self.archive_count
            && self.weights.values().all(|w| *w > 0.0);
        if !weighted {
            let sum: f64 = self.per_archive.values().sum();
            return (sum / self.archive_count as f64).min(100.0);
        }
        let total: f64 = self.weights.values().sum();
        let done: f64 = self
            .per_archive
            .iter()
            .map(|(archive, pct)| self.weights.get(archive).copied().unwrap_or(0.0) * pct)
            .sum();
        (done / total).min(100.0)
    }
}

fn archive_weights(archives: &[PathBuf], resources: &[Resource]) -> HashMap<PathBuf, f64> {
    let sizes: Vec<(String, u64)> = resources
        .iter()
        .filter_map(|r| {
            let name = Path::new(r.dest()).file_name()?.to_string_lossy().to_lowercase();
            Some((name, r.size))
        })
        .collect();
    archives
        .iter()
        .map(|archive| {
            let name = archive
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let weight: u64 = match name.strip_suffix(".001") {
                Some(stem) => sizes
                    .iter()
                    .filter(|(part, _)| {
                        part.strip_prefix(stem)
                            .and_then(|rest| rest.strip_prefix('.'))
                            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                    })
                    .map(|(_, size)| size)
                    .sum(),
                None => sizes
                    .iter()
                    .filter(|(part, _)| *part == name)
                    .map(|(_, size)| size)
                    .sum(),
            };
            (archive.clone(), weight as f64)
        })
        .collect()
}

pub async fn resolve_7z_binary(app: &AppHandle) -> Result<PathBuf, String> {
    resolve_7z(app, true).await
}

pub async fn resolve_bundled_7z_binary(app: &AppHandle) -> Result<PathBuf, String> {
    resolve_7z(app, false).await
}

async fn resolve_7z(app: &AppHandle, allow_system: bool) -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    if let Ok(env_path) = std::env::var("PEEBIFY_7Z_PATH") {
        if !env_path.is_empty() {
            return Ok(PathBuf::from(env_path));
        }
    }
    if let Ok(resource_dir) = app.path().resource_dir() {
        for candidate in [
            resource_dir.join("resources").join("7z.exe"),
            resource_dir.join("7z.exe"),
        ] {
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    if cfg!(debug_assertions) {
        let dev_resources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
        for candidate in [dev_resources.join("7z.exe")] {
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    if !allow_system {
        return Err(
            "The extractor bundled with Peebify is missing. Repair the launcher by reinstalling it, then try again."
                .to_string(),
        );
    }
    let mut cmd = tokio::process::Command::new("where");
    cmd.arg("7z")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.creation_flags(0x0800_0000);
    if let Ok(status) = cmd.status().await {
        if status.success() {
            log::warn!("Peebify's bundled extractor is missing, so 7-Zip from PATH is used.");
            return Ok(PathBuf::from("7z"));
        }
    }
    Err("No 7z extractor found. Reinstall Peebify to restore its bundled extractor, or install 7-Zip.".to_string())
}

enum HookMode {
    Scan { status: Mutex<String> },
    Transfer,
}

struct EngineHooks {
    mgr: Arc<GameDownloadManager>,
    mode: HookMode,
    check_status: Mutex<String>,
    checked: AtomicBool,
}

impl EngineHooks {
    fn scan(mgr: Arc<GameDownloadManager>, status: impl Into<String>) -> Self {
        Self {
            mgr,
            mode: HookMode::Scan {
                status: Mutex::new(status.into()),
            },
            check_status: Mutex::new(String::new()),
            checked: AtomicBool::new(false),
        }
    }

    fn transfer(mgr: Arc<GameDownloadManager>) -> Self {
        Self {
            mgr,
            mode: HookMode::Transfer,
            check_status: Mutex::new(String::new()),
            checked: AtomicBool::new(false),
        }
    }

    fn scan_status(&self) -> String {
        match &self.mode {
            HookMode::Scan { status } => status.lock().clone(),
            HookMode::Transfer => self.mgr.downloading_status(),
        }
    }

    fn set_scan_status(&self, label: &str) {
        if let HookMode::Scan { status } = &self.mode {
            *status.lock() = label.to_string();
        }
    }
}

impl progress::Control for EngineHooks {
    fn is_cancelled(&self) -> bool {
        self.mgr.is_cancelled()
    }

    fn gate(&self) -> Option<&progress::RunGate> {
        Some(&self.mgr.gate)
    }
}

impl progress::FileHooks for EngineHooks {
    fn event(&self, event: progress::FileEvent) {
        match (&self.mode, event) {
            (HookMode::Scan { status }, progress::FileEvent::Bytes { delta, .. }) => {
                self.mgr.tracker.update_validation_progress(delta as f64);
                if self.mgr.tracker.should_update_ui() {
                    self.mgr.send_progress(
                        Phase::Scanning,
                        &status.lock(),
                        json!({ "speed": 0, "eta": 0 }),
                    );
                }
            }
            (HookMode::Scan { .. }, progress::FileEvent::FileDone { .. }) => {}
            (HookMode::Transfer, progress::FileEvent::Bytes { path, delta }) => {
                self.mgr
                    .tracker
                    .update_file_progress(path, delta as f64, false);
                if self.mgr.tracker.should_update_ui() {
                    self.mgr.send_progress(
                        Phase::Downloading,
                        &self.mgr.downloading_status(),
                        json!({}),
                    );
                }
            }
            (HookMode::Transfer, progress::FileEvent::FileDone { path }) => {
                self.mgr.tracker.update_file_progress(path, 0.0, true);
            }
        }
    }

    fn status(&self, message: &str) {
        self.mgr.set_offline_power(!message.is_empty());
        if message.is_empty() {
            self.mgr.tracker.reset_speed_baseline();
            self.mgr.send_progress(
                Phase::Downloading,
                &self.mgr.downloading_status(),
                json!({}),
            );
        } else {
            self.mgr.send_progress(
                Update::new(Phase::Downloading).waiting_network(true),
                status::WAITING_NETWORK,
                json!({ "speed": 0, "subStatus": message }),
            );
        }
    }
}

impl reconcile::Hooks for EngineHooks {
    fn event(&self, event: reconcile::Event) {
        match event {
            reconcile::Event::Phase { message, name } => {
                let phase = match name {
                    "downloading" | "repairing" => Phase::Downloading,
                    _ => Phase::Scanning,
                };
                if name == "validating" {
                    self.check_status.lock().clone_from(&message);
                }
                if name == "downloading" && self.checked.swap(false, Ordering::SeqCst) {
                    self.mgr.tracker.reset();
                }
                self.mgr.send_progress(phase, &message, json!({}));
            }
            reconcile::Event::Totals { total_bytes, sizes } => {
                let total_files = sizes.len();
                self.mgr.tracker.begin(
                    "downloading",
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
                if done {
                    self.mgr.note_install_write();
                }
                self.mgr
                    .tracker
                    .update_file_progress(path, delta as f64, done);
                if self.mgr.tracker.should_update_ui() {
                    self.mgr.send_progress(
                        Phase::Downloading,
                        &self.mgr.downloading_status(),
                        json!({}),
                    );
                }
            }
        }
    }

    fn status(&self, message: &str) {
        progress::FileHooks::status(self, message);
    }

    fn checking(&self, event: reconcile::Event) {
        match event {
            reconcile::Event::Phase { .. } => {}
            reconcile::Event::Totals { total_bytes, sizes } => {
                self.checked.store(true, Ordering::SeqCst);
                let total_files = sizes.len();
                self.mgr.tracker.begin(
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
            reconcile::Event::Bytes { delta, .. } => {
                self.mgr.tracker.update_validation_progress(delta as f64);
                if self.mgr.tracker.should_update_ui() {
                    self.mgr.send_progress(
                        Phase::Scanning,
                        &self.check_status.lock(),
                        json!({ "speed": 0, "eta": 0 }),
                    );
                }
            }
        }
    }
}

// ------------ Install Folder Bookkeeping ------------
// Marks the install folder as ours, sweeps leftover part files, keeps the local file index and scan record, and writes the game's own config files.
pub(super) fn versions_semver_equal(a: &str, b: &str) -> bool {
    fn segment(part: &str) -> Result<i64, String> {
        part.parse::<i64>().map_err(|_| part.to_lowercase())
    }
    let p1: Vec<_> = a.trim().split('.').map(segment).collect();
    let p2: Vec<_> = b.trim().split('.').map(segment).collect();
    (0..p1.len().max(p2.len())).all(|i| {
        let zero = Ok(0i64);
        let s1 = p1.get(i).unwrap_or(&zero);
        let s2 = p2.get(i).unwrap_or(&zero);
        s1 == s2
    })
}

fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

pub(super) fn prepare_install_dir(install_path: &Path, game_id: &str) -> Result<(), String> {
    let owned_dir = !install_path.exists() || dir_is_empty(install_path);

    let profile = game_profiles::profile(game_id);
    if let Some(too_deep) = game_path::path_budget_error(install_path, profile) {
        if owned_dir {
            return Err(too_deep);
        }
        log::warn!(
            "{} is installed at {} ({} characters), which is deeper than the {} this game can work from. {too_deep}",
            game_profiles::display_name(profile),
            install_path.display(),
            game_path::root_len(install_path),
            game_path::max_install_root_len(profile),
        );
    }

    let not_writable = |e: &std::io::Error| {
        if super::fs_util::is_access_denied(e) {
            log::warn!("{} is not writable: {e}", install_path.display());
            super::fs_util::not_writable_message(install_path)
        } else {
            super::fs_util::fmt_io(
                &format!("Could not prepare {}", install_path.display()),
                e,
            )
        }
    };
    super::fs_util::probe_writable(install_path).map_err(|e| not_writable(&e))?;

    let marker = install_path.join(INSTALL_MARKER_FILE);
    if marker.exists() {
        let owner = read_install_marker(install_path)
            .and_then(|m| m.get("gameId").and_then(Value::as_str).map(str::to_string));
        if owner.as_deref() == Some(game_id) {
            return Ok(());
        }
        log::warn!(
            "Replacing the install marker in {} (it was for {}).",
            install_path.display(),
            owner.as_deref().unwrap_or("an unknown game")
        );
    }
    let payload = json!({
        "gameId": game_id,
        "ownedDir": owned_dir,
        "startedAt": chrono::Utc::now().to_rfc3339(),
    });
    let text = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    match std::fs::write(&marker, text) {
        Ok(()) => {}
        Err(e) if super::fs_util::is_access_denied(&e) => return Err(not_writable(&e)),
        Err(e) => log::warn!("Could not write install marker {}: {e}", marker.display()),
    }
    Ok(())
}

pub(super) fn clear_install_marker(install_path: &Path) {
    let marker = install_path.join(INSTALL_MARKER_FILE);
    if let Err(e) = std::fs::remove_file(&marker) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("Could not remove install marker {}: {e}", marker.display());
        }
    }
}

fn read_install_marker(install_path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(install_path.join(INSTALL_MARKER_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

pub(super) fn install_marker_owns_dir(install_path: &Path) -> Option<bool> {
    let marker = read_install_marker(install_path)?;
    Some(
        marker
            .get("ownedDir")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    )
}

fn marker_owns_dir_for(marker: &Value, game_id: &str) -> bool {
    marker.get("ownedDir").and_then(Value::as_bool).unwrap_or(false)
        && marker.get("gameId").and_then(Value::as_str) == Some(game_id)
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let is_dir = entry
            .file_type()
            .map(|t| t.is_dir() && !t.is_symlink())
            .unwrap_or(false);
        if is_dir {
            walk_files(&entry.path(), out);
        } else {
            out.push(entry.path());
        }
    }
}

struct SophonSummary<'a> {
    name: &'a str,
    tag: &'a str,
    scan_mode: &'a str,
    files: usize,
    bytes: u64,
    removed: usize,
    started: std::time::Instant,
    transfer_secs: f64,
}

fn sophon_summary_line(s: &SophonSummary, total_secs: f64) -> String {
    let rate = if s.transfer_secs > 0.0 {
        s.bytes as f64 / 1e6 / s.transfer_secs
    } else {
        0.0
    };
    format!(
        "{} sophon {} finished: {} scan, {} file(s) fetched ({:.2}GB), {} orphaned file(s) removed, {total_secs:.1}s in total, {rate:.1} MB/s while downloading.",
        s.name,
        s.tag,
        s.scan_mode,
        s.files,
        progress::gib(s.bytes as f64),
        s.removed
    )
}

fn log_sophon_summary(s: &SophonSummary) {
    log::info!("{}", sophon_summary_line(s, s.started.elapsed().as_secs_f64()));
}

const PART_ARTIFACT_SUFFIXES: &[&str] = &[
    ".part",
    ".part.segments.json",
    ".part.segments.json.tmp",
    ".part.id.json",
    ".part.id.json.tmp",
    ".peebify_tmp",
    ".peebify-tmp",
    ".peebify-part",
];

fn is_part_artifact(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    PART_ARTIFACT_SUFFIXES
        .iter()
        .any(|suffix| name.len() > suffix.len() && name.ends_with(suffix))
}

pub(super) fn sweep_orphaned_parts(
    install_path: &Path,
    min_age: std::time::Duration,
) -> (u64, u64) {
    if !install_path.is_dir() {
        return (0, 0);
    }

    let mut files = Vec::new();
    walk_files(install_path, &mut files);

    let mut removed_files = 0u64;
    let mut removed_bytes = 0u64;
    for file in files.iter().filter(|f| is_part_artifact(f)) {
        let Ok(meta) = std::fs::metadata(file) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let stale = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age >= min_age);
        if !stale {
            continue;
        }
        let size = meta.len();
        match std::fs::remove_file(file) {
            Ok(()) => {
                removed_files += 1;
                removed_bytes += size;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("Part sweep: could not remove {}: {e}", file.display()),
        }
    }

    if removed_files > 0 {
        log::info!(
            "Part sweep: removed {removed_files} abandoned part file(s), {:.2} GB from {}.",
            removed_bytes as f64 / 1e9,
            install_path.display()
        );
    }
    (removed_files, removed_bytes)
}

pub(super) fn discard_partial_install_for(install_path: &Path, game_id: &str) -> (u64, u64) {
    let Some(marker) = read_install_marker(install_path) else {
        log::info!(
            "Discard: no install marker at {}; leaving the folder untouched.",
            install_path.display()
        );
        return (0, 0);
    };
    let owned_dir = marker_owns_dir_for(&marker, game_id);

    let mut files = Vec::new();
    walk_files(install_path, &mut files);
    if !owned_dir {
        files.retain(|f| {
            is_part_artifact(f) || f.file_name().is_some_and(|n| n == INSTALL_MARKER_FILE)
        });
    }

    let mut removed_files = 0u64;
    let mut removed_bytes = 0u64;
    for file in &files {
        let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(file) {
            Ok(()) => {
                removed_files += 1;
                removed_bytes += size;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("Discard: could not remove {}: {e}", file.display()),
        }
    }

    if owned_dir {
        if let Err(e) = std::fs::remove_dir_all(install_path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("Discard: could not remove {}: {e}", install_path.display());
            }
        }
    }

    log::info!(
        "Discard: removed {removed_files} file(s), {:.2} GB from {} (owned={owned_dir}).",
        removed_bytes as f64 / 1e9,
        install_path.display()
    );
    (removed_files, removed_bytes)
}

pub(super) fn read_index_file(path: &Path) -> Vec<Resource> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    data.get("resource")
        .or_else(|| data.get("resources"))
        .and_then(|v| v.as_array())
        .map(|list| list.iter().map(Resource::from_json).collect())
        .unwrap_or_default()
}

pub(super) fn read_local_index(install_path: &Path) -> Vec<Resource> {
    read_index_file(&install_path.join(LOCAL_INDEX_FILE))
}

pub(super) fn write_local_index(install_path: &Path, resources: &[Resource]) -> Result<(), String> {
    if resources.is_empty() {
        return Err(format!(
            "an empty file list was not written over {LOCAL_INDEX_FILE}"
        ));
    }
    let index_json: Vec<Value> = resources.iter().map(Resource::to_json).collect();
    let text = serde_json::to_string_pretty(&json!({ "resource": index_json }))
        .map_err(|e| e.to_string())?;
    super::fs_util::write_atomic(&install_path.join(LOCAL_INDEX_FILE), text.as_bytes())
}

const SCAN_RECORD_FILE: &str = "peebify-scan-record.json";
const SCAN_RECORD_FORMAT: &str = "peebify-scan-record-v1";

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct ScanRecordEntry {
    dest: String,
    size: u64,
    md5: String,
    mtime: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ScanRecord {
    format: String,
    files: Vec<ScanRecordEntry>,
}

fn modified_nanos(meta: &std::fs::Metadata) -> Option<u64> {
    let since = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    u64::try_from(since.as_nanos()).ok()
}

fn on_disk_stamp(install_path: &Path, dest: &str) -> Option<(u64, u64)> {
    let path = super::fs_util::safe_join(install_path, dest).ok()?;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some((meta.len(), modified_nanos(&meta)?))
}

fn record_trusts(
    entry: Option<&ScanRecordEntry>,
    size: u64,
    md5: &str,
    on_disk: Option<(u64, u64)>,
) -> bool {
    let (Some(entry), Some((len, mtime))) = (entry, on_disk) else {
        return false;
    };
    !md5.is_empty()
        && entry.md5.eq_ignore_ascii_case(md5)
        && entry.size == size
        && len == size
        && mtime == entry.mtime
}

fn record_proves_stale(
    entry: Option<&ScanRecordEntry>,
    size: u64,
    md5: &str,
    on_disk: Option<(u64, u64)>,
) -> bool {
    let (Some(entry), Some((len, mtime))) = (entry, on_disk) else {
        return false;
    };
    !md5.is_empty()
        && !entry.md5.is_empty()
        && !entry.md5.eq_ignore_ascii_case(md5)
        && entry.size == size
        && len == size
        && mtime == entry.mtime
}

#[derive(Debug, Default)]
struct ScanRecordVerdicts {
    trusted: HashSet<String>,
    stale: HashSet<String>,
}

pub(super) fn clear_scan_record(install_path: &Path) {
    match std::fs::remove_file(install_path.join(SCAN_RECORD_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("Could not remove {SCAN_RECORD_FILE}: {e}"),
    }
}

fn read_scan_record(install_path: &Path) -> HashMap<String, ScanRecordEntry> {
    let Ok(text) = std::fs::read_to_string(install_path.join(SCAN_RECORD_FILE)) else {
        return HashMap::new();
    };
    match serde_json::from_str::<ScanRecord>(&text) {
        Ok(record) if record.format == SCAN_RECORD_FORMAT => record
            .files
            .into_iter()
            .map(|entry| (entry.dest.clone(), entry))
            .collect(),
        _ => HashMap::new(),
    }
}

fn trusted_by_scan_record<'a>(
    install_path: &Path,
    files: impl IntoIterator<Item = (&'a str, u64, &'a str)>,
) -> HashSet<String> {
    scan_record_verdicts(install_path, files).trusted
}

fn scan_record_verdicts<'a>(
    install_path: &Path,
    files: impl IntoIterator<Item = (&'a str, u64, &'a str)>,
) -> ScanRecordVerdicts {
    let mut verdicts = ScanRecordVerdicts::default();
    let record = read_scan_record(install_path);
    if record.is_empty() {
        return verdicts;
    }
    for (dest, size, md5) in files {
        let key = super::fs_util::manifest_key(dest);
        let entry = record.get(&key);
        let on_disk = on_disk_stamp(install_path, dest);
        if record_trusts(entry, size, md5, on_disk) {
            verdicts.trusted.insert(key);
        } else if record_proves_stale(entry, size, md5, on_disk) {
            verdicts.stale.insert(key);
        }
    }
    verdicts
}

pub(super) fn write_scan_record<'a>(
    install_path: &Path,
    files: impl IntoIterator<Item = (&'a str, u64, &'a str)>,
) -> Result<(), String> {
    let entries: Vec<ScanRecordEntry> = files
        .into_iter()
        .filter(|(dest, _, md5)| !dest.is_empty() && !md5.is_empty())
        .filter_map(|(dest, size, md5)| {
            let (len, mtime) = on_disk_stamp(install_path, dest)?;
            (len == size).then(|| ScanRecordEntry {
                dest: super::fs_util::manifest_key(dest),
                size,
                md5: md5.to_ascii_lowercase(),
                mtime,
            })
        })
        .collect();
    if entries.is_empty() {
        clear_scan_record(install_path);
        return Ok(());
    }
    let text = serde_json::to_string(&ScanRecord {
        format: SCAN_RECORD_FORMAT.to_string(),
        files: entries,
    })
    .map_err(|e| e.to_string())?;
    super::fs_util::write_atomic(&install_path.join(SCAN_RECORD_FILE), text.as_bytes())
}

pub(super) fn prune_removed_resources(install_path: &Path, next: &[Resource]) -> usize {
    let previous = read_local_index(install_path);
    if previous.is_empty() || next.is_empty() {
        return 0;
    }
    let keep: HashSet<String> = next
        .iter()
        .map(|r| crate::backend::fs_util::manifest_key(r.dest()))
        .collect();

    let mut removed = 0usize;
    for resource in previous
        .iter()
        .filter(|r| !keep.contains(&crate::backend::fs_util::manifest_key(r.dest())))
    {
        let dest = resource.dest();
        if dest.is_empty() {
            continue;
        }
        let Ok(path) = crate::backend::fs_util::safe_join(install_path, dest) else {
            continue;
        };
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if meta.len() != resource.size {
            log::info!("Keeping {dest} — its size no longer matches the recorded install.");
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                removed += 1;
                log::info!("Removed {dest} — it is no longer part of the game's file list.");
                let mut parent = path.parent();
                while let Some(dir) = parent {
                    if dir == install_path || std::fs::remove_dir(dir).is_err() {
                        break;
                    }
                    parent = dir.parent();
                }
            }
            Err(e) => log::warn!("Could not remove {dest}: {e}"),
        }
    }
    removed
}

pub fn update_game_config_file(install_path: &Path, version: &str) -> Result<(), String> {
    write_game_config(install_path, version, None, None)
}

pub(super) const PACK_RECORD_DIR: &str = "launcherDownloadConfig";

fn write_game_config(
    install_path: &Path,
    version: &str,
    app_id: Option<&str>,
    bundle: Option<&KuroBundle>,
) -> Result<(), String> {
    let version = version.trim();
    let bundles = match bundle {
        Some(bundle) => Some(json!({
            bundle.name.clone(): { "version": version, "state": "", "resourcePacks": bundle.packs }
        })),
        None => recorded_bundles(install_path, version),
    };
    let mut payload = match bundles {
        Some(bundles) => json!({ "version": version, "state": "", "bundles": bundles }),
        None => json!({ "version": version, "reUseVersion": "", "state": "" }),
    };
    if let Some(id) = app_id {
        payload["appId"] = json!(id);
    }
    let text = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    super::fs_util::write_atomic(&install_path.join(GAME_CONFIG_FILE), text.as_bytes())?;
    if let Some(bundle) = bundle {
        if let Err(e) = write_pack_records(install_path, &bundle.packs, version) {
            log::warn!("Could not record the installed resource packs: {e}");
        }
    }
    Ok(())
}

fn recorded_bundles(install_path: &Path, version: &str) -> Option<Value> {
    let text = std::fs::read_to_string(install_path.join(GAME_CONFIG_FILE)).ok()?;
    let mut bundles = serde_json::from_str::<Value>(&text).ok()?["bundles"].take();
    for bundle in bundles.as_object_mut()?.values_mut() {
        bundle["version"] = json!(version);
    }
    Some(bundles)
}

fn write_pack_records(install_path: &Path, packs: &[String], version: &str) -> Result<(), String> {
    let dir = install_path.join(PACK_RECORD_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for pack in packs {
        let record = json!({ "packName": pack, "version": version });
        super::fs_util::write_atomic(&dir.join(format!("{pack}.json")), record.to_string().as_bytes())?;
    }
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        let stale = path.extension().is_some_and(|ext| ext == "json")
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| !packs.iter().any(|p| p == stem));
        if stale {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

fn kuro_app_id(profile: &Value) -> Option<&str> {
    let url = profile.get("gameConfigUrl")?.as_str()?;
    url.split('/').find_map(|segment| {
        let (id, key) = segment.split_once('_')?;
        (!id.is_empty() && !key.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            .then_some(id)
    })
}

fn undersized_files(install_path: &Path, listed: Vec<(String, u64)>) -> HashSet<String> {
    listed
        .into_iter()
        .filter(|(dest, size)| {
            crate::backend::fs_util::safe_join(install_path, dest)
                .is_ok_and(|path| !validator::FileValidator::quick_validate(&path, *size))
        })
        .map(|(dest, _)| dest)
        .collect()
}

fn remove_part_files(install_path: &Path, dests: &[String]) {
    for dest in dests {
        if let Ok(path) = crate::backend::fs_util::safe_join(install_path, dest) {
            discard_part(&path);
        }
    }
}

fn downloaded_package_paths(install_path: &Path, resources: &[Resource]) -> Vec<PathBuf> {
    resources
        .iter()
        .filter_map(|r| Path::new(r.dest()).file_name())
        .map(|name| install_path.join(name))
        .collect()
}

fn remove_downloaded_packages(packages: &[PathBuf]) {
    if packages.is_empty() {
        return;
    }
    for fp in packages {
        if let Err(e) = std::fs::remove_file(fp) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("Failed to remove downloaded package {}: {e}", fp.display());
            }
        }
    }
    log::info!(
        "Package cleanup finished for {} manifest entr{}.",
        packages.len(),
        if packages.len() == 1 { "y" } else { "ies" }
    );
}

// ------------ Tests ------------
// Covers pruning, resume rules, scan records, part-file cleanup, error wording and disk space.
#[cfg(test)]
mod prune_tests {
    use super::*;
    use crate::backend::validator::Resource;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "peebify-prune-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            Self(base)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let p = crate::backend::fs_util::safe_join(&self.0, rel).unwrap();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, bytes).unwrap();
            p
        }
        fn record(&self, resources: &[Resource]) {
            write_local_index(&self.0, resources).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn res(dest: &str, size: u64) -> Resource {
        Resource::new(dest, size, "00000000000000000000000000000000")
    }

    #[test]
    fn sophon_summary_names_the_scan_mode_and_average_speed() {
        let line = sophon_summary_line(
            &SophonSummary {
                name: "Genshin Impact",
                tag: "7.0.0",
                scan_mode: "Diff",
                files: 12,
                bytes: 50_000_000,
                removed: 3,
                started: std::time::Instant::now(),
                transfer_secs: 10.0,
            },
            42.0,
        );
        assert!(line.contains("7.0.0 finished: Diff scan"), "{line}");
        assert!(line.contains("12 file(s) fetched"), "{line}");
        assert!(line.contains("3 orphaned file(s) removed"), "{line}");
        assert!(line.contains("42.0s in total, 5.0 MB/s while downloading"), "{line}");
    }

    #[test]
    fn discarding_in_a_foreign_folder_removes_part_sidecars_and_keeps_game_files() {
        let dir = TempDir::new("discard-foreign");
        dir.write(INSTALL_MARKER_FILE, br#"{"ownedDir": false}"#);
        let game_file = dir.write("Game/data.pak", b"live");
        let part = dir.write("Game/new.pak.part", b"partial");
        let sidecar = dir.write("Game/new.pak.part.segments.json", b"{}");

        let (removed, _) = discard_partial_install_for(dir.path(), "wuwa");

        assert_eq!(removed, 3);
        assert!(game_file.exists());
        assert!(!part.exists());
        assert!(!sidecar.exists());
        assert!(!dir.path().join(INSTALL_MARKER_FILE).exists());
    }

    #[test]
    fn another_games_owned_marker_does_not_let_a_discard_remove_the_folder() {
        let dir = TempDir::new("discard-other-game");
        dir.write(INSTALL_MARKER_FILE, br#"{"gameId": "hsr", "ownedDir": true}"#);
        let user_file = dir.write("ReShade/preset.ini", b"mine");
        let part = dir.write("Game/new.pak.part", b"partial");

        discard_partial_install_for(dir.path(), "wuwa");

        assert!(user_file.exists());
        assert!(!part.exists());
    }

    #[test]
    fn the_games_own_marker_lets_a_discard_remove_the_folder() {
        let dir = TempDir::new("discard-own-game");
        dir.write(INSTALL_MARKER_FILE, br#"{"gameId": "wuwa", "ownedDir": true}"#);
        dir.write("Game/data.pak", b"partial");

        discard_partial_install_for(dir.path(), "wuwa");

        assert!(!dir.path().exists());
    }

    #[test]
    fn preparing_over_another_games_marker_takes_it_over_without_owning_the_folder() {
        let dir = TempDir::new("prepare-other-game");
        dir.write(INSTALL_MARKER_FILE, br#"{"gameId": "hsr", "ownedDir": true}"#);

        prepare_install_dir(dir.path(), "wuwa").unwrap();

        let marker = read_install_marker(dir.path()).unwrap();
        assert_eq!(marker["gameId"], "wuwa");
        assert_eq!(marker["ownedDir"], false);
    }

    #[test]
    fn a_chunk_dropped_from_the_index_is_removed() {
        let dir = TempDir::new("dropped");
        let kept = dir.write("Client/Content/Paks/pakchunk0.pak", b"data");
        let dropped = dir.write("Client/Content/Paks/pakchunk73.pak", b"data");
        dir.record(&[
            res("Client/Content/Paks/pakchunk0.pak", 4),
            res("Client/Content/Paks/pakchunk73.pak", 4),
        ]);

        let removed =
            prune_removed_resources(dir.path(), &[res("Client/Content/Paks/pakchunk0.pak", 4)]);

        assert_eq!(removed, 1);
        assert!(!dropped.exists());
        assert!(kept.is_file());
    }

    #[test]
    fn a_file_we_never_recorded_is_never_touched() {
        let dir = TempDir::new("unrecorded");
        let stranger = dir.write("Client/Content/Paks/pakchunk9_old.pak", b"data");
        let locale = dir.write("Client/Binaries/Win64/locales/en-US.pak", b"data");
        dir.record(&[res("Client/Content/Paks/pakchunk0.pak", 4)]);
        dir.write("Client/Content/Paks/pakchunk0.pak", b"data");

        let removed =
            prune_removed_resources(dir.path(), &[res("Client/Content/Paks/pakchunk0.pak", 4)]);

        assert_eq!(removed, 0);
        assert!(stranger.is_file(), "an unrecorded pak was deleted");
        assert!(locale.is_file(), "a WebView locale pak was deleted");
    }

    #[test]
    fn a_file_whose_size_changed_since_we_wrote_it_is_kept() {
        let dir = TempDir::new("resized");
        let touched = dir.write("Client/Content/Paks/pakchunk73.pak", b"grown-since-install");
        dir.record(&[res("Client/Content/Paks/pakchunk73.pak", 4)]);

        let removed =
            prune_removed_resources(dir.path(), &[res("Client/Content/Paks/pakchunk0.pak", 4)]);

        assert_eq!(removed, 0);
        assert!(touched.is_file());
    }

    #[test]
    fn an_empty_new_index_prunes_nothing() {
        let dir = TempDir::new("empty-next");
        let p = dir.write("Client/Content/Paks/pakchunk0.pak", b"data");
        dir.record(&[res("Client/Content/Paks/pakchunk0.pak", 4)]);

        assert_eq!(prune_removed_resources(dir.path(), &[]), 0);
        assert!(p.is_file(), "an empty file list must not wipe the install");
    }

    #[test]
    fn no_recorded_index_means_no_pruning() {
        let dir = TempDir::new("no-record");
        let p = dir.write("Client/Content/Paks/pakchunk73.pak", b"data");

        let removed =
            prune_removed_resources(dir.path(), &[res("Client/Content/Paks/pakchunk0.pak", 4)]);

        assert_eq!(removed, 0);
        assert!(p.is_file());
    }

    #[test]
    fn a_file_renamed_only_by_case_is_kept() {
        let dir = TempDir::new("case-rename");
        let live = dir.write("Client/Content/Paks/Foo.pak", b"data");
        let dropped = dir.write("Client/Content/Paks/pakchunk73.pak", b"data");
        dir.record(&[
            res("Client/Content/Paks/Foo.pak", 4),
            res("Client/Content/Paks/pakchunk73.pak", 4),
        ]);

        let removed = prune_removed_resources(dir.path(), &[res("client/content/paks/foo.pak", 4)]);

        assert_eq!(removed, 1);
        assert!(live.is_file(), "a case only rename deleted the live file");
        assert!(!dropped.exists());
    }

    #[test]
    fn a_dest_that_only_changes_spelling_is_kept() {
        let dir = TempDir::new("respelled");
        let live = dir.write("Client/Content/Paks/pakchunk0.pak", b"data");
        dir.record(&[res(r"Client\Content\Paks\pakchunk0.pak", 4)]);

        let removed = prune_removed_resources(
            dir.path(),
            &[res(r"/client\Content/./Paks/PAKCHUNK0.pak", 4)],
        );

        assert_eq!(removed, 0);
        assert!(live.is_file());
    }

    #[test]
    fn an_empty_list_never_overwrites_the_local_index() {
        let dir = TempDir::new("empty-index");
        dir.record(&[res("Client/Content/Paks/pakchunk0.pak", 4)]);

        let written = write_local_index(dir.path(), &[]);

        assert!(written.is_err());
        assert_eq!(read_local_index(dir.path()).len(), 1);
    }

    #[test]
    fn only_the_live_channel_may_install() {
        assert!(unsupported_channel("default").is_none());
        assert!(unsupported_channel("predownload").is_some());
        assert!(unsupported_channel("").is_some());
    }

    #[test]
    fn kuro_indexes_are_read_under_either_key() {
        let entry = json!({ "dest": "a.pak", "size": 4, "md5": "x" });

        assert_eq!(kuro_index_resources(&json!({ "resource": [entry] })).len(), 1);
        assert_eq!(kuro_index_resources(&json!({ "resources": [entry] })).len(), 1);
        assert!(kuro_index_resources(&json!({ "resource": [] })).is_empty());
        assert!(kuro_index_resources(&json!({})).is_empty());
    }

    #[test]
    fn a_checksum_mismatch_reports_what_was_found() {
        let dir = TempDir::new("md5-mismatch");
        let file = dir.write("a.pak", b"data");

        let good = md5_mismatch(&file, 4, "8D777F385D3DFEC8815D20F7496026DC").unwrap();
        let bad = md5_mismatch(&file, 4, "00000000000000000000000000000000").unwrap();
        let short = md5_mismatch(&file, 8, "8d777f385d3dfec8815d20f7496026dc").unwrap();
        let missing = md5_mismatch(&dir.path().join("b.pak"), 4, "x").unwrap();

        assert_eq!(good, None);
        assert_eq!(bad.as_deref(), Some("8d777f385d3dfec8815d20f7496026dc"));
        assert_eq!(short.as_deref(), Some("4 bytes instead of 8"));
        assert_eq!(missing.as_deref(), Some("missing"));
    }

    #[test]
    fn progress_on_disk_counts_segments_before_the_part_length() {
        let dir = TempDir::new("progress-on-disk");
        let file = dir.path().join("big.pak");
        std::fs::write(part_path_for(&file), vec![0u8; 64]).unwrap();

        let streamed = bytes_on_disk(&file, 64);
        persist_sidecar(
            &file,
            &SegmentSidecar {
                format: SIDECAR_FORMAT.to_string(),
                size: 64,
                segments: vec![
                    SegmentState { start: 0, end: 32, done: 10 },
                    SegmentState { start: 32, end: 64, done: 5 },
                ],
                identity: None,
            },
        );
        let segmented = bytes_on_disk(&file, 64);

        assert_eq!(streamed, 64);
        assert_eq!(segmented, 15);
        assert_eq!(bytes_on_disk(&dir.path().join("none.pak"), 64), 0);
    }

    const MD5_A: &str = "0cc175b9c0f1b6a831c399e269772661";
    const MD5_B: &str = "92eb5ffee6ae2fec3ad71c777531578f";

    #[test]
    fn scan_record_trust_rule_needs_every_field_to_match() {
        let entry = ScanRecordEntry {
            dest: "client/a.pak".to_string(),
            size: 10,
            md5: MD5_A.to_string(),
            mtime: 77,
        };
        assert!(record_trusts(Some(&entry), 10, MD5_A, Some((10, 77))));
        assert!(record_trusts(Some(&entry), 10, &MD5_A.to_uppercase(), Some((10, 77))));
        assert!(!record_trusts(None, 10, MD5_A, Some((10, 77))));
        assert!(!record_trusts(Some(&entry), 10, MD5_B, Some((10, 77))));
        assert!(!record_trusts(Some(&entry), 10, "", Some((10, 77))));
        assert!(!record_trusts(Some(&entry), 11, MD5_A, Some((11, 77))));
        assert!(!record_trusts(Some(&entry), 10, MD5_A, Some((9, 77))));
        assert!(!record_trusts(Some(&entry), 10, MD5_A, Some((10, 78))));
        assert!(!record_trusts(Some(&entry), 10, MD5_A, None));
    }

    #[test]
    fn scan_record_round_trip_trusts_only_untouched_files() {
        let dir = TempDir::new("scan-record");
        dir.write("Client/a.pak", b"aaaaaaaaaa");
        let b = dir.write("Client/b.pak", b"bbbbbbbbbb");
        dir.write("Client/c.pak", b"cccccccccc");
        dir.write("Client/d.pak", b"dddddddddd");
        let listed = [
            ("Client/a.pak", 10u64, MD5_A),
            ("Client/b.pak", 10, MD5_A),
            ("Client/c.pak", 10, MD5_A),
            ("Client/d.pak", 10, MD5_A),
            ("Client/missing.pak", 10, MD5_A),
        ];
        write_scan_record(dir.path(), listed.iter().copied()).unwrap();
        assert_eq!(read_scan_record(dir.path()).len(), 4);

        let later = std::fs::metadata(&b).unwrap().modified().unwrap()
            + std::time::Duration::from_secs(5);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&b)
            .unwrap()
            .set_modified(later)
            .unwrap();
        dir.write("Client/c.pak", b"ccccc");

        let next = [
            ("client\\A.pak", 10u64, MD5_A),
            ("Client/b.pak", 10, MD5_A),
            ("Client/c.pak", 10, MD5_A),
            ("Client/d.pak", 10, MD5_B),
            ("Client/missing.pak", 10, MD5_A),
            ("Client/new.pak", 10, MD5_A),
        ];
        let trusted = trusted_by_scan_record(dir.path(), next.iter().copied());
        assert_eq!(
            trusted,
            HashSet::from([crate::backend::fs_util::manifest_key("Client/a.pak")])
        );

        clear_scan_record(dir.path());
        assert!(trusted_by_scan_record(dir.path(), next.iter().copied()).is_empty());
    }

    #[test]
    fn scan_record_marks_untouched_files_with_a_new_checksum_stale() {
        let entry = ScanRecordEntry {
            dest: "a.pak".to_string(),
            size: 10,
            md5: MD5_A.to_string(),
            mtime: 77,
        };
        assert!(record_proves_stale(Some(&entry), 10, MD5_B, Some((10, 77))));
        assert!(!record_proves_stale(Some(&entry), 10, MD5_A, Some((10, 77))));
        assert!(!record_proves_stale(Some(&entry), 10, "", Some((10, 77))));
        assert!(!record_proves_stale(Some(&entry), 10, MD5_B, Some((10, 78))));
        assert!(!record_proves_stale(Some(&entry), 11, MD5_B, Some((11, 77))));
        assert!(!record_proves_stale(None, 10, MD5_B, Some((10, 77))));
        assert!(!record_proves_stale(Some(&entry), 10, MD5_B, None));

        let dir = TempDir::new("scan-record-stale");
        dir.write("a.pak", b"aaaaaaaaaa");
        dir.write("b.pak", b"bbbbbbbbbb");
        dir.write("c.pak", b"cccccccccc");
        write_scan_record(
            dir.path(),
            [("a.pak", 10u64, MD5_A), ("b.pak", 10, MD5_A), ("c.pak", 10, MD5_A)],
        )
        .unwrap();
        dir.write("c.pak", b"ccccc");
        let verdicts = scan_record_verdicts(
            dir.path(),
            [("a.pak", 10u64, MD5_A), ("b.pak", 10, MD5_B), ("c.pak", 10, MD5_B)],
        );
        let key = crate::backend::fs_util::manifest_key;
        assert_eq!(verdicts.trusted, HashSet::from([key("a.pak")]));
        assert_eq!(verdicts.stale, HashSet::from([key("b.pak")]));
    }

    #[test]
    fn scan_record_of_another_format_is_ignored() {
        let dir = TempDir::new("scan-record-format");
        dir.write("a.pak", b"aaaaaaaaaa");
        write_scan_record(dir.path(), [("a.pak", 10u64, MD5_A)]).unwrap();
        let text = std::fs::read_to_string(dir.path().join(SCAN_RECORD_FILE))
            .unwrap()
            .replace(SCAN_RECORD_FORMAT, "something-else");
        std::fs::write(dir.path().join(SCAN_RECORD_FILE), text).unwrap();
        assert!(trusted_by_scan_record(dir.path(), [("a.pak", 10u64, MD5_A)]).is_empty());
    }

    #[test]
    fn seven_zip_exit_codes_are_named() {
        assert_eq!(seven_zip_exit_meaning(2), "fatal error");
        assert_eq!(seven_zip_exit_meaning(8), "not enough memory");
        assert_eq!(seven_zip_exit_meaning(-1), "unexpected exit");
    }

    #[test]
    fn raw_transfer_errors_reach_the_user_as_plain_advice() {
        let http =
            user_facing_error("HTTP Error: 404 Not Found for URL https://cdn.example.com/a.pak")
                .unwrap();
        assert!(http.contains("HTTP 404"), "{http}");
        assert!(!http.contains("cdn.example.com"), "{http}");
        let network = user_facing_error("Stream error: error decoding response body").unwrap();
        assert!(network.contains("press Retry"), "{network}");
        assert_eq!(
            user_facing_error(
                "a.pak is in use and could not be replaced. Close PGR and press Update again. (Finalize error for a.pak: Could not replace a.pak. Is the game running? [os:32])"
            ),
            None
        );
        assert_eq!(user_facing_error("Validation failed: 2 file(s) are still corrupt."), None);
        for text in [http, network] {
            assert!(!text.contains(" \u{2014} ") && !text.contains(" \u{2013} ") && !text.contains(" - "));
        }
    }

    #[test]
    fn the_connection_setting_shapes_big_file_segments() {
        assert_eq!(segments_per_file(24), MAX_SEGMENTS_PER_FILE);
        assert_eq!(segments_per_file(64), MAX_SEGMENTS_PER_FILE);
        assert_eq!(segments_per_file(8), 2);
        assert_eq!(segments_per_file(1), 1);
        assert_eq!(segments_per_file(0), 1);
    }

    #[test]
    fn each_kuro_game_records_its_own_app_id() {
        let wuwa = json!({ "gameConfigUrl": "https://cdn.example.com/launcher/game/G153/50004_obOH/index.json" });
        let pgr = json!({ "gameConfigUrl": "https://cdn.example.com/launcher/game/G143/50015_LWdk/index.json" });
        assert_eq!(kuro_app_id(&wuwa), Some("50004"));
        assert_eq!(kuro_app_id(&pgr), Some("50015"));
        assert_eq!(kuro_app_id(&json!({ "installMode": "gf2" })), None);
        assert_eq!(kuro_app_id(&json!({ "gameConfigUrl": "https://x.example.com/a/index.json" })), None);
        let bundled = json!({ "gameConfigUrl": "https://cdn.example.com/launcher/game/50004_P7xc/G153/official/index.json" });
        assert_eq!(kuro_app_id(&bundled), Some("50004"));

        let dir = TempDir::new("game-config");
        write_game_config(dir.path(), " 2.4.0 ", kuro_app_id(&pgr), None).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(GAME_CONFIG_FILE)).unwrap())
                .unwrap();
        assert_eq!(written["version"], json!("2.4.0"));
        assert_eq!(written["appId"], json!("50015"));
        update_game_config_file(dir.path(), "3.0").unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(GAME_CONFIG_FILE)).unwrap())
                .unwrap();
        assert_eq!(written["version"], json!("3.0"));
        assert!(written.get("appId").is_none());
        assert!(!dir.path().join(format!("{GAME_CONFIG_FILE}.tmp")).exists());
    }

    fn bundle_config() -> Value {
        let pack = |name: &str, size: u64| {
            json!({
                "version": "3.7.0",
                "indexFile": format!("launcher/game/G153/50004/3.7.0/abc/{name}/indexFile.json"),
                "baseUrl": "launcher/game/G153/50004/3.7.0/abc/zip/",
                "size": size,
                "unCompressSize": size,
            })
        };
        json!({
            "cdnList": [{ "url": "https://cdn.example.com/" }],
            "resourcePacks": {
                "common": pack("common", 40), "sd": pack("sd", 22), "hd": pack("hd", 46), "uhd": pack("uhd", 66)
            },
            "bundles": {
                "SD": { "resourcePacks": ["common", "sd"] },
                "HD": { "resourcePacks": ["common", "hd"] },
                "UHD": { "resourcePacks": ["common", "uhd"] }
            }
        })
    }

    #[test]
    fn a_bundle_config_resolves_each_quality_to_its_packs() {
        let config = bundle_config();
        assert!(is_bundle_config(&config));
        assert!(!is_bundle_config(&json!({ "default": { "version": "3.7.0" } })));
        assert_eq!(bundle_packs(&config, &bundle_name("uhd")).unwrap(), ["common", "uhd"]);
        assert!(bundle_packs(&config, "8K").is_err());
        assert_eq!(bundle_config_version(&config).as_deref(), Some("3.7.0"));
        assert_eq!(bundle_bytes(&config, "SD"), Some((62, 62)));
        assert_eq!(bundle_bytes(&config, "UHD"), Some((106, 106)));
    }

    #[test]
    fn a_bundle_install_is_recorded_the_way_the_official_launcher_reads_it() {
        let dir = TempDir::new("bundle-config");
        let records = dir.path().join(PACK_RECORD_DIR);
        std::fs::create_dir_all(&records).unwrap();
        std::fs::write(records.join("hd.json"), "{}").unwrap();

        let sd = KuroBundle { name: "SD".into(), packs: vec!["common".into(), "sd".into()] };
        write_game_config(dir.path(), "3.7.0", Some("50004"), Some(&sd)).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(GAME_CONFIG_FILE)).unwrap())
                .unwrap();
        assert_eq!(written["appId"], json!("50004"));
        assert_eq!(written["bundles"]["SD"]["resourcePacks"], json!(["common", "sd"]));
        assert!(written.get("reUseVersion").is_none());
        assert!(records.join("common.json").is_file());
        assert!(records.join("sd.json").is_file());
        assert!(!records.join("hd.json").exists());

        update_game_config_file(dir.path(), "3.8.0").unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(GAME_CONFIG_FILE)).unwrap())
                .unwrap();
        assert_eq!(written["version"], json!("3.8.0"));
        assert_eq!(written["bundles"]["SD"]["version"], json!("3.8.0"));
    }

    #[test]
    fn the_installed_quality_comes_from_the_recorded_bundles() {
        let wuwa = game_profiles::profile("wuwa");
        let dir = TempDir::new("installed-quality");
        assert!(qualities_on_disk(wuwa, dir.path()).is_empty());
        assert_eq!(installed_quality(wuwa, dir.path(), Some("sd")), None);

        std::fs::write(dir.path().join(GAME_CONFIG_FILE), r#"{"version":"3.6.1"}"#).unwrap();
        assert_eq!(qualities_on_disk(wuwa, dir.path()), ["hd"]);

        std::fs::write(
            dir.path().join(GAME_CONFIG_FILE),
            r#"{"version":"3.7.0","bundles":{"HD":{},"UHD":{}}}"#,
        )
        .unwrap();
        assert_eq!(installed_quality(wuwa, dir.path(), Some("uhd")).as_deref(), Some("uhd"));
        assert_eq!(installed_quality(wuwa, dir.path(), Some("sd")).as_deref(), Some("hd"));
        assert!(qualities_on_disk(game_profiles::profile("pgr"), dir.path()).is_empty());
    }

    #[test]
    fn a_null_channel_version_is_empty_not_the_word_null() {
        assert_eq!(channel_version(&json!({ "version": null })), "");
        assert_eq!(channel_version(&json!({})), "");
        assert_eq!(channel_version(&json!({ "version": "2.5.1" })), "2.5.1");
        assert_eq!(channel_version(&json!({ "version": 3 })), "3");
        assert!(kuro_channel(&json!({ "default": null })).is_err());
        assert!(kuro_channel(&json!({ "predownload": {} })).is_err());
        assert!(kuro_channel(&json!({ "default": {} })).is_ok());
    }

    #[test]
    fn the_size_sweep_flags_short_and_missing_files_and_parts_are_cleared() {
        let dir = TempDir::new("size-sweep");
        dir.write("Client/ok.pak", b"aaaa");
        dir.write("Client/short.pak", b"aa");
        let part = dir.write("Client/ok.pak.part", b"a");
        let short = undersized_files(
            dir.path(),
            vec![
                ("Client/ok.pak".to_string(), 4),
                ("Client/short.pak".to_string(), 4),
                ("Client/missing.pak".to_string(), 4),
            ],
        );
        assert_eq!(
            short,
            HashSet::from(["Client/short.pak".to_string(), "Client/missing.pak".to_string()])
        );
        remove_part_files(dir.path(), &["Client/ok.pak".to_string()]);
        assert!(!part.exists());
        assert!(dir.path().join("Client/ok.pak").exists());
    }

    #[test]
    fn downloaded_packages_are_listed_and_removed_from_the_install_root() {
        let dir = TempDir::new("packages");
        let first = dir.write("game.zip.001", b"a");
        let second = dir.write("game.zip.002", b"b");
        let unpacked = dir.write("Game/data.bin", b"c");
        let packages = downloaded_package_paths(
            dir.path(),
            &[
                res("game.zip.001", 1),
                res("sub/game.zip.002", 1),
                res("", 0),
                res("gone.zip", 1),
            ],
        );
        assert_eq!(
            packages,
            vec![first.clone(), second.clone(), dir.path().join("gone.zip")]
        );
        remove_downloaded_packages(&packages);
        assert!(!first.exists());
        assert!(!second.exists());
        assert!(unpacked.exists());
    }

    #[test]
    fn finishing_a_file_clears_its_part_and_every_record_beside_it() {
        let dir = TempDir::new("part-records");
        let names = [
            "Client/a.pak.part",
            "Client/a.pak.part.segments.json",
            "Client/a.pak.part.segments.json.tmp",
            "Client/a.pak.part.id.json",
            "Client/a.pak.part.id.json.tmp",
        ];
        let written: Vec<PathBuf> = names.iter().map(|n| dir.write(n, b"x")).collect();
        let game_file = dir.write("Client/a.pak", b"data");

        remove_part_files(dir.path(), &["Client/a.pak".to_string()]);

        for path in &written {
            assert!(!path.exists(), "{} was left behind", path.display());
        }
        assert!(game_file.exists());
    }

    #[test]
    fn every_temp_suffix_counts_as_a_part_artifact() {
        for name in [
            "a.pak.part",
            "a.pak.part.segments.json",
            "a.pak.part.segments.json.tmp",
            "a.pak.part.id.json",
            "a.pak.part.id.json.tmp",
            "pakchunk1.pak.peebify_tmp",
            "pakchunk1.pak.peebify-tmp",
            "data.bin.4096.peebify-part",
        ] {
            assert!(is_part_artifact(Path::new(name)), "{name}");
        }
        for name in ["a.pak", "partial.pak", "a.json", "a.part.pak", "readme.tmp"] {
            assert!(!is_part_artifact(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn the_sweep_removes_old_repair_and_unpack_temps_only() {
        let dir = TempDir::new("temp-sweep");
        let repair = dir.write("Client/a.pak.peebify_tmp", b"x");
        let nte = dir.write("Client/b.pak.peebify-tmp", b"x");
        let reconcile = dir.write("Game/c.bin.512.peebify-part", b"x");
        let game_file = dir.write("Client/a.pak", b"x");

        assert_eq!(sweep_orphaned_parts(dir.path(), Duration::from_secs(3600)).0, 0);
        assert_eq!(sweep_orphaned_parts(dir.path(), Duration::ZERO).0, 3);

        assert!(!repair.exists() && !nte.exists() && !reconcile.exists());
        assert!(game_file.exists());
    }

    #[test]
    fn a_part_resumes_only_for_the_version_it_was_started_for() {
        let url = "https://cdn-a.example.com/game/1.2/Client/a.pak?sign=1";
        let recorded = PartIdentity::expected(url, 100, "ABCDEF");
        let same = PartIdentity::expected(
            "https://cdn-b.example.com/game/1.2/Client/a.pak?sign=2",
            100,
            "abcdef",
        );
        assert!(recorded.matches(&same), "a mirror or a new signature is the same file");
        assert!(!recorded.matches(&PartIdentity::expected(url, 100, "123456")));
        assert!(!recorded.matches(&PartIdentity::expected(url, 101, "ABCDEF")));

        let mut client = PartIdentity::expected("https://cdn.example.com/1.0/client.zip", 100, "");
        client.etag = Some("\"v1\"".to_string());
        let mut next = PartIdentity::expected("https://cdn.example.com/1.0/client.zip", 100, "");
        next.etag = Some("\"v2\"".to_string());
        assert!(!client.matches(&next));
        next.etag = None;
        assert!(client.matches(&next));
        assert!(!client.matches(&PartIdentity::expected(
            "https://cdn.example.com/1.1/client.zip",
            100,
            ""
        )));
        assert_eq!(client.if_range(), Some("\"v1\""));
    }

    #[test]
    fn only_strong_etags_are_kept_for_if_range() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::ETAG, "W/\"weak\"".parse().unwrap());
        headers.insert(
            reqwest::header::LAST_MODIFIED,
            "Tue, 01 Sep 2026 10:00:00 GMT".parse().unwrap(),
        );
        let identity = PartIdentity::default().with_validators(&headers);
        assert_eq!(identity.etag, None);
        assert_eq!(identity.if_range(), Some("Tue, 01 Sep 2026 10:00:00 GMT"));
    }

    #[test]
    fn an_identity_survives_a_round_trip_and_old_sidecars_still_load() {
        let dir = TempDir::new("identity");
        let file = dir.path().join("a.pak");
        let identity = PartIdentity::expected("https://cdn.example.com/a.pak", 64, "abc");
        persist_identity(&file, &identity);
        assert_eq!(load_identity(&file), Some(identity));
        remove_identity(&file);
        assert_eq!(load_identity(&file), None);

        std::fs::write(
            sidecar_path_for(&file),
            r#"{"format":"seg-v1","size":64,"segments":[{"start":0,"end":64,"done":8}]}"#,
        )
        .unwrap();
        let old = load_sidecar(&file, 64).expect("a sidecar without an identity still loads");
        assert_eq!(old.identity, None);
    }

    #[test]
    fn replaced_files_count_only_their_growth_and_the_parts_in_flight() {
        let gib = 1u64 << 30;
        let mib = 1u64 << 20;
        let paks: Vec<(u64, u64)> = (0..10).map(|_| (2 * gib, 2 * gib - 100 * mib)).collect();
        let needed = replacement_write_bytes(&paks, 24);
        let growth = 10 * 100 * mib;
        let in_flight = 3 * (2 * gib - 100 * mib);
        assert_eq!(needed, growth + in_flight);
        assert!(needed < 20 * gib);

        assert_eq!(replacement_write_bytes(&[(gib, 0), (10 * mib, 0)], 24), gib + 10 * mib);

        assert_eq!(replacement_write_bytes(&[(10 * mib, 50 * mib)], 24), 10 * mib);

        let mixed: Vec<(u64, u64)> = std::iter::once((gib, gib))
            .chain((0..40).map(|_| (mib, mib)))
            .collect();
        let slots = small_pool_workers(1, 24) as u64;
        assert_eq!(replacement_write_bytes(&mixed, 24), gib + slots * mib);
        assert_eq!(replacement_write_bytes(&[], 24), 0);

        let late: Vec<(u64, u64)> = (0..3)
            .map(|_| (SEGMENT_MIN_BYTES, SEGMENT_MIN_BYTES))
            .chain((0..40).map(|_| (200 * mib, 200 * mib)))
            .collect();
        assert_eq!(replacement_write_bytes(&late, 24), 24 * 200 * mib);
    }

    #[test]
    fn finished_big_workers_hand_their_connections_to_small_files() {
        let per_file = segments_per_file(24);
        let handoff = SlotHandoff::new(5, 24);
        assert_eq!(handoff.small_slots.available_permits(), 2);
        handoff.big_worker_done();
        assert_eq!(handoff.small_slots.available_permits(), 24 - 2 * per_file);
        handoff.big_worker_done();
        assert_eq!(handoff.small_slots.available_permits(), 24 - per_file);
        handoff.big_worker_done();
        assert_eq!(handoff.small_slots.available_permits(), 24);
        handoff.big_worker_done();
        assert_eq!(handoff.small_slots.available_permits(), 24);

        let single = SlotHandoff::new(1, 24);
        assert_eq!(single.small_slots.available_permits(), 24 - per_file);
        single.big_worker_done();
        assert_eq!(single.small_slots.available_permits(), 24);
    }

    #[test]
    fn a_sparse_part_keeps_segment_writes_in_place() {
        use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
        let dir = TempDir::new("sparse-part");
        let path = dir.path().join("big.pak.part");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let _ = set_sparse(&file, true);
        file.set_len(4096).unwrap();
        file.seek(SeekFrom::Start(3072)).unwrap();
        file.write_all(&[7u8; 1024]).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&[1u8; 3072]).unwrap();
        let _ = set_sparse(&file, false);
        drop(file);

        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes.len(), 4096);
        assert!(bytes[..3072].iter().all(|b| *b == 1));
        assert!(bytes[3072..].iter().all(|b| *b == 7));
    }

    #[test]
    fn a_big_archive_weighs_more_than_a_voice_pack() {
        let dir = PathBuf::from("C:/Games/Endfield");
        let game = dir.join("game.zip.001");
        let voice = dir.join("voice_en.zip");
        let resources = [
            res("game.zip.001", 20),
            res("game.zip.002", 20),
            res("voice_en.zip", 5),
            res("unrelated.zip.001x", 99),
        ];
        let weights = archive_weights(&[game.clone(), voice.clone()], &resources);
        assert_eq!(weights[&game], 40.0);
        assert_eq!(weights[&voice], 5.0);

        let mut ex = ExtractionProgress::new(2, weights);
        ex.per_archive.insert(game.clone(), 50.0);
        let half_of_game = ex.overall_percent();
        assert!((half_of_game - 20.0 / 45.0 * 100.0).abs() < 1e-9, "{half_of_game}");
        ex.per_archive.insert(game, 100.0);
        ex.per_archive.insert(voice, 100.0);
        assert!((ex.overall_percent() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn extraction_falls_back_to_equal_weights_when_a_size_is_unknown() {
        let a = PathBuf::from("a.zip");
        let b = PathBuf::from("b.zip");
        let weights = archive_weights(&[a.clone(), b.clone()], &[res("a.zip", 10)]);
        assert_eq!(weights[&b], 0.0);
        let mut ex = ExtractionProgress::new(2, weights);
        ex.per_archive.insert(a, 100.0);
        assert_eq!(ex.overall_percent(), 50.0);

        let mut single = ExtractionProgress::new(1, HashMap::new());
        single.per_archive.insert(PathBuf::from("pkg.zip"), 30.0);
        assert_eq!(single.overall_percent(), 30.0);
    }

    #[test]
    fn a_host_outage_is_reported_plainly_and_never_read_as_a_network_blip() {
        let message = host_unreachable_message("cdn.example.com");
        assert!(is_host_outage(&message));
        assert!(message.contains("cdn.example.com"), "{message}");
        assert!(!is_network_error(&message), "{message}");
        assert_eq!(user_facing_error(&message), None);
        assert_eq!(
            super::super::fs_util::classify(&message),
            super::super::fs_util::FailureKind::Other
        );
    }

    #[test]
    fn a_full_disk_is_recognized_from_the_os_and_from_the_space_check() {
        assert!(is_disk_full_error(
            "Not enough disk space in D:\\Games. This needs about 3.0 GB free but only 1.0 GB is available. Free up space and try again."
        ));
        assert!(is_disk_full_error("Write error: x (os error 112)"));
        assert!(!is_disk_full_error("range request failed: connection reset"));
    }

    #[test]
    fn a_refused_resume_is_retried_not_treated_as_permanent() {
        let message = "The server could not continue a.pak from where it stopped, so it starts over.";
        assert_eq!(http::permanent_client_status(message), None);
        assert_eq!(
            super::super::fs_util::classify(message),
            super::super::fs_util::FailureKind::Other
        );
        assert!(http::permanent_client_status("HTTP Error: 404 for URL https://cdn/a.pak").is_some());
        assert!(http::permanent_client_status("HTTP Error: 503 for URL https://cdn/a.pak").is_none());
    }

    #[test]
    fn damaged_archives_are_told_apart_from_other_unpack_failures() {
        assert!(is_damaged_archive_error(
            "Process exited with code 2 (fatal error) while extracting x.zip: ERROR: Data Error : Client/a.pak"
        ));
        assert!(is_damaged_archive_error("ERROR: x.zip Can not open the file as archive"));
        assert!(is_damaged_archive_error("Unexpected end of archive"));
        assert!(!is_damaged_archive_error(
            "Process exited with code 2 (fatal error) while extracting x.zip: There is not enough space on the disk."
        ));
        assert!(!is_damaged_archive_error("cancelled"));
    }

    #[test]
    fn every_kuro_cdn_is_kept_in_order_without_repeats() {
        let channel = json!({ "cdnList": [
            { "url": "https://a.example.com/" },
            { "url": "" },
            { "url": "https://b.example.com/" },
            { "url": "https://a.example.com/" },
            { "K": 1 },
        ]});
        assert_eq!(
            kuro_cdn_urls(&channel),
            vec!["https://a.example.com/".to_string(), "https://b.example.com/".to_string()]
        );
        assert!(kuro_cdn_urls(&json!({})).is_empty());
    }

    #[test]
    fn a_host_outage_after_data_flowed_gets_the_full_budget() {
        let outages = HostOutages::default();
        let start = std::time::Instant::now();
        let host = "cdn.example.com";
        assert_eq!(outages.down_for(host, start), Duration::ZERO);
        assert_eq!(outages.down_for(host, start + Duration::from_secs(30)), Duration::from_secs(30));
        assert_eq!(outages.down_for("other.example.com", start + Duration::from_secs(30)), Duration::ZERO);
        outages.clear(host);
        let later = start + HOST_OUTAGE_BUDGET + Duration::from_secs(60);
        assert_eq!(outages.down_for(host, later), Duration::ZERO);
        assert!(outages.down_for(host, later + NETWORK_RECHECK_INTERVAL) < HOST_OUTAGE_BUDGET);
        let stale = HostOutages::default();
        stale.down_for(host, start);
        assert!(stale.down_for(host, later) >= HOST_OUTAGE_BUDGET);
        outages.clear_all();
        assert_eq!(outages.down_for(host, later + HOST_OUTAGE_BUDGET), Duration::ZERO);
    }
}

#[cfg(test)]
mod disk_space_tests {
    use super::*;

    #[test]
    fn nothing_to_write_needs_nothing() {
        assert_eq!(required_free_bytes(0, 2.1, HEADROOM_INSTALL), 0);
    }

    #[test]
    fn small_writes_use_the_headroom_floor() {
        let gib = 1u64 << 30;
        assert_eq!(required_free_bytes(gib, 1.0, HEADROOM_INSTALL), gib + HEADROOM_INSTALL);
    }

    #[test]
    fn large_writes_scale_the_headroom() {
        let write = 100u64 << 30;
        assert_eq!(
            required_free_bytes(write, SPLIT_ARCHIVE_MULTIPLIER, HEADROOM_INSTALL),
            (write as f64 * SPLIT_ARCHIVE_MULTIPLIER) as u64 + write / 20
        );
    }

    #[test]
    fn only_archive_modes_split() {
        assert!(splits_archives(InstallMode::Hypergryph));
        assert!(splits_archives(InstallMode::Bluepoch));
        assert!(!splits_archives(InstallMode::Default));
        assert!(!splits_archives(InstallMode::Bd2));
        assert!(!splits_archives(InstallMode::Sophon));
    }

    #[test]
    fn only_the_first_write_of_an_in_place_update_is_recorded() {
        let writes = InstallWrites::default();
        let records = std::sync::atomic::AtomicUsize::new(0);
        let write = || {
            writes.before_write(|| {
                records.fetch_add(1, Ordering::SeqCst);
            })
        };
        writes.begin(false);
        write();
        assert_eq!(records.load(Ordering::SeqCst), 0);
        writes.begin(true);
        write();
        write();
        assert_eq!(records.load(Ordering::SeqCst), 1);
        writes.end();
        write();
        assert_eq!(records.load(Ordering::SeqCst), 1);
        writes.begin(true);
        write();
        assert_eq!(records.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn writers_wait_until_the_first_write_is_recorded() {
        let writes = Arc::new(InstallWrites::default());
        writes.begin(true);
        let saved = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let first = {
            let (writes, saved) = (Arc::clone(&writes), Arc::clone(&saved));
            std::thread::spawn(move || {
                writes.before_write(|| {
                    entered_tx.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(100));
                    saved.store(true, Ordering::SeqCst);
                });
            })
        };
        entered_rx.recv().unwrap();
        writes.before_write(|| panic!("the record is saved once"));
        assert!(saved.load(Ordering::SeqCst));
        first.join().unwrap();
    }

    #[test]
    fn staged_packages_are_not_writes_into_the_game() {
        assert!(replaces_live_file(InstallMode::Default, "Client/Content/Paks/a.pak"));
        assert!(replaces_live_file(
            InstallMode::Gf2,
            "GF2_Exilium_Data/LocalCache/Data/AssetBundles_Windows/a.bundle"
        ));
        assert!(!replaces_live_file(InstallMode::Gf2, "GF2_Client.zip"));
        assert!(!replaces_live_file(InstallMode::Bd2, "BrownDust2.zip"));
        assert!(!replaces_live_file(InstallMode::Hypergryph, "Endfield.zip.001"));
        assert!(!replaces_live_file(InstallMode::Bluepoch, "Reverse1999.7z"));
    }
}
