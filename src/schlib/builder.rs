use std::collections::HashMap;

use serde_json::Value;

use crate::error::Result;
use crate::schlib::common;
use crate::util::nested_string;
use super::{Arc, Component, Ellipse, Implementation, Label, MapDefiner, Pin, Polyline, Rectangle, SchlibMetadata, SchlibParameter};

const WHITE_BGR: i32 = 0xFFFFFF;
const BODY_LINE_WIDTH_INDEX: i32 = 1;
const GRAPHIC_LINE_WIDTH_INDEX: i32 = 1;
const PIN_LENGTH_UNITS: f64 = 20.0;

// ── Entry point ───────────────────────────────────────────────────────────────

pub(super) fn build_component(
    payload: &Value,
    component_name: &str,
    metadata: &SchlibMetadata,
) -> Result<Component> {
    let rows = common::parse_easyeda_rows(payload)?;
    let mut parts: Vec<PartRaw> = Vec::new();
    let mut current_part_index = None;
    let mut has_part_rows = false;
    let mut attr_by_parent: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut root_attrs: HashMap<String, String> = HashMap::new();
    let mut global_bounds = common::OptionalBounds::default();

    for row in &rows {
        let Some(row_type) = row.first().and_then(Value::as_str) else {
            continue;
        };
        match row_type.trim().to_ascii_uppercase().as_str() {
            "PART" => {
                has_part_rows = true;
                let bounds = common::part_bounds_from_row(row);
                if let Some(bounds) = bounds {
                    global_bounds.update_x(bounds.min_x_units, bounds.max_x_units);
                    global_bounds.update_y(bounds.min_y_units, bounds.max_y_units);
                }
                let owner_part_id = parts.len() as i32 + 1;
                parts.push(PartRaw::new(owner_part_id, bounds));
                current_part_index = Some(parts.len() - 1);
            }
            "ATTR" => {
                let parent = common::row_string(row, 2);
                let key = common::row_string(row, 3);
                if key.trim().is_empty() {
                    continue;
                }
                let key_upper = key.trim().to_ascii_uppercase();
                let value = common::row_string(row, 4);
                if parent.trim().is_empty() {
                    root_attrs.insert(key_upper, value);
                    continue;
                }
                let attrs = attr_by_parent.entry(parent.trim().to_string()).or_default();
                attrs.insert(key_upper.clone(), value);
                attrs.insert(
                    format!("{key_upper}__VISIBLE"),
                    common::row_bool(row, 6, true).to_string(),
                );
            }
            "PIN" => {
                let part_index = ensure_current_part_index(&mut parts, &mut current_part_index);
                let owner_part_id = parts[part_index].owner_part_id;
                let pin = PinRaw {
                    id: common::row_string(row, 1),
                    x_units: common::row_f64(row, 4, 0.0),
                    y_units: common::row_f64(row, 5, 0.0),
                    length_units: common::row_f64(row, 6, PIN_LENGTH_UNITS),
                    rotation_degrees: common::row_f64(row, 7, 0.0),
                    owner_part_id,
                };
                if pin.id.trim().is_empty() {
                    continue;
                }
                let angle = common::normalize_angle(pin.rotation_degrees);
                let (dx, dy) = if !(45.0..315.0).contains(&angle) {
                    (pin.length_units, 0.0)
                } else if angle < 135.0 {
                    (0.0, pin.length_units)
                } else if angle < 225.0 {
                    (-pin.length_units, 0.0)
                } else {
                    (0.0, -pin.length_units)
                };
                let min_x = pin.x_units.min(pin.x_units + dx);
                let max_x = pin.x_units.max(pin.x_units + dx);
                let min_y = pin.y_units.min(pin.y_units + dy);
                let max_y = pin.y_units.max(pin.y_units + dy);
                parts[part_index].bounds.update_x(min_x, max_x);
                parts[part_index].bounds.update_y(min_y, max_y);
                parts[part_index].pins.push(pin);
                global_bounds.update_x(min_x, max_x);
                global_bounds.update_y(min_y, max_y);
            }
            "RECT" => {
                let part_index = ensure_current_part_index(&mut parts, &mut current_part_index);
                let owner_part_id = parts[part_index].owner_part_id;
                let rect = RectRaw {
                    x1_units: common::row_f64(row, 2, 0.0),
                    y1_units: common::row_f64(row, 3, 0.0),
                    x2_units: common::row_f64(row, 4, 0.0),
                    y2_units: common::row_f64(row, 5, 0.0),
                    owner_part_id,
                };
                let min_x = rect.x1_units.min(rect.x2_units);
                let max_x = rect.x1_units.max(rect.x2_units);
                let min_y = rect.y1_units.min(rect.y2_units);
                let max_y = rect.y1_units.max(rect.y2_units);
                parts[part_index].bounds.update_x(min_x, max_x);
                parts[part_index].bounds.update_y(min_y, max_y);
                parts[part_index].rectangles.push(rect);
                global_bounds.update_x(min_x, max_x);
                global_bounds.update_y(min_y, max_y);
            }
            "POLY" | "POLYGON" | "PATH" => {
                let Some(shape) = row.get(2) else {
                    continue;
                };
                let points = parse_path_raw_points(shape);
                if points.len() < 2 {
                    continue;
                }
                let part_index = ensure_current_part_index(&mut parts, &mut current_part_index);
                let owner_part_id = parts[part_index].owner_part_id;
                for point in &points {
                    parts[part_index].bounds.update_x(point.x_units, point.x_units);
                    parts[part_index].bounds.update_y(point.y_units, point.y_units);
                    global_bounds.update_x(point.x_units, point.x_units);
                    global_bounds.update_y(point.y_units, point.y_units);
                }
                parts[part_index].polylines.push(PolylineRaw {
                    points,
                    owner_part_id,
                });
            }
            "LINE" => {
                let points = vec![
                    PointUnits {
                        x_units: common::row_f64(row, 2, 0.0),
                        y_units: common::row_f64(row, 3, 0.0),
                    },
                    PointUnits {
                        x_units: common::row_f64(row, 4, 0.0),
                        y_units: common::row_f64(row, 5, 0.0),
                    },
                ];
                let part_index = ensure_current_part_index(&mut parts, &mut current_part_index);
                let owner_part_id = parts[part_index].owner_part_id;
                for point in &points {
                    parts[part_index].bounds.update_x(point.x_units, point.x_units);
                    parts[part_index].bounds.update_y(point.y_units, point.y_units);
                    global_bounds.update_x(point.x_units, point.x_units);
                    global_bounds.update_y(point.y_units, point.y_units);
                }
                parts[part_index].polylines.push(PolylineRaw {
                    points,
                    owner_part_id,
                });
            }
            "ARC" => {
                let part_index = ensure_current_part_index(&mut parts, &mut current_part_index);
                let owner_part_id = parts[part_index].owner_part_id;
                let arc = ArcRaw {
                    start: PointUnits {
                        x_units: common::row_f64(row, 2, 0.0),
                        y_units: common::row_f64(row, 3, 0.0),
                    },
                    mid: PointUnits {
                        x_units: common::row_f64(row, 4, 0.0),
                        y_units: common::row_f64(row, 5, 0.0),
                    },
                    end: PointUnits {
                        x_units: common::row_f64(row, 6, 0.0),
                        y_units: common::row_f64(row, 7, 0.0),
                    },
                    owner_part_id,
                };
                for point in [arc.start, arc.mid, arc.end] {
                    parts[part_index].bounds.update_x(point.x_units, point.x_units);
                    parts[part_index].bounds.update_y(point.y_units, point.y_units);
                    global_bounds.update_x(point.x_units, point.x_units);
                    global_bounds.update_y(point.y_units, point.y_units);
                }
                parts[part_index].arcs.push(arc);
            }
            "CIRCLE" => {
                let r = common::row_f64(row, 4, 0.0).abs();
                if r <= 0.000001 {
                    continue;
                }
                let part_index = ensure_current_part_index(&mut parts, &mut current_part_index);
                let owner_part_id = parts[part_index].owner_part_id;
                let filled = r <= 1.0 + f64::EPSILON;
                let ellipse = EllipseRaw {
                    center_x_units: common::row_f64(row, 2, 0.0),
                    center_y_units: common::row_f64(row, 3, 0.0),
                    radius_x_units: r,
                    radius_y_units: r,
                    owner_part_id,
                    is_filled: filled,
                    is_transparent: !filled,
                };
                update_ellipse_bounds(&mut parts[part_index].bounds, &ellipse);
                update_ellipse_bounds(&mut global_bounds, &ellipse);
                parts[part_index].ellipses.push(ellipse);
            }
            "ELLIPSE" => {
                let rx = common::row_f64(row, 4, 0.0).abs();
                let ry = common::row_f64(row, 5, 0.0).abs();
                if rx <= 0.000001 || ry <= 0.000001 {
                    continue;
                }
                let part_index = ensure_current_part_index(&mut parts, &mut current_part_index);
                let owner_part_id = parts[part_index].owner_part_id;
                let filled = rx.max(ry) <= 1.0 + f64::EPSILON;
                let ellipse = EllipseRaw {
                    center_x_units: common::row_f64(row, 2, 0.0),
                    center_y_units: common::row_f64(row, 3, 0.0),
                    radius_x_units: rx,
                    radius_y_units: ry,
                    owner_part_id,
                    is_filled: filled,
                    is_transparent: !filled,
                };
                update_ellipse_bounds(&mut parts[part_index].bounds, &ellipse);
                update_ellipse_bounds(&mut global_bounds, &ellipse);
                parts[part_index].ellipses.push(ellipse);
            }
            "TEXT" => {
                let text = normalize_text_value(&common::row_string(row, 5));
                if text.is_empty() {
                    continue;
                }
                let part_index = ensure_current_part_index(&mut parts, &mut current_part_index);
                let owner_part_id = parts[part_index].owner_part_id;
                let label = TextRaw {
                    text,
                    x_units: common::row_f64(row, 2, 0.0),
                    y_units: common::row_f64(row, 3, 0.0),
                    rotation_degrees: common::row_f64(row, 4, 0.0),
                    owner_part_id,
                };
                parts[part_index].bounds.update_x(label.x_units, label.x_units);
                parts[part_index].bounds.update_y(label.y_units, label.y_units);
                global_bounds.update_x(label.x_units, label.x_units);
                global_bounds.update_y(label.y_units, label.y_units);
                parts[part_index].labels.push(label);
            }
            _ => {}
        }
    }

    if parts.is_empty() {
        parts.push(PartRaw::new(1, None));
    }

    let description = metadata
        .description
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| nested_string(payload, &["result", "description"]))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Generated from EasyEDA symbol".to_string());
    let designator_text = metadata
        .designator
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| root_attrs.get("DESIGNATOR").cloned())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "*".to_string());
    let comment_text = metadata
        .comment
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| root_attrs.get("NAME").cloned())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "*".to_string());
    let parameters: Vec<SchlibParameter> = metadata
        .parameters
        .iter()
        .filter(|parameter| !parameter.name.trim().is_empty() && !parameter.value.trim().is_empty())
        .cloned()
        .collect();

    let mut component = Component {
        name: normalize_component_name(component_name),
        description,
        designator_text,
        comment_text,
        parameters,
        implementations: Vec::new(),
        part_count: parts.len().max(1),
        pins: Vec::new(),
        rectangles: Vec::new(),
        polylines: Vec::new(),
        arcs: Vec::new(),
        ellipses: Vec::new(),
        labels: Vec::new(),
    };
    let has_original_graphics = parts.iter().any(PartRaw::has_graphics);

    if !has_original_graphics && !has_part_rows {
        let layout_pins: Vec<common::PinRaw> = parts[0]
            .pins
            .iter()
            .map(|pin| common::PinRaw {
                id: pin.id.clone(),
                x_units: pin.x_units,
                y_units: pin.y_units,
                length_units: pin.length_units,
                rotation_degrees: pin.rotation_degrees,
            })
            .collect();
        let (placed_pins, laid_out_rect) = common::layout_pins(&layout_pins, &attr_by_parent);
        if !placed_pins.is_empty() {
            component.rectangles.push(Rectangle {
                corner1: common::CoordPoint::from_symbol_units(
                    laid_out_rect.x1_units,
                    laid_out_rect.height_units() - laid_out_rect.y1_units,
                ),
                corner2: common::CoordPoint::from_symbol_units(
                    laid_out_rect.x2_units,
                    laid_out_rect.height_units() - laid_out_rect.y2_units,
                ),
                color_bgr: common::BORDER_BGR,
                fill_color_bgr: common::FILL_BGR,
                is_filled: true,
                is_transparent: false,
                line_width_index: BODY_LINE_WIDTH_INDEX,
                owner_part_id: 1,
            });
            for pin in placed_pins {
                component.pins.push(Pin {
                    designator: pin.designator,
                    name: pin.name,
                    location: common::CoordPoint::from_symbol_units(
                        pin.x_units,
                        laid_out_rect.height_units() - pin.y_units,
                    ),
                    length_raw: common::raw_from_symbol_units(pin.length_units),
                    orientation: pin_orientation_from_easyeda_rotation(pin.rotation_degrees),
                    show_name: pin.show_name,
                    show_designator: pin.show_designator,
                    color_bgr: common::SYMBOL_BGR,
                    owner_part_id: 1,
                    owner_part_display_mode: 0,
                });
            }
            add_metadata_implementation(&mut component, metadata);
            return Ok(component);
        }
    }

    let mut has_any_body = false;
    for part in &parts {
        let complex = part.has_complex_body();
        if !part.rectangles.is_empty() {
            for rect in &part.rectangles {
                component.rectangles.push(Rectangle {
                    corner1: common::CoordPoint::from_symbol_units(rect.x1_units, rect.y1_units),
                    corner2: common::CoordPoint::from_symbol_units(rect.x2_units, rect.y2_units),
                    color_bgr: common::BORDER_BGR,
                    fill_color_bgr: if complex { WHITE_BGR } else { common::FILL_BGR },
                    is_filled: !complex,
                    is_transparent: complex,
                    line_width_index: BODY_LINE_WIDTH_INDEX,
                    owner_part_id: rect.owner_part_id,
                });
                has_any_body = true;
            }
        } else if let Some(bounds) = part.bounds.finish() {
            if !part.pins.is_empty() && !part.has_graphics() {
                component.rectangles.push(Rectangle {
                    corner1: common::CoordPoint::from_symbol_units(
                        bounds.min_x_units,
                        bounds.max_y_units,
                    ),
                    corner2: common::CoordPoint::from_symbol_units(
                        bounds.max_x_units,
                        bounds.min_y_units,
                    ),
                    color_bgr: common::BORDER_BGR,
                    fill_color_bgr: if complex { WHITE_BGR } else { common::FILL_BGR },
                    is_filled: !complex,
                    is_transparent: complex,
                    line_width_index: BODY_LINE_WIDTH_INDEX,
                    owner_part_id: part.owner_part_id,
                });
                has_any_body = true;
            }
        }
        for polyline in &part.polylines {
            component.polylines.push(Polyline {
                points: polyline
                    .points
                    .iter()
                    .map(|point| {
                        common::CoordPoint::from_symbol_units(point.x_units, point.y_units)
                    })
                    .collect(),
                color_bgr: common::SYMBOL_BGR,
                line_width_index: GRAPHIC_LINE_WIDTH_INDEX,
                owner_part_id: polyline.owner_part_id,
            });
            has_any_body = true;
        }
        for arc in &part.arcs {
            if let Some(converted) = arc_from_raw(arc) {
                component.arcs.push(converted);
            } else {
                component.polylines.push(Polyline {
                    points: vec![
                        common::CoordPoint::from_symbol_units(arc.start.x_units, arc.start.y_units),
                        common::CoordPoint::from_symbol_units(arc.mid.x_units, arc.mid.y_units),
                        common::CoordPoint::from_symbol_units(arc.end.x_units, arc.end.y_units),
                    ],
                    color_bgr: common::SYMBOL_BGR,
                    line_width_index: GRAPHIC_LINE_WIDTH_INDEX,
                    owner_part_id: arc.owner_part_id,
                });
            }
            has_any_body = true;
        }
        for ellipse in &part.ellipses {
            component.ellipses.push(Ellipse {
                center: common::CoordPoint::from_symbol_units(
                    ellipse.center_x_units,
                    ellipse.center_y_units,
                ),
                radius_x_raw: common::raw_from_symbol_units(ellipse.radius_x_units),
                radius_y_raw: common::raw_from_symbol_units(ellipse.radius_y_units),
                color_bgr: common::SYMBOL_BGR,
                fill_color_bgr: if ellipse.is_filled { common::SYMBOL_BGR } else { WHITE_BGR },
                is_filled: ellipse.is_filled,
                is_transparent: ellipse.is_transparent,
                line_width_index: GRAPHIC_LINE_WIDTH_INDEX,
                owner_part_id: ellipse.owner_part_id,
            });
            has_any_body = true;
        }
        for label in &part.labels {
            component.labels.push(Label {
                text: label.text.clone(),
                location: common::CoordPoint::from_symbol_units(label.x_units, label.y_units),
                orientation: text_orientation_from_rotation(label.rotation_degrees),
                color_bgr: common::SYMBOL_BGR,
                owner_part_id: label.owner_part_id,
            });
            has_any_body = true;
        }
        for (pin_index, pin) in part.pins.iter().enumerate() {
            let attrs = attr_by_parent.get(&pin.id);
            let designator = common::safe_attr(attrs, "NUMBER")
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| (pin_index + 1).to_string());
            let name = common::safe_attr(attrs, "NAME")
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| designator.clone());
            let source_length_units = if pin.length_units > 0.000001 {
                pin.length_units
            } else {
                10.0
            };
            let export_length_units = PIN_LENGTH_UNITS;
            let show_name = common::safe_attr_flag(attrs, "NAME", !name.trim().is_empty());
            let show_designator = common::safe_attr_flag(attrs, "NUMBER", true);
            let (location_x_units, location_y_units) = pin_inner_location_from_easyeda(
                pin.x_units,
                pin.y_units,
                source_length_units,
                pin.rotation_degrees,
            );
            let location =
                common::CoordPoint::from_symbol_units(location_x_units, location_y_units);
            let orientation = common::pin_orientation_from_rotation(pin.rotation_degrees);
            component.pins.push(Pin {
                designator,
                name: name.clone(),
                location,
                length_raw: common::raw_from_symbol_units(export_length_units),
                orientation,
                show_name,
                show_designator,
                color_bgr: common::SYMBOL_BGR,
                owner_part_id: pin.owner_part_id,
                owner_part_display_mode: 0,
            });
        }
    }

    if !has_any_body {
        if let Some(bounds) = global_bounds.finish() {
            component.rectangles.push(Rectangle {
                corner1: common::CoordPoint::from_symbol_units(
                    bounds.min_x_units,
                    bounds.max_y_units,
                ),
                corner2: common::CoordPoint::from_symbol_units(
                    bounds.max_x_units,
                    bounds.min_y_units,
                ),
                color_bgr: common::BORDER_BGR,
                fill_color_bgr: WHITE_BGR,
                is_filled: false,
                is_transparent: true,
                line_width_index: BODY_LINE_WIDTH_INDEX,
                owner_part_id: 1,
            });
        }
    }

    add_metadata_implementation(&mut component, metadata);
    Ok(component)
}

