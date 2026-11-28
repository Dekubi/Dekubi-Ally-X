//! Low-commit upload defer and `E_OUTOFMEMORY` allocation backoff.
//!
//! Ported from `cool_fox` commit `ab75ac3` (GPL-3.0-or-later, by codec_d and
//! contributors). Leaves commit headroom for live game allocations and
//! in-flight submissions on 8 GB VRAM / 32 GB RAM PCs, and backs off
//! speculative texture uploads for 500 ms after an `E_OUTOFMEMORY` failure.

use std::sync::atomic::{AtomicU64, Ordering};

const GIB: u64 = 1024 * 1024 * 1024;

// The texture sweep may run for seconds inside a single submitted frame.
static NEXT_COMMIT_SAMPLE: AtomicU64 = AtomicU64::new(0);
static AVAILABLE_COMMIT: AtomicU64 = AtomicU64::new(u64::MAX);
static RETRY_UPLOAD_AT: AtomicU64 = AtomicU64::new(0);

fn avail_commit() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: sized MEMORYSTATUSEX struct.
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullAvailPageFile)
}

/// Records a host D3D12 allocation failure; on `E_OUTOFMEMORY`, backs off
/// speculative texture uploads for 500 ms to avoid OOM thrashing storms.
pub(crate) fn allocation_error(error: &windows::core::Error) {
    if error.code() == windows::Win32::Foundation::E_OUTOFMEMORY {
        RETRY_UPLOAD_AT.fetch_max(crate::host::now_ms().saturating_add(500), Ordering::Relaxed);
    }
}

/// Whether speculative uploads are currently in backoff after `E_OUTOFMEMORY`.
pub(crate) fn upload_refused() -> bool {
    crate::host::now_ms() < RETRY_UPLOAD_AT.load(Ordering::Relaxed)
}

fn has_upload_reserve(commit: u64, bake: bool) -> bool {
    commit >= if bake { 2 * GIB } else { 4 * GIB }
}

/// Whether available Windows commit is below 4 GiB (used by idle texture
/// sweeps to bypass per-tick eviction caps before the process hits OOM).
pub(crate) fn low_commit() -> bool {
    avail_commit().is_some_and(|commit| commit < 4 * GIB)
}

/// Leave commit for game allocations and submissions already in flight.
/// Direct texture consumers bypass this gate; bake prefetch gets priority
/// over background lookahead.
pub(crate) fn admit_texture_pass(bake: bool) -> bool {
    let now = crate::host::now_ms();
    if now < RETRY_UPLOAD_AT.load(Ordering::Relaxed) {
        return false;
    }
    let next = NEXT_COMMIT_SAMPLE.load(Ordering::Relaxed);
    if now >= next
        && NEXT_COMMIT_SAMPLE
            .compare_exchange(
                next,
                now.saturating_add(32),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
        && let Some(commit) = avail_commit()
    {
        AVAILABLE_COMMIT.store(commit, Ordering::Relaxed);
    }
    has_upload_reserve(AVAILABLE_COMMIT.load(Ordering::Relaxed), bake)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bake_and_lookahead_leave_room_for_live_game_allocations() {
        for (commit, bake, lookahead) in [
            (277 << 20, false, false),
            (2 * GIB, true, false),
            (4 * GIB, true, true),
        ] {
            assert_eq!(has_upload_reserve(commit, true), bake);
            assert_eq!(has_upload_reserve(commit, false), lookahead);
        }
    }
}
