// ------------ Neverness to Everness Downloader ------------
// Neverness to Everness publishes an encrypted resource list, and each file is fetched from a content-addressed
// URL (by md5 and size). This reads the list, finds what is missing or damaged, and downloads those files in
// pieces, with resume and retries.

use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use md5::{Digest, Md5};
use parking_lot::Mutex;
use serde_json::Value;

use super::fs_util::md5_hex;
use super::http;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

const CONTAINER_MAGIC: &[u8] = b"PatcherXML0\0";
const CONTAINER_HEADER_LEN: usize = 16;
const CRYPTO_IV: &[u8; 16] = b"PatcherSDK000000";
const BASE_TAG: &str = "baseTag";
const LAUNCHER_TAG: &str = "launcher";
const LAUNCHER_DIR: &str = "NTEGlobal";
const MAX_LIST_BYTES: u64 = 64 * 1024 * 1024;

pub struct Config {
    pub res_version: String,
    pub list_hash: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub src_start: u64,
    pub dst_start: u64,
    pub size: u64,
    pub md5: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Archive {
    pub url: String,
    pub md5: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Resource {
    pub dest: String,
    pub size: u64,
    pub md5: String,
    pub tag: String,
    pub optional: bool,
    pub object_md5: String,
    pub object_size: u64,
    pub segments: Vec<Segment>,
    pub archive: Option<Archive>,
}

impl Resource {
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    pub fn download_size(&self) -> u64 {
        match &self.archive {
            Some(archive) => archive.size,
            None => self.size,
        }
    }

    pub fn fetch_bytes(&self) -> u64 {
        match &self.archive {
            Some(archive) => archive.size,
            None => self.segments.iter().map(|s| s.size).sum(),
        }
    }
}

pub struct ResList {
    pub version: String,
    pub resources: Vec<Resource>,
}

impl ResList {
    pub fn selected<'a>(
        &'a self,
        tags: Option<&'a [String]>,
    ) -> impl Iterator<Item = &'a Resource> + 'a {
        self.resources.iter().filter(move |r| {
            if !r.is_optional() {
                return true;
            }
            match tags {
                None => true,
                Some(list) => list.iter().any(|t| t == &r.tag),
            }
        })
    }

    pub fn selected_bytes(&self, tags: Option<&[String]>) -> u64 {
        self.selected(tags).map(|r| r.size).sum()
    }

    pub fn optional_tags(&self) -> Vec<(String, u64, u64)> {
        self.tag_totals()
            .into_iter()
            .filter(|(tag, _, _)| tag != BASE_TAG)
            .collect()
    }

    pub fn tag_totals(&self) -> Vec<(String, u64, u64)> {
        let mut order: Vec<String> = Vec::new();
        let mut totals: std::collections::HashMap<String, (u64, u64)> =
            std::collections::HashMap::new();
        for resource in &self.resources {
            let slot = totals.entry(resource.tag.clone()).or_insert_with(|| {
                order.push(resource.tag.clone());
                (0, 0)
            });
            slot.0 += resource.size;
            slot.1 += 1;
        }
        order
            .into_iter()
            .filter_map(|tag| {
                let (bytes, files) = totals.get(&tag)?;
                Some((tag, *bytes, *files))
            })
            .collect()
    }
}

pub fn tag_language(tag: &str) -> Option<&'static str> {
    match tag {
        "pakchunk101" => Some("Chinese"),
        "pakchunk102" => Some("English"),
        "pakchunk103" => Some("Japanese"),
        "pakchunk104" => Some("Korean"),
        _ => None,
    }
}

pub fn res_bases(profile: &Value) -> Vec<String> {
    profile
        .get("nteResUrls")
        .and_then(Value::as_array)
        .map(|urls| {
            urls.iter()
                .filter_map(|u| u.as_str())
                .filter(|u| !u.is_empty())
                .map(|u| format!("{}/{}", u.trim_end_matches('/'), branch(profile)))
                .collect()
        })
        .unwrap_or_default()
}

fn branch(profile: &Value) -> &str {
    profile
        .get("nteBranch")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("publish_PC")
}

fn app_id(profile: &Value) -> &str {
    profile
        .get("nteAppId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("3000001")
}

fn crypto_key(profile: &Value) -> [u8; 16] {
    let mut key = [b'0'; 16];
    let source = format!("{}@Patcher0", app_id(profile));
    for (slot, byte) in key.iter_mut().zip(source.bytes()) {
        *slot = byte;
    }
    key
}

pub fn object_url(base: &str, md5: &str, size: u64) -> String {
    let shard = md5.chars().next().unwrap_or('0');
    format!("{base}/Res/{shard}/{md5}.{size}")
}

fn xml_value<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim())
}

fn attribute<'a>(raw: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = raw;
    loop {
        let at = rest.find(name)?;
        let after = &rest[at + name.len()..];
        let before_ok = at == 0
            || rest[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        let trimmed = after.trim_start();
        if before_ok && trimmed.starts_with('=') {
            let quoted = trimmed[1..].trim_start();
            let quote = quoted.chars().next()?;
            if quote == '"' || quote == '\'' {
                let end = quoted[1..].find(quote)? + 1;
                return Some(&quoted[1..end]);
            }
        }
        rest = &rest[at + name.len()..];
    }
}

fn number(raw: &str, name: &str) -> Option<u64> {
    attribute(raw, name)?.trim().parse().ok()
}

pub async fn fetch_config(profile: &Value) -> Result<Config, String> {
    let bases = res_bases(profile);
    if bases.is_empty() {
        return Err("no nteResUrls for profile".to_string());
    }

    let mut last = String::new();
    for base in &bases {
        let url = format!("{base}/Version/Windows/config.xml");
        let outcome = match http::get_text(&url).await {
            Ok(xml) => parse_config(&xml),
            Err(e) => Err(e),
        };
        match outcome {
            Ok(config) => return Ok(config),
            Err(e) => {
                log::warn!("nte: version feed {url} failed: {e}");
                last = e;
            }
        }
    }
    Err(format!("Could not read the NTE version feed: {last}"))
}

fn parse_config(xml: &str) -> Result<Config, String> {
    let res_version = xml_value(xml, "ResVersion")
        .filter(|v| !v.is_empty())
        .ok_or("NTE config.xml has no ResVersion")?
        .to_string();
    let list_hash = xml_value(xml, "listHash")
        .filter(|v| v.len() == 32)
        .ok_or("NTE config.xml has no listHash")?
        .to_lowercase();
    Ok(Config {
        res_version,
        list_hash,
    })
}

// ------------ Resource List ------------
// Downloads and decrypts the resource list (an AES-wrapped container with a zip inside) and parses it into
// files, segments and optional content tags. The last parsed list is cached by its hash.
fn zip_entry(archive: &[u8], wanted: &str) -> Result<Vec<u8>, String> {
    zip_read(archive, wanted, MAX_LIST_BYTES)
}

fn zip_read(archive: &[u8], wanted: &str, max_out: u64) -> Result<Vec<u8>, String> {
    let mut cursor = 0usize;
    while cursor + 30 <= archive.len() {
        if archive[cursor..cursor + 4] != [0x50, 0x4b, 0x03, 0x04] {
            break;
        }
        let read_u16 = |at: usize| u16::from_le_bytes([archive[at], archive[at + 1]]) as usize;
        let read_u32 = |at: usize| {
            u32::from_le_bytes([
                archive[at],
                archive[at + 1],
                archive[at + 2],
                archive[at + 3],
            ]) as usize
        };
        let method = read_u16(cursor + 8);
        let compressed = read_u32(cursor + 18);
        let name_len = read_u16(cursor + 26);
        let extra_len = read_u16(cursor + 28);
        let name_at = cursor + 30;
        let data_at = name_at + name_len + extra_len;
        if data_at + compressed > archive.len() {
            return Err("zip archive is truncated".to_string());
        }
        let name = String::from_utf8_lossy(&archive[name_at..name_at + name_len]).to_string();
        let data = &archive[data_at..data_at + compressed];
        if wanted == name {
            return match method {
                0 => Ok(data.to_vec()),
                8 => {
                    let mut out = Vec::new();
                    flate2::read::DeflateDecoder::new(data)
                        .take(max_out)
                        .read_to_end(&mut out)
                        .map_err(|e| format!("zip entry {name} is not valid deflate: {e}"))?;
                    Ok(out)
                }
                other => Err(format!("zip entry {name} uses unsupported method {other}")),
            };
        }
        cursor = data_at + compressed;
    }
    Err(format!("zip archive has no {wanted}"))
}

pub fn decode_container(raw: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, String> {
    if raw.len() < CONTAINER_HEADER_LEN || !raw.starts_with(CONTAINER_MAGIC) {
        return Err("NTE resource list is not a PatcherXML container".to_string());
    }
    let declared = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]) as usize;

    let mut buffer = raw[CONTAINER_HEADER_LEN..].to_vec();
    if buffer.is_empty() || !buffer.len().is_multiple_of(16) {
        return Err("NTE resource list payload is not block aligned".to_string());
    }
    let plain = Aes128CbcDec::new(key.into(), CRYPTO_IV.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|_| "NTE resource list failed to decrypt".to_string())?;

    let mut xml = Vec::new();
    flate2::read::ZlibDecoder::new(plain)
        .take(MAX_LIST_BYTES)
        .read_to_end(&mut xml)
        .map_err(|e| format!("NTE resource list is not valid zlib: {e}"))?;

    if xml.len() != declared {
        return Err(format!(
            "NTE resource list expanded to {} bytes, expected {declared}",
            xml.len()
        ));
    }
    Ok(xml)
}