fn add_metadata_implementation(component: &mut Component, metadata: &SchlibMetadata) {
    use std::collections::HashSet;
    let Some(model_name) = metadata
        .footprint_model_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };

    let Some(data_file_entity) = metadata
        .footprint_library_file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };

    let mut seen_designators = HashSet::new();
    let mut map_definers = Vec::new();
    for pin in &component.pins {
        let designator = pin.designator.trim();
        if designator.is_empty() || !seen_designators.insert(designator.to_ascii_lowercase()) {
            continue;
        }
        map_definers.push(MapDefiner {
            designator_interface: designator.to_string(),
            designator_implementations: vec![designator.to_string()],
            is_trivial: true,
        });
    }

    component.implementations.push(Implementation {
        description: Some("PCB footprint".to_string()),
        model_name: model_name.to_string(),
        model_type: "PCBLIB".to_string(),
        is_current: true,
        data_file_kinds: vec!["PCBLib".to_string()],
        data_file_entities: vec![data_file_entity.to_string()],
        map_definers,
    });
}

// ── Shape / path parsing helpers ──────────────────────────────────────────────

fn update_ellipse_bounds(bounds: &mut common::OptionalBounds, ellipse: &EllipseRaw) {
    bounds.update_x(
        ellipse.center_x_units - ellipse.radius_x_units,
        ellipse.center_x_units + ellipse.radius_x_units,
    );
    bounds.update_y(
        ellipse.center_y_units - ellipse.radius_y_units,
        ellipse.center_y_units + ellipse.radius_y_units,
    );
}

