// ------------ Game Overlay ------------
// Runs the in-game overlay. It starts the separate overlay helper exe next to the game, registers the hotkeys,
// drives screenshots and recordings, and keeps the capture folder and audio track settings.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use peebify_helpers::overlay::section::{SectionView, Signal};
use peebify_helpers::overlay::shared::{
    HELPER_PROTOCOL, HOTKEY_COUNT, HOTKEY_RECORD, HOTKEY_SHOT, HOTKEY_TOGGLE, SIGNAL_TO_HELPER,
    SIGNAL_TO_LAUNCHER, STATUS_RUNNING,
};
use tokio::process::Child;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::state::BackendState;
use super::{arg_str, err_response, ok_with};

const HELPER_NAME: &str = "peebify-overlay-helper.exe";
const STATUS_INTERVAL: Duration = Duration::from_millis(500);
const STALE_TICKS: u32 = 6;
const HELPER_EXIT_WAIT: Duration = Duration::from_secs(5);
const HOTKEYS_REGISTERED_EVENT: &str = "overlay-hotkeys-registered";
const WINDOW_WAIT_TICKS: u32 = 120;
const RETIRED_WINDOW_WAIT_TICKS: u32 = 40;

pub const AUDIO_TRACKS: &[(&str, &str, bool)] = &[
    ("desktop", "Desktop", false),
    ("mic", "Microphone", false),
    ("game", "Game only", true),
    ("discord", "Discord only", true),
];

pub const HOTKEYS: &[(&str, &str, &str)] = &[
    ("overlayHotkey", "Open the overlay", "Alt+P"),
    ("overlayShotHotkey", "Screenshot", "Alt+S"),
    ("overlayRecHotkey", "Start or stop recording", "Alt+R"),
];

const _: () = assert!(HOTKEYS.len() <= HOTKEY_COUNT);

const BARE_KEY_WARNING: &str = "A key on its own does not reach the overlay while a game that runs as administrator is in front, and every supported game does. Combine it with Alt, Ctrl or Shift to be safe.";

const DEFAULT_AUDIO_TRACKS: &str = "desktop,mic";

pub fn supports_per_app_audio() -> bool {
    let build: u32 = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .ok()
        .and_then(|key| key.get_value::<String, _>("CurrentBuildNumber").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    build >= 20348
}

pub fn default_capture_dir(app: &AppHandle) -> PathBuf {
    use tauri::Manager;
    app.path()
        .video_dir()
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Videos")
        })
        .join("Peebify")
}

pub fn capture_dir(app: &AppHandle) -> PathBuf {
    capture_dir_in(app, &app.state::<BackendState>().config)
}

pub(super) fn capture_dir_in(app: &AppHandle, config: &super::config::LauncherConfig) -> PathBuf {
    let configured = config.get("behavior.overlayCaptureFolder");
    match configured.as_str().filter(|s| !s.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => default_capture_dir(app),
    }
}

fn csv_setting(app: &AppHandle, key: &str, fallback: &str) -> Vec<String> {
    let raw = app.state::<BackendState>().config.get(key);
    let text = raw.as_str().unwrap_or(fallback);
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub(super) async fn get_overlay_config(app: &AppHandle) -> Result<Value, String> {
    let config = &app.state::<BackendState>().config;

    let hotkeys: Vec<Value> = HOTKEYS
        .iter()
        .map(|(key, label, default)| {
            let value = config.get(&format!("behavior.{key}"));
            json!({
                "id": key,
                "label": label,
                "accelerator": value.as_str().filter(|s| !s.is_empty()).unwrap_or(default),
                "default": default,
            })
        })
        .collect();

    let per_app_audio = supports_per_app_audio();
    let tracks: Vec<Value> = AUDIO_TRACKS
        .iter()
        .map(|(id, label, needs_win11)| {
            json!({
                "id": id,
                "label": label,
                "available": !needs_win11 || per_app_audio,
            })
        })
        .collect();

    let folder = capture_dir(app);
    Ok(ok_with(json!({
        "hotkeys": hotkeys,
        "audioTracks": tracks,
        "selectedAudioTracks": csv_setting(app, "behavior.overlayAudioTracks", DEFAULT_AUDIO_TRACKS),
        "perAppAudio": per_app_audio,
        "captureFolder": folder.to_string_lossy(),
        "captureFolderIsDefault": folder == default_capture_dir(app),
        "launchAction": config.get("behavior.launchAction"),
        "recEstimate": rec_estimate(),
    })))
}

fn rec_estimate() -> Value {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    let (width, height) = super::overlay_window::game_hwnd()
        .and_then(super::overlay_window::client_rect)
        .map(|rect| (rect.width, rect.height))
        .unwrap_or_else(|| unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) });
    let mut model = super::recorder::bitrate_model();
    model["sourceWidth"] = json!(width.max(0));
    model["sourceHeight"] = json!(height.max(0));
    model
}

