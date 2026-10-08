// ------------ Mod Tools Updater ------------
// Keeps the XXMI mod loader packages up to date. It checks their GitHub releases about
// once an hour, verifies each download's signature, and holds an update back while a
// modded game is running, applying it once the game closes.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::download_engine::{resolve_7z_binary, run_7z_extract};
use super::state::BackendState;
use super::{game_profiles, http, xxmi};

pub const STATE_FILE: &str = "update-state.json";
pub const STAGING_DIR: &str = ".staging";
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(45);
const RATE_LIMIT_FALLBACK_SECS: i64 = 30 * 60;
const INTEGRITY_FILES: [&str; 3] = [xxmi::LOADER_DLL, "d3d11.dll", "d3dcompiler_47.dll"];

const SPECTRUM_KEY: &str = "MHYwEAYHKoZIzj0CAQYFK4EEACIDYgAEYac352uRGKZh6LOwK0fVDW/TpyECEfnRtUp+bP2PJPP63SWOkJ3a/d9pAnPfYezRVJ1hWjZtpRTT8HEAN/b4mWpJvqO43SAEV/1Q6vz9Rk/VvRV3jZ6B/tmqVnIeHKEb";
const GIMI_KEY: &str = "MHYwEAYHKoZIzj0CAQYFK4EEACIDYgAET5SWORxEdlJ3RXWIFiuwMX6oyZedz+DgaxtsbpWyxNQJDgIDj4uKLLJlvhRNpnkFEuQntgJKzJs0SpASBEguPOTE7VSnmp+x5uyDmsQsWzsRSAZip++a02jqR/K2j18H";
const ZZMI_KEY: &str = "MHYwEAYHKoZIzj0CAQYFK4EEACIDYgAEb11GjbKQS6SmRe8TcIc5VMu5Ob3moo5v2YeD+s53xEe4bVPGcToUNLu3Jgqo0OwWZ4RsNy1nR0HId6pR09HedyEMifxebsyPT3T5PH82QozEXHQlTDySklWUfGItoOdf";
const HIMI_KEY: &str = "MHYwEAYHKoZIzj0CAQYFK4EEACIDYgAEeigvK7REsX3f/vb+RRuFkZt/6VRbykI2oQcEU3IiI3N9s6jWqKkxAE2cTC9wKXDlkeSzlHjPxgzrTrKdqwkFzROMjw5T2LixFB5BYaT633aU/cCiHDbArIJ46+GrqemG";

const PUBLIC_KEYS: [(&str, &str); 7] = [
    ("xxmi", SPECTRUM_KEY),
    ("gimi", GIMI_KEY),
    ("srmi", SPECTRUM_KEY),
    ("zzmi", ZZMI_KEY),
    ("wwmi", SPECTRUM_KEY),
    ("himi", HIMI_KEY),
    ("efmi", SPECTRUM_KEY),
];

pub static UPDATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static PENDING: AtomicBool = AtomicBool::new(false);
static DEFERRED: parking_lot::Mutex<BTreeMap<String, String>> =
    parking_lot::Mutex::new(BTreeMap::new());

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct PackageState {
    pub etag: Option<String>,
    pub latest_tag: Option<String>,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub signature: Option<String>,
    pub retry_after: Option<i64>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateState {
    pub packages: BTreeMap<String, PackageState>,
    pub integrity: BTreeMap<String, String>,
}

static STATE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn load_state(root: &Path) -> Result<UpdateState, String> {
    let Some(stored) = super::fs_util::read_json_store(&root.join(STATE_FILE))? else {
        return Ok(UpdateState::default());
    };
    Ok(serde_json::from_value(stored).unwrap_or_else(|e| {
        log::warn!("xxmi: {STATE_FILE} could not be parsed ({e}), so it is read as empty");
        UpdateState::default()
    }))
}

fn current_state(root: &Path) -> Result<UpdateState, String> {
    let _guard = STATE_LOCK.lock();
    load_state(root)
}

pub fn read_state(root: &Path) -> UpdateState {
    current_state(root).unwrap_or_else(|e| {
        log::warn!("xxmi: {e}, so the update state reads as empty");
        UpdateState::default()
    })
}

fn write_state(root: &Path, state: &UpdateState) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| format!("Could not create {root:?}: {e}"))?;
    let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    xxmi::write_replacing(&root.join(STATE_FILE), text.as_bytes())
        .map_err(|e| format!("Could not write {STATE_FILE}: {e}"))
}

