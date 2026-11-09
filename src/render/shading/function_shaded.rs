//! Function-based shading — ISO 32000-1 §8.7.4.3 (ShadingType 1).
//!
//! Color comes from a 2-input function evaluated at each pixel of the
//! shading space; only the PostScript calculator function type (Type 4) is
//! wired up here — see `ps.rs`. Types 6/7 (Coons/tensor) patches live in
//! `mesh.rs` (deferred).

use crate::content::graphics_state::Matrix;
use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};

use super::{components_to_rgba, inverse_matrix, ShadingFunction};
use crate::render::canvas::PixmapBuffer;

/// A parsed function-based shading.
#[derive(Debug, Clone)]
pub struct FunctionShading {
    /// `(x_lo, x_hi, y_lo, y_hi)` in shading space; inputs outside the box
    /// are not painted.
    pub domain: [f64; 4],
    /// Shading-space → user-space matrix (§8.7.4.3), identity when absent.
    pub matrix: Matrix,
    /// Two-input function (FunctionType 4).
    pub function: ShadingFunction,
    /// Number of color components the function yields (1/3/4).
    pub n_components: usize,
}

impl FunctionShading {
    /// Parse the shading-typed fields of a Type 1 shading dictionary.
    ///
    /// The caller resolves the stream/dict; `function` is parsed by
    /// `super::shading::ShadingFunction::parse`. Returns an error when the
    /// attached function is not a 2-input PostScript calculator body.
    pub fn parse(dict: &PdfDict, function: ShadingFunction, n_components: usize) -> Result<Self> {
        let domain = {
            match dict.get("Domain") {
                Some(PdfObject::Array(arr)) => {
                    let vals: Vec<f64> = arr
                        .iter()
                        .filter_map(|o| match o {
                            PdfObject::Integer(i) => Some(*i as f64),
                            PdfObject::Real(r) => Some(*r),
                            _ => None,
                        })
                        .collect();
                    if vals.len() == 4 {
                        [vals[0], vals[1], vals[2], vals[3]]
                    } else {
                        return Err(PdfError::invalid_token(
                            0,
                            "type-1 shading /Domain needs 4 values",
                        ));
                    }
                }
                _ => [0.0, 1.0, 0.0, 1.0],
            }
        };

        let matrix = match dict.get("Matrix") {
            Some(PdfObject::Array(arr)) => {
                let vals: Vec<f64> = arr
                    .iter()
                    .filter_map(|o| match o {
                        PdfObject::Integer(i) => Some(*i as f64),
                        PdfObject::Real(r) => Some(*r),
                        _ => None,
                    })
                    .collect();
                if vals.len() == 6 {
                    Matrix {
                        a: vals[0],
                        b: vals[1],
                        c: vals[2],
                        d: vals[3],
                        e: vals[4],
                        f: vals[5],
                    }
                } else {
                    return Err(PdfError::invalid_token(
                        0,
                        "type-1 shading /Matrix needs 6 values",
                    ));
                }
            }
            _ => Matrix::default(),
        };

        if !matches!(function, ShadingFunction::PostScript { .. }) {
            return Err(PdfError::invalid_token(
                0,
                "type-1 shading requires a 2-input function (FunctionType 4)",
            ));
        }
        Ok(FunctionShading {
            domain,
            matrix,
            function,
            n_components,
        })
    }