pub(super) async fn set_overlay_hotkey(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let Some(id) = arg_str(args, 0) else {
        return Ok(err_response("No hotkey was specified."));
    };
    let Some((key, _, default)) = HOTKEYS.iter().find(|(k, _, _)| *k == id) else {
        return Ok(err_response("That isn't an overlay hotkey."));
    };

    let accelerator = arg_str(args, 1).unwrap_or(default).trim().to_string();
    let Some(parsed) = parse_accelerator(&accelerator) else {
        return Ok(err_response(
            "That shortcut won't work. Use a function key, or a modifier plus another key.",
        ));
    };
    let warning = (!accelerator.contains('+')).then_some(BARE_KEY_WARNING);

    let clash = HOTKEYS.iter().find_map(|(other, label, other_default)| {
        if other == key {
            return None;
        }
        let current = app
            .state::<BackendState>()
            .config
            .get(&format!("behavior.{other}"));
        let current = current
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(other_default)
            .to_string();
        (parse_accelerator(&current) == Some(parsed)).then_some(label)
    });
    if let Some(label) = clash {
        return Ok(err_response(format!(
            "\"{label}\" already uses that shortcut."
        )));
    }

    app.state::<BackendState>().config.set(
        &format!("behavior.{key}"),
        Value::String(accelerator.clone()),
    );
    log::info!("overlay: {key} bound to {accelerator}");
    app.state::<BackendState>().overlay.publish_hotkeys();
    let _ = app.emit("settings-changed", json!({ "key": key, "value": accelerator }));
    Ok(ok_with(
        json!({ "id": key, "accelerator": accelerator, "warning": warning }),
    ))
}

pub(super) async fn set_overlay_capture_folder(
    app: &AppHandle,
    args: &[Value],
) -> Result<Value, String> {
    let state = app.state::<BackendState>();

    if args.first() == Some(&Value::Null) || args.first().and_then(Value::as_str) == Some("") {
        let default_dir = default_capture_dir(app);
        super::fs_util::allow_asset_dir(app, &default_dir);
        state.config.set(
            "behavior.overlayCaptureFolder",
            Value::String(String::new()),
        );
        let _ = app.emit("settings-changed", json!({ "key": "overlayCaptureFolder" }));
        return Ok(ok_with(json!({
            "path": default_dir.to_string_lossy(),
            "isDefault": true,
        })));
    }

    let owner = if super::overlay_window::drawer_is_open() {
        super::overlay_window::DRAWER_LABEL
    } else {
        "main"
    };
    let picked = crate::backend::fs_util::dialog::show_open(
        app,
        json!({ "title": "Choose where captures are saved", "directory": true, "owner": owner }),
    )
    .await?;
    if picked["canceled"] == Value::Bool(true) {
        return Ok(json!({ "success": false, "cancelled": true }));
    }
    let Some(path) = picked["filePaths"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_str)
    else {
        return Ok(err_response("No folder was selected."));
    };

    super::fs_util::allow_asset_dir(app, std::path::Path::new(path));
    state.config.set(
        "behavior.overlayCaptureFolder",
        Value::String(path.to_string()),
    );
    log::info!("overlay: captures will be saved to {path}");
    let _ = app.emit("settings-changed", json!({ "key": "overlayCaptureFolder" }));
    Ok(ok_with(json!({ "path": path, "isDefault": false })))
}

pub(super) async fn open_launcher(app: &AppHandle) -> Result<Value, String> {
    let state = app.state::<BackendState>();
    state.overlay.close_drawer(false);
    state.window.show_window();
    Ok(super::ok_response())
}

pub(super) async fn open_capture_folder(app: &AppHandle) -> Result<Value, String> {
    let dir = capture_dir(app);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Ok(err_response(format!("Could not open that folder: {e}")));
    }
    crate::backend::fs_util::dialog::open_path(json!({ "path": dir.to_string_lossy() })).await?;
    Ok(super::ok_response())
}

#[derive(Debug, Default, PartialEq)]
struct HotkeyReport {
    registered: Vec<String>,
    unregistered: Vec<String>,
    invalid: Vec<String>,
}

impl HotkeyReport {
    fn describe(&self) -> String {
        let list = |keys: &[String]| {
            if keys.is_empty() {
                "none".to_string()
            } else {
                keys.join(", ")
            }
        };
        let mut text = format!(
            "hotkeys registered: {}; not registered: {}",
            list(&self.registered),
            list(&self.unregistered)
        );
        if !self.invalid.is_empty() {
            text.push_str(&format!("; invalid: {}", list(&self.invalid)));
        }
        text
    }
}

fn hotkey_report(stored: impl Fn(&str) -> Value, bits: u32) -> HotkeyReport {
    let mut report = HotkeyReport::default();
    for (index, (key, _, default)) in HOTKEYS.iter().enumerate() {
        let value = stored(&format!("behavior.{key}"));
        let text = value
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(default)
            .to_string();
        if parse_accelerator(&text).is_none() {
            report.invalid.push(text);
        } else if bits & (1 << index) != 0 {
            report.registered.push(text);
        } else {
            report.unregistered.push(text);
        }
    }
    report
}

async fn retire_helper(mut child: Child) {
    let deadline = Instant::now() + HELPER_EXIT_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                log::info!("overlay: the previous helper has exited ({status})");
                return;
            }
            Ok(None) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok(None) => {
                log::warn!(
                    "overlay: the previous helper did not exit within {}s, so it is being ended",
                    HELPER_EXIT_WAIT.as_secs()
                );
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                return;
            }
            Err(e) => {
                log::warn!("overlay: could not check on the previous helper ({e})");
                return;
            }
        }
    }
}

fn process_started_ms(pid: u32) -> Option<i64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const UNIX_EPOCH_MS: i64 = 11_644_473_600_000;

    if pid == 0 {
        return None;
    }
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    let timed = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let timed = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0;
        CloseHandle(handle);
        timed
    };
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    (timed && ticks != 0).then(|| (ticks / 10_000) as i64 - UNIX_EPOCH_MS)
}

