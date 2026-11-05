use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const WINDOW_LABEL: &str = "screen_annotation";
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(2);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, serde::Serialize, specta::Type)]
pub struct ScreenAnnotationSnapshot {
    session_id: u64,
    path: String,
    width: u32,
    height: u32,
    point_count: usize,
}

pub fn remove_audio_segments(wav_path: &Path) {
    if let Err(error) = std::fs::remove_file(audio_segments_path(wav_path)) {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::error!("Failed to remove annotation audio boundaries: {error}");
        }
    }
}

#[derive(Clone, Copy)]
struct ScreenBounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

struct AnnotationSession {
    snapshot: ScreenAnnotationSnapshot,
    snapshot_path: PathBuf,
    png: Vec<u8>,
    bounds: ScreenBounds,
    binding_id: String,
    target_window: isize,
    phase: AnnotationPhase,
    point_starts: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnnotationPhase {
    Recording,
    Finalizing,
    Finalized,
    Failed,
}

impl AnnotationPhase {
    fn can_show(self) -> bool {
        self == Self::Recording
    }

    fn should_wait(self) -> bool {
        self == Self::Finalizing
    }

    fn can_submit(self) -> bool {
        self == Self::Finalizing
    }
}

#[derive(Default)]
pub struct ScreenAnnotationState {
    session: Mutex<Option<AnnotationSession>>,
    finalized: Condvar,
}

pub fn initialize(app: &AppHandle) {
    app.manage(ScreenAnnotationState::default());
}

pub fn create_window(app: &AppHandle) {
    if !enabled() || app.get_webview_window(WINDOW_LABEL).is_some() {
        return;
    }

    #[cfg(target_os = "windows")]
    {
        let mut builder = tauri::WebviewWindowBuilder::new(
            app,
            WINDOW_LABEL,
            tauri::WebviewUrl::App("src/annotation/index.html".into()),
        )
        .title("Screen annotation")
        .decorations(false)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .closable(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focusable(true)
        .focused(false)
        .visible(false);

        if let Some(data_dir) = crate::portable::data_dir() {
            builder = builder.data_directory(data_dir.join("webview"));
        }

        if let Err(error) = builder.build() {
            log::error!("Failed to create screen annotation window: {error}");
        }
    }
}

pub fn enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        match std::env::var("HANDY_SCREEN_ANNOTATION") {
            Ok(value) => !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            ),
            Err(_) => true,
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

pub fn begin(app: &AppHandle, binding_id: &str) -> bool {
    if !enabled() {
        return false;
    }

    #[cfg(target_os = "windows")]
    {
        match begin_windows(app, binding_id) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("Screen annotation unavailable: {error}");
                false
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        false
    }
}

pub fn finish(app: &AppHandle) {
    if !enabled() {
        return;
    }

    let state = app.state::<ScreenAnnotationState>();
    {
        let Ok(mut session) = state.session.lock() else {
            log::error!("Failed to lock screen annotation state");
            return;
        };
        let Some(session) = session.as_mut() else {
            return;
        };
        session.phase = AnnotationPhase::Finalizing;
    }

    let Some(window) = app.get_webview_window(WINDOW_LABEL) else {
        mark_finalization_failed(&state);
        return;
    };

    if let Err(error) = window.emit("finish-screen-annotation", ()) {
        log::warn!("Failed to finalize screen annotation: {error}");
        mark_finalization_failed(&state);
        hide_and_restore(app);
    } else {
        // Dismiss immediately; PNG encoding and transcription can finish while
        // the hidden webview remains alive.
        hide_and_restore(app);
    }
}

pub fn cancel(app: &AppHandle) {
    let state = app.state::<ScreenAnnotationState>();
    let session = state
        .session
        .lock()
        .ok()
        .and_then(|mut session| session.take());
    state.finalized.notify_all();

    if let Some(session) = session {
        let _ = std::fs::remove_file(session.snapshot_path);
        restore_target_window(session.target_window);
    }

    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.hide();
    }
}

fn is_recording_session(app: &AppHandle, session_id: u64) -> bool {
    app.try_state::<ScreenAnnotationState>()
        .and_then(|state| {
            state.session.lock().ok().map(|session| {
                session.as_ref().is_some_and(|session| {
                    session.snapshot.session_id == session_id && session.phase.can_show()
                })
            })
        })
        .unwrap_or(false)
}

pub fn take_annotated_image(app: &AppHandle) -> Result<Option<Vec<u8>>, String> {
    let state = app.state::<ScreenAnnotationState>();
    let mut guard = state.session.lock().map_err(|error| error.to_string())?;

    if guard
        .as_ref()
        .is_some_and(|session| session.phase.should_wait())
    {
        let (new_guard, wait_result) = state
            .finalized
            .wait_timeout_while(guard, FINALIZE_TIMEOUT, |session| {
                session
                    .as_ref()
                    .is_some_and(|session| session.phase.should_wait())
            })
            .map_err(|error| error.to_string())?;
        guard = new_guard;
        if wait_result.timed_out() {
            return Err("Timed out waiting for the numbered screenshot".to_string());
        }
    }

    let Some(session) = guard.as_ref() else {
        return Ok(None);
    };
    if session.phase != AnnotationPhase::Finalized {
        return Err("The numbered screenshot is not ready".to_string());
    }
    let session = guard.take().ok_or("The annotation session ended")?;
    drop(guard);

    let _ = std::fs::remove_file(session.snapshot_path);
    restore_target_window(session.target_window);
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.hide();
    }
    Ok(Some(session.png))
}

