//! Hand-rolled Bloom filter, one per SSTable. Consulted before opening an
//! SSTable's data section on a `get`: LSM reads are the classic "read
//! amplification" problem (a missing key would otherwise require checking
//! every on-disk generation) — a filter miss skips the file entirely with
//! zero data I/O.
use std::hash::{Hash, Hasher};

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

    /// Double hashing (Kirsch–Mitzenmacher): one SipHash pass produces two
    /// independent-enough base hashes (the key alone, and the key salted
    /// with a fixed constant), and probe `i` combines them as
    /// `h1 + i*h2 (mod m)` instead of running `k` separate hash functions.
    fn base_hashes(key: &[u8]) -> (u64, u64) {
        let mut h1 = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h1);
        let a = h1.finish();

        let mut h2 = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h2);
        0x9E3779B97F4A7C15u64.hash(&mut h2);
        let b = h2.finish();
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

#[cfg(test)]
mod tests {
    use super::*;

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
