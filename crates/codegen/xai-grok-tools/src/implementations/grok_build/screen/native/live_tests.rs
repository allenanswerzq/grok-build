//! Opt-in tests of real Vulkan pixels. No synthetic GPU or windowing replacements.

use super::{render::Renderer, scene::Model};
use anyhow::Result;
use glam::Vec2;
use xai_grok_tools::implementations::grok_build::screen::{
    Annotation, ModelPart, OverlayScene, Primitive,
};

#[test]
fn vulkan_renders_every_primitive_with_transparency_and_camera_controls() -> Result<()> {
    let mut renderer = Renderer::new();
    for primitive in [
        Primitive::Box,
        Primitive::Sphere,
        Primitive::Cylinder,
        Primitive::Cone,
        Primitive::Torus,
    ] {
        let parts = vec![ModelPart {
            primitive,
            position: [0.0; 3],
            rotation: [0.1, 0.2, 0.3],
            scale: [2.0, 1.5, 1.0],
            color: "#88aaff".to_owned(),
        }];
        let scene = OverlayScene::Model3d {
            parts: parts.clone(),
        };
        let mut model = Model::new(&parts);
        let initial = renderer.draw(&scene, Some(&model), 640, 400, 1.5)?;
        assert_eq!((initial.width(), initial.height()), (960, 600));
        assert_eq!(initial.pixel(10, 100).unwrap().alpha(), 0);
        assert!(
            initial
                .pixels()
                .iter()
                .all(|pixel| pixel.red() <= pixel.alpha()
                    && pixel.green() <= pixel.alpha()
                    && pixel.blue() <= pixel.alpha()),
            "Vulkan pixels must be premultiplied"
        );
        assert!(
            initial
                .pixels()
                .iter()
                .any(|pixel| pixel.alpha() > 0 && pixel.alpha() < 255),
            "Multisample edges are missing"
        );
        let bounds = model.interactive_bounds(640.0, 400.0).unwrap();
        for y in (90..initial.height()).step_by(4) {
            for x in (0..initial.width()).step_by(4) {
                if initial.pixel(x, y).unwrap().alpha() > 0 {
                    assert!(
                        bounds.contains(Vec2::new(x as f32 / 1.5, y as f32 / 1.5)),
                        "{primitive:?} rendered outside its logical input bounds"
                    );
                }
            }
        }
        let colored = (100..500)
            .flat_map(|y| (200..760).map(move |x| (x, y)))
            .filter(|(x, y)| initial.pixel(*x, *y).unwrap().alpha() > 0)
            .count();
        assert!(
            colored > 500,
            "Primitive {primitive:?} is absent from the native pixels"
        );
        model.camera.orbit(Vec2::new(100.0, 30.0), 400.0);
        let orbit = renderer.draw(&scene, Some(&model), 640, 400, 1.5)?;
        assert_ne!(
            initial.data(),
            orbit.data(),
            "Orbit left {primitive:?} pixels unchanged"
        );
        model.camera.zoom(-240.0);
        let zoom = renderer.draw(&scene, Some(&model), 640, 400, 1.5)?;
        assert_ne!(
            orbit.data(),
            zoom.data(),
            "Zoom left {primitive:?} pixels unchanged"
        );
        model.camera.pan(Vec2::new(100.0, 0.0), 400.0);
        let pan = renderer.draw(&scene, Some(&model), 640, 400, 1.5)?;
        assert_ne!(
            zoom.data(),
            pan.data(),
            "Pan left {primitive:?} pixels unchanged"
        );
        model.camera.reset();
        assert_eq!(
            initial.data(),
            renderer.draw(&scene, Some(&model), 640, 400, 1.5)?.data()
        );
        let annotations = OverlayScene::Annotations {
            items: vec![Annotation::Text {
                x: 50.0,
                y: 60.0,
                text: "Replaced 3D".to_owned(),
                color: "#ffffff".to_owned(),
            }],
        };
        let image = renderer.draw(&annotations, None, 640, 400, 1.5)?;
        assert_eq!(
            image.pixel(480, 300).unwrap().alpha(),
            0,
            "Replacement retained the previous mesh"
        );
        assert_eq!(
            image
                .pixel((502.0 * 1.5) as u32, 25 * 3 / 2)
                .unwrap()
                .alpha(),
            0,
            "Replacement retained Reset"
        );
    }
    Ok(())
}

#[test]
fn small_mesh_pixels_are_unchanged_by_large_global_translations() -> Result<()> {
    let mut renderer = Renderer::new();
    let mut reference = None;
    for position in [[0.0; 3], [100_000.0; 3], [-100_000.0; 3]] {
        let parts = vec![ModelPart {
            primitive: Primitive::Box,
            position,
            rotation: [0.1, 0.2, 0.3],
            scale: [0.001, 0.002, 0.003],
            color: "#88aaff".to_owned(),
        }];
        let scene = OverlayScene::Model3d {
            parts: parts.clone(),
        };
        scene.validate()?;
        let mut model = Model::new(&parts);
        model.camera.zoom(-3000.0);
        let image = renderer.draw(&scene, Some(&model), 640, 400, 1.0)?;
        let mesh_pixels = (100..300)
            .flat_map(|y| (200..440).map(move |x| (x, y)))
            .filter(|(x, y)| image.pixel(*x, *y).unwrap().alpha() > 0)
            .count();
        assert!(mesh_pixels > 100, "Small mesh disappeared at {position:?}");
        if let Some(reference) = &reference {
            assert_eq!(
                image.data(),
                reference,
                "Translation changed rendered pixels at {position:?}"
            );
        } else {
            reference = Some(image.data().to_vec());
        }
    }
    Ok(())
}

#[test]
fn full_scale_frames_remain_premultiplied() -> Result<()> {
    let parts = vec![ModelPart {
        primitive: Primitive::Torus,
        position: [0.0; 3],
        rotation: [0.2; 3],
        scale: [2.0; 3],
        color: "#88aaff".to_owned(),
    }];
    let scene = OverlayScene::Model3d {
        parts: parts.clone(),
    };
    let mut model = Model::new(&parts);
    let mut renderer = Renderer::new();
    renderer.draw(&scene, Some(&model), 1280, 800, 2.0)?;
    let mut render_time = std::time::Duration::ZERO;
    for _ in 0..3 {
        model.camera.orbit(Vec2::new(10.0, 5.0), 800.0);
        let start = std::time::Instant::now();
        let image = renderer.draw(&scene, Some(&model), 1280, 800, 2.0)?;
        render_time += start.elapsed();
        assert_eq!((image.width(), image.height()), (2560, 1600));
        assert!(
            image
                .pixels()
                .iter()
                .all(|pixel| pixel.red() <= pixel.alpha()
                    && pixel.green() <= pixel.alpha()
                    && pixel.blue() <= pixel.alpha())
        );
    }
    eprintln!(
        "2560×1600 Rust overlay render/readback: {:?} per frame",
        render_time / 3
    );
    Ok(())
}
