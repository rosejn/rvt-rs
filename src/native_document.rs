//! Streaming current-record extraction with structural schemas and native IDs.
use crate::{
    RevitFile, compression,
    native_index::{self, Identity},
    native_parameters::{self, ObjectGraph},
    native_segments::{self, GroupSource},
    schema_registry,
};
use anyhow::{Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ResourceBudget {
    pub max_stream_bytes: u64,
    pub max_group_bytes: usize,
    pub max_graph_values: usize,
    pub max_graph_objects: usize,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub selected_ids: BTreeSet<u64>,
    pub channels: BTreeSet<u64>,
    pub max_stream_bytes: u64,
    pub max_group_bytes: usize,
    pub max_graph_values: usize,
    pub max_graph_objects: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            selected_ids: BTreeSet::new(),
            channels: BTreeSet::from([102]),
            max_stream_bytes: 512 * 1024 * 1024,
            max_group_bytes: 256 * 1024 * 1024,
            max_graph_values: 100_000,
            max_graph_objects: 100_000,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct Record {
    pub identity: Identity,
    pub derived_default_ifc_guid: Option<String>,
    pub derived_identifier_diagnostic: Option<String>,
    pub effective_ifc_parameter: Option<crate::native_metadata::EffectiveIfcParameter>,
    pub effective_ifc_parameter_diagnostic: Option<String>,
    pub channel: u64,
    pub class_name: Option<String>,
    pub status: String,
    pub diagnostic: Option<String>,
    pub source: RecordSource,
    pub graph: Option<ObjectGraph>,
    pub saved_metadata: Option<crate::native_metadata::SavedMetadata>,
    pub metadata_diagnostic: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct RecordSource {
    pub stream: String,
    pub group: GroupSource,
    pub group_record_offset: usize,
    pub body_bytes: usize,
    pub body_sha256: String,
}
/// A bounded, physical-framing diagnostic for one current record.
///
/// This is deliberately not a graph decode.  It exposes bytes only after the
/// same partition routing, marker selection, header, and dual-length checks
/// used by production extraction have succeeded.  `body_hex` is capped so a
/// diagnostic cannot turn a large selected record into an unbounded report.
#[derive(Debug, Serialize)]
pub struct PhysicalRecordProbe {
    pub element_id: u64,
    pub channel: u64,
    pub source: RecordSource,
    pub header_hex: String,
    pub trailer_hex: String,
    pub body_hex: String,
    pub body_preview_truncated: bool,
}
#[derive(Debug, Default, Serialize)]
pub struct ClassCoverage {
    pub records: usize,
    pub complete_graphs: usize,
    pub unsupported_graphs: usize,
    pub body_bytes: u64,
    pub complete_graph_body_bytes: u64,
    pub diagnostics: BTreeMap<String, usize>,
}
#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub budgets: ResourceBudget,
    pub global_streams: BTreeMap<String, StreamEnvelope>,
    pub extensible_storage_catalog: Option<crate::native_extensible_storage::Catalog>,
    pub extensible_storage_catalog_source: Option<serde_json::Value>,
    pub extensible_storage_catalog_diagnostic: Option<String>,
    pub classes: BTreeMap<String, ClassCoverage>,
    pub revit_version: u32,
    pub schema_sha256: String,
    pub indexed_elements: usize,
    pub graveyard_records: usize,
    pub selected_indexed_elements: usize,
    pub complete_document_geometry: bool,
    pub serialized_values_not_evaluated: bool,
    pub emitted_records: usize,
    pub complete_graph_records: usize,
    pub unsupported_graph_records: usize,
    pub refused_graph_ids: Vec<u64>,
    pub refused_graph_classes: BTreeMap<String, usize>,
    pub max_graph_values_observed: usize,
    pub max_graph_objects_observed: usize,
    pub adaptive_graph_retries: usize,
    pub adaptive_graph_recoveries: usize,
    pub projected_metadata_records: usize,
    pub unsupported_metadata_records: usize,
    pub skipped_embedded_content_groups: usize,
    pub skipped_historical_records: usize,
    pub requested_ids_absent_from_index: Vec<u64>,
    pub selected_ids_without_records: Vec<u64>,
    pub partitions: BTreeMap<String, native_segments::Statistics>,
    pub parameter_definitions: BTreeMap<i64, crate::native_parameter_definitions::Definition>,
    pub definition_diagnostics: BTreeMap<u64, String>,
    pub unit_formats: BTreeMap<String, serde_json::Value>,
    pub builtin_catalog_provenance: Option<serde_json::Value>,
    pub parameter_binding_context: serde_json::Value,
}

const ADAPTIVE_GRAPH_MAX_VALUES: usize = 1_000_000;
const ADAPTIVE_GRAPH_MAX_OBJECTS: usize = 1_000_000;

fn adaptive_graph_limit(current: usize, ceiling: usize) -> Option<usize> {
    (current < ceiling).then(|| {
        current
            .saturating_mul(4)
            .max(current.saturating_add(1))
            .min(ceiling)
    })
}

fn decode_graph_with_adaptive_budget(
    body: &[u8],
    registry: &schema_registry::Registry,
    options: &Options,
    catalog: Option<&crate::native_extensible_storage::Catalog>,
) -> Result<(ObjectGraph, native_parameters::GraphUsage, bool)> {
    let initial_limits = native_parameters::GraphLimits {
        max_values: options.max_graph_values,
        max_objects: options.max_graph_objects,
        ..Default::default()
    };
    let mut limits = initial_limits;
    let mut prior_error = None;
    loop {
        match native_parameters::decode_graph_with_catalog_usage(body, registry, &limits, catalog) {
            Ok((graph, usage)) => return Ok((graph, usage, prior_error.is_some())),
            Err(error) => {
                let rendered = error.to_string();
                let value_budget_failure = rendered.contains("native field value budget exceeded")
                    || rendered.contains("native container count budget exceeded")
                    || rendered.contains("native compact container count budget exceeded");
                let next_values = value_budget_failure
                    .then(|| adaptive_graph_limit(limits.max_values, ADAPTIVE_GRAPH_MAX_VALUES))
                    .flatten()
                    .unwrap_or(limits.max_values);
                let next_objects = rendered
                    .contains("native graph object budget exceeded")
                    .then(|| adaptive_graph_limit(limits.max_objects, ADAPTIVE_GRAPH_MAX_OBJECTS))
                    .flatten()
                    .unwrap_or(limits.max_objects);
                if next_values == limits.max_values && next_objects == limits.max_objects {
                    return if let Some(prior) = prior_error {
                        Err(anyhow::anyhow!(
                            "adaptive graph retry ({}, {} values/objects) failed: {error:#}; prior failure: {prior:#}",
                            limits.max_values,
                            limits.max_objects
                        ))
                    } else {
                        Err(error)
                    };
                }
                prior_error = Some(error);
                limits.max_values = next_values;
                limits.max_objects = next_objects;
            }
        }
    }
}
/// Integrity-checked member boundaries. The suffix is retained as opaque storage
/// evidence; its meaning and checksum algorithm are not yet decoded.
#[derive(Debug, Default, Serialize)]
pub struct StreamEnvelope {
    pub prepared_bytes: usize,
    pub member_offset: usize,
    pub member_bytes: usize,
    pub opaque_suffix_bytes: usize,
    pub opaque_suffix_sha256: String,
    pub suffix_interpreted: bool,
}

/// Validated physical locations for current native records. The index keeps
/// only group offsets, not record bodies, so it remains bounded by the number
/// of current owners while allowing later selected passes to skip unrelated
/// gzip payloads.
#[derive(Debug, Clone, Default)]
pub struct PhysicalIndex {
    groups: BTreeMap<String, BTreeMap<(u64, u64), usize>>,
    record_bytes: BTreeMap<(u64, u64), usize>,
    /// Current metadata/graphics stream locations by owner.  This is the
    /// scheduling index used to keep selected delivery shards partition-local;
    /// it avoids repeatedly searching every physical group for every owner.
    owner_streams: BTreeMap<u64, BTreeSet<String>>,
}

impl PhysicalIndex {
    /// Return the validated segment-group marker offset for one current native
    /// record. Diagnostic tools use this to inspect the same physical framing
    /// production extraction selected; it is not a logical element-table hint.
    pub fn group_offset_for(&self, stream: &str, channel: u64, id: u64) -> Option<usize> {
        self.groups
            .get(stream)
            .and_then(|groups| groups.get(&(channel, id)))
            .copied()
    }

    /// Return the validated current record ids for one native channel.
    ///
    /// This is intentionally derived from the physical index rather than the
    /// logical element table: callers can shard work only over records whose
    /// framing, routing, and current-state placement have already passed the
    /// same integrity checks used by extraction.
    pub fn current_ids(&self, channel: u64) -> BTreeSet<u64> {
        self.groups
            .values()
            .flat_map(|groups| groups.keys())
            .filter_map(|(candidate_channel, id)| (*candidate_channel == channel).then_some(*id))
            .collect()
    }

    /// Return the validated stored body size for a current native record.
    /// This is a scheduling estimate only: graph closure can add referenced
    /// symbol records, so callers must still enforce decoder resource limits.
    pub fn record_bytes(&self, channel: u64, id: u64) -> usize {
        self.record_bytes.get(&(channel, id)).copied().unwrap_or(0)
    }

    /// Plan deterministic, cost-bounded selections without scattering one
    /// shard across unrelated partition streams.  A selected owner can appear
    /// in both metadata (102) and graphics (103); its stream-set is therefore
    /// the exact union required by one delivery shard.  Grouping by that set
    /// lets later selected passes skip every other compressed partition.
    pub fn partition_local_shards(
        &self,
        requested: &[u64],
        max_ids: usize,
        max_cost_bytes: usize,
    ) -> Result<Vec<Vec<u64>>> {
        ensure!(
            max_ids > 0 || max_cost_bytes > 0,
            "partition-local sharding requires an id or byte budget"
        );
        let mut by_streams = BTreeMap::<BTreeSet<String>, Vec<u64>>::new();
        for &id in requested {
            let streams = self.owner_streams.get(&id).cloned().unwrap_or_default();
            ensure!(
                !streams.is_empty(),
                "selected owner {id} has no current physical 102/103 record"
            );
            by_streams.entry(streams).or_default().push(id);
        }
        let max_ids = if max_ids == 0 { usize::MAX } else { max_ids };
        let mut result = Vec::new();
        for ids in by_streams.into_values() {
            let mut shard = Vec::new();
            let mut cost = 0usize;
            for id in ids {
                let id_cost = self
                    .record_bytes(102, id)
                    .saturating_add(self.record_bytes(103, id))
                    .max(1);
                let exceeds_cost = max_cost_bytes > 0
                    && !shard.is_empty()
                    && cost.saturating_add(id_cost) > max_cost_bytes;
                if !shard.is_empty() && (shard.len() >= max_ids || exceeds_cost) {
                    result.push(std::mem::take(&mut shard));
                    cost = 0;
                }
                shard.push(id);
                cost = cost.saturating_add(id_cost);
            }
            if !shard.is_empty() {
                result.push(shard);
            }
        }
        Ok(result)
    }

    fn offsets_for(
        &self,
        stream: &str,
        channels: &BTreeSet<u64>,
        selected_ids: &BTreeSet<u64>,
    ) -> BTreeSet<usize> {
        self.groups
            .get(stream)
            .into_iter()
            .flat_map(|groups| groups.iter())
            .filter(|((channel, id), _)| channels.contains(channel) && selected_ids.contains(id))
            .map(|(_, offset)| *offset)
            .collect()
    }
}

/// Document-wide parameter-definition state reusable across selected passes.
///
/// Definition owners are independent of a delivery shard. Keeping their
/// resolved registry and diagnostics separate from the physical index avoids
/// re-decoding the same definition population for every shard.
#[derive(Debug)]
pub struct DefinitionContext {
    pub registry: crate::native_parameter_definitions::Registry,
    pub schema_sha256: String,
    pub diagnostics: BTreeMap<u64, String>,
    /// Current channel-102 owners decoded while constructing this context.
    /// Category-selected instance delivery reuses their derived registry state
    /// and deliberately does not decode these definition-only owners again.
    pub definition_owner_ids: BTreeSet<u64>,
    /// Minimal category-selection facts retained from definition owners that
    /// were already fully decoded to build the document registry.  This lets
    /// category selection include qualifying system/family types without
    /// decoding the same owners a second time.
    pub definition_owner_category_candidates: BTreeMap<u64, DefinitionOwnerCategoryCandidate>,
    /// File-wide ES schema declarations decoded once with the other document
    /// context. Selected passes borrow this rather than reopening and
    /// rematerializing Global/Latest for every shard.
    pub extensible_storage_catalog: Option<crate::native_extensible_storage::Catalog>,
    pub extensible_storage_catalog_diagnostic: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DefinitionOwnerCategoryCandidate {
    pub class_name: String,
    pub direct_category_id: Option<i64>,
    pub family_id: Option<i64>,
}

/// Build the physical group index once. This pass validates every partition
/// and every current 102/103 record header, but does not decode object graphs.
/// Its locations are safe to reuse for later selected extraction passes.
pub fn build_physical_index(file: &mut RevitFile, options: &Options) -> Result<PhysicalIndex> {
    let mut read = |name: &str| -> Result<Vec<u8>> { Ok(read_single_envelope(file, name)?.0) };
    let registry = schema_registry::parse(&read("Formats/Latest")?)?;
    let episodes = native_index::creation_episodes(&read("Global/History")?, &registry)?;
    let index = native_index::parse(&read("Global/ElemTable")?, &registry, &episodes)?;
    let id_bytes = physical_record_id_bytes(&index)?;
    let increments =
        native_index::storage_increments(&read("Global/DocumentIncrementTable")?, &registry)?;
    let mut names: Vec<_> = file
        .stream_names()
        .into_iter()
        .filter(|n| n.starts_with("Partitions/"))
        .collect();
    names.sort();
    let present: BTreeSet<u32> = names
        .iter()
        .map(|n| n[11..].parse())
        .collect::<Result<_, _>>()?;
    let mut physical = PhysicalIndex::default();
    let mut seen_current = BTreeSet::<(u64, u64)>::new();
    for name in names {
        let partition: u32 = name[11..].parse()?;
        let stored = file.read_stream_with_limit(&name, options.max_stream_bytes)?;
        let prepared = compression::prepare_stream_for_inflate(&name, &stored);
        let groups = physical.groups.entry(name.clone()).or_default();
        native_segments::walk(
            &prepared,
            &registry,
            options.max_group_bytes,
            |source, bytes| {
                if source.content_key.is_some() || !matches!(source.channel, 102 | 103) {
                    return Ok(());
                }
                let header = id_bytes + 8;
                let mut pos = 0;
                let mut body_sum = 0u64;
                while pos < bytes.len() {
                    ensure!(
                        bytes.len() - pos >= header + 4,
                        "truncated channel record header"
                    );
                    let id = if id_bytes == 4 {
                        u64::from(u32::from_le_bytes(bytes[pos..pos + 4].try_into()?))
                    } else {
                        u64::from_le_bytes(bytes[pos..pos + 8].try_into()?)
                    };
                    let length =
                        u32::from_le_bytes(bytes[pos + header - 4..pos + header].try_into()?)
                            as usize;
                    let start = pos + header;
                    let end = start
                        .checked_add(length)
                        .ok_or_else(|| anyhow::anyhow!("native record size overflow"))?;
                    ensure!(
                        end + 4 <= bytes.len()
                            && u32::from_le_bytes(bytes[end..end + 4].try_into()?) as usize
                                == length,
                        "native record dual lengths disagree"
                    );
                    if let Some(identity) = index.identities.get(&id)
                        && native_index::route_episode(
                            identity.stored_revision,
                            &increments,
                            &present,
                        )? == partition
                    {
                        ensure!(
                            groups
                                .insert((source.channel, id), source.first_marker_offset)
                                .is_none(),
                            "ambiguous current record for {id} in channel{}",
                            source.channel
                        );
                        ensure!(
                            seen_current.insert((source.channel, id)),
                            "ambiguous current record for {id} in channel{} across partitions",
                            source.channel
                        );
                        physical
                            .record_bytes
                            .insert((source.channel, id), length.max(1));
                        physical
                            .owner_streams
                            .entry(id)
                            .or_default()
                            .insert(name.clone());
                    }
                    body_sum += length as u64;
                    pos = end + 4;
                }
                ensure!(
                    records_count(bytes, id_bytes, source.channel)? as u64
                        == source.declared_objects
                        && body_sum == source.declared_body_bytes,
                    "native group record counts/body sizes disagree"
                );
                Ok(())
            },
        )?;
    }
    Ok(physical)
}

/// Locate one already-indexed current record and report its exact validated
/// physical frame.  This is a diagnostic seam for format research: consumers
/// must not treat its bytes as decoded semantics.
pub fn probe_current_record(
    file: &mut RevitFile,
    options: &Options,
    physical_index: &PhysicalIndex,
    channel: u64,
    element_id: u64,
) -> Result<PhysicalRecordProbe> {
    ensure!(matches!(channel, 101 | 102 | 103), "unsupported native record channel {channel}");
    let mut read = |name: &str| -> Result<Vec<u8>> { Ok(read_single_envelope(file, name)?.0) };
    let registry = schema_registry::parse(&read("Formats/Latest")?)?;
    let episodes = native_index::creation_episodes(&read("Global/History")?, &registry)?;
    let index = native_index::parse(&read("Global/ElemTable")?, &registry, &episodes)?;
    let id_bytes = physical_record_id_bytes(&index)?;
    let increments = native_index::storage_increments(&read("Global/DocumentIncrementTable")?, &registry)?;
    let identity = index
        .identities
        .get(&element_id)
        .ok_or_else(|| anyhow::anyhow!("selected element {element_id} absent from index"))?;
    let mut names: Vec<_> = file
        .stream_names()
        .into_iter()
        .filter(|name| name.starts_with("Partitions/"))
        .collect();
    names.sort();
    let present: BTreeSet<u32> = names.iter().map(|name| name[11..].parse()).collect::<Result<_, _>>()?;
    let routed_partition = native_index::route_episode(identity.stored_revision, &increments, &present)?;
    let mut found = None;
    for name in names {
        let partition: u32 = name[11..].parse()?;
        if partition != routed_partition {
            continue;
        }
        let Some(group_offset) = physical_index.group_offset_for(&name, channel, element_id) else {
            continue;
        };
        let stored = file.read_stream_with_limit(&name, options.max_stream_bytes)?;
        let prepared = compression::prepare_stream_for_inflate(&name, &stored);
        let mut offsets = BTreeSet::new();
        offsets.insert(group_offset);
        native_segments::walk_selected(
            &prepared,
            &registry,
            options.max_group_bytes,
            &offsets,
            |source, bytes| {
                ensure!(source.content_key.is_none(), "selected record is embedded content");
                ensure!(source.channel == channel, "physical index selected a channel mismatch");
                let records = physical_group_records(bytes, id_bytes, channel)?;
                ensure!(
                    records.len() as u64 == source.declared_objects
                        && records.iter().map(|record| record.body_len as u64).sum::<u64>()
                            == source.declared_body_bytes,
                    "native group record counts/body sizes disagree"
                );
                let record = records
                    .into_iter()
                    .find(|record| record.id == element_id)
                    .ok_or_else(|| anyhow::anyhow!("indexed record absent from its selected physical group"))?;
                ensure!(found.is_none(), "ambiguous current physical record {element_id} in channel{channel}");
                let preview = record.body.len().min(64 * 1024);
                found = Some(PhysicalRecordProbe {
                    element_id,
                    channel,
                    source: RecordSource {
                        stream: name.clone(),
                        group: source.clone(),
                        group_record_offset: record.offset,
                        body_bytes: record.body.len(),
                        body_sha256: format!("{:x}", Sha256::digest(record.body)),
                    },
                    header_hex: hex_bytes(record.header),
                    trailer_hex: hex_bytes(record.trailer),
                    body_hex: hex_bytes(&record.body[..preview]),
                    body_preview_truncated: preview != record.body.len(),
                });
                Ok(())
            },
        )?;
    }
    found.ok_or_else(|| anyhow::anyhow!("current physical record {element_id} absent from channel{channel}"))
}

#[derive(Debug)]
struct PhysicalGroupRecord<'a> {
    id: u64,
    offset: usize,
    body_len: usize,
    header: &'a [u8],
    body: &'a [u8],
    trailer: &'a [u8],
}

fn physical_group_records(bytes: &[u8], id_bytes: usize, channel: u64) -> Result<Vec<PhysicalGroupRecord<'_>>> {
    let header_len = match channel {
        101 => id_bytes + 4,
        102 | 103 => id_bytes + 8,
        _ => anyhow::bail!("unsupported native record channel {channel}"),
    };
    let mut records = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        ensure!(bytes.len() - pos >= header_len + 4, "truncated channel record header");
        let id = if id_bytes == 4 {
            u64::from(u32::from_le_bytes(bytes[pos..pos + 4].try_into()?))
        } else {
            u64::from_le_bytes(bytes[pos..pos + 8].try_into()?)
        };
        let body_len = u32::from_le_bytes(bytes[pos + header_len - 4..pos + header_len].try_into()?) as usize;
        let body_start = pos + header_len;
        let body_end = body_start.checked_add(body_len).ok_or_else(|| anyhow::anyhow!("native record size overflow"))?;
        ensure!(
            body_end + 4 <= bytes.len()
                && u32::from_le_bytes(bytes[body_end..body_end + 4].try_into()?) as usize == body_len,
            "native record dual lengths disagree"
        );
        records.push(PhysicalGroupRecord {
            id,
            offset: pos,
            body_len,
            header: &bytes[pos..body_start],
            body: &bytes[body_start..body_end],
            trailer: &bytes[body_end..body_end + 4],
        });
        pos = body_end + 4;
    }
    Ok(records)
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn records_count(bytes: &[u8], id_bytes: usize, channel: u64) -> Result<usize> {
    let header = match channel {
        101 => id_bytes + 4,
        102 | 103 => id_bytes + 8,
        _ => anyhow::bail!("unsupported native record channel {channel}"),
    };
    let mut pos = 0;
    let mut count = 0;
    while pos < bytes.len() {
        ensure!(
            bytes.len() - pos >= header + 4,
            "truncated channel record header"
        );
        let length = u32::from_le_bytes(bytes[pos + header - 4..pos + header].try_into()?) as usize;
        let end = pos
            .checked_add(header)
            .and_then(|start| start.checked_add(length))
            .ok_or_else(|| anyhow::anyhow!("native record size overflow"))?;
        ensure!(end + 4 <= bytes.len(), "truncated channel record body");
        pos = end + 4;
        count += 1;
    }
    Ok(count)
}
fn decode_single(prepared: &[u8], offset: usize) -> Result<(Vec<u8>, StreamEnvelope)> {
    let compressed = prepared
        .get(offset..)
        .ok_or_else(|| anyhow::anyhow!("truncated native stream prefix"))?;
    let mut decoder = flate2::bufread::GzDecoder::new(compressed);
    let mut bytes = Vec::new();
    (&mut decoder)
        .take(256 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 256 * 1024 * 1024,
        "native stream inflate budget exceeded"
    );
    let suffix = *decoder.get_ref();
    ensure!(
        suffix.len() <= compression::REVIT_STORED_PAGE_BYTES,
        "native stream opaque suffix exceeds one storage page"
    );
    Ok((
        bytes,
        StreamEnvelope {
            prepared_bytes: prepared.len(),
            member_offset: offset,
            member_bytes: compressed.len() - suffix.len(),
            opaque_suffix_bytes: suffix.len(),
            opaque_suffix_sha256: format!("{:x}", Sha256::digest(suffix)),
            suffix_interpreted: suffix.is_empty(),
        },
    ))
}

/// Read one CRC/ISIZE-validated member. Callers needing storage coverage should
/// use `extract`, whose summary records the uninterpreted suffix explicitly.
pub fn read_single(file: &mut RevitFile, name: &str) -> Result<Vec<u8>> {
    Ok(read_single_envelope(file, name)?.0)
}
fn read_single_envelope(file: &mut RevitFile, name: &str) -> Result<(Vec<u8>, StreamEnvelope)> {
    let stored = file.read_stream_with_limit(name, 512 * 1024 * 1024)?;
    let prepared = compression::prepare_stream_for_inflate(name, &stored);
    decode_single(&prepared, if name == "Formats/Latest" { 0 } else { 8 })
}
/// Extract selected current indexed records. Unsupported individual object
/// graphs produce explicit records and do not discard other decoded owners.
/// Framing, identity and routing errors abort instead of choosing a candidate.
pub fn extract(
    file: &mut RevitFile,
    options: &Options,
    emit: impl FnMut(Record) -> Result<()>,
) -> Result<Summary> {
    let context = build_definition_context_inner(file, options, None)?;
    let mut summary = extract_records(
        file,
        options,
        false,
        false,
        &context.registry,
        false,
        context.extensible_storage_catalog.as_ref(),
        None,
        emit,
    )?;
    summary.parameter_binding_context = serde_json::json!({
        "bindings": context.registry.bindings,
        "family_categories": context.registry.family_categories,
        "symbol_families": context.registry.symbol_families,
    });
    summary.parameter_definitions = context.registry.definitions;
    summary.unit_formats = context.registry.unit_formats;
    summary.builtin_catalog_provenance = context.registry.builtin_catalog_provenance;
    summary.definition_diagnostics = context.diagnostics;
    summary.extensible_storage_catalog_diagnostic = context.extensible_storage_catalog_diagnostic;
    Ok(summary)
}

/// Extract definition-resolved records while reusing a validated physical
/// group index for the selected output pass.
pub fn extract_using_index(
    file: &mut RevitFile,
    options: &Options,
    physical_index: &PhysicalIndex,
    emit: impl FnMut(Record) -> Result<()>,
) -> Result<Summary> {
    let definitions = build_definition_context(file, options, physical_index)?;
    extract_using_index_with_context(file, options, physical_index, &definitions, true, emit)
}

/// Build the reusable document-wide definition registry for selected metadata
/// extraction. This pass does not materialize extensible-storage values.
pub fn build_definition_context(
    file: &mut RevitFile,
    options: &Options,
    physical_index: &PhysicalIndex,
) -> Result<DefinitionContext> {
    build_definition_context_inner(file, options, Some(physical_index))
}

fn build_definition_context_inner(
    file: &mut RevitFile,
    options: &Options,
    physical_index: Option<&PhysicalIndex>,
) -> Result<DefinitionContext> {
    // Definition owners may follow their users physically. A bounded first pass
    // decodes only schema-declared definition owners; selected output still streams.
    let mut definitions = crate::native_parameter_definitions::Registry::default();
    let mut diagnostics = BTreeMap::new();
    let mut definition_owner_ids = BTreeSet::new();
    let mut definition_owner_category_candidates = BTreeMap::new();
    let mut definition_options = options.clone();
    definition_options.selected_ids.clear();
    definition_options.channels = BTreeSet::from([102]);
    let definition_summary = extract_records(
        file,
        &definition_options,
        true,
        false,
        &crate::native_parameter_definitions::Registry::default(),
        true,
        None,
        physical_index,
        |record| {
            definition_owner_ids.insert(record.identity.element_id);
            if let Some(root) = record
                .graph
                .as_ref()
                .and_then(|graph| graph.objects.first())
            {
                let direct_category_id = root
                    .fields
                    .get("m_categoryId")
                    .map(crate::native_metadata::identifier)
                    .transpose()?;
                let family_id =
                    crate::native_parameter_definitions::is_family_symbol_definition_owner(
                        &root.class_name,
                    )
                    .then(|| root.fields.get("m_familyId"))
                    .flatten()
                    .map(crate::native_metadata::identifier)
                    .transpose()?;
                ensure!(
                    definition_owner_category_candidates
                        .insert(
                            record.identity.element_id,
                            DefinitionOwnerCategoryCandidate {
                                class_name: root.class_name.clone(),
                                direct_category_id,
                                family_id,
                            },
                        )
                        .is_none(),
                    "duplicate definition owner category candidate"
                );
            }
            let result =
                    match record.graph.as_ref() {
                        Some(graph)
                            if graph
                                .objects
                                .first()
                                .is_some_and(|root| root.class_name == "UnitsElem") =>
                        {
                            definitions.ingest_units(graph)
                        }
                        Some(graph)
                            if graph.objects.first().is_some_and(|root| {
                                matches!(
                                    root.class_name.as_str(),
                                    "ParamBinding" | "Family"
                                )
                                    || crate::native_parameter_definitions::is_family_symbol_definition_owner(&root.class_name)
                            }) =>
                        {
                            definitions.ingest_binding_context(graph)
                        }
                        Some(graph) => crate::native_parameter_definitions::project(graph)
                            .and_then(|definition| match definition {
                                Some(definition) => definitions.insert(definition),
                                None => Err(anyhow::anyhow!(
                                    "definition owner has no nonnull definition"
                                )),
                            }),
                        None => Err(anyhow::anyhow!(
                            record
                                .diagnostic
                                .unwrap_or_else(|| "unsupported definition graph".into())
                        )),
                    };
            if let Err(error) = result {
                diagnostics.insert(record.identity.element_id, error.to_string());
            }
            Ok(())
        },
    )?;
    definitions.enable_builtin_catalog(&definition_summary.schema_sha256)?;
    Ok(DefinitionContext {
        registry: definitions,
        schema_sha256: definition_summary.schema_sha256,
        diagnostics,
        definition_owner_ids,
        definition_owner_category_candidates,
        extensible_storage_catalog: definition_summary.extensible_storage_catalog,
        extensible_storage_catalog_diagnostic: definition_summary
            .extensible_storage_catalog_diagnostic,
    })
}

/// Extract definition-resolved records with a caller-owned registry. The
/// extensible-storage catalog decode is explicit because package shards only
/// need typed metadata rows and should not rematerialize the document catalog.
pub fn extract_using_index_with_context(
    file: &mut RevitFile,
    options: &Options,
    physical_index: &PhysicalIndex,
    context: &DefinitionContext,
    decode_extensible_storage: bool,
    emit: impl FnMut(Record) -> Result<()>,
) -> Result<Summary> {
    let mut summary = extract_records(
        file,
        options,
        false,
        false,
        &context.registry,
        decode_extensible_storage,
        context.extensible_storage_catalog.as_ref(),
        Some(physical_index),
        emit,
    )?;
    summary.parameter_binding_context = serde_json::json!({
        "bindings": context.registry.bindings,
        "family_categories": context.registry.family_categories,
        "symbol_families": context.registry.symbol_families,
    });
    summary.parameter_definitions = context.registry.definitions.clone();
    summary.unit_formats = context.registry.unit_formats.clone();
    summary.builtin_catalog_provenance = context.registry.builtin_catalog_provenance.clone();
    summary.definition_diagnostics = context.diagnostics.clone();
    summary.extensible_storage_catalog_diagnostic =
        context.extensible_storage_catalog_diagnostic.clone();
    Ok(summary)
}

/// Scan only direct root fields for current selected records.  This is intended
/// for bounded selection decisions; it deliberately does not produce metadata
/// suitable for delivery.  Chosen records must later be decoded once in full.
pub fn extract_roots_using_index_with_context(
    file: &mut RevitFile,
    options: &Options,
    physical_index: &PhysicalIndex,
    context: &DefinitionContext,
    emit: impl FnMut(Record) -> Result<()>,
) -> Result<Summary> {
    extract_records(
        file,
        options,
        false,
        true,
        &context.registry,
        false,
        context.extensible_storage_catalog.as_ref(),
        Some(physical_index),
        emit,
    )
}

/// Extract records without the document-wide parameter-definition pass.
///
/// Saved graphics, material, and other geometry consumers need the decoded
/// owner graph but do not use parameter-definition bindings. Keeping this as
/// an explicit API makes the cheaper contract visible without weakening the
/// normal metadata extraction path above.
pub fn extract_without_definitions(
    file: &mut RevitFile,
    options: &Options,
    emit: impl FnMut(Record) -> Result<()>,
) -> Result<Summary> {
    extract_records(
        file,
        options,
        false,
        false,
        &crate::native_parameter_definitions::Registry::default(),
        false,
        None,
        None,
        emit,
    )
}

/// Extract records without definitions while reusing a validated physical
/// group index. This is intended for selected saved-graphics closure passes.
pub fn extract_without_definitions_using_index(
    file: &mut RevitFile,
    options: &Options,
    physical_index: &PhysicalIndex,
    emit: impl FnMut(Record) -> Result<()>,
) -> Result<Summary> {
    extract_without_definitions_using_index_with_catalog(file, options, physical_index, None, emit)
}

/// Selected graph extraction using a caller-owned, document-wide ES catalog.
/// Geometry callers use this to avoid losing an otherwise valid saved graph
/// merely because it carries a schema-defined ES payload.
pub fn extract_without_definitions_using_index_with_catalog(
    file: &mut RevitFile,
    options: &Options,
    physical_index: &PhysicalIndex,
    extensible_storage_catalog: Option<&crate::native_extensible_storage::Catalog>,
    emit: impl FnMut(Record) -> Result<()>,
) -> Result<Summary> {
    extract_records(
        file,
        options,
        false,
        false,
        &crate::native_parameter_definitions::Registry::default(),
        false,
        extensible_storage_catalog,
        Some(physical_index),
        emit,
    )
}

fn extract_records(
    file: &mut RevitFile,
    options: &Options,
    definitions_only: bool,
    root_only: bool,
    definitions: &crate::native_parameter_definitions::Registry,
    decode_extensible_storage: bool,
    reusable_extensible_storage_catalog: Option<&crate::native_extensible_storage::Catalog>,
    physical_index: Option<&PhysicalIndex>,
    mut emit: impl FnMut(Record) -> Result<()>,
) -> Result<Summary> {
    let version = file.basic_file_info()?.version;
    let mut global_streams = BTreeMap::new();
    let mut read = |name: &str| -> Result<Vec<u8>> {
        let (bytes, envelope) = read_single_envelope(file, name)?;
        global_streams.insert(name.to_string(), envelope);
        Ok(bytes)
    };
    let registry = schema_registry::parse(&read("Formats/Latest")?)?;
    let episodes = native_index::creation_episodes(&read("Global/History")?, &registry)?;
    let index = native_index::parse(&read("Global/ElemTable")?, &registry, &episodes)?;
    let id_bytes = physical_record_id_bytes(&index)?;
    let increments =
        native_index::storage_increments(&read("Global/DocumentIncrementTable")?, &registry)?;
    let decoded_es_catalog =
        if reusable_extensible_storage_catalog.is_none() && decode_extensible_storage {
            Some(
                read("Global/Latest")
                    .and_then(|bytes| crate::native_es_catalog::decode(&bytes, &registry)),
            )
        } else {
            // Preserve stream coverage when a caller supplied the already-decoded
            // document context, without decoding or retaining another catalog.
            read("Global/Latest")?;
            None
        };
    let catalog = reusable_extensible_storage_catalog.or_else(|| {
        decoded_es_catalog
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|(catalog, _)| catalog)
    });
    let mut names: Vec<_> = file
        .stream_names()
        .iter()
        .filter(|n| n.starts_with("Partitions/"))
        .cloned()
        .collect();
    names.sort();
    let present: BTreeSet<u32> = names
        .iter()
        .map(|n| n[11..].parse())
        .collect::<Result<_, _>>()?;
    let selected: BTreeSet<_> = index
        .identities
        .keys()
        .filter(|id| options.selected_ids.is_empty() || options.selected_ids.contains(id))
        .copied()
        .collect();
    let mut summary = Summary {
        budgets: ResourceBudget {
            max_stream_bytes: options.max_stream_bytes,
            max_group_bytes: options.max_group_bytes,
            max_graph_values: options.max_graph_values,
            max_graph_objects: options.max_graph_objects,
        },
        global_streams,
        revit_version: version,
        schema_sha256: registry.source_sha256.clone(),
        indexed_elements: index.identities.len(),
        graveyard_records: index.graveyard_rows.len(),
        selected_indexed_elements: selected.len(),
        serialized_values_not_evaluated: true,
        requested_ids_absent_from_index: options
            .selected_ids
            .iter()
            .filter(|id| !index.identities.contains_key(id))
            .copied()
            .collect(),
        ..Default::default()
    };
    let mut seen = BTreeSet::new();
    let mut seen_ids = BTreeSet::new();
    // Opt-in diagnostic telemetry for large selected delivery passes.  Root
    // category scans stay quiet; production output is unaffected unless the
    // caller explicitly requests progress on stderr.
    let progress_every = (!root_only)
        .then(|| std::env::var("RVT_NATIVE_PROGRESS_EVERY").ok())
        .flatten()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0);
    // This is deliberately separate from the periodic progress counter: when
    // investigating a long individual graph decode, the counter only advances
    // after decoding succeeds.  A caller can opt in to an exact, pre-decode
    // owner marker without adding any package data or changing normal output.
    let verbose_progress = !root_only
        && std::env::var_os("RVT_NATIVE_PROGRESS_VERBOSE").is_some_and(|value| !value.is_empty());
    let verbose_progress_from = std::env::var("RVT_NATIVE_PROGRESS_VERBOSE_FROM")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1);
    for name in names {
        let partition: u32 = name[11..].parse()?;
        // A physical index is specifically maintained so selected passes do
        // not repeatedly inflate unrelated partition streams.  Calculate the
        // group offsets before reading the CFB stream: a cost-bounded shard
        // with no requested owner in this partition has no extraction work
        // here at all.
        let target_offsets = physical_index
            .filter(|_| !options.selected_ids.is_empty())
            .map(|index| index.offsets_for(&name, &options.channels, &selected));
        if target_offsets.as_ref().is_some_and(BTreeSet::is_empty) {
            continue;
        }
        let stored = file.read_stream_with_limit(&name, options.max_stream_bytes)?;
        let prepared = compression::prepare_stream_for_inflate(&name, &stored);
        let mut visit = |source: &native_segments::GroupSource, bytes: &[u8]| {
            if source.content_key.is_some() {
                summary.skipped_embedded_content_groups += 1;
                return Ok(());
            }
            let header = match source.channel {
                101 => id_bytes + 4,
                102 | 103 => id_bytes + 8,
                _ => anyhow::bail!("unsupported native record channel {}", source.channel),
            };
            let mut pos = 0;
            let mut records = Vec::new();
            let mut body_sum = 0u64;
            while pos < bytes.len() {
                ensure!(
                    bytes.len() - pos >= header + 4,
                    "truncated channel record header"
                );
                let id = if id_bytes == 4 {
                    u64::from(u32::from_le_bytes(bytes[pos..pos + 4].try_into()?))
                } else {
                    u64::from_le_bytes(bytes[pos..pos + 8].try_into()?)
                };
                let length =
                    u32::from_le_bytes(bytes[pos + header - 4..pos + header].try_into()?) as usize;
                let start = pos + header;
                let end = start
                    .checked_add(length)
                    .ok_or_else(|| anyhow::anyhow!("native record size overflow"))?;
                ensure!(
                    end + 4 <= bytes.len()
                        && u32::from_le_bytes(bytes[end..end + 4].try_into()?) as usize == length,
                    "native record dual lengths disagree"
                );
                records.push((id, pos, start, end));
                body_sum += length as u64;
                pos = end + 4;
            }
            ensure!(
                records.len() as u64 == source.declared_objects
                    && body_sum == source.declared_body_bytes,
                "native group record counts/body sizes disagree"
            );
            if !options.channels.contains(&source.channel) {
                return Ok(());
            }
            for (id, offset, start, end) in records {
                if !selected.contains(&id) {
                    continue;
                }
                let identity = &index.identities[&id];
                if native_index::route_episode(identity.stored_revision, &increments, &present)?
                    != partition
                {
                    summary.skipped_historical_records += 1;
                    continue;
                }
                ensure!(
                    seen.insert((source.channel, id)),
                    "ambiguous current record for {id} in channel{}",
                    source.channel
                );
                seen_ids.insert(id);
                let body = &bytes[start..end];
                let class_name = body
                    .get(..2)
                    .and_then(|b| registry.class(u16::from_le_bytes([b[0], b[1]])))
                    .map(|c| c.name.clone());
                if definitions_only {
                    let mut tag = u16::from_le_bytes(
                        body.get(..2)
                            .ok_or_else(|| anyhow::anyhow!("truncated definition candidate tag"))?
                            .try_into()?,
                    );
                    let mut owns_definition = matches!(
                        class_name.as_deref(),
                        Some("UnitsElem" | "ParamBinding" | "Family")
                    );
                    owns_definition |= class_name.as_deref().is_some_and(
                        crate::native_parameter_definitions::is_family_symbol_definition_owner,
                    );
                    for _ in 0..128 {
                        let Some(class) = registry.class(tag) else {
                            break;
                        };
                        owns_definition |=
                            class.fields.iter().any(|field| field.name == "m_pParamDef");
                        tag = class.parent_reference.tag;
                    }
                    if !owns_definition {
                        continue;
                    }
                }
                if verbose_progress && summary.emitted_records + 1 >= verbose_progress_from {
                    eprintln!(
                        "native-document decode-start: channel={} emitted={} id={} class={} stream={} body_bytes={}",
                        source.channel,
                        summary.emitted_records + 1,
                        id,
                        class_name.as_deref().unwrap_or("<unregistered>"),
                        name,
                        body.len(),
                    );
                }
                let decoded = if root_only {
                    crate::native_parameters::decode_root_with_catalog_usage(
                        body,
                        &registry,
                        &crate::native_parameters::GraphLimits {
                            max_values: options.max_graph_values,
                            max_objects: 1,
                            max_depth: crate::native_parameters::GraphLimits::default().max_depth,
                        },
                        catalog,
                    )
                    .map(|root| {
                        (
                            crate::native_parameters::ObjectGraph {
                                consumed_bytes: root.consumed_bytes,
                                objects: vec![crate::native_parameters::GraphObject {
                                    class_tag: root.class_tag,
                                    class_name: root.class_name,
                                    token: 0,
                                    start: 2,
                                    fields_end: root.consumed_bytes,
                                    fields: root.fields,
                                }],
                                edges: vec![],
                            },
                            crate::native_parameters::GraphUsage {
                                values: root.values,
                                objects: 1,
                            },
                            false,
                        )
                    })
                } else {
                    decode_graph_with_adaptive_budget(body, &registry, options, catalog)
                };
                let decoded = decoded.and_then(|(graph, usage, recovered)| {
                    validate_serialized_owner_identity(&graph, source.channel, id)?;
                    Ok((graph, usage, recovered))
                });
                let (status, diagnostic, graph) = match decoded {
                    Ok((graph, usage, recovered)) => {
                        summary.max_graph_objects_observed =
                            summary.max_graph_objects_observed.max(usage.objects);
                        summary.max_graph_values_observed =
                            summary.max_graph_values_observed.max(usage.values);
                        if recovered {
                            summary.adaptive_graph_retries += 1;
                            summary.adaptive_graph_recoveries += 1;
                        }
                        summary.complete_graph_records += 1;
                        ("complete_bounded_graph", None, Some(graph))
                    }
                    Err(e) => {
                        summary.unsupported_graph_records += 1;
                        summary.refused_graph_ids.push(id);
                        if let Some(class_name) = &class_name {
                            *summary
                                .refused_graph_classes
                                .entry(class_name.clone())
                                .or_default() += 1;
                        }
                        ("unsupported_graph", Some(e.to_string()), None)
                    }
                };
                let coverage = summary
                    .classes
                    .entry(
                        class_name
                            .clone()
                            .unwrap_or_else(|| "<unregistered>".into()),
                    )
                    .or_default();
                coverage.records += 1;
                coverage.body_bytes += body.len() as u64;
                if graph.is_some() {
                    coverage.complete_graphs += 1;
                    coverage.complete_graph_body_bytes += body.len() as u64;
                } else {
                    coverage.unsupported_graphs += 1;
                    *coverage
                        .diagnostics
                        .entry(diagnostic.clone().unwrap_or_default())
                        .or_default() += 1;
                }
                let (saved_metadata, metadata_diagnostic) =
                    match (!root_only).then(|| graph.as_ref()).flatten().map(|graph| {
                        crate::native_metadata::project_with_definitions(graph, definitions)
                    }) {
                        Some(Ok(value)) => (Some(value), None),
                        Some(Err(error)) => (None, Some(error.to_string())),
                        None => (None, None),
                    };
                summary.projected_metadata_records += usize::from(saved_metadata.is_some());
                summary.unsupported_metadata_records += usize::from(metadata_diagnostic.is_some());
                summary.emitted_records += 1;
                if progress_every.is_some_and(|every| summary.emitted_records % every == 0) {
                    eprintln!(
                        "native-document progress: channel={} emitted={} id={} class={} stream={}",
                        source.channel,
                        summary.emitted_records,
                        id,
                        class_name.as_deref().unwrap_or("<unregistered>"),
                        name,
                    );
                }
                let (derived_default_ifc_guid, derived_identifier_diagnostic) =
                    match crate::native_metadata::derive_default_ifc_guid(&identity.unique_id) {
                        Ok(value) => (Some(value), None),
                        Err(error) => (None, Some(error.to_string())),
                    };
                let (effective_ifc_parameter, effective_ifc_parameter_diagnostic) = match (
                    class_name.as_deref(),
                    saved_metadata.as_ref(),
                    derived_default_ifc_guid.as_deref(),
                ) {
                    (Some(class), Some(metadata), Some(default_guid)) => {
                        match crate::native_metadata::effective_ifc_parameter(
                            class,
                            metadata,
                            default_guid,
                            definitions,
                        ) {
                            Ok(value) => (value, None),
                            Err(error) => (None, Some(error.to_string())),
                        }
                    }
                    _ => (None, None),
                };
                emit(Record {
                    identity: identity.clone(),
                    derived_default_ifc_guid,
                    derived_identifier_diagnostic,
                    effective_ifc_parameter,
                    effective_ifc_parameter_diagnostic,
                    channel: source.channel,
                    class_name,
                    status: status.into(),
                    diagnostic,
                    source: RecordSource {
                        stream: name.clone(),
                        group: source.clone(),
                        group_record_offset: offset,
                        body_bytes: body.len(),
                        body_sha256: format!("{:x}", Sha256::digest(body)),
                    },
                    graph,
                    saved_metadata,
                    metadata_diagnostic,
                })?;
            }
            Ok(())
        };
        let stats = if let Some(offsets) = target_offsets.as_ref() {
            native_segments::walk_selected(
                &prepared,
                &registry,
                options.max_group_bytes,
                offsets,
                &mut visit,
            )?
        } else {
            native_segments::walk(&prepared, &registry, options.max_group_bytes, &mut visit)?
        };
        summary.partitions.insert(name, stats);
    }
    summary.selected_ids_without_records = selected.difference(&seen_ids).copied().collect();
    if let Some(es_catalog) = decoded_es_catalog {
        match es_catalog {
            Ok((catalog, source)) => {
                summary.extensible_storage_catalog = Some(catalog);
                summary.extensible_storage_catalog_source = Some(source);
            }
            Err(error) => {
                summary.extensible_storage_catalog_diagnostic = Some(format!("{error:#}"))
            }
        }
    }
    Ok(summary)
}

