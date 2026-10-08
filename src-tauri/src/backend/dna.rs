// ------------ Duet Night Abyss ------------
// Talks to Pan Studio's CDN for Duet Night Abyss. A build ships as one HDiffPatch directory archive (.hdiff): a full archive unpacks the game
// from nothing, a diff archive patches an older build in place. Both are applied by the bundled hpatchz. The CDN also lists every file's MD5
// per build (Hash.json), which is what verification checks, and the official launcher's banner and notice feeds, which drive the news panel.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use super::fs_util;
use super::game_profiles;
use super::http;
use super::news::{article, as_i64, as_str, envelope, local_month_day, slide, MAX_ITEMS, MAX_SLIDES};

/// The official launcher's version record in the game folder: `{"version": 16001}`.
pub const VERSION_FILE: &str = "GameVersion.json";
/// Where Peebify keeps a downloading archive inside the game folder. hpatchz ignores it when patching in place.
pub const PATCH_DIR: &str = "PeebifyPatch";
pub const HPATCHZ: &str = "hpatchz.exe";

fn profile_str<'a>(profile: &'a Value, key: &str) -> &'a str {
    profile.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

/// The download mirrors, fastest first in the official launcher's own speed test. Only https mirrors are used.
pub fn cdns(profile: &Value) -> Vec<String> {
    profile
        .get("dnaCdns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|cdn| cdn.trim().trim_end_matches('/').to_string())
        .filter(|cdn| cdn.starts_with("https://"))
        .collect()
}

fn package_root(profile: &Value) -> Result<&str, String> {
    match profile_str(profile, "dnaPackagePath").trim_matches('/') {
        "" => Err(format!(
            "{} is missing its dnaPackagePath setting.",
            game_profiles::display_name(profile)
        )),
        root => Ok(root),
    }
}

fn number_of(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub latest: u64,
    /// The major version folder, such as "1.6". Its digits are the start of `latest` (1.6 -> 16001).
    pub number: String,
    pub min_supported: u64,
}

pub fn manifest_from(body: &Value) -> Result<Manifest, String> {
    let latest = number_of(body.get("latest_version"))
        .filter(|v| *v > 0)
        .ok_or("the version manifest has no latest_version")?;
    let number = as_str(body.get("latest_version_number")).trim().to_string();
    let well_formed = !number.is_empty()
        && number.split('.').count() <= 2
        && number.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
    if !well_formed {
        return Err(format!("the version manifest has a malformed latest_version_number ({number:?})"));
    }
    if !latest.to_string().starts_with(&number.replace('.', "")) {
        return Err(format!(
            "the version manifest's latest_version {latest} does not belong to version {number}"
        ));
    }
    let min_supported = number_of(body.get("min_supported_version")).unwrap_or(0);
    Ok(Manifest { latest, number, min_supported })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub version: u64,
    pub file_name: String,
    pub md5: String,
    pub download_bytes: u64,
    /// The size of the whole game once patched.
    pub install_bytes: u64,
    /// One address per mirror, best first.
    pub urls: Vec<String>,
    /// A full archive unpacks from nothing; otherwise it patches the build in the game folder.
    pub full: bool,
}

/// The CDN folder for a build: `1.6/16001/full_16001` for a full archive.
/// Diff archives are only tried for builds the manifest still supports, and the official launcher's naming for them
/// has not been seen yet (no diff build has shipped since it was studied), so `{from}_{to}` is a best guess and a miss falls back to full.
pub fn package_dir(manifest: &Manifest, from: Option<u64>) -> String {
    match from {
        Some(from) => format!("{}/{}/{from}_{}", manifest.number, manifest.latest, manifest.latest),
        None => format!("{}/{}/full_{}", manifest.number, manifest.latest, manifest.latest),
    }
}

pub fn package_from(body: &Value, version: u64, full: bool, folder_urls: &[String]) -> Result<Package, String> {
    let info = body.get("hdiff_file").ok_or("the archive record has no hdiff_file")?;
    let file_name = as_str(info.get("name")).trim().to_string();
    let plain = !file_name.is_empty()
        && file_name.ends_with(".hdiff")
        && file_name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && !file_name.contains("..");
    if !plain {
        return Err(format!("the archive record names an unexpected file ({file_name:?})"));
    }
    let md5 = as_str(info.get("md5")).trim().to_ascii_lowercase();
    if md5.len() != 32 || !md5.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("the archive record has no usable MD5".to_string());
    }
    let download_bytes = number_of(info.get("size")).filter(|s| *s > 0).ok_or("the archive record has no size")?;
    let install_bytes = number_of(body.get("new_size")).unwrap_or(0);
    Ok(Package {
        version,
        urls: folder_urls.iter().map(|folder| format!("{folder}/{file_name}")).collect(),
        file_name,
        md5,
        download_bytes,
        install_bytes,
        full,
    })
}

