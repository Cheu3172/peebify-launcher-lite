// ------------ HoYoPlay API ------------
// Small client for the Hoyoverse launcher's web API, used for each game's news and banners and to build the download branch URLs for the Hoyoverse games.
use serde_json::Value;

use super::http;

const BASE_URL: &str = "https://sg-hyp-api.hoyoverse.com/hyp/hyp-connect/api";
const DEFAULT_LAUNCHER_ID: &str = "VYTpXlbWo8";
const DEFAULT_LANG: &str = "en-us";

pub(super) fn build_url(endpoint: &str, params: &[(&str, &str)]) -> String {
    let mut url = url::Url::parse(&format!("{BASE_URL}/{endpoint}"))
        .expect("BASE_URL + endpoint is always a valid URL");
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("launcher_id", DEFAULT_LAUNCHER_ID);
        query.append_pair("language", DEFAULT_LANG);
        for (k, v) in params {
            query.append_pair(k, v);
        }
    }
    url.to_string()
}

pub async fn get_game_content(game_id: &str) -> Result<Value, String> {
    http::get_json(&build_url("getGameContent", &[("game_id", game_id)])).await
}

pub fn to_peebify_news(content_response: &Value) -> Value {
    let content = &content_response["data"]["content"];
    let empty: Vec<Value> = Vec::new();
    let posts = content["posts"].as_array().unwrap_or(&empty);
    let banners = content["banners"].as_array().unwrap_or(&empty);

    let as_item = |p: &Value| {
        super::news::article(
            p.get("title").and_then(|v| v.as_str()).unwrap_or(""),
            p.get("link").and_then(|v| v.as_str()).unwrap_or(""),
            super::news::short_date(p.get("date").and_then(|v| v.as_str()).unwrap_or("")),
        )
    };

    let titled = |p: &&Value| !p["title"].as_str().unwrap_or("").trim().is_empty();
    let notice: Vec<Value> = posts
        .iter()
        .filter(|p| p["type"].as_str() == Some("POST_TYPE_ANNOUNCE"))
        .filter(titled)
        .take(super::news::MAX_ITEMS)
        .map(as_item)
        .collect();
    let news: Vec<Value> = posts
        .iter()
        .filter(|p| p["type"].as_str() != Some("POST_TYPE_ANNOUNCE"))
        .filter(titled)
        .take(super::news::MAX_ITEMS)
        .map(as_item)
        .collect();
    let slideshow: Vec<Value> = banners
        .iter()
        .filter_map(|b| {
            let url = b["image"]["url"].as_str().unwrap_or("");
            (!url.is_empty())
                .then(|| super::news::slide(url, b["image"]["link"].as_str().unwrap_or("")))
        })
        .take(super::news::MAX_SLIDES)
        .collect();

    super::news::envelope(notice, news, slideshow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn news_drops_blank_posts_and_imageless_banners_and_is_capped() {
        let mut posts: Vec<Value> = (0..20)
            .map(|i| json!({ "type": "POST_TYPE_INFO", "title": format!("news {i}"), "link": "https://j", "date": "09/18" }))
            .collect();
        posts.push(json!({ "type": "POST_TYPE_ANNOUNCE", "title": "  ", "link": "https://j", "date": "09/18" }));
        posts.push(json!({ "type": "POST_TYPE_ANNOUNCE", "title": "Maintenance", "link": "https://j", "date": "09/18" }));
        let banners: Vec<Value> = (0..8)
            .map(|i| {
                let url = if i == 0 { String::new() } else { format!("https://img/{i}") };
                json!({ "image": { "url": url, "link": "https://b" } })
            })
            .collect();
        let out = to_peebify_news(&json!({ "data": { "content": { "posts": posts, "banners": banners } } }));
        let notices = out["guidance"]["notice"]["contents"].as_array().unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0]["content"], "Maintenance");
        assert_eq!(
            out["guidance"]["news"]["contents"].as_array().unwrap().len(),
            super::super::news::MAX_ITEMS
        );
        let slides = out["slideshow"].as_array().unwrap();
        assert_eq!(slides.len(), super::super::news::MAX_SLIDES);
        assert_eq!(slides[0]["url"], "https://img/1");
        assert_eq!(slides[0]["jumpUrl"], "https://b");
    }
}
