use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dioxus::html::geometry::WheelDelta;
use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

use crate::audio::{Engine, SAMPLE_RATE};
use crate::save::{next_unused_filename, with_wav_extension, write_wav, FILE_PREFIX};
use crate::waveform::{channel_path, column_peaks};

const CSS: &str = include_str!("style.css");

/// Fewest frames the main view may show when fully zoomed in.
const MIN_VIEW_FRAMES: f64 = 64.0;
const ZOOM_STEP: f64 = 1.25;
/// Mouse travel below this (px) counts as a click rather than a region drag.
const CLICK_SLOP_PX: f64 = 3.0;
const WAVE_H: f32 = 400.0;
const MINI_H: f32 = 56.0;
/// Meter floor in dB.
const METER_FLOOR_DB: f32 = -60.0;
/// Target rate for polling the engine into the UI.
const UI_FPS: u64 = 90;
/// Meter release: fraction of a peak still shown after one second of silence
/// (≈ -50 dB), independent of `UI_FPS`.
const METER_RELEASE_PER_SEC: f32 = 0.0026;

/// Engine state snapshot polled into the UI.
#[derive(Clone, Copy, PartialEq, Default)]
struct Status {
    frames: usize,
    recording: bool,
    playing: bool,
    play_pos: usize,
}

/// Visible window of the main waveform, in frames. `None` = whole recording.
#[derive(Clone, Copy, PartialEq)]
struct View {
    start: f64,
    len: f64,
}

impl View {
    fn full(frames: usize) -> Self {
        View {
            start: 0.0,
            len: frames.max(1) as f64,
        }
    }
}

type Region = Option<(usize, usize)>;

fn normalized(a: usize, b: usize) -> Region {
    if a == b {
        None
    } else {
        Some((a.min(b), a.max(b)))
    }
}

fn fmt_time(frames: usize) -> String {
    let secs = frames as f64 / SAMPLE_RATE as f64;
    format!("{}:{:06.3}", (secs / 60.0) as u64, secs % 60.0)
}