async fn get_json_from_any(profile: &Value, rel: &str) -> Result<Value, String> {
    let mirrors = cdns(profile);
    if mirrors.is_empty() {
        return Err(format!("{} has no download mirrors configured.", game_profiles::display_name(profile)));
    }
    let mut last = String::new();
    for cdn in &mirrors {
        match http::get_json(&format!("{cdn}/{rel}")).await {
            Ok(body) => return Ok(body),
            Err(e) => last = e,
        }
    }
    Err(last)
}

pub async fn fetch_manifest(profile: &Value) -> Result<Manifest, String> {
    let root = package_root(profile)?;
    manifest_from(&get_json_from_any(profile, &format!("{root}/VersionManifest.json")).await?)
}

pub async fn fetch_version(profile: &Value) -> Result<String, String> {
    Ok(fetch_manifest(profile).await?.latest.to_string())
}

async fn fetch_package_at(profile: &Value, manifest: &Manifest, from: Option<u64>) -> Result<Package, String> {
    let root = package_root(profile)?;
    let dir = package_dir(manifest, from);
    let body = get_json_from_any(profile, &format!("{root}/{dir}/HPatchDiffMd5.json")).await?;
    let folders: Vec<String> = cdns(profile).iter().map(|cdn| format!("{cdn}/{root}/{dir}")).collect();
    package_from(&body, manifest.latest, from.is_none(), &folders)
}

/// The archive that brings the game folder to the latest build: a diff when the installed build is still supported and one exists, else full.
/// `repair` always picks the full archive, as the official launcher does.
pub async fn fetch_package(profile: &Value, installed: Option<u64>, repair: bool) -> Result<(Manifest, Package), String> {
    let manifest = fetch_manifest(profile).await?;
    let patchable = installed.filter(|v| !repair && *v >= manifest.min_supported && *v < manifest.latest);
    if let Some(from) = patchable {
        match fetch_package_at(profile, &manifest, Some(from)).await {
            Ok(package) => return Ok((manifest, package)),
            Err(e) => log::warn!(
                "{}: no diff archive from {from} to {} ({e}); downloading the full game instead.",
                game_profiles::display_name(profile),
                manifest.latest
            ),
        }
    }
    let package = fetch_package_at(profile, &manifest, None).await?;
    Ok((manifest, package))
}

pub fn installed_version(game_path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(game_path.join(VERSION_FILE)).ok()?;
    let body: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    number_of(body.get("version")).filter(|v| *v > 0)
}

