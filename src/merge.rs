use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cfb::CompoundFile;
use encoding_rs::WINDOWS_1252;

use crate::error::{AppError, Result};
use crate::pcblib::{PcbLibrary, write_pcblib};
use crate::schlib::{Component, is_default_visible_parameter, write_schlib_library};

const PCBLIB_LIBRARY_DATA_TEMPLATE: &str = include_str!("pcblib_library_data_template.txt");

#[derive(Debug, Clone)]
pub(crate) struct SchlibRecord {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) header_part_count: usize,
    pub(crate) weight: usize,
    pub(crate) identity: Option<String>,
    pub(crate) data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct PcblibComponentRecord {
    pub(crate) name: String,
    pub(crate) primitive_count: i32,
    pub(crate) parameters: Vec<u8>,
    pub(crate) wide_strings: Vec<u8>,
    pub(crate) data: Vec<u8>,
    pub(crate) unique_id_primitive_information: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct PcblibModelRecord {
    pub(crate) entry: Vec<u8>,
    pub(crate) stream: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PcblibRecordLibrary {
    pub(crate) components: Vec<PcblibComponentRecord>,
    pub(crate) models: Vec<PcblibModelRecord>,
}

pub(crate) fn schlib_record_from_component(component: &Component) -> Result<SchlibRecord> {
    let path = temp_path("npnp_schlib_record", "SchLib");
    let result = (|| {
        write_schlib_library(std::slice::from_ref(component), &path)?;
        let mut records = read_schlib_records(&path)?;
        records.pop().ok_or_else(|| {
            AppError::Other("failed to capture temporary SchLib component".to_string())
        })
    })();
    fs::remove_file(&path).ok();
    result
}

pub(crate) fn pcblib_records_from_library(library: &PcbLibrary) -> Result<PcblibRecordLibrary> {
    let path = temp_path("npnp_pcblib_record", "PcbLib");
    let result = (|| {
        write_pcblib(library, &path)?;
        read_pcblib_records(&path)
    })();
    fs::remove_file(&path).ok();
    result
}

pub(crate) fn read_schlib_records(path: &Path) -> Result<Vec<SchlibRecord>> {
    let file = File::open(path)?;
    let mut compound = CompoundFile::open(file)?;
    let header = read_stream_bytes(&mut compound, "/FileHeader")?;
    let header_pairs = first_schlib_param_block_pairs(&header, "SchLib file header")?;
    let count = parse_usize_param(&header_pairs, "COMPCOUNT").unwrap_or(0);
    let names: Vec<String> = (0..count)
        .filter_map(|index| param_value(&header_pairs, &format!("LIBREF{index}")))
        .map(ToOwned::to_owned)
        .collect();

    let derived_sections = collect_sections(names.iter().map(String::as_str));
    let explicit_sections = read_schlib_section_keys(&mut compound).unwrap_or_default();
    let mut records = Vec::with_capacity(names.len());
    for (index, name) in names.into_iter().enumerate() {
        let section_key = resolve_section_key(
            &mut compound,
            &name,
            explicit_sections.get(&name).map(String::as_str),
            &derived_sections[index],
            "Data",
        )?;
        let data = read_stream_bytes(&mut compound, &format!("/{section_key}/Data"))?;
        records.push(parse_schlib_record(name, data)?);
    }
    Ok(records)
}

/// Patch the LIBREFERENCE (component name) and NPNP_COMPONENT_ID inside the raw SchLib
/// data byte stream of a component. Used after copying a template record so the embedded
/// name and identity match the actual component rather than the template source.
pub(crate) fn patch_schlib_data_component_name(
    data: &[u8],
    new_name: &str,
    new_identity: Option<&str>,
) -> Vec<u8> {
    let blocks = match parse_block_stream(data, "template patch") {
        Ok(b) => b,
        Err(_) => return data.to_vec(),
    };

    let mut out = Vec::with_capacity(data.len() + 128);

    for block in blocks {
        if block.flags != 0 {
            let header = ((block.flags as u32) << 24) | (block.payload.len() as u32);
            out.extend_from_slice(&header.to_le_bytes());
            out.extend_from_slice(block.payload);
            continue;
        }

        let text = schlib_cstring_text(block.payload);
        let pairs = parse_param_pairs(&text);

        let is_component_header = param_value(&pairs, "RECORD").is_some_and(|v| v == "1");
        let is_identity_param = new_identity.is_some()
            && param_value(&pairs, "NAME").is_some_and(matches_param_name);

        if !is_component_header && !is_identity_param {
            let header = block.payload.len() as u32;
            out.extend_from_slice(&header.to_le_bytes());
            out.extend_from_slice(block.payload);
            continue;
        }

        // Deduplicate pairs (last occurrence of each key wins, consistent with param_value).
        let mut deduped: Vec<(String, String)> = Vec::new();
        for (k, v) in &pairs {
            let key_lc = k.to_ascii_lowercase();
            if let Some(pos) = deduped.iter().position(|(ek, _)| ek.to_ascii_lowercase() == key_lc) {
                deduped[pos].1 = v.clone();
            } else {
                deduped.push((k.clone(), v.clone()));
            }
        }

        // Apply patches.
        for (k, v) in &mut deduped {
            if is_component_header && k.eq_ignore_ascii_case("LIBREFERENCE") {
                *v = new_name.to_string();
            }
            if is_identity_param {
                if k.eq_ignore_ascii_case("TEXT") {
                    *v = new_identity.unwrap().to_string();
                }
            }
        }

        let mut params = SchParams::default();
        for (k, v) in &deduped {
            params.push(k, v);
        }
        let mut writer = SchWriter::default();
        writer.write_block(0, |w| w.write_cstring(&params.as_string()));
        out.extend_from_slice(&writer.into_inner());
    }

    out
}

/// Return the TEXT value of the first RECORD=41 block whose NAME matches `key`
/// (case-insensitive). Returns `None` if the key is absent or the data is unparseable.
pub(crate) fn read_schlib_param(data: &[u8], key: &str) -> Option<String> {
    let blocks = parse_block_stream(data, "read param").ok()?;
    for block in &blocks {
        if block.flags != 0 {
            continue;
        }
        let text = schlib_cstring_text(block.payload);
        let pairs = parse_param_pairs(&text);
        if param_value(&pairs, "RECORD").is_some_and(|v| v == "41") {
            if let Some(name) = param_value(&pairs, "NAME") {
                if name.eq_ignore_ascii_case(key) {
                    return param_value(&pairs, "TEXT").map(|v| v.to_string());
                }
            }
        }
    }
    None
}

/// Return a copy of `data` with all RECORD=41 (user parameter) blocks removed.
/// Used to strip existing params from a donor symbol before re-patching with fresh ones.
pub(crate) fn strip_schlib_params(data: &[u8]) -> Vec<u8> {
    let blocks = match parse_block_stream(data, "strip params") {
        Ok(b) => b,
        Err(_) => return data.to_vec(),
    };
    let mut out = Vec::with_capacity(data.len());
    for block in &blocks {
        if block.flags == 0 {
            let text = schlib_cstring_text(block.payload);
            let pairs = parse_param_pairs(&text);
            if param_value(&pairs, "RECORD").is_some_and(|v| v == "41") {
                continue;
            }
        }
        let header = if block.flags != 0 {
            ((block.flags as u32) << 24) | (block.payload.len() as u32)
        } else {
            block.payload.len() as u32
        };
        out.extend_from_slice(&header.to_le_bytes());
        out.extend_from_slice(block.payload);
    }
    out
}

/// Read the DESINTF values from all RECORD=47 (pin map) blocks in a SchLib data stream.
/// Used to preserve the correct pin designators when reusing a donor symbol.
pub(crate) fn read_schlib_pin_designators(data: &[u8]) -> Vec<String> {
    let Ok(blocks) = parse_block_stream(data, "read pins") else {
        return Vec::new();
    };
    let mut pins = Vec::new();
    for block in &blocks {
        if block.flags != 0 {
            continue;
        }
        let text = schlib_cstring_text(block.payload);
        let pairs = parse_param_pairs(&text);
        if param_value(&pairs, "RECORD").is_some_and(|v| v == "47") {
            if let Some(des) = param_value(&pairs, "DESINTF") {
                pins.push(des.to_string());
            }
        }
    }
    pins
}

/// Patch the component header and matching parameter blocks in a SchLib data stream.
/// Extends `patch_schlib_data_component_name` by also replacing TEXT values for any
/// RECORD=41 parameter block whose NAME appears in `replacements`, AND inserts new
/// RECORD=41 blocks for any replacement keys that have no existing block. New blocks
/// are inserted just before RECORD=44 (the implementation group) or appended at end.
/// If `footprint` is `Some((model_name, library_file))`, RECORD=45/46/47 footprint model
/// blocks are written after RECORD=44; any pre-existing ones are stripped (idempotent).
/// `pin_designators` lists the pin designators used in the map; pass `&["1","2"]` for 2-pin
/// templates (all current cap/res templates).
/// Parameter name renames applied when patching templates: `(old_name_lowercase, new_name)`.
/// Old names exist in pre-existing template files; the patch renames them in the output so the
/// component ends up with the new canonical name without leaving a stale empty slot behind.
pub(crate) const SCHLIB_PARAM_RENAMES: &[(&str, &str)] = &[
    ("manufacturer part", "MPN"),
    ("supplier part", "Supplier Part Number"),
];

pub(crate) fn patch_schlib_data_with_params(
    data: &[u8],
    new_name: &str,
    new_description: Option<&str>,
    replacements: &HashMap<String, String>,
    footprint: Option<(&str, &str)>,
    pin_designators: &[&str],
    renames: &[(&str, &str)],
    designator: &str,
) -> Vec<u8> {
    let blocks = match parse_block_stream(data, "template patch") {
        Ok(b) => b,
        Err(_) => return data.to_vec(),
    };
    let lookup: HashMap<String, &str> = replacements
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.as_str()))
        .collect();
    let renames_map: HashMap<&str, &str> = renames.iter().map(|(o, n)| (*o, *n)).collect();

    // First pass: find which replacement keys already have a matching parameter block.
    let mut found_keys: HashSet<String> = HashSet::new();
    for block in &blocks {
        if block.flags != 0 {
            continue;
        }
        let text = schlib_cstring_text(block.payload);
        let pairs = parse_param_pairs(&text);
        if let Some(name) = param_value(&pairs, "NAME") {
            let lc = name.to_ascii_lowercase();
            if lookup.contains_key(&lc) {
                found_keys.insert(lc.clone());
            }
            // If this slot is being renamed, mark the new name as found too so no duplicate
            // block is inserted for the new name.
            if let Some(&new_slot_name) = renames_map.get(lc.as_str()) {
                found_keys.insert(new_slot_name.to_ascii_lowercase());
                found_keys.insert(lc);
            }
        }
    }

    // Collect keys that need new blocks (ordered for determinism).
    let mut missing: Vec<(&str, &str)> = replacements
        .iter()
        .filter(|(k, _)| {
            let lc = k.to_ascii_lowercase();
            !found_keys.contains(&lc)
        })
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    // Always ensure a hidden Comment annotation is present so Altium doesn't fall back
    // to displaying the Design Item ID as the comment on placed schematic components.
    if !found_keys.contains("comment") && !replacements.keys().any(|k| k.eq_ignore_ascii_case("Comment")) {
        missing.push(("Comment", ""));
    }
    missing.sort_by_key(|(k, _)| *k);

    // Second pass: write output with patches applied and new blocks inserted.
    let mut out = Vec::with_capacity(data.len() + 256 * missing.len().saturating_add(1));
    let mut inserted_missing = false;

    for block in &blocks {
        if block.flags != 0 {
            let header = ((block.flags as u32) << 24) | (block.payload.len() as u32);
            out.extend_from_slice(&header.to_le_bytes());
            out.extend_from_slice(block.payload);
            continue;
        }

        let text = schlib_cstring_text(block.payload);
        let pairs = parse_param_pairs(&text);
        let record_value = param_value(&pairs, "RECORD");

        // Skip stale footprint implementation records (RECORD=45/46/47) from the template.
        // Fresh ones are written after RECORD=44 below when `footprint` is provided.
        if record_value.is_some_and(|v| matches!(v, "45" | "46" | "47")) {
            continue;
        }

        // Insert new parameter blocks before the implementation group (RECORD=44).
        if !inserted_missing
            && !missing.is_empty()
            && record_value.is_some_and(|v| v == "44")
        {
            write_new_param_blocks(&mut out, &missing, new_name, designator);
            inserted_missing = true;
        }

        let is_component_header = record_value.is_some_and(|v| v == "1");
        let param_name_lc = param_value(&pairs, "NAME").map(|n| n.to_ascii_lowercase());
        let rename_to = param_name_lc.as_deref().and_then(|n| renames_map.get(n).copied());
        // When a slot is being renamed, look up the replacement value by new name.
        let effective_lc = rename_to
            .map(|n| n.to_ascii_lowercase())
            .or_else(|| param_name_lc.clone());
        let replacement_value = effective_lc.as_deref().and_then(|n| lookup.get(n).copied());

        let is_record_44 = record_value.is_some_and(|v| v == "44");
        let needs_patch = is_component_header || replacement_value.is_some() || rename_to.is_some();
        if !needs_patch {
            let header = block.payload.len() as u32;
            out.extend_from_slice(&header.to_le_bytes());
            out.extend_from_slice(block.payload);
            // After writing RECORD=44, inject footprint model blocks.
            if is_record_44 {
                if let Some((model_name, library_file)) = footprint {
                    write_footprint_impl_blocks(&mut out, model_name, library_file, pin_designators, new_name);
                }
            }
            continue;
        }

        let mut deduped: Vec<(String, String)> = Vec::new();
        for (k, v) in &pairs {
            let key_lc = k.to_ascii_lowercase();
            if let Some(pos) = deduped
                .iter()
                .position(|(ek, _)| ek.to_ascii_lowercase() == key_lc)
            {
                deduped[pos].1 = v.clone();
            } else {
                deduped.push((k.clone(), v.clone()));
            }
        }

        let mut text_was_set = false;
        let mut desc_was_set = false;
        for (k, v) in &mut deduped {
            if is_component_header {
                if k.eq_ignore_ascii_case("LIBREFERENCE") {
                    *v = new_name.to_string();
                }
                if let Some(desc) = new_description {
                    if k.eq_ignore_ascii_case("COMPONENTDESCRIPTION") {
                        *v = desc.to_string();
                        desc_was_set = true;
                    }
                }
            }
            if k.eq_ignore_ascii_case("NAME") {
                if let Some(new_slot_name) = rename_to {
                    *v = new_slot_name.to_string();
                }
            }
            if let Some(new_val) = replacement_value {
                if k.eq_ignore_ascii_case("TEXT") {
                    *v = new_val.to_string();
                    text_was_set = true;
                }
            }
        }
        // Insert COMPONENTDESCRIPTION if the component header block didn't already have it.
        if is_component_header {
            if let Some(desc) = new_description {
                if !desc_was_set && !desc.is_empty() {
                    deduped.push(("COMPONENTDESCRIPTION".to_string(), desc.to_string()));
                }
            }
        }
        // If the block had no TEXT field (value was wiped), add it.
        if let Some(new_val) = replacement_value {
            if !text_was_set && !new_val.is_empty() {
                deduped.push(("TEXT".to_string(), new_val.to_string()));
            }
        }
        // Ensure ISHYPERLINK=T when the replacement value is a URL.
        // Template slots may lack this flag; add it so the link is clickable in Altium.
        if let Some(new_val) = replacement_value {
            let is_url = new_val.starts_with("http://") || new_val.starts_with("https://");
            if is_url && !deduped.iter().any(|(k, _)| k.eq_ignore_ascii_case("ISHYPERLINK")) {
                deduped.push(("ISHYPERLINK".to_string(), "T".to_string()));
            }
        }

        let mut params = SchParams::default();
        for (k, v) in &deduped {
            params.push(k, v);
        }
        let mut writer = SchWriter::default();
        writer.write_block(0, |w| w.write_cstring(&params.as_string()));
        out.extend_from_slice(&writer.into_inner());
    }

    // Fallback: if no RECORD=44 was found, append new blocks at the end.
    if !inserted_missing && !missing.is_empty() {
        write_new_param_blocks(&mut out, &missing, new_name, designator);
    }

    out
}

