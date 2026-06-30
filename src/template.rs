use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};
use crate::merge::{
    PcblibRecordLibrary, SchlibRecord, patch_schlib_data_component_name, read_pcblib_records,
    read_schlib_records, strip_schlib_params, write_pcblib_records, write_schlib_records,
};
use crate::pcblib::{
    CoordPoint, PcbComponent, PcbComponentBody, PcbLibrary, PcbModel, PcbPad, PcbTrack,
    LAYER_MECHANICAL_1, LAYER_TOP, LAYER_TOP_OVERLAY, PAD_HOLE_ROUND, PAD_SHAPE_RECTANGULAR,
    stable_guid,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentClass {
    ChipResistor,
    ChipCapacitor,
    Other,
}

/// Classify a component by its designator prefix (R/C only — others → Other).
pub fn classify_component(designator: &str) -> ComponentClass {
    let stem = designator
        .trim()
        .trim_end_matches(|c: char| c == '?' || c.is_ascii_digit())
        .trim()
        .to_ascii_uppercase();
    match stem.as_str() {
        "R" | "RV" | "VR" => ComponentClass::ChipResistor,
        "C" | "CV" => ComponentClass::ChipCapacitor,
        _ => ComponentClass::Other,
    }
}

/// Extract standard package size string from a footprint name.
/// Returns the canonical size string, e.g. "0402", "1206".
pub fn extract_package_size(footprint_name: &str) -> Option<String> {
    let upper = footprint_name.to_ascii_uppercase();
    // Ordered largest → smallest to avoid "0402" matching inside a longer number.
    for &size in &["2512", "2010", "1812", "1806", "1210", "1206", "0805", "0603", "0402", "0201"] {
        if let Some(pos) = upper.find(size) {
            let before_ok = pos == 0 || !upper.as_bytes()[pos - 1].is_ascii_digit();
            let after_pos = pos + size.len();
            let after_ok = after_pos >= upper.len() || !upper.as_bytes()[after_pos].is_ascii_digit();
            if before_ok && after_ok {
                return Some(size.to_string());
            }
        }
    }
    None
}

/// Canonical footprint name for the given class and package, e.g. "R0402", "C0603".
pub fn standard_footprint_name(class: ComponentClass, package: &str) -> String {
    let prefix = match class {
        ComponentClass::ChipResistor => "R",
        ComponentClass::ChipCapacitor => "C",
        ComponentClass::Other => return package.to_string(),
    };
    format!("{prefix}{package}")
}

/// File name of the consolidated SchLib template stored alongside the merged library output.
pub const TEMPLATE_SCHLIB: &str = "npnp_template.SchLib";
/// File name of the consolidated PcbLib template stored alongside the merged library output.
pub const TEMPLATE_PCBLIB: &str = "npnp_template.PcbLib";

/// Locate the `assets/` directory — checked in order:
/// 1. Next to the binary executable
/// 2. Current working directory
/// 3. `NPNP_ASSETS_DIR` environment variable
pub fn find_assets_dir() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let dir = parent.join("assets");
            if dir.is_dir() {
                return Some(dir);
            }
        }
    }
    let cwd = PathBuf::from("assets");
    if cwd.is_dir() {
        return Some(cwd);
    }
    if let Ok(env_dir) = std::env::var("NPNP_ASSETS_DIR") {
        let dir = PathBuf::from(env_dir);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    None
}

/// Try to load a local STEP file for the given class/package from the assets directory.
/// Tries `R0402.step`, `R0402.stp`, `0402.step` naming conventions.
pub fn load_local_step(assets_dir: &Path, class: ComponentClass, package: &str) -> Option<Vec<u8>> {
    let prefix = match class {
        ComponentClass::ChipResistor => "R",
        ComponentClass::ChipCapacitor => "C",
        ComponentClass::Other => return None,
    };
    let names = [
        format!("{prefix}{package}.step"),
        format!("{prefix}{package}.stp"),
        format!("{package}.step"),
        format!("{package}.stp"),
    ];
    for name in &names {
        if let Ok(data) = std::fs::read(assets_dir.join(name)) {
            return Some(data);
        }
    }
    None
}

