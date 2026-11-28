// Ported from Kemerd/VIctory-NVIDIA-RT (`crates/victory-gpu/src/pipeline/pieces.rs`),
// a fork of VIctory (https://github.com/Marvisak/VIctory). GPL-3.0-or-later,
// by Kemerd, Marvisak and contributors. Adapted for Dekubi X (`DEKUBI_PIPELINE_PIECES`).

//! Pipelines the title makes from loose pieces: device methods 0x218
//! (compute) and 0x1c8 (graphics). Their descriptions name a root
//! signature of ours and then pieces that lie apart in title memory, each
//! as an address and a length (`ally_shader::pieces` has the layouts of
//! the pieces themselves, and how they were worked out):
//!
//! ```text
//! compute, 0x48 bytes           graphics, 0x108 bytes
//! +0x00 root signature          +0x00 root signature
//! +0x08 packet (0x1c bytes)     +0x08 packet
//! +0x18 0x14 bytes, not read    +0x18 state piece
//! +0x28 code                    +0x28 pixel code, +0x38 pixel metadata
//! +0x38 metadata                +0x48 vertex code, +0x58 vertex metadata
//!                               +0x68 20 words, zero in every call seen
//! ```
//!
//! Off by default unless `DEKUBI_PIPELINE_PIECES=1`.

use std::borrow::Cow;
use std::ffi::CString;
use std::sync::Arc;

use ally_shader::ShaderError;
use ally_shader::pieces::{self, Draft, Graphics, Refusal, Registers};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::PCSTR;

use super::host::GraphicsState;
use super::{Shader, hand_out, shader_of};
use crate::object::{HRESULT, Iid, This};

/// Whether `DEKUBI_PIPELINE_PIECES=1` is set: the switch that makes
/// pipelines from the pieces of the console's two creation methods. Off by
/// default.
pub(crate) fn asked() -> bool {
    static ASKED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ASKED.get_or_init(|| std::env::var("DEKUBI_PIPELINE_PIECES").is_ok_and(|v| v == "1"))
}

/// Reads up to `to.len()` bytes from `src` in this process through the
/// kernel (`ReadProcessMemory`), returning how many bytes were read without
/// faulting on unmapped pages.
fn read_there(src: *const u8, to: &mut [u8]) -> usize {
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    if src.is_null() || to.is_empty() {
        return 0;
    }
    let mut read = 0usize;
    // SAFETY: reading our own process into `to`; kernel checks page validity.
    let _ = unsafe {
        ReadProcessMemory(
            GetCurrentProcess(),
            src.cast(),
            to.as_mut_ptr().cast(),
            to.len(),
            &mut read,
        )
    };
    read
}

/// A piece of a description: where it is in title memory and how long.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Pair {
    address: u64,
    len: u64,
}

/// The description method 0x218 takes.
#[repr(C)]
#[derive(Default)]
struct ComputeDesc {
    root: u64,
    packet: Pair,
    /// 0x14 bytes of pipeline metadata nothing here needs; another length
    /// would be another format.
    pipeline: Pair,
    code: Pair,
    metadata: Pair,
}
const _: () = assert!(size_of::<ComputeDesc>() == 0x48);

/// The description method 0x1c8 takes: the pixel stage before the vertex
/// stage (all four of its words zero where the pipeline has none), then
/// words that were zero in every call seen.
#[repr(C)]
#[derive(Default)]
struct GraphicsDesc {
    root: u64,
    packet: Pair,
    state: Pair,
    pixel_code: Pair,
    pixel_metadata: Pair,
    vertex_code: Pair,
    vertex_metadata: Pair,
    rest: [u64; 20],
}
const _: () = assert!(size_of::<GraphicsDesc>() == 0x108);

/// A description read where the title has it; `None` if fewer than `least`
/// of its bytes are there. `T` is one of the two above: integers only.
fn described<T: Default>(desc: *const u8, least: usize) -> Option<T> {
    let mut d = T::default();
    // SAFETY: the description structures are integers without padding, so
    // any bytes are a value of theirs; the slice is exactly `d`.
    let bytes =
        unsafe { std::slice::from_raw_parts_mut((&raw mut d).cast::<u8>(), size_of::<T>()) };
    (!desc.is_null() && read_there(desc, bytes) >= least).then_some(d)
}

/// A piece's length, if it is one a piece of its kind can have.
fn len(piece: Pair, most: usize) -> Result<usize, Refusal> {
    if piece.address < 0x1_0000 || piece.len == 0 || piece.len > most as u64 {
        return Err("a piece with no address or an implausible length");
    }
    Ok(piece.len as usize)
}

