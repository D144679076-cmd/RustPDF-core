//! PDF Shading (gradient) rasteriser — ISO 32000-1 §8.7.
//!
//! ShadingTypes implemented here: 2 (axial) and 3 (radial) inline; mesh types
//! 4 (free-form) / 5 (lattice) via `mesh` and 6 (Coons) / 7 (tensor) via
//! `patch`, function-based type 1 via `function_shaded`, with the
//! FunctionType 4 PostScript evaluator in `ps`. Function types 2
//! (Exponential) and 3 (Stitching) supported for the 1-input axial/radial
//! path; Type 0 (sampled) is not yet implemented.

use crate::content::graphics_state::Matrix;
use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

use super::canvas::PixmapBuffer;

mod function_shaded;
mod mesh;
mod patch;
mod ps;

pub use function_shaded::FunctionShading;
pub use mesh::MeshShading;

// ---------------------------------------------------------------------------
// Shading function
// ---------------------------------------------------------------------------

/// A PDF shading function (ISO 32000-1 §7.10).
#[derive(Debug, Clone)]
pub enum ShadingFunction {
    /// Type 2 — Exponential interpolation: f(t) = C0 + t^N * (C1 - C0).
    Exponential { c0: Vec<f64>, c1: Vec<f64>, n: f64 },
    /// Type 3 — Stitching: concatenates sub-functions over breakpoints.
    Stitching {
        bounds: Vec<f64>,
        encode: Vec<f64>,
        functions: Vec<ShadingFunction>,
        domain: [f64; 2],
    },
    /// Type 4 — PostScript calculator, 2-input entry (`eval2`). Used by
    /// function-based (Type 1) shadings and mesh /Function overrides.
    PostScript {
        /// Decoded program body.
        body: Vec<u8>,
        /// The function's own /Domain `[x0 x1 y0 y1]`.
        ps_domain: [f64; 4],
    },
}

impl ShadingFunction {
    /// Parse a PDF Function object (dictionary or stream).
    ///
    /// Type 4 (PostScript) functions carry their program in the stream
    /// contents — decoded here; the other function types need only the dict.
    pub fn parse(obj: &PdfObject, doc: &PdfDocument) -> Result<Self> {
        match obj {
            PdfObject::Reference(..) => {
                let resolved = doc.resolve(obj)?;
                Self::parse(&resolved, doc)
            }
            PdfObject::Stream(s) => {
                let dict = s.dict.clone();
                let body = s.decode().map_err(|e| {
                    PdfError::invalid_token(0, format!("function stream decode failed: {}", e))
                })?;
                Self::parse_dict(&dict, doc, Some(body))
            }
            PdfObject::Dictionary(d) => Self::parse_dict(d, doc, None),
            other => Err(PdfError::invalid_token(
                0,
                format!("expected function dict, got {:?}", other),
            )),
        }
    }

