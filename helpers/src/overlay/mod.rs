// ------------ Overlay Module ------------
// Wires the overlay helper together and exposes its shared section, plus the watchdog the helper uses to notice
// that the launcher has exited.

pub mod shared;

#[cfg(all(windows, feature = "overlay-runtime"))]
pub mod hotkeys;
#[cfg(all(windows, feature = "overlay-runtime"))]
pub mod telemetry;

#[cfg(windows)]
pub mod section {
    pub use crate::common::section::Signal;

    use super::shared::OverlayShared;

    pub type SectionView = crate::common::section::SectionView<OverlayShared>;

    pub mod watchdog {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };

        pub struct Launcher(HANDLE);

        unsafe impl Send for Launcher {}

        impl Drop for Launcher {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    unsafe { CloseHandle(self.0) };
                }
            }
        }

        pub fn open(pid: u32) -> Option<Launcher> {
            if pid == 0 {
                return None;
            }
            let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            (!handle.is_null()).then_some(Launcher(handle))
        }

        pub fn gone(launcher: &Option<Launcher>) -> bool {
            let Some(launcher) = launcher else {
                return false;
            };
            unsafe { WaitForSingleObject(launcher.0, 0) == WAIT_OBJECT_0 }
        }
    }
}