/// Reads all of a piece into `to`, which has its length.
fn read(piece: Pair, to: &mut [u8]) -> Result<(), Refusal> {
    if read_there(piece.address as *const u8, to) != to.len() {
        return Err("a piece that is not all there to read");
    }
    Ok(())
}

/// A stage's container, whole: the code and the metadata read from title
/// memory into their places in `draft`, and the two checked against each
/// other.
fn filled(mut draft: Draft, code: Pair, metadata: Pair) -> Result<Vec<u8>, Refusal> {
    read(code, &mut draft.bytes[draft.code.clone()])?;
    read(metadata, &mut draft.bytes[draft.metadata.clone()])?;
    pieces::check(&draft.bytes).map_err(|e| match e {
        ShaderError::Malformed(why) => why,
        ShaderError::Unsupported { what, .. } => what,
        _ => "pieces that do not parse",
    })?;
    Ok(draft.bytes)
}

/// Validates that the description's first word points at a live COM object
/// (a root signature).
fn rooted(root: u64) -> Result<(), Refusal> {
    if root < 0x1_0000 {
        return Err("a description whose first word is not a root signature of ours");
    }
    let mut vtable = [0u8; 8];
    if read_there(root as *const u8, &mut vtable) != 8 || u64::from_le_bytes(vtable) < 0x1_0000 {
        return Err("a description whose first word is not a root signature of ours");
    }
    Ok(())
}

/// The container of the compute program `d` describes.
fn compute_container(d: &ComputeDesc) -> Result<Vec<u8>, Refusal> {
    if d.packet.len != pieces::COMPUTE_PACKET as u64
        || d.pipeline.len != pieces::COMPUTE_PIPELINE_PIECE as u64
    {
        return Err("a compute description whose first pieces are not 0x1c and 0x14 bytes");
    }
    let mut packet = [0u8; pieces::COMPUTE_PACKET];
    len(d.packet, packet.len())?;
    read(d.packet, &mut packet)?;
    let draft = pieces::compute(
        &packet,
        len(d.code, pieces::MAX_CODE)?,
        len(d.metadata, pieces::MAX_METADATA)?,
    )?;
    filled(draft, d.code, d.metadata)
}

/// `DEKUBI_PIPELINE_PIECES_ONLY=compute` or `=graphics`, read once: only
/// that kind of pipeline is made from pieces and the other kind is refused
/// as with the switch off. Unset, both kinds are made.
fn kind_refused(kind: &str) -> Result<(), Refusal> {
    static ONLY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let only = ONLY.get_or_init(|| {
        std::env::var("DEKUBI_PIPELINE_PIECES_ONLY")
            .ok()
            .filter(|v| !v.is_empty())
    });
    match only {
        Some(only) if only != kind => {
            Err("a kind of pipeline left to the title (DEKUBI_PIPELINE_PIECES_ONLY)")
        }
        _ => Ok(()),
    }
}

/// Device method 0x218: a compute pipeline from the pieces `desc` names,
/// or why it is not made.
pub(crate) fn compute(
    desc: *const u8,
    iid: *const Iid,
    out: *mut This,
) -> Result<HRESULT, Refusal> {
    kind_refused("compute")?;
    let d: ComputeDesc = described(desc, size_of::<ComputeDesc>())
        .ok_or("a description that is not there to read")?;
    rooted(d.root)?;
    let container = compute_container(&d)?;
    let shader = shader_of("compute", Cow::Owned(container));
    Ok(hand_out(vec![shader], None, iid, out))
}

/// Refuses a description whose words after the vertex stage are not zero.
fn other_stages(rest: &[u64; 20]) -> Result<(), Refusal> {
    let Some(slot) = rest.chunks(4).position(|w| w.iter().any(|&v| v != 0)) else {
        return Ok(());
    };
    let why = [
        "a graphics description with a stage in its first extra slot (+0x68)",
        "a graphics description with a stage in its second extra slot (+0x88)",
        "a graphics description with a stage in its third extra slot (+0xa8)",
        "a graphics description with a stage in its fourth extra slot (+0xc8)",
        "a graphics description with a stage in its fifth extra slot (+0xe8)",
    ][slot.min(4)];
    static SEEN: std::sync::Mutex<[bool; 5]> = std::sync::Mutex::new([false; 5]);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !std::mem::replace(&mut seen[slot.min(4)], true) {
        let w = &rest[slot * 4..slot * 4 + 4];
        tracing::warn!(
            words = format_args!("{:#x} {:#x} {:#x} {:#x}", w[0], w[1], w[2], w[3]),
            "a pipeline from pieces is not made: {why} (logged once for this slot)"
        );
    }
    Err(why)
}

