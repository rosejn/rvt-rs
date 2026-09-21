//! Explicit saved connector topology. Spatial coincidence never creates an edge.
#[path = "native_connector_geometry.rs"]
mod connector_geometry;
use crate::{
    native_document::Record, native_equipment::Identity, native_metadata::identifier,
    native_parameters::ObjectGraph,
};
use anyhow::{Result, ensure};
pub use connector_geometry::{Frame as ConnectorFrame, Geometry as ConnectorGeometry};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PortKey {
    pub owner_element_id: i64,
    pub connector_id: i64,
}
#[derive(Debug, Clone, Serialize)]
pub struct Source {
    pub owner: Identity,
    pub source_object: usize,
    pub source_field: String,
    pub body_sha256: String,
    pub stream: String,
    pub group_record_offset: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct Modifier {
    pub class_name: String,
    pub fields: Value,
    pub source: Source,
}
#[derive(Debug, Clone, Serialize)]
pub struct Observation {
    pub value: Value,
    pub raw_value: Value,
    pub spec_type_id: Option<&'static str>,
    pub value_unit_basis: &'static str,
    pub interpretation: &'static str,
    pub source: Source,
}
#[derive(Debug, Clone, Serialize)]
pub struct Port {
    pub geometry: ConnectorGeometry,
    pub observations: BTreeMap<String, Observation>,
    pub raw_unplaced_owner_id: Option<i64>,
    pub key: PortKey,
    pub owner: Identity,
    pub owner_class: String,
    pub mode: i64,
    pub modifiers: Vec<Modifier>,
    pub source: Source,
}
#[derive(Debug, Clone, Serialize)]
pub struct Edge {
    pub source_port: PortKey,
    pub target_port: PortKey,
    pub raw_target_mode: i64,
    pub kind: &'static str,
    pub target_owner: Option<Identity>,
    pub resolution: &'static str,
    pub reciprocal_reference: bool,
    pub source: Source,
}
#[derive(Debug, Clone, Serialize)]
pub struct Manager {
    pub owner: Identity,
    pub class_name: String,
    pub deleted_connector_ids: Value,
    pub source: Source,
}
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub element_id: u64,
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct SystemMember {
    /// The linked owner named by the saved logical system reference.
    pub owner: Identity,
    /// Exact serialized reference that establishes this membership.
    pub source: Source,
}
#[derive(Debug, Serialize)]
pub struct PanelDistribution {
    /// The FamilyInstance that carries the built-in electrical distribution
    /// system binding. This is not inferred from connector adjacency.
    pub owner: Identity,
    /// Raw referenced ElementId stored at built-in parameter -1140064.
    pub distribution_system_id: i64,
    pub source: Source,
    /// Saved built-in Panel Name (-1140078), retained separately from a
    /// circuit's API panel name because it applies only through a qualified
    /// base-equipment relationship.
    pub stored_panel_name: Option<String>,
    pub panel_name_status: &'static str,
    pub panel_name_source: Option<Source>,
}
#[derive(Debug, Serialize)]
pub struct System {
    pub owner: Identity,
    /// Native owner class identifies the admitted system family without
    /// inferring a domain from its connected ports.
    pub owner_class: String,
    /// `m_strName` when it is present as a serialized string. This is a
    /// stored native value, not a synthesized system label.
    pub stored_name: Option<String>,
    /// `present` means `stored_name` is a string; `missing` means the field
    /// was serialized with an unsupported value; `unavailable` means this
    /// qualified owner graph did not contain the field.
    pub name_status: &'static str,
    pub name_source: Option<Source>,
    /// Class-qualified serialized system facts. Raw electrical enum and
    /// display-derived values remain distinct rather than being guessed.
    pub observations: BTreeMap<String, Observation>,
    /// Electrical circuit members resolved from saved system logical edges;
    /// this is not an inferred physical-connectivity set.
    pub logical_members: Vec<SystemMember>,
    pub logical_member_status: &'static str,
    /// Revit circuit member semantics exclude a resolved base-equipment owner.
    /// The complete saved logical set remains above for lossless provenance.
    pub members: Vec<SystemMember>,
    pub member_status: &'static str,
    pub base_connector_references: Value,
    pub base_equipment: Option<Identity>,
    pub base_equipment_status: &'static str,
    pub base_equipment_sources: Vec<Source>,
}
#[derive(Debug, Serialize)]
pub struct Inventory {
    pub complete_supported_connector_geometry: bool,
    pub systems: Vec<System>,
    pub panel_distributions: Vec<PanelDistribution>,
    pub format: &'static str,
    pub complete_supported_network: bool,
    pub complete_network_parity: bool,
    pub ports: Vec<Port>,
    pub edges: Vec<Edge>,
    pub managers: Vec<Manager>,
    pub diagnostics: Vec<Diagnostic>,
    pub ignored_record_classes: BTreeMap<String, usize>,
    pub value_semantics: &'static str,
}
impl Inventory {
    /// Owner IDs named by saved connector references, including electrical
    /// system base-connector rows. This is structural closure only; it does
    /// not imply that the target record is present or that the edge resolves.
    pub fn referenced_owner_ids(&self) -> std::collections::BTreeSet<u64> {
        let mut ids = self
            .edges
            .iter()
            .filter_map(|edge| u64::try_from(edge.target_port.owner_element_id).ok())
            .collect::<std::collections::BTreeSet<_>>();
        for system in &self.systems {
            if let Some(rows) = system.base_connector_references.as_array() {
                for row in rows {
                    if let Ok(id) = crate::native_metadata::identifier(&row["m_id"]) {
                        if let Ok(id) = u64::try_from(id) {
                            ids.insert(id);
                        }
                    }
                }
            }
        }
        ids
    }
}
#[derive(Default)]
pub struct InventoryBuilder {
    geometry: connector_geometry::Context,
    systems: Vec<System>,
    panel_distributions: Vec<PanelDistribution>,
    identities: BTreeMap<i64, Identity>,
    ports: BTreeMap<PortKey, Port>,
    edges: Vec<Edge>,
    managers: Vec<Manager>,
    diagnostics: Vec<Diagnostic>,
    ignored: BTreeMap<String, usize>,
}
impl InventoryBuilder {
    /// IDs named by currently ingested saved references, including system
    /// base-connector rows. The builder remains mutable so bounded callers can
    /// plan closure rounds before finishing resolution once.
    pub fn referenced_owner_ids(&self) -> std::collections::BTreeSet<u64> {
        let mut ids = self
            .edges
            .iter()
            .filter_map(|edge| u64::try_from(edge.target_port.owner_element_id).ok())
            .collect::<std::collections::BTreeSet<_>>();
        for system in &self.systems {
            if let Some(rows) = system.base_connector_references.as_array() {
                for row in rows {
                    if let Ok(id) = crate::native_metadata::identifier(&row["m_id"]) {
                        if let Ok(id) = u64::try_from(id) {
                            ids.insert(id);
                        }
                    }
                }
            }
        }
        ids
    }

