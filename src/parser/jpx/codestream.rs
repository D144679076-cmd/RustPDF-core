//! JPEG 2000 codestream parsing (ISO 15444-1 Annex A) — main header
//! markers, tile parts, and the wavelet-lite feature rejects.
//!
//! Pure Rust (R2); every failure carries a byte offset (R7). A leading JP2
//! container is tolerated by locating the `jp2c` box; the PDF `/JPXDecode`
//! payload is normally a bare codestream.

use crate::error::{PdfError, Result};

// Marker codes (Annex A).
const SOC: u16 = 0xff4f;
const SIZ: u16 = 0xff51;
const COD: u16 = 0xff52;
const COC: u16 = 0xff53;
const QCD: u16 = 0xff5c;
const QCC: u16 = 0xff5d;
const RGN: u16 = 0xff5e;
const POC: u16 = 0xff5f;
const TLM: u16 = 0xff55;
const PLM: u16 = 0xff57;
const PLT: u16 = 0xff58;
const PPM: u16 = 0xff60;
const COM: u16 = 0xff64;
const SOT: u16 = 0xff90;
const SOD: u16 = 0xff93;
const EOC: u16 = 0xffd9;

/// Progression orders (SGcod / COC): LRCP, RLCP, RPCL, PCRL, CPRL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgOrder {
    Lrcp,
    Rlcp,
    RpcL,
    Pcrl,
    Cprl,
}

/// Per-component image info from SIZ (Ssiz / XRsiz / YRsiz).
#[derive(Debug, Clone, Copy)]
pub struct CompInfo {
    /// Precision in bits (1–38; we accept up to 16 and shift to 8 on output).
    pub prec: u8,
    pub sgnd: bool,
    /// Subsampling factors.
    pub xr: u32,
    pub yr: u32,
}

/// Coding parameters shared per component (COD/COC + QCD/QCC merge).
#[derive(Debug, Clone)]
pub struct Tccp {
    pub num_layers: u32,
    pub mct: bool,
    pub prog: ProgOrder,
    pub num_decomps: u32,
    /// Code-block width/height exponents-2 (actual size = 1 << (e+2)).
    pub cbw_exp: u32,
    pub cbh_exp: u32,
    pub cblk_sty: u8,
    /// Quantization style from Sqcd low 5 bits (0 = reversible / none).
    pub qntsty: u8,
    /// Number of guard bits (Sqcd >> 5).
    pub numgbits: u8,
    /// Step-size exponent per subband (`expn`); index = res0:0, res r band b:
    /// `3 * (r - 1) + b + 1`.
    pub expn: Vec<u8>,
}

impl Default for Tccp {
    fn default() -> Self {
        Tccp {
            num_layers: 1,
            mct: false,
            prog: ProgOrder::Lrcp,
            num_decomps: 0,
            cbw_exp: 4, // 64
            cbh_exp: 4,
            cblk_sty: 0,
            qntsty: 0,
            numgbits: 2,
            expn: Vec::new(),
        }
    }
}

/// Parsed codestream header + tile-part payload list.
#[derive(Debug)]
pub struct Codestream {
    /// Image grid from SIZ.
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub comps: Vec<CompInfo>,
    pub tccps: Vec<Tccp>,
    /// Tile size and tile origin.
    pub x_tsiz: u32,
    pub y_tsiz: u32,
    pub x_tosiz: u32,
    pub y_tosiz: u32,
    /// Tile-part payloads grouped by tile index, in order of appearance.
    pub tile_parts: Vec<(u32, Vec<u8>)>,
    /// COD segment style (Scod) — SOP/EPH presence.
    pub csty_sop: bool,
    pub csty_eph: bool,
}

fn u16be(d: &[u8], off: usize) -> u16 {
    ((d[off] as u16) << 8) | d[off + 1] as u16
}
fn u32be(d: &[u8], off: usize) -> u32 {
    ((d[off] as u32) << 24)
        | ((d[off + 1] as u32) << 16)
        | ((d[off + 2] as u32) << 8)
        | d[off + 3] as u32
}

