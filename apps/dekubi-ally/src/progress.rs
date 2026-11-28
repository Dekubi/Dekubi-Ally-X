//! A small window while the shaders are prepared before the game starts.
//! After a translator change that takes about two minutes on the owner's
//! PC (115 s for 0.1.9), and with nothing on screen a double-click looked
//! like it did nothing (2026-11-25). The window shows how far it is and
//! goes away when the game starts. `DEKUBI_PROGRESS_WINDOW=0` leaves it out.

/// The window, open until this is dropped.
pub struct Progress {
    #[cfg(windows)]
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(windows)]
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Progress {
    /// Opens the window on a thread of its own; `text` is asked for what
    /// it says about four times a second.
    pub fn show(text: impl Fn() -> String + Send + 'static) -> Self {
        #[cfg(windows)]
        {
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let on = std::env::var_os("DEKUBI_PROGRESS_WINDOW").is_none_or(|v| v != "0");
            let thread = on.then(|| {
                let stop = stop.clone();
                std::thread::Builder::new()
                    .name("dekubi-progress".into())
                    .spawn(move || window(&stop, &text))
                    .ok()
            });
            Self {
                stop,
                thread: thread.flatten(),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = text;
            Self {}
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn window(stop: &std::sync::atomic::AtomicBool, text: &dyn Fn() -> String) {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::UpdateWindow;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics,
        IDC_ARROW, LoadCursorW, MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SM_CXSCREEN,
        SM_CYSCREEN, SW_SHOWNOACTIVATE, SetWindowTextW, ShowWindow, TranslateMessage, WM_CLOSE,
        WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };

    // Closing it would only hide how far the preparing is; it closes
    // itself when the game starts.
    unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_CLOSE {
            return 0;
        }
        // SAFETY: the arguments the system passed.
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }

    let class = wide("DekubiXProgress");
    let title = wide("Dekubi X");
    let static_class = wide("STATIC");
    // SAFETY: plain Win32 calls on this thread; every string is a live
    // NUL-terminated buffer for the length of the call.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc_),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            // COLOR_WINDOW + 1
            hbrBackground: 6 as _,
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let (w, h) = (480, 130);
        let (x, y) = (
            (GetSystemMetrics(SM_CXSCREEN) - w) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - h) / 2,
        );
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_CAPTION,
            x,
            y,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }
        let first = wide(&text());
        let label = CreateWindowExW(
            0,
            static_class.as_ptr(),
            first.as_ptr(),
            // SS_CENTER
            WS_CHILD | WS_VISIBLE | 1,
            12,
            14,
            w - 30,
            h - 50,
            hwnd,
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        UpdateWindow(hwnd);
        let mut shown = String::new();
        let mut msg: MSG = std::mem::zeroed();
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let now = text();
            if now != shown && !label.is_null() {
                let buf = wide(&now);
                SetWindowTextW(label, buf.as_ptr());
                shown = now;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        DestroyWindow(hwnd);
    }
}
