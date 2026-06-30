use std::collections::{HashMap, HashSet};

use encoding_rs::WINDOWS_1252;

use crate::error::{AppError, Result};

// ── Binary block helpers ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub(crate) struct Block<'a> {
    pub(crate) flags: u8,
    pub(crate) payload: &'a [u8],
}

pub(crate) fn parse_block_stream<'a>(data: &'a [u8], label: &str) -> Result<Vec<Block<'a>>> {
    let mut offset = 0usize;
    let mut blocks = Vec::new();
    while offset < data.len() {
        blocks.push(read_block(data, &mut offset, label)?);
    }
    Ok(blocks)
}

pub(crate) fn read_block<'a>(data: &'a [u8], offset: &mut usize, label: &str) -> Result<Block<'a>> {
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

pub(crate) fn parse_param_pairs(text: &str) -> Vec<(String, String)> {
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

pub(crate) fn param_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .rev()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_str())
}

pub(crate) fn schlib_cstring_text(data: &[u8]) -> String {
    let len = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(data.len());
    let (text, _, _) = WINDOWS_1252.decode(&data[..len]);
    text.into_owned()
}

pub(crate) fn decode_utf8_parameter_value(text: &str) -> String {
    let (bytes, _, _) = WINDOWS_1252.encode(text);
    String::from_utf8_lossy(&bytes).into_owned()
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

// ── SchLib binary writers ─────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub(crate) struct SchParams(Vec<(String, String)>);

impl SchParams {
    pub(crate) fn push(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.push((key.into(), value.into()));
    }

    pub(crate) fn as_string(&self) -> String {
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
pub(crate) struct SchWriter {
    data: Vec<u8>,
}

impl SchWriter {
    pub(crate) fn into_inner(self) -> Vec<u8> {
        self.data
    }

    pub(crate) fn write_u8(&mut self, value: u8) {
        self.data.push(value);
    }

    pub(crate) fn write_i32(&mut self, value: i32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn write_u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn write_pascal_short_string(&mut self, value: &str) {
        let bytes = sch_encode_ansi_lossy(value);
        let len = bytes.len().min(255);
        self.write_u8(len as u8);
        self.data.extend_from_slice(&bytes[..len]);
    }

    pub(crate) fn write_cstring(&mut self, value: &str) {
        self.data.extend_from_slice(&sch_encode_ansi_lossy(value));
        self.write_u8(0);
    }

    pub(crate) fn write_block(&mut self, flags: u8, serializer: impl FnOnce(&mut Self)) {
        let mut child = Self::default();
        serializer(&mut child);
        let child_data = child.into_inner();
        self.write_u32(((flags as u32) << 24) | child_data.len() as u32);
        self.data.extend_from_slice(&child_data);
    }

    pub(crate) fn write_string_block(&mut self, value: &str) {
        self.write_block(0, |writer| writer.write_pascal_short_string(value));
    }

    pub(crate) fn write_cstring_param_block(&mut self, params: &SchParams) {
        let text = params.as_string();
        self.write_block(0, |writer| writer.write_cstring(&text));
    }
}

pub(crate) fn sch_encode_ansi_lossy(text: &str) -> Vec<u8> {
    let sanitized = text.replace('\0', "?");
    let (bytes, _, _) = WINDOWS_1252.encode(&sanitized);
    bytes.into_owned()
}

// ── Visibility ────────────────────────────────────────────────────────────────

fn designator_prefix(designator: &str) -> String {
    designator
        .trim()
        .trim_end_matches(|c: char| c == '?' || c.is_ascii_digit())
        .trim()
        .to_ascii_uppercase()
}

pub(crate) fn is_default_visible_parameter(designator: &str, param_name: &str) -> bool {
    let d = designator_prefix(designator);
    let n = param_name.trim().to_ascii_lowercase();
    match d.as_str() {
        "R" | "RV" | "VR" => {
            n.contains("resistance")
                || n.contains("tolerance")
                || n.contains("power")
                || n.contains("package")
                || n.contains("case")
        }
        "C" | "CE" | "CV" => {
            n.contains("capacitance")
                || n.contains("tolerance")
                || n.contains("voltage")
                || n.contains("temperature")
                || n.contains("package")
                || n.contains("case")
        }
        "L" | "FB" => {
            n.contains("inductance")
                || n.contains("tolerance")
                || n.contains("current")
                || n.contains("package")
                || n.contains("case")
        }
        "X" | "Y" | "XTAL" => {
            n.contains("frequency")
                || n.contains("package")
                || n.contains("case")
                || n.contains("tolerance")
        }
        _ => false,
    }
}

// ── SchLib param patching ─────────────────────────────────────────────────────

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

        let mut deduped: Vec<(String, String)> = Vec::new();
        for (k, v) in &pairs {
            let key_lc = k.to_ascii_lowercase();
            if let Some(pos) = deduped.iter().position(|(ek, _)| ek.to_ascii_lowercase() == key_lc) {
                deduped[pos].1 = v.clone();
            } else {
                deduped.push((k.clone(), v.clone()));
            }
        }

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
            if let Some(&new_slot_name) = renames_map.get(lc.as_str()) {
                found_keys.insert(new_slot_name.to_ascii_lowercase());
                found_keys.insert(lc);
            }
        }
    }

    let mut missing: Vec<(&str, &str)> = replacements
        .iter()
        .filter(|(k, _)| {
            let lc = k.to_ascii_lowercase();
            !found_keys.contains(&lc)
        })
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    if !found_keys.contains("comment")
        && !replacements.keys().any(|k| k.eq_ignore_ascii_case("Comment"))
    {
        missing.push(("Comment", ""));
    }
    missing.sort_by_key(|(k, _)| *k);

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

        if record_value.is_some_and(|v| matches!(v, "45" | "46" | "47")) {
            continue;
        }

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
            if is_record_44 {
                if let Some((model_name, library_file)) = footprint {
                    write_footprint_impl_blocks(
                        &mut out,
                        model_name,
                        library_file,
                        pin_designators,
                        new_name,
                    );
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
        if is_component_header {
            if let Some(desc) = new_description {
                if !desc_was_set && !desc.is_empty() {
                    deduped.push(("COMPONENTDESCRIPTION".to_string(), desc.to_string()));
                }
            }
        }
        if let Some(new_val) = replacement_value {
            if !text_was_set && !new_val.is_empty() {
                deduped.push(("TEXT".to_string(), new_val.to_string()));
            }
        }
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

    if !inserted_missing && !missing.is_empty() {
        write_new_param_blocks(&mut out, &missing, new_name, designator);
    }

    out
}

fn write_new_param_blocks(
    out: &mut Vec<u8>,
    params: &[(&str, &str)],
    component_name: &str,
    designator: &str,
) {
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
    let mut p = SchParams::default();
    p.push("RECORD", "45");
    p.push("DESCRIPTION", "PCB footprint");
    p.push("MODELNAME", model_name);
    p.push("MODELTYPE", "PCBLIB");
    p.push("DATAFILECOUNT", "1");
    p.push("MODELDATAFILEKIND1", "PCBLib");
    p.push("MODELDATAFILEENTITY1", library_file);
    p.push("ISCURRENT", "T");
    p.push(
        "UNIQUEID",
        schlib_stable_unique_id(component_name, &format!("IMPL0_{model_name}")),
    );
    let mut w = SchWriter::default();
    w.write_block(0, |wr| wr.write_cstring(&p.as_string()));
    out.extend_from_slice(&w.into_inner());

    let mut p = SchParams::default();
    p.push("RECORD", "46");
    let mut w = SchWriter::default();
    w.write_block(0, |wr| wr.write_cstring(&p.as_string()));
    out.extend_from_slice(&w.into_inner());

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

fn matches_param_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "npnp_component_id" | "supplier part" | "supplier part number" | "lcsc id"
    )
}
