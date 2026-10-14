// SPDX-License-Identifier: MIT OR Apache-2.0
//! A small, seedable pseudo-random number generator (SplitMix64).
//!
//! Not cryptographic. It only has to be fast, allocation-free and give the
//! same sequence for the same seed on every platform.

#[derive(Clone, Debug)]
pub(crate) struct Rng {
    state: u64,
}

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform value in `[0, 1)`.
    pub(crate) fn next_f32(&mut self) -> f32 {
        // The top 24 bits fill an f32 mantissa exactly.
        (self.next_u64() >> 40) as f32 * (1.0 / (1u32 << 24) as f32)
    }

    /// A uniform index in `0..n`. Returns 0 when `n` is 0.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        if n <= 1 {
            return 0;
        }
        let n = n as u64;
        // Lemire's multiply-and-reject method: unbiased, rarely loops.
        let threshold = n.wrapping_neg() % n;
        loop {
            let m = u128::from(self.next_u64()) * u128::from(n);
            if (m as u64) >= threshold {
                return (m >> 64) as usize;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_ne!(Rng::new(1).next_u64(), Rng::new(2).next_u64());
    }

    #[test]
    fn ranges_are_respected() {
        let mut rng = Rng::new(7);
        let mut seen = [0usize; 5];
        for _ in 0..5000 {
            let f = rng.next_f32();
            assert!((0.0..1.0).contains(&f));
            seen[rng.below(5)] += 1;
        }
        assert!(seen.iter().all(|&count| count > 800), "{seen:?}");
        assert_eq!(rng.below(0), 0);
        assert_eq!(rng.below(1), 0);
    }
}
