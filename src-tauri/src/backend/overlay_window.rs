// ------------ Overlay Windows ------------
// Creates the overlay drawer and the recording readout window and keeps them lined up on top of the game window,
// following it as it moves, resizes or loses focus. Windows only.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Once, OnceLock};

use parking_lot::Mutex;
use tauri::{AppHandle, Emitter, Listener, Manager, WebviewUrl, WebviewWindowBuilder};

use super::now_ms;
use super::window_manager::WEBVIEW_BROWSER_ARGS;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{ClipCursor, SetForegroundWindow};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumWindows, GetClientRect, GetForegroundWindow, GetWindow,
    GetWindowLongPtrW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    MsgWaitForMultipleObjectsEx, PeekMessageW, SetWindowLongPtrW, SetWindowPos, TranslateMessage,
    CHILDID_SELF, EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, GWL_EXSTYLE, GW_OWNER, HWND_NOTOPMOST,
    HWND_TOPMOST, MSG, MWMO_INPUTAVAILABLE, OBJID_WINDOW, PM_REMOVE, QS_ALLINPUT, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

pub const DRAWER_LABEL: &str = "overlay";
pub const HUD_LABEL: &str = "overlay-hud";

pub const FULLSCREEN_BLOCKED_EVENT: &str = "overlay-fullscreen-blocked";

const RECONCILE_MS: u128 = 500;
const DEBOUNCE_MS: u128 = 16;
const GAME_EVENT_RANGES: [(u32, u32); 3] = [
    (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
    (EVENT_OBJECT_DESTROY, EVENT_OBJECT_DESTROY),
    (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
];
const FEEDBACK_HOLD_MS: i64 = 5_000;
const RETIRE_WAIT_STEPS: u32 = 40;
const FEEDBACK_EVENTS: [&str; 5] = [
    "overlay-capture",
    "overlay-capture-failed",
    "overlay-record-started",
    "overlay-record-stopped",
    "overlay-record-failed",
];

static FEEDBACK_UNTIL_MS: AtomicI64 = AtomicI64::new(0);
static HUD_CREATING: AtomicBool = AtomicBool::new(false);

struct HookState {
    dirty: AtomicBool,
    foreground_changed: AtomicBool,
    windows_changed: AtomicBool,
    destroyed: AtomicBool,
    game_hwnd: AtomicI64,
}

fn hook_state() -> &'static HookState {
    static STATE: OnceLock<HookState> = OnceLock::new();
    STATE.get_or_init(|| HookState {
        dirty: AtomicBool::new(false),
        foreground_changed: AtomicBool::new(false),
        windows_changed: AtomicBool::new(false),
        destroyed: AtomicBool::new(false),
        game_hwnd: AtomicI64::new(0),
    })
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object_id: i32,
    child_id: i32,
    _thread: u32,
    _time: u32,
) {
    let state = hook_state();
    if event == EVENT_OBJECT_LOCATIONCHANGE && object_id != OBJID_WINDOW {
        return;
    }
    let tracked = state.game_hwnd.load(Ordering::Relaxed);
    if tracked != 0
        && hwnd as i64 != tracked
        && (event == EVENT_OBJECT_LOCATIONCHANGE || event == EVENT_OBJECT_DESTROY)
    {
        return;
    }

    match event {
        EVENT_OBJECT_DESTROY if object_id == OBJID_WINDOW && child_id == CHILDID_SELF as i32 => {
            state.destroyed.store(true, Ordering::Relaxed)
        }
        EVENT_OBJECT_DESTROY => {}
        EVENT_SYSTEM_FOREGROUND | EVENT_SYSTEM_MINIMIZESTART | EVENT_SYSTEM_MINIMIZEEND => {
            state.foreground_changed.store(true, Ordering::Relaxed);
            state.dirty.store(true, Ordering::Relaxed);
        }
        _ => state.dirty.store(true, Ordering::Relaxed),
    }
}

unsafe extern "system" fn foreground_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _object_id: i32,
    _child_id: i32,
    _thread: u32,
    _time: u32,
) {
    let state = hook_state();
    state.foreground_changed.store(true, Ordering::Relaxed);
    state.dirty.store(true, Ordering::Relaxed);
}

