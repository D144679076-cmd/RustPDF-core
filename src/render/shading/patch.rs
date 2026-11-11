//! Coons-patch (ShadingType 6) and tensor-patch (ShadingType 7) mesh
//! shading — ISO 32000-1 §8.7.4.6 / §8.7.4.7.
//!
//! Port of pdf.js v3.11.174 `src/core/pattern.js` `MeshShading`
//! (`_decodeType6Shading` / `_decodeType7Shading` / `_buildFigureFromPatch`).
//! Each patch is decoded through a flag machine (flags 0–3, where a non-zero
//! flag shares edges/colours with the previous patch), then converted into a
//! lattice of Gouraud triangles by evaluating the bicubic surface (Coons is
//! the special case where the interior control points p11/p12/p21/p22 are
//! derived from the edges) and bilinearly blending the four corner colours.
//!
//! Triangle density per figure follows the reference: the shading-space
//! extent of the patch relative to the whole mesh bounds scales a target of
//! 20 chunks per axis, clamped to `[3, 20]` (reference `TRIANGLE_DENSITY`,
//! `MIN/MAX_SPLIT_PATCH_CHUNKS_AMOUNT`).
//!
//! Divergence from the reference (deliberate): pdf.js does not byte-align
//! patch records; ISO 32000-1 §8.7.4.6 requires each patch to be
//! byte-aligned, and MuPDF (our golden master) follows the spec — we align.
//!
//! Pure Rust, no new deps (R2); no panic paths (R1).

use crate::error::{PdfError, Result};
use crate::parser::objects::PdfDict;

use super::mesh::{decode_int, BitReader, MeshDecode, MeshShading, Vertex};
use super::ShadingFunction;

/// One decoded patch: 16 control-point indices (ps — `q = 4·i + j`, `i` the
/// first Bernstein parameter, `j` the second) and 4 corner-colour indices
/// (cs — c00, c30, c03, c33).
struct Patch {
    ps: [usize; 16],
    cs: [usize; 4],
}

/// `figures.last()` or an error — chained patches (flags 1–3) require a
/// preceding flag-0 patch.
fn last_patch(figures: &[Patch]) -> Result<&Patch> {
    figures
        .last()
        .ok_or_else(|| PdfError::invalid_token(0, "chained patch without a flag-0 patch"))
}

/// `coords[ps[i]]` as a free fn — the Coons interior-point formulas index
/// `coords` repeatedly while new points are being pushed, so closures are
/// avoided (borrow checker).
fn pt(coords: &[(f64, f64)], ps: &[usize; 16], i: usize) -> (f64, f64) {
    coords[ps[i]]
}

