//! Incremental multi-resolution peak cache.
//!
//! Level `i` stores one min/max summary per `BASE^(i+1)` frames, built as
//! audio is appended. Any frame range can then be summarised from a handful
//! of cached blocks plus at most `BASE - 1` raw frames at each edge, so a
//! waveform render costs O(columns) regardless of recording length.

use crate::audio::CHANNELS;

/// Min/max per channel: `[l_min, l_max, r_min, r_max]`.
pub type Column = [f32; 4];

pub const EMPTY: Column = [f32::MAX, f32::MIN, f32::MAX, f32::MIN];

const BASE: usize = 16;
const LEVELS: usize = 4; // 16, 256, 4096, 65536 frames per block

#[inline]
fn merge(col: &mut Column, other: &Column) {
    col[0] = col[0].min(other[0]);
    col[1] = col[1].max(other[1]);
    col[2] = col[2].min(other[2]);
    col[3] = col[3].max(other[3]);
}

#[inline]
fn merge_frame(col: &mut Column, frame: &[f32]) {
    col[0] = col[0].min(frame[0]);
    col[1] = col[1].max(frame[0]);
    col[2] = col[2].min(frame[1]);
    col[3] = col[3].max(frame[1]);
}

const fn block_size(level: usize) -> usize {
    BASE.pow(level as u32 + 1)
}

#[derive(Default)]
pub struct PeakPyramid {
    levels: [Vec<Column>; LEVELS],
    frames: usize,
}

impl PeakPyramid {
    pub fn clear(&mut self) {
        self.levels.iter_mut().for_each(Vec::clear);
        self.frames = 0;
    }

    /// Append interleaved frames; must mirror the sample buffer exactly.
    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.as_chunks::<CHANNELS>().0 {
            let block = self.frames / BASE;
            if block == self.levels[0].len() {
                self.levels[0].push(EMPTY);
            }
            merge_frame(&mut self.levels[0][block], frame);
            self.frames += 1;
        }
        // Roll completed groups of BASE blocks up into the next level. Level 0's
        // last block may still be partial, so only complete ones are summarised.
        let mut complete = self.frames / BASE;
        for level in 1..LEVELS {
            let (lower, upper) = self.levels.split_at_mut(level);
            let lower = &lower[level - 1];
            let upper = &mut upper[0];
            while (upper.len() + 1) * BASE <= complete {
                let mut col = EMPTY;
                for c in &lower[upper.len() * BASE..(upper.len() + 1) * BASE] {
                    merge(&mut col, c);
                }
                upper.push(col);
            }
            complete = upper.len();
        }
    }

    /// Min/max over frames `[a, b)`, using the coarsest cached blocks that fit
    /// and `samples` (the interleaved buffer) only for sub-block edges.
    pub fn query(&self, samples: &[f32], a: usize, b: usize) -> Column {
        let mut col = EMPTY;
        let mut pos = a;
        'walk: while pos < b {
            for level in (0..LEVELS).rev() {
                let bs = block_size(level);
                if pos.is_multiple_of(bs) && pos + bs <= b && pos / bs < self.levels[level].len() {
                    merge(&mut col, &self.levels[level][pos / bs]);
                    pos += bs;
                    continue 'walk;
                }
            }
            merge_frame(
                &mut col,
                &samples[pos * CHANNELS..pos * CHANNELS + CHANNELS],
            );
            pos += 1;
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute(samples: &[f32], a: usize, b: usize) -> Column {
        let mut col = EMPTY;
        for f in samples[a * CHANNELS..b * CHANNELS]
            .as_chunks::<CHANNELS>()
            .0
        {
            merge_frame(&mut col, f);
        }
        col
    }

    fn noise(n: usize) -> Vec<f32> {
        // Deterministic pseudo-random samples in [-1, 1].
        let mut x: u32 = 0x9e37_79b9;
        (0..n * CHANNELS)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn matches_brute_force_at_every_level() {
        let samples = noise(70_001);
        let mut p = PeakPyramid::default();
        // Push in odd-sized chunks so blocks straddle pushes.
        for chunk in samples.chunks(7 * 2 * 53) {
            p.push(chunk);
        }
        assert_eq!(p.levels[0].len(), 70_001usize.div_ceil(16));
        assert_eq!(p.levels[1].len(), 70_001 / 256);
        assert_eq!(p.levels[2].len(), 70_001 / 4096);
        assert_eq!(p.levels[3].len(), 1);
        for &(a, b) in &[
            (0, 70_001),
            (0, 1),
            (5, 21),
            (16, 32),
            (4095, 65_537),
            (65_536, 70_001),
            (1234, 1234),
        ] {
            assert_eq!(
                p.query(&samples, a, b),
                brute(&samples, a, b),
                "range {a}..{b}"
            );
        }
    }

    #[test]
    fn clear_resets() {
        let samples = noise(1000);
        let mut p = PeakPyramid::default();
        p.push(&samples);
        p.clear();
        assert_eq!(p.frames, 0);
        p.push(&samples[..2 * 10]);
        assert_eq!(p.query(&samples, 0, 10), brute(&samples, 0, 10));
    }
}