fn write_new_param_blocks(out: &mut Vec<u8>, params: &[(&str, &str)], component_name: &str, designator: &str) {
    let mut visible_index = 0i32;
    for (name, value) in params {
        let is_url = value.starts_with("http://") || value.starts_with("https://");
        let is_visible = !is_url && is_default_visible_parameter(designator, name);
        let mut p = SchParams::default();
        p.push("RECORD", "41");
        p.push("OWNERPARTID", "-1");
        p.push("LOCATION.X_FRAC", "-5");
        if is_visible {
            let y = -20 - visible_index * 10;
            visible_index += 1;
            p.push("LOCATION.Y", &y.to_string());
        } else {
            p.push("LOCATION.Y_FRAC", "-15");
        }
        p.push("COLOR", "8388608");
        p.push("FONTID", "1");
        if !is_visible {
            p.push("ISHIDDEN", "T");
        }
        if is_url {
            p.push("ISHYPERLINK", "T");
        }
        p.push("TEXT", *value);
        p.push("NAME", *name);
        p.push(
            "UNIQUEID",
            schlib_stable_unique_id(component_name, &format!("PARAM_{name}")),
        );
        let mut writer = SchWriter::default();
        writer.write_block(0, |w| w.write_cstring(&p.as_string()));
        out.extend_from_slice(&writer.into_inner());
    }
}

