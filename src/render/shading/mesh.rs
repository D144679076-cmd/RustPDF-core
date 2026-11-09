//! Mesh-based shading rasteriser — ISO 32000-1 §8.7.4.4 / §8.7.4.5.
//!
//! Implements ShadingType 4 (free-form Gouraud triangles) and ShadingType 5
//! (lattice Gouraud). Vertex data is read from the shading object's stream:
//! bit-packed `flag? coords colors` records, byte-aligned per vertex.
//!
//! Flag semantics (ISO 32000-1 §8.7.4.5, mirrored from pdf.js `pattern.js`):
//! `0` starts a fresh 3-vertex patch; `1` attaches the current vertex to the
//! previous triangle's `P1–P2` edge; `2` attaches to its `P0–P2` edge.
//! The `ps` index stack reproduces pdf.js exactly — every 3 consecutive
//! entries form one triangle.
//!
//! Types 6 (Coons) and 7 (tensor) are deferred — see
//! `.doc/render-mesh-shading-2026-11-09.md`. Pure Rust, no new deps (R2);
//! no panic paths (R1).

use crate::content::graphics_state::Matrix;
use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};

use super::{components_to_rgba, cs_n_components};
use crate::render::canvas::PixmapBuffer;

/// One Gouraud mesh vertex: shading-space position + color components.
#[derive(Debug, Clone)]
pub struct Vertex {
    pub x: f64,
    pub y: f64,
    pub comps: Vec<f64>,
}

/// A packed mesh: flat vertex list plus triangle index triples.
#[derive(Debug, Clone, Default)]
pub struct MeshShading {
    pub vertices: Vec<Vertex>,
    pub triangles: Vec<[usize; 3]>,
    /// Number of color components per vertex (1/3/4).
    pub n_components: usize,
}

// ---------------------------------------------------------------------------
// Bit reader (MSB-first streams, byte-aligned per vertex)
// ---------------------------------------------------------------------------

struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, bit_pos: 0 }
    }

    /// Read `bits` (≤ 32) starting at the current bit position.
    fn read_bits(&mut self, bits: u32) -> Result<u32> {
        if bits == 0 || bits > 32 {
            return Err(PdfError::invalid_token(0, "mesh field width out of range"));
        }
        if self.bit_pos + bits as usize > self.data.len() * 8 {
            return Err(PdfError::eof(self.bit_pos / 8, "mesh stream truncated"));
        }
        let mut value: u32 = 0;
        for _ in 0..bits {
            let byte = self.data[self.bit_pos / 8];
            let bit = (byte >> (7 - (self.bit_pos % 8))) & 1;
            value = (value << 1) | bit as u32;
            self.bit_pos += 1;
        }
        Ok(value)
    }

    /// Discard bits up to the next byte boundary (each vertex is byte-aligned).
    fn align(&mut self) {
        if !self.bit_pos.is_multiple_of(8) {
            self.bit_pos += 8 - (self.bit_pos % 8);
        }
    }

    fn has_data(&self, bits: usize) -> bool {
        self.bit_pos + bits <= self.data.len() * 8
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn decode_int(dict: &PdfDict, key: &str, lo: u32, hi: u32) -> Result<u32> {
    let n = dict
        .get(key)
        .and_then(|o| o.as_integer())
        .ok_or_else(|| PdfError::invalid_token(0, format!("missing or non-numeric /{}", key)))?;
    if n < lo as i64 || n > hi as i64 {
        return Err(PdfError::invalid_token(
            0,
            format!("/{1} = {0} out of range {2}..={3}", n, key, lo, hi),
        ));
    }
    Ok(n as u32)
}

/// Mesh Decode mapping: `raw/(2^bits - 1)` into `[lo, hi]` (§8.7.4.3).
fn map_unit(raw: u32, bits: u32, lo: f64, hi: f64) -> f64 {
    let max = (1u64 << bits.min(31)) as f64 - 1.0;
    if max <= 0.0 {
        return lo;
    }
    lo + (raw as f64 / max) * (hi - lo)
}

fn obj_f64(o: &PdfObject) -> Option<f64> {
    match o {
        PdfObject::Integer(i) => Some(*i as f64),
        PdfObject::Real(r) => Some(*r),
        _ => None,
    }
}

/// Parse a Type 4 / Type 5 mesh from already-decoded stream bytes.
pub fn parse(dict: &PdfDict, data: &[u8], shading_type: u32) -> Result<MeshShading> {
    let bits_coord = decode_int(dict, "BitsPerCoordinate", 1, 32)?;
    let bits_comp = decode_int(dict, "BitsPerComponent", 1, 16)?;
    // Flags are encoded only for Type 4 (§8.7.4.5); Type 5 has none.
    let bits_flag = if shading_type == 4 {
        decode_int(dict, "BitsPerFlag", 2, 8)?
    } else {
        0
    };

    let mut decode: Vec<f64> = Vec::new();
    if let Some(PdfObject::Array(arr)) = dict.get("Decode") {
        decode.extend(arr.iter().filter_map(obj_f64));
    }

    let n_components = cs_n_components(dict.get("ColorSpace"));

    // /Decode layout: [x_lo x_hi y_lo y_hi  c0_lo c0_hi  c1_lo c1_hi …].
    // Defaults are the unit box ([0 1] per channel).
    let pair = |base: usize| -> [f64; 2] {
        if decode.is_empty() {
            return [0.0, 1.0];
        }
        [
            *decode.get(base).unwrap_or(&0.0),
            *decode.get(base + 1).unwrap_or(&1.0),
        ]
    };
    let coord_de = [pair(0), pair(2)];
    let comp_de: Vec<[f64; 2]> = (0..n_components).map(|i| pair(4 + 2 * i)).collect();

    let mut reader = BitReader::new(data);
    let mut mesh = MeshShading {
        n_components,
        ..Default::default()
    };
    let mut ps: Vec<usize> = Vec::new();

    let min_vertex_bits = 2 * bits_coord as usize + n_components * bits_comp as usize;

    let read_vertex = |reader: &mut BitReader| -> Result<Vertex> {
        let x_raw = reader.read_bits(bits_coord)? as f64;
        let y_raw = reader.read_bits(bits_coord)? as f64;
        let scale = (1u64 << bits_coord.min(31)) as f64 - 1.0;
        let x = coord_de[0][0] + x_raw / scale * (coord_de[0][1] - coord_de[0][0]);
        let y = coord_de[1][0] + y_raw / scale * (coord_de[1][1] - coord_de[1][0]);
        let mut comps = Vec::with_capacity(n_components);
        for pair in &comp_de {
            let raw = reader.read_bits(bits_comp)?;
            comps.push(map_unit(raw, bits_comp, pair[0], pair[1]));
        }
        Ok(Vertex { x, y, comps })
    };

    match shading_type {
        4 => loop {
            if !reader.has_data(min_vertex_bits + bits_flag as usize) {
                break;
            }
            let flag = reader.read_bits(bits_flag)?;
            if flag > 2 {
                return Err(PdfError::invalid_token(
                    0,
                    format!("unknown type-4 flag {}", flag),
                ));
            }
            let idx = mesh.vertices.len();
            mesh.vertices.push(read_vertex(&mut reader)?);

            if ps.len() < 3 && flag != 0 {
                return Err(PdfError::invalid_token(
                    0,
                    format!("type-4 flag {} before a patch has 3 vertices", flag),
                ));
            }
            if ps.len() >= 3 {
                match flag {
                    1 => {
                        // Shares the P1–P2 edge of the previous triangle.
                        let p1 = ps[ps.len() - 2];
                        let p2 = ps[ps.len() - 1];
                        ps.push(p1);
                        ps.push(p2);
                    }
                    2 => {
                        // Shares the P0–P2 edge of the previous triangle.
                        let p0 = ps[ps.len() - 3];
                        let p2 = ps[ps.len() - 1];
                        ps.push(p0);
                        ps.push(p2);
                    }
                    _ => ps.clear(), // flag 0: fresh patch
                }
            }
            ps.push(idx);
            if ps.len().is_multiple_of(3) {
                mesh.triangles
                    .push([ps[ps.len() - 3], ps[ps.len() - 2], ps[ps.len() - 1]]);
            }
            reader.align();
        },
        5 => {
            let vpr = match dict.get("VerticesPerRow").and_then(|o| o.as_integer()) {
                Some(n) if n >= 2 => n as usize,
                _ => {
                    return Err(PdfError::invalid_token(
                        0,
                        "type-5 mesh missing or invalid /VerticesPerRow",
                    ))
                }
            };
            let mut count = 0usize;
            while reader.has_data(min_vertex_bits) {
                match read_vertex(&mut reader) {
                    Ok(v) => mesh.vertices.push(v),
                    Err(e) => return Err(e),
                }
                count += 1;
                reader.align();
            }
            let rows = count / vpr;
            for r in 0..rows.saturating_sub(1) {
                for c in 0..vpr - 1 {
                    let a = r * vpr + c;
                    let b = r * vpr + c + 1;
                    let c0 = (r + 1) * vpr + c;
                    let d = (r + 1) * vpr + c + 1;
                    mesh.triangles.push([a, b, c0]);
                    mesh.triangles.push([c0, b, d]);
                }
            }
        }
        other => {
            return Err(PdfError::invalid_token(
                0,
                format!("mesh module handles types 4/5, got {}", other),
            ))
        }
    }

    if mesh.vertices.is_empty() {
        return Err(PdfError::eof(0, "mesh contains no vertices"));
    }
    Ok(mesh)
}

// ---------------------------------------------------------------------------
// Rasterisation
// ---------------------------------------------------------------------------

impl MeshShading {
    /// Rasterize all triangles with Gouraud (barycentric) color interpolation.
    ///
    /// Vertex positions are mapped shading-space → device pixels via `ctm`
    /// plus the canvas tile origin; interpolation happens in *raw color
    /// components* which are converted per-pixel through the shared
    /// `components_to_rgba` path (handles gray/RGB/CMYK).
    pub fn rasterize(&self, ctm: &Matrix, canvas: &mut PixmapBuffer) {
        let ox = canvas.origin.x as f64;
        let oy = canvas.origin.y as f64;
        let device: Vec<(f64, f64)> = self
            .vertices
            .iter()
            .map(|v| {
                let (dx, dy) = ctm.transform_point(v.x, v.y);
                (dx + ox, dy + oy)
            })
            .collect();
        let w = canvas.width as i32;
        let h = canvas.height as i32;
        let n = self.n_components.max(1);

        for tri in &self.triangles {
            let (x0, y0) = device[tri[0]];
            let (x1, y1) = device[tri[1]];
            let (x2, y2) = device[tri[2]];
            // Twice the signed area in device space.
            let area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
            if area.abs() < 1e-9 {
                continue; // degenerate or back-facing-zero triangle
            }

            let x_min = x0.min(x1).min(x2).floor().max(0.0) as i32;
            let x_max = x0.max(x1).max(x2).ceil().min(w as f64 - 1.0) as i32;
            let y_min = y0.min(y1).min(y2).floor().max(0.0) as i32;
            let y_max = y0.max(y1).max(y2).ceil().min(h as f64 - 1.0) as i32;
            if x_min > x_max || y_min > y_max {
                continue;
            }

            let buf_w = (x_max - x_min + 1) as usize;
            let buf_h = (y_max - y_min + 1) as usize;
            let mut rgba_buf = vec![0u8; buf_w * buf_h * 4];

            let colors: [&Vec<f64>; 3] = [
                &self.vertices[tri[0]].comps,
                &self.vertices[tri[1]].comps,
                &self.vertices[tri[2]].comps,
            ];
            let comp =
                |colors: &[&Vec<f64>], i: usize, k: usize| colors[i].get(k).copied().unwrap_or(0.0);

            for py in y_min..=y_max {
                for px in x_min..=x_max {
                    let fx = px as f64 + 0.5;
                    let fy = py as f64 + 0.5;
                    // Barycentric weights via edge cross products
                    // (each is 2·area × the weight of the opposite vertex).
                    let w0 = (x1 - fx) * (y2 - fy) - (x2 - fx) * (y1 - fy);
                    let w1 = (x2 - fx) * (y0 - fy) - (x0 - fx) * (y2 - fy);
                    let w2 = (x0 - fx) * (y1 - fy) - (x1 - fx) * (y0 - fy);
                    if !((w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0)
                        || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0))
                    {
                        continue;
                    }
                    let wa = w0 / area;
                    let wb = w1 / area;
                    let mut comps = Vec::with_capacity(n);
                    for k in 0..n {
                        let val = wa * comp(&colors, 0, k)
                            + wb * comp(&colors, 1, k)
                            + (1.0 - wa - wb) * comp(&colors, 2, k);
                        comps.push(val);
                    }
                    let rgba = components_to_rgba(&comps, n);
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
}

// ---------------------------------------------------------------------------
// Tests (R5)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::objects::PdfObject;

    fn dict(entries: &[(&str, i64)]) -> PdfDict {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), PdfObject::Integer(*v)))
            .collect()
    }

    #[test]
    fn lattice_triangle_count_matches_grid() {
        // VerticesPerRow = 3; two rows of vertices → 2 × 2 triangles.
        let mut d = dict(&[
            ("ShadingType", 5),
            ("BitsPerCoordinate", 8),
            ("BitsPerComponent", 8),
            ("VerticesPerRow", 3),
        ]);
        d.insert(
            "ColorSpace".to_string(),
            PdfObject::Name("DeviceRGB".to_string()),
        );
        let mut bytes = Vec::new();
        for i in 0..6u32 {
            let x = (i % 3) as u8;
            let y = (i / 3) as u8;
            // DeviceRGB: 2 coord bytes + 3 component bytes = 5 per vertex.
            bytes.extend_from_slice(&[x, y, x, x, x]);
        }
        let mesh = parse(&d, &bytes, 5).expect("lattice parses");
        assert_eq!(mesh.vertices.len(), 6);
        assert_eq!(mesh.triangles.len(), 4);
    }

    #[test]
    fn freeform_flags_build_connected_triangles() {
        let mut d = dict(&[
            ("ShadingType", 4),
            ("BitsPerFlag", 8),
            ("BitsPerCoordinate", 8),
            ("BitsPerComponent", 8),
        ]);
        d.insert(
            "ColorSpace".to_string(),
            PdfObject::Name("DeviceGray".to_string()),
        );
        let mut bytes = Vec::new();
        let push = |bytes: &mut Vec<u8>, flag: u8, x: u8| {
            bytes.push(flag);
            bytes.push(x);
            bytes.push(x);
            bytes.push(0); // one gray component
        };
        push(&mut bytes, 0, 0);
        push(&mut bytes, 0, 10);
        push(&mut bytes, 0, 5);
        push(&mut bytes, 1, 15);
        push(&mut bytes, 2, 20);
        let mesh = parse(&d, &bytes, 4).expect("type-4 parses");
        assert_eq!(mesh.vertices.len(), 5);
        // (0,1,2) fresh; (1,2,3) shares P1–P2; (1,3,4) shares P0–P2.
        assert_eq!(mesh.triangles, vec![[0, 1, 2], [1, 2, 3], [1, 3, 4]]);
    }

    #[test]
    fn truncated_stream_is_error() {
        let mut d = dict(&[
            ("ShadingType", 4),
            ("BitsPerFlag", 8),
            ("BitsPerCoordinate", 8),
            ("BitsPerComponent", 8),
        ]);
        d.insert(
            "ColorSpace".to_string(),
            PdfObject::Name("DeviceGray".to_string()),
        );
        let bytes = vec![0u8, 0u8]; // partial vertex
        assert!(parse(&d, &bytes, 4).is_err());
    }

    #[test]
    fn bad_flag_is_error() {
        let mut d = dict(&[
            ("ShadingType", 4),
            ("BitsPerFlag", 8),
            ("BitsPerCoordinate", 8),
            ("BitsPerComponent", 8),
        ]);
        d.insert(
            "ColorSpace".to_string(),
            PdfObject::Name("DeviceGray".to_string()),
        );
        let bytes = vec![7u8, 0, 0, 0];
        assert!(parse(&d, &bytes, 4).is_err());
    }

    #[test]
    fn lattice_requires_vertices_per_row() {
        let d = dict(&[
            ("ShadingType", 5),
            ("BitsPerCoordinate", 8),
            ("BitsPerComponent", 8),
        ]);
        assert!(parse(&d, &[0, 0, 0, 0, 0], 5).is_err());
    }

    fn identity() -> Matrix {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    #[test]
    fn rasterize_is_bounded_by_canvas() {
        // A huge off-canvas triangle must not panic.
        let mesh = MeshShading {
            vertices: vec![
                Vertex {
                    x: 1e5,
                    y: 1e5,
                    comps: vec![1.0],
                },
                Vertex {
                    x: -1e5,
                    y: 1e5,
                    comps: vec![0.0],
                },
                Vertex {
                    x: 0.0,
                    y: -1e5,
                    comps: vec![0.5],
                },
            ],
            triangles: vec![[0, 1, 2]],
            n_components: 1,
        };
        let mut canvas = PixmapBuffer::new(10, 10).unwrap();
        mesh.rasterize(&identity(), &mut canvas);
    }

    #[test]
    fn rasterize_paints_inner_pixel() {
        let mesh = MeshShading {
            vertices: vec![
                Vertex {
                    x: 0.0,
                    y: 0.0,
                    comps: vec![1.0],
                },
                Vertex {
                    x: 10.0,
                    y: 0.0,
                    comps: vec![1.0],
                },
                Vertex {
                    x: 5.0,
                    y: 10.0,
                    comps: vec![1.0],
                },
            ],
            triangles: vec![[0, 1, 2]],
            n_components: 1,
        };
        let mut canvas = PixmapBuffer::new(10, 10).unwrap();
        mesh.rasterize(&identity(), &mut canvas);
        let data = canvas.data();
        let mid = (5usize * 10 + 5) * 4;
        assert_eq!(data[mid], 255);
        assert_eq!(data[mid + 1], 255);
        assert_eq!(data[mid + 2], 255);
        assert_eq!(data[mid + 3], 255);
    }
}
