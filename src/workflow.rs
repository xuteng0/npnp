use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, to_string_pretty};

use crate::error::{AppError, Result};
use crate::footprint::build_pcblib_from_payload;
use crate::lceda::{LcedaClient, SearchItem};
use crate::lcsc::{LcscClient, LcscProduct};
use crate::passive_naming::build_passive_component_name;
use crate::merge::{patch_schlib_data_component_name, patch_schlib_data_with_params, schlib_record_from_component, write_pcblib_records, write_schlib_records, SCHLIB_PARAM_RENAMES};
use crate::pcblib::{PcbLibrary, write_pcblib};
use crate::schlib::{
    Component, SchlibMetadata, SchlibParameter, build_component_from_payload_with_metadata,
    write_schlib,
};
use crate::template::{
    build_ipc_pcblib, classify_component, extract_package_size, find_assets_dir,
    load_local_step, load_pcblib_template_records, load_schlib_template_record,
    standard_footprint_name, ComponentClass,
};
use crate::util::{nested_string, sanitize_filename, split_obj_and_mtl, value_to_string};

#[derive(Debug, Serialize)]
struct BundleManifest {
    component_name: String,
    manufacturer: String,
    search_index: usize,
    symbol_uuid: Option<String>,
    footprint_uuid: Option<String>,
    model_seed_uuid: Option<String>,
    model_resolved_uuid: Option<String>,
    symbol_file: Option<String>,
    footprint_file: Option<String>,
    step_file: Option<String>,
}

pub async fn download_step(
    client: &LcedaClient,
    item: &SearchItem,
    out_dir: &Path,
    force: bool,
) -> Result<PathBuf> {
    fs::create_dir_all(out_dir)?;
    let out_file = out_dir.join(item.choose_step_filename());
    if out_file.exists() && !force {
        return Ok(out_file);
    }

    let model_uuid = client.get_model_uuid(item).await?;
    let content = client.download_step_bytes(&model_uuid).await?;
    fs::write(&out_file, content)?;
    Ok(out_file)
}

pub async fn download_obj(
    client: &LcedaClient,
    item: &SearchItem,
    out_dir: &Path,
    force: bool,
) -> Result<(PathBuf, PathBuf)> {
    fs::create_dir_all(out_dir)?;
    let base_name = item.choose_obj_basename();
    let obj_file = out_dir.join(format!("{base_name}.obj"));
    let mtl_file = out_dir.join(format!("{base_name}.mtl"));

    if obj_file.exists() && mtl_file.exists() && !force {
        return Ok((obj_file, mtl_file));
    }

    let model_uuid = client.get_model_uuid(item).await?;
    let content = client.download_obj_bytes(&model_uuid).await?;
    let text = String::from_utf8_lossy(&content);
    let (obj_text, mtl_text) = split_obj_and_mtl(&text);
    let obj_with_header = format!("mtllib {base_name}.mtl\n{obj_text}");

    fs::write(&obj_file, obj_with_header)?;
    fs::write(&mtl_file, mtl_text)?;
    Ok((obj_file, mtl_file))
}

pub async fn export_easyeda_sources(
    client: &LcedaClient,
    item: &SearchItem,
    out_dir: &Path,
    force: bool,
) -> Result<BTreeMap<String, PathBuf>> {
    fs::create_dir_all(out_dir)?;

    let base = sanitize_filename(item.display_name());
    let symbol_uuid = item.symbol_uuid();
    let footprint_uuid = item.footprint_uuid();
    if symbol_uuid.is_none() && footprint_uuid.is_none() {
        return Err(AppError::MissingSymbolOrFootprint);
    }

    let mut exported = BTreeMap::new();

    if let Some(symbol_uuid) = symbol_uuid {
        let symbol_data = client.component_detail(&symbol_uuid).await?;
        let symbol_file = out_dir.join(format!("{base}_symbol_easyeda.json"));
        if force || !symbol_file.exists() {
            fs::write(&symbol_file, to_string_pretty(&symbol_data)?)?;
        }
        exported.insert("symbol".to_string(), symbol_file);
    }

    if let Some(footprint_uuid) = footprint_uuid {
        let footprint_data = client.component_detail(&footprint_uuid).await?;
        let footprint_file = out_dir.join(format!("{base}_footprint_easyeda.json"));
        if force || !footprint_file.exists() {
            fs::write(&footprint_file, to_string_pretty(&footprint_data)?)?;
        }
        exported.insert("footprint".to_string(), footprint_file);
    }

    Ok(exported)
}

fn resolved_symbol_component_name(item: &SearchItem, payload: &Value) -> String {
    first_non_empty([
        (!item.display_name().trim().is_empty()).then(|| item.display_name().to_string()),
        nested_string(payload, &["result", "display_title"]),
        nested_string(payload, &["display_title"]),
        nested_string(payload, &["result", "title"]),
        nested_string(payload, &["title"]),
    ])
    .unwrap_or_else(|| "component".to_string())
}

