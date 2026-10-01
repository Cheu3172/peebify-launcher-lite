// ------------ Launcher News ------------
// Turns each game's news source into one common shape (notices, news and a slideshow) for the home screen.
// A few games have their own adapter below; HoYoPlay and Hypergryph are fetched elsewhere, and the rest fall back to the launcher's api.json.

use serde_json::{json, Value};

use super::bd2;
use super::bluepoch;
use super::http;

pub(crate) const MAX_ITEMS: usize = 12;
pub const MAX_SLIDES: usize = 5;

pub fn as_i64(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn as_str(v: Option<&Value>) -> &str {
    v.and_then(|x| x.as_str()).unwrap_or("")
}

pub(crate) fn short_date(date: &str) -> String {
    let trimmed = date.trim();
    let first = trimmed
        .split(|c: char| c.is_whitespace() || c == 'T')
        .next()
        .unwrap_or("");
    let parts: Vec<&str> = first.split(['-', '/', '.']).collect();
    let (month, day) = match parts.as_slice() {
        [year, month, day] if year.len() == 4 => (*month, *day),
        [month, day] => (*month, *day),
        _ => return trimmed.to_string(),
    };
    match (month.parse::<u32>(), day.parse::<u32>()) {
        (Ok(m), Ok(d)) if (1..=12).contains(&m) && (1..=31).contains(&d) => {
            format!("{m:02}-{d:02}")
        }
        _ => trimmed.to_string(),
    }
}

const UTC_PLUS_8: i32 = 8 * 3600;

pub(crate) fn local_short_date(date: &str, source_offset_secs: i32) -> String {
    use chrono::{DateTime, FixedOffset, Local, NaiveDateTime, TimeZone};
    let trimmed = date.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(trimmed) {
        return at.with_timezone(&Local).format("%m-%d").to_string();
    }
    let naive = [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y/%m/%d %H:%M:%S",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(trimmed, format).ok());
    let source = FixedOffset::east_opt(source_offset_secs);
    match (naive, source) {
        (Some(naive), Some(source)) => match source.from_local_datetime(&naive).single() {
            Some(at) => at.with_timezone(&Local).format("%m-%d").to_string(),
            None => short_date(trimmed),
        },
        _ => short_date(trimmed),
    }
}

pub fn article(content: &str, jump_url: impl AsRef<str>, time: impl AsRef<str>) -> Value {
    json!({
        "content": content,
        "jumpUrl": jump_url.as_ref(),
        "time": time.as_ref(),
    })
}

pub(crate) fn slide(url: &str, jump_url: &str) -> Value {
    json!({ "url": url, "jumpUrl": jump_url })
}

fn dedupe_take(items: Vec<&Value>, id_of: impl Fn(&Value) -> i64) -> Vec<&Value> {
    let mut seen: Vec<i64> = Vec::with_capacity(MAX_ITEMS);
    items
        .into_iter()
        .filter(|a| {
            let id = id_of(a);
            if seen.contains(&id) {
                return false;
            }
            seen.push(id);
            true
        })
        .take(MAX_ITEMS)
        .collect()
}

pub fn envelope(notice: Vec<Value>, news: Vec<Value>, slideshow: Vec<Value>) -> Value {
    json!({
        "guidance": {
            "notice": { "contents": notice },
            "news": { "contents": news },
        },
        "slideshow": slideshow,
    })
}

// ------------ Punishing: Gray Raven News ------------
// Reads Kuro's public news JSON for PGR and splits it into notices, news and banner slides.
const JSON_BASE: &str = "https://media-cdn-zspms.kurogame.net/pnswebsite/website2.0/json/G167";
const PGR_ARTICLE_URL: &str = "https://pgr.kurogame.net/news";

const TYPE_NEWS: i64 = 47;
const TYPE_NOTICE: i64 = 48;
const TYPE_EVENT: i64 = 49;

const PICTURE_TYPE_NEWS_BANNER: i64 = 8;

fn pgr_collect_articles(main: &Value, wanted: &[i64]) -> Vec<Value> {
    let empty = Vec::new();
    let mut items: Vec<&Value> = main
        .get("article")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .filter(|a| wanted.contains(&as_i64(a.get("articleType"))))
        .collect();

    items.sort_by(|a, b| as_str(b.get("createTime")).cmp(as_str(a.get("createTime"))));

    dedupe_take(items, |a| as_i64(a.get("articleId")))
        .into_iter()
        .map(|a| {
            let id = as_i64(a.get("articleId"));
            article(
                as_str(a.get("articleTitle")),
                format!("{PGR_ARTICLE_URL}/{id}"),
                local_short_date(as_str(a.get("createTime")), UTC_PLUS_8),
            )
        })
        .collect()
}

fn pgr_collect_slideshow(main: &Value) -> Vec<Value> {
    let empty = Vec::new();
    let mut pictures: Vec<&Value> = main
        .get("picture")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .filter(|p| as_i64(p.get("pictureType")) == PICTURE_TYPE_NEWS_BANNER)
        .filter(|p| !as_str(p.get("imgUrl")).is_empty())
        .collect();

    pictures.sort_by_key(|p| std::cmp::Reverse(as_i64(p.get("sortingMark"))));

    pictures
        .into_iter()
        .take(MAX_SLIDES)
        .map(|p| slide(as_str(p.get("imgUrl")), as_str(p.get("clickUrl"))))
        .collect()
}

fn pgr_to_peebify_news(main: &Value) -> Value {
    envelope(
        pgr_collect_articles(main, &[TYPE_NOTICE]),
        pgr_collect_articles(main, &[TYPE_NEWS, TYPE_EVENT]),
        pgr_collect_slideshow(main),
    )
}

pub async fn get_pgr_news() -> Result<Value, String> {
    let main = http::get_json(&format!("{JSON_BASE}/MainMenu.json")).await?;
    Ok(pgr_to_peebify_news(&main))
}

// ------------ Girls' Frontline News ------------
// Girls' Frontline has no news site of its own, so this reads its Steam news and treats posts with words like
// maintenance or compensation in the title as notices.
const API_URL: &str = "https://api.steampowered.com/ISteamNews/GetNewsForApp/v2/";
const GF1_ARTICLE_URL: &str = "https://store.steampowered.com/news/app";
const APP_ID: &str = "3887700";

const FETCH_COUNT: usize = 40;

const NOTICE_KEYWORDS: [&str; 6] = [
    "maintenance",
    "notice",
    "compensation",
    "downtime",
    "hotfix",
    "server",
];

fn is_notice(title: &str) -> bool {
    let title = title.to_lowercase();
    NOTICE_KEYWORDS.iter().any(|kw| title.contains(kw))
}

pub(crate) fn local_month_day(timestamp: i64) -> String {
    use chrono::TimeZone;
    let millis = if timestamp < 10_000_000_000 {
        timestamp.saturating_mul(1000)
    } else {
        timestamp
    };
    match chrono::Local.timestamp_millis_opt(millis) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            dt.format("%m-%d").to_string()
        }
        chrono::LocalResult::None => String::new(),
    }
}

