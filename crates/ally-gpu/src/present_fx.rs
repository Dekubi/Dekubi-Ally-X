// Presentation-stage upscaling (FSR 1.0 EASU + RCAS) and optical-flow frame
// generation (FSR 3 style, ported from Dekubi-Ally-github's
// `fsr3_optical_flow.comp` and `fsr3_frame_interpolation.comp`) for Dekubi X.
// GPL-3.0-or-later.
//! Post-process presentation passes on the host D3D12 queue:
//!
//! - **`DEKUBI_UPSCALE=fsr1`** (also `fsr`, `1`, `on`): runs AMD FidelityFX
//!   FSR 1.0 Edge-Adaptive Spatial Upsampling (EASU) when upscaling, followed
//!   by Robust Contrast-Adaptive Sharpening (RCAS). Target size defaults to
//!   `1920x1080` when the source is smaller than 1080p, or the source size
//!   otherwise; override with `DEKUBI_UPSCALE_SIZE=1080p|1440p|4k|WxH` and
//!   sharpness stops with `DEKUBI_FSR_SHARPNESS=<0.0..=2.0>` (default `0.2`).
//! - **`DEKUBI_FRAME_GEN=fsr3`** (also `2x`, `1`, `on`): runs a half-res
//!   diamond-search optical-flow pass and a bidirectional motion-compensated
//!   interpolation pass between consecutive presented frames, presenting an
//!   interpolated midpoint frame (`t = 0.5`) ahead of each real frame (`t = 1`).
//!
//! When neither switch is enabled, the presenter uses its direct
//! `CopyResource` fast path with zero extra GPU work or allocations.

use std::sync::OnceLock;

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::s;

use crate::dxgi::parse_output_size;
use crate::host::transition;

/// Upscaling mode selected by `DEKUBI_UPSCALE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpscaleMode {
    Off,
    Fsr1,
}

/// Frame generation mode selected by `DEKUBI_FRAME_GEN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameGenMode {
    Off,
    Fsr3,
}

/// Parsed presentation FX configuration (read once from environment).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Config {
    pub(crate) upscale: UpscaleMode,
    pub(crate) upscale_size: Option<(u32, u32)>,
    pub(crate) sharpness: f32,
    pub(crate) frame_gen: FrameGenMode,
}

impl Config {
    pub(crate) fn active(self) -> bool {
        self.upscale != UpscaleMode::Off || self.frame_gen != FrameGenMode::Off
    }

    /// Output swapchain dimensions for a given title source texture size.
    pub(crate) fn output_size(self, src_w: u32, src_h: u32) -> (u32, u32) {
        if self.upscale == UpscaleMode::Off {
            return (src_w, src_h);
        }
        if let Some((w, h)) = self.upscale_size {
            return (w.max(src_w), h.max(src_h));
        }
        // Default: upscale sub-1080p frames (e.g. 1080p @ 67-75% render scale)
        // cleanly to 1920x1080; leave >= 1080p frames at native size for RCAS.
        if src_w < 1920 || src_h < 1080 {
            (1920, 1080)
        } else {
            (src_w, src_h)
        }
    }
}

fn parse_upscale(v: &str) -> UpscaleMode {
    match v.trim().to_ascii_lowercase().as_str() {
        "fsr1" | "fsr" | "easu" | "rcas" | "1" | "on" | "true" => UpscaleMode::Fsr1,
        _ => UpscaleMode::Off,
    }
}

fn parse_frame_gen(v: &str) -> FrameGenMode {
    match v.trim().to_ascii_lowercase().as_str() {
        "fsr3" | "2x" | "optical" | "1" | "on" | "true" => FrameGenMode::Fsr3,
        _ => FrameGenMode::Off,
    }
}

pub(crate) fn config() -> Config {
    static CFG: OnceLock<Config> = OnceLock::new();
    *CFG.get_or_init(|| {
        let upscale = std::env::var("DEKUBI_UPSCALE")
            .ok()
            .map(|v| parse_upscale(&v))
            .unwrap_or(UpscaleMode::Off);
        let upscale_size = std::env::var("DEKUBI_UPSCALE_SIZE")
            .ok()
            .and_then(|v| parse_output_size(&v));
        let sharpness = std::env::var("DEKUBI_FSR_SHARPNESS")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(0.2)
            .clamp(0.0, 2.0);
        let frame_gen = std::env::var("DEKUBI_FRAME_GEN")
            .ok()
            .map(|v| parse_frame_gen(&v))
            .unwrap_or(FrameGenMode::Off);
        let cfg = Config {
            upscale,
            upscale_size,
            sharpness,
            frame_gen,
        };
        if cfg.active() {
            tracing::info!(
                upscale = ?cfg.upscale,
                upscale_size = ?cfg.upscale_size,
                sharpness = cfg.sharpness,
                frame_gen = ?cfg.frame_gen,
                "presentation FX enabled"
            );
        }
        cfg
    })
}

pub(crate) fn active() -> bool {
    config().active()
}

pub(crate) fn output_size(src_w: u32, src_h: u32) -> (u32, u32) {
    config().output_size(src_w, src_h)
}

// ---------------------------------------------------------------------------
// Shared root signature and HLSL (cs_5_1) compute shaders
// ---------------------------------------------------------------------------