/// Load a PcbLib footprint template for `class`/`package` from the given directory.
///
/// Checks `npnp_template.PcbLib` (consolidated runtime file) first, then falls back to
/// the legacy per-footprint file (e.g. `R0402.PcbLib`) for development/assets compat.
/// Returns `None` when neither is found.
pub(crate) fn load_pcblib_template_records(
    dir: &Path,
    class: ComponentClass,
    package: &str,
) -> Option<PcblibRecordLibrary> {
    let footprint_name = standard_footprint_name(class, package);

    // Primary: consolidated template file next to the library output.
    if let Ok(lib) = read_pcblib_records(&dir.join(TEMPLATE_PCBLIB)) {
        if lib.components.iter().any(|c| c.name.eq_ignore_ascii_case(&footprint_name)) {
            let comp = lib.components.into_iter()
                .find(|c| c.name.eq_ignore_ascii_case(&footprint_name))
                .unwrap();
            return Some(PcblibRecordLibrary { components: vec![comp], models: lib.models });
        }
    }

    // Fallback: legacy per-footprint file (assets/ style).
    read_pcblib_records(&dir.join(format!("{footprint_name}.PcbLib"))).ok()
}

/// Load a SchLib template record by exact name from `npnp_template.SchLib` in `dir`.
/// Returns `None` when the file is absent or the named record is not found.
pub(crate) fn load_schlib_template_by_name(dir: &Path, name: &str) -> Option<SchlibRecord> {
    read_schlib_records(&dir.join(TEMPLATE_SCHLIB)).ok()?
        .into_iter()
        .find(|r| r.name.eq_ignore_ascii_case(name))
}

/// Load a SchLib template record for `class` from the given directory.
///
/// Checks `npnp_template.SchLib` (consolidated runtime file) first, then falls back to
/// the legacy per-class files (`RES_template.SchLib` / `CAP_template.SchLib`).
/// Returns `None` when neither is found.
pub(crate) fn load_schlib_template_record(dir: &Path, class: ComponentClass) -> Option<SchlibRecord> {
    let template_name = match class {
        ComponentClass::ChipResistor => "R",
        ComponentClass::ChipCapacitor => "C",
        ComponentClass::Other => return None,
    };

    // Primary: consolidated template file next to the library output.
    if let Some(r) = load_schlib_template_by_name(dir, template_name) {
        return Some(r);
    }

    // Fallback: legacy per-class file (assets/ style).
    let legacy = match class {
        ComponentClass::ChipResistor => "RES_template.SchLib",
        ComponentClass::ChipCapacitor => "CAP_template.SchLib",
        ComponentClass::Other => return None,
    };
    read_schlib_records(&dir.join(legacy)).ok()?.into_iter().next()
}

/// Load a PcbLib footprint by exact name from `npnp_template.PcbLib` in `dir`.
/// Returns `None` when the file is absent or the named footprint is not found.
pub(crate) fn load_pcblib_template_by_name(dir: &Path, name: &str) -> Option<PcblibRecordLibrary> {
    let lib = read_pcblib_records(&dir.join(TEMPLATE_PCBLIB)).ok()?;
    let comp = lib.components.into_iter().find(|c| c.name.eq_ignore_ascii_case(name))?;
    Some(PcblibRecordLibrary { components: vec![comp], models: lib.models })
}

/// Upsert a PcbLib footprint by name into the consolidated `npnp_template.PcbLib` in `dir`.
/// Silently does nothing if the write fails (best-effort).
pub(crate) fn save_pcblib_template_by_name(dir: &Path, name: &str, lib: &PcblibRecordLibrary) {
    let path = dir.join(TEMPLATE_PCBLIB);
    let mut merged = read_pcblib_records(&path).unwrap_or_default();
    merged.components.retain(|c| !c.name.eq_ignore_ascii_case(name));
    merged.components.extend(lib.components.iter().cloned());
    merged.models.extend(lib.models.iter().cloned());
    let _ = write_pcblib_records(&merged, &path);
}