fn err<T>(off: usize, msg: impl Into<String>) -> Result<T> {
    Err(PdfError::invalid_token(off, msg))
}

/// Parse SIZ: Rsiz..Csiz + per-component Ssiz/XRsiz/YRsiz.
fn parse_siz(d: &[u8], off: usize, len: usize, cs: &mut Codestream, base: usize) -> Result<()> {
    // body length after Lsiz: Rsiz(2) + grid(32) + Csiz(2) + 3*ncomp
    if len < 36 {
        return err(base + off, "SIZ too short");
    }
    let _rsiz = u16be(d, off);
    cs.x1 = u32be(d, off + 2);
    cs.y1 = u32be(d, off + 6);
    cs.x0 = u32be(d, off + 10);
    cs.y0 = u32be(d, off + 14);
    let x_tsiz = u32be(d, off + 18);
    let y_tsiz = u32be(d, off + 22);
    let x_tosiz = u32be(d, off + 26);
    let y_tosiz = u32be(d, off + 30);
    let ncomp = u16be(d, off + 34) as usize;
    if x_tsiz == 0 || y_tsiz == 0 || ncomp == 0 || ncomp > 256 {
        return err(base + off, "invalid SIZ dimensions");
    }
    if len < 36 + 3 * ncomp {
        return err(base + off, "SIZ component list truncated");
    }
    let mut comps = Vec::with_capacity(ncomp);
    for i in 0..ncomp {
        let ssiz = d[off + 36 + 3 * i];
        comps.push(CompInfo {
            prec: (ssiz & 0x7f) + 1,
            sgnd: ssiz & 0x80 != 0,
            xr: d[off + 36 + 3 * i + 1] as u32,
            yr: d[off + 36 + 3 * i + 2] as u32,
        });
    }
    cs.x_tsiz = x_tsiz;
    cs.y_tsiz = y_tsiz;
    cs.x_tosiz = x_tosiz;
    cs.y_tosiz = y_tosiz;
    cs.comps = comps;
    Ok(())
}

/// Parse COD or COC (COC adds the component number first).
fn parse_cod(
    d: &[u8],
    off: usize,
    _len: usize,
    coc: bool,
    cs: &mut Codestream,
    base: usize,
) -> Result<usize> {
    let mut p = off;
    let compno: Option<usize> = if coc {
        // component number: 1 byte when Csiz < 257, else 2 bytes
        let c = if cs.comps.len() < 257 {
            let c = d[p] as usize;
            p += 1;
            c
        } else {
            let c = u16be(d, p) as usize;
            p += 2;
            c
        };
        if c >= cs.comps.len() {
            return err(base + p, "COC component out of range");
        }
        Some(c)
    } else {
        None
    };
    let scod = d[p];
    p += 1;
    let prog_raw = d[p];
    let num_layers = u16be(d, p + 1) as u32;
    let mct = d[p + 3] != 0;
    p += 4;
    let num_decomps = d[p] as u32;
    let cbw_exp = d[p + 1] as u32;
    let cbh_exp = d[p + 2] as u32;
    let cblk_sty = d[p + 3];
    let transform = d[p + 4];
    p += 5;
    if transform != 1 {
        return err(
            base + p,
            format!("9-7 (irreversible) transform not supported; transform={transform}"),
        );
    }
    if scod & 0x01 != 0 {
        // precinct sizes defined — reject (wavelet-lite: single precinct)
        return err(base + p, "precinct partitioning (Scod bit 0) not supported");
    }
    if cbw_exp > 12 || cbh_exp > 12 || num_decomps > 16 {
        return err(base + p, "COD sizes out of range");
    }
    let prog = match prog_raw {
        0 => ProgOrder::Lrcp,
        1 => ProgOrder::Rlcp,
        2 => ProgOrder::RpcL,
        3 => ProgOrder::Pcrl,
        4 => ProgOrder::Cprl,
        other => return err(base + p, format!("unknown progression order {other}")),
    };
    let apply = |tccp: &mut Tccp| {
        tccp.num_layers = num_layers;
        tccp.mct = mct;
        tccp.prog = prog;
        tccp.num_decomps = num_decomps;
        tccp.cbw_exp = cbw_exp;
        tccp.cbh_exp = cbh_exp;
        tccp.cblk_sty = cblk_sty;
    };
    if coc {
        apply(&mut cs.tccps[compno.unwrap()]);
    } else {
        for t in cs.tccps.iter_mut() {
            apply(t);
        }
    }
    cs.csty_sop = scod & 0x02 != 0;
    cs.csty_eph = scod & 0x04 != 0;
    Ok(p - off)
}

