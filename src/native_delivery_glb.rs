//! Deterministic binary glTF 2.0 adapter for the unified native package.
use crate::{
    native_delivery::{DeliveryPackage, primitive_asset_key},
    native_delivery_spatial::rebase_positions,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct GlbArtifact {
    pub bytes: Vec<u8>,
    pub attributes: Value,
}

fn align_binary(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}
fn align_json(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        // GLB 2.0 requires JSON chunks to use ASCII-space padding. Binary
        // chunks remain zero-padded by `align_binary` above.
        v.push(b' ');
    }
}
fn f32le(v: &mut Vec<u8>, x: f32) {
    v.extend(x.to_le_bytes());
}
fn u32le(v: &mut Vec<u8>, x: u32) {
    v.extend(x.to_le_bytes());
}
fn yup(p: [f64; 3]) -> [f64; 3] {
    [p[0], p[2], -p[1]]
}
fn checked_normal(n: [f64; 3]) -> Result<[f64; 3]> {
    ensure!(n.iter().all(|x| x.is_finite()), "nonfinite mesh normal");
    let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    ensure!(
        length.is_finite() && length > 1e-12 && (length - 1.).abs() < 1e-4,
        "mesh normal is not unit length"
    );
    Ok(yup(n))
}

/// Emit a GLB. Source package coordinates are meters/Z-up; this applies the
/// fixed right-handed Z-up to glTF Y-up conversion exactly once.
pub fn build_glb(package: &DeliveryPackage) -> Result<GlbArtifact> {
    build_glb_internal(package)
}
fn build_glb_internal(package: &DeliveryPackage) -> Result<GlbArtifact> {
    let texture_mappings = package.texture_mappings.as_ref().and_then(|value| {
        serde_json::from_value::<crate::native_texture_mapping::Inventory>(value.clone()).ok()
    });
    let mut bin = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    let mut attrs = Vec::new();
    // Content-address the complete primitive payload. Multiple Revit owners
    // can reference the same saved symbol geometry; glTF meshes may carry
    // owner-specific material/provenance extras while sharing these accessors.
    let mut primitive_accessors = BTreeMap::<String, (usize, usize, usize)>::new();
    for (ai, owner) in package.geometry.iter().enumerate() {
        ensure!(
            owner.local_origin_meters.iter().all(|x| x.is_finite()),
            "nonfinite instance origin"
        );
        let mut prims = Vec::new();
        let mut mesh_rows = Vec::new();
        for (primitive_index, primitive) in owner.meshes.primitives.iter().enumerate() {
            ensure!(
                !primitive.vertices.is_empty() && !primitive.triangles.is_empty(),
                "empty mesh primitive"
            );
            ensure!(
                primitive
                    .vertices
                    .iter()
                    .all(|p| p.iter().all(|x| x.is_finite())),
                "nonfinite mesh position"
            );
            ensure!(
                primitive.normals.len() == primitive.vertices.len(),
                "mesh normal cardinality mismatch"
            );
            ensure!(
                primitive
                    .triangles
                    .iter()
                    .flatten()
                    .all(|i| (*i as usize) < primitive.vertices.len()),
                "mesh triangle index out of bounds"
            );
            let asset_key = primitive_asset_key(primitive);
            let (va, na, ia) = if let Some(accessors) = primitive_accessors.get(&asset_key) {
                *accessors
            } else {
                let points: Vec<_> = primitive.vertices.iter().copied().map(yup).collect();
                let rebased = rebase_positions(&points, [0., 0., 0.])?;
                let vo = bin.len();
                for p in &rebased.positions {
                    f32le(&mut bin, p[0]);
                    f32le(&mut bin, p[1]);
                    f32le(&mut bin, p[2]);
                }
                align_binary(&mut bin);
                let vi = views.len();
                views.push(
                    json!({"buffer":0,"byteOffset":vo,"byteLength":bin.len()-vo,"target":34962}),
                );
                let mut min = [f32::INFINITY; 3];
                let mut max = [f32::NEG_INFINITY; 3];
                for p in &rebased.positions {
                    for j in 0..3 {
                        min[j] = min[j].min(p[j]);
                        max[j] = max[j].max(p[j]);
                    }
                }
                let va = accessors.len();
                accessors.push(json!({"bufferView":vi,"componentType":5126,"count":rebased.positions.len(),"type":"VEC3","min":min,"max":max}));
                let no = bin.len();
                for n in &primitive.normals {
                    let q = checked_normal(*n)?;
                    f32le(&mut bin, q[0] as f32);
                    f32le(&mut bin, q[1] as f32);
                    f32le(&mut bin, q[2] as f32);
                }
                align_binary(&mut bin);
                let ni = views.len();
                views.push(
                    json!({"buffer":0,"byteOffset":no,"byteLength":bin.len()-no,"target":34962}),
                );
                let na = accessors.len();
                accessors.push(json!({"bufferView":ni,"componentType":5126,"count":primitive.vertices.len(),"type":"VEC3"}));
                let io = bin.len();
                for tri in &primitive.triangles {
                    for x in tri {
                        u32le(&mut bin, *x);
                    }
                }
                align_binary(&mut bin);
                let ii = views.len();
                views.push(
                    json!({"buffer":0,"byteOffset":io,"byteLength":bin.len()-io,"target":34963}),
                );
                let ia = accessors.len();
                accessors.push(json!({"bufferView":ii,"componentType":5125,"count":primitive.triangles.len()*3,"type":"SCALAR"}));
                primitive_accessors.insert(asset_key, (va, na, ia));
                (va, na, ia)
            };
            let mut attributes = json!({"POSITION":va,"NORMAL":na});
            let mut texture_mapping_status = None;
            let mut texture_mapping_reason: Option<String> = None;
            if let Some(mapping) = texture_mappings.as_ref().and_then(|inventory| {
                let unique_id = owner.element_key.rsplit(':').next()?;
                inventory.mappings.iter().find(|mapping| {
                    mapping.unique_id == unique_id && mapping.geometry_tag == primitive.face_tag
                })
            }) {
                let origin_feet = owner.local_origin_meters.map(|value| value / 0.3048);
                let uvs = primitive
                    .vertices
                    .iter()
                    .map(|point| {
                        mapping.evaluate(std::array::from_fn(|axis| {
                            point[axis] / 0.3048 + origin_feet[axis]
                        }))
                    })
                    .collect::<Result<Vec<_>>>();
                if let Ok(uvs) = uvs {
                    if uvs
                        .iter()
                        .all(|uv| uv.iter().all(|value| value.is_finite()))
                    {
                        let uo = bin.len();
                        for uv in &uvs {
                            f32le(&mut bin, uv[0] as f32);
                            f32le(&mut bin, uv[1] as f32);
                        }
                        align_binary(&mut bin);
                        let ui = views.len();
                        views.push(json!({"buffer":0,"byteOffset":uo,"byteLength":bin.len()-uo,"target":34962}));
                        let ua = accessors.len();
                        accessors.push(json!({"bufferView":ui,"componentType":5126,"count":uvs.len(),"type":"VEC2"}));
                        attributes["TEXCOORD_0"] = json!(ua);
                        texture_mapping_status = Some("emitted");
                    } else {
                        texture_mapping_status = Some("not_emitted");
                        texture_mapping_reason = Some("nonfinite evaluated UV".into());
                    }
                } else if let Err(error) = uvs {
                    texture_mapping_status = Some("not_emitted");
                    texture_mapping_reason = Some(error.to_string());
                }
            } else if texture_mappings.is_some() {
                texture_mapping_status = Some("not_available");
                texture_mapping_reason = Some("no mapping matched element and face tag".into());
            }
            let mut gltf_primitive = json!({"attributes":attributes,"indices":ia,"mode":4});
            if let Some(material) = owner.render_materials.get(&primitive_index) {
                gltf_primitive["extras"] = json!({"saved_material":material});
            }
            let mesh_key = format!("{}:mesh:{}", owner.element_key, primitive_index);
            let mut native_delivery = json!({"mesh_key":mesh_key,"element_key":owner.element_key,"primitive_index":primitive_index,"source_owner_id":primitive.source_owner_id,"graphics_object_index":primitive.object_index,"face_tag":primitive.face_tag,"render_style_id":primitive.render_style_id,"explicit_material_id":primitive.material_id});
            if let Some(status) = texture_mapping_status {
                native_delivery["texture_mapping"] = json!({
                    "status": status,
                    "reason": texture_mapping_reason,
                });
            }
            gltf_primitive["extras"]["native_delivery"] = native_delivery;
            mesh_rows.push(json!({"mesh_key":mesh_key,"primitive_index":primitive_index,"triangle_count":primitive.triangles.len()}));
            prims.push(gltf_primitive);
        }
        let mi = meshes.len();
        meshes.push(json!({"primitives":prims,"name":owner.element_key}));
        let origin = yup(owner.local_origin_meters);
        nodes.push(json!({"mesh":mi,"name":owner.element_key,"translation":origin,"extras":{"element_key":owner.element_key,"attribute_row":ai,"source_frame":owner.source_frame}}));
        attrs.push(json!({"element_key":owner.element_key,"attribute_row":ai,"instance_key":format!("{}:instance",owner.element_key),"source_frame":owner.source_frame,"mesh_rows":mesh_rows}));
    }
    align_binary(&mut bin);
    let scene_nodes: Vec<usize> = (0..nodes.len()).collect();
    let mut json_doc = json!({"asset":{"version":"2.0","generator":"rvt-rs native delivery"},"scene":0,"scenes":[{"nodes":scene_nodes}],"nodes":nodes,"meshes":meshes,"accessors":accessors,"bufferViews":views,"buffers":[{"byteLength":bin.len()}],"extras":{"attribute_table":attrs,"coordinate_conversion":"meters Z-up to meters Y-up exactly once","glb_byte_length":0}});
    let js = loop {
        let mut candidate = serde_json::to_vec(&json_doc)?;
        align_json(&mut candidate);
        let total = 12 + 8 + candidate.len() + 8 + bin.len();
        if json_doc["extras"]["glb_byte_length"] == json!(total) {
            break candidate;
        }
        json_doc["extras"]["glb_byte_length"] = json!(total);
    };
    let total = 12 + 8 + js.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    u32le(&mut out, 0x46546c67);
    u32le(&mut out, 2);
    u32le(&mut out, total as u32);
    u32le(&mut out, js.len() as u32);
    u32le(&mut out, 0x4e4f534a);
    out.extend(js);
    u32le(&mut out, bin.len() as u32);
    u32le(&mut out, 0x004e4942);
    out.extend(bin);
    Ok(GlbArtifact {
        bytes: out,
        attributes: json_doc["extras"]["attribute_table"].clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_delivery::{
        Coverage, DeliveryManifest, GeometryInstance, InstanceRow, MaterialRow, MeshRow,
        RelationshipRow, TypeRow,
    };
    use crate::native_saved_mesh::{GraphicsMeshes, Primitive};
    fn package() -> DeliveryPackage {
        let p = Primitive {
            source_owner_id: None,
            object_index: 0,
            face_tag: 0,
            render_style_id: 0,
            material_id: Some(7),
            vertices: vec![[1., 2., 3.], [2., 2., 3.], [1., 3., 3.]],
            normals: vec![[0., 0., 1.]; 3],
            triangles: vec![[0, 1, 2]],
        };
        DeliveryPackage {
            manifest: DeliveryManifest {
                format: "x",
                profile: "tiles",
                document_namespace: "d".into(),
                source_sha256: None,
                status: "x".into(),
                units: "meters",
                coordinate_frame: "z",
                parameter_definitions_uri: None,
                parameter_bindings_uri: None,
                coverage: Coverage::default(),
                tables: vec![],
            },
            elements: vec![],
            parameter_definitions: vec![],
            parameter_bindings: vec![],
            types: Vec::<TypeRow>::new(),
            relationships: Vec::<RelationshipRow>::new(),
            meshes: Vec::<MeshRow>::new(),
            instances: Vec::<InstanceRow>::new(),
            materials: Vec::<MaterialRow>::new(),
            geometry: vec![GeometryInstance {
                element_key: "d:e".into(),
                status: "ok".into(),
                source_frame: "z".into(),
                local_origin_meters: [10., 20., 30.],
                meshes: GraphicsMeshes {
                    primitives: vec![p],
                    ..Default::default()
                },
                render_materials: Default::default(),
                unresolved_material_primitives: Vec::new(),
            }],
            spatial_context: None,
            spatial_boundaries: None,
            room_connections: None,
            network: None,
            texture_mappings: None,
            metrics: Vec::new(),
        }
    }
    fn json_doc(bytes: &[u8]) -> Value {
        let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let chunk = &bytes[20..20 + length];
        serde_json::from_slice(chunk.split(|b| *b == 0).next().unwrap()).unwrap()
    }
    #[test]
    fn emits_glb_with_stable_join_and_yup_positions() {
        let mut package = package();
        package.texture_mappings = Some(
            serde_json::to_value(crate::native_texture_mapping::Inventory {
                mappings: vec![crate::native_texture_mapping::Mapping {
                    element_id: 7,
                    unique_id: "e".into(),
                    instance_object_index: None,
                    geometry_tag: 0,
                    material_id: 7,
                    placer_mirrored: false,
                    world_to_uv: [[1., 0., 0., 0.], [0., 1., 0., 0.]],
                    plane_origin: [0., 0., 33.0 / 0.3048],
                    plane_normal: [0., 0., 1.],
                    parametric_to_uv: None,
                    surface: None,
                    source: Value::Null,
                }],
                saved_fillings: vec![],
                diagnostics: vec![],
                excluded_graphics_groups: vec![],
            })
            .unwrap(),
        );
        let a = build_glb(&package).unwrap();
        assert_eq!(&a.bytes[0..4], b"glTF");
        assert_eq!(a.attributes[0]["element_key"], "d:e");
        let doc = json_doc(&a.bytes);
        let json_length = u32::from_le_bytes(a.bytes[12..16].try_into().unwrap()) as usize;
        let raw_json = &a.bytes[20..20 + json_length];
        assert!(
            raw_json
                .iter()
                .rev()
                .take_while(|byte| **byte == b' ')
                .count()
                <= 3
        );
        assert!(!raw_json.contains(&0));
        assert_eq!(
            doc["extras"]["glb_byte_length"].as_u64(),
            Some(a.bytes.len() as u64)
        );
        assert_eq!(
            doc["meshes"][0]["primitives"][0]["extras"]["native_delivery"]["mesh_key"],
            "d:e:mesh:0"
        );
        assert!(doc["meshes"][0]["primitives"][0]["attributes"]["TEXCOORD_0"].is_number());
    }
    #[test]
    fn refuses_malformed_normals_and_indices() {
        let mut bad = package();
        bad.geometry[0].meshes.primitives[0].normals[0] = [0., 0., 0.];
        assert!(build_glb(&bad).is_err());
        let mut bad = package();
        bad.geometry[0].meshes.primitives[0].triangles[0][0] = 9;
        assert!(build_glb(&bad).is_err());
    }

    #[test]
    fn shares_binary_accessors_for_identical_owner_geometry() {
        let mut package = package();
        let mut duplicate = package.geometry[0].clone();
        duplicate.element_key = "d:e-duplicate".into();
        duplicate.local_origin_meters = [100., 200., 300.];
        package.geometry.push(duplicate);
        let doc = json_doc(&build_glb(&package).unwrap().bytes);
        assert_eq!(doc["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(doc["meshes"].as_array().unwrap().len(), 2);
        assert_eq!(doc["bufferViews"].as_array().unwrap().len(), 3);
        assert_eq!(doc["accessors"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn records_uv_rejection_without_dropping_geometry() {
        let mut package = package();
        package.texture_mappings = Some(
            serde_json::to_value(crate::native_texture_mapping::Inventory {
                mappings: vec![crate::native_texture_mapping::Mapping {
                    element_id: 7,
                    unique_id: "e".into(),
                    instance_object_index: None,
                    geometry_tag: 0,
                    material_id: 7,
                    placer_mirrored: false,
                    world_to_uv: [[1., 0., 0., 0.], [0., 1., 0., 0.]],
                    plane_origin: [0., 0., 34.0 / 0.3048],
                    plane_normal: [0., 0., 1.],
                    parametric_to_uv: None,
                    surface: None,
                    source: Value::Null,
                }],
                saved_fillings: vec![],
                diagnostics: vec![],
                excluded_graphics_groups: vec![],
            })
            .unwrap(),
        );
        let doc = json_doc(&build_glb(&package).unwrap().bytes);
        let primitive = &doc["meshes"][0]["primitives"][0];
        assert!(primitive["attributes"]["TEXCOORD_0"].is_null());
        assert_eq!(
            primitive["extras"]["native_delivery"]["texture_mapping"]["status"],
            "not_emitted"
        );
        assert_eq!(
            primitive["extras"]["native_delivery"]["texture_mapping"]["reason"],
            "texture point outside saved face plane"
        );
        assert_eq!(primitive["indices"], 2);
    }
}
