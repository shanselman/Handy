use std::path::PathBuf;
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnnotationPhase {
    Recording,
    Finalizing,
    Finalized,
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
        mark_finalized(&state);
        return;
    };

    if let Err(error) = window.emit("finish-screen-annotation", ()) {
        log::warn!("Failed to finalize screen annotation: {error}");
        mark_finalized(&state);
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

pub fn take_annotated_image(app: &AppHandle) -> Option<Vec<u8>> {
    let state = app.try_state::<ScreenAnnotationState>()?;
    let mut guard = state.session.lock().ok()?;

    if guard
        .as_ref()
        .is_some_and(|session| session.phase.should_wait())
    {
        let (new_guard, wait_result) =
            state.finalized.wait_timeout(guard, FINALIZE_TIMEOUT).ok()?;
        guard = new_guard;
        if wait_result.timed_out() {
            log::warn!("Timed out waiting for annotated screenshot; using latest image");
        }
    }

    let session = guard.take()?;
    drop(guard);

    let _ = std::fs::remove_file(session.snapshot_path);
    restore_target_window(session.target_window);
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.hide();
    }
    Some(session.png)
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
        if session.snapshot.session_id != session_id || !session.phase.can_submit() {
            return Err("Screen annotation session is stale".to_string());
        }
        session.png = png_bytes;
        session.phase = AnnotationPhase::Finalized;
    }
    state.finalized.notify_all();
    hide_and_restore(&app);
    Ok(())
}

fn mark_finalized(state: &ScreenAnnotationState) {
    if let Ok(mut session) = state.session.lock() {
        if let Some(session) = session.as_mut() {
            session.phase = AnnotationPhase::Finalized;
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
    use super::AnnotationPhase;

    #[test]
    fn only_recording_sessions_can_show_the_overlay() {
        assert!(AnnotationPhase::Recording.can_show());
        assert!(!AnnotationPhase::Finalizing.can_show());
        assert!(!AnnotationPhase::Finalized.can_show());
    }

    #[test]
    fn only_finalizing_sessions_wait_for_canvas_submission() {
        assert!(!AnnotationPhase::Recording.should_wait());
        assert!(AnnotationPhase::Finalizing.should_wait());
        assert!(!AnnotationPhase::Finalized.should_wait());
    }

    #[test]
    fn only_finalizing_sessions_accept_canvas_submission() {
        assert!(!AnnotationPhase::Recording.can_submit());
        assert!(AnnotationPhase::Finalizing.can_submit());
        assert!(!AnnotationPhase::Finalized.can_submit());
    }
}
