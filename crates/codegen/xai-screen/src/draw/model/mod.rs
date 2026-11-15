//! The drawing model: shapes ([`Shape`]) in logical units from a monitor's top-left corner,
//! keys that change them over time ([`Key`], [`Repeat`]), charts ([`Chart`]) whose data
//! the overlay lays out, and the rules every drawing obeys ([`Request::check`]).

mod animation;
mod chart;
mod shape;
mod style;

pub use animation::{CameraKey, Key, ObjectKey, Repeat, seconds};
pub use chart::{Axis, Chart, Plot, Scale, Series};
pub(crate) use shape::COIL_POINTS;
pub use shape::{Arrows, Camera, Color, Material, Pattern, Point3, SceneObject, Shape};
pub use style::{Palette, Rgb, Style, hex};

use serde::Deserialize;

/// One request line: the `screen_draw` tool's input, as the model wrote it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub shapes: Vec<Shape>,
    #[serde(default)]
    pub repeat: Repeat,
}

impl Request {
    /// Reject a drawing that breaks a rule, or that leaves a `width` × `height` monitor at
    /// any key. Points on its edge fit.
    ///
    /// # Errors
    /// Names the first invalid shape and the rule it breaks.
    pub fn check(&self, width: f64, height: f64) -> Result<(), String> {
        validate(&self.shapes)?;
        match self
            .shapes
            .iter()
            .position(|shape| !shape.fits(width, height))
        {
            None => Ok(()),
            Some(index) => Err(invalid(
                index,
                &self.shapes[index],
                &format!("it extends past the monitor, which is {width} × {height} logical units"),
            )),
        }
    }
}

fn invalid(index: usize, shape: &Shape, problem: &str) -> String {
    format!("Shape {} ({}): {problem}", index + 1, shape.kind())
}