static RESLIST_CACHE: Mutex<Option<(String, Arc<ResList>)>> = Mutex::new(None);
static RESLIST_FETCH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn cached_reslist(list_hash: &str) -> Option<Arc<ResList>> {
    RESLIST_CACHE
        .lock()
        .as_ref()
        .filter(|(hash, _)| hash == list_hash)
        .map(|(_, list)| Arc::clone(list))
}

pub async fn fetch_reslist(profile: &Value, config: &Config) -> Result<Arc<ResList>, String> {
    if let Some(list) = cached_reslist(&config.list_hash) {
        return Ok(list);
    }
    let _flight = RESLIST_FETCH.lock().await;
    if let Some(list) = cached_reslist(&config.list_hash) {
        return Ok(list);
    }
    let list = Arc::new(download_reslist(profile, config).await?);
    *RESLIST_CACHE.lock() = Some((config.list_hash.clone(), Arc::clone(&list)));
    Ok(list)
}

async fn download_reslist(profile: &Value, config: &Config) -> Result<ResList, String> {
    let bases = res_bases(profile);
    let key = crypto_key(profile);
    let mut last = String::new();

    for base in &bases {
        let url = format!(
            "{base}/Version/Windows/version/{}/ResList.bin.zip",
            config.res_version
        );
        let xml = match fetch_reslist_xml(&url, &key, config).await {
            Ok(bytes) => bytes,
            Err(e) => {
                log::warn!("nte: resource list {url} failed: {e}");
                last = e;
                continue;
            }
        };
        let text = String::from_utf8(xml)
            .map_err(|e| format!("NTE resource list is not valid UTF-8: {e}"))?;
        return parse_reslist(&text);
    }

    Err(format!("Could not read the NTE resource list: {last}"))
}

async fn fetch_reslist_xml(url: &str, key: &[u8; 16], config: &Config) -> Result<Vec<u8>, String> {
    let archive = http::get_bytes(url).await?;
    let container = zip_entry(&archive, "ResList.bin")?;
    let xml = decode_container(&container, key)?;
    let got = md5_hex(&xml);
    if got != config.list_hash {
        return Err(format!(
            "NTE resource list digest {got} does not match the {} advertised by config.xml",
            config.list_hash
        ));
    }
    Ok(xml)
}

pub fn parse_reslist(xml: &str) -> Result<ResList, String> {
    let mut version = String::new();
    let mut tag_stack: Vec<String> = vec![BASE_TAG.to_string()];
    let mut resources: Vec<Resource> = Vec::new();
    let mut open_res: Option<Resource> = None;
    let mut seen_block = false;
    let mut pak: Option<(String, u64)> = None;

    for (name, body, self_closing, closing) in tags(xml) {
        match (name, closing) {
            ("ResList", false) if version.is_empty() => {
                if let Some(v) = attribute(body, "version") {
                    version = v.to_string();
                }
            }
            ("BaseVersion", false) => {
                let tag = attribute(body, "tag")
                    .map(str::to_string)
                    .unwrap_or_else(|| current_tag(&tag_stack));
                tag_stack.push(tag);
            }
            ("BaseVersion", true) if tag_stack.len() > 1 => {
                tag_stack.pop();
            }
            ("Res", false) => {
                let filename = attribute(body, "filename")
                    .ok_or("NTE resource list has a Res without filename")?;
                let size = number(body, "filesize")
                    .ok_or("NTE resource list has a Res without filesize")?;
                let md5 = attribute(body, "md5")
                    .ok_or("NTE resource list has a Res without md5")?
                    .to_lowercase();
                let tag = current_tag(&tag_stack);
                let resource = Resource {
                    dest: normalise(filename),
                    size,
                    md5: md5.clone(),
                    optional: tag != BASE_TAG,
                    tag,
                    object_md5: md5.clone(),
                    object_size: size,
                    segments: vec![Segment {
                        src_start: 0,
                        dst_start: 0,
                        size,
                        md5,
                    }],
                    archive: None,
                };
                if self_closing {
                    resources.push(resource);
                } else {
                    open_res = Some(resource);
                    seen_block = false;
                }
            }
            ("Block", false) => {
                if let Some(resource) = open_res.as_mut() {
                    let start = number(body, "start")
                        .ok_or("NTE resource list has a Block without start")?;
                    let size =
                        number(body, "size").ok_or("NTE resource list has a Block without size")?;
                    let md5 = attribute(body, "md5")
                        .ok_or("NTE resource list has a Block without md5")?
                        .to_lowercase();
                    if !seen_block {
                        resource.segments.clear();
                        seen_block = true;
                    }
                    resource.segments.push(Segment {
                        src_start: start,
                        dst_start: start,
                        size,
                        md5,
                    });
                }
            }
            ("Res", true) => {
                if let Some(resource) = open_res.take() {
                    resources.push(resource);
                }
            }
            ("Pak", false) => {
                let md5 = attribute(body, "md5")
                    .ok_or("NTE resource list has a Pak without md5")?
                    .to_lowercase();
                let size = number(body, "filesize")
                    .ok_or("NTE resource list has a Pak without filesize")?;
                pak = Some((md5, size));
            }
            ("Pak", true) => pak = None,
            ("Entry", false) => {
                let (pak_md5, pak_size) = pak
                    .clone()
                    .ok_or("NTE resource list has an Entry outside a Pak")?;
                let name =
                    attribute(body, "name").ok_or("NTE resource list has an Entry without name")?;
                let offset = number(body, "offset")
                    .ok_or("NTE resource list has an Entry without offset")?;
                let size =
                    number(body, "size").ok_or("NTE resource list has an Entry without size")?;
                let md5 = attribute(body, "md5")
                    .ok_or("NTE resource list has an Entry without md5")?
                    .to_lowercase();
                let tag = current_tag(&tag_stack);
                resources.push(Resource {
                    dest: normalise(name),
                    size,
                    md5: md5.clone(),
                    optional: tag != BASE_TAG,
                    tag,
                    object_md5: pak_md5,
                    object_size: pak_size,
                    segments: vec![Segment {
                        src_start: offset,
                        dst_start: 0,
                        size,
                        md5,
                    }],
                    archive: None,
                });
            }
            _ => {}
        }
    }

    if resources.is_empty() {
        return Err("NTE resource list contained no files".to_string());
    }
    Ok(ResList { version, resources })
}

fn current_tag(stack: &[String]) -> String {
    stack
        .last()
        .cloned()
        .unwrap_or_else(|| BASE_TAG.to_string())
}

fn normalise(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_string()
}

fn tags(xml: &str) -> Vec<(&str, &str, bool, bool)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(open) = xml[at..].find('<') {
        let start = at + open + 1;
        let Some(close) = xml[start..].find('>') else {
            break;
        };
        let end = start + close;
        let raw = &xml[start..end];
        at = end + 1;
        if raw.starts_with('?') || raw.starts_with('!') {
            continue;
        }
        let closing = raw.starts_with('/');
        let self_closing = raw.ends_with('/');
        let inner = raw.trim_start_matches('/').trim_end_matches('/');
        let name_end = inner
            .find(|c: char| c.is_whitespace())
            .unwrap_or(inner.len());
        let name = &inner[..name_end];
        if name.is_empty() {
            continue;
        }
        out.push((name, &inner[name_end..], self_closing, closing));
        if self_closing && !closing {
            out.push((name, "", false, true));
        }
    }
    out
}

// ------------ Launcher Runtime Files ------------
// The game also needs a few launcher runtime files published on a separate feed (Version.ini plus a manifest).
// These are added to the file list so they are installed with the game.
pub struct LauncherFeed {
    pub version: String,
    pub resources: Vec<Resource>,
}

impl LauncherFeed {
    pub fn download_bytes(&self) -> u64 {
        self.resources.iter().map(|r| r.download_size()).sum()
    }
}

