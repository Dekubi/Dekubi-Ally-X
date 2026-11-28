// Dekubi X, GPL-3.0-or-later. After Dekubi Ally (github.com/Dekubi/Dekubi-Ally,
// Fritchii): src/yuzu/dekubi/perf_overlay.cpp and the run summary, by Dekubi.
//! While a title runs: the performance overlay (a small click-through
//! window over the title's top-left corner: FPS, frame time, draws, video
//! memory, Dekubi X's RAM; F10 shows or hides it; `DEKUBI_OVERLAY=0` turns
//! it off) and the run summary (`<logs>/last-summary.txt`, rewritten every
//! 10 s, so it is there however the title ends).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Starts the summary writer and the overlay for a run of `exe`.
pub fn start(exe: &Path, log_dir: Option<PathBuf>) {
    let exe = exe.to_path_buf();
    let started = SystemTime::now();
    if let Some(dir) = log_dir {
        let summary = dir.join("last-summary.txt");
        let _ = std::thread::Builder::new()
            .name("dekubi-summary".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(10));
                    let _ = std::fs::write(&summary, summary_text(&exe, started));
                }
            });
    }
    #[cfg(windows)]
    if std::env::var("DEKUBI_OVERLAY").map_or(true, |v| v.trim() != "0") {
        overlay::start();
    }
}

fn summary_text(exe: &Path, started: SystemTime) -> String {
    let secs = started
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut out = format!(
        "Dekubi X {}\ngame        : {}\nstarted     : {} (UTC seconds)\ndisplay mode: {}\n",
        ally_core::VERSION,
        exe.display(),
        secs,
        std::env::var("DEKUBI_DISPLAY_MODE").unwrap_or_else(|_| "3840x2160".into()),
    );
    match ally_gpu::stats::totals() {
        Some(t) => out.push_str(&format!(
            "frames for : {} s\naverage fps : {:.1}\nlowest fps  : {:.1}\nVRAM peak   : {} MB\n",
            t.seconds, t.average_fps, t.lowest_fps, t.vram_peak_mb
        )),
        None => out.push_str("no frame yet\n"),
    }
    if let Some(s) = ally_gpu::stats::snapshot() {
        out.push_str(&format!(
            "last second : {:.1} fps, {:.1} ms, {} draws, VRAM {} / {} MB\n",
            s.fps, s.ms, s.draws, s.vram_used_mb, s.vram_budget_mb
        ));
    }
    out
}

/// The overlay's text for one second's numbers.
fn overlay_text(s: Option<ally_gpu::stats::Snapshot>, ram_mb: u64) -> String {
    match s {
        Some(s) => format!(
            "Dekubi X {}\n{:.1} FPS  {:.1} ms\n{} draws\nVRAM {} / {} MB\nRAM {} MB\nF10 hides",
            ally_core::VERSION,
            s.fps,
            s.ms,
            s.draws,
            s.vram_used_mb,
            s.vram_budget_mb,
            ram_mb
        ),
        None => format!(
            "Dekubi X {}\nloading...\nRAM {} MB\nF10 hides",
            ally_core::VERSION,
            ram_mb
        ),
    }
}

