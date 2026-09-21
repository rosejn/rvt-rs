//! Pure spatial primitives for native scene delivery.
//!
//! Coordinates remain `f64` until the final, caller-owned conversion to a
//! render buffer.  Matrices are row-major and act on column vectors.

use anyhow::{Result, ensure};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix4(pub [[f64; 4]; 4]);

impl Matrix4 {
    pub const IDENTITY: Self = Self([
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ]);
    pub fn new(rows: [[f64; 4]; 4]) -> Result<Self> {
        ensure!(
            rows.iter().flatten().all(|x| x.is_finite()),
            "nonfinite matrix"
        );
        ensure!(
            rows[3][0].abs() < 1e-12
                && rows[3][1].abs() < 1e-12
                && rows[3][2].abs() < 1e-12
                && (rows[3][3] - 1.).abs() < 1e-12,
            "matrix is not affine"
        );
        Ok(Self(rows))
    }
    pub fn compose(self, rhs: Self) -> Self {
        let mut out = [[0.; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                out[i][j] = (0..4).map(|k| self.0[i][k] * rhs.0[k][j]).sum();
            }
        }
        Self(out)
    }
    pub fn transform_point(self, p: [f64; 3]) -> [f64; 3] {
        [
            self.0[0][0] * p[0] + self.0[0][1] * p[1] + self.0[0][2] * p[2] + self.0[0][3],
            self.0[1][0] * p[0] + self.0[1][1] * p[1] + self.0[1][2] * p[2] + self.0[1][3],
            self.0[2][0] * p[0] + self.0[2][1] * p[1] + self.0[2][2] * p[2] + self.0[2][3],
        ]
    }
    pub fn transform_vector(self, v: [f64; 3]) -> [f64; 3] {
        [
            self.0[0][0] * v[0] + self.0[0][1] * v[1] + self.0[0][2] * v[2],
            self.0[1][0] * v[0] + self.0[1][1] * v[1] + self.0[1][2] * v[2],
            self.0[2][0] * v[0] + self.0[2][1] * v[1] + self.0[2][2] * v[2],
        ]
    }
    pub fn determinant3(self) -> f64 {
        let a = self.0;
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    }
    pub fn inverse(self) -> Result<Self> {
        let a = self.0;
        let d = self.determinant3();
        ensure!(d.abs() > 1e-14 && d.is_finite(), "singular affine matrix");
        let mut r = [[0.; 4]; 4];
        r[3][3] = 1.;
        r[0][0] = (a[1][1] * a[2][2] - a[1][2] * a[2][1]) / d;
        r[0][1] = (a[0][2] * a[2][1] - a[0][1] * a[2][2]) / d;
        r[0][2] = (a[0][1] * a[1][2] - a[0][2] * a[1][1]) / d;
        r[1][0] = (a[1][2] * a[2][0] - a[1][0] * a[2][2]) / d;
        r[1][1] = (a[0][0] * a[2][2] - a[0][2] * a[2][0]) / d;
        r[1][2] = (a[0][2] * a[1][0] - a[0][0] * a[1][2]) / d;
        r[2][0] = (a[1][0] * a[2][1] - a[1][1] * a[2][0]) / d;
        r[2][1] = (a[0][1] * a[2][0] - a[0][0] * a[2][1]) / d;
        r[2][2] = (a[0][0] * a[1][1] - a[0][1] * a[1][0]) / d;
        for i in 0..3 {
            r[i][3] = -(r[i][0] * a[0][3] + r[i][1] * a[1][3] + r[i][2] * a[2][3]);
        }
        Ok(Self(r))
    }
    /// Inverse-transpose of the linear part, for transforming normals.
    pub fn normal_inverse_transpose(self) -> Result<[[f64; 3]; 3]> {
        let i = self.inverse()?;
        Ok([
            [i.0[0][0], i.0[1][0], i.0[2][0]],
            [i.0[0][1], i.0[1][1], i.0[2][1]],
            [i.0[0][2], i.0[1][2], i.0[2][2]],
        ])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Bounds3 {
    pub min: [f64; 3],
    pub max: [f64; 3],
}
impl Bounds3 {
    pub fn from_points(points: &[[f64; 3]]) -> Result<Self> {
        ensure!(!points.is_empty(), "empty bounds");
        let mut min = points[0];
        let mut max = points[0];
        for p in points {
            ensure!(p.iter().all(|x| x.is_finite()), "nonfinite bound point");
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        Ok(Self { min, max })
    }
    pub fn corners(self) -> [[f64; 3]; 8] {
        let (x0, x1, y0, y1, z0, z1) = (
            self.min[0],
            self.max[0],
            self.min[1],
            self.max[1],
            self.min[2],
            self.max[2],
        );
        [
            [x0, y0, z0],
            [x1, y0, z0],
            [x0, y1, z0],
            [x1, y1, z0],
            [x0, y0, z1],
            [x1, y0, z1],
            [x0, y1, z1],
            [x1, y1, z1],
        ]
    }
    pub fn transform(self, m: Matrix4) -> Result<Self> {
        Bounds3::from_points(&self.corners().map(|p| m.transform_point(p)))
    }
    pub fn center(self) -> [f64; 3] {
        [
            (self.min[0] + self.max[0]) / 2.,
            (self.min[1] + self.max[1]) / 2.,
            (self.min[2] + self.max[2]) / 2.,
        ]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RebasedMesh {
    pub origin: [f64; 3],
    pub positions: Vec<[f32; 3]>,
}
pub fn rebase_positions(points: &[[f64; 3]], origin: [f64; 3]) -> Result<RebasedMesh> {
    ensure!(
        origin.iter().all(|x| x.is_finite()),
        "nonfinite rebase origin"
    );
    let positions = points
        .iter()
        .map(|p| {
            ensure!(p.iter().all(|x| x.is_finite()), "nonfinite position");
            let q = [p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]];
            ensure!(
                q.iter().all(|x| x.is_finite()),
                "nonfinite rebased position"
            );
            let out = q.map(|x| x as f32);
            ensure!(
                out.iter().all(|x| x.is_finite()),
                "rebased position exceeds f32 range"
            );
            Ok(out)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(RebasedMesh { origin, positions })
}
pub fn rebase_around_bounds(points: &[[f64; 3]]) -> Result<RebasedMesh> {
    let b = Bounds3::from_points(points)?;
    rebase_positions(points, b.center())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DocumentSiteMapping {
    pub source_frame: String,
    pub target_frame: String,
    pub matrix: [[f64; 4]; 4],
    pub units: String,
}
impl DocumentSiteMapping {
    pub fn matrix(&self) -> Result<Matrix4> {
        Matrix4::new(self.matrix)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SitePlacementStatus {
    Unregistered,
    Explicit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BakedGraphicsPlacement {
    pub instance: Matrix4,
    pub site: Option<DocumentSiteMapping>,
    pub status: SitePlacementStatus,
}

/// Resolve placement for graphics already baked in document coordinates.
/// A known native instance is inverted once to recover reusable local geometry;
/// the instance is then applied exactly once by the delivery consumer.
pub fn placement_for_baked_graphics(
    native_instance: Option<Matrix4>,
    site: Option<DocumentSiteMapping>,
) -> Result<BakedGraphicsPlacement> {
    if let Some(s) = &site {
        ensure!(
            !s.source_frame.trim().is_empty()
                && !s.target_frame.trim().is_empty()
                && !s.units.trim().is_empty(),
            "site mapping frame identifiers and units are required"
        );
        s.matrix()?;
    }
    let instance = native_instance.map_or(Ok(Matrix4::IDENTITY), Matrix4::inverse)?;
    let status = if site.is_some() {
        SitePlacementStatus::Explicit
    } else {
        SitePlacementStatus::Unregistered
    };
    Ok(BakedGraphicsPlacement {
        instance,
        site,
        status,
    })
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum BoundsCompleteness {
    ExtractedMeshSubset,
    CompleteObject,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SpatialBounds {
    pub bounds: Bounds3,
    pub completeness: BoundsCompleteness,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SpaceVolume {
    pub owner_element_id: u64,
    pub role: String,
    pub boundary_kind: String,
    pub loops: Vec<Vec<[[f64; 3]; 2]>>,
    pub base: f64,
    pub top: f64,
    pub bounds: SpatialBounds,
    pub host_link_refs: Vec<(i64, i64)>,
    pub assumption: String,
}

/// Derive a vertical space only from an evaluated boundary and explicit level elevations.
pub fn derive_space(
    room: &crate::native_spatial_boundaries::Room,
    elevations: &BTreeMap<i64, f64>,
) -> Result<SpaceVolume> {
    let matches: Vec<_> = room
        .evaluated_boundaries
        .iter()
        .filter(|b| b.boundary_location == "Finish")
        .collect();
    ensure!(
        matches.len() == 1,
        "missing or ambiguous Finish evaluated boundary"
    );
    let b = matches[0];
    ensure!(
        b.status.starts_with("evaluated_"),
        "boundary is not evaluated"
    );
    ensure!(
        !b.loops.is_empty() && b.loops.iter().all(|l| !l.is_empty()),
        "empty evaluated boundary"
    );
    let base = *elevations
        .get(&room.level_id)
        .ok_or_else(|| anyhow::anyhow!("missing base level elevation"))?
        + room.lower_offset;
    let top = if room.upper_level_id >= 0 {
        let z = *elevations
            .get(&room.upper_level_id)
            .ok_or_else(|| anyhow::anyhow!("missing upper level elevation"))?;
        z + room
            .upper_offset
            .ok_or_else(|| anyhow::anyhow!("missing upper level offset"))?
    } else {
        base + room
            .height
            .ok_or_else(|| anyhow::anyhow!("missing space height"))?
    };
    ensure!(top > base, "invalid or slanted space vertical extent");
    let loops = b
        .loops
        .iter()
        .map(|l| l.iter().map(|s| [s.start, s.end]).collect())
        .collect();
    let refs = b
        .loops
        .iter()
        .flat_map(|l| l.iter().map(|s| (s.element_id, s.link_element_id)))
        .collect();
    let mut points = Vec::new();
    for l in &b.loops {
        for s in l {
            points.push(s.start);
            points.push(s.end);
        }
    }
    let mut volume_points = points.clone();
    volume_points.extend(points.iter().map(|p| [p[0], p[1], top]));
    let bounds = SpatialBounds {
        bounds: Bounds3::from_points(&volume_points)?,
        completeness: BoundsCompleteness::CompleteObject,
    };
    Ok(SpaceVolume{owner_element_id:room.owner.element_id,role:room.source_kind.clone(),boundary_kind:b.boundary_location.clone(),loops,base,top,bounds,host_link_refs:refs,assumption:"qualified evaluated plan loops extruded between explicit level/height references; holes retained".into()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_equipment::Identity;
    use crate::native_spatial_boundaries::{Boundary, BoundarySegment, Room, Source};
    fn room(boundary_location: &str) -> Room {
        let source = Source {
            owner: Identity {
                element_id: 42,
                unique_id: "room".into(),
            },
            object_index: 0,
            field: "".into(),
            body_sha256: "".into(),
            stream: "".into(),
            group_record_offset: 0,
        };
        let boundary = Boundary {
            boundary_location: boundary_location.into(),
            loops: vec![vec![
                BoundarySegment {
                    element_id: 7,
                    link_element_id: -1,
                    start: [0., 0., 0.],
                    end: [4., 0., 0.],
                    sources: vec![],
                },
                BoundarySegment {
                    element_id: 7,
                    link_element_id: -1,
                    start: [4., 0., 0.],
                    end: [4., 3., 0.],
                    sources: vec![],
                },
                BoundarySegment {
                    element_id: 7,
                    link_element_id: -1,
                    start: [4., 3., 0.],
                    end: [0., 3., 0.],
                    sources: vec![],
                },
                BoundarySegment {
                    element_id: 7,
                    link_element_id: -1,
                    start: [0., 3., 0.],
                    end: [0., 0., 0.],
                    sources: vec![],
                },
            ]],
            status: "evaluated_test",
            coordinate_space: "document_internal",
            length_unit: "feet",
        };
        Room {
            owner: source.owner.clone(),
            source_kind: "room".into(),
            zone_scheme_id: -1,
            area_scheme_id: -1,
            level_id: 1,
            phase_id: 1,
            upper_level_id: 2,
            lower_offset: 0.,
            upper_offset: Some(0.),
            height: None,
            locationless: Some(false),
            cached_circuit_id: 1,
            topology_owner: None,
            circuit: None,
            carrier_loops: Vec::new(),
            status: "resolved".into(),
            source,
            evaluated_boundaries: vec![boundary],
        }
    }
    #[test]
    fn matrix_inverse_normals_and_mirror() {
        let m = Matrix4::new([
            [-2., 0., 0., 10.],
            [0., 3., 0., 20.],
            [0., 0., 1., 30.],
            [0., 0., 0., 1.],
        ])
        .unwrap();
        assert_eq!(m.transform_point([1., 2., 3.]), [8., 26., 33.]);
        assert!((m.compose(m.inverse().unwrap()).0[0][0] - 1.).abs() < 1e-9);
        assert!(m.determinant3() < 0.);
        let n = m.normal_inverse_transpose().unwrap();
        assert!((n[0][0] + 0.5).abs() < 1e-9);
    }
    #[test]
    fn bounds_corners_transform_and_large_rebase() {
        let p = [
            [1e12 + 1., 1e12 + 2., 1e12 + 3.],
            [1e12 + 5., 1e12 + 7., 1e12 + 9.],
        ];
        let b = Bounds3::from_points(&p).unwrap();
        assert_eq!(b.corners().len(), 8);
        let r = rebase_around_bounds(&p).unwrap();
        assert_eq!(r.positions, vec![[-2., -2.5, -3.], [2., 2.5, 3.]]);
    }
    #[test]
    fn missing_registration_is_not_identity() {
        let m = DocumentSiteMapping {
            source_frame: "doc".into(),
            target_frame: "site".into(),
            matrix: [[0.; 4]; 4],
            units: "m".into(),
        };
        assert!(m.matrix().is_err());
    }
    #[test]
    fn derives_only_qualified_finish_volume_with_explicit_levels() {
        let mut elevations = BTreeMap::from([(1, 0.), (2, 10.)]);
        let volume = derive_space(&room("Finish"), &elevations).unwrap();
        assert_eq!(volume.owner_element_id, 42);
        assert_eq!(volume.base, 0.);
        assert_eq!(volume.top, 10.);
        assert_eq!(
            volume.bounds.completeness,
            BoundsCompleteness::CompleteObject
        );
        elevations.remove(&2);
        assert!(derive_space(&room("Finish"), &elevations).is_err());
        assert!(derive_space(&room("Center"), &BTreeMap::from([(1, 0.), (2, 10.)])).is_err());
    }
}