fn launcher_bases(profile: &Value) -> Vec<String> {
    profile
        .get("nteLauncherUrls")
        .and_then(Value::as_array)
        .map(|urls| {
            urls.iter()
                .filter_map(|u| u.as_str())
                .filter(|u| !u.is_empty())
                .map(|u| u.trim_end_matches('/').to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn ini_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with(['#', ';']))
        .find_map(|line| {
            let (name, value) = line.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case(key)
                .then(|| value.trim().to_string())
        })
        .filter(|v| !v.is_empty())
}

pub fn parse_launcher_manifest(xml: &str) -> Result<LauncherFeed, String> {
    let mut base = String::new();
    let mut version = String::new();
    let mut resources = Vec::new();

    for (name, body, _, closing) in tags(xml) {
        if closing {
            continue;
        }
        match name {
            "Url" => {
                if let Some(v) = attribute(body, "BaseUrl") {
                    base = v.trim_end_matches('/').to_string();
                }
            }
            "ProductVersion" => {
                if let Some(v) = attribute(body, "Version") {
                    version = v.to_string();
                }
            }
            "File" => {
                let path = attribute(body, "Path")
                    .ok_or("NTE launcher manifest has a File without Path")?;
                let size =
                    number(body, "Size").ok_or("NTE launcher manifest has a File without Size")?;
                let md5 = attribute(body, "Checksum")
                    .ok_or("NTE launcher manifest has a File without Checksum")?
                    .to_lowercase();
                let zip_size = number(body, "ZipSize")
                    .ok_or("NTE launcher manifest has a File without ZipSize")?;
                let zip_md5 = attribute(body, "ZipChecksum")
                    .ok_or("NTE launcher manifest has a File without ZipChecksum")?
                    .to_lowercase();
                resources.push(Resource {
                    dest: format!("{LAUNCHER_DIR}/{}", normalise(path)),
                    size,
                    md5,
                    tag: LAUNCHER_TAG.to_string(),
                    optional: false,
                    object_md5: String::new(),
                    object_size: 0,
                    segments: Vec::new(),
                    archive: Some(Archive {
                        url: String::new(),
                        md5: zip_md5,
                        size: zip_size,
                    }),
                });
            }
            _ => {}
        }
    }

    if base.is_empty() || version.is_empty() {
        return Err("NTE launcher manifest has no BaseUrl or ProductVersion".to_string());
    }
    if resources.is_empty() {
        return Err("NTE launcher manifest listed no files".to_string());
    }

    for resource in &mut resources {
        let relative = resource
            .dest
            .strip_prefix(&format!("{LAUNCHER_DIR}/"))
            .unwrap_or(&resource.dest);
        if let Some(archive) = resource.archive.as_mut() {
            archive.url = format!("{base}/{version}/{relative}.zip");
        }
    }

    Ok(LauncherFeed { version, resources })
}

pub async fn fetch_launcher(profile: &Value) -> Result<LauncherFeed, String> {
    let bases = launcher_bases(profile);
    if bases.is_empty() {
        return Err("no nteLauncherUrls for profile".to_string());
    }

    let mut last = String::new();
    for base in &bases {
        let ini_url = format!("{base}/Version.ini");
        let ini = match http::get_text(&ini_url).await {
            Ok(text) => text,
            Err(e) => {
                log::warn!("nte: launcher feed {ini_url} failed: {e}");
                last = e;
                continue;
            }
        };
        let Some(list_url) = ini_value(&ini, "FileListURL") else {
            log::warn!("nte: launcher feed {ini_url} has no FileListURL");
            last = "NTE launcher Version.ini has no FileListURL".to_string();
            continue;
        };
        match http::get_text(&list_url).await {
            Ok(xml) => return parse_launcher_manifest(&xml),
            Err(e) => {
                log::warn!("nte: launcher manifest {list_url} failed: {e}");
                last = e;
            }
        }
    }
    Err(format!("Could not read the NTE launcher manifest: {last}"))
}

// ------------ Scanning Installed Files ------------
// Compares what is on disk with the resource list, either a quick size check or a full md5, and builds a plan of
// the files and segments that still need downloading.
pub struct Plan {
    pub fetch: Vec<Resource>,
    pub unchanged: usize,
    pub total_bytes: u64,
}

pub use super::progress::{FileEvent as Event, FileHooks as Hooks};

async fn wait_while_paused(hooks: &dyn Hooks) {
    super::progress::wait_while_paused(hooks).await;
}

async fn check_cancel_async(hooks: &dyn Hooks) -> Result<(), String> {
    if hooks.is_cancelled() {
        return Err("Download aborted by user.".to_string());
    }
    wait_while_paused(hooks).await;
    if hooks.is_cancelled() {
        return Err("Download aborted by user.".to_string());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanMode {
    Quick,
    Deep,
    Blocks,
}

enum ScanOutcome {
    Unchanged,
    Fetch(Box<Resource>, u64),
}

impl Plan {
    fn from_outcomes(outcomes: impl IntoIterator<Item = ScanOutcome>) -> Self {
        let mut plan = Plan {
            fetch: Vec::new(),
            unchanged: 0,
            total_bytes: 0,
        };
        for outcome in outcomes {
            match outcome {
                ScanOutcome::Unchanged => plan.unchanged += 1,
                ScanOutcome::Fetch(resource, bytes) => {
                    plan.total_bytes += bytes;
                    plan.fetch.push(*resource);
                }
            }
        }
        plan
    }
}

fn whole_fetch(resource: &Resource) -> ScanOutcome {
    ScanOutcome::Fetch(Box::new(resource.clone()), resource.download_size())
}

fn scan_resource(
    install_dir: &Path,
    resource: &Resource,
    mode: ScanMode,
    hooks: &dyn Hooks,
) -> Result<Option<ScanOutcome>, String> {
    let path = match super::fs_util::safe_join(install_dir, &resource.dest) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("nte: skipping a resource from the manifest: {e}");
            return Ok(None);
        }
    };
    let interrupted = has_pending(&path);
    let outcome = match mode {
        ScanMode::Quick if interrupted => {
            log::info!(
                "nte: {} was left partly written, checking its contents.",
                resource.dest
            );
            scan_blocks(&path, resource, hooks)?
        }
        ScanMode::Quick | ScanMode::Deep => {
            scan_whole(&path, resource, mode == ScanMode::Deep, hooks)?
        }
        ScanMode::Blocks => scan_blocks(&path, resource, hooks)?,
    };
    if interrupted && matches!(outcome, ScanOutcome::Unchanged) {
        clear_pending_blocking(&path);
    }
    hooks.event(Event::FileDone {
        path: &resource.dest,
    });
    Ok(Some(outcome))
}

fn scan_whole(
    path: &Path,
    resource: &Resource,
    deep: bool,
    hooks: &dyn Hooks,
) -> Result<ScanOutcome, String> {
    let matches = match std::fs::metadata(path) {
        Ok(meta) if meta.len() == resource.size => {
            if deep {
                unreadable_as_broken(file_md5(path, hooks), path, hooks)?
                    .is_some_and(|got| got == resource.md5)
            } else {
                true
            }
        }
        _ => false,
    };
    Ok(if matches {
        ScanOutcome::Unchanged
    } else {
        whole_fetch(resource)
    })
}

fn scan_blocks(path: &Path, resource: &Resource, hooks: &dyn Hooks) -> Result<ScanOutcome, String> {
    let Ok(current_len) = std::fs::metadata(path).map(|m| m.len()) else {
        return Ok(whole_fetch(resource));
    };
    let size_ok = current_len == resource.size;

    if resource.archive.is_some() || !segments_cover(resource) {
        if !size_ok {
            return Ok(whole_fetch(resource));
        }
        return scan_whole(path, resource, true, hooks);
    }

    let scanned = broken_segments(path, resource, current_len, hooks).map(|mut broken| {
        if broken.is_empty() && !size_ok {
            if let Some(last) = resource.segments.iter().max_by_key(|s| s.dst_start) {
                broken.push(last.clone());
            }
        }
        broken
    });
    Ok(
        match unreadable_as_broken(scanned, path, hooks)? {
            Some(broken) if broken.is_empty() && size_ok => ScanOutcome::Unchanged,
            Some(broken) if broken.is_empty() => whole_fetch(resource),
            Some(broken) => {
                let bytes: u64 = broken.iter().map(|s| s.size).sum();
                let mut partial = resource.clone();
                partial.segments = broken;
                ScanOutcome::Fetch(Box::new(partial), bytes)
            }
            None => whole_fetch(resource),
        },
    )
}

fn plan_sequential(
    install_dir: &Path,
    resources: &[Resource],
    mode: ScanMode,
    hooks: &dyn Hooks,
) -> Result<Plan, String> {
    let mut outcomes = Vec::with_capacity(resources.len());
    for resource in resources {
        super::progress::check_cancel(hooks, "Download aborted by user.")?;
        if let Some(outcome) = scan_resource(install_dir, resource, mode, hooks)? {
            outcomes.push(outcome);
        }
    }
    Ok(Plan::from_outcomes(outcomes))
}

pub fn plan_quick_scan(
    install_dir: &Path,
    resources: &[Resource],
    hooks: &dyn Hooks,
) -> Result<Plan, String> {
    plan_sequential(install_dir, resources, ScanMode::Quick, hooks)
}

pub fn plan_scan_parallel(
    install_dir: &Path,
    resources: &[Resource],
    mode: ScanMode,
    workers: usize,
    hooks: &dyn Hooks,
) -> Result<Plan, String> {
    let workers = workers.min(resources.len()).max(1);
    if workers == 1 {
        return plan_sequential(install_dir, resources, mode, hooks);
    }

    let cursor = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let first_error: Mutex<Option<String>> = Mutex::new(None);
    let mut collected: Vec<Vec<(usize, ScanOutcome)>> = Vec::with_capacity(workers);

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            handles.push(scope.spawn(|| {
                let mut local: Vec<(usize, ScanOutcome)> = Vec::new();
                loop {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let index = cursor.fetch_add(1, Ordering::SeqCst);
                    let Some(resource) = resources.get(index) else {
                        break;
                    };
                    let scanned = super::progress::check_cancel(hooks, "Download aborted by user.")
                        .and_then(|()| scan_resource(install_dir, resource, mode, hooks));
                    match scanned {
                        Ok(Some(outcome)) => local.push((index, outcome)),
                        Ok(None) => {}
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
                        *slot = Some("nte scan worker panicked".to_string());
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
    Ok(Plan::from_outcomes(
        merged.into_iter().map(|(_, outcome)| outcome),
    ))
}

fn unreadable_as_broken<T>(
    result: Result<T, String>,
    path: &Path,
    hooks: &dyn Hooks,
) -> Result<Option<T>, String> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if hooks.is_cancelled() => Err(e),
        Err(e) => {
            log::warn!(
                "nte: {} is unreadable, fetching it again: {e}",
                path.display()
            );
            Ok(None)
        }
    }
}

fn file_md5(path: &Path, hooks: &dyn Hooks) -> Result<String, String> {
    super::fs_util::md5_file(path, &mut || hooks.is_cancelled(), &mut |read| {
        hooks.event(Event::Bytes {
            path: "",
            delta: read,
        });
    })
}

fn segments_cover(resource: &Resource) -> bool {
    let mut segments: Vec<&Segment> = resource.segments.iter().collect();
    segments.sort_by_key(|s| s.dst_start);
    let mut expected = 0u64;
    for segment in segments {
        if segment.dst_start != expected {
            return false;
        }
        expected += segment.size;
    }
    expected == resource.size
}

fn broken_segments(
    path: &Path,
    resource: &Resource,
    len: u64,
    hooks: &dyn Hooks,
) -> Result<Vec<Segment>, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let mut ordered: Vec<&Segment> = resource.segments.iter().collect();
    ordered.sort_by_key(|s| s.dst_start);
    let mut broken: Vec<Segment> = Vec::new();
    let mut buf = vec![0u8; 1 << 20];
    for segment in ordered {
        super::progress::check_cancel(hooks, "Download aborted by user.")?;
        if segment.dst_start.saturating_add(segment.size) > len {
            broken.push(segment.clone());
            continue;
        }
        file.seek(std::io::SeekFrom::Start(segment.dst_start))
            .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        let mut hasher = Md5::new();
        let mut left = segment.size;
        while left > 0 {
            let want = (buf.len() as u64).min(left) as usize;
            file.read_exact(&mut buf[..want])
                .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
            hasher.update(&buf[..want]);
            left -= want as u64;
            hooks.event(Event::Bytes {
                path: &resource.dest,
                delta: want as u64,
            });
        }
        if hex::encode(hasher.finalize()) != segment.md5 {
            broken.push(segment.clone());
        }
    }
    Ok(broken)
}

// ------------ Downloading and Applying ------------
// Turns the plan into download jobs: big files are split into parts, and each part is written at the right offset
// and checked by md5. Failed jobs are retried and downloads pause while the network is down.
const SPLIT_MIN_BYTES: u64 = 256 << 20;
const PART_TARGET_BYTES: u64 = 128 << 20;
const MAX_PARTS_PER_SEGMENT: u64 = 8;
const READ_BACK_WINDOW: u64 = 64 << 20;
const FLUSH_BYTES: usize = 1 << 20;

fn split_segment(segment: &Segment) -> Vec<Segment> {
    if segment.size < SPLIT_MIN_BYTES {
        return Vec::new();
    }
    let count = segment
        .size
        .div_ceil(PART_TARGET_BYTES)
        .clamp(2, MAX_PARTS_PER_SEGMENT);
    let step = segment.size.div_ceil(count);
    let mut parts = Vec::new();
    let mut offset = 0u64;
    while offset < segment.size {
        let size = step.min(segment.size - offset);
        parts.push(Segment {
            src_start: segment.src_start + offset,
            dst_start: segment.dst_start + offset,
            size,
            md5: String::new(),
        });
        offset += size;
    }
    parts
}

struct PartGroup {
    segment: Segment,
    remaining: AtomicUsize,
}

enum Work {
    Range(Segment),
    Part {
        part: Segment,
        group: Arc<PartGroup>,
    },
    Archive {
        zip_md5: String,
        zip_size: u64,
        file_md5: String,
        file_size: u64,
    },
}

enum Source {
    Object { md5: String, size: u64 },
    Direct(String),
}

struct FileSlot {
    remaining: AtomicUsize,
    prepared: AtomicBool,
    preparing: Mutex<()>,
    file: Mutex<Option<Arc<std::fs::File>>>,
    size: Option<u64>,
}

impl FileSlot {
    fn new(jobs: usize, size: Option<u64>) -> Arc<Self> {
        Arc::new(Self {
            remaining: AtomicUsize::new(jobs),
            prepared: AtomicBool::new(false),
            preparing: Mutex::new(()),
            file: Mutex::new(None),
            size,
        })
    }

    fn finish_job(&self) -> bool {
        let last = self.remaining.fetch_sub(1, Ordering::AcqRel) == 1;
        if last {
            self.file.lock().take();
        }
        last
    }

    fn handle(&self) -> Result<Arc<std::fs::File>, String> {
        self.file
            .lock()
            .clone()
            .ok_or_else(|| "nte destination file was not prepared".to_string())
    }
}

struct Job {
    dest: PathBuf,
    path: String,
    source: Source,
    work: Work,
    slot: Arc<FileSlot>,
    before_write: super::sophon::BeforeWrite,
}

impl Job {
    fn url(&self, bases: &[String], attempt: u32) -> Result<String, String> {
        match &self.source {
            Source::Direct(url) => Ok(url.clone()),
            Source::Object { md5, size } => mirror(bases, attempt)
                .map(|base| object_url(base, md5, *size))
                .ok_or_else(|| "no nteResUrls for profile".to_string()),
        }
    }

    fn describe(&self, bases: &[String], attempt: u32) -> String {
        let url = self.url(bases, attempt).unwrap_or_default();
        match &self.work {
            Work::Range(segment) => format!(
                "{} from {url} at byte {} ({} bytes)",
                self.path, segment.src_start, segment.size
            ),
            Work::Part { part, .. } => format!(
                "{} from {url} at byte {} ({} bytes, part)",
                self.path, part.src_start, part.size
            ),
            Work::Archive { .. } => format!("{} from {url}", self.path),
        }
    }
}

fn mirror(bases: &[String], attempt: u32) -> Option<&str> {
    if bases.is_empty() {
        return None;
    }
    let index = attempt.saturating_sub(1) as usize % bases.len();
    Some(bases[index].as_str())
}

fn tmp_path(dest: &Path) -> PathBuf {
    let mut raw = dest.as_os_str().to_os_string();
    raw.push(".peebify-tmp");
    PathBuf::from(raw)
}

fn pending_marker(dest: &Path) -> PathBuf {
    let mut raw = dest.as_os_str().to_os_string();
    raw.push(".peebify-pending");
    PathBuf::from(raw)
}

fn has_pending(path: &Path) -> bool {
    pending_marker(path).exists()
}

async fn clear_pending(dest: &Path) {
    discard(&pending_marker(dest)).await;
}

fn clear_pending_blocking(path: &Path) {
    let marker = pending_marker(path);
    if let Err(e) = std::fs::remove_file(&marker) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("nte: could not remove {}: {e}", marker.display());
        }
    }
}

pub async fn apply(
    install_dir: &Path,
    plan: &Plan,
    bases: &[String],
    concurrency: usize,
    hooks: Arc<dyn Hooks>,
    cancelled: Arc<AtomicBool>,
    before_write: super::sophon::BeforeWrite,
) -> Result<(), String> {
    let mut jobs = Vec::new();
    for resource in &plan.fetch {
        let dest = super::fs_util::safe_join(install_dir, &resource.dest)?;

        if let Some(archive) = &resource.archive {
            jobs.push(Job {
                dest,
                path: resource.dest.clone(),
                source: Source::Direct(archive.url.clone()),
                work: Work::Archive {
                    zip_md5: archive.md5.clone(),
                    zip_size: archive.size,
                    file_md5: resource.md5.clone(),
                    file_size: resource.size,
                },
                slot: FileSlot::new(1, None),
                before_write: before_write.clone(),
            });
            continue;
        }

        if bases.is_empty() {
            return Err("no nteResUrls for profile".to_string());
        }
        let mut works = Vec::new();
        for segment in &resource.segments {
            let parts = split_segment(segment);
            if parts.is_empty() {
                works.push(Work::Range(segment.clone()));
                continue;
            }
            let group = Arc::new(PartGroup {
                segment: segment.clone(),
                remaining: AtomicUsize::new(parts.len()),
            });
            for part in parts {
                works.push(Work::Part {
                    part,
                    group: Arc::clone(&group),
                });
            }
        }
        let slot = FileSlot::new(works.len(), Some(resource.size));
        for work in works {
            jobs.push(Job {
                dest: dest.clone(),
                path: resource.dest.clone(),
                source: Source::Object {
                    md5: resource.object_md5.clone(),
                    size: resource.object_size,
                },
                work,
                slot: Arc::clone(&slot),
                before_write: before_write.clone(),
            });
        }
    }

    let bases: Arc<Vec<String>> = Arc::new(bases.to_vec());
    let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(jobs)));
    let failed = Arc::new(AtomicBool::new(false));
    let mut workers = Vec::new();
    for _ in 0..concurrency.max(1) {
        let queue = Arc::clone(&queue);
        let hooks = Arc::clone(&hooks);
        let cancelled = Arc::clone(&cancelled);
        let failed = Arc::clone(&failed);
        let bases = Arc::clone(&bases);
        workers.push(tauri::async_runtime::spawn(async move {
            loop {
                if cancelled.load(Ordering::SeqCst) {
                    return Err("Download aborted by user.".to_string());
                }
                if failed.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let job = queue.lock().pop_front();
                let Some(job) = job else {
                    return Ok(());
                };
                check_cancel_async(hooks.as_ref()).await?;
                let result = match prepare(&job).await {
                    Ok(()) => run_job(&job, &bases, hooks.as_ref()).await,
                    Err(e) => Err(e),
                };
                if let Err(e) = result {
                    failed.store(true, Ordering::SeqCst);
                    queue.lock().clear();
                    return Err(e);
                }
                if job.slot.finish_job() {
                    if job.slot.size.is_some() {
                        clear_pending(&job.dest).await;
                    }
                    hooks.event(Event::FileDone { path: &job.path });
                }
            }
        }));
    }

    super::progress::join_workers(workers, "nte worker").await
}

