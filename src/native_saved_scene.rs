//! Current native saved-graphics extraction with explicit shared-symbol closure.
use crate::{
    RevitFile,
    native_document::{self, Options, Record, Summary},
    native_saved_mesh::{self, GraphicsMeshes},
};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Serialize)]
pub struct SavedElement {
    pub id: u64,
    pub identity: crate::native_index::Identity,
    pub source: serde_json::Value,
    pub referenced_graph_sources: BTreeMap<u64, serde_json::Value>,
    pub status: String,
    pub requested_detail_level: i64,
    pub effective_detail_level: i64,
    #[serde(default)]
    pub detail_diagnostic: Option<String>,
    pub meshes: GraphicsMeshes,
    pub render_materials: BTreeMap<usize, crate::native_saved_materials::RenderMaterial>,
    pub unresolved_material_primitives: Vec<usize>,
}

/// Metadata already recovered from the current channel-102 owner record.
/// Saved graphics live on channel 103 and do not reliably restate the owner
/// class/category, so the delivery path supplies this immutable hint instead
/// of guessing from graphics structure or decoding metadata a second time.
#[derive(Debug, Clone, Default)]
pub struct OwnerDetailHint {
    pub class_name: Option<String>,
    pub category_id: Option<i64>,
}

// Fine family-symbol graphics can fan out through nested mechanical-equipment
// content without producing many top-level carriers. Keep this budget
// intentionally conservative; the emitted owner diagnostic records every
// downgrade and the requested detail remains in the package metadata.
const MAX_FINE_GRAPHICS_CARRIERS: usize = 512;

fn detail_decision(
    requested: i64,
    selected_carriers: usize,
    hit_resource_limit: bool,
) -> (i64, Option<String>) {
    if requested != 3 {
        return (requested, None);
    }
    if selected_carriers > MAX_FINE_GRAPHICS_CARRIERS || hit_resource_limit {
        return (
            2,
            Some(format!(
                "requested detail 3 reduced to detail 2: fine saved graphics preflight selected {} carriers (budget {})",
                selected_carriers, MAX_FINE_GRAPHICS_CARRIERS
            )),
        );
    }
    (requested, None)
}

fn resource_profile_requires_fallback(
    body_bytes: usize,
    decoded_objects: usize,
    declared_objects: u64,
) -> bool {
    body_bytes > 128 * 1024
        || decoded_objects > 64
        || (declared_objects > 2_048 && decoded_objects > 1)
}

fn is_bounded_mechanical_family(
    requested: i64,
    owner_class: Option<&str>,
    category_id: Option<i64>,
) -> bool {
    requested == 3
        && matches!(owner_class, Some("FamilyInstance") | Some("FamilySymbol"))
        && category_id == Some(-2_001_140)
}

fn detail_for_owner<'a>(
    graph: &'a crate::native_parameters::ObjectGraph,
    resolver: &dyn Fn(u64) -> Option<&'a crate::native_parameters::ObjectGraph>,
    requested: i64,
    owner_class: Option<&str>,
    category_id: Option<i64>,
    body_bytes: usize,
    declared_objects: u64,
) -> (i64, Option<String>) {
    if is_bounded_mechanical_family(requested, owner_class, category_id) {
        return (
            2,
            Some(
                "requested detail 3 reduced to detail 2: observed mechanical-equipment family Fine geometry is outside the bounded decode profile"
                    .into(),
            ),
        );
    }
    // A large declared-object count by itself is common on empty/reference
    // graphics groups: the record can advertise a shared definition table
    // while the owner decodes to one carrier and no surface.  Do not lower
    // authored detail for those owners.  Combine that signal with decoded
    // graph content so the fallback remains targeted at expensive geometry.
    if requested == 3
        && resource_profile_requires_fallback(body_bytes, graph.objects.len(), declared_objects)
    {
        return (
            2,
            Some(format!(
                "requested detail 3 reduced to detail 2: saved graphics resource profile is {} body bytes, {} declared objects, and {} decoded objects",
                body_bytes,
                declared_objects,
                graph.objects.len()
            )),
        );
    }
    let selection = crate::native_graphics_traversal::select_graphics_with_resolver_at_detail(
        graph, resolver, requested,
    );
    detail_decision(
        requested,
        selection.selected.len(),
        selection
            .diagnostics
            .iter()
            .any(|d| d.message.contains("resource limit")),
    )
}
#[derive(Debug, Serialize)]
pub struct SavedScene {
    pub source_sha256: Option<String>,
    pub format: &'static str,
    pub units: &'static str,
    pub coordinate_frame: &'static str,
    pub complete_document_geometry: bool,
    pub detail_level: i64,
    pub graphics_profile: &'static str,
    pub summaries: Vec<Summary>,
    pub elements: Vec<SavedElement>,
}

