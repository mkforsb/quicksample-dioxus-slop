//! Per-pixel-column peaks of the recording and SVG path generation for the
//! waveform view.

use std::fmt::Write;

use crate::audio::Recording;
pub use crate::peaks::Column;

/// Peaks for `columns` columns spanning frames `[start, start + len)`.
/// Columns that fall past the end of the recording are flat (all zeros).
pub fn column_peaks(rec: &Recording, start: f64, len: f64, columns: usize) -> Vec<Column> {
    let total = rec.frames();
    let mut out = Vec::with_capacity(columns);
    if columns == 0 || len <= 0.0 {
        return out;
    }
    let per_col = len / columns as f64;
    for c in 0..columns {
        let a = (start + c as f64 * per_col).floor().max(0.0) as usize;
        let b = ((start + (c + 1) as f64 * per_col).ceil().max(0.0) as usize).max(a + 1);
        let (a, b) = (a.min(total), b.min(total));
        if a >= b {
            out.push([0.0; 4]);
            continue;
        }
        out.push(rec.peaks.query(&rec.samples, a, b));
    }
    out
}

/// SVG path outlining the channel at `offset` (0 = left, 2 = right) as one
/// filled shape, centered at `cy` with half-height `amp`. Column `x` covers
/// pixels `[x, x + 1)` from its max to its min, like a 1px bar per column, but a
/// single outline rasterizes far faster than thousands of stroked segments.
pub fn channel_path(cols: &[Column], offset: usize, cy: f32, amp: f32) -> String {
    let mut d = String::with_capacity(cols.len() * 24);
    if cols.is_empty() {
        return d;
    }
    let spans: Vec<(f32, f32)> = cols
        .iter()
        .map(|col| {
            let (lo, hi) = (
                col[offset].clamp(-1.0, 1.0),
                col[offset + 1].clamp(-1.0, 1.0),
            );
            // Always at least one pixel tall so silence still shows a center line.
            let y_top = cy - hi * amp;
            (y_top, (cy - lo * amp).max(y_top + 1.0))
        })
        .collect();
    // Top edge left to right, then bottom edge back.
    let _ = write!(d, "M0 {:.1}", spans[0].0);
    for (x, (top, _)) in spans.iter().enumerate() {
        if x > 0 {
            let _ = write!(d, "V{top:.1}");
        }
        let _ = write!(d, "H{}", x + 1);
    }
    for (x, (_, bot)) in spans.iter().enumerate().rev() {
        let _ = write!(d, "V{bot:.1}H{x}");
    }
    d.push('Z');
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(samples: &[f32]) -> Recording {
        let mut r = Recording::default();
        r.samples.extend_from_slice(samples);
        r.peaks.push(samples);
        r
    }

    #[test]
    fn peaks_cover_each_column() {
        // 4 frames: L = 0.1, -0.5, 0.9, 0.0 ; R = 0.2, 0.3, -0.7, 0.4
        let s = [0.1, 0.2, -0.5, 0.3, 0.9, -0.7, 0.0, 0.4];
        let cols = column_peaks(&rec(&s), 0.0, 4.0, 2);
        assert_eq!(cols, vec![[-0.5, 0.1, 0.2, 0.3], [0.0, 0.9, -0.7, 0.4]]);
    }

    #[test]
    fn columns_past_end_are_flat() {
        let s = [0.5, 0.5];
        let cols = column_peaks(&rec(&s), 0.0, 4.0, 4);
        assert_eq!(cols[0], [0.5, 0.5, 0.5, 0.5]);
        assert_eq!(cols[3], [0.0; 4]);
    }

    #[test]
    fn empty_input_is_flat() {
        let cols = column_peaks(&rec(&[]), 0.0, 100.0, 3);
        assert_eq!(cols, vec![[0.0; 4]; 3]);
    }

    #[test]
    fn path_outlines_each_column() {
        let cols = vec![[0.0; 4], [-1.0, 1.0, 0.0, 0.0]];
        let d = channel_path(&cols, 0, 10.0, 10.0);
        assert_eq!(d, "M0 10.0H1V0.0H2V20.0H1V11.0H0Z");
        assert_eq!(channel_path(&[], 0, 10.0, 10.0), "");
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    /// `cargo test --release bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn render_cost_vs_length() {
        for &minutes in &[0.2f64, 1.0, 10.0, 60.0] {
            let frames = (minutes * 60.0 * crate::audio::SAMPLE_RATE as f64) as usize;
            let mut r = Recording::default();
            let chunk: Vec<f32> = (0..480 * 2).map(|i| ((i * 7919) % 200) as f32 / 100.0 - 1.0).collect();
            let t = Instant::now();
            while r.frames() < frames {
                r.samples.extend_from_slice(&chunk);
                r.peaks.push(&chunk);
            }
            let build = t.elapsed();
            let t = Instant::now();
            let n = 200;
            for _ in 0..n {
                std::hint::black_box(column_peaks(&r, 0.0, r.frames() as f64, 1000));
            }
            let per = t.elapsed() / n;
            println!("{minutes:>5} min: build {build:?}, full-view render {per:?}");
        }
    }
}
