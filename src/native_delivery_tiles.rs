//! Exact package-content 3D Tiles 1.1 adapter.
use crate::{native_delivery::DeliveryPackage, native_delivery_glb::build_glb};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct TilesArtifact {
    pub tileset: Value,
    pub contents: Vec<(String, Vec<u8>)>,
}

fn package_box(package: &DeliveryPackage) -> Result<[f64; 12]> {
    let mut mn = [f64::INFINITY; 3];
    let mut mx = [f64::NEG_INFINITY; 3];
    let mut any = false;
    for owner in &package.geometry {
        for primitive in &owner.meshes.primitives {
            if primitive.triangles.is_empty() {
                continue;
            }
            for vertex in &primitive.vertices {
                for i in 0..3 {
                    let value = vertex[i] + owner.local_origin_meters[i];
                    ensure!(value.is_finite(), "nonfinite tile bound");
                    mn[i] = mn[i].min(value);
                    mx[i] = mx[i].max(value);
                }
                any = true;
            }
        }
    }
    ensure!(any, "empty tile bounds");
    let c = [
        (mn[0] + mx[0]) / 2.,
        (mn[1] + mx[1]) / 2.,
        (mn[2] + mx[2]) / 2.,
    ];
    Ok([
        c[0],
        c[1],
        c[2],
        (mx[0] - mn[0]) / 2.,
        0.,
        0.,
        0.,
        (mx[1] - mn[1]) / 2.,
        0.,
        0.,
        0.,
        (mx[2] - mn[2]) / 2.,
    ])
}
/// Compute package bounds without constructing the binary GLB payload.
/// Sharded writers use this for the aggregate manifest; constructing a GLB
/// here would duplicate the peak geometry working set before the package is
/// written.
pub fn bounding_volume(package: &DeliveryPackage) -> Result<Value> {
    Ok(json!(package_box(package)?))
}

pub fn build_tileset(package: &DeliveryPackage) -> Result<TilesArtifact> {
    let mut skipped = Vec::new();
    for owner in &package.geometry {
        if owner
            .meshes
            .primitives
            .iter()
            .any(|p| !p.triangles.is_empty())
        {
            continue;
        }
        skipped.push(json!({
            "element_key": owner.element_key.clone(),
            "geometry_status": owner.status.clone(),
            "reason": "empty_geometry"
        }));
    }
    let root_box = bounding_volume(package)?;
    let glb = build_glb(package)?;
    let uri = "content.glb";
    let tileset = json!({"asset":{"version":"1.1","extras":{"lod":"exact package geometry; geometricError 0; no simplification"}},"geometricError":0,"root":{"boundingVolume":{"box":root_box},"geometricError":0,"refine":"ADD","content":{"uri":uri}},"extras":{"attribute_table_uri":"elements.json","glb_attribute_table":"content.glb#extras.attribute_table","coordinate_frame":"meters Z-up","skipped_empty_geometry_elements":skipped}});
    Ok(TilesArtifact {
        tileset,
        contents: vec![(uri.into(), glb.bytes)],
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_delivery::{
        Coverage, DeliveryManifest, ElementRow, GeometryInstance, InstanceRow, MaterialRow,
        MeshRow, RelationshipRow, TypeRow,
    };
    use crate::native_saved_mesh::{GraphicsMeshes, Primitive};

    fn package() -> DeliveryPackage {
        let primitive = Primitive {
            source_owner_id: None,
            object_index: 1,
            face_tag: 2,
            render_style_id: 3,
            material_id: None,
            vertices: vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
            normals: vec![[0., 0., 1.]; 3],
            triangles: vec![[0, 1, 2]],
        };
        let mesh = GraphicsMeshes {
            primitives: vec![primitive],
            ..Default::default()
        };
        let geometry = vec![
            GeometryInstance {
                element_key: "d:mesh".into(),
                status: "decoded".into(),
                source_frame: "z".into(),
                local_origin_meters: [10., 20., 30.],
                meshes: mesh,
                render_materials: Default::default(),
                unresolved_material_primitives: Vec::new(),
            },
            GeometryInstance {
                element_key: "d:metadata".into(),
                status: "metadata_only".into(),
                source_frame: "z".into(),
                local_origin_meters: [0., 0., 0.],
                meshes: GraphicsMeshes::default(),
                render_materials: Default::default(),
                unresolved_material_primitives: Vec::new(),
            },
        ];
        DeliveryPackage {
            manifest: DeliveryManifest {
                format: "x",
                profile: "tiles",
                document_namespace: "d".into(),
                source_sha256: None,
                status: "partial".into(),
                units: "meters",
                coordinate_frame: "z",
                parameter_definitions_uri: None,
                parameter_bindings_uri: None,
                coverage: Coverage::default(),
                tables: vec![],
            },
            elements: Vec::<ElementRow>::new(),
            parameter_definitions: vec![],
            parameter_bindings: vec![],
            types: Vec::<TypeRow>::new(),
            relationships: Vec::<RelationshipRow>::new(),
            meshes: Vec::<MeshRow>::new(),
            instances: Vec::<InstanceRow>::new(),
            materials: Vec::<MaterialRow>::new(),
            geometry,
            spatial_context: None,
            spatial_boundaries: None,
            room_connections: None,
            network: None,
            texture_mappings: None,
            metrics: Vec::new(),
        }
    }

    #[test]
    fn exact_tiles_skip_empty_geometry_with_explicit_receipt() {
        let artifact = build_tileset(&package()).unwrap();
        assert_eq!(artifact.contents.len(), 1);
        assert_eq!(artifact.tileset["root"]["content"]["uri"], "content.glb");
        assert_eq!(
            artifact.tileset["extras"]["skipped_empty_geometry_elements"][0]["element_key"],
            "d:metadata"
        );
        assert_eq!(
            artifact.tileset["asset"]["extras"]["lod"],
            "exact package geometry; geometricError 0; no simplification"
        );
    }
}
