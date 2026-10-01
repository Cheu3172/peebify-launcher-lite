// ------------ File Validator ------------
// Checks installed files against the expected size and hash using a few worker threads, reports progress while it goes,
// and hands back the files that are missing or wrong so they can be repaired.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::AppHandle;

use super::progress::Control;
use super::progress::ProgressTracker;
use super::queue::{Phase, Update};

const LOGGED_INVALID_FILES: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileCheck {
    Valid,
    Missing,
    SizeMismatch,
    HashMismatch,
    Unreadable,
}

impl FileCheck {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            FileCheck::Valid => "is valid",
            FileCheck::Missing => "is missing",
            FileCheck::SizeMismatch => "has the wrong size",
            FileCheck::HashMismatch => "fails the checksum",
            FileCheck::Unreadable => "could not be read",
        }
    }
}

pub struct FileValidator;

impl FileValidator {
    pub fn quick_validate(path: &Path, expected_size: u64) -> bool {
        match std::fs::metadata(path) {
            Ok(meta) => meta.len() == expected_size,
            Err(_) => false,
        }
    }

    pub fn check(
        path: &Path,
        expected_size: u64,
        expected_md5: &str,
        cancelled: Option<&AtomicBool>,
        mut on_progress: impl FnMut(u64),
    ) -> Result<FileCheck, String> {
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileCheck::Missing),
            Err(e) => {
                log::debug!("validate: could not stat {}: {e}", path.display());
                return Ok(FileCheck::Unreadable);
            }
        };
        if meta.len() != expected_size {
            return Ok(FileCheck::SizeMismatch);
        }
        if expected_md5.is_empty() {
            return Ok(FileCheck::Valid);
        }

        match super::fs_util::md5_file(
            path,
            &mut || cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst)),
            &mut |n| on_progress(n),
        ) {
            Ok(actual) if actual.eq_ignore_ascii_case(expected_md5) => Ok(FileCheck::Valid),
            Ok(_) => Ok(FileCheck::HashMismatch),
            Err(e) if e == super::fs_util::CANCELLED_MSG => {
                Err("Validation cancelled".to_string())
            }
            Err(e) => {
                log::warn!("validate: {e}");
                Ok(FileCheck::Unreadable)
            }
        }
    }
}

pub struct ValidationMeta {
    pub is_final: bool,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Resource {
    pub dest: Box<str>,
    pub size: u64,
    pub md5: Box<str>,
    pub url: Option<Box<str>>,
}

impl Resource {
    pub fn new(dest: impl Into<String>, size: u64, md5: impl Into<String>) -> Self {
        Self {
            dest: dest.into().into_boxed_str(),
            size,
            md5: md5.into().into_boxed_str(),
            url: None,
        }
    }

    pub fn with_url(mut self, url: Option<impl Into<String>>) -> Self {
        self.url = url.map(|u| u.into().into_boxed_str());
        self
    }

    pub fn dest(&self) -> &str {
        &self.dest
    }