pub fn audio_segment_starts(app: &AppHandle) -> Result<Option<Vec<usize>>, String> {
    let state = app.state::<ScreenAnnotationState>();
    let session = state.session.lock().map_err(|error| error.to_string())?;
    Ok(session.as_ref().map(|session| session.point_starts.clone()))
}

pub fn request_next_point(app: &AppHandle) {
    let result = advance_point(app, None).and_then(|snapshot| {
        app.get_webview_window(WINDOW_LABEL)
            .ok_or_else(|| "The annotation window is unavailable".to_string())
            .and_then(|window| {
                window
                    .emit("annotation-point-changed", snapshot)
                    .map_err(|error| error.to_string())
            })
    });
    if let Err(error) = result {
        log::error!("Failed to advance the annotation point: {error}");
        let _ = app.emit("transcription-error", error);
    }
}

#[tauri::command]
#[specta::specta]
pub async fn advance_screen_annotation(
    app: AppHandle,
    session_id: u64,
    point_number: usize,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        advance_point(&app, Some((session_id, point_number))).map(|snapshot| snapshot.point_count)
    })
    .await
    .map_err(|error| format!("Failed to start the next annotation point: {error}"))?
}

fn advance_point(
    app: &AppHandle,
    expected: Option<(u64, usize)>,
) -> Result<ScreenAnnotationSnapshot, String> {
    let state = app.state::<ScreenAnnotationState>();
    let mut session = state.session.lock().map_err(|error| error.to_string())?;
    let session = session.as_mut().ok_or("No screen annotation is active")?;
    let (session_id, point_number) =
        expected.unwrap_or((session.snapshot.session_id, session.point_starts.len()));
    validate_point_transition(session, session_id, point_number)?;
    // Serialize the boundary with finish/cancel and other point requests.
    let position = app
        .state::<std::sync::Arc<crate::managers::audio::AudioRecordingManager>>()
        .recording_position(&session.binding_id)?;
    session.point_starts.push(position);
    session.snapshot.point_count = session.point_starts.len();
    Ok(session.snapshot.clone())
}

fn validate_point_transition(
    session: &AnnotationSession,
    session_id: u64,
    point_number: usize,
) -> Result<(), String> {
    if session.snapshot.session_id != session_id
        || session.phase != AnnotationPhase::Recording
        || session.point_starts.len() != point_number
    {
        return Err("The annotation point is no longer current".to_string());
    }
    Ok(())
}

pub fn audio_segments_path(wav_path: &Path) -> PathBuf {
    wav_path.with_extension("annotation.json")
}

pub fn save_audio_segments(wav_path: &Path, starts: &[usize]) -> anyhow::Result<()> {
    std::fs::write(audio_segments_path(wav_path), serde_json::to_vec(starts)?)?;
    Ok(())
}

