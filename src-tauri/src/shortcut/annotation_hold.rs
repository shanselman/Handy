use handy_keys::{Hotkey, Key, KeyEvent, KeyboardListener, Modifiers};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

use crate::settings::{ShortcutActivation, ShortcutBinding};
use crate::TranscriptionCoordinator;

const BINDING_ID: &str = "transcribe_with_annotation";
pub const HOTKEY: &str = "alt_left+space";

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Start,
    Next,
    Stop,
    Cancel,
}

#[derive(Default)]
struct HeldChord {
    space_down: bool,
    consumed_space: bool,
    active: bool,
    ctrl_down: bool,
}

impl HeldChord {
    fn update(&mut self, event: KeyEvent) -> Option<Action> {
        let fresh_space = event.key == Some(Key::Space) && event.is_key_down && !self.space_down;
        if event.key == Some(Key::Space) {
            self.space_down = event.is_key_down;
            if !self.space_down {
                self.consumed_space = false;
            }
        }
        let ctrl_down = event.modifiers.intersects(Modifiers::CTRL);
        let next = ctrl_down && !self.ctrl_down;
        self.ctrl_down = ctrl_down;

        if self.active {
            if event.key == Some(Key::Escape) && event.is_key_down {
                self.active = false;
                return Some(Action::Cancel);
            }
            if !self.space_down || !event.modifiers.contains(Modifiers::OPT_LEFT) {
                self.active = false;
                return Some(Action::Stop);
            }
            if next {
                return Some(Action::Next);
            }
        } else if fresh_space && Modifiers::OPT_LEFT.matches(event.modifiers) {
            self.active = true;
            self.consumed_space = true;
            return Some(Action::Start);
        }
        None
    }
}

fn blocked_hotkeys(consumed_space: bool) -> HashSet<Hotkey> {
    if !consumed_space {
        return HashSet::from([Hotkey {
            modifiers: Modifiers::OPT_LEFT,
            key: Some(Key::Space),
        }]);
    }
    // Swallow repeats until Space is released, even if Alt is released first.
    // Otherwise a trailing Ctrl+Space repeat can start ordinary dictation.
    (0..16)
        .map(|bits| {
            let mut modifiers = Modifiers::empty();
            for (index, group) in [
                Modifiers::CTRL,
                Modifiers::OPT,
                Modifiers::SHIFT,
                Modifiers::CMD,
            ]
            .into_iter()
            .enumerate()
            {
                if bits & (1 << index) != 0 {
                    modifiers |= group;
                }
            }
            Hotkey {
                modifiers,
                key: Some(Key::Space),
            }
        })
        .collect()
}

pub fn is_binding(binding: &ShortcutBinding) -> bool {
    binding.id == BINDING_ID
        && binding
            .current_binding
            .parse::<Hotkey>()
            .is_ok_and(|hotkey| {
                hotkey.modifiers == Modifiers::OPT_LEFT && hotkey.key == Some(Key::Space)
            })
}

pub fn validate_binding_change(
    bindings: &HashMap<String, ShortcutBinding>,
    binding: &ShortcutBinding,
) -> Result<(), String> {
    let mut proposed = bindings.clone();
    proposed.insert(binding.id.clone(), binding.clone());
    if proposed.get(BINDING_ID).is_some_and(is_binding)
        && crate::settings::annotation_hold_conflicts(&proposed)
    {
        return Err("Left Alt + Space is assigned to another shortcut".to_string());
    }
    Ok(())
}

struct Worker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                log::error!("Held annotation shortcut worker panicked");
            }
        }
    }
}

#[derive(Default)]
struct HoldShortcutState {
    worker: Mutex<Option<Worker>>,
}

