// ------------ HTTP and Network Helpers ------------
// The shared web client every part of the backend uses, plus helpers for size-capped reads, retries with backoff,
// conditional GETs and a small TTL cache. It also runs the background monitor that tells the UI when we go online or offline.

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
pub const USER_AGENT: &str = concat!("PeebifyLauncher/", env!("CARGO_PKG_VERSION"));

const MAX_RETRY_DELAY_MS: u64 = 30_000;

// reqwest is built without a bundled crypto provider so the launcher keeps using ring; rustls needs
// it installed as the process default before the first client is built.
pub fn builder() -> reqwest::ClientBuilder {
    static PROVIDER: std::sync::Once = std::sync::Once::new();
    PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    reqwest::Client::builder()
}

pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        builder()
            .timeout(HTTP_TIMEOUT)
            .tcp_nodelay(true)
            .pool_idle_timeout(Duration::from_secs(15))
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest client construction cannot fail with these options")
    })
}

pub fn download_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        builder()
            .connect_timeout(HTTP_TIMEOUT)
            .read_timeout(HTTP_TIMEOUT)
            .no_gzip()
            .tcp_nodelay(true)
            .pool_max_idle_per_host(32)
            .pool_idle_timeout(Duration::from_secs(90))
            .http2_adaptive_window(true)
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest client construction cannot fail with these options")
    })
}

pub const MAX_IN_MEMORY_RESPONSE: u64 = 32 * 1024 * 1024;
pub const MAX_FEED_RESPONSE: u64 = 8 * 1024 * 1024;

#[cfg(debug_assertions)]
pub fn is_safe_override_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    match parsed.scheme() {
        "https" => true,
        "http" => matches!(
            parsed.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("[::1]") | Some("::1")
        ),
        _ => false,
    }
}

pub(crate) async fn read_capped(response: reqwest::Response, url: &str) -> Result<bytes::Bytes, String> {
    read_capped_to(response, url, MAX_IN_MEMORY_RESPONSE).await
}

pub(crate) async fn read_capped_to(
    response: reqwest::Response,
    url: &str,
    limit: u64,
) -> Result<bytes::Bytes, String> {
    use futures::StreamExt;

    if let Some(len) = response.content_length() {
        if len > limit {
            return Err(format!(
                "Refusing {url}: it declares {len} bytes, over the {limit}-byte limit."
            ));
        }
    }

    let mut collected = bytes::BytesMut::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Stream error: {}", describe(&e)))?;
        if collected.len() as u64 + chunk.len() as u64 > limit {
            return Err(format!(
                "Refusing {url}: the response exceeded the {limit}-byte limit."
            ));
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(collected.freeze())
}

pub(crate) async fn read_text_capped(
    response: reqwest::Response,
    url: &str,
    limit: u64,
) -> Result<String, String> {
    let bytes = read_capped_to(response, url, limit).await?;
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes[..]);
    Ok(String::from_utf8_lossy(body).into_owned())
}

const INTERRUPTED_CAUSE: &str = "the connection was interrupted";

fn cause_text(cause: &(dyn std::error::Error + 'static)) -> String {
    let text = cause.to_string();
    if text.to_ascii_lowercase().contains("cancel") {
        INTERRUPTED_CAUSE.to_string()
    } else {
        text
    }
}

fn causes(error: &reqwest::Error) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let text = cause_text(cause);
        if !text.is_empty() && !out.contains(&text) {
            out.push(text);
        }
        source = cause.source();
    }
    out
}

fn join_causes(head: String, causes: &[String]) -> String {
    let mut text = head;
    for cause in causes {
        if !text.contains(cause.as_str()) {
            text.push_str(": ");
            text.push_str(cause);
        }
    }
    text
}

pub fn describe(error: &reqwest::Error) -> String {
    join_causes(error.to_string(), &causes(error))
}

pub async fn get_text(url: &str) -> Result<String, String> {
    let response = client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Request error: {}", describe(&e)))?;
    let status = response.status().as_u16();
    note_reachable();
    if status != 200 {
        return Err(format!("HTTP {status} for {url}"));
    }
    let bytes = read_capped(response, url).await?;
    String::from_utf8(bytes.to_vec()).map_err(|e| format!("Invalid UTF-8 from {url}: {e}"))
}

pub async fn get_json(url: &str) -> Result<Value, String> {
    let text = get_text(url).await?;
    serde_json::from_str(&text).map_err(|e| format!("Invalid JSON from {url}: {e}"))
}

