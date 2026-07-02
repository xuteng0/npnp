use std::collections::HashMap;

use serde_json::Value;

use crate::error::Result;
use crate::footprint::build_pcblib_from_payload;
use crate::lceda::{LcedaClient, SearchItem};
use crate::lcsc::LcscProduct;
use crate::naming::build_passive_component_name;
use crate::pcblib::PcbLibrary;
use crate::schlib::{
    Component, SchlibMetadata, SchlibParameter, build_component_from_payload_with_metadata,
};
use crate::template::{classify_component, extract_package_size, standard_footprint_name, ComponentClass};
use crate::util::{nested_string, value_to_string};

// ── Name resolution ───────────────────────────────────────────────────────────

pub(crate) fn resolved_symbol_component_name(item: &SearchItem, payload: &Value) -> String {
    first_non_empty([
        (!item.display_name().trim().is_empty()).then(|| item.display_name().to_string()),
        nested_string(payload, &["result", "display_title"]),
        nested_string(payload, &["display_title"]),
        nested_string(payload, &["result", "title"]),
        nested_string(payload, &["title"]),
    ])
    .unwrap_or_else(|| "component".to_string())
}

/// Detect template footprint context: returns `(class, package, std_name)`.
pub(crate) fn detect_template_footprint(
    item: &SearchItem,
    footprint_name: &str,
) -> Option<(ComponentClass, String, String)> {
    let designator =
        nested_string(&item.raw, &["attributes", "Designator"]).unwrap_or_default();
    let class = classify_component(&designator);
    if class == ComponentClass::Other {
        return None;
    }
    let package = extract_package_size(footprint_name)?;
    let std_name = standard_footprint_name(class, &package);
    Some((class, package, std_name))
}

pub(crate) fn resolved_footprint_name(item: &SearchItem, payload: &Value) -> String {
    first_non_empty([
        nested_string(payload, &["result", "display_title"]),
        nested_string(payload, &["display_title"]),
        nested_string(payload, &["result", "package"]),
        nested_string(payload, &["package"]),
        nested_string(&item.raw, &["footprint", "display_title"]),
        nested_string(&item.raw, &["attributes", "Supplier Footprint"]),
        nested_string(&item.raw, &["attributes", "Footprint"]),
        (!item.display_name().trim().is_empty()).then(|| item.display_name().to_string()),
    ])
    .unwrap_or_else(|| "footprint".to_string())
}

/// Extract a footprint name from item attributes without fetching footprint detail.
pub(crate) fn footprint_name_from_item(item: &SearchItem) -> String {
    resolved_footprint_name(item, &serde_json::Value::Null)
}

// ── Replacement map builders ──────────────────────────────────────────────────

/// Minimal replacements map with only LCSC link parameters.
/// Used when full LCSC English metadata is unavailable.
pub(crate) fn build_link_replacements_from_lcsc_id(lcsc_id: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let supplier_link = format!("https://www.lcsc.com/search?q={lcsc_id}");
    map.insert("Supplier Link".to_string(), supplier_link.clone());
    map.insert("ComponentLink2URL".to_string(), supplier_link);
    map.insert("ComponentLink2Description".to_string(), "Supplier Link".to_string());
    map
}

pub(crate) fn build_schlib_replacements_from_lcsc(
    product: &LcscProduct,
    footprint_name: &str,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if !footprint_name.is_empty() {
        map.insert("Footprint".to_string(), footprint_name.to_string());
    }
    map.insert("Supplier".to_string(), "LCSC".to_string());
    map.insert("Supplier Part Number".to_string(), product.sku.clone());
    if let Some(mpn) = &product.mpn {
        map.insert("MPN".to_string(), mpn.clone());
    }
    if let Some(mfr) = &product.manufacturer {
        map.insert("Manufacturer".to_string(), mfr.clone());
    }
    if let Some(desc) = &product.description {
        map.insert("LCSC Part Name".to_string(), desc.clone());
    }
    if let Some(cat) = product.effective_category() {
        map.insert("Category".to_string(), cat.to_string());
    }
    // Altium renders a "Links" section from ComponentLink{n}Description + ComponentLink{n}URL
    // pairs. Both must be present — a lone URL param without its Description is ignored.
    // Components whose EasyEDA template already defines these slots get the URL patched in;
    // components with no pre-existing slots get new RECORD=41 param blocks written.
    if let Some(ds) = &product.datasheet_url {
        map.insert("Datasheet".to_string(), ds.clone());
        map.insert("ComponentLink1URL".to_string(), ds.clone());
        map.insert("ComponentLink1Description".to_string(), "Datasheet".to_string());
    }
    let supplier_link = format!("https://www.lcsc.com/search?q={}", product.sku);
    map.insert("Supplier Link".to_string(), supplier_link.clone());
    map.insert("ComponentLink2URL".to_string(), supplier_link);
    map.insert("ComponentLink2Description".to_string(), "Supplier Link".to_string());
    for prop in &product.properties {
        map.insert(prop.name.clone(), prop.value.clone());
    }
    map
}

