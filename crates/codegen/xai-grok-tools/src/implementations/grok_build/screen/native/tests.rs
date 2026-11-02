//! Behavior ported from the Three.js renderer fixture and native Qt self-test.

use super::scene::{Click, Interaction, Model, close_button, reset_button};
use glam::{Vec2, Vec3};
use xai_grok_tools::implementations::grok_build::screen::{ModelPart, OverlayScene, Primitive};

fn model(primitive: Primitive) -> Model {
    Model::new(&[ModelPart {
        primitive,
        position: [0.0; 3],
        rotation: [0.0; 3],
        scale: [2.0; 3],
        color: "#88aaff".to_owned(),
    }])
}

#[test]
fn model_starts_centered_and_input_bounds_use_logical_units() {
    let model = model(Primitive::Box);
    let bounds = model.interactive_bounds(1280.0, 800.0).unwrap();
    assert!(bounds.contains(Vec2::new(640.0, 400.0)));
    assert!(bounds.width > 0.0 && bounds.height > 0.0);
    assert!(!bounds.contains(Vec2::ZERO));
    assert!(close_button(1280.0).contains(Vec2::new(1230.0, 25.0)));
    assert!(reset_button(1280.0).contains(Vec2::new(1142.0, 25.0)));
}

#[test]
fn small_models_keep_their_shape_and_relative_placement_after_translation() {
    let parts: Vec<_> = [
        Primitive::Box,
        Primitive::Sphere,
        Primitive::Cylinder,
        Primitive::Cone,
        Primitive::Torus,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, primitive)| ModelPart {
        primitive,
        position: [i as f64 * 0.003, 0.0, 0.0],
        rotation: [0.1, 0.2, 0.3],
        scale: [0.001, 0.002, 0.003],
        color: "#88aaff".to_owned(),
    })
    .collect();
    let original = Model::new(&parts);
    let mut translated = parts;
    for part in &mut translated {
        for (coordinate, offset) in part
            .position
            .iter_mut()
            .zip([90_000.0, -80_000.0, 70_000.0])
        {
            *coordinate += offset;
        }
    }
    OverlayScene::Model3d {
        parts: translated.clone(),
    }
    .validate()
    .unwrap();
    let translated = Model::new(&translated);
    assert_eq!(original.vertices.len(), translated.vertices.len());
    for (a, b) in original.vertices.iter().zip(&translated.vertices) {
        assert!(
            Vec3::from_array(a.position).abs_diff_eq(Vec3::from_array(b.position), 1e-8),
            "Translation changed mesh geometry: {:?} -> {:?}",
            a.position,
            b.position
        );
    }
    assert!(original.min.abs_diff_eq(translated.min, 1e-8));
    assert!(original.max.abs_diff_eq(translated.max, 1e-8));
    let box_model = Model::new(&[ModelPart {
        primitive: Primitive::Box,
        position: [100_000.0; 3],
        rotation: [0.0; 3],
        scale: [0.001; 3],
        color: "#88aaff".to_owned(),
    }]);
    assert!(
        (box_model.max - box_model.min).abs_diff_eq(Vec3::splat(0.001), 1e-8),
        "Valid small box collapsed: {:?} .. {:?}",
        box_model.min,
        box_model.max
    );
}

