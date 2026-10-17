//! SHA-256 (FIPS 180-4), which a download pin names its file by. Written here, because a hash crate
//! would be one more crate to trust for the one function that decides whether a fetch is trusted.

use std::io::Read;

const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

const START: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// A hash in progress: the state, the bytes not yet a whole block, and the length so far.
pub struct Hasher {
    state: [u32; 8],
    block: [u8; 64],
    filled: usize,
    length: u64,
}

impl Default for Hasher {
    fn default() -> Self {
        Self {
            state: START,
            block: [0; 64],
            filled: 0,
            length: 0,
        }
    }
}

impl Hasher {
    pub fn update(&mut self, bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        for &byte in bytes {
            if let Some(slot) = self.block.get_mut(self.filled) {
                *slot = byte;
            }
            self.filled += 1;
            if self.filled == 64 {
                compress(&mut self.state, &self.block);
                self.filled = 0;
            }
        }
    }

    /// The hash as 64 lowercase hex digits, the form a pin file and `sha256sum` write.
    #[must_use]
    pub fn hex(mut self) -> String {
        let bits = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        let tail = bits.to_be_bytes();
        self.block[56..].copy_from_slice(&tail);
        compress(&mut self.state, &self.block);
        self.state
            .iter()
            .map(|word| format!("{word:08x}"))
            .collect()
    }
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes(*word);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (word, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *word = word.wrapping_add(add);
    }
}

/// The hash of `bytes`.
#[must_use]
pub fn of(bytes: &[u8]) -> String {
    let mut hasher = Hasher::default();
    hasher.update(bytes);
    hasher.hex()
}

/// The hash of a file, read a block at a time so a 200 MB archive is never held whole.
pub fn of_file(path: &std::path::Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Hasher::default();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hasher.hex());
        }
        hasher.update(buffer.get(..read).unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_vectors_hash_as_fips_180_4_says() {
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        let hashed = [of(b""), of(b"abc"), of(long)];
        let said = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 \
                    ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad \
                    248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1";
        assert_eq!(hashed.join(" "), said);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri takes 15 minutes to hash a million bytes")]
    fn a_million_bytes_hash_as_fips_180_4_says() {
        let said = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";
        assert_eq!(of(&[b'a'; 1_000_000]), said);
    }

    #[test]
    fn a_hash_fed_in_pieces_equals_the_hash_of_the_whole() {
        let bytes: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let mut hasher = Hasher::default();
        for piece in bytes.chunks(63) {
            hasher.update(piece);
        }
        assert_eq!(hasher.hex(), of(&bytes));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    #[allow(
        clippy::unwrap_used,
        reason = "a failed unwrap in a test is the test failing"
    )]
    fn a_file_hashes_as_its_bytes_and_a_missing_one_is_an_error() {
        let dir = crate::testdir::make("sha256-file");
        let bytes: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
        std::fs::write(dir.join("f"), &bytes).unwrap();
        assert_eq!(of_file(&dir.join("f")).unwrap(), of(&bytes));
        assert!(of_file(&dir.join("gone")).is_err());
    }
}