async fn prepare(job: &Job) -> Result<(), String> {
    if job.slot.prepared.load(Ordering::Acquire) {
        return Ok(());
    }
    let dest = job.dest.clone();
    let slot = Arc::clone(&job.slot);
    let before_write = job.before_write.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let _preparing = slot.preparing.lock();
        if slot.prepared.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
        }
        if let Some(size) = slot.size {
            if std::fs::metadata(&dest).map_or(true, |meta| meta.len() != size) {
                super::sophon::note_write(&before_write);
            }
            let marker = pending_marker(&dest);
            std::fs::File::create(&marker)
                .map_err(|e| format!("Could not create {}: {e}", marker.display()))?;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&dest)
                .map_err(|e| format!("Could not open {}: {e}", dest.display()))?;
            file.set_len(size).map_err(|e| {
                super::fs_util::explain_disk_full(&dest, size, format!("Could not size {}: {e}", dest.display()))
            })?;
            *slot.file.lock() = Some(Arc::new(file));
        }
        slot.prepared.store(true, Ordering::Release);
        Ok(())
    })
    .await
    .map_err(|e| format!("nte prepare task panicked: {e}"))?
}

struct ArchiveSpec<'a> {
    zip_md5: &'a str,
    zip_size: u64,
    file_md5: &'a str,
    file_size: u64,
}

