// ------------ System Notifications ------------
// Shows Windows toast notifications, but only when the launcher is in the background and the player has turned
// them on. Also registers the app's identity with Windows so the toast shows the Peebify name and icon.

use serde_json::Value;
use tauri::{AppHandle, Manager};

use super::state::BackendState;

pub fn notify_if_backgrounded(app: &AppHandle, title: &str, body: &str) {
    let enabled = matches!(
        app.state::<BackendState>()
            .config
            .get("behavior.osNotifications"),
        Value::Bool(true)
    );
    if !enabled {
        log::debug!("[os-notify] Skipped \"{title}\": system notifications are off");
        return;
    }

    if let Some(window) = app.get_webview_window("main") {
        let visible = window.is_visible().unwrap_or(true);
        let minimized = window.is_minimized().unwrap_or(false);
        let focused = window.is_focused().unwrap_or(false);
        if visible && !minimized && focused {
            log::debug!("[os-notify] Skipped \"{title}\": the launcher window is focused");
            return;
        }
    }

    if let Err(e) = show(app, title, body) {
        log::warn!("[os-notify] Failed to show system notification \"{title}\": {e}");
    }
}

fn show(app: &AppHandle, title: &str, body: &str) -> Result<(), String> {
    use tauri_winrt_notification::{Duration, Toast};

    let app = app.clone();
    Toast::new(AUMID)
        .title(title)
        .text1(body)
        .duration(Duration::Short)
        .on_activated(move |_| {
            if let Some(state) = app.try_state::<BackendState>() {
                state.window.show_window();
            }
            Ok(())
        })
        .show()
        .map_err(|e| e.to_string())
}

pub const AUMID: &str = "com.peebify.launcher";

const DISPLAY_NAME: &str = "Peebify Launcher";

pub fn icon_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;

    let installed = dir.join("icons").join("app.png");
    if installed.exists() {
        return Some(installed);
    }

    let dev = dir.parent()?.parent()?.join("webui/public/icons/app.png");
    dev.exists().then_some(dev)
}

fn describe_to_shell() -> std::io::Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(format!(r"Software\Classes\AppUserModelId\{AUMID}"))?;
    key.set_value("DisplayName", &DISPLAY_NAME)?;
    key.set_value("ShowInSettings", &1u32)?;

    match icon_path() {
        Some(icon) => key.set_value("IconUri", &icon.as_os_str())?,
        None => log::warn!("[aumid] app.png not found; toasts will show no icon"),
    }
    Ok(())
}

pub fn init() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    if let Err(e) = describe_to_shell() {
        log::warn!("[aumid] could not describe {AUMID} to the shell: {e}");
    }

    let wide: Vec<u16> = std::ffi::OsStr::new(AUMID)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let hr = unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) };
    if hr < 0 {
        log::warn!("[aumid] SetCurrentProcessExplicitAppUserModelID failed: 0x{hr:08X}");
    } else {
        log::info!("[aumid] process identity set to {AUMID}");
    }
}