#[component]
pub fn App() -> Element {
    let engine = use_context_provider(Engine::new);
    let mut status = use_signal(Status::default);
    let mut levels = use_signal(|| [0f32; 2]);
    let mut zoom = use_signal(|| Option::<View>::None);
    let mut region = use_signal(|| Region::None);
    let mut cursor = use_signal(|| 0usize);
    // Folder for Save; chosen via dialog on first use (or pre-seeded from the environment).
    let mut save_dir = use_signal(|| std::env::var_os("QUICKSAMPLE_SAVE_DIR").map(PathBuf::from));
    let mut message = use_signal(|| (String::new(), false));

    // Poll the audio engine at `UI_FPS` into signals; only writes on change so an
    // idle app doesn't re-render.
    use_future({
        let engine = engine.clone();
        move || {
            let engine = engine.clone();
            async move {
                let release = METER_RELEASE_PER_SEC.powf(1.0 / UI_FPS as f32);
                let mut ticks = tokio::time::interval(Duration::from_micros(1_000_000 / UI_FPS));
                ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticks.tick().await;
                    let s = Status {
                        frames: engine.frames(),
                        recording: engine.is_recording(),
                        playing: engine.is_playing(),
                        play_pos: engine.play_pos(),
                    };
                    if s != *status.peek() {
                        status.set(s);
                    }
                    let raw = if s.playing {
                        engine.output_levels.load()
                    } else {
                        engine.input_levels.load()
                    };
                    let prev = *levels.peek();
                    let smooth = |p: f32, n: f32| {
                        let v = n.max(p * release);
                        if v < 1e-4 {
                            0.0
                        } else {
                            v
                        }
                    };
                    let next = [smooth(prev[0], raw[0]), smooth(prev[1], raw[1])];
                    if next != prev {
                        levels.set(next);
                    }
                    if let Some(e) = engine.take_error() {
                        message.set((e, true));
                    }
                }
            }
        }
    });

    // Space / Escape are handled window-wide so focus never matters.
    use_future({
        let engine = engine.clone();
        move || {
            let engine = engine.clone();
            async move {
                let mut keys = document::eval(
                    r#"window.addEventListener('keydown', (e) => {
                        if (e.repeat) return;
                        if (e.code === 'Space' || e.code === 'Escape') {
                            e.preventDefault();
                            dioxus.send(e.code);
                        }
                    });"#,
                );
                while let Ok(code) = keys.recv::<String>().await {
                    match code.as_str() {
                        // Space toggles: stop if playing, otherwise play the
                        // selection (or cursor to end).
                        "Space" => {
                            if engine.is_playing() {
                                engine.stop_playback();
                                continue;
                            }
                            let frames = engine.frames();
                            if engine.is_recording() || frames == 0 {
                                continue;
                            }
                            let (a, b) = region.peek().unwrap_or((*cursor.peek(), frames));
                            engine.play(a, b);
                        }
                        // Escape stops playback first; when idle it clears the
                        // selection and cursor.
                        "Escape" => {
                            if engine.is_playing() {
                                engine.stop_playback();
                                continue;
                            }
                            region.set(None);
                            cursor.set(0);
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    let st = status();
    let idle = !st.recording && !st.playing;

    let toggle_record = {
        let engine = engine.clone();
        move |_| {
            if engine.is_recording() {
                engine.stop_recording();
            } else {
                engine.start_recording();
            }
        }
    };

    let clear = {
        let engine = engine.clone();
        move |_| {
            engine.clear();
            region.set(None);
            cursor.set(0);
            zoom.set(None);
            message.set((String::new(), false));
        }
    };

    // Shared by Save and Save As: exports the region if there is one, else everything.
    let save = {
        let engine = engine.clone();
        move |ask_name: bool| {
            let frames = engine.frames();
            if frames == 0 {
                return;
            }
            let dir = save_dir.peek().clone();
            let path = if ask_name {
                let default_name = dir
                    .as_deref()
                    .map(next_unused_filename)
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| format!("{FILE_PREFIX}-001.wav"));
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Save sample as")
                    .add_filter("WAV audio", &["wav"])
                    .set_file_name(default_name);
                if let Some(dir) = &dir {
                    dialog = dialog.set_directory(dir);
                }
                match dialog.save_file() {
                    Some(p) => with_wav_extension(p),
                    None => return,
                }
            } else {
                let dir = match dir {
                    Some(d) => d,
                    None => match rfd::FileDialog::new()
                        .set_title("Choose a folder for saved samples")
                        .pick_folder()
                    {
                        Some(d) => d,
                        None => return,
                    },
                };
                next_unused_filename(&dir)
            };
            let (a, b) = region.peek().unwrap_or((0, frames));
            let data = engine.slice(a, b);
            match write_wav(&path, &data) {
                Ok(()) => {
                    save_dir.set(path.parent().map(PathBuf::from));
                    message.set((format!("Saved {}", path.display()), false));
                }
                Err(e) => message.set((format!("Save failed: {e}"), true)),
            }
        }
    };
    let mut save_quick = save.clone();
    let mut save_as = save;

    let lv = levels();
    let (msg, msg_is_error) = message();
    let region_len = region().map(|(a, b)| b - a);

    rsx! {
        style { {CSS} }
        div { class: "app",
            div { class: "toolbar",
                button {
                    class: if st.recording { "record armed" } else { "record" },
                    title: if st.recording { "Stop recording" } else { "Start recording" },
                    onclick: toggle_record,
                    if st.recording {
                        svg { class: "icon", width: "14", height: "14", view_box: "0 0 14 14",
                            rect { x: "1", y: "1", width: "12", height: "12", rx: "2" }
                        }
                        "Stop"
                    } else {
                        svg { class: "icon", width: "14", height: "14", view_box: "0 0 14 14",
                            circle { cx: "7", cy: "7", r: "6" }
                        }
                        "Record"
                    }
                }
                button {
                    disabled: !idle,
                    title: "Clear the recording, selection and zoom",
                    onclick: clear,
                    "Clear"
                }
                div { class: "sep" }
                button {
                    disabled: st.frames == 0,
                    title: "Save the selection (or everything) to the chosen folder with an unused name",
                    onclick: move |_| save_quick(false),
                    "Save"
                }
                button {
                    disabled: st.frames == 0,
                    title: "Save the selection (or everything) to a file you pick",
                    onclick: move |_| save_as(true),
                    "Save As…"
                }
                div { class: "spacer" }
                Meter { levels: lv, output: st.playing }
                div { class: "sep" }
                div { class: "time",
                    title: "Recording length",
                    {fmt_time(st.frames)}
                }
            }
            Waveform { status: st, zoom, region, cursor }
            div { class: "statusbar",
                div { class: if msg_is_error { "msg error" } else { "msg" },
                    if msg.is_empty() {
                        if let Some(n) = region_len {
                            "Selection: {fmt_time(n)} — Save exports the selection"
                        } else if st.frames > 0 {
                            "No selection — Save exports the whole recording"
                        } else {
                            "Pick the input source in pavucontrol; press Record to begin."
                        }
                    } else {
                        "{msg}"
                    }
                }
                div { class: "hints",
                    "Click: play from here · Drag: select · Right-click: set selection end · Wheel: zoom · Space: play/stop · Esc: stop / clear selection"
                }
            }
        }
    }
}

#[component]
fn Meter(levels: [f32; 2], output: bool) -> Element {
    let bar = |level: f32| {
        let db = if level > 0.0 {
            20.0 * level.log10()
        } else {
            METER_FLOOR_DB
        };
        let db = db.clamp(METER_FLOOR_DB, 0.0);
        let pct = (db - METER_FLOOR_DB) / -METER_FLOOR_DB * 100.0;
        let label = if db <= METER_FLOOR_DB {
            "-inf".to_string()
        } else {
            format!("{db:.0} dB")
        };
        (100.0 - pct, label)
    };
    let (l_mask, l_db) = bar(levels[0]);
    let (r_mask, r_db) = bar(levels[1]);
    rsx! {
        div {
            class: if output { "meter output" } else { "meter" },
            title: if output { "Output level" } else { "Input level" },
            span { class: "ch", "L" }
            div { class: "track", div { class: "mask", style: "width: {l_mask:.1}%" } }
            span { class: "db", "{l_db}" }
            span { class: "ch", "R" }
            div { class: "track", div { class: "mask", style: "width: {r_mask:.1}%" } }
            span { class: "db", "{r_db}" }
        }
    }
}

#[component]
fn Waveform(
    status: Status,
    zoom: Signal<Option<View>>,
    region: Signal<Region>,
    cursor: Signal<usize>,
) -> Element {
    let engine = use_context::<Arc<Engine>>();
    let mut width = use_signal(|| 960.0f64);
    // Left-button drag in progress: (anchor frame, current frame).
    let mut drag = use_signal(|| Option::<(usize, usize)>::None);

    let frames = status.frames;
    let w = width().max(1.0);
    let full = View::full(frames);
    let view = zoom().unwrap_or(full);

    let px_to_frame = move |x: f64| -> usize {
        ((view.start + x / w * view.len).round().max(0.0) as usize).min(frames)
    };
    let frame_to_px = move |f: usize| -> f64 { (f as f64 - view.start) / view.len * w };

    // Apply a new view, snapping back to "full" when everything is visible.
    let mut set_view = move |start: f64, len: f64| {
        let len = len.clamp(MIN_VIEW_FRAMES.min(full.len), full.len);
        if len >= full.len {
            zoom.set(None);
        } else {
            zoom.set(Some(View {
                start: start.clamp(0.0, full.len - len),
                len,
            }));
        }
    };

    let finish_drag = {
        let engine = engine.clone();
        move |allow_click: bool| {
            let Some((a, c)) = drag.take() else { return };
            let moved = (frame_to_px(a) - frame_to_px(c)).abs() >= CLICK_SLOP_PX;
            if moved {
                region.set(normalized(a, c));
            } else if allow_click {
                cursor.set(a);
                if !status.recording && frames > 0 {
                    engine.play(a, frames);
                }
            }
        }
    };
    let mut finish_drag_move = finish_drag.clone();
    let mut finish_drag_up = finish_drag.clone();
    let mut finish_drag_leave = finish_drag;

    let shown_region = drag().and_then(|(a, c)| normalized(a, c)).or(region());

    rsx! {
        div { class: "wave-area",
            div {
                class: "wave",
                onresize: move |evt| {
                    if let Ok(size) = evt.get_content_box_size() {
                        width.set(size.width.max(1.0));
                    }
                },
                svg {
                    view_box: "0 0 {w:.0} {WAVE_H}",
                    preserve_aspect_ratio: "none",
                    line { class: "midline", x1: "0", y1: "{WAVE_H / 4.0}", x2: "{w:.0}", y2: "{WAVE_H / 4.0}" }
                    line { class: "midline", x1: "0", y1: "{WAVE_H * 3.0 / 4.0}", x2: "{w:.0}", y2: "{WAVE_H * 3.0 / 4.0}" }
                    WavePaths { zoom: zoom(), frames, width: w, height: WAVE_H, inset: 2.0 }
                }
                // Selection, cursor and playhead live in their own layer on top,
                // so moving them doesn't repaint the waveform underneath.
                svg {
                    class: "overlay",
                    view_box: "0 0 {w:.0} {WAVE_H}",
                    preserve_aspect_ratio: "none",
                    oncontextmenu: |evt| evt.prevent_default(),
                    onmousedown: move |evt| {
                        let f = px_to_frame(evt.element_coordinates().x);
                        match evt.trigger_button() {
                            Some(MouseButton::Primary) => drag.set(Some((f, f))),
                            Some(MouseButton::Secondary) => {
                                // Move the far edge of the selection; anchor at the
                                // cursor if there is no selection yet.
                                let start = region().map(|(a, _)| a).unwrap_or(cursor());
                                region.set(normalized(start, f));
                            }
                            _ => {}
                        }
                    },
                    onmousemove: move |evt| {
                        if let Some((a, _)) = drag() {
                            if evt.held_buttons().contains(MouseButton::Primary) {
                                let f = px_to_frame(evt.element_coordinates().x);
                                drag.set(Some((a, f)));
                            } else {
                                finish_drag_move(false);
                            }
                        }
                    },
                    onmouseup: move |evt| {
                        if evt.trigger_button() == Some(MouseButton::Primary) {
                            finish_drag_up(true);
                        }
                    },
                    onmouseleave: move |_| finish_drag_leave(false),
                    onwheel: move |evt| {
                        evt.prevent_default();
                        if frames == 0 {
                            return;
                        }
                        let dy = match evt.delta() {
                            WheelDelta::Pixels(v) => v.y,
                            WheelDelta::Lines(v) => v.y,
                            WheelDelta::Pages(v) => v.y,
                        };
                        if dy == 0.0 {
                            return;
                        }
                        let x = evt.element_coordinates().x;
                        let anchor = view.start + x / w * view.len;
                        let len = if dy < 0.0 { view.len / ZOOM_STEP } else { view.len * ZOOM_STEP };
                        set_view(anchor - x / w * len, len);
                    },

                    if let Some((a, b)) = shown_region {
                        rect {
                            class: "region",
                            x: "{frame_to_px(a):.1}", y: "0",
                            width: "{(frame_to_px(b) - frame_to_px(a)).max(1.0):.1}", height: "{WAVE_H}",
                        }
                        line { class: "region-edge", x1: "{frame_to_px(a):.1}", y1: "0", x2: "{frame_to_px(a):.1}", y2: "{WAVE_H}" }
                        line { class: "region-edge", x1: "{frame_to_px(b):.1}", y1: "0", x2: "{frame_to_px(b):.1}", y2: "{WAVE_H}" }
                    }
                    if frames > 0 {
                        line { class: "cursor", x1: "{frame_to_px(cursor()):.1}", y1: "0", x2: "{frame_to_px(cursor()):.1}", y2: "{WAVE_H}" }
                    }
                    if status.playing {
                        line { class: "playhead", x1: "{frame_to_px(status.play_pos):.1}", y1: "0", x2: "{frame_to_px(status.play_pos):.1}", y2: "{WAVE_H}" }
                    }
                }
                if frames == 0 && !status.recording {
                    div { class: "empty-hint", "Nothing recorded yet" }
                }
            }
            if zoom().is_some() {
                Minimap { status, view, width: w, on_pan: move |center: f64| set_view(center - view.len / 2.0, view.len) }
            }
        }
    }
}

/// Both channels' shapes for `zoom` (or the whole recording), with `inset`
/// px of headroom per channel. A separate component so it only re-renders
/// when the view, size or recording length changes, not on every playhead,
/// cursor or drag update.
#[component]
fn WavePaths(zoom: Option<View>, frames: usize, width: f64, height: f32, inset: f32) -> Element {
    let engine = use_context::<Arc<Engine>>();
    let view = zoom.unwrap_or(View::full(frames));
    let (l_path, r_path) = {
        let rec = engine.take.lock().unwrap();
        let peaks = column_peaks(&rec, view.start, view.len, width as usize);
        let q = height / 4.0;
        (
            channel_path(&peaks, 0, q, q - inset),
            channel_path(&peaks, 2, 3.0 * q, q - inset),
        )
    };
    rsx! {
        path { class: "wave-l", d: "{l_path}" }
        path { class: "wave-r", d: "{r_path}" }
    }
}

/// Small full-length waveform with a box marking the visible region; click or drag to pan.
#[component]
fn Minimap(status: Status, view: View, width: f64, on_pan: EventHandler<f64>) -> Element {
    let frames = status.frames.max(1) as f64;
    let w = width;
    let x0 = view.start / frames * w;
    let x1 = (view.start + view.len) / frames * w;
    let pan = move |evt: Event<MouseData>| {
        if evt.held_buttons().contains(MouseButton::Primary) {
            on_pan.call(evt.element_coordinates().x / w * frames);
        }
    };
    rsx! {
        div { class: "minimap",
            svg {
                view_box: "0 0 {w:.0} {MINI_H}",
                preserve_aspect_ratio: "none",
                onmousedown: pan,
                onmousemove: pan,
                WavePaths { zoom: None, frames: status.frames, width: w, height: MINI_H, inset: 1.0 }
                rect { class: "viewport", x: "{x0:.1}", y: "0.5", width: "{(x1 - x0).max(2.0):.1}", height: "{MINI_H - 1.0}" }
                if status.playing {
                    line { class: "playhead", x1: "{status.play_pos as f64 / frames * w:.1}", y1: "0", x2: "{status.play_pos as f64 / frames * w:.1}", y2: "{MINI_H}" }
                }
            }
        }
    }
}