fn write_footprint_impl_blocks(
    out: &mut Vec<u8>,
    model_name: &str,
    library_file: &str,
    pin_designators: &[&str],
    component_name: &str,
) {
    // RECORD=45: implementation entry
    let mut p = SchParams::default();
    p.push("RECORD", "45");
    p.push("DESCRIPTION", "PCB footprint");
    p.push("MODELNAME", model_name);
    p.push("MODELTYPE", "PCBLIB");
    p.push("DATAFILECOUNT", "1");
    p.push("MODELDATAFILEKIND1", "PCBLib");
    p.push("MODELDATAFILEENTITY1", library_file);
    p.push("ISCURRENT", "T");
    p.push("UNIQUEID", schlib_stable_unique_id(component_name, &format!("IMPL0_{model_name}")));
    let mut w = SchWriter::default();
    w.write_block(0, |wr| wr.write_cstring(&p.as_string()));
    out.extend_from_slice(&w.into_inner());

    // RECORD=46: map definer list header
    let mut p = SchParams::default();
    p.push("RECORD", "46");
    let mut w = SchWriter::default();
    w.write_block(0, |wr| wr.write_cstring(&p.as_string()));
    out.extend_from_slice(&w.into_inner());

    // RECORD=47: one entry per pin designator
    for (map_index, designator) in pin_designators.iter().enumerate() {
        let mut p = SchParams::default();
        p.push("RECORD", "47");
        p.push("DESINTF", *designator);
        p.push("DESIMPCOUNT", "1");
        p.push("DESIMP0", *designator);
        p.push("ISTRIVIAL", "T");
        p.push(
            "UNIQUEID",
            schlib_stable_unique_id(
                component_name,
                &format!("MAP0_{map_index}_{designator}"),
            ),
        );
        let mut w = SchWriter::default();
        w.write_block(0, |wr| wr.write_cstring(&p.as_string()));
        out.extend_from_slice(&w.into_inner());
    }
}

/// FNV-1a hash → 8-char base-26 ID, matching schlib_common::stable_unique_id.
fn schlib_stable_unique_id(name: &str, salt: &str) -> String {
    const ALPHABET: &[u8; 26] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut hash: u64 = 0xCBF29CE484222325;
    for byte in name.bytes().chain([b'|']).chain(salt.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001B3);
    }
    let mut value = hash;
    let mut id = String::with_capacity(8);
    for _ in 0..8 {
        id.push(ALPHABET[(value % 26) as usize] as char);
        value /= 26;
    }
    id
}