#[cfg(windows)]
mod overlay {
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontW, CreateSolidBrush, DT_LEFT, DT_NOPREFIX, DeleteObject, DrawTextW,
        EndPaint, FillRect, InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor,
        TRANSPARENT,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, EnumWindows, GetMessageW, GetWindowRect,
        GetWindowThreadProcessId, HWND_TOPMOST, IsIconic, IsWindowVisible, LWA_ALPHA, MSG,
        RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE,
        SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow, TranslateMessage, WM_PAINT,
        WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_EX_TRANSPARENT, WS_POPUP,
    };
    use windows_sys::core::BOOL;

    const WIDTH: i32 = 230;
    const HEIGHT: i32 = 128;
    const VK_F10: i32 = 0x79;

    static SHOWN: AtomicBool = AtomicBool::new(true);
    static OVERLAY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) fn start() {
        let _ = std::thread::Builder::new()
            .name("dekubi-overlay".into())
            .spawn(run);
    }

    fn ram_mb() -> u64 {
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        // SAFETY: our own process and a sized structure.
        unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
        counters.WorkingSetSize as u64 >> 20
    }

    /// The largest visible top-level window of this process, not the overlay.
    fn title_window() -> Option<RECT> {
        struct Found(Option<(RECT, i64)>);
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            // SAFETY: `data` is the `Found` passed below; plain window queries.
            unsafe {
                let found = &mut *(data as *mut Found);
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, &mut pid);
                if pid != GetCurrentProcessId()
                    || hwnd as usize == OVERLAY.load(Ordering::Relaxed)
                    || IsWindowVisible(hwnd) == 0
                    || IsIconic(hwnd) != 0
                {
                    return 1;
                }
                let mut r = RECT::default();
                if GetWindowRect(hwnd, &mut r) != 0 {
                    let area = i64::from(r.right - r.left) * i64::from(r.bottom - r.top);
                    if found.0.is_none_or(|(_, a)| area > a) {
                        found.0 = Some((r, area));
                    }
                }
                1
            }
        }
        let mut found = Found(None);
        // SAFETY: the callback reads `found` only during the call.
        unsafe { EnumWindows(Some(each), &raw mut found as LPARAM) };
        found.0.map(|(r, _)| r)
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        // SAFETY: window procedure calls on our own window.
        unsafe {
            match msg {
                WM_TIMER => {
                    let f10 = GetAsyncKeyState(VK_F10) as u16 & 1 != 0;
                    if f10 {
                        SHOWN.fetch_xor(true, Ordering::Relaxed);
                    }
                    match title_window().filter(|_| SHOWN.load(Ordering::Relaxed)) {
                        Some(r) => {
                            SetWindowPos(
                                hwnd,
                                HWND_TOPMOST,
                                r.left + 12,
                                r.top + 40,
                                0,
                                0,
                                SWP_NOSIZE | SWP_NOACTIVATE,
                            );
                            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                            InvalidateRect(hwnd, std::ptr::null(), 1);
                        }
                        None => {
                            ShowWindow(hwnd, SW_HIDE);
                        }
                    }
                    0
                }
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    let dc = BeginPaint(hwnd, &mut ps);
                    let mut r = RECT {
                        left: 0,
                        top: 0,
                        right: WIDTH,
                        bottom: HEIGHT,
                    };
                    let brush = CreateSolidBrush(0x0016_1210 as COLORREF);
                    FillRect(dc, &r, brush);
                    DeleteObject(brush);
                    let face = wide("Consolas");
                    let font = CreateFontW(17, 0, 0, 0, 600, 0, 0, 0, 0, 0, 0, 0, 0, face.as_ptr());
                    let old = SelectObject(dc, font);
                    SetBkMode(dc, TRANSPARENT as _);
                    SetTextColor(dc, 0x005E_C522 as COLORREF);
                    r.left += 10;
                    r.top += 6;
                    let text: Vec<u16> = super::overlay_text(ally_gpu::stats::snapshot(), ram_mb())
                        .encode_utf16()
                        .collect();
                    DrawTextW(
                        dc,
                        text.as_ptr(),
                        text.len() as i32,
                        &mut r,
                        DT_LEFT | DT_NOPREFIX,
                    );
                    SelectObject(dc, old);
                    DeleteObject(font);
                    EndPaint(hwnd, &ps);
                    0
                }
                _ => DefWindowProcW(hwnd, msg, w, l),
            }
        }
    }

    fn run() {
        let class = wide("DekubiXOverlay");
        let title = wide("Dekubi X overlay");
        // SAFETY: a window class and window of our own, used on this thread.
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                WIDTH,
                HEIGHT,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                tracing::warn!("performance overlay not made");
                return;
            }
            OVERLAY.store(hwnd as usize, Ordering::Relaxed);
            SetLayeredWindowAttributes(hwnd, 0, 210, LWA_ALPHA);
            SetTimer(hwnd, 1, 500, None);
            tracing::info!("performance overlay on (F10 shows or hides it)");
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_text_names_the_numbers() {
        let t = overlay_text(
            Some(ally_gpu::stats::Snapshot {
                fps: 29.9,
                ms: 33.4,
                draws: 21,
                vram_used_mb: 5000,
                vram_budget_mb: 7600,
            }),
            3000,
        );
        assert!(
            t.contains("29.9 FPS")
                && t.contains("VRAM 5000 / 7600 MB")
                && t.contains("RAM 3000 MB")
        );
        assert!(overlay_text(None, 1).contains("loading"));
    }
}