pub async fn get_bytes(url: &str) -> Result<bytes::Bytes, String> {
    let response = download_client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Request error: {}", describe(&e)))?;
    let status = response.status().as_u16();
    note_reachable();
    note_protocol(url, response.version());
    if status != 200 {
        return Err(format!("HTTP {status} for {url}"));
    }
    read_capped(response, url).await
}

pub fn note_protocol(url: &str, version: reqwest::Version) {
    static SEEN: OnceLock<parking_lot::Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "unknown host".to_string());
    let key = format!("{host} {version:?}");
    if SEEN
        .get_or_init(|| parking_lot::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .insert(key)
    {
        log::info!("transfer: {host} answered over {version:?}");
    }
}

pub async fn content_length(url: &str) -> Option<u64> {
    if let Some(length) = head_length(url).await {
        return Some(length);
    }
    ranged_length(url).await
}

async fn head_length(url: &str) -> Option<u64> {
    let response = download_client().head(url).send().await.ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|n| *n > 0)
}

async fn ranged_length(url: &str) -> Option<u64> {
    let response = download_client()
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .await
        .ok()?;
    if response.status().as_u16() != 206 {
        return None;
    }
    let total = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(total_from_content_range);
    drop(response);
    total
}

fn total_from_content_range(value: &str) -> Option<u64> {
    let (_, total) = value.trim().rsplit_once('/')?;
    let total = total.trim();
    if total == "*" {
        return None;
    }
    total.parse().ok().filter(|n| *n > 0)
}

pub fn retry_delay(attempt: u32, base_ms: u64) -> Duration {
    let exp = base_ms
        .saturating_mul(1u64 << attempt.saturating_sub(1).min(10))
        .min(MAX_RETRY_DELAY_MS);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let jitter = 0.5 + f64::from(nanos % 1000) / 1000.0;
    Duration::from_millis((exp as f64 * jitter) as u64)
}

pub fn permanent_client_status(error: &str) -> Option<u16> {
    let rest = error
        .strip_prefix("HTTP Error: ")
        .or_else(|| error.strip_prefix("HTTP "))?;
    let code: u16 = rest.split_whitespace().next()?.parse().ok()?;
    ((400..500).contains(&code) && !matches!(code, 408 | 425 | 429)).then_some(code)
}

pub async fn with_retry<T, F, Fut>(
    mut operation: F,
    max_retries: u32,
    base_delay_ms: u64,
    label: &str,
) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let mut attempt = 1u32;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(e) => {
                if attempt >= max_retries {
                    return Err(e);
                }
                let delay = retry_delay(attempt, base_delay_ms);
                log::warn!(
                    "{label} failed (attempt {attempt}/{max_retries}): {e} — retrying in {}ms",
                    delay.as_millis()
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

async fn resolves_google() -> bool {
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, tokio::net::lookup_host("google.com:443")).await,
        Ok(Ok(_))
    )
}

fn host_and_port(url: &str) -> Option<(String, u16)> {
    let parsed = url::Url::parse(url).ok()?;
    let host = match parsed.host()? {
        url::Host::Domain(domain) => domain.to_string(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => ip.to_string(),
    };
    Some((host, parsed.port_or_known_default()?))
}

async fn connects(host: &str, port: u16) -> bool {
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect((host, port))).await,
        Ok(Ok(_))
    )
}

pub async fn reaches_home() -> bool {
    match host_and_port(&super::api_config::api_origin()) {
        Some((host, port)) => connects(&host, port).await,
        None => false,
    }
}

pub async fn is_online() -> bool {
    let (reaches_home, resolves) = tokio::join!(reaches_home(), resolves_google());
    reaches_home || resolves
}

pub async fn can_reach(url: &str) -> bool {
    match host_and_port(url) {
        Some((host, port)) => connects(&host, port).await,
        None => is_online().await,
    }
}

const ONLINE_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);
const OFFLINE_CHECK_INTERVAL: Duration = Duration::from_secs(15);
const RESUME_SETTLE: Duration = Duration::from_secs(5);

static SETTLE_BEFORE_RECHECK: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn monitor_wake() -> &'static tokio::sync::Notify {
    static WAKE: OnceLock<tokio::sync::Notify> = OnceLock::new();
    WAKE.get_or_init(tokio::sync::Notify::new)
}

pub fn note_reachable() {
    if !is_online_cached() {
        monitor_wake().notify_one();
    }
}

pub fn note_unreachable() {
    if is_online_cached() {
        monitor_wake().notify_one();
    }
}