/// Parse QCD/QCC: Sqcd + SPqcd list.
fn parse_qcd(
    d: &[u8],
    off: usize,
    len: usize,
    qcc: bool,
    cs: &mut Codestream,
    base: usize,
) -> Result<usize> {
    let mut p = off;
    let compno: Option<usize> = if qcc {
        // QCC component number: 1 byte (Csiz < 257) or 2 bytes
        let c = if cs.comps.len() < 257 {
            let c = d[p] as usize;
            p += 1;
            c
        } else {
            let c = u16be(d, p) as usize;
            p += 2;
            c
        };
        if c >= cs.comps.len() {
            return err(base + p, "QCC component out of range");
        }
        Some(c)
    } else {
        None
    };
    let sqcd = d[p];
    p += 1;
    let qntsty = sqcd & 0x1f;
    let numgbits = sqcd >> 5;
    let n_bands = match compno {
        Some(c) => 3 * cs.tccps[c].num_decomps as usize + 1,
        None => 3 * cs.tccps[0].num_decomps as usize + 1,
    };
    let mut expn = Vec::new();
    match qntsty {
        0 => {
            // none (reversible): 1 byte per band, expn = byte >> 3
            let want = n_bands;
            if p + want > off + len {
                return err(base + p, "QCD SPqcd list truncated (no-quantization)");
            }
            for i in 0..want {
                expn.push(d[p + i] >> 3);
            }
            p += want;
        }
        1 => {
            // scalar derived: single 2-byte value shared by all bands
            if p + 2 > off + len {
                return err(base + p, "QCD SPqcd truncated (scalar derived)");
            }
            let v = u16be(d, p);
            let e = (v >> 11) as u8;
            expn.resize(n_bands, e);
            p += 2;
        }
        2 => {
            // scalar expounded: 2 bytes per band
            let want = n_bands;
            if p + 2 * want > off + len {
                return err(base + p, "QCD SPqcd list truncated (expounded)");
            }
            for i in 0..want {
                expn.push((u16be(d, p + 2 * i) >> 11) as u8);
            }
            p += 2 * want;
        }
        other => return err(base + p, format!("unknown quantization style {other}")),
    }
    // reversible codestream requires no-quantization style
    if qntsty != 0 {
        return err(
            base + p,
            "quantized (9-7 style) codestream not supported by wavelet-lite decoder",
        );
    }
    let targets: &mut [Tccp] = match compno {
        Some(c) => &mut cs.tccps[c..c + 1],
        None => &mut cs.tccps[..],
    };
    for t in targets.iter_mut() {
        t.qntsty = qntsty;
        t.numgbits = numgbits;
        t.expn = expn.clone();
    }
    Ok(p - off)
}

