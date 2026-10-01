// ------------ Girls' Frontline 2 Downloads ------------
// Girls' Frontline 2: Exilium ships its own launcher config and file list, so this reads those, turns them into resources the download engine can use, and keeps a local record of what Peebify installed.
// That record lets an update remove old asset bundles without touching files the game downloaded by itself.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::download_engine::combine_url;
use super::http;

pub const AB_DIR_PARTS: [&str; 4] = [
    "GF2_Exilium_Data",
    "LocalCache",
    "Data",
    "AssetBundles_Windows",
];
const AB_REMOTE_DIR: &str = "AssetBundles_Windows";
const AB_DEST_PREFIX: &str = "GF2_Exilium_Data/LocalCache/Data/AssetBundles_Windows/";
const LOCAL_VERSION_FILE: &str = "Version.txt";
const CLIENT_MARKER_FILE: &str = "GF2_Exilium_Data/LocalCache/.peebify-gf2.json";
const CONFIG_CACHE_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Config {
    pub client_version: String,
    pub client_url: String,
    pub ab_version: String,
    pub ab_base: String,
    pub entries: Vec<Entry>,
}

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub md5: String,
    pub size: u64,
    pub bucket: String,
    pub voice: Option<String>,
}

impl Entry {
    fn parse(line: &str) -> Option<Self> {
        let mut fields = line.trim().split('#');
        let path = fields.next()?;
        let md5 = fields.next()?;
        let size = fields.next()?.trim().parse().ok()?;
        let bucket = fields.next()?.trim();
        if md5.len() != 32 || bucket.is_empty() {
            return None;
        }
        let name = path.rsplit('/').next()?.trim();
        if name.is_empty() || name.contains('\\') {
            return None;
        }
        Some(Self {
            voice: voice_language(name),
            name: name.to_string(),
            md5: md5.trim().to_lowercase(),
            size,
            bucket: bucket.to_string(),
        })
    }

    fn manifest_line(&self) -> String {
        format!(
            "{{0}}/{}#{}#{}#{}",
            self.name,
            self.md5.to_uppercase(),
            self.size,
            self.bucket
        )
    }

    fn resource(&self, ab_base: &str) -> super::validator::Resource {
        super::validator::Resource::new(
            format!("{AB_DEST_PREFIX}{}", self.name),
            self.size,
            self.md5.clone(),
        )
        .with_url(Some(combine_url(
            ab_base,
            &format!("{AB_REMOTE_DIR}/{}/{}", self.bucket, self.name),
        )))
    }
}

fn voice_language(name: &str) -> Option<String> {
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    if stem.len() != 34 {
        return None;
    }
    let (Some(prefix), Some(hash)) = (stem.get(..2), stem.get(2..)) else {
        return None;
    };
    let looks_like_pack = prefix.chars().all(|c| c.is_ascii_uppercase())
        && hash.chars().all(|c| c.is_ascii_hexdigit());
    looks_like_pack.then(|| prefix.to_string())
}

fn config_url(profile: &Value) -> Result<&str, String> {
    profile
        .get("gf2ConfigUrl")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "no gf2ConfigUrl for profile".to_string())
}

fn client_package(profile: &Value) -> &str {
    profile
        .get("gf2ClientPackage")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("GF2_Exilium_Origin.zip")
}

fn plug_config(response: &Value) -> Result<Value, String> {
    let raw = response
        .pointer("/data/flexible_config/nexon_plug")
        .and_then(|v| v.as_str())
        .ok_or("GF2 launcher config has no nexon_plug section")?;
    serde_json::from_str(raw).map_err(|e| format!("Invalid GF2 nexon_plug payload: {e}"))
}

fn plug_string(plug: &Value, key: &str) -> Result<String, String> {
    plug.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("GF2 launcher config has no {key}"))
}

fn ab_bucket(ab_version: &str) -> &str {
    ab_version.rsplit('.').next().unwrap_or(ab_version)
}

pub fn parse_manifest(text: &str) -> Vec<Entry> {
    text.lines().filter_map(Entry::parse).collect()
}

type ConfigCache = Mutex<Option<(Value, Instant)>>;

fn config_cache() -> &'static ConfigCache {
    static CACHE: OnceLock<ConfigCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

async fn fetch_plug(profile: &Value, fresh: bool) -> Result<Value, String> {
    if !fresh {
        if let Some((plug, at)) = config_cache().lock().as_ref() {
            if at.elapsed() < CONFIG_CACHE_TIMEOUT {
                return Ok(plug.clone());
            }
        }
    }
    let response = http::get_json(config_url(profile)?).await?;
    let plug = plug_config(&response)?;
    *config_cache().lock() = Some((plug.clone(), Instant::now()));
    Ok(plug)
}