#[test]
fn toolbar_buttons_remain_separate_and_clickable_at_supported_widths() {
    let model = model(Primitive::Box);
    for width in [160.0, 170.0, 179.0, 180.0, 191.0, 192.0, 1280.0] {
        let close = close_button(width);
        let reset = reset_button(width);
        let overlap_x = close.x.max(reset.x) < (close.x + close.width).min(reset.x + reset.width);
        let overlap_y = close.y.max(reset.y) < (close.y + close.height).min(reset.y + reset.height);
        assert!(
            !(overlap_x && overlap_y),
            "Toolbar buttons overlap at width {width}"
        );
        for (button, expected) in [(reset, Click::Reset), (close, Click::Close)] {
            assert!(button.x >= 0.0 && button.x + button.width <= width);
            assert!(button.y >= 0.0 && button.y + button.height <= 120.0);
            // Corners exercise the former overlap, rather than only the safe centers.
            for dx in [1.0, button.width - 1.0] {
                for dy in [1.0, button.height - 1.0] {
                    let point = Vec2::new(button.x + dx, button.y + dy);
                    let mut input = Interaction::default();
                    input.press(0x110, point, width, Some(&model), 120.0);
                    assert_eq!(
                        input.release(0x110, point, width),
                        expected,
                        "width={width}, point={point:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn reset_recovers_camera_and_bounds_after_offscreen_pan() {
    let mut model = model(Primitive::Box);
    let initial = model.camera;
    let bounds = model.interactive_bounds(1280.0, 800.0);
    model.camera.pan(Vec2::new(20_000.0, 0.0), 800.0);
    assert!(model.interactive_bounds(1280.0, 800.0).is_none());
    model.camera.reset();
    assert_eq!(model.camera, initial);
    assert_eq!(model.interactive_bounds(1280.0, 800.0), bounds);
}

#[test]
fn orbit_zoom_and_pan_change_camera_and_projected_regions() {
    let mut model = model(Primitive::Box);
    let original = model.interactive_bounds(1280.0, 800.0);
    model.camera.orbit(Vec2::new(120.0, 45.0), 800.0);
    assert_ne!(model.interactive_bounds(1280.0, 800.0), original);
    let before = model.interactive_bounds(1280.0, 800.0).unwrap();
    model.camera.zoom(-120.0);
    assert!(model.interactive_bounds(1280.0, 800.0).unwrap().width > before.width);
    model.camera.pan(Vec2::new(100.0, 0.0), 800.0);
    assert_ne!(model.interactive_bounds(1280.0, 800.0), Some(before));
}

#[test]
fn all_primitives_have_finite_transformed_geometry_and_normals() {
    for primitive in [
        Primitive::Box,
        Primitive::Sphere,
        Primitive::Cylinder,
        Primitive::Cone,
        Primitive::Torus,
    ] {
        let model = model(primitive);
        assert!(!model.vertices.is_empty());
        assert_eq!(model.vertices.len() % 3, 0);
        for vertex in &model.vertices {
            assert!(Vec3::from_array(vertex.position).is_finite());
            assert!(Vec3::from_array(vertex.normal).is_finite());
        }
    }
}

#[test]
fn annotations_are_transparent_scaled_and_literal() -> anyhow::Result<()> {
    use super::render::Renderer;
    use xai_grok_tools::implementations::grok_build::screen::{Annotation, OverlayScene};

    let scene = OverlayScene::Annotations {
        items: vec![
            Annotation::Rectangle {
                x: 40.0,
                y: 80.0,
                width: 200.0,
                height: 100.0,
                color: "#ff3366".to_owned(),
            },
            Annotation::Arrow {
                x: 50.0,
                y: 200.0,
                to_x: 200.0,
                to_y: 200.0,
                color: "#88ffaa".to_owned(),
            },
            Annotation::Text {
                x: 50.0,
                y: 60.0,
                text: "<script>中文 & literal</script>".to_owned(),
                color: "#ffffff".to_owned(),
            },
        ],
    };
    let mut renderer = Renderer::new();
    for scale in [1.0, 1.5, 2.0, 2.5] {
        let image = renderer.draw(&scene, None, 1280, 800, scale)?;
        assert_eq!(
            (image.width(), image.height()),
            ((1280.0 * scale) as u32, (800.0 * scale) as u32)
        );
        let stroke = image
            .pixel((40.0 * scale) as u32, (110.0 * scale) as u32)
            .unwrap();
        assert!(stroke.red() > 220 && stroke.green() < 100);
        assert_eq!(
            image
                .pixel((10.0 * scale) as u32, (300.0 * scale) as u32)
                .unwrap()
                .alpha(),
            0
        );
        assert!(
            image
                .pixels()
                .iter()
                .all(|pixel| pixel.red() <= pixel.alpha()
                    && pixel.green() <= pixel.alpha()
                    && pixel.blue() <= pixel.alpha())
        );
        let text_pixels = (30..65)
            .flat_map(|y| (45..350).map(move |x| (x, y)))
            .filter(|(x, y)| {
                image
                    .pixel((*x as f32 * scale) as u32, (*y as f32 * scale) as u32)
                    .unwrap()
                    .alpha()
                    > 0
            })
            .count();
        assert!(
            text_pixels > 100,
            "Literal multilingual text did not render"
        );
    }
    assert!(renderer.draw(&scene, None, u32::MAX, 800, 2.0).is_err());
    assert!(renderer.draw(&scene, None, 1280, 800, f32::NAN).is_err());
    Ok(())
}

#[test]
fn gestures_keep_their_press_action_and_require_matching_release() {
    use super::scene::{Click, Interaction};
    let mut model = model(Primitive::Box);
    let original = model.camera;
    let mut input = Interaction::default();
    input.press(0x110, Vec2::new(1230.0, 25.0), 1280.0, Some(&model), 800.0);
    assert!(input.is_dragging());
    assert!(!input.motion(Vec2::new(640.0, 400.0), Some(&mut model), 800.0));
    assert_eq!(
        input.release(0x111, Vec2::new(1230.0, 25.0), 1280.0),
        Click::None
    );
    assert!(
        input.is_dragging(),
        "An unrelated button release must not drop pointer capture"
    );
    assert_eq!(
        input.release(0x110, Vec2::new(640.0, 400.0), 1280.0),
        Click::None
    );
    assert_eq!(model.camera, original);
    input.press(0x110, Vec2::new(640.0, 400.0), 1280.0, Some(&model), 800.0);
    assert!(input.motion(Vec2::new(1230.0, 25.0), Some(&mut model), 800.0));
    assert_eq!(
        input.release(0x110, Vec2::new(1230.0, 25.0), 1280.0),
        Click::None
    );
    assert_ne!(
        model.camera, original,
        "Orbit must continue across a toolbar button"
    );
    input.press(0x110, Vec2::new(1142.0, 25.0), 1280.0, Some(&model), 800.0);
    assert_eq!(
        input.release(0x110, Vec2::new(1142.0, 25.0), 1280.0),
        Click::Reset
    );
    input.press(0x110, Vec2::new(1230.0, 25.0), 1280.0, None, 800.0);
    assert_eq!(
        input.release(0x110, Vec2::new(1230.0, 25.0), 1280.0),
        Click::Close
    );
    input.press(0x110, Vec2::new(1142.0, 25.0), 1280.0, None, 800.0);
    assert!(
        !input.is_dragging(),
        "Annotation scenes have no Reset control"
    );
}

#[test]
fn very_small_valid_scales_do_not_produce_nonfinite_vertices() {
    let model = Model::new(&[ModelPart {
        primitive: Primitive::Sphere,
        position: [100_000.0; 3],
        rotation: [100_000.0; 3],
        scale: [f64::MIN_POSITIVE, 0.000_001, 100_000.0],
        color: "#aabbcc".to_owned(),
    }]);
    assert!(
        model
            .vertices
            .iter()
            .all(|vertex| Vec3::from_array(vertex.position).is_finite()
                && Vec3::from_array(vertex.normal).is_finite())
    );
    let bounds = model.interactive_bounds(1280.0, 800.0).unwrap();
    assert!(bounds.width.is_finite() && bounds.height.is_finite());
}
