#[allow(dead_code)]
mod common;
pub mod params;
mod builder;

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use serde_json::Value;

use crate::error::Result;
use crate::schlib::params::is_default_visible_parameter;


#[derive(Debug, Clone, Default)]
pub struct SchlibMetadata {
    pub description: Option<String>,
    pub designator: Option<String>,
    pub comment: Option<String>,
    pub parameters: Vec<SchlibParameter>,
    pub footprint_model_name: Option<String>,
    pub footprint_library_file: Option<String>,
    pub name_override: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SchlibParameter {
    pub name: String,
    pub value: String,
}

pub fn write_schlib_from_payload(
    payload: &Value,
    component_name: &str,
    output_path: &Path,
) -> Result<()> {
    write_schlib_from_payload_with_metadata(
        payload,
        component_name,
        &SchlibMetadata::default(),
        output_path,
    )
}

pub fn write_schlib_from_payload_with_metadata(
    payload: &Value,
    component_name: &str,
    metadata: &SchlibMetadata,
    output_path: &Path,
) -> Result<()> {
    let component = build_component_from_payload_with_metadata(payload, component_name, metadata)?;
    write_schlib_library(std::slice::from_ref(&component), output_path)
}

pub fn build_component_from_payload(payload: &Value, component_name: &str) -> Result<Component> {
    build_component_from_payload_with_metadata(payload, component_name, &SchlibMetadata::default())
}

pub fn build_component_from_payload_with_metadata(
    payload: &Value,
    component_name: &str,
    metadata: &SchlibMetadata,
) -> Result<Component> {
    builder::build_component(payload, component_name, metadata)
}

// ── Binary format writer ──────────────────────────────────────────────────────

pub fn write_schlib(component: &Component, output_path: &Path) -> Result<()> {
    write_schlib_library(std::slice::from_ref(component), output_path)
}

pub fn write_schlib_library(components: &[Component], output_path: &Path) -> Result<()> {
    if components.is_empty() {
        return Err(crate::error::AppError::Other(
            "cannot write empty SchLib library".to_string(),
        ));
    }

    let sections = collect_sections(components);
    let file = File::create(output_path)?;
    let mut compound = cfb::CompoundFile::create(file)?;

    write_stream(&mut compound, "/FileHeader", &file_header_bytes(components))?;
    let section_keys = collect_section_key_pairs(&sections);
    if !section_keys.is_empty() {
        write_stream(
            &mut compound,
            "/SectionKeys",
            &section_keys_bytes(&section_keys),
        )?;
    }
    for (component, section_key) in &sections {
        compound.create_storage(&format!("/{section_key}/"))?;
        write_stream(
            &mut compound,
            &format!("/{section_key}/Data"),
            &component_data_bytes(component),
        )?;
    }
    write_stream(&mut compound, "/Storage", &storage_bytes())?;
    compound.flush()?;
    Ok(())
}

fn collect_sections<'a>(components: &'a [Component]) -> Vec<(&'a Component, String)> {
    let mut used = HashSet::new();
    components
        .iter()
        .map(|component| {
            let section_key = unique_section_key(&component.name, &mut used);
            (component, section_key)
        })
        .collect()
}

fn collect_section_key_pairs(sections: &[(&Component, String)]) -> Vec<(String, String)> {
    sections
        .iter()
        .filter_map(|(component, section_key)| {
            (section_key.as_str() != component.name.as_str())
                .then(|| (component.name.clone(), section_key.clone()))
        })
        .collect()
}

