// ------------ Nexon Forum News ------------
// Reads a game's Nexon community forum (Blue Archive) for the home screen. The forum page is rendered in the browser from a small JSON API:
// one call lists a board's threads, one call describes the community and carries its banner carousel.
use serde_json::Value;

use super::http;
use super::news::{article, as_i64, as_str, envelope, local_month_day, slide, MAX_ITEMS, MAX_SLIDES};

fn profile_str<'a>(profile: &'a Value, key: &str) -> &'a str {
    profile.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

fn profile_boards(profile: &Value, key: &str) -> Vec<String> {
    let mut boards: Vec<String> = Vec::new();
    for entry in profile.get(key).and_then(Value::as_array).into_iter().flatten() {
        let id = match entry {
            Value::String(text) => text.trim().to_string(),
            Value::Number(number) => number.to_string(),
            _ => continue,
        };
        if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) && !boards.contains(&id) {
            boards.push(id);
        }
    }
    boards
}

fn is_plain_token(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn thread_url(base: &str, alias: &str, board: &str, thread: &str) -> String {
    format!("{base}/{alias}/board_view?board={board}&thread={thread}")
}

fn threads_url(base: &str, alias: &str, board: &str) -> String {
    format!(
        "{base}/api/v1/board/{board}/threads?alias={alias}&pageNo=1&blockStartKey=&blockStartNo=&paginationType=PAGING\
         &pageSize={MAX_ITEMS}&blockSize=5&hideType=WEB&headlineId=&searchKeywordType=THREAD_TITLE_AND_CONTENT&keywords="
    )
}

fn community_url(base: &str, alias: &str, country: &str) -> String {
    format!("{base}/api/v1/community/{alias}?alias={alias}&countryCode={country}")
}

fn error_of(body: &Value) -> Option<String> {
    let code = body.get("errorCode").and_then(Value::as_i64)?;
    let message = body.get("error").and_then(Value::as_str).unwrap_or("no message");
    Some(format!("Nexon forum answered error {code}: {message}"))
}

fn visible_threads(body: &Value) -> Vec<&Value> {
    body.get("threads")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|thread| {
            let hidden = thread.get("isDelete").and_then(Value::as_bool).unwrap_or(false)
                || thread.get("isWebHide").and_then(Value::as_bool).unwrap_or(false);
            let live = thread.get("release").and_then(Value::as_str).map_or(true, |r| r == "ON");
            !hidden && live && !as_str(thread.get("title")).trim().is_empty()
        })
        .collect()
}

// Rows are (publish time, thread id, article) so several boards can be merged newest first.
fn rows_from(body: &Value, base: &str, alias: &str, board: &str) -> Vec<(i64, i64, Value)> {
    visible_threads(body)
        .into_iter()
        .map(|thread| {
            let id = as_str(thread.get("threadId"));
            let at = as_i64(thread.get("createDate"));
            (
                at,
                as_i64(thread.get("threadId")),
                article(
                    as_str(thread.get("title")).trim(),
                    thread_url(base, alias, board, id),
                    local_month_day(at),
                ),
            )
        })
        .collect()
}

fn merge_rows(mut rows: Vec<(i64, i64, Value)>) -> Vec<Value> {
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let mut seen: Vec<i64> = Vec::new();
    let mut merged = Vec::new();
    for (_, id, row) in rows {
        if id != 0 && seen.contains(&id) {
            continue;
        }
        seen.push(id);
        merged.push(row);
        if merged.len() == MAX_ITEMS {
            break;
        }
    }
    merged
}

// The forum's banner carousel links to a full address, or occasionally just a thread number.
fn slides_from(community: &Value, base: &str, alias: &str) -> Vec<Value> {
    let banners = community
        .pointer("/communityHome/webPlace/pcBanners")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let home = format!("{base}/{alias}");
    let mut seen: Vec<&str> = Vec::new();
    let mut slides = Vec::new();
    for banner in banners {
        let image = as_str(banner.get("webImageUrl")).trim();
        if !image.starts_with("https://") || seen.contains(&image) {
            continue;
        }
        let target = as_str(banner.get("linkValue")).trim();
        let jump = if target.starts_with("https://") {
            target.to_string()
        } else if !target.is_empty() && target.chars().all(|c| c.is_ascii_digit()) {
            format!("{home}/board_view?thread={target}")
        } else {
            home.clone()
        };
        seen.push(image);
        slides.push(slide(image, &jump));
        if slides.len() == MAX_SLIDES {
            break;
        }
    }
    slides
}

async fn get_body(url: &str) -> Result<Value, String> {
    let body = http::get_json(url).await?;
    match error_of(&body) {
        Some(error) => Err(error),
        None => Ok(body),
    }
}

async fn board_rows(base: &str, alias: &str, board: &str) -> Result<Vec<(i64, i64, Value)>, String> {
    let body = get_body(&threads_url(base, alias, board)).await?;
    Ok(rows_from(&body, base, alias, board))
}

async fn boards_rows(base: &str, alias: &str, boards: &[String]) -> (Vec<Value>, Option<String>) {
    let requests = boards.iter().map(|board| board_rows(base, alias, board));
    let mut rows = Vec::new();
    let mut loaded = false;
    let mut last_error = None;
    for result in futures::future::join_all(requests).await {
        match result {
            Ok(found) => {
                loaded = true;
                rows.extend(found);
            }
            Err(e) => last_error = Some(e),
        }
    }
    let error = if loaded { None } else { last_error };
    (merge_rows(rows), error)
}

