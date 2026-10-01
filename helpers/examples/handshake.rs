// ------------ Stub Handshake Check ------------
// A manual check that the FPS stub DLL works. Plays the helper in its own process: creates the shared section,
// loads peebify_helpers.dll and waits for the stub to run and report that this is not a game. Pass the DLL path.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use peebify_helpers::common::wide;
use peebify_helpers::fps::section::SectionView;
use peebify_helpers::fps::shared::{STATUS_FAILED, STATUS_IDLE};

fn main() {
    let dll = std::env::args()
        .nth(1)
        .expect("pass the path to peebify_helpers.dll");

    let section = SectionView::create().expect("create the shared section");
    assert!(
        section.created,
        "another session is already using the section"
    );
    let shared = section.shared();
    shared.initialize();
    shared.target_fps.store(144, Ordering::Relaxed);

    let me = std::process::id();
    shared.game_pid.store(me, Ordering::Release);
    println!("section ready, claiming pid {me}");

    let path = wide(&dll);
    let module = unsafe { windows_sys::Win32::System::LibraryLoader::LoadLibraryW(path.as_ptr()) };
    assert!(
        !module.is_null(),
        "LoadLibraryW failed: {}",
        std::io::Error::last_os_error()
    );

    let hook = unsafe {
        windows_sys::Win32::System::LibraryLoader::GetProcAddress(
            module,
            c"WndProc".as_ptr().cast(),
        )
    };
    assert!(hook.is_some(), "the DLL does not export WndProc");
    println!("loaded the stub and found its WndProc export");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = shared.status.load(Ordering::Acquire);
        if status != STATUS_IDLE {
            println!("status = {status}");
            if status == STATUS_FAILED {
                println!("reported: {}", shared.error_message());
                println!("\nOK: the stub ran, recognised this process, and failed cleanly.");
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the stub never reported anything (status {status})"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