pub struct ClientInfo {
    pub version: String,
    pub url: String,
    pub file: String,
}

pub async fn fetch_client_info(profile: &Value) -> Result<ClientInfo, String> {
    let plug = fetch_plug(profile, false).await?;
    let version = plug_string(&plug, "game_client_version")?;
    let base = plug_string(&plug, "game_download_addr")?;
    let file = client_package(profile).to_string();
    Ok(ClientInfo {
        url: combine_url(&base, &format!("{version}/{file}")),
        version,
        file,
    })
}

pub struct Versions {
    pub client_version: String,
    pub ab_version: String,
}

pub async fn fetch_versions_with(profile: &Value, fresh: bool) -> Result<Versions, String> {
    let plug = fetch_plug(profile, fresh).await?;
    Ok(Versions {
        client_version: plug_string(&plug, "game_client_version")?,
        ab_version: plug_string(&plug, "ab_resource_version")?,
    })
}

pub async fn fetch_config(profile: &Value) -> Result<Config, String> {
    let plug = fetch_plug(profile, false).await?;
    let client = fetch_client_info(profile).await?;
    let ab_version = plug_string(&plug, "ab_resource_version")?;
    let ab_base = plug_string(&plug, "ab_resource_addr")?;

    let manifest_url = combine_url(
        &ab_base,
        &format!(
            "{AB_REMOTE_DIR}/{}/{LOCAL_VERSION_FILE}",
            ab_bucket(&ab_version)
        ),
    );
    let entries = parse_manifest(&http::get_text(&manifest_url).await?);
    if entries.is_empty() {
        return Err(format!(
            "GF2 resource manifest at {manifest_url} listed no files."
        ));
    }

    Ok(Config {
        client_url: client.url,
        client_version: client.version,
        ab_version,
        ab_base,
        entries,
    })
}

impl Config {
    pub fn base_entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.voice.is_none())
    }

    pub fn resources(&self) -> Vec<super::validator::Resource> {
        self.base_entries()
            .map(|e| e.resource(&self.ab_base))
            .collect()
    }

    pub fn download_bytes(&self) -> u64 {
        self.base_entries().map(|e| e.size).sum()
    }

    pub fn file_count(&self) -> u64 {
        self.base_entries().count() as u64
    }
}

pub fn ab_dir(install_path: &Path) -> PathBuf {
    AB_DIR_PARTS
        .iter()
        .fold(install_path.to_path_buf(), |p, part| p.join(part))
}

pub fn is_bundle_resource(resource: &super::validator::Resource) -> bool {
    resource.dest().starts_with(AB_DEST_PREFIX)
}

fn bundle_name(resource: &super::validator::Resource) -> Option<String> {
    if !is_bundle_resource(resource) {
        return None;
    }
    let name = resource.dest().rsplit('/').next()?;
    (!name.is_empty()).then(|| name.to_string())
}

pub fn client_version_from_url(url: &str) -> Option<&str> {
    url.rsplit('/').nth(1).filter(|v| !v.is_empty())
}

pub fn unchanged_resources(
    install_path: &Path,
    resources: &[super::validator::Resource],
) -> HashSet<String> {
    let dir = ab_dir(install_path);
    let recorded: HashMap<String, Entry> = std::fs::read_to_string(dir.join(LOCAL_VERSION_FILE))
        .map(|text| {
            parse_manifest(&text)
                .into_iter()
                .map(|e| (e.name.clone(), e))
                .collect()
        })
        .unwrap_or_default();

    resources
        .iter()
        .filter_map(|r| {
            let dest = r.dest();
            let size = r.size;

            if !is_bundle_resource(r) {
                let on_disk = size > 0
                    && std::fs::metadata(install_path.join(dest)).is_ok_and(|m| m.len() == size);
                return on_disk.then(|| dest.to_string());
            }

            let name = dest.rsplit('/').next()?;
            let entry = recorded.get(name)?;
            let md5 = r.md5();
            let matches = entry.size == size
                && entry.md5.eq_ignore_ascii_case(md5)
                && std::fs::metadata(dir.join(name)).is_ok_and(|m| m.len() == size);
            matches.then(|| dest.to_string())
        })
        .collect()
}

fn entry_from_resource(resource: &super::validator::Resource) -> Option<Entry> {
    let dest = resource.dest();
    let name = dest.rsplit('/').next()?.to_string();
    let url = resource.url()?;
    let bucket = url
        .rsplit('/')
        .nth(1)
        .filter(|b| !b.is_empty() && *b != AB_REMOTE_DIR)?;
    Some(Entry {
        voice: voice_language(&name),
        name,
        md5: resource.md5().to_lowercase(),
        size: resource.size,
        bucket: bucket.to_string(),
    })
}