/// Upsert a PcbLib footprint into the consolidated `npnp_template.PcbLib` in `dir`.
/// Silently does nothing if the write fails (best-effort).
pub(crate) fn save_pcblib_template(
    dir: &Path,
    class: ComponentClass,
    package: &str,
    new_lib: &PcblibRecordLibrary,
) {
    let footprint_name = standard_footprint_name(class, package);
    let path = dir.join(TEMPLATE_PCBLIB);
    let mut merged = read_pcblib_records(&path).unwrap_or_default();
    merged.components.retain(|c| !c.name.eq_ignore_ascii_case(&footprint_name));
    merged.components.extend(new_lib.components.iter().cloned());
    merged.models.extend(new_lib.models.iter().cloned());
    let _ = write_pcblib_records(&merged, &path);
}

/// Upsert an already-processed SchLib template record into `npnp_template.SchLib` in `dir`
/// without re-stripping. Use this when promoting a template loaded from a fallback source.
/// Silently does nothing if the write fails (best-effort).
pub(crate) fn promote_schlib_template(dir: &Path, record: &SchlibRecord) {
    let path = dir.join(TEMPLATE_SCHLIB);
    let mut records: Vec<SchlibRecord> = read_schlib_records(&path).unwrap_or_default();
    records.retain(|r| !r.name.eq_ignore_ascii_case(&record.name));
    records.push(record.clone());
    let _ = write_schlib_records(&records, &path);
}

/// Strip component-specific parameters from a SchLib record and upsert it into the
/// consolidated `npnp_template.SchLib` in `dir`.
/// Silently does nothing if the write fails (best-effort).
pub(crate) fn save_schlib_template(
    dir: &Path,
    class: ComponentClass,
    record: &SchlibRecord,
) {
    let template_name = match class {
        ComponentClass::ChipResistor => "R",
        ComponentClass::ChipCapacitor => "C",
        ComponentClass::Other => return,
    };
    let stripped = strip_schlib_params(&record.data);
    let renamed = patch_schlib_data_component_name(&stripped, template_name, None);
    let tmpl = SchlibRecord {
        name: template_name.to_string(),
        description: String::new(),
        identity: None,
        data: renamed,
        weight: record.weight,
        header_part_count: record.header_part_count,
    };
    let path = dir.join(TEMPLATE_SCHLIB);
    let mut records: Vec<SchlibRecord> = read_schlib_records(&path).unwrap_or_default();
    records.retain(|r| !r.name.eq_ignore_ascii_case(template_name));
    records.push(tmpl);
    let _ = write_schlib_records(&records, &path);
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_resistor_designators() {
        assert_eq!(classify_component("R?"), ComponentClass::ChipResistor);
        assert_eq!(classify_component("R1"), ComponentClass::ChipResistor);
        assert_eq!(classify_component("RV3"), ComponentClass::ChipResistor);
    }

    #[test]
    fn classifies_capacitor_designators() {
        assert_eq!(classify_component("C?"), ComponentClass::ChipCapacitor);
        assert_eq!(classify_component("C12"), ComponentClass::ChipCapacitor);
    }

    #[test]
    fn classifies_other_designators() {
        assert_eq!(classify_component("U1"), ComponentClass::Other);
        assert_eq!(classify_component("L1"), ComponentClass::Other);
    }

    #[test]
    fn extracts_common_package_sizes() {
        assert_eq!(extract_package_size("C0402_L1.0-W0.5"), Some("0402".to_string()));
        assert_eq!(extract_package_size("R0603_L1.6-W0.8"), Some("0603".to_string()));
        assert_eq!(extract_package_size("0805_2012Metric"), Some("0805".to_string()));
        assert_eq!(extract_package_size("CAP_100N_0402_16V_X7R"), Some("0402".to_string()));
        assert_eq!(extract_package_size("1206_3216Metric"), Some("1206".to_string()));
    }

    #[test]
    fn extracts_no_package_from_non_standard_name() {
        assert_eq!(extract_package_size("UFQFPN-20_L3.0"), None);
        assert_eq!(extract_package_size("QFN-24"), None);
    }

    #[test]
    fn builds_standard_footprint_names() {
        assert_eq!(standard_footprint_name(ComponentClass::ChipResistor, "0402"), "R0402");
        assert_eq!(standard_footprint_name(ComponentClass::ChipCapacitor, "0603"), "C0603");
    }

    #[test]
    fn ipc_pcblib_has_two_pads() {
        let lib = build_ipc_pcblib(ComponentClass::ChipResistor, "0402", "R0402", None).unwrap();
        assert_eq!(lib.components.len(), 1);
        assert_eq!(lib.components[0].pads.len(), 2);
        assert_eq!(lib.components[0].pads[0].designator, "1");
        assert_eq!(lib.components[0].pads[1].designator, "2");
    }

    #[test]
    fn ipc_pcblib_has_courtyard_and_silkscreen() {
        let lib = build_ipc_pcblib(ComponentClass::ChipCapacitor, "0603", "C0603", None).unwrap();
        let tracks = &lib.components[0].tracks;
        assert!(tracks.len() >= 4, "expected at least 4 courtyard tracks, got {}", tracks.len());
    }

    #[test]
    fn ipc_pcblib_unknown_package_returns_error() {
        let result = build_ipc_pcblib(ComponentClass::ChipResistor, "9999", "R9999", None);
        assert!(result.is_err());
    }
}

