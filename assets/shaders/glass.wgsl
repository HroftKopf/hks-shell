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
    sel_top: f32,
    sel_height: f32,
    row_count: f32,
    tint: vec3<f32>,
    caret_x: f32,
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
    // Buffer is rendered at 1.2x (physical); work in logical px so the layout
    // constants stay 1:1 while AA is computed at full physical density.
    let res = u.resolution / 1.2;
    let p = frag.xy / 1.2;
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
    // Magnifier centered in the icon column (ICON_LEFT 20 + ICON_SIZE/2 16.5 = 36.5).
    let icon_center = vec2<f32>(36.5, 25.0);
    let ip = p - icon_center;
    let ring = abs(length(ip) - 7.0) - 1.4;
    let handle = sd_segment(ip, vec2<f32>(5.0, 5.0), vec2<f32>(11.0, 11.0)) - 1.5;
    let icon_d = min(ring, handle);
    let icon_cov = (1.0 - smootherstep(-0.75, 0.75, icon_d)) * coverage;
    rgb = mix(rgb, vec3<f32>(0.95, 0.95, 0.98), icon_cov);
    a = max(a, icon_cov * 0.92);

    // Blinking text caret in the search bar (caret_x <= 0 = hidden).
    if (u.caret_x > 0.5) {
        let cd = sd_round_rect(p - vec2<f32>(u.caret_x, 25.0), vec2<f32>(1.0, 11.0), 1.0);
        let ccov = (1.0 - smootherstep(-0.75, 0.75, cd)) * coverage;
        rgb = mix(rgb, vec3<f32>(1.0, 1.0, 1.0), ccov);
        a = max(a, ccov);
    }

    // Selected-row highlight: bright in the middle, fading to transparent
    // toward its edges (a feathered band, not a glow).
    if (u.sel_height > 0.5) {
        let inset_x = 10.0;
        let hl_center = vec2<f32>(res.x * 0.5, u.sel_top + u.sel_height * 0.5);
        let hl_half = vec2<f32>(res.x * 0.5 - inset_x, u.sel_height * 0.5 - 3.0);
        let hl_d = sd_round_rect(p - hl_center, hl_half, 14.0);
        let hl_cov = (1.0 - smootherstep(-0.75, 0.75, hl_d)) * coverage;
        rgb = mix(rgb, vec3<f32>(0.34, 0.60, 1.0), hl_cov * 0.60);
        a = max(a, hl_cov * 0.52);
    }

    // Grey rounded-square tile behind each result icon. Drawn AFTER the
    // highlight so the tile keeps its colour on the selected row.
    // RESULTS_TOP=58, ROW_H=48, icon column center 36.5.
    if (u.row_count > 0.5 && p.y > 58.0) {
        let row = floor((p.y - 58.0) / 48.0);
        if (row < u.row_count) {
            let row_top = 58.0 + row * 48.0;
            let tile_center = vec2<f32>(36.5, row_top + 24.0);
            let td = sd_round_rect(p - tile_center, vec2<f32>(16.5, 16.5), 9.0);
            let tcov = (1.0 - smootherstep(-1.0, 1.0, td)) * coverage;
            rgb = mix(rgb, vec3<f32>(0.90, 0.90, 0.92), tcov * 0.85);
            a = max(a, tcov * 0.85);
        }
    }

    // Hairline divider under the search bar (only when results are shown).
    if (res.y > 60.0) {
        let dy = abs(p.y - 50.0);
        if (dy < 0.8 && p.x > 18.0 && p.x < res.x - 18.0) {
            let dcov = (1.0 - smootherstep(0.0, 0.8, dy)) * coverage;
            rgb = mix(rgb, vec3<f32>(1.0, 1.0, 1.0), dcov * 0.45);
            a = max(a, dcov * 0.45);
        }
    }

    let final_alpha = a * coverage;

    // Premultiplied output.
    return vec4<f32>(rgb * final_alpha, final_alpha);
}