    fn parse_dict(dict: &PdfDict, doc: &PdfDocument, ps_body: Option<Vec<u8>>) -> Result<Self> {
        let fn_type = dict.get("FunctionType").and_then(pdf_int).unwrap_or(2);

        match fn_type {
            2 => {
                let c0 = dict_f64_array(dict, "C0").unwrap_or_else(|| vec![0.0]);
                let c1 = dict_f64_array(dict, "C1").unwrap_or_else(|| vec![1.0]);
                let n = dict.get("N").and_then(obj_f64).unwrap_or(1.0);
                Ok(ShadingFunction::Exponential { c0, c1, n })
            }
            3 => {
                let bounds = dict_f64_array(dict, "Bounds").unwrap_or_default();
                let encode = dict_f64_array(dict, "Encode").unwrap_or_default();
                let domain_arr = dict_f64_array(dict, "Domain").unwrap_or_else(|| vec![0.0, 1.0]);
                let domain = [
                    domain_arr.first().copied().unwrap_or(0.0),
                    domain_arr.get(1).copied().unwrap_or(1.0),
                ];
                let fns_arr = match dict.get("Functions") {
                    Some(PdfObject::Array(a)) => a.clone(),
                    _ => {
                        return Err(PdfError::invalid_token(
                            0,
                            "stitching function missing Functions",
                        ))
                    }
                };
                let functions = fns_arr
                    .iter()
                    .map(|f| ShadingFunction::parse(f, doc))
                    .collect::<Result<Vec<_>>>()?;
                Ok(ShadingFunction::Stitching {
                    bounds,
                    encode,
                    functions,
                    domain,
                })
            }
            4 => {
                let body = ps_body.ok_or_else(|| {
                    PdfError::invalid_token(0, "FunctionType 4 requires its stream contents")
                })?;
                // /Domain for a 2-input function is [xlo xhi ylo yhi]; the
                // *mapping* into [0,1] happens inside ps::eval2.
                let dom_arr =
                    dict_f64_array(dict, "Domain").unwrap_or_else(|| vec![0.0, 1.0, 0.0, 1.0]);
                let ps_domain = [
                    *dom_arr.first().unwrap_or(&0.0),
                    *dom_arr.get(1).unwrap_or(&1.0),
                    *dom_arr.get(2).unwrap_or(&0.0),
                    *dom_arr.get(3).unwrap_or(&1.0),
                ];
                Ok(ShadingFunction::PostScript { body, ps_domain })
            }
            other => {
                // Unsupported — return identity grey function so rendering doesn't crash.
                log::warn!(
                    "unsupported shading FunctionType {}, using identity grey",
                    other
                );
                Ok(ShadingFunction::Exponential {
                    c0: vec![0.5],
                    c1: vec![0.5],
                    n: 1.0,
                })
            }
        }
    }

    /// Evaluate the function at `t` in [0, 1], returning color components.
    pub fn eval(&self, t: f64) -> Vec<f64> {
        match self {
            ShadingFunction::Exponential { c0, c1, n } => {
                let t_n = t.clamp(0.0, 1.0).powf(*n);
                c0.iter()
                    .zip(c1.iter())
                    .map(|(a, b)| a + t_n * (b - a))
                    .collect()
            }
            ShadingFunction::Stitching {
                bounds,
                encode,
                functions,
                domain,
            } => {
                // Find which sub-function applies.
                let t = t.clamp(domain[0], domain[1]);
                let n = functions.len();
                if n == 0 {
                    return vec![0.0];
                }
                // Bounds has n-1 entries; sub-ranges are [domain[0], bounds[0]], ..., [bounds[n-2], domain[1]]
                let mut idx = n - 1;
                for (i, &b) in bounds.iter().enumerate() {
                    if t < b {
                        idx = i;
                        break;
                    }
                }
                // Map t into the sub-function's domain via Encode.
                let (sub_lo, sub_hi) = if idx == 0 {
                    (domain[0], bounds.first().copied().unwrap_or(domain[1]))
                } else if idx < n - 1 {
                    (bounds[idx - 1], bounds[idx])
                } else {
                    (bounds.last().copied().unwrap_or(domain[0]), domain[1])
                };

                let enc_lo = encode.get(idx * 2).copied().unwrap_or(0.0);
                let enc_hi = encode.get(idx * 2 + 1).copied().unwrap_or(1.0);

                let t_local = if sub_hi > sub_lo {
                    enc_lo + (t - sub_lo) / (sub_hi - sub_lo) * (enc_hi - enc_lo)
                } else {
                    enc_lo
                };

                functions[idx].eval(t_local)
            }
            ShadingFunction::PostScript { .. } => {
                // 1-input eval is meaningless for the 2-input PostScript
                // calculator; renderers route through `eval2` (see `eval1`
                // for the mesh 1-input path). Return a flat grey so
                // axial/radial callers that somehow attach one don't crash.
                log::warn!("1-input eval on PostScript function; using grey");
                vec![0.5]
            }
        }
    }