fn gf1_collect_articles(items: &[&Value], notice: bool) -> Vec<Value> {
    let mut matching: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|item| is_notice(as_str(item.get("title"))) == notice)
        .collect();

    matching.sort_by_key(|item| std::cmp::Reverse(as_i64(item.get("date"))));

    dedupe_take(matching, |item| as_i64(item.get("gid")))
        .into_iter()
        .map(|item| {
            article(
                as_str(item.get("title")),
                format!(
                    "{GF1_ARTICLE_URL}/{APP_ID}/view/{}",
                    as_str(item.get("gid"))
                ),
                local_month_day(as_i64(item.get("date"))),
            )
        })
        .collect()
}

fn gf1_to_peebify_news(appnews: &Value) -> Value {
    let empty = Vec::new();
    let items: Vec<&Value> = appnews
        .get("newsitems")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .filter(|item| !as_str(item.get("title")).is_empty())
        .collect();

    envelope(
        gf1_collect_articles(&items, true),
        gf1_collect_articles(&items, false),
        Vec::new(),
    )
}

pub async fn get_gf1_news() -> Result<Value, String> {
    let url = format!("{API_URL}?appid={APP_ID}&count={FETCH_COUNT}&maxlength=1");
    let response = http::get_json(&url).await?;
    Ok(gf1_to_peebify_news(
        response.get("appnews").unwrap_or(&Value::Null),
    ))
}

// ------------ Girls' Frontline 2 News ------------
// Reads the Sunborn website API for GF2: article buckets for notices and news, plus the rotating banner slides.
const API_BASE: &str = "https://gf2-web-us-api.sunborngame.com/website";
const GF2_ARTICLE_URL: &str = "https://gf2exilium.sunborngame.com/NewsInfo";

fn gf2_collect_articles(top: &Value, buckets: &[&str]) -> Vec<Value> {
    let empty = Vec::new();
    let mut items: Vec<&Value> = buckets
        .iter()
        .flat_map(|bucket| {
            top.get(bucket)
                .and_then(|v| v.as_array())
                .unwrap_or(&empty)
                .iter()
        })
        .filter(|a| !as_str(a.get("Title")).is_empty())
        .collect();

    items.sort_by(|a, b| as_str(b.get("Date")).cmp(as_str(a.get("Date"))));

    dedupe_take(items, |a| as_i64(a.get("Id")))
        .into_iter()
        .map(|a| {
            let id = as_i64(a.get("Id"));
            let type_id = as_i64(a.get("Type"));
            article(
                as_str(a.get("Title")),
                format!("{GF2_ARTICLE_URL}?id={id}&typeId={type_id}"),
                short_date(as_str(a.get("Date"))),
            )
        })
        .collect()
}

fn gf2_banner_url(slide: &Value) -> &str {
    let launcher = as_str(slide.get("LauncherUrl"));
    if launcher.is_empty() {
        as_str(slide.get("PicUrl"))
    } else {
        launcher
    }
}

