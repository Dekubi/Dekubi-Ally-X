# What's New

## 0.1.34 (2026-11-27)

- **Native Host DXR 1.1 Stages 1 + 2 (`DEKUBI_BVH_HOOK=1`, `DEKUBI_NATIVE_BVH=1`)**
  (ported from `Linox-IDK/LNX-victory` `c04e615`, GPL-3.0-or-later, off by default):
  - Stage 1 (`crates/ally-hle/src/bvh_hook.rs`, `ally-loader` `wrap_imports`):
    wraps the 7 `XboxBvhBuilder.dll` imports to log every BLAS/TLAS build,
    rehydrate, prebuild, and copy request.
  - Stage 2 (`crates/ally-gpu/src/native_rt.rs`): builds real hardware D3D12
    DXR 1.1 BLAS/TLAS acceleration structures via `ID3D12Device5` /
    `ID3D12GraphicsCommandList4` beside the console structures with automatic
    static-BLAS reuse and idle eviction.
- **Texture Mip Reduction (`DEKUBI_MIP_SKIP=0..=3`)** (`crates/ally-gpu/src/texture.rs`):
  skips top mip levels on large plain 2D on-demand textures (`>= 512px`, `>= 4`
  mips), saving up to ~4.5 GB of VRAM on 8 GB GPUs with automatic `MostDetailedMip`
  shifting.
- **Asynchronous FSR 1.0 Present** (`crates/ally-gpu/src/present_fx.rs`, `host.rs`):
  runs FSR 1.0 upscaling on `p.ahead` command lists with an 8-frame descriptor
  ring via `execute_own`, eliminating the synchronous present GPU wait.

## 0.1.33 (2026-11-27)

- **Asynchronous batched logging (`DEKUBI_LOG_SYNC=1` opt-out)** (ported from
  `neptune-dolberth-nvidia-8gb` `658d561`): `INFO` and below log lines are
  batched up to 256 KiB on background threads so disk/paging I/O never stalls
  the GPU submitting thread, while `WARN`/`ERROR` flush synchronously so crash
  diagnostics are never lost.
- **Lock-free write-watch `known_view` (`KNOWN_VIEW` bitmap)** (ported from
  `neptune-dolberth-nvidia-8gb` `658d561`): eliminates `GUARDS` lock contention
  on the submitting thread under the journal's write lock.
- **AVX2 all-zero descriptor page skip** (ported from
  `neptune-dolberth-nvidia-8gb` `658d561`): uses `_mm256_testz_si256` on
  Ryzen/Zen 4 AVX2 CPUs to skip empty changed descriptor heap pages without
  hashing every slot.
- **8 GB VRAM & descriptor stability fixes** (ported from `victory-fork`
  `af23c65`, `46da5ee`, `a463dcb`):
  - `#1320`: split submissions release per-draw dynamic descriptor slots
    immediately after `host.execute`.
  - `#1336`: `alias_of` pins aliased textures with `add_ref_alive` under lock
    (`Option<Held>`), preventing use-after-free during 8 GB VRAM sweeps.
  - `#1353`: panicking pipeline builds are caught with `catch_unwind` so
    waiters are always notified instead of hanging for the frame budget.

## 0.1.32 (2026-11-26)

- **FSR 1.0 presentation upscaling (`DEKUBI_UPSCALE=fsr1`)**: D3D12 compute
  EASU + RCAS pass in `ally-gpu`'s `Present` path (`DEKUBI_UPSCALE_SIZE`,
  `DEKUBI_FSR_SHARPNESS=0.2`), upscaling sub-1080p render-scaled frames cleanly
  to 1080p+ while cutting VRAM pressure on 8 GB cards.
- **FSR 3 optical-flow frame generation (`DEKUBI_FRAME_GEN=fsr3`)**: ported
  from `Dekubi-Ally-github`'s `fsr3_optical_flow.comp` and
  `fsr3_frame_interpolation.comp` to D3D12 HLSL `cs_5_1` (half-res diamond
  optical flow + 3x3 median bidirectional interpolation at `t = 0.5`).
- **Series S environment selector (`DEKUBI_CONSOLE=series-s`)** and launcher
  presets (`Play Dekubi X (Series S Fast + FG).cmd`) alongside the default and
  safe-mode `.cmd` launchers.

