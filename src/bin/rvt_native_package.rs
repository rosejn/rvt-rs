//! Write an additive native delivery package from one RVT file.
use anyhow::{Context, Result, ensure};
use clap::{Parser, ValueEnum};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    time::Instant,
};

#[derive(Parser)]
#[command(
    name = "rvt-native-package",
    about = "Write a deterministic native delivery package"
)]
struct Args {
    file: PathBuf,
    #[arg(long)]
    document_namespace: String,
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(i64).range(1..=3))]
    detail_level: i64,
    /// Persist only viewer-ready tiles, attributes, and binary GLBs by default.
    #[arg(long, value_enum, default_value_t = ProfileArg::Tiles)]
    profile: ProfileArg,
    #[arg(long, value_delimiter = ',')]
    ids: Vec<u64>,
    /// Native Revit BuiltInCategory numeric IDs to retain. This is a
    /// fail-closed selection over decoded native metadata, not an ODA ID list.
    /// May be repeated or comma-separated; it cannot be combined with --ids.
    #[arg(long = "category-id", value_delimiter = ',')]
    category_ids: Vec<i64>,
    /// One of the checked product category profiles: arch-bul-v1 or
    /// mep-bul-v1. Its labels resolve through the versioned native vocabulary.
    #[arg(long)]
    category_profile: Option<String>,
    /// Additional checked `OST_*` labels, repeatable or comma-separated.
    #[arg(long = "category", value_delimiter = ',')]
    categories: Vec<String>,
    /// Maximum decoded values per native graph; raise explicitly for unusually large saved graphics.
    #[arg(long, default_value_t = 100_000)]
    max_graph_values: usize,
    /// Maximum decoded objects per native graph.
    #[arg(long, default_value_t = 100_000)]
    max_graph_objects: usize,
    /// Emit a bounded sharded package instead of one package. Zero keeps the
    /// single-package behavior; positive values are current element ids per shard
    /// unless a cost budget is also supplied.
    #[arg(long, default_value_t = 0)]
    shard_size: usize,
    /// Maximum estimated native body bytes per shard. The
    /// estimate includes current channel-102 and channel-103 records; symbol
    /// closure can still add work, so decoder graph budgets remain authoritative.
    #[arg(long, default_value_t = 0)]
    shard_max_cost_bytes: usize,
    /// Hard maximum number of selected owners in one shard. This is an
    /// additional cap applied alongside --shard-size and is useful when
    /// decoded graph/mesh expansion is much larger than stored-body size.
    #[arg(long, default_value_t = 0)]
    shard_max_owners: usize,
    /// Maximum resident memory for one process-isolated shard worker. Zero
    /// disables the guard; an exceeded multi-owner shard is deterministically
    /// split, while a singleton fails with its explicit owner scope.
    #[arg(long, default_value_t = 0)]
    worker_max_rss_bytes: u64,
    /// Maximum elapsed seconds for one process-isolated shard worker. Zero
    /// disables the guard; an exceeded multi-owner shard is deterministically
    /// split, while a singleton fails with its explicit owner scope.
    #[arg(long, default_value_t = 0)]
    worker_max_elapsed_seconds: u64,
    /// Open a fresh process and RVT reader per shard. This is reserved for
    /// pathological inputs requiring a hard RSS boundary; the default keeps
    /// one reader and physical index for the entire run.
    #[arg(long)]
    isolate_workers: bool,
    /// Write only the fail-closed native category-selection receipt and exit.
    /// This performs the one bounded metadata scan but does not decode saved
    /// graphics or create a package/staging directory.
    #[arg(long)]
    category_selection_receipt: Option<PathBuf>,
    #[arg(long, hide = true)]
    source_sha256: Option<String>,
    /// A strict source-bound ES witness in rvt-source-bound-es-witness-v1 format.
    /// It may carry a fully measured persisted field order for the exact
    /// source, but is never a general Revit API import.
    /// Supplying one hashes the RVT before extraction and refuses any witness
    /// not bound to those exact bytes.
    #[arg(long)]
    extensible_storage_witness: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProfileArg {
    Tiles,
    RichTiles,
    Audit,
}

impl From<ProfileArg> for rvt::native_delivery::DeliveryProfile {
    fn from(value: ProfileArg) -> Self {
        match value {
            ProfileArg::Tiles => Self::Tiles,
            ProfileArg::RichTiles => Self::RichTiles,
            ProfileArg::Audit => Self::Audit,
        }
    }
}