/// Parse a Type 6 / Type 7 patch mesh from already-decoded stream bytes into
/// a triangulated lattice ([`MeshShading`]) — the same shape `mesh.rs`
/// produces for Types 4/5, so the shared rasteriser handles all mesh types.
pub fn parse(
    dict: &PdfDict,
    data: &[u8],
    shading_type: u32,
    function: Option<&ShadingFunction>,
) -> Result<MeshShading> {
    let bits_flag = decode_int(dict, "BitsPerFlag", 2, 8)?;
    let coords_flag0 = match shading_type {
        6 => 12,
        7 => 16,
        other => {
            return Err(PdfError::invalid_token(
                0,
                format!("patch module handles types 6/7, got {}", other),
            ))
        }
    };
    let mesh_ctx = MeshDecode::from_dict(dict, function.cloned())?;
    let n_components = mesh_ctx.n_components;

    let mut reader = BitReader::new(data);
    let mut coords: Vec<(f64, f64)> = Vec::new();
    let mut colors: Vec<Vec<f64>> = Vec::new();
    let mut figures: Vec<Patch> = Vec::new();

    while reader.bit_pos < reader.len_bits() {
        let f = reader.read_bits(bits_flag)?;
        if f > 3 {
            return Err(PdfError::invalid_token(
                0,
                format!("unknown type-{shading_type} flag {f}"),
            ));
        }
        let pi = coords.len();
        // Flag 0 patches carry all control points and colours; a chained
        // patch (flag 1–3) reuses one shared edge, dropping the last 4
        // points and 2 colours (reference loop bounds `f !== 0 ? N-4 : N`).
        let n_coords = if f != 0 {
            coords_flag0 - 4
        } else {
            coords_flag0
        };
        for _ in 0..n_coords {
            coords.push(mesh_ctx.read_coord(&mut reader)?);
        }
        let ci = colors.len();
        let n_colors = if f != 0 { 2 } else { 4 };
        for _ in 0..n_colors {
            colors.push(mesh_ctx.read_comps(&mut reader)?);
        }

        let mut ps = [0usize; 16];
        let mut cs = [0usize; 4];
        match f {
            0 => {
                // Reference flag-0 map (12/16 stream points → ps[] slots).
                ps[12] = pi + 3;
                ps[13] = pi + 4;
                ps[14] = pi + 5;
                ps[15] = pi + 6;
                ps[8] = pi + 2;
                ps[11] = pi + 7;
                ps[4] = pi + 1;
                ps[7] = pi + 8;
                ps[0] = pi;
                ps[1] = pi + 11;
                ps[2] = pi + 10;
                ps[3] = pi + 9;
                if shading_type == 7 {
                    ps[9] = pi + 13;
                    ps[10] = pi + 14;
                    ps[5] = pi + 12;
                    ps[6] = pi + 15;
                }
                cs[2] = ci + 1;
                cs[3] = ci + 2;
                cs[0] = ci;
                cs[1] = ci + 3;
            }
            1 => {
                // Shares the previous patch's p03…p33 edge.
                let prev = last_patch(&figures)?;
                let (tmp1, tmp2, tmp3, tmp4) = (prev.ps[12], prev.ps[13], prev.ps[14], prev.ps[15]);
                ps[12] = tmp4;
                ps[13] = pi;
                ps[14] = pi + 1;
                ps[15] = pi + 2;
                ps[8] = tmp3;
                ps[11] = pi + 3;
                ps[4] = tmp2;
                ps[7] = pi + 4;
                ps[0] = tmp1;
                ps[1] = pi + 7;
                ps[2] = pi + 6;
                ps[3] = pi + 5;
                if shading_type == 7 {
                    ps[9] = pi + 9;
                    ps[10] = pi + 10;
                    ps[5] = pi + 8;
                    ps[6] = pi + 11;
                }
                let (ct1, ct2) = (prev.cs[2], prev.cs[3]);
                cs[2] = ct2;
                cs[3] = ci;
                cs[0] = ct1;
                cs[1] = ci + 1;
            }
            2 => {
                // Shares the previous patch's p30…p33 edge.
                let prev = last_patch(&figures)?;
                let tmp1 = prev.ps[15];
                let tmp2 = prev.ps[11];
                ps[12] = prev.ps[3];
                ps[13] = pi;
                ps[14] = pi + 1;
                ps[15] = pi + 2;
                ps[8] = prev.ps[7];
                ps[11] = pi + 3;
                ps[4] = tmp2;
                ps[7] = pi + 4;
                ps[0] = tmp1;
                ps[1] = pi + 7;
                ps[2] = pi + 6;
                ps[3] = pi + 5;
                if shading_type == 7 {
                    ps[9] = pi + 9;
                    ps[10] = pi + 10;
                    ps[5] = pi + 8;
                    ps[6] = pi + 11;
                }
                let ct1 = prev.cs[3];
                cs[2] = prev.cs[1];
                cs[3] = ci;
                cs[0] = ct1;
                cs[1] = ci + 1;
            }
            _ => {
                // Flag 3 — shares the previous patch's p33…p00 edge.
                let prev = last_patch(&figures)?;
                ps[12] = prev.ps[0];
                ps[13] = pi;
                ps[14] = pi + 1;
                ps[15] = pi + 2;
                ps[8] = prev.ps[1];
                ps[11] = pi + 3;
                ps[4] = prev.ps[2];
                ps[7] = pi + 4;
                ps[0] = prev.ps[3];
                ps[1] = pi + 7;
                ps[2] = pi + 6;
                ps[3] = pi + 5;
                if shading_type == 7 {
                    ps[9] = pi + 9;
                    ps[10] = pi + 10;
                    ps[5] = pi + 8;
                    ps[6] = pi + 11;
                }
                cs[2] = prev.cs[0];
                cs[3] = ci;
                cs[0] = prev.cs[1];
                cs[1] = ci + 1;
            }
        }

        if shading_type == 6 {
            // Coons: derive interior control points p11, p12, p21, p22
            // (ps[5], ps[6], ps[9], ps[10]) from the boundary — reference
            // formulas, exactly. All four depend only on boundary points,
            // so compute all coordinates first, then push (borrow checker).
            let (c0, c15) = (pt(&coords, &ps, 0), pt(&coords, &ps, 15));
            let (c3, c12) = (pt(&coords, &ps, 3), pt(&coords, &ps, 12));
            let (c1, c4) = (pt(&coords, &ps, 1), pt(&coords, &ps, 4));
            let (c7, c2) = (pt(&coords, &ps, 7), pt(&coords, &ps, 2));
            let (c13, c14) = (pt(&coords, &ps, 13), pt(&coords, &ps, 14));
            let (c8, c11) = (pt(&coords, &ps, 8), pt(&coords, &ps, 11));

            // p11 (ps[5])
            let p11 = (
                (-4.0 * c0.0 - c15.0 + 6.0 * (c4.0 + c1.0) - 2.0 * (c12.0 + c3.0)
                    + 3.0 * (c13.0 + c7.0))
                    / 9.0,
                (-4.0 * c0.1 - c15.1 + 6.0 * (c4.1 + c1.1) - 2.0 * (c12.1 + c3.1)
                    + 3.0 * (c13.1 + c7.1))
                    / 9.0,
            );
            // p12 (ps[6])
            let p12 = (
                (-4.0 * c3.0 - c12.0 + 6.0 * (c2.0 + c7.0) - 2.0 * (c0.0 + c15.0)
                    + 3.0 * (c4.0 + c14.0))
                    / 9.0,
                (-4.0 * c3.1 - c12.1 + 6.0 * (c2.1 + c7.1) - 2.0 * (c0.1 + c15.1)
                    + 3.0 * (c4.1 + c14.1))
                    / 9.0,
            );
            // p21 (ps[9])
            let p21 = (
                (-4.0 * c12.0 - c3.0 + 6.0 * (c8.0 + c13.0) - 2.0 * (c0.0 + c15.0)
                    + 3.0 * (c11.0 + c1.0))
                    / 9.0,
                (-4.0 * c12.1 - c3.1 + 6.0 * (c8.1 + c13.1) - 2.0 * (c0.1 + c15.1)
                    + 3.0 * (c11.1 + c1.1))
                    / 9.0,
            );
            // p22 (ps[10])
            let p22 = (
                (-4.0 * c15.0 - c0.0 + 6.0 * (c11.0 + c14.0) - 2.0 * (c12.0 + c3.0)
                    + 3.0 * (c2.0 + c8.0))
                    / 9.0,
                (-4.0 * c15.1 - c0.1 + 6.0 * (c11.1 + c14.1) - 2.0 * (c12.1 + c3.1)
                    + 3.0 * (c2.1 + c8.1))
                    / 9.0,
            );

            ps[5] = coords.len();
            coords.push(p11);
            ps[6] = coords.len();
            coords.push(p12);
            ps[9] = coords.len();
            coords.push(p21);
            ps[10] = coords.len();
            coords.push(p22);
        }

        figures.push(Patch { ps, cs });
        // Each patch record is byte-aligned (§8.7.4.6; pdf.js omits this).
        reader.align();
    }

    if figures.is_empty() {
        return Err(PdfError::eof(0, "patch mesh contains no patches"));
    }

    build_lattice(&figures, &coords, &colors, n_components)
}