/// Detect template footprint context for a component: returns `(class, package, std_name)`.
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
/// Falls back to item display name if no footprint attributes are present.
pub(crate) fn footprint_name_from_item(item: &SearchItem) -> String {
    resolved_footprint_name(item, &serde_json::Value::Null)
}

/// Build a parameter replacement map from LCSC English metadata for patching a template record.
/// Minimal replacements map containing only the LCSC-derived link parameters.
/// Used when full LCSC English metadata is unavailable but links should still
/// always be present regardless of what slots the template provides.
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
    if let Some(ds) = &product.datasheet_url {
        map.insert("Datasheet".to_string(), ds.clone());
        map.insert("ComponentLink1URL".to_string(), ds.clone());
    }
    let supplier_link = format!("https://www.lcsc.com/search?q={}", product.sku);
    map.insert("Supplier Link".to_string(), supplier_link.clone());
    map.insert("ComponentLink2URL".to_string(), supplier_link);
    for prop in &product.properties {
        map.insert(prop.name.clone(), prop.value.clone());
    }
    map
}

pub(crate) async fn load_step_bytes_for_pcblib_pub(
    client: &LcedaClient,
    item: &SearchItem,
    footprint_data: &Value,
) -> Option<Vec<u8>> {
    load_step_bytes_for_pcblib(client, item, footprint_data).await
}

