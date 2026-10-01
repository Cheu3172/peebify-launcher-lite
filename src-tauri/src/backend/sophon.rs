// ------------ Sophon Downloader ------------
// Hoyoverse's own download system, used for Genshin, Star Rail, Zenless and Honkai 3rd.
// Games are split into small chunks listed in a manifest, so this fetches the manifests,
// works out which chunks are missing and downloads, verifies and writes them in place.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;

use super::fs_util::{manifest_key, md5_hex, safe_join, write_all_at};
use super::http;

const BUILD_URL: &str = "https://sg-public-api.hoyoverse.com/downloader/sophon_chunk/api/getBuild";

pub const CATEGORY_GAME: &str = "game";

const CHUNK_MAX_RETRIES: u32 = 5;
const CHUNK_RETRY_BASE_MS: u64 = 500;
const OFFLINE_PROBE_ROUNDS: u32 = 10;
const CONTROL_MAX_RETRIES: u32 = 3;
const CONTROL_RETRY_BASE_MS: u64 = 1000;
const MAX_CHUNK_BYTES: u64 = 64 << 20;

const BRANCHES_CACHE_KEY: &str = "branches";
static BRANCHES_CACHE: http::TtlCache<Value> =
    http::TtlCache::new(std::time::Duration::from_secs(120), 1);

// ------------ Manifest Parsing ------------
// Sophon manifests are protobuf. This is a tiny hand-written reader for them, so we do
// not need a protobuf dependency, plus the file and chunk types it produces.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn eof(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut out: u64 = 0;
        let mut shift = 0u32;
        loop {
            let b = *self.buf.get(self.pos).ok_or("protobuf: truncated varint")?;
            self.pos += 1;
            out |= u64::from(b & 0x7f)
                .checked_shl(shift)
                .ok_or("protobuf: varint too long")?;
            if b & 0x80 == 0 {
                return Ok(out);
            }
            shift += 7;
            if shift > 63 {
                return Err("protobuf: varint overflow".into());
            }
        }
    }

    fn slice(&mut self) -> Result<&'a [u8], String> {
        let len = self.varint()? as usize;
        let end = self
            .pos
            .checked_add(len)
            .filter(|e| *e <= self.buf.len())
            .ok_or("protobuf: length-delimited field overruns buffer")?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn string(&mut self) -> Result<String, String> {
        Ok(String::from_utf8_lossy(self.slice()?).into_owned())
    }

    fn skip(&mut self, wire: u64) -> Result<(), String> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => self.pos = self.pos.saturating_add(8),
            2 => {
                self.slice()?;
            }
            5 => self.pos = self.pos.saturating_add(4),
            other => return Err(format!("protobuf: unsupported wire type {other}")),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chunk {
    pub name: String,
    pub md5: String,
    pub offset: u64,
    pub compressed_size: u64,
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Asset {
    pub name: String,
    pub chunks: Vec<Chunk>,
    pub asset_type: i32,
    pub size: u64,
    pub md5: String,
}

impl Asset {
    pub fn is_dir(&self) -> bool {
        self.chunks.is_empty() && self.size == 0 && (self.asset_type != 0 || self.md5.is_empty())
    }
}

fn parse_chunk(buf: &[u8]) -> Result<Chunk, String> {
    let mut r = Reader::new(buf);
    let mut c = Chunk::default();
    while !r.eof() {
        let key = r.varint()?;
        match (key >> 3, key & 7) {
            (1, 2) => c.name = r.string()?,
            (2, 2) => c.md5 = r.string()?,
            (3, 0) => c.offset = r.varint()?,
            (4, 0) => c.compressed_size = r.varint()?,
            (5, 0) => c.size = r.varint()?,
            (_, w) => r.skip(w)?,
        }
    }
    Ok(c)
}

fn parse_asset(buf: &[u8]) -> Result<Asset, String> {
    let mut r = Reader::new(buf);
    let mut a = Asset::default();
    while !r.eof() {
        let key = r.varint()?;
        match (key >> 3, key & 7) {
            (1, 2) => a.name = r.string()?,
            (2, 2) => a.chunks.push(parse_chunk(r.slice()?)?),
            (3, 0) => a.asset_type = r.varint()? as i32,
            (4, 0) => a.size = r.varint()?,
            (5, 2) => a.md5 = r.string()?,
            (_, w) => r.skip(w)?,
        }
    }
    Ok(a)
}

pub fn parse_manifest(buf: &[u8]) -> Result<Vec<Asset>, String> {
    let mut r = Reader::new(buf);
    let mut assets = Vec::new();
    while !r.eof() {
        let key = r.varint()?;
        match (key >> 3, key & 7) {
            (1, 2) => assets.push(parse_asset(r.slice()?)?),
            (_, w) => r.skip(w)?,
        }
    }
    Ok(assets)
}

pub fn validate_asset(asset: &Asset) -> Result<(), String> {
    let mut expected = 0u64;
    for c in &asset.chunks {
        if c.size > MAX_CHUNK_BYTES {
            return Err(format!(
                "{}: chunk {} claims {} bytes, more than the {MAX_CHUNK_BYTES} byte limit",
                asset.name, c.name, c.size
            ));
        }
        if c.offset != expected {
            return Err(format!(
                "{}: chunk {} starts at {} but {} was expected",
                asset.name, c.name, c.offset, expected
            ));
        }
        expected += c.size;
    }
    if expected != asset.size {
        return Err(format!(
            "{}: chunks cover {expected} bytes but the file is {}",
            asset.name, asset.size
        ));
    }
    Ok(())
}

// ------------ Builds And Branches ------------
// Asks the Hoyoverse API which build of a game is current, and which categories (game
// files, each voice language) it is made of, along with the credentials to fetch them.
#[derive(Debug, Clone)]
pub struct Category {
    pub matching_field: String,
    pub manifest_id: String,
    pub manifest_url: String,
    pub chunk_prefix: String,
    pub chunk_compressed: bool,
    pub manifest_compressed: bool,
    pub total_bytes: u64,
    pub compressed_bytes: u64,
    pub file_count: u64,
}

#[derive(Debug, Clone)]
pub struct Build {
    pub tag: String,
    pub categories: Vec<Category>,
}

impl Build {
    pub fn category(&self, matching_field: &str) -> Option<&Category> {
        self.categories
            .iter()
            .find(|c| c.matching_field == matching_field)
    }
}

pub fn is_language_field(field: &str) -> bool {
    let f = field.strip_prefix("mini-").unwrap_or(field);
    matches!(f.as_bytes(), [a, b, b'-', c, d] if [a, b, c, d].iter().all(|x| x.is_ascii_lowercase()))
}

pub fn install_categories<'a>(build: &'a Build, audio_language: &str) -> Vec<&'a Category> {
    let mut out: Vec<&Category> = build
        .categories
        .iter()
        .filter(|c| !is_language_field(&c.matching_field) && !c.matching_field.starts_with("mini-"))
        .collect();
    out.sort_by_key(|c| c.matching_field != CATEGORY_GAME);
    if let Some(audio) = build.category(audio_language) {
        out.push(audio);
    }
    out
}

pub fn category_label(matching_field: &str) -> String {
    match matching_field {
        CATEGORY_GAME => "game files".to_string(),
        "asb" => "asset bundles".to_string(),
        f if f.chars().all(|c| c.is_ascii_digit()) => "extra content".to_string(),
        f if is_language_field(f) => format!("{} audio", f.strip_prefix("mini-").unwrap_or(f)),
        f => f.to_string(),
    }
}

fn join_url(prefix: &str, name: &str, suffix: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    format!("{prefix}/{name}{suffix}")
}