/// Decoded graphics/material state for one extraction selection.
///
/// Sharded delivery may reuse this object while a package is being built, but
/// it must not retain decoded graphs or material provenance across completed
/// shards. Those values are proportional to the number of owners seen and can
/// otherwise turn a cost-bounded run into an unbounded RSS run.
pub struct ExtractionCache {
    graphics_records: BTreeMap<u64, Record>,
    attempted_material_ids: BTreeSet<u64>,
    resolver: crate::native_saved_materials::Resolver,
}

impl Default for ExtractionCache {
    fn default() -> Self {
        Self {
            graphics_records: BTreeMap::new(),
            attempted_material_ids: BTreeSet::new(),
            resolver: crate::native_saved_materials::Resolver::default_viewport_profile(),
        }
    }
}
impl ExtractionCache {
    /// Drop every decoded selection after one completed package. Meshes and
    /// materials have already been projected into that package. Keeping symbol
    /// graphs or resolver provenance for following shards makes RSS grow with
    /// the number of shards, so cross-shard reuse is deliberately not allowed.
    pub fn release_completed_selection(&mut self) {
        self.graphics_records.clear();
        self.attempted_material_ids.clear();
        self.resolver = crate::native_saved_materials::Resolver::default_viewport_profile();
    }
}
fn saved_element_status(meshes: &GraphicsMeshes) -> &'static str {
    // Selection exclusions are intentional profile decisions and stay visible
    // as counters without tainting an otherwise valid mesh. An unbounded face,
    // or a decoder diagnostic, means the owner was not fully tessellated.
    if meshes.diagnostics.is_empty() && meshes.unbounded_faces == 0 {
        "decoded_saved_graphics"
    } else {
        "partial_saved_graphics"
    }
}

fn retain_graphics_summary(mut summary: Summary) -> Summary {
    // Global/Latest can contain a very large extensible-storage source tree.
    // It is useful evidence for document extraction, but saved-scene callers
    // only need coverage and stream provenance. Do not retain one copy for
    // every shared-graphics closure pass.
    summary.extensible_storage_catalog = None;
    summary.extensible_storage_catalog_source = None;
    summary
}

pub fn extract(file: &mut RevitFile, options: &Options) -> Result<SavedScene> {
    extract_at_detail(file, options, 3)
}
pub fn extract_at_detail(
    file: &mut RevitFile,
    options: &Options,
    detail_level: i64,
) -> Result<SavedScene> {
    let mut index_options = options.clone();
    index_options.channels = BTreeSet::from([103]);
    let physical_index = native_document::build_physical_index(file, &index_options)?;
    extract_at_detail_using_index(file, options, detail_level, &physical_index)
}

/// Extract saved graphics using a previously validated physical record index.
pub fn extract_at_detail_using_index(
    file: &mut RevitFile,
    options: &Options,
    detail_level: i64,
    physical_index: &native_document::PhysicalIndex,
) -> Result<SavedScene> {
    let mut cache = ExtractionCache::default();
    extract_at_detail_using_index_with_cache(
        file,
        options,
        detail_level,
        physical_index,
        &mut cache,
    )
}

