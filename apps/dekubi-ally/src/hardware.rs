// Dekubi X, GPL-3.0-or-later. After Dekubi Ally (github.com/Dekubi/Dekubi-Ally,
// Fritchii): src/yuzu/dekubi/hardware_recommend.cpp, by Dekubi.
//! Hardware recommendation: settings picked from the graphics card's own
//! memory, applied only where the user set nothing.

/// The card Dekubi X will run on (the one with the most own memory).
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub name: String,
    pub vram_mb: u64,
}

/// Settings for a card with `vram_mb` of its own memory.
pub fn recommend(vram_mb: u64) -> Vec<(&'static str, &'static str)> {
    let mut out = vec![
        ("DEKUBI_RELAY", "2"),
        ("DEKUBI_PIPELINE_WAIT_MS", "1500"),
        // victory-fork f85101b/a253706/0b0074e (RTX 4070): at equal draws,
        // windows with plain copies 124 ms a frame against 145 at 4,500
        // draws (its earlier 6.0 -> 8.9 fps compared different scenes and
        // was withdrawn in 0b0074e). The driver watch (on by default) turns
        // windows off at the first display-driver event.
        ("DEKUBI_INDIRECT_WINDOW", "1"),
        ("DEKUBI_COPY_PLAIN", "1"),
        ("DEKUBI_ASYNC_SUBMIT", "1"),
        ("DEKUBI_QUEUE2", "1"),
        ("DEKUBI_TEXTURE_NO_WAIT", "1"),
        // VItoriaBr 18924c7's RTX 5060 Ti preset (3.5-6 fps in the world):
        // texture refreshes 900-3,700 ms -> 6-25 ms with the one above;
        // copies refreshed and merged by the wide workers; neighbouring
        // copies joined. (Its 6 GB of card buffers is left out: 8 GB here.)
        ("DEKUBI_WIDE_REFRESH", "1"),
        ("DEKUBI_WIDE_MERGE", "1"),
        ("DEKUBI_JOIN_COPIES", "1"),
        // 0.1.11 on the RTX 5060: the title's threads stood 119 ms a frame
        // in settles for title bytes under flights' shader writes, with
        // 34,900 changed runs a frame put in; past 512 runs a submission
        // the flights were settled instead. More runs go up as copies of
        // their own (each a few bytes) rather than waiting for the card.
        // 0.1.12 at 8,192: the settles went from 81 ms a frame to none in one
        // run and 65 ms in the next (2,872 submissions past the cap); 3,100
        // runs a frame of a few bytes, 0.02 MB.
        // 32,768 (0.1.13) was worse: 19,500 copies of a few bytes a frame,
        // the card 200-450 ms a frame, 106 ms per 1,000 draws against 73.
        ("DEKUBI_MERGE_RUNS", "8192"),
        // 0.1.12: with the merge settles gone the submitting thread stood
        // 109 ms a frame at 256 lists in the air; at 512 none, the card 91 %
        // busy (78 % before), 73 ms per 1,000 draws against 80, no slot
        // refused and no crash in 11 minutes.
        ("DEKUBI_LATE_WAIT_LISTS", "512"),
        // victory-fork 3973179 (RTX 4070, first rooms, 120 -> 104 ms a
        // frame with both): parts that follow one another go to the host as
        // one submission (29 -> 9 a frame, about 6 %), and what shaders wrote
        // goes back through a view of our own (about 9 %; 0 stores astray in
        // 1.36 million with its check).
        ("DEKUBI_JOIN_PARTS", "1"),
        ("DEKUBI_BACK_OWN_VIEW", "1"),
        // victory-fork 465b94f: indirect records that miss their window
        // clamped once per list, with the card's rehash passes joined.
        // Measured in one run switching every 20 s (2026-11-26, 0.1.22):
        // 48 against 58 ms per 1,000 draws, 205 against 227 ms a frame.
        ("DEKUBI_INDIRECT_GATHER", "1"),
        ("DEKUBI_CARD_REHASH_RUNS", "1"),
        // victory-fork 5557224: each copy piece hashed by a group of 64
        // threads. On the RTX 5060 (2026-11-26, 0.1.23, two runs) the card's
        // time fell from 63 to 34 ms a frame; frames stayed CPU-bound.
        ("DEKUBI_CARD_HASH_WIDE", "1"),
        // victory-fork de4146a/84c5dc0/006c353, measured together in one run
        // switching every 20 s (2026-11-26, 0.1.26): 45 against 53 ms per
        // 1,000 draws, 214 against 242 ms a frame. LIGHT_REFRESH with
        // ABSENT_MS=told is left off: the run with it lost the card at 90 s.
        ("DEKUBI_FILLS_PLAIN", "1"),
        ("DEKUBI_STEP_INDEX", "1"),
        ("DEKUBI_GATHER_BY_COPY", "1"),
        // neptune-dolberth-nvidia-8gb 1bdc77d: 64 MiB DirectStorage staging
        // floor to avoid texture-streaming pop-in stalls under heavy loads.
        ("DEKUBI_DSTORAGE_STAGING_MB", "64"),
        // victory-fork 06702fe: more room for flights before settling
        // (late_most x2, merge_runs x8; measured -11.5 ms prep thread).
        ("DEKUBI_SETTLE_ROOM", "1"),
        // victory-fork d4a528b: run bindless texture uploads/detiling on a
        // dedicated worker thread (good win on 6C/12T Ryzen 5 7500F).
        ("DEKUBI_HEAP_WORKER", "1"),
        // 0.1.31 anti-TDR & 8 GB VRAM eviction smoothing: split oversized
        // ExecuteCommandLists bursts across WDDM DMA packets (<=24 lists) and
        // cap per-second idle texture evictions (<=64).
        ("DEKUBI_MAX_SUBMIT_LISTS", "24"),
        ("DEKUBI_SWEEP_MAX_TEXTURES", "256"),
    ];
    // Up to 8 GB the title is told of a 1080p TV (Series X LODs/graphics at
    // crisp 1080p via VIctory-7900XTX f63b4b1's `DEKUBI_OUTPUT=1080p`, saving
    // VRAM and GPU time on 8 GB cards).
    if vram_mb <= 8 * 1024 + 512 {
        out.push(("DEKUBI_OUTPUT", "1080p"));
        // neptune-dolberth-nvidia-8gb 1bdc77d: widen texture room to 92% on 8 GB cards.
        out.push(("DEKUBI_TEXTURE_ROOM_PERCENT", "92"));
    }
    if vram_mb <= 12 * 1024 + 512 {
        out.push(("DEKUBI_LEVELS_ON_DEMAND", "1"));
    }
    out
}

