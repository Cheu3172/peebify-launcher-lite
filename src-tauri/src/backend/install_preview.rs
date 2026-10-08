// ------------ Install Preview ------------
// Answers the "what will this cost me?" question before a download starts: download size, install size, optional
// content packs and the free disk space needed, for every game. Results are cached for a few minutes.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use super::game_profiles::InstallMode;
use super::state::BackendState;
use super::{
    bd2, bluepoch, download_engine, err_response, game_profiles, gf2, http, hypergryph,
    hypergryph_reconcile as reconcile, nte, ok_with, sophon, yostar,
};

const PREVIEW_CACHE_TIMEOUT: Duration = Duration::from_secs(300);
const BUILD_CACHE_TIMEOUT: Duration = Duration::from_secs(120);
const OFFLINE: &str = "No internet connection.";

static PREVIEW_CACHE: http::TtlCache<Value> = http::TtlCache::new(PREVIEW_CACHE_TIMEOUT, 16);
static BUILD_CACHE: http::TtlCache<sophon::Build> = http::TtlCache::new(BUILD_CACHE_TIMEOUT, 4);

async fn offline_or(error: String) -> String {
    if !http::is_online_cached() || !http::is_online().await {
        OFFLINE.to_string()
    } else {
        error
    }
}

async fn cached_build(auth: &sophon::BranchAuth) -> Result<sophon::Build, String> {
    let key = format!("{}:{}:{}", auth.branch, auth.package_id, auth.tag);
    if let Some(build) = BUILD_CACHE.get(&key) {
        return Ok(build);
    }
    let build = sophon::fetch_build(auth).await?;
    BUILD_CACHE.set(&key, build.clone());
    Ok(build)
}

fn opt_bytes(total: u64) -> Value {
    if total > 0 {
        json!(total)
    } else {
        Value::Null
    }
}

fn merge_parts(entries: &[(String, String, u64, u64)]) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut totals: HashMap<String, (String, u64, u64)> = HashMap::new();
    for (key, label, bytes, files) in entries {
        let slot = totals.entry(label.clone()).or_insert_with(|| {
            order.push(label.clone());
            (key.clone(), 0, 0)
        });
        slot.1 += bytes;
        slot.2 += files;
    }
    order
        .iter()
        .filter_map(|label| {
            let (key, bytes, files) = totals.get(label)?;
            Some(json!({ "key": key, "label": label, "bytes": bytes, "files": files }))
        })
        .collect()
}

struct Preview {
    version: String,
    download_bytes: u64,
    install_bytes: u64,
    file_count: u64,
    approximate: bool,
    parts: Vec<Value>,
}

async fn sophon_preview(profile: &Value) -> Result<Preview, String> {
    let auth = sophon::cached_branch_auth_for_profile(profile).await?;
    let build = cached_build(&auth).await?;
    let audio_language = game_profiles::audio_language(game_profiles::profile_id(profile));
    let categories = sophon::install_categories(&build, &audio_language);

    let entries: Vec<(String, String, u64, u64)> = categories
        .iter()
        .map(|c| {
            (
                c.matching_field.clone(),
                sophon::category_label(&c.matching_field),
                c.total_bytes,
                c.file_count,
            )
        })
        .collect();

    Ok(Preview {
        version: build.tag.clone(),
        download_bytes: categories.iter().map(|c| c.compressed_bytes).sum(),
        install_bytes: categories.iter().map(|c| c.total_bytes).sum(),
        file_count: categories.iter().map(|c| c.file_count).sum(),
        approximate: false,
        parts: merge_parts(&entries),
    })
}

async fn hypergryph_preview(profile: &Value) -> Result<Preview, String> {
    let latest = hypergryph::get_latest_game(profile).await?;
    let resources = hypergryph::packs_as_resources(&latest["packs"]);
    let totals = match reconcile::latest_packs(&latest) {
        Some((_, packs)) => {
            let cancelled = Arc::new(AtomicBool::new(false));
            let handle = tokio::runtime::Handle::current();
            match tauri::async_runtime::spawn_blocking(move || {
                reconcile::remote_totals(packs, cancelled, handle)
            })
            .await
            {
                Ok(Ok(totals)) => Some(totals),
                Ok(Err(e)) => {
                    log::warn!("Endfield preview: could not read the package index: {e}");
                    None
                }
                Err(e) => {
                    log::warn!("Endfield preview: package index task failed: {e}");
                    None
                }
            }
        }
        None => None,
    };
    let (install_bytes, file_count, approximate) = match totals {
        Some((bytes, files)) if bytes > 0 => (bytes, files, false),
        _ => (0, 0, true),
    };
    Ok(Preview {
        version: latest["version"].as_str().unwrap_or("").to_string(),
        download_bytes: resources.iter().map(|r| r.size).sum(),
        install_bytes,
        file_count,
        approximate,
        parts: Vec::new(),
    })
}

