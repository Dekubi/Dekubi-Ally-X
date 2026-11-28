// Adapted from victory-fork (https://github.com/fxceless/victory-fork, commit 9376fd6 + 106 VIctory fixes), a fork of
// VIctory (https://github.com/Marvisak/VIctory). GPL-3.0-or-later, by Marvisak, fxceless
// and contributors. Renamed for Dekubi X.
//! `dekubi-x` (shipped as `Dekubi X.exe`): the front end. Launcher features
//! (double-click start, RTX 5060 8 GB defaults, shader warm-up, per-launch
//! log, error dialog) by Gemini, kept by the owner's request.
//!
//! Every command reads the user's own files and writes only to stdout or to
//! the user's cache/data/logs directories (outside the repository).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod census;
mod hardware;
mod inspect;
mod monitor;
#[cfg(windows)]
mod prebuild;
mod progress;
mod run;
mod scskiller;
mod shader;
mod single;
mod translate;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "Dekubi X",
    version,
    about = "Run an Xbox Series title natively on Windows"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Summarise one PE image: headers, sections, imports, TLS, build id.
    Inspect {
        file: PathBuf,
        /// List every imported function.
        #[arg(long)]
        imports: bool,
        /// List every exported function.
        #[arg(long)]
        exports: bool,
    },
    /// Aggregate the imports of every PE image under the given paths and
    /// show which ones our runtime covers. Defaults to the configured
    /// game and test-title directories.
    Census {
        paths: Vec<PathBuf>,
        /// Do not count host pass-through as covered.
        #[arg(long)]
        strict: bool,
        /// List every missing `dll!function`, most used first.
        #[arg(long)]
        missing: bool,
    },
    /// List the `dll!export` pairs an import library describes.
    LibExports {
        lib: PathBuf,
        /// List every export, not just per-DLL counts.
        #[arg(long)]
        list: bool,
    },
    /// Find shader containers in files or directories (also inside engine
    /// archives, if uncompressed) and report their layout.
    ShaderInfo {
        paths: Vec<PathBuf>,
        /// Print one line per container found.
        #[arg(long)]
        verbose: bool,
        /// Also list the decoded native instructions (implies --verbose).
        #[arg(long)]
        disasm: bool,
        /// Survey ray-tracing traversal programs (`image_bvh*_intersect_ray`)
        /// and cluster their loop/stack fingerprints (DXR Stage 3 analysis).
        #[arg(long)]
        rt_survey: bool,
        /// Only process containers whose label contains this substring.
        #[arg(long)]
        filter: Option<String>,
    },
    /// Run every shader found (containers, SGD5 files) through the
    /// translator and report what it cannot handle yet.
    Translate {
        paths: Vec<PathBuf>,
        /// Check emitted DXIL with dxil.dll's validator (`DEKUBI_DXIL`,
        /// else the newest Windows SDK's).
        #[arg(long)]
        validate: bool,
        /// Also create a host D3D12 pipeline for each emitted shader. With
        /// `--cache`: then make every pipeline of the title's shader pack
        /// into the host pipeline library `run` reads.
        #[arg(long)]
        pipelines: bool,
        /// Print one line per program and outcome.
        #[arg(long)]
        list: bool,
        /// Worker threads (default: every logical core).
        #[arg(long)]
        jobs: Option<usize>,
        /// Warm the run's shader cache: a file of shader hashes (the 16
        /// hex digits a run's log names a shader by, anywhere in a line),
        /// or `all` to warm every container found. Set `DEKUBI_HOST_WAVE`
        /// and `DEKUBI_VIEW_LIMIT` to match the host GPU if needed.
        #[arg(long, conflicts_with_all = ["validate", "list", "cache"])]
        warm: Option<PathBuf>,
        /// Shorthand for `--warm all`: translate every shader container
        /// found into the shader cache `run` reads.
        #[arg(long, conflicts_with_all = ["validate", "list", "warm"])]
        cache: bool,
    },
    /// Prepare, map and bind a title executable; with --execute, run it.
    Run {
        exe: PathBuf,
        /// What unimplemented imports do when called.
        #[arg(long, value_enum, default_value_t = Unimplemented::Trap)]
        unimplemented: Unimplemented,
        /// Disable host pass-through (everything must be implemented by us).
        #[arg(long)]
        strict: bool,
        /// Actually start the title. Without it, stop after binding.
        #[arg(long)]
        execute: bool,
        /// The console model the title sees (device type, memory).
        #[arg(long, value_enum, default_value_t = Console::SeriesX)]
        console: Console,
        /// Arguments for the title, after `--` (it reads the whole command
        /// line; a title that relaunches itself passes some).
        #[arg(last = true)]
        title_args: Vec<String>,
    },
    /// Show the resolved configuration and check it.
    Config,
    /// Show the current goals (finished and idle ones hidden).
    Notes,
}