fn main() -> Result<()> {
    let a = Args::parse();
    let category_ids = resolve_category_ids(&a)?;
    if a.category_selection_receipt.is_none() {
        ensure!(!a.output_dir.exists(), "output directory exists");
    }
    ensure!(
        category_ids.is_empty() || a.ids.is_empty(),
        "--category-id, --category, and --category-profile cannot be combined with --ids"
    );
    if let Some(receipt_path) = &a.category_selection_receipt {
        ensure!(
            !category_ids.is_empty(),
            "--category-selection-receipt requires --category-id, --category, or --category-profile"
        );
        ensure!(
            !receipt_path.exists(),
            "category selection receipt already exists"
        );
    }
    // Do not pre-hash the whole RVT here. The delivery reader has its own
    // bounded document passes; reading the complete container once merely to
    // compute a digest would be unaccounted duplicate I/O before extraction.
    // Callers that already have a source digest may provide it explicitly.
    let source_sha256 = if a.extensible_storage_witness.is_some() {
        let observed = format!("{:x}", Sha256::digest(fs::read(&a.file)?));
        if let Some(expected) = &a.source_sha256 {
            ensure!(expected == &observed, "--source-sha256 does not match RVT bytes");
        }
        Some(observed)
    } else {
        a.source_sha256.clone()
    };
    let source_bound_extensible_storage_catalog = match &a.extensible_storage_witness {
        Some(path) => {
            let source_sha256 = source_sha256
                .as_deref()
                .expect("witness mode computes a source SHA-256");
            Some(rvt::native_es_catalog::decode_source_bound_witness(
                &fs::read(path).with_context(|| format!("read ES witness {}", path.display()))?,
                source_sha256,
            )?)
        }
        None => None,
    };
    let options = rvt::native_delivery::DeliveryOptions {
        document_namespace: a.document_namespace.clone(),
        source_sha256: source_sha256.clone(),
        scene_detail: a.detail_level,
        profile: a.profile.into(),
        native: rvt::native_document::Options {
            selected_ids: a.ids.iter().copied().collect(),
            max_graph_values: a.max_graph_values,
            max_graph_objects: a.max_graph_objects,
            ..Default::default()
        },
        metadata_ids: None,
        source_bound_extensible_storage_catalog,
    };
    if !category_ids.is_empty() {
        if let Some(receipt_path) = &a.category_selection_receipt {
            let mut file = rvt::RevitFile::open(&a.file)?;
            let index = rvt::native_document::build_physical_index(&mut file, &options.native)?;
            let context =
                rvt::native_document::build_definition_context(&mut file, &options.native, &index)?;
            let prepared = rvt::native_delivery::prepare_category_selection(
                &mut file,
                &options.native,
                &index,
                &context,
                category_ids,
            )?;
            let selected_count = prepared.receipt.selected_records;
            fs::write(
                receipt_path,
                serde_json::to_vec_pretty(&json!({
                    "format": "rvt-native-category-selection-v1",
                    "receipt": prepared.receipt,
                    // This identity list belongs only to the explicitly
                    // requested diagnostic mode.  Production package
                    // manifests retain aggregate receipt counts and do not
                    // duplicate the element table.
                    "selected_ids": prepared.selected_ids(),
                }))?,
            )?;
            eprintln!(
                "wrote {} native category-selected ids to {}",
                selected_count,
                receipt_path.display()
            );
            return Ok(());
        }
        if a.shard_size > 0 || a.shard_max_cost_bytes > 0 || a.shard_max_owners > 0 {
            if a.isolate_workers {
                write_process_category_sharded_package(&a, source_sha256, category_ids)?;
            } else {
                write_single_session_category_sharded_package(&a, &options, category_ids)?;
            }
        } else {
            let mut file = rvt::RevitFile::open(&a.file)?;
            let (package, receipt) =
                rvt::native_delivery::extract_category_selected(&mut file, &options, category_ids)?;
            rvt::native_delivery::write_package_dir(&package, &a.output_dir)?;
            fs::write(
                a.output_dir.join("category-selection.json"),
                serde_json::to_vec_pretty(&receipt)?,
            )?;
            eprintln!(
                "wrote {} category-selected elements to {}",
                package.elements.len(),
                a.output_dir.display()
            );
        }
    } else if a.shard_size > 0 || a.shard_max_cost_bytes > 0 || a.shard_max_owners > 0 {
        if a.isolate_workers {
            write_process_sharded_package(&a, source_sha256)?;
        } else {
            ensure!(
                a.worker_max_rss_bytes == 0 && a.worker_max_elapsed_seconds == 0,
                "--worker-max-rss-bytes and --worker-max-elapsed-seconds require --isolate-workers"
            );
            write_single_session_sharded_package(&a, &options)?;
        }
        eprintln!(
            "wrote sharded delivery package to {} ({} ids per shard, {} estimated bytes per shard)",
            a.output_dir.display(),
            a.shard_size,
            a.shard_max_cost_bytes
        );
    } else {
        let mut file = rvt::RevitFile::open(&a.file)?;
        let package = rvt::native_delivery::extract(&mut file, &options)?;
        rvt::native_delivery::write_package_dir(&package, &a.output_dir)?;
        eprintln!(
            "wrote {} elements to {}",
            package.elements.len(),
            a.output_dir.display()
        );
    }
    Ok(())
}