fn parse_accelerator(text: &str) -> Option<(u32, u32)> {
    const MOD_ALT: u32 = 0x0001;
    const MOD_CONTROL: u32 = 0x0002;
    const MOD_SHIFT: u32 = 0x0004;
    const MOD_WIN: u32 = 0x0008;

    let mut modifiers = 0u32;
    let mut key: Option<u32> = None;

    for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= MOD_CONTROL,
            "alt" => modifiers |= MOD_ALT,
            "shift" => modifiers |= MOD_SHIFT,
            "win" | "meta" | "super" => modifiers |= MOD_WIN,
            other => {
                if key.is_some() {
                    return None;
                }
                key = virtual_key(other);
            }
        }
    }

    let vk = key?;
    let bare_ok = (0x70..=0x87).contains(&vk);
    (modifiers != 0 || bare_ok).then_some((modifiers, vk))
}

fn virtual_key(lowercase: &str) -> Option<u32> {
    if let Some(number) = lowercase.strip_prefix('f') {
        if let Ok(n) = number.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x6F + n);
            }
        }
    }
    let mut chars = lowercase.chars();
    let (first, rest) = (chars.next()?, chars.next());
    if rest.is_some() {
        return match lowercase {
            "space" => Some(0x20),
            "tab" => Some(0x09),
            "insert" => Some(0x2D),
            "delete" => Some(0x2E),
            "home" => Some(0x24),
            "end" => Some(0x23),
            "pageup" => Some(0x21),
            "pagedown" => Some(0x22),
            "left" | "arrowleft" => Some(0x25),
            "up" | "arrowup" => Some(0x26),
            "right" | "arrowright" => Some(0x27),
            "down" | "arrowdown" => Some(0x28),
            "enter" | "return" => Some(0x0D),
            "backspace" => Some(0x08),
            _ => None,
        };
    }
    match first {
        'a'..='z' => Some(first.to_ascii_uppercase() as u32),
        '0'..='9' => Some(first as u32),
        _ => None,
    }
}

// ------------ Overlay Manager ------------
// Owns the helper process for the current game: spawning it, watching its status, restarting it if it dies,
// and handling hotkey presses and the drawer.
enum HelperCheck {
    NotReady,
    Mismatch { theirs: u32 },
    Live,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HelperState {
    Idle,
    Starting,
    Running,
    Mismatched,
    Failed,
}

impl HelperState {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Mismatched => "mismatched",
            Self::Failed => "failed",
        }
    }
}

struct Link {
    section: SectionView,
    to_helper: Option<Signal>,
}

pub struct OverlayManager {
    app: AppHandle,
    link: Mutex<Option<Link>>,
    state: Mutex<HelperState>,
    attached_game: Mutex<Option<String>>,
    owner_game: Mutex<Option<String>>,
    generation: AtomicU64,
    helper: tokio::sync::Mutex<Option<Child>>,
    respawned: AtomicU64,
    window_error: Mutex<Option<String>>,
    suspend_token: AtomicU64,
    recorder: std::sync::OnceLock<super::recorder::RecorderHandle>,
}