pub(crate) fn write_schlib_records(records: &[SchlibRecord], output_path: &Path) -> Result<()> {
    if records.is_empty() {
        return Err(AppError::Other(
            "cannot write empty SchLib library".to_string(),
        ));
    }

    let sections = collect_sections(records.iter().map(|record| record.name.as_str()));
    let section_pairs =
        collect_section_key_pairs(records.iter().map(|record| record.name.as_str()), &sections);
    let file = File::create(output_path)?;
    let mut compound = CompoundFile::create(file)?;

    write_stream(
        &mut compound,
        "/FileHeader",
        &schlib_file_header_bytes(records),
    )?;
    if !section_pairs.is_empty() {
        write_stream(
            &mut compound,
            "/SectionKeys",
            &schlib_section_keys_bytes(&section_pairs),
        )?;
    }
    for (record, section_key) in records.iter().zip(sections.iter()) {
        compound.create_storage(&format!("/{section_key}/"))?;
        write_stream(&mut compound, &format!("/{section_key}/Data"), &record.data)?;
    }
    write_stream(&mut compound, "/Storage", &schlib_storage_bytes())?;
    compound.flush()?;
    Ok(())
}

pub(crate) fn read_pcblib_records(path: &Path) -> Result<PcblibRecordLibrary> {
    let file = File::open(path)?;
    let mut compound = CompoundFile::open(file)?;
    let names = read_pcblib_component_names(&mut compound)?;
    let derived_sections = collect_sections(names.iter().map(String::as_str));
    let explicit_sections = read_pcblib_section_keys(&mut compound).unwrap_or_default();

    let mut components = Vec::with_capacity(names.len());
    for (index, name) in names.into_iter().enumerate() {
        let section_key = resolve_section_key(
            &mut compound,
            &name,
            explicit_sections.get(&name).map(String::as_str),
            &derived_sections[index],
            "Header",
        )?;
        let primitive_count = read_storage_header(
            &mut compound,
            &format!("/{section_key}/Header"),
            "PcbLib component header",
        )?;
        let parameters = read_stream_bytes(&mut compound, &format!("/{section_key}/Parameters"))?;
        let wide_strings =
            read_stream_bytes(&mut compound, &format!("/{section_key}/WideStrings"))?;
        let data = read_stream_bytes(&mut compound, &format!("/{section_key}/Data"))?;
        let unique_id_primitive_information = read_stream_bytes(
            &mut compound,
            &format!("/{section_key}/UniqueIdPrimitiveInformation/Data"),
        )?;
        let _ = first_param_block_pairs(&parameters, "PcbLib parameters")?;
        components.push(PcblibComponentRecord {
            name,
            primitive_count,
            parameters,
            wide_strings,
            data,
            unique_id_primitive_information,
        });
    }

    let model_count = read_storage_header(
        &mut compound,
        "/Library/Models/Header",
        "PcbLib models header",
    )?;
    let model_entries = read_pcblib_model_entries(&mut compound, model_count.max(0) as usize)?;

    Ok(PcblibRecordLibrary {
        components,
        models: model_entries,
    })
}

pub(crate) fn write_pcblib_records(
    library: &PcblibRecordLibrary,
    output_path: &Path,
) -> Result<()> {
    let file = File::create(output_path)?;
    let mut compound = CompoundFile::create(file)?;
    let sections = collect_sections(library.components.iter().map(|record| record.name.as_str()));
    let section_pairs = collect_section_key_pairs(
        library.components.iter().map(|record| record.name.as_str()),
        &sections,
    );

    write_stream(&mut compound, "/FileHeader", &pcblib_file_header_bytes())?;
    if !section_pairs.is_empty() {
        write_stream(
            &mut compound,
            "/SectionKeys",
            &pcblib_section_keys_bytes(&section_pairs),
        )?;
    }

    compound.create_storage("/Library/")?;
    write_stream(&mut compound, "/Library/Header", &storage_header_bytes(1))?;
    write_stream(
        &mut compound,
        "/Library/Data",
        &pcblib_library_data_bytes(library, output_path),
    )?;

    compound.create_storage("/Library/Models/")?;
    write_stream(
        &mut compound,
        "/Library/Models/Header",
        &storage_header_bytes(library.models.len() as i32),
    )?;
    write_stream(
        &mut compound,
        "/Library/Models/Data",
        &pcblib_models_data_bytes(&library.models),
    )?;
    for (index, model) in library.models.iter().enumerate() {
        write_stream(
            &mut compound,
            &format!("/Library/Models/{index}"),
            &model.stream,
        )?;
    }

    compound.create_storage("/Library/Textures/")?;
    write_stream(
        &mut compound,
        "/Library/Textures/Header",
        &storage_header_bytes(0),
    )?;
    write_stream(&mut compound, "/Library/Textures/Data", &[])?;

    compound.create_storage("/Library/ModelsNoEmbed/")?;
    write_stream(
        &mut compound,
        "/Library/ModelsNoEmbed/Header",
        &storage_header_bytes(0),
    )?;
    write_stream(&mut compound, "/Library/ModelsNoEmbed/Data", &[])?;

    for (component, section_key) in library.components.iter().zip(sections.iter()) {
        compound.create_storage(&format!("/{section_key}/"))?;
        write_stream(
            &mut compound,
            &format!("/{section_key}/Header"),
            &storage_header_bytes(component.primitive_count),
        )?;
        write_stream(
            &mut compound,
            &format!("/{section_key}/Parameters"),
            &component.parameters,
        )?;
        write_stream(
            &mut compound,
            &format!("/{section_key}/WideStrings"),
            &component.wide_strings,
        )?;
        write_stream(
            &mut compound,
            &format!("/{section_key}/Data"),
            &component.data,
        )?;
        compound.create_storage(&format!("/{section_key}/UniqueIdPrimitiveInformation/"))?;
        write_stream(
            &mut compound,
            &format!("/{section_key}/UniqueIdPrimitiveInformation/Header"),
            &storage_header_bytes(component.primitive_count),
        )?;
        write_stream(
            &mut compound,
            &format!("/{section_key}/UniqueIdPrimitiveInformation/Data"),
            &component.unique_id_primitive_information,
        )?;
    }

    compound.flush()?;
    Ok(())
}

pub(crate) fn normalize_lcsc_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let Some(digits) = trimmed
        .strip_prefix('C')
        .or_else(|| trimmed.strip_prefix('c'))
    else {
        return None;
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(format!("C{digits}"))
}

/// Read an existing `.SchLib` and return the deduplicated list of LCSC part IDs
/// found in each component's `Supplier Part` (or equivalent) parameter.
pub(crate) fn extract_lcsc_ids_from_schlib(path: &Path) -> Result<Vec<String>> {
    let records = read_schlib_records(path)?;
    let mut seen = HashSet::new();
    let ids = records
        .into_iter()
        .filter_map(|record| record.identity)
        .filter(|id| seen.insert(id.clone()))
        .collect();
    Ok(ids)
}