/// The rules every drawing obeys, wherever it shows.
fn validate(shapes: &[Shape]) -> Result<(), String> {
    if shapes.len() > shape::MAX_SHAPES {
        return Err(format!("Use at most {} shapes", shape::MAX_SHAPES));
    }
    for (index, shape) in shapes.iter().enumerate() {
        shape
            .check()
            .map_err(|problem| invalid(index, shape, problem))?;
        shape
            .check_keys()
            .map_err(|problem| invalid(index, shape, &problem))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn shapes(value: serde_json::Value) -> Vec<Shape> {
        serde_json::from_value(value).unwrap()
    }

    fn error(result: Result<(), String>) -> String {
        result.unwrap_err()
    }

    #[test]
    fn shapes_take_theme_or_hex_colors_and_reject_unknown_kinds_and_fields() {
        let drawing = shapes(json!([
            {"kind": "box", "x": 104, "y": 82, "width": 120, "height": 36, "label": "Click Save"},
            {"kind": "line", "x": 400, "y": 300, "to_x": 230, "to_y": 100, "color": "red", "arrows": "end"},
            {"kind": "text", "x": 420, "y": 300, "text": "Then choose\na folder", "color": "#E8A0B4"}
        ]));
        assert!(matches!(
            drawing[0],
            Shape::Box {
                color: Color::Accent,
                ..
            }
        ));
        assert!(matches!(
            drawing[1],
            Shape::Line {
                color: Color::Red,
                arrows: Arrows::End,
                ..
            }
        ));
        assert!(matches!(
            drawing[2],
            Shape::Text {
                color: Color::Rgb([0xe8, 0xa0, 0xb4]),
                ..
            }
        ));
        assert!(validate(&drawing).is_ok());
        for bad in [
            json!([{"kind": "circle", "x": 1, "y": 1}]),
            json!([{"kind": "box", "x": 1, "y": 1, "width": 1, "height": 1, "color": "pink"}]),
            json!([{"kind": "box", "x": 1, "y": 1, "width": 1, "height": 1, "color": "#ff00"}]),
        ] {
            assert!(serde_json::from_value::<Vec<Shape>>(bad).is_err());
        }
    }

    #[test]
    fn validation_names_the_shape_and_the_rule() {
        let box_shape = |width: f64, label: &str| {
            shapes(
                json!([{"kind": "box", "x": 0, "y": 0, "width": width, "height": 10, "label": label}]),
            )
        };
        assert_eq!(
            error(validate(&box_shape(0.0, "ok"))),
            "Shape 1 (box): width and height must be positive"
        );
        assert!(error(validate(&box_shape(10.0, " "))).contains("must not be empty"));
        assert!(error(validate(&box_shape(10.0, "two\nlines"))).contains("control characters"));
        assert!(error(validate(&box_shape(10.0, &"x".repeat(201)))).contains("at most 200"));
        let line = shapes(json!([{"kind": "line", "x": 5, "y": 5, "to_x": 5, "to_y": 5}]));
        assert!(error(validate(&line)).contains("must have length"));
        let far = shapes(json!([{"kind": "text", "x": 1e9, "y": 0, "text": "far"}]));
        assert!(error(validate(&far)).contains("finite numbers"));
        let many =
            vec![shapes(json!([{"kind": "text", "x": 0, "y": 0, "text": "a"}]))[0].clone(); 65];
        assert!(error(validate(&many)).contains("at most 64"));
    }

    #[test]
    fn shapes_fit_an_area_at_every_key() {
        let edge =
            &shapes(json!([{"kind": "box", "x": 0, "y": 0, "width": 1280, "height": 800}]))[0];
        assert!(edge.fits(1280.0, 800.0) && !edge.fits(1279.0, 800.0));
        let leaving = &shapes(json!([{
            "kind": "text", "x": 10, "y": 10, "text": "bye", "keys": [{"at": 1, "x": 1300}]
        }]))[0];
        assert!(!leaving.fits(1280.0, 800.0));
        let request = |value: serde_json::Value| serde_json::from_value::<Request>(value);
        let past = request(json!({"shapes": [
            {"kind": "text", "x": 10, "y": 10, "text": "fits"},
            {"kind": "line", "x": 10, "y": 10, "to_x": 1300, "to_y": 10}
        ]}));
        assert_eq!(
            past.unwrap().check(1280.0, 800.0).unwrap_err(),
            "Shape 2 (line): it extends past the monitor, which is 1280 × 800 logical units"
        );
        assert!(request(json!({})).is_err());
        assert!(request(json!({"shapes": [], "monitor": "eDP-1"})).is_err());
    }

    #[test]
    fn scenes_check_their_camera_objects_and_meshes() {
        let scene = |camera: serde_json::Value, objects: serde_json::Value| {
            shapes(json!([{
                "kind": "scene", "x": 0, "y": 0, "width": 300, "height": 200,
                "camera": camera, "objects": objects
            }]))
        };
        let triangle = json!([{
            "kind": "mesh", "vertices": [[0, 0, 0], [1, 0, 0], [0, 1, 0]], "faces": [[0, 1, 2]]
        }]);
        assert!(validate(&scene(json!({}), triangle.clone())).is_ok());
        assert!(error(validate(&scene(json!({"pitch": 91}), triangle))).contains("pitch"));
        let bad_index = json!([{
            "kind": "mesh", "vertices": [[0, 0, 0]], "faces": [[0, 1, 2]]
        }]);
        assert_eq!(
            error(validate(&scene(json!({}), bad_index))),
            "Shape 1 (scene): mesh faces must use existing vertex indices"
        );
        let flat = json!([{"kind": "cuboid", "center": [0, 0, 0], "size": [1, 0, 1]}]);
        assert!(error(validate(&scene(json!({}), flat))).contains("sizes must be positive"));
        assert!(error(validate(&scene(json!({}), json!([])))).contains("1 to 64 objects"));
        let tube = |points: serde_json::Value, radii: serde_json::Value| {
            scene(
                json!({}),
                json!([{"kind": "tube", "points": points, "radius": 0.2, "radii": radii}]),
            )
        };
        let bend = json!([[0, 0, 0], [1, 1, 0], [2, 0, 1]]);
        assert!(validate(&tube(bend.clone(), json!([0.1, 0.3, 0.1]))).is_ok());
        assert!(error(validate(&tube(bend, json!([0.1])))).contains("one radius"));
        assert!(error(validate(&tube(json!([[0, 0, 0]]), json!(null)))).contains("2 to 64"));
        let bowel = |pouches: u32| {
            scene(
                json!({}),
                json!([
                    {"kind": "tube", "points": [[0, 0, 0], [4, 0, 0]], "radius": 0.5, "pouches": pouches},
                    {"kind": "coil", "center": [0, 0, 0], "size": [3, 2, 1], "radius": 0.2},
                ]),
            )
        };
        assert!(validate(&bowel(12)).is_ok());
        let swelling = scene(
            json!({}),
            json!([{
                "kind": "tube", "points": [[0, 0, 0], [4, 0, 0]], "radius": 0.5,
                "keys": [{"at": 1, "color": "red"}, {"at": 2, "radii": [0.5, 1]}],
            }]),
        );
        assert!(validate(&swelling).is_ok());
        assert!(error(validate(&bowel(0))).contains("pouches"));
        let coils = scene(
            json!({}),
            json!([
                {"kind": "coil", "center": [0, 0, 0], "size": [3, 2, 1], "radius": 0.2},
            ])
            .as_array()
            .unwrap()
            .iter()
            .cycle()
            .take(5)
            .cloned()
            .collect(),
        );
        assert!(error(validate(&coils)).contains("512 points"));
        let blob = |balls: serde_json::Value| {
            scene(
                json!({}),
                json!([{"kind": "blob", "balls": balls, "material": "flesh"}]),
            )
        };
        assert!(validate(&blob(json!([[0, 0, 0, 1], [1, 0, 0, 0.5]]))).is_ok());
        assert!(error(validate(&blob(json!([[0, 0, 0, 0]])))).contains("radius"));
        assert!(error(validate(&blob(json!([])))).contains("1 to 48"));
        let label = |text: &str| {
            scene(
                json!({}),
                json!([
                    {"kind": "sphere", "center": [0, 0, 0], "radius": 1, "material": "glass"},
                    {"kind": "label", "point": [0, 1, 0], "text": text},
                ]),
            )
        };
        assert!(validate(&label("Liver")).is_ok());
        assert!(validate(&label("")).is_err());
    }

    #[test]
    fn fill_is_an_opacity_and_a_box_must_stay_visible() {
        let box_shape = |extra: serde_json::Value| {
            let mut shape = json!({"kind": "box", "x": 0, "y": 0, "width": 10, "height": 10});
            shape
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            shapes(json!([shape]))
        };
        assert!(validate(&box_shape(json!({"fill": 1, "border": false}))).is_ok());
        assert!(validate(&box_shape(json!({"fill": 0}))).is_ok());
        assert!(matches!(
            box_shape(json!({}))[0],
            Shape::Box {
                fill: None,
                border: true,
                ..
            }
        ));
        let style = Style::default();
        let plain = &box_shape(json!({}))[0];
        assert_eq!((plain.fill(), plain.line_width(&style)), (0.08, 2.0));
        let line = &shapes(json!([{"kind": "line", "x": 0, "y": 0, "to_x": 5, "to_y": 5}]))[0];
        assert_eq!((line.fill(), line.line_width(&style)), (0.0, 3.0));
        assert_eq!(
            error(validate(&box_shape(json!({"fill": 1.5})))),
            "Shape 1 (box): fill must be between 0 and 1"
        );
        assert!(
            error(validate(&box_shape(json!({"fill": 0, "border": false}))))
                .contains("needs a fill above 0")
        );
    }

    #[test]
    fn keys_move_fields_through_time() {
        let shape = &shapes(json!([{
            "kind": "box", "x": 0, "y": 0, "width": 10, "height": 10,
            "keys": [{"at": 2, "x": 100}, {"at": 4, "x": 0, "ease": "step"}]
        }]))[0];
        let x = |t: f64| match shape.at(t, &Style::default()) {
            Shape::Box { x, .. } => x,
            other => panic!("expected a box, got {other:?}"),
        };
        assert_eq!([x(1.0), x(3.0), x(4.0), x(9.0)], [50.0, 100.0, 0.0, 0.0]);
        assert_eq!(Repeat::Once.time(5.0, 4.0), (4.0, true));
        assert_eq!(Repeat::Loop.time(5.0, 4.0), (1.0, false));
        assert_eq!(Repeat::Bounce.time(5.0, 4.0), (3.0, false));
        let line = |keys: serde_json::Value| {
            shapes(json!([{"kind": "line", "x": 0, "y": 0, "to_x": 5, "to_y": 5, "keys": keys}]))
        };
        assert_eq!(
            error(validate(&line(json!([{"at": 1, "width": 3}])))),
            "Shape 1 (line): a line key cannot set width"
        );
        let backward = line(json!([{"at": 2, "x": 1}, {"at": 1, "x": 2}]));
        assert!(error(validate(&backward)).contains("key times must rise"));
        let opaque = line(json!([{"at": 1, "opacity": 2}]));
        assert!(error(validate(&opaque)).contains("at 1 s, opacity must be between 0 and 1"));
        let turning = &shapes(json!([{
            "kind": "scene", "x": 0, "y": 0, "width": 300, "height": 200,
            "camera": {"yaw": 0, "pitch": 20}, "keys": [{"at": 2, "camera": {"yaw": 90}}],
            "objects": [{"kind": "sphere", "center": [0, 0, 0], "radius": 1}]
        }]))[0];
        match turning.at(1.0, &Style::default()) {
            Shape::Scene { camera, .. } => assert_eq!([camera.yaw, camera.pitch], [45.0, 20.0]),
            other => panic!("expected a scene, got {other:?}"),
        }
        let turned_line = line(json!([{"at": 1, "camera": {"yaw": 90}}]));
        assert!(error(validate(&turned_line)).contains("a line key cannot set camera"));
    }

    #[test]
    fn charts_check_their_data() {
        let chart = |extra: serde_json::Value| {
            let mut chart = json!({
                "kind": "chart", "x": 0, "y": 0, "width": 400, "height": 300, "plot": "bar",
                "categories": ["a", "b"], "series": [{"values": [3, 7]}, {"values": [-2, 4]}]
            });
            let fields = chart.as_object_mut().unwrap();
            fields.extend(extra.as_object().unwrap().clone());
            shapes(json!([chart]))
        };
        let only = |shapes: Vec<Shape>| match shapes.into_iter().next() {
            Some(Shape::Chart(chart)) => chart,
            other => panic!("expected a chart, got {other:?}"),
        };
        validate(&chart(json!({}))).unwrap();
        let unknown = json!([{"kind": "chart", "x": 0, "y": 0, "width": 9, "height": 9,
            "plot": "bar", "series": [], "colour": "red"}]);
        assert!(serde_json::from_value::<Vec<Shape>>(unknown).is_err());
        assert_eq!(
            error(validate(&chart(json!({"categories": ["a"]})))),
            "Shape 1 (chart): each series needs one value per category"
        );
        assert!(error(validate(&chart(json!({"plot": "pie"})))).contains("a pie takes one series"));
        let surface = chart(json!({"plot": "surface"}));
        assert!(error(validate(&surface)).contains("a grid plot takes one series"));

        let keys = json!([{"at": 1, "series": [{"values": [1, 30]}]}]);
        let keyed = chart(json!({"series": [{"values": [1, 2]}], "keys": keys}));
        validate(&keyed).unwrap();
        let halfway = only(vec![keyed[0].at(0.5, &Style::default())]);
        assert_eq!(halfway.series[0].values, [1.0, 16.0]);

        let candles = |points: serde_json::Value| {
            chart(json!({"plot": "candlestick", "series": [{"points": points}]}))
        };
        validate(&candles(json!([[10, 14, 9, 13], [13, 13, 6, 7]]))).unwrap();
        let high_too_low = candles(json!([[10, 12, 9, 13], [13, 13, 6, 7]]));
        assert!(error(validate(&high_too_low)).contains("high the highest"));

        let sankey = |links: serde_json::Value| {
            shapes(
                json!([{"kind": "chart", "x": 0, "y": 0, "width": 400, "height": 300,
                "plot": "sankey", "links": links}]),
            )
        };
        let flows = sankey(json!([
            {"from": "pay", "to": "rent", "value": 40}, {"from": "pay", "to": "saved", "value": 20},
            {"from": "bonus", "to": "saved", "value": 10}, {"from": "rent", "to": "landlord", "value": 40},
        ]));
        validate(&flows).unwrap();
        let nodes = only(flows).nodes().unwrap();
        let placed: Vec<_> = nodes
            .iter()
            .map(|n| (n.name.as_str(), n.column, n.value))
            .collect();
        assert_eq!(
            placed,
            [
                ("pay", 0, 60.0),
                ("rent", 1, 40.0),
                ("saved", 1, 30.0),
                ("bonus", 0, 10.0),
                ("landlord", 2, 40.0),
            ]
        );
        let looped = sankey(
            json!([{"from": "a", "to": "b", "value": 1}, {"from": "b", "to": "a", "value": 1}]),
        );
        assert!(error(validate(&looped)).contains("must not loop"));
        assert!(
            error(validate(&chart(
                json!({"links": [{"from": "a", "to": "b", "value": 1}]})
            )))
            .contains("only a sankey")
        );
    }
}
