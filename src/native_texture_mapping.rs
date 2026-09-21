//! Saved planar texture coordinates from current channel-103 graphics ownership.
//! This projection does not regenerate graphics after parameter changes. It is
//! separate from the material's subsequent bitmap sampling transform.
use crate::{
    RevitFile,
    native_document::{self, Record},
    native_graphics_traversal::{self, GraphicsTransform},
    native_metadata::identifier,
    native_parameters::ObjectGraph,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Deserialize, Serialize)]
pub struct Mapping {
    pub element_id: u64,
    pub unique_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_object_index: Option<usize>,
    pub geometry_tag: i64,
    pub material_id: i64,
    /// Saved placer mirror state. This is provenance for the later image/
    /// display transform; the raw geometry UV carrier remains `m_dir` and
    /// `m_origin`.
    #[serde(default)]
    pub placer_mirrored: bool,
    /// Row-major affine map: world feet `[x, y, z, 1]` -> raw exporter UV.
    pub world_to_uv: [[f64; 4]; 2],
    pub plane_origin: [f64; 3],
    pub plane_normal: [f64; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parametric_to_uv: Option<[[f64; 3]; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<SurfaceMapping>,
    pub source: Value,
}
impl Mapping {
    pub fn evaluate(&self, point: [f64; 3]) -> Result<[f64; 2]> {
        ensure!(
            point.iter().all(|x| x.is_finite()),
            "nonfinite texture mapping point"
        );
        if let (Some(surface), Some(transform)) = (&self.surface, self.parametric_to_uv) {
            let [u, v] = surface.parameters(point)?;
            return Ok(transform.map(|row| row[2] + row[0] * u + row[1] * v));
        }
        let distance: f64 = (0..3)
            .map(|i| (point[i] - self.plane_origin[i]) * self.plane_normal[i])
            .sum();
        ensure!(
            distance.abs() <= 1e-7,
            "texture point outside saved face plane"
        );
        Ok(self
            .world_to_uv
            .map(|r| r[3] + (0..3).map(|i| r[i] * point[i]).sum::<f64>()))
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum SurfaceMapping {
    CylSurf {
        center: [f64; 3],
        radius: f64,
        x_vec: [f64; 3],
        y_vec: [f64; 3],
        z_vec: [f64; 3],
        u_range: [f64; 2],
        v_range: [f64; 2],
    },
    SurfRev {
        center: [f64; 3],
        x_vec: [f64; 3],
        y_vec: [f64; 3],
        z_vec: [f64; 3],
        u_range: [f64; 2],
        v_range: [f64; 2],
        profile: SurfaceProfile,
    },
    HermiteSurf {
        u_range: [f64; 2],
        v_range: [f64; 2],
        u_params: Vec<f64>,
        v_params: Vec<f64>,
        periodic: [bool; 2],
        nodes: Vec<HermiteSurfaceNode>,
    },
    RuledSurf {
        u_range: [f64; 2],
        v_range: [f64; 2],
        profile1: SurfaceProfile,
        profile2: SurfaceProfile,
    },
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum SurfaceProfile {
    HermiteSpline {
        nodes: Vec<HermiteProfileNode>,
        range: [f64; 2],
        periodic: bool,
    },
    GArc {
        center: [f64; 3],
        radius: f64,
        x_vec: [f64; 3],
        y_vec: [f64; 3],
        range: [f64; 2],
    },
    GEllipse {
        center: [f64; 3],
        x_vec: [f64; 3],
        y_vec: [f64; 3],
        x_len: f64,
        y_len: f64,
        range: [f64; 2],
    },
    GLine {
        origin: [f64; 3],
        dir_vec: [f64; 3],
        range: [f64; 2],
    },
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HermiteProfileNode {
    parameter: f64,
    point: [f64; 3],
    tangent: [f64; 3],
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HermiteSurfaceNode {
    point: [f64; 3],
    u_tangent: [f64; 3],
    v_tangent: [f64; 3],
    mixed_derivative: [f64; 3],
}
impl SurfaceMapping {
    fn transform(&mut self, trf: [[f64; 4]; 4]) -> Result<()> {
        let transform_point = |point: [f64; 3]| {
            std::array::from_fn(|row| {
                trf[row][3]
                    + (0..3)
                        .map(|column| trf[row][column] * point[column])
                        .sum::<f64>()
            })
        };
        let transform_vector = |vector: [f64; 3]| {
            std::array::from_fn(|row| {
                (0..3)
                    .map(|column| trf[row][column] * vector[column])
                    .sum::<f64>()
            })
        };
        match self {
            Self::CylSurf {
                radius,
                center,
                x_vec,
                y_vec,
                z_vec,
                ..
            } => {
                *center = transform_point(*center);
                *x_vec = transform_vector(*x_vec);
                *y_vec = transform_vector(*y_vec);
                *z_vec = transform_vector(*z_vec);
                let scale = x_vec.iter().map(|x| x * x).sum::<f64>().sqrt();
                ensure!(
                    scale.is_finite()
                        && scale > 1e-12
                        && [*x_vec, *y_vec, *z_vec].iter().all(|v| {
                            (v.iter().map(|x| x * x).sum::<f64>().sqrt() - scale).abs()
                                <= scale * 1e-9
                        })
                        && dot(*x_vec, *y_vec).abs() <= scale * scale * 1e-9
                        && dot(*x_vec, *z_vec).abs() <= scale * scale * 1e-9
                        && dot(*y_vec, *z_vec).abs() <= scale * scale * 1e-9,
                    "scaled texture cylinder transform is not a similarity"
                );
                *radius *= scale;
                for vector in [x_vec, y_vec, z_vec] {
                    for component in vector.iter_mut() {
                        *component /= scale;
                    }
                }
            }
            Self::SurfRev {
                center,
                x_vec,
                y_vec,
                z_vec,
                ..
            } => {
                *center = transform_point(*center);
                *x_vec = transform_vector(*x_vec);
                *y_vec = transform_vector(*y_vec);
                *z_vec = transform_vector(*z_vec);
                let scale = x_vec.iter().map(|x| x * x).sum::<f64>().sqrt();
                ensure!(
                    scale.is_finite()
                        && scale > 1e-12
                        && [*x_vec, *y_vec, *z_vec].iter().all(|v| {
                            (v.iter().map(|x| x * x).sum::<f64>().sqrt() - scale).abs()
                                <= scale * 1e-9
                        })
                        && dot(*x_vec, *y_vec).abs() <= scale * scale * 1e-9
                        && dot(*x_vec, *z_vec).abs() <= scale * scale * 1e-9
                        && dot(*y_vec, *z_vec).abs() <= scale * scale * 1e-9,
                    "scaled texture revolution transform is not a similarity"
                );
                for vector in [x_vec, y_vec, z_vec] {
                    for component in vector.iter_mut() {
                        *component /= scale;
                    }
                }
            }
            Self::HermiteSurf { nodes, .. } => {
                for node in nodes {
                    node.point = transform_point(node.point);
                    node.u_tangent = transform_vector(node.u_tangent);
                    node.v_tangent = transform_vector(node.v_tangent);
                    node.mixed_derivative = transform_vector(node.mixed_derivative);
                }
            }
            Self::RuledSurf {
                profile1, profile2, ..
            } => {
                profile1.transform(trf);
                profile2.transform(trf);
            }
        }
        Ok(())
    }
    fn parameters(&self, point: [f64; 3]) -> Result<[f64; 2]> {
        match self {
            Self::CylSurf {
                center,
                radius,
                x_vec,
                y_vec,
                z_vec,
                u_range,
                v_range,
            } => {
                let q = std::array::from_fn(|i| point[i] - center[i]);
                let radial = [dot(q, *x_vec), dot(q, *y_vec)];
                let radial_length = radial[0].hypot(radial[1]);
                ensure!(
                    (radial_length - *radius).abs() <= 1e-6,
                    "texture point outside saved cylinder"
                );
                let raw_u = radial[1].atan2(radial[0]);
                let u = unwrap_periodic(raw_u, *u_range)?;
                let v = dot(q, *z_vec);
                ensure!(
                    v >= v_range[0] - 1e-6 && v <= v_range[1] + 1e-6,
                    "texture point outside saved cylinder envelope"
                );
                Ok([u, v])
            }
            Self::SurfRev {
                center,
                x_vec,
                y_vec,
                z_vec,
                u_range,
                v_range,
                profile,
            } => {
                let q = std::array::from_fn(|i| point[i] - center[i]);
                let local = [dot(q, *x_vec), dot(q, *y_vec), dot(q, *z_vec)];
                let u = unwrap_periodic(local[1].atan2(local[0]), *u_range)?;
                let radial = local[0].hypot(local[1]);
                let v = profile.parameter_for([radial, 0., local[2]])?;
                ensure!(
                    v >= v_range[0] - 1e-6 && v <= v_range[1] + 1e-6,
                    "texture point outside saved revolution envelope"
                );
                Ok([u, v])
            }
            Self::HermiteSurf {
                u_range,
                v_range,
                u_params,
                v_params,
                periodic,
                nodes,
            } => {
                let [u, v] = invert_hermite_surface(
                    point, *u_range, *v_range, u_params, v_params, *periodic, nodes,
                )?;
                Ok([u, v])
            }
            Self::RuledSurf {
                u_range,
                v_range,
                profile1,
                profile2,
            } => invert_ruled_surface(*u_range, *v_range, profile1, profile2, point),
        }
    }
}
impl SurfaceProfile {
    fn transform(&mut self, trf: [[f64; 4]; 4]) {
        let point = |value: [f64; 3]| {
            std::array::from_fn(|row| {
                trf[row][3]
                    + (0..3)
                        .map(|column| trf[row][column] * value[column])
                        .sum::<f64>()
            })
        };
        let vector = |value: [f64; 3]| {
            std::array::from_fn(|row| {
                (0..3)
                    .map(|column| trf[row][column] * value[column])
                    .sum::<f64>()
            })
        };
        match self {
            Self::HermiteSpline { nodes, .. } => {
                for node in nodes {
                    node.point = point(node.point);
                    node.tangent = vector(node.tangent);
                }
            }
            Self::GArc {
                center,
                x_vec,
                y_vec,
                ..
            }
            | Self::GEllipse {
                center,
                x_vec,
                y_vec,
                ..
            } => {
                *center = point(*center);
                *x_vec = vector(*x_vec);
                *y_vec = vector(*y_vec);
            }
            Self::GLine {
                origin, dir_vec, ..
            } => {
                *origin = point(*origin);
                *dir_vec = vector(*dir_vec);
            }
        }
    }

    fn evaluate(&self, t: f64) -> Result<[f64; 3]> {
        Ok(match self {
            Self::HermiteSpline {
                nodes,
                range,
                periodic,
            } => {
                if *periodic {
                    periodic_hermite_value(nodes, *range, t)?.0
                } else {
                    let (a, b, s) = hermite_segment(nodes, t)?;
                    hermite_value(a, b, s, b.parameter - a.parameter, false)
                }
            }
            Self::GArc {
                center,
                radius,
                x_vec,
                y_vec,
                ..
            } => std::array::from_fn(|i| {
                center[i] + radius * (t.cos() * x_vec[i] + t.sin() * y_vec[i])
            }),
            Self::GEllipse {
                center,
                x_vec,
                y_vec,
                x_len,
                y_len,
                ..
            } => std::array::from_fn(|i| {
                center[i] + x_len * t.cos() * x_vec[i] + y_len * t.sin() * y_vec[i]
            }),
            Self::GLine {
                origin, dir_vec, ..
            } => std::array::from_fn(|i| origin[i] + t * dir_vec[i]),
        })
    }

    fn parameter_for(&self, target: [f64; 3]) -> Result<f64> {
        match self {
            Self::HermiteSpline {
                nodes,
                range,
                periodic,
            } => {
                if !*periodic {
                    let mut t = range[0] + 0.5 * (range[1] - range[0]);
                    for _ in 0..16 {
                        let (a, b, s) = hermite_segment(nodes, t)?;
                        let delta = b.parameter - a.parameter;
                        let value = hermite_value(a, b, s, delta, false);
                        let derivative = hermite_value(a, b, s, delta, true);
                        let residual = std::array::from_fn(|i| value[i] - target[i]);
                        let denom = dot(derivative, derivative);
                        ensure!(
                            denom.is_finite() && denom > 1e-18,
                            "singular texture profile"
                        );
                        let step = dot(residual, derivative) / denom;
                        t = (t - step).clamp(range[0], range[1]);
                        if step.abs() <= 1e-12 {
                            break;
                        }
                    }
                    let (a, b, s) = hermite_segment(nodes, t)?;
                    let value = hermite_value(a, b, s, b.parameter - a.parameter, false);
                    let error = value
                        .iter()
                        .zip(target)
                        .map(|(a, b)| (a - b).powi(2))
                        .sum::<f64>()
                        .sqrt();
                    ensure!(
                        error <= 1e-6,
                        "texture point outside saved revolution profile"
                    );
                    return Ok(t);
                }
                let period = range[1] - range[0];
                ensure!(
                    period.is_finite() && period > 0.,
                    "invalid periodic texture profile range"
                );
                ensure!(nodes.len() >= 2, "periodic texture profile needs two nodes");
                let mut best: Option<(f64, f64)> = None;
                for seed_index in 0..32 {
                    let mut t = range[0] + period * (seed_index as f64 + 0.5) / 32.;
                    for _ in 0..24 {
                        let (value, derivative) = periodic_hermite_value(nodes, *range, t)?;
                        let residual = std::array::from_fn(|i| value[i] - target[i]);
                        let denom = dot(derivative, derivative);
                        if !denom.is_finite() || denom <= 1e-18 {
                            break;
                        }
                        let step = dot(residual, derivative) / denom;
                        t = range[0] + (t - range[0] - step).rem_euclid(period);
                        if step.abs() <= 1e-12 {
                            break;
                        }
                    }
                    let (value, _) = periodic_hermite_value(nodes, *range, t)?;
                    let error = value
                        .iter()
                        .zip(target)
                        .map(|(a, b)| (a - b).powi(2))
                        .sum::<f64>()
                        .sqrt();
                    if error.is_finite() && best.is_none_or(|(best_error, _)| error < best_error) {
                        best = Some((error, t));
                    }
                }
                let (error, t) = best.context("periodic texture profile inversion failed")?;
                ensure!(
                    error <= 1e-6,
                    "texture point outside saved periodic revolution profile"
                );
                Ok(t)
            }
            Self::GArc {
                center,
                radius,
                x_vec,
                y_vec,
                range,
            } => {
                ensure!(
                    radius.is_finite() && *radius > 0.,
                    "invalid texture arc radius"
                );
                let q = std::array::from_fn(|i| target[i] - center[i]);
                let x = dot(q, *x_vec);
                let y = dot(q, *y_vec);
                let normal = std::array::from_fn(|i| q[i] - x * x_vec[i] - y * y_vec[i]);
                ensure!(
                    x.hypot(y).is_finite()
                        && (x.hypot(y) - *radius).abs() <= 1e-6
                        && dot(normal, normal).sqrt() <= 1e-6,
                    "texture point outside saved revolution arc"
                );
                let raw = y.atan2(x);
                let t = unwrap_angle_in_range(raw, *range)?;
                let point: [f64; 3] = std::array::from_fn(|i| {
                    center[i] + radius * (t.cos() * x_vec[i] + t.sin() * y_vec[i])
                });
                ensure!(
                    point
                        .iter()
                        .zip(target)
                        .map(|(a, b)| (a - b).powi(2))
                        .sum::<f64>()
                        .sqrt()
                        <= 1e-6,
                    "texture point outside saved revolution arc"
                );
                Ok(t)
            }
            Self::GEllipse {
                center,
                x_vec,
                y_vec,
                x_len,
                y_len,
                range,
            } => {
                ensure!(
                    x_len.is_finite() && y_len.is_finite() && *x_len > 0. && *y_len > 0.,
                    "invalid texture ellipse lengths"
                );
                let q = std::array::from_fn(|i| target[i] - center[i]);
                let x = dot(q, *x_vec) / *x_len;
                let y = dot(q, *y_vec) / *y_len;
                let normal =
                    std::array::from_fn(|i| q[i] - x * *x_len * x_vec[i] - y * *y_len * y_vec[i]);
                ensure!(
                    (x * x + y * y - 1.).abs() <= 1e-6 && dot(normal, normal).sqrt() <= 1e-6,
                    "texture point outside saved revolution ellipse"
                );
                let t = unwrap_angle_in_range(y.atan2(x), *range)?;
                let point: [f64; 3] = std::array::from_fn(|i| {
                    center[i] + x_len * t.cos() * x_vec[i] + y_len * t.sin() * y_vec[i]
                });
                ensure!(
                    point
                        .iter()
                        .zip(target)
                        .map(|(a, b)| (a - b).powi(2))
                        .sum::<f64>()
                        .sqrt()
                        <= 1e-6,
                    "texture point outside saved revolution ellipse"
                );
                Ok(t)
            }
            Self::GLine {
                origin,
                dir_vec,
                range,
            } => {
                let q = std::array::from_fn(|i| target[i] - origin[i]);
                let denom = dot(*dir_vec, *dir_vec);
                ensure!(
                    denom.is_finite() && denom > 1e-18,
                    "singular texture line profile"
                );
                let t = dot(q, *dir_vec) / denom;
                ensure!(
                    t >= range[0] - 1e-6 && t <= range[1] + 1e-6,
                    "texture point outside saved revolution line"
                );
                let residual = std::array::from_fn(|i| origin[i] + t * dir_vec[i] - target[i]);
                ensure!(
                    dot(residual, residual).sqrt() <= 1e-6,
                    "texture point outside saved revolution line"
                );
                Ok(t)
            }
        }
    }
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn hermite_coefficients(s: f64, span: f64, derivative: bool) -> [f64; 4] {
    if derivative {
        [
            (6. * s * s - 6. * s) / span,
            3. * s * s - 4. * s + 1.,
            (-6. * s * s + 6. * s) / span,
            3. * s * s - 2. * s,
        ]
    } else {
        [
            2. * s * s * s - 3. * s * s + 1.,
            span * (s * s * s - 2. * s * s + s),
            -2. * s * s * s + 3. * s * s,
            span * (s * s * s - s * s),
        ]
    }
}

fn hermite_surface_point(
    nodes: &[HermiteSurfaceNode],
    u_params: &[f64],
    v_params: &[f64],
    u: f64,
    v: f64,
    derivative_u: bool,
    derivative_v: bool,
) -> Result<[f64; 3]> {
    let ui = u_params
        .windows(2)
        .position(|pair| u <= pair[1])
        .unwrap_or(u_params.len() - 2);
    let vi = v_params
        .windows(2)
        .position(|pair| v <= pair[1])
        .unwrap_or(v_params.len() - 2);
    let us = u_params[ui + 1] - u_params[ui];
    let vs = v_params[vi + 1] - v_params[vi];
    ensure!(
        us > 0. && vs > 0.,
        "Hermite surface parameter span is invalid"
    );
    let su = ((u - u_params[ui]) / us).clamp(0., 1.);
    let sv = ((v - v_params[vi]) / vs).clamp(0., 1.);
    let cu = hermite_coefficients(su, us, derivative_u);
    let cv = hermite_coefficients(sv, vs, derivative_v);
    let at = |j: usize, i: usize| &nodes[j * u_params.len() + i];
    let p00 = at(vi, ui);
    let p10 = at(vi, ui + 1);
    let p01 = at(vi + 1, ui);
    let p11 = at(vi + 1, ui + 1);
    Ok(std::array::from_fn(|k| {
        p00.point[k] * cu[0] * cv[0]
            + p00.u_tangent[k] * cu[1] * cv[0]
            + p10.point[k] * cu[2] * cv[0]
            + p10.u_tangent[k] * cu[3] * cv[0]
            + p01.point[k] * cu[0] * cv[2]
            + p01.u_tangent[k] * cu[1] * cv[2]
            + p11.point[k] * cu[2] * cv[2]
            + p11.u_tangent[k] * cu[3] * cv[2]
            + p00.v_tangent[k] * cu[0] * cv[1]
            + p00.mixed_derivative[k] * cu[1] * cv[1]
            + p10.v_tangent[k] * cu[2] * cv[1]
            + p10.mixed_derivative[k] * cu[3] * cv[1]
            + p01.v_tangent[k] * cu[0] * cv[3]
            + p01.mixed_derivative[k] * cu[1] * cv[3]
            + p11.v_tangent[k] * cu[2] * cv[3]
            + p11.mixed_derivative[k] * cu[3] * cv[3]
    }))
}

fn ruled_surface_point(
    u_range: [f64; 2],
    v_range: [f64; 2],
    profile1: &SurfaceProfile,
    profile2: &SurfaceProfile,
    u: f64,
    v: f64,
) -> Result<[f64; 3]> {
    let normalized_u = (u - u_range[0]) / (u_range[1] - u_range[0]);
    let normalized_v = (v - v_range[0]) / (v_range[1] - v_range[0]);
    let profile_parameter = |profile: &SurfaceProfile| {
        let range = match profile {
            SurfaceProfile::HermiteSpline { range, .. }
            | SurfaceProfile::GArc { range, .. }
            | SurfaceProfile::GEllipse { range, .. }
            | SurfaceProfile::GLine { range, .. } => *range,
        };
        range[0] + normalized_u * (range[1] - range[0])
    };
    let a = profile1.evaluate(profile_parameter(profile1))?;
    let b = profile2.evaluate(profile_parameter(profile2))?;
    Ok(std::array::from_fn(|i| {
        a[i] * (1. - normalized_v) + b[i] * normalized_v
    }))
}

fn invert_ruled_surface(
    u_range: [f64; 2],
    v_range: [f64; 2],
    profile1: &SurfaceProfile,
    profile2: &SurfaceProfile,
    target: [f64; 3],
) -> Result<[f64; 2]> {
    let mut best: Option<(f64, [f64; 2])> = None;
    let du = (u_range[1] - u_range[0]) * 1e-5;
    let dv = (v_range[1] - v_range[0]) * 1e-5;
    ensure!(du > 0. && dv > 0., "invalid ruled texture surface range");
    for seed_v in 0..=4 {
        for seed_u in 0..=4 {
            let mut uv = [
                u_range[0] + (u_range[1] - u_range[0]) * seed_u as f64 / 4.,
                v_range[0] + (v_range[1] - v_range[0]) * seed_v as f64 / 4.,
            ];
            for _ in 0..20 {
                let point =
                    ruled_surface_point(u_range, v_range, profile1, profile2, uv[0], uv[1])?;
                let plus_u = ruled_surface_point(
                    u_range,
                    v_range,
                    profile1,
                    profile2,
                    (uv[0] + du).min(u_range[1]),
                    uv[1],
                )?;
                let minus_u = ruled_surface_point(
                    u_range,
                    v_range,
                    profile1,
                    profile2,
                    (uv[0] - du).max(u_range[0]),
                    uv[1],
                )?;
                let plus_v = ruled_surface_point(
                    u_range,
                    v_range,
                    profile1,
                    profile2,
                    uv[0],
                    (uv[1] + dv).min(v_range[1]),
                )?;
                let minus_v = ruled_surface_point(
                    u_range,
                    v_range,
                    profile1,
                    profile2,
                    uv[0],
                    (uv[1] - dv).max(v_range[0]),
                )?;
                let duv = std::array::from_fn(|i| (plus_u[i] - minus_u[i]) / (2. * du));
                let dvv = std::array::from_fn(|i| (plus_v[i] - minus_v[i]) / (2. * dv));
                let residual = std::array::from_fn(|i| point[i] - target[i]);
                let a = dot(duv, duv);
                let b = dot(duv, dvv);
                let c = dot(dvv, dvv);
                let determinant = a * c - b * b;
                if !determinant.is_finite() || determinant.abs() <= 1e-18 {
                    break;
                }
                let ru = dot(duv, residual);
                let rv = dot(dvv, residual);
                let step = [
                    (c * ru - b * rv) / determinant,
                    (a * rv - b * ru) / determinant,
                ];
                uv[0] = (uv[0] - step[0]).clamp(u_range[0], u_range[1]);
                uv[1] = (uv[1] - step[1]).clamp(v_range[0], v_range[1]);
                if step[0].abs().max(step[1].abs()) <= 1e-12 {
                    break;
                }
            }
            let point = ruled_surface_point(u_range, v_range, profile1, profile2, uv[0], uv[1])?;
            let error = point
                .iter()
                .zip(target)
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f64>()
                .sqrt();
            if error.is_finite() && best.is_none_or(|(best_error, _)| error < best_error) {
                best = Some((error, uv));
            }
        }
    }
    let (error, uv) = best.context("texture ruled surface inversion failed")?;
    ensure!(error <= 1e-6, "texture point outside saved ruled surface");
    Ok(uv)
}

fn invert_hermite_surface(
    target: [f64; 3],
    u_range: [f64; 2],
    v_range: [f64; 2],
    u_params: &[f64],
    v_params: &[f64],
    periodic: [bool; 2],
    nodes: &[HermiteSurfaceNode],
) -> Result<[f64; 2]> {
    ensure!(
        !periodic[0] && !periodic[1],
        "periodic texture Hermite surface unsupported"
    );
    ensure!(
        u_params.len() >= 2
            && v_params.len() >= 2
            && nodes.len() == u_params.len() * v_params.len(),
        "texture Hermite surface grid is invalid"
    );
    let mut best: Option<(f64, [f64; 2])> = None;
    for seed_v in 0..=4 {
        for seed_u in 0..=4 {
            let mut uv = [
                u_range[0] + (u_range[1] - u_range[0]) * seed_u as f64 / 4.,
                v_range[0] + (v_range[1] - v_range[0]) * seed_v as f64 / 4.,
            ];
            for _ in 0..20 {
                let point =
                    hermite_surface_point(nodes, u_params, v_params, uv[0], uv[1], false, false)?;
                let du =
                    hermite_surface_point(nodes, u_params, v_params, uv[0], uv[1], true, false)?;
                let dv =
                    hermite_surface_point(nodes, u_params, v_params, uv[0], uv[1], false, true)?;
                let residual = std::array::from_fn(|i| point[i] - target[i]);
                let a = dot(du, du);
                let b = dot(du, dv);
                let c = dot(dv, dv);
                let ru = dot(du, residual);
                let rv = dot(dv, residual);
                let determinant = a * c - b * b;
                if !determinant.is_finite() || determinant.abs() <= 1e-18 {
                    break;
                }
                let step = [
                    (c * ru - b * rv) / determinant,
                    (a * rv - b * ru) / determinant,
                ];
                uv[0] = (uv[0] - step[0]).clamp(u_range[0], u_range[1]);
                uv[1] = (uv[1] - step[1]).clamp(v_range[0], v_range[1]);
                if step[0].abs().max(step[1].abs()) <= 1e-12 {
                    break;
                }
            }
            let point =
                hermite_surface_point(nodes, u_params, v_params, uv[0], uv[1], false, false)?;
            let error = point
                .iter()
                .zip(target)
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f64>()
                .sqrt();
            if error.is_finite() && best.is_none_or(|(best_error, _)| error < best_error) {
                best = Some((error, uv));
            }
        }
    }
    let (error, uv) = best.context("texture Hermite surface inversion failed")?;
    ensure!(error <= 1e-6, "texture point outside saved Hermite surface");
    Ok(uv)
}

fn unwrap_angle_in_range(raw: f64, range: [f64; 2]) -> Result<f64> {
    ensure!(
        range[1] >= range[0] && range[1] - range[0] <= std::f64::consts::TAU + 1e-9,
        "texture profile angle range unqualified"
    );
    let mut t = raw;
    while t < range[0] {
        t += std::f64::consts::TAU;
    }
    while t > range[1] {
        t -= std::f64::consts::TAU;
    }
    ensure!(
        t >= range[0] - 1e-9 && t <= range[1] + 1e-9,
        "texture point outside saved revolution arc"
    );
    Ok(t)
}
fn hermite_segment(
    nodes: &[HermiteProfileNode],
    t: f64,
) -> Result<(&HermiteProfileNode, &HermiteProfileNode, f64)> {
    ensure!(nodes.len() >= 2, "texture profile needs two nodes");
    let i = nodes
        .windows(2)
        .position(|pair| t <= pair[1].parameter)
        .unwrap_or(nodes.len() - 2);
    let a = &nodes[i];
    let b = &nodes[i + 1];
    let delta = b.parameter - a.parameter;
    ensure!(delta > 0., "texture profile parameters are not ordered");
    Ok((a, b, ((t - a.parameter) / delta).clamp(0., 1.)))
}
fn periodic_hermite_value(
    nodes: &[HermiteProfileNode],
    range: [f64; 2],
    t: f64,
) -> Result<([f64; 3], [f64; 3])> {
    let period = range[1] - range[0];
    ensure!(
        period.is_finite() && period > 0.,
        "invalid periodic texture profile range"
    );
    let first = nodes.first().context("periodic texture profile empty")?;
    let last = nodes.last().context("periodic texture profile empty")?;
    ensure!(
        first.parameter >= range[0] - 1e-9
            && last.parameter <= range[1] + 1e-9
            && nodes
                .windows(2)
                .all(|pair| pair[0].parameter < pair[1].parameter),
        "periodic texture profile parameters are not ordered"
    );
    let normalized = range[0] + (t - range[0]).rem_euclid(period);
    if normalized <= last.parameter {
        let (a, b, s) = hermite_segment(nodes, normalized)?;
        return Ok((
            hermite_value(a, b, s, b.parameter - a.parameter, false),
            hermite_value(a, b, s, b.parameter - a.parameter, true),
        ));
    }
    let mut first_wrapped = first.clone();
    first_wrapped.parameter += period;
    let evaluated = if normalized < first.parameter {
        normalized + period
    } else {
        normalized
    };
    let delta = first_wrapped.parameter - last.parameter;
    ensure!(
        delta > 0.,
        "periodic texture profile has empty closing segment"
    );
    let s = ((evaluated - last.parameter) / delta).clamp(0., 1.);
    Ok((
        hermite_value(last, &first_wrapped, s, delta, false),
        hermite_value(last, &first_wrapped, s, delta, true),
    ))
}
fn hermite_value(
    a: &HermiteProfileNode,
    b: &HermiteProfileNode,
    s: f64,
    delta: f64,
    derivative: bool,
) -> [f64; 3] {
    let (h00, h10, h01, h11, tangent_scale) = if derivative {
        (
            (6. * s * s - 6. * s) / delta,
            3. * s * s - 4. * s + 1.,
            (-6. * s * s + 6. * s) / delta,
            3. * s * s - 2. * s,
            1.,
        )
    } else {
        (
            2. * s * s * s - 3. * s * s + 1.,
            s * s * s - 2. * s * s + s,
            -2. * s * s * s + 3. * s * s,
            s * s * s - s * s,
            delta,
        )
    };
    std::array::from_fn(|i| {
        a.point[i] * h00
            + a.tangent[i] * tangent_scale * h10
            + b.point[i] * h01
            + b.tangent[i] * tangent_scale * h11
    })
}
fn unwrap_periodic(value: f64, range: [f64; 2]) -> Result<f64> {
    ensure!(
        range[0].is_finite()
            && range[1].is_finite()
            && range[1] > range[0]
            && range[1] - range[0] <= std::f64::consts::TAU + 1e-7,
        "invalid periodic texture parameter range"
    );
    let mut value = value;
    while value < range[0] - 1e-9 {
        value += std::f64::consts::TAU;
    }
    while value > range[1] + 1e-9 {
        value -= std::f64::consts::TAU;
    }
    ensure!(
        value >= range[0] - 1e-7 && value <= range[1] + 1e-7,
        "texture point crosses saved cylinder seam"
    );
    Ok(value)
}
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Inventory {
    pub mappings: Vec<Mapping>,
    pub saved_fillings: Vec<SavedFilling>,
    pub diagnostics: Vec<Value>,
    pub excluded_graphics_groups: Vec<Value>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SavedFilling {
    pub element_id: u64,
    pub unique_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_object_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_element_id: Option<u64>,
    pub geometry_tag: i64,
    pub material_id: i64,
    pub fill_color: u64,
    pub flags: u64,
    pub pattern_id: i64,
    pub data_pointer_token: u64,
    pub placer_direction: [f64; 2],
    pub placer_origin: [f64; 2],
    pub placer_scale: f64,
    pub placer_uv_scale: [f64; 2],
    pub placer_mirrored: bool,
    pub placed_draft: bool,
    pub source: Value,
}
fn target(g: &ObjectGraph, owner: usize, p: &Value, class: &str) -> Result<usize> {
    let i = target_index(g, owner, p)?;
    ensure!(
        g.objects.get(i).is_some_and(|o| o.class_name == class),
        "texture mapping expected {class}"
    );
    Ok(i)
}
fn target_index(g: &ObjectGraph, owner: usize, p: &Value) -> Result<usize> {
    let edges = g
        .edges
        .iter()
        .filter(|e| {
            e.source_object_index == owner && Some(e.pointer_offset as u64) == p["offset"].as_u64()
        })
        .collect::<Vec<_>>();
    ensure!(edges.len() == 1, "texture mapping pointer ownership");
    Ok(edges[0].target_object_index)
}
fn array<const N: usize>(v: &Value) -> Result<[f64; N]> {
    let a: [f64; N] = v
        .as_array()
        .context("texture mapping vector absent")?
        .iter()
        .map(|v| v.as_f64().context("texture mapping number absent"))
        .collect::<Result<Vec<_>>>()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("texture mapping vector length"))?;
    ensure!(
        a.iter().all(|x| x.is_finite()),
        "texture mapping nonfinite vector"
    );
    Ok(a)
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}

fn texture_surface_profile(
    g: &ObjectGraph,
    owner: usize,
    pointer: &Value,
) -> Result<SurfaceProfile> {
    let profile_index = target_index(g, owner, pointer)?;
    let profile = &g.objects[profile_index];
    let end = profile.fields["m_endParams"]
        .as_array()
        .context("texture profile parameter range")?;
    ensure!(end.len() == 2, "texture profile parameter range length");
    let profile_range = [
        end[0].as_f64().context("texture profile parameter min")?,
        end[1].as_f64().context("texture profile parameter max")?,
    ];
    ensure!(
        profile_range[1] > profile_range[0],
        "texture profile range is invalid"
    );
    match profile.class_name.as_str() {
        "GHermiteSpline" => {
            let periodic = profile.fields["m_Periodic"]
                .as_bool()
                .context("texture profile periodic flag")?;
            let values = profile.fields["m_NodeArray"]
                .as_array()
                .context("texture revolution profile nodes")?;
            ensure!(
                values.len() >= 2,
                "texture revolution profile needs two nodes"
            );
            let nodes = values
                .iter()
                .map(|value| {
                    Ok(HermiteProfileNode {
                        parameter: value["m_iParametr"]
                            .as_f64()
                            .context("texture profile node parameter")?,
                        point: array::<3>(&value["m_iPoint"])?,
                        tangent: array::<3>(&value["m_iTangent"])?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                nodes
                    .windows(2)
                    .all(|pair| pair[0].parameter < pair[1].parameter),
                "texture profile parameters are not ordered"
            );
            Ok(SurfaceProfile::HermiteSpline {
                nodes,
                range: profile_range,
                periodic,
            })
        }
        "GArc" => Ok(SurfaceProfile::GArc {
            center: array::<3>(&profile.fields["m_center"])?,
            radius: profile.fields["m_radius"]
                .as_f64()
                .context("texture profile arc radius")?,
            x_vec: array::<3>(&profile.fields["m_xVec"])?,
            y_vec: array::<3>(&profile.fields["m_yVec"])?,
            range: profile_range,
        }),
        "GEllipse" => Ok(SurfaceProfile::GEllipse {
            center: array::<3>(&profile.fields["m_center"])?,
            x_vec: array::<3>(&profile.fields["m_xVec"])?,
            y_vec: array::<3>(&profile.fields["m_yVec"])?,
            x_len: profile.fields["m_xLen"]
                .as_f64()
                .context("texture profile ellipse x length")?,
            y_len: profile.fields["m_yLen"]
                .as_f64()
                .context("texture profile ellipse y length")?,
            range: profile_range,
        }),
        "GLine" => Ok(SurfaceProfile::GLine {
            origin: array::<3>(&profile.fields["m_origin"])?,
            dir_vec: array::<3>(&profile.fields["m_dirVec"])?,
            range: profile_range,
        }),
        other => bail!("texture revolution profile unsupported {other}"),
    }
}
fn face(r: &Record, g: &ObjectGraph, fi: usize) -> Result<Mapping> {
    let f = &g.objects[fi].fields;
    ensure!(
        f["m_faceRegions"].as_array().is_some_and(Vec::is_empty),
        "texture face regions unqualified"
    );
    ensure!(
        f["m_pGFilling"]["pointer_token"].as_u64() != Some(0),
        "texture face filling absent"
    );
    let bi = target(g, fi, &f["m_pGFilling"], "GFilling")?;
    let b = &g.objects[bi].fields;
    ensure!(
        b["m_pGFace"]["pointer_token"].as_u64() == Some(u64::from(g.objects[fi].token)),
        "texture filling owner mismatch"
    );
    ensure!(
        b["m_flags"].as_i64() == Some(18)
            && b["m_data"]["pointer_token"].as_u64() == Some(0)
            && identifier(&b["m_patternId"])? == -1,
        "texture filling profile unqualified"
    );
    let placer = &b["m_placer"];
    let placer_mirrored = placer["m_isMirrored"]
        .as_bool()
        .context("texture placer mirror absent")?;
    ensure!(
        placer["m_placedDraft"].as_bool() == Some(false)
            && placer["m_scale"].as_f64() == Some(1.)
            && array::<2>(&placer["m_uvScale"]["m_uv"])? == [1., 1.],
        "texture placer draft/scale unqualified"
    );
    let d = array::<2>(&placer["m_dir"])?;
    let offset = array::<2>(&placer["m_origin"])?;
    ensure!(
        d[0] * d[0] + d[1] * d[1] > 1e-20,
        "texture placer direction is zero"
    );
    let pi = target_index(g, fi, &f["m_pSurf"])?;
    let p = &g.objects[pi];
    // `m_dir` is the saved affine UV basis, not merely a unit heading.  Real
    // wall graphics use both very small and very large magnitudes to encode
    // texture density.  Only a zero vector is invalid; the two coefficients
    // must be carried through without normalization.
    let axes = [[d[0], d[1]], [-d[1], d[0]]];
    let parametric_to_uv = axes.map(|a| [a[0], a[1], -a[0] * offset[0] - a[1] * offset[1]]);
    let (origin, normal, world_to_uv, surface, surface_fields) = match p.class_name.as_str() {
        "Plane" => {
            let origin = array::<3>(&p.fields["m_origin"])?;
            let x = array::<3>(&p.fields["m_xVec"])?;
            let y = array::<3>(&p.fields["m_yVec"])?;
            ensure!(
                (dot(x, x) - 1.).abs() < 1e-10
                    && (dot(y, y) - 1.).abs() < 1e-10
                    && dot(x, y).abs() < 1e-10,
                "texture plane basis not orthonormal"
            );
            let world_to_uv = axes.map(|a| {
                let v: [f64; 3] = std::array::from_fn(|i| a[0] * x[i] + a[1] * y[i]);
                [
                    v[0],
                    v[1],
                    v[2],
                    -dot(v, origin) - a[0] * offset[0] - a[1] * offset[1],
                ]
            });
            let normal = [
                x[1] * y[2] - x[2] * y[1],
                x[2] * y[0] - x[0] * y[2],
                x[0] * y[1] - x[1] * y[0],
            ];
            (origin, normal, world_to_uv, None, "Plane")
        }
        "CylSurf" => {
            let center = array::<3>(&p.fields["m_center"])?;
            let radius = p.fields["m_radius"]
                .as_f64()
                .context("texture cylinder radius")?;
            ensure!(
                radius.is_finite() && radius > 0.,
                "invalid texture cylinder radius"
            );
            let x_vec = array::<3>(&p.fields["m_xVec"])?;
            let y_vec = array::<3>(&p.fields["m_yVec"])?;
            let z_vec = array::<3>(&p.fields["m_zVec"])?;
            ensure!(
                [x_vec, y_vec, z_vec]
                    .iter()
                    .all(|v| (dot(*v, *v) - 1.).abs() < 1e-10),
                "texture cylinder basis not unit"
            );
            ensure!(
                dot(x_vec, y_vec).abs() < 1e-10
                    && dot(x_vec, z_vec).abs() < 1e-10
                    && dot(y_vec, z_vec).abs() < 1e-10,
                "texture cylinder basis not orthonormal"
            );
            let corners = p.fields["m_Envelope"]["m_corners"]
                .as_array()
                .context("texture cylinder envelope")?;
            ensure!(corners.len() == 2, "texture cylinder envelope corners");
            let u_range = [
                corners[0][0].as_f64().context("texture cylinder u min")?,
                corners[1][0].as_f64().context("texture cylinder u max")?,
            ];
            let v_range = [
                corners[0][1].as_f64().context("texture cylinder v min")?,
                corners[1][1].as_f64().context("texture cylinder v max")?,
            ];
            let surface = SurfaceMapping::CylSurf {
                center,
                radius,
                x_vec,
                y_vec,
                z_vec,
                u_range,
                v_range,
            };
            (center, z_vec, [[0.; 4]; 2], Some(surface), "CylSurf")
        }
        "SurfRev" => {
            let center = array::<3>(&p.fields["m_center"])?;
            let x_vec = array::<3>(&p.fields["m_xVec"])?;
            let y_vec = array::<3>(&p.fields["m_yVec"])?;
            let z_vec = array::<3>(&p.fields["m_zVec"])?;
            ensure!(
                [x_vec, y_vec, z_vec]
                    .iter()
                    .all(|v| (dot(*v, *v) - 1.).abs() < 1e-10),
                "texture revolution basis not unit"
            );
            ensure!(
                dot(x_vec, y_vec).abs() < 1e-10
                    && dot(x_vec, z_vec).abs() < 1e-10
                    && dot(y_vec, z_vec).abs() < 1e-10,
                "texture revolution basis not orthonormal"
            );
            let corners = p.fields["m_Envelope"]["m_corners"]
                .as_array()
                .context("texture revolution envelope")?;
            ensure!(corners.len() == 2, "texture revolution envelope corners");
            let u_range = [
                corners[0][0].as_f64().context("texture revolution u min")?,
                corners[1][0].as_f64().context("texture revolution u max")?,
            ];
            let v_range = [
                corners[0][1].as_f64().context("texture revolution v min")?,
                corners[1][1].as_f64().context("texture revolution v max")?,
            ];
            let profile_index = target_index(g, pi, &p.fields["m_pProfileCurve"])?;
            let profile_object = &g.objects[profile_index];
            let profile_range = [
                profile_object.fields["m_endParams"][0]
                    .as_f64()
                    .context("texture profile parameter min")?,
                profile_object.fields["m_endParams"][1]
                    .as_f64()
                    .context("texture profile parameter max")?,
            ];
            ensure!(
                profile_range
                    .iter()
                    .zip(v_range)
                    .all(|(profile, envelope)| (profile - envelope).abs() <= 1e-9),
                "texture revolution profile/envelope range mismatch"
            );
            let profile = match profile_object.class_name.as_str() {
                "GHermiteSpline" => {
                    let periodic = profile_object.fields["m_Periodic"]
                        .as_bool()
                        .context("texture profile periodic flag")?;
                    let mut nodes = Vec::new();
                    for value in profile_object.fields["m_NodeArray"]
                        .as_array()
                        .context("texture revolution profile nodes")?
                    {
                        nodes.push(HermiteProfileNode {
                            parameter: value["m_iParametr"]
                                .as_f64()
                                .context("texture profile node parameter")?,
                            point: array::<3>(&value["m_iPoint"])?,
                            tangent: array::<3>(&value["m_iTangent"])?,
                        });
                    }
                    ensure!(
                        nodes.len() >= 2,
                        "texture revolution profile needs two nodes"
                    );
                    SurfaceProfile::HermiteSpline {
                        nodes,
                        range: profile_range,
                        periodic,
                    }
                }
                "GArc" => SurfaceProfile::GArc {
                    center: array::<3>(&profile_object.fields["m_center"])?,
                    radius: profile_object.fields["m_radius"]
                        .as_f64()
                        .context("texture profile arc radius")?,
                    x_vec: array::<3>(&profile_object.fields["m_xVec"])?,
                    y_vec: array::<3>(&profile_object.fields["m_yVec"])?,
                    range: profile_range,
                },
                "GEllipse" => SurfaceProfile::GEllipse {
                    center: array::<3>(&profile_object.fields["m_center"])?,
                    x_vec: array::<3>(&profile_object.fields["m_xVec"])?,
                    y_vec: array::<3>(&profile_object.fields["m_yVec"])?,
                    x_len: profile_object.fields["m_xLen"]
                        .as_f64()
                        .context("texture profile ellipse x length")?,
                    y_len: profile_object.fields["m_yLen"]
                        .as_f64()
                        .context("texture profile ellipse y length")?,
                    range: profile_range,
                },
                "GLine" => SurfaceProfile::GLine {
                    origin: array::<3>(&profile_object.fields["m_origin"])?,
                    dir_vec: array::<3>(&profile_object.fields["m_dirVec"])?,
                    range: profile_range,
                },
                other => bail!("texture revolution profile unsupported {other}"),
            };
            let surface = SurfaceMapping::SurfRev {
                center,
                x_vec,
                y_vec,
                z_vec,
                u_range,
                v_range,
                profile,
            };
            (center, z_vec, [[0.; 4]; 2], Some(surface), "SurfRev")
        }
        "HermiteSurf" => {
            let corners = p.fields["m_Envelope"]["m_corners"]
                .as_array()
                .context("texture Hermite surface envelope")?;
            ensure!(
                corners.len() == 2,
                "texture Hermite surface envelope corners"
            );
            let u_range = [
                corners[0][0].as_f64().context("texture Hermite u min")?,
                corners[1][0].as_f64().context("texture Hermite u max")?,
            ];
            let v_range = [
                corners[0][1].as_f64().context("texture Hermite v min")?,
                corners[1][1].as_f64().context("texture Hermite v max")?,
            ];
            let u_params = p.fields["m_uParams"]
                .as_array()
                .context("texture Hermite u parameters")?
                .iter()
                .map(|v| v.as_f64().context("texture Hermite u parameter"))
                .collect::<Result<Vec<_>>>()?;
            let v_params = p.fields["m_vParams"]
                .as_array()
                .context("texture Hermite v parameters")?
                .iter()
                .map(|v| v.as_f64().context("texture Hermite v parameter"))
                .collect::<Result<Vec<_>>>()?;
            let periodic: [bool; 2] = p.fields["m_periodic"]
                .as_array()
                .context("texture Hermite periodic flags")?
                .iter()
                .map(|v| v.as_bool().context("texture Hermite periodic flag"))
                .collect::<Result<Vec<_>>>()?
                .try_into()
                .map_err(|_| anyhow::anyhow!("texture Hermite periodic flag count"))?;
            ensure!(
                !periodic[0] && !periodic[1],
                "periodic texture Hermite surface unsupported"
            );
            let values = p.fields["m_NodeArray"]
                .as_array()
                .context("texture Hermite surface nodes")?;
            ensure!(
                u_params.len() >= 2
                    && v_params.len() >= 2
                    && u_params.windows(2).all(|w| w[1] > w[0])
                    && v_params.windows(2).all(|w| w[1] > w[0])
                    && values.len() == u_params.len() * v_params.len(),
                "texture Hermite surface grid is invalid"
            );
            let nodes = values
                .iter()
                .map(|value| {
                    let tangent = value["m_iTangent"]
                        .as_array()
                        .context("texture Hermite surface tangents")?;
                    ensure!(tangent.len() == 2, "texture Hermite surface tangent count");
                    Ok(HermiteSurfaceNode {
                        point: array::<3>(&value["m_iPoint"])?,
                        u_tangent: array::<3>(&tangent[0])?,
                        v_tangent: array::<3>(&tangent[1])?,
                        mixed_derivative: array::<3>(&value["m_iMixedDer"])?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                (u_params[0] - u_range[0]).abs() <= 1e-9
                    && (u_params.last().unwrap() - u_range[1]).abs() <= 1e-9
                    && (v_params[0] - v_range[0]).abs() <= 1e-9
                    && (v_params.last().unwrap() - v_range[1]).abs() <= 1e-9,
                "texture Hermite surface grid/envelope mismatch"
            );
            let origin = nodes[0].point;
            let normal = cross(nodes[0].u_tangent, nodes[0].v_tangent);
            let surface = SurfaceMapping::HermiteSurf {
                u_range,
                v_range,
                u_params,
                v_params,
                periodic,
                nodes,
            };
            (origin, normal, [[0.; 4]; 2], Some(surface), "HermiteSurf")
        }
        "RuledSurf" => {
            let corners = p.fields["m_Envelope"]["m_corners"]
                .as_array()
                .context("texture ruled surface envelope")?;
            ensure!(corners.len() == 2, "texture ruled surface envelope corners");
            let u_range = [
                corners[0][0].as_f64().context("texture ruled u min")?,
                corners[1][0].as_f64().context("texture ruled u max")?,
            ];
            let v_range = [
                corners[0][1].as_f64().context("texture ruled v min")?,
                corners[1][1].as_f64().context("texture ruled v max")?,
            ];
            ensure!(
                u_range[1] > u_range[0] && v_range[1] > v_range[0],
                "texture ruled surface envelope is invalid"
            );
            let profile1 = texture_surface_profile(g, pi, &p.fields["m_pProfileCurve1"])?;
            let profile2 = if p.fields["m_pProfileCurve2"]["pointer_token"].as_u64() == Some(0) {
                let origin = array::<3>(&p.fields["m_Point1"])?;
                let end = array::<3>(&p.fields["m_Point2"])?;
                SurfaceProfile::GLine {
                    origin,
                    dir_vec: std::array::from_fn(|i| end[i] - origin[i]),
                    range: u_range,
                }
            } else {
                texture_surface_profile(g, pi, &p.fields["m_pProfileCurve2"])?
            };
            let origin = ruled_surface_point(
                u_range, v_range, &profile1, &profile2, u_range[0], v_range[0],
            )?;
            let u_step = (u_range[1] - u_range[0]) * 1e-6;
            let v_step = (v_range[1] - v_range[0]) * 1e-6;
            let point_u = ruled_surface_point(
                u_range,
                v_range,
                &profile1,
                &profile2,
                u_range[0] + u_step,
                v_range[0],
            )?;
            let point_v = ruled_surface_point(
                u_range,
                v_range,
                &profile1,
                &profile2,
                u_range[0],
                v_range[0] + v_step,
            )?;
            let normal = cross(
                std::array::from_fn(|i| point_u[i] - origin[i]),
                std::array::from_fn(|i| point_v[i] - origin[i]),
            );
            let surface = SurfaceMapping::RuledSurf {
                u_range,
                v_range,
                profile1,
                profile2,
            };
            (origin, normal, [[0.; 4]; 2], Some(surface), "RuledSurf")
        }
        other => {
            bail!(
                "texture mapping expected Plane, CylSurf, SurfRev, HermiteSurf or RuledSurf, got {other}"
            )
        }
    };
    Ok(Mapping {
        element_id: r.identity.element_id,
        unique_id: r.identity.unique_id.clone(),
        instance_object_index: None,
        geometry_tag: f["m_GInfo"]["m_tag"]
            .as_i64()
            .context("texture face tag absent")?,
        material_id: identifier(&f["m_renderStyleId"]).context("texture material absent")?,
        placer_mirrored,
        world_to_uv,
        plane_origin: origin,
        plane_normal: normal,
        parametric_to_uv: surface.as_ref().map(|_| parametric_to_uv),
        surface,
        source: serde_json::json!({"channel":103,"stream":r.source.stream,"body_sha256":r.source.body_sha256,"group_record_offset":r.source.group_record_offset,"face_object":fi,"filling_object":bi,"surface_object":pi,"surface_class":surface_fields,"placer_direction":d,"placer_origin":offset,"placer_mirrored":placer_mirrored,"fields":["Face.m_renderStyleId","GFilling.m_placer","surface.m_Envelope","surface basis"],"semantics":"saved_graphics_surface_parameter_uv_not_regenerated_or_material_transformed"}),
    })
}
fn saved_filling(r: &Record, g: &ObjectGraph, fi: usize) -> Result<SavedFilling> {
    let f = &g.objects[fi].fields;
    let bi = target(g, fi, &f["m_pGFilling"], "GFilling")?;
    let b = &g.objects[bi].fields;
    let placer = &b["m_placer"];
    Ok(SavedFilling {
        element_id: r.identity.element_id,
        unique_id: r.identity.unique_id.clone(),
        instance_object_index: None,
        symbol_element_id: None,
        geometry_tag: f["m_GInfo"]["m_tag"]
            .as_i64()
            .context("saved filling face tag absent")?,
        material_id: identifier(&f["m_renderStyleId"]).context("saved filling material absent")?,
        fill_color: b["m_fillColor"]
            .as_u64()
            .context("saved filling color absent")?,
        flags: b["m_flags"]
            .as_u64()
            .context("saved filling flags absent")?,
        pattern_id: identifier(&b["m_patternId"]).context("saved filling pattern absent")?,
        data_pointer_token: b["m_data"]["pointer_token"]
            .as_u64()
            .context("saved filling data pointer absent")?,
        placer_direction: array::<2>(&placer["m_dir"])?,
        placer_origin: array::<2>(&placer["m_origin"])?,
        placer_scale: placer["m_scale"]
            .as_f64()
            .context("saved filling placer scale absent")?,
        placer_uv_scale: array::<2>(&placer["m_uvScale"]["m_uv"])?,
        placer_mirrored: placer["m_isMirrored"]
            .as_bool()
            .context("saved filling placer mirror absent")?,
        placed_draft: placer["m_placedDraft"]
            .as_bool()
            .context("saved filling placer draft absent")?,
        source: serde_json::json!({
            "channel": 103,
            "stream": r.source.stream,
            "body_sha256": r.source.body_sha256,
            "group_record_offset": r.source.group_record_offset,
            "face_object": fi,
            "filling_object": bi,
            "fields": ["Face.m_renderStyleId", "Face.m_GInfo.m_tag", "GFilling.m_fillColor", "GFilling.m_flags", "GFilling.m_patternId", "GFilling.m_data", "GFilling.m_placer"],
            "semantics": "saved_non-raster_fill_pattern_or_unqualified_filling_metadata"
        }),
    })
}
fn parse(r: &Record) -> Result<Inventory> {
    ensure!(
        r.status == "complete_bounded_graph",
        "texture graphics graph incomplete"
    );
    let g = r.graph.as_ref().context("texture graphics graph absent")?;
    ensure!(
        g.consumed_bytes == r.source.body_bytes,
        "texture graphics EOF mismatch"
    );
    let root = &g.objects.first().context("texture graphics root absent")?;
    let owner_id = root.fields["m_elementId"]
        .as_u64()
        .or_else(|| root.fields["m_GInfo"]["m_tag"].as_u64());
    ensure!(
        root.class_name == "GElement" && owner_id == Some(r.identity.element_id),
        "texture graphics root owner mismatch"
    );
    let mut out = Inventory::default();
    let mut tags = BTreeSet::new();
    for p in root.fields["m_subNodes"]
        .as_array()
        .context("texture graphics subnodes absent")?
    {
        let gi = target_index(g, 0, p)?;
        match g.objects[gi].class_name.as_str() {
            "GGroup" => {
                out.excluded_graphics_groups.push(serde_json::json!({"element_id":r.identity.element_id,"object_index":gi,"reason":"auxiliary grouped graphics; mapping only direct body Geometry"}));
                continue;
            }
            "Geometry" => {}
            class_name => anyhow::bail!("texture mapping expected Geometry, got {class_name}"),
        }
        for p in g.objects[gi].fields["m_pFaces"]
            .as_array()
            .context("texture geometry faces absent")?
        {
            let fi = target(g, gi, p, "Face")?;
            let tag = g.objects[fi].fields["m_GInfo"]["m_tag"].clone();
            match face(r, g, fi) {
                Ok(m) => {
                    ensure!(tags.insert(m.geometry_tag), "duplicate texture face tag");
                    out.mappings.push(m)
                }
                Err(e) => {
                    if let Ok(filling) = saved_filling(r, g, fi) {
                        out.saved_fillings.push(filling);
                    }
                    out.diagnostics.push(serde_json::json!({"element_id":r.identity.element_id,"geometry_tag":tag,"reason":format!("{e:#}")}))
                }
            }
        }
    }
    Ok(out)
}

fn parse_selected_geometries(r: &Record) -> Result<Inventory> {
    let g = r.graph.as_ref().context("texture graphics graph absent")?;
    let selection = crate::native_graphics_traversal::select_graphics(g);
    ensure!(
        !selection.selected.is_empty(),
        "nested texture graph has no selected surface geometry"
    );
    let mut out = Inventory::default();
    let mut tags = BTreeSet::new();
    for diagnostic in selection.diagnostics {
        out.diagnostics.push(serde_json::json!({
            "element_id": r.identity.element_id,
            "object_index": diagnostic.object_index,
            "reason": diagnostic.message
        }));
    }
    for selected in selection.selected {
        let geometry = &g.objects[selected.object_index];
        ensure!(
            geometry.class_name == "Geometry",
            "nested texture selected non-face geometry {}",
            geometry.class_name
        );
        for pointer in geometry.fields["m_pFaces"]
            .as_array()
            .context("nested texture geometry faces absent")?
        {
            let face_index = target(g, selected.object_index, pointer, "Face")?;
            match face(r, g, face_index) {
                Ok(mut mapping) => {
                    ensure!(
                        tags.insert(mapping.geometry_tag),
                        "duplicate nested texture face tag"
                    );
                    if let Some(source) = mapping.source.as_object_mut() {
                        source.insert(
                            "geometry_object_index".into(),
                            serde_json::json!(selected.object_index),
                        );
                    }
                    out.mappings.push(mapping);
                }
                Err(error) => {
                    if let Ok(filling) = saved_filling(r, g, face_index) {
                        out.saved_fillings.push(filling);
                    }
                    out.diagnostics.push(serde_json::json!({
                        "element_id": r.identity.element_id,
                        "geometry_tag": g.objects[face_index].fields["m_GInfo"]["m_tag"],
                        "geometry_object_index": selected.object_index,
                        "reason": format!("{error:#}")
                    }))
                }
            }
        }
    }
    Ok(out)
}

fn parse_graphics_record(r: &Record) -> Result<Inventory> {
    match parse(r) {
        Ok(value) => Ok(value),
        Err(direct_error) => parse_selected_geometries(r).map_err(|nested_error| {
            anyhow::anyhow!(
                "direct texture graph: {direct_error:#}; nested graph: {nested_error:#}"
            )
        }),
    }
}

fn has_texture_candidate(r: &Record) -> bool {
    r.graph.as_ref().is_some_and(|g| {
        g.objects
            .iter()
            .any(|object| matches!(object.class_name.as_str(), "Geometry" | "GInstance"))
    })
}

fn instance_ref(g: &ObjectGraph, instance: usize) -> Result<(u64, GraphicsTransform)> {
    let info = target(
        g,
        instance,
        &g.objects[instance].fields["m_instanceInfo"],
        "InstanceInfo",
    )?;
    let symbol_id = u64::try_from(identifier(&g.objects[info].fields["m_symbolId"])?)
        .context("texture symbol id is not positive")?;
    let trf = &g.objects[info].fields["m_Trf"];
    let rotation = trf["m_3x3"]
        .as_array()
        .context("texture instance rotation absent")?;
    ensure!(rotation.len() == 3, "texture instance rotation rows");
    let mut matrix = [[0.; 4]; 4];
    for (row, value) in rotation.iter().enumerate() {
        let values = value.as_array().context("texture instance rotation row")?;
        ensure!(values.len() == 3, "texture instance rotation columns");
        for (column, value) in values.iter().enumerate() {
            matrix[row][column] = value.as_f64().context("texture instance rotation number")?;
        }
    }
    let origin = array::<3>(&trf["m_or"])?;
    for axis in 0..3 {
        ensure!(
            matrix[axis].iter().take(3).all(|v| v.is_finite()),
            "texture instance rotation nonfinite"
        );
        matrix[axis][3] = origin[axis];
    }
    matrix[3][3] = 1.;
    Ok((symbol_id, matrix))
}

fn collect_symbol_refs_at(
    g: &ObjectGraph,
    index: usize,
    seen: &mut BTreeSet<usize>,
    refs: &mut Vec<(usize, u64, GraphicsTransform)>,
) -> Result<()> {
    if !seen.insert(index) {
        return Ok(());
    }
    let object = g.objects.get(index).context("texture graph object index")?;
    match object.class_name.as_str() {
        "GElement" | "GGroup" | "GFilter" => {
            for pointer in object.fields["m_subNodes"]
                .as_array()
                .context("texture graphics children absent")?
            {
                collect_symbol_refs_at(g, target_index(g, index, pointer)?, seen, refs)?;
            }
        }
        "GInstance" => {
            let (symbol_id, transform) = instance_ref(g, index)?;
            refs.push((index, symbol_id, transform));
            let embedded = &object.fields["m_oEmbeddedSymbolGRep"];
            if embedded["pointer_token"].as_u64() != Some(0) {
                collect_symbol_refs_at(g, target_index(g, index, embedded)?, seen, refs)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn symbol_refs(r: &Record) -> Result<Vec<(usize, u64, GraphicsTransform)>> {
    let g = r.graph.as_ref().context("texture graphics graph absent")?;
    let root = g.objects.first().context("texture graphics root absent")?;
    ensure!(root.class_name == "GElement", "texture graphics root class");
    let mut refs = direct_symbol_refs(r)?;
    let mut recursive_refs = Vec::new();
    collect_symbol_refs_at(g, 0, &mut BTreeSet::new(), &mut recursive_refs)?;
    let mut seen_instances = refs
        .iter()
        .map(|(index, _, _)| *index)
        .collect::<BTreeSet<_>>();
    for reference in recursive_refs {
        if seen_instances.insert(reference.0) {
            refs.push(reference);
        }
    }
    Ok(refs)
}

fn direct_symbol_refs(r: &Record) -> Result<Vec<(usize, u64, [[f64; 4]; 4])>> {
    let g = r.graph.as_ref().context("texture graphics graph absent")?;
    let root = g.objects.first().context("texture graphics root absent")?;
    ensure!(root.class_name == "GElement", "texture graphics root class");
    let mut refs = Vec::new();
    for pointer in root.fields["m_subNodes"]
        .as_array()
        .context("texture graphics subnodes absent")?
    {
        let instance = target_index(g, 0, pointer)?;
        if g.objects
            .get(instance)
            .is_none_or(|object| object.class_name != "GInstance")
        {
            continue;
        }
        let info = target(
            g,
            instance,
            &g.objects[instance].fields["m_instanceInfo"],
            "InstanceInfo",
        )?;
        let symbol_id = u64::try_from(identifier(&g.objects[info].fields["m_symbolId"])?)
            .context("texture symbol id is not positive")?;
        let trf = &g.objects[info].fields["m_Trf"];
        let rotation = trf["m_3x3"]
            .as_array()
            .context("texture instance rotation absent")?;
        ensure!(rotation.len() == 3, "texture instance rotation rows");
        let mut matrix = [[0.; 4]; 4];
        for (row, value) in rotation.iter().enumerate() {
            let values = value.as_array().context("texture instance rotation row")?;
            ensure!(values.len() == 3, "texture instance rotation columns");
            for (column, value) in values.iter().enumerate() {
                matrix[row][column] = value.as_f64().context("texture instance rotation number")?;
            }
        }
        let origin = array::<3>(&trf["m_or"])?;
        for axis in 0..3 {
            ensure!(
                matrix[axis].iter().take(3).all(|v| v.is_finite()),
                "texture instance rotation nonfinite"
            );
            matrix[axis][3] = origin[axis];
        }
        matrix[3][3] = 1.;
        refs.push((instance, symbol_id, matrix));
    }
    Ok(refs)
}

fn map_symbol_to_instance(
    mapping: &mut Mapping,
    instance_id: u64,
    instance: usize,
    trf: [[f64; 4]; 4],
    owner: &Record,
) -> Result<()> {
    let mut local_to_world = [[0.; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            local_to_world[row][column] = trf[row][column];
        }
    }
    let local_origin = mapping.plane_origin;
    let world_origin = std::array::from_fn(|row| {
        trf[row][3]
            + (0..3)
                .map(|column| local_to_world[row][column] * local_origin[column])
                .sum::<f64>()
    });
    let local_normal = mapping.plane_normal;
    let world_normal = std::array::from_fn(|row| {
        (0..3)
            .map(|column| local_to_world[row][column] * local_normal[column])
            .sum::<f64>()
    });
    let mut world_to_uv = [[0.; 4]; 2];
    for row in 0..2 {
        for world_axis in 0..3 {
            world_to_uv[row][world_axis] = (0..3)
                .map(|local_axis| {
                    mapping.world_to_uv[row][local_axis] * local_to_world[local_axis][world_axis]
                })
                .sum();
        }
        world_to_uv[row][3] = mapping.world_to_uv[row][3]
            - (0..3)
                .map(|axis| world_to_uv[row][axis] * trf[axis][3])
                .sum::<f64>();
    }
    mapping.element_id = owner.identity.element_id;
    mapping.unique_id = owner.identity.unique_id.clone();
    mapping.instance_object_index = Some(instance);
    mapping.plane_origin = world_origin;
    let normal_length = world_normal.iter().map(|x| x * x).sum::<f64>().sqrt();
    ensure!(
        normal_length.is_finite() && normal_length > 1e-12,
        "invalid transformed texture plane normal"
    );
    mapping.plane_normal = world_normal.map(|x| x / normal_length);
    mapping.world_to_uv = world_to_uv;
    if let Some(surface) = mapping.surface.as_mut() {
        surface.transform(trf)?;
    }
    if let Some(source) = mapping.source.as_object_mut() {
        source.insert("symbol_element_id".into(), serde_json::json!(instance_id));
        source.insert("instance_object_index".into(), serde_json::json!(instance));
        source.insert("instance_transform".into(), serde_json::json!(trf));
        source.insert(
            "semantics".into(),
            serde_json::json!("saved symbol UV composed through the owning instance transform"),
        );
    }
    Ok(())
}

fn instance_on_path(g: &ObjectGraph, path: &[usize]) -> Option<usize> {
    path.iter().copied().find(|index| {
        g.objects
            .get(*index)
            .is_some_and(|object| object.class_name == "GInstance")
    })
}

fn map_saved_filling_to_instance(
    filling: &mut SavedFilling,
    symbol_id: u64,
    instance: usize,
    trf: GraphicsTransform,
    owner: &Record,
) {
    filling.element_id = owner.identity.element_id;
    filling.unique_id = owner.identity.unique_id.clone();
    filling.instance_object_index = Some(instance);
    filling.symbol_element_id = Some(symbol_id);
    if let Some(source) = filling.source.as_object_mut() {
        source.insert("symbol_element_id".into(), serde_json::json!(symbol_id));
        source.insert("instance_object_index".into(), serde_json::json!(instance));
        source.insert("instance_transform".into(), serde_json::json!(trf));
        source.insert(
            "semantics".into(),
            serde_json::json!(
                "saved symbol fill-pattern metadata retained through the owning instance"
            ),
        );
    }
}

fn parse_with_symbols(r: &Record, symbols: &BTreeMap<u64, Record>) -> Result<Inventory> {
    let graph = r.graph.as_ref().context("texture graphics graph absent")?;
    let resolver = |id: u64| symbols.get(&id).and_then(|record| record.graph.as_ref());
    let selection =
        native_graphics_traversal::select_graphics_with_resolver_at_detail(graph, &resolver, 3);
    let selection_diagnostic_text = selection
        .diagnostics
        .iter()
        .map(|diagnostic| format!("object {}: {}", diagnostic.object_index, diagnostic.message))
        .collect::<Vec<_>>()
        .join("; ");
    let mut out = Inventory::default();
    let selection_diagnostics = selection.diagnostics;
    let selection_empty = selection.selected.is_empty();
    for diagnostic in selection_diagnostics {
        out.diagnostics.push(serde_json::json!({
            "element_id": r.identity.element_id,
            "object_index": diagnostic.object_index,
            "reason": diagnostic.message
        }));
    }
    if selection.rejected_filters > 0 {
        out.diagnostics.push(serde_json::json!({
            "element_id": r.identity.element_id,
            "reason": format!("{} texture filter branches rejected by saved conditions", selection.rejected_filters)
        }));
    }
    for observation in selection.profile_observations {
        out.diagnostics.push(serde_json::json!({
            "element_id": r.identity.element_id,
            "reason": observation
        }));
    }
    ensure!(
        !selection_empty,
        "nested texture graph has no selected surface geometry{}",
        if selection_diagnostic_text.is_empty() {
            String::new()
        } else {
            format!("; {selection_diagnostic_text}")
        }
    );
    for selected in selection.selected {
        let (source_record, source_graph) = match selected.source_owner_id {
            Some(owner_id) => {
                let record = symbols
                    .get(&owner_id)
                    .with_context(|| format!("current symbol graphics {owner_id} absent"))?;
                (
                    record,
                    record
                        .graph
                        .as_ref()
                        .context("symbol graphics graph absent")?,
                )
            }
            None => (r, graph),
        };
        let geometry = source_graph
            .objects
            .get(selected.object_index)
            .context("selected texture geometry object absent")?;
        ensure!(
            geometry.class_name == "Geometry",
            "nested texture selected non-face geometry {}",
            geometry.class_name
        );
        let instance = instance_on_path(graph, &selected.path);
        let needs_transform =
            selected
                .world_transform
                .iter()
                .flatten()
                .enumerate()
                .any(|(i, value)| {
                    let row = i / 4;
                    let column = i % 4;
                    *value != if row == column { 1. } else { 0. }
                });
        if needs_transform {
            ensure!(
                instance.is_some(),
                "texture instance transform has no owner"
            );
        }
        for pointer in geometry.fields["m_pFaces"]
            .as_array()
            .context("nested texture geometry faces absent")?
        {
            let face_index = target(source_graph, selected.object_index, pointer, "Face")?;
            match face(source_record, source_graph, face_index) {
                Ok(mut mapping) => {
                    if needs_transform {
                        let symbol_id = selected.source_owner_id.unwrap_or(r.identity.element_id);
                        map_symbol_to_instance(
                            &mut mapping,
                            symbol_id,
                            instance.expect("checked texture instance owner"),
                            selected.world_transform,
                            r,
                        )?;
                    }
                    if let Some(source) = mapping.source.as_object_mut() {
                        source.insert(
                            "geometry_object_index".into(),
                            serde_json::json!(selected.object_index),
                        );
                        source.insert("path".into(), serde_json::json!(selected.path));
                    }
                    out.mappings.push(mapping);
                }
                Err(error) => {
                    if let Ok(mut filling) = saved_filling(source_record, source_graph, face_index)
                    {
                        if needs_transform {
                            map_saved_filling_to_instance(
                                &mut filling,
                                selected.source_owner_id.unwrap_or(r.identity.element_id),
                                instance.expect("checked texture instance owner"),
                                selected.world_transform,
                                r,
                            );
                        }
                        out.saved_fillings.push(filling);
                    }
                    out.diagnostics.push(serde_json::json!({
                        "element_id": r.identity.element_id,
                        "geometry_tag": source_graph.objects[face_index].fields["m_GInfo"]["m_tag"],
                        "geometry_object_index": selected.object_index,
                        "reason": format!("{error:#}")
                    }));
                }
            }
        }
    }
    Ok(out)
}

#[allow(dead_code)]
fn parse_with_symbols_legacy(r: &Record, symbols: &BTreeMap<u64, Record>) -> Result<Inventory> {
    let refs = direct_symbol_refs(r)?;
    ensure!(!refs.is_empty(), "no direct external symbol graphics");
    let mut out = Inventory::default();
    for (instance, symbol_id, trf) in refs {
        let symbol = symbols
            .get(&symbol_id)
            .with_context(|| format!("current symbol graphics {symbol_id} absent"))?;
        let inventory = parse_graphics_record(symbol)?;
        for mut mapping in inventory.mappings {
            map_symbol_to_instance(&mut mapping, symbol_id, instance, trf, r)?;
            out.mappings.push(mapping);
        }
        for mut filling in inventory.saved_fillings {
            filling.element_id = r.identity.element_id;
            filling.unique_id = r.identity.unique_id.clone();
            filling.instance_object_index = Some(instance);
            filling.symbol_element_id = Some(symbol_id);
            if let Some(source) = filling.source.as_object_mut() {
                source.insert("symbol_element_id".into(), serde_json::json!(symbol_id));
                source.insert("instance_object_index".into(), serde_json::json!(instance));
                source.insert("instance_transform".into(), serde_json::json!(trf));
                source.insert(
                    "semantics".into(),
                    serde_json::json!(
                        "saved symbol fill-pattern metadata retained through the owning instance"
                    ),
                );
            }
            out.saved_fillings.push(filling);
        }
        out.diagnostics.extend(inventory.diagnostics);
        out.excluded_graphics_groups
            .extend(inventory.excluded_graphics_groups);
    }
    let mut identities = BTreeSet::new();
    for mapping in &out.mappings {
        ensure!(
            identities.insert((mapping.instance_object_index, mapping.geometry_tag)),
            "duplicate texture instance face tag"
        );
    }
    Ok(out)
}
fn extract_graph_records(
    file: &mut RevitFile,
    options: &native_document::Options,
    physical_index: Option<&native_document::PhysicalIndex>,
    emit: impl FnMut(Record) -> Result<()>,
) -> Result<()> {
    if let Some(physical_index) = physical_index {
        native_document::extract_without_definitions_using_index(
            file,
            options,
            physical_index,
            emit,
        )?;
    } else {
        native_document::extract_without_definitions(file, options, emit)?;
    }
    Ok(())
}

/// Selected current saved graphics only; unsupported owners/faces are explicit.
pub fn read(file: &mut RevitFile, ids: BTreeSet<u64>) -> Result<Inventory> {
    read_impl(file, ids, None)
}

/// Selected current saved graphics using a caller-owned physical index. This
/// keeps texture closure extraction bounded when it runs alongside delivery
/// metadata and geometry projection.
pub fn read_using_index(
    file: &mut RevitFile,
    ids: BTreeSet<u64>,
    physical_index: &native_document::PhysicalIndex,
) -> Result<Inventory> {
    read_impl(file, ids, Some(physical_index))
}

fn read_impl(
    file: &mut RevitFile,
    ids: BTreeSet<u64>,
    physical_index: Option<&native_document::PhysicalIndex>,
) -> Result<Inventory> {
    let mut result = Inventory::default();
    if ids.is_empty() {
        return Ok(result);
    }
    ensure!(
        matches!(file.basic_file_info()?.version, 2023 | 2024 | 2027),
        "texture graphics version outside qualified native profile"
    );
    let mut seen = BTreeSet::new();
    let mut roots = BTreeMap::<u64, Record>::new();
    extract_graph_records(
        file,
        &native_document::Options {
            selected_ids: ids.clone(),
            channels: BTreeSet::from([103]),
            ..Default::default()
        },
        physical_index,
        |r| {
            seen.insert(r.identity.element_id);
            roots.insert(r.identity.element_id, r);
            Ok(())
        },
    )?;
    let mut symbol_ids = BTreeSet::new();
    for record in roots.values() {
        if let Ok(refs) = symbol_refs(record) {
            symbol_ids.extend(refs.into_iter().map(|(_, id, _)| id));
        }
    }
    let mut symbols = BTreeMap::<u64, Record>::new();
    let mut loaded_symbol_ids = BTreeSet::new();
    while !symbol_ids.is_empty() {
        let batch = symbol_ids
            .difference(&loaded_symbol_ids)
            .copied()
            .collect::<BTreeSet<_>>();
        if batch.is_empty() {
            break;
        }
        extract_graph_records(
            file,
            &native_document::Options {
                selected_ids: batch.clone(),
                channels: BTreeSet::from([103]),
                ..Default::default()
            },
            physical_index,
            |r| {
                symbols.insert(r.identity.element_id, r);
                Ok(())
            },
        )?;
        loaded_symbol_ids.extend(batch);
        for record in symbols.values() {
            if let Ok(refs) = symbol_refs(record) {
                symbol_ids.extend(refs.into_iter().map(|(_, id, _)| id));
            }
        }
    }
    for record in roots.values() {
        if !has_texture_candidate(record) {
            continue;
        }
        let inventory = match parse_graphics_record(record) {
            Ok(value) => Ok(value),
            Err(direct_error) if symbol_refs(record).is_ok() => {
                parse_with_symbols(record, &symbols).map_err(|error| {
                    anyhow::anyhow!(
                        "direct texture graph: {direct_error:#}; symbol graph: {error:#}"
                    )
                })
            }
            Err(error) => Err(error),
        };
        match inventory {
            Ok(mut value) => {
                result.mappings.append(&mut value.mappings);
                result.saved_fillings.append(&mut value.saved_fillings);
                result.diagnostics.append(&mut value.diagnostics);
                result
                    .excluded_graphics_groups
                    .append(&mut value.excluded_graphics_groups);
            }
            Err(error) => result.diagnostics.push(serde_json::json!({
                "element_id": record.identity.element_id,
                "reason": format!("{error:#}")
            })),
        }
    }
    for id in ids.difference(&seen) {
        result
            .diagnostics
            .push(serde_json::json!({"element_id":id,"reason":"current graphics channel absent"}))
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn affine_mapping_refuses_off_plane_and_nonfinite_points() {
        let m = Mapping {
            element_id: 1,
            unique_id: "test".into(),
            instance_object_index: None,
            geometry_tag: 2,
            material_id: 3,
            placer_mirrored: false,
            world_to_uv: [[0., 1., 0., -8.], [-1., 0., 0., 4.]],
            plane_origin: [0., 0., 7.],
            plane_normal: [0., 0., 1.],
            parametric_to_uv: None,
            surface: None,
            source: Value::Null,
        };
        assert_eq!(m.evaluate([2., 9., 7.]).unwrap(), [1., 2.]);
        assert!(m.evaluate([2., 9., 7.01]).is_err());
        assert!(m.evaluate([f64::NAN, 9., 7.]).is_err());
    }

    #[test]
    fn affine_mapping_preserves_non_unit_saved_placer_density() {
        let m = Mapping {
            element_id: 1,
            unique_id: "test".into(),
            instance_object_index: None,
            geometry_tag: 2,
            material_id: 3,
            placer_mirrored: false,
            world_to_uv: [[2., 0., 0., 0.], [0., 2., 0., 0.]],
            plane_origin: [0., 0., 0.],
            plane_normal: [0., 0., 1.],
            parametric_to_uv: None,
            surface: None,
            source: Value::Null,
        };
        assert_eq!(m.evaluate([1., 0.5, 0.]).unwrap(), [2., 1.]);
    }

    #[test]
    fn cylindrical_mapping_evaluates_saved_surface_parameters() {
        let m = Mapping {
            element_id: 1,
            unique_id: "test".into(),
            instance_object_index: None,
            geometry_tag: 2,
            material_id: 3,
            placer_mirrored: false,
            world_to_uv: [[0.; 4]; 2],
            plane_origin: [0.; 3],
            plane_normal: [0., 0., 1.],
            parametric_to_uv: Some([[1., 0., 0.], [0., 1., 0.]]),
            surface: Some(SurfaceMapping::CylSurf {
                center: [0.; 3],
                radius: 2.,
                x_vec: [1., 0., 0.],
                y_vec: [0., 1., 0.],
                z_vec: [0., 0., 1.],
                u_range: [0., std::f64::consts::TAU],
                v_range: [0., 3.],
            }),
            source: Value::Null,
        };
        let uv = m.evaluate([0., 2., 1.]).unwrap();
        assert!((uv[0] - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert_eq!(uv[1], 1.);
        assert!(m.evaluate([0., 1., 1.]).is_err());
    }

    #[test]
    fn revolved_hermite_mapping_inverts_saved_profile() {
        let m = Mapping {
            element_id: 1,
            unique_id: "test".into(),
            instance_object_index: None,
            geometry_tag: 2,
            material_id: 3,
            placer_mirrored: false,
            world_to_uv: [[0.; 4]; 2],
            plane_origin: [0.; 3],
            plane_normal: [0., 0., 1.],
            parametric_to_uv: Some([[1., 0., 0.], [0., 1., 0.]]),
            surface: Some(SurfaceMapping::SurfRev {
                center: [0.; 3],
                x_vec: [1., 0., 0.],
                y_vec: [0., 1., 0.],
                z_vec: [0., 0., 1.],
                u_range: [0., std::f64::consts::TAU],
                v_range: [0., 1.],
                profile: SurfaceProfile::HermiteSpline {
                    nodes: vec![
                        HermiteProfileNode {
                            parameter: 0.,
                            point: [2., 0., 0.],
                            tangent: [0., 0., 1.],
                        },
                        HermiteProfileNode {
                            parameter: 1.,
                            point: [2., 0., 1.],
                            tangent: [0., 0., 1.],
                        },
                    ],
                    range: [0., 1.],
                    periodic: false,
                },
            }),
            source: Value::Null,
        };
        let uv = m.evaluate([0., 2., 0.25]).unwrap();
        assert!((uv[0] - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!((uv[1] - 0.25).abs() < 1e-12);
    }

    #[test]
    fn revolved_analytic_profiles_invert_saved_arc_and_line() {
        let mut arc = Mapping {
            element_id: 1,
            unique_id: "arc".into(),
            instance_object_index: None,
            geometry_tag: 2,
            material_id: 3,
            placer_mirrored: false,
            world_to_uv: [[0.; 4]; 2],
            plane_origin: [0.; 3],
            plane_normal: [0., 0., 1.],
            parametric_to_uv: Some([[1., 0., 0.], [0., 1., 0.]]),
            surface: Some(SurfaceMapping::SurfRev {
                center: [0.; 3],
                x_vec: [1., 0., 0.],
                y_vec: [0., 1., 0.],
                z_vec: [0., 0., 1.],
                u_range: [0., std::f64::consts::TAU],
                v_range: [0., std::f64::consts::FRAC_PI_2],
                profile: SurfaceProfile::GArc {
                    center: [1., 0., 1.],
                    radius: 1.,
                    x_vec: [1., 0., 0.],
                    y_vec: [0., 0., 1.],
                    range: [0., std::f64::consts::FRAC_PI_2],
                },
            }),
            source: Value::Null,
        };
        let uv = arc.evaluate([0., 2., 1.]).unwrap();
        assert!((uv[0] - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!(uv[1].abs() < 1e-12);

        arc.surface = Some(SurfaceMapping::SurfRev {
            center: [0.; 3],
            x_vec: [1., 0., 0.],
            y_vec: [0., 1., 0.],
            z_vec: [0., 0., 1.],
            u_range: [0., std::f64::consts::TAU],
            v_range: [0., 1.],
            profile: SurfaceProfile::GLine {
                origin: [1., 0., 0.],
                dir_vec: [2., 0., 2.],
                range: [0., 1.],
            },
        });
        let uv = arc.evaluate([0., 2., 1.]).unwrap();
        assert!((uv[0] - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!((uv[1] - 0.5).abs() < 1e-12);
    }

    #[test]
    fn periodic_revolved_hermite_profile_wraps_closing_segment() {
        let m = Mapping {
            element_id: 1,
            unique_id: "periodic".into(),
            instance_object_index: None,
            geometry_tag: 2,
            material_id: 3,
            placer_mirrored: false,
            world_to_uv: [[0.; 4]; 2],
            plane_origin: [0.; 3],
            plane_normal: [0., 0., 1.],
            parametric_to_uv: Some([[1., 0., 0.], [0., 1., 0.]]),
            surface: Some(SurfaceMapping::SurfRev {
                center: [0.; 3],
                x_vec: [1., 0., 0.],
                y_vec: [0., 1., 0.],
                z_vec: [0., 0., 1.],
                u_range: [0., std::f64::consts::TAU],
                v_range: [0., std::f64::consts::TAU],
                profile: SurfaceProfile::HermiteSpline {
                    nodes: vec![
                        HermiteProfileNode {
                            parameter: 0.,
                            point: [3., 0., 1.],
                            tangent: [0., 0., 1.],
                        },
                        HermiteProfileNode {
                            parameter: std::f64::consts::FRAC_PI_2,
                            point: [2., 0., 2.],
                            tangent: [-1., 0., 0.],
                        },
                        HermiteProfileNode {
                            parameter: std::f64::consts::PI,
                            point: [1., 0., 1.],
                            tangent: [0., 0., -1.],
                        },
                        HermiteProfileNode {
                            parameter: 3. * std::f64::consts::FRAC_PI_2,
                            point: [2., 0., 0.],
                            tangent: [1., 0., 0.],
                        },
                    ],
                    range: [0., std::f64::consts::TAU],
                    periodic: true,
                },
            }),
            source: Value::Null,
        };
        let uv = m.evaluate([0., 2., 2.]).unwrap();
        assert!((uv[0] - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!((uv[1] - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        let uv = m.evaluate([0., 3., 1.]).unwrap();
        assert!(uv[1].abs() < 1e-9 || (uv[1] - std::f64::consts::TAU).abs() < 1e-9);
    }

    #[test]
    fn hermite_surface_mapping_inverts_saved_bicubic_grid() {
        let node = |point, u_tangent, v_tangent| HermiteSurfaceNode {
            point,
            u_tangent,
            v_tangent,
            mixed_derivative: [0., 0., 0.],
        };
        let m = Mapping {
            element_id: 1,
            unique_id: "hermite-surface".into(),
            instance_object_index: None,
            geometry_tag: 2,
            material_id: 3,
            placer_mirrored: false,
            world_to_uv: [[0.; 4]; 2],
            plane_origin: [0.; 3],
            plane_normal: [0., 0., 1.],
            parametric_to_uv: Some([[1., 0., 0.], [0., 1., 0.]]),
            surface: Some(SurfaceMapping::HermiteSurf {
                u_range: [0., 1.],
                v_range: [0., 1.],
                u_params: vec![0., 1.],
                v_params: vec![0., 1.],
                periodic: [false, false],
                nodes: vec![
                    node([0., 0., 0.], [1., 0., 0.], [0., 1., 0.]),
                    node([1., 0., 0.], [1., 0., 0.], [0., 1., 0.]),
                    node([0., 1., 0.], [1., 0., 0.], [0., 1., 0.]),
                    node([1., 1., 0.], [1., 0., 0.], [0., 1., 0.]),
                ],
            }),
            source: Value::Null,
        };
        let uv = m.evaluate([0.25, 0.75, 0.]).unwrap();
        assert!((uv[0] - 0.25).abs() < 1e-10);
        assert!((uv[1] - 0.75).abs() < 1e-10);
    }

    #[test]
    fn revolved_ellipse_profile_inverts_saved_axes_and_lengths() {
        let m = Mapping {
            element_id: 1,
            unique_id: "ellipse".into(),
            instance_object_index: None,
            geometry_tag: 2,
            material_id: 3,
            placer_mirrored: false,
            world_to_uv: [[0.; 4]; 2],
            plane_origin: [0.; 3],
            plane_normal: [0., 0., 1.],
            parametric_to_uv: Some([[1., 0., 0.], [0., 1., 0.]]),
            surface: Some(SurfaceMapping::SurfRev {
                center: [0.; 3],
                x_vec: [1., 0., 0.],
                y_vec: [0., 1., 0.],
                z_vec: [0., 0., 1.],
                u_range: [0., std::f64::consts::TAU],
                v_range: [0., std::f64::consts::PI],
                profile: SurfaceProfile::GEllipse {
                    center: [2., 0., 0.],
                    x_vec: [1., 0., 0.],
                    y_vec: [0., 0., 1.],
                    x_len: 2.,
                    y_len: 1.,
                    range: [0., std::f64::consts::PI],
                },
            }),
            source: Value::Null,
        };
        let uv = m.evaluate([0., 2., 1.]).unwrap();
        assert!((uv[0] - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!((uv[1] - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
    }

    #[test]
    fn ruled_surface_mapping_inverts_saved_profile_and_point_endpoints() {
        let profile1 = SurfaceProfile::GArc {
            center: [0., 0., 0.],
            radius: 1.,
            x_vec: [1., 0., 0.],
            y_vec: [0., 1., 0.],
            range: [0., std::f64::consts::FRAC_PI_2],
        };
        let profile2 = SurfaceProfile::GLine {
            origin: [0., 0., 1.],
            dir_vec: [1., 0., 0.],
            range: [0., 1.],
        };
        let point =
            ruled_surface_point([0., 1.], [0., 1.], &profile1, &profile2, 0.5, 0.25).unwrap();
        let m = Mapping {
            element_id: 1,
            unique_id: "ruled".into(),
            instance_object_index: None,
            geometry_tag: 2,
            material_id: 3,
            placer_mirrored: false,
            world_to_uv: [[0.; 4]; 2],
            plane_origin: [0.; 3],
            plane_normal: [0., 0., 1.],
            parametric_to_uv: Some([[1., 0., 0.], [0., 1., 0.]]),
            surface: Some(SurfaceMapping::RuledSurf {
                u_range: [0., 1.],
                v_range: [0., 1.],
                profile1,
                profile2,
            }),
            source: Value::Null,
        };
        let uv = m.evaluate(point).unwrap();
        assert!((uv[0] - 0.5).abs() < 1e-8, "{uv:?}");
        assert!((uv[1] - 0.25).abs() < 1e-8, "{uv:?}");
    }
}
