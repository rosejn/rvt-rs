//! File-local Extensible Storage catalog from the owned Global/Latest graph.
use crate::{
    native_extensible_storage::{Catalog, Field, Schema, guid_string},
    native_parameters::{self, GraphLimits, ObjectGraph},
    schema_registry::Registry,
};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Global object streams carry a fixed u32 footer after the deferred graph.
/// Only the independently observed zero-footer envelope is qualified here.
pub fn decode(bytes: &[u8], registry: &Registry) -> Result<(Catalog, Value)> {
    ensure!(
        bytes.len() >= 6 && bytes.len() <= 32 * 1024 * 1024,
        "ES global stream size unqualified"
    );
    let end = bytes.len() - 4;
    ensure!(
        bytes[end..] == [0; 4],
        "ES global fixed footer is not the qualified zero value"
    );
    let graph = native_parameters::decode_graph_with_limits(
        &bytes[..end],
        registry,
        &GraphLimits {
            max_values: 2_000_000,
            ..Default::default()
        },
    )?;
    let root = graph
        .objects
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty global graph"))?;
    ensure!(
        root.class_name == "ADocument",
        "ES catalog requires ADocument root"
    );
    let manager = owned(&graph, 0, &root.fields["m_pAppInfoManager"])?;
    ensure!(
        graph.objects[manager].class_name == "AppInfoManager",
        "ES manager class mismatch"
    );
    let pointers = graph.objects[manager].fields["m_appInfoArr"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("ES app info array absent"))?;
    let mut storages = Vec::new();
    for pointer in pointers {
        if pointer["pointer_token"].as_u64() == Some(0) {
            continue;
        }
        let index = owned(&graph, manager, pointer)?;
        if graph.objects[index].class_name == "ESSchemaStorage" {
            storages.push(index);
        }
    }
    ensure!(
        storages.len() == 1,
        "ES schema storage missing or ambiguous"
    );
    let storage = &graph.objects[storages[0]];
    let old = storage.fields.get("m_storedSchemas");
    let current = storage.fields.get("m_schemaUsageMap");
    ensure!(
        old.is_some() ^ current.is_some(),
        "ES schema storage layout missing or ambiguous"
    );
    let usage_map = current.is_some();
    let rows = old
        .or(current)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("ES schema entries absent"))?;
    let mut catalog = Catalog::default();
    for row in rows {
        let s = if usage_map {
            &row["second"]["m_schema"]
        } else {
            &row["second"]
        };
        let guid = guid(&row["first"])?;
        ensure!(
            guid == self::guid(&s["m_guid"])?,
            "ES map key and schema GUID disagree"
        );
        let mut fields = Vec::new();
        for f in s["m_fields"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("ES schema fields absent"))?
        {
            let sub = self::guid(&f["m_subSchemaGUID"])?;
            // Older saved schemas (including the 2020 Mannheim fixture) omit
            // the Forge spec wrapper entirely. Its absence is an explicit
            // "no saved spec" state, while a present wrapper remains strict.
            let spec = optional_string(&f["m_specTypeId"], "m_typeId")?;
            fields.push(Field {
                index: u32::try_from(
                    f["m_entryIndex"]
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("invalid ES field index"))?,
                )?,
                name: string(f, "m_fieldName")?,
                type_name: string(f, "m_fieldTypeName")?,
                container_type: i32::try_from(
                    f["m_containerType"]
                        .as_i64()
                        .ok_or_else(|| anyhow::anyhow!("invalid ES container type"))?,
                )?,
                subschema_guid: (sub != "00000000-0000-0000-0000-000000000000").then_some(sub),
                spec_type_id: (!spec.is_empty()).then_some(spec),
                raw_metadata: f.clone(),
            });
        }
        catalog.insert(Schema {
            guid,
            name: string(s, "m_schemaName")?,
            fields,
            raw_metadata: {
                let mut metadata = s.clone();
                if usage_map {
                    metadata["_storage_usage"] = row["second"].clone();
                }
                metadata
            },
        })?;
    }
    Ok((
        catalog,
        json!({"stream":"Global/Latest","inflated_sha256":format!("{:x}",Sha256::digest(bytes)),"graph_bytes":end,"stream_bytes":bytes.len(),"fixed_footer":0,"root_class":"ADocument","app_info_object_index":manager,"storage_object_index":storages[0],"storage_start":storage.start,"storage_end":storage.fields_end,"complete_global_graph":true,"scope":"Owned file-local schemas; general nonzero fixed footers remain unqualified"}),
    ))
}

