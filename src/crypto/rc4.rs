//! RC4 stream cipher (no external dependencies).
//!
//! Used by PDF Standard Security Handler Revisions 2–4 for key lengths 40–128 bits.

/// RC4 stream cipher state.
pub struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    /// Initialise RC4 from `key`.
    ///
    /// Returns `None` when `key` is empty or longer than 256 bytes.
    pub fn new(key: &[u8]) -> Option<Self> {
        if key.is_empty() || key.len() > 256 {
            return None;
        }
        let mut s = [0u8; 256];
        for (idx, v) in s.iter_mut().enumerate() {
            *v = idx as u8;
        }
        let mut j: u8 = 0;
        for i in 0..256usize {
            j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
            s.swap(i, j as usize);
        }
        Some(Rc4 { s, i: 0, j: 0 })
    }

    /// XOR `data` in-place with the RC4 keystream.
    pub fn apply_keystream(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.s[self.i as usize]);
            self.s.swap(self.i as usize, self.j as usize);
            let k =
                self.s[(self.s[self.i as usize].wrapping_add(self.s[self.j as usize])) as usize];
            *byte ^= k;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rc4_known_vector() {
        // Test vector from Wikipedia "RC4" article:
        // Key = "Key", Plaintext = "Plaintext"
        // Expected ciphertext = BBF316E8D940AF0AD3
        let key = b"Key";
        let mut data = b"Plaintext".to_vec();
        Rc4::new(key).unwrap().apply_keystream(&mut data);
        assert_eq!(
            data,
            vec![0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]
        );
    }

    #[test]
    fn test_rc4_roundtrip() {
        let key = b"secret-key-material";
        let original = b"Hello, PDF encryption!";
        let mut ciphertext = original.to_vec();
        Rc4::new(key).unwrap().apply_keystream(&mut ciphertext);

        let mut plaintext = ciphertext.clone();
        Rc4::new(key).unwrap().apply_keystream(&mut plaintext);
        assert_eq!(plaintext, original);
    }

    #[test]
    fn test_rc4_empty_key_rejected() {
        assert!(Rc4::new(b"").is_none());
    }

    #[test]
    fn test_rc4_key_too_long_rejected() {
        assert!(Rc4::new(&[0u8; 257]).is_none());
    }
}