fn resource_list_preview(config: download_engine::GameConfig) -> Preview {
    Preview {
        version: config.version,
        download_bytes: config.resources.iter().map(|r| r.size).sum(),
        install_bytes: config.resources.iter().map(|r| r.size).sum(),
        file_count: config.resources.len() as u64,
        approximate: false,
        parts: Vec::new(),
    }
}

async fn kuro_preview(profile: &Value, quality: Option<&str>) -> Result<Preview, String> {
    let headline = download_engine::kuro_headline(profile, quality).await?;
    if let (Some(download), false) = (headline.download_bytes, headline.version.is_empty()) {
        return Ok(Preview {
            version: headline.version,
            download_bytes: download,
            install_bytes: headline.install_bytes.unwrap_or(download),
            file_count: 0,
            approximate: false,
            parts: Vec::new(),
        });
    }
    let config = download_engine::resolve_kuro_config(profile, quality).await?;
    Ok(resource_list_preview(config))
}

async fn nte_preview(profile: &Value) -> Result<Preview, String> {
    let (config_list, launcher) = tokio::join!(
        async {
            let config = nte::fetch_config(profile).await?;
            let list = nte::fetch_reslist(profile, &config).await?;
            Ok::<_, String>((config, list))
        },
        nte::fetch_launcher(profile),
    );
    let (config, list) = config_list?;
    let launcher = launcher?;

    let wanted = game_profiles::content_tags(game_profiles::profile_id(profile));
    let keeps = |tag: &str| match wanted.as_deref() {
        None => true,
        Some(list) => list.iter().any(|t| t == tag),
    };

    let mut entries: Vec<(String, String, u64, u64)> = list
        .tag_totals()
        .into_iter()
        .filter(|(tag, _, _)| tag == "baseTag" || keeps(tag))
        .map(|(tag, bytes, files)| {
            let label = if tag == "baseTag" {
                "game files".to_string()
            } else {
                match nte::tag_language(&tag) {
                    Some(language) => format!("{language} voice"),
                    None => "voice pack".to_string(),
                }
            };
            (tag, label, bytes, files)
        })
        .collect();
    entries.push((
        "launcher".to_string(),
        "launcher runtime".to_string(),
        launcher.download_bytes(),
        launcher.resources.len() as u64,
    ));

    let installed: u64 = launcher.resources.iter().map(|r| r.size).sum();
    Ok(Preview {
        version: config.res_version,
        download_bytes: list.selected_bytes(wanted.as_deref()) + launcher.download_bytes(),
        install_bytes: list.selected_bytes(wanted.as_deref()) + installed,
        file_count: list.selected(wanted.as_deref()).count() as u64
            + launcher.resources.len() as u64,
        approximate: false,
        parts: merge_parts(&entries),
    })
}

async fn yostar_preview(profile: &Value) -> Result<Preview, String> {
    Ok(resource_list_preview(yostar::resolve_config(profile).await?))
}

async fn gf2_preview(profile: &Value) -> Result<Preview, String> {
    let config = gf2::fetch_config(profile).await?;
    let client_bytes = http::content_length(&config.client_url).await.unwrap_or(0);
    let resource_bytes = config.download_bytes();

    let entries = vec![
        (
            "client".to_string(),
            format!("game client {}", config.client_version),
            client_bytes,
            1,
        ),
        (
            "resources".to_string(),
            "game resources".to_string(),
            resource_bytes,
            config.file_count(),
        ),
    ];

    Ok(Preview {
        version: config.ab_version.clone(),
        download_bytes: client_bytes + resource_bytes,
        install_bytes: 0,
        file_count: config.file_count() + 1,
        approximate: true,
        parts: merge_parts(&entries),
    })
}

async fn bluepoch_preview(profile: &Value) -> Result<Preview, String> {
    let package = bluepoch::fetch_package(profile, "").await?;
    let parts = package.resources.len() as u64;
    Ok(Preview {
        version: package.version.clone(),
        download_bytes: package.download_bytes,
        install_bytes: package.install_bytes,
        file_count: parts,
        approximate: true,
        parts: merge_parts(&[(
            "game".to_string(),
            "game client".to_string(),
            package.download_bytes,
            parts,
        )]),
    })
}