fn normalize_text_value(text: &str) -> String {
    text.replace("\\n", "\n")
}

fn ensure_current_part_index(parts: &mut Vec<PartRaw>, current: &mut Option<usize>) -> usize {
    if current.is_none() {
        parts.push(PartRaw::new(1, None));
        *current = Some(0);
    }
    current.expect("part index")
}

fn parse_path_raw_points(shape: &Value) -> Vec<PointUnits> {
    match shape {
        Value::Array(values) => parse_path_array_points(values),
        Value::String(text) => parse_svg_path_points(text),
        _ => Vec::new(),
    }
}

fn parse_path_array_points(values: &[Value]) -> Vec<PointUnits> {
    let mut points = Vec::new();
    let mut index = 0usize;
    let mut start = None;
    let mut current = None;
    while index < values.len() {
        match &values[index] {
            Value::String(command) => {
                let cmd = command.trim().to_ascii_uppercase();
                index += 1;
                match cmd.as_str() {
                    "Z" | "CLOSE" => {
                        if let (Some(first), Some(last)) = (start, current) {
                            if !same_point(first, last) {
                                add_path_point(&mut points, first.x_units, first.y_units);
                                current = Some(first);
                            }
                        }
                    }
                    "M" | "L" => {
                        while index + 1 < values.len() {
                            let Some(x) = values.get(index).and_then(common::value_as_f64) else {
                                break;
                            };
                            let Some(y) = values.get(index + 1).and_then(common::value_as_f64)
                            else {
                                break;
                            };
                            add_path_point(&mut points, x, y);
                            current = Some(PointUnits { x_units: x, y_units: y });
                            if start.is_none() {
                                start = current;
                            }
                            index += 2;
                        }
                    }
                    "H" => {
                        while index < values.len() {
                            let Some(x) = values.get(index).and_then(common::value_as_f64) else {
                                break;
                            };
                            let y = current.map_or(0.0, |point| point.y_units);
                            add_path_point(&mut points, x, y);
                            current = Some(PointUnits { x_units: x, y_units: y });
                            if start.is_none() {
                                start = current;
                            }
                            index += 1;
                        }
                    }
                    "V" => {
                        while index < values.len() {
                            let Some(y) = values.get(index).and_then(common::value_as_f64) else {
                                break;
                            };
                            let x = current.map_or(0.0, |point| point.x_units);
                            add_path_point(&mut points, x, y);
                            current = Some(PointUnits { x_units: x, y_units: y });
                            if start.is_none() {
                                start = current;
                            }
                            index += 1;
                        }
                    }
                    "ARC" | "A" => {
                        if index + 2 < values.len() {
                            if let (Some(x), Some(y)) = (
                                values.get(index + 1).and_then(common::value_as_f64),
                                values.get(index + 2).and_then(common::value_as_f64),
                            ) {
                                add_path_point(&mut points, x, y);
                                current = Some(PointUnits { x_units: x, y_units: y });
                                if start.is_none() {
                                    start = current;
                                }
                                index += 3;
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {
                if index + 1 < values.len() {
                    if let (Some(x), Some(y)) = (
                        values.get(index).and_then(common::value_as_f64),
                        values.get(index + 1).and_then(common::value_as_f64),
                    ) {
                        add_path_point(&mut points, x, y);
                        current = Some(PointUnits { x_units: x, y_units: y });
                        if start.is_none() {
                            start = current;
                        }
                        index += 2;
                        continue;
                    }
                }
                index += 1;
            }
        }
    }
    points
}

fn parse_svg_path_points(text: &str) -> Vec<PointUnits> {
    let tokens = tokenize_svg_path(text);
    let mut points = Vec::new();
    let mut index = 0usize;
    let mut command = 'M';
    let mut current = PointUnits { x_units: 0.0, y_units: 0.0 };
    let mut start = None;
    while index < tokens.len() {
        if let Some(letter) = tokens[index]
            .chars()
            .next()
            .filter(|ch| tokens[index].len() == 1 && ch.is_ascii_alphabetic())
        {
            command = letter;
            index += 1;
            if matches!(command, 'Z' | 'z') {
                if let Some(first) = start {
                    if !same_point(first, current) {
                        add_path_point(&mut points, first.x_units, first.y_units);
                        current = first;
                    }
                }
                continue;
            }
        }
        match command {
            'M' | 'm' => {
                if index + 1 >= tokens.len() {
                    break;
                }
                let Some(mut x) = tokens[index].parse::<f64>().ok() else {
                    break;
                };
                let Some(mut y) = tokens[index + 1].parse::<f64>().ok() else {
                    break;
                };
                if command == 'm' {
                    x += current.x_units;
                    y += current.y_units;
                }
                add_path_point(&mut points, x, y);
                current = PointUnits { x_units: x, y_units: y };
                if start.is_none() {
                    start = Some(current);
                }
                command = if command == 'm' { 'l' } else { 'L' };
                index += 2;
            }
            'L' | 'l' => {
                if index + 1 >= tokens.len() {
                    break;
                }
                let Some(mut x) = tokens[index].parse::<f64>().ok() else {
                    break;
                };
                let Some(mut y) = tokens[index + 1].parse::<f64>().ok() else {
                    break;
                };
                if command == 'l' {
                    x += current.x_units;
                    y += current.y_units;
                }
                add_path_point(&mut points, x, y);
                current = PointUnits { x_units: x, y_units: y };
                if start.is_none() {
                    start = Some(current);
                }
                index += 2;
            }
            'H' | 'h' => {
                let Some(mut x) = tokens
                    .get(index)
                    .and_then(|token| token.parse::<f64>().ok())
                else {
                    break;
                };
                if command == 'h' {
                    x += current.x_units;
                }
                add_path_point(&mut points, x, current.y_units);
                current = PointUnits { x_units: x, y_units: current.y_units };
                if start.is_none() {
                    start = Some(current);
                }
                index += 1;
            }
            'V' | 'v' => {
                let Some(mut y) = tokens
                    .get(index)
                    .and_then(|token| token.parse::<f64>().ok())
                else {
                    break;
                };
                if command == 'v' {
                    y += current.y_units;
                }
                add_path_point(&mut points, current.x_units, y);
                current = PointUnits { x_units: current.x_units, y_units: y };
                if start.is_none() {
                    start = Some(current);
                }
                index += 1;
            }
            'A' | 'a' => {
                if index + 6 >= tokens.len() {
                    break;
                }
                let Some(mut x) = tokens[index + 5].parse::<f64>().ok() else {
                    break;
                };
                let Some(mut y) = tokens[index + 6].parse::<f64>().ok() else {
                    break;
                };
                if command == 'a' {
                    x += current.x_units;
                    y += current.y_units;
                }
                add_path_point(&mut points, x, y);
                current = PointUnits { x_units: x, y_units: y };
                if start.is_none() {
                    start = Some(current);
                }
                index += 7;
            }
            'C' | 'c' => {
                if index + 5 >= tokens.len() {
                    break;
                }
                let Some(mut x) = tokens[index + 4].parse::<f64>().ok() else {
                    break;
                };
                let Some(mut y) = tokens[index + 5].parse::<f64>().ok() else {
                    break;
                };
                if command == 'c' {
                    x += current.x_units;
                    y += current.y_units;
                }
                add_path_point(&mut points, x, y);
                current = PointUnits { x_units: x, y_units: y };
                if start.is_none() {
                    start = Some(current);
                }
                index += 6;
            }
            'Q' | 'q' | 'S' | 's' => {
                if index + 3 >= tokens.len() {
                    break;
                }
                let Some(mut x) = tokens[index + 2].parse::<f64>().ok() else {
                    break;
                };
                let Some(mut y) = tokens[index + 3].parse::<f64>().ok() else {
                    break;
                };
                if command.is_ascii_lowercase() {
                    x += current.x_units;
                    y += current.y_units;
                }
                add_path_point(&mut points, x, y);
                current = PointUnits { x_units: x, y_units: y };
                if start.is_none() {
                    start = Some(current);
                }
                index += 4;
            }
            _ => index += 1,
        }
    }
    points
}

fn tokenize_svg_path(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_ascii_alphabetic() {
            if !current.trim().is_empty() {
                tokens.push(current.trim().to_string());
                current.clear();
            }
            tokens.push(ch.to_string());
            continue;
        }
        if ch == ',' || ch.is_whitespace() {
            if !current.trim().is_empty() {
                tokens.push(current.trim().to_string());
                current.clear();
            }
            continue;
        }
        if (ch == '-' || ch == '+') && !current.is_empty() && current != "e" && current != "E" {
            if !current.trim().is_empty() {
                tokens.push(current.trim().to_string());
                current.clear();
            }
        }
        current.push(ch);
        if matches!(chars.peek(), Some(next) if next.is_ascii_alphabetic() || *next == ',' || next.is_whitespace())
        {
            if !current.trim().is_empty() {
                tokens.push(current.trim().to_string());
                current.clear();
            }
        }
    }
    if !current.trim().is_empty() {
        tokens.push(current.trim().to_string());
    }
    tokens
}

fn add_path_point(points: &mut Vec<PointUnits>, x_units: f64, y_units: f64) {
    let point = PointUnits { x_units, y_units };
    if points
        .last()
        .copied()
        .is_some_and(|last| same_point(last, point))
    {
        return;
    }
    points.push(point);
}

fn same_point(left: PointUnits, right: PointUnits) -> bool {
    (left.x_units - right.x_units).abs() < 1e-9 && (left.y_units - right.y_units).abs() < 1e-9
}

// ── Orientation / angle helpers ───────────────────────────────────────────────

fn text_orientation_from_rotation(rotation: f64) -> u8 {
    ((common::normalize_angle(rotation) / 90.0).round() as i32).rem_euclid(4) as u8
}

fn pin_orientation_from_easyeda_rotation(rotation: f64) -> u8 {
    ((common::normalize_angle(rotation) / 90.0).round() as i32).rem_euclid(4) as u8
}

fn pin_inner_location_from_easyeda(
    x_units: f64,
    y_units: f64,
    length_units: f64,
    rotation: f64,
) -> (f64, f64) {
    let angle = common::normalize_angle(rotation);
    let (dx_units, dy_units) = if !(45.0..315.0).contains(&angle) {
        (length_units, 0.0)
    } else if angle < 135.0 {
        (0.0, length_units)
    } else if angle < 225.0 {
        (-length_units, 0.0)
    } else {
        (0.0, -length_units)
    };
    (x_units + dx_units, y_units + dy_units)
}

fn arc_from_raw(raw: &ArcRaw) -> Option<Arc> {
    let (x1, y1, x2, y2, x3, y3) = (
        raw.start.x_units,
        raw.start.y_units,
        raw.mid.x_units,
        raw.mid.y_units,
        raw.end.x_units,
        raw.end.y_units,
    );
    let divisor = 2.0 * (x1 * (y2 - y3) + x2 * (y3 - y1) + x3 * (y1 - y2));
    if divisor.abs() <= 1e-9 {
        return None;
    }
    let x1_sq = x1 * x1 + y1 * y1;
    let x2_sq = x2 * x2 + y2 * y2;
    let x3_sq = x3 * x3 + y3 * y3;
    let cx = (x1_sq * (y2 - y3) + x2_sq * (y3 - y1) + x3_sq * (y1 - y2)) / divisor;
    let cy = (x1_sq * (x3 - x2) + x2_sq * (x1 - x3) + x3_sq * (x2 - x1)) / divisor;
    let radius = ((x1 - cx).powi(2) + (y1 - cy).powi(2)).sqrt();
    if !radius.is_finite() || radius <= 1e-9 {
        return None;
    }
    let mut start_angle = point_angle_degrees(cx, cy, x1, y1);
    let mid_angle = point_angle_degrees(cx, cy, x2, y2);
    let mut end_angle = point_angle_degrees(cx, cy, x3, y3);
    if !angle_lies_on_ccw_path(start_angle, mid_angle, end_angle) {
        std::mem::swap(&mut start_angle, &mut end_angle);
    }
    Some(Arc {
        center: common::CoordPoint::from_symbol_units(cx, cy),
        radius_raw: common::raw_from_symbol_units(radius),
        start_angle,
        end_angle,
        color_bgr: common::SYMBOL_BGR,
        line_width_index: GRAPHIC_LINE_WIDTH_INDEX,
        owner_part_id: raw.owner_part_id,
    })
}

fn point_angle_degrees(cx: f64, cy: f64, x: f64, y: f64) -> f64 {
    common::normalize_angle((y - cy).atan2(x - cx).to_degrees())
}

fn angle_lies_on_ccw_path(start: f64, mid: f64, end: f64) -> bool {
    angle_delta_ccw(start, mid) <= angle_delta_ccw(start, end) + 1e-6
}

fn angle_delta_ccw(start: f64, end: f64) -> f64 {
    let mut delta = common::normalize_angle(end) - common::normalize_angle(start);
    if delta < 0.0 {
        delta += 360.0;
    }
    delta
}

fn normalize_component_name(name: &str) -> String {
    common::normalize_component_name(name)
}

// ── Raw intermediate struct types ─────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct PointUnits {
    x_units: f64,
    y_units: f64,
}

#[derive(Debug, Clone)]
struct PartRaw {
    owner_part_id: i32,
    bounds: common::OptionalBounds,
    pins: Vec<PinRaw>,
    rectangles: Vec<RectRaw>,
    polylines: Vec<PolylineRaw>,
    arcs: Vec<ArcRaw>,
    ellipses: Vec<EllipseRaw>,
    labels: Vec<TextRaw>,
}

impl PartRaw {
    fn new(owner_part_id: i32, declared_bounds: Option<common::Bounds>) -> Self {
        let mut bounds = common::OptionalBounds::default();
        if let Some(bounds_decl) = declared_bounds {
            bounds.update_x(bounds_decl.min_x_units, bounds_decl.max_x_units);
            bounds.update_y(bounds_decl.min_y_units, bounds_decl.max_y_units);
        }
        Self {
            owner_part_id,
            bounds,
            pins: Vec::new(),
            rectangles: Vec::new(),
            polylines: Vec::new(),
            arcs: Vec::new(),
            ellipses: Vec::new(),
            labels: Vec::new(),
        }
    }

    fn has_graphics(&self) -> bool {
        !self.rectangles.is_empty()
            || !self.polylines.is_empty()
            || !self.arcs.is_empty()
            || !self.ellipses.is_empty()
            || !self.labels.is_empty()
    }

    fn has_complex_body(&self) -> bool {
        !self.polylines.is_empty() || !self.arcs.is_empty()
    }
}

#[derive(Debug, Clone)]
struct PinRaw {
    id: String,
    x_units: f64,
    y_units: f64,
    length_units: f64,
    rotation_degrees: f64,
    owner_part_id: i32,
}

#[derive(Debug, Clone, Copy)]
struct RectRaw {
    x1_units: f64,
    y1_units: f64,
    x2_units: f64,
    y2_units: f64,
    owner_part_id: i32,
}

#[derive(Debug, Clone)]
struct PolylineRaw {
    points: Vec<PointUnits>,
    owner_part_id: i32,
}

#[derive(Debug, Clone, Copy)]
struct ArcRaw {
    start: PointUnits,
    mid: PointUnits,
    end: PointUnits,
    owner_part_id: i32,
}

#[derive(Debug, Clone, Copy)]
struct EllipseRaw {
    center_x_units: f64,
    center_y_units: f64,
    radius_x_units: f64,
    radius_y_units: f64,
    owner_part_id: i32,
    is_filled: bool,
    is_transparent: bool,
}

#[derive(Debug, Clone)]
struct TextRaw {
    text: String,
    x_units: f64,
    y_units: f64,
    rotation_degrees: f64,
    owner_part_id: i32,
}