pub fn read_audio_segments(wav_path: &Path) -> anyhow::Result<Option<Vec<usize>>> {
    match std::fs::read(audio_segments_path(wav_path)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn transcribe_numbered_segments(
    samples: &[f32],
    starts: &[usize],
    mut transcribe: impl FnMut(Vec<f32>) -> anyhow::Result<String>,
) -> anyhow::Result<String> {
    use anyhow::{ensure, Context};

    ensure!(
        starts.first() == Some(&0)
            && starts.windows(2).all(|pair| pair[0] <= pair[1])
            && starts.last().is_some_and(|start| *start <= samples.len()),
        "Invalid annotation audio boundaries"
    );
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(samples.len());
            let audio = &samples[*start..end];
            let text = if audio.iter().all(|sample| *sample == 0.0) {
                String::new()
            } else {
                transcribe(audio.to_vec()).with_context(|| {
                    format!("Failed to transcribe annotation point {}", index + 1)
                })?
            };
            Ok(format!("{}. {}", index + 1, text.trim()))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(|points| points.join("\n"))
}

#[tauri::command]
#[specta::specta]
pub fn complete_screen_annotation(app: AppHandle) -> Result<(), String> {
    let state = app.state::<ScreenAnnotationState>();
    let binding_id = state
        .session
        .lock()
        .map_err(|_| "Failed to lock screen annotation state".to_string())?
        .as_ref()
        .map(|session| session.binding_id.clone())
        .ok_or_else(|| "No screen annotation is active".to_string())?;
    let coordinator = app
        .try_state::<crate::TranscriptionCoordinator>()
        .ok_or_else(|| "Transcription coordinator is unavailable".to_string())?;
    coordinator.send_external_input(&binding_id, "screen annotation done");
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_screen_annotation_snapshot(app: AppHandle) -> Result<ScreenAnnotationSnapshot, String> {
    let state = app.state::<ScreenAnnotationState>();
    let session = state
        .session
        .lock()
        .map_err(|_| "Failed to lock screen annotation state".to_string())?;
    session
        .as_ref()
        .map(|session| session.snapshot.clone())
        .ok_or_else(|| "No screen annotation is active".to_string())
}

#[tauri::command]
#[specta::specta]
pub fn show_screen_annotation(app: AppHandle, session_id: u64) -> Result<(), String> {
    let state = app.state::<ScreenAnnotationState>();
    let bounds = state
        .session
        .lock()
        .map_err(|_| "Failed to lock screen annotation state".to_string())?
        .as_ref()
        .filter(|session| session.snapshot.session_id == session_id && session.phase.can_show())
        .map(|session| session.bounds)
        .ok_or_else(|| "No screen annotation is active".to_string())?;

    let window = app
        .get_webview_window(WINDOW_LABEL)
        .ok_or_else(|| "Screen annotation window is unavailable".to_string())?;

    #[cfg(target_os = "windows")]
    place_annotation_window(&window, bounds)?;

    #[cfg(not(target_os = "windows"))]
    {
        let _ = bounds;
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn submit_screen_annotation(
    app: AppHandle,
    session_id: u64,
    png_data_url: String,
    point_count: usize,
) -> Result<(), String> {
    use base64::Engine;

    let encoded = png_data_url
        .strip_prefix("data:image/png;base64,")
        .ok_or_else(|| "Annotated screenshot is not a PNG data URL".to_string())?;
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| format!("Invalid annotated screenshot encoding: {error}"))?;
    let decoded = tauri::image::Image::from_bytes(&png_bytes)
        .map_err(|error| format!("Invalid annotated screenshot: {error}"))?;
    if decoded.width() == 0 || decoded.height() == 0 {
        return Err("Annotated screenshot is empty".to_string());
    }

    let state = app.state::<ScreenAnnotationState>();
    {
        let mut session = state
            .session
            .lock()
            .map_err(|_| "Failed to lock screen annotation state".to_string())?;
        let session = session
            .as_mut()
            .ok_or_else(|| "No screen annotation is active".to_string())?;
        if session.snapshot.session_id != session_id
            || !session.phase.can_submit()
            || session.point_starts.len() != point_count
        {
            return Err("Screen annotation session is stale".to_string());
        }
        session.png = png_bytes;
        session.phase = AnnotationPhase::Finalized;
    }
    state.finalized.notify_all();
    hide_and_restore(&app);
    Ok(())
}

fn mark_finalization_failed(state: &ScreenAnnotationState) {
    if let Ok(mut session) = state.session.lock() {
        if let Some(session) = session.as_mut() {
            session.phase = AnnotationPhase::Failed;
        }
    }
    state.finalized.notify_all();
}

fn hide_and_restore(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.hide();
        #[cfg(target_os = "windows")]
        if let Ok(hwnd) = window.hwnd() {
            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }

    if let Some(target_window) = app
        .state::<ScreenAnnotationState>()
        .session
        .lock()
        .ok()
        .and_then(|session| session.as_ref().map(|session| session.target_window))
    {
        restore_target_window(target_window);
    }
}

#[cfg(target_os = "windows")]
fn begin_windows(app: &AppHandle, binding_id: &str) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    if app.get_webview_window(WINDOW_LABEL).is_none() {
        return Err("Screen annotation window is unavailable".to_string());
    }

    let target_window = unsafe { GetForegroundWindow().0 as isize };
    let bounds = active_monitor_bounds(app)?;
    let rgba = capture_screen(bounds)?;
    let png = encode_png(&rgba, bounds.width, bounds.height)?;
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&cache_dir).map_err(|error| error.to_string())?;
    let snapshot_path = cache_dir.join(format!(
        "screen-annotation-{}.png",
        chrono::Utc::now().timestamp_millis()
    ));
    std::fs::write(&snapshot_path, &png).map_err(|error| error.to_string())?;

    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let snapshot = ScreenAnnotationSnapshot {
        session_id,
        path: snapshot_path.to_string_lossy().into_owned(),
        width: bounds.width,
        height: bounds.height,
        point_count: 1,
    };
    let reset_snapshot = snapshot.clone();

    let state = app.state::<ScreenAnnotationState>();
    if let Ok(mut session) = state.session.lock() {
        if let Some(previous) = session.replace(AnnotationSession {
            snapshot,
            snapshot_path,
            png,
            bounds,
            binding_id: binding_id.to_string(),
            target_window,
            phase: AnnotationPhase::Recording,
            point_starts: vec![0],
        }) {
            let _ = std::fs::remove_file(previous.snapshot_path);
        }
    } else {
        return Err("Failed to lock screen annotation state".to_string());
    }

    let app_handle = app.clone();
    app.run_on_main_thread(move || {
        if !is_recording_session(&app_handle, session_id) {
            return;
        }
        if let Some(window) = app_handle.get_webview_window(WINDOW_LABEL) {
            if let Err(error) = window.emit("reset-screen-annotation", reset_snapshot) {
                log::error!("Failed to reset screen annotation window: {error}");
                cancel(&app_handle);
                crate::utils::show_recording_overlay(&app_handle);
            }
        } else {
            cancel(&app_handle);
            crate::utils::show_recording_overlay(&app_handle);
        }
    })
    .map_err(|error| error.to_string())?;

    Ok(())
}

#[cfg(target_os = "windows")]
fn active_monitor_bounds(app: &AppHandle) -> Result<ScreenBounds, String> {
    let cursor = crate::input::get_cursor_position(app)
        .ok_or_else(|| "Failed to get cursor position".to_string())?;
    let monitors = app
        .available_monitors()
        .map_err(|error| error.to_string())?;
    let monitor = monitors
        .into_iter()
        .find(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            cursor.0 >= position.x
                && cursor.0 < position.x + size.width as i32
                && cursor.1 >= position.y
                && cursor.1 < position.y + size.height as i32
        })
        .or_else(|| app.primary_monitor().ok().flatten())
        .ok_or_else(|| "No display is available".to_string())?;

    Ok(ScreenBounds {
        x: monitor.position().x,
        y: monitor.position().y,
        width: monitor.size().width,
        height: monitor.size().height,
    })
}

#[cfg(target_os = "windows")]
fn capture_screen(bounds: ScreenBounds) -> Result<Vec<u8>, String> {
    use std::ffi::c_void;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
        DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
    };

    let width = i32::try_from(bounds.width).map_err(|_| "Display is too wide".to_string())?;
    let height = i32::try_from(bounds.height).map_err(|_| "Display is too tall".to_string())?;

    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err("GetDC failed".to_string());
        }

        let memory_dc = CreateCompatibleDC(Some(screen_dc));
        if memory_dc.is_invalid() {
            ReleaseDC(None, screen_dc);
            return Err("CreateCompatibleDC failed".to_string());
        }

        let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
        if bitmap.is_invalid() {
            let _ = DeleteDC(memory_dc);
            ReleaseDC(None, screen_dc);
            return Err("CreateCompatibleBitmap failed".to_string());
        }

        let old_object = SelectObject(memory_dc, HGDIOBJ(bitmap.0));
        let capture_result = BitBlt(
            memory_dc,
            0,
            0,
            width,
            height,
            Some(screen_dc),
            bounds.x,
            bounds.y,
            SRCCOPY | CAPTUREBLT,
        );
        SelectObject(memory_dc, old_object);

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; bounds.width as usize * bounds.height as usize * 4];

        let read_lines = if capture_result.is_ok() {
            GetDIBits(
                screen_dc,
                bitmap,
                0,
                bounds.height,
                Some(pixels.as_mut_ptr().cast::<c_void>()),
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory_dc);
        ReleaseDC(None, screen_dc);

        capture_result.map_err(|error| format!("BitBlt failed: {error}"))?;
        if read_lines == 0 {
            return Err("GetDIBits failed".to_string());
        }

        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        Ok(pixels)
    }
}