fn num(v: &Value) -> u64 {
    match v {
        Value::String(s) => s.parse().unwrap_or(0),
        Value::Number(n) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

pub fn parse_build(body: &Value) -> Result<Build, String> {
    let data = body.get("data").ok_or_else(|| {
        format!(
            "sophon getBuild failed: {}",
            body.get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("no data")
        )
    })?;

    let categories = data
        .get("manifests")
        .and_then(|v| v.as_array())
        .ok_or("sophon getBuild returned no manifests")?
        .iter()
        .filter_map(|m| {
            let manifest = m.get("manifest")?;
            let id = manifest.get("id")?.as_str()?.to_string();
            let md = m.get("manifest_download")?;
            let cd = m.get("chunk_download")?;
            let stats = m.get("stats");
            Some(Category {
                matching_field: m.get("matching_field")?.as_str()?.to_string(),
                manifest_url: join_url(
                    md.get("url_prefix")?.as_str()?,
                    &id,
                    md.get("url_suffix").and_then(|v| v.as_str()).unwrap_or(""),
                ),
                manifest_id: id,
                chunk_prefix: cd.get("url_prefix")?.as_str()?.to_string(),
                chunk_compressed: cd.get("compression").map(num).unwrap_or(0) != 0,
                manifest_compressed: md.get("compression").map(num).unwrap_or(0) != 0,
                total_bytes: stats
                    .and_then(|s| s.get("uncompressed_size"))
                    .map(num)
                    .unwrap_or(0),
                compressed_bytes: stats
                    .and_then(|s| s.get("compressed_size"))
                    .map(num)
                    .unwrap_or(0),
                file_count: stats
                    .and_then(|s| s.get("file_count"))
                    .map(num)
                    .unwrap_or(0),
            })
        })
        .collect::<Vec<_>>();

    if categories.is_empty() {
        return Err("sophon getBuild returned no usable manifests".into());
    }

    Ok(Build {
        tag: data
            .get("tag")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        categories,
    })
}

pub const BRANCH_MAIN: &str = "main";
const BRANCH_PRE_DOWNLOAD: &str = "pre_download";

static PRE_DOWNLOAD_LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Debug, Clone, PartialEq)]
pub struct BranchAuth {
    pub package_id: String,
    pub password: String,
    pub tag: String,
    pub branch: String,
}

fn find_branch_entry<'a>(
    body: &'a Value,
    biz: &str,
    game_id: Option<&str>,
) -> Result<Option<&'a Value>, String> {
    let branches = body
        .get("data")
        .and_then(|d| d.get("game_branches"))
        .and_then(|v| v.as_array())
        .ok_or("sophon getGameBranches returned no branches")?;

    Ok(game_id
        .and_then(|id| {
            branches
                .iter()
                .find(|b| b.pointer("/game/id").and_then(|v| v.as_str()) == Some(id))
        })
        .or_else(|| {
            branches
                .iter()
                .find(|b| b.pointer("/game/biz").and_then(|v| v.as_str()) == Some(biz))
        }))
}

pub fn parse_branch(
    body: &Value,
    biz: &str,
    game_id: Option<&str>,
    key: &str,
) -> Result<BranchAuth, String> {
    let entry = find_branch_entry(body, biz, game_id)?
        .and_then(|b| b.get(key))
        .filter(|v| v.is_object())
        .ok_or_else(|| format!("sophon: no {key} branch for biz {biz}"))?;

    let field = |k: &str| {
        entry
            .get(k)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    Ok(BranchAuth {
        package_id: field("package_id")
            .ok_or_else(|| format!("sophon: {key} branch for {biz} has no package_id"))?,
        password: field("password")
            .ok_or_else(|| format!("sophon: {key} branch for {biz} has no password"))?,
        tag: field("tag").unwrap_or_default(),
        branch: field("branch")
            .or_else(|| (key == BRANCH_MAIN).then(|| BRANCH_MAIN.to_string()))
            .ok_or_else(|| format!("sophon: {key} branch for {biz} has no branch name"))?,
    })
}

fn pre_download_entry<'a>(body: &'a Value, biz: &str, game_id: Option<&str>) -> Option<&'a Value> {
    find_branch_entry(body, biz, game_id)
        .ok()
        .flatten()
        .and_then(|b| b.get(BRANCH_PRE_DOWNLOAD))
        .filter(|v| v.is_object())
}

fn pre_download_log_key(body: &Value, biz: &str, game_id: Option<&str>) -> Option<(String, String)> {
    let entry = pre_download_entry(body, biz, game_id)?;
    Some(match parse_branch(body, biz, game_id, BRANCH_PRE_DOWNLOAD) {
        Ok(auth) => {
            let tag = if auth.tag.is_empty() {
                "an untagged build".to_string()
            } else {
                auth.tag
            };
            (
                format!("{biz}:{}:{tag}", auth.branch),
                format!("sophon: {biz} pre-download is open for {tag} on branch {}", auth.branch),
            )
        }
        Err(e) => {
            let fields: Vec<&str> = entry
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            (
                format!("{biz}:unreadable"),
                format!(
                    "sophon: {biz} pre-download entry could not be read ({e}); fields: {}",
                    fields.join(", ")
                ),
            )
        }
    })
}

fn note_pre_download(body: &Value, biz: &str, game_id: Option<&str>) {
    let Some((key, message)) = pre_download_log_key(body, biz, game_id) else {
        return;
    };
    let mut logged = PRE_DOWNLOAD_LOGGED.lock();
    if logged.contains(&key) {
        return;
    }
    logged.push(key);
    drop(logged);
    log::info!("{message}");
}

async fn fetch_branches(allow_cached: bool) -> Result<Value, String> {
    if allow_cached {
        if let Some(body) = BRANCHES_CACHE.get(BRANCHES_CACHE_KEY) {
            return Ok(body);
        }
    }
    let url = super::hoyoplay::build_url("getGameBranches", &[]);
    let body = http::with_retry(
        || http::get_json(&url),
        CONTROL_MAX_RETRIES,
        CONTROL_RETRY_BASE_MS,
        "sophon getGameBranches",
    )
    .await?;
    BRANCHES_CACHE.set(BRANCHES_CACHE_KEY, body.clone());
    Ok(body)
}

async fn branch_auth_for_profile(profile: &Value, allow_cached: bool) -> Result<BranchAuth, String> {
    let biz = profile
        .get("hoyoBiz")
        .and_then(|v| v.as_str())
        .ok_or("no hoyoBiz for sophon profile")?;
    let game_id = profile.get("hoyoGameId").and_then(|v| v.as_str());
    let body = fetch_branches(allow_cached).await?;
    note_pre_download(&body, biz, game_id);
    parse_branch(&body, biz, game_id, BRANCH_MAIN)
}

pub async fn fetch_branch_auth_for_profile(profile: &Value) -> Result<BranchAuth, String> {
    branch_auth_for_profile(profile, false).await
}

pub async fn cached_branch_auth_for_profile(profile: &Value) -> Result<BranchAuth, String> {
    branch_auth_for_profile(profile, true).await
}

pub async fn fetch_build(auth: &BranchAuth) -> Result<Build, String> {
    let url = format!(
        "{BUILD_URL}?branch={}&package_id={}&password={}",
        auth.branch, auth.package_id, auth.password
    );
    let body = http::with_retry(
        || http::get_json(&url),
        CONTROL_MAX_RETRIES,
        CONTROL_RETRY_BASE_MS,
        "sophon getBuild",
    )
    .await?;
    parse_build(&body)
}

const MAX_MANIFEST_BYTES: u64 = 1 << 30;

// ------------ Manifest Download ------------
// Downloads and decompresses each category's manifest, with a size cap so a bad
// response cannot eat all the memory.
fn decode_capped(bytes: &[u8], limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    zstd::stream::read::Decoder::new(bytes)?
        .take(limit + 1)
        .read_to_end(&mut out)?;
    if out.len() as u64 > limit {
        return Err(std::io::Error::other(format!(
            "it unpacks to more than {limit} bytes"
        )));
    }
    Ok(out)
}