// ── IPC-7351 nominal land-pattern dimensions ─────────────────────────────────

struct IpcDims {
    pad_w_mm: f64,
    pad_h_mm: f64,
    pitch_mm: f64,
    body_l_mm: f64,
    body_w_mm: f64,
    height_mm: f64,
}

fn ipc_dims(package: &str) -> Option<IpcDims> {
    Some(match package {
        "0201" => IpcDims { pad_w_mm: 0.28, pad_h_mm: 0.33, pitch_mm: 0.50, body_l_mm: 0.60, body_w_mm: 0.30, height_mm: 0.28 },
        "0402" => IpcDims { pad_w_mm: 0.56, pad_h_mm: 0.62, pitch_mm: 0.98, body_l_mm: 1.00, body_w_mm: 0.50, height_mm: 0.35 },
        "0603" => IpcDims { pad_w_mm: 0.87, pad_h_mm: 0.95, pitch_mm: 1.60, body_l_mm: 1.60, body_w_mm: 0.80, height_mm: 0.45 },
        "0805" => IpcDims { pad_w_mm: 1.17, pad_h_mm: 1.40, pitch_mm: 1.90, body_l_mm: 2.00, body_w_mm: 1.25, height_mm: 0.45 },
        "1206" => IpcDims { pad_w_mm: 1.57, pad_h_mm: 1.78, pitch_mm: 3.00, body_l_mm: 3.20, body_w_mm: 1.60, height_mm: 0.55 },
        "1210" => IpcDims { pad_w_mm: 1.57, pad_h_mm: 2.67, pitch_mm: 3.00, body_l_mm: 3.20, body_w_mm: 2.50, height_mm: 0.55 },
        "1806" => IpcDims { pad_w_mm: 1.57, pad_h_mm: 1.65, pitch_mm: 3.00, body_l_mm: 4.50, body_w_mm: 1.60, height_mm: 0.55 },
        "1812" => IpcDims { pad_w_mm: 1.57, pad_h_mm: 3.43, pitch_mm: 3.00, body_l_mm: 4.50, body_w_mm: 3.20, height_mm: 0.55 },
        "2010" => IpcDims { pad_w_mm: 2.07, pad_h_mm: 2.67, pitch_mm: 4.20, body_l_mm: 5.00, body_w_mm: 2.50, height_mm: 0.55 },
        "2512" => IpcDims { pad_w_mm: 2.07, pad_h_mm: 3.43, pitch_mm: 4.40, body_l_mm: 6.30, body_w_mm: 3.20, height_mm: 0.55 },
        _ => return None,
    })
}

