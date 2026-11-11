//! JPX (JPEG 2000) image decoder — "wavelet-lite" subset (plan §1.5).
//!
//! Pure-Rust port of the OpenJPEG 1.5.2 decode paths: codestream markers
//! (`codestream`), tile/band/code-block geometry + Tier-2 packet headers
//! (`packets`), tag trees + packet bit reader (`bio`), the MQ / RAW
//! arithmetic decoders (`mqc`), Tier-1 bit-plane passes (`t1`, with LUTs in
//! `t1_luts`), and the reversible 5-3 wavelet (`wavelet`).
//!
//! Scope decisions (deliberate, logged per plan §1.5):
//! - 5-3 reversible wavelet only (9-7 rejected at the QCD/COD markers);
//! - no ROI (`RGN` warned and ignored), no precinct partitions (rejected);
//! - all five progression orders supported on the single-precinct grid;
//! - inverse RCT (reversible multi-component transform) applied for 3
//!   equally-sized components when MCT is signalled.
//!
//! Public API: [`decode`] returns a [`JpxImage`] with one `i32` sample
//! plane per component; [`JpxImage::interleaved_u8`] flattens to 8-bit
//! interleaved samples for the render layer.

mod bio;
pub mod codestream;
pub mod mqc;
pub mod packets;
pub mod t1;
pub mod t1_luts;
pub mod wavelet;

use crate::error::{PdfError, Result};

use codestream::Codestream;
use t1::{Seg, T1};

/// One decoded component: signed integer samples at full precision.
pub struct JpxComp {
    pub w: u32,
    pub h: u32,
    pub prec: u8,
    pub sgnd: bool,
    /// Subsampling factors from SIZ.
    pub xr: u32,
    pub yr: u32,
    /// Row-major samples (already level-shifted into the component range).
    pub data: Vec<i32>,
}

/// A fully decoded JPX image.
pub struct JpxImage {
    pub comps: Vec<JpxComp>,
}

impl JpxImage {
    /// Width of the first (largest) component.
    pub fn width(&self) -> u32 {
        self.comps.first().map(|c| c.w).unwrap_or(0)
    }

    /// Height of the first (largest) component.
    pub fn height(&self) -> u32 {
        self.comps.first().map(|c| c.h).unwrap_or(0)
    }

    /// Flatten to 8-bit interleaved samples (gray, gray+alpha, RGB or CMYK
    /// component order preserved). Precisions are shifted into 0..255.
    pub fn interleaved_u8(&self) -> (u32, u32, usize, Vec<u8>) {
        let w = self.width();
        let h = self.height();
        let n = self.comps.len().min(4);
        let mut out = vec![0u8; (w as usize) * (h as usize) * n];
        for (ci, comp) in self.comps.iter().take(n).enumerate() {
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let src = comp.data[y * comp.w as usize + x];
                    // signed components shift into unsigned range first
                    let v = if comp.sgnd {
                        src + (1i32 << (comp.prec.saturating_sub(1)))
                    } else {
                        src
                    };
                    // scale precision into 8 bits
                    let v8 = if comp.prec > 8 {
                        (v >> (comp.prec - 8)).clamp(0, 255)
                    } else {
                        (v << (8 - comp.prec)).clamp(0, 255)
                    };
                    out[(y * w as usize + x) * n + ci] = v8 as u8;
                }
            }
        }
        (w, h, n, out)
    }
}

fn ceildivpow2(a: i32, b: u32) -> i32 {
    (a + ((1i32 << b) - 1)) >> b
}