pub struct OutageGate {
    held: tokio::sync::Mutex<()>,
    epoch: std::sync::atomic::AtomicU64,
}

impl Default for OutageGate {
    fn default() -> Self {
        Self::new()
    }
}

impl OutageGate {
    pub const fn new() -> Self {
        Self {
            held: tokio::sync::Mutex::const_new(()),
            epoch: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub async fn single_flight<T, F, Fut>(&self, wait: F) -> Option<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let seen = self.epoch.load(std::sync::atomic::Ordering::SeqCst);
        let _held = self.held.lock().await;
        if self.epoch.load(std::sync::atomic::Ordering::SeqCst) != seen {
            return None;
        }
        let out = wait().await;
        self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(out)
    }
}

pub fn recheck_after_resume() {
    SETTLE_BEFORE_RECHECK.store(true, std::sync::atomic::Ordering::SeqCst);
    monitor_wake().notify_one();
}

fn check_interval(is_online: bool) -> Duration {
    if is_online {
        ONLINE_CHECK_INTERVAL
    } else {
        OFFLINE_CHECK_INTERVAL
    }
}

fn online_state() -> &'static std::sync::atomic::AtomicBool {
    static STATE: OnceLock<std::sync::atomic::AtomicBool> = OnceLock::new();
    STATE.get_or_init(|| std::sync::atomic::AtomicBool::new(true))
}

pub fn is_online_cached() -> bool {
    online_state().load(std::sync::atomic::Ordering::SeqCst)
}

fn apply_status(app: &tauri::AppHandle, is_online: bool, first: bool) {
    use tauri::Emitter;

    let was_online = online_state().swap(is_online, std::sync::atomic::Ordering::SeqCst);
    if !first && was_online == is_online {
        return;
    }
    if !first {
        log::info!(
            "Network status changed: {}",
            if is_online { "ONLINE" } else { "OFFLINE" }
        );
        if is_online {
            on_reconnect(app);
        }
    }
    let _ = app.emit(
        "network-status-changed",
        serde_json::json!({ "isOnline": is_online }),
    );
}

fn on_reconnect(app: &tauri::AppHandle) {
    use tauri::Manager;

    if let Some(state) = app.try_state::<super::state::BackendState>() {
        state.game_updater.resume_skipped_startup_sweep();
    }
}

pub(super) async fn network_recheck(app: &tauri::AppHandle) -> Result<Value, String> {
    let online = is_online().await;
    apply_status(app, online, false);
    Ok(super::ok_with(serde_json::json!({ "isOnline": online })))
}

pub fn start_monitoring(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        log::info!("Connectivity monitoring started (every 5 minutes online, every 15 seconds offline)");
        let mut first = true;
        loop {
            let online = is_online().await;
            apply_status(&app, online, first);
            first = false;
            tokio::select! {
                _ = tokio::time::sleep(check_interval(online)) => {}
                _ = monitor_wake().notified() => {}
            }
            let resumed = SETTLE_BEFORE_RECHECK.swap(false, std::sync::atomic::Ordering::SeqCst);
            if resumed {
                tokio::time::sleep(RESUME_SETTLE).await;
            }
        }
    });
}

pub struct TtlCache<V> {
    entries: parking_lot::Mutex<Vec<(String, V, std::time::Instant)>>,
    ttl: Duration,
    max: usize,
}

impl<V: Clone> TtlCache<V> {
    pub const fn new(ttl: Duration, max: usize) -> Self {
        Self {
            entries: parking_lot::Mutex::new(Vec::new()),
            ttl,
            max,
        }
    }

    pub fn get(&self, key: &str) -> Option<V> {
        self.get_aged(key).and_then(|(v, fresh)| fresh.then_some(v))
    }

    pub fn get_aged(&self, key: &str) -> Option<(V, bool)> {
        let entries = self.entries.lock();
        let (_, value, at) = entries.iter().find(|(k, _, _)| k == key)?;
        Some((value.clone(), at.elapsed() < self.ttl))
    }

    pub fn remove(&self, key: &str) {
        self.entries.lock().retain(|(k, _, _)| k != key);
    }

    pub fn set(&self, key: &str, value: V) {
        let mut entries = self.entries.lock();
        let now = std::time::Instant::now();
        if let Some(slot) = entries.iter_mut().find(|(k, _, _)| k == key) {
            slot.1 = value;
            slot.2 = now;
            return;
        }
        if entries.len() >= self.max {
            entries.remove(0);
        }
        entries.push((key.to_string(), value, now));
    }
}

