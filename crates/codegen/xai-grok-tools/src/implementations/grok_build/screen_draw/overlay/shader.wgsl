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
}

@vertex
fn vertex_main(input: Vertex, @builtin(instance_index) index: u32) -> Varyings {
    let camera = cameras[index];
    var output: Varyings;
    output.position = camera.clip_from_world * vec4<f32>(input.position, 1.0);
    output.normal = (camera.view_from_world * vec4<f32>(input.normal, 0.0)).xyz;
    output.color = input.color;
    output.texel = input.texel;
    return output;
}

@fragment
fn solid(input: Varyings) -> @location(0) vec4<f32> {
    return input.color;
}

// A key light from the upper left and a soft fill from the lower right, both fixed to the
// camera, plus a small highlight. Surfaces stay between about 40% and 100% of their color.
@fragment
fn lit(input: Varyings) -> @location(0) vec4<f32> {
    var normal = normalize(input.normal);
    // Open meshes show both sides; light the side that faces the camera.
    if normal.z < 0.0 {
        normal = -normal;
    }
    let key_light = normalize(vec3<f32>(-0.4, 0.6, 0.7));
    let fill_light = normalize(vec3<f32>(0.5, -0.3, 0.6));
    let diffuse = 0.42 + 0.5 * max(dot(normal, key_light), 0.0) + 0.12 * max(dot(normal, fill_light), 0.0);
    let highlight = 0.16 * pow(max(dot(normal, normalize(key_light + vec3<f32>(0.0, 0.0, 1.0))), 0.0), 48.0);
    let rgb = min(input.color.rgb * diffuse + highlight * input.color.a, vec3<f32>(input.color.a));
    return vec4<f32>(rgb, input.color.a);
}

// Glyph quads sit on whole pixels, so each pixel reads exactly one atlas texel.
@fragment
fn glyph(input: Varyings) -> @location(0) vec4<f32> {
    return textureLoad(atlas, vec2<i32>(floor(input.texel)), 0) * input.color;
}
