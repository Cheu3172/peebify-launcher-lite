// ------------ Progress Tracking ------------
// Counts bytes and files for a running download or verify and works out the percentage, smoothed speed and time left.
// Also holds the pause and cancel controls that the download workers share.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};

const MIN_UI_UPDATE_INTERVAL_MS: u64 = 200;
const SPEED_SMOOTHING_FACTOR: f64 = 0.7;

struct TrackerState {
    total_bytes: f64,

    total_files: usize,
    file_sizes: HashMap<String, f64>,

    average_speed: f64,
    last_update: Instant,
    last_bytes: f64,

    phase: String,

    repaired_files: usize,
}

impl TrackerState {
    fn new() -> Self {
        Self {
            total_bytes: 0.0,
            total_files: 0,
            file_sizes: HashMap::new(),
            average_speed: 0.0,
            last_update: Instant::now(),
            last_bytes: 0.0,
            phase: "idle".to_string(),
            repaired_files: 0,
        }
    }

    fn is_transfer_phase(&self) -> bool {
        self.phase == "downloading" || self.phase == "repairing"
    }
}

pub struct ProgressTracker {
    state: Mutex<TrackerState>,
    downloaded: AtomicI64,
    processed: AtomicI64,
    file_counters: RwLock<HashMap<String, Arc<AtomicI64>>>,
    ui_epoch: Instant,
    last_ui_ms: AtomicU64,
    force_ui: AtomicBool,
}