// ── Coordinate helpers ────────────────────────────────────────────────────────

fn mm_to_raw(mm: f64) -> i32 {
    // 1 raw unit = 1/10000 mil; 1 mm = 39.3701 mils; 1 mm = 393701 raw
    (mm / 0.0254 * 10_000.0)
        .round()
        .clamp(i32::MIN as f64, i32::MAX as f64) as i32
}

fn pt(x_mm: f64, y_mm: f64) -> CoordPoint {
    CoordPoint::new(mm_to_raw(x_mm), mm_to_raw(y_mm))
}

fn make_smd_pad(designator: &str, x_mm: f64, y_mm: f64, w_mm: f64, h_mm: f64) -> PcbPad {
    let sz = CoordPoint::new(mm_to_raw(w_mm), mm_to_raw(h_mm));
    PcbPad {
        designator: designator.to_string(),
        location: pt(x_mm, y_mm),
        size_top: sz,
        size_middle: sz,
        size_bottom: sz,
        hole_size_raw: 0,
        shape_top: PAD_SHAPE_RECTANGULAR,
        shape_middle: PAD_SHAPE_RECTANGULAR,
        shape_bottom: PAD_SHAPE_RECTANGULAR,
        rotation: 0.0,
        is_plated: false,
        layer: LAYER_TOP,
        is_locked: false,
        is_tenting_top: false,
        is_tenting_bottom: false,
        is_keepout: false,
        mode: 0,
        power_plane_connect_style: 0,
        relief_air_gap_raw: 0,
        relief_conductor_width_raw: 0,
        relief_entries: 4,
        power_plane_clearance_raw: 0,
        power_plane_relief_expansion_raw: 0,
        paste_mask_expansion_raw: 0,
        solder_mask_expansion_raw: mm_to_raw(0.05),
        drill_type: 0,
        jumper_id: 0,
        hole_type: PAD_HOLE_ROUND,
        hole_slot_length_raw: 0,
        hole_rotation: 0.0,
        corner_radius_percentage: 0,
    }
}

fn make_track(layer: u8, x1: f64, y1: f64, x2: f64, y2: f64, w_mm: f64) -> PcbTrack {
    PcbTrack {
        layer,
        start: pt(x1, y1),
        end: pt(x2, y2),
        width_raw: mm_to_raw(w_mm),
        is_locked: false,
        is_tenting_top: false,
        is_tenting_bottom: false,
        is_keepout: false,
        net_index: 0,
        component_index: 0,
    }
}

// ── Programmatic IPC footprint builder ───────────────────────────────────────