impl OverlayManager {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app,
            link: Mutex::new(None),
            state: Mutex::new(HelperState::Idle),
            attached_game: Mutex::new(None),
            owner_game: Mutex::new(None),
            generation: AtomicU64::new(0),
            helper: tokio::sync::Mutex::new(None),
            respawned: AtomicU64::new(0),
            window_error: Mutex::new(None),
            suspend_token: AtomicU64::new(0),
            recorder: std::sync::OnceLock::new(),
        })
    }

    fn recorder(&self) -> &super::recorder::RecorderHandle {
        self.recorder
            .get_or_init(|| super::recorder::start(self.app.clone()))
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.get().is_some_and(|r| r.status.is_recording())
    }

    pub fn stop_recording_and_wait(&self, timeout: Duration) -> bool {
        self.recorder
            .get()
            .is_none_or(|recorder| recorder.stop_and_wait(timeout))
    }

    pub fn active_recording_path(&self) -> Option<PathBuf> {
        let recorder = self.recorder.get()?;
        (!recorder.status.is_idle())
            .then(|| recorder.status.active_path())
            .flatten()
    }

    fn rec_settings(&self) -> super::recorder::RecSettings {
        let config = self.config();
        let number = |key: &str, fallback: u32| {
            config
                .get(&format!("behavior.{key}"))
                .as_str()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(fallback)
        };
        let (audio, warnings) = self.audio_sources();
        super::recorder::RecSettings {
            height: match config.get("behavior.overlayRecRes").as_str() {
                Some("720") => 720,
                Some("1080") => 1080,
                Some("1440") => 1440,
                Some("2160") => 2160,
                _ => 0,
            },
            codec: config
                .get("behavior.overlayRecCodec")
                .as_str()
                .unwrap_or("h264")
                .to_string(),
            fps: number("overlayRecFps", 60).clamp(24, 240),
            quality: config
                .get("behavior.overlayRecQuality")
                .as_str()
                .unwrap_or("balanced")
                .to_string(),
            directory: capture_dir(&self.app),
            audio,
            warnings,
        }
    }

    fn game_pid(&self) -> u32 {
        self.link
            .lock()
            .as_ref()
            .map(|link| link.section.shared().game_pid.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    fn audio_sources(&self) -> (Vec<super::recorder::AudioSource>, Vec<String>) {
        use super::recorder::AudioSource;
        use super::recorder_audio::SourceKind;

        let chosen = csv_setting(
            &self.app,
            "behavior.overlayAudioTracks",
            DEFAULT_AUDIO_TRACKS,
        );
        let mut sources = Vec::new();
        let mut warnings = Vec::new();
        let source = |kind: SourceKind, name: &str| AudioSource {
            kind,
            name: name.to_string(),
        };
        let desktop = chosen.iter().any(|t| t == "desktop");

        if desktop {
            sources.push(source(SourceKind::Desktop, "desktop"));
        }
        if chosen.iter().any(|t| t == "mic") {
            sources.push(source(SourceKind::Microphone, "microphone"));
        }

        if desktop {
            if chosen.iter().any(|t| t == "game" || t == "discord") {
                log::info!(
                    "recorder: desktop audio already carries the game and Discord, so those tracks are not captured separately"
                );
            }
            return (sources, warnings);
        }

        if !supports_per_app_audio() {
            if chosen.iter().any(|t| t == "game" || t == "discord") {
                log::warn!(
                    "recorder: this build of Windows cannot capture one app's audio, so the game and Discord tracks are skipped"
                );
                warnings.push(
                    "No game or Discord audio in this clip: this version of Windows cannot capture one app's audio."
                        .to_string(),
                );
            }
            return (sources, warnings);
        }

        if chosen.iter().any(|t| t == "game") {
            match self.game_pid() {
                0 => {
                    log::warn!("recorder: no game process to capture audio from");
                    warnings.push(
                        "No game audio in this clip: the game process was not found.".to_string(),
                    );
                }
                pid => sources.push(source(SourceKind::Process(pid), "game")),
            }
        }

        if chosen.iter().any(|t| t == "discord") {
            let wanted = csv_setting(&self.app, "behavior.overlayAudioDiscord", "");
            let running = super::discord_audio::running_audio_clients();
            if running.is_empty() {
                log::warn!("recorder: no Discord client is running, so its audio is not captured");
                warnings.push("No Discord audio in this clip: Discord is not running.".to_string());
            }
            let chosen_client = |id: &String| wanted.is_empty() || wanted.contains(id);
            let take_all = !running.is_empty() && !running.iter().any(|(id, _, _)| chosen_client(id));
            if take_all {
                log::warn!(
                    "recorder: none of the chosen Discord clients ({}) is running, so every running Discord client is captured instead",
                    wanted.join(", ")
                );
            }
            for (id, label, pid) in running {
                if take_all || chosen_client(&id) {
                    log::info!("recorder: capturing audio from {label} (pid {pid})");
                    sources.push(source(SourceKind::Process(pid), &label));
                }
            }
        }

        (sources, warnings)
    }

    pub fn toggle_recording(&self) -> Result<(), String> {
        let idle = self.recorder.get().is_none_or(|r| r.status.toggle_starts());
        if idle && !super::recorder::recording_supported() {
            return self.refuse_recording(
                "Recording needs Windows Media Foundation. Install the Media Feature Pack from Optional features.",
            );
        }
        let hwnd = match super::overlay_window::game_hwnd() {
            Some(hwnd) => hwnd as i64,
            None if idle => return self.refuse_recording("No game is running."),
            None => 0,
        };
        let game_id = self.capture_game_id();
        let settings = self.rec_settings();
        self.recorder().toggle(hwnd, &game_id, settings);
        Ok(())
    }

    fn refuse_recording(&self, error: &str) -> Result<(), String> {
        let _ = self.app.emit("overlay-record-failed", json!({ "error": error }));
        Err(error.to_string())
    }

    fn config(&self) -> Arc<super::config::LauncherConfig> {
        self.app.state::<BackendState>().config.clone()
    }

    fn enabled(&self) -> bool {
        self.config().get("behavior.overlayEnabled") == Value::Bool(true)
    }

    fn set_state(&self, state: HelperState) {
        *self.state.lock() = state;
        let _ = self.app.emit(
            "overlay-status",
            json!({ "state": state.label(), "error": self.error_message() }),
        );
    }

    fn error_message(&self) -> String {
        self.link
            .lock()
            .as_ref()
            .map(|l| l.section.shared().error_message())
            .unwrap_or_default()
    }

    pub fn publish_hotkeys(&self) {
        let link = self.link.lock();
        let Some(link) = link.as_ref() else {
            return;
        };
        let shared = link.section.shared();
        let config = self.config();

        for (index, (key, label, default)) in HOTKEYS.iter().enumerate() {
            let stored = config.get(&format!("behavior.{key}"));
            let text = stored
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(default)
                .to_string();
            match parse_accelerator(&text) {
                Some((modifiers, vk)) => {
                    shared.hotkey_mods[index].store(modifiers, Ordering::Relaxed);
                    shared.hotkey_vk[index].store(vk, Ordering::Relaxed);
                    log::debug!("overlay: hotkey {index} is {text} (vk {vk}, mods {modifiers})");
                    if modifiers == 0 {
                        log::warn!("overlay: {label} is bound to the bare key {text}, which Windows will not deliver while an elevated game is in front");
                    }
                }
                None => {
                    log::warn!("overlay: \"{text}\" is not a usable shortcut for {label}");
                    shared.hotkey_vk[index].store(0, Ordering::Relaxed);
                }
            }
        }
        for index in HOTKEYS.len()..HOTKEY_COUNT {
            shared.hotkey_vk[index].store(0, Ordering::Relaxed);
        }
        shared.hotkey_generation.fetch_add(1, Ordering::Release);
        if let Some(signal) = link.to_helper.as_ref() {
            signal.raise();
        }
    }

    pub fn suspend_hotkeys(self: &Arc<Self>, on: bool) {
        let token = self.suspend_token.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let link = self.link.lock();
            let Some(link) = link.as_ref() else {
                return;
            };
            link.section
                .shared()
                .hotkey_suspend
                .store(u32::from(on), Ordering::Release);
            if let Some(signal) = link.to_helper.as_ref() {
                signal.raise();
            }
        }
        if !on {
            return;
        }
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(12)).await;
            if manager.suspend_token.load(Ordering::SeqCst) != token {
                return;
            }
            log::info!("overlay: the hotkey editor never finished, putting the shortcuts back");
            manager.suspend_hotkeys(false);
        });
    }

    pub fn set_game_in_front(&self, in_front: bool) {
        let link = self.link.lock();
        let Some(link) = link.as_ref() else {
            return;
        };
        let wanted = u32::from(in_front);
        if link.section.shared().game_in_front.swap(wanted, Ordering::AcqRel) == wanted {
            return;
        }
        log::info!(
            "overlay: {}",
            if in_front {
                "the game is in front again, taking the shortcuts back"
            } else {
                "another app is in front, handing the shortcuts back to it"
            }
        );
        if let Some(signal) = link.to_helper.as_ref() {
            signal.raise();
        }
    }

    fn hotkey_target_in_front(&self) -> bool {
        super::overlay_window::foreground_is_game_or_overlay(self.game_pid())
    }

    pub fn on_game_started(self: &Arc<Self>, game_id: &str, pid: Option<u32>) {
        if !self.enabled() {
            return;
        }
        let previous = self.owner_game.lock().replace(game_id.to_string());
        if let Some(previous) = previous.filter(|p| p != game_id) {
            log::info!("overlay: moving from {previous} to {game_id}");
        }
        let live = self.link.lock().is_some();
        if live {
            super::overlay_window::stop_tracking();
            self.end_session();
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        let manager = self.clone();
        let game = game_id.to_string();
        tauri::async_runtime::spawn(async move {
            manager.attach_windows(game, pid, generation).await;
        });

        let section = match SectionView::create() {
            Ok(section) => section,
            Err(code) => {
                log::error!("overlay: could not create the shared section (error {code})");
                self.set_state(HelperState::Failed);
                return;
            }
        };
        section
            .shared()
            .game_pid
            .store(pid.unwrap_or(0), Ordering::Relaxed);
        section.shared().game_in_front.store(0, Ordering::Relaxed);

        *self.link.lock() = Some(Link {
            section,
            to_helper: Signal::open_or_create(SIGNAL_TO_HELPER).ok(),
        });
        self.publish_hotkeys();
        self.set_state(HelperState::Starting);

        let manager = self.clone();
        let helper_game_id = game_id.to_string();
        tauri::async_runtime::spawn(async move {
            manager.spawn_helper(generation, &helper_game_id).await;
        });
    }

    async fn spawn_helper(self: &Arc<Self>, generation: u64, game_id: &str) {
        let Some(helper) = super::fs_util::resource(&self.app, HELPER_NAME) else {
            log::error!("overlay: {HELPER_NAME} is missing from this install");
            self.set_state(HelperState::Failed);
            return;
        };
        let cwd = helper
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        let launcher_pid = std::process::id().to_string();

        {
            let mut slot = self.helper.lock().await;
            if let Some(previous) = slot.take() {
                retire_helper(previous).await;
            }
            if self.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            match self.link.lock().as_ref() {
                Some(link) => link.section.shared().reset_session(),
                None => return,
            }
            match super::process_utils::spawn_tool(&helper, &[launcher_pid], &[], &cwd).await {
                Ok(child) => *slot = Some(child),
                Err(e) => {
                    log::warn!("overlay: the helper was not started ({e})");
                    self.set_state(HelperState::Failed);
                    return;
                }
            }
        }

        for _ in 0..40 {
            if self.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            match self.helper_check() {
                HelperCheck::Live => {
                    let bits = self
                        .link
                        .lock()
                        .as_ref()
                        .map(|l| l.section.shared().hotkey_registered.load(Ordering::Acquire))
                        .unwrap_or(0);
                    let config = self.config();
                    let report = hotkey_report(|key| config.get(key), bits);
                    log::info!(
                        "overlay: helper is running for {game_id} (protocol {HELPER_PROTOCOL:#x}; {})",
                        report.describe()
                    );
                    self.set_state(HelperState::Running);
                    self.clone().start_status_loop(generation);
                    return;
                }
                HelperCheck::Mismatch { theirs } => {
                    log::error!(
                        "overlay: the attached helper reports protocol {theirs:#x} but this launcher speaks {HELPER_PROTOCOL:#x}. It is a different build, so its readings are not being used. Helper: {}",
                        helper.display()
                    );
                    self.set_state(HelperState::Mismatched);
                    return;
                }
                HelperCheck::NotReady => {}
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        let status = self
            .link
            .lock()
            .as_ref()
            .map(|l| l.section.shared().status.load(Ordering::Acquire));
        let error = self.error_message();
        log::warn!(
            "overlay: the helper did not report in within 10s (status {}, error: {})",
            status.map_or_else(|| "unknown".to_string(), |s| s.to_string()),
            if error.is_empty() { "none" } else { error.as_str() }
        );
        self.set_state(HelperState::Failed);
    }

    async fn attach_windows(self: Arc<Self>, game_id: String, pid: Option<u32>, generation: u64) {
        if pid.filter(|p| *p != 0).is_none() {
            log::warn!("overlay: no pid for {game_id} — cannot attach");
            return;
        }
        let games = { self.app.state::<BackendState>().game.clone() };
        let mut watching = false;

        loop {
            let mut found = None;
            for _ in 0..WINDOW_WAIT_TICKS {
                if self.generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                let Some(probe) = games.running_pid(&game_id).or(pid).filter(|p| *p != 0) else {
                    self.set_game_in_front(false);
                    return;
                };
                if let Some(hwnd) = tauri::async_runtime::spawn_blocking(move || {
                    super::overlay_window::find_game_window(probe).map(|h| h as i64)
                })
                .await
                .ok()
                .flatten()
                {
                    found = Some((hwnd, probe));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }

            let Some((raw, probe)) = found else {
                log::warn!(
                    "overlay: {game_id} never showed a window — the overlay will not attach"
                );
                self.set_game_in_front(false);
                return;
            };

            for _ in 0..RETIRED_WINDOW_WAIT_TICKS {
                if !super::overlay_window::windows_retiring(&self.app) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }

            if self.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            *self.attached_game.lock() = Some(game_id.clone());
            let handle = raw as windows_sys::Win32::Foundation::HWND;
            let rect = super::overlay_window::client_rect(handle).unwrap_or_default();
            if let Err(e) = super::overlay_window::create_windows(&self.app, rect) {
                log::error!("overlay: {e}");
                *self.window_error.lock() = Some(e.clone());
                self.set_game_in_front(false);
                let state = *self.state.lock();
                let _ = self.app.emit(
                    "overlay-status",
                    json!({ "state": state.label(), "error": e, "windowError": e }),
                );
                super::notify::notify_if_backgrounded(
                    &self.app,
                    "Peebify overlay",
                    "The overlay window could not be created. Restart the game to try again.",
                );
                return;
            }
            *self.window_error.lock() = None;
            if let Some(link) = self.link.lock().as_ref() {
                link.section.shared().game_pid.store(probe, Ordering::Relaxed);
            }
            super::overlay_window::start_tracking(self.app.clone(), handle, probe);
            log::info!("overlay: attached to {game_id} (pid {probe})");

            if !watching {
                watching = true;
                self.clone().start_hotkey_watcher(generation);
            }

            loop {
                tokio::time::sleep(Duration::from_millis(300)).await;
                if self.generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                if super::overlay_window::tracking_ended() {
                    break;
                }
            }
            self.set_game_in_front(false);

            if !games.is_game_running_id(&game_id) {
                return;
            }
            log::info!(
                "overlay: {game_id} closed its window (pid {probe}), waiting for a new one or for the game to exit"
            );
        }
    }

    fn start_hotkey_watcher(self: Arc<Self>, generation: u64) {
        std::thread::Builder::new()
            .name("overlay-ipc".into())
            .spawn(move || {
                let Ok(signal) = Signal::open_or_create(SIGNAL_TO_LAUNCHER) else {
                    log::warn!("overlay: could not open the hotkey signal");
                    return;
                };
                let mut seen = [0u32; HOTKEY_COUNT];
                {
                    let link = self.link.lock();
                    if let Some(link) = link.as_ref() {
                        let shared = link.section.shared();
                        for (index, slot) in seen.iter_mut().enumerate() {
                            *slot = shared.hotkey_epoch[index].load(Ordering::Acquire);
                        }
                    }
                }

                loop {
                    signal.wait(500);
                    if self.generation.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    let fired: Vec<usize> = {
                        let link = self.link.lock();
                        let Some(link) = link.as_ref() else { return };
                        let shared = link.section.shared();
                        (0..HOTKEY_COUNT)
                            .filter(|index| {
                                let now = shared.hotkey_epoch[*index].load(Ordering::Acquire);
                                let changed = now != seen[*index];
                                seen[*index] = now;
                                changed
                            })
                            .collect()
                    };
                    for index in fired {
                        self.on_hotkey(index);
                    }
                }
            })
            .map(|_| ())
            .unwrap_or_else(|e| log::error!("overlay: could not start the hotkey watcher: {e}"));
    }

    fn on_hotkey(&self, index: usize) {
        let name = HOTKEYS
            .get(index)
            .map(|(_, label, _)| *label)
            .unwrap_or("unknown");
        if !self.hotkey_target_in_front() {
            log::info!("overlay: hotkey {index} ({name}) ignored because the game is not in front");
            return;
        }
        log::info!("overlay: hotkey {index} fired ({name})");
        match index {
            HOTKEY_TOGGLE => self.toggle_drawer(),
            HOTKEY_SHOT => {
                super::overlay_window::ensure_hud(&self.app);
                let app = self.app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = super::capture::take_screenshot(&app, &[]).await;
                });
            }
            HOTKEY_RECORD => {
                super::overlay_window::ensure_hud(&self.app);
                let _ = self.toggle_recording();
            }
            _ => {}
        }
    }

    pub fn on_setting_changed(self: &Arc<Self>, key: &str) {
        if HOTKEYS.iter().any(|(hotkey, _, _)| *hotkey == key) {
            self.publish_hotkeys();
            return;
        }
        if key == "overlayEnabled" {
            self.apply_enabled();
        }
    }

    fn apply_enabled(self: &Arc<Self>) {
        let live = self.link.lock().is_some();
        if !self.enabled() {
            if live || self.owner_game.lock().is_some() {
                log::info!("overlay: turned off while a game runs, so the session ends now");
                self.on_game_stopped();
            }
            return;
        }
        if live {
            return;
        }
        let running = self.app.state::<BackendState>().game.running_game();
        if let Some((game_id, pid)) = running {
            log::info!("overlay: turned on while {game_id} runs, so the session starts now");
            self.on_game_started(&game_id, pid);
        }
    }

    pub fn toggle_drawer(&self) {
        let Some(window) = self
            .app
            .get_webview_window(super::overlay_window::DRAWER_LABEL)
        else {
            log::warn!("overlay: the drawer window does not exist, so there is nothing to open");
            return;
        };
        if super::overlay_window::drawer_is_open() {
            log::info!("overlay: closing the drawer");
            self.close_drawer(true);
        } else if !super::overlay_window::drawer_blocked_by_fullscreen(&self.app) {
            log::info!("overlay: opening the drawer");
            super::overlay_window::set_drawer_open(true);
            let _ = window.show();
            super::overlay_window::activate_drawer(&self.app);
            let _ = self.app.emit("overlay-opened", Value::Null);
        }
    }

    pub fn close_drawer(&self, refocus_game: bool) {
        let held_foreground = refocus_game && super::overlay_window::we_hold_foreground();
        super::overlay_window::set_drawer_open(false);
        if let Some(window) = self
            .app
            .get_webview_window(super::overlay_window::DRAWER_LABEL)
        {
            let _ = window.hide();
        }
        if held_foreground {
            super::overlay_window::focus_game(&self.app);
        }
        let _ = self.app.emit("overlay-closed", Value::Null);
    }

    fn helper_check(&self) -> HelperCheck {
        let link = self.link.lock();
        let Some(link) = link.as_ref() else {
            return HelperCheck::NotReady;
        };
        let shared = link.section.shared();
        if !shared.is_valid() || shared.status.load(Ordering::Acquire) != STATUS_RUNNING {
            return HelperCheck::NotReady;
        }
        let theirs = shared.helper_protocol.load(Ordering::Acquire);
        if theirs != HELPER_PROTOCOL {
            return HelperCheck::Mismatch { theirs };
        }
        HelperCheck::Live
    }

    fn start_status_loop(self: Arc<Self>, generation: u64) {
        tauri::async_runtime::spawn(async move {
            let mut last_sample = 0u32;
            let mut stale_ticks = 0u32;
            let mut last_registered: Option<u32> = None;
            loop {
                tokio::time::sleep(STATUS_INTERVAL).await;
                if self.generation.load(Ordering::SeqCst) != generation {
                    return;
                }

                let registered = {
                    let link = self.link.lock();
                    let Some(link) = link.as_ref() else { return };
                    let shared = link.section.shared();
                    if !shared.is_valid() {
                        return;
                    }

                    let sample = shared.sample_seq.load(Ordering::Acquire);
                    if sample == last_sample {
                        stale_ticks += 1;
                    } else {
                        stale_ticks = 0;
                        last_sample = sample;
                    }

                    shared.hotkey_registered.load(Ordering::Acquire)
                };

                if last_registered.replace(registered) != Some(registered) {
                    let _ = self
                        .app
                        .emit(HOTKEYS_REGISTERED_EVENT, json!({ "registered": registered }));
                }
                if stale_ticks >= STALE_TICKS && self.restart_exited_helper(generation) {
                    return;
                }
                if stale_ticks == STALE_TICKS {
                    log::warn!("overlay: the helper stopped responding");
                    self.set_state(HelperState::Failed);
                } else if stale_ticks == 0 && *self.state.lock() == HelperState::Failed {
                    log::info!("overlay: the helper is responding again");
                    self.set_state(HelperState::Running);
                }
            }
        });
    }

    fn restart_exited_helper(self: &Arc<Self>, generation: u64) -> bool {
        let status = {
            let Ok(mut slot) = self.helper.try_lock() else {
                return false;
            };
            match slot.as_mut().map(Child::try_wait) {
                Some(Ok(Some(status))) => status,
                _ => return false,
            }
        };
        if self.respawned.swap(generation, Ordering::SeqCst) == generation {
            return false;
        }
        log::warn!("overlay: the helper exited ({status}), starting it again");
        self.set_state(HelperState::Starting);
        let manager = self.clone();
        let game_id = self.owner().unwrap_or_default();
        tauri::async_runtime::spawn(async move {
            manager.spawn_helper(generation, &game_id).await;
        });
        true
    }

    pub fn owner(&self) -> Option<String> {
        self.owner_game.lock().clone()
    }

    pub fn capture_game_id(&self) -> String {
        let state = self.app.state::<BackendState>();
        self.owner()
            .or_else(|| state.game.running_game().map(|(id, _)| id))
            .unwrap_or_else(|| state.config.active_game_id())
    }

    pub fn on_game_stopped(self: &Arc<Self>) {
        if let Some(recorder) = self.recorder.get() {
            recorder.stop();
        }
        super::overlay_window::stop_tracking();
        super::overlay_window::destroy_windows(&self.app);
        self.end_session();
        let generation = self.generation.load(Ordering::SeqCst);
        if let Some(game_id) = self.owner_game.lock().take() {
            log::info!("overlay: session ended for {game_id}");
        }
        self.set_state(HelperState::Idle);

        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut slot = manager.helper.lock().await;
            if manager.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            if let Some(child) = slot.take() {
                retire_helper(child).await;
            }
        });
    }

    fn end_session(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.attached_game.lock() = None;
        *self.window_error.lock() = None;

        let link = self.link.lock().take();
        if let Some(link) = link {
            link.section.shared().stop.store(1, Ordering::Release);
            if let Some(signal) = link.to_helper.as_ref() {
                signal.raise();
            }
        }
    }

    pub fn status(&self) -> Value {
        let state = *self.state.lock();
        let config = self.config();
        let (error, bits, game_pid) = {
            let link = self.link.lock();
            link.as_ref()
                .map(|l| {
                    let shared = l.section.shared();
                    (
                        shared.error_message(),
                        shared.hotkey_registered.load(Ordering::Acquire),
                        shared.game_pid.load(Ordering::Relaxed),
                    )
                })
                .unwrap_or((String::new(), 0, 0))
        };

        let live = matches!(state, HelperState::Running | HelperState::Mismatched);
        let mut report = hotkey_report(|key| config.get(key), bits);
        if !live {
            report.registered.clear();
            report.unregistered.clear();
        }

        json!({
            "state": state.label(),
            "enabled": self.enabled(),
            "gameId": self.attached_game.lock().clone(),
            "error": error,
            "registered": report.registered,
            "unregistered": report.unregistered,
            "invalid": report.invalid,
            "windowError": self.window_error.lock().clone(),
            "recording": self.is_recording(),
            "recordingSinceMs": self.recording_since_ms(),
            "sessionStartedMs": process_started_ms(game_pid),
            "hudFeedback": super::overlay_window::pending_feedback(),
        })
    }

    fn recording_since_ms(&self) -> u64 {
        self.recorder
            .get()
            .filter(|r| r.status.is_recording())
            .map(|r| r.status.started_ms())
            .unwrap_or(0)
    }
}

