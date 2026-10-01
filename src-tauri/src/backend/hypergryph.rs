// ------------ Arknights: Endfield Feed ------------
// Talks to Gryphline's launcher API to find the latest Endfield version and its download packs, and to pull the
// launcher news. The packs it returns are the split zip files that hypergryph_reconcile.rs installs from.

use serde_json::{json, Value};

use super::http;
use super::news;

struct HgApiConfig {
    api_url: String,
    web_api_url: String,
    appcode: String,
    channel: String,
    sub_channel: String,
    seq: String,
    language: String,
}

fn api_config(profile: &Value) -> Result<HgApiConfig, String> {
    let field = |key: &str| -> Result<String, String> {
        profile
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("profile is missing '{key}' for the Hypergryph API"))
    };
    Ok(HgApiConfig {
        api_url: field("hgApiUrl")?,
        web_api_url: field("hgWebApiUrl")?,
        appcode: field("hgAppCode")?,
        channel: field("hgChannel")?,
        sub_channel: field("hgSubChannel")?,
        seq: field("hgSeq")?,
        language: field("hgLanguage")?,
    })
}

async fn post_batch_proxy_once(url: &str, body: &Value) -> Result<Value, String> {
    let response = http::client()
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Request error: {e}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("HTTP {status} for {url}"));
    }
    let bytes = http::read_capped(response, url).await?;
    serde_json::from_slice(&bytes).map_err(|e| format!("Invalid JSON from {url}: {e}"))
}

async fn post_batch_proxy(url: &str, body: &Value) -> Result<Value, String> {
    http::with_retry(
        || post_batch_proxy_once(url, body),
        3,
        1000,
        "Gryphline batch_proxy",
    )
    .await
}

fn find_rsp<'a>(response: &'a Value, kind: &str) -> Option<&'a Value> {
    response["proxy_rsps"]
        .as_array()?
        .iter()
        .find(|r| r["kind"].as_str() == Some(kind))
}

fn response_snippet(response: &Value) -> String {
    response.to_string().chars().take(300).collect()
}

fn pack_size(pack: &Value) -> u64 {
    match pack.get("package_size") {
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

fn pack_str<'a>(pack: &'a Value, key: &str) -> &'a str {
    pack.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn validate_packs(packs: &[Value]) -> Result<(), String> {
    for (index, pack) in packs.iter().enumerate() {
        if pack_str(pack, "url").trim().is_empty() {
            return Err(format!("get_latest_game_rsp pack {index} has no url"));
        }
        if pack_str(pack, "md5").trim().is_empty() {
            return Err(format!("get_latest_game_rsp pack {index} has no md5"));
        }
        if pack_size(pack) == 0 {
            return Err(format!(
                "get_latest_game_rsp pack {index} has no valid package_size"
            ));
        }
    }
    Ok(())
}

fn latest_game_from_response(response: &Value) -> Result<Value, String> {
    let rsp = find_rsp(response, "get_latest_game")
        .and_then(|r| r.get("get_latest_game_rsp"))
        .ok_or_else(|| {
            format!(
                "batch_proxy response has no get_latest_game_rsp (response: {})",
                response_snippet(response)
            )
        })?;

    let version = rsp
        .get("version")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!(
                "get_latest_game_rsp has no version (response: {})",
                response_snippet(response)
            )
        })?;
    let packs = rsp["pkg"]["packs"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            format!(
                "get_latest_game_rsp has no pkg.packs (response: {})",
                response_snippet(response)
            )
        })?;
    validate_packs(packs)?;

    Ok(json!({ "version": version, "packs": packs }))
}

pub async fn get_latest_game(profile: &Value) -> Result<Value, String> {
    let cfg = api_config(profile)?;
    let body = json!({
        "seq": cfg.seq,
        "proxy_reqs": [{
            "kind": "get_latest_game",
            "get_latest_game_req": {
                "appcode": cfg.appcode,
                "launcher_appcode": cfg.appcode,
                "channel": cfg.channel,
                "sub_channel": cfg.sub_channel,
                "version": "",
            },
        }],
    });
    let response = post_batch_proxy(&cfg.api_url, &body).await?;
    latest_game_from_response(&response)
}

