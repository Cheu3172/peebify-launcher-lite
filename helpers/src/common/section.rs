// ------------ Shared Section ------------
// The shared memory link between the launcher and the helpers. SectionView maps a named block both sides read and
// write, and Signal is a named event to wake the other side. The FPS unlocker and overlay each lay it out their own way.

use core::ffi::c_void;
use core::marker::PhantomData;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ,
    FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MsgWaitForMultipleObjectsEx, MWMO_INPUTAVAILABLE, QS_ALLINPUT,
};

use super::wide;

const INVALID_HANDLE: HANDLE = -1isize as HANDLE;

pub trait Layout {
    const NAME: &'static str;
    const SIZE: usize;
    fn initialize(&self);
}

pub struct SectionView<T: Layout> {
    mapping: HANDLE,
    view: *mut c_void,
    pub created: bool,
    layout: PhantomData<T>,
}

unsafe impl<T: Layout> Send for SectionView<T> {}
unsafe impl<T: Layout> Sync for SectionView<T> {}

impl<T: Layout> SectionView<T> {
    pub fn create() -> Result<Self, u32> {
        let name = wide(T::NAME);
        let mapping = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE,
                core::ptr::null(),
                PAGE_READWRITE,
                0,
                T::SIZE as u32,
                name.as_ptr(),
            )
        };
        if mapping.is_null() {
            return Err(unsafe { GetLastError() });
        }
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        Self::map(mapping, !existed)
    }

    pub fn open() -> Result<Self, u32> {
        let name = wide(T::NAME);
        let mapping = unsafe { OpenFileMappingW(FILE_MAP_READ | FILE_MAP_WRITE, 0, name.as_ptr()) };
        if mapping.is_null() {
            return Err(unsafe { GetLastError() });
        }
        Self::map(mapping, false)
    }

    fn map(mapping: HANDLE, created: bool) -> Result<Self, u32> {
        let MEMORY_MAPPED_VIEW_ADDRESS { Value: view } =
            unsafe { MapViewOfFile(mapping, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, T::SIZE) };
        if view.is_null() {
            let error = unsafe { GetLastError() };
            unsafe { CloseHandle(mapping) };
            return Err(error);
        }
        let section = Self {
            mapping,
            view,
            created,
            layout: PhantomData,
        };
        if created {
            section.shared().initialize();
        }
        Ok(section)
    }

    pub fn shared(&self) -> &T {
        unsafe { &*(self.view as *const T) }
    }
}

impl<T: Layout> Drop for SectionView<T> {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.view });
            CloseHandle(self.mapping);
        }
    }
}

pub struct Signal(HANDLE);

unsafe impl Send for Signal {}
unsafe impl Sync for Signal {}

impl Signal {
    pub fn open_or_create(name: &str) -> Result<Self, u32> {
        let name = wide(name);
        let handle = unsafe { CreateEventW(core::ptr::null(), 0, 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(unsafe { GetLastError() });
        }
        Ok(Self(handle))
    }

    pub fn raise(&self) {
        unsafe { SetEvent(self.0) };
    }

    pub fn wait(&self, timeout_ms: u32) -> bool {
        unsafe { WaitForSingleObject(self.0, timeout_ms) == WAIT_OBJECT_0 }
    }

    pub fn wait_or_message(&self, timeout_ms: u32) -> bool {
        let woke = unsafe {
            MsgWaitForMultipleObjectsEx(1, &self.0, timeout_ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        woke == WAIT_OBJECT_0
    }
}

pub fn wait_for_message(timeout_ms: u32) {
    unsafe {
        MsgWaitForMultipleObjectsEx(
            0,
            core::ptr::null(),
            timeout_ms,
            QS_ALLINPUT,
            MWMO_INPUTAVAILABLE,
        )
    };
}

impl Drop for Signal {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
