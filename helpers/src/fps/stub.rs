// ------------ FPS Stub ------------
// The DLL (peebify_helpers.dll) that runs inside the game once the helper hooks it in. It finds the frame-rate field,
// then keeps writing the chosen FPS, a lower one in the background and the game own value back when paused.

use core::ffi::c_void;
use core::ops::Range;
use core::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, GetModuleHandleW};
use windows_sys::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
};
use windows_sys::Win32::System::Threading::{CreateThread, GetCurrentProcessId};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
};

use crate::fps::pe;
use crate::fps::scan;
use crate::fps::section::{SectionView, Signal};
use crate::fps::shared::{
    Shared, SIGNAL_EVENT_NAME, STATUS_ATTACHED, STATUS_IDLE, STATUS_READY, STOP_NONE, STOP_PAUSED,
};

const POLL_ACTIVE_MS: u32 = 16;
const POLL_BURST_MS: u32 = 4;
const POLL_BACKGROUND_MS: u32 = 100;
const POLL_PAUSED_MS: u32 = 250;
const BURST_WINDOW: Duration = Duration::from_secs(2);
const BACKGROUND_DELAY: Duration = Duration::from_secs(1);
const COMPANION_TITLE: &str = "Peebify Overlay";

const PAGE_ACCESS_MASK: u32 = 0xFF;

const DLL_PROCESS_ATTACH: u32 = 1;
const LDR_ADDREF_DLL_PIN: u32 = 1;

#[link(name = "ntdll")]
extern "system" {
    fn LdrAddRefDll(flags: u32, base: *mut c_void) -> i32;
}

#[no_mangle]
pub unsafe extern "system" fn WndProc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    CallNextHookEx(core::ptr::null_mut(), code, wparam, lparam)
}

#[no_mangle]
pub unsafe extern "system" fn DllMain(
    instance: *mut c_void,
    reason: u32,
    _reserved: *mut c_void,
) -> BOOL {
    if reason != DLL_PROCESS_ATTACH {
        return 1;
    }
    DisableThreadLibraryCalls(instance);
    let thread = CreateThread(
        core::ptr::null(),
        0,
        Some(worker),
        instance,
        0,
        core::ptr::null_mut(),
    );
    if !thread.is_null() {
        CloseHandle(thread);
    }
    1
}

unsafe extern "system" fn worker(instance: *mut c_void) -> u32 {
    let Ok(section) = SectionView::open() else {
        return 0;
    };
    let shared = section.shared();
    if !shared.is_configured() || shared.game_pid.load(Ordering::Acquire) != GetCurrentProcessId()
    {
        return 0;
    }

    LdrAddRefDll(LDR_ADDREF_DLL_PIN, instance);

    shared.status.store(STATUS_ATTACHED, Ordering::Release);

    let framerate = match locate(shared) {
        Ok(pointer) => pointer,
        Err(message) => {
            shared.fail(&message);
            return 0;
        }
    };

    let signal = Signal::open_or_create(SIGNAL_EVENT_NAME).ok();
    shared.status.store(STATUS_READY, Ordering::Release);
    if hold(shared, framerate, signal.as_ref()) {
        shared.status.store(STATUS_IDLE, Ordering::Release);
    }
    0
}

unsafe fn locate(shared: &Shared) -> Result<*mut i32, String> {
    let base = GetModuleHandleW(core::ptr::null()) as *const u8;
    if base.is_null() {
        return Err("Could not read the game's module base.".to_string());
    }
    let Some(image) = pe::parse(base) else {
        return Err(
            "The game's executable is not in a layout the unlocker understands.".to_string(),
        );
    };

    let live = LiveImage {
        base,
        size: image.size_of_image,
    };

    let site = shared.hint_rva.load(Ordering::Relaxed);
    if shared.hint_fingerprint.load(Ordering::Relaxed) == image.fingerprint {
        if let Some(rva) = hinted_field(&live, image.code_section.clone(), site) {
            let pointer = base.add(rva) as *mut i32;
            if is_writable(pointer) {
                shared.scan_micros.store(0, Ordering::Relaxed);
                publish(shared, site, image.fingerprint);
                return Ok(pointer);
            }
        }
    }

    let started = std::time::Instant::now();
    let found = scan::resolve_framerate_rva(&live, image.code_section.clone())
        .map_err(|error| error.message())?;
    let elapsed = started.elapsed().as_micros().clamp(1, u32::MAX as u128) as u32;

    let pointer = base.add(found.rva as usize) as *mut i32;
    if !is_writable(pointer) {
        return Err(
            "The frame-rate value was found in memory the unlocker is not allowed to write to."
                .to_string(),
        );
    }

    shared.scan_micros.store(elapsed, Ordering::Relaxed);
    publish(shared, found.site as u64, image.fingerprint);
    Ok(pointer)
}