async fn load_step_bytes_for_pcblib(
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

async fn build_pcblib_library_from_detail(
    client: &LcedaClient,
    item: &SearchItem,
    footprint_data: &Value,
    component_name: &str,
) -> Result<PcbLibrary> {
    let step_bytes = load_step_bytes_for_pcblib(client, item, footprint_data).await;
    build_pcblib_from_payload(footprint_data, component_name, step_bytes.as_deref())
}

fn build_schlib_component_from_detail(
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
        .ok_or(AppError::MissingSymbolOrFootprint)?;
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
        .ok_or(AppError::MissingSymbolOrFootprint)?;
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
pub async fn export_pcblib(
    client: &LcedaClient,
    item: &SearchItem,
    out_dir: &Path,
    force: bool,
) -> Result<PathBuf> {
    export_pcblib_with_options(client, item, out_dir, force, false).await
}

pub async fn export_pcblib_with_options(
    client: &LcedaClient,
    item: &SearchItem,
    out_dir: &Path,
    force: bool,
    use_template: bool,
) -> Result<PathBuf> {
    fs::create_dir_all(out_dir)?;
    let footprint_uuid = item
        .footprint_uuid()
        .ok_or(AppError::MissingSymbolOrFootprint)?;
    let footprint_data = client.component_detail(&footprint_uuid).await?;
    let easyeda_footprint_name = resolved_footprint_name(item, &footprint_data);

    // Detect template context: classify component and extract package size.
    let template_ctx = if use_template {
        let designator =
            nested_string(&item.raw, &["attributes", "Designator"]).unwrap_or_default();
        let class = classify_component(&designator);
        if class != ComponentClass::Other {
            extract_package_size(&easyeda_footprint_name).map(|pkg| (class, pkg))
        } else {
            None
        }
    } else {
        None
    };

    if let Some((class, package)) = template_ctx {
        let std_name = standard_footprint_name(class, &package);
        let out_file = out_dir.join(format!("{}.PcbLib", sanitize_filename(&std_name)));
        if out_file.exists() && !force {
            return Ok(out_file);
        }
        // Prefer the actual template file from assets/ (e.g. assets/R0402.PcbLib).
        if let Some(assets_dir) = find_assets_dir() {
            if let Some(records) = load_pcblib_template_records(&assets_dir, class, &package) {
                write_pcblib_records(&records, &out_file)?;
                return Ok(out_file);
            }
        }
        // Fall back to a programmatically generated IPC-7351 footprint.
        let step_bytes = find_assets_dir()
            .and_then(|dir| load_local_step(&dir, class, &package));
        let step_bytes = if step_bytes.is_some() {
            step_bytes
        } else {
            load_step_bytes_for_pcblib(client, item, &footprint_data).await
        };
        let library = build_ipc_pcblib(class, &package, &std_name, step_bytes)?;
        write_pcblib(&library, &out_file)?;
        return Ok(out_file);
    }

    let library =
        build_pcblib_library_from_detail(client, item, &footprint_data, &easyeda_footprint_name)
            .await?;
    let out_file =
        out_dir.join(format!("{}.PcbLib", sanitize_filename(&easyeda_footprint_name)));
    if out_file.exists() && !force {
        return Ok(out_file);
    }
    write_pcblib(&library, &out_file)?;
    Ok(out_file)
}

pub async fn export_schlib(
    client: &LcedaClient,
    item: &SearchItem,
    out_dir: &Path,
    force: bool,
) -> Result<PathBuf> {
    export_schlib_with_options(client, item, out_dir, force, false, false).await
}

pub async fn export_schlib_with_options(
    client: &LcedaClient,
    item: &SearchItem,
    out_dir: &Path,
    force: bool,
    lcsc_english: bool,
    use_template: bool,
) -> Result<PathBuf> {
    fs::create_dir_all(out_dir)?;
    let symbol_uuid = item
        .symbol_uuid()
        .ok_or(AppError::MissingSymbolOrFootprint)?;
    let symbol_data = client.component_detail(&symbol_uuid).await?;
    let component_name = resolved_symbol_component_name(item, &symbol_data);

    // Resolve footprint link, potentially overriding with the standard IPC name.
    let (footprint_model_name, footprint_library_file) =
        if let Some(footprint_uuid) = item.footprint_uuid() {
            let footprint_data = client.component_detail(&footprint_uuid).await?;
            let easyeda_footprint_name = resolved_footprint_name(item, &footprint_data);

            let (model_name, lib_file) = if use_template {
                let designator = nested_string(&item.raw, &["attributes", "Designator"])
                    .unwrap_or_default();
                let class = classify_component(&designator);
                if class != ComponentClass::Other {
                    if let Some(pkg) = extract_package_size(&easyeda_footprint_name) {
                        let std_name = standard_footprint_name(class, &pkg);
                        let lib = format!("{}.PcbLib", std_name);
                        (std_name, lib)
                    } else {
                        let lib = format!("{}.PcbLib", sanitize_filename(&easyeda_footprint_name));
                        (easyeda_footprint_name, lib)
                    }
                } else {
                    let lib = format!("{}.PcbLib", sanitize_filename(&easyeda_footprint_name));
                    (easyeda_footprint_name, lib)
                }
            } else {
                let lib = format!("{}.PcbLib", sanitize_filename(&easyeda_footprint_name));
                (easyeda_footprint_name, lib)
            };

            (Some(model_name), Some(lib_file))
        } else {
            (None, None)
        };

    let english_metadata = if lcsc_english {
        let lcsc_id = item
            .lcsc_id()
            .ok_or_else(|| AppError::Other("selected component has no LCSC ID".to_string()))?;
        Some(LcscClient::new().product_detail(&lcsc_id).await?)
    } else {
        None
    };

    let component = build_schlib_component_from_detail(
        item,
        &symbol_data,
        &component_name,
        footprint_model_name.as_deref(),
        footprint_library_file.as_deref(),
        english_metadata.as_ref(),
    )?;

    let out_file = out_dir.join(format!("{}.SchLib", sanitize_filename(component.name())));
    if out_file.exists() && !force {
        return Ok(out_file);
    }

    // If a SchLib template exists in assets/, substitute its geometry while keeping metadata.
    // Skip for polarized capacitors (electrolytic, tantalum, etc.) — template is MLCC-only.
    let template_record = if use_template {
        let designator = nested_string(&item.raw, &["attributes", "Designator"])
            .unwrap_or_default();
        let class = classify_component(&designator);
        let polarized = class == ComponentClass::ChipCapacitor
            && english_metadata
                .as_ref()
                .is_some_and(|p| p.is_polarized_capacitor());
        if class != ComponentClass::Other && !polarized {
            find_assets_dir().and_then(|dir| load_schlib_template_record(&dir, class))
        } else {
            None
        }
    } else {
        None
    };

    if let Some(mut tmpl) = template_record {
        let template_designator = tmpl.name.clone();
        let base = schlib_record_from_component(&component)?;
        if let Some(product) = &english_metadata {
            let fp_name = footprint_model_name.as_deref().unwrap_or("");
            let replacements = build_schlib_replacements_from_lcsc(product, fp_name);
            let description = product.description.clone().unwrap_or_default();
            let footprint_link = footprint_model_name.as_deref().zip(footprint_library_file.as_deref());
            tmpl.data = patch_schlib_data_with_params(
                &tmpl.data,
                &base.name,
                Some(&description),
                &replacements,
                footprint_link,
                &["1", "2"],
                SCHLIB_PARAM_RENAMES,
                &template_designator,
            );
        } else {
            tmpl.data = patch_schlib_data_component_name(
                &tmpl.data,
                &base.name,
                base.identity.as_deref(),
            );
        }
        tmpl.name = base.name;
        tmpl.description = base.description;
        tmpl.identity = base.identity;
        tmpl.weight = base.weight.max(tmpl.weight);
        write_schlib_records(&[tmpl], &out_file)?;
    } else {
        write_schlib(&component, &out_file)?;
    }
    Ok(out_file)
}

pub async fn export_bundle(
    client: &LcedaClient,
    item: &SearchItem,
    out_dir: &Path,
    force: bool,
) -> Result<BTreeMap<String, PathBuf>> {
    fs::create_dir_all(out_dir)?;

    let mut exported = export_easyeda_sources(client, item, out_dir, force).await?;
    let base = sanitize_filename(item.display_name());

    let mut resolved_model_uuid = None;
    let mut step_file = None;
    if item.model_uuid.is_some() {
        let model_uuid = client.get_model_uuid(item).await?;
        resolved_model_uuid = Some(model_uuid.clone());
        let path = out_dir.join(item.choose_step_filename());
        if force || !path.exists() {
            let content = client.download_step_bytes(&model_uuid).await?;
            fs::write(&path, content)?;
        }
        step_file = Some(path.clone());
        exported.insert("step".to_string(), path);
    }

    let manifest = BundleManifest {
        component_name: item.display_name().to_string(),
        manufacturer: item.manufacturer.clone(),
        search_index: item.index,
        symbol_uuid: item.symbol_uuid(),
        footprint_uuid: item.footprint_uuid(),
        model_seed_uuid: item.model_uuid.clone(),
        model_resolved_uuid: resolved_model_uuid,
        symbol_file: exported.get("symbol").map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }),
        footprint_file: exported.get("footprint").map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }),
        step_file: step_file.as_ref().map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }),
    };

    let manifest_file = out_dir.join(format!("{base}_bundle.json"));
    if force || !manifest_file.exists() {
        fs::write(&manifest_file, to_string_pretty(&manifest)?)?;
    }
    exported.insert("manifest".to_string(), manifest_file);

    Ok(exported)
}