pub fn write_local_manifest(
    install_path: &Path,
    resources: &[super::validator::Resource],
) -> Result<usize, String> {
    let dir = ab_dir(install_path);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let mut body = String::new();
    let mut written: HashSet<String> = HashSet::new();
    for entry in resources.iter().filter_map(entry_from_resource) {
        body.push_str(&entry.manifest_line());
        body.push('\n');
        written.insert(entry.name);
    }
    let mut carried = 0usize;
    for entry in recorded_entries(&dir) {
        if !written.contains(&entry.name)
            && std::fs::metadata(dir.join(&entry.name)).is_ok_and(|m| m.len() == entry.size)
        {
            body.push_str(&entry.manifest_line());
            body.push('\n');
            written.insert(entry.name);
            carried += 1;
        }
    }

    let path = dir.join(LOCAL_VERSION_FILE);
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    super::fs_util::finalize_replace(&tmp, &path)?;
    Ok(carried)
}

fn recorded_entries(ab_dir: &Path) -> Vec<Entry> {
    std::fs::read_to_string(ab_dir.join(LOCAL_VERSION_FILE))
        .map(|text| parse_manifest(&text))
        .unwrap_or_default()
}

pub fn prune_stale_bundles(
    install_path: &Path,
    resources: &[super::validator::Resource],
) -> (u64, u64) {
    if resources.is_empty() {
        return (0, 0);
    }
    let keep: HashSet<String> = resources.iter().filter_map(bundle_name).collect();

    let installed = super::download_engine::read_local_index(install_path);
    if installed.is_empty() {
        log::info!(
            "GF2: no resource index from a previous run — leaving the bundle folder untouched."
        );
        return (0, 0);
    }

    let mut removed = 0u64;
    let mut bytes = 0u64;
    for resource in &installed {
        let Some(name) = bundle_name(resource) else {
            continue;
        };
        if keep.contains(&name) {
            continue;
        }
        let Ok(path) = super::fs_util::safe_join(install_path, resource.dest()) else {
            continue;
        };
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if meta.len() != resource.size {
            log::info!("GF2: keeping {name} — its size no longer matches what we installed.");
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                removed += 1;
                bytes += meta.len();
            }
            Err(e) => log::warn!("GF2: could not remove stale bundle {name}: {e}"),
        }
    }
    (removed, bytes)
}

pub fn installed_client_version(install_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(install_path.join(CLIENT_MARKER_FILE)).ok()?;
    let marker: Value = serde_json::from_str(&text).ok()?;
    marker
        .get("clientVersion")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn record_client_version(install_path: &Path, version: &str) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&json!({ "clientVersion": version }))
        .map_err(|e| e.to_string())?;
    super::fs_util::write_atomic(&install_path.join(CLIENT_MARKER_FILE), text.as_bytes())
}

pub fn client_update_pending(install_path: &Path, client_version: &str) -> bool {
    installed_client_version(install_path).is_some_and(|v| v != client_version)
}

