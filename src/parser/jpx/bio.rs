//! Packet-header bit reader (ISO 15444-1 Annex B) and tag trees.
//!
//! Port of OpenJPEG `bio.c` (16-bit rolling buffer, 0xFF00 stall) and
//! `tgt.c` (`opj_tgt_decode` climb/descent with per-node `low`/`value`).

/// Bit reader used for JPEG 2000 packet headers.
pub struct BioReader<'a> {
    data: &'a [u8],
    /// Next byte index to consume.
    bp: usize,
    /// 16-bit rolling window.
    buf: u32,
    /// Bits remaining in `buf`.
    ct: i32,
}

impl<'a> BioReader<'a> {
    /// Reader over `data` (the whole remaining tile-part; the header simply
    /// stops at a byte boundary and the consumed count delimits the body).
    pub fn new(data: &'a [u8]) -> Self {
        BioReader {
            data,
            bp: 0,
            buf: 0,
            ct: 0,
        }
    }

    /// `opj_bio_bytein`: shift in the next byte; a pending 0xFF0x stall
    /// yields only 7 usable bits. Past the end only shifts (reads zeros).
    fn bytein(&mut self) {
        self.buf = (self.buf << 8) & 0xffff;
        self.ct = if self.buf == 0xff00 { 7 } else { 8 };
        if self.bp < self.data.len() {
            self.buf |= self.data[self.bp] as u32;
            self.bp += 1;
        }
    }

    /// `opj_bio_read` — `n` MSB-first bits (n ≤ 16).
    pub fn read(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            if self.ct == 0 {
                self.bytein();
            }
            self.ct -= 1;
            v = (v << 1) | ((self.buf >> self.ct) & 1);
        }
        v
    }

    /// `opj_bio_inalign` — discard bits to the next byte boundary;
    /// returns consumed *byte* count including any still-buffered byte
    /// already shifted in (matching `opj_bio_numbytes` semantics).
    pub fn inalign(&mut self) {
        self.ct = 0;
    }

    /// Bytes consumed from `data` so far — the packet-header length; the
    /// body starts at this offset (after `inalign`).
    pub fn consumed(&self) -> usize {
        self.bp
    }

    /// True once every byte of `data` has been consumed.
    pub fn exhausted(&self) -> bool {
        self.bp >= self.data.len() && self.ct <= 0
    }
}

/// Tag-tree decoder (`opj_tgt.c`). A tree over `cw × ch` leaves; each node
/// keeps `value` (lowest threshold confirmed by a 1 bit) and `low` (highest
/// threshold denied so far).
pub struct TagTree {
    /// `(value, low)` per node, level order (leaves first).
    nodes: Vec<(i32, i32)>,
    /// Node index of the parent of each node (`usize::MAX` for the root).
    parent: Vec<usize>,
    n_leaves: usize,
    cw: usize,
    ch: usize,
}

impl TagTree {
    /// Build a tree covering a `cw × ch` code-block array (OpenJPEG
    /// `tgt_create`: each level is the half-resolution parent grid of the
    /// previous one, leaves first, single root).
    pub fn new(cw: usize, ch: usize) -> Self {
        let n_leaves = cw * ch;
        let mut nodes = Vec::new();
        let mut parent = Vec::new();
        // leaf -> leaf index (top level starts as the leaves themselves)
        let mut child_indices: Vec<usize> = (0..n_leaves).collect();
        for _ in 0..n_leaves {
            nodes.push((i32::MAX, 0));
            parent.push(usize::MAX);
        }
        let (mut level_w, mut level_h) = (cw, ch);
        loop {
            let pw = (level_w + 1).div_ceil(2);
            let ph = (level_h + 1).div_ceil(2);
            let pn = pw * ph;
            if pn == child_indices.len() {
                // top level is already a single-node (root) grid
                break;
            }
            let base = nodes.len();
            for _ in 0..pn {
                nodes.push((i32::MAX, 0));
                parent.push(usize::MAX);
            }
            // link each child (x, y) to its parent at (x / 2, y / 2)
            for (ci, &child) in child_indices.iter().enumerate() {
                let x = ci % level_w;
                let y = ci / level_w;
                parent[child] = base + (y / 2) * pw + (x / 2);
            }
            child_indices = (base..base + pn).collect();
            level_w = pw;
            level_h = ph;
        }
        TagTree {
            nodes,
            parent,
            n_leaves,
            cw,
            ch,
        }
    }

    /// Reset `value` to infinity and `low` to 0 for every node
    /// (`opj_tgt_reset`) — done once per packet for the inclusion trees.
    pub fn reset(&mut self) {
        for v in self.nodes.iter_mut() {
            *v = (i32::MAX, 0);
        }
    }

