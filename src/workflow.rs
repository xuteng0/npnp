use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::to_string_pretty;

use crate::component::{
    build_pcblib_library_from_detail, build_schlib_component_from_detail,
    build_schlib_replacements_from_lcsc, load_step_bytes_for_pcblib,
    resolved_footprint_name, resolved_symbol_component_name,
};
use crate::error::{AppError, Result};
use crate::lceda::{LcedaClient, SearchItem};
use crate::lcsc::LcscClient;
use crate::merge::{schlib_record_from_component, write_pcblib_records, write_schlib_records};
use crate::schlib::params::{
    patch_schlib_data_component_name, patch_schlib_data_with_params, SCHLIB_PARAM_RENAMES,
};
use crate::pcblib::write_pcblib;
use crate::schlib::write_schlib;
use crate::template::{
    build_ipc_pcblib, classify_component, extract_package_size, find_assets_dir,
    load_local_step, load_pcblib_template_records, load_schlib_template_record,
    standard_footprint_name, ComponentClass,
};
use crate::util::{nested_string, sanitize_filename, split_obj_and_mtl};

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
        if let Some(assets_dir) = find_assets_dir() {
            if let Some(records) = load_pcblib_template_records(&assets_dir, class, &package) {
                write_pcblib_records(&records, &out_file)?;
                return Ok(out_file);
            }
        }
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