/// Parse a codestream: optional JP2 wrapper, main header, tile parts.
pub fn parse(data: &[u8]) -> Result<Codestream> {
    // JP2 container: signature box (12 bytes) then boxes; find 'jp2c'.
    let mut body = data;
    if data.len() >= 12 && &data[4..12] == b"jP  \x0d\x0a\x87\x0a" {
        let mut p = 0usize;
        let mut found = None;
        while p + 8 <= data.len() {
            let blen = u32be(data, p) as usize;
            if blen < 8 || p + blen > data.len() {
                return err(p, "invalid JP2 box length");
            }
            if &data[p + 4..p + 8] == b"jp2c" {
                found = Some(p + 8);
                break;
            }
            if blen == 0 {
                break;
            }
            p += blen;
        }
        match found {
            Some(s) => body = &data[s..],
            None => return err(0, "JP2 container without jp2c codestream box"),
        }
    }
    if body.len() < 2 || u16be(body, 0) != SOC {
        return err(0, "codestream must start with SOC (0xff4f)");
    }

    let mut cs = Codestream {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
        comps: Vec::new(),
        tccps: Vec::new(),
        x_tsiz: 0,
        y_tsiz: 0,
        x_tosiz: 0,
        y_tosiz: 0,
        tile_parts: Vec::new(),
        csty_sop: false,
        csty_eph: false,
    };

    let mut p = 2usize;
    let mut saw_siz = false;
    let mut saw_cod = false;
    let mut saw_qcd = false;
    while p < body.len() {
        let marker = u16be(body, p);
        let seg_start = p;
        if marker == SOC {
            return err(p, "unexpected SOC inside codestream");
        }
        if marker == EOC {
            p += 2;
            break;
        }
        if marker == SOT {
            p += 2;
            p = parse_tile_part(body, p, &mut cs, seg_start)?;
            continue;
        }
        // marker segment: 2-byte marker + 2-byte length
        if p + 4 > body.len() {
            return err(p, "truncated marker segment");
        }
        let len = u16be(body, p + 2) as usize;
        if len < 2 || p + 2 + len > body.len() {
            return err(p + 2, "invalid marker segment length");
        }
        let seg_body = p + 4; // after Ls
        match marker {
            SIZ => {
                parse_siz(body, seg_body, len - 2, &mut cs, seg_start)?;
                cs.tccps = vec![Tccp::default(); cs.comps.len()];
                saw_siz = true;
            }
            COD => {
                if !saw_siz {
                    return err(p, "COD before SIZ");
                }
                parse_cod(body, seg_body, len - 2, false, &mut cs, seg_start)?;
                saw_cod = true;
            }
            COC => {
                if !saw_cod {
                    return err(p, "COC before COD");
                }
                parse_cod(body, seg_body, len - 2, true, &mut cs, seg_start)?;
            }
            QCD => {
                if !saw_cod {
                    return err(p, "QCD before COD");
                }
                parse_qcd(body, seg_body, len - 2, false, &mut cs, seg_start)?;
                saw_qcd = true;
            }
            QCC => {
                if !saw_qcd {
                    return err(p, "QCC before QCD");
                }
                parse_qcd(body, seg_body, len - 2, true, &mut cs, seg_start)?;
            }
            RGN => {
                log::warn!("JPX: ROI (RGN marker) present; decoding without ROI scaling");
            }
            POC => {
                return err(p, "progression order change (POC) not supported");
            }
            PPM => {
                return err(p, "packed packet headers (PPM) not supported");
            }
            TLM | PLM | PLT | COM => { /* informational: skip */ }
            other => {
                return err(p, format!("unknown marker 0x{other:04x}"));
            }
        }
        p = seg_body + len - 2;
    }
    if !saw_siz || !saw_cod || !saw_qcd {
        return err(p, "missing required marker (SIZ/COD/QCD)");
    }
    if cs.tile_parts.is_empty() {
        return err(p, "codestream contains no tile parts");
    }
    Ok(cs)
}

