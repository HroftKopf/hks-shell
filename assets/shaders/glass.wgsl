// hks-shell glass mask — GPU port of the former SHM render_glass().
// Draws the surface silhouette (rounded rect, circular corners) with a light
// tint and a directional top-left rim, output as PREMULTIPLIED alpha so the
// compositor's liquid-glass effect (blur/refraction) shows through where alpha
// is low. This is only the CLIENT mask; blur/refraction are compositor-side.

struct Uniforms {
    resolution: vec2<f32>,
    radius: f32,
    edge_feather: f32,
    material_fade_width: f32,
    edge_alpha_scale: f32,
    base_alpha: f32,
    border_width: f32,
    highlight_strength: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    tint: vec3<f32>,
    _pad3: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Fullscreen triangle.
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(pos[vi], 0.0, 1.0);
}

fn smootherstep(a: f32, b: f32, x: f32) -> f32 {
    let t = clamp((x - a) / (b - a), 0.0, 1.0);
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

// Signed distance to a rounded rectangle with circular corners.
fn sd_round_rect(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2<f32>(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - r;
}

// Signed distance to a line segment a-b.
fn sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = u.resolution;
    let p = frag.xy;                       // pixel coords, origin top-left
    let half = res * 0.5;
    let r = clamp(u.radius, 1.0, min(half.x, half.y) - 0.5);

    let dist = sd_round_rect(p - half, half, r);

    let feather = max(u.edge_feather, 0.5);
    let coverage = 1.0 - smootherstep(-feather, feather, dist);
    if (coverage <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    let inside = max(-dist, 0.0);
    let material_fade = smootherstep(0.0, max(u.material_fade_width, 1.0), inside);
    let eas = clamp(u.edge_alpha_scale, 0.0, 1.0);
    let edge_material_scale = eas + (1.0 - eas) * material_fade;

    let n = p / res;
    let top_left = (1.0 - n.x) * 0.45 + (1.0 - n.y) * 0.55;
    let edge_factor = 1.0 - smootherstep(0.0, max(u.border_width, 0.01), inside);
    let border_highlight = edge_factor * pow(max(top_left, 0.0), 2.2) * u.highlight_strength;

    var rgb = u.tint;
    rgb = rgb + (vec3<f32>(1.0, 1.0, 1.0) - rgb) * border_highlight;

    var a = clamp(u.base_alpha * edge_material_scale + border_highlight * 0.12, 0.0, 0.65);

    // Magnifier icon on the left: a ring plus a diagonal handle, drawn in grey.
    let icon_center = vec2<f32>(23.0, res.y * 0.5);
    let ip = p - icon_center;
    let ring = abs(length(ip) - 6.5) - 1.4;
    let handle = sd_segment(ip, vec2<f32>(4.6, 4.6), vec2<f32>(10.4, 10.4)) - 1.6;
    let icon_d = min(ring, handle);
    let icon_cov = (1.0 - smootherstep(-0.75, 0.75, icon_d)) * coverage;
    rgb = mix(rgb, vec3<f32>(0.95, 0.95, 0.98), icon_cov);
    a = max(a, icon_cov * 0.92);

    let final_alpha = a * coverage;

    // Premultiplied output.
    return vec4<f32>(rgb * final_alpha, final_alpha);
}
