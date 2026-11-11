//! Tile geometry (resolution / band / code-block layout per `tcd.c`) and
//! Tier-2 packet decoding (packet iterator + packet headers per `t2.c`).
//!
//! Wavelet-lite scope: single-precinct codestreams only (the default; a
//! coded precinct partition is rejected in `codestream`), all five
//! progression orders collapsed onto the single-precinct precinct grid.

use crate::error::{PdfError, Result};

use super::bio::{BioReader, TagTree};
use super::codestream::{ProgOrder, Tccp};
use super::wavelet::gain;

fn ceildivpow2(a: i32, b: u32) -> i32 {
    (a + ((1i32 << b) - 1)) >> b
}
fn floordivpow2(a: i32, b: u32) -> i32 {
    a >> b
}

/// One code-block segment: coding passes, raw bytes (accumulated over the
/// packets in which the segment was extended), and the length announced by
/// the current packet header (`pending_len`).
#[derive(Debug, Default)]
pub struct SegData {
    pub numpasses: u32,
    pub data: Vec<u8>,
    pub pending_len: u32,
}

/// One code block.
#[derive(Debug, Default)]
pub struct Cblk {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    /// Effective bit-plane count (`band.numbps + 1 - imsb`).
    pub numbps: u32,
    pub numlenbits: u32,
    pub segs: Vec<SegData>,
}

/// One subband with its single precinct of code blocks.
pub struct Band {
    pub bandno: u8,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    /// Number of plain bit planes from the quantizer (`expn + numgbits - 1`).
    pub numbps: u32,
    /// Code-block grid (row-major).
    pub cw: usize,
    pub ch: usize,
    pub cblks: Vec<Cblk>,
    pub incltree: TagTree,
    pub imsbtree: TagTree,
}

impl Band {
    /// `opj_tcd_is_band_empty`.
    pub fn is_empty(&self) -> bool {
        self.x0 == self.x1 || self.y0 == self.y1
    }
}

/// One resolution level.
pub struct Resolution {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub bands: Vec<Band>,
}

