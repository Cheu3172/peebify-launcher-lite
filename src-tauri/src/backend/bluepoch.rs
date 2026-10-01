// ------------ Bluepoch API ------------
// Talks to the Bluepoch servers that host Reverse: 1999. It works out the latest version, builds the package to download, and sends news and activity requests.
// Responses can come back scrambled or gzipped, so they are decoded here first, and it rotates through backup hosts when one is down.
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::fs_util::md5_hex;
use super::http;
use super::validator::Resource;

pub const VERSION_FILE: &str = "reverse1999_version.ini";

const API_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const API_TIMEOUT: Duration = Duration::from_secs(30);

const PAIR_KEY: u8 = 0xED;

fn deobfuscate(buf: &mut [u8]) {
    for pair in buf.chunks_exact_mut(2) {
        let key = (pair[0] ^ pair[1]) ^ PAIR_KEY;
        pair[0] ^= key;
        pair[1] ^= key;
    }
}

fn gunzip(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 2 || bytes[0] != 0x1f || bytes[1] != 0x8b {
        return None;
    }
    let mut out = Vec::new();
    match flate2::read::GzDecoder::new(bytes).read_to_end(&mut out) {
        Ok(_) => Some(out),
        Err(_) if !out.is_empty() => Some(out),
        Err(_) => None,
    }
}

fn parse_json(bytes: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(bytes).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

fn decode_payload(bytes: &[u8]) -> Result<Value, String> {
    let mut candidates: Vec<Vec<u8>> = Vec::with_capacity(4);
    candidates.push(bytes.to_vec());
    if let Some(inflated) = gunzip(bytes) {
        candidates.push(inflated);
    }
    for index in 0..candidates.len() {
        let mut decoded = candidates[index].clone();
        deobfuscate(&mut decoded);
        candidates.push(decoded);
    }
    candidates
        .iter()
        .find_map(|candidate| parse_json(candidate))
        .ok_or_else(|| {
            "Bluepoch API returned a body that is not JSON in any known encoding.".to_string()
        })
}

pub struct ApiConfig {
    hot_update: Vec<String>,
    activity: Vec<String>,
    pub game_id: String,
    channel_id: i64,
    sub_channel_id: String,
    os_type: i64,
    env_type: i64,
    lang: String,
}

fn string_list(profile: &Value, key: &str) -> Vec<String> {
    profile
        .get(key)
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn api_config(profile: &Value) -> Result<ApiConfig, String> {
    let hot_update = string_list(profile, "bpHotUpdateUrls");
    if hot_update.is_empty() {
        return Err("profile is missing 'bpHotUpdateUrls' for the Bluepoch API".to_string());
    }
    let game_id = profile
        .get("bpGameId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("profile is missing 'bpGameId' for the Bluepoch API")?
        .to_string();

    Ok(ApiConfig {
        hot_update,
        activity: string_list(profile, "bpActivityUrls"),
        game_id,
        channel_id: setting(profile, "bpChannelId", Value::as_i64)?,
        sub_channel_id: setting(profile, "bpSubChannelId", Value::as_str)?.to_string(),
        os_type: setting(profile, "bpOsType", Value::as_i64)?,
        env_type: setting(profile, "bpEnvType", Value::as_i64)?,
        lang: setting(profile, "bpLang", Value::as_str)?.to_string(),
    })
}

fn setting<'a, T>(
    profile: &'a Value,
    key: &str,
    read: impl Fn(&'a Value) -> Option<T>,
) -> Result<T, String> {
    profile
        .get(key)
        .and_then(&read)
        .or_else(|| super::game_profiles::known_profile("re1999")?.get(key).and_then(&read))
        .ok_or_else(|| format!("profile is missing '{key}' for the Bluepoch API"))
}

impl ApiConfig {
    fn sign(&self, current_version: &str) -> String {
        md5_hex(
            format!(
                "{}_{}_{}_{}_{}_{}",
                self.game_id,
                self.os_type,
                self.env_type,
                self.channel_id,
                self.sub_channel_id,
                current_version
            )
            .as_bytes(),
        )
    }

    fn update_body(&self, current_version: &str, target_version: Option<&str>) -> Value {
        json!({
            "gameId": self.game_id,
            "osType": self.os_type,
            "envType": self.env_type,
            "channelId": self.channel_id,
            "subChannelId": self.sub_channel_id,
            "currentVersion": current_version,
            "sign": self.sign(current_version),
            "targetVersion": target_version,
        })
    }
}

fn api_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(API_CONNECT_TIMEOUT)
            .timeout(API_TIMEOUT)
            .tcp_nodelay(true)
            .pool_idle_timeout(Duration::from_secs(15))
            .user_agent(http::USER_AGENT)
            .build()
            .unwrap_or_else(|_| http::client().clone())
    })
}