/// Read a deliberately narrow, source-bound declaration witness.  This is not
/// a general Revit API import: it admits only declaration metadata required by
/// the native ES decoder and rejects all richer/unqualified API shapes.
///
/// The caller must bind `source_sha256` to the bytes it is decoding.  Keeping
/// this format separate from the API harness output makes the reviewable
/// boundary explicit and prevents a future harness expansion from silently
/// becoming parser input.
pub fn decode_source_bound_witness(bytes: &[u8], source_sha256: &str) -> Result<(Catalog, Value)> {
    #[derive(Deserialize)]
    struct Witness {
        format: String,
        source_sha256: String,
        schemas: Vec<WitnessSchema>,
    }
    #[derive(Deserialize)]
    struct WitnessSchema {
        guid: String,
        name: String,
        #[serde(default)]
        payload_prefix_bytes: u32,
        fields: Vec<WitnessField>,
    }
    #[derive(Deserialize)]
    struct WitnessField {
        api_order: u32,
        #[serde(default)]
        storage_index: Option<u32>,
        name: String,
        value_type: String,
        container: String,
        subschema_guid: String,
        spec: String,
    }
    let witness: Witness = serde_json::from_slice(bytes)?;
    ensure!(
        witness.format == "rvt-source-bound-es-witness-v1",
        "unrecognized source-bound ES witness format"
    );
    ensure!(
        witness.source_sha256 == source_sha256,
        "source-bound ES witness SHA-256 does not match source document"
    );
    ensure!(!witness.schemas.is_empty(), "source-bound ES witness has no schemas");
    let mut catalog = Catalog::default();
    for schema in witness.schemas {
        ensure!(is_guid(&schema.guid), "source-bound ES schema GUID is invalid");
        ensure!(!schema.name.is_empty(), "source-bound ES schema name is empty");
        ensure!(
            schema.payload_prefix_bytes <= 64,
            "source-bound ES payload prefix exceeds bounded qualification"
        );
        let has_storage_index = schema.fields.iter().any(|field| field.storage_index.is_some());
        ensure!(
            !has_storage_index || schema.fields.iter().all(|field| field.storage_index.is_some()),
            "source-bound ES storage order must be specified for every schema field"
        );
        ensure!(
            schema.payload_prefix_bytes == 0 || has_storage_index,
            "source-bound ES payload prefix requires a complete persisted field order"
        );
        let mut fields = Vec::new();
        for field in schema.fields {
            ensure!(
                field.value_type == "System.Int32",
                "source-bound ES witness value type is not qualified: {}",
                field.value_type
            );
            let container_type = match field.container.as_str() {
                "Simple" => 0,
                "Array" => 1,
                _ => anyhow::bail!("source-bound ES witness container is not qualified: {}", field.container),
            };
            ensure!(
                field.subschema_guid == "00000000-0000-0000-0000-000000000000" && field.spec.is_empty(),
                "source-bound ES witness field has unsupported subschema or spec"
            );
            fields.push(Field {
                // `Schema.ListFields()` exposes a presentation/API order; it
                // is not the persisted `m_entryIndex` order.  A source-bound
                // witness may supply the latter only when physical framing
                // has independently measured it.  Older declaration-only
                // witnesses remain zero-prefix/api-order declarations.
                index: field.storage_index.unwrap_or(field.api_order),
                name: field.name,
                type_name: "int".into(),
                container_type,
                subschema_guid: None,
                spec_type_id: None,
                raw_metadata: json!({"provenance":"source_bound_api_witness","api_value_type":"System.Int32","api_container":field.container}),
            });
        }
        catalog.insert(Schema {
            guid: schema.guid,
            name: schema.name,
            fields,
            raw_metadata: json!({
                "provenance":"source_bound_api_witness",
                "source_sha256":source_sha256,
                "source_bound_payload_prefix_bytes":schema.payload_prefix_bytes,
            }),
        })?;
    }
    let witness_sha256 = format!("{:x}", Sha256::digest(bytes));
    Ok((catalog, json!({"format":"rvt-source-bound-es-witness-v1","source_sha256":source_sha256,"witness_sha256":witness_sha256,"scope":"only System.Int32 Simple/Array declarations, with optional complete source-bound persisted order"})))
}