struct Search {
    pid: u32,
    best: HWND,
    best_area: i64,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let search = &mut *(lparam as *mut Search);

    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid != search.pid || IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    if !GetWindow(hwnd, GW_OWNER).is_null() {
        return 1;
    }

    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect) == 0 {
        return 1;
    }
    let area = (rect.right - rect.left) as i64 * (rect.bottom - rect.top) as i64;
    if area > search.best_area {
        search.best_area = area;
        search.best = hwnd;
    }
    1
}

pub fn find_game_window(pid: u32) -> Option<HWND> {
    if pid == 0 {
        return None;
    }
    let mut search = Search {
        pid,
        best: std::ptr::null_mut(),
        best_area: 0,
    };
    unsafe { EnumWindows(Some(enum_proc), &mut search as *mut Search as LPARAM) };
    (!search.best.is_null() && search.best_area > 0).then_some(search.best)
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub fn client_rect(hwnd: HWND) -> Option<Rect> {
    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
        return None;
    }
    let mut origin = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    if unsafe { ClientToScreen(hwnd, &mut origin) } == 0 {
        return None;
    }
    let width = client.right - client.left;
    let height = client.bottom - client.top;
    (width > 0 && height > 0).then_some(Rect {
        x: origin.x,
        y: origin.y,
        width,
        height,
    })
}

pub fn is_exclusive_fullscreen(hwnd: HWND) -> bool {
    use windows_sys::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_RUNNING_D3D_FULL_SCREEN,
    };

    let mut state = 0;
    let shell_says_fullscreen = unsafe { SHQueryUserNotificationState(&mut state) } >= 0
        && state == QUNS_RUNNING_D3D_FULL_SCREEN;
    if !shell_says_fullscreen {
        return false;
    }

    let mut window = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut window) } == 0
    {
        return false;
    }

    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info: MONITORINFO = unsafe { core::mem::zeroed() };
    info.cbSize = core::mem::size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return false;
    }

    window.left == info.rcMonitor.left
        && window.top == info.rcMonitor.top
        && window.right == info.rcMonitor.right
        && window.bottom == info.rcMonitor.bottom
}

pub fn drawer_blocked_by_fullscreen(app: &AppHandle) -> bool {
    let Some(hwnd) = game_hwnd() else {
        return false;
    };
    if !is_exclusive_fullscreen(hwnd) {
        return false;
    }
    log::info!("overlay: not opening the drawer because the game is in exclusive fullscreen");
    let _ = app.emit(FULLSCREEN_BLOCKED_EVENT, serde_json::Value::Null);
    true
}

struct Feedback {
    at_ms: i64,
    event: &'static str,
    payload: String,
}

fn last_feedback() -> &'static Mutex<Option<Feedback>> {
    static LAST: OnceLock<Mutex<Option<Feedback>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

fn note_capture_feedback(event: &'static str, payload: &str) {
    let now = now_ms();
    FEEDBACK_UNTIL_MS.store(now + FEEDBACK_HOLD_MS, Ordering::Relaxed);
    *last_feedback().lock() = Some(Feedback {
        at_ms: now,
        event,
        payload: payload.to_string(),
    });
    hook_state().dirty.store(true, Ordering::Relaxed);
    wake_tracker();
}

fn feedback_json(feedback: &Feedback, now: i64) -> Option<serde_json::Value> {
    if now - feedback.at_ms > FEEDBACK_HOLD_MS {
        return None;
    }
    let payload =
        serde_json::from_str(&feedback.payload).unwrap_or(serde_json::Value::Null);
    Some(serde_json::json!({ "event": feedback.event, "payload": payload }))
}