fn parse_schlib_record(name: String, data: Vec<u8>) -> Result<SchlibRecord> {
    let blocks = parse_block_stream(&data, "SchLib component data")?;
    let mut description = String::new();
    let mut header_part_count = 2usize;
    let mut identity = None;

    for block in &blocks {
        if block.flags != 0 {
            continue;
        }
        let pairs = parse_param_pairs(&schlib_cstring_text(block.payload));
        if param_value(&pairs, "RECORD").is_some_and(|value| value == "1") {
            if let Some(value) = param_value(&pairs, "COMPONENTDESCRIPTION") {
                description = value.to_string();
            }
            if let Some(value) =
                param_value(&pairs, "PARTCOUNT").and_then(|value| value.parse().ok())
            {
                header_part_count = value;
            }
        }
        if identity.is_none() {
            identity = extract_schlib_identity(&pairs);
        }
    }

    Ok(SchlibRecord {
        name,
        description,
        header_part_count,
        weight: blocks.len(),
        identity,
        data,
    })
}

fn read_schlib_section_keys(compound: &mut CompoundFile<File>) -> Result<HashMap<String, String>> {
    let data = match read_stream_bytes(compound, "/SectionKeys") {
        Ok(data) => data,
        Err(_) => return Ok(HashMap::new()),
    };
    let pairs = first_schlib_param_block_pairs(&data, "SchLib section keys")?;
    let count = parse_usize_param(&pairs, "KeyCount").unwrap_or(0);
    let mut sections = HashMap::with_capacity(count);
    for index in 0..count {
        let Some(name) = param_value(&pairs, &format!("LibRef{index}")) else {
            continue;
        };
        let Some(section_key) = param_value(&pairs, &format!("SectionKey{index}")) else {
            continue;
        };
        if !name.trim().is_empty() && !section_key.trim().is_empty() {
            sections.insert(name.to_string(), section_key.to_string());
        }
    }
    Ok(sections)
}

fn extract_schlib_identity(pairs: &[(String, String)]) -> Option<String> {
    let name = param_value(pairs, "NAME")?;
    let value = param_value(pairs, "TEXT")?;
    if !matches_param_name(name) {
        return None;
    }
    normalize_lcsc_id(value)
}

fn matches_param_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "npnp_component_id" | "supplier part" | "supplier part number" | "lcsc id"
    )
}

fn read_pcblib_component_names(compound: &mut CompoundFile<File>) -> Result<Vec<String>> {
    let data = read_stream_bytes(compound, "/Library/Data")?;
    let mut offset = 0usize;
    let _ = read_block(&data, &mut offset, "PcbLib library data params")?;
    if offset + 4 > data.len() {
        return Err(AppError::InvalidResponse(
            "invalid PcbLib library data stream".to_string(),
        ));
    }
    let count = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let block = read_block(&data, &mut offset, "PcbLib component name")?;
        names.push(parse_pascal_short_string(block.payload));
    }
    Ok(names)
}

fn read_pcblib_section_keys(compound: &mut CompoundFile<File>) -> Result<HashMap<String, String>> {
    let data = match read_stream_bytes(compound, "/SectionKeys") {
        Ok(data) => data,
        Err(_) => return Ok(HashMap::new()),
    };
    let mut offset = 0usize;
    if offset + 4 > data.len() {
        return Err(AppError::InvalidResponse(
            "invalid PcbLib section keys stream".to_string(),
        ));
    }
    let count = i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()).max(0) as usize;
    offset += 4;
    let mut sections = HashMap::with_capacity(count);
    for _ in 0..count {
        let name_block = read_block(&data, &mut offset, "PcbLib section key name")?;
        let key_block = read_block(&data, &mut offset, "PcbLib section key value")?;
        let name = parse_pascal_short_string(name_block.payload);
        let section_key = parse_pascal_short_string(key_block.payload);
        if !name.trim().is_empty() && !section_key.trim().is_empty() {
            sections.insert(name, section_key);
        }
    }
    Ok(sections)
}

fn read_pcblib_model_entries(
    compound: &mut CompoundFile<File>,
    count: usize,
) -> Result<Vec<PcblibModelRecord>> {
    let data = read_stream_bytes(compound, "/Library/Models/Data")?;
    let mut offset = 0usize;
    let mut records = Vec::with_capacity(count);
    for index in 0..count {
        if offset + 4 > data.len() {
            return Err(AppError::InvalidResponse(
                "invalid PcbLib models data stream".to_string(),
            ));
        }
        let len = i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
        if len < 0 {
            return Err(AppError::InvalidResponse(
                "invalid negative PcbLib model record length".to_string(),
            ));
        }
        let len = len as usize;
        offset += 4;
        if offset + len > data.len() {
            return Err(AppError::InvalidResponse(
                "truncated PcbLib model record".to_string(),
            ));
        }
        let entry = data[offset..offset + len].to_vec();
        offset += len;
        let stream = read_stream_bytes(compound, &format!("/Library/Models/{index}"))?;
        records.push(PcblibModelRecord { entry, stream });
    }
    Ok(records)
}

fn read_storage_header(compound: &mut CompoundFile<File>, path: &str, label: &str) -> Result<i32> {
    let data = read_stream_bytes(compound, path)?;
    if data.len() < 4 {
        return Err(AppError::InvalidResponse(format!("invalid {label} stream")));
    }
    Ok(i32::from_le_bytes(data[..4].try_into().unwrap()))
}

fn read_stream_bytes(compound: &mut CompoundFile<File>, path: &str) -> Result<Vec<u8>> {
    let mut stream = compound.open_stream(path)?;
    let mut data = Vec::new();
    use std::io::Read as _;
    stream.read_to_end(&mut data)?;
    Ok(data)
}

fn resolve_section_key(
    compound: &mut CompoundFile<File>,
    name: &str,
    explicit: Option<&str>,
    derived: &str,
    required_stream: &str,
) -> Result<String> {
    let candidates = section_key_candidates(name, explicit, derived);
    for section_key in &candidates {
        if compound
            .open_stream(&format!("/{section_key}/{required_stream}"))
            .is_ok()
        {
            return Ok(section_key.clone());
        }
    }

    let tried = candidates
        .iter()
        .map(|section_key| format!("/{section_key}/{required_stream}"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(AppError::Other(format!(
        "component {name:?} has no {required_stream} stream; tried {tried}"
    )))
}

fn first_param_block_pairs(data: &[u8], label: &str) -> Result<Vec<(String, String)>> {
    let mut offset = 0usize;
    let block = read_block(data, &mut offset, label)?;
    if block.flags != 0 {
        return Err(AppError::InvalidResponse(format!(
            "missing text block in {label}"
        )));
    }
    Ok(parse_param_pairs(&cstring_text(block.payload)))
}

fn first_schlib_param_block_pairs(data: &[u8], label: &str) -> Result<Vec<(String, String)>> {
    let mut offset = 0usize;
    let block = read_block(data, &mut offset, label)?;
    if block.flags != 0 {
        return Err(AppError::InvalidResponse(format!(
            "missing text block in {label}"
        )));
    }
    Ok(parse_param_pairs(&schlib_cstring_text(block.payload)))
}

fn parse_usize_param(pairs: &[(String, String)], key: &str) -> Option<usize> {
    param_value(pairs, key)?.trim().parse().ok()
}

fn param_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .rev()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_str())
}

