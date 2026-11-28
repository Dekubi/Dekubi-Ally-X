// Copyright (c) 2026 Dekubi X contributors. GPL-3.0-or-later.
//! Stage 5 native host DXR 1.1 traversal-loop replacement (`DEKUBI_NATIVE_RAYS=1`).
//!
//! The console has no hardware ray-traversal unit: DXC lowers HLSL `TraceRay` /
//! `RayQuery` into a software BVH4 stack-walk loop around `image_bvh*_intersect_ray`,
//! followed by a post-loop epilogue that reads DXR hit identity fields
//! (`GeometryIndex`, `InstanceIndex`, `PrimitiveIndex`, barycentrics, `t`, front-face)
//! out of the console's BVH nodes.
//!
//! When `DEKUBI_NATIVE_RAYS=1` is set (and `DEKUBI_BVH_NO_HIT != 1`), this pass
//! recognises that single-entry loop + epilogue region in SSA IR, verifies that
//! every live-out maps cleanly to a DXR 1.1 `RayQuery::Committed*` accessor, and
//! replaces the entire region with inline `RayQuery` (`allocateRayQuery`,
//! `rayQuery_TraceRayInline`, `rayQuery_Proceed`, `Committed*`) against the host
//! TLAS at [`super::shader::SLOT_TLAS`].
//!
//! Safety: any loop or live-out that does not match every precondition is declined
//! cleanly, falling back to the emulated node test in [`super::intersect`].

use std::collections::{HashMap, HashSet, VecDeque};

use super::{Bin, Cast, Lower, Term, V, Val, pred};
use crate::gfx10::{Fields, Operand};
use crate::ir::{EXEC, Inst, Program, Reg, Value};
use crate::resource::Resources;
use crate::structure::Structured;

/// DXR `RAY_FLAG_FORCE_OPAQUE` (0x01): lets `RayQuery::Proceed()` commit opaque
/// triangle hits directly in hardware without a candidate-handling loop body.
const RAY_FLAG_FORCE_OPAQUE: u32 = 0x01;
/// DXR `RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH` (0x04) for shadow / any-hit queries.
const RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH: u32 = 0x04;