#[derive(Clone, Copy, ValueEnum)]
enum Unimplemented {
    Trap,
    Zero,
}

#[derive(Clone, Copy, ValueEnum)]
enum Console {
    SeriesX,
    SeriesS,
}

fn main() -> Result<()> {
    #[cfg(windows)]
    if std::env::args_os().len() > 1 {
        // SAFETY: attach to the parent console (PowerShell/CMD) when invoked
        // with CLI arguments so stdout/stderr remain visible.
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(
                windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
    }
    // The audio recorder `run` starts for `DEKUBI_AUDIO_CAPTURE`.
    #[cfg(all(windows, feature = "diagnostics"))]
    if ally_hle::modules::audio::capture::child() {
        return Ok(());
    }
    let cli = Cli::parse();
    match cli.command {
        None => launch_default(),
        Some(Command::Inspect {
            file,
            imports,
            exports,
        }) => {
            ally_core::logging::init("info");
            inspect::run(&file, imports, exports)
        }
        Some(Command::Census {
            paths,
            strict,
            missing,
        }) => {
            ally_core::logging::init("info");
            census::run(paths, strict, missing)
        }
        Some(Command::LibExports { lib, list }) => {
            ally_core::logging::init("info");
            inspect::lib_exports(&lib, list)
        }
        Some(Command::ShaderInfo {
            paths,
            verbose,
            disasm,
            rt_survey,
            filter,
        }) => {
            ally_core::logging::init("info");
            shader::run(
                paths,
                verbose || disasm,
                disasm,
                rt_survey,
                filter.as_deref(),
            )
        }
        Some(Command::Translate {
            paths,
            validate,
            pipelines,
            list,
            jobs,
            warm,
            cache,
        }) => {
            ally_core::logging::init("info");
            // `DEKUBI_VIEW_LIMIT=<null slot>`: translate as on a host whose view heap is capped
            if let Some(n) = std::env::var("DEKUBI_VIEW_LIMIT")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                ally_shader::emit::set_view_heap_limit(n);
            }
            // `DEKUBI_HOST_WAVE=32`: translate as for a GPU whose waves are at most 32 lanes
            if let Some(n) = std::env::var("DEKUBI_HOST_WAVE")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                ally_shader::emit::set_host_max_wave(n);
            }
            if cache {
                let resolved_paths = default_paths(paths.clone())?;
                let threads = jobs
                    .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
                    .max(1);
                if pipelines && std::env::var_os("DEKUBI_PREBUILD_KEYS").is_some() {
                    #[cfg(windows)]
                    {
                        translate::probe_host_gpu_for_warm();
                        return prebuild::run(&resolved_paths, threads);
                    }
                    #[cfg(not(windows))]
                    anyhow::bail!("pipeline pre-build is only supported on Windows");
                }
                translate::warm(paths, Path::new("all"), jobs)?;
                if pipelines {
                    #[cfg(windows)]
                    {
                        translate::probe_host_gpu_for_warm();
                        prebuild::run(&resolved_paths, threads)?;
                    }
                    #[cfg(not(windows))]
                    anyhow::bail!("pipeline pre-build is only supported on Windows");
                }
                return Ok(());
            }
            if let Some(file) = warm {
                return translate::warm(paths, &file, jobs);
            }
            translate::run(paths, validate, pipelines, list, jobs)
        }
        Some(Command::Run {
            exe,
            unimplemented,
            strict,
            execute,
            console,
            title_args: _,
        }) => {
            if execute {
                let log_path = init_launch_logging("info");
                if let Some(log_path) = &log_path {
                    tracing::info!(log = %log_path.display(), "per-launch log started");
                }
                if !single::claim() {
                    already_running();
                    return Ok(());
                }
                apply_default_env();
                auto_warm(&exe);
                refused_dir(log_path.as_deref());
                monitor::start(
                    &exe,
                    log_path.and_then(|p| p.parent().map(Path::to_path_buf)),
                );
            } else {
                ally_core::logging::init("info");
            }
            show_notes();
            let code = run::run(&exe, unimplemented, strict, execute, console);
            show_notes();
            let code = code?;
            if code != 0 {
                // Preserve all 32 bits of the Windows process status.
                std::process::exit(code as i32);
            }
            Ok(())
        }
        Some(Command::Config) => {
            ally_core::logging::init("info");
            show_config()
        }
        Some(Command::Notes) => {
            show_notes();
            Ok(())
        }
    }
}