pub async fn get_news(profile: &Value) -> Result<Value, String> {
    let base = profile_str(profile, "nexonForumUrl").trim_end_matches('/');
    let alias = profile_str(profile, "nexonForumAlias");
    let country = match profile_str(profile, "nexonCountryCode") {
        "" => "US",
        country => country,
    };
    if !base.starts_with("https://") || !is_plain_token(alias) || !is_plain_token(country) {
        return Err("profile is missing a valid Nexon forum address".to_string());
    }
    let notice_boards = profile_boards(profile, "nexonNoticeBoards");
    let news_boards = profile_boards(profile, "nexonNewsBoards");

    let community_address = community_url(base, alias, country);
    let ((notices, notice_error), (news, news_error), community) = futures::join!(
        boards_rows(base, alias, &notice_boards),
        boards_rows(base, alias, &news_boards),
        get_body(&community_address),
    );

    let slides = match &community {
        Ok(body) => slides_from(body, base, alias),
        Err(e) => {
            log::warn!("news: Nexon forum banners failed ({e}); showing none");
            Vec::new()
        }
    };
    if notices.is_empty() && news.is_empty() && slides.is_empty() {
        return Err(notice_error
            .or(news_error)
            .or(community.err())
            .unwrap_or_else(|| "the Nexon forum returned no posts".to_string()));
    }
    Ok(envelope(notices, news, slides))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BASE: &str = "https://forum.nexon.com";
    const ALIAS: &str = "bluearchive-en";

    #[test]
    fn threads_become_articles_that_open_on_the_forum() {
        let body = json!({ "threads": [
            { "threadId": "3554410", "title": " Toy Pre-Orders ", "createDate": 1790668800, "release": "ON", "isSticky": true },
            { "threadId": "3550470", "title": "Developer's Letter", "createDate": 1790132400, "release": "ON" },
            { "threadId": "1", "title": "Deleted", "createDate": 1, "isDelete": true },
            { "threadId": "2", "title": "Hidden on web", "createDate": 2, "isWebHide": true },
            { "threadId": "3", "title": "Draft", "createDate": 3, "release": "OFF" },
            { "threadId": "4", "title": "   ", "createDate": 4 },
        ]});
        let rows = rows_from(&body, BASE, ALIAS, "3028");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].2["content"], "Toy Pre-Orders");
        assert_eq!(
            rows[0].2["jumpUrl"],
            "https://forum.nexon.com/bluearchive-en/board_view?board=3028&thread=3554410"
        );
    }

    #[test]
    fn boards_merge_newest_first_without_repeats_and_are_capped() {
        let mut rows = vec![
            (100, 1, article("Old", "https://x/1", "")),
            (300, 3, article("New", "https://x/3", "")),
            (200, 2, article("Mid", "https://x/2", "")),
            (300, 3, article("New again", "https://x/3", "")),
        ];
        let merged = merge_rows(rows.clone());
        let titles: Vec<&str> = merged.iter().filter_map(|a| a["content"].as_str()).collect();
        assert_eq!(titles, vec!["New", "Mid", "Old"]);
        for n in 10..40 {
            rows.push((n, n, article("Filler", "https://x/f", "")));
        }
        assert_eq!(merge_rows(rows).len(), MAX_ITEMS);
    }

    #[test]
    fn forum_banners_become_slides_with_safe_links() {
        let community = json!({ "communityHome": { "webPlace": { "pcBanners": [
            { "webImageUrl": "https://cdn.example/a.png", "linkType": "WEB_LINK", "linkValue": "https://forum.nexon.com/bluearchive-en/board_view?board=3218&thread=1" },
            { "webImageUrl": "https://cdn.example/a.png", "linkValue": "https://forum.nexon.com/other" },
            { "webImageUrl": "https://cdn.example/b.png", "linkValue": "3550498" },
            { "webImageUrl": "https://cdn.example/c.png", "linkValue": "javascript:alert(1)" },
            { "webImageUrl": "http://cdn.example/insecure.png", "linkValue": "3" },
            { "webImageUrl": "", "linkValue": "4" },
        ]}}});
        let slides = slides_from(&community, BASE, ALIAS);
        assert_eq!(slides.len(), 3);
        assert_eq!(slides[0]["jumpUrl"], "https://forum.nexon.com/bluearchive-en/board_view?board=3218&thread=1");
        assert_eq!(slides[1]["jumpUrl"], "https://forum.nexon.com/bluearchive-en/board_view?thread=3550498");
        assert_eq!(slides[2]["jumpUrl"], "https://forum.nexon.com/bluearchive-en");
    }

    #[test]
    fn board_ids_must_be_plain_numbers() {
        let profile = json!({ "nexonNoticeBoards": ["3028", 3219, "3028", "../x", "", true] });
        assert_eq!(profile_boards(&profile, "nexonNoticeBoards"), vec!["3028".to_string(), "3219".to_string()]);
        assert!(profile_boards(&json!({}), "nexonNoticeBoards").is_empty());
    }

    #[test]
    fn an_error_body_is_an_error_not_empty_news() {
        let failed = json!({ "errorCode": 4, "error": "invalid parameter" });
        assert!(error_of(&failed).unwrap().contains("invalid parameter"));
        assert!(error_of(&json!({ "threads": [] })).is_none());
    }
}