fn parse_param_pairs(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for segment in text.split('|').filter(|segment| !segment.is_empty()) {
        let Some((name, value)) = segment.split_once('=') else {
            continue;
        };
        if let Some(key) = name.strip_prefix("%UTF8%") {
            pairs.push((key.to_string(), decode_utf8_parameter_value(value)));
        } else {
            pairs.push((name.to_string(), value.to_string()));
        }
    }
    pairs
}

fn cstring_text(data: &[u8]) -> String {
    let len = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(data.len());
    let (text, _, _) = WINDOWS_1252.decode(&data[..len]);
    text.into_owned()
}

fn schlib_cstring_text(data: &[u8]) -> String {
    let len = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(data.len());
    let (text, _, _) = WINDOWS_1252.decode(&data[..len]);
    text.into_owned()
}

fn decode_utf8_parameter_value(text: &str) -> String {
    let (bytes, _, _) = WINDOWS_1252.encode(text);
    String::from_utf8_lossy(&bytes).into_owned()
}

#[derive(Debug, Clone, Copy)]
struct Block<'a> {
    flags: u8,
    payload: &'a [u8],
}

fn parse_block_stream<'a>(data: &'a [u8], label: &str) -> Result<Vec<Block<'a>>> {
    let mut offset = 0usize;
    let mut blocks = Vec::new();
    while offset < data.len() {
        blocks.push(read_block(data, &mut offset, label)?);
    }
    Ok(blocks)
}

fn read_block<'a>(data: &'a [u8], offset: &mut usize, label: &str) -> Result<Block<'a>> {
    if *offset + 4 > data.len() {
        return Err(AppError::InvalidResponse(format!(
            "truncated {label} block header"
        )));
    }
    let header = u32::from_le_bytes(data[*offset..*offset + 4].try_into().unwrap());
    *offset += 4;
    let flags = (header >> 24) as u8;
    let len = (header & 0x00FF_FFFF) as usize;
    if *offset + len > data.len() {
        return Err(AppError::InvalidResponse(format!(
            "truncated {label} block payload"
        )));
    }
    let payload = &data[*offset..*offset + len];
    *offset += len;
    Ok(Block { flags, payload })
}

fn parse_pascal_short_string(data: &[u8]) -> String {
    if data.is_empty() {
        return String::new();
    }
    let len = data[0] as usize;
    let end = (1 + len).min(data.len());
    decode_single_byte_text(&data[1..end])
}

fn decode_single_byte_text(data: &[u8]) -> String {
    data.iter().map(|byte| char::from(*byte)).collect()
}

fn temp_path(prefix: &str, extension: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}_{timestamp}.{extension}"))
}

fn collect_sections<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut used = HashSet::new();
    names
        .map(|name| unique_section_key(name, &mut used))
        .collect()
}

fn collect_section_key_pairs<'a>(
    names: impl Iterator<Item = &'a str>,
    sections: &[String],
) -> Vec<(String, String)> {
    names
        .zip(sections.iter())
        .filter_map(|(name, section_key)| {
            (section_key.as_str() != name).then(|| (name.to_string(), section_key.clone()))
        })
        .collect()
}