/// Written exactly as the official launcher writes it, so either launcher can pick the install up.
pub fn write_installed_version(game_path: &Path, version: u64) -> Result<(), String> {
    let text = format!("{{\n    \"version\": {version}\n}}");
    fs_util::write_atomic(&game_path.join(VERSION_FILE), text.as_bytes())
        .map_err(|e| format!("Could not record the installed Duet Night Abyss version: {e}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHash {
    pub path: String,
    pub md5: String,
}

pub fn hashes_from(body: &Value) -> Vec<FileHash> {
    let mut hashes: Vec<FileHash> = body
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(path, entry)| {
            let path = path.trim().trim_start_matches(['/', '\\']).replace('\\', "/");
            let safe = !path.is_empty() && !path.split('/').any(|part| part.is_empty() || part == "." || part == "..");
            let md5 = as_str(entry.get("md5")).trim().to_ascii_lowercase();
            (safe && md5.len() == 32 && md5.chars().all(|c| c.is_ascii_hexdigit())).then_some(FileHash { path, md5 })
        })
        .collect();
    hashes.sort_by(|a, b| a.path.cmp(&b.path));
    hashes
}

pub async fn fetch_hashes(profile: &Value, manifest: &Manifest) -> Result<Vec<FileHash>, String> {
    let root = package_root(profile)?;
    let rel = format!("{root}/{}/{}/Hash.json", manifest.number, manifest.latest);
    let hashes = hashes_from(&get_json_from_any(profile, &rel).await?);
    if hashes.is_empty() {
        return Err("the file list for this build is empty".to_string());
    }
    Ok(hashes)
}

/// Hashes every listed file and returns the ones that are missing or differ. `on_progress` receives bytes read.
pub fn verify_files(
    game_path: &Path,
    hashes: &[FileHash],
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64),
) -> Result<Vec<FileHash>, String> {
    let mut broken = Vec::new();
    for entry in hashes {
        if cancel.load(Ordering::Relaxed) {
            return Err(fs_util::CANCELLED_MSG.to_string());
        }
        let path = fs_util::safe_join(game_path, &entry.path)?;
        if !path.is_file() {
            broken.push(entry.clone());
            continue;
        }
        let got = fs_util::md5_file(&path, &mut || cancel.load(Ordering::Relaxed), &mut |n| on_progress(n));
        match got {
            Ok(got) if fs_util::checksum_matches(&entry.md5, &got) => {}
            Ok(_) => broken.push(entry.clone()),
            Err(e) if e == fs_util::CANCELLED_MSG => return Err(e),
            Err(_) => broken.push(entry.clone()),
        }
    }
    Ok(broken)
}

pub fn listed_bytes(game_path: &Path, hashes: &[FileHash]) -> u64 {
    hashes
        .iter()
        .filter_map(|entry| fs_util::safe_join(game_path, &entry.path).ok())
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len())
        .sum()
}

// ------------ News ------------
// The official launcher reads two small feeds from the CDN: notices (title + link) and head images (the banner carousel). Each entry carries
// every language and an end time. The site's news list fills the rest of the panel.

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn live(entry: &Value, now: i64) -> bool {
    match as_i64(entry.get("EndTimestamp")) {
        0 => true,
        end => end > now,
    }
}

fn in_language<'a>(content: Option<&'a Value>, language: &str) -> Option<&'a Value> {
    content?
        .as_array()?
        .iter()
        .find(|c| as_str(c.get("language")).eq_ignore_ascii_case(language))
}

/// The CDN still links some images over plain http; every mirror serves https too.
fn secure(url: &str) -> Option<String> {
    let url = url.trim();
    if url.starts_with("https://") {
        return Some(url.to_string());
    }
    let rest = url.strip_prefix("http://")?;
    let host = rest.split('/').next().unwrap_or("");
    host.ends_with(".dna-panstudio.com").then(|| format!("https://{rest}"))
}

fn valid_link(url: &str) -> Option<&str> {
    let url = url.trim();
    (url.starts_with("https://") && url.matches("://").count() == 1).then_some(url)
}