/// The containers of the stages `d` describes, vertex first, and the
/// pipeline's state.
fn graphics_parts(
    d: &GraphicsDesc,
) -> Result<(Vec<(&'static str, Vec<u8>)>, GraphicsState), Refusal> {
    other_stages(&d.rest)?;
    let pixel = [d.pixel_code, d.pixel_metadata];
    let has_pixel = pixel.iter().any(|p| p.address != 0 || p.len != 0);
    let (mut packet, mut state) = ([0u8; pieces::MAX_PACKET], [0u8; pieces::MAX_STATE]);
    let packet = &mut packet[..len(d.packet, pieces::MAX_PACKET)?];
    let state = &mut state[..len(d.state, pieces::MAX_STATE)?];
    read(d.packet, packet)?;
    read(d.state, state)?;
    let g = Graphics::parse(packet, state, has_pixel)
        .inspect_err(|why| unparsed(why, packet, state))?;
    let host = state_of(&g)?;
    let stage = |code: Pair,
                 metadata: Pair,
                 draft: fn(&Graphics, usize, usize) -> Result<Draft, Refusal>| {
        let draft = draft(
            &g,
            len(code, pieces::MAX_CODE)?,
            len(metadata, pieces::MAX_METADATA)?,
        )?;
        filled(draft, code, metadata)
    };
    let mut stages = vec![(
        "vertex",
        stage(d.vertex_code, d.vertex_metadata, Graphics::vertex)?,
    )];
    if has_pixel {
        stages.push((
            "pixel",
            stage(d.pixel_code, d.pixel_metadata, Graphics::pixel)?,
        ));
    }
    Ok((stages, host))
}

/// Device method 0x1c8: a graphics pipeline from the pieces `desc` names,
/// or why it is not made.
pub(crate) fn graphics(
    desc: *const u8,
    iid: *const Iid,
    out: *mut This,
) -> Result<HRESULT, Refusal> {
    kind_refused("graphics")?;
    let d: GraphicsDesc = described(desc, 0x68).ok_or("a description that is not there to read")?;
    rooted(d.root)?;
    let (stages, state) = graphics_parts(&d)?;
    let shaders: Vec<Arc<Shader>> = stages
        .into_iter()
        .map(|(stage, bytes)| shader_of(stage, Cow::Owned(bytes)))
        .collect();
    Ok(hand_out(shaders, Some(Box::new(state)), iid, out))
}

/// Says once per reason what the registers of a packet that does not parse
/// hold.
fn unparsed(why: Refusal, packet: &[u8], state: &[u8]) {
    static SEEN: std::sync::Mutex<Vec<&str>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.contains(&why) {
        return;
    }
    seen.push(why);
    tracing::warn!(
        registers = %Graphics::describe(packet, state),
        "a graphics packet is not parsed: {why} (logged once for this reason)"
    );
}

/// A register value this module has no host state for.
fn unknown(what: Refusal, register: &'static str, value: u32) -> Refusal {
    static SEEN: std::sync::Mutex<Vec<&str>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.contains(&what) {
        seen.push(what);
        tracing::warn!(
            register,
            value = format_args!("{value:#x}"),
            "a pipeline from pieces is not made: {what} (logged once for this reason)"
        );
    }
    what
}

/// The depth-stencil view format for the state piece's depth format byte,
/// `None` for one that has no depth-stencil format.
fn depth_format_of(format: u8) -> Option<DXGI_FORMAT> {
    let family = match crate::resource::pc_format(format.into()) {
        0 => return Some(DXGI_FORMAT_UNKNOWN),
        // R32G8X24_TYPELESS, R32_FLOAT_X8X24_TYPELESS, X32_TYPELESS_G8X24_UINT
        19..=22 => DXGI_FORMAT_D32_FLOAT_S8X24_UINT,
        // R32_TYPELESS and R32_FLOAT (as a depth texture's view)
        39..=41 => DXGI_FORMAT_D32_FLOAT,
        // R24G8_TYPELESS, R24_UNORM_X8_TYPELESS, X24_TYPELESS_G8_UINT
        44..=47 => DXGI_FORMAT_D24_UNORM_S8_UINT,
        // R16_TYPELESS, D16_UNORM and R16_UNORM
        53 | 55 | 56 => DXGI_FORMAT_D16_UNORM,
        _ => return None,
    };
    Some(family)
}

/// The host's graphics state for a packet's registers and its state piece.
fn state_of(g: &Graphics) -> Result<GraphicsState, Refusal> {
    let r: &Registers = &g.registers;
    let mut s = GraphicsState::default();

    s.targets.NumRenderTargets = u32::from(g.targets);
    for (to, &format) in s.targets.RTFormats.iter_mut().zip(&g.target_formats) {
        let host = crate::resource::target_format(crate::resource::pc_format(format.into()));
        *to = DXGI_FORMAT(host as i32);
    }
    let Some(depth_format) = depth_format_of(g.depth_format) else {
        return Err(unknown(
            "a depth format that is not handled",
            "the state piece's depth format",
            g.depth_format.into(),
        ));
    };
    s.depth_format = depth_format;
    s.topology = D3D12_PRIMITIVE_TOPOLOGY_TYPE(g.topology.into());

    s.names = g
        .elements
        .iter()
        .filter_map(|e| CString::new(e.name.as_str()).ok())
        .collect();
    if s.names.len() != g.elements.len() {
        return Err("an input element with a name that is not a plain semantic");
    }
    s.inputs = g
        .elements
        .iter()
        .zip(&s.names)
        .map(|(e, name)| D3D12_INPUT_ELEMENT_DESC {
            SemanticName: PCSTR(name.as_ptr().cast()),
            SemanticIndex: 0,
            Format: DXGI_FORMAT(crate::resource::pc_format(e.format) as i32),
            InputSlot: 0,
            AlignedByteOffset: e.offset,
            InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        })
        .collect();

    let cc = r.cb_color_control;
    if cc & !0x00ff_0071 != 0 {
        return Err(unknown(
            if cc & 2 != 0 {
                "CB_COLOR_CONTROL ENABLE_1FRAG_PS_INVOKE (bit 1)"
            } else if cc & 8 != 0 {
                "CB_COLOR_CONTROL DEGAMMA_ENABLE (bit 3)"
            } else {
                "CB_COLOR_CONTROL bits outside its documented fields"
            },
            "CB_COLOR_CONTROL",
            cc,
        ));
    }
    if cc >> 16 != 0xcc {
        return Err(unknown(
            "CB_COLOR_CONTROL ROP3 (bits 16-23) other than copy",
            "CB_COLOR_CONTROL",
            cc,
        ));
    }
    let colour = match cc >> 4 & 7 {
        1 => true,
        0 => false,
        _ => {
            return Err(unknown(
                "CB_COLOR_CONTROL MODE (bits 4-6) other than disabled and normal",
                "CB_COLOR_CONTROL",
                cc,
            ));
        }
    };
    let factor = |v: u32| match v {
        0..=10 => Some(D3D12_BLEND(v as i32 + 1)),
        _ => None,
    };
    let for_alpha = |b: Option<D3D12_BLEND>| {
        b.map(|b| match b {
            D3D12_BLEND_SRC_COLOR => D3D12_BLEND_SRC_ALPHA,
            D3D12_BLEND_INV_SRC_COLOR => D3D12_BLEND_INV_SRC_ALPHA,
            D3D12_BLEND_DEST_COLOR => D3D12_BLEND_DEST_ALPHA,
            D3D12_BLEND_INV_DEST_COLOR => D3D12_BLEND_INV_DEST_ALPHA,
            other => other,
        })
    };
    let function = |v: u32| match v {
        0 => Some(D3D12_BLEND_OP_ADD),
        1 => Some(D3D12_BLEND_OP_SUBTRACT),
        2 => Some(D3D12_BLEND_OP_MIN),
        3 => Some(D3D12_BLEND_OP_MAX),
        4 => Some(D3D12_BLEND_OP_REV_SUBTRACT),
        _ => None,
    };
    for (n, to) in s.blend.RenderTarget.iter_mut().enumerate() {
        let v = r.cb_blend_control[n];
        let mask = if colour {
            r.cb_target_mask >> (4 * n) & 0xf
        } else {
            0
        };
        to.RenderTargetWriteMask = mask as u8;
        if v & 1 << 30 == 0 {
            continue;
        }
        let alpha = if v & 1 << 29 != 0 { v >> 16 } else { v };
        let fields = (
            factor(v & 0x1f),
            factor(v >> 8 & 0x1f),
            function(v >> 5 & 7),
            for_alpha(factor(alpha & 0x1f)),
            for_alpha(factor(alpha >> 8 & 0x1f)),
            function(alpha >> 5 & 7),
        );
        let (Some(src), Some(dst), Some(op), Some(src_alpha), Some(dst_alpha), Some(op_alpha)) =
            fields
        else {
            return Err(unknown(
                "a blend factor or function that is not handled",
                "CB_BLEND_CONTROL",
                v,
            ));
        };
        to.BlendEnable = true.into();
        (to.SrcBlend, to.DestBlend, to.BlendOp) = (src, dst, op);
        (to.SrcBlendAlpha, to.DestBlendAlpha, to.BlendOpAlpha) = (src_alpha, dst_alpha, op_alpha);
    }
    let first = (
        r.cb_blend_control[0],
        s.blend.RenderTarget[0].RenderTargetWriteMask,
    );
    let same = (0..8).all(|n| {
        (
            r.cb_blend_control[n],
            s.blend.RenderTarget[n].RenderTargetWriteMask,
        ) == first
    });
    s.blend.IndependentBlendEnable = (!same).into();

    let raster = r.pa_su_sc_mode_cntl;
    if raster & !7 != 0x0020_1a40 {
        return Err(unknown(
            "a polygon mode that is not handled",
            "PA_SU_SC_MODE_CNTL",
            raster,
        ));
    }
    s.rasterizer.CullMode = match raster & 3 {
        0 => D3D12_CULL_MODE_NONE,
        1 => D3D12_CULL_MODE_FRONT,
        2 => D3D12_CULL_MODE_BACK,
        _ => {
            return Err(unknown(
                "culling of both faces",
                "PA_SU_SC_MODE_CNTL",
                raster,
            ));
        }
    };
    s.rasterizer.FrontCounterClockwise = (raster & 4 == 0).into();
    s.rasterizer.DepthClipEnable = match r.pa_cl_clip_cntl {
        0x0008_0000 => true.into(),
        0x0c08_0000 => false.into(),
        v => {
            return Err(unknown(
                "a clip control that is not handled",
                "PA_CL_CLIP_CNTL",
                v,
            ));
        }
    };

    let depth = r.db_depth_control;
    if depth & !0x0070_07f7 != 0 {
        return Err(unknown(
            "a depth control bit that is not handled",
            "DB_DEPTH_CONTROL",
            depth,
        ));
    }
    let compare = |v: u32| D3D12_COMPARISON_FUNC((v & 7) as i32 + 1);
    s.depth.DepthEnable = (depth & 2 != 0).into();
    s.depth.DepthWriteMask = if depth & 4 != 0 {
        D3D12_DEPTH_WRITE_MASK_ALL
    } else {
        D3D12_DEPTH_WRITE_MASK_ZERO
    };
    s.depth.DepthFunc = compare(depth >> 4);
    s.depth.StencilEnable = (depth & 1 != 0).into();
    if depth & 1 != 0 {
        let masks = r.db_stencilrefmask;
        if masks != r.db_stencilrefmask_bf || masks >> 24 != 1 {
            return Err(unknown(
                "stencil masks that differ by face",
                "DB_STENCILREFMASK",
                masks,
            ));
        }
        s.depth.StencilReadMask = (masks >> 8) as u8;
        s.depth.StencilWriteMask = (masks >> 16) as u8;
        let operation = |v: u32| match v & 0xf {
            0 => Some(D3D12_STENCIL_OP_KEEP),
            1 => Some(D3D12_STENCIL_OP_ZERO),
            3 => Some(D3D12_STENCIL_OP_REPLACE),
            5 => Some(D3D12_STENCIL_OP_INCR_SAT),
            6 => Some(D3D12_STENCIL_OP_DECR_SAT),
            7 => Some(D3D12_STENCIL_OP_INVERT),
            8 => Some(D3D12_STENCIL_OP_INCR),
            9 => Some(D3D12_STENCIL_OP_DECR),
            _ => None,
        };
        let face = |ops: u32, func: u32| {
            Some(D3D12_DEPTH_STENCILOP_DESC {
                StencilFailOp: operation(ops)?,
                StencilPassOp: operation(ops >> 4)?,
                StencilDepthFailOp: operation(ops >> 8)?,
                StencilFunc: compare(func),
            })
        };
        let ops = r.db_stencil_control;
        let front = face(ops, depth >> 8);
        let back = if depth & 0x80 != 0 {
            face(ops >> 12, depth >> 20)
        } else {
            front
        };
        let (Some(front), Some(back)) = (front, back) else {
            return Err(unknown(
                "a stencil operation that is not handled",
                "DB_STENCIL_CONTROL",
                ops,
            ));
        };
        (s.depth.FrontFace, s.depth.BackFace) = (front, back);
    }
    Ok(s)
}