async fn post_once(url: &str, body: &Value, lang: &str) -> Result<Value, String> {
    let response = api_client()
        .post(url)
        .header("lang", lang)
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Request error: {e}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("HTTP {status} for {url}"));
    }
    let bytes = http::read_capped(response, url).await?;
    decode_payload(&bytes)
}

fn preferred_hosts() -> &'static Mutex<HashMap<String, usize>> {
    static PREFERRED: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    PREFERRED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn host_list_key(hosts: &[String]) -> String {
    hosts.join("\n")
}

fn host_rotation(start: usize, len: usize) -> impl Iterator<Item = usize> {
    let start = if start < len { start } else { 0 };
    (0..len).map(move |offset| (start + offset) % len)
}

async fn post(hosts: &[String], path: &str, body: &Value, lang: &str) -> Result<Value, String> {
    let key = host_list_key(hosts);
    let start = preferred_hosts().lock().get(&key).copied().unwrap_or(0);
    let mut last = String::from("no Bluepoch host configured");
    for index in host_rotation(start, hosts.len()) {
        let url = format!("{}{path}", hosts[index].trim_end_matches('/'));
        match post_once(&url, body, lang).await {
            Ok(value) => {
                if index != start {
                    preferred_hosts().lock().insert(key, index);
                }
                return Ok(value);
            }
            Err(e) => {
                log::warn!("Bluepoch: {url} failed: {e}");
                last = e;
            }
        }
    }
    Err(last)
}

fn payload(response: &Value, what: &str) -> Result<Value, String> {
    let code = response.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 200 {
        let msg = response
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("Bluepoch {what} returned code {code}: {msg}"));
    }
    response
        .get("data")
        .filter(|d| !d.is_null())
        .cloned()
        .ok_or_else(|| format!("Bluepoch {what} response had no data"))
}

pub async fn latest_version(profile: &Value) -> Result<String, String> {
    let cfg = api_config(profile)?;
    let body = cfg.update_body("", None);
    let response = post(&cfg.hot_update, "/diff-update/version", &body, &cfg.lang).await?;
    let data = payload(&response, "diff-update/version")?;

    data.get("latestVersion")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Bluepoch diff-update/version had no latestVersion".to_string())
}

pub struct Package {
    pub version: String,
    pub resources: Vec<Resource>,
    pub download_bytes: u64,
    pub install_bytes: u64,
}

fn parse_package(url: &str) -> Option<(Resource, u64)> {
    let name = url.rsplit('/').next()?;
    if name.is_empty() || name.contains('\\') {
        return None;
    }
    let stem = name.strip_suffix(".7z").unwrap_or(name);
    let parts: Vec<&str> = stem.split('_').collect();
    let field = |key: &str| -> Option<&str> {
        parts
            .iter()
            .position(|p| *p == key)
            .and_then(|i| parts.get(i + 1))
            .copied()
    };

    let size: u64 = field("size")?.parse().ok()?;
    let md5 = field("md5")?;
    if md5.len() != 32 || !md5.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let origin = field("origin").and_then(|v| v.parse().ok()).unwrap_or(0);
    Some((
        Resource::new(name, size, md5.to_lowercase()).with_url(Some(url)),
        origin,
    ))
}

fn package_from(node: &Value, version: &str) -> Option<Package> {
    let empty: Vec<Value> = Vec::new();
    let mut resources = Vec::new();
    let mut download_bytes = 0u64;
    let mut install_bytes = 0u64;

    for entry in node.get("packageUrls")?.as_array().unwrap_or(&empty) {
        let Some(url) = entry.as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some((resource, origin)) = parse_package(url) else {
            log::warn!("Bluepoch: could not parse package metadata from {url}");
            continue;
        };
        download_bytes = download_bytes.saturating_add(resource.size);
        install_bytes = install_bytes.saturating_add(origin);
        resources.push(resource);
    }
    if resources.is_empty() {
        return None;
    }

    if let Some(real) = node
        .get("packageRealSize")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
    {
        install_bytes = real;
    }

    Some(Package {
        version: version.to_string(),
        resources,
        download_bytes,
        install_bytes,
    })
}