pub fn notices_from(feed: &Value, language: &str, now: i64) -> Vec<Value> {
    let mut rows: Vec<(i64, Value)> = feed
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, entry)| live(entry, now))
        .filter_map(|(_, entry)| {
            let event = entry.get("event")?;
            let content = in_language(event.get("content"), language)?;
            let title = as_str(content.get("title")).trim();
            if title.is_empty() {
                return None;
            }
            let link = valid_link(as_str(content.get("titleUrl"))).unwrap_or("");
            let at = as_i64(event.get("date"));
            Some((at, article(title, link, local_month_day(at))))
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    rows.into_iter().map(|(_, row)| row).take(MAX_ITEMS).collect()
}

pub fn slides_from(feed: &Value, language: &str, now: i64) -> Vec<Value> {
    let mut entries: Vec<(i64, &Value)> = feed
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, entry)| live(entry, now))
        .map(|(_, entry)| (as_i64(entry.get("_incrementId")), entry))
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    let mut seen: Vec<String> = Vec::new();
    let mut slides = Vec::new();
    for (_, entry) in entries {
        let Some(content) = in_language(entry.get("content"), language) else { continue };
        for image in content.get("headImages").and_then(Value::as_array).into_iter().flatten() {
            let Some(url) = secure(as_str(image.get("image"))) else { continue };
            if seen.contains(&url) {
                continue;
            }
            let jump = valid_link(as_str(image.get("imageURL"))).unwrap_or("").to_string();
            slides.push(slide(&url, &jump));
            seen.push(url);
            if slides.len() == MAX_SLIDES {
                return slides;
            }
        }
    }
    slides
}

pub fn site_news_from(body: &Value, site: &str) -> Vec<Value> {
    if as_i64(body.get("code")) != 0 {
        return Vec::new();
    }
    let site = site.trim_end_matches('/');
    let mut rows: Vec<(i64, Value)> = body
        .pointer("/data/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|post| {
            let title = as_str(post.get("title")).trim();
            let id = as_str(post.get("id")).trim();
            if title.is_empty() || id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let at = as_i64(post.get("online_at")).max(as_i64(post.get("created_at")));
            Some((at, article(title, format!("{site}/en/#/news/content?id={id}"), local_month_day(at))))
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    rows.into_iter().map(|(_, row)| row).take(MAX_ITEMS).collect()
}

async fn fetch_site_news(profile: &Value) -> Result<Value, String> {
    let site = profile_str(profile, "dnaSiteUrl").trim_end_matches('/');
    let category = profile_str(profile, "dnaNewsCategory");
    if !site.starts_with("https://") || category.is_empty() || !category.chars().all(|c| c.is_ascii_digit()) {
        return Err("profile is missing the news site settings".to_string());
    }
    let url = format!("{site}/api/common/get-list.html");
    let response = http::client()
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(format!("id={category}&pageSize={MAX_ITEMS}&page=1"))
        .send()
        .await
        .map_err(|e| format!("Request error: {}", http::describe(&e)))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("HTTP {status} for {url}"));
    }
    let bytes = http::read_capped(response, &url).await?;
    serde_json::from_slice(&bytes).map_err(|e| format!("Invalid JSON from {url}: {e}"))
}