/// Build the resolution/band/code-block tree for one tile component
/// (`tcd.c` geometry with default single precincts, `pdx = pdy = 15`).
pub fn build_geometry(
    tcx0: i32,
    tcy0: i32,
    tcx1: i32,
    tcy1: i32,
    tccp: &Tccp,
) -> Result<Vec<Resolution>> {
    let numres = tccp.num_decomps as usize + 1;
    let mut res = Vec::with_capacity(numres);
    for resno in 0..numres {
        let levelno = (numres - 1 - resno) as u32;
        let rx0 = ceildivpow2(tcx0, levelno);
        let ry0 = ceildivpow2(tcy0, levelno);
        let rx1 = ceildivpow2(tcx1, levelno);
        let ry1 = ceildivpow2(tcy1, levelno);

        let tlprcx = floordivpow2(rx0, 15) << 15;
        let tlprcy = floordivpow2(ry0, 15) << 15;
        let brprcx = ceildivpow2(rx1, 15) << 15;
        let brprcy = ceildivpow2(ry1, 15) << 15;
        let pw = ((brprcx - tlprcx) >> 15) as usize;
        let ph = ((brprcy - tlprcy) >> 15) as usize;
        if pw != 1 || ph != 1 {
            return Err(PdfError::invalid_token(
                0,
                format!("precinct partitioning not supported (res {resno}: {pw}x{ph})"),
            ));
        }

        let (tlcbgx, tlcbgy, cbgw_exp, cbgh_exp) = if resno == 0 {
            (tlprcx, tlprcy, 15u32, 15u32)
        } else {
            (ceildivpow2(tlprcx, 1), ceildivpow2(tlprcy, 1), 14u32, 14u32)
        };
        let cblkw_exp = (tccp.cbw_exp + 2).min(cbgw_exp);
        let cblkh_exp = (tccp.cbh_exp + 2).min(cbgh_exp);

        let nbands = if resno == 0 { 1 } else { 3 };
        let mut bands = Vec::with_capacity(nbands);
        for bandno in 0..nbands {
            let gbandno = if resno == 0 { 0 } else { bandno as u8 + 1 };
            let x0b = if gbandno == 1 || gbandno == 3 { 1 } else { 0 };
            let y0b = if gbandno == 2 || gbandno == 3 { 1 } else { 0 };
            let (bx0, by0, bx1, by1) = if gbandno == 0 {
                (rx0, ry0, rx1, ry1)
            } else {
                (
                    ceildivpow2(tcx0 - ((1i32 << levelno) * x0b), levelno + 1),
                    ceildivpow2(tcy0 - ((1i32 << levelno) * y0b), levelno + 1),
                    ceildivpow2(tcx1 - ((1i32 << levelno) * x0b), levelno + 1),
                    ceildivpow2(tcy1 - ((1i32 << levelno) * y0b), levelno + 1),
                )
            };

            // step-size exponent for this band (res0: 0; res r band b:
            // 3*(r-1) + b + 1)
            let ss_idx = if resno == 0 {
                0
            } else {
                3 * (resno - 1) + bandno + 1
            };
            let expn = *tccp
                .expn
                .get(ss_idx)
                .ok_or_else(|| PdfError::invalid_token(0, "QCD step-size list too short"))?;
            let numbps = (expn as u32 + tccp.numgbits as u32)
                .saturating_sub(1)
                .max(gain(gbandno));

            // single precinct, cbg == precinct bounds
            let cbgx1 = tlcbgx + (1i32 << cbgw_exp);
            let cbgy1 = tlcbgy + (1i32 << cbgh_exp);
            let prcx0 = tlcbgx.max(bx0);
            let prcy0 = tlcbgy.max(by0);
            let prcx1 = cbgx1.min(bx1);
            let prcy1 = cbgy1.min(by1);

            let tlcx = floordivpow2(prcx0, cblkw_exp) << cblkw_exp;
            let tlcy = floordivpow2(prcy0, cblkh_exp) << cblkh_exp;
            let brcx = ceildivpow2(prcx1, cblkw_exp) << cblkw_exp;
            let brcy = ceildivpow2(prcy1, cblkh_exp) << cblkh_exp;
            let cw = ((brcx - tlcx) >> cblkw_exp) as usize;
            let ch = ((brcy - tlcy) >> cblkh_exp) as usize;

            let mut cblks = Vec::with_capacity(cw * ch);
            for c in 0..cw * ch {
                let cx0 = tlcx + (c % cw) as i32 * (1i32 << cblkw_exp);
                let cy0 = tlcy + (c / cw) as i32 * (1i32 << cblkh_exp);
                cblks.push(Cblk {
                    x0: cx0.max(prcx0),
                    y0: cy0.max(prcy0),
                    x1: (cx0 + (1i32 << cblkw_exp)).min(prcx1),
                    y1: (cy0 + (1i32 << cblkh_exp)).min(prcy1),
                    numbps: 0,
                    numlenbits: 0,
                    segs: Vec::new(),
                });
            }
            bands.push(Band {
                bandno: gbandno,
                x0: bx0,
                y0: by0,
                x1: bx1,
                y1: by1,
                numbps,
                cw,
                ch,
                cblks,
                incltree: TagTree::new(cw, ch),
                imsbtree: TagTree::new(cw, ch),
            });
        }
        res.push(Resolution {
            x0: rx0,
            y0: ry0,
            x1: rx1,
            y1: ry1,
            bands,
        });
    }
    Ok(res)
}

/// `opj_t2_getnumpasses`.
fn getnumpasses(bio: &mut BioReader) -> u32 {
    if bio.read(1) == 0 {
        return 1;
    }
    if bio.read(1) == 0 {
        return 2;
    }
    let n = bio.read(2);
    if n != 3 {
        return 3 + n;
    }
    let n = bio.read(5);
    if n != 31 {
        return 6 + n;
    }
    37 + bio.read(7)
}

/// `opj_t2_getcommacode` — unary count of leading 1 bits.
fn getcommacode(bio: &mut BioReader) -> u32 {
    let mut n = 0;
    while bio.read(1) == 1 {
        n += 1;
    }
    n
}

