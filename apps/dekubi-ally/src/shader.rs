// Adapted from victory-fork (https://github.com/fxceless/victory-fork, commit 9376fd6 + 106 VIctory fixes), a fork of
// VIctory (https://github.com/Marvisak/VIctory). GPL-3.0-or-later, by Marvisak, fxceless
// and contributors. Renamed for Dekubi X.
//! Survey shader containers: bitcode, console additions and native code.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::PathBuf;

use ally_game::sgd5;
use ally_shader::gfx10::{self, Instruction};
use ally_shader::{Container, DxilProgram, FourCC, HwStage, ShaderError, native_programs};
use anyhow::Result;

/// Files above this size are skipped (read whole into memory for now).
pub(crate) const MAX_FILE: u64 = 2 << 30;

/// A shader container in a file: as is, or packed in an SGD5 record.
pub(crate) enum Source<'a> {
    Raw { offset: usize, data: &'a [u8] },
    Packed(sgd5::Record<'a>),
}

impl Source<'_> {
    /// The container bytes; `None` if a packed record is corrupt.
    pub(crate) fn bytes(&self) -> Option<Cow<'_, [u8]>> {
        match self {
            Source::Raw { data, .. } => Some(Cow::Borrowed(data)),
            Source::Packed(r) => r.unpack().map(Cow::Owned),
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Source::Raw { offset, .. } => format!("@{offset:#x}"),
            Source::Packed(r) => format!("@{:#x} {}", r.offset, r.name.unwrap_or("?")),
        }
    }
}

/// The packed records of SGD5 data, and every container stored as is (the
/// executable keeps some records uncompressed), found by its magic.
pub(crate) fn sources(data: &[u8]) -> Vec<Source<'_>> {
    let mut out: Vec<Source<'_>> = Vec::new();
    if sgd5::is_sgd5(data) {
        out.extend(sgd5::records(data).map(Source::Packed));
    }
    let raw = memchr::memmem::find_iter(data, b"DXBC")
        .filter(|&offset| Container::parse(&data[offset..]).is_ok())
        .map(|offset| Source::Raw {
            offset,
            data: &data[offset..],
        });
    out.extend(raw);
    out
}

#[derive(Default)]
struct Totals {
    containers: usize,
    with_dxil_part: usize,
    with_bitcode: usize,
    standard_layout: usize,
    xdat: usize,
    xbop: usize,
    kinds: BTreeMap<String, usize>,
    with_native: usize,
    native_stages: BTreeMap<HwStage, usize>,
    native_errors: BTreeMap<String, usize>,
    decoded: usize,
    instructions: usize,
    unnamed: BTreeMap<String, usize>,
    decode_errors: BTreeMap<&'static str, usize>,
    files_with_shaders: usize,
}

#[derive(Default)]
struct RtSurvey {
    programs: usize,
    wave64: usize,
    wave32: usize,
    bvh64_sites: usize,
    bvh32_sites: usize,
    uses_lds: usize,
    /// Cluster key: (stage, wave64, bvh_sites_in_prog, uses_lds, backward_branches_around_bvh, inst_bucket)
    clusters: BTreeMap<(String, bool, usize, bool, usize), usize>,
    samples: Vec<String>,
}