/// Build a category-selected cost plan from one physical index and one native
/// metadata projection. The selected records are subsequently consumed by the
/// shard writer, so this does not re-open or re-project metadata per shard.
fn write_single_session_category_sharded_package(
    a: &Args,
    options: &rvt::native_delivery::DeliveryOptions,
    category_ids: BTreeSet<i64>,
) -> Result<()> {
    let mut file = rvt::RevitFile::open(&a.file)?;
    let index = rvt::native_document::build_physical_index(&mut file, &options.native)?;
    let context =
        rvt::native_document::build_definition_context(&mut file, &options.native, &index)?;
    let mut prepared = rvt::native_delivery::prepare_category_selection(
        &mut file,
        &options.native,
        &index,
        &context,
        category_ids,
    )?;
    let requested = prepared.selected_ids().into_iter().collect::<Vec<_>>();
    let shard_size = effective_shard_size(a);
    let shard_ids = build_shards(&requested, shard_size, a.shard_max_cost_bytes, &index)?;
    let planned = shard_ids.iter().flatten().copied().collect::<BTreeSet<_>>();
    let requested_set = requested.iter().copied().collect::<BTreeSet<_>>();
    ensure!(
        planned == requested_set && planned.len() == requested.len(),
        "category shard plan does not cover selected IDs exactly once"
    );
    let receipt =
        rvt::native_delivery::write_sharded_prepared_category_package_dirs_with_selected_ids(
            &mut file,
            options,
            &a.output_dir,
            shard_size,
            &index,
            &context,
            &mut prepared,
            shard_ids,
        )?;
    eprintln!(
        "wrote {} category-selected elements to {}",
        receipt.selected_records,
        a.output_dir.display()
    );
    Ok(())
}

fn resolve_category_ids(a: &Args) -> Result<BTreeSet<i64>> {
    let mut ids = a.category_ids.iter().copied().collect::<BTreeSet<_>>();
    if let Some(profile) = &a.category_profile {
        ids.extend(rvt::native_category_profile::resolve_profile(profile)?);
    }
    ids.extend(rvt::native_category_profile::resolve_labels(&a.categories)?);
    Ok(ids)
}