fn hinted_field<I: scan::Image + ?Sized>(
    image: &I,
    code: Range<usize>,
    site: u64,
) -> Option<usize> {
    let site = usize::try_from(site).ok().filter(|&site| site != 0)?;
    let end = site.checked_add(scan::SITE_LEN)?;
    if site < code.start || end > code.end {
        return None;
    }
    let found = scan::resolve_framerate_rva(image, site..end).ok()?;
    usize::try_from(found.rva).ok()
}

fn publish(shared: &Shared, site: u64, fingerprint: u64) {
    shared.found_rva.store(site, Ordering::Relaxed);
    shared
        .found_fingerprint
        .store(fingerprint, Ordering::Release);
}

struct LiveImage {
    base: *const u8,
    size: usize,
}

impl LiveImage {
    fn region(&self, at: usize) -> Option<(usize, bool)> {
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { core::mem::zeroed() };
        let size = core::mem::size_of::<MEMORY_BASIC_INFORMATION>();
        let address = self.base.wrapping_add(at) as *const c_void;
        if unsafe { VirtualQuery(address, &mut info, size) } != size {
            return None;
        }
        let end = (info.BaseAddress as usize)
            .checked_add(info.RegionSize)?
            .checked_sub(self.base as usize)?
            .min(self.size);
        (end > at).then_some((end, is_readable(&info)))
    }
}

impl scan::Image for LiveImage {
    fn len(&self) -> usize {
        self.size
    }

    fn read(&self, at: usize, out: &mut [u8]) -> bool {
        let Some(end) = at.checked_add(out.len()) else {
            return false;
        };
        if end > self.size {
            return false;
        }
        let mut cursor = at;
        while cursor < end {
            match self.region(cursor) {
                Some((region_end, true)) => cursor = region_end,
                _ => return false,
            }
        }
        unsafe { copy_volatile(self.base.add(at), out) };
        true
    }

    fn readable_run(&self, at: usize, end: usize) -> Option<Range<usize>> {
        let end = end.min(self.size);
        let mut start = at;
        loop {
            if start >= end {
                return None;
            }
            let (region_end, readable) = self.region(start)?;
            if readable {
                break;
            }
            start = region_end;
        }
        let mut stop = start;
        while stop < end {
            match self.region(stop) {
                Some((region_end, true)) => stop = region_end,
                _ => break,
            }
        }
        Some(start..stop.min(end))
    }
}

fn is_readable(info: &MEMORY_BASIC_INFORMATION) -> bool {
    info.State == MEM_COMMIT
        && info.Protect & PAGE_GUARD == 0
        && matches!(
            info.Protect & PAGE_ACCESS_MASK,
            PAGE_READONLY
                | PAGE_READWRITE
                | PAGE_WRITECOPY
                | PAGE_EXECUTE_READ
                | PAGE_EXECUTE_READWRITE
                | PAGE_EXECUTE_WRITECOPY
        )
}

unsafe fn copy_volatile(source: *const u8, out: &mut [u8]) {
    const WORD: usize = core::mem::size_of::<usize>();
    let head = source.align_offset(WORD).min(out.len());
    for (index, slot) in out[..head].iter_mut().enumerate() {
        *slot = source.add(index).read_volatile();
    }
    let mut words = out[head..].chunks_exact_mut(WORD);
    let mut at = source.add(head);
    for slot in &mut words {
        let word = (at as *const usize).read_volatile();
        slot.copy_from_slice(&word.to_ne_bytes());
        at = at.add(WORD);
    }
    for (index, slot) in words.into_remainder().iter_mut().enumerate() {
        *slot = at.add(index).read_volatile();
    }
}

unsafe fn is_writable(pointer: *const i32) -> bool {
    let mut info: MEMORY_BASIC_INFORMATION = core::mem::zeroed();
    let size = core::mem::size_of::<MEMORY_BASIC_INFORMATION>();
    if VirtualQuery(pointer as *const c_void, &mut info, size) != size {
        return false;
    }
    matches!(
        info.Protect,
        PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY
    )
}