/// Build a PcbLibrary with a single IPC-7351 nominal 2-pad SMD footprint.
/// Optionally embeds a STEP 3D model.
pub fn build_ipc_pcblib(
    class: ComponentClass,
    package: &str,
    component_name: &str,
    step_bytes: Option<Vec<u8>>,
) -> Result<PcbLibrary> {
    let dims = ipc_dims(package)
        .ok_or_else(|| AppError::Other(format!("no IPC dimensions defined for package '{package}'")))?;

    let half_pitch = dims.pitch_mm / 2.0;

    let pad1 = make_smd_pad("1", -half_pitch, 0.0, dims.pad_w_mm, dims.pad_h_mm);
    let pad2 = make_smd_pad("2",  half_pitch, 0.0, dims.pad_w_mm, dims.pad_h_mm);

    let mut tracks = Vec::new();

    // Silkscreen — short vertical lines flanking the body
    let inner_edge = half_pitch - dims.pad_w_mm / 2.0;
    if inner_edge > 0.05 {
        let silk_x = inner_edge - 0.05;
        let silk_y = dims.body_w_mm / 2.0 + 0.05;
        tracks.push(make_track(LAYER_TOP_OVERLAY, -silk_x, -silk_y, -silk_x, silk_y, 0.10));
        tracks.push(make_track(LAYER_TOP_OVERLAY,  silk_x, -silk_y,  silk_x, silk_y, 0.10));
    }

    // Courtyard (Mechanical 1) — box 0.25 mm beyond pad and body edges
    let cty_x = half_pitch + dims.pad_w_mm / 2.0 + 0.25;
    let cty_y = dims.body_w_mm / 2.0 + 0.25;
    tracks.push(make_track(LAYER_MECHANICAL_1, -cty_x, -cty_y,  cty_x, -cty_y, 0.05));
    tracks.push(make_track(LAYER_MECHANICAL_1,  cty_x, -cty_y,  cty_x,  cty_y, 0.05));
    tracks.push(make_track(LAYER_MECHANICAL_1,  cty_x,  cty_y, -cty_x,  cty_y, 0.05));
    tracks.push(make_track(LAYER_MECHANICAL_1, -cty_x,  cty_y, -cty_x, -cty_y, 0.05));

    let class_label = match class {
        ComponentClass::ChipResistor => "Resistor",
        ComponentClass::ChipCapacitor => "Capacitor",
        ComponentClass::Other => "Component",
    };
    let description = format!("IPC-7351 {package} Chip {class_label}");

    let mut bodies: Vec<PcbComponentBody> = Vec::new();
    let mut models: Vec<PcbModel> = Vec::new();

    if let Some(step_data) = step_bytes {
        let model_id = stable_guid(component_name);
        let outline = vec![
            pt(-dims.body_l_mm / 2.0, -dims.body_w_mm / 2.0),
            pt( dims.body_l_mm / 2.0, -dims.body_w_mm / 2.0),
            pt( dims.body_l_mm / 2.0,  dims.body_w_mm / 2.0),
            pt(-dims.body_l_mm / 2.0,  dims.body_w_mm / 2.0),
        ];
        bodies.push(PcbComponentBody {
            layer_name: "MECHANICAL1".to_string(),
            name: "__BODY__".to_string(),
            kind: 0,
            subpoly_index: -1,
            union_index: 0,
            arc_resolution_raw: 5_000,
            is_shape_based: false,
            cavity_height_raw: 0,
            standoff_height_raw: 0,
            overall_height_raw: mm_to_raw(dims.height_mm),
            body_color_3d: 0x808080,
            body_opacity_3d: 1.0,
            body_projection: 0,
            model_id: model_id.clone(),
            model_embed: true,
            model_2d_location: CoordPoint::new(0, 0),
            model_2d_rotation: 0.0,
            model_3d_rot_x: 0.0,
            model_3d_rot_y: 0.0,
            model_3d_rot_z: 0.0,
            model_3d_dz_raw: 0,
            model_checksum: 0,
            model_name: component_name.to_string(),
            model_type: 1,
            model_source: "Undefined".to_string(),
            identifier: None,
            texture: String::new(),
            outline,
            is_locked: false,
            is_tenting_top: false,
            is_tenting_bottom: false,
            is_keepout: false,
        });
        models.push(PcbModel {
            id: model_id,
            name: component_name.to_string(),
            is_embedded: true,
            model_source: "Undefined".to_string(),
            rotation_x: 0.0,
            rotation_y: 0.0,
            rotation_z: 0.0,
            dz_raw: 0,
            checksum: 0,
            step_data,
        });
    }

    let component = PcbComponent {
        name: component_name.to_string(),
        description,
        height_raw: mm_to_raw(dims.height_mm),
        pads: vec![pad1, pad2],
        arcs: vec![],
        tracks,
        regions: vec![],
        bodies,
        extended_primitive_information: vec![],
    };

    let mut library = PcbLibrary::default();
    library.models = models;
    library.components.push(component);
    Ok(library)
}
