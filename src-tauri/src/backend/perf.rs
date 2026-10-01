// ------------ Transfer Performance ------------
// Tuning for downloads: how many connections to use, how big the write buffers are, and how many threads verify files.
// While a transfer is running it also asks Windows not to throttle us or put the PC to sleep.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use parking_lot::Mutex;

pub const DOWNLOAD_CONCURRENCY: usize = 24;

pub const WRITE_BUFFER_BYTES: usize = (8 << 20) / DOWNLOAD_CONCURRENCY;

pub fn validation_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get() / 2)
        .unwrap_or(4)
        .clamp(4, 8)
}

static TRANSFERS_ACTIVE: AtomicUsize = AtomicUsize::new(0);
static TRANSFERS_IDLE: AtomicUsize = AtomicUsize::new(0);
static HOLDING_AWAKE: Mutex<bool> = Mutex::new(false);

const IDLE_PAUSED: u8 = 1;
const IDLE_OFFLINE: u8 = 2;

pub fn allow_full_speed() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, ProcessPowerThrottling, SetProcessInformation,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE,
    };

    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0,
    };
    let ok = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            std::ptr::addr_of!(state).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    if ok == 0 {
        log::warn!(
            "[perf] Could not opt out of processor power throttling ({}). Downloads may run at reduced speed on laptops.",
            std::io::Error::last_os_error()
        );
    } else {
        log::info!("[perf] Opted out of processor power throttling for this process.");
    }
}

fn awake_channel() -> &'static std::sync::mpsc::Sender<bool> {
    use std::sync::mpsc;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
    };

    static TX: OnceLock<mpsc::Sender<bool>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<bool>();
        std::thread::Builder::new()
            .name("peebify-power".into())
            .spawn(move || {
                while let Ok(keep_awake) = rx.recv() {
                    let flags = if keep_awake {
                        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
                    } else {
                        ES_CONTINUOUS
                    };
                    if unsafe { SetThreadExecutionState(flags) } == 0 {
                        log::warn!("[perf] Could not update the system sleep request.");
                    }
                }
            })
            .expect("the power thread cannot fail to spawn");
        tx
    })
}

fn set_awake(keep_awake: bool) {
    let _ = awake_channel().send(keep_awake);
}

pub fn transfers_active() -> usize {
    TRANSFERS_ACTIVE.load(Ordering::SeqCst)
}

fn refresh_awake() -> bool {
    let mut holding = HOLDING_AWAKE.lock();
    let want = TRANSFERS_ACTIVE.load(Ordering::SeqCst) > TRANSFERS_IDLE.load(Ordering::SeqCst);
    if want == *holding {
        return false;
    }
    *holding = want;
    set_awake(want);
    true
}

fn idle_change(before: u8, reason: u8, on: bool) -> Option<bool> {
    let after = if on { before | reason } else { before & !reason };
    match (before != 0, after != 0) {
        (false, true) => Some(true),
        (true, false) => Some(false),
        _ => None,
    }
}

pub struct TransferGuard {
    idle: AtomicU8,
}

