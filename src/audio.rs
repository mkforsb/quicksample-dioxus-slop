//! PulseAudio engine: one always-on capture thread (input meter + recording)
//! and a playback thread spawned per play request.
//!
//! No device names are passed to PulseAudio, so the streams follow the default
//! source/sink and can be re-routed from pavucontrol.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

use crate::peaks::PeakPyramid;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
const APP_NAME: &str = "QuickSample";
/// Capture/playback chunk size in frames (10 ms at 48 kHz).
const CHUNK_FRAMES: usize = 480;

fn spec() -> Spec {
    let spec = Spec {
        format: Format::F32le,
        channels: CHANNELS as u8,
        rate: SAMPLE_RATE,
    };
    assert!(spec.is_valid());
    spec
}

fn frames_to_bytes(frames: usize) -> u32 {
    (frames * CHANNELS * std::mem::size_of::<f32>()) as u32
}

/// Peak level per channel, stored as f32 bits so it can be shared lock-free.
#[derive(Default)]
pub struct Levels([AtomicU32; CHANNELS]);

impl Levels {
    fn store(&self, l: f32, r: f32) {
        self.0[0].store(l.to_bits(), Ordering::Relaxed);
        self.0[1].store(r.to_bits(), Ordering::Relaxed);
    }

    pub fn load(&self) -> [f32; CHANNELS] {
        [
            f32::from_bits(self.0[0].load(Ordering::Relaxed)),
            f32::from_bits(self.0[1].load(Ordering::Relaxed)),
        ]
    }
}

fn peaks(interleaved: &[f32]) -> (f32, f32) {
    let mut l = 0f32;
    let mut r = 0f32;
    for frame in interleaved.as_chunks::<CHANNELS>().0 {
        l = l.max(frame[0].abs());
        r = r.max(frame[1].abs());
    }
    (l, r)
}

/// The recording plus its peak cache; the two are always appended together.
#[derive(Default)]
pub struct Recording {
    /// Interleaved stereo f32 samples.
    pub samples: Vec<f32>,
    pub peaks: PeakPyramid,
}

impl Recording {
    pub fn frames(&self) -> usize {
        self.samples.len() / CHANNELS
    }
}

pub struct Engine {
    pub take: Mutex<Recording>,
    /// Number of recorded frames; mirrors `samples.len() / CHANNELS` for cheap polling.
    frames: AtomicUsize,
    recording: AtomicBool,
    playing: AtomicBool,
    /// Frame index (relative to the recording) currently being played.
    play_pos: AtomicUsize,
    /// Bumped on every play/stop so stale playback threads exit.
    play_generation: AtomicU64,
    pub input_levels: Levels,
    pub output_levels: Levels,
    /// Last error from the audio threads, shown in the UI.
    pub error: Mutex<Option<String>>,
}

impl Engine {
    pub fn new() -> Arc<Self> {
        let engine = Arc::new(Engine {
            take: Mutex::new(Recording::default()),
            frames: AtomicUsize::new(0),
            recording: AtomicBool::new(false),
            playing: AtomicBool::new(false),
            play_pos: AtomicUsize::new(0),
            play_generation: AtomicU64::new(0),
            input_levels: Levels::default(),
            output_levels: Levels::default(),
            error: Mutex::new(None),
        });
        let capture = Arc::clone(&engine);
        thread::Builder::new()
            .name("pulse-capture".into())
            .spawn(move || capture.capture_loop())
            .expect("spawn capture thread");
        engine
    }

