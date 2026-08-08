# Screen Annotation Prototype

This Windows prototype combines speech transcription with a captured screen that
can be annotated while recording. It is intended for local evaluation before a
community discussion and production pull request.

## Interaction model

Ordinary dictation remains unchanged:

- **Transcribe** (`Ctrl+Space` by default) records and pastes text only.
- **Transcribe + Annotate** (`Ctrl+Alt+Space` by default) captures the monitor
  containing the pointer, opens a full-screen drawing surface, and records speech.

Both bindings are configurable in **Settings > General**. A separate binding is
used instead of a global checkbox so screenshot capture is selected per
invocation. Users who prefer a modifier convention can configure the annotation
binding as their normal dictation chord plus an additional modifier.

While annotating:

- Draw directly on the frozen screen image.
- Choose an ink color and stroke width.
- Clear all ink without discarding the screen capture.
- Read the live streaming transcript below the drawing controls.
- Press the green check or invoke the annotation shortcut again to finish.

Finishing hides the annotation window immediately, restores the previously active
application, then pastes the transcription followed by the annotated PNG. The
image is still pasted when the recording contains no usable speech.

## Implementation

- `src-tauri/src/screen_annotation.rs` captures the active monitor with Win32 GDI,
  owns the reusable Tauri annotation window, coordinates lifecycle state, and
  restores the target window.
- `src/annotation/` renders the full-screen canvas and live transcript controls.
- `src-tauri/src/actions.rs` starts annotation only for the dedicated
  `transcribe_with_annotation` action.
- `src-tauri/src/clipboard.rs` performs the two-stage text and image paste while
  preserving the previous clipboard content.

The screenshot is written to Handy's application cache only long enough for the
webview to load it. The cached file is deleted after paste or cancellation. Speech
and image processing remain local.

The annotation webview is created once and reused. Its lifecycle distinguishes
recording, finalizing, and finalized states so a delayed image-load callback
cannot re-show the overlay after recording stops.

## Development

The prototype is available on Windows. Set `HANDY_SCREEN_ANNOTATION=0` before
launching to disable the annotation window and binding behavior.

Run the normal development command:

```powershell
bun run tauri dev
```

On a machine without the Vulkan SDK, a CPU-only local run can be started with:

```powershell
$env:CMAKE_ARGS = "-DTRANSCRIBE_VULKAN=OFF"
$env:CARGO_PROFILE_DEV_DEBUG = "0"
bun run tauri dev
```

Relevant checks:

```powershell
bun run build
bun run lint
cargo test --manifest-path src-tauri\Cargo.toml --lib
```

## Prototype constraints

- Windows only.
- Captures one monitor: the monitor containing the pointer when recording starts.
- The destination application must support pasting images.
- Text and image are pasted as two clipboard operations because many applications
  do not consume mixed clipboard formats consistently.
- A production proposal should gather community feedback and decide whether the
  annotation binding is visible by default or placed behind an experimental
  master toggle. The per-invocation binding should remain the primary control.