/// Decode a JPEG 2000 codestream (optionally JP2-boxed) into components.
pub fn decode(data: &[u8]) -> Result<JpxImage> {
    let cs = codestream::parse(data)?;
    for c in &cs.comps {
        if c.prec > 16 {
            return Err(PdfError::invalid_token(
                0,
                format!("unsupported precision {}", c.prec),
            ));
        }
        if c.xr == 0 || c.yr == 0 {
            return Err(PdfError::invalid_token(
                0,
                "zero component subsampling factor",
            ));
        }
    }

    // tile grid
    let tw = (cs.x1 - cs.x_tosiz).div_ceil(cs.x_tsiz.max(1)) as u32;
    let th = (cs.y1 - cs.y_tosiz).div_ceil(cs.y_tsiz.max(1)) as u32;
    let n_tiles = (tw as usize) * (th as usize);
    if n_tiles == 0 {
        return Err(PdfError::invalid_token(0, "empty tile grid"));
    }
    if cs.x1 <= cs.x0 || cs.y1 <= cs.y0 {
        return Err(PdfError::invalid_token(0, "empty image dimensions"));
    }

    // concatenate tile parts per tile
    let mut tile_data: Vec<Vec<u8>> = vec![Vec::new(); n_tiles];
    for (isot, part) in cs.tile_parts.iter() {
        let t = *isot as usize;
        if t >= n_tiles {
            return Err(PdfError::invalid_token(
                0,
                format!("tile index {isot} out of range"),
            ));
        }
        tile_data[t].extend_from_slice(part);
    }

    // global per-component output planes
    let mut out: Vec<JpxComp> = cs
        .comps
        .iter()
        .map(|c| {
            let w = ceildivpow2(cs.x1 as i32, c.xr.trailing_zeros()).max(0) as u32;
            let h = ceildivpow2(cs.y1 as i32, c.yr.trailing_zeros()).max(0) as u32;
            JpxComp {
                w,
                h,
                prec: c.prec,
                sgnd: c.sgnd,
                xr: c.xr,
                yr: c.yr,
                data: vec![0; (w as usize) * (h as usize)],
            }
        })
        .collect();
    // component origins (global): ceildivpow2(XOsiz, xr)
    let comp_x0: Vec<i32> = cs
        .comps
        .iter()
        .map(|c| ceildivpow2(cs.x0 as i32, c.xr.trailing_zeros()))
        .collect();
    let comp_y0: Vec<i32> = cs
        .comps
        .iter()
        .map(|c| ceildivpow2(cs.y0 as i32, c.yr.trailing_zeros()))
        .collect();

    for (tile_idx, data) in tile_data.iter().enumerate() {
        decode_tile(&cs, tile_idx, tw, data, &comp_x0, &comp_y0, &mut out)?;
    }
    Ok(JpxImage { comps: out })
}