/// The project index is the source-of-truth for the physical partition record
/// identifier width.  Its two validated layouts have a 28-byte (32-bit id) or
/// 40-byte (64-bit id) row, respectively.  Do not infer this from the Revit
/// release: a document can be upgraded, while its persisted native layout is
/// what controls the bytes that follow each partition group marker.
fn physical_record_id_bytes(index: &native_index::ProjectIndex) -> Result<usize> {
    match index.row_stride {
        28 => Ok(4),
        40 => Ok(8),
        stride => anyhow::bail!(
            "unsupported ElemTable row stride {stride}; physical record framing is not qualified"
        ),
    }
}

/// Channel-102 owners are identified authoritatively by the validated element
/// index and physical record header.  Most also repeat that id as `m_id` in
/// their root graph; when present it must agree. Some valid owner classes
/// (for example EnergyAnalysisSurface) omit it or serialize `m_id: null`, so
/// treat that as unavailable redundant evidence rather than declaring the
/// otherwise complete graph unsupported.
fn validate_serialized_owner_identity(
    graph: &ObjectGraph,
    channel: u64,
    indexed_id: u64,
) -> Result<()> {
    if channel != 102 {
        return Ok(());
    }
    let root = graph
        .objects
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty owner graph"))?;
    let Some(saved) = root.fields.get("m_id") else {
        return Ok(());
    };
    if saved.is_null() {
        return Ok(());
    }
    let saved_id = crate::native_metadata::identifier(saved)?;
    ensure!(
        u64::try_from(saved_id).ok() == Some(indexed_id),
        "serialized owner identity disagrees with indexed record {indexed_id}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn project_index_with_stride(row_stride: usize) -> native_index::ProjectIndex {
        native_index::ProjectIndex {
            identities: BTreeMap::new(),
            graveyard_rows: Vec::new(),
            row_stride,
            source_fields: serde_json::Value::Null,
        }
    }

    #[test]
    fn physical_record_id_width_is_derived_from_validated_index_layout() {
        assert_eq!(
            physical_record_id_bytes(&project_index_with_stride(28)).unwrap(),
            4
        );
        assert_eq!(
            physical_record_id_bytes(&project_index_with_stride(40)).unwrap(),
            8
        );
        assert!(physical_record_id_bytes(&project_index_with_stride(32)).is_err());
    }

    #[test]
    fn physical_group_probe_framing_uses_both_length_sentinels() {
        let mut bytes = Vec::new();
        // Channel 102 / 32-bit owner id: id + opaque word + length + body + length.
        bytes.extend_from_slice(&42u32.to_le_bytes());
        bytes.extend_from_slice(&0xfeed_beefu32.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&[7, 8, 9]);
        bytes.extend_from_slice(&3u32.to_le_bytes());
        let records = physical_group_records(&bytes, 4, 102).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, 42);
        assert_eq!(records[0].offset, 0);
        assert_eq!(records[0].header.len(), 12);
        assert_eq!(records[0].body, [7, 8, 9]);
        assert_eq!(records[0].trailer, 3u32.to_le_bytes());
        bytes[15] = 2;
        assert!(physical_group_records(&bytes, 4, 102).is_err());
    }

    fn owner_graph(m_id: serde_json::Value) -> ObjectGraph {
        ObjectGraph {
            consumed_bytes: 0,
            objects: vec![native_parameters::GraphObject {
                class_tag: 12,
                class_name: "Owner".into(),
                token: 0,
                start: 0,
                fields_end: 0,
                fields: serde_json::json!({"m_id":m_id}),
            }],
            edges: Vec::new(),
        }
    }

    #[test]
    fn serialized_owner_identity_requires_a_match_but_allows_explicit_null() {
        assert!(
            validate_serialized_owner_identity(&owner_graph(serde_json::Value::Null), 102, 42)
                .is_ok()
        );
        let missing = ObjectGraph {
            consumed_bytes: 0,
            objects: vec![native_parameters::GraphObject {
                class_tag: 12,
                class_name: "Owner".into(),
                token: 0,
                start: 0,
                fields_end: 0,
                fields: serde_json::json!({}),
            }],
            edges: Vec::new(),
        };
        assert!(validate_serialized_owner_identity(&missing, 102, 42).is_ok());
        assert!(
            validate_serialized_owner_identity(&owner_graph(serde_json::json!(42)), 102, 42)
                .is_ok()
        );
        assert!(
            validate_serialized_owner_identity(&owner_graph(serde_json::json!(41)), 102, 42)
                .is_err()
        );
    }

    #[test]
    fn physical_index_exposes_deduplicated_channel_ids() {
        let mut index = PhysicalIndex::default();
        index
            .groups
            .entry("Partitions/0".into())
            .or_default()
            .insert((102, 12), 10);
        index
            .groups
            .entry("Partitions/1".into())
            .or_default()
            .insert((102, 13), 20);
        index
            .groups
            .entry("Partitions/1".into())
            .or_default()
            .insert((103, 12), 30);
        index.record_bytes.insert((102, 12), 4096);
        index.owner_streams.insert(
            12,
            BTreeSet::from(["Partitions/0".into(), "Partitions/1".into()]),
        );
        index
            .owner_streams
            .insert(13, BTreeSet::from(["Partitions/1".into()]));
        assert_eq!(index.current_ids(102), BTreeSet::from([12, 13]));
        assert_eq!(index.current_ids(103), BTreeSet::from([12]));
        assert!(index.current_ids(101).is_empty());
        assert_eq!(index.group_offset_for("Partitions/0", 102, 12), Some(10));
        assert_eq!(index.group_offset_for("Partitions/1", 103, 12), Some(30));
        assert_eq!(index.group_offset_for("Partitions/0", 103, 12), None);
        assert_eq!(index.record_bytes(102, 12), 4096);
        assert_eq!(index.record_bytes(102, 13), 0);
    }

    #[test]
    fn physical_index_plans_partition_local_cost_bounded_shards() {
        let mut index = PhysicalIndex::default();
        index
            .groups
            .entry("Partitions/0".into())
            .or_default()
            .extend([((102, 1), 10), ((103, 1), 20), ((102, 3), 30)]);
        index
            .groups
            .entry("Partitions/1".into())
            .or_default()
            .extend([((102, 2), 10), ((103, 2), 20)]);
        index.record_bytes.extend([
            ((102, 1), 4),
            ((103, 1), 4),
            ((102, 2), 4),
            ((103, 2), 4),
            ((102, 3), 4),
        ]);
        index.owner_streams.extend([
            (1, BTreeSet::from(["Partitions/0".into()])),
            (2, BTreeSet::from(["Partitions/1".into()])),
            (3, BTreeSet::from(["Partitions/0".into()])),
        ]);
        assert_eq!(
            index.partition_local_shards(&[1, 2, 3], 10, 8).unwrap(),
            vec![vec![1], vec![3], vec![2]]
        );
        assert!(index.partition_local_shards(&[4], 1, 0).is_err());
    }

    #[test]
    fn physical_index_splits_one_cost_group_by_owner_cap() {
        let mut index = PhysicalIndex::default();
        index
            .groups
            .entry("Partitions/0".into())
            .or_default()
            .extend([((102, 1), 10), ((102, 2), 20), ((102, 3), 30)]);
        index
            .record_bytes
            .extend([((102, 1), 1024), ((102, 2), 1024), ((102, 3), 1024)]);
        index.owner_streams.extend([
            (1, BTreeSet::from(["Partitions/0".into()])),
            (2, BTreeSet::from(["Partitions/0".into()])),
            (3, BTreeSet::from(["Partitions/0".into()])),
        ]);
        assert_eq!(
            index
                .partition_local_shards(&[1, 2, 3], 2, 64 * 1024)
                .unwrap(),
            vec![vec![1, 2], vec![3]]
        );
    }

    fn member() -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"saved state").unwrap();
        encoder.finish().unwrap()
    }
    #[test]
    fn opaque_suffix_is_accounted_separately_from_verified_payload() {
        let mut data = member();
        let size = data.len();
        data.extend_from_slice(&[0, 3, 9, 0]);
        let (payload, envelope) = decode_single(&data, 0).unwrap();
        assert_eq!(payload, b"saved state");
        assert_eq!(envelope.member_bytes, size);
        assert_eq!(envelope.opaque_suffix_bytes, 4);
        assert!(!envelope.suffix_interpreted);
    }
    #[test]
    fn damaged_or_truncated_member_is_not_an_opaque_suffix() {
        let mut data = member();
        data.pop();
        assert!(decode_single(&data, 0).is_err());
        let mut data = member();
        let crc = data.len() - 8;
        data[crc] ^= 1;
        assert!(decode_single(&data, 0).is_err());
    }
}