pub(super) async fn get_overlay_status(app: &AppHandle) -> Result<Value, String> {
    Ok(ok_with(app.state::<BackendState>().overlay.status()))
}

pub(super) async fn suspend_overlay_hotkeys(
    app: &AppHandle,
    args: &[Value],
) -> Result<Value, String> {
    let on = args.first().and_then(Value::as_bool).unwrap_or(false);
    app.state::<BackendState>().overlay.suspend_hotkeys(on);
    Ok(ok_with(json!({ "suspended": on })))
}

pub(super) async fn overlay_record(app: &AppHandle) -> Result<Value, String> {
    let overlay = app.state::<BackendState>().overlay.clone();
    Ok(match overlay.toggle_recording() {
        Ok(()) => ok_with(json!({ "pending": true })),
        Err(e) => err_response(e),
    })
}

pub(super) async fn overlay_toggle(app: &AppHandle, _args: &[Value]) -> Result<Value, String> {
    app.state::<BackendState>().overlay.close_drawer(true);
    Ok(ok_with(json!({})))
}

// ------------ Overlay Tests ------------
// Covers hotkey parsing and the clash report.
#[cfg(test)]
mod tests {
    use super::*;

    fn storable(text: &str) -> bool {
        parse_accelerator(text).is_some()
    }