pub fn pending_feedback() -> Option<serde_json::Value> {
    last_feedback()
        .lock()
        .as_ref()
        .and_then(|feedback| feedback_json(feedback, now_ms()))
}

fn install_feedback_listeners(app: &AppHandle) {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        for event in FEEDBACK_EVENTS {
            let handle = app.clone();
            app.listen_any(event, move |e| {
                note_capture_feedback(event, e.payload());
                ensure_hud(&handle);
            });
        }
    });
}

fn hud_window_wanted(should_show: bool, recording: bool, feedback: bool) -> bool {
    should_show && (recording || feedback)
}

fn hwnd_of(window: &tauri::WebviewWindow) -> Option<HWND> {
    window.hwnd().ok().map(|h| h.0 as HWND)
}

static DRAWER_HWND: AtomicI64 = AtomicI64::new(0);
static HUD_HWND: AtomicI64 = AtomicI64::new(0);

fn cached_hwnd(label: &str) -> Option<HWND> {
    let raw = if label == DRAWER_LABEL {
        DRAWER_HWND.load(Ordering::Relaxed)
    } else {
        HUD_HWND.load(Ordering::Relaxed)
    };
    (raw != 0).then_some(raw as HWND)
}

fn remember_hwnd(label: &str, hwnd: HWND) {
    let slot = if label == DRAWER_LABEL {
        &DRAWER_HWND
    } else {
        &HUD_HWND
    };
    slot.store(hwnd as i64, Ordering::Relaxed);
}

fn retiring(app: &AppHandle, label: &str) -> bool {
    cached_hwnd(label).is_none() && app.get_webview_window(label).is_some()
}

pub fn windows_retiring(app: &AppHandle) -> bool {
    retiring(app, DRAWER_LABEL) || retiring(app, HUD_LABEL)
}

fn wait_until_retired(app: &AppHandle, label: &str) {
    for _ in 0..RETIRE_WAIT_STEPS {
        if !retiring(app, label) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    log::warn!("overlay: the previous {label} window is still closing, reusing it");
}

fn our_hwnds() -> Vec<HWND> {
    [DRAWER_LABEL, HUD_LABEL]
        .iter()
        .filter_map(|label| cached_hwnd(label))
        .collect()
}

fn apply_readout_styles(hwnd: HWND) {
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let wanted = current as u32
            | WS_EX_NOACTIVATE
            | WS_EX_TRANSPARENT
            | WS_EX_TOOLWINDOW
            | WS_EX_LAYERED;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted as isize);
    }
}

fn apply_drawer_styles(hwnd: HWND) {
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            (current as u32 | WS_EX_TOOLWINDOW) as isize,
        );
    }
}

fn drop_topmost(hwnd: HWND) {
    unsafe {
        SetWindowPos(
            hwnd,
            HWND_NOTOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    };
}

fn reassert_topmost(hwnd: HWND) {
    unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    };
}

fn is_covered_by(ours: HWND, game: HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindow, GW_HWNDPREV};
    let mut cursor = ours;
    for _ in 0..64 {
        cursor = unsafe { GetWindow(cursor, GW_HWNDPREV) };
        if cursor.is_null() {
            return false;
        }
        if cursor == game {
            return true;
        }
    }
    false
}

pub fn focus_game(app: &AppHandle) {
    let Some(hwnd) = game_hwnd() else { return };
    let raw = hwnd as i64;
    let _ = app.run_on_main_thread(move || {
        if unsafe { SetForegroundWindow(raw as HWND) } == 0 {
            log::debug!("overlay: the game did not take the foreground back");
        }
    });
}

pub fn activate_drawer(app: &AppHandle) {
    let Some(hwnd) = cached_hwnd(DRAWER_LABEL) else {
        log::warn!("overlay: no drawer handle yet, cannot give it the foreground");
        return;
    };
    let raw = hwnd as i64;
    let _ = app.run_on_main_thread(move || {
        let hwnd = raw as HWND;
        unsafe {
            SetForegroundWindow(hwnd);
            SetFocus(hwnd);
            ClipCursor(std::ptr::null());
            if GetForegroundWindow() != hwnd {
                log::warn!(
                    "overlay: the drawer did not take the foreground, the game may keep the cursor"
                );
            }
        }
    });
}