async fn run_job(job: &Job, bases: &[String], hooks: &dyn Hooks) -> Result<(), String> {
    let describe = |attempt| job.describe(bases, attempt);
    match &job.work {
        Work::Range(segment) => fetch_segment(job, bases, segment, true, hooks).await,
        Work::Part { part, group } => {
            fetch_segment(job, bases, part, false, hooks).await?;
            if group.remaining.fetch_sub(1, Ordering::AcqRel) != 1 {
                return Ok(());
            }
            let segment = &group.segment;
            let got = read_back(&job.dest, segment.dst_start, segment.size, Md5::new(), hooks)
                .await
                .map(|hasher| hex::encode(hasher.finalize()))?;
            if got == segment.md5 {
                return Ok(());
            }
            log::warn!(
                "nte: {} at byte {} failed verification after its parts finished (got {got}, expected {}), fetching it again",
                job.path,
                segment.src_start,
                segment.md5
            );
            fetch_segment(job, bases, segment, true, hooks).await
        }
        Work::Archive {
            zip_md5,
            zip_size,
            file_md5,
            file_size,
        } => {
            let spec = ArchiveSpec {
                zip_md5,
                zip_size: *zip_size,
                file_md5,
                file_size: *file_size,
            };
            let tmp = tmp_path(&job.dest);
            let (spec, staged) = (&spec, tmp.as_path());
            with_job_retry(hooks, describe, |attempt| async move {
                let url = job.url(bases, attempt)?;
                let result = fetch_archive(job, &url, staged, spec, hooks).await;
                if result.is_err() {
                    discard(staged).await;
                }
                result
            })
            .await?;

            super::sophon::note_write(&job.before_write);
            let dest = job.dest.clone();
            let source = tmp.clone();
            let result = tauri::async_runtime::spawn_blocking(move || {
                super::fs_util::finalize_replace(&source, &dest)
            })
            .await
            .map_err(|e| format!("nte write task panicked: {e}"))?;
            if result.is_err() {
                discard(&tmp).await;
            }
            result
        }
    }
}

async fn fetch_segment(
    job: &Job,
    bases: &[String],
    segment: &Segment,
    verify: bool,
    hooks: &dyn Hooks,
) -> Result<(), String> {
    let file = job.slot.handle()?;
    let resume = std::sync::atomic::AtomicU64::new(0);
    let target = RangeTarget {
        file: &file,
        segment,
        verify,
        resume: &resume,
    };
    let target = &target;
    with_job_retry(
        hooks,
        |attempt| job.describe(bases, attempt),
        |attempt| async move {
            let url = job.url(bases, attempt)?;
            fetch_range(job, &url, target, hooks).await
        },
    )
    .await
}

async fn discard(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("nte: could not remove {}: {e}", path.display());
        }
    }
}

const JOB_MAX_RETRIES: u32 = 5;
const JOB_RETRY_BASE_MS: u64 = 500;
const OFFLINE_POLL: std::time::Duration = std::time::Duration::from_secs(3);
static OUTAGE: http::OutageGate = http::OutageGate::new();

async fn with_job_retry<D, F, Fut>(
    hooks: &dyn Hooks,
    describe: D,
    mut operation: F,
) -> Result<(), String>
where
    D: Fn(u32) -> String,
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    use super::fs_util::FailureKind;

    let mut attempt = 1u32;
    loop {
        let e = match operation(attempt).await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        let kind = super::fs_util::classify(&e);
        if hooks.is_cancelled()
            || matches!(
                kind,
                FailureKind::Cancelled
                    | FailureKind::DiskFull
                    | FailureKind::Locked
                    | FailureKind::AccessDenied
            )
        {
            return Err(e);
        }
        if kind == FailureKind::Network && !http::is_online().await {
            OUTAGE
                .single_flight(|| async {
                    log::warn!(
                        "nte {} failed while offline, waiting for the connection: {e}",
                        describe(attempt)
                    );
                    http::note_unreachable();
                    hooks.status(super::sophon::STATUS_OFFLINE);
                    while !hooks.is_cancelled() {
                        tokio::time::sleep(OFFLINE_POLL).await;
                        if http::is_online().await {
                            break;
                        }
                    }
                    if hooks.is_cancelled() {
                        return;
                    }
                    log::info!("nte: connection restored, resuming.");
                    http::note_reachable();
                    hooks.status("");
                })
                .await;
            if hooks.is_cancelled() {
                return Err("Download aborted by user.".to_string());
            }
            continue;
        }
        if attempt >= JOB_MAX_RETRIES {
            return Err(e);
        }
        let delay = http::retry_delay(attempt, JOB_RETRY_BASE_MS);
        log::warn!(
            "nte {} failed (attempt {attempt}/{JOB_MAX_RETRIES}): {e}, retrying in {}ms",
            describe(attempt),
            delay.as_millis()
        );
        tokio::time::sleep(delay).await;
        attempt += 1;
    }
}

// ------------ Streaming Unpack ------------
// Some files are stored as zip archives on the server. This inflates them as the bytes arrive, so nothing is
// saved as an intermediate zip.
enum UnpackState {
    Header(Vec<u8>),
    Stored(u64),
    Deflate(u64, Box<flate2::write::DeflateDecoder<Vec<u8>>>),
    Trailer,
}

struct Unpack {
    state: UnpackState,
    limit: u64,
    produced: u64,
    out: Vec<u8>,
    hasher: Md5,
}

fn local_header(buffer: &[u8]) -> Result<Option<(u16, u64, usize)>, String> {
    if buffer.len() < 4 {
        return Ok(None);
    }
    if buffer[..4] != [0x50, 0x4b, 0x03, 0x04] {
        return Err("zip archive has no local file header".to_string());
    }
    if buffer.len() < 30 {
        return Ok(None);
    }
    let read_u16 = |at: usize| u16::from_le_bytes([buffer[at], buffer[at + 1]]);
    let flags = read_u16(6);
    let method = read_u16(8);
    let compressed = u32::from_le_bytes([buffer[18], buffer[19], buffer[20], buffer[21]]);
    let data_at = 30 + read_u16(26) as usize + read_u16(28) as usize;
    if flags & 0x0008 != 0 {
        return Err("zip entry uses a data descriptor".to_string());
    }
    if method != 0 && method != 8 {
        return Err(format!("zip entry uses unsupported method {method}"));
    }
    if buffer.len() < data_at {
        return Ok(None);
    }
    Ok(Some((method, u64::from(compressed), data_at)))
}

impl Unpack {
    fn new(limit: u64) -> Self {
        Self {
            state: UnpackState::Header(Vec::new()),
            limit,
            produced: 0,
            out: Vec::new(),
            hasher: Md5::new(),
        }
    }

    fn feed(&mut self, input: &[u8]) -> Result<(), String> {
        use std::io::Write;

        let mut input = input;
        let rest;
        if let UnpackState::Header(buffer) = &mut self.state {
            buffer.extend_from_slice(input);
            let Some((method, compressed, data_at)) = local_header(buffer)? else {
                return Ok(());
            };
            rest = buffer.split_off(data_at);
            input = &rest;
            self.state = if method == 0 {
                UnpackState::Stored(compressed)
            } else {
                UnpackState::Deflate(
                    compressed,
                    Box::new(flate2::write::DeflateDecoder::new(Vec::new())),
                )
            };
        }

        let start = self.out.len();
        let done = match &mut self.state {
            UnpackState::Stored(left) => {
                let take = (*left).min(input.len() as u64);
                self.out.extend_from_slice(&input[..take as usize]);
                *left -= take;
                *left == 0
            }
            UnpackState::Deflate(left, decoder) => {
                let take = (*left).min(input.len() as u64);
                decoder
                    .write_all(&input[..take as usize])
                    .map_err(|e| format!("zip entry is not valid deflate: {e}"))?;
                *left -= take;
                if *left == 0 {
                    decoder
                        .try_finish()
                        .map_err(|e| format!("zip entry is not valid deflate: {e}"))?;
                }
                self.out.append(decoder.get_mut());
                *left == 0
            }
            UnpackState::Header(_) | UnpackState::Trailer => false,
        };
        if done {
            self.state = UnpackState::Trailer;
        }

        let fresh = &self.out[start..];
        self.produced += fresh.len() as u64;
        if self.produced > self.limit {
            return Err("zip entry unpacked to more bytes than expected".to_string());
        }
        self.hasher.update(fresh);
        Ok(())
    }