fn update_state(root: &Path, change: impl FnOnce(&mut UpdateState)) -> Result<(), String> {
    let _guard = STATE_LOCK.lock();
    let mut state = load_state(root)?;
    change(&mut state);
    write_state(root, &state)
}

fn update_package(root: &Path, package: &str, change: impl FnOnce(&mut PackageState)) {
    if let Err(e) = update_state(root, |state| {
        change(state.packages.entry(package.to_string()).or_default())
    }) {
        log::warn!("xxmi: could not record the {package} release check: {e}");
    }
}

#[derive(Clone, Debug)]
pub struct Release {
    pub tag: String,
    pub asset_name: String,
    pub download_url: String,
    pub signature: Option<String>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn cached_release(state: &PackageState) -> Option<Release> {
    Some(Release {
        tag: state.latest_tag.clone()?,
        asset_name: state.asset_name.clone()?,
        download_url: state.asset_url.clone()?,
        signature: state.signature.clone(),
    })
}

pub fn parse_signature(body: &str) -> Option<String> {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?m)^## Signature[\r\n]+- ((?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{4}|[A-Za-z0-9+/]{3}=|[A-Za-z0-9+/]{2}==))\s*$",
        )
        .expect("signature pattern")
    });
    pattern
        .captures(body)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

pub fn parse_release(package: &str, text: &str) -> Result<Release, String> {
    let json: Value = serde_json::from_str(text)
        .map_err(|e| format!("Invalid release JSON for {package}: {e}"))?;

    let tag = json
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("No tag_name in the latest {package} release"))?
        .to_string();

    let assets = json
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("No assets in the latest {package} release"))?;

    let asset = assets
        .iter()
        .find(|a| {
            a.get("name")
                .and_then(Value::as_str)
                .map(|n| {
                    let lower = n.to_ascii_lowercase();
                    lower.ends_with(".zip") && lower.contains("package")
                })
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("The latest {package} release has no package zip"))?;

    let asset_name = asset
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let download_url = asset
        .get("browser_download_url")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("The {package} package asset has no download URL"))?
        .to_string();
    let signature = json
        .get("body")
        .and_then(Value::as_str)
        .and_then(parse_signature);

    Ok(Release {
        tag,
        asset_name,
        download_url,
        signature,
    })
}

fn rate_limit_reset(headers: &reqwest::header::HeaderMap) -> i64 {
    if let Some(secs) = http::retry_after_secs(headers) {
        return now() + secs as i64;
    }
    headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|reset| *reset > now())
        .unwrap_or_else(|| now() + RATE_LIMIT_FALLBACK_SECS)
}

pub async fn check_package(root: &Path, package: &str) -> Result<Release, String> {
    let repo = xxmi::repo_for(package).ok_or_else(|| format!("Unknown mod package \"{package}\""))?;
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");

    let entry = current_state(root)?
        .packages
        .remove(package)
        .unwrap_or_default();

    if let Some(until) = entry.retry_after.filter(|until| *until > now()) {
        if let Some(cached) = cached_release(&entry) {
            log::info!(
                "xxmi: GitHub is rate limited for {} more s, using the cached {package} release",
                until - now()
            );
            return Ok(cached);
        }
        return Err(format!(
            "GitHub is rate limiting release checks for another {} minutes.",
            ((until - now()) / 60).max(1)
        ));
    }

    let mut request = http::client()
        .get(&url)
        .header("Accept", "application/vnd.github+json");
    if let Some(etag) = entry.etag.as_deref().filter(|e| !e.is_empty()) {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("Request error: {e}"))?;
    let status = response.status().as_u16();

    match status {
        304 => {
            update_package(root, package, |stored| stored.retry_after = None);
            cached_release(&entry).ok_or_else(|| format!("GitHub said the {package} release was unchanged, but nothing was cached."))
        }
        200 => {
            let etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let text = http::read_text_capped(response, &url, http::MAX_FEED_RESPONSE).await?;
            let release = parse_release(package, &text)?;
            update_package(root, package, |stored| {
                stored.etag = etag;
                stored.latest_tag = Some(release.tag.clone());
                stored.asset_name = Some(release.asset_name.clone());
                stored.asset_url = Some(release.download_url.clone());
                stored.signature = release.signature.clone();
                stored.retry_after = None;
            });
            log::info!(
                "xxmi: {package} {} resolves to asset \"{}\"",
                release.tag,
                release.asset_name
            );
            Ok(release)
        }
        403 | 429 => {
            let until = rate_limit_reset(response.headers());
            update_package(root, package, |stored| stored.retry_after = Some(until));
            let cached = cached_release(&entry);
            log::warn!(
                "xxmi: GitHub rate limited the {package} check (HTTP {status}), retrying after {} s",
                until - now()
            );
            cached.ok_or_else(|| {
                "GitHub is rate limiting release checks right now. Try again in a while.".to_string()
            })
        }
        other => Err(format!("HTTP {other} for {url}")),
    }
}

