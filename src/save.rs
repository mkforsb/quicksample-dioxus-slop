//! WAV export and output filename selection.

use std::path::{Path, PathBuf};

use crate::audio::{CHANNELS, SAMPLE_RATE};

pub const FILE_PREFIX: &str = "sample";

/// Write interleaved stereo f32 samples as a 16-bit PCM WAV.
pub fn write_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: CHANNELS as u16,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    {
        let mut w = writer.get_i16_writer(samples.len() as u32);
        for &s in samples {
            w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16);
        }
        w.flush().map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())
}

/// First `sample-NNN.wav` in `dir` that does not exist yet.
pub fn next_unused_filename(dir: &Path) -> PathBuf {
    (1..)
        .map(|n| dir.join(format!("{FILE_PREFIX}-{n:03}.wav")))
        .find(|p| !p.exists())
        .expect("unbounded iterator")
}

/// Ensure the chosen path ends in `.wav` (dialogs don't always add it).
pub fn with_wav_extension(path: PathBuf) -> PathBuf {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("wav") => path,
        _ => path.with_extension("wav"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_first_gap() {
        let dir = std::env::temp_dir().join(format!("quicksample-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(next_unused_filename(&dir), dir.join("sample-001.wav"));
        std::fs::write(dir.join("sample-001.wav"), b"").unwrap();
        std::fs::write(dir.join("sample-002.wav"), b"").unwrap();
        assert_eq!(next_unused_filename(&dir), dir.join("sample-003.wav"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wav_extension() {
        assert_eq!(with_wav_extension("a/b".into()), PathBuf::from("a/b.wav"));
        assert_eq!(
            with_wav_extension("a/b.WAV".into()),
            PathBuf::from("a/b.WAV")
        );
        assert_eq!(
            with_wav_extension("a/b.txt".into()),
            PathBuf::from("a/b.wav")
        );
    }

    #[test]
    fn roundtrip_wav() {
        let path = std::env::temp_dir().join(format!("quicksample-{}.wav", std::process::id()));
        write_wav(&path, &[0.0, 0.5, -0.5, 1.0]).unwrap();
        let mut r = hound::WavReader::open(&path).unwrap();
        assert_eq!(r.spec().channels, 2);
        assert_eq!(r.spec().sample_rate, SAMPLE_RATE);
        let v: Vec<i16> = r.samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(v, vec![0, 16384, -16384, i16::MAX]);
        std::fs::remove_file(&path).unwrap();
    }
}
