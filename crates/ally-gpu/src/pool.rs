// Ported from fxceless/victory-fork commit `2eade68` ("The card stays under
// its memory budget: no new texture block past the texture line"), a fork of
// VIctory (https://github.com/Marvisak/VIctory). GPL-3.0-or-later, by
// Marvisak, fxceless and contributors. Adapted for Dekubi X (`DEKUBI_POOL_GATE`,
// `DEKUBI_POOL_FULLEST`).

//! Texture budget gate and block-placement state (`DEKUBI_POOL_GATE`,
//! `DEKUBI_POOL_FULLEST`).
//!
//! Keeps the host GPU strictly under its VRAM budget: a new texture block (or
//! committed texture allocation when running without 64 MiB placed-texture
//! heaps) is admitted only while `used + kept + 64 MiB <= texture_line`
//! (`crate::resource::block_room`). Textures that can wait (background bindless
//! heap pass) stay due and retry later without poisoning `refused`; textures
//! that cannot wait (direct table reads or bakes) bypass the gate.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Whether a new block/allocation of textures is gated to stay under the
/// texture line (on by default; `DEKUBI_POOL_GATE=0`: off, as before).
pub(crate) fn gate() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DEKUBI_POOL_GATE").is_none_or(|v| v != "0"))
}

/// Whether a piece is cut from the fullest block that has room for it (on by
/// default; `DEKUBI_POOL_FULLEST=0`: first-fit, as before).
#[allow(dead_code)]
pub(crate) fn fullest() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DEKUBI_POOL_FULLEST").is_none_or(|v| v != "0"))
}

/// Picks the index of the block to place a piece of `need` bytes into from
/// `(free_bytes, largest_contiguous_run)` candidates: fullest fitting block
/// when `use_fullest` is true, else first fitting block.
#[allow(dead_code)]
pub(crate) fn pick_block(blocks: &[(u64, u64)], need: u64, use_fullest: bool) -> Option<usize> {
    let fitting = blocks
        .iter()
        .enumerate()
        .filter(|&(_, &(_, run))| run >= need);
    if use_fullest {
        fitting.min_by_key(|&(_, &(free, _))| free).map(|(i, _)| i)
    } else {
        fitting.map(|(i, _)| i).next()
    }
}

/// What had no place: `known` if a piece of `len` bytes at `align` would
/// have none where that had none, else this one.
#[allow(dead_code)]
fn smaller(known: Option<(u64, u64)>, len: u64, align: u64) -> (u64, u64) {
    match known {
        Some((l, a)) if l <= len && a <= align => (l, a),
        _ => (len, align),
    }
}

/// Since the start: blocks/textures admitted under the gate (`MADE`), times
/// an allocation was deferred for want of room (`NOT_MADE`), and allocations
/// made past the line for textures that could not wait (`PAST`).
pub(crate) static BLOCKS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
pub(crate) const MADE: usize = 0;
pub(crate) const NOT_MADE: usize = 1;
pub(crate) const PAST: usize = 2;

/// Whether the card had no room for a new block when last asked
/// (`crate::resource::block_room`).
static FULL: AtomicBool = AtomicBool::new(false);

/// See [`FULL`].
pub(crate) fn full() -> bool {
    gate() && FULL.load(Ordering::Relaxed)
}

/// `crate::resource::block_room`'s answer, for [`full`].
pub(crate) fn note_full(full: bool) {
    FULL.store(full, Ordering::Relaxed);
}

/// Whether the card has room for a new texture block/allocation
/// (`crate::resource::block_room`), cached per 100 ms tenth.
pub(crate) fn may_grow() -> bool {
    if !gate() {
        return true;
    }
    static LAST: AtomicU64 = AtomicU64::new(0);
    let tenth = crate::host::now_ms() / 100 + 1;
    let last = LAST.load(Ordering::Relaxed);
    if last >> 1 == tenth {
        return last & 1 != 0;
    }
    let room = crate::resource::block_room(crate::host::host().and_then(|h| h.video_memory()));
    LAST.store((tenth << 1) | u64::from(room), Ordering::Relaxed);
    room
}

thread_local! {
    /// The texture being made on this thread can wait for a place
    /// ([`waiting`]), and the last one asked for did ([`waits`]).
    static CAN_WAIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static WAITED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `make`, which may make a host texture that can wait: if the card has
/// no room under [`gate`], it is not made, and the second value returned is
/// `true`. Only the pass over textures picked by index asks this way so it
/// can leave the slot due.
pub(crate) fn waiting<T>(make: impl FnOnce() -> T) -> (T, bool) {
    let was = CAN_WAIT.replace(true);
    WAITED.set(false);
    let made = make();
    CAN_WAIT.set(was);
    (made, WAITED.replace(false))
}

/// Whether the current thread is inside [`waiting`].
pub(crate) fn can_wait() -> bool {
    CAN_WAIT.get()
}

/// Records that the current thread's [`waiting`] allocation was deferred.
pub(crate) fn note_waited() {
    WAITED.set(true);
    BLOCKS[NOT_MADE].fetch_add(1, Ordering::Relaxed);
}

/// Within [`waiting`]: whether the texture this thread just asked for was
/// deferred for want of room. (False outside it.)
pub(crate) fn waits() -> bool {
    WAITED.get()
}

/// Free bytes inside texture blocks (0 when using committed textures directly,
/// where freed textures return immediately to D3D12).
pub(crate) fn texture_room() -> u64 {
    0
}

/// Block fill census for the `"texture blocks: ..."` telemetry line:
/// `[blocks, free, q0, h0, q1, h1, q2, h2, q3, h3, given]`.
pub(crate) fn fill() -> [u64; 11] {
    [0; 11]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_tracks_deferred_allocations_and_resets() {
        assert!(!can_wait());
        assert!(!waits());
        let (res, deferred) = waiting(|| {
            assert!(can_wait());
            note_waited();
            assert!(waits());
            None::<u32>
        });
        assert_eq!(res, None);
        assert!(deferred);
        assert!(!can_wait());
        assert!(!waits());
    }

    #[test]
    fn fullest_picks_fullest_fitting_block_while_first_fit_picks_first() {
        // (free_bytes, largest_contiguous_run)
        let blocks = [
            (48 << 20, 32 << 20),
            (16 << 20, 12 << 20),
            (8 << 20, 8 << 20),
        ];
        // Need 10 MiB: blocks 0 and 1 fit; block 1 is fuller (16 MiB free < 48 MiB free).
        assert_eq!(pick_block(&blocks, 10 << 20, false), Some(0));
        assert_eq!(pick_block(&blocks, 10 << 20, true), Some(1));
        // Need 6 MiB: all three fit; block 2 is fullest (8 MiB free).
        assert_eq!(pick_block(&blocks, 6 << 20, false), Some(0));
        assert_eq!(pick_block(&blocks, 6 << 20, true), Some(2));
        // Need 40 MiB: none fits.
        assert_eq!(pick_block(&blocks, 40 << 20, true), None);
    }

    #[test]
    fn smaller_keeps_tightest_unplaceable_shape() {
        assert_eq!(smaller(None, 8 << 20, 65536), (8 << 20, 65536));
        assert_eq!(
            smaller(Some((8 << 20, 65536)), 16 << 20, 65536),
            (8 << 20, 65536)
        );
        assert_eq!(
            smaller(Some((8 << 20, 65536)), 4 << 20, 65536),
            (4 << 20, 65536)
        );
    }
}
