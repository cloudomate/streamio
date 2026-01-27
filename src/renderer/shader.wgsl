// Horizon visualization shader
// Maps depth values to a seismic-style color ramp

struct Uniforms {
    view_proj: mat4x4<f32>,
    depth_range: vec2<f32>,  // min, max
    _padding: vec2<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) depth: f32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) depth_normalized: f32,
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = uniforms.view_proj * vec4<f32>(in.position, 1.0);
    out.world_normal = in.normal;

    // Normalize depth to 0-1 range
    let depth_range = uniforms.depth_range.y - uniforms.depth_range.x;
    out.depth_normalized = (in.depth - uniforms.depth_range.x) / depth_range;

    return out;
}

// Seismic color ramp: blue -> cyan -> green -> yellow -> red
fn depth_to_color(t: f32) -> vec3<f32> {
    // Clamp to valid range
    let t_clamped = clamp(t, 0.0, 1.0);

    // 5-stop color ramp
    let c0 = vec3<f32>(0.0, 0.0, 0.5);    // Deep blue
    let c1 = vec3<f32>(0.0, 0.5, 1.0);    // Cyan
    let c2 = vec3<f32>(0.0, 0.8, 0.3);    // Green
    let c3 = vec3<f32>(1.0, 0.8, 0.0);    // Yellow
    let c4 = vec3<f32>(0.8, 0.0, 0.0);    // Red

    if t_clamped < 0.25 {
        return mix(c0, c1, t_clamped / 0.25);
    } else if t_clamped < 0.5 {
        return mix(c1, c2, (t_clamped - 0.25) / 0.25);
    } else if t_clamped < 0.75 {
        return mix(c2, c3, (t_clamped - 0.5) / 0.25);
    } else {
        return mix(c3, c4, (t_clamped - 0.75) / 0.25);
    }
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Get base color from depth
    let base_color = depth_to_color(in.depth_normalized);

    // Simple lighting
    let light_dir = normalize(vec3<f32>(0.5, 0.5, 1.0));
    let normal = normalize(in.world_normal);

    // Ambient + diffuse lighting
    let ambient = 0.3;
    let diffuse = max(dot(normal, light_dir), 0.0) * 0.7;
    let lighting = ambient + diffuse;

    let final_color = base_color * lighting;

    return vec4<f32>(final_color, 1.0);
}