const ROOT_SIG_DEF: &str = r#"
#define RS "RootFlags(0), " \
           "RootConstants(num32BitConstants=8, b0), " \
           "DescriptorTable(SRV(t0, numDescriptors=3)), " \
           "DescriptorTable(UAV(u0, numDescriptors=1)), " \
           "StaticSampler(s0, filter=FILTER_MIN_MAG_LINEAR_MIP_POINT, " \
           "addressU=TEXTURE_ADDRESS_CLAMP, addressV=TEXTURE_ADDRESS_CLAMP, addressW=TEXTURE_ADDRESS_CLAMP)"
"#;

/// FSR 1.0 Edge-Adaptive Spatial Upsampling (EASU) compute shader.
const FSR_EASU_HLSL: &str = r#"
cbuffer Params : register(b0) {
    float2 src_size;
    float2 dst_size;
    float2 inv_src;
    float2 inv_dst;
};

Texture2D<float4> SrcTex : register(t0);
RWTexture2D<float4> DstTex : register(u0);
SamplerState LinearClamp : register(s0);

float luma(float3 c) {
    return c.g * 0.5 + (c.r * 0.25 + c.b * 0.25);
}

void easu_set(inout float2 dir, inout float len, float2 pp, bool biS, bool biT, bool biU, bool biV,
              float lA, float lB, float lC, float lD, float lE) {
    float w = 0.0;
    if (biS) w = (1.0 - pp.x) * (1.0 - pp.y);
    if (biT) w = pp.x * (1.0 - pp.y);
    if (biU) w = (1.0 - pp.x) * pp.y;
    if (biV) w = pp.x * pp.y;
    float dc = lD - lC;
    float cb = lC - lB;
    float lenX = max(abs(dc), abs(cb));
    lenX = lenX > 1e-6 ? rcp(lenX) : 0.0;
    float dirX = lD - lB;
    dir.x += dirX * w;
    lenX = saturate(abs(dirX) * lenX);
    lenX *= lenX;
    len += lenX * w;
    float ec = lE - lC;
    float ca = lC - lA;
    float lenY = max(abs(ec), abs(ca));
    lenY = lenY > 1e-6 ? rcp(lenY) : 0.0;
    float dirY = lE - lA;
    dir.y += dirY * w;
    lenY = saturate(abs(dirY) * lenY);
    lenY *= lenY;
    len += lenY * w;
}

void easu_tap(inout float3 aC, inout float aW, float2 off, float2 dir, float2 len, float lob, float clp, float3 c) {
    float2 v;
    v.x = (off.x * dir.x) + (off.y * dir.y);
    v.y = (off.x * (-dir.y)) + (off.y * dir.x);
    v *= len;
    float d2 = min(dot(v, v), clp);
    float wB = 0.4 * d2 - 1.0;
    float wA = lob * d2 - 1.0;
    wB *= wB;
    wA *= wA;
    wB = 1.5625 * wB - 0.5625;
    float w = wB * wA;
    aC += c * w;
    aW += w;
}

float3 fetch_src(int2 p) {
    int2 mx = int2(src_size) - int2(1, 1);
    return SrcTex.Load(int3(clamp(p, int2(0, 0), mx), 0)).rgb;
}