// ── Component builders ────────────────────────────────────────────────────────

pub(crate) async fn load_step_bytes_for_pcblib(
    client: &LcedaClient,
    item: &SearchItem,
    footprint_data: &Value,
) -> Option<Vec<u8>> {
    let mut model_candidates = Vec::new();
    if let Some(model_uuid) = nested_string(footprint_data, &["result", "model_3d", "uri"])
        .filter(|uuid| !uuid.trim().is_empty())
    {
        model_candidates.push(model_uuid);
    }
    if let Some(model_uuid) = item
        .model_uuid
        .clone()
        .filter(|uuid| !uuid.trim().is_empty())
    {
        if !model_candidates
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&model_uuid))
        {
            model_candidates.push(model_uuid);
        }
    }
    if item.model_uuid.is_some() {
        if let Ok(model_uuid) = client.get_model_uuid(item).await {
            if !model_candidates
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&model_uuid))
            {
                model_candidates.push(model_uuid);
            }
        }
    }

    for model_uuid in model_candidates {
        if let Ok(bytes) = client.download_step_bytes(&model_uuid).await {
            return Some(bytes);
        }
    }
    None
}

pub(crate) async fn build_pcblib_library_from_detail(
    client: &LcedaClient,
    item: &SearchItem,
    footprint_data: &Value,
    component_name: &str,
) -> Result<PcbLibrary> {
    let step_bytes = load_step_bytes_for_pcblib(client, item, footprint_data).await;
    build_pcblib_from_payload(footprint_data, component_name, step_bytes.as_deref())
}

pub(crate) fn build_schlib_component_from_detail(
    item: &SearchItem,
    symbol_data: &Value,
    component_name: &str,
    footprint_model_name: Option<&str>,
    footprint_library_file: Option<&str>,
    english_metadata: Option<&LcscProduct>,
) -> Result<Component> {
    let metadata = build_schlib_metadata(
        item,
        symbol_data,
        footprint_model_name,
        footprint_library_file,
        english_metadata,
    );
    let effective_name = metadata
        .name_override
        .as_deref()
        .unwrap_or(component_name);
    build_component_from_payload_with_metadata(symbol_data, effective_name, &metadata)
}

pub async fn build_pcblib_library_for_item(
    client: &LcedaClient,
    item: &SearchItem,
    footprint_name: &str,
) -> Result<PcbLibrary> {
    let footprint_uuid = item
        .footprint_uuid()
        .ok_or(crate::error::AppError::MissingSymbolOrFootprint)?;
    let footprint_data = client.component_detail(&footprint_uuid).await?;
    build_pcblib_library_from_detail(client, item, &footprint_data, footprint_name).await
}

pub async fn build_schlib_component_for_item(
    client: &LcedaClient,
    item: &SearchItem,
    component_name: &str,
    footprint_model_name: Option<&str>,
    footprint_library_file: Option<&str>,
) -> Result<Component> {
    build_schlib_component_for_item_with_metadata(
        client,
        item,
        component_name,
        footprint_model_name,
        footprint_library_file,
        None,
    )
    .await
}

pub async fn build_schlib_component_for_item_with_metadata(
    client: &LcedaClient,
    item: &SearchItem,
    component_name: &str,
    footprint_model_name: Option<&str>,
    footprint_library_file: Option<&str>,
    english_metadata: Option<&LcscProduct>,
) -> Result<Component> {
    let symbol_uuid = item
        .symbol_uuid()
        .ok_or(crate::error::AppError::MissingSymbolOrFootprint)?;
    let symbol_data = client.component_detail(&symbol_uuid).await?;
    build_schlib_component_from_detail(
        item,
        &symbol_data,
        component_name,
        footprint_model_name,
        footprint_library_file,
        english_metadata,
    )
}

