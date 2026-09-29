//! Hand-rolled Bloom filter, one per SSTable. Consulted before opening an
//! SSTable's data section on a `get`: LSM reads are the classic "read
//! amplification" problem (a missing key would otherwise require checking
//! every on-disk generation) — a filter miss skips the file entirely with
//! zero data I/O.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BloomFilter {
    m: u32, // number of bits
    k: u32, // number of hash functions
    bits: Vec<u8>,
}

impl BloomFilter {
    /// Sizes the filter for `expected_items` entries at a target
    /// `false_positive_rate` (e.g. `0.01` for 1%), using the standard
    /// formulas `m = ceil(-n * ln(p) / ln(2)^2)`, `k = round((m/n) * ln(2))`.
    pub fn new(expected_items: usize, false_positive_rate: f64) -> Self {
        let n = (expected_items.max(1)) as f64;
        let p = false_positive_rate.clamp(1e-6, 0.5);
        let m = ((-(n * p.ln())) / std::f64::consts::LN_2.powi(2)).ceil() as u32;
        let m = m.max(64);
        let k = (((m as f64 / n) * std::f64::consts::LN_2).round() as u32).clamp(1, 32);
        let byte_len = m.div_ceil(8) as usize;
        Self {
            m,
            k,
            bits: vec![0u8; byte_len],
        }
    }

    /// Double hashing (Kirsch–Mitzenmacher): two independent-enough base
    /// hashes (the key alone, and the key salted with a fixed constant),
    /// and probe `i` combines them as `h1 + i*h2 (mod m)` instead of running
    /// `k` separate hash functions.
    ///
    /// Filters are persisted inside SSTables, so this hash must never change.
    /// It reproduces, byte for byte, what `std`'s `DefaultHasher::new()`
    /// (SipHash-1-3, zero keys) produced for `key.hash(..)` on 64-bit
    /// little-endian targets when the format was created, but is frozen
    /// here instead of depending on `std`, whose algorithm is explicitly
    /// unspecified and may change between Rust releases.
    fn base_hashes(key: &[u8]) -> (u64, u64) {
        // `<[u8] as Hash>::hash` writes a usize length prefix before the bytes.
        let len_prefix = (key.len() as u64).to_le_bytes();
        let a = sip13(&[&len_prefix, key]);
        let b = sip13(&[&len_prefix, key, &0x9E3779B97F4A7C15u64.to_le_bytes()]);
        (a, b)
    }

    fn probe_bits<'k>(&self, key: &'k [u8]) -> impl Iterator<Item = u32> + 'k {
        let (h1, h2) = Self::base_hashes(key);
        let m = self.m as u64;
        (0..self.k).map(move |i| (h1.wrapping_add((i as u64).wrapping_mul(h2)) % m) as u32)
    }

    pub fn insert(&mut self, key: &[u8]) {
        for bit in self.probe_bits(key).collect::<Vec<_>>() {
            let byte = (bit / 8) as usize;
            self.bits[byte] |= 1 << (bit % 8);
        }
    }

    pub fn might_contain(&self, key: &[u8]) -> bool {
        self.probe_bits(key).all(|bit| {
            let byte = (bit / 8) as usize;
            self.bits[byte] & (1 << (bit % 8)) != 0
        })
    }
}

/// SipHash-1-3 with keys `(0, 0)` over the concatenation of `parts`.
fn sip13(parts: &[&[u8]]) -> u64 {
    #[inline]
    fn round(v: &mut [u64; 4]) {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    }
    fn compress(v: &mut [u64; 4], m: u64) {
        v[3] ^= m;
        round(v);
        v[0] ^= m;
    }

    let mut v = [
        0x736f6d6570736575u64,
        0x646f72616e646f6du64,
        0x6c7967656e657261u64,
        0x7465646279746573u64,
    ];
    let mut buf = [0u8; 8];
    let mut buf_len = 0usize;
    let mut total = 0u64;
    for part in parts {
        for &byte in *part {
            buf[buf_len] = byte;
            buf_len += 1;
            if buf_len == 8 {
                compress(&mut v, u64::from_le_bytes(buf));
                buf_len = 0;
            }
        }
        total += part.len() as u64;
    }
    let mut last = (total & 0xff) << 56;
    for (i, &byte) in buf[..buf_len].iter().enumerate() {
        last |= (byte as u64) << (8 * i);
    }
    compress(&mut v, last);
    v[2] ^= 0xff;
    for _ in 0..3 {
        round(&mut v);
    }
    v[0] ^ v[1] ^ v[2] ^ v[3]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Existing `.bkndb` files were written with `DefaultHasher`; the frozen
    /// implementation must agree with it or their filters would report
    /// present keys as absent.
    #[cfg(all(target_pointer_width = "64", target_endian = "little"))]
    #[test]
    fn frozen_hash_matches_the_std_hasher_existing_files_were_written_with() {
        use std::hash::{Hash, Hasher};
        for key in [&b""[..], b"a", b"1234567", b"12345678", b"some-longer-key-spanning-several-words"] {
            let mut h1 = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut h1);
            let mut h2 = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut h2);
            0x9E3779B97F4A7C15u64.hash(&mut h2);
            assert_eq!(BloomFilter::base_hashes(key), (h1.finish(), h2.finish()), "key {key:?}");
        }
    }

    #[test]
    fn no_false_negatives() {
        let keys: Vec<Vec<u8>> = (0..2000u32).map(|i| i.to_be_bytes().to_vec()).collect();
        let mut filter = BloomFilter::new(keys.len(), 0.01);
        for k in &keys {
            filter.insert(k);
        }
        for k in &keys {
            assert!(filter.might_contain(k), "inserted key must never be reported absent");
        }
    }

    #[test]
    fn false_positive_rate_stays_within_a_generous_bound() {
        let keys: Vec<Vec<u8>> = (0..5000u32).map(|i| i.to_be_bytes().to_vec()).collect();
        let mut filter = BloomFilter::new(keys.len(), 0.01);
        for k in &keys {
            filter.insert(k);
        }
        // Disjoint sample: large negative offset keeps these out of the
        // inserted range's byte pattern space.
        let probes: Vec<Vec<u8>> = (0..5000u32).map(|i| (i + 10_000_000).to_be_bytes().to_vec()).collect();
        let false_positives = probes.iter().filter(|k| filter.might_contain(k)).count();
        let rate = false_positives as f64 / probes.len() as f64;
        assert!(rate < 0.05, "false positive rate {rate} should stay well under a generous 5% bound (target was 1%)");
    }
}