fn floorlog2(v: u32) -> u32 {
    31 - v.leading_zeros()
}

/// Segment max-passes budget (`opj_t2_init_seg`): deterministic per
/// `(cblksty, seg index)` — seg 0 starts at 10 for LAZY, then the 2/1
/// pattern; TERMALL pins 1; otherwise 109.
fn seg_maxpasses(cblksty: u8, segno: usize) -> u32 {
    const TERMALL: u8 = super::t1::CBLKSTY_TERMALL;
    const LAZY: u8 = super::t1::CBLKSTY_LAZY;
    if cblksty & TERMALL != 0 {
        1
    } else if cblksty & LAZY != 0 {
        if segno == 0 {
            10
        } else {
            let prev = seg_maxpasses(cblksty, segno - 1);
            if prev == 1 || prev == 10 {
                2
            } else {
                1
            }
        }
    } else {
        109
    }
}

/// Packet stream position: `(layno, resno, compno)` — the precinct loop is
/// dropped (single precinct per resolution).
type PacketPos = (u32, usize, usize);

/// Iterate packet positions in the given progression order (single-precinct
/// collapse of the Annex B iterator).
pub fn packet_iter(
    prog: ProgOrder,
    num_layers: u32,
    numres: usize,
    ncomp: usize,
) -> Vec<PacketPos> {
    let mut out = Vec::with_capacity(num_layers as usize * numres * ncomp);
    match prog {
        ProgOrder::Lrcp => {
            for l in 0..num_layers {
                for r in 0..numres {
                    for c in 0..ncomp {
                        out.push((l, r, c));
                    }
                }
            }
        }
        ProgOrder::Rlcp => {
            for r in 0..numres {
                for l in 0..num_layers {
                    for c in 0..ncomp {
                        out.push((l, r, c));
                    }
                }
            }
        }
        ProgOrder::RpcL | ProgOrder::Pcrl => {
            for r in 0..numres {
                for c in 0..ncomp {
                    for l in 0..num_layers {
                        out.push((l, r, c));
                    }
                }
            }
        }
        ProgOrder::Cprl => {
            for c in 0..ncomp {
                for r in 0..numres {
                    for l in 0..num_layers {
                        out.push((l, r, c));
                    }
                }
            }
        }
    }
    out
}

/// Decode all packets of one tile from `data` into the per-component
/// geometry built by [`build_geometry`].
pub fn read_packets(
    comps: &mut [Vec<Resolution>],
    data: &[u8],
    tccps: &[Tccp],
    numres: usize,
    csty_sop: bool,
    csty_eph: bool,
) -> Result<()> {
    let tccp0 = &tccps[0];
    let num_layers = tccp0.num_layers;
    let mut offset = 0usize;
    for (layno, resno, compno) in packet_iter(tccp0.prog, num_layers, numres, comps.len()) {
        if offset >= data.len() {
            log::warn!(
                "JPX: packet stream truncated after {offset} bytes; remaining packets skipped"
            );
            break;
        }
        let tccp = &tccps[compno];
        offset = read_one_packet(
            comps, data, offset, layno, resno, compno, tccp, csty_sop, csty_eph,
        )?;
    }
    Ok(())
}