fn gf2_collect_slideshow(rotation: &Value) -> Vec<Value> {
    let empty = Vec::new();
    let mut slides: Vec<&Value> = rotation
        .get("list")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .filter(|s| !gf2_banner_url(s).is_empty())
        .collect();

    slides.sort_by_key(|s| std::cmp::Reverse(as_i64(s.get("Sort"))));

    slides
        .into_iter()
        .take(MAX_SLIDES)
        .map(|s| slide(gf2_banner_url(s), as_str(s.get("JumpUrl"))))
        .collect()
}

fn gf2_to_peebify_news(top: &Value, rotation: &Value) -> Value {
    envelope(
        gf2_collect_articles(top, &["notice"]),
        gf2_collect_articles(top, &["newest", "news", "strategy"]),
        gf2_collect_slideshow(rotation),
    )
}

pub async fn get_gf2_news() -> Result<Value, String> {
    let top_url = format!("{API_BASE}/top_news_list");
    let rotation_url = format!("{API_BASE}/rotation");
    let (top, rotation) = tokio::join!(http::get_json(&top_url), http::get_json(&rotation_url));
    let top = top?;
    let rotation = rotation.unwrap_or_else(|e| {
        log::warn!("GF2 banner rotation failed ({e}), news will render without slides.");
        json!({})
    });
    Ok(gf2_to_peebify_news(
        top.get("data").unwrap_or(&Value::Null),
        rotation.get("data").unwrap_or(&Value::Null),
    ))
}

// ------------ Reverse: 1999 News ------------
// Reads the Bluepoch site for Reverse: 1999. Notices, events and broadcasts come from numbered collections.
const COLLECTION_NOTICE: i64 = 6;
const COLLECTION_EVENT: i64 = 7;
const COLLECTION_BROADCAST: i64 = 8;

const PAGE_SIZE: i64 = 20;
const DEFAULT_SITE: &str = "https://re1999.bluepoch.com/";

fn bp_site_url(profile: &Value) -> &str {
    profile
        .get("bpNewsUrl")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_SITE)
}

fn bp_collect(page: &[Value], site: &str, wanted: &[i64]) -> Vec<Value> {
    let mut items: Vec<&Value> = page
        .iter()
        .filter(|a| wanted.contains(&as_i64(a.get("collectionId"))))
        .filter(|a| !as_str(a.get("title")).trim().is_empty())
        .collect();

    items.sort_by(|a, b| {
        let pinned = |v: &Value| v.get("isTop").and_then(Value::as_bool).unwrap_or(false);
        pinned(b)
            .cmp(&pinned(a))
            .then_with(|| as_str(b.get("onlineTime")).cmp(as_str(a.get("onlineTime"))))
    });

    dedupe_take(items, |a| as_i64(a.get("id")))
        .into_iter()
        .map(|a| {
            article(
                as_str(a.get("title")).trim(),
                bp_article_url(a).unwrap_or(site),
                local_short_date(as_str(a.get("onlineTime")), UTC_PLUS_8),
            )
        })
        .collect()
}

fn bp_article_url(item: &Value) -> Option<&str> {
    ["jumpUrl", "linkUrl", "link"]
        .iter()
        .map(|key| as_str(item.get(*key)).trim())
        .find(|url| url.starts_with("https://") || url.starts_with("http://"))
}

fn bp_collect_slideshow(banner: &Value) -> Vec<Value> {
    let empty = Vec::new();
    banner
        .get("images")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter_map(|image| {
            let url = as_str(image.get("imageUrl"));
            (!url.is_empty()).then(|| slide(url, as_str(image.get("imageLink"))))
        })
        .take(MAX_SLIDES)
        .collect()
}

pub async fn get_bluepoch_news(profile: &Value) -> Result<Value, String> {
    let game_id = bluepoch::api_config(profile)?.game_id;
    let site = bp_site_url(profile);

    let (query, banner) = tokio::join!(
        bluepoch::activity_post(
            profile,
            "/activity/official/websites/information/query",
            json!({ "current": 1, "pageSize": PAGE_SIZE, "gameId": game_id }),
        ),
        bluepoch::activity_post(
            profile,
            "/activity/pc/launcher/banner",
            json!({ "gameId": game_id }),
        )
    );
    let query = query?;
    let banner = banner.unwrap_or_else(|e| {
        log::warn!("Bluepoch banners failed ({e}), news will render without slides.");
        Value::Null
    });

    let empty = Vec::new();
    let page = query
        .get("pageData")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    Ok(envelope(
        bp_collect(page, site, &[COLLECTION_NOTICE]),
        bp_collect(page, site, &[COLLECTION_EVENT, COLLECTION_BROADCAST]),
        bp_collect_slideshow(&banner),
    ))
}

// ------------ Brown Dust II News ------------
// Reads the Brown Dust II site feeds. Notices and maintenance go in the notice list, the rest is news.
const BD2_PAGE_SIZE: usize = 40;
const BD2_EVENT_PAGE_SIZE: usize = 12;
const BD2_NOTICE_CATEGORIES: [&str; 2] = ["notice", "inspection"];

