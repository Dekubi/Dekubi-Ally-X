// Dekubi X, GPL-3.0-or-later. After Dekubi Ally (github.com/Dekubi/Dekubi-Ally,
// Fritchii) perf_overlay and dekubi_summary, by Dekubi.
//! Frame numbers for the performance overlay and the run summary (after
//! Dekubi Ally's `perf_overlay` and `dekubi_summary`): written once a
//! second by the frame meter, read by anyone.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

static FPS_X10: AtomicU64 = AtomicU64::new(0);
static MS_X10: AtomicU64 = AtomicU64::new(0);
static DRAWS: AtomicU64 = AtomicU64::new(0);
static VRAM_USED_MB: AtomicU64 = AtomicU64::new(0);
static VRAM_BUDGET_MB: AtomicU64 = AtomicU64::new(0);
static SECONDS: AtomicU64 = AtomicU64::new(0);
static FPS_SUM_X10: AtomicU64 = AtomicU64::new(0);
static FPS_MIN_X10: AtomicU64 = AtomicU64::new(u64::MAX);
static VRAM_PEAK_MB: AtomicU64 = AtomicU64::new(0);

/// One second's numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub fps: f64,
    pub ms: f64,
    pub draws: u64,
    pub vram_used_mb: u64,
    pub vram_budget_mb: u64,
}

/// The whole run so far.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    pub seconds: u64,
    pub average_fps: f64,
    pub lowest_fps: f64,
    pub vram_peak_mb: u64,
}

pub(crate) fn record(fps: f64, ms: f64, draws: f64, video: Option<[u64; 4]>) {
    let fps10 = (fps * 10.0).round() as u64;
    FPS_X10.store(fps10, Relaxed);
    MS_X10.store((ms * 10.0).round() as u64, Relaxed);
    DRAWS.store(draws.round() as u64, Relaxed);
    if let Some([budget, used, ..]) = video {
        VRAM_BUDGET_MB.store(budget >> 20, Relaxed);
        VRAM_USED_MB.store(used >> 20, Relaxed);
        VRAM_PEAK_MB.fetch_max(used >> 20, Relaxed);
    }
    SECONDS.fetch_add(1, Relaxed);
    FPS_SUM_X10.fetch_add(fps10, Relaxed);
    FPS_MIN_X10.fetch_min(fps10, Relaxed);
}

/// The last second's numbers; `None` before the first frame second.
pub fn snapshot() -> Option<Snapshot> {
    (SECONDS.load(Relaxed) > 0).then(|| Snapshot {
        fps: FPS_X10.load(Relaxed) as f64 / 10.0,
        ms: MS_X10.load(Relaxed) as f64 / 10.0,
        draws: DRAWS.load(Relaxed),
        vram_used_mb: VRAM_USED_MB.load(Relaxed),
        vram_budget_mb: VRAM_BUDGET_MB.load(Relaxed),
    })
}

/// The run's totals; `None` before the first frame second.
pub fn totals() -> Option<Totals> {
    let seconds = SECONDS.load(Relaxed);
    (seconds > 0).then(|| Totals {
        seconds,
        average_fps: FPS_SUM_X10.load(Relaxed) as f64 / 10.0 / seconds as f64,
        lowest_fps: FPS_MIN_X10.load(Relaxed) as f64 / 10.0,
        vram_peak_mb: VRAM_PEAK_MB.load(Relaxed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_follow_the_seconds_recorded() {
        record(30.0, 33.3, 21.0, Some([8 << 30, 5 << 30, 0, 0]));
        record(10.0, 100.0, 40.0, None);
        let t = totals().unwrap();
        assert!(t.seconds >= 2);
        assert!(t.lowest_fps <= 10.0);
        assert!(t.vram_peak_mb >= 5 * 1024);
        assert!(snapshot().is_some());
    }
}
