struct Camera { view_projection: mat4x4<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
}

@vertex fn vertex_main(@location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>, @location(2) color: vec3<f32>) -> VertexOutput {
    var output: VertexOutput;
    output.position = camera.view_projection * vec4<f32>(position, 1.0);
    output.normal = normal;
    output.color = color;
    return output;
}

@fragment fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let light = normalize(vec3<f32>(3.0, 4.0, 5.0));
    let intensity = 0.3 + 0.7 * abs(dot(normalize(input.normal), light));
    let linear = input.color * intensity;
    // Encode before resolving coverage: the raster/Wayland buffer expects
    // premultiplied sRGB bytes, including partially covered mesh edges.
    let srgb = select(linear * 12.92,
        1.055 * pow(linear, vec3<f32>(1.0 / 2.4)) - 0.055,
        linear > vec3<f32>(0.0031308));
    return vec4<f32>(srgb, 1.0);
}
