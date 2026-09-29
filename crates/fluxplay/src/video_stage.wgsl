// Video stage: decoded planes → RGB, letterboxed and resampled in one pass.
// Planes are integer textures read with textureLoad, so NV12, P010 and RGBA
// share one pipeline and need no optional wgpu feature.

struct Params {
    // Picture rectangle in NDC: left, top, right, bottom.
    rect: vec4<f32>,
    // Luma (or RGBA) size, chroma size.
    src: vec4<f32>,
    // Kernel stretch x/y (> 1 when downscaling), tap radius x/y.
    filt: vec4<f32>,
    // R = Y + a·V, G = Y + b·U + c·V, B = Y + d·U.
    coeffs: vec4<f32>,
    // Raw sample → normalized: (v - off) * mul for luma, then chroma.
    range: vec4<f32>,
    // x: 1 = RGBA picture, y: 1 = target is sRGB (write linear), z: sample shift.
    mode: vec4<u32>,
};

@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var tex_y: texture_2d<u32>;
@group(0) @binding(2) var tex_uv: texture_2d<u32>;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    let corner = vec2<f32>(f32(i & 1u), f32((i >> 1u) & 1u));
    var o: VOut;
    o.pos = vec4<f32>(mix(P.rect.x, P.rect.z, corner.x), mix(P.rect.y, P.rect.w, corner.y), 0.0, 1.0);
    o.uv = corner;
    return o;
}

fn load_y(p: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(tex_y));
    let c = clamp(p, vec2<i32>(0), size - vec2<i32>(1));
    return vec4<f32>(textureLoad(tex_y, c, 0) >> vec4<u32>(P.mode.z));
}

fn load_uv(p: vec2<i32>) -> vec2<f32> {
    let size = vec2<i32>(textureDimensions(tex_uv));
    let c = clamp(p, vec2<i32>(0), size - vec2<i32>(1));
    return vec2<f32>(textureLoad(tex_uv, c, 0).xy >> vec2<u32>(P.mode.z));
}

// Catmull-Rom: sharp without the halos of Lanczos on flat video.
fn cubic(x: f32) -> f32 {
    let a = abs(x);
    if a < 1.0 {
        return (1.5 * a - 2.5) * a * a + 1.0;
    }
    if a < 2.0 {
        return ((-0.5 * a + 2.5) * a - 4.0) * a + 2.0;
    }
    return 0.0;
}

// `pos` in texels (centres at .5). When downscaling the kernel widens with
// the ratio so every source pixel contributes (no shimmering).
fn resample_y(pos: vec2<f32>) -> vec4<f32> {
    let c = pos - vec2<f32>(0.5);
    let base = floor(c);
    let f = c - base;
    let r = vec2<i32>(P.filt.zw);
    let b = vec2<i32>(base);
    var sum = vec4<f32>(0.0);
    var wsum = 0.0;
    for (var j = 1 - r.y; j <= r.y; j++) {
        let wy = cubic((f32(j) - f.y) / P.filt.y);
        for (var i = 1 - r.x; i <= r.x; i++) {
            let w = cubic((f32(i) - f.x) / P.filt.x) * wy;
            sum += w * load_y(b + vec2<i32>(i, j));
            wsum += w;
        }
    }
    return sum / wsum;
}

// 4:2:0 chroma sited left of its luma pair and between the two rows (MPEG-2 /
// H.264 / HEVC default), bilinear.
fn chroma(pos: vec2<f32>) -> vec2<f32> {
    let c = vec2<f32>((pos.x - 0.5) * 0.5, pos.y * 0.5 - 0.5);
    let base = floor(c);
    let f = c - base;
    let b = vec2<i32>(base);
    let top = mix(load_uv(b), load_uv(b + vec2<i32>(1, 0)), f.x);
    let bottom = mix(load_uv(b + vec2<i32>(0, 1)), load_uv(b + vec2<i32>(1, 1)), f.x);
    return mix(top, bottom, f.y);
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let pos = in.uv * P.src.xy;
    var rgb: vec3<f32>;
    if P.mode.x == 1u {
        rgb = resample_y(pos).rgb * P.range.y;
    } else {
        let y = (resample_y(pos).r - P.range.x) * P.range.y;
        let uv = (chroma(pos) - vec2<f32>(P.range.z)) * P.range.w;
        rgb = vec3<f32>(
            y + P.coeffs.x * uv.y,
            y + P.coeffs.y * uv.x + P.coeffs.z * uv.y,
            y + P.coeffs.w * uv.x,
        );
    }
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    if P.mode.y == 1u {
        rgb = srgb_to_linear(rgb);
    }
    return vec4<f32>(rgb, 1.0);
}