/// Cached variant for sequential shard projection.
pub fn extract_at_detail_using_index_with_cache(
    file: &mut RevitFile,
    options: &Options,
    detail_level: i64,
    physical_index: &native_document::PhysicalIndex,
    cache: &mut ExtractionCache,
) -> Result<SavedScene> {
    extract_at_detail_using_index_with_cache_and_catalog(
        file,
        options,
        detail_level,
        physical_index,
        cache,
        None,
        None,
    )
}

/// Cached selected extraction with the document-wide ES schema context.
/// Saved graphics can share the same owner graph with user Extensible Storage;
/// decoding that opaque-to-graphics payload is necessary to preserve cursor
/// alignment and reach the graphics objects that follow it.
pub fn extract_at_detail_using_index_with_cache_and_catalog(
    file: &mut RevitFile,
    options: &Options,
    detail_level: i64,
    physical_index: &native_document::PhysicalIndex,
    cache: &mut ExtractionCache,
    extensible_storage_catalog: Option<&crate::native_extensible_storage::Catalog>,
    owner_detail_hints: Option<&BTreeMap<u64, OwnerDetailHint>>,
) -> Result<SavedScene> {
    ensure!(
        (1..=3).contains(&detail_level),
        "detail level must be 1, 2 or 3"
    );
    let mut opts = options.clone();
    opts.channels = BTreeSet::from([103]);
    let requested = if opts.selected_ids.is_empty() {
        physical_index.current_ids(103)
    } else {
        opts.selected_ids.clone()
    };
    let missing_roots = requested
        .difference(&cache.graphics_records.keys().copied().collect())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut summaries = Vec::new();
    if !missing_roots.is_empty() {
        opts.selected_ids = missing_roots.clone();
        summaries.push(retain_graphics_summary(
            native_document::extract_without_definitions_using_index_with_catalog(
                file,
                &opts,
                physical_index,
                extensible_storage_catalog,
                |r| {
                    cache
                        .graphics_records
                        .entry(r.identity.element_id)
                        .or_insert(r);
                    Ok(())
                },
            )?,
        ));
    }
    let roots = requested
        .into_iter()
        .filter(|id| cache.graphics_records.contains_key(id))
        .collect::<BTreeSet<_>>();
    let mut frontier = roots.clone();
    for depth in 0..=128 {
        let mut wanted = BTreeSet::new();
        for id in &frontier {
            let symbols = cache.graphics_records[id]
                .graph
                .as_ref()
                .into_iter()
                .flat_map(|graph| graph.objects.iter())
                .filter(|object| object.class_name == "InstanceInfo")
                .filter_map(|object| {
                    crate::native_metadata::identifier(&object.fields["m_symbolId"])
                        .ok()
                        .and_then(|id| u64::try_from(id).ok())
                        .filter(|id| *id > 0)
                })
                .collect::<Vec<_>>();
            for symbol in symbols {
                if !cache.graphics_records.contains_key(&symbol) {
                    wanted.insert(symbol);
                }
            }
        }
        if wanted.is_empty() {
            break;
        }
        ensure!(depth < 128, "shared graphics closure depth budget");
        opts.selected_ids = wanted;
        let mut loaded = BTreeSet::new();
        summaries.push(retain_graphics_summary(
            native_document::extract_without_definitions_using_index_with_catalog(
                file,
                &opts,
                physical_index,
                extensible_storage_catalog,
                |r| {
                    loaded.insert(r.identity.element_id);
                    cache
                        .graphics_records
                        .entry(r.identity.element_id)
                        .or_insert(r);
                    Ok(())
                },
            )?,
        ));
        frontier = loaded;
    }
    let mut material_options = options.clone();
    material_options.channels = BTreeSet::from([102]);
    let mut wanted_material_ids = BTreeSet::new();
    for id in &roots {
        let Some(graph) = cache.graphics_records[id].graph.as_ref() else {
            continue;
        };
        let (dependencies, _diagnostics) =
            native_saved_mesh::material_dependency_ids_with_resolver_at_detail(
                graph,
                &|id| {
                    cache
                        .graphics_records
                        .get(&id)
                        .and_then(|r| r.graph.as_ref())
                },
                detail_level,
            );
        wanted_material_ids.insert(*id);
        wanted_material_ids.extend(dependencies);
    }
    for _ in 0..8 {
        let wanted = wanted_material_ids
            .difference(&cache.attempted_material_ids)
            .copied()
            .collect::<BTreeSet<_>>();
        if wanted.is_empty() {
            break;
        }
        cache.attempted_material_ids.extend(&wanted);
        material_options.selected_ids = wanted;
        summaries.push(retain_graphics_summary(
            native_document::extract_without_definitions_using_index_with_catalog(
                file,
                &material_options,
                &physical_index,
                extensible_storage_catalog,
                |r| cache.resolver.ingest(&r),
            )?,
        ));
        wanted_material_ids.extend(cache.resolver.dependency_ids());
    }
    let mut elements = Vec::new();
    for id in roots {
        let r = &cache.graphics_records[&id];
        let resolver = |id| {
            cache
                .graphics_records
                .get(&id)
                .and_then(|r| r.graph.as_ref())
        };
        let hint = owner_detail_hints.and_then(|hints| hints.get(&id));
        let category_id = hint
            .and_then(|hint| hint.category_id)
            .or(crate::native_delivery::category_id(Some(r))?);
        let (effective_detail_level, detail_diagnostic) = match &r.graph {
            Some(g) => detail_for_owner(
                g,
                &resolver,
                detail_level,
                hint.and_then(|hint| hint.class_name.as_deref())
                    .or(r.class_name.as_deref()),
                category_id,
                r.source.body_bytes,
                r.source.group.declared_objects,
            ),
            None => (detail_level, None),
        };
        let mut meshes = match &r.graph {
            Some(g) => native_saved_mesh::graphics_with_resolver_at_detail(
                g,
                &resolver,
                effective_detail_level,
            ),
            None => GraphicsMeshes::default(),
        };
        if r.graph.is_none() {
            meshes.diagnostics.push(
                r.diagnostic
                    .clone()
                    .unwrap_or_else(|| "graphics graph unavailable".into()),
            )
        }
        let refs: BTreeSet<_> = meshes
            .primitives
            .iter()
            .filter_map(|p| p.source_owner_id)
            .collect();
        let referenced_graph_sources = refs
            .into_iter()
            .map(|id| {
                Ok((
                    id,
                    serde_json::to_value(&cache.graphics_records[&id].source)?,
                ))
            })
            .collect::<Result<_>>()?;
        let mut render_materials = BTreeMap::new();
        let mut unresolved_material_primitives = Vec::new();
        for (index, p) in meshes.primitives.iter().enumerate() {
            match cache.resolver.resolve(
                id,
                p.source_owner_id,
                p.face_tag,
                p.render_style_id,
                p.material_id,
            ) {
                Some(m) => {
                    render_materials.insert(index, m);
                }
                None => unresolved_material_primitives.push(index),
            }
        }
        elements.push(SavedElement {
            render_materials,
            unresolved_material_primitives,
            id,
            identity: r.identity.clone(),
            source: serde_json::to_value(&r.source)?,
            referenced_graph_sources,
            status: saved_element_status(&meshes).into(),
            requested_detail_level: detail_level,
            effective_detail_level,
            detail_diagnostic,
            meshes,
        });
    }
    Ok(SavedScene {
        source_sha256: None,
        format: "rvt-native-saved-scene-v1",
        units: "feet",
        coordinate_frame: "document Z-up",
        complete_document_geometry: false,
        detail_level,
        graphics_profile: "default 3D viewport (qualified supported subset)",
        summaries,
        elements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u64) -> Record {
        Record {
            identity: crate::native_index::Identity {
                element_id: id,
                original_id_suffix: id,
                creation_episode: 0,
                stored_revision: 0,
                other_revision: 0,
                row_offset: 0,
                raw_fields: vec![],
                owning_element_id: -1,
                partition_id: 0,
                unique_id: format!("test-{id}"),
            },
            derived_default_ifc_guid: None,
            derived_identifier_diagnostic: None,
            effective_ifc_parameter: None,
            effective_ifc_parameter_diagnostic: None,
            channel: 103,
            class_name: None,
            status: "complete_bounded_graph".into(),
            diagnostic: None,
            source: crate::native_document::RecordSource {
                stream: "test".into(),
                group: crate::native_segments::GroupSource {
                    content_key: None,
                    channel: 103,
                    first_marker_offset: 0,
                    segment_count: 1,
                    declared_objects: 1,
                    declared_body_bytes: 0,
                },
                group_record_offset: 0,
                body_bytes: 0,
                body_sha256: String::new(),
            },
            graph: None,
            saved_metadata: None,
            metadata_diagnostic: None,
        }
    }

    #[test]
    fn status_taints_unbounded_faces_but_not_intentional_exclusions() {
        let mut meshes = GraphicsMeshes {
            excluded_visibility_branches: 2,
            excluded_non_surface_branches: 3,
            rejected_filters: 1,
            empty_trim_faces: 1,
            ..Default::default()
        };
        assert_eq!(saved_element_status(&meshes), "decoded_saved_graphics");
        meshes.unbounded_faces = 1;
        assert_eq!(saved_element_status(&meshes), "partial_saved_graphics");
        meshes.unbounded_faces = 0;
        meshes.diagnostics.push("unsupported face".into());
        assert_eq!(saved_element_status(&meshes), "partial_saved_graphics");
    }

    #[test]
    fn cache_releases_all_selection_state_at_shard_boundary() {
        let mut cache = ExtractionCache::default();
        cache.graphics_records.insert(10, record(10));
        cache.graphics_records.insert(20, record(20));
        cache.attempted_material_ids.insert(30);
        cache.release_completed_selection();
        assert!(!cache.graphics_records.contains_key(&10));
        assert!(!cache.graphics_records.contains_key(&20));
        assert!(cache.attempted_material_ids.is_empty());
    }

    #[test]
    fn fine_detail_fallback_is_explicit_and_bounded() {
        let (effective, diagnostic) = detail_decision(3, MAX_FINE_GRAPHICS_CARRIERS + 1, false);
        assert_eq!(effective, 2);
        assert!(diagnostic.unwrap().contains("budget"));
        assert_eq!(detail_decision(2, usize::MAX, false), (2, None));
        assert_eq!(
            detail_decision(3, MAX_FINE_GRAPHICS_CARRIERS, false),
            (3, None)
        );
    }

    #[test]
    fn declared_empty_reference_graph_does_not_lower_authored_detail() {
        assert!(!resource_profile_requires_fallback(2, 1, 4_321));
        assert!(resource_profile_requires_fallback(882, 14, 2_734));
        assert!(resource_profile_requires_fallback(2_059_602, 15_625, 1));
    }

    #[test]
    fn bounded_mechanical_family_rule_uses_current_owner_identity() {
        assert!(is_bounded_mechanical_family(
            3,
            Some("FamilyInstance"),
            Some(-2_001_140),
        ));
        assert!(is_bounded_mechanical_family(
            3,
            Some("FamilySymbol"),
            Some(-2_001_140),
        ));
        assert!(!is_bounded_mechanical_family(
            3,
            Some("FamilyInstance"),
            Some(-2_001_150),
        ));
    }
}