// ── SchLib metadata building ──────────────────────────────────────────────────

fn build_schlib_metadata(
    item: &SearchItem,
    symbol_data: &Value,
    footprint_model_name: Option<&str>,
    footprint_library_file: Option<&str>,
    english_metadata: Option<&LcscProduct>,
) -> SchlibMetadata {
    let mut parameters = Vec::new();
    let mut seen_names = std::collections::HashSet::new();
    let resolved_footprint = footprint_model_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            first_non_empty([
                nested_string(&item.raw, &["footprint", "display_title"]),
                nested_string(&item.raw, &["attributes", "Supplier Footprint"]),
            ])
        });

    if let Some(footprint_name) = resolved_footprint.as_deref() {
        push_schlib_parameter(&mut parameters, &mut seen_names, "Footprint", footprint_name);
    }

    if let Some(product) = english_metadata {
        push_lcsc_english_parameters(&mut parameters, &mut seen_names, product);
    } else if let Some(attributes) = item.raw.get("attributes").and_then(Value::as_object) {
        for (name, value) in attributes {
            let Some(value) = value_to_string(value) else {
                continue;
            };
            if value.trim().is_empty() || value.trim() == "-" {
                continue;
            }
            push_schlib_parameter(&mut parameters, &mut seen_names, name, value);
        }
    }

    let designator_raw = first_non_empty([nested_string(&item.raw, &["attributes", "Designator"])]);
    let name_override = build_passive_component_name(
        designator_raw.as_deref().unwrap_or(""),
        &parameters,
        item.lcsc_id().as_deref(),
    );

    SchlibMetadata {
        description: english_metadata
            .and_then(|product| product.description.clone())
            .or_else(|| {
                first_non_empty([
                    nested_string(&item.raw, &["description"]),
                    nested_string(symbol_data, &["result", "description"]),
                    nested_string(&item.raw, &["attributes", "LCSC Part Name"]),
                    nested_string(&item.raw, &["attributes", "Manufacturer Part"]),
                ])
            }),
        designator: designator_raw,
        comment: english_metadata
            .and_then(|product| product.mpn.clone())
            .or_else(|| resolve_schlib_comment(item)),
        parameters,
        footprint_model_name: resolved_footprint,
        footprint_library_file: footprint_library_file
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        name_override,
    }
}

fn push_lcsc_english_parameters(
    parameters: &mut Vec<SchlibParameter>,
    seen_names: &mut std::collections::HashSet<String>,
    product: &LcscProduct,
) {
    push_schlib_parameter(parameters, seen_names, "Supplier", "LCSC");
    push_schlib_parameter(parameters, seen_names, "Supplier Part Number", product.sku.clone());
    if let Some(mpn) = product.mpn.as_deref() {
        push_schlib_parameter(parameters, seen_names, "MPN", mpn);
    }
    if let Some(manufacturer) = product.manufacturer.as_deref() {
        push_schlib_parameter(parameters, seen_names, "Manufacturer", manufacturer);
    }
    if let Some(description) = product.description.as_deref() {
        push_schlib_parameter(parameters, seen_names, "LCSC Part Name", description);
    }
    if let Some(category) = product.effective_category() {
        push_schlib_parameter(parameters, seen_names, "Category", category);
    }
    if let Some(datasheet_url) = product.datasheet_url.as_deref() {
        push_schlib_parameter(parameters, seen_names, "Datasheet", datasheet_url);
        push_schlib_parameter(parameters, seen_names, "ComponentLink1Description", "Datasheet");
        push_schlib_parameter(parameters, seen_names, "ComponentLink1URL", datasheet_url);
    }
    let supplier_link = format!("https://www.lcsc.com/search?q={}", product.sku);
    push_schlib_parameter(parameters, seen_names, "Supplier Link", supplier_link.clone());
    push_schlib_parameter(parameters, seen_names, "ComponentLink2Description", "Supplier Link");
    push_schlib_parameter(parameters, seen_names, "ComponentLink2URL", supplier_link);
    for property in &product.properties {
        push_schlib_parameter(parameters, seen_names, &property.name, &property.value);
    }
}

