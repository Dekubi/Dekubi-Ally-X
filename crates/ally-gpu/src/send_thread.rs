// Ported from Kemerd/VIctory-NVIDIA-RT (`crates/victory-gpu/src/host/send_thread.rs`),
// a fork of VIctory (https://github.com/Marvisak/VIctory). GPL-3.0-or-later,
// by Kemerd, Marvisak and contributors. Adapted for Dekubi X (`DEKUBI_SEND_THREAD`).

//! `DEKUBI_SEND_THREAD=1` (off by default). A submission sent ahead
//! (`Host::submit` with `late`) used to end on the submitting thread with
//! `ExecuteCommandLists` of its lists, `Signal` of its fence value, and
//! `Host::card_sent`. With the switch, the submitting thread hands that tail
//! to one dedicated thread (`dekubi-send`) and goes on to prepare the next
//! submission. The inline path calls the same [`Job::run`], so the old
//! behaviour is this exact code.
//!
//! Order rules:
//! 1. Nothing about the job reads title memory; the lists are closed.
//! 2. The fence value is reserved by the submitting thread (`*submitted` was
//!    already incremented) and the `Flight` goes into `line()` from the
//!    submitting thread before the next submission looks at it.
//! 3. Everything that puts lists on the host queue from the submitting thread
//!    afterwards must come after this thread's jobs: `Host::card_before` and
//!    `Host::present` call [`wait_idle`] before executing sync lists or
//!    presenting.
//! 4. The thread always signals: every job's fence value is signalled even
//!    if a panic occurred, so no waiter can hang forever.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Instant;

use super::*;

/// Whether submissions sent ahead are sent by [`hand_over`]'s thread
/// (`DEKUBI_SEND_THREAD=1`, only with `late()`). Off by default. Read once.
pub(super) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| late() && std::env::var("DEKUBI_SEND_THREAD").as_deref() == Ok("1"))
}

/// The tail of a submission sent ahead: its closed lists and the fence value
/// to signal behind them.
pub(super) struct Job {
    /// The lists in the order they go to the queue.
    lists: Vec<Option<ID3D12CommandList>>,
    /// The value signalled behind the lists.
    fence: u64,
    /// The card plan's number of the submission, 0 when it has none; only
    /// used to check the order the jobs come in.
    sent_as: u64,
    /// Set once the fence value was signalled, so a panic after that does
    /// not signal it a second time.
    signalled: bool,
}

// SAFETY: closed lists of free-threaded D3D12 objects; nothing records into them again.
unsafe impl Send for Job {}

impl Job {
    /// A job for `lists`, to be followed by `fence`.
    pub(super) fn new(lists: Vec<Option<ID3D12CommandList>>, fence: u64, sent_as: u64) -> Job {
        Job {
            lists,
            fence,
            sent_as,
            signalled: false,
        }
    }

    /// Sends the lists (respecting `DEKUBI_MAX_SUBMIT_LISTS` anti-TDR burst
    /// splitting), signals the fence, and notes `card_sent`.
    pub(super) fn run(&mut self, host: &Host) -> windows::core::Result<()> {
        let chunks = submit_chunks(&self.lists, max_submit_lists());
        // SAFETY: closed lists; the fence is ours.
        let signal = unsafe {
            for chunk in chunks {
                host.queue.ExecuteCommandLists(chunk);
            }
            host.queue.Signal(&host.fence, self.fence)
        };
        self.signalled = true;
        host.card_sent();
        signal
    }
}

/// What the thread and its waiters share.
struct State {
    /// Jobs not taken by the thread yet, in the order they were handed over.
    jobs: VecDeque<Job>,
    /// Jobs handed over so far.
    handed: u64,
    /// Jobs the thread is finished with (sent, or failed and noted).
    done: u64,
}

static STATE: Mutex<State> = Mutex::new(State {
    jobs: VecDeque::new(),
    handed: 0,
    done: 0,
});
/// The thread waits here for a job.
static WORK: Condvar = Condvar::new();
/// Threads in [`wait_idle`] wait here for `done` to reach `handed`.
static IDLE: Condvar = Condvar::new();

/// Jobs, nanoseconds the thread was busy in `run`, waits of the submitting
/// thread that found it busy, nanoseconds of those waits, order violations.
static STATS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// Queues `job` for the thread, starting it on the first call. `Err(job)`
/// gives the job back when the thread could not be made (said once, never
/// tried again): the caller sends it itself.
pub(super) fn hand_over(job: Job) -> Result<(), Job> {
    static STARTED: OnceLock<bool> = OnceLock::new();
    let started = *STARTED.get_or_init(|| {
        match std::thread::Builder::new()
            .name("dekubi-send".into())
            .spawn(worker)
        {
            Ok(_) => {
                tracing::info!(
                    "submissions sent ahead are sent by a thread of their own (DEKUBI_SEND_THREAD=1)"
                );
                true
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "no thread to send submissions with: they are sent by the submitting thread (DEKUBI_SEND_THREAD=1)"
                );
                false
            }
        }
    });
    if !started {
        return Err(job);
    }
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.jobs.push_back(job);
    state.handed += 1;
    drop(state);
    WORK.notify_one();
    Ok(())
}