fn build_schlib_metadata(
    item: &SearchItem,
    symbol_data: &Value,
    footprint_model_name: Option<&str>,
    footprint_library_file: Option<&str>,
    english_metadata: Option<&LcscProduct>,
) -> SchlibMetadata {
    let mut parameters = Vec::new();
    let mut seen_names = std::collections::HashSet::new();
    let resolved_footprint_name = footprint_model_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            first_non_empty([
                nested_string(&item.raw, &["footprint", "display_title"]),
                nested_string(&item.raw, &["attributes", "Supplier Footprint"]),
            ])
        });


    if let Some(footprint_name) = resolved_footprint_name.as_deref() {
        push_schlib_parameter(
            &mut parameters,
            &mut seen_names,
            "Footprint",
            footprint_name,
        );
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
        footprint_model_name: resolved_footprint_name,
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

    #[test]
    fn resolves_manufacturer_part_formula_comment() {
        let item = SearchItem {
            index: 0,
            display_title: "RP2040".to_string(),
            title: String::new(),
            manufacturer: "Raspberry Pi".to_string(),
            model_uuid: None,
            raw: json!({
                "attributes": {
                    "Name": "={Manufacturer Part}",
                    "Manufacturer Part": "RP2040"
                }
            }),
        };

        assert_eq!(resolve_schlib_comment(&item).as_deref(), Some("RP2040"));
    }

    #[test]
    fn resolves_value_formula_comment() {
        let item = SearchItem {
            index: 0,
            display_title: "TMB12A05".to_string(),
            title: String::new(),
            manufacturer: "XUNPU".to_string(),
            model_uuid: None,
            raw: json!({
                "attributes": {
                    "Name": "={Value}",
                    "Value": "2.4kHz",
                    "Manufacturer Part": "TMB12A05"
                }
            }),
        };

        assert_eq!(resolve_schlib_comment(&item).as_deref(), Some("2.4kHz"));
    }

    #[test]
    fn falls_back_when_formula_cannot_be_resolved() {
        let item = SearchItem {
            index: 0,
            display_title: "XC7Z020-2CLG400I".to_string(),
            title: String::new(),
            manufacturer: "AMD".to_string(),
            model_uuid: None,
            raw: json!({
                "attributes": {
                    "Name": "={Missing Field}",
                    "Manufacturer Part": "XC7Z020-2CLG400I"
                }
            }),
        };

        assert_eq!(
            resolve_schlib_comment(&item).as_deref(),
            Some("XC7Z020-2CLG400I")
        );
    }

    #[test]
    fn resolves_footprint_name_from_payload_metadata() {
        let item = SearchItem {
            index: 0,
            display_title: "STM8S003F3U6TR".to_string(),
            title: String::new(),
            manufacturer: "ST".to_string(),
            model_uuid: None,
            raw: json!({
                "footprint": {
                    "display_title": "UFQFPN-20_L3.0-W3.0-P0.50-TL"
                }
            }),
        };
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