    pub fn frames(&self) -> usize {
        self.frames.load(Ordering::Acquire)
    }

    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Acquire)
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Acquire)
    }

    pub fn play_pos(&self) -> usize {
        self.play_pos.load(Ordering::Relaxed)
    }

    pub fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap().take()
    }

    fn set_error(&self, msg: String) {
        *self.error.lock().unwrap() = Some(msg);
    }

    pub fn start_recording(&self) {
        self.stop_playback();
        self.recording.store(true, Ordering::Release);
    }

    pub fn stop_recording(&self) {
        self.recording.store(false, Ordering::Release);
    }

    pub fn clear(&self) {
        self.stop_playback();
        self.stop_recording();
        let mut rec = self.take.lock().unwrap();
        rec.samples.clear();
        rec.peaks.clear();
        self.frames.store(0, Ordering::Release);
    }

    /// Copy of the recording as interleaved samples for `[start, end)` frames.
    pub fn slice(&self, start: usize, end: usize) -> Vec<f32> {
        let rec = self.take.lock().unwrap();
        let end = end.min(rec.frames());
        let start = start.min(end);
        rec.samples[start * CHANNELS..end * CHANNELS].to_vec()
    }

    pub fn stop_playback(&self) {
        self.play_generation.fetch_add(1, Ordering::AcqRel);
        self.playing.store(false, Ordering::Release);
        self.output_levels.store(0.0, 0.0);
    }

    /// Start (or restart) playback of frames `[start, end)`.
    pub fn play(self: &Arc<Self>, start: usize, end: usize) {
        let data = self.slice(start, end);
        if data.is_empty() {
            return;
        }
        let generation = self.play_generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.play_pos.store(start, Ordering::Relaxed);
        self.playing.store(true, Ordering::Release);
        let engine = Arc::clone(self);
        thread::Builder::new()
            .name("pulse-playback".into())
            .spawn(move || engine.playback_loop(data, start, generation))
            .expect("spawn playback thread");
    }

    fn capture_loop(&self) {
        let spec = spec();
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: frames_to_bytes(CHUNK_FRAMES),
        };
        let mut buf = vec![0f32; CHUNK_FRAMES * CHANNELS];
        loop {
            let stream = match Simple::new(
                None,
                APP_NAME,
                Direction::Record,
                None,
                "Input",
                &spec,
                None,
                Some(&attr),
            ) {
                Ok(s) => s,
                Err(e) => {
                    self.set_error(format!("PulseAudio capture unavailable: {e}"));
                    self.input_levels.store(0.0, 0.0);
                    thread::sleep(Duration::from_secs(2));
                    continue;
                }
            };
            loop {
                let bytes = as_bytes_mut(&mut buf);
                if let Err(e) = stream.read(bytes) {
                    self.set_error(format!("PulseAudio read failed: {e}"));
                    break;
                }
                let (l, r) = peaks(&buf);
                self.input_levels.store(l, r);
                if self.recording.load(Ordering::Acquire) {
                    let mut rec = self.take.lock().unwrap();
                    rec.samples.extend_from_slice(&buf);
                    rec.peaks.push(&buf);
                    self.frames.store(rec.frames(), Ordering::Release);
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    }

    fn playback_loop(&self, data: Vec<f32>, start: usize, generation: u64) {
        let spec = spec();
        let attr = BufferAttr {
            maxlength: u32::MAX,
            // Keep the sink buffer short so the playhead tracks what is heard.
            tlength: frames_to_bytes(CHUNK_FRAMES * 4),
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: u32::MAX,
        };
        let stream = match Simple::new(
            None,
            APP_NAME,
            Direction::Playback,
            None,
            "Playback",
            &spec,
            None,
            Some(&attr),
        ) {
            Ok(s) => s,
            Err(e) => {
                self.set_error(format!("PulseAudio playback unavailable: {e}"));
                self.finish_playback(generation);
                return;
            }
        };
        let is_current = || self.play_generation.load(Ordering::Acquire) == generation;
        let mut frame = 0;
        for chunk in data.chunks(CHUNK_FRAMES * CHANNELS) {
            if !is_current() {
                let _ = stream.flush();
                return;
            }
            let (l, r) = peaks(chunk);
            self.output_levels.store(l, r);
            self.play_pos.store(start + frame, Ordering::Relaxed);
            if let Err(e) = stream.write(as_bytes(chunk)) {
                self.set_error(format!("PulseAudio write failed: {e}"));
                break;
            }
            frame += chunk.len() / CHANNELS;
        }
        if is_current() {
            let _ = stream.drain();
        }
        self.finish_playback(generation);
    }

    fn finish_playback(&self, generation: u64) {
        // Only the most recent playback may flip the shared state back to idle.
        if self.play_generation.load(Ordering::Acquire) == generation {
            self.playing.store(false, Ordering::Release);
            self.output_levels.store(0.0, 0.0);
        }
    }
}

fn as_bytes(samples: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding or invalid bit patterns; the byte view covers exactly the slice.
    unsafe { std::slice::from_raw_parts(samples.as_ptr() as *const u8, samples.len() * 4) }
}

fn as_bytes_mut(samples: &mut [f32]) -> &mut [u8] {
    // SAFETY: every byte pattern is a valid f32, and the view covers exactly the slice.
    unsafe { std::slice::from_raw_parts_mut(samples.as_mut_ptr() as *mut u8, samples.len() * 4) }
}