fn bd2_article(item: &Value, site: &str, locale: &str) -> Option<Value> {
    let subject = as_str(item.get("subject")).trim();
    if subject.is_empty() {
        return None;
    }
    let jump = format!(
        "{}/{locale}/news/view?id={}",
        site.trim_end_matches('/'),
        as_str(item.get("id"))
    );
    Some(article(
        subject,
        jump,
        local_short_date(as_str(item.get("publishedAt")), 0),
    ))
}

fn bd2_collect(items: &[Value], notice: bool, site: &str, locale: &str) -> Vec<Value> {
    let mut seen: Vec<String> = Vec::with_capacity(MAX_ITEMS);
    items
        .iter()
        .filter(|item| {
            BD2_NOTICE_CATEGORIES.contains(&as_str(item.get("category"))) == notice
        })
        .filter(|item| {
            let id = as_str(item.get("id")).to_string();
            if id.is_empty() || seen.contains(&id) {
                return false;
            }
            seen.push(id);
            true
        })
        .filter_map(|item| bd2_article(item, site, locale))
        .take(MAX_ITEMS)
        .collect()
}

fn bd2_collect_slideshow(media: &Value) -> Vec<Value> {
    let empty = Vec::new();
    let mut items: Vec<&Value> = media.as_array().unwrap_or(&empty).iter().collect();
    items.sort_by_key(|entry| -as_i64(entry.get("priority")));
    items
        .into_iter()
        .filter_map(|entry| {
            let url = as_str(entry.get("posterUrl"));
            if url.is_empty() {
                return None;
            }
            let video = as_str(entry.get("videoId"));
            let jump = if video.is_empty() {
                String::new()
            } else {
                format!("https://www.youtube.com/watch?v={video}")
            };
            Some(slide(url, &jump))
        })
        .take(MAX_SLIDES)
        .collect()
}

pub async fn get_bd2_news(profile: &Value) -> Result<Value, String> {
    let api = bd2::news_api(profile).trim_end_matches('/').to_string();
    let site = bd2::news_site(profile).to_string();
    let locale = bd2::news_locale(profile).to_string();

    let page_url = format!("{api}/notices?locale={locale}&page=0&limit={BD2_PAGE_SIZE}");
    let events_url = format!(
        "{api}/notices?locale={locale}&page=0&limit={BD2_EVENT_PAGE_SIZE}&category=event"
    );
    let media_url = format!("{api}/media?locale={locale}");
    let (page, events, media) = tokio::join!(
        http::get_json(&page_url),
        http::get_json(&events_url),
        http::get_json(&media_url)
    );
    let page = page?;

    let empty = Vec::new();
    let mut items: Vec<Value> = page
        .get("items")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .clone();

    match events {
        Ok(events) => {
            if let Some(list) = events.get("items").and_then(Value::as_array) {
                items.extend(list.iter().cloned());
            }
        }
        Err(e) => log::warn!(
            "Brown Dust II events feed failed ({e}), the news bucket falls back to the general list."
        ),
    }

    items.sort_by(|a, b| as_str(b.get("publishedAt")).cmp(as_str(a.get("publishedAt"))));

    let media = media.unwrap_or_else(|e| {
        log::warn!("Brown Dust II media feed failed ({e}), news will render without slides.");
        Value::Null
    });

    Ok(envelope(
        bd2_collect(&items, true, &site, &locale),
        bd2_collect(&items, false, &site, &locale),
        bd2_collect_slideshow(&media),
    ))
}

#[cfg(test)]
mod bd2_tests {
    use super::*;

    fn page() -> Vec<Value> {
        vec![
            json!({
                "id": "01M2SD0PKYQ3MMA8MQ5DNWSJYH",
                "subject": "Collaboration PV Sharing Event — Results",
                "category": "event",
                "publishedAt": "2026-09-18T05:00:19.129Z"
            }),
            json!({
                "id": "01M2EVM39RAHH6Q42CNWF707ZE",
                "subject": "Notice of Live Update on September 16 (UTC)",
                "category": "inspection",
                "publishedAt": "2026-09-14T08:00:15.132Z"
            }),
            json!({
                "id": "01M2PGNJ0P386ZPVS1FW1AX4MC",
                "subject": "Wallpaper | Chained Soldier 2",
                "category": "notice",
                "publishedAt": "2026-09-17T07:00:26.019Z"
            }),
            json!({
                "id": "01M2PGNJ0P386ZPVS1FW1AX4MC",
                "subject": "Wallpaper | Chained Soldier 2",
                "category": "notice",
                "publishedAt": "2026-09-17T07:00:26.019Z"
            }),
            json!({
                "id": "01KZZE44B93AJK5483HAH9JCCF",
                "subject": "   ",
                "category": "shop",
                "publishedAt": "2026-09-12T01:00:00.000Z"
            }),
        ]
    }

    #[test]
    fn maintenance_rides_with_notices_and_everything_else_is_news() {
        let items = page();
        let notices = bd2_collect(&items, true, "https://www.browndust2.com", "en-us");
        let news = bd2_collect(&items, false, "https://www.browndust2.com", "en-us");

        assert_eq!(notices.len(), 2, "notice + inspection, deduped");
        assert_eq!(
            notices[0]["content"],
            "Notice of Live Update on September 16 (UTC)"
        );
        assert_eq!(notices[1]["content"], "Wallpaper | Chained Soldier 2");

        assert_eq!(news.len(), 1, "the blank-subject shop post is dropped");
        assert_eq!(news[0]["content"], "Collaboration PV Sharing Event — Results");
    }