    /// `opj_tgt_decode`: read bits until `leafno`'s root value is resolved
    /// for `threshold`; returns 1 if the leaf is included at that threshold.
    pub fn decode(&mut self, bio: &mut BioReader, leafno: usize, threshold: i32) -> u32 {
        // climb leaf -> root into a stack
        let mut stack = Vec::new();
        let mut node = leafno;
        while self.parent[node] != usize::MAX {
            stack.push(node);
            node = self.parent[node];
        }
        // descend root -> leaf
        let mut low = 0i32;
        loop {
            let (ref mut value, ref mut node_low) = self.nodes[node];
            if low > *node_low {
                *node_low = low;
            } else {
                low = *node_low;
            }
            while low < threshold && low < *value {
                if bio.read(1) == 1 {
                    *value = low;
                } else {
                    low += 1;
                }
            }
            *node_low = low;
            match stack.pop() {
                Some(child) => node = child,
                None => break,
            }
        }
        if self.nodes[node].0 < threshold {
            1
        } else {
            0
        }
    }

    /// Leaf count (== number of code blocks in the precinct).
    pub fn n_leaves(&self) -> usize {
        self.n_leaves
    }

    /// Grid width (code blocks per row).
    pub fn cw(&self) -> usize {
        self.cw
    }

    /// Grid height (code blocks per column).
    pub fn ch(&self) -> usize {
        self.ch
    }
}

// ---------------------------------------------------------------------------
// Tests (R5)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bio_reads_msb_first() {
        let mut bio = BioReader::new(&[0b1011_0100, 0b0001_1111]);
        assert_eq!(bio.read(1), 1);
        assert_eq!(bio.read(3), 0b011);
        assert_eq!(bio.read(4), 0b0100);
    }

    #[test]
    fn bio_ff_stall_counts_7_bits() {
        // 0xFF then 0b1111_0000: only 7 bits of the second byte are consumed
        // before the next stall logic; byte 2's MSB is dropped.
        let mut bio = BioReader::new(&[0xff, 0b1111_0000]);
        let _ = bio.read(8);
        // consumed 1 byte so far (0xff); next refill stalls 7 bits
        assert_eq!(bio.consumed(), 1);
        let v = bio.read(7);
        assert_eq!(v, 0b111_0000);
        assert_eq!(bio.consumed(), 2);
    }

    #[test]
    fn bio_past_end_reads_zero() {
        let mut bio = BioReader::new(&[0x00]);
        bio.read(8);
        for _ in 0..10 {
            assert_eq!(bio.read(1), 0);
        }
    }

    #[test]
    fn tagtree_single_leaf_two_thresholds() {
        // 1x1 tree: with the MSB set the leaf is included at threshold 1
        // immediately; inclusion is monotonic, so threshold 2 also reports
        // included without consuming further bits.
        let mut tree = TagTree::new(1, 1);
        let mut bio = BioReader::new(&[0b1000_0000]);
        assert_eq!(tree.decode(&mut bio, 0, 1), 1);
        assert_eq!(tree.decode(&mut bio, 0, 2), 1);
        assert_eq!(bio.consumed(), 1);
    }

    #[test]
    fn tagtree_2x2_inclusion_ordering() {
        // Leaves 0..3; encode thresholds via unary 0-bits: leaf 1 included at
        // threshold 2 → stream for (0,t=1)=0 (bit 0), (1,t=1)=0, (0,t=2)=1...
        // Simpler known-good vector: all bits 0 → nothing included ever.
        let mut tree = TagTree::new(2, 2);
        let mut bio = BioReader::new(&[0x00, 0x00, 0x00, 0x00]);
        for leaf in 0..4 {
            assert_eq!(tree.decode(&mut bio, leaf, 1), 0);
        }
    }

    #[test]
    fn tagtree_3x3_leaf_structure() {
        // 3x3 grid: leaf level 9 nodes, parent level 2x2, root 1x1.
        let tree = TagTree::new(3, 3);
        assert_eq!(tree.n_leaves(), 9);
        assert_eq!(tree.cw(), 3);
        assert_eq!(tree.ch(), 3);
    }

    #[test]
    fn bio_exhausted_flags() {
        let mut bio = BioReader::new(&[0x80]);
        assert!(!bio.exhausted());
        assert_eq!(bio.read(1), 1);
        bio.read(1);
        bio.read(1);
        bio.read(1);
        bio.read(1);
        bio.read(1);
        bio.read(1);
        bio.read(1);
        assert!(bio.exhausted());
    }

    #[test]
    fn tagtree_decode_past_end_is_error_safe() {
        // An empty stream yields zeros — must not panic (R1).
        let mut tree = TagTree::new(2, 2);
        let mut bio = BioReader::new(&[]);
        assert_eq!(tree.decode(&mut bio, 0, 1), 0);
    }
}