    fn pending(&self) -> usize {
        self.out.len()
    }

    fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    fn restore(&mut self, mut spare: Vec<u8>) {
        if self.out.is_empty() {
            spare.clear();
            self.out = spare;
        }
    }

    fn finish(&self) -> Result<(), String> {
        match self.state {
            UnpackState::Trailer => Ok(()),
            UnpackState::Header(_) => Err("zip archive has no entry".to_string()),
            UnpackState::Stored(_) | UnpackState::Deflate(..) => {
                Err("zip archive is truncated".to_string())
            }
        }
    }

    fn digest(self) -> String {
        hex::encode(self.hasher.finalize())
    }
}

async fn write_at(
    file: &Arc<std::fs::File>,
    dest: &Path,
    offset: u64,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let file = Arc::clone(file);
    tauri::async_runtime::spawn_blocking(move || {
        super::fs_util::write_all_at(&file, &bytes, offset).map(|()| bytes)
    })
    .await
    .map_err(|e| format!("nte write task panicked: {e}"))?
    .map_err(|e| format!("Could not write {}: {e}", dest.display()))
}

async fn create_file(path: &Path) -> Result<Arc<std::fs::File>, String> {
    let owned = path.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || std::fs::File::create(&owned))
        .await
        .map_err(|e| format!("nte write task panicked: {e}"))?
        .map(Arc::new)
        .map_err(|e| format!("Could not open {}: {e}", path.display()))
}

fn hash_window(path: &Path, offset: u64, len: u64, hasher: &mut Md5) -> std::io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    file.seek(std::io::SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len.min(FLUSH_BYTES as u64) as usize];
    let mut left = len;
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        file.read_exact(&mut buf[..want])?;
        hasher.update(&buf[..want]);
        left -= want as u64;
    }
    Ok(())
}

async fn read_back(
    path: &Path,
    offset: u64,
    len: u64,
    mut hasher: Md5,
    hooks: &dyn Hooks,
) -> Result<Md5, String> {
    let mut done = 0u64;
    while done < len {
        check_cancel_async(hooks).await?;
        let window = READ_BACK_WINDOW.min(len - done);
        let owned = path.to_path_buf();
        let at = offset + done;
        hasher = tauri::async_runtime::spawn_blocking(move || {
            hash_window(&owned, at, window, &mut hasher).map(|()| hasher)
        })
        .await
        .map_err(|e| format!("nte read task panicked: {e}"))?
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        done += window;
    }
    Ok(hasher)
}

async fn fetch_archive(
    job: &Job,
    url: &str,
    tmp: &Path,
    spec: &ArchiveSpec<'_>,
    hooks: &dyn Hooks,
) -> Result<(), String> {
    use futures::StreamExt;

    let response = http::download_client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Request error: {e}"))?;
    let status = response.status().as_u16();
    if !(status == 200 || status == 206) {
        return Err(format!("HTTP Error: {status} for URL {url}"));
    }

    let file = create_file(tmp).await?;
    let mut zip_hasher = Md5::new();
    let mut unpack = Unpack::new(spec.file_size);
    let mut received: u64 = 0;
    let mut written: u64 = 0;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if hooks.is_cancelled() {
            return Err("Download aborted by user.".to_string());
        }
        wait_while_paused(hooks).await;
        let chunk = chunk.map_err(|e| format!("Stream error: {e}"))?;
        received += chunk.len() as u64;
        if received > spec.zip_size {
            return Err(format!("{} returned more bytes than expected", job.path));
        }
        zip_hasher.update(&chunk);
        unpack
            .feed(&chunk)
            .map_err(|e| format!("{}: {e}", job.path))?;
        hooks.event(Event::Bytes {
            path: &job.path,
            delta: chunk.len() as u64,
        });
        if unpack.pending() >= FLUSH_BYTES {
            let out = unpack.take();
            let len = out.len() as u64;
            let spare = write_at(&file, tmp, written, out).await?;
            unpack.restore(spare);
            written += len;
        }
    }

    if received != spec.zip_size {
        return Err(format!(
            "{} returned {received} bytes, expected {}",
            job.path, spec.zip_size
        ));
    }
    let got = hex::encode(zip_hasher.finalize());
    if got != spec.zip_md5 {
        return Err(format!(
            "{} failed verification (got {got}, expected {})",
            job.path, spec.zip_md5
        ));
    }
    unpack.finish().map_err(|e| format!("{}: {e}", job.path))?;
    let out = unpack.take();
    if !out.is_empty() {
        let len = out.len() as u64;
        write_at(&file, tmp, written, out).await?;
        written += len;
    }
    if written != spec.file_size {
        return Err(format!(
            "{} unpacked to {written} bytes, expected {}",
            job.path, spec.file_size
        ));
    }
    if spec.file_size > 0 {
        let got = unpack.digest();
        if got != spec.file_md5 {
            return Err(format!(
                "{} failed verification after unpacking (got {got}, expected {})",
                job.path, spec.file_md5
            ));
        }
    }
    Ok(())
}

struct RangeTarget<'a> {
    file: &'a Arc<std::fs::File>,
    segment: &'a Segment,
    verify: bool,
    resume: &'a std::sync::atomic::AtomicU64,
}

fn resumed_at(content_range: Option<&str>, start: u64) -> bool {
    content_range.is_some_and(|v| v.trim_start().starts_with(&format!("bytes {start}-")))
}

fn body_is_segment(segment: &Segment, content_length: Option<u64>) -> bool {
    segment.src_start == 0 && content_length == Some(segment.size)
}

async fn fetch_range(
    job: &Job,
    url: &str,
    target: &RangeTarget<'_>,
    hooks: &dyn Hooks,
) -> Result<(), String> {
    use futures::StreamExt;

    let segment = target.segment;
    if segment.size == 0 {
        return Ok(());
    }
    let mut written = target.resume.load(Ordering::SeqCst).min(segment.size);
    let mut hasher = Md5::new();
    if target.verify && written > 0 {
        match read_back(&job.dest, segment.dst_start, written, Md5::new(), hooks).await {
            Ok(seeded) => hasher = seeded,
            Err(e) if hooks.is_cancelled() => return Err(e),
            Err(e) => {
                log::warn!(
                    "nte: could not reuse {written} bytes of {}, restarting the segment: {e}",
                    job.path
                );
                written = 0;
                target.resume.store(0, Ordering::SeqCst);
            }
        }
    }

    if written < segment.size {
        let start = segment.src_start + written;
        let last = segment.src_start + segment.size - 1;
        let response = http::download_client()
            .get(url)
            .header("Range", format!("bytes={start}-{last}"))
            .send()
            .await
            .map_err(|e| format!("Request error: {e}"))?;
        let status = response.status().as_u16();
        let whole_body =
            status == 200 && body_is_segment(segment, response.content_length());
        if written > 0 && status == 206 {
            let content_range = response
                .headers()
                .get("Content-Range")
                .and_then(|v| v.to_str().ok());
            if !resumed_at(content_range, start) {
                target.resume.store(0, Ordering::SeqCst);
                return Err(format!(
                    "Server sent an unexpected Content-Range for {} at byte {start}",
                    job.path
                ));
            }
        } else if written > 0 && whole_body {
            log::info!(
                "nte: server ignored the resume range for {}, restarting the segment.",
                job.path
            );
            written = 0;
            hasher = Md5::new();
            target.resume.store(0, Ordering::SeqCst);
        } else if status == 200 && !whole_body {
            return Err(format!(
                "The download server sent the whole file instead of the requested part of {} ({url}).",
                job.path
            ));
        } else if !(status == 206 || whole_body) {
            return Err(format!("HTTP Error: {status} for URL {url}"));
        }

        let mut stream = response.bytes_stream();
        let mut buffer: Vec<u8> =
            Vec::with_capacity(((segment.size - written) as usize).min(FLUSH_BYTES));

        while let Some(chunk) = stream.next().await {
            if hooks.is_cancelled() {
                return Err("Download aborted by user.".to_string());
            }
            wait_while_paused(hooks).await;
            let chunk = chunk.map_err(|e| format!("Stream error: {e}"))?;
            if written + buffer.len() as u64 + chunk.len() as u64 > segment.size {
                target.resume.store(0, Ordering::SeqCst);
                return Err(format!("{} returned more bytes than expected", job.path));
            }
            if target.verify {
                hasher.update(&chunk);
            }
            buffer.extend_from_slice(&chunk);
            hooks.event(Event::Bytes {
                path: &job.path,
                delta: chunk.len() as u64,
            });
            if buffer.len() >= FLUSH_BYTES {
                let len = buffer.len() as u64;
                super::sophon::note_write(&job.before_write);
                buffer = write_at(target.file, &job.dest, segment.dst_start + written, buffer)
                    .await?;
                buffer.clear();
                written += len;
                target.resume.store(written, Ordering::SeqCst);
            }
        }

        if !buffer.is_empty() {
            let len = buffer.len() as u64;
            super::sophon::note_write(&job.before_write);
            write_at(target.file, &job.dest, segment.dst_start + written, buffer).await?;
            written += len;
            target.resume.store(written, Ordering::SeqCst);
        }
    }
    if written != segment.size {
        return Err(format!(
            "{} returned {written} bytes, expected {}",
            job.path, segment.size
        ));
    }
    if target.verify {
        let got = hex::encode(hasher.finalize());
        if got != segment.md5 {
            target.resume.store(0, Ordering::SeqCst);
            return Err(format!(
                "{} failed verification (got {got}, expected {})",
                job.path, segment.md5
            ));
        }
    }
    Ok(())
}