    /// IDs required to resolve physical connector geometry. These are kept
    /// separate from saved network-edge closure because a FamilySymbol or
    /// Family is context, not a connector endpoint.
    pub fn geometry_dependency_ids(&self) -> std::collections::BTreeSet<u64> {
        self.geometry
            .dependency_ids()
            .into_iter()
            .filter_map(|id| u64::try_from(id).ok())
            .collect()
    }

    pub fn ingest(&mut self, record: &Record) -> Result<()> {
        if record.channel != 102 {
            return Ok(());
        }
        self.geometry.ingest(record);
        let owner = Identity {
            element_id: record.identity.element_id,
            unique_id: record.identity.unique_id.clone(),
        };
        ensure!(
            self.identities
                .insert(i64::try_from(owner.element_id)?, owner)
                .is_none(),
            "duplicate current network owner"
        );
        let class = record.class_name.as_deref().unwrap_or("<unknown>");
        if !supports_connector_owner_class(class) {
            *self.ignored.entry(class.into()).or_default() += 1;
            return Ok(());
        }
        if let Some(graph) = &record.graph {
            if class == "FamilyInstance" {
                if let Some(binding) = panel_distribution(record, graph)? {
                    self.panel_distributions.push(binding);
                }
            }
            if supports_system_owner_class(class)
                && let Some(root) = graph.objects.first()
            {
                let (stored_name, name_status, name_source) = match root.fields.get("m_strName") {
                    Some(Value::String(name)) => (
                        Some(name.clone()),
                        "present",
                        Some(source(record, 0, "m_strName")),
                    ),
                    Some(_) => (None, "missing", Some(source(record, 0, "m_strName"))),
                    None => (None, "unavailable", None),
                };
                let (base_connector_references, base_equipment_sources) =
                    match root.fields.get("m_baseConnectorIdArray") {
                        Some(references) => (
                            references.clone(),
                            vec![source(record, 0, "m_baseConnectorIdArray")],
                        ),
                        None => (json!([]), vec![]),
                    };
                let observations = system_observations(record, class, &root.fields)?;
                self.systems.push(System {
                    owner: source(record, 0, "").owner,
                    owner_class: class.into(),
                    stored_name,
                    name_status,
                    name_source,
                    observations,
                    logical_members: vec![],
                    logical_member_status: "not_applicable",
                    members: vec![],
                    member_status: "not_applicable",
                    base_connector_references,
                    base_equipment: None,
                    base_equipment_status: "unresolved",
                    base_equipment_sources,
                });
            }
            match project(record, graph) {
                Ok((ports, edges, managers)) => {
                    for port in ports {
                        ensure!(
                            self.ports.insert(port.key.clone(), port).is_none(),
                            "duplicate owner connector ID"
                        );
                    }
                    self.edges.extend(edges);
                    self.managers.extend(managers);
                }
                Err(error) => self.diagnostics.push(Diagnostic {
                    element_id: record.identity.element_id,
                    code: "unsupported_connector_graph".into(),
                    message: error.to_string(),
                }),
            }
        } else {
            self.diagnostics.push(Diagnostic {
                element_id: record.identity.element_id,
                code: "unsupported_connector_graph".into(),
                message: record
                    .diagnostic
                    .clone()
                    .unwrap_or("native graph absent".into()),
            });
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<Inventory> {
        let reference_pairs: std::collections::BTreeSet<_> = self
            .edges
            .iter()
            .map(|e| (e.source_port.clone(), e.target_port.clone()))
            .collect();
        for edge in &mut self.edges {
            edge.target_owner = self
                .identities
                .get(&edge.target_port.owner_element_id)
                .cloned();
            edge.reciprocal_reference =
                reference_pairs.contains(&(edge.target_port.clone(), edge.source_port.clone()));
            edge.resolution = match self.ports.get(&edge.target_port) {
                Some(port) if port.mode == edge.raw_target_mode => "resolved_native_port",
                Some(_) => "target_mode_mismatch",
                None if edge.target_owner.is_some() => "unresolved_connector_on_current_owner",
                None => "unresolved_target_owner",
            };
            if edge.resolution != "resolved_native_port" {
                self.diagnostics.push(Diagnostic {
                    element_id: edge.source.owner.element_id,
                    code: edge.resolution.into(),
                    message: format!(
                        "saved reference targets owner {} connector {} mode {}",
                        edge.target_port.owner_element_id,
                        edge.target_port.connector_id,
                        edge.raw_target_mode
                    ),
                });
            }
        }
        // Connector.IsConnected is admitted only from the complete saved
        // connector-reference set.  In particular, do not use coincident
        // geometry as a substitute for a saved reciprocal physical edge.
        // A positive result has an exact reciprocal resolved cross-owner
        // physical reference; a negative result is emitted only when every
        // saved outgoing reference has resolved, so an incomplete closure
        // cannot masquerade as disconnected.
        let connection_observations = self
            .ports
            .values()
            .filter_map(|port| {
                saved_physical_connection_observation(port, &self.edges)
                    .map(|observation| (port.key.clone(), observation))
            })
            .collect::<Vec<_>>();
        for (key, observation) in connection_observations {
            ensure!(
                self.ports
                    .get_mut(&key)
                    .expect("port collected from current inventory")
                    .observations
                    .insert("is_connected".into(), observation)
                    .is_none(),
                "connector IsConnected conflicts with a saved modifier observation"
            );
        }
        for system in &mut self.systems {
            let system_owner_id = i64::try_from(system.owner.element_id)?;
            if system.owner_class == "RbsElectricalSystem" {
                let member_edges = self
                    .edges
                    .iter()
                    .filter(|edge| {
                        edge.kind == "logical_system"
                            && edge.source_port.owner_element_id == system_owner_id
                            && edge.target_port.owner_element_id != system_owner_id
                    })
                    .collect::<Vec<_>>();
                let complete = member_edges.iter().all(|edge| {
                    edge.resolution == "resolved_native_port" && edge.target_owner.is_some()
                });
                let members = member_edges
                    .iter()
                    .filter_map(|edge| {
                        edge.target_owner.clone().map(|owner| {
                            (
                                owner.element_id,
                                SystemMember {
                                    owner,
                                    source: edge.source.clone(),
                                },
                            )
                        })
                    })
                    .collect::<BTreeMap<_, _>>();
                system.logical_members = members.into_values().collect();
                system.logical_member_status = if complete {
                    "resolved_from_saved_system_logical_edges"
                } else {
                    "unresolved_saved_system_logical_edges"
                };
            }
            let rows = system
                .base_connector_references
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("system base connector array malformed"))?;
            if rows.is_empty() {
                system.base_equipment_status = "no_saved_base_connector";
                continue;
            }
            if rows.len() != 1 {
                system.base_equipment_status = "unqualified_multiple_base_connectors";
                continue;
            }
            let row = &rows[0];
            let key = PortKey {
                owner_element_id: identifier(&row["m_id"])?,
                connector_id: integer(&row["m_nIndex"])?,
            };
            if key.owner_element_id != system_owner_id
                || integer(&row["m_connType"])? != 4
                || !self.ports.get(&key).is_some_and(|p| p.mode == 4)
            {
                system.base_equipment_status = "unresolved_system_base_port";
                continue;
            }
            let links: Vec<_> = self
                .edges
                .iter()
                .filter(|edge| {
                    edge.source_port == key
                        && edge.target_port.owner_element_id != key.owner_element_id
                        && edge.raw_target_mode == 4
                        && edge.resolution == "resolved_native_port"
                })
                .collect();
            if links.len() == 1
                && self
                    .ports
                    .get(&links[0].target_port)
                    .is_some_and(|p| p.owner_class == "FamilyInstance")
            {
                system.base_equipment = links[0].target_owner.clone();
                system.base_equipment_status = "resolved_via_saved_system_base_logical_port";
                system.base_equipment_sources.push(links[0].source.clone());
            } else {
                system.base_equipment_status = "unresolved_base_equipment_reference";
            }
        }
        for system in &mut self.systems {
            if system.owner_class != "RbsElectricalSystem" {
                continue;
            }
            let base_id = system.base_equipment.as_ref().map(|owner| owner.element_id);
            system.members = system
                .logical_members
                .iter()
                .filter(|member| Some(member.owner.element_id) != base_id)
                .cloned()
                .collect();
            system.member_status =
                match (system.logical_member_status, system.base_equipment_status) {
                    (
                        "resolved_from_saved_system_logical_edges",
                        "no_saved_base_connector" | "resolved_via_saved_system_base_logical_port",
                    ) => "resolved_from_saved_system_logical_edges_excluding_base_equipment",
                    ("resolved_from_saved_system_logical_edges", _) => {
                        "unqualified_base_equipment_for_member_projection"
                    }
                    _ => "unresolved_saved_system_logical_edges",
                };
        }
        for system in &self.systems {
            if !matches!(
                system.base_equipment_status,
                "no_saved_base_connector" | "resolved_via_saved_system_base_logical_port"
            ) {
                self.diagnostics.push(Diagnostic{element_id:system.owner.element_id,code:system.base_equipment_status.into(),message:"saved electrical base-port chain is not in the qualified single-base-port scope".into()});
            }
        }
        for port in self.ports.values_mut() {
            port.geometry = self.geometry.project(port);
        }
        let complete_supported_connector_geometry = self.ports.values().all(|p| {
            matches!(
                p.geometry.status.as_str(),
                "resolved" | "not_applicable_logical"
            )
        });
        Ok(Inventory {
            complete_supported_connector_geometry,
            systems: self.systems,
            panel_distributions: self.panel_distributions,
            format: "rvt-native-network/v1",
            complete_supported_network: self.diagnostics.is_empty(),
            complete_network_parity: false,
            ports: self.ports.into_values().collect(),
            edges: self.edges,
            managers: self.managers,
            diagnostics: self.diagnostics,
            ignored_record_classes: self.ignored,
            value_semantics: "serialized_connector_cache_internal_units_no_flow_simulation",
        })
    }
}

fn saved_physical_connection_observation(port: &Port, edges: &[Edge]) -> Option<Observation> {
    if port.mode != 1 {
        return None;
    }
    let outgoing = edges
        .iter()
        .filter(|edge| edge.source_port == port.key)
        .collect::<Vec<_>>();
    let connected = outgoing.iter().find(|edge| {
        edge.kind == "cross_owner_physical"
            && edge.resolution == "resolved_native_port"
            && edges.iter().any(|candidate| {
                candidate.source_port == edge.target_port
                    && candidate.target_port == edge.source_port
                    && candidate.kind == "cross_owner_physical"
                    && candidate.resolution == "resolved_native_port"
            })
    });
    if let Some(edge) = connected {
        return Some(Observation {
            value: json!(true),
            raw_value: json!({
                "edge_kind": "cross_owner_physical",
                "reciprocal_reference": true,
            }),
            spec_type_id: None,
            value_unit_basis: "boolean",
            interpretation: "derived_from_reciprocal_saved_cross_owner_physical_reference",
            source: edge.source.clone(),
        });
    }
    if outgoing
        .iter()
        .all(|edge| edge.resolution == "resolved_native_port")
    {
        return Some(Observation {
            value: json!(false),
            raw_value: json!({
                "outgoing_reference_count": outgoing.len(),
                "all_outgoing_references_resolved": true,
            }),
            spec_type_id: None,
            value_unit_basis: "boolean",
            interpretation: "no_reciprocal_saved_cross_owner_physical_reference_in_complete_reference_set",
            source: port.source.clone(),
        });
    }
    None
}

/// The built-in Distribution System parameter is saved in an element-id value
/// set. It is admitted only on a FamilyInstance that explicitly carries that
/// parameter; a connector or equipment classification never creates this fact.
fn panel_distribution(record: &Record, graph: &ObjectGraph) -> Result<Option<PanelDistribution>> {
    const DISTRIBUTION_SYSTEM_PARAMETER_ID: i64 = -1_140_064;
    const PANEL_NAME_PARAMETER_ID: i64 = -1_140_078;
    let mut result = None;
    let mut panel_name = None;
    let mut panel_name_source = None;
    for (object_index, object) in graph.objects.iter().enumerate() {
        let rows = match object.fields.get("m_paramSet") {
            Some(Value::Array(rows)) => rows,
            Some(_)
                if matches!(
                    object.class_name.as_str(),
                    "ParamValueSetElementId" | "ParamValueSetAString"
                ) =>
            {
                anyhow::bail!("{}.m_paramSet is not an array", object.class_name)
            }
            None => continue,
            Some(_) => continue,
        };
        for (row_index, row) in rows.iter().enumerate() {
            let parameter_id = identifier(&row["m_paramId"])?;
            if object.class_name == "ParamValueSetAString"
                && parameter_id == PANEL_NAME_PARAMETER_ID
            {
                ensure!(
                    panel_name.is_none(),
                    "multiple saved Panel Name values on one FamilyInstance"
                );
                let value = row["m_value"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Panel Name value is not a string"))?;
                panel_name = Some(value.to_owned());
                panel_name_source = Some(source(
                    record,
                    object_index,
                    &format!("m_paramSet[{row_index}].m_value"),
                ));
                continue;
            }
            if object.class_name != "ParamValueSetElementId"
                || parameter_id != DISTRIBUTION_SYSTEM_PARAMETER_ID
            {
                continue;
            }
            ensure!(
                result.is_none(),
                "multiple saved electrical distribution-system bindings on one FamilyInstance"
            );
            result = Some(PanelDistribution {
                owner: source(record, object_index, "").owner,
                distribution_system_id: identifier(&row["m_value"])?,
                source: source(
                    record,
                    object_index,
                    &format!("m_paramSet[{row_index}].m_value"),
                ),
                stored_panel_name: None,
                panel_name_status: "unavailable",
                panel_name_source: None,
            });
        }
    }
    if let Some(binding) = &mut result {
        binding.panel_name_status = if panel_name.is_some() {
            "present"
        } else {
            "unavailable"
        };
        binding.stored_panel_name = panel_name;
        binding.panel_name_source = panel_name_source;
    }
    Ok(result)
}

/// Saved connector graphs observed in the public MEP corpus.  This is an
/// explicit admission list: unknown owner classes remain visible as ignored
/// records until their graph shape is separately qualified.
fn supports_connector_owner_class(class: &str) -> bool {
    matches!(
        class,
        "FamilyInstance"
            | "RbsPipeCurve"
            | "RbsDuctCurve"
            | "RbsConduitCurve"
            | "RbsFlexDuctCurve"
            | "RbsWireCurve"
            | "RbsPipingSystem"
            | "RbsMechanicalSystem"
            | "RbsElectricalSystem"
            | "RbsHvacSystem"
    )
}

/// System rows are an explicit subset of connector-owning classes.  Do not
/// promote arbitrary owners with an `m_strName` field into MEP systems.
fn supports_system_owner_class(class: &str) -> bool {
    matches!(
        class,
        "RbsPipingSystem" | "RbsMechanicalSystem" | "RbsElectricalSystem" | "RbsHvacSystem"
    )
}

fn system_observations(
    record: &Record,
    class: &str,
    fields: &Value,
) -> Result<BTreeMap<String, Observation>> {
    let mut result = BTreeMap::new();
    if class != "RbsElectricalSystem" {
        return Ok(result);
    }
    for (output_key, source_field, spec_type_id, value_unit_basis, interpretation) in [
        (
            "voltage_internal",
            "m_dVoltage",
            Some("autodesk.spec.aec.electrical:potential-2.0.0"),
            "revit_internal_potential",
            "serialized_electrical_system_voltage",
        ),
        (
            "poles",
            "m_nPoles",
            None,
            "count",
            "serialized_electrical_system_poles",
        ),
        (
            "system_type_raw",
            "m_systemType",
            None,
            "native_enumeration",
            "serialized_electrical_system_type_not_api_mapped",
        ),
        (
            "circuit_number_serialized",
            "m_number",
            None,
            "serialized_string",
            "serialized_electrical_circuit_number_not_display_equated",
        ),
        (
            "number_of_elements_serialized",
            "m_numberOfElements",
            None,
            "count",
            "serialized_electrical_system_member_count",
        ),
        (
            "number_of_elements_in_network_serialized",
            "m_numberOfElementsInNetwork",
            None,
            "count",
            "serialized_electrical_system_network_count",
        ),
    ] {
        let Some(value) = fields.get(source_field) else {
            continue;
        };
        result.insert(
            output_key.into(),
            Observation {
                value: value.clone(),
                raw_value: value.clone(),
                spec_type_id,
                value_unit_basis,
                interpretation,
                source: source(record, 0, source_field),
            },
        );
    }
    Ok(result)
}

fn source(record: &Record, index: usize, field: &str) -> Source {
    Source {
        owner: Identity {
            element_id: record.identity.element_id,
            unique_id: record.identity.unique_id.clone(),
        },
        source_object: index,
        source_field: field.into(),
        body_sha256: record.source.body_sha256.clone(),
        stream: record.source.stream.clone(),
        group_record_offset: record.source.group_record_offset,
    }
}
fn target(graph: &ObjectGraph, owner: usize, pointer: &Value) -> Result<Option<usize>> {
    if pointer["pointer_token"].as_u64() == Some(0) {
        return Ok(None);
    }
    let offset = pointer["offset"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("missing connector pointer offset"))?
        as usize;
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.source_object_index == owner && e.pointer_offset == offset)
        .collect();
    ensure!(
        edges.len() == 1,
        "connector pointer requires exactly one owning edge"
    );
    let index = edges[0].target_object_index;
    ensure!(
        index < graph.objects.len(),
        "connector pointer outside graph"
    );
    Ok(Some(index))
}
fn integer(value: &Value) -> Result<i64> {
    value
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("invalid connector integer"))
}
fn project(record: &Record, graph: &ObjectGraph) -> Result<(Vec<Port>, Vec<Edge>, Vec<Manager>)> {
    let root = graph
        .objects
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty network graph"))?;
    ensure!(
        Some(root.class_name.as_str()) == record.class_name.as_deref(),
        "network root class mismatch"
    );
    let mut ports = Vec::new();
    let mut edges = Vec::new();
    let mut managers = Vec::new();
    for field in ["m_pConnectorManager", "m_pConnectorMgr"] {
        let Some(pointer) = root.fields.get(field) else {
            continue;
        };
        let Some(manager_index) = target(graph, 0, pointer)? else {
            continue;
        };
        let manager = &graph.objects[manager_index];
        ensure!(
            matches!(
                manager.class_name.as_str(),
                "FamilyInstanceConnectorManager"
                    | "RbsCurveConnectorManager"
                    | "RbsSystemConnectorManager"
            ),
            "unsupported owner connector manager class"
        );
        managers.push(Manager {
            owner: source(record, 0, field).owner,
            class_name: manager.class_name.clone(),
            deleted_connector_ids: manager.fields["m_setDeletedConnectors"].clone(),
            source: source(record, manager_index, field),
        });
        for pointer in manager.fields["m_connPtrArray"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("missing connector array"))?
        {
            let Some(index) = target(graph, manager_index, pointer)? else {
                continue;
            };
            let connector = &graph.objects[index];
            ensure!(
                connector.class_name == "Connector",
                "manager points to non-connector"
            );
            let key = PortKey {
                owner_element_id: i64::try_from(record.identity.element_id)?,
                connector_id: integer(&connector.fields["m_nIndex"])?,
            };
            let mode = integer(&connector.fields["m_mode"])?;
            let mut modifiers = Vec::new();
            for pointer in connector.fields["m_modifiers"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("connector modifiers absent"))?
            {
                if let Some(modifier_index) = target(graph, index, pointer)? {
                    let modifier = &graph.objects[modifier_index];
                    if let Some(backlink) = modifier.fields.get("m_pConnector") {
                        // Repeated pointer tokens may not emit an edge; when emitted it must agree.
                        if let Some(offset) = backlink["offset"].as_u64() {
                            for edge in graph.edges.iter().filter(|e| {
                                e.source_object_index == modifier_index
                                    && e.pointer_offset == offset as usize
                            }) {
                                ensure!(
                                    edge.target_object_index == index,
                                    "connector modifier backlink mismatch"
                                );
                            }
                        }
                    }
                    modifiers.push(Modifier {
                        class_name: modifier.class_name.clone(),
                        fields: modifier.fields.clone(),
                        source: source(
                            record,
                            modifier_index,
                            "m_connPtrArray.Connector.m_modifiers",
                        ),
                    });
                }
            }
            for (ordinal, reference) in connector.fields["m_arrRefs"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("connector references absent"))?
                .iter()
                .enumerate()
            {
                let target_key = PortKey {
                    owner_element_id: identifier(&reference["m_id"])?,
                    connector_id: integer(&reference["m_nIndex"])?,
                };
                let target_mode = integer(&reference["m_connType"])?;
                let kind = if mode == 4 || target_mode == 4 {
                    "logical_system"
                } else if mode == 1 && target_mode == 1 {
                    if key.owner_element_id == target_key.owner_element_id {
                        "same_owner_internal"
                    } else {
                        "cross_owner_physical"
                    }
                } else {
                    "unclassified_saved_reference"
                };
                edges.push(Edge {
                    source_port: key.clone(),
                    target_port: target_key,
                    raw_target_mode: target_mode,
                    kind,
                    target_owner: None,
                    resolution: "unresolved",
                    reciprocal_reference: false,
                    source: source(record, index, &format!("m_arrRefs[{ordinal}]")),
                });
            }
            let observations = observations(&modifiers)?;
            ports.push(Port {
                geometry: ConnectorGeometry::unresolved(),
                observations,
                raw_unplaced_owner_id: root
                    .fields
                    .get("m_unplacedOwnerId")
                    .map(identifier)
                    .transpose()?,
                key,
                owner: source(record, index, "").owner,
                owner_class: root.class_name.clone(),
                mode,
                modifiers,
                source: source(record, index, "m_connPtrArray.Connector"),
            });
        }
    }
    Ok((ports, edges, managers))
}

fn observations(modifiers: &[Modifier]) -> Result<BTreeMap<String, Observation>> {
    let mut result = BTreeMap::new();
    for modifier in modifiers {
        if let Some(observation) = saved_connector_domain_observation(modifier) {
            ensure!(
                result.insert("domain".into(), observation).is_none(),
                "ambiguous saved connector-domain modifier"
            );
        }
        if !matches!(
            modifier.class_name.as_str(),
            "MEPFamilyConnectorCalculationPiping" | "SegmentConnectorCalculation"
        ) {
            continue;
        }
        for (key, field) in [
            ("direction", "m_eDirection"),
            ("system_id", "m_idSystem"),
            ("cached_flow", "m_dFlow"),
            ("cached_actual_flow", "m_dActualFlow"),
        ] {
            let family_piping = modifier.class_name == "MEPFamilyConnectorCalculationPiping";
            let output_key = match (family_piping, key) {
                (true, "cached_flow") => "assigned_flow",
                (true, "cached_actual_flow") => "flow",
                _ => key,
            };
            let Some(raw) = modifier.fields.get(field) else {
                continue;
            };
            let (value, spec, basis, interpretation) = match key {
                "direction" => {
                    let label = match integer(raw)? {
                        0 => "Bidirectional",
                        1 => "In",
                        2 => "Out",
                        _ => continue,
                    };
                    (
                        Value::String(label.into()),
                        None,
                        "enumeration",
                        "class_qualified_saved_direction",
                    )
                }
                "system_id" => (
                    Value::from(identifier(raw)?),
                    None,
                    "element_reference",
                    "saved_calculation_system_reference",
                ),
                _ => {
                    ensure!(
                        raw.as_f64().is_some_and(f64::is_finite),
                        "invalid cached connector flow"
                    );
                    (
                        raw.clone(),
                        Some("autodesk.spec.aec.piping:flow-2.0.0"),
                        "revit_internal_cubic_feet_per_second",
                        if family_piping {
                            "class_qualified_saved_connector_flow"
                        } else {
                            "serialized_cache_not_verified_Connector_Flow_API"
                        },
                    )
                }
            };
            let mut provenance = modifier.source.clone();
            provenance.source_field = format!("{}.{}", modifier.class_name, field);
            ensure!(
                result
                    .insert(
                        output_key.into(),
                        Observation {
                            value,
                            raw_value: raw.clone(),
                            spec_type_id: spec,
                            value_unit_basis: basis,
                            interpretation,
                            source: provenance
                        }
                    )
                    .is_none(),
                "ambiguous calculation modifier observation"
            );
        }
    }
    Ok(result)
}

/// The modifier class is a serialized domain discriminator only for the two
/// independently witnessed family-connector data classes below.  In
/// particular, generic segment data and logical/system connectors do not
/// establish an API Connector.Domain value.
fn saved_connector_domain_observation(modifier: &Modifier) -> Option<Observation> {
    let value = match modifier.class_name.as_str() {
        "MEPFamilyConnectorDataPiping" => "DomainPiping",
        "MEPFamilyConnectorDataElectrical" => "DomainElectrical",
        _ => return None,
    };
    Some(Observation {
        value: Value::String(value.into()),
        raw_value: Value::String(modifier.class_name.clone()),
        spec_type_id: None,
        value_unit_basis: "enumeration",
        interpretation: "class_qualified_saved_connector_domain",
        source: modifier.source.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn provenance(id: u64) -> Source {
        Source {
            owner: Identity {
                element_id: id,
                unique_id: format!("owner-{id}"),
            },
            source_object: 0,
            source_field: "m_arrRefs[0]".into(),
            body_sha256: "synthetic".into(),
            stream: "Partitions/0".into(),
            group_record_offset: 0,
        }
    }
    fn port(owner: i64, index: i64, mode: i64) -> Port {
        Port {
            geometry: ConnectorGeometry::unresolved(),
            observations: BTreeMap::new(),
            raw_unplaced_owner_id: None,
            key: PortKey {
                owner_element_id: owner,
                connector_id: index,
            },
            owner: provenance(owner as u64).owner,
            owner_class: "synthetic".into(),
            mode,
            modifiers: vec![],
            source: provenance(owner as u64),
        }
    }
    fn edge(a: &Port, b: &Port) -> Edge {
        Edge {
            source_port: a.key.clone(),
            target_port: b.key.clone(),
            raw_target_mode: b.mode,
            kind: "logical_system",
            target_owner: None,
            resolution: "unresolved",
            reciprocal_reference: false,
            source: a.source.clone(),
        }
    }
    #[test]
    fn references_resolve_by_stable_id_not_array_ordinal_and_do_not_create_missing_reciprocals() {
        let a = port(10, 7, 1);
        let b = port(20, 3, 4);
        let absent = port(30, 0, 1);
        let mut builder = InventoryBuilder {
            edges: vec![edge(&a, &b), edge(&a, &absent)],
            ..InventoryBuilder::default()
        };
        for p in [a, b] {
            builder
                .identities
                .insert(p.key.owner_element_id, p.owner.clone());
            builder.ports.insert(p.key.clone(), p);
        }
        let inventory = builder.finish().unwrap();
        assert_eq!(inventory.edges[0].resolution, "resolved_native_port");
        assert!(!inventory.edges[0].reciprocal_reference);
        assert_eq!(inventory.edges[1].resolution, "unresolved_target_owner");
        assert_eq!(inventory.edges.len(), 2);
        assert!(!inventory.complete_supported_network);
    }
    #[test]
    fn is_connected_requires_a_resolved_reciprocal_cross_owner_physical_reference() {
        let a = port(10, 7, 1);
        let b = port(20, 3, 1);
        let mut forward = edge(&a, &b);
        forward.kind = "cross_owner_physical";
        forward.resolution = "resolved_native_port";
        let mut reverse = edge(&b, &a);
        reverse.kind = "cross_owner_physical";
        reverse.resolution = "resolved_native_port";
        let observation = saved_physical_connection_observation(&a, &[forward, reverse])
            .expect("qualified saved physical connection");
        assert_eq!(observation.value, json!(true));
        assert_eq!(
            observation.interpretation,
            "derived_from_reciprocal_saved_cross_owner_physical_reference"
        );
    }
    #[test]
    fn is_connected_is_false_only_after_the_complete_reference_set_resolves() {
        let a = port(10, 7, 1);
        let b = port(20, 3, 1);
        let mut forward = edge(&a, &b);
        forward.kind = "cross_owner_physical";
        forward.resolution = "resolved_native_port";
        let observation = saved_physical_connection_observation(&a, &[forward])
            .expect("resolved non-reciprocal reference set");
        assert_eq!(observation.value, json!(false));

        let missing = port(30, 5, 1);
        let mut unresolved = edge(&a, &missing);
        unresolved.kind = "cross_owner_physical";
        assert!(saved_physical_connection_observation(&a, &[unresolved]).is_none());
    }
    #[test]
    fn is_connected_does_not_promote_logical_or_non_end_references() {
        let a = port(10, 7, 1);
        let b = port(20, 3, 4);
        let mut logical = edge(&a, &b);
        logical.resolution = "resolved_native_port";
        assert_eq!(
            saved_physical_connection_observation(&a, &[logical])
                .expect("resolved complete logical set")
                .value,
            json!(false)
        );
        assert!(saved_physical_connection_observation(&b, &[]).is_none());
    }
    #[test]
    fn unrelated_pointer_offset_does_not_resolve_manager_ownership() {
        let graph:ObjectGraph=serde_json::from_value(json!({"consumed_bytes":20,"objects":[{"class_tag":1,"class_name":"Connector","token":0,"start":2,"fields_end":20,"fields":{}}],"edges":[{"source_object_index":1,"pointer_offset":4,"pointer_token":99,"target_object_index":0,"target_class_tag":1}]})).unwrap();
        assert!(target(&graph, 0, &json!({"offset":4,"pointer_token":99})).is_err());
        assert_eq!(
            target(&graph, 0, &json!({"pointer_token":0})).unwrap(),
            None
        );
    }
    #[test]
    fn calculation_semantics_are_class_qualified_and_flow_fields_stay_distinct() {
        let mut modifier = Modifier {
            class_name: "MEPFamilyConnectorCalculationPiping".into(),
            fields: json!({"m_dFlow":0.25,"m_dActualFlow":0.125,"m_eDirection":2,"m_idSystem":{"m_id":{"m_id64":55}}}),
            source: provenance(10),
        };
        let family = observations(&[modifier.clone()]).unwrap();
        assert_eq!(family["flow"].value, json!(0.125));
        assert_eq!(family["assigned_flow"].value, json!(0.25));
        assert_eq!(family["direction"].value, json!("Out"));
        assert_eq!(family["system_id"].value, json!(55));
        modifier.class_name = "SegmentConnectorCalculation".into();
        let segment = observations(&[modifier.clone()]).unwrap();
        assert!(!segment.contains_key("flow"));
        assert_eq!(segment["cached_actual_flow"].value, json!(0.125));
        modifier.class_name = "MEPFamilyConnectorCalculationElectrical".into();
        assert!(observations(&[modifier]).unwrap().is_empty());
    }

    #[test]
    fn connector_domain_is_admitted_only_for_witnessed_family_data_modifiers() {
        let mut piping = Modifier {
            class_name: "MEPFamilyConnectorDataPiping".into(),
            fields: json!({}),
            source: provenance(10),
        };
        let domain_observations = observations(&[piping.clone()]).unwrap();
        assert_eq!(domain_observations["domain"].value, json!("DomainPiping"));
        assert_eq!(
            domain_observations["domain"].interpretation,
            "class_qualified_saved_connector_domain"
        );
        piping.class_name = "MEPFamilyConnectorDataElectrical".into();
        assert_eq!(
            observations(&[piping.clone()]).unwrap()["domain"].value,
            json!("DomainElectrical")
        );
        piping.class_name = "SegmentConnectorDataModifier".into();
        assert!(observations(&[piping]).unwrap().get("domain").is_none());
    }

    #[test]
    fn observed_mep_curve_owner_classes_are_admitted_explicitly() {
        for class in [
            "RbsPipeCurve",
            "RbsDuctCurve",
            "RbsConduitCurve",
            "RbsFlexDuctCurve",
            "RbsWireCurve",
        ] {
            assert!(supports_connector_owner_class(class), "{class}");
        }
        assert!(!supports_connector_owner_class("RbsUnknownCurve"));
    }
    #[test]
    fn electrical_base_array_resolves_through_system_port_to_panel_owner() {
        let system_port = port(20, 2, 4);
        let mut panel_port = port(30, 50000, 4);
        panel_port.owner_class = "FamilyInstance".into();
        let load_port = port(40, 8, 1);
        let mut builder = InventoryBuilder::default();
        builder.systems.push(System{owner:provenance(20).owner,owner_class:"RbsElectricalSystem".into(),stored_name:None,name_status:"unavailable",name_source:None,observations:BTreeMap::new(),logical_members:vec![],logical_member_status:"not_applicable",members:vec![],member_status:"not_applicable",base_connector_references:json!([{"m_id":{"m_id":{"m_id64":20}},"m_nIndex":2,"m_connType":4}]),base_equipment:None,base_equipment_status:"unresolved",base_equipment_sources:vec![provenance(20)]});
        builder.edges.push(edge(&system_port, &panel_port));
        builder.edges.push(edge(&system_port, &load_port));
        for p in [system_port, panel_port, load_port] {
            builder
                .identities
                .insert(p.key.owner_element_id, p.owner.clone());
            builder.ports.insert(p.key.clone(), p);
        }
        let inventory = builder.finish().unwrap();
        assert_eq!(
            inventory.systems[0]
                .base_equipment
                .as_ref()
                .unwrap()
                .element_id,
            30
        );
        assert_ne!(
            inventory.systems[0]
                .base_equipment
                .as_ref()
                .unwrap()
                .element_id,
            20
        );
        assert_eq!(inventory.systems[0].base_equipment_sources.len(), 2);
        assert_eq!(
            inventory.systems[0]
                .logical_members
                .iter()
                .map(|member| member.owner.element_id)
                .collect::<Vec<_>>(),
            vec![30, 40]
        );
        assert_eq!(
            inventory.systems[0]
                .members
                .iter()
                .map(|member| member.owner.element_id)
                .collect::<Vec<_>>(),
            vec![40]
        );
        assert_eq!(
            inventory.systems[0].member_status,
            "resolved_from_saved_system_logical_edges_excluding_base_equipment"
        );
    }
}