unsafe fn hold(shared: &Shared, framerate: *mut i32, signal: Option<&Signal>) -> bool {
    let pid = GetCurrentProcessId();
    let mut focus = Focus::new(pid);
    let mut field = Field::default();
    let mut last_reset: Option<Instant> = None;

    loop {
        match command(shared, pid) {
            Command::Hold => {}
            Command::Pause => {
                if let Some(own) = field.release(framerate.read_volatile()) {
                    framerate.write_volatile(own);
                }
                match paused(shared, pid, signal) {
                    Command::Hold => continue,
                    other => return other == Command::Exit,
                }
            }
            Command::Exit => return true,
            Command::Leave => return false,
        }

        let power_save = shared.background_fps.load(Ordering::Relaxed) > 0;
        let focused = !power_save || focus.poll();
        let desired = shared.effective_fps(focused);

        let current = framerate.read_volatile();
        if field.observe(current) {
            shared.resets.fetch_add(1, Ordering::Relaxed);
            last_reset = Some(Instant::now());
        }
        if current != desired {
            framerate.write_volatile(desired);
        }
        field.hold(desired);

        wait(signal, poll_interval(focused, last_reset));
    }
}

unsafe fn paused(shared: &Shared, pid: u32, signal: Option<&Signal>) -> Command {
    loop {
        match command(shared, pid) {
            Command::Pause => wait(signal, POLL_PAUSED_MS),
            other => return other,
        }
    }
}

#[derive(Debug, PartialEq)]
enum Command {
    Hold,
    Pause,
    Exit,
    Leave,
}

fn command(shared: &Shared, pid: u32) -> Command {
    let stop = shared.stop.load(Ordering::Acquire);
    if shared.game_pid.load(Ordering::Acquire) != pid {
        return Command::Leave;
    }
    match stop {
        STOP_NONE => Command::Hold,
        STOP_PAUSED => Command::Pause,
        _ => Command::Exit,
    }
}

unsafe fn wait(signal: Option<&Signal>, interval: u32) {
    match signal {
        Some(signal) => {
            signal.wait(interval);
        }
        None => windows_sys::Win32::System::Threading::Sleep(interval),
    }
}

#[derive(Default)]
struct Field {
    held: Option<i32>,
    own: Option<i32>,
}

impl Field {
    fn observe(&mut self, current: i32) -> bool {
        match self.held {
            None => {
                self.own = Some(current);
                false
            }
            Some(held) if held != current => {
                self.own = Some(current);
                true
            }
            Some(_) => false,
        }
    }

    fn hold(&mut self, value: i32) {
        self.held = Some(value);
    }

    fn release(&mut self, current: i32) -> Option<i32> {
        let ours = self.held.take() == Some(current);
        self.own.filter(|&own| ours && own != current)
    }
}

fn poll_interval(focused: bool, last_reset: Option<Instant>) -> u32 {
    if !focused {
        return POLL_BACKGROUND_MS;
    }
    match last_reset {
        Some(at) if at.elapsed() < BURST_WINDOW => POLL_BURST_MS,
        _ => POLL_ACTIVE_MS,
    }
}

struct Foreground {
    pid: u32,
    companion: bool,
}

struct Focus {
    self_pid: u32,
    focused: bool,
    away_since: Option<Instant>,
}

impl Focus {
    fn new(self_pid: u32) -> Self {
        Self {
            self_pid,
            focused: true,
            away_since: None,
        }
    }

    unsafe fn poll(&mut self) -> bool {
        let owner = foreground_window().map(|(window, pid)| Foreground {
            pid,
            companion: pid != self.self_pid && is_companion(window),
        });
        self.observe(owner, Instant::now())
    }

    fn observe(&mut self, owner: Option<Foreground>, now: Instant) -> bool {
        match owner {
            None => {}
            Some(owner) if owner.pid == self.self_pid || owner.companion => {
                self.away_since = None;
                self.focused = true;
            }
            Some(_) => {
                let away = *self.away_since.get_or_insert(now);
                if now.duration_since(away) >= BACKGROUND_DELAY {
                    self.focused = false;
                }
            }
        }
        self.focused
    }
}

unsafe fn foreground_window() -> Option<(HWND, u32)> {
    let window: HWND = GetForegroundWindow();
    if window.is_null() {
        return None;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(window, &mut pid);
    (pid != 0).then_some((window, pid))
}

unsafe fn is_companion(window: HWND) -> bool {
    let mut buffer = [0u16; 32];
    let length = GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32);
    length > 0 && is_companion_title(&buffer[..length as usize])
}