[RootSignature(RS)]
[numthreads(8, 8, 1)]
void main(uint3 dtid : SV_DispatchThreadID) {
    if (dtid.x >= (uint)dst_size.x || dtid.y >= (uint)dst_size.y) return;

    float2 pp = (float2(dtid.xy) + 0.5) * (src_size * inv_dst) - 0.5;
    float2 fp = floor(pp);
    pp -= fp;
    int2 base = int2(fp);

    // 12-tap FSR 1.0 neighborhood:
    //     b c
    //   e f g h
    //   i j k l
    //     n o
    float3 b = fetch_src(base + int2( 0, -1));
    float3 c = fetch_src(base + int2( 1, -1));
    float3 e = fetch_src(base + int2(-1,  0));
    float3 f = fetch_src(base + int2( 0,  0));
    float3 g = fetch_src(base + int2( 1,  0));
    float3 h = fetch_src(base + int2( 2,  0));
    float3 i = fetch_src(base + int2(-1,  1));
    float3 j = fetch_src(base + int2( 0,  1));
    float3 k = fetch_src(base + int2( 1,  1));
    float3 l = fetch_src(base + int2( 2,  1));
    float3 n = fetch_src(base + int2( 0,  2));
    float3 o = fetch_src(base + int2( 1,  2));

    float bL = luma(b), cL = luma(c);
    float eL = luma(e), fL = luma(f), gL = luma(g), hL = luma(h);
    float iL = luma(i), jL = luma(j), kL = luma(k), lL = luma(l);
    float nL = luma(n), oL = luma(o);

    float2 dir = float2(0.0, 0.0);
    float len = 0.0;
    easu_set(dir, len, pp, true,  false, false, false, bL, eL, fL, gL, jL);
    easu_set(dir, len, pp, false, true,  false, false, cL, fL, gL, hL, kL);
    easu_set(dir, len, pp, false, false, true,  false, fL, iL, jL, kL, nL);
    easu_set(dir, len, pp, false, false, false, true,  gL, jL, kL, lL, oL);

    float dirR = dot(dir, dir);
    bool zro = dirR < (1.0 / 32768.0);
    dirR = zro ? 1.0 : rsqrt(dirR);
    dir = zro ? float2(1.0, 0.0) : (dir * dirR);

    len = len * 0.5;
    len *= len;
    float stretch = dot(dir, dir) / max(abs(dir.x), abs(dir.y));
    float2 len2 = float2(1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
    float lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
    float clp = rcp(lob);

    float3 min4 = min(min(f, g), min(j, k));
    float3 max4 = max(max(f, g), max(j, k));

    float3 aC = float3(0.0, 0.0, 0.0);
    float aW = 0.0;
    easu_tap(aC, aW, float2( 0.0, -1.0) - pp, dir, len2, lob, clp, b);
    easu_tap(aC, aW, float2( 1.0, -1.0) - pp, dir, len2, lob, clp, c);
    easu_tap(aC, aW, float2(-1.0,  1.0) - pp, dir, len2, lob, clp, i);
    easu_tap(aC, aW, float2( 0.0,  1.0) - pp, dir, len2, lob, clp, j);
    easu_tap(aC, aW, float2( 0.0,  0.0) - pp, dir, len2, lob, clp, f);
    easu_tap(aC, aW, float2(-1.0,  0.0) - pp, dir, len2, lob, clp, e);
    easu_tap(aC, aW, float2( 1.0,  1.0) - pp, dir, len2, lob, clp, k);
    easu_tap(aC, aW, float2( 2.0,  1.0) - pp, dir, len2, lob, clp, l);
    easu_tap(aC, aW, float2( 2.0,  0.0) - pp, dir, len2, lob, clp, h);
    easu_tap(aC, aW, float2( 1.0,  0.0) - pp, dir, len2, lob, clp, g);
    easu_tap(aC, aW, float2( 1.0,  2.0) - pp, dir, len2, lob, clp, o);
    easu_tap(aC, aW, float2( 0.0,  2.0) - pp, dir, len2, lob, clp, n);

    float3 pix = clamp(aC / max(aW, 1e-6), min4, max4);
    DstTex[dtid.xy] = float4(pix, 1.0);
}
"#;

/// FSR 1.0 Robust Contrast-Adaptive Sharpening (RCAS) compute shader.
const FSR_RCAS_HLSL: &str = r#"
cbuffer Params : register(b0) {
    float2 dst_size;
    float sharpness_scale; // exp2(-sharpness_stops)
    float _pad0;
    float4 _pad1;
};

Texture2D<float4> SrcTex : register(t0);
RWTexture2D<float4> DstTex : register(u0);

float luma(float3 c) {
    return c.g * 0.5 + (c.r * 0.25 + c.b * 0.25);
}

float3 fetch(int2 p, int2 mx) {
    return SrcTex.Load(int3(clamp(p, int2(0, 0), mx), 0)).rgb;
}

[RootSignature(RS)]
[numthreads(8, 8, 1)]
void main(uint3 dtid : SV_DispatchThreadID) {
    int2 size = int2(dst_size);
    if ((int)dtid.x >= size.x || (int)dtid.y >= size.y) return;

    int2 mx = size - int2(1, 1);
    int2 p = int2(dtid.xy);
    float3 b = fetch(p + int2( 0, -1), mx);
    float3 d = fetch(p + int2(-1,  0), mx);
    float3 e = fetch(p,                mx);
    float3 f = fetch(p + int2( 1,  0), mx);
    float3 h = fetch(p + int2( 0,  1), mx);

    float bL = luma(b), dL = luma(d), eL = luma(e), fL = luma(f), hL = luma(h);

    // Local noise detection (suppresses sharpening on noisy/dithered areas).
    float nz = 0.25 * (bL + dL + fL + hL) - eL;
    float rangeL = max(max(max(bL, dL), max(fL, hL)), eL) - min(min(min(bL, dL), min(fL, hL)), eL);
    nz = saturate(abs(nz) / max(rangeL, 1e-5));
    nz = -0.5 * nz + 1.0;

    float3 mn4 = min(min(b, d), min(f, h));
    float3 mx4 = max(max(b, d), max(f, h));

    float3 hitMin = mn4 / max(4.0 * mx4, 1e-5);
    float3 hitMax = (1.0 - mx4) / min(4.0 * mn4 - 4.0, -1e-5);
    float3 lobeRGB = max(-hitMin, hitMax);
    float lobe = max(-0.1875, min(max(lobeRGB.r, max(lobeRGB.g, lobeRGB.b)), 0.0)) * sharpness_scale;
    lobe *= nz;

    float rcpL = rcp(4.0 * lobe + 1.0);
    float3 pix = saturate(((b + d + f + h) * lobe + e) * rcpL);
    DstTex[dtid.xy] = float4(pix, 1.0);
}
"#;

/// FSR 3 optical-flow compute shader (ported from Dekubi-Ally-github's
/// `fsr3_optical_flow.comp` to HLSL `cs_5_1`).
const FSR3_FLOW_HLSL: &str = r#"
cbuffer Params : register(b0) {
    float2 inv_flow_size;
    float2 flow_size;
    float4 _pad;
};

Texture2D<float4> CurrentFrame  : register(t0);
Texture2D<float4> PreviousFrame : register(t1);
RWTexture2D<float2> OutMotionVectors : register(u0);
SamplerState LinearClamp : register(s0);

static const uint TILE = 16;
static const uint BORDER = 2;
static const uint PADDED = TILE + 2 * BORDER; // 20
groupshared float s_curr_luma[PADDED * PADDED];

float rgb2luma(float3 c) {
    return dot(c, float3(0.299, 0.587, 0.114));
}

void loadTile(int2 tile_origin, uint2 gtid) {
    uint thread_id = gtid.y * TILE + gtid.x;
    for (uint i = thread_id; i < PADDED * PADDED; i += TILE * TILE) {
        uint lx = i % PADDED;
        uint ly = i / PADDED;
        int2 coord = tile_origin + int2(lx, ly) - int2(BORDER, BORDER);
        float2 uv = (float2(coord) + 0.5) * inv_flow_size;
        s_curr_luma[i] = rgb2luma(CurrentFrame.SampleLevel(LinearClamp, uv, 0).rgb);
    }
    GroupMemoryBarrierWithGroupSync();
}

float getCachedLuma(int2 local_coord) {
    int2 padded = local_coord + int2(BORDER, BORDER);
    return s_curr_luma[padded.y * PADDED + padded.x];
}

[RootSignature(RS)]
[numthreads(16, 16, 1)]
void main(uint3 dtid : SV_DispatchThreadID, uint3 gid : SV_GroupID, uint3 gtid : SV_GroupThreadID) {
    int2 pixel_coord = int2(dtid.xy);
    int2 tile_origin = int2(gid.xy) * int2(TILE, TILE);
    loadTile(tile_origin, gtid.xy);

    if (pixel_coord.x >= (int)flow_size.x || pixel_coord.y >= (int)flow_size.y) {
        return;
    }

    float2 uv = (float2(pixel_coord) + 0.5) * inv_flow_size;
    float curr_luma = getCachedLuma(int2(gtid.xy));
    float center_diff = abs(curr_luma - rgb2luma(PreviousFrame.SampleLevel(LinearClamp, uv, 0).rgb));

    if (center_diff < 0.002) {
        OutMotionVectors[pixel_coord] = float2(0.0, 0.0);
        return;
    }

    float2 best_motion = float2(0.0, 0.0);
    float min_diff = center_diff;

    // Large diamond (radius 2)
    static const float2 diamond_large[4] = {
        float2( 0.0, -2.0),
        float2(-2.0,  0.0), float2(2.0, 0.0),
        float2( 0.0,  2.0)
    };
    [unroll]
    for (int i = 0; i < 4; ++i) {
        float2 offset_uv = uv + diamond_large[i] * inv_flow_size;
        float l = rgb2luma(PreviousFrame.SampleLevel(LinearClamp, offset_uv, 0).rgb);
        float d = abs(curr_luma - l) + 0.04 * length(diamond_large[i]);
        if (d < min_diff) {
            min_diff = d;
            best_motion = diamond_large[i];
        }
    }

    // Small refinement diamond (radius 1)
    static const float2 diamond_small[4] = {
        float2( 0.0, -1.0),
        float2(-1.0,  0.0), float2(1.0, 0.0),
        float2( 0.0,  1.0)
    };
    float2 center = best_motion;
    [unroll]
    for (int j = 0; j < 4; ++j) {
        float2 cand = center + diamond_small[j];
        float2 offset_uv = uv + cand * inv_flow_size;
        float l = rgb2luma(PreviousFrame.SampleLevel(LinearClamp, offset_uv, 0).rgb);
        float d = abs(curr_luma - l) + 0.04 * length(cand);
        if (d < min_diff) {
            min_diff = d;
            best_motion = cand;
        }
    }

    // Dampen tiny motions to prevent micro-jitter
    float motion_len = length(best_motion);
    if (motion_len < 0.5) {
        best_motion *= motion_len * 2.0;
    }

    OutMotionVectors[pixel_coord] = best_motion;
}
"#;

/// FSR 3 optical-flow frame interpolation compute shader (ported from
/// Dekubi-Ally-github's `fsr3_frame_interpolation.comp` to HLSL `cs_5_1`).
const FSR3_INTERP_HLSL: &str = r#"
cbuffer Params : register(b0) {
    float2 inv_render_size;
    float2 render_size;
    float2 inv_flow_size;
    float blend_factor;
    float _pad;
};

Texture2D<float4> CurrentFrame  : register(t0);
Texture2D<float4> PreviousFrame : register(t1);
Texture2D<float2> MotionVectors : register(t2);
RWTexture2D<float4> OutGeneratedFrame : register(u0);
SamplerState LinearClamp : register(s0);

void cswap(inout float a, inout float b) {
    float lo = min(a, b);
    float hi = max(a, b);
    a = lo;
    b = hi;
}

float median9(float v0, float v1, float v2, float v3, float v4, float v5, float v6, float v7, float v8) {
    // Optimal 19-comparator median-of-9 network (branchless, zero dynamic indexing).
    cswap(v1, v2); cswap(v4, v5); cswap(v7, v8);
    cswap(v0, v1); cswap(v3, v4); cswap(v6, v7);
    cswap(v1, v2); cswap(v4, v5); cswap(v7, v8);
    cswap(v0, v3); cswap(v5, v8); cswap(v4, v7);
    cswap(v3, v6); cswap(v1, v4); cswap(v2, v5);
    cswap(v4, v7); cswap(v4, v2); cswap(v6, v4);
    cswap(v4, v2);
    return v4;
}

float2 sampleMotionMedian(float2 uv) {
    float2 m0 = MotionVectors.SampleLevel(LinearClamp, uv + float2(-1, -1) * inv_flow_size, 0).xy;
    float2 m1 = MotionVectors.SampleLevel(LinearClamp, uv + float2( 0, -1) * inv_flow_size, 0).xy;
    float2 m2 = MotionVectors.SampleLevel(LinearClamp, uv + float2( 1, -1) * inv_flow_size, 0).xy;
    float2 m3 = MotionVectors.SampleLevel(LinearClamp, uv + float2(-1,  0) * inv_flow_size, 0).xy;
    float2 m4 = MotionVectors.SampleLevel(LinearClamp, uv + float2( 0,  0) * inv_flow_size, 0).xy;
    float2 m5 = MotionVectors.SampleLevel(LinearClamp, uv + float2( 1,  0) * inv_flow_size, 0).xy;
    float2 m6 = MotionVectors.SampleLevel(LinearClamp, uv + float2(-1,  1) * inv_flow_size, 0).xy;
    float2 m7 = MotionVectors.SampleLevel(LinearClamp, uv + float2( 0,  1) * inv_flow_size, 0).xy;
    float2 m8 = MotionVectors.SampleLevel(LinearClamp, uv + float2( 1,  1) * inv_flow_size, 0).xy;
    return float2(
        median9(m0.x, m1.x, m2.x, m3.x, m4.x, m5.x, m6.x, m7.x, m8.x),
        median9(m0.y, m1.y, m2.y, m3.y, m4.y, m5.y, m6.y, m7.y, m8.y)
    );
}

[RootSignature(RS)]
[numthreads(16, 16, 1)]
void main(uint3 dtid : SV_DispatchThreadID) {
    int2 pixel_coord = int2(dtid.xy);
    if (pixel_coord.x >= (int)render_size.x || pixel_coord.y >= (int)render_size.y) {
        return;
    }

    float2 uv = (float2(pixel_coord) + 0.5) * inv_render_size;
    float2 motion = sampleMotionMedian(uv) * inv_flow_size;

    float2 forward_uv  = saturate(uv + motion * blend_factor);
    float2 backward_uv = saturate(uv - motion * (1.0 - blend_factor));

    float4 forward_color  = PreviousFrame.SampleLevel(LinearClamp, forward_uv, 0);
    float4 backward_color = CurrentFrame.SampleLevel(LinearClamp,  backward_uv, 0);

    float3 color_diff = abs(forward_color.rgb - backward_color.rgb);
    float occlusion = saturate(dot(color_diff, float3(0.333, 0.333, 0.333)) * 2.0);

    float alpha = lerp(blend_factor, 0.5, occlusion * 0.4);
    float4 interpolated = lerp(forward_color, backward_color, alpha);

    // Anti-flicker: blend with un-warped center pixel on near-static areas
    float motion_magnitude = length(motion * render_size);
    float direct_weight = exp(-motion_magnitude * 0.5);
    float4 center_color = lerp(
        PreviousFrame.SampleLevel(LinearClamp, uv, 0),
        CurrentFrame.SampleLevel(LinearClamp, uv, 0),
        blend_factor
    );
    interpolated = lerp(interpolated, center_color, direct_weight * 0.3);

    OutGeneratedFrame[pixel_coord] = float4(interpolated.rgb, 1.0);
}
"#;

fn compile_cs(name: &str, body: &str) -> windows::core::Result<Vec<u8>> {
    let source = format!("{ROOT_SIG_DEF}\n{body}");
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: `source` lives through `D3DCompile`; output pointers are ours.
    let res = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            s!("present_fx"),
            None,
            None,
            s!("main"),
            s!("cs_5_1"),
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    let blob_bytes = |b: &ID3DBlob| -> Vec<u8> {
        // SAFETY: `b` owns the returned buffer.
        unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
            .to_vec()
    };
    if let Err(e) = res {
        if let Some(err) = errors {
            tracing::warn!(
                shader = name,
                error = %String::from_utf8_lossy(&blob_bytes(&err)),
                "present FX shader compile failed"
            );
        }
        return Err(e);
    }
    Ok(blob_bytes(&code.ok_or_else(windows::core::Error::empty)?))
}

/// Number of descriptor table slots (4 descriptors per dispatch: 3 SRVs + 1 UAV),
/// replicated across `RING_FRAMES` so async in-flight presents never overwrite
/// each other's shader-visible descriptors.
const PASSES_PER_FRAME: u32 = 4;
const RING_FRAMES: u32 = 8;
const MAX_PASSES_IN_FLIGHT: u32 = PASSES_PER_FRAME * RING_FRAMES;
const DESCS_PER_PASS: u32 = 4;

struct Pipelines {
    root: ID3D12RootSignature,
    easu: ID3D12PipelineState,
    rcas: ID3D12PipelineState,
    flow: ID3D12PipelineState,
    interp: ID3D12PipelineState,
}

/// Per-presenter D3D12 resources for FSR 1.0 upscaling and FSR 3 frame gen.
pub(crate) struct PresentFx {
    pipes: Pipelines,
    heap: ID3D12DescriptorHeap,
    step: u32,
    src_w: u32,
    src_h: u32,
    out_w: u32,
    out_h: u32,
    format: DXGI_FORMAT,
    src_tex: ID3D12Resource,
    easu_tex: ID3D12Resource,
    curr_tex: ID3D12Resource,
    prev_tex: ID3D12Resource,
    flow_tex: ID3D12Resource,
    interp_tex: ID3D12Resource,
    has_prev: bool,
    ring: u32,
}

// SAFETY: D3D12 objects are free-threaded; PresentFx is owned inside Presenter's Mutex.
unsafe impl Send for PresentFx {}

fn make_default_uav_tex(
    device: &ID3D12Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    initial: D3D12_RESOURCE_STATES,
) -> windows::core::Result<ID3D12Resource> {
    let props = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        ..Default::default()
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: u64::from(width.max(1)),
        Height: height.max(1),
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        ..Default::default()
    };
    let mut resource: Option<ID3D12Resource> = None;
    // SAFETY: valid device, heap props, and resource description.
    unsafe {
        device.CreateCommittedResource(
            &props,
            D3D12_HEAP_FLAG_NONE,
            &desc,
            initial,
            None,
            &mut resource,
        )?;
    }
    resource.ok_or_else(windows::core::Error::empty)
}

