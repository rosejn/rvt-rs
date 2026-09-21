//! Unified additive delivery package for current native records and saved graphics.
use crate::{
    RevitFile, native_delivery_spatial, native_delivery_tiles, native_document, native_saved_scene,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufWriter, Write},
    path::Path,
    time::Instant,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeliveryProfile {
    /// Viewer-ready 3D Tiles: manifest, attributes, tileset and binary GLBs.
    #[default]
    Tiles,
    /// Tiles plus domain tables used by rich project-data consumers.
    RichTiles,
    /// Rich tiles plus operational diagnostics and measurements.
    Audit,
}

impl DeliveryProfile {
    fn manifest_name(self) -> &'static str {
        match self {
            Self::Tiles => "tiles",
            Self::RichTiles => "rich-tiles",
            Self::Audit => "audit",
        }
    }

    fn emits_rich_tables(self) -> bool {
        matches!(self, Self::RichTiles | Self::Audit)
    }

    fn emits_audit_tables(self) -> bool {
        matches!(self, Self::Audit)
    }

    fn tables(self) -> Vec<&'static str> {
        // Parameter definitions are document-scoped. They are emitted once
        // rather than repeated inside every selected element's metadata.
        let mut tables = vec![
            "elements",
            "parameter_definitions",
            "parameter_bindings",
            "tileset",
        ];
        if self.emits_rich_tables() {
            tables.extend([
                "types",
                "relationships",
                "materials",
                "spatial_context",
                "spatial_boundaries",
                "room_connections",
                "network",
            ]);
        }
        if self.emits_audit_tables() {
            tables.extend(["metrics", "texture_mappings"]);
        }
        tables
    }
}

#[derive(Debug, Clone)]
pub struct DeliveryOptions {
    pub document_namespace: String,
    pub source_sha256: Option<String>,
    pub scene_detail: i64,
    /// Controls persisted artifacts only. Geometry is always represented by GLB,
    /// never by a parallel JSON vertex/index payload.
    pub profile: DeliveryProfile,
    pub native: native_document::Options,
    /// None means all current channel-102 records; Some is an explicit metadata filter.
    pub metadata_ids: Option<BTreeSet<u64>>,
    /// Optional source-bound ES catalog admitted for one verified source.
    /// A witness may carry a complete, measured persisted field order for that
    /// source; it never supplies general API values or overrides native schema
    /// declarations.
    /// hash. It supplements, never overrides, Global/Latest declarations.
    pub source_bound_extensible_storage_catalog: Option<(crate::native_extensible_storage::Catalog, Value)>,
}

fn build_definition_context(
    file: &mut RevitFile,
    options: &DeliveryOptions,
    physical_index: &native_document::PhysicalIndex,
) -> Result<native_document::DefinitionContext> {
    let mut context = native_document::build_definition_context(file, &options.native, physical_index)?;
    if let Some((supplement, receipt)) = &options.source_bound_extensible_storage_catalog {
        let catalog = context.extensible_storage_catalog.get_or_insert_with(Default::default);
        catalog.extend_nonconflicting(supplement.clone())?;
        context.extensible_storage_catalog_diagnostic = Some(format!(
            "supplemented by source-bound ES witness {}",
            receipt["witness_sha256"].as_str().unwrap_or("unknown")
        ));
    }
    Ok(context)
}

/// Fail-closed evidence for a native category selection.  The receipt is
/// deliberately based on decoded native metadata, never on an ODA-exported
/// element list.  Records without a recovered category are counted rather
/// than silently treated as matching a requested profile.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CategorySelectionReceipt {
    pub requested_category_ids: BTreeSet<i64>,
    /// Checked vocabulary label when known; unknown requested IDs retain a
    /// null label instead of being assigned a speculative category name.
    pub requested_category_labels: BTreeMap<i64, Option<&'static str>>,
    pub current_records: usize,
    /// Owners already decoded while building document definition/binding
    /// context. They are intentionally not decoded again by the category pass.
    pub definition_context_records_reused: usize,
    pub category_scan_records: usize,
    pub selected_records: usize,
    /// Selected from an owner-stored `m_categoryId` without a relationship
    /// lookup.
    pub selected_direct_category_records: usize,
    /// Selected by the exact FamilyInstance/FamilySymbol → Family category
    /// relationship held in the document definition registry.
    pub selected_inherited_category_records: usize,
    /// Selected by the small checked owner-class category vocabulary for
    /// system-native owners that serialize no category or family reference.
    pub selected_owner_class_category_records: usize,
    /// Selected from the structural `RoomElem` zone/area scheme pair. This is
    /// intentionally separate from the shared serialized class-name mapping:
    /// the same native class carries Rooms, MEP Spaces, and Areas.
    pub selected_room_space_scheme_category_records: usize,
    /// Selected from compact facts retained during the one-time definition
    /// context pass; these owners are never decoded a second time.
    pub selected_definition_context_category_records: usize,
    pub selected_by_category: BTreeMap<i64, usize>,
    pub excluded_by_category: BTreeMap<i64, usize>,
    pub no_category_records: usize,
    pub unsupported_graph_records: usize,
    pub unsupported_metadata_records: usize,
    pub no_category_classes: BTreeMap<String, usize>,
}

/// Metadata prepared during a whole-document category pass.  Owner graphs are
/// dropped immediately after projection: saved-scene extraction performs its
/// separate graphics decode, while delivery keeps the exact metadata already
/// used to make the selection decision.
#[derive(Debug)]
pub struct PreparedCategorySelection {
    selected_ids: BTreeSet<u64>,
    pub receipt: CategorySelectionReceipt,
}