## 0.1.31 (2026-11-26)

- **100% automated shader + GPU pipeline pre-build on launch**: double-clicking
  `Dekubi X.exe` now runs incremental shader translation followed automatically
  by D3D12 PSO pre-building (`prebuild::run_for_launch`) inside the same
  progress window (`DEKUBI_AUTO_PREBUILD=0` skips Phase 2; already-built PSOs
  in `prebuilt-pipelines.txt` are skipped in seconds).
- **Anti-TDR submission burst splitting** (`DEKUBI_MAX_SUBMIT_LISTS=24`,
  `0` = uncapped, on by default): splits oversized `ExecuteCommandLists`
  bursts across sequential WDDM DMA packets so a single packet never trips
  Windows' 2-second TDR (`0x887A0006`).
- **Smooth 8 GB VRAM texture eviction** (`DEKUBI_SWEEP_MAX_TEXTURES=64`,
  `0` = uncapped, on by default): caps idle host texture evictions per
  1-second sweep to avoid 140+ MB flush spikes, and guards
  `trim_library()` with `!flying()` so `VirtualUnlock` trims never race
  active GPU flights.
- **NVIDIA Wave32 shader polish**:
  - `VItoriaBr` `809ba50`: cycle-seeded uniformity proofs and stable-LDS
    checks for `v_readfirstlane_b32`, skipping unnecessary wave32
    half-exchange barriers.
  - `VItoriaBr` `448416c`: `v_permlane16_b32`/`v_permlanex16_b32` now
    request console lane ordering and wave size.
  - `DexNom-VIctory` `76749ce`: split-scalar wave64 mask recovery across
    branches for voxel visibility shaders.

## 0.1.30 (2026-11-26)

- **Ahead-of-time PSO pre-builder (`Dekubi X.exe translate --cache --pipelines`)**
  (ported from `VIctory`): parses the title's shader pack (`ally-game`:
  `cityhash`, `shaderpack`, `pipelines`, `sgd5` databases) and builds
  pipelines ahead of time into `<cache>/pipelines` via `ally-gpu`'s
  `Prebuild` API, with `prebuilt-pipelines.txt` persistence so subsequent
  runs skip already-built PSOs.
- **1080p Series X output & render scale** (`DEKUBI_OUTPUT=1080p`,
  `DEKUBI_RENDER_SCALE=<50..=100>`, ported from `VIctory-7900XTX`
  `f63b4b1` / `4d7185e`): default `DEKUBI_OUTPUT=1080p` on `<= 8 GB`
  cards gives full Series X graphics and LODs at crisp 1080p without 4K
  VRAM pressure (`DEKUBI_OUTPUT=4k` in `Play Dekubi X (safe).cmd`).
- **Dedicated texture-heap worker on by default** (`DEKUBI_HEAP_WORKER=1`,
  `victory-fork` `d4a528b`): enabled in the launcher profile for multi-core
  CPUs (e.g. Ryzen 5 7500F 6C/12T); off in `Play Dekubi X (safe).cmd`.
- **Low-commit GPU pool backoff** (`cool_fox` `ab75ac3`): backs off
  refused GPU pool growth and defers uploads under low commit to prevent
  8 GB VRAM / 32 GB RAM thrashing and OOM.
- **Pixel quad sampling repair & ray buffer view fixes** (`DexNom-VIctory`
  `921d8d3`): repairs pixel quad sampling to reduce triangle-edge
  artifacts on car paint, glass, and skin, plus GPU-side remap/ray view
  improvements. (`DEKUBI_PORT_CBV` skipped for `0.1.30` — requires AMD
  twin-heap rewrite incompatible with NVIDIA 1M view heap cap.)

## 0.1.29 (2026-11-26)

- **Borderless fullscreen toggle (`F11` / `Alt+Enter`)**
  (`neptune-dolberth-nvidia-8gb` `6f9f2ff` + `109355c`): switch between
  borderless fullscreen and windowed mode at any time.