pub fn register(app: &AppHandle) -> Result<(), String> {
    if !crate::screen_annotation::enabled() {
        return Err("Screen annotation is disabled".to_string());
    }
    if crate::settings::annotation_hold_conflicts(&crate::settings::get_settings(app).bindings) {
        return Err("Left Alt + Space is assigned to another shortcut".to_string());
    }
    if app.try_state::<HoldShortcutState>().is_none() {
        app.manage(HoldShortcutState::default());
    }
    let state = app.state::<HoldShortcutState>();
    let mut worker = state.worker.lock().map_err(|error| error.to_string())?;
    if worker.is_some() {
        return Err("Held annotation shortcut is already registered".to_string());
    }
    let blocking = Arc::new(Mutex::new(blocked_hotkeys(false)));
    let listener = KeyboardListener::new_with_blocking(Arc::clone(&blocking))
        .map_err(|error| format!("Failed to listen for the held annotation shortcut: {error}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let app = app.clone();
    let handle = std::thread::Builder::new()
        .name("annotation-hold".to_string())
        .spawn(move || {
            let mut chord = HeldChord::default();
            while !thread_stop.load(Ordering::Acquire) {
                let event = match listener.recv_timeout(Duration::from_millis(100)) {
                    Ok(event) => event,
                    Err(handy_keys::Error::Timeout) => continue,
                    Err(error) => {
                        report_error(
                            &app,
                            format!("Annotation keyboard listener stopped: {error}"),
                        );
                        break;
                    }
                };
                let was_consumed = chord.consumed_space;
                let action = chord.update(event);
                if was_consumed != chord.consumed_space {
                    match blocking.lock() {
                        Ok(mut hotkeys) => *hotkeys = blocked_hotkeys(chord.consumed_space),
                        Err(error) => {
                            report_error(
                                &app,
                                format!("Failed to update annotation keys: {error}"),
                            );
                            break;
                        }
                    }
                }
                if let Some(action) = action {
                    let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() else {
                        report_error(&app, "Transcription coordinator is unavailable".to_string());
                        break;
                    };
                    match action {
                        Action::Start => send_edge(&coordinator, true),
                        Action::Stop => send_edge(&coordinator, false),
                        Action::Next => coordinator.send_annotation_next_point(),
                        Action::Cancel => coordinator.send_annotation_cancel(),
                    }
                }
            }
            if chord.active {
                if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
                    send_edge(&coordinator, false);
                }
            }
        })
        .map_err(|error| format!("Failed to start the annotation shortcut worker: {error}"))?;
    *worker = Some(Worker {
        stop,
        handle: Some(handle),
    });
    log::info!("Registered held annotation shortcut: {HOTKEY}; Ctrl advances the point");
    Ok(())
}

pub fn unregister(app: &AppHandle) -> Result<(), String> {
    if let Some(state) = app.try_state::<HoldShortcutState>() {
        let worker = state
            .worker
            .lock()
            .map_err(|error| error.to_string())?
            .take();
        drop(worker);
    }
    Ok(())
}

fn send_edge(coordinator: &TranscriptionCoordinator, pressed: bool) {
    coordinator.send_input(
        BINDING_ID,
        HOTKEY,
        pressed,
        ShortcutActivation::PushToTalk,
        Duration::ZERO,
    );
}

fn report_error(app: &AppHandle, error: String) {
    log::error!("{error}");
    if let Err(emit_error) = app.emit("transcription-error", error) {
        log::error!("Failed to report annotation shortcut failure: {emit_error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_hold_rejects_conflicting_binding_changes_in_both_directions() {
        let mut bindings = crate::settings::get_default_settings().bindings;
        let held = bindings[BINDING_ID].clone();
        for id in ["transcribe", "cancel", "transcribe_with_post_process"] {
            let mut binding = bindings[id].clone();
            for shortcut in ["alt+space", "alt_left+space"] {
                binding.current_binding = shortcut.to_string();
                assert!(validate_binding_change(&bindings, &binding).is_err());
            }
            binding.current_binding = "alt_right+space".to_string();
            assert!(validate_binding_change(&bindings, &binding).is_ok());
        }
        assert!(validate_binding_change(&bindings, &bindings["transcribe"]).is_ok());

        bindings.get_mut(BINDING_ID).unwrap().current_binding = "ctrl+alt+space".to_string();
        let mut binding = bindings["transcribe"].clone();
        binding.current_binding = "alt+space".to_string();
        assert!(validate_binding_change(&bindings, &binding).is_ok());
        bindings.insert(binding.id.clone(), binding);
        assert!(validate_binding_change(&bindings, &held).is_err());
    }

    fn event(key: Option<Key>, modifiers: Modifiers, is_key_down: bool) -> KeyEvent {
        KeyEvent {
            key,
            modifiers,
            is_key_down,
            changed_modifier: None,
        }
    }

    #[test]
    fn annotation_hold_advances_once_per_ctrl_press_and_stops_on_release() {
        let alt = Modifiers::OPT_LEFT;
        let ctrl = alt | Modifiers::CTRL_LEFT;
        let mut chord = HeldChord::default();
        assert_eq!(
            chord.update(event(Some(Key::Space), alt, true)),
            Some(Action::Start)
        );
        assert_eq!(chord.update(event(Some(Key::Space), alt, true)), None);
        assert_eq!(chord.update(event(None, ctrl, true)), Some(Action::Next));
        assert_eq!(chord.update(event(Some(Key::Space), ctrl, true)), None);
        assert_eq!(
            chord.update(event(None, ctrl | Modifiers::CTRL_RIGHT, true)),
            None
        );
        assert_eq!(chord.update(event(None, alt, false)), None);
        assert_eq!(chord.update(event(None, ctrl, true)), Some(Action::Next));
        assert_eq!(
            chord.update(event(None, Modifiers::CTRL_LEFT, false)),
            Some(Action::Stop)
        );
        assert!(chord.consumed_space);
        assert_eq!(
            chord.update(event(Some(Key::Space), Modifiers::CTRL_LEFT, true)),
            None
        );
        assert_eq!(
            chord.update(event(Some(Key::Space), Modifiers::empty(), false)),
            None
        );
        assert!(!chord.consumed_space);
        assert_eq!(
            chord.update(event(Some(Key::Space), alt, true)),
            Some(Action::Start)
        );
        assert_eq!(
            chord.update(event(Some(Key::Space), alt, false)),
            Some(Action::Stop)
        );
    }

    #[test]
    fn annotation_hold_ignores_right_alt_and_preserves_other_shortcuts_when_idle() {
        let mut chord = HeldChord::default();
        assert_eq!(
            chord.update(event(Some(Key::Space), Modifiers::OPT_RIGHT, true)),
            None
        );
        chord.update(event(Some(Key::Space), Modifiers::empty(), false));
        assert_eq!(
            chord.update(event(Some(Key::Space), Modifiers::CTRL_LEFT, true)),
            None
        );
        let idle = blocked_hotkeys(false);
        assert!(!idle
            .iter()
            .any(|key| key.modifiers.matches(Modifiers::CTRL_LEFT)));
        assert!(!idle
            .iter()
            .any(|key| key.modifiers.matches(Modifiers::OPT_RIGHT)));
        assert!(idle
            .iter()
            .any(|key| key.modifiers.matches(Modifiers::OPT_LEFT)));
        let held = blocked_hotkeys(true);
        for modifiers in [
            Modifiers::empty(),
            Modifiers::CTRL_LEFT,
            Modifiers::OPT_LEFT | Modifiers::CTRL_RIGHT,
        ] {
            assert!(held.iter().any(|key| key.modifiers.matches(modifiers)));
        }
    }

    #[test]
    fn annotation_hold_escape_cancels_without_restarting_on_space_repeat() {
        let alt = Modifiers::OPT_LEFT;
        let mut chord = HeldChord::default();
        assert_eq!(
            chord.update(event(Some(Key::Space), alt, true)),
            Some(Action::Start)
        );
        assert_eq!(
            chord.update(event(Some(Key::Escape), alt, true)),
            Some(Action::Cancel)
        );
        assert_eq!(chord.update(event(Some(Key::Space), alt, true)), None);
        assert_eq!(chord.update(event(Some(Key::Space), alt, false)), None);
        assert!(!chord.consumed_space);
    }
}
