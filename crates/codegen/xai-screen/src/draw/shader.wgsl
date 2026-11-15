// One vertex stage for every pipeline; the fragment stage picks how pixels look.
// Colors are premultiplied sRGB.

struct Camera {
    clip_from_world: mat4x4<f32>,
    view_from_world: mat4x4<f32>,
}

@group(0) @binding(0) var<storage, read> cameras: array<Camera>;
@group(0) @binding(1) var atlas: texture_2d<f32>;

struct Vertex {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) texel: vec2<f32>,
}

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) texel: vec2<f32>,
    // World position, where surface detail is fixed as the camera moves.
    @location(3) place: vec3<f32>,
}

@vertex
fn vertex_main(input: Vertex, @builtin(instance_index) index: u32) -> Varyings {
    let camera = cameras[index];
    var output: Varyings;
    output.position = camera.clip_from_world * vec4<f32>(input.position, 1.0);
    output.normal = (camera.view_from_world * vec4<f32>(input.normal, 0.0)).xyz;
    output.color = input.color;
    output.texel = input.texel;
    output.place = input.position;
    return output;
}

@fragment
fn solid(input: Varyings) -> @location(0) vec4<f32> {
    return input.color;
}

const KEY_LIGHT: vec3<f32> = vec3<f32>(-0.45, 0.65, 0.6);
const FILL_LIGHT: vec3<f32> = vec3<f32>(0.55, -0.25, 0.5);
const FLESH: f32 = 1.0;

fn hash(p: vec3<f32>) -> f32 {
    let q = fract(p * 0.3183099 + vec3<f32>(0.71, 0.113, 0.419)) * 17.0;
    return fract(q.x * q.y * q.z * (q.x + q.y + q.z));
}

// Smooth value noise from 0 to 1.
fn noise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let corner = vec2<f32>(1.0, 0.0);
    let bottom = mix(
        mix(hash(i), hash(i + corner.xyy), u.x),
        mix(hash(i + corner.yxy), hash(i + corner.xxy), u.x),
        u.y,
    );
    let top = mix(
        mix(hash(i + corner.yyx), hash(i + corner.xyx), u.x),
        mix(hash(i + corner.yxx), hash(i + corner.xxx), u.x),
        u.y,
    );
    return mix(bottom, top, u.z);
}

// Bumps of three sizes, from 0 to 1.
fn bumps(p: vec3<f32>) -> f32 {
    return 0.55 * noise(p) + 0.3 * noise(p * 2.07) + 0.15 * noise(p * 4.13);
}

// The surface normal in camera space, turned toward the camera so open meshes show both
// sides.
fn facing(input: Varyings) -> vec3<f32> {
    let normal = normalize(input.normal);
    return select(normal, -normal, normal.z < 0.0);
}

// Studio lighting fixed to the camera: a soft key light from the upper left that wraps
// around curves, a dim fill from the lower right, and ambient light that is brighter from
// above. Plastic adds a glossy highlight and a faint rim that lifts edges off the
// background. Flesh adds fine bumps and mottling, a warm glow in its shadows as light
// scatters under the surface, and a wet highlight.
@fragment
fn lit(input: Varyings) -> @location(0) vec4<f32> {
    // Derivatives need every pixel of a quad, so the bumps are measured for every material.
    let height = bumps(input.place * input.texel.y);
    let slope = vec3<f32>(dpdx(height), -dpdy(height), 0.0);
    let flesh = abs(input.texel.x - FLESH) < 0.5;
    var normal = facing(input);
    if flesh {
        normal = normalize(normal - slope * 4.0);
    }
    let key_light = normalize(KEY_LIGHT);
    let ambient = mix(0.26, 0.4, 0.5 + 0.5 * normal.y);
    let key = max((dot(normal, key_light) + 0.3) / 1.3, 0.0);
    let fill = max(dot(normal, normalize(FILL_LIGHT)), 0.0);
    let half_way = normalize(key_light + vec3<f32>(0.0, 0.0, 1.0));
    let shine = max(dot(normal, half_way), 0.0);
    let edge = 1.0 - normal.z;
    var color = input.color.rgb;
    var light = vec3<f32>(ambient + 0.62 * key + 0.14 * fill);
    var gloss = 0.28 * pow(shine, 36.0) + 0.14 * pow(edge, 3.0);
    if flesh {
        color = color * (0.84 + 0.32 * height);
        light = light + vec3<f32>(0.32, 0.1, 0.07) * (1.0 - key);
        gloss = 0.42 * pow(shine, 90.0) + 0.1 * pow(shine, 12.0) + 0.12 * pow(edge, 2.5);
    }
    let rgb = min(color * light + gloss * input.color.a, vec3<f32>(input.color.a));
    return vec4<f32>(rgb, input.color.a);
}

// A see-through shell: nearly clear where it faces the camera and denser toward its edges,
// as light crosses more of it there, with a crisp highlight.
@fragment
fn glass(input: Varyings) -> @location(0) vec4<f32> {
    let normal = facing(input);
    let edge = pow(1.0 - normal.z, 2.2);
    let alpha = input.color.a * mix(0.05, 0.7, edge);
    let tint = input.color.rgb / max(input.color.a, 1e-4);
    let half_way = normalize(normalize(KEY_LIGHT) + vec3<f32>(0.0, 0.0, 1.0));
    let highlight = 0.35 * pow(max(dot(normal, half_way), 0.0), 140.0);
    let rgb = tint * (0.6 + 0.5 * edge) * alpha + highlight;
    return vec4<f32>(min(rgb, vec3<f32>(1.0)), min(alpha + highlight, 1.0));
}

// Glyph quads sit on whole pixels, so each pixel reads exactly one atlas texel.
@fragment
fn glyph(input: Varyings) -> @location(0) vec4<f32> {
    return textureLoad(atlas, vec2<i32>(floor(input.texel)), 0) * input.color;
}