    #[test]
    fn an_article_links_to_the_sites_own_viewer_and_shows_a_short_date() {
        let items = page();
        let news = bd2_collect(&items, false, "https://www.browndust2.com/", "en-us");
        assert_eq!(
            news[0]["jumpUrl"],
            "https://www.browndust2.com/en-us/news/view?id=01M2SD0PKYQ3MMA8MQ5DNWSJYH"
        );
        assert_eq!(
            news[0]["time"],
            local("2026-09-18T05:00:19.129Z"),
            "the ISO timestamp becomes the viewer's own date"
        );
    }

    fn local(rfc3339: &str) -> String {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%m-%d")
            .to_string()
    }

    #[test]
    fn publisher_times_are_read_in_their_own_zone() {
        assert_eq!(
            local_short_date("2026-09-26 10:00:00", UTC_PLUS_8),
            local("2026-09-26T10:00:00+08:00")
        );
        assert_eq!(
            local_short_date("2026-01-01T00:30:00", UTC_PLUS_8),
            local("2026-01-01T00:30:00+08:00")
        );
        assert_eq!(
            local_short_date("2026-09-18T05:00:19.129Z", UTC_PLUS_8),
            local("2026-09-18T05:00:19.129Z"),
            "an explicit zone wins over the publisher's"
        );
        assert_eq!(local_short_date("2026-09-18", UTC_PLUS_8), "09-18");
        assert_eq!(local_short_date("09/18", UTC_PLUS_8), "09-18");
        assert_eq!(local_short_date("", UTC_PLUS_8), "");
    }

    #[test]
    fn short_date_normalises_every_publisher_format() {
        assert_eq!(short_date("2026-09-18 10:00:00"), "09-18");
        assert_eq!(short_date("2026-09-18T05:00:19.129Z"), "09-18");
        assert_eq!(short_date("2026/9/8"), "09-08");
        assert_eq!(short_date("09/18"), "09-18");
        assert_eq!(short_date("2026.09.18"), "09-18");
        assert_eq!(short_date(""), "");
        assert_eq!(short_date("Sep 18, 2026"), "Sep 18, 2026");
        assert_eq!(short_date("2026-13-01"), "2026-13-01");
    }

    #[test]
    fn a_bluepoch_article_uses_its_own_link_when_the_feed_has_one() {
        let page = vec![
            json!({ "id": 1, "collectionId": 7, "title": "Event A", "onlineTime": "2026-09-18 10:00:00", "jumpUrl": "https://re1999.bluepoch.com/news/1" }),
            json!({ "id": 2, "collectionId": 8, "title": "Event B", "onlineTime": "2026-09-17 10:00:00" }),
        ];
        let news = bp_collect(&page, DEFAULT_SITE, &[COLLECTION_EVENT, COLLECTION_BROADCAST]);
        assert_eq!(news.len(), 2);
        assert_eq!(news[0]["jumpUrl"], "https://re1999.bluepoch.com/news/1");
        assert_eq!(news[1]["jumpUrl"], DEFAULT_SITE);
        assert_eq!(news[0]["time"], local("2026-09-18T10:00:00+08:00"));
    }

    #[test]
    fn the_envelope_carries_only_guidance_and_slides() {
        let out = envelope(Vec::new(), Vec::new(), Vec::new());
        assert!(out.get("social_media_list").is_none());
        assert!(out["guidance"]["notice"]["contents"].is_array());
        assert!(out["slideshow"].is_array());
    }

    #[test]
    fn slides_are_ordered_by_priority_and_point_at_the_video() {
        let media = json!([
            { "posterUrl": "https://x/a.png", "videoId": "aaa", "priority": 10 },
            { "posterUrl": "https://x/c.png", "videoId": "ccc", "priority": 30 },
            { "posterUrl": "", "videoId": "ddd", "priority": 40 },
            { "posterUrl": "https://x/b.png", "priority": 20 }
        ]);
        let slides = bd2_collect_slideshow(&media);
        assert_eq!(slides.len(), 3, "a poster-less entry is skipped");
        assert_eq!(slides[0]["url"], "https://x/c.png");
        assert_eq!(slides[0]["jumpUrl"], "https://www.youtube.com/watch?v=ccc");
        assert_eq!(slides[1]["url"], "https://x/b.png");
        assert_eq!(slides[1]["jumpUrl"], "", "no videoId means no link");
        assert_eq!(slides[2]["url"], "https://x/a.png");
    }

    #[test]
    fn an_empty_media_feed_is_not_an_error() {
        assert!(bd2_collect_slideshow(&Value::Null).is_empty());
        assert!(bd2_collect_slideshow(&json!([])).is_empty());
    }
}

pub(super) use channels::get_news_data;