#[cfg(target_os = "windows")]
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
        writer
            .write_image_data(rgba)
            .map_err(|error| error.to_string())?;
    }
    Ok(output)
}

#[cfg(target_os = "windows")]
fn place_annotation_window(
    window: &tauri::WebviewWindow,
    bounds: ScreenBounds,
) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, SetWindowPos, HWND_TOPMOST, SWP_SHOWWINDOW,
    };

    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            bounds.x,
            bounds.y,
            bounds.width as i32,
            bounds.height as i32,
            SWP_SHOWWINDOW,
        )
        .map_err(|error| error.to_string())?;
        if !SetForegroundWindow(hwnd).as_bool() {
            log::warn!("Windows did not grant focus to the screen annotation window");
        }
    }
    Ok(())
}

fn restore_target_window(target_window: isize) {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
        if target_window != 0
            && !unsafe { SetForegroundWindow(HWND(target_window as *mut _)) }.as_bool()
        {
            log::warn!("Windows did not restore focus to the original target window");
        }
    }

    #[cfg(not(target_os = "windows"))]
    let _ = target_window;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_annotation_keeps_silent_points_and_retry_boundaries() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("recording.wav");
        let starts = [0, 2, 4, 4, 6];
        save_audio_segments(&path, &starts).expect("save boundaries");
        let restored = read_audio_segments(&path)
            .expect("read boundaries")
            .expect("numbered recording");
        assert_eq!(restored, starts);
        let samples = [0.1, 0.2, 0.0, 0.0, 0.3, 0.4];
        let mut calls = Vec::new();
        let text = transcribe_numbered_segments(&samples, &restored, |audio| {
            calls.push(audio);
            Ok(if calls.len() == 1 { "first" } else { "second" }.to_string())
        })
        .expect("transcribe");
        assert_eq!(calls, vec![vec![0.1, 0.2], vec![0.3, 0.4]]);
        assert_eq!(text, "1. first\n2. \n3. \n4. second\n5. ");
        for invalid in [vec![], vec![1], vec![0, 4, 3], vec![0, 7]] {
            assert!(
                transcribe_numbered_segments(&samples, &invalid, |_| Ok(String::new())).is_err()
            );
        }
        let failure =
            transcribe_numbered_segments(&samples, &[0, 2], |_| anyhow::bail!("model failed"))
                .expect_err("model error");
        assert!(failure.to_string().contains("point 1"));
        remove_audio_segments(&path);
        assert!(read_audio_segments(&path)
            .expect("missing boundaries")
            .is_none());
    }

    #[test]
    fn numbered_annotation_rejects_stale_or_finalizing_point_changes() {
        let mut session = AnnotationSession {
            snapshot: ScreenAnnotationSnapshot {
                session_id: 7,
                path: String::new(),
                width: 1,
                height: 1,
                point_count: 2,
            },
            snapshot_path: PathBuf::new(),
            png: Vec::new(),
            bounds: ScreenBounds {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            binding_id: String::new(),
            target_window: 0,
            phase: AnnotationPhase::Recording,
            point_starts: vec![0, 100],
        };
        assert!(validate_point_transition(&session, 7, 2).is_ok());
        assert!(validate_point_transition(&session, 6, 2).is_err());
        assert!(validate_point_transition(&session, 7, 1).is_err());
        session.phase = AnnotationPhase::Finalizing;
        assert!(validate_point_transition(&session, 7, 2).is_err());
    }

    #[test]
    fn only_recording_sessions_can_show_the_overlay() {
        assert!(AnnotationPhase::Recording.can_show());
        assert!(!AnnotationPhase::Finalizing.can_show());
        assert!(!AnnotationPhase::Finalized.can_show());
        assert!(!AnnotationPhase::Failed.can_show());
    }

    #[test]
    fn only_finalizing_sessions_wait_for_canvas_submission() {
        assert!(!AnnotationPhase::Recording.should_wait());
        assert!(AnnotationPhase::Finalizing.should_wait());
        assert!(!AnnotationPhase::Finalized.should_wait());
        assert!(!AnnotationPhase::Failed.should_wait());
    }

    #[test]
    fn only_finalizing_sessions_accept_canvas_submission() {
        assert!(!AnnotationPhase::Recording.can_submit());
        assert!(AnnotationPhase::Finalizing.can_submit());
        assert!(!AnnotationPhase::Finalized.can_submit());
        assert!(!AnnotationPhase::Failed.can_submit());
    }
}