fn init_launch_logging(default_level: &str) -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let data_dir = ally_core::Config::load()
        .map(|c| c.data_dir)
        .unwrap_or_else(|_| std::env::temp_dir().join("dekubi-ally"));
    let log_dir = ally_core::logging::launch_log_dir(exe_dir.as_deref(), &data_dir);
    let log_path = ally_core::logging::launch_log_path(&log_dir);
    match ally_core::logging::init_with_file(default_level, &log_path) {
        Ok(()) => Some(log_path),
        Err(e) => {
            ally_core::logging::init(default_level);
            tracing::warn!(
                error = %e,
                path = %log_path.display(),
                "could not open launch log file"
            );
            None
        }
    }
}

/// A second launch while the game runs: says so (the running game's window
/// was brought to the front by `single::claim`).
fn already_running() {
    let message = "Dekubi X is already running.\n\nOnly one copy runs at a time: two share the graphics card and its memory, and both slow down.";
    #[cfg(windows)]
    show_error_dialog(message);
    #[cfg(not(windows))]
    eprintln!("{message}");
}

#[cfg(windows)]
fn show_error_dialog(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let title: Vec<u16> = "Dekubi X\0".encode_utf16().collect();
    let body: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: both buffers are valid NUL-terminated UTF-16 strings.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Double-click / no-argument launch: open a per-launch log file, apply safe
/// RTX 5060 8GB defaults, locate an unpacked GDK title folder, run incremental
/// shader cache warm-up, and start the title immediately.
fn launch_default() -> Result<()> {
    let log_path = init_launch_logging("info");
    if let Some(p) = &log_path {
        tracing::info!(
            version = ally_core::VERSION,
            log = %p.display(),
            "Dekubi X launch started"
        );
    } else {
        tracing::info!(version = ally_core::VERSION, "Dekubi X launch started");
    }

    if !single::claim() {
        already_running();
        return Ok(());
    }
    apply_default_env();

    let result = (|| -> Result<()> {
        let exe = find_default_title_exe()?;
        if let Ok(cfg) = ally_core::Config::load() {
            tracing::info!(
                exe = %exe.display(),
                cache_dir = %cfg.cache_dir.display(),
                data_dir = %cfg.data_dir.display(),
                "resolved launch paths"
            );
        } else {
            tracing::info!(exe = %exe.display(), "resolved title executable");
        }

        auto_warm(&exe);
        refused_dir(log_path.as_deref());
        monitor::start(
            &exe,
            log_path
                .as_ref()
                .and_then(|p| p.parent().map(Path::to_path_buf)),
        );

        println!(
            "Dekubi X {}: launching {}",
            ally_core::VERSION,
            exe.display()
        );
        show_notes();
        let console = match std::env::var("DEKUBI_CONSOLE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "series-s" | "series_s" | "seriess" | "s" | "lockhart" => Console::SeriesS,
            _ => Console::SeriesX,
        };
        run::run(&exe, Unimplemented::Trap, false, true, console).map(|_| ())
    })();
    if let Err(err) = &result {
        tracing::error!("fatal launch error: {err:#}");
        let msg = match &log_path {
            Some(p) => format!("{err:#}\n\nLog file:\n{}", p.display()),
            None => format!("{err:#}"),
        };
        #[cfg(windows)]
        show_error_dialog(&msg);
        #[cfg(not(windows))]
        eprintln!("Error: {msg}");
    }
    result
}

/// Defaults (only when unset): Gemini's RTX 5060 8 GB set, now picked by
/// the card's memory (`hardware::recommend`):
/// - `DEKUBI_RELAY=2`: async relay + late wait (cuts GPU idle time)
/// - `DEKUBI_LEVELS_ON_DEMAND=1`: texture levels on demand, saves ~3 GB VRAM
/// - `DEKUBI_PIPELINE_WAIT_MS=1500`: frames wait for pipelines still compiling (24 ms left 28% fewer objects drawn, run 000508)
fn apply_default_env() {
    let card = hardware::card();
    // Unknown card: as for 8 GB (the safe side).
    let vram_mb = card.as_ref().map_or(8 * 1024, |c| c.vram_mb);
    tracing::info!(
        card = card.as_ref().map_or("unknown", |c| c.name.as_str()),
        vram_mb,
        "hardware recommendation"
    );
    for (key, value) in hardware::recommend(vram_mb) {
        if std::env::var_os(key).is_none() {
            // SAFETY: called at startup, before any background or title
            // threads are spawned.
            unsafe { std::env::set_var(key, value) };
            tracing::info!(key, value, "applied default env setting");
        } else if let Ok(existing) = std::env::var(key) {
            tracing::info!(key, value = %existing, "keeping user env setting");
        }
    }
}

/// Incremental shader warm-up (`DEKUBI_AUTO_WARM=0` skips it) followed by
/// automatic D3D12 PSO pre-build (`DEKUBI_AUTO_PREBUILD=0` skips it), both
/// shown in the same progress window before the title starts.
fn auto_warm(exe: &Path) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, Ordering};

    if let Ok(cfg) = ally_core::Config::load() {
        scskiller::run_on_launch(&cfg.cache_dir);
    }
    let do_warm = std::env::var("DEKUBI_AUTO_WARM")
        .map(|v| v.trim() != "0")
        .unwrap_or(true);
    let do_prebuild = std::env::var("DEKUBI_AUTO_PREBUILD")
        .map(|v| v.trim() != "0")
        .unwrap_or(true);
    if !do_warm && !do_prebuild {
        tracing::info!(
            "automatic shader warm-up and PSO pre-build skipped \
             (DEKUBI_AUTO_WARM=0, DEKUBI_AUTO_PREBUILD=0)"
        );
        return;
    }

    let phase = Arc::new(AtomicU8::new(if do_warm { 0 } else { 1 }));
    let phase_ui = Arc::clone(&phase);
    let began = std::time::Instant::now();
    let window = progress::Progress::show(move || {
        warm_text(phase_ui.load(Ordering::Relaxed), began.elapsed().as_secs())
    });

    if do_warm {
        tracing::info!(
            exe = %exe.display(),
            "starting automatic incremental shader warm-up"
        );
        match translate::warm_for_launch(exe) {
            Ok(summary) => tracing::info!(
                files = summary.files,
                cached = summary.cached,
                translated = summary.translated,
                failed = summary.failed,
                elapsed_s = format_args!("{:.2}", summary.elapsed.as_secs_f64()),
                "automatic shader warm-up finished"
            ),
            Err(e) => tracing::warn!(error = %e, "automatic shader warm-up skipped"),
        }
    } else {
        tracing::info!("automatic shader warm-up skipped (DEKUBI_AUTO_WARM=0)");
    }

    if do_prebuild {
        phase.store(1, Ordering::Relaxed);
        #[cfg(windows)]
        {
            let pb_began = std::time::Instant::now();
            tracing::info!(
                exe = %exe.display(),
                "starting automatic GPU pipeline pre-build"
            );
            match prebuild::run_for_launch(exe) {
                Ok(()) => {
                    let [total, _done, made, cached] = prebuild::progress();
                    tracing::info!(
                        total,
                        made,
                        cached,
                        elapsed_s = format_args!("{:.2}", pb_began.elapsed().as_secs_f64()),
                        "automatic GPU pipeline pre-build finished"
                    );
                }
                Err(e) => tracing::warn!(error = %e, "automatic GPU pipeline pre-build skipped"),
            }
        }
    } else {
        tracing::info!("automatic GPU pipeline pre-build skipped (DEKUBI_AUTO_PREBUILD=0)");
    }

    drop(window);
}