// ------------ News Command and Cache ------------
// The get-news-data command. It picks the right adapter for a game, keeps results for ten minutes, and falls back
// to the last good copy if a fetch fails.
mod channels {
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};
    use tauri::{AppHandle, Manager};

    use super::super::state::BackendState;
    use super::super::{
        arg_str, err_response, game_profiles, hoyoplay, http, hypergryph, ok_with,
        resolve_profile,
    };

    const NEWS_CACHE_TTL: Duration = Duration::from_secs(60 * 10);

    static NEWS_CACHE: http::TtlCache<Value> =
        http::TtlCache::new(NEWS_CACHE_TTL, game_profiles::GAME_IDS.len());

    fn news_response(game_id: &str, news: Value) -> Value {
        ok_with(json!({ "gameId": game_id, "data": news }))
    }

    fn news_counts(news: &Value) -> String {
        let count = |pointer: &str| {
            news.pointer(pointer)
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        };
        format!(
            "notices={} news={} slides={}",
            count("/guidance/notice/contents"),
            count("/guidance/news/contents"),
            count("/slideshow")
        )
    }

    fn serve_fetched(
        game_id: &str,
        source: &str,
        started: Instant,
        fetched: Result<Value, String>,
        stale: Option<Value>,
    ) -> Value {
        match fetched {
            Ok(news) => {
                log::info!(
                    "news: {game_id} via {source} ok {} in {} ms",
                    news_counts(&news),
                    started.elapsed().as_millis()
                );
                NEWS_CACHE.set(game_id, news.clone());
                news_response(game_id, news)
            }
            Err(e) => stale_or_fail(game_id, source, stale, &e),
        }
    }

    fn stale_or_fail(game_id: &str, source: &str, stale: Option<Value>, error: &str) -> Value {
        match stale {
            Some(data) => {
                log::warn!("news: {game_id} via {source} failed ({error}); served stale");
                news_response(game_id, data)
            }
            None => {
                log::error!("news: {game_id} via {source} failed ({error}); nothing cached to serve");
                err_response("Failed to load news data")
            }
        }
    }

    async fn fetch_hoyoplay_news(profile: &Value) -> Result<Value, String> {
        let Some(hoyo_game_id) = profile.get("hoyoGameId").and_then(Value::as_str) else {
            return Err("profile has no hoyoGameId".to_string());
        };
        let content = hoyoplay::get_game_content(hoyo_game_id).await?;
        Ok(hoyoplay::to_peebify_news(&content))
    }

    async fn fetch_adapter_news(profile: &Value) -> Option<(&'static str, Result<Value, String>)> {
        let install_mode = game_profiles::install_mode(profile);
        let news_source = profile.get("newsSource").and_then(|v| v.as_str());
        let pair = match (install_mode, news_source) {
            (Some("sophon"), _) => ("HoYoPlay", fetch_hoyoplay_news(profile).await),
            (Some("hypergryph"), _) => ("Hypergryph", hypergryph::get_news(profile).await),
            (Some("gf2"), _) => ("GF2", super::get_gf2_news().await),
            (Some("bluepoch"), _) => ("Bluepoch", super::get_bluepoch_news(profile).await),
            (Some("bd2"), _) => ("Brown Dust II", super::get_bd2_news(profile).await),
            (_, Some("pgr")) => ("PGR", super::get_pgr_news().await),
            (_, Some("gf1")) => ("GF1", super::get_gf1_news().await),
            (Some("netease"), _) => ("NTE", super::nte_site::get_nte_news().await),
            _ => return None,
        };
        Some(pair)
    }

    pub(crate) async fn get_news_data(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
        let profile = resolve_profile(app, arg_str(args, 0));
        let game_id = game_profiles::profile_id(profile);

        let cached = NEWS_CACHE.get_aged(game_id);
        if let Some((data, true)) = &cached {
            return Ok(news_response(game_id, data.clone()));
        }
        let stale = cached.map(|(data, _)| data);
        let started = Instant::now();

        if let Some((source, fetched)) = fetch_adapter_news(profile).await {
            return Ok(serve_fetched(game_id, source, started, fetched, stale));
        }

        let api_config = app.state::<BackendState>().api_config.clone();
        if !api_config.is_loaded() && http::is_online().await {
            if let Err(e) = api_config.initialize().await {
                log::warn!("news: API config still empty after lazy init attempt: {e}");
            }
        }

        let client_key = profile
            .get("apiClientKey")
            .and_then(|v| v.as_str())
            .unwrap_or(game_id);
        let fetched = match api_config.news_url_for_client(Some(client_key)) {
            Some(url) => http::get_json(&url).await,
            None => Err("the API config is not loaded".to_string()),
        };
        Ok(serve_fetched(game_id, "api.json", started, fetched, stale))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn news_counts_reads_the_envelope() {
            let payload = super::super::envelope(
                vec![json!({}), json!({})],
                vec![json!({})],
                Vec::new(),
            );
            assert_eq!(news_counts(&payload), "notices=2 news=1 slides=0");
            assert_eq!(news_counts(&json!({})), "notices=0 news=0 slides=0");
        }

        #[test]
        fn failure_serves_stale_without_touching_the_cache() {
            let key = "news-test-stale";
            let stale = json!({ "slideshow": [1] });
            let served = serve_fetched(
                key,
                "test",
                Instant::now(),
                Err("offline".to_string()),
                Some(stale.clone()),
            );
            assert_eq!(served["success"], json!(true));
            assert_eq!(served["gameId"], json!(key));
            assert_eq!(served["data"], stale);
            assert!(NEWS_CACHE.get_aged(key).is_none());
        }

        #[test]
        fn failure_without_stale_is_an_error_and_is_not_cached() {
            let key = "news-test-empty";
            let served = serve_fetched(key, "test", Instant::now(), Err("offline".to_string()), None);
            assert_eq!(served["success"], json!(false));
            assert!(NEWS_CACHE.get_aged(key).is_none());
        }

        #[test]
        fn success_is_cached_under_the_requested_game() {
            let key = "news-test-ok";
            let payload = super::super::envelope(Vec::new(), Vec::new(), Vec::new());
            let served = serve_fetched(key, "test", Instant::now(), Ok(payload.clone()), None);
            assert_eq!(served["gameId"], json!(key));
            assert_eq!(NEWS_CACHE.get(key), Some(payload));
        }
    }
}