- **DirectStorage 64 MiB staging & 92% texture room on 8 GB cards**
  (`DEKUBI_DSTORAGE_STAGING_MB=64`, `DEKUBI_TEXTURE_ROOM_PERCENT=92`,
  `neptune-dolberth-nvidia-8gb` `1bdc77d`): on by default in the 8 GB
  launcher profile to reduce texture-streaming pop-in stalls; baseline
  values (`32` / `90`) in `Play Dekubi X (safe).cmd`.
- **More room for flights before settling** (`DEKUBI_SETTLE_ROOM=1|alt`,
  `victory-fork` `06702fe`): doubles in-flight list capacity and widens
  `merge_runs` 8x (measured -11.5 ms on the preparing thread). On by
  default in the launcher; off in `Play Dekubi X (safe).cmd`.
- **Dedicated bindless texture-heap worker** (`DEKUBI_HEAP_WORKER=1`,
  `victory-fork` `d4a528b`, opt-in / off by default): runs bindless
  texture uploads and detiling on a dedicated `dekubi-heap` thread.
- **Scoped bindless buffer sweep state** (`cool_fox` `18bdf79`): moves
  sweep state onto `HostHeap` so `Arc<Shadow>` references drop cleanly
  with their heap and never alias.
- **Shader & format fixes**:
  - NVIDIA wave32 lowering (`cool_fox` `8cc31ab`): single-block
    `v_writelane_b32`/`v_readlane_b32` spill/restore tracking, ordered LDS
    uniform reads, and a wave32 guard in `emit()`.
  - RDNA 2 legacy multiply-add (`victory-fork` `f6a09d3`):
    `v_mac_legacy_f32`/`v_mad_legacy_f32` fused product with FTZ sources
    and legacy zero rule.
  - BC7 rotation fix (`victory-fork` `06702fe` / VIctory #3111): restricts
    BC7 rotation bits to modes 4 and 5.
- **Docs & notes**: refreshed `NOTES.toml` and documented all 15 local
  reference clones in `docs/learnings/reference-repos.md` and
  `.claude/ARCHITECTURE_MAP.md`.

## 0.1.28 (2026-11-26)

- **Rays can miss everything** (`DEKUBI_BVH_NO_HIT=1`, victory-fork 96ff135,
  off by default): the ray-tracing structures are never built, so the
  title's traced passes walked whatever bytes lay where they would be. With
  the switch every node test answers "no hit": shadow rays find no occluder
  and probe rays see the sky. Tried for faces that stay dark under the
  ambient light. A translator switch: the first launch with it translates
  and compiles the shaders again.
- **Table views the stores never name are not copied back**
  (`DEKUBI_WRITTEN_ONLY=1|alt`, after VItoriaBr 6d97bea, off by default).
- victory-fork a183ea2 (the 64 KiB constant view cap before the rounding),
  41f9857 (a view moved to the first level with memory keeps the title's
  minimum level), 4679615 (`DEKUBI_VIEWS_REMADE_QUIET=1`, off by default).

## 0.1.27 (2026-11-26)

- **Three CPU switches on by default** (`DEKUBI_FILLS_PLAIN=1`,
  `DEKUBI_STEP_INDEX=1`, `DEKUBI_GATHER_BY_COPY=1`): in one run switching
  every 20 s, 45 against 53 ms per 1,000 draws, 214 against 242 ms a frame.
  Off in `Play Dekubi X (safe).cmd`.
- `DEKUBI_LIGHT_REFRESH` stays off: the run with it and `DEKUBI_ABSENT_MS=told`
  lost the card 90 s in; the run without them held.

## 0.1.26 (2026-11-26)

From victory-fork 2ba6e1f..006c353 (all CPU switches off by default, each
takes `=alt`):

- **Constant buffer views capped at 64 KiB** (fork PR #911, always on): the
  title names views larger than the host allows, an invalid call that an
  NVIDIA card answered with a device removal.
- **XMemVirtualAlloc commits told from the caller's address** (fork PR
  #1185, always on): a fix to `DEKUBI_ABSENT_MS=told`.
- `DEKUBI_FILLS_PLAIN=1`: fills under 256 bytes copied in place (estimated
  about 3 ms a frame off the submitting thread).
- `DEKUBI_STEP_INDEX=1`: a submission's steps find their copy by an index
  (estimated 1.5 to 4 ms a frame).
- `DEKUBI_LIGHT_REFRESH=1` (with `DEKUBI_ABSENT_ONCE=1
  DEKUBI_ABSENT_MS=told`): refreshes of unwritten blocks answered lock-free
  (estimated copies 80 -> 57 ms a frame outdoors).
- `DEKUBI_GATHER_BY_COPY=1`: gathering of indirect records stops copy by
  copy, not for the whole list.

## 0.1.25 (2026-11-26)

- **Ambient light where the title's rays would light** (`DEKUBI_AMBIENT`,
  ported from Belobug's VIctory-Belobug-AMD `ambient.rs`, GPL-3.0). GTA VI
  lights what the sun does not reach from ray-traced probes; rays do not
  run here, so characters and shaded areas came out as black silhouettes.
  Right after the probe resolve, its output is replaced by a light that
  follows the sky dome's colour and level, shaded in creases from the
  depth image; glossy reflections are held to it. On by default;
  `DEKUBI_AMBIENT=off` keeps the title's own resolve, `=alt` turns it on
  and off every 20 seconds. Off in `Play Dekubi X (safe).cmd`.
- **Pipeline wait 1.5 s a frame** (`DEKUBI_PIPELINE_WAIT_MS=1500`, was 24):
  in run 000508, 5,116 draws a frame against 3,989 and 37 against 58 ms per
  1,000 draws.

## 0.1.24 (2026-11-26)

- **Wide card hash on by default** (`DEKUBI_CARD_HASH_WIDE=1`, victory-fork
  5557224): the card's time fell from 63 to 34 ms a frame on the RTX 5060.
  Frames are still limited by the CPU (preparing and copying back), so the
  frame rate stays about the same for now; the card has room for heavier
  scenes. Off in `Play Dekubi X (safe).cmd`.

## 0.1.23 (2026-11-26)

- **Gathered indirect records on by default** (`DEKUBI_INDIRECT_GATHER=1`
  with `DEKUBI_CARD_REHASH_RUNS=1`): in one run switching every 20 seconds
  on the RTX 5060, 48 against 58 ms per 1,000 draws (205 against 227 ms a
  frame). Off in `Play Dekubi X (safe).cmd`.

## 0.1.22 (2026-11-26)

- **Shader constant tables cut to the constants (VIctory e04c078):** a
  shader that reads its constants at a computed offset carried its whole
  program (up to 64 KiB) as a constant table, which a driver may keep for
  every thread; VIctory found GBs of video memory reserved for it on
  NVIDIA. The table now starts where the instructions end. Shaders change,
  so the shader preparing runs once again.
- **victory-fork 6d20eed (5 commits), all off by default:** the card's
  compare hash with 64 threads (`DEKUBI_CARD_HASH_WIDE=1`), changed card
  copies taken from a list (`DEKUBI_CARD_CHANGED_LIST=1|alt`), unmapped
  view blocks remembered (`DEKUBI_ABSENT_ONCE=1|alt`).

## 0.1.21 (2026-11-26)

- **Less CPU work per draw (Belobug, from romainhedouin's pull requests):**
  - a constant buffer that is not 256-byte aligned is copied once per list
    for all its draws, not once per draw (84ea9c1);
  - a pipeline a list binds again is not looked up and kept again (92d353f);
  - descriptor tables made afresh are found by a quick hash and their
    records compared, so a hash collision can no longer reuse wrong
    descriptors (b41f3f3);
  - a range shaders write is declared once per list until title memory may
    change (dd799aa; `DEKUBI_GPU_WRITE_ONCE=0` as before, set in the safe
    launcher);
  - buffer-to-texture copies take their staging bytes from the list's
    pooled upload memory instead of a new upload buffer each (5e08eee).
- Checked and already here: b022d86, ff920f2. Left out: ab5ece8 (other
  passes record into the same list), e6b6386 (a large refactor for a few
  allocations a draw).

## 0.1.20 (2026-11-26)

- **victory-fork 9c184b3 (10 commits):**
  - Frame rate and frame time shown in the corner of the picture
    (`DEKUBI_FPS_COUNTER=0` hides it).
  - Changed bytes of title buffers found 64 bytes at a time by a mask (on;
    `DEKUBI_CHANGED_RUNS=0` the old walk).
  - Off by default, not yet measured here: copies of small buffers in the
    card's memory (`DEKUBI_CARD_BUFFERS_MIN_KB=64`, their card time 68 -> 50
    ms), textures the game reads back on the CPU (`DEKUBI_TEXTURE_BACK=1`, the
    water: a boat no longer sinks, but their run later lost the card), four
    dwords per scalar buffer load (`DEKUBI_SHADER_OPT=127`).
  - Diagnostics builds: frame shots (`DEKUBI_SHOT=<folder>`), copy-back
    read counters, a RenderDoc hook.
- Belobug's CPU-overhead commits checked: one (the heap fast path) is
  already here; the others need hand ports.

## 0.1.19 (2026-11-26)

- **Normalized buffer values exact (VIctory da82e44):** 8-, 10- and 16-bit
  normalized values read from buffers now convert to the float nearest the
  exact quotient, as the console does, instead of the host's division,
  which can land one step off. The game reads a material index from a
  vertex colour this way, so a value one step low picked another material.
  Also each format's ends are exactly 1.0 and -1.0. Pixel shaders change, so
  the shader preparing runs once again.
- Quad reads at triangle edges (VIctory 4015a18): already in Dekubi X.

## 0.1.18 (2026-11-26)

- **victory-fork 465b94f:** an indirect record that misses its window is
  given a slot that the list's prelude clamps in one pass with the list's
  other such records, instead of a pass of Dekubi X's in front of each
  draw (`DEKUBI_INDIRECT_GATHER=1`, off by default; `alt` switches every 20
  seconds). On victory-fork's RTX 4070, with `DEKUBI_CARD_REHASH_RUNS=1`,
  104 -> 97 ms a frame. Tried here with `Try gathered records.cmd`.
- Three diagnostics-build lints fixed.

## 0.1.17 (2026-11-26)

- **victory-fork 3973179 (9.6 fps in its first rooms, RTX 4070):** the
  game's work goes to the card in fewer, larger submissions
  (`DEKUBI_JOIN_PARTS=1`, about 6 % there) and what shaders wrote comes back
  through a view of Dekubi X's own (`DEKUBI_BACK_OWN_VIEW=1`, about 9 %).
  Both on by default here, off in `Play Dekubi X (safe).cmd`. Also its new
  meters (card time by pipeline, the game's fence pacing) and the game's
  other `clzero` loop handled whole (XCL-derived file).
- Found with the diagnostics build: about 497 of the ~500 draws dropped a
  frame are steps of the ray-tracing structure builder (off on purpose);
  the 7 refused compute pipelines need 64-lane waves, which the RTX 5060
  does not run.

## 0.1.16 (2026-11-25)

- **Refused shaders saved:** when the graphics driver refuses a pipeline,
  its translated shader is saved to `logs\refused-shaders\` (seven compute
  pipelines on the RTX 5060 so far), so why can be found without the game.

## 0.1.15 (2026-11-25)

- **Crash fix:** 0.1.14's run lost the graphics card ("device removed") after
  eight minutes, the first time in any log. The one shader seen to translate
  and run that did not before was compute f133e35efed2a48a, recovered by
  DexNom 76749ce (64-lane masks restored from scalar copies); that change is
  taken out again. DexNom's sampler fix stays.

## 0.1.14 (2026-11-25)

- **More shaders translate (DexNom ddb7332, 76749ce):** shaders that pick
  one of two fixed samplers by a branch (40 variants in DexNom's archive) and
  a voxel visibility shader that restores a 64-lane mask from scalar copies.
  Shaders that did not translate drop every draw that uses them.
- `DEKUBI_MERGE_RUNS` back to 8,192: 32,768 in 0.1.13 sent about 19,500
  copies of a few bytes to the card each frame and made frames slower (106 ms
  per 1,000 draws against 73). `Play Dekubi X.cmd` sets 8,192 meanwhile.

## 0.1.13 (2026-11-25)

- **More work queued for the card before waiting:** 512 batches in the air
  instead of 256 (`DEKUBI_LATE_WAIT_LISTS`), and up to 32,768 small memory
  changes passed on without waiting (`DEKUBI_MERGE_RUNS`). Tested on the
  RTX 5060 with `Try 512 lists.cmd`: the card went from 78 % to 91 % busy and
  frames took 73 ms per 1,000 draws against 80. `Play Dekubi X (safe).cmd`
  keeps 256 and 512.
- **Lost draws restored (Belobug 8629211):** the game asks for blending on
  integer render targets, which the console ignores and the PC's D3D12
  refuses; the whole pipeline was refused and its draws lost (in our logs:
  mesh pipeline f5e50eb2108c425b, the same one Belobug found). Blending is
  now turned off on those targets only.

## 0.1.12 (2026-11-25)

- **Less waiting on the card:** in 0.1.11 the game's threads stood about
  119 ms a frame waiting for the card whenever the game changed memory the
  card was also writing and more than 512 small changes had to be passed on;
  up to 8,192 are now passed on without waiting (`DEKUBI_MERGE_RUNS`, 512 in
  `Play Dekubi X (safe).cmd`). The log now says why each such wait happened.
- **Progress window during shader preparing:** a small window shows how many
  shaders are ready while they are prepared before the game starts, instead
  of nothing on screen. `DEKUBI_PROGRESS_WINDOW=0` hides it.
- **Missing areas that fill in later (0.1.11 report):** these are textures
  arriving late, not the hoisted clamps: the game's own streaming runs slowly
  at 4-8 fps, and textures wait whenever the card's memory is full. The
  texture numbers of 0.1.11 match 0.1.9.

## 0.1.11 (2026-11-25)

- **Hoisted clamps (after Belobug's AMD fork):** the check every indirect
  draw's arguments get before the card draws them now runs in a list of its
  own ahead of the draws, instead of between every two draws. On Belobug's
  card the GPU time of a 9,000-draw scene went from 225 to about 80 ms; this
  is its first run on NVIDIA. `DEKUBI_HOIST_CLAMPS=0` turns it off (so does
  `Play Dekubi X (safe).cmd`).

## 0.1.10 (2026-11-25)

- **1080p by default on cards up to 8 GB.** The small-picture bug of 0.1.6
  was the overlay, fixed in 0.1.8; at 1080p the picture fills the window and
  looked better (4.2 fps against 4.0 at 4K in the world).
- **victory-fork 0b0074e:** copies of game memory made by Dekubi X itself
  (`DEKUBI_COPY_PLAIN=1`, now a default), the submit and copy-back threads
  kept on the fast cores of hybrid processors.
- **Correction:** the "6.0 to 8.9 fps" figure quoted for draw windows in
  0.1.9 was withdrawn by victory-fork (two different scenes were compared).
  Its corrected figure: 145 to 124 ms a frame at equal draws.

## 0.1.9 (2026-11-25)

- **One copy at a time:** starting Dekubi X while it is already running brings
  the running game to the front and says so, instead of starting a second
  copy that shares the card (two copies halved the frame rate and one ran out
  of memory). `DEKUBI_ALLOW_SECOND=1` turns the check off.
- **Faster frames (victory-fork a253706, f85101b):** cheaper indirect draws
  (draw windows), a submit thread of Dekubi X's own, a copy queue for
  fetching what shaders wrote, a reader-writer lock on the shader heaps, a
  frame log and card meter, and a driver watch that turns draw windows off
  at the first display-driver warning. On victory-fork's RTX 4070 the
  windows alone took the first room from 6.0 to 8.9 fps.
- **RTX 5060 preset (VItoriaBr):** texture refreshes no longer wait while the
  game remaps memory (VItoriaBr measured 900-3,700 ms down to 6-25 ms),
  copies are refreshed and merged by worker threads, neighbouring copies are
  joined. All new settings are defaults only; any you set yourself win.
- **Shaders (VItoriaBr, upstream 0601955):** helper lanes read their own
  value and quads are read with quad reads, fixing lines on car paint,
  glass and skin; first-lane and lane-read refusals are retried wave-wide.
- Reviewed, not taken: Belobug's title memory in place (Dekubi X has the
  same, `DEKUBI_INPLACE`; on this RTX 5060 it made the card 3-4x slower).

## 0.1.8 (2026-11-25)

- Fixed: the game was drawn into the small performance overlay box and the
  game window stayed white. The overlay is no longer taken for the game's
  window.
- Fixed: the game showed as a small picture in the corner of a white window.
  Telling it the TV is 1080p (0.1.6) made it draw at that size into the 4K
  window. It is told 4K again; the hardware recommendation now only picks
  texture settings.

## 0.1.7 (2026-11-25)

From Dekubi Ally:
- **Performance overlay:** FPS, frame time, draws, video memory and RAM in
  the game window's top-left corner. F10 shows or hides it;
  `DEKUBI_OVERLAY=0` turns it off.
- **Run summary:** `logs\last-summary.txt` with the game, average and lowest
  FPS and peak video memory, rewritten every 10 s.
- **Hardware recommendation:** resolution and memory settings picked from the
  card's memory: 1080p up to 8 GB, 1440p up to 12 GB, 4K above.
- **Game controls:** remap keyboard keys and pad buttons in `controls.txt`
  next to `Dekubi X.exe` (example: `controls.example.txt`).

## 0.1.6 (2026-11-25)

- The game is told it is on a 1080p TV instead of 4K (idea from Dekubi Ally's
  Max Resolution and hardware recommendation), so it should draw about a
  quarter of the pixels on the 8 GB card. For 4K set
  `DEKUBI_DISPLAY_MODE=3840x2160`.

## 0.1.5 (2026-11-25)

- SCSKiller from Dekubi Ally, run automatically before each launch:
  raises the NVIDIA driver shader cache from 4 GB to 10 GB, keeps Windows
  Disk Cleanup from deleting compiled shaders, and notes driver updates.
  The first two need Dekubi X to run as administrator once; otherwise they
  are skipped. `DEKUBI_SCS_AUTO=0` turns it off.

## 0.1.4 (2026-11-25)

- Gemini's launcher is back (it was lost when the base moved to
  victory-fork): double-click `Dekubi X.exe` to find the game folder next to
  it and start it, with a log in `logs\` and an error box if it fails.
- RTX 5060 8 GB defaults are on again for every launch: texture levels on
  demand (about 3 GB less video memory), the relay, and a 24 ms pipeline wait
  instead of 500 ms. Without them the game ran at 1 to 2 fps in play.
- Shader warm-up before the game starts, so fewer objects are missing while
  shaders are made. The first launch takes longer while the cache fills.
- `scripts\build.ps1` also copies `Dekubi X.exe` to the project folder.

## 0.1.3 (2026-11-25)

- Fixed: the dynamic descriptor search could miss a free run that crosses
  the search start, so a table was refused while slots were free. The
  fork's walk cap cut off the part of the search VIctory #974 added.
  Found by the Windows test `dynamic_search_can_find_a_run_across_the_cursor`.
- New `scripts\autopad.ps1`: debug launcher that presses controller
  buttons on a timer (default: A every 4 s) to get through menus, and logs
  to `_auto-logs\autopad-*`.
- First real launch on the RTX 5060: the title boots with no missing
  imports and draws its first screens at 30 fps.

## 0.1.2 (2026-11-25)

- 106 of VIctory's 166 newer fixes are in, on top of victory-fork: GPU hang
  handling (TDR), XAsync/XTaskQueue fixes, loader crash log, kernel and media
  fixes, more shader instructions. List: docs/release-notes/0.1.2-upstream.md.
- Two fork-vs-upstream fits: shader instruction construction, and the
  descriptor heap lock held while mirroring (after VItoriaBr).

## 0.1.1 (2026-11-25)

- Dekubi X now carries a full runtime: loader, console services, D3D12.X to
  D3D12, and the RDNA2 to DXIL shader recompiler, from victory-fork 9376fd6.
- Includes victory-fork's world-rendering work: in-place title memory,
  texture levels on demand (helps 8 GB cards), DXIL clean-up passes,
  ray-tracing groundwork, descriptor mirroring.
- New `Dekubi X.exe` (built by `scripts\build.ps1`) and `scripts\play.ps1`.
- New notes: `Dekubi X.exe notes` shows current goals; they also print when
  `run` starts and ends. Edit `NOTES.toml`, or put a `notes.toml` in
  `%LOCALAPPDATA%\DekubiX\data\`.
- Settings moved from `VICTORY_*` to `DEKUBI_*` environment variables.