fn unique_section_key(name: &str, used: &mut HashSet<String>) -> String {
    let base = section_key_from_name(name);
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

fn section_key_from_name(name: &str) -> String {
    let mut key = String::new();
    for character in name.trim().chars() {
        if key.len() >= 31 {
            break;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
            key.push(character);
        } else {
            key.push('_');
        }
    }
    if key.is_empty() { "_".to_string() } else { key }
}

fn legacy_section_key_from_name(name: &str) -> String {
    if name.is_empty() {
        return "_".to_string();
    }
    name.chars()
        .take(31)
        .map(|character| if character == '/' { '_' } else { character })
        .collect()
}

fn section_key_candidates(name: &str, explicit: Option<&str>, derived: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(explicit) = explicit.filter(|value| !value.trim().is_empty()) {
        push_unique_section_key(&mut candidates, explicit.to_string());
    }
    push_unique_section_key(&mut candidates, derived.to_string());
    push_unique_section_key(&mut candidates, legacy_section_key_from_name(name));
    push_unique_section_key(&mut candidates, name.to_string());
    candidates
}

fn push_unique_section_key(candidates: &mut Vec<String>, value: String) {
    if value.is_empty() || candidates.iter().any(|existing| existing == &value) {
        return;
    }
    candidates.push(value);
}

fn write_stream(compound: &mut CompoundFile<File>, path: &str, data: &[u8]) -> std::io::Result<()> {
    let mut stream = compound.create_stream(path)?;
    stream.write_all(data)
}

fn schlib_file_header_bytes(records: &[SchlibRecord]) -> Vec<u8> {
    let mut writer = SchWriter::default();
    let mut params = SchParams::default();
    params.push(
        "HEADER",
        "Protel for Windows - Schematic Library Editor Binary File Version 5.0",
    );
    params.push(
        "WEIGHT",
        records
            .iter()
            .map(|record| record.weight)
            .sum::<usize>()
            .to_string(),
    );
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
    params.push("COMPCOUNT", records.len().to_string());
    for (index, record) in records.iter().enumerate() {
        params.push(format!("LIBREF{index}"), &record.name);
        params.push(format!("COMPDESCR{index}"), &record.description);
        params.push(
            format!("PARTCOUNT{index}"),
            record.header_part_count.to_string(),
        );
    }
    writer.write_cstring_param_block(&params);
    writer.write_i32(records.len() as i32);
    for record in records {
        writer.write_string_block(&record.name);
    }
    writer.into_inner()
}

fn schlib_section_keys_bytes(pairs: &[(String, String)]) -> Vec<u8> {
    let mut writer = SchWriter::default();
    let mut params = SchParams::default();
    params.push("KeyCount", pairs.len().to_string());
    for (index, (name, key)) in pairs.iter().enumerate() {
        params.push(format!("LibRef{index}"), name);
        params.push(format!("SectionKey{index}"), key);
    }
    writer.write_cstring_param_block(&params);
    writer.into_inner()
}

fn schlib_storage_bytes() -> Vec<u8> {
    let mut writer = SchWriter::default();
    let mut params = SchParams::default();
    params.push("HEADER", "Icon storage");
    writer.write_cstring_param_block(&params);
    writer.into_inner()
}

fn pcblib_file_header_bytes() -> Vec<u8> {
    let mut writer = PcbWriter::default();
    let version = "PCB 6.0 Binary Library File";
    writer.write_i32(version.len() as i32);
    writer.write_pascal_short_string(version);
    writer.into_inner()
}

fn pcblib_section_keys_bytes(pairs: &[(String, String)]) -> Vec<u8> {
    let mut writer = PcbWriter::default();
    writer.write_i32(pairs.len() as i32);
    for (name, key) in pairs {
        writer.write_pascal_string(name);
        writer.write_string_block(key);
    }
    writer.into_inner()
}

fn storage_header_bytes(record_count: i32) -> Vec<u8> {
    record_count.to_le_bytes().to_vec()
}

fn pcblib_library_data_bytes(library: &PcblibRecordLibrary, output_path: &Path) -> Vec<u8> {
    let mut writer = PcbWriter::default();
    writer.write_block(0, |inner| {
        inner.write_cstring(&pcblib_library_data_params(output_path))
    });
    writer.write_u32(library.components.len() as u32);
    for component in &library.components {
        writer.write_string_block(&component.name);
    }
    writer.into_inner()
}

fn pcblib_library_data_params(output_path: &Path) -> String {
    let mut filename = output_path
        .canonicalize()
        .unwrap_or_else(|_| output_path.to_path_buf())
        .to_string_lossy()
        .replace('/', "\\");
    if let Some(stripped) = filename.strip_prefix("\\\\?\\") {
        filename = stripped.to_string();
    }
    let (date_text, time_text) = current_library_date_time();
    PCBLIB_LIBRARY_DATA_TEMPLATE
        .replace("__FILE__", &filename)
        .replace("__DATE__", &date_text)
        .replace("__TIME__", &time_text)
}

fn current_library_date_time() -> (String, String) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour24 = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let (hour12, suffix) = match hour24 {
        0 => (12, "AM"),
        1..=11 => (hour24, "AM"),
        12 => (12, "PM"),
        _ => (hour24 - 12, "PM"),
    };
    (
        format!("{month}/{day}/{year}"),
        format!("{hour12}:{minute:02}:{second:02} {suffix}"),
    )
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

fn pcblib_models_data_bytes(models: &[PcblibModelRecord]) -> Vec<u8> {
    let mut writer = PcbWriter::default();
    for model in models {
        writer.write_i32(model.entry.len() as i32);
        writer.write_bytes(&model.entry);
    }
    writer.into_inner()
}

#[derive(Debug, Default)]
struct SchParams(Vec<(String, String)>);

impl SchParams {
    fn push(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.push((key.into(), value.into()));
    }

    fn as_string(&self) -> String {
        let mut text = String::new();
        for (key, value) in &self.0 {
            text.push('|');
            text.push_str(key);
            text.push('=');
            text.push_str(value);
            if requires_utf8_parameter(value) {
                text.push('|');
                text.push_str("%UTF8%");
                text.push_str(key);
                text.push('=');
                text.push_str(&encode_utf8_parameter_value(value));
            }
        }
        text
    }
}

#[derive(Debug, Default)]
struct SchWriter {
    data: Vec<u8>,
}

impl SchWriter {
    fn into_inner(self) -> Vec<u8> {
        self.data
    }

    fn write_u8(&mut self, value: u8) {
        self.data.push(value);
    }

    fn write_i32(&mut self, value: i32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    fn write_u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    fn write_pascal_short_string(&mut self, value: &str) {
        let bytes = sch_encode_ansi_lossy(value);
        let len = bytes.len().min(255);
        self.write_u8(len as u8);
        self.data.extend_from_slice(&bytes[..len]);
    }

    fn write_cstring(&mut self, value: &str) {
        self.data.extend_from_slice(&sch_encode_ansi_lossy(value));
        self.write_u8(0);
    }

    fn write_block(&mut self, flags: u8, serializer: impl FnOnce(&mut Self)) {
        let mut child = Self::default();
        serializer(&mut child);
        let child_data = child.into_inner();
        self.write_u32(((flags as u32) << 24) | child_data.len() as u32);
        self.data.extend_from_slice(&child_data);
    }

    fn write_string_block(&mut self, value: &str) {
        self.write_block(0, |writer| writer.write_pascal_short_string(value));
    }

    fn write_cstring_param_block(&mut self, params: &SchParams) {
        let text = params.as_string();
        self.write_block(0, |writer| writer.write_cstring(&text));
    }
}

fn sch_encode_ansi_lossy(text: &str) -> Vec<u8> {
    let sanitized = text.replace('\0', "?");
    let (bytes, _, _) = WINDOWS_1252.encode(&sanitized);
    bytes.into_owned()
}

#[derive(Debug, Default)]
struct PcbWriter {
    data: Vec<u8>,
}

impl PcbWriter {
    fn into_inner(self) -> Vec<u8> {
        self.data
    }

    fn write_u8(&mut self, value: u8) {
        self.data.push(value);
    }

    fn write_i32(&mut self, value: i32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    fn write_u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    fn write_bytes(&mut self, value: &[u8]) {
        self.data.extend_from_slice(value);
    }

    fn write_block(&mut self, flags: u8, serializer: impl FnOnce(&mut Self)) {
        let mut child = Self::default();
        serializer(&mut child);
        let child_data = child.into_inner();
        self.write_u32(((flags as u32) << 24) | child_data.len() as u32);
        self.write_bytes(&child_data);
    }

    fn write_pascal_short_string(&mut self, value: &str) {
        let bytes = pcb_encode_ansi_lossy(value);
        let len = bytes.len().min(255);
        self.write_u8(len as u8);
        self.write_bytes(&bytes[..len]);
    }

    fn write_cstring(&mut self, value: &str) {
        self.write_bytes(&pcb_encode_ansi_lossy(value));
        self.write_u8(0);
    }

    fn write_string_block(&mut self, value: &str) {
        self.write_block(0, |writer| writer.write_pascal_short_string(value));
    }

    fn write_pascal_string(&mut self, value: &str) {
        self.write_block(0, |writer| {
            writer.write_pascal_short_string(value);
            writer.write_u8(0);
        });
    }
}

fn pcb_encode_ansi_lossy(text: &str) -> Vec<u8> {
    let sanitized = text.replace('\0', "?");
    let (bytes, _, _) = WINDOWS_1252.encode(&sanitized);
    bytes.into_owned()
}

fn requires_utf8_parameter(text: &str) -> bool {
    let (_, _, had_errors) = WINDOWS_1252.encode(text);
    had_errors
}

fn encode_utf8_parameter_value(text: &str) -> String {
    let bytes = text.as_bytes();
    let (value, _, _) = WINDOWS_1252.decode(bytes);
    value.into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_lcsc_id, pcblib_records_from_library, read_pcblib_records, read_schlib_records,
        schlib_file_header_bytes, schlib_record_from_component, schlib_storage_bytes, temp_path,
        write_pcblib_records, write_schlib_records, write_stream,
    };
    use crate::footprint::build_pcblib_from_payload;
    use crate::schlib::{
        SchlibMetadata, SchlibParameter, build_component_from_payload_with_metadata,
    };
    use serde_json::Value;
    use std::fs::{self, File};

    const STEP_FIXTURE: &[u8] =
        b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n";

    #[test]
    fn normalizes_lcsc_ids_case_insensitively() {
        assert_eq!(normalize_lcsc_id(" c2040 ").as_deref(), Some("C2040"));
        assert!(normalize_lcsc_id("RP2040").is_none());
    }

    #[test]
    fn schlib_records_round_trip_component_identity() {
        let payload: Value =
            serde_json::from_str(include_str!("../tests/fixtures/easyeda_symbol.json"))
                .expect("symbol fixture");
        let make_component = |name: &str, component_id: &str| {
            let metadata = SchlibMetadata {
                description: Some("Roundtrip component".to_string()),
                designator: Some("U?".to_string()),
                comment: Some(name.to_string()),
                parameters: vec![SchlibParameter {
                    name: "NPNP_COMPONENT_ID".to_string(),
                    value: component_id.to_string(),
                }],
                footprint_model_name: None,
                footprint_library_file: None,
                name_override: None,
            };
            build_component_from_payload_with_metadata(&payload, name, &metadata)
                .expect("build component")
        };

        let record_a = schlib_record_from_component(&make_component("COMP_A", "C2040"))
            .expect("capture SchLib record");
        let record_b = schlib_record_from_component(&make_component("COMP_B", "C42"))
            .expect("capture SchLib record");
        assert_eq!(record_a.identity.as_deref(), Some("C2040"));
        assert_eq!(record_b.identity.as_deref(), Some("C42"));

        let path = temp_path("npnp_merge_schlib_roundtrip", "SchLib");
        write_schlib_records(&[record_a, record_b], &path).expect("write merged SchLib");
        let records = read_schlib_records(&path).expect("read merged SchLib");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].name, "COMP_A");
        assert_eq!(records[0].identity.as_deref(), Some("C2040"));
        assert_eq!(records[1].name, "COMP_B");
        assert_eq!(records[1].identity.as_deref(), Some("C42"));
        fs::remove_file(path).ok();
    }

    #[test]
    fn schlib_records_round_trip_non_ascii_component_name() {
        let payload: Value =
            serde_json::from_str(include_str!("../tests/fixtures/easyeda_symbol.json"))
                .expect("symbol fixture");
        let non_ascii_name = "SMD7525-32\u{03A9}";
        let metadata = SchlibMetadata {
            description: Some("Non ASCII component".to_string()),
            designator: Some("BUZZER?".to_string()),
            comment: Some(non_ascii_name.to_string()),
            parameters: vec![SchlibParameter {
                name: "NPNP_COMPONENT_ID".to_string(),
                value: "C50387083".to_string(),
            }],
            footprint_model_name: None,
            footprint_library_file: None,
            name_override: None,
        };
        let ascii_metadata = SchlibMetadata {
            description: Some("ASCII component".to_string()),
            designator: Some("U?".to_string()),
            comment: Some("COMP_A".to_string()),
            parameters: vec![SchlibParameter {
                name: "NPNP_COMPONENT_ID".to_string(),
                value: "C2040".to_string(),
            }],
            footprint_model_name: None,
            footprint_library_file: None,
            name_override: None,
        };
        let ascii_component =
            build_component_from_payload_with_metadata(&payload, "COMP_A", &ascii_metadata)
                .expect("build ASCII component");
        let component =
            build_component_from_payload_with_metadata(&payload, non_ascii_name, &metadata)
                .expect("build component");

        let record = schlib_record_from_component(&component).expect("capture SchLib record");
        assert_eq!(record.name, non_ascii_name);
        assert_eq!(record.identity.as_deref(), Some("C50387083"));
        let ascii_record =
            schlib_record_from_component(&ascii_component).expect("capture ASCII SchLib record");

        let path = temp_path("npnp_merge_schlib_non_ascii", "SchLib");
        write_schlib_records(&[ascii_record, record], &path).expect("write merged SchLib");
        let records = read_schlib_records(&path).expect("read merged SchLib");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].name, "COMP_A");
        assert_eq!(records[0].identity.as_deref(), Some("C2040"));
        assert_eq!(records[1].name, non_ascii_name);
        assert_eq!(records[1].identity.as_deref(), Some("C50387083"));
        fs::remove_file(path).ok();
    }

    #[test]
    fn schlib_records_read_legacy_space_section_name_without_section_keys() {
        let payload: Value =
            serde_json::from_str(include_str!("../tests/fixtures/easyeda_symbol.json"))
                .expect("symbol fixture");
        let name = "0.5-8P CTSJ-H2.0 119";
        let metadata = SchlibMetadata {
            description: Some("Legacy space section".to_string()),
            designator: Some("J?".to_string()),
            comment: Some(name.to_string()),
            parameters: vec![SchlibParameter {
                name: "NPNP_COMPONENT_ID".to_string(),
                value: "C424242".to_string(),
            }],
            footprint_model_name: None,
            footprint_library_file: None,
            name_override: None,
        };
        let component = build_component_from_payload_with_metadata(&payload, name, &metadata)
            .expect("build component");
        let record = schlib_record_from_component(&component).expect("capture SchLib record");

        let path = temp_path("npnp_merge_schlib_legacy_space", "SchLib");
        let file = File::create(&path).expect("create legacy SchLib");
        let mut compound = cfb::CompoundFile::create(file).expect("create compound");
        write_stream(
            &mut compound,
            "/FileHeader",
            &schlib_file_header_bytes(std::slice::from_ref(&record)),
        )
        .expect("write header");
        compound
            .create_storage(&format!("/{name}/"))
            .expect("create legacy storage");
        write_stream(&mut compound, &format!("/{name}/Data"), &record.data).expect("write data");
        write_stream(&mut compound, "/Storage", &schlib_storage_bytes()).expect("write storage");
        compound.flush().expect("flush compound");
        drop(compound);

        let records = read_schlib_records(&path).expect("read legacy SchLib");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, name);
        assert_eq!(records[0].identity.as_deref(), Some("C424242"));
        fs::remove_file(path).ok();
    }

    #[test]
    fn pcblib_records_round_trip_component_streams() {
        let payload: Value =
            serde_json::from_str(include_str!("../tests/fixtures/easyeda_footprint.json"))
                .expect("footprint fixture");
        let library =
            build_pcblib_from_payload(&payload, "ROUNDTRIP_FOOTPRINT", Some(STEP_FIXTURE))
                .expect("build footprint library");
        let records = pcblib_records_from_library(&library).expect("capture PcbLib records");
        assert_eq!(records.components.len(), 1);
        assert_eq!(records.models.len(), 1);

        let path = temp_path("npnp_merge_pcblib_roundtrip", "PcbLib");
        write_pcblib_records(&records, &path).expect("write merged PcbLib");
        let round_trip = read_pcblib_records(&path).expect("read merged PcbLib");
        assert_eq!(round_trip.components.len(), 1);
        assert_eq!(round_trip.components[0].name, "ROUNDTRIP_FOOTPRINT");
        assert_eq!(round_trip.models.len(), 1);
        fs::remove_file(path).ok();
    }
}