pub fn client_is_current(install_path: &Path, exe_name: &str, client_version: &str) -> bool {
    install_path.join(exe_name).is_file()
        && installed_client_version(install_path).as_deref() == Some(client_version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::validator::Resource;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "peebify-gf2-test-{tag}-{}-{:?}",
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
        fn bundle(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let dir = ab_dir(&self.0);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const MD5: &str = "0123456789abcdef0123456789abcdef";

    fn res(name: &str, size: u64) -> Resource {
        Resource::new(format!("{AB_DEST_PREFIX}{name}"), size, MD5).with_url(Some(format!(
            "https://cdn.example/prod/{AB_REMOTE_DIR}/27767/{name}"
        )))
    }

    fn record_installed(dir: &TempDir, resources: &[Resource]) {
        super::super::download_engine::write_local_index(dir.path(), resources).unwrap();
    }

    #[test]
    fn a_voice_pack_name_is_recognised_and_a_non_ascii_name_does_not_panic() {
        assert_eq!(
            voice_language("JA0123456789abcdef0123456789abcdef.bundle").as_deref(),
            Some("JA")
        );
        assert_eq!(voice_language("0123456789abcdef0123456789abcdef.bundle"), None);
        let name = format!("\u{4e2d}{}.bundle", "a".repeat(31));
        assert_eq!(voice_language(&name), None);
    }

    #[test]
    fn a_client_update_is_pending_only_when_the_recorded_version_differs() {
        let dir = TempDir::new("client-pending");
        assert!(!client_update_pending(dir.path(), "1.2.0"));

        record_client_version(dir.path(), "1.1.0").unwrap();
        assert!(client_update_pending(dir.path(), "1.2.0"));
        assert!(!client_update_pending(dir.path(), "1.1.0"));
    }

    #[test]
    fn a_bundle_the_game_downloaded_itself_survives_an_update() {
        let dir = TempDir::new("game-owned");
        let ours = dir.bundle("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", b"data");
        let theirs = dir.bundle("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.bundle", b"data");
        record_installed(&dir, &[res("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", 4)]);

        let (removed, _) = prune_stale_bundles(
            dir.path(),
            &[res("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", 4)],
        );

        assert_eq!(removed, 0);
        assert!(ours.is_file());
        assert!(theirs.is_file(), "a bundle the game fetched was deleted");
    }

    #[test]
    fn a_bundle_we_installed_and_the_manifest_dropped_is_removed() {
        let dir = TempDir::new("superseded");
        let kept = dir.bundle("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", b"data");
        let stale = dir.bundle("cccccccccccccccccccccccccccccccc.bundle", b"data");
        record_installed(
            &dir,
            &[
                res("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", 4),
                res("cccccccccccccccccccccccccccccccc.bundle", 4),
            ],
        );

        let (removed, bytes) = prune_stale_bundles(
            dir.path(),
            &[res("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", 4)],
        );

        assert_eq!((removed, bytes), (1, 4));
        assert!(kept.is_file());
        assert!(!stale.exists());
    }

    #[test]
    fn a_bundle_rewritten_since_we_installed_it_is_kept() {
        let dir = TempDir::new("resized");
        let touched = dir.bundle("cccccccccccccccccccccccccccccccc.bundle", b"grown-in-place");
        record_installed(&dir, &[res("cccccccccccccccccccccccccccccccc.bundle", 4)]);

        let (removed, _) = prune_stale_bundles(
            dir.path(),
            &[res("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", 4)],
        );

        assert_eq!(removed, 0);
        assert!(touched.is_file());
    }

    #[test]
    fn without_a_recorded_index_nothing_is_pruned() {
        let dir = TempDir::new("no-index");
        let stranger = dir.bundle("cccccccccccccccccccccccccccccccc.bundle", b"data");

        let (removed, _) = prune_stale_bundles(
            dir.path(),
            &[res("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", 4)],
        );

        assert_eq!(removed, 0);
        assert!(stranger.is_file());
    }

    #[test]
    fn an_empty_manifest_prunes_nothing() {
        let dir = TempDir::new("empty-manifest");
        let p = dir.bundle("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", b"data");
        record_installed(&dir, &[res("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", 4)]);

        assert_eq!(prune_stale_bundles(dir.path(), &[]), (0, 0));
        assert!(p.is_file(), "an empty manifest must not wipe the install");
    }

    #[test]
    fn the_rewritten_manifest_keeps_lines_the_game_added() {
        let dir = TempDir::new("carry-over");
        dir.bundle("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", b"data");
        dir.bundle("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.bundle", b"data");
        dir.bundle("CN000000000000000000000000000000ff.gf", b"data");
        dir.bundle("dddddddddddddddddddddddddddddddd.bundle", b"data");
        std::fs::write(
            ab_dir(dir.path()).join(LOCAL_VERSION_FILE),
            "{0}/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle#0123456789ABCDEF0123456789ABCDEF#4#27767\n\
             {0}/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.bundle#0123456789ABCDEF0123456789ABCDEF#4#27767\n\
             {0}/CN000000000000000000000000000000ff.gf#0123456789ABCDEF0123456789ABCDEF#4#27767\n\
             {0}/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.bundle#0123456789ABCDEF0123456789ABCDEF#4#27767\n",
        )
        .unwrap();

        let carried = write_local_manifest(
            dir.path(),
            &[res("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle", 4)],
        )
        .unwrap();

        let text = std::fs::read_to_string(ab_dir(dir.path()).join(LOCAL_VERSION_FILE)).unwrap();
        let recorded: Vec<String> = parse_manifest(&text).into_iter().map(|e| e.name).collect();

        assert_eq!(carried, 2);
        assert_eq!(recorded.len(), 3);
        assert!(recorded.contains(&"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bundle".to_string()));
        assert!(
            recorded.contains(&"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.bundle".to_string()),
            "a bundle the game fetched was dropped from Version.txt"
        );
        assert!(recorded.contains(&"CN000000000000000000000000000000ff.gf".to_string()));
        assert!(
            !recorded.contains(&"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.bundle".to_string()),
            "a line whose file is gone was carried over"
        );
        assert!(
            !recorded.contains(&"dddddddddddddddddddddddddddddddd.bundle".to_string()),
            "a file nobody recorded was invented into the manifest"
        );
    }
}