pub fn run(
    paths: Vec<PathBuf>,
    verbose: bool,
    disasm: bool,
    rt_survey: bool,
    filter: Option<&str>,
) -> Result<()> {
    let paths = crate::default_paths(paths)?;
    let mut t = Totals::default();
    let mut rt = RtSurvey::default();
    for root in &paths {
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE {
                eprintln!("skipping large file {}", path.display());
                continue;
            }
            let Ok(data) = std::fs::read(path) else {
                continue;
            };
            let mut found_here = 0;
            for source in sources(&data) {
                if let Some(f) = filter
                    && !source.label().contains(f)
                {
                    continue;
                }
                let Some(bytes) = source.bytes() else {
                    eprintln!("corrupt record {}{}", path.display(), source.label());
                    continue;
                };
                let Ok(container) = Container::parse(&bytes) else {
                    continue;
                };
                found_here += 1;
                t.containers += 1;
                let dxil = container.part(FourCC::DXIL);
                t.with_dxil_part += usize::from(dxil.is_some());
                let program = dxil.and_then(|p| DxilProgram::parse(p.data).ok());
                let mut listing = Vec::new();
                // Independent of the bitcode: titles may strip it.
                let native = match dxil.map(|p| native_programs(p.data)) {
                    Some(Ok(programs)) if !programs.is_empty() => {
                        t.with_native += 1;
                        let mut list = Vec::new();
                        for n in &programs {
                            *t.native_stages.entry(n.stage).or_default() += 1;
                            list.push(format!("{:?} {} B", n.stage, n.code.len()));
                            if disasm {
                                listing.push(format!("    {:?} stage I/O: {:?}", n.stage, n.io));
                                listing.extend(n.layout.iter().map(|p| format!("    {p:?}")));
                                listing.extend(n.used.iter().map(|u| format!("    {u:?}")));
                            }
                            match gfx10::decode(&n.code) {
                                Ok(insts) => {
                                    t.decoded += 1;
                                    t.instructions += insts.len();
                                    for i in insts.iter().filter(|i| i.name().is_none()) {
                                        let key = format!("{:?} {:#x}", i.encoding, i.opcode);
                                        *t.unnamed.entry(key).or_default() += 1;
                                    }
                                    if rt_survey {
                                        let mut b64 = 0usize;
                                        let mut b32 = 0usize;
                                        let mut lds = false;
                                        let mut bvh_offsets = Vec::new();
                                        let mut back_branches = Vec::new();
                                        for i in &insts {
                                            match i.name() {
                                                Some("image_bvh64_intersect_ray") => {
                                                    b64 += 1;
                                                    bvh_offsets.push(i.offset);
                                                }
                                                Some("image_bvh_intersect_ray") => {
                                                    b32 += 1;
                                                    bvh_offsets.push(i.offset);
                                                }
                                                Some(name) if name.starts_with("ds_") => {
                                                    lds = true;
                                                }
                                                Some(name)
                                                    if name.starts_with("s_cbranch")
                                                        || name == "s_branch" =>
                                                {
                                                    let simm = (i.words[0] & 0xffff) as i16 as i32;
                                                    if simm < 0 {
                                                        let target = (i.offset as i64)
                                                            + 4
                                                            + i64::from(simm) * 4;
                                                        if target >= 0 {
                                                            back_branches
                                                                .push((target as usize, i.offset));
                                                        }
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                        let total_sites = b64 + b32;
                                        if total_sites > 0 {
                                            rt.programs += 1;
                                            if n.wave64 {
                                                rt.wave64 += 1;
                                            } else {
                                                rt.wave32 += 1;
                                            }
                                            rt.bvh64_sites += b64;
                                            rt.bvh32_sites += b32;
                                            if lds {
                                                rt.uses_lds += 1;
                                            }
                                            let enclosing_loops = back_branches
                                                .iter()
                                                .filter(|&&(lo, hi)| {
                                                    bvh_offsets.iter().any(|&o| o >= lo && o <= hi)
                                                })
                                                .count();
                                            let key = (
                                                format!("{:?}", n.stage),
                                                n.wave64,
                                                total_sites,
                                                lds,
                                                enclosing_loops,
                                            );
                                            *rt.clusters.entry(key).or_default() += 1;
                                            if rt.samples.len() < 20 {
                                                rt.samples.push(format!(
                                                    "  {} {:?} wave{} insts={} bvh64={} bvh32={} lds={} loops={}",
                                                    source.label(),
                                                    n.stage,
                                                    if n.wave64 { 64 } else { 32 },
                                                    insts.len(),
                                                    b64,
                                                    b32,
                                                    lds,
                                                    enclosing_loops,
                                                ));
                                            }
                                        }
                                    }
                                    if disasm {
                                        listing.extend(
                                            insts.iter().map(|i| format_instruction(i, n.wave64)),
                                        );
                                    }
                                }
                                Err(e) => {
                                    let reason = match e {
                                        ShaderError::Isa { reason, .. } => reason,
                                        _ => "other",
                                    };
                                    *t.decode_errors.entry(reason).or_default() += 1;
                                    list.push(format!("({e})"));
                                }
                            }
                        }
                        format!(", native {}", list.join(" "))
                    }
                    Some(Err(e)) => {
                        *t.native_errors.entry(e.to_string()).or_default() += 1;
                        format!(", native: {e}")
                    }
                    _ => String::new(),
                };
                let line = match &program {
                    Some(p) => {
                        t.with_bitcode += 1;
                        let x = p.xbox_extras();
                        t.standard_layout += usize::from(p.is_standard_layout());
                        t.xdat += usize::from(x.xdat);
                        t.xbop += usize::from(x.xbop);
                        *t.kinds.entry(format!("{:?}", p.kind)).or_default() += 1;
                        format!(
                            "{:?} sm{}.{} bitcode {} B, xdat {}, xbop {}",
                            p.kind,
                            p.shader_model.0,
                            p.shader_model.1,
                            p.bitcode.len(),
                            x.xdat,
                            x.xbop
                        )
                    }
                    None => "no parsable DXIL program".to_owned(),
                } + &native;
                if verbose {
                    println!("{}{}: {line}", path.display(), source.label());
                    for l in &listing {
                        println!("{l}");
                    }
                }
            }
            t.files_with_shaders += usize::from(found_here > 0);
        }
    }
    println!(
        "containers found       {} (in {} files)",
        t.containers, t.files_with_shaders
    );
    println!("  with DXIL part       {}", t.with_dxil_part);
    println!("  with LLVM bitcode    {}", t.with_bitcode);
    println!("  PC-standard layout   {}", t.standard_layout);
    println!("  XDAT prefix          {}", t.xdat);
    println!("  xbop trailer         {}", t.xbop);
    for (k, n) in &t.kinds {
        println!("  {k:<20} {n}");
    }
    println!("  with native code     {}", t.with_native);
    for (stage, n) in &t.native_stages {
        println!("    {:<18} {n}", format!("{stage:?}"));
    }
    for (e, n) in &t.native_errors {
        println!("  native unreadable    {n}: {e}");
    }
    if t.with_native > 0 {
        println!(
            "  decoded programs     {} ({} instructions)",
            t.decoded, t.instructions
        );
        for (e, n) in &t.decode_errors {
            println!("  decode failed        {n}: {e}");
        }
        let mut unnamed: Vec<_> = t.unnamed.iter().collect();
        unnamed.sort_by_key(|&(_, n)| std::cmp::Reverse(*n));
        for (op, n) in unnamed.iter().take(10) {
            println!("  opcode not in table  {op} x{n}");
        }
    }
    if rt_survey {
        println!();
        println!("=== DXR Stage 3 RT-Shader Survey ===");
        println!(
            "  RT programs found    {} (wave64: {}, wave32: {})",
            rt.programs, rt.wave64, rt.wave32
        );
        println!(
            "  BVH intersect sites  bvh64={}, bvh32={}, programs with LDS={}",
            rt.bvh64_sites, rt.bvh32_sites, rt.uses_lds
        );
        let mut clusters: Vec<_> = rt.clusters.iter().collect();
        clusters.sort_by_key(|&(_, count)| std::cmp::Reverse(*count));
        println!(
            "  Loop/shape clusters  (stage, wave64, bvh_sites, uses_lds, enclosing_loops) -> count:"
        );
        for ((stage, w64, sites, lds, loops), count) in clusters {
            println!(
                "    ({stage}, wave{}, sites={sites}, lds={lds}, loops={loops}) -> {count}",
                if *w64 { 64 } else { 32 }
            );
        }
        if !rt.samples.is_empty() {
            println!("  Sample RT programs:");
            for s in &rt.samples {
                println!("{s}");
            }
        }
    }
    if t.containers == 0 {
        println!("(none found: shaders may sit in compressed or encrypted archives)");
    }
    Ok(())
}

fn format_instruction(i: &Instruction, wave64: bool) -> String {
    let words: Vec<_> = i.words.iter().map(|w| format!("{w:08X}")).collect();
    let text = match i.name() {
        Some(_) => i.text(wave64),
        None => format!("<{:?} {:#x}>", i.encoding, i.opcode),
    };
    format!("    {:06x}: {:<44} {text}", i.offset, words.join(" "))
}

#[cfg(test)]
mod rt_native_tests {
    use super::*;

    #[test]
    fn rt_native_rays_validate_all_shaders_in_rpf() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(rt_native_rays_validate_all_shaders_in_rpf_inner)
            .expect("spawn")
            .join()
            .expect("join");
    }

    fn rt_native_rays_validate_all_shaders_in_rpf_inner() {
        let rpf =
            std::path::Path::new(r"C:\Users\User\Dekubi Box Project\Dekubi Ally X\shaders.rpf");
        if !rpf.exists() {
            return;
        }
        let data = std::fs::read(rpf).expect("read shaders.rpf");
        let mut validator = ally_shader::emit::validator::Validator::load(
            ally_shader::emit::validator::Validator::default_path().as_deref(),
        )
        .ok();
        // SAFETY: single-threaded test invocation for offline RT shader survey.
        unsafe {
            std::env::set_var("DEKUBI_NATIVE_RAYS", "1");
        }
        let mut total_rt = 0usize;
        let mut native_matched = 0usize;
        let mut fallback_ok = 0usize;
        let mut translate_err = 0usize;
        let mut invalid_dxil = 0usize;
        let filter = std::env::var("DEKUBI_NATIVE_RAYS_TEST_FILTER").ok();
        for source in sources(&data) {
            if let Some(f) = &filter
                && !source.label().contains(f.as_str())
            {
                continue;
            }
            let Some(bytes) = source.bytes() else {
                continue;
            };
            let Ok(container) = Container::parse(&bytes) else {
                continue;
            };
            let Some(dxil) = container.part(FourCC::DXIL) else {
                continue;
            };
            let Ok(progs) = native_programs(dxil.data) else {
                continue;
            };
            let is_rt = progs.iter().any(|n| {
                gfx10::decode(&n.code)
                    .map(|insts| {
                        insts.iter().any(|i| {
                            matches!(
                                i.name(),
                                Some("image_bvh64_intersect_ray" | "image_bvh_intersect_ray")
                            )
                        })
                    })
                    .unwrap_or(false)
            });
            if !is_rt {
                continue;
            }
            total_rt += 1;
            match ally_shader::emit::translate(&bytes) {
                Ok(shader) => {
                    let is_native = shader.root_srvs.contains(&ally_shader::emit::SLOT_TLAS);
                    if is_native {
                        native_matched += 1;
                        println!("  [NATIVE]   {}", source.label());
                    } else {
                        fallback_ok += 1;
                    }
                    if let Some(v) = validator.as_mut()
                        && let Err(err) = v.validate(&mut shader.dxil.clone())
                    {
                        invalid_dxil += 1;
                        eprintln!(
                            "  [INVALID DXIL] {} (native={is_native}): {err}",
                            source.label()
                        );
                    }
                }
                Err(err) => {
                    translate_err += 1;
                    eprintln!("  [ERR]      {}: {err}", source.label());
                }
            }
        }
        unsafe {
            std::env::remove_var("DEKUBI_NATIVE_RAYS");
        }
        println!(
            "RT native summary: total={total_rt}, native_matched={native_matched}, fallback_ok={fallback_ok}, translate_err={translate_err}, invalid_dxil={invalid_dxil}"
        );
        assert_eq!(
            invalid_dxil, 0,
            "native ray rewrite must NEVER emit invalid DXIL"
        );
    }
}