fn unique_section_key(name: &str, used: &mut HashSet<String>) -> String {
    let base = common::section_key_from_name(name);
    if used.insert(base.clone()) {
        return base;
    }

    let mut index = 2usize;
    loop {
        let suffix = format!("_{index}");
        let max_len = 31usize.saturating_sub(suffix.len());
        let prefix: String = base.chars().take(max_len.max(1)).collect();
        let candidate = format!("{prefix}{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        index += 1;
    }
}

fn write_stream(
    compound: &mut cfb::CompoundFile<File>,
    stream_path: &str,
    data: &[u8],
) -> std::io::Result<()> {
    let mut stream = compound.create_stream(stream_path)?;
    stream.write_all(data)
}

fn file_header_bytes(components: &[Component]) -> Vec<u8> {
    let mut writer = common::BinaryWriter::default();
    let mut params = common::Params::default();
    params.push(
        "HEADER",
        "Protel for Windows - Schematic Library Editor Binary File Version 5.0",
    );
    params.push("WEIGHT", schlib_weight(components).to_string());
    params.push("MINORVERSION", "2");
    params.push("FONTIDCOUNT", "1");
    params.push("SIZE1", "10");
    params.push("FONTNAME1", "Times New Roman");
    params.push("USEMBCS", "T");
    params.push("ISBOC", "T");
    params.push("SHEETSTYLE", "9");
    params.push("SYSTEMFONT", "1");
    params.push("BORDERON", "T");
    params.push("SHEETNUMBERSPACESIZE", "12");
    params.push("AREACOLOR", "16317695");
    params.push("SNAPGRIDON", "T");
    params.push("SNAPGRIDSIZE", "10");
    params.push("VISIBLEGRIDON", "T");
    params.push("VISIBLEGRIDSIZE", "10");
    params.push("CUSTOMX", "18000");
    params.push("CUSTOMY", "18000");
    params.push("USECUSTOMSHEET", "T");
    params.push("REFERENCEZONESON", "T");
    params.push("DISPLAY_UNIT", "0");
    params.push("COMPCOUNT", components.len().to_string());
    for (index, component) in components.iter().enumerate() {
        params.push(format!("LIBREF{index}"), &component.name);
        params.push(format!("COMPDESCR{index}"), &component.description);
        params.push(
            format!("PARTCOUNT{index}"),
            (component.part_count + 1).to_string(),
        );
    }
    writer.write_cstring_param_block(&params);
    writer.write_i32(components.len() as i32);
    for component in components {
        writer.write_string_block(&component.name);
    }
    writer.into_inner()
}

fn section_keys_bytes(section_keys: &[(String, String)]) -> Vec<u8> {
    let mut writer = common::BinaryWriter::default();
    let mut params = common::Params::default();
    params.push("KeyCount", section_keys.len().to_string());
    for (index, (component_name, section_key)) in section_keys.iter().enumerate() {
        params.push(format!("LibRef{index}"), component_name);
        params.push(format!("SectionKey{index}"), section_key);
    }
    writer.write_cstring_param_block(&params);
    writer.into_inner()
}

fn storage_bytes() -> Vec<u8> {
    let mut writer = common::BinaryWriter::default();
    let mut params = common::Params::default();
    params.push("HEADER", "Icon storage");
    writer.write_cstring_param_block(&params);
    writer.into_inner()
}

fn schlib_weight(components: &[Component]) -> usize {
    components.iter().map(component_weight).sum()
}

fn component_weight(component: &Component) -> usize {
    1 + component.rectangles.len()
        + component.labels.len()
        + component.polylines.len()
        + component.arcs.len()
        + component.ellipses.len()
        + component.pins.len()
        + component.parameters.len()
        + 2
        + implementation_record_count(component)
}

fn component_data_bytes(component: &Component) -> Vec<u8> {
    let mut writer = common::BinaryWriter::default();
    let mut params = common::Params::default();
    params.push("RECORD", "1");
    params.push("LIBREFERENCE", &component.name);
    params.push("COMPONENTDESCRIPTION", &component.description);
    params.push("PARTCOUNT", (component.part_count + 1).to_string());
    params.push("DISPLAYMODECOUNT", "1");
    params.push("OWNERPARTID", "-1");
    params.push("CURRENTPARTID", "1");
    params.push("LIBRARYPATH", "*");
    params.push("SOURCELIBRARYNAME", "*");
    params.push("SHEETPARTFILENAME", "*");
    params.push("TARGETFILENAME", "*");
    params.push("UNIQUEID", stable_unique_id(&component.name, "COMP"));
    params.push("AREACOLOR", "11599871");
    params.push("COLOR", "128");
    params.push("PARTIDLOCKED", "T");
    params.push("DESIGNITEMID", &component.name);
    if !component.pins.is_empty() {
        params.push("ALLPINCOUNT", component.pins.len().to_string());
    }
    writer.write_cstring_param_block(&params);
    for (index, rect) in component.rectangles.iter().enumerate() {
        let mut p = common::Params::default();
        p.push("RECORD", "14");
        push_owned_part(&mut p, rect.owner_part_id);
        p.push_coord("LOCATION.X", rect.corner1.x_raw);
        p.push_coord("LOCATION.Y", rect.corner1.y_raw);
        p.push_coord("CORNER.X", rect.corner2.x_raw);
        p.push_coord("CORNER.Y", rect.corner2.y_raw);
        p.push("LINEWIDTH", rect.line_width_index.to_string());
        p.push_non_zero("COLOR", rect.color_bgr);
        p.push("AREACOLOR", rect.fill_color_bgr.to_string());
        p.push_bool("ISSOLID", rect.is_filled);
        p.push_bool("TRANSPARENT", rect.is_transparent);
        p.push(
            "UNIQUEID",
            stable_unique_id(
                &component.name,
                &format!("RECT{}_{index}", rect.owner_part_id),
            ),
        );
        writer.write_cstring_param_block(&p);
    }
    for (index, label) in component.labels.iter().enumerate() {
        let mut p = common::Params::default();
        p.push("RECORD", "4");
        push_owned_part(&mut p, label.owner_part_id);
        p.push_coord("LOCATION.X", label.location.x_raw);
        p.push_coord("LOCATION.Y", label.location.y_raw);
        p.push("FONTID", "1");
        p.push("TEXT", &label.text);
        p.push_non_zero("COLOR", label.color_bgr);
        p.push_non_zero("ORIENTATION", label.orientation as i32);
        p.push(
            "UNIQUEID",
            stable_unique_id(
                &component.name,
                &format!("TEXT{}_{index}", label.owner_part_id),
            ),
        );
        writer.write_cstring_param_block(&p);
    }
    for (index, polyline) in component.polylines.iter().enumerate() {
        let mut p = common::Params::default();
        p.push("RECORD", "6");
        push_owned_part(&mut p, polyline.owner_part_id);
        p.push("LINEWIDTH", polyline.line_width_index.to_string());
        p.push_non_zero("COLOR", polyline.color_bgr);
        p.push("LOCATIONCOUNT", polyline.points.len().to_string());
        for (point_index, point) in polyline.points.iter().enumerate() {
            p.push_coord(&format!("X{}", point_index + 1), point.x_raw);
            p.push_coord(&format!("Y{}", point_index + 1), point.y_raw);
        }
        p.push(
            "UNIQUEID",
            stable_unique_id(
                &component.name,
                &format!("POLY{}_{index}", polyline.owner_part_id),
            ),
        );
        writer.write_cstring_param_block(&p);
    }
    for (index, arc) in component.arcs.iter().enumerate() {
        let mut p = common::Params::default();
        p.push("RECORD", "12");
        push_owned_part(&mut p, arc.owner_part_id);
        p.push_coord("LOCATION.X", arc.center.x_raw);
        p.push_coord("LOCATION.Y", arc.center.y_raw);
        p.push_coord("RADIUS", arc.radius_raw);
        p.push("LINEWIDTH", arc.line_width_index.to_string());
        if arc.start_angle.abs() > f64::EPSILON {
            p.push("STARTANGLE", format_angle(arc.start_angle));
        }
        p.push("ENDANGLE", format_angle(arc.end_angle));
        p.push_non_zero("COLOR", arc.color_bgr);
        p.push(
            "UNIQUEID",
            stable_unique_id(
                &component.name,
                &format!("ARC{}_{index}", arc.owner_part_id),
            ),
        );
        writer.write_cstring_param_block(&p);
    }
    for (index, ellipse) in component.ellipses.iter().enumerate() {
        let mut p = common::Params::default();
        p.push("RECORD", "8");
        push_owned_part(&mut p, ellipse.owner_part_id);
        p.push_coord("LOCATION.X", ellipse.center.x_raw);
        p.push_coord("LOCATION.Y", ellipse.center.y_raw);
        p.push_coord("RADIUS", ellipse.radius_x_raw);
        p.push_coord("SECONDARYRADIUS", ellipse.radius_y_raw);
        p.push("LINEWIDTH", ellipse.line_width_index.to_string());
        p.push_non_zero("COLOR", ellipse.color_bgr);
        p.push("AREACOLOR", ellipse.fill_color_bgr.to_string());
        p.push_bool("ISSOLID", ellipse.is_filled);
        p.push_bool("TRANSPARENT", ellipse.is_transparent);
        p.push(
            "UNIQUEID",
            stable_unique_id(
                &component.name,
                &format!("ELLIPSE{}_{index}", ellipse.owner_part_id),
            ),
        );
        writer.write_cstring_param_block(&p);
    }
    for pin in &component.pins {
        writer.write_block(0x01, |w| {
            w.write_i32(2);
            w.write_u8(0);
            w.write_i16(pin.owner_part_id.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
            w.write_u8(pin.owner_part_display_mode as u8);
            w.write_u8(0);
            w.write_u8(0);
            w.write_u8(0);
            w.write_u8(0);
            w.write_pascal_short_string("");
            w.write_u8(0);
            w.write_u8(4);
            w.write_u8(pin_conglomerate(pin));
            w.write_i16(common::dxp_i16(pin.length_raw));
            w.write_i16(common::dxp_i16(pin.location.x_raw));
            w.write_i16(common::dxp_i16(pin.location.y_raw));
            w.write_i32(pin.color_bgr);
            w.write_pascal_short_string(&pin.name);
            w.write_pascal_short_string(&pin.designator);
            w.write_pascal_short_string("");
            w.write_pascal_short_string("");
            w.write_pascal_short_string("");
        });
    }
    let mut visible_index: i32 = 0;
    for (index, parameter) in component.parameters.iter().enumerate() {
        let visible = is_default_visible_parameter(&component.designator_text, &parameter.name);
        let mut p = common::Params::default();
        p.push("RECORD", "41");
        p.push("OWNERPARTID", "-1");
        p.push("LOCATION.X_FRAC", "-5");
        if visible {
            let y = -20 - visible_index * 10;
            visible_index += 1;
            p.push("LOCATION.Y", y.to_string());
        } else {
            p.push("LOCATION.Y_FRAC", "-15");
        }
        p.push("COLOR", "8388608");
        p.push("FONTID", "1");
        if !visible {
            p.push("ISHIDDEN", "T");
        }
        let value_is_url = parameter.value.starts_with("http://")
            || parameter.value.starts_with("https://");
        if value_is_url {
            p.push("ISHYPERLINK", "T");
        }
        p.push("TEXT", &parameter.value);
        p.push("NAME", &parameter.name);
        p.push(
            "UNIQUEID",
            stable_unique_id(&component.name, &format!("PARAM{index}_{}", parameter.name)),
        );
        writer.write_cstring_param_block(&p);
    }

    let mut d = common::Params::default();
    d.push("RECORD", "34");
    d.push("OWNERPARTID", "-1");
    d.push("LOCATION.X_FRAC", "-5");
    d.push("LOCATION.Y_FRAC", "5");
    d.push("COLOR", "8388608");
    d.push("FONTID", "1");
    d.push("TEXT", &component.designator_text);
    d.push("NAME", "Designator");
    d.push("READONLYSTATE", "1");
    d.push("UNIQUEID", stable_unique_id(&component.name, "DESIGNATOR"));
    writer.write_cstring_param_block(&d);
    let mut c = common::Params::default();
    c.push("RECORD", "41");
    c.push("OWNERPARTID", "-1");
    c.push("LOCATION.X_FRAC", "-5");
    c.push("LOCATION.Y_FRAC", "-15");
    c.push("COLOR", "8388608");
    c.push("FONTID", "1");
    c.push("ISHIDDEN", "T");
    c.push("TEXT", &component.comment_text);
    c.push("NAME", "Comment");
    c.push("UNIQUEID", stable_unique_id(&component.name, "COMMENT"));
    writer.write_cstring_param_block(&c);
    write_implementation_records(&mut writer, component);
    writer.into_inner()
}

fn implementation_record_count(component: &Component) -> usize {
    if component.implementations.is_empty() {
        1
    } else {
        1 + component
            .implementations
            .iter()
            .map(|implementation| 3 + implementation.map_definers.len())
            .sum::<usize>()
    }
}

fn write_implementation_records(writer: &mut common::BinaryWriter, component: &Component) {
    let mut list = common::Params::default();
    list.push("RECORD", "44");
    writer.write_cstring_param_block(&list);

    for (implementation_index, implementation) in component.implementations.iter().enumerate() {
        let mut implementation_params = common::Params::default();
        implementation_params.push("RECORD", "45");
        if let Some(description) = implementation.description.as_deref() {
            implementation_params.push("DESCRIPTION", description);
        }
        implementation_params.push("MODELNAME", &implementation.model_name);
        implementation_params.push("MODELTYPE", &implementation.model_type);
        let paired_count = implementation
            .data_file_kinds
            .len()
            .min(implementation.data_file_entities.len());
        implementation_params.push("DATAFILECOUNT", paired_count.to_string());
        for (data_file_index, (kind, entity)) in implementation
            .data_file_kinds
            .iter()
            .zip(implementation.data_file_entities.iter())
            .enumerate()
        {
            implementation_params.push(format!("MODELDATAFILEKIND{}", data_file_index + 1), kind);
            implementation_params.push(
                format!("MODELDATAFILEENTITY{}", data_file_index + 1),
                entity,
            );
        }
        implementation_params.push_bool("ISCURRENT", implementation.is_current);
        implementation_params.push(
            "UNIQUEID",
            stable_unique_id(
                &component.name,
                &format!("IMPL{implementation_index}_{}", implementation.model_name),
            ),
        );
        writer.write_cstring_param_block(&implementation_params);

        let mut map_definer_list = common::Params::default();
        map_definer_list.push("RECORD", "46");
        writer.write_cstring_param_block(&map_definer_list);

        for (map_index, map_definer) in implementation.map_definers.iter().enumerate() {
            let mut map_params = common::Params::default();
            map_params.push("RECORD", "47");
            map_params.push("DESINTF", &map_definer.designator_interface);
            map_params.push(
                "DESIMPCOUNT",
                map_definer.designator_implementations.len().to_string(),
            );
            for (designator_index, designator) in
                map_definer.designator_implementations.iter().enumerate()
            {
                map_params.push(format!("DESIMP{designator_index}"), designator);
            }
            map_params.push_bool("ISTRIVIAL", map_definer.is_trivial);
            map_params.push(
                "UNIQUEID",
                stable_unique_id(
                    &component.name,
                    &format!(
                        "MAP{implementation_index}_{map_index}_{}",
                        map_definer.designator_interface
                    ),
                ),
            );
            writer.write_cstring_param_block(&map_params);
        }

        let mut implementation_parameters = common::Params::default();
        implementation_parameters.push("RECORD", "48");
        writer.write_cstring_param_block(&implementation_parameters);
    }
}

fn push_owned_part(params: &mut common::Params, owner_part_id: i32) {
    params.push_bool("ISNOTACCESIBLE", true);
    params.push("OWNERPARTID", owner_part_id.to_string());
}

fn format_angle(angle: f64) -> String {
    format!("{:.3}", common::normalize_angle(angle))
}

fn stable_unique_id(name: &str, salt: &str) -> String {
    common::stable_unique_id(name, salt)
}

fn pin_conglomerate(pin: &Pin) -> u8 {
    let mut flags = pin.orientation & 0x03;
    if pin.show_name {
        flags |= 0x08;
    }
    if pin.show_designator {
        flags |= 0x10;
    }
    flags
}

// ── Output type definitions ───────────────────────────────────────────────────

#[derive(Debug)]
pub(in crate::schlib) struct Pin {
    pub(in crate::schlib) designator: String,
    pub(in crate::schlib) name: String,
    pub(in crate::schlib) location: common::CoordPoint,
    pub(in crate::schlib) length_raw: i64,
    pub(in crate::schlib) orientation: u8,
    pub(in crate::schlib) show_name: bool,
    pub(in crate::schlib) show_designator: bool,
    pub(in crate::schlib) color_bgr: i32,
    pub(in crate::schlib) owner_part_id: i32,
    pub(in crate::schlib) owner_part_display_mode: i32,
}

#[derive(Debug)]
pub(in crate::schlib) struct Rectangle {
    pub(in crate::schlib) corner1: common::CoordPoint,
    pub(in crate::schlib) corner2: common::CoordPoint,
    pub(in crate::schlib) color_bgr: i32,
    pub(in crate::schlib) fill_color_bgr: i32,
    pub(in crate::schlib) is_filled: bool,
    pub(in crate::schlib) is_transparent: bool,
    pub(in crate::schlib) line_width_index: i32,
    pub(in crate::schlib) owner_part_id: i32,
}

#[derive(Debug)]
pub(in crate::schlib) struct Polyline {
    pub(in crate::schlib) points: Vec<common::CoordPoint>,
    pub(in crate::schlib) color_bgr: i32,
    pub(in crate::schlib) line_width_index: i32,
    pub(in crate::schlib) owner_part_id: i32,
}

#[derive(Debug)]
pub(in crate::schlib) struct Arc {
    pub(in crate::schlib) center: common::CoordPoint,
    pub(in crate::schlib) radius_raw: i64,
    pub(in crate::schlib) start_angle: f64,
    pub(in crate::schlib) end_angle: f64,
    pub(in crate::schlib) color_bgr: i32,
    pub(in crate::schlib) line_width_index: i32,
    pub(in crate::schlib) owner_part_id: i32,
}

#[derive(Debug)]
pub(in crate::schlib) struct Ellipse {
    pub(in crate::schlib) center: common::CoordPoint,
    pub(in crate::schlib) radius_x_raw: i64,
    pub(in crate::schlib) radius_y_raw: i64,
    pub(in crate::schlib) color_bgr: i32,
    pub(in crate::schlib) fill_color_bgr: i32,
    pub(in crate::schlib) is_filled: bool,
    pub(in crate::schlib) is_transparent: bool,
    pub(in crate::schlib) line_width_index: i32,
    pub(in crate::schlib) owner_part_id: i32,
}

#[derive(Debug)]
pub(in crate::schlib) struct Label {
    pub(in crate::schlib) text: String,
    pub(in crate::schlib) location: common::CoordPoint,
    pub(in crate::schlib) orientation: u8,
    pub(in crate::schlib) color_bgr: i32,
    pub(in crate::schlib) owner_part_id: i32,
}

#[derive(Debug)]
pub struct Component {
    pub(in crate::schlib) name: String,
    pub(in crate::schlib) description: String,
    pub(in crate::schlib) designator_text: String,
    pub(in crate::schlib) comment_text: String,
    pub(in crate::schlib) parameters: Vec<SchlibParameter>,
    pub(in crate::schlib) implementations: Vec<Implementation>,
    pub(in crate::schlib) part_count: usize,
    pub(in crate::schlib) pins: Vec<Pin>,
    pub(in crate::schlib) rectangles: Vec<Rectangle>,
    pub(in crate::schlib) polylines: Vec<Polyline>,
    pub(in crate::schlib) arcs: Vec<Arc>,
    pub(in crate::schlib) ellipses: Vec<Ellipse>,
    pub(in crate::schlib) labels: Vec<Label>,
}

impl Component {
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug)]
pub(in crate::schlib) struct Implementation {
    pub(in crate::schlib) description: Option<String>,
    pub(in crate::schlib) model_name: String,
    pub(in crate::schlib) model_type: String,
    pub(in crate::schlib) is_current: bool,
    pub(in crate::schlib) data_file_kinds: Vec<String>,
    pub(in crate::schlib) data_file_entities: Vec<String>,
    pub(in crate::schlib) map_definers: Vec<MapDefiner>,
}

#[derive(Debug)]
pub(in crate::schlib) struct MapDefiner {
    pub(in crate::schlib) designator_interface: String,
    pub(in crate::schlib) designator_implementations: Vec<String>,
    pub(in crate::schlib) is_trivial: bool,
}

#[cfg(test)]
mod tests {
    use super::{
        SchlibMetadata, SchlibParameter, build_component_from_payload,
        write_schlib_from_payload_with_metadata, write_schlib_library,
    };
    use crate::schlib::common;
    use serde_json::json;
    use std::fs::File;
    use std::io::Read;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_payload() -> serde_json::Value {
        json!({"result": {"dataStr": r#"["DOCTYPE","SYMBOL","1.1"]
["PART","U.1",{"BBOX":[-10,-10,10,10]}]
["RECT","body",-10,-10,10,10,0,0,0,"st1",0]
["ATTR","root1","","Symbol","TEST",false,false,null,null,0,"st3",0]
["ATTR","root2","","Designator","U?",false,false,null,null,0,"st3",0]
["PIN","p1",1,null,-20,0,10,0,null,0,0,1]
["ATTR","p1n","p1","NAME","A",false,true,-5,0,0,"st3",0]
["ATTR","p1d","p1","NUMBER","1",false,true,-10,0,0,"st4",0]
["PIN","p2",1,null,20,0,10,180,null,0,0,1]
["ATTR","p2n","p2","NAME","B",false,true,5,0,0,"st3",0]
["ATTR","p2d","p2","NUMBER","2",false,true,10,0,0,"st4",0]"#}})
    }

    #[test]
    fn writes_metadata_records_into_schlib() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("npnp_schlib_meta_{timestamp}.SchLib"));
        let metadata = SchlibMetadata {
            description: Some("CPU Core: -; CPU Maximum Speed: 133MHz;".to_string()),
            designator: Some("U?".to_string()),
            comment: Some("={Manufacturer Part}".to_string()),
            parameters: vec![
                SchlibParameter {
                    name: "Footprint".to_string(),
                    value: "LQFN-56_L7.0-W7.0-P0.4-EP".to_string(),
                },
                SchlibParameter {
                    name: "Manufacturer".to_string(),
                    value: "Raspberry Pi".to_string(),
                },
            ],
            footprint_model_name: Some("LQFN-56_L7.0-W7.0-P0.4-EP".to_string()),
            footprint_library_file: Some("MyLib.PcbLib".to_string()),
            name_override: None,
        };

        write_schlib_from_payload_with_metadata(&sample_payload(), "TEST/COMP", &metadata, &path)
            .unwrap();

        let file = File::open(&path).unwrap();
        let mut compound = cfb::CompoundFile::open(file).unwrap();
        let mut data_stream = compound.open_stream("/TEST_COMP/Data").unwrap();
        let mut data = Vec::new();
        data_stream.read_to_end(&mut data).unwrap();
        let data_text = String::from_utf8_lossy(&data);

        assert!(
            data_text.contains("|COMPONENTDESCRIPTION=CPU Core: -; CPU Maximum Speed: 133MHz;|")
        );
        assert!(data_text.contains("|RECORD=34|"));
        assert!(data_text.contains("|NAME=Designator|"));
        assert!(data_text.contains("|TEXT=U?|"));
        assert!(data_text.contains("|NAME=Comment|"));
        assert!(data_text.contains("|TEXT=={Manufacturer Part}|"));
        assert!(data_text.contains("|NAME=Footprint|"));
        assert!(data_text.contains("|TEXT=LQFN-56_L7.0-W7.0-P0.4-EP|"));
        assert!(data_text.contains("|NAME=Manufacturer|"));
        assert!(data_text.contains("|TEXT=Raspberry Pi|"));
        assert!(data_text.contains("|RECORD=45|"));
        assert!(data_text.contains("|MODELNAME=LQFN-56_L7.0-W7.0-P0.4-EP|"));
        assert!(data_text.contains("|MODELTYPE=PCBLIB|"));
        assert!(data_text.contains("|MODELDATAFILEKIND1=PCBLib|"));
        assert!(data_text.contains("|MODELDATAFILEENTITY1=MyLib.PcbLib|"));
        assert!(data_text.contains("|RECORD=46"));
        assert!(data_text.contains("|RECORD=47|"));
        assert!(data_text.contains("|DESINTF=1|"));
        assert!(data_text.contains("|DESIMP0=1|"));
        assert!(data_text.contains("|RECORD=48"));
    }

    #[test]
    fn writes_multi_component_schlib_with_unique_section_keys() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("npnp_schlib_multi_{timestamp}.SchLib"));
        let name_a = format!("{}1", "A".repeat(31));
        let name_b = format!("{}2", "A".repeat(31));
        let component_a = build_component_from_payload(&sample_payload(), &name_a).unwrap();
        let component_b = build_component_from_payload(&sample_payload(), &name_b).unwrap();

        write_schlib_library(&[component_a, component_b], &path).unwrap();

        let file = File::open(&path).unwrap();
        let mut compound = cfb::CompoundFile::open(file).unwrap();
        let mut header_stream = compound.open_stream("/FileHeader").unwrap();
        let mut header = Vec::new();
        header_stream.read_to_end(&mut header).unwrap();
        let header_text = String::from_utf8_lossy(&header);
        assert!(header_text.contains("|COMPCOUNT=2|"));

        let mut section_keys_stream = compound.open_stream("/SectionKeys").unwrap();
        let mut section_keys = Vec::new();
        section_keys_stream.read_to_end(&mut section_keys).unwrap();
        let section_keys_text = String::from_utf8_lossy(&section_keys);
        assert!(section_keys_text.contains("|KeyCount=2|"));

        let first_key = "A".repeat(31);
        let second_key = format!("{}{}", "A".repeat(29), "_2");
        assert!(compound.open_stream(&format!("/{first_key}/Data")).is_ok());
        assert!(compound.open_stream(&format!("/{second_key}/Data")).is_ok());

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn schematic_graphics_use_black_outline_colors() {
        let component = build_component_from_payload(&sample_payload(), "TEST/COMP").unwrap();

        assert!(
            component
                .rectangles
                .iter()
                .all(|item| item.color_bgr == common::SYMBOL_BGR)
        );
        assert!(
            component
                .polylines
                .iter()
                .all(|item| item.color_bgr == common::SYMBOL_BGR)
        );
        assert!(
            component
                .labels
                .iter()
                .all(|item| item.color_bgr == common::SYMBOL_BGR)
        );
        assert!(
            component
                .pins
                .iter()
                .all(|item| item.color_bgr == common::SYMBOL_BGR)
        );
    }
}