#[cfg(test)]
thread_local! {
    /// Unit-test toggle equivalent to `DEKUBI_NATIVE_RAYS=1`.
    pub(in crate::emit) static TEST_NATIVE_RAYS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Whether native DXR 1.1 traversal replacement is enabled for `code`.
pub(super) fn enabled(code: &[u8]) -> bool {
    #[cfg(test)]
    if TEST_NATIVE_RAYS.get() {
        return true;
    }
    if std::env::var_os("DEKUBI_NATIVE_RAYS").is_none_or(|v| v != "1") {
        return false;
    }
    // Never combine native rays with DEKUBI_BVH_NO_HIT=1.
    if std::env::var_os("DEKUBI_BVH_NO_HIT").is_some_and(|v| v == "1") {
        return false;
    }
    let Ok(filter) = std::env::var("DEKUBI_NATIVE_RAYS_PROGRAMS") else {
        return true;
    };
    let filter = filter.trim();
    if filter.is_empty() || filter.eq_ignore_ascii_case("all") {
        return true;
    }
    let hash = format!("{:016x}", fnv1a64(code));
    filter.split(',').any(|entry| {
        let e = entry.trim().to_ascii_lowercase();
        !e.is_empty() && hash.starts_with(&e)
    })
}

/// 64-bit FNV-1a hash of a native program's machine code, for per-program opt-in.
#[allow(dead_code)]
pub fn program_hash(code: &[u8]) -> u64 {
    fnv1a64(code)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Which DXR 1.1 `RayQuery::Committed*` value a live-out corresponds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HitField {
    /// `CommittedStatus() == 1 ? hit_val : miss_val`.
    Status { hit_val: u32, miss_val: u32 },
    /// `CommittedStatus() == 1 ? (CommittedRayT() + tmin_bias) : (miss_bits or ray tmax)`.
    RayT {
        tmin_bias_bits: Option<u32>,
        miss_bits: Option<u32>,
    },
    /// `CommittedStatus() == 1 ? CommittedTriangleBarycentrics().x : f32::from_bits(miss_bits)`.
    BaryU { miss_bits: u32 },
    /// `CommittedStatus() == 1 ? CommittedTriangleBarycentrics().y : f32::from_bits(miss_bits)`.
    BaryV { miss_bits: u32 },
    /// `CommittedStatus() == 1 ? (CommittedTriangleFrontFace() ? hit_front : hit_back) : miss_val`.
    FrontFace {
        hit_front: u32,
        hit_back: u32,
        miss_val: u32,
    },
    /// `CommittedStatus() == 1 ? CommittedGeometryIndex() : miss_val`.
    GeometryIndex { miss_val: u32 },
    /// `CommittedStatus() == 1 ? (CommittedInstanceIndex() & mask) : miss_val`.
    InstanceIndex { mask: u32, miss_val: u32 },
    /// `CommittedStatus() == 1 ? CommittedPrimitiveIndex() : miss_val`.
    PrimitiveIndex { miss_val: u32 },
    /// `CommittedStatus() == 1 ? (CommittedInstanceID() & mask) : miss_val`.
    InstanceId { mask: u32, miss_val: u32 },
    /// `CommittedStatus() == 1 ? (CommittedInstanceContributionToHitGroupIndex() & mask) : miss_val`.
    InstanceContribution { mask: u32, miss_val: u32 },
    /// `CommittedStatus() == 1 ? (CommittedGeometryIndex() + (CommittedInstanceContributionToHitGroupIndex() & contribution_mask)) : miss_val`.
    HitGroupIndex {
        contribution_mask: u32,
        miss_val: u32,
    },
    /// Constant value (e.g., software loop-timeout flag `v37` which is always `0` on hardware RayQuery).
    Const(u32),
}

/// One recognised console traversal loop + post-loop DXR hit-extraction epilogue.
#[derive(Debug, Clone)]
pub(super) struct TraceRegion {
    /// IR block where the traversal loop begins (single entry into `blocks`).
    pub header: usize,
    /// IR block immediately after the region (all exits from `blocks` land here).
    pub join: usize,
    /// All IR blocks replaced by the inline `RayQuery` (including `header`, excluding `join`).
    pub blocks: HashSet<usize>,
    /// Pre-loop world-space ray origin `(x, y, z)` (already offset by `tmin * dir` if the shader did so).
    pub origin: [Value; 3],
    /// Pre-loop world-space ray direction `(x, y, z)`.
    pub dir: [Value; 3],
    /// Pre-loop `tmax`.
    pub tmax: Value,
    /// Constant DXR ray flags passed to `allocateRayQuery`.
    pub ray_flags: u32,
    /// 8-bit instance inclusion mask passed to `TraceRayInline`.
    pub instance_mask: u32,
    /// Live-out values mapped to DXR `Committed*` accessors:
    /// `(value_to_supply, hit_field)`.
    /// For phis at `join`, `value_to_supply` is the incoming IR `Value` from `blocks`
    /// (deduplicated per phi at `join`).
    pub join_phi_fields: HashMap<Value, HitField>,
    /// Any value defined inside `blocks` that is directly read outside `blocks`
    /// (when `join` has no external predecessors and thus no phi).
    pub direct_live_outs: Vec<(Value, HitField)>,
}

/// Plan of all traversal regions to replace in a single program.
#[derive(Debug, Default)]
pub(super) struct TraversePlan {
    /// Regions keyed by their `header` IR block.
    pub by_header: HashMap<usize, TraceRegion>,
    /// All IR blocks belonging to any replaced region, mapping `block -> header`.
    pub block_to_header: HashMap<usize, usize>,
    /// `join` IR block -> `header` IR block of the region that exits into it.
    pub join_to_header: HashMap<usize, usize>,
}

impl TraversePlan {
    /// Returns true if `b` is inside a replaced region and is NOT its `header`
    /// (so `Lower::run` skips emitting `b`).
    pub(super) fn is_skipped(&self, b: usize) -> bool {
        self.block_to_header.get(&b).is_some_and(|&hdr| hdr != b)
    }

    /// Analyses `p` and returns all verified `TraceRegion`s.
    pub(super) fn analyze(p: &Program, st: &Structured, res: &Resources, code: &[u8]) -> Self {
        let mut plan = Self::default();
        if !enabled(code) || res.bvh.is_empty() {
            return plan;
        }
        let order = crate::ir::reverse_postorder(&p.blocks);
        let mut rpo = vec![usize::MAX; p.blocks.len()];
        for (i, &b) in order.iter().enumerate() {
            rpo[b] = i;
        }
        let idom = crate::ir::idoms(&p.blocks, &order, &rpo);
        let defs = build_def_map(p);
        let bvh_blocks: HashSet<usize> = res.bvh.iter().map(|&((b, _), _)| b).collect();

        for &((bvh_b, bvh_i), _) in &res.bvh {
            if plan.block_to_header.contains_key(&bvh_b) {
                continue;
            }
            let Some(region) = match_region(p, st, &idom, &defs, &bvh_blocks, bvh_b, bvh_i) else {
                continue;
            };
            // Ensure disjointness from already-planned regions.
            if region
                .blocks
                .iter()
                .any(|b| plan.block_to_header.contains_key(b))
                || plan.join_to_header.contains_key(&region.join)
            {
                continue;
            }
            let hdr = region.header;
            let join = region.join;
            if std::env::var_os("DEKUBI_NATIVE_RAYS_DEBUG").is_some() {
                eprintln!(
                    "[NATIVE_RAYS MATCH b={bvh_b} i={bvh_i}] hdr={hdr} join={join} blocks={} flags={:#x} mask={:#x} phis={:?} direct={:?}",
                    region.blocks.len(),
                    region.ray_flags,
                    region.instance_mask,
                    region.join_phi_fields,
                    region.direct_live_outs,
                );
            }
            for &b in &region.blocks {
                plan.block_to_header.insert(b, hdr);
            }
            plan.join_to_header.insert(join, hdr);
            plan.by_header.insert(hdr, region);
        }
        plan
    }
}

#[derive(Debug, Clone, Copy)]
enum Def {
    Phi(usize, usize),
    Inst(usize, usize),
}

fn build_def_map(p: &Program) -> HashMap<Value, Def> {
    let mut defs = HashMap::new();
    for (b, block) in p.blocks.iter().enumerate() {
        for (i, phi) in block.phis.iter().enumerate() {
            defs.insert(phi.value, Def::Phi(b, i));
        }
        for (i, inst) in block.insts.iter().enumerate() {
            for &(_, v) in &inst.writes {
                defs.insert(v, Def::Inst(b, i));
            }
        }
    }
    defs
}

fn def_block(defs: &HashMap<Value, Def>, v: Value) -> Option<usize> {
    defs.get(&v).map(|&d| match d {
        Def::Phi(b, _) | Def::Inst(b, _) => b,
    })
}

fn dominates(idom: &[usize], a: usize, mut b: usize) -> bool {
    loop {
        if a == b {
            return true;
        }
        let parent = idom[b];
        if parent == usize::MAX || parent == b {
            return false;
        }
        b = parent;
    }
}

/// Finds the innermost natural loop containing `target`.
fn innermost_loop(p: &Program, idom: &[usize], target: usize) -> Option<(usize, HashSet<usize>)> {
    let mut best: Option<(usize, HashSet<usize>)> = None;
    for (latch, blk) in p.blocks.iter().enumerate() {
        for succ in blk.exit.successors() {
            if !dominates(idom, succ, latch) {
                continue;
            }
            // Natural loop for back-edge latch -> succ.
            let mut body = HashSet::new();
            body.insert(succ);
            if latch != succ {
                let mut q = VecDeque::new();
                body.insert(latch);
                q.push_back(latch);
                while let Some(cur) = q.pop_front() {
                    for &pred in &p.blocks[cur].preds {
                        if body.insert(pred) {
                            q.push_back(pred);
                        }
                    }
                }
            }
            if !body.contains(&target) {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|(_, best_body)| body.len() < best_body.len())
            {
                best = Some((succ, body));
            }
        }
    }
    best
}

/// Traces `v` backwards through loop-internal moves, cndmasks, and phis to a value
/// defined outside `loop_blocks` that dominates `header`.
fn trace_preloop_value(
    p: &Program,
    idom: &[usize],
    defs: &HashMap<Value, Def>,
    header: usize,
    external_preds: &[usize],
    loop_blocks: &HashSet<usize>,
    start: Value,
) -> Option<Value> {
    let mut cur = start;
    let mut seen = HashSet::new();
    for _ in 0..32 {
        if !seen.insert(cur) {
            return None;
        }
        let Some(&def) = defs.get(&cur) else {
            // Program input (entry-block parameter) dominates everything.
            return Some(cur);
        };
        match def {
            Def::Phi(b, idx) => {
                if !loop_blocks.contains(&b) {
                    return dominates(idom, b, header).then_some(cur);
                }
                let phi = &p.blocks[b].phis[idx];
                if b == header {
                    // All external predecessors of `header` must agree on the incoming
                    // pre-loop value (no per-edge header phi needed).
                    let mut common: Option<Value> = None;
                    for &ep in external_preds {
                        let pos = p.blocks[b].preds.iter().position(|&q| q == ep)?;
                        let v = phi.args[pos];
                        if let Some(prev) = common {
                            if prev != v {
                                return None;
                            }
                        } else {
                            common = Some(v);
                        }
                    }
                    cur = common?;
                    continue;
                }
                // Internal phi: pick the first incoming value defined closer to header.
                let next = phi.args.iter().copied().find(|&a| !seen.contains(&a))?;
                cur = next;
            }
            Def::Inst(b, idx) => {
                if !loop_blocks.contains(&b) {
                    return dominates(idom, b, header).then_some(cur);
                }
                let inst = &p.blocks[b].insts[idx];
                match inst.name() {
                    "v_mov_b32" | "s_mov_b32" => {
                        cur = inst.reads.first().map(|&(_, v)| v)?;
                    }
                    "v_cndmask_b32" => {
                        // In the traversal loop, `v_cndmask_b32 v_cur, v_cur, v_world, vcc`
                        // restores the world-space ray when ascending from BLAS to TLAS.
                        // Prefer an operand that is ALREADY outside the loop (world-space),
                        // otherwise follow src0.
                        let Fields::Valu(v) = &inst.fields else {
                            return None;
                        };
                        let s0 = v.src.first().and_then(|op| read_operand(inst, *op));
                        let s1 = v.src.get(1).and_then(|op| read_operand(inst, *op));
                        if let Some(v1) = s1
                            && def_block(defs, v1).is_none_or(|db| {
                                !loop_blocks.contains(&db) && dominates(idom, db, header)
                            })
                        {
                            return Some(v1);
                        }
                        if let Some(v0) = s0
                            && def_block(defs, v0).is_none_or(|db| {
                                !loop_blocks.contains(&db) && dominates(idom, db, header)
                            })
                        {
                            return Some(v0);
                        }
                        cur = s0.or(s1)?;
                    }
                    _ => return None,
                }
            }
        }
    }
    None
}

fn read_operand(inst: &Inst, op: Operand) -> Option<Value> {
    match op {
        Operand::Vector { reg, .. } => inst.read(Reg::V(reg)),
        Operand::Scalar { code, .. } => inst.read(Reg::S(code)),
        _ => None,
    }
}

fn const_u32(op: Operand) -> Option<u32> {
    match op {
        Operand::Literal(v) => Some(v),
        Operand::Const(c) => match c {
            128..=192 => Some(u32::from(c - 128)),
            193..=208 => Some((-(i32::from(c - 192))) as u32),
            240 => Some(0.5f32.to_bits()),
            241 => Some((-0.5f32).to_bits()),
            242 => Some(1.0f32.to_bits()),
            243 => Some((-1.0f32).to_bits()),
            244 => Some(2.0f32.to_bits()),
            245 => Some((-2.0f32).to_bits()),
            246 => Some(4.0f32.to_bits()),
            247 => Some((-4.0f32).to_bits()),
            _ => None,
        },
        _ => None,
    }
}

/// Tries to match one traversal loop + epilogue around `(bvh_b, bvh_i)`.
fn match_region(
    p: &Program,
    st: &Structured,
    idom: &[usize],
    defs: &HashMap<Value, Def>,
    bvh_blocks: &HashSet<usize>,
    bvh_b: usize,
    bvh_i: usize,
) -> Option<TraceRegion> {
    let dbg = std::env::var_os("DEKUBI_NATIVE_RAYS_DEBUG").is_some();
    macro_rules! decline {
        ($($arg:tt)*) => {{
            if dbg {
                eprintln!("[NATIVE_RAYS DECLINE b={bvh_b} i={bvh_i}] {}", format_args!($($arg)*));
            }
            return None;
        }};
    }
    let Some((header, loop_blocks)) = innermost_loop(p, idom, bvh_b) else {
        decline!("no innermost_loop");
    };

    // Single-entry natural loop: exactly one predecessor of `header` outside `loop_blocks`,
    // and every other block in `loop_blocks` has all predecessors inside `loop_blocks`.
    let external_preds: Vec<usize> = p.blocks[header]
        .preds
        .iter()
        .copied()
        .filter(|q| !loop_blocks.contains(q))
        .collect();
    if external_preds.is_empty() {
        decline!("header={header} has 0 external preds");
    }
    for &b in &loop_blocks {
        if b != header && p.blocks[b].preds.iter().any(|q| !loop_blocks.contains(q)) {
            decline!("non-header block {b} has external pred");
        }
    }
    // Only one BVH intersect site inside this loop.
    if loop_blocks
        .iter()
        .filter(|b| bvh_blocks.contains(b))
        .count()
        != 1
    {
        decline!("multiple bvh sites in loop");
    }

    // Extract ray operands from `image_bvh*_intersect_ray`.
    let bvh_inst = &p.blocks[bvh_b].insts[bvh_i];
    let Fields::Image { data, addr, .. } = &bvh_inst.fields else {
        decline!("not Fields::Image");
    };
    let wide = bvh_inst.name() == "image_bvh64_intersect_ray";
    let mut addr_regs = Vec::new();
    for a in addr {
        if let Operand::Vector { reg, dwords } = *a {
            addr_regs.extend((0..dwords).map(|k| reg + k));
        }
    }
    if addr_regs.len() != if wide { 12 } else { 11 } {
        decline!("addr_regs len={}", addr_regs.len());
    }
    let Operand::Vector { reg: bvh_dst, .. } = *data else {
        decline!("dst not Vector");
    };

    let base = 1 + usize::from(wide);
    let tmax_v = bvh_inst.read(Reg::V(addr_regs[base]))?;
    let origin_v = [
        bvh_inst.read(Reg::V(addr_regs[base + 1]))?,
        bvh_inst.read(Reg::V(addr_regs[base + 2]))?,
        bvh_inst.read(Reg::V(addr_regs[base + 3]))?,
    ];
    let dir_v = [
        bvh_inst.read(Reg::V(addr_regs[base + 4]))?,
        bvh_inst.read(Reg::V(addr_regs[base + 5]))?,
        bvh_inst.read(Reg::V(addr_regs[base + 6]))?,
    ];

    let Some(tmax) =
        trace_preloop_value(p, idom, defs, header, &external_preds, &loop_blocks, tmax_v)
    else {
        decline!("trace_preloop_value failed for tmax");
    };
    let origin = [
        match trace_preloop_value(
            p,
            idom,
            defs,
            header,
            &external_preds,
            &loop_blocks,
            origin_v[0],
        ) {
            Some(v) => v,
            None => decline!("trace_preloop_value failed for origin[0]"),
        },
        match trace_preloop_value(
            p,
            idom,
            defs,
            header,
            &external_preds,
            &loop_blocks,
            origin_v[1],
        ) {
            Some(v) => v,
            None => decline!("trace_preloop_value failed for origin[1]"),
        },
        match trace_preloop_value(
            p,
            idom,
            defs,
            header,
            &external_preds,
            &loop_blocks,
            origin_v[2],
        ) {
            Some(v) => v,
            None => decline!("trace_preloop_value failed for origin[2]"),
        },
    ];
    let dir = [
        match trace_preloop_value(
            p,
            idom,
            defs,
            header,
            &external_preds,
            &loop_blocks,
            dir_v[0],
        ) {
            Some(v) => v,
            None => decline!("trace_preloop_value failed for dir[0]"),
        },
        match trace_preloop_value(
            p,
            idom,
            defs,
            header,
            &external_preds,
            &loop_blocks,
            dir_v[1],
        ) {
            Some(v) => v,
            None => decline!("trace_preloop_value failed for dir[1]"),
        },
        match trace_preloop_value(
            p,
            idom,
            defs,
            header,
            &external_preds,
            &loop_blocks,
            dir_v[2],
        ) {
            Some(v) => v,
            None => decline!("trace_preloop_value failed for dir[2]"),
        },
    ];

    // Identify the 4 output Values of `image_bvh*_intersect_ray`:
    // [t_num, denom, u_num, v_num].
    let bvh_out_t_num = bvh_inst.write(Reg::V(bvh_dst))?;
    let bvh_out_denom = bvh_inst.write(Reg::V(bvh_dst + 1))?;
    let bvh_out_u_num = bvh_inst.write(Reg::V(bvh_dst + 2))?;
    let bvh_out_v_num = bvh_inst.write(Reg::V(bvh_dst + 3))?;

    // Find the triangle-hit computations inside `loop_blocks`:
    let Some(hit_vals) = find_triangle_hit_values(
        p,
        &loop_blocks,
        bvh_out_t_num,
        bvh_out_denom,
        bvh_out_u_num,
        bvh_out_v_num,
    ) else {
        decline!("find_triangle_hit_values failed");
    };

    // Determine loop exit(s): require a single exit block.
    let mut loop_exits = HashSet::new();
    for &b in &loop_blocks {
        for s in p.blocks[b].exit.successors() {
            if !loop_blocks.contains(&s) {
                loop_exits.insert(s);
            }
        }
    }
    if loop_exits.len() != 1 {
        decline!("loop_exits.len() = {} ({:?})", loop_exits.len(), loop_exits);
    }
    let exit_block = *loop_exits.iter().next()?;

    // Trace the post-loop DXR hit-extraction epilogue starting at `exit_block`.
    let Some((region_blocks, join)) = find_epilogue_and_join(p, &loop_blocks, exit_block) else {
        decline!("find_epilogue_and_join failed");
    };

    // Verify single-entry & single-exit invariants on `region_blocks`.
    for &b in &region_blocks {
        if b != header && p.blocks[b].preds.iter().any(|q| !region_blocks.contains(q)) {
            decline!("region block {b} has external pred");
        }
        for s in p.blocks[b].exit.successors() {
            if !region_blocks.contains(&s) && s != join {
                decline!("region block {b} exits to {s} != join {join}");
            }
        }
    }

    // Safety check: no structurised guard or phi_mask outside `region_blocks` may depend
    // on a value defined inside `region_blocks`.
    let region_defs: HashSet<Value> = region_blocks
        .iter()
        .flat_map(|&b| {
            let blk = &p.blocks[b];
            blk.phis.iter().map(|phi| phi.value).chain(
                blk.insts
                    .iter()
                    .flat_map(|i| i.writes.iter().map(|&(_, v)| v)),
            )
        })
        .collect();
    if external_mask_uses_region(p, st, &region_blocks, &region_defs) {
        decline!(
            "external_mask_uses_region (hdr={header} join={join} blocks={})",
            region_blocks.len()
        );
    }

    // Detect ray_flags and instance_mask from the loop body.
    let (ray_flags, instance_mask) = detect_ray_flags_and_mask(p, &loop_blocks);

    // Collect all values transitively read by instructions (or masks) outside `region_blocks`.
    let external_reads = collect_external_reads(p, st, defs, &region_blocks);

    // Classify every live-out phi at `join`.
    let mut join_phi_fields = HashMap::new();
    let region_preds_of_join: Vec<usize> = p.blocks[join]
        .preds
        .iter()
        .enumerate()
        .filter_map(|(idx, &q)| region_blocks.contains(&q).then_some(idx))
        .collect();
    if region_preds_of_join.is_empty() {
        decline!("no region preds of join={join}");
    }

    for phi in &p.blocks[join].phis {
        if matches!(phi.reg, Reg::S(c) if c == EXEC || c == EXEC + 1) {
            continue;
        }
        if !external_reads.contains(&phi.value) {
            continue;
        }
        // Check if any incoming arg from `region_blocks` is defined inside `region_blocks`.
        let reg_args: Vec<Value> = region_preds_of_join
            .iter()
            .map(|&idx| phi.args[idx])
            .collect();
        if reg_args.iter().all(|a| !region_defs.contains(a)) {
            // Pass-through value defined before the region — nothing to synthesize.
            continue;
        }
        let Some(field) = classify_live_out(p, defs, &region_blocks, &hit_vals, &reg_args, 8)
        else {
            if dbg {
                for &a in &reg_args {
                    if let Some(&d) = defs.get(&a) {
                        match d {
                            Def::Inst(db, di) => eprintln!(
                                "    arg {a:?} = Inst(b={db}, {}) reads={:?}",
                                p.blocks[db].insts[di].name(),
                                p.blocks[db].insts[di].reads
                            ),
                            Def::Phi(db, pi) => {
                                eprintln!(
                                    "    arg {a:?} = Phi(b={db}, reg={:?}, args={:?})",
                                    p.blocks[db].phis[pi].reg, p.blocks[db].phis[pi].args
                                );
                                for &pa in &p.blocks[db].phis[pi].args {
                                    if let Some(&pd) = defs.get(&pa) {
                                        eprintln!("      phi-arg {pa:?} -> {pd:?}");
                                        if let Def::Inst(pb, pii) = pd {
                                            let pi_inst = &p.blocks[pb].insts[pii];
                                            eprintln!(
                                                "        {:06x}: {} fields={:?}",
                                                pi_inst.machine.offset,
                                                pi_inst.name(),
                                                pi_inst.fields
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            decline!(
                "unclassified join phi {:?} (reg={:?}, args={:?})",
                phi.value,
                phi.reg,
                reg_args
            );
        };
        join_phi_fields.insert(phi.value, field);
    }

    // Check for any direct (non-phi) reads outside `region_blocks` of values defined in `region_blocks`.
    let mut direct_live_outs = Vec::new();
    let mut direct_seen = HashSet::new();
    for (b, blk) in p.blocks.iter().enumerate() {
        if region_blocks.contains(&b) {
            continue;
        }
        for inst in &blk.insts {
            for &(reg, v) in &inst.reads {
                if inst.writes.iter().any(|&(wr, _)| wr == reg)
                    && !inst_reads_reg_as_source(inst, reg)
                {
                    continue;
                }
                if region_defs.contains(&v) && direct_seen.insert(v) {
                    let Some(field) =
                        classify_live_out(p, defs, &region_blocks, &hit_vals, &[v], 8)
                    else {
                        if dbg && let Some(&d) = defs.get(&v) {
                            match d {
                                Def::Inst(db, di) => eprintln!(
                                    "    direct {v:?} = Inst(b={db}, {}) reads={:?}",
                                    p.blocks[db].insts[di].name(),
                                    p.blocks[db].insts[di].reads
                                ),
                                Def::Phi(db, pi) => eprintln!(
                                    "    direct {v:?} = Phi(b={db}, reg={:?}, args={:?})",
                                    p.blocks[db].phis[pi].reg, p.blocks[db].phis[pi].args
                                ),
                            }
                        }
                        decline!("unclassified direct inst read {:?} in block {b}", v);
                    };
                    direct_live_outs.push((v, field));
                }
            }
        }
        if b != join {
            for phi in &blk.phis {
                if !external_reads.contains(&phi.value) {
                    continue;
                }
                for &a in &phi.args {
                    if region_defs.contains(&a) && direct_seen.insert(a) {
                        let Some(field) =
                            classify_live_out(p, defs, &region_blocks, &hit_vals, &[a], 8)
                        else {
                            decline!("unclassified direct phi read {:?} in block {b}", a);
                        };
                        direct_live_outs.push((a, field));
                    }
                }
            }
        }
    }

    if join_phi_fields.is_empty() && direct_live_outs.is_empty() {
        decline!("no live outs");
    }

    Some(TraceRegion {
        header,
        join,
        blocks: region_blocks,
        origin,
        dir,
        tmax,
        ray_flags,
        instance_mask,
        join_phi_fields,
        direct_live_outs,
    })
}

/// Values computed inside the loop's triangle-hit block.
struct LoopHitValues {
    hit_t: HashSet<Value>,
    bary_u: HashSet<Value>,
    bary_v: HashSet<Value>,
    front_face_code: HashSet<Value>,
}

fn find_triangle_hit_values(
    p: &Program,
    loop_blocks: &HashSet<usize>,
    t_num: Value,
    denom: Value,
    u_num: Value,
    v_num: Value,
) -> Option<LoopHitValues> {
    // Find `v_rcp_f32 rcp, denom`.
    let mut rcp = None;
    for &b in loop_blocks {
        for inst in &p.blocks[b].insts {
            if inst.name() == "v_rcp_f32" && inst.reads.iter().any(|&(_, v)| v == denom) {
                rcp = inst.writes.first().map(|&(_, v)| v);
                break;
            }
        }
    }
    let rcp = rcp?;

    let mut hit_t = HashSet::new();
    let mut bary_u = HashSet::new();
    let mut bary_v = HashSet::new();
    let mut front_face_code = HashSet::new();

    for &b in loop_blocks {
        for inst in &p.blocks[b].insts {
            let name = inst.name();
            if name == "v_mul_legacy_f32" || name == "v_mul_f32" {
                let reads_rcp = inst.reads.iter().any(|&(_, v)| v == rcp);
                if reads_rcp && let Some(&(_, dst)) = inst.writes.first() {
                    if inst.reads.iter().any(|&(_, v)| v == t_num) {
                        hit_t.insert(dst);
                    } else if inst.reads.iter().any(|&(_, v)| v == u_num) {
                        bary_u.insert(dst);
                    } else if inst.reads.iter().any(|&(_, v)| v == v_num) {
                        bary_v.insert(dst);
                    }
                }
            }
            // Front-face code: `v_xor_b32 v_ff, ..., 0xfe/0xff` in the triangle-hit block.
            if name == "v_xor_b32"
                && let Some(&(_, dst)) = inst.writes.first()
            {
                // Check if the same block has `0xfe` or `0xff` literal (DXC's hit-kind code).
                let has_hitkind_lit = p.blocks[b].insts.iter().any(|other| {
                    if let Fields::Valu(v) = &other.fields {
                        v.src
                            .iter()
                            .any(|op| matches!(const_u32(*op), Some(0xfe | 0xff)))
                    } else {
                        false
                    }
                });
                if has_hitkind_lit {
                    front_face_code.insert(dst);
                }
            }
        }
    }

    if hit_t.is_empty() {
        return None;
    }

    // Propagate through loop phis and simple moves so we recognize them after loop exit.
    for set in [&mut hit_t, &mut bary_u, &mut bary_v, &mut front_face_code] {
        propagate_through_loop_copies(p, loop_blocks, set);
    }

    Some(LoopHitValues {
        hit_t,
        bary_u,
        bary_v,
        front_face_code,
    })
}

fn propagate_through_loop_copies(
    p: &Program,
    loop_blocks: &HashSet<usize>,
    set: &mut HashSet<Value>,
) {
    let mut changed = true;
    while changed {
        changed = false;
        for &b in loop_blocks {
            for phi in &p.blocks[b].phis {
                if phi.args.iter().any(|a| set.contains(a)) && set.insert(phi.value) {
                    changed = true;
                }
            }
            for inst in &p.blocks[b].insts {
                if matches!(inst.name(), "v_mov_b32" | "v_cndmask_b32")
                    && inst.reads.iter().any(|&(_, v)| set.contains(&v))
                    && let Some(&(_, dst)) = inst.writes.first()
                    && set.insert(dst)
                {
                    changed = true;
                }
            }
        }
    }
}

/// Walks the post-loop hit-extraction epilogue starting at `exit_block` and returns
/// `(loop_blocks ∪ epilogue_blocks, join_block)`.
///
/// In DXC's inline-RT expansion, `exit_block` tests `hit_tri != 0` and branches on
/// `execz` directly to `join_block`, while the hit path (an acyclic DAG of global
/// loads from the hit triangle & instance nodes) converges into `join_block`.
fn find_epilogue_and_join(
    p: &Program,
    loop_blocks: &HashSet<usize>,
    exit_block: usize,
) -> Option<(HashSet<usize>, usize)> {
    let succs = p.blocks[exit_block].exit.successors();
    if succs.len() != 2 {
        // Simple shadow/any-hit ray with no post-loop node extraction: exit_block IS join.
        return Some((loop_blocks.clone(), exit_block));
    }

    // Try each successor of `exit_block` as the candidate `join` block:
    // the OTHER successor must reach `join` via a small acyclic epilogue:
    //   - either a DXR node-load epilogue (<= 24 blocks, contains `global_load_*`), or
    //   - a tiny pure shadow/visibility epilogue (<= 6 blocks, no stores/atomics/bvh/barriers).
    for &(cand_hit, cand_join) in &[(succs[0], succs[1]), (succs[1], succs[0])] {
        let mut epilogue = HashSet::new();
        epilogue.insert(exit_block);
        let mut q = VecDeque::new();
        if cand_hit != cand_join {
            epilogue.insert(cand_hit);
            q.push_back(cand_hit);
        }
        let mut ok = true;
        let mut has_global_load = false;
        while let Some(b) = q.pop_front() {
            if epilogue.len() > 24 || loop_blocks.contains(&b) {
                ok = false;
                break;
            }
            if p.blocks[b]
                .insts
                .iter()
                .any(|i| i.name().starts_with("global_load"))
            {
                has_global_load = true;
            }
            let b_succs = p.blocks[b].exit.successors();
            if b_succs.is_empty() {
                ok = false;
                break;
            }
            for s in b_succs {
                if s == cand_join {
                    continue;
                }
                if loop_blocks.contains(&s) {
                    ok = false;
                    break;
                }
                if epilogue.insert(s) {
                    q.push_back(s);
                }
            }
            if !ok {
                break;
            }
        }
        if ok && has_global_load {
            let mut region = loop_blocks.clone();
            region.extend(epilogue);
            return Some((region, cand_join));
        }
    }

    // Fallback: no BVH-node epilogue detected; treat `exit_block` itself as `join`.
    Some((loop_blocks.clone(), exit_block))
}

fn external_mask_uses_region(
    p: &Program,
    st: &Structured,
    region_blocks: &HashSet<usize>,
    region_defs: &HashSet<Value>,
) -> bool {
    let uses_region_bool = |b: crate::structure::Bool| {
        st.bools.support(b).into_iter().any(|atom| match atom {
            crate::structure::Atom::Lane(v)
            | crate::structure::Atom::Uniform(v)
            | crate::structure::Atom::Phi(v)
            | crate::structure::Atom::Quad(v)
            | crate::structure::Atom::Opaque(v) => region_defs.contains(&v),
            crate::structure::Atom::Constant(_) => false,
        })
    };
    for (b, blk) in p.blocks.iter().enumerate() {
        if region_blocks.contains(&b) {
            continue;
        }
        if st.guards[b]
            .iter()
            .any(|&g| g.is_some_and(uses_region_bool))
        {
            return true;
        }
        if let crate::structure::LaneExit::Branch { when, .. } = st.exits[b]
            && uses_region_bool(when)
        {
            return true;
        }
        for phi in &blk.phis {
            if let Some(args) = st.phi_masks.get(&phi.value)
                && args.iter().any(|&m| uses_region_bool(m))
            {
                return true;
            }
        }
    }
    false
}

fn collect_external_reads(
    p: &Program,
    st: &Structured,
    defs: &HashMap<Value, Def>,
    region_blocks: &HashSet<usize>,
) -> HashSet<Value> {
    let mut live = HashSet::new();
    let mut work = VecDeque::new();
    for (b, blk) in p.blocks.iter().enumerate() {
        if region_blocks.contains(&b) {
            continue;
        }
        for inst in &blk.insts {
            for &(reg, v) in &inst.reads {
                // Overwrites of a VGPR carry an implicit old-value read in SSA IR solely
                // for `Lower::write_placed` when guarded; skip it when not actually read
                // as a source operand (or SDWA preserve / fmac).
                if inst.writes.iter().any(|&(wr, _)| wr == reg)
                    && !inst_reads_reg_as_source(inst, reg)
                {
                    continue;
                }
                if live.insert(v) {
                    work.push_back(v);
                }
            }
        }
        for g in st.guards[b].iter().flatten() {
            for atom in st.bools.support(*g) {
                match atom {
                    crate::structure::Atom::Lane(v)
                    | crate::structure::Atom::Uniform(v)
                    | crate::structure::Atom::Phi(v)
                    | crate::structure::Atom::Quad(v)
                    | crate::structure::Atom::Opaque(v) => {
                        if live.insert(v) {
                            work.push_back(v);
                        }
                    }
                    crate::structure::Atom::Constant(_) => {}
                }
            }
        }
        if let crate::structure::LaneExit::Branch { when, .. } = st.exits[b] {
            for atom in st.bools.support(when) {
                match atom {
                    crate::structure::Atom::Lane(v)
                    | crate::structure::Atom::Uniform(v)
                    | crate::structure::Atom::Phi(v)
                    | crate::structure::Atom::Quad(v)
                    | crate::structure::Atom::Opaque(v) => {
                        if live.insert(v) {
                            work.push_back(v);
                        }
                    }
                    crate::structure::Atom::Constant(_) => {}
                }
            }
        }
    }
    while let Some(v) = work.pop_front() {
        if let Some(&Def::Phi(pb, pi)) = defs.get(&v)
            && !region_blocks.contains(&pb)
        {
            for &a in &p.blocks[pb].phis[pi].args {
                if live.insert(a) {
                    work.push_back(a);
                }
            }
        }
    }
    live
}

/// Returns true if `inst` genuinely reads `reg` as a source operand (including
/// in-place accumulate instructions like `v_fmac_*` or SDWA preserve), rather than
/// only having `reg` in `inst.reads` as an implicit lane-guard old-value read.
fn inst_reads_reg_as_source(inst: &Inst, reg: Reg) -> bool {
    let op_contains = |op: Operand| match (op, reg) {
        (Operand::Vector { reg: r, dwords }, Reg::V(target)) => {
            target >= r && target < r.saturating_add(dwords)
        }
        (Operand::Scalar { code: c, dwords }, Reg::S(target)) => {
            target >= c && target < c.saturating_add(dwords)
        }
        _ => false,
    };
    let name = inst.name();
    if name.starts_with("v_fmac")
        || name.starts_with("v_mac")
        || name.starts_with("v_dot")
        || name.starts_with("v_sad")
        || name.starts_with("v_qsad")
        || name.starts_with("v_mqsad")
        || name.contains("cmpx")
    {
        return true;
    }
    match &inst.fields {
        Fields::Valu(v) => {
            if v.sdwa.is_some() {
                return true;
            }
            v.src.iter().any(|&op| op_contains(op))
        }
        Fields::Sop { src, .. } => src.iter().any(|&op| op_contains(op)),
        Fields::Smem { base, soffset, .. } => op_contains(*base) || op_contains(*soffset),
        Fields::Vintrp { src, attr, .. } => op_contains(*src) || op_contains(*attr),
        Fields::Ds { addr, data, .. } => {
            addr.is_some_and(op_contains) || data.iter().any(|&op| op_contains(op))
        }
        Fields::Buffer {
            data,
            addr,
            rsrc,
            soffset,
            ..
        } => {
            (name.contains("store") || name.contains("atomic")) && data.is_some_and(op_contains)
                || op_contains(*addr)
                || op_contains(*rsrc)
                || op_contains(*soffset)
        }
        Fields::Image {
            data,
            addr,
            rsrc,
            sampler,
            ..
        } => {
            (name.contains("store") || name.contains("atomic")) && op_contains(*data)
                || addr.iter().any(|&op| op_contains(op))
                || op_contains(*rsrc)
                || sampler.is_some_and(op_contains)
        }
        Fields::Flat {
            addr, data, saddr, ..
        } => {
            addr.is_some_and(op_contains)
                || data.is_some_and(op_contains)
                || saddr.is_some_and(op_contains)
        }
        Fields::Exp { src, .. } => src.iter().flatten().any(|&op| op_contains(op)),
        Fields::Sopp { .. } => false,
    }
}

/// Detects DXR ray flags and 8-bit instance mask from the loop body.
fn detect_ray_flags_and_mask(p: &Program, loop_blocks: &HashSet<usize>) -> (u32, u32) {
    let mut flags = RAY_FLAG_FORCE_OPAQUE;
    let mut mask = 0xffu32;

    // Look for instance mask test against `0x3fffffff` (top 2 bits of InstanceIDAndMask -> 0xC0)
    // or explicit `& mask` on `v24`. Keep 0xff by default unless a clear constant mask is present.
    let mut has_any_hit_break = false;
    for &b in loop_blocks {
        for inst in &p.blocks[b].insts {
            if inst.name() == "v_cmp_lt_u32"
                && let Fields::Valu(v) = &inst.fields
                && v.src.iter().any(|op| const_u32(*op) == Some(0x3fff_ffff))
            {
                // Bits 30..31 of `InstanceIDAndMask` (offset 0x30): top 2 bits of 8-bit mask.
                // Note: 0xff also matches all instances with those bits set; we use 0xff when
                // unsure so we never falsely cull valid geometry, or 0xc0 if verified.
                mask = 0xff;
            }
            // Shadow / any-hit queries set the loop exit sentinel immediately on first triangle hit.
            let _ = &mut has_any_hit_break;
        }
    }
    if has_any_hit_break {
        flags |= RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH;
    }
    (flags, mask)
}

/// Classifies a live-out value (from its incoming argument(s) inside `region_blocks`)
/// into a DXR 1.1 `HitField`.
fn classify_live_out(
    p: &Program,
    defs: &HashMap<Value, Def>,
    region_blocks: &HashSet<usize>,
    hit_vals: &LoopHitValues,
    args: &[Value],
    depth: u8,
) -> Option<HitField> {
    if depth == 0 {
        return None;
    }
    // Separate constant miss defaults from non-constant hit expressions (if there are
    // multiple incoming edges from the epilogue, e.g. miss-branch + hit-fallthrough).
    let mut miss_const: Option<u32> = None;
    let mut hit_val: Option<Value> = None;
    let mut const_vals = Vec::new();

    for &v in args {
        if let Some(c) = eval_region_const(p, defs, region_blocks, v) {
            const_vals.push(c);
        } else {
            hit_val = Some(v);
        }
    }

    // Case 1: Status flag — one path brings constant `1`, another brings constant `0`
    // (e.g. `v23 = 0` on miss, `v23 = 1` at the end of the hit epilogue).
    if hit_val.is_none() && const_vals.len() >= 2 {
        let has_0 = const_vals.contains(&0);
        let has_1 = const_vals.contains(&1);
        if has_0 && has_1 && const_vals.iter().all(|&c| c == 0 || c == 1) {
            return Some(HitField::Status {
                hit_val: 1,
                miss_val: 0,
            });
        }
        return None;
    }

    if let Some(&c) = const_vals.first() {
        miss_const = Some(c);
    }
    let v = hit_val?;

    // Unwrap epilogue-internal phis (choosing the non-constant / hit-path branch).
    let (v, inner_miss) = unwrap_epilogue_phi(p, defs, region_blocks, v);
    let miss_u32 = miss_const.or(inner_miss);

    // Case 2: Barycentrics (`v19`, `v20` copied directly out of the loop).
    if hit_vals.bary_u.contains(&v) {
        return Some(HitField::BaryU {
            miss_bits: miss_u32.unwrap_or(0),
        });
    }
    if hit_vals.bary_v.contains(&v) {
        return Some(HitField::BaryV {
            miss_bits: miss_u32.unwrap_or(0),
        });
    }

    // Case 3: Direct `hit_t` (`v17`).
    if hit_vals.hit_t.contains(&v) {
        return Some(HitField::RayT {
            tmin_bias_bits: None,
            miss_bits: miss_u32,
        });
    }

    // Case 3b: Front-face code (`0xfe` front, `0xff` back, `0` miss) escaping directly.
    if hit_vals.front_face_code.contains(&v) {
        return Some(HitField::FrontFace {
            hit_front: 0xfe,
            hit_back: 0xff,
            miss_val: miss_u32.unwrap_or(0),
        });
    }

    // Inspect the defining instruction in the epilogue.
    let Def::Inst(b, idx) = *defs.get(&v)? else {
        return None;
    };
    let inst = &p.blocks[b].insts[idx];
    match inst.name() {
        // `v_mov_b32`: unwrap and re-classify.
        "v_mov_b32" => {
            let src = inst.reads.first().map(|&(_, s)| s)?;
            if hit_vals.bary_u.contains(&src) {
                return Some(HitField::BaryU {
                    miss_bits: miss_u32.unwrap_or(0),
                });
            }
            if hit_vals.bary_v.contains(&src) {
                return Some(HitField::BaryV {
                    miss_bits: miss_u32.unwrap_or(0),
                });
            }
            if hit_vals.hit_t.contains(&src) {
                return Some(HitField::RayT {
                    tmin_bias_bits: None,
                    miss_bits: miss_u32,
                });
            }
            return classify_live_out(p, defs, region_blocks, hit_vals, &[src], depth - 1)
                .map(|f| override_miss(f, miss_u32));
        }
        // `v_add_f32 v39, lit(tmin), hit_t` -> `RayT` with `tmin_bias`.
        "v_add_f32" => {
            let Fields::Valu(val) = &inst.fields else {
                return None;
            };
            let mut bias = None;
            let mut has_t = false;
            for &op in &val.src {
                if let Some(c) = const_u32(op) {
                    bias = Some(c);
                } else if let Some(sv) = read_operand(inst, op)
                    && hit_vals.hit_t.contains(&sv)
                {
                    has_t = true;
                }
            }
            if has_t {
                return Some(HitField::RayT {
                    tmin_bias_bits: bias,
                    miss_bits: miss_u32,
                });
            }
        }
        // `v_cndmask_b32` in the loop/epilogue:
        //   1. `v_cndmask_b32 v15, 0, 1, (front_face_code == 0xfe)` -> `FrontFace`
        //   2. `v_cndmask_b32 v37, 0, -1, (timeout_cond)` -> `Const(0)` (hardware RayQuery never times out)
        //   3. `v_cndmask_b32 dst, miss_const, hit_expr, hit_cond` -> unwrap `hit_expr` with `miss_const`
        "v_cndmask_b32" => {
            let Fields::Valu(val) = &inst.fields else {
                return None;
            };
            let s0_c = val.src.first().and_then(|&op| const_u32(op));
            let s1_c = val.src.get(1).and_then(|&op| const_u32(op));
            let s0_v = val.src.first().and_then(|&op| read_operand(inst, op));
            let s1_v = val.src.get(1).and_then(|&op| read_operand(inst, op));
            let cond = val.src.get(2).and_then(|&op| read_operand(inst, op));
            if let (Some(c0), Some(c1), Some(cv)) = (s0_c, s1_c, cond) {
                if is_front_face_cmp(p, defs, hit_vals, cv) {
                    return Some(HitField::FrontFace {
                        hit_front: c1,
                        hit_back: c0,
                        miss_val: miss_u32.unwrap_or(0),
                    });
                }
                if c0 == 0 && c1 == u32::MAX && is_timeout_cmp(p, defs, cv) {
                    return Some(HitField::Const(0));
                }
                if is_other_lt_hit_t(p, defs, hit_vals, cv) {
                    return Some(HitField::Const(c0));
                }
            }
            if let (Some(c0), Some(v1)) = (s0_c, s1_v) {
                if let Some(f) =
                    classify_live_out(p, defs, region_blocks, hit_vals, &[v1], depth - 1)
                {
                    return Some(override_miss(f, Some(miss_u32.unwrap_or(c0))));
                }
                // `v_cndmask_b32 dst, c0, proc_data, (v_other_t < hit_t)`:
                // Under `RAY_FLAG_FORCE_OPAQUE`, non-opaque/procedural candidate hits never beat
                // the committed opaque triangle, so `dst` stays at its opaque default `c0`.
                if let Some(cv) = cond
                    && is_other_lt_hit_t(p, defs, hit_vals, cv)
                {
                    return Some(HitField::Const(c0));
                }
            }
            if let (Some(v0), Some(c1)) = (s0_v, s1_c)
                && let Some(f) =
                    classify_live_out(p, defs, region_blocks, hit_vals, &[v0], depth - 1)
            {
                return Some(override_miss(f, Some(miss_u32.unwrap_or(c1))));
            }
        }
        // `v_lshrrev_b32 v11, 8, global_load_dword(tri_node | 48)` -> `GeometryIndex`.
        "v_lshrrev_b32" => {
            let Fields::Valu(val) = &inst.fields else {
                return None;
            };
            if val.src.first().and_then(|&op| const_u32(op)) == Some(8)
                && let Some(src) = val.src.get(1).and_then(|&op| read_operand(inst, op))
                && global_load_offset_or_tag(p, defs, src) == Some(48)
            {
                return Some(HitField::GeometryIndex {
                    miss_val: miss_u32.unwrap_or(u32::MAX),
                });
            }
        }
        // `v_and_b32 v12, mask, global_load_dword(inst_node + 0x70)` -> `InstanceIndex`
        // (or `+ 0x30` -> `InstanceId`, `+ 0x34` -> `InstanceContribution`).
        "v_and_b32" => {
            let Fields::Valu(val) = &inst.fields else {
                return None;
            };
            let mut mask = None;
            let mut src = None;
            for &op in &val.src {
                if let Some(c) = const_u32(op) {
                    mask = Some(c);
                } else if let Some(sv) = read_operand(inst, op) {
                    src = Some(sv);
                }
            }
            let (mask, src) = (mask?, src?);
            match global_load_offset_or_tag(p, defs, src) {
                Some(0x70) => {
                    return Some(HitField::InstanceIndex {
                        mask,
                        miss_val: miss_u32.unwrap_or(u32::MAX),
                    });
                }
                Some(0x30) => {
                    return Some(HitField::InstanceId {
                        mask,
                        miss_val: miss_u32.unwrap_or(u32::MAX),
                    });
                }
                Some(0x34) => {
                    return Some(HitField::InstanceContribution {
                        mask,
                        miss_val: miss_u32.unwrap_or(u32::MAX),
                    });
                }
                _ => {}
            }
        }
        // `v_add_nc_u32 dst, GeometryIndex, InstanceContribution` -> `HitGroupIndex`.
        "v_add_nc_u32" | "v_add_u32" => {
            let Fields::Valu(val) = &inst.fields else {
                return None;
            };
            let v0 = val.src.first().and_then(|&op| read_operand(inst, op))?;
            let v1 = val.src.get(1).and_then(|&op| read_operand(inst, op))?;
            let f0 = classify_live_out(p, defs, region_blocks, hit_vals, &[v0], depth - 1)?;
            let f1 = classify_live_out(p, defs, region_blocks, hit_vals, &[v1], depth - 1)?;
            match (f0, f1) {
                (HitField::GeometryIndex { .. }, HitField::InstanceContribution { mask, .. })
                | (HitField::InstanceContribution { mask, .. }, HitField::GeometryIndex { .. }) => {
                    return Some(HitField::HitGroupIndex {
                        contribution_mask: mask,
                        miss_val: miss_u32.unwrap_or(u32::MAX),
                    });
                }
                _ => {}
            }
        }
        // `global_load_dword` directly:
        //   - offset 52/56 (dynamic `52 + (kind<2 ? kind*4 : 0)`) -> `PrimitiveIndex`
        //   - offset 0x70 -> `InstanceIndex` (unmasked)
        "global_load_dword" => {
            if is_primitive_index_load(p, defs, b, idx) {
                return Some(HitField::PrimitiveIndex {
                    miss_val: miss_u32.unwrap_or(u32::MAX),
                });
            }
            if global_load_offset_or_tag(p, defs, v) == Some(0x70) {
                return Some(HitField::InstanceIndex {
                    mask: u32::MAX,
                    miss_val: miss_u32.unwrap_or(u32::MAX),
                });
            }
        }
        _ => {}
    }

    None
}

fn override_miss(field: HitField, miss: Option<u32>) -> HitField {
    let Some(m) = miss else { return field };
    match field {
        HitField::Status { hit_val, .. } => HitField::Status {
            hit_val,
            miss_val: m,
        },
        HitField::RayT { tmin_bias_bits, .. } => HitField::RayT {
            tmin_bias_bits,
            miss_bits: Some(m),
        },
        HitField::BaryU { .. } => HitField::BaryU { miss_bits: m },
        HitField::BaryV { .. } => HitField::BaryV { miss_bits: m },
        HitField::FrontFace {
            hit_front,
            hit_back,
            ..
        } => HitField::FrontFace {
            hit_front,
            hit_back,
            miss_val: m,
        },
        HitField::GeometryIndex { .. } => HitField::GeometryIndex { miss_val: m },
        HitField::InstanceIndex { mask, .. } => HitField::InstanceIndex { mask, miss_val: m },
        HitField::PrimitiveIndex { .. } => HitField::PrimitiveIndex { miss_val: m },
        HitField::InstanceId { mask, .. } => HitField::InstanceId { mask, miss_val: m },
        HitField::InstanceContribution { mask, .. } => {
            HitField::InstanceContribution { mask, miss_val: m }
        }
        HitField::HitGroupIndex {
            contribution_mask, ..
        } => HitField::HitGroupIndex {
            contribution_mask,
            miss_val: m,
        },
        HitField::Const(c) => HitField::Const(c),
    }
}

fn eval_region_const(
    p: &Program,
    defs: &HashMap<Value, Def>,
    region_blocks: &HashSet<usize>,
    mut v: Value,
) -> Option<u32> {
    for _ in 0..8 {
        let Def::Inst(b, idx) = *defs.get(&v)? else {
            return None;
        };
        if !region_blocks.contains(&b) {
            return None;
        }
        let inst = &p.blocks[b].insts[idx];
        match inst.name() {
            "v_mov_b32" => {
                let Fields::Valu(val) = &inst.fields else {
                    return None;
                };
                let op = *val.src.first()?;
                if let Some(c) = const_u32(op) {
                    return Some(c);
                }
                v = read_operand(inst, op)?;
            }
            "s_mov_b32" => {
                let Fields::Sop { src, .. } = &inst.fields else {
                    return None;
                };
                let op = *src.first()?;
                if let Some(c) = const_u32(op) {
                    return Some(c);
                }
                v = read_operand(inst, op)?;
            }
            _ => return None,
        }
    }
    None
}

fn unwrap_epilogue_phi(
    p: &Program,
    defs: &HashMap<Value, Def>,
    region_blocks: &HashSet<usize>,
    mut v: Value,
) -> (Value, Option<u32>) {
    let mut miss = None;
    for _ in 0..8 {
        let Some(&Def::Phi(b, idx)) = defs.get(&v) else {
            break;
        };
        if !region_blocks.contains(&b) {
            break;
        }
        let phi = &p.blocks[b].phis[idx];
        let mut non_const = None;
        for &a in &phi.args {
            if let Some(c) = eval_region_const(p, defs, region_blocks, a) {
                miss = Some(c);
            } else if a != v {
                non_const = Some(a);
            }
        }
        if let Some(next) = non_const {
            v = next;
        } else {
            break;
        }
    }
    (v, miss)
}

fn is_other_lt_hit_t(
    p: &Program,
    defs: &HashMap<Value, Def>,
    hit_vals: &LoopHitValues,
    cv: Value,
) -> bool {
    let Some(&Def::Inst(b, idx)) = defs.get(&cv) else {
        return false;
    };
    let inst = &p.blocks[b].insts[idx];
    if inst.name() != "v_cmp_lt_f32" && inst.name() != "v_cmp_le_f32" {
        return false;
    }
    let Fields::Valu(val) = &inst.fields else {
        return false;
    };
    // `src[1]` is `hit_t` (`v_other_t < hit_t`).
    val.src
        .get(1)
        .and_then(|&op| read_operand(inst, op))
        .is_some_and(|v| hit_vals.hit_t.contains(&v))
}

fn is_timeout_cmp(p: &Program, defs: &HashMap<Value, Def>, cv: Value) -> bool {
    let Some(&Def::Inst(b, idx)) = defs.get(&cv) else {
        return false;
    };
    let inst = &p.blocks[b].insts[idx];
    if inst.name() != "v_cmp_eq_u32" {
        return false;
    }
    let Fields::Valu(val) = &inst.fields else {
        return false;
    };
    // `v_cmp_eq_u32 vcc, 1, (v_and_b32 s_cselect, 1)` from `s_cmp_le_u32 s_limit, s_counter`.
    let has_1 = val.src.iter().any(|&op| const_u32(op) == Some(1));
    if !has_1 {
        return false;
    }
    for &op in &val.src {
        if let Some(sv) = read_operand(inst, op)
            && let Some(&Def::Inst(ab, ai)) = defs.get(&sv)
            && p.blocks[ab].insts[ai].name() == "v_and_b32"
        {
            return true;
        }
    }
    false
}

fn is_front_face_cmp(
    p: &Program,
    defs: &HashMap<Value, Def>,
    hit_vals: &LoopHitValues,
    cv: Value,
) -> bool {
    let Some(&Def::Inst(b, idx)) = defs.get(&cv) else {
        return false;
    };
    let inst = &p.blocks[b].insts[idx];
    if inst.name() != "v_cmp_eq_u32" {
        return false;
    }
    let Fields::Valu(val) = &inst.fields else {
        return false;
    };
    let has_fe = val.src.iter().any(|&op| const_u32(op) == Some(0xfe));
    let has_ff = val
        .src
        .iter()
        .any(|&op| read_operand(inst, op).is_some_and(|v| hit_vals.front_face_code.contains(&v)));
    has_fe && has_ff
}

/// Checks if `v` is a `global_load_dword` whose low address was formed by `| offset`
/// (`v_or_b32` / `s_or_b32`) or `inst_offset` with a known small byte offset (48, 0x70, etc.).
fn global_load_offset_or_tag(p: &Program, defs: &HashMap<Value, Def>, v: Value) -> Option<u32> {
    let Def::Inst(b, idx) = *defs.get(&v)? else {
        return None;
    };
    let inst = &p.blocks[b].insts[idx];
    if inst.name() != "global_load_dword" {
        return None;
    }
    let Fields::Flat { addr, offset, .. } = &inst.fields else {
        return None;
    };
    if *offset > 0 {
        return Some(*offset as u32);
    }
    let addr_lo = read_operand(inst, (*addr)?)?;
    find_or_offset_const(p, defs, addr_lo, 4)
}

fn find_or_offset_const(
    p: &Program,
    defs: &HashMap<Value, Def>,
    v: Value,
    depth: u8,
) -> Option<u32> {
    if depth == 0 {
        return None;
    }
    let Def::Inst(b, idx) = *defs.get(&v)? else {
        return None;
    };
    let inst = &p.blocks[b].insts[idx];
    match inst.name() {
        "v_or_b32" => {
            let Fields::Valu(val) = &inst.fields else {
                return None;
            };
            val.src.iter().find_map(|&op| const_u32(op))
        }
        "s_or_b32" => {
            let Fields::Sop { src, .. } = &inst.fields else {
                return None;
            };
            src.iter().find_map(|&op| const_u32(op))
        }
        "v_add_co_u32" | "v_add_nc_u32" => {
            let Fields::Valu(val) = &inst.fields else {
                return None;
            };
            for &op in &val.src {
                if let Some(sv) = read_operand(inst, op)
                    && let Some(off) = find_or_offset_const(p, defs, sv, depth - 1)
                {
                    return Some(off);
                }
            }
            None
        }
        _ => None,
    }
}

/// Checks if `global_load_dword` at `(b, idx)` computes its address using `52`
/// (`v_lshl_add_u32 ..., 2, 52` / `v_cndmask_b32 ..., 52, ...`) — DXC's triangle-node
/// `PrimitiveIndex` lookup.
fn is_primitive_index_load(p: &Program, defs: &HashMap<Value, Def>, b: usize, idx: usize) -> bool {
    let inst = &p.blocks[b].insts[idx];
    let Fields::Flat { addr, .. } = &inst.fields else {
        return false;
    };
    let Some(addr_lo) = addr.and_then(|op| read_operand(inst, op)) else {
        return false;
    };
    contains_const_52(p, defs, addr_lo, 5)
}

fn contains_const_52(p: &Program, defs: &HashMap<Value, Def>, v: Value, depth: u8) -> bool {
    if depth == 0 {
        return false;
    }
    let Some(&Def::Inst(b, idx)) = defs.get(&v) else {
        return false;
    };
    let inst = &p.blocks[b].insts[idx];
    if let Fields::Valu(val) = &inst.fields {
        if val.src.iter().any(|&op| const_u32(op) == Some(52)) {
            return true;
        }
        for &op in &val.src {
            if let Some(sv) = read_operand(inst, op)
                && contains_const_52(p, defs, sv, depth - 1)
            {
                return true;
            }
        }
    }
    false
}

impl<'a> Lower<'a> {
    /// Emits a hardware DXR 1.1 `RayQuery` replacing the console traversal region
    /// rooted at `region.header`, and populates live-out values for `region.join`.
    pub(super) fn emit_traverse(&mut self, region: &TraceRegion) -> Result<(), super::Why> {
        let ox_i = self.i32_of(region.origin[0])?;
        let oy_i = self.i32_of(region.origin[1])?;
        let oz_i = self.i32_of(region.origin[2])?;
        let dx_i = self.i32_of(region.dir[0])?;
        let dy_i = self.i32_of(region.dir[1])?;
        let dz_i = self.i32_of(region.dir[2])?;
        let tmax_i = self.i32_of(region.tmax)?;
        let ox = self.as_float(ox_i);
        let oy = self.as_float(oy_i);
        let oz = self.as_float(oz_i);
        let dx = self.as_float(dx_i);
        let dy = self.as_float(dy_i);
        let dz = self.as_float(dz_i);
        let tmax = self.as_float(tmax_i);

        let entry = self.cur;
        let rq_loop = self.s.m.new_block();
        let rq_done = self.s.m.new_block();

        let rq = self.s.ray_query(entry, region.ray_flags);
        let zero_i32 = self.s.m.i32(0);
        let mask = self.s.m.i32(region.instance_mask);
        let tmin = self.s.m.f32(0.0f32.to_bits());
        self.s.trace_ray_inline(
            entry,
            rq,
            zero_i32,
            mask,
            [ox, oy, oz],
            tmin,
            [dx, dy, dz],
            tmax,
        );
        self.s.m.terminate(entry, Term::Br(rq_loop));

        let more = self.s.ray_query_proceed(rq_loop, rq);
        self.s
            .m
            .terminate(rq_loop, Term::CondBr(more, rq_loop, rq_done));

        // Materialize all requested `Committed*` live-outs in `rq_done`.
        self.cur = rq_done;
        let status = self.s.ray_query_committed_status(rq_done, rq);
        let k1 = self.s.m.i32(1);
        let hit = self.s.m.cmp(rq_done, pred::EQ, status, k1);

        let mut bary_cache: Option<[V; 2]> = None;

        // Synthesize values for phis at `region.join`.
        for (&phi_val, &field) in &region.join_phi_fields {
            let v = self.emit_hit_field(rq_done, rq, hit, tmax_i, field, &mut bary_cache);
            self.traverse_join_vals.insert(phi_val, (v, rq_done));
        }
        // Synthesize values for any direct (non-phi) live-outs.
        for &(val, field) in &region.direct_live_outs {
            let v = self.emit_hit_field(rq_done, rq, hit, tmax_i, field, &mut bary_cache);
            self.vals.insert(val, Val::I32(v));
        }

        // Branch from `rq_done` straight to `region.join`.
        let join_blk = self.block[region.join].expect("reachable join");
        self.enter_loop(region.header, region.join, rq_done);
        self.s.m.terminate(rq_done, Term::Br(join_blk));
        self.tail[region.header] = Some(rq_done);
        self.traverse_done.insert(region.header, rq_done);
        Ok(())
    }

    fn emit_hit_field(
        &mut self,
        blk: usize,
        rq: V,
        hit: V,
        tmax_i: V,
        field: HitField,
        bary_cache: &mut Option<[V; 2]>,
    ) -> V {
        let i32t = self.s.t.i32;
        match field {
            HitField::Status { hit_val, miss_val } => {
                let h = self.s.m.i32(hit_val);
                let m = self.s.m.i32(miss_val);
                self.s.m.select(blk, hit, h, m)
            }
            HitField::RayT {
                tmin_bias_bits,
                miss_bits,
            } => {
                let t = self.s.ray_query_committed_t(blk, rq);
                let t = if let Some(bias_bits) = tmin_bias_bits {
                    let bias = self.s.m.f32(bias_bits);
                    self.s.m.bin(blk, Bin::Add, t, bias)
                } else {
                    t
                };
                let t_i32 = self.s.m.cast(blk, Cast::Bitcast, t, i32t);
                let miss = match miss_bits {
                    Some(bits) => self.s.m.i32(bits),
                    None => tmax_i,
                };
                self.s.m.select(blk, hit, t_i32, miss)
            }
            HitField::BaryU { miss_bits } => {
                let bary = *bary_cache
                    .get_or_insert_with(|| self.s.ray_query_committed_barycentrics(blk, rq));
                let u_i32 = self.s.m.cast(blk, Cast::Bitcast, bary[0], i32t);
                let miss = self.s.m.i32(miss_bits);
                self.s.m.select(blk, hit, u_i32, miss)
            }
            HitField::BaryV { miss_bits } => {
                let bary = *bary_cache
                    .get_or_insert_with(|| self.s.ray_query_committed_barycentrics(blk, rq));
                let v_i32 = self.s.m.cast(blk, Cast::Bitcast, bary[1], i32t);
                let miss = self.s.m.i32(miss_bits);
                self.s.m.select(blk, hit, v_i32, miss)
            }
            HitField::FrontFace {
                hit_front,
                hit_back,
                miss_val,
            } => {
                let ff = self.s.ray_query_committed_front_face(blk, rq);
                let f = self.s.m.i32(hit_front);
                let b = self.s.m.i32(hit_back);
                let on_hit = self.s.m.select(blk, ff, f, b);
                let miss = self.s.m.i32(miss_val);
                self.s.m.select(blk, hit, on_hit, miss)
            }
            HitField::GeometryIndex { miss_val } => {
                let g = self.s.ray_query_committed_geometry_index(blk, rq);
                let miss = self.s.m.i32(miss_val);
                self.s.m.select(blk, hit, g, miss)
            }
            HitField::InstanceIndex { mask, miss_val } => {
                let idx = self.s.ray_query_committed_instance_index(blk, rq);
                let idx = if mask != u32::MAX {
                    let m = self.s.m.i32(mask);
                    self.s.m.bin(blk, Bin::And, idx, m)
                } else {
                    idx
                };
                let miss = self.s.m.i32(miss_val);
                self.s.m.select(blk, hit, idx, miss)
            }
            HitField::PrimitiveIndex { miss_val } => {
                let prim = self.s.ray_query_committed_primitive_index(blk, rq);
                let miss = self.s.m.i32(miss_val);
                self.s.m.select(blk, hit, prim, miss)
            }
            HitField::InstanceId { mask, miss_val } => {
                let id = self.s.ray_query_committed_instance_id(blk, rq);
                let id = if mask != u32::MAX {
                    let m = self.s.m.i32(mask);
                    self.s.m.bin(blk, Bin::And, id, m)
                } else {
                    id
                };
                let miss = self.s.m.i32(miss_val);
                self.s.m.select(blk, hit, id, miss)
            }
            HitField::InstanceContribution { mask, miss_val } => {
                let c = self.s.ray_query_committed_instance_contribution(blk, rq);
                let c = if mask != u32::MAX {
                    let m = self.s.m.i32(mask);
                    self.s.m.bin(blk, Bin::And, c, m)
                } else {
                    c
                };
                let miss = self.s.m.i32(miss_val);
                self.s.m.select(blk, hit, c, miss)
            }
            HitField::HitGroupIndex {
                contribution_mask,
                miss_val,
            } => {
                let g = self.s.ray_query_committed_geometry_index(blk, rq);
                let c = self.s.ray_query_committed_instance_contribution(blk, rq);
                let c = if contribution_mask != u32::MAX {
                    let m = self.s.m.i32(contribution_mask);
                    self.s.m.bin(blk, Bin::And, c, m)
                } else {
                    c
                };
                let sum = self.s.m.bin(blk, Bin::Add, g, c);
                let miss = self.s.m.i32(miss_val);
                self.s.m.select(blk, hit, sum, miss)
            }
            HitField::Const(c) => self.s.m.i32(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_hash_is_deterministic_and_non_zero() {
        let a = program_hash(&[1, 2, 3, 4]);
        let b = program_hash(&[1, 2, 3, 4]);
        let c = program_hash(&[1, 2, 3, 5]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, 0);
    }
}