async fn bd2_preview(profile: &Value) -> Result<Preview, String> {
    let package = bd2::fetch_package(profile).await?;
    Ok(Preview {
        version: package.version.clone(),
        download_bytes: package.download_bytes,
        install_bytes: package.install_bytes,
        file_count: 1,
        approximate: true,
        parts: merge_parts(&[(
            "game".to_string(),
            "game client".to_string(),
            package.download_bytes,
            1,
        )]),
    })
}

async fn dna_preview(profile: &Value) -> Result<Preview, String> {
    let (_, package) = super::dna::fetch_package(profile, None, false).await?;
    Ok(Preview {
        version: package.version.to_string(),
        download_bytes: package.download_bytes,
        install_bytes: package.install_bytes,
        file_count: 1,
        approximate: true,
        parts: merge_parts(&[(
            "game".to_string(),
            "game client".to_string(),
            package.download_bytes,
            1,
        )]),
    })
}

async fn build_preview(profile: &Value, quality: Option<&str>) -> Result<Preview, String> {
    match game_profiles::install_mode(profile) {
        Some("sophon") => sophon_preview(profile).await,
        Some("bluepoch") => bluepoch_preview(profile).await,
        Some("bd2") => bd2_preview(profile).await,
        Some("dna") => dna_preview(profile).await,
        Some("hypergryph") => hypergryph_preview(profile).await,
        Some("gf2") => gf2_preview(profile).await,
        Some("netease") => nte_preview(profile).await,
        Some("yostar") => yostar_preview(profile).await,
        _ => kuro_preview(profile, quality).await,
    }
}

fn required_free(mode: InstallMode, download_bytes: u64, install_bytes: u64) -> u64 {
    let (write_bytes, multiplier) = match mode {
        InstallMode::Bd2 | InstallMode::Dna => (download_bytes.saturating_add(install_bytes), 1.0),
        InstallMode::Hypergryph if install_bytes > 0 => {
            (download_bytes.saturating_add(install_bytes), 1.0)
        }
        mode if download_engine::splits_archives(mode) => {
            (download_bytes, download_engine::SPLIT_ARCHIVE_MULTIPLIER)
        }
        _ => (download_bytes.max(install_bytes), 1.0),
    };
    download_engine::required_free_bytes(
        write_bytes,
        multiplier,
        download_engine::HEADROOM_INSTALL,
    )
}

pub(super) fn invalidate_preview(profile_id: &str) {
    PREVIEW_CACHE.remove(profile_id);
    let profile = game_profiles::profile(profile_id);
    if let Some(qualities) = profile["resourceQualityArgs"].as_object() {
        for quality in qualities.keys() {
            PREVIEW_CACHE.remove(&format!("{profile_id}:{quality}"));
        }
    }
}

pub(super) async fn get_resource_quality(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let game_id = match args.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(id) => id.to_string(),
        None => app.state::<BackendState>().config.active_game_id(),
    };
    let profile = game_profiles::profile(&game_id);
    let Some(qualities) = profile["resourceQualityArgs"].as_object() else {
        return Ok(ok_with(json!({ "installed": [], "sizes": null })));
    };
    let key = format!("games.{}.gamePath", game_profiles::profile_id(profile));
    let game_path = app.state::<BackendState>().config.get(&key);
    let installed = match game_path.as_str().filter(|p| !p.is_empty()) {
        Some(path) => download_engine::qualities_on_disk(profile, std::path::Path::new(path)),
        None => Vec::new(),
    };

    let with_sizes = args.get(1).and_then(Value::as_bool).unwrap_or(false);
    if !with_sizes {
        return Ok(ok_with(json!({ "installed": installed, "sizes": Value::Null })));
    }

    let config_url = profile["gameConfigUrl"].as_str().unwrap_or_default();
    let sizes = match http::get_json(config_url).await {
        Ok(config) if download_engine::is_bundle_config(&config) => {
            let mut sizes = serde_json::Map::new();
            for quality in qualities.keys() {
                let bundle = download_engine::bundle_name(quality);
                if let Some((download, install)) = download_engine::bundle_bytes(&config, &bundle) {
                    sizes.insert(
                        quality.clone(),
                        json!({ "downloadBytes": download, "installBytes": install }),
                    );
                }
            }
            Value::Object(sizes)
        }
        Ok(_) => Value::Null,
        Err(e) => {
            log::debug!("Resource quality sizes unavailable for {game_id}: {e}");
            Value::Null
        }
    };
    Ok(ok_with(json!({ "installed": installed, "sizes": sizes })))
}