/// Parse one tile part starting after the SOT marker; returns offset past
/// the part's data. SOT: Lsot=10, Isot, Psot, TPsot, TNsot; then SOD; then
/// data up to Psot (0 = until next marker/EOC).
fn parse_tile_part(
    body: &[u8],
    mut p: usize,
    cs: &mut Codestream,
    seg_start: usize,
) -> Result<usize> {
    if p + 12 > body.len() {
        return err(seg_start, "truncated SOT segment");
    }
    let lsot = u16be(body, p);
    if lsot != 10 {
        return err(seg_start, format!("invalid SOT length {lsot}"));
    }
    let isot = u16be(body, p + 2) as u32;
    let psot = u32be(body, p + 4);
    let _tpsot = body[p + 8];
    let _tnsot = body[p + 9];
    p += 10;
    if u16be(body, p) != SOD {
        return err(p, "SOT not followed by SOD");
    }
    p += 2;
    let data_end = if psot == 0 {
        // runs to EOC or next marker (0xff followed by non-data)
        let mut e = p;
        while e + 1 < body.len() {
            if body[e] == 0xff && (body[e + 1] == 0x90 || body[e + 1] == 0xd9) {
                break;
            }
            e += 1;
        }
        e
    } else {
        let start = seg_start;
        if psot as usize > body.len() - start {
            return err(seg_start, "Psot extends past codestream end");
        }
        start + psot as usize
    };
    if data_end < p {
        return err(seg_start, "negative tile-part length");
    }
    cs.tile_parts.push((isot, body[p..data_end].to_vec()));
    Ok(data_end)
}