/// Bernstein-cubic basis row for `count+1` samples: `[t_³, 3t·t_², 3t²·t_, t³]`
/// (reference `getB`).
fn bernstein(count: usize) -> Vec<[f64; 4]> {
    (0..=count)
        .map(|i| {
            let t = i as f64 / count as f64;
            let t_ = 1.0 - t;
            [t_ * t_ * t_, 3.0 * t * t_ * t_, 3.0 * t * t * t_, t * t * t]
        })
        .collect()
}

/// Reference clamp `MAX(3, MIN(20, chunks))` (`MIN/MAX_SPLIT_PATCH_CHUNKS_AMOUNT`).
fn clamp_chunks(v: usize) -> usize {
    v.clamp(3, 20)
}

/// Convert patch figures into a lattice of Gouraud triangles (reference
/// `_buildFigureFromPatch` + `_updateBounds`).
fn build_lattice(
    figures: &[Patch],
    coords: &[(f64, f64)],
    colors: &[Vec<f64>],
    n_components: usize,
) -> Result<MeshShading> {
    if colors.is_empty() {
        return Err(PdfError::eof(0, "patch mesh contains no colours"));
    }
    // Whole-mesh bounds over the control points (reference `_updateBounds`
    // runs over all coords before figures are expanded).
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in coords {
        min_x = min_x.min(*x);
        min_y = min_y.min(*y);
        max_x = max_x.max(*x);
        max_y = max_y.max(*y);
    }
    let bounds_w = (max_x - min_x).max(1e-9);
    let bounds_h = (max_y - min_y).max(1e-9);

    let mut mesh = MeshShading {
        n_components,
        ..Default::default()
    };
    let n = n_components.max(1);

    for fig in figures {
        let pi = &fig.ps;
        let ci = &fig.cs;
        let figure_min_x = coords[pi[0]]
            .0
            .min(coords[pi[3]].0)
            .min(coords[pi[12]].0)
            .min(coords[pi[15]].0);
        let figure_min_y = coords[pi[0]]
            .1
            .min(coords[pi[3]].1)
            .min(coords[pi[12]].1)
            .min(coords[pi[15]].1);
        let figure_max_x = coords[pi[0]]
            .0
            .max(coords[pi[3]].0)
            .max(coords[pi[12]].0)
            .max(coords[pi[15]].0);
        let figure_max_y = coords[pi[0]]
            .1
            .max(coords[pi[3]].1)
            .max(coords[pi[12]].1)
            .max(coords[pi[15]].1);

        // Density: 20 chunks per mesh-bounds axis scaled by the figure's own
        // extent, clamped to [3, 20] per axis (reference `_buildFigureFromPatch`).
        let split_x =
            clamp_chunks((((figure_max_x - figure_min_x) * 20.0) / bounds_w).ceil() as usize);
        let split_y =
            clamp_chunks((((figure_max_y - figure_min_y) * 20.0) / bounds_h).ceil() as usize);

        let vpr = split_x + 1;
        let b_row = bernstein(split_y);
        let b_col = bernstein(split_x);

        // Bilinear corner-colour blend: cl runs along the j=0 edge, cr along
        // the j=split_x edge (reference `cl`/`cr`; colours stay f64 comps —
        // the reference quantises to Uint8 RGB, within snapshot tolerance).
        let blend_edge = |ca: &[f64], cb: &[f64], t: f64| -> Vec<f64> {
            (0..n)
                .map(|k| {
                    let a = ca.get(k).copied().unwrap_or(0.0);
                    let b = cb.get(k).copied().unwrap_or(0.0);
                    (a * (split_y as f64 - t) + b * t) / split_y as f64
                })
                .collect()
        };

        let base = mesh.vertices.len();
        for (row, b_r) in b_row.iter().enumerate().take(split_y + 1) {
            let cl = blend_edge(&colors[ci[0]], &colors[ci[2]], row as f64);
            let cr = blend_edge(&colors[ci[1]], &colors[ci[3]], row as f64);
            for (col, b_c) in b_col.iter().enumerate().take(split_x + 1) {
                let mut x = 0.0;
                let mut y = 0.0;
                for i in 0..4 {
                    for j in 0..4 {
                        let m = b_r[i] * b_c[j];
                        let c = coords[pi[4 * i + j]];
                        x += c.0 * m;
                        y += c.1 * m;
                    }
                }
                // Colour: horizontal blend of the two edge colours.
                let comps: Vec<f64> = (0..n)
                    .map(|k| {
                        let l = cl.get(k).copied().unwrap_or(0.0);
                        let r = cr.get(k).copied().unwrap_or(0.0);
                        (l * (split_x as f64 - col as f64) + r * col as f64) / split_x as f64
                    })
                    .collect();
                mesh.vertices.push(Vertex { x, y, comps });
            }
        }

        // Lattice triangulation — identical to the Type-5 path in `mesh.rs`.
        for row in 0..split_y {
            for col in 0..split_x {
                let a = base + row * vpr + col;
                let b = a + 1;
                let c = base + (row + 1) * vpr + col;
                let d = c + 1;
                mesh.triangles.push([a, b, c]);
                mesh.triangles.push([c, b, d]);
            }
        }
    }

    if mesh.vertices.is_empty() {
        return Err(PdfError::eof(0, "patch mesh produced no vertices"));
    }
    Ok(mesh)
}