    #[test]
    fn hotkey_report_splits_by_registration_bits() {
        let report = hotkey_report(|_| Value::Null, 0b011);
        assert_eq!(report.registered, vec!["Alt+P", "Alt+S"]);
        assert_eq!(report.unregistered, vec!["Alt+R"]);
        assert!(report.invalid.is_empty());
        assert!(!report.describe().contains("invalid"));

        let custom = hotkey_report(
            |key| {
                if key == "behavior.overlayShotHotkey" {
                    json!("Alt+Nope")
                } else {
                    json!("")
                }
            },
            0,
        );
        assert_eq!(custom.invalid, vec!["Alt+Nope"]);
        assert!(custom.registered.is_empty());
        assert!(custom.describe().starts_with("hotkeys registered: none;"));
        assert!(custom.describe().ends_with("; invalid: Alt+Nope"));
    }

    #[test]
    fn accepts_mappable_shortcuts() {
        assert!(storable("Alt+S"));
        assert!(storable("Ctrl+Shift+1"));
        assert!(storable("F9"));
        assert!(storable("Alt+PageUp"));
        assert!(storable("Ctrl+Space"));
        assert!(storable("Alt+ArrowUp"));
        assert!(storable("Ctrl+Enter"));
        assert!(storable("Alt+Backspace"));
    }