/// Build a cost-aware shard plan once, then decode every selection through
/// the same RVT reader. This avoids reopening the compound file and rebuilding
/// its physical index for every shard.
fn write_single_session_sharded_package(
    a: &Args,
    options: &rvt::native_delivery::DeliveryOptions,
) -> Result<()> {
    let mut file = rvt::RevitFile::open(&a.file)?;
    let index = rvt::native_document::build_physical_index(&mut file, &options.native)?;
    let requested: Vec<u64> = if a.ids.is_empty() {
        index.current_ids(102).into_iter().collect()
    } else {
        a.ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let shard_size = effective_shard_size(a);
    let shard_ids = build_shards(&requested, shard_size, a.shard_max_cost_bytes, &index)?;
    rvt::native_delivery::write_sharded_package_dirs_with_selected_ids(
        &mut file,
        options,
        &a.output_dir,
        shard_size,
        &index,
        shard_ids,
    )
}

/// Perform category selection once in the parent reader, then hand the exact
/// selected native IDs to the existing isolated-worker planner. Workers never
/// repeat category discovery; they only decode their assigned explicit IDs.
fn write_process_category_sharded_package(
    a: &Args,
    source_sha256: Option<String>,
    category_ids: BTreeSet<i64>,
) -> Result<()> {
    let mut file = rvt::RevitFile::open(&a.file)?;
    let native_options = rvt::native_document::Options {
        max_graph_values: a.max_graph_values,
        max_graph_objects: a.max_graph_objects,
        ..Default::default()
    };
    let index = rvt::native_document::build_physical_index(&mut file, &native_options)?;
    let context =
        rvt::native_document::build_definition_context(&mut file, &native_options, &index)?;
    let prepared = rvt::native_delivery::prepare_category_selection(
        &mut file,
        &native_options,
        &index,
        &context,
        category_ids,
    )?;
    let receipt = prepared.receipt.clone();
    let requested = prepared.selected_ids().into_iter().collect::<Vec<_>>();
    let parameter_definitions =
        rvt::native_delivery::parameter_definitions_from_registry(&context.registry)?;
    let parameter_bindings = context.registry.bindings.values().cloned().collect();
    drop(prepared);
    drop(context);
    drop(file);
    write_process_sharded_package_with_ids(
        a,
        source_sha256,
        requested,
        index,
        Some(receipt),
        parameter_definitions,
        parameter_bindings,
    )
}

/// Run each shard in a separate process. A fresh process is the only reliable
/// way to reclaim large CFB/DEFLATE allocations on files with pathological
/// graphics populations; dropping Rust values alone is not a hard RSS bound.
fn write_process_sharded_package(a: &Args, source_sha256: Option<String>) -> Result<()> {
    let mut index_file = rvt::RevitFile::open(&a.file)?;
    let index = rvt::native_document::build_physical_index(
        &mut index_file,
        &rvt::native_document::Options {
            max_graph_values: a.max_graph_values,
            max_graph_objects: a.max_graph_objects,
            ..Default::default()
        },
    )?;
    let requested: Vec<u64> = if a.ids.is_empty() {
        index.current_ids(102).into_iter().collect()
    } else {
        a.ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let definition_context = rvt::native_document::build_definition_context(
        &mut index_file,
        &rvt::native_document::Options {
            max_graph_values: a.max_graph_values,
            max_graph_objects: a.max_graph_objects,
            ..Default::default()
        },
        &index,
    )?;
    let parameter_definitions =
        rvt::native_delivery::parameter_definitions_from_registry(&definition_context.registry)?;
    let parameter_bindings = definition_context
        .registry
        .bindings
        .values()
        .cloned()
        .collect();
    drop(definition_context);
    drop(index_file);
    write_process_sharded_package_with_ids(
        a,
        source_sha256,
        requested,
        index,
        None,
        parameter_definitions,
        parameter_bindings,
    )
}

fn write_process_sharded_package_with_ids(
    a: &Args,
    source_sha256: Option<String>,
    requested: Vec<u64>,
    index: rvt::native_document::PhysicalIndex,
    category_selection: Option<rvt::native_delivery::CategorySelectionReceipt>,
    parameter_definitions: Vec<rvt::native_delivery::ParameterDefinitionRow>,
    parameter_bindings: Vec<rvt::native_parameter_definitions::Binding>,
) -> Result<()> {
    let shard_size = effective_shard_size(a);
    let shard_ids = build_shards(&requested, shard_size, a.shard_max_cost_bytes, &index)?;
    let id_costs = requested
        .iter()
        .map(|id| {
            (
                *id,
                index
                    .record_bytes(102, *id)
                    .saturating_add(index.record_bytes(103, *id)),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let shard_costs = shard_ids
        .iter()
        .map(|ids| estimated_cost(ids, &id_costs))
        .collect::<Vec<_>>();
    drop(index);

    let parent = a.output_dir.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".{}.staging",
        a.output_dir
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("rvt-package")
    ));
    ensure!(!staging.exists(), "staging directory exists");
    fs::create_dir(&staging)?;
    let worker_executable =
        std::env::temp_dir().join(format!("rvt-native-package-worker-{}", std::process::id()));
    ensure!(!worker_executable.exists(), "worker executable path exists");
    fs::copy(std::env::current_exe()?, &worker_executable)?;
    let result = (|| -> Result<()> {
        write_jsonl(
            &staging.join("parameter-definitions.jsonl"),
            &parameter_definitions,
        )?;
        write_jsonl(
            &staging.join("parameter-bindings.jsonl"),
            &parameter_bindings,
        )?;
        let mut shards = Vec::new();
        let mut pending = shard_ids
            .into_iter()
            .zip(shard_costs)
            .collect::<VecDeque<_>>();
        let mut tile_boxes = Vec::new();
        let mut shard_number = 0usize;
        let mut rss_split_count = 0usize;
        while let Some((ids, estimated_cost_bytes)) = pending.pop_front() {
            let uri = format!("shard-{shard_number:06}");
            let shard_output = staging.join(&uri);
            let ids_arg = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            let mut command = Command::new(&worker_executable);
            command
                .arg(&a.file)
                .arg("--document-namespace")
                .arg(&a.document_namespace)
                .arg("--output-dir")
                .arg(&shard_output)
                .arg("--detail-level")
                .arg(a.detail_level.to_string())
                .arg("--profile")
                .arg(
                    a.profile
                        .to_possible_value()
                        .expect("profile value")
                        .get_name(),
                )
                .arg("--ids")
                .arg(ids_arg)
                .arg("--max-graph-values")
                .arg(a.max_graph_values.to_string())
                .arg("--max-graph-objects")
                .arg(a.max_graph_objects.to_string());
            if let Some(source_sha256) = &source_sha256 {
                command.arg("--source-sha256").arg(source_sha256);
            }
            let outcome = match run_worker(
                &mut command,
                a.worker_max_rss_bytes,
                a.worker_max_elapsed_seconds,
                shard_number,
            ) {
                Ok(outcome) => outcome,
                Err(error)
                    if ids.len() > 1
                        && (a.worker_max_rss_bytes > 0 || a.worker_max_elapsed_seconds > 0)
                        && error.to_string().contains("exceeded worker budget") =>
                {
                    // A stored-body budget is only a scheduling estimate;
                    // imported symbols and graph fan-out can make one
                    // apparently ordinary shard pathological. Split the
                    // exact ordered id list and retry both halves, retaining
                    // all already-completed siblings in staging. A singleton
                    // still fails closed under the worker RSS guard.
                    let midpoint = ids.len() / 2;
                    let right = ids[midpoint..].to_vec();
                    let left = ids[..midpoint].to_vec();
                    let right_cost = estimated_cost(&right, &id_costs);
                    let left_cost = estimated_cost(&left, &id_costs);
                    remove_failed_shard_output(&staging, &shard_output)?;
                    pending.push_front((right, right_cost));
                    pending.push_front((left, left_cost));
                    rss_split_count += 1;
                    shard_number += 1;
                    continue;
                }
                Err(error) => return Err(error).context("run delivery shard worker"),
            };
            ensure!(
                outcome.status.success(),
                "delivery shard worker {shard_number} failed with {}",
                outcome.status
            );
            let manifest_path = shard_output.join("manifest.json");
            let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
            assert_worker_catalogs_match_root(&staging, &shard_output, shard_number)?;
            let child = manifest
                .as_object_mut()
                .context("isolated delivery child manifest object")?;
            child.insert(
                "parameter_definitions_uri".into(),
                Value::String("../parameter-definitions.jsonl".into()),
            );
            child.insert(
                "parameter_bindings_uri".into(),
                Value::String("../parameter-bindings.jsonl".into()),
            );
            if let Some(Value::Array(tables)) = child.get_mut("tables") {
                tables.retain(|table| {
                    !matches!(
                        table.as_str(),
                        Some("parameter_definitions" | "parameter_bindings")
                    )
                });
            }
            fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
            fs::remove_file(shard_output.join("parameter-definitions.jsonl"))?;
            fs::remove_file(shard_output.join("parameter-bindings.jsonl"))?;
            let coverage = manifest["coverage"].clone();
            let glb_bytes = fs::metadata(shard_output.join("content.glb"))
                .ok()
                .map(|metadata| metadata.len());
            let package_bytes = Some(directory_bytes(&shard_output)?);
            let tileset_uri = shard_output
                .join("tileset.json")
                .is_file()
                .then(|| format!("{uri}/tileset.json"));
            if tileset_uri.is_some() {
                let tileset: Value =
                    serde_json::from_slice(&fs::read(shard_output.join("tileset.json"))?)?;
                tile_boxes.push((
                    uri.clone(),
                    tileset["root"]["boundingVolume"]["box"].clone(),
                ));
            }
            shards.push(json!({
                "uri": uri,
                "first_element_id": ids.first(),
                "last_element_id": ids.last(),
                "requested_ids": ids.len(),
                "estimated_cost_bytes": estimated_cost_bytes,
                "peak_rss_bytes": outcome.peak_rss_bytes,
                "telemetry": {
                    "version": 1,
                    "elapsed_ms": outcome.elapsed_ms,
                    "renderable_vertices": coverage["renderable_vertices"].as_u64().unwrap_or(0),
                    "renderable_triangles": coverage["renderable_triangles"].as_u64().unwrap_or(0),
                    "glb_bytes": glb_bytes,
                    "package_bytes": package_bytes,
                    "peak_rss_bytes": outcome.peak_rss_bytes,
                },
                "tileset_uri": tileset_uri,
                "status": manifest["status"],
                "coverage": coverage,
            }));
            shard_number += 1;
        }
        let status = if shards.is_empty() {
            "empty"
        } else if shards
            .iter()
            .any(|shard| shard["status"] != "complete_supported_subset")
        {
            "partial"
        } else {
            "complete_supported_subset"
        };
        let mut manifest = json!({
            "format": "rvt-native-delivery-sharded-v2",
            "document_namespace": a.document_namespace,
            "source_sha256": source_sha256,
            "status": status,
            "shard_size": a.shard_size,
            "effective_shard_size": effective_shard_size(a),
            "shard_max_cost_bytes": a.shard_max_cost_bytes,
            "shard_max_owners": a.shard_max_owners,
            "worker_max_rss_bytes": a.worker_max_rss_bytes,
            "worker_max_elapsed_seconds": a.worker_max_elapsed_seconds,
            "rss_split_count": rss_split_count,
            "worker_budget_split_count": rss_split_count,
            "shard_count": shards.len(),
            "tileset_uri": (!tile_boxes.is_empty()).then_some("tileset.json"),
            "coordinate_frame": "each shard uses package local meters Z-up",
            "parameter_definitions_uri": "parameter-definitions.jsonl",
            "parameter_bindings_uri": "parameter-bindings.jsonl",
            "selection_semantics": if a.shard_max_cost_bytes > 0 { "shards are deterministic current channel-102 id order packed by validated channel-102/103 stored-body cost; worker-budget-pathological shards are recursively split into ordered child packages" } else { "shards are deterministic current channel-102 id ranges; worker-budget-pathological shards are recursively split into ordered child packages" },
            "shards": shards,
        });
        if let Some(receipt) = category_selection {
            manifest["category_selection"] = serde_json::to_value(receipt)?;
            manifest["selected_ids"] = json!(requested);
        }
        fs::write(
            staging.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        if !tile_boxes.is_empty() {
            let root_box = union_boxes(
                &tile_boxes
                    .iter()
                    .map(|(_, bounding_box)| bounding_box.clone())
                    .collect::<Vec<_>>(),
            )?;
            let children = manifest["shards"]
                .as_array()
                .context("sharded manifest entries")?
                .iter()
                .filter_map(|shard| {
                    let uri = shard["uri"].as_str()?;
                    let tileset_uri = shard["tileset_uri"].as_str()?;
                    let bounding_box = tile_boxes
                        .iter()
                        .find(|(candidate, _)| candidate == uri)
                        .map(|(_, bounding_box)| bounding_box.clone())?;
                    Some(json!({
                        "boundingVolume": {"box": bounding_box},
                        "geometricError": 0,
                        "content": {"uri": tileset_uri},
                        "extras": {"shard": uri}
                    }))
                })
                .collect::<Vec<_>>();
            fs::write(
                staging.join("tileset.json"),
                serde_json::to_vec_pretty(&json!({
                    "asset": {"version": "1.1", "extras": {
                        "lod": "exact external-child tilesets; geometricError 0; no simplification",
                        "manifest_uri": "manifest.json"
                    }},
                    "geometricError": 0,
                    "root": {"boundingVolume": {"box": root_box}, "geometricError": 0, "refine": "ADD", "children": children}
                }))?,
            )?;
        }
        fs::rename(&staging, &a.output_dir)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    let _ = fs::remove_file(&worker_executable);
    result
}

fn write_jsonl<T: serde::Serialize>(path: &Path, values: &[T]) -> Result<()> {
    let mut file = fs::File::create(path)?;
    for value in values {
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
    }
    file.flush()?;
    Ok(())
}

/// A hard worker-budget termination can occur after the child creates its
/// sibling staging directory but before its atomic rename. Remove both exact
/// paths before reusing the shard ordinal for the recursively split retry.
/// The parent staging directory and any completed sibling shards are never
/// candidates for removal.
fn remove_failed_shard_output(parent: &Path, shard_output: &Path) -> Result<()> {
    ensure!(
        shard_output.parent() == Some(parent),
        "failed shard output is not a direct child of parent staging"
    );
    let name = shard_output
        .file_name()
        .and_then(|name| name.to_str())
        .context("failed shard output name")?;
    ensure!(
        name.starts_with("shard-"),
        "failed shard output is not a shard directory"
    );
    for path in [
        shard_output.to_path_buf(),
        parent.join(format!(".{name}.staging")),
    ] {
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

/// The parent owns the document-scoped parameter catalogs in an isolated
/// package.  A worker's catalogs are transient, but they must be byte-for-byte
/// equivalent before we replace them with root references.  Otherwise a
/// reopened worker could silently make child parameters point at definitions
/// from a different document view.
fn assert_worker_catalogs_match_root(
    root: &Path,
    worker: &Path,
    shard_number: usize,
) -> Result<()> {
    for name in ["parameter-definitions.jsonl", "parameter-bindings.jsonl"] {
        let root_path = root.join(name);
        let worker_path = worker.join(name);
        ensure!(
            worker_path.is_file(),
            "isolated delivery worker {shard_number} omitted its {name} catalog"
        );
        ensure!(
            fs::read(&root_path)? == fs::read(&worker_path)?,
            "isolated delivery worker {shard_number} produced a {name} catalog that differs from the parent document catalog"
        );
    }
    Ok(())
}

struct WorkerOutcome {
    status: ExitStatus,
    elapsed_ms: u64,
    peak_rss_bytes: Option<u64>,
}

fn run_worker(
    command: &mut Command,
    max_rss_bytes: u64,
    max_elapsed_seconds: u64,
    shard_number: usize,
) -> Result<WorkerOutcome> {
    let started = Instant::now();
    if max_rss_bytes == 0 && max_elapsed_seconds == 0 {
        return Ok(WorkerOutcome {
            status: command.status()?,
            elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            peak_rss_bytes: None,
        });
    }
    let mut child = command.spawn()?;
    let mut peak_rss_bytes = None;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(WorkerOutcome {
                status,
                elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                peak_rss_bytes,
            });
        }
        if let Some(rss) = linux_child_rss_bytes(child.id())? {
            peak_rss_bytes = Some(peak_rss_bytes.unwrap_or(0).max(rss));
            if rss > max_rss_bytes {
                child.kill()?;
                let status = child.wait()?;
                anyhow::bail!(
                    "delivery shard worker {shard_number} exceeded worker budget: RSS {rss} > {max_rss_bytes} bytes ({status})"
                );
            }
        }
        if max_elapsed_seconds > 0
            && started.elapsed() >= std::time::Duration::from_secs(max_elapsed_seconds)
        {
            child.kill()?;
            let status = child.wait()?;
            anyhow::bail!(
                "delivery shard worker {shard_number} exceeded worker budget: elapsed {} >= {} seconds ({status})",
                started.elapsed().as_secs(),
                max_elapsed_seconds,
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

fn linux_child_rss_bytes(pid: u32) -> Result<Option<u64>> {
    let status = match fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(status) => status,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let Some(line) = status.lines().find(|line| line.starts_with("VmRSS:")) else {
        return Ok(None);
    };
    let Some(value) = line.split_whitespace().nth(1) else {
        return Ok(None);
    };
    Ok(Some(value.parse::<u64>()?.saturating_mul(1024)))
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total = total.saturating_add(directory_bytes(&entry.path())?);
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn build_shards(
    requested: &[u64],
    max_ids: usize,
    max_cost_bytes: usize,
    index: &rvt::native_document::PhysicalIndex,
) -> Result<Vec<Vec<u64>>> {
    index.partition_local_shards(requested, max_ids, max_cost_bytes)
}

fn effective_shard_size(a: &Args) -> usize {
    match (a.shard_size, a.shard_max_owners) {
        (0, cap) => cap,
        (size, 0) => size,
        (size, cap) => size.min(cap),
    }
}

fn estimated_cost(ids: &[u64], id_costs: &BTreeMap<u64, usize>) -> usize {
    ids.iter()
        .map(|id| id_costs.get(id).copied().unwrap_or(0))
        .sum()
}

fn union_boxes(boxes: &[Value]) -> Result<[f64; 12]> {
    ensure!(!boxes.is_empty(), "cannot union empty tile boxes");
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for value in boxes {
        let values = value.as_array().context("tile box array")?;
        ensure!(values.len() == 12, "tile box length");
        let numbers = values
            .iter()
            .map(|value| value.as_f64().context("tile box number"))
            .collect::<Result<Vec<_>>>()?;
        for axis in 0..3 {
            min[axis] = min[axis].min(numbers[axis] - numbers[3 + axis * 4]);
            max[axis] = max[axis].max(numbers[axis] + numbers[3 + axis * 4]);
        }
    }
    let center: [f64; 3] = std::array::from_fn(|axis| (min[axis] + max[axis]) / 2.);
    let extent: [f64; 3] = std::array::from_fn(|axis| (max[axis] - min[axis]) / 2.);
    Ok([
        center[0], center[1], center[2], extent[0], 0., 0., 0., extent[1], 0., 0., 0., extent[2],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_planning_rejects_ids_without_physical_records() {
        let index = rvt::native_document::PhysicalIndex::default();
        assert!(build_shards(&[4, 8, 15], 0, 2, &index).is_err());
    }

    #[test]
    fn owner_cap_is_composed_with_the_stored_cost_plan() {
        let args = Args::try_parse_from([
            "rvt-native-package",
            "model.rvt",
            "--document-namespace",
            "mepbul",
            "--output-dir",
            "package",
            "--shard-size",
            "4000",
            "--shard-max-cost-bytes",
            "67108864",
            "--shard-max-owners",
            "500",
        ])
        .unwrap();
        assert_eq!(effective_shard_size(&args), 500);
    }

    #[test]
    fn bul_profile_resolves_to_the_checked_native_category_set() {
        let args = Args::try_parse_from([
            "rvt-native-package",
            "model.rvt",
            "--document-namespace",
            "mepbul",
            "--output-dir",
            "package",
            "--category-profile",
            "mep-bul-v1",
            "--category",
            "OST_Walls",
        ])
        .unwrap();
        let categories = resolve_category_ids(&args).unwrap();
        assert!(categories.contains(&-2_008_044)); // OST_PipeCurves
        assert!(categories.contains(&-2_000_011)); // requested OST_Walls
        assert_eq!(categories.len(), 35);
    }

    #[test]
    fn isolated_worker_catalogs_must_match_the_parent_catalogs() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scratch = std::env::temp_dir().join(format!(
            "rvt-native-package-catalog-test-{}-{nonce}",
            std::process::id()
        ));
        let root = scratch.join("root");
        let worker = scratch.join("worker");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&worker).unwrap();
        for name in ["parameter-definitions.jsonl", "parameter-bindings.jsonl"] {
            fs::write(root.join(name), "{\"stable\":true}\n").unwrap();
            fs::write(worker.join(name), "{\"stable\":true}\n").unwrap();
        }
        assert_worker_catalogs_match_root(&root, &worker, 7).unwrap();
        fs::write(
            worker.join("parameter-bindings.jsonl"),
            "{\"stable\":false}\n",
        )
        .unwrap();
        assert!(
            assert_worker_catalogs_match_root(&root, &worker, 7)
                .unwrap_err()
                .to_string()
                .contains("differs from the parent")
        );
        fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn budget_retry_removes_only_the_failed_shard_and_its_hidden_staging_sibling() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rvt-native-package-retry-cleanup-{}-{nonce}",
            std::process::id()
        ));
        let failed = root.join("shard-000123");
        let hidden = root.join(".shard-000123.staging");
        let completed = root.join("shard-000122");
        fs::create_dir_all(&failed).unwrap();
        fs::create_dir_all(&hidden).unwrap();
        fs::create_dir_all(&completed).unwrap();
        remove_failed_shard_output(&root, &failed).unwrap();
        assert!(!failed.exists());
        assert!(!hidden.exists());
        assert!(completed.exists());
        assert!(remove_failed_shard_output(&root, &root.join("not-a-shard")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
