// ------------ Windows Startup ------------
// Adds or removes the launcher from Windows sign in through the Run registry key, and clears the flag Windows keeps
// when someone turned it off in Startup Apps. Boot launches are marked with --from-boot.

use std::sync::OnceLock;

const APP_NAME: &str = "PeebifyLauncher";

static BOOT_LAUNCH: OnceLock<bool> = OnceLock::new();

pub fn is_boot_launch() -> bool {
    *BOOT_LAUNCH.get_or_init(|| std::env::args().any(|a| a == "--from-boot"))
}

pub type StartupNote = Option<String>;

fn approval_disables(bytes: &[u8]) -> bool {
    bytes.first().is_some_and(|b| b & 1 == 1)
}

mod registry {
    use std::io;

    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;

    use super::APP_NAME;

    const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const STARTUP_APPROVED_SUBKEY: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    fn hkcu() -> RegKey {
        RegKey::predef(HKEY_CURRENT_USER)
    }

    fn delete_app_value(subkey: &str) -> io::Result<bool> {
        let key = match hkcu().open_subkey_with_flags(subkey, KEY_SET_VALUE) {
            Ok(key) => key,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        match key.delete_value(APP_NAME) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }


    pub fn write_run_value(command: &str) -> io::Result<()> {
        let (key, _) = hkcu().create_subkey(RUN_SUBKEY)?;
        key.set_value(APP_NAME, &command)
    }

    pub fn delete_run_value() -> io::Result<bool> {
        delete_app_value(RUN_SUBKEY)
    }

    pub fn startup_approval() -> Option<Vec<u8>> {
        hkcu()
            .open_subkey(STARTUP_APPROVED_SUBKEY)
            .ok()?
            .get_raw_value(APP_NAME)
            .ok()
            .map(|value| value.bytes)
    }

    pub fn clear_startup_approval() -> io::Result<bool> {
        delete_app_value(STARTUP_APPROVED_SUBKEY)
    }
}

fn disabled_in_startup_apps() -> bool {
    registry::startup_approval().is_some_and(|bytes| approval_disables(&bytes))
}

fn boot_command() -> Result<String, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("could not resolve launcher path: {e}"))?;
    Ok(format!("\"{}\" --from-boot", exe.to_string_lossy()))
}

fn write_run_value(command: &str) -> Result<(), String> {
    registry::write_run_value(command)
        .map_err(|e| format!("could not write the Windows sign in entry: {e}"))
}

pub async fn manage_windows_startup(enable: bool) -> Result<StartupNote, String> {
    if enable {
        write_run_value(&boot_command()?)?;
        if !disabled_in_startup_apps() {
            return Ok(None);
        }
        return Ok(match registry::clear_startup_approval() {
            Ok(_) => {
                log::info!(
                    "[startup] cleared the disabled flag Windows startup apps kept for {APP_NAME}"
                );
                None
            }
            Err(e) => {
                log::warn!("[startup] could not clear the Windows startup apps flag: {e}");
                Some(
                    "Peebify was added to Windows sign in, but Windows startup apps still has it \
                     turned off. Turn it on in Settings > Apps > Startup."
                        .to_string(),
                )
            }
        });
    }

    registry::delete_run_value()
        .map_err(|e| format!("could not remove the Windows sign in entry: {e}"))?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::approval_disables;

    #[test]
    fn startup_approval_odd_first_byte_means_disabled() {
        assert!(approval_disables(&[0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(approval_disables(&[0x07, 0, 0, 0]));
        assert!(approval_disables(&[0x01]));
    }

    #[test]
    fn startup_approval_even_or_missing_first_byte_means_enabled() {
        assert!(!approval_disables(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!approval_disables(&[0x06]));
        assert!(!approval_disables(&[]));
    }
}