impl PreparedCategorySelection {
    pub fn selected_ids(&self) -> BTreeSet<u64> {
        self.selected_ids.clone()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeliveryManifest {
    pub format: &'static str,
    pub profile: &'static str,
    pub document_namespace: String,
    pub source_sha256: Option<String>,
    pub status: String,
    pub units: &'static str,
    pub coordinate_frame: &'static str,
    /// Present on a sharded child when the document-scoped definitions are
    /// owned by the aggregate root rather than copied into this package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter_definitions_uri: Option<String>,
    /// Same ownership rule as `parameter_definitions_uri`, for the document
    /// binding catalog that maps custom parameters to categories.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter_bindings_uri: Option<String>,
    pub coverage: Coverage,
    pub tables: Vec<&'static str>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct Coverage {
    pub records: usize,
    pub metadata_rows: usize,
    pub geometry_instances: usize,
    pub mesh_rows: usize,
    pub instance_rows: usize,
    pub material_rows: usize,
    pub saved_filling_rows: usize,
    pub unresolved_material_primitives: usize,
    pub partial_records: usize,
    pub metadata_only_records: usize,
    /// A saved graphics graph decoded, but the requested detail/visibility
    /// selection retained no renderable surface primitive.
    pub nonrenderable_graphics_records: usize,
    pub geometry_only_records: usize,
    pub renderable_geometry_instances: usize,
    pub renderable_mesh_rows: usize,
    pub renderable_vertices: usize,
    pub renderable_triangles: usize,
    /// Owners whose requested authored detail was explicitly reduced by the
    /// bounded extraction policy. This is coverage telemetry, not a failure:
    /// the owner row retains both requested and effective detail levels.
    pub authored_detail_downgrade_records: usize,
    pub empty_geometry_owners: usize,
    pub unique_mesh_assets: usize,
    pub adaptive_graph_retries: usize,
    pub adaptive_graph_recoveries: usize,
    pub diagnostics: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct ElementRow {
    pub key: String,
    pub document_namespace: String,
    pub unique_id: String,
    pub element_id: u64,
    pub class_name: Option<String>,
    /// Revit BuiltInCategory numeric identity when stated by the decoded
    /// metadata graph. This stays numeric rather than guessing a display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_id: Option<i64>,
    /// Checked vocabulary label for a known numeric category. Unknown native
    /// IDs remain numeric and do not receive a guessed label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_label: Option<&'static str>,
    /// Axis-aligned bounds of retained renderable saved geometry in document
    /// meters/Z-up. This is compact audit evidence, not a second mesh form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geometry_bounds_meters: Option<[[f64; 3]; 2]>,
    pub status: String,
    pub source: Value,
    pub attributes: Value,
}

pub fn category_id(record: Option<&native_document::Record>) -> Result<Option<i64>> {
    let mut values = record
        .and_then(|record| record.saved_metadata.as_ref())
        .into_iter()
        .flat_map(|metadata| metadata.raw_field_parameters.iter())
        .filter(|parameter| {
            parameter.source_field == "m_categoryId" && parameter.storage_type == "ElementId"
        })
        .filter_map(|parameter| parameter.raw_value.as_i64())
        .collect::<BTreeSet<_>>();
    if let Some(value) = record
        .and_then(|record| record.graph.as_ref())
        .and_then(|graph| graph.objects.first())
        .and_then(|root| root.fields.get("m_categoryId"))
        .map(crate::native_metadata::identifier)
        .transpose()?
    {
        values.insert(value);
    }
    ensure!(
        values.len() <= 1,
        "conflicting saved BuiltInCategory values for one element"
    );
    Ok(values.into_iter().next())
}

/// Recover a category from direct root state or the already-decoded document
/// definition registry.  Category selection must not expand deferred owner
/// pointers merely to follow the ordinary FamilyInstance → FamilySymbol →
/// Family relationship: that relationship is document-scoped and was decoded
/// once while constructing `DefinitionContext`.
///
/// The returned source is receipt evidence, not a heuristic.  A direct and an
/// inherited value are required to agree when both are available.
fn root_category_id_with_definitions(
    record: &native_document::Record,
    registry: &crate::native_parameter_definitions::Registry,
) -> Result<Option<(i64, &'static str)>> {
    let direct = category_id(Some(record))?;
    let inherited = match record
        .graph
        .as_ref()
        .and_then(|graph| graph.objects.first())
    {
        Some(root) => match root.class_name.as_str() {
            class_name
                if crate::native_parameter_definitions::is_family_symbol_definition_owner(
                    class_name,
                ) =>
            {
                root.fields
                    .get("m_familyId")
                    .map(crate::native_metadata::identifier)
                    .transpose()?
                    .and_then(|family| registry.family_categories.get(&family).copied())
            }
            "FamilyInstance" => root
                .fields
                .get("m_masterSymbolId")
                .map(crate::native_metadata::identifier)
                .transpose()?
                .and_then(|symbol| registry.symbol_families.get(&symbol).copied())
                .and_then(|family| registry.family_categories.get(&family).copied()),
            _ => None,
        },
        None => None,
    };
    let owner_class = record
        .graph
        .as_ref()
        .and_then(|graph| graph.objects.first())
        .map(|root| root.class_name.as_str())
        .or(record.class_name.as_deref());
    // `RoomElem` is the common native carrier for architectural Rooms, Areas,
    // and MEP Spaces. Its category is not encoded by the class name: the
    // persisted scheme pair is the structural discriminator. Retain the
    // existing spatial-context classifier here so category selection and the
    // delivered world-model descriptor cannot drift apart.
    let room_space_category = match record
        .graph
        .as_ref()
        .and_then(|graph| graph.objects.first())
    {
        Some(root) if root.class_name == "RoomElem" => {
            let (kind, _, _) = crate::native_spatial_context::classify_room_space(&root.fields)?;
            match kind.as_str() {
                "room" => Some(-2_000_160),
                "mep_space" => Some(-2_003_600),
                // Areas and contradictory scheme state have no requested
                // category recovery. The raw scheme fields remain emitted
                // in spatial context for explicit downstream handling.
                "area" | "contradictory_room_space" => None,
                _ => unreachable!("closed RoomElem scheme classifier"),
            }
        }
        _ => None,
    };
    let class_category = owner_class
        .and_then(crate::native_category_profile::owner_class_category_id);
    let recovered = [direct, inherited, room_space_category, class_category]
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
    ensure!(
        recovered.len() <= 1,
        "direct, inherited, and owner-class BuiltInCategory values disagree for element {}",
        record.identity.element_id
    );
    Ok(direct
        .map(|value| (value, "direct_root_category"))
        .or_else(|| inherited.map(|value| (value, "symbol_family_category_chain")))
        .or_else(|| room_space_category.map(|value| (value, "checked_room_space_scheme_category")))
        .or_else(|| class_category.map(|value| (value, "checked_owner_class_category"))))
}

/// Resolve category facts retained from a definition owner already decoded to
/// construct `DefinitionContext`.  This deliberately uses only the compact
/// candidate, never a second owner decode.
fn definition_owner_category_id(
    candidate: &native_document::DefinitionOwnerCategoryCandidate,
    registry: &crate::native_parameter_definitions::Registry,
) -> Result<Option<(i64, &'static str)>> {
    let family_category = candidate
        .family_id
        .and_then(|family| registry.family_categories.get(&family).copied());
    let class_category =
        crate::native_category_profile::owner_class_category_id(&candidate.class_name);
    let recovered = [
        candidate.direct_category_id,
        family_category,
        class_category,
    ]
    .into_iter()
    .flatten()
    .collect::<BTreeSet<_>>();
    ensure!(
        recovered.len() <= 1,
        "definition owner category facts disagree for class {}",
        candidate.class_name
    );
    Ok(candidate
        .direct_category_id
        .map(|value| (value, "definition_context_direct_category"))
        .or_else(|| family_category.map(|value| (value, "definition_context_family_category")))
        .or_else(|| class_category.map(|value| (value, "definition_context_owner_class_category"))))
}

/// Decode current metadata once, select only explicitly requested native
/// BuiltInCategory values, and discard the decoded owner graph for retained
/// records.  The caller must reuse the returned records for packaging; doing
/// another selected metadata pass would duplicate the work this API exists to
/// eliminate.
pub fn prepare_category_selection(
    file: &mut RevitFile,
    options: &native_document::Options,
    physical_index: &native_document::PhysicalIndex,
    definition_context: &native_document::DefinitionContext,
    category_ids: BTreeSet<i64>,
) -> Result<PreparedCategorySelection> {
    ensure!(
        !category_ids.is_empty(),
        "category selection requires at least one BuiltInCategory ID"
    );
    let mut receipt = CategorySelectionReceipt {
        requested_category_labels: category_ids
            .iter()
            .map(|id| (*id, crate::native_category_profile::category_label(*id)))
            .collect(),
        requested_category_ids: category_ids,
        current_records: physical_index.current_ids(102).len(),
        definition_context_records_reused: definition_context.definition_owner_ids.len(),
        ..Default::default()
    };
    let mut metadata_options = options.clone();
    metadata_options.channels = BTreeSet::from([102]);
    metadata_options.selected_ids = physical_index.current_ids(102);
    for id in &definition_context.definition_owner_ids {
        metadata_options.selected_ids.remove(id);
    }
    let mut selected_ids = BTreeSet::new();
    native_document::extract_roots_using_index_with_context(
        file,
        &metadata_options,
        physical_index,
        definition_context,
        |record| {
            receipt.category_scan_records += 1;
            if record.graph.is_none() {
                receipt.unsupported_graph_records += 1;
            }
            if record.metadata_diagnostic.is_some() {
                receipt.unsupported_metadata_records += 1;
            }
            match root_category_id_with_definitions(&record, &definition_context.registry)? {
                Some((category_id, source))
                    if receipt.requested_category_ids.contains(&category_id) =>
                {
                    receipt.selected_records += 1;
                    *receipt.selected_by_category.entry(category_id).or_default() += 1;
                    if source == "direct_root_category" {
                        receipt.selected_direct_category_records += 1;
                    } else if source == "symbol_family_category_chain" {
                        receipt.selected_inherited_category_records += 1;
                    } else if source == "checked_room_space_scheme_category" {
                        receipt.selected_room_space_scheme_category_records += 1;
                    } else {
                        receipt.selected_owner_class_category_records += 1;
                    }
                    // This pass only establishes the bounded selection index.
                    // Full metadata is decoded once, later, for exactly these
                    // selected owners as their delivery shard is produced.
                    ensure!(
                        selected_ids.insert(record.identity.element_id),
                        "duplicate selected native record"
                    );
                }
                Some((category_id, _)) => {
                    *receipt.excluded_by_category.entry(category_id).or_default() += 1;
                }
                None => {
                    receipt.no_category_records += 1;
                    *receipt
                        .no_category_classes
                        .entry(
                            record
                                .class_name
                                .clone()
                                .unwrap_or_else(|| "<unregistered>".into()),
                        )
                        .or_default() += 1;
                }
            }
            Ok(())
        },
    )?;
    // Definition owners were intentionally omitted from the root pass because
    // the context already decoded them once.  Add their retained category
    // facts now; this covers qualifying system types and FamilySymbol-backed
    // type rows without duplicating their graph work.
    for (id, candidate) in &definition_context.definition_owner_category_candidates {
        let Some((category_id, _)) =
            definition_owner_category_id(candidate, &definition_context.registry)?
        else {
            continue;
        };
        if receipt.requested_category_ids.contains(&category_id) {
            receipt.selected_records += 1;
            receipt.selected_definition_context_category_records += 1;
            *receipt.selected_by_category.entry(category_id).or_default() += 1;
            ensure!(
                selected_ids.insert(*id),
                "definition context duplicated selected native record"
            );
        } else {
            *receipt.excluded_by_category.entry(category_id).or_default() += 1;
        }
    }
    ensure!(
        receipt.category_scan_records + receipt.definition_context_records_reused
            == receipt.current_records,
        "native category selection did not account for every current channel-102 owner"
    );
    Ok(PreparedCategorySelection {
        selected_ids,
        receipt,
    })
}
#[derive(Debug, Serialize)]
pub struct TypeRow {
    pub key: String,
    pub type_element_id: i64,
    pub class_name: Option<String>,
    pub source_element_key: String,
    pub attributes: Value,
}
#[derive(Debug, Serialize)]
pub struct ParameterDefinitionRow {
    pub parameter_id: i64,
    /// Distinguishes a document-owned native definition from the validated
    /// built-in catalog projection; parameter IDs remain the stable key.
    pub definition_kind: &'static str,
    /// The source metadata projection that defines this parameter. Owner rows
    /// retain values and refer to this stable numeric identity.
    pub definition: Value,
}
#[derive(Debug, Serialize)]
pub struct RelationshipRow {
    pub from: String,
    pub kind: String,
    pub to: String,
    pub status: String,
}
#[derive(Debug, Serialize)]
pub struct MeshRow {
    pub key: String,
    pub element_key: String,
    pub primitive_index: usize,
    pub source_owner_id: Option<u64>,
    pub graphics_object_index: usize,
    pub face_tag: i64,
    pub render_style_id: i64,
    pub explicit_material_id: Option<i64>,
    pub vertex_count: usize,
    pub triangle_count: usize,
    pub status: String,
}
#[derive(Debug, Serialize)]
pub struct InstanceRow {
    pub key: String,
    pub element_key: String,
    pub source_frame: String,
    pub local_origin_meters: [f64; 3],
    pub mesh_keys: Vec<String>,
    pub status: String,
}
#[derive(Debug, Serialize)]
pub struct MaterialRow {
    pub key: String,
    pub element_key: String,
    pub primitive_index: usize,
    pub material: crate::native_saved_materials::RenderMaterial,
}
#[derive(Debug, Serialize)]
pub struct OwnerMetricsRow {
    pub element_key: String,
    pub element_id: u64,
    pub class_name: Option<String>,
    pub metadata_present: bool,
    pub metadata_body_bytes: Option<usize>,
    pub graphics_present: bool,
    pub graphics_body_bytes: Option<usize>,
    pub graphics_status: Option<String>,
    pub geometry_retained: bool,
    pub primitive_count: usize,
    pub renderable_primitive_count: usize,
    pub vertex_count: usize,
    pub triangle_count: usize,
    pub estimated_geometry_bytes: usize,
    pub diagnostic_count: usize,
    pub unresolved_material_primitives: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct GeometryInstance {
    pub element_key: String,
    pub status: String,
    pub source_frame: String,
    pub local_origin_meters: [f64; 3],
    pub meshes: crate::native_saved_mesh::GraphicsMeshes,
    pub render_materials: BTreeMap<usize, crate::native_saved_materials::RenderMaterial>,
    pub unresolved_material_primitives: Vec<usize>,
}
#[derive(Debug, Serialize)]
pub struct DeliveryPackage {
    pub manifest: DeliveryManifest,
    pub elements: Vec<ElementRow>,
    pub parameter_definitions: Vec<ParameterDefinitionRow>,
    pub parameter_bindings: Vec<crate::native_parameter_definitions::Binding>,
    pub types: Vec<TypeRow>,
    pub relationships: Vec<RelationshipRow>,
    pub meshes: Vec<MeshRow>,
    pub instances: Vec<InstanceRow>,
    pub materials: Vec<MaterialRow>,
    pub geometry: Vec<GeometryInstance>,
    pub spatial_context: Option<Value>,
    pub spatial_boundaries: Option<Value>,
    /// Phase-qualified door adjacency derived only from explicit host topology.
    /// This remains an evidence-bearing partial projection; unresolved doors
    /// and unsupported parity are retained in the emitted inventory.
    pub room_connections: Option<Value>,
    /// Saved connector/system topology for this exact selected shard. Targets
    /// outside the selection remain explicit unresolved diagnostics; this is
    /// never a synthesized document-wide network.
    pub network: Option<Value>,
    pub texture_mappings: Option<Value>,
    pub metrics: Vec<OwnerMetricsRow>,
}

#[derive(Debug, Serialize)]
pub struct ShardTelemetry {
    /// Telemetry schema version; optional so older shard readers can ignore
    /// the entire field.
    pub version: u8,
    pub elapsed_ms: u64,
    pub renderable_vertices: u64,
    pub renderable_triangles: u64,
    pub glb_bytes: Option<u64>,
    pub package_bytes: Option<u64>,
    pub peak_rss_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ShardEntry {
    pub uri: String,
    pub first_element_id: Option<u64>,
    pub last_element_id: Option<u64>,
    pub requested_ids: usize,
    pub tileset_uri: Option<String>,
    pub coverage: Coverage,
    /// Optional v1 shard execution telemetry. Its absence is valid for
    /// manifests produced before telemetry was available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<ShardTelemetry>,
}

#[derive(Debug, Serialize)]
pub struct ShardedDeliveryManifest {
    pub format: &'static str,
    pub document_namespace: String,
    pub source_sha256: Option<String>,
    pub status: String,
    pub shard_size: usize,
    pub shard_count: usize,
    pub tileset_uri: Option<&'static str>,
    pub coordinate_frame: &'static str,
    pub parameter_definitions_uri: &'static str,
    pub parameter_bindings_uri: &'static str,
    /// Aggregate native selection evidence.  This is small document-level
    /// accounting, not a duplicate element table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_selection: Option<CategorySelectionReceipt>,
    /// Exact selected native IDs for category-selected sharded delivery.
    /// Optional to preserve the pre-category-selection manifest shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_ids: Option<Vec<u64>>,
    pub selection_semantics: &'static str,
    pub shards: Vec<ShardEntry>,
}

fn key(namespace: &str, unique_id: &str) -> String {
    format!("{namespace}:{unique_id}")
}

fn renderable(primitive: &crate::native_saved_mesh::Primitive) -> bool {
    !primitive.vertices.is_empty() && !primitive.triangles.is_empty()
}

pub(crate) fn primitive_asset_key(primitive: &crate::native_saved_mesh::Primitive) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"rvt-native-mesh-asset-v1");
    hasher.update((primitive.vertices.len() as u64).to_le_bytes());
    for vertex in &primitive.vertices {
        for value in vertex {
            hasher.update(value.to_bits().to_le_bytes());
        }
    }
    hasher.update((primitive.normals.len() as u64).to_le_bytes());
    for normal in &primitive.normals {
        for value in normal {
            hasher.update(value.to_bits().to_le_bytes());
        }
    }
    hasher.update((primitive.triangles.len() as u64).to_le_bytes());
    for triangle in &primitive.triangles {
        for value in triangle {
            hasher.update(value.to_le_bytes());
        }
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn source_body_bytes(source: &Value) -> Option<usize> {
    source
        .get("body_bytes")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn geometry_estimate(meshes: &crate::native_saved_mesh::GraphicsMeshes) -> (usize, usize, usize) {
    let mut vertices = 0;
    let mut triangles = 0;
    let mut bytes: usize = 0;
    for primitive in &meshes.primitives {
        if !renderable(primitive) {
            continue;
        }
        vertices += primitive.vertices.len();
        triangles += primitive.triangles.len();
        bytes = bytes.saturating_add(
            primitive
                .vertices
                .len()
                .saturating_mul(3 * std::mem::size_of::<f64>())
                .saturating_add(
                    primitive
                        .normals
                        .len()
                        .saturating_mul(3 * std::mem::size_of::<f64>()),
                )
                .saturating_add(
                    primitive
                        .triangles
                        .len()
                        .saturating_mul(3 * std::mem::size_of::<u32>()),
                ),
        );
    }
    (vertices, triangles, bytes)
}

fn geometry_bounds_meters(element: &native_saved_scene::SavedElement) -> Option<[[f64; 3]; 2]> {
    let mut minimum = [f64::INFINITY; 3];
    let mut maximum = [f64::NEG_INFINITY; 3];
    let mut any = false;
    for primitive in element
        .meshes
        .primitives
        .iter()
        .filter(|primitive| renderable(primitive))
    {
        for vertex in &primitive.vertices {
            for axis in 0..3 {
                let value = vertex[axis] * 0.3048;
                if !value.is_finite() {
                    return None;
                }
                minimum[axis] = minimum[axis].min(value);
                maximum[axis] = maximum[axis].max(value);
            }
            any = true;
        }
    }
    any.then_some([minimum, maximum])
}

/// Definitions describe the document, not an individual element.  A selected
/// record still carries the values/provenance that it owns, but the large
/// definition objects are pooled into one package-level catalog.
fn compact_metadata(
    metadata: &crate::native_metadata::SavedMetadata,
    has_shared_definition_catalog: bool,
    has_shared_binding_catalog: bool,
) -> Result<Value> {
    let mut value = serde_json::to_value(metadata)?;
    let object = value
        .as_object_mut()
        .context("saved metadata must serialize to an object")?;
    object.remove("parameter_definitions");
    if has_shared_definition_catalog {
        object.remove("builtin_parameter_definitions");
    }
    if has_shared_binding_catalog {
        // Binding applicability is document-scoped. Owner rows retain sparse
        // values and global-association state, while this repeated category
        // binding inventory is written once in parameter-bindings.jsonl.
        object.remove("declared_custom_parameters");
    }
    Ok(value)
}

fn collect_parameter_definitions<'a>(
    records: impl IntoIterator<Item = &'a native_document::Record>,
) -> Result<Vec<ParameterDefinitionRow>> {
    let mut definitions = BTreeMap::<i64, Value>::new();
    for record in records {
        let Some(metadata) = &record.saved_metadata else {
            continue;
        };
        for definition in &metadata.parameter_definitions {
            let value = serde_json::to_value(definition)?;
            if let Some(existing) = definitions.insert(definition.parameter_id, value.clone()) {
                ensure!(
                    existing == value,
                    "conflicting parameter definition {} across selected owners",
                    definition.parameter_id
                );
            }
        }
    }
    Ok(definitions
        .into_iter()
        .map(|(parameter_id, definition)| ParameterDefinitionRow {
            parameter_id,
            definition_kind: "native_document",
            definition,
        })
        .collect())
}

pub fn parameter_definitions_from_registry(
    registry: &crate::native_parameter_definitions::Registry,
) -> Result<Vec<ParameterDefinitionRow>> {
    let mut rows = BTreeMap::new();
    for (parameter_id, definition) in &registry.definitions {
        ensure!(
            rows.insert(
                *parameter_id,
                ParameterDefinitionRow {
                    parameter_id: *parameter_id,
                    definition_kind: "native_document",
                    definition: serde_json::to_value(definition)?,
                },
            )
            .is_none(),
            "duplicate native parameter definition {parameter_id}"
        );
    }
    for (parameter_id, definition) in &registry.builtin_definitions {
        ensure!(
            rows.insert(
                *parameter_id,
                ParameterDefinitionRow {
                    parameter_id: *parameter_id,
                    definition_kind: "builtin_catalog",
                    definition: serde_json::to_value(definition)?,
                },
            )
            .is_none(),
            "parameter ID {parameter_id} conflicts between native and builtin catalogs"
        );
    }
    Ok(rows.into_values().collect())
}

/// Join metadata records and saved-scene owners without dropping either population.
pub fn join_records(
    document_namespace: &str,
    records: Vec<native_document::Record>,
    scene: &native_saved_scene::SavedScene,
    source_sha256: Option<String>,
) -> Result<DeliveryPackage> {
    join_records_with_diagnostics(document_namespace, records, scene, source_sha256, &[])
}

pub fn join_records_with_diagnostics(
    document_namespace: &str,
    records: Vec<native_document::Record>,
    scene: &native_saved_scene::SavedScene,
    source_sha256: Option<String>,
    extra_diagnostics: &[String],
) -> Result<DeliveryPackage> {
    join_records_with_diagnostics_and_catalog(
        document_namespace,
        records,
        scene,
        source_sha256,
        extra_diagnostics,
        None,
        None,
        None,
    )
}

fn join_records_with_diagnostics_and_catalog(
    document_namespace: &str,
    records: Vec<native_document::Record>,
    scene: &native_saved_scene::SavedScene,
    source_sha256: Option<String>,
    extra_diagnostics: &[String],
    parameter_definitions: Option<Vec<ParameterDefinitionRow>>,
    parameter_bindings: Option<Vec<crate::native_parameter_definitions::Binding>>,
    category_registry: Option<&crate::native_parameter_definitions::Registry>,
) -> Result<DeliveryPackage> {
    ensure!(
        !document_namespace.trim().is_empty(),
        "document namespace is required"
    );
    let mut by_key = BTreeMap::<String, native_document::Record>::new();
    for record in records {
        ensure!(!record.identity.unique_id.is_empty(), "empty saved UID");
        let k = key(document_namespace, &record.identity.unique_id);
        ensure!(!by_key.contains_key(&k), "duplicate delivery identity {k}");
        by_key.insert(k, record);
    }
    let has_shared_definition_catalog = parameter_definitions.is_some();
    let parameter_definitions = match parameter_definitions {
        Some(parameter_definitions) => parameter_definitions,
        None => collect_parameter_definitions(by_key.values())?,
    };
    let has_shared_binding_catalog = parameter_bindings.is_some();
    let parameter_bindings = parameter_bindings.unwrap_or_default();
    let mut geometry = BTreeMap::new();
    for element in &scene.elements {
        ensure!(
            !element.identity.unique_id.is_empty(),
            "empty saved graphics UID"
        );
        ensure!(
            element.id == element.identity.element_id,
            "saved graphics numeric identity disagrees with element id"
        );
        let k = key(document_namespace, &element.identity.unique_id);
        ensure!(
            !geometry.contains_key(&k),
            "duplicate geometry identity {k}"
        );
        geometry.insert(k, element);
    }
    let all_scene_geometry = geometry.clone();
    let mut metric_keys = BTreeSet::new();
    metric_keys.extend(by_key.keys().cloned());
    metric_keys.extend(all_scene_geometry.keys().cloned());
    let mut metrics = Vec::with_capacity(metric_keys.len());
    for k in metric_keys {
        let record = by_key.get(&k);
        let graphics = all_scene_geometry.get(&k);
        let (
            primitive_count,
            renderable_primitive_count,
            vertex_count,
            triangle_count,
            estimated_geometry_bytes,
            diagnostic_count,
            unresolved_material_primitives,
        ) = graphics
            .map(|element| {
                let (vertices, triangles, bytes) = geometry_estimate(&element.meshes);
                (
                    element.meshes.primitives.len(),
                    element
                        .meshes
                        .primitives
                        .iter()
                        .filter(|p| renderable(p))
                        .count(),
                    vertices,
                    triangles,
                    bytes,
                    element.meshes.diagnostics.len()
                        + element.meshes.unbounded_faces
                        + element.meshes.empty_trim_faces
                        + element.meshes.rejected_filters,
                    element.unresolved_material_primitives.len(),
                )
            })
            .unwrap_or_default();
        let element_id = record
            .map(|record| record.identity.element_id)
            .or_else(|| graphics.map(|element| element.id))
            .unwrap_or_default();
        metrics.push(OwnerMetricsRow {
            element_key: k,
            element_id,
            class_name: record.and_then(|record| record.class_name.clone()),
            metadata_present: record.is_some(),
            metadata_body_bytes: record.map(|record| record.source.body_bytes),
            graphics_present: graphics.is_some(),
            graphics_body_bytes: graphics.and_then(|element| source_body_bytes(&element.source)),
            graphics_status: graphics.map(|element| element.status.clone()),
            geometry_retained: renderable_primitive_count > 0,
            primitive_count,
            renderable_primitive_count,
            vertex_count,
            triangle_count,
            estimated_geometry_bytes,
            diagnostic_count,
            unresolved_material_primitives,
        });
    }
    // Empty and unsupported graphics remain measurable in metrics, but are not
    // carried into the renderable geometry population or its package tables.
    geometry.retain(|_, element| element.meshes.primitives.iter().any(renderable));
    let mut uid_to_ids = BTreeMap::<String, BTreeSet<u64>>::new();
    let mut id_to_uids = BTreeMap::<u64, BTreeSet<String>>::new();
    for record in by_key.values() {
        uid_to_ids
            .entry(record.identity.unique_id.clone())
            .or_default()
            .insert(record.identity.element_id);
        id_to_uids
            .entry(record.identity.element_id)
            .or_default()
            .insert(record.identity.unique_id.clone());
    }
    for element in geometry.values() {
        uid_to_ids
            .entry(element.identity.unique_id.clone())
            .or_default()
            .insert(element.identity.element_id);
        id_to_uids
            .entry(element.identity.element_id)
            .or_default()
            .insert(element.identity.unique_id.clone());
    }
    ensure!(
        uid_to_ids.values().all(|ids| ids.len() <= 1),
        "same UID has conflicting numeric identities"
    );
    ensure!(
        id_to_uids.values().all(|uids| uids.len() <= 1),
        "numeric identity has conflicting UIDs"
    );
    let numeric_keys: BTreeMap<u64, String> = by_key
        .values()
        .map(|r| {
            (
                r.identity.element_id,
                key(document_namespace, &r.identity.unique_id),
            )
        })
        .chain(geometry.values().map(|e| {
            (
                e.identity.element_id,
                key(document_namespace, &e.identity.unique_id),
            )
        }))
        .collect();
    let mut all = BTreeSet::new();
    all.extend(by_key.keys().cloned());
    all.extend(geometry.keys().cloned());
    let mut elements = Vec::new();
    let mut rels = Vec::new();
    let mut types = BTreeMap::<i64, TypeRow>::new();
    let mut partial = 0;
    let mut metadata_only = 0;
    let mut nonrenderable_graphics = 0;
    let mut geometry_only = 0;
    for k in all {
        let record = by_key.get(&k);
        let mesh = geometry.get(&k);
        let graphics_receipt = all_scene_geometry.get(&k);
        let class = record.and_then(|r| r.class_name.clone());
        let source = record.map(|r| json!({"record":r.source,"channel":r.channel,"status":r.status,"diagnostic":r.diagnostic})).unwrap_or_else(|| json!({"source":"saved_scene"}));
        let metadata = record
            .and_then(|r| r.saved_metadata.as_ref())
            .map(|metadata| {
                compact_metadata(
                    metadata,
                    has_shared_definition_catalog,
                    has_shared_binding_catalog,
                )
            })
            .transpose()?;
        let metadata_complete = record.is_some_and(|r| {
            r.saved_metadata.is_some() && r.metadata_diagnostic.is_none() && r.diagnostic.is_none()
        });
        let status = match (record, mesh) {
            (Some(_), Some(g)) if metadata_complete && g.status == "decoded_saved_graphics" => {
                "complete_supported_subset"
            }
            (Some(_), Some(_)) => {
                partial += 1;
                "partial"
            }
            (Some(_), None) if graphics_receipt.is_some() => {
                nonrenderable_graphics += 1;
                "metadata_with_nonrenderable_graphics"
            }
            (Some(_), None) => {
                metadata_only += 1;
                "metadata_only"
            }
            (None, Some(_)) => {
                geometry_only += 1;
                "geometry_only"
            }
            _ => unreachable!(),
        };
        let unique = record
            .map(|r| r.identity.unique_id.clone())
            .or_else(|| mesh.map(|g| g.identity.unique_id.clone()))
            .unwrap_or_default();
        let eid = record
            .map(|r| r.identity.element_id)
            .or_else(|| mesh.map(|g| g.id))
            .unwrap_or_default();
        // Preserve the checked category recovery used by selection in the
        // delivered owner row as well. Direct metadata remains authoritative;
        // the definition-chain, room-scheme, and owner-class paths are
        // bounded provenance-backed recoveries, not a display-name guess.
        let category_id = match (record, category_registry) {
            (Some(record), Some(registry)) => root_category_id_with_definitions(record, registry)?
                .map(|(category_id, _)| category_id),
            _ => category_id(record)?,
        };
        let geometry_bounds_meters =
            graphics_receipt.and_then(|element| geometry_bounds_meters(element));
        let geometry_diagnostics = graphics_receipt.map(|element| {
            json!({
                "diagnostics": element.meshes.diagnostics,
                "unbounded_faces": element.meshes.unbounded_faces,
                "empty_trim_faces": element.meshes.empty_trim_faces,
                "rejected_filters": element.meshes.rejected_filters,
                "excluded_visibility_branches": element.meshes.excluded_visibility_branches,
                "excluded_non_surface_branches": element.meshes.excluded_non_surface_branches,
                "profile_observations": element.meshes.profile_observations,
                "unresolved_material_primitives": element.unresolved_material_primitives,
            })
        });
        let attributes = json!({"metadata":metadata,"metadata_diagnostic":record.and_then(|r| r.metadata_diagnostic.as_deref()),"identity":record.map(|r| &r.identity),"geometry_status":graphics_receipt.map(|g| &g.status),"geometry_detail_requested":graphics_receipt.map(|g| g.requested_detail_level),"geometry_detail_effective":graphics_receipt.map(|g| g.effective_detail_level),"geometry_detail_diagnostic":graphics_receipt.and_then(|g| g.detail_diagnostic.as_deref()),"geometry_diagnostics":geometry_diagnostics,"renderable_geometry_retained":mesh.is_some()});
        elements.push(ElementRow {
            key: k.clone(),
            document_namespace: document_namespace.into(),
            unique_id: unique,
            element_id: eid,
            class_name: class.clone(),
            category_id,
            category_label: category_id.and_then(crate::native_category_profile::category_label),
            geometry_bounds_meters,
            status: status.into(),
            source,
            attributes: attributes.clone(),
        });
        if let Some(r) = record {
            if let Some(m) = &r.saved_metadata {
                for field in &m.raw_field_parameters {
                    if field.source_field == "m_masterSymbolId"
                        && field.raw_value.as_i64().is_some_and(|id| id >= 0)
                    {
                        let type_id = field.raw_value.as_i64().unwrap();
                        if let Some(type_key) = numeric_keys.get(&(type_id as u64)) {
                            let type_record = by_key
                                .values()
                                .find(|candidate| candidate.identity.element_id == type_id as u64);
                            types.entry(type_id).or_insert(TypeRow {
                                key: type_key.clone(),
                                type_element_id: type_id,
                                class_name: type_record.and_then(|r| r.class_name.clone()),
                                source_element_key: type_key.clone(),
                                attributes: type_record
                                    .map(|r| {
                                        let metadata = r
                                            .saved_metadata
                                            .as_ref()
                                            .map(|metadata| {
                                                compact_metadata(
                                                    metadata,
                                                    has_shared_definition_catalog,
                                                    has_shared_binding_catalog,
                                                )
                                            })
                                            .transpose()?;
                                        Ok::<_, anyhow::Error>(
                                            json!({"source":r.source,"metadata":metadata}),
                                        )
                                    })
                                    .transpose()?
                                    .unwrap_or_else(|| json!({})),
                            });
                            rels.push(RelationshipRow {
                                from: k.clone(),
                                kind: "instance_of".into(),
                                to: type_key.clone(),
                                status: "saved_reference".into(),
                            });
                        } else {
                            rels.push(RelationshipRow {
                                from: k.clone(),
                                kind: "instance_of".into(),
                                to: format!("{document_namespace}:unresolved:{type_id}"),
                                status: "unresolved".into(),
                            });
                        }
                    }
                }
            }
        }
        if let Some(r) = record {
            if r.identity.owning_element_id >= 0 {
                let target = numeric_keys
                    .get(&(r.identity.owning_element_id as u64))
                    .cloned()
                    .unwrap_or_else(|| {
                        format!(
                            "{document_namespace}:unresolved:{:?}",
                            r.identity.owning_element_id
                        )
                    });
                rels.push(RelationshipRow {
                    from: k.clone(),
                    kind: "owned_by".into(),
                    to: target,
                    status: if numeric_keys.contains_key(&(r.identity.owning_element_id as u64)) {
                        "saved_reference"
                    } else {
                        "unresolved"
                    }
                    .into(),
                });
            }
            if let Some(m) = &r.saved_metadata {
                for (name, id) in &m.root_references {
                    let target = numeric_keys
                        .get(&(*id as u64))
                        .cloned()
                        .unwrap_or_else(|| format!("{document_namespace}:unresolved:{id}"));
                    rels.push(RelationshipRow {
                        from: k.clone(),
                        kind: name.clone(),
                        to: target,
                        status: if numeric_keys.contains_key(&(*id as u64)) {
                            "saved_reference"
                        } else {
                            "unresolved"
                        }
                        .into(),
                    });
                }
            }
        }
    }
    elements.sort_by(|a, b| a.key.cmp(&b.key));
    rels.sort_by(|a, b| (&a.from, &a.kind, &a.to).cmp(&(&b.from, &b.kind, &b.to)));
    let geometry_instances = geometry.len();
    let geometry = geometry
        .into_iter()
        .map(|(k, e)| -> Result<_> {
            let mut meshes = e.meshes.clone();
            meshes.primitives.retain(renderable);
            let mut points = Vec::new();
            for primitive in &mut meshes.primitives {
                for vertex in &mut primitive.vertices {
                    for coordinate in &mut *vertex {
                        *coordinate *= 0.3048;
                    }
                    points.push(*vertex);
                }
            }
            let rebased = if points.is_empty() {
                [0.; 3]
            } else {
                native_delivery_spatial::rebase_around_bounds(&points)?.origin
            };
            for primitive in &mut meshes.primitives {
                for vertex in &mut primitive.vertices {
                    for i in 0..3 {
                        vertex[i] -= rebased[i];
                    }
                }
            }
            Ok(GeometryInstance {
                element_key: k,
                status: e.status.clone(),
                source_frame: "document internal feet, rebased to package meters/Z-up".into(),
                local_origin_meters: rebased,
                meshes,
                render_materials: e.render_materials.clone(),
                unresolved_material_primitives: e.unresolved_material_primitives.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut mesh_rows = Vec::new();
    let mut instance_rows = Vec::new();
    let mut material_rows = Vec::new();
    for instance in &geometry {
        let mut mesh_keys = Vec::new();
        for (primitive_index, primitive) in instance.meshes.primitives.iter().enumerate() {
            let mesh_key = format!("{}:mesh:{}", instance.element_key, primitive_index);
            mesh_keys.push(mesh_key.clone());
            mesh_rows.push(MeshRow {
                key: mesh_key.clone(),
                element_key: instance.element_key.clone(),
                primitive_index,
                source_owner_id: primitive.source_owner_id,
                graphics_object_index: primitive.object_index,
                face_tag: primitive.face_tag,
                render_style_id: primitive.render_style_id,
                explicit_material_id: primitive.material_id,
                vertex_count: primitive.vertices.len(),
                triangle_count: primitive.triangles.len(),
                status: if instance
                    .unresolved_material_primitives
                    .contains(&primitive_index)
                {
                    "material_unresolved"
                } else if primitive.vertices.is_empty() || primitive.triangles.is_empty() {
                    "empty"
                } else {
                    "decoded"
                }
                .into(),
            });
            if let Some(material) = instance.render_materials.get(&primitive_index) {
                material_rows.push(MaterialRow {
                    key: format!("{}:material:{}", instance.element_key, primitive_index),
                    element_key: instance.element_key.clone(),
                    primitive_index,
                    material: material.clone(),
                });
            }
        }
        instance_rows.push(InstanceRow {
            key: format!("{}:instance", instance.element_key),
            element_key: instance.element_key.clone(),
            source_frame: instance.source_frame.clone(),
            local_origin_meters: instance.local_origin_meters,
            mesh_keys,
            status: instance.status.clone(),
        });
    }
    mesh_rows.sort_by(|a, b| a.key.cmp(&b.key));
    instance_rows.sort_by(|a, b| a.key.cmp(&b.key));
    material_rows.sort_by(|a, b| a.key.cmp(&b.key));
    let mut diagnostics = Vec::new();
    diagnostics.extend(extra_diagnostics.iter().cloned());
    // Keep the package manifest a bounded coverage summary. Per-owner material
    // and graph diagnostics already live on their respective rows (and audit
    // receipts when requested); copying their keys and IDs here made a tiles
    // manifest grow with the number of failures, even though tiles does not
    // write the material table at all.
    diagnostics.extend(scene.summaries.iter().flat_map(|s| {
        s.classes.iter().flat_map(|(class, coverage)| {
            let class = class.clone();
            coverage.diagnostics.iter().map(move |(reason, count)| {
                format!("refused_graph_class:{class}:count={count}:{reason}")
            })
        })
    }));
    let unresolved_material_primitives: usize = geometry
        .iter()
        .map(|instance| instance.unresolved_material_primitives.len())
        .sum();
    let coverage = Coverage {
        records: elements.len(),
        metadata_rows: elements.len(),
        geometry_instances,
        mesh_rows: mesh_rows.len(),
        instance_rows: instance_rows.len(),
        material_rows: material_rows.len(),
        saved_filling_rows: 0,
        unresolved_material_primitives,
        partial_records: partial,
        metadata_only_records: metadata_only,
        nonrenderable_graphics_records: nonrenderable_graphics,
        geometry_only_records: geometry_only,
        renderable_geometry_instances: geometry_instances,
        renderable_mesh_rows: mesh_rows.len(),
        renderable_vertices: geometry
            .iter()
            .flat_map(|instance| instance.meshes.primitives.iter())
            .filter(|primitive| renderable(primitive))
            .map(|primitive| primitive.vertices.len())
            .sum(),
        renderable_triangles: geometry
            .iter()
            .flat_map(|instance| instance.meshes.primitives.iter())
            .filter(|primitive| renderable(primitive))
            .map(|primitive| primitive.triangles.len())
            .sum(),
        authored_detail_downgrade_records: all_scene_geometry
            .values()
            .filter(|element| element.effective_detail_level < element.requested_detail_level)
            .count(),
        empty_geometry_owners: metrics
            .iter()
            .filter(|metric| metric.graphics_present && !metric.geometry_retained)
            .count(),
        unique_mesh_assets: geometry
            .iter()
            .flat_map(|instance| instance.meshes.primitives.iter())
            .filter(|primitive| renderable(primitive))
            .map(primitive_asset_key)
            .collect::<BTreeSet<_>>()
            .len(),
        adaptive_graph_retries: scene
            .summaries
            .iter()
            .map(|summary| summary.adaptive_graph_retries)
            .sum(),
        adaptive_graph_recoveries: scene
            .summaries
            .iter()
            .map(|summary| summary.adaptive_graph_recoveries)
            .sum(),
        diagnostics,
    };
    let status = if coverage.records == 0 {
        "empty"
    } else if coverage.partial_records > 0
        || coverage.metadata_only_records > 0
        || coverage.nonrenderable_graphics_records > 0
        || coverage.geometry_only_records > 0
        || coverage.unresolved_material_primitives > 0
        || !coverage.diagnostics.is_empty()
    {
        "partial"
    } else {
        "complete_supported_subset"
    };
    Ok(DeliveryPackage {
        manifest: DeliveryManifest {
            format: "rvt-native-delivery-v2",
            profile: DeliveryProfile::Tiles.manifest_name(),
            document_namespace: document_namespace.into(),
            source_sha256,
            status: status.into(),
            units: "meters",
            coordinate_frame: "package local meters Z-up",
            parameter_definitions_uri: None,
            parameter_bindings_uri: None,
            coverage,
            tables: DeliveryProfile::Tiles.tables(),
        },
        elements,
        parameter_definitions,
        parameter_bindings,
        types: types.into_values().collect(),
        relationships: rels,
        meshes: mesh_rows,
        instances: instance_rows,
        materials: material_rows,
        geometry,
        spatial_context: None,
        spatial_boundaries: None,
        room_connections: None,
        network: None,
        texture_mappings: None,
        metrics,
    })
}

impl DeliveryPackage {
    fn apply_profile(&mut self, profile: DeliveryProfile) {
        self.manifest.profile = profile.manifest_name();
        self.manifest.tables = profile.tables();
    }
}

pub fn extract(file: &mut RevitFile, options: &DeliveryOptions) -> Result<DeliveryPackage> {
    let physical_index = native_document::build_physical_index(file, &options.native)?;
    extract_using_index(file, options, &physical_index)
}

/// Extract one delivery package using a caller-owned validated physical index.
///
/// Keeping the index outside this function is important for sharded delivery:
/// validation and stream discovery happen once, while each shard decodes only
/// its selected current owners and the saved-graphics/material closure they
/// require.
pub fn extract_using_index(
    file: &mut RevitFile,
    options: &DeliveryOptions,
    physical_index: &native_document::PhysicalIndex,
) -> Result<DeliveryPackage> {
    let definition_context = build_definition_context(file, options, physical_index)?;
    extract_using_index_internal(file, options, physical_index, &definition_context)
}

/// Extract one package from a native category profile without re-reading
/// selected metadata after the profile scan.
pub fn extract_category_selected(
    file: &mut RevitFile,
    options: &DeliveryOptions,
    category_ids: BTreeSet<i64>,
) -> Result<(DeliveryPackage, CategorySelectionReceipt)> {
    let physical_index = native_document::build_physical_index(file, &options.native)?;
    let definition_context = build_definition_context(file, options, &physical_index)?;
    let prepared = prepare_category_selection(
        file,
        &options.native,
        &physical_index,
        &definition_context,
        category_ids,
    )?;
    let receipt = prepared.receipt.clone();
    let selected = prepared.selected_ids();
    let mut selected_options = options.clone();
    selected_options.native.selected_ids = selected.clone();
    selected_options.metadata_ids = Some(selected);
    let mut scene_cache = native_saved_scene::ExtractionCache::default();
    let package = extract_using_index_internal_with_cache_and_records(
        file,
        &selected_options,
        &physical_index,
        &definition_context,
        &mut scene_cache,
        None,
    )?;
    Ok((package, receipt))
}

fn extract_using_index_internal(
    file: &mut RevitFile,
    options: &DeliveryOptions,
    physical_index: &native_document::PhysicalIndex,
    definition_context: &native_document::DefinitionContext,
) -> Result<DeliveryPackage> {
    let mut scene_cache = native_saved_scene::ExtractionCache::default();
    extract_using_index_internal_with_cache(
        file,
        options,
        physical_index,
        definition_context,
        &mut scene_cache,
    )
}

fn extract_using_index_internal_with_cache(
    file: &mut RevitFile,
    options: &DeliveryOptions,
    physical_index: &native_document::PhysicalIndex,
    definition_context: &native_document::DefinitionContext,
    scene_cache: &mut native_saved_scene::ExtractionCache,
) -> Result<DeliveryPackage> {
    extract_using_index_internal_with_cache_and_records(
        file,
        options,
        physical_index,
        definition_context,
        scene_cache,
        None,
    )
}

/// Build a delivery package from a caller-owned prepared metadata selection.
/// The records must have been projected from this document/context already;
/// this function intentionally does not decode metadata a second time.
fn extract_using_index_internal_with_cache_and_records(
    file: &mut RevitFile,
    options: &DeliveryOptions,
    physical_index: &native_document::PhysicalIndex,
    definition_context: &native_document::DefinitionContext,
    scene_cache: &mut native_saved_scene::ExtractionCache,
    prepared_records: Option<Vec<native_document::Record>>,
) -> Result<DeliveryPackage> {
    // Recover selected current metadata once, before graphics traversal. The
    // same records both feed package projection and qualify owner-specific
    // saved-graphics detail decisions; channel-103 graphics cannot be relied
    // upon to carry the channel-102 class/category identity.
    let records = match prepared_records {
        Some(records) => records,
        None => {
            let mut metadata_options = options.native.clone();
            metadata_options.channels = BTreeSet::from([102]);
            metadata_options.selected_ids = options
                .metadata_ids
                .clone()
                .unwrap_or_else(|| options.native.selected_ids.clone());
            let mut records = Vec::new();
            native_document::extract_using_index_with_context(
                file,
                &metadata_options,
                physical_index,
                &definition_context,
                false,
                |r| {
                    records.push(r);
                    Ok(())
                },
            )?;
            records
        }
    };
    let owner_detail_hints = records
        .iter()
        .map(|record| {
            Ok((
                record.identity.element_id,
                native_saved_scene::OwnerDetailHint {
                    class_name: record.class_name.clone(),
                    category_id: category_id(Some(record))?,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut scene_diagnostics = Vec::new();
    let scene = match native_saved_scene::extract_at_detail_using_index_with_cache_and_catalog(
        file,
        &options.native,
        options.scene_detail,
        &physical_index,
        scene_cache,
        definition_context.extensible_storage_catalog.as_ref(),
        Some(&owner_detail_hints),
    ) {
        Ok(scene) => scene,
        Err(error) => {
            scene_diagnostics.push(format!("saved_scene:{error:#}"));
            native_saved_scene::SavedScene {
                source_sha256: None,
                format: "rvt-native-saved-scene-v1",
                units: "feet",
                coordinate_frame: "document Z-up",
                complete_document_geometry: false,
                detail_level: options.scene_detail,
                graphics_profile: "unavailable",
                summaries: vec![],
                elements: vec![],
            }
        }
    };
    // The returned scene owns the projected meshes/materials needed by the
    // package. The cache still owns decoded native graphs and resolver
    // provenance; keeping both alive through metadata projection and package
    // assembly makes the peak proportional to two representations of a heavy
    // shard. Release the decode state at this lifetime boundary.
    scene_cache.release_completed_selection();
    let mut projection_diagnostics = Vec::new();
    let spatial_inventory = options.profile.emits_rich_tables().then(|| {
        let mut context = crate::native_spatial_context::InventoryBuilder::default();
        let mut boundaries = crate::native_spatial_boundaries::InventoryBuilder::default();
        let mut lifecycle = crate::native_lifecycle::InventoryBuilder::default();
        let mut room_connections = crate::native_room_connections::InventoryBuilder::default();
        let mut network = crate::native_network::InventoryBuilder::default();
        for record in &records {
            if let Err(error) = context.ingest(record) {
                projection_diagnostics.push(format!(
                    "spatial_context:{}:{error:#}",
                    record.identity.unique_id
                ));
            }
            if let Err(error) = boundaries.ingest(record) {
                projection_diagnostics.push(format!(
                    "spatial_boundaries:{}:{error:#}",
                    record.identity.unique_id
                ));
            }
            if let Err(error) = lifecycle.ingest(record) {
                projection_diagnostics
                    .push(format!("lifecycle:{}:{error:#}", record.identity.unique_id));
            }
            if let Err(error) = room_connections.ingest(record) {
                projection_diagnostics.push(format!(
                    "room_connections:{}:{error:#}",
                    record.identity.unique_id
                ));
            }
            if let Err(error) = network.ingest(record) {
                projection_diagnostics
                    .push(format!("network:{}:{error:#}", record.identity.unique_id));
            }
        }
        (context, boundaries, lifecycle, room_connections, network)
    });
    // Use the one document-wide registry directly. Re-collecting equivalent
    // definitions from every selected owner would reintroduce CPU work even
    // after their serialized copies were removed.
    let mut package = join_records_with_diagnostics_and_catalog(
        &options.document_namespace,
        records,
        &scene,
        options.source_sha256.clone(),
        &scene_diagnostics,
        Some(parameter_definitions_from_registry(
            &definition_context.registry,
        )?),
        Some(
            definition_context
                .registry
                .bindings
                .values()
                .cloned()
                .collect(),
        ),
        Some(&definition_context.registry),
    )?;
    package.apply_profile(options.profile);
    if let Some((context, boundaries, lifecycle, room_connections, network)) = spatial_inventory {
        let spatial = match context.finish() {
            Ok(value) => {
                package.spatial_context = Some(serde_json::to_value(&value)?);
                Some(value)
            }
            Err(error) => {
                projection_diagnostics.push(format!("spatial_context_finish:{error:#}"));
                None
            }
        };
        let boundary_inventory = match boundaries.finish() {
            Ok(value) => {
                let mut qualified_space_volumes = Vec::new();
                for room in &value.rooms {
                    match native_delivery_spatial::derive_space(room, &value.level_elevations) {
                        Ok(space) => qualified_space_volumes.push(serde_json::to_value(space)?),
                        Err(error) => projection_diagnostics
                            .push(format!("space_volume:{}:{error:#}", room.owner.element_id)),
                    }
                }
                package.spatial_boundaries = Some(json!({
                    "inventory": serde_json::to_value(&value)?,
                    "qualified_space_volumes": qualified_space_volumes,
                    "qualification_semantics": "only evaluated Finish boundaries with explicit vertical references are emitted as volumes",
                }));
                Some(value)
            }
            Err(error) => {
                projection_diagnostics.push(format!("spatial_boundaries_finish:{error:#}"));
                None
            }
        };
        if let (Some(spatial), Some(boundaries)) = (spatial.as_ref(), boundary_inventory.as_ref()) {
            match lifecycle.finish() {
                Ok(lifecycle) => match room_connections.finish(spatial, &lifecycle, boundaries) {
                    Ok(value) => package.room_connections = Some(serde_json::to_value(value)?),
                    Err(error) => {
                        projection_diagnostics.push(format!("room_connections_finish:{error:#}"))
                    }
                },
                Err(error) => projection_diagnostics.push(format!("lifecycle_finish:{error:#}")),
            }
        }
        match network.finish() {
            Ok(value) => package.network = Some(serde_json::to_value(value)?),
            Err(error) => projection_diagnostics.push(format!("network_finish:{error:#}")),
        }
    }
    if options.profile.emits_audit_tables() {
        let texture_ids = options.metadata_ids.clone().or_else(|| {
            (!options.native.selected_ids.is_empty()).then(|| options.native.selected_ids.clone())
        });
        if let Some(ids) = texture_ids.filter(|ids| !ids.is_empty()) {
            match crate::native_texture_mapping::read_using_index(file, ids, physical_index) {
                Ok(value) => {
                    package.manifest.coverage.saved_filling_rows = value.saved_fillings.len();
                    if !value.diagnostics.is_empty() {
                        projection_diagnostics.extend(
                            value
                                .diagnostics
                                .iter()
                                .map(|diagnostic| format!("texture_mapping:{diagnostic}")),
                        );
                    }
                    package.texture_mappings = Some(serde_json::to_value(value)?);
                }
                Err(error) => projection_diagnostics.push(format!("texture_mapping:{error:#}")),
            }
        }
    }
    package
        .manifest
        .coverage
        .diagnostics
        .extend(projection_diagnostics);
    if !package.manifest.coverage.diagnostics.is_empty() {
        package.manifest.status = "partial".into();
    }
    Ok(package)
}

/// Write a bounded, deterministic collection of ordinary delivery packages.
///
/// The top-level directory is an index only; every `shard-XXXXXX` child is a
/// normal `rvt-native-delivery-v2` package and can be consumed independently.
/// Sharding is by validated current channel-102 element id order. Explicit
/// selections retain absent ids in their shard so the normal extraction
/// summary reports them instead of silently dropping the request.
pub fn write_sharded_package_dirs(
    file: &mut RevitFile,
    options: &DeliveryOptions,
    output: &Path,
    shard_size: usize,
) -> Result<()> {
    ensure!(shard_size > 0, "shard size must be positive");
    ensure!(!output.exists(), "output directory exists");
    let physical_index = native_document::build_physical_index(file, &options.native)?;
    let requested = current_requested_ids(options, &physical_index);
    let shard_ids = requested
        .chunks(shard_size)
        .map(|ids| ids.to_vec())
        .collect::<Vec<_>>();
    write_sharded_package_dirs_with_selected_ids(
        file,
        options,
        output,
        shard_size,
        &physical_index,
        shard_ids,
    )
}

/// Write caller-planned shards through one open RVT reader. This is the
/// production path for cost-bounded delivery: physical indexing, definition
/// discovery and CFB/DEFLATE state are retained once, while each completed
/// package is dropped before the next selection is decoded.
pub fn write_sharded_package_dirs_with_selected_ids(
    file: &mut RevitFile,
    options: &DeliveryOptions,
    output: &Path,
    shard_size: usize,
    physical_index: &native_document::PhysicalIndex,
    shard_ids: Vec<Vec<u64>>,
) -> Result<()> {
    let definition_context = build_definition_context(file, options, physical_index)?;
    let mut scene_cache = native_saved_scene::ExtractionCache::default();
    let parameter_definitions = parameter_definitions_from_registry(&definition_context.registry)?;
    let parameter_bindings = definition_context
        .registry
        .bindings
        .values()
        .cloned()
        .collect();
    write_sharded_package_dirs_with_index(
        options,
        output,
        shard_size,
        shard_ids,
        parameter_definitions,
        parameter_bindings,
        None,
        |ids| {
            let mut shard_options = options.clone();
            let selected = ids.iter().copied().collect::<BTreeSet<_>>();
            shard_options.native.selected_ids = selected.clone();
            shard_options.metadata_ids = Some(selected);
            let package = extract_using_index_internal_with_cache(
                file,
                &shard_options,
                physical_index,
                &definition_context,
                &mut scene_cache,
            )?;
            scene_cache.release_completed_selection();
            Ok(package)
        },
    )
}

/// Produce bounded category-selected shards without a second metadata decode.
/// A document-wide metadata projection determines the selected IDs once; each
/// retained graph is released immediately, then its saved graphics are decoded
/// only for the shard that will persist it.
pub fn write_sharded_category_package_dirs_with_selected_ids(
    file: &mut RevitFile,
    options: &DeliveryOptions,
    output: &Path,
    shard_size: usize,
    physical_index: &native_document::PhysicalIndex,
    shard_ids: Vec<Vec<u64>>,
    category_ids: BTreeSet<i64>,
) -> Result<CategorySelectionReceipt> {
    let definition_context = build_definition_context(file, options, physical_index)?;
    let mut prepared = prepare_category_selection(
        file,
        &options.native,
        physical_index,
        &definition_context,
        category_ids,
    )?;
    write_sharded_prepared_category_package_dirs_with_selected_ids(
        file,
        options,
        output,
        shard_size,
        physical_index,
        &definition_context,
        &mut prepared,
        shard_ids,
    )
}

/// Consume a caller-prepared selection.  This is public so a CLI can make a
/// byte-cost-aware shard plan from `selected_ids()` without performing a
/// second category/metadata scan.
pub fn write_sharded_prepared_category_package_dirs_with_selected_ids(
    file: &mut RevitFile,
    options: &DeliveryOptions,
    output: &Path,
    shard_size: usize,
    physical_index: &native_document::PhysicalIndex,
    definition_context: &native_document::DefinitionContext,
    prepared: &mut PreparedCategorySelection,
    shard_ids: Vec<Vec<u64>>,
) -> Result<CategorySelectionReceipt> {
    let receipt = prepared.receipt.clone();
    let selected_ids = prepared.selected_ids();
    ensure!(
        shard_ids
            .iter()
            .flatten()
            .all(|id| selected_ids.contains(id)),
        "category shard plan contains an unselected native record"
    );
    ensure!(
        shard_ids.iter().flatten().count() == selected_ids.len(),
        "category shard plan does not cover the prepared native selection exactly once"
    );
    let mut scene_cache = native_saved_scene::ExtractionCache::default();
    let parameter_definitions = parameter_definitions_from_registry(&definition_context.registry)?;
    let parameter_bindings = definition_context
        .registry
        .bindings
        .values()
        .cloned()
        .collect();
    write_sharded_package_dirs_with_index(
        options,
        output,
        shard_size,
        shard_ids,
        parameter_definitions,
        parameter_bindings,
        Some(receipt.clone()),
        |ids| {
            let selected = ids.iter().copied().collect::<BTreeSet<_>>();
            let mut shard_options = options.clone();
            shard_options.native.selected_ids = selected.clone();
            shard_options.metadata_ids = Some(selected);
            let package = extract_using_index_internal_with_cache_and_records(
                file,
                &shard_options,
                physical_index,
                definition_context,
                &mut scene_cache,
                None,
            )?;
            scene_cache.release_completed_selection();
            Ok(package)
        },
    )?;
    Ok(receipt)
}

/// Write sharded delivery while opening a fresh RVT reader for every shard.
///
/// This is the hard memory-isolation variant for large files. The physical
/// index is built once, but the CFB/DEFLATE reader and all decoded projections
/// are discarded between shards instead of relying on allocator reuse.
pub fn write_sharded_package_dirs_from_path(
    file_path: &Path,
    options: &DeliveryOptions,
    output: &Path,
    shard_size: usize,
) -> Result<()> {
    ensure!(shard_size > 0, "shard size must be positive");
    ensure!(!output.exists(), "output directory exists");
    let mut index_file = RevitFile::open(file_path)?;
    let physical_index = native_document::build_physical_index(&mut index_file, &options.native)?;
    let definition_context = build_definition_context(&mut index_file, options, &physical_index)?;
    let requested = current_requested_ids(options, &physical_index);
    let shard_ids = requested
        .chunks(shard_size)
        .map(|ids| ids.to_vec())
        .collect::<Vec<_>>();
    let parameter_definitions = parameter_definitions_from_registry(&definition_context.registry)?;
    let parameter_bindings = definition_context
        .registry
        .bindings
        .values()
        .cloned()
        .collect();
    write_sharded_package_dirs_with_index(
        options,
        output,
        shard_size,
        shard_ids,
        parameter_definitions,
        parameter_bindings,
        None,
        |ids| {
            let mut shard_file = RevitFile::open(file_path)?;
            let mut shard_options = options.clone();
            let selected = ids.iter().copied().collect::<BTreeSet<_>>();
            shard_options.native.selected_ids = selected.clone();
            shard_options.metadata_ids = Some(selected);
            extract_using_index_internal(
                &mut shard_file,
                &shard_options,
                &physical_index,
                &definition_context,
            )
        },
    )
}

fn current_requested_ids(
    options: &DeliveryOptions,
    physical_index: &native_document::PhysicalIndex,
) -> Vec<u64> {
    if let Some(ids) = &options.metadata_ids {
        ids.iter().copied().collect()
    } else if !options.native.selected_ids.is_empty() {
        options.native.selected_ids.iter().copied().collect()
    } else {
        physical_index.current_ids(102).into_iter().collect()
    }
}

fn write_sharded_package_dirs_with_index(
    options: &DeliveryOptions,
    output: &Path,
    shard_size: usize,
    shard_ids: Vec<Vec<u64>>,
    parameter_definitions: Vec<ParameterDefinitionRow>,
    parameter_bindings: Vec<crate::native_parameter_definitions::Binding>,
    category_selection: Option<CategorySelectionReceipt>,
    mut extract_package: impl FnMut(&[u64]) -> Result<DeliveryPackage>,
) -> Result<()> {
    ensure!(!output.exists(), "output directory exists");
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".{}.staging",
        output
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("rvt-package")
    ));
    ensure!(!staging.exists(), "staging directory exists");
    fs::create_dir(&staging)?;
    let result = (|| -> Result<()> {
        write_jsonl(
            &staging.join("parameter-definitions.jsonl"),
            parameter_definitions.iter(),
        )?;
        write_jsonl(
            &staging.join("parameter-bindings.jsonl"),
            parameter_bindings.iter(),
        )?;
        let mut shards = Vec::new();
        let mut tile_children = Vec::<(String, Value)>::new();
        for (shard_number, ids) in shard_ids.iter().enumerate() {
            let started = Instant::now();
            let uri = format!("shard-{shard_number:06}");
            let shard_output = staging.join(&uri);
            let package = extract_package(ids)?;
            let coverage = package.manifest.coverage.clone();
            let tile_root = if has_renderable_geometry(&package) {
                Some(native_delivery_tiles::bounding_volume(&package)?)
            } else {
                None
            };
            let tileset_uri = tile_root.as_ref().map(|_| format!("{uri}/tileset.json"));
            write_package_dir_with_external_parameter_catalogs(
                &package,
                &shard_output,
                Some((
                    "../parameter-definitions.jsonl",
                    "../parameter-bindings.jsonl",
                )),
            )?;
            if let Some(bounding_volume) = tile_root {
                tile_children.push((uri.clone(), bounding_volume));
            }
            let glb_bytes = fs::metadata(shard_output.join("content.glb"))
                .ok()
                .map(|metadata| metadata.len());
            let package_bytes = Some(directory_bytes(&shard_output)?);
            let telemetry = ShardTelemetry {
                version: 1,
                elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                renderable_vertices: coverage.renderable_vertices as u64,
                renderable_triangles: coverage.renderable_triangles as u64,
                glb_bytes,
                package_bytes,
                peak_rss_bytes: None,
            };
            shards.push(ShardEntry {
                tileset_uri,
                uri,
                first_element_id: ids.first().copied(),
                last_element_id: ids.last().copied(),
                requested_ids: ids.len(),
                coverage,
                telemetry: Some(telemetry),
            });
        }
        let status = if shards.is_empty() {
            "empty"
        } else if shards.iter().any(|shard| {
            !shard.coverage.diagnostics.is_empty()
                || shard.coverage.partial_records > 0
                || shard.coverage.metadata_only_records > 0
                || shard.coverage.nonrenderable_graphics_records > 0
                || shard.coverage.geometry_only_records > 0
                || shard.coverage.unresolved_material_primitives > 0
        }) {
            "partial"
        } else {
            "complete_supported_subset"
        };
        let selection_semantics = if category_selection.is_some() {
            "shards are deterministic selected native BuiltInCategory id ranges; every selected owner has one prepared metadata row"
        } else {
            "shards are deterministic current channel-102 id ranges; each child is a normal delivery package"
        };
        let selected_ids = if let Some(receipt) = category_selection.as_ref() {
            let mut ids = shard_ids.iter().flatten().copied().collect::<Vec<_>>();
            ids.sort_unstable();
            ids.dedup();
            ensure!(
                ids.len() == receipt.selected_records,
                "category shard manifest selected-id count disagrees with receipt"
            );
            Some(ids)
        } else {
            None
        };
        let manifest = ShardedDeliveryManifest {
            format: "rvt-native-delivery-sharded-v2",
            document_namespace: options.document_namespace.clone(),
            source_sha256: options.source_sha256.clone(),
            status: status.into(),
            shard_size,
            shard_count: shards.len(),
            tileset_uri: (!tile_children.is_empty()).then_some("tileset.json"),
            coordinate_frame: "each shard uses package local meters Z-up",
            parameter_definitions_uri: "parameter-definitions.jsonl",
            parameter_bindings_uri: "parameter-bindings.jsonl",
            category_selection,
            selected_ids,
            selection_semantics,
            shards,
        };
        fs::write(
            staging.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        if !tile_children.is_empty() {
            let boxes = tile_children
                .iter()
                .map(|(_, bounding_volume)| bounding_volume["box"].clone())
                .collect::<Vec<_>>();
            let root_box = union_tileset_boxes(&boxes)?;
            let children = tile_children
                .into_iter()
                .map(|(uri, bounding_volume)| {
                    json!({
                        "boundingVolume": bounding_volume,
                        "geometricError": 0,
                        "content": {"uri": format!("{uri}/tileset.json")}
                    })
                })
                .collect::<Vec<_>>();
            let tileset = json!({
                "asset": {
                    "version": "1.1",
                    "extras": {
                        "lod": "exact external-child tilesets; geometricError 0; no simplification",
                        "manifest_uri": "manifest.json"
                    }
                },
                "geometricError": 0,
                "root": {
                    "boundingVolume": {"box": root_box},
                    "geometricError": 0,
                    "refine": "ADD",
                    "children": children
                }
            });
            fs::write(
                staging.join("tileset.json"),
                serde_json::to_vec_pretty(&tileset)?,
            )?;
        }
        fs::rename(&staging, output)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn has_renderable_geometry(package: &DeliveryPackage) -> bool {
    package.geometry.iter().any(|owner| {
        owner
            .meshes
            .primitives
            .iter()
            .any(|primitive| !primitive.vertices.is_empty() && !primitive.triangles.is_empty())
    })
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

fn union_tileset_boxes(boxes: &[Value]) -> Result<[f64; 12]> {
    ensure!(!boxes.is_empty(), "cannot union empty tile bounds");
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for value in boxes {
        let values = value.as_array().context("tile box array")?;
        ensure!(values.len() == 12, "tile box length");
        let numbers = values
            .iter()
            .map(|value| value.as_f64().context("tile box number"))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            numbers.iter().all(|value| value.is_finite())
                && numbers[4].abs() < 1e-12
                && numbers[5].abs() < 1e-12
                && numbers[6].abs() < 1e-12
                && numbers[8].abs() < 1e-12
                && numbers[9].abs() < 1e-12
                && numbers[10].abs() < 1e-12,
            "aggregate tile box is not axis aligned"
        );
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

fn write_json_value(path: &Path, value: &impl Serialize, pretty: bool) -> Result<()> {
    let file = fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    if pretty {
        serde_json::to_writer_pretty(&mut writer, value)?;
    } else {
        serde_json::to_writer(&mut writer, value)?;
    }
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn write_json_array<T: Serialize>(path: &Path, values: &[T]) -> Result<()> {
    let file = fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    writer.write_all(b"[")?;
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            writer.write_all(b",")?;
        }
        serde_json::to_writer(&mut writer, value)?;
    }
    writer.write_all(b"]\n")?;
    writer.flush()?;
    Ok(())
}

fn write_jsonl<T: Serialize>(path: &Path, values: impl IntoIterator<Item = T>) -> Result<()> {
    let file = fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    for value in values {
        serde_json::to_writer(&mut writer, &value)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

pub fn write_package_dir(package: &DeliveryPackage, output: &Path) -> Result<()> {
    write_package_dir_with_external_parameter_catalogs(package, output, None)
}

fn write_package_dir_with_external_parameter_catalogs(
    package: &DeliveryPackage,
    output: &Path,
    parameter_catalog_uris: Option<(&str, &str)>,
) -> Result<()> {
    ensure!(!output.exists(), "output directory exists");
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".{}.staging",
        output
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("rvt-package")
    ));
    ensure!(!staging.exists(), "staging directory exists");
    fs::create_dir(&staging)?;
    let result = (|| -> Result<()> {
        let mut manifest = package.manifest.clone();
        if let Some((definitions_uri, bindings_uri)) = parameter_catalog_uris {
            manifest.parameter_definitions_uri = Some(definitions_uri.into());
            manifest.parameter_bindings_uri = Some(bindings_uri.into());
            manifest
                .tables
                .retain(|table| !matches!(*table, "parameter_definitions" | "parameter_bindings"));
        }
        write_json_value(&staging.join("manifest.json"), &manifest, true)?;
        write_json_array(&staging.join("elements.json"), &package.elements)?;
        if parameter_catalog_uris.is_none() {
            write_jsonl(
                &staging.join("parameter-definitions.jsonl"),
                package.parameter_definitions.iter(),
            )?;
            write_jsonl(
                &staging.join("parameter-bindings.jsonl"),
                package.parameter_bindings.iter(),
            )?;
        }
        let profile = match manifest.profile {
            "tiles" => DeliveryProfile::Tiles,
            "rich-tiles" => DeliveryProfile::RichTiles,
            "audit" => DeliveryProfile::Audit,
            value => anyhow::bail!("unknown delivery profile {value}"),
        };
        if profile.emits_rich_tables() {
            write_jsonl(&staging.join("types.jsonl"), package.types.iter())?;
            write_jsonl(
                &staging.join("relationships.jsonl"),
                package.relationships.iter(),
            )?;
            write_jsonl(&staging.join("materials.jsonl"), package.materials.iter())?;
            write_json_value(
                &staging.join("spatial-context.json"),
                &package.spatial_context,
                false,
            )?;
            write_json_value(
                &staging.join("spatial-boundaries.json"),
                &package.spatial_boundaries,
                false,
            )?;
            write_json_value(
                &staging.join("room-connections.json"),
                &package.room_connections,
                false,
            )?;
            write_json_value(&staging.join("network.json"), &package.network, false)?;
        }
        if profile.emits_audit_tables() {
            write_jsonl(&staging.join("metrics.jsonl"), package.metrics.iter())?;
            write_json_value(
                &staging.join("texture-mappings.json"),
                &package.texture_mappings,
                false,
            )?;
        }
        if has_renderable_geometry(package) {
            let tiles = native_delivery_tiles::build_tileset(package)?;
            write_json_value(&staging.join("tileset.json"), &tiles.tileset, true)?;
            for (uri, bytes) in tiles.contents {
                fs::write(staging.join(uri), bytes)?;
            }
        }
        fs::rename(&staging, output)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        native_document::{Record, RecordSource},
        native_index::Identity,
        native_parameters::{GraphObject, ObjectGraph},
        native_saved_mesh::{GraphicsMeshes, Primitive},
        native_saved_scene::SavedElement,
        native_segments::GroupSource,
    };

    fn identity(id: u64, uid: &str) -> Identity {
        Identity {
            element_id: id,
            original_id_suffix: id,
            creation_episode: 0,
            stored_revision: 0,
            other_revision: 0,
            row_offset: 0,
            raw_fields: vec![],
            owning_element_id: -1,
            partition_id: 0,
            unique_id: uid.into(),
        }
    }
    fn record(id: u64, uid: &str) -> Record {
        Record {
            identity: identity(id, uid),
            derived_default_ifc_guid: None,
            derived_identifier_diagnostic: None,
            effective_ifc_parameter: None,
            effective_ifc_parameter_diagnostic: None,
            channel: 102,
            class_name: Some("FamilyInstance".into()),
            status: "decoded".into(),
            diagnostic: None,
            source: RecordSource {
                stream: "Partitions/0".into(),
                group: GroupSource {
                    content_key: None,
                    channel: 102,
                    first_marker_offset: 0,
                    segment_count: 1,
                    declared_objects: 1,
                    declared_body_bytes: 0,
                },
                group_record_offset: 0,
                body_bytes: 0,
                body_sha256: "".into(),
            },
            graph: None,
            saved_metadata: None,
            metadata_diagnostic: None,
        }
    }

    fn root_record(id: u64, class_name: &str, fields: Value) -> Record {
        let mut record = record(id, "uid");
        record.class_name = Some(class_name.into());
        record.graph = Some(ObjectGraph {
            consumed_bytes: 0,
            objects: vec![GraphObject {
                class_tag: 0,
                class_name: class_name.into(),
                token: 0,
                start: 0,
                fields_end: 0,
                fields,
            }],
            edges: vec![],
        });
        record
    }

    #[test]
    fn root_category_selection_uses_document_family_registry() {
        let mut registry = crate::native_parameter_definitions::Registry::default();
        registry.symbol_families.insert(20, 10);
        registry.family_categories.insert(10, -2_001_120);
        let instance = root_record(1, "FamilyInstance", json!({"m_masterSymbolId":20}));
        let symbol = root_record(2, "FamilySymbol", json!({"m_familyId":10}));
        let system_mullion_symbol =
            root_record(5, "SysMullionFamSym", json!({"m_familyId":10}));
		let system_panel_symbol =
			root_record(6, "SysPanelFamSym", json!({"m_familyId":10}));
        let direct = root_record(3, "FamilyInstance", json!({"m_categoryId":-2_001_140}));
        let pipe = root_record(4, "RbsPipeCurve", json!({"m_idType":362258}));
        let room = root_record(
            7,
            "RoomElem",
            json!({"m_zoneSchemeId":{"m_id":-1},"m_areaSchemeId":{"m_id":-1}}),
        );
        let space = root_record(
            8,
            "RoomElem",
            json!({"m_zoneSchemeId":{"m_id":700},"m_areaSchemeId":{"m_id":-1}}),
        );
        let area = root_record(
            9,
            "RoomElem",
            json!({"m_zoneSchemeId":{"m_id":-1},"m_areaSchemeId":{"m_id":701}}),
        );
        assert_eq!(
            root_category_id_with_definitions(&instance, &registry).unwrap(),
            Some((-2_001_120, "symbol_family_category_chain"))
        );
        assert_eq!(
            root_category_id_with_definitions(&symbol, &registry).unwrap(),
            Some((-2_001_120, "symbol_family_category_chain"))
        );
        assert_eq!(
            root_category_id_with_definitions(&system_mullion_symbol, &registry).unwrap(),
            Some((-2_001_120, "symbol_family_category_chain"))
        );
		assert_eq!(
			root_category_id_with_definitions(&system_panel_symbol, &registry).unwrap(),
			Some((-2_001_120, "symbol_family_category_chain"))
		);
        assert_eq!(
            root_category_id_with_definitions(&direct, &registry).unwrap(),
            Some((-2_001_140, "direct_root_category"))
        );
        assert_eq!(
            root_category_id_with_definitions(&pipe, &registry).unwrap(),
            Some((-2_008_044, "checked_owner_class_category"))
        );
        assert_eq!(
            root_category_id_with_definitions(&room, &registry).unwrap(),
            Some((-2_000_160, "checked_room_space_scheme_category"))
        );
        assert_eq!(
            root_category_id_with_definitions(&space, &registry).unwrap(),
            Some((-2_003_600, "checked_room_space_scheme_category"))
        );
        assert_eq!(root_category_id_with_definitions(&area, &registry).unwrap(), None);
        let definition_type = native_document::DefinitionOwnerCategoryCandidate {
            class_name: "RbsPipingSystemType".into(),
            direct_category_id: None,
            family_id: None,
        };
        assert_eq!(
            definition_owner_category_id(&definition_type, &registry).unwrap(),
            Some((-2_008_043, "definition_context_owner_class_category"))
        );
    }

    #[test]
    fn root_category_selection_rejects_conflicting_direct_and_inherited_values() {
        let mut registry = crate::native_parameter_definitions::Registry::default();
        registry.symbol_families.insert(20, 10);
        registry.family_categories.insert(10, -2_001_120);
        let record = root_record(
            1,
            "FamilyInstance",
            json!({"m_masterSymbolId":20,"m_categoryId":-2_001_140}),
        );
        assert!(root_category_id_with_definitions(&record, &registry).is_err());
    }
    fn scene_element(id: u64, uid: &str) -> SavedElement {
        SavedElement {
            id,
            identity: identity(id, uid),
            source: json!({}),
            referenced_graph_sources: BTreeMap::new(),
            status: "partial_saved_graphics".into(),
            requested_detail_level: 1,
            effective_detail_level: 1,
            detail_diagnostic: None,
            meshes: GraphicsMeshes::default(),
            render_materials: BTreeMap::new(),
            unresolved_material_primitives: vec![],
        }
    }
    fn scene(elements: Vec<SavedElement>) -> native_saved_scene::SavedScene {
        native_saved_scene::SavedScene {
            source_sha256: None,
            format: "test",
            units: "feet",
            coordinate_frame: "document Z-up",
            complete_document_geometry: false,
            detail_level: 1,
            graphics_profile: "test",
            summaries: vec![],
            elements,
        }
    }

    fn scene_element_with_mesh(id: u64, uid: &str) -> SavedElement {
        let mut element = scene_element(id, uid);
        element.status = "decoded_saved_graphics".into();
        element.meshes.primitives.push(Primitive {
            source_owner_id: None,
            object_index: 7,
            face_tag: 8,
            render_style_id: 9,
            material_id: Some(10),
            vertices: vec![[1., 2., 3.], [2., 2., 3.], [1., 3., 3.]],
            normals: vec![[0., 0., 1.]; 3],
            triangles: vec![[0, 1, 2]],
        });
        element
    }

    fn metadata_with_definition(parameter_id: i64) -> crate::native_metadata::SavedMetadata {
        crate::native_metadata::SavedMetadata {
            value_semantics: "serialized_not_evaluated",
            complete_element_metadata: false,
            root_references: BTreeMap::new(),
            parameter_sets: vec![],
            family_parameter_slots: vec![],
            parameter_definitions: vec![crate::native_parameter_definitions::Definition {
                parameter_id,
                source_object: 0,
                definition_class: "ParamDefString".into(),
                owner_class: "ParamElemProject".into(),
                caption: "Asset Number".into(),
                type_id: None,
                spec_type_id: Some("autodesk.spec:spec.string-2.0.0".into()),
                group_type_id: None,
                spec_evidence: "serialized_spec_type_id",
                unit_type_id: None,
                unit_format_fields: None,
                unit_resolution: "not_applicable".into(),
                shared_guid: None,
                is_shared: Some(false),
                storage_type: Some("String".into()),
                storage_evidence: "validated_concrete_definition_class",
                definition_fields: json!({}),
                owner_fields: BTreeMap::new(),
            }],
            builtin_parameter_definitions: vec![],
            declared_custom_parameters: vec![],
            global_parameter_associations: vec![],
            raw_field_parameters: vec![],
            resolved_family_parameters: vec![],
        }
    }

    #[test]
    fn omits_empty_geometry_rows_but_keeps_metadata_rows() {
        let package = join_records(
            "doc-a",
            vec![record(2, "uid-b")],
            &scene(vec![scene_element(1, "uid-a")]),
            Some("rev-1".into()),
        )
        .unwrap();
        assert_eq!(package.manifest.source_sha256.as_deref(), Some("rev-1"));
        assert_eq!(
            package
                .elements
                .iter()
                .map(|x| x.key.as_str())
                .collect::<Vec<_>>(),
            vec!["doc-a:uid-b"]
        );
        assert_eq!(package.manifest.coverage.metadata_only_records, 1);
        assert_eq!(package.manifest.coverage.geometry_only_records, 0);
    }

    #[test]
    fn distinguishes_selected_nonrenderable_graphics_from_missing_graphics() {
        let mut empty = scene_element(1, "uid-a");
        empty.status = "decoded_saved_graphics".into();
        let package =
            join_records("doc-a", vec![record(1, "uid-a")], &scene(vec![empty]), None).unwrap();
        assert_eq!(package.manifest.coverage.metadata_only_records, 0);
        assert_eq!(package.manifest.coverage.nonrenderable_graphics_records, 1);
        assert_eq!(
            package.elements[0].status,
            "metadata_with_nonrenderable_graphics"
        );
        assert_eq!(
            package.elements[0].attributes["geometry_status"],
            "decoded_saved_graphics"
        );
        assert_eq!(
            package.elements[0].attributes["renderable_geometry_retained"],
            false
        );
        assert_eq!(
            package.elements[0].attributes["geometry_diagnostics"]["unbounded_faces"],
            0
        );
    }

    #[test]
    fn promotes_one_stated_builtin_category_without_guessing_a_name() {
        let mut current = record(1, "uid-a");
        current.saved_metadata = Some(crate::native_metadata::SavedMetadata {
            value_semantics: "serialized_not_evaluated",
            complete_element_metadata: false,
            root_references: BTreeMap::new(),
            parameter_sets: vec![],
            family_parameter_slots: vec![],
            parameter_definitions: vec![],
            builtin_parameter_definitions: vec![],
            declared_custom_parameters: vec![],
            global_parameter_associations: vec![],
            raw_field_parameters: vec![crate::native_metadata::RawFieldParameter {
                parameter_id: -1140362,
                source_object: 0,
                source_field: "m_categoryId".into(),
                source_element_id: Some(7),
                projection_rule: "owner_symbol_family_category_reference_chain",
                storage_type: "ElementId".into(),
                raw_value: json!(-2008016),
                value_semantics: "serialized_not_evaluated",
            }],
            resolved_family_parameters: vec![],
        });
        let package = join_records("doc-a", vec![current], &scene(vec![]), None).unwrap();
        assert_eq!(package.elements[0].category_id, Some(-2008016));
    }

    #[test]
    fn pools_document_parameter_definitions_outside_element_rows() {
        let mut first = record(1, "uid-a");
        let mut second = record(2, "uid-b");
        first.saved_metadata = Some(metadata_with_definition(42));
        second.saved_metadata = Some(metadata_with_definition(42));
        let package = join_records("doc-a", vec![first, second], &scene(vec![]), None).unwrap();
        assert_eq!(package.parameter_definitions.len(), 1);
        assert_eq!(package.parameter_definitions[0].parameter_id, 42);
        for element in &package.elements {
            assert!(
                element.attributes["metadata"]
                    .get("parameter_definitions")
                    .is_none()
            );
            assert!(
                element.attributes["metadata"]
                    .get("builtin_parameter_definitions")
                    .is_some()
            );
        }
    }

    #[test]
    fn pools_custom_parameter_bindings_outside_owner_metadata() {
        let mut current = record(1, "uid-a");
        let mut metadata = metadata_with_definition(42);
        metadata
            .declared_custom_parameters
            .push(crate::native_metadata::DeclaredCustomParameter {
                parameter_id: 42,
                storage_type: "String".into(),
                has_value: false,
                has_value_evidence: "matched_category_binding_absent_owner_value",
                source_object: None,
                binding_id: Some(7),
                owner_category_id: Some(-2008044),
                is_read_only: Some(false),
                read_only_evidence: Some("saved_definition_flag_and_owner_global_associations"),
                associated_global_parameter_id: None,
            });
        current.saved_metadata = Some(metadata);
        let package = join_records_with_diagnostics_and_catalog(
            "doc-a",
            vec![current],
            &scene(vec![]),
            None,
            &[],
            Some(vec![]),
            Some(vec![crate::native_parameter_definitions::Binding {
                binding_id: 7,
                parameter_id: 42,
                category_id: -2008044,
                elem_or_symbol: 1,
            }]),
            None,
        )
        .unwrap();
        assert_eq!(package.parameter_bindings.len(), 1);
        assert!(
            package.elements[0].attributes["metadata"]
                .get("declared_custom_parameters")
                .is_none()
        );
    }

    #[test]
    fn shared_definition_catalog_retains_builtin_parameter_definitions() {
        let mut registry = crate::native_parameter_definitions::Registry::default();
        registry.builtin_definitions.insert(
            -1001203,
            crate::native_parameter_definitions::BuiltinDefinition {
                parameter_id: -1001203,
                caption: "Mark".into(),
                storage_type: "String".into(),
                spec_type_id: "autodesk.spec:spec.string-2.0.0".into(),
                group_type_id: "autodesk.parameter.group:identity_data-1.0.0".into(),
                is_shared: false,
                unit_type_id: None,
                unit_format_fields: None,
                unit_resolution: "not_applicable".into(),
                definition_evidence: "test".into(),
                caption_locale: "en-US".into(),
            },
        );
        let catalog = parameter_definitions_from_registry(&registry).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].parameter_id, -1001203);
        assert_eq!(catalog[0].definition_kind, "builtin_catalog");
        assert_eq!(catalog[0].definition["caption"], "Mark");
    }

    #[test]
    fn namespace_prevents_revision_identity_collision() {
        let a = join_records(
            "doc-a",
            vec![record(1, "same")],
            &scene(vec![]),
            Some("rev-1".into()),
        )
        .unwrap();
        let b = join_records(
            "doc-a",
            vec![record(1, "same")],
            &scene(vec![]),
            Some("rev-2".into()),
        )
        .unwrap();
        assert_eq!(a.elements[0].key, b.elements[0].key);
        assert_ne!(a.manifest.source_sha256, b.manifest.source_sha256);
        assert_eq!(
            join_records("doc-b", vec![record(1, "same")], &scene(vec![]), None)
                .unwrap()
                .elements[0]
                .key,
            "doc-b:same"
        );
    }

    #[test]
    fn emits_explicit_mesh_instance_and_material_rows() {
        let package = join_records(
            "doc-a",
            vec![],
            &scene(vec![scene_element_with_mesh(1, "uid-a")]),
            None,
        )
        .unwrap();
        assert_eq!(package.manifest.coverage.mesh_rows, 1);
        assert_eq!(package.manifest.coverage.instance_rows, 1);
        assert_eq!(package.manifest.coverage.material_rows, 0);
        assert_eq!(package.meshes[0].key, "doc-a:uid-a:mesh:0");
        assert_eq!(package.instances[0].mesh_keys, vec!["doc-a:uid-a:mesh:0"]);
        assert_eq!(
            package.elements[0].geometry_bounds_meters,
            Some([
                [1. * 0.3048, 2. * 0.3048, 3. * 0.3048],
                [2. * 0.3048, 3. * 0.3048, 3. * 0.3048],
            ])
        );
    }

    #[test]
    fn counts_unresolved_materials_without_per_owner_manifest_duplication() {
        let mut element = scene_element_with_mesh(1, "uid-a");
        element.unresolved_material_primitives = vec![0];
        let package = join_records("doc-a", vec![], &scene(vec![element]), None).unwrap();
        assert_eq!(package.manifest.coverage.unresolved_material_primitives, 1);
        assert_eq!(package.meshes[0].status, "material_unresolved");
        assert!(package.materials.is_empty());
        assert!(package.manifest.coverage.diagnostics.is_empty());
    }

    #[test]
    fn package_artifact_is_created_without_overwrite() {
        let package = join_records("doc-a", vec![record(1, "uid")], &scene(vec![]), None).unwrap();
        let root = std::env::temp_dir().join(format!("rvt-native-delivery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_package_dir(&package, &root).unwrap();
        assert!(root.join("manifest.json").is_file());
        assert!(root.join("elements.json").is_file());
        assert!(root.join("parameter-definitions.jsonl").is_file());
        assert!(root.join("parameter-bindings.jsonl").is_file());
        assert!(package.manifest.tables.contains(&"parameter_definitions"));
        assert!(package.manifest.tables.contains(&"parameter_bindings"));
        assert!(!root.join("mesh-assets.jsonl").exists());
        assert!(!root.join("mesh-uses.jsonl").exists());
        assert!(!root.join("instances.jsonl").exists());
        assert!(!root.join("materials.jsonl").exists());
        assert!(!root.join("metrics.jsonl").exists());
        assert!(!root.join("geometry.json").exists());
        assert!(!root.join("meshes.json").exists());
        assert!(!root.join("tileset.json").exists());
        assert!(write_package_dir(&package, &root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sharded_packages_reference_one_root_parameter_catalog() {
        let root = std::env::temp_dir().join(format!(
            "rvt-native-delivery-shared-definitions-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let options = DeliveryOptions {
            document_namespace: "doc-a".into(),
            source_sha256: None,
            scene_detail: 3,
            profile: DeliveryProfile::Tiles,
            native: native_document::Options::default(),
            metadata_ids: None,
            source_bound_extensible_storage_catalog: None,
        };
        write_sharded_package_dirs_with_index(
            &options,
            &root,
            1,
            vec![vec![1], vec![2]],
            vec![ParameterDefinitionRow {
                parameter_id: 42,
                definition_kind: "native_document",
                definition: json!({"caption":"Asset Number"}),
            }],
            vec![],
            Some(CategorySelectionReceipt {
                requested_category_ids: BTreeSet::from([-2_001_140]),
                current_records: 3,
                selected_records: 2,
                selected_by_category: BTreeMap::from([(-2_001_140, 2)]),
                excluded_by_category: BTreeMap::from([(-2_000_011, 1)]),
                ..Default::default()
            }),
            |ids| join_records("doc-a", vec![record(ids[0], "uid")], &scene(vec![]), None),
        )
        .unwrap();
        assert!(root.join("parameter-definitions.jsonl").is_file());
        assert!(root.join("parameter-bindings.jsonl").is_file());
        let root_manifest: Value =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(root_manifest["category_selection"]["selected_records"], 2);
        for shard in root_manifest["shards"].as_array().unwrap() {
            assert_eq!(shard["telemetry"]["version"], 1);
            assert!(shard["telemetry"]["elapsed_ms"].is_u64());
            assert_eq!(shard["telemetry"]["renderable_vertices"], 0);
            assert_eq!(shard["telemetry"]["renderable_triangles"], 0);
            assert!(shard["telemetry"]["package_bytes"].as_u64().unwrap() > 0);
            assert!(shard["telemetry"]["glb_bytes"].is_null());
        }
        assert_eq!(
            root_manifest["selection_semantics"],
            "shards are deterministic selected native BuiltInCategory id ranges; every selected owner has one prepared metadata row"
        );
        for shard in ["shard-000000", "shard-000001"] {
            assert!(
                !root
                    .join(shard)
                    .join("parameter-definitions.jsonl")
                    .exists()
            );
            assert!(!root.join(shard).join("parameter-bindings.jsonl").exists());
            let manifest: Value = serde_json::from_slice(
                &std::fs::read(root.join(shard).join("manifest.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                manifest["parameter_definitions_uri"],
                "../parameter-definitions.jsonl"
            );
            assert_eq!(
                manifest["parameter_bindings_uri"],
                "../parameter-bindings.jsonl"
            );
            assert!(manifest["tables"].as_array().unwrap().iter().all(|value| {
                value != "parameter_definitions" && value != "parameter_bindings"
            }));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_artifact_never_serializes_mesh_arrays() {
        let package = join_records(
            "doc-a",
            vec![],
            &scene(vec![
                scene_element_with_mesh(1, "uid-a"),
                scene_element_with_mesh(2, "uid-b"),
            ]),
            None,
        )
        .unwrap();
        let root =
            std::env::temp_dir().join(format!("rvt-native-delivery-dedup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_package_dir(&package, &root).unwrap();
        assert!(!root.join("mesh-assets.jsonl").exists());
        assert!(!root.join("mesh-uses.jsonl").exists());
        assert_eq!(package.manifest.coverage.unique_mesh_assets, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn profiles_gate_optional_tables_without_restoring_json_meshes() {
        let mut package = join_records(
            "doc-a",
            vec![record(1, "uid")],
            &scene(vec![scene_element_with_mesh(1, "uid")]),
            None,
        )
        .unwrap();
        package.apply_profile(DeliveryProfile::Audit);
        let root =
            std::env::temp_dir().join(format!("rvt-native-delivery-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_package_dir(&package, &root).unwrap();
        assert_eq!(package.manifest.profile, "audit");
        assert!(root.join("types.jsonl").is_file());
        assert!(root.join("relationships.jsonl").is_file());
        assert!(root.join("materials.jsonl").is_file());
        assert!(root.join("network.json").is_file());
        assert!(root.join("metrics.jsonl").is_file());
        assert!(root.join("texture-mappings.json").is_file());
        assert!(!root.join("mesh-assets.jsonl").exists());
        assert!(!root.join("mesh-uses.jsonl").exists());
        assert!(!root.join("instances.jsonl").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn all_empty_geometry_omits_render_tiles_without_failing_package_write() {
        let package =
            join_records("doc-a", vec![], &scene(vec![scene_element(1, "uid")]), None).unwrap();
        let root =
            std::env::temp_dir().join(format!("rvt-native-delivery-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_package_dir(&package, &root).unwrap();
        assert!(!root.join("tileset.json").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn aggregate_tile_box_is_the_union_of_axis_aligned_children() {
        let union = union_tileset_boxes(&[
            json!([1., 2., 3., 1., 0., 0., 0., 2., 0., 0., 0., 3.]),
            json!([5., 4., 2., 2., 0., 0., 0., 1., 0., 0., 0., 1.]),
        ])
        .unwrap();
        assert_eq!(union, [3.5, 2.5, 3., 3.5, 0., 0., 0., 2.5, 0., 0., 0., 3.]);
    }

    #[test]
    fn rejects_uid_numeric_and_scene_id_conflicts() {
        assert!(
            join_records(
                "doc",
                vec![record(1, "same"), record(2, "same")],
                &scene(vec![]),
                None
            )
            .is_err()
        );
        assert!(
            join_records(
                "doc",
                vec![record(1, "one"), record(1, "two")],
                &scene(vec![]),
                None
            )
            .is_err()
        );
        let mut mismatched = scene_element(9, "uid");
        mismatched.id = 10;
        assert!(join_records("doc", vec![], &scene(vec![mismatched]), None).is_err());
    }
}