pub async fn get_news(profile: &Value) -> Result<Value, String> {
    let language = match profile_str(profile, "dnaNewsLanguage") {
        "" => "EN",
        language => language,
    };
    let (notice_feed, image_feed, site) = futures::join!(
        get_json_from_any(profile, "OperationLauncherNotice/OperationLauncherNoticeProductionGlobalonline.json"),
        get_json_from_any(profile, "OperationLauncherHeadImage/OperationLauncherHeadImageProductionGlobalonline.json"),
        fetch_site_news(profile),
    );
    let now = now_secs();
    let notices = notice_feed.as_ref().map(|f| notices_from(f, language, now)).unwrap_or_default();
    let slides = image_feed.as_ref().map(|f| slides_from(f, language, now)).unwrap_or_default();
    let news = match &site {
        Ok(body) => site_news_from(body, profile_str(profile, "dnaSiteUrl")),
        Err(e) => {
            log::warn!("news: Duet Night Abyss site list failed ({e})");
            Vec::new()
        }
    };
    if notices.is_empty() && news.is_empty() && slides.is_empty() {
        return Err(notice_feed
            .err()
            .or(image_feed.err())
            .or(site.err())
            .unwrap_or_else(|| "the Duet Night Abyss feeds were empty".to_string()));
    }
    Ok(envelope(notices, news, slides))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> Manifest {
        Manifest { latest: 16001, number: "1.6".into(), min_supported: 15001 }
    }

    #[test]
    fn the_manifest_is_read_and_its_version_must_match_its_folder() {
        let body = json!({ "latest_version": "16001", "latest_version_number": "1.6", "min_supported_version": "15001" });
        assert_eq!(manifest_from(&body).unwrap(), manifest());
        let mismatched = json!({ "latest_version": "17001", "latest_version_number": "1.6" });
        assert!(manifest_from(&mismatched).is_err());
        let malformed = json!({ "latest_version": "16001", "latest_version_number": "1.6/../x" });
        assert!(manifest_from(&malformed).is_err());
        assert!(manifest_from(&json!({ "latest_version_number": "1.6" })).is_err());
    }

    #[test]
    fn full_and_diff_archives_live_in_their_build_folder() {
        assert_eq!(package_dir(&manifest(), None), "1.6/16001/full_16001");
        assert_eq!(package_dir(&manifest(), Some(15001)), "1.6/16001/15001_16001");
    }

    #[test]
    fn the_archive_record_becomes_a_package_on_every_mirror() {
        let body = json!({
            "hdiff_file": { "name": "full_16001.hdiff", "md5": "1E60CDB01710F9F973ED14B50E0246BC", "size": 29723552214u64 },
            "new_size": 32637177238u64
        });
        let folders = vec!["https://a.example/p/1.6/16001/full_16001".to_string(), "https://b.example/p/1.6/16001/full_16001".to_string()];
        let package = package_from(&body, 16001, true, &folders).unwrap();
        assert_eq!(package.md5, "1e60cdb01710f9f973ed14b50e0246bc");
        assert_eq!(package.download_bytes, 29723552214);
        assert_eq!(package.install_bytes, 32637177238);
        assert_eq!(package.urls[1], "https://b.example/p/1.6/16001/full_16001/full_16001.hdiff");

        let escaping = json!({ "hdiff_file": { "name": "../evil.hdiff", "md5": "1e60cdb01710f9f973ed14b50e0246bc", "size": 1 } });
        assert!(package_from(&escaping, 1, true, &folders).is_err());
        let no_md5 = json!({ "hdiff_file": { "name": "full_1.hdiff", "md5": "nope", "size": 1 } });
        assert!(package_from(&no_md5, 1, true, &folders).is_err());
    }

    #[test]
    fn only_https_mirrors_are_used() {
        let profile = json!({ "dnaCdns": ["https://a.example/", "http://b.example", " https://c.example "] });
        assert_eq!(cdns(&profile), vec!["https://a.example".to_string(), "https://c.example".to_string()]);
    }

    #[test]
    fn the_version_record_round_trips_in_the_official_format() {
        let dir = std::env::temp_dir().join(format!("peebify-dna-version-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_installed_version(&dir, 16001).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(VERSION_FILE)).unwrap(), "{\n    \"version\": 16001\n}");
        assert_eq!(installed_version(&dir), Some(16001));
        std::fs::write(dir.join(VERSION_FILE), "{\"version\":\"oops\"}").unwrap();
        assert_eq!(installed_version(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_list_keeps_only_safe_paths_with_real_hashes() {
        let body = json!({
            "/EM.exe": { "md5": "7E06F6AFFE27BA1B531A80503ADCA722" },
            "/EM/Binaries/Win64/EM-Win64-Shipping.exe": { "md5": "3ef374f8f7eb44d211bcc0c063a520d9" },
            "/../escape.dll": { "md5": "3ef374f8f7eb44d211bcc0c063a520d9" },
            "/broken.bin": { "md5": "zz" },
        });
        let hashes = hashes_from(&body);
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes[0].path, "EM.exe");
        assert_eq!(hashes[0].md5, "7e06f6affe27ba1b531a80503adca722");
    }

    #[test]
    fn verification_reports_missing_and_changed_files() {
        let dir = std::env::temp_dir().join(format!("peebify-dna-verify-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("EM")).unwrap();
        std::fs::write(dir.join("EM/good.bin"), b"hello").unwrap();
        std::fs::write(dir.join("EM/bad.bin"), b"tampered").unwrap();
        let good = fs_util::md5_hex(b"hello");
        let hashes = vec![
            FileHash { path: "EM/good.bin".into(), md5: good.clone() },
            FileHash { path: "EM/bad.bin".into(), md5: good.clone() },
            FileHash { path: "EM/missing.bin".into(), md5: good },
        ];
        let mut read = 0;
        let broken = verify_files(&dir, &hashes, &AtomicBool::new(false), |n| read += n).unwrap();
        let names: Vec<&str> = broken.iter().map(|b| b.path.as_str()).collect();
        assert_eq!(names, vec!["EM/bad.bin", "EM/missing.bin"]);
        assert_eq!(read, 13);
        assert!(verify_files(&dir, &hashes, &AtomicBool::new(true), |_| {}).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notices_and_banners_come_from_the_launcher_feeds_in_english_and_unexpired() {
        let notices = json!({
            "a": { "EndTimestamp": 2054217599, "event": { "date": "1770048000", "content": [
                { "language": "CN", "title": "中文", "titleUrl": "https://x.example/cn" },
                { "language": "EN", "title": "Launcher FAQ", "titleUrl": "https://duetnightabyss.dna-panstudio.com/en/#/news/content?id=8673" }
            ]}},
            "b": { "EndTimestamp": 1, "event": { "date": "1770048001", "content": [{ "language": "EN", "title": "Expired" }] }},
        });
        let rows = notices_from(&notices, "EN", 1790000000);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["content"], "Launcher FAQ");

        let images = json!({
            "x": { "EndTimestamp": 1792425600, "_incrementId": 56, "content": [
                { "language": "CN", "headImages": [{ "image": "http://pan01-cdn-ali-jp.dna-panstudio.com/upload/cn", "imageURL": "https://wwhttps://www.youtube.com/x" }] },
                { "language": "EN", "headImages": [
                    { "image": "http://pan01-cdn-ali-jp.dna-panstudio.com/upload/en", "imageURL": "https://www.youtube.com/watch?v=ACQ9Wt7UWmU" },
                    { "image": "http://elsewhere.example/upload/x", "imageURL": "https://www.youtube.com/x" },
                    { "image": "http://pan01-cdn-ali-jp.dna-panstudio.com/upload/broken-link", "imageURL": "https://wwhttps://www.youtube.com/x" }
                ]}
            ]},
        });
        let slides = slides_from(&images, "EN", 1790000000);
        assert_eq!(slides.len(), 2);
        assert_eq!(slides[0]["url"], "https://pan01-cdn-ali-jp.dna-panstudio.com/upload/en");
        assert_eq!(slides[0]["jumpUrl"], "https://www.youtube.com/watch?v=ACQ9Wt7UWmU");
        assert_eq!(slides[1]["jumpUrl"], "");
    }

    #[test]
    fn site_posts_become_articles_newest_first() {
        let body = json!({ "code": 0, "data": { "data": [
            { "id": "9143", "title": " The Firmament Unbound | Update Notes ", "online_at": "1775209800", "created_at": "1775209800" },
            { "id": "9149", "title": "Update Preview", "online_at": "1775361000", "created_at": "1775197200" },
            { "id": "../x", "title": "Bad id", "online_at": "1" },
        ]}});
        let rows = site_news_from(&body, "https://duetnightabyss.dna-panstudio.com/");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["content"], "Update Preview");
        assert_eq!(rows[0]["jumpUrl"], "https://duetnightabyss.dna-panstudio.com/en/#/news/content?id=9149");
        assert!(site_news_from(&json!({ "code": 10002 }), "https://x").is_empty());
    }
}