fn decode_manifest(category: &Category, bytes: &[u8]) -> Result<Vec<Asset>, String> {
    let decoded;
    let raw = if category.manifest_compressed {
        decoded = decode_capped(bytes, MAX_MANIFEST_BYTES).map_err(|e| {
            format!(
                "sophon: manifest {} is not valid zstd: {e}",
                category.manifest_id
            )
        })?;
        &decoded[..]
    } else {
        bytes
    };
    let assets = parse_manifest(raw)?;
    for asset in &assets {
        validate_asset(asset).map_err(|e| {
            format!(
                "sophon: manifest {} is malformed: {e}",
                category.manifest_id
            )
        })?;
    }
    log_empty_assets(&category.manifest_id, &assets);
    Ok(assets)
}

fn log_empty_assets(manifest_id: &str, assets: &[Asset]) {
    let mut histogram: std::collections::BTreeMap<(i32, bool), usize> =
        std::collections::BTreeMap::new();
    for asset in assets.iter().filter(|a| a.chunks.is_empty() && a.size == 0) {
        *histogram
            .entry((asset.asset_type, asset.md5.is_empty()))
            .or_default() += 1;
    }
    if histogram.is_empty() {
        return;
    }
    let summary: Vec<String> = histogram
        .iter()
        .map(|((kind, no_md5), count)| {
            let md5 = if *no_md5 { "empty" } else { "set" };
            format!("type {kind} md5 {md5}: {count}")
        })
        .collect();
    log::info!(
        "sophon: manifest {manifest_id} zero size assets by kind ({})",
        summary.join(", ")
    );
}

pub async fn fetch_manifest(category: &Category) -> Result<Vec<Arc<Asset>>, String> {
    let cache_path = manifest_cache_path(&category.manifest_id);

    if let Some(path) = &cache_path {
        if let Ok(bytes) = tokio::fs::read(path).await {
            let cat = category.clone();
            let decoded = tauri::async_runtime::spawn_blocking(move || decode_manifest(&cat, &bytes))
                .await
                .map_err(|e| format!("sophon: manifest decode task failed: {e}"))?;
            match decoded {
                Ok(assets) => return Ok(assets.into_iter().map(Arc::new).collect()),
                Err(e) => {
                    log::warn!(
                        "sophon: cached manifest {} is unreadable ({e}) — refetching",
                        category.manifest_id
                    );
                    let _ = tokio::fs::remove_file(path).await;
                }
            }
        }
    }

    let bytes = http::with_retry(
        || http::get_bytes(&category.manifest_url),
        CONTROL_MAX_RETRIES,
        CONTROL_RETRY_BASE_MS,
        &format!("sophon manifest {}", category.manifest_id),
    )
    .await?;
    let (cat, raw) = (category.clone(), bytes.clone());
    let assets = tauri::async_runtime::spawn_blocking(move || decode_manifest(&cat, &raw))
        .await
        .map_err(|e| format!("sophon: manifest decode task failed: {e}"))??;

    if let Some(path) = cache_path {
        let saved =
            tauri::async_runtime::spawn_blocking(move || super::fs_util::write_atomic(&path, &bytes))
                .await;
        if let Ok(Err(e)) = saved {
            log::warn!("sophon: could not cache manifest {}: {e}", category.manifest_id);
        }
    }
    Ok(assets.into_iter().map(Arc::new).collect())
}

pub async fn fetch_manifests(categories: &[&Category]) -> Result<Vec<Vec<Arc<Asset>>>, String> {
    use futures::stream::{self, StreamExt, TryStreamExt};
    let owned: Vec<Category> = categories.iter().map(|c| (*c).clone()).collect();
    stream::iter(owned)
        .map(|c| async move { fetch_manifest(&c).await })
        .buffered(8)
        .try_collect()
        .await
}

pub fn chunk_url(category: &Category, chunk: &Chunk) -> String {
    join_url(&category.chunk_prefix, &chunk.name, "")
}

// ------------ Applied State ------------
// A record saved next to the game of what we last installed. Updates and repairs use it
// to skip files that have not changed, spot the voice language and remove leftovers.
pub const APPLIED_MANIFEST_FILE: &str = "PeebifySophonManifest.json";
const APPLIED_FORMAT: &str = "sophon-v1";

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppliedFile {
    pub path: String,
    pub size: u64,
    pub md5: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedCategory {
    pub matching_field: String,
    pub manifest_id: String,
    pub files: Vec<AppliedFile>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedManifest {
    format: String,
    pub tag: String,
    pub audio_languages: Vec<String>,
    pub categories: Vec<AppliedCategory>,
}

impl AppliedManifest {
    pub fn file_map(&self) -> std::collections::HashMap<String, AppliedFile> {
        self.categories
            .iter()
            .flat_map(|c| c.files.iter())
            .map(|f| (f.path.clone(), f.clone()))
            .collect()
    }
}

pub fn load_applied(install_dir: &Path) -> Option<AppliedManifest> {
    let path = install_dir.join(APPLIED_MANIFEST_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn!(
                "sophon: could not read {} ({e}), so it is ignored and every file is checked in full.",
                path.display()
            );
            return None;
        }
    };
    let manifest: AppliedManifest = match serde_json::from_str(&text) {
        Ok(manifest) => manifest,
        Err(e) => {
            log::warn!(
                "sophon: {} does not parse ({e}), so it is ignored and every file is checked in full.",
                path.display()
            );
            return None;
        }
    };
    if manifest.format != APPLIED_FORMAT {
        log::warn!(
            "sophon: {} has format {:?} but {APPLIED_FORMAT:?} is expected, so it is ignored and every file is checked in full.",
            path.display(),
            manifest.format
        );
        return None;
    }
    Some(manifest)
}

pub fn applied_tag(install_dir: &Path) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Header {
        format: String,
        tag: String,
    }
    let text = std::fs::read_to_string(install_dir.join(APPLIED_MANIFEST_FILE)).ok()?;
    let header: Header = serde_json::from_str(&text).ok()?;
    let tag = header.tag.trim();
    (header.format == APPLIED_FORMAT && !tag.is_empty()).then(|| tag.to_string())
}

pub fn save_applied(install_dir: &Path, manifest: &AppliedManifest) -> Result<(), String> {
    let text = serde_json::to_string(manifest).map_err(|e| e.to_string())?;
    super::fs_util::write_atomic(&install_dir.join(APPLIED_MANIFEST_FILE), text.as_bytes())
}