/// Where refused pipelines' shaders are saved (`DEKUBI_REFUSED_DIR`,
/// `ally_gpu` pipeline/host.rs): `refused-shaders` beside the run's log.
fn refused_dir(log_path: Option<&Path>) {
    if std::env::var_os("DEKUBI_REFUSED_DIR").is_some() {
        return;
    }
    if let Some(dir) = log_path.and_then(Path::parent) {
        // SAFETY: set before the title or any thread of ours reads the environment.
        unsafe { std::env::set_var("DEKUBI_REFUSED_DIR", dir.join("refused-shaders")) };
    }
}

/// What the progress window says during the shader warm-up (phase 0) and
/// GPU pipeline pre-build (phase 1).
fn warm_text(phase: u8, seconds: u64) -> String {
    if phase == 1 {
        #[cfg(windows)]
        let [total, done, made, cached] = prebuild::progress();
        #[cfg(not(windows))]
        let [total, done, made, cached] = [0usize; 4];
        if total == 0 {
            return format!(
                "Pre-building GPU pipelines ({seconds} s)\n\n\
                 Scanning shader pack...\n\
                 Already-built pipelines are skipped automatically."
            );
        }
        return format!(
            "Pre-building GPU pipelines ({seconds} s)\n\n\
             Pre-building GPU pipelines: {done} / {total} ({cached} cached, {made} new)\n\
             Already-built pipelines are skipped automatically."
        );
    }
    #[cfg(windows)]
    let [seen, translated, cached, failed] = translate::warm_progress();
    #[cfg(not(windows))]
    let [seen, translated, cached, failed] = [0usize; 4];
    let done = translated + cached + failed;
    format!(
        "Preparing shaders before the game starts ({seconds} s)\n\n\
         {done} of up to {seen} ready: {translated} new, {cached} already prepared\n\
         After an update this can take about two minutes.",
    )
}