    #[test]
    fn arrows_enter_and_backspace_map_to_their_virtual_keys() {
        assert_eq!(parse_accelerator("Alt+ArrowLeft"), Some((0x0001, 0x25)));
        assert_eq!(parse_accelerator("Alt+Up"), Some((0x0001, 0x26)));
        assert_eq!(parse_accelerator("Ctrl+ArrowRight"), Some((0x0002, 0x27)));
        assert_eq!(parse_accelerator("Shift+ArrowDown"), Some((0x0004, 0x28)));
        assert_eq!(parse_accelerator("Alt+Enter"), Some((0x0001, 0x0D)));
        assert_eq!(parse_accelerator("Alt+Backspace"), Some((0x0001, 0x08)));
    }

    #[test]
    fn modifier_order_and_case_do_not_hide_a_clash() {
        assert_eq!(parse_accelerator("Ctrl+Alt+S"), parse_accelerator("alt+ctrl+s"));
        assert_ne!(parse_accelerator("Alt+S"), parse_accelerator("Alt+Shift+S"));
    }

    #[test]
    fn rejects_shortcuts_the_helper_cannot_register() {
        assert!(!storable("Shift+!"));
        assert!(!storable("Alt+ScrollLock"));
        assert!(!storable("Alt+&"));
        assert!(!storable("Alt+Ы"));
        assert!(!storable("S"));
        assert!(!storable("F25"));
    }

    #[test]
    fn every_hotkey_has_a_matching_default_and_no_retired_slot() {
        let mut merged = json!({});
        super::super::config::sanitize_behavior(&mut merged);
        for (key, _, default) in HOTKEYS {
            assert_eq!(merged["behavior"][key], json!(default), "{key}");
            assert!(storable(default), "{key}");
        }
        for retired in ["overlayReplayHotkey", "overlayHudHotkey"] {
            assert!(HOTKEYS.iter().all(|(key, _, _)| *key != retired));
            assert!(merged["behavior"].get(retired).is_none());
        }
    }
}