pub fn applied_from_manifests(
    tag: &str,
    audio_languages: &[String],
    categories: &[&Category],
    manifests: &[Vec<Arc<Asset>>],
) -> AppliedManifest {
    AppliedManifest {
        format: APPLIED_FORMAT.to_string(),
        tag: tag.to_string(),
        audio_languages: audio_languages.to_vec(),
        categories: categories
            .iter()
            .zip(manifests)
            .map(|(category, assets)| AppliedCategory {
                matching_field: category.matching_field.clone(),
                manifest_id: category.manifest_id.clone(),
                files: assets
                    .iter()
                    .filter(|a| !a.is_dir())
                    .map(|a| AppliedFile {
                        path: a.name.clone(),
                        size: a.size,
                        md5: a.md5.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub fn applied_voice_languages(applied: &AppliedManifest) -> Vec<String> {
    applied
        .categories
        .iter()
        .map(|c| c.matching_field.as_str())
        .filter(|f| !f.starts_with("mini-") && is_language_field(f))
        .map(str::to_string)
        .collect()
}

pub fn can_short_circuit(
    applied: Option<&AppliedManifest>,
    tag: &str,
    audio_language: &str,
) -> bool {
    applied.is_some_and(|a| a.tag == tag && a.audio_languages.iter().any(|l| l == audio_language))
}

pub fn quick_verify_applied(install_dir: &Path, applied: &AppliedManifest) -> bool {
    applied
        .categories
        .iter()
        .flat_map(|c| c.files.iter())
        .all(|f| {
            safe_join(install_dir, &f.path)
                .ok()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.is_file() && m.len() == f.size)
                .unwrap_or(false)
        })
}

pub fn count_present_assets(install_dir: &Path, assets: &[Arc<Asset>]) -> usize {
    assets
        .iter()
        .filter(|a| !a.is_dir())
        .filter(|a| {
            safe_join(install_dir, &a.name)
                .ok()
                .and_then(|p| std::fs::metadata(p).ok())
                .is_some_and(|m| m.is_file() && m.len() == a.size)
        })
        .count()
}

pub fn pick_voice_language(counts: &[(String, usize, usize)]) -> Option<String> {
    let mut best: Option<&(String, usize, usize)> = None;
    for entry in counts {
        if entry.1 > 0 && best.is_none_or(|b| entry.1 > b.1) {
            best = Some(entry);
        }
    }
    best.map(|(lang, _, _)| lang.clone())
}

pub async fn detect_voice_language(
    install_dir: &Path,
    build: &Build,
    languages: &[&str],
) -> Result<Option<String>, String> {
    let categories: Vec<&Category> = languages
        .iter()
        .filter_map(|lang| build.category(lang))
        .collect();
    if categories.is_empty() {
        return Ok(None);
    }
    let manifests = fetch_manifests(&categories).await?;
    let fields: Vec<String> = categories.iter().map(|c| c.matching_field.clone()).collect();
    let dir = install_dir.to_path_buf();
    let counts = tauri::async_runtime::spawn_blocking(move || {
        fields
            .into_iter()
            .zip(&manifests)
            .map(|(field, assets)| {
                let total = assets.iter().filter(|a| !a.is_dir()).count();
                (field, count_present_assets(&dir, assets), total)
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| format!("voice pack detection panicked: {e}"))?;

    let significant: Vec<String> = counts
        .iter()
        .filter(|(_, present, total)| *present > 0 && *present * 10 >= *total)
        .map(|(lang, present, total)| format!("{lang} {present}/{total}"))
        .collect();
    if significant.len() > 1 {
        log::info!(
            "sophon: more than one voice pack is on disk in {} ({}); the one with the most files wins.",
            install_dir.display(),
            significant.join(", ")
        );
    }
    Ok(pick_voice_language(&counts))
}

pub fn compute_orphans(
    prev: &AppliedManifest,
    next_files: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    prev.categories
        .iter()
        .flat_map(|c| c.files.iter())
        .filter(|f| {
            let key = manifest_key(&f.path);
            !next_files.contains(&key) && seen.insert(key)
        })
        .map(|f| f.path.clone())
        .collect()
}

pub fn delete_orphans(install_dir: &Path, prev: &AppliedManifest, orphans: &[String]) -> usize {
    let recorded_size: std::collections::HashMap<&str, u64> = prev
        .categories
        .iter()
        .flat_map(|c| c.files.iter())
        .map(|f| (f.path.as_str(), f.size))
        .collect();

    let mut removed = 0usize;
    for rel in orphans {
        let Ok(path) = safe_join(install_dir, rel) else {
            continue;
        };
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if recorded_size.get(rel.as_str()).copied() != Some(meta.len()) {
            log::info!("sophon: keeping {rel} — its size no longer matches the recorded install");
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                removed += 1;
                log::info!("sophon: removed orphaned file {rel}");
                let mut parent = path.parent();
                while let Some(dir) = parent {
                    if dir == install_dir || std::fs::remove_dir(dir).is_err() {
                        break;
                    }
                    parent = dir.parent();
                }
            }
            Err(e) => log::warn!("sophon: could not remove orphaned file {rel}: {e}"),
        }
    }
    removed
}

pub fn finalize_applied_state(
    install_dir: &Path,
    tag: &str,
    audio_languages: &[String],
    categories: &[&Category],
    manifests: &[Vec<Arc<Asset>>],
    prev: Option<&AppliedManifest>,
) -> usize {
    let mut removed = 0usize;
    if let Some(prev) = prev {
        let next: std::collections::HashSet<String> = manifests
            .iter()
            .flatten()
            .filter(|a| !a.is_dir())
            .map(|a| manifest_key(&a.name))
            .collect();
        let orphans = compute_orphans(prev, &next);
        if !orphans.is_empty() {
            removed = delete_orphans(install_dir, prev, &orphans);
        }
    }
    let applied = applied_from_manifests(tag, audio_languages, categories, manifests);
    if let Err(e) = save_applied(install_dir, &applied) {
        log::warn!("sophon: could not record the applied manifest: {e}");
    }
    removed
}

static MANIFEST_CACHE_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_manifest_cache_dir(dir: PathBuf) {
    *MANIFEST_CACHE_DIR.lock() = Some(dir);
}

fn manifest_cache_path(manifest_id: &str) -> Option<PathBuf> {
    if manifest_id.is_empty()
        || !manifest_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    MANIFEST_CACHE_DIR
        .lock()
        .as_ref()
        .map(|d| d.join(manifest_id))
}

pub fn sweep_manifest_cache(max_age: std::time::Duration) {
    let Some(dir) = MANIFEST_CACHE_DIR.lock().clone() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|age| age > max_age)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// ------------ Scan Planning ------------
// Compares the manifest against the files on disk and builds a plan of exactly which
// chunks still need downloading, scanning in parallel.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetPlan {
    pub asset: Arc<Asset>,
    pub missing: Vec<usize>,
    pub recreate: bool,
    pub existing_len: u64,
}

impl AssetPlan {
    pub fn bytes_to_fetch(&self) -> u64 {
        self.missing
            .iter()
            .filter_map(|i| self.asset.chunks.get(*i))
            .map(|c| c.compressed_size)
            .sum()
    }

    pub fn growth_bytes(&self) -> u64 {
        self.asset.size.saturating_sub(self.existing_len)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Plan {
    pub assets: Vec<AssetPlan>,
    pub dirs: Vec<String>,
    pub total_bytes: u64,
    pub unchanged_files: usize,
}

impl Plan {
    pub fn growth_bytes(&self) -> u64 {
        self.assets.iter().map(AssetPlan::growth_bytes).sum()
    }
}

#[derive(Clone, Copy)]
pub enum ScanMode<'a> {
    Deep,
    Diff {
        prev: &'a std::collections::HashMap<String, AppliedFile>,
    },
}

fn diff_trusts(mode: ScanMode, asset: &Asset) -> bool {
    match mode {
        ScanMode::Diff { prev } => prev.get(&asset.name).is_some_and(|p| {
            !asset.md5.is_empty() && p.size == asset.size && p.md5.eq_ignore_ascii_case(&asset.md5)
        }),
        _ => false,
    }
}

fn plan_asset_scan(
    install_dir: &Path,
    asset: &Arc<Asset>,
    mode: ScanMode,
    hooks: &dyn Hooks,
    scratch: &mut Vec<u8>,
) -> Result<AssetPlan, String> {
    let all_missing = || (0..asset.chunks.len()).collect::<Vec<_>>();
    let whole = |missing: Vec<usize>, recreate: bool, existing_len: u64| {
        hooks.event(Event::Bytes {
            path: &asset.name,
            delta: asset.size,
        });
        AssetPlan {
            asset: Arc::clone(asset),
            missing,
            recreate,
            existing_len,
        }
    };
    let path = safe_join(install_dir, &asset.name)?;

    let Ok(meta) = std::fs::metadata(&path) else {
        return Ok(whole(all_missing(), true, 0));
    };
    let existing_len = if meta.is_file() { meta.len() } else { 0 };
    if !meta.is_file() || meta.len() != asset.size {
        return Ok(whole(all_missing(), true, existing_len));
    }
    if diff_trusts(mode, asset) {
        return Ok(whole(Vec::new(), false, existing_len));
    }

    let Ok(mut file) = std::fs::File::open(&path) else {
        return Ok(whole(all_missing(), true, existing_len));
    };

    let mut missing = Vec::new();
    let largest = asset
        .chunks
        .iter()
        .map(|c| usize::try_from(c.size).unwrap_or(usize::MAX))
        .max()
        .unwrap_or(0);
    if scratch.len() < largest {
        scratch.resize(largest, 0);
    }
    for (i, chunk) in asset.chunks.iter().enumerate() {
        super::progress::check_cancel(hooks, "Operation cancelled by user.")?;
        let want = usize::try_from(chunk.size).unwrap_or(usize::MAX);
        let ok = file
            .seek(SeekFrom::Start(chunk.offset))
            .and_then(|_| file.read_exact(&mut scratch[..want]))
            .is_ok()
            && md5_hex(&scratch[..want]) == chunk.md5;
        if !ok {
            missing.push(i);
        }
        hooks.event(Event::Bytes {
            path: &asset.name,
            delta: chunk.size,
        });
    }
    Ok(AssetPlan {
        asset: Arc::clone(asset),
        missing,
        recreate: false,
        existing_len,
    })
}

pub fn scan_totals(manifests: &[Vec<Arc<Asset>>]) -> (u64, usize) {
    let mut bytes = 0u64;
    let mut files = 0usize;
    for asset in manifests.iter().flat_map(|a| a.iter()) {
        if !asset.is_dir() {
            bytes += asset.size;
            files += 1;
        }
    }
    (bytes, files)
}

pub fn plan_scan(
    install_dir: &Path,
    assets: &[Arc<Asset>],
    mode: ScanMode,
    hooks: &dyn Hooks,
) -> Result<Plan, String> {
    let mut plan = Plan::default();
    let mut scratch: Vec<u8> = Vec::new();
    for asset in assets {
        super::progress::check_cancel(hooks, "Operation cancelled by user.")?;
        if asset.is_dir() {
            plan.dirs.push(asset.name.clone());
            continue;
        }
        let a = plan_asset_scan(install_dir, asset, mode, hooks, &mut scratch)?;
        hooks.event(Event::FileDone { path: &asset.name });
        if a.missing.is_empty() && !a.recreate {
            plan.unchanged_files += 1;
            continue;
        }
        plan.total_bytes += a.bytes_to_fetch();
        plan.assets.push(a);
    }
    Ok(plan)
}

pub fn default_scan_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8)
}

enum ScanOutcome {
    Dir(String),
    Unchanged,
    Fetch(AssetPlan),
}

pub fn plan_scan_parallel(
    install_dir: &Path,
    assets: &[Arc<Asset>],
    mode: ScanMode,
    workers: usize,
    hooks: &dyn Hooks,
) -> Result<Plan, String> {
    let workers = workers.min(assets.len()).max(1);
    if workers == 1 {
        return plan_scan(install_dir, assets, mode, hooks);
    }

    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let first_error: Mutex<Option<String>> = Mutex::new(None);
    let mut collected: Vec<Vec<(usize, ScanOutcome)>> = Vec::with_capacity(workers);

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            handles.push(scope.spawn(|| {
                let mut local: Vec<(usize, ScanOutcome)> = Vec::new();
                let mut scratch: Vec<u8> = Vec::new();
                loop {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let index = cursor.fetch_add(1, Ordering::SeqCst);
                    let Some(asset) = assets.get(index) else {
                        break;
                    };
                    if asset.is_dir() {
                        local.push((index, ScanOutcome::Dir(asset.name.clone())));
                        continue;
                    }
                    let scanned =
                        super::progress::check_cancel(hooks, "Operation cancelled by user.")
                            .and_then(|()| {
                                plan_asset_scan(install_dir, asset, mode, hooks, &mut scratch)
                            });
                    match scanned {
                        Ok(a) => {
                            hooks.event(Event::FileDone { path: &asset.name });
                            if a.missing.is_empty() && !a.recreate {
                                local.push((index, ScanOutcome::Unchanged));
                            } else {
                                local.push((index, ScanOutcome::Fetch(a)));
                            }
                        }
                        Err(e) => {
                            stop.store(true, Ordering::SeqCst);
                            let mut slot = first_error.lock();
                            if slot.is_none() {
                                *slot = Some(e);
                            }
                            break;
                        }
                    }
                }
                local
            }));
        }
        for handle in handles {
            match handle.join() {
                Ok(local) => collected.push(local),
                Err(_) => {
                    let mut slot = first_error.lock();
                    if slot.is_none() {
                        *slot = Some("sophon scan worker panicked".to_string());
                    }
                }
            }
        }
    });

    if let Some(e) = first_error.into_inner() {
        return Err(e);
    }

    let mut merged: Vec<(usize, ScanOutcome)> = collected.into_iter().flatten().collect();
    merged.sort_by_key(|(index, _)| *index);
    let mut plan = Plan::default();
    for (_, outcome) in merged {
        match outcome {
            ScanOutcome::Dir(name) => plan.dirs.push(name),
            ScanOutcome::Unchanged => plan.unchanged_files += 1,
            ScanOutcome::Fetch(a) => {
                plan.total_bytes += a.bytes_to_fetch();
                plan.assets.push(a);
            }
        }
    }
    Ok(plan)
}

pub use super::progress::{FileEvent as Event, FileHooks as Hooks};

// ------------ Chunk Download And Apply ------------
// Runs a plan: downloads each chunk with retries, checks its hash, decompresses it and
// writes it into the right spot in the game files.
pub type BeforeWrite = Option<Arc<dyn Fn() + Send + Sync>>;

pub fn note_write(before_write: &BeforeWrite) {
    if let Some(note) = before_write {
        note();
    }
}

fn open_asset_file(
    install_dir: &Path,
    name: &str,
    size: u64,
) -> Result<(PathBuf, std::fs::File), String> {
    let path = safe_join(install_dir, name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
        let _ = std::fs::remove_dir(&path);
    }
    let file = super::fs_util::retry_while_locked(|| {
        std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
    })
    .map_err(|e| super::fs_util::fmt_io(&format!("Could not open {}", path.display()), &e))?;
    file.set_len(size).map_err(|e| {
        super::fs_util::explain_disk_full(&path, size, format!("Could not size {}: {e}", path.display()))
    })?;
    Ok((path, file))
}

pub const STATUS_OFFLINE: &str = "Internet connection lost. It will resume automatically.";

struct AssetTarget {
    asset: Arc<Asset>,
    missing: usize,
}

struct OpenAsset {
    file: std::fs::File,
    remaining: std::sync::atomic::AtomicUsize,
}

struct AssetFiles {
    open: Mutex<std::collections::HashMap<usize, Arc<OpenAsset>>>,
    before_write: BeforeWrite,
}

impl AssetFiles {
    fn new(before_write: BeforeWrite) -> Self {
        Self {
            open: Mutex::new(std::collections::HashMap::new()),
            before_write,
        }
    }

    fn acquire(
        &self,
        install_dir: &Path,
        target: &AssetTarget,
        index: usize,
    ) -> Result<Arc<OpenAsset>, String> {
        if let Some(existing) = self.open.lock().get(&index) {
            return Ok(Arc::clone(existing));
        }
        note_write(&self.before_write);
        let (_path, file) = open_asset_file(install_dir, &target.asset.name, target.asset.size)?;
        let mut open = self.open.lock();
        if let Some(existing) = open.get(&index) {
            return Ok(Arc::clone(existing));
        }
        let handle = Arc::new(OpenAsset {
            file,
            remaining: std::sync::atomic::AtomicUsize::new(target.missing),
        });
        open.insert(index, Arc::clone(&handle));
        Ok(handle)
    }

    fn release(&self, index: usize) {
        self.open.lock().remove(&index);
    }
}

struct JobTarget {
    target_idx: usize,
    offset: u64,
    compressed_size: u64,
}

struct ChunkJob {
    url: String,
    name: String,
    compressed: bool,
    md5: String,
    size: u64,
    targets: Vec<JobTarget>,
}

fn build_chunk_jobs(
    plans: &[(Category, Plan)],
) -> (Vec<AssetTarget>, std::collections::VecDeque<ChunkJob>) {
    let mut targets: Vec<AssetTarget> = Vec::new();
    let mut jobs: Vec<ChunkJob> = Vec::new();
    let mut by_content: std::collections::HashMap<(String, u64), usize> =
        std::collections::HashMap::new();

    for (category, plan) in plans {
        for asset_plan in &plan.assets {
            let target_idx = targets.len();
            let mut queued = 0usize;
            for &chunk_idx in &asset_plan.missing {
                let Some(chunk) = asset_plan.asset.chunks.get(chunk_idx) else {
                    continue;
                };
                queued += 1;
                let target = JobTarget {
                    target_idx,
                    offset: chunk.offset,
                    compressed_size: chunk.compressed_size,
                };
                match by_content.entry((chunk.md5.clone(), chunk.size)) {
                    std::collections::hash_map::Entry::Occupied(slot) => {
                        jobs[*slot.get()].targets.push(target);
                    }
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(jobs.len());
                        jobs.push(ChunkJob {
                            url: chunk_url(category, chunk),
                            name: chunk.name.clone(),
                            compressed: category.chunk_compressed,
                            md5: chunk.md5.clone(),
                            size: chunk.size,
                            targets: vec![target],
                        });
                    }
                }
            }
            targets.push(AssetTarget {
                asset: Arc::clone(&asset_plan.asset),
                missing: queued,
            });
            debug_assert_eq!(targets.len() - 1, target_idx);
        }
    }
    (targets, jobs.into())
}

fn decode_verify_write(
    install_dir: &Path,
    job: &ChunkJob,
    raw: &[u8],
    targets: &[AssetTarget],
    files: &AssetFiles,
    hooks: &dyn Hooks,
) -> Result<(), String> {
    let bytes: std::borrow::Cow<[u8]> = if job.compressed {
        let capacity = usize::try_from(job.size).map_err(|_| {
            format!(
                "chunk {} size {} does not fit in memory",
                job.name, job.size
            )
        })?;
        std::borrow::Cow::Owned(
            zstd::bulk::decompress(raw, capacity)
                .map_err(|e| format!("chunk {} is not valid zstd: {e}", job.name))?,
        )
    } else {
        std::borrow::Cow::Borrowed(raw)
    };
    if bytes.len() as u64 != job.size {
        return Err(format!(
            "chunk {} decompressed to {} bytes, expected {}",
            job.name,
            bytes.len(),
            job.size
        ));
    }
    let got = md5_hex(&bytes);
    if got != job.md5 {
        return Err(format!(
            "chunk {} failed verification (got {got}, expected {})",
            job.name, job.md5
        ));
    }

    for t in &job.targets {
        let Some(target) = targets.get(t.target_idx) else {
            continue;
        };
        let open = files.acquire(install_dir, target, t.target_idx)?;
        write_all_at(&open.file, &bytes, t.offset)
            .map_err(|e| super::fs_util::fmt_io("Write error", &e))?;
        let previous = open.remaining.fetch_sub(1, Ordering::SeqCst);
        hooks.event(Event::Bytes {
            path: &target.asset.name,
            delta: t.compressed_size,
        });
        if previous <= 1 {
            open.file
                .sync_data()
                .map_err(|e| super::fs_util::fmt_io("Write error", &e))?;
            files.release(t.target_idx);
            hooks.event(Event::FileDone {
                path: &target.asset.name,
            });
        }
    }
    Ok(())
}

fn chunk_error_is_retryable(message: &str) -> bool {
    match super::fs_util::classify(message) {
        super::fs_util::FailureKind::Network | super::fs_util::FailureKind::Validation => true,
        super::fs_util::FailureKind::Other => {
            message.contains("not valid zstd")
                || message.contains("decompressed to")
                || message.contains("HTTP ")
        }
        _ => false,
    }
}

async fn process_job(
    install_dir: Arc<PathBuf>,
    job: Arc<ChunkJob>,
    targets: Arc<Vec<AssetTarget>>,
    files: Arc<AssetFiles>,
    hooks: Arc<dyn Hooks>,
    cancelled: Arc<AtomicBool>,
    outage: Arc<http::OutageGate>,
) -> Result<(), String> {
    let mut attempt = 1u32;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err("Operation cancelled by user.".to_string());
        }
        let outcome = match http::get_bytes(&job.url).await {
            Ok(raw) => {
                let job = Arc::clone(&job);
                let targets = Arc::clone(&targets);
                let files = Arc::clone(&files);
                let hooks = Arc::clone(&hooks);
                let dir = Arc::clone(&install_dir);
                tauri::async_runtime::spawn_blocking(move || {
                    decode_verify_write(&dir, &job, &raw, &targets, &files, hooks.as_ref())
                })
                .await
                .map_err(|e| format!("sophon chunk task panicked: {e}"))?
            }
            Err(e) => Err(e),
        };
        match outcome {
            Ok(()) => return Ok(()),
            Err(e) => {
                if super::fs_util::classify(&e) == super::fs_util::FailureKind::Network
                    && !http::can_reach(&job.url).await
                {
                    outage
                        .single_flight(|| async {
                            log::warn!("sophon: connection lost, waiting for it to return.");
                            http::note_unreachable();
                            hooks.status(STATUS_OFFLINE);
                            let mut restored = false;
                            let mut rounds = 0u32;
                            while !cancelled.load(Ordering::SeqCst) && rounds < OFFLINE_PROBE_ROUNDS {
                                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                                rounds += 1;
                                if http::can_reach(&job.url).await {
                                    restored = true;
                                    break;
                                }
                            }
                            if cancelled.load(Ordering::SeqCst) {
                                return;
                            }
                            if restored {
                                log::info!("sophon: connection restored, resuming.");
                                http::note_reachable();
                            } else {
                                log::info!("sophon: the connection probe still fails, so the chunk itself is tried again.");
                            }
                            hooks.status("");
                        })
                        .await;
                    if cancelled.load(Ordering::SeqCst) {
                        return Err("Operation cancelled by user.".to_string());
                    }
                    continue;
                }
                if !chunk_error_is_retryable(&e) || attempt >= CHUNK_MAX_RETRIES {
                    return Err(e);
                }
                log::warn!(
                    "sophon chunk {} failed (attempt {attempt}/{CHUNK_MAX_RETRIES}): {e} — retrying",
                    job.name
                );
                tokio::time::sleep(http::retry_delay(attempt, CHUNK_RETRY_BASE_MS)).await;
                attempt += 1;
            }
        }
    }
}