// ------------ Neverness to Everness News ------------
// Neverness to Everness posts news on a plain HTML site, so this scrapes the list pages and article banners and
// builds the slideshow from the newest articles.
mod nte_site {
    use std::sync::OnceLock;
    use std::time::Duration;

    use regex::Regex;
    use serde_json::{json, Value};

    use super::super::http;

    const NTE_ORIGIN: &str = "https://nte.perfectworld.com";
    const SLIDESHOW_ARTICLE_COUNT: usize = 5;
    const LIST_PAGES_PER_SECTION: usize = 3;
    const BANNER_CACHE_TTL: Duration = Duration::from_secs(60 * 60 * 12);

    static BANNER_CACHE: http::TtlCache<Option<String>> = http::TtlCache::new(BANNER_CACHE_TTL, 64);

    const NEWS_SECTION: &str = "gamenews";
    const NOTICES_SECTION: &str = "gamebroad";

    fn list_page_url(section: &str, page: usize) -> String {
        let file = if page == 0 {
            "index.html".to_string()
        } else {
            format!("index{page}.html")
        };
        format!("{NTE_ORIGIN}/en/article/news/{section}/{file}")
    }

    fn entity_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(r"&(#[xX][0-9a-fA-F]+|#[0-9]+|[a-zA-Z][a-zA-Z0-9]*);").expect("static regex")
        })
    }

    fn tag_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"<[^>]*>").expect("static regex"))
    }

    fn list_item_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(
                r#"<a\s[^>]*?href="([^"]+)"[^>]*>\s*<div class="listItem">[\s\S]*?<h2 class="title">([\s\S]*?)</h2>[\s\S]*?<p class="date">([\s\S]*?)</p>"#,
            )
            .expect("static regex")
        })
    }

    fn article_content_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(
                r#"<div class="articleContent">([\s\S]*?)</div>\s*<div class="rel articleBottom""#,
            )
            .expect("static regex")
        })
    }

    fn img_src_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r#"<img[^>]+src="([^"]+)""#).expect("static regex"))
    }

    fn decode_html_entities(text: &str) -> String {
        entity_re()
            .replace_all(text, |caps: &regex::Captures<'_>| {
                let body = &caps[1];
                if let Some(numeric) = body.strip_prefix('#') {
                    let code = if let Some(hex) = numeric
                        .strip_prefix('x')
                        .or_else(|| numeric.strip_prefix('X'))
                    {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        numeric.parse::<u32>().ok()
                    };
                    return match code.and_then(char::from_u32) {
                        Some(c) => c.to_string(),
                        None => caps[0].to_string(),
                    };
                }
                match body {
                    "amp" => "&".to_string(),
                    "lt" => "<".to_string(),
                    "gt" => ">".to_string(),
                    "quot" => "\"".to_string(),
                    "apos" => "'".to_string(),
                    "nbsp" => " ".to_string(),
                    "lsquo" => "\u{2018}".to_string(),
                    "rsquo" => "\u{2019}".to_string(),
                    "ldquo" => "\u{201C}".to_string(),
                    "rdquo" => "\u{201D}".to_string(),
                    "ndash" => "\u{2013}".to_string(),
                    "mdash" => "\u{2014}".to_string(),
                    "hellip" => "\u{2026}".to_string(),
                    _ => caps[0].to_string(),
                }
            })
            .into_owned()
    }

    fn strip_tags(html: &str) -> String {
        tag_re().replace_all(html, "").into_owned()
    }

    fn clean_text(raw: &str) -> String {
        decode_html_entities(&strip_tags(raw)).trim().to_string()
    }

    struct ListItem {
        url: String,
        title: String,
        date: String,
    }

    fn parse_news_list_page(html: &str, page_url: &str) -> Vec<ListItem> {
        let base = url::Url::parse(page_url).ok();
        list_item_re()
            .captures_iter(html)
            .filter_map(|caps| {
                let href = &caps[1];
                let url = base.as_ref()?.join(href).ok()?.to_string();
                Some(ListItem {
                    url,
                    title: clean_text(&caps[2]),
                    date: clean_text(&caps[3]),
                })
            })
            .collect()
    }

    fn parse_article_banner_image(html: &str, article_url: &str) -> Option<String> {
        let content = article_content_re().captures(html)?;
        let img = img_src_re().captures(content.get(1)?.as_str())?;
        url::Url::parse(article_url)
            .ok()?
            .join(&img[1])
            .ok()
            .map(|u| u.to_string())
    }

    async fn fetch_news_list(url: String) -> Result<Vec<ListItem>, String> {
        let html = http::get_text(&url)
            .await
            .map_err(|e| format!("{url}: {e}"))?;
        Ok(parse_news_list_page(&html, &url))
    }

    async fn fetch_news_section(section: &str) -> (Vec<ListItem>, Vec<String>, usize) {
        let mut seen = std::collections::HashSet::new();
        let mut items = Vec::new();
        let mut failures = Vec::new();
        let mut requests = 0;
        for page in 0..LIST_PAGES_PER_SECTION {
            if items.len() >= super::MAX_ITEMS {
                break;
            }
            requests += 1;
            match fetch_news_list(list_page_url(section, page)).await {
                Ok(page) if page.is_empty() => break,
                Ok(page) => items.extend(page.into_iter().filter(|item| seen.insert(item.url.clone()))),
                Err(e) => failures.push(e),
            }
        }
        (items, failures, requests)
    }

    fn check_list_failures(failures: &[String], requests: usize) -> Result<(), String> {
        let Some(first) = failures.first() else {
            return Ok(());
        };
        if failures.len() >= requests {
            return Err(format!(
                "all {requests} NTE list pages failed, first error: {first}"
            ));
        }
        log::warn!(
            "NTE news: {} of {requests} list pages failed, first error: {first}",
            failures.len()
        );
        Ok(())
    }

    async fn fetch_article_banner(item_url: String) -> Option<Value> {
        if let Some(cached) = BANNER_CACHE.get(&item_url) {
            let image = cached?;
            return Some(json!({ "url": image, "jumpUrl": item_url }));
        }
        let image = match http::get_text(&item_url).await {
            Ok(html) => parse_article_banner_image(&html, &item_url),
            Err(e) => {
                log::warn!("NTE news: failed to fetch article {item_url}: {e}");
                return None;
            }
        };
        BANNER_CACHE.set(&item_url, image.clone());
        Some(json!({ "url": image?, "jumpUrl": item_url }))
    }

    pub async fn get_nte_news() -> Result<Value, String> {
        let (
            (news_items, news_failures, news_requests),
            (notice_items, notice_failures, notice_requests),
        ) = tokio::join!(
            fetch_news_section(NEWS_SECTION),
            fetch_news_section(NOTICES_SECTION),
        );
        let failures: Vec<String> = news_failures.into_iter().chain(notice_failures).collect();
        check_list_failures(&failures, news_requests + notice_requests)?;
        if news_items.is_empty() && notice_items.is_empty() {
            return Err("NTE news: no articles parsed from any list page".to_string());
        }

        let banner_futures = news_items
            .iter()
            .take(SLIDESHOW_ARTICLE_COUNT)
            .map(|item| fetch_article_banner(item.url.clone()));
        let banner_slides: Vec<Value> = futures::future::join_all(banner_futures)
            .await
            .into_iter()
            .flatten()
            .collect();

        let as_guidance_item = |item: &ListItem| {
            super::article(&item.title, &item.url, super::short_date(&item.date))
        };

        Ok(super::envelope(
            notice_items
                .iter()
                .take(super::MAX_ITEMS)
                .map(as_guidance_item)
                .collect(),
            news_items
                .iter()
                .take(super::MAX_ITEMS)
                .map(as_guidance_item)
                .collect(),
            banner_slides,
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn list_failures_error_only_when_every_page_failed() {
            assert!(check_list_failures(&[], 6).is_ok());
            let some = ["a".to_string(), "b".to_string()];
            assert!(check_list_failures(&some, 6).is_ok());
            let all: Vec<String> = (0..6).map(|i| i.to_string()).collect();
            let err = check_list_failures(&all, 6).unwrap_err();
            assert!(err.contains("all 6 NTE list pages failed"));
        }

        #[test]
        fn titles_decode_named_decimal_and_hex_entities() {
            assert_eq!(
                decode_html_entities("It&rsquo;s &ldquo;here&rdquo; &mdash; 1&ndash;2&hellip;"),
                "It\u{2019}s \u{201C}here\u{201D} \u{2014} 1\u{2013}2\u{2026}"
            );
            assert_eq!(decode_html_entities("&#39;&#x27;&#X27;"), "'''");
            assert_eq!(decode_html_entities("Tom &amp; Jerry &lt;3"), "Tom & Jerry <3");
            assert_eq!(decode_html_entities("&#12a; &bogus;"), "&#12a; &bogus;");
        }
    }
}