/// Read one packet header at `offset`, then copy its body segments.
/// Returns the new offset.
#[allow(clippy::too_many_arguments)]
fn read_one_packet(
    comps: &mut [Vec<Resolution>],
    data: &[u8],
    mut offset: usize,
    layno: u32,
    resno: usize,
    compno: usize,
    tccp: &Tccp,
    csty_sop: bool,
    csty_eph: bool,
) -> Result<usize> {
    if csty_sop && offset + 6 <= data.len() && data[offset] == 0xff && data[offset + 1] == 0x91 {
        offset += 6;
    }

    let mut bio = BioReader::new(&data[offset..]);
    let present = bio.read(1);
    // (bandno, cblkno, segno) of segments extended by this packet, in
    // header order — the body copies bytes in the same order.
    let mut new_segs: Vec<(usize, usize, usize)> = Vec::new();
    if present == 1 {
        // tag trees reset on the first layer packet (t2.c: layno == 0)
        if layno == 0 {
            let res = &mut comps[compno][resno];
            for band in res.bands.iter_mut() {
                if band.is_empty() {
                    continue;
                }
                band.incltree.reset();
                band.imsbtree.reset();
                for cblk in band.cblks.iter_mut() {
                    cblk.segs.clear();
                }
            }
        }
        let band_count = comps[compno][resno].bands.len();
        for bandno in 0..band_count {
            let band = &mut comps[compno][resno].bands[bandno];
            if band.is_empty() {
                continue;
            }
            let band_numbps = band.numbps;
            for cblkno in 0..band.cw * band.ch {
                let included = if band.cblks[cblkno].segs.is_empty() {
                    band.incltree.decode(&mut bio, cblkno, layno as i32 + 1)
                } else {
                    bio.read(1)
                };
                if included == 0 {
                    continue;
                }
                if band.cblks[cblkno].segs.is_empty() {
                    // zero-bitplane tag tree (opj_tgt_decode loop)
                    let mut i = 0i32;
                    while band.imsbtree.decode(&mut bio, cblkno, i) == 0 {
                        i += 1;
                        if i > 65535 {
                            return Err(PdfError::invalid_token(
                                offset,
                                "zero-bitplane tag tree runaway",
                            ));
                        }
                    }
                    let cblk = &mut band.cblks[cblkno];
                    cblk.numbps = (band_numbps as i32 + 1 - i).max(0) as u32;
                    cblk.numlenbits = 3;
                }
                let numpasses = getnumpasses(&mut bio);
                {
                    let cblk = &mut band.cblks[cblkno];
                    cblk.numlenbits += getcommacode(&mut bio);

                    // segment splitting (opj_t2_read_packet_header tail)
                    let mut n = numpasses as i32;
                    let mut segno = if cblk.segs.is_empty() {
                        0usize
                    } else {
                        let last = cblk.segs.len() - 1;
                        if cblk.segs[last].numpasses >= seg_maxpasses(tccp.cblk_sty, last) {
                            cblk.segs.len()
                        } else {
                            last
                        }
                    };
                    while n > 0 {
                        let mp = seg_maxpasses(tccp.cblk_sty, segno);
                        let cur = if segno < cblk.segs.len() {
                            cblk.segs[segno].numpasses
                        } else {
                            0
                        };
                        let newp = mp.saturating_sub(cur).min(n as u32).max(1);
                        let bit_number = cblk.numlenbits + floorlog2(newp);
                        if bit_number > 32 {
                            return Err(PdfError::invalid_token(
                                offset,
                                "invalid length bit count",
                            ));
                        }
                        let newlen = bio.read(bit_number);
                        if segno < cblk.segs.len() {
                            cblk.segs[segno].numpasses += newp;
                            cblk.segs[segno].pending_len = newlen;
                        } else {
                            cblk.segs.push(SegData {
                                numpasses: newp,
                                data: Vec::new(),
                                pending_len: newlen,
                            });
                        }
                        new_segs.push((bandno, cblkno, segno));
                        n -= newp as i32;
                        segno += 1;
                    }
                }
            }
        }
    }

    // byte-align; header length = consumed bytes
    bio.inalign();
    let header_len = bio.consumed();
    offset += header_len;

    if csty_eph {
        if offset + 2 <= data.len() && data[offset] == 0xff && data[offset + 1] == 0x92 {
            offset += 2;
        } else {
            log::warn!("JPX: expected EPH marker after packet header");
        }
    }

    // body: copy segment data in header order (opj_t2_read_packet_data)
    for (bandno, cblkno, segno) in new_segs {
        let seg = &mut comps[compno][resno].bands[bandno].cblks[cblkno].segs[segno];
        let len = seg.pending_len as usize;
        if offset + len > data.len() {
            return Err(PdfError::eof(offset, "packet body truncated"));
        }
        seg.data.extend_from_slice(&data[offset..offset + len]);
        offset += len;
    }
    Ok(offset)
}