/// Returns once the thread has sent everything handed over so far; at once
/// when nothing is outstanding (always, with the switch off). Called by the
/// submitting thread before it puts a list of its own on the queue or
/// presents, so that work comes after the queued jobs.
pub(super) fn wait_idle() {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if state.handed == state.done {
        return;
    }
    let began = Instant::now();
    while state.handed != state.done {
        state = IDLE.wait(state).unwrap_or_else(|e| e.into_inner());
    }
    drop(state);
    STATS[2].fetch_add(1, Ordering::Relaxed);
    STATS[3].fetch_add(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
}

/// Whether a job comes in the order it was reserved in: its fence value is
/// above the last job's, and a card plan number (non-zero) is not below the
/// largest seen. `last` holds `(last fence, largest plan number)` and moves
/// on to this job whatever the answer.
fn in_order(last: &mut (u64, u64), fence: u64, sent_as: u64) -> bool {
    let ok = fence > last.0 && (sent_as == 0 || sent_as >= last.1);
    last.0 = fence;
    last.1 = last.1.max(sent_as);
    ok
}

/// The thread: takes jobs in the order they were handed over and runs them.
/// It does nothing else, and takes none of the host's other locks.
fn worker() {
    let Some(host) = host() else {
        fail(format_args!(
            "no host device for the thread that sends submissions (DEKUBI_SEND_THREAD=1)"
        ));
    };
    let mut last = (0u64, 0u64);
    loop {
        let mut job = {
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(job) = state.jobs.pop_front() {
                    break job;
                }
                state = WORK.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };
        if !in_order(&mut last, job.fence, job.sent_as) {
            let seen = STATS[4].fetch_add(1, Ordering::Relaxed);
            if seen < 4 {
                tracing::warn!(
                    fence = job.fence,
                    sent_as = job.sent_as,
                    "a submission reached the thread that sends out of order (DEKUBI_SEND_THREAD=1)"
                );
            }
        }
        let began = Instant::now();
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| job.run(host)));
        let busy = began.elapsed().as_nanos() as u64;
        let panicked = outcome.is_err();
        if panicked && !job.signalled {
            // SAFETY: our queue and fence; the value was reserved for this job.
            let _ = unsafe { host.queue.Signal(&host.fence, job.fence) };
        }
        STATS[0].fetch_add(1, Ordering::Relaxed);
        STATS[1].fetch_add(busy, Ordering::Relaxed);
        {
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            state.done += 1;
        }
        IDLE.notify_all();
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => fail(format_args!(
                "a submission sent ahead could not be followed by a signal ({e})"
            )),
            Err(_) => fail(format_args!("the thread that sends submissions panicked")),
        }
    }
}

/// The counters, per frame, for the frame-rate line; nothing is logged when
/// no job was sent.
pub(super) fn line(frames: f64) {
    let frames = frames.max(1.0);
    let s: [f64; 5] = std::array::from_fn(|i| STATS[i].swap(0, Ordering::Relaxed) as f64);
    if s[0] <= 0.0 && s[2] <= 0.0 && s[4] <= 0.0 {
        return;
    }
    tracing::info!(
        "submissions sent by a thread of their own per frame (DEKUBI_SEND_THREAD=1): {:.1} jobs, {:.2} ms in the sends on it, \
         the submitting thread waited {:.2} ms in {:.1} waits before putting its next list on the queue, {:.0} sent out of order",
        s[0] / frames,
        s[1] / 1e6 / frames,
        s[3] / 1e6 / frames,
        s[2] / frames,
        s[4] / frames
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rising_fences_are_in_order() {
        let mut last = (0, 0);
        assert!(in_order(&mut last, 1, 0));
        assert!(in_order(&mut last, 2, 0));
        assert!(in_order(&mut last, 5, 0));
        assert_eq!(last.0, 5);
    }

    #[test]
    fn a_plan_number_of_zero_is_ignored() {
        let mut last = (0, 0);
        assert!(in_order(&mut last, 1, 7));
        assert!(in_order(&mut last, 2, 0));
        assert!(in_order(&mut last, 3, 7));
        assert!(in_order(&mut last, 4, 9));
    }

    #[test]
    fn a_lower_plan_number_is_flagged() {
        let mut last = (0, 0);
        assert!(in_order(&mut last, 1, 9));
        assert!(!in_order(&mut last, 2, 8));
        // the largest stays the largest after a violation
        assert_eq!(last.1, 9);
        assert!(!in_order(&mut last, 3, 8));
    }

    #[test]
    fn a_repeated_or_lower_fence_is_flagged() {
        let mut last = (0, 0);
        assert!(in_order(&mut last, 4, 0));
        assert!(!in_order(&mut last, 4, 0));
        assert!(!in_order(&mut last, 3, 0));
        assert!(in_order(&mut last, 5, 0));
    }
}