fn resolve_schlib_comment(item: &SearchItem) -> Option<String> {
    let attributes = item.raw.get("attributes").and_then(Value::as_object);

    if let Some(name) = find_attribute_value_case_insensitive(attributes, "Name") {
        if let Some(resolved) = resolve_attribute_formula(&name, attributes) {
            return Some(resolved);
        }
        if extract_formula_field(&name).is_none() {
            return Some(name);
        }
    }

    first_non_empty([
        find_attribute_value_case_insensitive(attributes, "Manufacturer Part"),
        find_attribute_value_case_insensitive(attributes, "Value"),
        find_attribute_value_case_insensitive(attributes, "LCSC Part Name"),
        Some(item.display_name().to_string()),
    ])
}

fn resolve_attribute_formula(
    text: &str,
    attributes: Option<&serde_json::Map<String, Value>>,
) -> Option<String> {
    let field = extract_formula_field(text)?;
    find_attribute_value_case_insensitive(attributes, &field)
}

fn extract_formula_field(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let rhs = trimmed.strip_prefix('=')?.trim();
    let field = rhs.strip_prefix('{')?.strip_suffix('}')?.trim();
    if field.is_empty() {
        None
    } else {
        Some(field.to_string())
    }
}

fn find_attribute_value_case_insensitive(
    attributes: Option<&serde_json::Map<String, Value>>,
    name: &str,
) -> Option<String> {
    let attributes = attributes?;
    attributes
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value_to_string(value))
        .filter(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty() && trimmed != "-"
        })
}

fn first_non_empty<const N: usize>(candidates: [Option<String>; N]) -> Option<String> {
    candidates
        .into_iter()
        .flatten()
        .find(|value| !value.trim().is_empty())
}

fn push_schlib_parameter(
    parameters: &mut Vec<SchlibParameter>,
    seen_names: &mut std::collections::HashSet<String>,
    name: impl Into<String>,
    value: impl Into<String>,
) {
    let name = name.into();
    let value = value.into();
    let normalized = name.trim().to_ascii_lowercase();
    if normalized.is_empty() || !seen_names.insert(normalized) {
        return;
    }
    if should_skip_schlib_parameter(&name) {
        return;
    }
    parameters.push(SchlibParameter { name, value });
}

fn should_skip_schlib_parameter(name: &str) -> bool {
    const SKIP: [&str; 11] = [
        "Add into BOM",
        "Convert to PCB",
        "Symbol",
        "Designator",
        "Footprint",
        "3D Model",
        "3D Model Title",
        "3D Model Transform",
        "Name",
        "LCSC Part",
        "NPNP_COMPONENT_ID",
    ];
    SKIP.iter().any(|item| item.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_item(raw: serde_json::Value) -> SearchItem {
        SearchItem {
            index: 0,
            display_title: "TEST".to_string(),
            title: String::new(),
            manufacturer: String::new(),
            model_uuid: None,
            raw,
        }
    }

    #[test]
    fn resolves_manufacturer_part_formula_comment() {
        let item = make_item(json!({
            "attributes": {
                "Name": "={Manufacturer Part}",
                "Manufacturer Part": "RP2040"
            }
        }));
        assert_eq!(resolve_schlib_comment(&item).as_deref(), Some("RP2040"));
    }

    #[test]
    fn resolves_value_formula_comment() {
        let item = make_item(json!({
            "attributes": {
                "Name": "={Value}",
                "Value": "2.4kHz",
                "Manufacturer Part": "TMB12A05"
            }
        }));
        assert_eq!(resolve_schlib_comment(&item).as_deref(), Some("2.4kHz"));
    }

    #[test]
    fn falls_back_when_formula_cannot_be_resolved() {
        let item = make_item(json!({
            "attributes": {
                "Name": "={Missing Field}",
                "Manufacturer Part": "XC7Z020-2CLG400I"
            }
        }));
        assert_eq!(
            resolve_schlib_comment(&item).as_deref(),
            Some("XC7Z020-2CLG400I")
        );
    }

    #[test]
    fn resolves_footprint_name_from_payload_metadata() {
        let item = make_item(json!({
            "footprint": { "display_title": "UFQFPN-20_L3.0-W3.0-P0.50-TL" }
        }));
        let payload = json!({
            "result": {
                "display_title": "UFQFPN-20_L3.0-W3.0-P0.50-TL",
                "package": "UFQFPN-20"
            }
        });
        assert_eq!(
            resolved_footprint_name(&item, &payload),
            "UFQFPN-20_L3.0-W3.0-P0.50-TL"
        );
    }
}