pub fn public_key_for(package: &str) -> Option<&'static str> {
    PUBLIC_KEYS
        .iter()
        .find(|(key, _)| *key == package)
        .map(|(_, key)| *key)
}

pub fn verify_signature(package: &str, bytes: &[u8], signature: &str) -> Result<(), String> {
    let key_text = public_key_for(package)
        .ok_or_else(|| format!("Peebify has no publisher key for the {package} package."))?;
    verify_with_key(package, key_text, bytes, signature)
}

fn verify_with_key(package: &str, key_text: &str, bytes: &[u8], signature: &str) -> Result<(), String> {
    use base64::Engine;
    use p384::ecdsa::signature::hazmat::PrehashVerifier;
    use p384::ecdsa::{Signature, VerifyingKey};
    use p384::pkcs8::DecodePublicKey;
    use sha2::{Digest, Sha256};

    let key_der = base64::engine::general_purpose::STANDARD
        .decode(key_text)
        .map_err(|e| format!("The {package} publisher key is malformed: {e}"))?;
    let key = VerifyingKey::from_public_key_der(&key_der)
        .map_err(|e| format!("The {package} publisher key could not be parsed: {e}"))?;
    let signature_der = base64::engine::general_purpose::STANDARD
        .decode(signature.trim())
        .map_err(|e| format!("The {package} release signature is malformed: {e}"))?;
    let signature = Signature::from_der(&signature_der)
        .map_err(|e| format!("The {package} release signature could not be parsed: {e}"))?;
    let digest = Sha256::digest(bytes);
    key.verify_prehash(&digest, &signature).map_err(|_| {
        format!(
            "The {package} package does not match its publisher's signature. \
             Peebify will not install it."
        )
    })
}

pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match std::io::Read::read(&mut file, &mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn integrity_digests(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut pins = BTreeMap::new();
    for name in INTEGRITY_FILES {
        let digest = file_sha256(&root.join(name))
            .map_err(|e| format!("Could not read {name} to record its checksum: {e}"))?;
        pins.insert(name.to_string(), digest);
    }
    Ok(pins)
}

pub fn record_integrity(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let pins = integrity_digests(root)?;
    update_state(root, |state| state.integrity = pins.clone())?;
    Ok(pins)
}

fn record_integrity_locked(
    root: &Path,
    state: &mut UpdateState,
) -> Result<BTreeMap<String, String>, String> {
    state.integrity = integrity_digests(root)?;
    write_state(root, state)?;
    Ok(state.integrity.clone())
}

pub fn verify_integrity(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let _guard = STATE_LOCK.lock();
    let mut state = load_state(root)?;
    if state.integrity.is_empty() {
        let recorded = record_integrity_locked(root, &mut state)?;
        log::warn!("xxmi: no checksums were on record, so the mod tools on disk were recorded as trusted");
        return Ok(recorded);
    }
    let mut verified = BTreeMap::new();
    let mut added = false;
    for name in INTEGRITY_FILES {
        let digest = file_sha256(&root.join(name))
            .map_err(|e| format!("Could not read {name} before starting the mod loader: {e}"))?;
        verified.insert(name.to_string(), digest.clone());
        match state.integrity.get(name) {
            Some(recorded) if *recorded == digest => {}
            Some(recorded) => {
                log::error!("xxmi: {name} checksum mismatch, recorded {recorded}, found {digest}");
                return Err(format!(
                    "{name} on disk does not match the copy Peebify installed. Something else \
                     changed it, so mods will not be loaded. Open Mods, expand Setup and press \
                     Check for updates to repair the mod tools."
                ));
            }
            None => {
                log::warn!("xxmi: {name} had no recorded checksum, so the copy on disk was recorded");
                state.integrity.insert(name.to_string(), digest);
                added = true;
            }
        }
    }
    if added {
        write_state(root, &state)?;
    }
    Ok(verified)
}

pub async fn install_package(
    app: &AppHandle,
    root: &Path,
    package: &str,
    release: &Release,
    mut on_percent: impl FnMut(f64),
) -> Result<String, String> {
    let signature = release.signature.as_deref().ok_or_else(|| {
        format!(
            "The latest {package} release carries no publisher signature, so Peebify will not \
             install it."
        )
    })?;

    if !release.download_url.starts_with("https://") {
        return Err(format!(
            "The {package} release links to a download that is not HTTPS, so Peebify will not \
             install it."
        ));
    }

    std::fs::create_dir_all(root).map_err(|e| format!("Could not create {root:?}: {e}"))?;
    let staging = root.join(STAGING_DIR);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("Could not create staging dir: {e}"))?;

    let archive_name = super::fs_util::sanitize_folder_name(&release.asset_name);
    let archive_name = if archive_name.to_ascii_lowercase().ends_with(".zip") {
        archive_name
    } else {
        format!("{package}-package.zip")
    };
    let archive = staging.join(archive_name);
    xxmi::download_to(&release.download_url, &archive, |p| on_percent(p * 0.6)).await?;

    let checked = {
        let (archive, owned_package, signature) =
            (archive.clone(), package.to_string(), signature.to_string());
        tauri::async_runtime::spawn_blocking(move || {
            let bytes = std::fs::read(&archive).map_err(|e| {
                format!("Could not read the downloaded {owned_package} package: {e}")
            })?;
            verify_signature(&owned_package, &bytes, &signature)
        })
        .await
        .map_err(|e| format!("Could not check the downloaded {package} package: {e}"))?
    };
    checked.inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staging);
    })?;
    on_percent(62.0);

    let extract_dir = staging.join("out");
    std::fs::create_dir_all(&extract_dir)
        .map_err(|e| format!("Could not create extract dir: {e}"))?;
    let seven_zip = resolve_7z_binary(app).await?;
    run_7z_extract(&seven_zip, &archive, &extract_dir, |p| {
        on_percent(62.0 + p * 0.33)
    })
    .await?;
    if let Some(link) = super::fs_util::find_link(&extract_dir) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "The {package} package contains a link ({}), so Peebify will not install it.",
            link.display()
        ));
    }

    let source = xxmi::unwrap_single_dir(&extract_dir);
    let destination = xxmi::package_dir(root, package);
    std::fs::create_dir_all(&destination)
        .map_err(|e| format!("Could not create {}: {e}", destination.display()))?;
    let is_core = package == xxmi::CORE_KEY;
    if !is_core {
        xxmi::run_update_commands(&xxmi::xcmd_path(&source), "PreInstall", &destination);
    }
    let written = {
        let (source, destination) = (source.clone(), destination.clone());
        tauri::async_runtime::spawn_blocking(move || xxmi::merge_dir(&source, &destination))
            .await
            .map_err(|e| format!("Could not install the {package} package: {e}"))??
    };
    xxmi::record_owned(root, package, &written)?;
    if !is_core {
        xxmi::run_update_commands(&xxmi::xcmd_path(&destination), "PostInstall", &destination);
    }
    on_percent(98.0);

    let _ = std::fs::remove_dir_all(&staging);

    if is_core {
        record_integrity(root)?;
    } else {
        xxmi::disable_shaderfixes_extras(&destination);
    }

    xxmi::record_version(root, package, &release.tag)?;
    on_percent(100.0);

    log::info!(
        "xxmi: installed {package} {} into {} (signature verified)",
        release.tag,
        destination.display()
    );
    Ok(release.tag.clone())
}

pub fn updates_available(root: &Path, packages: &[&str]) -> Vec<Value> {
    let installed = xxmi::installed_versions(root);
    let state = read_state(root);
    installed
        .iter()
        .filter(|(package, _)| packages.contains(&package.as_str()))
        .filter_map(|(package, version)| {
            let latest = state.packages.get(package)?.latest_tag.clone()?;
            (latest != *version).then(|| {
                json!({ "package": package, "installed": version, "latest": latest })
            })
        })
        .collect()
}