pub async fn apply_plans(
    install_dir: &Path,
    plans: &[(Category, Plan)],
    concurrency: usize,
    hooks: Arc<dyn Hooks>,
    cancelled: Arc<AtomicBool>,
    before_write: BeforeWrite,
) -> Result<(), String> {
    let setup: Vec<(Vec<String>, Vec<Arc<Asset>>)> = plans
        .iter()
        .map(|(_, plan)| {
            let empty = plan
                .assets
                .iter()
                .filter(|a| a.missing.is_empty())
                .map(|a| Arc::clone(&a.asset))
                .collect();
            (plan.dirs.clone(), empty)
        })
        .collect();
    {
        let install_dir = install_dir.to_path_buf();
        let hooks = Arc::clone(&hooks);
        let before_write = before_write.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            for (dirs, empty) in setup {
                for dir in &dirs {
                    match safe_join(&install_dir, dir) {
                        Ok(path) => {
                            let _ = std::fs::create_dir_all(path);
                        }
                        Err(e) => log::warn!("sophon: skipping directory from manifest — {e}"),
                    }
                }
                for asset in empty {
                    if hooks.is_cancelled() {
                        return Err("Operation cancelled by user.".to_string());
                    }
                    note_write(&before_write);
                    open_asset_file(&install_dir, &asset.name, asset.size)?;
                    hooks.event(Event::FileDone { path: &asset.name });
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| format!("sophon: file setup task failed: {e}"))??;
    }

    let (targets, jobs) = build_chunk_jobs(plans);
    if jobs.is_empty() {
        return Ok(());
    }
    let unique = jobs.len();
    let occurrences: usize = jobs.iter().map(|j| j.targets.len()).sum();
    if occurrences > unique {
        log::info!(
            "sophon: {unique} unique chunks cover {occurrences} chunk slots — skipping {} duplicate downloads.",
            occurrences - unique
        );
    }

    let queue = Arc::new(Mutex::new(jobs));
    let files = Arc::new(AssetFiles::new(before_write));
    let targets = Arc::new(targets);
    let install_dir = Arc::new(install_dir.to_path_buf());
    let abort = Arc::new(AtomicBool::new(false));
    let outage = Arc::new(http::OutageGate::new());

    let mut workers = Vec::new();
    for _ in 0..concurrency.max(1) {
        let queue = Arc::clone(&queue);
        let files = Arc::clone(&files);
        let hooks = Arc::clone(&hooks);
        let cancelled = Arc::clone(&cancelled);
        let targets = Arc::clone(&targets);
        let install_dir = Arc::clone(&install_dir);
        let abort = Arc::clone(&abort);
        let outage = Arc::clone(&outage);
        workers.push(tauri::async_runtime::spawn(async move {
            loop {
                if cancelled.load(Ordering::SeqCst) {
                    return Err("Operation cancelled by user.".to_string());
                }
                if abort.load(Ordering::SeqCst) {
                    return Ok(());
                }
                super::progress::wait_while_paused(hooks.as_ref()).await;
                let Some(job) = queue.lock().pop_front() else {
                    return Ok(());
                };
                let result = process_job(
                    Arc::clone(&install_dir),
                    Arc::new(job),
                    Arc::clone(&targets),
                    Arc::clone(&files),
                    Arc::clone(&hooks),
                    Arc::clone(&cancelled),
                    Arc::clone(&outage),
                )
                .await;
                if let Err(e) = result {
                    abort.store(true, Ordering::SeqCst);
                    return Err(e);
                }
            }
        }));
    }

    let mut first_error: Option<String> = None;
    for w in workers {
        let outcome = match w.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(e) => Some(format!("Worker panicked: {e}")),
        };
        if let Some(e) = outcome {
            first_error.get_or_insert(e);
        }
    }
    files.open.lock().clear();
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ------------ Tests ------------
// Covers manifest size limits, the applied state record and voice language detection.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_that_unpack_past_the_limit_are_refused() {
        let packed = zstd::encode_all(&vec![0u8; 4096][..], 3).unwrap();
        assert_eq!(decode_capped(&packed, 4096).unwrap().len(), 4096);
        assert!(decode_capped(&packed, 4095).is_err());
        assert!(decode_capped(b"not zstd", 4096).is_err());
    }

    fn applied_with(fields: &[&str]) -> AppliedManifest {
        AppliedManifest {
            format: APPLIED_FORMAT.to_string(),
            tag: "1.0.0".to_string(),
            audio_languages: vec!["en-us".to_string()],
            categories: fields
                .iter()
                .map(|f| AppliedCategory {
                    matching_field: f.to_string(),
                    manifest_id: String::new(),
                    files: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn applied_voice_languages_reads_the_audio_categories_on_disk() {
        let applied = applied_with(&["game", "asb", "ja-jp", "mini-en-us"]);
        assert_eq!(applied_voice_languages(&applied), vec!["ja-jp".to_string()]);
    }

    #[test]
    fn applied_voice_languages_is_empty_for_a_build_without_audio_categories() {
        let applied = applied_with(&["game", "1234"]);
        assert!(applied_voice_languages(&applied).is_empty());
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "peebify-sophon-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let path = safe_join(&self.0, rel).unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn applied_tag_reads_the_saved_manifest_tag_only() {
        let dir = TempDir::new("applied-tag");
        assert_eq!(applied_tag(&dir.0), None);
        save_applied(&dir.0, &applied_with(&["game"])).unwrap();
        assert_eq!(applied_tag(&dir.0), Some("1.0.0".to_string()));
        dir.write(APPLIED_MANIFEST_FILE, br#"{"format":"other","tag":"2.0.0"}"#);
        assert_eq!(applied_tag(&dir.0), None);
    }

    #[test]
    fn count_present_assets_needs_the_exact_size() {
        let dir = TempDir::new("count-present");
        dir.write("Audio/Japanese/a.pck", b"abcd");
        dir.write("Audio/Japanese/b.pck", b"ab");
        let sized = |name: &str, size: u64| {
            Arc::new(Asset {
                name: name.to_string(),
                size,
                md5: "x".to_string(),
                ..Asset::default()
            })
        };
        let assets = vec![
            sized("Audio/Japanese/a.pck", 4),
            sized("Audio/Japanese/b.pck", 4),
            sized("Audio/Japanese/c.pck", 4),
            Arc::new(Asset {
                name: "Audio/Japanese".to_string(),
                asset_type: 64,
                ..Asset::default()
            }),
        ];
        assert_eq!(count_present_assets(&dir.0, &assets), 1);
    }

    #[test]
    fn pick_voice_language_takes_the_pack_with_the_most_files() {
        let counts = |list: &[(&str, usize)]| {
            list.iter()
                .map(|(l, n)| (l.to_string(), *n, 100))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            pick_voice_language(&counts(&[("en-us", 3), ("ja-jp", 90), ("ko-kr", 0)])),
            Some("ja-jp".to_string())
        );
        assert_eq!(
            pick_voice_language(&counts(&[("en-us", 40), ("ja-jp", 40)])),
            Some("en-us".to_string())
        );
        assert_eq!(
            pick_voice_language(&counts(&[("en-us", 0), ("ja-jp", 0)])),
            None
        );
        assert_eq!(pick_voice_language(&[]), None);
    }

    struct NoHooks;

    impl super::super::progress::Control for NoHooks {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    impl Hooks for NoHooks {
        fn event(&self, _event: Event) {}
    }

    fn asset_of(name: &str, parts: &[&[u8]]) -> Arc<Asset> {
        let mut offset = 0u64;
        let chunks = parts
            .iter()
            .enumerate()
            .map(|(i, bytes)| {
                let chunk = Chunk {
                    name: format!("chunk{i}"),
                    md5: md5_hex(bytes),
                    offset,
                    compressed_size: bytes.len() as u64,
                    size: bytes.len() as u64,
                };
                offset += bytes.len() as u64;
                chunk
            })
            .collect();
        Arc::new(Asset {
            name: name.to_string(),
            chunks,
            asset_type: 0,
            size: offset,
            md5: md5_hex(&parts.concat()),
        })
    }

    fn scan(dir: &Path, asset: &Arc<Asset>, mode: ScanMode) -> AssetPlan {
        plan_asset_scan(dir, asset, mode, &NoHooks, &mut Vec::new()).unwrap()
    }

    fn applied_files(files: &[(&str, u64)]) -> AppliedManifest {
        AppliedManifest {
            format: APPLIED_FORMAT.to_string(),
            tag: "1.0.0".to_string(),
            audio_languages: Vec::new(),
            categories: vec![AppliedCategory {
                matching_field: CATEGORY_GAME.to_string(),
                manifest_id: String::new(),
                files: files
                    .iter()
                    .map(|(path, size)| AppliedFile {
                        path: path.to_string(),
                        size: *size,
                        md5: String::new(),
                    })
                    .collect(),
            }],
        }
    }

    fn keys(names: &[&str]) -> std::collections::HashSet<String> {
        names.iter().map(|n| manifest_key(n)).collect()
    }

    #[test]
    fn a_partially_written_file_resumes_from_the_gap() {
        let dir = TempDir::new("gap");
        let asset = asset_of("Data/blob.blk", &[b"aaaa", b"bbbb", b"cccc"]);
        dir.write("Data/blob.blk", b"aaaa\0\0\0\0cccc");

        let plan = scan(&dir.0, &asset, ScanMode::Deep);

        assert_eq!(plan.missing, vec![1]);
        assert!(!plan.recreate);
        assert_eq!(plan.growth_bytes(), 0);
    }

    #[test]
    fn diff_mode_trusts_a_file_only_when_the_recorded_md5_matches() {
        let dir = TempDir::new("diff");
        let asset = asset_of("Data/blob.blk", &[b"aaaa", b"bbbb"]);
        dir.write("Data/blob.blk", b"\0\0\0\0\0\0\0\0");
        let recorded = |md5: &str| {
            std::collections::HashMap::from([(
                asset.name.clone(),
                AppliedFile {
                    path: asset.name.clone(),
                    size: asset.size,
                    md5: md5.to_string(),
                },
            )])
        };

        let same = recorded(&asset.md5.to_uppercase());
        let trusted = scan(&dir.0, &asset, ScanMode::Diff { prev: &same });
        assert!(trusted.missing.is_empty());

        let other = recorded("00000000000000000000000000000000");
        let rescanned = scan(&dir.0, &asset, ScanMode::Diff { prev: &other });
        assert_eq!(rescanned.missing, vec![0, 1]);
    }

    #[test]
    fn a_missing_file_needs_its_full_size_and_a_short_one_only_the_growth() {
        let dir = TempDir::new("growth");
        let asset = asset_of("Data/blob.blk", &[b"aaaa", b"bbbb", b"cccc"]);

        let fresh = scan(&dir.0, &asset, ScanMode::Deep);
        assert!(fresh.recreate);
        assert_eq!(fresh.growth_bytes(), 12);

        dir.write("Data/blob.blk", b"aaaa");
        let short = scan(&dir.0, &asset, ScanMode::Deep);
        assert!(short.recreate);
        assert_eq!(short.growth_bytes(), 8);

        let plan = Plan {
            assets: vec![fresh, short],
            ..Plan::default()
        };
        assert_eq!(plan.growth_bytes(), 20);
    }

    #[test]
    fn a_missing_chunk_offset_reads_as_zero() {
        let chunk = parse_chunk(&[0x0a, 1, b'a', 0x28, 7]).unwrap();
        assert_eq!(chunk.name, "a");
        assert_eq!(chunk.offset, 0);
        assert_eq!(chunk.size, 7);
    }

    #[test]
    fn an_empty_file_is_not_a_directory() {
        let empty_file = Asset {
            name: "marker".to_string(),
            md5: md5_hex(b""),
            ..Asset::default()
        };
        assert!(!empty_file.is_dir());

        let dir = Asset {
            name: "folder".to_string(),
            ..Asset::default()
        };
        assert!(dir.is_dir());

        let typed_dir = Asset {
            name: "folder".to_string(),
            asset_type: 64,
            md5: md5_hex(b""),
            ..Asset::default()
        };
        assert!(typed_dir.is_dir());
    }

    #[test]
    fn an_oversized_chunk_is_rejected() {
        let mut asset = (*asset_of("Data/blob.blk", &[b"aaaa"])).clone();
        asset.chunks[0].size = MAX_CHUNK_BYTES + 1;
        asset.size = MAX_CHUNK_BYTES + 1;
        assert!(validate_asset(&asset).unwrap_err().contains("byte limit"));
    }

    #[test]
    fn orphan_deletion_keeps_a_file_whose_size_changed() {
        let dir = TempDir::new("orphan-size");
        let gone = dir.write("Data/gone.blk", b"data");
        let grown = dir.write("Data/grown.blk", b"grown since install");
        let prev = applied_files(&[("Data/gone.blk", 4), ("Data/grown.blk", 4)]);

        let orphans = compute_orphans(&prev, &keys(&["Data/kept.blk"]));
        let removed = delete_orphans(&dir.0, &prev, &orphans);

        assert_eq!(removed, 1);
        assert!(!gone.exists());
        assert!(grown.is_file());
    }

    #[test]
    fn a_file_renamed_only_by_case_is_not_an_orphan() {
        let dir = TempDir::new("orphan-case");
        let live = dir.write("Data/Foo.blk", b"data");
        let prev = applied_files(&[("Data/Foo.blk", 4)]);

        let orphans = compute_orphans(&prev, &keys(&["data/foo.blk"]));

        assert!(orphans.is_empty());
        assert_eq!(delete_orphans(&dir.0, &prev, &orphans), 0);
        assert!(live.is_file());
    }

    #[test]
    fn a_path_recorded_with_backslashes_is_not_an_orphan() {
        let prev = applied_files(&[(r"Data\Sub\blob.blk", 4), ("Data/gone.blk", 4)]);
        let orphans = compute_orphans(&prev, &keys(&["/data/./sub/BLOB.blk"]));
        assert_eq!(orphans, vec!["Data/gone.blk".to_string()]);
    }

    #[test]
    fn the_applied_manifest_round_trips() {
        let dir = TempDir::new("applied");
        let applied = applied_files(&[("Data/blob.blk", 4)]);
        save_applied(&dir.0, &applied).unwrap();
        assert_eq!(load_applied(&dir.0), Some(applied));
        assert!(!dir.0.join(format!("{APPLIED_MANIFEST_FILE}.tmp")).exists());
    }

    fn branches_body(pre_download: Value) -> Value {
        serde_json::json!({
            "data": {
                "game_branches": [
                    {
                        "game": { "id": "other", "biz": "nap_global" },
                        "main": { "package_id": "p0", "password": "w0", "tag": "1.0.0" },
                        "pre_download": null
                    },
                    {
                        "game": { "id": "gopR6Cufr3", "biz": "hk4e_global" },
                        "main": {
                            "package_id": "p1",
                            "branch": "main",
                            "password": "w1",
                            "tag": "5.0.0"
                        },
                        "pre_download": pre_download
                    }
                ]
            }
        })
    }

    #[test]
    fn the_main_branch_defaults_its_branch_name_to_main() {
        let body = branches_body(Value::Null);
        let auth = parse_branch(&body, "nap_global", None, BRANCH_MAIN).unwrap();
        assert_eq!(auth.branch, "main");
        assert_eq!(auth.package_id, "p0");
        let auth = parse_branch(&body, "hk4e_global", Some("gopR6Cufr3"), BRANCH_MAIN).unwrap();
        assert_eq!(auth.tag, "5.0.0");
        assert_eq!(auth.branch, "main");
    }

    #[test]
    fn the_pre_download_branch_carries_its_own_branch_name() {
        let body = branches_body(serde_json::json!({
            "package_id": "p2",
            "branch": "predownload",
            "password": "w2",
            "tag": "5.1.0"
        }));
        let auth = parse_branch(&body, "hk4e_global", Some("gopR6Cufr3"), BRANCH_PRE_DOWNLOAD).unwrap();
        assert_eq!(
            auth,
            BranchAuth {
                package_id: "p2".to_string(),
                password: "w2".to_string(),
                tag: "5.1.0".to_string(),
                branch: "predownload".to_string(),
            }
        );
        let (key, message) = pre_download_log_key(&body, "hk4e_global", Some("gopR6Cufr3")).unwrap();
        assert_eq!(key, "hk4e_global:predownload:5.1.0");
        assert!(message.contains("5.1.0"));
        assert!(!message.contains("w2"));
    }

    #[test]
    fn a_closed_pre_download_is_not_reported() {
        let body = branches_body(Value::Null);
        assert!(parse_branch(&body, "hk4e_global", None, BRANCH_PRE_DOWNLOAD).is_err());
        assert!(pre_download_log_key(&body, "hk4e_global", None).is_none());
        assert!(pre_download_log_key(&body, "nap_global", None).is_none());
    }

    #[test]
    fn a_pre_download_without_a_branch_name_is_reported_without_its_secrets() {
        let body = branches_body(serde_json::json!({
            "package_id": "p2",
            "password": "w2",
            "tag": "5.1.0"
        }));
        assert!(parse_branch(&body, "hk4e_global", None, BRANCH_PRE_DOWNLOAD).is_err());
        let (key, message) = pre_download_log_key(&body, "hk4e_global", None).unwrap();
        assert_eq!(key, "hk4e_global:unreadable");
        assert!(message.contains("package_id"));
        assert!(!message.contains("w2"));
    }
}