pub(super) async fn get_content_packs(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let requested = args
        .first()
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let game_id = match requested {
        Some(id) => id.to_string(),
        None => app.state::<BackendState>().config.active_game_id(),
    };
    let profile = game_profiles::profile(&game_id);
    let profile_id = game_profiles::profile_id(profile).to_string();

    if game_profiles::install_mode(profile) != Some("netease") {
        return Ok(ok_with(
            json!({ "gameId": profile_id, "packs": [], "supported": false }),
        ));
    }
    let config = match nte::fetch_config(profile).await {
        Ok(config) => config,
        Err(e) => return Ok(err_response(offline_or(e).await)),
    };
    let list = match nte::fetch_reslist(profile, &config).await {
        Ok(list) => list,
        Err(e) => return Ok(err_response(offline_or(e).await)),
    };

    let selected = game_profiles::content_tags(&profile_id);
    let packs: Vec<Value> = list
        .optional_tags()
        .into_iter()
        .map(|(tag, bytes, files)| {
            let on = match selected.as_deref() {
                None => true,
                Some(list) => list.contains(&tag),
            };
            json!({
                "tag": tag,
                "language": nte::tag_language(&tag),
                "bytes": bytes,
                "files": files,
                "selected": on
            })
        })
        .collect();

    Ok(ok_with(
        json!({ "gameId": profile_id, "packs": packs, "supported": true }),
    ))
}

pub(super) async fn get_install_preview(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let requested = args
        .first()
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let game_id = match requested {
        Some(id) => id.to_string(),
        None => app.state::<BackendState>().config.active_game_id(),
    };
    let profile = game_profiles::profile(&game_id);
    let profile_id = game_profiles::profile_id(profile).to_string();

    if !game_profiles::is_managed(profile) {
        return Ok(ok_with(json!({
            "gameId": profile_id,
            "version": null,
            "downloadBytes": null,
            "installBytes": null,
            "fileCount": null,
            "approximate": false,
            "parts": [],
            "notSupported": true,
        })));
    }

    let quality = download_engine::selected_quality(app, profile);
    let cache_key = match &quality {
        Some(quality) => format!("{profile_id}:{quality}"),
        None => profile_id.clone(),
    };
    if let Some(hit) = PREVIEW_CACHE.get(&cache_key) {
        return Ok(ok_with(hit));
    }

    let preview = match build_preview(profile, quality.as_deref()).await {
        Ok(preview) => preview,
        Err(e) => {
            log::warn!("Install preview failed ({profile_id}): {e}");
            return Ok(err_response(offline_or(e).await));
        }
    };

    let required = required_free(
        InstallMode::of(profile),
        preview.download_bytes,
        preview.install_bytes,
    );
    let result = json!({
        "gameId": profile_id,
        "version": (!preview.version.is_empty()).then_some(preview.version),
        "downloadBytes": opt_bytes(preview.download_bytes),
        "installBytes": opt_bytes(preview.install_bytes),
        "requiredBytes": opt_bytes(required),
        "fileCount": opt_bytes(preview.file_count),
        "approximate": preview.approximate,
        "parts": preview.parts,
        "notSupported": false,
    });
    PREVIEW_CACHE.set(&cache_key, result.clone());
    Ok(ok_with(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn split_archive_modes_need_room_for_archives_and_extraction() {
        let download = 50 * GIB;
        let expected = (download as f64 * download_engine::SPLIT_ARCHIVE_MULTIPLIER) as u64
            + (download / 20).max(download_engine::HEADROOM_INSTALL);
        assert_eq!(required_free(InstallMode::Hypergryph, download, 0), expected);
        assert_eq!(required_free(InstallMode::Bluepoch, download, 60 * GIB), expected);
    }

    #[test]
    fn hypergryph_with_a_known_install_size_needs_the_archives_and_their_contents() {
        assert_eq!(
            required_free(InstallMode::Hypergryph, 50 * GIB, 60 * GIB),
            110 * GIB + (110 * GIB / 20).max(download_engine::HEADROOM_INSTALL)
        );
    }

    #[test]
    fn bd2_needs_the_package_and_its_contents() {
        assert_eq!(
            required_free(InstallMode::Bd2, 4 * GIB, 6 * GIB),
            10 * GIB + download_engine::HEADROOM_INSTALL
        );
    }

    #[test]
    fn other_modes_use_the_larger_of_download_and_install() {
        assert_eq!(
            required_free(InstallMode::Sophon, 20 * GIB, 30 * GIB),
            30 * GIB + download_engine::HEADROOM_INSTALL
        );
        assert_eq!(
            required_free(InstallMode::Default, 30 * GIB, 0),
            30 * GIB + download_engine::HEADROOM_INSTALL
        );
    }

    #[test]
    fn unknown_sizes_need_nothing() {
        assert_eq!(required_free(InstallMode::Default, 0, 0), 0);
    }
}