impl Default for ProgressTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressTracker {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TrackerState::new()),
            downloaded: AtomicI64::new(0),
            processed: AtomicI64::new(0),
            file_counters: RwLock::new(HashMap::new()),
            ui_epoch: Instant::now(),
            last_ui_ms: AtomicU64::new(0),
            force_ui: AtomicBool::new(true),
        }
    }

    pub fn reset(&self) {
        *self.state.lock() = TrackerState::new();
        self.downloaded.store(0, Ordering::SeqCst);
        self.processed.store(0, Ordering::SeqCst);
        self.file_counters.write().clear();
    }

    fn current_bytes(&self, state: &TrackerState) -> f64 {
        let raw = if state.is_transfer_phase() {
            self.downloaded.load(Ordering::Relaxed)
        } else {
            self.processed.load(Ordering::Relaxed)
        };
        (raw.max(0) as f64).min(state.total_bytes.max(0.0)).max(0.0)
    }

    fn counter_for(&self, file_id: &str) -> Arc<AtomicI64> {
        let existing = self.file_counters.read().get(file_id).map(Arc::clone);
        if let Some(counter) = existing {
            return counter;
        }
        let mut map = self.file_counters.write();
        Arc::clone(
            map.entry(file_id.to_string())
                .or_insert_with(|| Arc::new(AtomicI64::new(0))),
        )
    }

    pub fn begin(
        &self,
        phase: &str,
        total_bytes: f64,
        total_files: usize,
        file_sizes: Option<HashMap<String, f64>>,
    ) {
        self.reset();
        self.set_totals(total_bytes, total_files);
        if let Some(sizes) = file_sizes {
            self.set_file_sizes(sizes);
        }
        self.set_phase(phase);
    }

    pub fn set_totals(&self, total_bytes: f64, total_files: usize) {
        let mut s = self.state.lock();
        s.total_bytes = total_bytes;
        s.total_files = total_files;
    }

    pub fn set_file_sizes(&self, sizes: HashMap<String, f64>) {
        {
            let mut counters = self.file_counters.write();
            counters.clear();
            for id in sizes.keys() {
                counters.insert(id.clone(), Arc::new(AtomicI64::new(0)));
            }
        }
        self.state.lock().file_sizes = sizes;
    }

    pub fn set_phase(&self, phase: &str) {
        let mut s = self.state.lock();
        s.phase = phase.to_string();
        s.last_update = Instant::now();
        s.last_bytes = self.current_bytes(&s);
    }

    pub fn phase(&self) -> String {
        self.state.lock().phase.clone()
    }

    pub fn increment_repaired_files(&self) {
        self.state.lock().repaired_files += 1;
    }

    pub fn repaired_files(&self) -> usize {
        self.state.lock().repaired_files
    }

    pub fn total_files(&self) -> usize {
        self.state.lock().total_files
    }

    pub fn update_validation_progress(&self, bytes: f64) {
        self.processed.fetch_add(bytes as i64, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn update_download_progress(&self, bytes: f64) {
        self.downloaded.fetch_add(bytes as i64, Ordering::Relaxed);
    }

    pub fn update_file_progress(&self, file_id: &str, bytes: f64, complete: bool) {
        if complete {
            self.file_done(file_id);
        } else {
            self.add_bytes(file_id, bytes as i64);
        }
    }

    pub fn add_bytes(&self, file_id: &str, delta: i64) {
        self.downloaded.fetch_add(delta, Ordering::Relaxed);
        self.processed.fetch_add(delta, Ordering::Relaxed);
        let existing = self.file_counters.read().get(file_id).map(Arc::clone);
        match existing {
            Some(counter) => {
                counter.fetch_add(delta, Ordering::Relaxed);
            }
            None => {
                self.counter_for(file_id)
                    .fetch_add(delta, Ordering::Relaxed);
            }
        }
    }

    pub fn file_done(&self, file_id: &str) {
        let expected = self
            .state
            .lock()
            .file_sizes
            .get(file_id)
            .copied()
            .unwrap_or(0.0) as i64;
        let counter = self.counter_for(file_id);
        let counted = counter.swap(expected, Ordering::Relaxed);
        let settle = expected - counted;
        if settle != 0 {
            self.downloaded.fetch_add(settle, Ordering::Relaxed);
            self.processed.fetch_add(settle, Ordering::Relaxed);
        }
    }

    pub fn set_file_progress_absolute(&self, file_id: &str, bytes: f64) {
        let counter = self.counter_for(file_id);
        let counted = counter.swap(bytes as i64, Ordering::Relaxed);
        let settle = bytes as i64 - counted;
        if settle != 0 {
            self.downloaded.fetch_add(settle, Ordering::Relaxed);
            self.processed.fetch_add(settle, Ordering::Relaxed);
        }
    }

    pub fn reset_speed_baseline(&self) {
        let mut s = self.state.lock();
        s.last_update = Instant::now();
        s.last_bytes = self.current_bytes(&s);
    }

    pub fn force_completion(&self) {
        let mut s = self.state.lock();
        let total = s.total_bytes.max(0.0) as i64;
        self.downloaded.store(total, Ordering::SeqCst);
        self.processed.store(total, Ordering::SeqCst);
        s.last_bytes = s.total_bytes;
        log::info!("Progress forced to completion state.");
    }

    fn update_speed_metrics(&self, s: &mut TrackerState) {
        let now = Instant::now();
        let time_diff = now.duration_since(s.last_update).as_secs_f64();
        if time_diff > 0.1 {
            let current = self.current_bytes(s);
            let instant = ((current - s.last_bytes) / time_diff).max(0.0);
            s.average_speed = smoothed_speed(s.average_speed, instant);
            s.last_update = now;
            s.last_bytes = current;
        }
    }

    pub fn calculate_metrics(&self) -> Value {
        let mut s = self.state.lock();
        self.update_speed_metrics(&mut s);

        let current_bytes = self.current_bytes(&s);
        let percentage = if s.total_bytes > 0.0 {
            ((current_bytes / s.total_bytes) * 100.0).min(100.0)
        } else {
            0.0
        };
        let remaining = s.total_bytes - current_bytes;
        let eta = if s.average_speed > 0.0 {
            remaining / s.average_speed
        } else {
            0.0
        };
        json!({
            "percentage": percentage,
            "speed": s.average_speed,
            "eta": eta,
            "processedBytes": current_bytes,
            "totalBytes": s.total_bytes,
            "phase": s.phase,
        })
    }

    pub fn should_update_ui(&self) -> bool {
        let now = self.ui_epoch.elapsed().as_millis() as u64;
        if self.force_ui.swap(false, Ordering::Relaxed) {
            self.last_ui_ms.store(now, Ordering::Relaxed);
            return true;
        }
        let last = self.last_ui_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= MIN_UI_UPDATE_INTERVAL_MS {
            self.last_ui_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        } else {
            false
        }
    }

    pub fn force_next_update(&self) {
        self.force_ui.store(true, Ordering::Relaxed);
    }
}

fn smoothed_speed(previous: f64, instant: f64) -> f64 {
    let next = if previous <= 0.0 {
        instant
    } else {
        previous * SPEED_SMOOTHING_FACTOR + instant * (1.0 - SPEED_SMOOTHING_FACTOR)
    };
    next.max(0.0)
}

pub fn gib(bytes: f64) -> f64 {
    bytes / 1024.0 / 1024.0 / 1024.0
}

pub trait Control: Send + Sync {
    fn is_cancelled(&self) -> bool;
    fn gate(&self) -> Option<&RunGate> {
        None
    }
    fn is_paused(&self) -> bool {
        self.gate().is_some_and(RunGate::is_paused)
    }
    fn wait_if_paused(&self) {
        while self.is_paused() && !self.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

pub async fn wait_while_paused<C: Control + ?Sized>(control: &C) {
    if let Some(gate) = control.gate() {
        gate.wait_while_paused().await;
        return;
    }
    while control.is_paused() && !control.is_cancelled() {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
}

pub enum FileEvent<'a> {
    Bytes { path: &'a str, delta: u64 },
    FileDone { path: &'a str },
}

pub trait FileHooks: Control {
    fn event(&self, event: FileEvent);
    fn status(&self, _message: &str) {}
}

pub fn check_cancel(control: &dyn Control, message: &str) -> Result<(), String> {
    if control.is_cancelled() {
        return Err(message.to_string());
    }
    control.wait_if_paused();
    if control.is_cancelled() {
        return Err(message.to_string());
    }
    Ok(())
}

pub struct RunGate {
    paused: std::sync::atomic::AtomicBool,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pause_gate: tokio::sync::Notify,
    cancel_gate: tokio::sync::Notify,
    pause_epoch: std::sync::atomic::AtomicU64,
}

impl Default for RunGate {
    fn default() -> Self {
        Self::new()
    }
}

impl RunGate {
    pub fn new() -> Self {
        Self {
            paused: std::sync::atomic::AtomicBool::new(false),
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pause_gate: tokio::sync::Notify::new(),
            cancel_gate: tokio::sync::Notify::new(),
            pause_epoch: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn pause_epoch(&self) -> u64 {
        self.pause_epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn flag(&self) -> &std::sync::Arc<std::sync::atomic::AtomicBool> {
        &self.cancelled
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn reset(&self) {
        self.paused
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.cancelled
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn clear_paused(&self) {
        self.paused
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn pause(&self) -> bool {
        let newly = !self.paused.swap(true, std::sync::atomic::Ordering::SeqCst);
        if newly {
            self.pause_epoch
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        newly
    }

    pub fn resume(&self) -> bool {
        let was_paused = self.paused.swap(false, std::sync::atomic::Ordering::SeqCst);
        if was_paused {
            self.pause_gate.notify_waiters();
        }
        was_paused
    }

    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.cancel_gate.notify_waiters();
        self.pause_gate.notify_waiters();
    }

    pub async fn wait_while_paused(&self) {
        loop {
            let resumed = self.pause_gate.notified();
            tokio::pin!(resumed);
            resumed.as_mut().enable();
            if !self.is_paused() || self.is_cancelled() {
                return;
            }
            resumed.await;
        }
    }

    pub async fn cancellable<T, F>(&self, operation: F) -> Result<T, String>
    where
        F: std::future::Future<Output = Result<T, String>>,
    {
        tokio::pin!(operation);
        loop {
            let cancelled = self.cancel_gate.notified();
            tokio::pin!(cancelled);
            cancelled.as_mut().enable();
            if self.is_cancelled() {
                return Err("cancelled".to_string());
            }
            tokio::select! {
                outcome = &mut operation => return outcome,
                _ = cancelled => {}
            }
        }
    }
}

pub async fn join_workers<W>(workers: W, label: &str) -> Result<(), String>
where
    W: IntoIterator<Item = tauri::async_runtime::JoinHandle<Result<(), String>>>,
{
    let mut first_error: Option<String> = None;
    for worker in workers {
        match worker.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => first_error = first_error.or(Some(e)),
            Err(e) => first_error = first_error.or(Some(format!("{label} panicked: {e}"))),
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn processed(tracker: &ProgressTracker) -> f64 {
        tracker.calculate_metrics()["processedBytes"].as_f64().unwrap()
    }

    #[test]
    fn begin_downloading_reflects_download_progress() {
        let tracker = ProgressTracker::new();
        tracker.begin("downloading", 1000.0, 2, None);
        tracker.update_download_progress(400.0);
        let metrics = tracker.calculate_metrics();
        assert_eq!(metrics["processedBytes"].as_f64(), Some(400.0));
        assert_eq!(metrics["phase"].as_str(), Some("downloading"));
        assert_eq!(tracker.total_files(), 2);
        assert_eq!(metrics["percentage"].as_f64(), Some(40.0));
    }

    #[test]
    fn a_bare_reset_ignores_download_progress() {
        let tracker = ProgressTracker::new();
        tracker.reset();
        tracker.set_totals(1000.0, 1);
        tracker.update_download_progress(400.0);
        assert_eq!(processed(&tracker), 0.0);
    }

    #[test]
    fn begin_clears_the_previous_stage() {
        let tracker = ProgressTracker::new();
        tracker.begin("downloading", 1000.0, 1, None);
        tracker.update_download_progress(900.0);
        tracker.begin("validating", 500.0, 1, None);
        assert_eq!(processed(&tracker), 0.0);
        tracker.update_validation_progress(200.0);
        assert_eq!(processed(&tracker), 200.0);
    }

    #[test]
    fn file_done_settles_to_the_recorded_size() {
        let tracker = ProgressTracker::new();
        let sizes = HashMap::from([("a".to_string(), 300.0), ("b".to_string(), 700.0)]);
        tracker.begin("downloading", 1000.0, 2, Some(sizes));
        tracker.add_bytes("a", 120);
        tracker.file_done("a");
        assert_eq!(processed(&tracker), 300.0);
        tracker.add_bytes("b", 800);
        tracker.file_done("b");
        assert_eq!(processed(&tracker), 1000.0);
    }

    #[test]
    fn force_completion_reads_full() {
        let tracker = ProgressTracker::new();
        tracker.begin("downloading", 1000.0, 1, None);
        tracker.update_download_progress(10.0);
        tracker.force_completion();
        assert_eq!(
            tracker.calculate_metrics()["percentage"].as_f64(),
            Some(100.0)
        );
    }

    #[test]
    fn a_steady_rate_reads_true_from_the_first_sample() {
        assert_eq!(smoothed_speed(0.0, 100.0), 100.0);
        let mut speed = 0.0;
        for _ in 0..5 {
            speed = smoothed_speed(speed, 100.0);
        }
        assert!((speed - 100.0).abs() < 1e-9, "{speed}");
    }

    #[test]
    fn a_speed_drop_is_followed_within_a_few_seconds() {
        let mut speed = 100.0;
        for _ in 0..10 {
            speed = smoothed_speed(speed, 10.0);
        }
        assert!(speed > 10.0 && speed < 13.0, "{speed}");
    }

    #[test]
    fn the_tracker_reports_the_first_measured_rate_in_full() {
        let tracker = ProgressTracker::new();
        tracker.begin("downloading", 10_000.0, 1, None);
        tracker.state.lock().last_update = Instant::now() - std::time::Duration::from_secs(1);
        tracker.update_download_progress(1000.0);
        let speed = tracker.calculate_metrics()["speed"].as_f64().unwrap();
        assert!(speed > 800.0 && speed <= 1000.0, "{speed}");
    }

    #[test]
    fn speed_is_measured_on_the_monotonic_clock() {
        let tracker = ProgressTracker::new();
        tracker.begin("downloading", 1000.0, 1, None);
        tracker.state.lock().last_update = Instant::now() - std::time::Duration::from_secs(1);
        tracker.update_download_progress(500.0);
        let speed = tracker.calculate_metrics()["speed"].as_f64().unwrap();
        assert!(speed > 0.0);
    }
}