fn is_guid(value: &str) -> bool {
    value.len() == 36
        && [8, 13, 18, 23].into_iter().all(|index| value.as_bytes()[index] == b'-')
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit())
}
fn string(v: &Value, key: &str) -> Result<String> {
    v[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("ES string {key} absent"))
}
fn optional_string(v: &Value, key: &str) -> Result<String> {
    if v.is_null() {
        return Ok(String::new());
    }
    string(v, key)
}
fn guid(v: &Value) -> Result<String> {
    let v = if v.get("guid_bytes").is_some() {
        v
    } else {
        &v["m_guid"]
    };
    let a = v["guid_bytes"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("ES GUID bytes absent"))?;
    ensure!(a.len() == 16, "ES GUID width");
    let mut b = [0; 16];
    for (dst, src) in b.iter_mut().zip(a) {
        *dst = u8::try_from(
            src.as_u64()
                .ok_or_else(|| anyhow::anyhow!("ES GUID byte invalid"))?,
        )?;
    }
    Ok(guid_string(b))
}
fn owned(g: &ObjectGraph, owner: usize, pointer: &Value) -> Result<usize> {
    let offset = pointer["offset"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("ES ownership pointer absent"))?;
    let edges: Vec<_> = g
        .edges
        .iter()
        .filter(|e| e.source_object_index == owner && e.pointer_offset as u64 == offset)
        .collect();
    ensure!(edges.len() == 1, "ES owned pointer ambiguous or unresolved");
    let e = edges[0];
    ensure!(
        e.target_object_index < g.objects.len(),
        "ES target outside graph"
    );
    Ok(e.target_object_index)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guid_width_and_bytes_are_strict() {
        assert_eq!(guid(&json!({"m_guid":{"guid_bytes":[43,42,2,27,34,87,135,71,146,138,79,41,202,157,136,17]}})).unwrap(),"1b022a2b-5722-4787-928a-4f29ca9d8811");
        assert!(guid(&json!({"guid_bytes":vec![256;16]})).is_err());
    }

    #[test]
    fn missing_legacy_spec_wrapper_means_no_saved_spec_but_present_value_is_strict() {
        assert_eq!(optional_string(&Value::Null, "m_typeId").unwrap(), "");
        assert!(optional_string(&json!({}), "m_typeId").is_err());
        assert_eq!(
            optional_string(&json!({"m_typeId":"autodesk.spec.aec:length-2.0.1"}), "m_typeId").unwrap(),
            "autodesk.spec.aec:length-2.0.1"
        );
    }

    #[test]
    fn source_bound_witness_is_exact_and_narrow() {
        let bytes = serde_json::to_vec(&json!({
            "format":"rvt-source-bound-es-witness-v1", "source_sha256":"abc",
            "schemas":[{"guid":"57c66e83-4651-496b-aebb-69d085752c1b","name":"ExportViewSheetSetListSchema","payload_prefix_bytes":8,"fields":[
                {"api_order":0,"storage_index":1,"name":"major","value_type":"System.Int32","container":"Simple","subschema_guid":"00000000-0000-0000-0000-000000000000","spec":""},
                {"api_order":1,"storage_index":0,"name":"ids","value_type":"System.Int32","container":"Array","subschema_guid":"00000000-0000-0000-0000-000000000000","spec":""}
            ]}]
        })).unwrap();
        let (catalog, receipt) = decode_source_bound_witness(&bytes, "abc").unwrap();
        assert_eq!(catalog.schemas["57c66e83-4651-496b-aebb-69d085752c1b"].fields[0].index, 1);
        assert_eq!(catalog.schemas["57c66e83-4651-496b-aebb-69d085752c1b"].raw_metadata["source_bound_payload_prefix_bytes"], 8);
        assert_eq!(catalog.schemas["57c66e83-4651-496b-aebb-69d085752c1b"].fields[1].container_type, 1);
        assert_eq!(receipt["source_sha256"], "abc");
        assert!(decode_source_bound_witness(&bytes, "wrong").is_err());
    }

    #[test]
    fn source_bound_witness_refuses_partial_persisted_order() {
        let bytes = serde_json::to_vec(&json!({
            "format":"rvt-source-bound-es-witness-v1", "source_sha256":"abc",
            "schemas":[{"guid":"57c66e83-4651-496b-aebb-69d085752c1b","name":"Example","fields":[
                {"api_order":0,"storage_index":1,"name":"first","value_type":"System.Int32","container":"Simple","subschema_guid":"00000000-0000-0000-0000-000000000000","spec":""},
                {"api_order":1,"name":"second","value_type":"System.Int32","container":"Simple","subschema_guid":"00000000-0000-0000-0000-000000000000","spec":""}
            ]}]
        }))
        .unwrap();
        assert!(decode_source_bound_witness(&bytes, "abc").is_err());
    }
}