// ---------------------------------------------------------------------------
// Tests (R5)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid codestream: 1 component, 8-bit unsigned,
    /// 0 decomposition levels, 1 layer, LRCP, one tile part whose packet
    /// says "not present" (single 0x00 byte).
    fn minimal_codestream() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&SOC.to_be_bytes());
        // SIZ
        let mut siz = Vec::new();
        siz.extend_from_slice(&0u16.to_be_bytes()); // Rsiz
        siz.extend_from_slice(&1u32.to_be_bytes()); // Xsiz
        siz.extend_from_slice(&1u32.to_be_bytes()); // Ysiz
        siz.extend_from_slice(&0u32.to_be_bytes()); // XOsiz
        siz.extend_from_slice(&0u32.to_be_bytes()); // YOsiz
        siz.extend_from_slice(&1u32.to_be_bytes()); // XTsiz
        siz.extend_from_slice(&1u32.to_be_bytes()); // YTsiz
        siz.extend_from_slice(&0u32.to_be_bytes()); // XTOsiz
        siz.extend_from_slice(&0u32.to_be_bytes()); // YTOsiz
        siz.extend_from_slice(&1u16.to_be_bytes()); // Csiz
        siz.push(7); // Ssiz: 8-bit unsigned
        siz.push(1); // XRsiz
        siz.push(1); // YRsiz
        d.extend_from_slice(&SIZ.to_be_bytes());
        d.extend_from_slice(&(siz.len() as u16 + 2).to_be_bytes());
        d.extend_from_slice(&siz);
        // COD: Scod=0, prog=0, layers=1, mct=0, decomps=0, cbw=4 (64),
        // cbh=4, cblksty=0, transform=1 (5-3)
        let mut cod = vec![0u8, 0, 0, 1, 0, 0, 4, 4, 0, 1];
        d.extend_from_slice(&COD.to_be_bytes());
        d.extend_from_slice(&(cod.len() as u16 + 2).to_be_bytes());
        d.append(&mut cod);
        // QCD: Sqcd = guard 2, style 0 (reversible); 1 band, expn = 8
        let qcd = vec![0x40u8, 8 << 3];
        d.extend_from_slice(&QCD.to_be_bytes());
        d.extend_from_slice(&(qcd.len() as u16 + 2).to_be_bytes());
        d.extend_from_slice(&qcd);
        // SOT + SOD + one absent packet + EOC
        let packet = [0x00u8];
        let psot = 12u32 + 2 + packet.len() as u32;
        d.extend_from_slice(&SOT.to_be_bytes());
        d.extend_from_slice(&10u16.to_be_bytes());
        d.extend_from_slice(&0u16.to_be_bytes()); // Isot
        d.extend_from_slice(&psot.to_be_bytes());
        d.push(0); // TPsot
        d.push(1); // TNsot
        d.extend_from_slice(&SOD.to_be_bytes());
        d.extend_from_slice(&packet);
        d.extend_from_slice(&EOC.to_be_bytes());
        d
    }

    #[test]
    fn minimal_codestream_parses() {
        let cs = parse(&minimal_codestream()).unwrap();
        assert_eq!(cs.x1, 1);
        assert_eq!(cs.y1, 1);
        assert_eq!(cs.comps.len(), 1);
        assert_eq!(cs.comps[0].prec, 8);
        assert!(!cs.comps[0].sgnd);
        assert_eq!(cs.tccps[0].num_decomps, 0);
        assert_eq!(cs.tccps[0].expn.len(), 1);
        assert_eq!(cs.tccps[0].expn[0], 8);
        assert_eq!(cs.tile_parts.len(), 1);
        assert_eq!(cs.tile_parts[0].0, 0);
        assert_eq!(cs.tile_parts[0].1, vec![0x00]);
    }

    #[test]
    fn rejects_97_transform() {
        let mut d = minimal_codestream();
        // SPcod transform byte: SOC(2)+SIZ seg+COD seg → locate last byte
        // of COD payload (transform). COD marker found by scanning.
        let pos = d.windows(2).position(|w| w == COD.to_be_bytes()).unwrap();
        let lcod = u16be(&d, pos + 2) as usize;
        d[pos + 2 + lcod - 1] = 0; // transform = 0 (9-7)
        let e = parse(&d).unwrap_err();
        assert!(format!("{e}").contains("9-7"));
    }

    #[test]
    fn rejects_precinct_partition() {
        let mut d = minimal_codestream();
        let pos = d.windows(2).position(|w| w == COD.to_be_bytes()).unwrap();
        d[pos + 4] = 0x01; // Scod bit 0
        assert!(parse(&d).is_err());
    }

    #[test]
    fn rejects_quantized_qcd() {
        let mut d = minimal_codestream();
        let pos = d.windows(2).position(|w| w == QCD.to_be_bytes()).unwrap();
        d[pos + 4] = 0x42; // Sqcd: guard 2, style 2 (expounded)
        assert!(parse(&d).is_err());
    }

    #[test]
    fn rejects_unknown_marker() {
        let mut d = minimal_codestream();
        // overwrite COD marker with something unknown
        let pos = d.windows(2).position(|w| w == [0xff, 0x52]).unwrap();
        d[pos] = 0xff;
        d[pos + 1] = 0x77;
        assert!(parse(&d).is_err());
    }

    #[test]
    fn rejects_truncated_sot() {
        let mut d = minimal_codestream();
        d.truncate(d.len() - 8); // cut into the tile part
        assert!(parse(&d).is_err());
    }

    #[test]
    fn rejects_missing_siz() {
        let d = vec![0xff, 0x4f, 0xff, 0xd9];
        assert!(parse(&d).is_err());
    }

    #[test]
    fn rejects_bad_sot_length() {
        let mut d = minimal_codestream();
        let pos = d.windows(2).position(|w| w == [0xff, 0x90]).unwrap();
        d[pos + 3] = 0x08; // Lsot = 0x0008 (wrong)
        assert!(parse(&d).is_err());
    }

    #[test]
    fn jp2_container_unwrapped() {
        let cs_inner = minimal_codestream();
        let mut d = Vec::new();
        // signature box
        d.extend_from_slice(&0x0000000cu32.to_be_bytes());
        d.extend_from_slice(b"jP  ");
        d.extend_from_slice(&[0x0d, 0x0a, 0x87, 0x0a]);
        // jp2c box
        d.extend_from_slice(&(8 + cs_inner.len() as u32).to_be_bytes());
        d.extend_from_slice(b"jp2c");
        d.extend_from_slice(&cs_inner);
        let cs = parse(&d).unwrap();
        assert_eq!(cs.x1, 1);
    }
}