fn is_companion_title(title: &[u16]) -> bool {
    title.iter().copied().eq(COMPANION_TITLE.encode_utf16())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAME: u32 = 100;
    const OTHER: u32 = 200;

    fn owner(pid: u32, companion: bool) -> Option<Foreground> {
        Some(Foreground { pid, companion })
    }

    #[test]
    fn other_window_backgrounds_after_delay() {
        let mut focus = Focus::new(GAME);
        let start = Instant::now();
        assert!(focus.observe(owner(OTHER, false), start));
        assert!(!focus.observe(owner(OTHER, false), start + BACKGROUND_DELAY));
        assert!(focus.observe(owner(GAME, false), start + BACKGROUND_DELAY * 2));
    }

    #[test]
    fn companion_window_keeps_focus() {
        let mut focus = Focus::new(GAME);
        let start = Instant::now();
        assert!(focus.observe(owner(OTHER, true), start));
        assert!(focus.observe(owner(OTHER, true), start + BACKGROUND_DELAY * 5));
    }

    #[test]
    fn companion_resets_away_timer() {
        let mut focus = Focus::new(GAME);
        let start = Instant::now();
        focus.observe(owner(OTHER, false), start);
        focus.observe(owner(OTHER, true), start + BACKGROUND_DELAY / 2);
        assert!(focus.observe(owner(OTHER, false), start + BACKGROUND_DELAY));
    }

    #[test]
    fn live_image_skips_and_refuses_unreadable_pages() {
        use crate::fps::scan::Image;
        use windows_sys::Win32::System::Memory::{
            VirtualAlloc, VirtualFree, VirtualProtect, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS,
        };

        const PAGE: usize = 4096;
        unsafe {
            let base = VirtualAlloc(
                core::ptr::null(),
                PAGE * 3,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            ) as *mut u8;
            assert!(!base.is_null());
            for index in 0..PAGE * 3 {
                base.add(index).write(index as u8);
            }
            let mut old = 0u32;
            let middle = base.add(PAGE) as *const c_void;
            assert_ne!(VirtualProtect(middle, PAGE, PAGE_NOACCESS, &mut old), 0);

            let live = LiveImage {
                base,
                size: PAGE * 3,
            };
            assert_eq!(live.readable_run(0, PAGE * 3), Some(0..PAGE));
            assert_eq!(live.readable_run(PAGE, PAGE * 3), Some(PAGE * 2..PAGE * 3));
            assert_eq!(live.readable_run(PAGE + 8, PAGE * 2), None);

            let mut four = [0u8; 4];
            assert!(!live.read(PAGE - 2, &mut four));
            assert!(!live.read(PAGE * 3 - 2, &mut four));
            assert!(live.read(PAGE * 2 + 1, &mut four));
            assert_eq!(four, [1, 2, 3, 4]);

            let mut odd = [0u8; 37];
            assert!(live.read(3, &mut odd));
            assert!(odd.iter().enumerate().all(|(i, &b)| b == (i + 3) as u8));

            VirtualFree(base as *mut c_void, 0, MEM_RELEASE);
        }
    }

    const SITE: usize = 0x40;
    const FIELD: usize = 0x300;

    fn branch(image: &mut [u8], at: usize, opcode: u8, target: usize) {
        image[at] = opcode;
        let rel = target as i64 - (at as i64 + 5);
        image[at + 1..at + 5].copy_from_slice(&(rel as i32).to_le_bytes());
    }

    fn put_site(image: &mut [u8], site: usize, thunk: usize, mov: usize, field: usize) {
        image[site..site + 5].copy_from_slice(&[0xB9, 0x3C, 0x00, 0x00, 0x00]);
        branch(image, site + 5, 0xE8, thunk);
        branch(image, thunk, 0xE9, mov);
        image[mov..mov + 3].copy_from_slice(&[0x48, 0x89, 0x05]);
        let rel = field as i64 - (mov as i64 + 7);
        image[mov + 3..mov + 7].copy_from_slice(&(rel as i32).to_le_bytes());
    }

    fn image_with_site() -> Vec<u8> {
        let mut image = vec![0u8; 0x400];
        put_site(&mut image, SITE, 0x100, 0x180, FIELD);
        image
    }

    #[test]
    fn hinted_site_in_code_resolves_to_its_field() {
        let image = image_with_site();
        assert_eq!(hinted_field(&image[..], 0..0x400, SITE as u64), Some(FIELD));
        assert_eq!(
            hinted_field(&image[..], SITE..SITE + scan::SITE_LEN, SITE as u64),
            Some(FIELD)
        );
    }

    #[test]
    fn hints_that_are_not_a_call_site_in_code_are_refused() {
        let image = image_with_site();
        assert_eq!(hinted_field(&image[..], 0..0x400, FIELD as u64), None);
        assert_eq!(hinted_field(&image[..], 0..0x400, 0x100), None);
        assert_eq!(hinted_field(&image[..], 0..0x400, SITE as u64 + 1), None);
        assert_eq!(hinted_field(&image[..], 0..0x400, 0), None);
        assert_eq!(hinted_field(&image[..], 0..0x400, u64::MAX), None);
        assert_eq!(hinted_field(&image[..], 0x80..0x400, SITE as u64), None);
        assert_eq!(
            hinted_field(&image[..], 0..SITE + scan::SITE_LEN - 1, SITE as u64),
            None
        );
    }

    #[test]
    fn scanned_site_is_accepted_as_the_next_hint() {
        let mut image = image_with_site();
        put_site(&mut image, 0x10, 0x110, 0x1C0, 0x310);
        put_site(&mut image, 0x60, 0x120, 0x1A0, FIELD);
        let found = scan::resolve_framerate_rva(&image[..], 0..0x400).expect("resolve the field");
        assert_eq!((found.rva as usize, found.site as usize), (FIELD, SITE));
        assert_eq!(
            hinted_field(&image[..], 0..0x400, found.site as u64),
            Some(FIELD)
        );
    }

    #[test]
    fn pause_puts_back_the_games_own_value() {
        let mut field = Field::default();
        assert!(!field.observe(60));
        field.hold(120);
        assert!(!field.observe(120));
        field.hold(120);
        assert_eq!(field.release(120), Some(60));

        assert!(!field.observe(60));
        field.hold(144);
        assert_eq!(field.release(144), Some(60));
    }

    #[test]
    fn a_reset_by_the_game_becomes_its_own_value() {
        let mut field = Field::default();
        field.observe(60);
        field.hold(120);
        assert!(field.observe(30));
        field.hold(120);
        assert_eq!(field.release(120), Some(30));
    }

    #[test]
    fn pause_leaves_a_value_the_game_wrote_since_the_last_poll() {
        let mut field = Field::default();
        field.observe(60);
        field.hold(120);
        assert_eq!(field.release(45), None);
    }

    #[test]
    fn pause_before_the_first_poll_writes_nothing() {
        let mut field = Field::default();
        assert_eq!(field.release(60), None);
        field.observe(120);
        field.hold(120);
        assert_eq!(field.release(120), None);
    }

    fn section_for(pid: u32) -> Box<Shared> {
        let shared: Box<Shared> = Box::new(unsafe { core::mem::zeroed() });
        shared.initialize();
        shared.game_pid.store(pid, Ordering::Release);
        shared
    }

    #[test]
    fn the_stub_follows_its_own_sessions_stop() {
        use crate::fps::shared::STOP_ENDED;

        let shared = section_for(GAME);
        assert_eq!(command(&shared, GAME), Command::Hold);
        shared.stop.store(STOP_PAUSED, Ordering::Release);
        assert_eq!(command(&shared, GAME), Command::Pause);
        shared.stop.store(STOP_NONE, Ordering::Release);
        assert_eq!(command(&shared, GAME), Command::Hold);
        shared.stop.store(STOP_ENDED, Ordering::Release);
        assert_eq!(command(&shared, GAME), Command::Exit);
    }

    #[test]
    fn a_paused_stub_leaves_when_another_launch_takes_the_section() {
        let shared = section_for(GAME);
        shared.stop.store(STOP_PAUSED, Ordering::Release);

        shared.initialize();
        assert_eq!(command(&shared, GAME), Command::Leave);
        shared.game_pid.store(OTHER, Ordering::Release);
        assert_eq!(command(&shared, GAME), Command::Leave);
        shared.stop.store(STOP_PAUSED, Ordering::Release);
        assert_eq!(command(&shared, GAME), Command::Leave);
        assert_eq!(command(&shared, OTHER), Command::Pause);
    }

    #[test]
    fn companion_title_is_exact() {
        let title: Vec<u16> = COMPANION_TITLE.encode_utf16().collect();
        assert!(is_companion_title(&title));
        assert!(!is_companion_title(&title[..title.len() - 1]));
        let longer: Vec<u16> = "Peebify Overlay 2".encode_utf16().collect();
        assert!(!is_companion_title(&longer));
        assert!(!is_companion_title(&[]));
    }
}