    /// Evaluate a **1-input** function at `t` — the mesh /Function override
    /// form (§8.7.4.4: each colour record carries one t value).
    ///
    /// PostScript (FunctionType 4) functions run through the calculator with
    /// a single-value stack; the other types evaluate like [`Self::eval`].
    ///
    /// # Errors
    /// Malformed PostScript programs.
    pub fn eval1(&self, t: f64) -> std::result::Result<Vec<f64>, String> {
        match self {
            ShadingFunction::PostScript { body, ps_domain } => {
                ps::eval1(body, t, [ps_domain[0], ps_domain[1]])
            }
            other => Ok(other.eval(t)),
        }
    }

    /// Evaluate the 2-input function at `(t0, t1)` — PostScript variant only.
    ///
    /// Used by function-based shadings (Type 1) and mesh /Function overrides.
    pub fn eval2(&self, t0: f64, t1: f64) -> std::result::Result<Vec<f64>, String> {
        match self {
            ShadingFunction::PostScript { body, ps_domain } => ps::eval2(body, t0, t1, *ps_domain),
            _ => Err("eval2 requires a FunctionType 4 (PostScript) function".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Shading types
// ---------------------------------------------------------------------------

/// Axial (linear) shading — ShadingType 2.
#[derive(Debug, Clone)]
pub struct AxialShading {
    /// Axis start point in shading space.
    pub x0: f64,
    pub y0: f64,
    /// Axis end point in shading space.
    pub x1: f64,
    pub y1: f64,
    /// Domain [t0, t1] mapped to the axis endpoints.
    pub domain: [f64; 2],
    /// Color function.
    pub function: ShadingFunction,
    /// Whether to extend the shading beyond each axis endpoint.
    pub extend: [bool; 2],
    /// Number of color components (1=gray, 3=rgb, 4=cmyk).
    pub n_components: usize,
}

/// Radial shading — ShadingType 3.
#[derive(Debug, Clone)]
pub struct RadialShading {
    pub x0: f64,
    pub y0: f64,
    pub r0: f64,
    pub x1: f64,
    pub y1: f64,
    pub r1: f64,
    pub domain: [f64; 2],
    pub function: ShadingFunction,
    pub extend: [bool; 2],
    pub n_components: usize,
}

/// A parsed shading ready for rasterisation.
#[derive(Debug, Clone)]
pub enum Shading {
    Axial(AxialShading),
    Radial(RadialShading),
    /// ShadingType 1 — per-pixel 2-input function (see `function_shaded.rs`).
    FunctionBased(FunctionShading),
    /// ShadingType 4/5 — free-form/lattice Gouraud meshes (`mesh.rs`) and
    /// ShadingType 6/7 — Coons/tensor patch meshes (`patch.rs`); both decode
    /// into the same vertex/triangle lattice.
    Mesh(MeshShading),
}

impl Shading {
    /// Parse a PDF Shading object — the Shading resource value (dictionary)
    /// or an indirect stream (Types 4–7 or any stream-backed shading).
    pub fn parse(obj: &PdfObject, doc: &PdfDocument) -> Result<Self> {
        match obj {
            PdfObject::Reference(..) => {
                let resolved = doc.resolve(obj)?;
                Self::parse(&resolved, doc)
            }
            PdfObject::Stream(s) => {
                let data = s.decode().map_err(|e| {
                    PdfError::invalid_token(0, format!("shading stream decode failed: {}", e))
                })?;
                Self::parse_shading_dict(&s.dict, doc, data)
            }
            PdfObject::Dictionary(d) => Self::parse_shading_dict(d, doc, Vec::new()),
            other => Err(PdfError::invalid_token(
                0,
                format!("expected shading dict or stream, got {:?}", other),
            )),
        }
    }

    /// Parse the shading fields given the (fully decoded) stream payload for
    /// mesh types, or empty bytes for dict-only shadings.
    fn parse_shading_dict(dict: &PdfDict, doc: &PdfDocument, data: Vec<u8>) -> Result<Self> {
        let shading_type = dict.get("ShadingType").and_then(pdf_int).unwrap_or(0);

        let domain = {
            let arr = dict_f64_array(dict, "Domain").unwrap_or_else(|| vec![0.0, 1.0]);
            [
                arr.first().copied().unwrap_or(0.0),
                arr.get(1).copied().unwrap_or(1.0),
            ]
        };

        let extend = {
            let arr = match dict.get("Extend") {
                Some(PdfObject::Array(a)) => a.clone(),
                _ => vec![],
            };
            let e0 = matches!(arr.first(), Some(PdfObject::Boolean(true)));
            let e1 = matches!(arr.get(1), Some(PdfObject::Boolean(true)));
            [e0, e1]
        };

        // Infer component count from color space name.
        let n_components = cs_n_components(dict.get("ColorSpace"));

        let fn_obj = dict.get("Function");
        // /Function is required for Types 1–3; for mesh Types 4–7 it is an
        // optional override (§8.7.4.4: when present, each colour record
        // carries a single t value expanded through the function).
        let function = match fn_obj {
            Some(o) => Some(ShadingFunction::parse(o, doc)?),
            None => None,
        };

        match shading_type {
            2 => {
                let function = function
                    .ok_or_else(|| PdfError::invalid_token(0, "shading missing /Function"))?;
                let coords = dict_f64_array(dict, "Coords")
                    .ok_or_else(|| PdfError::invalid_token(0, "axial shading missing /Coords"))?;
                if coords.len() < 4 {
                    return Err(PdfError::invalid_token(
                        0,
                        "axial shading Coords needs 4 values",
                    ));
                }
                Ok(Shading::Axial(AxialShading {
                    x0: coords[0],
                    y0: coords[1],
                    x1: coords[2],
                    y1: coords[3],
                    domain,
                    function,
                    extend,
                    n_components,
                }))
            }
            3 => {
                let function = function
                    .ok_or_else(|| PdfError::invalid_token(0, "shading missing /Function"))?;
                let coords = dict_f64_array(dict, "Coords")
                    .ok_or_else(|| PdfError::invalid_token(0, "radial shading missing /Coords"))?;
                if coords.len() < 6 {
                    return Err(PdfError::invalid_token(
                        0,
                        "radial shading Coords needs 6 values",
                    ));
                }
                Ok(Shading::Radial(RadialShading {
                    x0: coords[0],
                    y0: coords[1],
                    r0: coords[2],
                    x1: coords[3],
                    y1: coords[4],
                    r1: coords[5],
                    domain,
                    function,
                    extend,
                    n_components,
                }))
            }
            1 => {
                let function = function
                    .ok_or_else(|| PdfError::invalid_token(0, "shading missing /Function"))?;
                Ok(Shading::FunctionBased(FunctionShading::parse(
                    dict,
                    function,
                    n_components,
                )?))
            }
            4..=7 => {
                if data.is_empty() {
                    return Err(PdfError::invalid_token(
                        0,
                        "mesh shading (types 4–7) requires a stream object with data",
                    ));
                }
                let mesh = match shading_type {
                    6 | 7 => patch::parse(dict, &data, shading_type as u32, function.as_ref())?,
                    _ => mesh::parse(dict, &data, shading_type as u32, function.as_ref())?,
                };
                Ok(Shading::Mesh(mesh))
            }
            other => Err(PdfError::invalid_token(
                0,
                format!(
                    "unsupported ShadingType {} (types 1–7 supported; 0 sampled not implemented)",
                    other
                ),
            )),
        }
    }

    /// Rasterise this shading into the canvas using the given CTM (user space → pixel space).
    pub fn rasterize(&self, ctm: &Matrix, canvas: &mut PixmapBuffer) {
        match self {
            Shading::Axial(s) => rasterize_axial(s, ctm, canvas),
            Shading::Radial(s) => rasterize_radial(s, ctm, canvas),
            Shading::FunctionBased(s) => s.rasterize(ctm, canvas),
            Shading::Mesh(m) => m.rasterize(ctm, canvas),
        }
    }
}

// ---------------------------------------------------------------------------
// Rasterisation helpers
// ---------------------------------------------------------------------------

/// Compute the inverse of a 2D affine matrix [a b c d e f].
pub(crate) fn inverse_matrix(m: &Matrix) -> Option<Matrix> {
    let det = m.a * m.d - m.b * m.c;
    if det.abs() < 1e-10 {
        return None;
    }
    Some(Matrix {
        a: m.d / det,
        b: -m.b / det,
        c: -m.c / det,
        d: m.a / det,
        e: (m.c * m.f - m.d * m.e) / det,
        f: (m.b * m.e - m.a * m.f) / det,
    })
}

/// Convert shading color components to RGBA bytes.
pub(crate) fn components_to_rgba(comps: &[f64], n: usize) -> [u8; 4] {
    let to_u8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0) as u8;
    match n {
        1 => {
            let v = to_u8(comps.first().copied().unwrap_or(0.0));
            [v, v, v, 255]
        }
        3 => [
            to_u8(comps.first().copied().unwrap_or(0.0)),
            to_u8(comps.get(1).copied().unwrap_or(0.0)),
            to_u8(comps.get(2).copied().unwrap_or(0.0)),
            255,
        ],
        4 => {
            // CMYK → RGB
            let c = comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let m = comps.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let y = comps.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let k = comps.get(3).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            [
                to_u8((1.0 - c) * (1.0 - k)),
                to_u8((1.0 - m) * (1.0 - k)),
                to_u8((1.0 - y) * (1.0 - k)),
                255,
            ]
        }
        _ => [128, 128, 128, 255],
    }
}

fn rasterize_axial(s: &AxialShading, ctm: &Matrix, canvas: &mut PixmapBuffer) {
    let inv = match inverse_matrix(ctm) {
        Some(m) => m,
        None => return,
    };

    let ax = s.x1 - s.x0;
    let ay = s.y1 - s.y0;
    let axis_len2 = ax * ax + ay * ay;
    if axis_len2 < 1e-10 {
        return;
    }

    let w = canvas.width as i32;
    let h = canvas.height as i32;
    let ox = canvas.origin.x as f64;
    let oy = canvas.origin.y as f64;

    // t(px, py) is linear in canvas-local pixel coordinates:
    //   t(px, py) = t00 + dt_dx*px + dt_dy*py
    // where px,py ∈ [0, w) × [0, h).
    let dt_dx = (ax * inv.a + ay * inv.b) / axis_len2;
    let dt_dy = (ax * inv.c + ay * inv.d) / axis_len2;
    let t00 = (ax * (inv.a * ox + inv.c * oy + inv.e - s.x0)
        + ay * (inv.b * ox + inv.d * oy + inv.f - s.y0))
        / axis_len2;

    // For non-extending gradients, skip rows whose t-range is entirely
    // outside [0, 1], avoiding full-canvas allocation and iteration.
    let (start_row, end_row) = if !s.extend[0] && !s.extend[1] {
        // Per-row t-range: [t00 + dt_dy*py + col_lo, t00 + dt_dy*py + col_hi]
        let col_hi = (dt_dx * (w - 1) as f64).max(0.0);
        let col_lo = (dt_dx * (w - 1) as f64).min(0.0);

        // Active when: max_t_in_row >= 0  AND  min_t_in_row <= 1
        let (r0, r1) = if dt_dy.abs() < 1e-12 {
            if t00 + col_hi >= 0.0 && t00 + col_lo <= 1.0 {
                (0i32, h - 1)
            } else {
                (0i32, -1i32) // gradient entirely outside this tile
            }
        } else if dt_dy > 0.0 {
            let lo = ((-t00 - col_hi) / dt_dy).ceil() as i32;
            let hi = ((1.0 - t00 - col_lo) / dt_dy).floor() as i32;
            (lo.max(0), hi.min(h - 1))
        } else {
            // dt_dy < 0: t increases as py decreases
            let lo = ((1.0 - t00 - col_lo) / dt_dy).ceil() as i32;
            let hi = ((-t00 - col_hi) / dt_dy).floor() as i32;
            (lo.max(0), hi.min(h - 1))
        };
        (r0, r1)
    } else {
        (0i32, h - 1)
    };

    if start_row > end_row {
        return; // gradient entirely outside this canvas region
    }

    let active_rows = (end_row - start_row + 1) as u32;
    let mut rgba_buf = vec![0u8; w as usize * active_rows as usize * 4];

    for py in start_row..=end_row {
        let buf_py = py - start_row;
        let t_row_base = t00 + dt_dy * py as f64;
        let mut t_raw = t_row_base;
        for px in 0..w {
            let t_opt = if t_raw < 0.0 {
                if s.extend[0] {
                    Some(0.0f64)
                } else {
                    None
                }
            } else if t_raw > 1.0 {
                if s.extend[1] {
                    Some(1.0f64)
                } else {
                    None
                }
            } else {
                Some(t_raw)
            };

            if let Some(t) = t_opt {
                let t_d = s.domain[0] + t * (s.domain[1] - s.domain[0]);
                let comps = s.function.eval(t_d);
                let pixel = components_to_rgba(&comps, s.n_components);
                let idx = (buf_py * w + px) as usize * 4;
                rgba_buf[idx] = pixel[0];
                rgba_buf[idx + 1] = pixel[1];
                rgba_buf[idx + 2] = pixel[2];
                rgba_buf[idx + 3] = pixel[3];
            }
            t_raw += dt_dx;
        }
    }

    canvas.blit_rgba(
        canvas.origin.x as i32,
        canvas.origin.y as i32 + start_row,
        &rgba_buf,
        w as u32,
        active_rows,
    );
}

fn rasterize_radial(s: &RadialShading, ctm: &Matrix, canvas: &mut PixmapBuffer) {
    let inv = match inverse_matrix(ctm) {
        Some(m) => m,
        None => return,
    };

    let w = canvas.width as i32;
    let h = canvas.height as i32;

    // Clip row iteration to the device-space bounding box of both circles
    // when neither extend flag is set.  The CTM scale approximation is
    // conservative: uses the larger of the two axis scale factors.
    let (start_row, end_row) = if !s.extend[0] && !s.extend[1] {
        let scale_x = (ctm.a * ctm.a + ctm.b * ctm.b).sqrt();
        let scale_y = (ctm.c * ctm.c + ctm.d * ctm.d).sqrt();
        let scale = scale_x.max(scale_y);

        let (_, dc0y) = ctm.transform_point(s.x0, s.y0);
        let (_, dc1y) = ctm.transform_point(s.x1, s.y1);
        let dr0 = s.r0 * scale;
        let dr1 = s.r1 * scale;

        let y_min = (dc0y - dr0).min(dc1y - dr1) - canvas.origin.y as f64;
        let y_max = (dc0y + dr0).max(dc1y + dr1) - canvas.origin.y as f64;

        let r0 = (y_min.floor() as i32).max(0);
        let r1 = (y_max.ceil() as i32).min(h - 1);
        if r0 > r1 {
            return; // gradient bounding box entirely outside this tile
        }
        (r0, r1)
    } else {
        (0i32, h - 1)
    };

    let active_rows = (end_row - start_row + 1) as u32;
    let mut rgba_buf = vec![0u8; w as usize * active_rows as usize * 4];

    let dx = s.x1 - s.x0;
    let dy = s.y1 - s.y0;
    let dr = s.r1 - s.r0;

    for py in start_row..=end_row {
        let buf_py = py - start_row;
        for px in 0..w {
            let (ux, uy) = inv.transform_point(
                px as f64 + canvas.origin.x as f64,
                py as f64 + canvas.origin.y as f64,
            );

            // Solve quadratic for t: point (ux,uy) is on circle at parameter t.
            // (x(t)-ux)^2 + (y(t)-uy)^2 = r(t)^2
            // where x(t) = x0+t*dx, y(t) = y0+t*dy, r(t) = r0+t*dr
            let ex = s.x0 - ux;
            let ey = s.y0 - uy;

            let a = dx * dx + dy * dy - dr * dr;
            let b = 2.0 * (ex * dx + ey * dy + s.r0 * dr);
            let c = ex * ex + ey * ey - s.r0 * s.r0;

            let t = if a.abs() < 1e-10 {
                // Linear case.
                if b.abs() < 1e-10 {
                    continue;
                }
                -c / b
            } else {
                let disc = b * b - 4.0 * a * c;
                if disc < 0.0 {
                    continue;
                }
                let sqrt_d = disc.sqrt();
                let t1 = (-b + sqrt_d) / (2.0 * a);
                let t2 = (-b - sqrt_d) / (2.0 * a);
                // Choose the largest t with r(t) >= 0 (outermost visible circle).
                let valid = |t: f64| s.r0 + t * dr >= 0.0;
                match (valid(t1), valid(t2)) {
                    (true, true) => t1.max(t2),
                    (true, false) => t1,
                    (false, true) => t2,
                    _ => continue,
                }
            };

            let t_clamped = if t < 0.0 {
                if s.extend[0] {
                    0.0
                } else {
                    continue;
                }
            } else if t > 1.0 {
                if s.extend[1] {
                    1.0
                } else {
                    continue;
                }
            } else {
                t
            };

            let t_d = s.domain[0] + t_clamped * (s.domain[1] - s.domain[0]);
            let comps = s.function.eval(t_d);
            let pixel = components_to_rgba(&comps, s.n_components);

            let idx = (buf_py * w + px) as usize * 4;
            rgba_buf[idx] = pixel[0];
            rgba_buf[idx + 1] = pixel[1];
            rgba_buf[idx + 2] = pixel[2];
            rgba_buf[idx + 3] = pixel[3];
        }
    }

    canvas.blit_rgba(
        canvas.origin.x as i32,
        canvas.origin.y as i32 + start_row,
        &rgba_buf,
        w as u32,
        active_rows,
    );
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn pdf_int(obj: &PdfObject) -> Option<i64> {
    match obj {
        PdfObject::Integer(n) => Some(*n),
        PdfObject::Real(r) => Some(*r as i64),
        _ => None,
    }
}

fn obj_f64(obj: &PdfObject) -> Option<f64> {
    match obj {
        PdfObject::Integer(n) => Some(*n as f64),
        PdfObject::Real(r) => Some(*r),
        _ => None,
    }
}

fn dict_f64_array(dict: &PdfDict, key: &str) -> Option<Vec<f64>> {
    match dict.get(key)? {
        PdfObject::Array(arr) => {
            let v: Vec<f64> = arr.iter().filter_map(obj_f64).collect();
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        }
        other => obj_f64(other).map(|v| vec![v]),
    }
}

pub(crate) fn cs_n_components(cs_obj: Option<&PdfObject>) -> usize {
    match cs_obj {
        Some(PdfObject::Name(n)) => match n.as_str() {
            "DeviceGray" | "CalGray" => 1,
            "DeviceCMYK" => 4,
            _ => 3,
        },
        Some(PdfObject::Array(arr)) => {
            match arr.first().and_then(|o| o.as_name()) {
                Some("DeviceGray") | Some("CalGray") => 1,
                Some("DeviceCMYK") => 4,
                Some("ICCBased") => 3, // assume sRGB; could read /N but avoid doc access here
                _ => 3,
            }
        }
        _ => 3,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_linear() {
        let f = ShadingFunction::Exponential {
            c0: vec![0.0, 0.0, 1.0],
            c1: vec![1.0, 0.0, 0.0],
            n: 1.0,
        };
        // t=0 → C0 = [0, 0, 1]
        let at_0 = f.eval(0.0);
        assert!((at_0[0] - 0.0).abs() < 1e-6);
        assert!((at_0[2] - 1.0).abs() < 1e-6);
        // t=1 → C1 = [1, 0, 0]
        let at_1 = f.eval(1.0);
        assert!((at_1[0] - 1.0).abs() < 1e-6);
        assert!((at_1[2] - 0.0).abs() < 1e-6);
        // t=0.5 → midpoint
        let at_half = f.eval(0.5);
        assert!((at_half[0] - 0.5).abs() < 1e-6);
        assert!((at_half[2] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_exponential_squared() {
        // n=2 → quadratic interpolation
        let f = ShadingFunction::Exponential {
            c0: vec![0.0],
            c1: vec![1.0],
            n: 2.0,
        };
        let at_half = f.eval(0.5);
        assert!((at_half[0] - 0.25).abs() < 1e-6); // 0.5^2 = 0.25
    }

    #[test]
    fn test_components_to_rgba_rgb() {
        let rgba = components_to_rgba(&[1.0, 0.0, 0.0], 3);
        assert_eq!(rgba, [255, 0, 0, 255]);
    }

    #[test]
    fn test_components_to_rgba_gray() {
        let rgba = components_to_rgba(&[0.5], 1);
        assert_eq!(rgba[0], rgba[1]);
        assert_eq!(rgba[1], rgba[2]);
        assert_eq!(rgba[0], 127);
    }

    #[test]
    fn test_components_to_rgba_cmyk_white() {
        let rgba = components_to_rgba(&[0.0, 0.0, 0.0, 0.0], 4);
        assert_eq!(rgba, [255, 255, 255, 255]);
    }

    #[test]
    fn test_inverse_matrix_identity() {
        let m = Matrix::identity();
        let inv = inverse_matrix(&m).unwrap();
        assert!((inv.a - 1.0).abs() < 1e-10);
        assert!((inv.d - 1.0).abs() < 1e-10);
        assert!(inv.b.abs() < 1e-10);
        assert!(inv.e.abs() < 1e-10);
    }

    #[test]
    fn test_inverse_matrix_scale() {
        let m = Matrix {
            a: 2.0,
            b: 0.0,
            c: 0.0,
            d: 2.0,
            e: 0.0,
            f: 0.0,
        };
        let inv = inverse_matrix(&m).unwrap();
        assert!((inv.a - 0.5).abs() < 1e-10);
        assert!((inv.d - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_axial_shading_rasterize_paints_canvas() {
        use super::super::canvas::PixmapBuffer;
        let shading = Shading::Axial(AxialShading {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 0.0,
            domain: [0.0, 1.0],
            function: ShadingFunction::Exponential {
                c0: vec![1.0, 0.0, 0.0],
                c1: vec![0.0, 0.0, 1.0],
                n: 1.0,
            },
            extend: [true, true],
            n_components: 3,
        });
        let ctm = Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        };
        let mut canvas = PixmapBuffer::new(4, 1).unwrap();
        shading.rasterize(&ctm, &mut canvas);
        // Canvas should no longer be all-white — some red or blue pixels.
        let data = canvas.data();
        // The first pixel (x=0, user x≈0) should be near red [255, 0, 0, 255].
        assert!(data[0] > 200, "first pixel R should be high (red end)");
        // The last pixel (x=3, user x≈3) should have more blue.
        assert!(
            data[14] > data[2],
            "last pixel B should be higher than first pixel B"
        );
    }
}