fn show_notes() {
    let data = ally_core::Config::load()
        .map(|c| c.data_dir)
        .unwrap_or_default();
    println!(
        "{}",
        ally_core::notes::render(&ally_core::notes::load(&data))
    );
}

fn find_default_title_exe() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(cfg) = ally_core::Config::load()
        && let Some(dir) = cfg.game_dir
    {
        candidates.push(dir);
    }
    if let Ok(self_exe) = std::env::current_exe()
        && let Some(exe_dir) = self_exe.parent()
    {
        candidates.push(exe_dir.to_path_buf());
        if let Ok(entries) = std::fs::read_dir(exe_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    candidates.push(p);
                }
            }
        }
        // When launched from `<repo>/target/release/`, also check siblings of `<repo>`.
        if let Some(parent) = exe_dir
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            && let Ok(entries) = std::fs::read_dir(parent)
        {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    candidates.push(p);
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }

    for dir in candidates {
        if let Some(exe) = find_gdk_exe_in(&dir) {
            return Ok(exe);
        }
    }
    anyhow::bail!(
        "could not find an Xbox Series X|S title folder (containing MicrosoftGame.Config). \
         Place Dekubi X.exe next to your game folder, configure dekubi-x.toml, or run:\n  \
         \"Dekubi X.exe\" run <path-to-title.exe> --execute"
    )
}

fn find_gdk_exe_in(dir: &std::path::Path) -> Option<PathBuf> {
    let config_path = dir.join("MicrosoftGame.Config");
    if !config_path.is_file() {
        return None;
    }
    if let Ok(xml) = std::fs::read_to_string(&config_path)
        && let Some(name) = parse_gdk_executable_name(&xml)
    {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    let self_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_ascii_lowercase()));
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let p = entry.path();
        let is_exe = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
        let is_self = p
            .file_name()
            .is_some_and(|n| Some(n.to_ascii_lowercase()) == self_name);
        (p.is_file() && is_exe && !is_self).then_some(p)
    })
}

fn parse_gdk_executable_name(xml: &str) -> Option<&str> {
    let idx = xml.find("<Executable")?;
    let rest = &xml[idx..];
    let end = rest.find('>')?;
    let tag = &rest[..end];
    let name_idx = tag.find("Name=")?;
    let after = &tag[name_idx + 5..];
    let quote = after.as_bytes().first().copied()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let inner = &after[1..];
    let close = inner.find(quote as char)?;
    Some(&inner[..close])
}

fn show_config() -> Result<()> {
    let cfg = ally_core::Config::load()?;
    let status = |p: &Option<PathBuf>| match p {
        Some(p) if p.exists() => format!("{}", p.display()),
        Some(p) => format!("{}  (does not exist)", p.display()),
        None => "(not set)".to_owned(),
    };
    println!(
        "config file : {}",
        cfg.source
            .as_ref()
            .map_or("(none)".into(), |p| p.display().to_string())
    );
    println!("game_dir    : {}", status(&cfg.game_dir));
    println!("cache_dir   : {}", cfg.cache_dir.display());
    println!("data_dir    : {}", cfg.data_dir.display());
    for t in &cfg.test_titles {
        println!("test_title  : {}", status(&Some(t.clone())));
    }
    for p in cfg
        .game_dir
        .iter()
        .chain(&cfg.test_titles)
        .chain([&cfg.cache_dir, &cfg.data_dir])
    {
        if let Err(e) = ally_core::ensure_outside_repo(p) {
            println!("POLICY VIOLATION: {e}");
        }
    }
    Ok(())
}

/// Paths given on the command line, or the configured game + test titles.
pub(crate) fn default_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    if !paths.is_empty() {
        return Ok(paths);
    }
    let cfg = ally_core::Config::load()?;
    let all: Vec<PathBuf> = cfg.game_dir.into_iter().chain(cfg.test_titles).collect();
    anyhow::ensure!(
        !all.is_empty(),
        "no paths given and none configured (see dekubi-x.example.toml)"
    );
    Ok(all)
}