/// The card with the most own memory, if the system lists one.
#[cfg(windows)]
pub fn card() -> Option<Card> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
    };
    // SAFETY: plain DXGI queries.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut best: Option<Card> = None;
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let vram_mb = (desc.DedicatedVideoMemory as u64) >> 20;
            if best.as_ref().is_none_or(|b| vram_mb > b.vram_mb) {
                let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
                best = Some(Card {
                    name: String::from_utf16_lossy(&desc.Description[..len])
                        .trim()
                        .to_owned(),
                    vram_mb,
                });
            }
        }
        best
    }
}

#[cfg(not(windows))]
pub fn card() -> Option<Card> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_levels_follow_video_memory() {
        let rec8 = recommend(8151);
        assert!(rec8.contains(&("DEKUBI_OUTPUT", "1080p")));
        assert!(rec8.contains(&("DEKUBI_TEXTURE_ROOM_PERCENT", "92")));
        assert!(rec8.contains(&("DEKUBI_DSTORAGE_STAGING_MB", "64")));
        assert!(rec8.contains(&("DEKUBI_SETTLE_ROOM", "1")));
        assert!(rec8.contains(&("DEKUBI_HEAP_WORKER", "1")));
        assert!(rec8.contains(&("DEKUBI_MAX_SUBMIT_LISTS", "24")));
        assert!(rec8.contains(&("DEKUBI_SWEEP_MAX_TEXTURES", "256")));
        assert!(
            !recommend(16 * 1024)
                .iter()
                .any(|(k, _)| *k == "DEKUBI_OUTPUT")
        );
        assert!(rec8.contains(&("DEKUBI_LEVELS_ON_DEMAND", "1")));
        assert!(
            !recommend(24 * 1024)
                .iter()
                .any(|(k, _)| *k == "DEKUBI_LEVELS_ON_DEMAND")
        );
    }
}