pub fn start_background(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            if let Err(e) = run_once(&app).await {
                log::warn!("xxmi: background update check skipped: {e}");
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

fn modded_game_active(app: &AppHandle) -> Option<(&'static str, bool)> {
    let state = app.state::<BackendState>();
    game_profiles::GAME_IDS.iter().find_map(|&id| {
        (state.game.is_game_active_id(id)
            && super::mods::active_for(app, id, game_profiles::profile(id)))
        .then(|| (id, state.game.is_game_running_id(id)))
    })
}

fn toolchain_in_use(app: &AppHandle) -> bool {
    modded_game_active(app).is_some() || xxmi::loader_active()
}

pub fn in_use_reason(app: &AppHandle) -> Option<String> {
    if let Some((game_id, true)) = modded_game_active(app) {
        let name = game_profiles::display_name(game_profiles::profile(game_id));
        return Some(format!("Close {name} before installing or updating the mod tools."));
    }
    toolchain_in_use(app)
        .then(|| "A modded game is starting right now. Try again once it has closed.".to_string())
}

fn first_deferral(package: &str, tag: &str) -> bool {
    DEFERRED.lock().insert(package.to_string(), tag.to_string()).as_deref() != Some(tag)
}

pub fn on_game_stopped(app: &AppHandle) {
    if !PENDING.swap(false, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = run_once(&app).await {
            log::warn!("xxmi: deferred update check skipped: {e}");
        }
    });
}

async fn run_once(app: &AppHandle) -> Result<(), String> {
    if !super::mods::master_enabled(app) {
        return Ok(());
    }
    let state = app.state::<BackendState>();
    let root = xxmi::root(&state.config, &state.user_data);
    if !xxmi::core_installed(&root) {
        return Ok(());
    }
    if !http::is_online_cached() {
        return Err("offline".to_string());
    }
    let auto_update = state.config.get("behavior.modsAutoUpdate") != Value::Bool(false);

    let installed = xxmi::installed_versions(&root);
    let mut packages: Vec<String> = vec![xxmi::CORE_KEY.to_string()];
    packages.extend(
        installed
            .keys()
            .filter(|k| *k != xxmi::CORE_KEY && xxmi::variant_installed(&root, k))
            .cloned(),
    );

    let mut changed = false;
    for package in packages {
        let release = match check_package(&root, &package).await {
            Ok(release) => release,
            Err(e) => {
                log::warn!("xxmi: could not check {package} for updates: {e}");
                continue;
            }
        };
        if installed.get(&package) == Some(&release.tag) {
            continue;
        }
        if !auto_update {
            if first_deferral(&package, &release.tag) {
                log::info!(
                    "xxmi: {package} {} is available (auto-update is off)",
                    release.tag
                );
            }
            changed = true;
            continue;
        }
        let _update = UPDATE_LOCK.lock().await;
        if xxmi::installed_versions(&root).get(&package) == Some(&release.tag) {
            continue;
        }
        if toolchain_in_use(app) {
            PENDING.store(true, Ordering::SeqCst);
            if first_deferral(&package, &release.tag) {
                log::info!(
                    "xxmi: {package} {} is available, installing once no game is running",
                    release.tag
                );
            }
            changed = true;
            continue;
        }
        let Some(_guard) = xxmi::try_lock_variant(&package) else {
            continue;
        };
        match install_package(app, &root, &package, &release, |_| {}).await {
            Ok(tag) => {
                log::info!("xxmi: background update installed {package} {tag}");
                changed = true;
            }
            Err(e) => log::warn!("xxmi: background update of {package} failed: {e}"),
        }
    }
    if changed {
        let _ = app.emit("mods-status-changed", json!({}));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_is_read_out_of_the_release_notes() {
        let body = "Some notes\r\n\r\n## Signature\r\n- MEUCIQDabc+/==\r\n\r\nmore";
        assert_eq!(parse_signature(body), None);
        let body = "## Signature\n- MEUCIQDa\n";
        assert_eq!(parse_signature(body).as_deref(), Some("MEUCIQDa"));
        let body = "## Signature\r\n- MEUCIQDabcd=\r\n";
        assert_eq!(parse_signature(body).as_deref(), Some("MEUCIQDabcd="));
    }

    #[test]
    fn a_release_needs_a_package_zip_and_keeps_its_signature() {
        let text = r###"{"tag_name":"v1.2.3","body":"## Signature\n- AAAA\n","assets":[{"name":"XXMI-PACKAGE-v1.2.3.zip","browser_download_url":"https://x/y.zip"}]}"###;
        let release = parse_release("xxmi", text).unwrap();
        assert_eq!(release.tag, "v1.2.3");
        assert_eq!(release.asset_name, "XXMI-PACKAGE-v1.2.3.zip");
        assert_eq!(release.signature.as_deref(), Some("AAAA"));
        let text = r#"{"tag_name":"v1","body":"","assets":[{"name":"notes.txt","browser_download_url":"u"}]}"#;
        assert!(parse_release("xxmi", text).is_err());
    }

    #[test]
    fn every_package_has_a_publisher_key_that_parses() {
        use base64::Engine;
        use p384::ecdsa::VerifyingKey;
        use p384::pkcs8::DecodePublicKey;
        for (package, key) in PUBLIC_KEYS {
            let der = base64::engine::general_purpose::STANDARD.decode(key).unwrap();
            assert!(VerifyingKey::from_public_key_der(&der).is_ok(), "{package}");
        }
    }

    #[test]
    fn a_bad_signature_is_refused() {
        let bogus = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            [0x30u8, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01],
        );
        assert!(verify_signature("xxmi", b"hello", &bogus).is_err());
        assert!(verify_signature("xxmi", b"hello", "not base64!").is_err());
    }

    #[test]
    fn update_state_round_trips() {
        let dir = std::env::temp_dir().join(format!("peebify-xxmi-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut state = UpdateState::default();
        state.packages.insert(
            "xxmi".into(),
            PackageState {
                etag: Some("\"abc\"".into()),
                latest_tag: Some("v1.0.0".into()),
                ..Default::default()
            },
        );
        state.integrity.insert("d3d11.dll".into(), "00".into());
        write_state(&dir, &state).unwrap();
        let back = read_state(&dir);
        assert_eq!(back.packages["xxmi"].latest_tag.as_deref(), Some("v1.0.0"));
        assert_eq!(back.integrity["d3d11.dll"], "00");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("peebify-xxmi-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_pin_is_recorded_and_a_changed_file_is_refused() {
        let dir = scratch("pins");
        for name in INTEGRITY_FILES {
            std::fs::write(dir.join(name), name.as_bytes()).unwrap();
        }
        let mut state = UpdateState::default();
        for name in [xxmi::LOADER_DLL, "d3d11.dll"] {
            state
                .integrity
                .insert(name.to_string(), file_sha256(&dir.join(name)).unwrap());
        }
        write_state(&dir, &state).unwrap();
        let verified = verify_integrity(&dir).unwrap();
        assert!(read_state(&dir).integrity.contains_key("d3dcompiler_47.dll"));
        for name in INTEGRITY_FILES {
            assert_eq!(verified[name], file_sha256(&dir.join(name)).unwrap());
        }
        std::fs::write(dir.join("d3dcompiler_47.dll"), b"swapped").unwrap();
        assert!(verify_integrity(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_first_check_returns_the_digests_it_recorded() {
        let dir = scratch("first-pins");
        for name in INTEGRITY_FILES {
            std::fs::write(dir.join(name), name.as_bytes()).unwrap();
        }
        let verified = verify_integrity(&dir).unwrap();
        let recorded = read_state(&dir).integrity;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(verified, recorded);
        assert_eq!(verified.len(), INTEGRITY_FILES.len());
        for digest in verified.values() {
            assert!(peebify_helpers::mods::parse_sha256(digest).is_some());
        }
    }

    #[test]
    fn an_unreadable_state_file_reads_as_empty() {
        let dir = scratch("torn");
        std::fs::write(dir.join(STATE_FILE), "{\"packages\":{\"xx").unwrap();
        assert!(read_state(&dir).packages.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_state_file_that_cannot_be_read_is_left_alone() {
        let dir = scratch("blocked");
        for name in INTEGRITY_FILES {
            std::fs::write(dir.join(name), name.as_bytes()).unwrap();
        }
        std::fs::create_dir_all(dir.join(STATE_FILE)).unwrap();
        let recorded = record_integrity(&dir);
        let still_there = dir.join(STATE_FILE).is_dir();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(recorded.is_err());
        assert!(still_there);
    }

    #[test]
    fn a_release_check_keeps_pins_recorded_while_it_ran() {
        let dir = scratch("pin-race");
        for name in INTEGRITY_FILES {
            std::fs::write(dir.join(name), name.as_bytes()).unwrap();
        }
        let mut stale = UpdateState::default();
        stale.integrity.insert("d3d11.dll".into(), "00".into());
        stale.packages.insert(
            "xxmi".into(),
            PackageState {
                etag: Some("\"abc\"".into()),
                ..Default::default()
            },
        );
        write_state(&dir, &stale).unwrap();

        let entry = current_state(&dir).unwrap().packages.remove("xxmi").unwrap();
        let pins = record_integrity(&dir).unwrap();
        update_package(&dir, "xxmi", |stored| stored.latest_tag = Some("v2".into()));
        let after = read_state(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(entry.etag.as_deref(), Some("\"abc\""));
        assert_eq!(after.integrity, pins);
        assert_eq!(after.packages["xxmi"].latest_tag.as_deref(), Some("v2"));
        assert_eq!(after.packages["xxmi"].etag.as_deref(), Some("\"abc\""));
    }

    #[test]
    fn concurrent_state_writers_lose_nothing() {
        let dir = scratch("writers");
        for name in INTEGRITY_FILES {
            std::fs::write(dir.join(name), name.as_bytes()).unwrap();
        }
        let packages: Vec<String> = (0..8).map(|i| format!("pkg{i}")).collect();
        std::thread::scope(|scope| {
            for package in &packages {
                let dir = &dir;
                scope.spawn(move || {
                    for round in 0..5 {
                        update_package(dir, package, |stored| stored.retry_after = Some(round));
                    }
                });
            }
            scope.spawn(|| {
                for _ in 0..5 {
                    verify_integrity(&dir).unwrap();
                    record_integrity(&dir).unwrap();
                }
            });
        });
        let state = read_state(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(state.integrity.len(), INTEGRITY_FILES.len());
        for package in &packages {
            assert_eq!(state.packages[package].retry_after, Some(4), "{package}");
        }
    }

    #[test]
    fn updates_are_limited_to_the_requested_packages() {
        let dir = scratch("updates");
        std::fs::write(dir.join(xxmi::VERSION_FILE), "xxmi=v1\ngimi=v1\nzzmi=v1\n").unwrap();
        let mut state = UpdateState::default();
        for package in ["xxmi", "gimi", "zzmi"] {
            state.packages.insert(
                package.into(),
                PackageState {
                    latest_tag: Some(if package == "zzmi" { "v1" } else { "v2" }.into()),
                    ..Default::default()
                },
            );
        }
        write_state(&dir, &state).unwrap();
        let updates = updates_available(&dir, &[xxmi::CORE_KEY, "zzmi"]);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0]["package"], "xxmi");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deferral_is_noted_once_per_release() {
        assert!(first_deferral("test-deferral", "v1"));
        assert!(!first_deferral("test-deferral", "v1"));
        assert!(first_deferral("test-deferral", "v2"));
    }

    #[tokio::test]
    #[ignore]
    async fn live_every_package_resolves_and_verifies() {
        let dir = std::env::temp_dir().join(format!("peebify-xxmi-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (package, _) in PUBLIC_KEYS {
            let release = check_package(&dir, package).await.unwrap();
            assert!(release.signature.is_some(), "{package} has no signature");
            let bytes = http::client()
                .get(&release.download_url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            verify_signature(package, &bytes, release.signature.as_deref().unwrap()).unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Produced by p384 0.13 from a fixed scalar. Pinned so a crate upgrade that changed key or
    // signature parsing would fail here instead of refusing every real XXMI release.
    #[test]
    fn a_pinned_publisher_signature_still_verifies() {
        let key = "MHYwEAYHKoZIzj0CAQYFK4EEACIDYgAEcszeM3U3YiReAV2pLkj6AoSVUi3EI1bH499R3PVqXhnedCrNOhn3mvNy3JcF9WDYV7kFEaBAasE3vmG2lZnOTIbBxTEK7cxP8LBKvJOuXGPRXkoBV89q57pfrIXn3mZi";
        let sig = "MGYCMQDDJeBNaUthhNxgQeHTZNF4sG7Su4LXw0kifkTE8koTy6FVoxfhqaoM/6hBb3w8WKkCMQCnUcUcckgCdy2g8pz2+l4e/7TxTgleQHEVReI660bVqm80AnqFfR5WXQwikLYG03Y=";
        assert!(verify_with_key("xxmi", key, b"peebify-xxmi-kat", sig).is_ok());
        assert!(verify_with_key("xxmi", key, b"peebify-xxmi-kaT", sig).is_err());
    }
}