// ------------ Downloader Tests ------------
// Unit tests for the streaming unpack, segment splitting and range resume handling.
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct NoHooks {
        cancelled: bool,
    }

    impl super::super::progress::Control for NoHooks {
        fn is_cancelled(&self) -> bool {
            self.cancelled
        }
    }

    impl Hooks for NoHooks {
        fn event(&self, _event: Event) {}
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "peebify-nte-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn zip_with(method: u16, flags: u16, name: &str, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x50, 0x4b, 0x03, 0x04, 20, 0];
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&3u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&[1, 2, 3]);
        out.extend_from_slice(payload);
        out.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02, 9, 9, 9, 9]);
        out
    }

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn unpack_in_chunks(
        archive: &[u8],
        limit: u64,
        chunk: usize,
    ) -> Result<(Vec<u8>, String), String> {
        let mut unpack = Unpack::new(limit);
        let mut output = Vec::new();
        for piece in archive.chunks(chunk) {
            unpack.feed(piece)?;
            output.extend(unpack.take());
        }
        unpack.finish()?;
        output.extend(unpack.take());
        Ok((output, unpack.digest()))
    }

    fn sample() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn unpack_streams_a_deflated_entry_across_small_chunks() {
        let data = sample();
        let archive = zip_with(8, 0, "libcef.dll", &deflate(&data));
        for chunk in [1, 7, 4096, archive.len()] {
            let (output, digest) = unpack_in_chunks(&archive, data.len() as u64, chunk).unwrap();
            assert_eq!(output, data);
            assert_eq!(digest, md5_hex(&data));
        }
    }

    #[test]
    fn unpack_copies_a_stored_entry() {
        let data = b"stored payload".to_vec();
        let archive = zip_with(0, 0, "a.txt", &data);
        let (output, digest) = unpack_in_chunks(&archive, data.len() as u64, 5).unwrap();
        assert_eq!(output, data);
        assert_eq!(digest, md5_hex(&data));
    }

    #[test]
    fn unpack_accepts_an_empty_stored_entry() {
        let archive = zip_with(0, 0, "empty.txt", &[]);
        let (output, _) = unpack_in_chunks(&archive, 0, 3).unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn unpack_rejects_output_past_the_expected_size() {
        let data = sample();
        let archive = zip_with(8, 0, "big.bin", &deflate(&data));
        let err = unpack_in_chunks(&archive, data.len() as u64 - 1, 4096).unwrap_err();
        assert!(err.contains("more bytes than expected"));
    }

    #[test]
    fn unpack_rejects_data_descriptors_and_unknown_methods() {
        let described = zip_with(8, 0x0008, "a.bin", &deflate(b"x"));
        assert!(unpack_in_chunks(&described, 10, 64)
            .unwrap_err()
            .contains("data descriptor"));
        let lzma = zip_with(14, 0, "a.bin", b"x");
        assert!(unpack_in_chunks(&lzma, 10, 64)
            .unwrap_err()
            .contains("unsupported method 14"));
        assert!(unpack_in_chunks(b"not a zip", 10, 64).is_err());
    }

    #[test]
    fn unpack_reports_a_truncated_entry() {
        let data = sample();
        let archive = zip_with(8, 0, "cut.bin", &deflate(&data));
        let cut = &archive[..archive.len() / 2];
        assert!(unpack_in_chunks(cut, data.len() as u64, 4096)
            .unwrap_err()
            .contains("truncated"));
    }

    #[test]
    fn mirror_rotates_through_bases_per_attempt() {
        let bases = vec!["a".to_string(), "b".to_string()];
        assert_eq!(mirror(&bases, 1), Some("a"));
        assert_eq!(mirror(&bases, 2), Some("b"));
        assert_eq!(mirror(&bases, 3), Some("a"));
        assert_eq!(mirror(&[], 1), None);
    }

    #[test]
    fn file_slot_reports_done_only_after_the_last_segment() {
        let slot = FileSlot::new(3, Some(10));
        assert!(!slot.finish_job());
        assert!(!slot.finish_job());
        assert!(slot.finish_job());
    }

    #[test]
    fn file_slot_releases_its_handle_after_the_last_job() {
        let dir = TempDir::new("slot-handle");
        let slot = FileSlot::new(2, Some(4));
        assert!(slot.handle().is_err());
        let file = std::fs::File::create(dir.0.join("a.pak")).unwrap();
        *slot.file.lock() = Some(Arc::new(file));
        assert!(slot.handle().is_ok());
        assert!(!slot.finish_job());
        assert!(slot.handle().is_ok());
        assert!(slot.finish_job());
        assert!(slot.handle().is_err());
    }

    fn segment(src_start: u64, dst_start: u64, size: u64) -> Segment {
        Segment {
            src_start,
            dst_start,
            size,
            md5: "ab".to_string(),
        }
    }

    #[test]
    fn small_segments_are_not_split() {
        assert!(split_segment(&segment(0, 0, SPLIT_MIN_BYTES - 1)).is_empty());
    }

    #[test]
    fn large_segments_split_into_contiguous_parts() {
        let whole = segment(1000, 50, SPLIT_MIN_BYTES * 3 + 7);
        let parts = split_segment(&whole);
        assert_eq!(parts.len(), 7);
        assert!(parts.iter().all(|p| p.md5.is_empty()));
        assert_eq!(parts.iter().map(|p| p.size).sum::<u64>(), whole.size);
        let mut src = whole.src_start;
        let mut dst = whole.dst_start;
        for part in &parts {
            assert_eq!(part.src_start, src);
            assert_eq!(part.dst_start, dst);
            src += part.size;
            dst += part.size;
        }
    }

    #[test]
    fn huge_segments_cap_the_part_count() {
        let parts = split_segment(&segment(0, 0, 10 << 30));
        assert_eq!(parts.len() as u64, MAX_PARTS_PER_SEGMENT);
        assert_eq!(parts.iter().map(|p| p.size).sum::<u64>(), 10 << 30);
    }

    #[test]
    fn resume_accepts_only_a_range_starting_where_it_left_off() {
        assert!(resumed_at(Some("bytes 4096-8191/8192"), 4096));
        assert!(resumed_at(Some(" bytes 4096-8191/*"), 4096));
        assert!(!resumed_at(Some("bytes 0-8191/8192"), 4096));
        assert!(!resumed_at(Some("bytes 40960-81919/81920"), 4096));
        assert!(!resumed_at(None, 4096));
    }

    #[test]
    fn full_body_stands_in_only_for_a_whole_object_segment() {
        assert!(body_is_segment(&segment(0, 0, 4096), Some(4096)));
        assert!(!body_is_segment(&segment(0, 0, 4096), Some(8192)));
        assert!(!body_is_segment(&segment(0, 0, 4096), None));
        assert!(!body_is_segment(&segment(4096, 0, 4096), Some(4096)));
    }

    #[test]
    fn quick_scan_checks_a_file_left_mid_download() {
        let temp = TempDir::new("pending");
        let wanted = two_block_resource("big.pak", b"first block", b"second block");
        let path = temp.0.join("big.pak");
        let hooks = NoHooks { cancelled: false };

        std::fs::write(&path, b"first block\0\0\0\0\0\0\0\0\0\0\0\0").unwrap();
        std::fs::write(pending_marker(&path), b"").unwrap();
        let plan = plan_quick_scan(&temp.0, std::slice::from_ref(&wanted), &hooks).unwrap();
        assert_eq!(plan.fetch.len(), 1);
        assert_eq!(plan.fetch[0].segments, vec![wanted.segments[1].clone()]);
        assert!(has_pending(&path));

        std::fs::write(&path, b"first blocksecond block").unwrap();
        let plan = plan_quick_scan(&temp.0, std::slice::from_ref(&wanted), &hooks).unwrap();
        assert_eq!(plan.unchanged, 1);
        assert!(!has_pending(&path));

        std::fs::write(&path, b"first block\0\0\0\0\0\0\0\0\0\0\0\0").unwrap();
        let plan = plan_quick_scan(&temp.0, std::slice::from_ref(&wanted), &hooks).unwrap();
        assert_eq!(plan.unchanged, 1, "without a marker the quick scan trusts the size");
    }

    #[test]
    fn hash_window_matches_the_bytes_on_disk() {
        let dir = TempDir::new("hash-window");
        let path = dir.0.join("a.pak");
        let data: Vec<u8> = (0..(FLUSH_BYTES * 2 + 17)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let mut hasher = Md5::new();
        hash_window(&path, 5, 100, &mut hasher).unwrap();
        hash_window(&path, 105, data.len() as u64 - 105, &mut hasher).unwrap();
        assert_eq!(hex::encode(hasher.finalize()), md5_hex(&data[5..]));
        assert!(hash_window(&path, 0, data.len() as u64 + 1, &mut Md5::new()).is_err());
    }

    #[test]
    fn unpack_reuses_a_restored_buffer() {
        let data = sample();
        let archive = zip_with(0, 0, "a", &data);
        let mut unpack = Unpack::new(data.len() as u64);
        unpack.feed(&archive[..archive.len() / 2]).unwrap();
        let first = unpack.take();
        let capacity = first.capacity();
        let mut output = first.clone();
        unpack.restore(first);
        assert_eq!(unpack.pending(), 0);
        unpack.feed(&archive[archive.len() / 2..]).unwrap();
        assert!(unpack.out.capacity() >= capacity);
        output.extend(unpack.take());
        unpack.finish().unwrap();
        assert_eq!(output, data);
    }

    #[test]
    fn optional_flag_follows_the_tag_not_the_nesting() {
        let xml = r#"<ResList version="7">
            <Res filename="Base/a.pak" filesize="1" md5="AA"/>
            <BaseVersion>
                <Res filename="Base/b.pak" filesize="2" md5="BB"/>
            </BaseVersion>
            <BaseVersion tag="baseTag">
                <Res filename="Base/c.pak" filesize="3" md5="CC"/>
            </BaseVersion>
            <BaseVersion tag="pakchunk102">
                <Res filename="Voice/en.pak" filesize="4" md5="DD"/>
                <BaseVersion>
                    <Res filename="Voice/en2.pak" filesize="5" md5="EE"/>
                </BaseVersion>
            </BaseVersion>
        </ResList>"#;
        let list = parse_reslist(xml).unwrap();
        let flags: Vec<(&str, &str, bool)> = list
            .resources
            .iter()
            .map(|r| (r.dest.as_str(), r.tag.as_str(), r.optional))
            .collect();
        assert_eq!(
            flags,
            vec![
                ("Base/a.pak", BASE_TAG, false),
                ("Base/b.pak", BASE_TAG, false),
                ("Base/c.pak", BASE_TAG, false),
                ("Voice/en.pak", "pakchunk102", true),
                ("Voice/en2.pak", "pakchunk102", true),
            ]
        );
        let none: Vec<String> = Vec::new();
        assert_eq!(list.selected(Some(&none)).count(), 3);
    }

    fn resource(dest: &str, data: &[u8]) -> Resource {
        let md5 = md5_hex(data);
        Resource {
            dest: dest.to_string(),
            size: data.len() as u64,
            md5: md5.clone(),
            tag: BASE_TAG.to_string(),
            optional: false,
            object_md5: md5.clone(),
            object_size: data.len() as u64,
            segments: vec![Segment {
                src_start: 0,
                dst_start: 0,
                size: data.len() as u64,
                md5,
            }],
            archive: None,
        }
    }

    #[cfg(windows)]
    #[test]
    fn scans_treat_an_unreadable_file_as_broken() {
        use std::os::windows::fs::OpenOptionsExt;

        let temp = TempDir::new("locked");
        let good = resource("good.bin", b"good bytes");
        let locked = resource("locked.bin", b"locked bytes");
        std::fs::write(temp.0.join("good.bin"), b"good bytes").unwrap();
        std::fs::write(temp.0.join("locked.bin"), b"locked bytes").unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(temp.0.join("locked.bin"))
            .unwrap();
        let resources = vec![good, locked];
        let hooks = NoHooks { cancelled: false };

        let plan = plan_sequential(&temp.0, &resources, ScanMode::Deep, &hooks).unwrap();
        assert_eq!(plan.unchanged, 1);
        assert_eq!(plan.fetch.len(), 1);
        assert_eq!(plan.fetch[0].dest, "locked.bin");
        assert_eq!(plan.total_bytes, 12);

        let blocks = plan_sequential(&temp.0, &resources, ScanMode::Blocks, &hooks).unwrap();
        assert_eq!(blocks.unchanged, 1);
        assert_eq!(blocks.fetch.len(), 1);
        assert_eq!(blocks.fetch[0].segments, resources[1].segments);
        drop(lock);
    }

    #[test]
    fn scans_still_stop_on_cancel() {
        let temp = TempDir::new("cancel");
        std::fs::write(temp.0.join("a.bin"), b"abc").unwrap();
        let resources = vec![resource("a.bin", b"abc")];
        let hooks = NoHooks { cancelled: true };
        assert!(plan_sequential(&temp.0, &resources, ScanMode::Deep, &hooks).is_err());
        assert!(plan_sequential(&temp.0, &resources, ScanMode::Blocks, &hooks).is_err());
        assert!(plan_scan_parallel(&temp.0, &resources, ScanMode::Blocks, 4, &hooks).is_err());
    }

    fn two_block_resource(dest: &str, first: &[u8], second: &[u8]) -> Resource {
        let whole = [first, second].concat();
        let mut r = resource(dest, &whole);
        r.segments = vec![
            Segment {
                src_start: 0,
                dst_start: 0,
                size: first.len() as u64,
                md5: md5_hex(first),
            },
            Segment {
                src_start: first.len() as u64,
                dst_start: first.len() as u64,
                size: second.len() as u64,
                md5: md5_hex(second),
            },
        ];
        r
    }

    #[test]
    fn parallel_scan_keeps_manifest_order_and_matches_sequential() {
        let temp = TempDir::new("parallel");
        let mut resources = Vec::new();
        for i in 0..24 {
            let dest = format!("f{i:02}.bin");
            let data = format!("payload {i}").into_bytes();
            let on_disk = if i % 3 == 0 {
                format!("PAYLOAD {i}").into_bytes()
            } else {
                data.clone()
            };
            if i % 7 != 0 {
                std::fs::write(temp.0.join(&dest), &on_disk).unwrap();
            }
            resources.push(resource(&dest, &data));
        }
        let hooks = NoHooks { cancelled: false };
        for mode in [ScanMode::Deep, ScanMode::Blocks] {
            let sequential = plan_sequential(&temp.0, &resources, mode, &hooks).unwrap();
            let parallel = plan_scan_parallel(&temp.0, &resources, mode, 4, &hooks).unwrap();
            let names = |p: &Plan| p.fetch.iter().map(|r| r.dest.clone()).collect::<Vec<_>>();
            assert_eq!(names(&parallel), names(&sequential));
            assert_eq!(parallel.unchanged, sequential.unchanged);
            assert_eq!(parallel.total_bytes, sequential.total_bytes);
            let mut sorted = names(&parallel);
            sorted.sort();
            assert_eq!(names(&parallel), sorted);
            assert_eq!(parallel.unchanged + parallel.fetch.len(), resources.len());
        }
    }

    #[test]
    fn block_scan_fetches_only_the_changed_block() {
        let temp = TempDir::new("blocks");
        let wanted = two_block_resource("big.pak", b"first block", b"second block");
        std::fs::write(temp.0.join("big.pak"), b"first blockSECOND BLOCK").unwrap();
        let hooks = NoHooks { cancelled: false };
        let plan =
            plan_scan_parallel(&temp.0, std::slice::from_ref(&wanted), ScanMode::Blocks, 4, &hooks)
                .unwrap();
        assert_eq!(plan.fetch.len(), 1);
        assert_eq!(plan.fetch[0].segments, vec![wanted.segments[1].clone()]);
        assert_eq!(plan.total_bytes, 12);

        let whole = plan_sequential(
            &temp.0,
            std::slice::from_ref(&wanted),
            ScanMode::Deep,
            &hooks,
        )
        .unwrap();
        assert_eq!(whole.fetch[0].segments.len(), 2);
        assert_eq!(whole.total_bytes, wanted.size);
    }

    #[test]
    fn block_scan_reuses_matching_blocks_of_a_file_that_grew() {
        let temp = TempDir::new("grew");
        let wanted = two_block_resource("big.pak", b"first block", b"second block");
        let hooks = NoHooks { cancelled: false };
        std::fs::write(temp.0.join("big.pak"), b"first blocksec").unwrap();
        let plan =
            plan_sequential(&temp.0, std::slice::from_ref(&wanted), ScanMode::Blocks, &hooks)
                .unwrap();
        assert_eq!(plan.fetch.len(), 1);
        assert_eq!(plan.fetch[0].segments, vec![wanted.segments[1].clone()]);
        assert_eq!(plan.total_bytes, 12);

        std::fs::write(temp.0.join("big.pak"), b"FIRST BLOCKsec").unwrap();
        let plan =
            plan_sequential(&temp.0, std::slice::from_ref(&wanted), ScanMode::Blocks, &hooks)
                .unwrap();
        assert_eq!(plan.fetch[0].segments, wanted.segments);
    }

    #[test]
    fn block_scan_still_trims_a_file_that_shrank() {
        let temp = TempDir::new("shrank");
        let wanted = two_block_resource("big.pak", b"first block", b"second block");
        let hooks = NoHooks { cancelled: false };
        std::fs::write(temp.0.join("big.pak"), b"first blocksecond block and more").unwrap();
        let plan =
            plan_sequential(&temp.0, std::slice::from_ref(&wanted), ScanMode::Blocks, &hooks)
                .unwrap();
        assert_eq!(plan.unchanged, 0);
        assert_eq!(plan.fetch.len(), 1);
        assert_eq!(plan.fetch[0].segments, vec![wanted.segments[1].clone()]);

        std::fs::remove_file(temp.0.join("big.pak")).unwrap();
        let plan =
            plan_sequential(&temp.0, std::slice::from_ref(&wanted), ScanMode::Blocks, &hooks)
                .unwrap();
        assert_eq!(plan.fetch[0].segments, wanted.segments);
        assert_eq!(plan.total_bytes, wanted.size);
    }
}
