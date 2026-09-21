//! Typed evaluators for the non-planar surfaces observed in saved graphs.

use crate::native_parameters::ObjectGraph;
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

pub type Point = [f64; 3];
fn p(v: &Value, n: &str) -> Result<Point> {
    let a = v
        .as_array()
        .with_context(|| format!("{n} must be an array"))?;
    ensure!(a.len() == 3, "{n} must have three coordinates");
    let r = [
        a[0].as_f64().context("coordinate")?,
        a[1].as_f64().context("coordinate")?,
        a[2].as_f64().context("coordinate")?,
    ];
    ensure!(
        r.iter().all(|x| x.is_finite()),
        "{n} has nonfinite coordinate"
    );
    Ok(r)
}
fn dot(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn add(a: Point, b: Point) -> Point {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn mul(a: Point, s: f64) -> Point {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn len(a: Point) -> f64 {
    dot(a, a).sqrt()
}
fn range(v: &Value, n: &str) -> Result<[f64; 2]> {
    let a = v
        .as_array()
        .with_context(|| format!("{n} must be an array"))?;
    ensure!(a.len() == 2, "{n} must have two values");
    let r = [
        a[0].as_f64().context("range")?,
        a[1].as_f64().context("range")?,
    ];
    ensure!(
        r.iter().all(|x| x.is_finite()) && r[1] > r[0],
        "invalid {n}"
    );
    Ok(r)
}
fn corner(v: &Value) -> Result<[f64; 2]> {
    let a = v.as_array().context("envelope corner")?;
    ensure!(a.len() == 2, "envelope corner must have two coordinates");
    let r = [
        a[0].as_f64().context("envelope coordinate")?,
        a[1].as_f64().context("envelope coordinate")?,
    ];
    ensure!(
        r.iter().all(|x| x.is_finite()),
        "nonfinite envelope coordinate"
    );
    Ok(r)
}

fn target(g: &ObjectGraph, source: usize, v: &Value) -> Result<usize> {
    crate::native_saved_mesh::pointer(g, source, v)?.context("missing surface pointer target")
}

fn basis(f: &Value) -> Result<(Point, Point, Point)> {
    let x = p(&f["m_xVec"], "surface x basis")?;
    let y = p(&f["m_yVec"], "surface y basis")?;
    let z = p(&f["m_zVec"], "surface z basis")?;
    ensure!(
        (len(x) - 1.).abs() <= 1e-6 && (len(y) - 1.).abs() <= 1e-6 && (len(z) - 1.).abs() <= 1e-6,
        "surface basis is not unit"
    );
    ensure!(
        dot(x, y).abs() <= 1e-6 && dot(x, z).abs() <= 1e-6 && dot(y, z).abs() <= 1e-6,
        "surface basis is not orthogonal"
    );
    Ok((x, y, z))
}

pub fn surface_of_revolution_point(g: &ObjectGraph, si: usize, u: f64, v: f64) -> Result<Point> {
    let surface = ParametricSurface::read(g, si)?;
    ensure!(
        matches!(surface, ParametricSurface::SurfRev { .. }),
        "not SurfRev"
    );
    surface.evaluate(u, v)
}

pub fn ruled_surface_point(g: &ObjectGraph, si: usize, u: f64, v: f64) -> Result<Point> {
    let surface = ParametricSurface::read(g, si)?;
    ensure!(
        matches!(surface, ParametricSurface::RuledSurf { .. }),
        "not RuledSurf"
    );
    surface.evaluate(u, v)
}

#[derive(Debug, Clone)]
pub enum Curve {
    Line {
        origin: Point,
        direction: Point,
        range: [f64; 2],
    },
    Arc {
        center: Point,
        x: Point,
        y: Point,
        radius: f64,
        range: [f64; 2],
    },
    Ellipse {
        center: Point,
        x: Point,
        y: Point,
        x_len: f64,
        y_len: f64,
        range: [f64; 2],
    },
    HermiteSpline {
        nodes: Vec<HermiteNode>,
        range: [f64; 2],
        periodic: bool,
    },
}

#[derive(Debug, Clone)]
pub struct HermiteNode {
    parameter: f64,
    point: Point,
    tangent: Point,
}

#[derive(Debug, Clone)]
pub struct HermiteSurfaceNode {
    point: Point,
    u_tangent: Point,
    v_tangent: Point,
    mixed_derivative: Point,
}

impl Curve {
    pub fn line(origin: Point, direction: Point, range: [f64; 2]) -> Self {
        Self::Line {
            origin,
            direction,
            range,
        }
    }
    fn read(g: &ObjectGraph, i: usize) -> Result<Self> {
        let o = g.objects.get(i).context("profile object")?;
        match o.class_name.as_str() {
            "GLine" => {
                let direction = p(&o.fields["m_dirVec"], "line direction")?;
                ensure!(len(direction) > 1e-12, "singular line direction");
                Ok(Self::Line {
                    origin: p(&o.fields["m_origin"], "line origin")?,
                    direction,
                    range: range(&o.fields["m_endParams"], "line range")?,
                })
            }
            "GArc" => {
                let x = p(&o.fields["m_xVec"], "arc x basis")?;
                let y = p(&o.fields["m_yVec"], "arc y basis")?;
                let radius = o.fields["m_radius"].as_f64().context("arc radius")?;
                ensure!(
                    radius.is_finite()
                        && radius > 0.
                        && (len(x) - 1.).abs() <= 1e-6
                        && (len(y) - 1.).abs() <= 1e-6
                        && dot(x, y).abs() <= 1e-6,
                    "invalid arc basis"
                );
                Ok(Self::Arc {
                    center: p(&o.fields["m_center"], "arc center")?,
                    x,
                    y,
                    radius,
                    range: range(&o.fields["m_endParams"], "arc range")?,
                })
            }
            "GEllipse" => {
                let x = p(&o.fields["m_xVec"], "ellipse x basis")?;
                let y = p(&o.fields["m_yVec"], "ellipse y basis")?;
                let x_len = o.fields["m_xLen"].as_f64().context("ellipse x length")?;
                let y_len = o.fields["m_yLen"].as_f64().context("ellipse y length")?;
                ensure!(
                    x_len.is_finite()
                        && y_len.is_finite()
                        && x_len > 0.
                        && y_len > 0.
                        && (len(x) - 1.).abs() <= 1e-6
                        && (len(y) - 1.).abs() <= 1e-6
                        && dot(x, y).abs() <= 1e-6,
                    "invalid ellipse basis"
                );
                Ok(Self::Ellipse {
                    center: p(&o.fields["m_center"], "ellipse center")?,
                    x,
                    y,
                    x_len,
                    y_len,
                    range: range(&o.fields["m_endParams"], "ellipse range")?,
                })
            }
            "GHermiteSpline" => {
                let periodic = o.fields["m_Periodic"]
                    .as_bool()
                    .context("Hermite spline periodic flag")?;
                let range = range(&o.fields["m_endParams"], "Hermite spline range")?;
                let values = o.fields["m_NodeArray"]
                    .as_array()
                    .context("Hermite spline nodes")?;
                ensure!(values.len() >= 2, "Hermite spline needs two nodes");
                let mut nodes = Vec::with_capacity(values.len());
                for value in values {
                    let parameter = value["m_iParametr"]
                        .as_f64()
                        .context("Hermite node parameter")?;
                    ensure!(parameter.is_finite(), "nonfinite Hermite node parameter");
                    nodes.push(HermiteNode {
                        parameter,
                        point: p(&value["m_iPoint"], "Hermite node point")?,
                        tangent: p(&value["m_iTangent"], "Hermite node tangent")?,
                    });
                }
                ensure!(
                    nodes
                        .windows(2)
                        .all(|pair| pair[1].parameter > pair[0].parameter),
                    "Hermite node parameters are not ordered"
                );
                ensure!(
                    (nodes[0].parameter - range[0]).abs() <= 1e-7,
                    "Hermite node range does not match start parameter"
                );
                ensure!(
                    if periodic {
                        nodes.last().unwrap().parameter <= range[1] + 1e-7
                    } else {
                        (nodes.last().unwrap().parameter - range[1]).abs() <= 1e-7
                    },
                    "Hermite node range does not match end parameters"
                );
                Ok(Self::HermiteSpline {
                    nodes,
                    range,
                    periodic,
                })
            }
            name => bail!("unsupported parametric profile curve {name}"),
        }
    }
    fn eval(&self, t: f64) -> Point {
        match self {
            Self::Line {
                origin, direction, ..
            } => add(*origin, mul(*direction, t)),
            Self::Arc {
                center,
                x,
                y,
                radius,
                ..
            } => add(
                *center,
                mul(add(mul(*x, t.cos()), mul(*y, t.sin())), *radius),
            ),
            Self::Ellipse {
                center,
                x,
                y,
                x_len,
                y_len,
                ..
            } => add(
                *center,
                add(mul(*x, *x_len * t.cos()), mul(*y, *y_len * t.sin())),
            ),
            Self::HermiteSpline {
                nodes,
                range,
                periodic,
            } => {
                if *periodic {
                    periodic_hermite_value(nodes, *range, t, false)
                } else {
                    hermite_value(nodes, t, false)
                }
            }
        }
    }
    fn derivative(&self, t: f64) -> Point {
        match self {
            Self::Line { direction, .. } => *direction,
            Self::Arc { x, y, radius, .. } => {
                mul(add(mul(*x, -t.sin()), mul(*y, t.cos())), *radius)
            }
            Self::Ellipse {
                x, y, x_len, y_len, ..
            } => add(mul(*x, -*x_len * t.sin()), mul(*y, *y_len * t.cos())),
            Self::HermiteSpline {
                nodes,
                range,
                periodic,
            } => {
                if *periodic {
                    periodic_hermite_value(nodes, *range, t, true)
                } else {
                    hermite_value(nodes, t, true)
                }
            }
        }
    }

    pub(crate) fn tessellation_budget(&self, chord: f64) -> f64 {
        match self {
            Self::Line {
                direction, range, ..
            } => {
                let displacement = std::array::from_fn(|k| direction[k] * (range[1] - range[0]));
                len(displacement).ceil().max(1.)
            }
            Self::Arc { radius, range, .. } => {
                let span = (range[1] - range[0]).abs();
                let step = (4. * (chord / (2. * radius.max(chord))).min(1.).sqrt().asin())
                    .min(std::f64::consts::FRAC_PI_2);
                (span / step).ceil().max(1.)
            }
            Self::Ellipse {
                x_len,
                y_len,
                range,
                ..
            } => {
                let radius = x_len.max(*y_len);
                let span = (range[1] - range[0]).abs();
                let step = (4. * (chord / (2. * radius.max(chord))).min(1.).sqrt().asin())
                    .min(std::f64::consts::FRAC_PI_2);
                (span / step).ceil().max(1.)
            }
            Self::HermiteSpline {
                nodes,
                range,
                periodic,
            } => {
                let mut n: f64 = 0.;
                let mut pairs = nodes
                    .windows(2)
                    .map(|pair| (pair[0].clone(), pair[1].clone()))
                    .collect::<Vec<_>>();
                if *periodic {
                    let mut first = nodes[0].clone();
                    first.parameter += range[1] - range[0];
                    if first.parameter - nodes.last().unwrap().parameter > 1e-12 {
                        pairs.push((nodes.last().unwrap().clone(), first));
                    }
                }
                for (a, b) in pairs {
                    let delta = b.parameter - a.parameter;
                    let second = |s: f64| {
                        std::array::from_fn(|k| {
                            ((12. * s - 6.) * a.point[k]
                                + (6. * s - 4.) * delta * a.tangent[k]
                                + (-12. * s + 6.) * b.point[k]
                                + (6. * s - 2.) * delta * b.tangent[k])
                                / (delta * delta)
                        })
                    };
                    let acceleration = len(second(0.)).max(len(second(1.)));
                    let step = if acceleration > 1e-15 {
                        (8. * chord / acceleration).sqrt().min(delta)
                    } else {
                        delta
                    };
                    n += delta / step;
                }
                n.ceil().max(1.)
            }
        }
    }

    pub(crate) fn max_revolution_radius(&self) -> f64 {
        let radius = |p: Point| p[0].hypot(p[1]);
        match self {
            Self::Line {
                origin,
                direction,
                range,
            } => [range[0], range[1]]
                .into_iter()
                .map(|t| radius(add(*origin, mul(*direction, t))))
                .fold(0., f64::max),
            Self::Arc {
                center, radius: r, ..
            } => radius(*center) + r,
            Self::Ellipse {
                center,
                x_len,
                y_len,
                ..
            } => radius(*center) + x_len.max(*y_len),
            Self::HermiteSpline {
                nodes,
                range,
                periodic,
            } => {
                let mut points = nodes
                    .windows(2)
                    .flat_map(|pair| {
                        let a = &pair[0];
                        let b = &pair[1];
                        let delta = b.parameter - a.parameter;
                        [
                            a.point,
                            add(a.point, mul(a.tangent, delta / 3.)),
                            sub(b.point, mul(b.tangent, delta / 3.)),
                            b.point,
                        ]
                    })
                    .collect::<Vec<_>>();
                if *periodic {
                    let a = nodes.last().unwrap();
                    let b = &nodes[0];
                    let delta = range[1] - range[0] - a.parameter + b.parameter;
                    if delta > 1e-12 {
                        points.extend([
                            a.point,
                            add(a.point, mul(a.tangent, delta / 3.)),
                            sub(b.point, mul(b.tangent, delta / 3.)),
                            b.point,
                        ]);
                    }
                }
                points.into_iter().map(radius).fold(0., f64::max)
            }
        }
    }
}

fn hermite_segment(nodes: &[HermiteNode], t: f64) -> (&HermiteNode, &HermiteNode, f64) {
    let mut index = nodes.len() - 2;
    for (i, pair) in nodes.windows(2).enumerate() {
        if t <= pair[1].parameter {
            index = i;
            break;
        }
    }
    let a = &nodes[index];
    let b = &nodes[index + 1];
    let s = ((t - a.parameter) / (b.parameter - a.parameter)).clamp(0., 1.);
    (a, b, s)
}

fn hermite_value(nodes: &[HermiteNode], t: f64, derivative: bool) -> Point {
    let (a, b, s) = hermite_segment(nodes, t);
    hermite_value_with_fraction(a, b, s, derivative)
}

fn hermite_value_with_fraction(
    a: &HermiteNode,
    b: &HermiteNode,
    s: f64,
    derivative: bool,
) -> Point {
    let delta = b.parameter - a.parameter;
    let (h00, h10, h01, h11) = if derivative {
        (
            (6. * s * s - 6. * s) / delta,
            3. * s * s - 4. * s + 1.,
            (-6. * s * s + 6. * s) / delta,
            3. * s * s - 2. * s,
        )
    } else {
        (
            2. * s * s * s - 3. * s * s + 1.,
            s * s * s - 2. * s * s + s,
            -2. * s * s * s + 3. * s * s,
            s * s * s - s * s,
        )
    };
    let tangent_scale = if derivative { 1. } else { delta };
    add(
        add(mul(a.point, h00), mul(a.tangent, tangent_scale * h10)),
        add(mul(b.point, h01), mul(b.tangent, tangent_scale * h11)),
    )
}

fn periodic_hermite_value(
    nodes: &[HermiteNode],
    range: [f64; 2],
    t: f64,
    derivative: bool,
) -> Point {
    let period = range[1] - range[0];
    let local = range[0] + (t - range[0]).rem_euclid(period);
    if local <= nodes.last().unwrap().parameter {
        return hermite_value(nodes, local, derivative);
    }
    let a = nodes.last().unwrap();
    let mut b = nodes[0].clone();
    b.parameter += period;
    let s = ((local - a.parameter) / (b.parameter - a.parameter)).clamp(0., 1.);
    hermite_value_with_fraction(a, &b, s, derivative)
}

fn hermite_surface_coefficients(s: f64, span: f64, derivative: bool) -> [f64; 4] {
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
) -> Result<Point> {
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
    let cu = hermite_surface_coefficients(su, us, derivative_u);
    let cv = hermite_surface_coefficients(sv, vs, derivative_v);
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

#[derive(Debug, Clone)]
pub enum ParametricSurface {
    SurfRev {
        center: Point,
        x: Point,
        y: Point,
        z: Point,
        envelope: [[f64; 2]; 2],
        orient: bool,
        profile: Curve,
    },
    ConeSurf {
        center: Point,
        x: Point,
        y: Point,
        z: Point,
        envelope: [[f64; 2]; 2],
        orient: bool,
        half_angle: f64,
    },
    RuledSurf {
        envelope: [[f64; 2]; 2],
        orient: bool,
        profile1: Curve,
        profile2: Curve,
    },
    HermiteSurf {
        envelope: [[f64; 2]; 2],
        orient: bool,
        u_params: Vec<f64>,
        v_params: Vec<f64>,
        periodic: [bool; 2],
        nodes: Vec<HermiteSurfaceNode>,
    },
}

impl ParametricSurface {
    pub fn read(g: &ObjectGraph, si: usize) -> Result<Self> {
        let s = g.objects.get(si).context("surface index")?;
        let corners = s.fields["m_Envelope"]["m_corners"]
            .as_array()
            .context("surface envelope")?;
        ensure!(corners.len() == 2, "surface envelope corners");
        let a = corner(&corners[0])?;
        let b = corner(&corners[1])?;
        ensure!(
            b[0] > a[0] && b[1] > a[1],
            "surface envelope is not ordered"
        );
        let envelope = [a, b];
        match s.class_name.as_str() {
            "SurfRev" => {
                let profile = Curve::read(g, target(g, si, &s.fields["m_pProfileCurve"])?)?;
                let (x, y, z) = basis(&s.fields)?;
                Ok(Self::SurfRev {
                    center: p(&s.fields["m_center"], "surface center")?,
                    x,
                    y,
                    z,
                    envelope,
                    orient: s.fields["m_orientFlag"]
                        .as_bool()
                        .context("surface orientation")?,
                    profile,
                })
            }
            "ConeSurf" => {
                let (x, y, z) = basis(&s.fields)?;
                let half_angle = s.fields["m_halfAngle"]
                    .as_f64()
                    .context("cone half angle")?;
                ensure!(
                    half_angle.is_finite()
                        && half_angle > 1e-9
                        && half_angle < std::f64::consts::PI - 1e-9
                        && half_angle.sin().abs() > 1e-9,
                    "invalid cone half angle"
                );
                Ok(Self::ConeSurf {
                    center: p(&s.fields["m_center"], "cone center")?,
                    x,
                    y,
                    z,
                    envelope,
                    orient: s.fields["m_orientFlag"]
                        .as_bool()
                        .context("cone orientation")?,
                    half_angle,
                })
            }
            "RuledSurf" => {
                let profile1 = Curve::read(g, target(g, si, &s.fields["m_pProfileCurve1"])?)?;
                let profile2 = if s.fields["m_pProfileCurve2"]["pointer_token"].as_u64() == Some(0)
                {
                    let point1 = p(&s.fields["m_Point1"], "ruled surface point 1")?;
                    let point2 = p(&s.fields["m_Point2"], "ruled surface point 2")?;
                    Curve::line(
                        point1,
                        sub(point2, point1),
                        [envelope[0][0], envelope[1][0]],
                    )
                } else {
                    Curve::read(g, target(g, si, &s.fields["m_pProfileCurve2"])?)?
                };
                Ok(Self::RuledSurf {
                    envelope,
                    orient: s.fields["m_orientFlag"]
                        .as_bool()
                        .context("surface orientation")?,
                    profile1,
                    profile2,
                })
            }
            "HermiteSurf" => {
                let u_params = s.fields["m_uParams"]
                    .as_array()
                    .context("Hermite surface u parameters")?
                    .iter()
                    .map(|value| value.as_f64().context("Hermite surface u parameter"))
                    .collect::<Result<Vec<_>>>()?;
                let v_params = s.fields["m_vParams"]
                    .as_array()
                    .context("Hermite surface v parameters")?
                    .iter()
                    .map(|value| value.as_f64().context("Hermite surface v parameter"))
                    .collect::<Result<Vec<_>>>()?;
                let periodic: [bool; 2] = s.fields["m_periodic"]
                    .as_array()
                    .context("Hermite surface periodic flags")?
                    .iter()
                    .map(|value| value.as_bool().context("Hermite surface periodic flag"))
                    .collect::<Result<Vec<_>>>()?
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Hermite surface periodic flag count"))?;
                ensure!(
                    !periodic[0] && !periodic[1],
                    "periodic Hermite surface unsupported"
                );
                ensure!(
                    u_params.len() >= 2
                        && v_params.len() >= 2
                        && u_params.windows(2).all(|pair| pair[1] > pair[0])
                        && v_params.windows(2).all(|pair| pair[1] > pair[0]),
                    "Hermite surface parameter grid is invalid"
                );
                let values = s.fields["m_NodeArray"]
                    .as_array()
                    .context("Hermite surface nodes")?;
                ensure!(
                    values.len() == u_params.len() * v_params.len(),
                    "Hermite surface node grid is invalid"
                );
                let mut nodes = Vec::with_capacity(values.len());
                for value in values {
                    let tangent = value["m_iTangent"]
                        .as_array()
                        .context("Hermite surface tangents")?;
                    ensure!(tangent.len() == 2, "Hermite surface tangent count");
                    nodes.push(HermiteSurfaceNode {
                        point: p(&value["m_iPoint"], "Hermite surface point")?,
                        u_tangent: p(&tangent[0], "Hermite surface u tangent")?,
                        v_tangent: p(&tangent[1], "Hermite surface v tangent")?,
                        mixed_derivative: p(
                            &value["m_iMixedDer"],
                            "Hermite surface mixed derivative",
                        )?,
                    });
                }
                ensure!(
                    (u_params[0] - envelope[0][0]).abs() <= 1e-7
                        && (u_params.last().unwrap() - envelope[1][0]).abs() <= 1e-7
                        && (v_params[0] - envelope[0][1]).abs() <= 1e-7
                        && (v_params.last().unwrap() - envelope[1][1]).abs() <= 1e-7,
                    "Hermite surface grid/envelope mismatch"
                );
                Ok(Self::HermiteSurf {
                    envelope,
                    orient: s.fields["m_orientFlag"]
                        .as_bool()
                        .context("surface orientation")?,
                    u_params,
                    v_params,
                    periodic,
                    nodes,
                })
            }
            name => bail!("unsupported parametric surface {name}"),
        }
    }
    pub fn bounds(&self) -> [[f64; 2]; 2] {
        match self {
            Self::SurfRev { envelope, .. }
            | Self::ConeSurf { envelope, .. }
            | Self::RuledSurf { envelope, .. }
            | Self::HermiteSurf { envelope, .. } => *envelope,
        }
    }
    pub fn orientation(&self) -> bool {
        match self {
            Self::SurfRev { orient, .. }
            | Self::ConeSurf { orient, .. }
            | Self::RuledSurf { orient, .. }
            | Self::HermiteSurf { orient, .. } => *orient,
        }
    }
    pub fn evaluate(&self, u: f64, v: f64) -> Result<Point> {
        let e = self.bounds();
        ensure!(
            u >= e[0][0] - 1e-9
                && u <= e[1][0] + 1e-9
                && v >= e[0][1] - 1e-9
                && v <= e[1][1] + 1e-9,
            "surface coordinates outside envelope"
        );
        Ok(match self {
            Self::SurfRev {
                center,
                x,
                y,
                z,
                profile,
                ..
            } => {
                let q = profile.eval(v);
                let local = [
                    q[0] * u.cos() - q[1] * u.sin(),
                    q[0] * u.sin() + q[1] * u.cos(),
                    q[2],
                ];
                add(
                    *center,
                    add(add(mul(*x, local[0]), mul(*y, local[1])), mul(*z, local[2])),
                )
            }
            Self::ConeSurf {
                center,
                x,
                y,
                z,
                half_angle,
                ..
            } => {
                let (sin, cos) = half_angle.sin_cos();
                let radial = [u.cos(), u.sin(), 0.];
                add(
                    *center,
                    mul(
                        add(
                            add(mul(*x, sin * radial[0]), mul(*y, sin * radial[1])),
                            mul(*z, cos),
                        ),
                        v,
                    ),
                )
            }
            Self::RuledSurf {
                envelope: _,
                profile1,
                profile2,
                ..
            } => {
                let t = |c: &Curve| c_range(c)[0] + u * (c_range(c)[1] - c_range(c)[0]);
                add(
                    mul(profile1.eval(t(profile1)), 1. - v),
                    mul(profile2.eval(t(profile2)), v),
                )
            }
            Self::HermiteSurf {
                u_params,
                v_params,
                nodes,
                ..
            } => hermite_surface_point(nodes, u_params, v_params, u, v, false, false)?,
        })
    }
    pub fn normal(&self, u: f64, v: f64) -> Result<Point> {
        let e = self.bounds();
        ensure!(
            u.is_finite()
                && v.is_finite()
                && u >= e[0][0] - 1e-9
                && u <= e[1][0] + 1e-9
                && v >= e[0][1] - 1e-9
                && v <= e[1][1] + 1e-9,
            "coordinates outside envelope"
        );
        if let Self::SurfRev {
            x,
            y,
            z,
            profile,
            orient,
            ..
        } = self
        {
            let q = profile.eval(v);
            let dq = profile.derivative(v);
            let cu = u.cos();
            let su = u.sin();
            let ru = [-q[0] * su - q[1] * cu, q[0] * cu - q[1] * su, 0.];
            let rv = [dq[0] * cu - dq[1] * su, dq[0] * su + dq[1] * cu, dq[2]];
            let transform = |q: Point| add(add(mul(*x, q[0]), mul(*y, q[1])), mul(*z, q[2]));
            let tu = transform(ru);
            let tv = transform(rv);
            let mut n = cross(tu, tv);
            ensure!(
                n.iter().all(|value| value.is_finite()),
                "nonfinite surface normal"
            );
            if len(n) <= 1e-12 {
                // At a revolution pole, the U derivative collapses.  Use the
                // limiting radial/axial profile tangent instead.
                ensure!(
                    q[0].abs() <= 1e-9 && q[1].abs() <= 1e-9 && dq[1].abs() <= 1e-9,
                    "unsupported nonmeridional parametric singularity"
                );
                // Serialized meridian profiles have their radial tangent in
                // local X.  At the rotation axis the limiting normal is
                // continuous in U; projecting dq onto the current radial
                // direction would spuriously change the pole sign.
                let pole_du = transform([-su, cu, 0.]);
                let pole_dv = transform([dq[0] * cu, dq[0] * su, dq[2]]);
                n = cross(pole_du, pole_dv);
                ensure!(len(n) > 1e-12, "surface normal is singular");
            }
            return Ok(mul(n, if *orient { 1. / len(n) } else { -1. / len(n) }));
        }
        if let Self::HermiteSurf {
            u_params,
            v_params,
            nodes,
            orient,
            ..
        } = self
        {
            let du = hermite_surface_point(nodes, u_params, v_params, u, v, true, false)?;
            let dv = hermite_surface_point(nodes, u_params, v_params, u, v, false, true)?;
            let n = cross(du, dv);
            let l = len(n);
            ensure!(
                n.iter().all(|value| value.is_finite()) && l > 1e-12,
                "Hermite surface normal is singular"
            );
            return Ok(mul(n, if *orient { 1. / l } else { -1. / l }));
        }
        if let Self::ConeSurf {
            x,
            y,
            z,
            orient,
            half_angle,
            ..
        } = self
        {
            let (sin, cos) = half_angle.sin_cos();
            let (su, cu) = u.sin_cos();
            let transform = |q: Point| add(add(mul(*x, q[0]), mul(*y, q[1])), mul(*z, q[2]));
            let du = transform([-v * sin * su, v * sin * cu, 0.]);
            let dv = transform([sin * cu, sin * su, cos]);
            let n = cross(du, dv);
            let l = len(n);
            ensure!(
                l.is_finite() && l > 1e-12,
                "cone surface normal is singular"
            );
            return Ok(mul(n, if *orient { 1. / l } else { -1. / l }));
        }
        let Self::RuledSurf {
            profile1,
            profile2,
            orient,
            ..
        } = self
        else {
            unreachable!()
        };
        let r1 = c_range(profile1);
        let r2 = c_range(profile2);
        let t1 = r1[0] + u * (r1[1] - r1[0]);
        let t2 = r2[0] + u * (r2[1] - r2[0]);
        let p1 = profile1.eval(t1);
        let p2 = profile2.eval(t2);
        let du = add(
            mul(profile1.derivative(t1), (1. - v) * (r1[1] - r1[0])),
            mul(profile2.derivative(t2), v * (r2[1] - r2[0])),
        );
        let dv = sub(p2, p1);
        let n = cross(du, dv);
        ensure!(
            n.iter().all(|value| value.is_finite()),
            "nonfinite surface normal"
        );
        let l = len(n);
        ensure!(l > 1e-12, "surface normal is singular");
        Ok(mul(n, if *orient { 1. / l } else { -1. / l }))
    }
}

fn cross(a: Point, b: Point) -> Point {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn c_range(c: &Curve) -> [f64; 2] {
    match c {
        Curve::Line { range, .. }
        | Curve::Arc { range, .. }
        | Curve::Ellipse { range, .. }
        | Curve::HermiteSpline { range, .. } => *range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_parameters::{GraphEdge, GraphObject};
    fn o(name: &str, tag: u16, token: u32, fields: Value) -> GraphObject {
        GraphObject {
            class_tag: tag,
            class_name: name.into(),
            token,
            start: 0,
            fields_end: 0,
            fields,
        }
    }
    fn e(source: usize, offset: usize, token: u32, target: usize, tag: u16) -> GraphEdge {
        GraphEdge {
            source_object_index: source,
            pointer_offset: offset,
            pointer_token: token,
            target_object_index: target,
            target_class_tag: tag,
        }
    }
    fn g(objects: Vec<GraphObject>, edges: Vec<GraphEdge>) -> ObjectGraph {
        ObjectGraph {
            consumed_bytes: 1,
            objects,
            edges,
        }
    }
    #[test]
    fn surface_revolution_uses_edge_target_not_token_index() {
        let s = o(
            "SurfRev",
            4283,
            u32::MAX,
            serde_json::json!({"m_Envelope":{"m_corners":[[0.,0.],[1.,1.]]},"m_center":[0.,0.,0.],"m_xVec":[1.,0.,0.],"m_yVec":[0.,1.,0.],"m_zVec":[0.,0.,1.],"m_orientFlag":true,"m_pProfileCurve":{"class_tag":1973,"offset":8,"pointer_token":55}}),
        );
        let filler = o("Plane", 634, 55, serde_json::json!({}));
        let line = o(
            "GLine",
            1973,
            77,
            serde_json::json!({"m_origin":[2.,0.,0.],"m_dirVec":[0.,0.,1.],"m_endParams":[0.,1.]}),
        );
        let mut graph = g(vec![s, filler, line], vec![e(0, 8, 55, 2, 1973)]);
        assert_eq!(
            surface_of_revolution_point(&graph, 0, 0.0, 0.5).unwrap(),
            [2.0, 0.0, 0.5]
        );
        let surface = ParametricSurface::read(&graph, 0).unwrap();
        assert_eq!(surface.normal(0.0, 0.5).unwrap(), [1.0, 0.0, 0.0]);
        graph.objects[0].fields["m_orientFlag"] = Value::Bool(false);
        let reversed = ParametricSurface::read(&graph, 0).unwrap();
        assert_eq!(reversed.normal(0.0, 0.5).unwrap(), [-1.0, 0.0, 0.0]);
    }

    #[test]
    fn hermite_spline_reads_nodes_and_evaluates_cubic_segment() {
        let graph = g(
            vec![o(
                "GHermiteSpline",
                1,
                2,
                serde_json::json!({
                    "m_NodeArray": [
                        {"m_iParametr": 0., "m_iPoint": [0., 0., 0.], "m_iTangent": [1., 0., 0.]},
                        {"m_iParametr": 1., "m_iPoint": [1., 1., 0.], "m_iTangent": [0., 1., 0.]}
                    ],
                    "m_Periodic": false,
                    "m_endParams": [0., 1.]
                }),
            )],
            vec![],
        );
        let curve = Curve::read(&graph, 0).unwrap();
        assert_eq!(curve.eval(0.), [0., 0., 0.]);
        assert_eq!(curve.eval(1.), [1., 1., 0.]);
        let midpoint = curve.eval(0.5);
        assert!((midpoint[0] - 0.625).abs() < 1e-12);
        assert!((midpoint[1] - 0.375).abs() < 1e-12);
        assert_eq!(curve.derivative(0.), [1., 0., 0.]);
        assert_eq!(curve.derivative(1.), [0., 1., 0.]);
        assert!(curve.tessellation_budget(0.0001) > 1.);
    }

    #[test]
    fn periodic_hermite_spline_evaluates_closing_segment_and_wraps() {
        let period = std::f64::consts::TAU;
        let graph = g(
            vec![o(
                "GHermiteSpline",
                1,
                2,
                serde_json::json!({
                    "m_NodeArray": [
                        {"m_iParametr": 0., "m_iPoint": [2., 0., 0.], "m_iTangent": [0., 1., 0.]},
                        {"m_iParametr": 1.5707963267948966, "m_iPoint": [0., 2., 0.], "m_iTangent": [-1., 0., 0.]},
                        {"m_iParametr": 3.141592653589793, "m_iPoint": [-2., 0., 0.], "m_iTangent": [0., -1., 0.]},
                        {"m_iParametr": 4.71238898038469, "m_iPoint": [0., -2., 0.], "m_iTangent": [1., 0., 0.]}
                    ],
                    "m_Periodic": true,
                    "m_endParams": [0., 6.283185307179586]
                }),
            )],
            vec![],
        );
        let curve = Curve::read(&graph, 0).unwrap();
        let start = curve.eval(0.);
        let end = curve.eval(period);
        assert!(start.iter().zip(end).all(|(a, b)| (a - b).abs() < 1e-12));
        let closing = curve.eval(5.497787143782138);
        assert!(closing[0] > 0. && closing[1] < 0., "{closing:?}");
        let wrapped = curve.eval(-0.7853981633974483);
        assert!(
            wrapped
                .iter()
                .zip(closing)
                .all(|(a, b)| (a - b).abs() < 1e-12)
        );
        assert!(curve.max_revolution_radius() >= 2.);
        assert!(curve.tessellation_budget(0.01) > 4.);
    }
    #[test]
    fn ruled_surface_blends_profiles() {
        let s = o(
            "RuledSurf",
            3859,
            u32::MAX,
            serde_json::json!({"m_Envelope":{"m_corners":[[0.,0.],[1.,1.]]},"m_orientFlag":true,"m_pProfileCurve1":{"class_tag":1973,"offset":8,"pointer_token":53},"m_pProfileCurve2":{"class_tag":1973,"offset":12,"pointer_token":54}}),
        );
        let a = o(
            "GLine",
            1973,
            1,
            serde_json::json!({"m_origin":[0.,0.,0.],"m_dirVec":[1.,0.,0.],"m_endParams":[0.,1.]}),
        );
        let b = o(
            "GLine",
            1973,
            2,
            serde_json::json!({"m_origin":[0.,0.,1.],"m_dirVec":[0.,1.,0.],"m_endParams":[0.,2.]}),
        );
        let graph = g(
            vec![s, a, b],
            vec![e(0, 8, 53, 1, 1973), e(0, 12, 54, 2, 1973)],
        );
        assert_eq!(
            ruled_surface_point(&graph, 0, 0.5, 0.5).unwrap(),
            [0.25, 0.5, 0.5]
        );
    }

    #[test]
    fn ruled_surface_false_orientation_reverses_normal() {
        let s = o(
            "RuledSurf",
            3859,
            u32::MAX,
            serde_json::json!({"m_Envelope":{"m_corners":[[0.,0.],[1.,1.]]},"m_orientFlag":true,"m_pProfileCurve1":{"class_tag":1973,"offset":8,"pointer_token":53},"m_pProfileCurve2":{"class_tag":1973,"offset":12,"pointer_token":54}}),
        );
        let a = o(
            "GLine",
            1973,
            1,
            serde_json::json!({"m_origin":[0.,0.,0.],"m_dirVec":[1.,0.,0.],"m_endParams":[0.,1.]}),
        );
        let b = o(
            "GLine",
            1973,
            2,
            serde_json::json!({"m_origin":[0.,0.,1.],"m_dirVec":[0.,1.,0.],"m_endParams":[0.,2.]}),
        );
        let mut graph = g(
            vec![s, a, b],
            vec![e(0, 8, 53, 1, 1973), e(0, 12, 54, 2, 1973)],
        );
        let positive = ParametricSurface::read(&graph, 0)
            .unwrap()
            .normal(0.5, 0.5)
            .unwrap();
        graph.objects[0].fields["m_orientFlag"] = Value::Bool(false);
        let negative = ParametricSurface::read(&graph, 0)
            .unwrap()
            .normal(0.5, 0.5)
            .unwrap();
        assert_eq!(negative, [-positive[0], -positive[1], -positive[2]]);
    }

    #[test]
    fn cone_surface_uses_slant_distance_and_half_angle() {
        let surface = ParametricSurface::ConeSurf {
            center: [1., 2., 3.],
            x: [1., 0., 0.],
            y: [0., 1., 0.],
            z: [0., 0., 1.],
            envelope: [[0., 1.], [std::f64::consts::FRAC_PI_2, 2.]],
            orient: true,
            half_angle: std::f64::consts::FRAC_PI_4,
        };
        let p = surface.evaluate(0., 2.).unwrap();
        let diagonal = 2_f64.sqrt();
        assert!((p[0] - (1. + diagonal)).abs() < 1e-12);
        assert_eq!(p[1], 2.);
        assert!((p[2] - (3. + diagonal)).abs() < 1e-12);
        let n = surface.normal(0., 2.).unwrap();
        assert!((n[0] - 2_f64.sqrt() / 2.).abs() < 1e-12);
        assert!((n[2] + 2_f64.sqrt() / 2.).abs() < 1e-12);
    }

    #[test]
    fn revolution_uses_local_profile_and_rotated_basis() {
        let s = o(
            "SurfRev",
            4283,
            u32::MAX,
            serde_json::json!({"m_Envelope":{"m_corners":[[0.,0.],[1.,1.]]},"m_center":[10.,20.,30.],"m_xVec":[0.,1.,0.],"m_yVec":[-1.,0.,0.],"m_zVec":[0.,0.,1.],"m_orientFlag":true,"m_pProfileCurve":{"class_tag":1973,"offset":8,"pointer_token":55}}),
        );
        let line = o(
            "GLine",
            1973,
            77,
            serde_json::json!({"m_origin":[2.,0.,0.],"m_dirVec":[0.,0.,1.],"m_endParams":[0.,1.]}),
        );
        let graph = g(vec![s, line], vec![e(0, 8, 55, 1, 1973)]);
        assert_eq!(
            surface_of_revolution_point(&graph, 0, 0.0, 0.5).unwrap(),
            [10., 22., 30.5]
        );
    }

    #[test]
    fn sphere_pole_normal_has_one_continuous_limit() {
        let surface = ParametricSurface::SurfRev {
            center: [0., 0., 0.],
            x: [1., 0., 0.],
            y: [0., 1., 0.],
            z: [0., 0., 1.],
            envelope: [
                [0., -std::f64::consts::FRAC_PI_2],
                [std::f64::consts::TAU, std::f64::consts::FRAC_PI_2],
            ],
            orient: true,
            profile: Curve::Arc {
                center: [0., 0., 0.],
                x: [1., 0., 0.],
                y: [0., 0., 1.],
                radius: 1.,
                range: [-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2],
            },
        };
        for u in [
            0.,
            std::f64::consts::FRAC_PI_2,
            std::f64::consts::PI,
            3. * std::f64::consts::FRAC_PI_2,
        ] {
            let north = surface.normal(u, std::f64::consts::FRAC_PI_2).unwrap();
            let south = surface.normal(u, -std::f64::consts::FRAC_PI_2).unwrap();
            assert!((north[2] - 1.).abs() < 1e-12, "u={u}: {north:?}");
            assert!((south[2] + 1.).abs() < 1e-12, "u={u}: {south:?}");
        }
    }

    #[test]
    fn ruled_normal_blends_profile_derivatives_by_v() {
        let surface = ParametricSurface::RuledSurf {
            envelope: [[0., 0.], [1., 1.]],
            orient: true,
            profile1: Curve::line([0., 0., 0.], [1., 0., 0.], [0., 2.]),
            profile2: Curve::line([0., 1., 1.], [0., 1., 0.], [0., 3.]),
        };
        let n = surface.normal(0.5, 0.5).unwrap();
        let norm = 19.25_f64.sqrt();
        let expected = [1.5 / norm, -1. / norm, 4. / norm];
        assert!((0..3).all(|i| (n[i] - expected[i]).abs() < 1e-3), "{n:?}");
        assert!(
            surface
                .normal(0.5, 0.)
                .unwrap()
                .iter()
                .all(|x| x.is_finite())
        );
        assert!(
            surface
                .normal(0.5, 1.)
                .unwrap()
                .iter()
                .all(|x| x.is_finite())
        );
    }

    #[test]
    fn nonmeridional_pole_singularity_is_rejected() {
        let surface = ParametricSurface::SurfRev {
            center: [0., 0., 0.],
            x: [1., 0., 0.],
            y: [0., 1., 0.],
            z: [0., 0., 1.],
            envelope: [[0., 0.], [std::f64::consts::TAU, 1.]],
            orient: true,
            profile: Curve::line([0., 0., 0.], [0., 1., 0.], [0., 1.]),
        };
        let error = surface.normal(0.5, 0.).unwrap_err().to_string();
        assert!(error.contains("nonmeridional"), "{error}");
    }

    #[test]
    fn left_handed_sphere_pole_matches_near_pole_normal() {
        let surface = ParametricSurface::SurfRev {
            center: [0., 0., 0.],
            x: [1., 0., 0.],
            y: [0., -1., 0.],
            z: [0., 0., 1.],
            envelope: [
                [0., -std::f64::consts::FRAC_PI_2],
                [std::f64::consts::TAU, std::f64::consts::FRAC_PI_2],
            ],
            orient: true,
            profile: Curve::Arc {
                center: [0., 0., 0.],
                x: [1., 0., 0.],
                y: [0., 0., 1.],
                radius: 1.,
                range: [-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2],
            },
        };
        let pole = surface.normal(0.7, std::f64::consts::FRAC_PI_2).unwrap();
        let near = surface
            .normal(0.7, std::f64::consts::FRAC_PI_2 - 1e-6)
            .unwrap();
        assert!(pole.iter().zip(near).map(|(a, b)| a * b).sum::<f64>() > 1. - 1e-8);
    }

    #[test]
    fn revolution_rotates_both_local_profile_coordinates() {
        let surface = ParametricSurface::SurfRev {
            center: [0., 0., 0.],
            x: [1., 0., 0.],
            y: [0., 1., 0.],
            z: [0., 0., 1.],
            envelope: [[0., 0.], [std::f64::consts::TAU, 1.]],
            orient: true,
            profile: Curve::line([1., 2., 0.], [0., 0., 1.], [0., 1.]),
        };
        let u = 0.5;
        let point = surface.evaluate(u, 0.25).unwrap();
        let expected = [u.cos() - 2. * u.sin(), u.sin() + 2. * u.cos(), 0.25];
        assert!((0..3).all(|i| (point[i] - expected[i]).abs() < 1e-12));
        assert!(
            surface
                .normal(u, 0.25)
                .unwrap()
                .iter()
                .all(|x| x.is_finite())
        );
    }
}