// ---------------------------------------------------------------------------
// Tests (R5)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::objects::PdfObject;

    fn coons_dict() -> PdfDict {
        let mut d: PdfDict = [
            ("ShadingType", 6i64),
            ("BitsPerFlag", 8i64),
            ("BitsPerCoordinate", 8i64),
            ("BitsPerComponent", 8i64),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), PdfObject::Integer(*v)))
        .collect();
        d.insert(
            "ColorSpace".to_string(),
            PdfObject::Name("DeviceGray".to_string()),
        );
        // Identity coordinate mapping (raw byte → value) so the fixture
        // geometry lands exactly on the written coordinates; components
        // decode into [0, 1].
        d.insert(
            "Decode".to_string(),
            PdfObject::Array(
                [0.0, 255.0, 0.0, 255.0, 0.0, 1.0]
                    .iter()
                    .map(|v| PdfObject::Real(*v))
                    .collect(),
            ),
        );
        d
    }

    fn u8(v: usize) -> u8 {
        v as u8
    }

    /// Straight-edge square Coons patch, flag 0: 12 coords + 4 colors.
    /// Stream order (pdf.js flag-0 ps[] map): p00, p01, p02, p03, p13, p23,
    /// p33, p32, p31, p30, p20, p10 — the unit 10×10 square with corners
    /// p00=(0,0), p03=(0,10), p30=(10,0), p33=(10,10).
    fn square_coons_patch() -> Vec<u8> {
        let mut b = Vec::new();
        b.push(0); // flag 0
        for &(x, y) in &[
            (0, 0),   // p00
            (0, 3),   // p01
            (0, 7),   // p02
            (0, 10),  // p03
            (3, 10),  // p13
            (7, 10),  // p23
            (10, 10), // p33
            (10, 7),  // p32
            (10, 3),  // p31
            (10, 0),  // p30
            (7, 0),   // p20
            (3, 0),   // p10
        ] {
            b.push(u8(x));
            b.push(u8(y));
        }
        // Corner colours — the flag-0 map reads cs[0]=ci (c00), cs[2]=ci+1
        // (c03), cs[3]=ci+2 (c33), cs[1]=ci+3 (c30), so stream colour order
        // is c00, c03, c33, c30.
        b.push(0); // c00 (black)
        b.push(255); // c03 (white)
        b.push(0); // c33 (black)
        b.push(255); // c30 (white)
        b
    }

    #[test]
    fn coons_square_patch_corner_colors() {
        let d = coons_dict();
        let mesh = parse(&d, &square_coons_patch(), 6, None).expect("coons parses");
        // Bounds 10×10 → split = clamp(ceil(10·20/10)) = 20 → 21×21 lattice.
        assert_eq!(mesh.triangles.len(), 20 * 20 * 2);
        assert_eq!(mesh.vertices.len(), 21 * 21);
        let vpr = 21;
        // Corner (row 0, col 0) = p00 with c00.
        assert_eq!(mesh.vertices[0].comps, vec![0.0]);
        // Corner (row 0, col 20) = p30 with c30.
        assert_eq!(mesh.vertices[20].comps, vec![1.0]);
        // Corner (row 20, col 0) = p03 with c03.
        assert_eq!(mesh.vertices[20 * vpr].comps, vec![1.0]);
        // Corner (row 20, col 20) = p33 with c33.
        assert_eq!(mesh.vertices[20 * vpr + 20].comps, vec![0.0]);
        // Corner positions: row ↔ i (y axis), col ↔ j (x axis) — p30 is the
        // (i=0, j=3) corner at (10, 0).
        assert!((mesh.vertices[0].x - 0.0).abs() < 1e-9);
        assert!((mesh.vertices[0].y - 0.0).abs() < 1e-9);
        assert!((mesh.vertices[20].x - 10.0).abs() < 1e-9);
        assert!((mesh.vertices[20].y - 0.0).abs() < 1e-9);
        assert!((mesh.vertices[20 * vpr].x - 0.0).abs() < 1e-9);
        assert!((mesh.vertices[20 * vpr].y - 10.0).abs() < 1e-9);
        assert!((mesh.vertices[20 * vpr + 20].x - 10.0).abs() < 1e-9);
        assert!((mesh.vertices[20 * vpr + 20].y - 10.0).abs() < 1e-9);
    }

    #[test]
    fn coons_flag1_chain_shares_edge() {
        let d = coons_dict();
        let mut bytes = square_coons_patch();
        // Flag 1 chain: 8 coords + 2 colours. The shared edge is the
        // previous p03…p33 row (y = 10); the new patch hangs below
        // (y = 10 → 20). Stream order (pdf.js flag-1 map): p13, p23, p33,
        // p32, p31, p30, p20, p10; grid (x = 10·i/3, y = 10 + 10·j/3).
        bytes.push(1); // flag
        for &(x, y) in &[
            (10, 13), // p13 (i=3, j=1)
            (10, 17), // p23 (i=3, j=2)
            (10, 20), // p33 (i=3, j=3)
            (7, 20),  // p32 (i=2, j=3)
            (3, 20),  // p31 (i=1, j=3)
            (0, 20),  // p30 (i=0, j=3)
            (0, 17),  // p20 (i=0, j=2)
            (0, 13),  // p10 (i=0, j=1)
        ] {
            bytes.push(u8(x));
            bytes.push(u8(y));
        }
        // Stream colour 0 → cs[3] (c33), colour 1 → cs[1] (c30); c00/c03
        // chain from the previous patch.
        bytes.push(128); // c33
        bytes.push(64); // c30
        let mesh = parse(&d, &bytes, 6, None).expect("coons chain parses");
        // Global bounds are 10×20 (the two patches stack vertically), so the
        // density formula gives each figure a 20×10 lattice: 21×11 vertices.
        assert_eq!(mesh.vertices.len(), 2 * 231);
        let base = 231;
        let vpr = 21; // split_x + 1
        let rows = 10; // split_y
                       // Chained c00 = prev c03 = 1.0, at the shared corner p03 (0, 10).
        assert_eq!(mesh.vertices[base].comps, vec![1.0]);
        assert!((mesh.vertices[base].x - 0.0).abs() < 1e-9);
        assert!((mesh.vertices[base].y - 10.0).abs() < 1e-9);
        // c30 = 64/255 at (0, 20) (i=0, j=3 → row 0, col 20).
        assert_eq!(mesh.vertices[base + 20].comps, vec![64.0 / 255.0]);
        assert!((mesh.vertices[base + 20].x - 0.0).abs() < 1e-9);
        assert!((mesh.vertices[base + 20].y - 20.0).abs() < 1e-9);
        // c03 = prev c33 = 0.0 at (10, 10) (i=3, j=0 → row 10, col 0).
        assert_eq!(mesh.vertices[base + rows * vpr].comps, vec![0.0]);
        assert!((mesh.vertices[base + rows * vpr].x - 10.0).abs() < 1e-9);
        assert!((mesh.vertices[base + rows * vpr].y - 10.0).abs() < 1e-9);
        // c33 = 128/255 at (10, 20) — last lattice vertex.
        assert_eq!(
            mesh.vertices[base + rows * vpr + 20].comps,
            vec![128.0 / 255.0]
        );
        assert!((mesh.vertices[base + rows * vpr + 20].x - 10.0).abs() < 1e-9);
        assert!((mesh.vertices[base + rows * vpr + 20].y - 20.0).abs() < 1e-9);
    }

    #[test]
    fn tensor_square_patch_corners() {
        let mut d = coons_dict();
        d.insert("ShadingType".to_string(), PdfObject::Integer(7));
        let mut b = Vec::new();
        b.push(0); // flag 0: 16 coords + 4 colours
                   // Stream order (pdf.js tensor flag-0 map): the 12 Coons-order points
                   // then the interior p11, p12, p22, p21 (stream 12–15).
        for &(x, y) in &[
            (0, 0),   // p00
            (0, 3),   // p01
            (0, 7),   // p02
            (0, 10),  // p03
            (3, 10),  // p13
            (7, 10),  // p23
            (10, 10), // p33
            (10, 7),  // p32
            (10, 3),  // p31
            (10, 0),  // p30
            (7, 0),   // p20
            (3, 0),   // p10
            (3, 3),   // p11 (i=1, j=1)
            (3, 7),   // p12 (i=2, j=1)
            (7, 7),   // p22 (i=2, j=2)
            (7, 3),   // p21 (i=1, j=2)
        ] {
            b.push(u8(x));
            b.push(u8(y));
        }
        // Stream colour order c00, c03, c33, c30 (same as Coons flag 0).
        b.push(0); // c00
        b.push(255); // c03
        b.push(0); // c33
        b.push(255); // c30
        let mesh = parse(&d, &b, 7, None).expect("tensor parses");
        assert_eq!(mesh.triangles.len(), 20 * 20 * 2);
        assert_eq!(mesh.vertices.len(), 21 * 21);
        let vpr = 21;
        // Corners carry the corner colours: c00 at p00, c30 at p30 (10, 0),
        // c03 at p03 (0, 10), c33 at p33 (10, 10).
        assert_eq!(mesh.vertices[0].comps, vec![0.0]);
        assert_eq!(mesh.vertices[20].comps, vec![1.0]);
        assert_eq!(mesh.vertices[20 * vpr].comps, vec![1.0]);
        assert_eq!(mesh.vertices[20 * vpr + 20].comps, vec![0.0]);
        assert!((mesh.vertices[0].x.abs() + mesh.vertices[0].y.abs()) < 1e-9);
        assert!((mesh.vertices[20].x - 10.0).abs() < 1e-9);
        assert!((mesh.vertices[20 * vpr].y - 10.0).abs() < 1e-9);
        assert!((mesh.vertices[20 * vpr + 20].x - 10.0).abs() < 1e-9);
        assert!((mesh.vertices[20 * vpr + 20].y - 10.0).abs() < 1e-9);
    }

    #[test]
    fn patch_bad_flag_is_error() {
        let d = coons_dict();
        let bytes = vec![5u8, 0, 0];
        assert!(parse(&d, &bytes, 6, None).is_err());
    }

    #[test]
    fn patch_truncated_is_error() {
        let d = coons_dict();
        // Flag byte present, coordinates missing.
        let bytes = vec![0u8];
        assert!(parse(&d, &bytes, 6, None).is_err());
    }

    #[test]
    fn chained_patch_without_flag0_is_error() {
        let d = coons_dict();
        // Flag 1 without a preceding flag-0 patch.
        let bytes = vec![1u8, 0, 0];
        assert!(parse(&d, &bytes, 6, None).is_err());
    }

    #[test]
    fn rasterize_is_bounded_by_canvas() {
        // Inflate coordinates far off-canvas via /Decode; rasterising must
        // not panic or grow the canvas.
        let mut big = coons_dict();
        big.insert(
            "Decode".to_string(),
            PdfObject::Array(
                [0.0, 1e5, 0.0, 1e5, 0.0, 1.0]
                    .iter()
                    .map(|v| PdfObject::Real(*v))
                    .collect(),
            ),
        );
        let mesh = parse(&big, &square_coons_patch(), 6, None).expect("coons parses");
        let mut canvas = crate::render::canvas::PixmapBuffer::new(10, 10).unwrap();
        mesh.rasterize(
            &crate::content::graphics_state::Matrix::identity(),
            &mut canvas,
        );
    }
}