pub fn we_hold_foreground() -> bool {
    let foreground = unsafe { GetForegroundWindow() };
    !foreground.is_null() && our_hwnds().contains(&foreground)
}

fn is_game_or_overlay(foreground: HWND, game_hwnd: Option<HWND>, game_pid: u32, ours: &[HWND]) -> bool {
    if foreground.is_null() {
        return false;
    }
    if Some(foreground) == game_hwnd || ours.contains(&foreground) {
        return true;
    }
    if game_pid == 0 {
        return false;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(foreground, &mut pid) };
    pid == game_pid
}

pub fn foreground_is_game_or_overlay(game_pid: u32) -> bool {
    let foreground = unsafe { GetForegroundWindow() };
    is_game_or_overlay(foreground, game_hwnd(), game_pid, &our_hwnds())
}

pub fn game_hwnd() -> Option<HWND> {
    session()
        .lock()
        .as_ref()
        .map(|s| s.game_hwnd as HWND)
        .filter(|h| !h.is_null())
}

struct Wake(HANDLE);

unsafe impl Send for Wake {}
unsafe impl Sync for Wake {}

impl Wake {
    fn new() -> Self {
        Self(unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) })
    }

    fn raise(&self) {
        if !self.0.is_null() {
            unsafe { SetEvent(self.0) };
        }
    }

    fn wait_or_message(&self, timeout_ms: u32) {
        let (count, handles) = if self.0.is_null() {
            (0, std::ptr::null())
        } else {
            (1, &self.0 as *const HANDLE)
        };
        unsafe {
            MsgWaitForMultipleObjectsEx(count, handles, timeout_ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

pub struct Session {
    stop: Arc<AtomicBool>,
    pub drawer_open: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    wake: Arc<Wake>,
    pub game_hwnd: i64,
}

static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

fn session() -> &'static Mutex<Option<Session>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

fn wake_tracker() {
    if let Some(session) = session().lock().as_ref() {
        session.wake.raise();
    }
}

pub fn create_windows(app: &AppHandle, rect: Rect) -> Result<(), String> {
    install_feedback_listeners(app);
    wait_until_retired(app, DRAWER_LABEL);
    if app.get_webview_window(DRAWER_LABEL).is_none() {
        let drawer =
            WebviewWindowBuilder::new(app, DRAWER_LABEL, WebviewUrl::App("overlay.html".into()))
                .title("Peebify Overlay")
                .inner_size(rect.width as f64, rect.height as f64)
                .position(rect.x as f64, rect.y as f64)
                .decorations(false)
                .transparent(true)
                .shadow(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .maximizable(false)
                .minimizable(false)
                .focused(false)
                .visible(false)
                .additional_browser_args(WEBVIEW_BROWSER_ARGS)
                .build()
                .map_err(|e| format!("could not create the overlay window: {e}"))?;
        let Some(hwnd) = hwnd_of(&drawer) else {
            let _ = drawer.destroy();
            return Err(
                "the overlay window was registered but never came up. This happens when its browser arguments differ from the main window's, which WebView2 refuses."
                    .to_string(),
            );
        };
        remember_hwnd(DRAWER_LABEL, hwnd);
        apply_drawer_styles(hwnd);
    } else if let Some(window) = app.get_webview_window(DRAWER_LABEL) {
        let Some(hwnd) = hwnd_of(&window) else {
            return Err("the overlay window from the previous session has no handle".to_string());
        };
        remember_hwnd(DRAWER_LABEL, hwnd);
    }

    create_hud(app, rect)?;
    Ok(())
}

fn create_hud(app: &AppHandle, rect: Rect) -> Result<(), String> {
    wait_until_retired(app, HUD_LABEL);
    if app.get_webview_window(HUD_LABEL).is_none() {
        let hud = WebviewWindowBuilder::new(app, HUD_LABEL, WebviewUrl::App("hud.html".into()))
            .title("Peebify Readout")
            .inner_size(rect.width as f64, rect.height as f64)
            .position(rect.x as f64, rect.y as f64)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .focused(false)
            .visible(false)
            .additional_browser_args(WEBVIEW_BROWSER_ARGS)
            .build()
            .map_err(|e| format!("could not create the readout window: {e}"))?;
        let _ = hud.set_ignore_cursor_events(true);
        let Some(hwnd) = hwnd_of(&hud) else {
            let _ = hud.destroy();
            return Err("the readout window was registered but never came up".to_string());
        };
        remember_hwnd(HUD_LABEL, hwnd);
        apply_readout_styles(hwnd);
    } else if let Some(window) = app.get_webview_window(HUD_LABEL) {
        let Some(hwnd) = hwnd_of(&window) else {
            return Err("the readout window from the previous session has no handle".to_string());
        };
        remember_hwnd(HUD_LABEL, hwnd);
    }
    Ok(())
}

pub fn ensure_hud(app: &AppHandle) {
    if HUD_HWND.load(Ordering::Relaxed) != 0 || HUD_CREATING.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(target) = game_hwnd() else {
        HUD_CREATING.store(false, Ordering::SeqCst);
        return;
    };
    let app = app.clone();
    let raw = target as i64;
    tauri::async_runtime::spawn_blocking(move || {
        let rect = client_rect(raw as HWND).unwrap_or_default();
        match create_hud(&app, rect) {
            Ok(()) if game_hwnd() == Some(raw as HWND) => {
                log::info!("overlay: created the readout window on demand");
                let state = hook_state();
                state.windows_changed.store(true, Ordering::Relaxed);
                state.dirty.store(true, Ordering::Relaxed);
                wake_tracker();
            }
            Ok(()) => {
                HUD_HWND.store(0, Ordering::Relaxed);
                if let Some(window) = app.get_webview_window(HUD_LABEL) {
                    let _ = window.destroy();
                }
            }
            Err(e) => log::error!("overlay: {e}"),
        }
        HUD_CREATING.store(false, Ordering::SeqCst);
    });
}

pub fn destroy_windows(app: &AppHandle) {
    DRAWER_HWND.store(0, Ordering::Relaxed);
    HUD_HWND.store(0, Ordering::Relaxed);
    for label in [DRAWER_LABEL, HUD_LABEL] {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.destroy();
        }
    }
}

pub fn start_tracking(app: AppHandle, game_hwnd: HWND, game_pid: u32) {
    stop_tracking();

    let stop = Arc::new(AtomicBool::new(false));
    let drawer_open = Arc::new(AtomicBool::new(false));
    let warned_fullscreen = Arc::new(AtomicU32::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let wake = Arc::new(Wake::new());
    if wake.0.is_null() {
        log::warn!("overlay: could not create the tracker's wake event, so it falls back to the reconciliation tick");
    }

    *session().lock() = Some(Session {
        stop: stop.clone(),
        drawer_open: drawer_open.clone(),
        finished: finished.clone(),
        wake: wake.clone(),
        game_hwnd: game_hwnd as i64,
    });

    let state = hook_state();
    state.game_hwnd.store(game_hwnd as i64, Ordering::Relaxed);
    state.destroyed.store(false, Ordering::Relaxed);
    state.dirty.store(true, Ordering::Relaxed);
    state.foreground_changed.store(true, Ordering::Relaxed);

    let raw_hwnd = game_hwnd as i64;
    std::thread::Builder::new()
        .name("overlay-winevent".into())
        .spawn(move || {
            track_loop(app, raw_hwnd, game_pid, stop, &wake, drawer_open, warned_fullscreen);
            finished.store(true, Ordering::Relaxed);
        })
        .map(|_| ())
        .unwrap_or_else(|e| log::error!("overlay: could not start the tracker thread: {e}"));
}

pub fn tracking_ended() -> bool {
    session()
        .lock()
        .as_ref()
        .map(|s| s.finished.load(Ordering::Relaxed))
        .unwrap_or(true)
}

pub fn stop_tracking() {
    if let Some(session) = session().lock().take() {
        session.stop.store(true, Ordering::Relaxed);
        session.wake.raise();
    }
}

pub fn set_drawer_open(open: bool) {
    if let Some(session) = session().lock().as_ref() {
        session.drawer_open.store(open, Ordering::Relaxed);
    }
}

pub fn drawer_is_open() -> bool {
    session()
        .lock()
        .as_ref()
        .is_some_and(|s| s.drawer_open.load(Ordering::Relaxed))
}

fn track_loop(
    app: AppHandle,
    raw_hwnd: i64,
    game_pid: u32,
    stop: Arc<AtomicBool>,
    wake: &Wake,
    drawer_open: Arc<AtomicBool>,
    warned_fullscreen: Arc<AtomicU32>,
) {
    let game_hwnd = raw_hwnd as HWND;
    let hooks: Vec<HWINEVENTHOOK> = GAME_EVENT_RANGES
        .iter()
        .map(|&(first, last)| unsafe {
            SetWinEventHook(
                first,
                last,
                std::ptr::null_mut(),
                Some(win_event_proc),
                game_pid,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        })
        .filter(|hook| !hook.is_null())
        .collect();
    if hooks.len() < GAME_EVENT_RANGES.len() {
        log::warn!("overlay: SetWinEventHook failed — falling back to the reconciliation tick");
    }

    let foreground_hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            std::ptr::null_mut(),
            Some(foreground_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    if foreground_hook.is_null() {
        log::warn!("overlay: the desktop foreground hook failed — leaving another app will take up to half a second to register");
    }

    let state = hook_state();
    let mut last_rect = Rect::default();
    let mut last_visible: Option<(bool, bool)> = None;
    let mut last_reconcile = std::time::Instant::now();
    let mut dirty_since: Option<std::time::Instant> = None;

    while !stop.load(Ordering::Relaxed) {
        pump_messages();

        if state.destroyed.load(Ordering::Relaxed) {
            log::debug!("overlay: the game window went away");
            break;
        }

        if state.windows_changed.swap(false, Ordering::Relaxed) {
            last_rect = Rect::default();
            last_visible = None;
        }

        if state.dirty.swap(false, Ordering::Relaxed) {
            dirty_since = Some(std::time::Instant::now());
        }

        let now = std::time::Instant::now();
        let debounced =
            dirty_since.is_some_and(|t| now.duration_since(t).as_millis() >= DEBOUNCE_MS);
        let due = now.duration_since(last_reconcile).as_millis() >= RECONCILE_MS;

        if debounced || due {
            dirty_since = None;
            last_reconcile = now;
            reconcile(
                &app,
                game_hwnd,
                game_pid,
                &mut last_rect,
                &mut last_visible,
                &drawer_open,
                &warned_fullscreen,
                state.foreground_changed.swap(false, Ordering::Relaxed),
            );
        }

        if stop.load(Ordering::Relaxed) {
            break;
        }
        wake.wait_or_message(wait_ms(
            dirty_since,
            last_reconcile,
            std::time::Instant::now(),
        ));
    }

    for hook in hooks {
        unsafe { UnhookWinEvent(hook) };
    }
    if !foreground_hook.is_null() {
        unsafe { UnhookWinEvent(foreground_hook) };
    }
    if drawer_open.swap(false, Ordering::Relaxed) {
        let _ = app.emit("overlay-closed", serde_json::Value::Null);
    }
    for label in [DRAWER_LABEL, HUD_LABEL] {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.hide();
        }
    }
}

fn wait_ms(
    dirty_since: Option<std::time::Instant>,
    last_reconcile: std::time::Instant,
    now: std::time::Instant,
) -> u32 {
    let left = |since: std::time::Instant, span: u128| {
        span.saturating_sub(now.saturating_duration_since(since).as_millis())
    };
    let reconcile = left(last_reconcile, RECONCILE_MS);
    let settle = dirty_since.map_or(reconcile, |since| left(since, DEBOUNCE_MS));
    settle.min(reconcile) as u32
}

fn pump_messages() {
    let mut message: MSG = unsafe { core::mem::zeroed() };
    while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn reconcile(
    app: &AppHandle,
    game_hwnd: HWND,
    game_pid: u32,
    last_rect: &mut Rect,
    last_visible: &mut Option<(bool, bool)>,
    drawer_open: &Arc<AtomicBool>,
    warned_fullscreen: &Arc<AtomicU32>,
    foreground_changed: bool,
) {
    let minimized = unsafe { IsIconic(game_hwnd) } != 0;
    let foreground = unsafe { GetForegroundWindow() };

    let ours: Vec<HWND> = our_hwnds();
    let foreground_is_relevant = foreground == game_hwnd || ours.contains(&foreground);
    if !foreground.is_null() {
        if let Some(state) = app.try_state::<super::state::BackendState>() {
            state.overlay.set_game_in_front(is_game_or_overlay(
                foreground,
                Some(game_hwnd),
                game_pid,
                &ours,
            ));
        }
    }

    let should_show = !minimized && foreground_is_relevant;
    let recording = app
        .try_state::<super::state::BackendState>()
        .is_some_and(|state| state.overlay.is_recording());
    let feedback = FEEDBACK_UNTIL_MS.load(Ordering::Relaxed) > now_ms();
    let hud_wanted = hud_window_wanted(should_show, recording, feedback);

    if !should_show && drawer_open.swap(false, Ordering::Relaxed) {
        log::info!("overlay: the foreground left the game, closing the drawer");
        let _ = app.emit("overlay-closed", serde_json::Value::Null);
        *last_visible = None;
    }
    let drawer_wanted = should_show && drawer_open.load(Ordering::Relaxed);

    if should_show {
        follow_client_rect(app, game_hwnd, last_rect);
    }

    if Some((drawer_wanted, hud_wanted)) != *last_visible {
        *last_visible = Some((drawer_wanted, hud_wanted));
        for label in [DRAWER_LABEL, HUD_LABEL] {
            let Some(window) = app.get_webview_window(label) else {
                continue;
            };
            let wanted = if label == DRAWER_LABEL {
                drawer_wanted
            } else {
                hud_wanted
            };
            let Some(hwnd) = cached_hwnd(label) else {
                continue;
            };
            let is_drawer = label == DRAWER_LABEL;
            let raw = hwnd as i64;
            let _ = app.run_on_main_thread(move || {
                let hwnd = raw as HWND;
                if wanted {
                    reassert_topmost(hwnd);
                } else {
                    drop_topmost(hwnd);
                }
                if is_drawer {
                    apply_drawer_styles(hwnd);
                } else {
                    apply_readout_styles(hwnd);
                }
            });
            let _ = if wanted { window.show() } else { window.hide() };
        }
    }

    if !should_show {
        return;
    }

    if foreground_changed {
        let covered: Vec<i64> = ours
            .iter()
            .filter(|hwnd| is_covered_by(**hwnd, game_hwnd))
            .map(|hwnd| *hwnd as i64)
            .collect();
        if !covered.is_empty() {
            let _ = app.run_on_main_thread(move || {
                for raw in covered {
                    reassert_topmost(raw as HWND);
                }
            });
        }

        if warned_fullscreen.load(Ordering::Relaxed) == 0 && is_exclusive_fullscreen(game_hwnd) {
            warned_fullscreen.store(1, Ordering::Relaxed);
            log::info!(
                "overlay: the game is in exclusive fullscreen, so the overlay cannot draw over it"
            );
            let _ = app.emit(FULLSCREEN_BLOCKED_EVENT, serde_json::Value::Null);
            super::notify::notify_if_backgrounded(
                app,
                "Peebify overlay",
                "Switch the game to Borderless or Windowed for the overlay to appear.",
            );
        }
    }
}

fn follow_client_rect(app: &AppHandle, game_hwnd: HWND, last_rect: &mut Rect) {
    if let Some(rect) = client_rect(game_hwnd) {
        if rect != *last_rect {
            let level = if *last_rect == Rect::default() {
                log::Level::Info
            } else {
                log::Level::Debug
            };
            log::log!(
                level,
                "overlay: game client area at {},{} size {}x{}",
                rect.x,
                rect.y,
                rect.width,
                rect.height
            );
            *last_rect = rect;
            for label in [DRAWER_LABEL, HUD_LABEL] {
                let Some(window) = app.get_webview_window(label) else {
                    continue;
                };
                let _ = window.set_position(tauri::PhysicalPosition::new(rect.x, rect.y));
                let _ = window.set_size(tauri::PhysicalSize::new(
                    rect.width as u32,
                    rect.height as u32,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hud_window_shows_for_recording_or_capture_feedback() {
        assert!(hud_window_wanted(true, true, false));
        assert!(hud_window_wanted(true, false, true));
        assert!(!hud_window_wanted(true, false, false));
        assert!(!hud_window_wanted(false, true, true));
    }

    #[test]
    fn a_late_readout_replays_only_recent_capture_feedback() {
        let feedback = Feedback {
            at_ms: 10_000,
            event: "overlay-capture",
            payload: r#"{"path":"C:\\shots\\a.png"}"#.to_string(),
        };
        let replay = feedback_json(&feedback, 10_000 + FEEDBACK_HOLD_MS).unwrap();
        assert_eq!(replay["event"], "overlay-capture");
        assert_eq!(replay["payload"]["path"], "C:\\shots\\a.png");
        assert!(feedback_json(&feedback, 10_001 + FEEDBACK_HOLD_MS).is_none());

        let empty = Feedback {
            at_ms: 0,
            event: "overlay-record-failed",
            payload: String::new(),
        };
        assert_eq!(feedback_json(&empty, 0).unwrap()["payload"], serde_json::Value::Null);
    }

    #[test]
    fn the_tracker_sleeps_until_a_change_settles_or_the_next_tick() {
        use std::time::{Duration, Instant};
        let now = Instant::now() + Duration::from_secs(1);
        let ago = |ms: u64| now - Duration::from_millis(ms);
        assert_eq!(wait_ms(None, now, now), RECONCILE_MS as u32);
        assert_eq!(wait_ms(None, ago(200), now), 300);
        assert_eq!(wait_ms(None, ago(900), now), 0);
        assert_eq!(wait_ms(Some(now), now, now), DEBOUNCE_MS as u32);
        assert_eq!(wait_ms(Some(ago(10)), ago(100), now), 6);
        assert_eq!(wait_ms(Some(ago(10)), ago(495), now), 5);
        assert_eq!(wait_ms(Some(ago(40)), ago(100), now), 0);
    }

    #[test]
    fn shortcuts_belong_to_the_game_only_while_it_or_the_overlay_is_in_front() {
        let game = 0x1000 as HWND;
        let drawer = 0x2000 as HWND;
        let other = 0x3000 as HWND;
        let ours = [drawer];
        assert!(is_game_or_overlay(game, Some(game), 0, &ours));
        assert!(is_game_or_overlay(drawer, Some(game), 0, &ours));
        assert!(!is_game_or_overlay(other, Some(game), 0, &ours));
        assert!(!is_game_or_overlay(other, None, u32::MAX, &ours));
        assert!(!is_game_or_overlay(std::ptr::null_mut(), Some(game), 0, &ours));
    }
}