pub async fn fetch_package(profile: &Value, current_version: &str) -> Result<Package, String> {
    let cfg = api_config(profile)?;
    let latest = latest_version(profile).await?;

    let body = cfg.update_body(current_version, Some(&latest));
    let response = post(&cfg.hot_update, "/diff-update/resource", &body, &cfg.lang).await?;
    let data = payload(&response, "diff-update/resource")?;

    let target = data
        .get("targetVersion")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(&latest)
        .to_string();

    if let Some(delta) = data
        .get("diffPackage")
        .filter(|v| !v.is_null())
        .and_then(|node| package_from(node, &target))
    {
        log::info!(
            "Bluepoch: delta package for {current_version} -> {target} ({} parts).",
            delta.resources.len()
        );
        return Ok(delta);
    }

    let full = data
        .get("fullPackage")
        .filter(|v| !v.is_null())
        .and_then(|node| package_from(node, &target))
        .ok_or("Bluepoch diff-update/resource listed no usable package")?;
    log::info!(
        "Bluepoch: full package for {target} ({} parts, {} bytes compressed).",
        full.resources.len(),
        full.download_bytes
    );
    Ok(full)
}

pub fn data_dir(install_path: &Path, executable_name: &str) -> PathBuf {
    install_path.join(format!("{}_Data", executable_name.trim_end_matches(".exe")))
}

pub fn staging_dirs(install_path: &Path, executable_name: &str) -> Vec<PathBuf> {
    let root = data_dir(install_path, executable_name)
        .join("StreamingAssets")
        .join("PersistentRoot");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("_tmp"))
        .map(|entry| entry.path())
        .collect()
}

pub fn installed_version(install_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(install_path.join(VERSION_FILE)).ok()?;
    let version = text.trim_start_matches('\u{feff}').trim().to_string();
    (!version.is_empty()).then_some(version)
}

pub fn record_version(install_path: &Path, version: &str) -> Result<(), String> {
    super::fs_util::write_atomic(&install_path.join(VERSION_FILE), version.as_bytes())
}

pub(crate) async fn activity_post(
    profile: &Value,
    path: &str,
    body: Value,
) -> Result<Value, String> {
    let cfg = api_config(profile)?;
    if cfg.activity.is_empty() {
        return Err("profile is missing 'bpActivityUrls'".to_string());
    }
    let response = post(&cfg.activity, path, &body, &cfg.lang).await?;
    payload(&response, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_rotation_starts_at_the_remembered_host_and_wraps() {
        assert_eq!(host_rotation(0, 2).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(host_rotation(1, 2).collect::<Vec<_>>(), [1, 0]);
        assert_eq!(host_rotation(2, 3).collect::<Vec<_>>(), [2, 0, 1]);
        assert_eq!(host_rotation(5, 2).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(host_rotation(0, 0).count(), 0);
    }

    #[test]
    fn missing_request_parameters_come_from_the_built_in_profile() {
        let bare = json!({ "bpHotUpdateUrls": ["https://a"], "bpGameId": "60001" });
        let cfg = api_config(&bare).unwrap();
        let builtin = super::super::game_profiles::profile("re1999");
        assert_eq!(Some(cfg.channel_id), builtin["bpChannelId"].as_i64());
        assert_eq!(Some(cfg.sub_channel_id.as_str()), builtin["bpSubChannelId"].as_str());
        assert_eq!(Some(cfg.os_type), builtin["bpOsType"].as_i64());
        assert_eq!(Some(cfg.env_type), builtin["bpEnvType"].as_i64());
        assert_eq!(Some(cfg.lang.as_str()), builtin["bpLang"].as_str());

        let custom = json!({
            "bpHotUpdateUrls": ["https://a"],
            "bpGameId": "60001",
            "bpChannelId": 7,
            "bpLang": "ja",
        });
        let cfg = api_config(&custom).unwrap();
        assert_eq!(cfg.channel_id, 7);
        assert_eq!(cfg.lang, "ja");
    }

    #[test]
    fn host_lists_get_distinct_keys() {
        let hot = vec!["https://a".to_string(), "https://b".to_string()];
        let activity = vec!["https://c".to_string(), "https://d".to_string()];
        assert_ne!(host_list_key(&hot), host_list_key(&activity));
        assert_eq!(host_list_key(&hot), "https://a\nhttps://b");
    }

    #[test]
    fn the_recorded_version_is_replaced_whole() {
        let dir = std::env::temp_dir().join(format!(
            "peebify-bluepoch-test-version-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        record_version(&dir, "2.1.0").unwrap();
        record_version(&dir, "2.2.0").unwrap();
        assert_eq!(installed_version(&dir).as_deref(), Some("2.2.0"));
        let tmp = dir.join(format!("{VERSION_FILE}.tmp"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while tmp.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!tmp.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