impl TransferGuard {
    pub fn acquire() -> Self {
        if TRANSFERS_ACTIVE.fetch_add(1, Ordering::SeqCst) == 0 {
            log::info!("[perf] Transfer started — holding the system awake and unthrottled.");
        }
        refresh_awake();
        Self {
            idle: AtomicU8::new(0),
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.set_idle_reason(IDLE_PAUSED, paused);
    }

    pub fn set_offline(&self, offline: bool) {
        self.set_idle_reason(IDLE_OFFLINE, offline);
    }

    fn set_idle_reason(&self, reason: u8, on: bool) {
        let before = if on {
            self.idle.fetch_or(reason, Ordering::SeqCst)
        } else {
            self.idle.fetch_and(!reason, Ordering::SeqCst)
        };
        match idle_change(before, reason, on) {
            Some(true) => {
                TRANSFERS_IDLE.fetch_add(1, Ordering::SeqCst);
                if refresh_awake() {
                    log::info!("[perf] Transfer paused or waiting for the network, released the sleep request.");
                }
            }
            Some(false) => {
                TRANSFERS_IDLE.fetch_sub(1, Ordering::SeqCst);
                if refresh_awake() {
                    log::info!("[perf] Transfer moving again, holding the system awake.");
                }
            }
            None => {}
        }
    }
}

impl Drop for TransferGuard {
    fn drop(&mut self) {
        if *self.idle.get_mut() != 0 {
            TRANSFERS_IDLE.fetch_sub(1, Ordering::SeqCst);
        }
        if TRANSFERS_ACTIVE.fetch_sub(1, Ordering::SeqCst) == 1 {
            log::info!("[perf] Transfers finished — released the system sleep request.");
        }
        refresh_awake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_counts_as_idle_once_whatever_the_reasons() {
        assert_eq!(idle_change(0, IDLE_PAUSED, true), Some(true));
        assert_eq!(idle_change(IDLE_PAUSED, IDLE_PAUSED, true), None);
        assert_eq!(idle_change(IDLE_PAUSED, IDLE_OFFLINE, true), None);
        assert_eq!(idle_change(IDLE_PAUSED | IDLE_OFFLINE, IDLE_OFFLINE, false), None);
        assert_eq!(idle_change(IDLE_PAUSED, IDLE_PAUSED, false), Some(false));
        assert_eq!(idle_change(0, IDLE_OFFLINE, false), None);
    }
}

pub use power::watch_suspend_resume;

mod power {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Once;

    use windows_sys::Win32::System::Power::{
        PowerRegisterSuspendResumeNotification, DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND,
    };

    use super::super::now_ms;

    static SUSPENDED_AT_MS: AtomicI64 = AtomicI64::new(0);
    static REGISTER: Once = Once::new();

    fn suspend_message(transfers_active: usize) -> String {
        format!("[power] System suspending (transfers active: {transfers_active}).")
    }

    fn resume_message(suspended_at_ms: i64, now_ms: i64) -> String {
        if suspended_at_ms <= 0 {
            return "[power] System resumed.".to_string();
        }
        let secs = (now_ms - suspended_at_ms).max(0) / 1000;
        format!("[power] Resumed after {secs}s.")
    }

    unsafe extern "system" fn on_power_event(
        _context: *const core::ffi::c_void,
        kind: u32,
        _setting: *const core::ffi::c_void,
    ) -> u32 {
        match kind {
            PBT_APMSUSPEND => {
                SUSPENDED_AT_MS.store(now_ms(), Ordering::SeqCst);
                log::info!("{}", suspend_message(super::transfers_active()));
            }
            PBT_APMRESUMEAUTOMATIC => {
                let suspended_at = SUSPENDED_AT_MS.swap(0, Ordering::SeqCst);
                log::info!("{}", resume_message(suspended_at, now_ms()));
                super::super::http::recheck_after_resume();
            }
            _ => {}
        }
        0
    }

    pub fn watch_suspend_resume() {
        REGISTER.call_once(|| {
            let params: &'static DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS =
                Box::leak(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                    Callback: Some(on_power_event),
                    Context: std::ptr::null_mut(),
                }));
            let mut handle: *mut core::ffi::c_void = std::ptr::null_mut();
            let status = unsafe {
                PowerRegisterSuspendResumeNotification(
                    DEVICE_NOTIFY_CALLBACK,
                    (params as *const DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS)
                        .cast_mut()
                        .cast(),
                    &mut handle,
                )
            };
            if status != 0 {
                log::warn!(
                    "[power] Could not subscribe to suspend and resume notifications ({}).",
                    std::io::Error::from_raw_os_error(status as i32)
                );
            }
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn suspend_message_names_active_transfers() {
            assert_eq!(
                suspend_message(2),
                "[power] System suspending (transfers active: 2)."
            );
        }

        #[test]
        fn resume_message_reports_whole_seconds_asleep() {
            assert_eq!(
                resume_message(1_000_000, 1_125_900),
                "[power] Resumed after 125s."
            );
        }

        #[test]
        fn resume_message_without_a_recorded_suspend() {
            assert_eq!(resume_message(0, 1_125_900), "[power] System resumed.");
        }

        #[test]
        fn resume_message_clamps_a_backwards_clock() {
            assert_eq!(
                resume_message(2_000_000, 1_000_000),
                "[power] Resumed after 0s."
            );
        }
    }
}