#[derive(Debug, Clone)]
pub enum Conditional {
    Unchanged,
    Fresh { text: String, etag: Option<String> },
}

fn strong_etag(etag: &str) -> &str {
    let etag = etag.trim();
    etag.strip_prefix("W/").unwrap_or(etag)
}

pub async fn get_text_conditional(url: &str, etag: Option<&str>) -> Result<Conditional, String> {
    let mut request = client().get(url);
    if let Some(etag) = etag.map(strong_etag).filter(|e| !e.is_empty()) {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("Request error: {}", describe(&e)))?;

    let status = response.status().as_u16();
    note_reachable();
    if status == 304 {
        return Ok(Conditional::Unchanged);
    }
    if status != 200 {
        return Err(format!("HTTP {status} for {url}"));
    }

    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(strong_etag)
        .filter(|e| !e.is_empty())
        .map(str::to_string);

    let text = read_text_capped(response, url, MAX_FEED_RESPONSE).await?;

    Ok(Conditional::Fresh { text, etag })
}

pub fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_base() -> String {
        std::env::var("PEEBIFY_TEST_SERVER").unwrap_or_else(|_| "http://127.0.0.1:3010".to_string())
    }

    #[test]
    fn a_content_range_total_is_read_after_the_slash() {
        assert_eq!(total_from_content_range("bytes 0-0/373564786"), Some(373_564_786));
        assert_eq!(total_from_content_range(" bytes 0-0/42 "), Some(42));
        assert_eq!(total_from_content_range("bytes 0-0/*"), None);
        assert_eq!(total_from_content_range("bytes 0-0/0"), None);
        assert_eq!(total_from_content_range("bytes 0-0"), None);
        assert_eq!(total_from_content_range("bytes 0-0/abc"), None);
    }

    #[test]
    fn only_permanent_client_errors_stop_the_retries() {
        for (error, code) in [
            ("HTTP Error: 404 for URL https://cdn.example.com/a.pak", 404),
            ("HTTP Error: 403 for URL https://cdn.example.com/a.pak", 403),
            ("HTTP Error: 410 for URL https://cdn.example.com/a.pak", 410),
            ("HTTP 404 for https://cdn.example.com/a.pak", 404),
        ] {
            assert_eq!(permanent_client_status(error), Some(code), "{error}");
        }
        for error in [
            "HTTP Error: 408 for URL https://cdn.example.com/a.pak",
            "HTTP Error: 425 for URL https://cdn.example.com/a.pak",
            "HTTP Error: 429 for URL https://cdn.example.com/a.pak",
            "HTTP Error: 500 for URL https://cdn.example.com/a.pak",
            "HTTP Error: 503 for URL https://cdn.example.com/a.pak",
            "HTTP 206 for https://cdn.example.com/a.pak",
            "Request error: operation timed out",
            "Stream error: connection reset",
            "Write error: HTTP 404",
            "HTTP Error: abc",
        ] {
            assert_eq!(permanent_client_status(error), None, "{error}");
        }
    }

    #[test]
    fn a_weak_etag_is_sent_as_its_strong_form() {
        assert_eq!(strong_etag("W/\"585f8b\""), "\"585f8b\"");
        assert_eq!(strong_etag(" \"585f8b\" "), "\"585f8b\"");
        assert_eq!(strong_etag("W/"), "");
    }

    #[test]
    fn causes_are_appended_once_and_never_read_as_a_cancel() {
        let causes = vec![
            "client error (Connect)".to_string(),
            "dns error".to_string(),
            "dns error".to_string(),
        ];
        assert_eq!(
            join_causes("error sending request".to_string(), &causes),
            "error sending request: client error (Connect): dns error"
        );
        let canceled = std::io::Error::other("operation was canceled");
        let text = join_causes("error sending request".to_string(), &[cause_text(&canceled)]);
        assert_eq!(
            super::super::fs_util::classify(&format!("Request error: {text}")),
            super::super::fs_util::FailureKind::Network
        );
    }

    #[test]
    fn an_offline_monitor_rechecks_quickly() {
        assert_eq!(check_interval(true), ONLINE_CHECK_INTERVAL);
        assert_eq!(check_interval(false), OFFLINE_CHECK_INTERVAL);
        assert!(OFFLINE_CHECK_INTERVAL < Duration::from_secs(60));
    }

    #[tokio::test]
    async fn only_the_first_worker_waits_out_an_outage() {
        let gate = std::sync::Arc::new(OutageGate::new());
        let waits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();

        let first = {
            let gate = std::sync::Arc::clone(&gate);
            let waits = std::sync::Arc::clone(&waits);
            tokio::spawn(async move {
                gate.single_flight(|| async {
                    waits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _ = started_tx.send(());
                    let _ = release_rx.await;
                })
                .await
            })
        };
        started_rx.await.unwrap();
        let second = {
            let gate = std::sync::Arc::clone(&gate);
            let waits = std::sync::Arc::clone(&waits);
            tokio::spawn(async move {
                gate.single_flight(|| async {
                    waits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })
                .await
            })
        };
        for _ in 0..3 {
            tokio::task::yield_now().await;
        }
        release_tx.send(()).unwrap();

        assert_eq!(first.await.unwrap(), Some(()));
        assert_eq!(second.await.unwrap(), None);
        assert_eq!(waits.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(gate.single_flight(|| async { 7 }).await, Some(7));
    }

    #[test]
    fn a_probe_target_comes_from_the_url_host() {
        assert_eq!(
            host_and_port("https://autopatch.example.com/a/b.pak"),
            Some(("autopatch.example.com".to_string(), 443))
        );
        assert_eq!(
            host_and_port("http://127.0.0.1:3010/x"),
            Some(("127.0.0.1".to_string(), 3010))
        );
        assert_eq!(host_and_port("http://[::1]/x"), Some(("::1".to_string(), 80)));
        assert_eq!(host_and_port("not a url"), None);
    }

    #[tokio::test]
    async fn a_refused_connection_names_its_cause() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = client()
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .expect_err("nothing listens on a port that was just released");
        let full = describe(&error);
        assert!(full.starts_with(&error.to_string()), "{full}");
        assert!(full.len() > error.to_string().len(), "{full}");
        assert_eq!(
            super::super::fs_util::classify(&format!("Request error: {full}")),
            super::super::fs_util::FailureKind::Network
        );
        assert!(!can_reach(&format!("http://127.0.0.1:{port}/")).await);
    }

    fn serve_once(response: Vec<u8>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
            }
            let _ = stream.write_all(&response);
        });
        format!("http://127.0.0.1:{port}/")
    }

    async fn fetch_capped(response: &[u8], limit: u64) -> Result<String, String> {
        let url = serve_once(response.to_vec());
        let response = client().get(&url).send().await.map_err(|e| describe(&e))?;
        read_text_capped(response, &url, limit).await
    }

    #[tokio::test]
    async fn capped_reads_refuse_bodies_over_the_limit() {
        let declared = b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\n\
            0123456789012345678901234567890123456789012345678901234567890123";
        let error = fetch_capped(declared, 16).await.unwrap_err();
        assert!(error.contains("declares 64 bytes"), "{error}");

        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
            20\r\n01234567890123456789012345678901\r\n0\r\n\r\n";
        let error = fetch_capped(chunked, 16).await.unwrap_err();
        assert!(error.contains("exceeded the 16-byte limit"), "{error}");

        let fits = b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n\xEF\xBB\xBF{\"a\"}";
        let text = fetch_capped(fits, 16).await.unwrap();
        assert_eq!(text, "{\"a\"}");
    }

    #[tokio::test]
    #[ignore = "needs a Peebify server on PEEBIFY_TEST_SERVER"]
    async fn a_second_fetch_of_an_unchanged_feed_costs_nothing() {
        let base = server_base();

        for path in [
            "/launcher/api.json",
            "/launcher/wallpaper/zenless-zone-zero/wallpapers-slogan.json",
        ] {
            let url = format!("{base}{path}");

            let first = get_text_conditional(&url, None)
                .await
                .unwrap_or_else(|e| panic!("{path}: {e}"));
            let Conditional::Fresh { text, etag } = first else {
                panic!("{path}: a first fetch with no etag must return a body");
            };
            let etag = etag.unwrap_or_else(|| panic!("{path}: the server sent no ETag"));
            assert!(!text.is_empty());

            let second = get_text_conditional(&url, Some(&etag)).await.unwrap();
            assert!(
                matches!(second, Conditional::Unchanged),
                "{path}: an unchanged feed must answer 304, not resend the body"
            );

            let changed = get_text_conditional(&url, Some("\"not-the-real-etag\""))
                .await
                .unwrap();
            assert!(
                matches!(changed, Conditional::Fresh { .. }),
                "{path}: a stale etag must still deliver the body"
            );
        }
    }
}