    /// Rasterize: for every tile pixel, invert `ctm ∘ /Matrix` into shading
    /// space, normalize by /Domain, then evaluate the function.
    pub fn rasterize(&self, ctm: &Matrix, canvas: &mut PixmapBuffer) {
        let total = ctm.concat(&self.matrix);
        let Some(inv) = inverse_matrix(&total) else {
            return;
        };

        let w = canvas.width as i32;
        let h = canvas.height as i32;
        let ox = canvas.origin.x as f64;
        let oy = canvas.origin.y as f64;

        // Device-space bounding box of the shading domain (conservative).
        let corners_node = [
            (self.domain[0], self.domain[2]),
            (self.domain[1], self.domain[2]),
            (self.domain[0], self.domain[3]),
            (self.domain[1], self.domain[3]),
        ];
        let (mut x_min, mut y_min, mut x_max, mut y_max) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for (sx, sy) in corners_node {
            let (dx, dy) = total.transform_point(sx, sy);
            x_min = x_min.min((dx + ox).floor() as i32);
            y_min = y_min.min((dy + oy).floor() as i32);
            x_max = x_max.max((dx + ox).ceil() as i32);
            y_max = y_max.max((dy + oy).ceil() as i32);
        }
        x_min = x_min.max(0);
        y_min = y_min.max(0);
        x_max = x_max.min(w - 1);
        y_max = y_max.min(h - 1);
        if x_min > x_max || y_min > y_max {
            return; // domain entirely outside this canvas region
        }

        let buf_w = (x_max - x_min + 1) as usize;
        let buf_h = (y_max - y_min + 1) as usize;
        let mut rgba_buf = vec![0u8; buf_w * buf_h * 4];

        let dxd = self.domain[1] - self.domain[0];
        let dyd = self.domain[3] - self.domain[2];
        if dxd.abs() < 1e-9 || dyd.abs() < 1e-9 {
            return;
        }

        for py in y_min..=y_max {
            for px in x_min..=x_max {
                let (sx, sy) = inv.transform_point(px as f64 + 0.5 - ox, py as f64 + 0.5 - oy);
                // Normalize into [0, 1] over the shading domain.
                let xn = ((sx - self.domain[0]) / dxd).clamp(0.0, 1.0);
                let yn = ((sy - self.domain[2]) / dyd).clamp(0.0, 1.0);

                // §8.7.4.2: the function is invoked with the *raw* shading-space
                // coordinate pair; its own /Domain decides normalization, and
                // the function result feeds the color.
                let comps = match &self.function {
                    ShadingFunction::PostScript { ps_domain, .. } => {
                        // Re-normalize from shading domain into fn domain:
                        let tx = ps_domain[0] + xn * (ps_domain[1] - ps_domain[0]);
                        let ty = ps_domain[2] + yn * (ps_domain[3] - ps_domain[2]);
                        match self.function.eval2(tx, ty) {
                            Ok(v) => v,
                            Err(e) => {
                                log::warn!("type-1 shading function eval failed: {}", e);
                                return;
                            }
                        }
                    }
                    _ => continue,
                };
                let rgba = components_to_rgba(&comps, self.n_components);
                let i = ((py - y_min) as usize * buf_w + (px - x_min) as usize) * 4;
                rgba_buf[i] = rgba[0];
                rgba_buf[i + 1] = rgba[1];
                rgba_buf[i + 2] = rgba[2];
                rgba_buf[i + 3] = rgba[3];
            }
        }

        canvas.blit_rgba(x_min, y_min, &rgba_buf, buf_w as u32, buf_h as u32);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::objects::PdfObject;

    #[test]
    fn parse_rejects_non_postscript_function() {
        let dict = PdfDict::new();
        let f = ShadingFunction::Exponential {
            c0: vec![0.0],
            c1: vec![1.0],
            n: 1.0,
        };
        assert!(FunctionShading::parse(&dict, f, 1).is_err());
    }

    #[test]
    fn domain_default_unit_box() {
        let dict = PdfDict::new();
        let f = ShadingFunction::PostScript {
            body: b"dup".to_vec(),
            ps_domain: [0.0, 1.0, 0.0, 1.0],
        };
        let fs = FunctionShading::parse(&dict, f, 1).expect("parses with defaults");
        assert_eq!(fs.domain, [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(fs.matrix, Matrix::default());
    }

    #[test]
    fn parse_reads_matrix_and_domain() {
        let mut dict = PdfDict::new();
        dict.insert(
            "Domain".to_string(),
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(10),
                PdfObject::Integer(0),
                PdfObject::Integer(20),
            ]),
        );
        dict.insert(
            "Matrix".to_string(),
            PdfObject::Array(vec![
                PdfObject::Integer(2),
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                PdfObject::Integer(2),
                PdfObject::Integer(0),
                PdfObject::Integer(0),
            ]),
        );
        let f = ShadingFunction::PostScript {
            body: b"dup".to_vec(),
            ps_domain: [0.0, 1.0, 0.0, 1.0],
        };
        let fs = FunctionShading::parse(&dict, f, 1).expect("parses arrays");
        assert_eq!(fs.domain, [0.0, 10.0, 0.0, 20.0]);
        assert_eq!(fs.matrix.a, 2.0);
    }
}
