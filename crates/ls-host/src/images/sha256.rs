//! Incremental SHA-256 (FIPS 180-4) for streaming blob verification
//! (round-03 #12): `load_verified` must not slurp multi-MiB layer blobs
//! into RAM only to hash them.
//!
//! A shared incremental hasher belongs in ls-core next to `sha256_hex`;
//! that crate's lockfile/ownership is outside this round's scope, so this
//! PRIVATE twin lives here, pinned by the same known-answer vectors plus a
//! differential cross-check against ls-core's one-shot implementation.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Streaming SHA-256 state: feed bytes with `update`, read the hex digest
/// with `finalize_hex`. Buffers at most 63 bytes between calls.
pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            h: H0,
            buf: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                compress(&mut self.h, &block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            let mut b = [0u8; 64];
            b.copy_from_slice(block);
            compress(&mut self.h, &b);
            data = rest;
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    pub fn finalize_hex(mut self) -> String {
        use std::fmt::Write as _;
        let bit_len = self.total_len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0x00]);
        }
        self.update(&bit_len.to_be_bytes());
        debug_assert_eq!(self.buf_len, 0);
        let mut out = String::with_capacity(64);
        for word in self.h {
            write!(out, "{word:08x}").expect("write to string");
        }
        out
    }
}

fn compress(h: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, word) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
        (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);

    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
    h[5] = h[5].wrapping_add(f);
    h[6] = h[6].wrapping_add(g);
    h[7] = h[7].wrapping_add(hh);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_chunks(data: &[u8], chunk: usize) -> String {
        let mut h = Sha256::new();
        for c in data.chunks(chunk.max(1)) {
            h.update(c);
        }
        h.finalize_hex()
    }

    /// FIPS 180-4 / RFC 6234 known-answer vectors, including the padding
    /// boundary lengths, fed through several odd chunkings to prove buffer
    /// handling.
    #[test]
    fn sha256_known_answer_vectors() {
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (b"".to_vec(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            (b"abc".to_vec(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".to_vec(),
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                vec![b'a'; 1_000_000],
                "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
            ),
            (
                // exactly 64 bytes: one full block, padding spills to a second
                vec![b'x'; 64],
                "7ce100971f64e7001e8fe5a51973ecdfe1ced42befe7ee8d5fd6219506b5393c",
            ),
            (
                // 55 bytes: padding fits inside the same block
                vec![b'y'; 55],
                "fb66d40c3bfff05b0d5af8612d0abfbfacc6f5f26c330bc7ad634f1f44bc20ad",
            ),
        ];
        for (data, expect) in cases {
            for chunk in [1, 7, 63, 64, 65, 8191, data.len().max(1)] {
                assert_eq!(
                    hash_chunks(&data, chunk),
                    expect,
                    "KAT mismatch len={} chunk={chunk}",
                    data.len()
                );
            }
        }
    }

    /// Differential check against ls-core's one-shot hasher (the module
    /// this twin will merge into once ls-core ownership allows).
    #[test]
    fn matches_ls_core_one_shot_on_pseudo_random_inputs() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut rand = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for len in [0usize, 1, 63, 64, 65, 127, 128, 10_000, 300_000] {
            let data: Vec<u8> = (0..len).map(|_| (rand() & 0xff) as u8).collect();
            assert_eq!(
                hash_chunks(&data, 733),
                ls_core::domain::idempotency::sha256_hex(&data),
                "differential mismatch at len={len}"
            );
        }
    }
}