    pub fn md5(&self) -> &str {
        &self.md5
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    pub fn from_json(value: &Value) -> Self {
        Self {
            dest: resource_dest(value).into(),
            size: resource_size(value),
            md5: value
                .get("md5")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
            url: value
                .get("fullUrl")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(Into::into),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "dest": self.dest.as_ref(),
            "size": self.size.to_string(),
            "md5": self.md5.as_ref(),
            "fullUrl": self.url.as_deref(),
        })
    }
}

fn resource_size(resource: &Value) -> u64 {
    match resource.get("size") {
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

fn resource_dest(resource: &Value) -> &str {
    resource.get("dest").and_then(|v| v.as_str()).unwrap_or("")
}

fn status_text(meta: &ValidationMeta) -> String {
    if meta.is_final {
        "Verifying integrity...".to_string()
    } else {
        meta.version.as_ref().map_or_else(
            || "Checking existing files...".to_string(),
            |v| format!("Checking files for Patch {v}..."),
        )
    }
}

fn stage(phase: &str, meta: &ValidationMeta) -> Update {
    match phase {
        "validating" if meta.is_final => Update::new(Phase::Verifying).kind("verify"),
        "validating" => Phase::Scanning.into(),
        _ => Phase::Downloading.into(),
    }
}

fn send_pipeline_progress(
    app: &AppHandle,
    tracker: &ProgressTracker,
    meta: &ValidationMeta,
    game_id: &str,
    paused: bool,
) {
    if !tracker.should_update_ui() {
        return;
    }
    let metrics = tracker.calculate_metrics();
    let phase = metrics
        .get("phase")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let status = status_text(meta);
    let mut progress = json!({
        "status": if paused { super::download_engine::status::PAUSED } else { status.as_str() },
        "gameId": if game_id.is_empty() { Value::Null } else { json!(game_id) },
    });
    if let (Some(target), Some(src)) = (progress.as_object_mut(), metrics.as_object()) {
        if !meta.is_final {
            target.insert("speed".to_string(), json!(0));
            target.insert("eta".to_string(), json!(0));
        }
        for (k, v) in src {
            target.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    super::queue::publish(
        app,
        "download-progress",
        stage(&phase, meta).paused(paused),
        progress,
    );
}

fn run_label(game_id: &str, version: Option<&str>) -> String {
    let parts: Vec<&str> = [Some(game_id), version]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

fn summarize(
    flagged: &[(Resource, FileCheck)],
    total: usize,
    meta: &ValidationMeta,
    game_id: &str,
    elapsed_secs: f64,
) {
    let count = |kind: FileCheck| flagged.iter().filter(|(_, check)| *check == kind).count();
    log::info!(
        "Validation{} finished in {elapsed_secs:.1}s: {} of {total} files invalid ({} missing, {} wrong size, {} bad checksum, {} unreadable).",
        run_label(game_id, meta.version.as_deref()),
        flagged.len(),
        count(FileCheck::Missing),
        count(FileCheck::SizeMismatch),
        count(FileCheck::HashMismatch),
        count(FileCheck::Unreadable),
    );
    if meta.is_final {
        for (resource, check) in flagged.iter().take(LOGGED_INVALID_FILES) {
            log::warn!("Invalid file detected: {} {}", resource.dest(), check.describe());
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn validate_resources(
    app: &AppHandle,
    tracker: &Arc<ProgressTracker>,
    resources: Arc<Vec<Resource>>,
    install_path: &Path,
    cancelled: &Arc<AtomicBool>,
    control: Option<Arc<dyn Control>>,
    meta: &ValidationMeta,
    game_id: &str,
) -> Result<Vec<Resource>, String> {
    let total_size: u64 = resources.iter().map(|r| r.size).sum();
    tracker.begin("validating", total_size as f64, resources.len(), None);

    log::info!("Starting validation of {} files...", resources.len());

    let started = Instant::now();
    let queue = Arc::new(AtomicUsize::new(0));
    let invalid: Arc<Mutex<Vec<(Resource, FileCheck)>>> = Arc::new(Mutex::new(Vec::new()));
    let resources_arc = Arc::clone(&resources);
    let install_path = install_path.to_path_buf();

    let meta_shared = Arc::new(ValidationMeta {
        is_final: meta.is_final,
        version: meta.version.clone(),
    });

    let workers = super::perf::validation_workers().min(resources.len().max(1));
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let app = app.clone();
        let tracker = Arc::clone(tracker);
        let queue = Arc::clone(&queue);
        let invalid = Arc::clone(&invalid);
        let resources = Arc::clone(&resources_arc);
        let cancelled = Arc::clone(cancelled);
        let install_path: PathBuf = install_path.clone();
        let meta = Arc::clone(&meta_shared);
        let game_id = game_id.to_string();
        let control = control.clone();

        handles.push(tauri::async_runtime::spawn_blocking(
            move || -> Result<(), String> {
                let paused = || control.as_ref().is_some_and(|c| c.is_paused());
                let hold = || {
                    if let Some(control) = &control {
                        control.wait_if_paused();
                    }
                };
                loop {
                    let index = queue.fetch_add(1, Ordering::SeqCst);
                    let Some(resource) = resources.get(index) else {
                        return Ok(());
                    };
                    hold();
                    if cancelled.load(Ordering::SeqCst) {
                        return Err("Validation cancelled".to_string());
                    }

                    let dest = resource.dest();
                    let expected_size = resource.size;
                    let expected_md5 = resource.md5().to_string();
                    let file_path = match crate::backend::fs_util::safe_join(&install_path, dest) {
                        Ok(p) => p,
                        Err(e) => {
                            log::warn!("validate: skipping resource — {e}");
                            continue;
                        }
                    };

                    let mut bytes_hashed: u64 = 0;
                    let check = FileValidator::check(
                        &file_path,
                        expected_size,
                        &expected_md5,
                        Some(&cancelled),
                        |chunk| {
                            bytes_hashed += chunk;
                            tracker.update_validation_progress(chunk as f64);
                            send_pipeline_progress(&app, &tracker, &meta, &game_id, paused());
                            hold();
                        },
                    )?;

                    let remaining = expected_size.saturating_sub(bytes_hashed);
                    if remaining > 0 {
                        tracker.update_validation_progress(remaining as f64);
                    }

                    if check != FileCheck::Valid {
                        log::debug!("validate: {dest} {}", check.describe());
                        invalid.lock().push((resource.clone(), check));
                    }
                    send_pipeline_progress(&app, &tracker, &meta, &game_id, paused());
                }
            },
        ));
    }

    super::progress::join_workers(handles, "validation worker").await?;

    let flagged = std::mem::take(&mut *invalid.lock());
    summarize(
        &flagged,
        resources.len(),
        meta,
        game_id,
        started.elapsed().as_secs_f64(),
    );
    Ok(flagged.into_iter().map(|(resource, _)| resource).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "peebify-validator-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            Self(base)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const HELLO_MD5: &str = "5d41402abc4b2a76b9719d911017c592";

    fn check(path: &Path, size: u64, md5: &str) -> FileCheck {
        FileValidator::check(path, size, md5, None, |_| {}).unwrap()
    }

    #[test]
    fn each_failure_reports_its_own_reason() {
        let dir = TempDir::new("reasons");
        let file = dir.0.join("hello.bin");
        std::fs::write(&file, b"hello").unwrap();

        assert_eq!(check(&dir.0.join("absent.bin"), 5, HELLO_MD5), FileCheck::Missing);
        assert_eq!(check(&file, 6, HELLO_MD5), FileCheck::SizeMismatch);
        assert_eq!(
            check(&file, 5, "00000000000000000000000000000000"),
            FileCheck::HashMismatch
        );
        assert_eq!(check(&file, 5, &HELLO_MD5.to_uppercase()), FileCheck::Valid);
    }

    #[test]
    fn a_resource_without_a_checksum_is_checked_by_size() {
        let dir = TempDir::new("no-md5");
        let file = dir.0.join("client.7z");
        std::fs::write(&file, b"hello").unwrap();

        assert_eq!(check(&file, 5, ""), FileCheck::Valid);
        assert_eq!(check(&file, 6, ""), FileCheck::SizeMismatch);
        assert_eq!(check(&dir.0.join("absent.7z"), 5, ""), FileCheck::Missing);
    }

    #[test]
    fn a_cancelled_hash_is_an_error_not_a_verdict() {
        let dir = TempDir::new("cancel");
        let file = dir.0.join("hello.bin");
        std::fs::write(&file, b"hello").unwrap();
        let cancelled = AtomicBool::new(true);

        let result = FileValidator::check(&file, 5, HELLO_MD5, Some(&cancelled), |_| {});
        assert_eq!(result, Err("Validation cancelled".to_string()));
    }

    #[test]
    fn a_pre_download_check_is_labelled_as_a_check() {
        let pre = ValidationMeta {
            is_final: false,
            version: Some("3.6.1".to_string()),
        };
        let bare = ValidationMeta {
            is_final: false,
            version: None,
        };
        let fin = ValidationMeta {
            is_final: true,
            version: None,
        };
        assert_eq!(status_text(&pre), "Checking files for Patch 3.6.1...");
        assert_eq!(status_text(&bare), "Checking existing files...");
        assert_eq!(status_text(&fin), "Verifying integrity...");
        assert_eq!(stage("validating", &pre).phase(), Phase::Scanning);
        assert_eq!(stage("validating", &bare).phase(), Phase::Scanning);
        assert_eq!(stage("validating", &fin).phase(), Phase::Verifying);
        assert_eq!(stage("downloading", &fin).phase(), Phase::Downloading);
    }

    #[test]
    fn the_run_label_skips_empty_parts() {
        assert_eq!(run_label("wuwa", Some("3.6.1")), " (wuwa, 3.6.1)");
        assert_eq!(run_label("wuwa", None), " (wuwa)");
        assert_eq!(run_label("", Some("")), "");
    }
}