/// Decode one tile into the global component planes.
#[allow(clippy::too_many_arguments)]
fn decode_tile(
    cs: &Codestream,
    tile_idx: usize,
    tw: u32,
    data: &[u8],
    comp_x0: &[i32],
    comp_y0: &[i32],
    out: &mut [JpxComp],
) -> Result<()> {
    let tx = tile_idx as u32 % tw;
    let ty = tile_idx as u32 / tw;
    let tcx0 = (cs.x_tosiz + tx * cs.x_tsiz).max(cs.x0) as i32;
    let tcy0 = (cs.y_tosiz + ty * cs.y_tsiz).max(cs.y0) as i32;
    let tcx1 = (cs.x_tosiz + (tx + 1) * cs.x_tsiz).min(cs.x1) as i32;
    let tcy1 = (cs.y_tosiz + (ty + 1) * cs.y_tsiz).min(cs.y1) as i32;
    if tcx1 <= tcx0 || tcy1 <= tcy0 {
        return Err(PdfError::invalid_token(0, "degenerate tile"));
    }

    let ncomp = cs.comps.len();
    let mut numres_all = 1usize;
    let mut geoms: Vec<Vec<packets::Resolution>> = Vec::with_capacity(ncomp);
    for (ci, comp) in cs.comps.iter().enumerate() {
        let tccp = &cs.tccps[ci];
        numres_all = numres_all.max(tccp.num_decomps as usize + 1);
        let ccx0 = ceildivpow2(tcx0, comp.xr.trailing_zeros());
        let ccy0 = ceildivpow2(tcy0, comp.yr.trailing_zeros());
        let ccx1 = ceildivpow2(tcx1, comp.xr.trailing_zeros());
        let ccy1 = ceildivpow2(tcy1, comp.yr.trailing_zeros());
        geoms.push(packets::build_geometry(ccx0, ccy0, ccx1, ccy1, tccp)?);
    }

    // Tier-2 over the concatenated tile-part data
    packets::read_packets(
        &mut geoms,
        data,
        &cs.tccps,
        numres_all,
        cs.csty_sop,
        cs.csty_eph,
    )?;

    // Tier-1 + tile buffer assembly per component
    let mut tile_bufs: Vec<Vec<i32>> = Vec::with_capacity(ncomp);
    for (ci, comp) in cs.comps.iter().enumerate() {
        let tccp = &cs.tccps[ci];
        let geom = &geoms[ci];
        let numres = tccp.num_decomps as usize + 1;
        // tile buffer spans the full tile-component size (tcd.c:
        // tilec->data is (x1 - x0) × (y1 - y0), band positions are
        // relative offsets computed from band/cblk global coords).
        let tile_w = (tcx1 - tcx0) as usize;
        let tile_h = (tcy1 - tcy0) as usize;
        let mut tile_buf = vec![0i32; tile_w * tile_h];

        for (resno, res) in geom.iter().enumerate() {
            for band in res.bands.iter() {
                if band.is_empty() {
                    continue;
                }
                let pres_w = if resno > 0 {
                    geom[resno - 1].x1 - geom[resno - 1].x0
                } else {
                    0
                };
                let pres_h = if resno > 0 {
                    geom[resno - 1].y1 - geom[resno - 1].y0
                } else {
                    0
                };
                for cblk in band.cblks.iter() {
                    let w = (cblk.x1 - cblk.x0).max(0) as usize;
                    let h = (cblk.y1 - cblk.y0).max(0) as usize;
                    if w == 0 || h == 0 || cblk.segs.is_empty() {
                        continue;
                    }
                    // gather non-empty segments as T1 input
                    let segs: Vec<Seg> = cblk
                        .segs
                        .iter()
                        .filter(|s| !s.data.is_empty())
                        .map(|s| Seg {
                            data: &s.data,
                            numpasses: s.numpasses,
                        })
                        .collect();
                    if segs.is_empty() {
                        continue;
                    }
                    let flags_len = (w + 2) * (h + 2);
                    let mut cblk_data = vec![0i32; w * h];
                    let mut cblk_flags = vec![0u16; flags_len];
                    let mut dec = T1::new(w, h, &mut cblk_data, &mut cblk_flags);
                    dec.decode_cblk(&segs, band.bandno as usize, cblk.numbps, tccp.cblk_sty);
                    let (cblk_data, _flags) = dec.into_parts();

                    // position in the tile buffer (t1_decode_cblks)
                    let mut x = cblk.x0 - band.x0;
                    let mut y = cblk.y0 - band.y0;
                    if band.bandno & 1 != 0 {
                        x += pres_w;
                    }
                    if band.bandno & 2 != 0 {
                        y += pres_h;
                    }
                    // band coords are in the resolution space; convert to
                    // tile-buffer offsets: tile buffer is indexed from the
                    // tile-comp origin, band coords from their own origin —
                    // the relative offsets already line up per tcd.c.
                    for j in 0..h {
                        for i in 0..w {
                            let tx_ = (x as usize) + i;
                            let ty_ = (y as usize) + j;
                            if tx_ < tile_w && ty_ < tile_h {
                                // reversible: tmp / 2 (C truncation)
                                tile_buf[ty_ * tile_w + tx_] = cblk_data[j * w + i] / 2;
                            }
                        }
                    }
                }
            }
        }

        // IDWT over the tile buffer
        let ccx0 = ceildivpow2(tcx0, comp.xr.trailing_zeros());
        let ccy0 = ceildivpow2(tcy0, comp.yr.trailing_zeros());
        let res_dims: Vec<(i32, i32, i32, i32)> = geom
            .iter()
            .map(|r| (r.x0 - ccx0, r.y0 - ccy0, r.x1 - ccx0, r.y1 - ccy0))
            .collect();
        wavelet::idwt(&mut tile_buf, tile_w, &res_dims, numres);

        tile_bufs.push(tile_buf);
    }

    // MCT (inverse RCT) across the first three components when signalled
    // and equally sized (OpenJPEG applies it unconditionally for 3 comps;
    // we guard subsampled mismatches).
    if cs.tccps[0].mct && ncomp >= 3 {
        let (w0, h0) = ((tcx1 - tcx0) as usize, (tcy1 - tcy0) as usize);
        let same = tile_bufs[0].len() == tile_bufs[1].len()
            && tile_bufs[1].len() == tile_bufs[2].len()
            && tile_bufs[0].len() == w0 * h0;
        if same {
            for i in 0..tile_bufs[0].len() {
                let yv = tile_bufs[0][i];
                let u = tile_bufs[1][i];
                let v = tile_bufs[2][i];
                let g = yv - ((u + v) >> 2);
                let r = v + g;
                let b = u + g;
                tile_bufs[0][i] = r;
                tile_bufs[1][i] = g;
                tile_bufs[2][i] = b;
            }
        } else {
            log::warn!("JPX: MCT signalled with mismatched component sizes; skipping");
        }
    }

    // level shift + clamp, write into the global planes
    for (ci, comp) in cs.comps.iter().enumerate() {
        let tccp = &cs.tccps[ci];
        let _ = tccp;
        let ccx0 = ceildivpow2(tcx0, comp.xr.trailing_zeros());
        let ccy0 = ceildivpow2(tcy0, comp.yr.trailing_zeros());
        let ccx1 = ceildivpow2(tcx1, comp.xr.trailing_zeros());
        let ccy1 = ceildivpow2(tcy1, comp.yr.trailing_zeros());
        let prec = comp.prec;
        let (min, max, adjust) = if comp.sgnd {
            (-(1i32 << (prec - 1)), (1i32 << (prec - 1)) - 1, 0i32)
        } else {
            (0i32, (1i32 << prec) - 1, 1i32 << (prec - 1))
        };
        let tw_ = (ccx1 - ccx0) as usize;
        let th_ = (ccy1 - ccy0) as usize;
        let tile_w = (tcx1 - tcx0) as usize;
        for j in 0..th_ {
            for i in 0..tw_ {
                let v = tile_bufs[ci][j * tile_w + i];
                let v = (v + adjust).clamp(min, max);
                let gx = (ccx0 - comp_x0[ci]) as usize + i;
                let gy = (ccy0 - comp_y0[ci]) as usize + j;
                let gw = out[ci].w as usize;
                if gx < gw && gy < out[ci].h as usize {
                    out[ci].data[gy * gw + gx] = v;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (R5)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal 1x1 single-component codestream whose only packet is absent
    /// (`present = 0`): decode must yield one 8-bit component whose sample
    /// is the unsigned level-shift midpoint (128).
    fn minimal_codestream() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&[0xff, 0x4f]); // SOC
        let mut siz = Vec::new();
        siz.extend_from_slice(&0u16.to_be_bytes());
        siz.extend_from_slice(&1u32.to_be_bytes()); // Xsiz
        siz.extend_from_slice(&1u32.to_be_bytes()); // Ysiz
        siz.extend_from_slice(&[0; 8]); // XOsiz, YOsiz
        siz.extend_from_slice(&1u32.to_be_bytes()); // XTsiz
        siz.extend_from_slice(&1u32.to_be_bytes()); // YTsiz
        siz.extend_from_slice(&[0; 8]); // XTOsiz, YTOsiz
        siz.extend_from_slice(&1u16.to_be_bytes()); // Csiz
        siz.extend_from_slice(&[7, 1, 1]);
        d.extend_from_slice(&[0xff, 0x51]);
        d.extend_from_slice(&(siz.len() as u16 + 2).to_be_bytes());
        d.extend_from_slice(&siz);
        let cod = [0u8, 0, 0, 1, 0, 0, 4, 4, 0, 1];
        d.extend_from_slice(&[0xff, 0x52]);
        d.extend_from_slice(&(cod.len() as u16 + 2).to_be_bytes());
        d.extend_from_slice(&cod);
        let qcd = [0x40u8, 8 << 3];
        d.extend_from_slice(&[0xff, 0x5c]);
        d.extend_from_slice(&(qcd.len() as u16 + 2).to_be_bytes());
        d.extend_from_slice(&qcd);
        let psot = 12u32 + 2 + 1;
        d.extend_from_slice(&[0xff, 0x90]);
        d.extend_from_slice(&10u16.to_be_bytes());
        d.extend_from_slice(&0u16.to_be_bytes()); // Isot
        d.extend_from_slice(&psot.to_be_bytes());
        d.extend_from_slice(&[0, 1]); // TPsot, TNsot
        d.extend_from_slice(&[0xff, 0x93]); // SOD
        d.push(0x00); // packet: absent
        d.extend_from_slice(&[0xff, 0xd9]); // EOC
        d
    }

    #[test]
    fn decode_absent_packet_yields_level_shifted_zero() {
        let img = decode(&minimal_codestream()).unwrap();
        assert_eq!(img.comps.len(), 1);
        assert_eq!(img.width(), 1);
        assert_eq!(img.height(), 1);
        assert_eq!(img.comps[0].data, vec![128]);
        let (w, h, n, pixels) = img.interleaved_u8();
        assert_eq!((w, h, n), (1, 1, 1));
        assert_eq!(pixels, vec![128]);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[0xff, 0x4f, 0x00, 0x01]).is_err());
    }

    #[test]
    fn decode_truncated_tile_part_is_error() {
        let mut d = minimal_codestream();
        d.truncate(d.len() - 4);
        assert!(decode(&d).is_err());
    }
}