pub fn packs_as_resources(packs: &Value) -> Vec<super::validator::Resource> {
    let empty: Vec<Value> = Vec::new();
    packs
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .map(|pack| {
            let url = pack_str(pack, "url");
            let dest = std::path::Path::new(url)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            super::validator::Resource::new(
                dest,
                pack_size(pack),
                pack_str(pack, "md5").to_lowercase(),
            )
            .with_url(Some(url))
        })
        .collect()
}

fn format_notice_date(start_ts: &Value) -> String {
    let raw = match start_ts {
        Value::String(s) => s.parse::<i64>().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    };
    raw.map(news::local_month_day).unwrap_or_default()
}

fn news_from_response(response: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let banner_rsp = find_rsp(response, "get_banner");
    let announcement_rsp = find_rsp(response, "get_announcement");
    if banner_rsp.is_none() || announcement_rsp.is_none() {
        log::warn!(
            "Gryphline news response is missing get_banner or get_announcement (response: {})",
            response_snippet(response)
        );
    }

    let slideshow: Vec<Value> = banner_rsp
        .and_then(|r| r["get_banner_rsp"]["banners"].as_array())
        .unwrap_or(&empty)
        .iter()
        .filter_map(|banner| {
            let url = pack_str(banner, "url");
            if url.is_empty() {
                return None;
            }
            Some(json!({
                "url": url,
                "jumpUrl": pack_str(banner, "jump_url"),
            }))
        })
        .take(news::MAX_SLIDES)
        .collect();

    let mut notice: Vec<Value> = Vec::new();
    let mut news_items: Vec<Value> = Vec::new();
    let tabs = announcement_rsp
        .and_then(|r| r["get_announcement_rsp"]["tabs"].as_array())
        .unwrap_or(&empty);
    for tab in tabs {
        let tab_name = pack_str(tab, "tabName").to_lowercase();
        let items = tab["announcements"].as_array().unwrap_or(&empty);
        let bucket = if tab_name.starts_with("news") {
            &mut news_items
        } else {
            &mut notice
        };
        for item in items {
            if bucket.len() >= news::MAX_ITEMS {
                break;
            }
            let title = pack_str(item, "content");
            if title.is_empty() {
                continue;
            }
            bucket.push(news::article(
                title,
                pack_str(item, "jump_url"),
                format_notice_date(&item["start_ts"]),
            ));
        }
    }

    news::envelope(notice, news_items, slideshow)
}

pub async fn get_news(profile: &Value) -> Result<Value, String> {
    let cfg = api_config(profile)?;
    let common = json!({
        "appcode": cfg.appcode,
        "language": cfg.language,
        "channel": cfg.channel,
        "sub_channel": cfg.sub_channel,
        "platform": "Windows",
        "source": "launcher",
    });
    let body = json!({
        "seq": cfg.seq,
        "proxy_reqs": [
            { "kind": "get_banner", "get_banner_req": common },
            { "kind": "get_announcement", "get_announcement_req": common },
        ],
    });
    let response = post_batch_proxy(&cfg.web_api_url, &body).await?;
    check_news_response(&response)?;
    Ok(news_from_response(&response))
}

