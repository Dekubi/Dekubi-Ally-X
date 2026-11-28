//! One Dekubi X game at a time. Two copies of the title share the card and
//! its 8 GB: on 2026-11-25 a second launch eight seconds after the first
//! halved the first one's frame rate and then failed for want of memory.
//!
//! [`claim`] takes a named mutex for the life of the process. A second
//! launch finds it taken, brings the running game's window to the front
//! and says so, and does not start. `DEKUBI_ALLOW_SECOND=1` skips the check
//! (two copies on purpose, e.g. comparing settings).

/// The mutex's name: per Windows session, shared by every build and both
/// file names (`dekubi-x.exe`, `Dekubi X.exe`).
#[cfg(windows)]
const NAME: &str = "Local\\DekubiX.Game";

/// Whether this process may run the game: `true` when no other Dekubi X
/// game is running (or the check is turned off or cannot be made).
#[cfg(windows)]
pub fn claim() -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    if std::env::var_os("DEKUBI_ALLOW_SECOND").is_some_and(|v| v == "1") {
        tracing::info!("second-copy check off (DEKUBI_ALLOW_SECOND=1)");
        return true;
    }
    let name: Vec<u16> = NAME.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: a NUL-terminated name; the handle is kept open for the life
    // of the process (the mutex lasts as long as one handle does).
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: read right after the call it reports on.
    let error = unsafe { GetLastError() };
    if handle.is_null() {
        tracing::warn!(error, "second-copy check not made: no mutex");
        return true;
    }
    if error != ERROR_ALREADY_EXISTS {
        return true;
    }
    tracing::warn!("Dekubi X is already running: this launch does not start the game");
    bring_other_forward();
    false
}

#[cfg(not(windows))]
pub fn claim() -> bool {
    true
}

/// Brings the first visible window of another Dekubi X process to the
/// front, if there is one.
#[cfg(windows)]
fn bring_other_forward() {
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GWL_EXSTYLE, GetWindowLongW, GetWindowThreadProcessId, IsIconic,
        IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow, WS_EX_TOOLWINDOW,
    };

    fn is_dekubi(pid: u32) -> bool {
        // SAFETY: a query-only handle, closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return false;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: a live handle and a buffer of `len` characters.
        let ok = unsafe {
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
        };
        // SAFETY: opened above.
        unsafe { CloseHandle(process) };
        if ok == 0 {
            return false;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase();
        let file = path.rsplit(['\\', '/']).next().unwrap_or("");
        file.starts_with("dekubi")
    }

    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> windows_sys::core::BOOL {
        let mut pid = 0u32;
        // SAFETY: a window handle EnumWindows gave.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        // SAFETY: as above.
        let shown = unsafe { IsWindowVisible(hwnd) } != 0
            && unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOOLWINDOW == 0;
        // SAFETY: no preconditions.
        if shown && pid != unsafe { GetCurrentProcessId() } && is_dekubi(pid) {
            // SAFETY: `found` points at the caller's HWND slot.
            unsafe { *(found as *mut HWND) = hwnd };
            return 0;
        }
        1
    }

    let mut found: HWND = std::ptr::null_mut();
    // SAFETY: the callback writes only into `found`, which outlives the call.
    unsafe { EnumWindows(Some(each), &mut found as *mut HWND as LPARAM) };
    if !found.is_null() {
        // SAFETY: a live top-level window.
        unsafe {
            if IsIconic(found) != 0 {
                ShowWindow(found, SW_RESTORE);
            }
            SetForegroundWindow(found);
        }
    }
}