impl PresentFx {
    pub(crate) fn new(
        device: &ID3D12Device,
        src_w: u32,
        src_h: u32,
        out_w: u32,
        out_h: u32,
        format: DXGI_FORMAT,
    ) -> windows::core::Result<Self> {
        let easu_bc = compile_cs("fsr_easu", FSR_EASU_HLSL)?;
        let rcas_bc = compile_cs("fsr_rcas", FSR_RCAS_HLSL)?;
        let flow_bc = compile_cs("fsr3_flow", FSR3_FLOW_HLSL)?;
        let interp_bc = compile_cs("fsr3_interp", FSR3_INTERP_HLSL)?;

        // SAFETY: compiled shaders embed the shared root signature; plain D3D12 creations.
        let pipes = unsafe {
            let root: ID3D12RootSignature = device.CreateRootSignature(0, &easu_bc)?;
            let make_pso = |bc: &[u8]| -> windows::core::Result<ID3D12PipelineState> {
                device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: bc.as_ptr().cast(),
                        BytecodeLength: bc.len(),
                    },
                    ..Default::default()
                })
            };
            Pipelines {
                easu: make_pso(&easu_bc)?,
                rcas: make_pso(&rcas_bc)?,
                flow: make_pso(&flow_bc)?,
                interp: make_pso(&interp_bc)?,
                root,
            }
        };

        let kind = D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV;
        // SAFETY: plain shader-visible descriptor heap creation.
        let (heap, step) = unsafe {
            let heap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: kind,
                NumDescriptors: MAX_PASSES_IN_FLIGHT * DESCS_PER_PASS,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            })?;
            let step = device.GetDescriptorHandleIncrementSize(kind);
            (heap, step)
        };

        let flow_w = (out_w / 2).max(1);
        let flow_h = (out_h / 2).max(1);
        let src_tex =
            make_default_uav_tex(device, src_w, src_h, format, D3D12_RESOURCE_STATE_COPY_DEST)?;
        let easu_tex = make_default_uav_tex(
            device,
            out_w,
            out_h,
            format,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        )?;
        let curr_tex =
            make_default_uav_tex(device, out_w, out_h, format, D3D12_RESOURCE_STATE_COMMON)?;
        let prev_tex = make_default_uav_tex(
            device,
            out_w,
            out_h,
            format,
            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
        )?;
        let flow_tex = make_default_uav_tex(
            device,
            flow_w,
            flow_h,
            DXGI_FORMAT_R16G16_FLOAT,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        )?;
        let interp_tex = make_default_uav_tex(
            device,
            out_w,
            out_h,
            format,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        )?;

        Ok(Self {
            pipes,
            heap,
            step,
            src_w,
            src_h,
            out_w,
            out_h,
            format,
            src_tex,
            easu_tex,
            curr_tex,
            prev_tex,
            flow_tex,
            interp_tex,
            has_prev: false,
            ring: 0,
        })
    }

    /// Advances the descriptor ring slot for a new present so async presents
    /// in flight on the GPU never share descriptor slots.
    pub(crate) fn next_frame_slot(&mut self) {
        self.ring = (self.ring + 1) % RING_FRAMES;
    }

    pub(crate) fn matches(
        &self,
        src_w: u32,
        src_h: u32,
        out_w: u32,
        out_h: u32,
        format: DXGI_FORMAT,
    ) -> bool {
        self.src_w == src_w
            && self.src_h == src_h
            && self.out_w == out_w
            && self.out_h == out_h
            && self.format == format
    }

    pub(crate) fn curr_tex(&self) -> &ID3D12Resource {
        &self.curr_tex
    }

    pub(crate) fn prev_tex(&self) -> &ID3D12Resource {
        &self.prev_tex
    }

    pub(crate) fn interp_tex(&self) -> &ID3D12Resource {
        &self.interp_tex
    }

    pub(crate) fn has_prev(&self) -> bool {
        self.has_prev
    }

    pub(crate) fn mark_prev_ready(&mut self) {
        self.has_prev = true;
    }

    /// Writes a pass's 3 SRVs + 1 UAV into descriptor slot `pass_idx` and
    /// dispatches `pso`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn dispatch(
        &self,
        device: &ID3D12Device,
        list: &ID3D12GraphicsCommandList,
        pso: &ID3D12PipelineState,
        pass_idx: u32,
        srvs: [Option<(&ID3D12Resource, DXGI_FORMAT)>; 3],
        uav: (&ID3D12Resource, DXGI_FORMAT),
        consts: [f32; 8],
        groups: (u32, u32),
    ) {
        // SAFETY: caller ensures `list` is open and resources are in SRV/UAV states.
        unsafe {
            let cpu0 = self.heap.GetCPUDescriptorHandleForHeapStart();
            let gpu0 = self.heap.GetGPUDescriptorHandleForHeapStart();
            let base = (self.ring * PASSES_PER_FRAME + pass_idx) * DESCS_PER_PASS;

            for (i, srv) in srvs.iter().enumerate() {
                let handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                    ptr: cpu0.ptr + ((base + i as u32) * self.step) as usize,
                };
                let (res, fmt) = match srv {
                    Some((r, f)) => (Some(*r), *f),
                    None => (None, self.format),
                };
                let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: fmt,
                    ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2D: D3D12_TEX2D_SRV {
                            MipLevels: 1,
                            ..Default::default()
                        },
                    },
                };
                device.CreateShaderResourceView(res, Some(&desc), handle);
            }

            let uav_handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: cpu0.ptr + ((base + 3) * self.step) as usize,
            };
            let uav_desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
                Format: uav.1,
                ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
                ..Default::default()
            };
            device.CreateUnorderedAccessView(uav.0, None, Some(&uav_desc), uav_handle);

            let heaps = [Some(self.heap.clone())];
            list.SetDescriptorHeaps(&heaps);
            list.SetComputeRootSignature(&self.pipes.root);
            list.SetPipelineState(pso);
            let raw_consts: [u32; 8] = consts.map(f32::to_bits);
            list.SetComputeRoot32BitConstants(0, 8, raw_consts.as_ptr().cast(), 0);
            list.SetComputeRootDescriptorTable(
                1,
                D3D12_GPU_DESCRIPTOR_HANDLE {
                    ptr: gpu0.ptr + u64::from(base * self.step),
                },
            );
            list.SetComputeRootDescriptorTable(
                2,
                D3D12_GPU_DESCRIPTOR_HANDLE {
                    ptr: gpu0.ptr + u64::from((base + 3) * self.step),
                },
            );
            list.Dispatch(groups.0, groups.1, 1);
        }
    }

    /// Produces `self.curr_tex` in `COPY_SOURCE` state from `src` (which starts
    /// in `src_state` and is restored to `src_state`).
    pub(crate) unsafe fn record_upscale_or_copy(
        &self,
        device: &ID3D12Device,
        list: &ID3D12GraphicsCommandList,
        src: &ID3D12Resource,
        src_state: D3D12_RESOURCE_STATES,
        src_w: u32,
        src_h: u32,
    ) {
        let cfg = config();
        // SAFETY: caller ensures `list` is open and `src` is live in `src_state`.
        unsafe {
            if cfg.upscale == UpscaleMode::Off {
                list.ResourceBarrier(&[
                    transition(src, src_state, D3D12_RESOURCE_STATE_COPY_SOURCE),
                    transition(
                        &self.curr_tex,
                        D3D12_RESOURCE_STATE_COMMON,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                    ),
                ]);
                list.CopyResource(&self.curr_tex, src);
                list.ResourceBarrier(&[
                    transition(src, D3D12_RESOURCE_STATE_COPY_SOURCE, src_state),
                    transition(
                        &self.curr_tex,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    ),
                ]);
                return;
            }

            let gx = self.out_w.div_ceil(8);
            let gy = self.out_h.div_ceil(8);
            let need_easu = src_w != self.out_w || src_h != self.out_h;

            // Copy `src` into our typed SRV-capable `src_tex` first so sRGB /
            // typeless / DENY_SHADER_RESOURCE title backbuffers always sample
            // cleanly with exact bit-for-bit sRGB values (matching CopyResource).
            list.ResourceBarrier(&[transition(src, src_state, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
            list.CopyResource(&self.src_tex, src);
            list.ResourceBarrier(&[
                transition(src, D3D12_RESOURCE_STATE_COPY_SOURCE, src_state),
                transition(
                    &self.src_tex,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                ),
                transition(
                    &self.curr_tex,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                ),
            ]);

            let rcas_input = if need_easu {
                self.dispatch(
                    device,
                    list,
                    &self.pipes.easu,
                    0,
                    [Some((&self.src_tex, self.format)), None, None],
                    (&self.easu_tex, self.format),
                    [
                        src_w as f32,
                        src_h as f32,
                        self.out_w as f32,
                        self.out_h as f32,
                        1.0 / src_w as f32,
                        1.0 / src_h as f32,
                        1.0 / self.out_w as f32,
                        1.0 / self.out_h as f32,
                    ],
                    (gx, gy),
                );
                list.ResourceBarrier(&[transition(
                    &self.easu_tex,
                    D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                )]);
                &self.easu_tex
            } else {
                &self.src_tex
            };

            let sharpness_scale = (-cfg.sharpness).exp2();
            self.dispatch(
                device,
                list,
                &self.pipes.rcas,
                1,
                [Some((rcas_input, self.format)), None, None],
                (&self.curr_tex, self.format),
                [
                    self.out_w as f32,
                    self.out_h as f32,
                    sharpness_scale,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                ],
                (gx, gy),
            );

            if need_easu {
                list.ResourceBarrier(&[
                    transition(
                        &self.easu_tex,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    ),
                    transition(
                        &self.src_tex,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                    ),
                    transition(
                        &self.curr_tex,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    ),
                ]);
            } else {
                list.ResourceBarrier(&[
                    transition(
                        &self.src_tex,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                    ),
                    transition(
                        &self.curr_tex,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    ),
                ]);
            }
        }
    }

    /// Runs optical flow + interpolation (`t = 0.5`) from `prev_tex` (in SRV)
    /// and `curr_tex` (starts in `COPY_SOURCE`, restored to `COPY_SOURCE`),
    /// leaving `interp_tex` in `COPY_SOURCE`.
    pub(crate) unsafe fn record_frame_gen(
        &self,
        device: &ID3D12Device,
        list: &ID3D12GraphicsCommandList,
    ) {
        let flow_w = (self.out_w / 2).max(1);
        let flow_h = (self.out_h / 2).max(1);
        // SAFETY: caller ensures `list` is open and states match the contract above.
        unsafe {
            list.ResourceBarrier(&[transition(
                &self.curr_tex,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            )]);

            // Pass 1: half-res optical flow
            self.dispatch(
                device,
                list,
                &self.pipes.flow,
                2,
                [
                    Some((&self.curr_tex, self.format)),
                    Some((&self.prev_tex, self.format)),
                    None,
                ],
                (&self.flow_tex, DXGI_FORMAT_R16G16_FLOAT),
                [
                    1.0 / flow_w as f32,
                    1.0 / flow_h as f32,
                    flow_w as f32,
                    flow_h as f32,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                ],
                (flow_w.div_ceil(16), flow_h.div_ceil(16)),
            );

            list.ResourceBarrier(&[transition(
                &self.flow_tex,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            )]);

            // Pass 2: full-res bidirectional interpolation at t = 0.5
            self.dispatch(
                device,
                list,
                &self.pipes.interp,
                3,
                [
                    Some((&self.curr_tex, self.format)),
                    Some((&self.prev_tex, self.format)),
                    Some((&self.flow_tex, DXGI_FORMAT_R16G16_FLOAT)),
                ],
                (&self.interp_tex, self.format),
                [
                    1.0 / self.out_w as f32,
                    1.0 / self.out_h as f32,
                    self.out_w as f32,
                    self.out_h as f32,
                    1.0 / flow_w as f32,
                    1.0 / flow_h as f32,
                    0.5,
                    0.0,
                ],
                (self.out_w.div_ceil(16), self.out_h.div_ceil(16)),
            );

            list.ResourceBarrier(&[
                transition(
                    &self.flow_tex,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                ),
                transition(
                    &self.curr_tex,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                ),
                transition(
                    &self.interp_tex,
                    D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                ),
            ]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_modes() {
        assert_eq!(parse_upscale("fsr1"), UpscaleMode::Fsr1);
        assert_eq!(parse_upscale("FSR"), UpscaleMode::Fsr1);
        assert_eq!(parse_upscale("off"), UpscaleMode::Off);
        assert_eq!(parse_upscale("0"), UpscaleMode::Off);

        assert_eq!(parse_frame_gen("fsr3"), FrameGenMode::Fsr3);
        assert_eq!(parse_frame_gen("2x"), FrameGenMode::Fsr3);
        assert_eq!(parse_frame_gen("off"), FrameGenMode::Off);
        assert_eq!(parse_frame_gen("0"), FrameGenMode::Off);
    }

    #[test]
    fn output_size_upscales_sub_1080p_by_default() {
        let cfg = Config {
            upscale: UpscaleMode::Fsr1,
            upscale_size: None,
            sharpness: 0.2,
            frame_gen: FrameGenMode::Off,
        };
        assert_eq!(cfg.output_size(1280, 720), (1920, 1080));
        assert_eq!(cfg.output_size(1440, 810), (1920, 1080));
        assert_eq!(cfg.output_size(1920, 1080), (1920, 1080));
        assert_eq!(cfg.output_size(3840, 2160), (3840, 2160));
    }

    #[test]
    fn output_size_honors_explicit_target_and_off() {
        let cfg = Config {
            upscale: UpscaleMode::Fsr1,
            upscale_size: Some((2560, 1440)),
            sharpness: 0.2,
            frame_gen: FrameGenMode::Fsr3,
        };
        assert_eq!(cfg.output_size(1280, 720), (2560, 1440));
        assert_eq!(cfg.output_size(3840, 2160), (3840, 2160));

        let off = Config {
            upscale: UpscaleMode::Off,
            upscale_size: Some((2560, 1440)),
            sharpness: 0.2,
            frame_gen: FrameGenMode::Fsr3,
        };
        assert_eq!(off.output_size(1280, 720), (1280, 720));
    }

    #[test]
    fn all_present_fx_hlsl_shaders_compile_with_fxc() {
        for (name, src) in [
            ("fsr_easu", FSR_EASU_HLSL),
            ("fsr_rcas", FSR_RCAS_HLSL),
            ("fsr3_flow", FSR3_FLOW_HLSL),
            ("fsr3_interp", FSR3_INTERP_HLSL),
        ] {
            let bc = compile_cs(name, src).unwrap_or_else(|e| panic!("{name} failed: {e}"));
            assert!(!bc.is_empty(), "{name} produced empty bytecode");
        }
    }
}