fn check_news_response(response: &Value) -> Result<(), String> {
    if find_rsp(response, "get_banner").is_none() && find_rsp(response, "get_announcement").is_none()
    {
        return Err(format!(
            "Gryphline news response has no banner or announcement data (response: {})",
            response_snippet(response)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latest(packs: Value) -> Value {
        json!({
            "proxy_rsps": [{
                "kind": "get_latest_game",
                "get_latest_game_rsp": { "version": "1.2.3", "pkg": { "packs": packs } },
            }],
        })
    }

    fn pack(url: &str, md5: &str, size: Value) -> Value {
        json!({ "url": url, "md5": md5, "package_size": size })
    }

    #[test]
    fn valid_packs_resolve() {
        let response = latest(json!([
            pack("https://cdn.example/a.zip.001", "ABC", json!("100")),
            pack("https://cdn.example/a.zip.002", "def", json!(50)),
        ]));
        let resolved = latest_game_from_response(&response).unwrap();
        assert_eq!(resolved["version"], "1.2.3");
        let resources = packs_as_resources(&resolved["packs"]);
        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0].dest(), "a.zip.001");
        assert_eq!(resources[0].size, 100);
        assert_eq!(&*resources[0].md5, "abc");
        assert_eq!(resources[1].size, 50);
    }

    #[test]
    fn packs_missing_fields_are_rejected() {
        let cases = [
            (pack("", "abc", json!(1)), "no url"),
            (pack("https://cdn.example/a", "", json!(1)), "no md5"),
            (pack("https://cdn.example/a", "abc", json!("0")), "package_size"),
            (pack("https://cdn.example/a", "abc", json!("x")), "package_size"),
            (json!({ "url": "https://cdn.example/a", "md5": "abc" }), "package_size"),
        ];
        for (bad, needle) in cases {
            let err = latest_game_from_response(&latest(json!([bad]))).unwrap_err();
            assert!(err.contains(needle), "{err}");
        }
    }

    #[test]
    fn missing_rsp_error_carries_a_bounded_snippet() {
        let long = "é".repeat(1000);
        let response = json!({ "code": 7, "msg": long });
        let err = latest_game_from_response(&response).unwrap_err();
        assert!(err.contains("\"code\":7"), "{err}");
        assert_eq!(response_snippet(&response).chars().count(), 300);
    }

    #[test]
    fn notice_dates_accept_seconds_and_millis() {
        let seconds = format_notice_date(&json!(1_758_196_800));
        let millis = format_notice_date(&json!("1758196800000"));
        assert_eq!(seconds, millis);
        assert_eq!(seconds.len(), 5);
        assert_eq!(&seconds[2..3], "-");
        assert_eq!(format_notice_date(&Value::Null), "");
    }

    #[test]
    fn an_error_envelope_is_a_failure_not_empty_news() {
        let err = check_news_response(&json!({ "code": 503, "msg": "maintenance" })).unwrap_err();
        assert!(err.contains("\"code\":503"), "{err}");
        let partial = json!({ "proxy_rsps": [{ "kind": "get_banner", "get_banner_rsp": {} }] });
        assert!(check_news_response(&partial).is_ok());
    }

    #[test]
    fn news_is_capped_and_skips_empty_banners() {
        let banners: Vec<Value> = (0..8)
            .map(|i| json!({ "url": if i == 0 { String::new() } else { format!("https://img/{i}") }, "jump_url": "" }))
            .collect();
        let announcements: Vec<Value> = (0..20)
            .map(|i| json!({ "content": format!("item {i}"), "jump_url": "https://j", "start_ts": "1758196800" }))
            .collect();
        let response = json!({
            "proxy_rsps": [
                { "kind": "get_banner", "get_banner_rsp": { "banners": banners } },
                { "kind": "get_announcement", "get_announcement_rsp": { "tabs": [
                    { "tabName": "News", "announcements": announcements },
                    { "tabName": "Notice", "announcements": announcements },
                ] } },
            ],
        });
        let out = news_from_response(&response);
        let slides = out["slideshow"].as_array().unwrap();
        assert_eq!(slides.len(), news::MAX_SLIDES);
        assert_eq!(slides[0]["url"], "https://img/1");
        assert_eq!(
            out["guidance"]["news"]["contents"].as_array().unwrap().len(),
            news::MAX_ITEMS
        );
        assert_eq!(
            out["guidance"]["notice"]["contents"].as_array().unwrap().len(),
            news::MAX_ITEMS
        );
        assert!(out.get("social_media_list").is_none());
    }
}
