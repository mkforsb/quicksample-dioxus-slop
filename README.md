# QuickSample

A small desktop sampler built with [Dioxus](https://dioxuslabs.com) and PulseAudio.
Record from whatever input is selected in `pavucontrol`, trim a region in the
waveform, and save it as a 48 kHz stereo 16-bit WAV.

## Build & run

Requires Rust, `libpulse-dev`, `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`,
`libxdo-dev` and a running PulseAudio (or PipeWire-Pulse). File dialogs use the
XDG desktop portal (falls back to `zenity`).

```sh
cargo run --release
```

## Controls

| Action | Effect |
|---|---|
| **Record / Stop** | Start or stop capturing. New takes append to the existing recording. |
| **Clear** | Drop the recording, selection, cursor and zoom (idle only). |
| **Save** | Asks for a folder on first use, then writes `sample-NNN.wav` with the next unused number. |
| **Save As…** | Always shows a file dialog, remembering the last folder. |
| Left click on waveform | Set the cursor and play from there to the end. |
| Left drag | Select a region. |
| Right click | Move the far end of the selection (anchored at the cursor if there is none). |
| Mouse wheel | Zoom in/out around the pointer; a minimap appears when zoomed, click/drag it to pan. |
| **Space** | Stop playback if playing; otherwise play the selection, or from the cursor to the end if there is none. |
| **Escape** | Stop playback if playing; otherwise clear the selection and reset the cursor to the start. |

Save and Save As export the selection if there is one, otherwise the whole recording.

Set `QUICKSAMPLE_SAVE_DIR=/some/folder` to skip the first-use folder dialog.
